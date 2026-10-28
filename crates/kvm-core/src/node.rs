use serde::{Deserialize, Serialize};

use crate::{NodeId, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Windows,
    Linux,
    Macos,
    Other,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "linux") {
            Os::Linux
        } else if cfg!(target_os = "macos") {
            Os::Macos
        } else {
            Os::Other
        }
    }
}

/// One physical monitor as the owning machine's OS sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub index: u32,
    /// Stable identity from EDID, in Windows' format (e.g.
    /// `DELA10F8ZSZTY2_2F_07E4_B4`); empty if unknown. The same monitor
    /// has the same id on any machine or OS it's plugged into.
    #[serde(default)]
    pub id: String,
    /// Human-readable model name, e.g. `DELL U2720Q`.
    pub name: String,
    /// Position in the machine's own desktop coordinates, physical pixels.
    pub rect: Rect,
    pub primary: bool,
}

/// What a machine reports about itself. Live data, not part of the layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: NodeId,
    pub name: String,
    pub os: Os,
    pub displays: Vec<DisplayInfo>,
    /// Heuristic (has a battery): new placements for laptops start unlocked.
    pub portable: bool,
    /// Hex X25519 static public key; must match the key proven in the
    /// Noise handshake.
    pub key: String,
}

impl NodeInfo {
    /// Bounding box of all displays in the node's own desktop coordinates.
    pub fn bounds(&self) -> Option<Rect> {
        Rect::bounding(self.displays.iter().map(|d| &d.rect))
    }
}
