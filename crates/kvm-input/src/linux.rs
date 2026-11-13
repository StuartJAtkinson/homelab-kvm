//! Linux input support through evdev/uinput, with X11 for cursor positioning.
//!
//! The input devices are opened and grabbed only while forwarding. This keeps
//! injected uinput events separate from physical events and lets desktop apps
//! receive input normally while this node owns the cursor.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Context;
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode};
use kvm_core::engine::LocalEvent;
use kvm_core::protocol::{InputEvent as KvmInput, MouseButton};
use kvm_core::{DisplayInfo, Point};
use tokio::sync::mpsc::UnboundedSender;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, Window};
use x11rb::rust_connection::RustConnection;

pub fn displays() -> anyhow::Result<Vec<DisplayInfo>> {
    let out = std::process::Command::new("xrandr")
        .arg("--listmonitors")
        .output()
        .context("running xrandr (is it installed and is DISPLAY set?)")?;
    anyhow::ensure!(
        out.status.success(),
        "xrandr failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(crate::xrandr::parse_listmonitors(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

pub fn has_battery() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
        return false;
    };
    entries.flatten().any(|e| {
        std::fs::read_to_string(e.path().join("type")).is_ok_and(|t| t.trim() == "Battery")
    })
}

struct XDisplay {
    conn: RustConnection,
    root: Window,
}

impl XDisplay {
    fn connect() -> anyhow::Result<Self> {
        let (conn, screen) = x11rb::connect(None).context("connecting to the X11 display")?;
        let root = conn
            .setup()
            .roots
            .get(screen)
            .context("X11 screen is unavailable")?
            .root;
        Ok(Self { conn, root })
    }

    fn cursor(&self) -> anyhow::Result<Point> {
        let p = self.conn.query_pointer(self.root)?.reply()?;
        Ok(Point::new(p.root_x as i32, p.root_y as i32))
    }

    fn warp(&self, point: Point) -> anyhow::Result<()> {
        self.conn
            .warp_pointer(
                x11rb::NONE,
                self.root,
                0,
                0,
                0,
                0,
                point.x as i16,
                point.y as i16,
            )?
            .check()?;
        self.conn.flush()?;
        Ok(())
    }
}

pub struct LinuxCapture {
    grabbed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LinuxCapture {
    pub fn start(sink: UnboundedSender<LocalEvent>) -> anyhow::Result<Self> {
        // Fail early with a useful message if this is a pure Wayland session
        // without XWayland, or the input device permissions are missing.
        XDisplay::connect()?;
        let mut found = 0;
        for (path, mut dev) in evdev::enumerate().filter(|(_, dev)| input_capable(dev)) {
            found += 1;
            dev.grab().with_context(|| {
                format!(
                    "grabbing {} (grant this user access to /dev/input)",
                    path.display()
                )
            })?;
            dev.ungrab().with_context(|| {
                format!("releasing {} after input access check", path.display())
            })?;
        }
        anyhow::ensure!(
            found > 0,
            "no readable keyboard or relative-pointer devices in /dev/input"
        );
        let grabbed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let g = grabbed.clone();
        let s = stop.clone();
        let thread = thread::Builder::new()
            .name("linux-input".into())
            .spawn(move || capture_loop(sink, g, s))?;
        Ok(Self {
            grabbed,
            stop,
            thread: Some(thread),
        })
    }
}

impl crate::InputCapture for LinuxCapture {
    fn set_grab(&self, grab: bool) {
        self.grabbed.store(grab, Ordering::SeqCst);
    }
}

impl Drop for LinuxCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn input_capable(dev: &evdev::Device) -> bool {
    let name = dev.name().unwrap_or_default().to_ascii_lowercase();
    if name.contains("homelab-kvm") {
        return false;
    }
    let keyboard = dev
        .supported_keys()
        .is_some_and(|keys| keys.iter().any(|k| k.0 < 0x100));
    let pointer = dev.supported_relative_axes().is_some_and(|axes| {
        axes.contains(RelativeAxisCode::REL_X) && axes.contains(RelativeAxisCode::REL_Y)
    });
    keyboard || pointer
}

fn capture_loop(
    sink: UnboundedSender<LocalEvent>,
    requested: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    let display = match XDisplay::connect() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("X11 input capture stopped: {e:#}");
            return;
        }
    };
    let mut devices: Vec<_> = evdev::enumerate()
        .filter(|(_, d)| input_capable(d))
        .filter_map(|(path, d)| {
            if let Err(e) = d.set_nonblocking(true) {
                tracing::warn!(device = %path.display(), "ignoring device that cannot be read without blocking: {e}");
                None
            } else {
                Some((path, d, false))
            }
        })
        .collect();
    let mut active = false;
    let mut modifiers = BTreeSet::<u16>::new();
    let mut last_warn = std::time::Instant::now() - Duration::from_secs(10);

    while !stop.load(Ordering::SeqCst) {
        let want = requested.load(Ordering::SeqCst);
        if want != active {
            let mut failed = false;
            for (path, dev, held) in &mut devices {
                if want {
                    match dev.grab() {
                        Ok(()) => *held = true,
                        Err(e) => {
                            tracing::warn!(device = %path.display(), "could not grab input device; releasing forwarding: {e}");
                            failed = true;
                        }
                    }
                } else if *held {
                    if let Err(e) = dev.ungrab() {
                        tracing::warn!(device = %path.display(), "could not release input device grab: {e}");
                    }
                    *held = false;
                }
            }
            if want && failed {
                for (_, dev, held) in &mut devices {
                    if *held {
                        let _ = dev.ungrab();
                        *held = false;
                    }
                }
                requested.store(false, Ordering::SeqCst);
                let _ = sink.send(LocalEvent::Release);
                active = false;
            } else {
                active = want;
            }
        }

        for (_, dev, _) in &mut devices {
            let events = match dev.fetch_events() {
                Ok(events) => events.collect::<Vec<_>>(),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(e) => {
                    if last_warn.elapsed() > Duration::from_secs(5) {
                        tracing::warn!("reading Linux input device failed: {e}");
                        last_warn = std::time::Instant::now();
                    }
                    continue;
                }
            };
            for event in events {
                match event.event_type() {
                    EventType::RELATIVE => match event.code() {
                        0 => {
                            let pos = display.cursor().unwrap_or_default();
                            let _ = sink.send(LocalEvent::Motion {
                                pos,
                                delta: Point::new(event.value(), 0),
                            });
                        }
                        1 => {
                            let pos = display.cursor().unwrap_or_default();
                            let _ = sink.send(LocalEvent::Motion {
                                pos,
                                delta: Point::new(0, event.value()),
                            });
                        }
                        6 => {
                            let _ = sink.send(LocalEvent::Input(KvmInput::Wheel {
                                dx: event.value().saturating_mul(120),
                                dy: 0,
                            }));
                        }
                        8 => {
                            let _ = sink.send(LocalEvent::Input(KvmInput::Wheel {
                                dx: 0,
                                dy: event.value().saturating_mul(120),
                            }));
                        }
                        _ => {}
                    },
                    EventType::KEY => {
                        let code = event.code();
                        let down = event.value() != 0;
                        if (0x110..=0x11f).contains(&code) {
                            if let Some(button) = linux_button(code) {
                                let _ =
                                    sink.send(LocalEvent::Input(KvmInput::Button { button, down }));
                            }
                            continue;
                        }
                        if matches!(code, 29 | 97 | 42 | 54 | 56 | 100) {
                            if down {
                                modifiers.insert(code);
                            } else {
                                modifiers.remove(&code);
                            }
                        }
                        // Ctrl+Alt+Shift+Esc is handled in the capture thread so
                        // it remains available even if the input agent is stalled.
                        if active && down && code == 1 && has_modifiers(&modifiers) {
                            requested.store(false, Ordering::SeqCst);
                            let _ = sink.send(LocalEvent::Release);
                            continue;
                        }
                        if let Some(usage) = crate::keymap::usage_from_linux_keycode(code) {
                            let _ = sink.send(LocalEvent::Input(KvmInput::Key { usage, down }));
                        }
                    }
                    EventType::SYNCHRONIZATION => {}
                    _ => {}
                }
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
    for (_, dev, held) in &mut devices {
        if *held {
            let _ = dev.ungrab();
        }
    }
}

fn has_modifiers(mods: &BTreeSet<u16>) -> bool {
    (mods.contains(&29) || mods.contains(&97))
        && (mods.contains(&56) || mods.contains(&100))
        && (mods.contains(&42) || mods.contains(&54))
}

fn linux_button(code: u16) -> Option<MouseButton> {
    match code {
        0x110 => Some(MouseButton::Left),
        0x111 => Some(MouseButton::Right),
        0x112 => Some(MouseButton::Middle),
        0x113 => Some(MouseButton::Back),
        0x114 => Some(MouseButton::Forward),
        _ => None,
    }
}

pub struct LinuxInjector {
    device: VirtualDevice,
    display: XDisplay,
    wheel_x: i32,
    wheel_y: i32,
}

impl LinuxInjector {
    pub fn new() -> anyhow::Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for code in 0..=0x2ff {
            keys.insert(KeyCode::new(code));
        }
        let mut rel = AttributeSet::<RelativeAxisCode>::new();
        for code in [
            RelativeAxisCode::REL_X,
            RelativeAxisCode::REL_Y,
            RelativeAxisCode::REL_WHEEL,
            RelativeAxisCode::REL_HWHEEL,
        ] {
            rel.insert(code);
        }
        let device = VirtualDevice::builder()?
            .name("homelab-kvm virtual input")
            .with_keys(&keys)?
            .with_relative_axes(&rel)?
            .build()
            .context("creating /dev/uinput device (check uinput permissions)")?;
        Ok(Self {
            device,
            display: XDisplay::connect()?,
            wheel_x: 0,
            wheel_y: 0,
        })
    }

    fn emit(&mut self, evs: &[InputEvent]) -> anyhow::Result<()> {
        self.device
            .emit(evs)
            .context("writing Linux virtual input")?;
        Ok(())
    }
}

impl crate::InputInjector for LinuxInjector {
    fn warp(&mut self, to: Point) -> anyhow::Result<()> {
        self.display.warp(to)
    }

    fn inject(&mut self, event: &KvmInput) -> anyhow::Result<()> {
        let syn = || InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0);
        match *event {
            KvmInput::MouseMove { dx, dy } => self.emit(&[
                InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_X.0, dx),
                InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_Y.0, dy),
                syn(),
            ]),
            KvmInput::Button { button, down } => {
                let code = match button {
                    MouseButton::Left => 0x110,
                    MouseButton::Right => 0x111,
                    MouseButton::Middle => 0x112,
                    MouseButton::Back => 0x113,
                    MouseButton::Forward => 0x114,
                };
                self.emit(&[
                    InputEvent::new(EventType::KEY.0, code, i32::from(down)),
                    syn(),
                ])
            }
            KvmInput::Wheel { dx, dy } => {
                self.wheel_x = self.wheel_x.saturating_add(dx);
                self.wheel_y = self.wheel_y.saturating_add(dy);
                let horizontal = self.wheel_x / 120;
                let vertical = self.wheel_y / 120;
                self.wheel_x %= 120;
                self.wheel_y %= 120;
                let mut evs = Vec::new();
                if vertical != 0 {
                    evs.push(InputEvent::new(
                        EventType::RELATIVE.0,
                        RelativeAxisCode::REL_WHEEL.0,
                        vertical,
                    ));
                }
                if horizontal != 0 {
                    evs.push(InputEvent::new(
                        EventType::RELATIVE.0,
                        RelativeAxisCode::REL_HWHEEL.0,
                        horizontal,
                    ));
                }
                if evs.is_empty() {
                    return Ok(());
                }
                evs.push(syn());
                self.emit(&evs)
            }
            KvmInput::Key { usage, down } => {
                let Some(code) = crate::keymap::linux_keycode_from_usage(usage) else {
                    return Ok(());
                };
                self.emit(&[
                    InputEvent::new(EventType::KEY.0, code, i32::from(down)),
                    syn(),
                ])
            }
        }
    }
}

pub fn cursor_position() -> Option<Point> {
    XDisplay::connect().ok()?.cursor().ok()
}
