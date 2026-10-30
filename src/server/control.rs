//! Control mode (`-C` / `-CC`): command replies in %begin/%end blocks, pane output as
//! %output, and notifications derived by diffing what each control client has seen.

use super::vars::Target;
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaneOutput {
    On,
    Off,
    Paused,
}

/// refresh-client -f / -A / -B state of a control client.
#[derive(Default)]
pub struct Flags {
    /// pause-after=N: pause a pane whose output is N seconds behind
    pub pause_after: Option<Duration>,
    pub no_output: bool,
    pub wait_exit: bool,
    pub ignore_size: bool,
    pub exiting: std::cell::Cell<bool>,
    pub panes: HashMap<PaneId, PaneOutput>,
    /// name -> (what, format, last value per target)
    pub subs: BTreeMap<String, (String, String, HashMap<String, String>)>,
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
        // wait-exit: after %exit the client is let go by its next line
        if self.clients.get(&cid).is_some_and(|c| c.ctl.exiting.get()) {
            if let Some(c) = self.clients.get(&cid) {
                c.send(S_EXIT, b"");
            }
            return;
        }
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

    /// %output (or %extended-output with pause-after) for every control client that can
    /// see the pane, honouring per-pane on/off/pause and pausing panes that fall behind.
    pub fn control_output(&mut self, pane: PaneId, data: &[u8]) {
        let controls: Vec<ClientId> = self.clients.values().filter(|c| c.control > 0 && c.attached && !c.ctl.no_output).map(|c| c.id).collect();
        if controls.is_empty() {
            return;
        }
        let Some(wid) = self.window_of(pane) else { return };
        let sessions: Vec<SessId> = self.links_of(wid).into_iter().map(|(s, _)| s).collect();
        let escaped = escape_output(data);
        for cid in controls {
            let c = self.clients.get_mut(&cid).unwrap();
            if !c.session.is_some_and(|s| sessions.contains(&s)) {
                continue;
            }
            match c.ctl.panes.get(&pane).copied().unwrap_or(PaneOutput::On) {
                PaneOutput::Off | PaneOutput::Paused => continue,
                PaneOutput::On => {}
            }
            let line = match c.ctl.pause_after {
                Some(limit) => {
                    let lag = c.lag();
                    if lag >= limit {
                        c.ctl.panes.insert(pane, PaneOutput::Paused);
                        c.send(S_OUTPUT, format!("%pause %{pane}\n").as_bytes());
                        continue;
                    }
                    format!("%extended-output %{pane} {} : {escaped}\n", lag.as_millis())
                }
                None => format!("%output %{pane} {escaped}\n"),
            };
            c.send(S_OUTPUT, line.as_bytes());
        }
    }

    /// refresh-client -A '%pane:on|off|pause|continue'
    pub fn control_pane_action(&mut self, cid: ClientId, spec: &str) -> Result<(), String> {
        let (target, action) = spec.rsplit_once(':').ok_or_else(|| format!("bad pane action: {spec}"))?;
        let pane: PaneId = target.trim_start_matches('%').parse().map_err(|_| format!("bad pane: {target}"))?;
        if !self.panes.contains_key(&pane) {
            return Err(format!("can't find pane: {target}"));
        }
        let c = self.clients.get_mut(&cid).ok_or("no client")?;
        let was = c.ctl.panes.get(&pane).copied().unwrap_or(PaneOutput::On);
        match action {
            "on" => {
                c.ctl.panes.insert(pane, PaneOutput::On);
            }
            "off" => {
                c.ctl.panes.insert(pane, PaneOutput::Off);
            }
            "pause" => {
                c.ctl.panes.insert(pane, PaneOutput::Paused);
                if was != PaneOutput::Paused {
                    c.send(S_OUTPUT, format!("%pause %{pane}\n").as_bytes());
                }
            }
            "continue" => {
                if was == PaneOutput::Paused {
                    c.ctl.panes.insert(pane, PaneOutput::On);
                    c.send(S_OUTPUT, format!("%continue %{pane}\n").as_bytes());
                }
            }
            other => return Err(format!("unknown pane action: {other}")),
        }
        Ok(())
    }

    /// refresh-client -f / -F: comma-separated client flags (a leading ! clears).
    pub fn control_flags(&mut self, cid: ClientId, list: &str) -> Result<(), String> {
        for flag in list.split(',').map(str::trim).filter(|f| !f.is_empty()) {
            let (on, name) = match flag.strip_prefix('!') {
                Some(n) => (false, n),
                None => (true, flag),
            };
            let c = self.clients.get_mut(&cid).ok_or("no client")?;
            match name.split_once('=') {
                Some(("pause-after", v)) => {
                    let secs: u64 = v.parse().map_err(|_| format!("bad pause-after: {v}"))?;
                    c.ctl.pause_after = (on && secs > 0).then(|| Duration::from_secs(secs));
                }
                None if name == "pause-after" => c.ctl.pause_after = None,
                None if name == "no-output" => c.ctl.no_output = on,
                None if name == "wait-exit" => c.ctl.wait_exit = on,
                None if name == "read-only" => c.readonly = on,
                None if name == "ignore-size" => c.ctl.ignore_size = on,
                None if matches!(name, "active-pane" | "no-detach-on-destroy") => {}
                _ => return Err(format!("unknown flag: {name}")),
            }
        }
        if let Some(sid) = self.client_session(cid) {
            self.update_session_size(sid);
        }
        Ok(())
    }

    /// refresh-client -B 'name:what:format' (what: "", %id, %*, @id, @*); just 'name' removes.
    pub fn control_subscribe(&mut self, cid: ClientId, spec: &str) -> Result<(), String> {
        let mut parts = spec.splitn(3, ':');
        let name = parts.next().unwrap_or("").to_string();
        if name.is_empty() {
            return Err("subscription needs a name".into());
        }
        let c = self.clients.get_mut(&cid).ok_or("no client")?;
        match (parts.next(), parts.next()) {
            (Some(what), Some(fmt)) => {
                c.ctl.subs.insert(name, (what.to_string(), fmt.to_string(), HashMap::new()));
            }
            _ => {
                c.ctl.subs.remove(&name);
            }
        }
        Ok(())
    }

    /// Sends %subscription-changed for subscriptions whose value changed.
    pub fn control_subscriptions(&mut self) {
        let ids: Vec<ClientId> = self.clients.values().filter(|c| c.control > 0 && c.attached && !c.ctl.subs.is_empty()).map(|c| c.id).collect();
        for cid in ids {
            let Some(sid) = self.client_session(cid) else { continue };
            let subs: Vec<(String, String, String)> = self.clients[&cid].ctl.subs.iter().map(|(n, (w, f, _))| (n.clone(), w.clone(), f.clone())).collect();
            let s = &self.sessions[&sid];
            let mut out = String::new();
            let mut updates = Vec::new();
            for (name, what, fmt) in subs {
                // (key, target, line prefix)
                let mut targets: Vec<(String, Target, String)> = Vec::new();
                let win_target = |idx: u32, wid: WinId| Target { sid: Some(sid), idx: Some(idx), wid: Some(wid), pane: Some(self.windows[&wid].active) };
                if what.is_empty() {
                    let t = win_target(s.active, s.active_window());
                    targets.push((String::new(), t, format!("${sid} - - -")));
                } else if what == "@*" || what.starts_with('@') {
                    for (idx, wid) in &s.links {
                        if what == "@*" || what == format!("@{wid}") {
                            targets.push((format!("@{wid}"), win_target(*idx, *wid), format!("${sid} @{wid} {idx} -")));
                        }
                    }
                } else if what == "%*" || what.starts_with('%') {
                    for (idx, wid) in &s.links {
                        for p in self.windows[wid].layout.panes() {
                            if what == "%*" || what == format!("%{p}") {
                                let t = Target { pane: Some(p), ..win_target(*idx, *wid) };
                                targets.push((format!("%{p}"), t, format!("${sid} @{wid} {idx} %{p}")));
                            }
                        }
                    }
                }
                for (key, t, prefix) in targets {
                    let value = self.expand(t, Some(cid), &fmt);
                    let last = self.clients[&cid].ctl.subs.get(&name).and_then(|x| x.2.get(&key));
                    if last != Some(&value) {
                        out.push_str(&format!("%subscription-changed {name} {prefix} : {value}\n"));
                        updates.push((name.clone(), key, value));
                    }
                }
            }
            let c = self.clients.get_mut(&cid).unwrap();
            for (name, key, value) in updates {
                if let Some(sub) = c.ctl.subs.get_mut(&name) {
                    sub.2.insert(key, value);
                }
            }
            if !out.is_empty() {
                c.send(S_OUTPUT, out.as_bytes());
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
