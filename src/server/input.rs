//! Client input: key tables, prompts, overlays, pane modes and the mouse.

use super::modes::{MenuAction, ModeAction};
use super::*;
use crate::{cmdparse, format, opts};
use crate::keys::{Input, Key, KeyCode};

pub use super::mouse::MouseCtx;

impl Client {
    /// tty_stop_tty: the reset sequences that depend on what the terminal was given (Se, or Ss 0,
    /// after a cursor style; Cr after a cursor colour; Dsesc for appesc). The client itself
    /// restores the fixed modes.
    pub(super) fn tty_stop_sequence(&self) -> String {
        let mut out = String::new();
        let cap = |name: &str| self.term_caps.get(name).filter(|v| !v.is_empty());
        if self.input_state.tty_cstyle != 0 {
            match cap("Se") {
                Some(se) => out.push_str(se),
                None => out.push_str(&features::string_capability(&self.term_caps, "Ss", "\x1b[%p1%d q", &["0"])),
            }
        }
        if self.input_state.tty_ccolour.is_some() {
            out.push_str(&features::string_capability(&self.term_caps, "Cr", "\x1b]112\x07", &[]));
        }
        if self.input_state.tty_appesc {
            out.push_str(&features::string_capability(&self.term_caps, "Dsesc", "\x1b[?7727l", &[]));
        }
        // Dsbp, Dsfcs and Dseks: the client always sends the usual sequences, so only an
        // override changes what goes out
        for (name, usual) in [("Dsbp", "\x1b[?2004l"), ("Dsfcs", "\x1b[?1004l"), ("Dseks", "\x1b[>4m")] {
            if let Some(v) = self.term_caps.get(name).filter(|v| !v.is_empty() && *v != usual) {
                out.push_str(v);
            }
        }
        out
    }

    /// server_client_set_key_table: the table and when it was set, which prefix-timeout
    /// measures from.
    pub fn set_key_table(&mut self, name: &str) {
        self.key_table = name.to_string();
        self.input_state.table_at = Some(Instant::now());
    }

    /// Before the client gives up the terminal (exit, suspend, lock): reset it and forget what
    /// it was given, so a later redraw sends it again.
    pub(super) fn tty_stop(&mut self) {
        if self.control != 0 { return; }
        let out = self.tty_stop_sequence();
        if !out.is_empty() { self.send(S_OUTPUT, out.as_bytes()); }
        self.input_state.tty_cstyle = 0;
        self.input_state.tty_ccolour = None;
        self.input_state.tty_appesc = false;
    }
}

/// Per-client key timing. Monotonic time avoids wall clock changes during a paste.
#[derive(Default)]
pub struct InputState {
    /// When the key table was last set (keytable->activity_time), for prefix-timeout.
    pub(super) table_at: Option<Instant>,
    last_key: String,
    key_at: Option<Instant>,
    assume_paste: bool,
    guess_at: Option<Instant>,
    paste_at: Option<Instant>,
    clipboard: Vec<(PaneId, Instant, bool)>,
    /// What the outer terminal was last given (tty->cstyle, tty->ccolour).
    pub(super) tty_cstyle: u8,
    pub(super) tty_ccolour: Option<(u8, u8, u8)>,
    /// Application escape mode was turned on (Enesc).
    pub(super) tty_appesc: bool,
}

pub fn default_bindings() -> BTreeMap<String, BTreeMap<String, Binding>> {
    let mut t: BTreeMap<String, BTreeMap<String, Binding>> = BTreeMap::new();
    for words in cmdparse::parse(include_str!("display-defaults.conf")).expect("tmux default bindings") {
        let o = opts::getopt(&words[1..], "nr", "TN").expect("default binding options");
        let table = if o.has('n') { "root" } else { o.get('T').unwrap_or("prefix") };
        let key = &o.rest[0];
        let key = keys::parse_key_name(key).map(|k| keys::key_name(&k)).unwrap_or_else(|| key.clone());
        let cmd = super::cmds::binding_command(&o.rest[1..]).trim().to_string();
        t.entry(table.into()).or_default().insert(key, Binding {
            cmd, repeat: o.has('r'), note: o.get('N').unwrap_or("").into(),
        });
    }
    // the copy mode tables of tmux 3.8, run by the copy mode engine while unchanged
    for (table, vi) in [("copy-mode", false), ("copy-mode-vi", true)] {
        for (k, c) in CopyMode::default_bindings(vi) {
            let key = keys::parse_key_name(k).map(|k| keys::key_name(&k)).unwrap_or_else(|| k.to_string());
            t.entry(table.into()).or_default().insert(key, Binding { cmd: super::copy::default_binding_command(c), repeat: false, note: String::new() });
        }
    }
    t
}

/// cmd_template_replace: in `template` replace %N (and %N%) with `s`, and the
/// first %% (or %%%) left. %% escapes single quotes, %%% and %N% escape
/// characters special inside double quotes.
fn template_replace(template: &str, s: &str, idx: usize) -> String {
    if !template.contains('%') {
        return template.to_string();
    }
    let b = template.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let (mut i, mut replaced) = (0, false);
    while i < b.len() {
        let ch = b[i];
        i += 1;
        if ch == b'%' {
            let quote = if i < b.len() && (b'1'..=b'9').contains(&b[i]) && (b[i] - b'0') as usize == idx {
                i += 1;
                Some(if i < b.len() && b[i] == b'%' { i += 1; '"' } else { ' ' })
            } else if i < b.len() && b[i] == b'%' && !replaced {
                replaced = true;
                i += 1;
                Some(if i < b.len() && b[i] == b'%' { i += 1; '"' } else { '\'' })
            } else {
                None
            };
            if let Some(quote) = quote {
                for c in s.bytes() {
                    if quote == '\'' && c == b'\'' {
                        out.extend_from_slice(b"'\\''");
                        continue;
                    }
                    if quote == '"' && b"\"\\$;~".contains(&c) {
                        out.push(b'\\');
                    }
                    out.push(c);
                }
                continue;
            }
        }
        out.push(ch);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// args_make_commands: each answer replaces %N and the next %% of the template.
pub fn fill_template(template: &str, answers: &[String]) -> String {
    let mut out = template.to_string();
    for (i, a) in answers.iter().enumerate() {
        out = template_replace(&out, a, i + 1);
    }
    out
}

impl Server {
    pub(crate) fn width_context(&self) -> crate::width::Guard {
        let entries = self.opts_server.array_items("codepoint-widths").into_iter().map(|(_, v)| v).collect::<Vec<_>>();
        crate::width::enter(features::codepoint_widths(&entries), options::is_on(&self.opt_server("variation-selector-always-wide")))
    }

    pub(crate) fn input_timers(&mut self) {
        let now = Instant::now();
        for c in self.clients.values_mut() {
            if c.input_state.guess_at.is_some_and(|t| now.duration_since(t) >= Duration::from_secs(5)) {
                c.input_state.assume_paste = false; c.input_state.guess_at = None;
            }
            c.input_state.clipboard.retain(|(_, t, _)| now.duration_since(*t) < Duration::from_secs(5));
        }
        let expired: Vec<_> = self.clients.iter().filter(|(_, c)| c.input_state.paste_at.is_some_and(|t| now.duration_since(t) >= Duration::from_secs(5))).map(|(id, _)| *id).collect();
        for cid in expired {
            let c = self.clients.get_mut(&cid).unwrap();
            c.input_state.paste_at = None;
            c.input_state.assume_paste = false;
            let inputs = c.keyp.expire_paste();
            for input in inputs { self.dispatch_input(cid, input); }
        }
        let expired: Vec<_> = self.sessions.values().filter_map(|s| {
            let seconds = self.opt_session_num(Some(s.id), "lock-after-time");
            (seconds > 0 && now_secs().saturating_sub(s.activity) >= seconds as u64).then_some(s.id)
        }).collect();
        for sid in expired {
            let ids: Vec<_> = self.clients.values().filter(|c| c.session == Some(sid)).map(|c| c.id).collect();
            for id in ids { self.lock_input_client(id); }
        }
    }

    pub(crate) fn lock_input_client(&mut self, cid: ClientId) {
        let Some(c) = self.clients.get(&cid) else { return };
        if !c.attached || c.suspended || c.control != 0 { return; }
        let command = self.opt_session(c.session, "lock-command");
        if command.is_empty() { return; }
        let c = self.clients.get_mut(&cid).unwrap();
        c.tty_stop();
        c.suspended = true;
        c.activity = now_secs();
        c.send(S_SHELL, command.as_bytes());
    }

    /// prompt_find_history_file: an absolute path or one under ~/; anything else is ignored.
    fn history_path(&self) -> Option<std::path::PathBuf> {
        let path = self.opt_server("history-file");
        if let Some(rest) = path.strip_prefix("~/").or_else(|| if cfg!(windows) { path.strip_prefix("~\\") } else { None }) {
            return procinfo::home_dir().map(|home| std::path::PathBuf::from(home).join(rest));
        }
        let path = std::path::PathBuf::from(path);
        (path.is_absolute() || path.starts_with("/")).then_some(path)
    }

    /// prompt_add_history: a line equal to the newest entry is not added again; the oldest
    /// entries go once prompt-history-limit is reached.
    pub(crate) fn add_prompt_history(&mut self, line: &str, ty: usize) {
        let limit = self.opt_server_num("prompt-history-limit").max(0) as usize;
        let Some(history) = self.prompt_history.get_mut(ty) else { return };
        if history.last().is_none_or(|last| last != line) { history.push(line.to_string()); }
        let excess = history.len().saturating_sub(limit);
        history.drain(..excess);
    }

    /// prompt_load_history: `type:line` entries; a line without a known type is command history.
    pub(crate) fn load_prompt_history(&mut self) {
        let Some(text) = self.history_path().and_then(|path| std::fs::read(path).ok()) else { return };
        let text = String::from_utf8_lossy(&text).into_owned();
        for line in text.split_inclusive('\n') {
            let line = line.strip_suffix('\n').unwrap_or(line);
            match line.split_once(':').and_then(|(ty, rest)| Some((prompt_type(ty)?, rest))) {
                Some((ty, rest)) => self.add_prompt_history(rest, ty),
                None => self.add_prompt_history(line, 0),
            }
        }
    }

    /// prompt_save_history: written once, when the server exits.
    pub(crate) fn save_prompt_history(&self) {
        let Some(path) = self.history_path() else { return };
        let mut text = String::new();
        for (ty, history) in self.prompt_history.iter().enumerate() {
            for line in history { text.push_str(&format!("{}:{line}\n", PROMPT_TYPES[ty])); }
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        if let Ok(mut file) = options.open(path) { let _ = file.write_all(text.as_bytes()); }
    }

    /// tty_set_selection: write the client's Ms capability (default OSC 52) with `clip` as the
    /// selection parameter.
    pub(crate) fn set_selection(&self, cid: ClientId, clip: &str, data: &[u8]) {
        let Some(c) = self.clients.get(&cid) else { return };
        if !c.attached || c.suspended || c.control != 0 || !c.features.has("clipboard") { return; }
        let encoded = super::cmds::base64(data);
        c.send(S_OUTPUT, features::string_capability(&c.term_caps, "Ms", "\x1b]52;%p1%s;%p2%s\x07", &[clip, &encoded]).as_bytes());
    }

    /// The OSC 52 queries, sets and passthrough of one read of output; returns
    /// whether each set reached the clipboard (pane-set-clipboard fires for those).
    pub(crate) fn pane_input_requests(&mut self, pane: PaneId, queries: Vec<(Vec<u8>, bool)>, sets: Vec<(Vec<u8>, Vec<u8>)>, passthrough: Vec<Vec<u8>>) -> Vec<bool> {
        let mut set = vec![false; sets.len()];
        // Clients showing the pane (screen_write_set_client_cb).
        let showing = |srv: &Self| -> Vec<ClientId> {
            srv.clients.values().filter(|c| c.attached && !c.suspended && c.control == 0 && c.session.is_some_and(|s| {
                let w = &srv.windows[&srv.sessions[&s].active_window()];
                w.panes().contains(&pane) && (!w.zoomed || w.active == pane)
            })).map(|c| c.id).collect()
        };
        // input_osc_52_parse: only with set-clipboard on; the selector keeps its allowed characters.
        if self.opt_server("set-clipboard") == "on" {
            for (i, (selector, data)) in sets.into_iter().enumerate() {
                if data.is_empty() { continue; }
                let Ok(decoded) = decode_osc52(&data) else { continue };
                let clip = clipboard_selector(&selector);
                for cid in showing(self) { self.set_selection(cid, &clip, &decoded); }
                set[i] = true;
                self.add_buffer(None, decoded, false);
            }
            for (selector, bel) in queries {
                let clip = clipboard_selector(&selector).bytes().next();
                match self.opt_server("get-clipboard").as_str() {
                    "buffer" => {
                        // paste_get_top: the newest automatic buffer only
                        if let Some(buffer) = self.buffers.iter().find(|b| b.automatic) {
                            if let Some(p) = self.panes.get(&pane) { p.send(clipboard_reply_bytes(clip, &buffer.data, bel)); }
                        }
                    }
                    "request" | "both" => {
                        // input_add_request: the most recently active attached client (not suspended or
                        // locked) whose session has the window; Ms goes to it if it has the feature
                        let cid = self.clients.values()
                            .filter(|c| c.attached && !c.suspended && c.control == 0 && c.session.is_some_and(|s| self.sessions[&s].links.values().any(|w| self.windows[w].panes().contains(&pane))))
                            .max_by_key(|c| (c.activity, c.input_state.key_at, std::cmp::Reverse(c.id)))
                            .filter(|c| c.features.has("clipboard")).map(|c| c.id);
                        if let Some(cid) = cid {
                            let c = self.clients.get_mut(&cid).unwrap();
                            if c.input_state.clipboard.len() < 64 {
                                c.input_state.clipboard.push((pane, Instant::now(), bel));
                                // tty_clipboard_query asks with an empty selection parameter
                                c.send(S_OUTPUT, features::string_capability(&c.term_caps, "Ms", "\x1b]52;%p1%s;%p2%s\x07", &["", "?"]).as_bytes());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let allow = self.opt_pane(pane, "allow-passthrough");
        if allow != "off" {
            for c in self.clients.values().filter(|c| c.attached && !c.suspended && c.control == 0) {
                let linked = c.session.is_some_and(|s| self.sessions[&s].links.values().any(|w| self.windows[w].panes().contains(&pane)));
                let visible = c.session.is_some_and(|s| { let w = &self.windows[&self.sessions[&s].active_window()]; w.panes().contains(&pane) && (!w.zoomed || w.active == pane) });
                if linked && (allow == "all" || visible) { for data in &passthrough { c.send(S_OUTPUT, data); } }
            }
        }
        set
    }

    /// tty_keys_clipboard: the terminal's OSC 52 reply answers the panes' get-clipboard
    /// requests, and makes a buffer if refresh-client -l asked for it in the last few seconds.
    fn clipboard_reply(&mut self, cid: ClientId, raw: &[u8]) {
        let Some(body) = raw.strip_prefix(b"\x1b]52;").and_then(|s| s.strip_suffix(b"\x07").or_else(|| s.strip_suffix(b"\x1b\\"))) else { return };
        let Some(at) = body.iter().position(|b| *b == b';') else { return };
        // tty_keys_clipboard keeps the selection only when exactly one was given
        let clip = (at == 1).then(|| body[0]);
        let Ok(data) = decode_osc52(&body[at + 1..]) else { return };
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let requests = std::mem::take(&mut c.input_state.clipboard);
        let query = c.clipboard_query.take().is_some_and(|d| Instant::now() < d);
        let setting = self.opt_server("get-clipboard");
        if !requests.is_empty() && matches!(setting.as_str(), "request" | "both") {
            for (pane, at, bel) in requests {
                if at.elapsed() <= Duration::from_secs(5) {
                    if let Some(p) = self.panes.get(&pane) { p.send(clipboard_reply_bytes(clip, &data, bel)); }
                    // input_request_clipboard_reply: both also makes a buffer for each request
                    if setting == "both" { self.add_buffer(None, data.clone(), false); }
                }
            }
        }
        if query && !data.is_empty() { self.add_buffer(None, data, false); }
    }

    pub fn handle_input(&mut self, cid: ClientId, data: &[u8]) {
        let user_keys = self.opts_server.array_items("user-keys").into_iter()
            .filter_map(|(n, s)| n.strip_prefix("user-keys[").and_then(|s| s.strip_suffix(']')).and_then(|s| s.parse::<u16>().ok()).filter(|n| *n <= 1000).map(|n| (n, s.into_bytes()))).collect();
        let Some(c) = self.clients.get_mut(&cid) else { return };
        if c.suspended { return; }
        c.keyp.set_user_keys(user_keys);
        let inputs = c.keyp.feed(data);
        if c.keyp.in_paste() && c.input_state.paste_at.is_none() { c.input_state.paste_at = Some(Instant::now()); }
        if !c.keyp.in_paste() { c.input_state.paste_at = None; }
        // a partial paste end or theme/colour reply restarts the deadline at 500ms or more
        let reply = c.keyp.partial_paste_end() || c.keyp.theme_reply_pending();
        c.esc_deadline = if c.keyp.has_pending() || c.keyp.partial_paste_end() {
            let mut ms = self.opts_server.get("escape-time").and_then(|v| v.parse().ok()).unwrap_or(10u64).max(1);
            // tty_keys_next: also while a clipboard request (TTY_OSC52QUERY) is outstanding
            let clipboard = !c.input_state.clipboard.is_empty() || c.clipboard_query.is_some_and(|d| Instant::now() < d);
            if reply || clipboard { ms = ms.max(500); }
            if reply { Some(Instant::now() + Duration::from_millis(ms)) }
            else { c.esc_deadline.or_else(|| Some(Instant::now() + Duration::from_millis(ms))) }
        } else {
            None
        };
        for i in inputs {
            self.dispatch_input(cid, i);
        }
    }

    pub fn dispatch_input(&mut self, cid: ClientId, input: Input) {
        if let Input::Unknown(raw) = &input {
            if raw.starts_with(b"\x1b]52;") {
                self.clipboard_reply(cid, raw);
                return;
            }
            // terminal replies (device attributes, DECRPM 2026, theme and colours) are
            // read for every client, read-only ones too (tty_keys_next)
            if self.terminal_report(cid, raw) { return; }
            // focus reports set the client's focus (also for read-only clients)
            if raw == b"\x1b[I" || raw == b"\x1b[O" {
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.focused = raw == b"\x1b[I";
                }
            }
        }
        if self.client_is_readonly(Some(cid)) && !matches!(&input, Input::Key(_, _)) {
            return;
        }
        match input {
            Input::Key(k, raw) => self.handle_key(cid, k, raw),
            Input::Mouse(m, _) => {
                if m.kind == keys::MouseKind::Move && self.clients.get(&cid).is_some_and(|c| c.prompt.is_none() && c.overlay.is_none()) {
                    if let Some(sid) = self.client_session(cid).filter(|s| self.opt_session_on(Some(*s), "focus-follows-mouse")) {
                        let wid = self.sessions[&sid].active_window();
                        let top = self.opt_session(Some(sid), "status-position") == "top";
                        let offset = if top { self.status_lines(Some(sid)) } else { 0 };
                        if let Some(y) = m.y.checked_sub(offset) {
                            if let Some((pane, _)) = self.rects(wid).0.iter().find(|(_, r)| {
                                let r = self.content_rect(wid, *r); m.x >= r.x && m.x < r.right() && y >= r.y && y < r.bottom()
                            }).copied() { self.select_pane(wid, pane); }
                        }
                    }
                }
                self.handle_mouse(cid, m)
            },
            Input::Paste(data) => {
                let Some(c) = self.clients.get_mut(&cid) else { return };
                if let Some(pr) = &mut c.prompt {
                    for ch in String::from_utf8_lossy(&data).chars().filter(|c| !c.is_control()) {
                        pr.input.insert(pr.cursor, ch);
                        pr.cursor += 1;
                    }
                    c.dirty = true;
                    return;
                }
                if c.is_readonly() {
                    return;
                }
                // the first pasted key closes a pane waiting for one (tmux only skips the
                // paste start and end markers)
                if let Some(pane) = self.client_active_pane(cid).filter(|_| !data.is_empty()) {
                    if self.dead_key(pane) {
                        return;
                    }
                }
                if let Some(target) = self.input_target(cid) {
                    // the paste-start key reaches the pane's mode first: it ends clock mode
                    // like any key; in another mode the pasted text is dropped
                    match self.panes.get(&target).map(|p| &p.mode) {
                        Some(Some(PaneMode::Clock)) => self.exit_mode(target),
                        Some(Some(_)) => return,
                        _ => {}
                    }
                    if self.panes.get(&target).is_some_and(|p| p.options.get("@input-disabled") == Some("on")) { return; }
                    self.paste(target, &data, true, true, None);
                }
            }
            Input::Unknown(raw) => {
                // focus reports reach panes through pane-focus-in/out (only those that asked)
                if raw == b"\x1b[I" || raw == b"\x1b[O" {
                    self.client_focus(cid, raw == b"\x1b[I");
                    return;
                }
                if let Some(t) = self.input_target(cid) {
                    if self.panes.get(&t).is_some_and(|p| p.mode.is_none()) {
                        self.panes[&t].send(raw);
                    }
                }
            }
        }
    }

    /// Where typed input goes: a popup's pane, else the active pane.
    fn input_target(&self, cid: ClientId) -> Option<PaneId> {
        if let Some(Overlay::Popup(p)) = self.clients.get(&cid).and_then(|c| c.overlay.as_ref()) {
            return Some(p.pane);
        }
        self.client_active_pane(cid)
    }

    fn send_key_to_pane(&mut self, cid: ClientId, pane: PaneId, k: &Key, raw: Vec<u8>) {
        if self.client_is_readonly(Some(cid)) {
            return;
        }
        if self.panes.get(&pane).is_none_or(|p| p.options.get("@input-disabled") == Some("on")) { return; }
        let Some(bytes) = self.encode_pane_key(pane, k, raw.clone()) else { return };
        if let Some(p) = self.panes.get(&pane) {
            p.send(bytes);
        }
        for q in self.sync_peers(pane) {
            if let Some(bytes) = self.encode_pane_key(q, k, raw.clone()) {
                self.panes[&q].send(bytes);
            }
        }
    }

    /// window_pane_copy_key: with synchronize-panes a key also goes to the window's other
    /// visible panes that have it on and are not in a mode, encoded for each of them
    /// (input_key_pane: its own cursor, keypad and extended key modes).
    pub(crate) fn sync_peers(&self, pane: PaneId) -> Vec<PaneId> {
        let Some(wid) = self.window_of(pane) else { return Vec::new() };
        if self.opt_pane(pane, "synchronize-panes") != "on" {
            return Vec::new();
        }
        self.windows[&wid].panes().into_iter().filter(|q| {
            *q != pane
                && self.panes.get(q).is_some_and(|pp| pp.mode.is_none() && pp.dead.is_none() && pp.options.get("@input-disabled") != Some("on"))
                && self.pane_visible(wid, *q)
                && self.opt_pane(*q, "synchronize-panes") == "on"
        }).collect()
    }

    /// window_pane_paste: pasted bytes go to the pane as they are, but not to a pane in a mode
    /// or with input disabled.
    fn paste_bytes(&mut self, cid: ClientId, pane: PaneId, data: &[u8]) {
        if self.client_is_readonly(Some(cid)) { return; }
        let ok = |p: &Pane| p.mode.is_none() && p.dead.is_none() && p.options.get("@input-disabled") != Some("on");
        if !self.panes.get(&pane).is_some_and(ok) { return; }
        let mut targets = vec![pane];
        if let Some(w) = self.window_of(pane).filter(|w| self.opt_window_on(Some(*w), "synchronize-panes")) {
            targets = self.windows[&w].panes();
        }
        for q in targets {
            if let Some(p) = self.panes.get(&q).filter(|p| ok(p)) { p.send(data.to_vec()); }
        }
    }

    /// input_key(): the bytes a key writes to a pane, or None where tmux returns -1 (send-keys
    /// then sends the key name as text).
    pub(crate) fn encode_pane_key(&self, pane: PaneId, k: &Key, raw: Vec<u8>) -> Option<Vec<u8>> {
        let p = self.panes.get(&pane)?;
        // internal function keys such as UserN are never written to a pane
        if matches!(k.code, KeyCode::User(_)) { return Some(Vec::new()); }
        let app = p.screen().application_cursor();
        let keypad = p.screen().application_keypad();
        // the pane's extended key mode (MODE_KEYS_EXTENDED, _2): set when the application asked
        // (or by extended-keys always at a reset), not read from the option for each key
        let mode = p.parser.callbacks().ext_keys;
        let csi_u = self.opt_server("extended-keys-format") == "csi-u";
        let char_code = |code: &KeyCode| -> Option<u32> {
            Some(match code {
                KeyCode::Char(c) => *c as u32,
                KeyCode::Space => 32,
                KeyCode::Enter => 13,
                KeyCode::Tab => 9,
                KeyCode::Escape => 27,
                _ => return None,
            })
        };
        let mut mapped = k.clone();
        let mut remapped = false;
        if k.code == KeyCode::BSpace {
            let option = self.opt_server("backspace");
            if k.mods.is_empty() {
                // an unmodified BSpace writes one byte, and only for a plain or Ctrl key
                return Some(backspace_byte(&option).map(|b| vec![b]).unwrap_or_default());
            }
            if let Some(mut backspace) = keys::parse_key_name(&option) {
                // key = newkey | modifiers: BSpace itself has no sequence, a character goes
                // through the usual modes, and a special key found only without the Meta
                // (no implied Meta) gets ESC before its unmodified form
                if backspace.code == KeyCode::BSpace && option.eq_ignore_ascii_case("BSpace") {
                    return Some(Vec::new());
                }
                backspace.mods.ctrl |= k.mods.ctrl;
                backspace.mods.meta |= k.mods.meta;
                backspace.mods.shift |= k.mods.shift;
                if backspace.code != KeyCode::BSpace {
                    if let Some(cp) = char_code(&backspace.code) {
                        return Some(keys::input_key_char(cp, false, backspace.mods, mode, csi_u));
                    }
                    let meta = std::mem::replace(&mut backspace.mods.meta, false);
                    let mut out = if meta { vec![0x1b] } else { Vec::new() };
                    out.extend(keys::encode_key_mode(&backspace, app, keypad));
                    return Some(out);
                }
                mapped = backspace;
                remapped = true;
            }
        } else if k.code == KeyCode::BTab {
            // KEYC_BTAB: S-Tab in extended mode 2, else ESC [ Z without modifiers
            return Some(keys::input_key_char(9, true, k.mods, mode, csi_u));
        } else if let Some((cp, btab, mods)) = keys::extended_input(&raw) {
            // a key the terminal sent in extended form: encode the exact key tmux parsed, so
            // C-i, C-m, C-@, C-[ and C-S-a stay distinct from Tab, Enter, C-Space, Escape, C-a
            return Some(keys::input_key_char(cp, btab, mods, mode, csi_u));
        }
        let k = &mapped;
        let extended = if mode > 0 { keys::encode_extended(k, mode >= 2, csi_u) } else { None };
        let special = matches!(k.code, KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
            | KeyCode::Insert | KeyCode::Delete | KeyCode::PageUp | KeyCode::PageDown | KeyCode::F(_));
        Some(if let Some(e) = extended {
            e
        } else if k.code == KeyCode::BSpace && k.mods.ctrl {
            // DEL has no Ctrl form in standard mode (input_key_vt10x returns -1)
            return None;
        } else if remapped || matches!(k.code, KeyCode::KP(_)) {
            keys::encode_key_mode(k, app, keypad)
        } else if special && k.mods.is_empty() {
            keys::encode_key(k, app)
        } else if special && raw.first() == Some(&0x1b) {
            // a modified special key is sent in tmux's own xterm form (M-Up from CSI 1;9 A is
            // CSI 1;3 A); ESC before an unmodified one is a Meta without the implied flag
            let mut plain = k.clone();
            if raw.starts_with(b"\x1b\x1b") && plain.mods.meta {
                plain.mods.meta = false;
                let mut out = vec![0x1b];
                out.extend(keys::encode_key_mode(&plain, app, keypad));
                out
            } else {
                keys::encode_key_mode(&plain, app, keypad)
            }
        } else if raw.starts_with(b"\x1b[") && (raw.ends_with(b"u") || raw.starts_with(b"\x1b[27;")) {
            // the outer terminal sent an extended key but this pane didn't ask for them
            keys::encode_key(k, app)
        } else {
            raw
        })
    }

    pub(super) fn handle_key(&mut self, cid: ClientId, k: Key, raw: Vec<u8>) {
        let name = keys::key_name(&k);
        let Some(c) = self.clients.get(&cid) else { return };
        let menu = if c.overlay.is_none() { self.client_window(cid).filter(|w| self.menus.contains_key(w)) } else { None };
        if c.is_readonly() && (c.prompt.is_some() || c.overlay.is_some() || menu.is_some()) { return; }
        // a key clears a message (and is then handled), unless display -N
        if c.message.is_some() && !c.is_readonly() {
            if c.message_ignore_keys { return; }
            let c = self.clients.get_mut(&cid).unwrap();
            c.message = None;
            c.dirty = true;
        }
        // server_client_handle_key0 order: the overlay, the active pane (dead, modal
        // cancel, capture-keys), the window's menu, the status prompt, then a pane prompt
        let Some(c) = self.clients.get(&cid) else { return };
        match &c.overlay {
            Some(Overlay::Popup(p)) => {
                // popup_key_cb: Escape and C-c close the popup unless -E leaves them
                // to its running command; after the command exits only they (or with
                // -k any key) do
                let (pane, close_on_exit, any_key) = (p.pane, p.close_on_exit, p.close_any_key);
                let exited = self.panes.get(&pane).is_none_or(|x| x.dead.is_some());
                let cancel = name == "Escape" || name == "C-c";
                if ((!close_on_exit || exited) && cancel) || (exited && any_key) {
                    let c = self.clients.get_mut(&cid).unwrap();
                    c.overlay = None;
                    c.dirty = true;
                    self.destroy_pane(pane);
                    return;
                }
                if !exited {
                    self.send_key_to_pane(cid, pane, &k, raw);
                }
                return;
            }
            None => {}
        }
        let Some(sid) = self.client_session(cid) else { return };
        // a modal pane closes on Escape or C-c with -D; with -K it gets every key, the
        // prefix included, unless it is in a mode (tmux server_client_handle_key, not for
        // read-only clients)
        if let Some(pane) = self.client_active_pane(cid).filter(|_| !self.client_is_readonly(Some(cid))) {
            if self.dead_key(pane) {
                return;
            }
            let fl = self.panes.get(&pane).map_or(0, |p| p.fflags);
            let modal = self.window_of(pane).is_some_and(|w| self.windows[&w].modal == Some(pane));
            if modal && fl & super::float::CLOSE_ON_CANCEL != 0 && (name == "Escape" || name == "C-c") {
                self.kill_pane(pane);
                return;
            }
            if fl & super::float::CAPTURE_KEYS != 0 && self.panes.get(&pane).is_some_and(|p| p.mode.is_none() && p.dead.is_none()) {
                self.send_key_to_pane(cid, pane, &k, raw);
                return;
            }
        }
        // the menu of the client's window comes before the prompt (server_client_handle_menu_key)
        if let Some(wid) = menu {
            let action = match self.menus.get_mut(&wid) {
                Some(m) => m.key(&name),
                None => MenuAction::None,
            };
            self.menu_action(cid, wid, action);
            return;
        }
        if self.clients[&cid].prompt.is_some() {
            let prompt_key = match k.code { KeyCode::KP('\r') => Key::plain(KeyCode::Enter), KeyCode::KP(ch) => Key::plain(KeyCode::Char(ch)), _ => k.clone() };
            if self.prompt_key(cid, &prompt_key, &name) {
                return;
            }
        }
        // a pane prompt in the window; read-only clients skip it (server_client_handle_key)
        if let Some(pane) = self.pane_prompt_target(cid).filter(|_| !self.client_is_readonly(Some(cid))) {
            if self.pane_prompt_key(cid, pane, &k, &name) {
                return;
            }
        }
        let now = Instant::now();
        let threshold = self.opt_session_num(Some(sid), "assume-paste-time").max(0) as u64;
        let c = self.clients.get_mut(&cid).unwrap();
        let quick = threshold > 0 && !c.features.has("bpaste") && c.input_state.key_at.is_some_and(|t| now.duration_since(t) < Duration::from_millis(threshold));
        let paste = quick && c.input_state.assume_paste;
        if quick && !c.input_state.assume_paste { c.input_state.guess_at = Some(now); }
        c.input_state.assume_paste = quick;
        c.input_state.key_at = Some(now);
        if paste {
            // paste_key: the bytes as they came (window_pane_paste), never to a pane in a mode
            // and without the dead-key check
            if let Some(pane) = self.input_target(cid) { self.paste_bytes(cid, pane, &raw); }
            return;
        }
        let mut table = self.clients[&cid].key_table.clone();
        let mut repeating = self.clients[&cid].repeat_until.is_some_and(|d| d > Instant::now());
        let prefix_ms = self.opt_server_num("prefix-timeout").max(0) as u64;
        let repeat_binding = self.bindings.get(&table).and_then(|t| t.get(&name)).is_some_and(|b| b.repeat);
        // prefix-timeout counts from when the table was set, also by switch-client -T
        if table == "prefix" && prefix_ms > 0 && !(repeating && repeat_binding)
            && self.clients[&cid].input_state.table_at.is_some_and(|t| now.duration_since(t) > Duration::from_millis(prefix_ms)) {
            table = "root".into();
            let c = self.clients.get_mut(&cid).unwrap(); c.set_key_table("root"); c.repeat_until = None;
        }
        let pane = self.client_active_pane(cid);
        loop {
            if table == "root" {
                let prefix = self.opt_session(Some(sid), "prefix");
                let prefix2 = self.opt_session(Some(sid), "prefix2");
                let is_prefix = |p: &str| p != "None" && keys::parse_key_name(p).is_some_and(|pk| keys::key_name(&pk) == name);
                if is_prefix(&prefix) || is_prefix(&prefix2) {
                    // root bindings for the prefix key itself take precedence (tmux checks root first)
                    if !self.bindings.get("root").is_some_and(|t| t.contains_key(&name)) {
                        let c = self.clients.get_mut(&cid).unwrap();
                        c.set_key_table("prefix");
                        c.repeat_until = None;
                        c.dirty = true;
                        return;
                    }
                }
            }
            let found = self.bindings.get(&table).and_then(|t| t.get(&name)).map(|b| (b.cmd.clone(), b.repeat));
            match found {
                Some((_, false)) if repeating => {
                    // a binding that does not repeat ends the repeat; the key is looked up
                    // again from the root table, where it may be the prefix
                    let c = self.clients.get_mut(&cid).unwrap();
                    c.set_key_table("root");
                    c.repeat_until = None;
                    c.dirty = true;
                    repeating = false;
                    table = "root".into();
                }
                Some((cmd, repeat)) => {
                    let mut ms = self.opt_session_num(Some(sid), "repeat-time").max(0) as u64;
                    let initial = self.opt_session_num(Some(sid), "initial-repeat-time").max(0) as u64;
                    if ms > 0 && initial > 0 && (!repeating || self.clients[&cid].input_state.last_key != name) { ms = initial; }
                    let c = self.clients.get_mut(&cid).unwrap();
                    c.input_state.last_key = name.clone();
                    if repeat && ms > 0 && table != "root" {
                        c.repeat_until = Some(Instant::now() + Duration::from_millis(ms));
                    } else {
                        c.repeat_until = None;
                        c.set_key_table("root");
                    }
                    c.dirty = true;
                    self.run_key_string(cid, &cmd, pane, &name);
                    return;
                }
                None if table != "root" => {
                    let c = self.clients.get_mut(&cid).unwrap();
                    c.set_key_table("root");
                    c.repeat_until = None;
                    c.dirty = true;
                    if !repeating {
                        return; // unbound key after the prefix is discarded
                    }
                    repeating = false;
                    table = "root".into();
                }
                None => break,
            }
        }
        // not bound: to the pane (or its mode)
        let Some(pane) = pane else { return };
        // forward_key: before the read-only check, so read-only clients may dismiss it too
        if self.dead_key(pane) { return; }
        if self.panes.get(&pane).is_some_and(|p| p.mode.is_some()) {
            self.mode_key(pane, Some(cid), &name);
            return;
        }
        self.send_key_to_pane(cid, pane, &k, raw);
    }

    /// Runs a key binding's commands for a client: the message log names the key (or,
    /// for send-keys -K, the client that sent it).
    pub fn run_key_string(&mut self, cid: ClientId, cmd: &str, pane: Option<PaneId>, key: &str) {
        match crate::cmdparse::parse(cmd).and_then(|c| c.iter().try_for_each(|c| self.check_parsed(c)).map(|_| c)) {
            Ok(cmds) => {
                let ctx = Ctx { client: Some(cid), pane, from_key: true, key: Some(key.to_string()), log_client: self.key_origin, ..Ctx::default() };
                self.run_queue(Queue { cmds: cmds.into(), ctx , soft: 0 });
            }
            Err(e) => self.set_message(cid, &e),
        }
    }

    /// Runs a command string for a client (key bindings, prompts, menus).
    pub fn run_string(&mut self, cid: ClientId, cmd: &str, pane: Option<PaneId>) {
        // cmd_parse_from_string: the whole list is checked before any of it runs
        match crate::cmdparse::parse(cmd).and_then(|c| c.iter().try_for_each(|c| self.check_parsed(c)).map(|_| c)) {
            Ok(cmds) => {
                let ctx = Ctx { client: Some(cid), pane, from_key: true, ..Ctx::default() };
                self.run_queue(Queue { cmds: cmds.into(), ctx , soft: 0 });
            }
            Err(e) => self.set_error(cid, &e),
        }
    }

    /// The window a client shows.
    pub fn client_window(&self, cid: ClientId) -> Option<WinId> {
        self.client_session(cid).and_then(|s| self.sessions.get(&s)).map(|s| s.active_window())
    }

    /// The rows above the window on a client (status lines at the top).
    pub fn client_window_top(&self, cid: ClientId) -> u16 {
        let Some(c) = self.clients.get(&cid) else { return 0 };
        if c.session.is_none() || self.opt_session(c.session, "status-position") != "top" {
            return 0;
        }
        self.status_lines(c.session).min(c.size.1.saturating_sub(1))
    }

    /// A menu belongs to a window (tmux 3.8 w->menu): every client showing the window sees
    /// and uses it, and opening another menu there replaces it (menu_close).
    /// menu_display / menu_close: a window with a menu has no focused pane, so focus is
    /// updated whenever one opens or closes (a new menu first closes the old one).
    pub fn set_window_menu(&mut self, wid: WinId, menu: Option<super::modes::Menu>) {
        let live = self.windows.contains_key(&wid);
        if self.menus.remove(&wid).is_some() && live {
            self.update_window_focus(wid);
        }
        if let Some(m) = menu {
            self.menus.insert(wid, m);
            if live {
                self.update_window_focus(wid);
            }
        }
        if live {
            self.mark_window_dirty(wid);
        }
    }

    pub(crate) fn menu_action(&mut self, cid: ClientId, wid: WinId, action: MenuAction) {
        match action {
            MenuAction::None => {
                if self.windows.contains_key(&wid) {
                    self.mark_window_dirty(wid);
                }
            }
            MenuAction::Cancel => self.set_window_menu(wid, None),
            MenuAction::Run(cmd) => {
                let pane = self.menus.get(&wid).and_then(|m| m.pane).filter(|p| self.panes.contains_key(p));
                self.set_window_menu(wid, None);
                self.run_string(cid, &cmd, pane.or_else(|| self.client_active_pane(cid)));
            }
        }
    }

    /// A key for a pane that is in a mode (copy, tree, clock). Read-only clients may use
    /// copy mode (but not copy), not the other modes.
    pub fn mode_key(&mut self, pane: PaneId, cid: Option<ClientId>, name: &str) {
        let copy = matches!(self.panes.get(&pane).and_then(|p| p.mode.as_ref()), Some(PaneMode::Copy(_)));
        if self.client_is_readonly(cid) && !copy { return; }
        let Some(p) = self.panes.get_mut(&pane) else { return };
        match &mut p.mode {
            Some(PaneMode::Clock) => self.exit_mode(pane),
            Some(PaneMode::Tree(_) | PaneMode::Switch(_) | PaneMode::Panes(_)) => {
                let (w, h) = (p.size.0 as usize, p.size.1 as usize);
                let Some(mut mode) = p.mode.take() else { return };
                let action = match &mut mode {
                    PaneMode::Tree(t) => {
                        let a = t.key(name, self, w, h);
                        for (ty, line) in t.take_history() {
                            self.add_prompt_history(&line, ty);
                        }
                        a
                    }
                    PaneMode::Switch(s) => s.key(name, self, w, h),
                    PaneMode::Panes(m) => m.key(name, self),
                    _ => ModeAction::None,
                };
                if let Some(p) = self.panes.get_mut(&pane) {
                    p.mode = Some(mode);
                }
                self.mode_action(pane, cid, action);
            }
            Some(PaneMode::Copy(_)) => self.copy_key(pane, cid, name),
            None => {}
        }
    }

    /// The client a mode's commands run for: the one whose key or click it was (tmux passes
    /// that client to mode_tree_run_command and friends), else the one that opened the mode.
    fn mode_client(&self, pane: PaneId, cid: Option<ClientId>) -> Option<ClientId> {
        let attached = |c: &ClientId| self.clients.get(c).is_some_and(|c| c.attached);
        if let Some(c) = cid.filter(attached) {
            return Some(c);
        }
        let opened = match self.panes.get(&pane).and_then(|p| p.mode.as_ref()) {
            Some(PaneMode::Tree(t)) => t.client,
            Some(PaneMode::Switch(s)) => s.client,
            Some(PaneMode::Panes(m)) => m.client,
            _ => None,
        };
        opened.filter(attached).or(cid)
    }

    fn run_mode_command(&mut self, client: Option<ClientId>, cmd: &str, pane: PaneId) {
        let pane = self.panes.contains_key(&pane).then_some(pane);
        match client {
            Some(c) => self.run_string(c, cmd, pane),
            None => match crate::cmdparse::parse(cmd).and_then(|c| c.iter().try_for_each(|c| self.check_parsed(c)).map(|_| c)) {
                Ok(cmds) => self.run_queue(Queue { cmds: cmds.into(), ctx: Ctx { pane, ..Ctx::default() }, soft: 0 }),
                Err(e) => log(&format!("mode command: {e}")),
            },
        }
    }

    /// Carry out what a mode asked for after a key or a click.
    pub fn mode_action(&mut self, pane: PaneId, cid: Option<ClientId>, action: ModeAction) {
        let client = self.mode_client(pane, cid);
        match action {
            ModeAction::None => {}
            ModeAction::Exit => self.exit_mode(pane),
            ModeAction::Run(cmd) => {
                // choosing a pane in panes mode unzooms its window (window_panes_key)
                if matches!(self.panes.get(&pane).and_then(|p| p.mode.as_ref()), Some(PaneMode::Panes(_))) {
                    if let Some(wid) = self.window_of(pane) {
                        self.unzoom_window(wid);
                    }
                }
                // like tmux, the mode is gone before the queued command runs
                self.exit_mode(pane);
                self.run_mode_command(client, &cmd, pane);
            }
            ModeAction::RunKeep(cmd) => self.run_mode_command(client, &cmd, pane),
            ModeAction::Menu { title, items, x, y } => {
                // the items send their key to the mode (mode_tree_menu_callback)
                let mode = self.panes.get(&pane).and_then(|p| p.mode.as_ref()).map(|m| m.name().to_string()).unwrap_or_default();
                let items: Vec<super::modes::MenuItem> = items
                    .into_iter()
                    .map(|(name, key, send)| super::modes::MenuItem {
                        name: name.to_string(),
                        key: key.to_string(),
                        cmd: if name.is_empty() { String::new() } else { format!("if-shell -F -t '%{pane}' '#{{==:#{{pane_mode}},{mode}}}' \"send-keys -t '%{pane}' {send}\"") },
                    })
                    .collect();
                if let (Some(wid), Some(r)) = (self.window_of(pane), self.pane_rect(pane)) {
                    let (w, h) = super::modes::Menu::box_size(&title, &items);
                    let x = if x >= w / 2 { x - w / 2 } else { 0 } + r.x;
                    let y = y + r.y;
                    // menu_prepare: inside the window
                    let (wsx, wsy) = self.windows.get(&wid).map_or((0, 0), |w| (w.sx, w.sy));
                    let fit = |p: u16, size: u16, total: u16| if size >= total { 0 } else if p + size > total { total - size } else { p };
                    let menu = super::modes::Menu::at(title, items, fit(x, w, wsx), fit(y, h, wsy), Some(pane), false, false, None);
                    self.set_window_menu(wid, Some(menu));
                }
            }
            ModeAction::Edit { value, command, buffer } => {
                if let Some(c) = client {
                    if let Err(e) = self.open_editor(c, &value, command, buffer) {
                        self.set_message(c, &e);
                    }
                }
            }
        }
        if let Some(wid) = self.window_of(pane) {
            self.mark_window_dirty(wid);
        }
    }

    /// spawn_editor: edit `value` in the editor option's program, in a modal floating pane
    /// 9/10 of the client's window in its middle; `command` runs with %% replaced by the
    /// text when the editor exits successfully.
    pub fn open_editor(&mut self, cid: ClientId, value: &[u8], command: String, buffer: Option<String>) -> Result<(), String> {
        let wid = self.client_window(cid).ok_or("no current window")?;
        let w = &self.windows[&wid];
        if w.modal.is_some() {
            return Err("window already has a modal pane".into());
        }
        let (wsx, wsy, active) = (w.sx, w.sy, w.active);
        let text: &[u8] = if value.is_empty() { b"\n" } else { value };
        let path = editor_file(text).map_err(|e| format!("editor: {e}"))?;
        let editor = self.opt_server("editor");
        // the editor option names the program (notepad.exe on Windows); the file is one argument
        let quoted = if cfg!(windows) { format!("\"{}\"", path.display()) } else { crate::cmdparse::quote(&path.display().to_string()) };
        let sid = self.client_session(cid);
        let shell = self.opt_session(sid, "default-shell");
        let shell = if shell.is_empty() { procinfo::default_shell() } else { shell };
        let cmd = editor_command(&editor, &quoted, cfg!(windows) && procinfo::is_powershell(&shell));
        // the layout cell: sx * 9 / 10 by sy * 9 / 10 at sx / 2 - w / 2, sy / 2 - h / 2
        // (new-pane's -x/-y count the border, -X/-Y place the border)
        let (pw, ph) = (((wsx as u32 * 9 / 10) as i64).max(1), ((wsy as u32 * 9 / 10) as i64).max(1));
        let (xoff, yoff) = (wsx as i64 / 2 - pw / 2, wsy as i64 / 2 - ph / 2);
        let b = i64::from(self.opt_window(Some(wid), "pane-border-lines") != "none");
        let args: Vec<String> = [
            "-O".to_string(), "-x".into(), (pw + 2 * b).to_string(), "-y".into(), (ph + 2 * b).to_string(),
            "-X".into(), (xoff - b).to_string(), "-Y".into(), (yoff - b).to_string(),
            "-c".into(), std::env::temp_dir().display().to_string(), "-t".into(), format!("%{active}"), cmd,
        ].into();
        let (bools, vals) = cmds::command_flags("new-pane");
        let o = crate::opts::getopt(&args, bools, vals)?;
        let mut ctx = Ctx { client: Some(cid), ..Default::default() };
        if let Err(e) = self.cmd_new_pane("new-pane", &o, &[], &mut ctx) {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        let Some(pane) = ctx.pane else { return Ok(()) };
        if let Some(p) = self.panes.get_mut(&pane) {
            p.options.set("remain-on-exit", "off".into());
        }
        self.editors.insert(pane, (cid, super::modes::PopupEditor { path, command, buffer }));
        Ok(())
    }

    /// The editor pane exited (spawn_editor_finish): nothing for a failed editor or an
    /// empty file, else the paste buffer gets the text (window_buffer_edit_close_cb) or the
    /// command runs with it, less a final newline.
    pub fn editor_done(&mut self, cid: ClientId, ed: super::modes::PopupEditor, status: Option<i32>) {
        let data = std::fs::read(&ed.path).ok();
        let _ = std::fs::remove_file(&ed.path);
        let Some(mut data) = data.filter(|d| status.unwrap_or(0) == 0 && !d.is_empty()) else { return };
        let strip = |d: &mut Vec<u8>| {
            if d.last() == Some(&b'\n') {
                d.pop();
                if cfg!(windows) && d.last() == Some(&b'\r') {
                    d.pop();
                }
            }
        };
        if let Some(name) = &ed.buffer {
            // only a buffer that is still there; its old final newline decides
            let Some(b) = self.buffers.iter_mut().find(|b| &b.name == name) else { return };
            if !b.data.is_empty() && b.data.last() != Some(&b'\n') {
                strip(&mut data);
            }
            if !data.is_empty() {
                b.data = data;
            }
            return;
        }
        strip(&mut data);
        let cmd = fill_template(&ed.command, &[String::from_utf8_lossy(&data).into_owned()]);
        self.run_string(cid, &cmd, None);
    }

    // ---- prompt ----------------------------------------------------------------------

    /// A key for the status line prompt (status_prompt_key): false when the
    /// prompt does not use it and it goes on to the key tables.
    fn prompt_key(&mut self, cid: ClientId, k: &Key, name: &str) -> bool {
        let Some(c) = self.clients.get_mut(&cid) else { return false };
        let Some(mut pr) = c.prompt.take() else { return false };
        c.dirty = true;
        let result = self.prompt_process(cid, &mut pr, k, name);
        self.prompt_done_key(cid, pr);
        matches!(result, PromptResult::Handled | PromptResult::Close)
    }

    /// Puts an edited prompt back unless it finished, and follows a copy-mode
    /// incremental search.
    fn prompt_done_key(&mut self, cid: ClientId, pr: Prompt) {
        let incremental = pr.copy_incremental;
        let text = (!pr.closed).then(|| pr.input.iter().collect::<String>());
        if let Some(host) = pr.host {
            if !pr.closed {
                if let Some(p) = self.panes.get_mut(&host) {
                    if p.prompt.is_none() { p.prompt = Some(pr); }
                }
            } else {
                // window_pane_clear_prompt
                let ty = PROMPT_TYPES.get(pr.history_type).copied().unwrap_or("command");
                self.fire_pane_prompt("pane-prompt-closed", host, ty);
            }
            if let Some(wid) = self.window_of(host) { self.mark_window_dirty(wid); }
        } else if !pr.closed {
            if let Some(c) = self.clients.get_mut(&cid) {
                if c.prompt.is_none() { c.prompt = Some(pr); }
                c.dirty = true;
            }
        }
        if let Some((pane, forward)) = incremental {
            // options copy mode reads as it runs (wrap-search, mode-keys, word-separators)
            self.copy_sync_options(pane);
            if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                match text {
                    Some(text) => cm.preview_search(&text, forward),
                    None => cm.finish_incremental_search(false),
                }
            }
            if let Some(wid) = self.window_of(pane) { self.mark_window_dirty(wid); }
        }
    }

    /// A prompt mouse event (prompt_mouse): button 1 presses on the prompt
    /// line move the cursor or pick an inline completion. Others are not used.
    pub(crate) fn prompt_mouse(&mut self, cid: ClientId, m: &keys::Mouse) -> bool {
        if m.kind != keys::MouseKind::Down || m.button != 1 || self.message_row(cid) != Some(m.y) {
            return false;
        }
        let Some(sid) = self.client_session(cid) else { return false };
        let cw = self.clients[&cid].size.0.max(1);
        let style_text = self.expand_time(self.current(&Ctx { client: Some(cid), ..Ctx::default() }), Some(cid), &self.opt_session(Some(sid), "message-style"));
        let (ax, aw) = super::render::message_area(cw, &style_text);
        let Some(mut pr) = self.clients.get_mut(&cid).and_then(|c| c.prompt.take()) else { return false };
        let used = self.prompt_mouse_at(cid, &mut pr, m.x as usize, ax, aw);
        if let Some(c) = self.clients.get_mut(&cid) {
            if c.prompt.is_none() { c.prompt = Some(pr); }
            c.dirty = true;
        }
        used
    }

    fn prompt_mouse_at(&mut self, cid: ClientId, pr: &mut Prompt, x: usize, ax: u16, aw: u16) -> bool {
        if x < ax as usize || x >= (ax + aw) as usize {
            return false;
        }
        let l = self.prompt_layout(cid, pr, ax, aw);
        if l.input_width == 0 {
            return true;
        }
        let pwidth = super::render::prompt_width(&pr.input) + pr.quote_next as usize;
        // prompt_mouse_complete: a click on one of the inline matches
        let (cx, cax, caw) = (l.cursor_x, l.content_x, l.content_width);
        if !pr.complete.is_empty() && pr.cursor == pr.input.len() && cx >= cax && cx - cax < caw && x >= cx {
            let avail = caw - (cx - cax);
            let clicked = x - cx;
            let width = format::width(&super::render::prompt_complete_display(&pr.complete)).min(avail);
            if clicked < width {
                let mut end = 0;
                for i in 0..pr.complete.len() {
                    let start = end + 1;
                    end = start + format::width(&pr.complete[i]);
                    if clicked >= start && clicked < end {
                        let replace = format!("{} ", pr.complete[i]);
                        if prompt_replace_word(pr, &replace) {
                            pr.complete.clear();
                        }
                        break;
                    }
                }
                return true;
            }
        }
        let target = if x <= l.input_x { l.input_offset } else { l.input_offset + x - l.input_x }.min(pwidth);
        let mut width = 0;
        let mut idx = 0;
        while idx < pr.input.len() && width < target {
            width += super::render::prompt_width(&pr.input[idx..idx + 1]);
            idx += 1;
        }
        if idx != pr.cursor {
            pr.cursor = idx;
            pr.complete.clear();
        }
        true
    }

    /// The prompt's input callback (cmd_command_prompt_callback,
    /// cmd_confirm_before_callback): `s` None cancels. True when the prompt closes.
    fn prompt_fire(&mut self, cid: ClientId, pr: &mut Prompt, s: Option<&str>, fire: Fire) -> bool {
        let close = match pr.kind {
            PromptKind::Key => {
                if let Some(s) = s {
                    let cmd = fill_template(&pr.template, &[s.to_string()]);
                    self.run_string(cid, &cmd, pr.pane);
                }
                true
            }
            PromptKind::Confirm => {
                let first = s.and_then(|s| s.chars().next());
                if first == Some(pr.confirm.0) || (first == Some('\r') && pr.confirm.1) {
                    let template = pr.template.clone();
                    self.run_string(cid, &template, pr.pane);
                }
                true
            }
            PromptKind::Command => match s {
                None => true,
                Some(_) if fire == Fire::Move => true,
                // a copy-mode search ends where the incremental search got to
                Some(_) if pr.copy_incremental.is_some() => fire == Fire::Close,
                Some(_) if fire == Fire::Close && pr.incremental => true,
                Some(s) => {
                    let mut argv = pr.answers.clone();
                    argv.push(s.to_string());
                    if fire == Fire::Close {
                        pr.answers.push(s.to_string());
                        if pr.answers.len() < pr.prompts.len() {
                            let (label, init) = pr.prompts[pr.answers.len()].clone();
                            pr.label = label;
                            pr.input = init.chars().collect();
                            pr.cursor = pr.input.len();
                            pr.history_pos = None;
                            pr.complete.clear();
                            return false;
                        }
                    }
                    let cmd = fill_template(&pr.template, &argv);
                    self.run_string(cid, &cmd, pr.pane);
                    // an incremental prompt stays open for more typing
                    !pr.incremental
                }
            },
        };
        if close {
            pr.closed = true;
        }
        close
    }

    /// tmux 3.8 prompt_key: edit the prompt with a key, firing the callback.
    fn prompt_process(&mut self, cid: ClientId, pr: &mut Prompt, k: &Key, name: &str) -> PromptResult {
        pr.closed = false;
        // inline completion matches are dropped by any key (Tab rebuilds them)
        pr.complete.clear();
        if matches!(pr.kind, PromptKind::Key) {
            self.prompt_fire(cid, pr, Some(name), Fire::Close);
            pr.closed = true;
            return PromptResult::Close;
        }
        let k = &prompt_keypad_key(k);
        if pr.numeric {
            // a digit is typed, anything else runs the command and is then handled
            if let KeyCode::Char(d @ '0'..='9') = k.code {
                if k.mods.is_empty() {
                    return self.prompt_append(cid, pr, d);
                }
            }
            let s: String = pr.input.iter().collect();
            self.prompt_fire(cid, pr, Some(&s), Fire::Close);
            pr.closed = true;
            return PromptResult::NotHandled;
        }
        if pr.single_key || pr.quote_next || matches!(pr.kind, PromptKind::Confirm) {
            let Some(ch) = prompt_key_char(k) else { return PromptResult::Handled };
            pr.quote_next = false;
            return self.prompt_append(cid, pr, ch);
        }
        let vi = self.opt_session(self.client_session(cid), "status-keys") == "vi";
        let op = if vi { prompt_vi_op(pr, k) } else { prompt_emacs_op(k) };
        // prompt_check_move: cursor keys end an incremental prompt
        if pr.incremental && matches!(op, PromptOp::Up | PromptOp::Down | PromptOp::Page | PromptOp::Left | PromptOp::Right) {
            let s: String = pr.input.iter().collect();
            if self.prompt_fire(cid, pr, Some(&s), Fire::Move) {
                return PromptResult::Close;
            }
            return PromptResult::Move;
        }
        let seps: Vec<char> = self.opt_session(self.client_session(cid), "word-separators").chars().collect();
        let n = pr.input.len();
        let mut prefix = '=';
        let changed = match op {
            PromptOp::None | PromptOp::Page => false,
            PromptOp::Insert(ch) => return self.prompt_append(cid, pr, ch),
            PromptOp::Enter => {
                let input: String = pr.input.iter().collect();
                // every non-empty answer goes in the history of the prompt's type,
                // copy-mode searches too (prompt_add_history)
                if !input.is_empty() { self.add_prompt_history(&input, pr.history_type); }
                return self.prompt_done(cid, pr, Some(&input));
            }
            PromptOp::Cancel => return self.prompt_done(cid, pr, None),
            PromptOp::BSpace if pr.bspace_exit && n == 0 => return self.prompt_done(cid, pr, None),
            PromptOp::BSpace => {
                let moved = pr.cursor > 0;
                if moved {
                    pr.cursor -= 1;
                    pr.input.remove(pr.cursor);
                }
                moved
            }
            PromptOp::Delete => {
                if pr.cursor < n {
                    pr.input.remove(pr.cursor);
                }
                pr.cursor < n
            }
            PromptOp::Left => {
                pr.cursor = pr.cursor.saturating_sub(1);
                false
            }
            PromptOp::Right => {
                pr.cursor = (pr.cursor + 1).min(n);
                false
            }
            PromptOp::Home => {
                pr.cursor = 0;
                false
            }
            PromptOp::End => {
                pr.cursor = n;
                false
            }
            // C-u and C-k don't fill the C-y buffer in tmux; only C-w does
            PromptOp::ClearLine => {
                pr.input.clear();
                pr.cursor = 0;
                true
            }
            PromptOp::KillLine => {
                pr.input.truncate(pr.cursor);
                pr.cursor < n
            }
            PromptOp::DeleteWord => {
                let i = prompt_backward_word(&pr.input, pr.cursor, &seps);
                pr.copied = pr.input.drain(i..pr.cursor).collect();
                pr.cursor = i;
                true
            }
            PromptOp::ForwardWord => {
                pr.cursor = prompt_forward_word(&pr.input, pr.cursor, false, &seps);
                true
            }
            PromptOp::ViForward(big) => {
                pr.cursor = prompt_forward_word(&pr.input, pr.cursor, true, if big { &[] } else { &seps });
                true
            }
            PromptOp::ViEnd(big) => {
                pr.cursor = prompt_end_word(&pr.input, pr.cursor, if big { &[] } else { &seps });
                true
            }
            PromptOp::BackWord(big) => {
                pr.cursor = prompt_backward_word(&pr.input, pr.cursor, if big { &[] } else { &seps });
                true
            }
            PromptOp::Up => {
                let history = self.prompt_history.get(pr.history_type).cloned().unwrap_or_default();
                let hist_len = history.len();
                if hist_len > 0 && pr.history_pos != Some(0) {
                    let pos = pr.history_pos.map(|p| p.min(hist_len).saturating_sub(1)).unwrap_or(hist_len - 1);
                    pr.history_pos = Some(pos);
                    pr.input = history[pos].chars().collect();
                    pr.cursor = pr.input.len();
                    true
                } else {
                    false
                }
            }
            PromptOp::Down => {
                // past the newest entry the line is empty (prompt_down_history)
                let history = self.prompt_history.get(pr.history_type).cloned().unwrap_or_default();
                let hist_len = history.len();
                match pr.history_pos.map(|p| p.min(hist_len)) {
                    Some(p) if p + 1 < hist_len => {
                        pr.history_pos = Some(p + 1);
                        pr.input = history[p + 1].chars().collect();
                    }
                    _ => {
                        pr.history_pos = None;
                        pr.input.clear();
                    }
                }
                pr.cursor = pr.input.len();
                true
            }
            PromptOp::Yank => {
                // prompt_paste: the C-w text, else the top buffer up to a control character
                let value: Option<Vec<char>> = if !pr.copied.is_empty() {
                    Some(pr.copied.clone())
                } else {
                    self.buffers.iter().find(|b| b.automatic).map(|b| prompt_paste_text(&b.data))
                };
                match value {
                    Some(value) => {
                        let len = value.len();
                        pr.input.splice(pr.cursor..pr.cursor, value);
                        pr.cursor += len;
                        true
                    }
                    None => false,
                }
            }
            PromptOp::Transpose => {
                let i = if pr.cursor < n { pr.cursor + 1 } else { pr.cursor };
                if i >= 2 {
                    pr.input.swap(i - 2, i - 1);
                    pr.cursor = i;
                }
                i >= 2
            }
            PromptOp::QuoteNext => {
                pr.quote_next = true;
                false
            }
            PromptOp::Tab => self.prompt_complete(pr),
            // C-r/C-s in an incremental prompt: search again, an empty prompt takes the -I text
            PromptOp::Search(back) if pr.incremental => {
                if pr.input.is_empty() {
                    pr.input = pr.last.chars().collect();
                    pr.cursor = pr.input.len();
                } else {
                    prefix = if back { '-' } else { '+' };
                }
                true
            }
            PromptOp::Search(_) => false,
        };
        if changed {
            self.prompt_changed(cid, pr, prefix);
        }
        PromptResult::Handled
    }

    /// prompt_done: fire the callback for a finished prompt.
    fn prompt_done(&mut self, cid: ClientId, pr: &mut Prompt, s: Option<&str>) -> PromptResult {
        if self.prompt_fire(cid, pr, s, Fire::Close) { PromptResult::Close } else { PromptResult::Handled }
    }

    /// The append_key path of prompt_key: insert a character; a single key
    /// prompt (-1, confirm-before) then finishes.
    fn prompt_append(&mut self, cid: ClientId, pr: &mut Prompt, ch: char) -> PromptResult {
        pr.input.insert(pr.cursor.min(pr.input.len()), ch);
        pr.cursor += 1;
        let mut result = PromptResult::Handled;
        if pr.single_key || matches!(pr.kind, PromptKind::Confirm) {
            if pr.input.len() != 1 {
                // with -I text there is no single key to report
                pr.closed = true;
                result = PromptResult::Close;
            } else {
                let s: String = pr.input.iter().collect();
                result = self.prompt_done(cid, pr, Some(&s));
            }
        }
        self.prompt_changed(cid, pr, '=');
        result
    }

    /// An incremental prompt runs its template on every change, the input
    /// after a '=', '+' (C-s) or '-' (C-r).
    fn prompt_changed(&mut self, cid: ClientId, pr: &mut Prompt, prefix: char) {
        if pr.incremental && !pr.closed {
            let s = format!("{prefix}{}", pr.input.iter().collect::<String>());
            self.prompt_fire(cid, pr, Some(&s), Fire::Handled);
        }
    }

    /// The pane prompt a client's keys go to (server_client_handle_key): the
    /// active pane's, else that of the first visible pane in the window's pane
    /// list, floating panes included (window_pane_is_visible).
    pub(crate) fn pane_prompt_target(&self, cid: ClientId) -> Option<PaneId> {
        let sid = self.client_session(cid)?;
        let wid = self.sessions[&sid].active_window();
        let win = self.windows.get(&wid)?;
        let has = |p: &PaneId| self.panes.get(p).is_some_and(|p| p.prompt.is_some());
        if has(&win.active) {
            return self.pane_visible(wid, win.active).then_some(win.active);
        }
        win.panes().into_iter().find(|p| has(p) && self.pane_visible(wid, *p))
    }

    /// A key for a pane prompt (window_pane_prompt_key): false when not used.
    fn pane_prompt_key(&mut self, cid: ClientId, pane: PaneId, k: &Key, name: &str) -> bool {
        let Some(mut pr) = self.panes.get_mut(&pane).and_then(|p| p.prompt.take()) else { return false };
        let result = self.prompt_process(cid, &mut pr, k, name);
        self.prompt_done_key(cid, pr);
        result != PromptResult::NotHandled
    }

    /// A mouse event while a pane prompt shows: a button 1 press on its line
    /// moves the cursor; every mouse event is used.
    pub(crate) fn pane_prompt_mouse(&mut self, cid: ClientId, pane: PaneId, m: &keys::Mouse) {
        let Some(wid) = self.window_of(pane) else { return };
        let Some((_, r)) = self.rects(wid).0.into_iter().find(|(p, _)| *p == pane) else { return };
        let r = self.content_rect(wid, r);
        let sid = self.client_session(cid);
        let oy = if self.opt_session(sid, "status-position") == "top" { self.status_lines(sid) } else { 0 };
        let row = oy + if oy > 0 { r.y } else { r.bottom().saturating_sub(1) };
        if m.kind != keys::MouseKind::Down || m.button != 1 || m.y != row || m.x < r.x || m.x >= r.right() {
            return;
        }
        let Some(mut pr) = self.panes.get_mut(&pane).and_then(|p| p.prompt.take()) else { return };
        self.prompt_mouse_at(cid, &mut pr, (m.x - r.x) as usize, 0, r.w);
        self.prompt_done_key(cid, pr);
    }

    /// prompt_incremental_start: an incremental prompt fires once when opened.
    pub(crate) fn prompt_incremental_start(&mut self, cid: ClientId, host: Option<PaneId>) {
        let pr = match host {
            Some(p) => self.panes.get_mut(&p).and_then(|p| p.prompt.take()),
            None => self.clients.get_mut(&cid).and_then(|c| c.prompt.take()),
        };
        let Some(mut pr) = pr else { return };
        if pr.incremental {
            let s = format!("={}", pr.input.iter().collect::<String>());
            self.prompt_fire(cid, &mut pr, Some(&s), Fire::Handled);
        }
        self.prompt_done_key(cid, pr);
    }

    /// prompt_replace_complete: Tab completes a command name at the start of
    /// the prompt to the longest common prefix of the matches, or keeps the
    /// matches for drawing after the input when there is nothing to add.
    fn prompt_complete(&self, pr: &mut Prompt) -> bool {
        let Some((first, last)) = prompt_word_bounds(pr) else { return false };
        let word: String = pr.input[first..last].iter().collect();
        if word.len() >= 64 || pr.history_type != 0 || first != 0 || word.is_empty() {
            return false;
        }
        let mut list: Vec<String> = super::cmds::command_names().filter(|n| n.starts_with(&word)).map(String::from).collect();
        for (_, alias) in self.opts_server.array_items("command-alias") {
            if let Some((name, _)) = alias.split_once('=') {
                if name.starts_with(&word) && !list.iter().any(|l| l == name) {
                    list.push(name.to_string());
                }
            }
        }
        if list.is_empty() {
            return false;
        }
        list.sort();
        let out = if list.len() == 1 {
            format!("{} ", list[0])
        } else {
            let mut prefix = list[0].clone();
            for item in &list[1..] {
                let common = prefix.chars().zip(item.chars()).take_while(|(a, b)| a == b).count();
                prefix = prefix.chars().take(common).collect();
            }
            prefix
        };
        if out == word {
            if list.len() > 1 {
                pr.complete = list;
            }
            return false;
        }
        prompt_replace_word(pr, &out)
    }
}

/// A command prompt editing operation (the emacs keys of tmux prompt_key).
#[derive(Clone, Copy, PartialEq, Debug)]
enum PromptOp {
    None, Left, Right, Home, End, Tab, BSpace, Delete, ClearLine, KillLine, DeleteWord,
    ForwardWord, ViForward(bool), ViEnd(bool), BackWord(bool), Up, Down, Yank, Transpose,
    Enter, Cancel, QuoteNext, Insert(char), Page, Search(bool),
}

/// What a prompt did with a key (enum prompt_key_result).
#[derive(Clone, Copy, PartialEq, Debug)]
enum PromptResult {
    Handled,
    Close,
    NotHandled,
    Move,
}

/// Why the prompt callback runs (PROMPT_KEY_CLOSE, _HANDLED, _MOVE).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Fire {
    Close,
    Handled,
    Move,
}

/// prompt_keypad_key: unmodified keypad keys are their characters.
fn prompt_keypad_key(k: &Key) -> Key {
    match k.code {
        KeyCode::KP('\r') if k.mods.is_empty() => Key { code: KeyCode::Enter, mods: k.mods },
        KeyCode::KP(c) if k.mods.is_empty() => Key { code: KeyCode::Char(c), mods: k.mods },
        _ => k.clone(),
    }
}

/// A key as the character a single key prompt (or C-v) takes: BSpace is
/// 0x7f, C-x the control character and other modifiers are dropped; keys
/// without a character give None and are ignored.
fn prompt_key_char(k: &Key) -> Option<char> {
    let c = match k.code {
        KeyCode::BSpace => return Some('\x7f'),
        KeyCode::Char(c) => c,
        KeyCode::Space => ' ',
        KeyCode::Enter => '\r',
        KeyCode::Tab => '\t',
        KeyCode::Escape => '\x1b',
        _ => return None,
    };
    Some(if k.mods.ctrl && c.is_ascii() { ((c as u8) & 0x1f) as char } else { c })
}

/// The top buffer as prompt_paste inserts it: up to the first control character.
fn prompt_paste_text(data: &[u8]) -> Vec<char> {
    let end = data.iter().position(|b| *b <= 31 || *b == 127).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).chars().collect()
}

/// The word around the cursor that completion replaces (prompt_replace_complete).
fn prompt_word_bounds(pr: &Prompt) -> Option<(usize, usize)> {
    let n = pr.input.len();
    let sp = |i: usize| i < n && pr.input[i] == ' ';
    let idx = pr.cursor.min(n).saturating_sub(1);
    let mut first = idx;
    while first > 0 && !sp(first) {
        first -= 1;
    }
    while first < n && sp(first) {
        first += 1;
    }
    let mut last = idx;
    while last < n && !sp(last) {
        last += 1;
    }
    while last > 0 && sp(last) {
        last -= 1;
    }
    if last < n {
        last += 1;
    }
    (last >= first).then_some((first, last))
}

/// Replaces the word around the cursor with `s`, the cursor after it.
fn prompt_replace_word(pr: &mut Prompt, s: &str) -> bool {
    let Some((first, last)) = prompt_word_bounds(pr) else { return false };
    let new: Vec<char> = s.chars().collect();
    let len = new.len();
    pr.input.splice(first..last, new);
    pr.cursor = first + len;
    true
}

fn prompt_emacs_op(k: &Key) -> PromptOp {
    let m = k.mods;
    let plain = !m.ctrl && !m.meta;
    match &k.code {
        KeyCode::Char(c) if m.ctrl && !m.meta => match c {
            'b' => PromptOp::Left,
            'f' => PromptOp::Right,
            'a' => PromptOp::Home,
            'e' => PromptOp::End,
            'h' => PromptOp::BSpace,
            'd' => PromptOp::Delete,
            'u' => PromptOp::ClearLine,
            'k' => PromptOp::KillLine,
            'w' => PromptOp::DeleteWord,
            'p' => PromptOp::Up,
            'n' => PromptOp::Down,
            'y' => PromptOp::Yank,
            't' => PromptOp::Transpose,
            'm' | 'j' => PromptOp::Enter,
            'c' | 'g' | '[' => PromptOp::Cancel,
            'v' => PromptOp::QuoteNext,
            'i' => PromptOp::Tab,
            'r' => PromptOp::Search(true),
            's' => PromptOp::Search(false),
            _ => PromptOp::None,
        },
        KeyCode::Char('f') if m.meta && !m.ctrl => PromptOp::ForwardWord,
        KeyCode::Char('b') if m.meta && !m.ctrl => PromptOp::BackWord(false),
        KeyCode::Right if m.ctrl && !m.meta => PromptOp::ForwardWord,
        KeyCode::Left if m.ctrl && !m.meta => PromptOp::BackWord(false),
        _ if !plain || m.shift && !matches!(k.code, KeyCode::Char(_)) => PromptOp::None,
        KeyCode::Char(c) => PromptOp::Insert(*c),
        KeyCode::KP('\r') | KeyCode::Enter => PromptOp::Enter,
        KeyCode::KP(c) => PromptOp::Insert(*c),
        KeyCode::Space => PromptOp::Insert(' '),
        KeyCode::Left => PromptOp::Left,
        KeyCode::Right => PromptOp::Right,
        KeyCode::Home => PromptOp::Home,
        KeyCode::End => PromptOp::End,
        KeyCode::Tab => PromptOp::Tab,
        KeyCode::BSpace => PromptOp::BSpace,
        KeyCode::Delete => PromptOp::Delete,
        KeyCode::Up => PromptOp::Up,
        KeyCode::Down => PromptOp::Down,
        KeyCode::PageUp | KeyCode::PageDown => PromptOp::Page,
        KeyCode::Escape => PromptOp::Cancel,
        _ => PromptOp::None,
    }
}

/// tmux prompt_translate_key: vi keys as emacs operations, switching between
/// insert and command mode. Escape in insert mode moves the cursor left.
fn prompt_vi_op(pr: &mut Prompt, k: &Key) -> PromptOp {
    let name = keys::key_name(k);
    if !pr.vi_command {
        return match name.as_str() {
            "C-a" | "C-c" | "C-e" | "C-g" | "C-h" | "Tab" | "C-k" | "C-n" | "C-p" | "C-t" | "C-u" | "C-v" | "C-w" | "C-y" | "Enter" | "KPEnter"
            | "C-m" | "C-j" | "C-Left" | "C-Right" | "BSpace" | "DC" | "Down" | "End" | "Home" | "Left" | "Right" | "Up" => prompt_emacs_op(k),
            "Escape" | "C-[" => {
                pr.vi_command = true;
                pr.cursor = pr.cursor.saturating_sub(1);
                PromptOp::None
            }
            // anything else is typed (only plain characters insert anything)
            _ => match prompt_emacs_op(k) {
                op @ PromptOp::Insert(_) => op,
                _ => PromptOp::None,
            },
        };
    }
    match name.as_str() {
        "BSpace" => return PromptOp::Left,
        "A" | "I" | "C" | "s" | "a" => pr.vi_command = false,
        "S" => {
            pr.vi_command = false;
            return PromptOp::ClearLine;
        }
        "i" => {
            pr.vi_command = false;
            return PromptOp::None;
        }
        _ => {}
    }
    match name.as_str() {
        "A" | "$" => PromptOp::End,
        "I" | "0" | "^" => PromptOp::Home,
        "C" | "D" => PromptOp::KillLine,
        "X" => PromptOp::BSpace,
        "b" => PromptOp::BackWord(false),
        "B" => PromptOp::BackWord(true),
        "d" => PromptOp::ClearLine,
        "e" => PromptOp::ViEnd(false),
        "E" => PromptOp::ViEnd(true),
        "w" => PromptOp::ViForward(false),
        "W" => PromptOp::ViForward(true),
        "p" => PromptOp::Yank,
        "q" | "C-c" => PromptOp::Cancel,
        "s" | "DC" | "x" => PromptOp::Delete,
        "Down" | "j" => PromptOp::Down,
        "Left" | "h" => PromptOp::Left,
        "a" | "Right" | "l" => PromptOp::Right,
        "Up" | "k" => PromptOp::Up,
        "C-h" => PromptOp::BSpace,
        "Enter" | "KPEnter" | "C-m" | "C-j" => PromptOp::Enter,
        _ => PromptOp::None,
    }
}

// Word movement over the prompt input as tmux prompt.c: separators (single
// characters from word-separators) form words of their own, spaces end words.
fn prompt_space(s: &[char], i: usize) -> bool {
    s.get(i) == Some(&' ')
}

fn prompt_in_list(s: &[char], i: usize, seps: &[char]) -> bool {
    s.get(i).is_some_and(|c| c.is_ascii() && seps.contains(c))
}

/// prompt_forward_word: vi stops at the start of the next word, emacs at the end.
fn prompt_forward_word(s: &[char], mut i: usize, vi: bool, seps: &[char]) -> usize {
    let n = s.len();
    if !vi {
        while i < n && prompt_space(s, i) {
            i += 1;
        }
    }
    if i >= n {
        return n;
    }
    let class = prompt_in_list(s, i, seps) && !prompt_space(s, i);
    loop {
        i += 1;
        if prompt_space(s, i) {
            if vi {
                while i < n && prompt_space(s, i) {
                    i += 1;
                }
            }
            break;
        }
        if i >= n || class != prompt_in_list(s, i, seps) {
            break;
        }
    }
    i
}

/// prompt_end_word: to the last character of this or the next word.
fn prompt_end_word(s: &[char], mut i: usize, seps: &[char]) -> usize {
    let n = s.len();
    if i >= n {
        return i;
    }
    loop {
        i += 1;
        if i >= n {
            return n;
        }
        if !prompt_space(s, i) {
            break;
        }
    }
    let class = prompt_in_list(s, i, seps);
    loop {
        i += 1;
        if i >= n || prompt_space(s, i) || class != prompt_in_list(s, i, seps) {
            break;
        }
    }
    i - 1
}

/// prompt_backward_word (and the start of the word C-w deletes).
fn prompt_backward_word(s: &[char], mut i: usize, seps: &[char]) -> usize {
    while i != 0 {
        i -= 1;
        if !prompt_space(s, i) {
            break;
        }
    }
    let class = prompt_in_list(s, i, seps);
    while i != 0 {
        i -= 1;
        if prompt_space(s, i) || class != prompt_in_list(s, i, seps) {
            i += 1;
            break;
        }
    }
    i
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_words_like_tmux() {
        // positions from tmux 3.8 with the default word-separators
        let seps: Vec<char> = "!\"#$%&'()*+,-./:;<=>?@[\\]^`{|}~".chars().collect();
        let s: Vec<char> = "set foo-bar.baz qux/quux end".chars().collect();
        assert_eq!(prompt_forward_word(&s, 4, true, &seps), 7);
        assert_eq!(prompt_forward_word(&s, 7, true, &seps), 8);
        assert_eq!(prompt_forward_word(&s, 4, true, &[]), 16);
        assert_eq!(prompt_end_word(&s, 4, &seps), 6);
        assert_eq!(prompt_end_word(&s, 6, &seps), 7);
        assert_eq!(prompt_end_word(&s, 4, &[]), 14);
        assert_eq!(prompt_backward_word(&s, 27, &seps), 25);
        assert_eq!(prompt_backward_word(&s, 25, &seps), 20);
        assert_eq!(prompt_backward_word(&s, 20, &seps), 19);
        assert_eq!(prompt_backward_word(&s, 25, &[]), 16);
        // emacs M-f goes to the end of the word
        assert_eq!(prompt_forward_word(&s, 0, false, &seps), 3);
        assert_eq!(prompt_forward_word(&s, 3, false, &seps), 7);
        assert_eq!(prompt_forward_word(&s, 28, false, &seps), 28);
        assert_eq!(prompt_backward_word(&[], 0, &seps), 0);
    }

    #[test]
    fn prompt_vi_translation() {
        let mut pr = Prompt::new(PromptKind::Command, vec![(String::new(), "ab".into())], String::new(), None);
        let key = |n: &str| keys::parse_key_name(n).unwrap();
        // insert mode: Escape is command mode and one to the left; M-f is dropped
        assert_eq!(prompt_vi_op(&mut pr, &key("M-f")), PromptOp::None);
        assert_eq!(prompt_vi_op(&mut pr, &key("Escape")), PromptOp::None);
        assert!(pr.vi_command && pr.cursor == 1);
        // command mode: Escape does nothing, BSpace is left, S clears and inserts
        assert_eq!(prompt_vi_op(&mut pr, &key("Escape")), PromptOp::None);
        assert_eq!(prompt_vi_op(&mut pr, &key("BSpace")), PromptOp::Left);
        assert_eq!(prompt_vi_op(&mut pr, &key("j")), PromptOp::Down);
        assert_eq!(prompt_vi_op(&mut pr, &key("C-a")), PromptOp::None);
        assert_eq!(prompt_vi_op(&mut pr, &key("S")), PromptOp::ClearLine);
        assert!(!pr.vi_command);
        assert_eq!(prompt_vi_op(&mut pr, &key("C-a")), PromptOp::Home);
        assert_eq!(prompt_emacs_op(&key("M-BSpace")), PromptOp::None);
        assert_eq!(prompt_emacs_op(&key("C-Left")), PromptOp::BackWord(false));
        assert_eq!(prompt_emacs_op(&key("C-r")), PromptOp::Search(true));
        // single keys: C-x is the control character, BSpace 0x7f, special keys none
        assert_eq!(prompt_key_char(&key("C-a")), Some('\x01'));
        assert_eq!(prompt_key_char(&key("BSpace")), Some('\x7f'));
        assert_eq!(prompt_key_char(&key("Escape")), Some('\x1b'));
        assert_eq!(prompt_key_char(&key("M-y")), Some('y'));
        assert_eq!(prompt_key_char(&key("Up")), None);
    }

    #[test]
    fn prompt_completion_word() {
        let mut pr = Prompt::new(PromptKind::Command, vec![(String::new(), "kill-se".into())], String::new(), None);
        assert_eq!(prompt_word_bounds(&pr), Some((0, 7)));
        pr.input = "set -g sta".chars().collect();
        pr.cursor = 10;
        assert_eq!(prompt_word_bounds(&pr), Some((7, 10)));
        pr.input = "kill-server ".chars().collect();
        pr.cursor = 12;
        assert_eq!(prompt_word_bounds(&pr), None);
        pr.input = "sel".chars().collect();
        pr.cursor = 3;
        assert!(prompt_replace_word(&mut pr, "select-"));
        assert_eq!((pr.input.iter().collect::<String>(), pr.cursor), ("select-".to_string(), 7));
    }

    #[test]
    fn editor_files_are_new_and_private() {
        let a = editor_file(b"one").unwrap();
        let b = editor_file(b"two").unwrap();
        assert_ne!(a, b);
        assert!(a.file_name().unwrap().to_string_lossy().starts_with("tmux."));
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }

    #[test]
    fn editor_waits_under_powershell() {
        assert_eq!(editor_command("vi", "'/tmp/tmux.x'", false), "vi '/tmp/tmux.x'");
        assert_eq!(editor_command("notepad.exe", "\"C:\\t\\tmux.x\"", true), "notepad.exe \"C:\\t\\tmux.x\" | Out-Null");
    }

    #[test]
    fn templates() {
        assert_eq!(fill_template("rename-window -- '%%'", &["it's".into()]), "rename-window -- 'it'\\''s'");
        assert_eq!(fill_template("a %1 b %2", &["x".into(), "y z".into()]), "a x b y z");
        assert_eq!(fill_template("%%", &["new-window -n x".into()]), "new-window -n x");
        // cmd_template_replace: %%% and %N% escape for double quotes, each answer takes the next %%
        assert_eq!(fill_template("display \"[%%%]\"", &["a\"b$c".into()]), "display \"[a\\\"b\\$c]\"");
        assert_eq!(fill_template("x %1% %1", &["a;b".into()]), "x a\\;b a;b");
        assert_eq!(fill_template("\"%%\"", &["it's".into()]), "\"it'\\''s\"");
        assert_eq!(fill_template("%% %%", &["q".into()]), "q %%");
        assert_eq!(fill_template("%% %%", &["q".into(), "r".into()]), "q r");
        assert_eq!(fill_template("100%", &["q".into()]), "100%");
        let b = default_bindings();
        assert!(b["prefix"].contains_key("%"));
        assert!(b["prefix"].contains_key("\""));
        assert!(b["copy-mode-vi"].len() > 20);
    }
}

/// The byte tmux writes for an unmodified BSpace: the backspace key's code truncated to a byte
/// when it has no modifiers, or its C0 control code when it has only Ctrl.
fn backspace_byte(option: &str) -> Option<u8> {
    let key = keys::parse_key_name(option)?;
    // tmux function key codes start at KEYC_NONE; their low byte is the enum position
    let code: u32 = match &key.code {
        KeyCode::BSpace if option.eq_ignore_ascii_case("BSpace") => 7,
        KeyCode::BSpace => 0x7f,
        KeyCode::Char(c) => { let mut b = [0; 4]; u32::from(c.encode_utf8(&mut b).as_bytes()[0]) }
        KeyCode::Space => 32,
        KeyCode::Enter => 13,
        KeyCode::Tab => 9,
        KeyCode::Escape => 27,
        KeyCode::F(n) => 7 + u32::from(*n),
        KeyCode::Insert => 20, KeyCode::Delete => 21, KeyCode::Home => 22, KeyCode::End => 23,
        KeyCode::PageDown => 24, KeyCode::PageUp => 25, KeyCode::BTab => 26,
        KeyCode::Up => 27, KeyCode::Down => 28, KeyCode::Left => 29, KeyCode::Right => 30,
        KeyCode::KP(c) => 31 + "/*-789+456123\r0.".find(*c)? as u32,
        _ => return None,
    };
    let m = key.mods;
    if !m.ctrl && !m.meta && !m.shift { return Some(code as u8); }
    if !m.ctrl || m.meta || m.shift { return None; }
    match code {
        0x3f => Some(0x7f),
        0x20 => option.ends_with('@').then_some(0),
        0x40..=0x5f => Some((code - 0x40) as u8),
        0x61..=0x7a => Some((code - 0x60) as u8),
        _ => None,
    }
}

/// The selection characters tmux accepts from an OSC 52 selector, each once.
fn clipboard_selector(selector: &[u8]) -> String {
    let mut clip = String::new();
    for &b in selector {
        if b"cpqs01234567".contains(&b) && !clip.contains(b as char) { clip.push(b as char); }
    }
    clip
}

/// OSC 52 data is decoded with b64_pton (whitespace skipped).
fn decode_osc52(data: &[u8]) -> Result<Vec<u8>, String> {
    super::cmds::base64_decode(data).ok_or_else(|| "invalid base64".to_string())
}

/// input_reply_clipboard: `ESC ] 52 ; [clip] ; base64 terminator`.
fn clipboard_reply_bytes(clip: Option<u8>, data: &[u8], bel: bool) -> Vec<u8> {
    let mut reply = b"\x1b]52;".to_vec();
    reply.extend(clip);
    reply.push(b';');
    if !data.is_empty() { reply.extend(super::cmds::base64(data).into_bytes()); }
    reply.extend(if bel { &b"\x07"[..] } else { &b"\x1b\\"[..] });
    reply
}

/// tmux 3.8 prompt history types (prompt_type_string).
pub const PROMPT_TYPES: [&str; 2] = ["command", "search"];

pub fn prompt_type(name: &str) -> Option<usize> {
    PROMPT_TYPES.iter().position(|t| *t == name)
}

/// "editor file" (spawn_editor). PowerShell does not wait for a GUI program such as the
/// default notepad.exe to exit, so the file would be read (and removed) while it is still
/// being edited; piping the program to Out-Null makes PowerShell wait for it.
fn editor_command(editor: &str, quoted: &str, powershell: bool) -> String {
    if powershell {
        format!("{editor} {quoted} | Out-Null")
    } else {
        format!("{editor} {quoted}")
    }
}

/// mkstemp(_PATH_TMP "tmux.XXXXXXXX"): a new file only this user can read, created
/// exclusively under a random name (never through an existing file or link).
fn editor_file(data: &[u8]) -> std::io::Result<std::path::PathBuf> {
    use std::hash::{BuildHasher, Hasher};
    use std::io::Write;
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let dir = std::env::temp_dir();
    let mut last = None;
    for attempt in 0..100u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(now_ms() ^ attempt);
        h.write_u32(std::process::id());
        let mut n = h.finish();
        let name: String = (0..8)
            .map(|_| {
                let c = CHARS[(n % CHARS.len() as u64) as usize] as char;
                n /= CHARS.len() as u64;
                c
            })
            .collect();
        let path = dir.join(format!("tmux.{name}"));
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        match o.open(&path) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(data) {
                    let _ = std::fs::remove_file(&path);
                    return Err(e);
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no temporary file")))
}
