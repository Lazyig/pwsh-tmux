//! Panes mode (tmux 3.8 window-panes.c): display-panes as a pane mode. The pane shows a
//! (scaled) preview of a window's layout with a big number on each pane; a number key or a
//! click runs the template for that pane.

use super::{expand_as, format_cells, mode_style, FmtType, ModeAction, FONT};
use crate::cells::{Cell, Color, Line, Style};
use crate::layout::Rect;
use crate::server::vars::Target;
use crate::server::*;

pub const DEFAULT_TEMPLATE: &str = "select-pane -t '%%'";

pub struct Panes {
    pub owner: PaneId,
    pub session: Option<SessId>,
    /// The window whose panes are shown (-s, else the window of the pane).
    pub source: WinId,
    pub template: String,
    /// When the mode closes by itself (display-panes-time or -d); None stays until a key.
    pub until: Option<Instant>,
    /// -N: keys do not choose a pane.
    pub ignore_keys: bool,
    /// Zoomed unless -Z: Some(was already zoomed).
    pub zoomed: Option<bool>,
    pub kill: bool,
    pub client: Option<ClientId>,
}

const BORDER_L: u8 = 1;
const BORDER_R: u8 = 2;
const BORDER_U: u8 = 4;
const BORDER_D: u8 = 8;

/// The area of each pane in the mode's screen, for clicks: (pane, x, y, w, h).
type Area = (PaneId, u16, u16, u16, u16);

fn map(v: u16, o: u16, d: u16) -> u16 {
    if o <= d { v } else { (v as u32 * d as u32 / o as u32) as u16 }
}

fn border_char(mask: u8) -> &'static str {
    const L: u8 = BORDER_L;
    const R: u8 = BORDER_R;
    const U: u8 = BORDER_U;
    const D: u8 = BORDER_D;
    match mask {
        m if m == L | R | U | D => "┼",
        m if m == L | R | U => "┴",
        m if m == L | R | D => "┬",
        m if m == L | R || m == L || m == R => "─",
        m if m == L | U | D => "┤",
        m if m == L | U => "┘",
        m if m == L | D => "┐",
        m if m == R | U | D => "├",
        m if m == R | U => "└",
        m if m == R | D => "┌",
        m if m == U | D || m == U || m == D => "│",
        _ => " ",
    }
}

struct BorderMap {
    w: usize,
    h: usize,
    m: Vec<u8>,
}

impl BorderMap {
    fn mark(&mut self, x: i32, y: i32, mask: u8) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            self.m[y as usize * self.w + x as usize] |= mask;
        }
    }
    fn get(&self, x: i32, y: i32) -> u8 {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h { self.m[y as usize * self.w + x as usize] } else { 0 }
    }
    fn vline(&mut self, x: i32, y: i32, y2: i32) {
        if x < 0 || x as usize >= self.w || y2 <= y {
            return;
        }
        let (y, y2) = (y.max(0), y2.min(self.h as i32));
        for yy in y..y2 {
            let mut mask = 0;
            if yy > y {
                mask |= BORDER_U;
            }
            if yy + 1 < y2 {
                mask |= BORDER_D;
            }
            self.mark(x, yy, if mask == 0 { BORDER_U | BORDER_D } else { mask });
        }
    }
    fn hline(&mut self, x: i32, x2: i32, y: i32) {
        if y < 0 || y as usize >= self.h || x2 <= x {
            return;
        }
        let (x, x2) = (x.max(0), x2.min(self.w as i32));
        for xx in x..x2 {
            let mut mask = 0;
            if xx > x {
                mask |= BORDER_L;
            }
            if xx + 1 < x2 {
                mask |= BORDER_R;
            }
            self.mark(xx, y, if mask == 0 { BORDER_L | BORDER_R } else { mask });
        }
    }
}

impl Panes {
    fn session_of(&self, srv: &Server) -> Option<SessId> {
        // the session given if it still has the window, else any session that does
        self.session
            .filter(|s| srv.sessions.get(s).is_some_and(|s| s.links.values().any(|w| *w == self.source)))
            .or_else(|| srv.links_of(self.source).first().map(|l| l.0))
    }

    /// The unzoomed layout of the source window: pane rects and separators.
    fn layout(&self, srv: &Server) -> Option<(Vec<(PaneId, Rect)>, Vec<crate::layout::Sep>, u16, u16)> {
        let w = srv.windows.get(&self.source)?;
        let area = Rect { x: 0, y: 0, w: w.sx.max(1), h: w.sy.max(1) };
        let (mut panes, mut seps) = (vec![], vec![]);
        w.layout.layout(area, &mut panes, &mut seps);
        Some((panes, seps, area.w, area.h))
    }

    fn pane_index(srv: &Server, wid: WinId, pane: PaneId) -> Option<usize> {
        let base = srv.opt_window_num(Some(wid), "pane-base-index").max(0) as usize;
        srv.windows.get(&wid)?.panes().iter().position(|p| *p == pane).map(|i| i + base)
    }

    /// Where each pane is drawn in a dsx x dsy screen (window_panes_get_geometry).
    fn areas(&self, srv: &Server, dsx: u16, dsy: u16) -> Vec<Area> {
        let Some((panes, _, osx, osy)) = self.layout(srv) else { return vec![] };
        let status = srv.opt_window(Some(self.source), "pane-border-status");
        let mut out = Vec::new();
        if dsx == 0 || dsy == 0 {
            return out;
        }
        for (p, r) in panes {
            let (x, y, mut x2, mut y2) = if osx <= dsx && osy <= dsy {
                (r.x, r.y, r.x + r.w, r.y + r.h)
            } else {
                (map(r.x, osx, dsx), map(r.y, osy, dsy), map(r.x + r.w, osx, dsx), map(r.y + r.h, osy, dsy))
            };
            if x >= dsx || y >= dsy {
                continue;
            }
            x2 = x2.max(x + 1).min(dsx);
            y2 = y2.max(y + 1).min(dsy);
            let (mut y, mut sy) = (y, y2 - y);
            if (status == "top" || status == "bottom") && sy > 1 {
                if status == "top" {
                    y += 1;
                }
                sy -= 1;
            }
            out.push((p, x, y, x2 - x, sy));
        }
        for (p, area, _) in self.floats(srv, dsx, dsy) {
            if let Some(area) = area {
                out.push((p, area.0, area.1, area.2, area.3));
            }
        }
        out
    }

    /// The floating panes of the source window back to front, as their saved (unzoomed)
    /// cells: the area drawn (window_panes_get_geometry clipped by
    /// window_panes_clip_floating_pane) and the border lines (x, y, x2, y2) around it
    /// (window_panes_get_floating_borders, -1 for a pane at the top or left edge).
    #[allow(clippy::type_complexity)]
    fn floats(&self, srv: &Server, dsx: u16, dsy: u16) -> Vec<(PaneId, Option<(u16, u16, u16, u16)>, (i32, i32, i32, i32))> {
        let Some(w) = srv.windows.get(&self.source) else { return vec![] };
        let (osx, osy) = (w.sx.max(1), w.sy.max(1));
        if dsx == 0 || dsy == 0 {
            return vec![];
        }
        let scaled = !(osx <= dsx && osy <= dsy);
        let m = |v: i64, o: u16, d: u16| if scaled { v * d as i64 / o as i64 } else { v };
        let mut out = Vec::new();
        for p in w.zlist().into_iter().rev() {
            let Some(g) = w.layout.geom(p) else { continue };
            let (gx, gy) = (g.x as i64, g.y as i64);
            let border = (
                if gx == 0 { -1 } else { m(gx - 1, osx, dsx) as i32 },
                if gy == 0 { -1 } else { m(gy - 1, osy, dsy) as i32 },
                m(gx + g.w as i64, osx, dsx) as i32,
                m(gy + g.h as i64, osy, dsy) as i32,
            );
            // tmux's unsigned offsets: a pane off the left or top is not drawn
            let area = (gx >= 0 && gy >= 0).then(|| {
                let (x, y) = (m(gx, osx, dsx), m(gy, osy, dsy));
                let (mut x2, mut y2) = (m(gx + g.w as i64, osx, dsx), m(gy + g.h as i64, osy, dsy));
                if x >= dsx as i64 || y >= dsy as i64 {
                    return None;
                }
                x2 = x2.max(x + 1).min(dsx as i64);
                y2 = y2.max(y + 1).min(dsy as i64);
                // window_panes_clip_floating_pane: inside the border lines
                let (mut bx, mut by, mut bx2, mut by2) = (x, y, x2 - 1, y2 - 1);
                let (cx, cy, cx2, cy2) = (border.0 as i64, border.1 as i64, border.2 as i64, border.3 as i64);
                if cx >= 0 && bx <= cx { bx = cx + 1; }
                if cy >= 0 && by <= cy { by = cy + 1; }
                if cx2 < dsx as i64 && bx2 >= cx2 { bx2 = cx2 - 1; }
                if cy2 < dsy as i64 && by2 >= cy2 { by2 = cy2 - 1; }
                (bx2 >= bx && by2 >= by).then_some((bx as u16, by as u16, (bx2 - bx + 1) as u16, (by2 - by + 1) as u16))
            }).flatten();
            out.push((p, area, border));
        }
        out
    }

    pub fn render(&self, srv: &Server, w: u16, h: u16) -> Vec<Line> {
        let (wu, hu) = (w as usize, h as usize);
        let mut grid = vec![vec![Cell { text: " ".into(), style: Style::default() }; wu]; hu];
        let Some((_, seps, osx, osy)) = self.layout(srv) else { return grid.into_iter().map(|cells| Line { cells, wrapped: false, ..Default::default() }).collect() };
        let sid = self.session_of(srv);
        let wid = self.source;
        let idx = sid.and_then(|s| srv.sessions.get(&s)).and_then(|s| s.links.iter().find(|(_, w)| **w == wid).map(|(i, _)| *i));
        let owner_t = srv.target_for_pane(self.owner);
        let scaled = !(osx <= w && osy <= h);
        let active = srv.windows.get(&wid).map(|w| w.active);
        let format = srv.opt_window(owner_t.wid, "display-panes-format");
        let draw_pane = |grid: &mut Vec<Vec<Cell>>, (p, x, y, sx, sy): Area| {
            let Some(pane) = srv.panes.get(&p) else { return };
            // the pane's content: the top left, or the area around the cursor when scaled
            let scr = pane.screen();
            let (cy, cx) = scr.cursor_position();
            let (pw, ph) = (pane.size.0, pane.size.1);
            let (px, py) = if !scaled || scr.hide_cursor() {
                (0, 0)
            } else {
                let px = cx.saturating_sub(sx / 3);
                let px = if px + sx > pw { if sx > pw { 0 } else { pw - sx } } else { px };
                let py = cy.saturating_sub(sy / 3);
                let py = if py + sy > ph { if sy > ph { 0 } else { ph - sy } } else { py };
                (px, py)
            };
            for yy in 0..sy {
                for xx in 0..sx {
                    let cell = scr.cell(py + yy, px + xx).map(crate::cells::cell_from_vt).unwrap_or_default();
                    if let Some(row) = grid.get_mut((y + yy) as usize) {
                        if let Some(c) = row.get_mut((x + xx) as usize) {
                            *c = if cell.text.is_empty() && xx == 0 { Cell { text: " ".into(), ..cell } } else { cell };
                        }
                    }
                }
            }
            // screen_write_preview marks the cursor with reverse (borders may cover it later)
            if scaled && !scr.hide_cursor() && cx >= px && cy >= py {
                if let Some(c) = grid.get_mut((y + cy - py) as usize).and_then(|r| r.get_mut((x + cx - px) as usize)) {
                    if c.text.is_empty() {
                        c.text = " ".into();
                    }
                    c.style.attrs |= crate::cells::INVERSE;
                }
            }
            // window_panes_draw_number
            let Some(number) = Panes::pane_index(srv, wid, p) else { return };
            let t = Target { sid, idx, wid: Some(wid), pane: Some(p) };
            let name = if active == Some(p) { "display-panes-active-colour" } else { "display-panes-colour" };
            let colour = crate::format::parse_color_with(&srv.opt_window(owner_t.wid, name), Some(&crate::server::vars::VarCtx { srv, t, client: None, extra: HashMap::new() })).unwrap_or(Color::Default);
            let fgc = Style { fg: colour, ..Style::default() };
            let bgc = Style { bg: colour, ..Style::default() };
            let buf = number.to_string();
            let lbuf = if number > 9 && number < 35 { ((b'a' + (number - 10) as u8) as char).to_string() } else { String::new() };
            let (len, llen) = (buf.len() as u16, lbuf.len() as u16);
            if sx < len {
                return;
            }
            let has_format = !format.is_empty();
            let put = |grid: &mut Vec<Vec<Cell>>, x: u16, y: u16, text: &str, style: Style| {
                for (i, ch) in text.chars().enumerate() {
                    if let Some(c) = grid.get_mut(y as usize).and_then(|r| r.get_mut(x as usize + i)) {
                        *c = Cell { text: ch.to_string(), style };
                    }
                }
            };
            let draw_format = |grid: &mut Vec<Vec<Cell>>| {
                if has_format {
                    let text = expand_as(srv, t, FmtType::Pane, &format, HashMap::new());
                    if !text.is_empty() {
                        for (i, c) in format_cells(&text, sx as usize, fgc).into_iter().enumerate() {
                            if c.text == " " && c.style == fgc {
                                continue; // format_draw leaves unused cells alone
                            }
                            if let Some(g) = grid.get_mut(y as usize).and_then(|r| r.get_mut(x as usize + i)) {
                                *g = c;
                            }
                        }
                    }
                }
            };
            let mut width = len * 6 - 1;
            if sx < width || sy < if has_format { 7 } else { 5 } {
                width = len;
                if llen != 0 && sx >= len + llen + 1 {
                    width += llen + 1;
                }
                let cx = x + (sx - width) / 2;
                let cy = y + sy / 2;
                put(grid, cx, cy, &buf, fgc);
                if width > len {
                    put(grid, cx + len, cy, &format!(" {lbuf}"), fgc);
                }
                if has_format && sy > 1 {
                    draw_format(grid);
                }
                return;
            }
            let mut px = (sx - width) / 2;
            let py = (sy - 5) / 2;
            for ch in buf.chars() {
                let Some((_, rows)) = FONT.iter().find(|(c, _)| *c == ch) else { continue };
                for (j, row) in rows.iter().enumerate() {
                    for (i, b) in row.chars().enumerate() {
                        if b == '#' {
                            put(grid, x + px + i as u16, y + py + j as u16, " ", bgc);
                        }
                    }
                }
                px += 6;
            }
            if sy <= 6 {
                return;
            }
            draw_format(grid);
            if llen != 0 {
                put(grid, (x + px).saturating_sub(llen + 1), y + py + 5, &lbuf, fgc);
            }
        };
        let floats = self.floats(srv, w, h);
        let tiled: Vec<Area> = self.areas(srv, w, h).into_iter().filter(|a| !floats.iter().any(|f| f.0 == a.0)).collect();
        for area in tiled {
            draw_pane(&mut grid, area);
        }

        // window_panes_draw_borders
        let mut bm = BorderMap { w: wu, h: hu, m: vec![0; wu * hu] };
        let mx = |v: u16| map(v, osx, w) as i32;
        let my = |v: u16| map(v, osy, h) as i32;
        for s in &seps {
            if s.vertical {
                bm.vline(mx(s.x), my(s.y), my(s.y + s.len));
            } else {
                bm.hline(mx(s.x), mx(s.x + s.len), my(s.y));
            }
        }
        let status = srv.opt_window(Some(wid), "pane-border-status");
        if status == "top" || status == "bottom" {
            if let Some((panes, _, _, _)) = self.layout(srv) {
                for (_, r) in panes {
                    let y = if status == "top" { my(r.y) } else { my(r.y + r.h) - 1 };
                    bm.hline(mx(r.x), mx(r.x + r.w), y);
                }
            }
        }
        // joins where a separator meets another line
        let horizontal = |m: u8| m & (BORDER_L | BORDER_R) != 0;
        let vertical = |m: u8| m & (BORDER_U | BORDER_D) != 0;
        for s in &seps {
            if s.vertical {
                let (x, y, y2) = (mx(s.x), my(s.y), my(s.y + s.len));
                if y > 0 && horizontal(bm.get(x, y - 1)) {
                    bm.mark(x, y - 1, BORDER_D);
                    bm.mark(x, y, BORDER_U);
                }
                if (y2 as usize) < hu && horizontal(bm.get(x, y2)) {
                    bm.mark(x, y2, BORDER_U);
                    bm.mark(x, y2 - 1, BORDER_D);
                }
            } else {
                let (x, x2, y) = (mx(s.x), mx(s.x + s.len), my(s.y));
                if x > 0 && vertical(bm.get(x - 1, y)) {
                    bm.mark(x - 1, y, BORDER_R);
                    bm.mark(x, y, BORDER_L);
                }
                if (x2 as usize) < wu && vertical(bm.get(x2, y)) {
                    bm.mark(x2, y, BORDER_L);
                    bm.mark(x2 - 1, y, BORDER_R);
                }
            }
        }
        let t = Target { sid, idx, wid: Some(wid), pane: active };
        let bstyle = mode_style(srv, &srv.opt_window(owner_t.wid, "display-panes-border-style"), Style::default(), t);
        for y in 0..hu {
            for x in 0..wu {
                let m = bm.m[y * wu + x];
                if m != 0 {
                    grid[y][x] = Cell { text: border_char(m).into(), style: bstyle };
                }
            }
        }
        // window_panes_draw_screen: the floating panes go over the tiled ones, back to front,
        // each on a cleared area inside its own border
        for (p, area, (bx, by, bx2, by2)) in floats {
            for yy in by.max(0)..=by2.min(h as i32 - 1) {
                for xx in bx.max(0)..=bx2.min(w as i32 - 1) {
                    grid[yy as usize][xx as usize] = Cell { text: " ".into(), style: Style::default() };
                }
            }
            if let Some((x, y, sx, sy)) = area {
                draw_pane(&mut grid, (p, x, y, sx, sy));
            }
            let mut fm = BorderMap { w: wu, h: hu, m: vec![0; wu * hu] };
            fm.hline(bx, bx2 + 1, by);
            fm.hline(bx, bx2 + 1, by2);
            fm.vline(bx, by, by2 + 1);
            fm.vline(bx2, by, by2 + 1);
            for y in 0..hu {
                for x in 0..wu {
                    let m = fm.m[y * wu + x];
                    if m != 0 {
                        grid[y][x] = Cell { text: border_char(m).into(), style: bstyle };
                    }
                }
            }
        }
        grid.into_iter().map(|cells| Line { cells, wrapped: false, ..Default::default() }).collect()
    }

    /// The pane a key chooses (window_panes_key_pane): 0-9 then a-z, by pane index.
    fn key_pane(&self, srv: &Server, key: &str) -> Option<PaneId> {
        let mut chars = key.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else { return None };
        let index = match c {
            '0'..='9' => c as usize - '0' as usize,
            'a'..='z' => 10 + (c as usize - 'a' as usize),
            _ => return None,
        };
        let w = srv.windows.get(&self.source)?;
        let base = srv.opt_window_num(Some(self.source), "pane-base-index").max(0) as usize;
        index.checked_sub(base).and_then(|i| w.panes().get(i).copied())
    }

    fn run(&self, pane: PaneId) -> ModeAction {
        ModeAction::Run(crate::server::input::fill_template(&self.template, &[format!("%{pane}")]))
    }

    pub fn key(&mut self, name: &str, srv: &Server) -> ModeAction {
        if name == "Escape" || name == "q" {
            return ModeAction::Exit;
        }
        if self.ignore_keys {
            return ModeAction::None;
        }
        match self.key_pane(srv, name) {
            Some(p) => self.run(p),
            None => ModeAction::Exit,
        }
    }

    pub fn mouse(&mut self, key: &str, x: u16, y: u16, srv: &Server, w: u16, h: u16) -> ModeAction {
        if self.ignore_keys || key != "MouseDown1Pane" {
            return ModeAction::None;
        }
        let hit = self.areas(srv, w, h).into_iter().rev().find(|(_, ax, ay, aw, ah)| x >= *ax && x < ax + aw && y >= *ay && y < ay + ah);
        match hit {
            Some((p, ..)) if srv.panes.contains_key(&p) => self.run(p),
            _ => ModeAction::None,
        }
    }
}
