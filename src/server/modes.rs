//! Interactive modes: tree modes (choose-tree, choose-buffer, choose-client, customize-mode),
//! switch-mode, panes mode (display-panes), menus, popups, the clock and big digits.

use super::*;
use crate::cells::{Cell, Color, Style};
use unicode_width::UnicodeWidthChar;

mod customize;
mod fuzzy;
mod panes;
pub mod prompt;
mod switch;
mod tree;

pub use panes::{Panes, DEFAULT_TEMPLATE as PANES_DEFAULT_TEMPLATE};
pub use switch::Switch;
pub use tree::{Collapse, Preview, Sort, Tree, TreeKind};

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
    /// Open `value` in the editor and run `command` (%% is the new text) when it closes.
    Edit { value: String, command: String },
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
}

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
            .map(|i| crate::format::width(&i.name) + if i.key.is_empty() { 0 } else { crate::format::width(&i.key) + 3 })
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
        Menu { title, items, sel, x, y, w, h, pane, nomouse, stayopen }
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
            .map(|i| crate::format::width(&i.name) + if i.key.is_empty() { 0 } else { crate::format::width(&i.key) + 3 })
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
        Menu { title, items, sel, x: mx, y: my, w, h, pane, nomouse: true, stayopen: false }
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

    pub fn draw(&self, f: &mut super::render::Frame, style: Style, border: Style, selected_style: Style) {
        let (x0, y0, w, h) = (self.x, self.y, self.w, self.h);
        super::render::draw_box(f, x0, y0, w, h, border, &self.title);
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
                    f.set(x, y, Cell { text: "─".into(), style: border });
                }
                f.set(x0, y, Cell { text: "├".into(), style: border });
                f.set(x0 + w - 1, y, Cell { text: "┤".into(), style: border });
                continue;
            }
            let dim = Style { attrs: style.attrs | crate::cells::DIM, ..st };
            f.text(x0 + 2, y, &it.name, if it.cmd.is_empty() { dim } else { st }, x0 + w - 1);
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
    pub title: String,
    /// The popup runs the editor for a mode (spawn_editor).
    pub editor: Option<Box<PopupEditor>>,
}

pub struct PopupEditor {
    pub path: std::path::PathBuf,
    /// Run with %% replaced by the edited text.
    pub command: String,
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

/// Draws `text` in 5x5 block digits centred in the rect; returns false if it doesn't fit.
pub fn big_text(f: &mut super::render::Frame, r: Rect, text: &str, colour: Color) -> bool {
    let n = text.chars().count() as u16;
    let width = n * 6;
    if r.w < width || r.h < 5 {
        return false;
    }
    let x0 = r.x + (r.w - width) / 2;
    let y0 = r.y + (r.h - 5) / 2;
    let block = Style { bg: colour, ..Style::default() };
    for (i, ch) in text.chars().enumerate() {
        let Some((_, rows)) = FONT.iter().find(|(c, _)| *c == ch) else { continue };
        for (dy, row) in rows.iter().enumerate() {
            for (dx, b) in row.chars().enumerate() {
                if b == '#' {
                    f.set(x0 + i as u16 * 6 + dx as u16, y0 + dy as u16, Cell { text: " ".into(), style: block });
                }
            }
        }
    }
    true
}

pub fn clock_text(style24: bool) -> String {
    use chrono::Timelike;
    let now = chrono::Local::now();
    if style24 {
        format!("{:02}:{:02}", now.hour(), now.minute())
    } else {
        let (pm, h) = now.hour12();
        format!("{:2}:{:02} {}", h, now.minute(), if pm { "PM" } else { "AM" }).trim_start().replace(' ', "")
    }
}
