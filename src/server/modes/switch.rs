//! switch-mode (tmux 3.8 window-switch.c): a fast switcher listing sessions or windows,
//! narrowed by fuzzy matching what is typed into a prompt on the last line.

use super::fuzzy::fuzzy_match;
use super::prompt::{PanePrompt, PromptKey};
use super::{expand_as, format_cells, mode_style, FmtType, ModeAction};
use crate::cells::{Line, Style};
use crate::server::vars::Target;
use crate::server::*;

pub const DEFAULT_COMMAND: &str = "switch-client -Zt '%%'";

pub const DEFAULT_FORMAT: &str = concat!(
    "#{?window_format,",
    "#{window_name} ",
    "#[dim]#{session_name}:#{window_index}#{window_flags}#[default] ",
    "#[dim]#{pane_current_command}#[default] ",
    "#[dim]#{?#{!=:#{pane_title},#{host_short}},#{pane_title},}#[default]",
    ",",
    "#{session_name} ",
    "#[dim]#{session_windows} windows#[default] ",
    "#{?session_attached,attached,#[dim]detached#[default]} ",
    "#[dim]#{window_name}#[default]",
    "}"
);

pub struct Switch {
    pub owner: PaneId,
    /// -w lists windows, otherwise sessions.
    pub windows: bool,
    pub format: String,
    pub command: String,
    pub prompt: PanePrompt,
    pub filter: String,
    pub current: usize,
    pub offset: usize,
    /// -Z: Some(was already zoomed); None when not asked to zoom.
    pub zoomed: Option<bool>,
    /// -k: kill the pane when the mode exits.
    pub kill: bool,
    pub client: Option<ClientId>,
    /// The list as built (window_switch_build runs on entry, resize and each key the prompt
    /// handles, not when the sessions change): the width and each item's target and text.
    built: std::cell::RefCell<Option<(usize, Vec<(String, String)>)>>,
}

pub struct SwitchItem {
    /// The target substituted for %%: =session: or =session:index.
    pub target: String,
    pub text: String,
    pub mask: Option<Vec<bool>>,
    pub score: u32,
}

impl Switch {
    pub fn new(owner: PaneId, windows: bool, format: Option<String>, command: Option<String>) -> Switch {
        Switch {
            owner,
            windows,
            format: format.unwrap_or_else(|| DEFAULT_FORMAT.to_string()),
            command: command.unwrap_or_else(|| DEFAULT_COMMAND.to_string()),
            prompt: PanePrompt::new("(search) ", ""),
            filter: String::new(),
            current: 0,
            offset: 0,
            zoomed: None,
            kill: false,
            client: None,
            built: std::cell::RefCell::new(None),
        }
    }

    /// The matching items, best first: sessions or windows by name.
    pub fn items(&self, srv: &Server, width: usize) -> Vec<SwitchItem> {
        let all = match &*self.built.borrow() {
            Some((w, all)) if *w == width => all.clone(),
            _ => self.build(srv),
        };
        *self.built.borrow_mut() = Some((width, all.clone()));
        self.matches(all, width)
    }

    /// Build the list now (when the mode is entered).
    pub fn snapshot(&self, srv: &Server, width: usize) {
        *self.built.borrow_mut() = Some((width, self.build(srv)));
    }

    fn build(&self, srv: &Server) -> Vec<(String, String)> {
        let mut sessions: Vec<&Session> = srv.sessions.values().collect();
        sessions.sort_by(|a, b| a.name.cmp(&b.name));
        let mut all = Vec::new();
        if self.windows {
            let mut wls: Vec<(&Session, u32, WinId)> = sessions.iter().flat_map(|s| s.links.iter().map(move |(i, w)| (*s, *i, *w))).collect();
            wls.sort_by(|a, b| {
                let name = |w: WinId| srv.windows.get(&w).map(|w| w.name.as_str()).unwrap_or("");
                name(a.2).cmp(name(b.2))
            });
            for (s, idx, wid) in wls {
                let pane = srv.windows.get(&wid).map(|w| w.active);
                let t = Target { sid: Some(s.id), idx: Some(idx), wid: Some(wid), pane };
                all.push((format!("={}:{idx}.", s.name), expand_as(srv, t, FmtType::Window, &self.format, HashMap::new())));
            }
        } else {
            for s in sessions {
                let wid = s.active_window();
                let t = Target { sid: Some(s.id), idx: Some(s.active), wid: Some(wid), pane: srv.windows.get(&wid).map(|w| w.active) };
                all.push((format!("={}:", s.name), expand_as(srv, t, FmtType::Session, &self.format, HashMap::new())));
            }
        }
        all
    }

    fn matches(&self, all: Vec<(String, String)>, width: usize) -> Vec<SwitchItem> {
        let mut out: Vec<(SwitchItem, usize)> = Vec::new();
        for (order, (target, text)) in all.into_iter().enumerate() {
            if self.filter.is_empty() {
                out.push((SwitchItem { target, text, mask: None, score: 0 }, order));
                continue;
            }
            if let Some((mask, score)) = fuzzy_match(&self.filter, &text, width) {
                out.push((SwitchItem { target, text, mask: Some(mask), score }, order));
            }
        }
        out.sort_by(|a, b| b.0.score.cmp(&a.0.score).then(a.1.cmp(&b.1)));
        out.into_iter().map(|(i, _)| i).collect()
    }

    fn visible(h: usize) -> usize {
        h.saturating_sub(1)
    }

    fn set_current(&mut self, current: usize, size: usize, h: usize) {
        if size == 0 {
            self.current = 0;
            self.offset = 0;
            return;
        }
        self.current = current.min(size - 1);
        let visible = Switch::visible(h);
        if self.current < self.offset {
            self.offset = self.current;
        } else if visible != 0 && self.current >= self.offset + visible {
            self.offset = self.current + 1 - visible;
        }
    }

    fn choose(&self, srv: &Server, w: usize) -> ModeAction {
        let items = self.items(srv, w);
        match items.get(self.current) {
            Some(it) => ModeAction::Run(crate::server::input::fill_template(&self.command, std::slice::from_ref(&it.target))),
            None => ModeAction::None,
        }
    }

    /// window_switch_key for a key name; `w` x `h` is the pane size.
    pub fn key(&mut self, name: &str, srv: &Server, w: usize, h: usize) -> ModeAction {
        let name = match name {
            "C-p" | "C-k" => "Up",
            "C-n" | "C-j" => "Down",
            n => n,
        };
        match name {
            "Enter" | "C-m" => return self.choose(srv, w),
            "Escape" | "C-[" | "C-c" | "C-g" => return ModeAction::Exit,
            _ => {}
        }
        match self.prompt.key(name) {
            PromptKey::Handled => {
                // window_switch_prompt_callback: every handled key builds the list again
                self.filter = self.prompt.text();
                self.snapshot(srv, w);
                self.current = 0;
                self.offset = 0;
                return ModeAction::None;
            }
            PromptKey::Done(_) | PromptKey::Cancel => return ModeAction::None,
            PromptKey::NotHandled if !matches!(name, "Up" | "Down" | "PPage" | "NPage" | "Home" | "End") => return ModeAction::None,
            PromptKey::NotHandled => {}
        }
        let size = self.items(srv, w).len();
        let current = self.current;
        match name {
            "Up" if size != 0 => self.set_current(if current == 0 { size - 1 } else { current - 1 }, size, h),
            "Down" if size != 0 => self.set_current(if current + 1 >= size { 0 } else { current + 1 }, size, h),
            "PPage" => self.set_current(current.saturating_sub(Switch::visible(h)), size, h),
            "NPage" => self.set_current(current + Switch::visible(h), size, h),
            "Home" => self.set_current(0, size, h),
            "End" if size > 0 => self.set_current(size - 1, size, h),
            _ => {}
        }
        ModeAction::None
    }

    /// Mouse keys at (x, y) in the pane: wheel moves, a click selects, a double click chooses.
    pub fn mouse(&mut self, key: &str, x: u16, y: u16, srv: &Server, w: usize, h: usize) -> ModeAction {
        let size = self.items(srv, w).len();
        if h != 0 && y as usize == h - 1 && key == "MouseDown1Pane" {
            // a click on the prompt line moves its cursor
            let label = crate::format::width(&self.prompt.label);
            let mut col = label;
            let mut i = 0;
            while i < self.prompt.input.len() && col < x as usize {
                col += crate::format::width(&self.prompt.input[i].to_string());
                i += 1;
            }
            self.prompt.cursor = i;
            return ModeAction::None;
        }
        match key {
            "WheelUpPane" if size != 0 && self.current != 0 => self.set_current(self.current - 1, size, h),
            "WheelDownPane" if size != 0 && self.current + 1 != size => self.set_current(self.current + 1, size, h),
            "MouseDown1Pane" | "DoubleClick1Pane" => {
                if (y as usize) >= Switch::visible(h) || self.offset + y as usize >= size {
                    return ModeAction::None;
                }
                self.set_current(self.offset + y as usize, size, h);
                if key == "DoubleClick1Pane" {
                    return self.choose(srv, w);
                }
            }
            _ => {}
        }
        ModeAction::None
    }

    /// window_switch_draw_screen: the lines and the prompt cursor.
    pub fn render(&self, srv: &Server, w: u16, h: u16) -> (Vec<Line>, Option<(u16, u16)>) {
        let (wu, hu) = (w as usize, h as usize);
        let blank = Line { cells: vec![crate::cells::Cell { text: " ".into(), style: Style::default() }; wu], wrapped: false };
        let mut lines = vec![blank; hu];
        if hu <= 1 {
            return (lines, None);
        }
        let t = srv.target_for_pane(self.owner);
        let wid = t.wid;
        let mstyle = mode_style(srv, &srv.opt_pane(self.owner, "switch-mode-match-style"), Style::default(), t);
        let sstyle = mode_style(srv, &srv.opt_window(wid, "mode-style"), Style::default(), t);
        let items = self.items(srv, wu);
        for i in 0..Switch::visible(hu) {
            let idx = self.offset + i;
            let Some(it) = items.get(idx) else { break };
            let base = if idx == self.current { sstyle } else { Style::default() };
            let mut cells = format_cells(&it.text, wu, base);
            if let Some(mask) = &it.mask {
                for (x, on) in mask.iter().enumerate() {
                    if *on && x < cells.len() {
                        let st = &mut cells[x].style;
                        st.fg = mstyle.fg;
                        st.bg = mstyle.bg;
                        st.attrs = mstyle.attrs;
                    }
                }
            }
            lines[i].cells = cells;
        }
        let sid = t.sid;
        let pstyle = mode_style(srv, &srv.opt_session(sid, "message-style"), Style::default(), t);
        let (cells, cx) = self.prompt.draw(wu, pstyle);
        lines[hu - 1].cells = cells;
        (lines, Some((cx as u16, h - 1)))
    }
}
