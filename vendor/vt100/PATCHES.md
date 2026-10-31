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

The surrounding upstream sources are unchanged. Project regression tests live
in src/cells.rs. Review this patch when updating vt100.
