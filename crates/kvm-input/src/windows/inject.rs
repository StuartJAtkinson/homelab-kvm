//! Replays forwarded input with `SendInput`. Events are marked injected by
//! Windows, which is how our own hooks (and other tools) tell them apart from
//! physical input.
//!
//! `SendInput` can't reach windows of a higher integrity level (an elevated
//! app in front) or the secure desktop (UAC prompts, the lock screen); those
//! need the app to run elevated or as a service.

use anyhow::ensure;
use kvm_core::protocol::{InputEvent, MouseButton};
use kvm_core::Point;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MOUSE_EVENT_FLAGS,
    VK_NUMLOCK, VK_PAUSE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::keymap;
use crate::InputInjector;

/// Tags our events (`dwExtraInfo`), for debugging with input-spy tools.
const MAGIC: usize = 0x4B564D21; // "KVM!"
const XBUTTON1: u32 = 1;
const XBUTTON2: u32 = 2;

pub struct WinInjector;

impl WinInjector {
    pub fn new() -> Self {
        super::ensure_dpi_aware();
        WinInjector
    }
}

impl Default for WinInjector {
    fn default() -> Self {
        Self::new()
    }
}

fn mouse(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: MAGIC },
        },
    }
}

fn send(input: INPUT) -> anyhow::Result<()> {
    let n = unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
    ensure!(n == 1, "SendInput was blocked (elevated window or secure desktop in front?)");
    Ok(())
}

/// Absolute coordinates for `SendInput` are 0..=65535 across the virtual desktop.
fn normalise(p: Point) -> (i32, i32) {
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let scale = |v: i32, origin: i32, size: i32| {
        // Round to the pixel's centre so the result maps back to the same pixel.
        (((v - origin) as i64 * 65536 + 32768) / size.max(1) as i64).clamp(0, 65535) as i32
    };
    (scale(p.x, vx, vw), scale(p.y, vy, vh))
}

impl InputInjector for WinInjector {
    fn warp(&mut self, to: Point) -> anyhow::Result<()> {
        let (x, y) = normalise(to);
        send(mouse(x, y, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK))
    }

    fn inject(&mut self, event: &InputEvent) -> anyhow::Result<()> {
        match *event {
            InputEvent::MouseMove { dx, dy } => send(mouse(dx, dy, 0, MOUSEEVENTF_MOVE)),
            InputEvent::Button { button, down } => {
                let (flags, data) = match (button, down) {
                    (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                    (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
                    (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                    (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
                    (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                    (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                    (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
                    (MouseButton::Back, false) => (MOUSEEVENTF_XUP, XBUTTON1),
                    (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
                    (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, XBUTTON2),
                };
                send(mouse(0, 0, data, flags))
            }
            InputEvent::Wheel { dx, dy } => {
                if dy != 0 {
                    send(mouse(0, 0, dy as u32, MOUSEEVENTF_WHEEL))?;
                }
                if dx != 0 {
                    send(mouse(0, 0, dx as u32, MOUSEEVENTF_HWHEEL))?;
                }
                Ok(())
            }
            InputEvent::Key { usage, down } => {
                let up = if down { 0 } else { KEYEVENTF_KEYUP };
                let (vk, scan, flags) = match usage {
                    keymap::USAGE_PAUSE => (VK_PAUSE, 0, up),
                    keymap::USAGE_NUM_LOCK => (VK_NUMLOCK, 0, up | KEYEVENTF_EXTENDEDKEY),
                    _ => {
                        let Some((sc, ext)) = keymap::scancode_from_usage(usage) else {
                            tracing::debug!(usage, "no scancode for HID usage");
                            return Ok(());
                        };
                        let ext = if ext { KEYEVENTF_EXTENDEDKEY } else { 0 };
                        (0, sc, KEYEVENTF_SCANCODE | ext | up)
                    }
                };
                send(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: MAGIC },
                    },
                })
            }
        }
    }
}
