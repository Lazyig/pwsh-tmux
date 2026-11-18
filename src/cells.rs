//! Shared screen-cell types used by rendering, copy mode and the status line.

pub use vt100::Color;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    /// The underline colour (tmux `us`).
    pub us: Color,
    pub attrs: u16,
}

pub const BOLD: u16 = 1;
pub const DIM: u16 = 2;
pub const ITALIC: u16 = 4;
pub const UNDERLINE: u16 = 8;
pub const INVERSE: u16 = 16;
pub const BLINK: u16 = 32;
pub const STRIKE: u16 = 64;
pub const HIDDEN: u16 = 128;
pub const OVERLINE: u16 = 256;
/// GRID_ATTR_CHARSET: the text is DEC special graphics (line drawing) characters.
pub const ACS: u16 = 512;
/// GRID_ATTR_UNDERSCORE_2 to _5.
pub const DOUBLE_UNDERLINE: u16 = 1024;
pub const CURLY_UNDERLINE: u16 = 2048;
pub const DOTTED_UNDERLINE: u16 = 4096;
pub const DASHED_UNDERLINE: u16 = 8192;
/// Any underline.
pub const ALL_UNDERLINE: u16 = UNDERLINE | DOUBLE_UNDERLINE | CURLY_UNDERLINE | DOTTED_UNDERLINE | DASHED_UNDERLINE;

/// tty_acs_table: the Unicode character for a DEC special graphics character.
pub fn acs_char(c: char) -> Option<&'static str> {
    Some(match c {
        '+' => "→", ',' => "←", '-' => "↑", '.' => "↓", '0' => "▮", '`' => "◆", 'a' => "▒", 'b' => "␉",
        'c' => "␌", 'd' => "␍", 'e' => "␊", 'f' => "°", 'g' => "±", 'h' => "␤", 'i' => "␋", 'j' => "┘",
        'k' => "┐", 'l' => "┌", 'm' => "└", 'n' => "┼", 'o' => "⎺", 'p' => "⎻", 'q' => "─", 'r' => "⎼",
        's' => "⎽", 't' => "├", 'u' => "┤", 'v' => "┴", 'w' => "┬", 'x' => "│", 'y' => "≤", 'z' => "≥",
        '{' => "π", '|' => "≠", '}' => "£", '~' => "·",
        _ => return None,
    })
}

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
/// `flags` are vt100's ROW_* line flags (OSC 133 marks, hyperlinks), `time` when the
/// row entered the history (0 if not), `metadata` the row's hyperlinks and tabs by column.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Line {
    pub cells: Vec<Cell>,
    pub wrapped: bool,
    pub flags: u8,
    pub time: u64,
    pub metadata: std::collections::BTreeMap<u16, vt100::CellMetadata>,
}

impl Line {
    /// The tab (GRID_FLAG_TAB) covering column `x`: its first column and width.
    pub fn tab_at(&self, x: usize) -> Option<(usize, usize)> {
        let (&at, m) = self.metadata.range(..=u16::try_from(x).ok()?).next_back()?;
        let (at, w) = (usize::from(at), usize::from(m.tab_width));
        (x < at + w).then_some((at, w))
    }

    /// GRID_FLAG_PADDING: the right half of a wide character or the rest of a tab.
    pub fn is_padding(&self, x: usize) -> bool {
        self.cells.get(x).is_some_and(Cell::is_continuation) || self.tab_at(x).is_some_and(|(at, _)| at != x)
    }

    /// What cell `x` gives to copied text: a tab for a tab, nothing for padding.
    pub fn cell_text(&self, x: usize) -> &str {
        match self.tab_at(x) {
            Some((at, _)) if at == x => "\t",
            Some(_) => "",
            None => self.cells.get(x).map_or("", |c| c.text.as_str()),
        }
    }

    /// Columns up to the end of the last tab (a tab is not a trailing blank; one wider
    /// than the row, kept by a reflow, ends with it).
    pub fn tabs_end(&self) -> usize {
        self.metadata.iter().filter(|(_, m)| m.tab_width > 0).map(|(&x, m)| usize::from(x) + usize::from(m.tab_width)).max().unwrap_or(0).min(self.cells.len())
    }

    /// format_grid_hyperlink: the link of the cell at `x`, from the start of a wide
    /// character or tab.
    pub fn hyperlink_at(&self, mut x: usize) -> Option<&std::sync::Arc<vt100::Hyperlink>> {
        while x > 0 && self.is_padding(x) {
            x -= 1;
        }
        self.metadata.get(&u16::try_from(x).ok()?)?.hyperlink.as_ref()
    }

    /// Text of the row with trailing blanks removed ([`text_at`] is the fast path).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn text(&self) -> String {
        let s: String = self.cells.iter().map(|c| c.text.as_str()).collect();
        // only blank cells: a C1 character such as U+0085 is white space to Rust but
        // is kept in its cell
        s.trim_end_matches(' ').to_string()
    }
}

/// The text and style [`cell_from_vt`] gives, without allocating.
pub fn cell_parts_from_vt(c: &vt100::Cell) -> (&str, Style) {
    let mut attrs = 0;
    for (on, bit) in [
        (c.bold(), BOLD), (c.dim(), DIM), (c.italic(), ITALIC), (c.inverse(), INVERSE), (c.blink(), BLINK), (c.hidden(), HIDDEN),
        (c.strikethrough(), STRIKE), (c.overline(), OVERLINE), (c.charset(), ACS),
    ] {
        if on {
            attrs |= bit;
        }
    }
    attrs |= match c.underline_style() {
        0 => 0,
        1 => UNDERLINE,
        2 => DOUBLE_UNDERLINE,
        3 => CURLY_UNDERLINE,
        4 => DOTTED_UNDERLINE,
        _ => DASHED_UNDERLINE,
    };
    let text = if c.is_wide_continuation() {
        ""
    } else if c.has_contents() {
        c.contents()
    } else {
        " "
    };
    (text, Style { fg: c.fgcolor(), bg: c.bgcolor(), us: c.underline_color(), attrs })
}

pub fn cell_from_vt(c: &vt100::Cell) -> Cell {
    let mut attrs = 0;
    for (on, bit) in [
        (c.bold(), BOLD), (c.dim(), DIM), (c.italic(), ITALIC), (c.inverse(), INVERSE), (c.blink(), BLINK), (c.hidden(), HIDDEN),
        (c.strikethrough(), STRIKE), (c.overline(), OVERLINE), (c.charset(), ACS),
    ] {
        if on {
            attrs |= bit;
        }
    }
    attrs |= match c.underline_style() {
        0 => 0,
        1 => UNDERLINE,
        2 => DOUBLE_UNDERLINE,
        3 => CURLY_UNDERLINE,
        4 => DOTTED_UNDERLINE,
        _ => DASHED_UNDERLINE,
    };
    let text = if c.is_wide_continuation() {
        String::new()
    } else if c.has_contents() {
        c.contents().to_string()
    } else {
        " ".into()
    };
    Cell { text, style: Style { fg: c.fgcolor(), bg: c.bgcolor(), us: c.underline_color(), attrs } }
}

/// Every row of a screen including its scrollback, oldest first.
pub fn snapshot(screen: &vt100::Screen) -> Vec<Line> {
    (0..screen.history_rows_len()).map(|i| {
        let mut line = line_at(screen, i);
        // a row that wrapped early (a wide character or tab moved on) ends at its last
        // used cell, so rejoining it in copy mode adds no blanks (grid_reflow reads cellused)
        if line.wrapped {
            if let Some(row) = screen.history_row(i) {
                line.cells.truncate(usize::from(row.used_len()));
            }
        }
        line
    }).collect()
}

/// Row `index` of [`snapshot`] (oldest history row = 0) without building the others.
pub fn line_at(screen: &vt100::Screen, index: usize) -> Line {
    let cols = screen.size().1.get();
    match screen.history_row(index) {
        Some(row) => Line { cells: (0..cols).map(|c| row.cell(c).map(cell_from_vt).unwrap_or_default()).collect(), wrapped: row.wrapped(), flags: row.flags(), time: row.time(), metadata: row.metadata().map(|(x,m)| (x,m.clone())).collect() },
        None => Line { cells: vec![Cell::default(); cols as usize], wrapped: false, ..Default::default() },
    }
}

/// Plain text of row `index` (trailing blanks removed, as [`Line::text`]; a tab as a tab)
/// and whether it wraps.
pub fn text_at(screen: &vt100::Screen, index: usize) -> (String, bool) {
    let cols = screen.size().1.get();
    let mut s = String::new();
    let Some(row) = screen.history_row(index) else { return (s, false) };
    if row.metadata().any(|(_, m)| m.tab_width != 0) {
        let line = line_at(screen, index);
        let s: String = (0..line.cells.len()).map(|x| line.cell_text(x)).collect();
        return (s.trim_end_matches(' ').to_string(), line.wrapped);
    }
    row.write_plain(&mut s, cols);
    let len = s.trim_end_matches(' ').len();
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
    fn line_metadata_helpers_follow_tabs_wide_characters_and_links() {
        let nz = |n| NonZeroU16::new(n).unwrap();
        let mut p = vt100::Parser::new(nz(3), nz(20), 50);
        p.process("a\tb\x1b]8;;http://w\x07中\x1b]8;;\x07x\t".as_bytes());
        let line = line_at(p.screen(), 0);
        assert_eq!((line.tab_at(0), line.tab_at(1), line.tab_at(7), line.tab_at(8)), (None, Some((1, 7)), Some((1, 7)), None));
        assert_eq!((0..14).filter(|&x| line.is_padding(x)).collect::<Vec<_>>(), [2, 3, 4, 5, 6, 7, 10, 13]);
        assert_eq!((0..20).map(|x| line.cell_text(x)).collect::<String>().trim_end_matches(' '), "a\tb中x\t");
        assert_eq!((line.hyperlink_at(9).map(|l| l.uri.as_str()), line.hyperlink_at(10).is_some(), line.hyperlink_at(11)), (Some("http://w"), true, None));
        // the plain capture path gives the tabs back and keeps a trailing one
        assert_eq!(text_at(p.screen(), 0), ("a\tb中x\t".to_string(), false));
        assert_eq!(line.tabs_end(), 16);
    }

    #[test]
    fn trailing_c1_characters_are_kept() {
        // U+0085 joins the cell before it and is white space only to Rust: a row ending
        // in it keeps it (capture-pane shows "s\302\205")
        let nz = |n| NonZeroU16::new(n).unwrap();
        let mut p = vt100::Parser::new(nz(3), nz(20), 0);
        p.process(b"abcdefghijklmnopqrs\xc2\x85\r\nx\xc2\x85  ");
        assert_eq!(text_at(p.screen(), 0).0, "abcdefghijklmnopqrs\u{85}");
        assert_eq!(line_at(p.screen(), 1).text(), "x\u{85}");
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
        // the cursor stays where it was on entering the alternate screen
        p.process(b"\x1b[?1049h\x1b[Habcdefghi\x1b[2;3H");
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

    #[test]
    fn output_that_widened_a_cell_in_the_last_column_is_read_and_reflowed() {
        // fuzzed pane output that killed the server: a pair of regional indicators made the
        // cell in the last column wide, then the cell was written over (with the pane's
        // rows, columns, history limit and the read sizes the fuzzer used)
        let nz = |n| NonZeroU16::new(n).unwrap();
        let fixture = |name: &str| std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pane-output-crash").join(name)).unwrap();
        let cases: [(&str, u16, u16, usize, &[usize]); 4] = [
            ("crash-5x25.bin", 5, 25, 0, &[]),
            ("panic-11.bin", 6, 1, 7, &[8, 35, 46, 8, 61, 44, 53, 40, 57, 31, 19, 6, 58, 50]),
            ("panic-1712.bin", 8, 18, 7, &[20, 43, 7, 21, 11, 5, 38, 6, 3, 24, 26, 28, 36, 50, 25, 11]),
            ("panic-1861.bin", 5, 25, 14, &[]),
        ];
        for (name, rows, cols, history, reads) in cases {
            let data = fixture(name);
            for split in [reads.to_vec(), vec![1; data.len()]] {
                let mut p = vt100::Parser::new(nz(rows), nz(cols), history);
                let mut at = 0;
                for n in split.iter().copied().chain([data.len()]) {
                    let end = (at + n).min(data.len());
                    p.process(&data[at..end]);
                    at = end;
                }
                let lines = snapshot(p.screen());
                for i in 0..lines.len() {
                    let _ = (text_at(p.screen(), i), line_at(p.screen(), i).text());
                }
                for y in 0..rows {
                    let _ = p.screen().contents_between(y, 0, y, cols);
                }
                for (r, c) in [(rows, 1), (rows, 2), (rows + 1, cols + 1), (1, cols.saturating_sub(1).max(1)), (rows, cols)] {
                    p.screen_mut().set_size(nz(r), nz(c));
                    p.process(b"\x1b[1;1H\x1b[@\x1b[P");
                    let _ = snapshot(p.screen());
                }
            }
        }
        // the screen tmux 3.8 shows for the first one (cat in a 25x5 pane)
        let mut p = vt100::Parser::new(nz(5), nz(25), 0);
        p.process(&fixture("crash-5x25.bin"));
        let texts: Vec<String> = snapshot(p.screen()).iter().map(Line::text).collect();
        assert_eq!(texts, ["uwerosumvqueullhaujzjnzjs", "wsoceeamizajzukpwaxgvqunv", "nsffhfezwlonsluvvuicbublc", "ffemsgmimjphtrvbrnypwffl\u{1F1F0}\u{1F1F7}", ""]);
        assert_eq!(p.screen().cursor_position(), (2, 25));
    }
}
