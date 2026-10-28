//! mDNS / DNS-SD advertisement and browsing.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};

use anyhow::Context;
use kvm_core::protocol::PROTOCOL_VERSION;
use kvm_core::{NodeId, NodeInfo};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;

pub const SERVICE_TYPE: &str = "_homelab-kvm._tcp.local.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAdvert {
    pub id: NodeId,
    pub name: String,
    pub proto: u32,
    pub addrs: Vec<SocketAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerEvent {
    Up(PeerAdvert),
    Down(NodeId),
}

/// Advertises this node and reports other nodes. Unregisters on drop.
pub struct Discovery {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Discovery {
    pub fn start(me: &NodeInfo, port: u16) -> anyhow::Result<(Self, mpsc::UnboundedReceiver<PeerEvent>)> {
        let daemon = ServiceDaemon::new().context("starting mDNS daemon")?;

        let instance = me.id.to_string();
        let host = format!("kvm-{}.local.", &instance[..8]);
        let proto = PROTOCOL_VERSION.to_string();
        let props = [("id", instance.as_str()), ("name", me.name.as_str()), ("proto", proto.as_str())];
        let info = ServiceInfo::new(SERVICE_TYPE, &instance, &host, "", port, &props[..])
            .context("building mDNS service info")?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        daemon.register(info).context("registering mDNS service")?;

        let browse = daemon.browse(SERVICE_TYPE).context("browsing mDNS")?;
        let (tx, rx) = mpsc::unbounded_channel();
        let me = me.id;
        std::thread::Builder::new().name("mdns-browse".into()).spawn(move || {
            // fullname -> id, since removals only carry the fullname.
            let mut seen: HashMap<String, NodeId> = HashMap::new();
            while let Ok(ev) = browse.recv() {
                let out = match ev {
                    ServiceEvent::ServiceResolved(info) => {
                        let Some(advert) = parse_advert(&info) else { continue };
                        if advert.id == me {
                            continue;
                        }
                        seen.insert(info.get_fullname().to_string(), advert.id);
                        PeerEvent::Up(advert)
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => match seen.remove(&fullname) {
                        Some(id) => PeerEvent::Down(id),
                        None => continue,
                    },
                    _ => continue,
                };
                if tx.send(out).is_err() {
                    break;
                }
            }
        })?;

        Ok((Self { daemon, fullname }, rx))
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

fn parse_advert(info: &ServiceInfo) -> Option<PeerAdvert> {
    let id = info.get_property_val_str("id")?.parse().ok()?;
    let name = info.get_property_val_str("name").unwrap_or("?").to_string();
    let proto = info.get_property_val_str("proto")?.parse().ok()?;
    let port = info.get_port();
    let mut addrs: Vec<SocketAddr> = info
        .get_addresses()
        .iter()
        .map(|ip| SocketAddr::new(*ip, port))
        .collect();
    // Prefer IPv4: link-local v6 needs a scope id we don't get here.
    addrs.sort_by_key(|a| !matches!(a.ip(), IpAddr::V4(_)));
    Some(PeerAdvert { id, name, proto, addrs })
}
