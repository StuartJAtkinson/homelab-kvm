// Layout editor. Talks to the Rust side through Tauri commands; opened
// directly in a browser it runs against an in-memory mock instead.

const SCALE = 1 / 14; // canvas px per physical pixel
const PAD = 40;       // canvas padding around a room's contents

const backend = window.__TAURI__ ? tauriBackend() : mockBackend();
let state = null;     // { me, online: [id], layout: { rooms, placements } }

// ---------- backends ----------

function tauriBackend() {
  const { invoke } = window.__TAURI__.core;
  window.__TAURI__.event.listen("kvm-changed", () => refresh());
  return { call: (cmd, args) => invoke(cmd, args) };
}

function mockBackend() {
  document.getElementById("mode").hidden = false;
  const uuid = () => crypto.randomUUID();
  const disp = (i, x, y, w, h) => ({ index: i, name: `DISPLAY${i + 1}`, rect: { x, y, w, h }, primary: i === 0 });
  const office = uuid(), lounge = uuid();
  const desk = uuid(), media = uuid(), laptop = uuid(), old = uuid();
  const s = {
    me: laptop,
    online: [desk, media, laptop],
    pending: [{
      node: { id: uuid(), name: "kids-pc", os: "windows", displays: [], portable: false, key: "" },
      code: "761 355",
      accepted_here: false,
    }],
    layout: {
      revision: 1, author: null,
      rooms: [{ id: office, name: "Office" }, { id: lounge, name: "Lounge" }],
      placements: [
        { node: desk, name: "desk-pc", room: office, origin: { x: 0, y: 0 }, locked: true,
          displays: [disp(0, 0, 0, 2560, 1440), disp(1, 2560, 180, 1920, 1080)] },
        { node: media, name: "media-pc", room: lounge, origin: { x: 0, y: 0 }, locked: true,
          displays: [disp(0, 0, 0, 3840, 2160)] },
        { node: laptop, name: "laptop", room: null, origin: { x: 0, y: 0 }, locked: false,
          displays: [disp(0, 0, 0, 1920, 1200)] },
        { node: old, name: "old-nuc", room: null, origin: { x: 0, y: 0 }, locked: true,
          displays: [disp(0, 0, 0, 1920, 1080)] },
      ],
    },
  };
  const find = (id) => s.layout.placements.find((p) => p.node === id);
  const ops = {
    snapshot: () => {},
    add_room: ({ name }) => s.layout.rooms.push({ id: uuid(), name }),
    rename_room: ({ room, name }) => { s.layout.rooms.find((r) => r.id === room).name = name; },
    remove_room: ({ room }) => {
      if (s.layout.placements.some((p) => p.room === room && p.locked)) throw "room still contains locked nodes";
      s.layout.rooms = s.layout.rooms.filter((r) => r.id !== room);
      s.layout.placements.forEach((p) => { if (p.room === room) p.room = null; });
    },
    set_locked: ({ node, locked }) => { find(node).locked = locked; },
    forget_node: ({ node }) => { s.layout.placements = s.layout.placements.filter((p) => p.node !== node); },
    accept_pairing: ({ node }) => {
      const p = s.pending.find((p) => p.node.id === node);
      s.pending = s.pending.filter((x) => x !== p);
      s.online.push(node);
      s.layout.placements.push({ node, name: p.node.name, room: null, origin: { x: 0, y: 0 }, locked: true,
        displays: [disp(0, 0, 0, 2560, 1440)], key: "mock" });
    },
    decline_pairing: ({ node }) => { s.pending = s.pending.filter((p) => p.node.id !== node); },
    place: ({ node, room, x, y }) => {
      const p = find(node);
      if (p.locked) throw `node ${p.name} is locked`;
      p.room = room;
      p.origin = room ? { x, y } : { x: 0, y: 0 };
    },
  };
  return {
    call: async (cmd, args = {}) => {
      ops[cmd](args);
      s.layout.revision++;
      return structuredClone(s);
    },
  };
}

// ---------- geometry ----------

function bounds(displays) {
  if (!displays.length) return { x: 0, y: 0, w: 1920, h: 1080 };
  const l = Math.min(...displays.map((d) => d.rect.x));
  const t = Math.min(...displays.map((d) => d.rect.y));
  const r = Math.max(...displays.map((d) => d.rect.x + d.rect.w));
  const b = Math.max(...displays.map((d) => d.rect.y + d.rect.h));
  return { x: l, y: t, w: r - l, h: b - t };
}

/** World-space top-left of a room's contents; maps to (PAD, PAD) on canvas. */
function roomOrigin(placements) {
  if (!placements.length) return { x: 0, y: 0 };
  return {
    x: Math.min(...placements.map((p) => p.origin.x)),
    y: Math.min(...placements.map((p) => p.origin.y)),
  };
}

// ---------- rendering ----------

const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
};

function nodeElement(p) {
  const b = bounds(p.displays);
  const online = state.online.includes(p.node);
  const n = el("div", "node");
  n.classList.toggle("locked", p.locked);
  n.classList.toggle("offline", !online);
  n.classList.toggle("mine", p.node === state.me);
  n.dataset.node = p.node;
  n.style.width = `${b.w * SCALE}px`;
  n.style.height = `${b.h * SCALE}px`;

  for (const d of p.displays) {
    const s = el("div", "screen" + (d.primary ? " primary" : ""), `${d.rect.w}×${d.rect.h}`);
    s.style.left = `${(d.rect.x - b.x) * SCALE}px`;
    s.style.top = `${(d.rect.y - b.y) * SCALE}px`;
    s.style.width = `${d.rect.w * SCALE}px`;
    s.style.height = `${d.rect.h * SCALE}px`;
    s.title = d.id ? `${d.name}\n${d.id}` : d.name;
    n.append(s);
  }

  const label = el("div", "label");
  label.append(el("span", "dot" + (online ? " on" : "")));
  label.append(el("span", "name", p.node === state.me ? `${p.name} (this machine)` : p.name));
  const lock = el("button", "lock", p.locked ? "🔒" : "🔓");
  lock.title = p.locked ? "Locked in place — click to unlock" : "Movable — click to lock";
  lock.addEventListener("pointerdown", (e) => e.stopPropagation());
  lock.addEventListener("click", () => run("set_locked", { node: p.node, locked: !p.locked }));
  label.append(lock);
  if (!online && p.node !== state.me) {
    const forget = el("button", "lock", "✕");
    forget.title = "Forget this machine";
    forget.addEventListener("pointerdown", (e) => e.stopPropagation());
    forget.addEventListener("click", () => run("forget_node", { node: p.node }));
    label.append(forget);
  }
  n.append(label);

  n.addEventListener("pointerdown", (e) => startDrag(e, n, p));
  return n;
}

function renderPairings() {
  const host = document.getElementById("pairings");
  host.replaceChildren();
  for (const p of state.pending) {
    const row = el("div", "pairing");
    const who = el("div", "who", `${p.node.name} wants to join`);
    who.append(el("small", null, p.accepted_here
      ? `Waiting for ${p.node.name} to accept too…`
      : "Only accept if the same code is showing on that machine."));
    const code = el("div", "code", p.code);
    code.title = "Pairing code";
    const actions = el("div", "actions");
    if (!p.accepted_here) {
      const yes = el("button", "primary", "Codes match — accept");
      yes.addEventListener("click", () => run("accept_pairing", { node: p.node.id }));
      actions.append(yes);
    }
    const no = el("button", null, "Decline");
    no.addEventListener("click", () => run("decline_pairing", { node: p.node.id }));
    actions.append(no);
    row.append(who, code, actions);
    host.append(row);
  }
}

function render() {
  renderPairings();
  const { rooms, placements } = state.layout;

  const tray = document.getElementById("tray-items");
  tray.replaceChildren();
  for (const p of placements.filter((p) => !p.room || !rooms.some((r) => r.id === p.room))) {
    const card = el("div", "tray-card");
    const preview = el("div", "preview");
    const n = nodeElement(p);
    n.style.position = "relative";
    preview.append(n);
    card.append(preview);
    tray.append(card);
  }
  if (!tray.children.length) tray.append(el("p", "hint", "Nothing waiting."));

  const host = document.getElementById("rooms");
  host.replaceChildren();
  for (const room of rooms) {
    const members = placements.filter((p) => p.room === room.id);
    const r = el("div", "room");

    const head = el("div", "room-head");
    const name = el("input", "room-name");
    name.value = room.name;
    name.setAttribute("aria-label", "Room name");
    name.addEventListener("change", () => run("rename_room", { room: room.id, name: name.value.trim() || room.name }));
    name.addEventListener("keydown", (e) => { if (e.key === "Enter") name.blur(); });
    head.append(name, el("span", "room-count", `${members.length} machine${members.length === 1 ? "" : "s"}`));
    const del = el("button", "icon", "✕");
    del.title = "Delete room (machines go back to Unassigned)";
    del.addEventListener("click", () => run("remove_room", { room: room.id }));
    head.append(del);

    const canvas = el("div", "room-canvas");
    canvas.dataset.room = room.id;
    const o = roomOrigin(members);
    canvas.dataset.ox = o.x;
    canvas.dataset.oy = o.y;
    let maxX = 0, maxY = 0;
    for (const p of members) {
      const b = bounds(p.displays);
      const n = nodeElement(p);
      const x = PAD + (p.origin.x - o.x) * SCALE;
      const y = PAD + (p.origin.y - o.y) * SCALE;
      n.style.left = `${x}px`;
      n.style.top = `${y}px`;
      maxX = Math.max(maxX, x + b.w * SCALE);
      maxY = Math.max(maxY, y + b.h * SCALE);
      canvas.append(n);
    }
    // Room to drop beside the existing machines.
    const spacer = el("div");
    spacer.style.cssText = `position:absolute;width:1px;height:1px;left:${maxX + 200}px;top:${maxY + PAD}px`;
    canvas.append(spacer);
    if (!members.length) canvas.append(el("div", "empty", "Drag a machine here"));

    r.append(head, canvas);
    host.append(r);
  }
  if (!rooms.length) {
    host.append(el("p", "hint", "No rooms yet. Add one, then drag machines into it."));
  }
}

// ---------- drag and drop ----------

let drag = null;

function startDrag(e, n, p) {
  if (e.button !== 0) return;
  if (p.locked) {
    n.classList.remove("shake");
    void n.offsetWidth; // restart the animation
    n.classList.add("shake");
    toast("Locked in place. Click the lock to move it.");
    return;
  }
  e.preventDefault();
  const rect = n.getBoundingClientRect();
  const ghost = n.cloneNode(true);
  ghost.classList.add("ghost");
  ghost.style.left = `${rect.left}px`;
  ghost.style.top = `${rect.top}px`;
  document.body.append(ghost);
  n.classList.add("dragging");
  drag = { p, n, ghost, dx: e.clientX - rect.left, dy: e.clientY - rect.top, target: null };
  window.addEventListener("pointermove", onDragMove);
  window.addEventListener("pointerup", onDragEnd, { once: true });
}

function dropTargetAt(x, y) {
  const hit = document.elementFromPoint(x, y);
  return hit?.closest(".room-canvas") || hit?.closest(".tray") || null;
}

function onDragMove(e) {
  drag.ghost.style.left = `${e.clientX - drag.dx}px`;
  drag.ghost.style.top = `${e.clientY - drag.dy}px`;
  const t = dropTargetAt(e.clientX, e.clientY);
  if (t !== drag.target) {
    drag.target?.classList.remove("drop-target");
    t?.classList.add("drop-target");
    drag.target = t;
  }
}

function onDragEnd(e) {
  window.removeEventListener("pointermove", onDragMove);
  const { p, n, ghost, target } = drag;
  target?.classList.remove("drop-target");
  const g = ghost.getBoundingClientRect();
  ghost.remove();
  n.classList.remove("dragging");
  drag = null;

  if (target?.classList.contains("room-canvas")) {
    const c = target.getBoundingClientRect();
    const x = Math.round((g.left - c.left + target.scrollLeft - PAD) / SCALE + Number(target.dataset.ox));
    const y = Math.round((g.top - c.top + target.scrollTop - PAD) / SCALE + Number(target.dataset.oy));
    run("place", { node: p.node, room: target.dataset.room, x, y });
  } else if (target?.classList.contains("tray") && p.room) {
    run("place", { node: p.node, room: null, x: 0, y: 0 });
  }
}

// ---------- plumbing ----------

let toastTimer;
function toast(msg, error = false) {
  const t = document.getElementById("toast");
  t.textContent = msg;
  t.classList.toggle("error", error);
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), 3000);
}

async function run(cmd, args) {
  try {
    state = await backend.call(cmd, args);
    render();
  } catch (err) {
    toast(String(err), true);
  }
}

const refresh = () => (drag ? null : run("snapshot"));

document.getElementById("add-room").addEventListener("click", () => {
  const n = state.layout.rooms.length + 1;
  run("add_room", { name: `Room ${n}` });
});

refresh();
