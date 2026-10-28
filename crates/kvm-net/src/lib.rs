//! LAN discovery, framed transport and the peer mesh that keeps the layout
//! in sync. [`KvmNode`] is the entry point for both the daemon and the app.

pub mod discovery;
pub mod node;
pub mod store;
pub mod transport;

pub use discovery::{Discovery, PeerAdvert, PeerEvent, SERVICE_TYPE};
pub use node::{Event, KvmNode, NodeConfig, PendingPairing};
pub use transport::Identity;
pub use store::Store;
