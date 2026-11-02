//! Mouse input, following tmux 3.6 (server_client_check_mouse and friends).
//!
//! A terminal mouse report is classified into a key such as `MouseDown1Pane`,
//! `SecondClick1Pane`, `MouseDrag1Border` or `MouseDragEnd1StatusDefault`. That key is
//! looked up in the key tables like any other key; unbound events go to the pane under
//! the mouse. Drags started by `resize-pane -M`, `copy-mode -M` or `begin-selection`
//! from a mouse binding are then followed directly without further key events.

use super::modes::MenuAction;
use super::*;
use crate::format::Range;
use crate::keys::{Mouse, MouseKind};
use crate::layout::Dir;

/// tmux KEYC_CLICK_TIMEOUT: the window for second and third clicks.
pub const CLICK_TIMEOUT: Duration = Duration::from_millis(300);
/// tmux WINDOW_COPY_DRAG_REPEAT_TIME: copy-mode scrolling while dragging at an edge.
pub const DRAG_REPEAT: Duration = Duration::from_millis(50);

/// A classified mouse event (tmux's struct mouse_event once valid).
#[derive(Clone, Debug, Default)]
pub struct MouseCtx {
    /// Client position of the event and of the event before it (tmux m->x/y, m->lx/ly).
    pub x: u16,
    pub y: u16,
    pub lx: u16,
    pub ly: u16,
    pub sid: Option<SessId>,
    /// Window from a status line window range, else the current window.
    pub idx: Option<u32>,
    pub wid: Option<WinId>,
    /// Pane under the mouse (m->wp); None on the status line, meaning the window's active pane.
    pub pane: Option<PaneId>,
    /// The terminal report, forwarded to applications.
    pub event: Option<Mouse>,
    /// The key the event was classified as (m->key), e.g. "MouseDown1Pane".
    pub key: String,
    /// Not delivered to applications (the delayed DoubleClick).
    pub ignore: bool,
    /// First row of the status lines and how many there are (m->statusat, m->statuslines).
    pub statusat: Option<u16>,
    pub statuslines: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Where {
    Pane,
    Border,
    Status,
    StatusLeft,
    StatusRight,
    StatusDefault,
}

impl Where {
    fn name(self) -> &'static str {
        match self {
            Where::Pane => "Pane",
            Where::Border => "Border",
            Where::Status => "Status",
            Where::StatusLeft => "StatusLeft",
            Where::StatusRight => "StatusRight",
            Where::StatusDefault => "StatusDefault",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Type {
    Move,
    Down,
    Up,
    Drag,
    Wheel,
    Second,
    Double,
    Triple,
}

impl Server {
    /// A mouse report from a client's terminal.
    pub fn handle_mouse(&mut self, cid: ClientId, m: Mouse) {
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let last = c.mouse_last;
        let button = if m.kind == MouseKind::Up && m.button == 0 { last.2 } else { m.button };
        c.mouse_last = (m.x, m.y, button);
        if self.client_is_readonly(Some(cid)) {
            return;
        }
        if self.mouse_overlay(cid, &m) {
            return;
        }
        if self.clients.get(&cid).is_none_or(|c| c.prompt.is_some()) {
            return;
        }
        self.mouse_event(cid, m, last, false);
    }

    /// The click timer expired: after two clicks with no third, fire DoubleClick.
    pub fn mouse_click_timer(&mut self, cid: ClientId) {
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let click = std::mem::take(&mut c.click);
        if click.waiting == 2 {
            if let Some(m) = click.event {
                let last = (m.x, m.y, m.button);
                self.mouse_event(cid, m, last, true);
            }
        }
    }

    /// Menus get the raw events (tmux menu_key_cb); true when the event was used.
    fn mouse_overlay(&mut self, cid: ClientId, m: &Mouse) -> bool {
        let Some(c) = self.clients.get_mut(&cid) else { return true };
        match &mut c.overlay {
            Some(Overlay::Menu(menu)) => {
                let action = menu.mouse(m);
                self.menu_action(cid, action);
                true
            }
            Some(Overlay::Popup(p)) => {
                let (ix, iy, iw, ih) = p.inner();
                let pane = p.pane;
                if m.x >= ix && m.x < ix + iw && m.y >= iy && m.y < iy + ih {
                    let rel = Mouse { x: m.x - ix, y: m.y - iy, ..*m };
                    if let Some(pp) = self.panes.get(&pane) {
                        let mode = pp.screen().mouse_protocol_mode();
                        let sgr = pp.screen().mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr;
                        if let Some(b) = keys::encode_mouse(&rel, mode, sgr) {
                            pp.send(b);
                        }
                    }
                }
                true
            }
            Some(Overlay::DisplayPanes { .. }) => {
                c.overlay = None;
                c.dirty = true;
                false
            }
            None => false,
        }
    }

    /// Classify an event (server_client_check_mouse) and run or forward the resulting key.
    fn mouse_event(&mut self, cid: ClientId, m: Mouse, (lx, ly, lb): (u16, u16, u8), double: bool) {
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let (ty, x, y, b) = if double {
            (Type::Double, m.x, m.y, m.button)
        } else {
            match m.kind {
                MouseKind::Move => (Type::Move, m.x, m.y, 0),
                MouseKind::Drag if c.mouse_drag_flag != 0 => {
                    if (m.x, m.y) == (lx, ly) {
                        return;
                    }
                    (Type::Drag, m.x, m.y, m.button)
                }
                // a drag is named after where it started
                MouseKind::Drag => (Type::Drag, lx, ly, if m.button != 0 { m.button } else { lb }),
                MouseKind::WheelUp | MouseKind::WheelDown => (Type::Wheel, m.x, m.y, 0),
                MouseKind::Up => (Type::Up, m.x, m.y, if m.button != 0 { m.button } else { lb }),
                MouseKind::Down => {
                    let same = c.click.button == m.button && c.click.event.is_some_and(|e| e.mods == m.mods);
                    let mut ty = None;
                    match c.click.waiting {
                        1 => {
                            c.click.waiting = 0;
                            if same {
                                ty = Some(Type::Second);
                                c.click.waiting = 2;
                            }
                        }
                        2 => {
                            c.click.waiting = 0;
                            if same {
                                ty = Some(Type::Triple);
                            }
                        }
                        _ => {}
                    }
                    if ty == Some(Type::Triple) {
                        c.click.deadline = None;
                    } else {
                        if ty.is_none() {
                            ty = Some(Type::Down);
                            c.click.waiting = 1;
                        }
                        c.click.event = Some(m);
                        c.click.button = m.button;
                        c.click.deadline = Some(Instant::now() + CLICK_TIMEOUT);
                    }
                    (ty.unwrap(), m.x, m.y, m.button)
                }
            }
        };
        let Some((wh, mut ctx)) = self.mouse_where(cid, x, y) else { return };
        ctx.x = m.x;
        ctx.y = m.y;
        ctx.lx = lx;
        ctx.ly = ly;
        ctx.event = Some(m);
        ctx.ignore = double;
        let mods = m.mods.prefix();

        // any other event ends a drag in progress with MouseDragEnd
        let c = self.clients.get_mut(&cid).unwrap();
        if !matches!(ty, Type::Drag | Type::Wheel | Type::Double | Type::Triple) && c.mouse_drag_flag != 0 {
            let button = c.mouse_drag_flag - 1;
            c.mouse_drag_flag = 0;
            if let Some(Drag { kind: DragKind::Copy { pane } }) = c.drag.take() {
                self.copy_drag_timers.remove(&pane);
            }
            let key = format!("{mods}MouseDragEnd{button}{}", wh.name());
            self.mouse_key(cid, key, ctx);
            return;
        }
        let key = match ty {
            Type::Move => format!("{mods}MouseMove{}", wh.name()),
            Type::Drag => {
                c.mouse_drag_flag = b + 1;
                if c.drag.is_some() {
                    self.mouse_drag_update(cid, &ctx);
                    return;
                }
                format!("{mods}MouseDrag{b}{}", wh.name())
            }
            Type::Wheel => format!("{mods}{}{}", if m.kind == MouseKind::WheelUp { "WheelUp" } else { "WheelDown" }, wh.name()),
            Type::Up => format!("{mods}MouseUp{b}{}", wh.name()),
            Type::Down => format!("{mods}MouseDown{b}{}", wh.name()),
            Type::Second => format!("{mods}SecondClick{b}{}", wh.name()),
            Type::Double => format!("{mods}DoubleClick{b}{}", wh.name()),
            Type::Triple => format!("{mods}TripleClick{b}{}", wh.name()),
        };
        self.mouse_key(cid, key, ctx);
    }

    /// What is at client position (x, y): a status line range, a pane or a border.
    fn mouse_where(&self, cid: ClientId, x: u16, y: u16) -> Option<(Where, MouseCtx)> {
        let sid = self.client_session(cid)?;
        let s = &self.sessions[&sid];
        let c = &self.clients[&cid];
        let (_, ch) = c.size;
        let nstatus = self.status_lines(Some(sid)).min(ch.saturating_sub(1));
        let top = self.opt_session(Some(sid), "status-position") == "top";
        let statusat = (nstatus > 0).then_some(if top { 0 } else { ch - nstatus });
        let mut ctx = MouseCtx { sid: Some(sid), idx: Some(s.active), wid: Some(s.active_window()), statusat, statuslines: nstatus, ..Default::default() };
        if let Some(at) = statusat.filter(|at| y >= *at && y < at + nstatus) {
            let range = c.status_ranges.get((y - at) as usize).and_then(|l| l.get(x as usize)).cloned().flatten();
            let wh = match range {
                None => Where::StatusDefault,
                Some(Range::Left) => Where::StatusLeft,
                Some(Range::Right) => Where::StatusRight,
                Some(Range::Window(idx)) => {
                    let wid = *s.links.get(&idx)?;
                    ctx.idx = Some(idx);
                    ctx.wid = Some(wid);
                    Where::Status
                }
                Some(Range::Pane(p)) => {
                    self.panes.get(&p)?;
                    ctx.pane = Some(p);
                    Where::Status
                }
                Some(Range::Session(id)) => {
                    let other = self.sessions.get(&id)?;
                    ctx.sid = Some(id);
                    ctx.idx = Some(other.active);
                    ctx.wid = Some(other.active_window());
                    Where::Status
                }
                Some(Range::User(_)) => Where::Status,
            };
            return Some((wh, ctx));
        }
        let py = match statusat {
            Some(0) if y >= nstatus => y - nstatus,
            Some(at) if at > 0 && y >= at => at - 1,
            _ => y,
        };
        let wid = s.active_window();
        let w = &self.windows[&wid];
        if x > w.sx || py > w.sy {
            return None;
        }
        let rects: Vec<(PaneId, Rect)> = self.rects(wid).0.into_iter().map(|(p, r)| (p, self.content_rect(wid, r))).collect();
        // window_get_active_at: the pane including its right and bottom border
        let (pane, r) = *rects.iter().find(|(_, r)| x >= r.x && x <= r.right() && py >= r.y && py <= r.bottom())?;
        ctx.pane = Some(pane);
        if x < r.right() && py < r.bottom() {
            return Some((Where::Pane, ctx));
        }
        if !w.zoomed {
            let on_border = rects.iter().any(|(_, f)| {
                (f.right() == x && f.y <= py + 1 && f.bottom() >= py) || (f.bottom() == py && f.x <= x + 1 && f.right() >= x)
            });
            if on_border {
                return Some((Where::Border, ctx));
            }
        }
        None
    }

    /// Look a mouse key up in the key tables and run it, or give the event to the pane
    /// (the mouse half of server_client_key_callback).
    fn mouse_key(&mut self, cid: ClientId, key: String, mut ctx: MouseCtx) {
        ctx.key = key.clone();
        let Some(sid) = self.client_session(cid) else { return };
        let fs_pane = self.mouse_pane(&ctx);
        if !self.opt_session_on(Some(sid), "mouse") {
            if let Some(p) = fs_pane {
                self.window_pane_mouse(p, Some(cid), &ctx);
            }
            return;
        }
        let default_table = self.opt_session(Some(sid), "key-table");
        let default_table = if default_table.is_empty() { "root".to_string() } else { default_table };
        let client_table = self.clients[&cid].key_table.clone();
        let repeating = self.clients[&cid].repeat_until.is_some_and(|d| d > Instant::now());
        let first = if client_table == default_table {
            fs_pane.and_then(|p| self.mode_key_table(p)).unwrap_or(client_table)
        } else {
            client_table
        };
        let mut first = first;
        let mut table = first.clone();
        loop {
            if let Some(b) = self.bindings.get(&table).and_then(|t| t.get(&key)) {
                let (cmd, repeat) = (b.cmd.clone(), b.repeat);
                let ms = self.opt_session_num(Some(sid), "repeat-time").max(0) as u64;
                let c = self.clients.get_mut(&cid).unwrap();
                if repeat && ms > 0 {
                    c.repeat_until = Some(Instant::now() + Duration::from_millis(ms));
                } else {
                    c.repeat_until = None;
                    c.key_table = default_table.clone();
                }
                c.dirty = true;
                match crate::cmdparse::parse(&cmd) {
                    Ok(cmds) => {
                        let qctx = Ctx { client: Some(cid), pane: fs_pane, mouse: Some(ctx), from_key: true, ..Ctx::default() };
                        self.run_queue(Queue { cmds: cmds.into(), ctx: qctx, soft: 0 });
                    }
                    Err(e) => self.set_message(cid, &e),
                }
                return;
            }
            // movement keys are only reported when asked for; they don't leave the prefix table
            if key.contains("MouseMove") {
                break;
            }
            if table != default_table || repeating {
                let c = self.clients.get_mut(&cid).unwrap();
                c.key_table = default_table.clone();
                c.repeat_until = None;
                c.dirty = true;
                if table == default_table {
                    break;
                }
                if repeating {
                    first = default_table.clone();
                }
                table = default_table.clone();
                continue;
            }
            if first != table {
                return;
            }
            break;
        }
        if let Some(p) = fs_pane {
            self.window_pane_mouse(p, Some(cid), &ctx);
        }
    }

    /// Key table of a pane's mode (copy-mode or copy-mode-vi), if it has one.
    pub fn mode_key_table(&self, pane: PaneId) -> Option<String> {
        match self.panes.get(&pane)?.mode {
            Some(PaneMode::Copy(_)) => {
                let vi = self.opt_window(self.window_of(pane), "mode-keys") == "vi";
                Some(if vi { "copy-mode-vi" } else { "copy-mode" }.to_string())
            }
            _ => None,
        }
    }

    /// The pane a mouse event refers to (cmd_mouse_pane): the pane under it, or on the
    /// status line the active pane of the window there.
    pub fn mouse_pane(&self, m: &MouseCtx) -> Option<PaneId> {
        let wid = m.wid.filter(|w| self.windows.contains_key(w))?;
        match m.pane {
            Some(p) if self.windows[&wid].panes().contains(&p) => Some(p),
            Some(_) => None,
            None => Some(self.windows[&wid].active),
        }
    }

    /// Visible content rectangle of a pane in its window.
    pub fn pane_rect(&self, pane: PaneId) -> Option<Rect> {
        let wid = self.window_of(pane)?;
        let (rects, _) = self.rects(wid);
        rects.into_iter().find(|(p, _)| *p == pane).map(|(_, r)| self.content_rect(wid, r))
    }

    /// Pane-relative position of the event, or of the event before it (cmd_mouse_at).
    pub fn mouse_at(&self, pane: PaneId, m: &MouseCtx, last: bool) -> Option<(u16, u16)> {
        let (x, mut y) = if last { (m.lx, m.ly) } else { (m.x, m.y) };
        if m.statusat == Some(0) && y >= m.statuslines {
            y -= m.statuslines;
        }
        let r = self.pane_rect(pane)?;
        if x < r.x || x >= r.right() || y < r.y || y >= r.bottom() {
            return None;
        }
        Some((x - r.x, y - r.y))
    }

    /// Deliver a mouse key to a pane (window_pane_key): its mode, or the application.
    pub fn window_pane_mouse(&mut self, pane: PaneId, cid: Option<ClientId>, m: &MouseCtx) {
        let Some(p) = self.panes.get(&pane) else { return };
        match &p.mode {
            Some(PaneMode::Tree(_)) => self.tree_mouse(pane, cid, m),
            Some(PaneMode::Clock) => self.exit_mode(pane),
            Some(_) => {}
            None => {
                if m.ignore || p.dead.is_some() || p.options.get("@input-disabled") == Some("on") {
                    return;
                }
                let Some(ev) = m.event else { return };
                let Some((x, y)) = self.mouse_at(pane, m, false) else { return };
                let p = &self.panes[&pane];
                let mode = p.screen().mouse_protocol_mode();
                let sgr = p.screen().mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr;
                if let Some(bytes) = keys::encode_mouse(&Mouse { x, y, ..ev }, mode, sgr) {
                    p.send(bytes);
                }
            }
        }
    }

    /// mode_tree_key for mouse keys: clicks pick a line, a double click chooses it.
    fn tree_mouse(&mut self, pane: PaneId, cid: Option<ClientId>, m: &MouseCtx) {
        let Some((_, y)) = self.mouse_at(pane, m, false) else { return };
        let height = self.pane_rect(pane).map(|r| r.h as usize).unwrap_or(1);
        let mut choose = false;
        if let Some(PaneMode::Tree(tr)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
            let list_h = tr.list_height(height);
            if (y as usize) > list_h {
                return;
            }
            let top = tr.top.min(tr.sel).max((tr.sel + 1).saturating_sub(list_h));
            let row = top + y as usize;
            if row < tr.items.len() && matches!(m.key.as_str(), "MouseDown1Pane" | "MouseDown3Pane" | "DoubleClick1Pane") {
                tr.sel = row;
                tr.top = top;
                choose = m.key == "DoubleClick1Pane";
            }
        }
        if choose {
            self.mode_key(pane, cid, "Enter");
        } else if let Some(w) = self.window_of(pane) {
            self.mark_window_dirty(w);
        }
    }

    fn mouse_drag_update(&mut self, cid: ClientId, m: &MouseCtx) {
        match self.clients.get(&cid).and_then(|c| c.drag.as_ref()).map(|d| match d.kind {
            DragKind::Border => None,
            DragKind::Copy { pane } => Some(pane),
        }) {
            Some(None) => self.border_drag_update(cid, m),
            Some(Some(_)) => self.copy_drag_update(m),
            None => {}
        }
    }

    // ---- resize-pane -M -------------------------------------------------------------

    /// `resize-pane -M`: borders next to the mouse follow it until the button is released.
    pub fn start_border_drag(&mut self, cid: ClientId, m: Option<MouseCtx>) {
        let Some(m) = m else { return };
        if m.wid.is_none_or(|w| !self.windows.contains_key(&w)) || self.client_session(cid) != m.sid {
            return;
        }
        if let Some(c) = self.clients.get_mut(&cid) {
            c.drag = Some(Drag { kind: DragKind::Border });
        }
        self.border_drag_update(cid, &m);
    }

    /// cmd_resize_pane_mouse_update: resize the layout cells whose border is next to the
    /// previous mouse position by how far the mouse moved.
    fn border_drag_update(&mut self, cid: ClientId, m: &MouseCtx) {
        let Some(wid) = m.wid.filter(|w| self.windows.contains_key(w)) else {
            if let Some(c) = self.clients.get_mut(&cid) {
                c.drag = None;
            }
            return;
        };
        let adjust = |v: u16| match m.statusat {
            Some(0) if v >= m.statuslines => v - m.statuslines,
            Some(at) if at > 0 && v >= at => at - 1,
            _ => v,
        };
        let (x, y, lx, ly) = (m.x, adjust(m.y), m.lx, adjust(m.ly));
        let (rects, _) = self.rects(wid);
        if self.windows[&wid].zoomed {
            return;
        }
        let mut cells: Vec<(PaneId, bool)> = Vec::new();
        for (dx, dy) in [(0i32, 0i32), (0, 1), (1, 0), (0, -1), (-1, 0)] {
            let (bx, by) = (lx as i32 + dx, ly as i32 + dy);
            if bx < 0 || by < 0 {
                continue;
            }
            let (bx, by) = (bx as u16, by as u16);
            // layout_search_by_border: the cell left of / above a border at (bx, by)
            let found = rects
                .iter()
                .find(|(_, r)| r.right() == bx && by >= r.y && by < r.bottom())
                .map(|(p, _)| (*p, true))
                .or_else(|| rects.iter().find(|(_, r)| r.bottom() == by && bx >= r.x && bx < r.right()).map(|(p, _)| (*p, false)));
            if let Some(cell) = found {
                if !cells.contains(&cell) {
                    cells.push(cell);
                }
            }
        }
        let area = self.window_area(wid);
        let mut resized = false;
        for (pane, vertical) in cells {
            let delta = if vertical { x as i32 - lx as i32 } else { y as i32 - ly as i32 };
            if delta == 0 {
                continue;
            }
            let dir = match (vertical, delta > 0) {
                (true, true) => Dir::Right,
                (true, false) => Dir::Left,
                (false, true) => Dir::Down,
                (false, false) => Dir::Up,
            };
            let w = self.windows.get_mut(&wid).unwrap();
            w.layout.resize(pane, dir, delta.unsigned_abs() as u16, area);
            resized = true;
        }
        if resized {
            self.apply_layout(wid);
            self.mark_window_dirty(wid);
        }
    }

    // ---- copy mode ------------------------------------------------------------------

    /// window_copy_start_drag: select from where the drag started (the previous event).
    pub fn start_copy_drag(&mut self, cid: ClientId, m: &MouseCtx) {
        let Some(pane) = self.mouse_pane(m) else { return };
        if !matches!(self.panes.get(&pane).and_then(|p| p.mode.as_ref()), Some(PaneMode::Copy(_))) {
            return;
        }
        let Some((x, y)) = self.mouse_at(pane, m, true) else { return };
        if let Some(c) = self.clients.get_mut(&cid) {
            c.drag = Some(Drag { kind: DragKind::Copy { pane } });
        }
        if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
            cm.start_drag(y, x);
        }
        self.copy_drag_update(m);
    }

    /// window_copy_drag_update: the selection follows the mouse, scrolling at the edges.
    fn copy_drag_update(&mut self, m: &MouseCtx) {
        let Some(pane) = self.mouse_pane(m) else { return };
        if !matches!(self.panes.get(&pane).and_then(|p| p.mode.as_ref()), Some(PaneMode::Copy(_))) {
            return;
        }
        self.copy_drag_timers.remove(&pane);
        let Some((x, y)) = self.mouse_at(pane, m, false) else { return };
        let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) else { return };
        let (old_cy, old_cx) = cm.view_cursor();
        cm.drag_to(y, x);
        let (cy, cx) = cm.view_cursor();
        if old_cy != cy || old_cx == cx {
            if y == 0 {
                self.copy_drag_timers.insert(pane, Instant::now() + DRAG_REPEAT);
                cm.scroll_line(true);
            } else if y as usize == cm.view_height() - 1 {
                self.copy_drag_timers.insert(pane, Instant::now() + DRAG_REPEAT);
                cm.scroll_line(false);
            }
        }
        if let Some(w) = self.window_of(pane) {
            self.mark_window_dirty(w);
        }
    }

    /// window_copy_scroll_timer: keep scrolling while the mouse rests at the top or bottom.
    pub fn copy_drag_timers_due(&mut self, now: Instant) {
        let due: Vec<PaneId> = self.copy_drag_timers.iter().filter(|(_, t)| **t <= now).map(|(p, _)| *p).collect();
        for pane in due {
            self.copy_drag_timers.remove(&pane);
            let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) else { continue };
            let (cy, _) = cm.view_cursor();
            if cy == 0 {
                self.copy_drag_timers.insert(pane, now + DRAG_REPEAT);
                cm.scroll_line(true);
            } else if cy as usize == cm.view_height() - 1 {
                self.copy_drag_timers.insert(pane, now + DRAG_REPEAT);
                cm.scroll_line(false);
            }
            if let Some(w) = self.window_of(pane) {
                self.mark_window_dirty(w);
            }
        }
    }

    /// window_copy_move_mouse: a copy-mode command run from a mouse binding first moves
    /// the cursor to the mouse.
    pub fn copy_move_mouse(&mut self, m: &MouseCtx) {
        if m.event.is_some_and(|e| matches!(e.kind, MouseKind::WheelUp | MouseKind::WheelDown)) {
            return;
        }
        let Some(pane) = self.mouse_pane(m) else { return };
        let Some((x, y)) = self.mouse_at(pane, m, false) else { return };
        if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
            // tmux moves only the cursor; here the cursor is one end of the selection, so
            // an existing selection (to be copied next) is left alone
            if !cm.selection_active() {
                cm.move_to_view(y, x);
            }
        }
    }

    /// Position of a menu or popup of `w` x `h` (cmd_display_menu_get_pos); None when it
    /// does not fit on the client.
    #[allow(clippy::too_many_arguments)]
    pub fn overlay_position(&self, cid: ClientId, t: vars::Target, m: Option<&MouseCtx>, xarg: Option<&str>, yarg: Option<&str>, w: u16, h: u16) -> Option<(u16, u16)> {
        let c = self.clients.get(&cid)?;
        let (sx, sy) = c.size;
        if w > sx || h > sy {
            return None;
        }
        let (sx, sy, w, h) = (sx as i64, sy as i64, w as i64, h as i64);
        let mut v: HashMap<String, String> = HashMap::new();
        let mut set = |k: &str, n: i64| {
            v.insert(k.to_string(), n.to_string());
        };
        if let Some(m) = m {
            set("popup_mouse_x", m.x as i64);
            set("popup_mouse_y", m.y as i64);
        }
        let sid = c.session;
        let lines = self.status_lines(sid).min((sy as u16).saturating_sub(1)) as i64;
        let mut top = 0;
        if lines > 0 {
            let at_top = self.opt_session(sid, "status-position") == "top";
            top = if at_top { lines } else { 0 };
            let found = c.status_ranges.iter().enumerate().find_map(|(line, tags)| {
                tags.iter().position(|r| matches!(r, Some(Range::Window(i)) if Some(*i) == t.idx)).map(|x| (line as i64, x as i64))
            });
            if let Some((line, x)) = found {
                set("popup_window_status_line_x", x);
                set("popup_window_status_line_y", if at_top { line + 1 + h } else { sy - lines + line });
            }
            set("popup_status_line_y", if at_top { lines + h } else { sy - lines });
        }
        set("popup_width", w);
        set("popup_height", h);
        set("popup_centre_x", ((sx - 1) / 2 - w / 2).max(0));
        let n = (sy - 1) / 2 + h / 2;
        set("popup_centre_y", if n >= sy { sy - h } else { n });
        if let Some(m) = m {
            let (mx, my) = (m.x as i64, m.y as i64);
            set("popup_mouse_centre_x", (mx - w / 2).max(0));
            let n = my - h / 2;
            set("popup_mouse_centre_y", if n + h >= sy { sy - h } else { n });
            set("popup_mouse_top", (my + h).min(sy - 1));
            set("popup_mouse_bottom", (my - h).max(0));
        }
        if let Some(r) = t.pane.and_then(|p| self.pane_rect(p)) {
            let n = top + r.y as i64 + h;
            set("popup_pane_top", if n >= sy { sy - h } else { n });
            set("popup_pane_bottom", top + r.bottom() as i64);
            set("popup_pane_left", r.x as i64);
            set("popup_pane_right", (r.right() as i64 - w).max(0));
        }
        let strtol = |s: &str| -> i64 {
            let s = s.trim_start();
            let end = s.char_indices().find(|(i, ch)| !(ch.is_ascii_digit() || (*i == 0 && (*ch == '-' || *ch == '+')))).map_or(s.len(), |(i, _)| i);
            s[..end].parse().unwrap_or(0)
        };
        let xf = match xarg {
            None | Some("C") => "#{popup_centre_x}",
            Some("R") => "#{popup_pane_right}",
            Some("P") => "#{popup_pane_left}",
            Some("M") => "#{popup_mouse_centre_x}",
            Some("W") => "#{popup_window_status_line_x}",
            Some(other) => other,
        };
        let mut x = strtol(&self.expand_extra(t, Some(cid), xf, v.clone()));
        if x + w >= sx {
            x = sx - w;
        } else if x < 0 {
            x = 0;
        }
        let yf = match yarg {
            None | Some("C") => "#{popup_centre_y}",
            Some("P") => "#{popup_pane_bottom}",
            Some("M") => "#{popup_mouse_top}",
            Some("S") => "#{popup_status_line_y}",
            Some("W") => "#{popup_window_status_line_y}",
            Some(other) => other,
        };
        let mut y = strtol(&self.expand_extra(t, Some(cid), yf, v));
        y = if y < h { 0 } else { y - h };
        if y + h >= sy {
            y = sy - h;
        }
        Some((x.max(0) as u16, y.max(0) as u16))
    }

    /// Mouse mode the client's terminal should report (server_client_reset_state): with the
    /// mouse option on, button events (all motion if a pane wants it); otherwise whatever the
    /// active pane asked for. 0 off, 1000, 1002 or 1003.
    pub fn outer_mouse_mode(&self, cid: ClientId) -> u16 {
        use vt100::MouseProtocolMode as P;
        let Some(c) = self.clients.get(&cid) else { return 0 };
        let Some(sid) = c.session else { return 0 };
        let wid = self.sessions[&sid].active_window();
        let level = |m: P| match m {
            P::None => 0,
            P::Press | P::PressRelease => 1000,
            P::ButtonMotion => 1002,
            P::AnyMotion => 1003,
        };
        let pane_mode = |p: PaneId| self.panes.get(&p).filter(|p| p.mode.is_none()).map_or(0, |p| level(p.screen().mouse_protocol_mode()));
        let mut mode = match &c.overlay {
            Some(Overlay::Menu(menu)) if !menu.nomouse => 1003,
            Some(Overlay::Popup(p)) => pane_mode(p.pane),
            Some(_) => 0,
            None => pane_mode(self.windows[&wid].active),
        };
        if self.opt_session_on(Some(sid), "mouse") {
            if c.overlay.is_none() {
                mode = if self.windows[&wid].panes().iter().any(|p| pane_mode(*p) == 1003) { 1003 } else { 0 };
            }
            if mode != 1003 {
                mode = 1002;
            }
        }
        mode
    }
}

impl Server {
    /// The mouse_* formats for the command being run from a mouse binding.
    pub fn mouse_var(&self, name: &str, client: Option<ClientId>) -> Option<String> {
        let m = self.cur_mouse.as_ref()?;
        let pane = self.mouse_pane(m);
        let at = pane.and_then(|p| self.mouse_at(p, m, false).map(|xy| (p, xy)));
        let attached = client.and_then(|c| self.clients.get(&c)).is_some_and(|c| c.attached);
        let status_y = || -> Option<u16> {
            let at = m.statusat?;
            (attached && m.y >= at && m.y < at + m.statuslines).then(|| m.y - at)
        };
        match name {
            "mouse_pane" => pane.map(|p| format!("%{p}")),
            "mouse_x" => at.map(|(_, (x, _))| x).or_else(|| status_y().map(|_| m.x)).map(|v| v.to_string()),
            "mouse_y" => at.map(|(_, (_, y))| y).or_else(status_y).map(|v| v.to_string()),
            "mouse_status_line" => status_y().map(|v| v.to_string()),
            "mouse_status_range" => {
                let y = status_y()?;
                let c = self.clients.get(&client?)?;
                let r = c.status_ranges.get(y as usize)?.get(m.x as usize)?.as_ref()?;
                Some(match r {
                    Range::User(s) => s.clone(),
                    r => r.name().to_string(),
                })
            }
            "mouse_word" | "mouse_line" => {
                let (p, (x, y)) = at?;
                let pane = self.panes.get(&p)?;
                let ws = self.opt_session(None, "word-separators");
                match &pane.mode {
                    Some(PaneMode::Copy(cm)) => {
                        let row = cm.view_top() + y as usize;
                        let get = |i: usize| cm.line(i).cloned();
                        if name == "mouse_word" { grid_word(get, cm.line_count(), x as usize, row, &ws) } else { grid_line(cm.line(row)?) }
                    }
                    Some(_) => None,
                    None => {
                        let screen = pane.screen();
                        let total = screen.history_rows_len();
                        let row = total - screen.size().0.get() as usize + y as usize;
                        let get = |i: usize| (i < total).then(|| crate::cells::line_at(screen, i));
                        if name == "mouse_word" { grid_word(get, total, x as usize, row, &ws) } else { grid_line(&get(row)?) }
                    }
                }
            }
            _ => None,
        }
    }
}

/// Cells used on a line, trailing blanks excluded (grid_line_length).
fn line_length(l: &Line) -> usize {
    l.cells.iter().rposition(|c| c.text != " ").map_or(0, |i| i + 1)
}

fn is_word_separator(ws: &str, c: Option<&crate::cells::Cell>) -> bool {
    match c {
        None => true,
        Some(c) => c.text == " " || c.text == "	" || (c.text.chars().count() == 1 && ws.contains(c.text.as_str())),
    }
}

/// The word around (x, y), following wrapped lines (format_grid_word).
fn grid_word(get: impl Fn(usize) -> Option<Line>, rows: usize, mut x: usize, mut y: usize, ws: &str) -> Option<String> {
    let mut line = get(y)?;
    let mut found = false;
    loop {
        let c = line.cells.get(x);
        if !c.is_some_and(|c| c.is_continuation()) && is_word_separator(ws, c) {
            found = true;
            break;
        }
        if x == 0 {
            if y == 0 {
                break;
            }
            let prev = get(y - 1)?;
            if !prev.wrapped {
                break;
            }
            y -= 1;
            line = prev;
            x = line_length(&line);
            if x == 0 {
                break;
            }
        }
        x -= 1;
    }
    let mut out = String::new();
    loop {
        if found {
            let end = line_length(&line);
            if end == 0 || x + 1 == end {
                if y + 1 >= rows || !line.wrapped {
                    break;
                }
                y += 1;
                line = get(y)?;
                x = 0;
            } else {
                x += 1;
            }
        }
        found = true;
        let c = line.cells.get(x);
        if c.is_some_and(|c| c.is_continuation()) {
            continue;
        }
        if is_word_separator(ws, c) {
            break;
        }
        out.push_str(&c.unwrap().text);
    }
    (!out.is_empty()).then_some(out)
}

/// The text of a line (format_grid_line).
fn grid_line(l: &Line) -> Option<String> {
    let s: String = l.cells[..line_length(l)].iter().filter(|c| !c.is_continuation()).map(|c| c.text.as_str()).collect();
    (!s.is_empty()).then_some(s)
}

impl super::modes::Menu {
    /// menu_key_cb for mouse events.
    pub fn mouse(&mut self, m: &Mouse) -> MenuAction {
        let release = m.kind == MouseKind::Up;
        let wheel = matches!(m.kind, MouseKind::WheelUp | MouseKind::WheelDown);
        let drag = matches!(m.kind, MouseKind::Drag | MouseKind::Move);
        if self.nomouse {
            // only the first button is ignored; anything else closes the menu
            return if m.kind == MouseKind::Down && m.button == 1 || m.kind == MouseKind::Drag && m.button == 1 {
                MenuAction::None
            } else {
                MenuAction::Cancel
            };
        }
        let count = self.items.len() as u16;
        let inner_w = self.w.saturating_sub(4);
        if m.x < self.x || m.x > self.x + 4 + inner_w || m.y < self.y + 1 || m.y > self.y + count {
            if !self.stayopen {
                if release {
                    return MenuAction::Cancel;
                }
            } else if !release && !wheel && !drag {
                return MenuAction::Cancel;
            }
            self.sel = None;
            return MenuAction::None;
        }
        let chosen = if !self.stayopen { release } else { !wheel && !drag };
        if chosen {
            return self.choose();
        }
        self.sel = Some((m.y - (self.y + 1)) as usize);
        MenuAction::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str, wrapped: bool) -> Line {
        let mut l = Line { cells: s.chars().map(|c| crate::cells::Cell { text: c.to_string(), style: Default::default() }).collect(), wrapped };
        l.cells.resize(12, crate::cells::Cell::default());
        l
    }

    #[test]
    fn mouse_word_and_line() {
        let ws = "!\"#$%&'()*+,-./:;<=>?@[\\]^`{|}~";
        let rows = [line("alpha beta", false), line("x=1 y=22  ab", true), line("cd tail", false)];
        let get = |i: usize| rows.get(i).cloned();
        assert_eq!(grid_word(get, 3, 7, 0, ws).as_deref(), Some("beta"));
        assert_eq!(grid_word(get, 3, 6, 1, ws).as_deref(), Some("22"));
        // a word wrapped onto the next line
        assert_eq!(grid_word(get, 3, 1, 2, ws).as_deref(), Some("abcd"));
        // on blank space past the text there is no word
        assert_eq!(grid_word(get, 3, 11, 0, ws), None);
        assert_eq!(grid_line(&rows[0]).as_deref(), Some("alpha beta"));
        assert_eq!(grid_line(&line("", false)), None);
    }

    #[test]
    fn run_shell_delay_is_removed_for_the_resumed_command() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(super::super::strip_delay(&v(&["run-shell", "-d", "0.3"])), v(&["run-shell"]));
        assert_eq!(super::super::strip_delay(&v(&["run-shell", "-bd", "1", "echo"])), v(&["run-shell", "-b", "echo"]));
        assert_eq!(super::super::strip_delay(&v(&["run-shell", "-d0.5", "-c", "/d", "echo -d"])), v(&["run-shell", "-c", "/d", "echo -d"]));
        assert_eq!(super::super::strip_delay(&v(&["run-shell", "-t", "%1", "-d", "2", "x"])), v(&["run-shell", "-t", "%1", "x"]));
    }
}
