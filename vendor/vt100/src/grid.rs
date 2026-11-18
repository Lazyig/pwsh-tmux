use crate::term::BufWrite as _;
use alloc::{string::String, vec::Vec};

#[derive(Clone, Debug)]
pub struct Grid {
    size: Size,
    pos: Pos,
    saved_pos: Pos,
    rows: Vec<crate::row::Row>,
    scroll_top: u16,
    scroll_bottom: u16,
    origin_mode: bool,
    saved_origin_mode: bool,
    scrollback: Option<ScrollbackState>,
    /// Rows added to history, rows dropped from it, and clears/rewraps.
    history_counters: (u64, u64, u64),
    /// The current time (seconds since the epoch, 0 = unknown) given to rows as
    /// they enter the history.
    now: u64,
}

#[derive(Clone, Debug)]
struct ScrollbackState {
    rows: alloc::collections::VecDeque<crate::row::Row>,
    capacity: usize,
    offset: usize,
    /// The number of rows of scrollback there *would* be if the scrollback
    /// capacity were unlimited.
    unbounded_len: usize,
}

#[derive(Clone, Copy, Debug)]
pub enum Scrollback {
    Disabled,
    Enabled { capacity: usize },
}

impl Grid {
    pub fn new(size: Size, scrollback: Scrollback) -> Self {
        let scrollback = match scrollback {
            Scrollback::Disabled => None,
            Scrollback::Enabled { capacity } => Some(ScrollbackState {
                rows: alloc::collections::VecDeque::new(),
                capacity,
                offset: 0,
                unbounded_len: 0,
            }),
        };
        Self {
            size,
            pos: Pos::default(),
            saved_pos: Pos::default(),
            rows: Vec::new(),
            scroll_top: 0,
            scroll_bottom: size.rows() - 1,
            origin_mode: false,
            saved_origin_mode: false,
            scrollback,
            history_counters: (0, 0, 0),
            now: 0,
        }
    }

    pub fn allocate_rows(&mut self) {
        if self.rows.is_empty() {
            self.rows.resize_with(usize::from(self.size.rows()), || {
                crate::row::Row::new(self.size.cols())
            });
        }
    }

    pub fn clear(&mut self) {
        self.pos = Pos::default();
        self.saved_pos = Pos::default();
        for row in self.drawing_rows_mut() {
            row.reset();
        }
        self.scroll_top = 0;
        self.scroll_bottom = self.size.rows() - 1;
        self.origin_mode = false;
        self.saved_origin_mode = false;
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn set_size<F>(&mut self, size: Size, on_row: F)
    where
        F: FnMut(&crate::row::Row),
    {
        if size == self.size {
            return;
        }

        // xterm always resets the scroll region and scrollback offset when the
        // screen is resized.
        self.scroll_top = 0;
        self.scroll_bottom = self.size.rows() - 1;
        if let Some(scrollback) = &mut self.scrollback {
            scrollback.offset = 0;
        }

        let new_row = || crate::row::Row::new(size.cols());
        let diff = self.size.rows().abs_diff(size.rows());
        // `rows` might be empty if this is the alternate screen and the rows
        // haven't been allocated (Self::allocate_rows) yet.
        let is_empty = self.rows.is_empty();
        let pos_diff = if is_empty {
            0
        } else if size.rows() < self.size.rows() {
            let num_excess_bottom = self.size.rows() - self.pos.row - 1;
            let num_pushed = diff.saturating_sub(num_excess_bottom);
            self.scroll_up_stamped(num_pushed, on_row, false);
            self.rows.truncate(size.rows().into());
            num_pushed
        } else if let Some(scrollback) = &mut self.scrollback {
            let num_pulled = u16::try_from(scrollback.unbounded_len)
                .unwrap_or(u16::MAX)
                .min(diff);
            // Number of rows actually pulled from scrollback.
            let num_pulled_scrollback =
                scrollback.rows.len().min(num_pulled.into());
            // Number of blank rows inserted at the top, because our scrollback
            // buffer isn't as large as the theoretical amount of scrollback
            // an unbounded terminal would have.
            let num_pulled_blank =
                usize::from(num_pulled) - num_pulled_scrollback;
            let pulled =
                core::iter::repeat_with(new_row)
                    .take(num_pulled_blank)
                    .chain(scrollback.rows.drain(
                        scrollback.rows.len() - num_pulled_scrollback..,
                    ));
            self.rows.splice(0..0, pulled);
            self.rows.resize_with(size.rows().into(), new_row);
            scrollback.unbounded_len -= usize::from(num_pulled);
            num_pulled
        } else {
            self.rows.resize_with(size.rows().into(), new_row);
            0
        };

        if let Some(row) = self.rows.last_mut() {
            // The last row is maintained to always be a valid row
            row.wrap(false);
        }
        // Reflow after the vertical resize. Alternate-screen grids deliberately
        // keep their fixed layout; only the normal screen has scrollback.
        let reflow = size.cols() != self.size.cols() && self.scrollback.is_some() && !is_empty;
        if !reflow {
            for row in &mut self.rows { row.resize(size.cols(), crate::Cell::new()); }
        }

        // Maintain the cursor position relative to the bottom-left corner of
        // the screen.
        if size.rows() < self.size.rows() {
            self.pos.row = self.pos.row.saturating_sub(pos_diff);
            self.saved_pos.row = self
                .saved_pos
                .row
                .saturating_sub(pos_diff)
                .min(size.rows() - 1);
        } else {
            self.pos.row += pos_diff;
            self.saved_pos.row += pos_diff;
        }

        if reflow { self.reflow(size); self.history_counters.2 += 1; }
        self.pos.col = self.pos.col.min(size.cols() - 1);
        self.saved_pos.col = self.saved_pos.col.min(size.cols() - 1);
        self.scroll_bottom = size.rows() - 1;
        self.size = size;

        if !is_empty {
            debug_assert_eq!(self.rows.len(), usize::from(self.size.rows()));
        }
        debug_assert!(self.pos.row < self.size.rows());
        debug_assert!(self.pos.col < self.size.cols());
        debug_assert!(self.saved_pos.row < self.size.rows());
        debug_assert!(self.saved_pos.col < self.size.cols());
    }

    /// Rewrap complete logical lines without losing their cells or attributes.
    /// The output is bounded by the configured history plus the visible rows,
    /// including when a very wide screen is resized to a single column.
    fn reflow(&mut self, size: Size) {
        use alloc::collections::VecDeque;
        let history = self.scrollback.as_mut().unwrap();
        let old_history = history.rows.len();
        let cursor_row = old_history + usize::from(self.pos.row);
        let saved_row = old_history + usize::from(self.saved_pos.row);
        let old_scrolled = history.unbounded_len;
        let limit = history.capacity.saturating_add(usize::from(size.rows()));
        let source = core::mem::take(&mut history.rows).into_iter().chain(core::mem::take(&mut self.rows));
        let mut out = VecDeque::new();
        let mut produced = 0usize;
        let mut row = crate::row::Row::new(size.cols());
        // `col` is the next cell of the new row, `used` tmux's width count for it: a tab
        // is one cell of its width followed by padding cells of one column each, so
        // grid_reflow puts the padding (which has no cells here) after the tab and moves
        // what follows on when it does not fit, as tmux 3.8 shows; the tab stays a tab
        let mut col = 0u16;
        let mut used = 0u16;
        let cols = size.cols();
        let mut cursor = (0usize, 0u16);
        let mut saved = (0usize, 0u16);
        let emit = |row: crate::row::Row, out: &mut VecDeque<crate::row::Row>, produced: &mut usize| {
            if out.len() == limit { out.pop_front(); }
            out.push_back(row);
            *produced += 1;
        };
        // grid_reflow splits the row a line starts on and joins the rows after it until
        // one does not fit, whose rest is then a line of its own (`line_row`); a split
        // whose first cell is wider than the row leaves an empty row before it
        let (mut line_row, mut split) = (0usize, false);
        macro_rules! wrap {
            () => {{
                row.wrap(true);
                emit(row, &mut out, &mut produced);
                row = crate::row::Row::new(cols);
                (col, used) = (0, 0);
            }};
        }
        for (index, mut old) in source.enumerate() {
            let wrapped = old.wrapped();
            if !wrapped { old.trim(); }
            let end = if wrapped { old.used_len() } else { old.cols() };
            // rewrapped rows keep the time their line entered the history
            if col == 0 && used == 0 { row.set_time(old.time()); row.set_flags(old.flags()); }
            let mut x = 0u16;
            while x < end {
                let Some(cell) = old.get(x) else { break };
                if cell.is_wide_continuation() { x += 1; continue; }
                let metadata = old.cell_metadata(x).cloned();
                let tab = metadata.as_ref().map_or(0, |m| m.tab_width);
                // columns taken (a tab wider than the row is kept, clipped to it), the
                // cells it has here and the padding counted after it
                // a wide cell in the last column has no continuation cell (its second
                // half was off the screen, screen_write_combine): it gets one here, and a
                // cursor after it keeps its offset, on the second half (grid_unwrap_position)
                let next = old.get(x + 1);
                let half = next.is_some_and(crate::Cell::is_wide_continuation);
                let (width, span, padding) = if tab > 0 { (tab, tab.min(end - x), tab - 1) } else if cell.is_wide() { (2, if next.is_some() && !half { 1 } else { 2 }, 0) } else { (1, 1, 0) };
                if used > 0 && used + width > cols {
                    if index != line_row {
                        // the rest of a row a join stopped in keeps its line (flags, time)
                        (line_row, split) = (index, false);
                        wrap!();
                        row.set_time(old.time());
                        row.set_flags(old.flags());
                    } else {
                        let first = !split;
                        split = true;
                        wrap!();
                        if first && width > cols { wrap!(); }
                    }
                }
                // a cursor inside a tab keeps its offset, on a wide character its first half
                let within = |c: u16| if tab > 0 { (col + c - x).min(cols - 1) } else if next.is_none() && c > x { col + 1 } else { col };
                let cover = if tab > 0 { span } else { width };
                if index == cursor_row && self.pos.col >= x && self.pos.col < x + cover { cursor = (produced, within(self.pos.col)); }
                if index == saved_row && self.saved_pos.col >= x && self.saved_pos.col < x + cover { saved = (produced, within(self.saved_pos.col)); }
                // A two-column glyph cannot fit in a one-column terminal.
                if tab > 0 || width <= cols {
                    for i in 0..width.min(cols - col) {
                        // a tab's cells cut off by an earlier, narrower row are its blanks again
                        let c = if i < span { old.get(x + i).cloned() } else { None }.filter(|c| tab > 0 || i == 0 || c.is_wide_continuation())
                            .unwrap_or_else(|| if tab > 0 { cell.clone() } else { let mut c = crate::Cell::new(); c.set_wide_continuation(true); c });
                        *row.get_mut_untracked(col + i).unwrap() = c;
                    }
                    if let Some(m) = metadata { row.set_metadata(col, m); }
                    col += width.min(cols - col);
                    used = used.saturating_add(width);
                }
                for _ in 0..padding {
                    if used + 1 > cols {
                        if index != line_row { (line_row, split) = (index, false); } else { split = true; }
                        wrap!();
                    }
                    used += 1;
                }
                x += span;
            }
            if index == cursor_row && self.pos.col >= x { cursor = (produced,col.saturating_add(self.pos.col-x).min(cols-1)); }
            if index == saved_row && self.saved_pos.col >= x { saved = (produced,col.saturating_add(self.saved_pos.col-x).min(cols-1)); }
            if !wrapped {
                emit(row, &mut out, &mut produced);
                row = crate::row::Row::new(cols);
                (col, used) = (0, 0);
                (line_row, split) = (index + 1, false);
            }
        }
        if col > 0 || used > 0 { emit(row, &mut out, &mut produced); }
        while out.len() < usize::from(size.rows()) {
            emit(crate::row::Row::new(size.cols()), &mut out, &mut produced);
        }
        let discarded = produced - out.len();
        let new_history = out.len() - usize::from(size.rows());
        self.rows = out.drain(new_history..).collect();
        history.rows = out;
        for row in &mut history.rows { row.compact(); }
        history.unbounded_len = old_scrolled.min(new_history);
        history.offset = 0;
        self.pos.row = cursor.0.saturating_sub(discarded + new_history).min(usize::from(size.rows()-1)) as u16;
        self.pos.col = cursor.1;
        self.saved_pos.row = saved.0.saturating_sub(discarded + new_history).min(usize::from(size.rows()-1)) as u16;
        self.saved_pos.col = saved.1;
    }

    pub fn history_counters(&self) -> (u64, u64, u64) {
        self.history_counters
    }

    pub fn now(&self) -> u64 {
        self.now
    }

    pub fn set_now(&mut self, now: u64) {
        self.now = now;
    }

    pub fn set_history_counters(&mut self, counters: (u64, u64, u64)) {
        self.history_counters = counters;
    }

    pub fn origin_mode(&self) -> bool {
        self.origin_mode
    }

    pub fn scroll_region(&self) -> (u16, u16) {
        (self.scroll_top, self.scroll_bottom)
    }

    pub fn pos(&self) -> Pos {
        self.pos
    }

    pub fn set_pos(&mut self, mut pos: Pos) {
        if self.origin_mode {
            pos.row = pos.row.saturating_add(self.scroll_top);
        }
        self.pos = pos;
        self.row_clamp_top(self.origin_mode);
        self.row_clamp_bottom(self.origin_mode);
        self.col_clamp();
    }

    /// Moves the cursor ignoring origin mode, kept on the screen.
    pub fn set_pos_abs(&mut self, pos: Pos) {
        self.pos = Pos {
            row: pos.row.min(self.size.rows() - 1),
            col: pos.col.min(self.size.cols() - 1),
        };
    }

    pub fn save_cursor(&mut self) {
        self.saved_pos = self.pos;
        self.saved_origin_mode = self.origin_mode;
    }

    pub fn restore_cursor(&mut self) {
        self.pos = self.saved_pos;
        self.origin_mode = self.saved_origin_mode;
    }

    pub fn visible_rows(&self) -> impl Iterator<Item = &crate::row::Row> {
        self.scrollback
            .as_ref()
            .map(|s| s.rows.iter().skip(s.rows.len() - s.offset))
            .into_iter()
            .flatten()
            .chain(&self.rows)
            .take(self.rows.len())
    }

    pub fn drawing_rows(&self) -> impl Iterator<Item = &crate::row::Row> {
        self.rows.iter()
    }

    pub fn drawing_rows_mut(
        &mut self,
    ) -> impl Iterator<Item = &mut crate::row::Row> {
        self.rows.iter_mut()
    }

    pub fn visible_row(&self, row: u16) -> Option<&crate::row::Row> {
        self.visible_rows().nth(usize::from(row))
    }

    pub fn drawing_row(&self, row: u16) -> Option<&crate::row::Row> {
        self.drawing_rows().nth(usize::from(row))
    }

    pub fn drawing_row_mut(
        &mut self,
        row: u16,
    ) -> Option<&mut crate::row::Row> {
        self.drawing_rows_mut().nth(usize::from(row))
    }

    pub fn current_row_mut(&mut self) -> &mut crate::row::Row {
        self.drawing_row_mut(self.pos.row)
            // we assume self.pos.row is always valid
            .unwrap()
    }

    pub fn visible_cell(&self, pos: Pos) -> Option<&crate::Cell> {
        self.visible_row(pos.row).and_then(|r| r.get(pos.col))
    }

    pub fn drawing_cell(&self, pos: Pos) -> Option<&crate::Cell> {
        self.drawing_row(pos.row).and_then(|r| r.get(pos.col))
    }

    pub fn drawing_cell_mut(&mut self, pos: Pos) -> Option<&mut crate::Cell> {
        self.drawing_row_mut(pos.row)
            .and_then(|r| r.get_mut(pos.col))
    }

    /// Number of rows from the oldest history row through the bottom of the screen.
    /// Row `index` counted from the oldest history row (0), ignoring the
    /// current scrollback view offset.
    pub fn absolute_row(&self, index: usize) -> Option<&crate::row::Row> {
        let history = self.scrollback_len();
        if index < history {
            self.scrollback.as_ref().and_then(|s| s.rows.get(index))
        } else {
            self.rows.get(index - history)
        }
    }

    /// Every history and screen row.
    pub fn for_each_row_mut(&mut self, f: impl FnMut(&mut crate::row::Row)) {
        let history = self.scrollback.iter_mut().flat_map(|s| s.rows.iter_mut());
        history.chain(self.rows.iter_mut()).for_each(f);
    }

    /// The history rows, taken (another grid's history goes in with
    /// [`Grid::set_history`]).
    pub fn take_history(&mut self) -> alloc::collections::VecDeque<crate::row::Row> {
        self.scrollback.as_mut().map(|s| { s.offset = 0; s.unbounded_len = 0; core::mem::take(&mut s.rows) }).unwrap_or_default()
    }

    pub fn set_history(&mut self, rows: alloc::collections::VecDeque<crate::row::Row>) {
        // as after a clear into the history (hscrolled 0): growing does not pull it back
        if let Some(s) = self.scrollback.as_mut() {
            s.unbounded_len = 0;
            s.rows = rows;
        }
    }

    pub fn scrollback_capacity(&self) -> usize {
        self.scrollback.as_ref().map_or(0, |s| s.capacity)
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.as_ref().map_or(0, |s| s.rows.len())
    }

    /// session_update_history: a new history-limit, with the history over it
    /// collected at once (grid_collect_history(gd, 1)). Like tmux a full history
    /// loses at least one line.
    pub fn set_scrollback_capacity(&mut self, limit: usize) {
        let Some(s) = self.scrollback.as_mut() else { return };
        s.capacity = limit;
        let len = s.rows.len();
        if len == 0 || len < limit {
            return;
        }
        let ny = (len - limit).max(1).min(len);
        s.rows.drain(..ny);
        self.history_counters.1 += ny as u64;
        s.offset = s.offset.min(s.rows.len());
        s.unbounded_len = s.unbounded_len.saturating_sub(ny);
    }

    pub fn scrollback(&self) -> usize {
        self.scrollback.as_ref().map_or(0, |s| s.offset)
    }

    /// Ensures every visible row of scrollback is at least the width of the
    /// screen.
    fn grow_visible_scrollback(scrollback: &mut ScrollbackState, size: Size) {
        let scrollback_len = scrollback.rows.len();
        scrollback
            .rows
            .iter_mut()
            .skip(scrollback_len - scrollback.offset)
            .take(size.rows().into())
            .for_each(|row| {
                row.grow(size.cols(), crate::cell::Cell::new());
            });
    }

    pub fn set_scrollback(&mut self, rows: usize) {
        let Some(scrollback) = &mut self.scrollback else {
            return;
        };
        scrollback.offset = rows.min(scrollback.rows.len());
        Self::grow_visible_scrollback(scrollback, self.size);
    }

    pub fn write_contents(&self, contents: &mut String) {
        let mut wrapping = false;
        for row in self.visible_rows() {
            row.write_contents(contents, 0, self.size.cols(), wrapping);
            if !row.wrapped() {
                contents.push('\n');
            }
            wrapping = row.wrapped();
        }

        while contents.ends_with('\n') {
            contents.truncate(contents.len() - 1);
        }
    }

    pub fn write_contents_formatted(
        &self,
        contents: &mut String,
    ) -> crate::attrs::Attrs {
        crate::term::ClearAttrs.write_buf(contents);
        crate::term::ClearScreen.write_buf(contents);

        let mut prev_attrs = crate::attrs::Attrs::default();
        let mut prev_pos = Pos::default();
        let mut wrapping = false;

        for (i, row) in self.visible_rows().enumerate() {
            // we limit the number of cols to a u16 (see Size), so
            // visible_rows() can never return more rows than will fit
            let i = i.try_into().unwrap();
            let (new_pos, new_attrs) = row.write_contents_formatted(
                contents,
                0,
                self.size.cols(),
                i,
                wrapping,
                Some(prev_pos),
                Some(prev_attrs),
            );
            prev_pos = new_pos;
            prev_attrs = new_attrs;
            wrapping = row.wrapped();
        }

        self.write_cursor_position_formatted(
            contents,
            Some(prev_pos),
            Some(prev_attrs),
        );

        prev_attrs
    }

    pub fn write_contents_formatted_basic(
        &self,
        writer: &mut impl alloc::fmt::Write,
        range: crate::capture::BasicFormattedCaptureRange<'_>,
    ) -> alloc::fmt::Result {
        fn write<'a>(
            rows: impl Iterator<Item = &'a crate::row::Row>,
            writer: &mut impl alloc::fmt::Write,
            state: &mut crate::capture::BasicFormattedCaptureState,
        ) -> alloc::fmt::Result {
            rows.map(crate::capture::RowContents)
                .try_for_each(|row| row.write_formatted_basic(writer, state))
        }
        match range {
            crate::capture::BasicFormattedCaptureRange::Full(state) => {
                if let Some(scrollback) = &self.scrollback {
                    write(scrollback.rows.iter(), writer, state)?;
                }
                write(self.rows.iter(), writer, state)
            }
            crate::capture::BasicFormattedCaptureRange::Visible => write(
                self.visible_rows(),
                writer,
                &mut crate::capture::BasicFormattedCaptureState::new(),
            ),
        }
    }

    pub fn write_contents_diff(
        &self,
        contents: &mut String,
        prev: &Self,
        mut prev_attrs: crate::attrs::Attrs,
    ) -> crate::attrs::Attrs {
        let mut prev_pos = prev.pos;
        let mut wrapping = false;
        let mut prev_wrapping = false;
        for (i, (row, prev_row)) in
            self.visible_rows().zip(prev.visible_rows()).enumerate()
        {
            // we limit the number of cols to a u16 (see Size), so
            // visible_rows() can never return more rows than will fit
            let i = i.try_into().unwrap();
            let (new_pos, new_attrs) = row.write_contents_diff(
                contents,
                prev_row,
                0,
                self.size.cols(),
                i,
                wrapping,
                prev_wrapping,
                prev_pos,
                prev_attrs,
            );
            prev_pos = new_pos;
            prev_attrs = new_attrs;
            wrapping = row.wrapped();
            prev_wrapping = prev_row.wrapped();
        }

        self.write_cursor_position_formatted(
            contents,
            Some(prev_pos),
            Some(prev_attrs),
        );

        prev_attrs
    }

    pub fn write_cursor_position_formatted(
        &self,
        contents: &mut String,
        prev_pos: Option<Pos>,
        prev_attrs: Option<crate::attrs::Attrs>,
    ) {
        let prev_attrs = prev_attrs.unwrap_or_default();
        // writing a character to the last column of a row doesn't wrap the
        // cursor immediately - it waits until the next character is actually
        // drawn. it is only possible for the cursor to have this kind of
        // position after drawing a character though, so if we end in this
        // position, we need to redraw the character at the end of the row.
        if prev_pos != Some(self.pos) && self.pos.col >= self.size.cols() {
            let mut pos = Pos {
                row: self.pos.row,
                col: self.size.cols() - 1,
            };
            if self
                .drawing_cell(pos)
                // we assume self.pos.row is always valid, and self.size.cols
                // - 1 is always a valid column
                .unwrap()
                .is_wide_continuation()
            {
                pos.col = self.size.cols() - 2;
            }
            let cell =
                // we assume self.pos.row is always valid, and self.size.cols
                // - 2 must be a valid column because self.size.cols - 1 is
                // always valid and we just checked that the cell at
                // self.size.cols - 1 is a wide continuation character, which
                // means that the first half of the wide character must be
                // before it
                self.drawing_cell(pos).unwrap();
            if cell.has_contents() {
                if let Some(prev_pos) = prev_pos {
                    crate::term::MoveFromTo::new(prev_pos, pos)
                        .write_buf(contents);
                } else {
                    crate::term::MoveTo::new(pos).write_buf(contents);
                }
                cell.attrs().write_escape_code_diff(contents, &prev_attrs);
                contents.push_str(cell.contents());
                prev_attrs.write_escape_code_diff(contents, cell.attrs());
            } else {
                // if the cell doesn't have contents, we can't have gotten
                // here by drawing a character in the last column. this means
                // that as far as i'm aware, we have to have reached here from
                // a newline when we were already after the end of an earlier
                // row. in the case where we are already after the end of an
                // earlier row, we can just write a few newlines, otherwise we
                // also need to do the same as above to get ourselves to after
                // the end of a row.
                let mut found = false;
                for i in (0..self.pos.row).rev() {
                    pos.row = i;
                    pos.col = self.size.cols() - 1;
                    if self
                        .drawing_cell(pos)
                        // i is always less than self.pos.row, which we assume
                        // to be always valid, so it must also be valid.
                        // self.size.cols - 1 is always a valid col.
                        .unwrap()
                        .is_wide_continuation()
                    {
                        pos.col = self.size.cols() - 2;
                    }
                    let cell = self
                        .drawing_cell(pos)
                        // i is always less than self.pos.row, which we assume
                        // to be always valid, so it must also be valid.
                        // self.size.cols - 2 is valid because self.size.cols
                        // - 1 is always valid, and col gets set to
                        // self.size.cols - 2 when the cell at self.size.cols
                        // - 1 is a wide continuation character, meaning that
                        // the first half of the wide character must be before
                        // it
                        .unwrap();
                    if cell.has_contents() {
                        if let Some(prev_pos) = prev_pos {
                            if prev_pos.row != i
                                || prev_pos.col < self.size.cols()
                            {
                                crate::term::MoveFromTo::new(prev_pos, pos)
                                    .write_buf(contents);
                                cell.attrs().write_escape_code_diff(
                                    contents,
                                    &prev_attrs,
                                );
                                contents.push_str(cell.contents());
                                prev_attrs.write_escape_code_diff(
                                    contents,
                                    cell.attrs(),
                                );
                            }
                        } else {
                            crate::term::MoveTo::new(pos).write_buf(contents);
                            cell.attrs().write_escape_code_diff(
                                contents,
                                &prev_attrs,
                            );
                            contents.push_str(cell.contents());
                            prev_attrs.write_escape_code_diff(
                                contents,
                                cell.attrs(),
                            );
                        }
                        contents.push_str(
                            &"\n".repeat(usize::from(self.pos.row - i)),
                        );
                        found = true;
                        break;
                    }
                }

                // this can happen if you get the cursor off the end of a row,
                // and then do something to clear the end of the current row
                // without moving the cursor (IL, DL, ED, EL, etc). we know
                // there can't be something in the last column because we
                // would have caught that above, so it should be safe to
                // overwrite it.
                if !found {
                    pos = Pos {
                        row: self.pos.row,
                        col: self.size.cols() - 1,
                    };
                    if let Some(prev_pos) = prev_pos {
                        crate::term::MoveFromTo::new(prev_pos, pos)
                            .write_buf(contents);
                    } else {
                        crate::term::MoveTo::new(pos).write_buf(contents);
                    }
                    contents.push(' ');
                    // we know that the cell has no contents, but it still may
                    // have drawing attributes (background color, etc)
                    let end_cell = self
                        .drawing_cell(pos)
                        // we assume self.pos.row is always valid, and
                        // self.size.cols - 1 is always a valid column
                        .unwrap();
                    end_cell
                        .attrs()
                        .write_escape_code_diff(contents, &prev_attrs);
                    crate::term::SaveCursor.write_buf(contents);
                    crate::term::Backspace.write_buf(contents);
                    crate::term::EraseChar::new(1).write_buf(contents);
                    crate::term::RestoreCursor.write_buf(contents);
                    prev_attrs
                        .write_escape_code_diff(contents, end_cell.attrs());
                }
            }
        } else if let Some(prev_pos) = prev_pos {
            crate::term::MoveFromTo::new(prev_pos, self.pos)
                .write_buf(contents);
        } else {
            crate::term::MoveTo::new(self.pos).write_buf(contents);
        }
    }

    pub fn erase_all_into_history<F>(&mut self, attrs: crate::attrs::Attrs, on_row: F)
    where F: FnMut(&crate::row::Row) {
        if self.scrollback.is_some() {
            let last = self.rows.iter().rposition(|r| (0..self.size.cols()).any(|x|
                r.get(x).is_some_and(|c| *c != crate::cell::Cell::DEFAULT))).map_or(0, |i| i+1);
            let region = (self.scroll_top,self.scroll_bottom);
            self.scroll_top = 0;
            self.scroll_bottom = self.size.rows()-1;
            self.scroll_up(last as u16,on_row);
            (self.scroll_top,self.scroll_bottom) = region;
            if let Some(h) = self.scrollback.as_mut() { h.unbounded_len = 0; }
        }
        self.erase_all(attrs);
    }

    pub fn erase_all(&mut self, attrs: crate::attrs::Attrs) {
        for row in self.drawing_rows_mut() {
            row.clear(attrs);
        }
    }

    pub fn erase_all_forward(&mut self, attrs: crate::attrs::Attrs) {
        let pos = self.pos;
        for row in self.drawing_rows_mut().skip(usize::from(pos.row) + 1) {
            row.clear(attrs);
        }

        if pos.col == 0 {
            // grid_clear_lines: from the line's start the whole line goes, marks too
            self.clear_line(pos.row, attrs);
        } else {
            self.erase_row_forward(attrs);
        }
    }

    pub fn erase_all_backward(&mut self, attrs: crate::attrs::Attrs) {
        let pos = self.pos;
        for row in self.drawing_rows_mut().take(usize::from(pos.row)) {
            row.clear(attrs);
        }
        if pos.row > 0 {
            self.unwrap_above(0);
        }

        if pos.col >= self.size.cols() - 1 {
            self.clear_line(pos.row, attrs);
        } else {
            self.erase_row_backward(attrs);
        }
    }

    /// grid_clear_lines on one screen line: its cells and every line flag go and the
    /// line above no longer wraps onto it.
    pub fn clear_line(&mut self, row: u16, attrs: crate::attrs::Attrs) {
        if let Some(r) = self.drawing_row_mut(row) {
            r.clear(attrs);
        }
        self.unwrap_above(row);
    }

    /// The line above screen line `row` (the last history line above the first) ends
    /// there (grid_clear_lines clears its GRID_LINE_WRAPPED).
    pub fn unwrap_above(&mut self, row: u16) {
        if row > 0 {
            if let Some(r) = self.drawing_row_mut(row - 1) {
                r.wrap(false);
            }
        } else if let Some(r) = self.scrollback.as_mut().and_then(|s| s.rows.back_mut()) {
            r.wrap(false);
        }
    }

    pub fn erase_all_scrollback<F>(&mut self, mut on_row: F)
    where
        F: FnMut(&crate::row::Row),
    {
        if let Some(scrollback) = &mut self.scrollback {
            for row in scrollback.rows.drain(..) {
                on_row(&row);
            }
            scrollback.unbounded_len = 0;
            scrollback.offset = 0;
            self.history_counters.2 += 1;
        }
    }

    pub fn erase_row(&mut self, attrs: crate::attrs::Attrs) {
        // screen_write_clearline: the line's OSC 133 marks survive; a line never written
        // (no cells, tmux's cellsize 0) with the default background is left alone, the
        // wrap of the line above included
        let row = self.current_row_mut();
        if row.used_len() == 0 && row.metadata().next().is_none() && attrs.bgcolor == crate::Color::Default {
            return;
        }
        let flags = row.flags() & !(crate::ROW_HYPERLINK | crate::ROW_EXTENDED);
        row.clear(attrs);
        row.set_flags(flags);
        self.unwrap_above(self.pos.row);
    }

    pub fn erase_row_forward(&mut self, attrs: crate::attrs::Attrs) {
        let size = self.size;
        let pos = self.pos;
        // screen_write_clearendofline: from the start it is the whole line
        if pos.col == 0 {
            self.erase_row(attrs);
            return;
        }
        let row = self.current_row_mut();
        for col in pos.col..size.cols() {
            row.erase(col, attrs);
        }
    }

    pub fn erase_row_backward(&mut self, attrs: crate::attrs::Attrs) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for col in 0..=pos.col.min(size.cols() - 1) {
            row.erase(col, attrs);
        }
    }

    pub fn insert_cells(&mut self, count: u16) {
        let pos = self.pos;
        let row = self.current_row_mut();
        row.insert(pos.col, count);
    }

    pub fn delete_cells(&mut self, count: u16) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        row.remove(pos.col..pos.col.saturating_add(count).min(size.cols()));
    }

    pub fn erase_cells(&mut self, count: u16, attrs: crate::attrs::Attrs) {
        let size = self.size;
        let pos = self.pos;
        // ECH over the whole line is grid_clear_lines
        if pos.col == 0 && count.max(1) >= size.cols() {
            self.clear_line(pos.row, attrs);
            return;
        }
        let row = self.current_row_mut();
        for col in pos.col..((pos.col.saturating_add(count)).min(size.cols()))
        {
            row.erase(col, attrs);
        }
    }

    pub fn insert_lines(&mut self, count: u16) {
        if count == 0 {
            return;
        }
        if !(self.scroll_top..=self.scroll_bottom).contains(&self.pos.row) {
            return;
        }
        let between = &mut self.rows
            [usize::from(self.pos.row)..=usize::from(self.scroll_bottom)];
        let num_between = between.len();
        let num_removed = usize::from(count).min(num_between);
        between.rotate_right(num_removed);
        for row in &mut between[..num_removed] {
            row.reset();
        }
        // self.scroll_bottom is maintained to always be a valid row
        self.rows[usize::from(self.scroll_bottom)].wrap(false);
    }

    pub fn delete_lines(&mut self, count: u16) {
        if !(self.scroll_top..=self.scroll_bottom).contains(&self.pos.row) {
            return;
        }
        let between = &mut self.rows
            [usize::from(self.pos.row)..=usize::from(self.scroll_bottom)];
        let num_between = between.len();
        let num_removed = usize::from(count).min(num_between);
        for row in &mut between[..num_removed] {
            row.reset();
        }
        between.rotate_left(num_removed);
    }

    pub fn scroll_up<F>(&mut self, count: u16, on_row: F)
    where
        F: FnMut(&crate::row::Row),
    {
        self.scroll_up_stamped(count, on_row, true);
    }

    /// Scrolls up; `stamp` gives the rows entering the history the current time
    /// (grid_scroll_history). Shrinking the screen moves rows into the history
    /// without one (screen_resize_y only grows hsize).
    fn scroll_up_stamped<F>(&mut self, count: u16, mut on_row: F, stamp: bool)
    where
        F: FnMut(&crate::row::Row),
    {
        // This can be true even when `self.scrollback` is `None`; it controls
        // whether we call the `on_row` callback. With a history the top line of any
        // scroll region goes into it (grid_scroll_history_region).
        let scrollback_enabled = self.scroll_top == 0 || self.scrollback.is_some();
        let between = &mut self.rows
            [usize::from(self.scroll_top)..=usize::from(self.scroll_bottom)];
        let num_between = between.len();
        let count = usize::from(count).min(num_between);

        // Number of to-be-removed rows.
        let num_removed = count.min(num_between);
        let new_row = || crate::row::Row::new(self.size.cols());
        let now = self.now;

        if let Some(scrollback) =
            self.scrollback.as_mut().filter(|_| scrollback_enabled)
        {
            scrollback.unbounded_len =
                scrollback.unbounded_len.saturating_add(num_removed);

            // Number of rows that will be pushed to the scrollback.
            let num_pushed = count.min(scrollback.capacity);

            // grid_collect_history(gd, 0) before each line enters it: a full
            // history loses a tenth of its limit (at least one line) at once.
            let cap = scrollback.capacity;
            let (mut len, mut scrollback_overflow) = (scrollback.rows.len(), 0);
            for _ in 0..num_pushed {
                if len > 0 && len >= cap {
                    let ny = (cap / 10).max(1).min(len);
                    len -= ny;
                    scrollback_overflow += ny;
                }
                len += 1;
            }
            let scrollback_overflow = scrollback_overflow.min(scrollback.rows.len());
            for row in scrollback.rows.drain(0..scrollback_overflow) {
                on_row(&row);
            }
            self.history_counters.0 += num_pushed as u64;
            self.history_counters.1 += scrollback_overflow as u64;

            // Number of rows that will simply be reset rather than pushed to
            // the scrollback.
            let num_reset = num_removed - num_pushed;

            for row in &mut between[..num_reset] {
                on_row(row);
                row.reset();
            }

            scrollback.rows.extend(
                between[num_reset..num_removed].iter_mut().map(|row| {
                    row.trim();
                    row.compact();
                    if stamp { row.set_time(now); }
                    core::mem::replace(row, new_row())
                }),
            );

            if scrollback.offset > 0 {
                scrollback.offset = scrollback
                    .offset
                    .saturating_add(count)
                    .min(scrollback.rows.len());
            }
            Self::grow_visible_scrollback(scrollback, self.size);
        } else {
            for row in &mut between[..num_removed] {
                if scrollback_enabled {
                    on_row(row);
                }
                row.reset();
            }
        }

        // Finally, rotate the rows so the "removed" ones (which have now been
        // reset to empty) are at the bottom.
        between.rotate_left(num_removed);
    }

    pub fn scroll_down(&mut self, count: u16) {
        if count == 0 {
            return;
        }
        let between = &mut self.rows
            [usize::from(self.scroll_top)..=usize::from(self.scroll_bottom)];
        let num_between = between.len();
        let num_removed = usize::from(count).min(num_between);
        between.rotate_right(num_removed);
        for row in &mut between[..num_removed] {
            row.reset();
        }
        // self.scroll_bottom is maintained to always be a valid row
        self.rows[usize::from(self.scroll_bottom)].wrap(false);
    }

    /// DECALN (screen_write_alignmenttest): E in every cell with default attributes, the
    /// cursor home and the scroll region reset.
    pub fn alignment_test(&mut self) {
        let cols = self.size.cols();
        for row in self.drawing_rows_mut() {
            row.clear(crate::attrs::Attrs::default());
            for col in 0..cols {
                if let Some(cell) = row.get_mut(col) {
                    cell.set('E', crate::attrs::Attrs::default());
                }
            }
        }
        self.scroll_top = 0;
        self.scroll_bottom = self.size.rows() - 1;
        self.pos = Pos::default();
    }

    pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let bottom = bottom.min(self.size().rows() - 1);
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
        } else {
            self.scroll_top = 0;
            self.scroll_bottom = self.size().rows() - 1;
        }
        self.pos.row = self.scroll_top;
        self.pos.col = 0;
    }

    fn in_scroll_region(&self) -> bool {
        self.pos.row >= self.scroll_top && self.pos.row <= self.scroll_bottom
    }

    pub fn set_origin_mode(&mut self, mode: bool) {
        self.origin_mode = mode;
        self.set_pos(Pos { row: 0, col: 0 });
    }

    pub fn row_inc_clamp(&mut self, count: u16) {
        let in_scroll_region = self.in_scroll_region();
        self.pos.row = self.pos.row.saturating_add(count);
        self.row_clamp_bottom(in_scroll_region);
    }

    pub fn row_inc_scroll<F>(&mut self, count: u16, on_row: F) -> u16
    where
        F: FnMut(&crate::row::Row),
    {
        let in_scroll_region = self.in_scroll_region();
        self.pos.row = self.pos.row.saturating_add(count);
        let lines = self.row_clamp_bottom(in_scroll_region);
        if in_scroll_region {
            self.scroll_up(lines, on_row);
            lines
        } else {
            0
        }
    }

    pub fn row_dec_clamp(&mut self, count: u16) {
        let in_scroll_region = self.in_scroll_region();
        self.pos.row = self.pos.row.saturating_sub(count);
        self.row_clamp_top(in_scroll_region);
    }

    pub fn row_dec_scroll(&mut self, count: u16) {
        let in_scroll_region = self.in_scroll_region();
        // need to account for clamping by both row_clamp_top and by
        // saturating_sub
        let extra_lines = count.saturating_sub(self.pos.row);
        self.pos.row = self.pos.row.saturating_sub(count);
        let lines = self.row_clamp_top(in_scroll_region);
        self.scroll_down(lines + extra_lines);
    }

    pub fn row_set(&mut self, i: u16) {
        self.pos.row = i;
        self.row_clamp();
    }

    pub fn col_inc(&mut self, count: u16) {
        self.pos.col = self.pos.col.saturating_add(count);
    }

    pub fn col_inc_clamp(&mut self, count: u16) {
        self.pos.col = self.pos.col.saturating_add(count);
        self.col_clamp();
    }

    pub fn col_dec(&mut self, count: u16) {
        self.pos.col = self.pos.col.saturating_sub(count);
    }

    pub fn col_set(&mut self, i: u16) {
        self.pos.col = i;
        self.col_clamp();
    }

    pub fn col_wrap<F>(&mut self, width: u16, wrap: bool, on_row: F)
    where
        F: FnMut(&crate::row::Row),
    {
        if self.pos.col <= self.size.cols() - width {
            // we haven't reached the end of the row yet
            return;
        }

        let mut prev_pos = self.pos;
        self.pos.col = 0;
        let scrolled = self.row_inc_scroll(1, on_row);
        if prev_pos.row == 0 && scrolled > 0 {
            // we're trying to scroll but we're already at the top of the
            // grid - this can happen when the grid height is set to 1. in
            // this case, we want to do the scrolling (via row_inc_scroll)
            // but don't want to set the wrap state (since it never makes
            // sense on a 1-height grid).
            return;
        }

        prev_pos.row -= scrolled;
        let new_pos = self.pos;
        self.drawing_row_mut(prev_pos.row)
            // we assume self.pos.row is always valid, and so prev_pos.row
            // must be valid because it is always less than or equal to
            // self.pos.row
            .unwrap()
            .wrap(wrap && prev_pos.row + 1 == new_pos.row);
    }

    fn row_clamp_top(&mut self, limit_to_scroll_region: bool) -> u16 {
        if limit_to_scroll_region && self.pos.row < self.scroll_top {
            let rows = self.scroll_top - self.pos.row;
            self.pos.row = self.scroll_top;
            rows
        } else {
            0
        }
    }

    fn row_clamp_bottom(&mut self, limit_to_scroll_region: bool) -> u16 {
        let bottom = if limit_to_scroll_region {
            self.scroll_bottom
        } else {
            self.size.rows() - 1
        };
        if self.pos.row > bottom {
            let rows = self.pos.row - bottom;
            self.pos.row = bottom;
            rows
        } else {
            0
        }
    }

    fn row_clamp(&mut self) {
        if self.pos.row > self.size.rows() - 1 {
            self.pos.row = self.size.rows() - 1;
        }
    }

    fn col_clamp(&mut self) {
        if self.pos.col > self.size.cols() - 1 {
            self.pos.col = self.size.cols() - 1;
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Size {
    pub rows: core::num::NonZeroU16,
    pub cols: core::num::NonZeroU16,
}

impl Size {
    pub fn rows(self) -> u16 {
        self.rows.get()
    }

    pub fn cols(self) -> u16 {
        self.cols.get()
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Pos {
    pub row: u16,
    pub col: u16,
}
