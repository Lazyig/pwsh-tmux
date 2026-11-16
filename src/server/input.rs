//! Client input: key tables, prompts, overlays, pane modes and the mouse.

use super::modes::{MenuAction, TreeAction};
use super::*;
use crate::copymode::Action;
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
        ("'", "command-prompt -p index { select-window -t ':%%' }", false, "Prompt for window index to select"),
        ("(", "switch-client -p", false, "Switch to previous client"),
        (")", "switch-client -n", false, "Switch to next client"),
        (",", "command-prompt -I '#W' { rename-window -- '%%' }", false, "Rename current window"),
        ("-", "delete-buffer", false, "Delete the most recent paste buffer"),
        (".", "command-prompt { move-window -t '%%' }", false, "Move the current window"),
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

    pub(crate) fn pane_input_requests(&mut self, pane: PaneId, queries: Vec<(Vec<u8>, bool)>, sets: Vec<(Vec<u8>, Vec<u8>)>, passthrough: Vec<Vec<u8>>) {
        // Clients showing the pane (screen_write_set_client_cb).
        let showing = |srv: &Self| -> Vec<ClientId> {
            srv.clients.values().filter(|c| c.attached && !c.suspended && c.control == 0 && c.session.is_some_and(|s| {
                let w = &srv.windows[&srv.sessions[&s].active_window()];
                w.panes().contains(&pane) && (!w.zoomed || w.active == pane)
            })).map(|c| c.id).collect()
        };
        // input_osc_52_parse: only with set-clipboard on; the selector keeps its allowed characters.
        if self.opt_server("set-clipboard") == "on" {
            for (selector, data) in sets {
                if data.is_empty() { continue; }
                let Ok(decoded) = decode_osc52(&data) else { continue };
                let clip = clipboard_selector(&selector);
                for cid in showing(self) { self.set_selection(cid, &clip, &decoded); }
                let ctx = Ctx { pane: Some(pane), ..Ctx::default() };
                self.run_hook(self.session_of_pane(pane).map(|x| x.0), "pane-set-clipboard", ctx);
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
                // tty_keys_sync: the DECRPM reply to the attach-time DECRQM 2026
                if let Some(status) = raw.strip_prefix(b"\x1b[?2026;").and_then(|r| r.strip_suffix(b"$y")).filter(|r| r.len() == 1 && (b'0'..=b'4').contains(&r[0])) {
                    if let Some(c) = self.clients.get_mut(&cid) {
                        if matches!(status[0], b'1'..=b'3') && !c.features.has("sync") {
                            c.features.parse("sync", &[',']);
                            c.prev = None;
                            c.dirty = true;
                        }
                    }
                    return;
                }
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
        if self.panes.get(&pane).is_none_or(|p| p.options.get("@input-disabled") == Some("on")) { return; }
        let Some(bytes) = self.encode_pane_key(pane, k, raw) else { return };
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
        } else if let Some(p) = self.panes.get(&pane) {
            p.send(bytes);
        }
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
        if c.is_readonly() && (c.prompt.is_some() || c.overlay.is_some()) { return; }
        if c.prompt.is_some() {
            let prompt_key = match k.code { KeyCode::KP('\r') => Key::plain(KeyCode::Enter), KeyCode::KP(ch) => Key::plain(KeyCode::Char(ch)), _ => k.clone() };
            self.prompt_key(cid, &prompt_key, &name);
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
                let dead = self.panes.get(&pane).is_none_or(|x| x.dead.is_some());
                // popup_key_cb: Escape and C-c close it unless it closes when its command
                // exits (and that is still running); -k: any key once the command exited
                let esc = (k.code == KeyCode::Escape && k.mods.is_empty()) || (k.code == KeyCode::Char('c') && k.mods.ctrl && !k.mods.meta);
                if (esc && (!p.close_on_exit || dead)) || (dead && p.any_key) {
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

    /// server_client_handle_dead_key: an unbound key closes a pane kept by remain-on-exit key or
    /// failed-key. It runs before the read-only check, so read-only clients may dismiss it too.
    fn dead_key(&mut self, pane: PaneId) -> bool {
        if !self.panes.get(&pane).is_some_and(|p| p.dead.is_some()) { return false; }
        if !matches!(self.opt_pane(pane, "remain-on-exit").as_str(), "key" | "failed-key") { return false; }
        self.kill_pane(pane);
        true
    }

    /// Runs a command string for a client (key bindings, prompts, menus).
    pub fn run_string(&mut self, cid: ClientId, cmd: &str, pane: Option<PaneId>) {
        // cmd_parse_from_string: the whole list is checked before any of it runs
        match crate::cmdparse::parse(cmd).and_then(|c| c.iter().try_for_each(|c| self.check_parsed(c)).map(|_| c)) {
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
                    // the default copy-mode search bindings use command-prompt -T search
                    if let Some(pr) = self.clients.get_mut(&c).and_then(|c| c.prompt.as_mut()) {
                        pr.history_type = usize::from(command.starts_with("search-"));
                    }
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
        let separators: Vec<char> = self.opt_session(self.client_session(cid), "word-separators").chars().collect();
        let top_buffer = self.buffers.iter().find(|b| b.automatic).map(|b| b.data.clone());
        let history_type = self.clients.get(&cid).and_then(|c| c.prompt.as_ref()).map_or(0, |p| p.history_type);
        let history = self.prompt_history.get(history_type).cloned().unwrap_or_default();
        let hist_len = history.len();
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
        let translated;
        let k = if vi {
            // prompt_translate_key: vi insert mode passes keys through, Escape enters command
            // mode; command mode maps vi keys to the emacs ones and drops the rest.
            let ctrl_key = |ch: char| Key::new(KeyCode::Char(ch), keys::Mods::CTRL);
            if !pr.command_mode {
                if k.code == KeyCode::Escape && k.mods.is_empty() {
                    pr.command_mode = true;
                    pr.cursor = pr.cursor.saturating_sub(1);
                    return;
                }
                k
            } else {
                let ch = match &k.code { KeyCode::Char(ch) if k.mods.is_empty() => Some(*ch), _ => None };
                if matches!(ch, Some('A' | 'I' | 'C' | 's' | 'a' | 'S' | 'i')) { pr.command_mode = false; }
                let vi_motion = |pr: &mut Prompt, m: char| match m {
                    'w' => pr.cursor = prompt_forward_word(&pr.input, pr.cursor, true, &separators),
                    'W' => pr.cursor = prompt_forward_word(&pr.input, pr.cursor, true, &[]),
                    'e' => pr.cursor = prompt_end_word(&pr.input, pr.cursor, &separators),
                    'E' => pr.cursor = prompt_end_word(&pr.input, pr.cursor, &[]),
                    _ => pr.cursor = prompt_backward_word(&pr.input, pr.cursor, &[]),
                };
                translated = match ch {
                    Some('i') => return,
                    Some('S') | Some('d') => ctrl_key('u'),
                    Some('A' | '$') => Key::plain(KeyCode::End),
                    Some('I' | '0' | '^') => Key::plain(KeyCode::Home),
                    Some('C' | 'D') => ctrl_key('k'),
                    Some('X') => Key::plain(KeyCode::BSpace),
                    Some('b') => Key::new(KeyCode::Char('b'), keys::Mods::META),
                    Some(m @ ('B' | 'e' | 'E' | 'w' | 'W')) => { vi_motion(pr, m); return; }
                    Some('p') => ctrl_key('y'),
                    Some('q') => ctrl_key('c'),
                    Some('s' | 'x') => Key::plain(KeyCode::Delete),
                    Some('j') => Key::plain(KeyCode::Down),
                    Some('h') => Key::plain(KeyCode::Left),
                    Some('a' | 'l') => Key::plain(KeyCode::Right),
                    Some('k') => Key::plain(KeyCode::Up),
                    Some(_) => return,
                    None => match k.code {
                        KeyCode::BSpace if k.mods.is_empty() => Key::plain(KeyCode::Left),
                        KeyCode::Delete | KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Enter if k.mods.is_empty() => k.clone(),
                        KeyCode::Char('h' | 'c') if k.mods == keys::Mods::CTRL => k.clone(),
                        _ => return,
                    },
                };
                &translated
            }
        } else { k };
        let ctrl = |ch: char| k.mods.ctrl && k.code == KeyCode::Char(ch);
        match &k.code {
            KeyCode::Enter => {
                let mut pr = c.prompt.take().unwrap();
                let input: String = pr.input.iter().collect();
                // prompt_key: every non-empty answer goes in the history of the prompt's
                // type, incremental copy-mode searches too
                if !input.is_empty() { self.add_prompt_history(&input, pr.history_type); }
                if let Some((pane, _)) = incremental {
                    if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                        cm.finish_incremental_search(false);
                    }
                    return;
                }
                pr.answers.push(input.clone());
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
            KeyCode::Left if !k.mods.ctrl => pr.cursor = pr.cursor.saturating_sub(1),
            _ if ctrl('b') => pr.cursor = pr.cursor.saturating_sub(1),
            KeyCode::Right if !k.mods.ctrl => pr.cursor = (pr.cursor + 1).min(pr.input.len()),
            _ if ctrl('f') => pr.cursor = (pr.cursor + 1).min(pr.input.len()),
            KeyCode::Home => pr.cursor = 0,
            _ if ctrl('a') => pr.cursor = 0,
            KeyCode::End => pr.cursor = pr.input.len(),
            _ if ctrl('e') => pr.cursor = pr.input.len(),
            _ if ctrl('k') => pr.input.truncate(pr.cursor),
            // C-u clears the whole line, as in tmux
            _ if ctrl('u') => {
                pr.input.clear();
                pr.cursor = 0;
            }
            _ if ctrl('w') => {
                let i = prompt_backward_word(&pr.input, pr.cursor, &separators);
                pr.copied = Some(pr.input.drain(i..pr.cursor).collect());
                pr.cursor = i;
            }
            // prompt_paste: the C-w text, else the newest automatic buffer up to a control char
            _ if ctrl('y') => {
                let text: Vec<char> = match &pr.copied {
                    Some(copied) => copied.clone(),
                    None => String::from_utf8_lossy(&top_buffer.unwrap_or_default()).chars().take_while(|c| !c.is_control()).collect(),
                };
                let n = text.len();
                pr.input.splice(pr.cursor..pr.cursor, text);
                pr.cursor += n;
            }
            KeyCode::Right if k.mods == keys::Mods::CTRL => pr.cursor = prompt_forward_word(&pr.input, pr.cursor, false, &separators),
            KeyCode::Char('f') if k.mods == keys::Mods::META => pr.cursor = prompt_forward_word(&pr.input, pr.cursor, false, &separators),
            KeyCode::Left if k.mods == keys::Mods::CTRL => pr.cursor = prompt_backward_word(&pr.input, pr.cursor, &separators),
            KeyCode::Char('b') if k.mods == keys::Mods::META => pr.cursor = prompt_backward_word(&pr.input, pr.cursor, &separators),
            _ if k.code == KeyCode::Up || ctrl('p') => {
                if hist_len > 0 {
                    let pos = pr.history_pos.map(|p| p.min(hist_len).saturating_sub(1)).unwrap_or(hist_len - 1);
                    pr.history_pos = Some(pos);
                    pr.input = history[pos].chars().collect();
                    pr.cursor = pr.input.len();
                }
            }
            _ if k.code == KeyCode::Down || ctrl('n') => {
                if let Some(p) = pr.history_pos.map(|p| p.min(hist_len)) {
                    if p + 1 < hist_len {
                        pr.history_pos = Some(p + 1);
                        pr.input = history[p + 1].chars().collect();
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

fn prompt_in(separators: &[char], c: char) -> bool { separators.contains(&c) }

/// prompt_forward_word: emacs skips leading spaces first; vi goes on to the next word start.
fn prompt_forward_word(input: &[char], cursor: usize, vi: bool, separators: &[char]) -> usize {
    let size = input.len();
    let mut idx = cursor.min(size);
    if !vi { while idx != size && input[idx] == ' ' { idx += 1; } }
    if idx == size { return idx; }
    let word_is_separators = prompt_in(separators, input[idx]) && input[idx] != ' ';
    loop {
        idx += 1;
        if idx < size && input[idx] == ' ' {
            if vi { while idx != size && input[idx] == ' ' { idx += 1; } }
            break;
        }
        if idx == size || word_is_separators != prompt_in(separators, input[idx]) { break; }
    }
    idx
}

/// prompt_end_word: to the last character of the next word.
fn prompt_end_word(input: &[char], cursor: usize, separators: &[char]) -> usize {
    let size = input.len();
    let mut idx = cursor.min(size);
    if idx == size { return idx; }
    loop {
        idx += 1;
        if idx == size { return idx; }
        if input[idx] != ' ' { break; }
    }
    let word_is_separators = prompt_in(separators, input[idx]);
    loop {
        idx += 1;
        if idx == size || input[idx] == ' ' || word_is_separators != prompt_in(separators, input[idx]) { break; }
    }
    idx - 1
}

/// prompt_backward_word: to the start of the previous word.
fn prompt_backward_word(input: &[char], cursor: usize, separators: &[char]) -> usize {
    let mut idx = cursor.min(input.len());
    while idx != 0 {
        idx -= 1;
        if input[idx] != ' ' { break; }
    }
    let word_is_separators = input.get(idx).is_some_and(|c| prompt_in(separators, *c));
    while idx != 0 {
        idx -= 1;
        if input[idx] == ' ' || word_is_separators != prompt_in(separators, input[idx]) { idx += 1; break; }
    }
    idx
}
