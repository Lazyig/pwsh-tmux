//! Binary split tree for the panes of one window.

use crate::server::Json;
use std::collections::HashMap;

pub type PaneId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
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

/// A floating pane's cell (tmux `layout_cell.g` with LAYOUT_CELL_FLOATING): the size and
/// position of the pane's content; its border, when it has one, is drawn one cell outside.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct FGeom {
    pub x: i32,
    pub y: i32,
    pub w: u16,
    pub h: u16,
}

#[derive(Debug, Clone)]
pub enum Node {
    Leaf(PaneId),
    /// A floating pane. tmux keeps floating cells in the layout tree (after the cell they
    /// were created from, or where a tiled pane was floated), but they take no tiled space:
    /// a split with no tiled pane on one side gives the whole area to the other side.
    Float(PaneId, FGeom),
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

/// split_len, moved so that each side keeps the room its own panes need when it fits.
fn split_len_min(total: u16, ratio: f32, min_a: u16, min_b: u16) -> (u16, u16) {
    let (a, b) = split_len(total, ratio);
    if b == 0 || (a >= min_a && b >= min_b) || min_a as u32 + min_b as u32 + 1 > total as u32 {
        return (a, b);
    }
    let a = a.clamp(min_a, total - 1 - min_b);
    (a, total - 1 - a)
}

impl Node {
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(p) | Node::Float(p, _) => out.push(*p),
            Node::Split { a, b, .. } => {
                a.collect(out);
                b.collect(out);
            }
        }
    }

    /// The tiled panes (no floating ones), in tree order.
    pub fn tiled(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.visit(&mut |n| if let Node::Leaf(p) = n { out.push(*p) });
        out
    }

    /// The floating panes and their cells, in tree order.
    pub fn floats(&self) -> Vec<(PaneId, FGeom)> {
        let mut out = Vec::new();
        self.visit(&mut |n| if let Node::Float(p, g) = n { out.push((*p, *g)) });
        out
    }

    fn visit(&self, f: &mut dyn FnMut(&Node)) {
        f(self);
        if let Node::Split { a, b, .. } = self {
            a.visit(f);
            b.visit(f);
        }
    }

    /// tmux `layout_cell_is_tiled || layout_cell_has_tiled_child`.
    /// The smallest size the tiled panes fit in, one cell each and one per separator
    /// (layout_resize_check's PANE_MINIMUM): a window is never smaller than this.
    pub fn min_size(&self) -> (u16, u16) {
        match self {
            Node::Leaf(_) | Node::Float(..) => (1, 1),
            Node::Split { a, b, .. } if !(a.has_tiled() && b.has_tiled()) => {
                if a.has_tiled() { a.min_size() } else if b.has_tiled() { b.min_size() } else { (1, 1) }
            }
            Node::Split { horizontal, a, b, .. } => {
                let ((aw, ah), (bw, bh)) = (a.min_size(), b.min_size());
                if *horizontal { (aw.saturating_add(bw).saturating_add(1), ah.max(bh)) } else { (aw.max(bw), ah.saturating_add(bh).saturating_add(1)) }
            }
        }
    }

    pub fn has_tiled(&self) -> bool {
        match self {
            Node::Leaf(_) => true,
            Node::Float(..) => false,
            Node::Split { a, b, .. } => a.has_tiled() || b.has_tiled(),
        }
    }

    pub fn is_float(&self, id: PaneId) -> bool {
        self.geom(id).is_some()
    }

    /// The cell of floating pane `id`.
    pub fn geom(&self, id: PaneId) -> Option<FGeom> {
        match self {
            Node::Float(p, g) if *p == id => Some(*g),
            Node::Split { a, b, .. } => a.geom(id).or_else(|| b.geom(id)),
            _ => None,
        }
    }

    pub fn geom_mut(&mut self, id: PaneId) -> Option<&mut FGeom> {
        match self {
            Node::Float(p, g) if *p == id => Some(g),
            Node::Split { a, b, .. } => match a.geom_mut(id) {
                Some(g) => Some(g),
                None => b.geom_mut(id),
            },
            _ => None,
        }
    }

    /// Turn tiled pane `id` into a floating one in place (tmux `layout_remove_tile`: its
    /// space goes to its neighbours) or back (`layout_insert_tile`).
    pub fn set_floating(&mut self, id: PaneId, g: Option<FGeom>) -> bool {
        match self {
            Node::Leaf(p) if *p == id => match g {
                Some(g) => {
                    *self = Node::Float(id, g);
                    true
                }
                None => false,
            },
            Node::Float(p, _) if *p == id => match g {
                Some(_) => false,
                None => {
                    *self = Node::Leaf(id);
                    true
                }
            },
            Node::Split { a, b, .. } => a.set_floating(id, g) || b.set_floating(id, g),
            _ => false,
        }
    }

    /// tmux `layout_floating_pane`: a new floating cell after the cell of `after` (in its
    /// parent, so it joins that node; a root leaf is first replaced by a top-bottom node).
    pub fn insert_float(&mut self, after: Option<PaneId>, new: PaneId, g: FGeom) {
        fn find(n: &mut Node, id: PaneId, parent: Option<bool>) -> Option<(&mut Node, Option<bool>)> {
            match n {
                Node::Leaf(p) | Node::Float(p, _) if *p == id => Some((n, parent)),
                Node::Split { horizontal, a, b, .. } => {
                    let h = Some(*horizontal);
                    match find(a, id, h) {
                        Some(x) => Some(x),
                        None => find(b, id, h),
                    }
                }
                _ => None,
            }
        }
        let float = Node::Float(new, g);
        let (n, parent) = match after {
            Some(a) if self.panes().contains(&a) => find(self, a, None).unwrap(),
            _ => (self, None),
        };
        let horizontal = parent.unwrap_or(false);
        let old = std::mem::replace(n, Node::Leaf(PaneId::MAX));
        *n = Node::Split { horizontal, ratio: 0.5, a: Box::new(old), b: Box::new(float) };
    }

    pub fn layout(&self, r: Rect, panes: &mut Vec<(PaneId, Rect)>, seps: &mut Vec<Sep>) {
        if let Node::Split { a, b, .. } = self {
            match (a.has_tiled(), b.has_tiled()) {
                (true, false) => return a.layout(r, panes, seps),
                (false, true) => return b.layout(r, panes, seps),
                (false, false) => return,
                _ => {}
            }
        }
        match self {
            Node::Leaf(p) => panes.push((*p, r)),
            Node::Float(..) => {}
            Node::Split { horizontal: true, ratio, a, b } => {
                let (wa, wb) = split_len_min(r.w, *ratio, a.min_size().0, b.min_size().0);
                a.layout(Rect { w: wa, ..r }, panes, seps);
                if wb > 0 {
                    seps.push(Sep { vertical: true, x: r.x + wa, y: r.y, len: r.h });
                    b.layout(Rect { x: r.x + wa + 1, w: wb, ..r }, panes, seps);
                } else {
                    b.layout(Rect { w: 0, ..r }, panes, seps);
                }
            }
            Node::Split { horizontal: false, ratio, a, b } => {
                let (ha, hb) = split_len_min(r.h, *ratio, a.min_size().1, b.min_size().1);
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

    /// layout_resize: a window resized from `old` to `new` grows or shrinks its cells as
    /// tmux does, one cell at a time to each child in turn (layout_resize_adjust), not in
    /// proportion. The sizes found are kept as the splits' ratios. `min_w` is the narrowest
    /// a pane can be (PANE_MINIMUM and a reserved scrollbar).
    pub fn tmux_resize(&mut self, old: Rect, new: Rect, min_w: u16) {
        let (mut panes, mut seps) = (Vec::new(), Vec::new());
        self.layout(old, &mut panes, &mut seps);
        let mut sizes: HashMap<PaneId, (i32, i32)> = panes.iter().map(|(p, r)| (*p, (i32::from(r.w), i32::from(r.h)))).collect();
        let Some(mut cell) = self.cell(&sizes) else { return };
        if (cell.sx, cell.sy) != (i32::from(old.w), i32::from(old.h)) {
            return;
        }
        for (horizontal, size) in [(true, i32::from(new.w)), (false, i32::from(new.h))] {
            let current = if horizontal { cell.sx } else { cell.sy };
            let limit = cell.check(horizontal, i32::from(min_w));
            let mut change = size - current;
            if change < 0 && change < -limit {
                change = -limit;
            }
            if limit == 0 {
                change = if size <= current { 0 } else { size - current };
            }
            if change != 0 {
                cell.adjust(horizontal, change, i32::from(min_w));
            }
        }
        cell.store(&mut sizes);
        self.set_ratios(&sizes);
    }

    /// The tiled part as tmux's cells: a run of splits in one direction is one cell.
    fn cell(&self, sizes: &HashMap<PaneId, (i32, i32)>) -> Option<TCell> {
        match self {
            Node::Leaf(p) => sizes.get(p).map(|&(sx, sy)| TCell { pane: Some(*p), horizontal: false, kids: Vec::new(), sx, sy }),
            Node::Float(..) => None,
            Node::Split { horizontal, a, b, .. } => match (a.has_tiled(), b.has_tiled()) {
                (true, false) => a.cell(sizes),
                (false, true) => b.cell(sizes),
                (false, false) => None,
                _ => {
                    let mut kids = Vec::new();
                    for c in [a.cell(sizes)?, b.cell(sizes)?] {
                        if c.pane.is_none() && c.horizontal == *horizontal { kids.extend(c.kids) } else { kids.push(c) }
                    }
                    let n = kids.len() as i32;
                    let (sx, sy) = if *horizontal {
                        (kids.iter().map(|k| k.sx).sum::<i32>() + n - 1, kids[0].sy)
                    } else {
                        (kids[0].sx, kids.iter().map(|k| k.sy).sum::<i32>() + n - 1)
                    };
                    Some(TCell { pane: None, horizontal: *horizontal, kids, sx, sy })
                }
            },
        }
    }

    /// The splits' ratios for these pane sizes.
    fn set_ratios(&mut self, sizes: &HashMap<PaneId, (i32, i32)>) -> Option<(i32, i32)> {
        match self {
            Node::Leaf(p) => sizes.get(p).copied(),
            Node::Float(..) => None,
            Node::Split { horizontal, ratio, a, b } => {
                let (ea, eb) = (a.set_ratios(sizes), b.set_ratios(sizes));
                match (ea, eb) {
                    (Some((aw, ah)), Some((bw, bh))) => {
                        let (x, y) = if *horizontal { (aw, bw) } else { (ah, bh) };
                        if x > 0 && y > 0 {
                            *ratio = x as f32 / (x + y) as f32;
                        }
                        Some(if *horizontal { (aw + 1 + bw, ah) } else { (aw, ah + 1 + bh) })
                    }
                    (e, None) | (None, e) => e,
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
            Node::Leaf(_) | Node::Float(..) => false,
            Node::Split { a, b, .. } => a.split(target, new, horizontal) || b.split(target, new, horizontal),
        }
    }

    /// Remove a leaf, collapsing its parent split. Returns None when the tree becomes empty.
    pub fn remove(self, target: PaneId) -> Option<Node> {
        match self {
            Node::Leaf(p) | Node::Float(p, _) if p == target => None,
            leaf @ (Node::Leaf(_) | Node::Float(..)) => Some(leaf),
            Node::Split { horizontal, ratio, a, b } => match (a.remove(target), b.remove(target)) {
                (Some(a), Some(b)) => Some(Node::Split { horizontal, ratio, a: Box::new(a), b: Box::new(b) }),
                (Some(n), None) | (None, Some(n)) => Some(n),
                (None, None) => None,
            },
        }
    }

    /// Grow/shrink the pane by moving the nearest enclosing separator of the
    /// matching orientation `cells` cells toward `dir` (the mouse: layout_resize_pane
    /// without taking from the other side).
    pub fn resize(&mut self, target: PaneId, dir: Dir, cells: u16, r: Rect) -> bool {
        self.resize_pane(target, dir, cells, r, false)
    }

    /// layout_resize_pane: `cells` right or down grows the pane's cell in the nearest
    /// parent of that direction (left or up shrinks it; the last cell moves the border
    /// before it), taking room from the next cell that has some, then with `opposite`
    /// the ones before; shrinking gives it to the cell after.
    pub fn resize_pane(&mut self, target: PaneId, dir: Dir, cells: u16, r: Rect, opposite: bool) -> bool {
        let (mut panes, mut seps) = (Vec::new(), Vec::new());
        self.layout(r, &mut panes, &mut seps);
        let mut sizes: HashMap<PaneId, (i32, i32)> = panes.iter().map(|(p, r)| (*p, (i32::from(r.w), i32::from(r.h)))).collect();
        let Some(mut cell) = self.cell(&sizes) else { return false };
        let horizontal = matches!(dir, Dir::Left | Dir::Right);
        let change = if matches!(dir, Dir::Right | Dir::Down) { i32::from(cells) } else { -i32::from(cells) };
        let Some(path) = cell.path(target) else { return false };
        // the nearest parent of the same type
        let mut depth = path.len();
        loop {
            if depth == 0 {
                return false;
            }
            let parent = cell.at(&path[..depth - 1]);
            if parent.pane.is_none() && parent.horizontal == horizontal {
                break;
            }
            depth -= 1;
        }
        let parent = cell.at_mut(&path[..depth - 1]);
        let mut idx = path[depth - 1];
        if idx + 1 == parent.kids.len() && idx > 0 {
            idx -= 1;
        }
        let mut needed = change;
        while needed != 0 {
            let size = if change > 0 { parent.grow(idx, horizontal, needed, opposite) } else { parent.shrink(idx, horizontal, needed) };
            if size == 0 {
                break;
            }
            needed += if change > 0 { -size } else { size };
        }
        if needed == change {
            return false;
        }
        cell.store(&mut sizes);
        self.set_ratios(&sizes);
        true
    }

    // Some(true) done, Some(false) target found below but not yet adjusted, None not found.
    fn resize_inner(&mut self, target: PaneId, dir: Dir, cells: u16, r: Rect) -> Option<bool> {
        match self {
            Node::Leaf(p) => (*p == target).then_some(false),
            Node::Float(..) => None,
            // a side with only floating panes is not part of the tiled layout
            Node::Split { a, b, .. } if !a.has_tiled() => b.resize_inner(target, dir, cells, r),
            Node::Split { a, b, .. } if !b.has_tiled() => a.resize_inner(target, dir, cells, r),
            Node::Split { horizontal, ratio, a, b } => {
                let total = if *horizontal { r.w } else { r.h };
                let (ma, mb) = (a.min_size(), b.min_size());
                let (la, _) = if *horizontal { split_len_min(total, *ratio, ma.0, mb.0) } else { split_len_min(total, *ratio, ma.1, mb.1) };
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

// ---- layout presets, editing and tmux layout strings ------------------------

/// Names accepted by [`preset`], in tmux's `next-layout` order.
#[allow(dead_code)] // wired up by the command layer
pub const PRESETS: [&str; 7] = [
    "even-horizontal", "even-vertical", "main-horizontal", "main-horizontal-mirrored",
    "main-vertical", "main-vertical-mirrored", "tiled",
];

/// tmux accepts exact names or unambiguous prefixes of preset names.
pub fn preset_name(name: &str) -> Option<&'static str> {
    if let Some(&name) = PRESETS.iter().find(|&&p| p == name) {
        return Some(name);
    }
    let mut matches = PRESETS.iter().copied().filter(|p| p.starts_with(name));
    let name = matches.next()?;
    matches.next().is_none().then_some(name)
}

/// tmux `PANE_MINIMUM`.
const PANE_MIN: u16 = 1;

/// Ratio that makes `split_len(total, ratio)` give exactly `a` cells to child a,
/// where `total = a + b + 1`.
fn exact_ratio(a: u16, b: u16) -> f32 {
    let avail = a as u32 + b as u32;
    if avail == 0 { 0.5 } else { a as f32 / avail as f32 }
}

/// Split `r` along the axis exactly like `Node::layout` does.
fn child_rects(horizontal: bool, ratio: f32, a: &Node, b: &Node, r: Rect) -> (Rect, Rect) {
    if horizontal {
        let (wa, wb) = split_len_min(r.w, ratio, a.min_size().0, b.min_size().0);
        let rb = if wb > 0 { Rect { x: r.x + wa + 1, w: wb, ..r } } else { Rect { w: 0, ..r } };
        (Rect { w: wa, ..r }, rb)
    } else {
        let (ha, hb) = split_len_min(r.h, ratio, a.min_size().1, b.min_size().1);
        let rb = if hb > 0 { Rect { y: r.y + ha + 1, h: hb, ..r } } else { Rect { h: 0, ..r } };
        (Rect { h: ha, ..r }, rb)
    }
}

/// [`child_rects`] for a split whose side without tiled panes takes no space.
fn tiled_rects(horizontal: bool, ratio: f32, a: &Node, b: &Node, r: Rect) -> (Rect, Rect) {
    if a.has_tiled() && b.has_tiled() { child_rects(horizontal, ratio, a, b, r) } else { (r, r) }
}

fn along(horizontal: bool, r: Rect) -> u16 {
    if horizontal { r.w } else { r.h }
}

/// tmux `layout_spread_cell`: split `size` cells (separators included) into `n`
/// cells as evenly as possible; the first cells take the remainder.
fn even_sizes(size: u16, n: usize) -> Vec<u16> {
    if n == 0 {
        return Vec::new();
    }
    let n32 = n as i64;
    let size = size as i64;
    let each = ((size - (n32 - 1)) / n32).max(1);
    let mut remainder = size - n32 * (each + 1) + 1;
    (0..n)
        .map(|_| {
            let mut this = each;
            if remainder > 0 {
                this += 1;
                remainder -= 1;
            }
            this as u16
        })
        .collect()
}

/// Chain nodes with the given cell sizes along one axis into a right-leaning
/// binary tree whose ratios reproduce those sizes exactly.
fn chain(mut items: Vec<(Node, u16)>, horizontal: bool) -> Node {
    if items.len() == 1 {
        return items.pop().unwrap().0;
    }
    let (first, sa) = items.remove(0);
    // floating items take no space
    let tiled: Vec<u16> = items.iter().filter(|(n, _)| n.has_tiled()).map(|(_, s)| *s).collect();
    let tb: u32 = tiled.iter().map(|s| *s as u32).sum::<u32>() + (tiled.len() as u32).saturating_sub(1);
    let tb = tb.min(u16::MAX as u32) as u16;
    let rest = chain(items, horizontal);
    Node::Split { horizontal, ratio: exact_ratio(sa, tb), a: Box::new(first), b: Box::new(rest) }
}

/// Build a tree for a preset over `panes` (in order; `panes[0]` is the main pane
/// of the main-* layouts). `main_size` is the main pane's rows (main-horizontal)
/// or columns (main-vertical); `None` uses tmux's defaults (24 / 80), clamped to fit.
#[allow(dead_code)]
pub fn preset(name: &str, panes: &[PaneId], area: Rect, main_size: Option<u16>) -> Option<Node> {
    preset_with_options(name, panes, area, main_size, None, 0)
}

pub fn preset_with_options(name: &str, panes: &[PaneId], area: Rect, main_size: Option<u16>, other_size: Option<u16>, max_columns: usize) -> Option<Node> {
    if !PRESETS.contains(&name) || panes.is_empty() {
        return None;
    }
    let n = panes.len();
    if n == 1 {
        return Some(Node::Leaf(panes[0]));
    }
    let even = |ps: &[PaneId], size: u16, horizontal: bool| {
        if ps.len() == 1 {
            return Node::Leaf(ps[0]);
        }
        let items = ps.iter().zip(even_sizes(size, ps.len())).map(|(&p, s)| (Node::Leaf(p), s)).collect();
        chain(items, horizontal)
    };
    let node = match name {
        "even-horizontal" => even(panes, area.w, true),
        "even-vertical" => even(panes, area.h, false),
        "main-horizontal" | "main-vertical" | "main-horizontal-mirrored" | "main-vertical-mirrored" => {
            let horizontal = name.starts_with("main-vertical");
            let total = along(horizontal, area);
            let avail = total.saturating_sub(1);
            let mut main = main_size.unwrap_or(if horizontal { 80 } else { 24 }).max(PANE_MIN);
            if main as u32 + PANE_MIN as u32 >= avail as u32 {
                main = if avail <= 2 * PANE_MIN { PANE_MIN } else { avail - PANE_MIN };
            }
            let other = other_size.filter(|s| *s > 0).unwrap_or(avail.saturating_sub(main)).min(avail.saturating_sub(main));
            main = avail.saturating_sub(other);
            let others = even(&panes[1..], along(!horizontal, area), !horizontal);
            let mut items = vec![(Node::Leaf(panes[0]), main), (others, other)];
            if name.ends_with("-mirrored") {
                items.reverse();
            }
            chain(items, horizontal)
        }
        _ => {
            // tiled (tmux layout_set_tiled).
            let (mut rows, mut cols) = (1usize, 1usize);
            while rows * cols < n {
                rows += 1;
                if rows * cols < n && (max_columns == 0 || cols < max_columns) {
                    cols += 1;
                }
            }
            let width = ((area.w as i64 - (cols as i64 - 1)) / cols as i64).max(PANE_MIN as i64) as u16;
            let height = ((area.h as i64 - (rows as i64 - 1)) / rows as i64).max(PANE_MIN as i64) as u16;
            let mut row_nodes = Vec::new();
            for chunk in panes.chunks(cols) {
                let row = if chunk.len() == 1 {
                    Node::Leaf(chunk[0])
                } else {
                    let mut sizes = vec![width; chunk.len()];
                    let used = chunk.len() as u32 * (width as u32 + 1) - 1;
                    if (area.w as u32) > used {
                        *sizes.last_mut().unwrap() += area.w - used as u16;
                    }
                    chain(chunk.iter().zip(sizes).map(|(&p, s)| (Node::Leaf(p), s)).collect(), true)
                };
                row_nodes.push(row);
            }
            let mut heights = vec![height; row_nodes.len()];
            let used = row_nodes.len() as u32 * (height as u32 + 1) - 1;
            if (area.h as u32) > used {
                *heights.last_mut().unwrap() += area.h - used as u16;
            }
            chain(row_nodes.into_iter().zip(heights).collect(), false)
        }
    };
    Some(node)
}

/// n-ary cell tree, as tmux stores layouts.
#[derive(Debug, Clone, PartialEq)]
struct Cell {
    w: u16,
    h: u16,
    x: i32,
    y: i32,
    kind: CellKind,
}

#[derive(Debug, Clone, PartialEq)]
enum CellKind {
    Leaf(Option<PaneId>),
    Float(PaneId),
    /// `true` = left-right (`{}`), `false` = top-bottom (`[]`).
    Container(bool, Vec<Cell>),
}

/// tmux `layout_checksum`.
fn layout_checksum(s: &str) -> u16 {
    let mut csum: u16 = 0;
    for &c in s.as_bytes() {
        csum = (csum >> 1) + ((csum & 1) << 15);
        csum = csum.wrapping_add(c as u16);
    }
    csum
}

/// What a JSON layout says about a pane besides its cell.
#[derive(Clone, Copy, Debug, Default)]
pub struct LeafInfo {
    pub active: bool,
    /// Position in the window's last-panes stack.
    pub last: Option<usize>,
    pub index: usize,
    /// Position among the floating panes (0 is the front), for floating panes.
    pub z: Option<usize>,
}

impl Cell {
    fn tiled_only(self) -> Option<Cell> {
        match self.kind {
            CellKind::Float(_) => None,
            CellKind::Leaf(_) => Some(self),
            CellKind::Container(h, ch) => {
                let mut ch: Vec<Cell> = ch.into_iter().filter_map(Cell::tiled_only).collect();
                match ch.len() {
                    0 => None,
                    1 => ch.pop(),
                    _ => Some(Cell { kind: CellKind::Container(h, ch), ..self }),
                }
            }
        }
    }

    fn dump_json(&self, out: &mut String, info: &dyn Fn(PaneId) -> LeafInfo) {
        use std::fmt::Write;
        let t = match &self.kind {
            CellKind::Container(true, _) => 'h',
            CellKind::Container(false, _) => 'v',
            _ => 'p',
        };
        let _ = write!(out, "{{\"t\":\"{t}\",\"w\":{},\"h\":{},\"x\":{},\"y\":{}", self.w, self.h, self.x, self.y);
        match &self.kind {
            CellKind::Container(_, ch) => {
                out.push_str(",\"c\":[");
                for (i, c) in ch.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    c.dump_json(out, info);
                }
                out.push(']');
            }
            kind => {
                let (id, floating) = match kind {
                    CellKind::Leaf(id) => (id.unwrap_or(0), false),
                    CellKind::Float(id) => (*id, true),
                    _ => unreachable!(),
                };
                let li = info(id);
                if li.active {
                    out.push_str(",\"a\":true");
                } else if let Some(l) = li.last {
                    let _ = write!(out, ",\"l\":{l}");
                }
                let _ = write!(out, ",\"i\":{}", li.index);
                if floating {
                    let _ = write!(out, ",\"z\":{}", li.z.unwrap_or(0));
                }
                let _ = write!(out, ",\"I\":\"%{id}\"");
            }
        }
        out.push('}');
    }

    fn dump(&self, out: &mut String) {
        use std::fmt::Write;
        let _ = write!(out, "{}x{},{},{}", self.w, self.h, self.x, self.y);
        match &self.kind {
            CellKind::Leaf(id) => {
                if let Some(id) = id {
                    let _ = write!(out, ",{id}");
                }
            }
            CellKind::Float(_) => {}
            CellKind::Container(h, children) => {
                out.push(if *h { '{' } else { '[' });
                for (i, c) in children.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    c.dump(out);
                }
                out.push(if *h { '}' } else { ']' });
            }
        }
    }

}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("invalid layout: expected '{}' at offset {}", c as char, self.i))
        }
    }

    fn num(&mut self) -> Result<u32, String> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i])
            .ok()
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| format!("invalid layout: expected number at offset {start}"))
    }

    fn num16(&mut self) -> Result<u16, String> {
        let n = self.num()?;
        u16::try_from(n).map_err(|_| "invalid layout: number too large".to_string())
    }

    fn cell(&mut self) -> Result<Cell, String> {
        let w = self.num16()?;
        self.expect(b'x')?;
        let h = self.num16()?;
        self.expect(b',')?;
        let x = self.num16()? as i32;
        self.expect(b',')?;
        let y = self.num16()? as i32;
        let mut cell = Cell { w, h, x, y, kind: CellKind::Leaf(None) };
        match self.peek() {
            Some(b',') => {
                // Either ",ID" or the comma before the next sibling ("WxH...").
                let mut j = self.i + 1;
                while self.s.get(j).is_some_and(|c| c.is_ascii_digit()) {
                    j += 1;
                }
                if j > self.i + 1 && self.s.get(j) != Some(&b'x') {
                    self.i += 1;
                    cell.kind = CellKind::Leaf(Some(self.num()?));
                }
            }
            Some(open @ (b'{' | b'[')) => {
                self.i += 1;
                let horizontal = open == b'{';
                let close = if horizontal { b'}' } else { b']' };
                let mut children = vec![self.cell()?];
                while self.peek() == Some(b',') {
                    self.i += 1;
                    children.push(self.cell()?);
                }
                self.expect(close)?;
                cell.kind = CellKind::Container(horizontal, children);
            }
            _ => {}
        }
        Ok(cell)
    }
}

/// tmux PANE_MAXIMUM and WINDOW_MAXIMUM.
const PANE_MAX: i64 = 10000;
const WINDOW_MAX: i64 = 10000;

/// A JSON layout's attributes of one pane cell.
#[derive(Clone, Copy, Debug, Default)]
struct LeafAttr {
    index: i64,
    active: bool,
    last: Option<i64>,
    z: Option<i64>,
}

/// A layout string applied to a window.
#[derive(Debug)]
pub struct Applied {
    pub node: Node,
    pub version: u8,
    /// JSON layouts: the floating panes, front first.
    pub zorder: Vec<PaneId>,
    /// JSON layouts: the active pane and the last-panes stack.
    pub active: Option<PaneId>,
    pub last: Vec<PaneId>,
}

impl Cell {
    fn count(&self, with_floating: bool) -> usize {
        match &self.kind {
            CellKind::Leaf(_) => 1,
            CellKind::Float(_) => usize::from(with_floating),
            CellKind::Container(_, ch) => ch.iter().map(|c| c.count(with_floating)).sum(),
        }
    }

    fn has_tiled(&self) -> bool {
        match &self.kind {
            CellKind::Leaf(_) => true,
            CellKind::Float(_) => false,
            CellKind::Container(_, ch) => ch.iter().any(Cell::has_tiled),
        }
    }

    /// tmux `layout_destroy_cell` of the bottom-right cell (the last child, recursively):
    /// its space goes to the previous cell and a node left with one child is replaced by it.
    /// Returns the slot of the removed leaf.
    fn remove_bottom_right(&mut self) -> Option<PaneId> {
        let CellKind::Container(h, ch) = &mut self.kind else { return None };
        let h = *h;
        let last = ch.last_mut()?;
        if matches!(last.kind, CellKind::Container(..)) {
            return last.remove_bottom_right();
        }
        let gone = ch.pop()?;
        let slot = match gone.kind {
            CellKind::Leaf(s) => s,
            CellKind::Float(s) => Some(s),
            _ => None,
        };
        if gone.has_tiled() {
            if let Some(prev) = ch.iter_mut().rev().find(|c| c.has_tiled()) {
                let add = if h { gone.w } else { gone.h } + 1;
                prev.grow(h, add);
            }
        }
        if ch.len() == 1 {
            let only = ch.pop().unwrap();
            let (x, y) = (self.x, self.y);
            *self = only;
            if self.has_tiled() && matches!(self.kind, CellKind::Leaf(_)) {
                (self.x, self.y) = (x, y);
            }
        }
        slot
    }

    /// Grow a cell along one axis, the last tiled child of a node of that axis taking it.
    fn grow(&mut self, horizontal: bool, add: u16) {
        if horizontal { self.w += add } else { self.h += add }
        match &mut self.kind {
            CellKind::Container(h, ch) if *h == horizontal => {
                if let Some(c) = ch.iter_mut().rev().find(|c| c.has_tiled()) {
                    c.grow(horizontal, add);
                }
            }
            CellKind::Container(_, ch) => {
                for c in ch.iter_mut().filter(|c| c.has_tiled()) {
                    c.grow(horizontal, add);
                }
            }
            _ => {}
        }
    }

    /// tmux `layout_check`, which ignores cells with no tiled pane.
    fn check_tiled(&self) -> bool {
        let CellKind::Container(h, ch) = &self.kind else { return true };
        let mut n = 0u32;
        for c in ch.iter().filter(|c| c.has_tiled()) {
            let (cross, size, pcross) = if *h { (c.h, c.w, self.h) } else { (c.w, c.h, self.w) };
            if cross != pcross || !c.check_tiled() {
                return false;
            }
            n += size as u32 + 1;
        }
        n == 0 || n - 1 == along(*h, Rect { x: 0, y: 0, w: self.w, h: self.h }) as u32
    }

    /// Leaf slots (tiled and floating) in tree order.
    fn slots(&self, out: &mut Vec<(PaneId, bool)>) {
        match &self.kind {
            CellKind::Leaf(s) => out.push((s.unwrap_or(0), false)),
            CellKind::Float(s) => out.push((*s, true)),
            CellKind::Container(_, ch) => ch.iter().for_each(|c| c.slots(out)),
        }
    }

    /// Replace leaf slots with pane ids.
    fn assign(&mut self, map: &HashMap<PaneId, PaneId>) {
        match &mut self.kind {
            CellKind::Leaf(s) => *s = s.and_then(|s| map.get(&s).copied()),
            CellKind::Float(s) => *s = map[s],
            CellKind::Container(_, ch) => ch.iter_mut().for_each(|c| c.assign(map)),
        }
    }

    fn into_tree(self) -> Node {
        match self.kind {
            CellKind::Leaf(id) => Node::Leaf(id.unwrap_or(0)),
            CellKind::Float(id) => Node::Float(id, FGeom { x: self.x, y: self.y, w: self.w, h: self.h }),
            CellKind::Container(h, ch) => chain(ch.into_iter().map(|c| (if h { c.w } else { c.h }, c)).map(|(s, c)| (c.into_tree(), s)).collect(), h),
        }
    }
}

/// tmux `layout_construct_v1`: checksum, then the cells.
fn parse_v1(s: &str) -> Result<Cell, String> {
    let (sum, body) = match s.split_once(',') {
        Some((sum, body)) if sum.len() == 4 && sum.bytes().all(|c| c.is_ascii_hexdigit()) => (sum, body),
        _ => return Err("malformed layout header".into()),
    };
    if layout_checksum(body) != u16::from_str_radix(sum, 16).unwrap_or(0) {
        return Err("invalid layout checksum".into());
    }
    let mut p = Parser { s: body.as_bytes(), i: 0 };
    let mut cell = p.cell().map_err(|_| "invalid layout".to_string())?;
    if p.i != body.len() {
        return Err("trailing data".into());
    }
    fn valid(c: &Cell) -> bool {
        (1..=PANE_MAX).contains(&(c.w as i64)) && (1..=PANE_MAX).contains(&(c.h as i64)) && c.x as i64 <= WINDOW_MAX && c.y as i64 <= WINDOW_MAX
            && match &c.kind { CellKind::Container(_, ch) => ch.iter().all(valid), _ => true }
    }
    if !valid(&cell) {
        return Err("invalid layout".into());
    }
    let mut n = 0;
    fn number(c: &mut Cell, n: &mut PaneId) {
        match &mut c.kind {
            CellKind::Leaf(s) => {
                *s = Some(*n);
                *n += 1;
            }
            CellKind::Container(_, ch) => ch.iter_mut().for_each(|c| number(c, n)),
            CellKind::Float(_) => {}
        }
    }
    number(&mut cell, &mut n);
    Ok(cell)
}

/// tmux `layout_parse_json_layout`: one cell of a JSON layout; leaves get slot numbers.
fn parse_json_cell(node: &Json, attrs: &mut Vec<LeafAttr>, active: &mut usize) -> Result<Cell, String> {
    let find = |key: &str| -> Option<&Json> { match node { Json::Object(m) => m.get(key), _ => None } };
    let not_found = |key: &str| format!("key \"{key}\" not found");
    let string = |key: &str| match find(key) {
        None => Err(not_found(key)),
        Some(Json::Str(s)) => Ok(s.clone()),
        _ => Err(format!("key \"{key}\" expected a string")),
    };
    let number = |key: &str| match find(key) {
        None => Err(not_found(key)),
        Some(Json::Num(n)) => Ok(*n),
        _ => Err(format!("key \"{key}\" expected a number")),
    };
    let t = string("t")?;
    let kind = match t.as_str() {
        "p" => None,
        "v" => Some(false),
        "h" => Some(true),
        _ => return Err(format!("unknown cell type \"{t}\"")),
    };
    let w = number("w")?;
    if !(1..=PANE_MAX).contains(&w) {
        return Err(format!("invalid width {w}"));
    }
    let h = number("h")?;
    if !(1..=PANE_MAX).contains(&h) {
        return Err(format!("invalid height {h}"));
    }
    let x = number("x")?;
    if !(-WINDOW_MAX..=WINDOW_MAX).contains(&x) {
        return Err(format!("invalid x-offset {x}"));
    }
    let y = number("y")?;
    if !(-WINDOW_MAX..=WINDOW_MAX).contains(&y) {
        return Err(format!("invalid y-offset {y}"));
    }
    let (w, h, x, y) = (w as u16, h as u16, x as i32, y as i32);
    let Some(horizontal) = kind else {
        if find("c").is_some() {
            return Err("panes cannot have children".into());
        }
        let index = number("i")?;
        if !(0..=i32::MAX as i64).contains(&index) {
            return Err(format!("invalid index {index}"));
        }
        let mut attr = LeafAttr { index, ..LeafAttr::default() };
        if let Some(a) = find("a") {
            let Json::Bool(a) = a else { return Err("key \"a\" expected a boolean".into()) };
            attr.active = *a;
            *active += usize::from(*a);
        } else if find("l").is_some() {
            let l = number("l")?;
            if !(0..=i32::MAX as i64).contains(&l) {
                return Err(format!("invalid last {l}"));
            }
            attr.last = Some(l);
        }
        if find("z").is_some() {
            let z = number("z")?;
            if !(0..i32::MAX as i64).contains(&z) {
                return Err(format!("invalid floating zindex {z}"));
            }
            attr.z = Some(z);
        }
        let slot = attrs.len() as PaneId;
        attrs.push(attr);
        let kind = if attr.z.is_some() { CellKind::Float(slot) } else { CellKind::Leaf(Some(slot)) };
        return Ok(Cell { w, h, x, y, kind });
    };
    let members = match find("c") {
        None => return Err(not_found("c")),
        Some(Json::Array(m)) => m,
        Some(_) => return Err("key \"c\" expected an array".into()),
    };
    if members.len() < 2 {
        return Err("nodes must have more than one child".into());
    }
    let ch = members.iter().map(|m| parse_json_cell(m, attrs, active)).collect::<Result<Vec<_>, _>>()?;
    Ok(Cell { w, h, x, y, kind: CellKind::Container(horizontal, ch) })
}

/// tmux `layout_parse`: apply a layout string, in the old format (`csum,WxH,X,Y...`) or the
/// JSON one (`{"V":2,"L":{...}}`), to a window whose panes are `panes` (in order) and whose
/// floating panes are `floating`. Extra cells are closed from the bottom right; the old
/// format has only tiled panes, so floating panes stay as they are and are added to the root.
pub fn apply_layout_string(s: &str, panes: &[PaneId], floating: &[(PaneId, FGeom)]) -> Result<Applied, String> {
    let s = s.trim_start();
    let mut attrs = Vec::new();
    let (version, mut cell) = if !s.starts_with('{') {
        (1u8, parse_v1(s)?)
    } else {
        let json = crate::server::json_parse(s)?;
        let Json::Object(top) = &json else { return Err("invalid layout json".into()) };
        let v = match top.get("V") {
            None => return Err("key \"V\" not found".into()),
            Some(Json::Num(n)) => *n,
            Some(_) => return Err("key \"V\" expected a number".into()),
        };
        let root = match top.get("L") {
            None => return Err("key \"L\" not found".into()),
            Some(o @ Json::Object(_)) => o,
            Some(_) => return Err("key \"L\" expected an object".into()),
        };
        let mut active = 0;
        let cell = parse_json_cell(root, &mut attrs, &mut active)?;
        if v != 2 {
            return Err("version mismatch".into());
        }
        if active > 1 {
            return Err("more than one active pane".into());
        }
        // tmux layout_parse_ctx_check_indexes
        let dup = |mut v: Vec<i64>| {
            v.sort_unstable();
            v.windows(2).any(|w| w[0] == w[1])
        };
        if dup(attrs.iter().map(|a| a.index).collect()) {
            return Err("duplicate pane index".into());
        }
        if dup(attrs.iter().filter_map(|a| a.z).collect()) {
            return Err("duplicate pane z-index".into());
        }
        if dup(attrs.iter().filter_map(|a| a.last).collect()) {
            return Err("duplicate last pane index".into());
        }
        (2u8, cell)
    };
    let with_floating = version > 1;
    let npanes = if with_floating { panes.len() } else { panes.len() - floating.len() };
    if npanes == 0 {
        return Err("window has no panes".into());
    }
    loop {
        let ncells = cell.count(with_floating);
        if npanes > ncells {
            return Err(format!("have {npanes} panes but need {ncells}"));
        }
        if npanes == ncells {
            break;
        }
        if cell.remove_bottom_right().is_none() {
            return Err("invalid layout".into());
        }
    }
    // older tmux could write a top cell bigger than its children
    if let CellKind::Container(h, ch) = &cell.kind {
        let tiled: Vec<&Cell> = ch.iter().filter(|c| c.has_tiled()).collect();
        if let Some(first) = tiled.first() {
            let sum = tiled.iter().map(|c| along(*h, Rect { x: 0, y: 0, w: c.w, h: c.h }) as u32 + 1).sum::<u32>() - 1;
            let (w, hh) = if *h { (sum as u16, first.h) } else { (first.w, sum as u16) };
            (cell.w, cell.h) = (w, hh);
        }
    }
    if !cell.check_tiled() {
        return Err("size mismatch after applying layout".into());
    }
    let mut slots = Vec::new();
    cell.slots(&mut slots);
    let mut map = HashMap::new();
    let mut applied = Applied { node: Node::Leaf(0), version, zorder: Vec::new(), active: None, last: Vec::new() };
    if with_floating {
        // cells in index order get the panes in order
        let mut order: Vec<PaneId> = slots.iter().map(|s| s.0).collect();
        order.sort_by_key(|s| attrs[*s as usize].index);
        for (slot, pane) in order.iter().zip(panes) {
            map.insert(*slot, *pane);
        }
        let mut z: Vec<(i64, PaneId)> = slots.iter().filter(|s| s.1).map(|s| (attrs[s.0 as usize].z.unwrap_or(0), map[&s.0])).collect();
        z.sort_unstable();
        applied.zorder = z.into_iter().map(|z| z.1).collect();
        applied.active = slots.iter().find(|s| attrs[s.0 as usize].active).map(|s| map[&s.0]);
        let mut last: Vec<(i64, PaneId)> = slots.iter().filter_map(|s| {
            let a = attrs[s.0 as usize];
            a.last.filter(|_| !a.active).map(|l| (l, map[&s.0]))
        }).collect();
        last.sort_unstable();
        applied.last = last.into_iter().map(|l| l.1).collect();
        cell.assign(&map);
        applied.node = cell.into_tree();
    } else {
        let tiled: Vec<PaneId> = panes.iter().copied().filter(|p| !floating.iter().any(|f| f.0 == *p)).collect();
        for (slot, pane) in slots.iter().zip(&tiled) {
            map.insert(slot.0, *pane);
        }
        cell.assign(&map);
        let mut node = cell.into_tree();
        for p in panes {
            if let Some((_, g)) = floating.iter().find(|f| f.0 == *p) {
                node = attach_float(node, *p, *g);
            }
        }
        applied.node = node;
    }
    Ok(applied)
}

// ---- n-ary cell operations ----------------------------------------------------------------
//
// Floating and tiling a pane and closing a cell follow tmux's n-ary layout cells exactly:
// the tree is converted to cells at the window size, changed like tmux changes its cells,
// and converted back (which reproduces the cell sizes exactly).

impl Cell {
    fn size(&self, horizontal: bool) -> u16 {
        if horizontal { self.w } else { self.h }
    }

    fn at(&self, path: &[usize]) -> &Cell {
        match (path.split_first(), &self.kind) {
            (Some((i, rest)), CellKind::Container(_, ch)) => ch[*i].at(rest),
            _ => self,
        }
    }

    fn at_mut(&mut self, path: &[usize]) -> &mut Cell {
        let mut c = self;
        for i in path {
            if !matches!(c.kind, CellKind::Container(..)) {
                break;
            }
            let CellKind::Container(_, ch) = &mut c.kind else { unreachable!() };
            c = &mut ch[*i];
        }
        c
    }

    fn path_to(&self, id: PaneId) -> Option<Vec<usize>> {
        match &self.kind {
            CellKind::Leaf(Some(p)) | CellKind::Float(p) if *p == id => Some(Vec::new()),
            CellKind::Container(_, ch) => ch.iter().enumerate().find_map(|(i, c)| {
                c.path_to(id).map(|mut p| {
                    p.insert(0, i);
                    p
                })
            }),
            _ => None,
        }
    }

    /// tmux `layout_resize_check`: how much the cell can shrink.
    fn resize_check(&self, horizontal: bool) -> i32 {
        match &self.kind {
            CellKind::Container(h, ch) if *h == horizontal => ch.iter().filter(|c| c.has_tiled()).map(|c| c.resize_check(horizontal)).sum(),
            CellKind::Container(_, ch) => ch.iter().filter(|c| c.has_tiled()).map(|c| c.resize_check(horizontal)).min().unwrap_or(0),
            _ => (self.size(horizontal) as i32 - PANE_MIN as i32).max(0),
        }
    }

    /// tmux `layout_resize_adjust`: change the size, sharing it out among the children.
    fn adjust(&mut self, horizontal: bool, mut change: i32) {
        let v = (self.size(horizontal) as i32 + change).max(0) as u16;
        if horizontal { self.w = v } else { self.h = v }
        let CellKind::Container(h, ch) = &mut self.kind else { return };
        if *h != horizontal {
            for c in ch.iter_mut().filter(|c| c.has_tiled()) {
                c.adjust(horizontal, change);
            }
            return;
        }
        while change != 0 {
            let mut changed = false;
            for c in ch.iter_mut().filter(|c| c.has_tiled()) {
                if change == 0 {
                    break;
                }
                if change > 0 {
                    c.adjust(horizontal, 1);
                    change -= 1;
                    changed = true;
                } else if c.resize_check(horizontal) > 0 {
                    c.adjust(horizontal, -1);
                    change += 1;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// tmux `layout_cell_get_neighbour`: the nearest tiled sibling, after the cell unless it
    /// is the last one.
    fn neighbour(&self, path: &[usize]) -> Option<usize> {
        let (idx, parent) = path.split_last()?;
        let CellKind::Container(_, ch) = &self.at(parent).kind else { return None };
        let after = |i: usize| (i + 1..ch.len()).find(|j| ch[*j].has_tiled());
        let before = |i: usize| (0..i).rev().find(|j| ch[*j].has_tiled());
        if *idx + 1 == ch.len() { before(*idx).or_else(|| after(*idx)) } else { after(*idx).or_else(|| before(*idx)) }
    }

    fn parent_type(&self, path: &[usize]) -> Option<bool> {
        let (_, parent) = path.split_last()?;
        match &self.at(parent).kind {
            CellKind::Container(h, _) => Some(*h),
            _ => None,
        }
    }

    /// tmux `layout_remove_tile`: give the cell's space to its neighbour.
    fn remove_tile(&mut self, path: &[usize]) {
        let Some(h) = self.parent_type(path) else { return };
        let (_, parent) = path.split_last().unwrap();
        match self.neighbour(path) {
            None => self.remove_tile(parent),
            Some(n) => {
                let add = self.at(path).size(h) as i32 + 1;
                let mut np = parent.to_vec();
                np.push(n);
                self.at_mut(&np).adjust(h, add);
            }
        }
        let lc = self.at_mut(path);
        (lc.w, lc.h, lc.x, lc.y) = (0, 0, 0, 0);
    }

    /// tmux `layout_insert_tile`: take half the space of the cell's neighbour.
    fn insert_tile(&mut self, path: &[usize], area: Rect) -> Result<(), String> {
        let Some(h) = self.parent_type(path) else {
            let lc = self.at_mut(path);
            (lc.w, lc.h, lc.x, lc.y) = (area.w, area.h, 0, 0);
            return Ok(());
        };
        let (_, parent) = path.split_last().unwrap();
        match self.neighbour(path) {
            None => {
                self.insert_tile(parent, area)?;
                let size = self.at(parent).size(h);
                let lc = self.at_mut(path);
                if h { lc.w = size } else { lc.h = size }
            }
            Some(n) => {
                let mut np = parent.to_vec();
                np.push(n);
                let ss = self.at(&np).size(h);
                if ss < PANE_MIN * 2 + 1 {
                    return Err("no space for a new pane".into());
                }
                let s2 = ((ss + 1) / 2 - 1).clamp(PANE_MIN, ss - 2);
                let s1 = ss - 1 - s2;
                self.at_mut(&np).adjust(h, s2 as i32 - ss as i32);
                let lc = self.at_mut(path);
                if h { lc.w = s1 } else { lc.h = s1 }
            }
        }
        let cross = self.at(parent).size(!h);
        let lc = self.at_mut(path);
        if h { lc.h = cross } else { lc.w = cross }
        Ok(())
    }

    /// tmux `layout_fix_offsets`: tiled children follow one another.
    fn fix_offsets(&mut self) {
        let (x, y) = (self.x, self.y);
        let CellKind::Container(h, ch) = &mut self.kind else { return };
        let mut off = if *h { x } else { y };
        for c in ch.iter_mut().filter(|c| c.has_tiled()) {
            if *h { (c.x, c.y) = (off, y) } else { (c.x, c.y) = (x, off) }
            c.fix_offsets();
            off += c.size(*h) as i32 + 1;
        }
    }

    /// tmux `layout_destroy_cell`. Returns false when no cell is left.
    fn destroy(&mut self, path: &[usize]) -> bool {
        let Some((idx, parent)) = path.split_last() else { return false };
        let h = self.parent_type(path).unwrap_or(false);
        if self.at(path).has_tiled() {
            match self.neighbour(path) {
                Some(n) => {
                    let add = self.at(path).size(h) as i32 + 1;
                    let mut np = parent.to_vec();
                    np.push(n);
                    self.at_mut(&np).adjust(h, add);
                }
                None => self.remove_tile(parent),
            }
        }
        let pc = self.at_mut(parent);
        if let CellKind::Container(_, ch) = &mut pc.kind {
            ch.remove(*idx);
            if ch.len() == 1 {
                let only = ch.pop().unwrap();
                *pc = only;
                if parent.is_empty() && pc.has_tiled() {
                    (pc.x, pc.y) = (0, 0);
                }
            }
        }
        true
    }
}

impl Node {
    /// Apply a tmux cell operation to the tree laid out over `area`.
    fn with_cells<T>(&mut self, area: Rect, f: impl FnOnce(&mut Cell) -> Result<T, String>) -> Result<T, String> {
        let mut cell = self.to_cell(area);
        let out = f(&mut cell)?;
        if cell.has_tiled() && matches!(cell.kind, CellKind::Leaf(_) | CellKind::Container(..)) {
            (cell.x, cell.y) = (0, 0);
            cell.fix_offsets();
        }
        *self = cell.into_tree();
        Ok(out)
    }

    /// break-pane -W: float tiled pane `id` at `g`, its space going to its neighbour.
    pub fn float_pane(&mut self, id: PaneId, g: FGeom, area: Rect) -> Result<(), String> {
        self.with_cells(area, |c| {
            let path = c.path_to(id).ok_or("pane not found")?;
            if !matches!(c.at(&path).kind, CellKind::Leaf(_)) {
                return Err("pane is already floating".into());
            }
            c.remove_tile(&path);
            let lc = c.at_mut(&path);
            *lc = Cell { w: g.w, h: g.h, x: g.x, y: g.y, kind: CellKind::Float(id) };
            Ok(())
        })
    }

    /// join-pane of a floating pane onto itself: tile it again where it was.
    pub fn tile_pane(&mut self, id: PaneId, area: Rect) -> Result<(), String> {
        self.with_cells(area, |c| {
            let path = c.path_to(id).ok_or("pane not found")?;
            if !matches!(c.at(&path).kind, CellKind::Float(_)) {
                return Err("pane is not floating".into());
            }
            c.at_mut(&path).kind = CellKind::Leaf(Some(id));
            c.insert_tile(&path, area)
        })
    }

    /// tmux `layout_close_pane`: remove pane `id`, its space going to its neighbour. None
    /// when no pane is left.
    pub fn close(mut self, id: PaneId, area: Rect) -> Option<Node> {
        let mut alive = true;
        let found = self.with_cells(area, |c| {
            let path = c.path_to(id).ok_or("pane not found")?;
            alive = c.destroy(&path);
            Ok(())
        });
        match found {
            Err(_) => Some(self),
            Ok(()) if alive => Some(self),
            Ok(()) => None,
        }
    }
}

/// A preset for a window whose panes (in order) may float (`Some` cell). Like tmux's
/// layout_set_*, it arranges the tiled panes, keeps the floating ones where they are and
/// links their cells in pane order into the node holding the other panes (even-*, and main-*
/// with several other panes), or at the end of the root. None when there are not two tiled
/// panes (tmux leaves the layout alone).
pub fn preset_with_floats(name: &str, panes: &[(PaneId, Option<FGeom>)], area: Rect, main_size: Option<u16>, other_size: Option<u16>, max_columns: usize) -> Option<Node> {
    let tiled: Vec<PaneId> = panes.iter().filter(|p| p.1.is_none()).map(|p| p.0).collect();
    if tiled.len() <= 1 {
        return None;
    }
    let mut cell = preset_with_options(name, &tiled, area, main_size, other_size, max_columns)?.to_cell(area);
    let float = |id: PaneId, g: FGeom| Cell { w: g.w, h: g.h, x: g.x, y: g.y, kind: CellKind::Float(id) };
    // interleave the floating cells with the tiled cells of a node, in pane order
    let interleave = |c: &mut Cell, skip: Option<PaneId>| {
        let CellKind::Container(_, ch) = &mut c.kind else { return false };
        let mut old = std::mem::take(ch).into_iter();
        for (id, g) in panes.iter().filter(|p| Some(p.0) != skip) {
            match g {
                Some(g) => ch.push(float(*id, *g)),
                None => ch.extend(old.next()),
            }
        }
        ch.extend(old);
        true
    };
    let main = tiled[0];
    let linked = match name {
        "even-horizontal" | "even-vertical" => interleave(&mut cell, None),
        n if n.starts_with("main-") => match &mut cell.kind {
            CellKind::Container(_, ch) => match ch.iter_mut().find(|c| matches!(c.kind, CellKind::Container(..))) {
                Some(other) => interleave(other, Some(main)),
                None => false,
            },
            _ => false,
        },
        _ => false,
    };
    if !linked {
        if let CellKind::Container(_, ch) = &mut cell.kind {
            ch.extend(panes.iter().filter_map(|(id, g)| g.map(|g| float(*id, g))));
        }
    }
    Some(cell.into_tree())
}

/// Add a floating pane at the end of the root node (a leaf root becomes a top-bottom node),
/// as tmux does for floating panes kept by an old-format layout or a preset.
pub fn attach_float(root: Node, id: PaneId, g: FGeom) -> Node {
    let horizontal = matches!(root, Node::Split { horizontal: true, .. } if root.has_tiled());
    Node::Split { horizontal, ratio: 0.5, a: Box::new(root), b: Box::new(Node::Float(id, g)) }
}

/// Parse a tmux layout string onto `panes` (all tiled).
#[allow(dead_code)]
pub fn parse_layout_string(s: &str, panes: &[PaneId], _area: Rect) -> Result<Node, String> {
    apply_layout_string(s, panes, &[]).map(|a| a.node)
}

#[allow(dead_code)]
impl Node {
    fn leaves_mut<'a>(&'a mut self, out: &mut Vec<&'a mut PaneId>) {
        match self {
            Node::Leaf(p) | Node::Float(p, _) => out.push(p),
            Node::Split { a, b, .. } => {
                a.leaves_mut(out);
                b.leaves_mut(out);
            }
        }
    }

    fn contains(&self, id: PaneId) -> bool {
        match self {
            Node::Leaf(p) | Node::Float(p, _) => *p == id,
            Node::Split { a, b, .. } => a.contains(id) || b.contains(id),
        }
    }

    fn is_split(&self, horizontal: bool) -> bool {
        matches!(self, Node::Split { horizontal: h, .. } if *h == horizontal)
    }

    /// Swap two panes' positions in the tree (no-op unless both are present).
    pub fn swap(&mut self, a: PaneId, b: PaneId) {
        if a == b || !self.contains(a) || !self.contains(b) {
            return;
        }
        let mut leaves = Vec::new();
        self.leaves_mut(&mut leaves);
        for p in leaves {
            if *p == a {
                *p = b;
            } else if *p == b {
                *p = a;
            }
        }
    }

    /// Rotate pane ids through the leaf positions (tmux `rotate-window`):
    /// `down` (-D) moves each pane to the next position (the last becomes first),
    /// otherwise (-U) each pane moves to the previous position (the first becomes last).
    pub fn rotate(&mut self, down: bool) {
        let mut ids = self.panes();
        if ids.len() < 2 {
            return;
        }
        if down {
            ids.rotate_right(1);
        } else {
            ids.rotate_left(1);
        }
        let mut leaves = Vec::new();
        self.leaves_mut(&mut leaves);
        for (p, id) in leaves.into_iter().zip(ids) {
            *p = id;
        }
    }

    /// Replace the leaf `old` with `new`.
    pub fn replace(&mut self, old: PaneId, new: PaneId) -> bool {
        let mut leaves = Vec::new();
        self.leaves_mut(&mut leaves);
        match leaves.into_iter().find(|p| **p == old) {
            Some(p) => {
                *p = new;
                true
            }
            None => false,
        }
    }

    fn find_leaf_node(&mut self, id: PaneId) -> Option<&mut Node> {
        if matches!(self, Node::Leaf(p) if *p == id) {
            return Some(self);
        }
        match self {
            Node::Leaf(_) | Node::Float(..) => None,
            Node::Split { a, b, .. } => match a.find_leaf_node(id) {
                Some(n) => Some(n),
                None => b.find_leaf_node(id),
            },
        }
    }

    /// Split `target` like [`Node::split`], optionally inserting the new pane
    /// before (left of / above) the target, and giving the new pane `size` cells
    /// (`None` = half, as tmux) of the target's current rect `target_rect`.
    pub fn split_ex(&mut self, target: PaneId, new: PaneId, horizontal: bool, before: bool, size: Option<u16>, target_rect: Rect) -> bool {
        let Some(leaf) = self.find_leaf_node(target) else {
            return false;
        };
        let total = along(horizontal, target_rect);
        let ratio = if total < 3 {
            0.5
        } else {
            // tmux layout_split_pane: size2 is the bottom/right cell.
            let size2 = match size {
                None => total.div_ceil(2) - 1,
                Some(s) if before => (total - 1).saturating_sub(s),
                Some(s) => s,
            }
            .clamp(PANE_MIN, total - 2);
            exact_ratio(total - 1 - size2, size2)
        };
        let (a, b) = if before { (new, target) } else { (target, new) };
        *leaf = Node::Split { horizontal, ratio, a: Box::new(Node::Leaf(a)), b: Box::new(Node::Leaf(b)) };
        true
    }

    /// Number of items in the n-ary group rooted here (consecutive splits of
    /// orientation `horizontal` flattened, as tmux stores them).
    fn group_count(&self, horizontal: bool) -> usize {
        match self {
            _ if !self.has_tiled() => 0,
            Node::Split { horizontal: h, a, b, .. } if *h == horizontal => a.group_count(horizontal) + b.group_count(horizontal),
            _ => 1,
        }
    }

    /// The flattened group items with their rects.
    fn group_items<'a>(&'a mut self, horizontal: bool, r: Rect, out: &mut Vec<(&'a mut Node, Rect)>) {
        if !self.has_tiled() {
            return;
        }
        if self.is_split(horizontal) {
            if let Node::Split { ratio, a, b, .. } = self {
                let (ra, rb) = tiled_rects(horizontal, *ratio, a, b, r);
                a.group_items(horizontal, ra, out);
                b.group_items(horizontal, rb, out);
            }
        } else {
            out.push((self, r));
        }
    }

    /// Set the ratios of the group rooted here so its items get `sizes` cells.
    /// Returns the total cell count of the group.
    fn assign_group(&mut self, horizontal: bool, sizes: &[u16]) -> u16 {
        match self {
            Node::Split { horizontal: h, a, b, .. } if *h == horizontal && !a.has_tiled() => b.assign_group(horizontal, sizes),
            Node::Split { horizontal: h, a, b, .. } if *h == horizontal && !b.has_tiled() => a.assign_group(horizontal, sizes),
            Node::Split { horizontal: h, ratio, a, b } if *h == horizontal => {
                let na = a.group_count(horizontal);
                let ta = a.assign_group(horizontal, &sizes[..na]);
                let tb = b.assign_group(horizontal, &sizes[na..]);
                *ratio = exact_ratio(ta, tb);
                ta.saturating_add(tb).saturating_add(1)
            }
            _ => sizes[0],
        }
    }

    /// Set the size of a pane (tmux `resize-pane -x/-y`): the cell containing
    /// the pane in the nearest enclosing split of the right orientation is
    /// resized, taking/giving space from its neighbours like tmux.
    pub fn set_size(&mut self, target: PaneId, width: Option<u16>, height: Option<u16>, area: Rect) -> bool {
        let mut done = false;
        if let Some(w) = width {
            done |= self.set_size_axis(target, true, w, area) == Some(true);
        }
        if let Some(h) = height {
            done |= self.set_size_axis(target, false, h, area) == Some(true);
        }
        done
    }

    // Some(true) applied, Some(false) target found but no suitable split yet, None not found.
    fn set_size_axis(&mut self, target: PaneId, horizontal: bool, want: u16, r: Rect) -> Option<bool> {
        let h = match self {
            Node::Leaf(p) => return (*p == target).then_some(false),
            Node::Float(..) => return None,
            Node::Split { horizontal, .. } => *horizontal,
        };
        let mut items = Vec::new();
        self.group_items(h, r, &mut items);
        let idx = items.iter().position(|(n, _)| n.contains(target))?;
        let (item, ir) = items.swap_remove(idx);
        if item.set_size_axis(target, horizontal, want, ir) == Some(true) {
            return Some(true);
        }
        if h != horizontal {
            return Some(false);
        }
        let mut items = Vec::new();
        self.group_items(h, r, &mut items);
        let mut sizes: Vec<u16> = items.iter().map(|(_, r)| along(h, *r)).collect();
        drop(items);
        resize_in_group(&mut sizes, idx, want.max(PANE_MIN));
        self.assign_group(h, &sizes);
        Some(true)
    }

    /// Spread the target's nearest sibling group that changes, climbing to its
    /// ancestors only if that group already has tmux's even sizes.
    pub fn spread(&mut self, target: PaneId, area: Rect) -> bool {
        let h = match self {
            Node::Leaf(_) | Node::Float(..) => return false,
            Node::Split { horizontal, .. } => *horizontal,
        };
        let mut items = Vec::new();
        self.group_items(h, area, &mut items);
        let Some(idx) = items.iter().position(|(n, _)| n.contains(target)) else { return false };
        let (item, r) = items.swap_remove(idx);
        if item.spread(target, r) {
            return true;
        }
        let mut items = Vec::new();
        self.group_items(h, area, &mut items);
        let sizes = even_sizes(along(h, area), items.len());
        let changed = items.iter().zip(&sizes).any(|((_, r), size)| along(h, *r) != *size);
        drop(items);
        self.assign_group(h, &sizes);
        changed
    }

    fn to_cell(&self, r: Rect) -> Cell {
        let kind = match self {
            Node::Leaf(p) => CellKind::Leaf(Some(*p)),
            Node::Float(p, g) => return Cell { w: g.w, h: g.h, x: g.x, y: g.y, kind: CellKind::Float(*p) },
            Node::Split { horizontal, ratio, a, b } => {
                let (ra, rb) = tiled_rects(*horizontal, *ratio, a, b, r);
                let mut children = Vec::new();
                for c in [a.to_cell(ra), b.to_cell(rb)] {
                    match c.kind {
                        CellKind::Container(h, ch) if h == *horizontal => children.extend(ch),
                        kind => children.push(Cell { kind, ..c }),
                    }
                }
                CellKind::Container(*horizontal, children)
            }
        };
        Cell { w: r.w, h: r.h, x: r.x as i32, y: r.y as i32, kind }
    }

    /// tmux's old (version 1) layout string (`csum,WxH,X,Y{...}`) for this tree laid out
    /// over `area`. It has only the tiled panes (tmux `layout_custom_copy_layout`): floating
    /// cells are left out and nodes left with one child are replaced by it.
    pub fn to_layout_string(&self, area: Rect) -> String {
        let Some(mut cell) = self.to_cell(area).tiled_only() else { return "0000,".into() };
        if matches!(cell.kind, CellKind::Leaf(_)) {
            (cell.x, cell.y) = (0, 0);
        }
        let mut body = String::new();
        cell.dump(&mut body);
        format!("{:04x},{body}", layout_checksum(&body))
    }

    /// tmux's JSON (version 2) layout string: every cell, including floating ones, with
    /// what `info` tells about each pane.
    pub fn to_json_layout(&self, area: Rect, info: &dyn Fn(PaneId) -> LeafInfo) -> String {
        let mut out = String::from("{\"V\":2,\"L\":");
        self.to_cell(area).dump_json(&mut out, info);
        out.push('}');
        out
    }
}

/// Resize item `i` of a group to `want` cells, like tmux layout_resize_pane_to:
/// growing takes space from the following items (then the preceding ones),
/// or from the preceding items for the last one; shrinking gives the space to
/// the adjacent item (the next one, or the previous one for the last item).
fn resize_in_group(sizes: &mut [u16], i: usize, want: u16) {
    let k = sizes.len();
    let cur = sizes[i];
    let last = i + 1 == k;
    if want > cur {
        let mut need = want - cur;
        let order: Vec<usize> = if last { (0..i).rev().collect() } else { (i + 1..k).chain((0..i).rev()).collect() };
        for j in order {
            let take = need.min(sizes[j].saturating_sub(PANE_MIN));
            sizes[j] -= take;
            sizes[i] += take;
            need -= take;
            if need == 0 {
                break;
            }
        }
    } else if want < cur && k > 1 {
        let give = cur - want;
        let j = if last { i - 1 } else { i + 1 };
        sizes[i] -= give;
        sizes[j] += give;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_size_keeps_every_pane_a_cell() {
        // a window never gets smaller than this, and each side of a split keeps what
        // its own panes need when the total allows it
        let inner = Node::Split { horizontal: false, ratio: 0.5, a: Box::new(Node::Leaf(0)), b: Box::new(Node::Leaf(3)) };
        let right = Node::Split { horizontal: true, ratio: 0.5, a: Box::new(Node::Leaf(2)), b: Box::new(Node::Leaf(1)) };
        let root = Node::Split { horizontal: true, ratio: 0.5, a: Box::new(inner), b: Box::new(right) };
        assert_eq!(root.min_size(), (5, 3));
        let (mut panes, mut seps) = (vec![], vec![]);
        root.layout(Rect { x: 0, y: 0, w: 5, h: 3 }, &mut panes, &mut seps);
        let got: Vec<_> = panes.iter().map(|(p, r)| (*p, r.x, r.y, r.w, r.h)).collect();
        assert_eq!(got, vec![(0, 0, 0, 1, 1), (3, 0, 2, 1, 1), (2, 2, 0, 1, 3), (1, 4, 0, 1, 3)]);
    }

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

    // ---- presets / editing / layout strings ----------------------------------

    fn area(w: u16, h: u16) -> Rect {
        Rect { x: 0, y: 0, w, h }
    }

    fn rects(n: &Node, r: Rect) -> Vec<(PaneId, Rect)> {
        let (mut p, mut s) = (vec![], vec![]);
        n.layout(r, &mut p, &mut s);
        p
    }

    fn rect_of(n: &Node, r: Rect, id: PaneId) -> Rect {
        rects(n, r).into_iter().find(|(p, _)| *p == id).unwrap().1
    }

    /// Every cell of `r` is covered exactly once by a pane or a separator, and
    /// every pane is non-empty.
    fn assert_cover(n: &Node, r: Rect) {
        let (mut p, mut s) = (vec![], vec![]);
        n.layout(r, &mut p, &mut s);
        let mut grid = vec![0u8; r.w as usize * r.h as usize];
        let mut mark = |x: u16, y: u16| {
            assert!(x >= r.x && x < r.right() && y >= r.y && y < r.bottom(), "cell {x},{y} outside");
            grid[(y - r.y) as usize * r.w as usize + (x - r.x) as usize] += 1;
        };
        for (id, pr) in &p {
            assert!(pr.w >= 1 && pr.h >= 1, "pane {id} empty: {pr:?}");
            for y in pr.y..pr.bottom() {
                for x in pr.x..pr.right() {
                    mark(x, y);
                }
            }
        }
        for sep in &s {
            for i in 0..sep.len {
                if sep.vertical { mark(sep.x, sep.y + i) } else { mark(sep.x + i, sep.y) }
            }
        }
        assert!(grid.iter().all(|&c| c == 1), "coverage not exact: {p:?}");
    }

    fn spread_of(v: impl Iterator<Item = u16>) -> u16 {
        let v: Vec<u16> = v.collect();
        v.iter().max().unwrap() - v.iter().min().unwrap()
    }

    /// Independent implementation of tmux's layout_checksum for the tests.
    fn tmux_csum(s: &str) -> String {
        let mut csum: u32 = 0;
        for c in s.bytes() {
            csum = (csum >> 1) + ((csum & 1) << 15);
            csum = (csum + c as u32) & 0xffff;
        }
        format!("{csum:04x}")
    }

    const AREAS: [(u16, u16); 3] = [(159, 48), (80, 24), (81, 23)];

    #[test]
    fn preset_unknown_or_empty() {
        assert!(preset("bogus", &[1, 2], area(80, 24), None).is_none());
        assert!(preset("tiled", &[], area(80, 24), None).is_none());
        for name in PRESETS {
            assert!(matches!(preset(name, &[7], area(80, 24), None), Some(Node::Leaf(7))));
        }
    }

    #[test]
    fn presets_cover_area() {
        for (w, h) in AREAS {
            for name in PRESETS {
                for n in 1..=8u32 {
                    let panes: Vec<PaneId> = (1..=n).collect();
                    let t = preset(name, &panes, area(w, h), None).unwrap();
                    let mut actual = t.panes();
                    actual.sort_unstable();
                    assert_eq!(actual, panes, "{name} {n}");
                    assert_cover(&t, area(w, h));
                }
            }
        }
    }

    #[test]
    fn even_presets_are_equal() {
        for (w, h) in AREAS {
            for n in 1..=6u32 {
                let panes: Vec<PaneId> = (1..=n).collect();
                let t = preset("even-horizontal", &panes, area(w, h), None).unwrap();
                let r = rects(&t, area(w, h));
                assert!(spread_of(r.iter().map(|(_, r)| r.w)) <= 1, "{r:?}");
                assert!(r.iter().all(|(_, r)| r.h == h && r.y == 0));
                assert!(r.windows(2).all(|p| p[0].1.right() + 1 == p[1].1.x));

                let t = preset("even-vertical", &panes, area(w, h), None).unwrap();
                let r = rects(&t, area(w, h));
                assert!(spread_of(r.iter().map(|(_, r)| r.h)) <= 1, "{r:?}");
                assert!(r.iter().all(|(_, r)| r.w == w && r.x == 0));
                assert!(r.windows(2).all(|p| p[0].1.bottom() + 1 == p[1].1.y));
            }
        }
        // tmux: the first cells take the remainder.
        let t = preset("even-vertical", &[1, 2, 3], area(80, 48), None).unwrap();
        let hs: Vec<u16> = rects(&t, area(80, 48)).iter().map(|(_, r)| r.h).collect();
        assert_eq!(hs, vec![16, 15, 15]);
    }

    #[test]
    fn main_horizontal() {
        let a = area(159, 48);
        for n in 2..=6u32 {
            let panes: Vec<PaneId> = (1..=n).collect();
            let t = preset("main-horizontal", &panes, a, None).unwrap();
            let r = rects(&t, a);
            assert_eq!(r[0], (1, Rect { x: 0, y: 0, w: 159, h: 24 }));
            let others = &r[1..];
            assert!(others.iter().all(|(_, r)| r.y == 25 && r.h == 23));
            assert!(spread_of(others.iter().map(|(_, r)| r.w)) <= 1);
        }
        let t = preset("main-horizontal", &[1, 2], a, Some(10)).unwrap();
        assert_eq!(rect_of(&t, a, 1).h, 10);
        assert_eq!(rect_of(&t, a, 2), Rect { x: 0, y: 11, w: 159, h: 37 });
        // Clamped to leave room for the other panes (tmux: sy - PANE_MINIMUM).
        let t = preset("main-horizontal", &[1, 2, 3], area(80, 24), None).unwrap();
        assert_eq!(rect_of(&t, area(80, 24), 1).h, 22);
        assert_eq!(rect_of(&t, area(80, 24), 2).h, 1);
    }

    #[test]
    fn main_vertical() {
        let a = area(159, 48);
        for n in 2..=6u32 {
            let panes: Vec<PaneId> = (1..=n).collect();
            let t = preset("main-vertical", &panes, a, None).unwrap();
            let r = rects(&t, a);
            assert_eq!(r[0], (1, Rect { x: 0, y: 0, w: 80, h: 48 }));
            let others = &r[1..];
            assert!(others.iter().all(|(_, r)| r.x == 81 && r.w == 78));
            assert!(spread_of(others.iter().map(|(_, r)| r.h)) <= 1);
        }
        let t = preset("main-vertical", &[1, 2, 3], a, Some(30)).unwrap();
        assert_eq!(rect_of(&t, a, 1).w, 30);
        assert_eq!(rect_of(&t, a, 3).x, 31);
        let t = preset("main-vertical", &[1, 2], area(80, 24), None).unwrap();
        assert_eq!(rect_of(&t, area(80, 24), 1).w, 78);
    }

    #[test]
    fn tiled() {
        let a = area(159, 48);
        let grid = |n: u32| {
            let panes: Vec<PaneId> = (1..=n).collect();
            rects(&preset("tiled", &panes, a, None).unwrap(), a)
        };
        // 2 panes: tmux's loop picks 2 rows x 1 column.
        let r = grid(2);
        assert_eq!(r[0].1, Rect { x: 0, y: 0, w: 159, h: 23 });
        assert_eq!(r[1].1, Rect { x: 0, y: 24, w: 159, h: 24 });
        // 3: two on top, the last spans the bottom row.
        let r = grid(3);
        assert_eq!(r[0].1, Rect { x: 0, y: 0, w: 79, h: 23 });
        assert_eq!(r[1].1, Rect { x: 80, y: 0, w: 79, h: 23 });
        assert_eq!(r[2].1, Rect { x: 0, y: 24, w: 159, h: 24 });
        // 4: 2x2.
        let r = grid(4);
        assert_eq!(r[3].1, Rect { x: 80, y: 24, w: 79, h: 24 });
        assert!(spread_of(r.iter().map(|(_, r)| r.w)) <= 1);
        assert!(spread_of(r.iter().map(|(_, r)| r.h)) <= 1);
        // 5: 3 rows x 2 columns, last row single full-width pane.
        let r = grid(5);
        assert_eq!(r[4].1, Rect { x: 0, y: 32, w: 159, h: 16 });
        assert!(spread_of(r[..4].iter().map(|(_, r)| r.w)) <= 1);
        assert!(spread_of(r.iter().map(|(_, r)| r.h)) <= 1);
        // 6: full 3x2.
        let r = grid(6);
        assert!(spread_of(r.iter().map(|(_, r)| r.w)) <= 1);
        assert!(spread_of(r.iter().map(|(_, r)| r.h)) <= 1);
        assert_eq!(r[5].1, Rect { x: 80, y: 32, w: 79, h: 16 });
        // 8 on 80x24: 3x3 grid, last row of two where the last pane stretches (tmux).
        let a = area(80, 24);
        let t = preset("tiled", &(1..=8).collect::<Vec<_>>(), a, None).unwrap();
        let r = rects(&t, a);
        assert_eq!(r[0].1, Rect { x: 0, y: 0, w: 26, h: 7 });
        assert_eq!(r[6].1, Rect { x: 0, y: 16, w: 26, h: 8 });
        assert_eq!(r[7].1, Rect { x: 27, y: 16, w: 53, h: 8 });
    }

    #[test]
    fn swap_rotate_replace() {
        let mut t = preset("tiled", &[1, 2, 3, 4], area(80, 24), None).unwrap();
        let before = rects(&t, area(80, 24));
        t.swap(1, 4);
        assert_eq!(t.panes(), vec![4, 2, 3, 1]);
        assert_eq!(rect_of(&t, area(80, 24), 4), before[0].1);
        t.swap(1, 99); // missing pane: no-op
        assert_eq!(t.panes(), vec![4, 2, 3, 1]);
        t.swap(4, 1);

        t.rotate(true); // -D: each pane to the next position
        assert_eq!(t.panes(), vec![4, 1, 2, 3]);
        assert_eq!(rect_of(&t, area(80, 24), 1), before[1].1);
        t.rotate(false); // -U undoes it
        assert_eq!(t.panes(), vec![1, 2, 3, 4]);
        t.rotate(false);
        assert_eq!(t.panes(), vec![2, 3, 4, 1]);
        assert_eq!(rect_of(&t, area(80, 24), 2), before[0].1);

        assert!(t.replace(3, 30));
        assert_eq!(t.panes(), vec![2, 30, 4, 1]);
        assert!(!t.replace(3, 31));
        let mut l = Node::Leaf(5);
        l.rotate(true);
        assert!(l.replace(5, 6));
        assert_eq!(l.panes(), vec![6]);
    }

    #[test]
    fn split_ex_positions_and_sizes() {
        let a = full(); // 81x24
        let mut t = Node::Leaf(1);
        assert!(t.split_ex(1, 2, true, false, None, a));
        assert_eq!(rects(&t, a), vec![(1, Rect { x: 0, y: 0, w: 40, h: 24 }), (2, Rect { x: 41, y: 0, w: 40, h: 24 })]);

        let mut t = Node::Leaf(1);
        assert!(t.split_ex(1, 2, true, false, Some(20), a));
        assert_eq!(rects(&t, a), vec![(1, Rect { x: 0, y: 0, w: 60, h: 24 }), (2, Rect { x: 61, y: 0, w: 20, h: 24 })]);

        let mut t = Node::Leaf(1);
        assert!(t.split_ex(1, 2, true, true, Some(20), a));
        assert_eq!(rects(&t, a), vec![(2, Rect { x: 0, y: 0, w: 20, h: 24 }), (1, Rect { x: 21, y: 0, w: 60, h: 24 })]);

        // Vertical, before, half: tmux gives the bottom cell ((sy+1)/2)-1.
        let mut t = Node::Leaf(1);
        assert!(t.split_ex(1, 2, false, true, None, a));
        assert_eq!(rects(&t, a), vec![(2, Rect { x: 0, y: 0, w: 81, h: 12 }), (1, Rect { x: 0, y: 13, w: 81, h: 11 })]);

        // Oversized request is clamped to leave the target 1 cell (tmux: sx - 2); nested target uses its own rect.
        let mut t = Node::Leaf(1);
        t.split_ex(1, 2, true, false, None, a);
        let r2 = rect_of(&t, a, 2);
        assert!(t.split_ex(2, 3, false, false, Some(500), r2));
        assert_eq!(rect_of(&t, a, 3), Rect { x: 41, y: 2, w: 40, h: 22 });
        assert!(!t.split_ex(9, 10, true, false, None, a));
        assert_cover(&t, a);
    }

    #[test]
    fn set_size_resizes_cells() {
        let a = area(159, 48);
        let mut t = preset("even-horizontal", &[1, 2, 3], a, None).unwrap();
        assert!(t.set_size(1, Some(100), None, a));
        let ws: Vec<u16> = rects(&t, a).iter().map(|(_, r)| r.w).collect();
        assert_eq!(ws, vec![100, 5, 52]);
        // Last pane: the space comes from / goes to the previous cell.
        assert!(t.set_size(3, Some(30), None, a));
        let ws: Vec<u16> = rects(&t, a).iter().map(|(_, r)| r.w).collect();
        assert_eq!(ws, vec![100, 27, 30]);
        // Growing beyond the available space takes everything down to 1 cell.
        assert!(t.set_size(2, Some(500), None, a));
        let ws: Vec<u16> = rects(&t, a).iter().map(|(_, r)| r.w).collect();
        assert_eq!(ws, vec![1, 155, 1]);
        // No top-bottom split: -y is not possible.
        assert!(!t.set_size(2, None, Some(10), a));
        assert_cover(&t, a);

        // 1 | (2 / 3)
        let a = full();
        let mut t = Node::Leaf(1);
        t.split(1, 2, true);
        t.split(2, 3, false);
        assert!(t.set_size(2, None, Some(5), a));
        assert_eq!(rect_of(&t, a, 2).h, 5);
        assert_eq!(rect_of(&t, a, 3), Rect { x: 41, y: 6, w: 40, h: 18 });
        assert!(t.set_size(3, Some(20), Some(10), a));
        assert_eq!(rect_of(&t, a, 3), Rect { x: 61, y: 14, w: 20, h: 10 });
        assert_eq!(rect_of(&t, a, 1).w, 60);
        assert!(!t.set_size(1, None, Some(3), a));
        assert!(!t.set_size(42, Some(3), None, a));
        assert_cover(&t, a);
    }

    #[test]
    fn spread_evens_out() {
        let a = area(159, 48);
        // 1 | (2 | (3 | 4)) with default ratios is uneven: 79, 39, 19, 19.
        let mut t = Node::Leaf(1);
        t.split(1, 2, true);
        t.split(2, 3, true);
        t.split(3, 4, true);
        t.split(4, 5, false);
        t.split(5, 6, false);
        t.resize(5, Dir::Down, 7, a);
        let first = rect_of(&t, a, 1);
        t.spread(5, a);
        let r = rects(&t, a);
        assert_eq!(rect_of(&t, a, 1), first, "unrelated group must stay unchanged");
        let col: Vec<u16> = r[3..].iter().map(|(_, r)| r.h).collect();
        assert_eq!(col, vec![16, 15, 15]);
        assert!(t.spread(5, a), "already even siblings: spread the parent group");
        assert_eq!(rect_of(&t, a, 1).w, 39);
        assert_eq!(t.panes(), vec![1, 2, 3, 4, 5, 6]);
        assert_cover(&t, a);
        let mut l = Node::Leaf(1);
        assert!(!l.spread(1, a));
    }

    #[test]
    fn layout_string_format_and_checksum() {
        let a = area(159, 48);
        let t = preset("even-horizontal", &[1, 2], a, None).unwrap();
        let body = "159x48,0,0{79x48,0,0,1,79x48,80,0,2}";
        assert_eq!(t.to_layout_string(a), format!("{},{body}", tmux_csum(body)));
        // Same-orientation chains are flattened into one n-ary node.
        let t = preset("even-horizontal", &[1, 2, 3], a, None).unwrap();
        let s = t.to_layout_string(a);
        assert_eq!(&s[5..], "159x48,0,0{53x48,0,0,1,52x48,54,0,2,52x48,107,0,3}");
        // Mixed orientations nest.
        let t = preset("main-vertical", &[1, 2, 3], a, None).unwrap();
        assert_eq!(&t.to_layout_string(a)[5..], "159x48,0,0{80x48,0,0,1,78x48,81,0[78x24,81,0,2,78x23,81,25,3]}");
        assert_eq!(&Node::Leaf(4).to_layout_string(area(80, 24))[5..], "80x24,0,0,4");
    }

    #[test]
    fn layout_string_round_trip() {
        for (w, h) in AREAS {
            let a = area(w, h);
            for name in PRESETS {
                for n in 1..=6u32 {
                    let panes: Vec<PaneId> = (1..=n).collect();
                    let mut t = preset(name, &panes, a, None).unwrap();
                    if n >= 3 {
                        t.resize(2, Dir::Right, 3, a);
                    }
                    let s = t.to_layout_string(a);
                    let other: Vec<PaneId> = (100..100 + n).collect();
                    let p = parse_layout_string(&s, &other, a).unwrap_or_else(|e| panic!("{s}: {e}"));
                    let got: Vec<Rect> = rects(&p, a).into_iter().map(|(_, r)| r).collect();
                    let want: Vec<Rect> = rects(&t, a).into_iter().map(|(_, r)| r).collect();
                    assert_eq!(got, want, "{name} {n} {s}");
                    assert_eq!(p.panes(), other);
                    // Re-dumping with the original ids gives the same string.
                    let mut q = p.clone();
                    for (o, n) in other.iter().zip(t.panes().iter()) {
                        q.replace(*o, *n);
                    }
                    assert_eq!(q.to_layout_string(a), s);
                }
            }
        }
    }

    #[test]
    fn parse_real_tmux_strings() {
        let a = area(159, 48);
        let body = "159x48,0,0{79x48,0,0,0,79x48,80,0[79x24,80,0,1,79x23,80,25,2]}";
        let s = format!("{},{body}", tmux_csum(body));
        let t = parse_layout_string(&s, &[10, 11, 12], a).unwrap();
        assert_eq!(
            rects(&t, a),
            vec![
                (10, Rect { x: 0, y: 0, w: 79, h: 48 }),
                (11, Rect { x: 80, y: 0, w: 79, h: 24 }),
                (12, Rect { x: 80, y: 25, w: 79, h: 23 }),
            ]
        );
        assert_cover(&t, a);
        // Old-style strings without pane ids.
        let body = "80x24,0,0[80x12,0,0,80x11,0,13]";
        let t = parse_layout_string(&format!("{},{body}", tmux_csum(body)), &[1, 2], area(80, 24)).unwrap();
        assert_eq!(rect_of(&t, area(80, 24), 2), Rect { x: 0, y: 13, w: 80, h: 11 });
        // A different window size scales proportionally and still covers it.
        let body = "159x48,0,0{79x48,0,0,1,79x48,80,0,2}";
        let t = parse_layout_string(&format!("{},{body}", tmux_csum(body)), &[1, 2], area(81, 30)).unwrap();
        assert_eq!(rect_of(&t, area(81, 30), 1).w, 40);
        assert_cover(&t, area(81, 30));
    }

    fn json(n: &Node, a: Rect) -> String {
        n.to_json_layout(a, &|p| LeafInfo { active: p == 1, last: None, index: p as usize, z: n.is_float(p).then_some(0) })
    }

    #[test]
    fn floating_cells_take_no_tiled_space() {
        let a = area(80, 24);
        let g = FGeom { x: 4, y: 2, w: 40, h: 6 };
        let mut n = Node::Leaf(1);
        n.insert_float(Some(1), 2, g);
        assert_eq!(rects(&n, a), vec![(1, a)]);
        assert_eq!(n.floats(), vec![(2, g)]);
        assert_eq!(json(&n, a), r#"{"V":2,"L":{"t":"v","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":80,"h":24,"x":0,"y":0,"a":true,"i":1,"I":"%1"},{"t":"p","w":40,"h":6,"x":4,"y":2,"i":2,"z":0,"I":"%2"}]}}"#);
        // the old format leaves floating panes out
        assert_eq!(&n.to_layout_string(a)[5..], "80x24,0,0,1");
        // splitting the tiled pane nests it; the floating cell stays after it
        n.split_ex(1, 3, true, false, None, a);
        assert_eq!(rects(&n, a), vec![(1, Rect { w: 40, ..a }), (3, Rect { x: 41, w: 39, ..a })]);
        assert!(json(&n, a).contains(r#""c":[{"t":"h","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":40"#));
        // floating a tiled pane gives its space to the neighbour; tiling it takes half back
        n.float_pane(3, g, a).unwrap();
        assert_eq!(rects(&n, a), vec![(1, a)]);
        assert!(n.float_pane(3, g, a).is_err());
        n.tile_pane(3, a).unwrap();
        // (the last cell takes the bigger half from the cell before it)
        assert_eq!(rects(&n, a), vec![(1, Rect { w: 39, ..a }), (3, Rect { x: 40, w: 40, ..a })]);
        // closing a pane gives its space to the next one (tmux layout_destroy_cell)
        let n = preset("even-horizontal", &[1, 3, 4], a, None).unwrap().close(1, a).unwrap();
        assert_eq!(rects(&n, a), vec![(3, Rect { x: 0, w: 53, ..a }), (4, Rect { x: 54, w: 26, ..a })]);
    }

    #[test]
    fn presets_link_floating_cells_like_tmux() {
        let a = area(80, 24);
        let g = FGeom { x: 4, y: 2, w: 40, h: 6 };
        let panes = [(1, None), (2, Some(g)), (3, None), (4, None)];
        let n = preset_with_floats("even-horizontal", &panes, a, None, None, 0).unwrap();
        let ids: Vec<PaneId> = n.panes();
        assert_eq!(ids, vec![1, 2, 3, 4]);
        assert!(json(&n, a).starts_with(r#"{"V":2,"L":{"t":"h","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":26"#));
        // main-*: the floating cell joins the other panes
        let n = preset_with_floats("main-vertical", &panes, a, None, None, 0).unwrap();
        assert!(json(&n, a).ends_with(r#"{"t":"p","w":40,"h":6,"x":4,"y":2,"i":2,"z":0,"I":"%2"},{"t":"p","w":1,"h":12,"x":79,"y":0,"i":3,"I":"%3"},{"t":"p","w":1,"h":11,"x":79,"y":13,"i":4,"I":"%4"}]}]}}"#), "{}", json(&n, a));
        // tiled: at the end of the root
        let n = preset_with_floats("tiled", &panes, a, None, None, 0).unwrap();
        assert_eq!(n.panes(), vec![1, 3, 4, 2]);
        // one tiled pane: unchanged
        assert!(preset_with_floats("tiled", &[(1, None), (2, Some(g))], a, None, None, 0).is_none());
    }

    #[test]
    fn json_layouts_parse_and_check() {
        let a = area(80, 24);
        let s = r#"{"V":2,"L":{"t":"v","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":80,"h":24,"x":0,"y":0,"l":0,"i":0},{"t":"p","w":20,"h":5,"x":-3,"y":3,"a":true,"i":1,"z":0}]}}"#;
        let ap = apply_layout_string(s, &[7, 8], &[]).unwrap();
        assert_eq!(ap.version, 2);
        assert_eq!(ap.active, Some(8));
        assert_eq!(ap.last, vec![7]);
        assert_eq!(ap.zorder, vec![8]);
        assert_eq!(ap.node.floats(), vec![(8, FGeom { x: -3, y: 3, w: 20, h: 5 })]);
        assert_eq!(rects(&ap.node, a), vec![(7, a)]);
        let err = |s: &str| apply_layout_string(s, &[7, 8], &[]).unwrap_err();
        assert_eq!(err(r#"{"V":2,"L":{"t":"p","w":0,"h":24,"x":0,"y":0,"i":0}}"#), "invalid width 0");
        assert_eq!(err(r#"{"V":2,"L":{"t":"v","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":80,"h":24,"x":0,"y":0,"a":true,"i":0},{"t":"p","w":80,"h":24,"x":0,"y":0,"a":true,"i":1,"z":0}]}}"#), "more than one active pane");
        assert_eq!(err(r#"{"V":2,"L":{"t":"v","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":80,"h":24,"x":0,"y":0,"i":0},{"t":"p","w":80,"h":24,"x":0,"y":0,"i":0,"z":0}]}}"#), "duplicate pane index");
        assert_eq!(err(r#"{"V":2,"L":{"t":"h","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":40,"h":24,"x":0,"y":0,"i":0},{"t":"p","w":39,"h":23,"x":41,"y":0,"i":1}]}}"#), "size mismatch after applying layout");
        assert_eq!(err(r#"{"V":2,"L":{"t":"p","w":80,"h":24,"x":0,"y":0,"i":0,"c":[]}}"#), "panes cannot have children");
        assert_eq!(err(r#"{"L":{}}"#), "key \"V\" not found");
    }

    #[test]
    fn parse_rejects_bad_strings() {
        let a = area(159, 48);
        let body = "159x48,0,0{79x48,0,0,1,79x48,80,0,2}";
        let good = format!("{},{body}", tmux_csum(body));
        assert!(parse_layout_string(&good, &[1, 2], a).is_ok());
        assert!(parse_layout_string(&good, &[1, 2, 3], a).is_err());
        // fewer panes than cells: tmux closes cells from the bottom right
        let t = parse_layout_string(&good, &[1], a).unwrap();
        assert_eq!(rects(&t, a), vec![(1, a)]);
        assert_eq!(parse_layout_string(&good, &[1, 2, 3], a).unwrap_err(), "have 3 panes but need 2");
        assert_eq!(parse_layout_string("bad", &[1], a).unwrap_err(), "malformed layout header");
        assert_eq!(parse_layout_string("1234,80x24,0,0", &[1], a).unwrap_err(), "invalid layout checksum");
        assert!(parse_layout_string(&format!("0000,{body}"), &[1, 2], a).is_err());
        assert!(parse_layout_string(body, &[1, 2], a).is_err());
        // a top cell bigger than its children is corrected (older tmux wrote such layouts)
        let fixed = "159x48,0,0{79x48,0,0,1,78x48,80,0,2}";
        assert!(parse_layout_string(&format!("{},{fixed}", tmux_csum(fixed)), &[1, 2], a).is_ok());
        for bad in ["159x48,0,0{79x48,0,0,1,79x47,80,0,2}", "159x48,0,0{79x48,0,0,1", "159x48,0,0,1junk", "x"] {
            assert!(parse_layout_string(&format!("{},{bad}", tmux_csum(bad)), &[1, 2], a).is_err(), "{bad}");
        }
    }
}

#[cfg(test)]
mod display_option_tests {
    use super::*;
    #[test]
    fn tiled_columns_and_other_sizes() {
        let area = Rect { x: 0, y: 0, w: 120, h: 50 };
        let panes: Vec<_> = (0..7).collect();
        let node = preset_with_options("tiled", &panes, area, None, None, 1).unwrap();
        let mut rects = Vec::new();
        node.layout(area, &mut rects, &mut Vec::new());
        assert!(rects.iter().all(|(_, r)| r.w == 120));
        for name in ["main-horizontal", "main-horizontal-mirrored", "main-vertical", "main-vertical-mirrored"] {
            let node = preset_with_options(name, &panes, area, Some(10), Some(15), 0).unwrap();
            rects.clear(); node.layout(area, &mut rects, &mut Vec::new());
            let r = rects.iter().find(|(p, _)| *p == 0).unwrap().1;
            assert_eq!(if name.starts_with("main-horizontal") { r.h } else { r.w }, if name.starts_with("main-horizontal") { 34 } else { 104 });
        }
    }
}

/// A cell of tmux's layout tree for [`Node::tmux_resize`]: a pane, or children side by
/// side (`horizontal`) or stacked.
struct TCell {
    pane: Option<PaneId>,
    horizontal: bool,
    kids: Vec<TCell>,
    sx: i32,
    sy: i32,
}

impl TCell {
    /// layout_resize_check: how much the cell can shrink in that direction.
    fn check(&self, horizontal: bool, min_w: i32) -> i32 {
        if self.pane.is_some() {
            let (available, minimum) = if horizontal { (self.sx, min_w) } else { (self.sy, 1) };
            return if available > minimum { available - minimum } else { 0 };
        }
        if self.horizontal == horizontal {
            self.kids.iter().map(|k| k.check(horizontal, min_w)).sum()
        } else {
            self.kids.iter().map(|k| k.check(horizontal, min_w)).min().unwrap_or(0)
        }
    }

    /// layout_resize_adjust.
    fn adjust(&mut self, horizontal: bool, change: i32, min_w: i32) {
        if horizontal { self.sx += change } else { self.sy += change }
        if self.pane.is_some() {
            return;
        }
        if self.horizontal != horizontal {
            for k in &mut self.kids {
                k.adjust(horizontal, change, min_w);
            }
            return;
        }
        let mut change = change;
        while change != 0 {
            let mut changed = false;
            for k in &mut self.kids {
                if change == 0 {
                    break;
                }
                if change > 0 {
                    k.adjust(horizontal, 1, min_w);
                    change -= 1;
                    changed = true;
                } else if k.check(horizontal, min_w) > 0 {
                    k.adjust(horizontal, -1, min_w);
                    change += 1;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn path(&self, pane: PaneId) -> Option<Vec<usize>> {
        if self.pane == Some(pane) {
            return Some(Vec::new());
        }
        self.kids.iter().enumerate().find_map(|(i, k)| k.path(pane).map(|mut p| {
            p.insert(0, i);
            p
        }))
    }

    fn at(&self, path: &[usize]) -> &TCell {
        path.iter().fold(self, |c, &i| &c.kids[i])
    }

    fn at_mut(&mut self, path: &[usize]) -> &mut TCell {
        path.iter().fold(self, |c, &i| &mut c.kids[i])
    }

    /// layout_resize_pane_grow: from the next cell with room after `idx`, else (with
    /// `opposite`) before it.
    fn grow(&mut self, idx: usize, horizontal: bool, needed: i32, opposite: bool) -> i32 {
        let min_w = 1;
        let mut found = (idx + 1..self.kids.len()).find_map(|j| Some((j, self.kids[j].check(horizontal, min_w))).filter(|(_, s)| *s > 0));
        if found.is_none() && opposite {
            found = (0..idx).rev().find_map(|j| Some((j, self.kids[j].check(horizontal, min_w))).filter(|(_, s)| *s > 0));
        }
        let Some((j, size)) = found else { return 0 };
        let size = size.min(needed);
        self.kids[idx].adjust(horizontal, size, min_w);
        self.kids[j].adjust(horizontal, -size, min_w);
        size
    }

    /// layout_resize_pane_shrink: from `idx` or the nearest cell with room before it,
    /// onto the cell after `idx`.
    fn shrink(&mut self, idx: usize, horizontal: bool, needed: i32) -> i32 {
        let min_w = 1;
        let Some((j, size)) = (0..=idx).rev().find_map(|j| Some((j, self.kids[j].check(horizontal, min_w))).filter(|(_, s)| *s > 0)) else { return 0 };
        if idx + 1 >= self.kids.len() {
            return 0;
        }
        let size = size.min(-needed);
        self.kids[idx + 1].adjust(horizontal, size, min_w);
        self.kids[j].adjust(horizontal, -size, min_w);
        size
    }

    fn store(&self, sizes: &mut HashMap<PaneId, (i32, i32)>) {
        match self.pane {
            Some(p) => {
                sizes.insert(p, (self.sx, self.sy));
            }
            None => self.kids.iter().for_each(|k| k.store(sizes)),
        }
    }
}
