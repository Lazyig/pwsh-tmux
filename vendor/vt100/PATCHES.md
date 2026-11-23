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
