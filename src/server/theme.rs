//! tmux 3.8 client themes, colour queries and pane notifications.
use super::*;
use super::vars::VarCtx;
use crate::cells::Color;
use crate::format::{self, Theme};

#[derive(Clone, Copy, Default, Debug)]
pub struct Terminal {
    /// Only mode 2031 / DSR 996 reports belong in client_theme.
    pub reported: Option<Theme>,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pending: Option<Instant>,
    control_fg: Option<Color>,
    control_bg: Option<Color>,
}

impl Terminal {
    pub fn detected(self) -> Option<Theme> {
        self.reported.or_else(|| self.bg.and_then(background_theme))
    }
}

pub fn background_theme(c: Color) -> Option<Theme> {
    if c == Color::Idx(0) || c == Color::Idx(8) { return Some(Theme::Dark); }
    if c == Color::Idx(7) || c == Color::Idx(15) { return Some(Theme::Light); }
    let (r, g, b) = rgb(c)?;
    Some(if r as u16 + g as u16 + b as u16 > 382 { Theme::Light } else { Theme::Dark })
}

fn rgb(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Default => None,
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Idx(i) => Some(features::idx_to_rgb(i)),
    }
}

fn x11(s: &str) -> Option<Color> {
    let c = format::parse_color(&parse_x11_colour(s)?)?;
    (c != Color::Default).then_some(c)
}

/// Complete outer-terminal colour reply (BEL and ST are both accepted).
fn colour_report(raw: &[u8]) -> Option<(bool, Color)> {
    let body = raw.strip_prefix(b"\x1b]")?;
    let body = body.strip_suffix(b"\x1b\\").or_else(|| body.strip_suffix(b"\x07"))?;
    let s = std::str::from_utf8(body).ok()?;
    let (n, value) = s.split_once(';')?;
    Some((match n { "10" => true, "11" => false, _ => return None }, x11(value)?))
}

impl VarCtx<'_> {
    pub(super) fn client_colour_escape(&self, name: &str, background: bool) -> Option<String> {
        let client = self.client.and_then(|id| self.srv.clients.get(&id))?;
        let lower = name.to_ascii_lowercase();
        let value = self.theme_value(&lower).unwrap_or(lower);
        let c = if value.starts_with("theme") { Color::Default } else { format::parse_color(&value)? };
        let fitted = features::fit_color(c, &client.features);
        let palette = (value.starts_with("colo") || matches!(c, Color::Rgb(..))) && client.features.has("256");
        let base = if background { 40 } else { 30 };
        Some(match fitted {
            Color::Default => format!("\x1b[{}m", base + 9),
            Color::Rgb(r, g, b) => format!("\x1b[{};2;{r};{g};{b}m", base + 8),
            Color::Idx(n) if palette || n >= 16 => format!("\x1b[{};5;{n}m", base + 8),
            Color::Idx(n) if n < 8 => format!("\x1b[{}m", base + n as u32),
            Color::Idx(n) => format!("\x1b[{}m", base + 60 + n as u32 - 8),
        })
    }

    pub(super) fn resolve_theme_colour(&self, name: &str) -> Option<Color> {
        let value = self.theme_value(name)?;
        Some(if value.to_ascii_lowercase().starts_with("theme") { Color::Default }
            else { format::parse_color(&value).unwrap_or(Color::Default) })
    }

    fn theme_value(&self, name: &str) -> Option<String> {
        let suffix = match name {
            "themeblack" => "black", "themewhite" => "white",
            "themelightgrey" => "light-grey", "themedarkgrey" => "dark-grey",
            "themegreen" => "green", "themeyellow" => "yellow", "themered" => "red",
            "themeblue" => "blue", "themecyan" => "cyan", "thememagenta" => "magenta",
            _ => return None,
        };
        let Some(client) = self.client.and_then(|id| self.srv.clients.get(&id)) else {
            return format::theme_color(name, Theme::Terminal).map(colour_name);
        };
        let theme = match self.srv.opt_server("theme").as_str() {
            "terminal" => return format::theme_color(name, Theme::Terminal).map(colour_name),
            "light" => Theme::Light,
            "dark" => Theme::Dark,
            _ => client.terminal.detected().unwrap_or(Theme::Dark),
        };
        let option = format!("{}-theme-{suffix}", if theme == Theme::Light { "light" } else { "dark" });
        // Palette values expand with jobs disabled and cannot recurse via colour modifiers.
        Some(format::expand(&self.srv.opt_server(&option), &PaletteVars(self)))
    }
}

struct PaletteVars<'a, 'b>(&'a VarCtx<'b>);
impl format::Vars for PaletteVars<'_, '_> {
    fn get(&self, name: &str) -> Option<String> { self.0.get(name) }
    fn theme_color(&self, _name: &str) -> Option<Color> { Some(Color::Default) }
    fn environ(&self, name: &str) -> Option<String> { self.0.environ(name) }
}

/// Current defaults for a pane, plus its OSC 10/11 palette and control reports.
#[derive(Clone, Copy, Default)]
pub struct PaneColours {
    terminal: Terminal,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    control: Terminal,
    last: Option<Theme>,
}

impl PaneColours {
    pub fn update(&mut self, terminal: Terminal) { self.terminal = terminal; }
    fn theme(&self) -> Option<Theme> {
        self.terminal.reported.or_else(|| self.terminal.control_bg.or(self.bg).or(self.terminal.bg).and_then(background_theme))
    }
    pub fn subscribe(&mut self) { self.last = self.theme(); }
    pub fn report(&mut self, replies: &mut Vec<u8>) {
        self.last = self.theme();
        match self.last {
            Some(Theme::Light) => replies.extend(b"\x1b[?997;2n"),
            Some(Theme::Dark) => replies.extend(b"\x1b[?997;1n"),
            _ => {}
        }
    }
    pub fn notify(&mut self, enabled: bool, replies: &mut Vec<u8>) {
        if enabled && self.last != self.theme() { self.report(replies); }
    }
    pub fn osc(&mut self, params: &[&[u8]], bel: bool, replies: &mut Vec<u8>) {
        match params {
            [b"110"] | [b"110", b""] => { self.fg = None; return },
            [b"111"] | [b"111", b""] => { self.bg = None; return },
            _ => {}
        }
        let [n, value, ..] = params else { return };
        let fg = match *n {
            b"10" => true, b"11" => false,
            b"110" if value.is_empty() => { self.fg = None; return },
            b"111" if value.is_empty() => { self.bg = None; return },
            _ => return,
        };
        if *value == b"?" {
            let colour = if fg { self.terminal.control_fg.or(self.fg).or(self.terminal.fg) } else { self.terminal.control_bg.or(self.bg).or(self.terminal.bg) };
            if let Some((r, g, b)) = colour.and_then(rgb) {
                let n = if fg { 10 } else { 11 };
                let end = if bel { "\x07" } else { "\x1b\\" };
                replies.extend(format!("\x1b]{n};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{end}").bytes());
            }
        } else if let Ok(value) = std::str::from_utf8(value) {
            if let Some(colour) = x11(value) {
                if fg { self.fg = Some(colour) } else { self.bg = Some(colour) }
            }
        }
    }
}

impl Server {
    pub(super) fn terminal_report(&mut self, cid: ClientId, raw: &[u8]) -> bool {
        let reported = match raw {
            b"\x1b[?997;1n" => Some(Theme::Dark),
            b"\x1b[?997;2n" => Some(Theme::Light),
            _ => None,
        };
        let colour = colour_report(raw);
        if reported.is_none() && colour.is_none() { return false; }
        let Some(c) = self.clients.get_mut(&cid) else { return true };
        if c.denied { return true; }
        if let Some(t) = reported {
            c.terminal.reported = Some(t);
            c.terminal.pending = Some(Instant::now());
            // tmux waits for colour replies before forwarding the theme notification.
            c.send(S_OUTPUT, b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\");
        }
        if let Some((fg, colour)) = colour {
            if fg { c.terminal.fg = Some(colour) } else { c.terminal.bg = Some(colour); c.terminal.pending = None; }
        }
        c.prev = None;
        c.dirty = true;
        let sid = c.session;
        if let Some(t) = reported {
            self.run_hook(sid, if t == Theme::Light { "client-light-theme" } else { "client-dark-theme" }, Ctx { client: Some(cid), ..Ctx::default() });
        }
        if colour.is_some() { self.notify_pane_themes(); }
        true
    }

    fn pane_clients(&self, pane: PaneId) -> impl Iterator<Item = &Client> {
        let wid = self.window_of(pane);
        self.clients.values().filter(move |c| {
            c.attached && c.control == 0 && (wid.is_some_and(|w| c.session.and_then(|s| self.sessions.get(&s)).is_some_and(|s| s.links.values().any(|id| *id == w)))
                || matches!(&c.overlay, Some(Overlay::Popup(p)) if p.pane == pane))
        })
    }

    pub(super) fn pane_colours(&self, pane: PaneId) -> Terminal {
        let mut terminal = Terminal::default();
        let mut light = false;
        let mut dark = false;
        for c in self.pane_clients(pane) {
            terminal.fg = terminal.fg.or(c.terminal.fg);
            terminal.bg = terminal.bg.or(c.terminal.bg);
            light |= c.terminal.reported == Some(Theme::Light);
            dark |= c.terminal.reported == Some(Theme::Dark);
        }
        terminal.reported = match (light, dark) {
            (true, false) => Some(Theme::Light), (false, true) => Some(Theme::Dark), _ => None,
        };
        if self.panes.contains_key(&pane) {
            let client = self.pane_clients(pane).next().map(|c| c.id);
            let ctx = VarCtx { srv: self, t: self.target_for_pane(pane), client, extra: HashMap::new() };
            let style = |name| format::parse_style_with(&self.opt_pane(pane, name), crate::cells::Style::default(), Some(&ctx)).ok().map(|p| p.style).unwrap_or_default();
            let mut defaults = style("window-style");
            if self.window_of(pane).is_some_and(|w| self.windows[&w].active == pane) {
                let active = style("window-active-style");
                if active.fg != Color::Default { defaults.fg = active.fg; }
                if active.bg != Color::Default { defaults.bg = active.bg; }
            }
            if defaults.fg != Color::Default { terminal.fg = Some(defaults.fg); }
            if defaults.bg != Color::Default { terminal.bg = Some(defaults.bg); }
        }
        if let Some(p) = self.panes.get(&pane) {
            let control = p.parser.callbacks().colours.control;
            if self.clients.values().any(|c| c.control > 0) {
                terminal.control_fg = control.fg;
                terminal.control_bg = control.bg;
            }
        }
        terminal
    }

    pub(super) fn notify_pane_themes(&mut self) {
        let panes: Vec<_> = self.panes.keys().copied().collect();
        for id in panes {
            if self.pane_clients(id).any(|c| c.terminal.pending.is_some_and(|t| t.elapsed() < Duration::from_secs(1))) { continue; }
            let terminal = self.pane_colours(id);
            let p = self.panes.get_mut(&id).unwrap();
            let enabled = p.screen().theme_updates();
            let cb = p.parser.callbacks_mut();
            cb.colours.update(terminal);
            let mut replies = Vec::new();
            cb.colours.notify(enabled, &mut replies);
            p.send(replies);
        }
    }

    pub(super) fn theme_control_report(&mut self, value: &str) {
        let Some((pane, report)) = value.split_once(':') else { return };
        let Some(id) = pane.strip_prefix('%').and_then(|p| p.parse::<PaneId>().ok()) else { return };
        let Some((fg, c)) = colour_report(report.as_bytes()) else { return };
        let Some(p) = self.panes.get_mut(&id) else { return };
        let control = &mut p.parser.callbacks_mut().colours.control;
        if fg { control.fg = Some(c) } else { control.bg = Some(c) }
        self.notify_pane_themes();
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Vars;
    use super::super::vars::Target;

    fn server() -> Server {
        let (tx, _) = mpsc::sync_channel(64);
        let mut srv = Server::new(tx);
        let (output, _) = mpsc::channel();
        srv.clients.insert(1, Client {
            id: 1,
            tx: output,
            ident: None,
            session: None,
            attached: false,
            readonly: false,
            size: (80, 24),
            key_table: "root".into(),
            repeat_until: None,
            keyp: keys::Parser::new(),
            esc_deadline: None,
            prompt: None,
            overlay: None,
            overlay_epoch: 0,
            message: None,
            prev: None,
            dirty: false,
            last_session: None,
            created: now_secs(),
            activity: now_secs(),
            drag: None,
            title: String::new(),
            suspended: false,
            click: Default::default(),
            mouse_last: (0, 0, 0),
            mouse_drag_flag: 0,
            status_ranges: Vec::new(),
            control: 0,
            control_size: None,
            control_seq: 0,
            seen: Default::default(),
            features: Default::default(),
            terminal: Default::default(),
            pending: Default::default(),
            queued_bytes: Default::default(),
            closed: Default::default(),
            backlog_since: Default::default(),
            ctl: Default::default(),
            user: None,
            denied: false,
            acl_readonly: false,
            io: Default::default(),
        });
        let c = srv.clients.get_mut(&1).unwrap();
        c.attached = true;
        c.features = features::compute("xterm-256color", "", &[], "");
        srv
    }
    fn ctx(srv: &Server) -> VarCtx<'_> {
        VarCtx { srv, t: Target::default(), client: Some(1), extra: HashMap::new() }
    }
    #[test]
    fn theme_resolution_options_and_client_depth() {
        let mut srv = server();
        assert_eq!(ctx(&srv).theme_color("themegreen"), format::parse_color("yellowgreen"));
        srv.clients.get_mut(&1).unwrap().terminal.bg = Some(Color::Rgb(255, 255, 255));
        assert_eq!(ctx(&srv).theme_color("themegreen"), format::parse_color("seagreen"));
        assert_eq!(ctx(&srv).get("client_theme"), None);
        srv.clients.get_mut(&1).unwrap().terminal.reported = Some(Theme::Dark);
        assert_eq!(ctx(&srv).get("client_theme"), Some("dark".into()));
        assert_eq!(ctx(&srv).theme_color("themegreen"), format::parse_color("yellowgreen"));
        srv.opts_server.set("theme", "light".into());
        assert_eq!(ctx(&srv).theme_color("themegreen"), format::parse_color("seagreen"));
        srv.opts_server.set("light-theme-green", "#{?#{==:#{client_theme},dark},#123456,red}".into());
        assert_eq!(ctx(&srv).theme_color("themegreen"), Some(Color::Rgb(18, 52, 86)));
        srv.opts_server.set("light-theme-green", "themeblue".into());
        assert_eq!(ctx(&srv).theme_color("themegreen"), Some(Color::Default));
        srv.opts_server.set("theme", "terminal".into());
        assert_eq!(ctx(&srv).theme_color("themegreen"), Some(Color::Idx(2)));
        assert_eq!(format::expand("#{c/f:themegreen}", &ctx(&srv)), "\x1b[32m");
        let spans = format::spans_with("#[fg=themegreen]x", crate::cells::Style::default(), Some(&ctx(&srv)));
        assert_eq!(spans[0].style.fg, Color::Idx(2));
        for (term, extra, depth) in [("dumb", "", "2"), ("vt100", "", "8"), ("xterm-16color", "", "16"), ("xterm-256color", "", "256"), ("xterm", "RGB", "16777216")] {
            srv.clients.get_mut(&1).unwrap().features = features::compute(term, "", &[], extra);
            assert_eq!(ctx(&srv).get("client_colours"), Some(depth.into()));
            srv.opts_server.set("theme", "dark".into());
            assert_eq!(ctx(&srv).theme_color("themeblack"), if depth.parse::<u32>().unwrap() >= 256 { format::parse_color("gray5") } else { Some(Color::Idx(0)) });
        }
    }

    #[test]
    fn background_detection_matches_tmux_threshold() {
        assert_eq!(background_theme(Color::Default), None);
        assert_eq!(background_theme(Color::Rgb(127, 127, 128)), Some(Theme::Dark));
        assert_eq!(background_theme(Color::Rgb(127, 128, 128)), Some(Theme::Light));
        assert_eq!(background_theme(Color::Idx(7)), Some(Theme::Light));
        assert_eq!(background_theme(Color::Idx(8)), Some(Theme::Dark));
        assert_eq!(background_theme(Color::Idx(232)), Some(Theme::Dark));
        assert_eq!(background_theme(Color::Idx(255)), Some(Theme::Light));
    }

    #[test]
    fn terminal_replies_are_consumed_even_for_readonly_clients() {
        let mut srv = server();
        srv.clients.get_mut(&1).unwrap().readonly = true;
        for byte in b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\" { srv.handle_input(1, &[*byte]); }
        assert_eq!(srv.clients[&1].terminal.detected(), Some(Theme::Light));
        assert_eq!(ctx(&srv).get("client_theme"), None);
        for byte in b"\x1b[?997;1n" { srv.handle_input(1, &[*byte]); }
        assert_eq!(ctx(&srv).get("client_theme"), Some("dark".into()));
        assert_eq!(srv.clients[&1].terminal.detected(), Some(Theme::Dark));
        assert!(srv.terminal_report(1, b"\x1b]10;rgb:12/34/56\x07"));
        assert_eq!(srv.clients[&1].terminal.fg, Some(Color::Rgb(18, 52, 86)));
        assert!(!srv.terminal_report(1, b"\x1b]11;rgb:zz/ff/00\x07"));
    }

    #[test]
    fn pane_queries_and_notifications_preserve_terminal_theme() {
        let nz = |n| NonZeroU16::new(n).unwrap();
        let mut parser = vt100::Parser::new_with_callbacks(nz(24), nz(80), 10, PaneCb::default());
        parser.callbacks_mut().colours.update(Terminal { reported: Some(Theme::Light), fg: Some(Color::Rgb(1, 2, 3)), bg: Some(Color::Rgb(255, 255, 255)), ..Terminal::default() });
        parser.process(b"\x1b[?25;2031h");
        assert!(parser.callbacks().replies.is_empty());
        parser.process(b"\x1b[?996n\x1b]10;?\x07\x1b]11;?\x1b\\");
        assert_eq!(parser.callbacks().replies, b"\x1b[?997;2n\x1b]10;rgb:0101/0202/0303\x07\x1b]11;rgb:ffff/ffff/ffff\x1b\\");
        parser.callbacks_mut().replies.clear();
        // A pane palette cannot override the terminal's explicit theme.
        parser.process(b"\x1b]11;#000000\x07\x1b[?996n");
        assert_eq!(parser.callbacks().replies, b"\x1b[?997;2n");
        parser.callbacks_mut().replies.clear();
        parser.process(b"\x1b[?2031$p");
        assert_eq!(parser.callbacks().replies, b"\x1b[?2031;1$y");
        let colours = &mut parser.callbacks_mut().colours;
        let mut replies = Vec::new();
        colours.update(Terminal { reported: Some(Theme::Dark), ..Terminal::default() });
        colours.notify(true, &mut replies);
        colours.notify(true, &mut replies);
        assert_eq!(replies, b"\x1b[?997;1n");
    }
}
