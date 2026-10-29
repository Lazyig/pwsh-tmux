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
    /// Text of the row with trailing blanks removed.
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
    let mut s = screen.clone();
    let (rows, cols) = s.size();
    let (rows, cols) = (rows.get(), cols.get());
    s.set_scrollback(usize::MAX);
    let history = s.scrollback();
    let mut out = Vec::with_capacity(history + rows as usize);
    let mut offset = history;
    loop {
        s.set_scrollback(offset);
        // With the view scrolled up by `offset`, row r is absolute line (history - offset + r).
        let start = history - offset;
        let take = if offset == 0 { rows as usize } else { offset.min(rows as usize) };
        for r in 0..take as u16 {
            if start + r as usize != out.len() {
                continue;
            }
            let cells = (0..cols).map(|c| s.cell(r, c).map(cell_from_vt).unwrap_or_default()).collect();
            out.push(Line { cells, wrapped: s.row_wrapped(r) });
        }
        if offset == 0 {
            break;
        }
        offset = offset.saturating_sub(rows as usize);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU16;

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
}
