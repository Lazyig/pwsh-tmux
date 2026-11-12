//! Command table, target resolution and the implementation of every command.

use super::modes::{Menu, MenuItem, Popup, Tree, TreeKind};
use super::vars::Target;
use super::*;
use crate::cmdparse;
use crate::layout::{self, Dir};
use crate::opts::{getopt, Opts};

/// (name, alias, flags without value, flags with value)
const COMMANDS: &[(&str, &str, &str, &str)] = &[
    ("attach-session", "attach", "dErx", "cfst"),
    ("bind-key", "bind", "nr", "TN"),
    ("break-pane", "breakp", "abdP", "FnstL"),
    ("capture-pane", "capturep", "aCeJNpPqT", "bESt"),
    ("choose-buffer", "", "NrZ", "FfKOt"),
    ("choose-client", "", "NrZ", "FfKOt"),
    ("choose-tree", "", "GNrswZ", "FfKOt"),
    ("clear-history", "clearhist", "H", "t"),
    ("clear-prompt-history", "clearphist", "", "T"),
    ("clock-mode", "", "", "t"),
    ("command-prompt", "", "1bFkiN", "IpTt"),
    ("confirm-before", "confirm", "by", "cpt"),
    ("copy-mode", "", "eHMqu", "st"),
    ("customize-mode", "", "NZ", "Fft"),
    ("delete-buffer", "deleteb", "", "b"),
    ("detach-client", "detach", "aP", "Esct"),
    ("display-menu", "menu", "MO", "bcCHstSTxy"),
    ("display-message", "display", "aINpv", "cdt"),
    ("display-panes", "displayp", "bN", "dt"),
    ("display-popup", "popup", "BCEk", "bcdeHhsStTwxy"),
    ("find-window", "findw", "CiNrTZ", "t"),
    ("has-session", "has", "", "t"),
    ("if-shell", "if", "bF", "t"),
    ("join-pane", "joinp", "bdfhv", "lst"),
    ("kill-pane", "killp", "a", "t"),
    ("kill-server", "", "", ""),
    ("kill-session", "", "aC", "t"),
    ("kill-window", "killw", "a", "t"),
    ("last-pane", "lastp", "deZ", "t"),
    ("last-window", "last", "", "t"),
    ("link-window", "linkw", "abdk", "st"),
    ("list-buffers", "lsb", "", "Ff"),
    ("list-clients", "lsc", "", "Fft"),
    ("list-commands", "lscm", "", "F"),
    ("list-keys", "lsk", "1aN", "PT"),
    ("list-panes", "lsp", "as", "Fft"),
    ("list-sessions", "ls", "", "Ff"),
    ("list-windows", "lsw", "a", "Fft"),
    ("load-buffer", "loadb", "w", "bt"),
    ("lock-client", "lockc", "", "t"),
    ("lock-server", "lock", "", ""),
    ("lock-session", "locks", "", "t"),
    ("move-pane", "movep", "bdfhv", "lst"),
    ("move-window", "movew", "abdkr", "st"),
    ("new-session", "new", "AdDEPX", "cefFnstxy"),
    ("new-window", "neww", "abdkPS", "cefFnt"),
    ("next-layout", "nextl", "", "t"),
    ("next-window", "next", "a", "t"),
    ("paste-buffer", "pasteb", "dpr", "bst"),
    ("pipe-pane", "pipep", "IOo", "t"),
    ("previous-layout", "prevl", "", "t"),
    ("previous-window", "prev", "a", "t"),
    ("refresh-client", "refresh", "cDlLRSU", "ABCfFt"),
    ("rename-session", "rename", "", "t"),
    ("rename-window", "renamew", "", "t"),
    ("resize-pane", "resizep", "DLMRTUZ", "txy"),
    ("resize-window", "resizew", "aADLRU", "txy"),
    ("respawn-pane", "respawnp", "k", "cet"),
    ("respawn-window", "respawnw", "k", "cet"),
    ("rotate-window", "rotatew", "DUZ", "t"),
    ("run-shell", "run", "bC", "cdt"),
    ("save-buffer", "saveb", "a", "b"),
    ("select-layout", "selectl", "Enop", "t"),
    ("select-pane", "selectp", "DdegLlMmRUZ", "Tt"),
    ("select-window", "selectw", "lnpT", "t"),
    ("send-keys", "send", "FHKlMRX", "cNt"),
    ("send-prefix", "", "2", "t"),
    ("server-access", "", "adlrw", ""),
    ("set-buffer", "setb", "aw", "bnt"),
    ("set-environment", "setenv", "Fhgru", "t"),
    ("set-hook", "", "agpRuw", "t"),
    ("set-option", "set", "aFgopqsuUw", "t"),
    ("set-window-option", "setw", "aFgoqu", "t"),
    ("show-buffer", "showb", "", "b"),
    ("show-environment", "showenv", "hgs", "t"),
    ("show-hooks", "", "gpw", "t"),
    ("show-messages", "showmsgs", "JT", "t"),
    ("show-options", "show", "AgHpqsvw", "t"),
    ("show-prompt-history", "showphist", "", "T"),
    ("show-window-options", "showw", "gv", "t"),
    ("source-file", "source", "Fnqv", "t"),
    ("split-window", "splitw", "bdfhIvPZ", "cefFlt"),
    ("start-server", "start", "", ""),
    ("suspend-client", "suspendc", "", "t"),
    ("swap-pane", "swapp", "dDUZ", "st"),
    ("swap-window", "swapw", "d", "st"),
    ("switch-client", "switchc", "ElnprZ", "ctT"),
    ("unbind-key", "unbind", "anq", "T"),
    ("unlink-window", "unlinkw", "k", "t"),
    ("wait-for", "wait", "LSU", ""),
];

pub fn canonical(name: &str) -> Option<&'static str> {
    if let Some(c) = COMMANDS.iter().find(|c| c.0 == name || (!c.1.is_empty() && c.1 == name)) {
        return Some(c.0);
    }
    // unique prefix, like tmux
    let m: Vec<&(&str, &str, &str, &str)> = COMMANDS.iter().filter(|c| c.0.starts_with(name)).collect();
    if m.len() == 1 { Some(m[0].0) } else { None }
}

/// Store a command option the way tmux prints its parsed list: full command
/// names, escaped arguments, commands joined by " ; ". Unknown names (aliases)
/// are kept as written.
fn canonical_command_list(v: &str) -> Result<String, String> {
    let cmds = cmdparse::parse(v)?;
    Ok(cmds.iter().filter(|c| !c.is_empty()).map(|c| {
        let name = canonical(&c[0]).map_or_else(|| options::escape_arg(&c[0]), String::from);
        std::iter::once(name).chain(c[1..].iter().map(|a| options::escape_arg(a))).collect::<Vec<_>>().join(" ")
    }).collect::<Vec<_>>().join(" ; "))
}

fn err<T>(s: impl Into<String>) -> Result<T, String> {
    Err(s.into())
}

/// Extracts every `-e VAR=value` (repeatable) before getopt sees them.
fn take_env(args: &mut Vec<String>) -> Vec<(String, String)> {
    let mut env = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" || !a.starts_with('-') {
            break;
        }
        if a == "-e" && i + 1 < args.len() {
            if let Some((k, v)) = args[i + 1].split_once('=') {
                env.push((k.to_string(), v.to_string()));
            }
            args.drain(i..i + 2);
            continue;
        }
        if let Some(kv) = a.strip_prefix("-e").filter(|s| s.contains('=')) {
            if let Some((k, v)) = kv.split_once('=') {
                env.push((k.to_string(), v.to_string()));
            }
            args.remove(i);
            continue;
        }
        i += 1;
    }
    env
}

fn fnmatch(pat: &str, s: &str) -> bool {
    fn m(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => m(&p[1..], s) || (!s.is_empty() && m(p, &s[1..])),
            (Some('?'), Some(_)) => m(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => m(&p[1..], &s[1..]),
            _ => false,
        }
    }
    m(&pat.chars().collect::<Vec<_>>(), &s.chars().collect::<Vec<_>>())
}

const DIRS: [(char, Dir); 4] = [('L', Dir::Left), ('R', Dir::Right), ('U', Dir::Up), ('D', Dir::Down)];

impl Server {
    // ---- target resolution ---------------------------------------------------------

    /// The default target for a command context.
    pub fn current(&self, ctx: &Ctx) -> Target {
        if let Some(p) = ctx.pane.filter(|p| self.panes.contains_key(p) && self.window_of(*p).is_some()) {
            // A key binding or CLI from inside a pane: but prefer the client's current
            // window when the pane's window isn't the one shown.
            return self.target_for_pane(p);
        }
        let sid = ctx.client.and_then(|c| self.client_session(c)).or_else(|| self.current_session());
        match sid {
            Some(sid) => {
                let s = &self.sessions[&sid];
                let wid = s.active_window();
                Target { sid: Some(sid), idx: Some(s.active), wid: Some(wid), pane: Some(self.windows[&wid].active) }
            }
            None => Target::default(),
        }
    }

    fn find_session(&self, ctx: &Ctx, name: &str) -> Result<SessId, String> {
        if name.is_empty() {
            return self.current(ctx).sid.ok_or_else(|| "no current session".to_string());
        }
        if name == "{mouse}" || name == "=" {
            return ctx.mouse.as_ref().and_then(|m| m.sid).ok_or_else(|| "no mouse target".to_string());
        }
        if name == "{marked}" || name == "~" {
            return self.marked.and_then(|p| self.session_of_pane(p)).map(|x| x.0).ok_or_else(|| "no marked target".to_string());
        }
        if let Some(id) = name.strip_prefix('$').and_then(|i| i.parse().ok()) {
            if self.sessions.contains_key(&id) {
                return Ok(id);
            }
        }
        if let Some(exact) = name.strip_prefix('=') {
            return self.sessions.values().find(|s| s.name == exact).map(|s| s.id).ok_or_else(|| format!("can't find session: {name}"));
        }
        if let Some(s) = self.sessions.values().find(|s| s.name == name) {
            return Ok(s.id);
        }
        // a pane or window id names its session
        if name.starts_with('%') || name.starts_with('@') {
            let t = self.resolve_pane(ctx, Some(name))?;
            return t.sid.ok_or_else(|| format!("can't find session: {name}"));
        }
        let m: Vec<SessId> = self.sessions.values().filter(|s| s.name.starts_with(name)).map(|s| s.id).collect();
        match m.len() {
            1 => return Ok(m[0]),
            0 => {}
            _ => return err(format!("more than one session: {name}")),
        }
        let m: Vec<SessId> = self.sessions.values().filter(|s| fnmatch(name, &s.name)).map(|s| s.id).collect();
        match m.len() {
            1 => Ok(m[0]),
            0 => err(format!("can't find session: {name}")),
            _ => err(format!("more than one session: {name}")),
        }
    }

    pub fn resolve_session(&self, ctx: &Ctx, t: Option<&str>) -> Result<SessId, String> {
        match t {
            None => self.current(ctx).sid.ok_or_else(|| "no current session".to_string()),
            Some(t) => {
                let s = t.split(':').next().unwrap_or("");
                self.find_session(ctx, s)
            }
        }
    }

    fn find_window_in(&self, sid: SessId, w: &str, ctx: &Ctx) -> Result<u32, String> {
        let s = &self.sessions[&sid];
        let keys: Vec<u32> = s.links.keys().copied().collect();
        let pos = keys.iter().position(|&k| k == s.active).unwrap_or(0);
        let offset = |n: i64| -> u32 {
            let len = keys.len() as i64;
            keys[((pos as i64 + n).rem_euclid(len)) as usize]
        };
        let r = match w {
            "" => Some(s.active),
            "{start}" | "^" => keys.first().copied(),
            "{end}" | "$" => keys.last().copied(),
            "{last}" | "!" => s.last.first().copied(),
            "{next}" | "+" => Some(offset(1)),
            "{previous}" | "-" => Some(offset(-1)),
            "{mouse}" | "=" => ctx.mouse.as_ref().and_then(|m| m.idx),
            _ => None,
        };
        if let Some(i) = r {
            return Ok(i);
        }
        if let Some(n) = w.strip_prefix('+').and_then(|n| n.parse::<i64>().ok()) {
            return Ok(offset(n));
        }
        if let Some(n) = w.strip_prefix('-').and_then(|n| n.parse::<i64>().ok()) {
            return Ok(offset(-n));
        }
        let exact = w.strip_prefix('=');
        let w2 = exact.unwrap_or(w);
        if let Ok(i) = w2.parse::<u32>() {
            if s.links.contains_key(&i) {
                return Ok(i);
            }
            if exact.is_some() {
                return err(format!("can't find window: {w}"));
            }
        }
        if let Some(id) = w2.strip_prefix('@').and_then(|i| i.parse::<WinId>().ok()) {
            return s.links.iter().find(|(_, x)| **x == id).map(|(i, _)| *i).ok_or_else(|| format!("can't find window: {w}"));
        }
        let named: Vec<u32> = s.links.iter().filter(|(_, wid)| self.windows[wid].name == w2).map(|(i, _)| *i).collect();
        if named.len() == 1 {
            return Ok(named[0]);
        }
        if named.len() > 1 {
            return err(format!("more than one window: {w}"));
        }
        if exact.is_none() {
            let m: Vec<u32> = s
                .links
                .iter()
                .filter(|(_, wid)| self.windows[wid].name.starts_with(w2) || fnmatch(w2, &self.windows[wid].name))
                .map(|(i, _)| *i)
                .collect();
            if m.len() == 1 {
                return Ok(m[0]);
            }
            if m.len() > 1 {
                return err(format!("more than one window: {w}"));
            }
        }
        err(format!("can't find window: {w}"))
    }

    pub fn resolve_window(&self, ctx: &Ctx, t: Option<&str>) -> Result<Target, String> {
        let Some(t) = t else {
            let c = self.current(ctx);
            return if c.wid.is_some() { Ok(c) } else { err("no current window") };
        };
        let mk = |sid: SessId, idx: u32| -> Target {
            let wid = self.sessions[&sid].links[&idx];
            Target { sid: Some(sid), idx: Some(idx), wid: Some(wid), pane: Some(self.windows[&wid].active) }
        };
        if t == "{mouse}" || t == "=" {
            let m = ctx.mouse.as_ref().ok_or("no mouse target")?;
            let wid = m.wid.filter(|w| self.windows.contains_key(w)).ok_or("no mouse target")?;
            let pane = m.pane.filter(|p| self.panes.contains_key(p)).unwrap_or(self.windows[&wid].active);
            return Ok(Target { sid: m.sid.filter(|s| self.sessions.contains_key(s)), idx: m.idx, wid: Some(wid), pane: Some(pane) });
        }
        if t == "{marked}" || t == "~" {
            let p = self.marked.ok_or("no marked target")?;
            return Ok(self.target_for_pane(p));
        }
        if let Some(id) = t.strip_prefix('@').and_then(|i| i.parse::<WinId>().ok()) {
            if self.windows.contains_key(&id) {
                let cur = self.current(ctx).sid;
                let links = self.links_of(id);
                let (sid, idx) = links.iter().find(|(s, _)| Some(*s) == cur).or(links.first()).copied().ok_or("window not linked")?;
                return Ok(mk(sid, idx));
            }
        }
        if t.starts_with('%') {
            let p = self.resolve_pane(ctx, Some(t))?;
            return Ok(p);
        }
        match t.split_once(':') {
            Some((s, w)) => {
                let w = w.split('.').next().unwrap_or("");
                let sid = self.find_session(ctx, s)?;
                let idx = self.find_window_in(sid, w, ctx)?;
                Ok(mk(sid, idx))
            }
            None => {
                let cur = self.current(ctx).sid;
                if let Some(sid) = cur {
                    if let Ok(idx) = self.find_window_in(sid, t, ctx) {
                        return Ok(mk(sid, idx));
                    }
                }
                let sid = self.find_session(ctx, t)?;
                let idx = self.sessions[&sid].active;
                Ok(mk(sid, idx))
            }
        }
    }

    fn find_pane_in(&self, wid: WinId, p: &str, cur_pane: Option<PaneId>, ctx: &Ctx) -> Result<PaneId, String> {
        let w = &self.windows[&wid];
        let panes = w.panes();
        let active = cur_pane.filter(|c| panes.contains(c)).unwrap_or(w.active);
        let pos = panes.iter().position(|&x| x == active).unwrap_or(0);
        let off = |n: i64| panes[((pos as i64 + n).rem_euclid(panes.len() as i64)) as usize];
        let (rects, _) = self.rects(wid);
        let rect_of = |id: PaneId| rects.iter().find(|(x, _)| *x == id).map(|(_, r)| *r);
        let pick = |f: &dyn Fn(&crate::layout::Rect) -> (i64, i64)| rects.iter().min_by_key(|(_, r)| f(r)).map(|(id, _)| *id);
        let dir_of = |d: Dir| {
            let cur = self.panes.get(&active).map(|x| x.screen().cursor_position()).unwrap_or((0, 0));
            let r = rect_of(active).unwrap_or_default();
            layout::neighbour(&rects, active, d, (r.x + cur.1, r.y + cur.0))
        };
        let r = match p {
            "" => Some(active),
            "{last}" | "!" => w.last_active,
            "{next}" | "+" => Some(off(1)),
            "{previous}" | "-" => Some(off(-1)),
            "{top}" => pick(&|r| (r.y as i64, r.x as i64)),
            "{bottom}" => pick(&|r| (-(r.bottom() as i64), r.x as i64)),
            "{left}" => pick(&|r| (r.x as i64, r.y as i64)),
            "{right}" => pick(&|r| (-(r.right() as i64), r.y as i64)),
            "{top-left}" => pick(&|r| (r.y as i64 + r.x as i64, 0)),
            "{top-right}" => pick(&|r| (r.y as i64 - r.right() as i64, 0)),
            "{bottom-left}" => pick(&|r| (r.x as i64 - r.bottom() as i64, 0)),
            "{bottom-right}" => pick(&|r| (-(r.right() as i64) - r.bottom() as i64, 0)),
            "{up-of}" => dir_of(Dir::Up),
            "{down-of}" => dir_of(Dir::Down),
            "{left-of}" => dir_of(Dir::Left),
            "{right-of}" => dir_of(Dir::Right),
            "{mouse}" | "=" => ctx.mouse.as_ref().and_then(|m| self.mouse_pane(m)),
            "{marked}" | "~" => self.marked,
            _ => None,
        };
        if let Some(x) = r {
            return Ok(x);
        }
        if let Some(n) = p.strip_prefix('+').and_then(|n| n.parse::<i64>().ok()) {
            return Ok(off(n));
        }
        if let Some(n) = p.strip_prefix('-').and_then(|n| n.parse::<i64>().ok()) {
            return Ok(off(-n));
        }
        if let Some(id) = p.strip_prefix('%').and_then(|i| i.parse::<PaneId>().ok()) {
            if panes.contains(&id) {
                return Ok(id);
            }
        }
        if let Ok(i) = p.parse::<usize>() {
            let base = self.opt_window_num(Some(wid), "pane-base-index").max(0) as usize;
            if let Some(x) = i.checked_sub(base).and_then(|i| panes.get(i)) {
                return Ok(*x);
            }
        }
        err(format!("can't find pane: {p}"))
    }

    pub fn resolve_pane(&self, ctx: &Ctx, t: Option<&str>) -> Result<Target, String> {
        let Some(t) = t else {
            let c = self.current(ctx);
            return if c.pane.is_some() { Ok(c) } else { err("no current pane") };
        };
        if let Some(id) = t.strip_prefix('%').and_then(|i| i.parse::<PaneId>().ok()) {
            if self.panes.contains_key(&id) && self.window_of(id).is_some() {
                return Ok(self.target_for_pane(id));
            }
            return err(format!("can't find pane: {t}"));
        }
        if t == "{mouse}" || t == "=" {
            let m = ctx.mouse.as_ref().ok_or("no mouse target")?;
            let p = self.mouse_pane(m).ok_or("no mouse target")?;
            return Ok(self.target_for_pane(p));
        }
        if t == "{marked}" || t == "~" {
            return Ok(self.target_for_pane(self.marked.ok_or("no marked target")?));
        }
        let (wpart, ppart) = match t.rfind('.') {
            Some(i) if !t[..i].ends_with(':') || t.contains(':') => (Some(&t[..i]), &t[i + 1..]),
            _ => (None, t),
        };
        if wpart.is_none() && !t.contains(':') {
            // a bare word: pane in the current window, else a window target
            let cur = self.current(ctx);
            if let Some(wid) = cur.wid {
                if let Ok(p) = self.find_pane_in(wid, t, ctx.pane, ctx) {
                    return Ok(Target { pane: Some(p), ..cur });
                }
            }
            return self.resolve_window(ctx, Some(t));
        }
        let wt = match wpart {
            Some(w) => self.resolve_window(ctx, Some(w))?,
            None => self.resolve_window(ctx, Some(t))?,
        };
        let wid = wt.wid.unwrap();
        let ppart = if wpart.is_some() { ppart } else { "" };
        let cur_pane = if wt.wid == self.current(ctx).wid { ctx.pane } else { None };
        let p = self.find_pane_in(wid, ppart, cur_pane, ctx)?;
        Ok(Target { pane: Some(p), ..wt })
    }

    /// Target client for commands with -c/-t client: a client name, or the context's client.
    pub fn resolve_client(&self, ctx: &Ctx, t: Option<&str>) -> Result<ClientId, String> {
        match t {
            Some(name) => {
                if let Some(c) = self.clients.values().find(|c| c.attached && (c.name() == name || c.id.to_string() == name)) {
                    return Ok(c.id);
                }
                // a session name → its most recent client
                if let Ok(sid) = self.find_session(ctx, name.trim_end_matches(':')) {
                    if let Some(c) = self.clients.values().filter(|c| c.attached && c.session == Some(sid)).max_by_key(|c| c.activity) {
                        return Ok(c.id);
                    }
                }
                err(format!("can't find client: {name}"))
            }
            None => {
                if let Some(c) = ctx.client.filter(|c| self.clients.get(c).is_some_and(|c| c.attached)) {
                    return Ok(c);
                }
                // CLI from inside a pane: a client attached to that pane's session
                let sid = ctx.pane.and_then(|p| self.session_of_pane(p)).map(|x| x.0);
                self.clients
                    .values()
                    .filter(|c| c.attached && (sid.is_none() || c.session == sid))
                    .max_by_key(|c| c.activity)
                    .map(|c| c.id)
                    .ok_or_else(|| "no current client".to_string())
            }
        }
    }

    // ---- lifecycle helpers ----------------------------------------------------------------

    fn next_index(&self, sid: SessId, after: Option<u32>) -> u32 {
        let base = self.opt_session_num(Some(sid), "base-index").max(0) as u32;
        let s = &self.sessions[&sid];
        let mut i = after.map(|a| a + 1).unwrap_or(base).max(base);
        while s.links.contains_key(&i) {
            i += 1;
        }
        i
    }

    /// Makes room at `idx` by shifting later windows up by one (new-window -a/-b).
    fn shift_up(&mut self, sid: SessId, idx: u32) {
        let s = self.sessions.get_mut(&sid).unwrap();
        if !s.links.contains_key(&idx) {
            return;
        }
        let moved: Vec<(u32, WinId)> = s.links.range(idx..).map(|(i, w)| (*i, *w)).collect();
        for (i, _) in &moved {
            s.links.remove(i);
        }
        for (i, w) in moved {
            s.links.insert(i + 1, w);
        }
        if s.active >= idx {
            s.active += 1;
        }
        for l in s.last.iter_mut() {
            if *l >= idx {
                *l += 1;
            }
        }
    }

    pub fn new_window_obj(&mut self, name: String, pane: PaneId, sx: u16, sy: u16) -> WinId {
        let wid = self.id();
        let now = now_secs();
        self.windows.insert(
            wid,
            Window {
                id: wid,
                name,
                layout: Node::Leaf(pane),
                pane_order: vec![pane],
                active: pane,
                last_active: None,
                zoomed: false,
                options: Options::default(),
                alerts: 0,
                layout_cycle: None,
                old_layout: None,
                created: now,
                activity: now,
                sx,
                sy,
            },
        );
        wid
    }

    fn link(&mut self, sid: SessId, idx: u32, wid: WinId, select: bool) {
        let s = self.sessions.get_mut(&sid).unwrap();
        s.links.insert(idx, wid);
        if select {
            if s.active != idx && s.links.contains_key(&s.active) {
                let a = s.active;
                s.last.retain(|&x| x != a);
                s.last.insert(0, a);
            }
            s.active = idx;
        }
        let (cols, rows) = (s.cols, s.rows);
        let status = self.status_lines(Some(sid));
        let w = self.windows.get_mut(&wid).unwrap();
        if w.sx == 0 {
            w.sx = cols;
            w.sy = rows.saturating_sub(status).max(1);
        }
        self.mark_session_dirty(sid);
    }

    fn window_name_for(&self, cmd: &[String], sid: Option<SessId>) -> String {
        match cmd.first() {
            Some(c) if cmd.len() == 1 => procinfo::program_name(c.split_whitespace().next().unwrap_or(c)),
            Some(c) => procinfo::program_name(c),
            None => {
                let dc = self.opt_session(sid, "default-command");
                if !dc.is_empty() {
                    return procinfo::program_name(dc.split_whitespace().next().unwrap_or(&dc));
                }
                let sh = self.opt_session(sid, "default-shell");
                procinfo::program_name(if sh.is_empty() { "shell" } else { &sh })
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_session(&mut self, ctx: &Ctx, name: Option<String>, cols: u16, rows: u16, cwd: Option<String>, win_name: Option<String>, cmd: &[String], env: &[(String, String)]) -> Result<SessId, String> {
        let name = match name {
            Some(n) if n.is_empty() || n.contains([':', '.']) => return err(format!("invalid session: {n}")),
            Some(n) if self.sessions.values().any(|s| s.name == n) => return err(format!("duplicate session: {n}")),
            Some(n) => n,
            None => (0..).map(|i: u32| i.to_string()).find(|n| !self.sessions.values().any(|s| &s.name == n)).unwrap(),
        };
        let sid = self.id();
        let now = now_secs();
        let cwd = cwd
            .filter(|d| !d.is_empty())
            .or_else(|| ctx.client.and_then(|c| self.clients.get(&c)).and_then(|c| c.ident.as_ref()).map(|i| i.cwd.clone()))
            .or_else(procinfo::home_dir)
            .unwrap_or_default();
        let mut senv = BTreeMap::new();
        if let Some(ident) = ctx.client.and_then(|c| self.clients.get(&c)).and_then(|c| c.ident.as_ref()) {
            for n in self.opt_array(None, "update-environment") {
                if let Some((_, v)) = ident.env.iter().find(|(k, _)| *k == n) {
                    senv.insert(n, Some(v.clone()));
                }
            }
        }
        for (k, v) in env {
            senv.insert(k.clone(), Some(v.clone()));
        }
        self.sessions.insert(
            sid,
            Session {
                id: sid,
                name: name.clone(),
                links: BTreeMap::new(),
                active: 0,
                last: Vec::new(),
                cols: cols.max(2),
                rows: rows.max(2),
                cwd: cwd.clone(),
                options: Options::default(),
                env: senv,
                hooks: BTreeMap::new(),
                created: now,
                activity: now,
                last_attached: 0,
                alerts: BTreeMap::new(),
                group: None,
            },
        );
        let status = self.status_lines(Some(sid));
        let (sx, sy) = (cols.max(2), rows.max(2).saturating_sub(status).max(1));
        let pane = match self.spawn_pane(Some(sid), sx, sy, cmd, Some(&cwd), &[]) {
            Ok(p) => p,
            Err(e) => {
                self.sessions.remove(&sid);
                return Err(e);
            }
        };
        let wname = win_name.clone().unwrap_or_else(|| self.window_name_for(cmd, Some(sid)));
        let wid = self.new_window_obj(wname, pane, sx, sy);
        if win_name.is_some() {
            self.windows.get_mut(&wid).unwrap().options.set("automatic-rename", "off".into());
        }
        let idx = self.next_index(sid, None);
        self.sessions.get_mut(&sid).unwrap().active = idx;
        self.link(sid, idx, wid, true);
        self.had_session = true;
        self.run_hook(Some(sid), "session-created", Ctx { client: ctx.client, ..Ctx::default() });
        Ok(sid)
    }

    pub fn select_window(&mut self, sid: SessId, idx: u32) {
        let s = self.sessions.get_mut(&sid).unwrap();
        if !s.links.contains_key(&idx) {
            return;
        }
        if s.active != idx {
            let a = s.active;
            s.last.retain(|&x| x != a && x != idx);
            s.last.insert(0, a);
            s.active = idx;
        }
        s.alerts.remove(&idx);
        s.activity = now_secs();
        self.mark_session_dirty(sid);
        self.run_hook(Some(sid), "session-window-changed", Ctx::default());
    }

    pub fn select_pane(&mut self, wid: WinId, pane: PaneId) {
        let w = self.windows.get_mut(&wid).unwrap();
        if w.active != pane {
            w.last_active = Some(w.active);
            w.active = pane;
            if w.zoomed {
                w.zoomed = false;
                self.apply_layout(wid);
            }
            self.run_hook(None, "window-pane-changed", Ctx { pane: Some(pane), ..Ctx::default() });
        }
        self.mark_window_dirty(wid);
    }

    fn select_dir(&mut self, t: Target, dir: Dir) -> Result<(), String> {
        let wid = t.wid.unwrap();
        let pane = t.pane.unwrap();
        let (rects, _) = self.rects(wid);
        let r = rects.iter().find(|(p, _)| *p == pane).map(|(_, r)| *r).ok_or("pane not visible")?;
        let (cy, cx) = self.panes.get(&pane).map(|p| p.screen().cursor_position()).unwrap_or((0, 0));
        let n = layout::neighbour(&rects, pane, dir, (r.x + cx, r.y + cy)).or_else(|| {
            // wrap around like tmux
            let opposite = match dir {
                Dir::Left => Dir::Right,
                Dir::Right => Dir::Left,
                Dir::Up => Dir::Down,
                Dir::Down => Dir::Up,
            };
            let mut cur = pane;
            while let Some(n) = layout::neighbour(&rects, cur, opposite, (r.x + cx, r.y + cy)) {
                cur = n;
            }
            (cur != pane).then_some(cur)
        });
        if let Some(n) = n {
            self.select_pane(wid, n);
        }
        Ok(())
    }

    pub fn split_pane(&mut self, t: Target, horizontal: bool, before: bool, size: Option<u16>, full: bool, cwd: Option<String>, cmd: &[String], env: &[(String, String)]) -> Result<PaneId, String> {
        let wid = t.wid.ok_or("no window")?;
        let target = t.pane.ok_or("no pane")?;
        let area = self.window_area(wid);
        let (rects, _) = self.rects(wid);
        let r = if full { area } else { rects.iter().find(|(p, _)| *p == target).map(|(_, r)| *r).ok_or("pane not visible")? };
        if (horizontal && r.w < 3) || (!horizontal && r.h < 3) {
            return err("no space for new pane");
        }
        let total = if horizontal { r.w } else { r.h };
        let new_size = size.unwrap_or((total - 1) / 2).clamp(1, total - 2);
        let (w, h) = if horizontal { (new_size, r.h) } else { (r.w, new_size) };
        let cwd = cwd.or_else(|| self.panes.get(&target).map(|p| p.current_path.clone()));
        let new = self.spawn_pane(t.sid, w, h, cmd, cwd.as_deref(), env)?;
        let win = self.windows.get_mut(&wid).unwrap();
        win.insert_pane(target, new, before);
        if full {
            let old = std::mem::replace(&mut win.layout, Node::Leaf(PaneId::MAX));
            let ratio_new = new_size as f32 / (total - 1) as f32;
            win.layout = if before {
                Node::Split { horizontal, ratio: ratio_new, a: Box::new(Node::Leaf(new)), b: Box::new(old) }
            } else {
                Node::Split { horizontal, ratio: 1.0 - ratio_new, a: Box::new(old), b: Box::new(Node::Leaf(new)) }
            };
        } else {
            // no size: tmux's middle split, whose first cell gets the odd column
            win.layout.split_ex(target, new, horizontal, before, size.map(|_| new_size), r);
        }
        win.zoomed = false;
        self.apply_layout(wid);
        self.mark_window_dirty(wid);
        Ok(new)
    }

    pub fn enter_copy_mode(&mut self, pane: PaneId) {
        let Some(p) = self.panes.get(&pane) else { return };
        if matches!(p.mode, Some(PaneMode::Copy(_))) {
            return;
        }
        let lines = p.snapshot();
        let (w, h) = p.size;
        let (cy, cx) = p.screen().cursor_position();
        let wid = self.window_of(pane);
        let vi = self.opt_window(wid, "mode-keys") == "vi";
        let sid = self.session_of_pane(pane).map(|x| x.0);
        let mut cm = CopyMode::new(lines, w, h, (cy, cx), vi);
        cm.set_word_separators(&self.opt_session(sid, "word-separators"));
        self.panes.get_mut(&pane).unwrap().mode = Some(PaneMode::Copy(Box::new(cm)));
        if let Some(wid) = wid {
            self.mark_window_dirty(wid);
        }
    }

    pub fn exit_mode(&mut self, pane: PaneId) {
        if let Some(p) = self.panes.get_mut(&pane) {
            p.mode = None;
        }
        if let Some(wid) = self.window_of(pane) {
            self.mark_window_dirty(wid);
        }
    }

    // ---- buffers ---------------------------------------------------------------------------

    pub fn add_buffer(&mut self, name: Option<String>, data: Vec<u8>, append: bool) {
        if let Some(n) = &name {
            if let Some(b) = self.buffers.iter_mut().find(|b| &b.name == n) {
                if append {
                    b.data.extend(data);
                } else {
                    b.data = data;
                }
                return;
            }
        } else if append {
            if let Some(b) = self.buffers.first_mut() {
                b.data.extend(data);
                return;
            }
        }
        let automatic = name.is_none();
        let name = name.unwrap_or_else(|| {
            let n = format!("buffer{}", self.buffer_seq);
            self.buffer_seq += 1;
            n
        });
        self.buffers.insert(0, Buffer { name, data, automatic, created: now_secs() });
        let limit = self.opt_server_num("buffer-limit").max(1) as usize;
        while self.buffers.iter().filter(|b| b.automatic).count() > limit {
            if let Some(pos) = self.buffers.iter().rposition(|b| b.automatic) {
                self.buffers.remove(pos);
            }
        }
    }

    /// Copy from copy mode: store in a buffer and forward to the outer terminal clipboard (OSC 52).
    pub fn copy_text(&mut self, text: String, cid: Option<ClientId>) {
        if text.is_empty() {
            return;
        }
        let clip = self.opt_server("set-clipboard");
        if clip != "off" {
            if let Some(c) = cid.and_then(|c| self.clients.get(&c)).filter(|c| c.control == 0 && c.features.has("clipboard")) {
                let enc = base64(text.as_bytes());
                c.send(S_OUTPUT, format!("\x1b]52;c;{enc}\x07").as_bytes());
            }
        }
        self.add_buffer(None, text.into_bytes(), false);
    }

    pub fn paste(&mut self, pane: PaneId, data: &[u8], bracket: bool, raw_lf: bool, sep: Option<&str>) {
        let Some(p) = self.panes.get(&pane) else { return };
        let mut out = Vec::new();
        let text = String::from_utf8_lossy(data);
        let sep = sep.unwrap_or("\r");
        let body: String = if raw_lf { text.into_owned() } else { text.replace("\r\n", "\n").replace('\n', sep) };
        let br = bracket && p.screen().bracketed_paste();
        if br {
            out.extend(b"\x1b[200~");
        }
        out.extend(body.as_bytes());
        if br {
            out.extend(b"\x1b[201~");
        }
        p.send(out);
    }

    // ---- the dispatcher ---------------------------------------------------------------------

    pub fn exec(&mut self, argv: Vec<String>, ctx: &mut Ctx) -> Result<Flow, String> {
        ctx.readonly |= self.readonly_context.get() || self.client_is_readonly(ctx.client) || self.client_is_readonly(ctx.reply);
        let previous = self.readonly_context.replace(ctx.readonly);
        let result = self.exec_inner(argv, ctx);
        self.readonly_context.set(previous);
        if ctx.out.overflowed() { err("command output limit exceeded") } else { result }
    }

    fn exec_inner(&mut self, argv: Vec<String>, ctx: &mut Ctx) -> Result<Flow, String> {
        let Some((name, rest)) = argv.split_first() else { return Ok(Flow::Next) };
        // NAME=value assignments from the command line
        if !name.starts_with('-') && name.contains('=') && rest.is_empty() {
            if ctx.readonly { return err("client is read-only"); }
            let (k, v) = name.split_once('=').unwrap();
            self.env.insert(k.to_string(), Some(v.to_string()));
            return Ok(Flow::Next);
        }
        let full = canonical(name).ok_or_else(|| format!("unknown command: {name}"))?;
        if matches!(full, "display-menu" | "display-popup" | "display-panes") && self.overlay_waits.len() >= 64 {
            return err("too many waiting overlays");
        }
        let spec = COMMANDS.iter().find(|c| c.0 == full).unwrap();
        let mut args = rest.to_vec();
        let env = if matches!(full, "new-session" | "new-window" | "split-window" | "respawn-pane" | "respawn-window" | "display-popup") {
            take_env(&mut args)
        } else {
            Vec::new()
        };
        // read-only users (server-access -r) may only look
        if ctx.readonly && !READ_ONLY_OK.contains(&full) {
            return err("client is read-only");
        }
        let o = getopt(&args, spec.2, spec.3).map_err(|e| format!("{full}: {e}"))?;
        if ctx.readonly {
            self.check_readonly_command(full, &o, ctx)?;
        }
        self.command(full, o, env, ctx)
    }

    fn check_readonly_command(&self, cmd: &str, o: &Opts, ctx: &Ctx) -> Result<(), String> {
        let own = ctx.client.ok_or("client is read-only")?;
        let pane_target = |s: &str| s.contains([':', '.']) || s.starts_with(['%', '@']);
        let allowed = match cmd {
            "attach-session" => !o.has('c') && !o.has('d') && !o.has('x') && !o.has('f')
                && !o.get('t').is_some_and(pane_target),
            "switch-client" => !o.has('T') && !o.has('r') && !o.has('Z') && !o.get('t').is_some_and(pane_target)
                && self.resolve_client(ctx, o.get('c'))? == own,
            "detach-client" => !['a', 's', 'P', 'E'].into_iter().any(|f| o.has(f))
                && self.resolve_client(ctx, o.get('t').or(o.get('c')))? == own,
            "refresh-client" => self.resolve_client(ctx, o.get('t'))? == own,
            "capture-pane" => o.has('p'),
            "display-message" => (o.has('p') || o.has('a')) && !o.has('I'),
            "server-access" => o.has('l') && !['a', 'd', 'r', 'w'].into_iter().any(|f| o.has(f)),
            _ => true,
        };
        if allowed { Ok(()) } else { err("client is read-only") }
    }

    fn command(&mut self, cmd: &str, o: Opts, env: Vec<(String, String)>, ctx: &mut Ctx) -> Result<Flow, String> {
        let t = o.get('t').map(String::from);
        let t = t.as_deref();
        match cmd {
            // ---------------------------------------------------------------- sessions
            "new-session" => {
                let name = o.get('s').map(String::from);
                if let Some(target) = o.get('t') {
                    return self.new_grouped_session(ctx, name, target, o.has('d'), o.has('P'), o.get('F'));
                }
                if o.has('A') {
                    if let Some(n) = &name {
                        if let Some(sid) = self.sessions.values().find(|s| &s.name == n).map(|s| s.id) {
                            return self.attach_or_switch(ctx, sid, o.has('D'), false, o.has('E'));
                        }
                    }
                }
                let cid = ctx.reply.or(ctx.client);
                let ident_size = cid.and_then(|c| self.clients.get(&c)).and_then(|c| c.ident.as_ref()).filter(|i| i.tty).map(|i| (i.cols, i.rows));
                let default = self.opt_session(None, "default-size");
                let (dx, dy) = default.split_once('x').map(|(a, b)| (a.parse().unwrap_or(80), b.parse().unwrap_or(24))).unwrap_or((80, 24));
                let control = cid.is_some_and(|c| self.is_control(c));
                let detached = o.has('d') || ident_size.is_none() && !control && ctx.client.and_then(|c| self.clients.get(&c)).is_none_or(|c| !c.attached);
                let ident_size = if control { None } else { ident_size };
                let (mut cols, mut rows) = if detached { (dx, dy) } else { ident_size.unwrap_or((dx, dy)) };
                if let Some(x) = o.get('x').and_then(|v| v.parse().ok()) {
                    cols = x;
                }
                if let Some(y) = o.get('y').and_then(|v| v.parse().ok()) {
                    rows = y;
                }
                // Detached/control dimensions describe the pane area; attached
                // terminal dimensions also include the status line.
                let (cols, rows) = (cols.min(MAX_SIZE), rows.min(MAX_SIZE));
                let rows = if detached || control { rows.saturating_add(self.status_lines(None)) } else { rows };
                if !detached {
                    // already attached clients switch, CLI clients attach
                    if let Some(c) = cid.and_then(|c| self.clients.get(&c)) {
                        if !c.attached && c.ident.as_ref().is_some_and(|i| i.ctx_pane.is_some()) {
                            return err("sessions should be nested with care, unset PWSH_TMUX to force");
                        }
                    }
                }
                let cwd = o.get('c').map(|c| self.expand(self.current(ctx), ctx.client, c));
                let sid = self.create_session(ctx, name, cols, rows, cwd, o.get('n').map(String::from), &o.rest, &env)?;
                if o.has('P') {
                    let fmt = o.get('F').unwrap_or("#{session_name}:");
                    let s = self.expand(Target { sid: Some(sid), ..Target::default() }, None, fmt);
                    ctx.out.push_str(&s);
                    ctx.out.push('\n');
                }
                if !detached {
                    if let Some(c) = cid {
                        self.attach_client(c, sid);
                    }
                }
                ctx.pane = self.sessions.get(&sid).map(|s| self.windows[&s.active_window()].active);
                Ok(Flow::Next)
            }
            "attach-session" => {
                let sid = self.resolve_session(ctx, t)?;
                if let Some(w) = t.filter(|t| t.contains(':') || t.starts_with('@') || t.starts_with('%')) {
                    let wt = self.resolve_pane(ctx, Some(w))?;
                    if let (Some(idx), Some(wid), Some(p)) = (wt.idx, wt.wid, wt.pane) {
                        self.select_window(sid, idx);
                        self.select_pane(wid, p);
                    }
                }
                if let Some(c) = o.get('c') {
                    let c = self.expand(self.current(ctx), None, c);
                    self.sessions.get_mut(&sid).unwrap().cwd = c;
                }
                self.attach_or_switch(ctx, sid, o.has('d') || o.has('x'), o.has('r'), o.has('E'))
            }
            "switch-client" => {
                let cid = self.resolve_client(ctx, o.get('c'))?;
                if let Some(table) = o.get('T') {
                    let c = self.clients.get_mut(&cid).unwrap();
                    c.key_table = table.to_string();
                    c.dirty = true;
                    return Ok(Flow::Next);
                }
                if o.has('r') {
                    let c = self.clients.get_mut(&cid).unwrap();
                    if c.is_readonly() { return err("client is read-only"); }
                    c.readonly = !c.readonly;
                    return Ok(Flow::Next);
                }
                let cur = self.client_session(cid);
                let sorted: Vec<SessId> = {
                    let mut v: Vec<&Session> = self.sessions.values().collect();
                    v.sort_by(|a, b| a.name.cmp(&b.name));
                    v.iter().map(|s| s.id).collect()
                };
                if sorted.is_empty() { return err("no sessions"); }
                let pos = cur.and_then(|c| sorted.iter().position(|s| *s == c)).unwrap_or(0);
                let sid = if o.has('l') {
                    self.clients[&cid].last_session.filter(|s| self.sessions.contains_key(s)).ok_or("can't find last session")?
                } else if o.has('n') {
                    sorted[(pos + 1) % sorted.len()]
                } else if o.has('p') {
                    sorted[(pos + sorted.len() - 1) % sorted.len()]
                } else {
                    let tt = self.resolve_pane(ctx, t).or_else(|_| self.resolve_session(ctx, t).map(|s| Target { sid: Some(s), ..Target::default() }))?;
                    let sid = tt.sid.ok_or("no session")?;
                    if t.is_some_and(|t| t.contains(':') || t.contains('.') || t.starts_with('%') || t.starts_with('@') || t == "=" || t == "{mouse}") {
                        if let (Some(idx), Some(wid), Some(p)) = (tt.idx, tt.wid, tt.pane) {
                            self.select_window(sid, idx);
                            self.select_pane(wid, p);
                        }
                    }
                    sid
                };
                if o.has('Z') {
                    let wid = self.sessions[&sid].active_window();
                    let w = self.windows.get_mut(&wid).unwrap();
                    if w.panes().len() > 1 {
                        w.zoomed = true;
                        self.apply_layout(wid);
                    }
                }
                self.switch_client(cid, sid);
                Ok(Flow::Next)
            }
            "detach-client" => {
                let msg_for = |s: &Server, sid: Option<SessId>| format!("[detached (from session {})]", sid.and_then(|s2| s.sessions.get(&s2)).map(|s| s.name.as_str()).unwrap_or(""));
                if let Some(s) = o.get('s') {
                    let sid = self.find_session(ctx, s)?;
                    let cids: Vec<ClientId> = self.clients.values().filter(|c| c.attached && c.session == Some(sid)).map(|c| c.id).collect();
                    let m = msg_for(self, Some(sid));
                    for c in cids {
                        self.detach_client(c, &m);
                    }
                    return Ok(Flow::Next);
                }
                let cid = self.resolve_client(ctx, o.get('t').or(o.get('c')))?;
                let sid = self.client_session(cid);
                let m = msg_for(self, sid);
                if o.has('a') {
                    let cids: Vec<ClientId> = self.clients.values().filter(|c| c.attached && c.id != cid).map(|c| c.id).collect();
                    for c in cids {
                        let m = msg_for(self, self.client_session(c));
                        self.detach_client(c, &m);
                    }
                } else {
                    let m = if o.has('P') { "[detached and SIGHUP]".to_string() } else { m };
                    self.detach_client(cid, &m);
                }
                Ok(Flow::Next)
            }
            "has-session" => self.resolve_session(ctx, t).map(|_| Flow::Next),
            "kill-session" => {
                let sid = self.resolve_session(ctx, t)?;
                if o.has('C') {
                    self.sessions.get_mut(&sid).unwrap().alerts.clear();
                    self.mark_session_dirty(sid);
                    return Ok(Flow::Next);
                }
                if o.has('a') {
                    let others: Vec<SessId> = self.sessions.keys().filter(|s| **s != sid).copied().collect();
                    for s in others {
                        self.kill_session(s, false);
                    }
                } else {
                    self.kill_session(sid, false);
                }
                Ok(Flow::Next)
            }
            "kill-server" => {
                self.quit = true;
                Ok(Flow::Next)
            }
            "start-server" => Ok(Flow::Next),
            "rename-session" => {
                let sid = self.resolve_session(ctx, t)?;
                let name = o.rest.first().ok_or("rename-session needs a name")?;
                let name = self.expand(self.current(ctx), ctx.client, name);
                if name.is_empty() || name.contains([':', '.']) {
                    return err(format!("invalid session: {name}"));
                }
                if self.sessions.values().any(|s| s.id != sid && s.name == name) {
                    return err(format!("duplicate session: {name}"));
                }
                self.sessions.get_mut(&sid).unwrap().name = name;
                self.mark_session_dirty(sid);
                self.run_hook(Some(sid), "session-renamed", Ctx::default());
                Ok(Flow::Next)
            }
            "list-sessions" => {
                let fmt = o.get('F').unwrap_or("#{session_name}: #{session_windows} windows (created #{t:session_created})#{?session_attached, (attached),}");
                let filter = o.get('f');
                let ids: Vec<SessId> = self.sessions.keys().copied().collect();
                for sid in ids {
                    let tt = Target { sid: Some(sid), idx: Some(self.sessions[&sid].active), wid: Some(self.sessions[&sid].active_window()), pane: None };
                    if filter.is_some_and(|f| !truthy(&self.expand(tt, ctx.client, f))) {
                        continue;
                    }
                    let line = self.expand(tt, ctx.client, fmt);
                    ctx.out.push_str(&line);
                    ctx.out.push('\n');
                }
                Ok(Flow::Next)
            }
            "list-clients" => {
                let fmt = o.get('F').unwrap_or("#{client_name}: #{session_name} [#{client_width}x#{client_height} #{client_termname}]#{?client_readonly, (ro),}");
                let only = t.map(|t| self.find_session(ctx, t)).transpose()?;
                let cids: Vec<ClientId> = self.clients.values().filter(|c| c.attached && (only.is_none() || c.session == only)).map(|c| c.id).collect();
                for cid in cids {
                    let sid = self.client_session(cid);
                    let tt = sid.map(|s| Target { sid: Some(s), idx: Some(self.sessions[&s].active), wid: Some(self.sessions[&s].active_window()), pane: None }).unwrap_or_default();
                    let line = self.expand(tt, Some(cid), fmt);
                    ctx.out.push_str(&line);
                    ctx.out.push('\n');
                }
                Ok(Flow::Next)
            }
            "lock-server" | "lock-session" | "lock-client" => {
                let cmdline = self.opt_session(None, "lock-command");
                if cmdline.is_empty() {
                    return err("lock-command is not set");
                }
                let cids: Vec<ClientId> = match cmd {
                    "lock-client" => vec![self.resolve_client(ctx, t)?],
                    "lock-session" => {
                        let sid = self.resolve_session(ctx, t)?;
                        self.clients.values().filter(|c| c.attached && c.session == Some(sid)).map(|c| c.id).collect()
                    }
                    _ => self.clients.values().filter(|c| c.attached).map(|c| c.id).collect(),
                };
                for c in cids {
                    if let Some(cl) = self.clients.get_mut(&c) {
                        cl.suspended = true;
                        cl.send(S_SHELL, cmdline.as_bytes());
                    }
                }
                Ok(Flow::Next)
            }
            "suspend-client" => {
                let cid = self.resolve_client(ctx, t)?;
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.suspended = true;
                    c.send(S_SUSPEND, b"");
                }
                Ok(Flow::Next)
            }
            "refresh-client" => {
                let cid = self.resolve_client(ctx, t)?;
                if let Some(size) = o.get('C') {
                    // control clients: -C WxH (or W,H)
                    let (w, h) = size.split_once(['x', ',']).ok_or_else(|| format!("bad size: {size}"))?;
                    let (w, h): (u16, u16) = (w.parse().map_err(|_| format!("bad size: {size}"))?, h.parse().map_err(|_| format!("bad size: {size}"))?);
                    if let Some(c) = self.clients.get_mut(&cid) {
                        c.control_size = Some((w.clamp(1, MAX_SIZE), h.clamp(1, MAX_SIZE)));
                    }
                    if let Some(sid) = self.client_session(cid) {
                        self.update_session_size(sid);
                    }
                    return Ok(Flow::Next);
                }
                let mut handled = false;
                if let Some(a) = o.get('A') {
                    self.control_pane_action(cid, a)?;
                    handled = true;
                }
                if let Some(b) = o.get('B') {
                    self.control_subscribe(cid, b)?;
                    handled = true;
                }
                if let Some(f) = o.get('f').or(o.get('F')) {
                    self.control_flags(cid, f)?;
                    handled = true;
                }
                if handled {
                    return Ok(Flow::Next);
                }
                if o.has('S') {
                    if let Some(c) = self.clients.get_mut(&cid) {
                        c.dirty = true;
                    }
                    return Ok(Flow::Next);
                }
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.prev = None;
                    c.dirty = true;
                }
                Ok(Flow::Next)
            }
            // ---------------------------------------------------------------- windows
            "new-window" => {
                let (sid, base_idx) = match t {
                    Some(tt) if tt.contains(':') => {
                        let (s, w) = tt.split_once(':').unwrap();
                        let sid = self.find_session(ctx, s)?;
                        (sid, if w.is_empty() { None } else { Some(w.trim_start_matches('=').parse::<u32>().map_err(|_| format!("bad index: {w}"))?) })
                    }
                    Some(tt) => match tt.trim_start_matches('=').parse::<u32>() {
                        Ok(i) => (self.current(ctx).sid.ok_or("no current session")?, Some(i)),
                        Err(_) => (self.find_session(ctx, tt)?, None),
                    },
                    None => (self.current(ctx).sid.ok_or("no current session")?, None),
                };
                let s = &self.sessions[&sid];
                let cur_idx = s.active;
                let mut replaced = None;
                let idx = if o.has('a') || o.has('b') {
                    let at = base_idx.unwrap_or(cur_idx) + if o.has('a') { 1 } else { 0 };
                    self.shift_up(sid, at);
                    at
                } else if let Some(i) = base_idx {
                    if self.sessions[&sid].links.contains_key(&i) {
                        if !o.has('k') {
                            return err(format!("index in use: {i}"));
                        }
                        // replaced after the new window is linked: unlinking first would
                        // leave (or destroy) a session with no windows
                        replaced = self.sessions[&sid].links.get(&i).copied();
                    }
                    i
                } else {
                    self.next_index(sid, None)
                };
                if !self.sessions.contains_key(&sid) {
                    return err("session gone");
                }
                if o.has('S') {
                    if let Some(n) = o.get('n') {
                        let existing = self.sessions[&sid].links.iter().find(|(_, w)| self.windows[w].name == n).map(|(i, _)| *i);
                        if let Some(i) = existing {
                            self.select_window(sid, i);
                            return Ok(Flow::Next);
                        }
                    }
                }
                let s = &self.sessions[&sid];
                let status = self.status_lines(Some(sid));
                let (sx, sy) = (s.cols.max(1), s.rows.saturating_sub(status).max(1));
                let cwd = o.get('c').map(|c| self.expand(self.current(ctx), ctx.client, c)).or_else(|| Some(s.cwd.clone()));
                let pane = self.spawn_pane(Some(sid), sx, sy, &o.rest, cwd.as_deref(), &env)?;
                let name = o.get('n').map(|n| self.expand(self.current(ctx), ctx.client, n));
                let wname = name.clone().unwrap_or_else(|| self.window_name_for(&o.rest, Some(sid)));
                let wid = self.new_window_obj(wname, pane, sx, sy);
                if name.is_some() {
                    self.windows.get_mut(&wid).unwrap().options.set("automatic-rename", "off".into());
                }
                self.link(sid, idx, wid, !o.has('d') || replaced.is_some_and(|_| self.sessions[&sid].active == idx));
                if let Some(old) = replaced {
                    if self.links_of(old).is_empty() {
                        self.kill_window(old);
                    }
                }
                if o.has('P') {
                    let fmt = o.get('F').unwrap_or("#{session_name}:#{window_index}.#{pane_index}");
                    let line = self.expand_for_pane(pane, fmt);
                    ctx.out.push_str(&line);
                    ctx.out.push('\n');
                }
                ctx.pane = Some(pane);
                self.run_hook(Some(sid), "window-linked", Ctx::default());
                Ok(Flow::Next)
            }
            "kill-window" => {
                let wt = self.resolve_window(ctx, t)?;
                let (sid, idx) = (wt.sid.unwrap(), wt.idx.unwrap());
                if o.has('a') {
                    // collect ids, not indices: renumber-windows shifts indices as we go
                    let others: Vec<WinId> = self.sessions[&sid].links.iter().filter(|(i, _)| **i != idx).map(|(_, w)| *w).collect();
                    for w in others {
                        if self.windows.contains_key(&w) {
                            self.kill_window(w);
                        }
                    }
                } else {
                    self.kill_window(wt.wid.unwrap());
                }
                Ok(Flow::Next)
            }
            "unlink-window" => {
                let wt = self.resolve_window(ctx, t)?;
                if !o.has('k') && self.links_of(wt.wid.unwrap()).len() < 2 {
                    return err("window only linked to one session");
                }
                self.unlink(wt.sid.unwrap(), wt.idx.unwrap());
                Ok(Flow::Next)
            }
            "link-window" | "move-window" => {
                if cmd == "move-window" && o.has('r') {
                    let sid = self.resolve_session(ctx, t)?;
                    self.renumber(sid);
                    self.mark_session_dirty(sid);
                    return Ok(Flow::Next);
                }
                let src = self.resolve_window(ctx, o.get('s'))?;
                let wid = src.wid.unwrap();
                let (dsid, didx) = match t {
                    Some(tt) => {
                        let (s, w) = tt.split_once(':').unwrap_or((tt, ""));
                        let sid = if s.is_empty() {
                            self.current(ctx).sid.ok_or("no session")?
                        } else {
                            match self.find_session(ctx, s) {
                                Ok(sid) => sid,
                                Err(_) if !tt.contains(':') && tt.parse::<u32>().is_ok() => self.current(ctx).sid.ok_or("no session")?,
                                Err(error) => return Err(error),
                            }
                        };
                        let idx = if w.is_empty() {
                            if tt.contains(':') || self.find_session(ctx, tt).is_ok() { None } else { tt.parse().ok() }
                        } else {
                            w.parse().ok().or_else(|| self.find_window_in(sid, w, ctx).ok())
                        };
                        (sid, idx)
                    }
                    None => (self.current(ctx).sid.ok_or("no session")?, None),
                };
                let idx = if o.has('a') || o.has('b') {
                    let base = didx.unwrap_or(self.sessions[&dsid].active) + if o.has('a') { 1 } else { 0 };
                    self.shift_up(dsid, base);
                    base
                } else {
                    match didx {
                        Some(i) if self.sessions[&dsid].links.contains_key(&i) => {
                            if self.sessions[&dsid].links[&i] == wid {
                                return Ok(Flow::Next);
                            }
                            if !o.has('k') {
                                return err(format!("index in use: {i}"));
                            }
                            let old = self.sessions[&dsid].links[&i];
                            self.sessions.get_mut(&dsid).unwrap().links.remove(&i);
                            if self.links_of(old).is_empty() {
                                self.kill_window(old);
                            }
                            i
                        }
                        Some(i) => i,
                        None => self.next_index(dsid, None),
                    }
                };
                if cmd == "move-window" {
                    let (ssid, sidx) = (src.sid.unwrap(), src.idx.unwrap());
                    // link first so the window isn't destroyed when unlinked
                    self.link(dsid, idx, wid, !o.has('d'));
                    // the source index may have shifted (-a/-b): find the old link by window id
                    let old = self.sessions.get(&ssid).and_then(|s| s.links.iter().find(|(i, w)| **w == wid && !(ssid == dsid && **i == idx)).map(|(i, _)| *i));
                    if let Some(sidx) = old.filter(|_| ssid != dsid || sidx != idx) {
                        let s = self.sessions.get_mut(&ssid).unwrap();
                        if ssid == dsid && s.active == sidx {
                            s.active = idx;
                        }
                        self.unlink(ssid, sidx);
                    }
                } else {
                    self.link(dsid, idx, wid, !o.has('d'));
                }
                self.mark_all_dirty();
                Ok(Flow::Next)
            }
            "swap-window" => {
                let src = self.resolve_window(ctx, o.get('s'))?;
                let dst = self.resolve_window(ctx, t)?;
                let (a, b) = ((src.sid.unwrap(), src.idx.unwrap()), (dst.sid.unwrap(), dst.idx.unwrap()));
                let (wa, wb) = (src.wid.unwrap(), dst.wid.unwrap());
                self.sessions.get_mut(&a.0).unwrap().links.insert(a.1, wb);
                self.sessions.get_mut(&b.0).unwrap().links.insert(b.1, wa);
                if !o.has('d') {
                    self.select_window(b.0, b.1);
                }
                self.update_session_size(a.0);
                self.update_session_size(b.0);
                Ok(Flow::Next)
            }
            "rename-window" => {
                let wt = self.resolve_window(ctx, t)?;
                let name = o.rest.first().ok_or("rename-window needs a name")?;
                let name = self.expand(wt, ctx.client, name);
                let w = self.windows.get_mut(&wt.wid.unwrap()).unwrap();
                w.name = name;
                w.options.set("automatic-rename", "off".into());
                self.mark_window_dirty(wt.wid.unwrap());
                self.run_hook(wt.sid, "window-renamed", Ctx::default());
                Ok(Flow::Next)
            }
            "select-window" | "next-window" | "previous-window" | "last-window" => {
                let wt = self.resolve_window(ctx, t)?;
                let sid = wt.sid.unwrap();
                let s = &self.sessions[&sid];
                let keys: Vec<u32> = s.links.keys().copied().collect();
                let pos = keys.iter().position(|k| *k == s.active).unwrap_or(0);
                let alert_only = o.has('a');
                let step = |dir: i64| -> Option<u32> {
                    let n = keys.len() as i64;
                    for k in 1..=n {
                        let i = keys[((pos as i64 + dir * k).rem_euclid(n)) as usize];
                        if !alert_only || s.alerts.get(&i).is_some_and(|a| *a != 0) {
                            return Some(i);
                        }
                    }
                    None
                };
                let idx = match cmd {
                    "next-window" => step(1).ok_or("no next window")?,
                    "previous-window" => step(-1).ok_or("no previous window")?,
                    "last-window" => s.last.first().copied().ok_or("no last window")?,
                    _ if o.has('n') => step(1).ok_or("no next window")?,
                    _ if o.has('p') => step(-1).ok_or("no previous window")?,
                    _ if o.has('l') => s.last.first().copied().ok_or("no last window")?,
                    _ => {
                        let idx = wt.idx.unwrap();
                        if o.has('T') && s.active == idx {
                            s.last.first().copied().unwrap_or(idx)
                        } else {
                            idx
                        }
                    }
                };
                self.select_window(sid, idx);
                if let Some(c) = ctx.client {
                    if self.client_session(c) != Some(sid) && cmd == "select-window" && t.is_some() && ctx.from_key {
                        self.switch_client(c, sid);
                    }
                }
                Ok(Flow::Next)
            }
            "find-window" => {
                let pat = o.rest.first().ok_or("find-window needs a match string")?.clone();
                let (bn, bt, bc) = (o.has('N'), o.has('T'), o.has('C'));
                let all = !bn && !bt && !bc;
                let sid = self.resolve_session(ctx, t)?;
                let mut hits = Vec::new();
                for (idx, wid) in self.sessions[&sid].links.clone() {
                    let w = &self.windows[&wid];
                    let m = |s: &str| if o.has('r') { s.contains(&pat) } else if o.has('i') { s.to_lowercase().contains(&pat.to_lowercase()) } else { fnmatch(&format!("*{pat}*"), s) };
                    let mut hit = (all || bn) && m(&w.name);
                    for p in w.panes() {
                        if let Some(pp) = self.panes.get(&p) {
                            hit |= (all || bt) && m(&pp.title);
                            hit |= (all || bc) && m(&pp.screen().contents());
                        }
                    }
                    if hit {
                        hits.push(idx);
                    }
                }
                match hits.as_slice() {
                    [] => err(format!("no windows matching: {pat}")),
                    [one] => {
                        self.select_window(sid, *one);
                        Ok(Flow::Next)
                    }
                    _ => {
                        let pane = self.current(ctx).pane.ok_or("no pane")?;
                        let filter: Vec<WinId> = hits.iter().map(|i| self.sessions[&sid].links[i]).collect();
                        self.open_tree(pane, TreeKind::Windows, None, Some(filter), o.has('Z'));
                        Ok(Flow::Next)
                    }
                }
            }
            "respawn-window" | "respawn-pane" => {
                let tt = if cmd == "respawn-window" { self.resolve_window(ctx, t)? } else { self.resolve_pane(ctx, t)? };
                let wid = tt.wid.unwrap();
                let pane = if cmd == "respawn-window" {
                    // kill all other panes
                    let w = &self.windows[&wid];
                    let keep = w.active;
                    let alive: Vec<PaneId> = w.panes().into_iter().filter(|p| *p != keep).collect();
                    if !o.has('k') && w.panes().iter().any(|p| self.panes.get(p).is_some_and(|x| x.dead.is_none())) {
                        return err(format!("window still active: {}", w.name));
                    }
                    for p in alive {
                        self.destroy_pane(p);
                    }
                    let w = self.windows.get_mut(&wid).unwrap();
                    w.layout = Node::Leaf(keep);
                    w.zoomed = false;
                    w.last_active = None;
                    keep
                } else {
                    tt.pane.unwrap()
                };
                let p = self.panes.get_mut(&pane).ok_or("no pane")?;
                if p.dead.is_none() && !o.has('k') {
                    return err(format!("pane still active: %{pane}"));
                }
                p.kill();
                let generation = p.generation + 1;
                let size = p.size;
                let cwd = o.get('c').map(String::from).unwrap_or_else(|| p.start_path.clone());
                let cmdv = if o.rest.is_empty() && !p.start_cmd.is_empty() { vec![p.start_cmd.clone()] } else { o.rest.clone() };
                self.spawn_into(pane, generation, tt.sid, size.0, size.1, &cmdv, Some(&cwd), &env)?;
                self.apply_layout(wid);
                self.mark_window_dirty(wid);
                Ok(Flow::Next)
            }
            "rotate-window" => {
                let wt = self.resolve_window(ctx, t)?;
                let wid = wt.wid.unwrap();
                let w = self.windows.get_mut(&wid).unwrap();
                let order = w.panes();
                let mut rotated = order.clone();
                if o.has('D') { rotated.rotate_right(1); } else { rotated.rotate_left(1); }
                // Replace via temporary ids so cycles do not overwrite one another.
                for (i, p) in order.iter().enumerate() { w.layout.replace(*p, PaneId::MAX - i as u32); }
                for (i, p) in rotated.iter().enumerate() { w.layout.replace(PaneId::MAX - i as u32, *p); }
                w.pane_order = rotated;
                w.zoomed = false;
                self.apply_layout(wid);
                self.mark_window_dirty(wid);
                Ok(Flow::Next)
            }
            "select-layout" | "next-layout" | "previous-layout" => {
                let wt = if cmd == "select-layout" { self.resolve_pane(ctx, t)? } else { self.resolve_window(ctx, t)? };
                let wid = wt.wid.unwrap();
                let area = self.window_area(wid);
                let main_size = |s: &Server, name: &str| -> Option<u16> {
                    let v = s.opt_window_num(Some(wid), if name.starts_with("main-horizontal") { "main-pane-height" } else { "main-pane-width" });
                    (v > 0).then_some(v as u16)
                };
                let w = &self.windows[&wid];
                let panes = w.panes();
                let old_layout = w.layout.to_layout_string(area);
                let name: Option<String> = if cmd == "next-layout" || o.has('n') {
                    Some(layout::PRESETS[w.layout_cycle.map_or(0, |i| (i + 1) % layout::PRESETS.len())].to_string())
                } else if cmd == "previous-layout" || o.has('p') {
                    Some(layout::PRESETS[w.layout_cycle.map_or(layout::PRESETS.len() - 1, |i| (i + layout::PRESETS.len() - 1) % layout::PRESETS.len())].to_string())
                } else if o.has('E') {
                    let w = self.windows.get_mut(&wid).unwrap();
                    w.old_layout = Some(old_layout);
                    w.layout.spread(wt.pane.unwrap_or(w.active), area);
                    w.zoomed = false;
                    self.apply_layout(wid);
                    self.mark_window_dirty(wid);
                    self.run_hook(wt.sid, "window-layout-changed", Ctx::default());
                    return Ok(Flow::Next);
                } else {
                    o.rest.first().cloned().or_else(|| {
                        if o.has('o') { w.old_layout.clone() } else { w.layout_cycle.map(|i| layout::PRESETS[i].to_string()) }
                    })
                };
                let node = if let Some(name) = name {
                    if let Some(preset) = layout::preset_name(&name).filter(|_| !o.has('o')) {
                        let n = layout::preset(preset, &panes, area, main_size(self, preset)).unwrap();
                        let w = self.windows.get_mut(&wid).unwrap();
                        w.layout_cycle = layout::PRESETS.iter().position(|p| *p == preset);
                        n
                    } else {
                        layout::parse_layout_string(&name, &panes, area).map_err(|e| format!("can't set layout: {e}"))?
                    }
                } else {
                    w.layout.clone()
                };
                let w = self.windows.get_mut(&wid).unwrap();
                w.old_layout = Some(old_layout);
                w.layout = node;
                w.zoomed = false;
                self.apply_layout(wid);
                self.mark_window_dirty(wid);
                self.run_hook(wt.sid, "window-layout-changed", Ctx::default());
                Ok(Flow::Next)
            }
            "resize-window" => {
                let wt = self.resolve_window(ctx, t)?;
                let wid = wt.wid.unwrap();
                let w = self.windows.get_mut(&wid).unwrap();
                let n: u16 = o.rest.first().and_then(|v| v.parse().ok()).unwrap_or(1);
                if o.has('A') || o.has('a') {
                    w.options.unset("window-size");
                    let sid = wt.sid.unwrap();
                    self.update_session_size(sid);
                    return Ok(Flow::Next);
                }
                w.options.set("window-size", "manual".into());
                if let Some(x) = o.get('x').and_then(|v| v.parse::<u16>().ok()) {
                    w.sx = x.clamp(1, MAX_SIZE);
                }
                if let Some(y) = o.get('y').and_then(|v| v.parse::<u16>().ok()) {
                    w.sy = y.clamp(1, MAX_SIZE);
                }
                if o.has('L') {
                    w.sx = w.sx.saturating_sub(n).max(1);
                }
                if o.has('R') {
                    w.sx = w.sx.saturating_add(n).min(MAX_SIZE);
                }
                if o.has('U') {
                    w.sy = w.sy.saturating_sub(n).max(1);
                }
                if o.has('D') {
                    w.sy = w.sy.saturating_add(n).min(MAX_SIZE);
                }
                self.apply_layout(wid);
                self.mark_window_dirty(wid);
                Ok(Flow::Next)
            }
            "list-windows" => {
                let fmt = o.get('F').unwrap_or("#{window_index}: #{window_name}#{window_raw_flags} (#{window_panes} panes) [#{window_width}x#{window_height}] [layout #{window_layout}] #{window_id}#{?window_active, (active),}");
                let fmt = if o.has('a') && o.get('F').is_none() { "#{session_name}:#{window_index}: #{window_name}#{window_raw_flags} (#{window_panes} panes) [#{window_width}x#{window_height}] " } else { fmt };
                let sids: Vec<SessId> = if o.has('a') { self.sessions.keys().copied().collect() } else { vec![self.resolve_session(ctx, t)?] };
                for sid in sids {
                    for (idx, wid) in self.sessions[&sid].links.clone() {
                        let tt = Target { sid: Some(sid), idx: Some(idx), wid: Some(wid), pane: Some(self.windows[&wid].active) };
                        if o.get('f').is_some_and(|f| !truthy(&self.expand(tt, ctx.client, f))) {
                            continue;
                        }
                        let line = self.expand(tt, ctx.client, fmt);
                        ctx.out.push_str(&line);
                        ctx.out.push('\n');
                    }
                }
                Ok(Flow::Next)
            }
            // ---------------------------------------------------------------- panes
            "split-window" => {
                let tt = self.resolve_pane(ctx, t)?;
                let size = match o.get('l') {
                    Some(l) if l.ends_with('%') => {
                        let pct: u32 = l.trim_end_matches('%').parse().map_err(|_| format!("bad size: {l}"))?;
                        let (rects, _) = self.rects(tt.wid.unwrap());
                        let r = if o.has('f') { self.window_area(tt.wid.unwrap()) } else { rects.iter().find(|(p, _)| Some(*p) == tt.pane).map(|(_, r)| *r).unwrap_or_default() };
                        let total = if o.has('h') { r.w } else { r.h } as u32;
                        Some((total * pct / 100) as u16)
                    }
                    Some(l) => Some(l.parse().map_err(|_| format!("bad size: {l}"))?),
                    None => None,
                };
                let cwd = o.get('c').map(|c| self.expand(tt, ctx.client, c));
                let new = self.split_pane(tt, o.has('h'), o.has('b'), size, o.has('f'), cwd, &o.rest, &env)?;
                let wid = tt.wid.unwrap();
                if !o.has('d') {
                    self.select_pane(wid, new);
                }
                if o.has('Z') {
                    self.windows.get_mut(&wid).unwrap().zoomed = true;
                    self.apply_layout(wid);
                }
                if o.has('P') {
                    let fmt = o.get('F').unwrap_or("#{session_name}:#{window_index}.#{pane_index}");
                    let line = self.expand_for_pane(new, fmt);
                    ctx.out.push_str(&line);
                    ctx.out.push('\n');
                }
                ctx.pane = Some(new);
                Ok(Flow::Next)
            }
            "kill-pane" => {
                let tt = self.resolve_pane(ctx, t)?;
                if o.has('a') {
                    let keep = tt.pane.unwrap();
                    let others: Vec<PaneId> = self.windows[&tt.wid.unwrap()].panes().into_iter().filter(|p| *p != keep).collect();
                    for p in others {
                        self.kill_pane(p);
                    }
                } else {
                    self.kill_pane(tt.pane.unwrap());
                }
                Ok(Flow::Next)
            }
            "select-pane" => {
                let tt = self.resolve_pane(ctx, t)?;
                let wid = tt.wid.unwrap();
                let pane = tt.pane.unwrap();
                if o.has('m') || o.has('M') {
                    self.marked = if o.has('M') || self.marked == Some(pane) { None } else { Some(pane) };
                    self.mark_all_dirty();
                    return Ok(Flow::Next);
                }
                if let Some(title) = o.get('T') {
                    let title = self.expand(tt, ctx.client, title);
                    self.panes.get_mut(&pane).unwrap().title = title;
                    self.mark_window_dirty(wid);
                    return Ok(Flow::Next);
                }
                if o.has('d') || o.has('e') {
                    // input disable/enable: stored as a pane option
                    let v = if o.has('d') { "on" } else { "off" };
                    self.panes.get_mut(&pane).unwrap().options.set("@input-disabled", v.into());
                    return Ok(Flow::Next);
                }
                if o.has('g') {
                    ctx.out.push_str(&self.opt_pane(pane, "window-style"));
                    ctx.out.push('\n');
                    return Ok(Flow::Next);
                }
                if let Some((_, d)) = DIRS.iter().find(|(c, _)| o.has(*c)) {
                    let zoomed = self.windows[&wid].zoomed;
                    self.select_dir(tt, *d)?;
                    if o.has('Z') && zoomed {
                        let w = self.windows.get_mut(&wid).unwrap();
                        w.zoomed = true;
                        self.apply_layout(wid);
                    }
                    return Ok(Flow::Next);
                }
                if o.has('l') {
                    let w = &self.windows[&wid];
                    let last = w.last_active.filter(|p| w.panes().contains(p)).ok_or("no last pane")?;
                    self.select_pane(wid, last);
                    return Ok(Flow::Next);
                }
                if let (Some(sid), Some(idx)) = (tt.sid, tt.idx) {
                    if self.sessions.get(&sid).is_some_and(|s| s.active != idx) {
                        self.select_window(sid, idx);
                    }
                }
                // hooks run by select_window may have removed the pane
                if self.windows.get(&wid).is_some_and(|w| w.panes().contains(&pane)) {
                    self.select_pane(wid, pane);
                }
                Ok(Flow::Next)
            }
            "last-pane" => {
                let wt = self.resolve_window(ctx, t)?;
                let wid = wt.wid.unwrap();
                let w = &self.windows[&wid];
                let last = w.last_active.filter(|p| w.panes().contains(p)).ok_or("no last pane")?;
                if o.has('d') || o.has('e') {
                    let v = if o.has('d') { "on" } else { "off" };
                    self.panes.get_mut(&last).unwrap().options.set("@input-disabled", v.into());
                    return Ok(Flow::Next);
                }
                let zoomed = self.windows[&wid].zoomed;
                self.select_pane(wid, last);
                if o.has('Z') && zoomed {
                    self.windows.get_mut(&wid).unwrap().zoomed = true;
                    self.apply_layout(wid);
                }
                Ok(Flow::Next)
            }
            "resize-pane" => {
                let tt = self.resolve_pane(ctx, t)?;
                let wid = tt.wid.unwrap();
                let pane = tt.pane.unwrap();
                if o.has('Z') {
                    let w = self.windows.get_mut(&wid).unwrap();
                    if w.panes().len() > 1 {
                        if w.active != pane {
                            w.last_active = Some(w.active);
                            w.active = pane;
                        }
                        w.zoomed = !w.zoomed;
                        self.apply_layout(wid);
                        self.mark_window_dirty(wid);
                    }
                    return Ok(Flow::Next);
                }
                if o.has('M') {
                    if let Some(cid) = ctx.client {
                        self.start_border_drag(cid, ctx.mouse.clone());
                    }
                    return Ok(Flow::Next);
                }
                if o.has('T') {
                    if let Some(p) = self.panes.get_mut(&pane) {
                        let (_, rows) = p.size;
                        let (cy, _) = p.screen().cursor_position();
                        let lines = rows.saturating_sub(cy + 1);
                        if lines > 0 {
                            p.parser.process(format!("\x1b[{}S", lines).as_bytes());
                        }
                    }
                    return Ok(Flow::Next);
                }
                let area = self.window_area(wid);
                let parse_size = |v: &str, total: u16| -> Option<u16> {
                    if let Some(p) = v.strip_suffix('%') {
                        p.parse::<u32>().ok().map(|p| (total as u32 * p / 100) as u16)
                    } else {
                        v.parse().ok()
                    }
                };
                let x = o.get('x').and_then(|v| parse_size(v, area.w));
                let y = o.get('y').and_then(|v| parse_size(v, area.h));
                let w = self.windows.get_mut(&wid).unwrap();
                if x.is_some() || y.is_some() {
                    w.layout.set_size(pane, x, y, area);
                }
                let n: u16 = o.rest.first().and_then(|v| v.parse().ok()).unwrap_or(1);
                for (c, d) in DIRS {
                    if o.has(c) {
                        w.layout.resize(pane, d, n, area);
                    }
                }
                self.apply_layout(wid);
                self.mark_window_dirty(wid);
                self.run_hook(tt.sid, "window-layout-changed", Ctx::default());
                Ok(Flow::Next)
            }
            "swap-pane" => {
                let dst = self.resolve_pane(ctx, t)?;
                let (dwid, dpane) = (dst.wid.unwrap(), dst.pane.unwrap());
                let (swid, spane) = if o.has('U') || o.has('D') {
                    let panes = self.windows[&dwid].panes();
                    let i = panes.iter().position(|p| *p == dpane).ok_or("pane not in window")?;
                    let n = panes.len();
                    let other = if o.has('U') { panes[(i + n - 1) % n] } else { panes[(i + 1) % n] };
                    (dwid, other)
                } else {
                    let src = match o.get('s') {
                        Some(s) => self.resolve_pane(ctx, Some(s))?,
                        None => match self.marked {
                            Some(m) if Some(m) != Some(dpane) => self.target_for_pane(m),
                            _ => self.current(ctx),
                        },
                    };
                    (src.wid.unwrap(), src.pane.unwrap())
                };
                if spane == dpane {
                    return Ok(Flow::Next);
                }
                if swid == dwid {
                    let w = self.windows.get_mut(&dwid).unwrap();
                    w.pane_order = w.panes();
                    w.swap_pane_order(spane, dpane);
                    w.layout.swap(spane, dpane);
                    if !o.has('d') {
                        // tmux keeps the active pane position unless -d
                        if w.active == dpane {
                            w.active = spane;
                        } else if w.active == spane {
                            w.active = dpane;
                        }
                    }
                } else {
                    for wid in [swid, dwid] {
                        let w = self.windows.get_mut(&wid).unwrap();
                        w.pane_order = w.panes();
                        w.swap_pane_order(spane, dpane);
                    }
                    let pane_tmp: PaneId = PaneId::MAX - 1;
                    self.windows.get_mut(&swid).unwrap().layout.replace(spane, pane_tmp);
                    self.windows.get_mut(&dwid).unwrap().layout.replace(dpane, spane);
                    self.windows.get_mut(&swid).unwrap().layout.replace(pane_tmp, dpane);
                    for wid in [swid, dwid] {
                        let w = self.windows.get_mut(&wid).unwrap();
                        if w.active == spane && wid == dwid || w.active == dpane && wid == swid {
                            continue;
                        }
                        if w.active == spane {
                            w.active = dpane;
                        } else if w.active == dpane {
                            w.active = spane;
                        }
                        w.last_active = None;
                    }
                    self.apply_layout(swid);
                    self.mark_window_dirty(swid);
                }
                if !o.has('Z') {
                    self.windows.get_mut(&dwid).unwrap().zoomed = false;
                }
                self.apply_layout(dwid);
                self.mark_window_dirty(dwid);
                Ok(Flow::Next)
            }
            "break-pane" => {
                let src = self.resolve_pane(ctx, o.get('s'))?;
                let (swid, pane) = (src.wid.unwrap(), src.pane.unwrap());
                let ssid = src.sid.ok_or("no session")?;
                let (dsid, didx) = match t {
                    Some(tt) => {
                        let (s, w) = tt.split_once(':').unwrap_or((tt, ""));
                        let sid = if s.is_empty() { ssid } else { self.find_session(ctx, s)? };
                        (sid, w.parse::<u32>().ok())
                    }
                    None => (ssid, None),
                };
                let name = o.get('n').map(String::from);
                if self.windows[&swid].panes().len() == 1 {
                    // already alone: just move the window
                    if let Some(n) = name {
                        self.windows.get_mut(&swid).unwrap().name = n;
                    }
                    return Ok(Flow::Next);
                }
                let w = self.windows.get_mut(&swid).unwrap();
                let l = std::mem::replace(&mut w.layout, Node::Leaf(PaneId::MAX));
                w.layout = l.remove(pane).unwrap();
                if w.active == pane {
                    w.active = w.last_active.filter(|p| *p != pane).unwrap_or(w.panes()[0]);
                }
                w.last_active = None;
                w.zoomed = false;
                self.apply_layout(swid);
                let (sx, sy) = (self.windows[&swid].sx, self.windows[&swid].sy);
                let wname = name.clone().unwrap_or_else(|| self.panes.get(&pane).map(|p| p.current_cmd.clone()).unwrap_or_default());
                let nwid = self.new_window_obj(wname, pane, sx, sy);
                if name.is_some() {
                    self.windows.get_mut(&nwid).unwrap().options.set("automatic-rename", "off".into());
                }
                let idx = if o.has('a') || o.has('b') {
                    let at = didx.unwrap_or(self.sessions[&dsid].active) + if o.has('a') { 1 } else { 0 };
                    self.shift_up(dsid, at);
                    at
                } else {
                    didx.filter(|i| !self.sessions[&dsid].links.contains_key(i)).unwrap_or_else(|| self.next_index(dsid, None))
                };
                self.link(dsid, idx, nwid, !o.has('d'));
                self.apply_layout(nwid);
                self.mark_all_dirty();
                if o.has('P') {
                    let fmt = o.get('F').unwrap_or("#{session_name}:#{window_index}.#{pane_index}");
                    let line = self.expand_for_pane(pane, fmt);
                    ctx.out.push_str(&line);
                    ctx.out.push('\n');
                }
                Ok(Flow::Next)
            }
            "join-pane" | "move-pane" => {
                let src = match o.get('s') {
                    Some(s) => self.resolve_pane(ctx, Some(s))?,
                    None => self.target_for_pane(self.marked.ok_or("no source pane (use -s or mark a pane)")?),
                };
                let dst = self.resolve_pane(ctx, t)?;
                let (swid, spane) = (src.wid.unwrap(), src.pane.unwrap());
                let (dwid, dpane) = (dst.wid.unwrap(), dst.pane.unwrap());
                if spane == dpane {
                    return err("source and target panes must be different");
                }
                let horizontal = o.has('h');
                let (rects, _) = self.rects(dwid);
                let r = if o.has('f') { self.window_area(dwid) } else { rects.iter().find(|(p, _)| *p == dpane).map(|(_, r)| *r).ok_or("target pane not visible")? };
                let total = if horizontal { r.w } else { r.h };
                if total < 3 {
                    return err("target pane too small");
                }
                let size = match o.get('l') {
                    Some(l) if l.ends_with('%') => l.trim_end_matches('%').parse::<u32>().ok().map(|p| (total as u32 * p / 100) as u16),
                    Some(l) => l.parse().ok(),
                    None => None,
                };
                // detach from source window
                let sw = self.windows.get_mut(&swid).unwrap();
                let l = std::mem::replace(&mut sw.layout, Node::Leaf(PaneId::MAX));
                match l.remove(spane) {
                    Some(n) => {
                        sw.layout = n;
                        if sw.active == spane {
                            sw.active = sw.panes()[0];
                        }
                        sw.last_active = sw.last_active.filter(|p| *p != spane);
                        sw.zoomed = false;
                        self.apply_layout(swid);
                    }
                    None => {
                        // source window becomes empty: unlink it everywhere (sessions, clients and
                        // hooks handled normally) without killing the moved pane
                        sw.layout = Node::Leaf(PaneId::MAX);
                        while let Some(&(sid, idx)) = self.links_of(swid).first() {
                            self.unlink(sid, idx);
                        }
                        self.windows.remove(&swid);
                    }
                }
                let dw = self.windows.get_mut(&dwid).unwrap();
                dw.insert_pane(dpane, spane, o.has('b'));
                if o.has('f') {
                    let old = std::mem::replace(&mut dw.layout, Node::Leaf(PaneId::MAX));
                    dw.layout = if o.has('b') {
                        Node::Split { horizontal, ratio: 0.5, a: Box::new(Node::Leaf(spane)), b: Box::new(old) }
                    } else {
                        Node::Split { horizontal, ratio: 0.5, a: Box::new(old), b: Box::new(Node::Leaf(spane)) }
                    };
                } else {
                    dw.layout.split_ex(dpane, spane, horizontal, o.has('b'), size, r);
                }
                dw.zoomed = false;
                if !o.has('d') {
                    dw.last_active = Some(dw.active);
                    dw.active = spane;
                }
                self.apply_layout(dwid);
                self.mark_all_dirty();
                Ok(Flow::Next)
            }
            "list-panes" => {
                let fmt = o.get('F').unwrap_or("#{pane_index}: [#{pane_width}x#{pane_height}] [history #{history_size}/#{history_limit}] #{pane_id}#{?pane_active, (active),}");
                let mut targets: Vec<(SessId, u32, WinId)> = Vec::new();
                if o.has('a') {
                    for s in self.sessions.values() {
                        for (i, w) in &s.links {
                            targets.push((s.id, *i, *w));
                        }
                    }
                } else if o.has('s') {
                    let sid = self.resolve_session(ctx, t)?;
                    for (i, w) in &self.sessions[&sid].links {
                        targets.push((sid, *i, *w));
                    }
                } else {
                    let wt = self.resolve_window(ctx, t)?;
                    targets.push((wt.sid.unwrap(), wt.idx.unwrap(), wt.wid.unwrap()));
                }
                let fmt = if (o.has('a') || o.has('s')) && o.get('F').is_none() { "#{session_name}:#{window_index}.#{pane_index}: [#{pane_width}x#{pane_height}] [history #{history_size}/#{history_limit}] #{pane_id}#{?pane_active, (active),}" } else { fmt };
                for (sid, idx, wid) in targets {
                    for p in self.windows[&wid].panes() {
                        let tt = Target { sid: Some(sid), idx: Some(idx), wid: Some(wid), pane: Some(p) };
                        if o.get('f').is_some_and(|f| !truthy(&self.expand(tt, ctx.client, f))) {
                            continue;
                        }
                        let line = self.expand(tt, ctx.client, fmt);
                        ctx.out.push_str(&line);
                        ctx.out.push('\n');
                    }
                }
                Ok(Flow::Next)
            }
            "capture-pane" => {
                let tt = self.resolve_pane(ctx, t)?;
                let p = self.panes.get(&tt.pane.unwrap()).ok_or("no pane")?;
                let screen = p.screen();
                // only the requested rows are read; plain text skips per-cell conversion
                let total = screen.history_rows_len() as i64;
                let hist = screen.scrollback_len() as i64;
                let rows = p.size.1 as i64;
                let parse = |v: Option<&str>, default: i64| -> i64 {
                    match v {
                        Some("-") => i64::MIN,
                        Some(s) => s.parse().unwrap_or(default),
                        None => default,
                    }
                };
                let start = parse(o.get('S'), 0);
                let end = parse(o.get('E'), rows - 1);
                let to_abs = |v: i64| -> i64 { if v == i64::MIN { 0 } else { (hist + v).clamp(0, total - 1) } };
                let (a, b) = (to_abs(start), if o.get('E') == Some("-") { total - 1 } else { to_abs(end) });
                let mut out = String::new();
                let mut i = a;
                while i <= b {
                    let (text, wrapped) = if o.has('e') || o.has('N') {
                        let line = crate::cells::line_at(screen, i as usize);
                        let text = if o.has('e') { render::line_with_escapes(&line) } else { line.cells.iter().map(|c| c.text.as_str()).collect() };
                        (text, line.wrapped)
                    } else {
                        crate::cells::text_at(screen, i as usize)
                    };
                    out.push_str(&text);
                    if !(o.has('J') && wrapped) {
                        out.push('\n');
                    }
                    i += 1;
                }
                if o.has('p') {
                    ctx.out.push_str(&out);
                } else {
                    self.add_buffer(o.get('b').map(String::from), out.into_bytes(), false);
                }
                Ok(Flow::Next)
            }
            "clear-history" => {
                let tt = self.resolve_pane(ctx, t)?;
                if let Some(p) = self.panes.get_mut(&tt.pane.unwrap()) {
                    p.parser.screen_mut().clear_scrollback();
                }
                Ok(Flow::Next)
            }
            "pipe-pane" => {
                let tt = self.resolve_pane(ctx, t)?;
                let pane = tt.pane.unwrap();
                let had = self.panes.get(&pane).is_some_and(|p| p.pipe.is_some());
                if let Some(p) = self.panes.get_mut(&pane) {
                    p.pipe = None;
                }
                let Some(command) = o.rest.first() else { return Ok(Flow::Next) };
                if o.has('o') && had {
                    return Ok(Flow::Next);
                }
                let command = self.expand(tt, ctx.client, command);
                let mut c = procinfo::job_command(&command);
                let to_cmd = o.has('O') || !o.has('I');
                c.stdin(if to_cmd { std::process::Stdio::piped() } else { std::process::Stdio::null() });
                c.stdout(if o.has('I') { std::process::Stdio::piped() } else { std::process::Stdio::null() });
                c.stderr(std::process::Stdio::null());
                let mut child = c.spawn().map_err(|e| format!("pipe-pane: {e}"))?;
                if to_cmd {
                    let mut stdin = child.stdin.take().unwrap();
                    let (ptx, prx) = mpsc::channel::<Vec<u8>>();
                    thread::spawn(move || {
                        for d in prx {
                            if stdin.write_all(&d).is_err() {
                                break;
                            }
                        }
                    });
                    self.panes.get_mut(&pane).unwrap().pipe = Some(ptx);
                }
                if o.has('I') {
                    let mut stdout = child.stdout.take().unwrap();
                    let input = self.panes[&pane].input.clone();
                    thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        while let Ok(n) = stdout.read(&mut buf) {
                            if n == 0 || input.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                    });
                }
                thread::spawn(move || {
                    let _ = child.wait();
                });
                Ok(Flow::Next)
            }
            "display-panes" => {
                let cid = self.resolve_client(ctx, t)?;
                // like tmux, nothing happens while another overlay is shown
                if self.clients.get(&cid).is_some_and(|c| c.overlay.is_some()) {
                    return Ok(Flow::Next);
                }
                let sid = self.client_session(cid);
                let ms = o.get('d').and_then(|v| v.parse().ok()).unwrap_or_else(|| self.opt_window_num(sid.and_then(|s| self.sessions.get(&s)).map(|s| s.active_window()), "display-panes-time").max(1) as u64);
                let template = o.rest.first().cloned().unwrap_or_else(|| "select-pane -t '%%'".into());
                let epoch = self.replace_overlay(cid, Some(Overlay::DisplayPanes { until: Instant::now() + Duration::from_millis(ms), template }))?;
                if o.has('b') { Ok(Flow::Next) } else { Ok(Flow::WaitOverlay(cid, epoch)) }
            }
            // ---------------------------------------------------------------- modes
            "copy-mode" => {
                let tt = self.resolve_pane(ctx, t)?;
                let mut pane = tt.pane.unwrap();
                if o.has('q') {
                    self.exit_mode(pane);
                    return Ok(Flow::Next);
                }
                if o.has('M') {
                    // the pane under the mouse, for a client attached to that session
                    let Some(m) = ctx.mouse.clone() else { return Ok(Flow::Next) };
                    let Some(p) = self.mouse_pane(&m) else { return Ok(Flow::Next) };
                    if ctx.client.is_none() || ctx.client.and_then(|c| self.client_session(c)) != m.sid {
                        return Ok(Flow::Next);
                    }
                    pane = p;
                }
                let already = matches!(self.panes.get(&pane).and_then(|p| p.mode.as_ref()), Some(PaneMode::Copy(_)));
                if !already {
                    self.enter_copy_mode(pane);
                    if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                        cm.set_exit_on_bottom(o.has('e'));
                        if o.has('H') {
                            cm.set_show_position(false);
                        }
                    }
                    if o.has('M') {
                        if let (Some(cid), Some(m)) = (ctx.client, ctx.mouse.clone()) {
                            self.start_copy_drag(cid, &m);
                        }
                    }
                }
                if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                    if o.has('u') {
                        cm.run("page-up", &[]);
                    }
                    if o.has('d') {
                        let exit = o.has('e');
                        if cm.page_down_exit(exit) {
                            self.exit_mode(pane);
                        }
                    }
                }
                if let Some(w) = tt.wid {
                    self.mark_window_dirty(w);
                }
                Ok(Flow::Next)
            }
            "clock-mode" => {
                let tt = self.resolve_pane(ctx, t)?;
                let pane = tt.pane.unwrap();
                self.panes.get_mut(&pane).unwrap().mode = Some(PaneMode::Clock);
                self.mark_window_dirty(tt.wid.unwrap());
                Ok(Flow::Next)
            }
            "choose-tree" | "choose-buffer" | "choose-client" | "customize-mode" => {
                let tt = self.resolve_pane(ctx, t)?;
                let kind = match cmd {
                    "choose-buffer" => TreeKind::Buffers,
                    "choose-client" => TreeKind::Clients,
                    "customize-mode" => TreeKind::Options,
                    _ if o.has('s') => TreeKind::Sessions,
                    _ => TreeKind::Windows,
                };
                let template = o.rest.first().cloned();
                self.open_tree(tt.pane.unwrap(), kind, template, None, o.has('Z'));
                // remember an attached client (a command-line client is gone by the time a
                // choice is made)
                if let Some(cid) = ctx.client.filter(|c| self.clients.get(c).is_some_and(|c| c.attached)) {
                    if let Some(PaneMode::Tree(tr)) = self.panes.get_mut(&tt.pane.unwrap()).and_then(|p| p.mode.as_mut()) {
                        tr.client = Some(cid);
                    }
                }
                Ok(Flow::Next)
            }
            "display-menu" => {
                let cid = self.resolve_client(ctx, o.get('c'))?;
                if self.clients.get(&cid).is_some_and(|c| c.overlay.is_some()) {
                    return Ok(Flow::Next);
                }
                let tt = self.resolve_pane(ctx, t).unwrap_or_else(|_| self.current(ctx));
                let start = match o.get('C') {
                    Some("-") => None,
                    Some(v) => Some(v.parse::<usize>().map_err(|_| format!("starting choice {v} invalid"))?),
                    None => Some(0),
                };
                let mut items: Vec<MenuItem> = Vec::new();
                let mut it = o.rest.iter();
                while let Some(name) = it.next() {
                    if name.is_empty() {
                        // a line: never first or twice in a row (menu_add_item)
                        if items.last().is_some_and(|l| !l.name.is_empty()) {
                            items.push(MenuItem { name: String::new(), key: String::new(), cmd: String::new() });
                        }
                        continue;
                    }
                    let (Some(key), Some(command)) = (it.next(), it.next()) else { return err("not enough arguments") };
                    // names and commands are expanded now, while the mouse event is known
                    let name = self.expand(tt, Some(cid), name);
                    if name.is_empty() {
                        continue;
                    }
                    if let Some(rest) = name.strip_prefix('-') {
                        items.push(MenuItem { name: rest.to_string(), key: String::new(), cmd: String::new() });
                    } else {
                        let key = keys::parse_key_name(key).map(|k| keys::key_name(&k)).unwrap_or_else(|| key.clone());
                        items.push(MenuItem { name, key, cmd: self.expand(tt, Some(cid), command) });
                    }
                }
                if items.is_empty() {
                    return Ok(Flow::Next);
                }
                let title = o.get('T').map(|s| self.expand(tt, Some(cid), s)).unwrap_or_default();
                let (w, h) = Menu::box_size(&title, &items);
                let Some((x, y)) = self.overlay_position(cid, tt, ctx.mouse.as_ref(), o.get('x'), o.get('y'), w, h) else { return Ok(Flow::Next) };
                let nomouse = ctx.mouse.is_none() && !o.has('M');
                let menu = Menu::at(title, items, x, y, tt.pane, nomouse, o.has('O'), start);
                let epoch = self.replace_overlay(cid, Some(Overlay::Menu(menu)))?;
                Ok(Flow::WaitOverlay(cid, epoch))
            }
            "display-popup" => {
                let cid = self.resolve_client(ctx, o.get('c'))?;
                if o.has('C') {
                    if matches!(self.clients.get(&cid).and_then(|c| c.overlay.as_ref()), Some(Overlay::Popup(_))) {
                        self.replace_overlay(cid, None)?;
                    }
                    return Ok(Flow::Next);
                }
                if self.clients.get(&cid).is_some_and(|c| c.overlay.is_some()) {
                    return Ok(Flow::Next);
                }
                let (cw, ch) = self.clients[&cid].size;
                let dim = |v: Option<&str>, total: u16| -> u16 {
                    match v {
                        Some(s) if s.ends_with('%') => (total as u32 * s.trim_end_matches('%').parse::<u32>().unwrap_or(50) / 100) as u16,
                        Some(s) => s.parse().unwrap_or(total / 2),
                        None => total / 2,
                    }
                };
                if cw < 3 || ch < 3 {
                    return err("client too small for a popup");
                }
                let w = dim(o.get('w'), cw).clamp(3, cw);
                let h = dim(o.get('h'), ch).clamp(3, ch);
                let x = o.get('x').and_then(|v| v.parse().ok()).unwrap_or(((cw - 1) / 2).saturating_sub(w / 2)).min(cw - w);
                let y = o.get('y').and_then(|v| v.parse().ok()).unwrap_or(((ch - 1) / 2).saturating_sub(h / 2)).min(ch - h);
                let border = !o.has('B');
                let (iw, ih) = if border { (w - 2, h - 2) } else { (w, h) };
                let tt = self.resolve_pane(ctx, t).unwrap_or_else(|_| self.current(ctx));
                let cwd = o.get('d').map(|d| self.expand(tt, Some(cid), d)).or_else(|| tt.pane.and_then(|p| self.panes.get(&p)).map(|p| p.current_path.clone()));
                let pane = self.spawn_pane(tt.sid, iw.max(1), ih.max(1), &o.rest, cwd.as_deref(), &env)?;
                let title = o.get('T').map(|s| self.expand(tt, Some(cid), s)).unwrap_or_default();
                let epoch = self.replace_overlay(cid, Some(Overlay::Popup(Popup { pane, x, y, w, h, border, close_on_exit: o.has('E'), title })))?;
                Ok(Flow::WaitOverlay(cid, epoch))
            }
            // ---------------------------------------------------------------- prompts & messages
            "command-prompt" => {
                let cid = self.resolve_client(ctx, t)?;
                let tt = self.current(&Ctx { client: Some(cid), ..ctx.clone() });
                let template = o.rest.first().cloned().unwrap_or_else(|| "%1".into());
                let labels: Vec<String> = match o.get('p') {
                    Some(p) => p.split(',').map(|s| self.expand(tt, Some(cid), s)).collect(),
                    None => vec![if o.rest.is_empty() { ":".into() } else { format!("({})", template.split_whitespace().next().unwrap_or("")) }],
                };
                let inits: Vec<String> = match o.get('I') {
                    Some(i) => i.split(',').map(|s| self.expand(tt, Some(cid), s)).collect(),
                    None => Vec::new(),
                };
                let prompts: Vec<(String, String)> = labels.into_iter().enumerate().map(|(i, l)| (l, inits.get(i).cloned().unwrap_or_default())).collect();
                let kind = if o.has('k') { PromptKind::Key } else { PromptKind::Command };
                self.open_prompt(cid, kind, prompts, template, ctx.pane, o.has('N'), o.has('1'));
                Ok(Flow::Next)
            }
            "confirm-before" => {
                let cid = self.resolve_client(ctx, t)?;
                let command = o.rest.first().ok_or("confirm-before needs a command")?.clone();
                let tt = self.current(&Ctx { client: Some(cid), ..ctx.clone() });
                let label = match o.get('p') {
                    Some(p) => self.expand(tt, Some(cid), p),
                    None => format!("Confirm '{}'? (y/n)", command),
                };
                self.open_prompt(cid, PromptKind::Confirm, vec![(label, String::new())], command, ctx.pane, false, true);
                Ok(Flow::Next)
            }
            "display-message" => {
                let tt = self.resolve_pane(ctx, t).unwrap_or_else(|_| self.current(ctx));
                let cid = o.get('c').map(|c| self.resolve_client(ctx, Some(c))).transpose()?
                    .or_else(|| ctx.client.filter(|id| self.clients.get(id).is_some_and(|c| c.attached)))
                    .or_else(|| self.resolve_client(ctx, None).ok()).or(ctx.client);
                if o.has('a') {
                    let mut names: Vec<&str> = VAR_NAMES.to_vec();
                    names.sort();
                    let vc = super::vars::VarCtx { srv: self, t: tt, client: cid, extra: HashMap::new() };
                    for n in names {
                        let v = crate::format::Vars::get(&vc, n).unwrap_or_default();
                        ctx.out.push_str(&format!("{n}={v}\n"));
                    }
                    return Ok(Flow::Next);
                }
                let fmt = o.rest.first().map(String::as_str).unwrap_or("[#{session_name}] #{window_index}:#{window_name}, current pane #{pane_index} - (%H:%M %d-%b-%y)");
                let msg = self.expand_time(tt, cid, fmt);
                let attached_client = cid.filter(|c| self.clients.get(c).is_some_and(|c| c.attached));
                if o.has('p') || attached_client.is_none() {
                    ctx.out.push_str(&msg);
                    ctx.out.push('\n');
                } else {
                    let c = attached_client.unwrap();
                    self.set_message(c, &msg);
                    if let Some(ms) = o.get('d').and_then(|v| v.parse::<u64>().ok()) {
                        if let Some(cl) = self.clients.get_mut(&c) {
                            if let Some(m) = &mut cl.message {
                                m.1 = Instant::now() + Duration::from_millis(if ms == 0 { 86_400_000 } else { ms });
                            }
                        }
                    }
                }
                Ok(Flow::Next)
            }
            "show-messages" => {
                for (t, m) in &self.messages {
                    ctx.out.push_str(&format!("{}: {m}\n", crate::format::expand_time(&format!("#{{t:@t}}"), &HashMap::from([("@t".to_string(), t.to_string())]))));
                }
                Ok(Flow::Next)
            }
            "clear-prompt-history" => {
                self.prompt_history.clear();
                Ok(Flow::Next)
            }
            "show-prompt-history" => {
                for h in &self.prompt_history {
                    ctx.out.push_str(h);
                    ctx.out.push('\n');
                }
                Ok(Flow::Next)
            }
            // ---------------------------------------------------------------- keys
            "bind-key" => {
                let mut args = o.rest.clone();
                if args.is_empty() {
                    return err("bind-key needs a key");
                }
                let key = args.remove(0);
                let kname = keys::parse_key_name(&key).map(|k| keys::key_name(&k)).unwrap_or(key.clone());
                let table = if o.has('n') { "root".to_string() } else { o.get('T').unwrap_or("prefix").to_string() };
                let command = if args.len() == 1 { args[0].clone() } else { args.iter().map(|a| if a == ";" { "\\;".into() } else { cmdparse::quote(a) }).collect::<Vec<_>>().join(" ") };
                if command.is_empty() {
                    return err("bind-key needs a command");
                }
                self.bindings.entry(table).or_default().insert(kname, Binding { cmd: command, repeat: o.has('r'), note: o.get('N').unwrap_or("").to_string() });
                Ok(Flow::Next)
            }
            "unbind-key" => {
                let table = if o.has('n') { "root".to_string() } else { o.get('T').unwrap_or("prefix").to_string() };
                if o.has('a') {
                    if let Some(tb) = self.bindings.get_mut(&table) {
                        tb.clear();
                    }
                    return Ok(Flow::Next);
                }
                let key = o.rest.first().ok_or("unbind-key needs a key")?;
                let kname = keys::parse_key_name(key).map(|k| keys::key_name(&k)).unwrap_or(key.clone());
                let removed = self.bindings.get_mut(&table).and_then(|t| t.remove(&kname)).is_some();
                if !removed && !o.has('q') {
                    // tmux is silent here too
                }
                Ok(Flow::Next)
            }
            "list-keys" => {
                let only_table = o.get('T');
                let only_key = o.rest.first().map(|k| keys::parse_key_name(k).map(|k| keys::key_name(&k)).unwrap_or(k.clone()));
                for (table, tb) in &self.bindings {
                    if only_table.is_some_and(|t| t != table) {
                        continue;
                    }
                    for (key, b) in tb {
                        if only_key.as_ref().is_some_and(|k| k != key) {
                            continue;
                        }
                        if o.has('N') {
                            if b.note.is_empty() && !o.has('a') {
                                continue;
                            }
                            let prefix = if table == "prefix" { format!("{} ", self.opt_session(None, "prefix")) } else { String::new() };
                            ctx.out.push_str(&format!("{}{prefix}{key:<10} {}\n", o.get('P').unwrap_or(""), if b.note.is_empty() { &b.cmd } else { &b.note }));
                        } else {
                            ctx.out.push_str(&format!("bind-key {}-T {table} {} {}\n", if b.repeat { "-r " } else { "   " }, cmdparse::quote(key), b.cmd));
                        }
                        if o.has('1') {
                            return Ok(Flow::Next);
                        }
                    }
                }
                Ok(Flow::Next)
            }
            "send-keys" | "send-prefix" => {
                let tt = self.resolve_pane(ctx, t)?;
                let pane = tt.pane.unwrap();
                if cmd == "send-prefix" {
                    let name = self.opt_session(tt.sid, if o.has('2') { "prefix2" } else { "prefix" });
                    if let Some(k) = keys::parse_key_name(&name) {
                        let app = self.panes.get(&pane).is_some_and(|p| p.screen().application_cursor());
                        self.panes[&pane].send(keys::encode_key(&k, app));
                    }
                    return Ok(Flow::Next);
                }
                if o.has('X') {
                    let name = o.rest.first().ok_or("send-keys -X needs a command")?.clone();
                    let rest: Vec<String> = o.rest[1..].iter().map(|a| if o.has('F') { self.expand(tt, ctx.client, a) } else { a.clone() }).collect();
                    let n: usize = o.get('N').and_then(|v| v.parse().ok()).unwrap_or(1);
                    if let Some(m) = ctx.mouse.clone() {
                        // from a mouse binding: the cursor goes to the mouse first, and
                        // begin-selection starts a drag (window_copy_command)
                        self.copy_move_mouse(&m);
                        if name == "begin-selection" {
                            if let Some(cid) = ctx.client {
                                self.start_copy_drag(cid, &m);
                            }
                            return Ok(Flow::Next);
                        }
                    }
                    for _ in 0..n {
                        self.copy_mode_command(pane, ctx.client, &name, &rest);
                    }
                    return Ok(Flow::Next);
                }
                if o.has('M') {
                    let m = ctx.mouse.clone().ok_or("no mouse target")?;
                    let p = self.mouse_pane(&m).ok_or("no mouse target")?;
                    self.window_pane_mouse(p, ctx.client, &m);
                    return Ok(Flow::Next);
                }
                if o.has('R') {
                    if let Some(p) = self.panes.get_mut(&pane) {
                        p.parser.process(b"\x1bc");
                    }
                    self.mark_window_dirty(tt.wid.unwrap());
                }
                let n: usize = o.get('N').and_then(|v| v.parse().ok()).unwrap_or(1);
                let app = self.panes.get(&pane).is_some_and(|p| p.screen().application_cursor());
                let mut data = Vec::new();
                for k in &o.rest {
                    let k = if o.has('F') { self.expand(tt, ctx.client, k) } else { k.clone() };
                    if o.has('H') {
                        for h in k.split_whitespace() {
                            data.push(u8::from_str_radix(h, 16).map_err(|_| format!("invalid hex: {h}"))?);
                        }
                        continue;
                    }
                    if o.has('l') {
                        data.extend(k.as_bytes());
                        continue;
                    }
                    match keys::parse_key_name(&k) {
                        Some(key) if k.chars().count() > 1 || k == " " => data.extend(keys::encode_key(&key, app)),
                        _ => data.extend(k.as_bytes()),
                    }
                }
                let mut all = Vec::new();
                for _ in 0..n {
                    all.extend_from_slice(&data);
                }
                // keys sent to a pane in copy mode are interpreted by the mode
                let in_mode = self.panes.get(&pane).is_some_and(|p| p.mode.is_some());
                if in_mode && !o.has('l') && !o.has('H') {
                    for k in &o.rest {
                        for _ in 0..n {
                            self.mode_key(pane, ctx.client, k);
                        }
                    }
                } else if let Some(p) = self.panes.get(&pane) {
                    p.send(all);
                }
                Ok(Flow::Next)
            }
            // ---------------------------------------------------------------- buffers
            "set-buffer" => {
                let name = o.get('b').map(String::from);
                if let Some(newname) = o.get('n') {
                    let b = match &name {
                        Some(n) => self.buffers.iter_mut().find(|b| &b.name == n),
                        None => self.buffers.first_mut(),
                    }
                    .ok_or("no buffer")?;
                    b.name = newname.to_string();
                    b.automatic = false;
                    return Ok(Flow::Next);
                }
                let data = o.rest.first().ok_or("no data specified")?.clone();
                if o.has('w') {
                    if let Some(c) = ctx.client.and_then(|c| self.clients.get(&c)) {
                        c.send(S_OUTPUT, format!("\x1b]52;c;{}\x07", base64(data.as_bytes())).as_bytes());
                    }
                }
                self.add_buffer(name, data.into_bytes(), o.has('a'));
                Ok(Flow::Next)
            }
            "delete-buffer" => {
                match o.get('b') {
                    Some(n) => {
                        let pos = self.buffers.iter().position(|b| b.name == n).ok_or_else(|| format!("unknown buffer: {n}"))?;
                        self.buffers.remove(pos);
                    }
                    None => {
                        if self.buffers.is_empty() {
                            return err("no buffer");
                        }
                        self.buffers.remove(0);
                    }
                }
                Ok(Flow::Next)
            }
            "list-buffers" => {
                let fmt = o.get('F').unwrap_or("#{buffer_name}: #{buffer_size} bytes: \"#{buffer_sample}\"");
                let lines: Vec<String> = self.buffers.iter().map(|b| self.expand_extra(Target::default(), ctx.client, fmt, buffer_vars(b))).collect();
                for l in lines {
                    ctx.out.push_str(&l);
                    ctx.out.push('\n');
                }
                Ok(Flow::Next)
            }
            "show-buffer" => {
                let b = match o.get('b') {
                    Some(n) => self.buffers.iter().find(|b| b.name == n).ok_or_else(|| format!("unknown buffer: {n}"))?,
                    None => self.buffers.first().ok_or("no buffers")?,
                };
                ctx.out.push_str(&String::from_utf8_lossy(&b.data));
                Ok(Flow::Next)
            }
            "save-buffer" => {
                let b = match o.get('b') {
                    Some(n) => self.buffers.iter().find(|b| b.name == n).ok_or_else(|| format!("unknown buffer: {n}"))?,
                    None => self.buffers.first().ok_or("no buffers")?,
                };
                let path = o.rest.first().ok_or("save-buffer needs a path")?;
                if path == "-" {
                    ctx.out.push_str(&String::from_utf8_lossy(&b.data));
                    return Ok(Flow::Next);
                }
                let path = self.resolve_path(ctx, path);
                let mut f = std::fs::OpenOptions::new().create(true).write(true).append(o.has('a')).truncate(!o.has('a')).open(&path).map_err(|e| format!("{path}: {e}"))?;
                f.write_all(&b.data).map_err(|e| format!("{path}: {e}"))?;
                Ok(Flow::Next)
            }
            "load-buffer" => {
                let path = o.rest.first().ok_or("load-buffer needs a path")?;
                let data = if path.starts_with(STDIN_BUFFER_PREFIX) {
                    decode_stdin_buffer(path)?
                } else if path.starts_with("\x01stdin:") {
                    return err("unsafe legacy stdin buffer encoding; update the client");
                } else {
                    let p = self.resolve_path(ctx, path);
                    std::fs::read(&p).map_err(|e| format!("{p}: {e}"))?
                };
                if o.has('w') {
                    if let Some(c) = ctx.client.and_then(|c| self.clients.get(&c)) {
                        c.send(S_OUTPUT, format!("\x1b]52;c;{}\x07", base64(&data)).as_bytes());
                    }
                }
                self.add_buffer(o.get('b').map(String::from), data, false);
                Ok(Flow::Next)
            }
            "paste-buffer" => {
                let tt = self.resolve_pane(ctx, t)?;
                let idx = match o.get('b') {
                    Some(n) => self.buffers.iter().position(|b| b.name == n).ok_or_else(|| format!("unknown buffer: {n}"))?,
                    None => {
                        if self.buffers.is_empty() {
                            return Ok(Flow::Next);
                        }
                        0
                    }
                };
                let data = self.buffers[idx].data.clone();
                self.paste(tt.pane.unwrap(), &data, o.has('p'), o.has('r'), o.get('s'));
                if o.has('d') {
                    self.buffers.remove(idx);
                }
                Ok(Flow::Next)
            }
            // ---------------------------------------------------------------- options & env
            "set-option" | "set-window-option" => self.cmd_set_option(cmd == "set-window-option", &o, ctx).map(|_| Flow::Next),
            "show-options" | "show-window-options" => self.cmd_show_options(cmd == "show-window-options", &o, ctx).map(|_| Flow::Next),
            "set-environment" => {
                let name = o.rest.first().ok_or("set-environment needs a name")?.clone();
                let value = o.rest.get(1).map(|v| if o.has('F') { self.expand(self.current(ctx), ctx.client, v) } else { v.clone() });
                let map = if o.has('g') { &mut self.env } else { &mut self.sessions.get_mut(&self.resolve_session(ctx, t)?).unwrap().env };
                if o.has('u') {
                    map.remove(&name);
                } else if o.has('r') {
                    map.insert(name, None);
                } else {
                    map.insert(name, Some(value.ok_or("no value specified")?));
                }
                Ok(Flow::Next)
            }
            "show-environment" => {
                let map = if o.has('g') { self.env.clone() } else { self.sessions[&self.resolve_session(ctx, t)?].env.clone() };
                for (k, v) in map {
                    if o.rest.first().is_some_and(|n| *n != k) {
                        continue;
                    }
                    match (v, o.has('s')) {
                        (Some(v), false) => ctx.out.push_str(&format!("{k}={v}\n")),
                        (Some(v), true) => ctx.out.push_str(&format!("{k}={}; export {k};\n", cmdparse::quote(&v))),
                        (None, false) => ctx.out.push_str(&format!("-{k}\n")),
                        (None, true) => ctx.out.push_str(&format!("unset {k};\n")),
                    }
                }
                Ok(Flow::Next)
            }
            "set-hook" => {
                let name = o.rest.first().ok_or("set-hook needs a hook name")?.clone();
                let base = name.split('[').next().unwrap().to_string();
                if o.has('R') {
                    let sid = self.current(ctx).sid;
                    self.run_hook(sid, &base, ctx.clone());
                    return Ok(Flow::Next);
                }
                let map = if o.has('g') || self.resolve_session(ctx, t).is_err() { &mut self.hooks } else { &mut self.sessions.get_mut(&self.resolve_session(ctx, t)?).unwrap().hooks };
                if o.has('u') {
                    map.remove(&base);
                    return Ok(Flow::Next);
                }
                let command = o.rest.get(1).ok_or("set-hook needs a command")?.clone();
                let e = map.entry(base).or_default();
                if o.has('a') {
                    e.push(command);
                } else {
                    *e = vec![command];
                }
                Ok(Flow::Next)
            }
            "show-hooks" => {
                let map = if o.has('g') { self.hooks.clone() } else { self.resolve_session(ctx, t).ok().map(|s| self.sessions[&s].hooks.clone()).unwrap_or_default() };
                for (k, v) in map {
                    for (i, c) in v.iter().enumerate() {
                        ctx.out.push_str(&format!("{k}[{i}] {c}\n"));
                    }
                }
                Ok(Flow::Next)
            }
            // ---------------------------------------------------------------- scripting
            "source-file" => {
                let mut injected = Vec::new();
                for pat in &o.rest {
                    let pat = if o.has('F') { self.expand(self.current(ctx), ctx.client, pat) } else { pat.clone() };
                    let path = self.resolve_path(ctx, &pat);
                    let files = glob_files(&path);
                    if files.is_empty() && !o.has('q') {
                        return err(format!("{path}: No such file or directory"));
                    }
                    for f in files {
                        let text = match std::fs::read_to_string(&f) {
                            Ok(t) => t,
                            Err(e) if o.has('q') => {
                                let _ = e;
                                continue;
                            }
                            Err(e) => return err(format!("{f}: {e}")),
                        };
                        // #{current_file} is this file while its %if lines are evaluated
                        let saved_file = self.cur_file.replace(f.clone());
                        let parsed = {
                            let cond = |c: &str| truthy(&self.expand(self.current(ctx), ctx.client, c));
                            let env = |n: &str| self.env.get(n).cloned().flatten();
                            cmdparse::parse_with_env(&text, &cond, &env)
                        };
                        self.cur_file = saved_file;
                        let cmds = parsed.map_err(|e| format!("{f}:{e}"))?;
                        if o.has('n') {
                            continue;
                        }
                        if o.has('v') {
                            for c in &cmds {
                                ctx.out.push_str(&format!("{f}: {}\n", c.join(" ")));
                            }
                        }
                        // Run the file's commands next in this queue (so async ones like
                        // run-shell work); errors are reported without stopping, like tmux.
                        // The markers around them set #{current_file} while they run.
                        injected.push(vec![FILE_PUSH.to_string(), f.clone()]);
                        injected.extend(cmds);
                        injected.push(vec![FILE_POP.to_string()]);
                    }
                }
                Ok(Flow::Inject(injected, true))
            }
            "if-shell" => {
                let cond = o.rest.first().ok_or("if-shell needs a condition")?.clone();
                let then = o.rest.get(1).ok_or("if-shell needs a command")?.clone();
                let otherwise = o.rest.get(2).cloned();
                let tt = self.resolve_pane(ctx, t).unwrap_or_else(|_| self.current(ctx));
                let cond = self.expand(tt, ctx.client, &cond);
                if o.has('F') {
                    let branch = if truthy(&cond) { Some(then) } else { otherwise };
                    return match branch {
                        Some(b) => Ok(Flow::Inject(cmdparse::parse(&b)?, false)),
                        None => Ok(Flow::Next),
                    };
                }
                // async: park the remaining queue
                Err(format!("\0job-if\0{cond}\0{then}\0{}\0{}", otherwise.unwrap_or_default(), if o.has('b') { "b" } else { "" }))
            }
            "run-shell" => {
                let tt = self.resolve_pane(ctx, t).unwrap_or_else(|_| self.current(ctx));
                if o.has('C') {
                    let c = o.rest.first().ok_or("run-shell -C needs a command")?;
                    let cmds = cmdparse::parse(&self.expand(tt, ctx.client, c))?;
                    return Ok(Flow::Inject(cmds, false));
                }
                if let Some(d) = o.get('d') {
                    // like tmux, wait on a timer: the queue resumes without -d afterwards
                    let secs = d.parse::<f64>().ok().filter(|s| s.is_finite() && *s >= 0.0).ok_or_else(|| format!("invalid delay time: {d}"))?;
                    return Err(format!(" delay {}", (secs * 1000.0).min(u32::MAX as f64) as u64));
                }
                let command = o.rest.first().map(|c| self.expand(tt, ctx.client, c)).unwrap_or_default();
                if command.is_empty() {
                    return Ok(Flow::Next);
                }
                let cwd = o.get('c').map(String::from).or_else(|| tt.pane.and_then(|p| self.panes.get(&p)).map(|p| p.current_path.clone()));
                Err(format!("\0job-run\0{command}\0{}\0{}\0{}", cwd.unwrap_or_default(), if o.has('b') { "b" } else { "" }, tt.pane.map(|p| p.to_string()).unwrap_or_default()))
            }
            "wait-for" => {
                let ch = o.rest.first().ok_or("wait-for needs a channel")?.clone();
                if o.has('S') {
                    let locked = self.waits.get(&ch).is_some_and(|e| e.0);
                    let waiters = self.waits.get_mut(&ch).map(|e| std::mem::take(&mut e.1)).unwrap_or_default();
                    if !locked {
                        self.waits.remove(&ch);
                    }
                    if waiters.is_empty() {
                        // like tmux: a signal nobody is waiting for is kept for the next wait-for
                        if !locked {
                            self.waits_woken.insert(ch);
                        }
                        return Ok(Flow::Next);
                    }
                    for q in waiters {
                        self.run_queue(q);
                    }
                    return Ok(Flow::Next);
                }
                if o.has('L') {
                    let e = self.waits.entry(ch.clone()).or_insert((false, Vec::new()));
                    if !e.0 {
                        e.0 = true;
                        return Ok(Flow::Next);
                    }
                    return Err(format!("\0wait\0{ch}"));
                }
                if o.has('U') {
                    let Some(e) = self.waits.get_mut(&ch) else { return err(format!("channel {ch} not locked")) };
                    if !e.0 {
                        return err(format!("channel {ch} not locked"));
                    }
                    if e.1.is_empty() {
                        e.0 = false;
                    } else {
                        let q = e.1.remove(0);
                        self.run_queue(q);
                    }
                    return Ok(Flow::Next);
                }
                if self.waits_woken.remove(&ch) {
                    return Ok(Flow::Next); // already signalled
                }
                Err(format!("\0wait\0{ch}"))
            }
            "list-commands" => {
                for (n, a, b, v) in COMMANDS {
                    if o.rest.first().is_some_and(|x| x != n && x != a) {
                        continue;
                    }
                    let alias = if a.is_empty() { String::new() } else { format!(" ({a})") };
                    let flags = if b.is_empty() { String::new() } else { format!(" [-{b}]") };
                    let vals: String = v.chars().map(|c| format!(" [-{c} arg]")).collect();
                    ctx.out.push_str(&format!("{n}{alias}{flags}{vals}\n"));
                }
                Ok(Flow::Next)
            }
            "clear-prompt" => Ok(Flow::Next),
            "server-access" => {
                if o.has('l') {
                    for (id, write) in &self.acl {
                        ctx.out.push_str(&format!("{} ({})\n", ipc::user_name_for_id(id), if *write { "W" } else { "R" }));
                    }
                    return Ok(Flow::Next);
                }
                if ctx.client.and_then(|c| self.clients.get(&c)).is_some_and(|c| c.user.as_ref().is_some_and(|u| *u != self.owner && !ipc::is_superuser(u))) {
                    return err("only the server owner can change access");
                }
                let name = o.rest.first().ok_or("server-access needs a user")?;
                let id = ipc::user_id_for_name(name).ok_or_else(|| format!("unknown user: {name}"))?;
                if id == self.owner {
                    return err("the server owner always has access");
                }
                let clients_of = |s: &Server| -> Vec<ClientId> { s.clients.values().filter(|c| c.user.as_deref() == Some(id.as_str())).map(|c| c.id).collect() };
                if o.has('d') {
                    self.acl.retain(|a| a.0 != id);
                    for cid in clients_of(self) {
                        if self.clients.get(&cid).is_some_and(|c| c.attached) {
                            self.detach_client(cid, "[access not allowed]");
                        }
                        if let Some(c) = self.clients.get_mut(&cid) {
                            c.denied = true;
                        }
                    }
                    if self.acl.is_empty() {
                        ipc::set_shared(false);
                    }
                    return Ok(Flow::Next);
                }
                let write = !o.has('r');
                if o.has('a') {
                    if self.acl.iter().any(|a| a.0 == id) {
                        return err(format!("user {name} is already added"));
                    }
                    self.acl.push((id.clone(), write));
                    ipc::set_shared(true);
                    if cfg!(windows) && self.pipe_acl.as_ref().is_some_and(|p| !p.contains(&id)) {
                        ctx.out.push_str("note: the named pipe's ACL is set when the server starts; put this in the config file or restart the server for it to take effect\n");
                    }
                } else if o.has('r') || o.has('w') {
                    let e = self.acl.iter_mut().find(|a| a.0 == id).ok_or_else(|| format!("user {name} not found"))?;
                    e.1 = o.has('w');
                } else {
                    return err("server-access needs -a, -d, -l, -r or -w");
                }
                let write = self.acl.iter().find(|a| a.0 == id).map_or(write, |a| a.1);
                for cid in clients_of(self) {
                    if let Some(c) = self.clients.get_mut(&cid) {
                        c.acl_readonly = !write;
                        c.readonly = !write;
                    }
                }
                Ok(Flow::Next)
            }
            _ => err(format!("{cmd}: not implemented")),
        }
    }

    /// new-session -t: a session sharing the target's windows (a session group).
    fn new_grouped_session(&mut self, ctx: &mut Ctx, name: Option<String>, target: &str, detached: bool, print: bool, fmt: Option<&str>) -> Result<Flow, String> {
        let tsid = self.find_session(ctx, target)?;
        let name = match name {
            Some(n) if n.is_empty() || n.contains([':', '.']) => return err(format!("invalid session: {n}")),
            Some(n) if self.sessions.values().any(|s| s.name == n) => return err(format!("duplicate session: {n}")),
            Some(n) => n,
            None => (0..).map(|i: u32| i.to_string()).find(|n| !self.sessions.values().any(|s| &s.name == n)).unwrap(),
        };
        let gid = match self.sessions[&tsid].group {
            Some(g) => g,
            None => {
                let g = self.id();
                self.sessions.get_mut(&tsid).unwrap().group = Some(g);
                self.groups.insert(g, self.sessions[&tsid].links.clone());
                g
            }
        };
        let sid = self.id();
        let now = now_secs();
        let t = &self.sessions[&tsid];
        let session = Session {
            id: sid,
            name,
            links: t.links.clone(),
            active: t.active,
            last: Vec::new(),
            cols: t.cols,
            rows: t.rows,
            cwd: t.cwd.clone(),
            options: Options::default(),
            env: t.env.clone(),
            hooks: BTreeMap::new(),
            created: now,
            activity: now,
            last_attached: 0,
            alerts: BTreeMap::new(),
            group: Some(gid),
        };
        self.sessions.insert(sid, session);
        self.run_hook(Some(sid), "session-created", Ctx { client: ctx.client, ..Ctx::default() });
        if print {
            let s = self.expand(Target { sid: Some(sid), ..Target::default() }, None, fmt.unwrap_or("#{session_name}:"));
            ctx.out.push_str(&s);
            ctx.out.push('\n');
        }
        let cid = ctx.reply.or(ctx.client);
        let attachable = cid.and_then(|c| self.clients.get(&c)).is_some_and(|c| c.attached || c.ident.as_ref().is_some_and(|i| i.tty));
        if !detached && attachable {
            return self.attach_or_switch(ctx, sid, false, false, false);
        }
        Ok(Flow::Next)
    }

    fn attach_or_switch(&mut self, ctx: &mut Ctx, sid: SessId, detach_others: bool, readonly: bool, _no_env: bool) -> Result<Flow, String> {
        let cid = ctx.reply.or(ctx.client).ok_or("no client")?;
        let c = self.clients.get(&cid).ok_or("no client")?;
        if !c.attached {
            let ident = c.ident.as_ref();
            if ident.is_some_and(|i| i.ctx_pane.is_some()) {
                return err("sessions should be nested with care, unset PWSH_TMUX to force");
            }
            if !ident.is_some_and(|i| i.tty) && c.control == 0 {
                return err("open terminal failed: not a terminal");
            }
        }
        if detach_others {
            let name = self.sessions[&sid].name.clone();
            let others: Vec<ClientId> = self.clients.values().filter(|c| c.attached && c.session == Some(sid) && c.id != cid).map(|c| c.id).collect();
            for o in others {
                self.detach_client(o, &format!("[detached (from session {name})]"));
            }
        }
        if let Some(c) = self.clients.get_mut(&cid) {
            c.readonly |= readonly || c.acl_readonly;
        }
        self.switch_client(cid, sid);
        Ok(Flow::Next)
    }

    fn resolve_path(&self, ctx: &Ctx, p: &str) -> String {
        let p = if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
            format!("{}/{rest}", procinfo::home_dir().unwrap_or_default())
        } else if p == "~" {
            procinfo::home_dir().unwrap_or_default()
        } else {
            p.to_string()
        };
        if std::path::Path::new(&p).is_absolute() {
            return p;
        }
        let base = ctx
            .reply
            .and_then(|c| self.clients.get(&c))
            .and_then(|c| c.ident.as_ref())
            .map(|i| i.cwd.clone())
            .or_else(|| self.current(ctx).sid.map(|s| self.sessions[&s].cwd.clone()))
            .or_else(procinfo::home_dir)
            .unwrap_or_default();
        std::path::Path::new(&base).join(p).to_string_lossy().into_owned()
    }

    /// Scope for an option name like options_scope_from_name: table options use
    /// their own scope (-p selects the pane level of window+pane options); user
    /// options and listings without a name follow -s/-p/-w.
    fn option_scope(window_cmd: bool, o: &Opts, name: Option<&str>) -> Scope {
        match name.filter(|n| !n.starts_with('@')).and_then(options::def) {
            Some(d) if d.scope == Scope::Pane && o.has('p') => Scope::Pane,
            Some(d) => options::natural_scope(d.name).unwrap_or(d.scope),
            None if o.has('s') => Scope::Server,
            None if o.has('p') => Scope::Pane,
            None if o.has('w') || window_cmd => Scope::Window,
            None => Scope::Session,
        }
    }

    fn option_target(&self, scope: Scope, global: bool, ctx: &Ctx, t: Option<&str>) -> Result<Target, String> {
        Ok(match scope {
            Scope::Session if !global => Target { sid: Some(self.resolve_session(ctx, t)?), ..Target::default() },
            Scope::Window if !global => self.resolve_window(ctx, t)?,
            Scope::Pane => self.resolve_pane(ctx, t)?,
            _ => Target::default(),
        })
    }

    /// Resolve the option argument (format-expanded, like tmux); None means -q swallowed an error.
    fn option_name(&self, o: &Opts, ctx: &Ctx, arg: &str) -> Result<Option<String>, String> {
        let arg = if arg.contains('#') { self.expand(self.current(ctx), ctx.client, arg) } else { arg.to_string() };
        match options::canonical_name(&arg) {
            Ok(n) => Ok(Some(n)),
            Err(e) if o.has('q') && !e.starts_with("not an array") => Ok(None),
            Err(e) => err(e),
        }
    }

    fn cmd_set_option(&mut self, window_cmd: bool, o: &Opts, ctx: &Ctx) -> Result<(), String> {
        let Some(name) = self.option_name(o, ctx, o.rest.first().ok_or("missing argument")?)? else { return Ok(()) };
        let value = o.rest.get(1).map(|v| if o.has('F') { self.expand(self.current(ctx), ctx.client, v) } else { v.clone() });
        let scope = Self::option_scope(window_cmd, o, Some(&name));
        // -g has no meaning for server options and is ignored with -p
        let global = scope == Scope::Server || (o.has('g') && scope != Scope::Pane);
        let tt = self.option_target(scope, global, ctx, o.get('t'))?;
        let def = options::def(&name);
        let (present, current) = match self.store(scope, global, tt) {
            Some(s) => (s.has(&name), s.get(&name).map(String::from)),
            None => (false, None),
        };
        if o.has('o') && !o.has('u') && present {
            return if o.has('q') { Ok(()) } else { err(format!("already set: {name}")) };
        }
        // toggles start from the effective (possibly inherited) value
        let effective = current.clone().or_else(|| match scope {
            Scope::Server => self.opts_server.get(&name).map(String::from),
            Scope::Session => Some(self.opt_session(tt.sid, &name)),
            Scope::Window => Some(self.opt_window(tt.wid, &name)),
            Scope::Pane => tt.pane.map(|p| self.opt_pane(p, &name)),
        });
        if o.has('U') && scope == Scope::Window && !global {
            let pids = tt.wid.and_then(|w| self.windows.get(&w)).map(|w| w.panes()).unwrap_or_default();
            for p in pids {
                if let Some(p) = self.panes.get_mut(&p) { p.options.unset(&name); }
            }
        }
        let store = self.store_mut(scope, global, tt).ok_or("no such target")?;
        if o.has('u') || o.has('U') {
            // global levels go back to the table default, others inherit again
            if global && def.is_some() { store.reset(&name); } else { store.unset(&name); }
        } else if def.is_some_and(|d| d.kind == options::Kind::Array) && !name.contains('[') {
            store.assign_array(&name, value.as_deref().ok_or("empty value")?, o.has('a'))?;
        } else {
            let mut v = options::validate(&name, value.as_deref(), effective.as_deref())?;
            // -a appends to this level's own string value (options_set_string)
            let string = def.is_none_or(|d| matches!(d.kind, options::Kind::Str | options::Kind::Style | options::Kind::ColourFormat)
                || (d.kind == options::Kind::Array && d.name != "pane-colours"));
            if o.has('a') && string && let Some(old) = &current {
                let sep = if name.contains('[') { "" } else { def.map_or("", |d| d.append_separator()) };
                v = options::validate(&name, Some(&format!("{old}{sep}{v}")), None)?;
            }
            if def.is_some_and(|d| d.kind == options::Kind::Command) {
                v = canonical_command_list(&v)?;
            }
            store.set(&name, v);
        }
        // side effects
        match name.as_str() {
            "status" | "status-position" | "pane-border-status" | "window-size" => {
                let sids: Vec<SessId> = self.sessions.keys().copied().collect();
                for s in sids {
                    self.update_session_size(s);
                }
                let wids: Vec<WinId> = self.windows.keys().copied().collect();
                for w in wids {
                    self.apply_layout(w);
                }
            }
            "mode-keys" => {}
            _ => {}
        }
        self.run_hook(None, "option-changed", Ctx::default());
        self.mark_all_dirty();
        for c in self.clients.values_mut() {
            c.prev = None;
        }
        Ok(())
    }

    fn store(&self, scope: Scope, global: bool, t: Target) -> Option<&Options> {
        match (scope, global) {
            (Scope::Server, _) => Some(&self.opts_server),
            (Scope::Session, true) => Some(&self.opts_session),
            (Scope::Session, false) => t.sid.and_then(|s| self.sessions.get(&s)).map(|s| &s.options),
            (Scope::Window, true) | (Scope::Pane, true) => Some(&self.opts_window),
            (Scope::Window, false) => t.wid.and_then(|w| self.windows.get(&w)).map(|w| &w.options),
            (Scope::Pane, false) => t.pane.and_then(|p| self.panes.get(&p)).map(|p| &p.options),
        }
    }

    fn store_mut(&mut self, scope: Scope, global: bool, t: Target) -> Option<&mut Options> {
        match (scope, global) {
            (Scope::Server, _) => Some(&mut self.opts_server),
            (Scope::Session, true) => Some(&mut self.opts_session),
            (Scope::Session, false) => t.sid.and_then(|s| self.sessions.get_mut(&s)).map(|s| &mut s.options),
            (Scope::Window, true) | (Scope::Pane, true) => Some(&mut self.opts_window),
            (Scope::Window, false) => t.wid.and_then(|w| self.windows.get_mut(&w)).map(|w| &mut w.options),
            (Scope::Pane, false) => t.pane.and_then(|p| self.panes.get_mut(&p)).map(|p| &mut p.options),
        }
    }

    fn cmd_show_options(&mut self, window_cmd: bool, o: &Opts, ctx: &mut Ctx) -> Result<(), String> {
        let name = match o.rest.first() {
            Some(arg) => match self.option_name(o, ctx, arg)? { Some(n) => Some(n), None => return Ok(()) },
            None => None,
        };
        let scope = Self::option_scope(window_cmd, o, name.as_deref());
        let global = scope == Scope::Server || (o.has('g') && scope != Scope::Pane);
        let tt = self.option_target(scope, global, ctx, o.get('t'))?;
        let Some(store) = self.store(scope, global, tt) else { return err("no such target") };
        // -A walks the parent levels: pane -> window -> global window, session -> global session
        let parents: Vec<&Options> = match scope {
            _ if global => vec![],
            Scope::Session => vec![&self.opts_session],
            Scope::Window => vec![&self.opts_window],
            Scope::Pane => tt.wid.and_then(|w| self.windows.get(&w)).map(|w| &w.options).into_iter().chain([&self.opts_window]).collect(),
            Scope::Server => vec![],
        };
        // the level holding an option: this one, or with -A the nearest parent
        let find = |base: &str| -> Option<(&Options, bool)> {
            if store.has(base) { return Some((store, false)); }
            if !o.has('A') { return None; }
            parents.iter().find(|p| p.has(base)).map(|p| (*p, true))
        };
        let print = |out: &mut String, key: &str, value: Option<&str>, parent: bool| {
            let star = if parent { "*" } else { "" };
            match value {
                Some(v) if o.has('v') => out.push_str(&format!("{v}\n")),
                Some(v) => out.push_str(&format!("{key}{star} {}\n", options::print_value(key, v))),
                None if o.has('v') => {}
                None => out.push_str(&format!("{key}{star}\n")),
            }
        };
        // print one option like cmd_show_options_print: arrays show each element
        let show = |out: &mut String, lvl: &Options, name: &str, parent: bool, named: bool| {
            let base = name.split('[').next().unwrap_or(name);
            if name.contains('[') {
                print(out, name, Some(lvl.get(name).unwrap_or("")), parent);
            } else if options::def(base).is_some_and(|d| d.kind == options::Kind::Array) {
                let items = lvl.array_items(base);
                if items.is_empty() {
                    print(out, base, None, parent && !named);
                }
                for (k, v) in items {
                    print(out, &k, Some(&v), parent);
                }
            } else if let Some(v) = lvl.get(name) {
                print(out, name, Some(v), parent);
            }
        };
        let mut out = String::new();
        match &name {
            Some(n) => {
                let base = n.split('[').next().unwrap_or(n);
                match find(base) {
                    Some((lvl, parent)) => show(&mut out, lvl, n, parent, true),
                    None if base.starts_with('@') && !o.has('q') => return err(format!("invalid option: {n}")),
                    None => {}
                }
            }
            None => {
                // user options of this level first, then the table in tmux order
                for (k, v) in store.0.iter().filter(|(k, _)| k.starts_with('@')) {
                    print(&mut out, k, Some(v), false);
                }
                let wanted = |d: &&options::OptDef| d.scope == scope || (d.scope == Scope::Pane && scope == Scope::Window);
                for d in options::DEFS.iter().filter(wanted) {
                    if let Some((lvl, parent)) = find(d.name) {
                        show(&mut out, lvl, d.name, parent, false);
                    }
                }
            }
        }
        ctx.out.push_str(&out);
        Ok(())
    }

    pub fn open_tree(&mut self, pane: PaneId, kind: TreeKind, template: Option<String>, filter: Option<Vec<WinId>>, zoom: bool) {
        let tree = Tree::new(kind, template, filter, self, pane);
        if let Some(p) = self.panes.get_mut(&pane) {
            p.mode = Some(PaneMode::Tree(Box::new(tree)));
        }
        if let Some(wid) = self.window_of(pane) {
            if zoom {
                let w = self.windows.get_mut(&wid).unwrap();
                if w.panes().len() > 1 && !w.zoomed {
                    w.active = pane;
                    w.zoomed = true;
                    self.apply_layout(wid);
                }
            }
            self.mark_window_dirty(wid);
        }
    }

    pub fn open_prompt(&mut self, cid: ClientId, kind: PromptKind, prompts: Vec<(String, String)>, template: String, pane: Option<PaneId>, numeric: bool, single: bool) {
        let (label, init) = prompts.first().cloned().unwrap_or_default();
        let input: Vec<char> = init.chars().collect();
        let cursor = input.len();
        if let Some(c) = self.clients.get_mut(&cid) {
            c.prompt = Some(Prompt { kind, prompts, answers: Vec::new(), label, input, cursor, template, history_pos: None, pane, numeric, single_key: single, copy_incremental: None });
            c.dirty = true;
        }
    }
}

/// Commands a read-only (server-access -r) client may run.
const READ_ONLY_OK: &[&str] = &[
    "attach-session", "detach-client", "has-session", "list-sessions", "list-windows", "list-panes", "list-clients", "list-buffers", "list-keys",
    "list-commands", "display-message", "show-options", "show-window-options", "show-environment", "show-hooks", "show-messages", "show-buffer",
    "capture-pane", "refresh-client", "switch-client", "server-access",
];

pub fn truthy(s: &str) -> bool {
    !s.is_empty() && s != "0"
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn buffer_vars(b: &Buffer) -> HashMap<String, String> {
    let sample: String = String::from_utf8_lossy(&b.data).chars().take(50).map(|c| if c.is_control() { ' ' } else { c }).collect();
    HashMap::from([
        ("buffer_name".to_string(), b.name.clone()),
        ("buffer_size".to_string(), b.data.len().to_string()),
        ("buffer_sample".to_string(), sample),
        ("buffer_created".to_string(), b.created.to_string()),
    ])
}

fn glob_files(path: &str) -> Vec<String> {
    if !path.contains(['*', '?']) {
        return if std::path::Path::new(path).exists() { vec![path.to_string()] } else { vec![] };
    }
    let p = std::path::Path::new(path);
    let (dir, pat) = (p.parent().map(|d| d.to_path_buf()).unwrap_or_default(), p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
    let mut v: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| rd.flatten().filter(|e| fnmatch(&pat, &e.file_name().to_string_lossy())).map(|e| e.path().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

pub const VAR_NAMES: &[&str] = &[
    "host", "host_short", "pid", "version", "current_file", "socket_path", "start_time", "session_name", "session_id", "session_windows", "session_attached", "session_created", "session_activity",
    "session_path", "session_alerts", "window_index", "window_name", "window_id", "window_active", "window_flags", "window_panes", "window_zoomed_flag", "window_layout",
    "window_width", "window_height", "pane_index", "pane_id", "pane_title", "pane_current_path", "pane_current_command", "pane_pid", "pane_width", "pane_height", "pane_active",
    "pane_dead", "pane_in_mode", "pane_mode", "pane_left", "pane_top", "pane_right", "pane_bottom", "cursor_x", "cursor_y", "history_size", "history_limit", "alternate_on",
    "client_name", "client_width", "client_height", "client_session", "client_key_table", "client_prefix", "client_termname",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(base64(b"ab"), "YWI=");
        assert!(fnmatch("*.conf", "a.conf") && !fnmatch("a?", "abc"));
        assert_eq!(canonical("neww"), Some("new-window"));
        assert_eq!(canonical("list-k"), Some("list-keys"));
        let mut a: Vec<String> = ["-e", "A=1", "-eB=2", "-d", "cmd"].iter().map(|s| s.to_string()).collect();
        let env = take_env(&mut a);
        assert_eq!(env.len(), 2);
        assert_eq!(a, vec!["-d", "cmd"]);
    }
}
