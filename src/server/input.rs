//! Client input: key tables, prompts, overlays, pane modes and the mouse.

use super::modes::{MenuAction, TreeAction};
use super::*;
use crate::copymode::Action;
use crate::{cmdparse, opts};
use crate::keys::{Input, Key, KeyCode};

pub use super::mouse::MouseCtx;

/// Mouse bindings of the copy-mode and copy-mode-vi tables.
const COPY_MODE_MOUSE: &[(&str, &str)] = &[
    ("MouseDown1Pane", "select-pane"),
    ("MouseDrag1Pane", "select-pane ; send-keys -X begin-selection"),
    ("MouseDragEnd1Pane", "send-keys -X copy-pipe-and-cancel"),
    ("WheelUpPane", "select-pane ; send-keys -X -N 5 scroll-up"),
    ("WheelDownPane", "select-pane ; send-keys -X -N 5 scroll-down"),
    ("DoubleClick1Pane", "select-pane ; send-keys -X select-word ; run-shell -d 0.3 ; send-keys -X copy-pipe-and-cancel"),
    ("TripleClick1Pane", "select-pane ; send-keys -X select-line ; run-shell -d 0.3 ; send-keys -X copy-pipe-and-cancel"),
];

pub fn default_bindings() -> BTreeMap<String, BTreeMap<String, Binding>> {
    let mut t: BTreeMap<String, BTreeMap<String, Binding>> = BTreeMap::new();
    for words in cmdparse::parse(include_str!("display-defaults.conf")).expect("tmux default bindings") {
        let o = opts::getopt(&words[1..], "nr", "TN").expect("default binding options");
        let table = if o.has('n') { "root" } else { o.get('T').unwrap_or("prefix") };
        let key = &o.rest[0];
        let key = keys::parse_key_name(key).map(|k| keys::key_name(&k)).unwrap_or_else(|| key.clone());
        let args = &o.rest[1..];
        let cmd = if args.len() == 1 { args[0].trim().to_string() } else {
            args.iter().map(|a| cmdparse::quote(a)).collect::<Vec<_>>().join(" ")
        };
        t.entry(table.into()).or_default().insert(key, Binding {
            cmd, repeat: o.has('r'), note: o.get('N').unwrap_or("").into(),
        });
    }
    // Tiled-only forms of the floating-aware resize bindings (the fixture leaves
    // the tmux 3.8 if-shell versions to the floating pane package).
    for (key, cmd, note) in [
        ("M-Up", "resize-pane -U 5", "Resize the pane up by 5"),
        ("M-Left", "resize-pane -L 5", "Resize the pane left by 5"),
        ("C-Up", "resize-pane -U", "Resize the pane up"),
        ("C-Left", "resize-pane -L", "Resize the pane left"),
    ] {
        t.entry("prefix".into()).or_default().entry(key.into()).or_insert(Binding { cmd: cmd.into(), repeat: true, note: note.into() });
    }
    for (table, vi) in [("copy-mode", false), ("copy-mode-vi", true)] {
        for (k, c) in CopyMode::default_bindings(vi) {
            t.entry(table.to_string()).or_default().insert(k.to_string(), Binding { cmd: format!("send-keys -X {c}"), repeat: false, note: "default".into() });
        }
        for (k, c) in COPY_MODE_MOUSE {
            t.entry(table.to_string()).or_default().insert(k.to_string(), Binding { cmd: c.to_string(), repeat: false, note: String::new() });
        }
    }
    t
}

fn escape_for(template: &str, at: usize, input: &str) -> String {
    let before = template[..at].chars().last();
    match before {
        Some('\'') => input.replace('\'', "'\\''"),
        Some('"') => input.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$"),
        // unquoted: inserted as-is, so a bare %% / %1 template runs the input as a command
        _ => input.to_string(),
    }
}

/// Replaces %1..%9 and successive %% in a prompt template.
pub fn fill_template(template: &str, answers: &[String]) -> String {
    let mut out = String::new();
    let mut next = 0;
    let b = template.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 1 < b.len() {
            let c = b[i + 1];
            if c == b'%' {
                let a = answers.get(next).or(answers.last()).cloned().unwrap_or_default();
                next += 1;
                out.push_str(&escape_for(template, i, &a));
                i += 2;
                continue;
            }
            if c.is_ascii_digit() && c != b'0' {
                let a = answers.get((c - b'1') as usize).cloned().unwrap_or_default();
                out.push_str(&escape_for(template, i, &a));
                i += 2;
                continue;
            }
        }
        let ch = template[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

impl Server {
    pub fn handle_input(&mut self, cid: ClientId, data: &[u8]) {
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let inputs = c.keyp.feed(data);
        c.esc_deadline = if c.keyp.has_pending() {
            let ms = self.opts_server.get("escape-time").and_then(|v| v.parse().ok()).unwrap_or(10u64);
            Some(Instant::now() + Duration::from_millis(ms))
        } else {
            None
        };
        for i in inputs {
            self.dispatch_input(cid, i);
        }
    }

    pub fn dispatch_input(&mut self, cid: ClientId, input: Input) {
        if self.client_is_readonly(Some(cid)) && !matches!(&input, Input::Key(_, _)) {
            return;
        }
        match input {
            Input::Key(k, raw) => self.handle_key(cid, k, raw),
            Input::Mouse(m, _) => self.handle_mouse(cid, m),
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
                if let Some(target) = self.input_target(cid) {
                    self.paste(target, &data, true, true, None);
                }
            }
            Input::Unknown(raw) => {
                // focus reports are only forwarded when the pane asked for them
                if raw == b"\x1b[I" || raw == b"\x1b[O" {
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
        let Some(p) = self.panes.get(&pane) else { return };
        if p.options.get("@input-disabled") == Some("on") {
            return;
        }
        let app = p.screen().application_cursor();
        // extended-keys: on = only for applications that asked (CSI > 4 ; n m), always = everyone
        let requested = p.parser.callbacks().ext_keys;
        let csi_u = self.opt_server("extended-keys-format") == "csi-u";
        let extended = match self.opt_server("extended-keys").as_str() {
            "always" => keys::encode_extended(k, requested >= 2, csi_u),
            "on" if requested > 0 => keys::encode_extended(k, requested >= 2, csi_u),
            _ => None,
        };
        let bytes = if let Some(e) = extended {
            e
        } else if matches!(k.code, KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End) && k.mods.is_empty() {
            keys::encode_key(k, app)
        } else if raw.starts_with(b"\x1b[") && (raw.ends_with(b"u") || raw.starts_with(b"\x1b[27;")) {
            // the outer terminal sent an extended key but this pane didn't ask for them
            keys::encode_key(k, app)
        } else {
            raw
        };
        let wid = self.window_of(pane);
        if wid.is_some_and(|w| self.opt_window_on(Some(w), "synchronize-panes")) {
            let panes = self.windows[&wid.unwrap()].panes();
            for q in panes {
                if let Some(pp) = self.panes.get(&q) {
                    if pp.mode.is_none() && pp.options.get("@input-disabled") != Some("on") {
                        pp.send(bytes.clone());
                    }
                }
            }
        } else {
            p.send(bytes);
        }
    }

    fn handle_key(&mut self, cid: ClientId, k: Key, raw: Vec<u8>) {
        let name = keys::key_name(&k);
        let Some(c) = self.clients.get(&cid) else { return };
        if c.is_readonly() && (c.prompt.is_some() || c.overlay.is_some()) { return; }
        // a key clears a message (and is then handled), unless display -N
        if c.message.is_some() && !c.is_readonly() {
            if c.message_ignore_keys { return; }
            let c = self.clients.get_mut(&cid).unwrap();
            c.message = None;
            c.dirty = true;
        }
        let c = &self.clients[&cid];
        if c.prompt.is_some() {
            self.prompt_key(cid, &k, &name);
            return;
        }
        // overlays
        match &c.overlay {
            Some(Overlay::Menu(_)) => {
                let action = match &mut self.clients.get_mut(&cid).unwrap().overlay {
                    Some(Overlay::Menu(m)) => m.key(&name),
                    _ => MenuAction::None,
                };
                self.menu_action(cid, action);
                return;
            }
            Some(Overlay::DisplayPanes { template, .. }) => {
                let template = template.clone();
                let c = self.clients.get_mut(&cid).unwrap();
                c.overlay = None;
                c.dirty = true;
                if let KeyCode::Char(ch) = k.code {
                    if let Some(d) = ch.to_digit(10) {
                        if let Some(sid) = self.client_session(cid) {
                            let wid = self.sessions[&sid].active_window();
                            let base = self.opt_window_num(Some(wid), "pane-base-index").max(0) as u32;
                            let panes = self.windows[&wid].panes();
                            if let Some(p) = d.checked_sub(base).and_then(|i| panes.get(i as usize)) {
                                let cmd = fill_template(&template, &[format!("%{p}")]);
                                self.run_string(cid, &cmd, Some(*p));
                            }
                        }
                    }
                }
                return;
            }
            Some(Overlay::Popup(p)) => {
                let pane = p.pane;
                if self.panes.get(&pane).is_some_and(|x| x.dead.is_some()) {
                    // any key closes a popup whose command has exited
                    let c = self.clients.get_mut(&cid).unwrap();
                    c.overlay = None;
                    c.dirty = true;
                    self.destroy_pane(pane);
                    return;
                }
                self.send_key_to_pane(cid, pane, &k, raw);
                return;
            }
            None => {}
        }
        let Some(sid) = self.client_session(cid) else { return };
        let mut table = self.clients[&cid].key_table.clone();
        let repeating = self.clients[&cid].repeat_until.is_some_and(|d| d > Instant::now());
        if table == "root" {
            let prefix = self.opt_session(Some(sid), "prefix");
            let prefix2 = self.opt_session(Some(sid), "prefix2");
            let is_prefix = |p: &str| p != "None" && keys::parse_key_name(p).is_some_and(|pk| keys::key_name(&pk) == name);
            if is_prefix(&prefix) || is_prefix(&prefix2) {
                // root bindings for the prefix key itself take precedence (tmux checks root first)
                if !self.bindings.get("root").is_some_and(|t| t.contains_key(&name)) {
                    let c = self.clients.get_mut(&cid).unwrap();
                    c.key_table = "prefix".into();
                    c.repeat_until = None;
                    c.dirty = true;
                    return;
                }
            }
        }
        let pane = self.client_active_pane(cid);
        loop {
            let found = self.bindings.get(&table).and_then(|t| t.get(&name)).map(|b| (b.cmd.clone(), b.repeat));
            match found {
                Some((cmd, repeat)) => {
                    let ms = self.opt_session_num(Some(sid), "repeat-time").max(0) as u64;
                    let c = self.clients.get_mut(&cid).unwrap();
                    if repeat && table != "root" {
                        c.repeat_until = Some(Instant::now() + Duration::from_millis(ms));
                        c.key_table = table.clone();
                    } else {
                        c.repeat_until = None;
                        c.key_table = "root".into();
                    }
                    c.dirty = true;
                    self.run_string(cid, &cmd, pane);
                    return;
                }
                None if table != "root" => {
                    let c = self.clients.get_mut(&cid).unwrap();
                    c.key_table = "root".into();
                    c.repeat_until = None;
                    c.dirty = true;
                    if !repeating {
                        return; // unbound key after the prefix is discarded
                    }
                    table = "root".into();
                }
                None => break,
            }
        }
        // not bound: to the pane (or its mode)
        let Some(pane) = pane else { return };
        if self.panes.get(&pane).is_some_and(|p| p.mode.is_some()) {
            self.mode_key(pane, Some(cid), &name);
            return;
        }
        self.send_key_to_pane(cid, pane, &k, raw);
    }

    /// Runs a command string for a client (key bindings, prompts, menus).
    pub fn run_string(&mut self, cid: ClientId, cmd: &str, pane: Option<PaneId>) {
        match crate::cmdparse::parse(cmd) {
            Ok(cmds) => {
                let ctx = Ctx { client: Some(cid), pane, from_key: true, ..Ctx::default() };
                self.run_queue(Queue { cmds: cmds.into(), ctx , soft: 0 });
            }
            Err(e) => self.set_message(cid, &e),
        }
    }

    pub(crate) fn menu_action(&mut self, cid: ClientId, action: MenuAction) {
        match action {
            MenuAction::None => {
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.dirty = true;
                }
            }
            MenuAction::Cancel => {
                let c = self.clients.get_mut(&cid).unwrap();
                c.overlay = None;
                c.dirty = true;
            }
            MenuAction::Run(cmd) => {
                let c = self.clients.get_mut(&cid).unwrap();
                let pane = match c.overlay.take() {
                    Some(Overlay::Menu(m)) => m.pane,
                    _ => None,
                };
                c.dirty = true;
                self.run_string(cid, &cmd, pane.or_else(|| self.client_active_pane(cid)));
            }
        }
    }

    /// A key for a pane that is in a mode (copy, tree, clock).
    pub fn mode_key(&mut self, pane: PaneId, cid: Option<ClientId>, name: &str) {
        if self.client_is_readonly(cid) { return; }
        let Some(p) = self.panes.get_mut(&pane) else { return };
        match &mut p.mode {
            Some(PaneMode::Clock) => self.exit_mode(pane),
            Some(PaneMode::Tree(_)) => {
                let h = p.size.1 as usize;
                let Some(PaneMode::Tree(mut tree)) = p.mode.take() else { return };
                let action = tree.key(name, tree.list_height(h), self);
                // the client that opened the tree, if it is still attached
                let client = tree.client.filter(|c| self.clients.get(c).is_some_and(|c| c.attached)).or(cid);
                match action {
                    TreeAction::None => {
                        self.panes.get_mut(&pane).unwrap().mode = Some(PaneMode::Tree(tree));
                    }
                    TreeAction::Cancel => {}
                    TreeAction::Run(cmd) => {
                        let kill = name == "x" || name == "X";
                        if kill {
                            self.panes.get_mut(&pane).unwrap().mode = Some(PaneMode::Tree(tree));
                        }
                        if let Some(c) = client {
                            self.run_string(c, &cmd, Some(pane));
                        } else {
                            let cmds = crate::cmdparse::parse(&cmd).unwrap_or_default();
                            self.run_queue(Queue { cmds: cmds.into(), ctx: Ctx { pane: Some(pane), ..Ctx::default() } , soft: 0 });
                        }
                        if kill {
                            if let Some(PaneMode::Tree(mut t)) = self.panes.get_mut(&pane).and_then(|p| p.mode.take()) {
                                t.rebuild(self);
                                if let Some(p) = self.panes.get_mut(&pane) {
                                    p.mode = Some(PaneMode::Tree(t));
                                }
                            }
                        }
                    }
                }
                // unzoom if the tree zoomed the window
                if self.panes.get(&pane).is_some_and(|p| p.mode.is_none()) {
                    if let Some(wid) = self.window_of(pane) {
                        if self.windows[&wid].zoomed {
                            self.windows.get_mut(&wid).unwrap().zoomed = false;
                            self.apply_layout(wid);
                        }
                    }
                }
                if let Some(wid) = self.window_of(pane) {
                    self.mark_window_dirty(wid);
                }
            }
            Some(PaneMode::Copy(_)) => {
                let wrap = self.opt_window_on(self.window_of(pane), "wrap-search");
                let vi = self.opt_window(self.window_of(pane), "mode-keys") == "vi";
                let table = if vi { "copy-mode-vi" } else { "copy-mode" };
                let binding = self.bindings.get(table).and_then(|t| t.get(name)).map(|b| (b.cmd.clone(), b.note == "default"));
                match binding {
                    Some((cmd, false)) => {
                        // user binding in the copy-mode table
                        if let Some(c) = cid {
                            self.run_string(c, &cmd, Some(pane));
                        }
                    }
                    _ => {
                        let before = self.copy_scroll_offset(pane);
                        let action = match self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                            Some(PaneMode::Copy(cm)) => { cm.wrap_search = wrap; cm.key(name) },
                            _ => return,
                        };
                        self.copy_action(pane, cid, action, None);
                        self.show_scrollbar_if_scrolled(pane, before, false);
                    }
                }
            }
            None => {}
        }
    }

    pub fn copy_mode_command(&mut self, pane: PaneId, cid: Option<ClientId>, name: &str, args: &[String]) {
        // copy-pipe without a command uses copy-command
        let piped = name.starts_with("copy-pipe");
        let pipe = if !piped {
            None
        } else if !args.is_empty() {
            Some(args[0].clone())
        } else {
            Some(self.opt_server("copy-command")).filter(|c| !c.is_empty())
        };
        let run_args: Vec<String> = if piped && !args.is_empty() { args[1..].to_vec() } else { args.to_vec() };
        // these always show an auto-hide scrollbar; others when the view scrolled
        let always = matches!(name, "page-up" | "page-down" | "page-down-and-cancel" | "halfpage-up" | "halfpage-down"
            | "halfpage-down-and-cancel" | "history-top" | "history-bottom" | "scroll-to-mouse");
        let before = self.copy_scroll_offset(pane);
        self.copy_mode_command_inner(pane, cid, name, pipe, run_args);
        self.show_scrollbar_if_scrolled(pane, before, always);
    }

    fn copy_mode_command_inner(&mut self, pane: PaneId, cid: Option<ClientId>, name: &str, pipe: Option<String>, run_args: Vec<String>) {
        let wrap = options::is_on(&self.opt_window(self.window_of(pane), "wrap-search"));
        if name == "scroll-to-mouse" {
            if let Some(cid) = cid {
                if let Some(m) = self.cur_mouse.clone() {
                    if let Some(c) = self.clients.get_mut(&cid) { c.drag = Some(Drag { kind: DragKind::Scrollbar { pane } }); }
                    self.scrollbar_to_mouse(pane, &m);
                }
            }
            return;
        }
        let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) else {
            if let Some(PaneMode::Tree(_)) = self.panes.get(&pane).and_then(|p| p.mode.as_ref()) {
                // tree mode understands a few -X commands
                let key = match name {
                    "cursor-up" => "Up",
                    "cursor-down" => "Down",
                    "cancel" => "q",
                    _ => "",
                };
                if !key.is_empty() {
                    self.mode_key(pane, cid, key);
                }
            }
            return;
        };
        cm.wrap_search = wrap;
        let action = cm.run(name, &run_args);
        self.copy_action(pane, cid, action, pipe);
    }

    fn copy_action(&mut self, pane: PaneId, cid: Option<ClientId>, action: Action, pipe: Option<String>) {
        match action {
            Action::None => {}
            Action::Cancel => self.exit_mode(pane),
            Action::Copy { text, cancel } => {
                if let Some(cmd) = pipe {
                    let mut c = procinfo::job_command(&cmd);
                    c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
                    if let Ok(mut child) = c.spawn() {
                        let data = text.clone();
                        thread::spawn(move || {
                            if let Some(mut si) = child.stdin.take() {
                                let _ = si.write_all(data.as_bytes());
                            }
                            let _ = child.wait();
                        });
                    }
                }
                self.copy_text(text, cid);
                if cancel {
                    self.exit_mode(pane);
                }
            }
            Action::Prompt { label, command } => {
                if let Some(c) = cid {
                    // the pane is the prompt's context target (a %id in the template would read as %N)
                    let template = format!("send-keys -X {command} '%%'");
                    self.open_prompt(c, PromptKind::Command, vec![(label, String::new())], template, Some(pane), false, false);
                    if command.ends_with("-incremental") {
                        if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                            cm.begin_incremental_search();
                        }
                        if let Some(pr) = self.clients.get_mut(&c).and_then(|c| c.prompt.as_mut()) {
                            pr.copy_incremental = Some((pane, command.starts_with("search-forward")));
                        }
                    }
                }
            }
        }
        if let Some(wid) = self.window_of(pane) {
            self.mark_window_dirty(wid);
        }
    }

    // ---- prompt ----------------------------------------------------------------------

    fn prompt_key(&mut self, cid: ClientId, k: &Key, name: &str) {
        let vi = self.opt_session(self.client_session(cid), "status-keys") == "vi";
        let seps: Vec<char> = self.opt_session(self.client_session(cid), "word-separators").chars().collect();
        // C-y without a C-w text inserts the top buffer up to a control character
        let paste = self.buffers.iter().find(|b| b.automatic).map(|b| String::from_utf8_lossy(&b.data).into_owned()).unwrap_or_default();
        let hist_len = self.prompt_history.len();
        let c = self.clients.get_mut(&cid).unwrap();
        c.dirty = true;
        let pr = c.prompt.as_mut().unwrap();
        let incremental = pr.copy_incremental;
        match pr.kind {
            PromptKind::Confirm => {
                let (key, default_yes) = pr.confirm;
                let yes = (k.code == KeyCode::Char(key) && !k.mods.ctrl && !k.mods.meta) || (k.code == KeyCode::Enter && default_yes);
                let pr = c.prompt.take().unwrap();
                if yes {
                    self.run_string(cid, &pr.template, pr.pane);
                }
                return;
            }
            PromptKind::Key => {
                let pr = c.prompt.take().unwrap();
                let cmd = fill_template(&pr.template, &[name.to_string()]);
                self.run_string(cid, &cmd, pr.pane);
                return;
            }
            PromptKind::Command => {}
        }
        // tmux prompt_key: a quoted key (C-v) is inserted as its character
        if pr.quote_next {
            pr.quote_next = false;
            let ch = match &k.code {
                KeyCode::BSpace => Some('\x7f'),
                KeyCode::Char(ch) if k.mods.ctrl => Some(((*ch as u8) & 0x1f) as char),
                KeyCode::Char(ch) if !k.mods.meta => Some(*ch),
                KeyCode::Space => Some(' '),
                KeyCode::Tab => Some('\t'),
                KeyCode::Enter => Some('\r'),
                KeyCode::Escape => Some('\x1b'),
                _ => None,
            };
            if let Some(ch) = ch {
                pr.input.insert(pr.cursor, ch);
                pr.cursor += 1;
            }
            return;
        }
        let op = if vi && !pr.single_key { prompt_vi_op(pr, k) } else { prompt_emacs_op(k) };
        let n = pr.input.len();
        match op {
            PromptOp::None => {}
            PromptOp::Enter => {
                let mut pr = c.prompt.take().unwrap();
                if let Some((pane, _)) = incremental {
                    if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                        cm.finish_incremental_search(false);
                    }
                    return;
                }
                let input: String = pr.input.iter().collect();
                pr.answers.push(input.clone());
                if !input.is_empty() && !pr.template.starts_with("send-keys -X ") {
                    self.prompt_history.retain(|h| h != &input);
                    self.prompt_history.push(input);
                    let lim = self.opt_server_num("prompt-history-limit").max(1) as usize;
                    while self.prompt_history.len() > lim {
                        self.prompt_history.remove(0);
                    }
                }
                if pr.answers.len() < pr.prompts.len() {
                    let (label, init) = pr.prompts[pr.answers.len()].clone();
                    pr.label = label;
                    pr.input = init.chars().collect();
                    pr.cursor = pr.input.len();
                    self.clients.get_mut(&cid).unwrap().prompt = Some(pr);
                    return;
                }
                if pr.answers.iter().all(|a| a.is_empty()) && pr.template != "%1" && pr.template.contains('%') {
                    return; // empty input cancels, like tmux
                }
                let cmd = fill_template(&pr.template, &pr.answers);
                self.run_string(cid, &cmd, pr.pane);
            }
            PromptOp::Cancel => c.prompt = None,
            PromptOp::BSpace if pr.bspace_exit && n == 0 => c.prompt = None,
            PromptOp::BSpace => {
                if pr.cursor > 0 {
                    pr.cursor -= 1;
                    pr.input.remove(pr.cursor);
                }
            }
            PromptOp::Delete => {
                if pr.cursor < n {
                    pr.input.remove(pr.cursor);
                }
            }
            PromptOp::Left => pr.cursor = pr.cursor.saturating_sub(1),
            PromptOp::Right => pr.cursor = (pr.cursor + 1).min(n),
            PromptOp::Home => pr.cursor = 0,
            PromptOp::End => pr.cursor = n,
            // C-u and C-k don't fill the C-y buffer in tmux; only C-w does
            PromptOp::ClearLine => {
                pr.input.clear();
                pr.cursor = 0;
            }
            PromptOp::KillLine => pr.input.truncate(pr.cursor),
            PromptOp::DeleteWord => {
                let i = prompt_backward_word(&pr.input, pr.cursor, &seps);
                pr.copied = pr.input.drain(i..pr.cursor).collect();
                pr.cursor = i;
            }
            PromptOp::ForwardWord => pr.cursor = prompt_forward_word(&pr.input, pr.cursor, false, &seps),
            PromptOp::ViForward(big) => pr.cursor = prompt_forward_word(&pr.input, pr.cursor, true, if big { &[] } else { &seps }),
            PromptOp::ViEnd(big) => pr.cursor = prompt_end_word(&pr.input, pr.cursor, if big { &[] } else { &seps }),
            PromptOp::BackWord(big) => pr.cursor = prompt_backward_word(&pr.input, pr.cursor, if big { &[] } else { &seps }),
            PromptOp::Up => {
                if hist_len > 0 && pr.history_pos != Some(0) {
                    let pos = pr.history_pos.map(|p| p.min(hist_len).saturating_sub(1)).unwrap_or(hist_len - 1);
                    pr.history_pos = Some(pos);
                    pr.input = self.prompt_history[pos].chars().collect();
                    pr.cursor = pr.input.len();
                }
            }
            PromptOp::Down => {
                // past the newest entry the line is empty (prompt_down_history)
                match pr.history_pos.map(|p| p.min(hist_len)) {
                    Some(p) if p + 1 < hist_len => {
                        pr.history_pos = Some(p + 1);
                        pr.input = self.prompt_history[p + 1].chars().collect();
                    }
                    _ => {
                        pr.history_pos = None;
                        pr.input.clear();
                    }
                }
                pr.cursor = pr.input.len();
            }
            PromptOp::Yank => {
                let value: Vec<_> = if pr.copied.is_empty() { paste.chars().take_while(|c| !c.is_control()).collect() } else { pr.copied.clone() };
                let len = value.len();
                pr.input.splice(pr.cursor..pr.cursor, value);
                pr.cursor += len;
            }
            PromptOp::Transpose => {
                let i = if pr.cursor < n { pr.cursor + 1 } else { pr.cursor };
                if i >= 2 {
                    pr.input.swap(i - 2, i - 1);
                    pr.cursor = i;
                }
            }
            PromptOp::QuoteNext => pr.quote_next = true,
            PromptOp::Tab => {
                // complete a command name at the start
                let text: String = pr.input.iter().collect();
                if !text.contains(' ') {
                    let names: Vec<&str> = COMMAND_NAMES.iter().copied().filter(|n| n.starts_with(&text)).collect();
                    if names.len() == 1 {
                        pr.input = format!("{} ", names[0]).chars().collect();
                        pr.cursor = pr.input.len();
                    }
                }
            }
            PromptOp::Insert(ch) => {
                if pr.numeric && !ch.is_ascii_digit() {
                    return;
                }
                pr.input.insert(pr.cursor, ch);
                pr.cursor += 1;
                if pr.single_key {
                    let mut pr = c.prompt.take().unwrap();
                    pr.answers.push(pr.input.iter().collect());
                    let cmd = fill_template(&pr.template, &pr.answers);
                    self.run_string(cid, &cmd, pr.pane);
                }
            }
        }
        if let Some((pane, forward)) = incremental {
            let text = self.clients.get(&cid).and_then(|c| c.prompt.as_ref()).map(|pr| pr.input.iter().collect::<String>());
            let wrap = self.opt_window_on(self.window_of(pane), "wrap-search");
            if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                cm.wrap_search = wrap;
                match text {
                    Some(text) => cm.preview_search(&text, forward),
                    None => cm.finish_incremental_search(false),
                }
            }
            if let Some(wid) = self.window_of(pane) { self.mark_window_dirty(wid); }
        }
    }

}

/// A command prompt editing operation (the emacs keys of tmux prompt_key).
#[derive(Clone, Copy, PartialEq, Debug)]
enum PromptOp {
    None, Left, Right, Home, End, Tab, BSpace, Delete, ClearLine, KillLine, DeleteWord,
    ForwardWord, ViForward(bool), ViEnd(bool), BackWord(bool), Up, Down, Yank, Transpose,
    Enter, Cancel, QuoteNext, Insert(char),
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

pub const COMMAND_NAMES: &[&str] = &[
    "attach-session", "bind-key", "break-pane", "capture-pane", "choose-buffer", "choose-client", "choose-tree", "clear-history", "clock-mode", "command-prompt",
    "confirm-before", "copy-mode", "customize-mode", "delete-buffer", "detach-client", "display-menu", "display-message", "display-panes", "display-popup", "find-window",
    "has-session", "if-shell", "join-pane", "kill-pane", "kill-server", "kill-session", "kill-window", "last-pane", "last-window", "link-window", "list-buffers",
    "list-clients", "list-commands", "list-keys", "list-panes", "list-sessions", "list-windows", "load-buffer", "lock-client", "lock-server", "lock-session", "move-pane",
    "move-window", "new-session", "new-window", "next-layout", "next-window", "paste-buffer", "pipe-pane", "previous-layout", "previous-window", "refresh-client",
    "rename-session", "rename-window", "resize-pane", "resize-window", "respawn-pane", "respawn-window", "rotate-window", "run-shell", "save-buffer", "select-layout",
    "select-pane", "select-window", "send-keys", "send-prefix", "set-buffer", "set-environment", "set-hook", "set-option", "set-window-option", "show-buffer",
    "show-environment", "show-hooks", "show-messages", "show-options", "show-window-options", "source-file", "split-window", "start-server", "suspend-client",
    "swap-pane", "swap-window", "switch-client", "unbind-key", "unlink-window", "wait-for",
];

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
        let mut pr = Prompt {
            vi_command: false, quote_next: false, copied: Vec::new(), kind: PromptKind::Command, prompts: Vec::new(), answers: Vec::new(),
            label: String::new(), input: "ab".chars().collect(), cursor: 2, template: String::new(), history_pos: None, pane: None,
            numeric: false, single_key: false, copy_incremental: None, bspace_exit: false, confirm: ('y', false),
        };
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
    }

    #[test]
    fn templates() {
        assert_eq!(fill_template("rename-window -- '%%'", &["it's".into()]), "rename-window -- 'it'\\''s'");
        assert_eq!(fill_template("a %1 b %2", &["x".into(), "y z".into()]), "a x b y z");
        assert_eq!(fill_template("%%", &["new-window -n x".into()]), "new-window -n x");
        let b = default_bindings();
        assert!(b["prefix"].contains_key("%"));
        assert!(b["prefix"].contains_key("\""));
        assert!(b["copy-mode-vi"].len() > 20);
    }
}
