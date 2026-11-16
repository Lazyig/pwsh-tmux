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
    if w < 2 || h < 2 {
        return;
    }
    for i in x + 1..x + w - 1 {
        f.set(i, y, Cell { text: "─".into(), style });
        f.set(i, y + h - 1, Cell { text: "─".into(), style });
    }
    for j in y + 1..y + h - 1 {
        f.set(x, j, Cell { text: "│".into(), style });
        f.set(x + w - 1, j, Cell { text: "│".into(), style });
    }
    f.set(x, y, Cell { text: "┌".into(), style });
    f.set(x + w - 1, y, Cell { text: "┐".into(), style });
    f.set(x, y + h - 1, Cell { text: "└".into(), style });
    f.set(x + w - 1, y + h - 1, Cell { text: "┘".into(), style });
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

const BORDER_SETS: [(&str, [&str; 11]); 6] = [
    // h, v, tl, tr, bl, br, cross, t-down, t-up, t-right, t-left
    ("single", ["─", "│", "┌", "┐", "└", "┘", "┼", "┬", "┴", "├", "┤"]),
    ("double", ["═", "║", "╔", "╗", "╚", "╝", "╬", "╦", "╩", "╠", "╣"]),
    ("heavy", ["━", "┃", "┏", "┓", "┗", "┛", "╋", "┳", "┻", "┣", "┫"]),
    ("simple", ["-", "|", "+", "+", "+", "+", "+", "+", "+", "+", "+"]),
    // tmux window_get_border_cell draws both as blanks; the borders keep their cells
    ("spaces", [" "; 11]),
    ("none", [" "; 11]),
];

/// The pane a border cell is drawn for (tmux redraw_get_pane_for_border_style): the pane chosen
/// by the two-pane colouring, else the active pane if it borders the cell, else the pane above,
/// below, to the left or to the right, in that order. `rects` are the tiled panes; `status` the
/// window pane-border-status, which drops the upper (top) or lower (bottom) pane's own line.
/// `two` is the two-pane colouring split (left-right, window width and height) when it applies.
fn border_owner(rects: &[(PaneId, Rect)], active: PaneId, two: Option<(bool, u16, u16)>, status: &str, x: u16, y: u16) -> Option<PaneId> {
    let (cx, cy) = (x, y);
    let (x, y) = (x as i32, y as i32);
    let span_x = |r: &Rect| x >= r.x as i32 - 1 && x <= r.right() as i32;
    let span_y = |r: &Rect| y >= r.y as i32 - 1 && y <= r.bottom() as i32;
    let above = rects.iter().find(|(_, r)| status != "top" && y == r.bottom() as i32 && span_x(r)).map(|(p, _)| *p);
    let below = rects.iter().find(|(_, r)| status != "bottom" && y + 1 == r.y as i32 && span_x(r)).map(|(p, _)| *p);
    let left = rects.iter().find(|(_, r)| x == r.right() as i32 && span_y(r)).map(|(p, _)| *p);
    let right = rects.iter().find(|(_, r)| x + 1 == r.x as i32 && span_y(r)).map(|(p, _)| *p);
    match two {
        Some((true, _, sy)) if left.is_some() && right.is_some() => return if cy <= sy / 2 { left } else { right },
        Some((false, sx, _)) if above.is_some() && below.is_some() => return if cx <= sx / 2 { above } else { below },
        _ => {}
    }
    let all = [above, below, left, right];
    if all.contains(&Some(active)) {
        return Some(active);
    }
    all.into_iter().flatten().next()
}

/// The digit tmux draws for pane-border-lines number: the pane index, modulo 10.
fn border_number(panes: &[PaneId], base: usize, pane: Option<PaneId>) -> String {
    match pane.and_then(|p| panes.iter().position(|q| *q == p)) {
        Some(i) => char::from(b'0' + ((i + base) % 10) as u8).to_string(),
        None => "*".into(),
    }
}

impl Server {
    fn expand_status_time(&self, t: Target, cid: ClientId, template: &str) -> String {
        format::expand_status_time(template, &vars::VarCtx { srv: self, t, client: Some(cid), extra: HashMap::new() })
    }

    /// A pane status line (tmux window_make_pane_status): pane-border-format drawn over the
    /// border line in `width` cells; None cells keep the border. The ranges are what a click
    /// on the line hits (window_pane_status_get_range).
    pub fn pane_status_line(&self, id: PaneId, width: usize, base: Style) -> (Vec<Option<Cell>>, Vec<Option<format::Range>>) {
        let text = self.expand_for_pane(id, &self.opt_pane(id, "pane-border-format"));
        format::draw_line(&format::spans(&text, base), width, base)
    }

    pub fn render_all(&mut self) {
        let ids: Vec<ClientId> = self.clients.iter().filter(|(_, c)| c.dirty && c.attached && !c.suspended && c.control == 0 && c.session.is_some()).map(|(id, _)| *id).collect();
        for id in ids {
            self.render(id);
        }
    }

    /// A style option for a target and client. As options_string_to_style, a style containing
    /// `#{` is expanded as a format first, so colours in styles can be formats (3.8).
    fn style_option(&self, value: &str, base: Style, t: Target, cid: Option<ClientId>) -> Option<Style> {
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
                wcells.extend(spans_cells(&format::spans(&sep, base), usize::MAX, base));
                wtags.resize(wcells.len(), None);
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

    /// Draw a pane's content (or its mode) with its top left at (x0, y0) of the window,
    /// clipped to `clip` (window columns and rows on the client); sets the cursor and the
    /// terminal modes for the active pane.
    #[allow(clippy::too_many_arguments)]
    fn draw_pane_body(&self, f: &mut Frame, cid: ClientId, wid: WinId, id: PaneId, (x0, y0, w, h): (i32, i32, u16, u16), clip: (u16, u16), oy: u16, modes: &mut Modes, cursor: &mut Option<(u16, u16)>) {
        let win = &self.windows[&wid];
        let Some(p) = self.panes.get(&id) else { return };
        let cw = clip.0;
        // columns and rows of the pane that are on the client
        let vis = |x: u16, y: u16| -> Option<(u16, u16)> {
            let (ax, ay) = (x0 + x as i32, y0 + y as i32);
            (ax >= 0 && ay >= 0 && ax < clip.0 as i32 && ay < clip.1 as i32).then_some((ax as u16, ay as u16))
        };
        let cr = Rect { x: x0.max(0) as u16, y: y0.max(0) as u16, w, h };
        let is_active = id == win.active;
        let pane_target = self.target_for_pane(id);
        let pst = |name: &str| self.style_option(&self.opt_pane(id, name), Style::default(), pane_target, Some(cid)).unwrap_or_default();
        let wstyle = pst(if is_active { "window-active-style" } else { "window-style" });
        let wstyle = if is_active && wstyle == Style::default() { pst("window-style") } else { wstyle };
        let mode_lines: Option<(Vec<Line>, Option<(u16, u16)>)> = match &p.mode {
            Some(PaneMode::Copy(cm)) => {
                let style = self.style_option(&self.opt_window(Some(wid), "mode-style"), Style::default(), pane_target, Some(cid)).unwrap_or_default();
                let (lines, cur) = cm.render_with_style(style);
                Some((lines, Some((cur.1, cur.0))))
            }
            Some(PaneMode::Tree(t)) => Some((t.render(self, w, h), None)),
            Some(PaneMode::Clock) => Some((Vec::new(), None)),
            None => None,
        };
        match &mode_lines {
            Some((lines, _)) => {
                for (y, line) in lines.iter().enumerate().take(h as usize) {
                    for (x, cell) in line.cells.iter().enumerate().take(w as usize) {
                        let mut cell = cell.clone();
                        overlay_style(&mut cell.style, wstyle);
                        if let Some((ax, ay)) = vis(x as u16, y as u16) {
                            f.set(ax, oy + ay, cell);
                        }
                    }
                }
            }
            None => {
                let scr = p.screen();
                // only the part on the client: a floating pane may be far bigger
                let span = |o: i32, n: u16, c: u16| (o.max(0) - o).min(n as i32) as u16..((c as i32 - o).clamp(0, n as i32)) as u16;
                for y in span(y0, h, clip.1) {
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
                    }
                }
            }
        }
        if matches!(p.mode, Some(PaneMode::Clock)) {
            let colour = format::parse_color(&self.opt_window(Some(wid), "clock-mode-colour")).unwrap_or(Color::Idx(4));
            let text = clock_text(self.opt_window(Some(wid), "clock-mode-style") != "12");
            let rr = Rect { y: oy + cr.y, h: cr.h.saturating_sub(1), ..cr };
            if cr.h < 6 || !big_text(f, rr, &text, colour) {
                f.text(cr.x, oy + cr.y + cr.h / 2, &text, Style { fg: colour, ..Style::default() }, cr.x + cr.w);
            }
        }
        if is_active {
            match &mode_lines {
                Some((_, Some((cx, cy)))) => {
                    if *cx < w && *cy < h {
                        *cursor = vis(*cx, *cy).map(|(x, y)| (x, oy + y));
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
                        if cx < w && cy < h {
                            *cursor = vis(cx, cy).map(|(x, y)| (x, oy + y));
                        }
                    }
                }
            }
        }
    }

    /// Draw the floating panes above the tiled ones, back to front: border lines (the pane's
    /// own pane-border-lines, -style and -status), the title on the border and the content.
    #[allow(clippy::too_many_arguments)]
    fn draw_floats(&self, f: &mut Frame, cid: ClientId, wid: WinId, clip: (u16, u16), oy: u16, modes: &mut Modes, cursor: &mut Option<(u16, u16)>) {
        let win = &self.windows[&wid];
        let floats = self.float_rects(wid);
        if floats.is_empty() {
            return;
        }
        // a floating pane above the active pane's cursor hides it
        if let Some((cx, cy)) = *cursor {
            let (wx, wy) = (cx as i32, cy as i32 - oy as i32);
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
                if x >= 0 && y >= 0 && x < clip.0 as i32 && y < clip.1 as i32 {
                    f.set(x as u16, oy + y as u16, Cell { text: text.into(), style: st });
                }
            };
            if lines != "none" {
                // number draws the pane's own index (tmux window_get_border_cell)
                let digit = border_number(&panes, base_index, Some(id));
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
                    let width = self.pane_status_width(wid, id, (g.x + 2, by), g.w.saturating_sub(2) as usize, clip);
                    let (cells, _) = if width > 0 { self.pane_status_line(id, width, st) } else { Default::default() };
                    for (i, c) in cells.into_iter().enumerate() {
                        let x = g.x + 2 + i as i32;
                        if let Some(c) = c.filter(|_| x >= 0 && by >= 0 && x < clip.0 as i32 && by < clip.1 as i32 && x < r) {
                            f.set(x as u16, oy + by as u16, c);
                        }
                    }
                }
            }
            self.draw_pane_body(f, cid, wid, id, (g.x, g.y, g.w, g.h), clip, oy, modes, cursor);
        }
        // ... and so does one above an active floating pane
        if let (Some((cx, cy)), true) = (*cursor, win.is_floating(win.active)) {
            let (wx, wy) = (cx as i32, cy as i32 - oy as i32);
            let above = floats.iter().skip_while(|(p, _)| *p != win.active).skip(1);
            if above.into_iter().any(|(p, _)| self.float_contains(wid, *p, wx, wy)) {
                *cursor = None;
            }
        }
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
        // top-floating and bottom-floating show status lines on floating panes only
        let border_status = match self.opt_window(Some(wid), "pane-border-status").as_str() {
            s @ ("top" | "bottom") => s.to_string(),
            _ => "off".to_string(),
        };
        let active_target = self.target_for_pane(win.active);
        let active_border = self.style_option(&self.opt_window(Some(wid), "pane-active-border-style"), Style::default(), active_target, Some(cid)).unwrap_or_default();
        let inactive_border = self.style_option(&self.opt_window(Some(wid), "pane-border-style"), Style::default(), active_target, Some(cid)).unwrap_or_default();
        let lines_kind = self.opt_window(Some(wid), "pane-border-lines");
        let bset = BORDER_SETS.iter().find(|(n, _)| *n == lines_kind).map(|(_, s)| *s).unwrap_or(BORDER_SETS[0].1);
        let indicators = self.opt_window(Some(wid), "pane-border-indicators");
        let all_panes = win.panes();
        let base_index = self.opt_window_num(Some(wid), "pane-base-index").max(0) as usize;
        let tiled_two = rects.len() == 2;

        // panes
        for (id, r) in &rects {
            let cr = self.content_rect(wid, *r);
            self.draw_pane_body(&mut f, cid, wid, *id, (cr.x as i32, cr.y as i32, cr.w, cr.h), (cw, area_h), oy, &mut modes, &mut cursor);
        }

        // borders with junctions
        let mut is_border = vec![false; cw as usize * area_h as usize];
        for sep in &seps {
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
                    (true, _, false, false) | (_, true, false, false) => bset[1],
                    _ => bset[0],
                };
                let mut near = active_rect.is_some_and(|a| x + 1 >= a.x && x <= a.right() && y + 1 >= a.y && y <= a.bottom()) && rects.len() > 1;
                if near && rects.len() == 2 {
                    if let Some(a) = active_rect {
                        if rects[0].1.y == rects[1].1.y {
                            near = if a.x == 0 { y <= a.h / 2 } else { y > a.h / 2 };
                        } else if border_status == "off" {
                            near = if a.y == 0 { x <= a.w / 2 } else { x > a.w / 2 };
                        }
                    }
                }
                let text = if lines_kind == "number" {
                    // tmux redraw_mark_two_pane_colours splits a two-pane window's border in half
                    let two = (tiled_two && matches!(indicators.as_str(), "colour" | "both")).then(|| (rects[0].1.y == rects[1].1.y, win.sx, win.sy));
                    border_number(&all_panes, base_index, border_owner(&rects, win.active, two, &border_status, x, y))
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
                let clip = (cw.min(win.sx), area_h.min(win.sy));
                let width = self.pane_status_width(wid, *id, (r.x as i32 + 2, by as i32), r.w.saturating_sub(2) as usize, clip);
                let (cells, _) = if width > 0 { self.pane_status_line(*id, width, st) } else { Default::default() };
                for (i, c) in cells.into_iter().enumerate() {
                    if let Some(c) = c.filter(|_| r.x as usize + 2 + i < cw as usize) {
                        f.set(r.x + 2 + i as u16, oy + by, c);
                    }
                }
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
        // floating panes above the tiled ones
        self.draw_floats(&mut f, cid, wid, (cw.min(win.sx), area_h.min(win.sy)), oy, &mut modes, &mut cursor);
        // area outside the window (client bigger than window)
        for y in 0..area_h {
            for x in 0..cw {
                if x >= win.sx || y >= win.sy {
                    f.set(x, oy + y, Cell { text: "·".into(), style: Style { attrs: DIM, ..Style::default() } });
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
                    let label = |f: &mut Frame, id: PaneId, r: Rect| {
                        let col = if id == win.active { acolour } else { colour };
                        let label = (all_panes.iter().position(|p| *p == id).unwrap_or(0) + base).to_string();
                        // tmux window_panes_draw_number: big digits only from 7 rows (the
                        // size line takes one) and len * 6 - 1 columns
                        let small = r.w < label.len() as u16 * 6 - 1 || r.h < 7;
                        if small || !big_text(f, Rect { y: oy + r.y, ..r }, &label, col) {
                            f.text(r.x + r.w.saturating_sub(label.len() as u16) / 2, oy + r.y + r.h / 2, &label, Style { fg: col, ..Style::default() }, r.right());
                        }
                        let size = format!("{}x{}", r.w, r.h);
                        f.text(r.right().saturating_sub(size.len() as u16), oy + r.y, &size, Style { fg: col, ..Style::default() }, r.right());
                    };
                    for (id, r) in &rects {
                        label(&mut f, *id, *r);
                    }
                    // tmux window_panes_draw_screen: the floating panes go over the tiled
                    // numbers, back to front, each with its own number clipped to the window
                    let clip = (cw.min(win.sx), area_h.min(win.sy));
                    let floats = self.float_rects(wid);
                    if !floats.is_empty() {
                        let mut ignored = None;
                        self.draw_floats(&mut f, cid, wid, clip, oy, &mut modes, &mut ignored);
                        for (id, g) in floats {
                            let (x0, y0) = (g.x.max(0), g.y.max(0));
                            let (x1, y1) = ((g.x + g.w as i32).min(clip.0 as i32), (g.y + g.h as i32).min(clip.1 as i32));
                            if x1 > x0 && y1 > y0 {
                                label(&mut f, id, Rect { x: x0 as u16, y: y0 as u16, w: (x1 - x0) as u16, h: (y1 - y0) as u16 });
                            }
                        }
                    }
                }
                Overlay::Menu(m) => {
                    let ost = |name: &str, base: Style| self.style_option(&self.opt_window(Some(wid), name), base, active_target, Some(cid)).unwrap_or(base);
                    let st = ost("popup-style", Style::default());
                    let bst = ost("popup-border-style", Style::default());
                    let st = ost("menu-style", st);
                    let bst = ost("menu-border-style", bst);
                    let selected = ost("menu-selected-style", Style { fg: Color::Idx(0), bg: Color::Idx(3), ..Style::default() });
                    m.draw(&mut f, st, bst, selected);
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
                        draw_box(&mut f, pp.x, pp.y, pp.w, pp.h, bst, &pp.title);
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
        let my = if nstatus > 0 { if top { 0 } else { ch - nstatus } } else { ch - 1 };
        if let Some(pr) = &c.prompt {
            let st = self.style_option(&self.opt_session(Some(sid), "message-style"), Style::default(), active_target, Some(cid)).unwrap_or_default();
            for x in 0..cw {
                f.set(x, my, Cell { text: " ".into(), style: st });
            }
            let label = pr.label.clone() + if matches!(pr.kind, PromptKind::Confirm) || pr.label == ":" || pr.label.ends_with(' ') || pr.label.is_empty() { "" } else { " " };
            let lx = f.text(0, my, &label, st, cw);
            let input: String = pr.input.iter().collect();
            let before: String = pr.input[..pr.cursor].iter().collect();
            let bw = format::width(&before) as u16;
            let room = cw.saturating_sub(lx + 1);
            let skip = bw.saturating_sub(room);
            let mut x = lx;
            let mut col = 0u16;
            for ch in input.chars() {
                let w = ch.width().unwrap_or(0) as u16;
                if col >= skip {
                    if x + w > cw {
                        break;
                    }
                    f.set(x, my, Cell { text: ch.to_string(), style: st });
                    if w == 2 {
                        f.set(x + 1, my, Cell { text: String::new(), style: st });
                    }
                    x += w;
                }
                col += w;
            }
            cursor = Some(((lx + bw - skip).min(cw - 1), my));
        } else if let Some((msg, _)) = &c.message {
            cursor = None;
            let st = self.style_option(&self.opt_session(Some(sid), "message-style"), Style::default(), active_target, Some(cid)).unwrap_or_default();
            for x in 0..cw {
                f.set(x, my, Cell { text: " ".into(), style: st });
            }
            f.text(0, my, msg, st, cw);
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
