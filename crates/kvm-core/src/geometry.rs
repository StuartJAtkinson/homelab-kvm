use std::ops::{Add, Sub};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

impl Add for Point {
    type Output = Point;
    fn add(self, o: Point) -> Point {
        Point::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Point {
    type Output = Point;
    fn sub(self, o: Point) -> Point {
        Point::new(self.x - o.x, self.y - o.y)
    }
}

/// Axis-aligned rectangle; `x`/`y` is the top-left corner, right/bottom edges
/// are exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn origin(&self) -> Point {
        Point::new(self.x, self.y)
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }

    pub fn translate(&self, d: Point) -> Rect {
        Rect::new(self.x + d.x, self.y + d.y, self.w, self.h)
    }

    /// Smallest rect containing all of `rects`, or `None` if empty.
    pub fn bounding<'a>(rects: impl IntoIterator<Item = &'a Rect>) -> Option<Rect> {
        let mut it = rects.into_iter();
        let first = *it.next()?;
        let (mut l, mut t, mut r, mut b) = (first.x, first.y, first.right(), first.bottom());
        for rc in it {
            l = l.min(rc.x);
            t = t.min(rc.y);
            r = r.max(rc.right());
            b = b.max(rc.bottom());
        }
        Some(Rect::new(l, t, r - l, b - t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_are_exclusive() {
        let r = Rect::new(0, 0, 10, 10);
        assert!(r.contains(Point::new(9, 9)));
        assert!(!r.contains(Point::new(10, 5)));
        // Touching rects do not intersect.
        assert!(!r.intersects(&Rect::new(10, 0, 10, 10)));
        assert!(r.intersects(&Rect::new(9, 9, 10, 10)));
    }

    #[test]
    fn bounding_box() {
        let rs = [Rect::new(-1920, 0, 1920, 1080), Rect::new(0, -200, 2560, 1440)];
        assert_eq!(Rect::bounding(&rs), Some(Rect::new(-1920, -200, 4480, 1440)));
        assert_eq!(Rect::bounding(&[]), None);
    }
}
