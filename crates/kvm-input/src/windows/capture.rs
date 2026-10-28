//! Physical input capture via low-level hooks (`WH_MOUSE_LL`, `WH_KEYBOARD_LL`)
//! on a dedicated message-loop thread.
//!
//! Hook callbacks are plain functions, so their state lives in statics; only
//! one capture can run per process. Injected events (ours or any other tool's)
//! are passed through unreported. While grabbed, physical events are
//! swallowed and motion is measured against a parked cursor.
//!
//! Safety valves, all inside the hook so they work even if the rest of the
//! app is wedged: Ctrl+Alt+Shift+Esc ungrabs, and a closed event channel
//! ungrabs. (Ctrl+Alt+Del never reaches hooks, so it always works too.)

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering::Relaxed};
use std::sync::Mutex;
use std::thread::JoinHandle;

use anyhow::{bail, Context};
use kvm_core::engine::LocalEvent;
use kvm_core::protocol::{InputEvent, MouseButton};
use kvm_core::Point;
use tokio::sync::mpsc::UnboundedSender;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_CONTROL, VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_MENU, VK_RCONTROL, VK_RMENU,
    VK_RSHIFT, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetCursorPos, GetMessageW, GetSystemMetrics, PostThreadMessageW, SetCursorPos,
    SetWindowsHookExW, UnhookWindowsHookEx, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED,
    LLKHF_UP, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, SM_CXSCREEN, SM_CYSCREEN, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN,
    WM_XBUTTONUP,
};

use crate::keymap;
use crate::InputCapture;

const XBUTTON1: u32 = 1;
const MOD_CTRL: u8 = 1;
const MOD_ALT: u8 = 2;
const MOD_SHIFT: u8 = 4;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static SINK: Mutex<Option<UnboundedSender<LocalEvent>>> = Mutex::new(None);
static GRAB: AtomicBool = AtomicBool::new(false);
static PARK_X: AtomicI32 = AtomicI32::new(0);
static PARK_Y: AtomicI32 = AtomicI32::new(0);
static LAST_X: AtomicI32 = AtomicI32::new(0);
static LAST_Y: AtomicI32 = AtomicI32::new(0);
static MODS: AtomicU8 = AtomicU8::new(0);

pub struct WinCapture {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl WinCapture {
    pub fn start(sink: UnboundedSender<LocalEvent>) -> anyhow::Result<Self> {
        if ACTIVE.swap(true, Relaxed) {
            bail!("input capture is already running in this process");
        }
        super::ensure_dpi_aware();
        *SINK.lock().unwrap() = Some(sink);
        GRAB.store(false, Relaxed);
        MODS.store(0, Relaxed);
        store_last(cursor_pos());

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new().name("input-hooks".into()).spawn(move || unsafe {
            let hmod = GetModuleHandleW(std::ptr::null());
            let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hmod, 0);
            let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), hmod, 0);
            let ok = !mouse.is_null() && !keyboard.is_null();
            let err = std::io::Error::last_os_error();
            let _ = ready_tx.send(if ok { Ok(GetCurrentThreadId()) } else { Err(err) });
            if ok {
                // Hooks are called from this thread's message loop.
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {}
            }
            if !mouse.is_null() {
                UnhookWindowsHookEx(mouse);
            }
            if !keyboard.is_null() {
                UnhookWindowsHookEx(keyboard);
            }
        })?;

        match ready_rx.recv().context("hook thread died")? {
            Ok(thread_id) => Ok(Self { thread_id, thread: Some(thread) }),
            Err(e) => {
                let _ = thread.join();
                cleanup();
                Err(anyhow::Error::new(e).context("installing input hooks"))
            }
        }
    }
}

impl InputCapture for WinCapture {
    fn set_grab(&self, grab: bool) {
        if grab {
            // Park at the centre of the primary display (its top-left is 0,0).
            let park = unsafe { Point::new(GetSystemMetrics(SM_CXSCREEN) / 2, GetSystemMetrics(SM_CYSCREEN) / 2) };
            PARK_X.store(park.x, Relaxed);
            PARK_Y.store(park.y, Relaxed);
            GRAB.store(true, Relaxed);
            unsafe { SetCursorPos(park.x, park.y) };
        } else {
            GRAB.store(false, Relaxed);
            store_last(cursor_pos());
        }
    }
}

impl Drop for WinCapture {
    fn drop(&mut self) {
        GRAB.store(false, Relaxed);
        unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        cleanup();
    }
}

fn cleanup() {
    *SINK.lock().unwrap() = None;
    GRAB.store(false, Relaxed);
    ACTIVE.store(false, Relaxed);
}

fn cursor_pos() -> Point {
    let mut p = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut p) };
    Point::new(p.x, p.y)
}

fn store_last(p: Point) {
    LAST_X.store(p.x, Relaxed);
    LAST_Y.store(p.y, Relaxed);
}

fn emit(ev: LocalEvent) {
    let delivered = SINK.lock().unwrap().as_ref().is_some_and(|tx| tx.send(ev).is_ok());
    if !delivered {
        // Nobody is listening: never leave the user's input swallowed.
        GRAB.store(false, Relaxed);
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }
    let info = &*(lparam as *const MSLLHOOKSTRUCT);
    if info.flags & LLMHF_INJECTED != 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }
    let grabbed = GRAB.load(Relaxed);
    let button = |button, down| Some(LocalEvent::Input(InputEvent::Button { button, down }));
    let wheel = (info.mouseData >> 16) as u16 as i16 as i32;
    let x_button = if (info.mouseData >> 16) == XBUTTON1 { MouseButton::Back } else { MouseButton::Forward };

    let ev = match wparam as u32 {
        WM_MOUSEMOVE => {
            let pt = Point::new(info.pt.x, info.pt.y);
            if grabbed {
                let park = Point::new(PARK_X.load(Relaxed), PARK_Y.load(Relaxed));
                Some(LocalEvent::Motion { pos: park, delta: pt - park })
            } else {
                let last = Point::new(LAST_X.load(Relaxed), LAST_Y.load(Relaxed));
                store_last(pt);
                Some(LocalEvent::Motion { pos: pt, delta: pt - last })
            }
        }
        WM_LBUTTONDOWN => button(MouseButton::Left, true),
        WM_LBUTTONUP => button(MouseButton::Left, false),
        WM_RBUTTONDOWN => button(MouseButton::Right, true),
        WM_RBUTTONUP => button(MouseButton::Right, false),
        WM_MBUTTONDOWN => button(MouseButton::Middle, true),
        WM_MBUTTONUP => button(MouseButton::Middle, false),
        WM_XBUTTONDOWN => button(x_button, true),
        WM_XBUTTONUP => button(x_button, false),
        WM_MOUSEWHEEL => Some(LocalEvent::Input(InputEvent::Wheel { dx: 0, dy: wheel })),
        WM_MOUSEHWHEEL => Some(LocalEvent::Input(InputEvent::Wheel { dx: wheel, dy: 0 })),
        _ => None,
    };
    if let Some(ev) = ev {
        emit(ev);
    }
    if grabbed && GRAB.load(Relaxed) {
        return 1;
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }
    let info = &*(lparam as *const KBDLLHOOKSTRUCT);
    if info.flags & LLKHF_INJECTED != 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }
    let down = info.flags & LLKHF_UP == 0;
    let vk = info.vkCode as u16;
    let bit = match vk {
        VK_CONTROL | VK_LCONTROL | VK_RCONTROL => MOD_CTRL,
        VK_MENU | VK_LMENU | VK_RMENU => MOD_ALT,
        VK_SHIFT | VK_LSHIFT | VK_RSHIFT => MOD_SHIFT,
        _ => 0,
    };
    if down {
        MODS.fetch_or(bit, Relaxed);
    } else {
        MODS.fetch_and(!bit, Relaxed);
    }

    let grabbed = GRAB.load(Relaxed);
    if grabbed && down && vk == VK_ESCAPE && MODS.load(Relaxed) == MOD_CTRL | MOD_ALT | MOD_SHIFT {
        GRAB.store(false, Relaxed);
        store_last(cursor_pos());
        emit(LocalEvent::Release);
        return 1;
    }

    if let Some(usage) = keymap::usage_from_scancode(info.scanCode as u16, info.flags & LLKHF_EXTENDED != 0) {
        emit(LocalEvent::Input(InputEvent::Key { usage, down }));
    }
    if grabbed && GRAB.load(Relaxed) {
        return 1;
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}
