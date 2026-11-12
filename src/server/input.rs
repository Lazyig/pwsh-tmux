//! Client input: key tables, prompts, overlays, pane modes and the mouse.

use super::modes::{MenuAction, ModeAction};
use super::*;
use crate::copymode::Action;
use crate::keys::{Input, Key, KeyCode};

pub use super::mouse::MouseCtx;

// The default mouse menus of tmux 3.6a (key-bindings.c).
const PANE_MENU: &str = r##"display-menu -T "#[align=centre]#{pane_index} (#{pane_id})" -t = -x M -y M "#{?#{m/r:(copy|view)-mode,#{pane_mode}},Go To Top,}" < { send-keys -X history-top } "#{?#{m/r:(copy|view)-mode,#{pane_mode}},Go To Bottom,}" > { send-keys -X history-bottom } '' "#{?mouse_word,Search For #[underscore]#{=/9/...:mouse_word},}" C-r { if-shell -F "#{?#{m/r:(copy|view)-mode,#{pane_mode}},0,1}" "copy-mode -t=" ; send-keys -X -t = search-backward -- "#{q:mouse_word}" } "#{?mouse_word,Type #[underscore]#{=/9/...:mouse_word},}" C-y { copy-mode -q ; send-keys -l "#{q:mouse_word}" } "#{?mouse_word,Copy #[underscore]#{=/9/...:mouse_word},}" c { copy-mode -q ; set-buffer "#{q:mouse_word}" } "#{?mouse_line,Copy Line,}" l { copy-mode -q ; set-buffer "#{q:mouse_line}" } '' "#{?mouse_hyperlink,Type #[underscore]#{=/9/...:mouse_hyperlink},}" C-h { copy-mode -q ; send-keys -l "#{q:mouse_hyperlink}" } "#{?mouse_hyperlink,Copy #[underscore]#{=/9/...:mouse_hyperlink},}" h { copy-mode -q ; set-buffer "#{q:mouse_hyperlink}" } '' "Horizontal Split" h { split-window -h } "Vertical Split" v { split-window -v } '' "#{?#{>:#{window_panes},1},,-}Swap Up" u { swap-pane -U } "#{?#{>:#{window_panes},1},,-}Swap Down" d { swap-pane -D } "#{?pane_marked_set,,-}Swap Marked" s { swap-pane } '' Kill X { kill-pane } Respawn R { respawn-pane -k } "#{?pane_marked,Unmark,Mark}" m { select-pane -m } "#{?#{>:#{window_panes},1},,-}#{?window_zoomed_flag,Unzoom,Zoom}" z { resize-pane -Z }"##;

const WINDOW_MENU: &str = r##"display-menu -T "#[align=centre]#{window_index}:#{window_name}" -t = -x W -y W "#{?#{>:#{session_windows},1},,-}Swap Left" l { swap-window -t :-1 } "#{?#{>:#{session_windows},1},,-}Swap Right" r { swap-window -t :+1 } "#{?pane_marked_set,,-}Swap Marked" s { swap-window } '' Kill X { kill-window } Respawn R { respawn-window -k } "#{?pane_marked,Unmark,Mark}" m { select-pane -m } Rename n { command-prompt -F -I "#W" { rename-window -t "#{window_id}" "%%" } } '' "New After" w { new-window -a } "New At End" W { new-window }"##;

const SESSION_MENU: &str = r##"display-menu -T "#[align=centre]#{session_name}" -t = -x M -y W Next n { switch-client -n } Previous p { switch-client -p } '' Renumber N { move-window -r } Rename n { command-prompt -I "#S" { rename-session "%%" } } '' "New Session" s { new-session } "New Window" w { new-window }"##;

const IN_MODE: &str = "#{||:#{pane_in_mode},#{mouse_any_flag}}";

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
    let mut add = |table: &str, key: &str, cmd: &str, repeat: bool, note: &str| {
        let kname = keys::parse_key_name(key).map(|k| keys::key_name(&k)).unwrap_or_else(|| key.to_string());
        t.entry(table.to_string()).or_default().insert(kname, Binding { cmd: cmd.to_string(), repeat, note: note.to_string() });
    };
    let prefix: &[(&str, &str, bool, &str)] = &[
        ("C-b", "send-prefix", false, "Send the prefix key"),
        ("C-o", "rotate-window", false, "Rotate through the panes"),
        ("C-z", "suspend-client", false, "Suspend the current client"),
        ("Space", "next-layout", false, "Select next layout"),
        ("!", "break-pane", false, "Break pane to a new window"),
        ("\"", "split-window", false, "Split window vertically"),
        ("#", "list-buffers", false, "List all paste buffers"),
        ("$", "command-prompt -I '#S' { rename-session -- '%%' }", false, "Rename current session"),
        ("%", "split-window -h", false, "Split window horizontally"),
        ("&", "confirm-before -p \"kill-window #W? (y/n)\" kill-window", false, "Kill current window"),
        ("'", "command-prompt -T window-target -p index { select-window -t ':%%' }", false, "Prompt for window index to select"),
        ("(", "switch-client -p", false, "Switch to previous client"),
        (")", "switch-client -n", false, "Switch to next client"),
        (",", "command-prompt -I '#W' { rename-window -- '%%' }", false, "Rename current window"),
        ("-", "delete-buffer", false, "Delete the most recent paste buffer"),
        (".", "command-prompt -T target { move-window -t '%%' }", false, "Move the current window"),
        ("/", "command-prompt -k -p key { list-keys -1N '%%' }", false, "Describe key binding"),
        (":", "command-prompt", false, "Prompt for a command"),
        (";", "last-pane", false, "Move to the previously active pane"),
        ("<", &WINDOW_MENU.replacen(" -t = -x W -y W", " -x W -y W", 1), false, "Display window menu"),
        ("=", "choose-buffer -Z", false, "Choose a paste buffer from a list"),
        (">", &PANE_MENU.replacen(" -t = -x M -y M", " -x P -y P", 1), false, "Display pane menu"),
        ("?", "list-keys -N", false, "List key bindings"),
        ("C", "customize-mode -Z", false, "Customize options"),
        ("D", "choose-client -Z", false, "Choose and detach a client from a list"),
        ("E", "select-layout -E", false, "Spread panes out evenly"),
        ("L", "switch-client -l", false, "Switch to the last client"),
        ("M", "select-pane -M", false, "Clear the marked pane"),
        ("[", "copy-mode", false, "Enter copy mode"),
        ("]", "paste-buffer -p", false, "Paste the most recent paste buffer"),
        ("c", "new-window", false, "Create a new window"),
        ("d", "detach-client", false, "Detach the current client"),
        ("f", "command-prompt { find-window -Z -- '%%' }", false, "Search for a pane"),
        ("i", "display-message", false, "Display window information"),
        ("l", "last-window", false, "Select the previously current window"),
        ("m", "select-pane -m", false, "Toggle the marked pane"),
        ("n", "next-window", false, "Select the next window"),
        ("o", "select-pane -t :.+", false, "Select the next pane"),
        ("p", "previous-window", false, "Select the previous window"),
        ("q", "display-panes", false, "Display pane numbers"),
        ("r", "refresh-client", false, "Redraw the current client"),
        ("s", "choose-tree -Zs", false, "Choose a session from a list"),
        ("t", "clock-mode", false, "Show a clock"),
        ("w", "choose-tree -Zw", false, "Choose a window from a list"),
        ("x", "confirm-before -p \"kill-pane #P? (y/n)\" kill-pane", false, "Kill the active pane"),
        ("z", "resize-pane -Z", false, "Zoom the active pane"),
        ("{", "swap-pane -U", false, "Swap the active pane with the pane above"),
        ("}", "swap-pane -D", false, "Swap the active pane with the pane below"),
        ("~", "show-messages", false, "Show messages"),
        ("PPage", "copy-mode -u", false, "Enter copy mode and scroll up"),
        ("Up", "select-pane -U", true, "Select the pane above the active pane"),
        ("Down", "select-pane -D", true, "Select the pane below the active pane"),
        ("Left", "select-pane -L", true, "Select the pane to the left of the active pane"),
        ("Right", "select-pane -R", true, "Select the pane to the right of the active pane"),
        ("M-1", "select-layout even-horizontal", false, "Set the even-horizontal layout"),
        ("M-2", "select-layout even-vertical", false, "Set the even-vertical layout"),
        ("M-3", "select-layout main-horizontal", false, "Set the main-horizontal layout"),
        ("M-4", "select-layout main-vertical", false, "Set the main-vertical layout"),
        ("M-5", "select-layout tiled", false, "Select the tiled layout"),
        ("M-n", "next-window -a", false, "Select the next window with an alert"),
        ("M-o", "rotate-window -D", false, "Rotate through the panes in reverse"),
        ("M-p", "previous-window -a", false, "Select the previous window with an alert"),
        ("M-Up", "resize-pane -U 5", true, "Resize the pane up by 5"),
        ("M-Down", "resize-pane -D 5", true, "Resize the pane down by 5"),
        ("M-Left", "resize-pane -L 5", true, "Resize the pane left by 5"),
        ("M-Right", "resize-pane -R 5", true, "Resize the pane right by 5"),
        ("C-Up", "resize-pane -U", true, "Resize the pane up"),
        ("C-Down", "resize-pane -D", true, "Resize the pane down"),
        ("C-Left", "resize-pane -L", true, "Resize the pane left"),
        ("C-Right", "resize-pane -R", true, "Resize the pane right"),
        ("S-Up", "refresh-client -U 10", true, ""),
        ("S-Down", "refresh-client -D 10", true, ""),
    ];
    for (k, c, r, n) in prefix {
        add("prefix", k, c, *r, n);
    }
    for i in 0..10 {
        add("prefix", &i.to_string(), &format!("select-window -t :={i}"), false, &format!("Select window {i}"));
    }
    let root: Vec<(&str, String)> = vec![
        ("MouseDown1Pane", "select-pane -t = ; send-keys -M".into()),
        ("MouseDown1Status", "switch-client -t =".into()),
        ("MouseDown1ScrollbarUp", "copy-mode -u".into()),
        ("MouseDown1ScrollbarDown", "copy-mode -d".into()),
        ("MouseDrag1ScrollbarSlider", "copy-mode -S".into()),
        ("MouseDown2Pane", format!("select-pane -t = ; if-shell -F '{IN_MODE}' {{ send-keys -M }} {{ paste-buffer -p }}")),
        ("MouseDown3Pane", format!("if-shell -F -t = '#{{||:#{{mouse_any_flag}},#{{&&:#{{pane_in_mode}},#{{?#{{m/r:(copy|view)-mode,#{{pane_mode}}}},0,1}}}}}}' {{ select-pane -t = ; send-keys -M }} {{ {PANE_MENU} }}")),
        ("M-MouseDown3Pane", PANE_MENU.to_string()),
        ("MouseDown3Status", WINDOW_MENU.to_string()),
        ("M-MouseDown3Status", WINDOW_MENU.to_string()),
        ("MouseDown3StatusLeft", SESSION_MENU.to_string()),
        ("M-MouseDown3StatusLeft", SESSION_MENU.to_string()),
        ("MouseDrag1Pane", format!("if-shell -F '{IN_MODE}' {{ send-keys -M }} {{ copy-mode -M }}")),
        ("MouseDrag1Border", "resize-pane -M".into()),
        ("WheelUpPane", "if-shell -F '#{||:#{alternate_on},#{pane_in_mode},#{mouse_any_flag}}' { send-keys -M } { copy-mode -e }".into()),
        ("WheelUpStatus", "previous-window".into()),
        ("WheelDownStatus", "next-window".into()),
        ("DoubleClick1Pane", format!("select-pane -t = ; if-shell -F '{IN_MODE}' {{ send-keys -M }} {{ copy-mode -H ; send-keys -X select-word ; run-shell -d 0.3 ; send-keys -X copy-pipe-and-cancel }}")),
        ("TripleClick1Pane", format!("select-pane -t = ; if-shell -F '{IN_MODE}' {{ send-keys -M }} {{ copy-mode -H ; send-keys -X select-line ; run-shell -d 0.3 ; send-keys -X copy-pipe-and-cancel }}")),
    ];
    for (k, c) in &root {
        add("root", k, c, false, "");
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
            Some(PaneMode::Tree(_) | PaneMode::Switch(_) | PaneMode::Panes(_)) => {
                let (w, h) = (p.size.0 as usize, p.size.1 as usize);
                let Some(mut mode) = p.mode.take() else { return };
                let action = match &mut mode {
                    PaneMode::Tree(t) => t.key(name, self, w, h),
                    PaneMode::Switch(s) => s.key(name, self, w, h),
                    PaneMode::Panes(m) => m.key(name, self),
                    _ => ModeAction::None,
                };
                if let Some(p) = self.panes.get_mut(&pane) {
                    p.mode = Some(mode);
                }
                self.mode_action(pane, cid, action);
            }
            Some(PaneMode::Copy(_)) => {
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
                            Some(PaneMode::Copy(cm)) => cm.key(name),
                            _ => return,
                        };
                        self.copy_action(pane, cid, action, None);
                    }
                }
            }
            None => {}
        }
    }

    /// The client a mode's commands run for: the one that opened it while it is attached.
    fn mode_client(&self, pane: PaneId, cid: Option<ClientId>) -> Option<ClientId> {
        let opened = match self.panes.get(&pane).and_then(|p| p.mode.as_ref()) {
            Some(PaneMode::Tree(t)) => t.client,
            Some(PaneMode::Switch(s)) => s.client,
            Some(PaneMode::Panes(m)) => m.client,
            _ => None,
        };
        opened.filter(|c| self.clients.get(c).is_some_and(|c| c.attached)).or(cid)
    }

    fn run_mode_command(&mut self, client: Option<ClientId>, cmd: &str, pane: PaneId) {
        let pane = self.panes.contains_key(&pane).then_some(pane);
        match client {
            Some(c) => self.run_string(c, cmd, pane),
            None => match crate::cmdparse::parse(cmd) {
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
                        if self.windows[&wid].zoomed {
                            self.windows.get_mut(&wid).unwrap().zoomed = false;
                            self.apply_layout(wid);
                        }
                    }
                }
                // like tmux, the mode is gone before the queued command runs
                self.exit_mode(pane);
                self.run_mode_command(client, &cmd, pane);
            }
            ModeAction::RunKeep(cmd) => self.run_mode_command(client, &cmd, pane),
            ModeAction::Edit { value, command } => {
                if let Some(c) = client {
                    if let Err(e) = self.open_editor(c, &value, command) {
                        self.set_message(c, &e);
                    }
                }
            }
        }
        if let Some(wid) = self.window_of(pane) {
            self.mark_window_dirty(wid);
        }
    }

    /// spawn_editor: edit `value` in the editor option's program, in a popup over the client;
    /// `command` runs with %% replaced by the text when the editor exits successfully.
    pub fn open_editor(&mut self, cid: ClientId, value: &str, command: String) -> Result<(), String> {
        if self.clients.get(&cid).is_some_and(|c| c.overlay.is_some()) {
            return Err("an overlay is already open".into());
        }
        let path = std::env::temp_dir().join(format!("tmux.{}.{}", std::process::id(), now_ms()));
        let text = if value.is_empty() { "\n".to_string() } else { value.to_string() };
        std::fs::write(&path, text).map_err(|e| format!("editor: {e}"))?;
        let editor = self.opt_server("editor");
        // the editor option names the program (notepad.exe on Windows); the file is one argument
        let quoted = if cfg!(windows) { format!("\"{}\"", path.display()) } else { crate::cmdparse::quote(&path.display().to_string()) };
        let cmd = format!("{editor} {quoted}");
        let (cw, ch) = self.clients[&cid].size;
        let (w, h) = ((cw as u32 * 9 / 10) as u16, (ch.saturating_sub(self.status_lines(self.client_session(cid))) as u32 * 9 / 10) as u16);
        let (w, h) = (w.max(3), h.max(3));
        let (x, y) = (cw / 2 - w / 2, ch / 2 - h / 2);
        let sid = self.client_session(cid);
        let pane = match self.spawn_pane(sid, w - 2, h - 2, &[cmd], Some(&std::env::temp_dir().display().to_string()), &[]) {
            Ok(p) => p,
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                return Err(e);
            }
        };
        let editor = Some(Box::new(super::modes::PopupEditor { path, command }));
        self.replace_overlay(cid, Some(Overlay::Popup(super::modes::Popup { pane, x, y, w, h, border: true, close_on_exit: true, title: String::new(), editor })))?;
        Ok(())
    }

    /// The editor popup closed: run its command with the new text (spawn_editor_finish).
    pub fn editor_done(&mut self, cid: ClientId, ed: super::modes::PopupEditor, status: Option<i32>) {
        let text = std::fs::read_to_string(&ed.path).ok();
        let _ = std::fs::remove_file(&ed.path);
        let Some(mut text) = text.filter(|_| status.unwrap_or(0) == 0) else { return };
        if text.ends_with('\n') {
            text.pop();
            if text.ends_with('\r') {
                text.pop();
            }
        }
        let cmd = fill_template(&ed.command, &[text]);
        self.run_string(cid, &cmd, None);
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
        let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) else {
            if self.panes.get(&pane).and_then(|p| p.mode.as_ref()).is_some_and(|m| !matches!(m, PaneMode::Clock)) {
                // the other modes understand a few -X commands
                let key = match name {
                    "cursor-up" => "Up",
                    "cursor-down" => "Down",
                    "cancel" => {
                        self.exit_mode(pane);
                        return;
                    }
                    _ => "",
                };
                if !key.is_empty() {
                    self.mode_key(pane, cid, key);
                }
            }
            return;
        };
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
            _ if ctrl('k') => pr.input.truncate(pr.cursor),
            _ if ctrl('u') => {
                pr.input.drain(..pr.cursor);
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
                pr.input.drain(i..pr.cursor);
                pr.cursor = i;
            }
            KeyCode::Up => {
                if hist_len > 0 {
                    let pos = pr.history_pos.map(|p| p.min(hist_len).saturating_sub(1)).unwrap_or(hist_len - 1);
                    pr.history_pos = Some(pos);
                    pr.input = self.prompt_history[pos].chars().collect();
                    pr.cursor = pr.input.len();
                }
            }
            KeyCode::Down => {
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
            if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
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
    "swap-pane", "swap-window", "switch-client", "switch-mode", "unbind-key", "unlink-window", "wait-for",
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
