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

    /// Columns up to the last cell that is not blank or the end of the last tab
    /// (grid_line_length).
    pub fn text_len(&self) -> usize {
        self.cells.iter().rposition(|c| c.text != " ").map_or(0, |i| i + 1).max(self.tabs_end())
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

/// Every row of a screen including its scrollback, oldest first (copy mode reads them one
/// at a time with [`snapshot_line`]; this is what that must give).
#[cfg(test)]
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

/// Row `index` of [`snapshot`]: [`line_at`], a row that wrapped early ending at its last
/// used cell.
pub fn snapshot_line(screen: &vt100::Screen, index: usize) -> Line {
    let mut line = line_at(screen, index);
    if line.wrapped {
        if let Some(row) = screen.history_row(index) {
            line.cells.truncate(usize::from(row.used_len()));
        }
    }
    line
}

/// The text [`cell_from_vt`] gives a cell (a cell past the stored row is blank).
fn vt_cell_text(c: Option<&vt100::Cell>) -> &str {
    match c {
        Some(c) if c.is_wide_continuation() => "",
        Some(c) if c.has_contents() => c.contents(),
        _ => " ",
    }
}

/// The number of cells of [`snapshot_line`] for a row of the screen (None for a row past
/// it, which is blank).
fn row_cells_len(row: Option<&vt100::capture::RowContents<'_>>, cols: usize) -> usize {
    match row {
        Some(row) if row.wrapped() => cols.min(usize::from(row.used_len())),
        _ => cols,
    }
}

fn cols_of(screen: &vt100::Screen) -> usize {
    usize::from(screen.size().1.get())
}

/// The number of cells of [`snapshot_line`], read from the screen.
pub fn snapshot_cells_len(screen: &vt100::Screen, index: usize) -> usize {
    row_cells_len(screen.history_row(index).as_ref(), cols_of(screen))
}

/// [`Line::text_len`] of [`snapshot_line`], read from the screen.
pub fn snapshot_text_len(screen: &vt100::Screen, index: usize) -> usize {
    let Some(row) = screen.history_row(index) else { return 0 };
    let n = row_cells_len(Some(&row), cols_of(screen));
    let used = (0..n).rposition(|x| vt_cell_text(row.cell(x as u16)) != " ").map_or(0, |x| x + 1);
    let tabs = row.metadata().filter(|(_, m)| m.tab_width > 0).map(|(x, m)| usize::from(x) + usize::from(m.tab_width)).max().unwrap_or(0).min(n);
    used.max(tabs)
}

/// [`Line::tab_at`] of [`snapshot_line`], read from the screen's row.
fn row_tab_at(row: &vt100::capture::RowContents<'_>, x: usize) -> Option<(usize, usize)> {
    let x16 = u16::try_from(x).ok()?;
    let (at, m) = row.metadata().take_while(|&(at, _)| at <= x16).last()?;
    let (at, w) = (usize::from(at), usize::from(m.tab_width));
    (x < at + w).then_some((at, w))
}

/// The text of cell `x` of [`snapshot_line`] for a row (its [`Cell::text`], None past its
/// cells).
fn row_cell<'a>(row: Option<&'a vt100::capture::RowContents<'_>>, cols: usize, x: usize) -> Option<&'a str> {
    if x >= row_cells_len(row, cols) {
        return None;
    }
    Some(row.map_or(" ", |row| vt_cell_text(row.cell(x as u16))))
}

/// `f` of the text of cell `x` of [`snapshot_line`] (its [`Cell::text`], None past its
/// cells), read from the screen.
pub fn with_snapshot_cell<R>(screen: &vt100::Screen, index: usize, x: usize, f: impl FnOnce(Option<&str>) -> R) -> R {
    let row = screen.history_row(index);
    f(row_cell(row.as_ref(), cols_of(screen), x))
}

/// [`Line::is_padding`] of [`snapshot_line`], read from the screen.
pub fn snapshot_is_padding(screen: &vt100::Screen, index: usize, x: usize) -> bool {
    let row = screen.history_row(index);
    row_cell(row.as_ref(), cols_of(screen), x) == Some("")
        || row.is_some_and(|row| row_tab_at(&row, x).is_some_and(|(at, _)| at != x))
}

/// Appends [`Line::cell_text`] of each of columns `xs` of [`snapshot_line`] to `out`, read
/// from the screen.
pub fn push_snapshot_cell_texts(out: &mut String, screen: &vt100::Screen, index: usize, xs: std::ops::Range<usize>) {
    let row = screen.history_row(index);
    let cols = cols_of(screen);
    let tabs = row.as_ref().is_some_and(|row| row.metadata().any(|(_, m)| m.tab_width != 0));
    for x in xs {
        match row.as_ref().filter(|_| tabs).and_then(|row| row_tab_at(row, x)) {
            Some((at, _)) if at == x => out.push('\t'),
            Some(_) => {}
            None => out.push_str(row_cell(row.as_ref(), cols, x).unwrap_or("")),
        }
    }
}

/// What columns `0..width` of [`snapshot_line`] give to a search: [`Line::cell_text`] of
/// each of its cells, a blank past them. Read from the screen unless the row has a tab.
pub fn snapshot_texts(screen: &vt100::Screen, index: usize, width: usize, mut f: impl FnMut(&str)) {
    let Some(row) = screen.history_row(index) else { return (0..width).for_each(|_| f(" ")) };
    if row.metadata().any(|(_, m)| m.tab_width != 0) {
        let line = snapshot_line(screen, index);
        return (0..width).for_each(|x| f(if x < line.cells.len() { line.cell_text(x) } else { " " }));
    }
    let n = snapshot_cells_len(screen, index).min(width);
    (0..n).for_each(|x| f(vt_cell_text(row.cell(x as u16))));
    (n..width).for_each(|_| f(" "));
}

/// Row `index` of the screen counting its history (oldest history row = 0).
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

/// A screen of random content for the tests that check copy mode's row-by-row reads
/// against [`snapshot`]: wrapped rows (also ones that wrap early), wide and combining
/// characters, links, tabs, prompt marks, the alternate screen, scroll regions, and a
/// history from none to past its limit.
#[cfg(test)]
pub(crate) fn random_parser(seed: u64) -> vt100::Parser {
    let mut rng = TestRng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let nz = |n: usize| std::num::NonZeroU16::new(n as u16).unwrap();
    let (rows, cols) = (2 + rng.below(7), 4 + rng.below(27));
    let limit = [0, 1, 5, 40, 200][rng.below(5)];
    let mut p = vt100::Parser::new(nz(rows), nz(cols), limit);
    for _ in 0..rng.below(400) {
        random_chunk(&mut rng, &mut p);
    }
    p
}

/// More random output for [`random_parser`]'s screen, or a resize (which rewraps it).
#[cfg(test)]
pub(crate) fn random_chunk(rng: &mut TestRng, p: &mut vt100::Parser) {
    let cols = usize::from(p.screen().size().1.get());
    const PARTS: &[&str] = &[
        "abc", "word ", "foo(bar) ", "  ", "\r\n", "\r\n", "\r\n", "\n", "\r", "\t", "a\tb", "한", "글자", "e\u{301}",
        "\u{1F469}\u{200d}\u{1F4BB}", "\u{1F1F0}\u{1F1F7}", "\x1b[31m", "\x1b[0m", "\x1b[1;4m", "\x1b[38;2;1;2;3m",
        "\x1b[44m", "\x1b]8;;http://a\x07", "\x1b]8;id=2;http://b\x07", "\x1b]8;;\x07", "\x1b]133;A\x07",
        "\x1b]133;C\x07", "\x1b[?1049h", "\x1b[?1049l", "\x1b[H", "\x1b[2J", "\x1b[K", "\x1b[5G", "\x1b[3D",
        "\x1b[2;4r", "\x1b[r", "\x1bD", "\x1bM", "\x1b[?7l", "\x1b[?7h", "\x08", "[x] ", "$ ls -l",
    ];
    match rng.below(10) {
        0 => p.process("x".repeat(rng.below(2 * cols + 2)).as_bytes()),
        1 => p.process(format!("{}\u{4e2d}", "y".repeat(cols - 1)).as_bytes()),
        2 if rng.below(4) == 0 => {
            let rows = p.screen().size().0;
            p.screen_mut().set_size(rows, std::num::NonZeroU16::new(2 + rng.below(30) as u16).unwrap());
        }
        3 => (0..rng.below(60)).for_each(|i| p.process(format!("{i} {}\r\n", "z ".repeat(i % 7)).as_bytes())),
        _ => p.process(PARTS[rng.below(PARTS.len())].as_bytes()),
    }
}

#[cfg(test)]
pub(crate) struct TestRng(pub u64);

#[cfg(test)]
impl TestRng {
    pub(crate) fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU16;

    #[test]
    fn snapshot_rows_read_one_at_a_time_match_the_whole_snapshot() {
        // copy mode builds its lines from a frozen screen with snapshot_line, and reads a
        // row's length, cells and search text without building it
        let mut wrapped_early = 0;
        for seed in 0..600 {
            let p = random_parser(seed);
            let s = p.screen();
            let all = snapshot(s);
            assert_eq!(all.len(), s.history_rows_len());
            let cols = usize::from(s.size().1.get());
            for (i, line) in all.iter().enumerate() {
                assert_eq!(&snapshot_line(s, i), line, "seed {seed} row {i}");
                assert_eq!(snapshot_cells_len(s, i), line.cells.len(), "seed {seed} row {i}");
                assert_eq!(snapshot_text_len(s, i), line.text_len(), "seed {seed} row {i}");
                for width in [cols, cols + 3] {
                    let mut texts = Vec::new();
                    snapshot_texts(s, i, width, |t| texts.push(t.to_string()));
                    let want: Vec<&str> = (0..width).map(|x| if x < line.cells.len() { line.cell_text(x) } else { " " }).collect();
                    assert_eq!(texts, want, "seed {seed} row {i}");
                }
                for x in 0..cols + 3 {
                    let cell = with_snapshot_cell(s, i, x, |c| c.map(str::to_string));
                    assert_eq!(cell.as_deref(), line.cells.get(x).map(|c| c.text.as_str()), "seed {seed} row {i} col {x}");
                    assert_eq!(snapshot_is_padding(s, i, x), line.is_padding(x), "seed {seed} row {i} col {x}");
                    let mut text = String::new();
                    push_snapshot_cell_texts(&mut text, s, i, x..x + 1);
                    assert_eq!(text, line.cell_text(x), "seed {seed} row {i} col {x}");
                }
                for (a, b) in [(0, cols), (1, cols + 3), (cols / 2, cols), (2, 1)] {
                    let mut text = String::new();
                    push_snapshot_cell_texts(&mut text, s, i, a..b);
                    assert_eq!(text, (a..b).map(|x| line.cell_text(x)).collect::<String>(), "seed {seed} row {i} cols {a}..{b}");
                }
                wrapped_early += usize::from(line.wrapped && line.cells.len() < cols);
            }
        }
        assert!(wrapped_early > 0, "a row that wraps early is covered");
    }

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
