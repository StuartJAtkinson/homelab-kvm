//! Desktop app: runs a `KvmNode` and serves the layout editor.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::time::Duration;

use kvm_core::{Layout, LayoutError, NodeId, Point, RoomId};
use kvm_net::{Event, KvmNode, NodeConfig, PendingPairing, Store};
use serde::Serialize;
use tauri::{Emitter, Manager, State};

const PORT: u16 = 24800;
/// Snap distance for drops, in physical pixels.
const SNAP: i32 = 64;
const DISPLAY_POLL: Duration = Duration::from_secs(5);

struct App {
    node: KvmNode,
    /// Held for the app's lifetime; `None` if input sharing failed to start.
    _agent: Option<kvm_agent::Agent>,
}

/// Everything the editor needs to render.
#[derive(Serialize)]
struct Snapshot {
    me: NodeId,
    online: Vec<NodeId>,
    layout: Layout,
    pending: Vec<PendingPairing>,
}

type Reply = Result<Snapshot, String>;

fn snapshot_of(node: &KvmNode) -> Snapshot {
    let mut online: Vec<NodeId> = node.peers().iter().map(|p| p.id).collect();
    online.push(node.id());
    Snapshot { me: node.id(), online, layout: node.layout(), pending: node.pending_pairings() }
}

fn edit(app: &App, f: impl FnOnce(&mut Layout) -> Result<(), LayoutError>) -> Reply {
    app.node.edit_layout(f).map_err(|e| e.to_string())?;
    Ok(snapshot_of(&app.node))
}

#[tauri::command]
fn snapshot(app: State<App>) -> Snapshot {
    snapshot_of(&app.node)
}

#[tauri::command]
fn add_room(app: State<App>, name: String) -> Reply {
    edit(&app, |l| {
        l.add_room(name);
        Ok(())
    })
}

#[tauri::command]
fn rename_room(app: State<App>, room: RoomId, name: String) -> Reply {
    edit(&app, |l| l.rename_room(room, name))
}

#[tauri::command]
fn remove_room(app: State<App>, room: RoomId) -> Reply {
    edit(&app, |l| l.remove_room(room))
}

#[tauri::command]
fn set_locked(app: State<App>, node: NodeId, locked: bool) -> Reply {
    edit(&app, |l| l.set_locked(node, locked))
}

#[tauri::command]
fn accept_pairing(app: State<App>, node: NodeId) -> Reply {
    if !app.node.accept_pairing(node) {
        return Err("that machine is no longer waiting to pair".into());
    }
    Ok(snapshot_of(&app.node))
}

#[tauri::command]
fn decline_pairing(app: State<App>, node: NodeId) -> Snapshot {
    app.node.decline_pairing(node);
    snapshot_of(&app.node)
}

#[tauri::command]
fn forget_node(app: State<App>, node: NodeId) -> Reply {
    edit(&app, |l| l.remove_node(node))
}

/// Drop `node` at world `(x, y)` in `room` (or back in the tray for `None`).
#[tauri::command]
fn place(app: State<App>, node: NodeId, room: Option<RoomId>, x: i32, y: i32) -> Reply {
    edit(&app, |l| {
        let origin = match room {
            Some(r) => l.snap(node, r, Point::new(x, y), SNAP),
            None => Point::default(),
        };
        l.place(node, room, origin)
    })
}

async fn start_node() -> anyhow::Result<(KvmNode, Option<kvm_agent::Agent>)> {
    let store = Store::open(None)?;
    let me = kvm_input::local_node_info(store.node_id()?)?;
    let node = KvmNode::start(NodeConfig {
        info: me,
        identity: store.identity()?,
        layout: store.load_layout(),
        port: PORT,
        discovery: true,
    }).await?;
    store.persist(&node);

    let watcher = node.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(DISPLAY_POLL);
        loop {
            tick.tick().await;
            let Ok(displays) = kvm_input::displays() else { continue };
            let mut info = watcher.info();
            if info.displays != displays {
                info.displays = displays;
                watcher.update_self(info);
            }
        }
    });
    let agent = kvm_agent::Agent::start(node.clone())
        .inspect_err(|e| tracing::warn!("input sharing disabled: {e:#}"))
        .ok();
    Ok((node, agent))
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    tauri::Builder::default()
        .setup(|app| {
            let (node, agent) = tauri::async_runtime::block_on(start_node())?;

            // Tell the UI to re-fetch whenever something it shows changes.
            let handle = app.handle().clone();
            let mut rx = node.subscribe();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(Event::Message { .. }) => {}
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let _ = handle.emit("kvm-changed", ());
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            app.manage(App { node, _agent: agent });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            snapshot,
            add_room,
            rename_room,
            remove_room,
            set_locked,
            forget_node,
            accept_pairing,
            decline_pairing,
            place
        ])
        .run(tauri::generate_context!())
        .expect("error while running homelab-kvm");
}
