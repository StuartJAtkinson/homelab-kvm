//! Platform glue: what this machine looks like (displays, laptop or not) and,
//! from phase 2, capturing and injecting keyboard/mouse input.

use kvm_core::protocol::InputEvent;
use kvm_core::{DisplayInfo, NodeId, NodeInfo, Os};

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

/// Receives physical input on the active node. Phase 2.
pub trait InputCapture: Send {
    /// Start delivering events. While `grab` is true, events are swallowed
    /// locally (the cursor is on another machine).
    fn start(&mut self, sink: Box<dyn FnMut(InputEvent) + Send>) -> anyhow::Result<()>;
    fn set_grab(&mut self, grab: bool);
    fn cursor_position(&self) -> anyhow::Result<kvm_core::Point>;
}

/// Replays forwarded input on a target node. Phase 2.
pub trait InputInjector: Send {
    fn warp(&mut self, to: kvm_core::Point) -> anyhow::Result<()>;
    fn inject(&mut self, event: &InputEvent) -> anyhow::Result<()>;
}
