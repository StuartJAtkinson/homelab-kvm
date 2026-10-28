//! Headless homelab-kvm node: discovery, layout sync and logging.
//!
//! ```text
//! kvmd [--port N] [--connect HOST:PORT]... [--no-discovery] [--no-input] [--config-dir DIR]
//! ```
//! `RUST_LOG=debug` for more detail.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context};
use kvm_net::{Event, KvmNode, NodeConfig, Store};
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{info, warn};

const DEFAULT_PORT: u16 = 24800;
const DISPLAY_POLL: Duration = Duration::from_secs(5);

struct Args {
    port: u16,
    connect: Vec<SocketAddr>,
    discovery: bool,
    input: bool,
    config_dir: Option<PathBuf>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = Args { port: DEFAULT_PORT, connect: vec![], discovery: true, input: true, config_dir: None };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut val = || it.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--port" => args.port = val()?.parse()?,
            "--connect" => args.connect.push(val()?.parse()?),
            "--no-discovery" => args.discovery = false,
            "--no-input" => args.input = false,
            "--config-dir" => args.config_dir = Some(val()?.into()),
            "-h" | "--help" => {
                println!("kvmd [--port N] [--connect HOST:PORT]... [--no-discovery] [--no-input] [--config-dir DIR]");
                std::process::exit(0);
            }
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(args)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = parse_args()?;

    let store = Store::open(args.config_dir)?;
    let id = store.node_id()?;
    let me = kvm_input::local_node_info(id)?;
    info!(name = %me.name, displays = me.displays.len(), portable = me.portable, config = %store.dir().display(), "starting");

    let node = KvmNode::start(NodeConfig {
        info: me,
        identity: store.identity()?,
        layout: store.load_layout(),
        port: args.port,
        discovery: args.discovery,
    })
    .await?;
    store.persist(&node);

    for addr in args.connect {
        if let Err(e) = node.connect(addr).await {
            warn!(%addr, "connect failed: {e:#}");
        }
    }

    // Kept alive until shutdown; dropping it releases any grab.
    let _agent = if args.input {
        match kvm_agent::Agent::start(node.clone()) {
            Ok(a) => {
                info!("input sharing on (Ctrl+Alt+Shift+Esc releases a stuck grab)");
                Some(a)
            }
            Err(e) => {
                warn!("input sharing disabled: {e:#}");
                None
            }
        }
    } else {
        None
    };

    tokio::spawn(watch_displays(node.clone()));
    tokio::spawn(log_events(node.clone()));
    tokio::spawn(console(node.clone()));

    tokio::signal::ctrl_c().await?;
    info!("shutting down");
    Ok(())
}

/// Pick up monitors being plugged in or rearranged.
async fn watch_displays(node: KvmNode) {
    let mut tick = tokio::time::interval(DISPLAY_POLL);
    loop {
        tick.tick().await;
        let Ok(displays) = kvm_input::displays() else { continue };
        let mut info = node.info();
        if info.displays != displays {
            info!(count = displays.len(), "displays changed");
            info.displays = displays;
            node.update_self(info);
        }
    }
}

async fn log_events(node: KvmNode) {
    let mut rx = node.subscribe();
    while let Ok(ev) = rx.recv().await {
        match ev {
            Event::LayoutChanged(l) => {
                let rooms: Vec<&str> = l.rooms.iter().map(|r| r.name.as_str()).collect();
                info!(revision = l.revision, ?rooms, nodes = l.placements.len(), "layout");
            }
            Event::PeerInfo(n) => info!(peer = %n.name, displays = n.displays.len(), "peer updated"),
            Event::PairingRequested(p) => info!(
                peer = %p.node.name,
                code = %p.code,
                "pairing requested: check the code matches the other machine, then type `pairs` and `accept N`"
            ),
            Event::Message { .. } | Event::PeerConnected(_) | Event::PeerDisconnected(_) | Event::PairingChanged => {}
        }
    }
}

/// Minimal stdin commands for pairing a headless machine.
async fn console(node: KvmNode) {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut words = line.split_whitespace();
        let pending = node.pending_pairings();
        let pick = |n: Option<&str>| n.and_then(|n| n.parse::<usize>().ok()).and_then(|i| pending.get(i));
        match (words.next(), words.next()) {
            (Some("pairs"), _) => {
                if pending.is_empty() {
                    println!("no pending pairings");
                }
                for (i, p) in pending.iter().enumerate() {
                    let state = if p.accepted_here { " (accepted here, waiting for them)" } else { "" };
                    println!("{i}: {} code {}{state}", p.node.name, p.code);
                }
            }
            (Some("accept"), n) => match pick(n) {
                Some(p) => {
                    node.accept_pairing(p.node.id);
                }
                None => println!("usage: accept N (see `pairs`)"),
            },
            (Some("decline"), n) => match pick(n) {
                Some(p) => node.decline_pairing(p.node.id),
                None => println!("usage: decline N (see `pairs`)"),
            },
            (Some("peers"), _) => {
                for p in node.peers() {
                    println!("{} {}", p.id, p.name);
                }
            }
            (None, _) => {}
            _ => println!("commands: pairs | accept N | decline N | peers"),
        }
    }
}
