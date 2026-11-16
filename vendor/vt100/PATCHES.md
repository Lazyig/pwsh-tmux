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

Local addition: OSC 133 marks are parsed in vt100, with callbacks still receiving
OSC 133 for pane events. Rows retain marks and their existing history timestamps.
OSC 8 links and original HT spans use an optional boxed map on each row; normal
cells remain 32 bytes, and ordinary rows allocate no metadata. Link strings and
identities are shared with Arc; named links use a weak lookup table, anonymous
links do not allocate lookup entries. RowContents exposes metadata to snapshots,
capture and rendering. Edits invalidate/shift entries and history compaction and
reflow retain them. Parser regressions are in lib.rs::metadata_tests.
