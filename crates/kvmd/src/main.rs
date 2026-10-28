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

const HELP: &str = "\
commands:
  pairs                      machines waiting to pair (with their codes)
  accept N | decline N       answer pairing request N
  peers                      connected machines
  layout                     rooms and machines (numbered)
  room NAME                  add a room
  place N ROOM X Y           put machine N in ROOM at X,Y (pixels)
  beside N M                 put machine N just right of machine M
  lock N | unlock N          fix or free machine N's position";

/// Stdin commands for pairing and arranging machines without the editor.
async fn console(node: KvmNode) {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let words: Vec<&str> = line.split_whitespace().collect();
        if let Err(e) = command(&node, &words) {
            println!("{e}");
        }
    }
}

fn command(node: &KvmNode, words: &[&str]) -> Result<(), String> {
    let pending = node.pending_pairings();
    let layout = node.layout();
    let num = |w: Option<&&str>| w.and_then(|w| w.parse::<usize>().ok());
    let pairing = |w| num(w).and_then(|i| pending.get(i)).ok_or("unknown pairing number (see `pairs`)");
    let machine = |w| {
        num(w)
            .and_then(|i| layout.placements.get(i))
            .map(|p| p.node)
            .ok_or("unknown machine number (see `layout`)")
    };
    let edited = |r: Result<(), kvm_core::LayoutError>| r.map_err(|e| e.to_string());

    match words {
        [] => {}
        ["pairs"] => {
            if pending.is_empty() {
                println!("no pending pairings");
            }
            for (i, p) in pending.iter().enumerate() {
                let state = if p.accepted_here { " (accepted here, waiting for them)" } else { "" };
                println!("{i}: {} code {}{state}", p.node.name, p.code);
            }
        }
        ["accept", ..] => {
            node.accept_pairing(pairing(words.get(1))?.node.id);
        }
        ["decline", ..] => node.decline_pairing(pairing(words.get(1))?.node.id),
        ["peers"] => {
            for p in node.peers() {
                println!("{} {}", p.id, p.name);
            }
        }
        ["layout"] => {
            let online: Vec<_> = node.peers().iter().map(|p| p.id).chain([node.id()]).collect();
            let room_name = |id| layout.rooms.iter().find(|r| Some(r.id) == id).map_or("-", |r| r.name.as_str());
            let rooms: Vec<&str> = layout.rooms.iter().map(|r| r.name.as_str()).collect();
            println!("rooms: {}", if rooms.is_empty() { "(none)".into() } else { rooms.join(", ") });
            for (i, p) in layout.placements.iter().enumerate() {
                let size: Vec<String> = p.displays.iter().map(|d| format!("{}x{}", d.rect.w, d.rect.h)).collect();
                println!(
                    "{i}: {}{} room={} at {},{} {} screens=[{}] {}",
                    p.name,
                    if p.node == node.id() { " (this machine)" } else { "" },
                    room_name(p.room),
                    p.origin.x,
                    p.origin.y,
                    if p.locked { "locked" } else { "unlocked" },
                    size.join(" "),
                    if online.contains(&p.node) { "online" } else { "offline" },
                );
            }
        }
        ["room", name @ ..] if !name.is_empty() => {
            let name = name.join(" ");
            edited(node.edit_layout(|l| {
                l.add_room(name);
                Ok(())
            }))?;
        }
        ["place", _, room, x, y] => {
            let n = machine(words.get(1))?;
            let room = layout.rooms.iter().find(|r| r.name.eq_ignore_ascii_case(room)).ok_or("no such room")?.id;
            let (x, y) = (x.parse().map_err(|_| "bad X")?, y.parse().map_err(|_| "bad Y")?);
            edited(node.edit_layout(|l| l.place(n, Some(room), kvm_core::Point::new(x, y))))?;
        }
        ["beside", _, _] => {
            let (n, m) = (machine(words.get(1))?, machine(words.get(2))?);
            if n == m {
                return Err("pick two different machines".into());
            }
            let anchor = layout.placement(m).ok_or("unknown machine")?;
            let room = anchor.room.ok_or("that machine isn't in a room yet")?;
            let width = kvm_core::Rect::bounding(anchor.displays.iter().map(|d| &d.rect)).map_or(0, |b| b.w);
            let at = anchor.origin + kvm_core::Point::new(width, 0);
            edited(node.edit_layout(|l| l.place(n, Some(room), at)))?;
        }
        [verb @ ("lock" | "unlock"), _] => {
            let n = machine(words.get(1))?;
            edited(node.edit_layout(|l| l.set_locked(n, *verb == "lock")))?;
        }
        _ => println!("{HELP}"),
    }
    Ok(())
}
