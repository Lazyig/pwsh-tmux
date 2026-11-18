//! Composes a client's screen (panes, borders, status line, overlays) and sends the diff.

use super::modes::{clock_text, draw_clock};
use super::vars::Target;
use super::*;
use crate::cells::{Cell, Color, Line, Style, BLINK, BOLD, DIM, INVERSE, ITALIC, STRIKE, UNDERLINE};
use crate::format::{self, Span};
use std::fmt::Write as _;
use crate::width::char_width;

/// tty_check_codeset for a client without UTF-8: line drawing goes through the ACS charset and
/// anything else becomes one underscore per column the cell takes (`wide`: two columns, as
/// for a character made wide by VS16).
pub(super) fn non_utf8_text(text: &str, wide: bool) -> String {
    const ACS: &[(char, u8)] = &[('\u{b7}', b'~'), ('\u{2500}', b'q'), ('\u{2501}', b'q'), ('\u{2502}', b'x'), ('\u{2503}', b'x'), ('\u{250c}', b'l'), ('\u{250f}', b'k'), ('\u{2510}', b'k'), ('\u{2513}', b'l'), ('\u{2514}', b'm'), ('\u{2517}', b'm'), ('\u{2518}', b'j'), ('\u{251b}', b'j'), ('\u{251c}', b't'), ('\u{2523}', b't'), ('\u{2524}', b'u'), ('\u{252b}', b'u'), ('\u{2533}', b'w'), ('\u{2534}', b'v'), ('\u{253b}', b'v'), ('\u{253c}', b'n'), ('\u{254b}', b'n'), ('\u{2550}', b'q'), ('\u{2551}', b'x'), ('\u{2554}', b'l'), ('\u{2557}', b'k'), ('\u{255a}', b'm'), ('\u{255d}', b'j'), ('\u{2560}', b't'), ('\u{2563}', b'u'), ('\u{2566}', b'w'), ('\u{2569}', b'v'), ('\u{256c}', b'n')];
    let mut chars = text.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        if let Some((_, acs)) = ACS.iter().find(|(c, _)| *c == ch) {
            return format!("\x1b(0{}\x1b(B", *acs as char);
        }
    }
    "_".repeat(1 + wide as usize)
}

// ConPTY counts decomposed accents as extra columns. Compose when Windows can do
// so, and retain the original text if the platform cannot normalize it.
fn terminal_text(text: &str) -> std::borrow::Cow<'_, str> {
    #[cfg(windows)]
    if text.chars().any(|c| char_width(c) == Some(0)) {
        use windows_sys::Win32::Globalization::{NormalizeString, NormalizationC};
        let source: Vec<u16> = text.encode_utf16().collect();
        let needed = unsafe { NormalizeString(NormalizationC, source.as_ptr(), source.len() as i32, std::ptr::null_mut(), 0) };
        if needed > 0 {
            let mut dest = vec![0u16; needed as usize];
            let written = unsafe { NormalizeString(NormalizationC, source.as_ptr(), source.len() as i32, dest.as_mut_ptr(), needed) };
            if written > 0 { return String::from_utf16_lossy(&dest[..written as usize]).into(); }
        }
    }
    text.into()
}

pub struct Frame {
    pub w: u16,
    pub h: u16,
    pub cells: Vec<Cell>,
    links: std::collections::BTreeMap<usize, std::sync::Arc<vt100::Hyperlink>>,
}

impl Frame {
    pub fn new(w: u16, h: u16) -> Frame {
        Frame { w, h, cells: vec![Cell::default(); w as usize * h as usize], links: Default::default() }
    }
    pub fn set(&mut self, x: u16, y: u16, cell: Cell) {
        if x < self.w && y < self.h {
            let i = y as usize * self.w as usize + x as usize;
            self.links.remove(&i);
            // Overwriting half of a wide char: blank the other half.
            if self.cells[i].text.is_empty() && x > 0 {
                self.links.remove(&(i - 1));
                self.cells[i - 1] = Cell { text: " ".into(), style: self.cells[i - 1].style };
            }
            if !cell.text.is_empty() && x + 1 < self.w && self.cells[i + 1].text.is_empty() && self.cells[i].text.chars().next().and_then(|c| char_width(c)) == Some(2) {
                self.links.remove(&(i + 1));
                self.cells[i + 1] = Cell { text: " ".into(), style: self.cells[i + 1].style };
            }
            self.cells[i] = cell;
        }
    }
    /// The OSC 8 link of a cell set with [`Frame::set`] (cleared by the next set).
    fn set_link(&mut self, x: u16, y: u16, link: Option<&std::sync::Arc<vt100::Hyperlink>>) {
        if let Some(link) = link.filter(|_| x < self.w && y < self.h) {
            self.links.insert(y as usize * self.w as usize + x as usize, link.clone());
        }
    }
    /// Row `y` (its cells and links) as it is in `other`, a frame of the same size.
    fn copy_row(&mut self, other: &Frame, y: u16) {
        let w = self.w as usize;
        let start = y as usize * w;
        self.cells[start..start + w].clone_from_slice(other.row(y));
        let gone: Vec<usize> = self.links.range(start..start + w).map(|(i, _)| *i).collect();
        for i in gone {
            self.links.remove(&i);
        }
        for (i, l) in other.row_links(y) {
            self.links.insert(*i, l.clone());
        }
    }
    fn row_links(&self, y: u16) -> impl Iterator<Item = (&usize, &std::sync::Arc<vt100::Hyperlink>)> {
        let start = y as usize * self.w as usize;
        self.links.range(start..start + self.w as usize)
    }
    pub fn get(&self, x: u16, y: u16) -> Option<&Cell> {
        (x < self.w && y < self.h).then(|| &self.cells[y as usize * self.w as usize + x as usize])
    }
    pub fn row(&self, y: u16) -> &[Cell] {
        let w = self.w as usize;
        &self.cells[y as usize * w..(y as usize + 1) * w]
    }
    /// Writes `text` from (x, y) up to column `limit`; returns the next column.
    pub fn text(&mut self, mut x: u16, y: u16, text: &str, style: Style, limit: u16) -> u16 {
        let start = x;
        for ch in text.chars() {
            let cw = char_width(ch).unwrap_or(0) as u16;
            if cw == 0 {
                if x > start && y < self.h {
                    let mut previous = x - 1;
                    if self.get(previous, y).is_some_and(|c| c.text.is_empty()) && previous > start { previous -= 1; }
                    if previous < self.w { self.cells[y as usize * self.w as usize + previous as usize].text.push(ch); }
                }
                continue;
            }
            if x + cw > limit.min(self.w) {
                break;
            }
            self.set(x, y, Cell { text: ch.to_string(), style });
            if cw == 2 {
                self.set(x + 1, y, Cell { text: String::new(), style });
            }
            x += cw;
        }
        x
    }
    pub fn put_cells(&mut self, x: u16, y: u16, cells: &[Cell]) {
        for (i, c) in cells.iter().enumerate() {
            self.set(x + i as u16, y, c.clone());
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Modes {
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    /// Mouse reporting asked of the terminal: 0 off, 1000, 1002 or 1003.
    pub mouse: u16,
    pub focus: bool,
    pub cursor_style: u8,
    pub cursor_colour: Option<vt100::Color>,
    pub appesc: bool,
    /// modifyOtherKeys level requested from the outer terminal (extended-keys).
    pub extkeys: u8,
    /// While composing: the active pane's cursor is outside the part of a bigger window
    /// shown (moved into RenderState::park).
    pub park: bool,
}

/// Box characters for the *-border-lines options: horizontal, vertical, the corners
/// (tl, tr, bl, br), a cross, the left and right ends of a separator, then the top and
/// bottom junctions.
pub fn box_lines(kind: &str) -> [&'static str; 11] {
    match kind {
        "double" => ["═", "║", "╔", "╗", "╚", "╝", "╬", "╠", "╣", "╦", "╩"],
        "heavy" => ["━", "┃", "┏", "┓", "┗", "┛", "╋", "┣", "┫", "┳", "┻"],
        "rounded" => ["─", "│", "╭", "╮", "╰", "╯", "┼", "├", "┤", "┬", "┴"],
        "simple" => ["-", "|", "+", "+", "+", "+", "+", "+", "+", "+", "+"],
        "padded" | "none" => [" "; 11],
        _ => ["─", "│", "┌", "┐", "└", "┘", "┼", "├", "┤", "┬", "┴"],
    }
}

#[allow(clippy::too_many_arguments)]
pub fn draw_box_lines(f: &mut Frame, x: u16, y: u16, w: u16, h: u16, style: Style, title: &str, kind: &str) {
    let lines = box_lines(kind);
    if w < 2 || h < 2 {
        return;
    }
    for i in x + 1..x + w - 1 {
        f.set(i, y, Cell { text: lines[0].into(), style });
        f.set(i, y + h - 1, Cell { text: lines[0].into(), style });
    }
    for j in y + 1..y + h - 1 {
        f.set(x, j, Cell { text: lines[1].into(), style });
        f.set(x + w - 1, j, Cell { text: lines[1].into(), style });
    }
    f.set(x, y, Cell { text: lines[2].into(), style });
    f.set(x + w - 1, y, Cell { text: lines[3].into(), style });
    f.set(x, y + h - 1, Cell { text: lines[4].into(), style });
    f.set(x + w - 1, y + h - 1, Cell { text: lines[5].into(), style });
    if !title.is_empty() && w > 4 {
        let spans = format::spans(title, style);
        let cells = spans_cells(&spans, w.saturating_sub(4) as usize, style);
        let start = match spans.first().map(|s| s.align) {
            // format_draw from x + 2 in w - 4 cells: the centre is available / 2 - width / 2
            Some(format::Align::Centre | format::Align::AbsoluteCentre) => x + 2 + (w - 4) / 2 - (cells.len() as u16 / 2).min((w - 4) / 2),
            Some(format::Align::Right) => x + w - 2 - cells.len() as u16,
            _ => x + 2,
        };
        f.put_cells(start, y, &cells);
    }
}

/// The fill= colour and align= of a style (tmux keeps both in struct style).
fn style_fill_align(style: &str) -> (Option<Color>, String) {
    let (mut fill, mut align) = (None, "left".to_string());
    for word in style.split(',').map(str::trim) {
        if let Some(v) = word.strip_prefix("fill=") { fill = format::parse_color(v).filter(|c| *c != Color::Default); }
        if let Some(v) = word.strip_prefix("align=") { align = v.to_string(); }
    }
    (fill, align)
}

/// A prompt input character as drawn (prompt_redraw_character) and its width.
fn prompt_char(ch: char) -> (String, usize) {
    if ch <= '\x1f' || ch == '\x7f' {
        (format!("^{}", if ch == '\x7f' { '?' } else { (ch as u8 | 0x40) as char }), 2)
    } else {
        (ch.to_string(), char_width(ch).unwrap_or(0))
    }
}

/// The width of prompt input as drawn.
pub(crate) fn prompt_width(chars: &[char]) -> usize {
    chars.iter().map(|ch| prompt_char(*ch).1).sum()
}

/// prompt_store_complete: a leading space then the matches separated by spaces.
pub(crate) fn prompt_complete_display(list: &[String]) -> String {
    list.iter().map(|m| format!(" {m}")).collect()
}

/// Where prompt_layout puts the parts of a prompt.
#[derive(Default)]
pub(crate) struct PromptLayout {
    pub spans: Vec<Span>,
    pub style: Style,
    pub fill: Option<Color>,
    pub content_x: usize,
    pub content_width: usize,
    pub label_width: usize,
    pub input_x: usize,
    pub cursor_x: usize,
    pub input_offset: usize,
    pub input_width: usize,
}

pub(crate) fn message_area(width: u16, style: &str) -> (u16, u16) {
    let mut w = width;
    let mut align = "left";
    for word in style.split(',') {
        if let Some(value) = word.strip_prefix("width=") {
            let n = if let Some(pct) = value.strip_suffix('%') {
                pct.parse::<u32>().ok().map(|p| (width as u32 * p / 100) as u16)
            } else { value.parse().ok() };
            w = n.filter(|n| *n > 0 && *n <= width).unwrap_or(width);
        } else if let Some(value) = word.strip_prefix("align=") { align = value; }
    }
    (match align { "right" => width - w, "centre" | "absolute-centre" => (width - w) / 2, _ => 0 }, w)
}

// Preserve the existing status cells unless the format explicitly requests fill.
fn message_cells(f: &mut Frame, y: u16, text: &str, base: Style) {
    // No public style attribute uses bit 7, so it marks untouched cells.
    let marker = Style { attrs: 0x80, ..Style::default() };
    let mut fill = None;
    let mut rest = text;
    while let Some(i) = rest.find('#') {
        rest = &rest[i..];
        if rest.starts_with("##") { rest = &rest[2..]; continue; }
        if rest.starts_with("#[") {
            if let Some(end) = rest.find(']') {
                for word in rest[2..end].split(',') {
                    if let Some(bg) = word.strip_prefix("fill=").and_then(format::parse_color) { fill = Some(Style { bg, ..base }); }
                }
                rest = &rest[end + 1..];
                continue;
            }
        }
        rest = &rest[1..];
    }
    let cells = format::layout_line(&format::spans(text, base), f.w as usize, fill.unwrap_or(marker));
    for (x, cell) in cells.into_iter().enumerate() {
        if cell.style != marker { f.set(x as u16, y, cell); }
    }
}

fn push_color(out: &mut String, c: Color, base: u8) {
    match c {
        Color::Default => {}
        Color::Idx(i) if i < 8 => {
            let _ = write!(out, ";{}", base + i);
        }
        Color::Idx(i) if i < 16 => {
            let _ = write!(out, ";{}", base + 60 + i - 8);
        }
        Color::Idx(i) | Color::Palette(i) => {
            let _ = write!(out, ";{};5;{}", base + 8, i);
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";{};2;{};{};{}", base + 8, r, g, b);
        }
    }
}

/// Colours and attributes the client's terminal can show.
fn fit_style(mut st: Style, f: &super::features::Features) -> Style {
    let palette_fg = matches!(st.fg, Color::Rgb(..) | Color::Palette(_) | Color::Idx(16..=255));
    // tty_check_fg runs before tty_check_bg: black-on-black compares with the unconverted bg
    let bg = st.bg;
    st.fg = super::features::fit_color(st.fg, f);
    st.bg = super::features::fit_color(st.bg, f);
    if f.colours() == 2 {
        if st.attrs & INVERSE != 0 {
            if st.fg != Color::Idx(7) && st.fg != Color::Default { st.attrs &= !INVERSE; }
        } else if st.bg != Color::Idx(0) && st.bg != Color::Default { st.attrs |= INVERSE; }
        st.fg = Color::Default;
        st.bg = Color::Default;
    } else if f.colours() < 16 {
        if let Color::Idx(i @ 8..=15) = st.bg { st.bg = Color::Idx(i & 7); }
        if let Color::Idx(i @ 8..=15) = st.fg {
            st.fg = Color::Idx(i & 7);
            if !palette_fg { st.attrs |= BOLD; }
            else if st.fg == bg && matches!(st.fg, Color::Idx(0 | 7)) {
                st.fg = if st.fg == Color::Idx(0) { Color::Idx(7) } else { Color::Idx(0) };
            }
        }
    }
    if !f.has("strikethrough") {
        st.attrs &= !STRIKE;
    }
    // tty_attributes: underline styles and colour need Smulx and Setulc (usstyle), else a
    // plain underline; overline needs Smol
    if !f.has("usstyle") {
        if st.attrs & crate::cells::ALL_UNDERLINE != 0 {
            st.attrs = (st.attrs & !crate::cells::ALL_UNDERLINE) | UNDERLINE;
        }
        st.us = Color::Default;
    } else if st.us != Color::Default {
        st.us = super::features::fit_color(st.us, f);
    }
    if !f.has("overline") {
        st.attrs &= !crate::cells::OVERLINE;
    }
    st
}

/// tty_dim_default_colour: use terminal colours, then the explicit terminal theme.
fn dim_default(colour: Color, foreground: bool, terminal: &super::theme::Terminal, dim: u8) -> Color {
    let colour = if colour != Color::Default { colour } else {
        (if foreground { terminal.fg } else { terminal.bg }).unwrap_or_else(|| match terminal.reported {
            Some(format::Theme::Dark) => Color::Idx(if foreground { 7 } else { 0 }),
            Some(format::Theme::Light) => Color::Idx(if foreground { 0 } else { 7 }),
            _ => Color::Default,
        })
    };
    format::dim_color(colour, dim)
}

pub fn push_sgr(out: &mut String, st: &Style) {
    use crate::cells::{CURLY_UNDERLINE, DASHED_UNDERLINE, DOTTED_UNDERLINE, DOUBLE_UNDERLINE, HIDDEN, OVERLINE};
    out.push_str("\x1b[0");
    for (bit, code) in [(BOLD, ";1"), (DIM, ";2"), (ITALIC, ";3"), (BLINK, ";5"), (INVERSE, ";7"), (HIDDEN, ";8"), (STRIKE, ";9"), (OVERLINE, ";53")] {
        if st.attrs & bit != 0 {
            out.push_str(code);
        }
    }
    // one underline: Smulx with its style (the styles fit_style keeps), else smul
    let underline = [(DOUBLE_UNDERLINE, ";4:2"), (CURLY_UNDERLINE, ";4:3"), (DOTTED_UNDERLINE, ";4:4"), (DASHED_UNDERLINE, ";4:5"), (UNDERLINE, ";4")];
    if let Some((_, code)) = underline.iter().find(|(bit, _)| st.attrs & bit != 0) {
        out.push_str(code);
    }
    push_color(out, st.fg, 30);
    push_color(out, st.bg, 40);
    out.push('m');
    // Setulc and Setulc1 of the usstyle feature (its "::" is an escaped ':')
    match st.us {
        Color::Default => {}
        Color::Rgb(r, g, b) => {
            let _ = write!(out, "\x1b[58:2:{r}:{g}:{b}m");
        }
        Color::Idx(i) | Color::Palette(i) => {
            let _ = write!(out, "\x1b[58:5:{i}m");
        }
    }
}

/// A drawn line as sequences a pane's parser takes back (format_draw onto a pane, as
/// remain-on-exit-format): the SGR of each style (overline as 53, the line drawing set
/// as G0) and the text, up to the last used cell. A plain colour (no COLOUR_FLAG_256) as
/// the underscore colour is left out, as grid_string_cells_us leaves it out.
pub fn line_for_parser(line: &Line) -> String {
    let mut out = String::new();
    let used = (0..line.cells.len()).rposition(|x| line.cells[x].text != " " || line.cells[x].style != Style::default()).map_or(0, |x| x + 1);
    let mut last: Option<Style> = None;
    for (x, c) in line.cells.iter().enumerate().take(used) {
        if line.is_padding(x) {
            continue;
        }
        let mut st = c.style;
        if matches!(st.us, Color::Idx(0..=15)) {
            st.us = Color::Default;
        }
        if last != Some(st) {
            push_sgr(&mut out, &st);
            let acs = st.attrs & crate::cells::ACS != 0;
            if last.is_none_or(|l| (l.attrs & crate::cells::ACS != 0) != acs) {
                out.push_str(if acs { "\x1b(0" } else { "\x1b(B" });
            }
            last = Some(st);
        }
        out.push_str(line.cell_text(x));
    }
    out.push_str("\x1b(B");
    out
}

/// What grid_string_cells carries from one line to the next: the last cell read (its
/// attributes and colours, lastgc) and its link.
#[derive(Default)]
pub struct EscState {
    pub style: Style,
    pub link: Option<u64>,
}

/// grid_string_cells_fg/bg/us: the SGR parameters of a colour.
fn sgr_colour(c: Color, which: u8) -> Vec<u16> {
    let (base, default) = match which {
        b'f' => (30, Some(39)),
        b'b' => (40, Some(49)),
        _ => (58, None),
    };
    match c {
        Color::Default => default.into_iter().collect(),
        Color::Idx(i) if i < 8 && which != b'u' => vec![base + u16::from(i)],
        Color::Idx(i) if i < 16 && which != b'u' => vec![base + 60 + u16::from(i) - 8],
        Color::Idx(i) | Color::Palette(i) => vec![if which == b'u' { 58 } else { base + 8 }, 5, u16::from(i)],
        Color::Rgb(r, g, b) => vec![if which == b'u' { 58 } else { base + 8 }, 2, u16::from(r), u16::from(g), u16::from(b)],
    }
}

/// grid_string_cells_code: the sequences that change `last` into `cur`. A removed
/// attribute (or underline colour) starts with 0; each newly set attribute follows (a
/// code over 9 is written with a colon, so overline is 5:3 as in tmux); then the
/// colours that changed (one that is default after a reset is left out) and SO or SI.
pub fn sgr_diff(last: &Style, cur: &Style) -> String {
    use crate::cells::{ACS, CURLY_UNDERLINE, DASHED_UNDERLINE, DOTTED_UNDERLINE, DOUBLE_UNDERLINE, HIDDEN, OVERLINE};
    const CODES: [(u16, u16); 13] = [
        (BOLD, 1), (DIM, 2), (ITALIC, 3), (UNDERLINE, 4), (BLINK, 5), (INVERSE, 7), (HIDDEN, 8), (STRIKE, 9),
        (DOUBLE_UNDERLINE, 42), (CURLY_UNDERLINE, 43), (DOTTED_UNDERLINE, 44), (DASHED_UNDERLINE, 45), (OVERLINE, 53),
    ];
    let (attr, mut lastattr) = (cur.attrs, last.attrs);
    let mut s: Vec<u16> = Vec::new();
    if CODES.iter().any(|(m, _)| attr & m == 0 && lastattr & m != 0) || (last.us != Color::Default && cur.us == Color::Default) {
        s.push(0);
        lastattr &= ACS;
    }
    s.extend(CODES.iter().filter(|(m, _)| attr & m != 0 && lastattr & m == 0).map(|(_, c)| *c));
    let mut out = String::new();
    if !s.is_empty() {
        let codes: Vec<String> = s.iter().map(|c| if *c < 10 { c.to_string() } else { format!("{}:{}", c / 10, c % 10) }).collect();
        let _ = write!(out, "\x1b[{}m", codes.join(";"));
    }
    let reset = s.first() == Some(&0);
    for (n, o) in [(cur.fg, last.fg), (cur.bg, last.bg), (cur.us, last.us)].into_iter().zip([b'f', b'b', b'u']).map(|((n, o), w)| (sgr_colour(n, w), sgr_colour(o, w))) {
        if n.is_empty() || (!reset && n == o) || (reset && matches!(n[0], 39 | 49)) {
            continue;
        }
        let v: Vec<String> = n.iter().map(u16::to_string).collect();
        let _ = write!(out, "\x1b[{}m", v.join(";"));
    }
    if attr & ACS != 0 && lastattr & ACS == 0 {
        out.push('\x0e');
    }
    if attr & ACS == 0 && lastattr & ACS != 0 {
        out.push('\x0f');
    }
    out
}

/// capture-pane -e: a line with SGR escapes, after the lines before it (grid_string_cells keeps lastgc from
/// line to line: a line goes on with the attributes and link the line before ended in,
/// though each line closes the link it opened). With `allocated`
/// (GRID_STRING_EMPTY_CELLS) the cells read go on to what the line has room for, which
/// grid_expand_line grows to a quarter, half or all of the width; otherwise they end
/// with the last used one. Trailing spaces go unless `keep` (-N: no GRID_STRING_TRIM_SPACES).
pub fn line_with_escapes_from(line: &Line, st: &mut EscState, allocated: bool, keep: bool) -> String {
    let mut out = String::new();
    let mut open = false;
    let used = (0..line.cells.len()).rposition(|x| line.cells[x].text != " " || line.cells[x].style != Style::default() || line.metadata.contains_key(&(x as u16))).map_or(0, |x| x + 1);
    let end = if allocated { super::vars::line_cellsize(line.cells.len(), used) } else { used };
    for (x, c) in line.cells.iter().enumerate().take(end) {
        if line.is_padding(x) {
            continue;
        }
        out.push_str(&sgr_diff(&st.style, &c.style));
        st.style = c.style;
        // grid_string_cells_code: an OSC 8 link (with its own id) follows the attributes;
        // a cell without one closes the link only if this line opened it
        let next = line.metadata.get(&(x as u16)).and_then(|m| m.hyperlink.as_ref());
        if next.map(|l| l.inner) != st.link {
            if let Some(l) = next {
                let _ = write!(out, "\x1b]8;{}{};{}\x1b\\", if l.id.is_empty() { "" } else { "id=" }, l.id, l.uri);
                open = true;
            } else if open {
                out.push_str("\x1b]8;;\x1b\\");
                open = false;
            }
            st.link = next.map(|l| l.inner);
        }
        out.push_str(line.cell_text(x));
    }
    if open {
        out.push_str("\x1b]8;;\x1b\\");
    }
    if keep { out } else { out.trim_end_matches(' ').to_string() }
}

fn overlay_style(cell: &mut Style, over: Style) {
    if cell.fg == Color::Default {
        cell.fg = over.fg;
    }
    if cell.bg == Color::Default {
        cell.bg = over.bg;
    }
}

/// A blank that EL (erase to end of line, default background) reproduces exactly.
/// A link= of a style as a hyperlink: one for each URI, apart from the panes' links.
fn style_link(uri: &str) -> std::sync::Arc<vt100::Hyperlink> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    uri.hash(&mut h);
    let inner = h.finish() | (1 << 63);
    std::sync::Arc::new(vt100::Hyperlink { inner, id: String::new(), external_id: format!("tmux{:X}", inner & 0xffff_ffff), uri: uri.to_string() })
}

/// tty-features.c's Hls (the hyperlinks feature).
const HLS: &str = "\x1b]8;%?%p1%l%tid=%p1%s%;;%p2%s\x1b\\";

fn erasable(c: &Cell) -> bool {
    c.text == " " && c.style.bg == Color::Default && c.style.attrs & (UNDERLINE | INVERSE | STRIKE) == 0
}

fn spans_cells(spans: &[Span], max: usize, fill: Style) -> Vec<Cell> {
    let total: usize = spans.iter().map(|s| format::width(&s.text)).sum();
    let left_aligned: Vec<Span> = spans.iter().map(|s| Span { align: format::Align::Left, ..s.clone() }).collect();
    format::layout_line(&left_aligned, total.min(max), fill)
}

const BORDER_SETS: [(&str, [&str; 11]); 6] = [
    // h, v, tl, tr, bl, br, cross, t-down, t-up, t-right, t-left
    ("single", ["─", "│", "┌", "┐", "└", "┘", "┼", "┬", "┴", "├", "┤"]),
    ("double", ["═", "║", "╔", "╗", "╚", "╝", "╬", "╦", "╩", "╠", "╣"]),
    ("heavy", ["━", "┃", "┏", "┓", "┗", "┛", "╋", "┳", "┻", "┣", "┫"]),
    ("simple", ["-", "|", "+", "+", "+", "+", "+", "+", "+", "+", "+"]),
    // window_get_border_cell: PANE_LINES_SPACES and PANE_LINES_NONE are blank
    ("spaces", [" "; 11]),
    ("none", [" "; 11]),
];

/// Border connections of a scene cell (REDRAW_BORDER_L, _R, _U, _D).
const BORDER_L: u8 = 1;
const BORDER_R: u8 = 2;
const BORDER_U: u8 = 4;
const BORDER_D: u8 = 8;

/// The border glyph (index into a BORDER_SETS entry) for the connections of a border
/// cell (redraw_get_cell_type); None for CELL_NONE.
fn border_glyph(mask: u8) -> Option<usize> {
    const LR: u8 = BORDER_L | BORDER_R;
    const UD: u8 = BORDER_U | BORDER_D;
    Some(match mask {
        m if m == LR | UD => 6,
        m if m == LR | BORDER_U => 8,
        m if m == LR | BORDER_D => 7,
        m if m != 0 && m & UD == 0 => 0,
        m if m == BORDER_L | UD => 10,
        m if m == BORDER_L | BORDER_U => 5,
        m if m == BORDER_L | BORDER_D => 3,
        m if m == BORDER_R | UD => 9,
        m if m == BORDER_R | BORDER_U => 4,
        m if m == BORDER_R | BORDER_D => 2,
        m if m != 0 && m & LR == 0 => 1,
        _ => return None,
    })
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
enum SceneKind {
    #[default]
    Empty,
    Pane,
    Border,
    Status,
}

/// A cell of the scene (redraw_build_cell) for the tiled panes.
#[derive(Clone, Copy, Default, Debug)]
struct SceneCell {
    kind: SceneKind,
    /// the panes above, below, left and right of a border cell (top_wp, bottom_wp,
    /// left_wp, right_wp) and the pane of its half with two panes (style_wp)
    top: Option<PaneId>,
    bottom: Option<PaneId>,
    left: Option<PaneId>,
    right: Option<PaneId>,
    style: Option<PaneId>,
    /// connections to the cells round it (kept by a status cell for its fill)
    mask: u8,
    arrow: bool,
    /// the pane a status cell belongs to
    status: Option<PaneId>,
}

impl SceneCell {
    /// redraw_data_has_pane
    fn has(&self, p: PaneId) -> bool {
        [self.top, self.bottom, self.left, self.right].contains(&Some(p))
    }
}

/// The visible part of a window as tmux builds it before drawing: one cell per client
/// cell, in window coordinates from (ox, oy).
struct Scene {
    ox: i32,
    oy: i32,
    sx: i32,
    sy: i32,
    wsx: i32,
    wsy: i32,
    cells: Vec<SceneCell>,
}

impl Scene {
    fn new((ox, oy): (i32, i32), (sx, sy): (u16, u16), (wsx, wsy): (u16, u16)) -> Scene {
        Scene { ox, oy, sx: sx as i32, sy: sy as i32, wsx: wsx as i32, wsy: wsy as i32, cells: vec![SceneCell::default(); sx as usize * sy as usize] }
    }

    /// redraw_window_to_scene: the window's right and bottom edges are in the scene too
    fn index(&self, wx: i32, wy: i32) -> Option<usize> {
        if wx < 0 || wy < 0 || wx > self.wsx || wy > self.wsy || wx < self.ox || wy < self.oy {
            return None;
        }
        let (x, y) = (wx - self.ox, wy - self.oy);
        (x < self.sx && y < self.sy).then(|| (y * self.sx + x) as usize)
    }

    fn get(&self, wx: i32, wy: i32) -> Option<&SceneCell> {
        self.index(wx, wy).map(|i| &self.cells[i])
    }

    /// The columns from `a` to `b` (inclusive) that can be in the scene.
    fn cols(&self, a: i32, b: i32) -> std::ops::RangeInclusive<i32> {
        a.max(self.ox)..=b.min(self.ox + self.sx - 1)
    }

    fn rows(&self, a: i32, b: i32) -> std::ops::RangeInclusive<i32> {
        a.max(self.oy)..=b.min(self.oy + self.sy - 1)
    }

    /// redraw_mark_border_cell for a tiled pane with content columns `x0..x1`: only empty
    /// and border cells are marked, and a border cell merges what each pane marks.
    fn mark_border(&mut self, (wx, wy): (i32, i32), p: PaneId, (x0, x1): (i32, i32), (top_owner, bottom_owner): (bool, bool), mask: u8) {
        let Some(i) = self.index(wx, wy) else { return };
        let c = &mut self.cells[i];
        match c.kind {
            SceneKind::Empty => *c = SceneCell { kind: SceneKind::Border, ..SceneCell::default() },
            SceneKind::Border => {}
            _ => return,
        }
        if top_owner { c.top = Some(p); }
        if bottom_owner { c.bottom = Some(p); }
        if mask & (BORDER_U | BORDER_D) != 0 {
            if wx < x0 { c.right = Some(p); } else if wx >= x1 { c.left = Some(p); }
        }
        c.mask |= mask;
    }

    /// redraw_mark_pane for tiled pane `p` in layout cell `r` with content `cr`.
    fn mark_pane(&mut self, p: PaneId, r: Rect, cr: Rect, status: &str, arrows: bool) {
        // the inside and a scrollbar beside it
        for wy in self.rows(cr.y as i32, cr.bottom() as i32 - 1) {
            for wx in self.cols(r.x as i32, r.right() as i32 - 1) {
                if let Some(i) = self.index(wx, wy) { self.cells[i] = SceneCell { kind: SceneKind::Pane, ..SceneCell::default() }; }
            }
        }
        // redraw_mark_pane_borders: a pane status line belongs to its pane, so with the
        // line at the top a pane does not mark its bottom (unless at the window's bottom)
        // and at the bottom not its top
        let (left, right, top, bottom) = (r.x as i32 - 1, r.right() as i32, cr.y as i32 - 1, cr.bottom() as i32);
        let (mark_left, mark_right) = (left >= 0, right <= self.wsx);
        let mut mark_top = top >= 0;
        let mut mark_bottom = bottom <= self.wsy;
        if status == "top" && bottom < self.wsy { mark_bottom = false; } else if status == "bottom" { mark_top = false; }
        let content = (cr.x as i32, cr.right() as i32);
        // the pane is below its top border and above its bottom one
        for (on, wy, owners) in [(mark_top, top, (false, true)), (mark_bottom, bottom, (true, false))] {
            if !on { continue; }
            for wx in self.cols(left, right) {
                let mask = if wx > left { BORDER_L } else { 0 } | if wx < right { BORDER_R } else { 0 };
                self.mark_border((wx, wy), p, content, owners, mask);
            }
        }
        for (on, wx) in [(mark_left, left), (mark_right, right)] {
            if !on { continue; }
            for wy in self.rows(top, bottom) {
                let mask = if wy > top { BORDER_U } else { 0 } | if wy < bottom { BORDER_D } else { 0 };
                self.mark_border((wx, wy), p, content, (false, false), mask);
            }
        }
        // redraw_mark_border_status: its border cells on the status row from two in
        if status != "off" {
            let wy = if status == "top" { top } else { bottom };
            for wx in self.cols(cr.x as i32 + 2, right - 1) {
                let Some(i) = self.index(wx, wy) else { continue };
                let c = &mut self.cells[i];
                if c.kind == SceneKind::Border { *c = SceneCell { kind: SceneKind::Status, mask: c.mask, status: Some(p), ..SceneCell::default() }; }
            }
        }
        // redraw_mark_border_arrows: the border cells one in from its corners
        if arrows {
            let flag = |s: &mut Scene, wx: i32, wy: i32| {
                if let Some(i) = s.index(wx, wy) { if s.cells[i].kind == SceneKind::Border { s.cells[i].arrow = true; } }
            };
            let wx = cr.x as i32 + 1;
            if wx >= left && wx <= right { flag(self, wx, top); flag(self, wx, bottom); }
            let wy = cr.y as i32 + 1;
            if wy >= top && wy <= bottom { flag(self, left, wy); flag(self, right, wy); }
        }
    }

    /// redraw_mark_two_pane_colours: a border cell between the two panes takes the pane of
    /// its half of the window.
    fn two_pane_colours(&mut self, left_right: bool) {
        for y in 0..self.sy {
            for x in 0..self.sx {
                let (wx, wy) = (self.ox + x, self.oy + y);
                let c = &mut self.cells[(y * self.sx + x) as usize];
                if c.kind != SceneKind::Border { continue; }
                if left_right {
                    if let (Some(l), Some(r)) = (c.left, c.right) { c.style = Some(if wy <= self.wsy / 2 { l } else { r }); }
                } else if let (Some(t), Some(b)) = (c.top, c.bottom) {
                    c.style = Some(if wx <= self.wsx / 2 { t } else { b });
                }
            }
        }
    }
}

/// The digit tmux draws for pane-border-lines number: the pane index, modulo 10.
fn pane_number(panes: &[PaneId], base: usize, pane: Option<PaneId>) -> String {
    match pane.and_then(|p| panes.iter().position(|q| *q == p)) {
        Some(i) => char::from(b'0' + ((i + base) % 10) as u8).to_string(),
        None => "*".into(),
    }
}

impl Server {
    /// A status line format. As status.c's format_defaults(ft, c, NULL, NULL, NULL), its
    /// type is unknown (the window list entries are window formats); the target only
    /// fills in what the client's session would.
    fn expand_status_time(&self, t: Target, cid: ClientId, template: &str, ftype: &str) -> String {
        let extra = HashMap::from([(vars::FORMAT_TYPE.to_string(), ftype.to_string())]);
        format::expand_status_time(template, &vars::VarCtx { srv: self, t, client: Some(cid), extra })
    }

    /// A pane status line (tmux window_make_pane_status): pane-border-format drawn over the
    /// border line in `width` cells; None cells keep the border. The ranges are what a click
    /// on the line hits (window_pane_status_get_range).
    pub fn pane_status_line(&self, id: PaneId, width: usize, base: Style, cid: Option<ClientId>) -> (Vec<Option<Cell>>, Vec<Option<format::Range>>) {
        let text = self.expand_for_pane(id, &self.opt_pane(id, "pane-border-format"));
        let ctx = vars::VarCtx { srv: self, t: self.target_for_pane(id), client: cid, extra: HashMap::new() };
        format::draw_line(&format::spans_with(&text, base, Some(&ctx)), width, base)
    }

    pub fn render_all(&mut self) {
        self.notify_pane_themes();
        let ids: Vec<ClientId> = self.clients.iter().filter(|(_, c)| c.dirty && c.attached && !c.suspended && c.control == 0 && c.session.is_some()).map(|(id, _)| *id).collect();
        for id in ids {
            // theme colours parsed without a client are drawn in this client's palette
            let palette = vars::VarCtx { srv: self, t: Target::default(), client: Some(id), extra: HashMap::new() }.theme_palette();
            format::with_draw_palette(palette, || self.render(id));
        }
    }

    fn styled_spans(&self, text: &str, base: Style, t: Target, cid: ClientId) -> Vec<format::Span> {
        let ctx = vars::VarCtx { srv: self, t, client: Some(cid), extra: HashMap::new() };
        format::spans_with(text, base, Some(&ctx))
    }

    /// Expand a style option with the rendering client and target (options_string_to_style).
    pub(super) fn style_option(&self, value: &str, base: Style, t: Target, cid: Option<ClientId>) -> Option<Style> {
        let ctx = vars::VarCtx { srv: self, t, client: cid, extra: HashMap::new() };
        format::parse_style_with(value, base, Some(&ctx)).ok().map(|p| p.style)
    }

    /// A display-menu/display-popup -s/-S/-H style over the option's style, as
    /// menu_reapply_styles: it is parsed from the default cell and both its colours
    /// replace the option's (a colour it does not give becomes the default).
    fn flag_style(&self, value: Option<&str>, base: Style, t: Target, cid: ClientId) -> Style {
        let Some(value) = value else { return base };
        let ctx = vars::VarCtx { srv: self, t, client: Some(cid), extra: HashMap::new() };
        match format::parse_style_from(value, Style::default(), base, Some(&ctx)) {
            Ok(p) => Style { fg: p.style.fg, bg: p.style.bg, ..base },
            Err(_) => base,
        }
    }

    /// The target of a session's current pane, which status and message styles are expanded for.
    fn session_target(&self, sid: SessId) -> Target {
        let s = &self.sessions[&sid];
        let wid = s.active_window();
        Target { sid: Some(sid), idx: Some(s.active), wid: Some(wid), pane: self.windows.get(&wid).map(|w| w.active) }
    }

    fn status_style(&self, sid: SessId, cid: ClientId) -> Style {
        let cur = self.session_target(sid);
        let mut base = self.style_option(&self.opt_session(Some(sid), "status-style"), Style::default(), cur, Some(cid)).unwrap_or_default();
        if let Some(c) = format::parse_color(&self.opt_session(Some(sid), "status-fg")).filter(|c| *c != Color::Default) {
            base.fg = c;
        }
        if let Some(c) = format::parse_color(&self.opt_session(Some(sid), "status-bg")).filter(|c| *c != Color::Default) {
            base.bg = c;
        }
        base
    }

    /// Draw a pane's content (or its mode) with its top left at (x0, y0) of the window,
    /// clipped to `clip` (window columns and rows on the client); sets the cursor and the
    /// terminal modes for the active pane.
    #[allow(clippy::too_many_arguments)]
    fn draw_pane_body(&self, f: &mut Frame, cid: ClientId, wid: WinId, id: PaneId, (x0, y0, w, h): (i32, i32, u16, u16), clip: (u16, u16), oy: u16, modes: &mut Modes, cursor: &mut Option<(u16, u16)>) {
        let win = &self.windows[&wid];
        let Some(p) = self.panes.get(&id) else { return };
        let Some(c) = self.clients.get(&cid) else { return };
        let cw = clip.0;
        // columns and rows of the pane that are on the client
        let vis = |x: u16, y: u16| -> Option<(u16, u16)> {
            let (ax, ay) = (x0 + x as i32, y0 + y as i32);
            (ax >= 0 && ay >= 0 && ax < clip.0 as i32 && ay < clip.1 as i32).then_some((ax as u16, ay as u16))
        };
        // only the part on the client: a floating pane may be far bigger
        let span = |o: i32, n: u16, c: u16| (o.max(0) - o).min(n as i32) as u16..((c as i32 - o).clamp(0, n as i32)) as u16;
        let is_active = id == win.active;
        let pane_target = self.target_for_pane(id);
        let pst = |name: &str| self.style_option(&self.opt_pane(id, name), Style::default(), pane_target, Some(cid)).unwrap_or_default();
        // Active: active style > pane palette > window style. Inactive: window style > palette.
        let mut wstyle = pst("window-style");
        let own = &p.parser.callbacks().colours;
        if is_active {
            if let Some(c) = own.fg { wstyle.fg = c; }
            if let Some(c) = own.bg { wstyle.bg = c; }
            let active = pst("window-active-style");
            if active.fg != Color::Default { wstyle.fg = active.fg; }
            if active.bg != Color::Default { wstyle.bg = active.bg; }
        }
        if wstyle.fg == Color::Default { wstyle.fg = own.fg.unwrap_or(Color::Default); }
        if wstyle.bg == Color::Default { wstyle.bg = own.bg.unwrap_or(Color::Default); }
        let (default_fg, default_bg) = self.pane_style_colours(id);
        if default_fg == super::theme::StyleColour::Terminal { wstyle.fg = Color::Default; }
        if default_bg == super::theme::StyleColour::Terminal { wstyle.bg = Color::Default; }
        let dim_ctx = vars::VarCtx { srv: self, t: pane_target, client: Some(cid), extra: HashMap::new() };
        let mut dim = format::parse_style_with(&self.opt_pane(id, "window-style"), Style::default(), Some(&dim_ctx)).map(|p| p.dim).unwrap_or(0);
        if is_active {
            let active_dim = format::parse_style_with(&self.opt_pane(id, "window-active-style"), Style::default(), Some(&dim_ctx)).map(|p| p.dim).unwrap_or(0);
            dim = active_dim;
        }
        let dim_style = |style: &mut Style| {
            if dim != 0 {
                style.fg = dim_default(style.fg, true, &c.terminal, dim);
                style.bg = dim_default(style.bg, false, &c.terminal, dim);
            }
        };
        // a huge floating pane's mode screen is drawn only in the rows on the client
        let rows = if (w as usize * h as usize) <= 1 << 20 { 0..h as usize } else { let r = span(y0, h, clip.1); r.start as usize..r.end as usize };
        let mode_lines: Option<(Vec<Line>, Option<(u16, u16)>)> = match &p.mode {
            Some(PaneMode::Copy(cm)) => {
                let (lines, cur) = cm.render_rows(&self.copy_render_style(id, Some(cid)), rows);
                Some((lines, Some((cur.1, cur.0))))
            }
            Some(PaneMode::Tree(t)) => Some(t.render_rows(self, w, h, rows)),
            Some(PaneMode::Switch(s)) => Some(s.render_rows(self, w, h, rows)),
            Some(PaneMode::Panes(m)) => Some((m.render_rows(self, w, h, rows), None)),
            // the clock's screen is cleared before the time is drawn
            Some(PaneMode::Clock) => Some((Vec::new(), None)),
            None => None,
        };
        match &mode_lines {
            Some((lines, _)) => {
                for y in span(y0, h, clip.1) {
                    for x in span(x0, w, clip.0) {
                        let Some((ax, ay)) = vis(x, y) else { continue };
                        let mut cell = lines.get(y as usize).and_then(|l| l.cells.get(x as usize)).cloned().unwrap_or_default();
                        overlay_style(&mut cell.style, wstyle);
                        f.set(ax, oy + ay, cell);
                        f.set_link(ax, oy + ay, lines.get(y as usize).and_then(|l| l.tab_at(x as usize).map_or(l.metadata.get(&x), |(at, _)| l.metadata.get(&(at as u16)))?.hyperlink.as_ref()));
                    }
                }
            }
            None => {
                let scr = p.screen();
                for y in span(y0, h, clip.1) {
                    let row = scr.history_row(scr.scrollback_len() + y as usize);
                    for x in span(x0, w, clip.0) {
                        let Some((ax, ay)) = vis(x, y) else { continue };
                        let Some(vc) = scr.cell(y, x) else {
                            f.set(ax, oy + ay, Cell { text: " ".into(), style: wstyle });
                            continue;
                        };
                        let mut cell = crate::cells::cell_from_vt(vc);
                        if vc.is_wide() && (x + 1 >= w || ax + 1 >= cw) {
                            cell.text = " ".into();
                        }
                        overlay_style(&mut cell.style, wstyle);
                        f.set(ax, oy + ay, cell);
                        f.set_link(ax, oy + ay, row.as_ref().and_then(|r| r.hyperlink_at(x)));
                    }
                }
            }
        }
        // the clock is drawn in the pane and then clipped with it (a huge pane's clock is
        // far off the client anyway)
        if matches!(p.mode, Some(PaneMode::Clock)) && (w as usize * h as usize) <= 1 << 20 {
            // style_apply of clock-mode-colour: a format, theme colours from the client
            let colour = self.style_option(&format!("fg={}", self.opt_window(Some(wid), "clock-mode-colour")), Style::default(), pane_target, Some(cid)).map_or(Color::Idx(4), |s| s.fg);
            let text = clock_text(&self.opt_window(Some(wid), "clock-mode-style"), chrono::Local::now().time());
            let mut pf = Frame::new(w, h);
            let marker = Style { attrs: 0x80, ..Style::default() };
            pf.cells.iter_mut().for_each(|c| c.style = marker);
            draw_clock(&mut pf, Rect { x: 0, y: 0, w, h }, &text, colour, wstyle.bg);
            for y in span(y0, h, clip.1) {
                for x in span(x0, w, clip.0) {
                    if let (Some((ax, ay)), Some(cell)) = (vis(x, y), pf.get(x, y).filter(|c| c.style != marker)) {
                        f.set(ax, oy + ay, cell.clone());
                    }
                }
            }
        }
        if dim != 0 {
            for y in span(y0, h, clip.1) {
                for x in span(x0, w, clip.0) {
                    let Some((ax, ay)) = vis(x, y) else { continue };
                    if let Some(cell) = f.get(ax, oy + ay).cloned() {
                        let mut cell = cell;
                        dim_style(&mut cell.style);
                        f.set(ax, oy + ay, cell);
                    }
                }
            }
        }
        // server_client_reset_state: a cursor off the part of the window shown (the edges
        // count as on it)
        let off_view = |cx: u16, cy: u16| {
            let (ax, ay) = (x0 + cx as i32, y0 + cy as i32);
            ax < 0 || ay < 0 || ax > clip.0 as i32 || ay > clip.1 as i32
        };
        if is_active {
            match &mode_lines {
                Some((_, Some((cx, cy)))) => {
                    modes.park = off_view(*cx, *cy);
                    if *cx < w && *cy < h {
                        *cursor = vis(*cx, *cy).map(|(x, y)| (x, oy + y));
                    }
                }
                Some(_) => {}
                None => {
                    let scr = p.screen();
                    modes.app_cursor = scr.application_cursor();
                    modes.bracketed_paste = scr.bracketed_paste();
                    modes.cursor_style = merged_cursor_style(scr.cursor_style() as u8, scr.cursor_blinking(), features::cursor_style(&self.opt_pane(id, "cursor-style")));
                    let colour = p.parser.callbacks().osc.cursor_colour.clone().unwrap_or_else(|| self.opt_pane(id, "cursor-colour"));
                    // a colour option, not a format (OPTIONS_TABLE_IS_COLOUR)
                    modes.cursor_colour = format::parse_color(&colour).filter(|c| *c != vt100::Color::Default);
                    let (cy, cx) = scr.cursor_position();
                    modes.park = off_view(cx, cy);
                    if !scr.hide_cursor() && cx < w && cy < h {
                        *cursor = vis(cx, cy).map(|(x, y)| (x, oy + y));
                    }
                }
            }
        }
    }

    /// A pane prompt over the last line of its pane, or the first with the status line at
    /// the top (redraw_draw_pane_prompt), clipped like the pane.
    #[allow(clippy::too_many_arguments)]
    fn draw_pane_prompt(&self, f: &mut Frame, cid: ClientId, wid: WinId, id: PaneId, (x0, y0, w, h): (i32, i32, u16, u16), clip: (u16, u16), oy: u16, modes: &mut Modes, cursor: &mut Option<(u16, u16)>) {
        let Some(pr) = self.panes.get(&id).and_then(|p| p.prompt.as_ref()) else { return };
        let Some(sid) = self.clients.get(&cid).and_then(|c| c.session) else { return };
        if w == 0 || h == 0 {
            return;
        }
        let top = self.opt_session(Some(sid), "status-position") == "top" && self.status_lines(Some(sid)) > 0;
        let row = y0 + if top { 0 } else { h as i32 - 1 };
        if row < 0 || row >= clip.1 as i32 {
            return;
        }
        // the visible columns of a wide pane only
        let first = (-x0).max(0).min(w as i32) as u16;
        let last = (clip.0 as i32 - x0).clamp(0, w as i32) as u16;
        if first >= last {
            return;
        }
        let mut line = Frame::new(w, 1);
        let x = self.draw_prompt(&mut line, cid, pr, 0, w, 0);
        for i in first..last {
            if let Some(cell) = line.get(i, 0) {
                f.set((x0 + i as i32) as u16, oy + row as u16, cell.clone());
            }
        }
        if id == self.windows[&wid].active {
            let cx = x0 + x.min(w - 1) as i32;
            *cursor = (cx >= 0 && cx < clip.0 as i32).then_some((cx as u16, oy + row as u16));
            self.prompt_cursor(modes, sid, pr);
        }
    }

    /// Draw the floating panes above the tiled ones, back to front: border lines (the pane's
    /// own pane-border-lines, -style and -status), the title on the border and the content.
    #[allow(clippy::too_many_arguments)]
    fn draw_floats(&self, f: &mut Frame, cid: ClientId, wid: WinId, clip: (u16, u16), (px, py): (u16, u16), oy: u16, modes: &mut Modes, cursor: &mut Option<(u16, u16)>) {
        let win = &self.windows[&wid];
        let floats = self.float_rects(wid);
        if floats.is_empty() {
            return;
        }
        // `clip` is on the client; the window cells it shows start at (px, py)
        let (pxi, pyi) = (px as i32, py as i32);
        let wclip = (clip.0 + px, clip.1 + py);
        // a floating pane above the active pane's cursor hides it
        if let Some((cx, cy)) = *cursor {
            let (wx, wy) = (cx as i32 + pxi, cy as i32 - oy as i32 + pyi);
            if floats.iter().any(|(p, _)| *p != win.active && self.float_contains(wid, *p, wx, wy)) && !win.is_floating(win.active) {
                *cursor = None;
            }
        }
        let panes = win.panes();
        let base_index = self.opt_window_num(Some(wid), "pane-base-index").max(0) as usize;
        let arrows = matches!(self.opt_window(Some(wid), "pane-border-indicators").as_str(), "arrows" | "both");
        for &(id, g) in &floats {
            if win.is_floating(win.active) && id == win.active {
                *cursor = None;
            }
            let t = self.target_for_pane(id);
            let lines = self.pane_lines(wid, id);
            let active = id == win.active;
            let base = self.style_option(&self.opt_pane(id, "pane-border-style"), Style::default(), t, Some(cid)).unwrap_or_default();
            let st = if active { self.style_option(&self.opt_pane(id, "pane-active-border-style"), base, t, Some(cid)).unwrap_or(base) } else { base };
            let st = if self.marked == Some(id) { Style { attrs: st.attrs | INVERSE, ..st } } else { st };
            let put = |f: &mut Frame, x: i32, y: i32, text: &str| {
                let (x, y) = (x - pxi, y - pyi);
                if x >= 0 && y >= 0 && x < clip.0 as i32 && y < clip.1 as i32 {
                    f.set(x as u16, oy + y as u16, Cell { text: text.into(), style: st });
                }
            };
            if lines != "none" {
                // number draws the pane's own index (tmux window_get_border_cell)
                let digit = pane_number(&panes, base_index, Some(id));
                let set: [&str; 6] = match lines.as_str() {
                    "double" => ["═", "║", "╔", "╗", "╚", "╝"],
                    "heavy" => ["━", "┃", "┏", "┓", "┗", "┛"],
                    "simple" => ["-", "|", "+", "+", "+", "+"],
                    "spaces" => [" "; 6],
                    "number" => [digit.as_str(); 6],
                    _ => ["─", "│", "┌", "┐", "└", "┘"],
                };
                let (l, r, top, bot) = (g.x - 1, g.x + g.w as i32, g.y - 1, g.y + g.h as i32);
                for x in l..=r {
                    let (tc, bc) = if x == l { (set[2], set[4]) } else if x == r { (set[3], set[5]) } else { (set[0], set[0]) };
                    put(f, x, top, tc);
                    put(f, x, bot, bc);
                }
                for y in g.y..bot {
                    put(f, l, y, set[1]);
                    put(f, r, y, set[1]);
                }
                // tmux redraw_mark_border_arrows: the active floating pane's own border
                // points at it (the cells taken by the status line are skipped below)
                if active && arrows {
                    let (ax, ay) = (g.x + 1, g.y + 1);
                    if ax <= r {
                        put(f, ax, top, "↓");
                        put(f, ax, bot, "↑");
                    }
                    if ay < bot {
                        put(f, l, ay, "→");
                        put(f, r, ay, "←");
                    }
                }
                let status = self.pane_status(wid, id);
                if status != "off" {
                    let by = if status == "top" { top } else { bot };
                    let width = self.pane_status_width(wid, id, (g.x + 2, by), g.w.saturating_sub(2) as usize, wclip);
                    let (cells, _) = if width > 0 { self.pane_status_line(id, width, st, Some(cid)) } else { Default::default() };
                    for (i, c) in cells.into_iter().enumerate() {
                        let x = g.x + 2 + i as i32;
                        let (cx, cy) = (x - pxi, by - pyi);
                        if let Some(c) = c.filter(|_| cx >= 0 && cy >= 0 && cx < clip.0 as i32 && cy < clip.1 as i32 && x < r) {
                            f.set(cx as u16, oy + cy as u16, c);
                        }
                    }
                }
            }
            // the content less a reserved scrollbar, then the bar inside the border
            let cg = self.float_content(wid, id, g);
            self.draw_pane_body(f, cid, wid, id, (cg.x - pxi, cg.y - pyi, cg.w, cg.h), clip, oy, modes, cursor);
            if let Some(sb) = self.float_scrollbar(wid, id, g) { super::display::draw_scrollbar(f, sb, (-pxi, -pyi), clip, oy); }
            self.draw_pane_prompt(f, cid, wid, id, (cg.x - pxi, cg.y - pyi, cg.w, cg.h), clip, oy, modes, cursor);
        }
        // ... and so does one above an active floating pane
        if let (Some((cx, cy)), true) = (*cursor, win.is_floating(win.active)) {
            let (wx, wy) = (cx as i32 + pxi, cy as i32 - oy as i32 + pyi);
            let above = floats.iter().skip_while(|(p, _)| *p != win.active).skip(1);
            if above.into_iter().any(|(p, _)| self.float_contains(wid, *p, wx, wy)) {
                *cursor = None;
            }
        }
    }

    pub(super) fn compose(&self, cid: ClientId) -> Option<RenderState> {
        let _widths = self.width_context();
        let c = self.clients.get(&cid)?;
        let sid = c.session?;
        let s = self.sessions.get(&sid)?;
        let (cw, ch) = (c.size.0.max(1), c.size.1.max(1));
        let mut f = Frame::new(cw, ch);
        let wid = s.active_window();
        let win = &self.windows[&wid];
        let nstatus = self.status_lines(Some(sid)).min(ch.saturating_sub(1));
        let top = self.opt_session(Some(sid), "status-position") == "top";
        let oy = if top { nstatus } else { 0 };
        let (rects, _) = self.rects(wid);
        let mut cursor = None;
        let mut status_ranges = Vec::new();
        // a window bigger than the client is shown from (px, py): client column x is
        // window column x + px (tty_window_offset)
        let view = self.view_offset(cid);
        let (px, py) = (view.ox, view.oy);
        let (pxi, pyi) = (px as i32, py as i32);
        let mut modes = Modes { mouse: self.outer_mouse_mode(cid), focus: options::is_on(&self.opt_server("focus-events")), ..Modes::default() };
        let area_h = ch.saturating_sub(nstatus);
        // window position to client position, when it is on the client
        let to_client = |x: i32, y: i32| -> Option<(u16, u16)> {
            let (x, y) = (x - pxi, y - pyi);
            (x >= 0 && y >= 0 && x < cw as i32 && y < area_h as i32).then_some((x as u16, y as u16))
        };
        // the window columns and rows shown, in window coordinates
        let wclip = ((px as u32 + cw as u32).min(win.sx as u32) as u16, (py as u32 + area_h as u32).min(win.sy as u32) as u16);
        // top-floating and bottom-floating show status lines on floating panes only
        let border_status = match self.opt_window(Some(wid), "pane-border-status").as_str() {
            s @ ("top" | "bottom") => s.to_string(),
            _ => "off".to_string(),
        };
        let active_target = self.target_for_pane(win.active);
        let active_border = self.style_option(&self.opt_window(Some(wid), "pane-active-border-style"), Style::default(), active_target, Some(cid)).unwrap_or_default();
        let inactive_border = self.style_option(&self.opt_window(Some(wid), "pane-border-style"), Style::default(), active_target, Some(cid)).unwrap_or_default();
        let indicators = self.opt_window(Some(wid), "pane-border-indicators");
        let lines_kind = self.opt_window(Some(wid), "pane-border-lines");
        let bset = BORDER_SETS.iter().find(|(n, _)| *n == lines_kind).map(|(_, s)| *s).unwrap_or(BORDER_SETS[0].1);

        // Fill every unused cell; is_inside distinguishes the window from the
        // extra client area when window-size is manual or another client is larger.
        let fill_option = self.opt_window(Some(wid), "fill-character");
        for inside in [false, true] {
            let vars = super::vars::VarCtx { srv: self, t: active_target, client: Some(cid), extra: HashMap::from([
                ("is_inside".into(), if inside { "1" } else { "0" }.into()),
                ("is_outside".into(), if inside { "0" } else { "1" }.into()),
            ]) };
            let text = format::expand(&fill_option, &vars);
            let spans = format::spans(&text, Style::default());
            // window_get_fill_cell: the fill cell's colours and attributes go over
            // the default border style (pane-border-style)
            let over = |s: Style| Style {
                fg: if s.fg == Color::Default { inactive_border.fg } else { s.fg },
                bg: if s.bg == Color::Default { inactive_border.bg } else { s.bg },
                us: if s.us == Color::Default { inactive_border.us } else { s.us },
                attrs: inactive_border.attrs | s.attrs,
            };
            let cell = spans.iter().find(|s| !s.text.is_empty()).and_then(|s| s.text.chars().next().map(|ch| {
                let ch = if text.contains("#[acs]") { match ch {
                    '~' => '·', 'q' => '─', 'x' => '│', 'l' => '┌', 'k' => '┐', 'm' => '└', 'j' => '┘', 'n' => '┼', 't' => '├', 'u' => '┤', 'v' => '┴', 'w' => '┬', _ => ch,
                } } else { ch };
                if char_width(ch) == Some(1) { Cell { text: ch.to_string(), style: over(s.style) } } else { Cell { text: " ".into(), style: inactive_border } }
            })).unwrap_or(Cell { text: " ".into(), style: inactive_border });
            for y in 0..area_h { for x in 0..cw {
                if ((x as u32 + px as u32) < win.sx as u32 && (y as u32 + py as u32) < win.sy as u32) == inside { f.set(x, oy + y, cell.clone()); }
            } }
        }

        // panes
        for (id, r) in &rects {
            let cr = self.content_rect(wid, *r);
            self.draw_pane_body(&mut f, cid, wid, *id, (cr.x as i32 - pxi, cr.y as i32 - pyi, cr.w, cr.h), (cw, area_h), oy, &mut modes, &mut cursor);
        }

        for (id, r) in &rects {
            if let Some(sb) = self.pane_scrollbar(wid, *id, *r) { super::display::draw_scrollbar(&mut f, sb, (-pxi, -pyi), (cw, area_h), oy); }
        }

        // The tiled panes' borders and status lines, built like tmux's scene
        // (redraw_build_cells): back to front in the z-order each pane marks its inside,
        // its borders round its layout cell from its status line, turns its status row
        // into status cells and flags its arrow cells. Tiled panes are in the order they
        // were made, so the first made wins a side two panes mark; a status cell is
        // the pane's alone.
        let mut scene = Scene::new((pxi, pyi), (cw, area_h), (win.sx, win.sy));
        let arrows = matches!(indicators.as_str(), "arrows" | "both");
        for p in win.zlist().into_iter().rev() {
            let Some((_, r)) = rects.iter().find(|(q, _)| *q == p) else { continue };
            scene.mark_pane(p, *r, self.content_rect(wid, *r), &border_status, arrows);
        }
        // redraw_mark_two_pane_colours: with two panes and colour indicators the
        // cells between them take the pane of their half
        if matches!(indicators.as_str(), "colour" | "both") && rects.len() == 2 && !win.zoomed {
            scene.two_pane_colours(rects[0].1.y == rects[1].1.y);
        }
        let base = self.opt_window_num(Some(wid), "pane-base-index").max(0) as usize;
        let pane_list = win.panes();
        let marked = self.marked.filter(|m| pane_list.contains(m));
        // window_get_border_cell: pane-border-lines number draws the index of the pane
        // the cell is drawn for
        let glyph = |mask: u8, pane: Option<PaneId>| -> String {
            match border_glyph(mask) {
                None if matches!(lines_kind.as_str(), "spaces" | "none") => " ".into(),
                None => "·".into(),
                Some(_) if lines_kind == "number" => pane_number(&pane_list, base, pane),
                Some(g) => bset[g].into(),
            }
        };
        for y in 0..area_h {
            for x in 0..cw {
                let Some(c) = scene.get(x as i32 + pxi, y as i32 + pyi) else { continue };
                if c.kind != SceneKind::Border { continue; }
                // redraw_get_pane_for_border_style: the half's pane, else the active pane
                // when the cell is next to it, else the pane above, below, left or right
                let owner = c.style.or(if c.has(win.active) { Some(win.active) } else { c.top.or(c.bottom).or(c.left).or(c.right) });
                let mut style = if owner == Some(win.active) { active_border } else { inactive_border };
                if marked.is_some_and(|m| c.has(m)) { style.attrs ^= INVERSE; }
                let active = Some(win.active);
                let arrow = if !c.arrow { None }
                    else if c.left == active { Some("←") }
                    else if c.right == active { Some("→") }
                    else if c.top == active { Some("↑") }
                    else if c.bottom == active { Some("↓") }
                    else { None };
                let text = arrow.map_or_else(|| glyph(c.mask, owner), str::to_string);
                f.set(x, oy + y, Cell { text, style });
            }
        }
        // window_make_pane_status: pane-border-format drawn over the pane's own border
        // cells (each the type of the cell beneath), starting two in from the pane
        if border_status != "off" {
            for (id, r) in &rects {
                let cr = self.content_rect(wid, *r);
                let by = if border_status == "top" { cr.y as i32 - 1 } else { cr.bottom() as i32 };
                let sx = cr.x + 2;
                if sx >= r.right() { continue; }
                let st = if *id == win.active { active_border } else { inactive_border };
                // as wide as what floating panes leave visible (redraw_pane_status_width)
                let width = self.pane_status_width(wid, *id, (sx as i32, by), (r.right() - sx) as usize, wclip);
                if width == 0 { continue; }
                let (cells, _) = self.pane_status_line(*id, width, st, Some(cid));
                for (off, cell) in cells.into_iter().enumerate() {
                    let wx = sx as i32 + off as i32;
                    let Some(sc) = scene.get(wx, by) else { continue };
                    if sc.kind != SceneKind::Status || sc.status != Some(*id) { continue; }
                    let Some((cx, cy)) = to_client(wx, by) else { continue };
                    let cell = cell.unwrap_or_else(|| Cell { text: glyph(sc.mask, Some(*id)), style: st });
                    f.set(cx, oy + cy, cell);
                }
            }
        }
        // pane prompts over the last line of their panes (the first with the
        // status line at the top, redraw_draw_pane_prompt)
        for (id, r) in &rects {
            // a zoomed window shows only the active pane's
            if win.zoomed && *id != win.active { continue; }
            let cr = self.content_rect(wid, *r);
            self.draw_pane_prompt(&mut f, cid, wid, *id, (cr.x as i32 - pxi, cr.y as i32 - pyi, cr.w, cr.h), (cw, area_h), oy, &mut modes, &mut cursor);
        }
        // floating panes above the tiled ones
        self.draw_floats(&mut f, cid, wid, (wclip.0 - px, wclip.1 - py), (px, py), oy, &mut modes, &mut cursor);

        // the window menu, above the panes (redraw_mark_menu)
        if let Some(m) = self.menus.get(&wid).filter(|_| c.overlay.is_none()) {
            let ost = |name: &str, base: Style| self.style_option(&self.opt_window(Some(wid), name), base, active_target, Some(cid)).unwrap_or(base);
            // menu_reapply_styles: each from the default cell (style_apply)
            let st = ost("menu-style", Style::default());
            let bst = ost("menu-border-style", Style::default());
            let selected = ost("menu-selected-style", Style::default());
            // display-menu -s -S -H -b
            let given = |v: &Option<String>, base: Style| self.flag_style(v.as_deref(), base, active_target, cid);
            let (st, bst, selected) = (given(&m.look.style, st), given(&m.look.border_style, bst), given(&m.look.selected_style, selected));
            let lines = m.look.lines.clone().unwrap_or_else(|| self.opt_window(Some(wid), "menu-border-lines"));
            if (px, py) == (0, 0) {
                m.draw(&mut f, oy, st, bst, selected, &lines);
            } else {
                // drawn in the window, so it pans with it
                let mut mf = Frame::new(m.w, m.h);
                m.draw_at(&mut mf, (0, 0), st, bst, selected, &lines);
                for (i, cell) in mf.cells.into_iter().enumerate() {
                    let (x, y) = ((i % m.w as usize) as i32 + m.x as i32, (i / m.w as usize) as i32 + m.y as i32);
                    if let Some((x, y)) = to_client(x, y) { f.set(x, oy + y, cell); }
                }
            }
            cursor = None;
        }

        // status lines
        if nstatus > 0 {
            let sy0 = if top { 0 } else { ch - nstatus };
            let formats: Vec<String> = (0..nstatus).map(|i| self.opt_session(Some(sid), &format!("status-format[{i}]"))).collect();
            for i in 0..nstatus {
                let y = sy0 + i;
                let custom = &formats[i as usize];
                // The built-in first line has a dedicated renderer for list clipping.
                // the default first line too: its window list is a list= of format_draw
                let (line, tags, links) = if !custom.is_empty() {
                    let base = self.status_style(sid, cid);
                    let cur = Target { sid: Some(sid), idx: Some(s.active), wid: Some(wid), pane: Some(win.active) };
                    let text = self.expand_status_time(cur, cid, custom, "");
                    format::layout_line_links(&self.styled_spans(&text, base, cur, cid), cw as usize, base)
                } else {
                    (vec![Cell { text: " ".into(), style: self.status_style(sid, cid) }; cw as usize], vec![None; cw as usize], vec![None; cw as usize])
                };
                f.put_cells(0, y, &line);
                // style link= (the status screen's own hyperlinks)
                for (x, uri) in links.iter().enumerate() {
                    if let Some(uri) = uri {
                        f.set_link(x as u16, y, Some(&style_link(uri)));
                    }
                }
                status_ranges.push(tags);
            }
        }
        // message / prompt replace the first status line (or the last row without a status line)
        let message_line = self.opt_session_num(Some(sid), "message-line").max(0) as u16;
        let my = if nstatus > 0 { (if top { 0 } else { ch - nstatus }) + message_line.min(nstatus - 1) } else { ch - 1 };
        if let Some(pr) = &c.prompt {
            let style_text = self.expand_time(active_target, Some(cid), &self.opt_session(Some(sid), "message-style"));
            let (ax, aw) = message_area(cw, &style_text);
            let x = self.draw_prompt(&mut f, cid, pr, ax, aw, my);
            cursor = Some((x.min(cw - 1), my));
            self.prompt_cursor(&mut modes, sid, pr);
        } else if let Some((msg, _)) = &c.message {
            let style_text = self.expand_time(active_target, Some(cid), &self.opt_session(Some(sid), "message-style"));
            let (ax, aw) = message_area(cw, &style_text);
            // status_message_escape: a text message's # are drawn as they are
            let msg = if c.message_ignore_styles { msg.replace('#', "##") } else { msg.clone() };
            let extra = HashMap::from([("message".to_string(), msg), ("command_prompt".to_string(), "0".to_string())]);
            let vars = super::vars::VarCtx { srv: self, t: active_target, client: Some(cid), extra };
            let st = self.style_option(&style_text, Style::default(), active_target, Some(cid)).unwrap_or_default();
            let expanded = format::expand_time(&self.opt_session(Some(sid), "message-format"), &vars);
            let mut area = Frame::new(aw, 1);
            area.put_cells(0, 0, &f.cells[(my as usize * cw as usize + ax as usize)..(my as usize * cw as usize + ax as usize + aw as usize)]);
            message_cells(&mut area, 0, &expanded, st);
            f.put_cells(ax, my, &area.cells);
            cursor = None;
        }

        // overlays, over the status line like tmux's overlay_draw
        if let Some(ov) = c.overlay.as_ref() {
            match ov {
                Overlay::Popup(pp) => {
                    let st = self.style_option(&self.opt_window(Some(wid), "popup-style"), Style::default(), active_target, Some(cid)).unwrap_or_default();
                    let bst = self.style_option(&self.opt_window(Some(wid), "popup-border-style"), Style::default(), active_target, Some(cid)).unwrap_or_default();
                    // display-popup -s -S -b; popup_reapply_styles drops the attributes
                    let given = |v: &Option<String>, base: Style| Style { attrs: 0, ..self.flag_style(v.as_deref(), base, active_target, cid) };
                    let (st, bst) = (given(&pp.look.style, st), given(&pp.look.border_style, bst));
                    let lines = pp.look.lines.clone().unwrap_or_else(|| self.opt_window(Some(wid), "popup-border-lines"));
                    for y in pp.y..pp.y + pp.h {
                        for x in pp.x..pp.x + pp.w {
                            f.set(x, y, Cell { text: " ".into(), style: st });
                        }
                    }
                    if pp.border {
                        draw_box_lines(&mut f, pp.x, pp.y, pp.w, pp.h, bst, &pp.title, &lines);
                    }
                    let (ix, iy, iw, ih) = pp.inner();
                    cursor = None;
                    if let Some(p) = self.panes.get(&pp.pane) {
                        let scr = p.screen();
                        for y in 0..ih {
                            let row = scr.history_row(scr.scrollback_len() + y as usize);
                            for x in 0..iw {
                                if let Some(vc) = scr.cell(y, x) {
                                    let mut cell = crate::cells::cell_from_vt(vc);
                                    if vc.is_wide() && x + 1 >= iw {
                                        cell.text = " ".into();
                                    }
                                    overlay_style(&mut cell.style, st);
                                    f.set(ix + x, iy + y, cell);
                                    f.set_link(ix + x, iy + y, row.as_ref().and_then(|r| r.hyperlink_at(x)));
                                }
                            }
                        }
                        modes.app_cursor = scr.application_cursor();
                        modes.bracketed_paste = scr.bracketed_paste();
                        if !scr.hide_cursor() {
                            let (cy, cx) = scr.cursor_position();
                            if cx < iw && cy < ih {
                                cursor = Some((ix + cx, iy + cy));
                            }
                        }
                    }
                }
            }
        }

        // server_client_reset_state: when the active pane's cursor is outside the part of a
        // bigger window shown, the hidden cursor is moved to the top left
        let park = std::mem::take(&mut modes.park) && cursor.is_none() && view.bigger && c.prompt.is_none() && c.overlay.is_none() && !self.menus.contains_key(&wid);
        modes.appesc = c.features.has("appesc");
        modes.app_cursor = true;
        modes.bracketed_paste = c.features.has("bpaste");
        // TTY_FREEZE (status_prompt_set): while a prompt is open nothing a pane writes is
        // drawn, only the status lines and the prompt row are; what redraws the whole
        // window (CLIENT_REDRAWWINDOW, server_client_check_redraw) still draws it, and
        // closing the prompt redraws everything (status_prompt_clear)
        let freeze = c.prompt.as_ref().filter(|p| p.freeze && !p.incremental && p.copy_incremental.is_none() && p.host.is_none()).map(|_| {
            let panes: Vec<_> = rects.iter().map(|(p, r)| (*p, *r, self.panes.get(p).and_then(|p| p.mode.as_ref()).map(std::mem::discriminant))).collect();
            format!("{wid:?} {:?} {panes:?} {px} {py} {:?} {}", win.active, c.overlay.is_some(), self.menus.contains_key(&wid))
        });
        if let Some(key) = &freeze && let Some(prev) = c.prev.as_ref().filter(|p| p.freeze.as_ref() == Some(key) && p.frame.w == f.w && p.frame.h == f.h) {
            let status = if top { 0..nstatus } else { ch - nstatus..ch };
            for y in (0..ch).filter(|y| !status.contains(y) && *y != my) {
                f.copy_row(&prev.frame, y);
            }
        }
        Some(RenderState { frame: f, cursor, park, modes, status_ranges, freeze })
    }

    /// prompt_draw: while a prompt has the cursor it takes prompt-cursor-style and
    /// prompt-cursor-colour (the prompt-command-* ones in vi command mode).
    fn prompt_cursor(&self, modes: &mut Modes, sid: SessId, pr: &Prompt) {
        let prefix = if pr.vi_command { "prompt-command-cursor" } else { "prompt-cursor" };
        modes.cursor_style = features::cursor_style(&self.opt_session(Some(sid), &format!("{prefix}-style")));
        modes.cursor_colour = format::parse_color(&self.opt_session(Some(sid), &format!("{prefix}-colour"))).filter(|c| *c != vt100::Color::Default);
    }

    /// The status line row of the client's prompt and messages
    /// (status_prompt_screen_line).
    pub(crate) fn message_row(&self, cid: ClientId) -> Option<u16> {
        let c = self.clients.get(&cid)?;
        let sid = c.session?;
        let ch = c.size.1.max(1);
        let nstatus = self.status_lines(Some(sid)).min(ch.saturating_sub(1));
        let top = self.opt_session(Some(sid), "status-position") == "top";
        let line = self.opt_session_num(Some(sid), "message-line").max(0) as u16;
        Some(if nstatus > 0 { (if top { 0 } else { ch - nstatus }) + line.min(nstatus - 1) } else { ch - 1 })
    }

    /// tmux 3.8 prompt_layout: message-format is expanded with #{message} set
    /// to the prompt string and drawn as a label, the input follows it; both
    /// (and any inline completion matches) are placed in the area by the
    /// effective style's align (message-command-style in vi command mode).
    pub(crate) fn prompt_layout(&self, cid: ClientId, pr: &Prompt, ax: u16, aw: u16) -> PromptLayout {
        let sid = self.client_session(cid);
        let t = sid.map(|s| self.sessions[&s].active_window()).map(|w| self.target_for_pane(self.windows[&w].active)).unwrap_or_default();
        // prompt_create keeps the command's find state: a pane prompt expands for its pane
        let t = match pr.host.filter(|h| self.panes.contains_key(h)) {
            Some(h) if self.window_of(h) == t.wid => Target { pane: Some(h), ..t },
            Some(h) => self.target_for_pane(h),
            None => t,
        };
        let flags =[(pr.single_key, "SINGLE"), (pr.numeric, "NUMERIC"), (pr.incremental || pr.copy_incremental.is_some(), "INCREMENTAL"), (pr.bspace_exit, "BSPACE_EXIT"), (pr.vi_command, "COMMANDMODE"), (pr.host.is_some(), "ISPANE")];
        let extra = HashMap::from([
            ("prompt_input".to_string(), pr.input.iter().collect::<String>()),
            ("prompt_flags".to_string(), flags.into_iter().filter_map(|(on, flag)| on.then_some(flag)).collect::<Vec<_>>().join(",")),
            ("prompt_type".to_string(), super::input::PROMPT_TYPES.get(pr.history_type).copied().unwrap_or("command").to_string()),
            ("command_prompt".to_string(), if pr.vi_command { "1" } else { "0" }.to_string()),
        ]);
        let mut vars = super::vars::VarCtx { srv: self, t, client: Some(cid), extra };
        let label = format::expand_time(&pr.label, &vars);
        vars.extra.insert("message".to_string(), label);
        let effective = format::expand_time(&self.opt_session(sid, if pr.vi_command { "message-command-style" } else { "message-style" }), &vars);
        let style = self.style_option(&effective, Style::default(), t, Some(cid)).unwrap_or_default();
        let (fill, align) = style_fill_align(&effective);
        let expanded = format::expand_time(&self.opt_session(sid, "message-format"), &vars);
        let spans = format::spans(&expanded, style);
        let mut l = PromptLayout { spans, style, fill, ..PromptLayout::default() };
        if aw == 0 {
            return l;
        }
        let aw = aw as usize;
        l.label_width = l.spans.iter().map(|s| format::width(&s.text)).sum::<usize>().min(aw);
        let pcursor = prompt_width(&pr.input[..pr.cursor.min(pr.input.len())]);
        let pwidth = prompt_width(&pr.input) + pr.quote_next as usize;
        let avail = aw - l.label_width;
        let mut cursor_x = l.label_width;
        if avail > 0 {
            let (offset, width) = if pcursor >= avail { (pcursor - avail + 1, avail) } else { (0, pwidth.min(avail)) };
            l.input_offset = offset;
            l.input_width = width;
            cursor_x = l.label_width + pcursor - offset;
        }
        l.content_width = l.label_width + l.input_width;
        if !pr.complete.is_empty() && pr.cursor == pr.input.len() && cursor_x < aw {
            let width = format::width(&prompt_complete_display(&pr.complete)).min(aw - cursor_x);
            l.content_width = l.content_width.max(cursor_x + width);
        }
        l.content_width = l.content_width.min(aw);
        l.content_x = ax as usize + match align.as_str() {
            "centre" | "absolute-centre" => (aw - l.content_width) / 2,
            "right" => aw - l.content_width,
            _ => 0,
        };
        l.input_x = l.content_x + l.label_width;
        l.cursor_x = cursor_x + l.content_x;
        l
    }

    /// prompt_draw: the prompt in columns ax..ax+aw of row y; returns the cursor column.
    pub(crate) fn draw_prompt(&self, f: &mut Frame, cid: ClientId, pr: &Prompt, ax: u16, aw: u16, y: u16) -> u16 {
        let l = self.prompt_layout(cid, pr, ax, aw);
        let sy = l.style;
        if let Some(bg) = l.fill {
            for x in ax..ax + aw { f.set(x, y, Cell { text: " ".into(), style: Style { bg, ..Style::default() } }); }
        }
        if l.content_width == 0 {
            return l.cursor_x as u16;
        }
        if l.label_width > 0 {
            f.put_cells(l.content_x as u16, y, &format::layout_line(&l.spans, l.label_width, sy));
        }
        // input from offset, control characters as ^X
        // and a '^' at the cursor while C-v waits for the key to quote
        let (mut width, mut x) = (0, l.input_x);
        let mut shown: Vec<char> = pr.input.clone();
        if pr.quote_next { shown.insert(pr.cursor.min(pr.input.len()), '^'); }
        for ch in &shown {
            let (text, w) = prompt_char(*ch);
            if width < l.input_offset { width += w; continue; }
            if width + w > l.input_offset + l.input_width { break; }
            width += w;
            if text.len() == 2 && text.starts_with('^') {
                f.set(x as u16, y, Cell { text: "^".into(), style: sy });
                f.set(x as u16 + 1, y, Cell { text: text[1..].into(), style: sy });
            } else {
                f.set(x as u16, y, Cell { text, style: sy });
                if w == 2 { f.set(x as u16 + 1, y, Cell { text: String::new(), style: sy }); }
            }
            x += w;
        }
        // prompt_draw_complete: the matches underlined after the input
        if !pr.complete.is_empty() && pr.cursor == pr.input.len() && l.cursor_x >= l.content_x && l.cursor_x - l.content_x < l.content_width {
            let avail = l.content_width - (l.cursor_x - l.content_x);
            let st = Style { attrs: sy.attrs | UNDERLINE, ..sy };
            let (mut x, mut used) = (l.cursor_x, 0);
            for ch in prompt_complete_display(&pr.complete).chars() {
                let w = char_width(ch).unwrap_or(0);
                if used + w > avail { break; }
                f.set(x as u16, y, Cell { text: ch.to_string(), style: st });
                if w == 2 { f.set(x as u16 + 1, y, Cell { text: String::new(), style: st }); }
                x += w;
                used += w;
            }
        }
        l.cursor_x as u16
    }

    pub fn render(&mut self, cid: ClientId) {
        let _widths = self.width_context();
        self.update_view(cid);
        let Some(next) = self.compose(cid) else { return };
        // window title
        let mut title_update = None;
        if let Some(sid) = self.client_session(cid) {
            if self.opt_session_on(Some(sid), "set-titles") {
                let s = &self.sessions[&sid];
                let wid = s.active_window();
                let t = Target { sid: Some(sid), idx: Some(s.active), wid: Some(wid), pane: Some(self.windows[&wid].active) };
                let title = self.expand_time(t, Some(cid), &self.opt_session(Some(sid), "set-titles-string"));
                if self.clients[&cid].title != title {
                    title_update = Some(title);
                }
            }
        }
        let ext_keys = self.opt_server("extended-keys");
        let c = self.clients.get_mut(&cid).unwrap();
        let feats = c.features.clone();
        if !feats.has("title") {
            title_update = None;
        }
        let mut next = next;
        if !feats.has("mouse") {
            next.modes.mouse = 0;
        }
        next.modes.focus &= feats.has("focus");
        // tty_update_features sends Eneks for both on and always
        next.modes.extkeys = (feats.has("extkeys") && ext_keys != "off") as u8;
        c.dirty = false;
        c.status_ranges = next.status_ranges.clone();
        let prev = c.prev.as_ref().filter(|p| p.frame.w == next.frame.w && p.frame.h == next.frame.h);
        let mut out = String::new();
        // a full redraw writes every row (EL ends each one), so nothing is cleared first:
        // with clear-on-attach off an ED 2 would push the first screen into a terminal's
        // scrollback a second time (tmux draws the lines, tty_draw_line)
        if prev.is_none() {
            out.push_str("\x1b[0m\x1b[H");
        }
        let mut changed = false;
        let hyperlinks = feats.has("hyperlinks") || c.term_caps.contains_key("Hls");
        for y in 0..next.frame.h {
            let row = next.frame.row(y);
            if prev.is_some_and(|p| p.frame.row(y) == row && p.frame.row_links(y).eq(next.frame.row_links(y))) {
                continue;
            }
            changed = true;
            let _ = write!(out, "\x1b[{};1H", y + 1);
            let mut cur: Option<Style> = None;
            // Like tmux, blank default-background cells at the end of the row are erased
            // with EL instead of being written as spaces. Terminals that keep text per line
            // (the IntelliJ terminal's editor) then don't get full-width lines, which an
            // IME's composed-text inlay at the cursor would push into a soft wrap.
            let end = row.iter().rposition(|c| !erasable(c)).map_or(0, |i| i + 1).max(next.frame.row_links(y).last().map_or(0, |(i,_)| i % next.frame.w as usize + 1));
            let mut link = None;
            for (x, cell) in row.iter().enumerate().take(end) {
                if cell.text.is_empty() {
                    continue;
                }
                if cur != Some(cell.style) {
                    push_sgr(&mut out, &fit_style(cell.style, &feats));
                    cur = Some(cell.style);
                }
                // tty_hyperlink (after the attributes): Hls with the link's external id, when the terminal has it
                if hyperlinks {
                    let next_link = next.frame.links.get(&(y as usize * next.frame.w as usize + x));
                    if next_link.map(|l| l.inner) != link {
                        let (id, uri) = next_link.map_or(("", ""), |l| (l.external_id.as_str(), l.uri.as_str()));
                        out.push_str(&features::string_capability(&c.term_caps, "Hls", HLS, &[id, uri]));
                        link = next_link.map(|l| l.inner);
                    }
                }
                // tty_acs_get: a DEC graphics character is drawn as its Unicode form
                let acs = (cell.style.attrs & crate::cells::ACS != 0).then(|| cell.text.chars().next().and_then(crate::cells::acs_char)).flatten();
                let text = acs.unwrap_or(&cell.text);
                if c.utf8() || text.is_ascii() { out.push_str(&terminal_text(text)); }
                else { out.push_str(&non_utf8_text(text, row.get(x + 1).is_some_and(|n| n.text.is_empty()))); }
                if cfg!(windows) && cell.text.chars().any(|c| char_width(c) == Some(0)) {
                    let width = cell.text.chars().filter_map(|c| char_width(c)).sum::<usize>();
                    if x + width < next.frame.w as usize { let _ = write!(out, "\x1b[{};{}H", y + 1, x + width + 1); }
                }
            }
            if link.is_some() {
                out.push_str(&features::string_capability(&c.term_caps, "Hls", HLS, &["", ""]));
            }
            if end < row.len() {
                // EL erases with the current background: make sure it is the default
                if cur != Some(Style::default()) {
                    out.push_str("\x1b[0m");
                }
                out.push_str("\x1b[K");
            }
        }
        let pm = prev.map(|p| p.modes);
        let modes_changed = pm != Some(next.modes);
        let cursor_changed = prev.is_none_or(|p| p.cursor != next.cursor || p.park != next.park);
        if let Some(t) = title_update {
            // tty_set_title: tsl and fsl, from the title feature unless overridden
            let _ = write!(out, "{}{}{}", features::string_capability(&c.term_caps, "tsl", "\x1b]0;", &[]), t, features::string_capability(&c.term_caps, "fsl", "\x07", &[]));
            c.title = t;
            changed = true;
        }
        if !changed && !modes_changed && !cursor_changed {
            c.prev = Some(next);
            return;
        }
        out.insert_str(0, "\x1b[?25l");
        out.push_str("\x1b[0m");
        if modes_changed {
            let m = next.modes;
            out.push_str(if m.app_cursor { "\x1b[?1h" } else { "\x1b[?1l" });
            out.push_str(&features::string_capability(&c.term_caps, if m.bracketed_paste { "Enbp" } else { "Dsbp" }, if m.bracketed_paste { "\x1b[?2004h" } else { "\x1b[?2004l" }, &[]));
            out.push_str(match m.mouse {
                0 => "\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l",
                1000 => "\x1b[?1003l\x1b[?1002l\x1b[?1000h\x1b[?1006h",
                1002 => "\x1b[?1003l\x1b[?1000h\x1b[?1002h\x1b[?1006h",
                _ => "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h",
            });
            out.push_str(&features::string_capability(&c.term_caps, if m.focus { "Enfcs" } else { "Dsfcs" }, if m.focus { "\x1b[?1004h" } else { "\x1b[?1004l" }, &[]));
            // tty_force_cursor_colour: Cs with rgb:RR/GG/BB (colour_force_rgb), Cr back to the
            // default, each only when the colour the terminal has changes
            let colour = m.cursor_colour.and_then(features::colour_force_rgb);
            if feats.has("ccolour") && colour != c.input_state.tty_ccolour {
                match colour {
                    Some((r, g, b)) => out.push_str(&features::string_capability(&c.term_caps, "Cs", "\x1b]12;%p1%s\x07", &[&format!("rgb:{r:02x}/{g:02x}/{b:02x}")])),
                    None => out.push_str(&features::string_capability(&c.term_caps, "Cr", "\x1b]112\x07", &[])),
                }
                c.input_state.tty_ccolour = colour;
            }
            // tty_update_cursor: the style when it changes, and a set style again on a full
            // redraw (tty_invalidate resets the cursor modes)
            if feats.has("cstyle") && (m.cursor_style != c.input_state.tty_cstyle || (pm.is_none() && m.cursor_style != 0)) {
                out.push_str(&features::cursor_sequence(&c.term_caps, m.cursor_style));
                c.input_state.tty_cstyle = m.cursor_style;
            }
            if pm.is_none_or(|p| p.appesc != m.appesc) && (m.appesc || pm.is_some_and(|p| p.appesc)) { out.push_str(&features::string_capability(&c.term_caps, if m.appesc { "Enesc" } else { "Dsesc" }, if m.appesc { "\x1b[?7727h" } else { "\x1b[?7727l" }, &[])); }
            c.input_state.tty_appesc = m.appesc;
            if pm.map_or(0, |p| p.extkeys) != m.extkeys {
                // modifyOtherKeys: have the terminal report keys like C-Enter distinctly
                out.push_str(&if m.extkeys != 0 {
                    features::string_capability(&c.term_caps, "Eneks", "\x1b[>4;2m", &[])
                } else {
                    features::string_capability(&c.term_caps, "Dseks", "\x1b[>4m", &[])
                });
            }
        }
        if let Some((x, y)) = next.cursor {
            let _ = write!(out, "\x1b[{};{}H\x1b[?25h", y + 1, x + 1);
        } else if next.park {
            out.push_str("\x1b[H");
        }
        if feats.has("sync") {
            // synchronized output: the terminal shows the whole frame at once
            out.insert_str(0, "\x1b[?2026h");
            out.push_str("\x1b[?2026l");
        }
        c.send(S_OUTPUT, out.as_bytes());
        c.prev = Some(next);
    }
}

/// The DECSCUSR style tmux sends (tty_update_cursor): the shape set by the application, or
/// the cursor-style option's when it set none, blinking as CSI ? 12 h/l or the last
/// DECSCUSR left it; with neither, the option's own blinking.
pub(super) fn merged_cursor_style(app: u8, (blinking, blinking_set): (bool, bool), option: u8) -> u8 {
    let with_blink = |style: u8| if style == 0 { 0 } else { (style + 1) / 2 * 2 - blinking as u8 };
    match app {
        0 if !blinking_set => option,
        0 => with_blink(option),
        n => with_blink(n),
    }
}


#[cfg(test)]
mod theme_tests {
    use super::*;


    #[test]
    fn mono_and_modern_terminals_preserve_their_colour_capabilities() {
        for term in ["vt100", "dumb"] {
            let f = super::super::features::compute(term, "", &[], "");
            let style = fit_style(Style { fg: Color::Idx(1), bg: Color::Idx(4), ..Style::default() }, &f);
            assert_eq!((style.fg, style.bg, style.attrs & INVERSE), (Color::Default, Color::Default, INVERSE));
            let mut out = String::new();
            push_sgr(&mut out, &style);
            assert_eq!(out, "\x1b[0;7m");
        }
        for term in ["xterm", "foot", "xterm-ghostty", "wezterm"] {
            let f = super::super::features::compute(term, "", &[], "");
            let style = fit_style(Style { fg: Color::Idx(9), ..Style::default() }, &f);
            assert_eq!((style.fg, style.attrs & BOLD), (Color::Idx(9), 0), "{term}");
        }
        let f = super::super::features::compute("screen", "", &[], "");
        let mut parser = vt100::Parser::default();
        parser.process(b"\x1b[38;5;9mX");
        let cell = crate::cells::cell_from_vt(parser.screen().cell(0, 0).unwrap());
        assert_eq!(cell.style.fg, Color::Palette(9));
        let style = fit_style(cell.style, &f);
        assert_eq!((style.fg, style.attrs & BOLD), (Color::Idx(1), 0));
    }

    #[test]
    fn dim_defaults_follow_explicit_terminal_colours_and_theme() {
        let mut t = super::super::theme::Terminal::default();
        t.reported = Some(format::Theme::Dark);
        assert_eq!(dim_default(Color::Default, true, &t, 50), format::dim_color(Color::Idx(7), 50));
        t.reported = Some(format::Theme::Light);
        assert_eq!(dim_default(Color::Default, false, &t, 50), format::dim_color(Color::Idx(7), 50));
        t.bg = Some(Color::Rgb(20, 40, 60));
        assert_eq!(dim_default(Color::Default, false, &t, 50), Color::Rgb(10, 20, 30));
    }
    #[test]
    fn eight_colour_style_downgrade_matches_tty_checks() {
        let f = super::super::features::compute("screen", "", &[], "");
        let style = fit_style(Style { fg: Color::Idx(9), bg: Color::Idx(15), ..Style::default() }, &f);
        assert_eq!((style.fg, style.bg, style.attrs & BOLD), (Color::Idx(1), Color::Idx(7), BOLD));
        let style = fit_style(Style { fg: Color::Idx(196), bg: Color::Idx(231), ..Style::default() }, &f);
        assert_eq!((style.fg, style.bg, style.attrs & BOLD), (Color::Idx(1), Color::Idx(7), 0));
        // colour240 is bright black, so black: not on black, which would hide it
        let style = fit_style(Style { fg: Color::Idx(240), bg: Color::Idx(0), ..Style::default() }, &f);
        assert_eq!((style.fg, style.bg), (Color::Idx(7), Color::Idx(0)));
        // the swap looks at the bg before it is converted: colour233 is not black yet
        let style = fit_style(Style { fg: Color::Idx(240), bg: Color::Idx(233), ..Style::default() }, &f);
        assert_eq!((style.fg, style.bg), (Color::Idx(0), Color::Idx(0)));
        // colour244 is white in colour_256to16, so nothing is swapped
        let style = fit_style(Style { fg: Color::Idx(244), bg: Color::Idx(7), ..Style::default() }, &f);
        assert_eq!((style.fg, style.bg), (Color::Idx(7), Color::Idx(7)));
        let f = super::super::features::compute("xterm-16color", "", &[], "");
        let style = fit_style(Style { fg: Color::Idx(196), bg: Color::Idx(231), ..Style::default() }, &f);
        assert_eq!((style.fg, style.bg), (Color::Idx(9), Color::Idx(15)));
    }
}

#[cfg(test)]
mod display_tests {
    use super::*;
    #[test]
    fn message_format_preserves_status_unless_filled() {
        assert_eq!(message_area(80, "width=25%,align=right"), (60, 20));
        assert_eq!(message_area(80, "width=20,align=centre"), (30, 20));
        assert_eq!(message_area(80, "width=0"), (0, 80));
        let mut f = Frame::new(12, 2);
        f.text(0, 1, "status stays", Style::default(), 12);
        message_cells(&mut f, 1, "#[fg=red]Hi", Style::default());
        assert_eq!(f.cells[12..].iter().map(|c| c.text.as_str()).collect::<String>(), "Hiatus stays");
        message_cells(&mut f, 1, "#[align=right,fill=blue]bye", Style::default());
        assert_eq!(f.cells[12..].iter().map(|c| c.text.as_str()).collect::<String>(), "         bye");
        assert_eq!(f.cells[12].style.bg, Color::Idx(4));
    }
    #[test]
    fn menu_and_popup_border_lines() {
        for (kind, corner, line) in [("single", "┌", "─"), ("double", "╔", "═"), ("heavy", "┏", "━"), ("simple", "+", "-"), ("rounded", "╭", "─"), ("padded", " ", " "), ("none", " ", " ")] {
            let mut f = Frame::new(10, 6);
            draw_box_lines(&mut f, 0, 0, 10, 6, Style::default(), "", kind);
            assert_eq!(f.cells[0].text, corner);
            assert_eq!(f.cells[1].text, line);
        }
    }
}

#[cfg(test)]
mod border_tests {
    use super::*;
    use super::super::pan::tests::{clients, kill, run};

    fn rows(srv: &Server) -> Vec<String> {
        let f = srv.compose(1).unwrap().frame;
        (0..f.h).map(|y| f.row(y).iter().map(|c| c.text.as_str()).collect()).collect()
    }

    /// A 60x14 window without a status line: pane 0 on the left, 1 above 2 on the right.
    fn three_panes() -> Server {
        let (mut srv, _) = clients((60, 14), None);
        for args in [&["new-session", "-d", "-s", "s", "-x", "60", "-y", "14"][..], &["split-window", "-h", "-d"], &["split-window", "-v", "-d", "-t", "s:0.1"],
            &["set", "-g", "status", "off"], &["set", "-g", "pane-border-format", " #{pane_index} "], &["set", "-g", "pane-active-border-style", "fg=red"]] {
            run(&mut srv, 1, args).unwrap();
        }
        let sid = *srv.sessions.keys().next().unwrap();
        srv.attach_client(1, sid);
        srv
    }

    #[test]
    fn pane_status_lines_own_their_border_cells() {
        let mut srv = three_panes();
        run(&mut srv, 1, &["set", "-g", "pane-border-lines", "number"]).unwrap();
        run(&mut srv, 1, &["set", "-g", "pane-border-status", "top"]).unwrap();
        // the separator above pane 2 is its status line: drawn with its number, the
        // junction with the active pane's (redraw_mark_pane_borders)
        let r = rows(&srv);
        assert_eq!(&r[0][..36], "00 0 0000000000000000000000000011 1 ");
        assert_eq!(&r[7][29..36], " 022 2 ");
        run(&mut srv, 1, &["select-pane", "-t", "s:0.2"]).unwrap();
        // the first pane wins a cell two panes mark (they are marked last to first)
        assert_eq!(&rows(&srv)[0][..36], "00 0 0000000000000000000000000011 1 ");
        run(&mut srv, 1, &["set", "-g", "pane-border-status", "bottom"]).unwrap();
        let r = rows(&srv);
        assert_eq!(&r[7][29..36], " 111 1 ");
        assert_eq!(&r[13][..36], "00 0 0000000000000000000000000222 2 ");
        // the active style goes with the pane a cell is drawn for, not with any pane next to it
        run(&mut srv, 1, &["set", "-g", "pane-border-status", "top"]).unwrap();
        run(&mut srv, 1, &["set", "-g", "pane-border-lines", "single"]).unwrap();
        run(&mut srv, 1, &["select-pane", "-t", "s:0.1"]).unwrap();
        let f = srv.compose(1).unwrap().frame;
        let red = |x: u16, y: u16| f.get(x, y).unwrap().style.fg == Color::Idx(1);
        assert!(red(30, 7) && !red(31, 7) && !red(32, 7) && !red(34, 7));
        assert!(red(30, 0) && red(31, 0) && red(34, 0));
        kill(&mut srv);
    }

    /// A session of `w`x`h` without a status line, split by `splits`, attached to client 1.
    fn layout(w: &str, h: &str, splits: &[&[&str]]) -> Server {
        let (mut srv, _) = clients((w.parse().unwrap(), h.parse().unwrap()), None);
        run(&mut srv, 1, &["new-session", "-d", "-s", "s", "-x", w, "-y", h]).unwrap();
        for args in [&["set", "-g", "status", "off"][..], &["set", "-g", "pane-border-format", " #{pane_index} "], &["set", "-g", "pane-active-border-style", "fg=red"],
            &["set", "-g", "pane-border-lines", "number"]].iter().chain(splits) {
            run(&mut srv, 1, args).unwrap();
        }
        let sid = *srv.sessions.keys().next().unwrap();
        srv.attach_client(1, sid);
        srv
    }

    #[test]
    fn junction_cells_belong_to_the_first_pane_made() {
        // panes 0 top left, 1 below it, 2 on the right: the junction of 1's status
        // line with the separator is 2's (2 marks it after 1's line is made)
        let mut srv = layout("40", "10", &[&["split-window", "-h", "-d"], &["split-window", "-v", "-d", "-t", "s:0.0"], &["set", "-g", "pane-border-status", "bottom"]]);
        assert_eq!(rows(&srv)[9], "11 1 111111111111111222 2 22222222222222");
        kill(&mut srv);
        // split-window -b gives the newer pane the lower index: pane 3 was made before
        // pane 1, so the junction between them is 3's, drawn inactive for active pane 1
        let mut srv = layout("60", "14", &[&["split-window", "-h", "-d"], &["split-window", "-v", "-b", "-d", "-t", "s:0.1"], &["split-window", "-v", "-d", "-t", "s:0.0"],
            &["set", "-g", "pane-border-status", "top"], &["set", "-g", "pane-border-indicators", "colour"], &["select-pane", "-t", "s:0.1"]]);
        assert_eq!(rows(&srv)[7], "11 1 1111111111111111111111111333 3 333333333333333333333333");
        let f = srv.compose(1).unwrap().frame;
        let red = |x: u16, y: u16| f.get(x, y).unwrap().style.fg == Color::Idx(1);
        assert!(red(29, 7) && !red(30, 7) && red(30, 8));
        run(&mut srv, 1, &["set", "-g", "pane-border-status", "off"]).unwrap();
        assert_eq!(&rows(&srv)[7][24..36], "111111022222");
        kill(&mut srv);
    }
}

#[cfg(test)]
mod huge_float_tests {
    use super::*;
    use super::super::pan::tests::{clients, kill, run};

    #[test]
    fn a_huge_floating_pane_draws_only_the_rows_of_its_mode_on_the_client() {
        let _huge = super::super::float::HUGE_PANES.lock().unwrap_or_else(|e| e.into_inner());
        let (mut srv, _) = clients((80, 24), None);
        run(&mut srv, 1, &["new-session", "-d", "-s", "s", "-x", "80", "-y", "23"]).unwrap();
        let sid = *srv.sessions.keys().next().unwrap();
        srv.attach_client(1, sid);
        run(&mut srv, 1, &["new-pane", "-d", "-x", "4000", "-y", "4000", "-X", "2", "-Y", "2"]).unwrap();
        let wid = srv.client_window(1).unwrap();
        let float = srv.float_rects(wid)[0].0;
        let g = srv.panes[&float].generation;
        srv.pane_data(float, g, b"\x1b[2J\x1b[Hhello huge");
        run(&mut srv, 1, &["copy-mode", "-t", &format!("%{float}")]).unwrap();
        let Some(PaneMode::Copy(cm)) = &srv.panes[&float].mode else { panic!("no copy mode") };
        let (w, h) = srv.panes[&float].size;
        let (lines, _) = cm.render_rows(&srv.copy_render_style(float, Some(1)), 0..3);
        assert_eq!(lines.len(), h as usize);
        assert!(lines[0].cells.len() == w as usize && lines[3].cells.is_empty() && lines[h as usize - 1].cells.is_empty());
        let f = srv.compose(1).unwrap().frame;
        let row: String = f.row(3).iter().map(|c| c.text.as_str()).collect();
        assert!(row.contains("hello huge"), "{row}");
        srv.exit_mode(float);
        run(&mut srv, 1, &["choose-tree", "-t", &format!("%{float}")]).unwrap();
        let Some(PaneMode::Tree(t)) = &srv.panes[&float].mode else { panic!("no tree mode") };
        let (lines, _) = t.render_rows(&srv, w, h, 0..21);
        assert!(lines[20].cells.len() == w as usize && lines[21].cells.is_empty());
        let f = srv.compose(1).unwrap().frame;
        let row: String = f.row(3).iter().map(|c| c.text.as_str()).collect();
        assert!(row.contains("(0)") && row.contains("s:"), "{row}");
        kill(&mut srv);
    }
}
