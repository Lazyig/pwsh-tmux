//! Customize mode (tmux 3.8 window-customize.c) on the mode tree: options, hooks, the
//! environment and key bindings, each changed by running the matching command.

use super::tree::{Builder, Built, ItemData, Tree, TreePrompt};
use super::{expand_as, format_cells, put_cells, put_text, FmtType, ModeAction};
use crate::cells::{Cell, Style};
use crate::cmdparse::quote;
use crate::options::{self, Scope as OScope};
use crate::server::vars::Target;
use crate::server::*;

pub const DEFAULT_FORMAT: &str = concat!(
    "#{?is_option,",
    "#{?option_is_global,,#[reverse](#{option_scope})#[default] }",
    "#[fg=themelightgrey]#[ignore]#{option_value}",
    "#{?option_unit, #{option_unit},}",
    ",",
    "#{?is_environment,",
    "#[fg=themelightgrey]#[ignore]#{environment_value}",
    ",",
    "#{key}",
    "}",
    "}"
);

pub const HELP: &[(&str, &str)] = &[
    ("   Enter, s", "Set %1 value"),
    ("          S", "Set global %1 value"),
    ("          w", "Set window %1 value"),
    ("          d", "Set to default value"),
    ("          D", "Set tagged %1s to default value"),
    ("          u", "Unset an %1"),
    ("          U", "Unset tagged %1s"),
    ("          a", "Change array key"),
    ("          e", "Open %1 value in editor"),
    ("          f", "Enter a filter"),
    ("          C", "Toggle only changed items"),
    ("          v", "Toggle information"),
];

/// Where an option or environment variable is set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scope {
    Server,
    GlobalSession,
    Session(SessId),
    GlobalWindow,
    Window(WinId),
    Pane(PaneId),
    GlobalEnv,
    SessionEnv(SessId),
}

impl Scope {
    fn global(self) -> bool {
        matches!(self, Scope::Server | Scope::GlobalSession | Scope::GlobalWindow | Scope::GlobalEnv)
    }

    /// The set-option (or set-environment) flags for this scope.
    fn flags(self) -> String {
        match self {
            Scope::Server => "-s".into(),
            Scope::GlobalSession | Scope::GlobalEnv => "-g".into(),
            Scope::Session(s) | Scope::SessionEnv(s) => format!("-t '${s}'"),
            Scope::GlobalWindow => "-gw".into(),
            Scope::Window(w) => format!("-w -t '@{w}'"),
            Scope::Pane(p) => format!("-p -t '%{p}'"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// A top-level group.
    Group,
    Option { scope: Scope, name: String, key: Option<String>, hook: bool },
    Env { scope: Scope, name: String },
    /// A binding (part 0) or its Command (1), Note (2) and Repeat (3) lines.
    Key { table: String, key: String, part: u8 },
}

#[derive(Clone, Copy, Debug)]
pub enum Change {
    Reset,
    Unset,
}

pub enum PromptFor {
    Set { item: Item, value_scope: Scope },
    ArrayKey(Item),
    KeyPart(Item),
    Current(Change, Item),
    Tagged(Change),
}

fn scope_text(srv: &Server, s: Scope) -> String {
    match s {
        Scope::Pane(p) => srv.window_of(p).and_then(|w| {
            let base = srv.opt_window_num(Some(w), "pane-base-index").max(0) as usize;
            srv.windows[&w].panes().iter().position(|x| *x == p).map(|i| format!("pane {}", i + base))
        }).unwrap_or_default(),
        Scope::Session(s) | Scope::SessionEnv(s) => srv.sessions.get(&s).map(|s| format!("session {}", s.name)).unwrap_or_default(),
        Scope::Window(w) => srv.links_of(w).first().map(|l| format!("window {}", l.1)).unwrap_or_default(),
        _ => String::new(),
    }
}

/// The option levels of a group, nearest first, as (scope, store).
fn levels<'a>(srv: &'a Server, t: Target, table: OScope) -> Vec<(Scope, &'a options::Options)> {
    let mut v = Vec::new();
    match table {
        OScope::Server => v.push((Scope::Server, &srv.opts_server)),
        OScope::Session => {
            if let Some(s) = t.sid.and_then(|s| srv.sessions.get(&s)) {
                v.push((Scope::Session(s.id), &s.options));
            }
            v.push((Scope::GlobalSession, &srv.opts_session));
        }
        _ => {
            if let Some(p) = t.pane.and_then(|p| srv.panes.get(&p)) {
                v.push((Scope::Pane(p.id), &p.options));
            }
            if let Some(w) = t.wid.and_then(|w| srv.windows.get(&w)) {
                v.push((Scope::Window(w.id), &w.options));
            }
            v.push((Scope::GlobalWindow, &srv.opts_window));
        }
    }
    v
}

fn display(v: &str) -> String {
    v.replace('#', "##")
}

fn value_changed(name: &str, value: &str) -> bool {
    options::def(name).is_none_or(|d| d.default != value)
}

fn escape(v: &str) -> String {
    quote(v)
}

pub fn build(tree: &Tree, srv: &Server, b: &mut Builder, filter: Option<&str>) {
    let t = tree.origin;
    let fixed_t = srv.target_for_pane(tree.owner);
    let t = if t.sid.is_some_and(|s| srv.sessions.contains_key(&s)) && t.pane.is_some_and(|p| srv.panes.contains_key(&p)) { t } else { fixed_t };
    let groups: [(&str, OScope, bool); 5] = [
        ("Server Options", OScope::Server, false),
        ("Session Options", OScope::Session, false),
        ("Window & Pane Options", OScope::Window, false),
        ("Session Hooks", OScope::Session, true),
        ("Window & Pane Hooks", OScope::Window, true),
    ];
    for (title, table, hooks) in groups {
        let top = b.add(None, format!("g{title}"), title.to_string(), None, ItemData::Custom(Item::Group), 0);
        b.nodes[top].no_tag = true;
        let lv = levels(srv, t, table);
        let mut count = 0;
        if hooks {
            let names: Vec<&str> = options::HOOKS.iter().filter(|(_, s)| if table == OScope::Session { *s == OScope::Session } else { *s != OScope::Session }).map(|(n, _)| *n).collect();
            for name in names {
                let session_hooks = t.sid.and_then(|s| srv.sessions.get(&s)).and_then(|s| s.hooks.get(name));
                let (scope, cmds) = match session_hooks {
                    Some(c) if table == OScope::Session => (Scope::Session(t.sid.unwrap()), c.clone()),
                    _ => (if table == OScope::Session { Scope::GlobalSession } else { Scope::GlobalWindow }, srv.hooks.get(name).cloned().unwrap_or_default()),
                };
                count += add_option(tree, srv, b, top, scope, name, None, &cmds.iter().enumerate().map(|(i, c)| (i.to_string(), c.clone())).collect::<Vec<_>>(), true, filter);
            }
        } else {
            // user options first, then the table, by name
            let mut names: Vec<String> = Vec::new();
            for (_, store) in &lv {
                for k in store.0.keys().filter(|k| k.starts_with('@') && !k.contains('[')) {
                    if !names.contains(k) {
                        names.push(k.clone());
                    }
                }
            }
            names.sort();
            let mut table_names: Vec<&str> = options::DEFS.iter().filter(|d| match table {
                OScope::Server => d.scope == OScope::Server,
                OScope::Session => d.scope == OScope::Session,
                _ => d.scope == OScope::Window || d.scope == OScope::Pane,
            }).map(|d| d.name).collect();
            table_names.sort();
            names.extend(table_names.into_iter().map(String::from));
            for name in names {
                let Some((scope, store)) = lv.iter().find(|(_, s)| s.has(&name)).copied() else { continue };
                if options::is_array(&name) {
                    let items: Vec<(String, String)> = store.array_items(&name).into_iter().map(|(k, v)| (k[name.len() + 1..k.len() - 1].to_string(), v)).collect();
                    count += add_option(tree, srv, b, top, scope, &name, None, &items, false, filter);
                } else {
                    let value = store.get(&name).unwrap_or("").to_string();
                    count += add_option(tree, srv, b, top, scope, &name, Some(&value), &[], false, filter);
                }
            }
        }
        if tree.hide_default && count == 0 {
            b.remove(top);
        }
    }
    if !tree.hide_default {
        let session = t.sid.and_then(|s| srv.sessions.get(&s));
        let envs: Vec<(&str, Scope, Option<&BTreeMap<String, Option<String>>>)> =
            vec![("Global Environment", Scope::GlobalEnv, Some(&srv.env)), ("Session Environment", session.map(|s| Scope::SessionEnv(s.id)).unwrap_or(Scope::GlobalEnv), session.map(|s| &s.env))];
        for (title, scope, env) in envs {
            let top = b.add(None, format!("g{title}"), title.to_string(), None, ItemData::Custom(Item::Group), 0);
            b.nodes[top].no_tag = true;
            for (name, value) in env.into_iter().flatten() {
                let mut extra = HashMap::new();
                extra.insert("is_option".to_string(), "0".to_string());
                extra.insert("is_key".to_string(), "0".to_string());
                extra.insert("is_environment".to_string(), "1".to_string());
                extra.insert("environment_is_global".to_string(), if scope == Scope::GlobalEnv { "1" } else { "0" }.to_string());
                extra.insert("environment_scope".to_string(), scope_text(srv, scope));
                extra.insert("environment_name".to_string(), name.clone());
                extra.insert("environment_hidden".to_string(), "0".to_string());
                extra.insert("environment_removed".to_string(), if value.is_none() { "1" } else { "0" }.to_string());
                extra.insert("environment_value".to_string(), display(value.as_deref().unwrap_or("")));
                if let Some(f) = filter {
                    if !crate::server::cmds::truthy(&expand_as(srv, t, FmtType::None, f, extra.clone())) {
                        continue;
                    }
                }
                let (item_name, text) = match value {
                    None => (format!("-{name}"), None),
                    Some(_) => (name.clone(), Some(expand_as(srv, t, FmtType::None, &tree.format, extra))),
                };
                let tag = format!("e{}{name}", if scope == Scope::GlobalEnv { "g" } else { "s" });
                b.add(Some(top), tag, item_name, text, ItemData::Custom(Item::Env { scope, name: name.clone() }), 0);
            }
        }
    }
    let defaults = crate::server::input::default_bindings();
    for (table, keys) in &srv.bindings {
        if keys.is_empty() {
            continue;
        }
        let title = format!("Key Table - {table}");
        let top = b.add(None, format!("k{table}"), title, None, ItemData::Custom(Item::Group), 0);
        b.nodes[top].no_tag = true;
        let mut count = 0;
        for (key, bd) in keys {
            if tree.hide_default && !key_changed(&defaults, table, key, bd) {
                continue;
            }
            let mut extra = HashMap::new();
            extra.insert("is_option".to_string(), "0".to_string());
            extra.insert("is_key".to_string(), "1".to_string());
            extra.insert("is_environment".to_string(), "0".to_string());
            extra.insert("key".to_string(), key.clone());
            if !bd.note.is_empty() && bd.note != "default" {
                extra.insert("key_note".to_string(), bd.note.clone());
            }
            if let Some(f) = filter {
                if !crate::server::cmds::truthy(&expand_as(srv, t, FmtType::None, f, extra.clone())) {
                    continue;
                }
            }
            let name = expand_as(srv, t, FmtType::None, &tree.format, extra);
            let item = |part| ItemData::Custom(Item::Key { table: table.clone(), key: key.clone(), part });
            let child = b.add(Some(top), format!("k{table}\u{1}{key}"), name, None, item(0), 0);
            let note = if bd.note == "default" { "" } else { bd.note.as_str() };
            for (part, label, text) in [(1u8, "Command", display(&bd.cmd)), (2, "Note", display(note)), (3, "Repeat", if bd.repeat { "on" } else { "off" }.to_string())] {
                let m = b.add(Some(child), format!("k{table}\u{1}{key}\u{1}{part}"), label.to_string(), Some(format!("#[fg=themelightgrey]{text}")), item(part), -1);
                b.nodes[m].draw_as_parent = true;
                b.nodes[m].no_tag = true;
            }
            count += 1;
        }
        if tree.hide_default && count == 0 {
            b.remove(top);
        }
    }
}

fn key_changed(defaults: &BTreeMap<String, BTreeMap<String, Binding>>, table: &str, key: &str, bd: &Binding) -> bool {
    match defaults.get(table).and_then(|t| t.get(key)) {
        None => true,
        Some(d) => d.cmd != bd.cmd || d.repeat != bd.repeat || (d.note != bd.note && bd.note != "default"),
    }
}

#[allow(clippy::too_many_arguments)]
fn add_option(tree: &Tree, srv: &Server, b: &mut Builder, top: usize, scope: Scope, name: &str, value: Option<&str>, array: &[(String, String)], hook: bool, filter: Option<&str>) -> usize {
    let global = scope.global();
    if tree.hide_global && global {
        return 0;
    }
    let is_array = value.is_none();
    let changed = if hook {
        !array.is_empty()
    } else if is_array {
        let mut o = options::Options::default();
        o.reset(name);
        let defaults: Vec<(String, String)> = o.array_items(name).into_iter().map(|(k, v)| (k[name.len() + 1..k.len() - 1].to_string(), v)).collect();
        defaults != array
    } else {
        value_changed(name, value.unwrap_or(""))
    };
    if tree.hide_default && !changed {
        return 0;
    }
    let t = srv.target_for_pane(tree.owner);
    let mut extra = HashMap::new();
    extra.insert("is_option".to_string(), "1".to_string());
    extra.insert("is_key".to_string(), "0".to_string());
    extra.insert("is_environment".to_string(), "0".to_string());
    extra.insert("option_name".to_string(), name.to_string());
    extra.insert("option_is_global".to_string(), if global { "1" } else { "0" }.to_string());
    extra.insert("option_is_array".to_string(), if is_array { "1" } else { "0" }.to_string());
    extra.insert("option_is_hook".to_string(), if hook { "1" } else { "0" }.to_string());
    extra.insert("option_is_monitor".to_string(), "0".to_string());
    extra.insert("option_scope".to_string(), scope_text(srv, scope));
    extra.insert("option_unit".to_string(), String::new());
    extra.insert("option_monitor".to_string(), String::new());
    if let Some(v) = value {
        extra.insert("option_value".to_string(), display(v));
    }
    if let Some(f) = filter {
        if !crate::server::cmds::truthy(&expand_as(srv, t, FmtType::None, f, extra.clone())) {
            return 0;
        }
    }
    let text = (!is_array).then(|| expand_as(srv, t, FmtType::None, &tree.format, extra.clone()));
    let item = b.add(Some(top), format!("o{name}"), name.to_string(), text, ItemData::Custom(Item::Option { scope, name: name.to_string(), key: None, hook }), 0);
    let mut count = 1;
    for (k, v) in array {
        let full = format!("{name}[{k}]");
        extra.insert("option_name".to_string(), full.clone());
        extra.insert("option_value".to_string(), display(v));
        let text = expand_as(srv, t, FmtType::None, &tree.format, extra.clone());
        b.add(Some(item), format!("o{full}"), full, Some(text), ItemData::Custom(Item::Option { scope, name: name.to_string(), key: Some(k.clone()), hook }), -1);
        count += 1;
    }
    count
}

/// The current value of an option item (an array element or a plain option).
fn option_value(srv: &Server, scope: Scope, name: &str, key: Option<&str>, hook: bool) -> Option<String> {
    if hook {
        let cmds = match scope {
            Scope::Session(s) => srv.sessions.get(&s)?.hooks.get(name)?,
            _ => srv.hooks.get(name)?,
        };
        return key.and_then(|k| k.parse::<usize>().ok()).and_then(|i| cmds.get(i).cloned());
    }
    let store = match scope {
        Scope::Server => &srv.opts_server,
        Scope::GlobalSession => &srv.opts_session,
        Scope::Session(s) => &srv.sessions.get(&s)?.options,
        Scope::GlobalWindow => &srv.opts_window,
        Scope::Window(w) => &srv.windows.get(&w)?.options,
        Scope::Pane(p) => &srv.panes.get(&p)?.options,
        _ => return None,
    };
    let full = match key {
        Some(k) => format!("{name}[{k}]"),
        None => name.to_string(),
    };
    store.get(&full).map(String::from)
}

fn env_value(srv: &Server, scope: Scope, name: &str) -> Option<String> {
    match scope {
        Scope::SessionEnv(s) => srv.sessions.get(&s)?.env.get(name).cloned().flatten(),
        _ => srv.env.get(name).cloned().flatten(),
    }
}

/// The command that sets an item to `value` (window_customize_set_option and friends).
fn set_command(srv: &Server, item: &Item, value_scope: Scope, value: &str) -> Option<String> {
    match item {
        Item::Option { name, key, hook, .. } => {
            let full = match key {
                Some(k) => format!("{name}[{k}]"),
                None => name.clone(),
            };
            if *hook {
                let flags = if value_scope.global() { "-g".to_string() } else { value_scope.flags() };
                return Some(format!("set-hook {flags} {} {}", escape(&full), escape(value)));
            }
            Some(format!("set-option {} {} {}", value_scope.flags(), escape(&full), escape(value)))
        }
        Item::Env { scope, name } => Some(format!("set-environment {} {} {}", scope.flags(), escape(name), escape(value))),
        Item::Key { table, key, part } => {
            let bd = srv.bindings.get(table)?.get(key)?;
            let note = if bd.note == "default" { "" } else { bd.note.as_str() };
            let (cmd, note) = match part {
                2 => (bd.cmd.clone(), value.to_string()),
                _ => (value.to_string(), note.to_string()),
            };
            let note = if note.is_empty() { String::new() } else { format!("-N {} ", escape(&note)) };
            Some(format!("bind-key {}{note}-T {} {} {}", if bd.repeat { "-r " } else { "" }, escape(table), escape(key), cmd))
        }
        Item::Group => None,
    }
}

fn change_command(srv: &Server, item: &Item, change: Change) -> Option<String> {
    match (item, change) {
        (Item::Option { scope, name, key, hook }, _) if *hook => {
            let full = match key {
                Some(k) => format!("{name}[{k}]"),
                None => name.clone(),
            };
            let flags = if scope.global() { "-g".to_string() } else { scope.flags() };
            Some(format!("set-hook -u {flags} {}", escape(&full)))
        }
        (Item::Option { scope, name, key: Some(k), .. }, _) => Some(format!("set-option -u {} {}", scope.flags(), escape(&format!("{name}[{k}]")))),
        (Item::Option { scope, name, key: None, .. }, Change::Unset) => Some(format!("set-option -u {} {}", scope.flags(), escape(name))),
        (Item::Option { scope, name, key: None, .. }, Change::Reset) => {
            let d = options::def(name)?;
            if options::is_array(name) {
                return Some(format!("set-option -u {} {}", scope.flags(), escape(name)));
            }
            Some(format!("set-option {} {} {}", scope.flags(), escape(name), escape(d.default)))
        }
        (Item::Env { scope, name }, Change::Unset) => Some(format!("set-environment -u {} {}", scope.flags(), escape(name))),
        (Item::Env { .. }, Change::Reset) => None,
        (Item::Key { table, key, .. }, change) => {
            let defaults = crate::server::input::default_bindings();
            match (change, defaults.get(table).and_then(|t| t.get(key))) {
                (Change::Reset, Some(d)) => {
                    let note = if d.note.is_empty() { String::new() } else { format!("-N {} ", escape(&d.note)) };
                    Some(format!("bind-key {}{note}-T {} {} {}", if d.repeat { "-r " } else { "" }, escape(table), escape(key), d.cmd))
                }
                _ => srv.bindings.get(table).and_then(|t| t.get(key)).map(|_| format!("unbind-key -T {} {}", escape(table), escape(key))),
            }
        }
        (Item::Group, _) => None,
    }
}

fn item_of(b: &Built, i: usize) -> Option<Item> {
    match &b.nodes[i].data {
        ItemData::Custom(Item::Group) => None,
        ItemData::Custom(it) => Some(it.clone()),
        _ => None,
    }
}

fn label_of(it: &Item) -> String {
    match it {
        Item::Option { name, key: Some(k), .. } => format!("{name}[{k}]"),
        Item::Option { name, .. } | Item::Env { name, .. } => name.clone(),
        Item::Key { key, .. } => key.clone(),
        Item::Group => String::new(),
    }
}

pub fn key(tree: &mut Tree, name: &str, srv: &Server, b: &mut Built) -> ModeAction {
    let cur = b.lines[tree.cur.min(b.lines.len() - 1)].item;
    let item = item_of(b, cur);
    let set_prompt = |tree: &mut Tree, kind: PromptFor, label: &str, input: &str, single: bool| {
        let mut p = super::prompt::PanePrompt::new(label, input);
        p.single = single;
        tree.prompt = Some((TreePrompt::Customize(kind), p));
    };
    match name {
        "e" => {
            let Some(it) = item else { return ModeAction::None };
            let value = match &it {
                Item::Option { scope, name, key, hook } => {
                    if options::is_array(name) && key.is_none() && !*hook {
                        return ModeAction::None;
                    }
                    option_value(srv, *scope, name, key.as_deref(), *hook)
                }
                Item::Env { scope, name } => env_value(srv, *scope, name),
                Item::Key { table, key, part: 1 } => srv.bindings.get(table).and_then(|t| t.get(key)).map(|b| b.cmd.clone()),
                Item::Key { table, key, part: 2 } => srv.bindings.get(table).and_then(|t| t.get(key)).map(|b| if b.note == "default" { String::new() } else { b.note.clone() }),
                _ => None,
            };
            let Some(value) = value else { return ModeAction::None };
            let scope = match &it {
                Item::Option { scope, .. } => *scope,
                _ => Scope::Server,
            };
            // a placeholder quote() leaves alone stands for the edited text
            if let Some(command) = set_command(srv, &it, scope, "PwshTmuxEditedValue") {
                return ModeAction::Edit { value, command: command.replace("PwshTmuxEditedValue", "'%%'") };
            }
        }
        "a" => {
            if let Some(it @ Item::Option { key: Some(_), .. }) = item {
                set_prompt(tree, PromptFor::ArrayKey(it.clone()), "New key: ", "", false);
            }
        }
        "Enter" | "s" | "w" | "S" | "W" => {
            let key_name = name;
            let Some(it) = item else { return ModeAction::None };
            let (value, scope) = match &it {
                Item::Option { scope, name, key, hook } => {
                    if key.is_none() && (options::is_array(name) || *hook) {
                        return ModeAction::None;
                    }
                    let def = options::def(name).map(|d| d.scope);
                    let owner = srv.target_for_pane(tree.owner);
                    // Enter and s set it where it is set now, S and W globally, w for the window
                    let to = match key_name {
                        "S" | "W" => match def {
                            Some(OScope::Server) => Scope::Server,
                            Some(OScope::Session) => Scope::GlobalSession,
                            None if *hook => if matches!(scope, Scope::Session(_) | Scope::GlobalSession) { Scope::GlobalSession } else { Scope::GlobalWindow },
                            None if matches!(scope, Scope::Server) => Scope::Server,
                            None if matches!(scope, Scope::Session(_) | Scope::GlobalSession) => Scope::GlobalSession,
                            _ => Scope::GlobalWindow,
                        },
                        "w" => match owner.wid {
                            Some(w) if matches!(def, Some(OScope::Pane) | Some(OScope::Window)) => Scope::Window(w),
                            _ => *scope,
                        },
                        _ => *scope,
                    };
                    (option_value(srv, *scope, name, key.as_deref(), *hook).unwrap_or_default(), to)
                }
                Item::Env { scope, name } => {
                    if key_name == "w" {
                        return ModeAction::None;
                    }
                    (env_value(srv, *scope, name).unwrap_or_default(), *scope)
                }
                Item::Key { table, key, part } => {
                    let Some(bd) = srv.bindings.get(table).and_then(|t| t.get(key)) else { return ModeAction::None };
                    match part {
                        3 => {
                            let mut cmd = format!("bind-key {}-T {} {} {}", if bd.repeat { "" } else { "-r " }, escape(table), escape(key), bd.cmd);
                            if !bd.note.is_empty() && bd.note != "default" {
                                cmd = cmd.replacen("bind-key ", &format!("bind-key -N {} ", escape(&bd.note)), 1);
                            }
                            return ModeAction::RunKeep(cmd);
                        }
                        2 => (if bd.note == "default" { String::new() } else { bd.note.clone() }, Scope::Server),
                        _ => (bd.cmd.clone(), Scope::Server),
                    }
                }
                Item::Group => return ModeAction::None,
            };
            let label = format!("({}) ", label_of(&it));
            let kind = match &it {
                Item::Key { .. } => PromptFor::KeyPart(it.clone()),
                _ => PromptFor::Set { item: it.clone(), value_scope: scope },
            };
            set_prompt(tree, kind, &label, &value, false);
        }
        "d" | "u" => {
            let Some(it) = item else { return ModeAction::None };
            let change = if name == "d" { Change::Reset } else { Change::Unset };
            if name == "d" && matches!(&it, Item::Option { key: Some(_), .. } | Item::Env { .. }) {
                return ModeAction::None;
            }
            let label = if name == "d" { format!("Reset {} to default? ", label_of(&it)) } else { format!("Unset {}? ", label_of(&it)) };
            if tree.accept {
                return prompt_done(tree, PromptFor::Current(change, it), "y".into(), srv, b);
            }
            set_prompt(tree, PromptFor::Current(change, it), &label, "", true);
        }
        "D" | "U" => {
            let tagged = b.lines.iter().filter(|l| b.nodes[l.item].tagged).count();
            if tagged == 0 {
                return ModeAction::None;
            }
            let change = if name == "D" { Change::Reset } else { Change::Unset };
            if tree.accept {
                return prompt_done(tree, PromptFor::Tagged(change), "y".into(), srv, b);
            }
            let label = if name == "D" { format!("Reset {tagged} tagged to default? ") } else { format!("Unset {tagged} tagged? ") };
            set_prompt(tree, PromptFor::Tagged(change), &label, "", true);
        }
        "H" => tree.hide_global = !tree.hide_global,
        "C" => tree.hide_default = !tree.hide_default,
        _ => {}
    }
    ModeAction::None
}

pub fn prompt_done(tree: &mut Tree, p: PromptFor, s: String, srv: &Server, b: &mut Built) -> ModeAction {
    let yes = s.eq_ignore_ascii_case("y");
    let cmd = match p {
        PromptFor::Set { item, value_scope } => set_command(srv, &item, value_scope, &s),
        PromptFor::KeyPart(item) => {
            if matches!(item, Item::Key { part: 1 | 0, .. }) && s.is_empty() {
                return ModeAction::None;
            }
            set_command(srv, &item, Scope::Server, &s)
        }
        PromptFor::ArrayKey(Item::Option { scope, name, key: Some(old), hook }) => {
            if s.is_empty() || s == old {
                return ModeAction::None;
            }
            let value = option_value(srv, scope, &name, Some(&old), hook).unwrap_or_default();
            let new = Item::Option { scope, name: name.clone(), key: Some(s.clone()), hook };
            let oldi = Item::Option { scope, name, key: Some(old), hook };
            match (set_command(srv, &new, scope, &value), change_command(srv, &oldi, Change::Unset)) {
                (Some(a), Some(c)) => Some(format!("{a} ; {c}")),
                _ => None,
            }
        }
        PromptFor::ArrayKey(_) => None,
        PromptFor::Current(change, item) if yes => change_command(srv, &item, change),
        PromptFor::Tagged(change) if yes => {
            let cmds: Vec<String> = b.lines.iter().filter(|l| b.nodes[l.item].tagged).filter_map(|l| item_of(b, l.item)).filter_map(|it| change_command(srv, &it, change)).collect();
            (!cmds.is_empty()).then(|| cmds.join(" ; "))
        }
        _ => None,
    };
    let _ = tree;
    match cmd {
        Some(c) => ModeAction::RunKeep(c),
        None => ModeAction::None,
    }
}

/// The information shown for the current item (window_customize_draw).
#[allow(clippy::too_many_arguments)]
pub fn draw(tree: &Tree, srv: &Server, b: &Built, item: usize, grid: &mut [Vec<Cell>], x: usize, y: usize, sx: usize, sy: usize) {
    let Some(it) = item_of(b, item) else { return };
    let mut lines: Vec<String> = Vec::new();
    match &it {
        Item::Option { scope, name, key, hook } => {
            lines.push(if *hook { "This hook doesn't have a description.".into() } else { "This option doesn't have a description.".into() });
            lines.push(String::new());
            let kind = match options::def(name).map(|d| d.scope) {
                None if *hook => match options::hook_scope(name) {
                    Some(OScope::Session) => "session",
                    _ => "window and pane",
                },
                None => "user",
                Some(OScope::Pane) => "window and pane",
                Some(OScope::Window) => "window",
                Some(OScope::Session) => "session",
                Some(OScope::Server) => "server",
            };
            lines.push(format!("This is a {kind} {}.", if *hook { "hook" } else { "option" }));
            if let Some(d) = options::def(name) {
                if let options::Kind::Choice(c) = d.kind {
                    lines.push(format!("Available values are: {}", c.join(", ")));
                }
                if d.kind == options::Kind::Array {
                    lines.push("This is an array option, index 0 is the first.".into());
                }
            }
            lines.push(String::new());
            let value = option_value(srv, *scope, name, key.as_deref(), *hook).unwrap_or_default();
            let st = scope_text(srv, *scope);
            if !(options::is_array(name) && key.is_none()) {
                lines.push(format!("{}: {value}", if scope.global() || st.is_empty() { "Option value".to_string() } else { format!("Option value ({st})") }));
            }
            if let (Some(d), None) = (options::def(name), key) {
                if !options::is_array(name) && d.default != value {
                    lines.push(format!("The default is: {}", d.default));
                }
            }
        }
        Item::Env { scope, name } => {
            lines.push(format!("This is an environment variable in the {} environment.", if *scope == Scope::GlobalEnv { "global" } else { "session" }));
            lines.push(String::new());
            lines.push(format!("Value: {}", env_value(srv, *scope, name).unwrap_or_default()));
        }
        Item::Key { table, key, .. } => {
            let Some(bd) = srv.bindings.get(table).and_then(|t| t.get(key)) else { return };
            let note = if bd.note.is_empty() || bd.note == "default" { "There is no note for this key." } else { bd.note.as_str() };
            lines.push(format!("{note}{}", if note.ends_with('.') { "" } else { "." }));
            lines.push(String::new());
            lines.push(format!("This key is in the {table} table."));
            lines.push(format!("Repeat: {}", if bd.repeat { "on" } else { "off" }));
            lines.push(String::new());
            lines.push(format!("Command: {}", bd.cmd));
            if let Some(d) = crate::server::input::default_bindings().get(table).and_then(|t| t.get(key)) {
                if d.cmd != bd.cmd {
                    lines.push(format!("The default is: {}", d.cmd));
                }
            }
        }
        Item::Group => {}
    }
    let _ = tree;
    let mut row = 0;
    for l in lines {
        // screen_write_text wraps at the width
        let chars: Vec<char> = l.chars().collect();
        if chars.is_empty() {
            row += 1;
        }
        for chunk in chars.chunks(sx.max(1)) {
            if row >= sy {
                return;
            }
            put_text(grid, x, y + row, &chunk.iter().collect::<String>(), Style::default(), x + sx);
            row += 1;
        }
    }
    let _ = (format_cells, put_cells);
}
