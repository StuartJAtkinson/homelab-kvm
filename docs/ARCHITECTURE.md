# homelab-kvm architecture

Share one mouse and keyboard across every machine in the house. Machines find
each other on the LAN automatically, and a drag-and-drop editor arranges their
monitors into **rooms**. Desktops with fixed monitors are **locked** in place;
laptops stay **mobile** so they can be dragged around (or between rooms) as
they move.

## Goals

- Zero-config discovery on the local network (mDNS / DNS-SD).
- Peer-to-peer: every machine runs the same app. Whichever machine you are
  physically touching drives the cursor.
- A layout editor: rooms, monitor groups, drag and drop, lock/unlock.
- Windows and Linux (X11 first, Wayland after).

Non-goals for v1: clipboard sync, file drag, macOS, crossing the internet.

## Crates

| Path              | Role                                                                 |
|-------------------|----------------------------------------------------------------------|
| `crates/kvm-core` | Pure logic: IDs, geometry, layout model, cursor crossing, protocol. No I/O. |
| `crates/kvm-net`  | mDNS discovery, framed TCP transport, peer mesh, layout sync (`KvmNode`). |
| `crates/kvm-input`| Platform glue: display enumeration, laptop detection; input capture/injection (phase 2). |
| `crates/kvmd`     | Headless daemon (useful for servers / debugging).                    |
| `app/`            | Tauri desktop app: tray + layout editor UI (`app/ui`, plain HTML/JS). |

`kvmd` and the Tauri app both embed `KvmNode`; they are interchangeable on a
given machine (run one, not both).

## Layout model

```
Layout
 ├─ revision, author            last-writer-wins sync
 ├─ rooms: [Room { id, name }]
 └─ placements: NodeId → Placement { room: Option<RoomId>, origin: Point, locked: bool }

NodeInfo (reported live by each machine, not stored in the layout)
 └─ displays: [DisplayInfo { index, name, rect (OS desktop coords), primary }]
```

Key decisions:

- **A node's monitors move as one rigid group.** The OS already arranges a
  machine's own monitors and moves the cursor between them; we only position
  the whole group in the room. This means "lock" is per machine, which matches
  the real world: a desktop's three screens don't move, a laptop does.
- **World coordinates are per room, in physical pixels.** A display's world
  rect is `placement.origin + (display.rect - node_bounds.min)`. Rooms are
  independent planes; the cursor never crosses between rooms.
- **Unassigned nodes** (`room: None`) are discovered but parked in the editor
  tray; they receive no input until dropped into a room.
- **Locked** placements reject moves and room changes. New nodes default to
  locked unless they report a battery (laptop heuristic).
- **Overlap** between nodes in the same room is rejected; dragging snaps to
  neighbouring monitor edges (`Layout::snap`) so edges line up exactly.

Mixed DPI is handled by using physical pixels everywhere (processes declare
per-monitor DPI awareness). A 4K panel next to a 1080p one therefore looks
"twice as tall" in the editor, which is accurate for cursor travel. A later
option is a per-node scale so edges match physical size.

## Cursor crossing

1. The **active** node (the one with physical input) has a low-level hook.
2. When the cursor sits on the edge of its desktop and motion pushes outward,
   it calls `layout::cross(from, local_point, delta)`.
3. If a monitor of another node in the same room contains the projected world
   point, the active node sends `Enter { target, x, y }` to that peer, hides
   and parks its own cursor, and starts **forwarding** raw input as
   `Input(InputEvent)` messages. Local input is swallowed while forwarding.
4. The target injects events. When its cursor hits an edge, it runs the same
   `cross` computation and replies with `Enter` for the next hop (which may be
   the origin node, or a third node: the origin re-targets its stream).

### Ownership in a peer-to-peer mesh

Any node can become the source when someone touches its physical mouse or
keyboard. A node that sees local physical input while not the source broadcasts
`Claim { epoch }`; peers accept the highest epoch (ties broken by node id) and
stop forwarding. Injected events are tagged so hooks don't mistake them for
physical input.

Key codes on the wire are **USB HID usage IDs** (page 0x07), translated per
platform (Windows scan codes, Linux evdev codes). Hotkeys: `Scroll Lock` toggles
"stay on this machine"; `Ctrl+Alt+Shift+Esc` releases back to the source.

## Discovery and transport

- Service `_homelab-kvm._tcp.local.`, instance name = node UUID.
  TXT: `id`, `name`, `proto`.
- Each pair keeps one TCP connection. To avoid duplicates, the node with the
  **lower** UUID dials; the other only accepts.
- Every connection starts with a Noise handshake (below). After it, each
  `Message` is JSON, split into encrypted chunks of up to 64 KiB
  (`u16` length + ciphertext; first plaintext byte flags "more chunks").
  JSON keeps things easy to debug; input frames may move to `postcard` later.
  `TCP_NODELAY` is set: input latency matters more than throughput.
- Then both sides send `Hello { proto, node }`. Paired members exchange
  layouts, which merge last-writer-wins on `(revision, author)`. Every local
  edit bumps the revision and broadcasts the full layout (it's tiny).

## Security

Keystrokes include passwords, so nothing travels in plaintext.

- **Identity**: each machine has a long-term X25519 key pair
  (`identity.key` in the config dir; mode 0600 on Linux). Its public key is in
  `NodeInfo.key`.
- **Transport**: `Noise_XX_25519_ChaChaPoly_BLAKE2s` (via `snow`) with
  prologue `homelab-kvm`. XX gives mutual authentication and forward secrecy.
  `Hello.node.key` must equal the key the handshake proved, so a node can't
  claim someone else's id.
- **Membership lives in the layout**: a placement with a `key` is a paired
  household member. `Layout::trust(id, key)` returns `Member`,
  `Unknown` (never paired) or `KeyMismatch` (refused outright). Because the
  layout replicates, pairing a new machine with *any* member makes it trusted
  by all of them, and "Forget" on any machine revokes it everywhere: on each
  layout change, connections to non-members are dropped. A node never
  adopts a layout it has been removed from (otherwise it would re-add itself
  and gossip the reinstatement).
- **Pairing**: an `Unknown` peer gets a pending pairing showing a 6-digit code
  derived from the handshake hash. Matching codes on both screens rule out
  a man in the middle. Each side sends `PairAccept { layout }` once its user
  accepts; with both, each side computes the same `Layout::join`: the bigger
  household's layout wins (ties: higher revision), both machines are added, and
  the revision moves past both. So a fresh laptop joins the house rather
  than wiping it. Until then the peer receives nothing but pairing messages.
- **Decline** refuses the peer for the rest of the session.
- Discovery data (id, name, protocol version) is public by nature.

Known gaps: a forgotten machine isn't told it was removed (it just finds every
member hanging up); no key rotation; pairing is per-session if declined.

## Platform input backends (phase 2)

| Platform | Capture | Inject | Notes |
|----------|---------|--------|-------|
| Windows  | `SetWindowsHookEx(WH_MOUSE_LL / WH_KEYBOARD_LL)` | `SendInput` | Can't inject into elevated windows unless the app runs elevated or as a service; secure desktop (UAC/lock screen) needs a SYSTEM service. |
| Linux X11 | XInput2 raw events + grab | XTest | Simple, well-trodden. |
| Linux Wayland | evdev read (`input` group) | `uinput` virtual device | Compositor-independent; needs udev rule. `libei` later where available. |

## Roadmap

1. **Scaffold**: layout model + tests, discovery, peer mesh, layout sync,
   editor UI, daemon. *(done)*
2. Pairing + Noise transport. *(done)*
3. Windows capture/inject, crossing between two Windows machines.
4. Linux backends.
5. Polish: tray icon, autostart, hotkeys, per-node DPI scale, clipboard text.
