//! Command table, target resolution and the implementation of every command.

use super::modes::{Menu, MenuItem, Popup, Tree, TreeKind};
use super::vars::Target;
use super::*;
use crate::cmdparse;
use crate::layout::{self, Dir};
use crate::opts::{getopt, Opts};
use std::cmp::Ordering;

mod json;
mod sort;
mod vis;

/// (name, alias, flags without value, flags with value)
const COMMANDS: &[(&str, &str, &str, &str)] = &[
    ("attach-session", "attach", "dErx", "cfst"),
    ("bind-key", "bind", "nr", "TN"),
    ("break-pane", "breakp", "abdP", "FnstL"),
    ("capture-pane", "capturep", "aCeFHIJLMNpPqRT", "bESt"),
    ("choose-buffer", "", "NrZy", "FfKOt"),
    ("choose-client", "", "iNrZy", "FfKOt"),
    ("choose-tree", "", "GNrswZy", "FfKOt"),
    ("clear-history", "clearhist", "H", "t"),
    ("clear-prompt-history", "clearphist", "", "T"),
    ("clock-mode", "", "", "t"),
    ("command-prompt", "", "1bCeFkilN", "IpTt"),
    ("confirm-before", "confirm", "by", "cpt"),
    ("copy-mode", "", "deHMqSu", "st"),
    ("customize-mode", "", "NZy", "Fft"),
    ("delete-buffer", "deleteb", "", "b"),
    ("detach-client", "detach", "aP", "Esct"),
    ("display-menu", "menu", "MO", "bcCHstSTxy"),
    ("display-message", "display", "aCIjlNpv", "cdFt"),
    ("display-panes", "displayp", "bN", "dt"),
    ("display-popup", "popup", "BCEkN", "bcdeHhsStTwxy"),
    ("find-window", "findw", "CiNrTZ", "t"),
    ("has-session", "has", "", "t"),
    ("if-shell", "if", "bF", "t"),
    ("join-pane", "joinp", "bdfhv", "lpst"),
    ("kill-pane", "killp", "a", "ft"),
    ("kill-server", "", "", ""),
    ("kill-session", "", "aCg", "ft"),
    ("kill-window", "killw", "a", "ft"),
    ("last-pane", "lastp", "deZ", "t"),
    ("last-window", "last", "", "t"),
    ("link-window", "linkw", "abdk", "st"),
    ("list-buffers", "lsb", "r", "FfO"),
    ("list-clients", "lsc", "r", "FfOt"),
    ("list-commands", "lscm", "", "F"),
    ("list-keys", "lsk", "1aNr", "FOPT"),
    ("list-panes", "lsp", "ars", "FfOt"),
    ("list-sessions", "ls", "r", "FfO"),
    ("list-windows", "lsw", "ar", "FfOt"),
    ("load-buffer", "loadb", "w", "bt"),
    ("lock-client", "lockc", "", "t"),
    ("lock-server", "lock", "", ""),
    ("lock-session", "locks", "", "t"),
    ("move-pane", "movep", "bdfhv", "lst"),
    ("move-window", "movew", "abdkr", "st"),
    ("new-session", "new", "AdDEPX", "cefFnstxy"),
    ("new-window", "neww", "abdEkPS", "cefFnt"),
    ("next-layout", "nextl", "", "t"),
    ("next-window", "next", "a", "t"),
    ("paste-buffer", "pasteb", "dprS", "bst"),
    ("pipe-pane", "pipep", "IOo", "t"),
    ("previous-layout", "prevl", "", "t"),
    ("previous-window", "prev", "a", "t"),
    ("refresh-client", "refresh", "cDlLRSU", "ABCfFrt"),
    ("rename-session", "rename", "", "t"),
    ("rename-window", "renamew", "", "t"),
    ("resize-pane", "resizep", "DLMRTUZ", "txy"),
    ("resize-window", "resizew", "aADLRU", "txy"),
    ("respawn-pane", "respawnp", "Ek", "cet"),
    ("respawn-window", "respawnw", "Ek", "cet"),
    ("rotate-window", "rotatew", "DUZ", "t"),
    ("run-shell", "run", "bCE", "cdst"),
    ("save-buffer", "saveb", "a", "b"),
    ("select-layout", "selectl", "Enop", "t"),
    ("select-pane", "selectp", "DdegLlMmRUZ", "PTt"),
    ("select-window", "selectw", "lnpT", "t"),
    ("send-keys", "send", "FHKlMRX", "cNt"),
    ("send-prefix", "", "2", "t"),
    ("server-access", "", "adglrw", ""),
    ("set-buffer", "setb", "aw", "bnt"),
    ("set-environment", "setenv", "Fhgru", "t"),
    ("set-hook", "", "agpRuw", "t"),
    ("set-option", "set", "aFgopqsuUw", "t"),
    ("set-window-option", "setw", "aFgoqu", "t"),
    ("show-buffer", "showb", "", "b"),
    ("show-environment", "showenv", "hgs", "t"),
    ("show-hooks", "", "gpw", "Ft"),
    ("show-messages", "showmsgs", "JT", "t"),
    ("show-options", "show", "AgHpqsvw", "Ft"),
    ("show-prompt-history", "showphist", "", "T"),
    ("show-window-options", "showw", "gv", "Ft"),
    ("source-file", "source", "Fnqv", "t"),
    ("split-window", "splitw", "bdEfhIvPZ", "cefFlt"),
    ("start-server", "start", "", ""),
    ("suspend-client", "suspendc", "", "t"),
    ("swap-pane", "swapp", "dDUZ", "st"),
    ("swap-window", "swapw", "d", "st"),
    ("switch-client", "switchc", "EFlnprZ", "cOtT"),
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

/// The command given to spawn_pane for a pane without a process (-E, or an empty command).
pub const EMPTY_PANE: &str = "\u{1}empty-pane";

/// The command for a new pane: -E (or a single empty argument, or `input` for split-window
/// -I) makes an empty pane, which can't also be given a command.
fn spawn_command(o: &Opts, input: bool) -> Result<Vec<String>, String> {
    let empty_arg = matches!(o.rest.as_slice(), [one] if one.is_empty());
    let empty = o.has('E') || input || empty_arg;
    if empty && !o.rest.is_empty() && !empty_arg {
        return err("command cannot be given for empty pane");
    }
    Ok(if empty { vec![EMPTY_PANE.to_string()] } else { o.rest.clone() })
}

/// Store a command option (default-client-command, hooks) the way tmux parses
/// it with cmd_parse_from_string and prints it with cmd_list_print: command-alias
/// entries expanded, unknown commands and flags rejected, flags sorted as
/// args_print does, { } blocks kept as command lists, commands joined by " ; ".
fn command_list_string(v: &str, aliases: &[String]) -> Result<String, String> {
    // errors of a string parse have no file:line prefix
    let cmds = cmdparse::parse_blocks(v).map_err(|e| match e.split_once(": ") {
        Some((l, rest)) if l.starts_with("line ") => rest.to_string(),
        _ => e,
    })?;
    let mut out = Vec::new();
    for c in cmds {
        out.extend(print_parsed_command(c, aliases, true)?);
    }
    Ok(out.join(" ; "))
}

/// A stored command string (a key binding or hook) printed as cmd_list_print does:
/// full command names, flags sorted, arguments escaped, { } blocks kept; commands are
/// separated by ` \; ` when `escaped` (list-keys), else ` ; `. Strings that do not
/// parse are shown as they are.
pub fn print_command_string(v: &str, aliases: &[String], escaped: bool) -> String {
    let Ok(cmds) = cmdparse::parse_blocks(v) else { return v.to_string() };
    let mut out = Vec::new();
    for c in cmds {
        match print_parsed_command(c, aliases, true) {
            Ok(c) => out.extend(c),
            Err(_) => return v.to_string(),
        }
    }
    out.join(if escaped { " \\; " } else { " ; " })
}

/// Whether positional argument `idx` of a command may be a { } block
/// (ARGS_PARSE_COMMANDS_OR_STRING in the command's args_parse callback).
fn takes_commands(cmd: &str, idx: usize, flags: &[char]) -> bool {
    match cmd {
        "if-shell" => idx == 1 || idx == 2,
        "set-option" | "set-window-option" | "set-hook" => idx == 1,
        "run-shell" => flags.contains(&'C'),
        "bind-key" | "confirm-before" | "command-prompt" | "display-menu" | "choose-tree" | "choose-client"
        | "choose-buffer" | "customize-mode" | "switch-mode" | "display-panes" => true,
        _ => false,
    }
}

fn print_parsed_command(words: Vec<(String, bool)>, aliases: &[String], expand: bool) -> Result<Vec<String>, String> {
    let Some((name, brace)) = words.first().cloned() else { return Ok(vec![]) };
    if brace { return err("syntax error"); }
    // cmd_parse_expand_alias: the alias's last command takes the arguments
    if expand && let Some(alias) = aliases.iter().find_map(|a| a.split_once('=').filter(|(n, _)| *n == name).map(|(_, v)| v)) {
        let mut cmds = cmdparse::parse_blocks(alias)?;
        let Some(last) = cmds.last_mut() else { return Ok(vec![]) };
        last.extend(words.into_iter().skip(1));
        let mut out = Vec::new();
        for c in cmds {
            out.extend(print_parsed_command(c, aliases, false)?);
        }
        return Ok(out);
    }
    // blocks are built before the command itself
    let mut args = Vec::new();
    for (a, brace) in words.into_iter().skip(1) {
        let a = if brace {
            let mut inner = Vec::new();
            for c in cmdparse::parse_blocks(&a)? {
                inner.extend(print_parsed_command(c, aliases, expand)?);
            }
            format!("{{ {} }}", inner.join(" ; "))
        } else { a };
        args.push((a, brace));
    }
    let full = canonical(&name).ok_or_else(|| {
        let m: Vec<&str> = COMMANDS.iter().map(|c| c.0).filter(|c| c.starts_with(name.as_str())).collect();
        if m.len() > 1 { format!("ambiguous command: {name}, could be: {}", m.join(", ")) } else { format!("unknown command: {name}") }
    })?;
    let spec = COMMANDS.iter().find(|c| c.0 == full).unwrap();
    let fail = |e: String| format!("command {full}: {e}");
    let (mut bare, mut valued) = (Vec::new(), Vec::new());
    let mut i = 0;
    while let Some((a, brace)) = args.get(i) {
        if *brace || !a.starts_with('-') || a.len() < 2 { break; }
        i += 1;
        if a == "--" { break; }
        let chars: Vec<char> = a[1..].chars().collect();
        for (j, &c) in chars.iter().enumerate() {
            if spec.2.contains(c) {
                bare.push(c);
            } else if spec.3.contains(c) {
                let inline: String = chars[j + 1..].iter().collect();
                let v = if !inline.is_empty() { inline } else {
                    let v = match args.get(i) {
                        Some((v, false)) => v.clone(),
                        Some((_, true)) => return Err(fail(format!("-{c} argument must be a string"))),
                        None => return Err(fail(format!("-{c} expects an argument"))),
                    };
                    i += 1;
                    v
                };
                valued.push((c, v));
                break;
            } else {
                return Err(fail(format!("unknown flag -{c}")));
            }
        }
    }
    // args_print: bare flags together, then flags with values, both by flag
    bare.sort_unstable();
    valued.sort_by_key(|(c, _)| *c);
    let mut out = vec![full.to_string()];
    if !bare.is_empty() { out.push(format!("-{}", bare.iter().collect::<String>())); }
    out.extend(valued.into_iter().map(|(c, v)| format!("-{c} {}", options::escape_arg(&v))));
    for (k, (a, brace)) in args[i..].iter().enumerate() {
        if *brace && !takes_commands(full, k, &bare) {
            return Err(fail(format!("argument {} must be \"string\"", k + 1)));
        }
        out.push(if *brace { a.clone() } else { options::escape_arg(a) });
    }
    Ok(vec![out.join(" ")])
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

/// environ_update: each update-environment pattern copies the matching variables of the
/// client's environment, or marks the pattern removed when none match.
pub fn environ_update(patterns: &[String], src: &[(String, String)]) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for p in patterns {
        let found: Vec<&(String, String)> = src.iter().filter(|(k, _)| fnmatch(p, k)).collect();
        if found.is_empty() {
            out.push((p.clone(), None));
        }
        out.extend(found.into_iter().map(|(k, v)| (k.clone(), Some(v.clone()))));
    }
    out
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

    /// A whole target of `@`, `{current}` or `{active}` (tmux 3.7): the current window and
    /// active pane of the client running the command, which must be attached.
    fn client_current(&self, ctx: &Ctx, t: &str) -> Option<Result<Target, String>> {
        if !matches!(t, "@" | "{current}" | "{active}") {
            return None;
        }
        let sid = ctx.client.and_then(|c| self.clients.get(&c)).filter(|c| c.attached).and_then(|c| c.session).filter(|s| self.sessions.contains_key(s));
        Some(match sid {
            Some(sid) => {
                let s = &self.sessions[&sid];
                let wid = s.active_window();
                Ok(Target { sid: Some(sid), idx: Some(s.active), wid: Some(wid), pane: Some(self.windows[&wid].active) })
            }
            None => err("no current client"),
        })
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
                if let Some(c) = self.client_current(ctx, t) {
                    return c.map(|c| c.sid.unwrap());
                }
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
        if let Some(c) = self.client_current(ctx, t) {
            return c;
        }
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
        if let Some(c) = self.client_current(ctx, t) {
            return c;
        }
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

    // ---- list helpers (sort.c orders) -------------------------------------------------------

    /// Sessions in tmux's natural order (by name) or the -O order.
    fn sorted_sessions(&self, crit: sort::Criteria) -> Vec<SessId> {
        let mut v: Vec<&Session> = self.sessions.values().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        crit.sort(&mut v, |a, b, order| {
            match order {
                sort::Order::Index => a.id.cmp(&b.id),
                sort::Order::Creation => (a.created, a.id).cmp(&(b.created, b.id)),
                sort::Order::Activity => b.activity_seq.cmp(&a.activity_seq),
                sort::Order::Name => a.name.cmp(&b.name),
                _ => Ordering::Equal,
            }
            .then_with(|| a.name.cmp(&b.name))
        });
        v.into_iter().map(|s| s.id).collect()
    }

    /// Winlinks (session, index, window) sorted by the -O order.
    fn sort_winlinks(&self, v: &mut [(SessId, u32, WinId)], crit: sort::Criteria) {
        crit.sort(v, |a, b, order| {
            let (wa, wb) = (&self.windows[&a.2], &self.windows[&b.2]);
            match order {
                sort::Order::Index => a.1.cmp(&b.1),
                sort::Order::Creation => (wa.created, wa.id).cmp(&(wb.created, wb.id)),
                sort::Order::Activity => wb.activity.cmp(&wa.activity),
                sort::Order::Name => wa.name.cmp(&wb.name),
                sort::Order::Size => (wa.sx as u32 * wa.sy as u32).cmp(&(wb.sx as u32 * wb.sy as u32)),
                _ => Ordering::Equal,
            }
            .then_with(|| wa.name.cmp(&wb.name))
        });
    }

    /// The panes of a window sorted by the -O order. Activity approximates tmux's
    /// active_point: panes never active, then the last active, then the active pane.
    fn sorted_panes(&self, wid: WinId, crit: sort::Criteria) -> Vec<PaneId> {
        let w = &self.windows[&wid];
        let order_of = w.panes();
        let mut v = order_of.clone();
        let title = |p: &PaneId| self.panes.get(p).map(|x| x.title.as_str()).unwrap_or("");
        let index = |p: &PaneId| order_of.iter().position(|x| x == p).unwrap_or(0);
        let point = |p: &PaneId| if *p == w.active { 2 } else if w.last_active == Some(*p) { 1 } else { 0 };
        let size = |p: &PaneId| self.panes.get(p).map(|x| x.size.0 as u32 * x.size.1 as u32).unwrap_or(0);
        crit.sort(&mut v, |a, b, order| {
            match order {
                sort::Order::Activity => point(a).cmp(&point(b)),
                sort::Order::Creation => a.cmp(b),
                sort::Order::Size => size(a).cmp(&size(b)),
                sort::Order::Index | sort::Order::Z => index(a).cmp(&index(b)),
                sort::Order::Name => title(a).cmp(title(b)),
                _ => Ordering::Equal,
            }
            .then_with(|| title(a).cmp(title(b)))
        });
        v
    }

    /// One line of a list command: `filter` (-f) decides, `fmt` (-F) is printed.
    fn list_line(&self, ctx: &mut Ctx, t: Target, client: Option<ClientId>, fmt: &str, filter: Option<&str>, extra: HashMap<String, String>) {
        if filter.is_some_and(|f| !truthy(&self.expand_extra(t, client, f, extra.clone()))) {
            return;
        }
        let line = self.expand_extra(t, client, fmt, extra);
        ctx.out.push_str(&line);
        ctx.out.push('\n');
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
            senv.extend(environ_update(&self.opt_array(None, "update-environment"), &ident.env));
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
                activity_seq: super::activity_seq(),
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
        // session_set_current leaves the session's activity alone: only attaching and
        // keys from its clients count
        s.alerts.remove(&idx);
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
            // tmux keeps the pane's last search and clears unseen changes with the mode
            if let Some(PaneMode::Copy(cm)) = &p.mode {
                if let Some(s) = cm.last_search() {
                    p.track.search = s.to_string();
                }
            }
            p.track.unseen_changes = false;
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
        // cmdq_add_message: commands from clients go in the message log (show-messages)
        if let Some(c) = ctx.client.or(ctx.reply).and_then(|c| self.clients.get(&c)) {
            let words = std::iter::once(full.to_string()).chain(args.iter().cloned()).map(|a| (a, false)).collect();
            let printed = print_parsed_command(words, &[], false).ok().and_then(|v| v.into_iter().next())
                .unwrap_or_else(|| std::iter::once(full).chain(args.iter().map(String::as_str)).collect::<Vec<_>>().join(" "));
            let msg = format!("{} command: {printed}", c.name());
            self.add_message(&msg);
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
                            return self.attach_or_switch(ctx, sid, o.has('D'), false, o.has('E'), None);
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
                if o.has('E') {
                    // -E: no update-environment, only the -e variables
                    if let Some(s) = self.sessions.get_mut(&sid) {
                        s.env = env.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect();
                    }
                }
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
                self.attach_or_switch(ctx, sid, o.has('d') || o.has('x'), o.has('r'), o.has('E'), o.get('f'))
            }
            "switch-client" => {
                let cid = self.resolve_client(ctx, o.get('c'))?;
                if let Some(table) = o.get('T') {
                    let c = self.clients.get_mut(&cid).unwrap();
                    c.key_table = table.to_string();
                    c.dirty = true;
                    return Ok(Flow::Next);
                }
                // -r toggles read-only and also reverses the -n/-p order; -F is accepted and
                // unused, as in tmux
                if o.has('r') {
                    let c = self.clients.get_mut(&cid).unwrap();
                    if c.is_readonly() { return err("client is read-only"); }
                    // read-only and ignore-size together
                    c.readonly = !c.readonly;
                    c.ctl.ignore_size = c.readonly;
                    if t.is_none() && !o.has('n') && !o.has('p') && !o.has('l') {
                        return Ok(Flow::Next);
                    }
                }
                let crit = sort::Criteria::from_args(o.get('O'), o.has('r'))?;
                let cur = self.client_session(cid);
                let sorted = self.sorted_sessions(crit);
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
                // -E: no update-environment
                self.switch_client_env(cid, sid, !o.has('E'));
                Ok(Flow::Next)
            }
            "detach-client" => {
                let msg_for = |s: &Server, sid: Option<SessId>| format!("[detached (from session {})]", sid.and_then(|s2| s.sessions.get(&s2)).map(|s| s.name.as_str()).unwrap_or(""));
                if let Some(s) = o.get('s') {
                    let sid = self.find_session(ctx, s)?;
                    let cids: Vec<ClientId> = self.clients.values().filter(|c| c.attached && c.session == Some(sid)).map(|c| c.id).collect();
                    let m = msg_for(self, Some(sid));
                    for c in cids {
                        self.detach_client_exec(c, &m, o.get('E'));
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
                        self.detach_client_exec(c, &m, o.get('E'));
                    }
                } else {
                    let m = if o.has('P') { "[detached and SIGHUP]".to_string() } else { m };
                    self.detach_client_exec(cid, &m, o.get('E'));
                }
                Ok(Flow::Next)
            }
            "has-session" => self.resolve_session(ctx, t).map(|_| Flow::Next),
            "kill-session" => {
                let sid = self.resolve_session(ctx, t)?;
                let filter = o.get('f');
                if filter.is_some() && (!o.has('a') || o.has('C')) {
                    return err("-f only valid with -a");
                }
                if o.has('C') {
                    self.sessions.get_mut(&sid).unwrap().alerts.clear();
                    self.mark_session_dirty(sid);
                    return Ok(Flow::Next);
                }
                if o.has('a') {
                    let others: Vec<SessId> = self.sorted_sessions(sort::Criteria { order: None, reversed: false }).into_iter().filter(|s| *s != sid).collect();
                    for s in others {
                        let tt = Target { sid: Some(s), ..Target::default() };
                        if self.sessions.contains_key(&s) && filter.is_none_or(|f| truthy(&self.expand(tt, None, f))) {
                            self.kill_session(s, false);
                        }
                    }
                } else if let Some(g) = self.sessions[&sid].group.filter(|_| o.has('g')) {
                    // -g: every session in the target's group
                    let members: Vec<SessId> = self.sessions.values().filter(|s| s.group == Some(g)).map(|s| s.id).collect();
                    for s in members {
                        if self.sessions.contains_key(&s) {
                            self.kill_session(s, false);
                        }
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
                let fmt = o.get('F').unwrap_or("#{session_name}: #{session_windows} windows (created #{t:session_created})#{?session_grouped, (group ,}#{session_group}#{?session_grouped,),}#{?session_attached, (attached),}");
                let crit = sort::Criteria::from_args(o.get('O'), o.has('r'))?;
                for (i, sid) in self.sorted_sessions(crit).into_iter().enumerate() {
                    let tt = Target { sid: Some(sid), idx: Some(self.sessions[&sid].active), wid: Some(self.sessions[&sid].active_window()), pane: None };
                    self.list_line(ctx, tt, ctx.client, fmt, o.get('f'), HashMap::from([("line".to_string(), i.to_string())]));
                }
                Ok(Flow::Next)
            }
            "list-clients" => {
                let fmt = o.get('F').unwrap_or("#{client_name}: #{session_name} [#{client_width}x#{client_height} #{client_termname}] #{?#{!=:#{client_uid},#{uid}},[user #{?client_user,#{client_user},#{client_uid},}] ,}#{?client_flags,(,}#{client_flags}#{?client_flags,),}");
                let crit = sort::Criteria::from_args(o.get('O'), o.has('r'))?;
                let only = t.map(|t| self.find_session(ctx, t)).transpose()?;
                let mut cids: Vec<ClientId> = self.clients.values().filter(|c| c.attached).map(|c| c.id).collect();
                crit.sort(&mut cids, |a, b, order| {
                    let (a, b) = (&self.clients[a], &self.clients[b]);
                    match order {
                        sort::Order::Name => a.name().cmp(&b.name()),
                        sort::Order::Size => a.size.cmp(&b.size),
                        sort::Order::Creation => (a.created, a.id).cmp(&(b.created, b.id)),
                        sort::Order::Activity => b.activity.cmp(&a.activity),
                        _ => Ordering::Equal,
                    }
                    .then_with(|| a.name().cmp(&b.name()))
                });
                for (i, cid) in cids.into_iter().enumerate() {
                    let Some(sid) = self.client_session(cid) else { continue };
                    if only.is_some_and(|o| o != sid) {
                        continue;
                    }
                    let tt = Target { sid: Some(sid), idx: Some(self.sessions[&sid].active), wid: Some(self.sessions[&sid].active_window()), pane: None };
                    self.list_line(ctx, tt, Some(cid), fmt, o.get('f'), HashMap::from([("line".to_string(), i.to_string())]));
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
                // -c/-L/-R/-U/-D pan a window larger than the client; windows are drawn from
                // their top left here, so only the adjustment is checked
                if ['c', 'L', 'R', 'U', 'D'].into_iter().any(|f| o.has(f)) {
                    if let Some(a) = o.rest.first() {
                        match a.parse::<i64>() {
                            Ok(n) if (1..=i32::MAX as i64).contains(&n) => {}
                            Ok(n) if n < 1 => return err("adjustment too small"),
                            Ok(_) => return err("adjustment too large"),
                            Err(_) => return err("adjustment invalid"),
                        }
                    }
                    if let Some(c) = self.clients.get_mut(&cid) {
                        c.dirty = true;
                    }
                    return Ok(Flow::Next);
                }
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
                let command = spawn_command(&o, false)?;
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
                let pane = self.spawn_pane(Some(sid), sx, sy, &command, cwd.as_deref(), &env)?;
                let name = o.get('n').map(|n| self.expand(self.current(ctx), ctx.client, n));
                let named = if command.first().is_some_and(|c| c == EMPTY_PANE) { &[][..] } else { &command[..] };
                let wname = name.clone().unwrap_or_else(|| self.window_name_for(named, Some(sid)));
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
                let (sid, idx, wid) = (wt.sid.unwrap(), wt.idx.unwrap(), wt.wid.unwrap());
                let filter = o.get('f');
                if filter.is_some() && !o.has('a') {
                    return err("-f only valid with -a");
                }
                if o.has('a') {
                    let matches = |s: &Server, i: u32, w: WinId| {
                        let tt = Target { sid: Some(sid), idx: Some(i), wid: Some(w), pane: Some(s.windows[&w].active) };
                        filter.is_none_or(|f| truthy(&s.expand(tt, None, f)))
                    };
                    // collect ids, not indices: renumber-windows shifts indices as we go
                    let others: Vec<WinId> = self.sessions[&sid].links.iter().filter(|(i, w)| **w != wid && matches(self, **i, **w)).map(|(_, w)| *w).collect();
                    for w in others {
                        if self.windows.contains_key(&w) {
                            self.kill_window(w);
                        }
                    }
                    // the current window too if it is linked here more than once and matches
                    let again: Vec<u32> = self.sessions.get(&sid).map(|s| s.links.iter().filter(|(_, w)| **w == wid).map(|(i, _)| *i).collect()).unwrap_or_default();
                    if again.len() > 1 && again.iter().any(|i| matches(self, *i, wid)) {
                        self.kill_window(wid);
                    }
                } else {
                    let _ = idx;
                    self.kill_window(wid);
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
                let requested = spawn_command(&o, false)?;
                let tt = if cmd == "respawn-window" { self.resolve_window(ctx, t)? } else { self.resolve_pane(ctx, t)? };
                let wid = tt.wid.unwrap();
                let pane = if cmd == "respawn-window" {
                    // kill all other panes
                    let w = &self.windows[&wid];
                    let keep = w.active;
                    let alive: Vec<PaneId> = w.panes().into_iter().filter(|p| *p != keep).collect();
                    // a pane is active while it has a process (empty panes have none)
                    if !o.has('k') && w.panes().iter().any(|p| self.panes.get(p).is_some_and(|x| x.dead.is_none() && x.pid.is_some())) {
                        let sname = tt.sid.and_then(|s| self.sessions.get(&s)).map(|s| s.name.clone()).unwrap_or_default();
                        return err(format!("respawn window failed: window {sname}:{} still active", tt.idx.unwrap_or(0)));
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
                let index = self.windows[&wid].panes().iter().position(|x| *x == pane).unwrap_or(0) as i64 + self.opt_window_num(Some(wid), "pane-base-index");
                let sname = tt.sid.and_then(|s| self.sessions.get(&s)).map(|s| s.name.clone()).unwrap_or_default();
                let p = self.panes.get_mut(&pane).ok_or("no pane")?;
                if p.dead.is_none() && p.pid.is_some() && !o.has('k') {
                    return err(format!("respawn pane failed: pane {sname}:{}.{index} still active", tt.idx.unwrap_or(0)));
                }
                p.kill();
                let generation = p.generation + 1;
                let size = p.size;
                let cwd = o.get('c').map(String::from).unwrap_or_else(|| p.start_path.clone());
                let cmdv = if o.rest.is_empty() && !p.track.argv.is_empty() && !o.has('E') { p.track.argv.clone() } else { requested };
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
                // -Z keeps a zoomed window zoomed
                w.zoomed = w.zoomed && o.has('Z');
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
                let fmt = match o.get('F') {
                    Some(f) => f,
                    None if o.has('a') => "#{session_name}:#{window_index}: #{window_name}#{window_raw_flags} (#{window_panes} panes) [#{window_width}x#{window_height}] ",
                    None => "#{window_index}: #{window_name}#{window_raw_flags} (#{window_panes} panes) [#{window_width}x#{window_height}] [layout #{window_layout}] #{window_id}#{?window_active, (active),}",
                };
                let crit = sort::Criteria::from_args(o.get('O'), o.has('r'))?;
                let sids: Vec<SessId> = if o.has('a') { self.sorted_sessions(sort::Criteria { order: None, reversed: false }) } else { vec![self.resolve_session(ctx, t)?] };
                let mut wls: Vec<(SessId, u32, WinId)> = sids.iter().flat_map(|sid| self.sessions[sid].links.iter().map(|(i, w)| (*sid, *i, *w))).collect();
                self.sort_winlinks(&mut wls, crit);
                // like tmux, #{line} is the number of windows here
                let n = wls.len().to_string();
                for (sid, idx, wid) in wls {
                    let tt = Target { sid: Some(sid), idx: Some(idx), wid: Some(wid), pane: Some(self.windows[&wid].active) };
                    self.list_line(ctx, tt, ctx.client, fmt, o.get('f'), HashMap::from([("line".to_string(), n.clone())]));
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
                let command = spawn_command(&o, o.has('I'))?;
                let new = self.split_pane(tt, o.has('h'), o.has('b'), size, o.has('f'), cwd, &command, &env)?;
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
                let filter = o.get('f');
                if filter.is_some() && !o.has('a') {
                    return err("-f only valid with -a");
                }
                if o.has('a') {
                    let keep = tt.pane.unwrap();
                    let wid = tt.wid.unwrap();
                    let others: Vec<PaneId> = self.windows[&wid]
                        .panes()
                        .into_iter()
                        .filter(|p| *p != keep && filter.is_none_or(|f| truthy(&self.expand(Target { pane: Some(*p), ..tt }, None, f))))
                        .collect();
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
                if let Some(style) = o.get('P') {
                    // deprecated -P: the pane's window-style and window-active-style
                    let v = options::validate("window-style", Some(style), None).map_err(|_| format!("bad style: {style}"))?;
                    let p = self.panes.get_mut(&pane).unwrap();
                    p.options.set("window-style", v.clone());
                    p.options.set("window-active-style", v);
                    self.mark_window_dirty(wid);
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
                let size = match (o.get('l'), o.get('p')) {
                    (Some(l), _) if l.ends_with('%') => l.trim_end_matches('%').parse::<u32>().ok().map(|p| (total as u32 * p / 100) as u16),
                    (Some(l), _) => l.parse().ok(),
                    // -p: a percentage of the target (tmux's older form of -l N%)
                    (None, Some(p)) => {
                        let v = self.expand(dst, ctx.client, p);
                        let pct = v.parse::<u32>().ok().filter(|v| *v <= 100).ok_or_else(|| format!("size or position invalid tiled geometry {v}"))?;
                        Some((total as u32 * pct / 100) as u16)
                    }
                    (None, None) => None,
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
                let crit = sort::Criteria::from_args(o.get('O'), o.has('r'))?;
                let mut targets: Vec<(SessId, u32, WinId)> = Vec::new();
                let sids = if o.has('a') { self.sorted_sessions(sort::Criteria { order: None, reversed: false }) } else if o.has('s') { vec![self.resolve_session(ctx, t)?] } else { Vec::new() };
                for sid in &sids {
                    for (i, w) in &self.sessions[sid].links {
                        targets.push((*sid, *i, *w));
                    }
                }
                if !o.has('a') && !o.has('s') {
                    let wt = self.resolve_window(ctx, t)?;
                    targets.push((wt.sid.unwrap(), wt.idx.unwrap(), wt.wid.unwrap()));
                }
                const SIZE: &str = "[#{pane_width}x#{pane_height}#{?pane_floating_flag, #{pane_x}#,#{pane_y}#,#{pane_z}}] [history #{history_size}/#{history_limit}, #{history_bytes} bytes] #{pane_id}#{?pane_active, (active),}#{?pane_dead, (dead),}";
                let fmt = match o.get('F') {
                    Some(f) => f.to_string(),
                    None if o.has('a') => format!("#{{session_name}}:#{{window_index}}.#{{pane_index}}: {SIZE}"),
                    None if o.has('s') => format!("#{{window_index}}.#{{pane_index}}: {SIZE}"),
                    None => format!("#{{pane_index}}: {SIZE}"),
                };
                for (sid, idx, wid) in targets {
                    // panes are sorted within each window, as tmux does
                    let panes = self.sorted_panes(wid, crit);
                    let n = panes.len().to_string();
                    for p in panes {
                        let tt = Target { sid: Some(sid), idx: Some(idx), wid: Some(wid), pane: Some(p) };
                        self.list_line(ctx, tt, ctx.client, &fmt, o.get('f'), HashMap::from([("line".to_string(), n.clone())]));
                    }
                }
                Ok(Flow::Next)
            }
            "capture-pane" => {
                let tt = self.resolve_pane(ctx, t)?;
                let p = self.panes.get(&tt.pane.unwrap()).ok_or("no pane")?;
                let screen = p.screen();
                let mut out = String::new();
                if o.has('R') {
                    out = capture_grid(screen, self.opt_session_num(tt.sid, "history-limit").max(0) as usize);
                } else if (o.has('P') && !o.has('H')) || (o.has('a') && !screen.alternate_screen()) {
                    // -P: the start of an incomplete escape sequence; the parser keeps none to show
                    if !o.has('P') && !o.has('q') {
                        return err("no alternate screen");
                    }
                } else {
                    let cols = p.size.0;
                    let mode_lines = match (&p.mode, o.has('M')) {
                        (Some(PaneMode::Copy(cm)), true) => Some(cm.render().0),
                        _ => None,
                    };
                    // the rows to read: the mode's screen (-M), the normal screen saved under
                    // the alternate screen (-a, no history) or the pane with its history
                    let (total, hist) = if o.has('a') {
                        (p.size.1 as i64, 0)
                    } else if let Some(l) = &mode_lines {
                        (l.len() as i64, 0)
                    } else {
                        (screen.history_rows_len() as i64, screen.scrollback_len() as i64)
                    };
                    let rows = total - hist;
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
                    let (mut a, mut b) = (to_abs(start), if o.get('E') == Some("-") { total - 1 } else { to_abs(end) });
                    if b < a {
                        std::mem::swap(&mut a, &mut b);
                    }
                    let line_at = |i: usize| -> crate::cells::Line {
                        if let Some(l) = &mode_lines {
                            return l.get(i).cloned().unwrap_or_default();
                        }
                        if o.has('a') {
                            return screen.saved_row(i as u16).map(|r| row_line(&r, cols)).unwrap_or_default();
                        }
                        crate::cells::line_at(screen, i)
                    };
                    for i in a..=b {
                        let (text, wrapped) = if o.has('H') {
                            // hyperlinks (OSC 8) are not kept, so no line has any
                            (String::new(), false)
                        } else if o.has('e') || o.has('N') || o.has('C') || o.has('a') || mode_lines.is_some() {
                            let line = line_at(i as usize);
                            (capture_line(&line, o.has('e'), o.has('N') && !o.has('J'), o.has('T'), o.has('C')), line.wrapped)
                        } else {
                            crate::cells::text_at(screen, i as usize)
                        };
                        if o.has('H') && text.is_empty() {
                            continue;
                        }
                        if o.has('L') {
                            out.push_str(&format!("{} ", i - hist));
                        }
                        if o.has('I') {
                            // when the line entered the history (0 for screen lines)
                            let time = if mode_lines.is_some() || o.has('a') { 0 } else { screen.history_row_time(i as usize) };
                            out.push_str(&format!("{time} "));
                        }
                        if o.has('F') {
                            out.push_str(if wrapped { "W " } else { "- " });
                        }
                        out.push_str(&text);
                        if !(o.has('J') && wrapped) {
                            out.push('\n');
                        }
                    }
                }
                if o.has('p') {
                    // like tmux: the last newline is replaced by one of its own
                    ctx.out.push_str(out.strip_suffix('\n').unwrap_or(&out));
                    ctx.out.push('\n');
                } else if !out.is_empty() {
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
                    p.track.pipe_pid = None;
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
                    let p = self.panes.get_mut(&pane).unwrap();
                    p.pipe = Some(ptx);
                    p.track.pipe_pid = Some(child.id());
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
                if let Some(b) = o.get('b').filter(|b| !super::modes::BOX_LINES.contains(b)) {
                    return err(format!("menu-border-lines unknown value: {b}"));
                }
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
                let mut menu = Menu::at(title, items, x, y, tt.pane, nomouse, o.has('O'), start);
                menu.look = super::modes::OverlayLook {
                    style: o.get('s').map(String::from),
                    selected_style: o.get('H').map(String::from),
                    border_style: o.get('S').map(String::from),
                    lines: o.get('b').map(String::from),
                };
                let epoch = self.replace_overlay(cid, Some(Overlay::Menu(menu)))?;
                Ok(Flow::WaitOverlay(cid, epoch))
            }
            "display-popup" => {
                let cid = self.resolve_client(ctx, o.get('c'))?;
                if let Some(b) = o.get('b').filter(|b| !super::modes::BOX_LINES.contains(b)) {
                    return err(format!("popup-border-lines unknown value: {b}"));
                }
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
                // -B, or -b none: no border
                let border = !o.has('B') && o.get('b') != Some("none");
                let (iw, ih) = if border { (w - 2, h - 2) } else { (w, h) };
                let tt = self.resolve_pane(ctx, t).unwrap_or_else(|_| self.current(ctx));
                let cwd = o.get('d').map(|d| self.expand(tt, Some(cid), d)).or_else(|| tt.pane.and_then(|p| self.panes.get(&p)).map(|p| p.current_path.clone()));
                let pane = self.spawn_pane(tt.sid, iw.max(1), ih.max(1), &o.rest, cwd.as_deref(), &env)?;
                let title = o.get('T').map(|s| self.expand(tt, Some(cid), s)).unwrap_or_default();
                let epoch = self.replace_overlay(cid, Some(Overlay::Popup(Popup {
                    pane, x, y, w, h, border, close_on_exit: o.has('E'), title, any_key: o.has('k') && !o.has('N'),
                    look: super::modes::OverlayLook { style: o.get('s').map(String::from), border_style: o.get('S').map(String::from), lines: o.get('b').map(String::from), ..Default::default() },
                })))?;
                Ok(Flow::WaitOverlay(cid, epoch))
            }
            // ---------------------------------------------------------------- prompts & messages
            "command-prompt" => {
                let cid = self.resolve_client(ctx, t)?;
                let tt = self.current(&Ctx { client: Some(cid), ..ctx.clone() });
                let template = o.rest.first().cloned().unwrap_or_else(|| "%1".into());
                // -F: the template is a format, expanded now
                let template = if o.has('F') { self.expand(tt, Some(cid), &template) } else { template };
                // -l: one prompt and input taken literally, not split at commas
                let split = |v: &str| -> Vec<String> { if o.has('l') { vec![v.to_string()] } else { v.split(',').map(String::from).collect() } };
                let labels: Vec<String> = match o.get('p') {
                    Some(p) => split(p).iter().map(|s| self.expand(tt, Some(cid), s)).collect(),
                    None => vec![if o.rest.is_empty() { ":".into() } else { format!("({})", template.split_whitespace().next().unwrap_or("")) }],
                };
                let inits: Vec<String> = match o.get('I') {
                    Some(i) => split(i).iter().map(|s| self.expand(tt, Some(cid), s)).collect(),
                    None => Vec::new(),
                };
                let prompts: Vec<(String, String)> = labels.into_iter().enumerate().map(|(i, l)| (l, inits.get(i).cloned().unwrap_or_default())).collect();
                let kind = if o.has('k') { PromptKind::Key } else { PromptKind::Command };
                // -T: which history the prompt uses (tmux 3.8 has command and search)
                let search = match o.get('T') {
                    None | Some("command") => false,
                    Some("search") => true,
                    Some(ty) => return err(format!("unknown type: {ty}")),
                };
                self.open_prompt(cid, kind, prompts, template, ctx.pane, o.has('N'), o.has('1'));
                // -e: backspace on empty input closes the prompt; -C (keep updating panes) is
                // how prompts behave here anyway
                if let Some(pr) = self.clients.get_mut(&cid).and_then(|c| c.prompt.as_mut()) {
                    pr.bspace_exit = o.has('e') && !o.has('1') && !o.has('N') && !o.has('i') && !o.has('k');
                    pr.search = search;
                }
                Ok(Flow::Next)
            }
            "confirm-before" => {
                let cid = self.resolve_client(ctx, t)?;
                let command = o.rest.first().ok_or("confirm-before needs a command")?.clone();
                let tt = self.current(&Ctx { client: Some(cid), ..ctx.clone() });
                // -c: the confirming key (printable ASCII), -y: Enter confirms too
                let key = match o.get('c') {
                    Some(c) => match c.as_bytes() {
                        [b] if (32..127).contains(b) => *b as char,
                        _ => return err("invalid confirm key"),
                    },
                    None => 'y',
                };
                let label = match o.get('p') {
                    Some(p) => self.expand(tt, Some(cid), p),
                    None => {
                        // tmux names the first command, not the whole command line
                        let name = cmdparse::parse(&command).ok().and_then(|c| c.first().and_then(|c| c.first()).cloned()).map(|n| canonical(&n).map(String::from).unwrap_or(n));
                        format!("Confirm '{}'? ({key}/n)", name.unwrap_or_else(|| command.clone()))
                    }
                };
                self.open_prompt(cid, PromptKind::Confirm, vec![(label, String::new())], command, ctx.pane, false, true);
                if let Some(pr) = self.clients.get_mut(&cid).and_then(|c| c.prompt.as_mut()) {
                    pr.confirm = (key, o.has('y'));
                }
                Ok(Flow::Next)
            }
            "display-message" => {
                let tt = self.resolve_pane(ctx, t).unwrap_or_else(|_| self.current(ctx));
                let cid = o.get('c').map(|c| self.resolve_client(ctx, Some(c))).transpose()?
                    .or_else(|| ctx.client.filter(|id| self.clients.get(id).is_some_and(|c| c.attached)))
                    .or_else(|| self.resolve_client(ctx, None).ok());
                // the formats use -c (or the current client) if it shows the target session,
                // else the best client of that session, or of any if it has none attached
                let fc = match tt.sid {
                    Some(s) if cid.and_then(|c| self.clients.get(&c)).is_some_and(|c| c.session == Some(s)) => cid,
                    Some(s) => {
                        let attached = |c: &&Client| c.attached && c.session.is_some_and(|x| self.sessions.contains_key(&x));
                        let on_s = self.clients.values().filter(attached).any(|c| c.session == Some(s));
                        self.clients.values().filter(attached).filter(|c| !on_s || c.session == Some(s)).max_by_key(|c| (c.activity, c.id)).map(|c| c.id)
                    }
                    None => None,
                };
                // cmdq_merge_formats adds the command's name
                let extra = HashMap::from([("command".to_string(), "display-message".to_string())]);
                if o.has('a') && !o.has('j') {
                    let mut names: Vec<&str> = VAR_NAMES.to_vec();
                    names.sort();
                    let vc = super::vars::VarCtx { srv: self, t: tt, client: fc, extra: HashMap::new() };
                    // format_each: the table (variables without a value are left out),
                    // then the added entries
                    for n in names {
                        if let Some(v) = crate::format::Vars::get(&vc, n) {
                            ctx.out.push_str(&format!("{n}={v}\n"));
                        }
                    }
                    for (n, v) in &extra {
                        ctx.out.push_str(&format!("{n}={v}\n"));
                    }
                    return Ok(Flow::Next);
                }
                if o.has('F') && !o.rest.is_empty() {
                    return err("only one of -F or argument must be given");
                }
                let fmt = match o.rest.first().map(String::as_str).or(o.get('F')) {
                    Some(f) => f,
                    None if o.has('j') => "",
                    None => "[#{session_name}] #{window_index}:#{window_name}, current pane #{pane_index} - (%H:%M %d-%b-%y)",
                };
                // -l: literal, -j: JSON printed back in tmux's form; -C (keep updating the
                // pane) and -N (ignore keys) are how messages behave here anyway
                let msg = if o.has('l') { fmt.to_string() } else if o.has('v') {
                    // -v: the expansion steps are printed first
                    let (msg, log) = crate::format::expand_time_verbose(fmt, &super::vars::VarCtx { srv: self, t: tt, client: fc, extra });
                    for line in log {
                        ctx.out.push_str(&line);
                        ctx.out.push('\n');
                    }
                    msg
                } else {
                    crate::format::expand_time(fmt, &super::vars::VarCtx { srv: self, t: tt, client: fc, extra })
                };
                let msg = if o.has('j') { json::normalize(&msg)? } else { msg };
                let attached_client = cid.filter(|c| self.clients.get(c).is_some_and(|c| c.attached));
                // without -p the message needs a client to show it on; with none, tmux
                // shows nothing (a command without any client, as from the configuration,
                // reports it as an error)
                if o.has('p') || (attached_client.is_none() && ctx.client.is_none() && ctx.reply.is_none()) {
                    ctx.out.push_str(&msg);
                    ctx.out.push('\n');
                } else if let Some(c) = attached_client {
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
                // -T: the clients' terminals (there is no terminfo description to list,
                // only which terminal each client has), -J: the running jobs
                let mut blank = false;
                if o.has('T') {
                    let only = o.get('t').map(|t| self.resolve_client(ctx, Some(t))).transpose()?;
                    let mut n = 0;
                    for c in self.clients.values().filter(|c| c.attached && c.control == 0 && only.is_none_or(|o| o == c.id)) {
                        let term = c.ident.as_ref().map(|i| i.term.as_str()).unwrap_or("");
                        ctx.out.push_str(&format!("Terminal {n}: {term} for {}, flags=0x0:\n", c.name()));
                        n += 1;
                    }
                    blank = n != 0;
                }
                if o.has('J') {
                    let mut jobs: Vec<(&u32, &Parked)> = self.jobs.iter().collect();
                    jobs.sort_by_key(|(id, j)| std::cmp::Reverse((j.started, **id)));
                    for (n, (_, j)) in jobs.into_iter().enumerate() {
                        if std::mem::take(&mut blank) {
                            ctx.out.push('\n');
                        }
                        let pid = j.pid.map_or(-1, |p| p as i64);
                        ctx.out.push_str(&format!("Job {n}: {} [fd=-1, pid={pid}, status=0]\n", j.cmd));
                    }
                }
                if o.has('T') || o.has('J') {
                    return Ok(Flow::Next);
                }
                // newest first, as "#{t/p:message_time}: #{message_text}"
                for (t, m) in self.messages.iter().rev() {
                    let vars = HashMap::from([("message_time".to_string(), t.to_string())]);
                    ctx.out.push_str(&format!("{}: {m}\n", crate::format::expand(&format!("#{{t/p:message_time}}"), &vars)));
                }
                Ok(Flow::Next)
            }
            "clear-prompt-history" | "show-prompt-history" => {
                // two histories: command and search
                let types: Vec<&str> = match o.get('T') {
                    Some(ty @ ("command" | "search")) => vec![ty],
                    Some(ty) => return err(format!("invalid type: {ty}")),
                    None => vec!["command", "search"],
                };
                for ty in types {
                    let h = if ty == "search" { &mut self.prompt_history_search } else { &mut self.prompt_history };
                    if cmd == "clear-prompt-history" {
                        h.clear();
                        continue;
                    }
                    ctx.out.push_str(&format!("History for {ty}:\n\n"));
                    for (i, v) in h.iter().enumerate() {
                        ctx.out.push_str(&format!("{}: {v}\n", i + 1));
                    }
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
                // separate arguments: ";" separates commands and "{" "}" delimit blocks, as
                // cmd_parse_from_arguments treats them
                let command = if args.len() == 1 { args[0].clone() } else {
                    args.iter().map(|a| match a.as_str() { ";" => "\\;".into(), "{" | "}" => a.clone(), _ => cmdparse::quote(a) }).collect::<Vec<_>>().join(" ")
                };
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
                let crit = sort::Criteria::from_args(o.get('O'), o.has('r'))?;
                let only_key = match o.rest.first() {
                    Some(k) if k.chars().count() > 1 && keys::parse_key_name(k).is_none() && sort::key_code(k) == u64::MAX >> 8 => return err(format!("invalid key: {k}")),
                    Some(k) => Some(keys::parse_key_name(k).map(|k| keys::key_name(&k)).unwrap_or(k.clone())),
                    None => None,
                };
                if let Some(tb) = o.get('T').filter(|tb| !self.bindings.contains_key(*tb)) {
                    return err(format!("table {tb} doesn't exist"));
                }
                let prefix = match o.get('P') {
                    Some(p) => p.to_string(),
                    None => self.opt_session(None, "prefix").replace("None", ""),
                };
                let notes_only = o.has('N');
                // each table is in key order; -N lists prefix then root, each sorted on its own
                fn table_list<'a>(all: &'a BTreeMap<String, BTreeMap<String, Binding>>, tb: &str) -> Vec<(&'a str, &'a str, &'a Binding)> {
                    let mut v: Vec<(&str, &str, &Binding)> = all.get_key_value(tb).map(|(t, m)| m.iter().map(|(k, b)| (t.as_str(), k.as_str(), b)).collect()).unwrap_or_default();
                    v.sort_by_key(|(_, k, _)| sort::key_code(k));
                    v
                }
                let cmp = |a: &(&str, &str, &Binding), b: &(&str, &str, &Binding), order: sort::Order| {
                    let (ka, kb) = (sort::key_code(a.1), sort::key_code(b.1));
                    let mods = 0x00ff_0000_0000_00u64;
                    match order {
                        sort::Order::Index => ka.cmp(&kb),
                        sort::Order::Modifier => (ka & mods).cmp(&(kb & mods)),
                        sort::Order::Name => a.0.to_lowercase().cmp(&b.0.to_lowercase()),
                        _ => Ordering::Equal,
                    }
                    .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
                    .then_with(|| ka.cmp(&kb))
                };
                let mut l: Vec<(&str, &str, &Binding)> = Vec::new();
                if let Some(tb) = o.get('T') {
                    l = table_list(&self.bindings, tb);
                    crit.sort(&mut l, cmp);
                } else if notes_only {
                    for tb in ["prefix", "root"] {
                        let mut v = table_list(&self.bindings, tb);
                        crit.sort(&mut v, cmp);
                        l.extend(v);
                    }
                } else {
                    for tb in self.bindings.keys() {
                        l.extend(table_list(&self.bindings, tb));
                    }
                    crit.sort(&mut l, cmp);
                }
                l.retain(|(_, k, b)| only_key.as_ref().is_none_or(|o| o == k) && !(notes_only && !o.has('a') && b.note.is_empty()));
                if let (Some(k), true) = (o.rest.first(), l.is_empty()) {
                    if only_key.is_some() {
                        return err(format!("unknown key: {k}"));
                    }
                }
                if o.has('1') {
                    l.truncate(1);
                }
                let has_repeat = l.iter().any(|(_, _, b)| b.repeat);
                let key_width = l.iter().map(|(_, k, _)| crate::format::width(k)).max().unwrap_or(0);
                let table_width = l.iter().map(|(t, _, _)| crate::format::width(t)).max().unwrap_or(0);
                let pad = |s: &str, w: usize| format!("{s}{}", " ".repeat(w.saturating_sub(crate::format::width(s))));
                let aliases = self.command_aliases();
                let mut lines = Vec::new();
                for (table, key, b) in &l {
                    let line = match o.get('F') {
                        Some(f) => {
                            let vars = HashMap::from([
                                ("notes_only".to_string(), (notes_only as u8).to_string()),
                                ("key_has_repeat".to_string(), (has_repeat as u8).to_string()),
                                ("key_string_width".to_string(), key_width.to_string()),
                                ("key_table_width".to_string(), table_width.to_string()),
                                ("key_repeat".to_string(), (b.repeat as u8).to_string()),
                                ("key_note".to_string(), b.note.clone()),
                                ("key_prefix".to_string(), prefix.clone()),
                                ("key_table".to_string(), table.to_string()),
                                ("key_string".to_string(), key.to_string()),
                                ("key_command".to_string(), print_command_string(&b.cmd, &aliases, true)),
                            ]);
                            self.expand_extra(self.current(ctx), ctx.client, f, vars)
                        }
                        None if notes_only => format!("{prefix} {} {}", pad(key, key_width), if b.note.is_empty() { print_command_string(&b.cmd, &aliases, true) } else { b.note.clone() }),
                        None => {
                            let r = if !has_repeat { "" } else if b.repeat { "-r" } else { "  " };
                            format!("bind-key {r} -T {} {} {}", pad(table, table_width), pad(&vis::args_escape(key), key_width), print_command_string(&b.cmd, &aliases, true))
                        }
                    };
                    if !line.is_empty() {
                        lines.push(line);
                    }
                }
                let attached = ctx.client.filter(|c| self.clients.get(c).is_some_and(|c| c.attached && c.control == 0));
                match (o.has('1'), attached, lines.first()) {
                    (true, Some(c), Some(line)) => {
                        let line = line.clone();
                        self.set_message(c, &line);
                    }
                    _ => {
                        for line in lines {
                            ctx.out.push_str(&line);
                            ctx.out.push('\n');
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
                if o.has('K') {
                    // -K: the keys are handled as if the client (-c) typed them (prefix, key
                    // tables, bindings); bindings run before the commands after send-keys
                    let Ok(cid) = self.resolve_client(ctx, o.get('c')) else { return Ok(Flow::Next) };
                    let app = self.panes.get(&pane).is_some_and(|p| p.screen().application_cursor());
                    let mut list = Vec::new();
                    for k in &o.rest {
                        let k = if o.has('F') { self.expand(tt, ctx.client, k) } else { k.clone() };
                        let text = if o.has('H') {
                            let bytes = k.split_whitespace().map(|h| u8::from_str_radix(h, 16).map_err(|_| format!("invalid hex: {h}"))).collect::<Result<Vec<u8>, String>>()?;
                            String::from_utf8_lossy(&bytes).into_owned()
                        } else { k };
                        match keys::parse_key_name(&text).filter(|_| !o.has('l') && !o.has('H')) {
                            Some(key) => list.push(key),
                            None => list.extend(text.chars().filter_map(|c| keys::parse_key_name(&c.to_string()))),
                        }
                    }
                    let n: usize = o.get('N').and_then(|v| v.parse().ok()).unwrap_or(1);
                    for _ in 0..n {
                        for key in &list {
                            self.handle_key(cid, key.clone(), keys::encode_key(key, app));
                        }
                    }
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
                let crit = sort::Criteria::from_args(o.get('O'), o.has('r'))?;
                // natural order is newest first, which is also tmux's creation order
                let mut idx: Vec<usize> = (0..self.buffers.len()).collect();
                crit.sort(&mut idx, |a, b, order| {
                    let (pa, pb) = (&self.buffers[*a], &self.buffers[*b]);
                    match order {
                        sort::Order::Name => pa.name.cmp(&pb.name),
                        sort::Order::Creation => a.cmp(b),
                        sort::Order::Size => pa.data.len().cmp(&pb.data.len()),
                        _ => Ordering::Equal,
                    }
                    .then_with(|| pa.name.cmp(&pb.name))
                });
                for i in idx {
                    let vars = buffer_vars(&self.buffers[i]);
                    self.list_line(ctx, Target::default(), ctx.client, fmt, o.get('f'), vars);
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
                // tmux 3.8 passes the text through vis(3) so it can't carry control sequences
                // (a bracketed paste end, for one); -S pastes it unchanged
                let data = if o.has('S') { data } else { vis::strvis(&data, vis::SAFE | vis::NOSLASH) };
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
                if name.is_empty() { return err("empty variable name"); }
                if name.contains('=') { return err("variable name contains ="); }
                let level = if o.has('g') { 0 } else { self.resolve_session(ctx, t)? };
                let map = if level == 0 { &mut self.env } else { &mut self.sessions.get_mut(&level).unwrap().env };
                if o.has('u') {
                    if value.is_some() { return err("can't specify a value with -u"); }
                    map.remove(&name);
                    self.env_hidden.remove(&(level, name));
                } else if o.has('r') {
                    // environ_clear keeps the hidden flag
                    if value.is_some() { return err("can't specify a value with -r"); }
                    map.insert(name, None);
                } else {
                    map.insert(name.clone(), Some(value.ok_or("no value specified")?));
                    // -h: hidden, not passed to new processes; setting without -h shows it again
                    if o.has('h') { self.env_hidden.insert((level, name)); } else { self.env_hidden.remove(&(level, name)); }
                }
                Ok(Flow::Next)
            }
            "show-environment" => {
                let level = if o.has('g') { 0 } else { self.resolve_session(ctx, t)? };
                let map = if level == 0 { self.env.clone() } else { self.sessions[&level].env.clone() };
                if let Some(n) = o.rest.first().filter(|n| !map.contains_key(*n)) {
                    return err(format!("unknown variable: {n}"));
                }
                for (k, v) in map {
                    if o.rest.first().is_some_and(|n| *n != k) {
                        continue;
                    }
                    // hidden variables show only with -h, and -h shows only those
                    if self.env_hidden.contains(&(level, k.clone())) != o.has('h') {
                        continue;
                    }
                    match (v, o.has('s')) {
                        (Some(v), false) => ctx.out.push_str(&format!("{k}={v}\n")),
                        (Some(v), true) => {
                            // POSIX interprets $ ` " and \ in double quotes
                            let escaped: String = v.chars().flat_map(|c| if matches!(c, '$' | '`' | '"' | '\\') { vec!['\\', c] } else { vec![c] }).collect();
                            ctx.out.push_str(&format!("{k}=\"{escaped}\"; export {k};\n"));
                        }
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
                let ft = self.resolve_pane(ctx, t).unwrap_or_else(|_| self.current(ctx));
                let aliases = self.command_aliases();
                for (k, v) in map {
                    if o.rest.first().is_some_and(|n| n.split('[').next() != Some(k.as_str())) {
                        continue;
                    }
                    // hook[N] shows only that element
                    let only: Option<usize> = o.rest.first().and_then(|n| n.split_once('[')).and_then(|(_, i)| i.trim_end_matches(']').parse().ok());
                    for (i, c) in v.iter().enumerate().filter(|(i, c)| !c.is_empty() && only.is_none_or(|o| o == *i)) {
                        match o.get('F') {
                            Some(f) => {
                                let vars = HashMap::from([
                                    ("option_name".to_string(), k.clone()),
                                    ("option_value".to_string(), print_command_string(c, &aliases, false)),
                                    ("option_value_only".to_string(), "0".to_string()),
                                    ("option_is_parent".to_string(), "0".to_string()),
                                    ("option_is_array".to_string(), "1".to_string()),
                                    ("option_is_string".to_string(), "0".to_string()),
                                    ("option_is_hook".to_string(), "1".to_string()),
                                    ("option_is_user".to_string(), (k.starts_with('@') as u8).to_string()),
                                    ("option_has_value".to_string(), "1".to_string()),
                                    ("option_has_array_key".to_string(), "1".to_string()),
                                    ("option_array_key".to_string(), i.to_string()),
                                ]);
                                let line = self.expand_extra(ft, ctx.client, f, vars);
                                ctx.out.push_str(&line);
                                ctx.out.push('\n');
                            }
                            None => ctx.out.push_str(&format!("{k}[{i}] {}\n", print_command_string(c, &aliases, false))),
                        }
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
                // arguments after the command are #{1}, #{2}, ...
                let args: HashMap<String, String> = o.rest.iter().skip(1).enumerate().map(|(i, a)| ((i + 1).to_string(), a.clone())).collect();
                let command = o.rest.first().map(|c| self.expand_extra(tt, ctx.client, c, args)).unwrap_or_default();
                if command.is_empty() {
                    return Ok(Flow::Next);
                }
                let cwd = o.get('c').map(|c| self.expand(tt, ctx.client, c)).or_else(|| tt.pane.and_then(|p| self.panes.get(&p)).map(|p| p.current_path.clone()));
                let flags = format!("{}{}", if o.has('b') { "b" } else { "" }, if o.has('E') { "E" } else { "" });
                Err(format!("\0job-run\0{command}\0{}\0{flags}\0{}", cwd.unwrap_or_default(), tt.pane.map(|p| p.to_string()).unwrap_or_default()))
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
                    match o.get('F') {
                        Some(f) => {
                            let vars = HashMap::from([
                                ("command_list_name".to_string(), n.to_string()),
                                ("command_list_alias".to_string(), a.to_string()),
                                ("command_list_usage".to_string(), format!("{flags}{vals}").trim_start().to_string()),
                            ]);
                            let line = self.expand_extra(self.current(ctx), ctx.client, f, vars);
                            if !line.is_empty() {
                                ctx.out.push_str(&line);
                                ctx.out.push('\n');
                            }
                        }
                        None => ctx.out.push_str(&format!("{n}{alias}{flags}{vals}\n")),
                    }
                }
                Ok(Flow::Next)
            }
            "clear-prompt" => Ok(Flow::Next),
            "server-access" => {
                if o.has('l') {
                    // user entries are U, group entries G; R read-only, W writable
                    for (id, write) in &self.acl {
                        let kind = if id.starts_with("g:") { 'G' } else { 'U' };
                        ctx.out.push_str(&format!("{} ({kind},{})\n", ipc::user_name_for_id(id), if *write { "W" } else { "R" }));
                    }
                    return Ok(Flow::Next);
                }
                if ctx.client.and_then(|c| self.clients.get(&c)).is_some_and(|c| c.user.as_ref().is_some_and(|u| *u != self.owner && !ipc::is_superuser(u))) {
                    return err("only the server owner can change access");
                }
                let arg = o.rest.first().ok_or("missing user or group argument")?;
                let name = self.expand(self.current(ctx), ctx.client, arg);
                let group = o.has('g');
                let kind = if group { "group" } else { "user" };
                let id = if group {
                    if cfg!(windows) {
                        return err("groups are not supported on Windows");
                    }
                    ipc::group_id_for_name(&name)
                } else {
                    ipc::user_id_for_name(&name)
                };
                let id = id.ok_or_else(|| format!("unknown {kind}: {name}"))?;
                if !group && (id == self.owner || ipc::is_superuser(&id)) {
                    return err(format!("{name} owns the server, can't change access"));
                }
                if o.has('a') && o.has('d') {
                    return err("-a and -d cannot be used together");
                }
                if o.has('r') && o.has('w') {
                    return err("-r and -w cannot be used together");
                }
                // the clients an entry applies to: its user, or members of its group without a
                // user entry of their own
                let clients_of = |s: &Server| -> Vec<ClientId> {
                    s.clients
                        .values()
                        .filter(|c| {
                            c.user.as_deref().is_some_and(|u| {
                                if group {
                                    // the owner and root are never limited by a group entry
                                    u != s.owner && !ipc::is_superuser(u) && ipc::group_id_of_user(u).as_deref() == Some(id.as_str()) && !s.acl.iter().any(|a| a.0 == u)
                                } else {
                                    u == id
                                }
                            })
                        })
                        .map(|c| c.id)
                        .collect()
                };
                if o.has('d') {
                    if !self.acl.iter().any(|a| a.0 == id) {
                        return err(format!("{kind} {name} not found"));
                    }
                    self.acl.retain(|a| a.0 != id);
                    for cid in clients_of(self) {
                        // still allowed by the user's group entry?
                        let other = self.clients.get(&cid).and_then(|c| c.user.clone()).and_then(|u| ipc::group_id_of_user(&u)).and_then(|g| self.acl.iter().find(|a| a.0 == g).cloned());
                        if let Some((_, write)) = other {
                            if let Some(c) = self.clients.get_mut(&cid) {
                                c.acl_readonly = !write;
                                c.readonly = !write;
                            }
                            continue;
                        }
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
                if o.has('a') {
                    if self.acl.iter().any(|a| a.0 == id) {
                        return err(format!("{kind} {name} is already added"));
                    }
                }
                if o.has('a') || o.has('r') || o.has('w') {
                    // -r and -w add the entry if it is missing (tmux 3.8)
                    if !self.acl.iter().any(|a| a.0 == id) {
                        self.acl.push((id.clone(), !o.has('r')));
                        ipc::set_shared(true);
                        if cfg!(windows) && self.pipe_acl.as_ref().is_some_and(|p| !p.contains(&id)) {
                            ctx.out.push_str("note: the named pipe's ACL is set when the server starts; put this in the config file or restart the server for it to take effect\n");
                        }
                    }
                } else {
                    return Ok(Flow::Next);
                }
                if o.has('r') || o.has('w') {
                    let e = self.acl.iter_mut().find(|a| a.0 == id).unwrap();
                    e.1 = o.has('w');
                }
                let write = self.acl.iter().find(|a| a.0 == id).is_none_or(|a| a.1);
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
            activity_seq: super::activity_seq(),
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
            return self.attach_or_switch(ctx, sid, false, false, false, None);
        }
        Ok(Flow::Next)
    }

    fn attach_or_switch(&mut self, ctx: &mut Ctx, sid: SessId, detach_others: bool, readonly: bool, no_env: bool, flags: Option<&str>) -> Result<Flow, String> {
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
        // -f sets client flags; -r is read-only,ignore-size
        if let Some(f) = flags {
            self.control_flags(cid, f)?;
        }
        if let Some(c) = self.clients.get_mut(&cid) {
            c.readonly |= readonly || c.acl_readonly;
            c.ctl.ignore_size |= readonly;
        }
        self.switch_client_env(cid, sid, !no_env);
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

    /// The level an option lives at, like options_scope_from_name: a target that
    /// does not resolve reports "no such <level>: <target>" (cmd_find runs with
    /// CMD_FIND_CANFAIL, so its own error is not shown).
    fn option_target(&self, scope: Scope, global: bool, ctx: &Ctx, t: Option<&str>) -> Result<Target, String> {
        let cause = |what: &str| t.map_or_else(|| format!("no current {what}"), |t| format!("no such {what}: {t}"));
        Ok(match scope {
            Scope::Session if !global => Target { sid: Some(self.resolve_session(ctx, t).map_err(|_| cause("session"))?), ..Target::default() },
            Scope::Window if !global => self.resolve_window(ctx, t).map_err(|_| cause("window"))?,
            Scope::Pane => self.resolve_pane(ctx, t).map_err(|_| cause("pane"))?,
            _ => Target::default(),
        })
    }

    /// Resolve the option argument (format-expanded, like tmux) to (name, argument);
    /// None means -q swallowed an error.
    fn option_name(&self, o: &Opts, ctx: &Ctx, arg: &str) -> Result<Option<(String, String)>, String> {
        let arg = if arg.contains('#') { self.expand(self.current(ctx), ctx.client, arg) } else { arg.to_string() };
        match options::canonical_name(&arg) {
            Ok(n) => Ok(Some((n, arg))),
            Err(_) if o.has('q') => Ok(None),
            Err(e) => err(e),
        }
    }

    fn command_aliases(&self) -> Vec<String> {
        self.opts_server.array_items("command-alias").into_iter().map(|(_, v)| v).collect()
    }

    /// The hook map a hook option lives in: the server's with -g, otherwise the
    /// target session's (pwsh-tmux keeps window and pane hooks per session).
    fn hook_level(&self, o: &Opts, ctx: &Ctx) -> Result<Option<SessId>, String> {
        if o.has('g') { return Ok(None); }
        let t = o.get('t');
        self.resolve_session(ctx, t).map(Some)
            .map_err(|_| t.map_or_else(|| "no current session".to_string(), |t| format!("no such session: {t}")))
    }

    /// A hook's commands at a level: the global level holds every hook (an empty
    /// array by default), a session only those set on it.
    fn hook_values(&self, sid: Option<SessId>, base: &str) -> Option<&[String]> {
        match sid {
            None => Some(self.hooks.get(base).map_or(&[][..], |v| v.as_slice())),
            Some(s) => self.sessions.get(&s)?.hooks.get(base).map(|v| v.as_slice()),
        }
    }

    /// set-option on a hook: an array of command lists. Unset elements are kept as
    /// "" so the other keys stay put (string keys are not supported).
    fn set_hook_option(&mut self, o: &Opts, ctx: &Ctx, name: &str, arg: &str, value: Option<String>) -> Result<(), String> {
        let sid = match self.hook_level(o, ctx) {
            Ok(s) => s,
            Err(_) if o.has('q') => return Ok(()),
            Err(e) => return err(e),
        };
        let (base, key) = match name.split_once('[') {
            Some((b, k)) => {
                let k = k.trim_end_matches(']');
                (b, Some(k.parse::<usize>().map_err(|_| format!("bad array key: {k}"))?))
            }
            None => (name, None),
        };
        let already = self.hook_values(sid, base).is_some_and(|v| key.is_none_or(|i| v.get(i).is_some_and(|c| !c.is_empty())));
        if o.has('o') && !o.has('u') && already {
            return if o.has('q') { Ok(()) } else { err(format!("already set: {arg}")) };
        }
        let cmd = match &value {
            _ if o.has('u') || o.has('U') => None,
            None => return err("empty value"),
            Some(v) => Some(command_list_string(v, &self.command_aliases())),
        };
        let map = match sid {
            None => &mut self.hooks,
            Some(s) => &mut self.sessions.get_mut(&s).ok_or("no such session")?.hooks,
        };
        let Some(cmd) = cmd else {
            match key {
                None => { map.remove(base); }
                Some(i) => {
                    if let Some(v) = map.get_mut(base) && i < v.len() {
                        v[i].clear();
                        while v.last().is_some_and(String::is_empty) { v.pop(); }
                    }
                }
            }
            return Ok(());
        };
        // the level gets the (empty) array even when the command does not parse
        let v = map.entry(base.to_string()).or_default();
        let cmd = cmd?;
        let i = match key {
            None => {
                if !o.has('a') { v.clear(); }
                // options_array_assign adds nothing for an empty value, else uses the first free key
                if value.as_deref().unwrap_or("").is_empty() { return Ok(()); }
                v.iter().position(String::is_empty).unwrap_or(v.len())
            }
            Some(i) => i,
        };
        if v.len() <= i { v.resize(i + 1, String::new()); }
        v[i] = cmd;
        Ok(())
    }

    fn cmd_set_option(&mut self, window_cmd: bool, o: &Opts, ctx: &Ctx) -> Result<(), String> {
        let Some((name, arg)) = self.option_name(o, ctx, o.rest.first().ok_or("missing argument")?)? else { return Ok(()) };
        let value = o.rest.get(1).map(|v| if o.has('F') { self.expand(self.current(ctx), ctx.client, v) } else { v.clone() });
        if options::hook_scope(&name).is_some() {
            return self.set_hook_option(o, ctx, &name, &arg, value);
        }
        let scope = Self::option_scope(window_cmd, o, Some(&name));
        // -g has no meaning for server options and is ignored with -p
        let global = scope == Scope::Server || (o.has('g') && scope != Scope::Pane);
        let tt = match self.option_target(scope, global, ctx, o.get('t')) {
            Ok(t) => t,
            Err(_) if o.has('q') => return Ok(()),
            Err(e) => return err(e),
        };
        // keys need an array; -q does not hide this
        if name.contains('[') && !options::is_array(&name) {
            return err(format!("not an array: {arg}"));
        }
        let def = options::def(&name);
        let (present, current) = match self.store(scope, global, tt) {
            Some(s) => (s.has(&name), s.get(&name).map(String::from)),
            None => (false, None),
        };
        if o.has('o') && !o.has('u') && present {
            return if o.has('q') { Ok(()) } else { err(format!("already set: {arg}")) };
        }
        // toggles start from the effective (possibly inherited) value
        let effective = current.clone().or_else(|| match scope {
            Scope::Server => self.opts_server.get(&name).map(String::from),
            Scope::Session => Some(self.opt_session(tt.sid, &name)),
            Scope::Window => Some(self.opt_window(tt.wid, &name)),
            Scope::Pane => tt.pane.map(|p| self.opt_pane(p, &name)),
        });
        // -U also clears the panes of the target window, with or without -g
        if o.has('U') && scope == Scope::Window {
            let wid = if global { self.resolve_window(ctx, o.get('t')).ok().and_then(|t| t.wid) } else { tt.wid };
            let pids = wid.and_then(|w| self.windows.get(&w)).map(|w| w.panes()).unwrap_or_default();
            for p in pids {
                if let Some(p) = self.panes.get_mut(&p) { p.options.unset(&name); }
            }
        }
        let aliases = self.command_aliases();
        let store = self.store_mut(scope, global, tt).ok_or("no such target")?;
        if o.has('u') || o.has('U') {
            // global levels go back to the table default, others inherit again
            if global && def.is_some() { store.reset(&name); } else { store.unset(&name); }
        } else if def.is_some_and(|d| d.kind == options::Kind::Array) && !name.contains('[') {
            store.assign_array(&name, value.as_deref().ok_or("empty value")?, o.has('a'))?;
        } else {
            if let Some((base, _)) = name.split_once('[') {
                // options_empty: the level holds the array before the element is checked
                store.touch_array(base);
            }
            // -a appends to this level's own string value (options_set_string) and
            // checks the result
            let string = def.is_none_or(|d| matches!(d.kind, options::Kind::Str | options::Kind::Style | options::Kind::ColourFormat)
                || (d.kind == options::Kind::Array && d.name != "pane-colours"));
            let mut v = match (&current, &value) {
                (Some(old), Some(new)) if o.has('a') && string => {
                    let sep = if name.contains('[') { "" } else { def.map_or("", |d| d.append_separator()) };
                    options::validate(&name, Some(&format!("{old}{sep}{new}")), None)?
                }
                _ => options::validate(&name, value.as_deref(), effective.as_deref())?,
            };
            if def.is_some_and(|d| d.kind == options::Kind::Command) {
                v = command_list_string(&v, &aliases)?;
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
        let named = match o.rest.first() {
            Some(arg) => match self.option_name(o, ctx, arg)? { Some(n) => Some(n), None => return Ok(()) },
            None => None,
        };
        let name = named.as_ref().map(|n| n.0.as_str());
        let hook = name.is_some_and(|n| options::hook_scope(n).is_some());
        let scope = Self::option_scope(window_cmd, o, name.filter(|_| !hook));
        let global = scope == Scope::Server || (o.has('g') && scope != Scope::Pane);
        // -F: each option as format variables (option_name, option_value, ...) on the target pane
        let ft = self.resolve_pane(ctx, o.get('t')).unwrap_or_else(|_| self.current(ctx));
        let print = |out: &mut String, key: &str, value: Option<&str>, parent: bool| {
            let star = if parent { "*" } else { "" };
            if let Some(f) = o.get('F') {
                let (base, akey) = match key.split_once('[') {
                    Some((b, rest)) => (b, Some(rest.trim_end_matches(']'))),
                    None => (key, None),
                };
                let def = options::def(base);
                let is_hook = options::hook_scope(base).is_some();
                let is_array = is_hook || def.is_some_and(|d| d.kind == options::Kind::Array);
                // OPTIONS_IS_STRING: user options and string-typed ones (styles and string
                // arrays), not commands (hooks), colours or numbers
                let is_string = !is_hook && def.is_none_or(|d| matches!(d.kind, options::Kind::Str | options::Kind::Style)
                    || (d.kind == options::Kind::Array && d.name != "pane-colours"));
                let b = |v: bool| if v { "1".to_string() } else { "0".to_string() };
                let vars = HashMap::from([
                    ("option_name".to_string(), base.to_string()),
                    ("option_value".to_string(), value.unwrap_or("").to_string()),
                    ("option_value_only".to_string(), b(o.has('v'))),
                    ("option_is_parent".to_string(), b(parent)),
                    ("option_is_array".to_string(), b(is_array)),
                    ("option_is_string".to_string(), b(is_string)),
                    ("option_is_hook".to_string(), b(is_hook)),
                    ("option_is_user".to_string(), b(base.starts_with('@'))),
                    ("option_has_value".to_string(), b(value.is_some())),
                    ("option_has_array_key".to_string(), b(akey.is_some())),
                    ("option_array_key".to_string(), akey.unwrap_or("").to_string()),
                ]);
                out.push_str(&self.expand_extra(ft, ctx.client, f, vars));
                out.push('\n');
                return;
            }
            match value {
                Some(v) if o.has('v') => out.push_str(&format!("{v}\n")),
                Some(v) => out.push_str(&format!("{key}{star} {}\n", options::print_value(key, v))),
                None if o.has('v') => {}
                None => out.push_str(&format!("{key}{star}\n")),
            }
        };
        // hooks live in the hook maps: the session's (if not -g), then the global one
        let hook_sid = match self.hook_level(o, ctx) {
            Ok(s) => s,
            Err(_) if o.has('q') && hook => return Ok(()),
            Err(e) if hook => return err(e),
            Err(_) => None,
        };
        let show_hook = |out: &mut String, name: &str, named: bool| {
            let (base, key) = match name.split_once('[') {
                Some((b, k)) => (b, Some(k.trim_end_matches(']'))),
                None => (name, None),
            };
            let (values, parent) = match self.hook_values(hook_sid, base) {
                Some(v) => (v, false),
                None if o.has('A') => (self.hook_values(None, base).unwrap_or_default(), true),
                None => return,
            };
            // set-hook keeps the command as given; it is shown as tmux prints it
            let aliases = self.command_aliases();
            let values: Vec<String> = values.iter().map(|v| if v.is_empty() { String::new() } else { print_command_string(v, &aliases, false) }).collect();
            match key {
                Some(k) => print(out, name, Some(k.parse::<usize>().ok().and_then(|i| values.get(i)).map_or("", String::as_str)), parent),
                None if values.iter().all(String::is_empty) => print(out, base, None, parent && !named),
                None => {
                    for (i, v) in values.iter().enumerate().filter(|(_, v)| !v.is_empty()) {
                        print(out, &format!("{base}[{i}]"), Some(v), parent);
                    }
                }
            }
        };
        if let Some((n, _)) = named.as_ref().filter(|_| hook) {
            let mut out = String::new();
            show_hook(&mut out, n, true);
            ctx.out.push_str(&out);
            return Ok(());
        }
        let tt = match self.option_target(scope, global, ctx, o.get('t')) {
            Ok(t) => t,
            Err(_) if o.has('q') => return Ok(()),
            Err(e) => return err(e),
        };
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
        // print one option like cmd_show_options_print: arrays show each element,
        // a key on a non-array shows the plain value under that key
        let show = |out: &mut String, lvl: &Options, name: &str, parent: bool, named: bool| {
            let base = name.split('[').next().unwrap_or(name);
            if name.contains('[') {
                let v = if options::is_array(name) { lvl.get(name) } else { lvl.get(base) };
                print(out, name, Some(v.unwrap_or("")), parent);
            } else if options::is_array(base) {
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
        match &named {
            Some((n, arg)) => {
                let base = n.split('[').next().unwrap_or(n);
                match find(base) {
                    Some((lvl, parent)) => show(&mut out, lvl, n, parent, true),
                    None if base.starts_with('@') && !o.has('q') => return err(format!("invalid option: {arg}")),
                    None => {}
                }
            }
            None => {
                // user options of this level first, then the table in tmux order
                for (k, v) in store.0.iter().filter(|(k, _)| k.starts_with('@')) {
                    print(&mut out, k, Some(v), false);
                }
                let wanted = |s: Scope| s == scope || (s == Scope::Pane && scope == Scope::Window);
                for d in options::DEFS.iter().filter(|d| wanted(d.scope)) {
                    if let Some((lvl, parent)) = find(d.name) {
                        show(&mut out, lvl, d.name, parent, false);
                    }
                }
                // hooks come last in the table and only show with -H
                if o.has('H') && scope != Scope::Server {
                    for (h, _) in options::HOOKS.iter().filter(|h| wanted(h.1)) {
                        show_hook(&mut out, h, false);
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
            c.prompt = Some(Prompt { kind, prompts, answers: Vec::new(), label, input, cursor, template, history_pos: None, pane, numeric, single_key: single, copy_incremental: None, bspace_exit: false, confirm: ('y', false), search: false });
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

/// paste_make_sample: the first 200 bytes vis-encoded, cut at 200 bytes with "..." when
/// the buffer or its encoding is longer.
pub fn buffer_sample(data: &[u8]) -> String {
    const WIDTH: usize = 200;
    let mut v = vis::strvis(&data[..data.len().min(WIDTH)], vis::OCTAL | vis::CSTYLE | vis::TAB | vis::NL);
    if data.len() > WIDTH || v.len() > WIDTH {
        v.truncate(WIDTH);
        v.extend_from_slice(b"...");
    }
    String::from_utf8_lossy(&v).into_owned()
}

pub fn buffer_vars(b: &Buffer) -> HashMap<String, String> {
    let sample = buffer_sample(&b.data);
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

/// A history row as cells (see cells::line_at).
fn row_line(row: &vt100::capture::RowContents, cols: u16) -> crate::cells::Line {
    crate::cells::Line { cells: (0..cols).map(|c| row.cell(c).map(crate::cells::cell_from_vt).unwrap_or_default()).collect(), wrapped: row.wrapped() }
}

/// One captured line: -e adds attribute sequences, `keep` (-N) keeps trailing spaces (up to
/// the last styled cell with -T), -C escapes backslashes and writes ESC as \033.
fn capture_line(line: &crate::cells::Line, escapes: bool, keep: bool, trim_unused: bool, escape: bool) -> String {
    let mut line = line.clone();
    if escape {
        for c in &mut line.cells {
            if c.text == "\\" {
                c.text = "\\\\".into();
            }
        }
    }
    if keep && trim_unused {
        while line.cells.last().is_some_and(|c| c.text == " " && c.style == crate::cells::Style::default()) {
            line.cells.pop();
        }
    }
    let text = if escapes {
        render::line_with_escapes(&line)
    } else {
        line.cells.iter().map(|c| c.text.as_str()).collect::<String>()
    };
    let text = if escape { text.replace('\x1b', "\\033") } else { text };
    if keep || escapes { text } else { text.trim_end_matches(' ').to_string() }
}

/// capture-pane -R: a dump of the pane's grid in the shape of tmux's (sizes, then each line
/// and cell with its flags, attributes and colours).
fn capture_grid(screen: &vt100::Screen, limit: usize) -> String {
    use std::fmt::Write as _;
    let (sy, sx) = (screen.size().0.get(), screen.size().1.get());
    let hsize = screen.scrollback_len();
    let mut out = format!("G {sx}x{sy} ({hsize}/{limit})\n");
    let colour = |c: vt100::Color| -> (String, u32) {
        const NAMES: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
        match c {
            vt100::Color::Default => ("default".into(), 8),
            vt100::Color::Idx(n) if n < 8 => (NAMES[n as usize].into(), n as u32),
            vt100::Color::Idx(n) if n < 16 => (format!("bright{}", NAMES[n as usize - 8]), 90 + n as u32 - 8),
            vt100::Color::Idx(n) => (format!("colour{n}"), 0x0100_0000 | n as u32),
            vt100::Color::Rgb(r, g, b) => (format!("#{r:02x}{g:02x}{b:02x}"), 0x0200_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32),
        }
    };
    let names = |v: u32, table: &[(u32, &str)]| -> String {
        let s: Vec<&str> = table.iter().filter(|(m, _)| v & m != 0).map(|(_, n)| *n).collect();
        if s.is_empty() { "NONE".into() } else { s.join(",") }
    };
    for yy in 0..screen.history_rows_len() {
        let Some(row) = screen.history_row(yy) else { continue };
        let p = if yy < hsize { "-".to_string() } else { (yy - hsize).to_string() };
        let cells: Vec<Option<&vt100::Cell>> = (0..sx).map(|x| row.cell(x)).collect();
        let used = cells.iter().rposition(|c| c.is_some_and(|c| c.has_contents())).map_or(0, |i| i + 1);
        let lflags = row.wrapped() as u32;
        let _ = writeln!(out, "\tL {yy} ({p}) flags={}[{lflags:x}] {used}/{}", names(lflags, &[(1, "WRAPPED")]), cells.len());
        for (xx, c) in cells.iter().enumerate() {
            let (data, width, padding) = match c {
                Some(c) if c.is_wide_continuation() => (String::new(), 0, true),
                Some(c) if c.has_contents() => (c.contents().to_string(), if c.is_wide() { 2 } else { 1 }, false),
                _ => (" ".to_string(), 1, false),
            };
            let (fg, fgv) = colour(c.map_or(vt100::Color::Default, |c| c.fgcolor()));
            let (bg, bgv) = colour(c.map_or(vt100::Color::Default, |c| c.bgcolor()));
            let attr = c.map_or(0, |c| c.bold() as u32 | (c.dim() as u32) << 1 | (c.underline() as u32) << 2 | (c.inverse() as u32) << 4 | (c.italic() as u32) << 6);
            let flags = (fgv & 0x0100_0000 != 0) as u32 | ((bgv & 0x0100_0000 != 0) as u32) << 1 | (padding as u32) << 2;
            let escaped = String::from_utf8_lossy(&vis::strvis(data.as_bytes(), vis::OCTAL | vis::CSTYLE | vis::TAB | vis::NL)).into_owned();
            let _ = writeln!(
                out,
                "\t\tC {yy},{xx} data=({width},{},{escaped}) flags={}[{flags:x}] attr={}[{attr:x}] fg={fg}[{fgv:x}] bg={bg}[{bgv:x}] us=default[8] link=NONE linkid=NONE",
                data.len(),
                names(flags, &[(1, "FG256"), (2, "BG256"), (4, "PADDING")]),
                names(attr, &[(1, "BRIGHT"), (2, "DIM"), (4, "UNDERSCORE"), (0x10, "REVERSE"), (0x40, "ITALICS")]),
            );
        }
    }
    out
}

pub const VAR_NAMES: &[&str] = &[
    // tmux 3.8's format_table, which display-message -a lists
    "active_window_index", "alternate_on", "alternate_saved_x", "alternate_saved_y", "bracket_paste_flag", "buffer_created", "buffer_full",
    "buffer_mode_format", "buffer_name", "buffer_sample", "buffer_size", "client_activity", "client_cell_height", "client_cell_width",
    "client_colours", "client_control_mode", "client_created", "client_discarded", "client_flags", "client_height", "client_key_table",
    "client_last_session", "client_mode_format", "client_name", "client_pid", "client_prefix", "client_readonly", "client_session",
    "client_termfeatures", "client_termname", "client_termtype", "client_theme", "client_tty", "client_uid", "client_user", "client_utf8",
    "client_width", "client_written", "config_files", "cursor_blinking", "cursor_character", "cursor_colour", "cursor_flag", "cursor_shape",
    "cursor_very_visible", "cursor_x", "cursor_y", "history_added", "history_all_bytes", "history_bytes", "history_collected", "history_generation",
    "history_limit", "history_size", "host", "host_short", "insert_flag", "keypad_cursor_flag", "keypad_flag", "last_window_index", "mouse_all_flag",
    "mouse_any_flag", "mouse_button_flag", "mouse_hyperlink", "mouse_line", "mouse_pane", "mouse_sgr_flag", "mouse_standard_flag",
    "mouse_status_line", "mouse_status_range", "mouse_utf8_flag", "mouse_word", "mouse_x", "mouse_y", "next_session_id", "origin_flag", "pane_active",
    "pane_at_bottom", "pane_at_left", "pane_at_right", "pane_at_top", "pane_bg", "pane_bottom", "pane_command_duration", "pane_command_end_time",
    "pane_command_running", "pane_command_start_time", "pane_command_status", "pane_current_command", "pane_current_path", "pane_dead",
    "pane_dead_signal", "pane_dead_status", "pane_dead_time", "pane_fg", "pane_flags", "pane_floating_flag", "pane_format", "pane_height", "pane_id",
    "pane_index", "pane_in_mode", "pane_input_off", "pane_key_mode", "pane_last", "pane_last_output_time", "pane_last_prompt_time", "pane_left",
    "pane_marked", "pane_marked_set", "pane_modal_flag", "pane_mode", "pane_output_generation", "pane_path", "pane_pb_progress", "pane_pb_state",
    "pane_pid", "pane_pipe", "pane_pipe_pid", "pane_private_modes", "pane_right", "pane_search_string", "pane_start_command",
    "pane_start_command_list", "pane_start_path", "pane_synchronized", "pane_tabs", "pane_title", "pane_top", "pane_tty", "pane_unseen_changes",
    "pane_unzoomed_height", "pane_unzoomed_width", "pane_width", "pane_x", "pane_y", "pane_z", "pane_zoomed_flag", "pid", "scroll_region_lower",
    "scroll_region_upper", "server_sessions", "session_active", "session_activity", "session_activity_flag", "session_alert", "session_alerts",
    "session_attached", "session_attached_list", "session_bell_flag", "session_created", "session_format", "session_group", "session_group_attached",
    "session_group_attached_list", "session_grouped", "session_group_list", "session_group_many_attached", "session_group_size", "session_id",
    "session_last_attached", "session_many_attached", "session_marked", "session_name", "session_path", "session_silence_flag", "session_stack",
    "session_windows", "sixel_support", "socket_path", "start_time", "synchronized_output_flag", "tree_mode_format", "uid", "user", "version",
    "window_active", "window_active_clients", "window_active_clients_list", "window_active_sessions", "window_active_sessions_list",
    "window_activity", "window_activity_flag", "window_bell_flag", "window_bigger", "window_cell_height", "window_cell_width", "window_end_flag",
    "window_flags", "window_format", "window_height", "window_id", "window_index", "window_last_flag", "window_layout", "window_linked",
    "window_linked_sessions", "window_linked_sessions_list", "window_manual_height", "window_manual_width", "window_marked_flag", "window_modal_pane",
    "window_name", "window_offset_x", "window_offset_y", "window_panes", "window_raw_flags", "window_silence_flag", "window_stack_index",
    "window_start_flag", "window_visible_layout", "window_width", "window_zoomed_flag", "wrap_flag",
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
