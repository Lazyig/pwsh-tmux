//! Format variables (#{session_name}, #{pane_current_path}, ...) for a target.

use super::*;
use crate::cells::Color;
use crate::format::{self, Vars};

/// #{pane_mode}: the name of the pane's current mode, empty without one.
pub fn pane_mode_name(p: &Pane) -> &'static str {
    p.mode.as_ref().map_or("", |m| m.name())
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Target {
    pub sid: Option<SessId>,
    pub idx: Option<u32>,
    pub wid: Option<WinId>,
    pub pane: Option<PaneId>,
}

/// An `extra` entry naming the format's type (format_defaults' ft->type): "S", "W", "P" or
/// "" for none; without it the type is the most specific part of the target.
pub const FORMAT_TYPE: &str = "\u{1}format_type";

pub struct VarCtx<'a> {
    pub srv: &'a Server,
    pub t: Target,
    pub client: Option<ClientId>,
    pub extra: HashMap<String, String>,
}

fn b(v: bool) -> String {
    if v { "1".into() } else { "0".into() }
}

const TREE_MODE_FORMAT: &str = "#{?pane_format,#{?pane_marked,#[fg=thememagenta],}#{?pane_floating_flag,#[underscore],}#{pane_current_command}#[fg=themelightgrey]#{pane_flags}#{?#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}},: \"#{pane_title}\",},window_format,#{?window_marked_flag,#[fg=thememagenta],}#{window_name}#[fg=themelightgrey]#{window_flags}#{?#{&&:#{==:#{window_panes},1},#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}}},: \"#{pane_title}\",},#[fg=themelightgrey]#{session_windows} windows#{?session_grouped, (group #{session_group}: #{session_group_list}),}#{?session_attached, (attached),}}";

// Sizes of tmux's grid structures, for history_bytes and history_all_bytes.
const GRID_LINE: usize = 40;
const GRID_CELL: usize = 5;
const GRID_EXTD: usize = 23;

/// What tmux's grid would hold for the pane's history and screen: (lines, cells, extended
/// cells). tmux allocates a line's cells up to the last one written, in steps of a quarter,
/// half and the full width; cells with wide or non-ASCII text or 256/RGB colours are extended.
fn history_usage(screen: &vt100::Screen) -> (usize, usize, usize) {
    let width = screen.size().1.get() as usize;
    let (mut cells, mut ext) = (0, 0);
    let lines = screen.history_rows_len();
    for i in 0..lines {
        let Some(row) = screen.history_row(i) else { continue };
        let mut used = 0;
        for x in 0..width {
            let Some(c) = row.cell(x as u16) else { break };
            if c.has_contents() || c.bgcolor() != vt100::Color::Default {
                used = x + 1;
            }
            let wide_colour = |c: vt100::Color| matches!(c, vt100::Color::Rgb(..)) || matches!(c, vt100::Color::Idx(i) if i >= 16);
            if c.is_wide() || !c.contents().is_ascii() || c.contents().len() > 1 || wide_colour(c.fgcolor()) || wide_colour(c.bgcolor()) {
                ext += 1;
            }
        }
        if used > 0 {
            cells += if used < width / 4 { width / 4 } else if used < width / 2 { width / 2 } else { width };
        }
    }
    (lines, cells, ext)
}

/// tmux's pane_private_modes: the DEC private modes the application has turned on.
fn private_modes(s: &vt100::Screen) -> String {
    use vt100::MouseProtocolMode as M;
    let (blink, blink_set) = s.cursor_blinking();
    let (utf8, sgr) = s.mouse_encodings();
    let mouse = s.mouse_protocol_mode();
    [
        (s.application_cursor(), 1),
        (s.origin_mode(), 6),
        (s.wrap_mode(), 7),
        // only when the application set it, not from a cursor style
        (blink && blink_set, 12),
        (!s.hide_cursor(), 25),
        (matches!(mouse, M::PressRelease | M::Press), 1000),
        (mouse == M::ButtonMotion, 1002),
        (mouse == M::AnyMotion, 1003),
        (s.focus_events(), 1004),
        (utf8, 1005),
        (sgr, 1006),
        (s.bracketed_paste(), 2004),
        (s.synchronized_output(), 2026),
        (s.theme_updates(), 2031),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .map(|(_, n)| n.to_string())
    .collect::<Vec<_>>()
    .join(",")
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
        let _widths = self.width_context();
        format::expand(fmt, &VarCtx { srv: self, t, client, extra: HashMap::new() })
    }

    pub fn expand_time(&self, t: Target, client: Option<ClientId>, fmt: &str) -> String {
        let _widths = self.width_context();
        format::expand_time(fmt, &VarCtx { srv: self, t, client, extra: HashMap::new() })
    }

    pub fn expand_extra(&self, t: Target, client: Option<ClientId>, fmt: &str, extra: HashMap<String, String>) -> String {
        let _widths = self.width_context();
        format::expand(fmt, &VarCtx { srv: self, t, client, extra })
    }

    pub fn window_flags(&self, sid: SessId, idx: u32) -> String {
        let s = &self.sessions[&sid];
        let Some(wid) = s.links.get(&idx) else { return String::new() };
        let w = &self.windows[wid];
        let alerts = s.alerts.get(&idx).copied().unwrap_or(0);
        // tmux's window_printable_flags order: alerts, current, last, marked, zoomed
        let mut f = String::new();
        if alerts & ALERT_ACTIVITY != 0 {
            f.push('#');
        }
        if alerts & ALERT_BELL != 0 {
            f.push('!');
        }
        if alerts & ALERT_SILENCE != 0 {
            f.push('~');
        }
        if s.active == idx {
            f.push('*');
        }
        if s.last.first() == Some(&idx) {
            f.push('-');
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
    fn theme_color(&self, name: &str) -> Option<Color> {
        self.resolve_theme_colour(name)
    }

    fn colour_escape(&self, name: &str, background: bool) -> Option<String> {
        self.client_colour_escape(name, background)
    }

    fn contexts(&self, kind: char, flags: &str) -> Vec<(Box<dyn Vars + '_>, bool)> {
        let mut targets = Vec::new();
        match kind {
            'S' => {
                let mut sessions: Vec<_> = self.srv.sessions.values().collect();
                if flags.contains('n') { sessions.sort_by(|a, b| a.name.cmp(&b.name)); }
                else if flags.contains('t') { sessions.sort_by_key(|s| std::cmp::Reverse(s.activity_seq)); }
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
        // the loops' format_defaults pass the session, the winlink, or the pane
        let mut extra = self.extra.clone();
        extra.insert(FORMAT_TYPE.to_string(), match kind { 'S' => "S", 'W' => "W", _ => "P" }.to_string());
        targets.into_iter().map(|(t, active)| {
            let ctx: Box<dyn Vars + '_> = Box::new(VarCtx { srv: self.srv, t, client: self.client, extra: extra.clone() });
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
        // a hook's commands see its formats (hook, hook_*)
        if let Some(v) = self.srv.ev.cur_formats.as_ref().and_then(|f| f.get(name)) {
            return Some(v.clone());
        }
        let srv = self.srv;
        let mut t = self.t;
        let client = self.client.and_then(|c| srv.clients.get(&c));
        let ftype = match self.extra.get(FORMAT_TYPE) {
            Some(ty) => ty.as_str(),
            None if t.pane.is_some() => "P",
            None if t.wid.is_some() => "W",
            None if t.sid.is_some() => "S",
            None => "",
        };
        // format_defaults: the client's session, the session's current window and the
        // window's active pane stand in for what the target leaves out
        if t.sid.is_none() {
            t.sid = client.and_then(|c| c.session).filter(|s| srv.sessions.contains_key(s));
        }
        if t.wid.is_none() && let Some(s) = t.sid.and_then(|s| srv.sessions.get(&s)) {
            t.idx = Some(s.active);
            t.wid = s.links.get(&s.active).copied();
        }
        if t.pane.is_none() {
            t.pane = t.wid.and_then(|w| srv.windows.get(&w)).map(|w| w.active);
        }
        let sess = t.sid.and_then(|s| srv.sessions.get(&s));
        let win = t.wid.and_then(|w| srv.windows.get(&w));
        let pane = t.pane.and_then(|p| srv.panes.get(&p));
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
            // session, window and pane ids share one counter here
            "next_session_id" => Some(format!("${}", srv.next_session_id)),
            "config_files" => Some(srv.cfg_files.join(",")),
            "sixel_support" => Some("0".into()),
            "uid" => Some(ipc::own_user_id()),
            "user" => Some(ipc::user_name_for_id(&ipc::own_user_id())),
            // default formats of choose-buffer, choose-client and choose-tree in tmux 3.8
            "buffer_mode_format" => Some("#{t/p:buffer_created}: #{buffer_sample}".into()),
            "client_mode_format" => Some("#[fg=themelightgrey]#{t/p:client_activity}: session #[default]#{session_name}".into()),
            "tree_mode_format" => Some(TREE_MODE_FORMAT.into()),
            // outside a buffer list, buffer_* describe the most recent automatic
            // buffer (format_defaults: paste_get_top)
            "buffer_name" | "buffer_size" | "buffer_sample" | "buffer_created" => {
                srv.buffers.iter().find(|b| b.automatic).and_then(|b| super::cmds::buffer_vars(b).remove(name))
            }
            "buffer_full" => match self.extra.get("buffer_name") {
                Some(n) => srv.buffers.iter().find(|b| &b.name == n),
                None => srv.buffers.iter().find(|b| b.automatic),
            }.map(|b| String::from_utf8_lossy(&b.data).into_owned()),
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
                    .join(",")
            }),
            "session_grouped" => sess.map(|s| b(s.group.is_some())),
            "session_group" | "session_group_size" | "session_group_list" | "session_group_attached" => {
                let s = sess?;
                // no value outside a group
                let g = s.group?;
                let members: Vec<&Session> = srv.sessions.values().filter(|x| x.group == Some(g)).collect();
                Some(match name {
                    // tmux names a group after its first session
                    "session_group" => srv.group_name(g),
                    "session_group_size" => members.len().to_string(),
                    "session_group_attached" => members.iter().map(|x| srv.clients.values().filter(|c| c.attached && c.session == Some(x.id)).count()).sum::<usize>().to_string(),
                    _ => members.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(","),
                })
            }
            "session_format" => Some(b(ftype == "S")),
            "session_active" => sess.zip(client).map(|(s, c)| b(c.session == Some(s.id))),
            "session_activity_flag" | "session_bell_flag" | "session_silence_flag" => sess.map(|s| {
                let flag = match name { "session_activity_flag" => ALERT_ACTIVITY, "session_bell_flag" => ALERT_BELL, _ => ALERT_SILENCE };
                b(s.alerts.values().any(|a| a & flag != 0))
            }),
            // each alert once, in the order windows first show them
            "session_alert" => sess.map(|s| {
                let mut out = String::new();
                for a in s.alerts.values() {
                    for (flag, c) in [(ALERT_ACTIVITY, '#'), (ALERT_BELL, '!'), (ALERT_SILENCE, '~')] {
                        if a & flag != 0 && !out.contains(c) {
                            out.push(c);
                        }
                    }
                }
                out
            }),
            "session_attached_list" => sess.map(|s| srv.clients.values().filter(|c| c.attached && c.session == Some(s.id)).map(|c| c.name()).collect::<Vec<_>>().join(",")).filter(|v| !v.is_empty()),
            "session_group_attached_list" | "session_group_many_attached" => {
                let g = sess?.group?;
                let members: Vec<SessId> = srv.sessions.values().filter(|x| x.group == Some(g)).map(|x| x.id).collect();
                let names: Vec<String> = srv.clients.values().filter(|c| c.attached && c.session.is_some_and(|s| members.contains(&s))).map(|c| c.name()).collect();
                if name == "session_group_many_attached" { Some(b(names.len() > 1)) } else { Some(names.join(",")).filter(|v| !v.is_empty()) }
            }
            "session_marked" => sess.map(|s| b(srv.marked.and_then(|m| srv.window_of(m)).is_some_and(|w| s.links.values().any(|x| *x == w)))),
            "session_stack" => sess.map(|s| std::iter::once(&s.active).chain(s.last.iter()).map(|i| i.to_string()).collect::<Vec<_>>().join(",")),
            "active_window_index" => sess.map(|s| s.active.to_string()),
            "last_window_index" => sess.and_then(|s| s.links.keys().next_back()).map(|i| i.to_string()),
            "session_width" => sess.map(|s| s.cols.to_string()),
            "session_height" => sess.map(|s| s.rows.to_string()),
            // window
            "window_index" => t.idx.map(|i| i.to_string()),
            "window_name" => win.map(|w| w.name.clone()),
            "window_id" => win.map(|w| format!("@{}", w.id)),
            "window_active" => Some(b(sess.zip(t.idx).is_some_and(|(s, i)| s.active == i))),
            "window_last_flag" => Some(b(sess.zip(t.idx).is_some_and(|(s, i)| s.last.first() == Some(&i)))),
            // window_flags doubles the # so it survives being drawn as a style string
            "window_flags" => t.sid.zip(t.idx).map(|(s, i)| srv.window_flags(s, i).replace('#', "##")),
            "window_raw_flags" => t.sid.zip(t.idx).map(|(s, i)| srv.window_flags(s, i)),
            "window_panes" => win.map(|w| w.panes().len().to_string()),
            "window_zoomed_flag" => win.map(|w| b(w.zoomed)),
            "window_width" => win.map(|w| w.sx.to_string()),
            "window_height" => win.map(|w| w.sy.to_string()),
            "window_layout" => win.map(|w| w.layout.to_layout_string(srv.window_area(w.id))),
            // a zoomed window shows only its zoomed pane (layout_root while zoomed)
            "window_visible_layout" => win.map(|w| {
                let area = srv.window_area(w.id);
                if w.zoomed { crate::layout::Node::Leaf(w.active).to_layout_string(area) } else { w.layout.to_layout_string(area) }
            }),
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
            // windows always fit their clients here (no offset)
            "window_bigger" => client.map(|_| "0".into()),
            "window_active_clients" | "window_active_clients_list" => win.map(|w| {
                let names: Vec<String> = srv.clients.values()
                    .filter(|c| c.attached && c.session.and_then(|s| srv.sessions.get(&s)).is_some_and(|s| s.links.get(&s.active) == Some(&w.id)))
                    .map(|c| c.name()).collect();
                if name == "window_active_clients" { names.len().to_string() } else { names.join(",") }
            }).filter(|v| !v.is_empty()),
            "window_active_sessions" | "window_active_sessions_list" => win.map(|w| {
                let names: Vec<String> = srv.links_of(w.id).into_iter()
                    .filter(|(s, i)| srv.sessions[s].active == *i).map(|(s, _)| srv.sessions[&s].name.clone()).collect();
                if name == "window_active_sessions" { names.len().to_string() } else { names.join(",") }
            }).filter(|v| !v.is_empty()),
            "window_linked_sessions_list" => win.map(|w| srv.links_of(w.id).into_iter().map(|(s, _)| srv.sessions[&s].name.clone()).collect::<Vec<_>>().join(",")).filter(|v| !v.is_empty()),
            "window_stack_index" => sess.zip(t.idx).map(|(s, i)| s.last.iter().position(|x| *x == i).map_or(0, |n| n + 1).to_string()),
            // tmux's default cell size when the terminal does not report pixels
            "window_cell_width" => win.map(|_| "16".into()),
            "window_cell_height" => win.map(|_| "32".into()),
            "window_manual_width" | "window_manual_height" => win.map(|w| {
                if srv.opt_window(Some(w.id), "window-size") != "manual" { String::new() }
                else if name == "window_manual_width" { w.sx.to_string() } else { w.sy.to_string() }
            }),
            // no floating panes yet, so never a modal one
            "window_modal_pane" => None,
            // only set while the client shows part of a larger window, which never happens here
            "window_offset_x" | "window_offset_y" => None,
            "window_format" => Some(b(ftype == "W")),
            // pane
            "pane_index" => win.zip(t.pane).and_then(|(w, p)| {
                let base = srv.opt_window_num(Some(w.id), "pane-base-index").max(0) as usize;
                w.panes().iter().position(|x| *x == p).map(|i| (i + base).to_string())
            }),
            "pane_id" => t.pane.map(|p| format!("%{p}")),
            "pane_title" => pane.map(|p| p.title.clone()),
            "pane_current_path" => pane.map(|p| p.current_path.clone()),
            "pane_current_command" => pane.map(|p| p.current_cmd.clone()),
            // cmd_stringify_argv: each argument escaped as args_print does
            "pane_start_command" => pane.map(|p| p.track.argv.iter().map(|a| options::escape_arg(a)).collect::<Vec<_>>().join(" ")),
            "pane_start_path" => pane.map(|p| p.start_path.clone()),
            "pane_pid" => pane.and_then(|p| p.pid).map(|p| p.to_string()),
            // the pty's device on Unix; a ConPTY has no name
            #[cfg(unix)]
            "pane_tty" => pane.map(|p| p.master.tty_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            #[cfg(not(unix))]
            "pane_tty" => pane.map(|_| String::new()),
            "pane_width" => pane.map(|p| p.size.0.to_string()),
            "pane_height" => pane.map(|p| p.size.1.to_string()),
            "pane_active" => Some(b(win.zip(t.pane).is_some_and(|(w, p)| w.active == p))),
            "pane_last" => Some(b(win.zip(t.pane).is_some_and(|(w, p)| w.last_active == Some(p)))),
            "pane_marked" => Some(b(t.pane.is_some() && srv.marked == t.pane)),
            "pane_marked_set" => Some(b(srv.marked.is_some())),
            "pane_dead" => pane.map(|p| b(p.dead.is_some())),
            "pane_dead_status" => pane.and_then(|p| p.dead).and_then(|d| d.0).filter(|s| super::exit_signal(*s).is_none()).map(|s| s.to_string()),
            "pane_dead_time" => pane.and_then(|p| p.dead).map(|d| d.1.to_string()),
            // only a pane ended by a signal has one (sig2name: the number here)
            "pane_dead_signal" => pane.and_then(|p| p.dead).and_then(|d| d.0).and_then(super::exit_signal).map(|s| s.to_string()),
            // format_cb_pane_in_mode: the number of modes on the pane's stack
            "pane_in_mode" => pane.map(|p| (p.mode.iter().count() + p.mode_stack.len()).to_string()),
            "pane_mode" => pane.map(pane_mode_name).filter(|m| !m.is_empty()).map(String::from),
            "pane_synchronized" => t.pane.map(|p| b(options::is_on(&srv.opt_pane(p, "synchronize-panes")))),
            "pane_format" => Some(b(ftype == "P")),
            "pane_created" => pane.map(|p| p.created.to_string()),
            "pane_flags" => win.zip(pane).map(|(w, p)| {
                let mut f = String::new();
                if w.active == p.id { f.push('*'); }
                if w.last_active == Some(p.id) { f.push('-'); }
                if w.zoomed && w.active == p.id { f.push('Z'); }
                f
            }),
            "pane_zoomed_flag" => win.zip(pane).map(|(w, p)| b(w.zoomed && w.active == p.id)),
            // floating panes are not implemented: every pane is tiled, none is modal
            "pane_floating_flag" | "pane_modal_flag" => pane.map(|_| "0".into()),
            "pane_z" => pane.map(|_| "1".into()),
            "pane_input_off" => pane.map(|p| b(p.options.get("@input-disabled").is_some_and(options::is_on))),
            "pane_unseen_changes" => pane.map(|p| b(p.track.unseen_changes && p.mode.is_some())),
            "pane_last_output_time" => pane.map(|p| p.track.last_output_time).filter(|t| *t != 0).map(|t| t.to_string()),
            "pane_output_generation" => pane.map(|p| p.track.output_generation.to_string()),
            "pane_path" => pane.map(|p| p.parser.callbacks().osc.path.clone()),
            "pane_pb_progress" => pane.map(|p| p.parser.callbacks().osc.pb_progress.to_string()),
            "pane_pb_state" => pane.map(|p| ["hidden", "normal", "error", "indeterminate", "paused"][p.parser.callbacks().osc.pb_state.min(4) as usize].into()),
            "pane_last_prompt_time" => pane.map(|p| p.parser.callbacks().osc.prompt_time).filter(|t| *t != 0).map(|t| t.to_string()),
            "pane_command_start_time" => pane.map(|p| p.parser.callbacks().osc.cmd_start).filter(|t| *t != 0).map(|t| t.to_string()),
            "pane_command_end_time" => pane.map(|p| p.parser.callbacks().osc.cmd_end).filter(|t| *t != 0).map(|t| t.to_string()),
            "pane_command_running" => pane.map(|p| b(p.parser.callbacks().osc.cmd_running)),
            "pane_command_status" => pane.and_then(|p| p.parser.callbacks().osc.cmd_status).map(|s| s.to_string()),
            "pane_command_duration" => pane.and_then(|p| {
                let o = &p.parser.callbacks().osc;
                if o.cmd_start == 0 { return None; }
                let end = if o.cmd_running { crate::server::now_secs() } else { o.cmd_end };
                Some(end.saturating_sub(o.cmd_start).to_string())
            }),
            "pane_pipe" => pane.map(|p| b(p.pipe.is_some())),
            "pane_pipe_pid" => pane.filter(|p| p.pipe.is_some()).and_then(|p| p.track.pipe_pid).map(|p| p.to_string()),
            "pane_search_string" => pane.map(|p| match &p.mode {
                Some(PaneMode::Copy(c)) if c.last_search().is_some() => c.last_search().unwrap_or_default().to_string(),
                _ => p.track.search.clone(),
            }),
            "pane_start_command_list" => pane.map(|p| p.track.argv.iter().map(|a| format!("'{}'", a.replace('\'', "'\\''"))).collect::<Vec<_>>().join(" ")),
            "pane_fg" | "pane_bg" => win.and(t.pane).map(|p| {
                use super::theme::StyleColour;
                // tty_default_colours: window-active-style for the active pane, then
                // pane palette, then window-style; inactive panes prefer window-style.
                let (sfg, sbg) = srv.pane_style_colours(p);
                let palette = srv.panes.get(&p).map(|pp| &pp.parser.callbacks().colours);
                let pick = |st: StyleColour, own: Option<Color>| if st == StyleColour::Default { own.map_or(st, StyleColour::Colour) } else { st };
                if name == "pane_fg" { pick(sfg, palette.and_then(|c| c.fg)) } else { pick(sbg, palette.and_then(|c| c.bg)) }.name()
            }),
            "pane_left" | "pane_top" | "pane_right" | "pane_bottom" | "pane_at_left" | "pane_at_right" | "pane_at_top" | "pane_at_bottom" | "pane_x" | "pane_y"
            | "pane_unzoomed_width" | "pane_unzoomed_height" => {
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
                let c = srv.pane_content_rect(w.id, p, r);
                Some(match name {
                    "pane_left" | "pane_x" => c.x.to_string(),
                    "pane_top" | "pane_y" => c.y.to_string(),
                    "pane_unzoomed_width" | "pane_unzoomed_height" => {
                        // the pane's cell in the layout without zoom
                        let (mut panes, mut seps) = (Vec::new(), Vec::new());
                        w.layout.layout(area, &mut panes, &mut seps);
                        let u = srv.content_rect(w.id, panes.into_iter().find(|(id, _)| *id == p)?.1);
                        if name == "pane_unzoomed_width" { u.w } else { u.h }.to_string()
                    }
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
            // copy mode (window_copy_formats)
            _ if super::copy::is_copy_var(name) => t.pane.and_then(|p| self.srv.copy_var(p, name)),
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
            "insert_flag" => pane.map(|p| b(p.screen().insert_mode())),
            "origin_flag" => pane.map(|p| b(p.screen().origin_mode())),
            "wrap_flag" => pane.map(|p| b(p.screen().wrap_mode())),
            "mouse_utf8_flag" => pane.map(|p| b(p.screen().mouse_encodings().0)),
            "bracket_paste_flag" => pane.map(|p| b(p.screen().bracketed_paste())),
            "synchronized_output_flag" => pane.map(|p| b(p.track.sync_since.is_some_and(|t| t.elapsed() < Duration::from_secs(1)))),
            "alternate_saved_x" | "alternate_saved_y" => pane.map(|p| {
                // tmux shows UINT_MAX until the alternate screen saved a cursor
                p.screen().alternate_saved_cursor().map_or(u32::MAX.to_string(), |(y, x)| if name == "alternate_saved_x" { x } else { y }.to_string())
            }),
            "scroll_region_upper" => pane.map(|p| p.screen().scroll_region().0.to_string()),
            "scroll_region_lower" => pane.map(|p| p.screen().scroll_region().1.to_string()),
            "cursor_blinking" => pane.map(|p| b(p.screen().cursor_blinking().0)),
            "cursor_very_visible" => pane.map(|p| b(p.screen().cursor_very_visible())),
            "cursor_shape" => pane.map(|p| match p.screen().cursor_style() {
                vt100::CursorStyle::BlinkBlock | vt100::CursorStyle::SteadyBlock => "block",
                vt100::CursorStyle::BlinkUnderline | vt100::CursorStyle::SteadyUnderline => "underline",
                vt100::CursorStyle::BlinkVerticalBar | vt100::CursorStyle::SteadyVerticalBar => "bar",
                vt100::CursorStyle::Default => "default",
            }.into()),
            "cursor_colour" => pane.map(|p| match &p.parser.callbacks().osc.cursor_colour {
                Some(c) => c.clone(),
                None => {
                    // the cursor-colour option, "none" when unset
                    let o = srv.opt_pane(p.id, "cursor-colour");
                    if let Some(i) = crate::format::theme_index(&o) { return crate::format::theme_name(i).into(); }
                    if o.is_empty() || o == "none" { "none".into() } else { super::theme::style_colours(&format!("fg={o}"), self).0.name() }
                }
            }),
            "cursor_character" => pane.and_then(|p| {
                let (y, x) = p.screen().cursor_position();
                match p.screen().cell(y, x) {
                    Some(c) if c.is_wide_continuation() => None,
                    Some(c) if c.has_contents() => Some(c.contents().to_string()),
                    _ => Some(" ".into()),
                }
            }),
            "pane_private_modes" => pane.map(|p| private_modes(p.screen())),
            "pane_tabs" => pane.map(|p| p.screen().tab_stops().iter().map(|t| t.to_string()).collect::<Vec<_>>().join(",")).filter(|v| !v.is_empty()),
            "pane_key_mode" => pane.map(|p| {
                // the pane's own mode (format_cb_pane_key_mode)
                match p.parser.callbacks().ext_keys {
                    2 => "Ext 2",
                    1 => "Ext 1",
                    _ => "VT10x",
                }.into()
            }),
            "history_added" => pane.map(|p| p.screen().history_counters().0.to_string()),
            "history_collected" => pane.map(|p| p.screen().history_counters().1.to_string()),
            "history_generation" => pane.map(|p| p.screen().history_counters().2.to_string()),
            "history_bytes" => pane.map(|p| {
                let (lines, cells, ext) = history_usage(p.screen());
                (lines * GRID_LINE + cells * GRID_CELL + ext * GRID_EXTD).to_string()
            }),
            "history_all_bytes" => pane.map(|p| {
                let (lines, cells, ext) = history_usage(p.screen());
                format!("{lines},{},{cells},{},{ext},{}", lines * GRID_LINE, cells * GRID_CELL, ext * GRID_EXTD)
            }),
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
            "client_utf8" => client.map(|c| if c.utf8() { "1" } else { "0" }.into()),
            "client_control_mode" => client.map(|c| b(c.control > 0)),
            "client_written" => client.map(|c| c.io.written.load(std::sync::atomic::Ordering::Relaxed).to_string()),
            "client_discarded" => client.map(|c| c.io.discarded.load(std::sync::atomic::Ordering::Relaxed).to_string()),
            "client_uid" => client.and_then(|c| c.user.clone()),
            // terminals here do not report a cell size: tmux's defaults
            "client_cell_width" => client.filter(|c| c.attached && c.control == 0).map(|_| "16".into()),
            "client_cell_height" => client.filter(|c| c.attached && c.control == 0).map(|_| "32".into()),
            "client_colours" => client.filter(|c| c.attached && c.control == 0).map(|c| {
                c.features.colours().to_string()
            }),
            "client_theme" => client.and_then(|c| c.terminal.reported).map(|t| if t == format::Theme::Light { "light" } else { "dark" }.into()),
            // server_client_get_flags
            "client_flags" => client.map(|c| {
                let pause = c.ctl.pause_after.map(|d| format!("pause-after={}", d.as_secs()));
                [
                    (c.attached, "attached"),
                    (c.focused, "focused"),
                    (c.control > 0, "control-mode"),
                    (c.ctl.ignore_size, "ignore-size"),
                    (c.ctl.no_detach_on_destroy, "no-detach-on-destroy"),
                    (c.ctl.no_output, "no-output"),
                    (c.ctl.wait_exit, "wait-exit"),
                    (c.ctl.new_layouts, "new-layouts"),
                    (pause.is_some(), pause.as_deref().unwrap_or("")),
                    (c.is_readonly(), "read-only"),
                    (c.suspended, "suspended"),
                    (true, "UTF-8"),
                ]
                .iter()
                .filter(|(on, _)| *on)
                .map(|(_, n)| *n)
                .collect::<Vec<_>>()
                .join(",")
            }),
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
            let level = match flags { "" | "s" => self.t.sid.unwrap_or(u32::MAX), "g" => super::GLOBAL_ENV, _ => u32::MAX };
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
                ("environ_hidden".into(), b(srv.env_hidden.contains(&(level, name.clone())))),
                ("environ_name".into(), name),
                ("environ_removed".into(), b(value.is_none())),
                ("environ_value".into(), value.unwrap_or_default()),
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
        let mut items: Vec<(String, Option<String>, String)> = opts.0.iter().map(|(key, value)| {
            match key.split_once('[') {
                Some((name, index)) => (name.to_string(), Some(index.trim_end_matches(']').to_string()), value.clone()),
                None => (key.clone(), None, value.clone()),
            }
        }).collect();
        // Hooks are array options of their level (already among the options); the
        // global levels always hold every table hook of their scope, empty by default.
        if global && !flags.contains('v') && !flags.contains('p') {
            let window = flags.contains('w');
            for (name, scope) in options::HOOKS {
                if (*scope == options::Scope::Session) != window && !items.iter().any(|o| o.0 == *name) {
                    items.push((name.to_string(), None, String::new()));
                }
            }
        }
        // an array with items is shown by its items only
        let with_items: std::collections::HashSet<String> = items.iter().filter(|o| o.1.is_some()).map(|o| o.0.clone()).collect();
        items.retain(|o| o.1.is_some() || !with_items.contains(&o.0));
        let index_key = |i: &Option<String>| i.as_ref().map(|i| (i.parse::<u64>().unwrap_or(u64::MAX), i.clone()));
        items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| index_key(&a.1).cmp(&index_key(&b.1))));
        items.iter().enumerate().map(|(i, (name, index, value))| {
            let is_hook = options::hook_scope(name).is_some();
            let is_array = index.is_some() || is_hook || options::def(name).is_some_and(|d| d.kind == options::Kind::Array);
            let count = items.iter().filter(|o| &o.0 == name && o.1.is_some()).count();
            let first = index.is_none() || i == 0 || &items[i - 1].0 != name;
            let last = index.is_none() || items.get(i + 1).is_none_or(|o| &o.0 != name);
            HashMap::from([
                ("option_name".into(), name.clone()),
                ("option_value".into(), value.clone()),
                ("option_is_array".into(), b(is_array)),
                ("option_array_key".into(), index.clone().unwrap_or_default()),
                ("option_array_index".into(), index.clone().unwrap_or_default()),
                ("option_array_count".into(), count.to_string()),
                ("option_array_first".into(), b(is_array && first)),
                ("option_array_last".into(), b(is_array && last)),
                ("option_is_hook".into(), b(is_hook)),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> (Server, mpsc::Receiver<Event>) {
        let (tx, events) = mpsc::sync_channel(64);
        (Server::new(tx), events)
    }

    fn run(srv: &mut Server, args: &[&str]) {
        srv.exec(args.iter().map(|s| s.to_string()).collect(), &mut Ctx::default()).unwrap();
    }

    fn pane_of(srv: &Server, sid: &str, idx: u32) -> PaneId {
        let s = srv.sessions.values().find(|s| s.name == sid).unwrap();
        srv.windows[&s.links[&idx]].active
    }

    /// Feeds output to a pane as if its program wrote it.
    fn output(srv: &mut Server, pane: PaneId, data: &[u8]) {
        let generation = srv.panes[&pane].generation;
        srv.pane_data(pane, generation, data);
    }

    const MODES: &str = "#{insert_flag} #{origin_flag} #{wrap_flag} #{cursor_blinking} #{cursor_very_visible} #{cursor_shape}|#{pane_private_modes}|#{pane_tabs}|#{scroll_region_upper},#{scroll_region_lower}|#{alternate_on} #{alternate_saved_x},#{alternate_saved_y}|#{cursor_colour}|#{pane_pb_state} #{pane_pb_progress}|#{pane_path}|#{pane_command_running} #{pane_command_status} #{?pane_command_start_time,S,} #{?pane_last_prompt_time,P,}|#{mouse_utf8_flag} #{mouse_sgr_flag} #{bracket_paste_flag} #{synchronized_output_flag}|#{cursor_character}";

    // Expected values are what tmux 3.8 prints for the same output.
    #[test]
    fn terminal_modes_and_osc_state() {
        let (mut srv, _events) = server();
        run(&mut srv, &["new-session", "-d", "-s", "m", "-x", "80", "-y", "24"]);
        let p = pane_of(&srv, "m", 0);
        // reset whatever the shell has sent so far
        srv.panes.get_mut(&p).unwrap().parser.process(b"\x1bc");
        assert_eq!(srv.expand_for_pane(p, MODES), "0 0 1 0 0 default|7,25|8,16,24,32,40,48,56,64,72|0,23|0 4294967295,4294967295|none|hidden 0||0   |0 0 0 0| ");
        output(&mut srv, p, b"\x1b[4h\x1b[?6h\x1b[?7l\x1b[?12h\x1b[34l\x1b[5 q\x1b[?1004h\x1b[?1005h\x1b[?1006h\x1b[?1002h\x1b[?2004h\x1b[?2031h\x1b[3g\x1b[1;5H\x1bH\x1b[1;20H\x1bH\x1b[3;10r\x1b]12;rgb:ff/00/80\x07\x1b]9;4;1;42\x07\x1b]7;file://host/tmp/x\x07\x1b]133;A\x07\x1b]133;C\x07\x1b[?6l\x1b[2;3HQ\x1b[2;3H");
        assert_eq!(srv.expand_for_pane(p, MODES), "1 0 0 1 1 bar|12,25,1002,1004,1005,1006,2004,2031|4,19|2,9|0 4294967295,4294967295|#ff0080|normal 42|file://host/tmp/x|1  S P|1 1 1 0|Q");
        // RIS keeps the cursor shape (screen_write_reset resets only the mode bits)
        output(&mut srv, p, b"\x1bc\x1b[2;5Hab\x1b[?1049hcd\x1b]133;D;7\x07\x1b]9;4;3\x07\x1b]112\x07\x1b[?2026h");
        assert_eq!(srv.expand_for_pane(p, MODES), "0 0 1 0 0 bar|7,25,2026|8,16,24,32,40,48,56,64,72|0,23|1 6,1|none|indeterminate 42|file://host/tmp/x|0 7 S P|0 0 0 1| ");
        output(&mut srv, p, b"\x1b[?1049l\x1b[?2026l\x1b]133;C\x07\x1b]133;D;abc\x07\x1b]9;4;2;5\x07\x1b]12;#00ff00\x07\x1b[0 q\x1b[?12l");
        assert_eq!(srv.expand_for_pane(p, MODES), "0 0 1 0 0 default|7,25|8,16,24,32,40,48,56,64,72|0,23|0 6,1|#00ff00|error 5|file://host/tmp/x|0 255 S P|0 0 0 0| ");
        // tabs set by HTS move the cursor
        output(&mut srv, p, b"\x1bc\x1b[3g\x1b[1;11H\x1bH\x1b[1;1H\tX");
        assert_eq!(srv.expand_for_pane(p, "#{cursor_x} #{pane_tabs}"), "11 10");
        // the command duration counts from C to D
        assert!(srv.expand_for_pane(p, "#{pane_command_duration}").parse::<u64>().unwrap() <= 1);
        assert!(srv.expand_for_pane(p, "#{pane_last_output_time}").parse::<u64>().unwrap() > 0);
        assert!(srv.expand_for_pane(p, "#{pane_output_generation}").parse::<u64>().unwrap() >= 4);
        srv.shutdown();
    }

    #[test]
    fn key_mode_follows_extended_keys() {
        let (mut srv, _events) = server();
        run(&mut srv, &["new-session", "-d", "-s", "k"]);
        let p = pane_of(&srv, "k", 0);
        output(&mut srv, p, b"\x1b[>4;2m");
        // extended-keys off: the request is ignored
        assert_eq!(srv.expand_for_pane(p, "#{pane_key_mode}"), "VT10x");
        // the mode is the pane's: turning the option on later does not apply the old request
        run(&mut srv, &["set", "-g", "extended-keys", "on"]);
        assert_eq!(srv.expand_for_pane(p, "#{pane_key_mode}"), "VT10x");
        output(&mut srv, p, b"\x1b[>4;2m");
        assert_eq!(srv.expand_for_pane(p, "#{pane_key_mode}"), "Ext 2");
        output(&mut srv, p, b"\x1b[>4n");
        assert_eq!(srv.expand_for_pane(p, "#{pane_key_mode}"), "VT10x");
        // always: mode 1 from the next reset, and CSI > 4 n cannot go below it
        run(&mut srv, &["set", "-g", "extended-keys", "always"]);
        assert_eq!(srv.expand_for_pane(p, "#{pane_key_mode}"), "VT10x");
        output(&mut srv, p, b"\x1bc");
        assert_eq!(srv.expand_for_pane(p, "#{pane_key_mode}"), "Ext 1");
        output(&mut srv, p, b"\x1b[>4;2m\x1b[>4n");
        assert_eq!(srv.expand_for_pane(p, "#{pane_key_mode}"), "Ext 1");
        srv.shutdown();
    }

    #[test]
    fn history_counters_and_sizes() {
        let (mut srv, _events) = server();
        run(&mut srv, &["new-session", "-d", "-s", "h", "-x", "80", "-y", "24"]);
        let p = pane_of(&srv, "h", 0);
        srv.panes.get_mut(&p).unwrap().parser.process(b"\x1bc\x1b[H\x1b[2J");
        srv.panes.get_mut(&p).unwrap().parser.screen_mut().clear_scrollback();
        let base = srv.expand_for_pane(p, "#{history_added} #{history_collected} #{history_generation}");
        let n: Vec<u64> = base.split(' ').map(|v| v.parse().unwrap()).collect();
        let text: String = (1..=60).map(|i| format!("{i}\r\n")).collect();
        output(&mut srv, p, text.as_bytes());
        assert_eq!(srv.expand_for_pane(p, "#{history_size} #{history_added}"), format!("37 {}", n[0] + 37));
        // 61 lines of 40 bytes; 60 lines with text use a quarter of the width (20 cells)
        assert_eq!(srv.expand_for_pane(p, "#{history_bytes}|#{history_all_bytes}"), "8440|61,2440,1200,6000,0,0");
        run(&mut srv, &["clear-history", "-t", "h"]);
        assert_eq!(srv.expand_for_pane(p, "#{history_generation}"), (n[2] + 1).to_string());
        srv.shutdown();
    }

    #[test]
    fn session_and_window_lists() {
        let (mut srv, _events) = server();
        run(&mut srv, &["new-session", "-d", "-s", "s1", "-x", "80", "-y", "24"]);
        run(&mut srv, &["new-window", "-d", "-t", "s1:3"]);
        run(&mut srv, &["new-window", "-d", "-t", "s1:5"]);
        for w in ["s1:3", "s1:5", "s1:0"] {
            run(&mut srv, &["select-window", "-t", w]);
        }
        run(&mut srv, &["split-window", "-d", "-h", "-t", "s1:0"]);
        run(&mut srv, &["new-session", "-d", "-s", "s2", "-x", "80", "-y", "24"]);
        run(&mut srv, &["link-window", "-s", "s1:5", "-t", "s2:7"]);
        run(&mut srv, &["select-window", "-t", "s2:7"]);
        let f = "#{active_window_index} #{last_window_index} #{session_stack} #{window_stack_index} #{window_active_sessions}:#{window_active_sessions_list} #{window_linked_sessions_list}";
        let t = |srv: &Server, s: &str, i: u32| {
            let sid = srv.sessions.values().find(|x| x.name == s).unwrap().id;
            let wid = srv.sessions[&sid].links[&i];
            Target { sid: Some(sid), idx: Some(i), wid: Some(wid), pane: Some(srv.windows[&wid].active) }
        };
        assert_eq!(srv.expand(t(&srv, "s1", 0), None, f), "0 5 0,5,3 0 1:s1 s1");
        assert_eq!(srv.expand(t(&srv, "s1", 3), None, f), "0 5 0,5,3 2 0: s1");
        assert_eq!(srv.expand(t(&srv, "s1", 5), None, f), "0 5 0,5,3 1 1:s2 s1,s2");
        assert_eq!(srv.expand(t(&srv, "s2", 7), None, f), "7 7 7,0 0 1:s2 s1,s2");
        // zoom: the zoomed pane is flagged, the other keeps its unzoomed size
        let w0 = t(&srv, "s1", 0);
        let panes = srv.windows[&w0.wid.unwrap()].panes();
        run(&mut srv, &["resize-pane", "-Z", "-t", &format!("%{}", panes[0])]);
        let pf = "#{pane_flags} #{pane_zoomed_flag} #{pane_unzoomed_width}x#{pane_unzoomed_height} #{pane_x},#{pane_y} #{pane_floating_flag} #{pane_z}";
        assert_eq!(srv.expand_for_pane(panes[0], pf), "*Z 1 40x24 0,0 0 1");
        assert_eq!(srv.expand_for_pane(panes[1], pf), " 0 39x24 41,0 0 1");
        assert_eq!(srv.expand(w0, None, "#{window_flags}|#{window_raw_flags}|#{window_manual_width}"), "*Z|*Z|");
        run(&mut srv, &["resize-window", "-t", "s1:3", "-x", "50", "-y", "20"]);
        assert_eq!(srv.expand(t(&srv, "s1", 3), None, "#{window_manual_width}x#{window_manual_height}"), "50x20");
        srv.shutdown();
    }

    #[test]
    fn alerts_flags_and_marks() {
        let (mut srv, _events) = server();
        run(&mut srv, &["new-session", "-d", "-s", "a"]);
        run(&mut srv, &["new-window", "-d", "-t", "a:3"]);
        run(&mut srv, &["new-window", "-d", "-t", "a:4"]);
        let s = srv.sessions.values_mut().next().unwrap();
        s.alerts.insert(3, ALERT_ACTIVITY | ALERT_BELL);
        s.alerts.insert(4, ALERT_ACTIVITY);
        let sid = s.id;
        let tgt = |i: u32, srv: &Server| Target { sid: Some(sid), idx: Some(i), wid: Some(srv.sessions[&sid].links[&i]), pane: None };
        assert_eq!(srv.expand(tgt(3, &srv), None, "#{window_flags} #{window_raw_flags} #{session_alert} #{session_alerts} #{session_activity_flag}#{session_bell_flag}#{session_silence_flag}"),
            "##! #! #! 3#!,4# 110");
        let p = srv.windows[&srv.sessions[&sid].links[&4]].active;
        srv.marked = Some(p);
        assert_eq!(srv.expand(tgt(4, &srv), None, "#{window_flags} #{session_marked}"), "##M 1");
        srv.shutdown();
    }

    #[test]
    fn start_command_list_quotes_each_argument() {
        let (mut srv, _events) = server();
        run(&mut srv, &["new-session", "-d", "-s", "c"]);
        let p = pane_of(&srv, "c", 0);
        assert_eq!(srv.expand_for_pane(p, "[#{pane_start_command_list}]"), "[]");
        srv.panes.get_mut(&p).unwrap().track.argv = vec!["sleep 100".into(), "it's".into()];
        assert_eq!(srv.expand_for_pane(p, "#{pane_start_command_list}"), r"'sleep 100' 'it'\''s'");
        srv.shutdown();
    }

    #[test]
    fn colours_print_like_tmux() {
        assert_eq!(colour_name(Color::Default), "default");
        assert_eq!(colour_name(Color::Idx(1)), "red");
        assert_eq!(colour_name(Color::Idx(9)), "brightred");
        assert_eq!(colour_name(Color::Idx(200)), "colour200");
        assert_eq!(colour_name(Color::Rgb(1, 2, 255)), "#0102ff");
        assert_eq!(parse_x11_colour("rgb:ffff/0000/8080").as_deref(), Some("#ff0080"));
        assert_eq!(parse_x11_colour("#102030").as_deref(), Some("#102030"));
        assert_eq!(parse_x11_colour("1,2,3").as_deref(), Some("#010203"));
        assert_eq!(parse_x11_colour("rgb:f/0/0"), None);
        assert_eq!(super::super::osc133_exit_status("D"), 0);
        assert_eq!(super::super::osc133_exit_status("D;3;aid=1"), 3);
        assert_eq!(super::super::osc133_exit_status("D;err=x"), 0);
        assert_eq!(super::super::osc133_exit_status("D;256"), 255);
        assert_eq!(super::super::osc133_exit_status("D; 3"), 3);
        assert_eq!(super::super::osc133_exit_status("D;3 "), 255);
    }

    #[test]
    fn style_colours_of_pane() {
        let (mut srv, _events) = server();
        run(&mut srv, &["new-session", "-d", "-s", "st"]);
        run(&mut srv, &["split-window", "-d", "-t", "st"]);
        let w = srv.windows.values().next().unwrap();
        let (active, other) = (w.active, w.panes().into_iter().find(|p| *p != w.active).unwrap());
        assert_eq!(srv.expand_for_pane(active, "#{pane_fg} #{pane_bg}"), "default default");
        run(&mut srv, &["set", "-g", "window-style", "fg=colour100,bg=#102030"]);
        run(&mut srv, &["set", "-g", "window-active-style", "bg=blue"]);
        assert_eq!(srv.expand_for_pane(active, "#{pane_fg} #{pane_bg}"), "colour100 blue");
        assert_eq!(srv.expand_for_pane(other, "#{pane_fg} #{pane_bg}"), "colour100 #102030");
        // tty_default_colours: the window styles win over a pane's own OSC 10/11 colours
        output(&mut srv, other, b"\x1b]10;#ff0000\x07\x1b]11;#00ff00\x07");
        assert_eq!(srv.expand_for_pane(other, "#{pane_fg} #{pane_bg}"), "colour100 #102030");
        run(&mut srv, &["set", "-g", "window-style", "bg=themecyan"]);
        assert_eq!(srv.expand_for_pane(other, "#{pane_fg} #{pane_bg}"), "#ff0000 themecyan");
        run(&mut srv, &["set", "-gu", "window-style"]);
        assert_eq!(srv.expand_for_pane(other, "#{pane_fg} #{pane_bg}"), "#ff0000 #00ff00");
        srv.shutdown();
    }
}
