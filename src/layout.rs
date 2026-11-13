//! Binary split tree for the panes of one window.

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
fn child_rects(horizontal: bool, ratio: f32, r: Rect) -> (Rect, Rect) {
    if horizontal {
        let (wa, wb) = split_len(r.w, ratio);
        let rb = if wb > 0 { Rect { x: r.x + wa + 1, w: wb, ..r } } else { Rect { w: 0, ..r } };
        (Rect { w: wa, ..r }, rb)
    } else {
        let (ha, hb) = split_len(r.h, ratio);
        let rb = if hb > 0 { Rect { y: r.y + ha + 1, h: hb, ..r } } else { Rect { h: 0, ..r } };
        (Rect { h: ha, ..r }, rb)
    }
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
    let tb: u32 = items.iter().map(|(_, s)| *s as u32).sum::<u32>() + items.len() as u32 - 1;
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
    x: u16,
    y: u16,
    kind: CellKind,
}

#[derive(Debug, Clone, PartialEq)]
enum CellKind {
    Leaf(Option<PaneId>),
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

impl Cell {
    fn dump(&self, out: &mut String) {
        use std::fmt::Write;
        let _ = write!(out, "{}x{},{},{}", self.w, self.h, self.x, self.y);
        match &self.kind {
            CellKind::Leaf(id) => {
                if let Some(id) = id {
                    let _ = write!(out, ",{id}");
                }
            }
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

    fn leaves(&self) -> usize {
        match &self.kind {
            CellKind::Leaf(_) => 1,
            CellKind::Container(_, ch) => ch.iter().map(Cell::leaves).sum(),
        }
    }

    /// tmux `layout_check`: children fill their container exactly.
    fn check(&self) -> Result<(), String> {
        if let CellKind::Container(h, ch) = &self.kind {
            if ch.is_empty() {
                return Err("empty layout container".into());
            }
            let mut sum = ch.len() as u32 - 1;
            for c in ch {
                let (cross, size, pcross) = if *h { (c.h, c.w, self.h) } else { (c.w, c.h, self.w) };
                if cross != pcross {
                    return Err("layout cell size mismatch".into());
                }
                sum += size as u32;
                c.check()?;
            }
            if sum != along(*h, Rect { x: 0, y: 0, w: self.w, h: self.h }) as u32 {
                return Err("layout cell sizes do not add up".into());
            }
        }
        Ok(())
    }

    fn into_node(self, ids: &mut impl Iterator<Item = PaneId>) -> Node {
        match self.kind {
            CellKind::Leaf(_) => Node::Leaf(ids.next().expect("pane count checked")),
            CellKind::Container(h, ch) => {
                let items = ch
                    .into_iter()
                    .map(|c| {
                        let s = if h { c.w } else { c.h };
                        (c.into_node(ids), s)
                    })
                    .collect();
                chain(items, h)
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
        let x = self.num16()?;
        self.expect(b',')?;
        let y = self.num16()?;
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

/// Parse a tmux layout string (checksum verified) and map its pane slots, in
/// order, onto `panes`. The returned tree's ratios reproduce the string's cell
/// sizes exactly when laid out over an area of the same size as the layout, and
/// proportionally otherwise.
#[allow(dead_code)]
pub fn parse_layout_string(s: &str, panes: &[PaneId], _area: Rect) -> Result<Node, String> {
    let s = s.trim();
    let (sum, body) = s.split_once(',').ok_or("invalid layout: missing checksum")?;
    if sum.len() != 4 {
        return Err("invalid layout: bad checksum field".into());
    }
    let want = u16::from_str_radix(sum, 16).map_err(|_| "invalid layout: bad checksum field".to_string())?;
    if layout_checksum(body) != want {
        return Err("invalid layout: checksum mismatch".into());
    }
    let mut p = Parser { s: body.as_bytes(), i: 0 };
    let cell = p.cell()?;
    if p.i != body.len() {
        return Err(format!("invalid layout: trailing data at offset {}", p.i));
    }
    cell.check()?;
    if cell.leaves() != panes.len() {
        return Err(format!("layout has {} panes, window has {}", cell.leaves(), panes.len()));
    }
    Ok(cell.into_node(&mut panes.iter().copied()))
}

#[allow(dead_code)]
impl Node {
    fn leaves_mut<'a>(&'a mut self, out: &mut Vec<&'a mut PaneId>) {
        match self {
            Node::Leaf(p) => out.push(p),
            Node::Split { a, b, .. } => {
                a.leaves_mut(out);
                b.leaves_mut(out);
            }
        }
    }

    fn contains(&self, id: PaneId) -> bool {
        match self {
            Node::Leaf(p) => *p == id,
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
            Node::Leaf(_) => None,
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
            Node::Split { horizontal: h, a, b, .. } if *h == horizontal => a.group_count(horizontal) + b.group_count(horizontal),
            _ => 1,
        }
    }

    /// The flattened group items with their rects.
    fn group_items<'a>(&'a mut self, horizontal: bool, r: Rect, out: &mut Vec<(&'a mut Node, Rect)>) {
        if self.is_split(horizontal) {
            if let Node::Split { ratio, a, b, .. } = self {
                let (ra, rb) = child_rects(horizontal, *ratio, r);
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
            Node::Leaf(_) => return false,
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
            Node::Split { horizontal, ratio, a, b } => {
                let (ra, rb) = child_rects(*horizontal, *ratio, r);
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
        Cell { w: r.w, h: r.h, x: r.x, y: r.y, kind }
    }

    /// tmux layout string (`csum,WxH,X,Y{...}`) for this tree laid out over `area`.
    pub fn to_layout_string(&self, area: Rect) -> String {
        let mut body = String::new();
        self.to_cell(area).dump(&mut body);
        format!("{:04x},{body}", layout_checksum(&body))
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

    #[test]
    fn parse_rejects_bad_strings() {
        let a = area(159, 48);
        let body = "159x48,0,0{79x48,0,0,1,79x48,80,0,2}";
        let good = format!("{},{body}", tmux_csum(body));
        assert!(parse_layout_string(&good, &[1, 2], a).is_ok());
        assert!(parse_layout_string(&good, &[1, 2, 3], a).is_err());
        assert!(parse_layout_string(&good, &[1], a).is_err());
        assert!(parse_layout_string(&format!("0000,{body}"), &[1, 2], a).is_err());
        assert!(parse_layout_string(body, &[1, 2], a).is_err());
        for bad in ["159x48,0,0{79x48,0,0,1,78x48,80,0,2}", "159x48,0,0{79x48,0,0,1,79x47,80,0,2}", "159x48,0,0{79x48,0,0,1", "159x48,0,0,1junk", "x"] {
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
