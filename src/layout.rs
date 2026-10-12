//! Pane layout of a tab: a binary tree of splits whose leaves are pane ids.
//! Pure geometry, no I/O. Panes are separated by 1-cell borders.

use serde::{Deserialize, Serialize};

/// Smallest and largest share a split gives its first child.
const MIN_RATIO: f32 = 0.1;
const MAX_RATIO: f32 = 0.9;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    fn axis(self) -> Axis {
        match self {
            Dir::Left | Dir::Right => Axis::Horizontal,
            Dir::Up | Dir::Down => Axis::Vertical,
        }
    }
}

/// `Horizontal`: children side by side (split right). `Vertical`: stacked (split down).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
}

impl Rect {
    pub fn new(x: u16, y: u16, cols: u16, rows: u16) -> Self {
        Rect { x, y, cols, rows }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Leaf(u32),
    /// `ratio` is the share of space (minus the border) given to `a`.
    Split { axis: Axis, ratio: f32, a: Box<Node>, b: Box<Node> },
}

impl Node {
    pub fn leaves(&self) -> Vec<u32> {
        match self {
            Node::Leaf(id) => vec![*id],
            Node::Split { a, b, .. } => [a.leaves(), b.leaves()].concat(),
        }
    }

    pub fn contains(&self, id: u32) -> bool {
        match self {
            Node::Leaf(l) => *l == id,
            Node::Split { a, b, .. } => a.contains(id) || b.contains(id),
        }
    }

    fn first_leaf(&self) -> u32 {
        match self {
            Node::Leaf(id) => *id,
            Node::Split { a, .. } => a.first_leaf(),
        }
    }

    /// Split leaf `target` in two, `new` going right of / below it.
    pub fn split(&mut self, target: u32, new: u32, axis: Axis) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                let old = Node::Leaf(*id);
                *self = Node::Split { axis, ratio: 0.5, a: Box::new(old), b: Box::new(Node::Leaf(new)) };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.split(target, new, axis) || b.split(target, new, axis),
        }
    }

    /// Remove leaf `id`; its sibling takes the parent's place. Returns the pane
    /// that should take focus, or `None` if `id` wasn't found below a split
    /// (removing the root leaf is the caller's job: the tab goes away).
    pub fn remove(&mut self, id: u32) -> Option<u32> {
        let Node::Split { a, b, .. } = self else { return None };
        let sibling = if **a == Node::Leaf(id) {
            std::mem::replace(&mut **b, Node::Leaf(0))
        } else if **b == Node::Leaf(id) {
            std::mem::replace(&mut **a, Node::Leaf(0))
        } else {
            return a.remove(id).or_else(|| b.remove(id));
        };
        *self = sibling;
        Some(self.first_leaf())
    }

    /// Rect of every pane inside `area`.
    pub fn geometry(&self, area: Rect) -> Vec<(u32, Rect)> {
        let mut out = Vec::new();
        self.layout_into(area, &mut out);
        out
    }

    fn layout_into(&self, area: Rect, out: &mut Vec<(u32, Rect)>) {
        match self {
            Node::Leaf(id) => out.push((*id, area)),
            Node::Split { axis, ratio, a, b } => {
                let (ra, rb) = split_rect(area, *axis, *ratio);
                a.layout_into(ra, out);
                b.layout_into(rb, out);
            }
        }
    }

    /// Move the divider of the nearest split around `id` along `dir`'s axis
    /// by `step` (a fraction of that split). Returns false if there is none.
    pub fn resize(&mut self, id: u32, dir: Dir, step: f32) -> bool {
        let Node::Split { axis, ratio, a, b } = self else { return false };
        if !a.contains(id) && !b.contains(id) {
            return false;
        }
        if a.resize(id, dir, step) || b.resize(id, dir, step) {
            return true;
        }
        if *axis != dir.axis() {
            return false;
        }
        let delta = if matches!(dir, Dir::Left | Dir::Up) { -step } else { step };
        *ratio = (*ratio + delta).clamp(MIN_RATIO, MAX_RATIO);
        true
    }
}

/// Split `area` into two rects with a 1-cell border between them.
fn split_rect(area: Rect, axis: Axis, ratio: f32) -> (Rect, Rect) {
    let total = match axis {
        Axis::Horizontal => area.cols,
        Axis::Vertical => area.rows,
    };
    let avail = total.saturating_sub(1);
    let first = if avail < 2 { avail } else { ((avail as f32 * ratio).round() as u16).clamp(1, avail - 1) };
    let second = avail - first;
    match axis {
        Axis::Horizontal => (
            Rect { cols: first, ..area },
            Rect { x: area.x + first + 1, cols: second, ..area },
        ),
        Axis::Vertical => (
            Rect { rows: first, ..area },
            Rect { y: area.y + first + 1, rows: second, ..area },
        ),
    }
}

/// The pane next to `from` in direction `dir`: the closest one that overlaps
/// it on the other axis, preferring the one nearest its top/left edge.
pub fn neighbor(rects: &[(u32, Rect)], from: u32, dir: Dir) -> Option<u32> {
    let cur = rects.iter().find(|(id, _)| *id == from)?.1;
    let overlap = |a0: u16, alen: u16, b0: u16, blen: u16| a0 < b0 + blen && b0 < a0 + alen;
    rects
        .iter()
        .filter(|(id, _)| *id != from)
        .filter_map(|&(id, r)| {
            let (dist, edge) = match dir {
                Dir::Left if r.x + r.cols <= cur.x && overlap(r.y, r.rows, cur.y, cur.rows) => {
                    (cur.x - r.x - r.cols, r.y.abs_diff(cur.y))
                }
                Dir::Right if r.x >= cur.x + cur.cols && overlap(r.y, r.rows, cur.y, cur.rows) => {
                    (r.x - cur.x - cur.cols, r.y.abs_diff(cur.y))
                }
                Dir::Up if r.y + r.rows <= cur.y && overlap(r.x, r.cols, cur.x, cur.cols) => {
                    (cur.y - r.y - r.rows, r.x.abs_diff(cur.x))
                }
                Dir::Down if r.y >= cur.y + cur.rows && overlap(r.x, r.cols, cur.x, cur.cols) => {
                    (r.y - cur.y - cur.rows, r.x.abs_diff(cur.x))
                }
                _ => return None,
            };
            Some((dist, edge, id))
        })
        .min()
        .map(|(_, _, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0, 0, 81, 25)
    }

    #[test]
    fn split_right_leaves_border() {
        let mut t = Node::Leaf(1);
        assert!(t.split(1, 2, Axis::Horizontal));
        assert_eq!(t.geometry(area()), vec![(1, Rect::new(0, 0, 40, 25)), (2, Rect::new(41, 0, 40, 25))]);
    }

    #[test]
    fn split_down_leaves_border() {
        let mut t = Node::Leaf(1);
        t.split(1, 2, Axis::Vertical);
        assert_eq!(t.geometry(area()), vec![(1, Rect::new(0, 0, 81, 12)), (2, Rect::new(0, 13, 81, 12))]);
    }

    #[test]
    fn nested_split_and_missing_target() {
        let mut t = Node::Leaf(1);
        t.split(1, 2, Axis::Horizontal);
        t.split(2, 3, Axis::Vertical);
        assert!(!t.split(9, 4, Axis::Vertical));
        assert_eq!(t.leaves(), vec![1, 2, 3]);
        let g = t.geometry(area());
        assert_eq!(g[1], (2, Rect::new(41, 0, 40, 12)));
        assert_eq!(g[2], (3, Rect::new(41, 13, 40, 12)));
    }

    #[test]
    fn remove_collapses_tree() {
        let mut t = Node::Leaf(1);
        t.split(1, 2, Axis::Horizontal);
        t.split(2, 3, Axis::Vertical);
        assert_eq!(t.remove(2), Some(3));
        assert_eq!(t.leaves(), vec![1, 3]);
        assert_eq!(t.geometry(area())[1], (3, Rect::new(41, 0, 40, 25)));
        assert_eq!(t.remove(1), Some(3));
        assert_eq!(t, Node::Leaf(3));
        // Root leaf is not removed here: the caller closes the tab.
        assert_eq!(t.remove(3), None);
        assert_eq!(t.remove(42), None);
    }

    #[test]
    fn remove_focuses_first_leaf_of_sibling_subtree() {
        let mut t = Node::Leaf(1);
        t.split(1, 2, Axis::Horizontal);
        t.split(2, 3, Axis::Vertical);
        assert_eq!(t.remove(1), Some(2));
        assert_eq!(t.leaves(), vec![2, 3]);
    }

    #[test]
    fn tiny_area_does_not_underflow() {
        let mut t = Node::Leaf(1);
        t.split(1, 2, Axis::Horizontal);
        t.split(2, 3, Axis::Horizontal);
        for cols in 0..4 {
            let g = t.geometry(Rect::new(0, 0, cols, 1));
            assert_eq!(g.len(), 3);
            assert!(g.iter().filter(|(_, r)| r.cols > 0).all(|(_, r)| r.x + r.cols <= cols));
        }
    }

    #[test]
    fn focus_direction() {
        // 1 | 2
        //   | -
        //   | 3
        let mut t = Node::Leaf(1);
        t.split(1, 2, Axis::Horizontal);
        t.split(2, 3, Axis::Vertical);
        let g = t.geometry(area());
        assert_eq!(neighbor(&g, 1, Dir::Right), Some(2));
        assert_eq!(neighbor(&g, 3, Dir::Left), Some(1));
        assert_eq!(neighbor(&g, 2, Dir::Down), Some(3));
        assert_eq!(neighbor(&g, 3, Dir::Up), Some(2));
        assert_eq!(neighbor(&g, 1, Dir::Left), None);
        assert_eq!(neighbor(&g, 1, Dir::Up), None);
        assert_eq!(neighbor(&g, 2, Dir::Right), None);
    }

    #[test]
    fn resize_moves_nearest_matching_divider() {
        let mut t = Node::Leaf(1);
        t.split(1, 2, Axis::Horizontal);
        t.split(2, 3, Axis::Vertical);
        // Pane 3 has a vertical split above it and a horizontal one at the root.
        assert!(t.resize(3, Dir::Left, 0.1));
        assert_eq!(t.geometry(area())[0].1.cols, 32);
        assert!(t.resize(3, Dir::Down, 0.1));
        assert_eq!(t.geometry(area())[1].1.rows, 14);
        for _ in 0..20 {
            t.resize(1, Dir::Left, 0.1);
        }
        assert_eq!(t.geometry(area())[0].1.cols, 8); // clamped at MIN_RATIO
        assert!(!Node::Leaf(1).resize(1, Dir::Left, 0.1));
        let mut v = Node::Leaf(1);
        v.split(1, 2, Axis::Vertical);
        assert!(!v.resize(1, Dir::Right, 0.1)); // no horizontal split
    }
}
