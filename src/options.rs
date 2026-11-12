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
    /// A string colour, possibly containing formats (unlike a numeric colour).
    ColourFormat,
    Command,
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

// Defaults and choices from tmux 3.8 options-table.c, in table order (show-options
// lists them in this order). Pane means window + pane. Build-dependent defaults
// (TMUX_TERM, _PATH_BSHELL, TMUX_LOCK_CMD) keep pwsh-tmux's platform choices:
// default-shell is filled in at server start and an empty lock-command means none.
pub const STATUS_FORMATS: &[&str] = &[
    "#[align=left range=left #{E:status-left-style}]#[push-default]#{T;=/#{status-left-length}:status-left}#[pop-default]#[norange default]#[list=on align=#{status-justify}]#[list=left-marker]<#[list=right-marker]>#[list=on]#{W:#[range=window|#{window_index} #{E:window-status-style}#{?#{&&:#{window_last_flag},#{!=:#{E:window-status-last-style},default}}, #{E:window-status-last-style},}#{?#{&&:#{window_bell_flag},#{!=:#{E:window-status-bell-style},default}}, #{E:window-status-bell-style},#{?#{&&:#{||:#{window_activity_flag},#{window_silence_flag}},#{!=:#{E:window-status-activity-style},default}}, #{E:window-status-activity-style},}}]#[push-default]#{T:window-status-format}#[pop-default]#[norange default]#{?loop_last_flag,,#{E:window-status-separator}},#[range=window|#{window_index} list=focus #{?#{!=:#{E:window-status-current-style},default},#{E:window-status-current-style},#{E:window-status-style}}#{?#{&&:#{window_last_flag},#{!=:#{E:window-status-last-style},default}}, #{E:window-status-last-style},}#{?#{&&:#{window_bell_flag},#{!=:#{E:window-status-bell-style},default}}, #{E:window-status-bell-style},#{?#{&&:#{||:#{window_activity_flag},#{window_silence_flag}},#{!=:#{E:window-status-activity-style},default}}, #{E:window-status-activity-style},}}]#[push-default]#{T:window-status-current-format}#[pop-default]#[norange list=on default]#{?loop_last_flag,,#{E:window-status-separator}}}#[nolist align=right range=right #{E:status-right-style}]#[push-default]#{T;=/#{status-right-length}:status-right}#[pop-default]#[norange default]",
    "#[align=left]#{R: ,#{e|-:#{w;T;=/#{status-left-length}:status-left},3}}P: #[norange default]#[list=on align=#{status-justify}]#[list=left-marker]<#[list=right-marker]>#[list=on]#{P:#[range=pane|#{pane_id} #{E:pane-status-style}]#[push-default]#{T:window-pane-status-format}#[pop-default]#[norange list=on default]#{?loop_last_flag,,#{E:window-status-separator}},#[range=pane|#{pane_id} list=focus #{?#{!=:#{E:pane-status-current-style},default},#{E:pane-status-current-style},#{E:pane-status-style}}]#[push-default]#{T:window-pane-current-status-format}#[pop-default]#[norange list=on default]#{?loop_last_flag,,#{E:window-status-separator}}}",
    "#[align=left]#{R: ,#{e|-:#{w;T;=/#{status-left-length}:status-left},3}}S: #[norange default]#[list=on align=#{status-justify}]#[list=left-marker]<#[list=right-marker]>#[list=on]#{S:#[range=session|#{session_id} #{E:session-status-style}]#[push-default]#S#{q/h:session_alert}#[pop-default]#[norange list=on default]  ,#[range=session|#{session_id} list=focus #{?#{!=:#{E:session-status-current-style},default},#{E:session-status-current-style},#{E:session-status-style}}]#[push-default]#S*#{q/h:session_alert}#[pop-default]#[norange list=on default] }",
];

pub static DEFS: &[OptDef] = &[
    d("backspace", Server, Key, "C-?"),
    d("buffer-limit", Server, Num, "50"),
    d("clear-on-attach", Server, Flag, "on"),
    d("command-alias", Server, Array, "split-pane=split-window,splitp=split-window,server-info=show-messages -JT,info=show-messages -JT,choose-window=choose-tree -w,choose-session=choose-tree -s"),
    d("codepoint-widths", Server, Array, ""),
    d("copy-command", Server, Str, ""),
    d("cursor-colour", Pane, ColourFormat, ""),
    d("cursor-style", Pane, Choice(&["default", "blinking-block", "block", "blinking-underline", "underline", "blinking-bar", "bar"]), "default"),
    d("default-client-command", Server, Command, "new-session"),
    d("default-terminal", Server, Str, "xterm-256color"),
    d("editor", Server, Str, if cfg!(windows) { "notepad.exe" } else { "/usr/bin/vi" }),
    d("escape-time", Server, Num, "10"),
    d("exit-empty", Server, Flag, "on"),
    d("exit-unattached", Server, Flag, "off"),
    d("extended-keys", Server, Choice(&["off", "on", "always"]), "off"),
    d("extended-keys-format", Server, Choice(&["csi-u", "xterm"]), "xterm"),
    d("focus-events", Server, Flag, "off"),
    d("get-clipboard", Server, Choice(&["off", "buffer", "request", "both"]), "buffer"),
    d("history-file", Server, Str, ""),
    d("input-buffer-size", Server, Num, "1048576"),
    d("menu-style", Window, Style, "bg=themedarkgrey,fg=themewhite"),
    d("menu-selected-style", Window, Style, "bg=themeyellow,fg=themeblack"),
    d("menu-border-style", Window, Style, "bg=themedarkgrey,fg=themelightgrey"),
    d("menu-border-lines", Window, Choice(&["single", "double", "heavy", "simple", "rounded", "padded", "none"]), "single"),
    d("message-limit", Server, Num, "1000"),
    d("prefix-timeout", Server, Num, "0"),
    d("prompt-history-limit", Server, Num, "100"),
    d("set-clipboard", Server, Choice(&["off", "external", "on"]), "external"),
    d("terminal-overrides", Server, Array, "linux*:AX@"),
    d("terminal-features", Server, Array, "xterm*:clipboard:ccolour:cstyle:focus:title,screen*:title,rxvt*:ignorefkeys"),
    d("theme", Server, Choice(&["detect", "terminal", "light", "dark"]), "detect"),
    d("dark-theme-black", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},gray5,black}"),
    d("dark-theme-white", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},gray90,white}"),
    d("dark-theme-light-grey", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},gray70,white}"),
    d("dark-theme-dark-grey", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},gray15,black}"),
    d("dark-theme-green", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},yellowgreen,green}"),
    d("dark-theme-yellow", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},darkgoldenrod,yellow}"),
    d("dark-theme-red", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},indianred,red}"),
    d("dark-theme-blue", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},skyblue3,blue}"),
    d("dark-theme-cyan", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},cadetblue,cyan}"),
    d("dark-theme-magenta", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},mediumpurple,magenta}"),
    d("light-theme-black", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},gray10,black}"),
    d("light-theme-white", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},gray95,white}"),
    d("light-theme-light-grey", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},gray80,white}"),
    d("light-theme-dark-grey", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},gray45,black}"),
    d("light-theme-green", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},seagreen,green}"),
    d("light-theme-yellow", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},darkgoldenrod,yellow}"),
    d("light-theme-red", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},indianred4,red}"),
    d("light-theme-blue", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},steelblue,blue}"),
    d("light-theme-cyan", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},darkcyan,cyan}"),
    d("light-theme-magenta", Server, ColourFormat, "#{?#{e|>=:#{client_colours},256},purple4,magenta}"),
    d("user-keys", Server, Array, ""),
    d("variation-selector-always-wide", Server, Flag, "on"),
    d("activity-action", Session, Choice(&["none", "any", "current", "other"]), "other"),
    d("assume-paste-time", Session, Num, "1"),
    d("base-index", Session, Num, "0"),
    d("bell-action", Session, Choice(&["none", "any", "current", "other"]), "any"),
    d("default-command", Session, Str, ""),
    d("default-shell", Session, Str, ""),
    d("default-size", Session, Str, "80x24"),
    d("destroy-unattached", Session, Choice(&["off", "on", "keep-last", "keep-group"]), "off"),
    d("detach-on-destroy", Session, Choice(&["off", "on", "no-detached", "previous", "next"]), "on"),
    d("display-time", Session, Num, "750"),
    d("focus-follows-mouse", Session, Flag, "off"),
    d("history-limit", Session, Num, "2000"),
    d("initial-repeat-time", Session, Num, "0"),
    d("key-table", Session, Str, "root"),
    d("lock-after-time", Session, Num, "0"),
    d("lock-command", Session, Str, ""),
    d("message-command-style", Session, Style, "bg=themeblack,fg=themeyellow,#{?#{m/r:(^|#,)IS(PANE|MODE)($|#,),#{prompt_flags}},,fill=themeblack}"),
    d("message-format", Session, Str, "#[#{?#{command_prompt},#{E:message-command-style},#{E:message-style}}]#{message}"),
    d("message-line", Session, Choice(&["0", "1", "2", "3", "4"]), "0"),
    d("message-style", Session, Style, "bg=themeyellow,fg=themeblack,#{?#{m/r:(^|#,)IS(PANE|MODE)($|#,),#{prompt_flags}},,fill=themeyellow}"),
    d("mouse", Session, Flag, "on"),
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
    d("status-style", Session, Style, "bg=themegreen,fg=themeblack"),
    d("pane-status-current-style", Window, Style, "underscore"),
    d("pane-status-style", Window, Style, "default"),
    d("prompt-cursor-colour", Session, ColourFormat, ""),
    d("prompt-command-cursor-colour", Session, ColourFormat, ""),
    d("prompt-cursor-style", Session, Choice(&["default", "blinking-block", "block", "blinking-underline", "underline", "blinking-bar", "bar"]), "default"),
    d("prompt-command-cursor-style", Session, Choice(&["default", "blinking-block", "block", "blinking-underline", "underline", "blinking-bar", "bar"]), "default"),
    d("session-status-current-style", Window, Style, "underscore"),
    d("session-status-style", Window, Style, "default"),
    d("update-environment", Session, Array, "DISPLAY KRB5CCNAME MSYSTEM SSH_ASKPASS SSH_AUTH_SOCK SSH_AGENT_PID SSH_CONNECTION WAYLAND_DISPLAY WINDOWID XAUTHORITY XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE"),
    d("visual-activity", Session, Choice(&["off", "on", "both"]), "off"),
    d("visual-bell", Session, Choice(&["off", "on", "both"]), "off"),
    d("visual-silence", Session, Choice(&["off", "on", "both"]), "off"),
    d("word-separators", Session, Str, "!\"#$%&'()*+,-./:;<=>?@[\\]^`{|}~"),
    d("aggressive-resize", Window, Flag, "off"),
    d("allow-passthrough", Pane, Choice(&["off", "on", "all"]), "off"),
    d("allow-rename", Pane, Flag, "off"),
    d("allow-set-title", Pane, Flag, "on"),
    d("alternate-screen", Pane, Flag, "on"),
    d("automatic-rename", Window, Flag, "on"),
    d("automatic-rename-format", Window, Str, "#{?pane_in_mode,[tmux],#{pane_current_command}}#{?pane_dead,[dead],}"),
    d("clock-mode-colour", Window, ColourFormat, "themeblue"),
    d("clock-mode-style", Window, Choice(&["12", "24", "12-with-seconds", "24-with-seconds"]), "24"),
    d("display-panes-active-colour", Window, ColourFormat, "themered"),
    d("display-panes-border-style", Window, Style, "bg=themedarkgrey,fg=themelightgrey"),
    d("display-panes-colour", Window, ColourFormat, "themeblue"),
    d("display-panes-format", Window, Str, "#[align=right]#{pane_unzoomed_width}x#{pane_unzoomed_height}"),
    d("display-panes-time", Window, Num, "1000"),
    d("copy-mode-match-style", Window, Style, "bg=themecyan,fg=themeblack"),
    d("copy-mode-current-match-style", Window, Style, "bg=thememagenta,fg=themeblack"),
    d("copy-mode-mark-style", Window, Style, "bg=themeyellow,fg=themeblack"),
    d("copy-mode-position-format", Pane, Str, "#[align=right]#{t/p:top_line_time}#{?#{e|>:#{top_line_time},0}, ,}[#{copy_position}/#{copy_position_limit}]#{?search_timed_out, (timed out),#{?search_count, (#{search_count}#{?search_count_partial,+,} results),}}"),
    d("copy-mode-position-style", Window, Style, "#{E:mode-style}"),
    d("copy-mode-selection-style", Window, Style, "#{E:mode-style}"),
    d("copy-mode-current-line-number-style", Window, Style, "fg=themeyellow"),
    d("copy-mode-current-line-style", Window, Style, "default"),
    d("copy-mode-line-number-style", Window, Style, "fg=themelightgrey,dim"),
    d("copy-mode-line-numbers", Window, Choice(&["off", "default", "absolute", "relative", "hybrid"]), "off"),
    d("fill-character", Window, Str, "#{?is_inside,#[bg=themedarkgrey] ,#[fg=themelightgrey]#[acs]~}"),
    d("main-pane-height", Window, Str, "24"),
    d("main-pane-width", Window, Str, "80"),
    d("mode-keys", Window, Choice(&["emacs", "vi"]), "emacs"),
    d("mode-style", Window, Style, "noattr,bg=themeyellow,fg=themeblack"),
    d("monitor-activity", Window, Flag, "off"),
    d("monitor-bell", Window, Flag, "on"),
    d("monitor-silence", Window, Num, "0"),
    d("other-pane-height", Window, Str, "0"),
    d("other-pane-width", Window, Str, "0"),
    d("pane-active-border-style", Pane, Style, "fg=#{?pane_modal_flag,themeblue,#{?pane_marked,thememagenta,#{?synchronize-panes,themered,#{?pane_in_mode,themeyellow,themegreen}}}}"),
    d("pane-base-index", Window, Num, "0"),
    d("pane-border-format", Pane, Str, "#{?pane_active,#[reverse],}#{pane_index}#[default] \"#{pane_title}\"#{?#{mouse},#[align=right]#[range=control|7][#{?#{pane_floating_flag},t,f}]#[norange]#[range=control|8][#{?#{window_zoomed_flag},u,z}]#[norange]#[range=control|9][x]#[norange],}"),
    d("pane-border-indicators", Window, Choice(&["off", "colour", "arrows", "both"]), "colour"),
    d("pane-border-lines", Pane, Choice(&["single", "double", "heavy", "simple", "number", "spaces", "none"]), "single"),
    d("pane-border-status", Pane, Choice(&["off", "top", "bottom", "top-floating", "bottom-floating"]), "off"),
    d("pane-border-style", Pane, Style, "fg=themelightgrey"),
    d("pane-colours", Pane, Array, ""),
    d("pane-scrollbars", Window, Choice(&["off", "modal", "on", "auto-hide"]), "off"),
    d("pane-scrollbars-timeout", Window, Num, "500"),
    d("pane-scrollbars-style", Pane, Style, "bg=themedarkgrey,fg=themelightgrey,width=1,pad=0"),
    d("pane-scrollbars-position", Window, Choice(&["right", "left"]), "right"),
    d("popup-style", Window, Style, "bg=themedarkgrey,fg=themewhite"),
    d("popup-border-style", Window, Style, "bg=themedarkgrey,fg=themelightgrey"),
    d("popup-border-lines", Window, Choice(&["single", "double", "heavy", "simple", "rounded", "padded", "none"]), "single"),
    d("remain-on-exit", Pane, Choice(&["off", "on", "failed", "key", "failed-key"]), "off"),
    d("remain-on-exit-format", Pane, Str, "Pane is dead (#{?#{!=:#{pane_dead_status},},status #{pane_dead_status},}#{?#{!=:#{pane_dead_signal},},signal #{pane_dead_signal},}, #{t:pane_dead_time})"),
    d("scroll-on-clear", Pane, Flag, "on"),
    d("switch-mode-match-style", Pane, Style, "bg=cyan fg=black"),
    d("synchronize-panes", Pane, Flag, "off"),
    d("tiled-layout-max-columns", Window, Num, "0"),
    d("tree-mode-border-style", Window, Style, "bg=themedarkgrey,fg=themelightgrey"),
    d("tree-mode-preview-format", Pane, Str, "#{?pane_format,#{pane_index}:#{pane_title},#{window_index}:#{window_name}}"),
    d("tree-mode-preview-style", Window, Style, "fg=#{?#{||:#{&&:#{pane_format},#{pane_active}},#{&&:#{window_format},#{window_active}}},themered,themeblue}"),
    d("tree-mode-selection-style", Window, Style, "#{E:mode-style}"),
    d("window-active-style", Pane, Style, "default"),
    d("window-pane-current-status-format", Window, Str, "#P:[#T]#{?pane_flags,#{pane_flags}, }"),
    d("window-pane-status-format", Window, Str, "#P:[#T]#{?pane_flags,#{pane_flags}, }"),
    d("window-size", Window, Choice(&["largest", "smallest", "manual", "latest"]), "latest"),
    d("window-style", Pane, Style, "default"),
    d("window-status-activity-style", Window, Style, "reverse"),
    d("window-status-bell-style", Window, Style, "reverse"),
    d("window-status-current-format", Window, Str, WINDOW_STATUS),
    d("window-status-current-style", Window, Style, "underscore"),
    d("window-status-format", Window, Str, WINDOW_STATUS),
    d("window-status-last-style", Window, Style, "default"),
    d("window-status-separator", Window, Str, " "),
    d("window-status-style", Window, Style, "default"),
    d("wrap-search", Window, Flag, "on"),
    d("xterm-keys", Window, Flag, "on"),
];

impl OptDef {
    pub fn number_range(&self) -> (i64, i64) {
        match self.name {
            "buffer-limit" => (1, 2147483647),
            "escape-time" => (0, 2147483647),
            "input-buffer-size" => (1048576, 4294967295),
            "message-limit" => (0, 2147483647),
            "prefix-timeout" => (0, 2147483647),
            "prompt-history-limit" => (0, 2147483647),
            "assume-paste-time" => (0, 2147483647),
            "base-index" => (0, 2147483647),
            "display-time" => (0, 2147483647),
            "history-limit" => (0, 2147483647),
            "initial-repeat-time" => (0, 2000000),
            "lock-after-time" => (0, 2147483647),
            "repeat-time" => (0, 2000000),
            "status-interval" => (0, 2147483647),
            "status-left-length" => (0, 32767),
            "status-right-length" => (0, 32767),
            "display-panes-time" => (1, 2147483647),
            "monitor-silence" => (0, 2147483647),
            "pane-base-index" => (0, 65535),
            "pane-scrollbars-timeout" => (0, 2147483647),
            "tiled-layout-max-columns" => (0, 65535),
            _ => (i64::MIN, i64::MAX),
        }
    }

    /// Separators splitting an array value (options-table.c `.separator`, default " ,").
    pub fn array_separator(&self) -> &'static str {
        match self.name {
            "command-alias" | "codepoint-widths" | "terminal-overrides" | "terminal-features" | "user-keys" => ",",
            _ => " ,",
        }
    }

    /// Separator inserted by `set -a` between the old and new value (styles use ",").
    pub fn append_separator(&self) -> &'static str {
        if self.kind == Style { "," } else { "" }
    }
}

// Old spellings tmux still accepts (options_other_names).
const OTHER_NAMES: &[(&str, &str)] = &[
    ("display-panes-color", "display-panes-colour"),
    ("display-panes-active-color", "display-panes-active-colour"),
    ("clock-mode-color", "clock-mode-colour"),
    ("cursor-color", "cursor-colour"),
    ("prompt-cursor-color", "prompt-cursor-colour"),
    ("prompt-command-cursor-color", "prompt-command-cursor-colour"),
    ("pane-colors", "pane-colours"),
];

/// Resolve an option argument like tmux's options_match: the array key is
/// canonicalised (numeric keys lose leading zeros), old -color names are mapped
/// and a unique prefix selects an option. A malformed key reports "ambiguous
/// option" as tmux does. Array keys on non-array options are rejected.
pub fn canonical_name(arg: &str) -> Result<String, String> {
    let ambiguous = || format!("ambiguous option: {arg}");
    if arg.is_empty() { return Err(ambiguous()); }
    let (base, key) = match arg.split_once('[') {
        None => (arg, None),
        Some((base, tail)) => {
            let key = tail.strip_suffix(']').filter(|k| !k.is_empty() && !k.contains(']')).ok_or_else(ambiguous)?;
            let key = if key.bytes().all(|b| b.is_ascii_digit()) {
                key.parse::<u32>().map_err(|_| ambiguous())?.to_string()
            } else { key.to_string() };
            (base, Some(key))
        }
    };
    let name = if base.starts_with('@') { base } else {
        let base = OTHER_NAMES.iter().find(|(from, _)| *from == base).map_or(base, |(_, to)| to);
        match DEFS.iter().find(|d| d.name == base) {
            Some(d) => d.name,
            None => {
                let mut m = DEFS.iter().filter(|d| d.name.starts_with(base));
                match (m.next(), m.next()) {
                    (Some(d), None) => d.name,
                    (Some(_), Some(_)) => return Err(ambiguous()),
                    _ => return Err(format!("invalid option: {arg}")),
                }
            }
        }
    };
    match key {
        None => Ok(name.into()),
        Some(_) if !def(name).is_some_and(|d| d.kind == Array) => Err(format!("not an array: {arg}")),
        Some(k) => Ok(format!("{name}[{k}]")),
    }
}

pub fn def(name: &str) -> Option<&'static OptDef> {
    let base = name.split('[').next().unwrap_or(name);
    DEFS.iter().find(|d| d.name == base)
}

/// tmux sorts numeric array keys numerically and before string keys.
pub fn compare_names(a: &str, b: &str) -> std::cmp::Ordering {
    let (ab, ak) = a.split_once('[').unwrap_or((a, ""));
    let (bb, bk) = b.split_once('[').unwrap_or((b, ""));
    ab.cmp(bb).then_with(|| {
        let an = ak.strip_suffix(']').and_then(|k| k.parse::<u32>().ok());
        let bn = bk.strip_suffix(']').and_then(|k| k.parse::<u32>().ok());
        match (an, bn) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) if !bk.is_empty() => std::cmp::Ordering::Less,
            (None, Some(_)) if !ak.is_empty() => std::cmp::Ordering::Greater,
            _ => ak.cmp(bk),
        }
    })
}

/// How show-options prints a value: string options go through args_escape;
/// numbers, flags, choices, keys, colours and commands print raw.
pub fn print_value(name: &str, value: &str) -> String {
    if def(name).is_some_and(|d| matches!(d.kind, Num | Flag | Choice(_) | Key | Colour | Command) || d.name == "pane-colours") {
        return value.into();
    }
    escape_arg(value)
}

/// tmux's args_escape: double quotes when the value has any of ` #';${}%`,
/// single quotes when it has `"`, then utf8_stravis with VIS_OCTAL|VIS_CSTYLE|
/// VIS_TAB|VIS_NL (and VIS_DQ inside double quotes).
pub fn escape_arg(s: &str) -> String {
    if s.is_empty() { return "''".into(); }
    let quote = if s.contains([' ', '#', '\'', ';', '$', '{', '}', '%']) { Some('"') }
        else if s.contains('"') { Some('\'') } else { None };
    let chars: Vec<char> = s.chars().collect();
    if let [c] = chars[..] {
        if c != ' ' && (quote.is_some() || c == '~') { return format!("\\{c}"); }
    }
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let next = chars.get(i + 1).copied();
        match c {
            // VIS_DQ escapes '$' only where the parser would expand it.
            '$' if quote == Some('"') => {
                if next.is_some_and(|n| n.is_ascii_alphabetic() || n == '_' || n == '{') { out.push('\\'); }
                out.push('$');
            }
            '"' if quote == Some('"') => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\x08' => out.push_str("\\b"),
            '\x07' => out.push_str("\\a"),
            '\x0b' => out.push_str("\\v"),
            '\t' => out.push_str("\\t"),
            '\x0c' => out.push_str("\\f"),
            '\0' => out.push_str(if next.is_some_and(|n| ('0'..='7').contains(&n)) { "\\000" } else { "\\0" }),
            c if c.is_ascii_control() => out.push_str(&format!("\\{:03o}", c as u32)),
            c => out.push(c),
        }
    }
    match quote {
        Some('"') if out.starts_with('~') => format!("\"\\{out}\""),
        Some(q) => format!("{q}{out}{q}"),
        None if out.starts_with('~') => format!("\\{out}"),
        None => out,
    }
}

/// Scope an option lives in when set without -s/-w/-p: user options (@x) default to session.
pub fn natural_scope(name: &str) -> Option<Scope> {
    if name.starts_with('@') {
        return Some(Session);
    }
    def(name).map(|d| if d.scope == Pane { Window } else { d.scope })
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
    /// Whether this level holds the option itself (an array counts when it has
    /// any element or was assigned, even empty), like options_get_only.
    pub fn has(&self, name: &str) -> bool {
        self.0.contains_key(name) || (def(name).is_some_and(|d| d.kind == Array) && !name.contains('[')
            && self.0.range(format!("{name}[")..).next().is_some_and(|(k, _)| k.starts_with(&format!("{name}["))))
    }

    /// Array elements (name[key], value) in tmux key order.
    pub fn array_items(&self, name: &str) -> Vec<(String, String)> {
        let prefix = format!("{name}[");
        let mut v: Vec<_> = self.0.range(prefix.clone()..).take_while(|(k, _)| k.starts_with(&prefix))
            .map(|(k, v)| (k.clone(), v.clone())).collect();
        v.sort_by(|a, b| compare_names(&a.0, &b.0));
        v
    }

    pub fn unset(&mut self, name: &str) -> bool {
        let base = format!("{name}[");
        let had = self.0.remove(name).is_some();
        let before = self.0.len();
        self.0.retain(|k, _| !k.starts_with(&base));
        had || self.0.len() != before
    }

    /// Reset a whole option (including every default array element).
    pub fn reset(&mut self, name: &str) {
        self.unset(name);
        let Some(d) = def(name) else { return };
        if name.contains('[') { return; }
        if d.kind != Array {
            self.set(name, d.default.into());
        } else if name == "status-format" {
            self.set(name, String::new());
            for (i, value) in STATUS_FORMATS.iter().enumerate() {
                self.set(&format!("{name}[{i}]"), (*value).into());
            }
        } else {
            self.assign_array(name, d.default, false).expect("valid default array");
        }
    }

    pub fn assign_array(&mut self, name: &str, value: &str, append: bool) -> Result<(), String> {
        let d = def(name).ok_or_else(|| format!("invalid option: {name}"))?;
        let parts: Vec<_> = value.split(|c| d.array_separator().contains(c)).filter(|v| !v.is_empty()).collect();
        // Validate before changing the store, so a bad palette leaves the array intact.
        let parts: Vec<_> = parts.into_iter().map(|v| validate_array_value(d, v)).collect::<Result<_, _>>()?;
        if !append { self.unset(name); }
        self.set(name, String::new());
        let mut i = 0u32;
        for value in parts {
            while self.get(&format!("{name}[{i}]")).is_some() { i += 1; }
            self.set(&format!("{name}[{i}]"), value);
        }
        Ok(())
    }

    /// Global options for a scope initialised with the defaults.
    pub fn defaults(scope: Scope) -> Options {
        let mut o = Options::default();
        for def in DEFS.iter().filter(|d| d.scope == scope || (scope == Window && d.scope == Pane)) {
            o.reset(def.name);
        }
        o
    }
}

pub fn is_on(v: &str) -> bool {
    matches!(v, "on" | "1" | "yes" | "true")
}

/// Normalises a value for an option. `current` is used for toggling flags and
/// two-way choices when `value` is None; other types need a value like tmux.
pub fn validate(name: &str, value: Option<&str>, current: Option<&str>) -> Result<String, String> {
    if name.starts_with('@') {
        return value.map(String::from).ok_or_else(|| "empty value".into());
    }
    let def = def(name).ok_or_else(|| format!("invalid option: {name}"))?;
    let need = || value.ok_or_else(|| "empty value".to_string());
    match def.kind {
        Flag => match value {
            None | Some("") => Ok(if current.is_some_and(is_on) { "off" } else { "on" }.into()),
            Some(v) if ["on", "1", "yes", "true"].iter().any(|x| v.eq_ignore_ascii_case(x)) => Ok("on".into()),
            Some(v) if ["off", "0", "no", "false"].iter().any(|x| v.eq_ignore_ascii_case(x)) => Ok("off".into()),
            Some(v) => Err(format!("bad value: {v}")),
        },
        Num => {
            let v = need()?;
            let n = v.parse::<i64>().map_err(|_| format!("value is invalid: {v}"))?;
            let (min, max) = def.number_range();
            if n < min { return Err(format!("value is too small: {v}")); }
            if n > max { return Err(format!("value is too large: {v}")); }
            Ok(n.to_string())
        }
        Choice(choices) => {
            let v = match value {
                Some(v) => v,
                // like tmux, only the first two choices toggle
                None => {
                    let i = choices.iter().position(|v| Some(*v) == current).unwrap_or(0);
                    choices[if i < 2 { 1 - i } else { i }]
                }
            };
            // pwsh-tmux also accepts the US spelling of centre
            let v = if v == "center" { "centre" } else { v };
            if choices.contains(&v) { Ok(v.to_string()) } else { Err(format!("unknown value: {v}")) }
        }
        Array => {
            let v = need()?;
            if name.contains('[') { validate_array_value(def, v) } else { Ok(v.into()) }
        }
        Colour => normalize_colour(need()?),
        ColourFormat => {
            let v = need()?;
            if v.is_empty() || v.contains("#{") || valid_colour(v) { Ok(v.into()) }
            else { Err(format!("invalid colour: {v}")) }
        }
        Style => {
            let v = need()?;
            validate_style(v)?;
            Ok(v.into())
        }
        Key => {
            let v = need()?;
            if v.eq_ignore_ascii_case("none") { return Ok("None".into()); }
            crate::keys::parse_key_name(v).map(|k| crate::keys::key_name(&k))
                .ok_or_else(|| format!("bad key: {v}"))
        }
        Command => {
            let v = need()?;
            crate::cmdparse::parse(v)?;
            Ok(v.into())
        }
        Str => {
            let v = need()?;
            // options-table.c pattern "[0-9]*x[0-9]*" (fnmatch)
            if def.name == "default-size" && !(v.starts_with(|c: char| c.is_ascii_digit())
                && v.match_indices('x').any(|(i, _)| i > 0 && v[i + 1..].starts_with(|c: char| c.is_ascii_digit()))) {
                return Err(format!("value is invalid: {v}"));
            }
            Ok(v.into())
        }
    }
}

fn validate_array_value(def: &OptDef, v: &str) -> Result<String, String> {
    if def.name == "pane-colours" { normalize_colour(v) } else { Ok(v.into()) }
}


// X11 colour names accepted by tmux 3.8 colour.c. Values are used only to
// canonicalise numeric colour options; theme colour strings stay unexpanded.
const NAMED_COLOURS: &str = concat!(
    "AliceBlue=f0f8ff|AntiqueWhite=faebd7|AntiqueWhite1=ffefdb|AntiqueWhite2=eedfcc|AntiqueWhite3=cdc0b0|",
    "AntiqueWhite4=8b8378|BlanchedAlmond=ffebcd|BlueViolet=8a2be2|CadetBlue=5f9ea0|CadetBlue1=98f5ff|",
    "CadetBlue2=8ee5ee|CadetBlue3=7ac5cd|CadetBlue4=53868b|CornflowerBlue=6495ed|DarkBlue=00008b|DarkCyan=008b8b|",
    "DarkGoldenrod=b8860b|DarkGoldenrod1=ffb90f|DarkGoldenrod2=eead0e|DarkGoldenrod3=cd950c|DarkGoldenrod4=8b6508|",
    "DarkGray=a9a9a9|DarkGreen=006400|DarkGrey=a9a9a9|DarkKhaki=bdb76b|DarkMagenta=8b008b|DarkOliveGreen=556b2f|",
    "DarkOliveGreen1=caff70|DarkOliveGreen2=bcee68|DarkOliveGreen3=a2cd5a|DarkOliveGreen4=6e8b3d|DarkOrange=ff8c00|",
    "DarkOrange1=ff7f00|DarkOrange2=ee7600|DarkOrange3=cd6600|DarkOrange4=8b4500|DarkOrchid=9932cc|",
    "DarkOrchid1=bf3eff|DarkOrchid2=b23aee|DarkOrchid3=9a32cd|DarkOrchid4=68228b|DarkRed=8b0000|DarkSalmon=e9967a|",
    "DarkSeaGreen=8fbc8f|DarkSeaGreen1=c1ffc1|DarkSeaGreen2=b4eeb4|DarkSeaGreen3=9bcd9b|DarkSeaGreen4=698b69|",
    "DarkSlateBlue=483d8b|DarkSlateGray=2f4f4f|DarkSlateGray1=97ffff|DarkSlateGray2=8deeee|DarkSlateGray3=79cdcd|",
    "DarkSlateGray4=528b8b|DarkSlateGrey=2f4f4f|DarkTurquoise=00ced1|DarkViolet=9400d3|DeepPink=ff1493|",
    "DeepPink1=ff1493|DeepPink2=ee1289|DeepPink3=cd1076|DeepPink4=8b0a50|DeepSkyBlue=00bfff|DeepSkyBlue1=00bfff|",
    "DeepSkyBlue2=00b2ee|DeepSkyBlue3=009acd|DeepSkyBlue4=00688b|DimGray=696969|DimGrey=696969|DodgerBlue=1e90ff|",
    "DodgerBlue1=1e90ff|DodgerBlue2=1c86ee|DodgerBlue3=1874cd|DodgerBlue4=104e8b|FloralWhite=fffaf0|",
    "ForestGreen=228b22|GhostWhite=f8f8ff|GreenYellow=adff2f|HotPink=ff69b4|HotPink1=ff6eb4|HotPink2=ee6aa7|",
    "HotPink3=cd6090|HotPink4=8b3a62|IndianRed=cd5c5c|IndianRed1=ff6a6a|IndianRed2=ee6363|IndianRed3=cd5555|",
    "IndianRed4=8b3a3a|LavenderBlush=fff0f5|LavenderBlush1=fff0f5|LavenderBlush2=eee0e5|LavenderBlush3=cdc1c5|",
    "LavenderBlush4=8b8386|LawnGreen=7cfc00|LemonChiffon=fffacd|LemonChiffon1=fffacd|LemonChiffon2=eee9bf|",
    "LemonChiffon3=cdc9a5|LemonChiffon4=8b8970|LightBlue=add8e6|LightBlue1=bfefff|LightBlue2=b2dfee|",
    "LightBlue3=9ac0cd|LightBlue4=68838b|LightCoral=f08080|LightCyan=e0ffff|LightCyan1=e0ffff|LightCyan2=d1eeee|",
    "LightCyan3=b4cdcd|LightCyan4=7a8b8b|LightGoldenrod=eedd82|LightGoldenrod1=ffec8b|LightGoldenrod2=eedc82|",
    "LightGoldenrod3=cdbe70|LightGoldenrod4=8b814c|LightGoldenrodYellow=fafad2|LightGray=d3d3d3|LightGreen=90ee90|",
    "LightGrey=d3d3d3|LightPink=ffb6c1|LightPink1=ffaeb9|LightPink2=eea2ad|LightPink3=cd8c95|LightPink4=8b5f65|",
    "LightSalmon=ffa07a|LightSalmon1=ffa07a|LightSalmon2=ee9572|LightSalmon3=cd8162|LightSalmon4=8b5742|",
    "LightSeaGreen=20b2aa|LightSkyBlue=87cefa|LightSkyBlue1=b0e2ff|LightSkyBlue2=a4d3ee|LightSkyBlue3=8db6cd|",
    "LightSkyBlue4=607b8b|LightSlateBlue=8470ff|LightSlateGray=778899|LightSlateGrey=778899|LightSteelBlue=b0c4de|",
    "LightSteelBlue1=cae1ff|LightSteelBlue2=bcd2ee|LightSteelBlue3=a2b5cd|LightSteelBlue4=6e7b8b|",
    "LightYellow=ffffe0|LightYellow1=ffffe0|LightYellow2=eeeed1|LightYellow3=cdcdb4|LightYellow4=8b8b7a|",
    "LimeGreen=32cd32|MediumAquamarine=66cdaa|MediumBlue=0000cd|MediumOrchid=ba55d3|MediumOrchid1=e066ff|",
    "MediumOrchid2=d15fee|MediumOrchid3=b452cd|MediumOrchid4=7a378b|MediumPurple=9370db|MediumPurple1=ab82ff|",
    "MediumPurple2=9f79ee|MediumPurple3=8968cd|MediumPurple4=5d478b|MediumSeaGreen=3cb371|MediumSlateBlue=7b68ee|",
    "MediumSpringGreen=00fa9a|MediumTurquoise=48d1cc|MediumVioletRed=c71585|MidnightBlue=191970|MintCream=f5fffa|",
    "MistyRose=ffe4e1|MistyRose1=ffe4e1|MistyRose2=eed5d2|MistyRose3=cdb7b5|MistyRose4=8b7d7b|NavajoWhite=ffdead|",
    "NavajoWhite1=ffdead|NavajoWhite2=eecfa1|NavajoWhite3=cdb38b|NavajoWhite4=8b795e|NavyBlue=000080|",
    "OldLace=fdf5e6|OliveDrab=6b8e23|OliveDrab1=c0ff3e|OliveDrab2=b3ee3a|OliveDrab3=9acd32|OliveDrab4=698b22|",
    "OrangeRed=ff4500|OrangeRed1=ff4500|OrangeRed2=ee4000|OrangeRed3=cd3700|OrangeRed4=8b2500|PaleGoldenrod=eee8aa|",
    "PaleGreen=98fb98|PaleGreen1=9aff9a|PaleGreen2=90ee90|PaleGreen3=7ccd7c|PaleGreen4=548b54|PaleTurquoise=afeeee|",
    "PaleTurquoise1=bbffff|PaleTurquoise2=aeeeee|PaleTurquoise3=96cdcd|PaleTurquoise4=668b8b|PaleVioletRed=db7093|",
    "PaleVioletRed1=ff82ab|PaleVioletRed2=ee799f|PaleVioletRed3=cd6889|PaleVioletRed4=8b475d|PapayaWhip=ffefd5|",
    "PeachPuff=ffdab9|PeachPuff1=ffdab9|PeachPuff2=eecbad|PeachPuff3=cdaf95|PeachPuff4=8b7765|PowderBlue=b0e0e6|",
    "RebeccaPurple=663399|RosyBrown=bc8f8f|RosyBrown1=ffc1c1|RosyBrown2=eeb4b4|RosyBrown3=cd9b9b|RosyBrown4=8b6969|",
    "RoyalBlue=4169e1|RoyalBlue1=4876ff|RoyalBlue2=436eee|RoyalBlue3=3a5fcd|RoyalBlue4=27408b|SaddleBrown=8b4513|",
    "SandyBrown=f4a460|SeaGreen=2e8b57|SeaGreen1=54ff9f|SeaGreen2=4eee94|SeaGreen3=43cd80|SeaGreen4=2e8b57|",
    "SkyBlue=87ceeb|SkyBlue1=87ceff|SkyBlue2=7ec0ee|SkyBlue3=6ca6cd|SkyBlue4=4a708b|SlateBlue=6a5acd|",
    "SlateBlue1=836fff|SlateBlue2=7a67ee|SlateBlue3=6959cd|SlateBlue4=473c8b|SlateGray=708090|SlateGray1=c6e2ff|",
    "SlateGray2=b9d3ee|SlateGray3=9fb6cd|SlateGray4=6c7b8b|SlateGrey=708090|SpringGreen=00ff7f|SpringGreen1=00ff7f|",
    "SpringGreen2=00ee76|SpringGreen3=00cd66|SpringGreen4=008b45|SteelBlue=4682b4|SteelBlue1=63b8ff|",
    "SteelBlue2=5cacee|SteelBlue3=4f94cd|SteelBlue4=36648b|VioletRed=d02090|VioletRed1=ff3e96|VioletRed2=ee3a8c|",
    "VioletRed3=cd3278|VioletRed4=8b2252|WebGray=808080|WebGreen=008000|WebGrey=808080|WebMaroon=800000|",
    "WebPurple=800080|WhiteSmoke=f5f5f5|X11Gray=bebebe|X11Green=00ff00|X11Grey=bebebe|X11Maroon=b03060|",
    "X11Purple=a020f0|YellowGreen=9acd32|alice blue=f0f8ff|antique white=faebd7|aqua=00ffff|aquamarine=7fffd4|",
    "aquamarine1=7fffd4|aquamarine2=76eec6|aquamarine3=66cdaa|aquamarine4=458b74|azure=f0ffff|azure1=f0ffff|",
    "azure2=e0eeee|azure3=c1cdcd|azure4=838b8b|beige=f5f5dc|bisque=ffe4c4|bisque1=ffe4c4|bisque2=eed5b7|",
    "bisque3=cdb79e|bisque4=8b7d6b|black=000000|blanched almond=ffebcd|blue violet=8a2be2|blue=0000ff|blue1=0000ff|",
    "blue2=0000ee|blue3=0000cd|blue4=00008b|brown=a52a2a|brown1=ff4040|brown2=ee3b3b|brown3=cd3333|brown4=8b2323|",
    "burlywood=deb887|burlywood1=ffd39b|burlywood2=eec591|burlywood3=cdaa7d|burlywood4=8b7355|cadet blue=5f9ea0|",
    "chartreuse=7fff00|chartreuse1=7fff00|chartreuse2=76ee00|chartreuse3=66cd00|chartreuse4=458b00|",
    "chocolate=d2691e|chocolate1=ff7f24|chocolate2=ee7621|chocolate3=cd661d|chocolate4=8b4513|coral=ff7f50|",
    "coral1=ff7256|coral2=ee6a50|coral3=cd5b45|coral4=8b3e2f|cornflower blue=6495ed|cornsilk=fff8dc|",
    "cornsilk1=fff8dc|cornsilk2=eee8cd|cornsilk3=cdc8b1|cornsilk4=8b8878|crimson=dc143c|cyan=00ffff|cyan1=00ffff|",
    "cyan2=00eeee|cyan3=00cdcd|cyan4=008b8b|dark blue=00008b|dark cyan=008b8b|dark goldenrod=b8860b|",
    "dark gray=a9a9a9|dark green=006400|dark grey=a9a9a9|dark khaki=bdb76b|dark magenta=8b008b|",
    "dark olive green=556b2f|dark orange=ff8c00|dark orchid=9932cc|dark red=8b0000|dark salmon=e9967a|",
    "dark sea green=8fbc8f|dark slate blue=483d8b|dark slate gray=2f4f4f|dark slate grey=2f4f4f|",
    "dark turquoise=00ced1|dark violet=9400d3|deep pink=ff1493|deep sky blue=00bfff|dim gray=696969|",
    "dim grey=696969|dodger blue=1e90ff|firebrick=b22222|firebrick1=ff3030|firebrick2=ee2c2c|firebrick3=cd2626|",
    "firebrick4=8b1a1a|floral white=fffaf0|forest green=228b22|fuchsia=ff00ff|gainsboro=dcdcdc|ghost white=f8f8ff|",
    "gold=ffd700|gold1=ffd700|gold2=eec900|gold3=cdad00|gold4=8b7500|goldenrod=daa520|goldenrod1=ffc125|",
    "goldenrod2=eeb422|goldenrod3=cd9b1d|goldenrod4=8b6914|green yellow=adff2f|green=00ff00|green1=00ff00|",
    "green2=00ee00|green3=00cd00|green4=008b00|honeydew=f0fff0|honeydew1=f0fff0|honeydew2=e0eee0|honeydew3=c1cdc1|",
    "honeydew4=838b83|hot pink=ff69b4|indian red=cd5c5c|indigo=4b0082|ivory=fffff0|ivory1=fffff0|ivory2=eeeee0|",
    "ivory3=cdcdc1|ivory4=8b8b83|khaki=f0e68c|khaki1=fff68f|khaki2=eee685|khaki3=cdc673|khaki4=8b864e|",
    "lavender blush=fff0f5|lavender=e6e6fa|lawn green=7cfc00|lemon chiffon=fffacd|light blue=add8e6|",
    "light coral=f08080|light cyan=e0ffff|light goldenrod yellow=fafad2|light goldenrod=eedd82|light gray=d3d3d3|",
    "light green=90ee90|light grey=d3d3d3|light pink=ffb6c1|light salmon=ffa07a|light sea green=20b2aa|",
    "light sky blue=87cefa|light slate blue=8470ff|light slate gray=778899|light slate grey=778899|",
    "light steel blue=b0c4de|light yellow=ffffe0|lime green=32cd32|lime=00ff00|linen=faf0e6|magenta=ff00ff|",
    "magenta1=ff00ff|magenta2=ee00ee|magenta3=cd00cd|magenta4=8b008b|maroon=b03060|maroon1=ff34b3|maroon2=ee30a7|",
    "maroon3=cd2990|maroon4=8b1c62|medium aquamarine=66cdaa|medium blue=0000cd|medium orchid=ba55d3|",
    "medium purple=9370db|medium sea green=3cb371|medium slate blue=7b68ee|medium spring green=00fa9a|",
    "medium turquoise=48d1cc|medium violet red=c71585|midnight blue=191970|mint cream=f5fffa|misty rose=ffe4e1|",
    "moccasin=ffe4b5|navajo white=ffdead|navy blue=000080|navy=000080|old lace=fdf5e6|olive drab=6b8e23|",
    "olive=808000|orange red=ff4500|orange=ffa500|orange1=ffa500|orange2=ee9a00|orange3=cd8500|orange4=8b5a00|",
    "orchid=da70d6|orchid1=ff83fa|orchid2=ee7ae9|orchid3=cd69c9|orchid4=8b4789|pale goldenrod=eee8aa|",
    "pale green=98fb98|pale turquoise=afeeee|pale violet red=db7093|papaya whip=ffefd5|peach puff=ffdab9|",
    "peru=cd853f|pink=ffc0cb|pink1=ffb5c5|pink2=eea9b8|pink3=cd919e|pink4=8b636c|plum=dda0dd|plum1=ffbbff|",
    "plum2=eeaeee|plum3=cd96cd|plum4=8b668b|powder blue=b0e0e6|purple=a020f0|purple1=9b30ff|purple2=912cee|",
    "purple3=7d26cd|purple4=551a8b|rebecca purple=663399|red=ff0000|red1=ff0000|red2=ee0000|red3=cd0000|",
    "red4=8b0000|rosy brown=bc8f8f|royal blue=4169e1|saddle brown=8b4513|salmon=fa8072|salmon1=ff8c69|",
    "salmon2=ee8262|salmon3=cd7054|salmon4=8b4c39|sandy brown=f4a460|sea green=2e8b57|seashell=fff5ee|",
    "seashell1=fff5ee|seashell2=eee5de|seashell3=cdc5bf|seashell4=8b8682|sienna=a0522d|sienna1=ff8247|",
    "sienna2=ee7942|sienna3=cd6839|sienna4=8b4726|silver=c0c0c0|sky blue=87ceeb|slate blue=6a5acd|",
    "slate gray=708090|slate grey=708090|snow=fffafa|snow1=fffafa|snow2=eee9e9|snow3=cdc9c9|snow4=8b8989|",
    "spring green=00ff7f|steel blue=4682b4|tan=d2b48c|tan1=ffa54f|tan2=ee9a49|tan3=cd853f|tan4=8b5a2b|teal=008080|",
    "thistle=d8bfd8|thistle1=ffe1ff|thistle2=eed2ee|thistle3=cdb5cd|thistle4=8b7b8b|tomato=ff6347|tomato1=ff6347|",
    "tomato2=ee5c42|tomato3=cd4f39|tomato4=8b3626|turquoise=40e0d0|turquoise1=00f5ff|turquoise2=00e5ee|",
    "turquoise3=00c5cd|turquoise4=00868b|violet red=d02090|violet=ee82ee|web gray=808080|web green=008000|",
    "web grey=808080|web maroon=800000|web purple=800080|wheat=f5deb3|wheat1=ffe7ba|wheat2=eed8ae|wheat3=cdba96|",
    "wheat4=8b7e66|white smoke=f5f5f5|white=ffffff|x11 gray=bebebe|x11 green=00ff00|x11 grey=bebebe|",
    "x11 maroon=b03060|x11 purple=a020f0|yellow green=9acd32|yellow=ffff00|yellow1=ffff00|yellow2=eeee00|",
    "yellow3=cdcd00|yellow4=8b8b00|",
);

fn colour_rgb(v: &str) -> Option<u32> {
    let lower = v.to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix("grey").or_else(|| lower.strip_prefix("gray")) {
        let n = if n.is_empty() { 0xbebebe } else {
            let n = n.parse::<u32>().ok().filter(|n| *n <= 100)?;
            let n = (2.55 * n as f64).round() as u32;
            n * 0x010101
        };
        return Some(n);
    }
    NAMED_COLOURS.split('|').filter_map(|s| s.split_once('='))
        .find(|(name, _)| name.eq_ignore_ascii_case(&lower)).and_then(|(_, hex)| u32::from_str_radix(hex, 16).ok())
}

fn valid_colour(v: &str) -> bool { normalize_colour(v).is_ok() }

fn normalize_colour(v: &str) -> Result<String, String> {
    let lower = v.to_ascii_lowercase();
    const BASIC: &[&str] = &["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
    if BASIC.contains(&lower.as_str()) || matches!(lower.as_str(), "default" | "terminal") {
        return Ok(lower);
    }
    if let Some(n) = lower.strip_prefix("bright") {
        if BASIC.contains(&n) { return Ok(lower); }
    }
    if let Some(n) = lower.strip_prefix("theme") {
        if BASIC.contains(&n) || matches!(n, "lightgrey" | "darkgrey") { return Ok(lower); }
    }
    if let Some(n) = lower.strip_prefix("colour").or_else(|| lower.strip_prefix("color")) {
        if let Ok(n) = n.parse::<u8>() { return Ok(format!("colour{n}")); }
    } else if let Ok(n) = lower.parse::<usize>() {
        if n < 8 { return Ok(BASIC[n].into()); }
        if n == 8 { return Ok("default".into()); }
        if n == 9 { return Ok("terminal".into()); }
        if (90..=97).contains(&n) { return Ok(format!("bright{}", BASIC[n - 90])); }
    }
    if lower.starts_with('#') && lower.len() == 7 && lower[1..].bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(lower);
    }
    if let Some(rgb) = colour_rgb(v) { return Ok(format!("#{rgb:06x}")); }
    Err(format!("bad colour: {v}"))
}

fn validate_style(v: &str) -> Result<(), String> {
    // tmux defers validation of style strings containing formats until expansion.
    if v.contains("#{") { return Ok(()); }
    let mut words = Vec::new();
    for word in v.split([',', ' ', '\n']).filter(|w| !w.is_empty()) {
        let lower = word.to_ascii_lowercase();
        if let Some((key, value)) = lower.split_once('=') {
            match key {
                "fg" | "bg" | "us" | "fill" => {
                    if !valid_colour(value) { return Err(format!("invalid style: {v}")); }
                    words.push(format!("{key}=default"));
                    continue;
                }
                "width" | "pad" | "dim" => {
                    let percent = value.ends_with('%');
                    let raw = value.strip_suffix('%').unwrap_or(value);
                    let max = if key == "dim" || (key == "width" && percent) { 100 } else { u32::MAX };
                    if (percent && key == "pad") || !raw.parse::<u32>().is_ok_and(|n| n <= max) {
                        return Err(format!("invalid style: {v}"));
                    }
                    continue;
                }
                "link" => continue,
                "list" if !["on", "focus", "left-marker", "right-marker"].contains(&value) => return Err(format!("invalid style: {v}")),
                _ => {}
            }
        }
        if ["noattr", "ignore", "noignore", "set-default", "nolink"].contains(&lower.as_str()) { continue; }
        words.push(lower);
    }
    crate::format::parse_style(&words.join(","), crate::cells::Style::default())
        .map(|_| ()).map_err(|_| format!("invalid style: {v}"))
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

    #[test]
    fn tmux_38_defaults_and_types() {
        let server = Options::defaults(Server);
        assert_eq!(server.get("theme"), Some("detect"));
        assert_eq!(server.get("clear-on-attach"), Some("on"));
        assert_eq!(server.get("input-buffer-size"), Some("1048576"));
        assert_eq!(server.get("dark-theme-blue"), Some("#{?#{e|>=:#{client_colours},256},skyblue3,blue}"));
        assert_eq!(server.get("light-theme-yellow"), Some("#{?#{e|>=:#{client_colours},256},darkgoldenrod,yellow}"));
        assert_eq!(server.get("command-alias[2]"), Some("server-info=show-messages -JT"));
        assert_eq!(server.get("terminal-overrides[0]"), Some("linux*:AX@"));
        let session = Options::defaults(Session);
        assert_eq!(session.get("mouse"), Some("on"));
        assert!(session.get("message-style").unwrap().contains("fill=themeyellow"));
        assert_eq!(session.get("status-format[0]"), Some(STATUS_FORMATS[0]));
        assert!(session.get("status-format[1]").unwrap().contains("#{P:"));
        assert!(session.get("status-format[2]").unwrap().contains("#{S:"));
        assert_eq!(session.get("update-environment[2]"), Some("MSYSTEM"));
        let window = Options::defaults(Window);
        assert_eq!(window.get("window-status-current-style"), Some("underscore"));
        assert_eq!(window.get("pane-scrollbars-style"), Some("bg=themedarkgrey,fg=themelightgrey,width=1,pad=0"));
        assert_eq!(window.get("allow-set-title"), Some("on"));
        assert_eq!(natural_scope("cursor-style"), Some(Window));
        assert_eq!(def("cursor-style").unwrap().scope, Pane);
        assert_eq!(natural_scope("display-panes-time"), Some(Window));
        assert_eq!(def("main-pane-width").unwrap().kind, Str);
        assert_eq!(validate("main-pane-width", Some("50%"), None).unwrap(), "50%");
        // Every table default must be accepted by its declared type.
        for d in DEFS {
            if d.kind != Array { assert!(validate(d.name, Some(d.default), None).is_ok(), "{}", d.name); }
        }
    }

    #[test]
    fn tmux_38_choices_and_validation() {
        for (name, good, bad) in [
            ("theme", "terminal", "auto"),
            ("get-clipboard", "both", "on"),
            ("copy-mode-line-numbers", "hybrid", "yes"),
            ("cursor-style", "blinking-bar", "beam"),
            ("pane-scrollbars", "auto-hide", "always"),
            ("pane-border-lines", "spaces", "rounded"),
            ("pane-border-status", "bottom-floating", "floating"),
            ("popup-border-lines", "padded", "number"),
            ("clock-mode-style", "24-with-seconds", "seconds"),
            ("destroy-unattached", "keep-group", "group"),
            ("remain-on-exit", "failed-key", "failure"),
            ("allow-passthrough", "all", "always"),
        ] {
            assert_eq!(validate(name, Some(good), None).unwrap(), good);
            assert!(validate(name, Some(bad), None).is_err(), "{name}");
        }
        assert_eq!(validate("pane-scrollbars", None, Some("on")).unwrap(), "on");
        assert_eq!(validate("destroy-unattached", None, Some("off")).unwrap(), "on");
        for (name, value) in [("input-buffer-size", "1048575"), ("prefix-timeout", "-1"), ("initial-repeat-time", "2000001"), ("tiled-layout-max-columns", "65536"), ("message-line", "5")] {
            assert!(validate(name, Some(value), None).is_err(), "{name}");
        }
        assert!(validate("cursor-colour", Some("nonsense"), None).is_err());
        assert!(validate("pane-scrollbars-style", Some("width=oops"), None).is_err());
        assert!(validate("prefix", Some("bad-key"), None).is_err());
        assert!(validate("default-client-command", Some("'unterminated"), None).is_err());
        assert_eq!(validate("cursor-colour", Some("#{@cursor}"), None).unwrap(), "#{@cursor}");
        assert_eq!(validate("pane-colours[10]", Some("IndianRed4"), None).unwrap(), "#8b3a3a");
        assert!(validate("pane-colours[0]", Some("colour256"), None).is_err());
    }

    #[test]
    fn tmux_38_arrays() {
        assert_eq!(canonical_name("pane-colours[0010]").unwrap(), "pane-colours[10]");
        assert_eq!(canonical_name("codepoint-widths[emoji]").unwrap(), "codepoint-widths[emoji]");
        for name in ["mouse[0]", "pane-colours[]", "pane-colours[4294967296]", "pane-colours[1]tail", "pane-colours[1"] {
            assert!(canonical_name(name).is_err(), "{name}");
        }
        let mut o = Options::defaults(Server);
        o.assign_array("command-alias", "one=display-message a,two=display-message b", false).unwrap();
        assert_eq!(o.get("command-alias[0]"), Some("one=display-message a"));
        o.unset("command-alias[0]");
        o.assign_array("command-alias", "three=list-sessions", true).unwrap();
        assert_eq!(o.get("command-alias[0]"), Some("three=list-sessions"));
        assert_eq!(o.get("command-alias[1]"), Some("two=display-message b"));
        o.reset("command-alias");
        assert_eq!(o.get("command-alias[0]"), Some("split-pane=split-window"));
        let mut session = Options::defaults(Session);
        session.assign_array("status-format", "custom", false).unwrap();
        session.reset("status-format");
        assert_eq!(session.get("status-format[1]"), Some(STATUS_FORMATS[1]));
        let mut window = Options::defaults(Window);
        window.assign_array("pane-colours", "red blue,colour42", false).unwrap();
        assert_eq!(window.get("pane-colours[2]"), Some("colour42"));
        assert!(window.assign_array("pane-colours", "bad-colour", false).is_err());
        assert_eq!(window.get("pane-colours[0]"), Some("red"));
    }


    #[test]
    fn tmux_38_name_matching() {
        assert_eq!(canonical_name("mous").unwrap(), "mouse");
        assert_eq!(canonical_name("display-panes-color").unwrap(), "display-panes-colour");
        assert_eq!(canonical_name("pane-colors[3]").unwrap(), "pane-colours[3]");
        assert_eq!(canonical_name("status").unwrap(), "status");
        assert_eq!(canonical_name("@user[x]").unwrap_err(), "not an array: @user[x]");
        assert_eq!(canonical_name("status-l").unwrap_err(), "ambiguous option: status-l");
        assert_eq!(canonical_name("codepoint-widths[").unwrap_err(), "ambiguous option: codepoint-widths[");
        assert_eq!(canonical_name("no-such").unwrap_err(), "invalid option: no-such");
        assert_eq!(validate("status-left", None, None).unwrap_err(), "empty value");
        assert_eq!(validate("@user", None, None).unwrap_err(), "empty value");
        assert!(validate("default-size", Some("120x40"), None).is_ok());
        assert_eq!(validate("default-size", Some("x40"), None).unwrap_err(), "value is invalid: x40");
        assert_eq!(def("status-style").unwrap().append_separator(), ",");
        assert_eq!(def("status-left").unwrap().append_separator(), "");
    }

    #[test]
    fn tmux_option_printing() {
        assert_eq!(escape_arg("!\"#$%&"), "\"!\\\"#$%&\"");
        assert_eq!(escape_arg("a$b ${x} $"), "\"a\\$b \\${x} $\"");
        assert_eq!(escape_arg("~a\"b"), "'~a\"b'");
        assert_eq!(escape_arg("~a b"), "\"\\~a b\"");
        assert_eq!(escape_arg("~"), "\\~");
        assert_eq!(escape_arg("$"), "\\$");
        assert_eq!(escape_arg(" "), "\" \"");
        assert_eq!(escape_arg("a\nb\tc\x1b"), "a\\nb\\tc\\033");
        assert_eq!(print_value("dark-theme-black", "#{@black}"), "\"#{@black}\"");
        assert_eq!(print_value("pane-colours[0]", "#ff0000"), "#ff0000");
        assert_eq!(print_value("default-client-command", "new-session -A"), "new-session -A");
        assert_eq!(print_value("status-left", "a b"), "\"a b\"");
        assert_eq!(print_value("status-left", "a\"b"), "'a\"b'");
        assert_eq!(compare_names("pane-colours[2]", "pane-colours[10]"), std::cmp::Ordering::Less);
    }

}
