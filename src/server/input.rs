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
                self.show_scrollbar(pane);
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
                        let action = match self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                            Some(PaneMode::Copy(cm)) => { cm.wrap_search = wrap; cm.key(name) },
                            _ => return,
                        };
                        self.copy_action(pane, cid, action, None);
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
        self.show_scrollbar(pane);
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
        let paste = self.buffers.last().map(|b| String::from_utf8_lossy(&b.data).into_owned()).unwrap_or_default();
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
        if vi && !pr.single_key {
            if k.code == KeyCode::Escape {
                if !pr.vi_command { pr.cursor = pr.cursor.saturating_sub(1); }
                pr.vi_command = true;
                return;
            }
            if pr.vi_command {
                let mapped = match name {
                    "i" => { pr.vi_command = false; return; }
                    "a" => { pr.vi_command = false; pr.cursor = (pr.cursor + 1).min(pr.input.len()); return; }
                    "I" | "S" => {
                        pr.vi_command = false; pr.cursor = 0;
                        if name == "S" { pr.input.clear(); }
                        return;
                    }
                    "A" => { pr.vi_command = false; pr.cursor = pr.input.len(); return; }
                    "s" => { pr.vi_command = false; if pr.cursor < pr.input.len() { pr.input.remove(pr.cursor); } return; }
                    "D" | "C" => { pr.input.truncate(pr.cursor); if name == "C" { pr.vi_command = false; } return; }
                    "e" | "E" => {
                        pr.cursor = (pr.cursor + 1).min(pr.input.len());
                        while pr.cursor < pr.input.len() && pr.input[pr.cursor].is_whitespace() { pr.cursor += 1; }
                        while pr.cursor + 1 < pr.input.len() && !pr.input[pr.cursor + 1].is_whitespace() { pr.cursor += 1; }
                        return;
                    }
                    "w" | "W" => {
                        while pr.cursor < pr.input.len() && !pr.input[pr.cursor].is_whitespace() { pr.cursor += 1; }
                        while pr.cursor < pr.input.len() && pr.input[pr.cursor].is_whitespace() { pr.cursor += 1; }
                        return;
                    }
                    "b" | "B" => {
                        pr.cursor = pr.cursor.saturating_sub(1);
                        while pr.cursor > 0 && pr.input[pr.cursor].is_whitespace() { pr.cursor -= 1; }
                        while pr.cursor > 0 && !pr.input[pr.cursor - 1].is_whitespace() { pr.cursor -= 1; }
                        return;
                    }
                    "h" => "Left", "l" => "Right", "j" => "Down", "k" => "Up",
                    "0" | "^" => "Home", "$" => "End", "x" => "DC", "X" => "BSpace",
                    "BSpace" => "Left", "DC" => "DC", "Left" => "Left", "Right" => "Right", "Up" => "Up", "Down" => "Down",
                    "d" => "C-u", "p" => "C-y", "q" => "C-c",
                    "Enter" => "Enter", "C-c" => "C-c", "C-h" => "C-h",
                    _ => return,
                };
                if let Some(key) = keys::parse_key_name(mapped) {
                    // Reuse the emacs editing operations, then restore command mode.
                    pr.vi_command = false;
                    self.prompt_key(cid, &key, mapped);
                    if let Some(pr) = self.clients.get_mut(&cid).and_then(|c| c.prompt.as_mut()) {
                        pr.vi_command = true;
                    }
                }
                return;
            }
        }
        let ctrl = |ch: char| k.mods.ctrl && k.code == KeyCode::Char(ch);
        match &k.code {
            KeyCode::Enter => {
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
            KeyCode::Escape => {
                c.prompt = None;
            }
            _ if ctrl('c') || ctrl('g') => {
                c.prompt = None;
            }
            KeyCode::BSpace | KeyCode::Char('h') if pr.bspace_exit && pr.input.is_empty() && (k.code == KeyCode::BSpace || ctrl('h')) => {
                c.prompt = None;
            }
            KeyCode::BSpace => {
                if pr.cursor > 0 {
                    pr.cursor -= 1;
                    pr.input.remove(pr.cursor);
                }
            }
            _ if ctrl('h') => {
                if pr.cursor > 0 {
                    pr.cursor -= 1;
                    pr.input.remove(pr.cursor);
                }
            }
            KeyCode::Delete => {
                if pr.cursor < pr.input.len() {
                    pr.input.remove(pr.cursor);
                }
            }
            _ if ctrl('d') => {
                if pr.cursor < pr.input.len() {
                    pr.input.remove(pr.cursor);
                }
            }
            KeyCode::Left => pr.cursor = pr.cursor.saturating_sub(1),
            _ if ctrl('b') => pr.cursor = pr.cursor.saturating_sub(1),
            KeyCode::Right => pr.cursor = (pr.cursor + 1).min(pr.input.len()),
            _ if ctrl('f') => pr.cursor = (pr.cursor + 1).min(pr.input.len()),
            KeyCode::Home => pr.cursor = 0,
            _ if ctrl('a') => pr.cursor = 0,
            KeyCode::End => pr.cursor = pr.input.len(),
            _ if ctrl('e') => pr.cursor = pr.input.len(),
            _ if ctrl('k') => { pr.copied = pr.input[pr.cursor..].to_vec(); pr.input.truncate(pr.cursor); }
            _ if ctrl('y') => {
                let value: Vec<_> = if pr.copied.is_empty() { paste.chars().take_while(|c| !c.is_control()).collect() } else { pr.copied.clone() };
                let n = value.len(); pr.input.splice(pr.cursor..pr.cursor, value); pr.cursor += n;
            }
            _ if ctrl('t') => {
                if pr.cursor > 0 && pr.input.len() > 1 {
                    let at = pr.cursor.min(pr.input.len() - 1); pr.input.swap(at - 1, at); pr.cursor = (at + 1).min(pr.input.len());
                }
            }
            _ if k.mods.meta && matches!(k.code, KeyCode::Char('b') | KeyCode::BSpace) => {
                let old = pr.cursor;
                while pr.cursor > 0 && pr.input[pr.cursor - 1].is_whitespace() { pr.cursor -= 1; }
                while pr.cursor > 0 && !pr.input[pr.cursor - 1].is_whitespace() { pr.cursor -= 1; }
                if k.code == KeyCode::BSpace { pr.copied = pr.input.drain(pr.cursor..old).collect(); }
            }
            _ if k.mods.meta && matches!(k.code, KeyCode::Char('f' | 'd')) => {
                let old = pr.cursor;
                while pr.cursor < pr.input.len() && pr.input[pr.cursor].is_whitespace() { pr.cursor += 1; }
                while pr.cursor < pr.input.len() && !pr.input[pr.cursor].is_whitespace() { pr.cursor += 1; }
                if k.code == KeyCode::Char('d') { pr.copied = pr.input.drain(old..pr.cursor).collect(); pr.cursor = old; }
            }
            _ if ctrl('u') => {
                pr.copied = std::mem::take(&mut pr.input);
                pr.cursor = 0;
            }
            _ if ctrl('w') => {
                let mut i = pr.cursor;
                while i > 0 && pr.input[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && pr.input[i - 1] != ' ' {
                    i -= 1;
                }
                pr.copied = pr.input.drain(i..pr.cursor).collect();
                pr.cursor = i;
            }
            _ if k.code == KeyCode::Up || ctrl('p') => {
                if hist_len > 0 {
                    let pos = pr.history_pos.map(|p| p.min(hist_len).saturating_sub(1)).unwrap_or(hist_len - 1);
                    pr.history_pos = Some(pos);
                    pr.input = self.prompt_history[pos].chars().collect();
                    pr.cursor = pr.input.len();
                }
            }
            _ if k.code == KeyCode::Down || ctrl('n') => {
                if let Some(p) = pr.history_pos.map(|p| p.min(hist_len)) {
                    if p + 1 < hist_len {
                        pr.history_pos = Some(p + 1);
                        pr.input = self.prompt_history[p + 1].chars().collect();
                    } else {
                        pr.history_pos = None;
                        pr.input.clear();
                    }
                    pr.cursor = pr.input.len();
                }
            }
            KeyCode::Tab => {
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
            KeyCode::Space => {
                pr.input.insert(pr.cursor, ' ');
                pr.cursor += 1;
            }
            KeyCode::Char(ch) if !k.mods.ctrl && !k.mods.meta => {
                if pr.numeric && !ch.is_ascii_digit() {
                    return;
                }
                pr.input.insert(pr.cursor, *ch);
                pr.cursor += 1;
                if pr.single_key {
                    let mut pr = c.prompt.take().unwrap();
                    pr.answers.push(pr.input.iter().collect());
                    let cmd = fill_template(&pr.template, &pr.answers);
                    self.run_string(cid, &cmd, pr.pane);
                }
            }
            _ => {}
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
