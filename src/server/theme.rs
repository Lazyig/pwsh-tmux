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
    control_fg: Option<Color>,
    control_bg: Option<Color>,
    /// TERM_VT100LIKE: only these terminals are asked for their theme and colours.
    pub vt100: bool,
    last_requests: Option<Instant>,
    /// A popup's pane: tmux runs it without a window pane, so it gets no theme or colour replies.
    popup: bool,
    /// Effective pane defaults, including active-style and pane palette precedence.
    style_fg: StyleColour,
    style_bg: StyleColour,
    active_style: Option<(StyleColour, StyleColour)>,
    /// TTY_HAVEDA, TTY_HAVEDA2, TTY_HAVEXDA, TTY_HAVESYNC: the replies received, or no
    /// longer waited for.
    have: u8,
    /// When the requests were sent (tty_start_start_timer).
    started: Option<Instant>,
}

/// The request flags of Terminal::have (TTY_ALL_REQUEST_FLAGS is all of them).
pub const HAVE_DA: u8 = 1;
pub const HAVE_DA2: u8 = 2;
pub const HAVE_XDA: u8 = 4;
pub const HAVE_SYNC: u8 = 8;
const HAVE_ALL: u8 = HAVE_DA | HAVE_DA2 | HAVE_XDA | HAVE_SYNC;

/// tmux TTY_QUERY_TIMEOUT: how long replies to the requests are waited for.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// tmux TTY_REQUEST_LIMIT: colour requests are repeated (on resize) at most this often.
const REQUEST_LIMIT: Duration = Duration::from_secs(30);

impl Terminal {
    pub fn detected(self) -> Option<Theme> {
        self.reported.or_else(|| self.bg.and_then(background_theme))
    }
    /// tty_start_tty + tty_send_requests: subscribe to theme changes and ask for the theme,
    /// the primary, secondary and extended device attributes and synchronized output
    /// (DECRQM 2026) not answered yet, and the colours (VT100-like terminals only).
    pub fn start(&mut self) -> Option<Vec<u8>> {
        if !self.vt100 {
            return None;
        }
        let mut out = b"\x1b[?2031h\x1b[?996n".to_vec();
        for (bit, q) in [(HAVE_DA, &b"\x1b[c"[..]), (HAVE_DA2, b"\x1b[>c"), (HAVE_XDA, b"\x1b[>q"), (HAVE_SYNC, b"\x1b[?2026$p")] {
            if self.have & bit == 0 {
                out.extend(q);
            }
        }
        out.extend(self.requests(true)?);
        self.started = Some(Instant::now());
        Some(out)
    }
    /// A reply of this kind is still taken: not received yet, and the start timer has
    /// not fired (tty_start_timer_callback stops waiting for all of them).
    pub fn waiting(&mut self, bit: u8) -> bool {
        if self.started.is_some_and(|t| t.elapsed() > QUERY_TIMEOUT) {
            self.have = HAVE_ALL;
        }
        self.have & bit == 0
    }
    pub fn received(&mut self, bit: u8) {
        self.have |= bit;
    }
    /// tty_repeat_requests: ask for the colours again, unless asked within REQUEST_LIMIT.
    pub fn requests(&mut self, force: bool) -> Option<&'static [u8]> {
        if !self.vt100 || (!force && self.last_requests.is_some_and(|t| t.elapsed() <= REQUEST_LIMIT)) {
            return None;
        }
        self.last_requests = Some(Instant::now());
        Some(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\")
    }
}

/// TERM_VT100LIKE (tty_term_create): terminfo has XT or clear starts with CSI. Without
/// terminfo, clear is taken as CSI for every TERM but dumb and XT for xterm-like names;
/// terminal-overrides entries for the TERM may change either.
pub fn vt100_like(term: &str, overrides: &[String]) -> bool {
    if term.is_empty() {
        return true; // Windows console
    }
    const XT: [&str; 8] = ["xterm", "rxvt", "st", "tmux", "alacritty", "foot", "kitty", "wezterm"];
    let mut xt = XT.iter().any(|p| term.starts_with(p));
    let mut clear_csi = term != "dumb";
    for entry in overrides {
        let mut caps = entry.split(':');
        if !caps.next().is_some_and(|pat| !pat.is_empty() && features::fnmatch(pat, term)) {
            continue;
        }
        for cap in caps {
            match cap {
                "XT" => xt = true,
                "XT@" => xt = false,
                "clear@" => clear_csi = false,
                _ => if let Some(v) = cap.strip_prefix("clear=") {
                    clear_csi = ["\\E[", "\\e[", "\\033[", "^[[", "\x1b["].iter().any(|csi| v.starts_with(csi));
                },
            }
        }
    }
    xt || clear_csi
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
        Color::Idx(i) | Color::Palette(i) => Some(features::idx_to_rgb(i)),
    }
}

fn x11(s: &str) -> Option<Color> {
    let c = format::parse_color(&parse_x11_colour(s)?)?;
    (c != Color::Default).then_some(c)
}

/// A complete outer-terminal colour reply (tty_keys_colours): ESC ] 10 or 11 ; up to BEL or ST.
fn is_colour_reply(raw: &[u8]) -> bool {
    (raw.starts_with(b"\x1b]10;") || raw.starts_with(b"\x1b]11;")) && (raw.ends_with(b"\x07") || (raw.len() > 6 && raw.ends_with(b"\x1b\\")))
}

/// The colour of a complete reply, if it parses (BEL and ST are both accepted).
fn colour_report(raw: &[u8]) -> Option<(bool, Color)> {
    let body = raw.strip_prefix(b"\x1b]")?;
    let body = body.strip_suffix(b"\x1b\\").or_else(|| body.strip_suffix(b"\x07"))?;
    let s = std::str::from_utf8(body).ok()?;
    let (n, value) = s.split_once(';')?;
    Some((match n { "10" => true, "11" => false, _ => return None }, x11(value)?))
}

/// Theme names in a style parsed without a client: probe colours stand in for them so they
/// can be told apart from real colours, as tmux keeps COLOUR_FLAG_THEME for them.
struct ThemeProbe<'a>(&'a dyn format::Vars, u8);
impl format::Vars for ThemeProbe<'_> {
    fn get(&self, name: &str) -> Option<String> { self.0.get(name) }
    fn theme_color(&self, name: &str) -> Option<Color> { Some(Color::Rgb(self.1, 0xa5, format::theme_index(name)? as u8)) }
    fn environ(&self, name: &str) -> Option<String> { self.0.environ(name) }
}

/// A style's default foreground or background, with theme colours left unresolved.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum StyleColour {
    #[default]
    Default,
    Theme(usize),
    Terminal,
    Colour(Color),
}

impl StyleColour {
    /// colour_tostring: theme colours print their name.
    pub fn name(self) -> String {
        match self {
            StyleColour::Default => "default".into(),
            StyleColour::Terminal => "terminal".into(),
            StyleColour::Theme(i) => format::theme_name(i).into(),
            StyleColour::Colour(c) => colour_name(c),
        }
    }
    /// The colour a pane reports for OSC 10/11 and its theme: None for a theme colour, which
    /// has no RGB value (colour_force_rgb fails) and no theme (colour_totheme).
    fn or_else(self, fallback: impl FnOnce() -> Option<Color>) -> Option<Color> {
        match self { StyleColour::Default => fallback(), StyleColour::Theme(_) => None, StyleColour::Terminal => fallback(), StyleColour::Colour(c) => Some(c) }
    }
}

/// The fg and bg of a style option value, parsed as tmux does without a client.
pub fn style_colours(value: &str, vars: &dyn format::Vars) -> (StyleColour, StyleColour) {
    let value = format::expand(value, vars);
    let mut fg_name = None;
    let mut bg_name = None;
    for word in value.split([',', ' ']) {
        if word.eq_ignore_ascii_case("default") { fg_name = None; bg_name = None; }
        if let Some((key, val)) = word.split_once('=') {
            let val = val.to_ascii_lowercase();
            let identity = (val == "terminal").then_some(StyleColour::Terminal);
            if key == "fg" { fg_name = identity; }
            if key == "bg" { bg_name = identity; }
        }
    }
    let parse = |salt| format::parse_style_with(&value, crate::cells::Style::default(), Some(&ThemeProbe(vars, salt))).map(|p| p.style).unwrap_or_default();
    let (a, b) = (parse(1), parse(2));
    let one = |x: Color, y: Color| match (x, y) {
        (Color::Rgb(1, 0xa5, i), Color::Rgb(2, 0xa5, j)) if i == j => StyleColour::Theme(i as usize),
        (Color::Default, _) => StyleColour::Default,
        _ => StyleColour::Colour(x),
    };
    (fg_name.unwrap_or_else(|| one(a.fg, b.fg)), bg_name.unwrap_or_else(|| one(a.bg, b.bg)))
}

impl VarCtx<'_> {
    /// colour_toescape for #{c/f:} and #{c/b:}: a theme name is the client's palette colour
    /// (the default colour if that is itself a theme name or bad), and the escape is never
    /// reduced to what the terminal supports (tmux's downgrade tests mix TERM_ and COLOUR_
    /// flags and are always false).
    pub(super) fn client_colour_escape(&self, name: &str, background: bool) -> Option<String> {
        self.client.and_then(|id| self.srv.clients.get(&id))?;
        let lower = name.to_ascii_lowercase();
        let (c, value) = match self.theme_value(&lower) {
            Some(v) if format::theme_index(&v).is_some() => (Color::Default, v),
            Some(v) => (format::parse_color(&v).unwrap_or(Color::Default), v),
            None => (format::parse_color(&lower)?, lower),
        };
        let palette = value.to_ascii_lowercase().starts_with("colo");
        let base = if background { 40 } else { 30 };
        Some(match c {
            Color::Default => format!("\x1b[{}m", base + 9),
            Color::Rgb(r, g, b) => format!("\x1b[{};2;{r};{g};{b}m", base + 8),
            Color::Palette(n) => format!("\x1b[{};5;{n}m", base + 8),
            Color::Idx(n) if palette || n >= 16 => format!("\x1b[{};5;{n}m", base + 8),
            Color::Idx(n) if n < 8 => format!("\x1b[{}m", base + n as u32),
            Color::Idx(n) => format!("\x1b[{}m", base + 60 + n as u32 - 8),
        })
    }

    /// The colours theme names resolve to for this context's client, in theme_index order.
    pub(super) fn theme_palette(&self) -> [Color; 10] {
        std::array::from_fn(|i| self.resolve_theme_colour(format::theme_name(i)).unwrap_or(Color::Default))
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
        let client_only = VarCtx { srv: self.srv, t: Default::default(), client: self.client, extra: HashMap::new() };
        Some(format::expand(&self.srv.opt_server(&option), &PaletteVars(&client_only)))
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
    pub fn reset(&mut self) { self.fg = None; self.bg = None; }
    pub fn update(&mut self, terminal: Terminal) { self.terminal = terminal; }
    /// window_pane_get_fg/bg: control report, effective pane defaults, then client colours.
    fn colour(&self, fg: bool) -> Option<Color> {
        let t = &self.terminal;
        let (control, style, own, client) = if fg { (t.control_fg, t.style_fg, self.fg, t.fg) }
            else { (t.control_bg, t.style_bg, self.bg, t.bg) };
        let resolve = |style: StyleColour, fallback: Option<Color>| {
            if style == StyleColour::Terminal { client } else { style.or_else(|| fallback) }
        };
        let base = || resolve(style, own.or(client));
        control.or_else(|| match t.active_style {
            Some((afg, abg)) => resolve(if fg { afg } else { abg }, own.or_else(base)),
            None => base(),
        })
    }
    /// window_pane_get_theme: the attached clients' agreed theme, else the background's.
    fn theme(&self) -> Option<Theme> {
        self.terminal.reported.or_else(|| self.colour(false).and_then(background_theme))
    }
    pub fn subscribe(&mut self) { self.last = self.theme(); }
    pub fn report(&mut self, replies: &mut Vec<u8>) {
        if self.terminal.popup { return; }
        self.last = self.theme();
        match self.last {
            Some(Theme::Light) => replies.extend(b"\x1b[?997;2n"),
            Some(Theme::Dark) => replies.extend(b"\x1b[?997;1n"),
            _ => {}
        }
    }
    pub fn notify(&mut self, enabled: bool, replies: &mut Vec<u8>) {
        if enabled && !self.terminal.popup && self.last != self.theme() { self.report(replies); }
    }
    pub fn osc(&mut self, params: &[&[u8]], bel: bool, replies: &mut Vec<u8>) {
        match params {
            [b"104"] | [b"104", b""] => { self.reset(); return },
            [b"110"] | [b"110", b""] => { self.fg = None; return },
            [b"111"] | [b"111", b""] => { self.bg = None; return },
            _ => {}
        }
        let [n, value] = params else { return };
        let fg = match *n {
            b"10" => true, b"11" => false,
            b"110" if value.is_empty() => { self.fg = None; return },
            b"111" if value.is_empty() => { self.bg = None; return },
            _ => return,
        };
        if *value == b"?" {
            if self.terminal.popup { return; }
            if let Some((r, g, b)) = self.colour(fg).and_then(rgb) {
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
        if let Some(c) = self.clients.get_mut(&cid) {
            // tty_keys_device_attributes2 and _extended: once per terminal
            let bit = if raw.starts_with(b"\x1bP>|") { HAVE_XDA } else if raw.starts_with(b"\x1b[>") { HAVE_DA2 } else { 0 };
            if bit != 0 && !c.denied && c.terminal.waiting(bit) && c.features.device_report(raw) {
                c.terminal.received(bit);
                if let Some(body) = raw.strip_prefix(b"\x1bP>|").and_then(|s| s.strip_suffix(b"\x1b\\")) {
                    c.term_type = String::from_utf8_lossy(body).into_owned();
                }
                c.prev = None;
                c.dirty = true;
                return true;
            }
            // tty_keys_device_attributes: VT420 and later levels list features
            if raw.starts_with(b"\x1b[?") && raw.ends_with(b"c") && c.terminal.waiting(HAVE_DA) {
                if let Ok(body) = std::str::from_utf8(&raw[3..raw.len() - 1]) {
                    if body.bytes().all(|b| b.is_ascii_digit() || b == b';') {
                        let p: Vec<u32> = body.split(';').take(32).map(|n| n.parse().unwrap_or(0)).collect();
                        if (61..=65).contains(&p[0]) {
                            for n in &p[1..] {
                                let f = match n { 4 => "sixel", 21 => "margins", 28 => "rectfill", 52 => "clipboard", _ => continue };
                                c.features.parse(f, &[',']);
                            }
                        }
                        c.terminal.received(HAVE_DA);
                        c.prev = None;
                        c.dirty = true;
                        return true;
                    }
                }
            }
            // tty_keys_sync: the DECRPM reply to DECRQM 2026
            if let Some(status) = raw.strip_prefix(b"\x1b[?2026;").and_then(|r| r.strip_suffix(b"$y")).filter(|r| r.len() == 1 && (b'0'..=b'4').contains(&r[0])) {
                if c.terminal.waiting(HAVE_SYNC) {
                    if matches!(status[0], b'1'..=b'3') && !c.features.has("sync") {
                        c.features.parse("sync", &[',']);
                        c.prev = None;
                        c.dirty = true;
                    }
                    c.terminal.received(HAVE_SYNC);
                    return true;
                }
            }
        }
        let reported = match raw {
            b"\x1b[?997;1n" => Some(Theme::Dark),
            b"\x1b[?997;2n" => Some(Theme::Light),
            _ => None,
        };
        // tty_keys_colours consumes any complete reply, even one whose colour does not parse.
        if reported.is_none() && !is_colour_reply(raw) { return false; }
        let colour = colour_report(raw);
        let Some(c) = self.clients.get_mut(&cid) else { return true };
        if c.denied { return true; }
        if let Some(t) = reported {
            c.terminal.reported = Some(t);
            // Requery colours, but explicit theme reports notify panes immediately.
            if let Some(q) = c.terminal.requests(true) { c.send(S_OUTPUT, q); }
        }
        if let Some((fg, colour)) = colour {
            if fg { c.terminal.fg = Some(colour) } else { c.terminal.bg = Some(colour); }
        }
        c.prev = None;
        c.dirty = true;
        if let Some(t) = reported {
            self.fire_client(if t == Theme::Light { "client-light-theme" } else { "client-dark-theme" }, cid);
        }
        if colour.is_some() || reported.is_some() { self.notify_pane_themes(); }
        true
    }

    /// The theme and colour queries for a client starting on its terminal.
    pub(super) fn start_terminal_queries(&mut self, cid: ClientId) {
        if let Some(c) = self.clients.get_mut(&cid).filter(|c| c.control == 0) {
            if let Some(q) = c.terminal.start() { c.send(S_OUTPUT, &q); }
        }
    }

    /// MSG_RESIZE: tty_repeat_requests without force.
    pub(super) fn repeat_terminal_queries(&mut self, cid: ClientId) {
        if let Some(c) = self.clients.get_mut(&cid).filter(|c| c.control == 0 && c.attached) {
            if let Some(q) = c.terminal.requests(false) { c.send(S_OUTPUT, q); }
        }
    }

    /// Clients whose terminal colours and theme a pane follows: attached clients on a session
    /// with its window (window_pane_get_theme). Popups have no window pane in tmux.
    pub(super) fn pane_clients(&self, pane: PaneId) -> impl Iterator<Item = &Client> {
        let wid = self.window_of(pane);
        let mut clients: Vec<_> = self.clients.values().collect();
        clients.sort_by_key(|c| c.id);
        clients.into_iter().filter(move |c| {
            c.attached && c.control == 0 && wid.is_some_and(|w| c.session.and_then(|s| self.sessions.get(&s)).is_some_and(|s| s.links.values().any(|id| *id == w)))
        })
    }

    /// The window-style / window-active-style default colours of a pane (tty_default_colours),
    /// parsed without a client so theme colours stay unresolved.
    pub(super) fn pane_style_colours(&self, pane: PaneId) -> (StyleColour, StyleColour) {
        let ctx = VarCtx { srv: self, t: self.target_for_pane(pane), client: None, extra: HashMap::new() };
        let (mut fg, mut bg) = style_colours(&self.opt_pane(pane, "window-style"), &ctx);
        if self.window_of(pane).is_some_and(|w| self.windows[&w].active == pane) {
            if let Some(p) = self.panes.get(&pane) {
                let own = &p.parser.callbacks().colours;
                if let Some(c) = own.fg { fg = StyleColour::Colour(c); }
                if let Some(c) = own.bg { bg = StyleColour::Colour(c); }
            }
            let (afg, abg) = style_colours(&self.opt_pane(pane, "window-active-style"), &ctx);
            if afg != StyleColour::Default { fg = afg; }
            if abg != StyleColour::Default { bg = abg; }
        }
        (fg, bg)
    }

    pub(super) fn pane_colours(&self, pane: PaneId) -> Terminal {
        let mut terminal = Terminal::default();
        if self.window_of(pane).is_none() {
            terminal.popup = true;
            return terminal;
        }
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
            let ctx = VarCtx { srv: self, t: self.target_for_pane(pane), client: None, extra: HashMap::new() };
            (terminal.style_fg, terminal.style_bg) = style_colours(&self.opt_pane(pane, "window-style"), &ctx);
            if self.window_of(pane).is_some_and(|w| self.windows[&w].active == pane) {
                terminal.active_style = Some(style_colours(&self.opt_pane(pane, "window-active-style"), &ctx));
            }
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
            input_state: Default::default(),
            term_caps: Default::default(),
            term_type: String::new(),
            esc_deadline: None,
            prompt: None,
            overlay: None,
            overlay_epoch: 0,
            message: None,
            message_ignore_keys: false,
            message_ignore_styles: true,
            slider_mpos: 0,
            prev: None,
            dirty: false,
            last_session: None,
            created: now_secs(),
            activity: now_secs(),
            activity_seq: 0,
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
            features: Default::default(),
            clipboard_query: None,
            focused: false,
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
            pan: None,
        });
        let c = srv.clients.get_mut(&1).unwrap();
        c.attached = true;
        // the 256 feature, as DA replies or -2 add it (terminfo colors#256 alone is 16)
        c.features = features::compute("xterm-256color", "", &[], "256");
        srv
    }
    fn ctx(srv: &Server) -> VarCtx<'_> {
        VarCtx { srv, t: Target::default(), client: Some(1), extra: HashMap::new() }
    }

    fn with_pane() -> (Server, PaneId) {
        let mut srv = server();
        srv.exec(["new-session", "-d", "-s", "a", "sleep 30"].map(String::from).to_vec(), &mut Ctx::default()).unwrap();
        let sid = srv.sessions.values().find(|s| s.name == "a").unwrap().id;
        let pane = srv.windows[&srv.sessions[&sid].active_window()].active;
        srv.clients.get_mut(&1).unwrap().session = Some(sid);
        (srv, pane)
    }
    fn command(srv: &mut Server, args: &[&str]) {
        srv.exec(args.iter().map(|s| s.to_string()).collect(), &mut Ctx { client: Some(1), ..Ctx::default() }).unwrap();
    }

    #[test]
    fn osc_colours_reset_and_reject_extra_arguments() {
        let nz = |n| NonZeroU16::new(n).unwrap();
        let mut parser = vt100::Parser::new_with_callbacks(nz(24), nz(80), 10, PaneCb::default());
        for reset in [b"\x1bc".as_slice(), b"\x1b]104\x07", b"\x1b]104;\x07"] {
            parser.process(b"\x1b]10;#ff0000\x07\x1b]11;#00ff00\x07");
            assert!(parser.callbacks().colours.fg.is_some());
            parser.process(reset);
            assert_eq!((parser.callbacks().colours.fg, parser.callbacks().colours.bg), (None, None));
        }
        parser.process(b"\x1b]10;#ff0000;#00ff00\x07\x1b]11;?;?\x07");
        assert_eq!(parser.callbacks().colours.fg, None);
        assert!(parser.callbacks().replies.is_empty());
        let (mut srv, pane) = with_pane();
        srv.panes.get_mut(&pane).unwrap().parser.process(b"\x1b]10;#ff0000\x07");
        command(&mut srv, &["send-keys", "-R"]);
        assert_eq!(srv.expand_for_pane(pane, "#{pane_fg}"), "default");
        srv.shutdown();
    }

    #[test]
    fn active_palette_precedes_window_style_in_formats_replies_and_clock() {
        let (mut srv, pane) = with_pane();
        command(&mut srv, &["set", "-g", "window-style", "fg=colour100,bg=#102030"]);
        srv.panes.get_mut(&pane).unwrap().parser.process(b"\x1b]10;#ff0000\x07\x1b]11;#00ff00\x07");
        assert_eq!(srv.expand_for_pane(pane, "#{pane_fg} #{pane_bg}"), "#ff0000 #00ff00");
        let defaults = srv.pane_colours(pane);
        let cb = srv.panes.get_mut(&pane).unwrap().parser.callbacks_mut();
        cb.colours.update(defaults);
        cb.colours.osc(&[b"11", b"?"], true, &mut cb.replies);
        assert_eq!(cb.replies, b"\x1b]11;rgb:0000/ffff/0000\x07");
        assert_eq!(srv.compose(1).unwrap().frame.get(0, 0).unwrap().style.bg, Color::Rgb(0, 255, 0));
        command(&mut srv, &["clock-mode"]);
        assert_eq!(srv.compose(1).unwrap().frame.get(0, 0).unwrap().style.bg, Color::Rgb(0, 255, 0));
        command(&mut srv, &["set", "-g", "window-active-style", "bg=blue"]);
        assert_eq!(srv.expand_for_pane(pane, "#{pane_bg}"), "blue");
        srv.shutdown();
    }

    #[test]
    fn palette_values_use_only_client_context() {
        // server_client_update_theme_colours: format_defaults with only the client, so the
        // client's session (not the pane being drawn) fills in session_name (red on tmux 3.8)
        let (mut srv, pane) = with_pane();
        srv.opts_server.set("dark-theme-green", "#{?session_name,red,#{?window_name,green,#{?pane_index,white,#123456}}}".into());
        let vars = VarCtx { srv: &srv, t: srv.target_for_pane(pane), client: Some(1), extra: HashMap::new() };
        assert_eq!(vars.theme_color("themegreen"), Some(Color::Idx(1)));
        assert_eq!(format::expand("#{c/f:themegreen}", &vars), "\x1b[31m");
        // without a session the nested conditions all fall through
        srv.clients.get_mut(&1).unwrap().session = None;
        let vars = VarCtx { srv: &srv, t: srv.target_for_pane(pane), client: Some(1), extra: HashMap::new() };
        assert_eq!(vars.theme_color("themegreen"), Some(Color::Rgb(18, 52, 86)));
        srv.shutdown();
    }


    #[test]
    fn pane_dimming_and_colour_names() {
        let (mut srv, pane) = with_pane();
        command(&mut srv, &["set", "-g", "window-style", "fg=#804020,bg=#204080,dim=50"]);
        command(&mut srv, &["set", "-g", "window-active-style", "dim=50"]);
        srv.panes.get_mut(&pane).unwrap().parser.process(b"x");
        let frame = srv.compose(1).unwrap().frame;
        assert_eq!((frame.get(0, 0).unwrap().style.fg, frame.get(0, 0).unwrap().style.bg), (Color::Rgb(64, 32, 16), Color::Rgb(16, 32, 64)));
        command(&mut srv, &["set", "-g", "window-active-style", "dim=100"]);
        assert_eq!(srv.compose(1).unwrap().frame.get(0, 0).unwrap().style.fg, Color::Rgb(0, 0, 0));
        for (value, expected) in [("colour3", "colour3"), ("color3", "colour3"), ("colour15", "colour15"), ("terminal", "terminal")] {
            command(&mut srv, &["set", "-g", "window-style", &format!("fg={value}")]);
            command(&mut srv, &["set", "-g", "cursor-colour", value]);
            assert_eq!(srv.expand_for_pane(pane, "#{pane_fg}"), expected);
            assert_eq!(srv.expand_for_pane(pane, "#{cursor_colour}"), expected);
        }
        srv.shutdown();
    }

    #[test]
    fn device_reports_are_reassembled_and_update_client_palette_depth() {
        let mut srv = server();
        for byte in b"\x1bP>|XTerm(400)\x1b\\" { srv.handle_input(1, &[*byte]); }
        assert_eq!(srv.clients[&1].features.colours(), 16777216);
        for (raw, depth) in [(b"\x1b[>85;1;0c".as_slice(), 256), (b"\x1b[>84;1;0c", 16777216), (b"\x1bP>|foot(1.0)\x1b\\", 16777216)] {
            let mut f = features::Features::default();
            assert!(f.device_report(raw));
            assert_eq!(f.colours(), depth);
        }
    }

    #[test]
    fn tree_prefixes_use_theme_colours() {
        let (mut srv, pane) = with_pane();
        command(&mut srv, &["new-window", "-d", "sleep 30"]);
        command(&mut srv, &["choose-tree", "-w", "-t", &format!("%{pane}")]);
        // theme names parsed without a client are drawn in the client's palette
        let palette = ctx(&srv).theme_palette();
        let render = |srv: &Server, h: u16| match srv.panes[&pane].mode.as_ref() {
            Some(PaneMode::Tree(t)) => crate::format::with_draw_palette(palette, || t.render(srv, 80, h).0),
            _ => panic!("not in tree mode"),
        };
        let lines = render(&srv, 24);
        let grey = ctx(&srv).theme_color("themelightgrey").unwrap();
        assert!(lines.iter().skip(1).flat_map(|l| &l.cells).any(|c| c.style.fg == grey));
        // t tags the current window ("0", selected when the mode opens): a tagged item's
        // name and '*' are themecyan
        srv.mode_key(pane, Some(1), "t");
        let tagged = render(&srv, 24);
        let cyan = ctx(&srv).theme_color("themecyan").unwrap();
        let row = &tagged[1];
        assert!(row.text().contains("0*: "), "{:?}", row.text());
        let star = row.cells.iter().position(|c| c.text == "*").unwrap();
        assert!(row.cells[star - 1..=star].iter().all(|c| c.style.fg == cyan));
        srv.mode_key(pane, Some(1), "i");
        let lines = render(&srv, 40);
        assert!(lines.iter().any(|l| l.text().contains("Window") && l.cells.iter().any(|c| c.text == "W" && c.style.fg == grey)));
        srv.shutdown();
    }
    #[test]
    fn queries_obey_overrides_and_resize_request_limit() {
        assert!(!vt100_like("dumb", &[]));
        assert!(!vt100_like("xterm", &["xterm*:XT@:clear@".into()]));
        assert!(vt100_like("dumb", &["dumb:XT".into()]));
        assert!(vt100_like("xterm", &["xterm:XT@:clear=\\E[H".into()]));
        let mut t = Terminal { vt100: false, ..Default::default() };
        assert!(t.start().is_none());
        assert!(t.requests(true).is_none());
        t.vt100 = true;
        assert!(t.start().is_some());
        assert!(t.requests(false).is_none());
        t.last_requests = Some(Instant::now() - REQUEST_LIMIT - Duration::from_secs(1));
        assert!(t.requests(false).is_some());
        assert!(t.requests(false).is_none());
        assert!(t.requests(true).is_some());
    }


    #[test]
    fn resize_path_does_not_repeat_queries_inside_thirty_seconds() {
        let mut srv = server();
        let (tx, rx) = mpsc::channel();
        srv.clients.get_mut(&1).unwrap().tx = tx;
        srv.clients.get_mut(&1).unwrap().terminal.vt100 = true;
        srv.start_terminal_queries(1);
        assert!(rx.try_recv().is_ok());
        srv.repeat_terminal_queries(1);
        assert!(rx.try_recv().is_err());
        srv.clients.get_mut(&1).unwrap().terminal.last_requests = Some(Instant::now() - REQUEST_LIMIT - Duration::from_secs(1));
        srv.repeat_terminal_queries(1);
        assert!(rx.try_recv().is_ok());
        srv.repeat_terminal_queries(1);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn osc_changes_and_queries_in_one_chunk_use_current_active_palette() {
        let (mut srv, pane) = with_pane();
        command(&mut srv, &["set", "-g", "window-style", "bg=#102030"]);
        let defaults = srv.pane_colours(pane);
        let parser = &mut srv.panes.get_mut(&pane).unwrap().parser;
        parser.callbacks_mut().colours.update(defaults);
        parser.process(b"\x1b]11;#00ff00\x07\x1b]11;?\x07");
        assert_eq!(parser.callbacks().replies, b"\x1b]11;rgb:0000/ffff/0000\x07");
        parser.callbacks_mut().replies.clear();
        parser.process(b"\x1bc\x1b]11;?\x07");
        assert_eq!(parser.callbacks().replies, b"\x1b]11;rgb:1010/2020/3030\x07");
        srv.shutdown();
    }
    #[test]
    fn theme_notifications_do_not_wait_for_colour_queries() {
        let (mut srv, pane) = with_pane();
        let defaults = srv.pane_colours(pane);
        let p = srv.panes.get_mut(&pane).unwrap();
        p.parser.callbacks_mut().colours.update(defaults);
        p.parser.process(b"\x1b[?2031h");
        srv.clients.get_mut(&1).unwrap().terminal.vt100 = true;
        srv.terminal_report(1, b"\x1b[?997;2n");
        assert_eq!(srv.panes[&pane].parser.callbacks().colours.last, Some(Theme::Light));
        srv.terminal_report(1, b"\x1b[?997;1n");
        assert_eq!(srv.panes[&pane].parser.callbacks().colours.last, Some(Theme::Dark));
        srv.shutdown();
    }

    #[test]
    fn refresh_report_and_flags_precede_early_return_actions() {
        let (mut srv, pane) = with_pane();
        srv.clients.get_mut(&1).unwrap().control = 1;
        for action in [("-A", format!("%{pane}:on")), ("-B", "test::#{pane_bg}".into()), ("-C", "80x24".into())] {
            let report = format!("%{pane}:\x1b]11;rgb:ff/ff/ff\x07");
            command(&mut srv, &["refresh-client", "-F", "no-output", "-f", "wait-exit", "-r", &report, action.0, &action.1]);
            assert!(srv.clients[&1].ctl.no_output && srv.clients[&1].ctl.wait_exit);
            assert_eq!(srv.panes[&pane].parser.callbacks().colours.control.bg, Some(Color::Rgb(255, 255, 255)));
            srv.panes.get_mut(&pane).unwrap().parser.callbacks_mut().colours.control.bg = None;
            srv.clients.get_mut(&1).unwrap().ctl.no_output = false;
            srv.clients.get_mut(&1).unwrap().ctl.wait_exit = false;
        }
        srv.shutdown();
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
        for (term, extra, depth) in [("dumb", "", "2"), ("vt100", "", "2"), ("linux", "", "8"), ("xterm-16color", "", "16"), ("xterm-256color", "", "16"), ("xterm-256color", "256", "256"), ("xterm", "RGB", "16777216")] {
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
        // tty_keys_colours consumes a complete reply whose colour does not parse, unchanged
        let bg = srv.clients[&1].terminal.bg;
        assert!(srv.terminal_report(1, b"\x1b]11;rgb:zz/ff/00\x07"));
        assert_eq!(srv.clients[&1].terminal.bg, bg);
        assert!(!srv.terminal_report(1, b"\x1b]12;rgb:ff/ff/00\x07"));
    }

    #[test]
    fn pane_theme_consensus_palette_and_control_reports() {
        let mut srv = server();
        srv.exec(vec!["new-session".into(), "-d".into(), "-s".into(), "theme".into(), "sleep 30".into()], &mut Ctx::default()).unwrap();
        let sid = srv.sessions.values().find(|s| s.name == "theme").unwrap().id;
        let wid = srv.sessions[&sid].active_window();
        let pane = srv.windows[&wid].active;
        let c = srv.clients.get_mut(&1).unwrap();
        c.session = Some(sid);
        c.terminal.reported = Some(Theme::Dark);
        c.terminal.bg = Some(Color::Rgb(0, 0, 0));
        let mut second = server().clients.remove(&1).unwrap();
        second.id = 2;
        second.session = Some(sid);
        second.terminal.reported = Some(Theme::Light);
        second.terminal.bg = Some(Color::Rgb(255, 255, 255));
        srv.clients.insert(2, second);
        let defaults = srv.pane_colours(pane);
        assert_eq!(defaults.reported, None); // conflicting explicit themes: use pane background
        assert_eq!(defaults.bg, Some(Color::Rgb(0, 0, 0))); // first attached client
        let cb = srv.panes.get_mut(&pane).unwrap().parser.callbacks_mut();
        cb.colours.bg = Some(Color::Rgb(255, 255, 255));
        cb.colours.update(defaults);
        assert_eq!(cb.colours.theme(), Some(Theme::Light));
        srv.clients.get_mut(&2).unwrap().control = 1;
        assert_eq!(srv.pane_colours(pane).reported, Some(Theme::Dark));
        srv.clients.get_mut(&1).unwrap().terminal.reported = None;
        srv.theme_control_report(&format!("%{pane}:\x1b]11;rgb:00/00/00\x07"));
        let cb = srv.panes.get_mut(&pane).unwrap().parser.callbacks_mut();
        assert_eq!(cb.colours.theme(), Some(Theme::Dark)); // control report wins over pane palette
        let mut replies = Vec::new();
        cb.colours.osc(&[b"11", b"?"], true, &mut replies);
        assert_eq!(replies, b"\x1b]11;rgb:0000/0000/0000\x07");
        srv.kill_session(sid, false);
    }

    #[test]
    fn pane_queries_and_notifications_preserve_terminal_theme() {
        let nz = |n| NonZeroU16::new(n).unwrap();
        let mut parser = vt100::Parser::new_with_callbacks(nz(24), nz(80), 10, PaneCb::default());
        parser.process(b"\x1b[?996n\x1b]10;?\x07\x1b]11;?\x07");
        assert!(parser.callbacks().replies.is_empty());
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
