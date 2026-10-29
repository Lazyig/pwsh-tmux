//! Interactive modes: choose-tree / choose-buffer / choose-client / customize (tree mode),
//! menus, popups, the clock and big digits for display-panes.

use super::*;
use crate::cells::{Cell, Color, Line, Style, BOLD, INVERSE};
use std::collections::HashSet;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TreeKind {
    Sessions,
    Windows,
    Buffers,
    Clients,
    Options,
}

#[derive(Clone, Debug)]
pub struct TreeItem {
    pub id: String,
    pub depth: u8,
    pub label: String,
    pub target: String,
    pub kill: String,
    pub expandable: bool,
    pub preview: Option<PaneId>,
}

pub struct Tree {
    pub kind: TreeKind,
    pub template: String,
    pub items: Vec<TreeItem>,
    pub expanded: HashSet<String>,
    pub sel: usize,
    pub top: usize,
    pub filter: Option<Vec<WinId>>,
    pub client: Option<ClientId>,
    pub tagged: HashSet<String>,
}

pub enum TreeAction {
    None,
    Cancel,
    Run(String),
}

fn line_from(text: &str, width: usize, style: Style) -> Line {
    let mut cells = Vec::with_capacity(width);
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if w == 0 {
            continue;
        }
        if cells.len() + w > width {
            break;
        }
        cells.push(Cell { text: ch.to_string(), style });
        if w == 2 {
            cells.push(Cell { text: String::new(), style });
        }
    }
    while cells.len() < width {
        cells.push(Cell { text: " ".into(), style });
    }
    Line { cells, wrapped: false }
}

impl Tree {
    pub fn new(kind: TreeKind, template: Option<String>, filter: Option<Vec<WinId>>, srv: &Server, pane: PaneId) -> Tree {
        let template = template.unwrap_or_else(|| {
            match kind {
                TreeKind::Sessions | TreeKind::Windows => "switch-client -t '%%'",
                TreeKind::Buffers => "paste-buffer -p -b '%%'",
                TreeKind::Clients => "detach-client -t '%%'",
                TreeKind::Options => "",
            }
            .to_string()
        });
        let mut expanded = HashSet::new();
        if kind == TreeKind::Windows {
            for s in srv.sessions.values() {
                expanded.insert(format!("s{}", s.id));
            }
        }
        let mut t = Tree { kind, template, items: Vec::new(), expanded, sel: 0, top: 0, filter, client: None, tagged: HashSet::new() };
        t.rebuild(srv);
        // select the current session / window
        let cur = srv.target_for_pane(pane);
        let want = match (kind, cur.sid, cur.wid) {
            (TreeKind::Windows, Some(s), Some(w)) => format!("s{s}w{w}"),
            (TreeKind::Sessions, Some(s), _) => format!("s{s}"),
            _ => String::new(),
        };
        if let Some(i) = t.items.iter().position(|it| it.id == want) {
            t.sel = i;
        }
        t
    }

    pub fn rebuild(&mut self, srv: &Server) {
        let mut items = Vec::new();
        match self.kind {
            TreeKind::Sessions | TreeKind::Windows => {
                let mut sessions: Vec<&Session> = srv.sessions.values().collect();
                sessions.sort_by(|a, b| a.name.cmp(&b.name));
                for s in sessions {
                    let wins: Vec<(u32, WinId)> = s.links.iter().filter(|(_, w)| self.filter.as_ref().is_none_or(|f| f.contains(w))).map(|(i, w)| (*i, *w)).collect();
                    if wins.is_empty() {
                        continue;
                    }
                    let attached = srv.clients.values().any(|c| c.attached && c.session == Some(s.id));
                    let sid = format!("s{}", s.id);
                    items.push(TreeItem {
                        id: sid.clone(),
                        depth: 0,
                        label: format!("{}: {} windows{}", s.name, s.links.len(), if attached { " (attached)" } else { "" }),
                        target: format!("${}", s.id),
                        kill: format!("kill-session -t '${}'", s.id),
                        expandable: true,
                        preview: Some(srv.windows[&s.active_window()].active),
                    });
                    if !self.expanded.contains(&sid) {
                        continue;
                    }
                    for (idx, wid) in wins {
                        let w = &srv.windows[&wid];
                        let wkey = format!("s{}w{}", s.id, wid);
                        let flags = srv.window_flags(s.id, idx);
                        let np = w.layout.panes().len();
                        items.push(TreeItem {
                            id: wkey.clone(),
                            depth: 1,
                            label: format!("{idx}: {}{flags} ({np} panes) \"{}\"", w.name, srv.panes.get(&w.active).map(|p| p.title.as_str()).unwrap_or("")),
                            target: format!("${}:{idx}", s.id),
                            kill: format!("kill-window -t '@{wid}'"),
                            expandable: np > 1,
                            preview: Some(w.active),
                        });
                        if np > 1 && self.expanded.contains(&wkey) {
                            for (pi, p) in w.layout.panes().iter().enumerate() {
                                let pp = srv.panes.get(p);
                                items.push(TreeItem {
                                    id: format!("p{p}"),
                                    depth: 2,
                                    label: format!("{pi}: {}{} \"{}\"", pp.map(|x| x.current_cmd.as_str()).unwrap_or(""), if *p == w.active { "*" } else { "" }, pp.map(|x| x.title.as_str()).unwrap_or("")),
                                    target: format!("%{p}"),
                                    kill: format!("kill-pane -t '%{p}'"),
                                    expandable: false,
                                    preview: Some(*p),
                                });
                            }
                        }
                    }
                }
            }
            TreeKind::Buffers => {
                for b in &srv.buffers {
                    let sample: String = String::from_utf8_lossy(&b.data).chars().take(60).map(|c| if c.is_control() { ' ' } else { c }).collect();
                    items.push(TreeItem {
                        id: format!("b{}", b.name),
                        depth: 0,
                        label: format!("{}: {} bytes: \"{}\"", b.name, b.data.len(), sample),
                        target: b.name.clone(),
                        kill: format!("delete-buffer -b '{}'", b.name),
                        expandable: false,
                        preview: None,
                    });
                }
            }
            TreeKind::Clients => {
                for c in srv.clients.values().filter(|c| c.attached) {
                    let sname = c.session.and_then(|s| srv.sessions.get(&s)).map(|s| s.name.clone()).unwrap_or_default();
                    items.push(TreeItem {
                        id: format!("c{}", c.id),
                        depth: 0,
                        label: format!("{}: session {} [{}x{}]", c.name(), sname, c.size.0, c.size.1),
                        target: c.name(),
                        kill: format!("detach-client -t '{}'", c.name()),
                        expandable: false,
                        preview: c.session.and_then(|s| srv.sessions.get(&s)).map(|s| srv.windows[&s.active_window()].active),
                    });
                }
            }
            TreeKind::Options => {
                for (scope, store) in [("-s", &srv.opts_server), ("-g", &srv.opts_session), ("-gw", &srv.opts_window)] {
                    for (k, v) in &store.0 {
                        items.push(TreeItem {
                            id: format!("o{scope}{k}"),
                            depth: 0,
                            label: format!("{k} {v}"),
                            target: format!("command-prompt -I {} -p '{k}' \"set-option {scope} {k} '%%'\"", crate::cmdparse::quote(v)),
                            kill: format!("set-option -u {scope} {k}"),
                            expandable: false,
                            preview: None,
                        });
                    }
                }
            }
        }
        self.items = items;
        if self.sel >= self.items.len() {
            self.sel = self.items.len().saturating_sub(1);
        }
    }

    fn command_for(&self, item: &TreeItem) -> String {
        if self.kind == TreeKind::Options {
            return item.target.clone();
        }
        let t = self.template.replace("%%", &item.target).replace("%1", &item.target);
        if t.contains('%') || self.template.contains("%%") || self.template.contains("%1") {
            t
        } else {
            format!("{} -t '{}'", self.template, item.target)
        }
    }

    /// Handles one key; `height` is the list height (for paging).
    pub fn key(&mut self, key: &str, height: usize, srv: &Server) -> TreeAction {
        let n = self.items.len();
        if n == 0 {
            return if matches!(key, "q" | "Escape" | "C-c" | "C-g") { TreeAction::Cancel } else { TreeAction::None };
        }
        let page = height.max(1);
        match key {
            "q" | "Escape" | "C-c" | "C-g" => return TreeAction::Cancel,
            "Up" | "k" | "C-p" => self.sel = (self.sel + n - 1) % n,
            "Down" | "j" | "C-n" => self.sel = (self.sel + 1) % n,
            "PPage" | "C-b" => self.sel = self.sel.saturating_sub(page),
            "NPage" | "C-f" => self.sel = (self.sel + page).min(n - 1),
            "Home" | "g" => self.sel = 0,
            "End" | "G" => self.sel = n - 1,
            "Left" | "h" | "-" => {
                let it = &self.items[self.sel];
                if self.expanded.remove(&it.id) {
                    self.rebuild(srv);
                } else if it.depth > 0 {
                    // go to parent
                    let d = it.depth;
                    if let Some(p) = (0..self.sel).rev().find(|i| self.items[*i].depth < d) {
                        self.sel = p;
                    }
                }
            }
            "Right" | "l" | "+" => {
                let it = &self.items[self.sel];
                if it.expandable && self.expanded.insert(it.id.clone()) {
                    self.rebuild(srv);
                }
            }
            "M-+" => {
                for it in self.items.clone() {
                    if it.expandable {
                        self.expanded.insert(it.id.clone());
                    }
                }
                self.rebuild(srv);
            }
            "M--" => {
                self.expanded.clear();
                self.rebuild(srv);
            }
            "t" => {
                let id = self.items[self.sel].id.clone();
                if !self.tagged.remove(&id) {
                    self.tagged.insert(id);
                }
                self.sel = (self.sel + 1).min(n - 1);
            }
            "T" => self.tagged.clear(),
            "C-t" => {
                self.tagged = self.items.iter().map(|i| i.id.clone()).collect();
            }
            "x" => {
                let cmd = self.items[self.sel].kill.clone();
                return TreeAction::Run(cmd);
            }
            "X" => {
                let cmds: Vec<String> = self.items.iter().filter(|i| self.tagged.contains(&i.id)).map(|i| i.kill.clone()).collect();
                self.tagged.clear();
                if cmds.is_empty() {
                    return TreeAction::None;
                }
                return TreeAction::Run(cmds.join(" ; "));
            }
            "Enter" => {
                if !self.tagged.is_empty() && self.kind != TreeKind::Options {
                    let cmds: Vec<String> = self.items.iter().filter(|i| self.tagged.contains(&i.id)).map(|i| self.command_for(i)).collect();
                    return TreeAction::Run(cmds.join(" ; "));
                }
                let it = self.items[self.sel].clone();
                return TreeAction::Run(self.command_for(&it));
            }
            k => {
                // quick keys 0-9 then M-a..M-z
                let idx = match k.chars().collect::<Vec<_>>().as_slice() {
                    [c] if c.is_ascii_digit() => Some(*c as usize - '0' as usize),
                    ['M', '-', c] if c.is_ascii_lowercase() => Some(10 + (*c as usize - 'a' as usize)),
                    _ => None,
                };
                if let Some(i) = idx.filter(|i| *i < n) {
                    let it = self.items[i].clone();
                    return TreeAction::Run(self.command_for(&it));
                }
            }
        }
        if self.sel < self.top {
            self.top = self.sel;
        }
        if self.sel >= self.top + page {
            self.top = self.sel + 1 - page;
        }
        TreeAction::None
    }

    pub fn render(&self, srv: &Server, w: u16, h: u16) -> Vec<Line> {
        let (w, h) = (w as usize, h as usize);
        let preview = h >= 12 && self.items.get(self.sel).and_then(|i| i.preview).is_some();
        let list_h = if preview { h / 2 } else { h };
        let mut top = self.top;
        if self.sel < top {
            top = self.sel;
        }
        if self.sel >= top + list_h {
            top = self.sel + 1 - list_h;
        }
        let mut out = Vec::with_capacity(h);
        let sel_style = Style { attrs: INVERSE, ..Style::default() };
        let tag_style = Style { attrs: BOLD, ..Style::default() };
        for row in 0..list_h {
            let i = top + row;
            let Some(it) = self.items.get(i) else {
                out.push(line_from("", w, Style::default()));
                continue;
            };
            let quick = if i < 10 { format!("({i}) ") } else if i < 36 { format!("(M-{}) ", (b'a' + (i - 10) as u8) as char) } else { "     ".into() };
            let arrow = if it.expandable { if self.expanded.contains(&it.id) { "- " } else { "+ " } } else { "  " };
            let indent = "  ".repeat(it.depth as usize);
            let tag = if self.tagged.contains(&it.id) { "*" } else { "" };
            let text = format!("{quick}{indent}{arrow}{}{tag}", it.label);
            let style = if i == self.sel { sel_style } else if self.tagged.contains(&it.id) { tag_style } else { Style::default() };
            out.push(line_from(&text, w, style));
        }
        if preview {
            let pane = self.items[self.sel].preview.unwrap();
            let border = Style { fg: Color::Idx(2), ..Style::default() };
            let title = format!(" {} ", self.items[self.sel].target);
            let mut bar = String::from("─");
            bar.push_str(&title);
            while bar.chars().count() < w {
                bar.push('─');
            }
            out.push(line_from(&bar, w, border));
            let ph = h - list_h - 1;
            if let Some(p) = srv.panes.get(&pane) {
                let scr = p.screen();
                for r in 0..ph {
                    let mut cells: Vec<Cell> = (0..w as u16).map(|c| scr.cell(r as u16, c).map(crate::cells::cell_from_vt).unwrap_or_default()).collect();
                    if let Some(last) = cells.last_mut() {
                        if last.text.chars().next().and_then(|c| c.width()) == Some(2) {
                            *last = Cell::default();
                        }
                    }
                    out.push(Line { cells, wrapped: false });
                }
            }
        }
        while out.len() < h {
            out.push(line_from("", w, Style::default()));
        }
        out
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
}

pub enum MenuAction {
    None,
    Cancel,
    Run(String),
}

impl Menu {
    #[allow(clippy::too_many_arguments)]
    pub fn new(title: String, items: Vec<MenuItem>, x: Option<&str>, y: Option<&str>, cw: u16, ch: u16, mouse: Option<(u16, u16)>, pane: Option<PaneId>) -> Menu {
        let inner = items
            .iter()
            .map(|i| crate::format::width(&i.name) + if i.key.is_empty() { 0 } else { crate::format::width(&i.key) + 3 })
            .chain(std::iter::once(crate::format::width(&title) + 2))
            .max()
            .unwrap_or(10) as u16;
        let w = (inner + 4).min(cw);
        let h = (items.len() as u16 + 2).min(ch);
        let pos = |v: Option<&str>, total: u16, size: u16, mouse: Option<u16>| -> u16 {
            let p = match v {
                Some("C") => total.saturating_sub(size) / 2,
                Some("R") => total.saturating_sub(size),
                Some("M") => mouse.unwrap_or(0),
                Some("P") | Some("W") | None => mouse.map(|m| m.saturating_sub(1)).unwrap_or(total.saturating_sub(size) / 2),
                Some("S") => total.saturating_sub(size + 1),
                Some(n) => n.parse().unwrap_or(0),
            };
            p.min(total.saturating_sub(size))
        };
        let mx = pos(x, cw, w, mouse.map(|m| m.0));
        let my = pos(y, ch, h, mouse.map(|m| m.1));
        let sel = items.iter().position(|i| !i.cmd.is_empty());
        Menu { title, items, sel, x: mx, y: my, w, h, pane }
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
            "Enter" | "Space" => match self.sel {
                Some(i) => MenuAction::Run(self.items[i].cmd.clone()),
                None => MenuAction::Cancel,
            },
            k => match self.items.iter().find(|i| !i.cmd.is_empty() && i.key == k) {
                Some(i) => MenuAction::Run(i.cmd.clone()),
                None => MenuAction::None,
            },
        }
    }

    /// Mouse click in client coordinates.
    pub fn click(&mut self, x: u16, y: u16, release: bool) -> MenuAction {
        if x < self.x || x >= self.x + self.w || y < self.y || y >= self.y + self.h {
            return if release { MenuAction::None } else { MenuAction::Cancel };
        }
        let row = y.saturating_sub(self.y + 1) as usize;
        if y > self.y && row < self.items.len() && !self.items[row].cmd.is_empty() {
            self.sel = Some(row);
            if release {
                return MenuAction::Run(self.items[row].cmd.clone());
            }
        }
        MenuAction::None
    }

    pub fn draw(&self, f: &mut super::render::Frame, style: Style, border: Style) {
        let (x0, y0, w, h) = (self.x, self.y, self.w, self.h);
        super::render::draw_box(f, x0, y0, w, h, border, &self.title);
        for (i, it) in self.items.iter().enumerate() {
            let y = y0 + 1 + i as u16;
            if y + 1 >= y0 + h {
                break;
            }
            let selected = self.sel == Some(i);
            let st = if selected { Style { attrs: style.attrs ^ INVERSE, ..style } } else { style };
            for x in x0 + 1..x0 + w - 1 {
                f.set(x, y, Cell { text: " ".into(), style: st });
            }
            if it.name.is_empty() {
                for x in x0 + 1..x0 + w - 1 {
                    f.set(x, y, Cell { text: "─".into(), style: border });
                }
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

const FONT: [(char, [&str; 5]); 14] = [
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
