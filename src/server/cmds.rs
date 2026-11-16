//! Command table, target resolution and the implementation of every command.

use super::modes::{Collapse, Menu, MenuItem, Panes, Popup, Preview, Sort, Switch, Tree, TreeKind};
use super::vars::Target;
use super::*;
use crate::cmdparse;
use crate::layout::{self, Dir};
use crate::opts::{getopt_optional, Opts};
use std::cmp::Ordering;

pub(crate) mod json;
pub(crate) mod sort;
mod vis;

/// The names of all commands, for prompt completion.
pub fn command_names() -> impl Iterator<Item = &'static str> {
    COMMANDS.iter().map(|c| c.0)
}

/// (name, alias, flags without value, flags with value)
const COMMANDS: &[(&str, &str, &str, &str)] = &[
    ("attach-session", "attach", "dErx", "cfst"),
    ("bind-key", "bind", "nr", "TN"),
    ("break-pane", "breakp", "abdPW", "FnstLxXyY"),
    ("capture-pane", "capturep", "aCeFHIJLMNpPqRT", "bESt"),
    ("choose-buffer", "", "kNrZy", "FfKOt"),
    ("choose-client", "", "hikNrZy", "FfKOt"),
    ("choose-tree", "", "GhkNrswZy", "FfKOt"),
    ("clear-history", "clearhist", "H", "t"),
    ("clear-prompt-history", "clearphist", "", "T"),
    ("clock-mode", "", "", "t"),
    ("command-prompt", "", "1bCeFkilNP", "IpTt"),
    ("confirm-before", "confirm", "by", "cpt"),
    ("copy-mode", "", "dekHMqSu", "st"),
    ("customize-mode", "", "kNZy", "Fft"),
    ("delete-buffer", "deleteb", "", "b"),
    ("detach-client", "detach", "aP", "Esct"),
    ("display-menu", "menu", "MO", "bcCHstSTxy"),
    ("display-message", "display", "aCIjlNpv", "cdFt"),
    ("display-panes", "displayp", "kNZ", "dst"),
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
    ("move-pane", "movep", "bdfhMvDLRU", "lPstXYz"),
    ("move-window", "movew", "abdkr", "st"),
    ("new-pane", "newp", "AbCDdEfhIkKLMOPvWZ", "BcefFlmpRsStTxXyY"),
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
    ("set-hook", "", "agpERTuw", "Bt"),
    ("set-option", "set", "aFgopqsuUw", "t"),
    ("set-window-option", "setw", "aFgoqu", "t"),
    ("show-buffer", "showb", "", "b"),
    ("show-environment", "showenv", "hgs", "t"),
    ("show-hooks", "", "Bgpw", "Ft"),
    ("show-messages", "showmsgs", "JT", "t"),
    ("show-options", "show", "AgHpqsvw", "Ft"),
    ("show-prompt-history", "showphist", "", "T"),
    ("show-window-options", "showw", "gv", "Ft"),
    ("source-file", "source", "Fnqv", "t"),
    ("split-window", "splitw", "bdEfhIkPvWZ", "BcefFlmpRsStT"),
    ("start-server", "start", "", ""),
    ("suspend-client", "suspendc", "", "t"),
    ("swap-pane", "swapp", "dDUZ", "st"),
    ("swap-window", "swapw", "d", "st"),
    ("switch-client", "switchc", "EFlnprZ", "cOtT"),
    ("switch-mode", "", "kswZ", "Ft"),
    ("unbind-key", "unbind", "anq", "T"),
    ("unlink-window", "unlinkw", "k", "t"),
    ("wait-for", "wait", "ELSUlv", "Fw"),
];

/// A command's flags without and with a value.
pub fn command_flags(full: &str) -> (&'static str, &'static str) {
    COMMANDS.iter().find(|c| c.0 == full).map_or(("", ""), |c| (c.2, c.3))
}

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
/// An empty pane from a single empty argument: its argv stays [""] (pane-created's
/// pane_command is `''`).
pub const EMPTY_PANE_ARG: &str = "\u{1}empty-pane-arg";

/// Whether a spawn command is one of the empty pane markers.
pub fn is_empty_pane(cmd: &[String]) -> bool {
    matches!(cmd, [one] if one == EMPTY_PANE || one == EMPTY_PANE_ARG)
}

/// The command for a new pane: -E (or a single empty argument, or `input` for split-window
/// -I) makes an empty pane, which can't also be given a command.
pub(super) fn spawn_command(o: &Opts, input: bool) -> Result<Vec<String>, String> {
    let empty_arg = matches!(o.rest.as_slice(), [one] if one.is_empty());
    let empty = o.has('E') || input || empty_arg;
    if empty && !o.rest.is_empty() && !empty_arg {
        return err("command cannot be given for empty pane");
    }
    Ok(match (empty, empty_arg) {
        (true, true) => vec![EMPTY_PANE_ARG.to_string()],
        (true, false) => vec![EMPTY_PANE.to_string()],
        _ => o.rest.clone(),
    })
}

/// The checks cmd_parse_build_commands makes before anything runs: the command
/// exists, its flags parse and the argument count fits (args_parse).
pub fn check_command(argv: &[String]) -> Result<(), String> {
    let Some((name, rest)) = argv.split_first() else { return Ok(()) };
    if !name.starts_with('-') && name.contains('=') && rest.is_empty() {
        return Ok(());
    }
    let full = canonical(name).ok_or_else(|| format!("unknown command: {name}"))?;
    let mut args = rest.to_vec();
    if ENV_COMMANDS.contains(&full) {
        take_env(&mut args);
    }
    parse_args(full, &args).map(|_| ())
}

/// Commands whose `-e VAR=value` flags are taken out before args_parse.
const ENV_COMMANDS: &[&str] = &["new-session", "new-window", "split-window", "new-pane", "respawn-pane", "respawn-window", "display-popup"];

/// args_parse for a command: its flags, those with an optional value (tmux `D::`), and
/// the argument count, with the errors cmd_parse reports.
fn parse_args(full: &str, args: &[String]) -> Result<Opts, String> {
    let spec = COMMANDS.iter().find(|c| c.0 == full).unwrap();
    let optional = if matches!(full, "move-pane" | "resize-pane") { "DLRU" } else { "" };
    let o = getopt_optional(args, spec.2, spec.3, optional).map_err(|e| format!("command {full}: {e}"))?;
    if let Some(&(_, min, max)) = ARG_COUNTS.iter().find(|a| a.0 == full) {
        if (o.rest.len() as i32) < min {
            return err(format!("command {full}: too few arguments (need at least {min})"));
        }
        if max != -1 && o.rest.len() as i32 > max {
            return err(format!("command {full}: too many arguments (need at most {max})"));
        }
    }
    Ok(o)
}

/// Store a command option (default-client-command, hooks) the way tmux parses
/// it with cmd_parse_from_string and prints it with cmd_list_print: command-alias
/// entries expanded, unknown commands and flags rejected, flags sorted as
/// args_print does, { } blocks kept as command lists, commands joined by " ; ".
pub(super) fn command_list_string(v: &str, aliases: &[String]) -> Result<String, String> {
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

/// The command string of bind-key's arguments after the key. Separate arguments:
/// ";" separates commands as cmd_parse_from_arguments does ("{" and "}" are plain
/// strings there); { } blocks of a parsed command come marked and stay blocks.
pub fn binding_command(args: &[String]) -> String {
    let block = |a: &str| a.strip_prefix(cmdparse::BLOCK_MARK).map(str::to_string);
    if args.len() == 1 {
        return block(&args[0]).unwrap_or_else(|| args[0].clone());
    }
    cmdparse::split_arguments(args).iter().map(|c| {
        c.iter().map(|a| block(a).map_or_else(|| cmdparse::quote(a), |b| format!("{{ {b} }}"))).collect::<Vec<_>>().join(" ")
    }).collect::<Vec<_>>().join(" ; ")
}

/// Flags whose value is optional ("D::" in the tmux template): without a value
/// args_print shows them bare and ends the flags with "--".
fn optional_value_flag(cmd: &str, c: char) -> bool {
    matches!(cmd, "resize-pane" | "move-pane") && "DLRU".contains(c)
}

/// Whether positional argument `idx` of a command may be a { } block
/// (ARGS_PARSE_COMMANDS_OR_STRING in the command's args_parse callback).
fn takes_commands(cmd: &str, idx: usize, flags: &[char]) -> bool {
    match cmd {
        "if-shell" => idx == 1 || idx == 2,
        "set-option" | "set-window-option" => idx == 1,
        "set-hook" => idx == 1 || flags.contains(&'B'),
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
            if optional_value_flag(full, c) {
                // args_parse_flag_argument: the next word is the value unless it looks like a flag
                let inline: String = chars[j + 1..].iter().collect();
                let v = if !inline.is_empty() { Some(inline) } else {
                    match args.get(i) {
                        Some((v, false)) if !(v.starts_with('-') && v[1..].starts_with(|n: char| n == '-' || n.is_ascii_alphabetic())) => {
                            i += 1;
                            Some(v.clone())
                        }
                        _ => None,
                    }
                };
                valued.push((c, v));
                break;
            } else if spec.2.contains(c) {
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
                valued.push((c, Some(v)));
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
    let ends_optional = valued.last().is_some_and(|(_, v)| v.is_none());
    out.extend(valued.into_iter().map(|(c, v)| match v {
        Some(v) => format!("-{c} {}", options::escape_arg(&v)),
        None => format!("-{c}"),
    }));
    if ends_optional {
        out.push("--".into());
    }
    for (k, (a, brace)) in args[i..].iter().enumerate() {
        if *brace && !takes_commands(full, k, &bare) {
            return Err(fail(format!("argument {} must be \"string\"", k + 1)));
        }
        out.push(if *brace { a.clone() } else { options::escape_arg(a) });
    }
    // args_parse checks the argument counts last
    if let Some(&(_, min, max)) = ARG_COUNTS.iter().find(|a| a.0 == full) {
        let n = (args.len() - i) as i32;
        if n < min {
            return Err(fail(format!("too few arguments (need at least {min})")));
        }
        if max != -1 && n > max {
            return Err(fail(format!("too many arguments (need at most {max})")));
        }
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

/// The target type of a command (CMD_FIND_SESSION, _WINDOW, _PANE).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Find {
    Session,
    Window,
    Pane,
}

/// CMD_FIND_CANFAIL: an unresolved target is an empty target, not an error.
pub const FIND_CANFAIL: u8 = 1;

const DIRS: [(char, Dir); 4] = [('L', Dir::Left), ('R', Dir::Right), ('U', Dir::Up), ('D', Dir::Down)];

impl Server {
    // ---- target resolution ---------------------------------------------------------

    /// The default target for a command context.
    pub fn current(&self, ctx: &Ctx) -> Target {
        if let Some(t) = ctx.target.filter(|t| t.pane == ctx.pane && self.target_is_valid(*t)) {
            return t;
        }
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

    /// cmd_find_get_session: a `$id`, an exact name, a client's session, else (unless
    /// exact) a unique prefix and then a unique fnmatch pattern.
    fn get_session(&self, name: &str, exact: bool) -> Option<SessId> {
        if let Some(id) = name.strip_prefix('$') {
            return id.parse().ok().filter(|id| self.sessions.contains_key(id));
        }
        if let Some(s) = self.sessions.values().find(|s| s.name == name) {
            return Some(s.id);
        }
        if let Some(sid) = self.clients.values().find(|c| c.attached && c.name() == name).and_then(|c| c.session).filter(|s| self.sessions.contains_key(s)) {
            return Some(sid);
        }
        if exact {
            return None;
        }
        let unique = |f: &dyn Fn(&Session) -> bool| -> Result<Option<SessId>, ()> {
            let m: Vec<SessId> = self.sessions.values().filter(|s| f(s)).map(|s| s.id).collect();
            match m.len() {
                0 => Ok(None),
                1 => Ok(Some(m[0])),
                _ => Err(()),
            }
        };
        match unique(&|s| s.name.starts_with(name)) {
            Ok(Some(s)) => return Some(s),
            Ok(None) => {}
            Err(()) => return None,
        }
        unique(&|s| fnmatch(name, &s.name)).ok().flatten()
    }

    /// cmd_find_best_session_with_window and cmd_find_best_winlink_with_window: the most
    /// recently active session holding the window, at its current winlink or the first.
    fn best_link(&self, wid: WinId, sid: Option<SessId>) -> Option<(SessId, u32)> {
        let links = self.links_of(wid);
        let sid = match sid {
            Some(s) => s,
            None => links.iter().map(|l| l.0).max_by_key(|s| self.sessions[s].activity_seq)?,
        };
        let s = self.sessions.get(&sid)?;
        if s.links.get(&s.active) == Some(&wid) {
            return Some((sid, s.active));
        }
        s.links.iter().find(|(_, w)| **w == wid).map(|(i, _)| (sid, *i))
    }

    /// A session by name for commands that look one up directly: a whole session target.
    fn find_session(&self, ctx: &Ctx, name: &str) -> Result<SessId, String> {
        let t = self.cmd_find(ctx, Some(name), Find::Session, 0)?;
        t.sid.ok_or_else(|| format!("can't find session: {name}"))
    }

    pub fn resolve_session(&self, ctx: &Ctx, t: Option<&str>) -> Result<SessId, String> {
        let t = self.cmd_find(ctx, t, Find::Session, 0)?;
        t.sid.ok_or_else(|| "no current target".to_string())
    }

    pub fn resolve_window(&self, ctx: &Ctx, t: Option<&str>) -> Result<Target, String> {
        self.cmd_find(ctx, t, Find::Window, 0)
    }

    pub fn resolve_pane(&self, ctx: &Ctx, t: Option<&str>) -> Result<Target, String> {
        self.cmd_find(ctx, t, Find::Pane, 0)
    }

    /// cmd_find_target: split a target into session, window and pane parts (a bare
    /// word is the part the command's target type names, or a `$`/`@`/`%` id), look
    /// each up, and report the part that failed. With FIND_CANFAIL a failure is an
    /// empty target instead of an error.
    pub fn cmd_find(&self, ctx: &Ctx, target: Option<&str>, ty: Find, flags: u8) -> Result<Target, String> {
        let canfail = flags & FIND_CANFAIL != 0;
        let fail = |e: String| if canfail { Ok(Target::default()) } else { Err(e) };
        let target = target.unwrap_or("");
        if let Some(c) = self.client_current(ctx, target) {
            return c;
        }
        let cur = self.current(ctx);
        let (Some(csid), Some(cwid)) = (cur.sid, cur.wid) else {
            return fail("no current target".to_string());
        };
        if target.is_empty() {
            return Ok(cur);
        }
        let mk = |sid: SessId, idx: u32| -> Target {
            let wid = self.sessions[&sid].links[&idx];
            Target { sid: Some(sid), idx: Some(idx), wid: Some(wid), pane: Some(self.windows[&wid].active) }
        };
        if target == "{mouse}" || target == "=" {
            let m = ctx.mouse.as_ref();
            if ty == Find::Pane {
                if let Some(p) = m.and_then(|m| self.mouse_pane(m)) {
                    let wid = self.window_of(p);
                    let sid = m.and_then(|m| m.sid).filter(|s| self.sessions.contains_key(s));
                    let idx = m.and_then(|m| m.idx).filter(|_| sid.is_some());
                    return Ok(Target { sid: sid.or(cur.sid), idx: idx.or(cur.idx), wid, pane: Some(p) });
                }
            }
            if let Some(wid) = m.and_then(|m| m.wid).filter(|w| self.windows.contains_key(w)) {
                let sid = m.and_then(|m| m.sid).filter(|s| self.sessions.contains_key(s));
                return Ok(Target { sid, idx: m.and_then(|m| m.idx), wid: Some(wid), pane: Some(self.windows[&wid].active) });
            }
            return fail("no mouse target".to_string());
        }
        if target == "{marked}" || target == "~" {
            return match self.marked.filter(|p| self.window_of(*p).is_some()) {
                Some(p) => Ok(self.target_for_pane(p)),
                None => fail("no marked target".to_string()),
            };
        }

        let (copy, colon) = match target.split_once(':') {
            Some((a, b)) => (a, Some(b)),
            None => (target, None),
        };
        let (mut session, mut window, mut pane) = (None, None, None);
        let (mut window_only, mut pane_only) = (false, false);
        match colon {
            Some(rest) => {
                session = Some(copy);
                window_only = true;
                match rest.split_once('.') {
                    Some((w, p)) => (window, pane, pane_only) = (Some(w), Some(p), true),
                    None => window = Some(rest),
                }
            }
            None => match copy.split_once('.') {
                Some((w, p)) => (window, pane, pane_only) = (Some(w), Some(p), true),
                None if copy.starts_with('$') => session = Some(copy),
                None if copy.starts_with('@') => window = Some(copy),
                None if copy.starts_with('%') => pane = Some(copy),
                None => match ty {
                    Find::Session => session = Some(copy),
                    Find::Window => window = Some(copy),
                    Find::Pane => pane = Some(copy),
                },
            },
        }
        let exact_session = session.is_some_and(|s| s.starts_with('='));
        let session = session.map(|s| s.strip_prefix('=').unwrap_or(s)).filter(|s| !s.is_empty());
        let exact_window = window.is_some_and(|w| w.starts_with('='));
        let window = window.map(|w| w.strip_prefix('=').unwrap_or(w)).filter(|w| !w.is_empty());
        let pane = pane.filter(|p| !p.is_empty()).map(|p| match p {
            "{last}" => "!",
            "{next}" => "+",
            "{previous}" => "-",
            _ => p,
        });

        // with CANFAIL, what was found before the failing part stays in the state
        let partial = |t: Target, e: String| if canfail { Ok(t) } else { Err(e) };
        let no_session = |s: &str| fail(format!("can't find session: {s}"));
        let no_window = |t: Target, w: &str| partial(t, format!("can't find window: {w}"));
        let no_pane = |t: Target, p: &str| partial(t, format!("can't find pane: {p}"));
        let empty = Target::default();
        // cmd_find_get_window_with_session
        let window_in = |sid: SessId, w: &str| -> Option<u32> {
            if let Some(id) = w.strip_prefix('@') {
                let wid = id.parse::<WinId>().ok()?;
                return self.best_link(wid, Some(sid)).map(|l| l.1);
            }
            let w = if exact_window { format!("={w}") } else { w.to_string() };
            self.find_window_in(sid, &w, ctx).ok()
        };
        // cmd_find_get_pane_with_window
        let pane_in = |t: Target, p: &str| -> Option<Target> {
            let wid = t.wid?;
            let cur_pane = if t.wid == cur.wid { ctx.pane } else { None };
            if p.starts_with('%') {
                let id = p[1..].parse::<PaneId>().ok()?;
                return self.windows[&wid].panes().contains(&id).then_some(Target { pane: Some(id), ..t });
            }
            self.find_pane_in(wid, p, cur_pane, ctx).ok().map(|id| Target { pane: Some(id), ..t })
        };
        // cmd_find_get_window: an id, a window of the current session, else a session
        let get_window = |w: &str, only: bool| -> Option<Target> {
            if let Some(id) = w.strip_prefix('@') {
                let wid = id.parse::<WinId>().ok().filter(|w| self.windows.contains_key(w))?;
                let (sid, idx) = self.best_link(wid, None)?;
                return Some(mk(sid, idx));
            }
            if let Some(idx) = window_in(csid, w) {
                return Some(mk(csid, idx));
            }
            if !only && let Some(sid) = self.get_session(w, exact_session) {
                return Some(mk(sid, self.sessions[&sid].active));
            }
            None
        };

        if let Some(sn) = session {
            let Some(sid) = self.get_session(sn, exact_session) else { return no_session(sn) };
            let here = mk(sid, self.sessions[&sid].active);
            let only_session = Target { sid: Some(sid), ..empty };
            return match (window, pane) {
                (None, None) => Ok(here),
                (Some(w), None) => match window_in(sid, w) {
                    Some(idx) => Ok(mk(sid, idx)),
                    None => no_window(only_session, w),
                },
                (None, Some(p)) => {
                    if let Some(id) = p.strip_prefix('%') {
                        let found = id.parse::<PaneId>().ok().and_then(|id| Some((id, self.window_of(id)?)));
                        return match found.and_then(|(id, wid)| Some((id, self.best_link(wid, Some(sid))?))) {
                            Some((id, (_, idx))) => Ok(Target { pane: Some(id), ..mk(sid, idx) }),
                            None => no_pane(only_session, p),
                        };
                    }
                    match pane_in(here, p) {
                        Some(t) => Ok(t),
                        None => no_pane(Target { pane: None, ..here }, p),
                    }
                }
                (Some(w), Some(p)) => {
                    let Some(idx) = window_in(sid, w) else { return no_window(only_session, w) };
                    match pane_in(mk(sid, idx), p) {
                        Some(t) => Ok(t),
                        None => no_pane(Target { pane: None, ..mk(sid, idx) }, p),
                    }
                }
            };
        }
        match (window, pane) {
            (Some(w), p) => {
                let Some(t) = get_window(w, window_only) else { return no_window(empty, w) };
                match p {
                    None => Ok(t),
                    Some(p) => match pane_in(t, p) {
                        Some(t) => Ok(t),
                        None => no_pane(Target { pane: None, ..t }, p),
                    },
                }
            }
            (None, Some(p)) => {
                // cmd_find_get_pane: an id, a pane of the current window, else a window
                if let Some(id) = p.strip_prefix('%') {
                    return match id.parse::<PaneId>().ok().filter(|id| self.panes.contains_key(id)).and_then(|id| Some((id, self.window_of(id)?))) {
                        Some((id, wid)) => match self.best_link(wid, None) {
                            Some((sid, idx)) => Ok(Target { pane: Some(id), ..mk(sid, idx) }),
                            None => no_pane(empty, p),
                        },
                        None => no_pane(empty, p),
                    };
                }
                let here = Target { sid: Some(csid), idx: cur.idx, wid: Some(cwid), pane: cur.pane };
                if let Some(t) = pane_in(here, p) {
                    return Ok(t);
                }
                match (!pane_only).then(|| get_window(p, false)).flatten() {
                    Some(t) => Ok(t),
                    None => no_pane(empty, p),
                }
            }
            (None, None) => Ok(cur),
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
        // window_find_string names a corner or edge, in any case
        let lower = p.to_ascii_lowercase();
        let p = match lower.as_str() {
            "top" | "bottom" | "left" | "right" | "top-left" | "top-right" | "bottom-left" | "bottom-right" => format!("{{{lower}}}"),
            _ => p.to_string(),
        };
        let p = p.as_str();
        let r = match p {
            "" => Some(active),
            "{last}" | "!" => w.last(),
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

    /// Target client for commands with -c/-t client: a client name, or the context's client.
    /// The target client of a CMD_CLIENT_CANFAIL command: a client that is not found
    /// is no error, there is just no target client.
    pub fn resolve_client_canfail(&self, ctx: &Ctx, t: Option<&str>) -> Option<ClientId> {
        self.resolve_client(ctx, t).ok()
    }

    pub fn resolve_client(&self, ctx: &Ctx, t: Option<&str>) -> Result<ClientId, String> {
        match t {
            Some(name) => {
                // cmd_find_client: one trailing ':' is trimmed, then only client names
                // match (a session name is not a client)
                let n = name.strip_suffix(':').unwrap_or(name);
                if let Some(c) = self.clients.values().find(|c| c.attached && (c.name() == n || c.id.to_string() == n)) {
                    return Ok(c.id);
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
                    .max_by_key(|c| c.activity_seq)
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
                sort::Order::Activity => wb.activity_us.cmp(&wa.activity_us),
                sort::Order::Name => wa.name.cmp(&wb.name),
                sort::Order::Size => (wa.sx as u32 * wa.sy as u32).cmp(&(wb.sx as u32 * wb.sy as u32)),
                _ => Ordering::Equal,
            }
            .then_with(|| wa.name.cmp(&wb.name))
        });
    }

    /// The panes of a window sorted by the -O order. Activity is tmux's active_point
    /// (the order panes were last made active, never-active panes first), which the
    /// last-panes stack holds most recent first.
    fn sorted_panes(&self, wid: WinId, crit: sort::Criteria) -> Vec<PaneId> {
        let w = &self.windows[&wid];
        let order_of = w.panes();
        let mut v = order_of.clone();
        let title = |p: &PaneId| self.panes.get(p).map(|x| x.title.as_str()).unwrap_or("");
        let index = |p: &PaneId| order_of.iter().position(|x| x == p).unwrap_or(0);
        let point = |p: &PaneId| {
            if *p == w.active {
                usize::MAX
            } else {
                w.last_panes.iter().position(|x| x == p).map_or(0, |i| w.last_panes.len() - i)
            }
        };
        let size = |p: &PaneId| self.panes.get(p).map(|x| x.size.0 as u32 * x.size.1 as u32).unwrap_or(0);
        crit.sort(&mut v, |a, b, order| {
            match order {
                sort::Order::Activity => point(a).cmp(&point(b)),
                sort::Order::Creation => a.cmp(b),
                sort::Order::Size => size(a).cmp(&size(b)),
                sort::Order::Index => index(a).cmp(&index(b)),
                // floating panes front to back, then the tiled ones (tmux window_pane_zindex)
                sort::Order::Z => self.pane_z(wid, *a).cmp(&self.pane_z(wid, *b)),
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
        let wid = self.new_window_id();
        let now = now_secs();
        self.windows.insert(
            wid,
            Window {
                id: wid,
                name,
                layout: Node::Leaf(pane),
                pane_order: vec![pane],
                active: pane,
                last_panes: Vec::new(),
                zoomed: false,
                zoom_pane: None,
                zorder: Vec::new(),
                modal: None,
                modal_last: None,
                last_new_pane: (0, 0),
                float_saved: HashMap::new(),
                options: Options::default(),
                alerts: 0,
                layout_cycle: None,
                old_layout: None,
                created: now,
                activity: now,
                activity_us: now_us(),
                sx,
                sy,
                new_size: None,
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
            // session_create: an unnamed session is named by its id (the next free one)
            None => loop {
                let n = self.next_session_id.to_string();
                if !self.sessions.values().any(|s| s.name == n) { break n; }
                self.next_session_id += 1;
            },
        };
        let sid = self.new_session_id();
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
                created: now,
                activity: now,
                activity_seq: super::activity_seq(),
                activity_us: now_us(),
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
        let _ = ctx;
        // its window's events come first (spawn_window), then session-created
        self.detect_changes(None);
        self.fire_session("session-created", sid);
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
    }

    pub fn select_pane(&mut self, wid: WinId, pane: PaneId) {
        let w = &self.windows[&wid];
        // a modal pane keeps the focus
        if w.active != pane && w.modal.is_none_or(|m| m == pane) {
            // window_push_zoom: the unzoom's events come first; a pane shown above the
            // zoom keeps it
            if self.zoomed_pane(wid).is_some() && !self.pane_visible(wid, pane) {
                self.unzoom_window(wid);
            }
            self.set_active_pane(wid, pane);
        }
        self.mark_window_dirty(wid);
    }

    fn select_dir(&mut self, t: Target, dir: Dir) -> Result<(), String> {
        let wid = t.wid.unwrap();
        let pane = t.pane.unwrap();
        let (mut rects, _) = self.rects(wid);
        // floating panes are found by their position like tiled ones (tmux window_pane_find_*)
        for (p, g) in self.float_rects(wid) {
            rects.push((p, Rect { x: g.x.max(0) as u16, y: g.y.max(0) as u16, w: g.w, h: g.h }));
        }
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

    /// server_client_get_cwd: a command client without a session (the CLI) gives its own
    /// directory, else the target session's, else the client's session's, else home.
    pub fn client_cwd(&self, ctx: &Ctx, sid: Option<SessId>) -> String {
        let client = ctx.client.and_then(|c| self.clients.get(&c));
        if let Some(i) = client.filter(|c| c.session.is_none()).and_then(|c| c.ident.as_ref()).filter(|i| !i.cwd.is_empty()) {
            return i.cwd.clone();
        }
        let session = |s: Option<SessId>| s.and_then(|s| self.sessions.get(&s)).map(|s| s.cwd.clone()).filter(|d| !d.is_empty());
        session(sid).or_else(|| session(client.and_then(|c| c.session))).or_else(procinfo::home_dir).unwrap_or_else(|| "/".into())
    }

    /// spawn_pane's working directory: -c (a format, relative to server_client_get_cwd)
    /// or server_client_get_cwd.
    pub fn spawn_cwd(&self, ctx: &Ctx, sid: Option<SessId>, given: Option<String>) -> String {
        match given {
            Some(d) if std::path::Path::new(&d).is_absolute() => d,
            Some(d) if d.is_empty() => self.client_cwd(ctx, sid),
            Some(d) => format!("{}/{d}", self.client_cwd(ctx, sid)),
            None => self.client_cwd(ctx, sid),
        }
    }

    pub fn split_pane(&mut self, t: Target, horizontal: bool, before: bool, size: Option<u16>, full: bool, cwd: Option<String>, cmd: &[String], env: &[(String, String)]) -> Result<PaneId, String> {
        let wid = t.wid.ok_or("no window")?;
        let target = t.pane.ok_or("no pane")?;
        // window_push_zoom: unzoomed (with its events) before splitting
        self.unzoom_window(wid);
        let area = self.window_area(wid);
        let (rects, _) = self.rects(wid);
        let r = if full { area } else { rects.iter().find(|(p, _)| *p == target).map(|(_, r)| *r).ok_or("pane not visible")? };
        if (horizontal && r.w < 3) || (!horizontal && r.h < 3) {
            return err("no space for a new pane");
        }
        let total = if horizontal { r.w } else { r.h };
        let new_size = size.unwrap_or((total - 1) / 2).clamp(1, total - 2);
        let (w, h) = if horizontal { (new_size, r.h) } else { (r.w, new_size) };
        let cwd = cwd.or_else(|| self.panes.get(&target).map(|p| p.current_path.clone()));
        let new = self.spawn_pane(t.sid, w, h, cmd, cwd.as_deref(), env)?;
        let win = self.windows.get_mut(&wid).unwrap();
        win.add_pane_order(target, new, before, full, false);
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

    /// copy-mode -s: the content comes from `source`, shown in `pane`.
    pub fn enter_copy_mode_from(&mut self, pane: PaneId, source: PaneId) {
        if self.raise_mode(pane, "copy-mode", None) {
            return;
        }
        let Some(p) = self.panes.get(&pane) else { return };
        let size = p.size;
        let Some(src) = self.panes.get(&source) else { return };
        let lines = src.snapshot();
        let (cy, cx) = src.screen().cursor_position();
        let wid = self.window_of(pane);
        let vi = self.opt_window(wid, "mode-keys") == "vi";
        let sid = self.session_of_pane(pane).map(|x| x.0);
        // window_copy_clone_screen: trimmed and rewrapped for another pane
        let mut cm = CopyMode::clone_from(lines, src.size, (cy, cx), size, vi, source != pane);
        cm.set_wrap_search(self.opt_window_on(wid, "wrap-search"));
        cm.set_word_separators(&self.opt_session(sid, "word-separators"));
        cm.from_other = source != pane;
        cm.set_pane_search(&p.track.search);
        cm.set_line_number_option(crate::copymode::LineNumbers::parse(&self.opt_window(wid, "copy-mode-line-numbers")));
        if let Some(p) = self.panes.get_mut(&pane) {
            p.track.unseen_changes = false;
        }
        self.set_pane_mode(pane, PaneMode::Copy(Box::new(cm)));
    }

    pub fn exit_mode(&mut self, pane: PaneId) {
        if let Some(p) = self.panes.get_mut(&pane) {
            // tmux keeps the pane's last search and clears unseen changes with the mode
            if let Some(PaneMode::Copy(cm)) = &p.mode {
                if let Some(s) = cm.last_search() {
                    p.track.search = s.to_string();
                }
            }
            let Some(mode) = p.mode.take() else {
                return;
            };
            // window_pane_reset_mode: the next mode in the stack comes back
            p.mode = p.mode_stack.pop();
            if p.mode.is_none() {
                p.track.unseen_changes = false;
            }
            let next = p.mode.as_ref().map(|m| m.name());
            // unzoom if the mode zoomed the window itself
            if mode.zoomed_window() {
                if let Some(wid) = self.window_of(pane) {
                    self.unzoom_window(wid);
                }
            }
            // window_fire_pane_mode_changed
            self.fire_pane_mode(pane, false, Some(mode.name()), next);
            // copy-mode -k and the other modes' -k: the pane goes with the mode
            if mode.kill_on_exit() {
                self.kill_pane(pane);
                return;
            }
        }
        if let Some(wid) = self.window_of(pane) {
            self.mark_window_dirty(wid);
        }
    }

    /// window_fire_pane_mode_changed: pane-mode-entered or pane-mode-exited, then
    /// pane-mode-changed, with the previous and current mode names.
    fn fire_pane_mode(&mut self, pane: PaneId, entered: bool, previous: Option<&str>, current: Option<&str>) {
        if self.window_of(pane).is_none() {
            return;
        }
        let mut p = self.payload_pane(pane);
        if let Some(c) = current {
            p.str("current_mode", c);
        }
        if let Some(o) = previous {
            p.str("previous_mode", o);
        }
        p.int("mode_entered", entered as i64);
        self.fire(if entered { "pane-mode-entered" } else { "pane-mode-exited" }, p.clone());
        self.fire("pane-mode-changed", p);
    }

    /// window_pane_reset_mode_all.
    pub fn exit_all_modes(&mut self, pane: PaneId) {
        while self.panes.get(&pane).is_some_and(|p| p.mode.is_some()) {
            self.exit_mode(pane);
        }
    }

    // ---- buffers ---------------------------------------------------------------------------

    /// paste_set / paste_add. A named buffer is replaced by a new one (the old one is
    /// freed first, firing paste-buffer-deleted); `append` keeps the old data in front.
    /// tmux appends only to a named buffer: without -b a new automatic one is added.
    pub fn add_buffer(&mut self, name: Option<String>, mut data: Vec<u8>, append: bool) {
        if let Some(n) = &name {
            if let Some(pos) = self.buffers.iter().position(|b| &b.name == n) {
                let old = self.buffers.remove(pos);
                if append {
                    let mut joined = old.data;
                    joined.extend(data);
                    data = joined;
                }
                self.buffer_event(&old.name, true);
                self.buffers.insert(0, Buffer { name: n.clone(), data, automatic: false, created: now_secs() });
                self.buffer_event(n, false);
                return;
            }
        }
        match name {
            None => self.paste_add("buffer", data),
            Some(name) if !data.is_empty() => {
                self.buffers.insert(0, Buffer { name: name.clone(), data, automatic: false, created: now_secs() });
                self.buffer_event(&name, false);
            }
            Some(_) => {}
        }
    }

    /// paste_add: a new automatic buffer named `{prefix}N`, freeing the oldest automatic
    /// buffers over buffer-limit first; paste-buffer-changed fires with the final name.
    pub fn paste_add(&mut self, prefix: &str, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        let limit = self.opt_server_num("buffer-limit").max(1) as usize;
        while self.buffers.iter().filter(|b| b.automatic).count() >= limit {
            let Some(pos) = self.buffers.iter().rposition(|b| b.automatic) else { break };
            let old = self.buffers.remove(pos);
            self.buffer_event(&old.name, true);
        }
        let name = loop {
            let n = format!("{prefix}{}", self.buffer_seq);
            self.buffer_seq += 1;
            if !self.buffers.iter().any(|b| b.name == n) {
                break n;
            }
        };
        self.buffers.insert(0, Buffer { name: name.clone(), data, automatic: true, created: now_secs() });
        self.buffer_event(&name, false);
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
        ctx.skip_after = false;
        ctx.readonly |= self.readonly_context.get() || self.client_is_readonly(ctx.client) || self.client_is_readonly(ctx.reply);
        let previous = self.readonly_context.replace(ctx.readonly);
        // Resolve each alias once; arguments belong to its last command (cmd-parse.y).
        let result = if argv.first().is_some_and(|s| s.starts_with("\0alias:")) {
            let mut argv = argv;
            argv[0] = argv[0].trim_start_matches("\0alias:").to_string();
            self.exec_inner(argv, ctx)
        } else if argv.first().is_some_and(|name| self.command_aliases().iter().any(|a| a.split_once('=').is_some_and(|(n, _)| n == name))) {
            print_parsed_command(argv.into_iter().map(|s| (s, false)).collect(), &self.command_aliases(), true)
                .and_then(|cmds| cmdparse::parse(&cmds.join(" ; ")))
                .map(|mut cmds| { for cmd in &mut cmds { if let Some(name) = cmd.first_mut() { *name = format!("\0alias:{name}"); } } Flow::Inject(cmds, false) })
        } else { self.exec_inner(argv, ctx) };
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
        if full == "display-popup" && self.overlay_waits.len() >= 64 {
            return err("too many waiting overlays");
        }
        let mut args = rest.to_vec();
        let env = if ENV_COMMANDS.contains(&full) { take_env(&mut args) } else { Vec::new() };
        // read-only users (server-access -r) may only look
        if ctx.readonly && !READ_ONLY_OK.contains(&full) {
            return err("client is read-only");
        }
        let o = parse_args(full, &args)?;
        if ctx.readonly {
            self.check_readonly_command(full, &o, ctx)?;
        }
        // cmdq_add_message: each command goes in the message log (show-messages) once
        // the configuration is loaded, by key for a bound command of an attached client
        if self.cfg_finished {
            let words = std::iter::once(full.to_string()).chain(args.iter().cloned())
                .map(|a| match a.strip_prefix(cmdparse::BLOCK_MARK) { Some(b) => (b.to_string(), true), None => (a, false) }).collect();
            let printed = print_parsed_command(words, &[], false).ok().and_then(|v| v.into_iter().next())
                .unwrap_or_else(|| std::iter::once(full).chain(args.iter().map(String::as_str)).collect::<Vec<_>>().join(" "));
            let msg = match ctx.log_client.or(ctx.client).or(ctx.reply).and_then(|c| self.clients.get(&c)) {
                Some(c) if c.attached && c.session.is_some() && ctx.key.is_some() => format!("{} key {}: {printed}", c.name(), ctx.key.as_deref().unwrap_or("")),
                Some(c) => format!("{} command: {printed}", c.name()),
                None => format!("command: {printed}"),
            };
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
            // copy mode works for read-only clients, its commands check themselves
            "send-keys" => o.has('X'),
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
                let (dx, dy): (u16, u16) = default.split_once('x').map(|(a, b)| (a.parse().unwrap_or(80), b.parse().unwrap_or(24))).unwrap_or((80, 24));
                let control = cid.is_some_and(|c| self.is_control(c));
                let detached = o.has('d') || ident_size.is_none() && !control && ctx.client.and_then(|c| self.clients.get(&c)).is_none_or(|c| !c.attached);
                let ident_size = if control { None } else { ident_size };
                // cmd_new_session_exec: -x/-y (or "-" for the client's size) give the
                // default size; an attached client's session gets the terminal's size
                // (less a status line), others default-size with -x/-y over it
                let dim = |f: char, tty: Option<u16>, def: u16, what: &str| -> Result<u16, String> {
                    match o.get(f) {
                        Some("-") => Ok(tty.unwrap_or(def)),
                        Some(v) => options::strtonum(v, 1, u16::MAX as i64).map(|n| n as u16).map_err(|e| format!("{what} {e}")),
                        None => Ok(def),
                    }
                };
                let dsx = dim('x', ident_size.map(|s| s.0), 80, "width")?;
                let dsy = dim('y', ident_size.map(|s| s.1), 24, "height")?;
                let status = self.status_lines(None);
                let (sx, sy) = if !detached && !control {
                    let (c, r) = ident_size.unwrap_or((dx, dy.saturating_add(status)));
                    (c, r.saturating_sub(status))
                } else {
                    let (mut sx, mut sy) = if default.split_once('x').is_some_and(|(a, b)| a.parse::<u16>().is_ok() && b.parse::<u16>().is_ok()) { (dx, dy) } else { (dsx, dsy) };
                    if o.has('x') { sx = dsx; }
                    if o.has('y') { sy = dsy; }
                    (sx, sy)
                };
                let (sx, sy) = (sx.clamp(1, MAX_SIZE), sy.clamp(1, MAX_SIZE));
                // -x/-y also set the session's default-size, as given
                let default_size = (o.has('x') || o.has('y')).then(|| {
                    format!("{}x{}", if o.has('x') { dsx } else { sx }, if o.has('y') { dsy } else { sy })
                });
                let (cols, rows) = (sx, sy.saturating_add(status));
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
                if let (Some(ds), Some(s)) = (default_size, self.sessions.get_mut(&sid)) {
                    s.options.set("default-size", ds);
                }
                if o.has('E') {
                    // -E: no update-environment, only the -e variables
                    if let Some(s) = self.sessions.get_mut(&sid) {
                        s.env = env.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect();
                    }
                }
                if o.has('P') {
                    let fmt = o.get('F').unwrap_or("#{session_name}:");
                    let s = self.expand(self.target_for_session(sid), None, fmt);
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
                    c.set_key_table(table);
                    c.dirty = true;
                    return Ok(Flow::Next);
                }
                // -r toggles read-only and also reverses the -n/-p order; -F is accepted and
                // unused, as in tmux
                if o.has('r') {
                    // only the server's own user may make a read-only client writable
                    let own_user = ctx.client.or(ctx.reply).and_then(|c| self.clients.get(&c))
                        .is_some_and(|c| c.user.as_ref().is_none_or(|u| *u == self.owner));
                    let c = self.clients.get_mut(&cid).unwrap();
                    if c.is_readonly() && !own_user { return err("client is read-only"); }
                    // read-only and ignore-size together (also the read-only of server-access -r)
                    let ro = c.is_readonly();
                    c.readonly = !ro;
                    c.acl_readonly &= !ro;
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
                let old = std::mem::replace(&mut self.sessions.get_mut(&sid).unwrap().name, name.clone());
                self.mark_session_dirty(sid);
                let mut p = self.payload_session(sid);
                p.str("old_name", old).str("new_name", name);
                self.fire("session-renamed", p);
                Ok(Flow::Next)
            }
            "list-sessions" => {
                let fmt = o.get('F').unwrap_or("#{session_name}: #{session_windows} windows (created #{t:session_created})#{?session_grouped, (group ,}#{session_group}#{?session_grouped,),}#{?session_attached, (attached),}");
                let crit = sort::Criteria::from_args(o.get('O'), o.has('r'))?;
                for (i, sid) in self.sorted_sessions(crit).into_iter().enumerate() {
                    let tt = Target { sid: Some(sid), idx: Some(self.sessions[&sid].active), wid: Some(self.sessions[&sid].active_window()), pane: None };
                    let extra = HashMap::from([("line".to_string(), i.to_string()), (super::vars::FORMAT_TYPE.to_string(), "S".to_string())]);
                    self.list_line(ctx, tt, ctx.client, fmt, o.get('f'), extra);
                }
                Ok(Flow::Next)
            }
            "list-clients" => {
                let fmt = o.get('F').unwrap_or("#{client_name}: #{session_name} [#{client_width}x#{client_height} #{client_termname}] #{?#{!=:#{client_uid},#{uid}},[user #{?client_user,#{client_user},#{client_uid},}] ,}#{?client_flags,(,}#{client_flags}#{?client_flags,),}");
                let crit = sort::Criteria::from_args(o.get('O'), o.has('r'))?;
                let only = t.map(|t| self.find_session(ctx, t)).transpose()?;
                // sort_get_clients skips CLIENT_UNATTACHEDFLAGS (suspended or locked clients)
                let mut cids: Vec<ClientId> = self.clients.values().filter(|c| c.attached && !c.suspended).map(|c| c.id).collect();
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
                    // format_defaults gets only the client: no format type
                    let extra = HashMap::from([("line".to_string(), i.to_string()), (super::vars::FORMAT_TYPE.to_string(), String::new())]);
                    self.list_line(ctx, tt, Some(cid), fmt, o.get('f'), extra);
                }
                Ok(Flow::Next)
            }
            "lock-server" | "lock-session" | "lock-client" => {
                let cids: Vec<ClientId> = match cmd {
                    "lock-client" => vec![self.resolve_client(ctx, t)?],
                    "lock-session" => {
                        let sid = self.resolve_session(ctx, t)?;
                        self.clients.values().filter(|c| c.attached && c.session == Some(sid)).map(|c| c.id).collect()
                    }
                    _ => self.clients.values().filter(|c| c.attached).map(|c| c.id).collect(),
                };
                for c in cids { self.lock_input_client(c); }
                Ok(Flow::Next)
            }
            "suspend-client" => {
                let cid = self.resolve_client(ctx, t)?;
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.tty_stop();
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
                if o.has('l') {
                    // tty_clipboard_query: ask the terminal for its clipboard (Ms with "?");
                    // a reply within 5 seconds makes a new buffer
                    if let Some(c) = self.clients.get_mut(&cid).filter(|c| c.attached && c.control == 0 && c.features.has("clipboard")) {
                        if c.clipboard_query.is_none_or(|d| Instant::now() >= d) {
                            c.send(S_OUTPUT, b"\x1b]52;;?\x07");
                            c.clipboard_query = Some(Instant::now() + Duration::from_secs(5));
                        }
                    }
                    return Ok(Flow::Next);
                }
                // -f/-F and -r apply first; -A, -B and -C are for control clients only (checked
                // before their argument)
                if let Some(f) = o.get('F') {
                    self.control_flags(cid, f)?;
                }
                if let Some(f) = o.get('f') {
                    self.control_flags(cid, f)?;
                }
                if let Some(report) = o.get('r') {
                    self.theme_control_report(report);
                }
                if (o.has('A') || o.has('B') || o.has('C')) && !self.is_control(cid) {
                    return err("not a control client");
                }
                if let Some(a) = o.get('A') {
                    self.control_pane_action(cid, a)?;
                    return Ok(Flow::Next);
                }
                if let Some(b) = o.get('B') {
                    self.control_subscribe(cid, b)?;
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
                        self.update_session_size_now(sid);
                    }
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
                let cwd = o.get('c').map(|c| self.expand(self.current(ctx), ctx.client, c));
                let cwd = self.spawn_cwd(ctx, Some(sid), cwd);
                let pane = self.spawn_pane(Some(sid), sx, sy, &command, Some(&cwd), &env)?;
                let name = o.get('n').map(|n| self.expand(self.current(ctx), ctx.client, n));
                let named = if is_empty_pane(&command) { &[][..] } else { &command[..] };
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
                // the winlinks stay current with the windows swapped inside; -d
                // selects the target (and source) index, keeping the window
                if o.has('d') {
                    self.select_window(b.0, b.1);
                    if a.0 != b.0 {
                        self.select_window(a.0, a.1);
                    }
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
                // cmd_find_window_exec: tree mode with a filter made from the match string
                let pat = o.rest.first().ok_or("find-window needs a match string")?.clone();
                let tt = self.resolve_pane(ctx, t)?;
                let pane = tt.pane.ok_or("no pane")?;
                let star = if o.has('r') { "" } else { "*" };
                let suffix = match (o.has('r'), o.has('i')) {
                    (true, true) => "/ri",
                    (true, false) => "/r",
                    (false, true) => "/i",
                    _ => "",
                };
                let (mut c, mut n, mut tf) = (o.has('C'), o.has('N'), o.has('T'));
                if !c && !n && !tf {
                    (c, n, tf) = (true, true, true);
                }
                let content = format!("#{{C{suffix}:{pat}}}");
                let name = format!("#{{m{suffix}:{star}{pat}{star},#{{window_name}}}}");
                let title = format!("#{{m{suffix}:{star}{pat}{star},#{{pane_title}}}}");
                let filter = match (c, n, tf) {
                    (true, true, true) => format!("#{{||:{content},#{{||:{name},{title}}}}}"),
                    (true, true, false) => format!("#{{||:{content},{name}}}"),
                    (true, false, true) => format!("#{{||:{content},{title}}}"),
                    (false, true, true) => format!("#{{||:{name},{title}}}"),
                    (true, false, false) => content,
                    (false, true, false) => name,
                    _ => title,
                };
                if self.raise_mode(pane, "tree-mode", Some(false)) {
                    return Ok(Flow::Next);
                }
                let mut tree = Tree::new(TreeKind::Tree, pane, tt);
                tree.filter = Some(filter);
                self.open_tree(pane, tree, o.has('Z'), ctx);
                Ok(Flow::Next)
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
                    w.zoom_pane = None;
                    w.modal = None;
                    w.modal_last = None;
                    w.last_panes.clear();
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
                // spawn_window without SPAWN_DETACHED selects the respawned window
                if let (true, Some(sid), Some(idx)) = (cmd == "respawn-window", tt.sid, tt.idx) {
                    if self.sessions.get(&sid).is_some_and(|s| s.links.get(&idx) == Some(&wid)) {
                        self.select_window(sid, idx);
                    }
                }
                Ok(Flow::Next)
            }
            "rotate-window" => {
                let wt = self.resolve_window(ctx, t)?;
                let wid = wt.wid.unwrap();
                // window_push_zoom: unzoomed (with its events); -Z zooms it again
                let zoomed = self.unzoom_window(wid);
                let w = self.windows.get_mut(&wid).unwrap();
                let order = w.panes();
                let mut rotated = order.clone();
                if o.has('D') { rotated.rotate_right(1); } else { rotated.rotate_left(1); }
                // Replace via temporary ids so cycles do not overwrite one another.
                for (i, p) in order.iter().enumerate() { w.layout.replace(*p, PaneId::MAX - i as u32); }
                for (i, p) in rotated.iter().enumerate() { w.layout.replace(PaneId::MAX - i as u32, *p); }
                // the pane now in the active pane's place becomes active (TAILQ_NEXT /
                // TAILQ_PREV of the old active pane in the rotated list), unless a modal
                // pane keeps the focus (window_set_active_pane, no raise)
                if let Some(i) = order.iter().position(|p| *p == w.active) {
                    let next = rotated[i];
                    if next != w.active && w.modal.is_none_or(|m| m == next) {
                        w.set_active(next);
                    }
                }
                w.pane_order = rotated;
                self.apply_layout(wid);
                self.mark_window_dirty(wid);
                if zoomed && o.has('Z') {
                    self.detect_changes(Some("rotate-window"));
                    self.rezoom_window(wid, None);
                }
                Ok(Flow::Next)
            }
            "select-layout" | "next-layout" | "previous-layout" => {
                let wt = if cmd == "select-layout" { self.resolve_pane(ctx, t)? } else { self.resolve_window(ctx, t)? };
                let wid = wt.wid.unwrap();
                // server_unzoom_window, with its events, first
                self.unzoom_window(wid);
                let area = self.window_area(wid);
                // args_string_percentage(value, 0, total, total): anything over the
                // available size is an error, and the layout uses its default
                let size_option = |s: &Server, name: &str, total: u16| -> Option<u16> {
                    let v = s.opt_window(Some(wid), name);
                    let n = match v.strip_suffix('%') {
                        Some(pct) => pct.parse::<u32>().ok().filter(|p| *p <= 1000).map(|p| total as u32 * p / 100),
                        None => v.parse::<u32>().ok(),
                    };
                    n.filter(|n| *n <= total as u32).map(|n| n as u16)
                };
                // control clients without new-layouts get (and give) the old format
                let old_layout = self.window_layout(wid, false, ctx.client).unwrap_or_default();
                let w = &self.windows[&wid];
                // presets arrange the tiled panes; floating ones stay as they are
                let all: Vec<(PaneId, Option<crate::layout::FGeom>)> = w.panes().into_iter().map(|p| (p, w.layout.geom(p))).collect();
                let name: Option<String> = if cmd == "next-layout" || o.has('n') {
                    Some(layout::PRESETS[w.layout_cycle.map_or(0, |i| (i + 1) % layout::PRESETS.len())].to_string())
                } else if cmd == "previous-layout" || o.has('p') {
                    Some(layout::PRESETS[w.layout_cycle.map_or(layout::PRESETS.len() - 1, |i| (i + layout::PRESETS.len() - 1) % layout::PRESETS.len())].to_string())
                } else if o.has('E') {
                    let w = self.windows.get_mut(&wid).unwrap();
                    w.old_layout = Some(old_layout);
                    w.layout.spread(wt.pane.unwrap_or(w.active), area);
                    self.apply_layout(wid);
                    self.mark_window_dirty(wid);
                    self.ev.layout_fired.push((wid, 1));
                    return Ok(Flow::Next);
                } else {
                    o.rest.first().cloned().or_else(|| {
                        if o.has('o') { w.old_layout.clone() } else { w.layout_cycle.map(|i| layout::PRESETS[i].to_string()) }
                    })
                };
                match name {
                    Some(name) => match layout::preset_name(&name).filter(|_| !o.has('o')) {
                        Some(preset) => {
                            let horizontal = preset.starts_with("main-horizontal");
                            let dimension = if horizontal { "height" } else { "width" };
                            let total = if horizontal { area.h } else { area.w }.saturating_sub(1);
                            let main = size_option(self, &format!("main-pane-{dimension}"), total);
                            let other = size_option(self, &format!("other-pane-{dimension}"), total);
                            let columns = self.opt_window_num(Some(wid), "tiled-layout-max-columns").max(0) as usize;
                            if let Some(n) = layout::preset_with_floats(preset, &all, area, main, other, columns) {
                                self.windows.get_mut(&wid).unwrap().layout = n;
                            }
                            self.windows.get_mut(&wid).unwrap().layout_cycle = layout::PRESETS.iter().position(|p| *p == preset);
                        }
                        // cmd_select_layout: "cause: layout"
                        None => self.set_layout_string(wid, &name).map_err(|e| format!("{e}: {name}"))?,
                    },
                    None => {
                        // layout_dump into w->old_layout happens whether or not anything changes
                        self.windows.get_mut(&wid).unwrap().old_layout = Some(old_layout);
                        return Ok(Flow::Next);
                    }
                }
                let w = self.windows.get_mut(&wid).unwrap();
                w.old_layout = Some(old_layout);
                self.apply_layout(wid);
                self.mark_window_dirty(wid);
                // layout_set_select and layout_parse fire it too, with panes to lay out
                let tiled = all.iter().filter(|p| p.1.is_none()).count();
                self.ev.layout_fired.push((wid, if tiled > 1 { 2 } else { 1 }));
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
                    self.update_session_size_now(sid);
                    return Ok(Flow::Next);
                }
                w.options.set("window-size", "manual".into());
                w.new_size = None;
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
                self.clamp_floats(wid);
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
                    let extra = HashMap::from([("line".to_string(), n.clone()), (super::vars::FORMAT_TYPE.to_string(), "W".to_string())]);
                    self.list_line(ctx, tt, ctx.client, fmt, o.get('f'), extra);
                }
                Ok(Flow::Next)
            }
            // ---------------------------------------------------------------- panes
            "split-window" | "new-pane" => self.cmd_new_pane(cmd, &o, &env, ctx),
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
                ctx.skip_after = true;
                let tt = self.resolve_pane(ctx, t)?;
                let wid = tt.wid.unwrap();
                let pane = tt.pane.unwrap();
                if o.has('m') || o.has('M') {
                    let last = self.marked.filter(|p| self.panes.contains_key(p));
                    self.marked = if o.has('M') || self.marked == Some(pane) { None } else { Some(pane) };
                    self.mark_all_dirty();
                    self.fire_marked_changed(pane, last);
                    return Ok(Flow::Next);
                }
                if let Some(title) = o.get('T') {
                    let title = self.expand(tt, ctx.client, title);
                    let p = self.panes.get_mut(&pane).unwrap();
                    p.title = title.clone();
                    let mut ev = self.payload_pane(pane);
                    ev.str("new_title", title);
                    self.fire("pane-title-changed", ev);
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
                    let active = self.windows[&wid].active;
                    self.select_dir(tt, *d)?;
                    ctx.pane = Some(self.windows[&wid].active);
                    ctx.skip_after = ctx.pane == Some(active);
                    if o.has('Z') && zoomed {
                        // window_pop_zoom, after window-pane-changed
                        self.flush_active_pane(wid);
                        self.rezoom_window(wid, None);
                    }
                    return Ok(Flow::Next);
                }
                if o.has('l') {
                    let last = self.last_pane_of(wid).ok_or("no last pane")?;
                    self.select_pane(wid, last);
                    ctx.pane = Some(last);
                    return Ok(Flow::Next);
                }
                // the pane becomes active in its window; the session's current window
                // stays (cmd-select-pane.c selects no window)
                if self.windows.get(&wid).is_some_and(|w| w.panes().contains(&pane)) {
                    ctx.skip_after = self.windows[&wid].active == pane;
                    let zoomed = self.windows[&wid].zoomed && !ctx.skip_after;
                    self.select_pane(wid, pane);
                    ctx.pane = Some(pane);
                    if o.has('Z') && zoomed {
                        self.flush_active_pane(wid);
                        self.rezoom_window(wid, None);
                    }
                }
                Ok(Flow::Next)
            }
            "last-pane" => {
                ctx.skip_after = true;
                let wt = self.resolve_window(ctx, t)?;
                let wid = wt.wid.unwrap();
                let last = self.last_pane_of(wid).ok_or("no last pane")?;
                if o.has('d') || o.has('e') {
                    let v = if o.has('d') { "on" } else { "off" };
                    self.panes.get_mut(&last).unwrap().options.set("@input-disabled", v.into());
                    return Ok(Flow::Next);
                }
                let zoomed = self.windows[&wid].zoomed;
                self.select_pane(wid, last);
                ctx.pane = Some(last);
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
                if o.has('M') {
                    if let Some(cid) = ctx.client {
                        // a floating pane under the mouse is resized (or moved by its top border)
                        match ctx.mouse.clone() {
                            Some(m) if self.mouse_pane(&m).is_some_and(|p| self.windows.get(&wid).is_some_and(|w| w.is_floating(p))) => {
                                let p = self.mouse_pane(&m).unwrap();
                                self.start_float_drag(cid, &m, p, false, false);
                            }
                            m => self.start_border_drag(cid, m),
                        }
                    }
                    return Ok(Flow::Next);
                }
                if o.has('Z') {
                    // tmux window_zoom: a zoom of the pane (unless the active pane floats above
                    // the zoom, the pane becomes active) or an unzoom
                    if !self.unzoom(wid) && self.windows[&wid].panes().len() > 1 {
                        let w = &self.windows[&wid];
                        if w.active != pane && !(w.is_floating(w.active) && self.over_zoom(w.active)) {
                            self.set_active_pane(wid, pane);
                        }
                        let w = self.windows.get_mut(&wid).unwrap();
                        w.zoomed = true;
                        w.zoom_pane = (w.active != pane).then_some(pane);
                        self.apply_layout(wid);
                        self.mark_window_dirty(wid);
                    }
                    return Ok(Flow::Next);
                }
                if self.windows[&wid].is_floating(pane) && self.zoomed_pane(wid) != Some(pane) {
                    return self.cmd_resize_float(&o, wid, pane, tt);
                }
                self.unzoom(wid);
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
                // server_unzoom_window first
                self.unzoom_window(wid);
                let before = self.windows[&wid].layout.to_layout_string(area);
                let w = self.windows.get_mut(&wid).unwrap();
                if x.is_some() || y.is_some() {
                    w.layout.set_size(pane, x, y, area);
                }
                for (c, d) in DIRS {
                    if let Some(v) = o.get(c) {
                        // tmux D:: and friends: the flag's value, else the argument, else 1
                        let v = if v.is_empty() { o.rest.first().map_or("1", String::as_str) } else { v };
                        let n: i64 = v.parse().map_err(|_| "adjustment invalid".to_string())?;
                        // a negative adjustment moves the border the other way
                        let d = if n < 0 {
                            match d { Dir::Left => Dir::Right, Dir::Right => Dir::Left, Dir::Up => Dir::Down, Dir::Down => Dir::Up }
                        } else { d };
                        w.layout.resize(pane, d, n.unsigned_abs().min(u16::MAX as u64) as u16, area);
                    }
                }
                let moved = self.windows[&wid].layout.to_layout_string(area) != before;
                self.apply_layout(wid);
                self.mark_window_dirty(wid);
                // as does layout_resize_pane, when there is a cell to resize
                self.ev.layout_fired.push((wid, if moved { 2 } else { 1 }));
                Ok(Flow::Next)
            }
            "swap-pane" => {
                let dst = self.resolve_pane(ctx, t)?;
                let (dwid, dpane) = (dst.wid.unwrap(), dst.pane.unwrap());
                let (swid, spane) = if o.has('U') || o.has('D') {
                    // the previous or next tiled pane
                    if self.windows[&dwid].is_floating(dpane) {
                        return err(format!("cannot swap {} on floating pane", if o.has('D') { "down" } else { "up" }));
                    }
                    let w = &self.windows[&dwid];
                    let panes: Vec<PaneId> = w.panes().into_iter().filter(|p| !w.is_floating(*p)).collect();
                    let i = panes.iter().position(|p| *p == dpane).ok_or("pane not in window")?;
                    let n = panes.len();
                    let other = if o.has('D') { panes[(i + 1) % n] } else { panes[(i + n - 1) % n] };
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
                if self.windows[&swid].modal == Some(spane) || self.windows[&dwid].modal == Some(dpane) {
                    return err("pane is modal");
                }
                // window_push_zoom on both windows (their events first); -Z zooms them
                // again at the end
                let dzoom = self.unzoom_window(dwid);
                let szoom = swid != dwid && self.unzoom_window(swid);
                if spane == dpane {
                    if o.has('Z') && dzoom {
                        self.rezoom_window(dwid, None);
                    }
                    return Ok(Flow::Next);
                }
                self.ev.pane_move_order.extend([spane, dpane]);
                // the panes swap places in the z-order too (a floating cell keeps its depth)
                for wid in if swid == dwid { vec![swid] } else { vec![swid, dwid] } {
                    let w = self.windows.get_mut(&wid).unwrap();
                    let mut z = w.zlist();
                    for p in &mut z {
                        if *p == spane { *p = dpane } else if *p == dpane { *p = spane }
                    }
                    w.zorder = z;
                }
                if swid == dwid {
                    let w = self.windows.get_mut(&dwid).unwrap();
                    w.pane_order = w.panes();
                    w.swap_pane_order(spane, dpane);
                    w.layout.swap(spane, dpane);
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
                    self.apply_layout(swid);
                    self.mark_window_dirty(swid);
                }
                // tmux: the target pane becomes active (the source in the target window), or
                // with -d an active pane that moved is followed by its replacement
                let activate = |srv: &mut Server, wid: WinId, p: PaneId| srv.windows.get_mut(&wid).unwrap().set_active(p);
                if !o.has('d') {
                    activate(self, swid, dpane);
                    if swid != dwid {
                        activate(self, dwid, spane);
                    }
                } else {
                    if self.windows[&swid].active == spane {
                        activate(self, swid, dpane);
                    }
                    if self.windows[&dwid].active == dpane {
                        activate(self, dwid, spane);
                    }
                }
                if swid != dwid {
                    self.windows.get_mut(&swid).unwrap().last_panes.retain(|p| *p != spane);
                    self.windows.get_mut(&dwid).unwrap().last_panes.retain(|p| *p != dpane);
                }
                self.apply_layout(dwid);
                self.mark_window_dirty(dwid);
                if o.has('Z') && (dzoom || szoom) {
                    // window_pop_zoom after the swap's own events
                    self.detect_changes(Some("swap-pane"));
                    for (wid, z) in [(swid, if swid == dwid { dzoom } else { szoom }), (dwid, dzoom && swid != dwid)] {
                        if z {
                            self.rezoom_window(wid, None);
                        }
                    }
                }
                Ok(Flow::Next)
            }
            "break-pane" => {
                let src = self.resolve_pane(ctx, o.get('s'))?;
                let (swid, pane) = (src.wid.unwrap(), src.pane.unwrap());
                if self.windows[&swid].modal == Some(pane) {
                    return err("pane is modal");
                }
                if o.has('W') {
                    return self.cmd_float_pane(&o, swid, pane, src);
                }
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
                // cmd-break-pane.c: server_unzoom_window, window_lost_pane (the new
                // active pane), layout_close_pane, then the new window is attached,
                // laid out and created, and the pane moved
                self.unzoom_window(swid);
                self.detach_pane(swid, pane);
                self.apply_layout(swid);
                self.flush_active_pane(swid);
                self.flush_window_layout(swid);
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
                self.link_now(dsid, idx, nwid);
                self.apply_layout(nwid);
                self.pane_resized_now(pane);
                self.detect_window_created(nwid);
                self.pane_moved_now(pane);
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
                let dst = self.resolve_pane(ctx, t)?;
                let (dwid, dpane) = (dst.wid.unwrap(), dst.pane.unwrap());
                if cmd == "move-pane" {
                    if o.has('M') {
                        // drag the floating pane under the mouse (from a mouse binding)
                        if let (Some(cid), Some(m)) = (ctx.client, ctx.mouse.clone()) {
                            if let Some(p) = self.mouse_pane(&m) {
                                self.start_float_drag(cid, &m, p, true, false);
                            }
                        }
                        return Ok(Flow::Next);
                    }
                    if ['P', 'z', 'X', 'Y', 'U', 'D', 'L', 'R'].into_iter().any(|c| o.has(c)) {
                        return self.cmd_move_float(&o, dwid, dpane, dst);
                    }
                }
                // the source is the marked pane, else the current one
                let src = match o.get('s') {
                    Some(s) => self.resolve_pane(ctx, Some(s))?,
                    None => match self.marked.filter(|m| self.window_of(*m).is_some()) {
                        Some(m) => self.target_for_pane(m),
                        None => self.current(ctx),
                    },
                };
                let (swid, spane) = (src.wid.ok_or("no source pane")?, src.pane.ok_or("no source pane")?);
                if self.windows[&swid].modal == Some(spane) || self.windows[&dwid].modal == Some(dpane) {
                    return err("pane is modal");
                }
                if spane == dpane {
                    // a floating pane joined to itself is tiled again
                    if self.windows[&swid].is_floating(spane) {
                        self.unzoom_window(swid);
                        return self.cmd_tile_pane(&o, swid, spane, src);
                    }
                    return err("source and target panes must be different");
                }
                // server_unzoom_window on both, with their events
                self.unzoom_window(dwid);
                self.unzoom_window(swid);
                if self.windows[&dwid].is_floating(dpane) {
                    return err("size or position can't split a floating pane");
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
                // detach from source window: layout_close_pane, then window_lost_pane
                if self.detach_pane(swid, spane) {
                    self.apply_layout(swid);
                    self.flush_window_layout(swid);
                    if swid != dwid {
                        self.flush_active_pane(swid);
                    }
                } else {
                    self.fire_window("window-layout-changed", swid);
                    // source window becomes empty: unlink it everywhere (sessions, clients and
                    // hooks handled normally) without killing the moved pane
                    self.windows.get_mut(&swid).unwrap().layout = Node::Leaf(PaneId::MAX);
                    while let Some(&(sid, idx)) = self.links_of(swid).first() {
                        self.unlink(sid, idx);
                    }
                    self.windows.remove(&swid);
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
                    dw.set_active(spane);
                }
                self.apply_layout(dwid);
                self.mark_all_dirty();
                // layout_assign_pane resizes the panes; the new active pane and window
                // (without -d the destination window becomes current: session_select),
                // pane-moved, then the layouts of both windows (cmd-join-pane.c)
                for p in self.windows[&dwid].panes() {
                    self.pane_resized_now(p);
                }
                if !o.has('d') {
                    self.flush_active_pane(dwid);
                    if let (Some(sid), Some(idx)) = (dst.sid, dst.idx) {
                        if self.sessions.contains_key(&sid) {
                            self.select_window(sid, idx);
                            self.detect_current_window(sid, false);
                        }
                    }
                }
                self.pane_moved_now(spane);
                if self.windows.contains_key(&swid) {
                    self.layout_changed_now(swid);
                } else {
                    // server_kill_window for the emptied source comes first
                    self.seen_layout_now(dwid);
                    self.detect_changes(Some("join-pane"));
                }
                self.layout_changed_now(dwid);
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
                        // the mode's screen is its copy of the pane (window_copy_get_screen)
                        (Some(PaneMode::Copy(cm)), true) => Some(cm.backing_screen()),
                        _ => None,
                    };
                    // the rows to read: the mode's screen (-M), the normal screen saved under
                    // the alternate screen (-a, no history) or the pane with its history
                    let (total, hist) = if o.has('a') {
                        (p.size.1 as i64, 0)
                    } else if let Some((l, h)) = &mode_lines {
                        (l.len() as i64, *h as i64)
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
                        if let Some((l, _)) = &mode_lines {
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
                            // the line's marks: start of output (O) or prompt (P), wrapped (W)
                            // and extended cells (X); hyperlinks (H) are not kept
                            let (marks, extended) = if mode_lines.is_some() || o.has('a') { (0, false) } else { screen.history_row_flags(i as usize) };
                            let mut f = String::new();
                            if marks & vt100::ROW_START_OUTPUT != 0 { f.push('O'); }
                            if marks & vt100::ROW_START_PROMPT != 0 { f.push('P'); }
                            if wrapped { f.push('W'); }
                            if extended { f.push('X'); }
                            if f.is_empty() { f.push('-'); }
                            out.push_str(&f);
                            out.push(' ');
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
            // ---------------------------------------------------------------- modes
            "copy-mode" => {
                let tt = self.resolve_pane(ctx, t)?;
                let mut pane = tt.pane.unwrap();
                if o.has('q') {
                    self.exit_all_modes(pane);
                    return Ok(Flow::Next);
                }
                if o.has('M') || o.has('S') {
                    // the pane under the mouse, for a client attached to that session
                    let Some(m) = ctx.mouse.clone() else { return Ok(Flow::Next) };
                    let Some(p) = self.mouse_pane(&m) else { return Ok(Flow::Next) };
                    if ctx.client.is_none() || ctx.client.and_then(|c| self.client_session(c)) != m.sid {
                        return Ok(Flow::Next);
                    }
                    pane = p;
                }
                // -s: copy from another pane
                let source = match o.get('s') {
                    Some(s) => self.resolve_pane(ctx, Some(s))?.pane.unwrap_or(pane),
                    None => pane,
                };
                // entered with the mouse, line numbers stay off
                let line_numbers = ctx.mouse.is_none();
                let already = matches!(self.panes.get(&pane).and_then(|p| p.mode.as_ref()), Some(PaneMode::Copy(_)));
                if !already {
                    self.enter_copy_mode_from(pane, source);
                    if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                        cm.set_exit_on_bottom(o.has('e'));
                        cm.set_show_position(!o.has('H'));
                        cm.kill = o.has('k');
                        cm.set_line_numbers(line_numbers);
                    }
                    if o.has('M') {
                        if let (Some(cid), Some(m)) = (ctx.client, ctx.mouse.clone()) {
                            self.start_copy_drag(cid, &m);
                        }
                    }
                } else if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                    cm.set_line_numbers(line_numbers);
                }
                if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                    if o.has('u') {
                        cm.page_up_once();
                    }
                    if o.has('d') && cm.page_down_exit(o.has('e')) {
                        self.exit_mode(pane);
                    }
                }
                if o.has('S') {
                    if let (Some(cid), Some(m)) = (ctx.client, ctx.mouse.clone()) {
                        self.copy_scroll_to_mouse(cid, pane, &m, o.has('e'));
                    }
                }
                // window_copy_pageup/pagedown and the scrollbar drag show an
                // auto-hide scrollbar; just entering copy mode does not
                if o.has('u') || o.has('d') || o.has('S') {
                    self.show_scrollbar(pane);
                }
                if let Some(w) = tt.wid { self.mark_window_dirty(w); }
                Ok(Flow::Next)
            }
            "clock-mode" => {
                let tt = self.resolve_pane(ctx, t)?;
                let pane = tt.pane.unwrap();
                if !self.raise_mode(pane, "clock-mode", None) {
                    self.set_pane_mode(pane, PaneMode::Clock);
                }
                Ok(Flow::Next)
            }
            "choose-tree" | "choose-buffer" | "choose-client" | "customize-mode" => {
                let tt = self.resolve_pane(ctx, t)?;
                let pane = tt.pane.unwrap();
                let kind = match cmd {
                    "choose-buffer" => TreeKind::Buffers,
                    "choose-client" => TreeKind::Clients,
                    "customize-mode" => TreeKind::Options,
                    _ => TreeKind::Tree,
                };
                let order = match o.get('O') {
                    Some(v) => Some(Sort::parse(v).ok_or("invalid sort order")?),
                    None => None,
                };
                // like tmux, there is nothing to choose from without buffers or clients
                if (kind == TreeKind::Buffers && self.buffers.is_empty()) || (kind == TreeKind::Clients && !self.clients.values().any(|c| c.attached)) {
                    return Ok(Flow::Next);
                }
                let name = match kind {
                    TreeKind::Tree => "tree-mode",
                    TreeKind::Buffers => "buffer-mode",
                    TreeKind::Clients => "client-mode",
                    TreeKind::Options => "options-mode",
                };
                if self.raise_mode(pane, name, Some(o.has('k'))) {
                    return Ok(Flow::Next);
                }
                let mut tree = Tree::new(kind, pane, tt);
                if let Some(t) = o.rest.first() {
                    tree.command = t.clone();
                }
                if let Some(f) = o.get('F') {
                    tree.format = f.to_string();
                }
                tree.key_format = o.get('K').map(String::from);
                tree.filter = o.get('f').map(String::from);
                tree.order = order;
                tree.reversed = o.has('r');
                tree.preview = match o.count('N') {
                    0 => Preview::Normal,
                    1 => Preview::Off,
                    _ => Preview::Big,
                };
                tree.collapse = if o.has('s') { Collapse::Sessions } else if o.has('w') { Collapse::Windows } else { Collapse::None };
                tree.squash_groups = !o.has('G');
                tree.hide_this_pane = o.has('h');
                tree.info = o.has('i') && kind == TreeKind::Clients;
                tree.accept = o.has('y');
                tree.kill = o.has('k');
                self.open_tree(pane, tree, o.has('Z'), ctx);
                Ok(Flow::Next)
            }
            "switch-mode" => {
                let tt = self.resolve_pane(ctx, t)?;
                let pane = tt.pane.unwrap();
                if self.raise_mode(pane, "switch-mode", Some(o.has('k'))) {
                    return Ok(Flow::Next);
                }
                let mut sw = Switch::new(pane, o.has('w'), o.get('F').map(String::from), o.rest.first().cloned());
                sw.kill = o.has('k');
                sw.client = self.ctx_mode_client(ctx);
                if o.has('Z') {
                    sw.zoomed = Some(self.zoom_for_mode(pane));
                }
                sw.snapshot(self, self.panes.get(&pane).map_or(0, |p| p.size.0 as usize));
                self.set_pane_mode(pane, PaneMode::Switch(Box::new(sw)));
                Ok(Flow::Next)
            }
            "display-panes" => {
                let tt = self.resolve_pane(ctx, t)?;
                let pane = tt.pane.unwrap();
                let source = match o.get('s') {
                    Some(s) => self.resolve_window(ctx, Some(s))?,
                    None => tt,
                };
                let delay = match o.get('d') {
                    Some(d) => options::strtonum(d, 0, u32::MAX as i64).map_err(|e| format!("delay {e}"))? as u64,
                    None => self.opt_window_num(tt.wid, "display-panes-time").max(0) as u64,
                };
                if self.raise_mode(pane, "panes-mode", Some(o.has('k'))) {
                    return Ok(Flow::Next);
                }
                let mut m = Panes {
                    owner: pane,
                    session: source.sid,
                    source: source.wid.ok_or("no window")?,
                    template: o.rest.first().cloned().unwrap_or_else(|| super::modes::PANES_DEFAULT_TEMPLATE.into()),
                    until: (delay != 0).then(|| Instant::now() + Duration::from_millis(delay)),
                    ignore_keys: o.has('N'),
                    zoomed: None,
                    kill: o.has('k'),
                    client: self.ctx_mode_client(ctx),
                };
                if !o.has('Z') {
                    m.zoomed = Some(self.zoom_for_mode(pane));
                }
                self.set_pane_mode(pane, PaneMode::Panes(Box::new(m)));
                Ok(Flow::Next)
            }
            "display-menu" => {
                let cid = self.resolve_client(ctx, o.get('c'))?;
                let tt = self.resolve_pane(ctx, t).unwrap_or_else(|_| self.current(ctx));
                let start = match o.get('C') {
                    Some("-") => None,
                    Some(v) => Some(options::strtonum(v, 0, u32::MAX as i64).map_err(|e| format!("starting choice {e}"))? as usize),
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
                let Some((x, y)) = self.menu_position(cid, tt, ctx.mouse.as_ref(), o.get('x'), o.get('y'), w, h) else { return Ok(Flow::Next) };
                if let Some(v) = o.get('b') {
                    options::validate("menu-border-lines", Some(v), None).map_err(|e| format!("menu-border-lines {e}"))?;
                }
                let nomouse = ctx.mouse.is_none() && !o.has('M');
                // the menu belongs to the target window and is placed in it (menu_display)
                let Some(wid) = tt.wid.or_else(|| self.client_window(cid)) else { return Ok(Flow::Next) };
                let (wsx, wsy) = self.windows.get(&wid).map_or((0, 0), |w| (w.sx, w.sy));
                let fit = |p: u16, size: u16, total: u16| if size >= total { 0 } else if p + size > total { total - size } else { p };
                let x = fit(x, w, wsx);
                let y = fit(y.saturating_sub(self.client_window_top(cid)), h, wsy);
                let mut menu = Menu::at(title, items, x, y, tt.pane, nomouse, o.has('O'), start);
                menu.look = super::modes::OverlayLook {
                    style: o.get('s').map(String::from),
                    selected_style: o.get('H').map(String::from),
                    border_style: o.get('S').map(String::from),
                    lines: o.get('b').map(String::from),
                };
                self.set_window_menu(wid, Some(menu));
                // cmd_display_menu_exec returns at once (only display-popup waits)
                Ok(Flow::Next)
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
                let tt = self.resolve_pane(ctx, t).unwrap_or_else(|_| self.current(ctx));
                // cmd_display_menu_get_popup_pos: -x/-y are formats (or letters standing
                // for them), y is the bottom row
                let Some((x, y)) = self.overlay_position(cid, tt, ctx.mouse.as_ref(), o.get('x'), o.get('y'), w, h) else { return Ok(Flow::Next) };
                // -B, or border lines (-b or popup-border-lines) none: no border
                let border = !o.has('B') && o.get('b').map(String::from).unwrap_or_else(|| self.opt_window(self.current(ctx).wid, "popup-border-lines")) != "none";
                let (iw, ih) = if border { (w - 2, h - 2) } else { (w, h) };
                // cmd_display_popup_exec: -d, else server_client_get_cwd for the target client
                let cwd = match o.get('d') {
                    Some(d) => self.expand(tt, Some(cid), d),
                    None => self.client_cwd(&Ctx { client: Some(cid), ..Default::default() }, tt.sid),
                };
                let pane = self.spawn_pane(tt.sid, iw.max(1), ih.max(1), &o.rest, Some(&cwd), &env)?;
                let title = o.get('T').map(|s| self.expand(tt, Some(cid), s)).unwrap_or_default();
                let epoch = self.replace_overlay(cid, Some(Overlay::Popup(Popup {
                    pane, x, y, w, h, border, close_on_exit: o.has('E'), close_any_key: o.has('k'), title,
                    look: super::modes::OverlayLook { style: o.get('s').map(String::from), border_style: o.get('S').map(String::from), lines: o.get('b').map(String::from), ..Default::default() },
                    editor: None,
                })))?;
                Ok(Flow::WaitOverlay(cid, epoch))
            }
            // ---------------------------------------------------------------- prompts & messages
            "command-prompt" => {
                let cid = self.resolve_client(ctx, t)?;
                let tt = self.current(&Ctx { client: Some(cid), ..ctx.clone() });
                // -P: the prompt belongs to the pane and is drawn over its last line
                let host = if o.has('P') { ctx.pane.or(tt.pane) } else { None };
                if o.has('P') {
                    if host.is_none_or(|p| self.panes.get(&p).is_none_or(|p| p.prompt.is_some())) {
                        return Ok(Flow::Next);
                    }
                } else if self.clients.get(&cid).is_some_and(|c| c.prompt.is_some()) {
                    return Ok(Flow::Next);
                }
                let template = o.rest.first().cloned().unwrap_or_else(|| "%1".into());
                // -F: the template is a format, expanded now
                let template = if o.has('F') { self.expand(tt, Some(cid), &template) } else { template };
                // -l: one prompt and input taken literally, not split at commas; others
                // get a space after the prompt, except the default ':' (prompts are
                // formats expanded when drawn, inputs are expanded now)
                let (labels, space): (Vec<String>, bool) = match o.get('p') {
                    Some(p) if o.has('l') => (vec![p.to_string()], false),
                    Some(p) => (p.split(',').map(String::from).collect(), true),
                    None if o.rest.is_empty() => (vec![":".into()], false),
                    None => (vec![format!("({})", template.split_whitespace().next().unwrap_or(""))], true),
                };
                let inits: Vec<String> = match o.get('I') {
                    Some(i) if o.has('l') => vec![self.expand(tt, Some(cid), i)],
                    Some(i) => i.split(',').map(|s| self.expand(tt, Some(cid), s)).collect(),
                    None => Vec::new(),
                };
                let prompts: Vec<(String, String)> = labels.into_iter().enumerate()
                    .map(|(i, l)| (if space { format!("{l} ") } else { l }, inits.get(i).cloned().unwrap_or_default())).collect();
                let history_type = match o.get('T') { Some(ty) => input::prompt_type(ty).ok_or_else(|| format!("unknown type: {ty}"))?, None => 0 };
                let kind = if o.has('k') && !o.has('1') && !o.has('N') && !o.has('i') { PromptKind::Key } else { PromptKind::Command };
                // -1, -N, -i, -k and -e exclude each other in this order
                let mut pr = Prompt::new(kind, prompts, template, ctx.pane);
                pr.single_key = o.has('1');
                pr.numeric = o.has('N') && !o.has('1');
                pr.history_type = history_type;
                pr.incremental = o.has('i') && !o.has('1') && !o.has('N');
                if pr.incremental {
                    // the -I text is kept for C-r/C-s, the prompt starts empty
                    pr.last = pr.input.drain(..).collect();
                    pr.cursor = 0;
                }
                pr.bspace_exit = o.has('e') && !o.has('1') && !o.has('N') && !o.has('i') && !o.has('k');
                let id = self.next_prompt_id();
                pr.id = id;
                match host {
                    Some(pane) => {
                        pr.host = Some(pane);
                        self.set_pane_prompt(pane, pr);
                    }
                    None => {
                        if let Some(c) = self.clients.get_mut(&cid) {
                            c.prompt = Some(pr);
                            c.dirty = true;
                        }
                    }
                }
                self.prompt_incremental_start(cid, host);
                // -C (keep updating panes) is how prompts behave here anyway; without -b
                // (or -i) the command waits until the prompt is done
                self.prompt_wait(cid, id, o.has('b') || o.has('i'))
            }
            "confirm-before" => {
                let cid = self.resolve_client(ctx, t)?;
                let command = o.rest.first().ok_or("confirm-before needs a command")?.clone();
                // -c: the confirming key (printable ASCII), -y: Enter confirms too
                let key = match o.get('c') {
                    Some(c) => match c.as_bytes() {
                        [b] if (32..127).contains(b) => *b as char,
                        _ => return err("invalid confirm key"),
                    },
                    None => 'y',
                };
                // the prompt is a format expanded when drawn, followed by a space
                let label = match o.get('p') {
                    Some(p) => format!("{p} "),
                    None => {
                        // tmux names the first command, not the whole command line
                        let name = cmdparse::parse(&command).ok().and_then(|c| c.first().and_then(|c| c.first()).cloned()).map(|n| canonical(&n).map(String::from).unwrap_or(n));
                        format!("Confirm '{}'? ({key}/n) ", name.unwrap_or_else(|| command.clone()))
                    }
                };
                let id = self.open_prompt(cid, PromptKind::Confirm, vec![(label, String::new())], command, ctx.pane, false, true);
                if let Some(pr) = self.clients.get_mut(&cid).and_then(|c| c.prompt.as_mut()) {
                    pr.confirm = (key, o.has('y'));
                }
                self.prompt_wait(cid, id, o.has('b'))
            }
            "display-message" => {
                let tt = self.cmd_find(ctx, t, Find::Pane, FIND_CANFAIL)?;
                // CMD_CLIENT_CANFAIL: a -c that is not found leaves no target client
                let cid = match o.get('c') {
                    Some(c) => self.resolve_client_canfail(ctx, Some(c)),
                    None => ctx.client.filter(|id| self.clients.get(id).is_some_and(|c| c.attached))
                        .or_else(|| self.resolve_client(ctx, None).ok()),
                };
                // the formats use -c (or the current client) if it shows the target session,
                // else the best client of that session, or of any if it has none attached
                let fc = match tt.sid {
                    Some(s) if cid.and_then(|c| self.clients.get(&c)).is_some_and(|c| c.session == Some(s)) => cid,
                    Some(s) => {
                        let attached = |c: &&Client| c.attached && c.session.is_some_and(|x| self.sessions.contains_key(&x));
                        let on_s = self.clients.values().filter(attached).any(|c| c.session == Some(s));
                        self.clients.values().filter(attached).filter(|c| !on_s || c.session == Some(s)).max_by_key(|c| c.activity_seq).map(|c| c.id)
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
                    // the added entries are a sorted tree: the command's, and the copy
                    // mode's (window_copy_formats) while the pane is in copy mode
                    let mut tree: Vec<(String, String)> = extra.iter().map(|(n, v)| (n.clone(), v.clone())).collect();
                    // (format_each gives a callback without a value an empty one)
                    let copying = tt.pane.is_some_and(|p| matches!(self.panes.get(&p).and_then(|p| p.mode.as_ref()), Some(PaneMode::Copy(_))));
                    tree.extend(super::copy::COPY_VARS.iter().filter_map(|n| match crate::format::Vars::get(&vc, n) {
                        Some(v) => Some((n.to_string(), v)),
                        None if copying && super::copy::COPY_CALLBACK_VARS.contains(n) => Some((n.to_string(), String::new())),
                        None => None,
                    }));
                    tree.sort();
                    for (n, v) in tree {
                        ctx.out.push_str(&format!("{n}={v}\n"));
                    }
                    return Ok(Flow::Next);
                }
                if o.has('F') && !o.rest.is_empty() {
                    return err("only one of -F or argument must be given");
                }
                let delay = o.get('d').map(|d| options::strtonum(d, 0, u32::MAX as i64).map_err(|e| format!("delay {e}"))).transpose()?;
                let fmt = match o.rest.first().map(String::as_str).or(o.get('F')) {
                    Some(f) => f,
                    None if o.has('j') => "",
                    None => "[#{session_name}] #{window_index}:#{window_name}, current pane #{pane_index} - (%H:%M %d-%b-%y)",
                };
                // -l: literal, -j: JSON printed back in tmux's form; -C (keep updating the
                // pane) is how messages behave here anyway; -N: keys don't clear it
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
                if ctx.client.is_none() && ctx.reply.is_none() && self.cfg_finished {
                    // no client at all after startup (a hook with nobody attached):
                    // cmdq_error logs it
                    self.add_message(&format!("message: {msg}"));
                } else if o.has('p') || (attached_client.is_none() && ctx.client.is_none() && ctx.reply.is_none()) {
                    // -p, or a command without any client (the configuration, where
                    // cmdq_error reports it as an error); without -p the message needs a
                    // client to show it on, with none tmux shows nothing
                    ctx.out.push_str(&msg);
                    ctx.out.push('\n');
                } else if let Some(c) = attached_client.filter(|c| self.is_control(*c)) {
                    // server_client_print: inside the block of the control client's own
                    // command, else straight to that client
                    if ctx.reply == Some(c) || ctx.client == Some(c) {
                        ctx.out.push_str(&format!("%message {msg}\n"));
                    } else {
                        self.control_write(c, &format!("%message {msg}\n"));
                    }
                } else if let Some(c) = attached_client {
                    self.set_message_for(c, &msg, delay.map(|d| d as u64), o.has('N'));
                }
                Ok(Flow::Next)
            }
            "show-messages" => {
                // -T: the clients' terminals (there is no terminfo description to list,
                // only which terminal each client has), -J: the running jobs
                let mut blank = false;
                if o.has('T') {
                    let only = o.get('t').and_then(|t| self.resolve_client_canfail(ctx, Some(t)));
                    let mut n = 0;
                    for c in self.clients.values().filter(|c| c.attached && c.control == 0 && only.is_none_or(|o| o == c.id)) {
                        let term = c.ident.as_ref().map(|i| i.term.as_str()).unwrap_or("");
                        // tty_term's flags from what the terminal supports: 256 and RGB
                        // colours, margins (DECSLRM), rectfill (DECFRA); the output is
                        // always VT100-like
                        let f = |name: &str, bit: u32| if c.features.has(name) { bit } else { 0 };
                        let flags = f("256", 0x1) | f("margins", 0x4) | f("rectfill", 0x8) | f("RGB", 0x11) | 0x20;
                        ctx.out.push_str(&format!("Terminal {n}: {term} for {}, flags=0x{flags:x}:\n", c.name()));
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
                        ctx.out.push_str(&format!("Job {n}: {} [fd={}, pid={pid}, status=0]\n", j.cmd, j.fd));
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
                let types: Vec<usize> = match o.get('T') {
                    Some(t) => vec![input::prompt_type(t).ok_or_else(|| format!("invalid type: {t}"))?],
                    None => (0..input::PROMPT_TYPES.len()).collect(),
                };
                for ty in types {
                    if cmd == "clear-prompt-history" { self.prompt_history[ty].clear(); continue; }
                    ctx.out.push_str(&format!("History for {}:\n\n", input::PROMPT_TYPES[ty]));
                    for (i, h) in self.prompt_history[ty].iter().enumerate() { ctx.out.push_str(&format!("{}: {h}\n", i + 1)); }
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
                let parsed = keys::parse_key_name(&key);
                // key_string_lookup_string: UserN beyond KEYC_NUSER and unknown names fail
                if parsed.is_none() && (key.contains("User") || sort::key_code(&key) == u64::MAX >> 8) {
                    return err(format!("unknown key: {key}"));
                }
                let kname = parsed.map(|k| keys::key_name(&k)).unwrap_or(key.clone());
                let table = if o.has('n') { "root".to_string() } else { o.get('T').unwrap_or("prefix").to_string() };
                let command = binding_command(&args);
                if command.is_empty() {
                    return err("bind-key needs a command");
                }
                // tmux parses the command when binding: aliases expand now and unknown commands
                // or flags are rejected; list-keys prints the parsed list
                let command = command_list_string(&command, &self.command_aliases())?;
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
                    let cmd = print_command_string(&b.cmd, &aliases, true);
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
                                ("key_command".to_string(), cmd.clone()),
                            ]);
                            self.expand_extra(self.current(ctx), ctx.client, f, vars)
                        }
                        None if notes_only => format!("{prefix} {} {}", pad(key, key_width), if b.note.is_empty() { &cmd } else { &b.note }),
                        None => {
                            let r = if !has_repeat { "" } else if b.repeat { "-r" } else { "  " };
                            format!("bind-key {r} -T {} {} {}", pad(table, table_width), pad(&vis::args_escape(key), key_width), cmd)
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
                        if let Some(bytes) = self.encode_pane_key(pane, &k, keys::encode_key(&k, app)) { self.panes[&pane].send(bytes); }
                    }
                    return Ok(Flow::Next);
                }
                // cmd_send_keys_exec: the target client (-c, CANFAIL, or the current one)
                // must not be read-only unless -X; -N is a count from 1, which with -X (or
                // with no keys) is the mode's repeat count (wme->prefix); -X (and -N with
                // no keys in a mode) needs a mode with commands (only copy mode has them)
                let tc = match o.get('c') {
                    Some(c) => self.resolve_client_canfail(ctx, Some(c)),
                    None => ctx.client.filter(|id| self.clients.get(id).is_some_and(|c| c.attached)).or_else(|| self.resolve_client(ctx, None).ok()),
                };
                if !o.has('X') && tc.is_some() && self.client_is_readonly(tc) {
                    return err("client is read-only");
                }
                let n: usize = match o.get('N') {
                    Some(v) => options::strtonum(&self.expand(tt, ctx.client, v), 1, u32::MAX as i64).map_err(|e| format!("repeat count {e}"))? as usize,
                    None => 1,
                };
                let has_commands = self.panes.get(&pane).is_some_and(|p| matches!(p.mode, Some(PaneMode::Copy(_))));
                let in_any_mode = self.panes.get(&pane).is_some_and(|p| p.mode.is_some());
                if o.has('N') && in_any_mode && (o.has('X') || o.rest.is_empty()) {
                    if !has_commands {
                        return err("not in a mode");
                    }
                    if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                        cm.set_count(n);
                    }
                }
                if o.has('X') && !has_commands {
                    return err("not in a mode");
                }
                if o.has('X') {
                    // cmd_send_keys_exec: only copy and view mode take commands
                    if !matches!(self.panes.get(&pane).and_then(|p| p.mode.as_ref()), Some(PaneMode::Copy(_))) {
                        return err("not in a mode");
                    }
                    let name = o.rest.first().ok_or("send-keys -X needs a command")?.clone();
                    let rest: Vec<String> = o.rest[1..].iter().map(|a| if o.has('F') { self.expand(tt, ctx.client, a) } else { a.clone() }).collect();
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
                    // run once: -N is the mode's repeat count
                    self.copy_mode_command(pane, ctx.client, &name, &rest);
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
                    // the bound commands are the sending client's (cmdq_insert_after)
                    let saved = self.key_origin.replace(ctx.client.or(ctx.reply).unwrap_or(cid));
                    for _ in 0..n {
                        for key in &list {
                            self.handle_key(cid, key.clone(), keys::encode_key(key, app));
                        }
                    }
                    self.key_origin = saved;
                    return Ok(Flow::Next);
                }
                if o.has('R') {
                    if let Some(p) = self.panes.get_mut(&pane) {
                        p.parser.process(b"\x1bc");
                    }
                    self.mark_window_dirty(tt.wid.unwrap());
                }
                if o.rest.is_empty() && (o.has('N') || o.has('R')) {
                    return Ok(Flow::Next);
                }
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
                        // a key the pane cannot represent goes as its name (window_pane_key failed)
                        Some(key) if k.chars().count() > 1 || k == " " => data.extend(self.encode_pane_key(pane, &key, keys::encode_key(&key, app)).unwrap_or_else(|| k.as_bytes().to_vec())),
                        _ => data.extend(k.as_bytes()),
                    }
                }
                let mut all = Vec::new();
                for _ in 0..n {
                    all.extend_from_slice(&data);
                }
                // keys sent to a pane in a mode are interpreted by the mode, one key at a time
                // (cmd_send_keys_inject_string): -H bytes and -l text, or a string that is
                // not a key name, go as one key per character
                let in_mode = self.panes.get(&pane).is_some_and(|p| p.mode.is_some());
                if in_mode {
                    // the target client (cmd_find_current_client) gets the keys: modes
                    // without a key table see none when there is no client (window_pane_key)
                    let tc = self.resolve_client(ctx, None).ok();
                    let copy = matches!(self.panes.get(&pane).and_then(|p| p.mode.as_ref()), Some(PaneMode::Copy(_)));
                    if tc.is_none() && !copy {
                        return Ok(Flow::Next);
                    }
                    let mut names = Vec::new();
                    for k in &o.rest {
                        let k = if o.has('F') { self.expand(tt, ctx.client, k) } else { k.clone() };
                        if o.has('H') {
                            for h in k.split_whitespace() {
                                if let Ok(b) = u8::from_str_radix(h, 16) {
                                    names.push(super::modes::byte_key_name(b));
                                }
                            }
                            continue;
                        }
                        if !o.has('l') {
                            if let Some(key) = keys::parse_key_name(&k) {
                                names.push(keys::key_name(&key));
                                continue;
                            }
                        }
                        names.extend(k.chars().map(|c| if (c as u32) < 0x80 { super::modes::byte_key_name(c as u8) } else { c.to_string() }));
                    }
                    for _ in 0..n {
                        for k in &names {
                            match self.panes.get(&pane) {
                                Some(p) if p.mode.is_some() => self.mode_key(pane, tc.or(ctx.client), k),
                                // the mode has gone: the rest goes to the pane itself
                                Some(p) => {
                                    let app = p.screen().application_cursor();
                                    if let Some(key) = keys::parse_key_name(k) {
                                        p.send(keys::encode_key(&key, app));
                                    }
                                }
                                None => break,
                            }
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
                    let pos = match &name {
                        Some(n) => self.buffers.iter().position(|b| &b.name == n).ok_or_else(|| format!("unknown buffer: {n}"))?,
                        None if self.buffers.is_empty() => return err("no buffer"),
                        None => 0,
                    };
                    if newname.is_empty() {
                        return err("new name is empty");
                    }
                    if self.buffers[pos].name == newname {
                        return Ok(Flow::Next);
                    }
                    // paste_rename frees a buffer that already has the new name
                    let mut pos = pos;
                    if let Some(other) = self.buffers.iter().position(|b| b.name == newname) {
                        let gone = self.buffers.remove(other);
                        self.buffer_event(&gone.name, true);
                        if other < pos {
                            pos -= 1;
                        }
                    }
                    let b = &mut self.buffers[pos];
                    let old = std::mem::replace(&mut b.name, newname.to_string());
                    b.automatic = false;
                    self.buffer_event(&old, true);
                    self.buffer_event(newname, false);
                    return Ok(Flow::Next);
                }
                let data = o.rest.first().ok_or("no data specified")?.clone();
                // tty_set_selection on the target client (CMD_CLIENT_CANFAIL), clip ""
                if o.has('w') {
                    if let Ok(cid) = self.resolve_client(ctx, o.get('t')) { self.set_selection(cid, "", data.as_bytes()); }
                }
                self.add_buffer(name, data.into_bytes(), o.has('a'));
                Ok(Flow::Next)
            }
            "delete-buffer" => {
                match o.get('b') {
                    Some(n) => {
                        let pos = self.buffers.iter().position(|b| b.name == n).ok_or_else(|| format!("unknown buffer: {n}"))?;
                        let old = self.buffers.remove(pos);
                        self.buffer_event(&old.name, true);
                    }
                    None => {
                        if self.buffers.is_empty() {
                            return err("no buffer");
                        }
                        let old = self.buffers.remove(0);
                        self.buffer_event(&old.name, true);
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
                    if let Ok(cid) = self.resolve_client(ctx, o.get('t')) { self.set_selection(cid, "", &data); }
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
                    let old = self.buffers.remove(idx);
                    self.buffer_event(&old.name, true);
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
                let level = if o.has('g') { GLOBAL_ENV } else { self.resolve_session(ctx, t)? };
                let map = if level == GLOBAL_ENV { &mut self.env } else { &mut self.sessions.get_mut(&level).unwrap().env };
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
                let level = if o.has('g') { GLOBAL_ENV } else { self.resolve_session(ctx, t)? };
                let map = if level == GLOBAL_ENV { self.env.clone() } else { self.sessions[&level].env.clone() };
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
                let aliases = self.command_aliases();
                self.cmd_set_hook(&o, ctx, &aliases)
            }
            "show-hooks" => self.cmd_show_hooks(&o, ctx),
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
                            cmdparse::parse_with_env_lines(&text, &cond, &env)
                        };
                        self.cur_file = saved_file;
                        let (cmds, lines) = parsed.map_err(|e| format!("{f}:{e}"))?;
                        // cmd_parse_from_file: the whole file is checked before any of
                        // it runs; an error names the file and line
                        for (c, line) in cmds.iter().zip(&lines) {
                            self.check_parsed(c).map_err(|e| format!("{f}:{line}: {e}"))?;
                        }
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
                let cwd = Some(o.get('c').map(|c| self.expand(tt, ctx.client, c)).unwrap_or_else(|| self.client_cwd(ctx, tt.sid)));
                let flags = format!("{}{}", if o.has('b') { "b" } else { "" }, if o.has('E') { "E" } else { "" });
                // output goes to a pane only with -t (cdata->wp_id), else to the client
                let out_pane = tt.pane.filter(|_| o.has('t'));
                Err(format!("\0job-run\0{command}\0{}\0{flags}\0{}", cwd.unwrap_or_default(), out_pane.map(|p| p.to_string()).unwrap_or_default()))
            }
            "wait-for" => self.cmd_wait_for(&o, ctx),
            "list-commands" => {
                let rank = |n: &str| USAGE.iter().position(|(u, _)| *u == n).unwrap_or(usize::MAX);
                let mut cmds: Vec<_> = COMMANDS.iter().collect();
                cmds.sort_by_key(|(n, ..)| rank(n));
                for (n, a, b, v) in cmds {
                    if o.rest.first().is_some_and(|x| x != n && x != a) {
                        continue;
                    }
                    let alias = if a.is_empty() { String::new() } else { format!(" ({a})") };
                    let (flags, vals) = match USAGE.iter().find(|(u, _)| u == n) {
                        Some((_, usage)) => (format!(" {usage}"), String::new()),
                        None => (if b.is_empty() { String::new() } else { format!(" [-{b}]") }, v.chars().map(|c| format!(" [-{c} arg]")).collect()),
                    };
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
            // grouped sessions are named by the group (or target) and their id
            None => {
                let group = self.sessions[&tsid].group;
                let prefix = group.and_then(|g| self.sessions.values().filter(|x| x.group == Some(g)).min_by_key(|x| x.created).map(|x| x.name.clone()))
                    .unwrap_or_else(|| self.sessions[&tsid].name.clone());
                loop {
                    let n = format!("{prefix}-{}", self.next_session_id);
                    if !self.sessions.values().any(|s| s.name == n) { break n; }
                    self.next_session_id += 1;
                }
            }
        };
        let sid = self.new_session_id();
        let now = now_secs();
        let t = &self.sessions[&tsid];
        let (tcols, trows, tactive) = (t.cols, t.rows, t.active);
        let (sx, sy) = self.windows.get(&t.active_window()).map(|w| (w.sx, w.sy)).unwrap_or((80, 24));
        let session = Session {
            id: sid,
            name,
            links: BTreeMap::new(),
            active: 0,
            last: Vec::new(),
            cols: tcols,
            rows: trows,
            cwd: t.cwd.clone(),
            options: Options::default(),
            env: t.env.clone(),
            created: now,
            activity: now,
            activity_seq: super::activity_seq(),
            activity_us: now_us(),
            last_attached: 0,
            alerts: BTreeMap::new(),
            group: None,
        };
        let cwd = session.cwd.clone();
        self.sessions.insert(sid, session);
        // cmd-new-session.c spawns the session's own first window, then
        // session_group_synchronize_to replaces it with the group's windows: its
        // pane and window are created, linked, unlinked and closed (with no process
        // here: it never runs)
        if let Ok(pane) = self.spawn_pane(Some(sid), sx, sy, &[EMPTY_PANE.to_string()], Some(&cwd), &[]) {
            if let Some(sp) = self.ev.spawned.iter_mut().find(|sp| sp.pane == pane) {
                sp.empty = false;
            }
            let wname = self.window_name_for(&[], Some(sid));
            let wid = self.new_window_obj(wname, pane, sx, sy);
            let idx = self.next_index(sid, None);
            self.sessions.get_mut(&sid).unwrap().active = idx;
            self.link(sid, idx, wid, true);
            self.detect_changes(None);
            self.sessions.get_mut(&sid).unwrap().links.clear();
            if let Some(w) = self.windows.remove(&wid) {
                for p in w.panes() {
                    self.destroy_pane(p);
                }
            }
        }
        let gid = match self.sessions[&tsid].group {
            Some(g) => g,
            None => {
                let g = self.id();
                self.sessions.get_mut(&tsid).unwrap().group = Some(g);
                self.groups.insert(g, self.sessions[&tsid].links.clone());
                g
            }
        };
        let links = self.sessions[&tsid].links.clone();
        let s = self.sessions.get_mut(&sid).unwrap();
        // the lowest window is current (session_select of RB_MIN): no
        // session-window-changed
        s.active = links.keys().next().copied().unwrap_or(tactive);
        s.links = links;
        s.group = Some(gid);
        self.seen_active_now(sid);
        // session_group_add, then every group window linked and the first one unlinked
        self.detect_groups();
        self.detect_group_sync(sid);
        self.detect_changes(None);
        self.fire_session("session-created", sid);
        if print {
            let s = self.expand(self.target_for_session(sid), None, fmt.unwrap_or("#{session_name}:"));
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

    pub(crate) fn command_aliases(&self) -> Vec<String> {
        self.opts_server.array_items("command-alias").into_iter().map(|(_, v)| v).collect()
    }

    /// check_command after cmd_parse_expand_alias: an alias expands first (its last
    /// command takes the arguments) and the commands it gives are checked, not expanded again.
    pub(crate) fn check_parsed(&self, argv: &[String]) -> Result<(), String> {
        let Some(name) = argv.first() else { return Ok(()) };
        let aliases = self.command_aliases();
        let Some(alias) = aliases.iter().find_map(|a| a.split_once('=').filter(|(n, _)| n == name).map(|(_, v)| v)) else {
            return check_command(argv);
        };
        let mut cmds = cmdparse::parse(alias)?;
        if let Some(last) = cmds.last_mut() { last.extend(argv[1..].iter().cloned()); }
        cmds.iter().try_for_each(|c| check_command(c))
    }

    pub(super) fn cmd_set_option(&mut self, window_cmd: bool, o: &Opts, ctx: &Ctx) -> Result<(), String> {
        let Some((name, arg)) = self.option_name(o, ctx, o.rest.first().ok_or("missing argument")?)? else { return Ok(()) };
        let value = o.rest.get(1).map(|v| if o.has('F') { self.expand(self.current(ctx), ctx.client, v) } else { v.clone() });
        if options::hook_scope(&name).is_some() {
            let aliases = self.command_aliases();
            return self.set_hook_option(o, ctx, &name, &arg, value, &aliases);
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
            "status" | "status-position" | "pane-border-status" | "window-size" | "pane-scrollbars" | "pane-scrollbars-style" | "pane-scrollbars-position" => {
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
        self.copy_sync_all();
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

    pub(super) fn cmd_show_options(&mut self, window_cmd: bool, o: &Opts, ctx: &mut Ctx) -> Result<(), String> {
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
        // hooks are array options of their level; -A falls back to the parent levels
        let show_hook = |out: &mut String, level: events::Level, name: &str, named: bool| {
            let base = name.split('[').next().unwrap_or(name);
            let (lvl, parent) = if self.level_holds(level, base) {
                (level, false)
            } else if o.has('A') {
                match self.hook_parents(level).into_iter().find(|l| self.level_holds(*l, base)) {
                    Some(l) => (l, true),
                    None => return,
                }
            } else {
                return;
            };
            let mut lines = Vec::new();
            self.hook_lines(lvl, name, &mut lines);
            for (k, v, _) in lines {
                let empty = v.is_none() && !name.contains('[');
                print(out, &k, v.as_deref(), parent && !(named && empty));
            }
        };
        if let Some((n, _)) = named.as_ref().filter(|_| hook) {
            let level = match self.hook_scope_level(o, ctx, n.split('[').next().unwrap_or(n)) {
                Ok(l) => l,
                Err(_) if o.has('q') => return Ok(()),
                Err(e) => return err(e),
            };
            let mut out = String::new();
            show_hook(&mut out, level, n, true);
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
                // user options of this level first (@ hooks only with -H), then the table in tmux order
                let level = match (scope, global) {
                    (Scope::Server, _) => None,
                    (Scope::Session, true) => Some(events::Level::GlobalSession),
                    (Scope::Session, false) => tt.sid.map(events::Level::Session),
                    (_, true) => Some(events::Level::GlobalWindow),
                    (Scope::Window, false) => tt.wid.map(events::Level::Window),
                    (Scope::Pane, false) => tt.pane.map(events::Level::Pane),
                };
                for (k, v) in store.0.iter().filter(|(k, _)| k.starts_with('@')) {
                    if o.has('H') || level.is_none_or(|l| !self.is_user_hook(l, k)) {
                        print(&mut out, k, Some(v), false);
                    }
                }
                let wanted = |s: Scope| s == scope || (s == Scope::Pane && scope == Scope::Window);
                for d in options::DEFS.iter().filter(|d| wanted(d.scope)) {
                    if let Some((lvl, parent)) = find(d.name) {
                        show(&mut out, lvl, d.name, parent, false);
                    }
                }
                // hooks come last in the table and only show with -H
                if let Some(level) = level.filter(|_| o.has('H')) {
                    for (h, _) in options::HOOKS.iter().filter(|h| wanted(h.1)) {
                        show_hook(&mut out, level, h, false);
                    }
                }
            }
        }
        ctx.out.push_str(&out);
        Ok(())
    }

    /// The attached client a mode acts for (a command-line client is gone by the time a
    /// choice is made).
    fn ctx_mode_client(&self, ctx: &Ctx) -> Option<ClientId> {
        ctx.client.filter(|c| self.clients.get(c).is_some_and(|c| c.attached))
    }

    /// mode_tree_zoom and friends: zoom the window to the pane; whether it was zoomed already.
    pub fn zoom_for_mode(&mut self, pane: PaneId) -> bool {
        let Some(wid) = self.window_of(pane) else { return true };
        let w = self.windows.get_mut(&wid).unwrap();
        if w.zoomed {
            return true;
        }
        if w.panes().len() > 1 {
            w.active = pane;
            w.zoomed = true;
            self.apply_layout(wid);
        }
        false
    }

    /// The start of window_pane_set_mode for a mode named `name`: true when the pane is in
    /// it already, on top (left alone) or further down the stack (raised to the top, with
    /// `kill` as its new -k); a mode that does not stack (panes mode) on top is left first.
    pub fn raise_mode(&mut self, pane: PaneId, name: &str, kill: Option<bool>) -> bool {
        let Some(p) = self.panes.get(&pane) else { return true };
        match &p.mode {
            Some(m) if m.name() == name => return true,
            Some(PaneMode::Panes(_)) => self.exit_mode(pane),
            _ => {}
        }
        let Some(p) = self.panes.get_mut(&pane) else { return true };
        let Some(i) = p.mode_stack.iter().position(|m| m.name() == name) else { return false };
        let mut m = p.mode_stack.remove(i);
        if let Some(k) = kill {
            m.set_kill(k);
        }
        let current = m.name();
        let old = std::mem::replace(&mut p.mode, Some(m));
        let previous = old.as_ref().map(|m| m.name());
        p.mode_stack.extend(old);
        if let Some(wid) = self.window_of(pane) {
            self.mark_window_dirty(wid);
        }
        self.fire_pane_mode(pane, true, previous, Some(current));
        true
    }

    /// Put a pane into a new mode on top of any it is in (window_pane_set_mode).
    pub fn set_pane_mode(&mut self, pane: PaneId, mode: PaneMode) {
        let Some(p) = self.panes.get_mut(&pane) else { return };
        let current = mode.name();
        let old = std::mem::replace(&mut p.mode, Some(mode));
        let previous = old.as_ref().map(|m| m.name());
        p.mode_stack.extend(old);
        if let Some(wid) = self.window_of(pane) {
            self.mark_window_dirty(wid);
        }
        self.fire_pane_mode(pane, true, previous, Some(current));
    }

    pub fn open_tree(&mut self, pane: PaneId, mut tree: Tree, zoom: bool, ctx: &Ctx) {
        tree.client = self.ctx_mode_client(ctx);
        if zoom {
            tree.zoomed = Some(self.zoom_for_mode(pane));
        }
        tree.start(self);
        self.set_pane_mode(pane, PaneMode::Tree(Box::new(tree)));
    }

    /// CMD_RETURN_WAIT for the prompt `id` just opened (unless `background`): the
    /// queue goes on when that prompt is closed.
    fn prompt_wait(&self, cid: ClientId, id: u64, background: bool) -> Result<Flow, String> {
        if !background && self.prompt_open(id) {
            Ok(Flow::WaitPrompt(cid, id))
        } else {
            Ok(Flow::Next)
        }
    }

    /// A new prompt id (see Prompt::id); zero is never used.
    pub fn next_prompt_id(&mut self) -> u64 {
        self.prompt_ids += 1;
        self.prompt_ids
    }

    /// Whether the prompt with this id is still shown, on a client or as a pane prompt.
    pub fn prompt_open(&self, id: u64) -> bool {
        id != 0 && (self.clients.values().any(|c| c.prompt.as_ref().is_some_and(|p| p.id == id))
            || self.panes.values().any(|p| p.prompt.as_ref().is_some_and(|p| p.id == id)))
    }

    /// window_pane_set_prompt: the pane's own prompt, drawn over its last line.
    pub fn set_pane_prompt(&mut self, pane: PaneId, pr: Prompt) {
        let ty = input::PROMPT_TYPES.get(pr.history_type).copied().unwrap_or("command");
        let Some(p) = self.panes.get_mut(&pane) else { return };
        p.prompt = Some(pr);
        if let Some(wid) = self.window_of(pane) {
            self.mark_window_dirty(wid);
        }
        self.fire_pane_prompt("pane-prompt-opened", pane, ty);
    }

    /// Shows a status line prompt on a client; returns its id.
    pub fn open_prompt(&mut self, cid: ClientId, kind: PromptKind, prompts: Vec<(String, String)>, template: String, pane: Option<PaneId>, numeric: bool, single: bool) -> u64 {
        let mut pr = Prompt::new(kind, prompts, template, pane);
        pr.numeric = numeric;
        pr.single_key = single;
        pr.id = self.next_prompt_id();
        let id = pr.id;
        if let Some(c) = self.clients.get_mut(&cid) {
            c.prompt = Some(pr);
            c.dirty = true;
        }
        id
    }
}

/// Commands a read-only (server-access -r) client may run.
const READ_ONLY_OK: &[&str] = &[
    "attach-session", "detach-client", "has-session", "list-sessions", "list-windows", "list-panes", "list-clients", "list-buffers", "list-keys",
    "list-commands", "display-message", "show-options", "show-window-options", "show-environment", "show-hooks", "show-messages", "show-buffer",
    "capture-pane", "refresh-client", "switch-client", "server-access", "copy-mode", "send-keys",
];

/// tmux 3.8's argument counts (args_parse's lower and upper; -1 is no limit).
const ARG_COUNTS: &[(&str, i32, i32)] = &[
    ("attach-session", 0, 0),
    ("bind-key", 1, -1),
    ("break-pane", 0, 0),
    ("capture-pane", 0, 0),
    ("choose-buffer", 0, 1),
    ("choose-client", 0, 1),
    ("choose-tree", 0, 1),
    ("clear-history", 0, 0),
    ("clear-prompt-history", 0, 0),
    ("clock-mode", 0, 0),
    ("command-prompt", 0, 1),
    ("confirm-before", 1, 1),
    ("copy-mode", 0, 0),
    ("customize-mode", 0, 0),
    ("delete-buffer", 0, 0),
    ("detach-client", 0, 0),
    ("display-menu", 1, -1),
    ("display-message", 0, 1),
    ("display-popup", 0, -1),
    ("display-panes", 0, 1),
    ("find-window", 1, 1),
    ("has-session", 0, 0),
    ("if-shell", 2, 3),
    ("join-pane", 0, 0),
    ("kill-pane", 0, 0),
    ("kill-server", 0, 0),
    ("kill-session", 0, 0),
    ("kill-window", 0, 0),
    ("last-pane", 0, 0),
    ("last-window", 0, 0),
    ("link-window", 0, 0),
    ("list-buffers", 0, 0),
    ("list-clients", 0, 0),
    ("list-commands", 0, 1),
    ("list-keys", 0, 1),
    ("list-panes", 0, 0),
    ("list-sessions", 0, 0),
    ("list-windows", 0, 0),
    ("load-buffer", 1, 1),
    ("lock-client", 0, 0),
    ("lock-server", 0, 0),
    ("lock-session", 0, 0),
    ("move-pane", 0, 0),
    ("move-window", 0, 0),
    ("new-pane", 0, -1),
    ("new-session", 0, -1),
    ("new-window", 0, -1),
    ("next-layout", 0, 0),
    ("next-window", 0, 0),
    ("paste-buffer", 0, 0),
    ("pipe-pane", 0, 1),
    ("previous-layout", 0, 0),
    ("previous-window", 0, 0),
    ("refresh-client", 0, 1),
    ("rename-session", 1, 1),
    ("rename-window", 1, 1),
    ("resize-pane", 0, 1),
    ("resize-window", 0, 1),
    ("respawn-pane", 0, -1),
    ("respawn-window", 0, -1),
    ("rotate-window", 0, 0),
    ("run-shell", 0, -1),
    ("save-buffer", 1, 1),
    ("select-layout", 0, 1),
    ("select-pane", 0, 0),
    ("select-window", 0, 0),
    ("send-keys", 0, -1),
    ("send-prefix", 0, 0),
    ("server-access", 0, 1),
    ("set-buffer", 0, 1),
    ("set-environment", 1, 2),
    ("set-hook", 0, 2),
    ("set-option", 1, 2),
    ("set-window-option", 1, 2),
    ("show-buffer", 0, 0),
    ("show-environment", 0, 1),
    ("show-hooks", 0, 1),
    ("show-messages", 0, 0),
    ("show-options", 0, 1),
    ("show-prompt-history", 0, 0),
    ("show-window-options", 0, 1),
    ("source-file", 1, -1),
    ("split-window", 0, -1),
    ("start-server", 0, 0),
    ("suspend-client", 0, 0),
    ("swap-pane", 0, 0),
    ("swap-window", 0, 0),
    ("switch-client", 0, 0),
    ("switch-mode", 0, 1),
    ("unbind-key", 0, 1),
    ("unlink-window", 0, 0),
    ("wait-for", 1, 1),
];

/// tmux 3.8's usage strings, in cmd_table order (list-commands prints them in this order).
const USAGE: &[(&str, &str)] = &[
    ("attach-session", "[-dErx] [-c working-directory] [-f flags] [-t target-session]"),
    ("bind-key", "[-nr] [-T key-table] [-N note] key [command [argument ...]]"),
    ("break-pane", "[-abdPW] [-F format] [-n window-name] [-s src-pane] [-t dst-window] [-x width] [-y height] [-X x-position] [-Y y-position]"),
    ("capture-pane", "[-aCeFHIJLMNpPqRT] [-b buffer-name] [-E end-line] [-S start-line] [-t target-pane]"),
    ("choose-buffer", "[-kNrZ] [-F format] [-f filter] [-K key-format] [-O sort-order] [-t target-pane] [template]"),
    ("choose-client", "[-hikNrZ] [-F format] [-f filter] [-K key-format] [-O sort-order] [-t target-pane] [template]"),
    ("choose-tree", "[-GhkNrswZ] [-F format] [-f filter] [-K key-format] [-O sort-order] [-t target-pane] [template]"),
    ("clear-history", "[-H] [-t target-pane]"),
    ("clear-prompt-history", "[-T prompt-type]"),
    ("clock-mode", "[-t target-pane]"),
    ("command-prompt", "[-1CbeFiklNP] [-I inputs] [-p prompts] [-t target-client] [-T prompt-type] [template]"),
    ("confirm-before", "[-by] [-c confirm-key] [-p prompt] [-t target-client] command"),
    ("copy-mode", "[-dekHMqSu] [-s src-pane] [-t target-pane]"),
    ("customize-mode", "[-kNZ] [-F format] [-f filter] [-t target-pane]"),
    ("delete-buffer", "[-b buffer-name]"),
    ("detach-client", "[-aP] [-E shell-command] [-s target-session] [-t target-client]"),
    ("display-menu", "[-MO] [-b border-lines] [-c target-client] [-C starting-choice] [-H selected-style] [-s style] [-S border-style] [-t target-pane] [-T title] [-x position] [-y position] name [key] [command] ..."),
    ("display-message", "[-aCIjlNpv] [-c target-client] [-d delay] [-F format] [-t target-pane] [message]"),
    ("display-popup", "[-BCEkN] [-b border-lines] [-c target-client] [-d start-directory] [-e environment] [-h height] [-s style] [-S border-style] [-t target-pane] [-T title] [-w width] [-x position] [-y position] [shell-command [argument ...]]"),
    ("display-panes", "[-kNZ] [-d duration] [-s source-window] [-t target-pane] [template]"),
    ("find-window", "[-CiNrTZ] [-t target-pane] match-string"),
    ("has-session", "[-t target-session]"),
    ("if-shell", "[-bF] [-t target-pane] shell-command command [command]"),
    ("join-pane", "[-bdfhv] [-l size] [-s src-pane] [-t dst-pane]"),
    ("kill-pane", "[-a] [-f filter] [-t target-pane]"),
    ("kill-server", ""),
    ("kill-session", "[-aCg] [-f filter] [-t target-session]"),
    ("kill-window", "[-a] [-f filter] [-t target-window]"),
    ("last-pane", "[-deZ] [-t target-window]"),
    ("last-window", "[-t target-session]"),
    ("link-window", "[-abdk] [-s src-window] [-t dst-window]"),
    ("list-buffers", "[-F format] [-f filter] [-O order]"),
    ("list-clients", "[-F format] [-f filter] [-O order][-t target-session]"),
    ("list-commands", "[-F format] [command]"),
    ("list-keys", "[-1aNr] [-F format] [-O order] [-P prefix-string][-T key-table] [key]"),
    ("list-panes", "[-asr] [-F format] [-f filter] [-O order][-t target-window]"),
    ("list-sessions", "[-r] [-F format] [-f filter] [-O order]"),
    ("list-windows", "[-ar] [-F format] [-f filter] [-O order][-t target-session]"),
    ("load-buffer", "[-b buffer-name] [-t target-client] path"),
    ("lock-client", "[-t target-client]"),
    ("lock-server", ""),
    ("lock-session", "[-t target-session]"),
    ("move-pane", "[-bdfhMv] [-D lines] [-l size] [-L columns] [-P position] [-R columns] [-s src-pane] [-t dst-pane] [-U lines] [-X x-position] [-Y y-position] [-z z-index]"),
    ("move-window", "[-abdkr] [-s src-window] [-t dst-window]"),
    ("new-pane", "[-AbCDefhIkKLMOPvWZ] [-B border-lines] [-c start-directory] [-e environment] [-F format] [-l size] [-m message] [-p percentage] [-s style] [-S active-border-style] [-R inactive-border-style] [-T title] [-x width] [-y height] [-X x-position] [-Y y-position] [-t target-pane] [shell-command [argument ...]]"),
    ("new-session", "[-AdDEPX] [-c start-directory] [-e environment] [-F format] [-f flags] [-n window-name] [-s session-name] [-t target-session] [-x width] [-y height] [shell-command [argument ...]]"),
    ("new-window", "[-abdEkPS] [-c start-directory] [-e environment] [-F format] [-n window-name] [-t target-window] [shell-command [argument ...]]"),
    ("next-layout", "[-t target-window]"),
    ("next-window", "[-a] [-t target-session]"),
    ("paste-buffer", "[-dprS] [-s separator] [-b buffer-name] [-t target-pane]"),
    ("pipe-pane", "[-IOo] [-t target-pane] [shell-command]"),
    ("previous-layout", "[-t target-window]"),
    ("previous-window", "[-a] [-t target-session]"),
    ("refresh-client", "[-cDlLRSU] [-A pane:state] [-B name:what:format] [-C XxY] [-f flags] [-r pane:report] [-t target-client] [adjustment]"),
    ("rename-session", "[-t target-session] new-name"),
    ("rename-window", "[-t target-window] new-name"),
    ("resize-pane", "[-MTZ] [-D lines] [-L columns] [-R columns] [-U lines] [-x width] [-y height] [-t target-pane]"),
    ("resize-window", "[-aADLRU] [-x width] [-y height] [-t target-window] [adjustment]"),
    ("respawn-pane", "[-Ek] [-c start-directory] [-e environment] [-t target-pane] [shell-command [argument ...]]"),
    ("respawn-window", "[-Ek] [-c start-directory] [-e environment] [-t target-window] [shell-command [argument ...]]"),
    ("rotate-window", "[-DUZ] [-t target-window]"),
    ("run-shell", "[-bCE] [-c start-directory] [-d delay] [-t target-pane] [shell-command [argument ...]]"),
    ("save-buffer", "[-a] [-b buffer-name] path"),
    ("select-layout", "[-Enop] [-t target-pane] [layout-name]"),
    ("select-pane", "[-DdeLlMmRUZ] [-T title] [-t target-pane]"),
    ("select-window", "[-lnpT] [-t target-window]"),
    ("send-keys", "[-FHKlMRX] [-c target-client] [-N repeat-count] [-t target-pane] [key ...]"),
    ("send-prefix", "[-2] [-t target-pane]"),
    ("server-access", "[-adglrw] [-t target-pane] [user|group]"),
    ("set-buffer", "[-aw] [-b buffer-name] [-n new-buffer-name] [-t target-client] [data]"),
    ("set-environment", "[-Fhgru] [-t target-session] variable [value]"),
    ("set-hook", "[-agpERTuw] [-B name:what:format] [-t target-pane] [hook] [command]"),
    ("set-option", "[-aFgopqsuUw] [-t target-pane] option [value]"),
    ("set-window-option", "[-aFgoqu] [-t target-window] option [value]"),
    ("show-buffer", "[-b buffer-name]"),
    ("show-environment", "[-hgs] [-t target-session] [variable]"),
    ("show-hooks", "[-Bgpw] [-F format] [-t target-pane] [hook]"),
    ("show-messages", "[-JT] [-t target-client]"),
    ("show-options", "[-AgHpqsvw] [-F format] [-t target-pane] [option]"),
    ("show-prompt-history", "[-T prompt-type]"),
    ("show-window-options", "[-gv] [-F format] [-t target-window] [option]"),
    ("source-file", "[-Fnqv] [-t target-pane] path ..."),
    ("split-window", "[-bdefhIklPvWZ] [-B border-lines] [-c start-directory] [-e environment] [-F format] [-l size] [-m message] [-p percentage] [-s style] [-S active-border-style] [-R inactive-border-style] [-T title] [-t target-pane] [shell-command [argument ...]]"),
    ("start-server", ""),
    ("suspend-client", "[-t target-client]"),
    ("swap-pane", "[-dDUZ] [-s src-pane] [-t dst-pane]"),
    ("swap-window", "[-d] [-s src-window] [-t dst-window]"),
    ("switch-client", "[-ElnprZ] [-c target-client] [-t target-session] [-T key-table] [-O order]"),
    ("switch-mode", "[-kswZ] [-F format] [-t target-pane] [command]"),
    ("unbind-key", "[-anq] [-T key-table] key"),
    ("unlink-window", "[-k] [-t target-window]"),
    ("wait-for", "[-ELSUlv] [-F format] [-w waiter] name"),
];

pub fn truthy(s: &str) -> bool {
    !s.is_empty() && s != "0"
}

/// b64_pton: whitespace is skipped, '=' ends the data; None for anything invalid.
pub fn base64_decode(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut padding = false;
    for &b in data {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                padding = true;
                continue;
            }
            b if b.is_ascii_whitespace() => continue,
            _ => return None,
        };
        if padding {
            return None;
        }
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
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
            vt100::Color::Idx(n) | vt100::Color::Palette(n) => (format!("colour{n}"), 0x0100_0000 | n as u32),
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
        assert_eq!(base64_decode(b"aGVs\nbG8="), Some(b"hello".to_vec()));
        assert_eq!(base64_decode(b"YWI"), Some(b"ab".to_vec()));
        assert_eq!(base64_decode(b"a*b"), None);
        assert_eq!(base64_decode(b"YQ==YQ"), None);
        assert!(fnmatch("*.conf", "a.conf") && !fnmatch("a?", "abc"));
        assert_eq!(canonical("neww"), Some("new-window"));
        assert_eq!(canonical("list-k"), Some("list-keys"));
        let mut a: Vec<String> = ["-e", "A=1", "-eB=2", "-d", "cmd"].iter().map(|s| s.to_string()).collect();
        let env = take_env(&mut a);
        assert_eq!(env.len(), 2);
        assert_eq!(a, vec!["-d", "cmd"]);
    }
}
