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
| `crates/kvm-input`| Platform glue: displays with stable EDID ids, laptop detection, input capture (hooks) and injection (`SendInput`). |
| `crates/kvm-agent`| Runs the input `Engine` (in kvm-core) against a `KvmNode` and the platform devices. |
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
 └─ displays: [DisplayInfo { index, id (EDID), name, rect (OS desktop coords), primary }]
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

`kvm_core::engine::Engine` is a pure state machine (fully unit-tested);
`kvm-agent` feeds it local events and peer messages and performs its actions.

| Mode | Meaning |
|------|---------|
| `Idle` | The cursor is here; local input drives it as normal. |
| `Forwarding { to }` | We own the physical input. It's grabbed (swallowed locally) and sent to `to`. |
| `Controlled { by, pos }` | A peer's input drives our cursor; we track `pos`. |

1. **Leaving.** In `Idle`, every physical motion is checked with
   `Layout::cross(me, pos, delta)`. A cursor pinned against a screen edge
   reports zero delta, so each direction the motion isn't moving away from is
   tried with a 1px step too. A hit sends `Enter { target, x, y }`, grabs
   input (the cursor parks at the centre of the primary display) and switches
   to `Forwarding`.
2. **Driving.** The source sends `Input(MouseMove { dx, dy })` (post-acceleration
   deltas measured against the parked cursor), buttons, wheel and keys. The
   target moves its own tracked position, warps its cursor there, and clamps
   at edges with nothing beyond.
3. **Moving on.** When the target's cursor leaves its screens towards a
   neighbour, it sends `Handoff { to, x, y }` to the *source*, which either
   re-targets (`Enter` to the next machine) or, if `to` is itself, ungrabs and
   warps its own cursor there. Keys and buttons held down on the old target
   are released first, so nothing sticks.
4. **Taking back.** Physical input on a `Controlled` machine (someone touched
   it) sends `Claim` to the source, which ungrabs.

Safety valves: `Ctrl+Alt+Shift+Esc` ungrabs (checked inside the hook, so it
works even if the app is wedged); a dead event channel ungrabs; a peer
disconnecting ungrabs. Ctrl+Alt+Del never reaches hooks anyway.

Key codes on the wire are **USB HID usage IDs** (page 0x07), mapped to PC/AT
set-1 scancodes (`kvm-input::keymap`), which is what Windows hooks report and
what Linux evdev uses for the base keys.

Known gaps: the parked cursor stays visible on the source; modifiers held
*while* crossing stay down on the source; mixed DPI maps 1 physical pixel to
1 physical pixel.

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

## Platform input backends

| Platform | Capture | Inject | Notes |
|----------|---------|--------|-------|
| Windows *(done)* | `SetWindowsHookEx(WH_MOUSE_LL / WH_KEYBOARD_LL)` | `SendInput` | Can't inject into elevated windows unless the app runs elevated or as a service; secure desktop (UAC/lock screen) needs a SYSTEM service. |
| Linux X11 | XInput2 raw events + grab | XTest | Simple, well-trodden. |
| Linux Wayland | evdev read (`input` group) | `uinput` virtual device | Compositor-independent; needs udev rule. `libei` later where available. |

## Monitor identity and Windows' own arrangement

Findings from probing Windows 11 (see `cargo run -p kvm-input --example probe`):

- **Live arrangement:** `QueryDisplayConfig` (the CCD API, what Settings ->
  Display uses) gives each active source's desktop position and size, and
  `DisplayConfigGetDeviceInfo(GET_TARGET_NAME)` gives the monitor's friendly
  name and device path, e.g. `\\?\DISPLAY#DELA10F#<instance>#{guid}`. The
  device path names a registry key holding the raw EDID:
  `HKLM\SYSTEM\CurrentControlSet\Enum\DISPLAY\<hw id>\<instance>\Device Parameters\EDID`.
- **Windows' monitor ids:** Windows keys its saved arrangements by an id built
  from the EDID: `<PNP id><serial>_<week>_<year>_<checksum>`, e.g.
  `DELA10F8ZSZTY2_2F_07E4_B4` (serial = the EDID serial text, else the numeric
  serial in decimal). `kvm-input::edid` reproduces this exactly (tested
  against real EDIDs), and `DisplayInfo.id` carries it. It's computable from
  EDID on any OS (Linux exposes `/sys/class/drm/*/edid`), so a monitor keeps its
  id across machines.
- **Saved arrangements:** `HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers\Configuration`
  has one key per monitor *combination* ever connected, named by those ids
  joined with `*` (extended) or `+` (clone), holding per-source `Position.cx/cy`,
  `PrimSurfSize`, `Rotation` and so on. Readable without admin rights;
  the format is undocumented, so treat it as read-only.
- **Changing it:** the supported write path is `SetDisplayConfig` with
  `SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_SAVE_TO_DATABASE`, which
  applies a layout and saves it to that database.
- **Change notifications:** top-level windows receive `WM_DISPLAYCHANGE`; we
  currently poll every 5s instead.

What this enables (not built yet):

1. Recognise a docked laptop's external monitors by id and restore where that
   desk lives in the room layout ("the laptop is at the office desk").
2. Show Windows' saved arrangements for a machine in the editor.
3. Let the editor rearrange a machine's *own* monitors and push that to
   Windows with `SetDisplayConfig`, so the in-machine and room layouts stay
   consistent from one place.

## Roadmap

1. **Scaffold**: layout model + tests, discovery, peer mesh, layout sync,
   editor UI, daemon. *(done)*
2. Pairing + Noise transport. *(done)*
3. Windows capture/inject, crossing between two Windows machines. *(done;
   needs testing across two real machines)*
4. Linux backends.
5. Polish: tray icon, autostart, hotkeys, per-node DPI scale, clipboard text.
