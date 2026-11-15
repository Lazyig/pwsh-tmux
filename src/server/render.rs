//! Composes a client's screen (panes, borders, status line, overlays) and sends the diff.

use super::modes::{big_text, clock_text};
use super::vars::Target;
use super::*;
use crate::cells::{Cell, Color, Line, Style, BLINK, BOLD, DIM, INVERSE, ITALIC, STRIKE, UNDERLINE};
use crate::format::{self, Span};
use std::fmt::Write as _;
use unicode_width::UnicodeWidthChar;

// ConPTY counts decomposed accents as extra columns. Compose when Windows can do
// so, and retain the original text if the platform cannot normalize it.
fn terminal_text(text: &str) -> std::borrow::Cow<'_, str> {
    #[cfg(windows)]
    if text.chars().any(|c| c.width() == Some(0)) {
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
            if !cell.text.is_empty() && x + 1 < self.w && self.cells[i + 1].text.is_empty() && self.cells[i].text.chars().next().and_then(|c| c.width()) == Some(2) {
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
            let cw = ch.width().unwrap_or(0) as u16;
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
    /// modifyOtherKeys level requested from the outer terminal (extended-keys).
    pub extkeys: u8,
}

pub fn draw_box(f: &mut Frame, x: u16, y: u16, w: u16, h: u16, style: Style, title: &str) {
    draw_box_lines(f, x, y, w, h, style, title, "single");
}

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
        (ch.to_string(), ch.width().unwrap_or(0))
    }
}

fn message_area(width: u16, style: &str) -> (u16, u16) {
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
        Color::Idx(i) => {
            let _ = write!(out, ";{};5;{}", base + 8, i);
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";{};2;{};{};{}", base + 8, r, g, b);
        }
    }
}

/// Colours and attributes the client's terminal can show.
fn fit_style(mut st: Style, f: &super::features::Features) -> Style {
    st.fg = super::features::fit_color(st.fg, f);
    st.bg = super::features::fit_color(st.bg, f);
    if !f.has("strikethrough") {
        st.attrs &= !STRIKE;
    }
    st
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

const BORDER_SETS: [(&str, [&str; 11]); 4] = [
    // h, v, tl, tr, bl, br, cross, t-down, t-up, t-right, t-left
    ("single", ["─", "│", "┌", "┐", "└", "┘", "┼", "┬", "┴", "├", "┤"]),
    ("double", ["═", "║", "╔", "╗", "╚", "╝", "╬", "╦", "╩", "╠", "╣"]),
    ("heavy", ["━", "┃", "┏", "┓", "┗", "┛", "╋", "┳", "┻", "┣", "┫"]),
    ("simple", ["-", "|", "+", "+", "+", "+", "+", "+", "+", "+", "+"]),
];

impl Server {
    fn expand_status_time(&self, t: Target, cid: ClientId, template: &str) -> String {
        format::expand_status_time(template, &vars::VarCtx { srv: self, t, client: Some(cid), extra: HashMap::new() })
    }

    pub fn render_all(&mut self) {
        let ids: Vec<ClientId> = self.clients.iter().filter(|(_, c)| c.dirty && c.attached && !c.suspended && c.control == 0 && c.session.is_some()).map(|(id, _)| *id).collect();
        for id in ids {
            self.render(id);
        }
    }

    /// A style option for a target and client. As options_string_to_style, a style containing
    /// `#{` is expanded as a format first, so colours in styles can be formats (3.8).
    pub(super) fn style_option(&self, value: &str, base: Style, t: Target, cid: Option<ClientId>) -> Option<Style> {
        let ctx = vars::VarCtx { srv: self, t, client: cid, extra: HashMap::new() };
        format::parse_style_with(value, base, Some(&ctx)).ok().map(|p| p.style)
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
        let left = self.expand_status_time(cur, cid, &self.opt_session(Some(sid), "status-left"));
        let right = self.expand_status_time(cur, cid, &self.opt_session(Some(sid), "status-right"));
        let left_len = self.opt_session_num(Some(sid), "status-left-length").max(0) as usize;
        let right_len = self.opt_session_num(Some(sid), "status-right-length").max(0) as usize;
        let lcells = spans_cells(&format::spans(&left, left_style), left_len.min(width), base);
        let mut rcells = spans_cells(&format::spans(&right, right_style), right_len, base);
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
            let text = self.expand_status_time(t, cid, &fmt);
            let start = wcells.len();
            wcells.extend(spans_cells(&format::spans(&text, st), usize::MAX, st));
            wtags.resize(wcells.len(), Some(format::Range::Window(*idx)));
            if is_cur {
                cur_range = (start, wcells.len());
            }
            if k + 1 < n {
                let sep_start = wcells.len();
                wcells.extend(spans_cells(&format::spans(&sep, base), usize::MAX, base));
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
            if line[i].text.is_empty() && (i == 0 || line[i - 1].text.chars().next().and_then(|c| c.width()) != Some(2)) {
                line[i].text = " ".into();
            }
        }
        (line, tags)
    }

    fn compose(&self, cid: ClientId) -> Option<RenderState> {
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
                if ch.width() == Some(1) { Cell { text: ch.to_string(), style: over(s.style) } } else { Cell { text: " ".into(), style: inactive_border } }
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
            let wstyle = pst(if is_active { "window-active-style" } else { "window-style" });
            let wstyle = if is_active && wstyle == Style::default() { pst("window-style") } else { wstyle };
            let mode_lines: Option<(Vec<Line>, Option<(u16, u16)>)> = match &p.mode {
                Some(PaneMode::Copy(cm)) => {
                    let style = self.style_option(&self.opt_window(Some(wid), "mode-style"), Style::default(), pane_target, Some(cid)).unwrap_or_default();
                    let (lines, cur) = cm.render_with_style(style);
                    Some((lines, Some((cur.1, cur.0))))
                }
                Some(PaneMode::Tree(t)) => Some((t.render(self, cr.w, cr.h), None)),
                Some(PaneMode::Clock) => Some((Vec::new(), None)),
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
                let colour = self.style_option(&format!("fg={}", self.opt_window(Some(wid), "clock-mode-colour")), Style::default(), pane_target, Some(cid)).unwrap_or_default().fg;
                let text = clock_text(&self.opt_window(Some(wid), "clock-mode-style"));
                let rr = Rect { y: oy + cr.y, h: cr.h.saturating_sub(1), ..cr };
                if cr.h < 6 || !big_text(&mut f, rr, &text, colour) {
                    f.text(cr.x, oy + cr.y + cr.h / 2, &text, Style { fg: colour, ..Style::default() }, cr.x + cr.w);
                }
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
                        modes.cursor_style = scr.cursor_style() as u8;
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
            let mut r = *r;
            if border_status == "top" && r.y == 0 && r.h > 1 { r.y += 1; r.h -= 1; }
            if border_status == "bottom" && r.bottom() == win.sy && r.h > 1 { r.h -= 1; }
            if let Some(sb) = self.scrollbar(wid, *id, r) { super::display::draw_scrollbar(&mut f, sb, oy); }
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
                // redraw_mark_border_arrows: every pane marks the border cells one
                // in from its corners; a marked cell next to the active pane points at it
                let marked = || rects.iter().any(|(_, p)| (x == p.x + 1 && (y + 1 == p.y || y == p.bottom())) || (y == p.y + 1 && (x + 1 == p.x || x == p.right())));
                let arrow = if matches!(indicators.as_str(), "arrows" | "both") && marked() {
                    active_rect.and_then(|a| {
                        let inside = |px: i32, py: i32| px >= a.x as i32 && px < a.right() as i32 && py >= a.y as i32 && py < a.bottom() as i32;
                        if inside(xi - 1, yi) { Some("←") }
                        else if inside(xi + 1, yi) { Some("→") }
                        else if inside(xi, yi - 1) { Some("↑") }
                        else if inside(xi, yi + 1) { Some("↓") }
                        else { None }
                    })
                } else { None };
                let text = if let Some(arrow) = arrow { arrow.into() } else if lines_kind == "number" {
                    // The pane below or to the right owns a shared border.
                    let panes = win.panes();
                    let active = panes.iter().position(|p| *p == win.active).unwrap_or(0);
                    (0..panes.len()).map(|i| (active+i)%panes.len()).find(|i| {
                        rects.iter().find(|(p,_)| *p==panes[*i]).is_some_and(|(_,a)|
                            (x == a.right() && y >= a.y && y <= a.bottom()) ||
                            (x + 1 == a.x && y >= a.y && y <= a.bottom()) ||
                            (y == a.bottom() && x >= a.x && x <= a.right()) ||
                            (y + 1 == a.y && x >= a.x && x <= a.right()))
                    }).map(|i| char::from(b'0' + (i%10) as u8).to_string()).unwrap_or_else(|| "*".into())
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
                for x in r.x..r.right() {
                    if x >= r.x + 2 || !at(x as i32, by as i32) { f.set(x, oy + by, Cell { text: bset[0].into(), style: st }); }
                }
                let text = self.expand_for_pane(*id, &self.opt_window(Some(wid), "pane-border-format"));
                let cells = spans_cells(&format::spans(&text, st), r.w.saturating_sub(2) as usize, st);
                f.put_cells(r.x + 2, oy + by, &cells);
            }
        }
        if border_status != "off" {
            let by = if border_status == "top" { 0 } else { area_h.saturating_sub(1) };
            for sep in &seps {
                if sep.vertical && (if by == 0 { sep.y == 0 } else { sep.y + sep.len == area_h }) {
                    if let Some(cell) = f.get(sep.x, oy + by).cloned() {
                        f.set(sep.x, oy + by, Cell { text: (if by == 0 { bset[7] } else { bset[8] }).into(), ..cell });
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
        // overlays
        if let Some(ov) = &c.overlay {
            match ov {
                Overlay::DisplayPanes { .. } => {
                    cursor = None;
                    let colour = format::parse_color(&self.opt_window(Some(wid), "display-panes-colour")).unwrap_or(Color::Idx(4));
                    let acolour = format::parse_color(&self.opt_window(Some(wid), "display-panes-active-colour")).unwrap_or(Color::Idx(1));
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
                    m.draw(&mut f, st, bst, selected, &self.opt_window(Some(wid), "menu-border-lines"));
                    cursor = None;
                }
                Overlay::Popup(pp) => {
                    let st = self.style_option(&self.opt_window(Some(wid), "popup-style"), Style::default(), active_target, Some(cid)).unwrap_or_default();
                    let bst = self.style_option(&self.opt_window(Some(wid), "popup-border-style"), Style::default(), active_target, Some(cid)).unwrap_or_default();
                    for y in pp.y..pp.y + pp.h {
                        for x in pp.x..pp.x + pp.w {
                            f.set(x, y, Cell { text: " ".into(), style: st });
                        }
                    }
                    if pp.border {
                        draw_box_lines(&mut f, pp.x, pp.y, pp.w, pp.h, bst, &pp.title, &self.opt_window(Some(wid), "popup-border-lines"));
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
                    let text = self.expand_status_time(cur, cid, custom);
                    format::layout_line_ranged(&format::spans(&text, base), cw as usize, base)
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
            // tmux 3.8 prompt_layout/prompt_draw: message-format is expanded with
            // #{message} set to the prompt string and drawn as a label, the input
            // follows it; both are placed in the message-style area by the
            // effective style (message-command-style in vi command mode).
            let style_text = self.expand_time(active_target, Some(cid), &self.opt_session(Some(sid), "message-style"));
            let (ax, aw) = message_area(cw, &style_text);
            let label = pr.label.clone() + if matches!(pr.kind, PromptKind::Confirm) || pr.label == ":" || pr.label.ends_with(' ') || pr.label.is_empty() { "" } else { " " };
            let flags = [(pr.single_key, "SINGLE"), (pr.numeric, "NUMERIC"), (pr.copy_incremental.is_some(), "INCREMENTAL"), (pr.bspace_exit, "BSPACE_EXIT"), (pr.vi_command, "COMMANDMODE")];
            let extra = HashMap::from([
                ("prompt_input".to_string(), pr.input.iter().collect::<String>()),
                ("prompt_flags".to_string(), flags.into_iter().filter_map(|(on, flag)| on.then_some(flag)).collect::<Vec<_>>().join(",")),
                ("command_prompt".to_string(), if pr.vi_command { "1" } else { "0" }.to_string()),
                ("message".to_string(), label),
            ]);
            let vars = super::vars::VarCtx { srv: self, t: active_target, client: Some(cid), extra };
            let effective = format::expand_time(&self.opt_session(Some(sid), if pr.vi_command { "message-command-style" } else { "message-style" }), &vars);
            let sy = self.style_option(&effective, Style::default(), active_target, Some(cid)).unwrap_or_default();
            let (fill, align) = style_fill_align(&effective);
            let expanded = format::expand_time(&self.opt_session(Some(sid), "message-format"), &vars);
            let spans = format::spans(&expanded, sy);
            let label_width = spans.iter().map(|s| format::width(&s.text)).sum::<usize>().min(aw as usize);
            let cw_of = |chars: &[char]| chars.iter().map(|ch| prompt_char(*ch).1).sum::<usize>();
            let (pcursor, pwidth) = (cw_of(&pr.input[..pr.cursor.min(pr.input.len())]), cw_of(&pr.input));
            let avail = aw as usize - label_width;
            let (offset, input_width) = if avail == 0 { (0, 0) } else if pcursor >= avail { (pcursor - avail + 1, avail) } else { (0, pwidth.min(avail)) };
            let content_width = (label_width + input_width).min(aw as usize);
            let content_x = ax as usize + match align.as_str() {
                "centre" | "absolute-centre" => (aw as usize - content_width) / 2,
                "right" => aw as usize - content_width,
                _ => 0,
            };
            if let Some(bg) = fill {
                for x in ax..ax + aw { f.set(x, my, Cell { text: " ".into(), style: Style { bg, ..Style::default() } }); }
            }
            if label_width > 0 {
                f.put_cells(content_x as u16, my, &format::layout_line(&spans, label_width, sy));
            }
            // input from offset, control characters as ^X
            let (mut width, mut x) = (0, content_x + label_width);
            for ch in &pr.input {
                let (text, w) = prompt_char(*ch);
                if width < offset { width += w; continue; }
                if width + w > offset + input_width { break; }
                width += w;
                if text.len() == 2 && text.starts_with('^') {
                    f.set(x as u16, my, Cell { text: "^".into(), style: sy });
                    f.set(x as u16 + 1, my, Cell { text: text[1..].into(), style: sy });
                } else {
                    f.set(x as u16, my, Cell { text, style: sy });
                    if w == 2 { f.set(x as u16 + 1, my, Cell { text: String::new(), style: sy }); }
                }
                x += w;
            }
            cursor = Some(((content_x + label_width + pcursor - offset).min(cw as usize - 1) as u16, my));
        } else if let Some((msg, _)) = &c.message {
            let style_text = self.expand_time(active_target, Some(cid), &self.opt_session(Some(sid), "message-style"));
            let (ax, aw) = message_area(cw, &style_text);
            let extra = HashMap::from([("message".to_string(), msg.clone()), ("command_prompt".to_string(), "0".to_string())]);
            let vars = super::vars::VarCtx { srv: self, t: active_target, client: Some(cid), extra };
            let st = self.style_option(&style_text, Style::default(), active_target, Some(cid)).unwrap_or_default();
            let expanded = format::expand_time(&self.opt_session(Some(sid), "message-format"), &vars);
            let mut area = Frame::new(aw, 1);
            area.put_cells(0, 0, &f.cells[(my as usize * cw as usize + ax as usize)..(my as usize * cw as usize + ax as usize + aw as usize)]);
            message_cells(&mut area, 0, &expanded, st);
            f.put_cells(ax, my, &area.cells);
            cursor = None;
        }

        modes.app_cursor = true;
        modes.bracketed_paste = c.features.has("bpaste");
        Some(RenderState { frame: f, cursor, modes, status_ranges })
    }

    pub fn render(&mut self, cid: ClientId) {
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
        next.modes.extkeys = if feats.has("extkeys") {
            match ext_keys.as_str() {
                "always" => 2,
                "on" => 1,
                _ => 0,
            }
        } else {
            0
        };
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
                out.push_str(&terminal_text(&cell.text));
                if cfg!(windows) && cell.text.chars().any(|c| c.width() == Some(0)) {
                    let width = cell.text.chars().filter_map(|c| c.width()).sum::<usize>();
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
            let _ = write!(out, "\x1b]2;{}\x07", t);
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
            out.push_str(if m.bracketed_paste { "\x1b[?2004h" } else { "\x1b[?2004l" });
            out.push_str(match m.mouse {
                0 => "\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l",
                1000 => "\x1b[?1003l\x1b[?1002l\x1b[?1000h\x1b[?1006h",
                1002 => "\x1b[?1003l\x1b[?1000h\x1b[?1002h\x1b[?1006h",
                _ => "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h",
            });
            out.push_str(if m.focus { "\x1b[?1004h" } else { "\x1b[?1004l" });
            if feats.has("cstyle") {
                let _ = write!(out, "\x1b[{} q", m.cursor_style);
            }
            if pm.map_or(0, |p| p.extkeys) != m.extkeys {
                // modifyOtherKeys: have the terminal report keys like C-Enter distinctly
                out.push_str(match m.extkeys {
                    2 => "\x1b[>4;2m",
                    1 => "\x1b[>4;1m",
                    _ => "\x1b[>4m",
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
