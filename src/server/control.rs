//! Control mode (`-C` / `-CC`): command replies in %begin/%end blocks, pane output as
//! %output, and notifications derived by diffing what each control client has seen.

use super::*;

/// What a control client was last told about.
#[derive(Default, Clone, PartialEq)]
pub struct Seen {
    pub session: Option<(SessId, String)>,
    pub sessions: BTreeMap<SessId, String>,
    /// window -> (name, layout) for windows in the client's session
    pub windows: BTreeMap<WinId, (String, String)>,
    pub active_window: Option<WinId>,
    pub active_pane: BTreeMap<WinId, PaneId>,
    pub pane_modes: BTreeMap<PaneId, bool>,
}

/// tmux escapes bytes below space and backslash as \ooo in %output.
pub fn escape_output(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len());
    let mut bytes = Vec::with_capacity(data.len());
    for &b in data {
        if b < b' ' || b == b'\\' {
            out.push_str(&String::from_utf8_lossy(&std::mem::take(&mut bytes)));
            out.push_str(&format!("\\{b:03o}"));
        } else {
            bytes.push(b);
        }
    }
    out.push_str(&String::from_utf8_lossy(&bytes));
    out
}

impl Server {
    pub fn is_control(&self, cid: ClientId) -> bool {
        self.clients.get(&cid).is_some_and(|c| c.control > 0)
    }

    /// Sends a protocol line (or several) to a control client.
    pub fn control_write(&self, cid: ClientId, text: &str) {
        if let Some(c) = self.clients.get(&cid) {
            c.send(S_OUTPUT, text.as_bytes());
        }
    }

    /// A line typed by a control client: an empty line detaches, anything else is a command.
    pub fn control_line(&mut self, cid: ClientId, line: &str) {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            self.detach_client(cid, "");
            return;
        }
        let Some(c) = self.clients.get_mut(&cid) else { return };
        c.control_seq += 1;
        let seq = c.control_seq;
        let pane = self.client_active_pane(cid);
        let ctx = Ctx { client: Some(cid), pane, control_seq: Some(seq), ..Ctx::default() };
        match crate::cmdparse::parse(line) {
            Ok(cmds) => self.run_queue(Queue { cmds: cmds.into(), ctx, soft: 0 }),
            Err(e) => self.control_block(cid, seq, "", Some(&e)),
        }
    }

    pub fn control_block(&self, cid: ClientId, seq: u64, out: &str, err: Option<&str>) {
        let t = now_secs();
        let mut s = format!("%begin {t} {seq} 1\n");
        s.push_str(out);
        if !out.is_empty() && !out.ends_with('\n') {
            s.push('\n');
        }
        match err {
            Some(e) => {
                s.push_str(e.trim_end());
                s.push('\n');
                s.push_str(&format!("%error {t} {seq} 1\n"));
            }
            None => s.push_str(&format!("%end {t} {seq} 1\n")),
        }
        self.control_write(cid, &s);
    }

    /// %output for every control client that can see the pane.
    pub fn control_output(&self, pane: PaneId, data: &[u8]) {
        let controls: Vec<ClientId> = self.clients.values().filter(|c| c.control > 0 && c.attached).map(|c| c.id).collect();
        if controls.is_empty() {
            return;
        }
        let Some(wid) = self.window_of(pane) else { return };
        let sessions: Vec<SessId> = self.links_of(wid).into_iter().map(|(s, _)| s).collect();
        let line = format!("%output %{pane} {}\n", escape_output(data));
        for cid in controls {
            if self.clients[&cid].session.is_some_and(|s| sessions.contains(&s)) {
                self.control_write(cid, &line);
            }
        }
    }

    fn seen_now(&self, cid: ClientId) -> Seen {
        let mut seen = Seen { sessions: self.sessions.values().map(|s| (s.id, s.name.clone())).collect(), ..Seen::default() };
        let Some(sid) = self.client_session(cid) else { return seen };
        let s = &self.sessions[&sid];
        seen.session = Some((sid, s.name.clone()));
        seen.active_window = Some(s.active_window());
        for wid in s.links.values() {
            let w = &self.windows[wid];
            seen.windows.insert(*wid, (w.name.clone(), w.layout.to_layout_string(self.window_area(*wid))));
            seen.active_pane.insert(*wid, w.active);
            for p in w.layout.panes() {
                seen.pane_modes.insert(p, self.panes.get(&p).is_some_and(|x| x.mode.is_some()));
            }
        }
        seen
    }

    /// Emits notifications for whatever changed since each control client was last told.
    pub fn control_notify(&mut self) {
        let ids: Vec<ClientId> = self.clients.values().filter(|c| c.control > 0 && c.attached).map(|c| c.id).collect();
        for cid in ids {
            let now = self.seen_now(cid);
            let before = self.clients[&cid].seen.clone();
            if now == before {
                continue;
            }
            let mut out = String::new();
            if now.session != before.session {
                if let Some((sid, name)) = &now.session {
                    out.push_str(&format!("%session-changed ${sid} {name}\n"));
                }
            } else if let (Some((sid, a)), Some((_, b))) = (&now.session, &before.session) {
                if a != b {
                    out.push_str(&format!("%session-renamed ${sid} {a}\n"));
                }
            }
            if now.sessions.keys().ne(before.sessions.keys()) {
                out.push_str("%sessions-changed\n");
            }
            let same_session = now.session.as_ref().map(|s| s.0) == before.session.as_ref().map(|s| s.0);
            if same_session {
                for (wid, (name, layout)) in &now.windows {
                    match before.windows.get(wid) {
                        None => out.push_str(&format!("%window-add @{wid}\n")),
                        Some((old_name, old_layout)) => {
                            if old_name != name {
                                out.push_str(&format!("%window-renamed @{wid} {name}\n"));
                            }
                            if old_layout != layout {
                                let zoomed = self.windows.get(wid).is_some_and(|w| w.zoomed);
                                out.push_str(&format!("%layout-change @{wid} {layout} {layout} {}\n", if zoomed { "*Z" } else { "*" }));
                            }
                        }
                    }
                }
                for wid in before.windows.keys() {
                    if !now.windows.contains_key(wid) {
                        out.push_str(&format!("%window-close @{wid}\n"));
                    }
                }
                if now.active_window != before.active_window {
                    if let (Some((sid, _)), Some(w)) = (&now.session, now.active_window) {
                        out.push_str(&format!("%session-window-changed ${sid} @{w}\n"));
                    }
                }
                for (wid, p) in &now.active_pane {
                    if before.active_pane.get(wid).is_some_and(|old| old != p) {
                        out.push_str(&format!("%window-pane-changed @{wid} %{p}\n"));
                    }
                }
                for (p, m) in &now.pane_modes {
                    if before.pane_modes.get(p).is_some_and(|old| old != m) {
                        out.push_str(&format!("%pane-mode-changed %{p}\n"));
                    }
                }
            }
            if let Some(c) = self.clients.get_mut(&cid) {
                c.seen = now;
            }
            if !out.is_empty() {
                self.control_write(cid, &out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_escaping() {
        assert_eq!(escape_output(b"a\r\nb\\c\x1b[0m"), "a\\015\\012b\\134c\\033[0m");
        assert_eq!(escape_output("한글".as_bytes()), "한글");
    }
}
