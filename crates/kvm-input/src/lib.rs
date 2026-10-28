//! Platform glue: what this machine looks like (displays with stable ids,
//! laptop or not), and capturing/injecting keyboard and mouse input.

use kvm_core::engine::LocalEvent;
use kvm_core::protocol::InputEvent;
use kvm_core::{DisplayInfo, NodeId, NodeInfo, Os, Point};
use tokio::sync::mpsc::UnboundedSender;

pub mod edid;
pub mod keymap;
#[cfg(target_os = "linux")]
mod linux;
mod xrandr;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(windows)]
use windows as platform;

/// Describe the local machine.
pub fn local_node_info(id: NodeId) -> anyhow::Result<NodeInfo> {
    let name = hostname::get()?.to_string_lossy().into_owned();
    // `key` is filled in by `KvmNode::start` from the identity.
    Ok(NodeInfo { id, name, os: Os::current(), displays: displays()?, portable: has_battery(), key: String::new() })
}

/// Physical monitors in the OS desktop coordinate space, in physical pixels.
pub fn displays() -> anyhow::Result<Vec<DisplayInfo>> {
    platform::displays()
}

/// Laptop heuristic used to default new placements to unlocked.
pub fn has_battery() -> bool {
    platform::has_battery()
}

/// Reports physical input as [`LocalEvent`]s. Dropping it stops capture.
pub trait InputCapture: Send {
    /// While grabbed, physical input is swallowed locally (the cursor is on
    /// another machine) and motion is reported as deltas.
    fn set_grab(&self, grab: bool);
}

/// Replays forwarded input on this machine.
pub trait InputInjector: Send {
    /// Move the cursor to `to`, in local desktop coordinates.
    fn warp(&mut self, to: Point) -> anyhow::Result<()>;
    fn inject(&mut self, event: &InputEvent) -> anyhow::Result<()>;
}

pub fn start_capture(sink: UnboundedSender<LocalEvent>) -> anyhow::Result<Box<dyn InputCapture>> {
    #[cfg(windows)]
    return Ok(Box::new(windows::WinCapture::start(sink)?));
    #[cfg(not(windows))]
    {
        let _ = sink;
        anyhow::bail!("input capture isn't implemented on this platform yet")
    }
}

pub fn injector() -> anyhow::Result<Box<dyn InputInjector>> {
    #[cfg(windows)]
    return Ok(Box::new(windows::WinInjector::new()));
    #[cfg(not(windows))]
    anyhow::bail!("input injection isn't implemented on this platform yet")
}

/// Current cursor position, for diagnostics.
pub fn cursor_position() -> Option<Point> {
    #[cfg(windows)]
    return windows::cursor_position();
    #[cfg(not(windows))]
    None
}
