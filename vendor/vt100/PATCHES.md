Based on vt100 0.18.1 from crates.io (MIT; see LICENSE).

Local change: src/grid.rs reflows normal-screen logical lines when the column
count changes. Upstream truncates columns. Reflow preserves cell attributes,
wide glyphs, combining clusters and cursor positions, and bounds the retained
rows by the configured history capacity plus screen height. Alternate-screen
grids retain the upstream fixed-layout resize behavior.

Local change: src/screen.rs adds opt-in scroll-on-clear, enabled for project
panes according to the pane option. ED 2, and ED 0 at the home position, preserve
used normal-screen rows in bounded history. Alternate-screen clearing stays
unchanged. The setting survives terminal reset.

Local change: history rows use lossless indexed cell storage in src/row_cells.rs.
Default-style ASCII cells share an immutable table; other cells use a bounded
per-row palette that preserves complete contents, attributes and wide-cell flags.
Rows with too many distinct cells or no storage saving retain dense storage.
Current screen rows are sparse until written: a new, reset or default-cleared row
stores no cells, and editing a cell stores the cells up to it only (inserts and
deletes shift the stored cells only), so a huge, mostly empty screen (a floating
pane of up to 10000x10000) stays small like tmux's lazily grown grid lines.
Clearing with a colour or rewriting a whole row makes it dense; editing a compact
row expands it on demand.
Read-only cell access does not expand compact rows. Reflow compacts its history
output as well. This does not lower the history limit or change cell capacity.
Compacting builds the palette (in order of first appearance) straight from sparse
or dense cells through a 256-slot hash; a row that scrolls into the history
compact hands its emptied cell storage (up to 1024 cells) to the new row that
replaces it. Trimming and truncating a compact row keep it compact. Reflow moves
a line that fits the new width whole when nothing in it would change
(grid_reflow_move: not wrapped, not the cursor's or saved cursor's row, no tabs or
links, every wide cell paired with its continuation), and trims the unwrapped
rows it puts in the history like rows that scroll there. Tests:
grid::reflow_tests, row_cells::tests.

Local addition: `Screen::history_rows_len` / `Screen::history_row` (backed by
`Grid::absolute_len` / `Grid::absolute_row`) and `RowContents::cell`, `wrapped`
and `write_plain` read any history row by absolute index without cloning the
screen or moving its scrollback view. Used by capture-pane and copy mode.
`Screen::saved_row` reads the normal screen while the alternate screen is
active (capture-pane -a).

Project regression tests live in src/cells.rs. Run the bundled unit/doc tests
with `python tools/test_vt100.py` from the project root. Review these changes
when updating vt100.

Local addition: mode and state tracking for tmux format variables. `Screen`
records IRM (CSI 4 h/l), DECAWM (?7, on by default), cursor blinking (?12 and
DECSCUSR, as tmux), very visible cursor (CSI 34 h/l), focus reporting (?1004),
the 1005/1006 mouse encodings as separate flags, synchronized output (?2026) and
theme updates (?2031); only the reporting changes, not how output is drawn. It
also keeps the cursor saved by DECSET 1049 (`alternate_saved_cursor`), tab stops
set with HTS (ESC H) and cleared with TBC (CSI g), which HT now follows (reset to
every 8 columns when the width changes), and exposes `origin_mode` and
`scroll_region`. `Grid` counts rows added to and dropped from history and history
clears/rewraps (`history_counters`); RIS keeps those counts.

Local addition: tmux 3.8 grid metadata. OSC 133 A/N/P, B/I, C and D set the
cursor row's ROW_* flags (GRID_LINE_START_PROMPT and friends; callbacks still get
OSC 133 for the pane events); erasing a row keeps them, reflow keeps them on the
row a line starts on. OSC 8 links (hyperlinks.c: named links shared by id and
URI, anonymous links always new, URIs over 1024 bytes dropped) and HT tab cells
(GRID_FLAG_TAB: HT over up to 32 blank cells that look alike keeps the tab with
the first cell's style and link; a tab's own cells are content to a later HT) live
in an optional boxed map per row, so normal cells stay 32 bytes and ordinary rows
allocate nothing. URIs and ids are vis-encoded (utf8_stravis) before the 1024 byte
limit; dead named links are swept once the lookup table doubles. `CSI m` with no
parameter resets the link (vte reports it as 0, so the parser notes it), `CSI 0 m`
keeps it. Rewriting a cell drops its entry and turns a tab covering it into
spaces; an insert or delete inside a tab keeps the part before it. Reflow counts
widths as grid_reflow does (a tab is a cell of its width and each padding cell
another column, a row that wrapped early ends at its last used cell, a split whose
first cell is wider than the row leaves an empty row, the rest of a row a join
stopped in is a line of its own), so the tab stays a tab at any width; the padding
has no cells here, which only moves where text after a tab is drawn on a row the
padding went on to. ROW_HYPERLINK / ROW_EXTENDED (also set by wide, multi-byte and
RGB cells) stay set until the row is cleared, as tmux's line flags; EL/ED/ECH over
a whole line clear its flags (EL keeps the OSC 133 marks) and the line above's
wrap. Rows that a shrink moves into the history get no time; the top line of a
scroll region not at the top goes into the history (grid_scroll_history_region);
RIS keeps the history and, with scroll-on-clear, moves the screen into it; IRM
makes text and tabs insert. `Screen::reset_hyperlinks` is screen_reset_hyperlinks
(clear-history -H). Tests: lib.rs::metadata_tests.

Local change: a character that combines with the cell before the cursor and makes
it wide (a second regional indicator, an emoji modifier, VS16 with
variation-selector-always-wide) widens the cell where it is, as
screen_write_combine does. In the last column the second half is off the screen:
the cell stays there, wide, with no continuation cell after it, the cursor does
not move and the client draws it blank. Upstream moved the character to the next
row, which after a scroll read the wrong row and left such a cell behind; writing
over it then panicked. Code that tidies the cells around a wide cell
(`text_cleanup`) tolerates the missing continuation and clears only continuation
cells. Reflow gives such a cell its continuation and keeps a cursor after it on
the second half (the offset grid_unwrap_position keeps); a row of the fixed
alternate screen that grows shows the second half too. A zero width character
joins the cell before the cursor only when that cell is as wide as the distance
to the cursor (tmux drops it with the cursor on a wide cell's second half or
after a wide cell in the last column). Tests:
lib.rs::metadata_tests, src/cells.rs and tests/commands.rs with the fuzzed
outputs in tests/fixtures/pane-output-crash.

Local change (speed only): outside control strings and not right after ESC or
ESC [, `Parser::process` hands vte the bytes up to the next ESC or 0xC2 in one
call instead of one byte at a time. A run never starts at 0xC2 or a UTF-8
continuation byte, because vte 0.15 handles a character split over calls
differently from one it decodes whole (it prints a UTF-8 C1 control it
completes from a split but executes a whole one, and drops the bytes after a
character completed from a split). Printable ASCII with wrapping on and insert
mode off goes through `WrappedScreen::text_ascii`, the one-column case of
`text` with a single row lookup. Neither changes the result. Tests:
parser::tests, screen::tests.

Local change: each row keeps tmux's gl->cellsize (`Row::cellsize`, read with
`RowContents::cellsize`), the cells grid_expand_line gives a line: none until a
cell is written, then a quarter, half or all of the width. Printed cells grow it
(a run of printable ASCII in one step when the run ends, as
screen_write_collect_add writes it with one grid_set_cells, so a run ending at a
quarter of the width takes the next size; a character of a run only notes where
the run ends; other characters and DECALN a cell or line at a time, a tab its
padding cells one at a time and then its own cell, as screen_write_cell sets
them), ICH and DCH grow it to the width (grid_move_cells), ECH, EL and EL 1 with
a background colour to the cells cleared in one step (grid_clear); with the
default background they clear only what the line has, so a line redrawn with CR
and EL keeps its size. Clearing a whole line (EL 2, ED, a scroll) empties it, or
gives it the width with a background colour (grid_empty_line). Reflow keeps the
size of a line moved across, gives the first part of a split line exactly its
cells, the rest of a line a join took part of exactly what is left, and writes
the other parts of a split line and the cells joined onto a line a cell at a
time (grid_reflow_split and _join with grid_set_cell), so a part of a quarter or
half of the new width stays there.
EL 2 on a line with no cells leaves it alone only while its size is 0
(screen_write_clearline). capture-pane -N, -e and -R and history_bytes read it.
Tests: lib.rs::metadata_tests (sizes from tmux 3.8's capture-pane -R).

Local change: erasing and blank cells as tmux 3.8 makes them. ED, EL and ECH
clear with the background colour of the current style only (grid_clear_cell:
no attributes, foreground or underline colour; `Attrs::blank`); upstream copied
the whole style. ICH, DCH, IL, DL, SU, SD, a line feed (LF, VT, FF, IND, NEL)
and RI bring in cells and rows with that background (tmux passes `bg` to all of
them); a wrap scrolls in a default row (screen_write_linefeed(ctx, 1, 8)). With
the default background rows stay empty and cheap. IL and DL with the cursor
outside the scroll region move the rows from the cursor to the bottom of the
screen (grid_view_insert_lines/delete_lines) instead of doing nothing.
DECSTBM is screen_write_scrollregion: both rows are clamped to the screen, a
region of less than two rows is ignored with the cursor left where it is, and a
valid one sends the cursor to 0,0 whatever origin mode says (upstream reset the
region for a bad one and homed to the region's top). vte reports an empty and
a 0 parameter alike, so `CSI 3;0r` is the whole-height default here where tmux
reads 0 as 1 and ignores it. The scroll region, origin mode and the cursor
DECSC saved belong to the screen in tmux, so they carry over to and from the
alternate screen (`Grid::take_screen_state`), and a resize resets the region
only when the height changes (screen_resize_y). Width: utf8.c's built-in table
(`utf8_default_width_cache`, src/tmux_width.rs, `default_width`) comes after
codepoint-widths and before the Unicode width, so U+261D, U+26F9, U+270C,
U+270D, U+1F3CB, U+1F3CC, U+1F574, U+1F575 and U+1F590 are two columns as in
tmux 3.8 (the regional indicators, one column, already agree). Tests:
lib.rs::tmux_screen_tests, tmux_width::tests.
