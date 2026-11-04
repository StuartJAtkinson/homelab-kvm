# Considerations — homelab-kvm

Open questions that need a human decision. Each bullet names the evidence to
look at; the question is the bold part at the end.

- The app icon is still the stock Tauri placeholder — two generic monitors on a blue rounded square, used at 32×32, 128×128 and in the `.ico`. **Keep the placeholder, or design homelab-kvm's own mark?** Evidence: `app/src-tauri/icons/128x128.png` (also `app/src-tauri/icons/32x32.png`, `app/src-tauri/icons/icon.ico`).

- The same ✕ glyph is a bordered box in the room header (`button.icon`, 25×20px, border #dde1e7) and a bare glyph on machine labels (`.lock`, 20×17px, no border) — and the machine label's ✕ *forgets* a machine while the room header's ✕ *deletes a room*. **Which treatment should every destructive ✕ use, and should forgetting a machine wear the same look as deleting a room?** Options (a) boxed, (b) bare glyph, (c) boxed and tinted #dc2626, side by side: `.atelier/views/icon-buttons.html`.

- One state, four words: "locked in place"/"movable" (editor tooltips and toast), "locked"/"unlocked" (`kvmd layout` output), "fix or free" (`kvmd` help) and "mobile" (README). **Which pair do we use everywhere?** Options (a) locked/unlocked, (b) locked in place/movable, (c) fixed/free, each shown in both surfaces: `.atelier/views/lock-terminology.html`.
