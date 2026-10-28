//! Platform-independent model for homelab-kvm: node identity, monitor
//! geometry, the room layout, cursor crossing and the wire protocol.

pub mod geometry;
pub mod ids;
pub mod layout;
pub mod node;
pub mod protocol;

pub use geometry::{Point, Rect};
pub use ids::{NodeId, RoomId};
pub use layout::{Crossing, Layout, LayoutError, Placement, Room, Trust, WorldMonitor};
pub use node::{DisplayInfo, NodeInfo, Os};
