# UX — homelab-kvm

Visual conventions of this project, as the code actually follows them. Two
surfaces: the **editor** (Tauri app, `app/ui/`) and the **kvmd console**
(`crates/kvmd/`, plain text). Deviations are tracked in
[ISSUES.md](ISSUES.md); open decisions in [CONSIDERATIONS.md](CONSIDERATIONS.md).

## Colour

All colour is declared as tokens on `:root` (`app/ui/style.css:1-34`); no
other stylesheet, and no inline style in `app.js`, introduces a colour.

| Token | Light | Dark | Used for |
|---|---|---|---|
| `--bg` | `#f5f6f8` | `#0f1218` | page behind everything |
| `--panel` | `#ffffff` | `#171b23` | cards, rooms, bars, buttons |
| `--border` | `#dde1e7` | `#2a303c` | 1px hairlines, button borders |
| `--text` | `#1c2330` | `#e6e9ef` | body text, toast fill |
| `--muted` | `#687385` | `#8b94a5` | hints, section headings, counts |
| `--accent` | `#2563eb` | `#4f8cff` | primary button, drop target, pairing border |
| `--accent-soft` | `#dbe7fe` | `#1d2a45` | pairing banner fill, drop-target fill |
| `--screen` | `#1e293b` | `#3a4559` | secondary monitor rectangle |
| `--screen-primary` | `#2563eb` | `#4f8cff` | primary monitor rectangle |
| `--mine` | `#0f766e` | `#2dd4bf` | name of *this* machine |
| `--online` | `#16a34a` | *(not overridden)* | status dot when online |
| `--offline` | `#9aa3b2` | *(not overridden)* | status dot when offline |
| `--danger` | `#dc2626` | *(not overridden)* | error toast fill |
| `--grid` | `#eef0f3` | `#1b2029` | room-canvas grid lines |

`#ffffff` is the only literal colour outside the tokens, used for text on
solid fills (primary button, monitor labels, error toast). Dark mode is
driven by `prefers-color-scheme` alone; the `data-theme` attribute is only
half-wired (see ISSUES.md).

## Type

System UI stack: `system-ui, -apple-system, "Segoe UI", sans-serif`. Monospace
only for the pairing code and console output: `ui-monospace, "Cascadia Mono",
Consolas, monospace`.

| Size | Weight | Where |
|---|---|---|
| 22px | 600 | pairing code (letter-spacing `0.08em`) |
| 16px | 700 | app title in the header |
| 15px | 600 | room title (inline-editable) |
| 14px | 400 | body, buttons |
| 13px | 700, uppercase, letter-spacing `0.04em` | section heading (`UNASSIGNED`) |
| 13px | 400 | toast text |
| 12px | 400 | hints, member count, machine label, mode badge |
| 10px | 400 | monitor resolution text inside a screen |

## Spacing

A 4px base: `4`, `8`, `10`, `12`, `16`, `24`, `40`.
Page gutter and column gap are `16px`; the header is `10px 16px`; panel
padding is `12px`; card padding `8px`; the room header is `8px 12px` with an
`8px` gap; room contents are inset `40px` from the canvas edge; the canvas
grid squares are `24px`.

## Radii

`3px` monitor rectangles · `4px` room-title input · `6px` buttons ·
`8px` tray cards and toasts · `10px` panels, rooms and the pairing banner ·
`999px` the mode badge.

## Layout and control placement

- **Header** — title left, mode badge next to it, spacer, `+ Room` primary
  button right.
- **Pairing banner** — a full-width strip above the grid: who is asking
  (left), the 6-digit code in monospace (middle), accept/decline (right).
  Accept is the primary button.
- **Two columns** — `240px` `Unassigned` tray on the left, the stack of rooms
  on the right; one column at `≤760px`.
- **Room header** — room title as an inline-editable input on the left
  (transparent until hover/focus), member count, then the delete `✕` on the
  right.
- **Machine** — the monitors are one rigid group of rectangles; the label
  sits above the top-left of that group: status dot, name, lock glyph, and a
  `✕` to forget the machine when it is offline. The name of *this* machine is
  teal and suffixed `(this machine)`.
- **Console** — no chrome; `layout` and `pairs` print numbered lists, and
  every command takes those numbers.

## Terminology

The editor says **machine** (never "node"), **room**, **unassigned** (never
"tray" or "waiting"), **pairing code**, **locked**/**movable**. A pairing code
is six digits shown as `042 917`. Monitor rectangles are labelled with their
resolution, `2560×1440`. The console reuses the same words; the model calls
the same field `locked`.

## Editor geometry

The canvas draws at `1/14` of a physical pixel per canvas pixel; world
positions in the layout are physical pixels, so `kvmd`'s `place N ROOM X Y`
takes the same units the editor stores.
