//! Binary split tree for the panes of one window.

pub type PaneId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub fn right(&self) -> u16 {
        self.x + self.w
    }
    pub fn bottom(&self) -> u16 {
        self.y + self.h
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone)]
pub enum Node {
    Leaf(PaneId),
    /// `horizontal`: children side by side (tmux `split-window -h`), else stacked.
    Split {
        horizontal: bool,
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

/// A separator line: vertical when `horizontal` split, drawn at (x, y) with length `len`.
#[derive(Clone, Copy, Debug)]
pub struct Sep {
    pub vertical: bool,
    pub x: u16,
    pub y: u16,
    pub len: u16,
}

fn split_len(total: u16, ratio: f32) -> (u16, u16) {
    // One cell goes to the separator.
    if total < 3 {
        return (total.max(1), 0);
    }
    let avail = total - 1;
    let a = ((avail as f32 * ratio).round() as u16).clamp(1, avail - 1);
    (a, avail - a)
}

impl Node {
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(p) => out.push(*p),
            Node::Split { a, b, .. } => {
                a.collect(out);
                b.collect(out);
            }
        }
    }

    pub fn layout(&self, r: Rect, panes: &mut Vec<(PaneId, Rect)>, seps: &mut Vec<Sep>) {
        match self {
            Node::Leaf(p) => panes.push((*p, r)),
            Node::Split { horizontal: true, ratio, a, b } => {
                let (wa, wb) = split_len(r.w, *ratio);
                a.layout(Rect { w: wa, ..r }, panes, seps);
                if wb > 0 {
                    seps.push(Sep { vertical: true, x: r.x + wa, y: r.y, len: r.h });
                    b.layout(Rect { x: r.x + wa + 1, w: wb, ..r }, panes, seps);
                } else {
                    b.layout(Rect { w: 0, ..r }, panes, seps);
                }
            }
            Node::Split { horizontal: false, ratio, a, b } => {
                let (ha, hb) = split_len(r.h, *ratio);
                a.layout(Rect { h: ha, ..r }, panes, seps);
                if hb > 0 {
                    seps.push(Sep { vertical: false, x: r.x, y: r.y + ha, len: r.w });
                    b.layout(Rect { y: r.y + ha + 1, h: hb, ..r }, panes, seps);
                } else {
                    b.layout(Rect { h: 0, ..r }, panes, seps);
                }
            }
        }
    }

    /// Replace leaf `target` with a split of `target` and `new`.
    pub fn split(&mut self, target: PaneId, new: PaneId, horizontal: bool) -> bool {
        match self {
            Node::Leaf(p) if *p == target => {
                *self = Node::Split {
                    horizontal,
                    ratio: 0.5,
                    a: Box::new(Node::Leaf(target)),
                    b: Box::new(Node::Leaf(new)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.split(target, new, horizontal) || b.split(target, new, horizontal),
        }
    }

    /// Remove a leaf, collapsing its parent split. Returns None when the tree becomes empty.
    pub fn remove(self, target: PaneId) -> Option<Node> {
        match self {
            Node::Leaf(p) if p == target => None,
            leaf @ Node::Leaf(_) => Some(leaf),
            Node::Split { horizontal, ratio, a, b } => match (a.remove(target), b.remove(target)) {
                (Some(a), Some(b)) => Some(Node::Split { horizontal, ratio, a: Box::new(a), b: Box::new(b) }),
                (Some(n), None) | (None, Some(n)) => Some(n),
                (None, None) => None,
            },
        }
    }

    /// Grow/shrink the pane by moving the nearest enclosing separator of the
    /// matching orientation `cells` cells toward `dir`.
    pub fn resize(&mut self, target: PaneId, dir: Dir, cells: u16, r: Rect) -> bool {
        self.resize_inner(target, dir, cells, r) == Some(true)
    }

    // Some(true) done, Some(false) target found below but not yet adjusted, None not found.
    fn resize_inner(&mut self, target: PaneId, dir: Dir, cells: u16, r: Rect) -> Option<bool> {
        match self {
            Node::Leaf(p) => (*p == target).then_some(false),
            Node::Split { horizontal, ratio, a, b } => {
                let total = if *horizontal { r.w } else { r.h };
                let (la, _) = split_len(total, *ratio);
                let (ra, rb) = if *horizontal {
                    (Rect { w: la, ..r }, Rect { x: r.x + la + 1, w: total.saturating_sub(la + 1), ..r })
                } else {
                    (Rect { h: la, ..r }, Rect { y: r.y + la + 1, h: total.saturating_sub(la + 1), ..r })
                };
                let found = match a.resize_inner(target, dir, cells, ra) {
                    Some(done) => Some(done),
                    None => b.resize_inner(target, dir, cells, rb),
                };
                match found {
                    Some(false) if *horizontal == matches!(dir, Dir::Left | Dir::Right) && total >= 3 => {
                        let avail = (total - 1) as f32;
                        let delta = cells as f32 / avail;
                        let new = match dir {
                            Dir::Right | Dir::Down => *ratio + delta,
                            Dir::Left | Dir::Up => *ratio - delta,
                        };
                        *ratio = new.clamp(1.0 / avail, 1.0 - 1.0 / avail);
                        Some(true)
                    }
                    other => other,
                }
            }
        }
    }
}

/// Pick the neighbour of `from` in `dir`, preferring the one overlapping `cursor`.
pub fn neighbour(panes: &[(PaneId, Rect)], from: PaneId, dir: Dir, cursor: (u16, u16)) -> Option<PaneId> {
    let cur = panes.iter().find(|(p, _)| *p == from)?.1;
    let overlaps = |lo1: u16, hi1: u16, lo2: u16, hi2: u16| lo1 < hi2 && lo2 < hi1;
    let mut best: Option<(PaneId, bool)> = None;
    for (id, r) in panes {
        if *id == from || r.w == 0 || r.h == 0 {
            continue;
        }
        let (adjacent, hit) = match dir {
            Dir::Left => (r.right() + 1 == cur.x && overlaps(r.y, r.bottom(), cur.y, cur.bottom()), r.y <= cursor.1 && cursor.1 < r.bottom()),
            Dir::Right => (cur.right() + 1 == r.x && overlaps(r.y, r.bottom(), cur.y, cur.bottom()), r.y <= cursor.1 && cursor.1 < r.bottom()),
            Dir::Up => (r.bottom() + 1 == cur.y && overlaps(r.x, r.right(), cur.x, cur.right()), r.x <= cursor.0 && cursor.0 < r.right()),
            Dir::Down => (cur.bottom() + 1 == r.y && overlaps(r.x, r.right(), cur.x, cur.right()), r.x <= cursor.0 && cursor.0 < r.right()),
        };
        if adjacent && best.is_none_or(|(_, h)| !h && hit) {
            best = Some((*id, hit));
        }
    }
    best.map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> Rect {
        Rect { x: 0, y: 0, w: 81, h: 24 }
    }

    #[test]
    fn split_and_layout() {
        let mut n = Node::Leaf(1);
        assert!(n.split(1, 2, true));
        assert!(n.split(2, 3, false));
        let (mut p, mut s) = (vec![], vec![]);
        n.layout(full(), &mut p, &mut s);
        assert_eq!(p[0], (1, Rect { x: 0, y: 0, w: 40, h: 24 }));
        assert_eq!(p[1].1, Rect { x: 41, y: 0, w: 40, h: 12 });
        assert_eq!(p[2].1, Rect { x: 41, y: 13, w: 40, h: 11 });
        assert_eq!(s.len(), 2);
        assert_eq!(neighbour(&p, 1, Dir::Right, (0, 20)), Some(3));
        assert_eq!(neighbour(&p, 1, Dir::Right, (0, 2)), Some(2));
        assert_eq!(neighbour(&p, 3, Dir::Up, (50, 13)), Some(2));
        assert_eq!(neighbour(&p, 3, Dir::Left, (50, 13)), Some(1));
        assert_eq!(neighbour(&p, 1, Dir::Left, (0, 0)), None);
    }

    #[test]
    fn remove_collapses() {
        let mut n = Node::Leaf(1);
        n.split(1, 2, true);
        n.split(2, 3, false);
        let n = n.remove(2).unwrap();
        assert_eq!(n.panes(), vec![1, 3]);
        let n = n.remove(1).unwrap();
        assert!(matches!(n, Node::Leaf(3)));
        assert!(n.remove(3).is_none());
    }

    #[test]
    fn resize_moves_separator() {
        let mut n = Node::Leaf(1);
        n.split(1, 2, true);
        assert!(n.resize(1, Dir::Right, 10, full()));
        let (mut p, mut s) = (vec![], vec![]);
        n.layout(full(), &mut p, &mut s);
        assert_eq!(p[0].1.w, 50);
        // No vertical split exists, so up/down cannot resize.
        assert!(!n.resize(1, Dir::Up, 1, full()));
    }
}
