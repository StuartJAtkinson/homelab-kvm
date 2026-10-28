//! Messages exchanged between peers. Framing lives in `kvm-net`.

use serde::{Deserialize, Serialize};

use crate::{Layout, NodeId, NodeInfo};

/// Bumped on any incompatible change to [`Message`].
pub const PROTOCOL_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// First message after the Noise handshake, in both directions.
    /// `node.key` must equal the static key the handshake proved.
    Hello { proto: u32, node: NodeInfo },
    /// The user confirmed the pairing code on the sender's side. Carries
    /// the sender's layout so both sides can compute [`Layout::join`].
    PairAccept { layout: Layout },
    /// The sender's displays or name changed.
    NodeUpdate(NodeInfo),
    /// Full replacement layout; receiver keeps it if it supersedes its own.
    /// Only exchanged between paired members.
    Layout(Layout),
    /// Cursor is entering `target` at `(x, y)` in the target's coordinates.
    Enter { target: NodeId, x: i32, y: i32 },
    /// Forwarded input for the node most recently sent `Enter`.
    Input(InputEvent),
    /// Target to source: the cursor left my screens towards `to`, arriving at
    /// `(x, y)` in `to`'s coordinates. `to` may be the source itself.
    Handoff { to: NodeId, x: i32, y: i32 },
    /// Sender saw physical input on itself while being driven remotely, and
    /// is taking its cursor back.
    Claim,
    Ping { nonce: u64 },
    Pong { nonce: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputEvent {
    /// Relative motion in physical pixels.
    MouseMove { dx: i32, dy: i32 },
    Button { button: MouseButton, down: bool },
    /// Wheel motion, 120 per detent (as on Windows); `dy > 0` scrolls away
    /// from the user, `dx > 0` scrolls right.
    Wheel { dx: i32, dy: i32 },
    /// `usage` is a USB HID usage ID from the keyboard page (0x07).
    Key { usage: u16, down: bool },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_json_shape() {
        let m = Message::Input(InputEvent::Key { usage: 0x04, down: true });
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(s, r#"{"type":"input","kind":"key","usage":4,"down":true}"#);
        assert_eq!(serde_json::from_str::<Message>(&s).unwrap(), m);
    }
}
