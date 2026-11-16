//! Customize mode (tmux 3.8 window-customize.c) on the mode tree: options, hooks, the
//! environment and key bindings, each changed by running the matching command.

use super::tree::{Builder, Built, ItemData, Tree, TreePrompt};
use super::customize_text;
use super::{expand_as, FmtType, ModeAction};
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
    /// A new user option or hook (window_customize_add_option).
    AddOption { scope: Scope, hook: bool },
    /// A new environment variable (window_customize_add_environment).
    AddEnv(Scope),
    /// A new key in a table (window_customize_add_key).
    AddKey(String),
    ArrayKey(Item),
    /// A new element at the end of an array option or hook (the [+] prompt).
    Append(Item),
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

/// The target the mode was opened for while it is valid, else the mode's own pane.
fn mode_target(tree: &Tree, srv: &Server) -> Target {
    let t = tree.origin;
    if t.sid.is_some_and(|s| srv.sessions.contains_key(&s)) && t.pane.is_some_and(|p| srv.panes.contains_key(&p)) { t } else { srv.target_for_pane(tree.owner) }
}

pub fn build(tree: &Tree, srv: &Server, b: &mut Builder, filter: Option<&str>) {
    let t = mode_target(tree, srv);
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
                // hooks are array options of the levels (the nearest that holds one, else
                // the global level)
                let Some(&(global, _)) = lv.last() else { continue };
                let scope = lv.iter().find(|(_, st)| !st.array_items(name).is_empty()).map_or(global, |(sc, _)| *sc);
                count += add_option(tree, srv, b, top, scope, name, None, &hook_items(srv, scope, name), true, filter);
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
        // the key bindings tree is in key code order, as list-keys prints them
        let mut keys: Vec<(&String, &Binding)> = keys.iter().collect();
        keys.sort_by_key(|(k, _)| crate::server::cmds::sort::key_code(k));
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
    let defaults: Vec<(String, String)> = if is_array && !hook {
        let mut o = options::Options::default();
        o.reset(name);
        o.array_items(name).into_iter().map(|(k, v)| (k[name.len() + 1..k.len() - 1].to_string(), v)).collect()
    } else {
        Vec::new()
    };
    let changed = if hook {
        !array.is_empty()
    } else if is_array {
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
        // window_customize_build_array: with C, only elements that differ from the default
        if tree.hide_default && !hook && defaults.iter().any(|(dk, dv)| dk == k && dv == v) {
            continue;
        }
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
        let k = key?;
        return hook_items(srv, scope, name).into_iter().find(|(i, _)| i == k).map(|(_, v)| v);
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

/// The value an option has at a scope, from the nearest level that sets it
/// (options_get), or None when no level does.
fn effective_value(srv: &Server, scope: Scope, name: &str) -> Option<String> {
    let chain: Vec<Scope> = match scope {
        Scope::Pane(p) => {
            let w = srv.window_of(p);
            [Some(Scope::Pane(p)), w.map(Scope::Window), Some(Scope::GlobalWindow)].into_iter().flatten().collect()
        }
        Scope::Window(w) => vec![Scope::Window(w), Scope::GlobalWindow],
        Scope::Session(s) => vec![Scope::Session(s), Scope::GlobalSession],
        s => vec![s],
    };
    chain.into_iter().find_map(|s| option_value(srv, s, name, None, false))
}

/// The values of an array option or hook in key order.
fn array_values(srv: &Server, scope: Scope, name: &str, hook: bool) -> Vec<String> {
    if hook {
        return hook_items(srv, scope, name).into_iter().map(|(_, v)| v).collect();
    }
    store_of(srv, scope).map(|o| o.array_items(name).into_iter().map(|(_, v)| v).collect()).unwrap_or_default()
}

/// A hook's commands at one level, as (array index, command).
fn hook_items(srv: &Server, scope: Scope, name: &str) -> Vec<(String, String)> {
    store_of(srv, scope).map(|o| o.array_items(name).into_iter().map(|(k, v)| (k[name.len() + 1..k.len() - 1].to_string(), v)).collect()).unwrap_or_default()
}

/// The hook level of a customize scope.
fn hook_level(scope: Scope) -> Option<crate::server::events::Level> {
    use crate::server::events::Level;
    Some(match scope {
        Scope::GlobalSession => Level::GlobalSession,
        Scope::Session(s) => Level::Session(s),
        Scope::GlobalWindow => Level::GlobalWindow,
        Scope::Window(w) => Level::Window(w),
        Scope::Pane(p) => Level::Pane(p),
        _ => return None,
    })
}

fn store_of(srv: &Server, scope: Scope) -> Option<&options::Options> {
    Some(match scope {
        Scope::Server => &srv.opts_server,
        Scope::GlobalSession => &srv.opts_session,
        Scope::Session(s) => &srv.sessions.get(&s)?.options,
        Scope::GlobalWindow => &srv.opts_window,
        Scope::Window(w) => &srv.windows.get(&w)?.options,
        Scope::Pane(p) => &srv.panes.get(&p)?.options,
        _ => return None,
    })
}

/// window_customize_set_option's scope for a key: `global` moves session, window and pane
/// items to their global level; otherwise global items move to the mode's session, and
/// window items to its pane (with `pane`) or window.
fn new_scope(tree: &Tree, srv: &Server, item: Scope, global: bool, pane: bool) -> Scope {
    let t = mode_target(tree, srv);
    let local = |s: Scope| -> Scope {
        match (s, pane) {
            (Scope::Session(_) | Scope::GlobalSession, _) => t.sid.map(Scope::Session).unwrap_or(s),
            (_, true) => t.pane.map(Scope::Pane).unwrap_or(s),
            _ => t.wid.map(Scope::Window).unwrap_or(s),
        }
    };
    match (item, global) {
        (Scope::Session(_), true) => Scope::GlobalSession,
        (Scope::Window(_) | Scope::Pane(_), true) => Scope::GlobalWindow,
        (Scope::GlobalSession, false) => local(item),
        (Scope::Window(_) | Scope::Pane(_) | Scope::GlobalWindow, false) => local(item),
        _ => item,
    }
}

/// The scope part of a set prompt (window_customize_scope_text): ", for session NAME",
/// ", for window N", ", for pane N", or ", global" for global levels other than the server.
fn scope_label(srv: &Server, tree: &Tree, scope: Scope, env: bool) -> String {
    let t = mode_target(tree, srv);
    let text = match scope {
        Scope::Pane(p) => srv.window_of(p).and_then(|w| {
            let base = srv.opt_window_num(Some(w), "pane-base-index").max(0) as usize;
            srv.windows[&w].panes().iter().position(|x| *x == p).map(|i| format!("pane {}", i + base))
        }),
        Scope::Session(s) | Scope::SessionEnv(s) => srv.sessions.get(&s).map(|s| format!("session {}", s.name)),
        Scope::Window(w) => t.idx.filter(|_| t.wid == Some(w)).or_else(|| srv.links_of(w).first().map(|l| l.1)).map(|i| format!("window {i}")),
        _ => None,
    };
    match text {
        Some(text) => format!(", for {text}"),
        None if env && scope == Scope::GlobalEnv => ", global".into(),
        None if !env && scope != Scope::Server => ", global".into(),
        None => String::new(),
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
            // the command list is one argument, as window_customize_set_key keeps it whole
            Some(format!("bind-key {}{note}-T {} {} {}", if bd.repeat { "-r " } else { "" }, escape(table), escape(key), escape(&cmd)))
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
                    Some(format!("bind-key {}{note}-T {} {} {}", if d.repeat { "-r " } else { "" }, escape(table), escape(key), escape(&d.cmd)))
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
        tree.set_prompt(TreePrompt::Customize(kind), label, input, single);
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
                return ModeAction::Edit { value: value.into_bytes(), command: command.replace("PwshTmuxEditedValue", "'%%'"), buffer: None };
            }
        }
        "a" => {
            if let Some(it @ Item::Option { key: Some(k), .. }) = &item {
                let label = format!("({}) ", label_of(it));
                set_prompt(tree, PromptFor::ArrayKey(it.clone()), &label, &k.clone(), false);
            }
        }
        "Enter" | "s" if item.is_none() => {
            // a group: add a new item to it (window_customize_add_current)
            let t = mode_target(tree, srv);
            let group = b.nodes[cur].name.clone();
            let (kind, label, input) = match group.as_str() {
                "Server Options" => (PromptFor::AddOption { scope: Scope::Server, hook: false }, "New user option: ".to_string(), "@"),
                "Session Options" | "Session Hooks" => {
                    let Some(sid) = t.sid else { return ModeAction::None };
                    let hook = group.ends_with("Hooks");
                    (PromptFor::AddOption { scope: Scope::Session(sid), hook }, format!("New user {}: ", if hook { "hook" } else { "option" }), "@")
                }
                "Window & Pane Options" | "Window & Pane Hooks" => {
                    let Some(p) = t.pane else { return ModeAction::None };
                    let hook = group.ends_with("Hooks");
                    (PromptFor::AddOption { scope: Scope::Pane(p), hook }, format!("New user {}: ", if hook { "hook" } else { "option" }), "@")
                }
                "Global Environment" => (PromptFor::AddEnv(Scope::GlobalEnv), "New environment: ".to_string(), ""),
                "Session Environment" => {
                    let Some(sid) = t.sid else { return ModeAction::None };
                    (PromptFor::AddEnv(Scope::SessionEnv(sid)), "New environment: ".to_string(), "")
                }
                g => match g.strip_prefix("Key Table - ") {
                    Some(table) => (PromptFor::AddKey(table.to_string()), format!("New key in {table}: "), ""),
                    None => return ModeAction::None,
                },
            };
            set_prompt(tree, kind, &label, input, false);
        }
        "Enter" | "s" | "w" | "S" | "W" => {
            let Some(it) = item else { return ModeAction::None };
            let key_name = name;
            // Enter and s: pane and not global; w: window; S and W: global
            let (global, pane) = match name {
                "S" | "W" => (true, false),
                "w" => (false, false),
                _ => (false, true),
            };
            let (value, kind, label) = match &it {
                Item::Option { scope, name, key, hook } => {
                    let def = options::def(name);
                    let array = *hook || options::is_array(name);
                    // window_customize_set_option: arrays stay where they are, the rest go
                    // to the scope for the key
                    let to = if array { *scope } else { new_scope(tree, srv, *scope, global, pane && def.is_none_or(|d| d.scope == OScope::Pane)) };
                    if !array {
                        if let Some(d) = def {
                            // flags toggle and choices move on without a prompt
                            let current = effective_value(srv, to, name).unwrap_or_else(|| d.default.to_string());
                            let next = match d.kind {
                                options::Kind::Flag => Some(if matches!(current.as_str(), "on" | "1" | "yes") { "off".to_string() } else { "on".to_string() }),
                                options::Kind::Choice(c) => {
                                    let i = c.iter().position(|x| *x == current).map_or(0, |i| (i + 1) % c.len());
                                    Some(c[i].to_string())
                                }
                                _ => None,
                            };
                            if let Some(v) = next {
                                return ModeAction::RunKeep(format!("set-option {} {} {}", to.flags(), escape(name), escape(&v)));
                            }
                        }
                    }
                    let space = scope_label(srv, tree, to, false);
                    let label = match key {
                        Some(k) => format!("({name}[{k}]{space}) "),
                        None if array => format!("({name}[+]{space}) "),
                        None => format!("({name}{space}) "),
                    };
                    // options_to_string: a whole array is its values joined by spaces
                    let value = match key {
                        None if array => array_values(srv, *scope, name, *hook).join(" "),
                        _ => option_value(srv, *scope, name, key.as_deref(), *hook).unwrap_or_default(),
                    };
                    let kind = if key.is_none() && array { PromptFor::Append(it.clone()) } else { PromptFor::Set { item: it.clone(), value_scope: to } };
                    (value, kind, label)
                }
                Item::Env { scope, name } => {
                    if key_name == "w" {
                        return ModeAction::None;
                    }
                    let to = if global { Scope::GlobalEnv } else { *scope };
                    let label = format!("({name}{}) ", scope_label(srv, tree, to, true));
                    let value = env_value(srv, *scope, name).unwrap_or_default();
                    (value, PromptFor::Set { item: Item::Env { scope: to, name: name.clone() }, value_scope: to }, label)
                }
                Item::Key { table, key, part } => {
                    if key_name != "Enter" && key_name != "s" {
                        return ModeAction::None;
                    }
                    let Some(bd) = srv.bindings.get(table).and_then(|t| t.get(key)) else { return ModeAction::None };
                    let value = match part {
                        3 => {
                            let mut cmd = format!("bind-key {}-T {} {} {}", if bd.repeat { "" } else { "-r " }, escape(table), escape(key), escape(&bd.cmd));
                            if !bd.note.is_empty() && bd.note != "default" {
                                cmd = cmd.replacen("bind-key ", &format!("bind-key -N {} ", escape(&bd.note)), 1);
                            }
                            return ModeAction::RunKeep(cmd);
                        }
                        2 => if bd.note == "default" { String::new() } else { bd.note.clone() },
                        _ => bd.cmd.clone(),
                    };
                    (value, PromptFor::KeyPart(it.clone()), format!("({key}) "))
                }
                Item::Group => return ModeAction::None,
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
        // window_customize_set_option_callback: an empty answer changes nothing
        PromptFor::Set { .. } | PromptFor::Append(_) if s.is_empty() => None,
        PromptFor::Set { item, value_scope } => set_command(srv, &item, value_scope, &s),
        PromptFor::Append(Item::Option { scope, name, hook, .. }) => {
            // the first index that is not set
            let used: Vec<u32> = if hook {
                (0..array_values(srv, scope, &name, true).len() as u32).collect()
            } else {
                store_of(srv, scope).map(|o| o.array_items(&name).into_iter().filter_map(|(k, _)| k[name.len() + 1..k.len() - 1].parse().ok()).collect()).unwrap_or_default()
            };
            let idx = (0..).find(|i| !used.contains(i)).unwrap_or(0);
            set_command(srv, &Item::Option { scope, name, key: Some(idx.to_string()), hook }, scope, &s)
        }
        PromptFor::Append(_) => None,
        PromptFor::KeyPart(item) => {
            if matches!(item, Item::Key { part: 1 | 0, .. }) && s.is_empty() {
                return ModeAction::None;
            }
            set_command(srv, &item, Scope::Server, &s)
        }
        PromptFor::AddOption { scope, hook } => {
            if s.is_empty() {
                return ModeAction::None;
            }
            let (name, value) = s.split_once([' ', '\t']).map_or((s.as_str(), ""), |(n, v)| (n, v.trim_start_matches([' ', '\t'])));
            if name.is_empty() || value.is_empty() {
                return message("User option must be @name value");
            }
            if !name.starts_with('@') || name.contains('[') {
                return message(&format!("User {} name must start with @", if hook { "hook" } else { "option" }));
            }
            Some(if hook { format!("set-hook {} {} {}", scope.flags(), escape(name), escape(value)) } else { format!("set-option {} {} {}", scope.flags(), escape(name), escape(value)) })
        }
        PromptFor::AddEnv(scope) => {
            if s.is_empty() {
                return ModeAction::None;
            }
            if let Some(name) = s.strip_prefix('-') {
                if name.is_empty() || name.contains('=') {
                    return message(&format!("Bad environment variable: {s}"));
                }
                Some(format!("set-environment {} -r {}", scope.flags(), escape(name)))
            } else {
                match s.split_once('=') {
                    Some((name, value)) if !name.is_empty() => Some(format!("set-environment {} {} {}", scope.flags(), escape(name), escape(value))),
                    _ => return message("Environment variable must be NAME=value"),
                }
            }
        }
        PromptFor::AddKey(table) => {
            if s.is_empty() {
                return ModeAction::None;
            }
            let (key, command) = s.split_once([' ', '\t']).map_or((s.as_str(), ""), |(k, c)| (k, c.trim_start_matches([' ', '\t'])));
            if key.is_empty() || command.is_empty() {
                return message("Key binding must be key command");
            }
            let Some(k) = crate::keys::parse_key_name(key) else { return message(&format!("Unknown key: {key}")) };
            Some(format!("bind-key -T {} {} {}", escape(&table), escape(&crate::keys::key_name(&k)), escape(command)))
        }
        PromptFor::ArrayKey(Item::Option { scope, name, key: Some(old), hook }) => {
            // a key that is set already is left alone
            if s.is_empty() || s == old || option_value(srv, scope, &name, Some(&s), hook).is_some() {
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

/// status_message_set for a prompt answer the mode cannot use.
fn message(text: &str) -> ModeAction {
    ModeAction::RunKeep(format!("display-message -- {}", escape(&text.replace('#', "##"))))
}

/// screen_write_text and window_customize_write_value over the preview area: text wraps
/// at spaces within the width and stops at the last line.
struct Writer<'a> {
    grid: &'a mut [Vec<Cell>],
    cx: usize,
    cy: usize,
    sx: usize,
    sy: usize,
    x: usize,
    y: usize,
}

impl Writer<'_> {
    /// screen_write_text with the lines left; `more` leaves the cursor after the text.
    fn text(&mut self, more: bool, style: Style, s: &str) -> bool {
        let lines = self.sy.saturating_sub(self.y - self.cy);
        if lines == 0 || self.sx == 0 {
            return false;
        }
        let y0 = self.y;
        let text: Vec<char> = s.chars().collect();
        let w = |c: char| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        let mut left = (self.cx + self.sx).saturating_sub(self.x);
        let mut idx = 0;
        loop {
            let (mut at, mut end) = (0, idx);
            while end < text.len() && text[end] != '\n' && at + w(text[end]) <= left {
                at += w(text[end]);
                end += 1;
            }
            let next = if end == text.len() {
                end
            } else if text[end] == '\n' || text[end] == ' ' {
                end + 1
            } else {
                match (idx + 1..=end).rev().find(|i| text[*i] == ' ') {
                    Some(i) => {
                        end = i;
                        i + 1
                    }
                    None => end,
                }
            };
            for c in &text[idx..end] {
                if let Some(cell) = self.grid.get_mut(self.y).and_then(|r| r.get_mut(self.x)) {
                    *cell = Cell { text: c.to_string(), style };
                }
                self.x += w(*c);
            }
            idx = next;
            if self.y == y0 + lines - 1 || idx == text.len() {
                break;
            }
            self.y += 1;
            self.x = self.cx;
            left = self.sx;
        }
        if (self.y == y0 + lines - 1 && (!more || self.x == self.cx + self.sx)) || idx != text.len() {
            return false;
        }
        if !more || self.x == self.cx + self.sx {
            self.skip();
        }
        true
    }

    /// window_customize_write_value: the label, then the value in light grey.
    fn value(&mut self, label: &str, value: &str) -> bool {
        let lines = self.sy.saturating_sub(self.y - self.cy);
        let y0 = self.y;
        if lines == 0 || !self.text(true, Style::default(), label) || self.y - y0 >= lines {
            return false;
        }
        let grey = Style { fg: crate::format::parse_color("themelightgrey").unwrap_or_default(), ..Style::default() };
        self.text(false, grey, value)
    }

    /// screen_write_cursormove to the start of the next line.
    fn skip(&mut self) {
        self.y += 1;
        self.x = self.cx;
    }

    /// The skip-line checks of window_customize_draw_*: false once on the last line.
    fn skip_line(&mut self) -> bool {
        self.skip();
        self.y < self.cy + self.sy.saturating_sub(1)
    }
}

/// hook_fire_count and hook_fire_time of a hook (hooks_monitor_get_fire_count/time), as
/// the event runner keeps them.
fn hook_fire(srv: &Server, scope: Scope, name: &str) -> (u32, u64) {
    hook_level(scope).and_then(|l| srv.ev.fired.get(&(l, name.to_string())).copied()).unwrap_or((0, 0))
}

/// window_customize_write_hook_fire.
fn write_hook_fire(w: &mut Writer, srv: &Server, scope: Scope, name: &str) -> bool {
    let (count, time) = hook_fire(srv, scope, name);
    if time != 0 {
        let when = crate::format::expand_time("#{t/p:@t}", &HashMap::from([("@t".to_string(), time.to_string())]));
        return w.text(false, Style::default(), &format!("This hook has been fired {count} times, last {when}."));
    }
    w.text(false, Style::default(), &format!("This hook has been fired {count} times."))
}

/// window_customize_draw_option.
fn draw_option(tree: &Tree, srv: &Server, w: &mut Writer, scope: Scope, name: &str, key: Option<&str>, hook: bool) {
    let def = options::def(name);
    let is_hook = hook && !name.starts_with('@');
    let is_user_hook = hook && name.starts_with('@');
    let has_oe = def.is_some() || is_hook;
    let info = customize_text::option_text(name).filter(|_| has_oe);
    let unit = info.map(|i| i.1).filter(|u| !u.is_empty()).map(|u| format!(" {u}")).unwrap_or_default();
    let text = match info {
        Some((t, _)) => t,
        None if is_user_hook => "This hook doesn't have a description.",
        None => "This option doesn't have a description.",
    };
    if !w.text(false, Style::default(), text) || !w.skip_line() {
        return;
    }
    let oscope = if is_hook { options::hook_scope(name) } else { def.map(|d| d.scope) };
    let kind = match oscope {
        _ if !has_oe => "user",
        Some(OScope::Pane) => "window and pane",
        Some(OScope::Window) => "window",
        Some(OScope::Session) => "session",
        _ => "server",
    };
    let ok = if is_user_hook {
        w.text(false, Style::default(), "This is a user hook.")
    } else if is_hook {
        w.text(false, Style::default(), &format!("This is a {kind} hook."))
    } else {
        w.text(false, Style::default(), &format!("This is a {kind} option."))
    };
    if !ok {
        return;
    }
    let is_array = is_hook || def.is_some_and(|d| d.kind == options::Kind::Array);
    if is_array {
        if is_hook {
            if key.is_none() {
                if w.text(false, Style::default(), "This is an array hook.") {
                    write_hook_fire(w, srv, scope, name);
                }
                return;
            }
        } else {
            let t = match key {
                Some(k) => format!("This is an array option, key {k}."),
                None => "This is an array option.".to_string(),
            };
            if !w.text(false, Style::default(), &t) {
                return;
            }
        }
        if key.is_none() {
            return;
        }
    }
    if !w.skip_line() {
        return;
    }
    let value = option_value(srv, scope, name, key, hook).unwrap_or_default();
    let default_value = def.filter(|_| key.is_none() && !is_array).map(|d| d.default).filter(|d| *d != value);
    if hook {
        if !w.value("Hook command: ", &format!("{value}{unit}")) || !write_hook_fire(w, srv, scope, name) {
            return;
        }
    } else if !w.value("Option value: ", &format!("{value}{unit}")) {
        return;
    }
    let t = mode_target(tree, srv);
    // format_expand of a string option (or a user option) with the mode's target
    if !has_oe || customize_text::STRING_OPTIONS.contains(&name) {
        let expanded = srv.expand_extra(t, None, &value, HashMap::new());
        if expanded != value && !w.value("This expands to: ", &expanded) {
            return;
        }
    }
    if let Some(options::Kind::Choice(c)) = def.map(|d| d.kind) {
        if !w.value("Available values are: ", &c.join(", ")) {
            return;
        }
    }
    let example = |w: &mut Writer, label: &str, st: Style| w.text(true, Style::default(), label) && w.text(false, st, "EXAMPLE");
    if customize_text::COLOUR_OPTIONS.contains(&name) || customize_text::COLOUR_STRING_OPTIONS.contains(&name) {
        let fg = crate::format::parse_color(&srv.expand_extra(t, None, &value, HashMap::new())).unwrap_or_default();
        if !example(w, "This is a colour option: ", Style { fg, ..Style::default() }) {
            return;
        }
    }
    if customize_text::STYLE_OPTIONS.contains(&name) {
        let st = super::mode_style(srv, &value, Style::default(), t);
        if !example(w, "This is a style option: ", st) {
            return;
        }
    }
    if let Some(d) = default_value {
        if !w.value("The default is: ", &format!("{d}{unit}")) {
            return;
        }
    }
    w.skip();
    if w.y > w.cy + w.sy.saturating_sub(1) || is_array {
        return;
    }
    // the levels above the item's: the window's for a pane, the global one for a pane,
    // window or session
    let (wo, go) = match scope {
        Scope::Pane(p) => (srv.window_of(p).map(Scope::Window), Some(Scope::GlobalWindow)),
        Scope::Window(_) => (None, Some(Scope::GlobalWindow)),
        Scope::Session(_) => (None, Some(Scope::GlobalSession)),
        _ => (None, None),
    };
    if let Some(v) = wo.and_then(|s| option_value(srv, s, name, None, false)) {
        let idx = t.idx.unwrap_or(0);
        if !w.value(&format!("Window value (from window {idx}): "), &format!("{v}{unit}")) {
            return;
        }
    }
    if let Some(v) = go.and_then(|s| option_value(srv, s, name, None, false)) {
        w.value("Global value: ", &format!("{v}{unit}"));
    }
}

/// window_customize_draw_environment.
fn draw_environment(srv: &Server, w: &mut Writer, scope: Scope, name: &str) {
    let env = match scope {
        Scope::SessionEnv(s) => srv.sessions.get(&s).map(|s| &s.env),
        _ => Some(&srv.env),
    };
    let Some(entry) = env.and_then(|e| e.get(name)) else { return };
    let text = if scope == Scope::GlobalEnv { "global" } else { "session" };
    if !w.text(false, Style::default(), &format!("This is a {text} environment variable.")) || !w.skip_line() {
        return;
    }
    let ok = match entry {
        None => w.text(false, Style::default(), "Variable is removed."),
        Some(v) => w.value("Variable value: ", v),
    };
    if !ok || scope == Scope::GlobalEnv {
        return;
    }
    match srv.env.get(name) {
        None => {}
        Some(None) => {
            w.text(false, Style::default(), "Global variable is removed.");
        }
        Some(Some(v)) => {
            w.value("Global value: ", v);
        }
    }
}

/// window_customize_draw_key.
fn draw_key(srv: &Server, w: &mut Writer, table: &str, key: &str) {
    let Some(bd) = srv.bindings.get(table).and_then(|t| t.get(key)) else { return };
    let note = if bd.note.is_empty() || bd.note == "default" { "There is no note for this key." } else { bd.note.as_str() };
    let period = if !note.is_empty() && !note.ends_with('.') { "." } else { "" };
    if !w.text(false, Style::default(), &format!("{note}{period}")) || !w.skip_line() {
        return;
    }
    if !w.text(false, Style::default(), &format!("This key is in the {table} table.")) || !w.value("Repeat: ", if bd.repeat { "on" } else { "off" }) || !w.skip_line() {
        return;
    }
    if !w.value("Command: ", &bd.cmd) {
        return;
    }
    if let Some(d) = crate::server::input::default_bindings().get(table).and_then(|t| t.get(key)) {
        if d.cmd != bd.cmd {
            w.value("The default is: ", &d.cmd);
        }
    }
}

/// The information shown for the current item (window_customize_draw).
#[allow(clippy::too_many_arguments)]
pub fn draw(tree: &Tree, srv: &Server, b: &Built, item: usize, grid: &mut [Vec<Cell>], x: usize, y: usize, sx: usize, sy: usize) {
    let Some(it) = item_of(b, item) else { return };
    let mut w = Writer { grid, cx: x, cy: y, sx, sy, x, y };
    match &it {
        Item::Option { scope, name, key, hook } => draw_option(tree, srv, &mut w, *scope, name, key.as_deref(), *hook),
        Item::Env { scope, name } => draw_environment(srv, &mut w, *scope, name),
        Item::Key { table, key, .. } => draw_key(srv, &mut w, table, key),
        Item::Group => {}
    }
}
