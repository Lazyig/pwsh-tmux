//! Display option geometry shared by composition and mouse classification.
use super::*;
use crate::cells::{Cell, Style};

#[derive(Clone, Copy, Debug)]
pub struct Scrollbar {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub slider_y: u16,
    pub slider_h: u16,
    pub style: Style,
    pub visible: bool,
}

impl Server {
    pub fn scrollbar(&self, wid: WinId, pane: PaneId, r: Rect) -> Option<Scrollbar> {
        let p = self.panes.get(&pane)?;
        let option = self.opt_window(Some(wid), "pane-scrollbars");
        let copying = matches!(p.mode, Some(PaneMode::Copy(_)));
        if option == "off" || (option == "modal" && !copying) || p.screen().alternate_screen() || matches!(p.mode, Some(PaneMode::Tree(_) | PaneMode::Clock)) || r.w < 2 || r.h == 0 {
            return None;
        }
        let text = self.expand_for_pane(pane, &self.opt_pane(pane, "pane-scrollbars-style"));
        let number = |name: &str, default: u16| text.split(',').find_map(|s| s.trim().strip_prefix(name).and_then(|v| v.parse::<u16>().ok())).unwrap_or(default);
        let width = number("width=", 1).min(r.w.saturating_sub(1));
        if width == 0 { return None; }
        let left = self.opt_window(Some(wid), "pane-scrollbars-position") == "left";
        let x = if left { r.x } else { r.right().saturating_sub(width) };
        let (history, offset) = match &p.mode {
            Some(PaneMode::Copy(cm)) => (cm.history_size(), cm.scroll_offset()),
            _ => (p.screen().history_rows_len().saturating_sub(p.size.1 as usize), 0),
        };
        let total = history + r.h as usize;
        let slider_h = ((r.h as usize * r.h as usize / total.max(1)) as u16).max(1);
        let slider_y = (((r.h as usize + 1) * history.saturating_sub(offset) / total.max(1)) as u16).min(r.h.saturating_sub(slider_h));
        let style = self.style_option(&text, Style::default(), self.target_for_pane(pane), None).unwrap_or_default();
        Some(Scrollbar { x, y: r.y, width, height: r.h, slider_y, slider_h, style,
            visible: option == "on" || p.scrollbar_hover || p.scrollbar_until.is_some_and(|d| d > Instant::now()) })
    }

    pub fn show_scrollbar(&mut self, pane: PaneId) {
        let wid = self.window_of(pane);
        let ms = self.opt_window_num(wid, "pane-scrollbars-timeout").max(0) as u64;
        if let Some(p) = self.panes.get_mut(&pane) { p.scrollbar_until = Some(Instant::now() + Duration::from_millis(ms)); }
        if let Some(wid) = wid { self.mark_window_dirty(wid); }
    }

    pub fn scrollbar_to_mouse(&mut self, pane: PaneId, m: &super::mouse::MouseCtx) {
        let Some(wid) = self.window_of(pane) else { return };
        let Some((_, r)) = self.rects(wid).0.into_iter().find(|(p, _)| *p == pane) else { return };
        let y = m.y.saturating_sub(if m.statusat == Some(0) { m.statuslines } else { 0 }).saturating_sub(r.y);
        if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
            cm.scroll_fraction(y as usize, r.h as usize);
        }
        self.show_scrollbar(pane);
    }
}

pub fn draw_scrollbar(f: &mut super::render::Frame, sb: Scrollbar, oy: u16) {
    if !sb.visible { return; }
    for y in 0..sb.height {
        let slider = y >= sb.slider_y && y < sb.slider_y + sb.slider_h;
        let style = Style { bg: if slider { sb.style.fg } else { sb.style.bg }, ..Style::default() };
        for x in 0..sb.width { f.set(sb.x + x, oy + sb.y + y, Cell { text: " ".into(), style }); }
    }
}
