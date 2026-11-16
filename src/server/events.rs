//! tmux 3.8 events: every hook (and control mode notification) is an event with a
//! name and a key/value payload. Hooks run from the options of the event's target,
//! wait-for -E waits for events, set-hook -B monitors fire events when a format
//! changes, and the payload keys are formats (hook_*) for the hook's commands.
//!
//! State tmux keeps per object is diffed here (`Seen`) after each command and on
//! each turn of the main loop, so changes are reported however they happen.

use super::vars::Target;
use super::*;
use crate::opts::Opts;
use std::collections::HashSet;

/// Events parsed from a single pane read must retain their order and payloads.
pub enum OutputEvent {
    Osc(&'static str, OscState),
    Title(String),
    Clipboard((String, String)),
}

/// A payload value, printed as tmux's event_payload_item_print does.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Str(String),
    Int(i64),
    Time(u64),
    /// (id, name)
    Client(ClientId, String),
    Session(SessId, String),
    Window(WinId, String),
    Pane(PaneId),
}

impl Val {
    pub fn print(&self) -> String {
        match self {
            Val::Str(s) => s.clone(),
            Val::Int(n) => n.to_string(),
            Val::Time(t) => t.to_string(),
            Val::Client(_, n) => n.clone(),
            Val::Session(id, _) => format!("${id}"),
            Val::Window(id, _) => format!("@{id}"),
            Val::Pane(id) => format!("%{id}"),
        }
    }
}

/// Where an option (hook) lives: tmux's global_s_options, global_w_options, or the
/// options of a session, window or pane.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Level {
    GlobalSession,
    GlobalWindow,
    Session(SessId),
    Window(WinId),
    Pane(PaneId),
}

#[derive(Clone, Default)]
pub struct Payload {
    pub items: BTreeMap<String, Val>,
    /// The hook's target (cmd_find_state).
    pub target: Target,
    /// Set by a monitor (tmux's _hooks_monitor): only that monitor's hook runs.
    pub monitor: Option<(Level, String)>,
    /// after-* and command-error: the hook runs straight after the command.
    pub after: bool,
    /// Fired by a read-only request: no hooks run (pwsh-tmux's privilege boundary).
    pub readonly: bool,
}

impl Payload {
    pub fn set(&mut self, key: &str, v: Val) -> &mut Self {
        self.items.insert(key.to_string(), v);
        self
    }
    pub fn str(&mut self, key: &str, v: impl Into<String>) -> &mut Self {
        self.set(key, Val::Str(v.into()))
    }
    pub fn int(&mut self, key: &str, v: i64) -> &mut Self {
        self.set(key, Val::Int(v))
    }
    pub fn get(&self, key: &str) -> Option<&Val> {
        self.items.get(key)
    }

    /// The payload as formats (event_payload_add_formats): sessions and windows also
    /// give a _name.
    pub fn formats(&self, prefix: &str) -> HashMap<String, String> {
        let mut f = HashMap::new();
        for (k, v) in self.items.iter().filter(|(k, _)| !k.starts_with('_')) {
            f.insert(format!("{prefix}{k}"), v.print());
            match v {
                Val::Session(_, n) | Val::Window(_, n) => {
                    f.insert(format!("{prefix}{k}_name"), n.clone());
                }
                _ => {}
            }
        }
        f
    }
}

/// What set-hook -B watches (the refresh-client -B subscription syntax).
#[derive(Clone, PartialEq, Debug)]
pub enum MonitorWhat {
    Session,
    Pane(u32),
    AllPanes,
    Window(u32),
    AllWindows,
}

impl MonitorWhat {
    /// monitor_parse: "name:what:format" with what "", %N, %*, @N or @*.
    pub fn parse(spec: &str) -> Option<(String, MonitorWhat, String)> {
        let (name, rest) = spec.split_once(':')?;
        let (what, format) = rest.split_once(':')?;
        let num = |s: &str| s.parse::<i64>().ok().filter(|n| *n >= 0 && *n <= u32::MAX as i64).map(|n| n as u32);
        let what = match what {
            "%*" => MonitorWhat::AllPanes,
            "@*" => MonitorWhat::AllWindows,
            "" => MonitorWhat::Session,
            w if w.starts_with('%') => MonitorWhat::Pane(num(leading_number(&w[1..]))?),
            w if w.starts_with('@') => MonitorWhat::Window(num(leading_number(&w[1..]))?),
            _ => return None,
        };
        Some((name.to_string(), what, format.to_string()))
    }

    pub fn target(&self) -> String {
        match self {
            MonitorWhat::Session => String::new(),
            MonitorWhat::Pane(n) => format!("%{n}"),
            MonitorWhat::AllPanes => "%*".into(),
            MonitorWhat::Window(n) => format!("@{n}"),
            MonitorWhat::AllWindows => "@*".into(),
        }
    }
}

/// sscanf("%d") reads a leading number and ignores the rest.
fn leading_number(s: &str) -> &str {
    let end = s.char_indices().find(|(i, c)| !(c.is_ascii_digit() || (*i == 0 && (*c == '-' || *c == '+')))).map_or(s.len(), |(i, _)| i);
    &s[..end]
}

pub struct Monitor {
    pub what: MonitorWhat,
    pub format: String,
    /// -T: only when the format is true.
    pub only_true: bool,
    /// The session of a non-global monitor (else the first session by name).
    pub session: Option<SessId>,
    /// Last value per (pane or window, winlink index); "" key for a session monitor.
    pub last: HashMap<(u32, u32), String>,
    pub fire_count: u32,
    pub fire_time: u64,
}

/// A wait-for channel: its waiters and lockers are separate (wc->waiters, wc->lockers).
#[derive(Default)]
pub struct Channel {
    pub locked: bool,
    /// Signalled with nobody waiting: the next wait-for returns at once.
    pub woken: bool,
    pub waiters: Vec<Queue>,
    pub lockers: Vec<Queue>,
}

impl Channel {
    fn is_empty(&self) -> bool {
        self.waiters.is_empty() && self.lockers.is_empty()
    }
    /// cmd_wait_for_remove_empty: unlocked, nobody waiting and not signalled.
    fn removable(&self) -> bool {
        !self.locked && self.is_empty() && !self.woken
    }
    /// cmd_wait_for_remove: unlocked, no waiters and signalled (the signal is used up).
    fn spent(&self) -> bool {
        !self.locked && self.waiters.is_empty() && self.woken
    }
}

/// A wait-for -E client.
pub struct Waiter {
    pub name: String,
    pub filter: Option<String>,
    pub verbose: bool,
    pub queue: Queue,
}

#[derive(Clone, Default, PartialEq)]
pub struct WinSeen {
    pub name: String,
    pub active: PaneId,
    pub zoomed: bool,
    pub size: (u16, u16),
    pub layout: String,
}

#[derive(Clone, Default, PartialEq)]
pub struct PaneSeen {
    pub wid: WinId,
    pub idx: Option<u32>,
    pub size: (u16, u16),
    pub mode: String,
    /// Modes seen under the current one (the pane's mode stack).
    pub below: Vec<String>,
}

#[derive(Clone, Default, PartialEq)]
pub struct SessSeen {
    pub name: String,
    pub active: (u32, WinId),
    pub links: BTreeMap<u32, WinId>,
    /// (group id, name)
    pub group: Option<(u32, String)>,
}

/// What hooks were last told about.
#[derive(Default)]
pub struct Seen {
    pub windows: HashMap<WinId, WinSeen>,
    pub panes: HashMap<PaneId, PaneSeen>,
    pub sessions: BTreeMap<SessId, SessSeen>,
}

/// A spawned pane waiting to be placed before pane-created fires.
pub struct Spawned {
    pub pane: PaneId,
    pub respawn: bool,
    pub empty: bool,
    pub command: String,
    pub cwd: String,
}

#[derive(Default)]
pub struct State {
    /// @ names set with set-hook (hooks_add_event): user options that are hooks.
    pub user_events: HashSet<String>,
    /// hook_fire_count and hook_fire_time of each hook option.
    pub fired: HashMap<(Level, String), (u32, u64)>,
    pub monitors: HashMap<(Level, String), Monitor>,
    pub waiters: Vec<Waiter>,
    /// Hook command lists queued by events, run after the current command list.
    pub pending: VecDeque<Queue>,
    /// after-* and command-error hook commands for the running queue.
    pub inject: Vec<Vec<String>>,
    draining: bool,
    /// The running command came from a hook (CMDQ_STATE_NOHOOKS).
    pub in_hook: bool,
    /// Hook formats of the running command (cmdq_add_formats).
    pub cur_formats: Option<Arc<HashMap<String, String>>>,
    pub seen: Seen,
    pub spawned: Vec<Spawned>,
    /// swap-pane fires movement for the source before the destination.
    pub pane_move_order: Vec<PaneId>,
    /// session-closed payloads of sessions destroyed since the last check.
    pub closed: Vec<(SessId, Payload)>,
    /// Group members synchronized from a changed session: (changed, others).
    pub synced: Vec<(SessId, Vec<SessId>)>,
    /// Windows whose command fires window-layout-changed itself: how many times in
    /// all, counting the change itself.
    pub layout_fired: Vec<(WinId, usize)>,
    /// Clients told to exit while attached (their session went): client-detached
    /// fires when they are lost.
    pub lost_attached: HashSet<ClientId>,
    /// Panes with output since the last turn of the loop (PANE_ACTIVITY).
    pub activity: HashSet<PaneId>,
    /// Panes that have focus (PANE_FOCUSED) and clients that lost it (~CLIENT_FOCUSED).
    pub focused: HashSet<PaneId>,
    pub unfocused: HashSet<ClientId>,
    /// Focus needs checking; whether each client had an overlay.
    pub focus_dirty: bool,
    pub overlays: HashMap<ClientId, bool>,
    /// The latest client of each window (w->latest).
    pub latest: HashMap<WinId, ClientId>,
    /// Control mode notifications waiting until no %begin block is open.
    pub ctl_queue: BTreeMap<ClientId, output::Output>,
}

/// Hook names (tmux's hooks_valid_event_name): table hooks and @ names.
pub fn valid_event_name(name: &str) -> bool {
    name.starts_with('@') || options::HOOKS.iter().any(|h| h.0 == name)
}

/// tmux's cmd_stringify_argv: arguments with spaces and other specials quoted.
pub fn stringify_argv(argv: &[String]) -> String {
    argv.iter().map(|a| options::escape_arg(a)).collect::<Vec<_>>().join(" ")
}

impl Server {
    // ---- payloads ----------------------------------------------------------------------

    fn val_session(&self, sid: SessId) -> Val {
        Val::Session(sid, self.sessions.get(&sid).map(|s| s.name.clone()).unwrap_or_default())
    }
    fn val_window(&self, wid: WinId) -> Val {
        let name = self.windows.get(&wid).map(|w| w.name.clone())
            .or_else(|| self.ev.seen.windows.get(&wid).map(|w| w.name.clone())).unwrap_or_default();
        Val::Window(wid, name)
    }
    fn val_client(&self, cid: ClientId) -> Val {
        Val::Client(cid, self.clients.get(&cid).map(|c| c.name()).unwrap_or_else(|| format!("client-{cid}")))
    }

    /// A session's current window, if it still has one (a session being destroyed
    /// may have none).
    pub fn session_window(&self, sid: SessId) -> Option<(u32, WinId)> {
        let s = self.sessions.get(&sid)?;
        s.links.get(&s.active).map(|w| (s.active, *w)).or_else(|| s.links.iter().next().map(|(i, w)| (*i, *w)))
    }

    /// cmd_find_from_session: its current window and active pane.
    pub fn target_for_session(&self, sid: SessId) -> Target {
        let Some((idx, wid)) = self.session_window(sid) else {
            return Target { sid: Some(sid).filter(|s| self.sessions.contains_key(s)), ..Target::default() };
        };
        Target { sid: Some(sid), idx: Some(idx), wid: Some(wid), pane: self.windows.get(&wid).map(|w| w.active) }
    }
    /// cmd_find_from_winlink.
    pub fn target_for_winlink(&self, sid: SessId, idx: u32) -> Target {
        let Some(wid) = self.sessions.get(&sid).and_then(|s| s.links.get(&idx)).copied() else { return self.target_for_session(sid) };
        Target { sid: Some(sid), idx: Some(idx), wid: Some(wid), pane: self.windows.get(&wid).map(|w| w.active) }
    }
    /// cmd_find_from_window: the best session holding the window.
    pub fn target_for_window(&self, wid: WinId) -> Target {
        let Some(w) = self.windows.get(&wid) else { return Target::default() };
        match self.session_of_pane(w.active) {
            Some((s, i)) => Target { sid: Some(s), idx: Some(i), wid: Some(wid), pane: Some(w.active) },
            None => Target { wid: Some(wid), pane: Some(w.active), ..Target::default() },
        }
    }
    /// cmd_find_from_client: the client's session, else the best session.
    pub fn target_for_client(&self, cid: ClientId) -> Target {
        self.current(&Ctx { client: Some(cid), ..Ctx::default() })
    }

    /// event_payload_set_target and the standard keys of events_fire_client.
    fn add_target_keys(&self, p: &mut Payload, t: Target) {
        p.target = t;
        if let Some(s) = t.sid.filter(|s| self.sessions.contains_key(s)) {
            p.set("session", self.val_session(s));
        }
        if let Some(w) = t.wid {
            p.set("window", self.val_window(w));
        }
        if let Some(i) = t.idx {
            p.int("window_index", i as i64);
        }
        if let Some(pp) = t.pane {
            p.set("pane", Val::Pane(pp));
        }
    }

    pub fn payload_client(&self, cid: ClientId) -> Payload {
        let mut p = Payload::default();
        let t = self.target_for_client(cid);
        p.set("client", self.val_client(cid));
        self.add_target_keys(&mut p, t);
        p
    }
    pub fn payload_session(&self, sid: SessId) -> Payload {
        let mut p = Payload { target: self.target_for_session(sid), ..Payload::default() };
        p.set("session", self.val_session(sid));
        p
    }
    pub fn payload_window(&self, wid: WinId) -> Payload {
        let mut p = Payload { target: self.target_for_window(wid), ..Payload::default() };
        p.set("window", self.val_window(wid));
        p
    }
    pub fn payload_pane(&self, pane: PaneId) -> Payload {
        let mut p = Payload { target: self.target_for_pane(pane), ..Payload::default() };
        p.set("pane", Val::Pane(pane));
        if let Some(w) = p.target.wid.or_else(|| self.ev.seen.panes.get(&pane).map(|s| s.wid)) {
            p.set("window", self.val_window(w));
        }
        p
    }
    pub fn payload_winlink(&self, sid: SessId, idx: u32, wid: WinId) -> Payload {
        let mut p = Payload { target: self.target_for_winlink(sid, idx), ..Payload::default() };
        p.target.wid = Some(wid);
        p.set("session", self.val_session(sid)).set("window", self.val_window(wid)).int("window_index", idx as i64);
        p
    }

    pub fn fire_client(&mut self, name: &str, cid: ClientId) {
        let p = self.payload_client(cid);
        self.fire(name, p);
    }
    pub fn fire_session(&mut self, name: &str, sid: SessId) {
        let p = self.payload_session(sid);
        self.fire(name, p);
    }
    pub fn fire_window(&mut self, name: &str, wid: WinId) {
        let p = self.payload_window(wid);
        self.fire(name, p);
    }
    pub fn fire_pane(&mut self, name: &str, pane: PaneId) {
        let p = self.payload_pane(pane);
        self.fire(name, p);
    }
    pub fn fire_winlink(&mut self, name: &str, sid: SessId, idx: u32, wid: WinId) {
        let p = self.payload_winlink(sid, idx, wid);
        self.fire(name, p);
    }

    // ---- dispatch ------------------------------------------------------------------------

    /// events_fire: control mode notifications, hooks, then wait-for -E waiters.
    /// The pre-3.8 entry point, kept for callers in other areas: fires `name` as
    /// an event for the context's client (else the session). window-layout-changed
    /// and the other change events are found by detect_changes and need no call.
    #[allow(dead_code)]
    pub fn run_hook(&mut self, sid: Option<SessId>, name: &str, ctx: Ctx) {
        if matches!(name, "window-layout-changed" | "window-resized" | "window-zoomed" | "window-unzoomed" | "pane-resized") {
            return;
        }
        let mut p = match (ctx.client.filter(|c| self.clients.contains_key(c)), sid.filter(|s| self.sessions.contains_key(s))) {
            (Some(c), _) => self.payload_client(c),
            (None, Some(s)) => self.payload_session(s),
            (None, None) => Payload::default(),
        };
        p.readonly = ctx.readonly;
        self.fire(name, p);
    }

    pub fn fire(&mut self, name: &str, mut p: Payload) {
        p.items.insert("event".into(), Val::Str(name.into()));
        p.readonly |= self.readonly_context.get();
        if p.monitor.is_none() {
            self.control_event(name, &p);
            if (options::hook_scope(name).is_some() || self.ev.user_events.contains(name)) && !self.ev.in_hook {
                self.insert_hook(name, &p, None, false);
            }
        } else if let Some((level, _)) = p.monitor.clone().filter(|m| m.1 == name && !self.ev.in_hook) {
            self.insert_hook(name, &p, Some(level), true);
        }
        self.wake_event_waiters(name, &p);
    }

    /// Whether a level holds an option itself (options_get_only); the global levels
    /// always hold the table hooks of their scope.
    pub fn level_holds(&self, level: Level, name: &str) -> bool {
        let base = name.split('[').next().unwrap_or(name);
        let global_hook = |s: Scope| options::hook_scope(base).is_some_and(|h| (h == Scope::Session) == (s == Scope::Session));
        match level {
            Level::GlobalSession if global_hook(Scope::Session) => return true,
            Level::GlobalWindow if global_hook(Scope::Window) => return true,
            _ => {}
        }
        let Some(store) = self.level_store(level) else { return false };
        let prefix = format!("{base}[");
        store.0.contains_key(base) || store.0.range(prefix.clone()..).next().is_some_and(|(k, _)| k.starts_with(&prefix))
    }

    pub fn level_store(&self, level: Level) -> Option<&Options> {
        match level {
            Level::GlobalSession => Some(&self.opts_session),
            Level::GlobalWindow => Some(&self.opts_window),
            Level::Session(s) => self.sessions.get(&s).map(|s| &s.options),
            Level::Window(w) => self.windows.get(&w).map(|w| &w.options),
            Level::Pane(p) => self.panes.get(&p).map(|p| &p.options),
        }
    }

    pub fn level_store_mut(&mut self, level: Level) -> Option<&mut Options> {
        match level {
            Level::GlobalSession => Some(&mut self.opts_session),
            Level::GlobalWindow => Some(&mut self.opts_window),
            Level::Session(s) => self.sessions.get_mut(&s).map(|s| &mut s.options),
            Level::Window(w) => self.windows.get_mut(&w).map(|w| &mut w.options),
            Level::Pane(p) => self.panes.get_mut(&p).map(|p| &mut p.options),
        }
    }

    /// A target whose objects all still exist, else the best current one
    /// (event_payload_get_target, then cmd_find_from_nothing).
    fn valid_target(&self, t: Target) -> Target {
        if self.target_is_valid(t) {
            return t;
        }
        if let Some(p) = t.pane.filter(|p| self.panes.contains_key(p) && self.window_of(*p).is_some()) {
            return self.target_for_pane(p);
        }
        if let Some(w) = t.wid.filter(|w| self.windows.contains_key(w)) {
            let wt = self.target_for_window(w);
            if wt.sid.is_some() {
                return wt;
            }
        }
        if let Some(s) = t.sid.filter(|s| self.sessions.contains_key(s)) {
            return self.target_for_session(s);
        }
        self.current(&Ctx::default())
    }

    /// cmd_find_valid_state: a session, and its winlink, window and pane all still there.
    pub fn target_is_valid(&self, t: Target) -> bool {
        let sess_ok = t.sid.is_some_and(|s| self.sessions.contains_key(&s));
        let win_ok = t.wid.is_none_or(|w| self.windows.contains_key(&w));
        let pane_ok = t.pane.is_none_or(|p| t.wid.and_then(|w| self.windows.get(&w)).is_some_and(|w| w.panes().contains(&p)));
        let link_ok = match (t.sid, t.idx) {
            (Some(s), Some(i)) => self.sessions.get(&s).and_then(|s| s.links.get(&i)) == t.wid.as_ref(),
            _ => true,
        };
        sess_ok && win_ok && pane_ok && link_ok
    }

    /// cmd_find_current_client for an item without a client: the most recently
    /// active client of the best session, else of any session. pwsh-tmux leaves out
    /// read-only clients, which would make the hook read-only too.
    pub fn best_client(&self) -> Option<ClientId> {
        let ok = |c: &&Client| c.attached && c.session.is_some_and(|s| self.sessions.contains_key(&s)) && !c.denied && !c.is_readonly();
        let best = |sid: Option<SessId>| self.clients.values().filter(ok).filter(|c| sid.is_none() || c.session == sid).max_by_key(|c| (c.activity, c.id)).map(|c| c.id);
        let sid = self.current_session();
        let attached = sid.is_some_and(|s| self.clients.values().any(|c| c.attached && c.session == Some(s)));
        best(sid.filter(|_| attached)).or_else(|| best(None))
    }

    /// The level whose entry for a hook applies at a target (hooks_insert): the
    /// session's options and their parent, then the pane's, then the window's.
    fn hook_level(&self, name: &str, t: Target) -> Option<Level> {
        let session_hook = name.starts_with('@') || options::hook_scope(name) == Some(Scope::Session);
        if session_hook {
            if let Some(s) = t.sid.filter(|s| self.level_holds(Level::Session(*s), name)) {
                return Some(Level::Session(s));
            }
            if self.level_holds(Level::GlobalSession, name) {
                return Some(Level::GlobalSession);
            }
        }
        if t.pane.is_some() || t.wid.is_some() {
            let pane = t.pane.map(Level::Pane);
            let win = t.wid.map(Level::Window);
            for l in [pane, win, Some(Level::GlobalWindow)].into_iter().flatten() {
                if (name.starts_with('@') || options::hook_scope(name).is_some_and(|s| s != Scope::Session)) && self.level_holds(l, name) {
                    return Some(l);
                }
            }
        }
        None
    }

    /// The command lists of a hook at a level: array members in key order, or the
    /// string value of an @ hook.
    pub fn hook_commands(&self, level: Level, name: &str) -> Vec<String> {
        let Some(store) = self.level_store(level) else { return Vec::new() };
        if name.starts_with('@') {
            return store.get(name).map(|v| vec![v.to_string()]).unwrap_or_default();
        }
        store.array_items(name).into_iter().map(|(_, v)| v).filter(|v| !v.is_empty()).collect()
    }

    /// hooks_insert_event: queue the hook's commands with the payload as hook_* formats.
    fn insert_hook(&mut self, name: &str, p: &Payload, level: Option<Level>, expand: bool) {
        if p.readonly {
            return;
        }
        let target = self.valid_target(p.target);
        let level = match level {
            Some(l) => (self.level_holds(l, name)).then_some(l),
            None => self.hook_level(name, target),
        };
        let Some(level) = level else { return };
        let fired = self.ev.fired.entry((level, name.to_string())).or_insert((0, 0));
        fired.0 += 1;
        fired.1 = now_secs();
        let mut formats = p.formats("hook_");
        formats.insert("hook".into(), name.into());
        let client = match p.get("client") {
            Some(Val::Client(c, _)) => Some(*c).filter(|c| self.clients.contains_key(c)),
            _ => None,
        };
        // each member is its own command list (hooks_insert_one): an error stops
        // only that one
        let mut lists = Vec::new();
        for value in self.hook_commands(level, name) {
            let value = if expand { self.expand_extra(target, client, &value, formats.clone()) } else { value };
            match crate::cmdparse::parse(&value) {
                Ok(v) if !v.is_empty() => lists.push(VecDeque::from(v)),
                Ok(_) => {}
                Err(e) => log(&format!("hook {name}: {e}")),
            }
        }
        let formats = Arc::new(formats);
        if p.after {
            // inserted after the command, in its client's queue (cmdq_insert_after)
            for cmds in lists {
                self.ev.inject.extend(wrap_hook(cmds, target, &formats));
            }
            return;
        }
        // the global queue: the item has no client, so it runs as the best one
        let best = self.best_client();
        for cmds in lists {
            let ctx = Ctx {
                client: best,
                pane: target.pane,
                target: Some(target),
                hook_depth: 2,
                formats: Some(formats.clone()),
                ..Ctx::default()
            };
            self.ev.pending.push_back(Queue { cmds, ctx, soft: 0 });
        }
    }

    /// hooks_run (set-hook -R): the hook's commands to run now, after the command,
    /// with only #{hook}.
    pub fn run_hook_now(&mut self, name: &str, t: Target, client: Option<ClientId>, readonly: bool) -> Vec<Vec<String>> {
        let target = self.valid_target(t);
        let Some(level) = self.hook_level(name, target) else { return Vec::new() };
        if readonly || self.readonly_context.get() || self.client_is_readonly(client) {
            return Vec::new();
        }
        let fired = self.ev.fired.entry((level, name.to_string())).or_insert((0, 0));
        fired.0 += 1;
        fired.1 = now_secs();
        let formats = HashMap::from([("hook".to_string(), name.to_string())]);
        let mut out = Vec::new();
        for value in self.hook_commands(level, name) {
            match crate::cmdparse::parse(&value) {
                Ok(v) => out.extend(wrap_hook(v.into(), target, &formats)),
                Err(e) => log(&format!("hook {name}: {e}")),
            }
        }
        out
    }

    /// Runs hook command lists queued by events (the global command queue).
    pub fn drain_hooks(&mut self) {
        if self.ev.draining {
            return;
        }
        self.ev.draining = true;
        while let Some(q) = self.ev.pending.pop_front() {
            self.run_queue(q);
        }
        self.ev.draining = false;
    }

    // ---- after hooks -------------------------------------------------------------------

    /// cmdq_insert_hook: after-<command> and command-error carry the command's
    /// arguments (arguments, argument_N, flag_X and flag_X_N).
    pub fn command_hook(&mut self, hook: &str, argv: &[String], ctx: &Ctx) {
        if ctx.hook_depth > 0 || self.ev.in_hook {
            return;
        }
        let Some(full) = argv.first().and_then(|n| cmds::canonical(n)) else { return };
        let mut p = Payload { after: true, readonly: ctx.readonly, ..Payload::default() };
        let (bools, vals) = cmds::command_flags(full);
        let args = &argv[1..];
        let mut flags: BTreeMap<char, Vec<Option<String>>> = BTreeMap::new();
        let mut i = 0;
        while let Some(a) = args.get(i) {
            if a == "--" {
                i += 1;
                break;
            }
            if !a.starts_with('-') || a.len() < 2 {
                break;
            }
            i += 1;
            let chars: Vec<char> = a[1..].chars().collect();
            for (j, &c) in chars.iter().enumerate() {
                if bools.contains(c) {
                    flags.entry(c).or_default().push(None);
                } else if vals.contains(c) {
                    let inline: String = chars[j + 1..].iter().collect();
                    let v = if inline.is_empty() {
                        i += 1;
                        args.get(i - 1).cloned().unwrap_or_default()
                    } else {
                        inline
                    };
                    flags.entry(c).or_default().push(Some(v));
                    break;
                }
            }
        }
        let rest = &args[i.min(args.len())..];
        // args_print: bare flags together, then flags with values, then arguments
        let mut printed = String::new();
        let bare: String = flags.iter().filter(|(_, v)| v.iter().all(Option::is_none)).flat_map(|(c, v)| std::iter::repeat_n(*c, v.len())).collect();
        if !bare.is_empty() {
            printed.push('-');
            printed.push_str(&bare);
        }
        for (c, vs) in &flags {
            for v in vs.iter().flatten() {
                if !printed.is_empty() {
                    printed.push(' ');
                }
                printed.push_str(&format!("-{c} {}", options::escape_arg(v)));
            }
        }
        for a in rest {
            if !printed.is_empty() {
                printed.push(' ');
            }
            let braced = a.starts_with('{') && a.ends_with('}');
            printed.push_str(&if braced { a.clone() } else { options::escape_arg(a) });
        }
        p.str("arguments", printed);
        for (n, a) in rest.iter().enumerate() {
            p.str(&format!("argument_{n}"), a.clone());
        }
        for (c, vs) in &flags {
            let last = vs.iter().rev().flatten().next().cloned();
            p.str(&format!("flag_{c}"), last.unwrap_or_else(|| "1".into()));
            for (n, v) in vs.iter().flatten().enumerate() {
                p.str(&format!("flag_{c}_{n}"), v.clone());
            }
        }
        // the command's target, else the current state
        let t = flags.get(&'t').and_then(|v| v.iter().rev().flatten().next())
            .and_then(|t| self.resolve_pane(ctx, Some(t)).or_else(|_| self.resolve_window(ctx, Some(t))).ok())
            .unwrap_or_else(|| self.current(ctx));
        // cmdq_fire_command: an after hook needs a valid target (no session, as in a
        // startup config, means none); command-error fires without one
        if hook != "command-error" && !self.target_is_valid(t) {
            return;
        }
        p.target = t;
        self.fire(hook, p);
    }

    // ---- wait-for ----------------------------------------------------------------------

    fn wake_event_waiters(&mut self, name: &str, p: &Payload) {
        let mut i = 0;
        let n = self.ev.waiters.len();
        let mut checked = 0;
        while checked < n && i < self.ev.waiters.len() {
            checked += 1;
            if self.ev.waiters[i].name != name {
                i += 1;
                continue;
            }
            if self.ev.waiters[i].verbose {
                let mut text = String::new();
                for (k, v) in p.items.iter().filter(|(k, _)| !k.starts_with('_')) {
                    text.push_str(&format!("{k}={}\n", v.print()));
                }
                self.waiter_print(i, &text);
            }
            let w = &self.ev.waiters[i];
            if let Some(f) = &w.filter {
                // the payload only, no target or client (format_create without
                // format_defaults: ft->c is unset); tmux's own variables (window_name,
                // client_name, ...) come first and are empty
                let mut extra = p.formats("");
                extra.retain(|k, _| !cmds::VAR_NAMES.contains(&k.as_str()));
                let ok = cmds::truthy(&self.expand_extra(Target::default(), None, f, extra));
                if !ok {
                    i += 1;
                    continue;
                }
            }
            let w = self.ev.waiters.remove(i);
            self.ev.pending.push_back(w.queue);
        }
    }

    /// cmdq_print to a waiting client: straight to the stdout of a command client;
    /// bare lines to a control client, whose block for wait-for already ended (in
    /// order with the event's notifications).
    fn waiter_print(&mut self, i: usize, text: &str) {
        let w = &mut self.ev.waiters[i];
        if let (Some(_), Some(cid)) = (w.queue.ctx.control_seq, w.queue.ctx.client) {
            self.ev.ctl_queue.entry(cid).or_default().push_str(text);
            return;
        }
        let reply = w.queue.ctx.reply.filter(|_| w.queue.ctx.control_seq.is_none());
        match reply.and_then(|r| self.clients.get(&r)).filter(|c| !c.attached) {
            Some(c) => {
                let mut out = std::mem::take(&mut w.queue.ctx.out).to_string();
                out.push_str(text);
                c.send(S_STDOUT, out.as_bytes());
            }
            None => w.queue.ctx.out.push_str(text),
        }
    }

    fn waiter_name(&self, ctx: &Ctx) -> String {
        ctx.reply.or(ctx.client).and_then(|c| self.clients.get(&c)).map(|c| c.name()).unwrap_or_default()
    }

    /// wait-for: channels (-S/-L/-U, waiting, -l, -w) and events (-E).
    pub(super) fn cmd_wait_for(&mut self, o: &Opts, ctx: &mut Ctx) -> Result<Flow, String> {
        let name = o.rest.first().ok_or("command wait-for: too few arguments (need at least 1)")?.clone();
        if o.rest.len() > 1 {
            return Err("command wait-for: too many arguments (need at most 1)".into());
        }
        // the client whose queue waits (none in hooks)
        let able = ctx.hook_depth < 2 && ctx.reply.or(ctx.client).is_some_and(|c| self.clients.contains_key(&c));
        if o.has('E') {
            if !valid_event_name(&name) {
                return Err(format!("invalid event: {name}"));
            }
            if o.has('l') {
                for w in self.ev.waiters.iter().filter(|w| w.name == name) {
                    ctx.out.push_str(&format!("{}\n", self.waiter_name(&w.queue.ctx)));
                }
                return Ok(Flow::Next);
            }
            if let Some(who) = o.get('w') {
                let found = self.ev.waiters.iter().position(|w| w.name == name && self.waiter_name(&w.queue.ctx) == who);
                let Some(i) = found else { return Err(format!("waiter {who} not found")) };
                let w = self.ev.waiters.remove(i);
                self.ev.pending.push_back(w.queue);
                return Ok(Flow::Next);
            }
            if !able {
                return Err("not able to wait".into());
            }
            return Err(format!("\0wait-event\0{name}\0{}\0{}\0{}", o.get('F').unwrap_or(""), if o.has('v') { "v" } else { "" }, if o.get('F').is_some() { "F" } else { "" }));
        }
        if o.has('l') {
            if let Some(ch) = self.waits.get(&name) {
                // waiters, then lockers
                for q in ch.waiters.iter().chain(&ch.lockers) {
                    ctx.out.push_str(&format!("{}\n", self.waiter_name(&q.ctx)));
                }
            }
            return Ok(Flow::Next);
        }
        if let Some(who) = o.get('w') {
            // a waiter, else a locker (which goes on without the lock)
            let clients: HashSet<ClientId> = self.clients.values().filter(|c| c.name() == who).map(|c| c.id).collect();
            let mine = |q: &Queue| q.ctx.reply.or(q.ctx.client).is_some_and(|c| clients.contains(&c));
            let Some(ch) = self.waits.get_mut(&name) else { return Ok(Flow::Next) };
            let w = ch.waiters.iter().position(mine);
            let q = match w {
                Some(i) => Some(ch.waiters.remove(i)),
                None => ch.lockers.iter().position(mine).map(|i| ch.lockers.remove(i)),
            };
            if let Some(q) = q {
                if ch.removable() {
                    self.waits.remove(&name);
                }
                self.ev.pending.push_back(q);
            }
            return Ok(Flow::Next);
        }
        if o.has('S') {
            let ch = self.waits.entry(name.clone()).or_default();
            if ch.waiters.is_empty() && !ch.woken {
                // a signal nobody is waiting for is kept for the next wait-for
                ch.woken = true;
                return Ok(Flow::Next);
            }
            // only the waiters: lockers wait for -U. They go on once this command
            // list is done (cmdq_continue).
            let waiters = std::mem::take(&mut ch.waiters);
            // cmd_wait_for_remove: a second signal uses up the remembered one
            if ch.spent() || ch.removable() {
                self.waits.remove(&name);
            }
            self.ev.pending.extend(waiters);
            return Ok(Flow::Next);
        }
        if o.has('L') {
            if !able {
                return Err("not able to lock".into());
            }
            let ch = self.waits.entry(name.clone()).or_default();
            if !ch.locked {
                ch.locked = true;
                return Ok(Flow::Next);
            }
            return Err(format!("\0lock\0{name}"));
        }
        if o.has('U') {
            let Some(ch) = self.waits.get_mut(&name).filter(|c| c.locked) else { return Err(format!("channel {name} not locked")) };
            if ch.lockers.is_empty() {
                ch.locked = false;
                if ch.spent() || ch.removable() {
                    self.waits.remove(&name);
                }
            } else {
                // the first locker gets the lock
                let q = ch.lockers.remove(0);
                self.ev.pending.push_back(q);
            }
            return Ok(Flow::Next);
        }
        if !able {
            return Err("not able to wait".into());
        }
        let ch = self.waits.entry(name.clone()).or_default();
        if ch.woken {
            // already signalled (a locked channel stays signalled)
            if ch.spent() {
                self.waits.remove(&name);
            }
            return Ok(Flow::Next);
        }
        Err(format!("\0wait\0{name}"))
    }

    /// Parks a queue on wait-for -E (from run_queue).
    pub fn park_event_waiter(&mut self, name: String, filter: Option<String>, verbose: bool, q: Queue) {
        self.ev.waiters.push(Waiter { name, filter, verbose, queue: q });
    }

    /// cmd_wait_for_client_lost: a client that goes away stops waiting, and the rest of
    /// its command list does not run.
    pub fn wait_for_client_lost(&mut self, cid: ClientId) {
        let mine = |q: &Queue| q.ctx.reply == Some(cid) || (q.ctx.reply.is_none() && q.ctx.client == Some(cid));
        self.ev.waiters.retain(|w| !mine(&w.queue));
        for ch in self.waits.values_mut() {
            ch.waiters.retain(|q| !mine(q));
            ch.lockers.retain(|q| !mine(q));
        }
        // cmd_wait_for_remove_empty
        self.waits.retain(|_, ch| ch.locked || ch.woken || !ch.is_empty());
    }

    // ---- set-hook / show-hooks -------------------------------------------------------------

    /// The target of an option command: -t as a pane, window or session, else the
    /// current one (CMD_FIND_PANE with CMD_FIND_CANFAIL).
    fn option_cmd_target(&self, o: &Opts, ctx: &Ctx) -> Target {
        match o.get('t') {
            Some(t) => self.resolve_pane(ctx, Some(t)).or_else(|_| self.resolve_window(ctx, Some(t)))
                .or_else(|_| self.resolve_session(ctx, Some(t)).map(|s| Target { sid: Some(s), ..Target::default() }))
                .unwrap_or_default(),
            None => self.current(ctx),
        }
    }

    /// The level for a scope (options_scope_from_flags / _from_name).
    fn level_for(&self, o: &Opts, ctx: &Ctx, scope: Scope) -> Result<Level, String> {
        let target = self.option_cmd_target(o, ctx);
        let missing = |what: &str| match o.get('t') {
            Some(t) => format!("no such {what}: {t}"),
            None => format!("no current {what}"),
        };
        match scope {
            Scope::Pane => target.pane.map(Level::Pane).ok_or_else(|| missing("pane")),
            Scope::Window | Scope::Server if o.has('g') => Ok(Level::GlobalWindow),
            Scope::Window => target.wid.map(Level::Window).ok_or_else(|| missing("window")),
            _ if o.has('g') => Ok(Level::GlobalSession),
            _ => target.sid.map(Level::Session).ok_or_else(|| missing("session")),
        }
    }

    /// The level a hook option is set at or shown from (options_scope_from_name for
    /// table hooks, options_scope_from_flags for @ names).
    pub fn hook_scope_level(&self, o: &Opts, ctx: &Ctx, name: &str) -> Result<Level, String> {
        let scope = if name.starts_with('@') {
            if o.has('p') { Scope::Pane } else if o.has('w') { Scope::Window } else { Scope::Session }
        } else {
            match options::hook_scope(name).ok_or_else(|| format!("unknown option: {name}"))? {
                Scope::Pane if o.has('p') => Scope::Pane,
                Scope::Pane | Scope::Window => Scope::Window,
                s => s,
            }
        };
        self.level_for(o, ctx, scope)
    }

    /// set-option / set-hook on a table hook: an array of command lists.
    pub fn set_hook_option(&mut self, o: &Opts, ctx: &Ctx, name: &str, arg: &str, value: Option<String>, aliases: &[String]) -> Result<(), String> {
        let level = match self.hook_scope_level(o, ctx, name.split('[').next().unwrap_or(name)) {
            Ok(l) => l,
            Err(_) if o.has('q') => return Ok(()),
            Err(e) => return Err(e),
        };
        let (base, key) = match name.split_once('[') {
            Some((b, k)) => (b.to_string(), Some(k.trim_end_matches(']').to_string())),
            None => (name.to_string(), None),
        };
        let already = match &key {
            None => self.level_holds(level, &base),
            Some(k) => self.level_store(level).is_some_and(|s| s.get(&format!("{base}[{k}]")).is_some()),
        };
        if o.has('o') && !o.has('u') && already {
            return if o.has('q') { Ok(()) } else { Err(format!("already set: {arg}")) };
        }
        let global = matches!(level, Level::GlobalSession | Level::GlobalWindow);
        if o.has('u') || o.has('U') {
            if o.has('U') && level_is_window(level) {
                if let Level::Window(w) = level {
                    let panes = self.windows.get(&w).map(|w| w.panes()).unwrap_or_default();
                    for p in panes {
                        if let Some(p) = self.panes.get_mut(&p) {
                            unset_hook(&mut p.options, &base, key.as_deref(), false);
                        }
                    }
                }
            }
            let store = self.level_store_mut(level).ok_or("no such target")?;
            unset_hook(store, &base, key.as_deref(), global);
            if key.is_none() {
                self.ev.fired.remove(&(level, base.clone()));
            }
            return Ok(());
        }
        let value = value.ok_or("empty value")?;
        let cmd = cmds::command_list_string(&value, aliases);
        let store = self.level_store_mut(level).ok_or("no such target")?;
        // the level gets the (empty) array even when the command does not parse
        store.touch_array(&base);
        // options_array_clear comes before options_array_assign: a command that does
        // not parse still empties the hook
        if key.is_none() && !o.has('a') {
            let keys: Vec<String> = store.array_items(&base).into_iter().map(|(k, _)| k).collect();
            for k in keys {
                store.0.remove(&k);
            }
        }
        let cmd = cmd?;
        match key {
            Some(k) => store.set(&format!("{base}[{k}]"), cmd),
            None => {
                // options_array_assign adds nothing for an empty value, else uses the first free key
                if !value.is_empty() {
                    let mut i = 0u32;
                    while store.get(&format!("{base}[{i}]")).is_some() {
                        i += 1;
                    }
                    store.set(&format!("{base}[{i}]"), cmd);
                }
            }
        }
        Ok(())
    }

    /// set-hook: -E fires an event, -B adds a monitor, -R runs a hook, otherwise it
    /// sets the hook like set-option.
    pub(super) fn cmd_set_hook(&mut self, o: &Opts, ctx: &mut Ctx, aliases: &[String]) -> Result<Flow, String> {
        let t = self.resolve_pane(ctx, o.get('t')).ok();
        if o.has('E') {
            if o.rest.is_empty() {
                return Err("missing argument".into());
            }
            if o.rest.len() != 1 {
                return Err("too many arguments".into());
            }
            let cur = t.unwrap_or_else(|| self.current(ctx));
            let name = self.expand(cur, ctx.client, &o.rest[0]);
            if !name.starts_with('@') {
                return Err("event name must start with @".into());
            }
            let mut p = Payload::default();
            if let Some(c) = ctx.reply.or(ctx.client).filter(|c| self.clients.contains_key(c)) {
                p.set("client", self.val_client(c));
            }
            if let Some(t) = t {
                self.add_target_keys(&mut p, t);
            }
            self.fire(&name, p);
            return Ok(Flow::Next);
        }
        if let Some(spec) = o.get('B') {
            return self.set_hook_monitor(o, ctx, spec.to_string());
        }
        if o.rest.len() > 2 {
            return Err("command set-hook: too many arguments (need at most 2)".into());
        }
        let arg = o.rest.first().ok_or("missing argument")?;
        let cur = t.unwrap_or_else(|| self.current(ctx));
        let arg = self.expand(cur, ctx.client, arg);
        if o.has('R') {
            // hooks_run looks the name up as given: "hook[N]" is no option
            if arg.contains('[') {
                return Ok(Flow::Next);
            }
            let cmds = self.run_hook_now(&arg, cur, ctx.client, ctx.readonly);
            return Ok(if cmds.is_empty() { Flow::Next } else { Flow::Inject(cmds, false) });
        }
        let name = options::canonical_name(&arg)?;
        if name.starts_with('@') {
            let level = self.hook_scope_level(o, ctx, &name)?;
            if name.contains('[') {
                return Err(format!("not an array: {arg}"));
            }
            if o.has('u') {
                if let Some(s) = self.level_store_mut(level) {
                    s.unset(&name);
                }
                self.ev.monitors.remove(&(level, name.clone()));
                self.ev.fired.remove(&(level, name));
                return Ok(Flow::Next);
            }
            let value = o.rest.get(1).ok_or("empty value")?.clone();
            let store = self.level_store_mut(level).ok_or("no such target")?;
            let value = match store.get(&name) {
                Some(old) if o.has('a') => format!("{old}{value}"),
                _ => value,
            };
            store.set(&name, value);
            self.ev.user_events.insert(name);
            return Ok(Flow::Next);
        }
        if options::hook_scope(&name).is_none() {
            // set-hook is set-option (cmd_set_option_exec) for any other option
            self.cmd_set_option(false, o, ctx)?;
            return Ok(Flow::Next);
        }
        let value = o.rest.get(1).cloned();
        self.set_hook_option(o, ctx, &name, &arg, value, aliases)
            .map(|_| Flow::Next)
    }

    /// set-hook -B name:what:format [command]
    fn set_hook_monitor(&mut self, o: &Opts, ctx: &Ctx, spec: String) -> Result<Flow, String> {
        if o.rest.len() > 1 {
            return Err("too many arguments".into());
        }
        let parsed = MonitorWhat::parse(&spec);
        let name = match (&parsed, o.has('u')) {
            (Some(p), _) => p.0.clone(),
            (None, true) => spec.clone(),
            (None, false) => return Err(format!("invalid subscription: {spec}")),
        };
        if !name.starts_with('@') {
            return Err("monitor hook name must start with @".into());
        }
        let level = self.hook_scope_level(o, ctx, &name)?;
        if o.has('u') {
            self.ev.monitors.remove(&(level, name));
            return Ok(Flow::Next);
        }
        let (_, what, format) = parsed.unwrap();
        let store = self.level_store_mut(level).ok_or("no such target")?;
        match o.rest.first() {
            Some(v) => {
                let v = match store.get(&name) {
                    Some(old) if o.has('a') => format!("{old}{v}"),
                    _ => v.clone(),
                };
                store.set(&name, v);
            }
            None if store.get(&name).is_none() => store.set(&name, String::new()),
            None => {}
        }
        let session = match level {
            Level::GlobalSession | Level::GlobalWindow => None,
            _ => self.resolve_pane(ctx, o.get('t')).ok().and_then(|t| t.sid).or_else(|| self.current(ctx).sid),
        };
        self.ev.monitors.insert((level, name), Monitor {
            what,
            format,
            only_true: o.has('T'),
            session,
            last: HashMap::new(),
            fire_count: 0,
            fire_time: 0,
        });
        Ok(Flow::Next)
    }

    /// Whether an @ option is a hook (an event registered by set-hook, or a monitor).
    pub fn is_user_hook(&self, level: Level, name: &str) -> bool {
        self.ev.user_events.contains(name) || self.ev.monitors.contains_key(&(level, name.to_string()))
    }

    /// show-hooks: hook options at a level, -B the monitors.
    pub(super) fn cmd_show_hooks(&mut self, o: &Opts, ctx: &mut Ctx) -> Result<Flow, String> {
        if o.rest.len() > 1 {
            return Err("command show-hooks: too many arguments (need at most 1)".into());
        }
        self.prune_monitors();
        let ft = self.resolve_pane(ctx, o.get('t')).unwrap_or_else(|_| self.current(ctx));
        let named = match o.rest.first() {
            Some(a) => {
                let a = self.expand(ft, ctx.client, a);
                Some((options::canonical_name(&a)?, a))
            }
            None => None,
        };
        if let Some((n, _)) = named.as_ref().filter(|(n, _)| !n.starts_with('@') && options::hook_scope(n.split('[').next().unwrap_or(n)).is_none()) {
            // any other option is shown as show-options shows it
            let _ = n;
            self.cmd_show_options(false, o, ctx)?;
            return Ok(Flow::Next);
        }
        let level = match &named {
            Some((n, _)) => self.hook_scope_level(o, ctx, n.split('[').next().unwrap_or(n))?,
            None => self.level_for(o, ctx, if o.has('p') { Scope::Pane } else if o.has('w') { Scope::Window } else { Scope::Session })?,
        };
        let mut out = String::new();
        if o.has('B') {
            let mut names: Vec<String> = match &named {
                Some((n, arg)) => {
                    if n.starts_with('@') && self.level_store(level).and_then(|s| s.get(n)).is_none() {
                        return Err(format!("invalid option: {arg}"));
                    }
                    vec![n.clone()]
                }
                None => self.ev.monitors.keys().filter(|k| k.0 == level).map(|k| k.1.clone()).collect(),
            };
            names.sort();
            for n in names {
                let Some(m) = self.ev.monitors.get(&(level, n.clone())) else { continue };
                let target = m.what.target();
                let mut vars = HashMap::new();
                for (k, v) in [
                    ("option_name", n.clone()), ("option_value", format!("{n}:{target}:{}", m.format)),
                    ("option_value_only", "0".into()), ("option_is_parent", "0".into()), ("option_is_array", "0".into()),
                    ("option_is_string", "1".into()), ("option_is_hook", "1".into()), ("option_is_user", "1".into()),
                    ("option_has_value", "1".into()), ("option_array_key", String::new()), ("option_has_array_key", "0".into()),
                    ("hook_monitor_target", target.clone()), ("hook_monitor_format", m.format.clone()),
                    ("hook_fire_count", m.fire_count.to_string()),
                ] {
                    vars.insert(k.to_string(), v);
                }
                if m.fire_time != 0 {
                    vars.insert("hook_fire_time".into(), m.fire_time.to_string());
                }
                let f = o.get('F').unwrap_or("#{option_name}:#{hook_monitor_target}:#{hook_monitor_format}");
                out.push_str(&self.expand_extra(ft, ctx.client, f, vars));
                out.push('\n');
            }
            ctx.out.push_str(&out);
            return Ok(Flow::Next);
        }
        let mut lines: Vec<(String, Option<String>, Level)> = Vec::new();
        match &named {
            Some((n, arg)) => {
                let base = n.split('[').next().unwrap_or(n);
                if base.starts_with('@') {
                    match self.level_store(level).and_then(|s| s.get(base)) {
                        Some(v) => lines.push((base.to_string(), Some(v.to_string()), level)),
                        None => return Err(format!("invalid option: {arg}")),
                    }
                } else if self.level_holds(level, base) {
                    self.hook_lines(level, n, &mut lines);
                }
            }
            None => {
                if let Some(store) = self.level_store(level) {
                    for (k, v) in store.0.iter().filter(|(k, _)| k.starts_with('@') && !k.contains('[')) {
                        if self.is_user_hook(level, k) {
                            lines.push((k.clone(), Some(v.clone()), level));
                        }
                    }
                }
                for (h, scope) in options::HOOKS {
                    let wanted = match level {
                        Level::GlobalSession | Level::Session(_) => *scope == Scope::Session,
                        Level::GlobalWindow | Level::Window(_) => *scope != Scope::Session,
                        Level::Pane(_) => *scope == Scope::Pane,
                    };
                    if wanted && self.level_holds(level, h) {
                        self.hook_lines(level, h, &mut lines);
                    }
                }
            }
        }
        for (key, value, lvl) in lines {
            let base = key.split('[').next().unwrap_or(&key).to_string();
            let akey = key.split_once('[').map(|(_, k)| k.trim_end_matches(']').to_string());
            let fired = self.ev.fired.get(&(lvl, base.clone())).copied().unwrap_or((0, 0));
            match o.get('F') {
                Some(f) => {
                    let user = base.starts_with('@');
                    let b = |v: bool| if v { "1".to_string() } else { "0".to_string() };
                    let mut vars: HashMap<String, String> = [
                        ("option_name", base.clone()), ("option_value", value.clone().unwrap_or_default()),
                        ("option_value_only", "0".into()), ("option_is_parent", "0".into()), ("option_is_array", b(!user)),
                        ("option_is_string", b(user)), ("option_is_hook", b(!user)), ("option_is_user", b(user)),
                        ("option_has_value", b(value.is_some())), ("option_has_array_key", b(akey.is_some())),
                        ("option_array_key", akey.clone().unwrap_or_default()), ("hook_fire_count", fired.0.to_string()),
                    ].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
                    if fired.1 != 0 {
                        vars.insert("hook_fire_time".into(), fired.1.to_string());
                    }
                    out.push_str(&self.expand_extra(ft, ctx.client, f, vars));
                    out.push('\n');
                }
                None => match value {
                    Some(v) if base.starts_with('@') => out.push_str(&format!("{key} {}\n", options::escape_arg(&v))),
                    Some(v) => out.push_str(&format!("{key} {v}\n")),
                    None => out.push_str(&format!("{key}\n")),
                },
            }
        }
        ctx.out.push_str(&out);
        Ok(Flow::Next)
    }

    /// The lines a hook option shows at a level: each member, or the bare name of
    /// an empty array (or one member when a key is named).
    pub fn hook_lines(&self, level: Level, name: &str, out: &mut Vec<(String, Option<String>, Level)>) {
        let base = name.split('[').next().unwrap_or(name);
        let items = self.level_store(level).map(|s| s.array_items(base)).unwrap_or_default();
        if name.contains('[') {
            let v = self.level_store(level).and_then(|s| s.get(name)).unwrap_or("").to_string();
            out.push((name.to_string(), Some(v), level));
            return;
        }
        if items.is_empty() {
            out.push((base.to_string(), None, level));
        }
        for (k, v) in items {
            out.push((k, Some(v), level));
        }
    }

    /// The levels a level inherits from (pane -> window -> global window, session ->
    /// global session).
    pub fn hook_parents(&self, level: Level) -> Vec<Level> {
        match level {
            Level::Pane(p) => self.window_of(p).map(Level::Window).into_iter().chain([Level::GlobalWindow]).collect(),
            Level::Window(_) => vec![Level::GlobalWindow],
            Level::Session(_) => vec![Level::GlobalSession],
            _ => Vec::new(),
        }
    }

    /// marked-pane-changed (select-pane -m/-M): the new marked pane, else the old one.
    pub fn fire_marked_changed(&mut self, pane: PaneId, last: Option<PaneId>) {
        let marked = self.marked;
        let subject = marked.or(last).unwrap_or(pane);
        let mut p = self.payload_pane(subject);
        if let Some(m) = marked {
            p.set("new_pane", Val::Pane(m));
        }
        if let Some(l) = last {
            p.set("old_pane", Val::Pane(l));
        }
        p.int("marked", marked.is_some() as i64);
        self.fire("marked-pane-changed", p);
    }

    // ---- buffers and clipboard ---------------------------------------------------------

    /// paste-buffer-changed / paste-buffer-deleted (paste_fire_event).
    pub fn buffer_event(&mut self, name: &str, deleted: bool) {
        let mut p = Payload::default();
        p.str("paste_buffer", name);
        self.fire(if deleted { "paste-buffer-deleted" } else { "paste-buffer-changed" }, p);
    }

    /// OSC 52 from a pane (input_osc_52): with set-clipboard on, the text goes to the
    /// terminals showing the pane and into a new buffer.
    pub fn pane_clipboard(&mut self, pane: PaneId, (sel, data): (String, String)) -> bool {
        if self.opt_server("set-clipboard") != "on" || data.is_empty() || data == "?" {
            return false;
        }
        let Ok(bytes) = decode_stdin_buffer(&format!("{STDIN_BUFFER_PREFIX}{data}")) else { return false };
        if bytes.is_empty() {
            return false;
        }
        let mut clip = String::new();
        for c in sel.chars().filter(|c| "cpqs01234567".contains(*c)) {
            if !clip.contains(c) {
                clip.push(c);
            }
        }
        let wid = self.window_of(pane);
        let seq = format!("\x1b]52;{clip};{}\x07", cmds::base64(&bytes));
        for c in self.clients.values().filter(|c| c.attached && c.control == 0 && c.features.has("clipboard")) {
            if c.session.and_then(|s| self.session_window(s)).is_some_and(|(_, w)| Some(w) == wid) {
                c.send(S_OUTPUT, seq.as_bytes());
            }
        }
        self.add_buffer(None, bytes, false);
        true
    }

    // ---- monitors ----------------------------------------------------------------------

    /// Monitors live with their option (hooks_monitor_free): those whose option was
    /// unset or whose session, window or pane went are dropped, and so is the fire
    /// count of hooks on objects that went.
    pub fn prune_monitors(&mut self) {
        let keys: Vec<(Level, String)> = self.ev.monitors.keys().cloned().collect();
        for key in keys {
            if self.level_store(key.0).and_then(|s| s.get(&key.1)).is_none() {
                self.ev.monitors.remove(&key);
            }
        }
        let gone: Vec<(Level, String)> = self.ev.fired.keys().filter(|k| self.level_store(k.0).is_none()).cloned().collect();
        for k in gone {
            self.ev.fired.remove(&k);
        }
    }

    /// Checks every set-hook -B monitor (once a second, monitor_timer).
    pub fn check_monitors(&mut self) {
        self.prune_monitors();
        // monitor_timer: session monitors, single panes and windows, every pane, every
        // window; by name within each
        let group = |w: &MonitorWhat| match w {
            MonitorWhat::Session => 0,
            MonitorWhat::Pane(_) | MonitorWhat::Window(_) => 1,
            MonitorWhat::AllPanes => 2,
            MonitorWhat::AllWindows => 3,
        };
        let mut keys: Vec<(u8, (Level, String))> = self.ev.monitors.iter().map(|(k, m)| (group(&m.what), k.clone())).collect();
        keys.sort_by(|a, b| (a.0, &a.1 .1).cmp(&(b.0, &b.1 .1)));
        let keys: Vec<(Level, String)> = keys.into_iter().map(|k| k.1).collect();
        for key in keys {
            let Some(m) = self.ev.monitors.get(&key) else { continue };
            // monitor_get_session: its own session, else the first session by name
            let sid = match m.session {
                Some(s) => self.sessions.contains_key(&s).then_some(s),
                None => self.sessions.values().min_by(|a, b| a.name.cmp(&b.name)).map(|s| s.id),
            };
            let Some(sid) = sid else { continue };
            let (what, format) = (m.what.clone(), m.format.clone());
            let s = &self.sessions[&sid];
            // (key, target, winlink, pane)
            let mut targets: Vec<((u32, u32), Target, Option<u32>, Option<PaneId>)> = Vec::new();
            match what {
                MonitorWhat::Session => targets.push(((u32::MAX, u32::MAX), self.target_for_session(sid), None, None)),
                MonitorWhat::Pane(id) => {
                    if let Some(wid) = self.panes.contains_key(&id).then(|| self.window_of(id)).flatten() {
                        for (idx, w) in &s.links {
                            if *w == wid {
                                targets.push(((id, *idx), Target { sid: Some(sid), idx: Some(*idx), wid: Some(wid), pane: Some(id) }, Some(*idx), Some(id)));
                            }
                        }
                    }
                }
                MonitorWhat::Window(id) => {
                    for (idx, w) in &s.links {
                        if *w == id {
                            targets.push(((id, *idx), self.target_for_winlink(sid, *idx), Some(*idx), None));
                        }
                    }
                }
                MonitorWhat::AllPanes => {
                    for (idx, wid) in &s.links {
                        for p in self.windows[wid].panes() {
                            targets.push(((p, *idx), Target { sid: Some(sid), idx: Some(*idx), wid: Some(*wid), pane: Some(p) }, Some(*idx), Some(p)));
                        }
                    }
                }
                MonitorWhat::AllWindows => {
                    for idx in s.links.keys() {
                        targets.push(((s.links[idx], *idx), self.target_for_winlink(sid, *idx), Some(*idx), None));
                    }
                }
            }
            let mut fire = Vec::new();
            let mut seen_keys = HashSet::new();
            for (k, t, idx, pane) in targets {
                seen_keys.insert(k);
                // monitor_check_pane skips a pane without a process (wp->fd == -1)
                if pane.is_some_and(|p| self.panes.get(&p).is_none_or(|p| p.dead.is_some() || p.pid.is_none())) {
                    continue;
                }
                let value = self.expand(t, None, &format);
                let m = self.ev.monitors.get_mut(&key).unwrap();
                match m.last.insert(k, value.clone()) {
                    // the first value is only recorded
                    None => {}
                    Some(last) if last == value => {}
                    Some(last) => {
                        if !m.only_true || cmds::truthy(&value) {
                            m.fire_count += 1;
                            m.fire_time = now_secs();
                            fire.push((t, idx, pane, value, last));
                        }
                    }
                }
            }
            if matches!(what, MonitorWhat::AllPanes | MonitorWhat::AllWindows) {
                if let Some(m) = self.ev.monitors.get_mut(&key) {
                    m.last.retain(|k, _| seen_keys.contains(k));
                }
            }
            for (t, idx, pane, value, last) in fire {
                let mut p = Payload { target: t, monitor: Some(key.clone()), ..Payload::default() };
                p.str("value", value).str("last", last);
                p.set("session", self.val_session(sid));
                if let (Some(i), Some(w)) = (idx, t.wid) {
                    p.set("window", self.val_window(w)).int("window_index", i as i64);
                }
                if let Some(pp) = pane {
                    p.set("pane", Val::Pane(pp));
                }
                self.fire(&key.1, p);
            }
        }
    }

    // ---- change detection --------------------------------------------------------------

    /// session-window-changed for sessions whose current window changed.
    /// session-window-changed when a session's current winlink changed. After
    /// swap-window the winlink is the same with another window in it: no event.
    pub(super) fn detect_current_window(&mut self, sid: SessId, swapped: bool) {
        let Some(s) = self.sessions.get(&sid) else { return };
        let Some(&wid) = s.links.get(&s.active) else { return };
        let now = (s.active, wid);
        let Some(old) = self.ev.seen.sessions.get(&sid).map(|x| x.active).filter(|o| *o != now) else { return };
        if let Some(x) = self.ev.seen.sessions.get_mut(&sid) {
            x.active = now;
        }
        if old == (0, 0) || (swapped && old.0 == now.0) {
            return;
        }
        let mut p = self.payload_winlink(sid, now.0, now.1);
        p.set("new_window", self.val_window(now.1)).int("new_window_index", now.0 as i64);
        p.set("old_window", self.val_window(old.1)).int("old_window_index", old.0 as i64);
        // session_set_current updates the old and new windows' focus first, if
        // focus-events is on
        if options::is_on(&self.opt_server("focus-events")) {
            self.update_window_focus(old.1);
            self.update_window_focus(now.1);
        }
        self.fire("session-window-changed", p);
    }

    /// window-created for a new window as it is first linked (spawn_window).
    pub(super) fn detect_window_created(&mut self, wid: WinId) {
        if self.ev.seen.windows.contains_key(&wid) {
            return;
        }
        let Some(w) = self.windows.get(&wid) else { return };
        let seen = WinSeen { name: w.name.clone(), active: w.active, zoomed: w.zoomed, size: (w.sx, w.sy), layout: w.layout.to_layout_string(self.window_area(wid)) };
        self.ev.seen.windows.insert(wid, seen);
        self.fire_window("window-created", wid);
    }

    /// window-unlinked, and window-closed once its last link went (window_remove_ref).
    pub(crate) fn fire_unlinked(&mut self, p: Payload, wid: WinId) {
        self.fire("window-unlinked", p);
        let linked = self.ev.seen.sessions.values().any(|s| s.links.values().any(|w| *w == wid));
        if !linked && !self.windows.contains_key(&wid) && self.ev.seen.windows.contains_key(&wid) {
            let mut p = Payload::default();
            p.set("window", self.val_window(wid));
            self.fire("window-closed", p);
            self.ev.seen.windows.remove(&wid);
            self.ev.seen.panes.retain(|_, s| s.wid != wid);
            self.ev.latest.remove(&wid);
        }
    }

    /// Session by session (in name order, as tmux walks them): the current window,
    /// windows unlinked (and closed), windows created and linked. link-window and
    /// move-window link before selecting (server_link_window); elsewhere the current
    /// window changes first (session_detach, spawn_window). swap-window swaps the
    /// windows inside the winlinks: nothing is linked or unlinked.
    fn detect_links(&mut self, command: Option<&str>) {
        let links_first = matches!(command, Some("link-window" | "move-window"));
        let swapped = command == Some("swap-window");
        let synced = std::mem::take(&mut self.ev.synced);
        let mut sids: Vec<(String, SessId)> = self.sessions.values().map(|s| (s.name.clone(), s.id)).collect();
        sids.sort_unstable();
        let followers: HashSet<SessId> = synced.iter().flat_map(|s| s.1.iter().copied()).collect();
        for (_, sid) in sids {
            if followers.contains(&sid) {
                continue;
            }
            self.detect_session_links(sid, links_first, swapped);
            // the rest of its group follow it
            for (_, others) in synced.iter().filter(|s| s.0 == sid) {
                for &o in others {
                    self.detect_group_sync(o);
                }
            }
        }
        // followers whose leader went
        for (leader, others) in &synced {
            if !self.sessions.contains_key(leader) {
                for &o in others {
                    self.detect_group_sync(o);
                }
            }
        }
        // windows not linked anywhere
        let mut wids: Vec<WinId> = self.windows.keys().copied().collect();
        wids.sort_unstable();
        for wid in wids {
            self.detect_window_created(wid);
        }
    }

    fn detect_session_links(&mut self, sid: SessId, links_first: bool, swapped: bool) {
        let Some(s) = self.sessions.get(&sid) else { return };
        let links = s.links.clone();
        let name = s.name.clone();
        let e = self.ev.seen.sessions.entry(sid).or_default();
        let new_session = e.links.is_empty();
        let old = std::mem::replace(&mut e.links, links.clone());
        e.name = name;
        if !links_first && !new_session {
            self.detect_current_window(sid, swapped);
        }
        // server_link_window unlinks a window it replaces (-k) first; move-window
        // unlinks the source only after linking and selecting
        let unlinked: Vec<(u32, WinId)> = if swapped {
            Vec::new()
        } else {
            old.iter().filter(|(i, w)| links.get(i) != Some(w)).map(|(i, w)| (*i, *w)).collect()
        };
        let (replaced, rest): (Vec<_>, Vec<_>) = unlinked.into_iter().partition(|(i, _)| !links_first || links.contains_key(i));
        for (idx, wid) in replaced {
            let p = self.payload_winlink(sid, idx, wid);
            self.fire_unlinked(p, wid);
        }
        for (idx, wid) in &links {
            self.detect_window_created(*wid);
            if !swapped && old.get(idx) != Some(wid) {
                self.fire_winlink("window-linked", sid, *idx, *wid);
            }
        }
        if new_session {
            // no session-window-changed for the first window
            self.seen_active_now(sid);
        } else if links_first {
            self.detect_current_window(sid, swapped);
        }
        for (idx, wid) in rest {
            let p = self.payload_winlink(sid, idx, wid);
            self.fire_unlinked(p, wid);
        }
    }

    pub(super) fn seen_active_now(&mut self, sid: SessId) {
        let active = self.sessions.get(&sid).and_then(|s| s.links.get(&s.active).map(|w| (s.active, *w))).unwrap_or_default();
        if let Some(e) = self.ev.seen.sessions.get_mut(&sid) {
            e.active = active;
        }
    }

    /// session_group_synchronize1: a new current window if the old one went, every
    /// window linked again, then the windows no longer there unlinked.
    pub(super) fn detect_group_sync(&mut self, sid: SessId) {
        let Some(s) = self.sessions.get(&sid) else { return };
        let links = s.links.clone();
        let name = s.name.clone();
        let e = self.ev.seen.sessions.entry(sid).or_default();
        let new_session = e.links.is_empty();
        let old = std::mem::replace(&mut e.links, links.clone());
        e.name = name;
        let old_active = e.active;
        if !new_session && !links.contains_key(&old_active.0) {
            self.detect_current_window(sid, false);
        }
        for (idx, wid) in &links {
            self.detect_window_created(*wid);
            self.fire_winlink("window-linked", sid, *idx, *wid);
        }
        for (idx, wid) in &old {
            if !links.values().any(|w| w == wid) {
                let p = self.payload_winlink(sid, *idx, *wid);
                self.fire_unlinked(p, *wid);
            }
        }
        self.seen_active_now(sid);
    }

    /// Fires the events for what changed since the last check: windows created,
    /// linked, renamed, resized, zoomed or closed, active panes and windows, pane
    /// modes, sizes, moves and focus, and session groups. `command` is the command
    /// that ran, if any.
    pub fn detect_changes(&mut self, command: Option<&str>) {
        // panes resized as layouts were fixed come first
        let mut panes: Vec<(PaneId, WinId)> = Vec::new();
        let mut all: Vec<WinId> = self.windows.keys().copied().collect();
        all.sort_unstable();
        for &wid in &all {
            if let Some(w) = self.windows.get(&wid) {
                panes.extend(w.panes().into_iter().map(|p| (p, wid)));
            }
        }
        let live: HashSet<PaneId> = panes.iter().map(|p| p.0).collect();
        self.ev.seen.panes.retain(|p, _| live.contains(p));
        self.ev.focused.retain(|p| live.contains(p));
        let mut pane_changes: Vec<(PaneId, WinId, Option<u32>, String, PaneSeen)> = Vec::new();
        for (pane, wid) in panes {
            let Some(pp) = self.panes.get(&pane) else { continue };
            let size = pp.size;
            let mode = self.expand(Target { pane: Some(pane), wid: Some(wid), ..Target::default() }, None, "#{pane_mode}");
            let idx = self.session_of_pane(pane).map(|x| x.1);
            let Some(old) = self.ev.seen.panes.insert(pane, PaneSeen { wid, idx, size, mode: mode.clone(), below: Vec::new() }) else {
                // a new pane starts at the window's size (window_pane_create) and is
                // resized as it is laid out
                let new = self.ev.spawned.iter().any(|s| s.pane == pane && !s.respawn);
                let wsize = self.windows.get(&wid).map(|w| (w.sx, w.sy)).unwrap_or(size);
                if new && wsize != size {
                    let mut p = self.payload_pane(pane);
                    p.int("width", size.0 as i64).int("height", size.1 as i64).int("old_width", wsize.0 as i64).int("old_height", wsize.1 as i64);
                    self.fire("pane-resized", p);
                }
                continue;
            };
            if old.size != size {
                let mut p = self.payload_pane(pane);
                p.int("width", size.0 as i64).int("height", size.1 as i64).int("old_width", old.size.0 as i64).int("old_height", old.size.1 as i64);
                self.fire("pane-resized", p);
            }
            pane_changes.push((pane, wid, idx, mode, old));
        }
        // panes spawned since: pane-created once they are in a window
        let spawned = std::mem::take(&mut self.ev.spawned);
        for sp in spawned {
            if !self.panes.contains_key(&sp.pane) {
                continue;
            }
            if self.window_of(sp.pane).is_none() {
                continue; // a popup
            }
            let mut p = self.payload_pane(sp.pane);
            let t = p.target;
            // spawn_window names the window after spawn_pane fired pane-created
            if let Some(w) = t.wid.filter(|w| !sp.respawn && !self.ev.seen.windows.contains_key(w)) {
                p.set("window", Val::Window(w, String::new()));
            }
            if let Some(s) = t.sid {
                p.set("session", self.val_session(s));
            }
            if let Some(i) = t.idx {
                p.int("window_index", i as i64);
            }
            if !sp.command.is_empty() {
                p.str("pane_command", sp.command);
            }
            p.str("pane_current_path", sp.cwd).int("created_empty", sp.empty as i64).int("created_respawn", sp.respawn as i64);
            self.fire("pane-created", p);
        }
        self.detect_groups();
        self.detect_links(command);
        let mut wids: Vec<WinId> = self.windows.keys().copied().collect();
        wids.sort_unstable();
        // sessions gone: their links went too
        let gone: Vec<SessId> = self.ev.seen.sessions.keys().copied().filter(|s| !self.sessions.contains_key(s)).collect();
        for sid in gone {
            // session_destroy: session-closed, then the group, then the unlinks
            if let Some(i) = self.ev.closed.iter().position(|c| c.0 == sid) {
                let (_, p) = self.ev.closed.remove(i);
                self.fire("session-closed", p);
            }
            // session_group_remove fires before the session leaves the group
            let size = self.ev.seen.sessions[&sid].group.as_ref().map(|g| self.seen_group_size(g.0));
            let seen = self.ev.seen.sessions.remove(&sid).unwrap();
            if let (Some((_, g)), Some(size)) = (&seen.group, size) {
                let mut p = Payload::default();
                p.set("session", Val::Session(sid, seen.name.clone())).str("group", g.clone()).int("group_size", size as i64);
                self.fire("session-removed-from-group", p);
            }
            for (idx, wid) in seen.links {
                let mut p = Payload::default();
                p.set("session", Val::Session(sid, seen.name.clone())).set("window", self.val_window(wid)).int("window_index", idx as i64);
                self.fire_unlinked(p, wid);
            }
        }
        // sessions that came and went unseen
        for (_, p) in std::mem::take(&mut self.ev.closed) {
            self.fire("session-closed", p);
        }
        // windows closed
        let closed: Vec<WinId> = self.ev.seen.windows.keys().copied().filter(|w| !self.windows.contains_key(w)).collect();
        for wid in closed {
            let mut p = Payload::default();
            p.set("window", self.val_window(wid));
            self.fire("window-closed", p);
            self.ev.seen.windows.remove(&wid);
            self.ev.seen.panes.retain(|_, s| s.wid != wid);
            self.ev.latest.remove(&wid);
        }
        // then the active pane changes, pane moves, the window events and modes
        let mut changed: Vec<(WinId, WinSeen, WinSeen)> = Vec::new();
        for &wid in &wids {
            let Some(w) = self.windows.get(&wid) else { continue };
            let now = WinSeen { name: w.name.clone(), active: w.active, zoomed: w.zoomed, size: (w.sx, w.sy), layout: w.layout.to_layout_string(self.window_area(wid)) };
            let Some(old) = self.ev.seen.windows.get(&wid).cloned() else { continue };
            if old != now {
                self.ev.seen.windows.insert(wid, now.clone());
                changed.push((wid, old, now));
            }
        }
        // window_set_active_pane updates the old and new panes' focus before
        // window-pane-changed, if focus-events is on
        let focus_events = options::is_on(&self.opt_server("focus-events"));
        for (wid, old, now) in &changed {
            if old.active != now.active && self.windows.get(wid).is_some_and(|w| w.active == now.active) {
                if focus_events {
                    self.update_pane_focus(&[old.active, now.active]);
                }
                let mut p = Payload { target: self.target_for_pane(now.active), ..Payload::default() };
                p.set("window", self.val_window(*wid)).set("pane", Val::Pane(now.active)).set("new_pane", Val::Pane(now.active)).set("old_pane", Val::Pane(old.active));
                self.fire("window-pane-changed", p);
            }
        }
        // panes moved to another window (break-pane, join-pane, swap-pane)
        let order = std::mem::take(&mut self.ev.pane_move_order);
        let mut moved: Vec<_> = pane_changes.iter().collect();
        moved.sort_by_key(|(pane, ..)| order.iter().position(|p| p == pane).unwrap_or(order.len()));
        for (pane, wid, idx, _, old) in moved {
            let (pane, wid, idx) = (*pane, *wid, *idx);
            if old.wid != wid {
                let mut p = self.payload_pane(pane);
                p.set("window", self.val_window(wid)).set("new_window", self.val_window(wid)).set("old_window", self.val_window(old.wid));
                if let Some(i) = old.idx {
                    p.int("old_window_index", i as i64);
                }
                if let Some(i) = idx {
                    p.int("window_index", i as i64).int("new_window_index", i as i64);
                }
                self.fire("pane-moved", p);
            }
        }
        let mut layout_seen = Vec::new();
        for (wid, old, now) in changed {
            if old.name != now.name {
                let mut p = self.payload_window(wid);
                p.str("old_name", old.name.clone()).str("new_name", now.name.clone());
                self.fire("window-renamed", p);
            }
            // resizing a zoomed window unzooms it and zooms it again (resize_window)
            let rezoom = old.zoomed && now.zoomed && old.size != now.size;
            if rezoom {
                self.fire_window("window-unzoomed", wid);
                let mut p = self.payload_window(wid);
                p.str("_unzoomed_layout", old.layout.clone());
                self.fire("window-layout-changed", p);
                self.fire_window("window-zoomed", wid);
                self.fire_window("window-layout-changed", wid);
            }
            if old.zoomed != now.zoomed {
                self.fire_window(if now.zoomed { "window-zoomed" } else { "window-unzoomed" }, wid);
            }
            // rotate-window does not fire it
            if (old.layout != now.layout || old.zoomed != now.zoomed || old.size != now.size) && command != Some("rotate-window") {
                self.fire_window("window-layout-changed", wid);
                layout_seen.push(wid);
            }
            if old.size != now.size {
                let mut p = self.payload_window(wid);
                p.int("width", now.size.0 as i64).int("height", now.size.1 as i64)
                    .int("old_width", old.size.0 as i64).int("old_height", old.size.1 as i64);
                self.fire("window-resized", p);
            }
        }
        // commands that fire it themselves as well (select-layout, resize-pane)
        for (wid, n) in std::mem::take(&mut self.ev.layout_fired) {
            let n = if layout_seen.contains(&wid) { n - 1 } else { n };
            for _ in 0..n {
                if self.windows.contains_key(&wid) {
                    self.fire_window("window-layout-changed", wid);
                }
            }
        }
        for (pane, _, _, mode, old) in pane_changes {
            let mut below = old.below.clone();
            // window_pane_reset_mode pops back to the mode under it: exited, with the
            // next mode current; anything else on top of a mode was pushed
            let entered = match (old.mode.is_empty(), mode.is_empty()) {
                _ if old.mode == mode => false,
                (_, true) => {
                    below.clear();
                    false
                }
                (true, false) => true,
                (false, false) if below.last() == Some(&mode) => {
                    below.pop();
                    false
                }
                (false, false) => {
                    below.push(old.mode.clone());
                    true
                }
            };
            if let Some(seen) = self.ev.seen.panes.get_mut(&pane) {
                seen.below = below;
            }
            if old.mode != mode {
                let mut p = self.payload_pane(pane);
                if !mode.is_empty() {
                    p.str("current_mode", mode.clone());
                }
                if !old.mode.is_empty() {
                    p.str("previous_mode", old.mode.clone());
                }
                p.int("mode_entered", entered as i64);
                self.fire(if entered { "pane-mode-entered" } else { "pane-mode-exited" }, p.clone());
                self.fire("pane-mode-changed", p);
            }
        }
        // focus changes where tmux updates it: active panes and windows, overlays,
        // attaching and focus reports (not when a client goes)
        let overlays: HashMap<ClientId, bool> = self.clients.values().map(|c| (c.id, c.overlay.is_some())).collect();
        // server_client_set_overlay / clear_overlay: the client's current window
        let mut windows = Vec::new();
        for (c, o) in &overlays {
            if self.ev.overlays.get(c) != Some(o) && (*o || self.ev.overlays.contains_key(c)) {
                windows.extend(self.client_session(*c).and_then(|s| self.session_window(s)).map(|w| w.1));
            }
        }
        self.ev.overlays = overlays;
        windows.sort_unstable();
        windows.dedup();
        for w in windows {
            self.update_window_focus(w);
        }
        if std::mem::take(&mut self.ev.focus_dirty) {
            self.update_focus();
        }
    }

    /// Events for a window's layout as it is now, in the middle of a command (tmux
    /// fires them where the layout changes, before what the command does next):
    /// pane-resized for its panes (layout_fix_panes), window-zoomed/unzoomed and
    /// window-layout-changed. detect_changes reports the rest of the command.
    pub fn flush_window_layout(&mut self, wid: WinId) {
        let Some(w) = self.windows.get(&wid) else { return };
        let (zoomed, layout) = (w.zoomed, w.layout.to_layout_string(self.window_area(wid)));
        for pane in w.panes() {
            let Some(size) = self.panes.get(&pane).map(|p| p.size) else { continue };
            let Some(seen) = self.ev.seen.panes.get_mut(&pane).filter(|s| s.wid == wid) else { continue };
            if seen.size != size {
                let old = std::mem::replace(&mut seen.size, size);
                let mut p = self.payload_pane(pane);
                p.int("width", size.0 as i64).int("height", size.1 as i64).int("old_width", old.0 as i64).int("old_height", old.1 as i64);
                self.fire("pane-resized", p);
            }
        }
        let Some(seen) = self.ev.seen.windows.get_mut(&wid) else { return };
        let zoom_changed = seen.zoomed != zoomed;
        let changed = zoom_changed || seen.layout != layout;
        seen.zoomed = zoomed;
        seen.layout = layout;
        if zoom_changed {
            self.fire_window(if zoomed { "window-zoomed" } else { "window-unzoomed" }, wid);
        }
        if changed {
            self.fire_window("window-layout-changed", wid);
        }
    }

    /// window_unzoom with notify (server_unzoom_window, window_push_zoom): its events
    /// come before those of the command that unzooms. Whether it was zoomed.
    pub fn unzoom_window(&mut self, wid: WinId) -> bool {
        let Some(w) = self.windows.get_mut(&wid).filter(|w| w.zoomed) else { return false };
        w.zoomed = false;
        self.apply_layout(wid);
        self.flush_window_layout(wid);
        true
    }

    /// window-linked now, for a command that links a window before its other events
    /// (session_attach in break-pane).
    pub fn link_now(&mut self, sid: SessId, idx: u32, wid: WinId) {
        let Some(e) = self.ev.seen.sessions.get_mut(&sid) else { return };
        if e.links.insert(idx, wid) != Some(wid) {
            self.fire_winlink("window-linked", sid, idx, wid);
        }
    }

    /// pane-resized for one pane now (window_pane_resize).
    pub fn pane_resized_now(&mut self, pane: PaneId) {
        let Some(size) = self.panes.get(&pane).map(|p| p.size) else { return };
        let Some(seen) = self.ev.seen.panes.get_mut(&pane) else { return };
        if seen.size == size {
            return;
        }
        let old = std::mem::replace(&mut seen.size, size);
        let mut p = self.payload_pane(pane);
        p.int("width", size.0 as i64).int("height", size.1 as i64).int("old_width", old.0 as i64).int("old_height", old.1 as i64);
        self.fire("pane-resized", p);
    }

    /// pane-moved now (window_fire_pane_moved) for a pane that went to another window.
    pub fn pane_moved_now(&mut self, pane: PaneId) {
        let Some(wid) = self.window_of(pane) else { return };
        let idx = self.session_of_pane(pane).map(|x| x.1);
        let Some(old) = self.ev.seen.panes.get(&pane).cloned().filter(|s| s.wid != wid) else { return };
        if let Some(seen) = self.ev.seen.panes.get_mut(&pane) {
            seen.wid = wid;
            seen.idx = idx;
        }
        let mut p = self.payload_pane(pane);
        p.set("window", self.val_window(wid)).set("new_window", self.val_window(wid)).set("old_window", self.val_window(old.wid));
        if let Some(i) = old.idx {
            p.int("old_window_index", i as i64);
        }
        if let Some(i) = idx {
            p.int("window_index", i as i64).int("new_window_index", i as i64);
        }
        self.fire("pane-moved", p);
    }

    /// Records a window's layout as reported, for a command that fires its
    /// window-layout-changed itself.
    pub fn seen_layout_now(&mut self, wid: WinId) {
        let Some(w) = self.windows.get(&wid) else { return };
        let (zoomed, layout) = (w.zoomed, w.layout.to_layout_string(self.window_area(wid)));
        if let Some(seen) = self.ev.seen.windows.get_mut(&wid) {
            seen.zoomed = zoomed;
            seen.layout = layout;
        }
    }

    /// window-layout-changed now for a command that fires it itself.
    pub fn layout_changed_now(&mut self, wid: WinId) {
        if self.windows.contains_key(&wid) {
            self.seen_layout_now(wid);
            self.fire_window("window-layout-changed", wid);
        }
    }

    /// window_pop_zoom: zooms the (new) active pane again.
    pub fn rezoom_window(&mut self, wid: WinId) {
        let Some(w) = self.windows.get_mut(&wid).filter(|w| !w.zoomed && w.panes().len() > 1) else { return };
        w.zoomed = true;
        self.apply_layout(wid);
        self.flush_window_layout(wid);
    }

    /// window-pane-changed now if the active pane changed (window_set_active_pane,
    /// which updates focus first if focus-events is on).
    pub fn flush_active_pane(&mut self, wid: WinId) {
        let Some(active) = self.windows.get(&wid).map(|w| w.active) else { return };
        let Some(old) = self.ev.seen.windows.get(&wid).map(|w| w.active).filter(|a| *a != active) else { return };
        if let Some(seen) = self.ev.seen.windows.get_mut(&wid) {
            seen.active = active;
        }
        if options::is_on(&self.opt_server("focus-events")) {
            self.update_pane_focus(&[old, active]);
        }
        let mut p = Payload { target: self.target_for_pane(active), ..Payload::default() };
        p.set("window", self.val_window(wid)).set("pane", Val::Pane(active)).set("new_pane", Val::Pane(active)).set("old_pane", Val::Pane(old));
        self.fire("window-pane-changed", p);
    }

    /// Sessions joining and leaving groups (session_group_add/remove), oldest
    /// session first; a new session joins before its windows are synchronized.
    pub(super) fn detect_groups(&mut self) {
        let mut by_age: Vec<(u64, SessId)> = self.sessions.values().map(|s| (s.created, s.id)).collect();
        by_age.sort_unstable();
        for (_, sid) in by_age {
            let Some(s) = self.sessions.get(&sid) else { continue };
            let group = s.group.map(|g| (g, self.group_name(g)));
            let old = self.ev.seen.sessions.entry(sid).or_default().group.clone();
            if group.as_ref().map(|g| g.0) == old.as_ref().map(|g| g.0) {
                continue;
            }
            if let Some((gid, name)) = old {
                // pwsh-tmux dissolves a group left with one session; tmux keeps it
                let dissolved = group.is_none() && !self.sessions.values().any(|s| s.group == Some(gid));
                if !dissolved {
                    let size = self.seen_group_size(gid);
                    let mut p = self.payload_session(sid);
                    p.str("group", name).int("group_size", size as i64);
                    self.fire("session-removed-from-group", p);
                }
            }
            if let Some(e) = self.ev.seen.sessions.get_mut(&sid) {
                e.group = group.clone();
            }
            if let Some((gid, name)) = group {
                let size = self.seen_group_size(gid);
                let mut p = self.payload_session(sid);
                p.str("group", name).int("group_size", size as i64);
                self.fire("session-added-to-group", p);
            }
        }
    }

    fn seen_group_size(&self, gid: u32) -> usize {
        self.ev.seen.sessions.values().filter(|x| x.group.as_ref().is_some_and(|g| g.0 == gid)).count()
    }

    pub(super) fn group_name(&self, gid: u32) -> String {
        if let Some((_, name)) = self.ev.seen.sessions.values().filter_map(|s| s.group.as_ref()).find(|g| g.0 == gid) {
            return name.clone();
        }
        self.sessions.values().filter(|s| s.group == Some(gid)).min_by_key(|s| (s.created, s.id)).map(|s| s.name.clone()).unwrap_or_default()
    }

    /// window_pane_update_focus for every window: the active pane of a window shown
    /// to a focused client (without an overlay) has focus; changes are sent to panes
    /// that asked for focus events and fire pane-focus-in/out.
    pub fn update_focus(&mut self) {
        let mut focused = HashSet::new();
        for c in self.clients.values() {
            if !c.attached || c.overlay.is_some() || self.ev.unfocused.contains(&c.id) {
                continue;
            }
            let Some((_, wid)) = c.session.and_then(|s| self.session_window(s)) else { continue };
            if let Some(w) = self.windows.get(&wid) {
                focused.insert(w.active);
            }
        }
        let out: Vec<PaneId> = self.ev.focused.difference(&focused).copied().collect();
        let inn: Vec<PaneId> = focused.difference(&self.ev.focused).copied().collect();
        self.ev.focused = focused;
        for (list, seq, name) in [(out, "\x1b[O", "pane-focus-out"), (inn, "\x1b[I", "pane-focus-in")] {
            for p in list {
                let Some(pane) = self.panes.get(&p) else { continue };
                if pane.screen().focus_events() {
                    pane.send(seq.as_bytes().to_vec());
                }
                self.fire_pane(name, p);
            }
        }
    }

    /// Whether a pane should have focus: the active pane of a window that is current
    /// for a focused client without an overlay.
    fn pane_should_focus(&self, pane: PaneId) -> bool {
        let Some(wid) = self.window_of(pane) else { return false };
        if self.windows.get(&wid).is_none_or(|w| w.active != pane) {
            return false;
        }
        self.clients.values().any(|c| {
            c.attached && c.overlay.is_none() && !self.ev.unfocused.contains(&c.id)
                && c.session.and_then(|s| self.session_window(s)).is_some_and(|(_, w)| w == wid)
        })
    }

    /// window_pane_update_focus for these panes only: other panes keep the focus
    /// they had, even if stale (tmux updates only the windows it is told about).
    pub fn update_pane_focus(&mut self, panes: &[PaneId]) {
        for &p in panes {
            if !self.panes.contains_key(&p) {
                continue;
            }
            let focused = self.pane_should_focus(p);
            let (seq, name) = match (self.ev.focused.contains(&p), focused) {
                (true, false) => ("\x1b[O", "pane-focus-out"),
                (false, true) => ("\x1b[I", "pane-focus-in"),
                _ => continue,
            };
            if focused {
                self.ev.focused.insert(p);
            } else {
                self.ev.focused.remove(&p);
            }
            if let Some(pane) = self.panes.get(&p).filter(|x| x.screen().focus_events()) {
                pane.send(seq.as_bytes().to_vec());
            }
            self.fire_pane(name, p);
        }
    }

    /// window_update_focus: the window's active pane.
    pub fn update_window_focus(&mut self, wid: WinId) {
        if let Some(a) = self.windows.get(&wid).map(|w| w.active) {
            self.update_pane_focus(&[a]);
        }
    }

    /// server_client_update_latest: a client typing or resized becomes its window's
    /// latest client.
    pub fn update_latest(&mut self, cid: ClientId) {
        let Some((_, wid)) = self.client_session(cid).and_then(|s| self.session_window(s)) else { return };
        if self.ev.latest.insert(wid, cid) != Some(cid) {
            self.fire_client("client-active", cid);
        }
    }

    /// A client's focus report (only sent with focus-events on).
    pub fn client_focus(&mut self, cid: ClientId, focus: bool) {
        // tty_keys: the client's current window
        let wid = self.client_session(cid).and_then(|s| self.session_window(s)).map(|w| w.1);
        if focus {
            self.ev.unfocused.remove(&cid);
            self.fire_client("client-focus-in", cid);
            if let Some(w) = wid {
                self.update_window_focus(w);
            }
        } else {
            self.ev.unfocused.insert(cid);
            if let Some(w) = wid {
                self.update_window_focus(w);
            }
            self.fire_client("client-focus-out", cid);
        }
    }

    /// The pane events of one read of output: activity, bell, title, OSC 133 and
    /// clipboard.
    pub fn pane_output_events(&mut self, pane: PaneId, events: Vec<OutputEvent>) {
        if self.window_of(pane).is_none() {
            return;
        }
        if self.ev.activity.insert(pane) {
            self.fire_pane("pane-activity", pane);
        }
        for event in events {
            let (name, st) = match event {
                OutputEvent::Title(t) => {
                    let mut p = self.payload_pane(pane);
                    p.str("new_title", t);
                    self.fire("pane-title-changed", p);
                    continue;
                }
                OutputEvent::Clipboard(data) => {
                    if self.pane_clipboard(pane, data) {
                        self.fire_pane("pane-set-clipboard", pane);
                    }
                    continue;
                }
                OutputEvent::Osc(name, st) => (name, st),
            };
            if name == "pane-shell-prompt" || name == "pane-bell" {
                self.fire_pane(name, pane);
                continue;
            }
            // input_fire_command_event
            let mut p = self.payload_pane(pane);
            let t = p.target;
            if let Some(s) = t.sid {
                p.set("session", self.val_session(s));
            }
            if let Some(i) = t.idx {
                p.int("window_index", i as i64);
            }
            if let Some(s) = st.cmd_status {
                p.int("command_status", s as i64);
            }
            if st.cmd_start != 0 {
                p.set("command_start_time", Val::Time(st.cmd_start));
            }
            if st.cmd_end != 0 {
                p.set("command_end_time", Val::Time(st.cmd_end));
            }
            if st.cmd_start != 0 {
                let end = if st.cmd_running { now_secs() } else { st.cmd_end }.max(st.cmd_start);
                p.int("command_duration", (end - st.cmd_start) as i64);
            }
            self.fire(name, p);
        }
    }

    /// Called by a pane prompt's owner on opening, replacing and closing it.
    /// Status-line prompts do not fire these events (window_fire_pane_prompt).
    #[allow(dead_code)]
    pub fn fire_pane_prompt(&mut self, name: &str, pane: PaneId, prompt_type: &str) {
        if self.window_of(pane).is_none() {
            return;
        }
        let mut p = self.payload_pane(pane);
        p.str("prompt_type", prompt_type);
        self.fire(name, p);
    }

    /// pane-exited, or pane-died when remain-on-exit keeps the pane.
    pub fn fire_pane_exit(&mut self, name: &str, pane: PaneId, status: Option<i32>) {
        let mut p = self.payload_pane(pane);
        // sig2name: the number where there are no signal names (Linux)
        match status {
            Some(s) => match super::exit_signal(s) {
                Some(sig) => {
                    p.str("exit_signal", sig.to_string());
                }
                None => {
                    p.int("exit_status", s as i64);
                }
            },
            None => {
                p.str("exit_signal", "");
            }
        }
        p.int("exit_success", (status == Some(0)) as i64);
        self.fire(name, p);
    }
}

pub static HOOK_MARK: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| internal_mark("hook"));

/// Marks hook commands run inside another queue: the first sets the hook's state
/// (no hooks, its target and formats), the last clears it. Each hook command list
/// is wrapped on its own, so an error ends only that list.
fn wrap_hook(cmds: VecDeque<Vec<String>>, t: Target, formats: &HashMap<String, String>) -> Vec<Vec<String>> {
    if cmds.is_empty() {
        return Vec::new();
    }
    let n = |v: Option<u32>| v.map(|v| v.to_string()).unwrap_or_default();
    let mut push = vec![HOOK_MARK.to_string(), "1".into(), n(t.pane), n(t.sid), n(t.idx), n(t.wid)];
    for (k, v) in formats {
        push.push(k.clone());
        push.push(v.clone());
    }
    let mut out = vec![push];
    out.extend(cmds);
    out.push(vec![HOOK_MARK.to_string(), "0".into()]);
    out
}

/// What a hook's open mark replaced: depth, pane, target, formats and the queue's
/// soft (config file) commands still to run outside the hook.
pub type HookSaved = Vec<(u8, Option<PaneId>, Option<Target>, Option<Arc<HashMap<String, String>>>, usize)>;

/// Whether a queue entry opens (Some(true)) or closes (Some(false)) a hook's commands.
pub fn hook_mark_kind(cmd: &[String]) -> Option<bool> {
    (cmd.first() == Some(&*HOOK_MARK)).then(|| cmd.get(1).map(String::as_str) == Some("1"))
}

/// Applies a hook mark to the queue's context, saving or restoring what it replaces.
/// Hook commands are items of their own (cmdq_insert_after), not lines of the file
/// they were inserted into: they take none of its soft slots.
pub fn apply_hook_mark(mark: &[String], ctx: &mut Ctx, saved: &mut HookSaved, soft: &mut usize) {
    if mark.get(1).map(String::as_str) == Some("1") {
        saved.push((ctx.hook_depth, ctx.pane, ctx.target, ctx.formats.clone(), std::mem::take(soft)));
        ctx.hook_depth = ctx.hook_depth.max(1);
        let n = |i: usize| mark.get(i).and_then(|p| p.parse::<u32>().ok());
        if let Some(p) = n(2) {
            ctx.pane = Some(p);
        }
        ctx.target = n(3).map(|sid| Target { sid: Some(sid), idx: n(4), wid: n(5), pane: n(2) });
        let f: HashMap<String, String> = mark.get(6..).unwrap_or_default().chunks(2).filter(|c| c.len() == 2).map(|c| (c[0].clone(), c[1].clone())).collect();
        ctx.formats = Some(Arc::new(f));
    } else if let Some((depth, pane, target, formats, outer)) = saved.pop() {
        *soft = outer;
        ctx.hook_depth = depth;
        ctx.pane = pane;
        ctx.target = target;
        ctx.formats = formats;
    }
}

fn level_is_window(l: Level) -> bool {
    matches!(l, Level::Window(_) | Level::GlobalWindow)
}

/// options_remove_or_default on a hook: a key removes that member, the whole hook
/// is emptied at a global level and removed elsewhere.
fn unset_hook(store: &mut Options, base: &str, key: Option<&str>, global: bool) {
    match key {
        Some(k) => {
            store.0.remove(&format!("{base}[{k}]"));
        }
        // a global level keeps the (empty) table hook: see level_holds
        None => {
            let _ = global;
            store.unset(base);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_specs() {
        assert_eq!(MonitorWhat::parse("@a::#{x}"), Some(("@a".into(), MonitorWhat::Session, "#{x}".into())));
        assert_eq!(MonitorWhat::parse("@a:%3:f:g"), Some(("@a".into(), MonitorWhat::Pane(3), "f:g".into())));
        assert_eq!(MonitorWhat::parse("@a:%*:f"), Some(("@a".into(), MonitorWhat::AllPanes, "f".into())));
        assert_eq!(MonitorWhat::parse("@a:@12x:f"), Some(("@a".into(), MonitorWhat::Window(12), "f".into())));
        assert_eq!(MonitorWhat::parse("@a:@*:f").map(|m| m.1.target()), Some("@*".into()));
        assert_eq!(MonitorWhat::parse("@a:zz:f"), None);
        assert_eq!(MonitorWhat::parse("@a:%-1:f"), None);
        assert_eq!(MonitorWhat::parse("bad"), None);
    }

    #[test]
    fn osc_events_in_order() {
        let mut p = vt100::Parser::new_with_callbacks(nz(24), nz(80), 0, PaneCb::default());
        p.process(b"\x1b]133;A\x07\x1b]133;C\x07out\x1b]133;D;7\x07\x07\x1b]52;c;aGk=\x07");
        let cb = p.callbacks_mut();
        let names: Vec<&str> = cb.events.iter().map(|e| match e { OutputEvent::Osc(name, _) => *name, OutputEvent::Title(_) => "title", OutputEvent::Clipboard(_) => "clipboard" }).collect();
        assert_eq!(names, ["pane-shell-prompt", "pane-command-started", "pane-command-finished", "pane-bell", "clipboard"]);
        let OutputEvent::Osc(_, started) = &cb.events[1] else { panic!("command start") };
        let OutputEvent::Osc(_, finished) = &cb.events[2] else { panic!("command end") };
        assert!(started.cmd_running && started.cmd_status.is_none());
        assert_eq!(finished.cmd_status, Some(7));
        assert!(!finished.cmd_running);
        assert_eq!(cb.clipboard, Some(("c".into(), "aGk=".into())));
    }

    #[test]
    fn hook_marks_save_and_restore_the_queue_state() {
        let mut ctx = Ctx { pane: Some(3), ..Ctx::default() };
        let cmds = VecDeque::from([vec!["display".to_string()]]);
        let t = Target { sid: Some(1), idx: Some(4), wid: Some(2), pane: Some(9) };
        let wrapped = wrap_hook(cmds, t, &HashMap::from([("hook".to_string(), "x".to_string())]));
        assert_eq!(wrapped.len(), 3);
        let mut saved = Vec::new();
        let mut soft = 4;
        apply_hook_mark(&wrapped[0], &mut ctx, &mut saved, &mut soft);
        assert_eq!((ctx.hook_depth, ctx.pane, soft), (1, Some(9), 0));
        assert_eq!(ctx.target.and_then(|t| t.idx), Some(4));
        assert_eq!(ctx.formats.as_ref().unwrap()["hook"], "x");
        apply_hook_mark(&wrapped[2], &mut ctx, &mut saved, &mut soft);
        assert_eq!((ctx.hook_depth, ctx.pane, soft), (0, Some(3), 4));
        assert!(ctx.formats.is_none() && ctx.target.is_none());
        // a forged mark (a parsed "\0hook" word) is not one, and a short one is no panic
        assert_eq!(hook_mark_kind(&["\0hook".to_string(), "1".to_string()]), None);
        apply_hook_mark(&[HOOK_MARK.to_string(), "1".to_string()], &mut ctx, &mut saved, &mut soft);
        assert_eq!(ctx.hook_depth, 1);
    }

    #[test]
    fn payload_formats_and_printing() {
        let mut p = Payload::default();
        p.set("session", Val::Session(2, "main".into())).set("window", Val::Window(5, "vim".into()))
            .set("pane", Val::Pane(7)).set("client", Val::Client(3, "client-3".into())).int("window_index", 1)
            .set("command_start_time", Val::Time(99));
        let f = p.formats("hook_");
        assert_eq!(f["hook_session"], "$2");
        assert_eq!(f["hook_session_name"], "main");
        assert_eq!(f["hook_window"], "@5");
        assert_eq!(f["hook_window_name"], "vim");
        assert_eq!(f["hook_pane"], "%7");
        assert_eq!(f["hook_client"], "client-3");
        assert_eq!(f["hook_window_index"], "1");
        assert_eq!(f["hook_command_start_time"], "99");
        assert!(!f.contains_key("hook_pane_name"));
        assert!(valid_event_name("@x") && valid_event_name("pane-exited") && valid_event_name("after-new-window"));
        assert!(!valid_event_name("foo") && !valid_event_name("after-has-session"));
        assert_eq!(stringify_argv(&["sleep 99".into()]), "\"sleep 99\"");
    }
}
