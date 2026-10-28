//! Rooms, node placements and the cursor-crossing calculation.
//!
//! Each room is an independent plane in physical pixels. A node's monitors
//! are placed as one rigid group: `world = origin + (display.rect - bounds.min)`.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{DisplayInfo, NodeId, NodeInfo, Point, Rect, RoomId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Room {
    pub id: RoomId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub node: NodeId,
    /// Last-known name, so offline machines still render sensibly.
    pub name: String,
    /// `None` = discovered but not yet put in a room.
    pub room: Option<RoomId>,
    /// World position of the top-left of the node's display bounding box.
    pub origin: Point,
    /// Locked placements can't be moved or change room.
    pub locked: bool,
    /// Last-known display geometry, in the node's own desktop coordinates.
    pub displays: Vec<DisplayInfo>,
    /// The node's static public key (hex). A placement with a key is a
    /// paired household member; removing the placement revokes it.
    #[serde(default)]
    pub key: String,
}

impl Placement {
    fn bounds(&self) -> Option<Rect> {
        Rect::bounding(self.displays.iter().map(|d| &d.rect))
    }

    /// Display rects in world coordinates if the group's origin were `origin`.
    fn world_rects_at(&self, origin: Point) -> impl Iterator<Item = (u32, Rect)> + '_ {
        let min = self.bounds().map(|b| b.origin()).unwrap_or_default();
        self.displays
            .iter()
            .map(move |d| (d.index, d.rect.translate(origin - min)))
    }

    fn to_world(&self, local: Point) -> Point {
        let min = self.bounds().map(|b| b.origin()).unwrap_or_default();
        self.origin + (local - min)
    }

    fn to_local(&self, world: Point) -> Point {
        let min = self.bounds().map(|b| b.origin()).unwrap_or_default();
        world - self.origin + min
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldMonitor {
    pub node: NodeId,
    pub index: u32,
    pub rect: Rect,
}

/// Result of the cursor leaving one node's desktop for another's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Crossing {
    pub to: NodeId,
    /// Where to put the cursor, in the target node's desktop coordinates.
    pub local: Point,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LayoutError {
    #[error("unknown node {0}")]
    UnknownNode(NodeId),
    #[error("unknown room {0}")]
    UnknownRoom(RoomId),
    #[error("node {0} is locked")]
    Locked(NodeId),
    #[error("room {0} still contains locked nodes")]
    RoomHasLockedNodes(RoomId),
    #[error("node {0} would overlap node {1}")]
    Overlap(NodeId, NodeId),
}

/// How a layout regards a peer presenting `(node, key)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// Paired: the placement's key matches.
    Member,
    /// Never paired (or a placement from before keys existed).
    Unknown,
    /// Claims a member's id with a different key: refuse.
    KeyMismatch,
}

/// The shared, replicated arrangement of rooms and nodes. Synced between
/// peers last-writer-wins on `(revision, author)`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub revision: u64,
    pub author: Option<NodeId>,
    pub rooms: Vec<Room>,
    pub placements: Vec<Placement>,
}

impl Layout {
    /// Record a local edit. Call after every successful mutation.
    pub fn touch(&mut self, author: NodeId) {
        self.revision += 1;
        self.author = Some(author);
    }

    /// Whether `self` should replace `other` during sync.
    pub fn supersedes(&self, other: &Layout) -> bool {
        (self.revision, self.author) > (other.revision, other.author)
    }

    pub fn placement(&self, node: NodeId) -> Option<&Placement> {
        self.placements.iter().find(|p| p.node == node)
    }

    fn placement_mut(&mut self, node: NodeId) -> Result<&mut Placement, LayoutError> {
        self.placements
            .iter_mut()
            .find(|p| p.node == node)
            .ok_or(LayoutError::UnknownNode(node))
    }

    fn room_exists(&self, room: RoomId) -> bool {
        self.rooms.iter().any(|r| r.id == room)
    }

    pub fn trust(&self, node: NodeId, key: &str) -> Trust {
        match self.placement(node) {
            Some(p) if p.key.is_empty() => Trust::Unknown,
            Some(p) if p.key == key => Trust::Member,
            Some(_) => Trust::KeyMismatch,
            None => Trust::Unknown,
        }
    }

    /// Add `info` as an unassigned member, or refresh its cached name,
    /// displays and key. Returns true if anything changed. Called by each node
    /// for itself, and for a peer at the moment both sides accept pairing.
    pub fn sync_node(&mut self, info: &NodeInfo) -> bool {
        match self.placements.iter_mut().find(|p| p.node == info.id) {
            Some(p) => {
                if p.name == info.name && p.displays == info.displays && p.key == info.key {
                    return false;
                }
                p.name = info.name.clone();
                p.displays = info.displays.clone();
                p.key = info.key.clone();
                true
            }
            None => {
                self.placements.push(Placement {
                    node: info.id,
                    name: info.name.clone(),
                    room: None,
                    origin: Point::default(),
                    locked: !info.portable,
                    displays: info.displays.clone(),
                    key: info.key.clone(),
                });
                true
            }
        }
    }

    fn member_count(&self) -> usize {
        self.placements.iter().filter(|p| !p.key.is_empty()).count()
    }

    /// The layout two nodes adopt when they finish pairing. Symmetric: both
    /// sides compute the same result from the same inputs. The side with the
    /// bigger household wins (so a fresh laptop joins the house rather than
    /// wiping it), and both nodes become members.
    pub fn join(a: &Layout, a_node: &NodeInfo, b: &Layout, b_node: &NodeInfo) -> Layout {
        let rank = |l: &Layout, n: &NodeInfo| (l.member_count(), l.revision, n.id);
        let (winner_layout, winner, loser) = if rank(a, a_node) > rank(b, b_node) {
            (a, a_node, b_node)
        } else {
            (b, b_node, a_node)
        };
        let mut joined = winner_layout.clone();
        joined.sync_node(winner);
        joined.sync_node(loser);
        joined.revision = a.revision.max(b.revision) + 1;
        joined.author = Some(winner.id);
        joined
    }

    pub fn add_room(&mut self, name: impl Into<String>) -> RoomId {
        let id = RoomId::new();
        self.rooms.push(Room { id, name: name.into() });
        id
    }

    pub fn rename_room(&mut self, id: RoomId, name: impl Into<String>) -> Result<(), LayoutError> {
        let room = self
            .rooms
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or(LayoutError::UnknownRoom(id))?;
        room.name = name.into();
        Ok(())
    }

    /// Delete a room, moving its nodes to unassigned. Refuses if any of them
    /// are locked, so a fixed desk never silently loses its position.
    pub fn remove_room(&mut self, id: RoomId) -> Result<(), LayoutError> {
        if !self.room_exists(id) {
            return Err(LayoutError::UnknownRoom(id));
        }
        if self.placements.iter().any(|p| p.room == Some(id) && p.locked) {
            return Err(LayoutError::RoomHasLockedNodes(id));
        }
        self.rooms.retain(|r| r.id != id);
        for p in self.placements.iter_mut().filter(|p| p.room == Some(id)) {
            p.room = None;
        }
        Ok(())
    }

    pub fn set_locked(&mut self, node: NodeId, locked: bool) -> Result<(), LayoutError> {
        self.placement_mut(node)?.locked = locked;
        Ok(())
    }

    /// Forget a node entirely (e.g. a machine that's been retired).
    pub fn remove_node(&mut self, node: NodeId) -> Result<(), LayoutError> {
        let before = self.placements.len();
        self.placements.retain(|p| p.node != node);
        if self.placements.len() == before {
            return Err(LayoutError::UnknownNode(node));
        }
        Ok(())
    }

    /// Move a node to `origin` in `room` (or unassign it with `None`).
    pub fn place(&mut self, node: NodeId, room: Option<RoomId>, origin: Point) -> Result<(), LayoutError> {
        let p = self.placement(node).ok_or(LayoutError::UnknownNode(node))?;
        if p.room == room && (room.is_none() || p.origin == origin) {
            return Ok(());
        }
        if p.locked {
            return Err(LayoutError::Locked(node));
        }
        if let Some(room) = room {
            if !self.room_exists(room) {
                return Err(LayoutError::UnknownRoom(room));
            }
            if let Some(other) = self.first_overlap(node, room, origin) {
                return Err(LayoutError::Overlap(node, other));
            }
        }
        let p = self.placement_mut(node)?;
        p.room = room;
        p.origin = origin;
        Ok(())
    }

    fn first_overlap(&self, node: NodeId, room: RoomId, origin: Point) -> Option<NodeId> {
        let me = self.placement(node)?;
        let mine: Vec<Rect> = me.world_rects_at(origin).map(|(_, r)| r).collect();
        self.world_monitors(room)
            .into_iter()
            .filter(|m| m.node != node)
            .find(|m| mine.iter().any(|r| r.intersects(&m.rect)))
            .map(|m| m.node)
    }

    /// All monitors in `room`, in world coordinates.
    pub fn world_monitors(&self, room: RoomId) -> Vec<WorldMonitor> {
        self.placements
            .iter()
            .filter(|p| p.room == Some(room))
            .flat_map(|p| {
                p.world_rects_at(p.origin)
                    .map(move |(index, rect)| WorldMonitor { node: p.node, index, rect })
            })
            .collect()
    }

    /// Adjust a proposed `origin` so the node's edges line up with nearby
    /// monitor edges of other nodes in `room`, within `threshold` pixels.
    /// Each axis snaps independently.
    pub fn snap(&self, node: NodeId, room: RoomId, origin: Point, threshold: i32) -> Point {
        let Some(me) = self.placement(node) else { return origin };
        let mine: Vec<Rect> = me.world_rects_at(origin).map(|(_, r)| r).collect();
        let others: Vec<Rect> = self
            .world_monitors(room)
            .into_iter()
            .filter(|m| m.node != node)
            .map(|m| m.rect)
            .collect();

        let best = |pairs: &mut dyn Iterator<Item = i32>| -> i32 {
            pairs
                .filter(|d| d.abs() <= threshold)
                .min_by_key(|d| d.abs())
                .unwrap_or(0)
        };
        let dx = best(&mut mine.iter().flat_map(|a| {
            others.iter().flat_map(move |b| {
                [b.right() - a.x, b.x - a.right(), b.x - a.x, b.right() - a.right()]
            })
        }));
        let dy = best(&mut mine.iter().flat_map(|a| {
            others.iter().flat_map(move |b| {
                [b.bottom() - a.y, b.y - a.bottom(), b.y - a.y, b.bottom() - a.bottom()]
            })
        }));
        origin + Point::new(dx, dy)
    }

    /// The cursor on node `from` is at `local` (its own desktop coordinates)
    /// and is being pushed by `delta` past the edge of its desktop. If that
    /// lands on another node's monitor in the same room, return where.
    pub fn cross(&self, from: NodeId, local: Point, delta: Point) -> Option<Crossing> {
        let src = self.placement(from)?;
        let room = src.room?;
        let target = src.to_world(local) + delta;

        // Still on our own desktop: the OS handles it.
        if src.world_rects_at(src.origin).any(|(_, r)| r.contains(target)) {
            return None;
        }
        let hit = self
            .world_monitors(room)
            .into_iter()
            .find(|m| m.node != from && m.rect.contains(target))?;
        let dst = self.placement(hit.node)?;
        Some(Crossing { to: hit.node, local: dst.to_local(target) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Os;

    fn node(name: &str, portable: bool, rects: &[Rect]) -> NodeInfo {
        NodeInfo {
            id: NodeId::new(),
            name: name.into(),
            os: Os::Windows,
            portable,
            key: format!("key-{name}"),
            displays: rects
                .iter()
                .enumerate()
                .map(|(i, r)| DisplayInfo { index: i as u32, id: format!("MON{i}"), name: format!("D{i}"), rect: *r, primary: i == 0 })
                .collect(),
        }
    }

    /// Desktop with two 1920x1080 monitors at world (0,0); laptop unplaced.
    fn office() -> (Layout, RoomId, NodeInfo, NodeInfo) {
        let mut l = Layout::default();
        let room = l.add_room("Office");
        // OS coordinates deliberately don't start at 0,0.
        let desk = node("desk", false, &[Rect::new(-1920, 0, 1920, 1080), Rect::new(0, 0, 1920, 1080)]);
        let laptop = node("laptop", true, &[Rect::new(0, 0, 1440, 900)]);
        l.sync_node(&desk);
        l.sync_node(&laptop);
        l.set_locked(desk.id, false).unwrap();
        l.place(desk.id, Some(room), Point::new(0, 0)).unwrap();
        l.set_locked(desk.id, true).unwrap();
        (l, room, desk, laptop)
    }

    #[test]
    fn new_nodes_lock_unless_portable() {
        let (l, _, desk, laptop) = office();
        assert!(l.placement(desk.id).unwrap().locked);
        let lp = l.placement(laptop.id).unwrap();
        assert!(!lp.locked);
        assert_eq!(lp.room, None);
    }

    #[test]
    fn sync_node_reports_changes_only() {
        let (mut l, _, mut desk, _) = office();
        assert!(!l.sync_node(&desk));
        desk.displays.pop();
        assert!(l.sync_node(&desk));
        // Placement survives a display change.
        assert!(l.placement(desk.id).unwrap().locked);
    }

    #[test]
    fn trust_follows_placement_keys() {
        let (mut l, _, desk, laptop) = office();
        assert_eq!(l.trust(desk.id, "key-desk"), Trust::Member);
        assert_eq!(l.trust(desk.id, "key-evil"), Trust::KeyMismatch);
        assert_eq!(l.trust(NodeId::new(), "key-desk"), Trust::Unknown);
        // Forgetting a machine revokes it.
        l.remove_node(laptop.id).unwrap();
        assert_eq!(l.trust(laptop.id, "key-laptop"), Trust::Unknown);
        // Layouts saved before keys existed load with empty keys: not members.
        l.placements[0].key.clear();
        assert_eq!(l.trust(desk.id, ""), Trust::Unknown);
    }

    #[test]
    fn join_keeps_the_bigger_household_and_is_symmetric() {
        let (house, room, desk, laptop) = office();
        // A brand-new machine with its own one-node layout and a higher revision.
        let newbie = node("newbie", true, &[Rect::new(0, 0, 1280, 800)]);
        let mut fresh = Layout::default();
        fresh.sync_node(&newbie);
        for _ in 0..100 {
            fresh.touch(newbie.id);
        }

        let j1 = Layout::join(&house, &desk, &fresh, &newbie);
        let j2 = Layout::join(&fresh, &newbie, &house, &desk);
        assert_eq!(j1, j2);
        assert_eq!(j1.rooms, house.rooms);
        assert_eq!(j1.placement(desk.id).unwrap().room, Some(room));
        for n in [&desk, &laptop, &newbie] {
            assert_eq!(j1.trust(n.id, &n.key), Trust::Member);
        }
        assert!(j1.supersedes(&house) && j1.supersedes(&fresh));
    }

    #[test]
    fn locked_node_cannot_move() {
        let (mut l, room, desk, _) = office();
        assert_eq!(l.place(desk.id, Some(room), Point::new(10, 0)), Err(LayoutError::Locked(desk.id)));
        assert_eq!(l.place(desk.id, None, Point::default()), Err(LayoutError::Locked(desk.id)));
        // Re-placing at the same spot is a no-op, not an error.
        assert_eq!(l.place(desk.id, Some(room), Point::new(0, 0)), Ok(()));
    }

    #[test]
    fn overlap_is_rejected() {
        let (mut l, room, desk, laptop) = office();
        assert_eq!(
            l.place(laptop.id, Some(room), Point::new(3000, 500)),
            Err(LayoutError::Overlap(laptop.id, desk.id))
        );
        assert_eq!(l.place(laptop.id, Some(room), Point::new(3840, 500)), Ok(()));
    }

    #[test]
    fn world_monitors_normalise_os_coordinates() {
        let (l, room, desk, _) = office();
        let ms = l.world_monitors(room);
        assert_eq!(ms.len(), 2);
        assert!(ms.iter().all(|m| m.node == desk.id));
        assert_eq!(ms[0].rect, Rect::new(0, 0, 1920, 1080));
        assert_eq!(ms[1].rect, Rect::new(1920, 0, 1920, 1080));
    }

    #[test]
    fn snap_to_neighbour_edges() {
        let (l, room, _, laptop) = office();
        // Dropped slightly off the desk's right edge and top: snaps flush.
        assert_eq!(l.snap(laptop.id, room, Point::new(3852, 7), 20), Point::new(3840, 0));
        // Too far away: unchanged.
        assert_eq!(l.snap(laptop.id, room, Point::new(3900, 300), 20), Point::new(3900, 300));
    }

    #[test]
    fn cursor_crosses_to_neighbour_and_back() {
        let (mut l, room, desk, laptop) = office();
        l.place(laptop.id, Some(room), Point::new(3840, 100)).unwrap();

        // Right edge of the desk's right monitor (OS x = 1919) at y = 500.
        let c = l.cross(desk.id, Point::new(1919, 500), Point::new(3, 0)).unwrap();
        assert_eq!(c, Crossing { to: laptop.id, local: Point::new(2, 400) });

        // And back from the laptop's left edge.
        let back = l.cross(laptop.id, Point::new(0, 400), Point::new(-1, 0)).unwrap();
        assert_eq!(back, Crossing { to: desk.id, local: Point::new(1919, 500) });
    }

    #[test]
    fn no_crossing_into_gaps_own_desktop_or_other_rooms() {
        let (mut l, room, desk, laptop) = office();
        l.place(laptop.id, Some(room), Point::new(3840, 100)).unwrap();
        // Laptop only spans y 100..1000; y = 50 at the desk edge hits nothing.
        assert_eq!(l.cross(desk.id, Point::new(1919, 50), Point::new(3, 0)), None);
        // Moving between the desk's own monitors isn't a crossing.
        assert_eq!(l.cross(desk.id, Point::new(-1, 500), Point::new(3, 0)), None);

        let lounge = l.add_room("Lounge");
        l.place(laptop.id, Some(lounge), Point::new(3840, 100)).unwrap();
        assert_eq!(l.cross(desk.id, Point::new(1919, 500), Point::new(3, 0)), None);
    }

    #[test]
    fn remove_room_respects_locks() {
        let (mut l, room, desk, laptop) = office();
        l.place(laptop.id, Some(room), Point::new(3840, 0)).unwrap();
        assert_eq!(l.remove_room(room), Err(LayoutError::RoomHasLockedNodes(room)));
        l.set_locked(desk.id, false).unwrap();
        l.remove_room(room).unwrap();
        assert!(l.rooms.is_empty());
        assert!(l.placements.iter().all(|p| p.room.is_none()));
    }

    #[test]
    fn last_writer_wins() {
        let a = NodeId::new();
        let b = NodeId::new();
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        let mut x = Layout::default();
        let mut y = Layout::default();
        x.touch(lo);
        y.touch(hi);
        assert!(y.supersedes(&x));
        x.touch(lo);
        assert!(x.supersedes(&y));
        assert!(!x.supersedes(&x.clone()));
    }

    #[test]
    fn round_trips_through_json() {
        let (l, ..) = office();
        let s = serde_json::to_string(&l).unwrap();
        assert_eq!(serde_json::from_str::<Layout>(&s).unwrap(), l);
    }
}
