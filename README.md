# homelab-kvm

Share one mouse and keyboard across every machine in the house. Machines find
each other on the LAN automatically, and a drag-and-drop editor arranges their
monitors into rooms. Desktops with fixed monitors are **locked** in place;
laptops stay **mobile** and can be dragged around or into another room.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design.

## Status

Working now: discovery, encrypted (Noise) connections, pairing with a 6-digit
code, the peer mesh, layout sync and persistence, the layout model (rooms,
lock, snapping, overlap checks, cursor-crossing maths) and the layout editor.
**Input capture/forwarding isn't implemented yet.**

## Layout

```
crates/kvm-core    layout model, geometry, protocol (pure, unit-tested)
crates/kvm-net     mDNS discovery, TCP mesh, layout sync, persistence
crates/kvm-input   display enumeration, laptop detection; input backends later
crates/kvmd        headless daemon
app/src-tauri      desktop app (Tauri 2)
app/ui             layout editor (plain HTML/JS, no build step)
```

## Building

Needs Rust ([rustup](https://rustup.rs)).

```sh
cargo test                # core, net, input, daemon
cargo run -p kvmd         # headless node; RUST_LOG=debug for detail
```

New machines must be paired: both show the same 6-digit code; accept on both
(in the app's banner, or type `pairs` then `accept N` in a `kvmd` console).
Pairing with any one household member is enough.

`kvmd` listens on TCP 24800 and advertises `_homelab-kvm._tcp` over mDNS
(UDP 5353); allow both through the firewall. Use `--connect HOST:PORT` where
multicast doesn't reach (e.g. across VLANs). State lives in `%APPDATA%\homelab-kvm`
or `~/.config/homelab-kvm`.

### Desktop app

The app isn't in the workspace's default members because it needs extra
platform dependencies:

- **Windows:** the MSVC toolchain (Visual Studio Build Tools, "Desktop
  development with C++") and WebView2 (preinstalled on Windows 11).
- **Linux:** `webkit2gtk-4.1`, `libgtk-3-dev`, `librsvg2-dev` (see the
  [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)).

```sh
cargo run -p homelab-kvm
```

To iterate on the editor UI without building, open `app/ui/index.html` in a
browser: it runs against built-in mock data.
