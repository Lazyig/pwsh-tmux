use crate::term::BufWrite as _;
use alloc::string::String;

#[derive(Clone, Debug)]
pub struct Row {
    cells: crate::row_cells::RowCells,
    wrapped: bool,
    /// When the row entered the history (seconds since the epoch), 0 if it
    /// has not or the time was not set (tmux's grid_line time).
    time: u64,
    /// ROW_* (tmux's GRID_LINE_* flags).
    flags: u8,
    /// Hyperlinks and tabs by column, only for rows that have any.
    metadata: Option<alloc::boxed::Box<alloc::collections::BTreeMap<u16, crate::CellMetadata>>>,
}

/// The line has the start of a shell prompt (OSC 133;A, N or P).
pub const ROW_START_PROMPT: u8 = 1;
/// The line has the start of command output (OSC 133;C).
pub const ROW_START_OUTPUT: u8 = 2;
/// The line has had a hyperlink cell (GRID_LINE_HYPERLINK, kept until the
/// line is cleared).
pub const ROW_HYPERLINK: u8 = 4;
/// The line has had a hyperlink or tab cell (part of GRID_LINE_EXTENDED).
pub const ROW_EXTENDED: u8 = 8;
/// OSC 133 B/I command start.
pub const ROW_START_COMMAND: u8 = 16;
/// OSC 133 D output end.
pub const ROW_END_OUTPUT: u8 = 32;
/// OSC 133 P;k=s continuation prompt.
pub const ROW_SECOND_PROMPT: u8 = 64;

impl Row {
    pub fn new(cols: u16) -> Self {
        Self {
            cells: crate::row_cells::RowCells::new(cols),
            wrapped: false,
            time: 0,
            flags: 0,
            metadata: None,
        }
    }

    pub fn flags(&self) -> u8 {
        self.flags
    }

    pub fn set_flags(&mut self, flags: u8) {
        self.flags = flags;
    }

    /// Whether a cell needs more than tmux's compact cell entry (multi-byte or
    /// wide characters, RGB colours, hyperlinks and tabs: GRID_LINE_EXTENDED).
    pub fn extended(&self) -> bool {
        self.flags & ROW_EXTENDED != 0 || self.cells().any(|c| {
            c.contents().len() > 1
                || c.is_wide()
                || matches!(c.fgcolor(), crate::Color::Rgb(..))
                || matches!(c.bgcolor(), crate::Color::Rgb(..))
        })
    }

    pub fn time(&self) -> u64 {
        self.time
    }

    pub fn set_time(&mut self, time: u64) {
        self.time = time;
    }

    fn cols(&self) -> u16 {
        self.cells
            .len()
            .try_into()
            // we limit the number of cols to a u16 (see Size)
            .unwrap()
    }

    pub fn clear(&mut self, attrs: crate::attrs::Attrs) {
        if attrs == crate::attrs::Attrs::new() {
            self.cells.clear_default();
        } else {
            for cell in self.cells.dense_mut() {
                cell.clear(attrs);
            }
        }
        self.wrapped = false;
        self.flags = 0;
        self.metadata = None;
    }

    pub fn reset(&mut self) {
        self.cells.clear_default();
        self.wrapped = false;
        self.time = 0;
        self.flags = 0;
        self.metadata = None;
    }

    fn cells(&self) -> impl Iterator<Item = &crate::Cell> {
        self.cells.iter()
    }

    pub fn get(&self, col: u16) -> Option<&crate::Cell> {
        self.cells.get(usize::from(col))
    }

    pub fn metadata(&self) -> impl Iterator<Item = (u16, &crate::CellMetadata)> {
        self.metadata.iter().flat_map(|m| m.iter().map(|(&x, m)| (x, m)))
    }

    pub fn set_metadata(&mut self, col: u16, value: crate::CellMetadata) {
        if value != crate::CellMetadata::default() {
            self.flags |= ROW_EXTENDED;
            if value.hyperlink.is_some() { self.flags |= ROW_HYPERLINK; }
            self.metadata.get_or_insert_with(Default::default).insert(col, value);
        }
    }

    /// The metadata of the cell at `col`.
    pub fn cell_metadata(&self, col: u16) -> Option<&crate::CellMetadata> {
        self.metadata.as_ref()?.get(&col)
    }

    /// Drops the hyperlinks of every cell (screen_reset_hyperlinks leaves the
    /// cells' links unresolvable); tabs and the line's flags stay.
    pub fn clear_hyperlinks(&mut self) {
        if let Some(m) = &mut self.metadata {
            m.retain(|_, v| { v.hyperlink = None; v.tab_width != 0 });
            if m.is_empty() { self.metadata = None; }
        }
    }

    /// Cell `col` is rewritten: its own metadata goes and a tab covering it
    /// becomes plain spaces.
    fn invalidate(&mut self, col: u16) {
        let Some(m) = &mut self.metadata else { return };
        m.remove(&col);
        if let Some((&x, v)) = m.range_mut(..col).next_back() {
            if col < x.saturating_add(v.tab_width) {
                v.tab_width = 0;
                if v.hyperlink.is_none() { m.remove(&x); }
            }
        }
        if m.is_empty() { self.metadata = None; }
    }

    pub fn get_mut_untracked(&mut self, col: u16) -> Option<&mut crate::Cell> { self.cells.get_mut(usize::from(col)) }

    pub fn get_mut(&mut self, col: u16) -> Option<&mut crate::Cell> {
        self.invalidate(col);
        self.cells.get_mut(usize::from(col))
    }

    /// Inserts empty cells without changing the size of the row.
    ///
    /// Cells at the end are dropped.
    pub fn insert(&mut self, i: u16, count: u16) {
        let count = usize::from(count).min(self.cells.len() - usize::from(i));
        if count == 0 {
            // Always "truncate", even when `count` is 0. However, because we
            // didn't actually insert any cells, we don't need to follow the
            // whole truncation procedure in `Self::set_truncated`; we can just
            // clear the `wrapped` flag.
            self.wrapped = false;
            return;
        }

        let is_wide_continuation = self.get(i).is_some_and(crate::Cell::is_wide_continuation);
        if is_wide_continuation {
            self.get_mut(i).unwrap().set_wide_continuation(false);
        }

        if let Some(m) = self.metadata.take() {
            for (x, mut v) in *m {
                // a tab cut by the insert keeps the cells before it (the rest of its
                // padding moves on as blanks, as tmux's moved padding cells print nothing)
                if x < i && x.saturating_add(v.tab_width) > i { v.tab_width = i - x; }
                let x = if x >= i { x.saturating_add(count as u16) } else { x };
                if x < self.cols() { self.set_metadata(x, v); }
            }
        }
        // only the stored cells (and the empty ones they move over) shift
        let stored = self.cells.stored_len();
        if usize::from(i) < stored {
            let n = (stored + count).min(self.cells.len());
            let slice = &mut self.cells.stored_mut(n)[usize::from(i)..n];
            slice.rotate_right(count);
            slice[..count].fill(crate::Cell::new());
        }
        if is_wide_continuation {
            self.get_mut(i).unwrap().set_wide_continuation(true);
        }
        self.set_truncated();
    }

    /// Removes cells without changing the size of the row.
    ///
    /// Empty cells are added to the end.
    pub fn remove(&mut self, range: core::ops::Range<u16>) {
        self.wrapped = false;
        if range.start >= range.end {
            return;
        }

        self.clear_wide(range.start);
        let last = range.end - 1;
        if last != range.start {
            self.clear_wide(last);
        }

        if let Some(m) = self.metadata.take() {
            for (x, mut v) in *m {
                if x < range.start && x.saturating_add(v.tab_width) > range.start { v.tab_width = range.start - x; }
                if range.contains(&x) { continue; }
                let x = if x >= range.end { x - (range.end - range.start) } else { x };
                self.set_metadata(x, v);
            }
        }
        let count = usize::from(range.end - range.start);
        let stored = self.cells.stored_len();
        if usize::from(range.start) < stored {
            let n = stored.max(usize::from(range.end));
            let slice = &mut self.cells.stored_mut(n)[usize::from(range.start)..n];
            slice[..count].fill(crate::Cell::new());
            slice.rotate_left(count);
        }
    }

    pub fn erase(&mut self, i: u16, attrs: crate::attrs::Attrs) {
        self.invalidate(i);
        let wide = self.cells[usize::from(i)].is_wide();
        self.clear_wide(i);
        self.cells[usize::from(i)].clear(attrs);
        // `wide` is captured pre-erase on purpose: a 2-wide char occupies the
        // logical last column at cols() - 2. saturating_sub guards the
        // degenerate case of an orphaned wide cell in a one-column row, where
        // cols() - 2 would underflow.
        if i == self.cols().saturating_sub(if wide { 2 } else { 1 }) {
            self.wrapped = false;
        }
    }

    pub fn truncate(&mut self, len: u16) {
        if let Some(m) = &mut self.metadata { m.retain(|&x, v| { if x.saturating_add(v.tab_width) > len { v.tab_width = 0; } x < len }); }
        self.cells.truncate(usize::from(len));
        self.set_truncated();
    }

    fn set_truncated(&mut self) {
        self.wrapped = false;
        let Some(last) = self.cells.len().checked_sub(1) else { return };
        if self.cells[last].is_wide() {
            let attrs = *self.cells[last].attrs();
            self.cells[last].clear(attrs);
        }
    }

    fn grow_unchecked(&mut self, len: u16, cell: crate::Cell) {
        debug_assert!(self.cells.len() < usize::from(len));
        self.cells.resize(len.into(), cell);
        self.wrapped = false;
    }

    /// Like [`Self::resize`], but never decreases the size of the row.
    pub fn grow(&mut self, len: u16, cell: crate::Cell) {
        if self.cells.len() < usize::from(len) {
            self.grow_unchecked(len, cell);
        }
    }

    pub fn resize(&mut self, len: u16, cell: crate::Cell) {
        match usize::from(len).cmp(&self.cells.len()) {
            core::cmp::Ordering::Equal => {}
            core::cmp::Ordering::Less => {
                self.truncate(len);
            }
            core::cmp::Ordering::Greater => {
                self.grow_unchecked(len, cell);
            }
        }
    }

    /// Trims trailing blank cells from the row.
    pub fn trim(&mut self) {
        let excess = self
            .cells
            .iter()
            .rev()
            .take_while(|c| **c == crate::cell::Cell::DEFAULT)
            .count();
        if excess > 0 {
            let len = self.cells.len() - excess;
            self.cells.truncate(len);
        }
        self.cells.shrink_to_fit();
    }

    pub(crate) fn compact(&mut self) {
        self.cells.compact();
    }

    pub fn wrap(&mut self, wrap: bool) {
        self.wrapped = wrap;
    }

    pub fn wrapped(&self) -> bool {
        self.wrapped
    }

    pub fn clear_wide(&mut self, col: u16) {
        let col_idx = usize::from(col);
        let cell = &self.cells[col_idx];
        let other_col = if cell.is_wide() { col.saturating_add(1) } else if cell.is_wide_continuation() { col.saturating_sub(1) } else { return };
        self.invalidate(other_col);
        let cell = &self.cells[col_idx];
        let other = if cell.is_wide() {
            let next = col_idx + 1;
            if next >= self.cells.len() {
                // Wide char at the last column with no continuation
                // (e.g. a shrinking resize that split the pair).
                // Clear the cell in place instead of indexing past
                // the end.
                let attrs = *self.cells[col_idx].attrs();
                self.cells[col_idx].clear(attrs);
                return;
            }
            &mut self.cells[next]
        } else if cell.is_wide_continuation() {
            &mut self.cells[usize::from(col - 1)]
        } else {
            return;
        };
        other.clear(*other.attrs());
    }

    pub fn write_contents(
        &self,
        contents: &mut String,
        start: u16,
        width: u16,
        wrapping: bool,
    ) {
        let mut prev_was_wide = false;

        let mut prev_col = start;
        for (col, cell) in self
            .cells()
            .enumerate()
            .skip(usize::from(start))
            .take(usize::from(width))
        {
            if prev_was_wide {
                prev_was_wide = false;
                continue;
            }
            prev_was_wide = cell.is_wide();

            // we limit the number of cols to a u16 (see Size)
            let col: u16 = col.try_into().unwrap();
            if cell.has_contents() {
                for _ in 0..(col - prev_col) {
                    contents.push(' ');
                }
                prev_col += col - prev_col;

                contents.push_str(cell.contents());
                prev_col += if cell.is_wide() { 2 } else { 1 };
            }
        }
        if prev_col == start && wrapping {
            contents.push('\n');
        }
    }

    pub fn write_contents_formatted(
        &self,
        contents: &mut String,
        start: u16,
        width: u16,
        row: u16,
        wrapping: bool,
        prev_pos: Option<crate::grid::Pos>,
        prev_attrs: Option<crate::attrs::Attrs>,
    ) -> (crate::grid::Pos, crate::attrs::Attrs) {
        let mut prev_was_wide = false;

        let mut prev_pos = prev_pos.unwrap_or_else(|| {
            if wrapping {
                crate::grid::Pos {
                    row: row - 1,
                    col: self.cols(),
                }
            } else {
                crate::grid::Pos { row, col: start }
            }
        });
        let mut prev_attrs = prev_attrs.unwrap_or_default();

        let first_cell = &self.cells[usize::from(start)];
        if wrapping && first_cell == &crate::cell::Cell::DEFAULT {
            let default_attrs = crate::cell::Cell::DEFAULT.attrs();
            if prev_attrs != *default_attrs {
                default_attrs.write_escape_code_diff(contents, &prev_attrs);
                prev_attrs = *default_attrs;
            }
            contents.push(' ');
            crate::term::Backspace.write_buf(contents);
            crate::term::EraseChar::new(1).write_buf(contents);
            prev_pos = crate::grid::Pos { row, col: 0 };
        }

        let mut erase: Option<(u16, &crate::attrs::Attrs)> = None;
        for (col, cell) in self
            .cells()
            .enumerate()
            .skip(usize::from(start))
            .take(usize::from(width))
        {
            if prev_was_wide {
                prev_was_wide = false;
                continue;
            }
            prev_was_wide = cell.is_wide();

            // we limit the number of cols to a u16 (see Size)
            let col: u16 = col.try_into().unwrap();
            let pos = crate::grid::Pos { row, col };

            if let Some((prev_col, attrs)) = erase {
                if cell.has_contents() || cell.attrs() != attrs {
                    let new_pos = crate::grid::Pos { row, col: prev_col };
                    if wrapping
                        && prev_pos.row + 1 == new_pos.row
                        && prev_pos.col >= self.cols()
                    {
                        if new_pos.col > 0 {
                            contents.push_str(
                                &" ".repeat(usize::from(new_pos.col)),
                            );
                        } else {
                            contents.push(' ');
                            crate::term::Backspace.write_buf(contents);
                        }
                    } else {
                        crate::term::MoveFromTo::new(prev_pos, new_pos)
                            .write_buf(contents);
                    }
                    prev_pos = new_pos;
                    if &prev_attrs != attrs {
                        attrs.write_escape_code_diff(contents, &prev_attrs);
                        prev_attrs = *attrs;
                    }
                    crate::term::EraseChar::new(pos.col - prev_col)
                        .write_buf(contents);
                    erase = None;
                }
            }

            if *cell != crate::cell::Cell::DEFAULT {
                let attrs = cell.attrs();
                if cell.has_contents() {
                    if pos != prev_pos {
                        if !wrapping
                            || prev_pos.row + 1 != pos.row
                            || prev_pos.col
                                < self.cols() - u16::from(cell.is_wide())
                            || pos.col != 0
                        {
                            crate::term::MoveFromTo::new(prev_pos, pos)
                                .write_buf(contents);
                        }
                        prev_pos = pos;
                    }

                    if &prev_attrs != attrs {
                        attrs.write_escape_code_diff(contents, &prev_attrs);
                        prev_attrs = *attrs;
                    }

                    prev_pos.col += if cell.is_wide() { 2 } else { 1 };
                    let cell_contents = cell.contents();
                    contents.push_str(cell_contents);
                } else if erase.is_none() {
                    erase = Some((pos.col, attrs));
                }
            }
        }
        if let Some((prev_col, attrs)) = erase {
            let new_pos = crate::grid::Pos { row, col: prev_col };
            if wrapping
                && prev_pos.row + 1 == new_pos.row
                && prev_pos.col >= self.cols()
            {
                if new_pos.col > 0 {
                    contents.push_str(&" ".repeat(usize::from(new_pos.col)));
                } else {
                    contents.push(' ');
                    crate::term::Backspace.write_buf(contents);
                }
            } else {
                crate::term::MoveFromTo::new(prev_pos, new_pos)
                    .write_buf(contents);
            }
            prev_pos = new_pos;
            if &prev_attrs != attrs {
                attrs.write_escape_code_diff(contents, &prev_attrs);
                prev_attrs = *attrs;
            }
            crate::term::ClearRowForward.write_buf(contents);
        }

        (prev_pos, prev_attrs)
    }

    pub fn write_contents_formatted_basic(
        &self,
        writer: &mut impl alloc::fmt::Write,
        current_attrs: &mut crate::attrs::Attrs,
    ) -> alloc::fmt::Result {
        let mut prev_was_wide = false;
        let mut num_empty = 0;

        for cell in self.cells() {
            if core::mem::take(&mut prev_was_wide) {
                continue;
            }
            prev_was_wide = cell.is_wide();

            if *cell == crate::cell::Cell::DEFAULT {
                num_empty += 1;
                continue;
            }

            for _ in 0..core::mem::take(&mut num_empty) {
                writer.write_char(' ')?;
            }

            let new_attrs = *cell.attrs();
            new_attrs.write_escape_code_diff_fmt(writer, current_attrs)?;
            *current_attrs = new_attrs;
            if cell.has_contents() {
                writer.write_str(cell.contents())?;
            } else {
                writer.write_char(' ')?;
            }
        }
        Ok(())
    }

    // while it's true that most of the logic in this is identical to
    // write_contents_formatted, i can't figure out how to break out the
    // common parts without making things noticeably slower.
    pub fn write_contents_diff(
        &self,
        contents: &mut String,
        prev: &Self,
        start: u16,
        width: u16,
        row: u16,
        wrapping: bool,
        prev_wrapping: bool,
        mut prev_pos: crate::grid::Pos,
        mut prev_attrs: crate::attrs::Attrs,
    ) -> (crate::grid::Pos, crate::attrs::Attrs) {
        let mut prev_was_wide = false;

        let first_cell = &self.cells[usize::from(start)];
        let prev_first_cell = &prev.cells[usize::from(start)];
        if wrapping
            && !prev_wrapping
            && first_cell == prev_first_cell
            && prev_pos.row + 1 == row
            && prev_pos.col
                >= self.cols() - u16::from(prev_first_cell.is_wide())
        {
            let first_cell_attrs = first_cell.attrs();
            if &prev_attrs != first_cell_attrs {
                first_cell_attrs
                    .write_escape_code_diff(contents, &prev_attrs);
                prev_attrs = *first_cell_attrs;
            }
            let mut cell_contents = prev_first_cell.contents();
            let need_erase = if cell_contents.is_empty() {
                cell_contents = " ";
                true
            } else {
                false
            };
            contents.push_str(cell_contents);
            crate::term::Backspace.write_buf(contents);
            if prev_first_cell.is_wide() {
                crate::term::Backspace.write_buf(contents);
            }
            if need_erase {
                crate::term::EraseChar::new(1).write_buf(contents);
            }
            prev_pos = crate::grid::Pos { row, col: 0 };
        }

        let mut erase: Option<(u16, &crate::attrs::Attrs)> = None;
        for (col, (cell, prev_cell)) in self
            .cells()
            .zip(prev.cells())
            .enumerate()
            .skip(usize::from(start))
            .take(usize::from(width))
        {
            if prev_was_wide {
                prev_was_wide = false;
                continue;
            }
            prev_was_wide = cell.is_wide();

            // we limit the number of cols to a u16 (see Size)
            let col: u16 = col.try_into().unwrap();
            let pos = crate::grid::Pos { row, col };

            if let Some((prev_col, attrs)) = erase {
                if cell.has_contents() || cell.attrs() != attrs {
                    let new_pos = crate::grid::Pos { row, col: prev_col };
                    if wrapping
                        && prev_pos.row + 1 == new_pos.row
                        && prev_pos.col >= self.cols()
                    {
                        if new_pos.col > 0 {
                            contents.push_str(
                                &" ".repeat(usize::from(new_pos.col)),
                            );
                        } else {
                            contents.push(' ');
                            crate::term::Backspace.write_buf(contents);
                        }
                    } else {
                        crate::term::MoveFromTo::new(prev_pos, new_pos)
                            .write_buf(contents);
                    }
                    prev_pos = new_pos;
                    if &prev_attrs != attrs {
                        attrs.write_escape_code_diff(contents, &prev_attrs);
                        prev_attrs = *attrs;
                    }
                    crate::term::EraseChar::new(pos.col - prev_col)
                        .write_buf(contents);
                    erase = None;
                }
            }

            if cell != prev_cell {
                let attrs = cell.attrs();
                if cell.has_contents() {
                    if pos != prev_pos {
                        if !wrapping
                            || prev_pos.row + 1 != pos.row
                            || prev_pos.col
                                < self.cols() - u16::from(cell.is_wide())
                            || pos.col != 0
                        {
                            crate::term::MoveFromTo::new(prev_pos, pos)
                                .write_buf(contents);
                        }
                        prev_pos = pos;
                    }

                    if &prev_attrs != attrs {
                        attrs.write_escape_code_diff(contents, &prev_attrs);
                        prev_attrs = *attrs;
                    }

                    prev_pos.col += if cell.is_wide() { 2 } else { 1 };
                    contents.push_str(cell.contents());
                } else if erase.is_none() {
                    erase = Some((pos.col, attrs));
                }
            }
        }
        if let Some((prev_col, attrs)) = erase {
            let new_pos = crate::grid::Pos { row, col: prev_col };
            if wrapping
                && prev_pos.row + 1 == new_pos.row
                && prev_pos.col >= self.cols()
            {
                if new_pos.col > 0 {
                    contents.push_str(&" ".repeat(usize::from(new_pos.col)));
                } else {
                    contents.push(' ');
                    crate::term::Backspace.write_buf(contents);
                }
            } else {
                crate::term::MoveFromTo::new(prev_pos, new_pos)
                    .write_buf(contents);
            }
            prev_pos = new_pos;
            if &prev_attrs != attrs {
                attrs.write_escape_code_diff(contents, &prev_attrs);
                prev_attrs = *attrs;
            }
            crate::term::ClearRowForward.write_buf(contents);
        }

        // if this row is going from wrapped to not wrapped, we need to erase
        // and redraw the last character to break wrapping. if this row is
        // wrapped, we need to redraw the last character without erasing it to
        // position the cursor after the end of the line correctly so that
        // drawing the next line can just start writing and be wrapped.
        if (!self.wrapped && prev.wrapped) || (!prev.wrapped && self.wrapped)
        {
            let end_pos = if self.cells[usize::from(self.cols() - 1)]
                .is_wide_continuation()
            {
                crate::grid::Pos {
                    row,
                    col: self.cols() - 2,
                }
            } else {
                crate::grid::Pos {
                    row,
                    col: self.cols() - 1,
                }
            };
            crate::term::MoveFromTo::new(prev_pos, end_pos)
                .write_buf(contents);
            prev_pos = end_pos;
            if !self.wrapped {
                crate::term::EraseChar::new(1).write_buf(contents);
            }
            let end_cell = &self.cells[usize::from(end_pos.col)];
            if end_cell.has_contents() {
                let attrs = end_cell.attrs();
                if &prev_attrs != attrs {
                    attrs.write_escape_code_diff(contents, &prev_attrs);
                    prev_attrs = *attrs;
                }
                contents.push_str(end_cell.contents());
                prev_pos.col += if end_cell.is_wide() { 2 } else { 1 };
            }
        }

        (prev_pos, prev_attrs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // An orphaned wide cell (wide flag set, no continuation) in a
    // one-column row must not make Row::erase underflow when it derives the
    // logical last column as cols() - 2. Covers the Copilot review note on
    // the clear_wide OOB fix: clear_wide itself was hardened, but its caller
    // erase computed cols() - 2 == 1 - 2 from the captured `wide` value.
    #[test]
    fn erase_wide_cell_in_one_column_row_does_not_underflow() {
        let attrs = crate::attrs::Attrs::default();
        let mut row = Row::new(1);
        // A 2-wide char in the only column: a wide cell with no room for a
        // continuation cell.
        row.get_mut(0).unwrap().set('中', attrs);
        assert!(row.get(0).unwrap().is_wide());

        row.erase(0, attrs);

        let cell = row.get(0).unwrap();
        assert!(!cell.is_wide(), "erased cell must no longer be wide");
        assert!(!cell.has_contents(), "erased cell must be empty");
    }
}
