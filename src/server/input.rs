//! Client input: key tables, prompts, overlays, pane modes and the mouse.

use super::modes::{MenuAction, TreeAction};
use super::*;
use crate::copymode::Action;
use crate::{cmdparse, format, opts};
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
        if self.clients[&cid].prompt.is_some() && self.prompt_key(cid, &k, &name) {
            return;
        }
        let Some(c) = self.clients.get(&cid) else { return };
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
        if let Some(pane) = self.pane_prompt_target(cid) {
            if self.pane_prompt_key(cid, pane, &k, &name) {
                return;
            }
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
            Err(e) => self.set_error(cid, &e),
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
        // these always show an auto-hide scrollbar; others (history-top and
        // history-bottom too) when the view scrolled
        let always = matches!(name, "page-up" | "page-down" | "page-down-and-cancel" | "halfpage-up" | "halfpage-down"
            | "halfpage-down-and-cancel" | "scroll-to-mouse");
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
                    self.scrollbar_to_mouse(cid, pane, &m);
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
                if cid.is_some() {
                    // the pane is the prompt's context target (a %id in the template would read as %N)
                    // tmux 3.8 copy-mode bindings prompt in the pane (command-prompt -P)
                    let template = format!("send-keys -X {command} '%%'");
                    let mut pr = Prompt::new(PromptKind::Command, vec![(format!("{label} "), String::new())], template, Some(pane));
                    pr.search = command.starts_with("search");
                    pr.host = Some(pane);
                    if command.ends_with("-incremental") {
                        if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                            cm.begin_incremental_search();
                        }
                        pr.copy_incremental = Some((pane, command.starts_with("search-forward")));
                    }
                    self.set_pane_prompt(pane, pr);
                }
            }
        }
        if let Some(wid) = self.window_of(pane) {
            self.mark_window_dirty(wid);
        }
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
            }
            if let Some(wid) = self.window_of(host) { self.mark_window_dirty(wid); }
        } else if !pr.closed {
            if let Some(c) = self.clients.get_mut(&cid) {
                if c.prompt.is_none() { c.prompt = Some(pr); }
                c.dirty = true;
            }
        }
        if let Some((pane, forward)) = incremental {
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
                if !input.is_empty() && !pr.template.starts_with("send-keys -X ") && !pr.search {
                    self.prompt_history.retain(|h| h != &input);
                    self.prompt_history.push(input.clone());
                    let lim = self.opt_server_num("prompt-history-limit").max(1) as usize;
                    while self.prompt_history.len() > lim {
                        self.prompt_history.remove(0);
                    }
                }
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
                let hist_len = self.prompt_history.len();
                if hist_len > 0 && pr.history_pos != Some(0) {
                    let pos = pr.history_pos.map(|p| p.min(hist_len).saturating_sub(1)).unwrap_or(hist_len - 1);
                    pr.history_pos = Some(pos);
                    pr.input = self.prompt_history[pos].chars().collect();
                    pr.cursor = pr.input.len();
                    true
                } else {
                    false
                }
            }
            PromptOp::Down => {
                // past the newest entry the line is empty (prompt_down_history)
                let hist_len = self.prompt_history.len();
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
    /// active pane's, else that of a visible pane of the window.
    pub(crate) fn pane_prompt_target(&self, cid: ClientId) -> Option<PaneId> {
        let sid = self.client_session(cid)?;
        let wid = self.sessions[&sid].active_window();
        let win = self.windows.get(&wid)?;
        let has = |p: &PaneId| self.panes.get(p).is_some_and(|p| p.prompt.is_some());
        if has(&win.active) {
            return Some(win.active);
        }
        self.rects(wid).0.into_iter().map(|(p, _)| p).find(has)
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
        if word.len() >= 64 || pr.search || first != 0 || word.is_empty() {
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
