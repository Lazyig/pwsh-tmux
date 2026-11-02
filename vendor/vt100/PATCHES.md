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
Current screen rows remain dense; editing a compact row expands it on demand.
Read-only cell access does not expand compact rows. Reflow compacts its history
output as well. This does not lower the history limit or change cell capacity.

Project regression tests live in src/cells.rs. Run the bundled unit/doc tests
with `python tools/test_vt100.py` from the project root. Review these changes
when updating vt100.
