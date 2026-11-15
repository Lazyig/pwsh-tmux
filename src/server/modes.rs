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
    pub owner: PaneId,
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
            if let Some(cell) = cells.iter_mut().rev().find(|c: &&mut Cell| !c.text.is_empty()) { cell.text.push(ch); }
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
        let mut t = Tree { kind, owner: pane, template, items: Vec::new(), expanded, sel: 0, top: 0, filter, client: None, tagged: HashSet::new() };
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
                        let np = w.panes().len();
                        items.push(TreeItem {
                            id: wkey.clone(),
                            depth: 1,
                            label: format!("{idx}: {}{flags}{}", w.name, if np == 1 { format!(": \"{}\"", srv.panes.get(&w.active).map(|p| p.title.as_str()).unwrap_or("")) } else { String::new() }),
                            target: format!("${}:{idx}", s.id),
                            kill: format!("kill-window -t '@{wid}'"),
                            expandable: np > 0,
                            preview: Some(w.active),
                        });
                        if np > 0 && self.expanded.contains(&wkey) {
                            for (pi, p) in w.panes().iter().enumerate() {
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
                    let sample: String = String::from_utf8_lossy(&b.data).chars().take(200).flat_map(|c| c.escape_default()).collect();
                    let time = chrono::DateTime::from_timestamp(b.created as i64, 0).map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_default();
                    items.push(TreeItem {
                        id: format!("b{}", b.name),
                        depth: 0,
                        label: format!("{}: {time}: {sample}", b.name),
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

    pub fn list_height(&self, h: usize) -> usize {
        let mut height = (h / 3) * 2;
        if height > self.items.len() { height = h / 2; }
        if height < 10 || h.saturating_sub(height) < 2 { h } else { height }
    }

    pub fn render(&self, srv: &Server, w: u16, h: u16) -> Vec<Line> {
        use super::render::{Frame, draw_box};
        let list_h = self.list_height(h as usize);
        let mut f = Frame::new(w, h);
        let top = self.top.min(self.sel).max((self.sel + 1).saturating_sub(list_h));
        let pane = self.items.get(self.sel).and_then(|i| i.preview);
        let wid = srv.target_for_pane(self.owner).wid;
        let selected = crate::format::parse_style(&srv.opt_window(wid, "mode-style"), Style { fg: Color::Idx(0), bg: Color::Idx(3), ..Style::default() }).unwrap_or_default();
        for row in 0..list_h {
            let i = top + row;
            let Some(it) = self.items.get(i) else { continue };
            let quick = if i < 10 { format!("({i}) ") } else if i < 36 { format!("(M-{}) ", (b'a' + (i - 10) as u8) as char) } else { "     ".into() };
            let flat = !matches!(self.kind, TreeKind::Windows | TreeKind::Sessions);
            let arrow = if flat { "" } else if it.expandable { if self.expanded.contains(&it.id) { "- " } else { "+ " } } else { "  " };
            let last = self.items.get(i + 1).is_none_or(|n| n.depth < it.depth) || !self.items[i+1..].iter().take_while(|n| n.depth >= it.depth).any(|n| n.depth == it.depth);
            let indent = if it.depth == 0 { String::new() } else { format!("{}{}", "    ".repeat(it.depth.saturating_sub(1) as usize), if last { "└─> " } else { "├─> " }) };
            let text = format!("{quick}{indent}{arrow}{}{}", it.label, if self.tagged.contains(&it.id) { "*" } else { "" });
            let style = if i == self.sel { selected } else if self.tagged.contains(&it.id) { Style { attrs: BOLD, ..Style::default() } } else { Style::default() };
            f.put_cells(0, row as u16, &line_from(&text, w as usize, style).cells);
        }
        let y = list_h as u16;
        if h.saturating_sub(y) > 4 && w > 4 {
            if let Some(it) = self.items.get(self.sel) {
                draw_box(&mut f, 0, y, w, h-y, Style::default(), "");
                let name = if self.kind == TreeKind::Buffers { it.target.clone() } else { it.target.rsplit(':').next().unwrap_or(&it.target).to_string() };
                f.text(1, y, &format!(" {name} (sort: {}) ", if self.kind == TreeKind::Buffers { "time" } else { "index" }), Style::default(), w-1);
                let (pw,ph) = (w-4,h-y-2);
                if self.kind == TreeKind::Buffers {
                    if let Some(b) = srv.buffers.iter().find(|b| b.name == it.target) {
                        for (row,line) in String::from_utf8_lossy(&b.data).lines().take(ph as usize).enumerate() { f.text(2, y+1+row as u16, line, Style::default(), w-2); }
                    }
                } else if let Some(p) = pane.and_then(|p| srv.panes.get(&p)) {
                    let window = &srv.windows[&srv.target_for_pane(p.id).wid.unwrap()];
                    let panes = window.panes();
                    let visible = panes.len().min((pw / 24).max(1) as usize);
                    let current = panes.iter().position(|id| *id == p.id).unwrap_or(0);
                    let start = current.saturating_sub(visible-1).min(panes.len()-visible);
                    let each = pw / visible as u16;
                    for (i,id) in panes.iter().enumerate().skip(start).take(visible) {
                        let pp = &srv.panes[id];
                        let x = 2 + (i-start) as u16 * each;
                        let width = if i == start+visible-1 { pw - (i-start) as u16 * each } else { each-1 };
                        let scr = pp.screen();
                        let (cy,cx) = scr.cursor_position();
                        let px = if scr.hide_cursor() { 0 } else { cx.saturating_sub(width/3).min(pp.size.0.saturating_sub(width)) };
                        let py = if scr.hide_cursor() { 0 } else { cy.saturating_sub(ph/3).min(pp.size.1.saturating_sub(ph)) };
                        for yy in 0..ph { for xx in 0..width {
                            if let Some(cell) = scr.cell(py+yy,px+xx) {
                                let mut cell = crate::cells::cell_from_vt(cell);
                                if !scr.hide_cursor() && (px+xx,py+yy)==(cx,cy) { cell.style.attrs |= INVERSE; }
                                f.set(x+xx,y+1+yy,cell);
                            }
                        } }
                        if i != start+visible-1 { for yy in 0..ph { f.text(x+width,y+1+yy,"│",Style::default(),w); } }
                        let label = format!(" {i} ");
                        let len = label.len() as u16;
                        if len <= each && ph > 1 {
                            let ox = (each-len+1)/2;
                            let oy = (ph+1)/2;
                            if ox>1 && ox+len<each-1 && ph>=3 { draw_box(&mut f,x+ox-1,y+oy,len+2,3,Style::default(),""); }
                            let color = if *id==window.active { "display-panes-active-colour" } else { "display-panes-colour" };
                            let style = Style { fg: crate::format::parse_color(&srv.opt_window(Some(window.id),color)).unwrap_or(Color::Default), ..Style::default() };
                            f.text(x+ox,y+1+oy,&label,style,w-2);
                        }
                    }
                }
            }
        }
        f.cells.chunks(w as usize).map(|c| Line { cells:c.to_vec(),wrapped:false }).collect()
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
    /// display-menu -b: border lines instead of menu-border-lines.
    pub lines: Option<String>,
    /// display-menu -s, -S and -H: their fg and bg replace those of menu-style,
    /// menu-border-style and menu-selected-style.
    pub styles: [Option<String>; 3],
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
        Menu { title, items, sel, x, y, w, h, pane, nomouse, stayopen, lines: None, styles: [None, None, None] }
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
        Menu { title, items, sel, x: mx, y: my, w, h, pane, nomouse: true, stayopen: false, lines: None, styles: [None, None, None] }
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

    pub fn draw(&self, f: &mut super::render::Frame, style: Style, border: Style, selected_style: Style, lines_kind: &str) {
        let (x0, y0, w, h) = (self.x, self.y, self.w, self.h);
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
    /// display-popup -b: border lines instead of popup-border-lines.
    pub lines: Option<String>,
    /// display-popup -s and -S: their fg and bg replace those of popup-style and
    /// popup-border-style.
    pub styles: [Option<String>; 2],
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
    // Matching foreground/background makes a solid block with colours; the
    // glyph itself remains visible when a terminal ignores colour escapes.
    let block = Style { fg: colour, bg: colour, ..Style::default() };
    for (i, ch) in text.chars().enumerate() {
        let Some((_, rows)) = FONT.iter().find(|(c, _)| *c == ch) else { continue };
        for (dy, row) in rows.iter().enumerate() {
            for (dx, b) in row.chars().enumerate() {
                if b == '#' {
                    f.set(x0 + i as u16 * 6 + dx as u16, y0 + dy as u16, Cell { text: "#".into(), style: block });
                }
            }
        }
    }
    true
}

pub fn clock_text(style: &str) -> String {
    clock_text_at(style, chrono::Local::now())
}

fn clock_text_at(style: &str, now: chrono::DateTime<chrono::Local>) -> String {
    use chrono::Timelike;
    let seconds = if style.ends_with("-with-seconds") { format!(":{:02}", now.second()) } else { String::new() };
    if style.starts_with("24") {
        format!("{:02}:{:02}{seconds}", now.hour(), now.minute())
    } else {
        // strftime "%l:%M " (or "%l:%M:%S ") then AM or PM
        let (pm, h) = now.hour12();
        format!("{h:>2}:{:02}{seconds} {}", now.minute(), if pm { "PM" } else { "AM" })
    }
}

#[cfg(test)]
mod clock_display_tests {
    use super::*;
    use chrono::TimeZone;
    #[test]
    fn clock_seconds_and_monochrome() {
        let now = chrono::Local.with_ymd_and_hms(2026, 11, 13, 13, 2, 3).unwrap();
        for (style, text) in [("12", " 1:02 PM"), ("24", "13:02"), ("12-with-seconds", " 1:02:03 PM"), ("24-with-seconds", "13:02:03")] {
            assert_eq!(clock_text_at(style, now), text);
        }
        let mut f = super::super::render::Frame::new(80, 10);
        assert!(big_text(&mut f, Rect { x: 0, y: 0, w: 80, h: 10 }, "12:34", Color::Default));
        assert!(f.cells.iter().any(|c| c.text == "#"));
    }
}
