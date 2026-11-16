//! Copy mode in the server: running its keys and commands for a pane, refreshing it from
//! the pane's content, its format variables and the styles it is drawn with.

use super::cmds::base64;
use super::mouse::MouseCtx;
use super::*;
use crate::cells::Style;
use crate::copymode::{self, Action, LineNumbers, RenderStyle};
use crate::format;

/// window_copy_refresh_timer: how often automatic refresh looks for new output.
const REFRESH_INTERVAL: Duration = Duration::from_millis(50);

/// Copy-mode format variables (window_copy_formats) besides the cursor word and line.
pub(super) const COPY_VARS: &[&str] = &[
    "scroll_position", "copy_position", "copy_position_limit", "copy_line_numbers", "refresh_active",
    "rectangle_toggle", "copy_cursor_x", "copy_cursor_y", "selection_start_x", "selection_start_y",
    "selection_end_x", "selection_end_y", "selection_active", "selection_present", "selection_mode",
    "search_present", "search_timed_out", "search_count", "search_count_partial", "search_match",
    "top_line_time", "copy_cursor_word", "copy_cursor_line", "copy_cursor_hyperlink",
];

/// The ones window_copy_formats adds as callbacks (format_add_cb).
pub(super) const COPY_CALLBACK_VARS: &[&str] = &["search_match", "copy_cursor_word", "copy_cursor_line", "copy_cursor_hyperlink"];

pub(super) fn is_copy_var(name: &str) -> bool {
    COPY_VARS.contains(&name)
}

/// A default copy-mode binding (as tmux's list-keys prints it) in the form bind-key
/// stores a command (cmds::binding_command), so a rebinding to the same commands compares
/// equal and the stored commands run like any other binding.
pub(super) fn default_binding_command(cmd: &str) -> String {
    // parsed as bind-key's arguments, so { } blocks stay blocks
    match crate::cmdparse::parse(&format!("bind-key K {cmd}")) {
        Ok(cmds) if cmds.len() == 1 => super::cmds::binding_command(&cmds[0][2..]),
        _ => cmd.to_string(),
    }
}

/// The commands with CMD_READONLY: all a read-only client's key binding may run.
const READONLY_BINDING_CMDS: &[&str] = &["attach-session", "copy-mode", "detach-client", "list-clients", "send-keys", "switch-client"];

impl Server {
    /// key_bindings_dispatch: a read-only client's binding runs only if all its commands
    /// (cmd_list_all_have, with command aliases expanded as the parser does) may.
    pub(super) fn binding_allowed_readonly(&self, cmd: &str) -> bool {
        let Ok(cmds) = crate::cmdparse::parse(cmd) else { return false };
        let aliases = self.command_aliases();
        let readonly = |c: &[String]| c.first().and_then(|n| super::cmds::canonical(n)).is_some_and(|n| READONLY_BINDING_CMDS.contains(&n));
        cmds.iter().all(|c| {
            let alias = c.first().and_then(|name| aliases.iter().find_map(|a| a.split_once('=').filter(|(n, _)| n == name).map(|(_, v)| v.to_string())));
            match alias {
                Some(v) => crate::cmdparse::parse(&v).is_ok_and(|cs| cs.iter().all(|c| readonly(c))),
                None => readonly(c),
            }
        })
    }

    /// The copy-mode-line-numbers option for a pane's window.
    fn copy_line_numbers_option(&self, pane: PaneId) -> LineNumbers {
        LineNumbers::parse(&self.opt_window(self.window_of(pane), "copy-mode-line-numbers"))
    }

    /// Options copy mode reads as it runs (tmux looks them up each time).
    pub(super) fn copy_sync_options(&mut self, pane: PaneId) {
        let ln = self.copy_line_numbers_option(pane);
        let wrap = self.opt_window(self.window_of(pane), "wrap-search") == "on";
        let vi = self.opt_window(self.window_of(pane), "mode-keys") == "vi";
        let sid = self.session_of_pane(pane).map(|s| s.0);
        let separators = self.opt_session(sid, "word-separators");
        if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
            cm.set_line_number_option(ln);
            cm.set_wrap_search(wrap);
            cm.set_vi(vi);
            cm.set_word_separators(&separators);
        }
    }

    /// Every pane in copy mode picks up changed options.
    pub(super) fn copy_sync_all(&mut self) {
        let panes: Vec<PaneId> = self.panes.iter().filter(|(_, p)| matches!(p.mode, Some(PaneMode::Copy(_)))).map(|(id, _)| *id).collect();
        for p in panes {
            self.copy_sync_options(p);
        }
    }

    /// A key for a pane in copy mode: its binding in copy-mode or copy-mode-vi. The default
    /// bindings are run by the copy mode engine (which keeps the repeat count and asks for
    /// arguments itself); others are run as commands.
    pub(super) fn copy_key(&mut self, pane: PaneId, cid: Option<ClientId>, name: &str) {
        let canonical = keys::parse_key_name(name).map(|k| keys::key_name(&k));
        let name = canonical.as_deref().unwrap_or(name);
        self.copy_sync_options(pane);
        let vi = self.opt_window(self.window_of(pane), "mode-keys") == "vi";
        let table = if vi { "copy-mode-vi" } else { "copy-mode" };
        // the numeric prompt takes digits; any other key closes it and is looked up in the
        // table with the count as the prefix (PROMPT_NUMERIC returns PROMPT_KEY_NOT_HANDLED)
        let digit = name.len() == 1 && name.as_bytes()[0].is_ascii_digit();
        if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
            if cm.counting() && !digit {
                cm.end_count();
            }
        }
        let pending = matches!(self.panes.get(&pane).and_then(|p| p.mode.as_ref()), Some(PaneMode::Copy(cm)) if cm.waiting_for_key());
        let binding = self.bindings.get(table).and_then(|t| t.get(name)).map(|b| b.cmd.clone());
        let default_raw = copymode::CopyMode::default_bindings(vi).iter().find(|(k, _)| keys::parse_key_name(k).map(|k| keys::key_name(&k)).as_deref().unwrap_or(k) == name).map(|(_, c)| *c);
        let default = default_raw.map(default_binding_command);
        // the repeat count bindings open the real numeric pane prompt ("(repeat) N"): it
        // takes the digits and gives the count to the mode (send-keys -N) when another key
        // closes it, which is then looked up again
        let numeric = default_raw.is_some_and(|c| c.starts_with("command-prompt -NP"));
        // a read-only client cannot prompt or run other commands from copy mode
        if let (Some(cmd), Some(c), false) = (&binding, cid, pending) {
            if self.client_is_readonly(cid) && !self.binding_allowed_readonly(cmd) {
                // key_bindings_dispatch: cmdq_error, which capitalizes a client's message
                self.set_message(c, "Client is read-only");
                return;
            }
        }
        match binding {
            Some(cmd) if !pending && (Some(&cmd) != default.as_ref() || (numeric && cid.is_some())) => {
                if let Some(c) = cid {
                    self.run_key_string(c, &cmd, Some(pane), name);
                }
            }
            // an unbound key does nothing (unless a jump waits for its character)
            None if !pending => {}
            // the default prompts (and repeat counts) need a client, as command-prompt does
            Some(cmd) if !pending && cid.is_none() && cmd.starts_with("command-prompt") => {}
            _ => {
                let before = self.copy_scroll_offset(pane);
                let action = match self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                    Some(PaneMode::Copy(cm)) => cm.key(name),
                    _ => return,
                };
                self.copy_action(pane, cid, action, None, (false, false), None);
                self.show_scrollbar_if_scrolled(pane, before, false);
            }
        }
    }

    /// `send-keys -X name args`: a copy-mode command. The paging commands and the
    /// scrollbar drag always show an auto-hide scrollbar; others (history-top and
    /// history-bottom too) when the view scrolled.
    pub fn copy_mode_command(&mut self, pane: PaneId, cid: Option<ClientId>, name: &str, args: &[String]) {
        let always = matches!(name, "page-up" | "page-down" | "page-down-and-cancel" | "halfpage-up" | "halfpage-down"
            | "halfpage-down-and-cancel" | "scroll-to-mouse");
        let before = self.copy_scroll_offset(pane);
        self.copy_mode_command_inner(pane, cid, name, args);
        self.show_scrollbar_if_scrolled(pane, before, always);
    }

    /// Read-only clients may only move around (WINDOW_COPY_CMD_FLAG_READONLY).
    fn copy_mode_command_inner(&mut self, pane: PaneId, cid: Option<ClientId>, name: &str, args: &[String]) {
        if !matches!(self.panes.get(&pane).and_then(|p| p.mode.as_ref()), Some(PaneMode::Copy(_))) {
            return;
        }
        if self.client_is_readonly(cid) && copymode::is_command(name) && !copymode::command_allowed_readonly(name) {
            if let Some(c) = cid {
                self.set_message(c, "client is read-only");
            }
            return;
        }
        self.copy_sync_options(pane);
        if name == "scroll-to-mouse" {
            // window_copy_cmd_scroll_to_mouse: the slider follows the mouse (-e: leave at
            // the bottom); later drag events keep moving it
            if let (Some(c), Some(m)) = (cid, self.cur_mouse.clone()) {
                let exit = args.iter().any(|a| a == "-e");
                self.copy_scroll_to_mouse(c, pane, &m, exit);
            }
            return;
        }
        // args_parse with the command's flags: the copy commands take -C (no clipboard) and
        // -P (no paste buffer), next-prompt and previous-prompt -o; an unknown flag means the
        // command is not run (window_copy_command ignores the parse error), "--" ends flags
        let mut args = args.to_vec();
        let mut flags = (false, false);
        let copying = name.starts_with("copy-") || name.starts_with("append-");
        let allowed = if name.starts_with("copy-") { "CP" } else if matches!(name, "next-prompt" | "previous-prompt") { "o" } else { "" };
        if copymode::is_command(name) {
            while let Some(a) = args.first().filter(|a| a.len() > 1 && a.starts_with('-')).cloned() {
                args.remove(0);
                if a == "--" {
                    break;
                }
                if !a[1..].chars().all(|c| allowed.contains(c)) {
                    return;
                }
                flags.0 |= a.contains('C');
                flags.1 |= a.contains('P');
            }
        }
        // copy-pipe without a command uses copy-command
        let piped = name.starts_with("copy-pipe") || name.starts_with("pipe");
        let pipe = if !piped {
            None
        } else if !args.is_empty() && !args[0].is_empty() {
            Some(self.expand(self.target_for_pane(pane), cid, &args[0]))
        } else {
            Some(self.opt_server("copy-command")).filter(|c| !c.is_empty())
        };
        let run_args: Vec<String> = if piped && !args.is_empty() { args[1..].to_vec() } else { args.clone() };
        // copy commands: the optional prefix names the new buffer
        // the command and the prefix are formats (format_single)
        let prefix = if copying { run_args.first().filter(|p| !p.is_empty()).map(|p| self.expand(self.target_for_pane(pane), cid, p)) } else { None };
        let action = match self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
            Some(PaneMode::Copy(cm)) => cm.run(name, &run_args),
            _ => return,
        };
        self.copy_action(pane, cid, action, pipe, flags, prefix);
    }

    fn copy_action(&mut self, pane: PaneId, cid: Option<ClientId>, action: Action, pipe: Option<String>, flags: (bool, bool), prefix: Option<String>) {
        match action {
            Action::None => {}
            Action::Cancel => self.exit_mode(pane),
            Action::Run(name, args, format) => {
                let args: Vec<String> = if format { args.iter().map(|a| self.expand(self.target_for_pane(pane), cid, a)).collect() } else { args };
                self.copy_mode_command(pane, cid, &name, &args);
                return;
            }
            Action::Refresh => self.copy_refresh(pane),
            Action::Pipe { text, cancel } => {
                if let Some(cmd) = pipe {
                    pipe_to(&cmd, text);
                }
                if cancel {
                    self.exit_mode(pane);
                }
            }
            Action::Copy { text, cancel, append } => {
                if let Some(cmd) = pipe {
                    pipe_to(&cmd, text.clone());
                }
                let (no_clip, no_paste) = flags;
                if append {
                    self.copy_to_clipboard(pane, &text);
                    // window_copy_append_selection: the top automatic buffer (paste_get_top)
                    // is replaced under its name by its data followed by the selection
                    match self.buffers.iter().find(|b| b.automatic).map(|b| b.name.clone()) {
                        Some(top) => self.add_buffer(Some(top), text.into_bytes(), true),
                        None => self.paste_add("buffer", text.into_bytes()),
                    }
                } else {
                    if !no_clip {
                        self.copy_to_clipboard(pane, &text);
                    }
                    if !no_paste {
                        // paste_add(prefix, ...): the automatic name uses the prefix
                        self.paste_add(prefix.as_deref().unwrap_or("buffer"), text.into_bytes());
                    }
                }
                if cancel {
                    self.exit_mode(pane);
                }
            }
            Action::Prompt { label, command } => {
                if cid.is_some() {
                    // the pane is the prompt's context target (a %id in the template would read as %N);
                    // tmux 3.8's copy-mode bindings prompt in the pane (command-prompt -P)
                    let template = format!("send-keys -X {command} -- '%%'");
                    let mut pr = Prompt::new(PromptKind::Command, vec![(format!("{label} "), String::new())], template, Some(pane));
                    pr.id = self.next_prompt_id();
                    // the default copy-mode search bindings use command-prompt -T search
                    pr.history_type = usize::from(command.starts_with("search"));
                    pr.host = Some(pane);
                    if command.ends_with("-incremental") {
                        self.copy_sync_options(pane);
                        if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                            cm.begin_incremental_search(command.starts_with("search-forward"));
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

    /// window_copy_copy_buffer's clipboard half (set-clipboard): screen_write_setselection
    /// on the pane with no selection parameter (OSC 52 ;;), so the terminals of the attached clients showing its window get the
    /// OSC 52 (Ms), whichever client (or command) did the copy.
    fn copy_to_clipboard(&mut self, pane: PaneId, text: &str) {
        if text.is_empty() || self.opt_server("set-clipboard") == "off" {
            return;
        }
        let Some(wid) = self.window_of(pane) else { return };
        let enc = base64(text.as_bytes());
        for c in self.clients.values().filter(|c| c.attached && c.control == 0 && c.features.has("clipboard")) {
            let shown = c.session.and_then(|s| self.sessions.get(&s)).and_then(|s| s.links.get(&s.active)).is_some_and(|w| *w == wid);
            if shown {
                c.send(S_OUTPUT, features::string_capability(&c.term_caps, "Ms", "\x1b]52;%p1%s;%p2%s\x07", &["", &enc]).as_bytes());
            }
        }
        // window_copy_copy_buffer
        self.fire_pane("pane-set-clipboard", pane);
    }

    /// refresh-now and the automatic refresh: the pane's content again.
    pub(super) fn copy_refresh(&mut self, pane: PaneId) {
        let Some(p) = self.panes.get_mut(&pane) else { return };
        let lines = p.snapshot();
        p.track.unseen_changes = false;
        if let Some(PaneMode::Copy(cm)) = &mut p.mode {
            cm.refresh(lines);
        }
        if let Some(wid) = self.window_of(pane) {
            self.mark_window_dirty(wid);
        }
    }

    /// window_copy_refresh_timer for every pane with refresh on: new output is shown while
    /// nothing is being selected.
    pub(super) fn copy_refresh_timers(&mut self, now: Instant) {
        let mut due = Vec::new();
        for (id, p) in self.panes.iter_mut() {
            let Some(PaneMode::Copy(cm)) = &mut p.mode else { continue };
            if !cm.refresh_active() {
                cm.refresh_due = None;
                continue;
            }
            match cm.refresh_due {
                Some(d) if d > now => {}
                Some(_) => {
                    cm.refresh_due = Some(now + REFRESH_INTERVAL);
                    if p.track.unseen_changes && cm.refresh_ready() {
                        due.push(*id);
                    }
                }
                None => cm.refresh_due = Some(now + REFRESH_INTERVAL),
            }
        }
        for id in due {
            self.copy_refresh(id);
        }
    }

    /// When the next refresh timer is due.
    pub(super) fn copy_refresh_deadline(&self) -> Option<Instant> {
        self.panes.values().filter_map(|p| match &p.mode {
            Some(PaneMode::Copy(cm)) if cm.refresh_active() => Some(cm.refresh_due.unwrap_or_else(Instant::now)),
            _ => None,
        }).min()
    }

    /// copy-mode -S and scroll-to-mouse (window_copy_scroll): drag the scrollbar slider so
    /// the point of it the drag started on (the client's slider_mpos) follows the mouse.
    /// Later drag events of the same drag go here too (DragKind::Scrollbar keeps `exit`).
    pub(super) fn copy_scroll_to_mouse(&mut self, cid: ClientId, pane: PaneId, m: &MouseCtx, exit: bool) {
        let Some(wid) = self.window_of(pane) else { return };
        let Some((_, r)) = self.rects(wid).0.into_iter().find(|(p, _)| *p == pane) else { return };
        let area = self.scrollbar_area(wid, r);
        let Some(c) = self.clients.get_mut(&cid) else { return };
        c.drag = Some(Drag { kind: DragKind::Scrollbar { pane, exit } });
        let sl_mpos = c.slider_mpos as usize;
        let my = m.y.saturating_sub(if m.statusat == Some(0) { m.statuslines } else { 0 }) as i64 - area.y as i64;
        let leave = match self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
            Some(PaneMode::Copy(cm)) => cm.scroll_to_slider(sl_mpos, my, exit),
            _ => return,
        };
        if leave {
            self.exit_mode(pane);
        } else {
            self.show_scrollbar(pane);
        }
        self.mark_window_dirty(wid);
    }

    /// The copy-mode-*-style options for a pane, and its position indicator.
    pub(super) fn copy_render_style(&self, pane: PaneId, cid: Option<ClientId>) -> RenderStyle {
        let t = self.target_for_pane(pane);
        let wid = self.window_of(pane);
        let style = |name: &str| {
            let ctx = vars::VarCtx { srv: self, t, client: cid, extra: HashMap::new() };
            format::parse_style_with(&self.opt_window(wid, name), Style::default(), Some(&ctx)).map(|p| p.style).unwrap_or_default()
        };
        let current_line = (self.opt_window(wid, "copy-mode-current-line-style") != "default").then(|| style("copy-mode-current-line-style"));
        let show = matches!(self.panes.get(&pane).and_then(|p| p.mode.as_ref()), Some(PaneMode::Copy(cm)) if cm.position_shown());
        let position_text = if show { Some(self.expand(t, cid, &self.opt_pane(pane, "copy-mode-position-format"))) } else { None };
        // noattr is not kept in a Style: look for it in the expanded option
        let noattr = self.expand(t, cid, &self.opt_window(wid, "copy-mode-selection-style")).split([',', ' ']).any(|a| a.trim() == "noattr");
        RenderStyle {
            selection: style("copy-mode-selection-style"),
            selection_noattr: noattr,
            matched: style("copy-mode-match-style"),
            current_match: style("copy-mode-current-match-style"),
            mark: style("copy-mode-mark-style"),
            current_line,
            line_number: style("copy-mode-line-number-style"),
            current_line_number: style("copy-mode-current-line-number-style"),
            position: style("copy-mode-position-style"),
            position_text,
        }
    }

    /// A copy-mode format variable for a pane (window_copy_formats).
    pub(super) fn copy_var(&self, pane: PaneId, name: &str) -> Option<String> {
        let Some(PaneMode::Copy(cm)) = self.panes.get(&pane).and_then(|p| p.mode.as_ref()) else { return None };
        let b = |v: bool| if v { "1".to_string() } else { "0".to_string() };
        let (cx, cy) = cm.cursor();
        let sel = cm.selection_ends();
        Some(match name {
            "scroll_position" => cm.scroll_offset().to_string(),
            "copy_position" => cm.position().0.to_string(),
            "copy_position_limit" => cm.position().1.to_string(),
            "copy_line_numbers" => b(cm.line_numbers_active()),
            "refresh_active" => b(cm.refresh_active()),
            "rectangle_toggle" => b(cm.rectangle()),
            "copy_cursor_x" => cx.to_string(),
            "copy_cursor_y" => cy.to_string(),
            "selection_start_x" => sel?.0.0.to_string(),
            "selection_start_y" => sel?.0.1.to_string(),
            "selection_end_x" => sel?.1.0.to_string(),
            "selection_end_y" => sel?.1.1.to_string(),
            "selection_active" => b(cm.selection_following()),
            "selection_present" => b(cm.selection_present()),
            "selection_mode" => cm.selection_mode().name().to_string(),
            "search_present" => b(cm.search_present()),
            "search_timed_out" => b(cm.search_timed_out()),
            "search_count" => cm.search_count()?.to_string(),
            "search_count_partial" => cm.search_count().map(|_| b(cm.search_count_partial()))?,
            "search_match" => cm.search_match()?,
            // the time lines entered the history is not kept
            "top_line_time" => "0".into(),
            "copy_cursor_word" => {
                let ws = self.opt_session(self.session_of_pane(pane).map(|s| s.0), "word-separators");
                super::mouse::grid_word(|i| cm.line(i).cloned(), cm.line_count(), cx, cm.view_top() + cy, &ws)?
            }
            "copy_cursor_line" => Some(cm.cursor_line()).filter(|l| !l.is_empty())?,
            _ => return None,
        })
    }
}

/// Gives text to a shell command's standard input (copy-pipe and pipe).
fn pipe_to(cmd: &str, text: String) {
    let mut c = procinfo::job_command(cmd);
    c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if let Ok(mut child) = c.spawn() {
        thread::spawn(move || {
            if let Some(mut si) = child.stdin.take() {
                let _ = si.write_all(text.as_bytes());
            }
            let _ = child.wait();
        });
    }
}
