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

/// tmux strtonum.
fn strtonum(v: &str, lo: i64, hi: i64) -> Result<i64, String> {
    let n: i64 = v.trim().parse().map_err(|_| "invalid".to_string())?;
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

    /// tmux `window_set_active_pane` (with `window_redraw_active_switch` raising a floating
    /// pane to the front): nothing but the modal pane can become active while there is one,
    /// and a zoomed window is unzoomed unless the pane is shown above the zoom.
    pub fn set_active_pane(&mut self, wid: WinId, pane: PaneId) -> bool {
        let Some(w) = self.windows.get(&wid) else { return false };
        if w.modal.is_some_and(|m| m != pane) {
            return false;
        }
        if w.is_floating(pane) {
            self.raise(wid, pane);
        }
        let w = &self.windows[&wid];
        if pane == w.active {
            return false;
        }
        let zoomed = self.zoomed_pane(wid);
        let keep_zoom = zoomed.is_some() && self.pane_visible(wid, pane);
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

    pub fn unzoom(&mut self, wid: WinId) -> bool {
        let Some(w) = self.windows.get_mut(&wid) else { return false };
        if !w.zoomed {
            return false;
        }
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
        let Some(queues) = self.pane_waits.remove(&id) else { return };
        let code = status.map_or(129, |s| s.clamp(0, 255) as u8);
        for mut q in queues {
            q.ctx.retval = Some(code);
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
            if g.x + g.w as i32 + pad > sx {
                g.x = if g.w as i32 + 2 * pad >= sx { pad } else { sx - g.w as i32 - pad };
            }
            if g.y + g.h as i32 + pad > sy {
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
        let z = w.zlist();
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
                self.unzoom(wid);
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
            self.unzoom(wid);
            let cwd = cwd.or_else(|| self.panes.get(&target).map(|p| p.current_path.clone()));
            let new = match self.spawn_pane(tt.sid, g.w, g.h, &command, cwd.as_deref(), env) {
                Ok(n) => n,
                Err(e) => {
                    if restore_zoom {
                        self.rezoom(wid, was_zoomed);
                    }
                    return Err(e);
                }
            };
            let w = self.windows.get_mut(&wid).unwrap();
            w.layout.insert_float(Some(target), new, g);
            if o.has('b') {
                w.insert_pane(target, new, true);
            } else {
                let mut order = w.panes();
                order.retain(|p| *p != new);
                order.push(new);
                w.pane_order = order;
            }
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
                self.unzoom(wid);
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
            if let Some(t) = title {
                p.title = t;
            }
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
            self.rezoom(wid, was_zoomed);
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
        self.run_hook(tt.sid, "window-layout-changed", Ctx::default());
        if o.has('P') {
            let fmt = o.get('F').unwrap_or("#{session_name}:#{window_index}.#{pane_index}");
            let line = self.expand_for_pane(new, fmt);
            ctx.out.push_str(&line);
            ctx.out.push('\n');
        }
        ctx.pane = Some(new);
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
        self.run_hook(t.sid, "window-layout-changed", Ctx::default());
        Ok(Flow::Next)
    }

    /// join-pane of a floating pane onto itself (tmux `cmd_join_pane_tile`): tile it again.
    pub fn cmd_tile_pane(&mut self, o: &Opts, wid: WinId, pane: PaneId, t: Target) -> Result<Flow, String> {
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
        self.run_hook(t.sid, "window-layout-changed", Ctx::default());
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
        self.run_hook(t.sid, "window-layout-changed", Ctx::default());
        Ok(Flow::Next)
    }

    /// resize-pane on a floating pane: -x/-y set the size with the border lines (tmux
    /// `layout_resize_floating_pane_to`), -U/-D/-L/-R move a border (`_floating_pane`).
    pub fn cmd_resize_float(&mut self, o: &Opts, wid: WinId, pane: PaneId, t: Target) -> Result<Flow, String> {
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
            let y = percentage(v, 0, PANE_MAX, wsy).map_err(|e| format!("height {e}"))?;
            let y = to(y)?;
            if let Some(g) = self.windows.get_mut(&wid).unwrap().layout.geom_mut(pane) {
                g.h = y;
            }
        }
        for c in ['U', 'D', 'L', 'R'] {
            let Some(v) = o.get(c) else { continue };
            let v = if !v.is_empty() { v.to_string() } else { o.rest.first().cloned().unwrap_or_else(|| "1".into()) };
            let n = strtonum(&v, i32::MIN as i64, i32::MAX as i64).map_err(|e| format!("adjustment {e}"))? as i32;
            if n == 0 {
                continue;
            }
            let g = self.windows.get_mut(&wid).unwrap().layout.geom_mut(pane).unwrap();
            let (size, opposite) = match c {
                'U' | 'D' => (g.h as i32 + n, c == 'U'),
                _ => (g.w as i32 + n, c == 'L'),
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
        self.run_hook(t.sid, "window-layout-changed", Ctx::default());
        Ok(Flow::Next)
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
        srv.pane_waits.entry(id).or_default().push(Queue { cmds: VecDeque::new(), ctx: Ctx::default(), soft: 0 });
        srv.finish_pane_waits(id, Some(3));
        assert!(srv.pane_waits.is_empty());
        srv.shutdown();
    }
}
