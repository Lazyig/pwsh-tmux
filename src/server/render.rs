//! Composes a client's screen (panes, borders, status line, overlays) and sends the diff.

use super::modes::{big_text, clock_text};
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
}

impl Frame {
    pub fn new(w: u16, h: u16) -> Frame {
        Frame { w, h, cells: vec![Cell::default(); w as usize * h as usize] }
    }
    pub fn set(&mut self, x: u16, y: u16, cell: Cell) {
        if x < self.w && y < self.h {
            let i = y as usize * self.w as usize + x as usize;
            // Overwriting half of a wide char: blank the other half.
            if self.cells[i].text.is_empty() && x > 0 {
                self.cells[i - 1] = Cell { text: " ".into(), style: self.cells[i - 1].style };
            }
            if !cell.text.is_empty() && x + 1 < self.w && self.cells[i + 1].text.is_empty() && self.cells[i].text.chars().next().and_then(|c| char_width(c)) == Some(2) {
                self.cells[i + 1] = Cell { text: " ".into(), style: self.cells[i + 1].style };
            }
            self.cells[i] = cell;
        }
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
}

pub fn draw_box(f: &mut Frame, x: u16, y: u16, w: u16, h: u16, style: Style, title: &str) {
    draw_box_lines(f, x, y, w, h, style, title, "single");
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
            Some(format::Align::Centre | format::Align::AbsoluteCentre) => x + (w.saturating_sub(cells.len() as u16)) / 2,
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

/// The panes around a border cell (redraw span top_wp, bottom_wp, left_wp, right_wp).
#[derive(Default)]
struct BorderOwners {
    above: Option<PaneId>,
    below: Option<PaneId>,
    left: Option<PaneId>,
    right: Option<PaneId>,
    active: bool,
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
    out.push_str("\x1b[0");
    for (bit, code) in [(BOLD, ";1"), (DIM, ";2"), (ITALIC, ";3"), (UNDERLINE, ";4"), (BLINK, ";5"), (INVERSE, ";7"), (STRIKE, ";9")] {
        if st.attrs & bit != 0 {
            out.push_str(code);
        }
    }
    push_color(out, st.fg, 30);
    push_color(out, st.bg, 40);
    out.push('m');
}

/// capture-pane -e: text with SGR escapes.
pub fn line_with_escapes(line: &Line) -> String {
    let mut out = String::new();
    let mut cur = Style::default();
    for c in &line.cells {
        if c.text.is_empty() {
            continue;
        }
        if c.style != cur {
            push_sgr(&mut out, &c.style);
            cur = c.style;
        }
        out.push_str(&c.text);
    }
    let trimmed = out.trim_end_matches(' ').to_string();
    if cur != Style::default() { trimmed + "\x1b[0m" } else { trimmed }
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

impl Server {
    /// A status line format. As status.c's format_defaults(ft, c, NULL, NULL, NULL), its
    /// type is unknown (the window list entries are window formats); the target only
    /// fills in what the client's session would.
    fn expand_status_time(&self, t: Target, cid: ClientId, template: &str, ftype: &str) -> String {
        let extra = HashMap::from([(vars::FORMAT_TYPE.to_string(), ftype.to_string())]);
        format::expand_status_time(template, &vars::VarCtx { srv: self, t, client: Some(cid), extra })
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

    /// The default status line and the mouse range of each cell (status-left is `left`,
    /// each window entry `window|N` and status-right `right`, as in tmux's status-format).
    fn status_line(&self, cid: ClientId, sid: SessId, width: u16) -> (Vec<Cell>, Vec<Option<format::Range>>) {
        let s = &self.sessions[&sid];
        let width = width as usize;
        let base = self.status_style(sid, cid);
        let cur = Target { sid: Some(sid), idx: Some(s.active), wid: Some(s.active_window()), pane: Some(self.windows[&s.active_window()].active) };
        let left_style = self.style_option(&self.opt_session(Some(sid), "status-left-style"), base, cur, Some(cid)).unwrap_or(base);
        let right_style = self.style_option(&self.opt_session(Some(sid), "status-right-style"), base, cur, Some(cid)).unwrap_or(base);
        let left = self.expand_status_time(cur, cid, &self.opt_session(Some(sid), "status-left"), "");
        let right = self.expand_status_time(cur, cid, &self.opt_session(Some(sid), "status-right"), "");
        let left_len = self.opt_session_num(Some(sid), "status-left-length").max(0) as usize;
        let right_len = self.opt_session_num(Some(sid), "status-right-length").max(0) as usize;
        let lcells = spans_cells(&self.styled_spans(&left, left_style, cur, cid), left_len.min(width), base);
        let mut rcells = spans_cells(&self.styled_spans(&right, right_style, cur, cid), right_len, base);
        let available = width.saturating_sub(lcells.len());
        if rcells.len() > available { rcells.drain(..rcells.len() - available); }

        // window list
        let sep = self.opt_window(None, "window-status-separator");
        let mut wcells: Vec<Cell> = Vec::new();
        let mut wtags: Vec<Option<format::Range>> = Vec::new();
        let mut cur_range = (0, 0);
        let n = s.links.len();
        for (k, (idx, wid)) in s.links.iter().enumerate() {
            let wid = *wid;
            let t = Target { sid: Some(sid), idx: Some(*idx), wid: Some(wid), pane: Some(self.windows[&wid].active) };
            let is_cur = *idx == s.active;
            let fmt = self.opt_window(Some(wid), if is_cur { "window-status-current-format" } else { "window-status-format" });
            let wst = |name: &str, base: Style| self.style_option(&self.opt_window(Some(wid), name), base, t, Some(cid)).unwrap_or(base);
            let mut st = wst("window-status-style", base);
            if is_cur {
                st = wst("window-status-current-style", st);
            } else if s.last.first() == Some(idx) {
                st = wst("window-status-last-style", st);
            }
            let alerts = s.alerts.get(idx).copied().unwrap_or(0);
            if alerts & ALERT_BELL != 0 {
                st = wst("window-status-bell-style", st);
            } else if alerts & (ALERT_ACTIVITY | ALERT_SILENCE) != 0 {
                st = wst("window-status-activity-style", st);
            }
            let text = self.expand_status_time(t, cid, &fmt, "W");
            let start = wcells.len();
            wcells.extend(spans_cells(&self.styled_spans(&text, st, t, cid), usize::MAX, st));
            wtags.resize(wcells.len(), Some(format::Range::Window(*idx)));
            if is_cur {
                cur_range = (start, wcells.len());
            }
            if k + 1 < n {
                let sep_start = wcells.len();
                wcells.extend(spans_cells(&self.styled_spans(&sep, base, t, cid), usize::MAX, base));
                wtags.resize(wcells.len(), None);
                // tmux's window range (closed by #[norange]) covers one cell more
                if let Some(t) = wtags.get_mut(sep_start) {
                    *t = Some(format::Range::Window(*idx));
                }
            }
        }
        let avail = width.saturating_sub(lcells.len() + rcells.len());
        // keep the current window visible when the list is too long
        let (wstart, wlen) = if wcells.len() <= avail {
            (0, wcells.len())
        } else {
            let centre = cur_range.0 + (cur_range.1 - cur_range.0) / 2;
            let start = centre.saturating_sub(avail / 2).min(wcells.len().saturating_sub(avail));
            (start, avail)
        };
        let mut wslice: Vec<Cell> = wcells[wstart..wstart + wlen].to_vec();
        if let Some(c) = wslice.first_mut() {
            if c.text.is_empty() {
                *c = Cell { text: " ".into(), style: c.style };
            }
        }
        if wstart > 0 {
            if let Some(c) = wslice.first_mut() {
                *c = Cell { text: "<".into(), style: c.style };
            }
        }
        if wstart + wlen < wcells.len() {
            if let Some(c) = wslice.last_mut() {
                *c = Cell { text: ">".into(), style: c.style };
            }
        }
        let mut line = vec![Cell { text: " ".into(), style: base }; width];
        let mut tags: Vec<Option<format::Range>> = vec![None; width];
        let justify = self.opt_session(Some(sid), "status-justify");
        let wpos = match justify.as_str() {
            "centre" => lcells.len() + (avail - wslice.len()) / 2,
            "absolute-centre" => (width.saturating_sub(wslice.len()) / 2).max(lcells.len()),
            "right" => lcells.len() + avail - wslice.len(),
            _ => lcells.len(),
        };
        for (i, c) in lcells.into_iter().enumerate() {
            line[i] = c;
            tags[i] = Some(format::Range::Left);
        }
        for (i, c) in wslice.into_iter().enumerate() {
            if wpos + i < width {
                line[wpos + i] = c;
                tags[wpos + i] = wtags[wstart + i].clone();
            }
        }
        let rstart = width - rcells.len();
        for (i, c) in rcells.into_iter().enumerate() {
            line[rstart + i] = c;
            tags[rstart + i] = Some(format::Range::Right);
        }
        // a wide char cut by a region boundary
        for i in 0..width {
            if line[i].text.is_empty() && (i == 0 || line[i - 1].text.chars().next().and_then(|c| char_width(c)) != Some(2)) {
                line[i].text = " ".into();
            }
        }
        (line, tags)
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
        let (rects, seps) = self.rects(wid);
        let mut cursor = None;
        let mut status_ranges = Vec::new();
        let mut modes = Modes { mouse: self.outer_mouse_mode(cid), focus: options::is_on(&self.opt_server("focus-events")), ..Modes::default() };
        let active_rect = rects.iter().find(|(p, _)| *p == win.active).map(|(_, r)| *r);
        let area_h = ch.saturating_sub(nstatus);
        let border_status = self.opt_window(Some(wid), "pane-border-status");
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
                attrs: inactive_border.attrs | s.attrs,
            };
            let cell = spans.iter().find(|s| !s.text.is_empty()).and_then(|s| s.text.chars().next().map(|ch| {
                let ch = if text.contains("#[acs]") { match ch {
                    '~' => '·', 'q' => '─', 'x' => '│', 'l' => '┌', 'k' => '┐', 'm' => '└', 'j' => '┘', 'n' => '┼', 't' => '├', 'u' => '┤', 'v' => '┴', 'w' => '┬', _ => ch,
                } } else { ch };
                if char_width(ch) == Some(1) { Cell { text: ch.to_string(), style: over(s.style) } } else { Cell { text: " ".into(), style: inactive_border } }
            })).unwrap_or(Cell { text: " ".into(), style: inactive_border });
            for y in 0..area_h { for x in 0..cw {
                if (x < win.sx && y < win.sy) == inside { f.set(x, oy + y, cell.clone()); }
            } }
        }

        // panes
        for (id, r) in &rects {
            let Some(p) = self.panes.get(id) else { continue };
            let cr = self.content_rect(wid, *r);
            let is_active = *id == win.active;
            let pane_target = self.target_for_pane(*id);
            let pst = |name: &str| self.style_option(&self.opt_pane(*id, name), Style::default(), pane_target, Some(cid)).unwrap_or_default();
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
            let (default_fg, default_bg) = self.pane_style_colours(*id);
            if default_fg == super::theme::StyleColour::Terminal { wstyle.fg = Color::Default; }
            if default_bg == super::theme::StyleColour::Terminal { wstyle.bg = Color::Default; }
            let dim_ctx = vars::VarCtx { srv: self, t: pane_target, client: Some(cid), extra: HashMap::new() };
            let mut dim = format::parse_style_with(&self.opt_pane(*id, "window-style"), Style::default(), Some(&dim_ctx)).map(|p| p.dim).unwrap_or(0);
            if is_active {
                let active_dim = format::parse_style_with(&self.opt_pane(*id, "window-active-style"), Style::default(), Some(&dim_ctx)).map(|p| p.dim).unwrap_or(0);
                dim = active_dim;
            }
            let dim_style = |style: &mut Style| {
                if dim != 0 {
                    style.fg = dim_default(style.fg, true, &c.terminal, dim);
                    style.bg = dim_default(style.bg, false, &c.terminal, dim);
                }
            };
            let mode_lines: Option<(Vec<Line>, Option<(u16, u16)>)> = match &p.mode {
                Some(PaneMode::Copy(cm)) => {
                    let style = self.style_option(&self.opt_window(Some(wid), "mode-style"), Style::default(), pane_target, Some(cid)).unwrap_or_default();
                    let (lines, cur) = cm.render_with_style(style);
                    Some((lines, Some((cur.1, cur.0))))
                }
                Some(PaneMode::Tree(t)) => Some((t.render(self, cid, cr.w, cr.h), None)),
                // the clock's screen is cleared before the time is drawn
                Some(PaneMode::Clock) => Some((vec![Line { cells: vec![Cell::default(); cr.w as usize], wrapped: false }; cr.h as usize], None)),
                None => None,
            };
            match &mode_lines {
                Some((lines, _)) => {
                    for (y, line) in lines.iter().enumerate().take(cr.h as usize) {
                        for (x, cell) in line.cells.iter().enumerate().take(cr.w as usize) {
                            let mut cell = cell.clone();
                            overlay_style(&mut cell.style, wstyle);
                            f.set(cr.x + x as u16, oy + cr.y + y as u16, cell);
                        }
                    }
                }
                None => {
                    let scr = p.screen();
                    for y in 0..cr.h {
                        for x in 0..cr.w {
                            let Some(vc) = scr.cell(y, x) else { continue };
                            let mut cell = crate::cells::cell_from_vt(vc);
                            if vc.is_wide() && (x + 1 >= cr.w || cr.x + x + 1 >= cw) {
                                cell.text = " ".into();
                            }
                            overlay_style(&mut cell.style, wstyle);
                            f.set(cr.x + x, oy + cr.y + y, cell);
                        }
                    }
                }
            }
            if matches!(p.mode, Some(PaneMode::Clock)) {
                let ctx = vars::VarCtx { srv: self, t: pane_target, client: Some(cid), extra: HashMap::new() };
                let value = format::expand(&self.opt_window(Some(wid), "clock-mode-colour"), &ctx);
                let colour = format::parse_color_with(&value, Some(&ctx)).unwrap_or(Color::Idx(4));
                let text = clock_text(&self.opt_window(Some(wid), "clock-mode-style"));
                let rr = Rect { y: oy + cr.y, h: cr.h.saturating_sub(1), ..cr };
                // too small for the big digits: the plain text, centred, if it fits
                let len = text.chars().count() as u16;
                if (cr.h < 6 || !super::modes::big_clock(&mut f, rr, &text, colour)) && cr.w >= len && cr.h > 0 {
                    f.text(cr.x + cr.w / 2 - len / 2, oy + cr.y + cr.h / 2, &text, Style { fg: colour, bg: wstyle.bg, ..Style::default() }, cr.x + cr.w);
                }
            }
            if dim != 0 {
                for y in 0..cr.h { for x in 0..cr.w {
                    if let Some(cell) = f.get(cr.x + x, oy + cr.y + y).cloned() {
                        let mut cell = cell;
                        dim_style(&mut cell.style);
                        f.set(cr.x + x, oy + cr.y + y, cell);
                    }
                } }
            }
            if is_active {
                match &mode_lines {
                    Some((_, Some((cx, cy)))) => {
                        if *cx < cr.w && *cy < cr.h {
                            cursor = Some((cr.x + cx, oy + cr.y + cy));
                        }
                    }
                    Some(_) => {}
                    None => {
                        let scr = p.screen();
                        modes.app_cursor = scr.application_cursor();
                        modes.bracketed_paste = scr.bracketed_paste();
                        modes.cursor_style = merged_cursor_style(scr.cursor_style() as u8, scr.cursor_blinking(), features::cursor_style(&self.opt_pane(*id, "cursor-style")));
                        let colour = p.parser.callbacks().osc.cursor_colour.clone().unwrap_or_else(|| self.opt_pane(*id, "cursor-colour"));
                        // a colour option, not a format (OPTIONS_TABLE_IS_COLOUR)
                        modes.cursor_colour = format::parse_color(&colour).filter(|c| *c != vt100::Color::Default);
                        if !scr.hide_cursor() {
                            let (cy, cx) = scr.cursor_position();
                            if cx < cr.w && cy < cr.h {
                                cursor = Some((cr.x + cx, oy + cr.y + cy));
                            }
                        }
                    }
                }
            }
        }

        for (id, r) in &rects {
            if let Some(sb) = self.pane_scrollbar(wid, *id, *r) { super::display::draw_scrollbar(&mut f, sb, oy); }
        }

        // borders with junctions
        let mut is_border = vec![false; cw as usize * area_h as usize];
        // the window's right and bottom edges are borders when the client is larger
        let mut edges = Vec::new();
        if cw > win.sx { edges.push(crate::layout::Sep { vertical: true, x: win.sx, y: 0, len: win.sy + u16::from(area_h > win.sy) }); }
        if area_h > win.sy { edges.push(crate::layout::Sep { vertical: false, x: 0, y: win.sy, len: win.sx + u16::from(cw > win.sx) }); }
        for sep in seps.iter().chain(&edges) {
            for k in 0..sep.len {
                let (x, y) = if sep.vertical { (sep.x, sep.y + k) } else { (sep.x + k, sep.y) };
                if x < cw && y < area_h {
                    is_border[y as usize * cw as usize + x as usize] = true;
                }
            }
        }
        let at = |x: i32, y: i32| -> bool { x >= 0 && y >= 0 && (x as u16) < cw && (y as u16) < area_h && is_border[y as usize * cw as usize + x as usize] };
        // Each pane's border runs round its layout cell, from the pane status line
        // (top or bottom) when there is one; scrollbars sit inside it.
        let border_rects: Vec<(PaneId, Rect, i32, i32, i32, i32)> = rects.iter().map(|(p, r)| {
            let cr = self.content_rect(wid, *r);
            (*p, cr, r.x as i32 - 1, r.right() as i32, cr.y as i32 - 1, cr.bottom() as i32)
        }).collect();
        // The panes a border cell belongs to, by side: the pane above, below, to the
        // left or right of it (top_wp, bottom_wp, left_wp, right_wp), and whether one
        // of them is the active pane.
        let border_owners = |x: i32, y: i32| -> BorderOwners {
            let mut o = BorderOwners::default();
            for (p, _, l, r, t, b) in &border_rects {
                if x < *l || x > *r || y < *t || y > *b || (x > *l && x < *r && y > *t && y < *b) { continue; }
                if y == *t { o.below = Some(*p); }
                if y == *b { o.above = Some(*p); }
                if x == *l { o.right = Some(*p); }
                if x == *r { o.left = Some(*p); }
                o.active |= *p == win.active;
            }
            o
        };
        // redraw_mark_border_arrows: each pane marks the border cells one in from its corners
        let border_marked = |x: u16, y: u16| -> bool {
            let (x, y) = (x as i32, y as i32);
            border_rects.iter().any(|(_, cr, l, r, t, b)| (x == cr.x as i32 + 1 && (y == *t || y == *b)) || (y == cr.y as i32 + 1 && (x == *l || x == *r)))
        };
        // pane-border-lines number: window_pane_index of the pane the cell is drawn
        // for (redraw_get_pane_for_border_style): with two panes and colour
        // indicators the pane of that half, else the active pane if the cell borders
        // it, else the pane above, below, left or right of it
        let two_pane = matches!(indicators.as_str(), "colour" | "both") && rects.len() == 2 && !win.zoomed;
        let base = self.opt_window_num(Some(wid), "pane-base-index").max(0) as usize;
        let border_number = |x: i32, y: i32| -> String {
            let o = border_owners(x, y);
            let split = if !two_pane { None } else if rects[0].1.y == rects[1].1.y {
                o.left.zip(o.right).map(|(l, r)| if y <= win.sy as i32 / 2 { l } else { r })
            } else {
                o.above.zip(o.below).map(|(a, b)| if x <= win.sx as i32 / 2 { a } else { b })
            };
            let owner = split.or(if o.active { Some(win.active) } else { o.above.or(o.below).or(o.left).or(o.right) });
            owner.and_then(|o| win.panes().iter().position(|p| *p == o)).map(|i| char::from(b'0' + ((i + base) % 10) as u8).to_string()).unwrap_or_else(|| "*".into())
        };
        for y in 0..area_h {
            for x in 0..cw {
                if !is_border[y as usize * cw as usize + x as usize] {
                    continue;
                }
                let (xi, yi) = (x as i32, y as i32);
                let (u, d, l, r) = (at(xi, yi - 1), at(xi, yi + 1), at(xi - 1, yi), at(xi + 1, yi));
                let ch = match (u, d, l, r) {
                    (true, true, true, true) => bset[6],
                    (false, true, true, true) => bset[7],
                    (true, false, true, true) => bset[8],
                    (true, true, false, true) => bset[9],
                    (true, true, true, false) => bset[10],
                    (true, false, true, false) => bset[5],
                    (false, true, true, false) => bset[3],
                    (true, false, false, true) => bset[4],
                    (false, true, false, true) => bset[2],
                    (true, _, false, false) | (_, true, false, false) => bset[1],
                    _ => bset[0],
                };
                let edge = x == win.sx || y == win.sy;
                let mut near = active_rect.is_some_and(|a| x + 1 >= a.x && x <= a.right() && y + 1 >= a.y && y <= a.bottom()) && (rects.len() > 1 || edge);
                if near && !edge && rects.len() == 2 && matches!(indicators.as_str(), "colour" | "both") {
                    if let Some(a) = active_rect {
                        if rects[0].1.y == rects[1].1.y {
                            near = if a.x == 0 { y <= a.h / 2 } else { y > a.h / 2 };
                        } else if border_status == "off" {
                            near = if a.y == 0 { x <= a.w / 2 } else { x > a.w / 2 };
                        }
                    }
                }
                let arrow = if matches!(indicators.as_str(), "arrows" | "both") && border_marked(x, y) {
                    let o = border_owners(xi, yi);
                    let active = Some(win.active);
                    if o.left == active { Some("←") }
                    else if o.right == active { Some("→") }
                    else if o.above == active { Some("↑") }
                    else if o.below == active { Some("↓") }
                    else { None }
                } else { None };
                let text = if let Some(arrow) = arrow { arrow.into() } else if lines_kind == "number" {
                    border_number(xi, yi)
                } else { ch.into() };
                f.set(x, oy + y, Cell { text, style: if near { active_border } else { inactive_border } });
            }
        }
        // Internal separators double as pane titles; only the outer row
        // consumes pane space.
        if border_status != "off" {
            for (id, r) in &rects {
                let cr = self.content_rect(wid, *r);
                let by = if border_status == "top" { cr.y.saturating_sub(1) } else { cr.bottom() };
                let st = if *id == win.active { active_border } else { inactive_border };
                // window_make_pane_status: the line starts two in from the pane
                // (after a left scrollbar), filled with the pane's border cells, and
                // the format is drawn over it with its alignment
                let sx = cr.x + 2;
                let fill = |x: u16| if lines_kind == "number" { border_number(x as i32, by as i32) } else { bset[0].to_string() };
                for x in r.x..r.right() {
                    if x >= sx || !at(x as i32, by as i32) { f.set(x, oy + by, Cell { text: fill(x), style: st }); }
                }
                // the cell before the line is an arrow when the pane is the active one
                if matches!(indicators.as_str(), "arrows" | "both") && *id == win.active && cr.x + 1 < r.right() {
                    f.set(cr.x + 1, oy + by, Cell { text: (if border_status == "top" { "↓" } else { "↑" }).into(), style: st });
                }
                if sx < r.right() {
                    let text = self.expand_for_pane(*id, &self.opt_window(Some(wid), "pane-border-format"));
                    // no public style attribute uses bit 7: it marks cells the format left empty
                    let marker = Style { attrs: 0x80, ..Style::default() };
                    let cells = format::layout_line(&self.styled_spans(&text, st, self.target_for_pane(*id), cid), (r.right() - sx) as usize, marker);
                    for (i, cell) in cells.into_iter().enumerate() {
                        if cell.style != marker { f.set(sx + i as u16, oy + by, cell); }
                    }
                }
            }
        }
        if border_status != "off" {
            let by = if border_status == "top" { 0 } else { area_h.saturating_sub(1) };
            for sep in &seps {
                if sep.vertical && (if by == 0 { sep.y == 0 } else { sep.y + sep.len == area_h }) {
                    if let Some(cell) = f.get(sep.x, oy + by).cloned() {
                        let text = if lines_kind == "number" { border_number(sep.x as i32, by as i32) } else { (if by == 0 { bset[7] } else { bset[8] }).into() };
                        f.set(sep.x, oy + by, Cell { text, ..cell });
                    }
                }
            }
        }
        // marked pane: reverse its borders
        if let Some(m) = self.marked.filter(|m| win.panes().contains(m)) {
            if let Some((_, r)) = rects.iter().find(|(p, _)| *p == m) {
                for y in r.y.saturating_sub(1)..=r.bottom().min(area_h.saturating_sub(1)) {
                    for x in r.x.saturating_sub(1)..=r.right().min(cw - 1) {
                        if at(x as i32, y as i32) {
                            if let Some(cell) = f.get(x, oy + y).cloned() {
                                f.set(x, oy + y, Cell { style: Style { attrs: cell.style.attrs | INVERSE, ..cell.style }, ..cell });
                            }
                        }
                    }
                }
            }
        }
        // pane prompts over the last line of their panes (the first with the
        // status line at the top, redraw_draw_pane_prompt)
        for (id, r) in &rects {
            let Some(pr) = self.panes.get(id).and_then(|p| p.prompt.as_ref()) else { continue };
            let cr = self.content_rect(wid, *r);
            if cr.w == 0 || cr.h == 0 { continue; }
            let row = if top && nstatus > 0 { cr.y } else { cr.bottom() - 1 };
            let mut line = Frame::new(cr.w, 1);
            let x = self.draw_prompt(&mut line, cid, pr, 0, cr.w, 0);
            f.put_cells(cr.x, oy + row, &line.cells);
            if *id == win.active {
                cursor = Some((cr.x + x.min(cr.w - 1), oy + row));
                self.prompt_cursor(&mut modes, sid, pr);
            }
        }

        // status lines
        if nstatus > 0 {
            let sy0 = if top { 0 } else { ch - nstatus };
            let formats: Vec<String> = (0..nstatus).map(|i| self.opt_session(Some(sid), &format!("status-format[{i}]"))).collect();
            for i in 0..nstatus {
                let y = sy0 + i;
                let custom = &formats[i as usize];
                // The built-in first line has a dedicated renderer for list clipping.
                let (line, tags) = if !custom.is_empty() && !(i == 0 && custom == options::STATUS_FORMATS[0]) {
                    let base = self.status_style(sid, cid);
                    let cur = Target { sid: Some(sid), idx: Some(s.active), wid: Some(wid), pane: Some(win.active) };
                    let text = self.expand_status_time(cur, cid, custom, "");
                    format::layout_line_ranged(&self.styled_spans(&text, base, cur, cid), cw as usize, base)
                } else if i == 0 {
                    self.status_line(cid, sid, cw)
                } else {
                    (vec![Cell { text: " ".into(), style: self.status_style(sid, cid) }; cw as usize], vec![None; cw as usize])
                };
                f.put_cells(0, y, &line);
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
        if let Some(ov) = &c.overlay {
            match ov {
                Overlay::DisplayPanes { .. } => {
                    cursor = None;
                    let ctx = vars::VarCtx { srv: self, t: active_target, client: Some(cid), extra: HashMap::new() };
                    let colour = |name| format::parse_color_with(&format::expand(&self.opt_window(Some(wid), name), &ctx), Some(&ctx)).unwrap_or(Color::Default);
                    let acolour = colour("display-panes-active-colour");
                    let colour = colour("display-panes-colour");
                    let base = self.opt_window_num(Some(wid), "pane-base-index").max(0) as usize;
                    for (i, (id, r)) in rects.iter().enumerate() {
                        let col = if *id == win.active { acolour } else { colour };
                        let label = (i + base).to_string();
                        let rr = Rect { y: oy + r.y, ..*r };
                        if !big_text(&mut f, rr, &label, col) {
                            f.text(r.x + r.w / 2, oy + r.y + r.h / 2, &label, Style { fg: col, ..Style::default() }, r.right());
                        }
                        let size = format!("{}x{}", r.w, r.h);
                        f.text(r.right().saturating_sub(size.len() as u16), oy + r.y, &size, Style { fg: col, ..Style::default() }, r.right());
                    }
                }
                Overlay::Menu(m) => {
                    let ost = |name: &str, base: Style| self.style_option(&self.opt_window(Some(wid), name), base, active_target, Some(cid)).unwrap_or(base);
                    let st = ost("popup-style", Style::default());
                    let bst = ost("popup-border-style", Style::default());
                    let st = ost("menu-style", st);
                    let bst = ost("menu-border-style", bst);
                    let selected = ost("menu-selected-style", Style { fg: Color::Idx(0), bg: Color::Idx(3), ..Style::default() });
                    // display-menu -s -S -H -b
                    let given = |v: &Option<String>, base: Style| self.flag_style(v.as_deref(), base, active_target, cid);
                    let (st, bst, selected) = (given(&m.look.style, st), given(&m.look.border_style, bst), given(&m.look.selected_style, selected));
                    let lines = m.look.lines.clone().unwrap_or_else(|| self.opt_window(Some(wid), "menu-border-lines"));
                    m.draw(&mut f, st, bst, selected, &lines);
                    cursor = None;
                }
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
                            for x in 0..iw {
                                if let Some(vc) = scr.cell(y, x) {
                                    let mut cell = crate::cells::cell_from_vt(vc);
                                    if vc.is_wide() && x + 1 >= iw {
                                        cell.text = " ".into();
                                    }
                                    overlay_style(&mut cell.style, st);
                                    f.set(ix + x, iy + y, cell);
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

        modes.appesc = c.features.has("appesc");
        modes.app_cursor = true;
        modes.bracketed_paste = c.features.has("bpaste");
        Some(RenderState { frame: f, cursor, modes, status_ranges })
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
        let flags = [(pr.single_key, "SINGLE"), (pr.numeric, "NUMERIC"), (pr.incremental || pr.copy_incremental.is_some(), "INCREMENTAL"), (pr.bspace_exit, "BSPACE_EXIT"), (pr.vi_command, "COMMANDMODE"), (pr.host.is_some(), "ISPANE")];
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
        if prev.is_none() {
            out.push_str("\x1b[0m\x1b[H\x1b[2J");
        }
        let mut changed = false;
        for y in 0..next.frame.h {
            let row = next.frame.row(y);
            if prev.is_some_and(|p| p.frame.row(y) == row) {
                continue;
            }
            changed = true;
            let _ = write!(out, "\x1b[{};1H", y + 1);
            let mut cur: Option<Style> = None;
            // Like tmux, blank default-background cells at the end of the row are erased
            // with EL instead of being written as spaces. Terminals that keep text per line
            // (the IntelliJ terminal's editor) then don't get full-width lines, which an
            // IME's composed-text inlay at the cursor would push into a soft wrap.
            let end = row.iter().rposition(|c| !erasable(c)).map_or(0, |i| i + 1);
            for (x, cell) in row.iter().enumerate().take(end) {
                if cell.text.is_empty() {
                    continue;
                }
                if cur != Some(cell.style) {
                    push_sgr(&mut out, &fit_style(cell.style, &feats));
                    cur = Some(cell.style);
                }
                if c.utf8() || cell.text.is_ascii() { out.push_str(&terminal_text(&cell.text)); }
                else { out.push_str(&non_utf8_text(&cell.text, row.get(x + 1).is_some_and(|n| n.text.is_empty()))); }
                if cfg!(windows) && cell.text.chars().any(|c| char_width(c) == Some(0)) {
                    let width = cell.text.chars().filter_map(|c| char_width(c)).sum::<usize>();
                    if x + width < next.frame.w as usize { let _ = write!(out, "\x1b[{};{}H", y + 1, x + width + 1); }
                }
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
        let cursor_changed = prev.is_none_or(|p| p.cursor != next.cursor);
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
