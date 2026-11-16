//! Display option geometry shared by composition and mouse classification.
use super::*;
use crate::cells::{Cell, Style};

#[derive(Clone, Copy, Debug)]
pub struct Scrollbar {
    /// Left column of the bar itself (the pad is beside it, toward the pane).
    pub x: u16,
    pub y: u16,
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
        let x = if left { r.x } else { r.right().saturating_sub(width) };
        let (history, offset) = match &p.mode {
            Some(PaneMode::Copy(cm)) => (cm.history_size(), cm.scroll_offset()),
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
        Some(Scrollbar { x, y: r.y, width, pad, left, height: r.h, slider_y, slider_h, style, pad_style,
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

    /// window_copy_scroll1: drag the slider so the point of it grabbed (the client's
    /// slider_mpos) follows the mouse, stopping at the ends of the bar.
    pub fn scrollbar_to_mouse(&mut self, cid: ClientId, pane: PaneId, m: &super::mouse::MouseCtx) {
        let Some(wid) = self.window_of(pane) else { return };
        let Some((_, r)) = self.rects(wid).0.into_iter().find(|(p, _)| *p == pane) else { return };
        let area = self.scrollbar_area(wid, r);
        let Some(sb) = self.scrollbar(wid, pane, area) else { return };
        let mpos = self.clients.get(&cid).map_or(0, |c| c.slider_mpos) as i32;
        let my = m.y.saturating_sub(if m.statusat == Some(0) { m.statuslines } else { 0 }) as i32;
        let (top, h, sh) = (area.y as i32, area.h as i32, sb.slider_h as i32);
        let slider_y = if my <= top + mpos { 0 } else if my - mpos > top + h - sh { h - sh } else { my - top - mpos };
        if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
            cm.scroll_to_slider(slider_y.max(0) as usize, area.h as usize);
        }
        self.show_scrollbar(pane);
    }
}

pub fn draw_scrollbar(f: &mut super::render::Frame, sb: Scrollbar, oy: u16) {
    if !sb.visible { return; }
    // redraw_draw_scrollbar_span: track in the style, slider with its colours
    // swapped, pad cells in the pane's default colours
    let pad_x = if sb.left { sb.x + sb.width } else { sb.x.saturating_sub(sb.pad) };
    for y in 0..sb.height {
        let slider = y >= sb.slider_y && y < sb.slider_y + sb.slider_h;
        let style = if slider { Style { fg: sb.style.bg, bg: sb.style.fg, ..sb.style } } else { sb.style };
        for x in 0..sb.width { f.set(sb.x + x, oy + sb.y + y, Cell { text: " ".into(), style }); }
        for x in 0..sb.pad { f.set(pad_x + x, oy + sb.y + y, Cell { text: " ".into(), style: sb.pad_style }); }
    }
}
