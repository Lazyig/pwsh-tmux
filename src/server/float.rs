//! Floating panes (tmux 3.7/3.8): panes above the tiled layout with their own position,
//! size and z-order, modal panes, and the JSON layout format.

use super::vars::Target;
use super::*;
use crate::layout::{FGeom, LeafInfo};
use crate::opts::Opts;

/// tmux PANE_FLOATOVERZOOM: stays visible above a zoomed pane (new-pane -A, -O).
pub const OVER_ZOOM: u8 = 1;
/// PANE_CAPTUREALLKEYS: every key, the prefix included, goes to the pane (new-pane -O -K).
pub const CAPTURE_KEYS: u8 = 2;
/// PANE_CLOSEONCLICK: a click outside closes the modal pane (new-pane -O -C).
pub const CLOSE_ON_CLICK: u8 = 4;
/// PANE_CLOSEONCANCEL: Escape or C-c closes the modal pane (new-pane -O -D).
pub const CLOSE_ON_CANCEL: u8 = 8;

/// tmux PANE_MINIMUM and PANE_MAXIMUM.
const PANE_MIN: i64 = 1;
const PANE_MAX: i64 = 10000;

fn err<T>(s: impl Into<String>) -> Result<T, String> {
    Err(s.into())
}

/// The window row of a mouse row (below a top status line, or clamped above a bottom one).
fn drag_row(m: &super::input::MouseCtx, v: u16) -> i32 {
    // in the window (m->y + m->oy), less the status lines
    let v = v.saturating_add(m.oy);
    (match m.statusat {
        Some(0) if v >= m.statuslines => v - m.statuslines,
        Some(at) if at > 0 && v >= at => at - 1,
        _ => v,
    }) as i32
}

/// tmux strtonum.
/// The exit status new-pane -W returns: tmux's WEXITSTATUS (the low 8 bits) for a command
/// that exited; an out-of-range code (a Windows NTSTATUS such as 0xC000013A) whose low bits
/// are 0 still fails.
fn wait_status(s: i32) -> u8 {
    match s {
        0..=255 => s as u8,
        _ => match (s as u32 & 0xff) as u8 {
            0 => 1,
            b => b,
        },
    }
}

/// strtonum: strtoll skips white space in front; anything after the number is invalid.
pub(crate) fn strtonum(v: &str, lo: i64, hi: i64) -> Result<i64, String> {
    let n: i64 = v.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']).parse::<i128>()
        .map(|n| n.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
        .map_err(|_| "invalid".to_string())?;
    if n < lo {
        Err("too small".into())
    } else if n > hi {
        Err("too large".into())
    } else {
        Ok(n)
    }
}

/// tmux args_string_percentage: `N` within lo..=hi or `N%` of `cur`.
pub fn percentage(v: &str, lo: i64, hi: i64, cur: i64) -> Result<i64, String> {
    if v.is_empty() {
        return Err("empty".into());
    }
    match v.strip_suffix('%') {
        Some(p) => {
            let n = strtonum(p, 0, 1000)? * cur / 100;
            if n < lo {
                Err("too small".into())
            } else if n > hi {
                Err("too large".into())
            } else {
                Ok(n)
            }
        }
        None => strtonum(v, lo, hi),
    }
}

impl Server {
    pub fn over_zoom(&self, pane: PaneId) -> bool {
        self.panes.get(&pane).is_some_and(|p| p.fflags & OVER_ZOOM != 0)
    }

    /// The pane filling a zoomed window (tmux `window_zoomed_pane`).
    pub fn zoomed_pane(&self, wid: WinId) -> Option<PaneId> {
        let w = self.windows.get(&wid)?;
        if !w.zoomed {
            return None;
        }
        let active_over = w.is_floating(w.active) && self.over_zoom(w.active);
        Some(match w.zoom_pane {
            Some(z) if active_over && z != w.active && w.layout.panes().contains(&z) => z,
            _ => w.active,
        })
    }

    /// tmux `window_active_pane_is_over_zoom`.
    pub fn active_over_zoom(&self, wid: WinId) -> bool {
        let w = &self.windows[&wid];
        w.zoomed && w.is_floating(w.active) && self.over_zoom(w.active) && self.zoomed_pane(wid) != Some(w.active)
    }

    /// tmux `window_pane_is_visible`: a zoomed window shows its zoomed pane and the floating
    /// panes above the zoom.
    pub fn pane_visible(&self, wid: WinId, pane: PaneId) -> bool {
        let w = &self.windows[&wid];
        match self.zoomed_pane(wid) {
            None => true,
            Some(z) => z == pane || (w.is_floating(pane) && self.over_zoom(pane)),
        }
    }

    /// tmux `window_pane_is_floating`: floating and shown as such (a zoom hides the other
    /// floating panes and makes a zoomed floating pane fill the window).
    pub fn shown_floating(&self, wid: WinId, pane: PaneId) -> bool {
        self.windows.get(&wid).is_some_and(|w| w.is_floating(pane)) && self.pane_visible(wid, pane) && self.zoomed_pane(wid) != Some(pane)
    }

    /// The visible floating panes and their cells, back to front (drawing order).
    pub fn float_rects(&self, wid: WinId) -> Vec<(PaneId, FGeom)> {
        let Some(w) = self.windows.get(&wid) else { return Vec::new() };
        let zoomed = self.zoomed_pane(wid);
        w.zlist()
            .into_iter()
            .rev()
            .filter_map(|p| {
                let g = w.layout.geom(p)?;
                match zoomed {
                    Some(z) if p == z || !self.over_zoom(p) => None,
                    _ => Some((p, g)),
                }
            })
            .collect()
    }

    /// tmux `window_pane_get_pane_lines`: a floating pane's own option, else the window's.
    pub fn pane_lines(&self, wid: WinId, pane: PaneId) -> String {
        if self.windows.get(&wid).is_some_and(|w| w.is_floating(pane)) {
            self.opt_pane(pane, "pane-border-lines")
        } else {
            self.opt_window(Some(wid), "pane-border-lines")
        }
    }

    /// tmux `window_pane_get_pane_status`: `top`, `bottom` or `off`. Tiled panes follow the
    /// window option (`top-floating` and `bottom-floating` are off for them); floating panes
    /// their own, with the floating variants meaning top and bottom.
    pub fn pane_status(&self, wid: WinId, pane: PaneId) -> &'static str {
        let floating = self.windows.get(&wid).is_some_and(|w| w.is_floating(pane) && self.zoomed_pane(wid) != Some(pane));
        let v = if floating {
            if self.pane_lines(wid, pane) == "none" {
                return "off";
            }
            self.opt_pane(pane, "pane-border-status")
        } else {
            self.opt_window(Some(wid), "pane-border-status")
        };
        match v.as_str() {
            "top" => "top",
            "bottom" => "bottom",
            "top-floating" if floating => "top",
            "bottom-floating" if floating => "bottom",
            _ => "off",
        }
    }

    /// Whether (x, y) of the window is on floating pane `pane`, its border included (tmux
    /// `window_pane_contains`).
    pub fn float_contains(&self, wid: WinId, pane: PaneId, x: i32, y: i32) -> bool {
        let Some(g) = self.windows.get(&wid).and_then(|w| w.layout.geom(pane)) else { return false };
        let b = i32::from(self.pane_lines(wid, pane) != "none");
        x >= g.x - b && x < g.x + g.w as i32 + b && y >= g.y - b && y < g.y + g.h as i32 + b
    }

    /// The topmost floating pane at (x, y), or the modal pane's answer (tmux
    /// `window_get_active_at` for floating panes): `Some(None)` means a modal pane blocks it.
    pub fn float_at(&self, wid: WinId, x: i32, y: i32) -> Option<Option<PaneId>> {
        let w = self.windows.get(&wid)?;
        if let Some(m) = w.modal {
            return Some(self.float_contains(wid, m, x, y).then_some(m));
        }
        self.float_rects(wid).into_iter().rev().find(|(p, _)| self.float_contains(wid, *p, x, y)).map(|(p, _)| Some(p))
    }

    /// tmux `window_pane_zindex`: position among the floating panes (0 is the front); tiled
    /// panes (and floating panes hidden by a zoom) are behind those before them.
    pub fn pane_z(&self, wid: WinId, pane: PaneId) -> usize {
        let w = &self.windows[&wid];
        let mut i = 0;
        for p in w.zlist() {
            let floating = self.shown_floating(wid, p);
            if p == pane {
                return if floating { i } else { i + 1 };
            }
            if floating {
                i += 1;
            }
        }
        i
    }

    /// The content position and size of a pane in its window (floating or tiled).
    pub fn pane_geom(&self, wid: WinId, pane: PaneId) -> Option<FGeom> {
        let w = self.windows.get(&wid)?;
        if self.zoomed_pane(wid) != Some(pane) {
            if let Some(g) = w.layout.geom(pane) {
                return Some(g);
            }
        }
        let r = self.rects(wid).0.into_iter().find(|(p, _)| *p == pane)?.1;
        let c = self.content_rect(wid, r);
        Some(FGeom { x: c.x as i32, y: c.y as i32, w: c.w, h: c.h })
    }

    /// tmux server_client_handle_dead_key: a key pressed in a dead pane kept by
    /// remain-on-exit key or failed-key (split-window/new-pane -k and -m) closes it. Callers
    /// decide about read-only clients (server_client_handle_key skips them, forward_key
    /// does not). False when the key goes on as usual.
    pub fn dead_key(&mut self, pane: PaneId) -> bool {
        if self.panes.get(&pane).is_none_or(|p| p.dead.is_none()) {
            return false;
        }
        if !matches!(self.opt_pane(pane, "remain-on-exit").as_str(), "key" | "failed-key") {
            return false;
        }
        if let Some(p) = self.panes.get_mut(&pane) {
            p.options.set("remain-on-exit", "off".into());
        }
        self.kill_pane(pane);
        true
    }

    /// tmux `window_set_active_pane` (with `window_redraw_active_switch` raising a floating
    /// pane to the front): nothing but the modal pane can become active while there is one,
    /// and a zoomed window is unzoomed unless the pane is shown above the zoom.
    pub fn set_active_pane(&mut self, wid: WinId, pane: PaneId) -> bool {
        let Some(w) = self.windows.get(&wid) else { return false };
        if w.modal.is_some_and(|m| m != pane) || pane == w.active {
            return false;
        }
        let zoomed = self.zoomed_pane(wid);
        let keep_zoom = zoomed.is_some() && self.pane_visible(wid, pane);
        if zoomed.is_some() && !keep_zoom {
            self.unzoom_z(wid);
        }
        if self.windows[&wid].is_floating(pane) {
            self.raise(wid, pane);
        }
        let w = self.windows.get_mut(&wid).unwrap();
        w.set_active(pane);
        if let Some(z) = zoomed {
            if keep_zoom {
                // the zoomed pane stays zoomed below a pane shown above the zoom
                w.zoom_pane = (z != pane).then_some(z);
            } else {
                w.zoomed = false;
                w.zoom_pane = None;
            }
        }
        self.apply_layout(wid);
        self.mark_window_dirty(wid);
        true
    }

    /// Bring a floating pane to the front (behind a modal pane).
    pub fn raise(&mut self, wid: WinId, pane: PaneId) {
        let w = self.windows.get_mut(&wid).unwrap();
        let mut z = w.zlist();
        z.retain(|p| *p != pane);
        let at = w.modal.filter(|m| *m != pane).and_then(|m| z.iter().position(|p| *p == m)).map_or(0, |i| i + 1);
        z.insert(at, pane);
        w.zorder = z;
    }

    /// The z-order change of tmux `window_unzoom`: a zoomed floating pane goes back to the
    /// front when it is active, else behind the other floating panes.
    pub fn unzoom_z(&mut self, wid: WinId) {
        let order = self.unzoomed_zlist(wid);
        if let Some(w) = self.windows.get_mut(&wid) {
            w.zorder = order;
        }
    }

    /// The z-order the window will have once unzoomed (see [`Server::unzoom_z`]).
    fn unzoomed_zlist(&self, wid: WinId) -> Vec<PaneId> {
        let w = &self.windows[&wid];
        let mut order = w.zlist();
        let Some(z) = self.zoomed_pane(wid).filter(|z| w.is_floating(*z)) else { return order };
        order.retain(|p| *p != z);
        let at = if z == w.active { 0 } else { order.iter().position(|p| !w.layout.is_float(*p)).unwrap_or(order.len()) };
        order.insert(at, z);
        order
    }

    pub fn unzoom(&mut self, wid: WinId) -> bool {
        if !self.windows.get(&wid).is_some_and(|w| w.zoomed) {
            return false;
        }
        self.unzoom_z(wid);
        let w = self.windows.get_mut(&wid).unwrap();
        w.zoomed = false;
        w.zoom_pane = None;
        self.apply_layout(wid);
        self.mark_window_dirty(wid);
        true
    }

    /// Zoom `pane`'s window again after an unzoom (tmux `window_pop_zoom`): the active pane,
    /// or `was` while the active pane floats above the zoom.
    pub fn rezoom(&mut self, wid: WinId, was: Option<PaneId>) {
        let w = &self.windows[&wid];
        let active_over = w.is_floating(w.active) && self.over_zoom(w.active);
        let target = if active_over { was.filter(|p| w.layout.panes().contains(p)) } else { Some(w.active) };
        let Some(target) = target.or(Some(w.active)) else { return };
        if w.panes().len() < 2 {
            return;
        }
        let w = self.windows.get_mut(&wid).unwrap();
        w.zoomed = true;
        w.zoom_pane = (target != w.active).then_some(target);
        self.apply_layout(wid);
        self.mark_window_dirty(wid);
    }

    /// Remove pane `id` from its window's layout and pick the next active pane (tmux
    /// `window_lost_pane` and `layout_close_pane`). False when the window has no pane left.
    pub fn detach_pane(&mut self, wid: WinId, id: PaneId) -> bool {
        let area = self.window_area(wid);
        let w = self.windows.get_mut(&wid).unwrap();
        let order = w.panes();
        let layout = std::mem::replace(&mut w.layout, Node::Leaf(PaneId::MAX));
        match layout.close(id, area) {
            Some(n) => w.layout = n,
            None => return false,
        }
        if w.modal_last == Some(id) {
            w.modal_last = None;
        }
        if w.zoom_pane == Some(id) {
            w.zoom_pane = None;
        }
        w.last_panes.retain(|p| *p != id);
        w.zorder.retain(|p| *p != id);
        w.float_saved.remove(&id);
        if w.active == id {
            let live = w.layout.panes();
            let mut next = None;
            if w.modal == Some(id) {
                next = w.modal_last.take();
                w.modal = None;
            }
            let i = order.iter().position(|p| *p == id).unwrap_or(0);
            let next = next
                .filter(|p| live.contains(p))
                .or_else(|| w.last_panes.iter().copied().find(|p| live.contains(p)))
                .or_else(|| if i > 0 { order.get(i - 1).copied() } else { order.get(1).copied() })
                .unwrap_or(live[0]);
            w.active = next;
            w.last_panes.retain(|p| *p != next);
        } else if w.modal == Some(id) {
            w.modal = None;
            w.modal_last = None;
        }
        true
    }

    /// Continue the queues of new-pane -W once the pane's command has exited (or the pane
    /// is gone: 128 + SIGHUP).
    pub fn finish_pane_waits(&mut self, id: PaneId, status: Option<i32>) {
        if let Some(queues) = self.pane_waits.remove(&id) {
            self.resume_pane_waits(queues, status);
        }
    }

    /// Continue parked new-pane -W queues: split-window's after hook (tmux inserts it
    /// before blocking, so it runs once the item continues), then the rest of the queue.
    pub fn resume_pane_waits(&mut self, queues: Vec<(Vec<String>, Queue)>, status: Option<i32>) {
        let code = status.map_or(129, wait_status);
        for (argv, mut q) in queues {
            q.ctx.retval = code.into();
            self.after_command(&argv, &q.ctx);
            for c in std::mem::take(&mut self.ev.inject).into_iter().rev() {
                q.cmds.push_front(c);
            }
            self.run_queue(q);
        }
    }

    /// tmux's last pane for last-pane and select-pane -l: the top of the last-panes stack, or
    /// the other pane of a window with two panes.
    pub fn last_pane_of(&self, wid: WinId) -> Option<PaneId> {
        let w = self.windows.get(&wid)?;
        w.last().or_else(|| {
            let panes = w.panes();
            (panes.len() == 2).then(|| if panes[0] == w.active { panes[1] } else { panes[0] })
        })
    }

    /// Keep floating panes inside the window after it is resized (tmux
    /// `layout_clamp_floating_panes`).
    pub fn clamp_floats(&mut self, wid: WinId) {
        let Some(w) = self.windows.get(&wid) else { return };
        let (sx, sy) = (w.sx as i32, w.sy as i32);
        let pads: Vec<(PaneId, i32)> = w.layout.floats().into_iter().map(|(p, _)| (p, i32::from(self.pane_lines(wid, p) != "none"))).collect();
        let w = self.windows.get_mut(&wid).unwrap();
        for (p, pad) in pads {
            let Some(g) = w.layout.geom_mut(p) else { continue };
            let avail = (sx - 2 * pad).max(0);
            if g.w as i32 > avail {
                g.w = avail.max(PANE_MIN as i32) as u16;
            }
            let avail = (sy - 2 * pad).max(0);
            if g.h as i32 > avail {
                g.h = avail.max(PANE_MIN as i32) as u16;
            }
            // tmux compares in unsigned arithmetic: a pane far enough off the left or top
            // edge wraps round and is pulled back to the padding
            let past = |off: i32, size: u16, s: i32| (off as u32).wrapping_add(size as u32 + pad as u32) > s as u32;
            if past(g.x, g.w, sx) {
                g.x = if g.w as i32 + 2 * pad >= sx { pad } else { sx - g.w as i32 - pad };
            }
            if past(g.y, g.h, sy) {
                g.y = if g.h as i32 + 2 * pad >= sy { pad } else { sy - g.h as i32 - pad };
            }
        }
    }

    /// Whether a control client asked for the JSON layouts (refresh-client -f new-layouts):
    /// other control clients get the old format.
    fn old_layouts(&self, client: Option<ClientId>) -> bool {
        client.and_then(|c| self.clients.get(&c)).is_some_and(|c| c.control > 0 && !c.ctl.new_layouts)
    }

    /// #{window_layout} (the unzoomed layout) and #{window_visible_layout} (what is shown:
    /// a zoomed window has its zoomed pane and the panes floating above it).
    pub fn window_layout(&self, wid: WinId, visible: bool, client: Option<ClientId>) -> Option<String> {
        let w = self.windows.get(&wid)?;
        let area = self.window_area(wid);
        let zoomed = self.zoomed_pane(wid).filter(|_| visible);
        let node = match zoomed {
            None => w.layout.clone(),
            Some(z) => {
                let mut n = Node::Leaf(z);
                for (p, g) in self.float_rects(wid).into_iter().rev() {
                    n.insert_float(Some(z), p, g);
                }
                n
            }
        };
        if self.old_layouts(client) {
            return Some(node.to_layout_string(area));
        }
        let base = self.opt_window_num(Some(wid), "pane-base-index").max(0) as usize;
        let panes = w.panes();
        // the saved layout of a zoomed window has its z-order once unzoomed (layout_cell_zindex)
        let z = if visible { w.zlist() } else { self.unzoomed_zlist(wid) };
        let info = |p: PaneId| {
            let floating = node.is_float(p);
            LeafInfo {
                active: p == w.active,
                last: w.last_panes.iter().filter(|x| panes.contains(x) && **x != w.active).position(|x| *x == p),
                index: panes.iter().position(|x| *x == p).unwrap_or(0) + base,
                z: floating.then(|| z.iter().filter(|x| node.is_float(**x)).position(|x| *x == p).unwrap_or(0)),
            }
        };
        Some(node.to_json_layout(area, &info))
    }

    /// tmux redraw_pane_status_width: how wide a pane status line is formatted. Its cells
    /// start two in from the pane (`x0`) on row `by` of the window; only those on the client
    /// (`clip`, window columns and rows shown), inside the window for a floating pane and not
    /// under a floating pane above it count, and the line ends with the last of them. 0 when
    /// none is shown.
    pub fn pane_status_width(&self, wid: WinId, pane: PaneId, (x0, by): (i32, i32), full: usize, clip: (u16, u16)) -> usize {
        let Some(w) = self.windows.get(&wid) else { return 0 };
        if by < 0 || by >= clip.1 as i32 {
            return 0;
        }
        let floats = self.float_rects(wid);
        let floating = floats.iter().any(|(p, _)| *p == pane);
        // a floating pane clipped by the window edge has its right border on the last column
        let mut end = x0 + full as i32;
        if floating {
            end = end.min(w.sx as i32 - 1);
        }
        end = end.min(clip.0 as i32);
        let above: Vec<PaneId> = if floating {
            floats.iter().skip_while(|(p, _)| *p != pane).skip(1).map(|(p, _)| *p).collect()
        } else {
            floats.iter().map(|(p, _)| *p).collect()
        };
        (x0.max(0)..end).rev().find(|x| !above.iter().any(|p| self.float_contains(wid, *p, *x, by))).map_or(0, |x| (x - x0 + 1) as usize)
    }

    /// The floating panes' geometries, for telling a layout change (the old layout format
    /// leaves them out). Not the z-order a raised pane changes: tmux fires
    /// window-layout-changed for move-pane -z but not for selecting a pane.
    pub fn float_key(&self, wid: WinId) -> String {
        let Some(w) = self.windows.get(&wid) else { return String::new() };
        let mut floats = w.layout.floats();
        floats.sort_by_key(|f| f.0);
        format!("{floats:?}")
    }

    /// Expand a size or position argument for a target (tmux
    /// args_percentage_and_expand).
    pub fn pct_expand(&self, v: &str, lo: i64, hi: i64, cur: i64, t: Target) -> Result<i64, String> {
        match v.strip_suffix('%') {
            Some(p) => percentage(&format!("{}%", self.expand(t, None, p)), lo, hi, cur),
            None => percentage(&self.expand(t, None, v), lo, hi, cur),
        }
    }

    /// tmux `layout_floating_args_parse`: a floating cell from -x/-y (size, with the border
    /// lines), -X/-Y (position of the border); unset ones come from `base` or default to half
    /// the width and a quarter of the height, cascaded from the top left.
    fn float_geometry(&mut self, wid: WinId, o: &Opts, lines_none: bool, base: Option<FGeom>, t: Target) -> Result<FGeom, String> {
        let w = &self.windows[&wid];
        let (wsx, wsy) = (w.sx as i64, w.sy as i64);
        let has_floats = w.layout.floats().iter().any(|(p, _)| self.shown_floating(wid, *p));
        let mut sx = base.map_or(wsx / 2, |g| g.w as i64);
        let mut sy = base.map_or(wsy / 4, |g| g.h as i64);
        let mut ox = base.map(|g| g.x as i64);
        let mut oy = base.map(|g| g.y as i64);
        let border = if lines_none { 0 } else { 1 };
        if let Some(v) = o.get('x') {
            sx = self.pct_expand(v, 0, PANE_MAX, wsx, t).map_err(|e| format!("position {e}"))? - 2 * border;
        }
        if let Some(v) = o.get('y') {
            sy = self.pct_expand(v, 0, PANE_MAX, wsy, t).map_err(|e| format!("position {e}"))? - 2 * border;
        }
        if let Some(v) = o.get('X') {
            ox = Some(self.pct_expand(v, -sx, wsx, wsx, t).map_err(|e| format!("position {e}"))? + border);
        }
        if let Some(v) = o.get('Y') {
            oy = Some(self.pct_expand(v, -sy, wsy, wsy, t).map_err(|e| format!("position {e}"))? + border);
        }
        let w = self.windows.get_mut(&wid).unwrap();
        if !has_floats {
            w.last_new_pane = (0, 0);
        }
        let ox = ox.unwrap_or_else(|| {
            let last = w.last_new_pane.0 as i64;
            let mut x = if last == 0 { 4 } else { last + 4 };
            if last != 0 && x + sx + border > wsx {
                x = 4;
            }
            w.last_new_pane.0 = x as i32;
            x
        });
        let oy = oy.unwrap_or_else(|| {
            let last = w.last_new_pane.1 as i64;
            let mut y = if last == 0 { 2 } else { last + 2 };
            if last != 0 && y + sy + border > wsy {
                y = 2;
            }
            w.last_new_pane.1 = y as i32;
            y
        });
        if !(PANE_MIN..=PANE_MAX).contains(&sx) {
            return Err("invalid width".into());
        }
        if !(PANE_MIN..=PANE_MAX).contains(&sy) {
            return Err("invalid height".into());
        }
        Ok(FGeom { x: ox as i32, y: oy as i32, w: sx as u16, h: sy as u16 })
    }

    /// tmux `layout_split_floating_cell`: split-window in a floating pane puts a pane of the
    /// same size beside it (-h), below it, or before it (-b), sharing out the space when it
    /// would not fit; -f makes it as big as it can be. Returns (target, new) cells.
    fn split_float_geometry(&self, wid: WinId, target: FGeom, lines_none: bool, horizontal: bool, before: bool, full: bool) -> Result<(FGeom, FGeom), String> {
        let w = &self.windows[&wid];
        let (tb, bb, lb, rb) = (1i32, w.sy as i32 - 1, 3i32, w.sx as i32 - 3);
        let border = if lines_none { 0 } else { 1 };
        let mut old = target;
        let (mut ow, mut oh) = (old.w as i32, old.h as i32);
        if lb > old.x - border {
            old.x = lb + border;
        }
        if rb < old.x + ow + border {
            old.x = rb - ow - border;
        }
        if tb > old.y - border {
            old.y = tb + border;
        }
        if bb < old.y + oh + border {
            old.y = bb - oh - border;
        }
        let mut new = old;
        let (mut nw, mut nh) = (ow, oh);
        if horizontal {
            new.x += if before { -(ow + 2 * border) } else { ow + 2 * border };
        } else {
            new.y += if before { -(oh + 2 * border) } else { oh + 2 * border };
        }
        if lb > new.x - border {
            let space = old.x + ow - lb - 3 * border + 1;
            nw = space / 2;
            ow = space / 2;
            new.x = lb + border;
            old.x = new.x + nw + 2 * border;
            if space % 2 == 0 {
                ow -= 1;
            }
        } else if rb < new.x + nw + border {
            let space = rb - old.x - 3 * border + 1;
            nw = space / 2;
            ow = space / 2;
            new.x = old.x + ow + 2 * border;
            if space % 2 == 0 {
                nw -= 1;
            }
        } else if tb > new.y - border {
            let space = oh + old.y - tb - 3 * border + 1;
            nh = space / 2;
            oh = space / 2;
            new.y = tb + border;
            old.y = new.y + nh + 2 * border;
            if space % 2 == 0 {
                oh -= 1;
            }
        } else if bb < new.y + nh + border {
            let space = bb - old.y - 3 * border + 1;
            nh = space / 2;
            oh = space / 2;
            new.y = old.y + oh + 2 * border;
            if space % 2 == 0 {
                nh -= 1;
            }
        }
        if full {
            if horizontal {
                new.y = tb + border;
                nh = bb - tb - 2 * border;
                if before {
                    new.x = lb + border;
                    nw = old.x - new.x - 2 * border;
                } else {
                    nw = rb - new.x - border;
                }
            } else {
                new.x = lb + border;
                nw = rb - lb - 2 * border;
                if before {
                    new.y = tb + border;
                    nh = old.y - new.y - 2 * border;
                } else {
                    nh = bb - new.y - border;
                }
            }
        }
        if nw < 1 || nh < 1 || ow < 1 || oh < 1 {
            return Err("no space for a new pane".into());
        }
        (old.w, old.h, new.w, new.h) = (ow as u16, oh as u16, nw as u16, nh as u16);
        Ok((old, new))
    }

    /// new-pane and split-window (tmux `cmd_split_window_exec`).
    pub fn cmd_new_pane(&mut self, cmd: &str, o: &Opts, env: &[(String, String)], ctx: &mut Ctx) -> Result<Flow, String> {
        let tt = self.resolve_pane(ctx, o.get('t'))?;
        let (wid, target) = (tt.wid.ok_or("no window")?, tt.pane.ok_or("no pane")?);
        let mut restore_zoom = self.active_over_zoom(wid);
        let was_zoomed = self.zoomed_pane(wid);
        let floating = if cmd == "new-pane" {
            !o.has('L')
        } else {
            if !self.pane_visible(wid, target) {
                restore_zoom = false;
            }
            if !restore_zoom {
                self.unzoom_window(wid);
            }
            self.windows[&wid].is_floating(target)
        };
        let split = cmd == "split-window";
        if o.has('O') {
            if !floating {
                return err("modal pane must be floating");
            }
            if self.windows[&wid].modal.is_some() {
                return err("window already has a modal pane");
            }
        }
        // -M sizes the pane by dragging; it needs the mouse event of a binding
        if o.has('M') && floating && ctx.mouse.is_none() {
            return Ok(Flow::Next);
        }
        let over_zoom = o.has('O') || (floating && o.has('A'));
        if self.windows[&wid].zoomed && over_zoom {
            restore_zoom = true;
        }
        let command = cmds::spawn_command(o, o.has('I'))?;
        let lines = match o.get('B') {
            None => self.opt_window(Some(wid), "pane-border-lines"),
            Some(v) => options::validate("pane-border-lines", Some(v), None).map_err(|e| format!("pane-border-lines {e}"))?,
        };
        let cwd = o.get('c').map(|c| self.expand(tt, ctx.client, c));
        let new = if floating {
            let lines_none = lines == "none";
            let g = if split {
                let tg = self.windows[&wid].layout.geom(target).ok_or("pane not floating")?;
                let (old, new) = self.split_float_geometry(wid, tg, lines_none, o.has('h'), o.has('b'), o.has('f'))?;
                if let Some(g) = self.windows.get_mut(&wid).unwrap().layout.geom_mut(target) {
                    *g = old;
                }
                new
            } else {
                self.float_geometry(wid, o, lines_none, None, tt)?
            };
            self.unzoom_window(wid);
            let cwd = cwd.or_else(|| self.panes.get(&target).map(|p| p.current_path.clone()));
            let new = match self.spawn_pane(tt.sid, g.w, g.h, &command, cwd.as_deref(), env) {
                Ok(n) => n,
                Err(e) => {
                    if restore_zoom {
                        self.rezoom_window(wid, was_zoomed);
                    }
                    return Err(e);
                }
            };
            let w = self.windows.get_mut(&wid).unwrap();
            w.layout.insert_float(Some(target), new, g);
            w.add_pane_order(target, new, o.has('b'), o.has('f'), true);
            let mut z = w.zlist();
            z.retain(|p| *p != new);
            let at = w.modal.and_then(|m| z.iter().position(|p| *p == m)).map_or(0, |i| i + 1);
            z.insert(at, new);
            w.zorder = z;
            new
        } else {
            if self.windows[&wid].is_floating(target) {
                return err("can't split a floating pane");
            }
            let horizontal = o.has('h');
            let r = if o.has('f') {
                self.window_area(wid)
            } else {
                let rects = self.rects(wid).0;
                rects.iter().find(|(p, _)| *p == target).map(|(_, r)| self.content_rect(wid, *r)).unwrap_or_default()
            };
            let cur = if horizontal { r.w } else { r.h } as i64;
            let size = if let Some(l) = o.get('l') {
                Some(self.pct_expand(l, 0, i32::MAX as i64, cur, tt).map_err(|e| format!("invalid tiled geometry {e}"))?)
            } else if let Some(p) = o.get('p') {
                let p = strtonum(&self.expand(tt, ctx.client, p), 0, 100).map_err(|e| format!("invalid tiled geometry {e}"))?;
                Some(cur * p / 100)
            } else {
                None
            };
            if !restore_zoom {
                self.unzoom_window(wid);
            }
            let new = self.split_pane(tt, horizontal, o.has('b'), size.map(|s| s.min(u16::MAX as i64) as u16), o.has('f'), cwd, &command, env)?;
            self.windows.get_mut(&wid).unwrap().zorder.retain(|p| *p != new);
            new
        };
        // pane attributes
        let mut flags = 0;
        if over_zoom {
            flags |= OVER_ZOOM;
        }
        if o.has('O') {
            for (c, f) in [('K', CAPTURE_KEYS), ('C', CLOSE_ON_CLICK), ('D', CLOSE_ON_CANCEL)] {
                if o.has(c) {
                    flags |= f;
                }
            }
        }
        // styles are stored as given (tmux options_set_string does not check them)
        let mut set = Vec::new();
        if let Some(style) = o.get('s') {
            set.push(("window-style", style.to_string()));
            set.push(("window-active-style", style.to_string()));
        }
        if let Some(style) = o.get('S') {
            set.push(("pane-active-border-style", style.to_string()));
        }
        if let Some(style) = o.get('R') {
            set.push(("pane-border-style", style.to_string()));
        }
        if o.has('B') {
            set.push(("pane-border-lines", lines.clone()));
        }
        if o.has('k') || o.has('m') {
            set.push(("remain-on-exit", "key".into()));
            if let Some(m) = o.get('m') {
                set.push(("remain-on-exit-format", m.to_string()));
            }
        }
        let title = o.get('T').map(|t| self.expand(tt, ctx.client, t));
        if let Some(p) = self.panes.get_mut(&new) {
            p.fflags = flags;
            for (k, v) in set {
                p.options.set(k, v);
            }
            if let Some(t) = &title {
                p.title = t.clone();
            }
        }
        if let Some(t) = title {
            let mut p = self.payload_pane(new);
            p.str("new_title", t);
            self.fire("pane-title-changed", p);
        }
        // the new pane becomes active (a modal pane always), unless -d or a modal pane
        let w = self.windows.get_mut(&wid).unwrap();
        if o.has('O') {
            w.modal_last = Some(w.active);
            w.modal = Some(new);
            w.set_active(new);
        } else if (!o.has('d') || w.panes().len() == 1) && w.modal.is_none() {
            w.set_active(new);
        }
        if restore_zoom {
            self.rezoom_window(wid, was_zoomed);
        } else if !floating && o.has('Z') {
            let w = self.windows.get_mut(&wid).unwrap();
            w.zoomed = true;
            w.zoom_pane = None;
        }
        self.apply_layout(wid);
        if let Some(sid) = tt.sid {
            self.mark_session_dirty(sid);
        }
        self.mark_window_dirty(wid);
        // spawn_pane's window-layout-changed: reported with the command's other changes
        if o.has('P') {
            let fmt = o.get('F').unwrap_or("#{session_name}:#{window_index}.#{pane_index}");
            let line = self.expand_for_pane(new, fmt);
            ctx.out.push_str(&line);
            ctx.out.push('\n');
        }
        ctx.pane = Some(new);
        // -M: the new pane follows the drag that created it
        if o.has('M') && floating {
            if let (Some(cid), Some(m)) = (ctx.client, ctx.mouse.clone()) {
                self.start_float_drag(cid, &m, new, false, true);
            }
        }
        if o.has('W') {
            // continued by finish_pane_waits when the command exits
            return Err(format!("\0pane-wait\0{new}"));
        }
        Ok(Flow::Next)
    }

    /// break-pane -W (tmux `cmd_break_pane_float`): float a tiled pane where it is, its
    /// size and position restored from when it last floated unless given.
    pub fn cmd_float_pane(&mut self, o: &Opts, wid: WinId, pane: PaneId, t: Target) -> Result<Flow, String> {
        let w = &self.windows[&wid];
        if w.is_floating(pane) {
            return err("pane is already floating");
        }
        if w.zoomed {
            return err("can't float a pane while window is zoomed");
        }
        let base = w.float_saved.get(&pane).copied();
        let lines_none = self.opt_window(Some(wid), "pane-border-lines") == "none";
        let g = self.float_geometry(wid, o, lines_none, base, t).map_err(|e| format!("failed to float pane: {e}"))?;
        let area = self.window_area(wid);
        let w = self.windows.get_mut(&wid).unwrap();
        w.layout.float_pane(pane, g, area)?;
        w.float_saved.insert(pane, g);
        self.raise(wid, pane);
        if !o.has('d') {
            self.set_active_pane(wid, pane);
        }
        self.apply_layout(wid);
        self.mark_window_dirty(wid);
        // join-pane.c / cmd-resize-pane.c fire it whether or not anything changed
        self.ev.layout_fired.push((wid, 1));
        Ok(Flow::Next)
    }

    /// join-pane of a floating pane onto itself (tmux `cmd_join_pane_tile`): tile it again.
    pub fn cmd_tile_pane(&mut self, o: &Opts, wid: WinId, pane: PaneId, _t: Target) -> Result<Flow, String> {
        let w = &self.windows[&wid];
        let Some(g) = w.layout.geom(pane) else { return err("pane is not floating") };
        if w.zoomed {
            return err("can't tile a pane while window is zoomed");
        }
        let area = self.window_area(wid);
        let w = self.windows.get_mut(&wid).unwrap();
        w.layout.tile_pane(pane, area)?;
        w.float_saved.insert(pane, g);
        let mut z = w.zlist();
        z.retain(|p| *p != pane);
        z.push(pane);
        w.zorder = z;
        if !o.has('d') {
            self.set_active_pane(wid, pane);
        }
        self.apply_layout(wid);
        self.mark_window_dirty(wid);
        // join-pane.c / cmd-resize-pane.c fire it whether or not anything changed
        self.ev.layout_fired.push((wid, 1));
        Ok(Flow::Next)
    }

    /// move-pane -P/-z/-X/-Y/-U/-D/-L/-R (tmux `cmd_join_pane_place`, `_zindex`, `_move`).
    pub fn cmd_move_float(&mut self, o: &Opts, wid: WinId, pane: PaneId, t: Target) -> Result<Flow, String> {
        let w = &self.windows[&wid];
        let Some(g) = w.layout.geom(pane) else { return err("pane is not floating") };
        let (wx, wy) = (w.sx as i32, w.sy as i32);
        let border = i32::from(self.pane_lines(wid, pane) != "none");
        let (mut x, mut y) = (g.x, g.y);
        if let Some(pos) = o.get('P') {
            let (px, py) = (g.w as i32, g.h as i32);
            let mut z = self.windows[&wid].zlist();
            let floating_hidden = |p: PaneId| self.windows[&wid].is_floating(p);
            let visible = |p: PaneId| self.pane_visible(wid, p);
            let i = z.iter().position(|p| *p == pane).unwrap_or(0);
            match pos {
                "top-left" => (x, y) = (border, border),
                "top-centre" | "top-center" => (x, y) = ((wx - px) / 2, border),
                "top-right" => (x, y) = (wx - px - border, border),
                "centre-left" | "center-left" => (x, y) = (border, (wy - py) / 2),
                "centre" | "center" => (x, y) = ((wx - px) / 2, (wy - py) / 2),
                "centre-right" | "center-right" => (x, y) = (wx - px - border, (wy - py) / 2),
                "bottom-left" => (x, y) = (border, wy - py - border),
                "bottom-centre" | "bottom-center" => (x, y) = ((wx - px) / 2, wy - py - border),
                "bottom-right" => (x, y) = (wx - px - border, wy - py - border),
                "top-left-centre" | "top-left-center" => (x, y) = (wx / 4 - px / 2, wy / 4 - py / 2),
                "top-right-centre" | "top-right-center" => (x, y) = ((3 * wx) / 4 - px / 2, wy / 4 - py / 2),
                "bottom-left-centre" | "bottom-left-center" => (x, y) = (wx / 4 - px / 2, (3 * wy) / 4 - py / 2),
                "bottom-right-centre" | "bottom-right-center" => (x, y) = ((3 * wx) / 4 - px / 2, (3 * wy) / 4 - py / 2),
                "front" => {
                    z.remove(i);
                    z.insert(0, pane);
                }
                "back" => {
                    z.remove(i);
                    let at = z.iter().position(|p| !floating_hidden(*p)).unwrap_or(z.len());
                    z.insert(at, pane);
                }
                "forward" | "forward-loop" => {
                    let prev = (0..i).rev().find(|j| visible(z[*j]));
                    match prev {
                        Some(j) => {
                            z.remove(i);
                            z.insert(j, pane);
                        }
                        None if pos == "forward-loop" => {
                            z.remove(i);
                            let at = z.iter().position(|p| !floating_hidden(*p)).unwrap_or(z.len());
                            z.insert(at, pane);
                        }
                        None => {}
                    }
                }
                "backward" | "backward-loop" => {
                    let next = (i + 1..z.len()).find(|j| visible(z[*j]));
                    match next {
                        Some(j) if floating_hidden(z[j]) => {
                            let other = z[j];
                            z.remove(i);
                            let at = z.iter().position(|p| *p == other).unwrap() + 1;
                            z.insert(at, pane);
                        }
                        _ if pos == "backward-loop" => {
                            z.remove(i);
                            z.insert(0, pane);
                        }
                        _ => {}
                    }
                }
                _ => return err(format!("unknown position: {pos}")),
            }
            self.windows.get_mut(&wid).unwrap().zorder = z;
        } else if let Some(v) = o.get('z') {
            let n = strtonum(v, 0, u32::MAX as i64).map_err(|e| format!("z-index {e}"))? as usize;
            let w = self.windows.get_mut(&wid).unwrap();
            let mut z = w.zlist();
            z.retain(|p| *p != pane);
            let floats = z.iter().take_while(|p| w.is_floating(**p)).count();
            z.insert(n.min(floats), pane);
            w.zorder = z;
        } else {
            if let Some(v) = o.get('X') {
                x = self.pct_expand(v, -wx as i64, wx as i64, wx as i64, t).map_err(|e| format!("position {e}"))? as i32 + border;
            }
            if let Some(v) = o.get('Y') {
                y = self.pct_expand(v, -wy as i64, wy as i64, wy as i64, t).map_err(|e| format!("position {e}"))? as i32 + border;
            }
            for c in ['U', 'D', 'L', 'R'] {
                let Some(v) = o.get(c) else { continue };
                let v = if v.is_empty() { "1" } else { v };
                let n = strtonum(v, i32::MIN as i64, i32::MAX as i64).map_err(|e| format!("offset {e}"))? as i32;
                match c {
                    'U' => y -= n,
                    'D' => y += n,
                    'L' => x -= n,
                    _ => x += n,
                }
            }
            if (x, y) == (g.x, g.y) {
                return Ok(Flow::Next);
            }
        }
        if let Some(g) = self.windows.get_mut(&wid).unwrap().layout.geom_mut(pane) {
            (g.x, g.y) = (x, y);
        }
        self.mark_window_dirty(wid);
        // join-pane.c / cmd-resize-pane.c fire it whether or not anything changed
        self.ev.layout_fired.push((wid, 1));
        Ok(Flow::Next)
    }

    /// resize-pane on a floating pane: -x/-y set the size with the border lines (tmux
    /// `layout_resize_floating_pane_to`), -U/-D/-L/-R move a border (`_floating_pane`).
    pub fn cmd_resize_float(&mut self, o: &Opts, wid: WinId, pane: PaneId, _t: Target) -> Result<Flow, String> {
        let w = &self.windows[&wid];
        let (wsx, wsy) = (w.sx as i64, w.sy as i64);
        let lines_none = self.pane_lines(wid, pane) == "none";
        let to = |size: i64| -> Result<u16, String> {
            let size = if !lines_none && size >= PANE_MIN + 2 { size - 2 } else { size };
            if !(PANE_MIN..=PANE_MAX).contains(&size) {
                return Err("size size is too big or too small".into());
            }
            Ok(size as u16)
        };
        if let Some(v) = o.get('x') {
            let x = percentage(v, 0, PANE_MAX, wsx).map_err(|e| format!("width {e}"))?;
            let x = to(x)?;
            if let Some(g) = self.windows.get_mut(&wid).unwrap().layout.geom_mut(pane) {
                g.w = x;
            }
        }
        if let Some(v) = o.get('y') {
            let mut y = percentage(v, 0, PANE_MAX, wsy).map_err(|e| format!("height {e}"))?;
            // tmux counts a window pane status line next to the pane (window_get_pane_status:
            // only top and bottom, not the -floating values)
            let g = self.windows[&wid].layout.geom(pane).unwrap_or_default();
            match self.opt_window(Some(wid), "pane-border-status").as_str() {
                "top" if g.y == 1 => y += 1,
                "bottom" if g.y + g.h as i32 == wsy as i32 - 1 => y += 1,
                _ => {}
            }
            let y = to(y)?;
            if let Some(g) = self.windows.get_mut(&wid).unwrap().layout.geom_mut(pane) {
                g.h = y;
            }
        }
        // once -U or -L is seen, later flags move the offset too (tmux sets `opposite`
        // and never clears it)
        let mut opposite = false;
        for c in ['U', 'D', 'L', 'R'] {
            let Some(v) = o.get(c) else { continue };
            let v = if !v.is_empty() { v.to_string() } else { o.rest.first().cloned().unwrap_or_else(|| "1".into()) };
            let n = strtonum(&v, i32::MIN as i64, i32::MAX as i64).map_err(|e| format!("adjustment {e}"))? as i32;
            if n == 0 {
                continue;
            }
            let g = self.windows.get_mut(&wid).unwrap().layout.geom_mut(pane).unwrap();
            opposite |= matches!(c, 'U' | 'L');
            let size = match c {
                'U' | 'D' => g.h as i32 + n,
                _ => g.w as i32 + n,
            };
            if !(PANE_MIN as i32..=PANE_MAX as i32).contains(&size) {
                return err("adjustment change is too big or too small");
            }
            if matches!(c, 'U' | 'D') {
                g.h = size as u16;
                if opposite {
                    g.y -= n;
                }
            } else {
                g.w = size as u16;
                if opposite {
                    g.x -= n;
                }
            }
        }
        self.apply_layout(wid);
        self.mark_window_dirty(wid);
        // join-pane.c / cmd-resize-pane.c fire it whether or not anything changed
        self.ev.layout_fired.push((wid, 1));
        Ok(Flow::Next)
    }

    /// Start dragging a floating pane from a mouse binding: resize-pane -M (`moving` false),
    /// move-pane -M, or new-pane -M (`new`, sized from where the drag started).
    pub fn start_float_drag(&mut self, cid: ClientId, m: &super::input::MouseCtx, pane: PaneId, moving: bool, new: bool) {
        let Some(wid) = self.window_of(pane) else { return };
        if !self.windows[&wid].is_floating(pane) || self.client_session(cid) != m.sid {
            return;
        }
        let (lx, ly) = (m.lx as i32 + m.ox as i32, drag_row(m, m.ly));
        let kind = if new { DragKind::NewFloat { pane, x: lx, y: ly } } else { DragKind::Float { pane, moving } };
        if !new {
            self.set_active_pane(wid, pane);
        }
        if let Some(c) = self.clients.get_mut(&cid) {
            c.drag = Some(Drag { kind });
        }
        self.float_drag_update(cid, m);
    }

    /// One mouse event of a floating pane drag (tmux cmd_resize_pane_mouse_resize_move_floating,
    /// cmd_join_pane_mouse_move and cmd_split_window_mouse_resize).
    pub fn float_drag_update(&mut self, cid: ClientId, m: &super::input::MouseCtx) {
        let kind = match self.clients.get(&cid).and_then(|c| c.drag.as_ref()).map(|d| &d.kind) {
            Some(DragKind::Float { pane, moving }) => (*pane, *moving, None),
            Some(DragKind::NewFloat { pane, x, y }) => (*pane, false, Some((*x, *y))),
            _ => return,
        };
        let (pane, moving, start) = kind;
        let Some(wid) = self.window_of(pane).filter(|w| self.windows[w].is_floating(pane)) else {
            if let Some(c) = self.clients.get_mut(&cid) {
                c.drag = None;
            }
            return;
        };
        let (x, y, lx, ly) = (m.x as i32 + m.ox as i32, drag_row(m, m.y), m.lx as i32 + m.ox as i32, drag_row(m, m.ly));
        let border = self.pane_lines(wid, pane) != "none";
        let w = self.windows.get_mut(&wid).unwrap();
        let Some(g) = w.layout.geom_mut(pane) else { return };
        let old = *g;
        let min = PANE_MIN as i32;
        if let Some((dx, dy)) = start {
            let b = i32::from(border);
            let (mut sx, mut xoff) = if x >= dx { (x - dx + 1, dx + b) } else { (dx - x + 1, x + b) };
            let (mut sy, mut yoff) = if y >= dy { (y - dy + 1, dy + b) } else { (dy - y + 1, y + b) };
            if x < dx {
                xoff = dx - sx + 1 + b;
            }
            if y < dy {
                yoff = dy - sy + 1 + b;
            }
            if border {
                sx = if sx <= 2 { min } else { sx - 2 };
                sy = if sy <= 2 { min } else { sy - 2 };
            }
            *g = FGeom { x: xoff, y: yoff, w: sx.max(min) as u16, h: sy.max(min) as u16 };
        } else if moving {
            g.x += x - lx;
            g.y += y - ly;
        } else {
            let (left, right, top, bottom) = (g.x - 1, g.x + g.w as i32, g.y - 1, g.y + g.h as i32);
            let (gw, gh) = (g.w as i32, g.h as i32);
            let at_left = lx == left || lx == left + 1;
            let at_right = lx == right + 1 || lx == right;
            if at_left && ly == top {
                *g = FGeom { x: x + 1, y: y + 1, w: (gw + lx - x).max(min) as u16, h: (gh + ly - y).max(min) as u16 };
            } else if at_right && ly == top {
                *g = FGeom { y: y + 1, w: (x - g.x).max(min) as u16, h: (gh + ly - y).max(min) as u16, ..*g };
            } else if at_left && ly == bottom {
                if y - g.y < min {
                    return;
                }
                *g = FGeom { x: x + 1, w: (gw + lx - x).max(min) as u16, h: (y - g.y) as u16, ..*g };
            } else if at_right && ly == bottom {
                *g = FGeom { w: (x - g.x).max(min) as u16, h: (y - g.y).max(min) as u16, ..*g };
            } else if lx == right {
                if x - g.x < min {
                    return;
                }
                g.w = (x - g.x) as u16;
            } else if lx == left {
                if gw + lx - x < min {
                    return;
                }
                *g = FGeom { x: x + 1, w: (gw + lx - x) as u16, ..*g };
            } else if ly == bottom {
                if y - g.y < min {
                    return;
                }
                g.h = (y - g.y) as u16;
            } else if ly == top {
                // the top border moves the pane
                *g = FGeom { x: g.x + x - lx, y: y + 1, ..*g };
            }
        }
        if *g != old {
            self.apply_layout(wid);
            self.mark_window_dirty(wid);
        }
    }

    /// select-layout with a layout string (tmux `layout_parse`).
    pub fn set_layout_string(&mut self, wid: WinId, s: &str) -> Result<(), String> {
        let w = &self.windows[&wid];
        let panes = w.panes();
        let floating = w.layout.floats();
        let a = crate::layout::apply_layout_string(s, &panes, &floating).map_err(|e| if e == "window has no panes" { format!("window @{wid} has no panes") } else { e })?;
        let w = self.windows.get_mut(&wid).unwrap();
        w.layout = a.node;
        if a.version > 1 {
            let mut z = a.zorder;
            z.extend(w.zlist().into_iter().filter(|p| !w.layout.is_float(*p)));
            w.zorder = z;
            if let Some(p) = a.active {
                if w.modal.is_none() {
                    w.set_active(p);
                }
            }
            w.last_panes = a.last;
        }
        // a pane can stop or start floating with the layout
        if w.modal.is_some_and(|m| !w.layout.is_float(m)) {
            w.modal = None;
        }
        Ok(())
    }
}

/// Tests that allocate huge panes take this, so that one measuring the process's memory
/// does not run beside another.
#[cfg(test)]
pub(crate) static HUGE_PANES: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> (Server, mpsc::Receiver<Event>) {
        let (tx, events) = mpsc::sync_channel(256);
        (Server::new(tx), events)
    }

    fn run(srv: &mut Server, args: &[&str]) -> Result<String, String> {
        let mut ctx = Ctx::default();
        match srv.exec(args.iter().map(|s| s.to_string()).collect(), &mut ctx) {
            Ok(_) => Ok(ctx.out.to_string()),
            Err(e) if e.starts_with('\0') => Ok(String::new()),
            Err(e) => Err(e),
        }
    }

    fn ok(srv: &mut Server, args: &[&str]) -> String {
        run(srv, args).unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    fn fmt(srv: &Server, pane: &str, f: &str) -> String {
        let id: PaneId = pane.trim_start_matches('%').parse().unwrap();
        srv.expand_for_pane(id, f)
    }

    fn win(srv: &Server) -> WinId {
        *srv.windows.keys().next().unwrap()
    }

    fn layout(srv: &Server) -> String {
        srv.window_layout(win(srv), false, None).unwrap()
    }

    fn panes(srv: &Server) -> String {
        let w = &srv.windows[&win(srv)];
        w.panes().iter().map(|p| srv.expand_for_pane(*p, "#{pane_id}:#{pane_flags}:#{pane_floating_flag}:#{pane_z}:#{pane_width}x#{pane_height}@#{pane_x},#{pane_y}")).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn new_pane_layout_and_formats() {
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "f", "-x", "80", "-y", "24"]);
        let p0 = ok(&mut srv, &["display", "-p", "-t", "f", "#{pane_id}"]).trim().to_string();
        let n0: PaneId = p0[1..].parse().unwrap();
        assert_eq!(fmt(&srv, &p0, "#{pane_z} #{pane_floating_flag}"), "1 0");
        let p1 = ok(&mut srv, &["new-pane", "-d", "-P", "-F", "#{pane_id}", "-t", "f"]).trim().to_string();
        let n1: PaneId = p1[1..].parse().unwrap();
        // tmux 3.8: {"V":2,"L":{"t":"v",...,"c":[{"t":"p",...,"a":true,"i":0,"I":"%0"},{"t":"p","w":40,"h":6,"x":4,"y":2,"i":1,"z":0,"I":"%1"}]}}
        assert_eq!(
            layout(&srv),
            format!(r#"{{"V":2,"L":{{"t":"v","w":80,"h":24,"x":0,"y":0,"c":[{{"t":"p","w":80,"h":24,"x":0,"y":0,"a":true,"i":0,"I":"%{n0}"}},{{"t":"p","w":40,"h":6,"x":4,"y":2,"i":1,"z":0,"I":"%{n1}"}}]}}}}"#)
        );
        assert_eq!(panes(&srv), format!("%{n0}:*:0:2:80x24@0,0 %{n1}:F:1:0:40x6@4,2"));
        // a second one cascades, goes after the target in the layout and in front
        let p2 = ok(&mut srv, &["new-pane", "-P", "-F", "#{pane_id}", "-t", &p0]).trim().to_string();
        let n2: PaneId = p2[1..].parse().unwrap();
        assert_eq!(panes(&srv), format!("%{n0}:-:0:3:80x24@0,0 %{n1}:F:1:1:40x6@4,2 %{n2}:*F:1:0:40x6@8,4"));
        assert!(layout(&srv).contains(&format!(r#""l":0,"i":0,"I":"%{n0}"}},{{"t":"p","w":40,"h":6,"x":8,"y":4,"a":true,"i":2,"z":0"#)));
        // split-window -h on the tiled pane nests under the top node, floating panes stay last
        ok(&mut srv, &["split-window", "-h", "-t", &p0]);
        let l = layout(&srv);
        assert!(l.starts_with(r#"{"V":2,"L":{"t":"v","w":80,"h":24,"x":0,"y":0,"c":[{"t":"h","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":40,"h":24,"x":0,"y":0,"l":1,"i":0,"#), "{l}");
        // the old format has the tiled panes only
        assert_eq!(&srv.windows[&win(&srv)].layout.to_layout_string(srv.window_area(win(&srv)))[5..], format!("80x24,0,0{{40x24,0,0,{n0},39x24,41,0,{}}}", n2 + 1));
        assert_eq!(srv.expand(srv.target_for_pane(n0), None, "#{window_modal_pane}|#{pane_modal_flag}"), "|0");
        srv.shutdown();
    }

    #[test]
    fn new_pane_geometry_flags_and_errors() {
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "g", "-x", "80", "-y", "24"]);
        let p = ok(&mut srv, &["new-pane", "-P", "-F", "#{pane_id}", "-x", "50%", "-y", "10", "-X", "2", "-Y", "3", "-B", "double", "-T", "hello", "-s", "bg=red", "-S", "fg=blue", "-R", "fg=green"]);
        let p = p.trim();
        // -x/-y include the border, -X/-Y place it
        assert_eq!(fmt(&srv, p, "#{pane_width}x#{pane_height} #{pane_x},#{pane_y} #{pane_title}"), "38x8 3,4 hello");
        assert_eq!(fmt(&srv, p, "#{pane-border-lines}|#{window-style}|#{pane-active-border-style}|#{pane-border-style}"), "double|bg=red|fg=blue|fg=green");
        assert_eq!(run(&mut srv, &["new-pane", "-x", "0"]).unwrap_err(), "invalid width");
        assert_eq!(run(&mut srv, &["new-pane", "-y", "abc"]).unwrap_err(), "position invalid");
        assert_eq!(run(&mut srv, &["new-pane", "-B", "bogus"]).unwrap_err(), "pane-border-lines unknown value: bogus");
        assert_eq!(run(&mut srv, &["new-pane", "-X", "200"]).unwrap_err(), "position too large");
        assert_eq!(run(&mut srv, &["new-pane", "-x", "50%", "-X", "-41"]).unwrap_err(), "position too small");
        assert_eq!(run(&mut srv, &["new-pane", "-L", "-O"]).unwrap_err(), "modal pane must be floating");
        assert_eq!(run(&mut srv, &["new-pane", "-E", "cmd"]).unwrap_err(), "command cannot be given for empty pane");
        // -L behaves like split-window
        let n = srv.windows[&win(&srv)].panes().len();
        ok(&mut srv, &["new-pane", "-L", "-d", "-t", "g:0.0"]);
        assert_eq!(srv.windows[&win(&srv)].layout.tiled().len(), 2);
        assert_eq!(srv.windows[&win(&srv)].panes().len(), n + 1);
        srv.shutdown();
    }

    #[test]
    fn modal_panes() {
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "m", "-x", "80", "-y", "24"]);
        let t = ok(&mut srv, &["display", "-p", "-t", "m", "#{pane_id}"]).trim().to_string();
        let m = ok(&mut srv, &["new-pane", "-O", "-K", "-C", "-D", "-P", "-F", "#{pane_id}"]).trim().to_string();
        let mid: PaneId = m[1..].parse().unwrap();
        assert_eq!(fmt(&srv, &m, "#{pane_flags} #{pane_modal_flag} #{window_modal_pane} #{window_flags}"), format!("*FAO 1 {m} *O"));
        assert_eq!(srv.panes[&mid].fflags, OVER_ZOOM | CAPTURE_KEYS | CLOSE_ON_CLICK | CLOSE_ON_CANCEL);
        assert_eq!(run(&mut srv, &["new-pane", "-O"]).unwrap_err(), "window already has a modal pane");
        // nothing else can become active
        ok(&mut srv, &["select-pane", "-t", &t]);
        assert_eq!(fmt(&srv, &t, "#{pane_active}"), "0");
        ok(&mut srv, &["new-pane", "-d"]);
        assert_eq!(fmt(&srv, &m, "#{pane_active}"), "1");
        assert_eq!(run(&mut srv, &["break-pane", "-s", &m]).unwrap_err(), "pane is modal");
        assert_eq!(run(&mut srv, &["swap-pane", "-s", &m, "-t", &t]).unwrap_err(), "pane is modal");
        // closing it gives the pane active before it back
        ok(&mut srv, &["kill-pane", "-t", &m]);
        assert_eq!(fmt(&srv, &t, "#{pane_active} #{window_modal_pane}"), "1 ");
        srv.shutdown();
    }

    #[test]
    fn default_floating_bindings() {
        let b = super::super::input::default_bindings();
        // C-b g enters the move table of tmux 3.8 (19 keys)
        // stored as key-bindings.c writes it; list-keys prints "switch-client -T move"
        assert_eq!(b["prefix"]["g"].cmd, "switch-client -Tmove");
        assert_eq!(b["move"].len(), 19);
        assert_eq!(b["move"]["M-Right"].note, "Move pane to right and resize");
        assert_eq!(b["prefix"]["*"].cmd, "new-pane");
        assert!(b["prefix"]["M-Up"].repeat);
        for k in ["C-MouseDrag1Pane", "C-MouseDrag1Empty", "M-MouseDrag1Pane", "M-MouseDrag1Border", "MouseDown1Control7", "MouseDown1Control8", "MouseDown1Control9", "MouseDown3Empty", "MouseDown1Border"] {
            assert!(b["root"].contains_key(k), "{k}");
        }
        for (table, keys) in &b {
            for (k, bd) in keys {
                assert!(crate::cmdparse::parse(&bd.cmd).is_ok(), "{table} {k}: {}", bd.cmd);
            }
        }
        // the move menus run move-pane and resize-pane in the active floating pane
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "m", "-x", "80", "-y", "24"]);
        let f = ok(&mut srv, &["new-pane", "-P", "-F", "#{pane_id}", "-x", "20", "-y", "6"]).trim().to_string();
        ok(&mut srv, &["resize-pane", "-x", "50%", "-y", "50%"]);
        ok(&mut srv, &["move-pane", "-P", "bottom-right"]);
        assert_eq!(fmt(&srv, &f, "#{pane_width}x#{pane_height} #{pane_x},#{pane_y}"), "38x10 41,13");
        ok(&mut srv, &["resize-pane", "-x", "100%", "-y", "100%"]);
        ok(&mut srv, &["move-pane", "-P", "top-left"]);
        assert_eq!(fmt(&srv, &f, "#{pane_width}x#{pane_height} #{pane_x},#{pane_y}"), "78x22 1,1");
        srv.shutdown();
    }

    #[test]
    fn zoomed_floating_pane_z_order() {
        // tmux 3.8: a zoomed floating pane goes back in front when it is active as the window
        // is unzoomed (and the saved layout already shows it there), else behind the others
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "z", "-x", "80", "-y", "24"]);
        let a = ok(&mut srv, &["display", "-p", "-t", "z", "#{pane_id}"]).trim().to_string();
        let b = ok(&mut srv, &["new-pane", "-P", "-F", "#{pane_id}"]).trim().to_string();
        let c = ok(&mut srv, &["new-pane", "-d", "-P", "-F", "#{pane_id}"]).trim().to_string();
        assert_eq!((fmt(&srv, &b, "#{pane_z}"), fmt(&srv, &c, "#{pane_z}")), ("1".into(), "0".into()));
        ok(&mut srv, &["resize-pane", "-Z", "-t", &b]);
        assert!(layout(&srv).contains(&format!(r#""a":true,"i":1,"z":0,"I":"{b}""#)), "{}", layout(&srv));
        ok(&mut srv, &["select-pane", "-t", &a]);
        assert_eq!((fmt(&srv, &b, "#{pane_z}"), fmt(&srv, &c, "#{pane_z}")), ("0".into(), "1".into()));
        // unzoomed while another pane is active, it goes behind the floating panes
        ok(&mut srv, &["resize-pane", "-Z", "-t", &b]);
        ok(&mut srv, &["select-pane", "-t", &b]);
        let d = ok(&mut srv, &["new-pane", "-A", "-P", "-F", "#{pane_id}"]).trim().to_string();
        assert_eq!(fmt(&srv, &d, "#{window_zoomed_flag}"), "1");
        ok(&mut srv, &["resize-pane", "-Z"]);
        assert_eq!(panes(&srv).split(' ').map(|p| p.split(':').nth(3).unwrap()).collect::<Vec<_>>().join(","), "4,2,1,0");
        srv.shutdown();
    }

    #[test]
    fn zoom_and_floating() {
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "z", "-x", "80", "-y", "24"]);
        let t = ok(&mut srv, &["display", "-p", "-t", "z", "#{pane_id}"]).trim().to_string();
        ok(&mut srv, &["split-window", "-t", &t]);
        ok(&mut srv, &["resize-pane", "-Z"]);
        let wid = win(&srv);
        assert!(srv.windows[&wid].zoomed);
        // a plain floating pane unzooms
        let f = ok(&mut srv, &["new-pane", "-P", "-F", "#{pane_id}"]).trim().to_string();
        assert!(!srv.windows[&wid].zoomed);
        ok(&mut srv, &["kill-pane", "-t", &f]);
        // -A stays above the zoom: the window is zoomed again with the floating pane active
        ok(&mut srv, &["resize-pane", "-Z"]);
        let zoomed = srv.zoomed_pane(wid).unwrap();
        let a = ok(&mut srv, &["new-pane", "-A", "-P", "-F", "#{pane_id}"]).trim().to_string();
        assert!(srv.windows[&wid].zoomed);
        assert_eq!(srv.zoomed_pane(wid), Some(zoomed));
        assert_eq!(fmt(&srv, &a, "#{pane_flags} #{window_zoomed_flag}"), "*FA 1");
        assert_eq!(srv.float_rects(wid).len(), 1);
        let vis = srv.window_layout(wid, true, None).unwrap();
        assert!(vis.starts_with(&format!(r#"{{"V":2,"L":{{"t":"v","w":80,"h":24,"x":0,"y":0,"c":[{{"t":"p","w":80,"h":24,"x":0,"y":0,"l":0"#)), "{vis}");
        // killing it keeps the zoom
        ok(&mut srv, &["kill-pane", "-t", &a]);
        assert!(srv.windows[&wid].zoomed);
        srv.shutdown();
    }

    #[test]
    fn float_tile_swap_and_move() {
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "b", "-x", "80", "-y", "24"]);
        let a = ok(&mut srv, &["display", "-p", "-t", "b", "#{pane_id}"]).trim().to_string();
        let b = ok(&mut srv, &["split-window", "-h", "-P", "-F", "#{pane_id}", "-t", &a]).trim().to_string();
        let c = ok(&mut srv, &["split-window", "-h", "-P", "-F", "#{pane_id}", "-t", &b]).trim().to_string();
        assert_eq!(fmt(&srv, &a, "#{pane_width}"), "40");
        assert_eq!(fmt(&srv, &b, "#{pane_width} #{pane_x}"), "19 41");
        // float the middle pane: its space goes to the next pane
        ok(&mut srv, &["break-pane", "-W", "-s", &b, "-x", "20", "-y", "6", "-X", "10", "-Y", "5"]);
        assert_eq!(fmt(&srv, &b, "#{pane_floating_flag} #{pane_width}x#{pane_height} #{pane_x},#{pane_y} #{pane_active}"), "1 18x4 11,6 1");
        assert_eq!(fmt(&srv, &c, "#{pane_width} #{pane_x}"), "39 41");
        assert_eq!(run(&mut srv, &["break-pane", "-W", "-s", &b]).unwrap_err(), "pane is already floating");
        // move it
        ok(&mut srv, &["move-pane", "-t", &b, "-R", "3", "-D"]);
        assert_eq!(fmt(&srv, &b, "#{pane_x},#{pane_y}"), "14,7");
        ok(&mut srv, &["move-pane", "-t", &b, "-P", "bottom-right"]);
        assert_eq!(fmt(&srv, &b, "#{pane_x},#{pane_y}"), "61,19");
        ok(&mut srv, &["move-pane", "-t", &b, "-X", "0", "-Y", "0"]);
        assert_eq!(fmt(&srv, &b, "#{pane_x},#{pane_y}"), "1,1");
        assert_eq!(run(&mut srv, &["move-pane", "-t", &a, "-U"]).unwrap_err(), "pane is not floating");
        assert_eq!(run(&mut srv, &["move-pane", "-t", &b, "-P", "nowhere"]).unwrap_err(), "unknown position: nowhere");
        // resize it: -x/-y include the border, -L moves the left border
        ok(&mut srv, &["resize-pane", "-t", &b, "-x", "30", "-y", "10"]);
        assert_eq!(fmt(&srv, &b, "#{pane_width}x#{pane_height}"), "28x8");
        ok(&mut srv, &["resize-pane", "-t", &b, "-L", "2"]);
        ok(&mut srv, &["move-pane", "-t", &b, "-X", "5"]);
        assert_eq!(fmt(&srv, &b, "#{pane_width} #{pane_x}"), "30 6");
        ok(&mut srv, &["resize-pane", "-t", &b, "-R-5"]);
        assert_eq!(fmt(&srv, &b, "#{pane_width}"), "25");
        // join-pane tiles it again, taking half its neighbour
        ok(&mut srv, &["join-pane", "-s", &b, "-t", &b]);
        assert_eq!(fmt(&srv, &b, "#{pane_floating_flag} #{pane_width} #{pane_x}"), "0 19 41");
        assert_eq!(fmt(&srv, &c, "#{pane_width} #{pane_x}"), "19 61");
        assert_eq!(run(&mut srv, &["join-pane", "-s", &b, "-t", &b]).unwrap_err(), "source and target panes must be different");
        // floating again restores the last floating size and position
        ok(&mut srv, &["break-pane", "-W", "-d", "-s", &b]);
        assert_eq!(fmt(&srv, &b, "#{pane_width}x#{pane_height} #{pane_x},#{pane_y} #{pane_active}"), "25x8 6,1 1");
        // swap-pane exchanges cells: the tiled pane floats where the floating one was
        ok(&mut srv, &["swap-pane", "-d", "-s", &b, "-t", &a]);
        assert_eq!(fmt(&srv, &a, "#{pane_floating_flag} #{pane_width}x#{pane_height} #{pane_x},#{pane_y}"), "1 25x8 6,1");
        assert_eq!(fmt(&srv, &b, "#{pane_floating_flag} #{pane_width} #{pane_x}"), "0 40 0");
        // z-order
        let d = ok(&mut srv, &["new-pane", "-d", "-P", "-F", "#{pane_id}"]).trim().to_string();
        assert_eq!(fmt(&srv, &d, "#{pane_z}"), "0");
        ok(&mut srv, &["move-pane", "-t", &d, "-P", "back"]);
        assert_eq!(format!("{} {}", fmt(&srv, &d, "#{pane_z}"), fmt(&srv, &a, "#{pane_z}")), "1 0");
        ok(&mut srv, &["move-pane", "-t", &d, "-z", "0"]);
        assert_eq!(format!("{} {}", fmt(&srv, &d, "#{pane_z}"), fmt(&srv, &a, "#{pane_z}")), "0 1");
        srv.shutdown();
    }

    #[test]
    fn split_in_floating_pane() {
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "s", "-x", "80", "-y", "24"]);
        let f = ok(&mut srv, &["new-pane", "-P", "-F", "#{pane_id}", "-x", "22", "-y", "8", "-X", "10", "-Y", "5"]).trim().to_string();
        let n = ok(&mut srv, &["split-window", "-h", "-P", "-F", "#{pane_id}", "-t", &f]).trim().to_string();
        assert_eq!(fmt(&srv, &n, "#{pane_floating_flag} #{pane_width}x#{pane_height} #{pane_x},#{pane_y}"), "1 20x6 33,6");
        assert_eq!(fmt(&srv, &f, "#{pane_width}x#{pane_height} #{pane_x},#{pane_y} #{pane_flags}"), "20x6 11,6 -F");
        let v = ok(&mut srv, &["split-window", "-b", "-P", "-F", "#{pane_id}", "-t", &f]).trim().to_string();
        // not enough room above: the space is shared
        assert_eq!(fmt(&srv, &v, "#{pane_height} #{pane_y}"), "4 2");
        assert_eq!(fmt(&srv, &f, "#{pane_height} #{pane_y}"), "4 8");
        // -f takes all the room it can
        let e = ok(&mut srv, &["split-window", "-f", "-P", "-F", "#{pane_id}", "-t", &n]).trim().to_string();
        assert_eq!(fmt(&srv, &e, "#{pane_width}x#{pane_height} #{pane_x},#{pane_y} #{pane_z} #{pane_index}"), "72x8 4,14 0 4");
        // floating cells follow the cell they were split from (tmux 3.8 output)
        let ids: Vec<String> = [&f, &v, &n, &e].iter().map(|p| p.to_string()).collect();
        let mut want = r#"{"V":2,"L":{"t":"v","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":80,"h":24,"x":0,"y":0,"l":3,"i":0,"I":"A"},{"t":"p","w":20,"h":4,"x":11,"y":8,"l":2,"i":2,"z":3,"I":"B"},{"t":"p","w":20,"h":4,"x":11,"y":2,"l":0,"i":1,"z":1,"I":"D"},{"t":"p","w":20,"h":6,"x":33,"y":6,"l":1,"i":3,"z":2,"I":"C"},{"t":"p","w":72,"h":8,"x":4,"y":14,"a":true,"i":4,"z":0,"I":"E"}]}}"#.to_string();
        let a = format!("%{}", srv.windows[&win(&srv)].layout.tiled()[0]);
        for (k, id) in ["\"A\"", "\"B\"", "\"D\"", "\"C\"", "\"E\""].iter().zip([&a, &ids[0], &ids[1], &ids[2], &ids[3]]) {
            want = want.replace(k, &format!("\"{id}\""));
        }
        assert_eq!(layout(&srv), want);
        srv.shutdown();
    }

    #[test]
    fn json_layouts_round_trip() {
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "j", "-x", "80", "-y", "24"]);
        ok(&mut srv, &["split-window", "-h"]);
        ok(&mut srv, &["new-pane", "-d"]);
        let before = layout(&srv);
        ok(&mut srv, &["select-layout", "even-vertical"]);
        assert_ne!(layout(&srv), before);
        assert_eq!(srv.windows[&win(&srv)].layout.floats().len(), 1);
        ok(&mut srv, &["select-layout", &before]);
        assert_eq!(layout(&srv), before);
        // errors are reported with the layout
        let bad = r#"{"V":2,"L":{"t":"q","w":80,"h":24,"x":0,"y":0,"i":0}}"#;
        assert_eq!(run(&mut srv, &["select-layout", bad]).unwrap_err(), format!("unknown cell type \"q\": {bad}"));
        let short = r#"{"V":2,"L":{"t":"p","w":80,"h":24,"x":0,"y":0,"i":0}}"#;
        assert_eq!(run(&mut srv, &["select-layout", short]).unwrap_err(), format!("have 3 panes but need 1: {short}"));
        // an old-format layout keeps the floating panes as they are
        let old = srv.windows[&win(&srv)].layout.to_layout_string(srv.window_area(win(&srv)));
        ok(&mut srv, &["select-layout", "tiled"]);
        ok(&mut srv, &["select-layout", &old]);
        assert_eq!(srv.windows[&win(&srv)].layout.floats().len(), 1);
        assert_eq!(srv.windows[&win(&srv)].layout.to_layout_string(srv.window_area(win(&srv))), old);
        srv.shutdown();
    }

    #[test]
    fn new_pane_wait_and_exit_status() {
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "w", "-x", "80", "-y", "24"]);
        let mut ctx = Ctx::default();
        let r = srv.exec(["new-pane", "-W", "-E", "-P", "-F", "#{pane_id}"].iter().map(|s| s.to_string()).collect(), &mut ctx);
        let e = r.err().unwrap();
        assert!(e.starts_with("\0pane-wait\0"));
        let id: PaneId = e.rsplit('\0').next().unwrap().parse().unwrap();
        srv.pane_waits.entry(id).or_default().push((vec!["new-pane".into()], Queue { cmds: VecDeque::new(), ctx: Ctx::default(), soft: 0 }));
        srv.finish_pane_waits(id, Some(3));
        assert!(srv.pane_waits.is_empty());
        // after-split-window runs when the -W item continues, not before
        ok(&mut srv, &["set-hook", "-g", "after-split-window", "set -ga @c S"]);
        let r = srv.exec(["new-pane", "-W", "-E", "-P", "-F", "#{pane_id}"].iter().map(|s| s.to_string()).collect(), &mut Ctx::default());
        let id: PaneId = r.err().unwrap().rsplit('\0').next().unwrap().parse().unwrap();
        assert_eq!(ok(&mut srv, &["show", "-gqv", "@c"]), "");
        srv.pane_waits.entry(id).or_default().push((vec!["new-pane".into()], Queue { cmds: VecDeque::new(), ctx: Ctx::default(), soft: 0 }));
        srv.finish_pane_waits(id, Some(0));
        assert_eq!(ok(&mut srv, &["show", "-gqv", "@c"]), "S\n");
        // WEXITSTATUS; a failing Windows status never looks like success
        assert_eq!((wait_status(0), wait_status(7), wait_status(255)), (0, 7, 255));
        assert_eq!(wait_status(0xC000013Au32 as i32), 0x3A);
        assert_eq!((wait_status(256), wait_status(-1)), (1, 255));
        srv.shutdown();
    }

    #[test]
    fn resize_float_like_tmux() {
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "r", "-x", "80", "-y", "24"]);
        let g = "#{pane_left},#{pane_top} #{pane_width}x#{pane_height}";
        ok(&mut srv, &["set", "-g", "pane-border-status", "top"]);
        let p = ok(&mut srv, &["new-pane", "-P", "-F", "#{pane_id}", "-x", "30", "-y", "10", "-X", "20", "-Y", "0"]).trim().to_string();
        // -y counts the top status line of a pane at row 1
        ok(&mut srv, &["resize-pane", "-t", &p, "-y", "10"]);
        assert_eq!(fmt(&srv, &p, g), "21,1 28x9");
        ok(&mut srv, &["set", "-g", "pane-border-status", "bottom"]);
        ok(&mut srv, &["resize-pane", "-t", &p, "-y", "10"]);
        ok(&mut srv, &["move-pane", "-t", &p, "-X", "20", "-Y", "14"]);
        assert_eq!(fmt(&srv, &p, g), "21,15 28x8");
        // ... and the bottom status line of a pane ending at the last row but one
        ok(&mut srv, &["resize-pane", "-t", &p, "-y", "10"]);
        assert_eq!(fmt(&srv, &p, g), "21,15 28x9");
        ok(&mut srv, &["resize-pane", "-t", &p, "-y", "10"]);
        assert_eq!(fmt(&srv, &p, g), "21,15 28x8");
        // top-floating is not a window pane status
        ok(&mut srv, &["set", "-g", "pane-border-status", "top-floating"]);
        ok(&mut srv, &["move-pane", "-t", &p, "-X", "20", "-Y", "0"]);
        ok(&mut srv, &["resize-pane", "-t", &p, "-y", "10"]);
        assert_eq!(fmt(&srv, &p, g), "21,1 28x8");
        // -U makes every later flag move the offset as well
        ok(&mut srv, &["set", "-g", "pane-border-status", "off"]);
        ok(&mut srv, &["move-pane", "-t", &p, "-X", "44", "-Y", "13"]);
        ok(&mut srv, &["resize-pane", "-t", &p, "-U", "3", "-D", "2"]);
        assert_eq!(fmt(&srv, &p, g), "45,9 28x13");
        ok(&mut srv, &["resize-pane", "-t", &p, "-L", "3", "-R", "2"]);
        assert_eq!(fmt(&srv, &p, g), "40,9 33x13");
        ok(&mut srv, &["resize-pane", "-t", &p, "-D", "2", "-U", "1"]);
        assert_eq!(fmt(&srv, &p, g), "40,6 33x16");
        srv.shutdown();
    }

    #[test]
    fn join_pane_selects_the_destination_window() {
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "j", "-x", "80", "-y", "24"]);
        ok(&mut srv, &["new-window", "-t", "j"]);
        ok(&mut srv, &["new-window", "-t", "j"]);
        ok(&mut srv, &["new-window", "-t", "j"]);
        let cur = |srv: &mut Server| ok(srv, &["display", "-p", "-t", "j", "#{window_index}"]);
        assert_eq!(cur(&mut srv), "3\n");
        ok(&mut srv, &["join-pane", "-d", "-s", "j:1.0", "-t", "j:0.0"]);
        assert_eq!(cur(&mut srv), "3\n");
        ok(&mut srv, &["join-pane", "-s", "j:2.0", "-t", "j:0.0"]);
        assert_eq!(cur(&mut srv), "0\n");
        srv.shutdown();
    }

    #[test]
    fn pane_status_width_is_the_visible_part() {
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "s", "-x", "40", "-y", "10"]);
        ok(&mut srv, &["set", "-g", "pane-border-status", "top"]);
        let wid = win(&srv);
        let tiled = srv.windows[&wid].active;
        let p: PaneId = ok(&mut srv, &["new-pane", "-P", "-F", "#{pane_id}", "-x", "16", "-y", "4", "-X", "30", "-Y", "4"]).trim().trim_start_matches('%').parse().unwrap();
        // the floating pane's right border is clamped to the last column
        assert_eq!(srv.pane_status_width(wid, p, (33, 4), 12, (40, 10)), 6);
        assert_eq!(srv.pane_status_width(wid, p, (33, 4), 12, (36, 10)), 3);
        assert_eq!(srv.pane_status_width(wid, p, (33, 4), 12, (40, 4)), 0);
        // the tiled status ends left of the floating pane covering its right end, but a
        // floating pane in the middle leaves the end visible
        assert_eq!(srv.pane_status_width(wid, tiled, (2, 0), 38, (40, 10)), 38);
        ok(&mut srv, &["move-pane", "-t", &format!("%{p}"), "-X", "30", "-Y", "0"]);
        assert_eq!(srv.pane_status_width(wid, tiled, (2, 0), 38, (40, 10)), 28);
        ok(&mut srv, &["move-pane", "-t", &format!("%{p}"), "-X", "10", "-Y", "0"]);
        assert_eq!(srv.pane_status_width(wid, tiled, (2, 0), 38, (40, 10)), 38);
        srv.shutdown();
    }

    /// tmux keeps a 9998x9998 floating pane at a few hundred KB (its grid lines grow
    /// lazily): so must the pane's screen, or a couple of panes take gigabytes.
    #[test]
    #[cfg(target_os = "linux")]
    fn huge_floating_panes_stay_small() {
        let _huge = HUGE_PANES.lock().unwrap_or_else(|e| e.into_inner());
        let rss = || {
            let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
            statm.split_whitespace().nth(1).unwrap().parse::<u64>().unwrap() * 4096
        };
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "h", "-x", "80", "-y", "24"]);
        let before = rss();
        for _ in 0..2 {
            let p = ok(&mut srv, &["new-pane", "-E", "-d", "-P", "-F", "#{pane_id}", "-x", "10000", "-y", "10000"]).trim().to_string();
            assert_eq!(fmt(&srv, &p, "#{pane_width}x#{pane_height}"), "9998x9998");
            let id: PaneId = p.trim_start_matches('%').parse().unwrap();
            let pane = srv.panes.get_mut(&id).unwrap();
            for i in 0..12000 {
                pane.parser.process(format!("{i}\r\n").as_bytes());
            }
            pane.parser.process(b"\x1b[2J\x1b[H\x1b[3@x");
        }
        let grown = rss().saturating_sub(before);
        assert!(grown < 256 << 20, "{} MB", grown >> 20);
        srv.shutdown();
    }

    #[test]
    fn clamp_floats_off_the_top_left() {
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "c", "-x", "80", "-y", "24"]);
        let g = "#{pane_left},#{pane_top} #{pane_width}x#{pane_height}";
        let p = ok(&mut srv, &["new-pane", "-P", "-F", "#{pane_id}", "-x", "20", "-y", "5", "-X", "-5", "-Y", "-2"]).trim().to_string();
        assert_eq!(fmt(&srv, &p, g), "-4,-1 18x3");
        ok(&mut srv, &["resize-window", "-x", "20", "-y", "6"]);
        assert_eq!(fmt(&srv, &p, g), "-4,-1 18x3");
        // xoff + sx + pad < 0 wraps round in tmux and pulls the pane back to the padding
        ok(&mut srv, &["resize-window", "-x", "3", "-y", "3"]);
        assert_eq!(fmt(&srv, &p, g), "1,-1 1x1");
        ok(&mut srv, &["resize-window", "-x", "80", "-y", "24"]);
        assert_eq!(fmt(&srv, &p, g), "1,-1 1x1");
        srv.shutdown();
    }

    #[test]
    fn pane_order_rotate_and_activity() {
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "o", "-x", "80", "-y", "40"]);
        let cmd = |srv: &Server, p: &PaneId| srv.panes[p].title.clone();
        let titles = |srv: &Server| srv.windows[&win(srv)].panes().iter().map(|p| cmd(srv, p)).collect::<Vec<_>>().join(" ");
        ok(&mut srv, &["select-pane", "-T", "a"]);
        // tmux window_add_pane: after the target, -f at the tail, -f -b at the head, a
        // floating pane at the tail and -b before the target
        for (args, t) in [(&["split-window", "-d"][..], "b"), (&["split-window", "-d", "-f"][..], "c"), (&["split-window", "-d", "-f", "-b"][..], "d"), (&["new-pane", "-d"][..], "e"), (&["new-pane", "-d", "-b"][..], "f")] {
            let mut a = args.to_vec();
            a.extend(["-P", "-F", "#{pane_id}"]);
            let id = ok(&mut srv, &a).trim().to_string();
            ok(&mut srv, &["select-pane", "-t", &id, "-T", t]);
        }
        assert_eq!(titles(&srv), "d f a b c e");
        // list-panes -O activity: never-active panes first, then in the order they were last
        // made active (select-pane -T alone does not select)
        ok(&mut srv, &["select-pane", "-t", "o.4"]);
        ok(&mut srv, &["select-pane", "-t", "o.0"]);
        ok(&mut srv, &["select-pane", "-t", "o.2"]);
        let order = ok(&mut srv, &["list-panes", "-t", "o", "-O", "activity", "-F", "#{pane_title}"]);
        assert_eq!(order.split_whitespace().collect::<Vec<_>>(), ["b", "e", "f", "c", "d", "a"]);
        // rotate-window: the pane taking the active pane's place becomes active, without
        // being raised (tmux window_set_active_pane)
        // (f takes d's tiled cell)
        ok(&mut srv, &["select-pane", "-t", "o.0"]);
        ok(&mut srv, &["rotate-window", "-t", "o"]);
        assert_eq!(ok(&mut srv, &["display", "-p", "-t", "o", "#{pane_index} #{pane_title} #{pane_floating_flag}"]).trim(), "0 f 0");
        ok(&mut srv, &["rotate-window", "-D", "-t", "o"]);
        assert_eq!(ok(&mut srv, &["display", "-p", "-t", "o", "#{pane_index} #{pane_title}"]).trim(), "0 d");
        srv.shutdown();
    }

    #[test]
    fn kept_dead_pane_and_message() {
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "k", "-x", "80", "-y", "24"]);
        let id: PaneId = ok(&mut srv, &["new-pane", "-d", "-m", "bye #{pane_dead_status}", "-y", "8", "-P", "-F", "#{pane_id}", "-E"]).trim().trim_start_matches('%').parse().unwrap();
        srv.pane_exited(id, Some(3));
        assert!(srv.panes[&id].dead.is_some());
        // the message is on the last line after a line feed, and the cursor is hidden
        let screen = srv.panes[&id].screen();
        assert_eq!(screen.contents().lines().last().unwrap_or(""), "bye 3");
        assert!(screen.hide_cursor());
        // failed-key keeps a pane only when its command failed
        let id2: PaneId = ok(&mut srv, &["new-pane", "-d", "-P", "-F", "#{pane_id}", "-E"]).trim().trim_start_matches('%').parse().unwrap();
        srv.panes.get_mut(&id2).unwrap().options.set("remain-on-exit", "failed-key".into());
        srv.pane_exited(id2, Some(0));
        assert!(!srv.panes.contains_key(&id2));
        srv.shutdown();
    }

    #[test]
    fn display_panes_mode_draws_floating_panes() {
        let (mut srv, _ev) = server();
        ok(&mut srv, &["new-session", "-d", "-s", "p", "-x", "50", "-y", "14"]);
        ok(&mut srv, &["split-window", "-h"]);
        ok(&mut srv, &["new-pane", "-x", "20", "-y", "6", "-X", "5", "-Y", "3"]);
        ok(&mut srv, &["display-panes", "-d", "0"]);
        let wid = win(&srv);
        let a = srv.windows[&wid].active;
        let Some(PaneMode::Panes(m)) = &srv.panes[&a].mode else { panic!("no panes mode") };
        let lines = m.render_rows(&srv, 50, 14, 0..14);
        let row = |y: usize| -> String { lines[y].cells.iter().map(|c| c.text.as_str()).collect() };
        // window_panes_draw_screen: the tiled panes with their display-panes-format...
        assert_eq!(row(0), "                    25x14│                   24x14");
        // ...then the floating pane on a cleared area inside its own border, with its number
        // (as tmux 3.8 draws the same window: border from column 5, row 3)
        assert_eq!(&row(3)[..row(3).char_indices().nth(26).unwrap().0], "     ┌──────────────────┐│");
        assert!(row(4).starts_with("     │") && row(4).contains("18x4││"), "{}", row(4));
        assert_eq!(&row(8)[..row(8).char_indices().nth(26).unwrap().0], "     └──────────────────┘│");
    }

}
