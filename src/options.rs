//! tmux options: definitions with defaults, per-scope storage and validation.

use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    Server,
    Session,
    Window,
    Pane,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Str,
    Num,
    Flag,
    Choice(&'static [&'static str]),
    Style,
    Colour,
    Key,
    /// Array option such as status-format; stored as name[index].
    Array,
}

pub struct OptDef {
    pub name: &'static str,
    pub scope: Scope,
    pub kind: Kind,
    pub default: &'static str,
}

const fn d(name: &'static str, scope: Scope, kind: Kind, default: &'static str) -> OptDef {
    OptDef { name, scope, kind, default }
}

use Kind::*;
use Scope::*;

pub const STATUS_RIGHT: &str = "#{?window_bigger,[#{window_offset_x}#,#{window_offset_y}] ,}\"#{=21:pane_title}\" %H:%M %d-%b-%y";
pub const WINDOW_STATUS: &str = "#I:#W#{?window_flags,#{window_flags}, }";

pub static DEFS: &[OptDef] = &[
    // server
    d("backspace", Server, Key, "C-?"),
    d("buffer-limit", Server, Num, "50"),
    d("default-terminal", Server, Str, "xterm-256color"),
    d("escape-time", Server, Num, "10"),
    d("exit-empty", Server, Flag, "on"),
    d("exit-unattached", Server, Flag, "off"),
    d("extended-keys", Server, Choice(&["off", "on", "always"]), "off"),
    d("focus-events", Server, Flag, "off"),
    d("history-file", Server, Str, ""),
    d("message-limit", Server, Num, "1000"),
    d("prompt-history-limit", Server, Num, "100"),
    d("set-clipboard", Server, Choice(&["off", "external", "on"]), "external"),
    d("terminal-overrides", Server, Array, ""),
    d("user-keys", Server, Array, ""),
    // session
    d("activity-action", Session, Choice(&["none", "any", "current", "other"]), "other"),
    d("assume-paste-time", Session, Num, "1"),
    d("base-index", Session, Num, "0"),
    d("bell-action", Session, Choice(&["none", "any", "current", "other"]), "any"),
    d("default-command", Session, Str, ""),
    d("default-shell", Session, Str, ""),
    d("default-size", Session, Str, "80x24"),
    d("destroy-unattached", Session, Flag, "off"),
    d("detach-on-destroy", Session, Choice(&["off", "on", "no-detached", "previous", "next"]), "on"),
    d("display-panes-active-colour", Session, Colour, "red"),
    d("display-panes-colour", Session, Colour, "blue"),
    d("display-panes-time", Session, Num, "1000"),
    d("display-time", Session, Num, "750"),
    d("history-limit", Session, Num, "2000"),
    d("key-table", Session, Str, "root"),
    d("lock-after-time", Session, Num, "0"),
    d("lock-command", Session, Str, ""),
    d("message-command-style", Session, Style, "bg=black,fg=yellow"),
    d("message-style", Session, Style, "bg=yellow,fg=black"),
    d("mouse", Session, Flag, "off"),
    d("prefix", Session, Key, "C-b"),
    d("prefix2", Session, Key, "None"),
    d("renumber-windows", Session, Flag, "off"),
    d("repeat-time", Session, Num, "500"),
    d("set-titles", Session, Flag, "off"),
    d("set-titles-string", Session, Str, "#S:#I:#W - \"#T\" #{session_alerts}"),
    d("silence-action", Session, Choice(&["none", "any", "current", "other"]), "other"),
    d("status", Session, Choice(&["off", "on", "2", "3", "4", "5"]), "on"),
    d("status-bg", Session, Colour, "default"),
    d("status-fg", Session, Colour, "default"),
    d("status-format", Session, Array, ""),
    d("status-interval", Session, Num, "15"),
    d("status-justify", Session, Choice(&["left", "centre", "right", "absolute-centre"]), "left"),
    d("status-keys", Session, Choice(&["emacs", "vi"]), "emacs"),
    d("status-left", Session, Str, "[#{session_name}] "),
    d("status-left-length", Session, Num, "10"),
    d("status-left-style", Session, Style, "default"),
    d("status-position", Session, Choice(&["top", "bottom"]), "bottom"),
    d("status-right", Session, Str, STATUS_RIGHT),
    d("status-right-length", Session, Num, "40"),
    d("status-right-style", Session, Style, "default"),
    d("status-style", Session, Style, "bg=green,fg=black"),
    d("update-environment", Session, Array, "DISPLAY KRB5CCNAME SSH_ASKPASS SSH_AUTH_SOCK SSH_AGENT_PID SSH_CONNECTION WINDOWID XAUTHORITY"),
    d("visual-activity", Session, Choice(&["off", "on", "both"]), "off"),
    d("visual-bell", Session, Choice(&["off", "on", "both"]), "off"),
    d("visual-silence", Session, Choice(&["off", "on", "both"]), "off"),
    d("word-separators", Session, Str, " "),
    // window
    d("aggressive-resize", Window, Flag, "off"),
    d("automatic-rename", Window, Flag, "on"),
    d("automatic-rename-format", Window, Str, "#{?pane_in_mode,[tmux],#{pane_current_command}}#{?pane_dead,[dead],}"),
    d("clock-mode-colour", Window, Colour, "blue"),
    d("clock-mode-style", Window, Choice(&["12", "24"]), "24"),
    d("fill-character", Window, Str, ""),
    d("main-pane-height", Window, Num, "24"),
    d("main-pane-width", Window, Num, "80"),
    d("mode-keys", Window, Choice(&["emacs", "vi"]), "emacs"),
    d("mode-style", Window, Style, "bg=yellow,fg=black"),
    d("monitor-activity", Window, Flag, "off"),
    d("monitor-bell", Window, Flag, "on"),
    d("monitor-silence", Window, Num, "0"),
    d("other-pane-height", Window, Num, "0"),
    d("other-pane-width", Window, Num, "0"),
    d("pane-active-border-style", Window, Style, "fg=green"),
    d("pane-base-index", Window, Num, "0"),
    d("pane-border-format", Window, Str, "#{?pane_active,#[reverse],}#{pane_index}#[default] \"#{pane_title}\""),
    d("pane-border-lines", Window, Choice(&["single", "double", "heavy", "simple", "number"]), "single"),
    d("pane-border-status", Window, Choice(&["off", "top", "bottom"]), "off"),
    d("pane-border-style", Window, Style, "default"),
    d("popup-style", Window, Style, "default"),
    d("popup-border-style", Window, Style, "default"),
    d("window-size", Window, Choice(&["largest", "smallest", "manual", "latest"]), "latest"),
    d("window-status-activity-style", Window, Style, "reverse"),
    d("window-status-bell-style", Window, Style, "reverse"),
    d("window-status-current-format", Window, Str, WINDOW_STATUS),
    d("window-status-current-style", Window, Style, "default"),
    d("window-status-format", Window, Str, WINDOW_STATUS),
    d("window-status-last-style", Window, Style, "default"),
    d("window-status-separator", Window, Str, " "),
    d("window-status-style", Window, Style, "default"),
    d("wrap-search", Window, Flag, "on"),
    // pane (inherit from window options)
    d("allow-passthrough", Pane, Flag, "off"),
    d("allow-rename", Pane, Flag, "off"),
    d("alternate-screen", Pane, Flag, "on"),
    d("remain-on-exit", Pane, Choice(&["off", "on", "failed"]), "off"),
    d("remain-on-exit-format", Pane, Str, "Pane is dead (#{?pane_dead_signal,signal #{pane_dead_signal},status #{pane_dead_status}}, #{t:pane_dead_time})"),
    d("synchronize-panes", Pane, Flag, "off"),
    d("window-active-style", Pane, Style, "default"),
    d("window-style", Pane, Style, "default"),
];

pub fn def(name: &str) -> Option<&'static OptDef> {
    let base = name.split('[').next().unwrap_or(name);
    DEFS.iter().find(|d| d.name == base)
}

/// Scope an option lives in when set without -s/-w/-p: user options (@x) default to session.
pub fn natural_scope(name: &str) -> Option<Scope> {
    if name.starts_with('@') {
        return Some(Session);
    }
    def(name).map(|d| d.scope)
}

/// One level of option values (server, global session, a session, global window, a window, a pane).
#[derive(Default, Clone, Debug)]
pub struct Options(pub BTreeMap<String, String>);

impl Options {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
    pub fn set(&mut self, name: &str, value: String) {
        self.0.insert(name.to_string(), value);
    }
    pub fn unset(&mut self, name: &str) -> bool {
        let base = format!("{name}[");
        let had = self.0.remove(name).is_some();
        let before = self.0.len();
        self.0.retain(|k, _| !k.starts_with(&base));
        had || self.0.len() != before
    }

    /// Global options for a scope initialised with the defaults.
    pub fn defaults(scope: Scope) -> Options {
        let mut o = Options::default();
        for def in DEFS.iter().filter(|d| d.scope == scope || (scope == Window && d.scope == Pane)) {
            if def.kind == Array {
                for (i, part) in def.default.split(' ').filter(|s| !s.is_empty()).enumerate() {
                    o.set(&format!("{}[{i}]", def.name), part.to_string());
                }
            } else {
                o.set(def.name, def.default.to_string());
            }
        }
        o
    }
}

pub fn is_on(v: &str) -> bool {
    matches!(v, "on" | "1" | "yes" | "true")
}

/// Normalises a value for an option. `current` is used for toggling flags when `value` is None.
pub fn validate(name: &str, value: Option<&str>, current: Option<&str>) -> Result<String, String> {
    if name.starts_with('@') {
        return Ok(value.unwrap_or("").to_string());
    }
    let def = def(name).ok_or_else(|| format!("invalid option: {name}"))?;
    match def.kind {
        Flag => match value {
            None | Some("") => Ok(if current.is_some_and(is_on) { "off" } else { "on" }.into()),
            Some(v) if matches!(v, "on" | "1" | "yes" | "true") => Ok("on".into()),
            Some(v) if matches!(v, "off" | "0" | "no" | "false") => Ok("off".into()),
            Some(v) => Err(format!("bad value: {v}")),
        },
        Num => {
            let v = value.ok_or("value required")?;
            v.parse::<i64>().map(|n| n.to_string()).map_err(|_| format!("value is invalid: {v}"))
        }
        Choice(choices) => {
            let v = value.ok_or("value required")?;
            let v = if v == "center" { "centre" } else { v };
            if choices.contains(&v) {
                Ok(v.to_string())
            } else {
                Err(format!("unknown value: {v}"))
            }
        }
        Str | Style | Colour | Key | Array => Ok(value.unwrap_or("").to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_validation() {
        let s = Options::defaults(Session);
        assert_eq!(s.get("prefix"), Some("C-b"));
        assert_eq!(s.get("status-left"), Some("[#{session_name}] "));
        assert!(s.get("update-environment[0]").is_some());
        let w = Options::defaults(Window);
        assert_eq!(w.get("synchronize-panes"), Some("off"));
        assert_eq!(validate("mouse", None, Some("off")).unwrap(), "on");
        assert_eq!(validate("mouse", Some("yes"), None).unwrap(), "on");
        assert!(validate("base-index", Some("x"), None).is_err());
        assert_eq!(validate("status-justify", Some("center"), None).unwrap(), "centre");
        assert!(validate("nope", Some("1"), None).is_err());
        assert_eq!(validate("@mine", Some("v"), None).unwrap(), "v");
        assert_eq!(natural_scope("mode-keys"), Some(Window));
        assert_eq!(natural_scope("status-format[1]"), Some(Session));
    }
}
