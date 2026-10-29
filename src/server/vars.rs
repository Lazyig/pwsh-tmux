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
        if self.marked.is_some_and(|m| w.layout.panes().contains(&m)) {
            f.push('M');
        }
        if w.zoomed {
            f.push('Z');
        }
        f
    }
}

impl Vars for VarCtx<'_> {
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
            "version" => Some(env!("CARGO_PKG_VERSION").into()),
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
            "session_group" | "session_grouped" | "session_group_size" => Some(String::new()),
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
            "window_panes" => win.map(|w| w.layout.panes().len().to_string()),
            "window_zoomed_flag" => win.map(|w| b(w.zoomed)),
            "window_width" => win.map(|w| w.sx.to_string()),
            "window_height" => win.map(|w| w.sy.to_string()),
            "window_layout" | "window_visible_layout" => win.map(|w| w.layout.to_layout_string(srv.window_area(w.id))),
            "window_activity" => win.map(|w| w.activity.to_string()),
            "window_created" => win.map(|w| w.created.to_string()),
            "window_activity_flag" => Some(b(sess.zip(t.idx).is_some_and(|(s, i)| s.alerts.get(&i).is_some_and(|a| a & ALERT_ACTIVITY != 0)))),
            "window_bell_flag" => Some(b(sess.zip(t.idx).is_some_and(|(s, i)| s.alerts.get(&i).is_some_and(|a| a & ALERT_BELL != 0)))),
            "window_silence_flag" => Some(b(sess.zip(t.idx).is_some_and(|(s, i)| s.alerts.get(&i).is_some_and(|a| a & ALERT_SILENCE != 0)))),
            "window_marked_flag" => Some(b(win.is_some_and(|w| srv.marked.is_some_and(|m| w.layout.panes().contains(&m))))),
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
                w.layout.panes().iter().position(|x| *x == p).map(|i| (i + base).to_string())
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
                let (rects, _) = srv.rects(w.id);
                let r = rects.iter().find(|(id, _)| *id == p)?.1;
                let area = srv.window_area(w.id);
                Some(match name {
                    "pane_left" => r.x.to_string(),
                    "pane_top" => r.y.to_string(),
                    "pane_right" => (r.right().saturating_sub(1)).to_string(),
                    "pane_bottom" => (r.bottom().saturating_sub(1)).to_string(),
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
            "selection_present" => pane.map(|p| b(matches!(&p.mode, Some(PaneMode::Copy(c)) if c.selection_active()))),
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
            "client_readonly" => client.map(|c| b(c.readonly)),
            "client_termname" | "client_termtype" => client.and_then(|c| c.ident.as_ref()).map(|i| i.term.clone()),
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
            if let Some(p) = t.pane {
                if let Some(v) = srv.panes.get(&p).and_then(|p| p.options.get(name)) {
                    return Some(v.to_string());
                }
            }
            if let Some(v) = win.and_then(|w| w.options.get(name)) {
                return Some(v.to_string());
            }
            if let Some(v) = sess.and_then(|s| s.options.get(name)) {
                return Some(v.to_string());
            }
            return srv.opts_window.get(name).or(srv.opts_session.get(name)).or(srv.opts_server.get(name)).map(String::from);
        }
        None
    }
}
