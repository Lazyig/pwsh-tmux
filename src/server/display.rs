//! Display option geometry shared by composition and mouse classification.
use super::*;
use crate::cells::{Cell, Style};

#[derive(Clone, Copy, Debug)]
pub struct Scrollbar {
    /// Left column of the bar itself (the pad is beside it, toward the pane), in the
    /// window: a floating pane's may be off its left or top edge.
    pub x: i32,
    pub y: i32,
    pub width: u16,
    pub pad: u16,
    pub left: bool,
    pub height: u16,
    pub slider_y: u16,
    pub slider_h: u16,
    pub style: Style,
    /// The pane's default colours, for the pad.
    pub pad_style: Style,
    pub visible: bool,
}

/// style_set_scrollbar_style_from_option: the option goes over the default
/// style; width below 1 is 1.
fn scrollbar_style_text(option: &str) -> String {
    format!("{},{option}", crate::options::def("pane-scrollbars-style").map_or("", |d| d.default))
}

impl Server {
    /// The rows beside which a pane's scrollbar is drawn: its layout cell less a
    /// pane status line.
    pub fn scrollbar_area(&self, wid: WinId, r: Rect) -> Rect {
        match self.opt_window(Some(wid), "pane-border-status").as_str() {
            "top" if r.y == 0 && r.h > 1 => Rect { y: r.y + 1, h: r.h - 1, ..r },
            "bottom" if self.windows.get(&wid).is_some_and(|w| r.bottom() == w.sy) && r.h > 1 => Rect { h: r.h - 1, ..r },
            _ => r,
        }
    }

    /// The scrollbar of a pane in layout cell `r`, as drawn.
    pub fn pane_scrollbar(&self, wid: WinId, pane: PaneId, r: Rect) -> Option<Scrollbar> {
        self.scrollbar(wid, pane, self.scrollbar_area(wid, r))
    }

    pub fn scrollbar(&self, wid: WinId, pane: PaneId, r: Rect) -> Option<Scrollbar> {
        self.scrollbar_at(wid, pane, (r.x as i32, r.y as i32, r.w, r.h))
    }

    /// A floating pane's scrollbar: inside its border, over the pane's last columns
    /// (modal and auto-hide) or beside the narrower pane (on, which reserves the room).
    pub fn float_scrollbar(&self, wid: WinId, pane: PaneId, g: crate::layout::FGeom) -> Option<Scrollbar> {
        self.scrollbar_at(wid, pane, (g.x, g.y, g.w, g.h))
    }

    /// The content of a floating pane with geometry `g`: less a reserved scrollbar
    /// (window_pane_scrollbar_reserve), as for a tiled pane.
    pub fn float_content(&self, wid: WinId, pane: PaneId, mut g: crate::layout::FGeom) -> crate::layout::FGeom {
        if self.opt_window(Some(wid), "pane-scrollbars") == "on" {
            if let Some(sb) = self.float_scrollbar(wid, pane, g) {
                let width = sb.width.saturating_add(sb.pad).min(g.w.saturating_sub(1));
                g.w -= width;
                if sb.left { g.x += width as i32; }
            }
        }
        g
    }

    /// The scrollbars of a window's panes, as drawn: the tiled panes', then the floating
    /// panes' back to front.
    pub fn scrollbars(&self, wid: WinId) -> Vec<(PaneId, Scrollbar)> {
        let mut bars: Vec<(PaneId, Scrollbar)> = self.rects(wid).0.into_iter().filter_map(|(p, r)| self.pane_scrollbar(wid, p, r).map(|sb| (p, sb))).collect();
        bars.extend(self.float_rects(wid).into_iter().filter_map(|(p, g)| self.float_scrollbar(wid, p, g).map(|sb| (p, sb))));
        bars
    }

    fn scrollbar_at(&self, wid: WinId, pane: PaneId, (rx, ry, rw, rh): (i32, i32, u16, u16)) -> Option<Scrollbar> {
        let r = Rect { x: 0, y: 0, w: rw, h: rh };
        let p = self.panes.get(&pane)?;
        let option = self.opt_window(Some(wid), "pane-scrollbars");
        let copying = matches!(p.mode, Some(PaneMode::Copy(_)));
        // window_pane_show_scrollbar: clock and tree modes are not copy modes, so
        // they have the bar of a pane without a mode
        if option == "off" || (option == "modal" && !copying) || p.screen().alternate_screen() || r.w < 2 || r.h == 0 {
            return None;
        }
        let text = self.expand_for_pane(pane, &scrollbar_style_text(&self.opt_pane(pane, "pane-scrollbars-style")));
        let number = |name: &str, default: u16| text.split(',').filter_map(|s| s.trim().strip_prefix(name).and_then(|v| v.parse::<u16>().ok())).last().unwrap_or(default);
        let width = number("width=", 1).max(1).min(r.w.saturating_sub(1));
        let pad = number("pad=", 0).min(r.w.saturating_sub(1) - width);
        let left = self.opt_window(Some(wid), "pane-scrollbars-position") == "left";
        let x = rx + if left { 0 } else { r.right().saturating_sub(width) as i32 };
        let (history, offset) = match &p.mode {
            Some(PaneMode::Copy(cm)) => (cm.hsize_lines(), cm.scroll_offset()),
            // other modes draw their own screen, which has no history
            Some(_) => (0, 0),
            None => (p.screen().history_rows_len().saturating_sub(p.size.1 as usize), 0),
        };
        let total = history + r.h as usize;
        let slider_h = ((r.h as usize * r.h as usize / total.max(1)) as u16).max(1);
        let slider_y = (((r.h as usize + 1) * history.saturating_sub(offset) / total.max(1)) as u16).min(r.h.saturating_sub(slider_h));
        let style = self.style_option(&text, Style::default(), self.target_for_pane(pane), None).unwrap_or_default();
        let pst = |name: &str| self.style_option(&self.opt_pane(pane, name), Style::default(), self.target_for_pane(pane), None).unwrap_or_default();
        let active = self.windows.get(&wid).is_some_and(|w| w.active == pane);
        let pad_style = if active && pst("window-active-style") != Style::default() { pst("window-active-style") } else { pst("window-style") };
        Some(Scrollbar { x, y: ry, width, pad, left, height: r.h, slider_y, slider_h, style, pad_style,
            visible: option == "on" || p.scrollbar_until.is_some_and(|d| d > Instant::now()) })
    }

    pub fn show_scrollbar(&mut self, pane: PaneId) {
        let wid = self.window_of(pane);
        // only modal and auto-hide scrollbars come and go, and only while shown at
        // all (window_pane_show_scrollbar: modal ones in copy mode, never on the
        // alternate screen)
        let option = self.opt_window(wid, "pane-scrollbars");
        if !matches!(option.as_str(), "modal" | "auto-hide") { return; }
        let ms = self.opt_window_num(wid, "pane-scrollbars-timeout").max(0) as u64;
        let Some(p) = self.panes.get_mut(&pane) else { return };
        if p.screen().alternate_screen() || (option == "modal" && !matches!(p.mode, Some(PaneMode::Copy(_)))) { return; }
        let shown = p.scrollbar_until.replace(Instant::now() + Duration::from_millis(ms)).is_some();
        if let Some(wid) = wid.filter(|_| !shown) { self.mark_window_dirty(wid); }
    }

    /// The copy-mode view offset of a pane, to tell whether an action scrolled.
    pub fn copy_scroll_offset(&self, pane: PaneId) -> Option<usize> {
        match self.panes.get(&pane).and_then(|p| p.mode.as_ref()) {
            Some(PaneMode::Copy(cm)) => Some(cm.scroll_offset()),
            _ => None,
        }
    }

    /// window_copy scrolling functions show an auto-hide scrollbar; cursor
    /// movement that does not scroll and leaving the mode do not.
    pub fn show_scrollbar_if_scrolled(&mut self, pane: PaneId, before: Option<usize>, always: bool) {
        let after = self.copy_scroll_offset(pane);
        if after.is_some() && (always || after != before) {
            self.show_scrollbar(pane);
        }
    }
}

pub fn draw_scrollbar(f: &mut super::render::Frame, sb: Scrollbar, (dx, dy): (i32, i32), clip: (u16, u16), oy: u16) {
    if !sb.visible { return; }
    // redraw_draw_scrollbar_span: track in the style, slider with its colours
    // swapped, pad cells in the pane's default colours; (dx, dy) moves the bar from
    // window to client coordinates, and only what is on the client is drawn
    let mut put = |x: i32, y: i32, style: Style| {
        let (x, y) = (x + dx, y + dy);
        if x >= 0 && y >= 0 && x < clip.0 as i32 && y < clip.1 as i32 {
            f.set(x as u16, oy + y as u16, Cell { text: " ".into(), style });
        }
    };
    let pad_x = if sb.left { sb.x + sb.width as i32 } else { sb.x - sb.pad as i32 };
    for y in 0..sb.height {
        let slider = y >= sb.slider_y && y < sb.slider_y + sb.slider_h;
        let style = if slider { Style { fg: sb.style.bg, bg: sb.style.fg, ..sb.style } } else { sb.style };
        let row = sb.y + y as i32;
        for x in 0..sb.width { put(sb.x + x as i32, row, style); }
        for x in 0..sb.pad { put(pad_x + x as i32, row, sb.pad_style); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::pan::tests::{clients, kill, run};
    use crate::cells::Color;
    use crate::keys::{Input, Mods, Mouse, MouseKind};

    fn fmt(srv: &Server, pane: PaneId, f: &str) -> String {
        srv.expand_for_pane(pane, f)
    }

    #[test]
    fn floating_panes_have_scrollbars_inside_their_borders() {
        let (mut srv, _) = clients((60, 16), None);
        run(&mut srv, 1, &["new-session", "-d", "-s", "s", "-x", "60", "-y", "15"]).unwrap();
        let sid = *srv.sessions.keys().next().unwrap();
        srv.attach_client(1, sid);
        run(&mut srv, 1, &["set", "-g", "pane-scrollbars-style", "bg=red,fg=blue,width=1,pad=0"]).unwrap();
        run(&mut srv, 1, &["new-pane", "-d", "-x", "20", "-y", "6", "-X", "30", "-Y", "3"]).unwrap();
        let wid = srv.client_window(1).unwrap();
        let float = srv.float_rects(wid)[0].0;
        let g = srv.panes[&float].generation;
        let text: String = (1..=50).map(|i| format!("{i}\r\n")).collect();
        srv.pane_data(float, g, text.as_bytes());
        // on: the pane is narrower and the bar is at its right, inside the border
        run(&mut srv, 1, &["set", "-g", "pane-scrollbars", "on"]).unwrap();
        assert_eq!(fmt(&srv, float, "#{pane_width}x#{pane_height}@#{pane_left},#{pane_top}"), "17x4@31,4");
        let bg = |srv: &Server, x: u16, y: u16| srv.compose(1).unwrap().frame.get(x, y).unwrap().style.bg;
        let f = srv.compose(1).unwrap().frame;
        assert_eq!(f.get(49, 4).unwrap().text, "│");
        // the track above the slider (at the bottom: the pane shows its last lines)
        assert_eq!(bg(&srv, 48, 4), Color::Idx(1));
        assert_eq!(bg(&srv, 48, 7), Color::Idx(4));
        run(&mut srv, 1, &["set", "-g", "pane-scrollbars-position", "left"]).unwrap();
        assert_eq!(fmt(&srv, float, "#{pane_width}@#{pane_left}"), "17@32");
        assert_eq!(bg(&srv, 31, 4), Color::Idx(1));
        run(&mut srv, 1, &["set", "-g", "pane-scrollbars-position", "right"]).unwrap();
        // a click on the track above the slider is a ScrollbarUp click on the floating
        // pane (MouseDown1ScrollbarUp scrolls it up in copy mode), not one in its content
        run(&mut srv, 1, &["set", "-g", "mouse", "on"]).unwrap();
        for kind in [MouseKind::Down, MouseKind::Up] {
            srv.dispatch_input(1, Input::Mouse(Mouse { kind, button: 1, x: 48, y: 4, mods: Mods::NONE }, vec![]));
        }
        assert!(matches!(srv.panes[&float].mode, Some(PaneMode::Copy(_))));
        assert_ne!(fmt(&srv, float, "#{scroll_position}"), "0");
        srv.exit_mode(float);
        // auto-hide: over the pane's last column once shown, which keeps its width
        run(&mut srv, 1, &["set", "-g", "pane-scrollbars", "auto-hide"]).unwrap();
        assert_eq!(fmt(&srv, float, "#{pane_width}"), "18");
        assert_ne!(bg(&srv, 48, 4), Color::Idx(1));
        srv.dispatch_input(1, Input::Mouse(Mouse { kind: MouseKind::Move, button: 0, x: 48, y: 5, mods: Mods::NONE }, vec![]));
        assert_eq!(bg(&srv, 48, 4), Color::Idx(1));
        kill(&mut srv);
    }
}
