//! The input state machine. Platform- and network-free: callers feed it local
//! physical events and peer messages, and carry out the [`Action`]s it returns.
//!
//! - **Idle**: the cursor is here and local input drives it. Reaching an edge
//!   that has a neighbour in the layout starts forwarding there.
//! - **Forwarding**: we own the physical input and it's grabbed (swallowed
//!   locally); events go to the target, which tracks the cursor and sends
//!   `Handoff` when it leaves its screens.
//! - **Controlled**: a peer's input drives our cursor. Touching our own mouse
//!   or keyboard takes it back (`Claim`).

use std::collections::BTreeSet;

use crate::protocol::{InputEvent, Message, MouseButton};
use crate::{Layout, NodeId, Point};

/// Physical input observed on this machine (injected input is never reported).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LocalEvent {
    /// Pointer motion. `pos` is where the OS put the cursor (meaningless while
    /// grabbed); `delta` is the motion since the previous event.
    Motion { pos: Point, delta: Point },
    Input(InputEvent),
    /// The emergency hotkey (Ctrl+Alt+Shift+Esc) was pressed while grabbed.
    /// The capture layer has already released the grab.
    Release,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Send(NodeId, Message),
    /// Start or stop swallowing local physical input.
    Grab(bool),
    /// Move the local cursor, in local desktop coordinates.
    Warp(Point),
    /// Replay a button, wheel or key event locally.
    Inject(InputEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Idle,
    Forwarding { to: NodeId },
    Controlled { by: NodeId, pos: Point },
}

/// Keys and buttons currently held down on the forwarding target, so they can
/// be released when the cursor moves on (no stuck Shift on the old machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Held {
    Key(u16),
    Button(MouseButton),
}

pub struct Engine {
    me: NodeId,
    mode: Mode,
    held: BTreeSet<Held>,
}

const DIRECTIONS: [Point; 4] = [Point::new(1, 0), Point::new(-1, 0), Point::new(0, 1), Point::new(0, -1)];

impl Engine {
    pub fn new(me: NodeId) -> Self {
        Self { me, mode: Mode::Idle, held: BTreeSet::new() }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn local(&mut self, ev: LocalEvent, layout: &Layout) -> Vec<Action> {
        match (self.mode, ev) {
            (Mode::Idle, LocalEvent::Motion { pos, delta }) => {
                let Some(c) = self.edge_crossing(layout, pos, delta) else { return vec![] };
                self.mode = Mode::Forwarding { to: c.to };
                vec![
                    Action::Grab(true),
                    Action::Send(c.to, Message::Enter { target: c.to, x: c.local.x, y: c.local.y }),
                ]
            }
            (Mode::Idle, _) => vec![],

            (Mode::Forwarding { to }, LocalEvent::Motion { delta, .. }) => {
                if delta == Point::default() {
                    return vec![];
                }
                vec![Action::Send(to, Message::Input(InputEvent::MouseMove { dx: delta.x, dy: delta.y }))]
            }
            (Mode::Forwarding { to }, LocalEvent::Input(ev)) => {
                self.track(&ev);
                vec![Action::Send(to, Message::Input(ev))]
            }
            (Mode::Forwarding { to }, LocalEvent::Release) => {
                let mut out = self.release_held(to);
                self.mode = Mode::Idle;
                // The capture layer already ungrabbed; saying so keeps state consistent.
                out.push(Action::Grab(false));
                out.push(Action::Send(to, Message::Claim));
                out
            }

            (Mode::Controlled { by, .. }, ev) => {
                let physical = match ev {
                    LocalEvent::Motion { delta, .. } => delta != Point::default(),
                    LocalEvent::Input(_) => true,
                    LocalEvent::Release => false,
                };
                if !physical {
                    return vec![];
                }
                self.mode = Mode::Idle;
                vec![Action::Send(by, Message::Claim)]
            }
        }
    }

    pub fn remote(&mut self, from: NodeId, msg: &Message, layout: &Layout) -> Vec<Action> {
        match (self.mode, msg) {
            (_, Message::Enter { target, x, y }) if *target == self.me => {
                let mut out = vec![];
                if let Mode::Forwarding { to } = self.mode {
                    // Someone else took over while we were driving a peer.
                    out = self.release_held(to);
                    out.push(Action::Grab(false));
                }
                let pos = Point::new(*x, *y);
                self.mode = Mode::Controlled { by: from, pos };
                out.push(Action::Warp(pos));
                out
            }

            (Mode::Controlled { by, pos }, Message::Input(ev)) if by == from => match *ev {
                InputEvent::MouseMove { dx, dy } => self.move_controlled(by, pos, Point::new(dx, dy), layout),
                other => vec![Action::Inject(other)],
            },

            (Mode::Forwarding { to }, Message::Handoff { to: next, x, y }) if to == from => {
                let mut out = self.release_held(to);
                let at = Point::new(*x, *y);
                if *next == self.me {
                    self.mode = Mode::Idle;
                    out.push(Action::Grab(false));
                    out.push(Action::Warp(at));
                } else {
                    self.mode = Mode::Forwarding { to: *next };
                    out.push(Action::Send(*next, Message::Enter { target: *next, x: at.x, y: at.y }));
                }
                out
            }

            (Mode::Forwarding { to }, Message::Claim) if to == from => {
                self.held.clear(); // They're using their own keyboard now.
                self.mode = Mode::Idle;
                vec![Action::Grab(false)]
            }
            (Mode::Controlled { by, .. }, Message::Claim) if by == from => {
                self.mode = Mode::Idle;
                vec![]
            }
            _ => vec![],
        }
    }

    /// A peer disconnected (or its send queue failed).
    pub fn peer_lost(&mut self, peer: NodeId) -> Vec<Action> {
        match self.mode {
            Mode::Forwarding { to } if to == peer => {
                self.held.clear();
                self.mode = Mode::Idle;
                vec![Action::Grab(false)]
            }
            Mode::Controlled { by, .. } if by == peer => {
                self.mode = Mode::Idle;
                vec![]
            }
            _ => vec![],
        }
    }

    /// The cursor sits at `pos` and moved by `delta`. Crossing happens when it
    /// is on (or pushed past) an outer edge with a neighbour beyond. A cursor
    /// pinned against an edge reports zero delta, so also try each direction
    /// the motion isn't moving away from.
    fn edge_crossing(&self, layout: &Layout, pos: Point, delta: Point) -> Option<crate::Crossing> {
        if delta != Point::default() {
            if let Some(c) = layout.cross(self.me, pos, delta) {
                return Some(c);
            }
        }
        DIRECTIONS
            .iter()
            .filter(|d| d.x * delta.x >= 0 && d.y * delta.y >= 0)
            .find_map(|d| layout.cross(self.me, pos, *d))
    }

    fn move_controlled(&mut self, by: NodeId, pos: Point, delta: Point, layout: &Layout) -> Vec<Action> {
        let new = pos + delta;
        let Some(me) = layout.placement(self.me) else { return vec![] };
        if me.displays.iter().any(|d| d.rect.contains(new)) {
            self.mode = Mode::Controlled { by, pos: new };
            return vec![Action::Warp(new)];
        }
        if let Some(c) = layout.cross(self.me, pos, delta) {
            self.mode = Mode::Idle;
            return vec![Action::Send(by, Message::Handoff { to: c.to, x: c.local.x, y: c.local.y })];
        }
        // Nothing there: stop at the edge of the display we're on.
        let Some(d) = me.displays.iter().find(|d| d.rect.contains(pos)) else { return vec![] };
        let clamped = Point::new(
            new.x.clamp(d.rect.x, d.rect.right() - 1),
            new.y.clamp(d.rect.y, d.rect.bottom() - 1),
        );
        if clamped == pos {
            return vec![];
        }
        self.mode = Mode::Controlled { by, pos: clamped };
        vec![Action::Warp(clamped)]
    }

    fn track(&mut self, ev: &InputEvent) {
        let (held, down) = match *ev {
            InputEvent::Key { usage, down } => (Held::Key(usage), down),
            InputEvent::Button { button, down } => (Held::Button(button), down),
            _ => return,
        };
        if down {
            self.held.insert(held);
        } else {
            self.held.remove(&held);
        }
    }

    fn release_held(&mut self, to: NodeId) -> Vec<Action> {
        std::mem::take(&mut self.held)
            .into_iter()
            .map(|h| {
                let ev = match h {
                    Held::Key(usage) => InputEvent::Key { usage, down: false },
                    Held::Button(button) => InputEvent::Button { button, down: false },
                };
                Action::Send(to, Message::Input(ev))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DisplayInfo, NodeInfo, Os, Rect};

    fn node(name: &str, w: i32, h: i32) -> NodeInfo {
        NodeInfo {
            id: NodeId::new(),
            name: name.into(),
            os: Os::Windows,
            portable: true,
            key: format!("k-{name}"),
            displays: vec![DisplayInfo { index: 0, id: String::new(), name: "D".into(), rect: Rect::new(0, 0, w, h), primary: true }],
        }
    }

    /// desk (1920x1080) | laptop (1440x900, top-aligned) | tv (1920x1080)
    fn setup() -> (Layout, NodeInfo, NodeInfo, NodeInfo) {
        let mut l = Layout::default();
        let room = l.add_room("Office");
        let (desk, laptop, tv) = (node("desk", 1920, 1080), node("laptop", 1440, 900), node("tv", 1920, 1080));
        for (n, x) in [(&desk, 0), (&laptop, 1920), (&tv, 3360)] {
            l.sync_node(n);
            l.place(n.id, Some(room), Point::new(x, 0)).unwrap();
        }
        (l, desk, laptop, tv)
    }

    fn motion(x: i32, y: i32, dx: i32, dy: i32) -> LocalEvent {
        LocalEvent::Motion { pos: Point::new(x, y), delta: Point::new(dx, dy) }
    }

    fn key(usage: u16, down: bool) -> LocalEvent {
        LocalEvent::Input(InputEvent::Key { usage, down })
    }

    #[test]
    fn idle_cursor_crosses_at_edge_even_when_pinned() {
        let (l, desk, laptop, _) = setup();
        let mut e = Engine::new(desk.id);
        assert_eq!(e.local(motion(1000, 500, 5, 0), &l), vec![]);
        // Pinned against the right edge: the OS reports zero delta.
        let out = e.local(motion(1919, 500, 0, 0), &l);
        assert_eq!(
            out,
            vec![Action::Grab(true), Action::Send(laptop.id, Message::Enter { target: laptop.id, x: 0, y: 500 })]
        );
        assert_eq!(e.mode(), Mode::Forwarding { to: laptop.id });
    }

    #[test]
    fn moving_away_from_an_edge_does_not_cross() {
        let (l, desk, _, _) = setup();
        let mut e = Engine::new(desk.id);
        // On the right edge but heading left.
        assert_eq!(e.local(motion(1919, 500, -4, 0), &l), vec![]);
        // Left edge has no neighbour.
        assert_eq!(e.local(motion(0, 500, -4, 0), &l), vec![]);
    }

    #[test]
    fn controlled_target_moves_then_hands_off_onward_and_back() {
        let (l, desk, laptop, tv) = setup();
        let mut src = Engine::new(desk.id);
        let mut dst = Engine::new(laptop.id);

        src.local(motion(1919, 500, 0, 0), &l);
        let enter = Message::Enter { target: laptop.id, x: 0, y: 500 };
        assert_eq!(dst.remote(desk.id, &enter, &l), vec![Action::Warp(Point::new(0, 500))]);

        let mv = |dx, dy| Message::Input(InputEvent::MouseMove { dx, dy });
        assert_eq!(dst.remote(desk.id, &mv(100, -10), &l), vec![Action::Warp(Point::new(100, 490))]);
        // Below the laptop (y >= 900) is empty: clamp to its bottom edge.
        assert_eq!(dst.remote(desk.id, &mv(0, 1000), &l), vec![Action::Warp(Point::new(100, 899))]);

        // Off its right edge onto the tv: tell the source.
        dst.remote(desk.id, &mv(0, -399), &l);
        let out = dst.remote(desk.id, &mv(1345, 0), &l);
        let handoff = Message::Handoff { to: tv.id, x: 5, y: 500 };
        assert_eq!(out, vec![Action::Send(desk.id, handoff.clone())]);
        assert_eq!(dst.mode(), Mode::Idle);

        // The source retargets the tv.
        assert_eq!(
            src.remote(laptop.id, &handoff, &l),
            vec![Action::Send(tv.id, Message::Enter { target: tv.id, x: 5, y: 500 })]
        );
        assert_eq!(src.mode(), Mode::Forwarding { to: tv.id });

        // Later a handoff back to the source itself releases the grab.
        let back = Message::Handoff { to: desk.id, x: 1915, y: 20 };
        assert_eq!(
            src.remote(tv.id, &back, &l),
            vec![Action::Grab(false), Action::Warp(Point::new(1915, 20))]
        );
        assert_eq!(src.mode(), Mode::Idle);
    }

    #[test]
    fn held_keys_are_released_on_the_old_target() {
        let (l, desk, laptop, tv) = setup();
        let mut e = Engine::new(desk.id);
        e.local(motion(1919, 500, 0, 0), &l);
        e.local(key(0xE1, true), &l); // Left Shift down
        e.local(key(0x04, true), &l);
        e.local(key(0x04, false), &l);
        let out = e.remote(laptop.id, &Message::Handoff { to: tv.id, x: 0, y: 0 }, &l);
        assert_eq!(out[0], Action::Send(laptop.id, Message::Input(InputEvent::Key { usage: 0xE1, down: false })));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn touching_a_controlled_machine_claims_it_back() {
        let (l, desk, laptop, _) = setup();
        let mut src = Engine::new(desk.id);
        let mut dst = Engine::new(laptop.id);
        src.local(motion(1919, 500, 0, 0), &l);
        dst.remote(desk.id, &Message::Enter { target: laptop.id, x: 0, y: 500 }, &l);

        // Zero-delta motion (e.g. a warp echo) isn't a human.
        assert_eq!(dst.local(motion(0, 500, 0, 0), &l), vec![]);
        assert_eq!(dst.local(motion(3, 500, 3, 0), &l), vec![Action::Send(desk.id, Message::Claim)]);
        assert_eq!(src.remote(laptop.id, &Message::Claim, &l), vec![Action::Grab(false)]);
        assert_eq!(src.mode(), Mode::Idle);
    }

    #[test]
    fn hotkey_and_lost_peer_release_the_grab() {
        let (l, desk, laptop, _) = setup();
        let mut e = Engine::new(desk.id);
        e.local(motion(1919, 500, 0, 0), &l);
        e.local(key(0xE0, true), &l);
        let out = e.local(LocalEvent::Release, &l);
        assert!(out.contains(&Action::Grab(false)));
        assert!(out.contains(&Action::Send(laptop.id, Message::Input(InputEvent::Key { usage: 0xE0, down: false }))));
        assert_eq!(e.mode(), Mode::Idle);

        e.local(motion(1919, 500, 0, 0), &l);
        assert_eq!(e.peer_lost(laptop.id), vec![Action::Grab(false)]);
        assert_eq!(e.mode(), Mode::Idle);
    }

    #[test]
    fn input_from_the_wrong_peer_is_ignored() {
        let (l, desk, laptop, tv) = setup();
        let mut dst = Engine::new(laptop.id);
        dst.remote(desk.id, &Message::Enter { target: laptop.id, x: 0, y: 0 }, &l);
        let click = Message::Input(InputEvent::Button { button: MouseButton::Left, down: true });
        assert_eq!(dst.remote(tv.id, &click, &l), vec![]);
        assert_eq!(dst.remote(desk.id, &click, &l), vec![Action::Inject(InputEvent::Button { button: MouseButton::Left, down: true })]);
    }
}
