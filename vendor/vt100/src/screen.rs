use crate::term::BufWrite as _;
use alloc::{string::String, vec::Vec};
use unicode_width::UnicodeWidthChar as _;

const MODE_APPLICATION_KEYPAD: u8 = 0b0000_0001;
const MODE_APPLICATION_CURSOR: u8 = 0b0000_0010;
const MODE_HIDE_CURSOR: u8 = 0b0000_0100;
const MODE_ALTERNATE_SCREEN: u8 = 0b0000_1000;
const MODE_BRACKETED_PASTE: u8 = 0b0001_0000;

// Modes kept only so they can be reported (tmux's pane_private_modes and the
// *_flag format variables); the parser does not change behaviour for them.
const XMODE_INSERT: u16 = 1 << 0;
const CHARSET_G0: u8 = 1;
const CHARSET_G1: u8 = 2;
const CHARSET_SHIFT: u8 = 4;
const XMODE_WRAP: u16 = 1 << 1;
const XMODE_CURSOR_BLINKING: u16 = 1 << 2;
const XMODE_CURSOR_BLINKING_SET: u16 = 1 << 3;
const XMODE_CURSOR_VERY_VISIBLE: u16 = 1 << 4;
const XMODE_FOCUS: u16 = 1 << 5;
const XMODE_MOUSE_UTF8: u16 = 1 << 6;
const XMODE_MOUSE_SGR: u16 = 1 << 7;
const XMODE_SYNC: u16 = 1 << 8;
const XMODE_THEME_UPDATES: u16 = 1 << 9;

/// The xterm mouse handling mode currently in use.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum MouseProtocolMode {
    /// Mouse handling is disabled.
    #[default]
    None,

    /// Mouse button events should be reported on button press. Also known as
    /// X10 mouse mode.
    Press,

    /// Mouse button events should be reported on button press and release.
    /// Also known as VT200 mouse mode.
    PressRelease,

    // Highlight,
    /// Mouse button events should be reported on button press and release, as
    /// well as when the mouse moves between cells while a button is held
    /// down.
    ButtonMotion,

    /// Mouse button events should be reported on button press and release,
    /// and mouse motion events should be reported when the mouse moves
    /// between cells regardless of whether a button is held down or not.
    AnyMotion,
    // DecLocator,
}

/// The encoding to use for the enabled [`MouseProtocolMode`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum MouseProtocolEncoding {
    /// Default single-printable-byte encoding.
    #[default]
    Default,

    /// UTF-8-based encoding.
    Utf8,

    /// SGR-like encoding.
    Sgr,
    // Urxvt,
}

/// The cursor's visual style.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum CursorStyle {
    /// The cursor should match the terminal's default.
    #[default]
    Default,

    /// The cursor should be a blinking block.
    BlinkBlock,

    /// The cursor should be a steady block.
    SteadyBlock,

    /// The cursor should be a blinking underline.
    BlinkUnderline,

    /// The cursor should be a steady underline.
    SteadyUnderline,

    /// The cursor should be a blinking vertical bar.
    BlinkVerticalBar,

    /// The cursor should be a steady vertical bar.
    SteadyVerticalBar,
}

/// Represents the overall terminal state.
#[derive(Clone, Debug)]
pub struct Screen {
    grid: crate::grid::Grid,
    alternate_grid: crate::grid::Grid,

    attrs: crate::attrs::Attrs,
    saved_attrs: crate::attrs::Attrs,
    /// Character sets (tmux cell.g0set, g1set and set): CHARSET_G0 and CHARSET_G1 when
    /// that set is DEC special graphics (ESC ( 0, ESC ) 0), CHARSET_SHIFT after SO.
    charset: u8,
    saved_charset: u8,

    modes: u8,
    xmodes: u16,
    mouse_protocol_mode: MouseProtocolMode,
    mouse_protocol_encoding: MouseProtocolEncoding,
    cursor_style: CursorStyle,

    last_char: Option<char>,
    scroll_on_clear: bool,
    allow_alternate: bool,
    widths: Vec<(u32, u32, u16)>,
    variation_wide: bool,
    /// The run of printable ASCII being written on the cursor's row
    /// (screen_write_collect_add): the column after it, 0 for none. tmux writes a run
    /// with one grid_set_cells when it ends, so the line grows to the run's end in one
    /// step (a run ending at a quarter or half of the width gets the next size, one cell
    /// at a time would not); anything but a printable character, a wrap and the end of
    /// what was read end the run (input_parse), and `end_print_run` grows the line then.
    /// A character only notes the run's end and the row's size (`print_run_size`), so
    /// the line is looked up again only when it grows.
    print_run: u16,
    /// The row's cellsize while the run is written (the run itself does not grow it
    /// until it ends).
    print_run_size: u16,
    /// The row's cellsize before the run, if something in the run grew it (a wide
    /// character's second half overwritten); otherwise it is `print_run_size`.
    print_run_base: Option<u16>,

    /// Cursor saved by entering the alternate screen with DECSET 1049 (tmux's
    /// saved_cx/saved_cy), as (row, col).
    alternate_saved: Option<(u16, u16)>,
    alternate_saved_attrs: crate::attrs::Attrs,
    /// Tab stops when changed by HTS/TBC; None means one every 8 columns.
    tabs: Option<Vec<bool>>,
    /// The OSC 8 link of new text, the one DECSC saved, and named links by
    /// (id, URI) so the same pair gets the same link.
    hyperlink: Option<alloc::sync::Arc<crate::Hyperlink>>,
    saved_hyperlink: Option<alloc::sync::Arc<crate::Hyperlink>>,
    hyperlinks: alloc::collections::BTreeMap<(String, String), alloc::sync::Weak<crate::Hyperlink>>,
    /// Table size at which dead entries are swept next.
    pub(crate) hyperlinks_sweep: usize,
    /// The SGR being dispatched had no parameters at all (`CSI m`), which vte reports as
    /// a 0: tmux resets the link too only for that one.
    pub(crate) empty_sgr: bool,

}

impl Screen {
    pub(crate) fn new(
        size: crate::grid::Size,
        scrollback_capacity: usize,
    ) -> Self {
        let mut grid = crate::grid::Grid::new(
            size,
            crate::grid::Scrollback::Enabled {
                capacity: scrollback_capacity,
            },
        );
        grid.allocate_rows();
        Self {
            grid,
            alternate_grid: crate::grid::Grid::new(
                size,
                crate::grid::Scrollback::Disabled,
            ),

            attrs: crate::attrs::Attrs::default(),
            saved_attrs: crate::attrs::Attrs::default(),
            charset: 0,
            saved_charset: 0,

            modes: 0,
            xmodes: XMODE_WRAP,
            mouse_protocol_mode: MouseProtocolMode::default(),
            mouse_protocol_encoding: MouseProtocolEncoding::default(),
            cursor_style: CursorStyle::default(),

            last_char: None,
            scroll_on_clear: false,
            allow_alternate: true, widths: Vec::new(), variation_wide: false,
            print_run: 0,
            print_run_size: 0,
            print_run_base: None,

            alternate_saved: None,
            alternate_saved_attrs: crate::attrs::Attrs::default(),
            tabs: None, hyperlink: None, saved_hyperlink: None, hyperlinks: Default::default(), hyperlinks_sweep: 5000, empty_sgr: false,
        }
    }

    /// Preserve cleared normal-screen rows in history, as tmux does.
    /// Pane policy for DEC alternate-screen modes.
    pub fn set_alternate_screen_allowed(&mut self, enabled: bool) { self.allow_alternate = enabled; }

    /// Ends the run of printable characters being written (see `print_run`).
    #[inline]
    pub(crate) fn end_print_run(&mut self) {
        if self.print_run != 0 {
            self.grow_print_run();
        }
    }

    /// grid_set_cells over the run that ended: the line has room for it, in one
    /// grid_expand_line from its size before the run.
    fn grow_print_run(&mut self) {
        let end = core::mem::take(&mut self.print_run);
        let size = core::mem::take(&mut self.print_run_size);
        if end <= self.print_run_base.take().unwrap_or(size) {
            return;
        }
        let cols = self.grid().size().cols();
        let row = self.grid_mut().current_row_mut();
        let size = row.cellsize();
        row.set_cellsize(crate::row::expanded_cellsize(end, cols).max(size));
    }

    /// Width ranges are applied in order; the last matching entry wins.
    pub fn set_codepoint_widths(&mut self, widths: Vec<(u32, u32, u16)>, variation_wide: bool) {
        self.widths = widths; self.variation_wide = variation_wide;
    }

    /// Scroll existing contents into history when clearing the screen.
    pub fn set_scroll_on_clear(&mut self, enabled: bool) { self.scroll_on_clear = enabled; }

    /// The current time (seconds since the epoch) recorded on rows as they
    /// enter the history; 0 records no time.
    pub fn set_time(&mut self, now: u64) {
        self.grid.set_now(now);
        self.alternate_grid.set_now(now);
    }

    /// Marks the cursor's line (OSC 133 prompt and output starts).
    pub fn mark_cursor_row(&mut self, flag: u8) {
        let row = self.grid_mut().current_row_mut();
        row.set_flags(row.flags() | flag);
    }

    /// The marks of row `index` (counted as for [`Screen::history_row`]) and
    /// whether it holds extended cells.
    #[must_use]
    pub fn history_row_flags(&self, index: usize) -> (u8, bool) {
        self.base_row(index).map_or((0, false), |r| (r.flags(), r.extended()))
    }

    /// When row `index` (counted as for [`Screen::history_row`]) entered the
    /// history, 0 if it has not or no time was set.
    #[must_use]
    pub fn history_row_time(&self, index: usize) -> u64 {
        self.base_row(index).map_or(0, crate::row::Row::time)
    }

    /// Row `index` of tmux's base grid: the normal screen's history, then the
    /// active screen's rows (the alternate screen keeps the history above it).
    fn base_row(&self, index: usize) -> Option<&crate::row::Row> {
        let history = self.grid.scrollback_len();
        if index < history {
            self.grid.absolute_row(index)
        } else {
            let g = self.grid();
            g.absolute_row(g.scrollback_len() + index - history)
        }
    }

    /// Apply OSC 8 (input_osc_8, hyperlinks_put), retaining only links referenced by
    /// live/history cells. The URI and id are stored vis-encoded (utf8_stravis with
    /// VIS_OCTAL|VIS_CSTYLE) and a URI over 1024 bytes once encoded is no link.
    pub fn set_hyperlink(&mut self, params: &[u8], uri: &[u8]) {
        let ids: Vec<_> = params.split(|&b| b == b':').filter_map(|p| p.strip_prefix(b"id=")).filter(|s| !s.is_empty()).collect();
        if ids.len() > 1 { return; }
        if uri.is_empty() { self.hyperlink = None; return; }
        let uri = stravis(uri);
        if uri.len() > 1024 { self.hyperlink = None; return; }
        let id = ids.first().map_or_else(String::new, |id| stravis(id));
        let key = (id, uri);
        if !key.0.is_empty() {
            if let Some(link) = self.hyperlinks.get(&key).and_then(alloc::sync::Weak::upgrade) {
                self.hyperlink = Some(link); return;
            }
        }
        // dead entries go once the table has doubled since the last sweep, so a long run
        // of live named links costs amortised O(log n) each, not a full scan per link
        if self.hyperlinks.len() >= self.hyperlinks_sweep {
            self.hyperlinks.retain(|_, p| p.strong_count() != 0);
            self.hyperlinks_sweep = (self.hyperlinks.len() * 2).max(5000);
        }
        static NEXT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);
        let inner = NEXT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let (id, uri) = key;
        let link = alloc::sync::Arc::new(crate::Hyperlink { inner, id, external_id: alloc::format!("tmux{:X}", inner), uri });
        if !link.id.is_empty() { self.hyperlinks.insert((link.id.clone(), link.uri.clone()), alloc::sync::Arc::downgrade(&link)); }
        self.hyperlink = Some(link);
    }

    /// screen_reset_hyperlinks (clear-history -H): every cell loses its link and
    /// the current one is forgotten.
    pub fn reset_hyperlinks(&mut self) {
        self.hyperlink = None;
        self.saved_hyperlink = None;
        self.hyperlinks.clear();
        self.grid.for_each_row_mut(crate::row::Row::clear_hyperlinks);
        self.alternate_grid.for_each_row_mut(crate::row::Row::clear_hyperlinks);
    }

    /// Resizes the screen **without calling** the [`on_scroll`] callback.
    ///
    /// This method behaves the same as [`Parser::set_size`] but does not call
    /// the [`on_scroll`] callback. If you want that, use [`Parser::set_size`].
    ///
    /// [`on_scroll`]: crate::Callbacks::on_scroll
    /// [`Parser::set_size`]: crate::Parser::set_size
    pub fn set_size(
        &mut self,
        rows: core::num::NonZeroU16,
        cols: core::num::NonZeroU16,
    ) {
        self.reset_tabs_for(cols.get());
        self.grid.set_size(crate::grid::Size { rows, cols }, |_| {});
        self.alternate_grid
            .set_size(crate::grid::Size { rows, cols }, |_| {});
    }

    // tmux resets the tab stops when the width changes
    fn reset_tabs_for(&mut self, cols: u16) {
        if cols != self.grid.size().cols() {
            self.tabs = None;
        }
    }

    /// Returns the current size of the terminal.
    ///
    /// The return value will be (rows, cols).
    #[must_use]
    pub fn size(&self) -> (core::num::NonZeroU16, core::num::NonZeroU16) {
        let size = self.grid().size();
        (size.rows, size.cols)
    }

    /// Returns the number of lines in the scrollback: the normal screen's,
    /// also while the alternate screen is active (tmux keeps the history in
    /// the grid the alternate screen is drawn in).
    #[must_use]
    pub fn scrollback_len(&self) -> usize {
        self.grid.scrollback_len()
    }

    /// Returns the maximum capacity of the scrollback.
    #[must_use]
    pub fn scrollback_capacity(&self) -> usize {
        self.grid().scrollback_capacity()
    }

    /// Scrolls to the given position in the scrollback.
    ///
    /// This position indicates the offset from the top of the screen, and
    /// should be `0` to put the normal screen in view.
    ///
    /// This affects the return values of methods called on the screen: for
    /// instance, `screen.cell(0, 0)` will return the top left corner of the
    /// screen after taking the scrollback offset into account.
    ///
    /// The value given will be clamped to the actual size of the scrollback.
    pub fn set_scrollback(&mut self, rows: usize) {
        self.grid_mut().set_scrollback(rows);
    }

    /// Clears the scrollback history for the active screen and returns the
    /// scrollback position to the live viewport.
    ///
    /// This does not erase the visible screen contents, reset terminal
    /// modes, or call the `on_row` callback. To model a terminal action that
    /// clears both the visible display and scrollback history, process both
    /// `CSI 2 J` and `CSI 3 J`.
    pub fn clear_scrollback(&mut self) {
        // grid_clear_history(wp->base.grid): the normal screen's history also
        // while the alternate screen is active
        self.grid.erase_all_scrollback(|_| {});
        self.alternate_grid.erase_all_scrollback(|_| {});
    }

    /// Returns the current position in the scrollback.
    ///
    /// This position indicates the offset from the top of the screen, and is
    /// `0` when the normal screen is in view.
    #[must_use]
    pub fn scrollback(&self) -> usize {
        self.grid().scrollback()
    }

    /// Returns the text contents of the terminal.
    ///
    /// This will not include any formatting information, and will be in plain
    /// text format.
    #[must_use]
    pub fn contents(&self) -> String {
        let mut contents = String::new();
        self.write_contents(&mut contents);
        contents
    }

    fn write_contents(&self, contents: &mut String) {
        self.grid().write_contents(contents);
    }

    /// Returns the text contents of the terminal by row, restricted to the
    /// given subset of columns.
    ///
    /// This will not include any formatting information, and will be in plain
    /// text format.
    ///
    /// Newlines will not be included.
    pub fn rows(
        &self,
        start: u16,
        width: u16,
    ) -> impl Iterator<Item = String> + '_ {
        self.grid().visible_rows().map(move |row| {
            let mut contents = String::new();
            row.write_contents(&mut contents, start, width, false);
            contents
        })
    }

    /// Returns the text contents of the terminal logically between two cells.
    /// This will include the remainder of the starting row after `start_col`,
    /// followed by the entire contents of the rows between `start_row` and
    /// `end_row`, followed by the beginning of the `end_row` up until
    /// `end_col`. This is useful for things like determining the contents of
    /// a clipboard selection.
    #[must_use]
    pub fn contents_between(
        &self,
        start_row: u16,
        start_col: u16,
        end_row: u16,
        end_col: u16,
    ) -> String {
        match start_row.cmp(&end_row) {
            core::cmp::Ordering::Less => {
                let (_, cols) = self.size();
                let cols = cols.get();
                let mut contents = String::new();
                for (i, row) in self
                    .grid()
                    .visible_rows()
                    .enumerate()
                    .skip(usize::from(start_row))
                    .take(usize::from(end_row) - usize::from(start_row) + 1)
                {
                    if i == usize::from(start_row) {
                        row.write_contents(
                            &mut contents,
                            start_col,
                            cols - start_col,
                            false,
                        );
                        if !row.wrapped() {
                            contents.push('\n');
                        }
                    } else if i == usize::from(end_row) {
                        row.write_contents(&mut contents, 0, end_col, false);
                    } else {
                        row.write_contents(&mut contents, 0, cols, false);
                        if !row.wrapped() {
                            contents.push('\n');
                        }
                    }
                }
                contents
            }
            core::cmp::Ordering::Equal => {
                if start_col < end_col {
                    self.rows(start_col, end_col - start_col)
                        .nth(usize::from(start_row))
                        .unwrap_or_default()
                } else {
                    String::new()
                }
            }
            core::cmp::Ordering::Greater => String::new(),
        }
    }

    /// Return escape codes sufficient to reproduce the entire contents of the
    /// current terminal state. This is a convenience wrapper around
    /// [`contents_formatted`](Self::contents_formatted) and
    /// [`input_mode_formatted`](Self::input_mode_formatted).
    #[must_use]
    pub fn state_formatted(&self) -> String {
        let mut contents = String::new();
        self.write_contents_formatted(&mut contents);
        self.write_input_mode_formatted(&mut contents);
        contents
    }

    /// Return escape codes sufficient to turn the terminal state of the
    /// screen `prev` into the current terminal state. This is a convenience
    /// wrapper around [`contents_diff`](Self::contents_diff) and
    /// [`input_mode_diff`](Self::input_mode_diff).
    #[must_use]
    pub fn state_diff(&self, prev: &Self) -> String {
        let mut contents = String::new();
        self.write_contents_diff(&mut contents, prev);
        self.write_input_mode_diff(&mut contents, prev);
        contents
    }

    /// Returns the formatted visible contents of the terminal.
    ///
    /// Formatting information will be included inline as terminal escape
    /// codes. The result will be suitable for feeding directly to a raw
    /// terminal parser, and will result in the same visual output.
    #[must_use]
    pub fn contents_formatted(&self) -> String {
        let mut contents = String::new();
        self.write_contents_formatted(&mut contents);
        contents
    }

    fn write_contents_formatted(&self, contents: &mut String) {
        crate::term::HideCursor::new(self.hide_cursor()).write_buf(contents);
        let prev_attrs = self.grid().write_contents_formatted(contents);
        self.attrs.write_escape_code_diff(contents, &prev_attrs);
    }

    /// Returns the formatted visible contents of the terminal in a "basic"
    /// format.
    ///
    /// The contents will contain no escape sequences except SGR sequences,
    /// and no control characters except `'\n'`.
    ///
    /// A newline will be inserted after each row that does not wrap onto the
    /// next line, except the last. Trailing whitespace will not be trimmed
    /// from the string; a blank terminal *n* rows tall will result in a string
    /// with *n* - 1 newlines.
    ///
    /// Terminal attributes should be reset before and after displaying the
    /// returned string; it will not begin or end with a reset sequence.
    #[must_use]
    pub fn contents_formatted_basic(&self) -> String {
        let mut contents = String::new();
        // Writing to a `String` cannot fail.
        #[allow(clippy::missing_panics_doc)]
        self.write_contents_formatted_basic(
            &mut contents,
            crate::capture::BasicFormattedCaptureRange::Visible,
        )
        .unwrap();
        contents
    }

    /// Like [`Self::contents_formatted_basic`] but writes into the provided
    /// writer.
    ///
    /// For the `range` parameter, if you used the callback to write terminal
    /// data into this writer in a streaming fashion, pass
    /// [`BasicFormattedCaptureRange::Full`][full] with your existing
    /// [`BasicFormattedCaptureState`][state]. Otherwise, if you want the same
    /// result as [`Self::contents_formatted_basic`], pass
    /// [`BasicFormattedCaptureRange::Visible`][visible].
    ///
    /// [`on_scroll`]: crate::Callbacks::on_scroll
    /// [full]: crate::capture::BasicFormattedCaptureRange::Full
    /// [state]: crate::capture::BasicFormattedCaptureState
    /// [visible]: crate::capture::BasicFormattedCaptureRange::Visible
    ///
    /// # Errors
    ///
    /// If the writer returns an error, this method will forward that error.
    /// Otherwise, this method will not return any errors of its own.
    pub fn write_contents_formatted_basic(
        &self,
        writer: &mut impl alloc::fmt::Write,
        range: crate::capture::BasicFormattedCaptureRange<'_>,
    ) -> alloc::fmt::Result {
        self.grid().write_contents_formatted_basic(writer, range)
    }

    /// Returns the formatted visible contents of the terminal by row,
    /// restricted to the given subset of columns.
    ///
    /// Formatting information will be included inline as terminal escape
    /// codes. The result will be suitable for feeding directly to a raw
    /// terminal parser, and will result in the same visual output.
    ///
    /// You are responsible for positioning the cursor before printing each
    /// row, and the final cursor position after displaying each row is
    /// unspecified.
    // the unwraps in this method shouldn't be reachable
    #[allow(clippy::missing_panics_doc)]
    pub fn rows_formatted(
        &self,
        start: u16,
        width: u16,
    ) -> impl Iterator<Item = String> + '_ {
        let mut wrapping = false;
        self.grid().visible_rows().enumerate().map(move |(i, row)| {
            // number of rows in a grid is stored in a u16 (see Size), so
            // visible_rows can never return enough rows to overflow here
            let i = i.try_into().unwrap();
            let mut contents = String::new();
            row.write_contents_formatted(
                &mut contents,
                start,
                width,
                i,
                wrapping,
                None,
                None,
            );
            if start == 0 && width == self.grid.size().cols() {
                wrapping = row.wrapped();
            }
            contents
        })
    }

    /// Returns a string containing escape sequences sufficient to turn the
    /// visible contents of the screen described by `prev` into the visible
    /// contents of the screen described by `self`.
    ///
    /// The result of rendering `prev.contents_formatted()` followed by
    /// `self.contents_diff(prev)` should be equivalent to the result of
    /// rendering `self.contents_formatted()`. This is primarily useful when
    /// you already have a terminal parser whose state is described by `prev`,
    /// since the diff will likely require less memory and cause less
    /// flickering than redrawing the entire screen contents.
    #[must_use]
    pub fn contents_diff(&self, prev: &Self) -> String {
        let mut contents = String::new();
        self.write_contents_diff(&mut contents, prev);
        contents
    }

    fn write_contents_diff(&self, contents: &mut String, prev: &Self) {
        if self.hide_cursor() != prev.hide_cursor() {
            crate::term::HideCursor::new(self.hide_cursor())
                .write_buf(contents);
        }
        let prev_attrs = self.grid().write_contents_diff(
            contents,
            prev.grid(),
            prev.attrs,
        );
        self.attrs.write_escape_code_diff(contents, &prev_attrs);
    }

    /// Returns a sequence of strings containing escape sequences sufficient to
    /// turn the visible contents of the subset of each row from `prev` (as
    /// described by `start` and `width`) into the visible contents of the
    /// corresponding row subset in `self`.
    ///
    /// You are responsible for positioning the cursor before printing each
    /// row, and the final cursor position after displaying each row is
    /// unspecified.
    // the unwraps in this method shouldn't be reachable
    #[allow(clippy::missing_panics_doc)]
    pub fn rows_diff<'a>(
        &'a self,
        prev: &'a Self,
        start: u16,
        width: u16,
    ) -> impl Iterator<Item = String> + 'a {
        self.grid()
            .visible_rows()
            .zip(prev.grid().visible_rows())
            .enumerate()
            .map(move |(i, (row, prev_row))| {
                // number of rows in a grid is stored in a u16 (see Size), so
                // visible_rows can never return enough rows to overflow here
                let i = i.try_into().unwrap();
                let mut contents = String::new();
                row.write_contents_diff(
                    &mut contents,
                    prev_row,
                    start,
                    width,
                    i,
                    false,
                    false,
                    crate::grid::Pos { row: i, col: start },
                    crate::attrs::Attrs::default(),
                );
                contents
            })
    }

    /// Returns terminal escape sequences sufficient to set the current
    /// terminal's input modes.
    ///
    /// Supported modes are:
    /// * application keypad
    /// * application cursor
    /// * bracketed paste
    /// * xterm mouse support
    #[must_use]
    pub fn input_mode_formatted(&self) -> String {
        let mut contents = String::new();
        self.write_input_mode_formatted(&mut contents);
        contents
    }

    fn write_input_mode_formatted(&self, contents: &mut String) {
        crate::term::ApplicationKeypad::new(
            self.mode(MODE_APPLICATION_KEYPAD),
        )
        .write_buf(contents);
        crate::term::ApplicationCursor::new(
            self.mode(MODE_APPLICATION_CURSOR),
        )
        .write_buf(contents);
        crate::term::BracketedPaste::new(self.mode(MODE_BRACKETED_PASTE))
            .write_buf(contents);
        crate::term::MouseProtocolMode::new(
            self.mouse_protocol_mode,
            MouseProtocolMode::None,
        )
        .write_buf(contents);
        crate::term::MouseProtocolEncoding::new(
            self.mouse_protocol_encoding,
            MouseProtocolEncoding::Default,
        )
        .write_buf(contents);
        crate::term::CursorStyle::new(
            self.cursor_style,
            CursorStyle::Default,
        )
        .write_buf(contents);
    }

    /// Returns terminal escape sequences sufficient to change the previous
    /// terminal's input modes to the input modes enabled in the current
    /// terminal.
    #[must_use]
    pub fn input_mode_diff(&self, prev: &Self) -> String {
        let mut contents = String::new();
        self.write_input_mode_diff(&mut contents, prev);
        contents
    }

    fn write_input_mode_diff(&self, contents: &mut String, prev: &Self) {
        if self.mode(MODE_APPLICATION_KEYPAD)
            != prev.mode(MODE_APPLICATION_KEYPAD)
        {
            crate::term::ApplicationKeypad::new(
                self.mode(MODE_APPLICATION_KEYPAD),
            )
            .write_buf(contents);
        }
        if self.mode(MODE_APPLICATION_CURSOR)
            != prev.mode(MODE_APPLICATION_CURSOR)
        {
            crate::term::ApplicationCursor::new(
                self.mode(MODE_APPLICATION_CURSOR),
            )
            .write_buf(contents);
        }
        if self.mode(MODE_BRACKETED_PASTE) != prev.mode(MODE_BRACKETED_PASTE)
        {
            crate::term::BracketedPaste::new(self.mode(MODE_BRACKETED_PASTE))
                .write_buf(contents);
        }
        crate::term::MouseProtocolMode::new(
            self.mouse_protocol_mode,
            prev.mouse_protocol_mode,
        )
        .write_buf(contents);
        crate::term::MouseProtocolEncoding::new(
            self.mouse_protocol_encoding,
            prev.mouse_protocol_encoding,
        )
        .write_buf(contents);
        crate::term::CursorStyle::new(self.cursor_style, prev.cursor_style)
            .write_buf(contents);
    }

    /// Returns terminal escape sequences sufficient to set the current
    /// terminal's drawing attributes.
    ///
    /// Supported drawing attributes are:
    /// * fgcolor
    /// * bgcolor
    /// * bold
    /// * dim
    /// * italic
    /// * underline
    /// * inverse
    ///
    /// This is not typically necessary, since
    /// [`contents_formatted`](Self::contents_formatted) will leave
    /// the current active drawing attributes in the correct state, but this
    /// can be useful in the case of drawing additional things on top of a
    /// terminal output, since you will need to restore the terminal state
    /// without the terminal contents necessarily being the same.
    #[must_use]
    pub fn attributes_formatted(&self) -> String {
        let mut contents = String::new();
        self.write_attributes_formatted(&mut contents);
        contents
    }

    fn write_attributes_formatted(&self, contents: &mut String) {
        crate::term::ClearAttrs.write_buf(contents);
        self.attrs.write_escape_code_diff(
            contents,
            &crate::attrs::Attrs::default(),
        );
    }

    /// Returns the current cursor position of the terminal.
    ///
    /// The return value will be (row, col).
    #[must_use]
    pub fn cursor_position(&self) -> (u16, u16) {
        let pos = self.grid().pos();
        (pos.row, pos.col)
    }

    /// Returns terminal escape sequences sufficient to set the current
    /// cursor state of the terminal.
    ///
    /// This is not typically necessary, since
    /// [`contents_formatted`](Self::contents_formatted) will leave
    /// the cursor in the correct state, but this can be useful in the case of
    /// drawing additional things on top of a terminal output, since you will
    /// need to restore the terminal state without the terminal contents
    /// necessarily being the same.
    ///
    /// Note that the string returned by this function may alter the active
    /// drawing attributes, because it may require redrawing existing cells in
    /// order to position the cursor correctly (for instance, in the case
    /// where the cursor is past the end of a row). Therefore, you should
    /// ensure to reset the active drawing attributes if necessary after
    /// processing this data, for instance by using
    /// [`attributes_formatted`](Self::attributes_formatted).
    #[must_use]
    pub fn cursor_state_formatted(&self) -> String {
        let mut contents = String::new();
        self.write_cursor_state_formatted(&mut contents);
        contents
    }

    fn write_cursor_state_formatted(&self, contents: &mut String) {
        crate::term::HideCursor::new(self.hide_cursor()).write_buf(contents);
        self.grid()
            .write_cursor_position_formatted(contents, None, None);

        // we don't just call write_attributes_formatted here, because that
        // would still be confusing - consider the case where the user sets
        // their own unrelated drawing attributes (on a different parser
        // instance) and then calls cursor_state_formatted. just documenting
        // it and letting the user handle it on their own is more
        // straightforward.
    }

    /// Returns the [`Cell`](crate::Cell) object at the given location in the
    /// terminal, if it exists.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<&crate::Cell> {
        self.grid().visible_cell(crate::grid::Pos { row, col })
    }

    /// Number of rows of the active screen counting history: scrollback rows
    /// followed by the visible screen. Index these with [`Screen::history_row`].
    #[must_use]
    pub fn history_rows_len(&self) -> usize {
        self.base_history_len()
    }

    /// Row `index` of the active screen counted from the oldest scrollback row
    /// (0) to the bottom of the screen. Unlike [`Screen::cell`], this does not
    /// depend on (or require changing) the scrollback view offset, so callers
    /// can read history without cloning the screen.
    #[must_use]
    pub fn history_row(&self, index: usize) -> Option<crate::capture::RowContents<'_>> {
        self.base_row(index).map(crate::capture::RowContents)
    }

    /// tmux's base grid (wp->base.grid) as the history formats see it: the
    /// normal screen's history, then the visible rows of the active screen
    /// (tmux swaps the alternate screen into the base grid's visible area).
    #[must_use]
    pub fn base_history_len(&self) -> usize {
        self.grid.scrollback_len() + usize::from(self.grid().size().rows.get())
    }

    /// Normal screen history limit and length, also while the alternate
    /// screen is active.
    #[must_use]
    pub fn base_scrollback(&self) -> (usize, usize) {
        (self.grid.scrollback_len(), self.grid.scrollback_capacity())
    }

    /// A new history-limit for the normal screen's history (session_update_history).
    pub fn set_history_limit(&mut self, limit: usize) {
        self.grid.set_scrollback_capacity(limit);
    }

    /// Row `index` of [`Screen::base_history_len`].
    #[must_use]
    pub fn base_history_row(&self, index: usize) -> Option<crate::capture::RowContents<'_>> {
        self.history_row(index)
    }

    /// Visible row `row` of the normal screen while the alternate screen is
    /// active (tmux's saved grid, read by `capture-pane -a`); `None` otherwise.
    #[must_use]
    pub fn saved_row(&self, row: u16) -> Option<crate::capture::RowContents<'_>> {
        if !self.mode(MODE_ALTERNATE_SCREEN) {
            return None;
        }
        self.grid
            .absolute_row(self.grid.scrollback_len() + usize::from(row))
            .map(crate::capture::RowContents)
    }

    /// Returns whether the text in row `row` should wrap to the next line.
    #[must_use]
    pub fn row_wrapped(&self, row: u16) -> bool {
        self.grid()
            .visible_row(row)
            .is_some_and(crate::row::Row::wrapped)
    }

    /// Returns whether the alternate screen is currently in use.
    #[must_use]
    pub fn alternate_screen(&self) -> bool {
        self.mode(MODE_ALTERNATE_SCREEN)
    }

    /// Returns whether the terminal should be in application keypad mode.
    #[must_use]
    pub fn application_keypad(&self) -> bool {
        self.mode(MODE_APPLICATION_KEYPAD)
    }

    /// Returns whether the terminal should be in application cursor mode.
    #[must_use]
    pub fn application_cursor(&self) -> bool {
        self.mode(MODE_APPLICATION_CURSOR)
    }

    /// Returns whether the terminal should be in hide cursor mode.
    #[must_use]
    pub fn hide_cursor(&self) -> bool {
        self.mode(MODE_HIDE_CURSOR)
    }

    /// Returns whether the terminal should be in bracketed paste mode.
    #[must_use]
    pub fn bracketed_paste(&self) -> bool {
        self.mode(MODE_BRACKETED_PASTE)
    }

    /// Returns the currently active [`MouseProtocolMode`].
    #[must_use]
    pub fn mouse_protocol_mode(&self) -> MouseProtocolMode {
        self.mouse_protocol_mode
    }

    /// Returns the currently active [`MouseProtocolEncoding`].
    #[must_use]
    pub fn mouse_protocol_encoding(&self) -> MouseProtocolEncoding {
        self.mouse_protocol_encoding
    }

    /// Returns the currently active [`CursorStyle`].
    #[must_use]
    pub fn cursor_style(&self) -> CursorStyle {
        self.cursor_style
    }

    /// Insert mode (IRM, `CSI 4 h`) as set by the application.
    #[must_use]
    pub fn insert_mode(&self) -> bool {
        self.xmode(XMODE_INSERT)
    }

    /// Origin mode (DECOM, `CSI ? 6 h`).
    #[must_use]
    pub fn origin_mode(&self) -> bool {
        self.grid().origin_mode()
    }

    /// Autowrap mode (DECAWM, `CSI ? 7 h`), on by default.
    #[must_use]
    pub fn wrap_mode(&self) -> bool {
        self.xmode(XMODE_WRAP)
    }

    /// Whether the cursor blinks (`CSI ? 12 h` or a blinking DECSCUSR style),
    /// and whether that was set with `CSI ? 12 h/l` rather than DECSCUSR.
    #[must_use]
    pub fn cursor_blinking(&self) -> (bool, bool) {
        (self.xmode(XMODE_CURSOR_BLINKING), self.xmode(XMODE_CURSOR_BLINKING_SET))
    }

    /// Very visible cursor (`CSI 34 l`).
    #[must_use]
    pub fn cursor_very_visible(&self) -> bool {
        self.xmode(XMODE_CURSOR_VERY_VISIBLE)
    }

    /// Focus reporting (`CSI ? 1004 h`).
    #[must_use]
    pub fn focus_events(&self) -> bool {
        self.xmode(XMODE_FOCUS)
    }

    /// The mouse encodings asked for, as (UTF-8 1005, SGR 1006).
    #[must_use]
    pub fn mouse_encodings(&self) -> (bool, bool) {
        (self.xmode(XMODE_MOUSE_UTF8), self.xmode(XMODE_MOUSE_SGR))
    }

    /// Synchronized output (`CSI ? 2026 h`) in progress.
    #[must_use]
    pub fn synchronized_output(&self) -> bool {
        self.xmode(XMODE_SYNC)
    }

    /// Theme change notifications (`CSI ? 2031 h`).
    #[must_use]
    pub fn theme_updates(&self) -> bool {
        self.xmode(XMODE_THEME_UPDATES)
    }

    /// Cursor position saved on entering the alternate screen with DECSET
    /// 1049, as (row, col); None before that has happened.
    #[must_use]
    pub fn alternate_saved_cursor(&self) -> Option<(u16, u16)> {
        self.alternate_saved
    }

    /// The scroll region of the active screen as (top, bottom) rows.
    #[must_use]
    pub fn scroll_region(&self) -> (u16, u16) {
        self.grid().scroll_region()
    }

    /// Columns with a tab stop.
    #[must_use]
    pub fn tab_stops(&self) -> Vec<u16> {
        let cols = self.grid.size().cols();
        match &self.tabs {
            Some(t) => (0..cols).filter(|&i| t.get(usize::from(i)).copied().unwrap_or(false)).collect(),
            None => (8..cols).step_by(8).collect(),
        }
    }

    /// History counters of the normal screen: (rows added to history, rows
    /// dropped from it, times it was cleared or rewrapped), like tmux's
    /// history_added, history_collected and history_generation.
    #[must_use]
    pub fn history_counters(&self) -> (u64, u64, u64) {
        self.grid.history_counters()
    }

    /// Returns the currently active foreground color.
    #[must_use]
    pub fn fgcolor(&self) -> crate::Color {
        self.attrs.fgcolor
    }

    /// Returns the currently active background color.
    #[must_use]
    pub fn bgcolor(&self) -> crate::Color {
        self.attrs.bgcolor
    }

    /// Returns whether newly drawn text should be rendered with the bold text
    /// attribute.
    #[must_use]
    pub fn bold(&self) -> bool {
        self.attrs.bold()
    }

    /// Returns whether newly drawn text should be rendered with the dim text
    /// attribute.
    #[must_use]
    pub fn dim(&self) -> bool {
        self.attrs.dim()
    }

    /// Returns whether newly drawn text should be rendered with the italic
    /// text attribute.
    #[must_use]
    pub fn italic(&self) -> bool {
        self.attrs.italic()
    }

    /// Returns whether newly drawn text should be rendered with the
    /// underlined text attribute.
    #[must_use]
    pub fn underline(&self) -> bool {
        self.attrs.underline()
    }

    /// Returns whether newly drawn text should be rendered with the inverse
    /// text attribute.
    #[must_use]
    pub fn inverse(&self) -> bool {
        self.attrs.inverse()
    }

    pub(crate) fn grid(&self) -> &crate::grid::Grid {
        if self.mode(MODE_ALTERNATE_SCREEN) {
            &self.alternate_grid
        } else {
            &self.grid
        }
    }

    fn grid_mut(&mut self) -> &mut crate::grid::Grid {
        if self.mode(MODE_ALTERNATE_SCREEN) {
            &mut self.alternate_grid
        } else {
            &mut self.grid
        }
    }

    /// screen_write_alternateon: DECSET 47, 1047 and 1049 (`cursor`). The
    /// alternate screen starts clear with the cursor where it was; nothing
    /// happens when already there or when alternate-screen is off.
    fn alternate_on(&mut self, cursor: bool) {
        if !self.allow_alternate || self.mode(MODE_ALTERNATE_SCREEN) {
            return;
        }
        let pos = self.grid.pos();
        if cursor {
            self.alternate_saved = Some((pos.row, pos.col));
            self.alternate_saved_attrs = self.attrs;
        }
        self.grid.set_scrollback(0);
        self.alternate_grid.clear();
        self.alternate_grid.allocate_rows();
        self.alternate_grid.set_pos_abs(pos);
        self.set_mode(MODE_ALTERNATE_SCREEN);
    }

    /// screen_write_alternateoff: DECRST 47, 1047 and 1049 (`cursor`). With
    /// 1049 the saved cursor comes back even outside the alternate screen.
    fn alternate_off(&mut self, cursor: bool) {
        if !self.allow_alternate {
            return;
        }
        let mut pos = self.grid().pos();
        if let Some((row, col)) = self.alternate_saved.filter(|_| cursor) {
            pos = crate::grid::Pos { row, col };
            self.attrs = self.alternate_saved_attrs;
        }
        self.clear_mode(MODE_ALTERNATE_SCREEN);
        self.grid.set_pos_abs(pos);
    }

    fn save_cursor(&mut self) {
        self.grid_mut().save_cursor();
        self.saved_attrs = self.attrs;
        self.saved_charset = self.charset;
        self.saved_hyperlink = self.hyperlink.clone();
    }

    /// SCS (ESC ( and ESC )): G0 or G1 is DEC special graphics ('0') or ASCII ('B').
    pub fn designate_charset(&mut self, g1: bool, graphics: bool) {
        let bit = if g1 { CHARSET_G1 } else { CHARSET_G0 };
        if graphics { self.charset |= bit } else { self.charset &= !bit }
    }

    /// SO (true) and SI (false): G1 or G0 is the set in use.
    pub fn shift_charset(&mut self, g1: bool) {
        if g1 { self.charset |= CHARSET_SHIFT } else { self.charset &= !CHARSET_SHIFT }
    }

    /// input_print: a 7-bit character is drawn from the set in use.
    fn graphics_charset(&self) -> bool {
        let bit = if self.charset & CHARSET_SHIFT != 0 { CHARSET_G1 } else { CHARSET_G0 };
        self.charset & bit != 0
    }

    fn restore_cursor(&mut self) {
        self.grid_mut().restore_cursor();
        self.attrs = self.saved_attrs;
        self.charset = self.saved_charset;
        // input_restore_state: the saved cell has its link
        self.hyperlink = self.saved_hyperlink.clone();
    }

    fn set_mode(&mut self, mode: u8) {
        self.modes |= mode;
    }

    fn clear_mode(&mut self, mode: u8) {
        self.modes &= !mode;
    }

    fn mode(&self, mode: u8) -> bool {
        self.modes & mode != 0
    }

    fn set_mouse_mode(&mut self, mode: MouseProtocolMode) {
        self.mouse_protocol_mode = mode;
    }

    fn clear_mouse_mode(&mut self, mode: MouseProtocolMode) {
        if self.mouse_protocol_mode == mode {
            self.mouse_protocol_mode = MouseProtocolMode::default();
        }
    }

    fn set_mouse_encoding(&mut self, encoding: MouseProtocolEncoding) {
        self.mouse_protocol_encoding = encoding;
    }

    fn clear_mouse_encoding(&mut self, encoding: MouseProtocolEncoding) {
        if self.mouse_protocol_encoding == encoding {
            self.mouse_protocol_encoding = MouseProtocolEncoding::default();
        }
    }

    fn set_cursor_style(&mut self, style: CursorStyle) {
        self.cursor_style = style;
    }

    fn set_xmode(&mut self, mode: u16, on: bool) {
        if on {
            self.xmodes |= mode;
        } else {
            self.xmodes &= !mode;
        }
    }

    fn xmode(&self, mode: u16) -> bool {
        self.xmodes & mode != 0
    }

    fn tabs_mut(&mut self) -> &mut Vec<bool> {
        let cols = usize::from(self.grid.size().cols());
        self.tabs.get_or_insert_with(|| (0..cols).map(|i| i > 0 && i % 8 == 0).collect())
    }

    // determine the width of the character we're about to draw, and whether
    // it makes sense to even attempt to draw a character of that width.
    fn calculate_width(&self, c: char) -> Option<u16> {
        let pos = self.grid().pos();
        let size = self.grid().size();

        // codepoint-widths never applies to 7-bit characters (tmux does not look them up)
        let width = self.widths.iter().rev().find(|(a, b, _)| !c.is_ascii() && (*a..=*b).contains(&(c as u32)))
            .map(|(_, _, w)| *w as usize).or_else(|| c.width());
        // utf8_width: C1 control characters are always zero width
        let width = if ('\u{80}'..='\u{9f}').contains(&c) { Some(width.unwrap_or(0)) } else { width };
        if width.is_none() && (u32::from(c)) < 256 {
            // don't even try to draw control characters
            return None;
        }
        let width = u16::try_from(width.unwrap_or(1))
            // char::width() can only return 0, 1, 2, or 3
            .unwrap();
        // TODO: this is invalid for U+17D8 which has a width of 3 for some
        // reason, but adding triple width characters is going to be a
        // massive pain just for this one character
        let width = width.min(2);

        // if the character is wider than the screen, we can't draw it, so
        // just ignore it
        if width > size.cols() {
            return None;
        }

        // a continuation character without a base character before it is
        // meaningless
        if width == 0 && pos.col == 0 {
            return None;
        }

        Some(width)
    }

    /// screen_write_combine for a character with a width: whether it joins the cell
    /// before the cursor (after a zero width joiner; a skin tone modifier and an emoji
    /// that takes one, in either order; two regional indicators) and whether that makes
    /// the cell wide (utf8_should_combine).
    fn combine_with_last(&self, c: char) -> Option<bool> {
        let pos = self.grid().pos();
        // a 7-bit character never combines
        if pos.col == 0 || c.is_ascii() {
            return None;
        }
        let mut col = pos.col - 1;
        let cell = self.grid().drawing_cell(crate::grid::Pos { row: pos.row, col })?;
        let cell = if cell.is_wide_continuation() && col > 0 {
            col -= 1;
            self.grid().drawing_cell(crate::grid::Pos { row: pos.row, col })?
        } else {
            cell
        };
        // the cell must be as wide as the distance to the cursor
        if cell.is_wide_continuation() || cell.is_wide() != (pos.col - col == 2) {
            return None;
        }
        let text = cell.contents();
        let ri = |x: char| ('\u{1F1E6}'..='\u{1F1FF}').contains(&x);
        let modifier = |x: char| ('\u{1F3FB}'..='\u{1F3FF}').contains(&x);
        // utf8_towc reads the cell's first character; regional indicators pair only
        // when the cell has one (utf8_regional_count)
        if let Some(l) = text.chars().next() {
            let pair = ri(c) && ri(l) && text.chars().filter(|x| ri(*x)).count() == 1;
            if pair || (modifier(c) && EMOJI_MODIFIER_BASE.contains(&(l as u32))) || (modifier(l) && EMOJI_MODIFIER_BASE.contains(&(c as u32))) {
                return Some(true);
            }
        }
        text.ends_with('\u{200D}').then_some(false)
    }

    // if this is the initial codepoint in a new cell, we need to determine
    // whether wrapping that character to the next row should set the row's
    // wrapping flag
    fn new_character_should_wrap(&self, width: u16) -> bool {
        let pos = self.grid().pos();
        let size = self.grid().size();

        // it doesn't make any sense to wrap if the last column in a row
        // didn't already have contents. don't try to handle the case
        // where a character wraps because there was only one column left
        // in the previous row - literally everything handles this case
        // differently, and this is tmux behavior (and also the
        // simplest). i'm open to reconsidering this behavior, but only
        // with a really good reason (xterm handles this by introducing
        // the concept of triple width cells, which i really don't want
        // to do).
        let mut wrap = false;
        if pos.col + width > size.cols() {
            let last_cell = self
                .grid()
                .drawing_cell(crate::grid::Pos {
                    row: pos.row,
                    col: size.cols() - 1,
                })
                // pos.row is valid, since it comes directly from
                // self.grid().pos() which we assume to always have a
                // valid row value. size.cols - 1 is also always a valid
                // column.
                .unwrap();
            if last_cell.has_contents() || last_cell.is_wide_continuation() {
                wrap = true;
            }
        }

        wrap
    }

    // once we have drawn a character, clean up the wide character state of
    // nearby cells that may have been disrupted.
    fn text_cleanup(
        &mut self,
        base_pos: crate::grid::Pos,
        cell_was_wide: bool,
        cell_is_wide: bool,
    ) {
        let size = self.grid().size();
        let attrs = self.attrs;

        if base_pos.col > 0 && self.grid().drawing_cell(crate::grid::Pos { row: base_pos.row, col: base_pos.col - 1 }).is_some_and(crate::Cell::is_wide) {
            let prev_cell = self
                .grid_mut()
                .drawing_cell_mut(crate::grid::Pos {
                    row: base_pos.row,
                    col: base_pos.col - 1,
                })
                // pos.row is assumed valid, and pos.col was just
                // checked to be greater than 0.
                .unwrap();
            if prev_cell.is_wide() {
                prev_cell.clear(*prev_cell.attrs());
            }
        }

        // a wide cell in the last column has its second half off the screen
        // (screen_write_combine), so the cell after a wide cell may be missing
        let next = crate::grid::Pos { row: base_pos.row, col: base_pos.col + 1 };
        if cell_was_wide && !cell_is_wide {
            if let Some(next_cell) = self.grid_mut().drawing_cell_mut(next) {
                if next_cell.is_wide_continuation() {
                    next_cell.set(' ', attrs);
                }
            }
        }

        if !cell_was_wide && cell_is_wide {
            let Some(next_cell) = self.grid_mut().drawing_cell_mut(next) else {
                return;
            };
            let next_cell_was_wide = next_cell.is_wide();
            next_cell.clear(crate::attrs::Attrs::default());
            next_cell.set_wide_continuation(true);
            if next_cell_was_wide {
                let next_next = crate::grid::Pos { row: base_pos.row, col: base_pos.col + 2 };
                if let Some(next_next_cell) = self.grid_mut().drawing_cell_mut(next_next).filter(|c| c.is_wide_continuation()) {
                    next_next_cell.clear(attrs);
                    if base_pos.col + 2 == size.cols() - 1 {
                        if let Some(row) = self.grid_mut().drawing_row_mut(base_pos.row) {
                            row.wrap(false);
                        }
                    }
                }
            }
        }
    }
}

pub struct WrappedScreen<CB: crate::Callbacks> {
    pub screen: Screen,
    pub callbacks: CB,
}

impl<CB: crate::Callbacks> WrappedScreen<CB> {
    /// Gets the `on_row` callback for methods like
    /// [`crate::grid::Grid::scroll_up`].
    fn on_row<'cb>(
        screen: &Screen,
        callbacks: &'cb mut CB,
    ) -> impl FnMut(&crate::row::Row) + 'cb {
        let is_alternate = screen.mode(MODE_ALTERNATE_SCREEN);
        move |row| {
            callbacks
                .on_scroll(crate::capture::RowContents(row), is_alternate);
        }
    }

    /// Resizes the screen, calling [`on_scroll`] for each row that is pushed
    /// off the top of the screen's scrollback.
    ///
    /// [`on_scroll`]: crate::Callbacks::on_scroll
    pub fn set_size_with_cb(
        &mut self,
        rows: core::num::NonZeroU16,
        cols: core::num::NonZeroU16,
    ) {
        self.screen.reset_tabs_for(cols.get());
        let callbacks = &mut self.callbacks;
        self.screen
            .grid
            .set_size(crate::grid::Size { rows, cols }, |row| {
                callbacks.on_scroll(crate::capture::RowContents(row), false);
            });
        self.screen.alternate_grid.set_size(
            crate::grid::Size { rows, cols },
            |row| {
                callbacks.on_scroll(crate::capture::RowContents(row), true);
            },
        );
    }

    pub fn text(&mut self, c: char) {
        if matches!(c, ' '..='~') && self.screen.wrap_mode() && !self.screen.insert_mode() {
            self.text_ascii(c);
        } else {
            self.text_any(c);
        }
    }

    fn text_any(&mut self, c: char) {
        let screen = &mut self.screen;
        // screen_write_cell: one cell at a time
        screen.end_print_run();
        let pos = screen.grid().pos();
        let mut attrs = screen.attrs;
        attrs.set_charset(c.is_ascii() && screen.graphics_charset());

        let Some(width) = screen.calculate_width(c) else {
            return;
        };
        let combine = if width > 0 { screen.combine_with_last(c) } else { None };
        let width = if combine.is_some() { 0 } else { width };

        // base_pos will always be set to a valid cell, so
        // .drawing_cell(base_pos).unwrap() will always be valid
        let mut base_pos;
        let cell_was_wide;

        let nowrap = !screen.wrap_mode();
        if width > 0 {
            if nowrap {
                // screen_write_cell without MODE_WRAP: a character that does not fit is
                // dropped, the last column is written over
                if pos.col + width > screen.grid().size().cols() {
                    return;
                }
            } else {
                let wrap = screen.new_character_should_wrap(width);
                let on_row = Self::on_row(screen, &mut self.callbacks);
                screen.grid_mut().col_wrap(width, wrap, on_row);
            }
            // col_wrap always sets self.grid().pos() to a valid cell
            base_pos = screen.grid().pos();
            // IRM (screen_write_cell): the character makes room for itself
            if screen.insert_mode() {
                let row = screen.grid_mut().drawing_row_mut(base_pos.row).unwrap();
                let wrapped = row.wrapped();
                row.insert(base_pos.col, width);
                row.wrap(wrapped);
            }
            cell_was_wide =
                screen.grid().drawing_cell(base_pos).unwrap().is_wide();

            screen
                .grid_mut()
                .drawing_cell_mut(base_pos)
                .unwrap()
                .set(c, attrs);
            screen.grid_mut().drawing_cell_mut(base_pos).unwrap().set_wide(width == 2);
            screen.last_char = Some(c);
            screen.grid_mut().col_inc(width);
            if nowrap && screen.grid().pos().col >= screen.grid().size().cols() {
                let last = screen.grid().size().cols() - 1;
                screen.grid_mut().col_set(last);
            }
        } else {
            // pos.row is valid, since it comes directly from
            // self.grid().pos() which we assume to always have a
            // valid row value. pos.col - 1 is valid because we just
            // returned early if pos.col == 0.
            base_pos = crate::grid::Pos {
                row: pos.row,
                col: pos.col - 1,
            };
            if screen
                .grid()
                .drawing_cell(base_pos)
                .unwrap()
                .is_wide_continuation()
            {
                // the first half of the wide character is before its
                // continuation; a continuation in the first column has none
                let Some(col) = pos.col.checked_sub(2) else {
                    return;
                };
                base_pos = crate::grid::Pos { row: pos.row, col };
            }
            // screen_write_combine: the cell must be as wide as the distance to
            // the cursor, so nothing joins a wide cell from its second half or
            // from beyond a wide cell in the last column
            let base = screen.grid().drawing_cell(base_pos).unwrap();
            if base.is_wide_continuation() || base.is_wide() != (pos.col - base_pos.col == 2) {
                return;
            }
            cell_was_wide =
                screen.grid().drawing_cell(base_pos).unwrap().is_wide();

            screen
                .grid_mut()
                .drawing_cell_mut(base_pos)
                .unwrap()
                .append(c);
            // VS16 makes a narrow base wide only with variation-selector-always-wide on;
            // a modifier or a second regional indicator always does
            if c == '\u{FE0F}' || combine == Some(true) {
                let wide = screen.variation_wide || cell_was_wide || combine == Some(true);
                screen.grid_mut().drawing_cell_mut(base_pos).unwrap().set_wide(wide);
            }
            let cell_is_wide =
                screen.grid().drawing_cell(base_pos).unwrap().is_wide();
            // screen_write_combine widens the cell where it is and moves the cursor
            // past its second half; in the last column that half is off the screen
            // (the cell is drawn blank, tty_draw_line_get_empty) and the cursor stays
            if !cell_was_wide && cell_is_wide && base_pos.col + 1 < screen.grid().size().cols() {
                screen.grid_mut().col_inc(1);
            }
        }

        let cell_is_wide =
            screen.grid().drawing_cell(base_pos).unwrap().is_wide();

        screen.text_cleanup(base_pos, cell_was_wide, cell_is_wide);
        // grid_set_cell and grid_set_padding: the line has room for the character and
        // its second half (screen_write_overwrite's blank after a half it replaced too)
        let cols = screen.grid().size().cols();
        let end = base_pos.col + if cell_is_wide || cell_was_wide { 2 } else { 1 };
        screen.grid_mut().drawing_row_mut(base_pos.row).unwrap().expand(end.min(cols));
        let hyperlink = screen.hyperlink.clone();
        // grid_extended_cell: the line keeps GRID_LINE_EXTENDED once an extended cell is
        // written to it, even after the cell is overwritten
        let cell = screen.grid().drawing_cell(base_pos).unwrap();
        let extended = cell.contents().len() > 1 || cell.is_wide() || matches!(cell.fgcolor(), crate::Color::Rgb(..)) || matches!(cell.bgcolor(), crate::Color::Rgb(..));
        let row = screen.grid_mut().drawing_row_mut(base_pos.row).unwrap();
        if extended {
            row.set_flags(row.flags() | crate::ROW_EXTENDED);
        }
        if hyperlink.is_some() {
            row.set_metadata(base_pos.col, crate::CellMetadata { hyperlink, tab_width: 0 });
        }
    }

    /// `text` for a printable ASCII character with wrapping on and insert mode off,
    /// looking the row up once. Such a character is one column wide, never combines and
    /// never makes a cell wide, so this is the width-1 path of `text` with those cases
    /// left out: the same cells are read and written in the same order (a written cell
    /// loses its metadata, `Row::get_mut`), then `text_cleanup` and the line flags.
    fn text_ascii(&mut self, c: char) {
        let screen = &mut self.screen;
        let mut attrs = screen.attrs;
        attrs.set_charset(screen.graphics_charset());
        let wrap = screen.new_character_should_wrap(1);
        // a wrap ends the run on the row it was written on
        if wrap {
            screen.end_print_run();
        }
        let on_row = Self::on_row(screen, &mut self.callbacks);
        screen.grid_mut().col_wrap(1, wrap, on_row);
        // col_wrap always leaves the cursor on a valid cell
        let pos = screen.grid().pos();
        screen.last_char = Some(c);
        screen.grid_mut().col_inc(1);
        let cleanup_attrs = screen.attrs;
        let hyperlink = screen.hyperlink.clone();
        // the run goes on to this cell (its line grows when it ends)
        screen.print_run = pos.col + 1;
        let mut base = None;
        let row = screen.grid_mut().drawing_row_mut(pos.row).unwrap();
        let size = row.cellsize();
        let cell = row.get_mut(pos.col).unwrap();
        let cell_was_wide = cell.is_wide();
        // Cell::set clears the wide flags of a one column character
        cell.set(c, attrs);
        // text_cleanup with cell_is_wide false
        if pos.col > 0 && row.get(pos.col - 1).is_some_and(crate::Cell::is_wide) {
            let prev_cell = row.get_mut(pos.col - 1).unwrap();
            prev_cell.clear(*prev_cell.attrs());
        }
        if cell_was_wide {
            if let Some(next_cell) = row.get_mut(pos.col + 1) {
                if next_cell.is_wide_continuation() {
                    next_cell.set(' ', cleanup_attrs);
                    base = Some(size);
                    row.expand(pos.col + 2);
                }
            }
        }
        // grid_extended_cell: of the reasons in `text`, only an RGB colour applies
        if matches!(attrs.fgcolor, crate::Color::Rgb(..)) || matches!(attrs.bgcolor, crate::Color::Rgb(..)) {
            row.set_flags(row.flags() | crate::ROW_EXTENDED);
        }
        if hyperlink.is_some() {
            row.set_metadata(pos.col, crate::CellMetadata { hyperlink, tab_width: 0 });
        }
        screen.print_run_size = size;
        if let Some(base) = base {
            screen.print_run_base.get_or_insert(base);
        }
    }

    // control codes

    pub fn bs(&mut self) {
        self.screen.grid_mut().col_dec(1);
    }

    /// HT (input_c0_dispatch): over blank cells that look alike (up to 32) a
    /// tab is kept (GRID_FLAG_TAB) with the first cell's style and link, so
    /// capture-pane and copy mode give back the tab; otherwise the cursor just moves.
    pub fn tab(&mut self) {
        let col = self.screen.grid().pos().col;
        let last = self.screen.grid().size().cols() - 1;
        if col >= last { return; }
        let next = (col + 1..=last).find(|&i| self.screen.tabs.as_ref().map_or(i % 8 == 0, |t| t.get(usize::from(i)).copied().unwrap_or(false))).unwrap_or(last);
        let insert = self.screen.insert_mode();
        let row = self.screen.grid_mut().current_row_mut();
        let first = row.get(col).unwrap().clone();
        let link = |x: u16| row.cell_metadata(x).and_then(|m| m.hyperlink.as_ref()).map(|l| l.inner);
        // a tab's own cell (its width is its size) and padding are content, except that a
        // one-column tab looks like the one-column tab it is compared with
        let tab = |x: u16| row.metadata().take_while(|&(at, _)| at <= x).filter(|(at, m)| x < at.saturating_add(m.tab_width)).map(|(at, m)| (at, m.tab_width)).last();
        let blank = |x: u16| row.get(x).is_some_and(|c| (!c.has_contents() || c.contents() == " ") && !c.is_wide_continuation() && c.attrs() == first.attrs() && link(x) == link(col))
            && match (tab(col), tab(x)) { (None, None) => true, (Some((at, 1)), Some(t)) => at == col && t == (col, 1) && next == col + 1, _ => false };
        if next - col <= 32 && (col..next).all(blank) {
            let hyperlink = row.cell_metadata(col).and_then(|m| m.hyperlink.clone());
            // in IRM the tab is inserted like any cell
            if insert {
                let wrapped = row.wrapped();
                row.insert(col, next - col);
                row.wrap(wrapped);
            }
            for x in col..next { row.get_mut(x).unwrap().set(' ', *first.attrs()); }
            // screen_write_cell sets the padding cells one at a time (grid_set_padding),
            // then the tab's own cell
            row.expand_cells(col + 1, next);
            row.expand(col + 1);
            row.set_metadata(col, crate::CellMetadata { tab_width: next - col, hyperlink });
        }
        self.screen.grid_mut().col_set(next);
    }

    // ESC H
    pub fn hts(&mut self) {
        let col = usize::from(self.screen.grid().pos().col);
        if let Some(t) = self.screen.tabs_mut().get_mut(col) {
            *t = true;
        }
    }

    // CSI g
    pub fn tbc(&mut self, mode: u16) {
        match mode {
            0 => {
                let col = usize::from(self.screen.grid().pos().col);
                if let Some(t) = self.screen.tabs_mut().get_mut(col) {
                    *t = false;
                }
            }
            3 => self.screen.tabs_mut().iter_mut().for_each(|t| *t = false),
            _ => {}
        }
    }

    // CSI h
    pub fn sm(&mut self, params: &vte::Params, mut unhandled: impl FnMut(&mut Self)) {
        for param in params {
            match param {
                [4] => self.screen.set_xmode(XMODE_INSERT, true),
                [34] => self.screen.set_xmode(XMODE_CURSOR_VERY_VISIBLE, false),
                _ => unhandled(self),
            }
        }
    }

    // CSI l
    pub fn rm(&mut self, params: &vte::Params, mut unhandled: impl FnMut(&mut Self)) {
        for param in params {
            match param {
                [4] => self.screen.set_xmode(XMODE_INSERT, false),
                [34] => self.screen.set_xmode(XMODE_CURSOR_VERY_VISIBLE, true),
                _ => unhandled(self),
            }
        }
    }

    pub fn lf(&mut self) {
        let on_row = Self::on_row(&self.screen, &mut self.callbacks);
        self.screen.grid_mut().row_inc_scroll(1, on_row);
    }

    pub fn vt(&mut self) {
        self.lf();
    }

    pub fn ff(&mut self) {
        self.lf();
    }

    pub fn cr(&mut self) {
        self.screen.grid_mut().col_set(0);
    }

    // escape codes

    // ESC 7
    pub fn decsc(&mut self) {
        self.screen.save_cursor();
    }

    // ESC 8
    pub fn decrc(&mut self) {
        self.screen.restore_cursor();
        // input_restore_state moves with screen_write_cursormove, which keeps the cursor
        // on the screen: a saved pending wrap is gone
        self.leave_pending_wrap();
    }

    // ESC =
    pub fn deckpam(&mut self) {
        self.screen.set_mode(MODE_APPLICATION_KEYPAD);
    }

    // ESC >
    pub fn deckpnm(&mut self) {
        self.screen.clear_mode(MODE_APPLICATION_KEYPAD);
    }

    // ESC M
    pub fn ri(&mut self) {
        self.screen.grid_mut().row_dec_scroll(1);
    }

    // ESC # 8
    pub fn decaln(&mut self) {
        self.screen.grid_mut().alignment_test();
    }

    // ESC c
    pub fn ris(&mut self) {
        let scroll_on_clear = self.screen.scroll_on_clear;
        let allow_alternate = self.screen.allow_alternate;
        let widths = core::mem::take(&mut self.screen.widths);
        let variation_wide = self.screen.variation_wide;
        // screen_write_reset keeps the cursor shape (only the mode bits are reset)
        let cursor_style = self.screen.cursor_style;
        let now = self.screen.grid.now();
        // screen_write_reset clears the screen like ED 2: the history stays and, with
        // scroll-on-clear, the main screen's lines go into it
        if scroll_on_clear && !self.screen.mode(MODE_ALTERNATE_SCREEN) {
            let attrs = crate::attrs::Attrs::default();
            let on_row = Self::on_row(&self.screen, &mut self.callbacks);
            self.screen.grid.erase_all_into_history(attrs, on_row);
        }
        let history = self.screen.grid.take_history();
        let counters = self.screen.grid.history_counters();
        self.screen = Screen::new(
            self.screen.grid.size(),
            self.screen.grid.scrollback_capacity(),
        );
        self.screen.scroll_on_clear = scroll_on_clear;
        self.screen.allow_alternate = allow_alternate;
        self.screen.widths = widths;
        self.screen.variation_wide = variation_wide;
        self.screen.cursor_style = cursor_style;
        self.screen.grid.set_history_counters(counters);
        self.screen.grid.set_history(history);
        self.screen.set_time(now);
    }

    // csi codes

    // CSI SP
    pub(crate) fn decscusr(
        &mut self,
        cursor_style: u16,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        match cursor_style {
            0 => self.screen.set_cursor_style(CursorStyle::Default),
            1 => self.screen.set_cursor_style(CursorStyle::BlinkBlock),
            2 => self.screen.set_cursor_style(CursorStyle::SteadyBlock),
            3 => self.screen.set_cursor_style(CursorStyle::BlinkUnderline),
            4 => self.screen.set_cursor_style(CursorStyle::SteadyUnderline),
            5 => self.screen.set_cursor_style(CursorStyle::BlinkVerticalBar),
            6 => self.screen.set_cursor_style(CursorStyle::SteadyVerticalBar),
            _ => return unhandled(self),
        }
        // like tmux: odd styles blink, even ones do not, and 0 goes back to
        // the default blinking state
        match cursor_style {
            0 => self.screen.set_xmode(XMODE_CURSOR_BLINKING_SET, false),
            n => self.screen.set_xmode(XMODE_CURSOR_BLINKING, n % 2 == 1),
        }
    }

    // CSI @
    pub fn ich(&mut self, count: u16) {
        self.screen.grid_mut().insert_cells(count);
    }

    // CSI A
    pub fn cuu(&mut self, offset: u16) {
        // screen_write_cursorup: the cursor leaves a pending wrap for the last column
        self.leave_pending_wrap();
        self.screen.grid_mut().row_dec_clamp(offset);
    }

    // CSI B
    pub fn cud(&mut self, offset: u16) {
        self.leave_pending_wrap();
        self.screen.grid_mut().row_inc_clamp(offset);
    }

    fn leave_pending_wrap(&mut self) {
        let cols = self.screen.grid().size().cols();
        if self.screen.grid().pos().col >= cols {
            self.screen.grid_mut().col_set(cols - 1);
        }
    }

    // ESC D (IND): a line feed without a carriage return
    pub fn ind(&mut self) {
        let on_row = Self::on_row(&self.screen, &mut self.callbacks);
        self.screen.grid_mut().row_inc_scroll(1, on_row);
    }

    // ESC E (NEL)
    pub fn nel(&mut self) {
        self.cr();
        self.ind();
    }

    // CSI Z (CBT): back to the previous tab stop, `count` times
    pub fn cbt(&mut self, count: u16) {
        let cols = self.screen.grid().size().cols();
        let mut cx = self.screen.grid().pos().col.min(cols - 1);
        let stop = |s: &Screen, x: u16| s.tabs.as_ref().map_or(x % 8 == 0, |t| t.get(usize::from(x)).copied().unwrap_or(false));
        let mut n = count;
        while cx > 0 && n > 0 {
            n -= 1;
            loop {
                cx -= 1;
                if cx == 0 || stop(&self.screen, cx) {
                    break;
                }
            }
        }
        self.screen.grid_mut().col_set(cx);
    }

    // CSI C
    pub fn cuf(&mut self, offset: u16) {
        self.screen.grid_mut().col_inc_clamp(offset);
    }

    // CSI D
    pub fn cub(&mut self, offset: u16) {
        self.screen.grid_mut().col_dec(offset);
    }

    // CSI E
    pub fn cnl(&mut self, offset: u16) {
        self.screen.grid_mut().col_set(0);
        self.screen.grid_mut().row_inc_clamp(offset);
    }

    // CSI F
    pub fn cpl(&mut self, offset: u16) {
        self.screen.grid_mut().col_set(0);
        self.screen.grid_mut().row_dec_clamp(offset);
    }

    // CSI G
    pub fn cha(&mut self, col: u16) {
        self.screen.grid_mut().col_set(col - 1);
    }

    // CSI `  (HPA - Horizontal Position Absolute, equivalent to CHA)
    pub(crate) fn hpa(&mut self, col: u16) {
        self.cha(col);
    }

    // CSI b (REP - Repeat last printed character)
    pub(crate) fn rep(&mut self, count: u16) {
        if let Some(c) = self.screen.last_char {
            for _ in 0..count {
                self.text(c);
            }
        }
    }

    // CSI H
    pub fn cup(&mut self, (row, col): (u16, u16)) {
        self.screen.grid_mut().set_pos(crate::grid::Pos {
            row: row - 1,
            col: col - 1,
        });
    }

    // CSI J
    pub fn ed(&mut self, mode: u16, mut unhandled: impl FnMut(&mut Self)) {
        let attrs = self.screen.attrs;
        match mode {
            0 if self.screen.scroll_on_clear && self.screen.grid().pos() == crate::grid::Pos { row: 0, col: 0 } => {
                let on_row = Self::on_row(&self.screen, &mut self.callbacks);
                self.screen.grid_mut().erase_all_into_history(attrs,on_row);
            }
            0 => self.screen.grid_mut().erase_all_forward(attrs),
            1 => self.screen.grid_mut().erase_all_backward(attrs),
            2 if self.screen.scroll_on_clear => {
                let on_row = Self::on_row(&self.screen, &mut self.callbacks);
                self.screen.grid_mut().erase_all_into_history(attrs,on_row);
            }
            2 => {
                self.screen.grid_mut().erase_all(attrs);
                self.screen.grid_mut().unwrap_above(0);
            }
            3 => {
                let on_row = Self::on_row(&self.screen, &mut self.callbacks);
                self.screen.grid_mut().erase_all_scrollback(on_row);
            }
            _ => unhandled(self),
        }
    }

    // CSI ? J
    pub fn decsed(&mut self, mode: u16, unhandled: impl FnMut(&mut Self)) {
        self.ed(mode, unhandled);
    }

    // CSI K
    pub fn el(&mut self, mode: u16, mut unhandled: impl FnMut(&mut Self)) {
        let attrs = self.screen.attrs;
        match mode {
            0 => self.screen.grid_mut().erase_row_forward(attrs),
            // screen_write_clearstartofline: to the last column it is the whole line
            1 if self.screen.grid().pos().col >= self.screen.grid().size().cols() - 1 => self.screen.grid_mut().erase_row(attrs),
            1 => self.screen.grid_mut().erase_row_backward(attrs),
            2 => self.screen.grid_mut().erase_row(attrs),
            _ => unhandled(self),
        }
    }

    // CSI ? K
    pub fn decsel(&mut self, mode: u16, unhandled: impl FnMut(&mut Self)) {
        self.el(mode, unhandled);
    }

    // CSI L
    pub fn il(&mut self, count: u16) {
        self.screen.grid_mut().insert_lines(count);
    }

    // CSI M
    pub fn dl(&mut self, count: u16) {
        self.screen.grid_mut().delete_lines(count);
    }

    // CSI P
    pub fn dch(&mut self, count: u16) {
        self.screen.grid_mut().delete_cells(count);
    }

    // CSI S
    pub fn su(&mut self, count: u16) {
        let on_row = Self::on_row(&self.screen, &mut self.callbacks);
        self.screen.grid_mut().scroll_up(count, on_row);
    }

    // CSI T
    pub fn sd(&mut self, count: u16) {
        self.screen.grid_mut().scroll_down(count);
    }

    // CSI X
    pub fn ech(&mut self, count: u16) {
        let attrs = self.screen.attrs;
        self.screen.grid_mut().erase_cells(count, attrs);
    }

    // CSI d
    pub fn vpa(&mut self, row: u16) {
        self.screen.grid_mut().row_set(row - 1);
    }

    // CSI ? h
    pub fn decset(
        &mut self,
        params: &vte::Params,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        for param in params {
            let screen = &mut self.screen;
            match param {
                [1] => screen.set_mode(MODE_APPLICATION_CURSOR),
                [6] => screen.grid_mut().set_origin_mode(true),
                [7] => screen.set_xmode(XMODE_WRAP, true),
                [9] => screen.set_mouse_mode(MouseProtocolMode::Press),
                [12] => screen.set_xmode(XMODE_CURSOR_BLINKING | XMODE_CURSOR_BLINKING_SET, true),
                [25] => screen.clear_mode(MODE_HIDE_CURSOR),
                [47] | [1047] => screen.alternate_on(false),
                [1000] => {
                    screen.set_mouse_mode(MouseProtocolMode::PressRelease);
                }
                [1002] => {
                    screen.set_mouse_mode(MouseProtocolMode::ButtonMotion);
                }
                [1003] => screen.set_mouse_mode(MouseProtocolMode::AnyMotion),
                [1004] => screen.set_xmode(XMODE_FOCUS, true),
                [1005] => {
                    screen.set_xmode(XMODE_MOUSE_UTF8, true);
                    screen.set_mouse_encoding(MouseProtocolEncoding::Utf8);
                }
                [1006] => {
                    screen.set_xmode(XMODE_MOUSE_SGR, true);
                    screen.set_mouse_encoding(MouseProtocolEncoding::Sgr);
                }
                [1049] => screen.alternate_on(true),
                [2004] => screen.set_mode(MODE_BRACKETED_PASTE),
                [2026] => screen.set_xmode(XMODE_SYNC, true),
                [2031] => {
                    screen.set_xmode(XMODE_THEME_UPDATES, true);
                    self.callbacks.theme_subscription(&mut self.screen);
                },
                _ => unhandled(self),
            }
        }
    }

    // CSI ? l
    pub fn decrst(
        &mut self,
        params: &vte::Params,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        for param in params {
            let screen = &mut self.screen;
            match param {
                [1] => screen.clear_mode(MODE_APPLICATION_CURSOR),
                [6] => screen.grid_mut().set_origin_mode(false),
                [7] => screen.set_xmode(XMODE_WRAP, false),
                [9] => screen.clear_mouse_mode(MouseProtocolMode::Press),
                [12] => {
                    screen.set_xmode(XMODE_CURSOR_BLINKING, false);
                    screen.set_xmode(XMODE_CURSOR_BLINKING_SET, true);
                }
                [25] => screen.set_mode(MODE_HIDE_CURSOR),
                [47] | [1047] => screen.alternate_off(false),
                [1000] => {
                    screen.clear_mouse_mode(MouseProtocolMode::PressRelease);
                }
                [1002] => {
                    screen.clear_mouse_mode(MouseProtocolMode::ButtonMotion);
                }
                [1003] => {
                    screen.clear_mouse_mode(MouseProtocolMode::AnyMotion);
                }
                [1004] => screen.set_xmode(XMODE_FOCUS, false),
                [1005] => {
                    screen.set_xmode(XMODE_MOUSE_UTF8, false);
                    screen.clear_mouse_encoding(MouseProtocolEncoding::Utf8);
                }
                [1006] => {
                    screen.set_xmode(XMODE_MOUSE_SGR, false);
                    screen.clear_mouse_encoding(MouseProtocolEncoding::Sgr);
                }
                [1049] => screen.alternate_off(true),
                [2004] => screen.clear_mode(MODE_BRACKETED_PASTE),
                [2026] => screen.set_xmode(XMODE_SYNC, false),
                [2031] => screen.set_xmode(XMODE_THEME_UPDATES, false),
                _ => unhandled(self),
            }
        }
    }

    // CSI m
    pub fn sgr(
        &mut self,
        params: &vte::Params,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        // XXX really i want to just be able to pass in a default Params
        // instance with a 0 in it, but vte doesn't allow creating new Params
        // instances
        // input_csi_dispatch_sgr: no parameters is the default cell, link included; SGR 0
        // keeps the link
        if params.is_empty() || self.screen.empty_sgr {
            self.screen.attrs = crate::attrs::Attrs::default();
            self.screen.hyperlink = None;
            return;
        }

        // input_csi_dispatch_sgr: each parameter in turn; 38, 48 and 58 take a colour
        // from the ones after them, a parameter with colons (input_csi_dispatch_sgr_colon)
        // is one colour or underline style, and anything unknown is skipped
        let _ = &mut unhandled;
        let list: alloc::vec::Vec<&[u16]> = params.iter().collect();
        let get = |i: usize| -> Option<u16> { list.get(i).and_then(|p| p.first().copied()) };
        let attrs = &mut self.screen.attrs;
        let set = |attrs: &mut crate::attrs::Attrs, which: u16, c: crate::Color| match which {
            38 => attrs.fgcolor = c,
            48 => attrs.bgcolor = c,
            _ => attrs.ulcolor = c,
        };
        let mut i = 0;
        while i < list.len() {
            let p = list[i];
            if p.len() > 1 {
                // a colon list: up to seven parts
                if p.len() < 8 {
                    let n = p.len();
                    match (p[0], p[1]) {
                        (4, style) if n == 2 && style <= 5 => attrs.set_underline_style(style as u8),
                        (38 | 48 | 58, 2) if n >= 3 => {
                            let at = if n == 5 { 2 } else { 3 };
                            if n >= at + 3 && p[at..at + 3].iter().all(|v| *v <= 255) {
                                set(attrs, p[0], crate::Color::Rgb(p[at] as u8, p[at + 1] as u8, p[at + 2] as u8));
                            }
                        }
                        (38 | 48 | 58, 5) if n >= 3 => match p[2] {
                            c @ 0..=255 => set(attrs, p[0], crate::Color::palette(c as u8)),
                            _ if p[0] != 58 => set(attrs, p[0], crate::Color::Default),
                            _ => {}
                        },
                        _ => {}
                    }
                }
                i += 1;
                continue;
            }
            let n = p[0];
            if matches!(n, 38 | 48 | 58) {
                i += 1;
                match get(i) {
                    Some(2) => {
                        let rgb = (get(i + 1), get(i + 2), get(i + 3));
                        if let (Some(r @ 0..=255), Some(g @ 0..=255), Some(b @ 0..=255)) = rgb {
                            set(attrs, n, crate::Color::Rgb(r as u8, g as u8, b as u8));
                            i += 3;
                        }
                    }
                    Some(5) => {
                        match get(i + 1) {
                            Some(c @ 0..=255) => set(attrs, n, crate::Color::palette(c as u8)),
                            _ if n != 58 => set(attrs, n, crate::Color::Default),
                            _ => {}
                        }
                        i += 1;
                    }
                    _ => {}
                }
                i += 1;
                continue;
            }
            match n {
                0 => *attrs = crate::attrs::Attrs::default(),
                1 => attrs.set_bold(),
                2 => attrs.set_dim(),
                3 => attrs.set_italic(true),
                4 => attrs.set_underline_style(1),
                5 | 6 => attrs.set_blink(true),
                7 => attrs.set_inverse(true),
                8 => attrs.set_hidden(true),
                9 => attrs.set_strikethrough(true),
                21 => attrs.set_underline_style(2),
                22 => attrs.set_normal_intensity(),
                23 => attrs.set_italic(false),
                24 => attrs.set_underline_style(0),
                25 => attrs.set_blink(false),
                27 => attrs.set_inverse(false),
                28 => attrs.set_hidden(false),
                29 => attrs.set_strikethrough(false),
                30..=37 => attrs.fgcolor = crate::Color::Idx((n - 30) as u8),
                39 => attrs.fgcolor = crate::Color::Default,
                40..=47 => attrs.bgcolor = crate::Color::Idx((n - 40) as u8),
                49 => attrs.bgcolor = crate::Color::Default,
                53 => attrs.set_overline(true),
                55 => attrs.set_overline(false),
                59 => attrs.ulcolor = crate::Color::Default,
                90..=97 => attrs.fgcolor = crate::Color::Idx((n - 82) as u8),
                100..=107 => attrs.bgcolor = crate::Color::Idx((n - 92) as u8),
                _ => {}
            }
            i += 1;
        }
    }

    // CSI r
    pub fn decstbm(&mut self, (top, bottom): (u16, u16)) {
        self.screen
            .grid_mut()
            .set_scroll_region(top - 1, bottom - 1);
    }
}

/// utf8_stravis(VIS_OCTAL|VIS_CSTYLE): complete UTF-8 characters and printable ASCII
/// pass (a backslash doubled), other bytes become C-style or octal escapes.
fn stravis(src: &[u8]) -> String {
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        let n = match c { 0xc2..=0xdf => 2, 0xe0..=0xef => 3, 0xf0..=0xf4 => 4, _ => 1 };
        if n > 1 {
            if let Some(ch) = src.get(i..i + n).and_then(|b| core::str::from_utf8(b).ok()) {
                out.push_str(ch);
                i += n;
                continue;
            }
        }
        match c {
            b'\\' => out.push_str("\\\\"),
            b' ' | b'\t' | b'\n' => out.push(char::from(c)),
            _ if c.is_ascii_graphic() => out.push(char::from(c)),
            b'\r' => out.push_str("\\r"),
            0x08 => out.push_str("\\b"),
            0x07 => out.push_str("\\a"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0 => out.push_str(if src.get(i + 1).is_some_and(|b| (b'0'..=b'7').contains(b)) { "\\000" } else { "\\0" }),
            _ => { let _ = core::fmt::Write::write_fmt(&mut out, format_args!("\\{:03o}", c)); }
        }
        i += 1;
    }
    out
}

/// The emoji a skin tone modifier combines with (utf8_should_combine).
const EMOJI_MODIFIER_BASE: [u32; 72] = [
    0x1F44B, 0x1F44C, 0x1F44D, 0x1F44E, 0x1F44F, 0x1F450, 0x1F466, 0x1F467, 0x1F468, 0x1F469,
    0x1F46E, 0x1F470, 0x1F471, 0x1F472, 0x1F473, 0x1F474, 0x1F475, 0x1F476, 0x1F477, 0x1F478,
    0x1F47C, 0x1F481, 0x1F482, 0x1F483, 0x1F485, 0x1F486, 0x1F487, 0x1F4AA, 0x1F575, 0x1F57A,
    0x1F590, 0x1F595, 0x1F596, 0x1F645, 0x1F646, 0x1F647, 0x1F64B, 0x1F64C, 0x1F64D, 0x1F64E,
    0x1F64F, 0x1F6B4, 0x1F6B5, 0x1F6B6, 0x1F926, 0x1F937, 0x1F938, 0x1F939, 0x1F93D, 0x1F93E,
    0x1F9B5, 0x1F9B6, 0x1F9B8, 0x1F9B9, 0x1F9CD, 0x1F9CE, 0x1F9CF, 0x1F9D1, 0x1F9D2, 0x1F9D3,
    0x1F9D4, 0x1F9D5, 0x1F9D6, 0x1F9D7, 0x1F9D8, 0x1F9D9, 0x1F9DA, 0x1F9DB, 0x1F9DC, 0x1F9DD,
    0x1F9DE, 0x1F9DF,
];

#[cfg(test)]
mod tests {
    use super::{Screen, WrappedScreen};

    #[derive(Clone, Default)]
    struct Scrolls(Vec<String>);
    impl crate::Callbacks for Scrolls {
        fn on_scroll(&mut self, row: crate::capture::RowContents<'_>, alt: bool) {
            self.0.push(alloc::format!("{:?} {alt}", row.0));
        }
    }

    /// `text_ascii` is the printable ASCII case of `text_any`: both give the same screen
    /// (cells, wide state around the cell, links, line flags and wraps, the scroll it
    /// causes) after random output with wide, combining and RGB cells, links, charsets,
    /// scroll regions and the alternate screen.
    #[test]
    fn text_ascii_matches_text_any() {
        const SETUP: [&str; 34] = [
            "\u{3042}", "\u{1F600}", "e\u{301}", "\u{1F1F0}\u{1F1F7}", "\u{1F1F0}", "\u{200D}", "\u{FE0F}", "\u{1F44D}\u{1F3FD}",
            "\x1b[38;2;1;2;3m", "\x1b[48;2;4;5;6m", "\x1b[0m", "\x1b[31;1m", "\x1b]8;;http://x\x07", "\x1b]8;;\x07",
            "\r\n", "\n", "\r", "\x1bD", "\x1bM", "\x1b[2;4r", "\x1b[r", "\x1b(0", "\x1b(B", "\x0e", "\x0f",
            "\x1b[?1049h", "\x1b[?1049l", "\t", "\x08", "\x1b[@", "\x1b[P", "\x1b[3X", "\x1b[K", "\x1b[2J",
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut checked = 0;
        for _ in 0..300 {
            let rows = core::num::NonZeroU16::new(1 + next(6) as u16).unwrap();
            let cols = core::num::NonZeroU16::new(1 + next(12) as u16).unwrap();
            let mut ws = WrappedScreen { screen: Screen::new(crate::grid::Size { rows, cols }, next(3) as usize * 5), callbacks: Scrolls::default() };
            ws.screen.variation_wide = next(2) == 0;
            let mut vte = vte::Parser::new();
            for _ in 0..200 {
                match next(20) {
                    0..=7 => {
                        let c = char::from(b' ' + next(95) as u8);
                        if !ws.screen.wrap_mode() || ws.screen.insert_mode() { continue; }
                        // a run of printable characters grows its line in one step, `text_any`
                        // a cell at a time: the same for the first character of a run
                        ws.screen.end_print_run();
                        let mut other = WrappedScreen { screen: ws.screen.clone(), callbacks: ws.callbacks.clone() };
                        ws.text_ascii(c);
                        ws.screen.end_print_run();
                        other.text_any(c);
                        assert_eq!(alloc::format!("{:?}", ws.screen), alloc::format!("{:?}", other.screen));
                        assert_eq!(ws.callbacks.0, other.callbacks.0);
                        checked += 1;
                    }
                    8 => {
                        let (r, c) = (1 + next(7), 1 + next(13));
                        vte.advance(&mut ws, alloc::format!("\x1b[{r};{c}H").as_bytes());
                    }
                    9 => vte.advance(&mut ws, [&b"\x1b[4h"[..], b"\x1b[4l", b"\x1b[?7l", b"\x1b[?7h"][next(4) as usize]),
                    _ => vte.advance(&mut ws, SETUP[next(SETUP.len() as u64) as usize].as_bytes()),
                }
            }
        }
        assert!(checked > 5000, "{checked}");
    }
}
