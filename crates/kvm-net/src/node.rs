//! The peer mesh. Every machine runs one `KvmNode`; it advertises itself,
//! connects to the others over Noise, pairs with new machines, and keeps the
//! shared [`Layout`] in sync between paired members.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context};
use kvm_core::protocol::{Message, PROTOCOL_VERSION};
use kvm_core::{Layout, LayoutError, NodeId, NodeInfo, Trust};
use serde::Serialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, Notify};
use tokio::time::{interval, timeout};
use tracing::{debug, info, warn};

use crate::discovery::{Discovery, PeerAdvert, PeerEvent};
use crate::transport::{handshake, Identity, Session};

const PING_EVERY: Duration = Duration::from_secs(5);
/// No frame (not even a ping) for this long and the peer is considered gone,
/// e.g. a laptop that went to sleep without closing its socket.
const READ_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REDIAL_EVERY: Duration = Duration::from_secs(5);

pub struct NodeConfig {
    /// `info.key` is filled in from `identity`.
    pub info: NodeInfo,
    pub identity: Identity,
    /// Persisted layout from the last run (or `Layout::default()`).
    pub layout: Layout,
    /// TCP port to listen on; 0 picks a free one (mDNS advertises it).
    pub port: u16,
    /// Disable to only use [`KvmNode::connect`] (tests, routed networks).
    pub discovery: bool,
}

/// A connected machine that isn't a household member yet.
#[derive(Debug, Clone, Serialize)]
pub struct PendingPairing {
    pub node: NodeInfo,
    /// Shown on both machines; the user checks they match before accepting.
    pub code: String,
    pub accepted_here: bool,
}

#[derive(Debug, Clone)]
pub enum Event {
    PeerConnected(NodeInfo),
    PeerDisconnected(NodeId),
    PeerInfo(NodeInfo),
    /// The layout changed, locally or from a peer. Persist it.
    LayoutChanged(Layout),
    PairingRequested(PendingPairing),
    /// A pending pairing was accepted, declined or went away.
    PairingChanged,
    /// Input-path messages (`Enter`, `Input`, `Claim`) for the input engine.
    Message { from: NodeId, msg: Message },
}

#[derive(Clone)]
pub struct KvmNode {
    inner: Arc<Inner>,
}

struct Inner {
    id: NodeId,
    identity: Identity,
    local_addr: SocketAddr,
    state: Mutex<State>,
    events: broadcast::Sender<Event>,
    discovery: Mutex<Option<Discovery>>,
}

struct State {
    info: NodeInfo,
    layout: Layout,
    peers: HashMap<NodeId, Peer>,
    known: HashMap<NodeId, PeerAdvert>,
    dialing: HashSet<NodeId>,
    /// Pairing requests refused this session; their connections are dropped.
    declined: HashSet<NodeId>,
    next_conn: u64,
}

struct Peer {
    info: NodeInfo,
    /// Distinguishes a replaced connection from the current one on teardown.
    conn: u64,
    tx: mpsc::Sender<Message>,
    kill: Arc<Notify>,
    /// Paired member. Untrusted peers can only exchange pairing messages.
    trusted: bool,
    code: String,
    /// Layout we sent with our `PairAccept`, and the one they sent with theirs.
    accepted_here: Option<Layout>,
    accepted_there: Option<Layout>,
}

impl KvmNode {
    pub async fn start(mut cfg: NodeConfig) -> anyhow::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, cfg.port))
            .await
            .with_context(|| format!("binding TCP port {}", cfg.port))?;
        let local_addr = listener.local_addr()?;
        let id = cfg.info.id;
        cfg.info.key = cfg.identity.public_hex();

        let mut layout = cfg.layout;
        if layout.sync_node(&cfg.info) {
            layout.touch(id);
        }

        let (events, _) = broadcast::channel(256);
        let node = KvmNode {
            inner: Arc::new(Inner {
                id,
                identity: cfg.identity,
                local_addr,
                state: Mutex::new(State {
                    info: cfg.info.clone(),
                    layout,
                    peers: HashMap::new(),
                    known: HashMap::new(),
                    dialing: HashSet::new(),
                    declined: HashSet::new(),
                    next_conn: 0,
                }),
                events,
                discovery: Mutex::new(None),
            }),
        };

        tokio::spawn(node.clone().accept_loop(listener));
        if cfg.discovery {
            let (disc, rx) = Discovery::start(&cfg.info, local_addr.port())?;
            *node.inner.discovery.lock().unwrap() = Some(disc);
            tokio::spawn(node.clone().discovery_loop(rx));
            tokio::spawn(node.clone().redial_loop());
        }
        info!(%id, %local_addr, "node started");
        Ok(node)
    }

    pub fn id(&self) -> NodeId {
        self.inner.id
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.inner.local_addr
    }

    pub fn info(&self) -> NodeInfo {
        self.state().info.clone()
    }

    pub fn layout(&self) -> Layout {
        self.state().layout.clone()
    }

    /// Connected, paired peers.
    pub fn peers(&self) -> Vec<NodeInfo> {
        self.state().peers.values().filter(|p| p.trusted).map(|p| p.info.clone()).collect()
    }

    pub fn pending_pairings(&self) -> Vec<PendingPairing> {
        self.state()
            .peers
            .values()
            .filter(|p| !p.trusted)
            .map(|p| PendingPairing {
                node: p.info.clone(),
                code: p.code.clone(),
                accepted_here: p.accepted_here.is_some(),
            })
            .collect()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    /// Apply a local edit. On success (and if anything changed) the revision
    /// is bumped and the new layout is sent to every peer.
    pub fn edit_layout<R>(
        &self,
        f: impl FnOnce(&mut Layout) -> Result<R, LayoutError>,
    ) -> Result<R, LayoutError> {
        let (r, snapshot) = {
            let mut s = self.state();
            let mut l = s.layout.clone();
            let r = f(&mut l)?;
            if l == s.layout {
                return Ok(r);
            }
            l.touch(self.id());
            s.layout = l.clone();
            enforce_trust(&s);
            (r, l)
        };
        self.emit(Event::LayoutChanged(snapshot.clone()));
        self.broadcast(Message::Layout(snapshot));
        Ok(r)
    }

    /// Our displays or name changed (monitor plugged in, laptop docked).
    pub fn update_self(&self, mut info: NodeInfo) {
        info.key = self.inner.identity.public_hex();
        let layout = {
            let mut s = self.state();
            s.info = info.clone();
            s.layout.sync_node(&info).then(|| {
                s.layout.touch(self.inner.id);
                s.layout.clone()
            })
        };
        self.broadcast(Message::NodeUpdate(info));
        if let Some(l) = layout {
            self.emit(Event::LayoutChanged(l.clone()));
            self.broadcast(Message::Layout(l));
        }
    }

    /// The user checked the code and accepts `id` into the household.
    /// Pairing completes once the other machine accepts too.
    pub fn accept_pairing(&self, id: NodeId) -> bool {
        let layout = {
            let mut s = self.state();
            let layout = s.layout.clone();
            let Some(p) = s.peers.get_mut(&id).filter(|p| !p.trusted) else { return false };
            if p.accepted_here.is_some() {
                return true;
            }
            p.accepted_here = Some(layout.clone());
            let _ = p.tx.try_send(Message::PairAccept { layout: layout.clone() });
            layout
        };
        debug!(%id, revision = layout.revision, "pairing accepted here");
        self.emit(Event::PairingChanged);
        self.try_complete_pairing(id);
        true
    }

    /// Refuse `id` for the rest of this session and drop its connection.
    pub fn decline_pairing(&self, id: NodeId) {
        let mut s = self.state();
        s.declined.insert(id);
        if let Some(p) = s.peers.get(&id).filter(|p| !p.trusted) {
            p.kill.notify_one();
        }
        drop(s);
        self.emit(Event::PairingChanged);
    }

    /// Send to one paired peer. Returns false if not connected or its queue is full.
    pub fn send(&self, to: NodeId, msg: Message) -> bool {
        self.state()
            .peers
            .get(&to)
            .is_some_and(|p| p.trusted && p.tx.try_send(msg).is_ok())
    }

    /// Send to every paired peer.
    pub fn broadcast(&self, msg: Message) {
        for p in self.state().peers.values().filter(|p| p.trusted) {
            let _ = p.tx.try_send(msg.clone());
        }
    }

    /// Connect to a peer directly, bypassing mDNS.
    pub async fn connect(&self, addr: SocketAddr) -> anyhow::Result<()> {
        let stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await??;
        tokio::spawn(self.clone().run_conn(stream, true));
        Ok(())
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap()
    }

    fn emit(&self, ev: Event) {
        // No subscribers is fine.
        let _ = self.inner.events.send(ev);
    }

    async fn accept_loop(self, listener: TcpListener) {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::spawn(self.clone().run_conn(stream, false));
                }
                Err(e) => {
                    warn!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }

    async fn discovery_loop(self, mut rx: mpsc::UnboundedReceiver<PeerEvent>) {
        while let Some(ev) = rx.recv().await {
            match ev {
                PeerEvent::Up(advert) => {
                    if advert.proto != PROTOCOL_VERSION {
                        warn!(peer = %advert.name, proto = advert.proto, "incompatible protocol, ignoring");
                        continue;
                    }
                    debug!(peer = %advert.name, addrs = ?advert.addrs, "discovered");
                    let id = advert.id;
                    self.state().known.insert(id, advert);
                    self.maybe_dial(id);
                }
                PeerEvent::Down(id) => {
                    self.state().known.remove(&id);
                }
            }
        }
    }

    async fn redial_loop(self) {
        let mut tick = interval(REDIAL_EVERY);
        loop {
            tick.tick().await;
            let ids: Vec<NodeId> = self.state().known.keys().copied().collect();
            for id in ids {
                self.maybe_dial(id);
            }
        }
    }

    /// Exactly one side of each pair dials: the one with the lower id.
    fn maybe_dial(&self, id: NodeId) {
        let addrs = {
            let mut s = self.state();
            if self.inner.id > id
                || s.peers.contains_key(&id)
                || s.dialing.contains(&id)
                || s.declined.contains(&id)
            {
                return;
            }
            let Some(advert) = s.known.get(&id) else { return };
            let addrs = advert.addrs.clone();
            s.dialing.insert(id);
            addrs
        };
        let node = self.clone();
        tokio::spawn(async move {
            for addr in addrs {
                match timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
                    Ok(Ok(stream)) => {
                        node.clone().run_conn(stream, true).await;
                        break;
                    }
                    Ok(Err(e)) => debug!(%addr, "dial failed: {e}"),
                    Err(_) => debug!(%addr, "dial timed out"),
                }
            }
            node.state().dialing.remove(&id);
        });
    }

    async fn run_conn(self, stream: TcpStream, initiator: bool) {
        let addr = stream.peer_addr().ok();
        if let Err(e) = self.conn_inner(stream, initiator).await {
            debug!(?addr, "connection ended: {e:#}");
        }
    }

    async fn conn_inner(&self, stream: TcpStream, initiator: bool) -> anyhow::Result<()> {
        stream.set_nodelay(true)?;
        let (rd, wr) = stream.into_split();
        let Session { mut reader, mut writer, remote_key, code } =
            timeout(READ_TIMEOUT, handshake(rd, wr, &self.inner.identity, initiator))
                .await
                .context("handshake timed out")??;
        let key = hex::encode(remote_key);

        writer.send(&Message::Hello { proto: PROTOCOL_VERSION, node: self.info() }).await?;
        let Some(Message::Hello { proto, node }) = timeout(READ_TIMEOUT, reader.recv()).await?? else {
            bail!("expected hello");
        };
        ensure!(proto == PROTOCOL_VERSION, "peer speaks protocol {proto}, we speak {PROTOCOL_VERSION}");
        ensure!(node.id != self.inner.id, "connected to ourselves");
        ensure!(node.key == key, "{} claimed a key it didn't prove", node.name);

        let peer_id = node.id;
        let (tx, mut rx) = mpsc::channel::<Message>(256);
        let kill = Arc::new(Notify::new());
        let (conn, trusted) = {
            let mut s = self.state();
            if s.declined.contains(&peer_id) {
                bail!("pairing with {} was declined", node.name);
            }
            let trusted = match s.layout.trust(peer_id, &key) {
                Trust::Member => true,
                Trust::Unknown => false,
                Trust::KeyMismatch => {
                    warn!(peer = %node.name, "key differs from the paired one; forget it to pair again");
                    bail!("key mismatch");
                }
            };
            if s.peers.contains_key(&peer_id) {
                bail!("already connected to {}", node.name);
            }
            s.next_conn += 1;
            let conn = s.next_conn;
            s.peers.insert(
                peer_id,
                Peer {
                    info: node.clone(),
                    conn,
                    tx: tx.clone(),
                    kill: kill.clone(),
                    trusted,
                    code: code.clone(),
                    accepted_here: None,
                    accepted_there: None,
                },
            );
            (conn, trusted)
        };
        if trusted {
            info!(peer = %node.name, id = %peer_id, "peer connected");
            let _ = tx.try_send(Message::Layout(self.layout()));
            self.emit(Event::PeerConnected(node));
        } else {
            info!(peer = %node.name, %code, "pairing requested");
            self.emit(Event::PairingRequested(PendingPairing { node, code, accepted_here: false }));
        }

        let writer_task = tokio::spawn(async move {
            let mut ping = interval(PING_EVERY);
            let mut nonce = 0u64;
            loop {
                let msg = tokio::select! {
                    m = rx.recv() => match m { Some(m) => m, None => break },
                    _ = ping.tick() => { nonce += 1; Message::Ping { nonce } }
                };
                if writer.send(&msg).await.is_err() {
                    break;
                }
            }
        });

        let result = loop {
            tokio::select! {
                r = timeout(READ_TIMEOUT, reader.recv()) => match r {
                    Ok(Ok(Some(msg))) => self.handle(peer_id, msg),
                    Ok(Ok(None)) => break Ok(()),
                    Ok(Err(e)) => break Err(e.into()),
                    Err(_) => break Err(anyhow!("peer timed out")),
                },
                _ = kill.notified() => break Err(anyhow!("closed locally")),
            }
        };

        writer_task.abort();
        let removed = {
            let mut s = self.state();
            if s.peers.get(&peer_id).is_some_and(|p| p.conn == conn) {
                s.peers.remove(&peer_id)
            } else {
                None
            }
        };
        match removed {
            Some(p) if p.trusted => {
                info!(id = %peer_id, "peer disconnected");
                self.emit(Event::PeerDisconnected(peer_id));
            }
            Some(_) => self.emit(Event::PairingChanged),
            None => {}
        }
        result
    }

    fn handle(&self, from: NodeId, msg: Message) {
        let Some(trusted) = self.state().peers.get(&from).map(|p| p.trusted) else { return };
        match msg {
            Message::Ping { nonce } => {
                if let Some(p) = self.state().peers.get(&from) {
                    let _ = p.tx.try_send(Message::Pong { nonce });
                }
            }
            Message::Pong { .. } => {}
            Message::Hello { .. } => warn!(%from, "unexpected second hello"),
            Message::PairAccept { layout } => {
                if trusted {
                    return;
                }
                if let Some(p) = self.state().peers.get_mut(&from) {
                    p.accepted_there = Some(layout);
                }
                self.emit(Event::PairingChanged);
                self.try_complete_pairing(from);
            }
            _ if !trusted => debug!(%from, "ignoring message from unpaired peer"),
            Message::NodeUpdate(info) => {
                let mut s = self.state();
                let Some(p) = s.peers.get_mut(&from) else { return };
                if info.id != from || info.key != p.info.key {
                    warn!(%from, "ignoring node update with a different id or key");
                    return;
                }
                p.info = info.clone();
                drop(s);
                self.emit(Event::PeerInfo(info));
            }
            Message::Layout(layout) => {
                self.merge_layout(layout, Some(from));
            }
            msg @ (Message::Enter { .. } | Message::Input(_) | Message::Claim { .. }) => {
                self.emit(Event::Message { from, msg });
            }
        }
    }

    /// Both sides accepted: make the peer a member and adopt the joint layout.
    fn try_complete_pairing(&self, id: NodeId) {
        let (joined, peer) = {
            let mut s = self.state();
            let me = s.info.clone();
            let Some(p) = s.peers.get_mut(&id) else { return };
            let (Some(mine), Some(theirs)) = (&p.accepted_here, &p.accepted_there) else { return };
            let joined = Layout::join(mine, &me, theirs, &p.info);
            p.trusted = true;
            (joined, p.info.clone())
        };
        info!(peer = %peer.name, "paired");
        self.emit(Event::PairingChanged);
        self.emit(Event::PeerConnected(peer.clone()));
        if !self.merge_layout(joined, None) {
            // Our layout moved on since we accepted; add the member directly.
            let _ = self.edit_layout(|l| Ok(l.sync_node(&peer)));
        }
    }

    /// Adopt `remote` if it's newer, refreshing our own placement if it has
    /// stale displays, and gossip it on. Returns whether it was adopted.
    fn merge_layout(&self, remote: Layout, from: Option<NodeId>) -> bool {
        let (adopted, resynced) = {
            let mut s = self.state();
            if !remote.supersedes(&s.layout) {
                return false;
            }
            if remote.placement(self.inner.id).is_none() {
                // We were forgotten. Re-adding ourselves would undo that, so
                // keep our old layout; members are about to disconnect us.
                warn!("this machine was removed from the household");
                return false;
            }
            s.layout = remote;
            let info = s.info.clone();
            let resynced = s.layout.sync_node(&info);
            if resynced {
                s.layout.touch(self.inner.id);
            }
            enforce_trust(&s);
            (s.layout.clone(), resynced)
        };
        self.emit(Event::LayoutChanged(adopted.clone()));
        let msg = Message::Layout(adopted);
        for (id, p) in self.state().peers.iter().filter(|(_, p)| p.trusted) {
            // The sender already has it, unless we changed it.
            if resynced || Some(*id) != from {
                let _ = p.tx.try_send(msg.clone());
            }
        }
        true
    }
}

/// Drop connections to peers the layout no longer lists as members
/// (someone pressed "Forget" on this or another machine).
fn enforce_trust(s: &State) {
    for (id, p) in s.peers.iter().filter(|(_, p)| p.trusted) {
        if s.layout.trust(*id, &p.info.key) != Trust::Member {
            info!(peer = %p.info.name, "membership revoked, disconnecting");
            p.kill.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvm_core::{DisplayInfo, Os, Rect};

    fn info(name: &str) -> NodeInfo {
        NodeInfo {
            id: NodeId::new(),
            name: name.into(),
            os: Os::current(),
            portable: false,
            key: String::new(),
            displays: vec![DisplayInfo { index: 0, name: "D0".into(), rect: Rect::new(0, 0, 1920, 1080), primary: true }],
        }
    }

    async fn start(name: &str) -> KvmNode {
        let _ = tracing_subscriber::fmt().with_test_writer().with_env_filter("kvm_net=debug").try_init();
        KvmNode::start(NodeConfig {
            info: info(name),
            identity: Identity::generate(),
            layout: Layout::default(),
            port: 0,
            discovery: false,
        })
        .await
        .unwrap()
    }

    async fn dial(from: &KvmNode, to: &KvmNode) {
        from.connect(SocketAddr::from((Ipv4Addr::LOCALHOST, to.local_addr().port()))).await.unwrap();
    }

    async fn eventually(what: &str, f: impl Fn() -> bool) {
        timeout(Duration::from_secs(5), async {
            while !f() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for: {what}"));
    }

    async fn pair(a: &KvmNode, b: &KvmNode) {
        dial(b, a).await;
        let code = |n: &KvmNode, id: NodeId| n.pending_pairings().into_iter().find(|p| p.node.id == id).map(|p| p.code);
        eventually("both see a pending pairing", || code(a, b.id()).is_some() && code(b, a.id()).is_some()).await;
        assert_eq!(code(a, b.id()), code(b, a.id()));
        assert!(a.accept_pairing(b.id()));
        assert!(b.accept_pairing(a.id()));
        let has = |n: &KvmNode, id: NodeId| n.peers().iter().any(|p| p.id == id);
        eventually("paired", || has(a, b.id()) && has(b, a.id())).await;
    }

    #[tokio::test]
    async fn unpaired_peers_get_nothing() {
        let a = start("a").await;
        let b = start("b").await;
        a.edit_layout(|l| Ok(l.add_room("Secret lab"))).unwrap();
        dial(&b, &a).await;
        eventually("pending", || a.pending_pairings().len() == 1).await;
        assert!(a.peers().is_empty() && b.peers().is_empty());
        // Messages to an unpaired peer aren't delivered, and layouts don't sync.
        assert!(!a.send(b.id(), Message::Claim { epoch: 1 }));
        a.edit_layout(|l| Ok(l.add_room("Another"))).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(b.layout().rooms.is_empty());
        assert!(b.layout().placement(a.id()).is_none());
    }

    #[tokio::test]
    async fn pairing_joins_the_bigger_household() {
        let a = start("a").await;
        let b = start("b").await;
        let room = a.edit_layout(|l| Ok(l.add_room("Office"))).unwrap();
        // b has more edits (a higher revision) but a smaller household.
        for i in 0..20 {
            b.edit_layout(|l| Ok(l.add_room(format!("junk {i}")))).unwrap();
        }
        pair(&a, &b).await;

        eventually("layouts converge", || a.layout() == b.layout()).await;
        let l = a.layout();
        // Both households have one member, so the higher revision (b) wins.
        assert_eq!(l.rooms.len(), 20);
        assert!(!l.rooms.iter().any(|r| r.id == room));
        assert_eq!(l.trust(a.id(), &a.info().key), Trust::Member);
        assert_eq!(l.trust(b.id(), &b.info().key), Trust::Member);

        // A third machine pairing with either joins the 2-member household.
        let c = start("c").await;
        for i in 0..50 {
            c.edit_layout(|l| Ok(l.add_room(format!("c {i}")))).unwrap();
        }
        let household = a.layout().rooms;
        pair(&a, &c).await;
        eventually("c converges", || c.layout() == a.layout()).await;
        assert_eq!(c.layout().rooms, household);

        // And b trusts c without pairing again, because membership synced.
        eventually("b learns c", || b.layout().trust(c.id(), &c.info().key) == Trust::Member).await;
        dial(&c, &b).await;
        eventually("b and c connected", || b.peers().len() == 2 && c.peers().len() == 2).await;
        assert!(b.pending_pairings().is_empty());
    }

    #[tokio::test]
    async fn forgetting_a_machine_disconnects_it_everywhere() {
        let a = start("a").await;
        let b = start("b").await;
        let c = start("c").await;
        pair(&a, &b).await;
        pair(&a, &c).await;
        dial(&c, &b).await;
        eventually("mesh", || b.peers().len() == 2).await;

        let c_id = c.id();
        b.edit_layout(|l| l.remove_node(c_id)).unwrap();
        eventually("c dropped by a and b", || a.peers().len() == 1 && b.peers().len() == 1).await;
        eventually("c alone", || c.peers().is_empty()).await;

        // Reconnecting now needs pairing again.
        dial(&c, &a).await;
        eventually("pending again", || a.pending_pairings().len() == 1).await;
    }

    #[tokio::test]
    async fn declined_pairing_drops_the_connection() {
        let a = start("a").await;
        let b = start("b").await;
        dial(&b, &a).await;
        eventually("pending", || a.pending_pairings().len() == 1).await;
        a.decline_pairing(b.id());
        eventually("dropped", || a.pending_pairings().is_empty() && b.pending_pairings().is_empty()).await;
        dial(&b, &a).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(a.pending_pairings().is_empty());
    }
}
