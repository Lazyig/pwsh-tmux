//! Interactive modes: tree modes (choose-tree, choose-buffer, choose-client, customize-mode),
//! switch-mode, panes mode (display-panes), menus, popups, the clock and big digits.

use super::*;
use crate::cells::{Cell, Color, Style};
use unicode_width::UnicodeWidthChar;

mod customize;
mod customize_text;
mod fuzzy;
mod panes;
pub mod prompt;
mod switch;
mod tree;

pub use panes::{Panes, DEFAULT_TEMPLATE as PANES_DEFAULT_TEMPLATE};
pub use switch::Switch;
pub use tree::{Collapse, Preview, Sort, Tree, TreeKind};

/// The key a byte is when sent to a mode (send-keys -H and -l): control bytes as their
/// C- names, Enter, Tab, Escape and BSpace.
pub fn byte_key_name(b: u8) -> String {
    match b {
        b'\r' => "Enter".into(),
        b'\t' => "Tab".into(),
        0x1b => "Escape".into(),
        0x7f => "BSpace".into(),
        b' ' => "Space".into(),
        0 => "C-Space".into(),
        1..=26 => format!("C-{}", (b'a' + b - 1) as char),
        28..=31 => format!("C-{}", (b + 0x40) as char),
        _ => (b as char).to_string(),
    }
}

/// What a mode wants done after a key.
#[derive(Debug, PartialEq)]
pub enum ModeAction {
    None,
    /// Leave the mode.
    Exit,
    /// Run a command, then leave the mode.
    Run(String),
    /// Run a command and stay in the mode.
    RunKeep(String),
    /// Open `value` in the editor and run `command` (%% is the new text) when it closes,
    /// or replace the contents of paste buffer `buffer` with it.
    Edit { value: Vec<u8>, command: String, buffer: Option<String> },
    /// mode_tree_display_menu: a menu at (x, y) in the pane whose items are keys for the
    /// mode, as (name, key shown, key sent).
    Menu { title: String, items: Vec<(&'static str, &'static str, &'static str)>, x: u16, y: u16 },
}

/// Which of session_format, window_format and pane_format a format sees as set (the type
/// tmux's format_defaults gives a format tree).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FmtType {
    None,
    Session,
    Window,
    Pane,
}

/// Expand a format for an item of a mode.
pub fn expand_as(srv: &Server, t: vars::Target, ty: FmtType, fmt: &str, mut extra: HashMap<String, String>) -> String {
    if ty != FmtType::None {
        let b = |v: bool| if v { "1" } else { "0" }.to_string();
        extra.entry("session_format".into()).or_insert_with(|| b(ty == FmtType::Session));
        extra.entry("window_format".into()).or_insert_with(|| b(ty == FmtType::Window));
        extra.entry("pane_format".into()).or_insert_with(|| b(ty == FmtType::Pane));
    }
    srv.expand_extra(t, None, fmt, extra)
}

/// A style option value applied over `base`, expanded as a format for the target.
pub fn mode_style(srv: &Server, value: &str, base: Style, t: vars::Target) -> Style {
    let ctx = vars::VarCtx { srv, t, client: None, extra: HashMap::new() };
    crate::format::parse_style_with(value, base, Some(&ctx)).map(|p| p.style).unwrap_or(base)
}

/// Draw already expanded format text into `width` cells over `base` (format_draw).
pub fn format_cells(text: &str, width: usize, base: Style) -> Vec<Cell> {
    crate::format::layout_line(&crate::format::spans(text, base), width, base)
}

pub fn put_cells(row: &mut [Cell], x: usize, cells: &[Cell]) {
    for (i, c) in cells.iter().enumerate() {
        if let Some(d) = row.get_mut(x + i) {
            *d = c.clone();
        }
    }
}

/// Plain text at (x, y) up to column `limit`.
pub fn put_text(grid: &mut [Vec<Cell>], x: usize, y: usize, text: &str, style: Style, limit: usize) {
    let Some(row) = grid.get_mut(y) else { return };
    let mut x = x;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if w == 0 {
            continue;
        }
        if x + w > limit.min(row.len()) {
            break;
        }
        row[x] = Cell { text: ch.to_string(), style };
        if w == 2 {
            row[x + 1] = Cell { text: String::new(), style };
        }
        x += w;
    }
}

/// A single-line box (screen_write_box).
pub fn draw_box(grid: &mut [Vec<Cell>], x: usize, y: usize, w: usize, h: usize, style: Style) {
    if w < 2 || h < 2 {
        return;
    }
    let mut set = |xx: usize, yy: usize, s: &str| {
        if let Some(c) = grid.get_mut(yy).and_then(|r| r.get_mut(xx)) {
            *c = Cell { text: s.into(), style };
        }
    };
    for i in x + 1..x + w - 1 {
        set(i, y, "─");
        set(i, y + h - 1, "─");
    }
    for j in y + 1..y + h - 1 {
        set(x, j, "│");
        set(x + w - 1, j, "│");
    }
    set(x, y, "┌");
    set(x + w - 1, y, "┐");
    set(x, y + h - 1, "└");
    set(x + w - 1, y + h - 1, "┘");
}

/// screen_write_preview: a pane's screen in nx x ny at (x, y), around the cursor when it is on.
pub fn preview_into(srv: &Server, pane: PaneId, grid: &mut [Vec<Cell>], x: usize, y: usize, nx: usize, ny: usize) {
    let Some(p) = srv.panes.get(&pane) else { return };
    let scr = p.screen();
    let (sy, sx) = (p.size.1 as usize, p.size.0 as usize);
    let (cy, cx) = scr.cursor_position();
    let (cx, cy) = (cx as usize, cy as usize);
    let cursor = !scr.hide_cursor();
    let (px, py) = if cursor {
        let px = if cx < nx / 3 { 0 } else { cx - nx / 3 };
        let px = if px + nx > sx { if nx > sx { 0 } else { sx - nx } } else { px };
        let py = if cy < ny / 3 { 0 } else { cy - ny / 3 };
        let py = if py + ny > sy { if ny > sy { 0 } else { sy - ny } } else { py };
        (px, py)
    } else {
        (0, 0)
    };
    for yy in 0..ny {
        for xx in 0..nx {
            let Some(vc) = scr.cell((py + yy) as u16, (px + xx) as u16) else { continue };
            let mut cell = crate::cells::cell_from_vt(vc);
            if cell.text.is_empty() && xx == 0 {
                cell.text = " ".into();
            }
            if cursor && (px + xx, py + yy) == (cx, cy) {
                cell.style.attrs |= crate::cells::INVERSE;
            }
            if let Some(c) = grid.get_mut(y + yy).and_then(|r| r.get_mut(x + xx)) {
                *c = cell;
            }
        }
    }
}

// ---- menu ------------------------------------------------------------------------

/// Width of a menu item name drawn as a format (format_width).
fn styled_width(name: &str) -> usize {
    crate::format::spans(name, Style::default()).iter().map(|s| crate::format::width(&s.text)).sum()
}

pub struct MenuItem {
    pub name: String,
    pub key: String,
    pub cmd: String,
}

pub struct Menu {
    pub title: String,
    pub items: Vec<MenuItem>,
    pub sel: Option<usize>,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub pane: Option<PaneId>,
    /// Opened from the keyboard: the mouse does not choose items (MENU_NOMOUSE).
    pub nomouse: bool,
    /// -O: releasing the button does not close the menu (MENU_STAYOPEN).
    pub stayopen: bool,
    pub look: OverlayLook,
}

/// Styles and border lines given to display-menu (-s -H -S -b) or display-popup
/// (-s -S -b); unset ones come from the menu-* / popup-* options.
#[derive(Default, Clone)]
pub struct OverlayLook {
    pub style: Option<String>,
    pub selected_style: Option<String>,
    pub border_style: Option<String>,
    pub lines: Option<String>,
}

/// The values of menu-border-lines and popup-border-lines.
pub const BOX_LINES: [&str; 7] = ["single", "rounded", "double", "heavy", "simple", "padded", "none"];

pub enum MenuAction {
    None,
    Cancel,
    Run(String),
}

impl Menu {
    /// Width and height of the box for these items (tmux menu->width + 4, count + 2).
    pub fn box_size(title: &str, items: &[MenuItem]) -> (u16, u16) {
        let inner = items
            .iter()
            .map(|i| styled_width(&i.name) + if i.key.is_empty() { 0 } else { crate::format::width(&i.key) + 3 })
            .chain(std::iter::once(crate::format::spans(title, Style::default()).iter().map(|s| crate::format::width(&s.text)).sum()))
            .max()
            .unwrap_or(0) as u16;
        (inner + 4, items.len() as u16 + 2)
    }

    /// A menu at a position from [`Server::overlay_position`]; `start` is the first
    /// selected item for keyboard menus (tmux -C), None for no selection.
    #[allow(clippy::too_many_arguments)]
    pub fn at(title: String, items: Vec<MenuItem>, x: u16, y: u16, pane: Option<PaneId>, nomouse: bool, stayopen: bool, start: Option<usize>) -> Menu {
        let (w, h) = Menu::box_size(&title, &items);
        let enabled = |i: usize| items.get(i).is_some_and(|it| !it.cmd.is_empty());
        let sel = if !nomouse {
            None
        } else {
            start.and_then(|s| {
                let n = items.len();
                if n == 0 {
                    return None;
                }
                if s >= n {
                    (0..n).rev().find(|&i| enabled(i))
                } else {
                    (0..n).map(|k| (s + k) % n).find(|&i| enabled(i))
                }
            })
        };
        Menu { title, items, sel, x, y, w, h, pane, nomouse, stayopen, look: OverlayLook::default() }
    }

    /// Run the selected item (tmux menu_key_cb "chosen").
    pub fn choose(&self) -> MenuAction {
        match self.sel.and_then(|i| self.items.get(i)) {
            None => MenuAction::Cancel,
            Some(it) if it.cmd.is_empty() => {
                if self.stayopen { MenuAction::None } else { MenuAction::Cancel }
            }
            Some(it) => MenuAction::Run(it.cmd.clone()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    pub fn new(title: String, items: Vec<MenuItem>, x: Option<&str>, y: Option<&str>, cw: u16, ch: u16, mouse: Option<(u16, u16)>, pane: Option<PaneId>) -> Menu {
        let inner = items
            .iter()
            .map(|i| styled_width(&i.name) + if i.key.is_empty() { 0 } else { crate::format::width(&i.key) + 3 })
            .chain(std::iter::once(crate::format::spans(&title, Style::default()).iter().map(|s| crate::format::width(&s.text)).sum()))
            .max()
            .unwrap_or(10) as u16;
        let w = (inner + 4).min(cw);
        let h = (items.len() as u16 + 2).min(ch);
        let pos = |v: Option<&str>, total: u16, size: u16, mouse: Option<u16>| -> u16 {
            let p = match v {
                Some("C") => ((total.saturating_sub(1)) / 2).saturating_sub(size / 2),
                Some("R") => total.saturating_sub(size),
                Some("M") => mouse.unwrap_or(0),
                Some("P") | Some("W") | None => mouse.map(|m| m.saturating_sub(1)).unwrap_or(total.saturating_sub(size) / 2),
                Some("S") => total.saturating_sub(size + 1),
                Some(n) => n.parse().unwrap_or(0),
            };
            p.min(total.saturating_sub(size))
        };
        let mx = pos(x, cw, w, mouse.map(|m| m.0));
        let my = if y == Some("C") { ((ch.saturating_sub(1)) / 2).saturating_sub(h.div_ceil(2)) } else { pos(y, ch, h, mouse.map(|m| m.1)) };
        let sel = items.iter().position(|i| !i.cmd.is_empty());
        Menu { title, items, sel, x: mx, y: my, w, h, pane, nomouse: true, stayopen: false, look: OverlayLook::default() }
    }

    fn step(&mut self, dir: isize) {
        let n = self.items.len() as isize;
        if n == 0 {
            return;
        }
        let mut i = self.sel.map(|s| s as isize).unwrap_or(-1);
        for _ in 0..n {
            i = (i + dir).rem_euclid(n);
            if !self.items[i as usize].cmd.is_empty() {
                self.sel = Some(i as usize);
                return;
            }
        }
    }

    pub fn key(&mut self, key: &str) -> MenuAction {
        match key {
            "Escape" | "q" | "C-c" | "C-g" => MenuAction::Cancel,
            "Up" | "k" | "BTab" => {
                self.step(-1);
                MenuAction::None
            }
            "Down" | "j" | "Tab" => {
                self.step(1);
                MenuAction::None
            }
            "Enter" | "Space" => self.choose(),
            k => match self.items.iter().find(|i| !i.cmd.is_empty() && i.key == k) {
                Some(i) => MenuAction::Run(i.cmd.clone()),
                None => MenuAction::None,
            },
        }
    }

    /// Draw the menu with the window `oy` rows down the client.
    pub fn draw(&self, f: &mut super::render::Frame, oy: u16, style: Style, border: Style, selected_style: Style, lines_kind: &str) {
        let (x0, y0, w, h) = (self.x, self.y + oy, self.w, self.h);
        let lines = super::render::box_lines(lines_kind);
        super::render::draw_box_lines(f, x0, y0, w, h, border, &self.title, lines_kind);
        for (i, it) in self.items.iter().enumerate() {
            let y = y0 + 1 + i as u16;
            if y + 1 >= y0 + h {
                break;
            }
            let selected = self.sel == Some(i);
            let st = if selected { selected_style } else { style };
            for x in x0 + 1..x0 + w - 1 {
                f.set(x, y, Cell { text: " ".into(), style: st });
            }
            if it.name.is_empty() {
                for x in x0 + 1..x0 + w - 1 {
                    f.set(x, y, Cell { text: lines[0].into(), style: border });
                }
                f.set(x0, y, Cell { text: lines[7].into(), style: border });
                f.set(x0 + w - 1, y, Cell { text: lines[8].into(), style: border });
                continue;
            }
            // names are formats (menu_add_item: "Paste #[underscore]..."), drawn
            // like format_draw over the item's style
            let dim = Style { attrs: style.attrs | crate::cells::DIM, ..st };
            let base = if it.cmd.is_empty() { dim } else { st };
            let cells = crate::format::layout_line(&crate::format::spans(&it.name, base), w.saturating_sub(4) as usize, st);
            f.put_cells(x0 + 2, y, &cells);
            if !it.key.is_empty() {
                let k = format!("({})", it.key);
                let kw = crate::format::width(&k) as u16;
                f.text((x0 + w - 2).saturating_sub(kw), y, &k, st, x0 + w - 1);
            }
        }
    }
}

// ---- popup -------------------------------------------------------------------------

pub struct Popup {
    pub pane: PaneId,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub border: bool,
    pub close_on_exit: bool,
    /// -EE (POPUP_CLOSEEXITZERO): it closes when the command exits with status 0.
    pub close_exit_zero: bool,
    /// -k: after the command exits any key closes the popup.
    pub close_any_key: bool,
    pub title: String,
    pub look: OverlayLook,
}

/// The editor a mode runs in a modal floating pane (spawn_editor_state).
pub struct PopupEditor {
    pub path: std::path::PathBuf,
    /// Run with %% replaced by the edited text.
    pub command: String,
    /// The paste buffer being edited (window_buffer_edit_close_cb) instead of a command.
    pub buffer: Option<String>,
}

impl Popup {
    pub fn inner(&self) -> (u16, u16, u16, u16) {
        if self.border {
            (self.x + 1, self.y + 1, self.w.saturating_sub(2), self.h.saturating_sub(2))
        } else {
            (self.x, self.y, self.w, self.h)
        }
    }
}

// ---- big digits (clock-mode, display-panes) ----------------------------------------

pub(super) const FONT: [(char, [&str; 5]); 14] = [
    ('0', ["#####", "#   #", "#   #", "#   #", "#####"]),
    ('1', ["    #", "    #", "    #", "    #", "    #"]),
    ('2', ["#####", "    #", "#####", "#    ", "#####"]),
    ('3', ["#####", "    #", "#####", "    #", "#####"]),
    ('4', ["#   #", "#   #", "#####", "    #", "    #"]),
    ('5', ["#####", "#    ", "#####", "    #", "#####"]),
    ('6', ["#####", "#    ", "#####", "#   #", "#####"]),
    ('7', ["#####", "    #", "    #", "    #", "    #"]),
    ('8', ["#####", "#   #", "#####", "#   #", "#####"]),
    ('9', ["#####", "#   #", "#####", "    #", "#####"]),
    (':', ["     ", "  #  ", "     ", "  #  ", "     "]),
    ('A', ["#####", "#   #", "#####", "#   #", "#   #"]),
    ('P', ["#####", "#   #", "#####", "#    ", "#    "]),
    ('M', ["#   #", "## ##", "# # #", "#   #", "#   #"]),
];

/// The clock-mode text for a clock-mode-style (window_clock_draw_screen): "12" and
/// "12-with-seconds" are strftime "%l:%M " or "%l:%M:%S " plus AM or PM, the others "%H:%M"
/// or "%H:%M:%S".
pub fn clock_text(style: &str, now: chrono::NaiveTime) -> String {
    use chrono::Timelike;
    let seconds = style.ends_with("-with-seconds");
    if style.starts_with("12") {
        let (pm, h) = now.hour12();
        let mut t = format!("{:2}:{:02}", h, now.minute());
        if seconds {
            t += &format!(":{:02}", now.second());
        }
        t + if pm { " PM" } else { " AM" }
    } else if seconds {
        format!("{:02}:{:02}:{:02}", now.hour(), now.minute(), now.second())
    } else {
        format!("{:02}:{:02}", now.hour(), now.minute())
    }
}

/// window_clock_draw_screen: big 5x5 digits from (sx / 2 - 3 * len, sy / 2 - 3) drawn as
/// '#' in the colour on the colour (spaces only advance), or the plain text in the middle
/// when the pane is too small, or nothing when it is narrower than the text.
pub fn draw_clock(f: &mut super::render::Frame, r: Rect, text: &str, colour: Color, bg: Color) {
    let len = text.chars().count() as u16;
    if r.w < 6 * len || r.h < 6 {
        if r.w >= len && r.h != 0 {
            f.text(r.x + r.w / 2 - len / 2, r.y + r.h / 2, text, Style { fg: colour, bg, ..Style::default() }, r.x + r.w);
        }
        return;
    }
    let (mut x, y) = (r.x + r.w / 2 - 3 * len, r.y + r.h / 2 - 3);
    let block = Style { fg: colour, bg: colour, ..Style::default() };
    for ch in text.chars() {
        if let Some((_, rows)) = FONT.iter().find(|(c, _)| *c == ch) {
            for (dy, row) in rows.iter().enumerate() {
                for (dx, b) in row.chars().enumerate() {
                    if b == '#' {
                        f.set(x + dx as u16, y + dy as u16, Cell { text: "#".into(), style: block });
                    }
                }
            }
        }
        x += 6;
    }
}

#[cfg(test)]
mod menu_format_tests {
    use super::*;
    #[test]
    fn menu_names_are_drawn_as_formats() {
        let items = vec![MenuItem { name: "Paste #[underscore]abc".into(), key: "p".into(), cmd: "paste-buffer".into() }];
        let (w, h) = Menu::box_size("", &items);
        assert_eq!((w, h), (9 + 4 + 4, 3));
        let m = Menu::at(String::new(), items, 0, 0, None, true, false, None);
        let mut f = super::super::render::Frame::new(30, 4);
        m.draw(&mut f, 0, Style::default(), Style::default(), Style::default(), "single");
        let row: String = (0..w).map(|x| f.cells[(30 + x) as usize].text.clone()).collect();
        assert_eq!(row, "│ Paste abc (p) │");
        assert_ne!(f.cells[30 + 8].style.attrs & crate::cells::UNDERLINE, 0);
    }
}

#[cfg(test)]
mod clock_tests {
    use super::clock_text;
    use chrono::NaiveTime;

    #[test]
    fn clock_mode_styles() {
        let t = NaiveTime::from_hms_opt(9, 5, 7).unwrap();
        assert_eq!(clock_text("24", t), "09:05");
        assert_eq!(clock_text("24-with-seconds", t), "09:05:07");
        assert_eq!(clock_text("12", t), " 9:05 AM");
        assert_eq!(clock_text("12-with-seconds", t), " 9:05:07 AM");
        let t = NaiveTime::from_hms_opt(0, 30, 0).unwrap();
        assert_eq!(clock_text("12", t), "12:30 AM");
        let t = NaiveTime::from_hms_opt(23, 59, 59).unwrap();
        assert_eq!(clock_text("12-with-seconds", t), "11:59:59 PM");
    }

    #[test]
    fn clock_blocks_show_without_colours() {
        // window_clock_draw: '#' in the colour on the colour
        let mut f = super::super::render::Frame::new(80, 10);
        super::draw_clock(&mut f, super::Rect { x: 0, y: 0, w: 80, h: 10 }, "12:34", super::Color::Default, super::Color::Default);
        assert!(f.cells.iter().any(|c| c.text == "#"));
    }
}
