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
            _ => self.dense_mut().truncate(new_len),
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

    pub(crate) fn compact(&mut self) {
        if matches!(self, Self::Sparse { .. }) {
            self.dense_mut();
        }
        let Self::Dense(cells) = self else { return };
        if cells.is_empty() {
            return;
        }
        let mut indices = Vec::with_capacity(cells.len());
        let mut extra = Vec::new();
        for cell in cells.iter() {
            let index = if let Some(i) = cell.ascii_index() {
                i
            } else if let Some(i) = extra.iter().position(|c| c == cell) {
                128 + i as u8
            } else {
                // Bound both palette storage and the work of palette lookup.
                if extra.len() == 128 {
                    return;
                }
                let i = 128 + extra.len() as u8;
                extra.push(cell.clone());
                i
            };
            indices.push(index);
        }
        let dense_bytes = cells.len() * core::mem::size_of::<crate::Cell>();
        let compact_bytes = indices.len() + extra.len() * core::mem::size_of::<crate::Cell>();
        if compact_bytes >= dense_bytes {
            return;
        }
        *self = Self::Compact {
            indices: indices.into_boxed_slice(),
            extra: extra.into_boxed_slice(),
        };
    }
}

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
}
