use alloc::{boxed::Box, vec::Vec};
use core::ops::{Index, IndexMut};

/// Visible rows remain directly editable. History rows share default ASCII
/// cells and keep a small, lossless palette for all other cells. Reads return
/// references without allocating or expanding the history; only edits expand.
///
/// New and cleared rows are sparse: only the cells up to the last one written
/// are stored, the rest are default cells (like tmux's lazily extended grid
/// lines), so a huge but mostly empty screen costs little memory.
#[derive(Clone, Debug)]
pub(crate) enum RowCells {
    Dense(Vec<crate::Cell>),
    Sparse {
        cells: Vec<crate::Cell>,
        len: usize,
    },
    Compact {
        indices: Box<[u8]>,
        extra: Box<[crate::Cell]>,
    },
}

impl RowCells {
    pub(crate) fn new(cols: u16) -> Self {
        Self::Sparse {
            cells: Vec::new(),
            len: usize::from(cols),
        }
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Dense(cells) => cells.len(),
            Self::Sparse { len, .. } => *len,
            Self::Compact { indices, .. } => indices.len(),
        }
    }

    /// Makes every cell a default cell without storing any.
    pub(crate) fn clear_default(&mut self) {
        *self = Self::Sparse {
            cells: Vec::new(),
            len: self.len(),
        };
    }

    pub(crate) fn truncate(&mut self, new_len: usize) {
        match self {
            Self::Sparse { cells, len } => {
                cells.truncate(new_len);
                *len = (*len).min(new_len);
            }
            // a compact row stays compact: its palette may keep cells it no longer uses
            Self::Compact { indices, .. } => {
                if new_len < indices.len() {
                    let mut kept = core::mem::take(indices).into_vec();
                    kept.truncate(new_len);
                    *indices = kept.into_boxed_slice();
                }
            }
            Self::Dense(cells) => cells.truncate(new_len),
        }
    }

    /// How many cells at the end are default cells, read without expanding the row.
    pub(crate) fn trailing_default(&self) -> usize {
        let default = |c: &&crate::Cell| **c == crate::Cell::DEFAULT;
        match self {
            Self::Dense(cells) => cells.iter().rev().take_while(default).count(),
            Self::Sparse { cells, len } => {
                let stored = cells.iter().rev().take_while(default).count();
                *len - cells.len() + stored
            }
            // index 0 is the default cell, and no palette entry is one
            Self::Compact { indices, .. } => indices.iter().rev().take_while(|&&i| i == 0).count(),
        }
    }

    /// Every wide cell is followed by its continuation and every continuation follows a
    /// wide cell, so copying the cells one by one into a row as wide gives the same row.
    pub(crate) fn wide_pairs_ok(&self) -> bool {
        let any = match self {
            Self::Compact { extra, .. } => extra.iter().any(|c| c.is_wide() || c.is_wide_continuation()),
            Self::Sparse { cells, .. } | Self::Dense(cells) => cells.iter().any(|c| c.is_wide() || c.is_wide_continuation()),
        };
        if !any {
            return true;
        }
        let n = self.len();
        let mut i = 0;
        while i < n {
            let c = &self[i];
            if c.is_wide_continuation() {
                return false;
            }
            if c.is_wide() {
                if !self.get(i + 1).is_some_and(|next| next.is_wide_continuation() && !next.is_wide()) {
                    return false;
                }
                i += 2;
            } else {
                i += 1;
            }
        }
        true
    }

    /// A row of `new_len` default cells after the current ones (at most `new_len`),
    /// stored sparse like a new row the cells were written into.
    pub(crate) fn widen_sparse(&mut self, new_len: usize) {
        debug_assert!(self.len() <= new_len);
        let cells = match core::mem::replace(self, Self::Sparse { cells: Vec::new(), len: 0 }) {
            Self::Dense(cells) | Self::Sparse { cells, .. } => cells,
            compact @ Self::Compact { .. } => compact.iter().cloned().collect(),
        };
        *self = Self::Sparse { cells, len: new_len };
    }

    /// Gives a new row (no stored cells yet) `storage` to write its cells into, when
    /// it is not too large to keep.
    pub(crate) fn give_storage(&mut self, mut storage: Vec<crate::Cell>) {
        if let Self::Sparse { cells, .. } = self {
            if cells.capacity() == 0 && storage.capacity() <= REUSED_CELLS {
                storage.clear();
                *cells = storage;
            }
        }
    }

    pub(crate) fn resize(&mut self, new_len: usize, cell: crate::Cell) {
        match self {
            Self::Sparse { cells, len } if cell == crate::Cell::DEFAULT => {
                cells.truncate(new_len);
                *len = new_len;
            }
            _ => self.dense_mut().resize(new_len, cell),
        }
    }

    pub(crate) fn shrink_to_fit(&mut self) {
        match self {
            Self::Dense(cells) | Self::Sparse { cells, .. } => {
                cells.shrink_to_fit();
            }
            Self::Compact { .. } => {}
        }
    }

    pub(crate) fn get(&self, index: usize) -> Option<&crate::Cell> {
        match self {
            Self::Dense(cells) => cells.get(index),
            Self::Sparse { cells, len } => (index < *len).then(|| {
                cells.get(index).unwrap_or(&crate::cell::ASCII_CELLS[0])
            }),
            Self::Compact { indices, extra } => indices.get(index).map(|&i| {
                if i < 128 {
                    &crate::cell::ASCII_CELLS[usize::from(i)]
                } else {
                    &extra[usize::from(i - 128)]
                }
            }),
        }
    }

    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = &crate::Cell> + ExactSizeIterator {
        (0..self.len()).map(|i| &self[i])
    }

    pub(crate) fn dense_mut(&mut self) -> &mut Vec<crate::Cell> {
        match self {
            Self::Dense(_) => {}
            Self::Sparse { cells, len } => {
                let mut cells = core::mem::take(cells);
                cells.resize(*len, crate::Cell::new());
                *self = Self::Dense(cells);
            }
            Self::Compact { .. } => {
                *self = Self::Dense(self.iter().cloned().collect());
            }
        }
        match self {
            Self::Dense(cells) => cells,
            _ => unreachable!(),
        }
    }

    /// How many cells are stored: those past them are default cells.
    pub(crate) fn stored_len(&self) -> usize {
        match self {
            Self::Sparse { cells, .. } => cells.len(),
            _ => self.len(),
        }
    }

    /// The stored cells, at least the first `upto` of the row.
    pub(crate) fn stored_mut(&mut self, upto: usize) -> &mut [crate::Cell] {
        match self {
            Self::Sparse { cells, len } => {
                let n = upto.min(*len);
                if cells.len() < n {
                    cells.resize(n, crate::Cell::new());
                }
                cells
            }
            _ => self.dense_mut(),
        }
    }

    /// Editing a cell of a sparse row stores the cells up to it only.
    pub(crate) fn get_mut(&mut self, index: usize) -> Option<&mut crate::Cell> {
        match self {
            Self::Sparse { cells, len } => {
                if index >= *len {
                    return None;
                }
                if index >= cells.len() {
                    cells.resize(index + 1, crate::Cell::new());
                }
                cells.get_mut(index)
            }
            _ => self.dense_mut().get_mut(index),
        }
    }

    /// Stores the row compact when that is smaller: ASCII cells with default attributes
    /// as their index, every other distinct cell once in a palette of at most 128
    /// cells, in the order they first appear. A sparse or dense row that becomes
    /// compact hands back its emptied storage; otherwise the row is left as it was.
    pub(crate) fn compact_take(&mut self) -> Option<Vec<crate::Cell>> {
        let (cells, len) = match self {
            Self::Dense(cells) => (&cells[..], cells.len()),
            Self::Sparse { cells, len } => (&cells[..], *len),
            Self::Compact { .. } => return None,
        };
        if len == 0 {
            return None;
        }
        let mut indices = Vec::with_capacity(len);
        let mut extra: Vec<crate::Cell> = Vec::new();
        // open addressing over 256 slots for at most 128 entries: an empty slot (0) is
        // always reached, any other holds palette index + 1
        let mut slots = [0u8; 256];
        for cell in cells {
            let index = if let Some(i) = cell.ascii_index() {
                i
            } else {
                let mut h = cell.palette_hash() as usize & 255;
                loop {
                    let slot = slots[h];
                    if slot == 0 {
                        // Bound both palette storage and the work of palette lookup.
                        if extra.len() == 128 {
                            return None;
                        }
                        extra.push(cell.clone());
                        slots[h] = extra.len() as u8;
                        break 127 + extra.len() as u8;
                    }
                    if extra[usize::from(slot - 1)] == *cell {
                        break 127 + slot;
                    }
                    h = (h + 1) & 255;
                }
            };
            indices.push(index);
        }
        // the cells a sparse row does not store are default cells
        indices.resize(len, 0);
        let dense_bytes = len * core::mem::size_of::<crate::Cell>();
        let compact_bytes = indices.len() + extra.len() * core::mem::size_of::<crate::Cell>();
        if compact_bytes >= dense_bytes {
            return None;
        }
        let compact = Self::Compact {
            indices: indices.into_boxed_slice(),
            extra: extra.into_boxed_slice(),
        };
        match core::mem::replace(self, compact) {
            Self::Dense(cells) | Self::Sparse { cells, .. } => Some(cells),
            Self::Compact { .. } => unreachable!(),
        }
    }

    pub(crate) fn compact(&mut self) {
        self.compact_take();
    }
}

/// The largest storage (in cells) a new row takes over from a row that went into the
/// history compact.
const REUSED_CELLS: usize = 1024;

impl Index<usize> for RowCells {
    type Output = crate::Cell;
    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).expect("cell index out of bounds")
    }
}

impl IndexMut<usize> for RowCells {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        self.get_mut(index).expect("cell index out of bounds")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rows_store_only_written_cells() {
        let mut cells = RowCells::new(10000);
        assert_eq!(cells.len(), 10000);
        assert!(!cells[9999].has_contents());
        assert!(cells.get(10000).is_none());
        cells[2].set('x', crate::attrs::Attrs::new());
        let RowCells::Sparse { cells: stored, .. } = &cells else {
            panic!("an edit keeps the row sparse")
        };
        assert_eq!(stored.len(), 3);
        assert_eq!(cells[2].contents(), "x");
        cells.resize(20000, crate::Cell::new());
        assert_eq!(cells.len(), 20000);
        cells.truncate(2);
        assert_eq!((cells.len(), cells.iter().filter(|c| c.has_contents()).count()), (2, 0));
        cells.resize(5, crate::Cell::new());
        cells[4].set('y', crate::attrs::Attrs::new());
        let full: Vec<_> = cells.iter().cloned().collect();
        assert_eq!(cells.dense_mut(), &full);
        cells.clear_default();
        assert!(matches!(cells, RowCells::Sparse { ref cells, len: 5 } if cells.is_empty()));
    }

    #[test]
    fn ascii_history_reads_and_clones_stay_compact() {
        let mut cells = RowCells::new(209);
        for i in 0..204 {
            cells[i].set((b' ' + (i % 95) as u8) as char, crate::attrs::Attrs::new());
        }
        let expected: Vec<_> = cells.iter().cloned().collect();
        cells.compact();
        let RowCells::Compact { indices, extra } = &cells else {
            panic!("ASCII history must compact")
        };
        assert_eq!(indices.len(), 209);
        assert!(extra.is_empty());
        assert_eq!(cells.iter().cloned().collect::<Vec<_>>(), expected);
        assert!(!cells[208].has_contents());
        assert_eq!(cells[0].contents(), " ");
        let mut edited = cells.clone();
        edited[0].set('X', crate::attrs::Attrs::new());
        assert_eq!(edited[0].contents(), "X");
        assert_eq!(cells[0].contents(), " ");
        assert!(matches!(cells, RowCells::Compact { .. }));
    }

    #[test]
    fn palette_preserves_clusters_width_flags_and_colored_empty_cells() {
        let mut cells = RowCells::new(240);
        let mut attrs = crate::attrs::Attrs::new();
        attrs.fgcolor = crate::Color::Rgb(12, 34, 56);
        attrs.bgcolor = crate::Color::Idx(23);
        attrs.set_bold();
        attrs.set_italic(true);
        let mut palette = Vec::new();
        for text in [
            "한",
            "글",
            "e\u{301}",
            "\u{2764}\u{fe0f}",
            "👩\u{200d}💻",
            " ",
        ] {
            let mut cell = crate::Cell::new();
            cell.set_contents(text, attrs);
            palette.push(cell);
        }
        let mut continuation = crate::Cell::new();
        continuation.set_wide_continuation(true);
        palette.push(continuation);
        let mut blank = crate::Cell::new();
        blank.clear(attrs);
        palette.push(blank);
        for i in 0..cells.len() {
            cells[i] = palette[i % palette.len()].clone();
        }
        let expected: Vec<_> = cells.iter().cloned().collect();
        cells.compact();
        assert!(matches!(cells, RowCells::Compact { .. }));
        assert_eq!(cells.iter().cloned().collect::<Vec<_>>(), expected);
        assert_eq!(cells.dense_mut(), &expected);
    }

    #[test]
    fn palette_boundary_and_high_entropy_fallback_are_lossless() {
        for distinct in [128, 129] {
            let mut cells = RowCells::new(400);
            for i in 0..cells.len() {
                cells[i].set(
                    char::from_u32(0x400 + (i % distinct) as u32).unwrap(),
                    crate::attrs::Attrs::new(),
                );
            }
            let expected: Vec<_> = cells.iter().cloned().collect();
            cells.compact();
            assert_eq!(matches!(cells, RowCells::Compact { .. }), distinct == 128);
            assert_eq!(cells.iter().cloned().collect::<Vec<_>>(), expected);
        }
    }

    /// The palette search compact used before the hashed one: a linear search.
    fn compact_linear(cells: &[crate::Cell]) -> Option<(Vec<u8>, Vec<crate::Cell>)> {
        let mut indices = Vec::new();
        let mut extra: Vec<crate::Cell> = Vec::new();
        for cell in cells {
            let index = if let Some(i) = cell.ascii_index() {
                i
            } else if let Some(i) = extra.iter().position(|c| c == cell) {
                128 + i as u8
            } else {
                if extra.len() == 128 {
                    return None;
                }
                extra.push(cell.clone());
                127 + extra.len() as u8
            };
            indices.push(index);
        }
        let smaller = indices.len() + extra.len() * core::mem::size_of::<crate::Cell>()
            < cells.len() * core::mem::size_of::<crate::Cell>();
        smaller.then_some((indices, extra))
    }

    #[test]
    fn hashed_palette_matches_the_linear_search_in_order() {
        let mut seed = 0x1234_5678u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for round in 0..300 {
            let len = 1 + next() as usize % 300;
            let distinct = 1 + next() % [4, 40, 127, 128, 129, 300][round % 6];
            let sparse = round % 3 == 0;
            let mut cells = RowCells::new(len as u16);
            if !sparse {
                cells.dense_mut();
            }
            for i in 0..len - if sparse { len / 3 } else { 0 } {
                let mut attrs = crate::attrs::Attrs::new();
                let v = next() % distinct;
                match v % 4 {
                    0 => attrs.fgcolor = crate::Color::Idx((v / 4) as u8),
                    1 => attrs.bgcolor = crate::Color::Rgb((v / 4) as u8, 0, 1),
                    2 => attrs.ulcolor = crate::Color::Palette((v / 4) as u8),
                    _ => {}
                }
                match next() % 5 {
                    0 => cells[i].clear(attrs),
                    1 => cells[i].set(char::from_u32(0x4e00 + v).unwrap(), attrs),
                    2 => cells[i].set(char::from(b'a' + (v % 26) as u8), crate::attrs::Attrs::new()),
                    3 => cells[i].set_wide_continuation(true),
                    _ => cells[i].set(char::from(b'a' + (v % 26) as u8), attrs),
                }
            }
            let expected: Vec<_> = cells.iter().cloned().collect();
            let before = cells.clone();
            let storage = cells.compact_take();
            match (compact_linear(&expected), &cells) {
                (Some((indices, extra)), RowCells::Compact { indices: i, extra: e }) => {
                    assert_eq!((&indices[..], &extra[..]), (&i[..], &e[..]), "round {round}");
                    assert!(storage.is_some(), "round {round}");
                }
                (None, _) => {
                    assert!(storage.is_none());
                    assert_eq!(format!("{cells:?}"), format!("{before:?}"), "round {round}");
                }
                (Some(_), _) => panic!("round {round}: the row did not compact"),
            }
            assert_eq!(cells.iter().cloned().collect::<Vec<_>>(), expected, "round {round}");
        }
    }

    #[test]
    fn trimming_a_compact_row_keeps_it_compact() {
        let mut cells = RowCells::new(50);
        let mut attrs = crate::attrs::Attrs::new();
        attrs.fgcolor = crate::Color::Idx(3);
        for i in 0..30 {
            cells[i].set(if i % 2 == 0 { 'x' } else { '\u{4e2d}' }, if i < 20 { attrs } else { crate::attrs::Attrs::new() });
        }
        assert_eq!(cells.trailing_default(), 20);
        let expected: Vec<_> = cells.iter().take(25).cloned().collect();
        cells.compact();
        assert!(matches!(cells, RowCells::Compact { .. }));
        assert_eq!(cells.trailing_default(), 20);
        cells.truncate(25);
        assert!(matches!(cells, RowCells::Compact { .. }));
        assert_eq!(cells.iter().cloned().collect::<Vec<_>>(), expected);
        assert_eq!(cells.trailing_default(), 0);
        cells.widen_sparse(40);
        assert!(matches!(&cells, RowCells::Sparse { cells, len: 40 } if cells.len() == 25));
        assert_eq!(cells.iter().take(25).cloned().collect::<Vec<_>>(), expected);
        assert_eq!(cells.trailing_default(), 15);
    }

    #[test]
    fn wide_pairs_must_match_up() {
        let row = |spec: &str| {
            let mut cells = RowCells::new(spec.len() as u16);
            for (i, b) in spec.bytes().enumerate() {
                match b {
                    b'W' => cells[i].set('\u{4e2d}', crate::attrs::Attrs::new()),
                    b'c' => cells[i].set_wide_continuation(true),
                    b'B' => {
                        cells[i].set('\u{4e2d}', crate::attrs::Attrs::new());
                        cells[i].set_wide_continuation(true);
                    }
                    b'x' => cells[i].set('x', crate::attrs::Attrs::new()),
                    _ => {}
                }
            }
            cells.wide_pairs_ok()
        };
        assert!(row("x x"));
        assert!(row("Wcx Wc"));
        assert!(!row("W"));
        assert!(!row("Wx"));
        assert!(!row("xc"));
        assert!(!row("Wcc"));
        assert!(!row("WB"));
        assert!(!row("xBc"));
        assert!(!row("WW"));
    }
}
