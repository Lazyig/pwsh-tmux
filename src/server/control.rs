//! Control mode (`-C` / `-CC`): command replies in %begin/%end blocks, pane output as
//! %output, and notifications from events (control-notify.c), held back while a
//! command's %begin/%end block is open.

use super::events::{Payload, Val};
use super::vars::Target;
use super::*;

const MAX_SUBS_BYTES: usize = 1 << 20;

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
    /// no-detach-on-destroy: switch to another session when this one is destroyed
    pub no_detach_on_destroy: bool,
    pub new_layouts: bool,
    /// A waiting command holds this client's queue; later input lines stay ordered.
    pub command_waiting: bool,
    pub command_lines: VecDeque<String>,
    pub command_bytes: usize,
    pub exiting: std::cell::Cell<bool>,
    /// Told to exit (CLIENT_EXIT): the exit is sent once the reply of the running
    /// command is written.
    pub deferred_exit: std::cell::RefCell<Option<String>>,
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
        if c.ctl.command_waiting {
            c.ctl.command_bytes = c.ctl.command_bytes.saturating_add(line.len() + 1);
            if c.ctl.command_bytes > output::MAX_REPLY {
                c.closed.store(true, std::sync::atomic::Ordering::SeqCst);
                return;
            }
            c.ctl.command_lines.push_back(line.to_string());
            return;
        }
        c.control_seq += 1;
        let seq = c.control_seq;
        let pane = self.client_active_pane(cid);
        let ctx = Ctx { client: Some(cid), pane, control_seq: Some(seq), control_line: true, ..Ctx::default() };
        // tmux finds the commands while parsing: an unknown one fails the whole line
        let unknown = super::cmds::command_list_string(line, &self.command_aliases()).err()
            .filter(|e| e.starts_with("unknown command: ") || e.starts_with("ambiguous command: "));
        if let Some(e) = unknown {
            self.control_block(cid, seq, true, "", Some(&format!("parse error: {e}")));
            return;
        }
        match crate::cmdparse::parse(line) {
            Ok(cmds) => self.run_queue(Queue { cmds: cmds.into(), ctx, soft: 0 }),
            // control_error
            Err(e) => self.control_block(cid, seq, true, "", Some(&format!("parse error: {e}"))),
        }
    }

    /// A command's reply; the flags are 1 for commands the client typed
    /// (CMDQ_STATE_CONTROL), 0 for those from its command line.
    pub fn control_block(&self, cid: ClientId, seq: u64, typed: bool, out: &str, err: Option<&str>) {
        let t = now_secs();
        let flags = typed as u8;
        let mut s = format!("%begin {t} {seq} {flags}\n");
        s.push_str(out);
        if !out.is_empty() && !out.ends_with('\n') {
            s.push('\n');
        }
        match err {
            Some(e) => {
                s.push_str(e.trim_end());
                s.push('\n');
                s.push_str(&format!("%error {t} {seq} {flags}\n"));
            }
            None => s.push_str(&format!("%end {t} {seq} {flags}\n")),
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
                        self.control_notify_write(cid, &format!("%pause %{pane}"));
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
                    self.control_notify_write(cid, &format!("%pause %{pane}"));
                }
            }
            "continue" => {
                if was == PaneOutput::Paused {
                    c.ctl.panes.insert(pane, PaneOutput::On);
                    self.control_notify_write(cid, &format!("%continue %{pane}"));
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
            // server_client_set_flags: the control mode flags only apply to control
            // clients and unknown flags are ignored
            let control = c.control > 0;
            match name.split_once('=') {
                Some(("pause-after", v)) if control => {
                    let secs: u64 = v.parse().map_err(|_| format!("bad pause-after: {v}"))?;
                    c.ctl.pause_after = (on && secs > 0).then(|| Duration::from_secs(secs));
                }
                None if name == "pause-after" && control => c.ctl.pause_after = None,
                None if name == "no-output" && control => c.ctl.no_output = on,
                None if name == "wait-exit" && control => c.ctl.wait_exit = on,
                None if name == "new-layouts" && control => c.ctl.new_layouts = on,
                None if name == "read-only" => {
                    if !on && c.is_readonly() { return Err("client is read-only".into()); }
                    c.readonly = on || c.acl_readonly;
                }
                None if name == "ignore-size" => c.ctl.ignore_size = on,
                None if name == "no-detach-on-destroy" => c.ctl.no_detach_on_destroy = on,
                _ => {}
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
                if name.len() > 256 || fmt.len() > 65536 || what.len() > 256 {
                    return Err("subscription too large".into());
                }
                if c.ctl.subs.len() >= 128 && !c.ctl.subs.contains_key(&name) {
                    return Err("too many subscriptions".into());
                }
                let total: usize = c.ctl.subs.iter().map(|(n, (w, f, _))| n.len() + w.len() + f.len()).sum();
                let previous = c.ctl.subs.get(&name).map_or(0, |(w, f, _)| name.len() + w.len() + f.len());
                if total.saturating_sub(previous).saturating_add(name.len() + what.len() + fmt.len()) > MAX_SUBS_BYTES {
                    return Err("subscription memory limit exceeded".into());
                }
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
        'clients: for cid in ids {
            let Some(sid) = self.client_session(cid) else { continue };
            let subs: Vec<(String, String, String)> = self.clients[&cid].ctl.subs.iter().map(|(n, (w, f, _))| (n.clone(), w.clone(), f.clone())).collect();
            let s = &self.sessions[&sid];
            let mut out = output::Output::default();
            let mut updates = Vec::new();
            let mut cached_bytes: usize = self.clients[&cid].ctl.subs.values()
                .flat_map(|(_, _, values)| values.iter()).map(|(key, value)| key.len() + value.len()).sum();
            // monitor_timer's order: session subscriptions, then single panes and
            // windows (by name), then every pane, then every window (by winlink,
            // each with the subscriptions in name order)
            let win_target = |idx: u32, wid: WinId| Target { sid: Some(sid), idx: Some(idx), wid: Some(wid), pane: Some(self.windows[&wid].active) };
            // (name, format, key, target, line prefix)
            let mut targets: Vec<(String, String, String, Target, String)> = Vec::new();
            for (name, _, fmt) in subs.iter().filter(|s| s.1.is_empty()) {
                let t = win_target(s.active, s.active_window());
                targets.push((name.clone(), fmt.clone(), String::new(), t, format!("${sid} - - -")));
            }
            for (name, what, fmt) in &subs {
                if what.starts_with('@') && what != "@*" {
                    for (idx, wid) in &s.links {
                        if *what == format!("@{wid}") {
                            targets.push((name.clone(), fmt.clone(), format!("@{wid}"), win_target(*idx, *wid), format!("${sid} @{wid} {idx} -")));
                        }
                    }
                } else if what.starts_with('%') && what != "%*" {
                    for (idx, wid) in &s.links {
                        for p in self.windows[wid].panes() {
                            if *what == format!("%{p}") {
                                let t = Target { pane: Some(p), ..win_target(*idx, *wid) };
                                targets.push((name.clone(), fmt.clone(), format!("%{p}"), t, format!("${sid} @{wid} {idx} %{p}")));
                            }
                        }
                    }
                }
            }
            for (idx, wid) in &s.links {
                for p in self.windows[wid].panes() {
                    for (name, _, fmt) in subs.iter().filter(|s| s.1 == "%*") {
                        let t = Target { pane: Some(p), ..win_target(*idx, *wid) };
                        targets.push((name.clone(), fmt.clone(), format!("%{p}"), t, format!("${sid} @{wid} {idx} %{p}")));
                    }
                }
            }
            for (idx, wid) in &s.links {
                for (name, _, fmt) in subs.iter().filter(|s| s.1 == "@*") {
                    targets.push((name.clone(), fmt.clone(), format!("@{wid}"), win_target(*idx, *wid), format!("${sid} @{wid} {idx} -")));
                }
            }
            {
                for (name, fmt, key, t, prefix) in targets {
                    let value = self.expand(t, Some(cid), &fmt);
                    let last = self.clients[&cid].ctl.subs.get(&name).and_then(|x| x.2.get(&key));
                    if last != Some(&value) {
                        let previous = last.map_or(0, |v| key.len() + v.len());
                        cached_bytes = cached_bytes.saturating_sub(previous).saturating_add(key.len() + value.len());
                        if cached_bytes > MAX_SUBS_BYTES {
                            self.clients[&cid].closed.store(true, std::sync::atomic::Ordering::SeqCst);
                            continue 'clients;
                        }
                        out.push_str(&format!("%subscription-changed {name} {prefix} : {value}\n"));
                        if out.overflowed() {
                            self.clients[&cid].closed.store(true, std::sync::atomic::Ordering::SeqCst);
                            continue 'clients;
                        }
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

    /// Control clients that get notifications (CONTROL_SHOULD_NOTIFY_CLIENT): not
    /// those already exiting.
    fn notify_clients(&self) -> Vec<(ClientId, Option<SessId>)> {
        self.clients.values().filter(|c| c.control > 0 && c.attached && !c.ctl.exiting.get() && c.ctl.deferred_exit.borrow().is_none())
            .map(|c| (c.id, c.session.filter(|s| self.sessions.contains_key(s)))).collect()
    }

    /// control_notify_write: notifications never go inside a %begin/%end block, so
    /// they wait until the reply of the command that caused them is written.
    pub(super) fn control_notify_write(&mut self, cid: ClientId, line: &str) {
        let q = self.ev.ctl_queue.entry(cid).or_default();
        q.push_str(line);
        q.push('\n');
    }

    /// The notifications control clients get for an event (control_build_events).
    pub fn control_event(&mut self, name: &str, p: &Payload) {
        let clients = self.notify_clients();
        if clients.is_empty() {
            return;
        }
        let window = match p.get("window") { Some(Val::Window(w, _)) => Some(*w), _ => None };
        let in_session = |srv: &Self, sid: Option<SessId>, wid: WinId| {
            sid.and_then(|s| srv.sessions.get(&s)).and_then(|s| s.links.iter().find(|(_, w)| **w == wid).map(|(i, _)| *i))
        };
        let mut lines: Vec<(ClientId, String)> = Vec::new();
        match name {
            "pane-mode-changed" => {
                if let Some(v) = p.get("pane") {
                    for (c, _) in &clients {
                        lines.push((*c, format!("%pane-mode-changed {}", v.print())));
                    }
                }
            }
            "window-layout-changed" => {
                // a window losing its last pane goes away soon: nothing to say
                let Some(wid) = window.filter(|w| self.windows.contains_key(w)) else { return };
                for (c, sid) in &clients {
                    let Some(idx) = in_session(self, *sid, wid) else { continue };
                    let t = Target { sid: *sid, idx: Some(idx), wid: Some(wid), pane: Some(self.windows[&wid].active) };
                    let mut extra = HashMap::new();
                    // the unzoomed moment of a zoomed window's resize: its layout at the old
                    // size, in the client's format (new-layouts or not)
                    if let (Some(Val::Int(sx)), Some(Val::Int(sy))) = (p.get("_unzoomed_sx"), p.get("_unzoomed_sy")) {
                        let area = crate::layout::Rect { x: 0, y: 0, w: (*sx).max(1) as u16, h: (*sy).max(1) as u16 };
                        let layout = self.window_layout_at(wid, false, Some(*c), area).unwrap_or_default();
                        let flags = self.expand(t, Some(*c), "#{window_raw_flags}").replace('Z', "");
                        extra.extend([("window_layout".to_string(), layout.clone()), ("window_visible_layout".to_string(), layout), ("window_raw_flags".to_string(), flags)]);
                    }
                    let line = self.expand_extra(t, Some(*c), "%layout-change #{window_id} #{window_layout} #{window_visible_layout} #{window_raw_flags}", extra);
                    lines.push((*c, line));
                }
            }
            "window-pane-changed" => {
                let Some(w) = window.and_then(|w| self.windows.get(&w)) else { return };
                for (c, _) in &clients {
                    lines.push((*c, format!("%window-pane-changed @{} %{}", w.id, w.active)));
                }
            }
            "window-unlinked" | "window-linked" | "window-renamed" => {
                let Some(wid) = window else { return };
                let from = match p.get("session") { Some(Val::Session(s, _)) => Some(*s), _ => None };
                for (c, sid) in &clients {
                    let Some(sid) = sid else { continue };
                    // window-unlinked fires as the link goes: still the client's window then
                    let mine = in_session(self, Some(*sid), wid).is_some() || (name == "window-unlinked" && from == Some(*sid));
                    let pre = if mine { "" } else { "unlinked-" };
                    let line = match name {
                        "window-unlinked" => format!("%{pre}window-close @{wid}"),
                        "window-linked" => format!("%{pre}window-add @{wid}"),
                        _ => match self.windows.get(&wid) {
                            Some(w) => format!("%{pre}window-renamed @{wid} {}", w.name),
                            None => continue,
                        },
                    };
                    lines.push((*c, line));
                }
            }
            "client-session-changed" => {
                let Some(Val::Client(cc, ccname)) = p.get("client") else { return };
                let Some(sid) = self.client_session(*cc) else { return };
                let sname = self.sessions[&sid].name.clone();
                for (c, csid) in &clients {
                    if csid.is_none() {
                        continue;
                    }
                    let line = if c == cc { format!("%session-changed ${sid} {sname}") } else { format!("%client-session-changed {ccname} ${sid} {sname}") };
                    lines.push((*c, line));
                }
            }
            "client-detached" => {
                let Some(Val::Client(_, ccname)) = p.get("client") else { return };
                for (c, _) in &clients {
                    lines.push((*c, format!("%client-detached {ccname}")));
                }
            }
            "session-renamed" => {
                let Some(Val::Session(sid, _)) = p.get("session") else { return };
                let Some(s) = self.sessions.get(sid) else { return };
                for (c, _) in &clients {
                    lines.push((*c, format!("%session-renamed ${sid} {}", s.name)));
                }
            }
            "session-created" | "session-closed" => {
                for (c, _) in &clients {
                    lines.push((*c, "%sessions-changed".into()));
                }
            }
            "session-window-changed" => {
                let Some(Val::Session(sid, _)) = p.get("session") else { return };
                let Some((_, wid)) = self.session_window(*sid) else { return };
                for (c, _) in &clients {
                    lines.push((*c, format!("%session-window-changed ${sid} @{wid}")));
                }
            }
            "paste-buffer-changed" | "paste-buffer-deleted" => {
                let Some(Val::Str(b)) = p.get("paste_buffer") else { return };
                for (c, _) in &clients {
                    lines.push((*c, format!("%{name} {b}")));
                }
            }
            _ => {}
        }
        for (c, line) in lines {
            self.control_notify_write(c, &line);
        }
    }

    /// Writes the notifications held for a control client.
    pub fn control_flush(&mut self, cid: ClientId) {
        if let Some(text) = self.ev.ctl_queue.remove(&cid) {
            if text.overflowed() {
                if let Some(c) = self.clients.get(&cid) {
                    c.closed.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                return;
            }
            if self.clients.get(&cid).is_some_and(|c| !c.ctl.exiting.get()) {
                self.control_write(cid, &text);
            }
        }
    }

    /// Writes every held notification (no command block is open between commands),
    /// then lets go of control clients told to exit.
    pub fn control_notify(&mut self) {
        // Continue input queued behind wait-for, run-shell and other async commands.
        loop {
            let next = self.clients.values_mut().find_map(|c| {
                if c.ctl.command_waiting || c.ctl.exiting.get() || c.ctl.deferred_exit.borrow().is_some() || c.closed.load(std::sync::atomic::Ordering::SeqCst) {
                    return None;
                }
                let line = c.ctl.command_lines.pop_front()?;
                c.ctl.command_bytes = c.ctl.command_bytes.saturating_sub(line.len() + 1);
                Some((c.id, line))
            });
            let Some((cid, line)) = next else { break };
            self.control_line(cid, &line);
        }
        let ids: Vec<ClientId> = self.ev.ctl_queue.keys().copied().collect();
        for cid in ids {
            self.control_flush(cid);
        }
        for c in self.clients.values() {
            c.finish_exit();
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
