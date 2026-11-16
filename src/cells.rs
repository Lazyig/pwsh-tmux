//! Shared screen-cell types used by rendering, copy mode and the status line.

pub use vt100::Color;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub attrs: u8,
}

pub const BOLD: u8 = 1;
pub const DIM: u8 = 2;
pub const ITALIC: u8 = 4;
pub const UNDERLINE: u8 = 8;
pub const INVERSE: u8 = 16;
pub const BLINK: u8 = 32;
pub const STRIKE: u8 = 64;

#[derive(Clone, PartialEq, Debug)]
pub struct Cell {
    /// Grapheme text; " " for blank, empty for the right half of a wide character.
    pub text: String,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { text: " ".into(), style: Style::default() }
    }
}

impl Cell {
    pub fn is_continuation(&self) -> bool {
        self.text.is_empty()
    }
}

/// One terminal row. `wrapped` means the row continues on the next one (soft wrap).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Line {
    pub cells: Vec<Cell>,
    pub wrapped: bool,
}

impl Line {
    /// Text of the row with trailing blanks removed ([`text_at`] is the fast path).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn text(&self) -> String {
        let s: String = self.cells.iter().map(|c| c.text.as_str()).collect();
        s.trim_end().to_string()
    }
}

pub fn cell_from_vt(c: &vt100::Cell) -> Cell {
    let mut attrs = 0;
    for (on, bit) in [(c.bold(), BOLD), (c.dim(), DIM), (c.italic(), ITALIC), (c.underline(), UNDERLINE), (c.inverse(), INVERSE)] {
        if on {
            attrs |= bit;
        }
    }
    let text = if c.is_wide_continuation() {
        String::new()
    } else if c.has_contents() {
        c.contents().to_string()
    } else {
        " ".into()
    };
    Cell { text, style: Style { fg: c.fgcolor(), bg: c.bgcolor(), attrs } }
}

/// Every row of a screen including its scrollback, oldest first.
pub fn snapshot(screen: &vt100::Screen) -> Vec<Line> {
    (0..screen.history_rows_len()).map(|i| line_at(screen, i)).collect()
}

/// Row `index` of [`snapshot`] (oldest history row = 0) without building the others.
pub fn line_at(screen: &vt100::Screen, index: usize) -> Line {
    let cols = screen.size().1.get();
    match screen.history_row(index) {
        Some(row) => Line { cells: (0..cols).map(|c| row.cell(c).map(cell_from_vt).unwrap_or_default()).collect(), wrapped: row.wrapped() },
        None => Line { cells: vec![Cell::default(); cols as usize], wrapped: false },
    }
}

/// Plain text of row `index` (trailing blanks removed, as [`Line::text`]) and whether it wraps.
pub fn text_at(screen: &vt100::Screen, index: usize) -> (String, bool) {
    let cols = screen.size().1.get();
    let mut s = String::new();
    let Some(row) = screen.history_row(index) else { return (s, false) };
    row.write_plain(&mut s, cols);
    let len = s.trim_end().len();
    s.truncate(len);
    (s, row.wrapped())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU16;

    #[test]
    fn row_access_matches_cell_snapshot() {
        // text_at / line_at (no screen clone) must agree with the cell-by-cell view
        let nz = |n| NonZeroU16::new(n).unwrap();
        let mut p = vt100::Parser::new(nz(3), nz(10), 50);
        p.process("a  b  \r\n\x1b[31m한글\x1b[0m x\r\n".as_bytes());
        p.process(b"0123456789wrapped\r\nlast");
        let lines = snapshot(p.screen());
        assert_eq!(lines.len(), p.screen().history_rows_len());
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(line, &line_at(p.screen(), i));
            assert_eq!(text_at(p.screen(), i), (line.text(), line.wrapped), "row {i}");
        }
        assert!(lines.iter().any(|l| l.wrapped), "a soft-wrapped row is covered");
        // reading history must not move a scrolled view
        p.screen_mut().set_scrollback(1);
        let _ = snapshot(p.screen());
        assert_eq!(p.screen().scrollback(), 1);
    }

    #[test]
    fn snapshot_includes_history() {
        let mut p = vt100::Parser::new(NonZeroU16::new(3).unwrap(), NonZeroU16::new(10).unwrap(), 100);
        for i in 0..7 {
            p.process(format!("line{i}\r\n").as_bytes());
        }
        let lines = snapshot(p.screen());
        let texts: Vec<String> = lines.iter().map(Line::text).collect();
        assert_eq!(texts, vec!["line0", "line1", "line2", "line3", "line4", "line5", "line6", ""]);
        assert_eq!(p.screen().scrollback(), 0, "live screen untouched");
    }

    #[test]
    fn history_compaction_preserves_styled_clusters_through_capture_and_reflow() {
        let nz = |n| NonZeroU16::new(n).unwrap();
        let mut p = vt100::Parser::new(nz(4), nz(80), 100);
        let text = "ASCII 한글 e\u{301} ❤️ 👩\u{200d}💻";
        for _ in 0..20 {
            p.process(format!("\x1b[1;3;38;2;12;34;56m{text}\x1b[0m\r\n").as_bytes());
        }
        let before = snapshot(p.screen());
        assert_eq!(before.len(), 21);
        for row in &before[..20] {
            assert_eq!(row.text(), text);
            assert_eq!(row.cells[0].style.fg, Color::Rgb(12, 34, 56));
            assert_eq!(row.cells[0].style.attrs & (BOLD | ITALIC), BOLD | ITALIC);
        }
        p.set_size(nz(4), nz(12));
        p.set_size(nz(4), nz(80));
        assert_eq!(snapshot(p.screen()), before);
        assert_eq!(p.screen().scrollback(), 0, "history capture must not move the live view");
    }

    #[test]
    fn resize_reflows_history_and_preserves_styles_and_clusters() {
        let nz = |n| NonZeroU16::new(n).unwrap();
        let mut p = vt100::Parser::new(nz(4), nz(12), 100);
        p.process("\x1b[31mabcdef한글é\x1b[0m\r\nnext".as_bytes());
        p.set_size(nz(4), nz(5));
        let small = snapshot(p.screen());
        assert!(small.len() > 4);
        let text: String = small.iter().flat_map(|r| r.cells.iter().map(|c| c.text.as_str())).collect();
        assert!(text.contains("abcde"));
        assert!(text.contains("한글"));
        assert_eq!(small[0].cells[0].style.fg, Color::Idx(1));
        p.set_size(nz(4), nz(12));
        let large = snapshot(p.screen());
        assert!(large.iter().any(|r| r.text() == "abcdef한글é"));
        assert!(large.iter().any(|r| r.text() == "next"));
    }

    #[test]
    fn resize_reflow_is_bounded_and_alternate_screen_stays_fixed() {
        let nz = |n| NonZeroU16::new(n).unwrap();
        let mut p = vt100::Parser::new(nz(3), nz(100), 5);
        p.process("x".repeat(290).as_bytes());
        p.set_size(nz(3), nz(1));
        assert!(snapshot(p.screen()).len() <= 8);
        p.set_size(nz(3), nz(10));
        p.process(b"\x1b[?1049habcdefghi\x1b[2;3H");
        p.set_size(nz(3), nz(5));
        assert_eq!(p.screen().cursor_position(), (1, 2));
        assert!(p.screen().contents().starts_with("abcde"));
        assert!(!p.screen().contents().contains("fghi"));
    }

    #[test]
    fn scroll_on_clear_preserves_used_rows_and_can_be_disabled() {
        let nz = |n| NonZeroU16::new(n).unwrap();
        let mut p = vt100::Parser::new(nz(4), nz(12), 10);
        p.screen_mut().set_scroll_on_clear(true);
        p.process(b"first\r\nsecond\x1b[2J\x1b[Hnew");
        let rows = snapshot(p.screen());
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0].text(), "first");
        assert_eq!(rows[1].text(), "second");
        assert_eq!(rows[2].text(), "new");
        p.screen_mut().set_scroll_on_clear(false);
        p.process(b"\x1b[2J");
        assert_eq!(snapshot(p.screen()).len(), 6);
        p.screen_mut().set_scroll_on_clear(true);
        p.process(b"\x1b[?1049halternate\x1b[2J\x1b[?1049l");
        assert_eq!(snapshot(p.screen()).len(), 6, "alternate screen has no history");
    }

    #[test]
    fn sparse_rows_edit_like_dense_ones() {
        // vt100 rows store cells up to the last one written only (vendor PATCHES.md):
        // inserts, deletes, erases and resizes must see the empty cells past them
        let nz = |n| NonZeroU16::new(n).unwrap();
        let mut p = vt100::Parser::new(nz(6), nz(20), 0);
        p.process(b"abc\x1b[1;2H\x1b[2@");
        p.process(b"\x1b[2;10Hx\x1b[2;3H\x1b[4@\x1b[2;2H\x1b[2P");
        p.process(b"\x1b[3;1H0123456789abcdefghij\x1b[3;5H\x1b[3@");
        p.process(b"\x1b[4;5Hy\x1b[4;9H\x1b[3P\x1b[4;1H\x1b[2@");
        p.process(b"\x1b[5;1H\x1b[41m\x1b[K\x1b[0m");
        let rows: Vec<String> = p.screen().rows(0, 20).collect();
        assert_eq!(&rows[..4], ["a  bc", "           x", "0123   456789abcdefg", "      y"]);
        assert_eq!(p.screen().cell(4, 19).map(vt100::Cell::bgcolor), Some(vt100::Color::Idx(1)));
        assert!(p.screen().cell(5, 19).is_some_and(|c| !c.has_contents()));
        // a resized row has the new columns, empty
        p.screen_mut().set_size(nz(6), nz(30));
        assert!(p.screen().cell(0, 29).is_some_and(|c| !c.has_contents()));
        p.process(b"\x1b[1;30Hz\x1b[2J\x1b[1;1H\x1b[1@");
        assert_eq!(p.screen().contents(), "");
        // a huge screen with a little output (a floating pane of 10000x10000)
        let mut p = vt100::Parser::new(nz(10000), nz(10000), 100);
        for i in 0..20000 {
            p.process(format!("{i}\r\n").as_bytes());
        }
        p.process(b"\x1b[5;5H\x1b[3@q\x1b[2P");
        assert_eq!(p.screen().cell(4, 4).map(|c| c.contents().to_string()).as_deref(), Some("q"));
        assert_eq!(p.screen().cell(9999, 9999).map(vt100::Cell::has_contents), Some(false));
    }
}
