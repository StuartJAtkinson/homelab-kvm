//! Runs the input [`Engine`] against a live [`KvmNode`] and this machine's
//! input devices: local physical events and peer messages go in, and the
//! resulting actions are carried out (send, grab, warp, inject).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use kvm_core::engine::{Action, Engine, LocalEvent, Mode};
use kvm_input::{InputCapture, InputInjector};
use kvm_net::{Event, KvmNode};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Stops the agent (and releases any grab) when dropped.
pub struct Agent {
    task: JoinHandle<()>,
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Agent {
    /// Start with this machine's real input devices.
    pub fn start(node: KvmNode) -> anyhow::Result<Agent> {
        let (tx, rx) = mpsc::unbounded_channel();
        let capture = kvm_input::start_capture(tx)?;
        let injector = kvm_input::injector()?;
        Ok(Self::start_with(node, capture, rx, injector))
    }

    /// Start with explicit devices (tests, or a future Linux backend).
    pub fn start_with(
        node: KvmNode,
        capture: Box<dyn InputCapture>,
        local: mpsc::UnboundedReceiver<LocalEvent>,
        injector: Box<dyn InputInjector>,
    ) -> Agent {
        let events = node.subscribe();
        let runner = Runner { engine: Engine::new(node.id()), layout: node.layout(), node, capture, injector, last_drop_warning: None };
        Agent { task: tokio::spawn(runner.run(local, events)) }
    }
}

struct Runner {
    node: KvmNode,
    engine: Engine,
    layout: kvm_core::Layout,
    capture: Box<dyn InputCapture>,
    injector: Box<dyn InputInjector>,
    last_drop_warning: Option<Instant>,
}

impl Runner {
    async fn run(mut self, mut local: mpsc::UnboundedReceiver<LocalEvent>, mut events: tokio::sync::broadcast::Receiver<Event>) {
        loop {
            let before = self.engine.mode();
            let actions = tokio::select! {
                ev = local.recv() => match ev {
                    Some(ev) => self.engine.local(ev, &self.layout),
                    None => break,
                },
                ev = events.recv() => match ev {
                    Ok(Event::Message { from, msg }) => self.engine.remote(from, &msg, &self.layout),
                    Ok(Event::LayoutChanged(l)) => {
                        self.layout = l;
                        vec![]
                    }
                    Ok(Event::PeerDisconnected(id)) => self.engine.peer_lost(id),
                    Ok(_) => vec![],
                    Err(RecvError::Lagged(n)) => {
                        warn!(n, "input agent fell behind; dropped events");
                        // We may have missed a layout change.
                        self.layout = self.node.layout();
                        vec![]
                    }
                    Err(RecvError::Closed) => break,
                },
            };
            self.execute(actions);
            let after = self.engine.mode();
            if after != before {
                info!(?after, "input mode");
            }
        }
        self.capture.set_grab(false);
    }

    fn execute(&mut self, actions: Vec<Action>) {
        let mut queue = VecDeque::from(actions);
        while let Some(action) = queue.pop_front() {
            match action {
                Action::Send(to, msg) => {
                    if self.node.send(to, msg) {
                        continue;
                    }
                    if self.node.is_connected(to) {
                        // Queue full: drop this event rather than give up the peer.
                        let now = Instant::now();
                        if self.last_drop_warning.is_none_or(|t| now - t > Duration::from_secs(5)) {
                            warn!(%to, "peer can't keep up; dropping input");
                            self.last_drop_warning = Some(now);
                        }
                    } else {
                        queue.extend(self.engine.peer_lost(to));
                    }
                }
                Action::Grab(grab) => self.capture.set_grab(grab),
                Action::Warp(p) => {
                    if let Err(e) = self.injector.warp(p) {
                        debug!("warp failed: {e:#}");
                    }
                }
                Action::Inject(ev) => {
                    if let Err(e) = self.injector.inject(&ev) {
                        debug!("inject failed: {e:#}");
                    }
                }
            }
        }
    }
}

/// True while this machine's input is being forwarded elsewhere.
pub fn is_forwarding(mode: Mode) -> bool {
    matches!(mode, Mode::Forwarding { .. })
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use kvm_core::protocol::InputEvent;
    use kvm_core::{DisplayInfo, Layout, NodeId, NodeInfo, Os, Point, Rect};
    use kvm_net::{Identity, NodeConfig};

    use super::*;

    #[derive(Clone, Default)]
    struct Mock {
        grabbed: Arc<AtomicBool>,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl InputCapture for Mock {
        fn set_grab(&self, grab: bool) {
            self.grabbed.store(grab, Ordering::SeqCst);
        }
    }

    impl InputInjector for Mock {
        fn warp(&mut self, to: Point) -> anyhow::Result<()> {
            self.log.lock().unwrap().push(format!("warp {} {}", to.x, to.y));
            Ok(())
        }
        fn inject(&mut self, event: &InputEvent) -> anyhow::Result<()> {
            self.log.lock().unwrap().push(format!("{event:?}"));
            Ok(())
        }
    }

    async fn node(name: &str) -> KvmNode {
        let _ = tracing_subscriber::fmt().with_test_writer().with_env_filter("kvm_agent=debug").try_init();
        let info = NodeInfo {
            id: NodeId::new(),
            name: name.into(),
            os: Os::current(),
            portable: true,
            key: String::new(),
            displays: vec![DisplayInfo { index: 0, id: String::new(), name: "D".into(), rect: Rect::new(0, 0, 1920, 1080), primary: true }],
        };
        let cfg = NodeConfig { info, identity: Identity::generate(), layout: Layout::default(), port: 0, discovery: false };
        KvmNode::start(cfg).await.unwrap()
    }

    async fn eventually(what: &str, f: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !f() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for: {what}"));
    }

    #[tokio::test]
    async fn cursor_crosses_to_a_peer_types_and_comes_back() {
        let (a, b) = (node("desk").await, node("laptop").await);
        b.connect(SocketAddr::from((Ipv4Addr::LOCALHOST, a.local_addr().port()))).await.unwrap();
        eventually("pending", || a.pending_pairings().len() == 1 && b.pending_pairings().len() == 1).await;
        a.accept_pairing(b.id());
        b.accept_pairing(a.id());
        eventually("paired", || a.is_connected(b.id()) && b.is_connected(a.id())).await;

        // desk at x=0, laptop to its right.
        let (ai, bi) = (a.id(), b.id());
        a.edit_layout(|l| {
            let room = l.add_room("Office");
            l.place(ai, Some(room), Point::new(0, 0))?;
            l.place(bi, Some(room), Point::new(1920, 0))
        })
        .unwrap();
        eventually("b has the layout", || b.layout() == a.layout()).await;

        let (ma, mb) = (Mock::default(), Mock::default());
        let (a_tx, a_rx) = mpsc::unbounded_channel();
        let (_b_tx, b_rx) = mpsc::unbounded_channel();
        let _agent_a = Agent::start_with(a.clone(), Box::new(ma.clone()), a_rx, Box::new(ma.clone()));
        let _agent_b = Agent::start_with(b.clone(), Box::new(mb.clone()), b_rx, Box::new(mb.clone()));
        let blog = || mb.log.lock().unwrap().clone();

        // Push against the desk's right edge.
        a_tx.send(LocalEvent::Motion { pos: Point::new(1919, 500), delta: Point::new(0, 0) }).unwrap();
        eventually("laptop warped to its left edge", || blog().contains(&"warp 0 500".to_string())).await;
        assert!(ma.grabbed.load(Ordering::SeqCst));

        a_tx.send(LocalEvent::Motion { pos: Point::new(960, 540), delta: Point::new(50, 0) }).unwrap();
        a_tx.send(LocalEvent::Input(InputEvent::Key { usage: 0x04, down: true })).unwrap();
        a_tx.send(LocalEvent::Input(InputEvent::Key { usage: 0x04, down: false })).unwrap();
        eventually("moved and typed on the laptop", || {
            let log = blog();
            log.contains(&"warp 50 500".to_string()) && log.iter().any(|l| l.contains("down: false"))
        })
        .await;

        // Back left past the laptop's edge: the laptop hands the cursor back.
        a_tx.send(LocalEvent::Motion { pos: Point::new(960, 540), delta: Point::new(-100, 0) }).unwrap();
        eventually("desk ungrabbed", || !ma.grabbed.load(Ordering::SeqCst)).await;
        assert!(ma.log.lock().unwrap().contains(&"warp 1870 500".to_string()));
    }
}
