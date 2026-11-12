//! Format variables (#{session_name}, #{pane_current_path}, ...) for a target.

use super::*;
use crate::format::{self, Vars};

#[derive(Clone, Copy, Default, Debug)]
pub struct Target {
    pub sid: Option<SessId>,
    pub idx: Option<u32>,
    pub wid: Option<WinId>,
    pub pane: Option<PaneId>,
}

pub struct VarCtx<'a> {
    pub srv: &'a Server,
    pub t: Target,
    pub client: Option<ClientId>,
    pub extra: HashMap<String, String>,
}

fn b(v: bool) -> String {
    if v { "1".into() } else { "0".into() }
}

impl Server {
    pub fn client_is_readonly(&self, client: Option<ClientId>) -> bool {
        client.is_some_and(|id| self.clients.get(&id).is_none_or(|c| c.denied || c.is_readonly()))
    }

    pub fn target_for_pane(&self, pane: PaneId) -> Target {
        let wid = self.window_of(pane);
        let (sid, idx) = match self.session_of_pane(pane) {
            Some((s, i)) => (Some(s), Some(i)),
            None => (None, None),
        };
        Target { sid, idx, wid, pane: Some(pane) }
    }

    pub fn expand_for_pane(&self, pane: PaneId, fmt: &str) -> String {
        self.expand(self.target_for_pane(pane), None, fmt)
    }

    pub fn expand(&self, t: Target, client: Option<ClientId>, fmt: &str) -> String {
        format::expand(fmt, &VarCtx { srv: self, t, client, extra: HashMap::new() })
    }

    pub fn expand_time(&self, t: Target, client: Option<ClientId>, fmt: &str) -> String {
        format::expand_time(fmt, &VarCtx { srv: self, t, client, extra: HashMap::new() })
    }

    pub fn expand_extra(&self, t: Target, client: Option<ClientId>, fmt: &str, extra: HashMap<String, String>) -> String {
        format::expand(fmt, &VarCtx { srv: self, t, client, extra })
    }

    pub fn window_flags(&self, sid: SessId, idx: u32) -> String {
        let s = &self.sessions[&sid];
        let Some(wid) = s.links.get(&idx) else { return String::new() };
        let w = &self.windows[wid];
        let alerts = s.alerts.get(&idx).copied().unwrap_or(0);
        let mut f = String::new();
        if s.active == idx {
            f.push('*');
        }
        if s.last.first() == Some(&idx) {
            f.push('-');
        }
        if alerts & ALERT_ACTIVITY != 0 {
            f.push('#');
        }
        if alerts & ALERT_BELL != 0 {
            f.push('!');
        }
        if alerts & ALERT_SILENCE != 0 {
            f.push('~');
        }
        if self.marked.is_some_and(|m| w.panes().contains(&m)) {
            f.push('M');
        }
        if w.zoomed {
            f.push('Z');
        }
        f
    }
}

impl Vars for VarCtx<'_> {
    fn contexts(&self, kind: char, flags: &str) -> Vec<(Box<dyn Vars + '_>, bool)> {
        let mut targets = Vec::new();
        match kind {
            'S' => {
                let mut sessions: Vec<_> = self.srv.sessions.values().collect();
                if flags.contains('n') { sessions.sort_by(|a, b| a.name.cmp(&b.name)); }
                else if flags.contains('t') { sessions.sort_by_key(|s| (std::cmp::Reverse(s.activity), s.id)); }
                for s in sessions {
                    let wid = s.links.get(&s.active).copied();
                    let pane = wid.and_then(|w| self.srv.windows.get(&w)).map(|w| w.active);
                    targets.push((Target { sid: Some(s.id), idx: Some(s.active), wid, pane }, false));
                }
            }
            'W' => {
                if let Some(s) = self.t.sid.and_then(|s| self.srv.sessions.get(&s)) {
                    for (&idx, &wid) in &s.links {
                        let pane = self.srv.windows.get(&wid).map(|w| w.active);
                        targets.push((Target { sid: Some(s.id), idx: Some(idx), wid: Some(wid), pane }, idx == s.active));
                    }
                }
            }
            'P' => {
                if let Some(w) = self.t.wid.and_then(|w| self.srv.windows.get(&w)) {
                    let mut panes = w.panes();
                    panes.sort_unstable();
                    for pane in panes {
                        targets.push((Target { pane: Some(pane), ..self.t }, pane == w.active));
                    }
                }
            }
            _ => {}
        }
        if kind == 'W' && flags.contains('n') {
            targets.sort_by_key(|(t, _)| self.srv.windows[&t.wid.unwrap()].name.clone());
        } else if kind == 'W' && flags.contains('t') {
            targets.sort_by_key(|(t, _)| std::cmp::Reverse(self.srv.windows[&t.wid.unwrap()].activity));
        }
        if flags.contains('r') { targets.reverse(); }
        targets.into_iter().map(|(t, active)| {
            let ctx: Box<dyn Vars + '_> = Box::new(VarCtx { srv: self.srv, t, client: self.client, extra: self.extra.clone() });
            (ctx, active)
        }).collect()
    }

    fn search(&self, pattern: &str, regex: bool, ignore_case: bool) -> usize {
        let Some(p) = self.t.pane.and_then(|p| self.srv.panes.get(&p)) else { return 0 };
        let screen = p.screen();
        let (rows, cols) = screen.size();
        let pattern = if regex { pattern.to_string() } else { format!("*{pattern}*") };
        (0..rows.get()).find(|&row| {
            let text = screen.contents_between(row, 0, row, cols.get());
            if regex { format::regex_matches(&pattern, text.trim_end(), ignore_case) }
            else { format::glob_match(&pattern, text.trim_end(), ignore_case) }
        }).map_or(0, |row| row as usize + 1)
    }

    /// Like tmux: run the command in the background, show its last output line until the
    /// next run, and re-run at most every status-interval seconds.
    fn shell(&self, command: &str) -> Option<String> {
        let srv = self.srv;
        // Check the originating request as well as the format's target client.
        // Do not expose cached output of privileged jobs to a restricted caller.
        if srv.readonly_context.get() || srv.client_is_readonly(self.client) {
            return None;
        }
        let interval = Duration::from_secs(srv.opt_session_num(self.t.sid, "status-interval").max(1) as u64);
        let mut cache = srv.shell_cache.borrow_mut();
        let stale = cache.get(command).is_none_or(|e| !e.2 && e.1.elapsed() >= interval);
        if stale {
            let prev = cache.get(command).map(|e| e.0.clone()).unwrap_or_default();
            cache.insert(command.to_string(), (prev, Instant::now(), true));
            let tx = srv.tx.clone();
            let cmd = command.to_string();
            let cwd = self.t.pane.and_then(|p| srv.panes.get(&p)).map(|p| p.current_path.clone());
            thread::spawn(move || {
                let mut c = procinfo::job_command(&cmd);
                c.stdin(std::process::Stdio::null());
                if let Some(d) = cwd.filter(|d| std::path::Path::new(d).is_dir()) {
                    c.current_dir(d);
                }
                let out = c.output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
                // tmux shows the last line of output
                let line = out.trim_end().lines().last().unwrap_or("").to_string();
                let _ = tx.send(Event::FormatShell(cmd, line));
            });
        }
        cache.get(command).map(|e| e.0.clone())
    }

    fn get(&self, name: &str) -> Option<String> {
        if let Some(v) = self.extra.get(name) {
            return Some(v.clone());
        }
        let srv = self.srv;
        let t = self.t;
        let sess = t.sid.and_then(|s| srv.sessions.get(&s));
        let win = t.wid.and_then(|w| srv.windows.get(&w));
        let pane = t.pane.and_then(|p| srv.panes.get(&p));
        let client = self.client.and_then(|c| srv.clients.get(&c));
        let v: Option<String> = match name {
            "host" => Some(srv.host.clone()),
            "host_short" => Some(srv.host.split('.').next().unwrap_or("").to_string()),
            "pid" => Some(std::process::id().to_string()),
            // the tmux release this aims to match, so configs' version checks work
            "version" => Some(TMUX_VERSION.into()),
            "current_file" => srv.cur_file.clone(),
            "socket_path" => Some(ipc::describe()),
            "start_time" => Some(srv.start_time.to_string()),
            "server_sessions" => Some(srv.sessions.len().to_string()),
            // session
            "session_name" => sess.map(|s| s.name.clone()),
            "session_id" => sess.map(|s| format!("${}", s.id)),
            "session_windows" => sess.map(|s| s.links.len().to_string()),
            "session_attached" => sess.map(|s| srv.clients.values().filter(|c| c.attached && c.session == Some(s.id)).count().to_string()),
            "session_many_attached" => sess.map(|s| b(srv.clients.values().filter(|c| c.attached && c.session == Some(s.id)).count() > 1)),
            "session_created" => sess.map(|s| s.created.to_string()),
            "session_activity" => sess.map(|s| s.activity.to_string()),
            "session_last_attached" => sess.map(|s| s.last_attached.to_string()),
            "session_path" => sess.map(|s| s.cwd.clone()),
            "session_alerts" => sess.map(|s| {
                s.alerts
                    .iter()
                    .filter(|(_, a)| **a != 0)
                    .map(|(i, a)| {
                        let mut x = i.to_string();
                        if a & ALERT_ACTIVITY != 0 {
                            x.push('#');
                        }
                        if a & ALERT_BELL != 0 {
                            x.push('!');
                        }
                        if a & ALERT_SILENCE != 0 {
                            x.push('~');
                        }
                        x
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }),
            "session_grouped" => sess.map(|s| b(s.group.is_some())),
            "session_group" | "session_group_size" | "session_group_list" | "session_group_attached" => {
                let s = sess?;
                let Some(g) = s.group else { return Some(if name == "session_group_size" || name == "session_group_attached" { "0".into() } else { String::new() }) };
                let members: Vec<&Session> = srv.sessions.values().filter(|x| x.group == Some(g)).collect();
                Some(match name {
                    // tmux names a group after its first session
                    "session_group" => members.iter().min_by_key(|x| x.created).map(|x| x.name.clone()).unwrap_or_default(),
                    "session_group_size" => members.len().to_string(),
                    "session_group_attached" => members.iter().map(|x| srv.clients.values().filter(|c| c.attached && c.session == Some(x.id)).count()).sum::<usize>().to_string(),
                    _ => members.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(","),
                })
            }
            "session_format" => Some(b(sess.is_some())),
            "session_width" => sess.map(|s| s.cols.to_string()),
            "session_height" => sess.map(|s| s.rows.to_string()),
            // window
            "window_index" => t.idx.map(|i| i.to_string()),
            "window_name" => win.map(|w| w.name.clone()),
            "window_id" => win.map(|w| format!("@{}", w.id)),
            "window_active" => Some(b(sess.zip(t.idx).is_some_and(|(s, i)| s.active == i))),
            "window_last_flag" => Some(b(sess.zip(t.idx).is_some_and(|(s, i)| s.last.first() == Some(&i)))),
            "window_flags" | "window_raw_flags" => t.sid.zip(t.idx).map(|(s, i)| srv.window_flags(s, i)),
            "window_panes" => win.map(|w| w.panes().len().to_string()),
            "window_zoomed_flag" => win.map(|w| b(w.zoomed)),
            "window_width" => win.map(|w| w.sx.to_string()),
            "window_height" => win.map(|w| w.sy.to_string()),
            "window_layout" | "window_visible_layout" => win.map(|w| w.layout.to_layout_string(srv.window_area(w.id))),
            "window_activity" => win.map(|w| w.activity.to_string()),
            "window_created" => win.map(|w| w.created.to_string()),
            "window_activity_flag" => Some(b(sess.zip(t.idx).is_some_and(|(s, i)| s.alerts.get(&i).is_some_and(|a| a & ALERT_ACTIVITY != 0)))),
            "window_bell_flag" => Some(b(sess.zip(t.idx).is_some_and(|(s, i)| s.alerts.get(&i).is_some_and(|a| a & ALERT_BELL != 0)))),
            "window_silence_flag" => Some(b(sess.zip(t.idx).is_some_and(|(s, i)| s.alerts.get(&i).is_some_and(|a| a & ALERT_SILENCE != 0)))),
            "window_marked_flag" => Some(b(win.is_some_and(|w| srv.marked.is_some_and(|m| w.panes().contains(&m))))),
            "window_start_flag" => Some(b(sess.zip(t.idx).is_some_and(|(s, i)| s.links.keys().next() == Some(&i)))),
            "window_end_flag" => Some(b(sess.zip(t.idx).is_some_and(|(s, i)| s.links.keys().next_back() == Some(&i)))),
            "window_linked" => win.map(|w| b(srv.links_of(w.id).len() > 1)),
            "window_linked_sessions" => win.map(|w| srv.links_of(w.id).len().to_string()),
            "window_bigger" => Some("0".into()),
            "window_offset_x" | "window_offset_y" => Some("0".into()),
            "window_format" => Some(b(win.is_some())),
            // pane
            "pane_index" => win.zip(t.pane).and_then(|(w, p)| {
                let base = srv.opt_window_num(Some(w.id), "pane-base-index").max(0) as usize;
                w.panes().iter().position(|x| *x == p).map(|i| (i + base).to_string())
            }),
            "pane_id" => t.pane.map(|p| format!("%{p}")),
            "pane_title" => pane.map(|p| p.title.clone()),
            "pane_current_path" => pane.map(|p| p.current_path.clone()),
            "pane_current_command" => pane.map(|p| p.current_cmd.clone()),
            "pane_start_command" => pane.map(|p| p.start_cmd.clone()),
            "pane_start_path" => pane.map(|p| p.start_path.clone()),
            "pane_pid" => pane.and_then(|p| p.pid).map(|p| p.to_string()),
            "pane_tty" => Some(String::new()),
            "pane_width" => pane.map(|p| p.size.0.to_string()),
            "pane_height" => pane.map(|p| p.size.1.to_string()),
            "pane_active" => Some(b(win.zip(t.pane).is_some_and(|(w, p)| w.active == p))),
            "pane_last" => Some(b(win.zip(t.pane).is_some_and(|(w, p)| w.last_active == Some(p)))),
            "pane_marked" => Some(b(t.pane.is_some() && srv.marked == t.pane)),
            "pane_marked_set" => Some(b(srv.marked.is_some())),
            "pane_dead" => pane.map(|p| b(p.dead.is_some())),
            "pane_dead_status" => pane.and_then(|p| p.dead).and_then(|d| d.0).map(|s| s.to_string()),
            "pane_dead_time" => pane.and_then(|p| p.dead).map(|d| d.1.to_string()),
            "pane_dead_signal" => Some(String::new()),
            "pane_in_mode" => pane.map(|p| b(p.mode.is_some())),
            "pane_mode" => pane.map(|p| match &p.mode {
                Some(PaneMode::Copy(_)) => "copy-mode".into(),
                Some(PaneMode::Clock) => "clock-mode".into(),
                Some(PaneMode::Tree(_)) => "tree-mode".into(),
                None => String::new(),
            }),
            "pane_synchronized" => t.pane.map(|p| b(options::is_on(&srv.opt_pane(p, "synchronize-panes")))),
            "pane_format" => Some(b(pane.is_some())),
            "pane_created" => pane.map(|p| p.created.to_string()),
            "pane_left" | "pane_top" | "pane_right" | "pane_bottom" | "pane_at_left" | "pane_at_right" | "pane_at_top" | "pane_at_bottom" => {
                let (w, p) = (win?, t.pane?);
                let area = srv.window_area(w.id);
                // a pane hidden by zoom keeps its place in the unzoomed layout
                let r = if w.zoomed && p != w.active {
                    let (mut panes, mut seps) = (Vec::new(), Vec::new());
                    w.layout.layout(area, &mut panes, &mut seps);
                    panes.into_iter().find(|(id, _)| *id == p)?.1
                } else {
                    srv.rects(w.id).0.into_iter().find(|(id, _)| *id == p)?.1
                };
                // position of the pane's content (below a pane-border-status line)
                let c = srv.content_rect(w.id, r);
                Some(match name {
                    "pane_left" => c.x.to_string(),
                    "pane_top" => c.y.to_string(),
                    "pane_right" => (c.right().saturating_sub(1)).to_string(),
                    "pane_bottom" => (c.bottom().saturating_sub(1)).to_string(),
                    "pane_at_left" => b(r.x == 0),
                    "pane_at_top" => b(r.y == 0),
                    "pane_at_right" => b(r.right() >= area.w),
                    _ => b(r.bottom() >= area.h),
                })
            }
            "cursor_x" => pane.map(|p| p.screen().cursor_position().1.to_string()),
            "cursor_y" => pane.map(|p| p.screen().cursor_position().0.to_string()),
            "cursor_flag" => pane.map(|p| b(!p.screen().hide_cursor())),
            "alternate_on" => pane.map(|p| b(p.screen().alternate_screen())),
            "history_size" => pane.map(|p| p.screen().scrollback_len().to_string()),
            "history_limit" => pane.map(|p| p.screen().scrollback_capacity().to_string()),
            "scroll_position" => pane.and_then(|p| match &p.mode {
                Some(PaneMode::Copy(c)) => Some(c.scroll_offset().to_string()),
                _ => None,
            }),
            "copy_cursor_x" | "copy_cursor_y" => pane.and_then(|p| match &p.mode {
                Some(PaneMode::Copy(c)) => {
                    let (y, x) = c.render().1;
                    Some(if name == "copy_cursor_x" { x } else { y }.to_string())
                }
                _ => None,
            }),
            "selection_start_x" | "selection_start_y" | "selection_end_x" | "selection_end_y" => pane.and_then(|p| match &p.mode {
                Some(PaneMode::Copy(c)) => c.selection_ends().map(|(s, e)| {
                    match name {
                        "selection_start_x" => s.0,
                        "selection_start_y" => s.1,
                        "selection_end_x" => e.0,
                        _ => e.1,
                    }
                    .to_string()
                }),
                _ => None,
            }),
            "selection_present" => pane.and_then(|p| match &p.mode {
                Some(PaneMode::Copy(c)) => Some(b(c.selection_active())),
                _ => None,
            }),
            "mouse_x" | "mouse_y" | "mouse_word" | "mouse_line" | "mouse_hyperlink" | "mouse_status_line" | "mouse_status_range" | "mouse_pane" => {
                self.srv.mouse_var(name, self.client)
            }
            "mouse_any_flag" => pane.map(|p| b(p.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None)),
            "mouse_standard_flag" => pane.map(|p| b(matches!(p.screen().mouse_protocol_mode(), vt100::MouseProtocolMode::PressRelease | vt100::MouseProtocolMode::Press))),
            "mouse_button_flag" => pane.map(|p| b(p.screen().mouse_protocol_mode() == vt100::MouseProtocolMode::ButtonMotion)),
            "mouse_all_flag" => pane.map(|p| b(p.screen().mouse_protocol_mode() == vt100::MouseProtocolMode::AnyMotion)),
            "mouse_sgr_flag" => pane.map(|p| b(p.screen().mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr)),
            "keypad_cursor_flag" => pane.map(|p| b(p.screen().application_cursor())),
            "keypad_flag" => pane.map(|p| b(p.screen().application_keypad())),
            "insert_flag" | "origin_flag" | "wrap_flag" => Some("0".into()),
            // client
            "client_name" => client.map(|c| c.name()),
            "client_tty" => client.map(|c| c.name()),
            "client_pid" => client.and_then(|c| c.ident.as_ref()).map(|i| i.pid.to_string()),
            "client_width" => client.map(|c| c.size.0.to_string()),
            "client_height" => client.map(|c| c.size.1.to_string()),
            "client_session" => client.and_then(|c| c.session).and_then(|s| srv.sessions.get(&s)).map(|s| s.name.clone()),
            "client_last_session" => client.and_then(|c| c.last_session).and_then(|s| srv.sessions.get(&s)).map(|s| s.name.clone()),
            "client_key_table" => client.map(|c| c.key_table.clone()),
            "client_prefix" => client.map(|c| b(c.key_table == "prefix")),
            "client_readonly" => client.map(|c| b(c.is_readonly())),
            "client_termname" | "client_termtype" => client.and_then(|c| c.ident.as_ref()).map(|i| i.term.clone()),
            "client_termfeatures" => client.map(|c| c.features.list()),
            "client_user" => client.and_then(|c| c.user.as_ref()).map(|u| ipc::user_name_for_id(u)),
            "client_created" => client.map(|c| c.created.to_string()),
            "client_activity" => client.map(|c| c.activity.to_string()),
            "client_utf8" => Some("1".into()),
            "client_flags" => client.map(|c| if c.attached { "attached".into() } else { String::new() }),
            "client_cwd" => client.and_then(|c| c.ident.as_ref()).map(|i| i.cwd.clone()),
            _ => None,
        };
        if v.is_some() {
            return v;
        }
        // Options are available as format variables too.
        if options::def(name).is_some() || name.starts_with('@') {
            let value = |v: &str| {
                if options::def(name).is_some_and(|d| matches!(d.kind, options::Kind::Flag)) {
                    b(options::is_on(v))
                } else { v.to_string() }
            };
            if let Some(p) = t.pane {
                if let Some(v) = srv.panes.get(&p).and_then(|p| p.options.get(name)) {
                    return Some(value(v));
                }
            }
            if let Some(v) = win.and_then(|w| w.options.get(name)) {
                return Some(value(v));
            }
            if let Some(v) = sess.and_then(|s| s.options.get(name)) {
                return Some(value(v));
            }
            return srv.opts_window.get(name).or(srv.opts_session.get(name)).or(srv.opts_server.get(name)).map(value);
        }
        None
    }

    fn environ(&self, name: &str) -> Option<String> {
        let sess = self.t.sid.and_then(|s| self.srv.sessions.get(&s));
        match sess.and_then(|s| s.env.get(name)) {
            Some(v) => v.clone(),
            None => self.srv.env.get(name).cloned().flatten(),
        }
    }

    fn user_options(&self) -> Vec<(String, String)> {
        self.t.wid.and_then(|w| self.srv.windows.get(&w)).map(|w| w.options.0.iter()
            .filter(|(k, _)| k.starts_with('@')).map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default()
    }

    fn client_info(&self, kind: char, name: &str) -> Option<String> {
        let c = self.client.and_then(|id| self.srv.clients.get(&id)).filter(|c| c.attached)?;
        let ident = c.ident.as_ref()?;
        Some(match kind {
            'e' => ident.env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or_default(),
            'f' => b(c.features.list().split(',').any(|f| f == name)),
            // No terminfo database is kept, so no capability is known.
            _ => "0".into(),
        })
    }

    fn client_contexts(&self, flags: &str) -> Vec<Box<dyn Vars + '_>> {
        // Attached clients in creation order, or by name (n) or most recent activity (t).
        let mut clients: Vec<_> = self.srv.clients.values().filter(|c| c.attached).collect();
        clients.sort_by_key(|c| c.id);
        if flags.contains('n') {
            clients.sort_by_key(|c| c.name());
        } else if flags.contains('t') {
            clients.sort_by_key(|c| (std::cmp::Reverse(c.activity), c.name()));
        }
        if flags.contains('r') {
            clients.reverse();
        }
        clients.into_iter().map(|c| -> Box<dyn Vars + '_> {
            Box::new(VarCtx { srv: self.srv, t: self.t, client: Some(c.id), extra: self.extra.clone() })
        }).collect()
    }

    fn entries(&self, kind: char, flags: &str) -> Vec<HashMap<String, String>> {
        let srv = self.srv;
        if kind == 'V' {
            // V or V/s: the session environment, V/g: global, V/c: the client's.
            let mut env: Vec<(String, Option<String>)> = match flags {
                "" | "s" => self.t.sid.and_then(|s| srv.sessions.get(&s)).map(|s| s.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default(),
                "g" => srv.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                "c" => self.client.and_then(|c| srv.clients.get(&c)).and_then(|c| c.ident.as_ref())
                    .map(|i| i.env.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect()).unwrap_or_default(),
                _ => Vec::new(),
            };
            env.sort();
            env.dedup_by(|a, b| a.0 == b.0);
            return env.into_iter().map(|(name, value)| HashMap::from([
                ("environ_name".into(), name),
                ("environ_removed".into(), b(value.is_none())),
                ("environ_value".into(), value.unwrap_or_default()),
                ("environ_hidden".into(), "0".into()),
            ])).collect();
        }
        if kind != 'O' {
            return Vec::new();
        }
        // Local options of the target by default; g for global, v server, w window, s session, p pane.
        let flags = if flags.is_empty() { "s" } else { flags };
        let global = flags.contains('g');
        let opts = if flags.contains('v') {
            Some(&srv.opts_server)
        } else if flags.contains('w') {
            if global { Some(&srv.opts_window) } else { self.t.wid.and_then(|w| srv.windows.get(&w)).map(|w| &w.options) }
        } else if flags.contains('s') {
            if global { Some(&srv.opts_session) } else { self.t.sid.and_then(|s| srv.sessions.get(&s)).map(|s| &s.options) }
        } else if flags.contains('p') {
            if global { None } else { self.t.pane.and_then(|p| srv.panes.get(&p)).map(|p| &p.options) }
        } else if global {
            Some(&srv.opts_session)
        } else {
            None
        };
        let Some(opts) = opts else { return Vec::new() };
        // Array items (name[index]) follow their option in index order.
        let mut items: Vec<(&str, Option<&str>, &String)> = opts.0.iter().map(|(key, value)| {
            match key.split_once('[') {
                Some((name, index)) => (name, Some(index.trim_end_matches(']')), value),
                None => (key.as_str(), None, value),
            }
        }).collect();
        let index_key = |i: Option<&str>| i.map(|i| (i.parse::<u64>().unwrap_or(u64::MAX), i.to_string()));
        items.sort_by(|a, b| a.0.cmp(b.0).then_with(|| index_key(a.1).cmp(&index_key(b.1))));
        items.iter().enumerate().map(|(i, &(name, index, value))| {
            let is_array = index.is_some() || options::def(name).is_some_and(|d| d.kind == options::Kind::Array);
            let count = items.iter().filter(|o| o.0 == name && o.1.is_some()).count();
            let first = index.is_none() || i == 0 || items[i - 1].0 != name;
            let last = index.is_none() || items.get(i + 1).is_none_or(|o| o.0 != name);
            HashMap::from([
                ("option_name".into(), name.to_string()),
                ("option_value".into(), value.clone()),
                ("option_is_array".into(), b(is_array)),
                ("option_array_key".into(), index.unwrap_or("").to_string()),
                ("option_array_index".into(), index.unwrap_or("").to_string()),
                ("option_array_count".into(), count.to_string()),
                ("option_array_first".into(), b(is_array && first)),
                ("option_array_last".into(), b(is_array && last)),
                // Hooks are not kept as options here.
                ("option_is_hook".into(), "0".into()),
                ("option_is_user".into(), b(name.starts_with('@'))),
            ])
        }).collect()
    }

    fn schedule_animation(&self) {
        if let Some(cid) = self.client {
            self.srv.animation_deadlines.borrow_mut().entry(cid).or_insert_with(|| Instant::now() + Duration::from_millis(100));
        }
    }
}
