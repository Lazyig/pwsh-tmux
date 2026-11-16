//! Command-level behaviour checked through the CLI against a private server.

mod common;
use common::*;
use std::time::Duration;
use std::io::Write;
use std::process::Stdio;

fn ids(ctx: &Ctx, t: &str) -> Vec<String> {
    ctx.ok(&["list-panes", "-t", t, "-F", "#{pane_id}"]).lines().map(String::from).collect()
}

#[test]
fn options_and_formats() {
    let ctx = Ctx::new("opts");
    ctx.ok(&["new-session", "-d", "-s", "o", "-x", "100", "-y", "30"]);
    ctx.ok(&["set-option", "-g", "status-left", "LEFT"]);
    assert_eq!(ctx.ok(&["show-options", "-gv", "status-left"]).trim(), "LEFT");
    ctx.ok(&["set", "-g", "@user", "val ue"]);
    assert_eq!(ctx.fmt("o", "#{@user}"), "val ue");
    ctx.ok(&["setw", "-t", "o", "synchronize-panes", "on"]);
    assert_eq!(ctx.ok(&["show-window-options", "-v", "-t", "o", "synchronize-panes"]).trim(), "on");
    // flag without value toggles
    ctx.ok(&["set", "-g", "mouse"]);
    assert_eq!(ctx.ok(&["show", "-gv", "mouse"]).trim(), "off");
    ctx.ok(&["set", "-u", "-g", "mouse"]);
    assert_eq!(ctx.ok(&["show", "-gv", "mouse"]).trim(), "on");
    ctx.fails(&["set", "-g", "no-such-option", "1"]);
    ctx.fails(&["set", "-g", "base-index", "abc"]);
    ctx.ok(&["set", "-ag", "status-left", "+X"]);
    assert_eq!(ctx.ok(&["show", "-gv", "status-left"]).trim(), "LEFT+X");
    ctx.ok(&["set", "-o", "-q", "-g", "status-left", "ignored"]);
    assert_eq!(ctx.ok(&["show", "-gv", "status-left"]).trim(), "LEFT+X");
    // session option overrides global, -A shows inherited
    ctx.ok(&["set", "-t", "o", "status-right", "R"]);
    assert!(ctx.ok(&["show", "-t", "o"]).contains("status-right R"));
    assert!(ctx.ok(&["show", "-A", "-t", "o"]).contains("status-left*"));
    // formats
    assert_eq!(ctx.fmt("o", "#{?#{==:#{session_name},o},yes,no}"), "yes");
    assert_eq!(ctx.fmt("o", "#S:#I.#P #{window_panes}"), "o:0.0 1");
    assert_eq!(ctx.fmt("o", "#{=2:#{l:abcdef}}"), "ab");
    assert_eq!(ctx.fmt("o", "#{e|+|:2,3}"), "5");
    assert_eq!(ctx.fmt("o", "#{pane_width}x#{pane_height}"), "100x30");
    assert!(ctx.ok(&["display-message", "-a", "-t", "o"]).contains("session_name=o"));
}

#[test]
fn tmux_38_options_scopes_arrays_and_choices() {
    let ctx = Ctx::new("opts38");
    ctx.ok(&["new-session", "-d", "-s", "o"]);
    assert_eq!(ctx.ok(&["show", "-sv", "theme"]).trim(), "detect");
    ctx.ok(&["set", "-s", "theme", "light"]);
    assert_eq!(ctx.ok(&["show", "-sv", "theme"]).trim(), "light");
    ctx.fails(&["set", "-s", "theme", "auto"]);
    assert!(ctx.ok(&["show", "-gv", "status-format[1]"]).contains("#{P:"));
    ctx.ok(&["set", "-gw", "pane-border-status", "top-floating"]);
    ctx.ok(&["set", "-p", "-t", "o", "cursor-style", "bar"]);
    assert_eq!(ctx.ok(&["show", "-pv", "-t", "o", "cursor-style"]).trim(), "bar");
    assert_eq!(ctx.ok(&["show", "-gwv", "cursor-style"]).trim(), "default");
    ctx.ok(&["set", "-g", "pane-scrollbars", "auto-hide"]);
    assert_eq!(ctx.ok(&["show", "-gwv", "pane-scrollbars"]).trim(), "auto-hide");
    ctx.ok(&["set", "-gw", "pane-colours[0010]", "red"]);
    assert_eq!(ctx.ok(&["show", "-gwv", "pane-colours[10]"]).trim(), "red");
    ctx.fails(&["set", "-gw", "pane-colours[2]", "bad-colour"]);
    ctx.ok(&["set", "-s", "codepoint-widths[emoji]", "U+1F600=2"]);
    assert_eq!(ctx.ok(&["show", "-sv", "codepoint-widths[emoji]"]).trim(), "U+1F600=2");
    ctx.ok(&["set", "-s", "command-alias", "one=display-message hello,two=list-sessions"]);
    assert_eq!(ctx.ok(&["show", "-sv", "command-alias[0]"]).trim(), "one=display-message hello");
    ctx.ok(&["set", "-su", "command-alias[0]"]);
    ctx.ok(&["set", "-sa", "command-alias", "three=list-windows"]);
    assert_eq!(ctx.ok(&["show", "-sv", "command-alias[0]"]).trim(), "three=list-windows");
    ctx.ok(&["set", "-sgu", "command-alias"]);
    assert_eq!(ctx.ok(&["show", "-sv", "command-alias[0]"]).trim(), "split-pane=split-window");
    ctx.fails(&["set", "-g", "mouse[0]", "on"]);
    ctx.fails(&["show", "-s", "codepoint-widths[]"]);
    // names: unique prefixes and old -color spellings, like options_match
    ctx.ok(&["set", "-g", "display-panes-color", "red"]);
    assert_eq!(ctx.ok(&["show", "-gw", "display-panes-col"]).trim(), "display-panes-colour red");
    assert!(ctx.fails(&["set", "-g", "status-l", "x"]).contains("ambiguous option"));
    // -a appends to this level only; styles get a comma
    ctx.ok(&["set", "-at", "o", "status-left", "X"]);
    assert_eq!(ctx.ok(&["show", "-t", "o", "status-left"]).trim(), "status-left X");
    ctx.ok(&["set", "-ag", "status-style", "fg=red"]);
    assert_eq!(ctx.ok(&["show", "-gv", "status-style"]).trim(), "bg=themegreen,fg=themeblack,fg=red");
    // an array set on a session does not copy the global elements
    ctx.ok(&["set", "-t", "o", "status-format[1]", "X"]);
    assert_eq!(ctx.ok(&["show", "-t", "o", "status-format"]).trim(), "status-format[1] X");
    assert!(ctx.ok(&["show", "-A", "-t", "o"]).contains("mouse* on\n"));
    // listings follow the tmux table order, user options first
    ctx.ok(&["set", "-s", "@z", "1"]);
    let all = ctx.ok(&["show", "-s"]);
    assert!(all.starts_with("@z 1\nbackspace C-?\nbuffer-limit 50\nclear-on-attach on\ncommand-alias[0]"), "{all}");
    assert!(all.find("\ntheme light").unwrap() < all.find("dark-theme-black").unwrap());
    ctx.ok(&["set", "-s", "default-client-command", "new -A"]);
    assert_eq!(ctx.ok(&["show", "-sv", "default-client-command"]).trim(), "new-session -A");
    assert!(ctx.fails(&["set", "-g", "status-left"]).contains("empty value"));
}

#[test]
fn tmux_38_option_levels_targets_and_hooks() {
    let ctx = Ctx::new("opts38b");
    ctx.ok(&["new-session", "-d", "-s", "o"]);
    ctx.ok(&["split-window", "-d", "-t", "o"]);
    // -U clears the target window's panes with -g too
    ctx.ok(&["set", "-p", "-t", "o:0.0", "pane-border-style", "fg=red"]);
    ctx.ok(&["set", "-p", "-t", "o:0.1", "pane-border-style", "fg=blue"]);
    ctx.ok(&["set", "-w", "-t", "o", "pane-border-style", "fg=green"]);
    ctx.ok(&["set", "-gU", "pane-border-style"]);
    assert_eq!(ctx.ok(&["show", "-p", "-t", "o:0.0"]), "");
    assert_eq!(ctx.ok(&["show", "-p", "-t", "o:0.1"]), "");
    assert_eq!(ctx.ok(&["show", "-pA", "-t", "o:0.0", "pane-border-style"]).trim(), "pane-border-style* fg=green");
    // an array made by element sets stays, empty, and hides the global one
    ctx.ok(&["set", "-t", "o", "status-format[1]", "Y"]);
    ctx.ok(&["set", "-u", "-t", "o", "status-format[1]"]);
    assert_eq!(ctx.ok(&["show", "-t", "o"]).trim(), "status-format");
    assert_eq!(ctx.ok(&["show", "-At", "o", "status-format"]).trim(), "status-format");
    ctx.ok(&["set", "-w", "-t", "o", "pane-colours[3]", "red"]);
    ctx.ok(&["set", "-wU", "-t", "o", "pane-colours[3]"]);
    assert!(ctx.ok(&["show", "-w", "-t", "o"]).contains("\npane-colours\n"));
    // a failing whole-array assignment keeps what was assigned before the error
    ctx.ok(&["set", "-gw", "pane-colours", "red,blue"]);
    assert_eq!(ctx.fails(&["set", "-gw", "pane-colours", "red,nope"]).trim(), "bad colour: nope");
    assert_eq!(ctx.ok(&["show", "-gw", "pane-colours"]).trim(), "pane-colours[0] red");
    // show-options takes a key on any option and prints the plain value
    assert_eq!(ctx.ok(&["show", "-g", "mouse[0]"]).trim(), "mouse[0] on");
    assert_eq!(ctx.ok(&["show", "-gv", "mouse[0]"]).trim(), "on");
    ctx.ok(&["set", "-g", "@u", "x"]);
    assert_eq!(ctx.ok(&["show", "-g", "@u[0]"]).trim(), "@u[0] x");
    assert_eq!(ctx.fails(&["set", "-g", "mouse[0]", "on"]).trim(), "not an array: mouse[0]");
    assert_eq!(ctx.fails(&["set", "-qg", "mouse[0]", "on"]).trim(), "not an array: mouse[0]");
    // targets: "no such ..." errors, hidden by -q
    assert_eq!(ctx.ok(&["set", "-qt", "nosuch", "status-left", "x"]), "");
    assert_eq!(ctx.ok(&["show", "-qt", "nosuch"]), "");
    assert_eq!(ctx.ok(&["set", "-wq", "-t", "o:9", "aggressive-resize", "on"]), "");
    assert_eq!(ctx.fails(&["set", "-t", "nosuch", "status-left", "x"]).trim(), "no such session: nosuch");
    assert_eq!(ctx.fails(&["show", "-w", "-t", "o:9"]).trim(), "no such window: o:9");
    assert_eq!(ctx.fails(&["set", "-p", "-t", "%99", "pane-border-style", "fg=red"]).trim(), "no such pane: %99");
    // values
    assert_eq!(ctx.fails(&["set", "-g", "mouse", "true"]).trim(), "bad value: true");
    assert_eq!(ctx.fails(&["set", "-gw", "pane-border-lines", "center"]).trim(), "unknown value: center");
    ctx.ok(&["set", "-g", "base-index", " 5"]);
    assert_eq!(ctx.ok(&["show", "-gv", "base-index"]).trim(), "5");
    ctx.ok(&["set", "-g", "status-left-style", "fg=red"]);
    assert_eq!(ctx.fails(&["set", "-ag", "status-left-style", "align=center"]).trim(), "invalid style: fg=red,align=center");
    assert_eq!(ctx.ok(&["show", "-gv", "status-left-style"]).trim(), "fg=red");
    ctx.fails(&["set", "-g", "status-style", "range=user"]);
    for (key, shown) in [("C-@", "C-@"), ("^?", "C-?"), ("M-S-a", "M-S-a"), ("MouseDown1Pane", "MouseDown1Pane"), ("0x41", "A")] {
        ctx.ok(&["set", "-g", "prefix2", key]);
        assert_eq!(ctx.ok(&["show", "-gv", "prefix2"]).trim(), shown);
    }
    // command options: aliases, unknown commands and flags, { } blocks, args_print order
    for (value, shown) in [
        ("info", "show-messages -JT"),
        ("splitp -h", "split-window -h"),
        ("if -F 1 { display a } { display b }", "if-shell -F 1 { display-message a } { display-message b }"),
        ("lsk -Tprefix -1 -N -a", "list-keys -1Na -T prefix"),
        ("display a ; display b", "display-message a ; display-message b"),
    ] {
        ctx.ok(&["set", "-s", "default-client-command", value]);
        assert_eq!(ctx.ok(&["show", "-sv", "default-client-command"]).trim(), shown, "{value}");
    }
    assert_eq!(ctx.fails(&["set", "-s", "default-client-command", "nosuchcmd x"]).trim(), "unknown command: nosuchcmd");
    assert_eq!(ctx.fails(&["set", "-s", "default-client-command", "new -Q"]).trim(), "command new-session: unknown flag -Q");
    assert_eq!(ctx.fails(&["set", "-s", "default-client-command", "{ new }"]).trim(), "syntax error");
    assert_eq!(ctx.fails(&["set", "-s", "default-client-command", "neww { x }"]).trim(), "unknown command: x");
    assert_eq!(ctx.fails(&["set", "-s", "default-client-command", "neww { display a }"]).trim(), "command new-window: argument 1 must be \"string\"");
    assert!(ctx.fails(&["set", "-s", "default-client-command", "li"]).starts_with("ambiguous command: li, could be: link-window, list-buffers"));
    // hooks are options too
    ctx.ok(&["set", "-g", "after-new-window", "display hi"]);
    assert_eq!(ctx.ok(&["show", "-g", "after-new-window"]).trim(), "after-new-window[0] display-message hi");
    ctx.ok(&["set", "-g", "after-new-window[3]", "info"]);
    ctx.ok(&["set", "-gu", "after-new-window[0]"]);
    assert_eq!(ctx.ok(&["show", "-g", "after-new-window"]).trim(), "after-new-window[3] show-messages -JT");
    assert_eq!(ctx.ok(&["show", "-g", "after-split-window"]).trim(), "after-split-window");
    ctx.ok(&["set", "-t", "o", "session-renamed", "display x"]);
    assert_eq!(ctx.ok(&["show", "-t", "o", "session-renamed"]).trim(), "session-renamed[0] display-message x");
    assert_eq!(ctx.ok(&["show", "-H", "-t", "o"]).trim(), "status-format\nsession-renamed[0] display-message x");
    assert_eq!(ctx.ok(&["show", "-At", "o", "session-created"]).trim(), "session-created");
    assert!(!ctx.ok(&["show", "-t", "o"]).contains("session-renamed"));
    ctx.ok(&["set", "-g", "pane-exited", "display x"]);
    assert_eq!(ctx.fails(&["show", "-gw", "pane-c"]).trim(), "ambiguous option: pane-c");
    assert_eq!(ctx.fails(&["show", "-s", "com"]).trim(), "ambiguous option: com");
    // the hook runs from the option
    ctx.ok(&["set", "-g", "after-new-window", "set -g @ran yes"]);
    ctx.ok(&["new-window", "-d", "-t", "o"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@ran"]).trim(), "yes");
}

#[test]
fn buffers_and_paste() {
    let ctx = Ctx::new("buf");
    ctx.ok(&["new-session", "-d", "-s", "b"]);
    ctx.wait_prompt("b");
    ctx.ok(&["set-buffer", "-b", "one", "first"]);
    ctx.ok(&["set-buffer", "second"]);
    assert_eq!(ctx.ok(&["show-buffer", "-b", "one"]), "first");
    assert_eq!(ctx.ok(&["show-buffer"]), "second");
    let list = ctx.ok(&["list-buffers"]);
    assert!(list.contains("one: 5 bytes: \"first\"") && list.contains("buffer0: 6 bytes"), "{list}");
    ctx.ok(&["set-buffer", "-a", "-b", "one", "+more"]);
    assert_eq!(ctx.ok(&["show-buffer", "-b", "one"]), "first+more");
    ctx.ok(&["set-buffer", "-b", "one", "-n", "renamed"]);
    assert_eq!(ctx.ok(&["show-buffer", "-b", "renamed"]), "first+more");
    let file = std::env::temp_dir().join(format!("pt-buf-{}.txt", std::process::id()));
    let fpath = file.to_string_lossy().to_string();
    ctx.ok(&["save-buffer", "-b", "renamed", &fpath]);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "first+more");
    std::fs::write(&file, "loaded-text").unwrap();
    ctx.ok(&["load-buffer", "-b", "ld", &fpath]);
    assert_eq!(ctx.ok(&["show-buffer", "-b", "ld"]), "loaded-text");
    let _ = std::fs::remove_file(&file);
    // paste into the shell and run it
    ctx.ok(&["set-buffer", "-b", "cmd", "echo pasted-42"]);
    ctx.ok(&["paste-buffer", "-b", "cmd", "-t", "b", "-d"]);
    ctx.ok(&["send-keys", "-t", "b", "Enter"]);
    wait_for("pasted output", || ctx.capture("b").matches("pasted-42").count() >= 2);
    ctx.fails(&["show-buffer", "-b", "cmd"]);
    ctx.ok(&["delete-buffer", "-b", "ld"]);
    ctx.fails(&["show-buffer", "-b", "ld"]);
}

#[test]
fn binary_stdin_buffers_cannot_inject_commands() {
    let ctx = Ctx::new("stdin-bytes");
    ctx.ok(&["new-session", "-d", "-s", "buffer"]);
    let mut data: Vec<u8> = (0..=255).collect();
    data.extend_from_slice(b"\0;\0set-option\0-g\0@injected\0yes\\;");
    let mut child = ctx.command().args(["load-buffer", "-", ";", "set-option", "-g", "@after-load", "done"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(&data).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert!(ctx.ok(&["show-options", "-gqv", "@injected"]).is_empty());
    assert_eq!(ctx.ok(&["show-options", "-gqv", "@after-load"]).trim(), "done");
    let output = std::env::temp_dir().join(format!("{}-buffer.bin", ctx.socket));
    ctx.ok(&["save-buffer", output.to_str().unwrap()]);
    let saved = std::fs::read(&output).unwrap();
    std::fs::remove_file(&output).unwrap();
    assert_eq!(saved, data);
}

#[test]
fn oversized_stdin_does_not_execute_any_queued_command() {
    let ctx = Ctx::new("stdin-limit");
    ctx.ok(&["new-session", "-d", "-s", "buffer"]);
    ctx.ok(&["set-buffer", "original"]);
    let mut child = ctx.command().args(["set-option", "-g", "@oversize-executed", "yes", ";", "load-buffer", "-"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    // Base64 fills the frame budget; the argument overhead must count too.
    let _ = child.stdin.take().unwrap().write_all(&vec![b'x'; (1 << 20) / 4 * 3]);
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("command frame too large"));
    assert!(ctx.ok(&["show-options", "-gqv", "@oversize-executed"]).is_empty());
    assert_eq!(ctx.ok(&["show-buffer"]), "original");
}

#[test]
fn invalid_window_destinations_leave_sessions_unchanged() {
    let ctx = Ctx::new("invalid-destination");
    ctx.ok(&["new-session", "-d", "-s", "source"]);
    ctx.ok(&["new-session", "-d", "-s", "destination"]);
    let before = ctx.ok(&["list-windows", "-a", "-F", "#{session_name}:#{window_index}:#{window_id}"]);
    for command in ["move-window", "link-window"] {
        let error = ctx.fails(&[command, "-s", "source:0", "-t", "does-not-exist:5"]);
        assert!(error.contains("can't find session"), "{error}");
        ctx.ok(&["has-session", "-t", "source"]);
        assert_eq!(ctx.ok(&["list-windows", "-a", "-F", "#{session_name}:#{window_index}:#{window_id}"]), before);
    }
    // A numeric window index without a session is still a valid target.
    ctx.ok(&["link-window", "-s", "source:0", "-t", "5"]);
    assert_eq!(ctx.fmt("destination:5", "#{window_id}"), ctx.fmt("source:0", "#{window_id}"));
}

#[test]
fn relative_startup_config_uses_the_client_directory() {
    let ctx = Ctx::new("relative-config");
    let directory = std::env::temp_dir().join(format!("{} config directory", ctx.socket));
    std::fs::create_dir(&directory).unwrap();
    let config = directory.join("project settings.conf");
    std::fs::write(&config, "set-option -g @review-config loaded\n").unwrap();
    let result = ctx.command().current_dir(&directory).args(["-f", "project settings.conf", "new-session", "-d", "-s", "review", "-c", env!("CARGO_MANIFEST_DIR")]).output().unwrap();
    let configured = if result.status.success() { ctx.ok(&["show-options", "-gqv", "@review-config"]) } else { String::new() };
    std::fs::remove_file(&config).unwrap();
    std::fs::remove_dir(&directory).unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(configured.trim(), "loaded");
}

#[test]
fn copy_mode_commands() {
    let ctx = Ctx::new("copy");
    ctx.ok(&["new-session", "-d", "-s", "c", "-x", "80", "-y", "24"]);
    ctx.wait_prompt("c");
    ctx.ok(&["send-keys", "-t", "c", "echo copy-me-123", "Enter"]);
    wait_for("output", || ctx.capture("c").matches("copy-me-123").count() >= 2);
    ctx.ok(&["copy-mode", "-t", "c"]);
    assert_eq!(ctx.fmt("c", "#{pane_in_mode} #{pane_mode}"), "1 copy-mode");
    ctx.ok(&["send-keys", "-t", "c", "-X", "search-backward", "copy-me-123"]);
    ctx.ok(&["send-keys", "-t", "c", "-X", "select-line"]);
    ctx.ok(&["send-keys", "-t", "c", "-X", "copy-selection-and-cancel"]);
    assert_eq!(ctx.fmt("c", "#{pane_in_mode}"), "0");
    let buf = ctx.ok(&["show-buffer"]);
    assert!(buf.contains("copy-me-123"), "{buf:?}");
    // key names sent to a pane in copy mode drive the mode (q quits)
    ctx.ok(&["copy-mode", "-t", "c"]);
    ctx.ok(&["send-keys", "-t", "c", "q"]);
    assert_eq!(ctx.fmt("c", "#{pane_in_mode}"), "0");
    // view mode for command output from clock / clear-history
    ctx.ok(&["clock-mode", "-t", "c"]);
    assert_eq!(ctx.fmt("c", "#{pane_mode}"), "clock-mode");
    // without a client the key does not reach the mode (window_pane_key), and clock mode
    // takes no -X commands
    ctx.ok(&["send-keys", "-t", "c", "q"]);
    assert_eq!(ctx.fmt("c", "#{pane_in_mode}"), "1");
    assert!(ctx.fails(&["send-keys", "-t", "c", "-X", "cancel"]).contains("not in a mode"));
    ctx.ok(&["copy-mode", "-q", "-t", "c"]);
    assert_eq!(ctx.fmt("c", "#{pane_in_mode}"), "0");
    ctx.ok(&["clear-history", "-t", "c"]);
    assert_eq!(ctx.fmt("c", "#{history_size}"), "0");
}

#[test]
fn compatible_layout_history_and_pane_indices() {
    let ctx = Ctx::new("compat-layout");
    ctx.ok(&["new-session", "-d", "-s", "s", "-x", "121", "-y", "41"]);
    assert_eq!(ctx.fmt("s", "#{pane_height}"), "41");
    ctx.ok(&["split-window", "-d", "-h", "-t", "s:0.0", "-l", "30"]);
    ctx.ok(&["split-window", "-d", "-v", "-t", "s:0.1", "-l", "10"]);
    let ids_before = ids(&ctx, "s");
    let left = ctx.fmt("s:0.0", "#{pane_width}");
    ctx.ok(&["select-layout", "-E", "-t", "s:0.1"]);
    assert_eq!(ctx.fmt("s:0.0", "#{pane_width}"), left);
    assert_eq!(ctx.fmt("s:0.1", "#{pane_height}"), "20");
    assert_eq!(ctx.fmt("s:0.2", "#{pane_height}"), "20");
    let spread = ctx.fmt("s", "#{window_layout}");
    ctx.ok(&["select-layout", "-t", "s", "main-vertical-mirrored"]);
    assert_eq!(ids(&ctx, "s"), ids_before, "layout geometry must not change pane indices");
    assert_eq!(ctx.fmt("s:0.0", "#{pane_left} #{pane_width}"), "41 80");
    ctx.ok(&["select-layout", "-o", "-t", "s"]);
    assert_eq!(ctx.fmt("s", "#{window_layout}"), spread);
    ctx.ok(&["select-layout", "-o", "-t", "s"]);
    // Restoring a JSON layout puts each pane back in the cell of its index (tmux 3.8).
    assert_eq!(ctx.fmt("s:0.0", "#{pane_left} #{pane_width}"), "41 80");
    ctx.ok(&["select-layout", "-t", "s", "even-horiz"]);
    ctx.fails(&["select-layout", "-t", "s", "even-"]);
}

#[test]
fn format_loops_and_utf8_length() {
    let ctx = Ctx::new("compat-format");
    ctx.ok(&["new-session", "-d", "-s", "z"]);
    ctx.ok(&["new-session", "-d", "-s", "a"]);
    ctx.ok(&["split-window", "-d", "-t", "z:0.0"]);
    // "z:" not "z": a bare name tries the current session's windows first, and the
    // shell's window (e.g. "zsh") would match it, as in tmux
    assert_eq!(ctx.fmt("z:", "#{S:#S;}"), "z;a;");
    assert_eq!(ctx.fmt("z:", "#{S/n:#S;}"), "a;z;");
    assert_eq!(ctx.fmt("z:", "#{P:#P,[#P]}"), "[0]1");
    assert_eq!(ctx.fmt("z:", "#{S:#{W:#{P:#S:#I.#P;}}}"), "z:0.0;z:0.1;a:0.0;");
    ctx.ok(&["set", "-g", "@text", "가나"]);
    assert_eq!(ctx.fmt("z:", "#{n:@text} #{s/./X/:@text}"), "6 XX");
}

#[test]
fn layouts_swaps_and_moves() {
    let ctx = Ctx::new("layout");
    ctx.ok(&["new-session", "-d", "-s", "l", "-x", "121", "-y", "41"]);
    for _ in 0..3 {
        ctx.ok(&["split-window", "-t", "l"]);
    }
    assert_eq!(ids(&ctx, "l").len(), 4);
    ctx.ok(&["select-layout", "-t", "l", "even-horizontal"]);
    let widths: Vec<u32> = ctx.ok(&["lsp", "-t", "l", "-F", "#{pane_width}"]).lines().map(|w| w.parse().unwrap()).collect();
    assert!(widths.iter().max().unwrap() - widths.iter().min().unwrap() <= 1, "{widths:?}");
    ctx.ok(&["select-layout", "-t", "l", "tiled"]);
    let layout = ctx.fmt("l", "#{window_layout}");
    ctx.ok(&["select-layout", "-t", "l", "main-vertical"]);
    ctx.ok(&["select-layout", "-t", "l", &layout]);
    assert_eq!(ctx.fmt("l", "#{window_layout}"), layout);
    ctx.ok(&["next-layout", "-t", "l"]);
    // swap and rotate
    let before = ids(&ctx, "l");
    ctx.ok(&["swap-pane", "-s", &before[0], "-t", &before[1]]);
    let after = ids(&ctx, "l");
    assert_eq!((after[0].clone(), after[1].clone()), (before[1].clone(), before[0].clone()));
    ctx.ok(&["rotate-window", "-t", "l"]);
    let rotated = ids(&ctx, "l");
    assert_eq!(rotated[0], after[1]);
    // resize and zoom
    ctx.ok(&["select-layout", "-t", "l", "even-horizontal"]);
    let first = rotated[0].clone();
    let w0: u32 = ctx.fmt(&first, "#{pane_width}").parse().unwrap();
    ctx.ok(&["resize-pane", "-t", &first, "-R", "5"]);
    let w1: u32 = ctx.fmt(&first, "#{pane_width}").parse().unwrap();
    assert_eq!(w1, w0 + 5);
    ctx.ok(&["resize-pane", "-t", &first, "-x", "50"]);
    assert_eq!(ctx.fmt(&first, "#{pane_width}"), "50");
    ctx.ok(&["resize-pane", "-Z", "-t", &first]);
    assert_eq!(ctx.fmt("l", "#{window_zoomed_flag} #{pane_width}"), "1 121");
    ctx.ok(&["resize-pane", "-Z", "-t", &first]);
    // break and join
    ctx.ok(&["break-pane", "-s", &first, "-d"]);
    assert_eq!(ctx.ok(&["lsw", "-t", "l"]).lines().count(), 2);
    ctx.ok(&["join-pane", "-s", &first, "-t", "l:0", "-h"]);
    assert_eq!(ctx.ok(&["lsw", "-t", "l"]).lines().count(), 1);
    assert_eq!(ids(&ctx, "l").len(), 4);
    // kill-pane -a keeps only the target
    ctx.ok(&["kill-pane", "-a", "-t", &first]);
    assert_eq!(ids(&ctx, "l"), vec![first.clone()]);
    // select-pane -T sets the title, -m marks (after the shell has started: a
    // prompt that sets the title by OSC 2 would overwrite it)
    ctx.wait_prompt(&first);
    ctx.ok(&["select-pane", "-t", &first, "-T", "my title"]);
    assert_eq!(ctx.fmt(&first, "#{pane_title}"), "my title");
    ctx.ok(&["select-pane", "-m", "-t", &first]);
    assert_eq!(ctx.fmt(&first, "#{pane_marked}"), "1");
}

#[test]
fn windows_and_sessions() {
    let ctx = Ctx::new("win");
    ctx.ok(&["new-session", "-d", "-s", "a", "-n", "zero"]);
    ctx.ok(&["new-window", "-t", "a", "-n", "one"]);
    ctx.ok(&["new-window", "-t", "a", "-n", "two", "-d"]);
    assert_eq!(ctx.fmt("a", "#{window_index}"), "1");
    ctx.ok(&["move-window", "-s", "a:two", "-t", "a:7"]);
    assert_eq!(ctx.ok(&["lsw", "-t", "a", "-F", "#{window_index}"]).lines().collect::<Vec<_>>(), vec!["0", "1", "7"]);
    ctx.ok(&["swap-window", "-s", "a:0", "-t", "a:7"]);
    assert_eq!(ctx.fmt("a:0", "#{window_name}"), "two");
    ctx.ok(&["move-window", "-r", "-t", "a"]);
    assert_eq!(ctx.ok(&["lsw", "-t", "a", "-F", "#{window_index}:#{window_name}"]).lines().collect::<Vec<_>>(), vec!["0:two", "1:one", "2:zero"]);
    ctx.ok(&["select-window", "-t", "a:2"]);
    ctx.ok(&["last-window", "-t", "a"]);
    assert_eq!(ctx.fmt("a", "#{window_index}"), "1");
    ctx.ok(&["next-window", "-t", "a"]);
    assert_eq!(ctx.fmt("a", "#{window_index}"), "2");
    ctx.ok(&["previous-window", "-t", "a"]);
    assert_eq!(ctx.fmt("a", "#{window_index}"), "1");
    ctx.ok(&["rename-window", "-t", "a:1", "uno"]);
    assert_eq!(ctx.fmt("a:1", "#W #{automatic-rename}"), "uno 0");
    // find-window opens tree mode filtered to the matches (tmux 3.8)
    ctx.ok(&["find-window", "-t", "a", "-N", "zero"]);
    assert_eq!(ctx.fmt("a", "#{pane_mode}"), "tree-mode");
    ctx.ok(&["copy-mode", "-q", "-t", "a"]);
    // link into another session
    ctx.ok(&["new-session", "-d", "-s", "b"]);
    ctx.ok(&["link-window", "-s", "a:uno", "-t", "b:5"]);
    assert_eq!(ctx.fmt("b:5", "#{window_name} #{window_linked}"), "uno 1");
    ctx.ok(&["unlink-window", "-t", "b:5"]);
    assert_eq!(ctx.ok(&["lsw", "-t", "b"]).lines().count(), 1);
    // base-index and renumber-windows
    ctx.ok(&["set", "-g", "base-index", "1"]);
    ctx.ok(&["set", "-g", "renumber-windows", "on"]);
    ctx.ok(&["new-session", "-d", "-s", "c"]);
    assert_eq!(ctx.fmt("c", "#{window_index}"), "1");
    ctx.ok(&["new-window", "-t", "c"]);
    ctx.ok(&["new-window", "-t", "c"]);
    ctx.ok(&["kill-window", "-t", "c:2"]);
    assert_eq!(ctx.ok(&["lsw", "-t", "c", "-F", "#I"]).lines().collect::<Vec<_>>(), vec!["1", "2"]);
    // sessions
    ctx.ok(&["rename-session", "-t", "c", "cee"]);
    ctx.ok(&["has-session", "-t", "cee"]);
    ctx.fails(&["new-session", "-d", "-s", "cee"]);
    ctx.ok(&["kill-session", "-a", "-t", "cee"]);
    assert_eq!(ctx.ok(&["ls", "-F", "#S"]).trim(), "cee");
    // prefix matching and $id targets
    let id = ctx.fmt("ce", "#{session_id}");
    assert_eq!(ctx.fmt(&id, "#S"), "cee");
    ctx.fails(&["bogus-command"]);
}

#[test]
fn hooks_shell_and_wait() {
    let ctx = Ctx::new("hooks");
    ctx.ok(&["new-session", "-d", "-s", "h"]);
    ctx.ok(&["set-hook", "-g", "after-new-window", "set -g @hooked yes"]);
    ctx.ok(&["new-window", "-t", "h"]);
    assert_eq!(ctx.fmt("h", "#{@hooked}"), "yes");
    assert!(ctx.ok(&["show-hooks", "-g"]).contains("after-new-window"));
    ctx.ok(&["set-hook", "-gu", "after-new-window"]);
    // run-shell output goes to stdout for CLI clients
    assert_eq!(ctx.ok(&["run-shell", "echo from-shell"]).trim(), "from-shell");
    ctx.ok(&["if-shell", "exit 0", "set -g @if yes", "set -g @if no"]);
    assert_eq!(ctx.fmt("h", "#{@if}"), "yes");
    ctx.ok(&["if-shell", "exit 1", "set -g @if yes", "set -g @if no"]);
    assert_eq!(ctx.fmt("h", "#{@if}"), "no");
    ctx.ok(&["if-shell", "-F", "#{==:#S,h}", "set -g @iff match"]);
    assert_eq!(ctx.fmt("h", "#{@iff}"), "match");
    // command lists with ;
    ctx.ok(&["set", "-g", "@a", "1", ";", "set", "-g", "@b", "2"]);
    assert_eq!(ctx.fmt("h", "#{@a}#{@b}"), "12");
    // wait-for blocks until signalled
    let mut waiter = ctx.command().args(["wait-for", "chan"]).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(waiter.try_wait().unwrap().is_none(), "wait-for returned early");
    ctx.ok(&["wait-for", "-S", "chan"]);
    wait_for("waiter exit", || waiter.try_wait().unwrap().is_some());
    // a signal sent before anyone waits is kept for the next wait (tmux behaviour) ...
    ctx.ok(&["wait-for", "-S", "early"]);
    let mut late = ctx.command().args(["wait-for", "early"]).spawn().unwrap();
    wait_for("late waiter returns at once", || late.try_wait().unwrap().is_some());
    // ... but only once
    let mut again = ctx.command().args(["wait-for", "early"]).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(again.try_wait().unwrap().is_none(), "a signal was consumed twice");
    ctx.ok(&["wait-for", "-S", "early"]);
    wait_for("second waiter exit", || again.try_wait().unwrap().is_some());
    // environment
    ctx.ok(&["set-environment", "-t", "h", "FOO_PT", "bar"]);
    assert!(ctx.ok(&["show-environment", "-t", "h", "FOO_PT"]).contains("FOO_PT=bar"));
    ctx.ok(&["set-environment", "-t", "h", "-u", "FOO_PT"]);
    assert!(!ctx.ok(&["show-environment", "-t", "h"]).contains("FOO_PT"));
}

#[test]
fn keys_config_and_capture() {
    let ctx = Ctx::new("keys");
    ctx.ok(&["new-session", "-d", "-s", "k"]);
    ctx.ok(&["bind-key", "-T", "prefix", "y", "display-message", "hi"]);
    assert!(ctx.ok(&["list-keys", "-T", "prefix", "y"]).contains("display-message hi"));
    ctx.ok(&["unbind-key", "y"]);
    assert!(!ctx.ok(&["list-keys", "-T", "prefix"]).contains("display-message hi"));
    assert!(ctx.ok(&["list-keys", "-N"]).contains("Split window horizontally"));
    // config file in tmux syntax
    let conf = std::env::temp_dir().join(format!("pt-conf-{}.conf", std::process::id()));
    std::fs::write(
        &conf,
        "# comment\nset -g @conf loaded\nbind -n F5 { set -g @f5 pressed ; display done }\nbind -T mytab z if -F 1 { display a } 'display b'\nbind -T mytab y display-menu -T x A a {send -X history-top} B b { new-window }\nset -g status-right '#[fg=yellow]%H:%M'\n%if #{==:#{host},#{host}}\nset -g @cond yes\n%endif\n",
    )
    .unwrap();
    ctx.ok(&["source-file", &conf.to_string_lossy()]);
    let _ = std::fs::remove_file(&conf);
    assert_eq!(ctx.fmt("k", "#{@conf} #{@cond}"), "loaded yes");
    // list-keys prints the parsed command list: full names, " \; " between commands
    assert!(ctx.ok(&["list-keys", "-T", "root", "F5"]).contains("set-option -g @f5 pressed \\; display-message done"));
    // blocks among bind-key's arguments stay blocks
    assert_eq!(
        ctx.ok(&["list-keys", "-T", "mytab"]),
        "bind-key  -T mytab y display-menu -T x A a { send-keys -X history-top } B b { new-window }\nbind-key  -T mytab z if-shell -F 1 { display-message a } \"display b\"\n"
    );
    ctx.fails(&["source-file", "/no/such/file.conf"]);
    ctx.ok(&["source-file", "-q", "/no/such/file.conf"]);
    // #{current_file} and assignments used by the next lines, as theme plugins
    // (catppuccin) rely on: source -F "#{d:current_file}/${MOD}/x.conf"
    let dir = std::env::temp_dir().join(format!("pt-cf-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(
        dir.join("main.conf"),
        "%hidden MOD=\"sub\"\nset -g @mod \"${MOD}\"\nsource -F \"#{d:current_file}/${MOD}/inner.conf\"\nset -gF @after \"#{b:current_file}\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("sub").join("inner.conf"),
        "set -gF @inner \"#{b:current_file}\"\n%if \"#{==:#{b:current_file},inner.conf}\"\nset -g @inner_if yes\n%endif\n",
    )
    .unwrap();
    ctx.ok(&["source-file", &dir.join("main.conf").to_string_lossy()]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(ctx.fmt("k", "#{@mod} #{@inner} #{@inner_if} #{@after}"), "sub inner.conf yes main.conf");
    assert_eq!(ctx.fmt("k", "[#{current_file}] #{version}"), "[] 3.8");
    // send-keys variants and capture-pane
    ctx.wait_prompt("k");
    ctx.ok(&["send-keys", "-t", "k", "-l", "echo literal-Enter"]);
    ctx.ok(&["send-keys", "-t", "k", "-H", "0d"]);
    wait_for("literal", || ctx.capture("k").matches("literal-Enter").count() >= 2);
    ctx.ok(&["send-keys", "-t", "k", "-N", "3", "x"]);
    wait_for("repeat", || ctx.capture("k").contains("xxx"));
    ctx.ok(&["send-keys", "-t", "k", "C-u"]);
    let all = ctx.ok(&["capture-pane", "-p", "-t", "k", "-S", "-", "-E", "-"]);
    // the plain-text fast path agrees with the cell-based -N output, minus trailing blanks
    // (retried: the shell may still be redrawing between the two captures)
    wait_for("plain capture == -N capture", || {
        let plain = ctx.ok(&["capture-pane", "-p", "-t", "k", "-S", "-", "-E", "-"]);
        let cells = ctx.ok(&["capture-pane", "-p", "-N", "-t", "k", "-S", "-", "-E", "-"]);
        plain.lines().collect::<Vec<_>>() == cells.lines().map(str::trim_end).collect::<Vec<_>>()
    });
    assert!(all.contains("literal-Enter"));
    let esc = ctx.ok(&["capture-pane", "-p", "-e", "-t", "k"]);
    assert!(!esc.is_empty());
    ctx.ok(&["capture-pane", "-t", "k", "-b", "cap"]);
    assert!(ctx.ok(&["show-buffer", "-b", "cap"]).contains("literal-Enter"));
}

#[test]
fn respawn_and_remain_on_exit() {
    let ctx = Ctx::new("respawn");
    ctx.ok(&["new-session", "-d", "-s", "r"]);
    ctx.ok(&["set", "-g", "remain-on-exit", "on"]);
    ctx.wait_prompt("r");
    ctx.ok(&["send-keys", "-t", "r", "exit", "Enter"]);
    wait_for("pane dead", || ctx.fmt("r", "#{pane_dead}") == "1");
    assert!(ctx.capture("r").contains("Pane is dead"));
    ctx.ok(&["respawn-pane", "-t", "r"]);
    wait_for("pane alive", || ctx.fmt("r", "#{pane_dead}") == "0");
    ctx.fails(&["respawn-pane", "-t", "r"]); // still running without -k
    ctx.ok(&["respawn-pane", "-k", "-t", "r"]);
    ctx.ok(&["set", "-g", "remain-on-exit", "off"]);
    ctx.ok(&["split-window", "-t", "r"]);
    ctx.ok(&["respawn-window", "-k", "-t", "r"]);
    assert_eq!(ids(&ctx, "r").len(), 1);
}

#[test]
fn split_window_options() {
    let ctx = Ctx::new("split");
    ctx.ok(&["new-session", "-d", "-s", "s", "-x", "101", "-y", "41"]);
    let p = ctx.ok(&["split-window", "-h", "-l", "30", "-t", "s", "-P", "-F", "#{pane_id}"]).trim().to_string();
    assert_eq!(ctx.fmt(&p, "#{pane_width}"), "30");
    let q = ctx.ok(&["split-window", "-v", "-b", "-l", "25%", "-t", &p, "-P", "-F", "#{pane_id}"]).trim().to_string();
    assert_eq!(ctx.fmt(&q, "#{pane_top} #{pane_height}"), "0 10");
    let full = ctx.ok(&["split-window", "-f", "-v", "-t", "s", "-d", "-P", "-F", "#{pane_id}"]).trim().to_string();
    assert_eq!(ctx.fmt("s", "#{window_panes}"), "4");
    assert_eq!(ctx.fmt(&full, "#{pane_width}"), "101");
    // -c sets the directory
    let dir = std::env::temp_dir();
    let d = ctx.ok(&["split-window", "-t", "s", "-c", &dir.to_string_lossy(), "-P", "-F", "#{pane_start_path}"]);
    assert_eq!(std::path::Path::new(d.trim()), dir.as_path());
}

/// Scenarios from the code review; each used to panic the server or destroy the wrong window.
#[test]
fn review_regressions() {
    let ctx = Ctx::new("review");
    ctx.ok(&["new-session", "-d", "-s", "r", "-n", "w0"]);
    // respawn-window must not leave last-pane pointing at a destroyed pane
    ctx.ok(&["split-window", "-t", "r"]);
    ctx.ok(&["respawn-window", "-k", "-t", "r"]);
    ctx.fails(&["last-pane", "-t", "r"]);
    ctx.ok(&["swap-pane", "-U", "-t", "r"]);
    // kill-window -a with renumber-windows keeps the target window
    ctx.ok(&["set", "-g", "renumber-windows", "on"]);
    ctx.ok(&["new-window", "-t", "r", "-n", "w1"]);
    ctx.ok(&["new-window", "-t", "r", "-n", "w2"]);
    ctx.ok(&["kill-window", "-a", "-t", "r:w2"]);
    assert_eq!(ctx.ok(&["lsw", "-t", "r", "-F", "#W"]).trim(), "w2");
    // move-window -a inside one session
    ctx.ok(&["new-window", "-t", "r", "-n", "w3"]);
    ctx.ok(&["new-window", "-t", "r", "-n", "w4"]);
    ctx.ok(&["move-window", "-a", "-s", "r:w4", "-t", "r:w2"]);
    assert_eq!(ctx.ok(&["lsw", "-t", "r", "-F", "#W"]).lines().collect::<Vec<_>>(), vec!["w2", "w4", "w3"]);
    // a window linked twice into one session, then killed
    ctx.ok(&["link-window", "-s", "r:w3", "-t", "r:9"]);
    ctx.fails(&["kill-window", "-t", "r:w3"]); // ambiguous, like tmux
    ctx.ok(&["kill-window", "-t", "r:9"]);
    assert_eq!(ctx.ok(&["lsw", "-t", "r", "-F", "#W"]).lines().collect::<Vec<_>>(), vec!["w2", "w4"]);
    // new-window -k replacing the only window keeps the session
    ctx.ok(&["new-session", "-d", "-s", "solo"]);
    ctx.ok(&["new-window", "-k", "-t", "solo:0", "-n", "replaced"]);
    assert_eq!(ctx.fmt("solo", "#W"), "replaced");
    // join-pane from a single-pane session removes that session cleanly
    ctx.ok(&["join-pane", "-s", "solo:", "-t", "r:"]);
    ctx.fails(&["has-session", "-t", "solo"]);
    assert_eq!(ctx.fmt("r", "#{window_panes}"), "2");
    // run-shell / if-shell inside a sourced file run (asynchronously) in order
    let conf = std::env::temp_dir().join(format!("pt-rv-{}.conf", std::process::id()));
    // (a file with an unknown command runs none of its commands: tmux 3.8 parses the
    // whole file first)
    std::fs::write(&conf, "if-shell 'exit 0' 'set -g @rv yes' 'set -g @rv no'\nset -g @after 1\nset -g @tail ok\n").unwrap();
    ctx.ok(&["source-file", &conf.to_string_lossy()]);
    let _ = std::fs::remove_file(&conf);
    assert_eq!(ctx.fmt("r", "#{@rv} #{@after} #{@tail}"), "yes 1 ok");
    // load-buffer - reads stdin
    let mut child = ctx.command().args(["load-buffer", "-b", "stdin", "-"]).stdin(std::process::Stdio::piped()).spawn().unwrap();
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(b"from stdin").unwrap();
    }
    assert!(child.wait().unwrap().success());
    assert_eq!(ctx.ok(&["show-buffer", "-b", "stdin"]), "from stdin");
    // absurd sizes are clamped instead of exhausting memory
    ctx.ok(&["resize-window", "-t", "r", "-x", "60000", "-y", "60000"]);
    assert_eq!(ctx.fmt("r", "#{window_width}x#{window_height}"), "1000x1000");
    ctx.ok(&["resize-window", "-A", "-t", "r"]);
}

#[test]
fn format_shell_commands() {
    let ctx = Ctx::new("fshell");
    ctx.ok(&["new-session", "-d", "-s", "f"]);
    ctx.ok(&["set", "-g", "status-interval", "1"]);
    // runs in the background; later expansions show the cached last line
    wait_for("#() output", || ctx.fmt("f", "[#(echo shell-out)]") == "[shell-out]");
    // formats inside the command are expanded first
    wait_for("#() with format", || ctx.fmt("f", "#(echo #{session_name}-x)") == "f-x");
}

#[test]
fn session_groups() {
    let ctx = Ctx::new("groups");
    ctx.ok(&["new-session", "-d", "-s", "g1", "-n", "one"]);
    ctx.ok(&["new-window", "-t", "g1", "-n", "two"]);
    ctx.ok(&["new-session", "-d", "-t", "g1", "-s", "g2"]);
    let names = |s: &str| ctx.ok(&["lsw", "-t", s, "-F", "#W"]).lines().map(String::from).collect::<Vec<_>>();
    assert_eq!(names("g2"), vec!["one", "two"]);
    assert_eq!(ctx.fmt("g2", "#{session_grouped} #{session_group} #{session_group_size} #{session_group_list}"), "1 g1 2 g1,g2");
    // the current window is per session
    ctx.ok(&["select-window", "-t", "g2:0"]);
    assert_eq!(ctx.fmt("g1", "#W"), "two");
    assert_eq!(ctx.fmt("g2", "#W"), "one");
    // window list changes propagate both ways
    ctx.ok(&["new-window", "-t", "g2", "-n", "three"]);
    assert_eq!(names("g1"), vec!["one", "two", "three"]);
    ctx.ok(&["kill-window", "-t", "g1:three"]);
    assert_eq!(names("g2"), vec!["one", "two"]);
    ctx.ok(&["rename-window", "-t", "g1:one", "uno"]);
    assert_eq!(names("g2"), vec!["uno", "two"]);
    // killing one member keeps the windows for the other
    ctx.ok(&["kill-session", "-t", "g1"]);
    assert_eq!(names("g2"), vec!["uno", "two"]);
    assert_eq!(ctx.fmt("g2", "#{session_grouped} #{session_group} #{session_group_size}"), "1 g1 1");
}

#[test]
fn control_mode() {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::{Arc, Mutex};
    let ctx = Ctx::new("control");
    ctx.ok(&["new-session", "-d", "-s", "c"]);
    ctx.wait_prompt("c");
    let mut child = ctx
        .command()
        .args(["-C", "attach", "-t", "c"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let out = Arc::new(Mutex::new(String::new()));
    {
        let out = out.clone();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                out.lock().unwrap().push_str(&(line + "\n"));
            }
        });
    }
    let mut stdin = child.stdin.take().unwrap();
    let mut send = |l: &str| {
        stdin.write_all(format!("{l}\n").as_bytes()).unwrap();
        stdin.flush().unwrap();
    };
    let text = || out.lock().unwrap().clone();
    wait_for("attach reply", || text().contains("%end ") && text().contains("%session-changed $"));
    send("display-message -p 'hello #{session_name}'");
    wait_for("command block", || text().contains("hello c\n%end"));
    send("send-keys 'echo ctl-out-42' Enter");
    wait_for("%output", || text().lines().any(|l| l.starts_with("%output %") && l.contains("ctl-out-42")));
    send("new-window -n extra");
    wait_for("%window-add", || text().contains("%window-add @"));
    wait_for("%session-window-changed", || text().contains("%session-window-changed $"));
    send("rename-window renamed-win");
    wait_for("%window-renamed", || text().contains("renamed-win\n") && text().contains("%window-renamed @"));
    send("split-window");
    wait_for("%layout-change", || text().contains("%layout-change @"));
    // control clients get old-format layouts unless they set new-layouts (tmux 3.8)
    let change = text().lines().find(|l| l.starts_with("%layout-change @")).unwrap().to_string();
    assert!(change.split(' ').nth(2).is_some_and(|l| l.as_bytes().get(4) == Some(&b',')) && !change.contains("\"V\":2"), "{change}");
    send("display-message -p 'L=#{window_layout}'");
    wait_for("old layout", || text().lines().any(|l| l.starts_with("L=") && l.as_bytes().get(6) == Some(&b',')));
    // floating panes and zoom are layout changes too, though the old format leaves them out
    let changes = || text().lines().filter(|l| l.starts_with("%layout-change @")).count();
    let before = changes();
    send("new-pane -d -x 10 -y 4");
    wait_for("floating %layout-change", || changes() == before + 1);
    send("resize-pane -Z");
    wait_for("zoom %layout-change", || text().lines().any(|l| l.starts_with("%layout-change @") && l.ends_with(" *Z")));
    send("resize-pane -Z");
    wait_for("unzoom %layout-change", || changes() == before + 3);
    send("kill-pane -t :.2");
    wait_for("closed floating %layout-change", || changes() == before + 4);
    send("refresh-client -f new-layouts");
    send("display-message -p 'J=#{window_layout}'");
    wait_for("JSON layout", || text().contains("J={\"V\":2,\"L\":"));
    // switching the format is no layout change of its own, nor is selecting a pane
    let before = changes();
    send("select-pane -t :.0");
    send("refresh-client -f !new-layouts");
    send("display-message -p 'O=#{window_layout}'");
    wait_for("old layout again", || text().lines().any(|l| l.starts_with("O=") && l.as_bytes().get(6) == Some(&b',')));
    assert_eq!(changes(), before, "{}", text());
    send("kill-window");
    wait_for("%window-close", || text().contains("%window-close @"));
    send("no-such-command");
    wait_for("%error", || text().contains("%error "));
    send("refresh-client -C 120x40");
    // control clients have no status line: the window gets the whole size
    wait_for("control size", || ctx.fmt("c", "#{window_width}x#{window_height}") == "120x40");
    // an empty line detaches
    send("");
    wait_for("%exit", || text().contains("%exit"));
    wait_for("client exit", || child.try_wait().unwrap().is_some());
    assert!(ctx.ok(&["ls"]).contains("c: 1 windows"));
}

#[test]
fn control_mode_flow_and_subscriptions() {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::{Arc, Mutex};
    let ctx = Ctx::new("ctlflow");
    ctx.ok(&["new-session", "-d", "-s", "c", "-n", "first"]);
    ctx.wait_prompt("c");
    let pane = ctx.fmt("c", "#{pane_id}");
    let mut child = ctx.command().args(["-C", "attach", "-t", "c"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    let out = Arc::new(Mutex::new(String::new()));
    {
        let out = out.clone();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                out.lock().unwrap().push_str(&(line + "\n"));
            }
        });
    }
    let mut stdin = child.stdin.take().unwrap();
    let mut send = |l: &str| {
        stdin.write_all(format!("{l}\n").as_bytes()).unwrap();
        stdin.flush().unwrap();
    };
    let text = || out.lock().unwrap().clone();
    let clear = || out.lock().unwrap().clear();
    wait_for("attached", || text().contains("%session-changed"));
    // pause / continue one pane
    send(&format!("refresh-client -A '{pane}:pause'"));
    wait_for("%pause", || text().contains(&format!("%pause {pane}")));
    clear();
    ctx.ok(&["send-keys", "-t", "c", "echo paused-out", "Enter"]);
    wait_for("pane output", || ctx.capture("c").matches("paused-out").count() >= 2);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!text().contains("%output"), "output while paused: {}", text());
    send(&format!("refresh-client -A '{pane}:continue'"));
    wait_for("%continue", || text().contains(&format!("%continue {pane}")));
    // no-output flag
    send("refresh-client -f no-output");
    std::thread::sleep(Duration::from_millis(300));
    clear();
    ctx.ok(&["send-keys", "-t", "c", "echo hidden-out", "Enter"]);
    wait_for("pane output", || ctx.capture("c").matches("hidden-out").count() >= 2);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!text().contains("%output"));
    // pause-after switches to %extended-output
    send("refresh-client -f '!no-output,pause-after=5'");
    std::thread::sleep(Duration::from_millis(300));
    ctx.ok(&["send-keys", "-t", "c", "echo ext-out", "Enter"]);
    wait_for("%extended-output", || text().lines().any(|l| l.starts_with(&format!("%extended-output {pane} ")) && l.contains(" : ")));
    // subscriptions report changes
    send("refresh-client -B 'wn::#{window_name}'");
    wait_for("subscription", || text().contains("%subscription-changed wn $") && text().contains(" : first"));
    ctx.ok(&["rename-window", "-t", "c", "second"]);
    wait_for("subscription change", || text().contains(" : second"));
    send("refresh-client -A 'bogus'");
    wait_for("%error", || text().contains("%error"));
    send("");
    wait_for("client exit", || child.try_wait().unwrap().is_some());
}

#[test]
fn server_access_commands() {
    let ctx = Ctx::new("access");
    ctx.ok(&["new-session", "-d", "-s", "a"]);
    assert_eq!(ctx.ok(&["server-access", "-l"]), "");
    assert!(ctx.fails(&["server-access", "-a", "no-such-user-pt-xyz"]).contains("unknown user"));
    let me = if cfg!(windows) { std::env::var("USERNAME").unwrap() } else { std::env::var("USER").unwrap_or_else(|_| "root".into()) };
    assert!(ctx.fails(&["server-access", "-a", &me]).contains("owns the server"));
    ctx.fails(&["server-access", "-w", "no-such-user-pt-xyz"]);
}

#[test]
fn readonly_control_client_cannot_execute_or_remove_restriction() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    let ctx = Ctx::new("readonly");
    ctx.ok(&["new-session", "-d", "-s", "ro"]);
    ctx.ok(&["set-option", "-g", "update-environment[0]", "READONLY_ENV_PROOF"]);
    ctx.ok(&["set-environment", "-t", "ro", "READONLY_ENV_PROOF", "original"]);
    let mut child = ctx.command().args(["-C", "attach-session", "-r", "-t", "ro"])
        .env("READONLY_ENV_PROOF", "forbidden")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
    let out = Arc::new(Mutex::new(String::new()));
    let reader_out = out.clone();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            reader_out.lock().unwrap().push_str(&(line + "\n"));
        }
    });
    let text = || out.lock().unwrap().clone();
    let mut stdin = child.stdin.take().unwrap();
    wait_for("readonly attach", || text().contains("%session-changed"));
    for (i, line) in [
        "refresh-client -f !read-only",
        "switch-client -r",
        "run-shell 'echo forbidden'",
        "send-keys 'echo forbidden' Enter",
        "capture-pane",
        "detach-client -a",
        "new-window",
    ].iter().enumerate() {
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
        wait_for("readonly command rejected", || text().matches("client is read-only").count() == i + 1);
    }
    writeln!(stdin, "attach-session -t ro").unwrap();
    writeln!(stdin, "display-message -p 'READONLY=#{{client_readonly}}'").unwrap();
    stdin.flush().unwrap();
    wait_for("readonly flag retained", || text().contains("READONLY=1"));
    // Each command has its own reply block. Each key row expands separately,
    // so this exceeds the reply limit without exceeding the format limit.
    let oversized = format!("list-keys -F '{}'", "#{p10000:#{l:x}}".repeat(100));
    writeln!(stdin, "{oversized}").unwrap();
    stdin.flush().unwrap();
    wait_for("oversized reply rejected", || text().contains("command output limit exceeded"));
    assert_eq!(ctx.ok(&["show-environment", "-t", "ro", "READONLY_ENV_PROOF"]).trim(), "READONLY_ENV_PROOF=original");
    // A writable connection still works and the forbidden command made no window.
    assert_eq!(ctx.fmt("ro", "#{session_windows}"), "1");
    ctx.ok(&["new-window", "-d", "-t", "ro"]);
    assert_eq!(ctx.fmt("ro", "#{session_windows}"), "2");
    writeln!(stdin, "detach-client").unwrap();
    stdin.flush().unwrap();
    wait_for("readonly detach", || child.try_wait().unwrap().is_some());
    let mut overflowing = ctx.command().args(["-C", "attach-session", "-r", "-t", "ro"])
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    wait_for("subscription client attach", || ctx.ok(&["list-clients"]).lines().count() == 1);
    let mut feed = overflowing.stdin.take().unwrap();
    let big = "#{p10000:#{l:x}}".repeat(100);
    writeln!(feed, "refresh-client -B 'first::{big}'").unwrap();
    writeln!(feed, "refresh-client -B 'second::{big}'").unwrap();
    feed.flush().unwrap();
    wait_for("subscription cache overflow disconnect", || overflowing.try_wait().unwrap().is_some());
    assert!(!overflowing.try_wait().unwrap().unwrap().success());
    assert_eq!(ctx.fmt("ro", "#{session_windows}"), "2");
}

#[test]
fn tmux38_format_variables() {
    let ctx = Ctx::new("vars38");
    ctx.ok(&["new-session", "-d", "-s", "v", "-x", "80", "-y", "24"]);
    ctx.ok(&["new-window", "-d", "-t", "v:4"]);
    ctx.ok(&["split-window", "-h", "-t", "v:0"]);
    // display -a lists tmux 3.8's variables
    let all = ctx.ok(&["display-message", "-a", "-t", "v:0"]);
    for name in ["active_window_index=0", "last_window_index=4", "session_stack=0", "pane_flags=*", "pane_z=1", "pane_floating_flag=0",
        "pane_pb_state=hidden", "pane_key_mode=", "history_added=", "next_session_id=$", "tree_mode_format=", "window_raw_flags=*"] {
        assert!(all.lines().any(|l| l.starts_with(name)), "{name} missing from display -a:\n{all}");
    }
    assert_eq!(ctx.fmt("v:0.0", "#{pane_flags}|#{pane_zoomed_flag}|#{pane_unzoomed_width}|#{pane_x}"), "-|0|40|0");
    ctx.ok(&["resize-pane", "-Z", "-t", "v:0.1"]);
    assert_eq!(ctx.fmt("v:0.1", "#{pane_flags}|#{pane_zoomed_flag}|#{pane_unzoomed_width}x#{pane_unzoomed_height}|#{window_raw_flags}"), "*Z|1|39x24|*Z");
    assert_eq!(ctx.fmt("v:4", "#{window_stack_index} #{window_active_sessions} #{window_linked_sessions_list} [#{window_manual_width}] #{session_alert}"), "0 0 v [] ");
    assert_eq!(ctx.fmt("v", "#{pane_pipe} #{pane_input_off} #{pane_unseen_changes} #{pane_modal_flag} [#{window_modal_pane}] #{sixel_support}"), "0 0 0 0 [] 0");
    ctx.ok(&["select-pane", "-d", "-t", "v:0.1"]);
    assert_eq!(ctx.fmt("v:0.1", "#{pane_input_off}"), "1");
    // copy-mode search text stays after copy mode exits
    ctx.ok(&["copy-mode", "-t", "v:0.1"]);
    ctx.ok(&["send-keys", "-t", "v:0.1", "-X", "search-backward", "needle"]);
    assert_eq!(ctx.fmt("v:0.1", "#{pane_search_string}"), "needle");
    ctx.ok(&["send-keys", "-t", "v:0.1", "-X", "cancel"]);
    assert_eq!(ctx.fmt("v:0.1", "#{pane_in_mode} #{pane_search_string}"), "0 needle");
}

#[test]
fn list_sort_orders_and_kill_filters() {
    let ctx = Ctx::new("sort");
    for s in ["zeta", "alpha", "mid"] {
        ctx.ok(&["new-session", "-d", "-s", s]);
    }
    // sessions list by name; -r alone keeps the natural order, -O sorts (tmux 3.8 sort.c)
    assert_eq!(ctx.ok(&["list-sessions", "-F", "#{session_name}:#{line}"]), "alpha:0\nmid:1\nzeta:2\n");
    assert_eq!(ctx.ok(&["ls", "-r", "-F", "#{session_name}"]), "alpha\nmid\nzeta\n");
    assert_eq!(ctx.ok(&["ls", "-O", "name", "-r", "-F", "#{session_name}"]), "zeta\nmid\nalpha\n");
    assert_eq!(ctx.ok(&["ls", "-O", "index", "-F", "#{session_name}"]), "zeta\nalpha\nmid\n");
    assert_eq!(ctx.ok(&["ls", "-O", "creation", "-r", "-f", "#{!=:#{session_name},mid}", "-F", "#{session_name}"]), "alpha\nzeta\n");
    assert!(ctx.fails(&["ls", "-O", "bogus"]).contains("invalid sort order"));
    // windows: #{line} is the window count, as in tmux
    ctx.ok(&["rename-window", "-t", "alpha:0", "ccc"]);
    ctx.ok(&["new-window", "-d", "-t", "alpha", "-n", "bbb"]);
    ctx.ok(&["new-window", "-d", "-t", "alpha", "-n", "aaa"]);
    assert_eq!(ctx.ok(&["lsw", "-t", "alpha", "-O", "name", "-F", "#{window_name} #{line}"]), "aaa 3\nbbb 3\nccc 3\n");
    assert_eq!(ctx.ok(&["lsw", "-t", "alpha", "-O", "index", "-r", "-F", "#{window_index}"]), "2\n1\n0\n");
    assert_eq!(ctx.ok(&["lsw", "-a", "-F", "#{session_name}:#{window_index}"]), "alpha:0\nalpha:1\nalpha:2\nmid:0\nzeta:0\n");
    // panes sort within each window
    ctx.ok(&["split-window", "-d", "-t", "mid"]);
    assert_eq!(ctx.ok(&["lsp", "-t", "mid", "-O", "creation", "-r", "-F", "#{pane_index}"]), "1\n0\n");
    assert_eq!(ctx.ok(&["lsp", "-s", "-t", "mid", "-F", "#{window_index}.#{pane_index}"]), "0.0\n0.1\n");
    assert!(ctx.fails(&["lsp", "-O", "nope"]).contains("invalid sort order"));
    // buffers: newest first, or by name / size
    for (b, v) in [("z1", "hello"), ("a2", "xx"), ("m3", "yyy")] {
        ctx.ok(&["set-buffer", "-b", b, v]);
    }
    assert_eq!(ctx.ok(&["lsb", "-F", "#{buffer_name}"]), "m3\na2\nz1\n");
    assert_eq!(ctx.ok(&["lsb", "-O", "name", "-F", "#{buffer_name}"]), "a2\nm3\nz1\n");
    assert_eq!(ctx.ok(&["lsb", "-O", "size", "-r", "-F", "#{buffer_name}"]), "z1\nm3\na2\n");
    assert_eq!(ctx.ok(&["lsb", "-f", "#{==:#{buffer_size},2}", "-F", "#{buffer_name}"]), "a2\n");
    assert_eq!(ctx.ok(&["lsc", "-O", "size"]), "");
    // kill -f filters need -a
    assert!(ctx.fails(&["kill-window", "-f", "1", "-t", "alpha"]).contains("-f only valid with -a"));
    assert!(ctx.fails(&["kill-pane", "-f", "1"]).contains("-f only valid with -a"));
    ctx.ok(&["kill-window", "-a", "-t", "alpha:0", "-f", "#{==:#{window_name},bbb}"]);
    assert_eq!(ctx.ok(&["lsw", "-t", "alpha", "-F", "#{window_name}"]), "ccc\naaa\n");
    ctx.ok(&["kill-pane", "-a", "-t", "mid:0.0", "-f", "#{==:#{pane_index},0}"]);
    assert_eq!(ctx.fmt("mid", "#{window_panes}"), "2");
    ctx.ok(&["kill-pane", "-a", "-t", "mid:0.0", "-f", "#{==:#{pane_index},1}"]);
    assert_eq!(ctx.fmt("mid", "#{window_panes}"), "1");
    ctx.ok(&["kill-session", "-a", "-t", "alpha", "-f", "#{==:#{session_name},mid}"]);
    assert_eq!(ctx.ok(&["ls", "-F", "#{session_name}"]), "alpha\nzeta\n");
    // kill-session -g: the whole group
    ctx.ok(&["new-session", "-d", "-t", "zeta", "-s", "zeta2"]);
    ctx.ok(&["kill-session", "-g", "-t", "zeta2"]);
    assert_eq!(ctx.ok(&["ls", "-F", "#{session_name}"]), "alpha\n");
}

#[test]
fn list_keys_layout_format_and_order() {
    let ctx = Ctx::new("lsk");
    ctx.ok(&["new-session", "-d", "-s", "k"]);
    ctx.ok(&["bind-key", "-r", "-T", "mytab", "x", "display", "hi"]);
    ctx.ok(&["bind-key", "-T", "mytab", "#", "display", "hash"]);
    ctx.ok(&["bind-key", "-N", "a note", "-T", "mytab", "\"", "display", "q"]);
    // tmux 3.8: padded table and key columns, keys in key-code order, quoted keys
    assert_eq!(ctx.ok(&["list-keys", "-T", "mytab"]), "bind-key    -T mytab \\\" display-message q\nbind-key    -T mytab \\# display-message hash\nbind-key -r -T mytab x display-message hi\n");
    assert_eq!(ctx.ok(&["lsk", "-T", "mytab", "-O", "name", "-r"]).lines().next(), Some("bind-key -r -T mytab x display-message hi"));
    assert_eq!(ctx.ok(&["lsk", "-1", "-T", "mytab"]), "bind-key  -T mytab \\\" display-message q\n");
    assert_eq!(
        ctx.ok(&["lsk", "-T", "mytab", "-F", "#{key_table}|#{key_string}|#{key_repeat}|#{key_note}|#{key_command}|#{key_string_width}|#{key_has_repeat}"]),
        "mytab|\"|0|a note|display-message q|1|1\nmytab|#|0||display-message hash|1|1\nmytab|x|1||display-message hi|1|1\n"
    );
    assert_eq!(ctx.ok(&["lsk", "-T", "mytab", "x"]), "bind-key -r -T mytab x display-message hi\n");
    assert!(ctx.fails(&["lsk", "-T", "mytab", "y"]).contains("unknown key: y"));
    assert!(ctx.fails(&["lsk", "-T", "mytab", "zzz"]).contains("invalid key: zzz"));
    assert!(ctx.fails(&["lsk", "-T", "nosuch"]).contains("table nosuch doesn't exist"));
    assert!(ctx.fails(&["lsk", "-O", "x"]).contains("invalid sort order"));
    // -N lists prefix then root notes, each with the prefix string
    ctx.ok(&["bind-key", "-n", "-N", "root note", "F5", "display", "x"]);
    assert!(ctx.ok(&["lsk", "-N", "-P", ">"]).lines().any(|l| l.starts_with("> F5") && l.ends_with("root note")));
}

#[test]
fn show_options_and_hooks_formats() {
    let ctx = Ctx::new("showf");
    ctx.ok(&["new-session", "-d", "-s", "o"]);
    ctx.ok(&["set", "-g", "@foo", "bar baz"]);
    let f = "#{option_name}|#{option_value}|#{option_is_string}|#{option_is_user}|#{option_is_array}|#{option_has_array_key}";
    assert_eq!(ctx.ok(&["show", "-g", "-F", f, "@foo"]), "@foo|bar baz|1|1|0|0\n");
    assert_eq!(ctx.ok(&["show", "-g", "-F", "#{option_name}=#{option_value}|#{option_is_string}", "base-index"]), "base-index=0|0\n");
    ctx.ok(&["set", "-g", "status-left", "L R"]);
    assert_eq!(ctx.ok(&["show", "-A", "-t", "o", "-F", "#{option_name}#{?option_is_parent,*,}", "status-left"]), "status-left*\n");
    assert_eq!(ctx.ok(&["showw", "-g", "-F", "#{option_name}:#{option_value}", "mode-keys"]), "mode-keys:emacs\n");
    assert_eq!(ctx.ok(&["show", "-gv", "-F", "V=#{option_value}#{option_value_only}", "status-left"]), "V=L R1\n");
    ctx.ok(&["set-hook", "-g", "session-renamed", "display hi"]);
    assert_eq!(ctx.ok(&["show-hooks", "-g", "-F", "#{option_name}[#{option_array_key}] #{option_value} #{option_is_hook}", "session-renamed"]), "session-renamed[0] display-message hi 1\n");
    // global options hold every hook of their scope: empty ones are shown by name
    assert_eq!(ctx.ok(&["show-hooks", "-g", "client-attached"]), "client-attached\n");
    assert_eq!(ctx.ok(&["show-hooks", "-g", "-F", "#{option_name}:#{option_has_value}:#{option_is_array}:#{hook_fire_count}", "after-new-window"]), "after-new-window:0:1:0\n");
    let all = ctx.ok(&["show-hooks", "-g"]);
    assert!(all.starts_with("after-bind-key\nafter-capture-pane\n") && all.contains("\nsession-renamed[0] display-message hi\n") && all.ends_with("\nwindow-unlinked\n"), "{all}");
    assert_eq!(all.lines().count(), 62);
    let win = ctx.ok(&["show-hooks", "-gw"]);
    assert!(win.starts_with("pane-activity\n") && win.ends_with("\nwindow-zoomed\n") && win.lines().count() == 27, "{win}");
    assert_eq!(ctx.ok(&["show-hooks", "-gp"]), "");
    // a session's options only hold the hooks set there, -w lists window hooks only
    ctx.ok(&["set-hook", "-t", "o", "session-renamed", "display s"]);
    ctx.ok(&["set-hook", "-w", "-t", "o", "pane-focus-in", "display w"]);
    assert_eq!(ctx.ok(&["show-hooks", "-t", "o"]), "session-renamed[0] display-message s\n");
    assert_eq!(ctx.ok(&["show-hooks", "-w", "-t", "o"]), "pane-focus-in[0] display-message w\n");
}

#[test]
fn run_shell_paste_and_display_flags() {
    let ctx = Ctx::new("runflags");
    ctx.ok(&["new-session", "-d", "-s", "r"]);
    ctx.wait_prompt("r");
    // run-shell: arguments are #{1}, #{2}...; stderr only with -E
    assert_eq!(ctx.ok(&["run-shell", "echo #{1}-#{2}-#{session_name}", "foo", "bar"]).trim(), "foo-bar-r");
    if cfg!(unix) {
        let both = "echo out; echo err >&2";
        assert_eq!(ctx.ok(&["run-shell", both]).trim(), "out");
        let shown = ctx.ok(&["run-shell", "-E", both]);
        assert!(shown.contains("out") && shown.contains("err"), "{shown}");
        // the shell is "sh"; a failure is printed after the output and the client
        // exits with the command's status (signals as 128 + the signal)
        assert_eq!(ctx.ok(&["run-shell", "echo $0"]).trim(), "sh");
        let out = ctx.command().args(["run-shell", "echo out; exit 3", ";", "display", "-p", "after"]).output().unwrap();
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n'echo out; exit 3' returned 3\nafter\n");
        assert!(out.stderr.is_empty(), "{out:?}");
        let out = ctx.command().args(["run-shell", "kill -9 $$"]).output().unwrap();
        assert_eq!(out.status.code(), Some(137));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "'kill -9 $$' terminated by signal 9\n");
        assert_eq!(ctx.command().args(["run-shell", "-b", "exit 4"]).output().unwrap().status.code(), Some(0));
    }
    assert_eq!(ctx.ok(&["run", "-d", "0.1", "-s", "x", "echo delayed"]).trim(), "delayed");
    // display-message -l/-F/-j
    assert_eq!(ctx.ok(&["display-message", "-p", "-l", "#{session_name}"]), "#{session_name}\n");
    assert_eq!(ctx.ok(&["display-message", "-p", "-F", "#{session_name}"]), "r\n");
    assert!(ctx.fails(&["display-message", "-p", "-F", "x", "y"]).contains("only one of -F or argument"));
    assert_eq!(ctx.ok(&["display-message", "-p", "-j", "{ \"b\": 1, \"a\": [ {\"t\": true} ] }"]), "{\"a\":[{\"t\":true}],\"b\":1}\n");
    assert!(ctx.fails(&["display-message", "-p", "-j", "{\"a\":01}"]).contains("invalid number"));
    // paste-buffer passes control characters through vis(3) unless -S (typed at the prompt)
    ctx.ok(&["set-buffer", "x\u{1b}y"]);
    ctx.ok(&["paste-buffer", "-t", "r"]);
    wait_for("vis paste", || ctx.capture("r").contains("x^[y"));
    ctx.ok(&["set-buffer", "zplain"]);
    ctx.ok(&["paste-buffer", "-S", "-t", "r"]);
    wait_for("raw paste", || ctx.capture("r").contains("x^[yzplain"));
}

#[test]
fn capture_pane_line_details_and_empty_panes() {
    let ctx = Ctx::new("capflags");
    // the shell commands that make the content are POSIX ones
    #[cfg(unix)]
    capture_details(&ctx);
    empty_panes(&ctx);
}

#[cfg(unix)]
fn capture_details(ctx: &Ctx) {
    ctx.ok(&["new-session", "-d", "-s", "c", "-x", "20", "-y", "4", "printf 'l1\\nlong line wrapping around\\n\\\\z'; sleep 100"]);
    wait_for("output", || ctx.capture("c").contains("\\z"));
    assert_eq!(ctx.ok(&["capture-pane", "-p", "-L", "-t", "c"]), "0 l1\n1 long line wrapping a\n2 round\n3 \\z\n");
    assert_eq!(ctx.ok(&["capture-pane", "-p", "-F", "-I", "-E", "1", "-t", "c"]), "0 - l1\n0 W long line wrapping a\n");
    assert_eq!(ctx.ok(&["capture-pane", "-p", "-C", "-S", "3", "-t", "c"]), "\\\\z\n");
    assert_eq!(ctx.ok(&["capture-pane", "-p", "-H", "-t", "c"]), "\n");
    assert_eq!(ctx.ok(&["capture-pane", "-p", "-P", "-t", "c"]), "\n");
    assert!(ctx.fails(&["capture-pane", "-p", "-a", "-t", "c"]).contains("no alternate screen"));
    assert_eq!(ctx.ok(&["capture-pane", "-p", "-a", "-q", "-t", "c"]), "\n");
    assert!(ctx.ok(&["capture-pane", "-p", "-R", "-t", "c"]).starts_with("G 20x4 (0/"));
    ctx.ok(&["copy-mode", "-t", "c"]);
    assert!(ctx.ok(&["capture-pane", "-p", "-M", "-t", "c"]).contains("long line"));
    // -M reads the mode's copy of the pane, not its scrolled view, nor the selection
    ctx.ok(&["send-keys", "-X", "-t", "c", "begin-selection"]);
    assert_eq!(ctx.ok(&["capture-pane", "-p", "-M", "-S", "1", "-E", "1", "-t", "c"]), "long line wrapping a\n");
}

fn empty_panes(ctx: &Ctx) {
    // empty panes: a terminal without a process
    ctx.ok(&["new-session", "-d", "-s", "e", "-x", "80", "-y", "24"]);
    ctx.ok(&["new-window", "-E", "-t", "e"]);
    assert_eq!(ctx.fmt("e", "[#{pane_pid}] [#{pane_dead}] [#{pane_start_command}]"), "[] [0] []");
    assert!(ctx.fails(&["new-window", "-E", "-t", "e", "echo hi"]).contains("command cannot be given for empty pane"));
    ctx.ok(&["split-window", "-E", "-t", "e"]);
    assert_eq!(ctx.fmt("e", "#{window_panes} [#{pane_pid}]"), "2 []");
    // an empty pane has no process, so it respawns without -k
    ctx.ok(&["respawn-pane", "-t", "e", "echo respawned; sleep 100"]);
    wait_for("respawned", || ctx.capture("e").contains("respawned"));
    assert!(ctx.fails(&["respawn-pane", "-E", "-t", "e"]).contains("still active"));
    ctx.ok(&["respawn-pane", "-k", "-E", "-t", "e"]);
    assert_eq!(ctx.fmt("e", "[#{pane_pid}]"), "[]");
    ctx.ok(&["respawn-window", "-k", "-E", "-t", "e:0"]);
    assert_eq!(ctx.fmt("e:0", "#{window_panes} [#{pane_pid}]"), "1 []");
}

#[test]
fn pane_and_target_flags() {
    let ctx = Ctx::new("paneflags");
    ctx.ok(&["new-session", "-d", "-s", "p", "-x", "80", "-y", "31"]);
    ctx.ok(&["new-window", "-d", "-t", "p"]);
    // join-pane -p: a percentage of the target pane
    ctx.ok(&["join-pane", "-p", "30", "-s", "p:1", "-t", "p:0"]);
    assert_eq!(ctx.ok(&["lsp", "-t", "p:0", "-F", "#{pane_height}"]), "21\n9\n");
    assert!(ctx.fails(&["join-pane", "-p", "130", "-s", "p:0.1", "-t", "p:0.0"]).contains("invalid tiled geometry"));
    // select-pane -P sets the pane style (and selects the pane)
    ctx.ok(&["select-pane", "-P", "bg=red", "-t", "p:0.0"]);
    assert_eq!(ctx.ok(&["select-pane", "-g", "-t", "p:0.0"]), "bg=red\n");
    assert_eq!(ctx.fmt("p", "#{pane_index}"), "0");
    // {current}/{active} need an attached client
    // display-message's -c can fail (CMD_CLIENT_CANFAIL)
    assert_eq!(ctx.ok(&["display-message", "-p", "-t", "p", "-c", "nope", "x"]), "x\n");
    assert!(ctx.fails(&["select-window", "-t", "{current}"]).contains("no current client"));
    assert!(ctx.fails(&["kill-pane", "-t", "@"]).contains("no current client"));
    assert_eq!(ctx.ok(&["list-commands", "-F", "#{command_list_name}|#{command_list_alias}", "list-keys"]), "list-keys|lsk\n");
    // tmux's usage strings, in its command table order
    assert_eq!(ctx.ok(&["lscm", "display-message"]), "display-message (display) [-aCIjlNpv] [-c target-client] [-d delay] [-F format] [-t target-pane] [message]\n");
    assert_eq!(ctx.ok(&["lscm", "-F", "#{command_list_usage}|", "kill-server"]), "|\n");
    let names: Vec<String> = ctx.ok(&["lscm", "-F", "#{command_list_name}"]).lines().map(String::from).collect();
    let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
    assert!(pos("display-popup") < pos("display-panes") && pos("attach-session") == 0, "{names:?}");
}

#[cfg(unix)]
#[test]
fn server_access_groups() {
    let ctx = Ctx::new("acl-group");
    ctx.ok(&["new-session", "-d", "-s", "a"]);
    let out = std::process::Command::new("id").arg("-gn").output().unwrap();
    let group = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(ctx.fails(&["server-access", "-g", "-a", "no-such-group-pt-xyz"]).contains("unknown group"));
    ctx.ok(&["server-access", "-g", "-a", &group]);
    assert_eq!(ctx.ok(&["server-access", "-l"]), format!("{group} (G,W)\n"));
    assert!(ctx.fails(&["server-access", "-g", "-a", &group]).contains("is already added"));
    ctx.ok(&["server-access", "-g", "-r", &group]);
    assert_eq!(ctx.ok(&["server-access", "-l"]), format!("{group} (G,R)\n"));
    assert!(ctx.fails(&["server-access", "-g", "-a", "-d", &group]).contains("cannot be used together"));
    ctx.ok(&["server-access", "-g", "-d", &group]);
    assert_eq!(ctx.ok(&["server-access", "-l"]), "");
}

#[test]
fn format_option_environment_and_window_loops() {
    let ctx = Ctx::new("fmt-loops-38");
    ctx.ok(&["new-session", "-d", "-s", "f", "-n", "first"]);
    ctx.ok(&["new-window", "-d", "-t", "f:1", "-n", "second"]);
    ctx.ok(&["set", "-t", "f", "@b", "2"]);
    ctx.ok(&["set", "-t", "f", "@a", "1"]);
    ctx.ok(&["setw", "-t", "f:1", "@c", "red"]);
    ctx.ok(&["set-environment", "-t", "f", "FMT_SET", "yes"]);
    ctx.ok(&["set-environment", "-t", "f", "-r", "FMT_GONE"]);
    let user = "#{O:#{?#{m:@*,#{option_name}},#{option_name}=#{option_value}/#{option_is_user};,}}";
    assert_eq!(ctx.fmt("f", user), "@a=1/1;@b=2/1;");
    assert_eq!(ctx.fmt("f", "#{O/gw:#{?#{==:#{option_name},mode-keys},#{option_value},}}"), "emacs");
    let env = "#{V:#{?#{m:FMT_*,#{environ_name}},#{environ_name}=#{environ_value}/#{environ_removed};,}}";
    assert_eq!(ctx.fmt("f", env), "FMT_GONE=/1;FMT_SET=yes/0;");
    assert_eq!(ctx.fmt("f", "#{W:#{window_index}/#{next_window_index}/#{next_@c}/#{window_after_active};}"), "0/1/red/0;1///1;");
    assert_eq!(ctx.fmt("f", "#{N:second}#{N:third}#{N/s:f}"), "101");
    // No attached client: no client loop rows, client information or animation.
    assert_eq!(ctx.fmt("f", "[#{L:x}#{I/f:RGB}#{A:a,b}]"), "[]");
    assert_eq!(ctx.fmt("f", "#{c:red}#{c/f:themered}#{R:ab,2}"), "800000\x1b[31mabab");
}

#[test]
fn display_all_samples_and_printed_commands() {
    let ctx = Ctx::new("p1fix-print");
    ctx.ok(&["new-session", "-d", "-s", "d", "sleep 100"]);
    ctx.ok(&["new-window", "-d", "-t", "d", "sleep", "99"]);
    // display -a leaves out variables without a value and adds the command name
    let all = ctx.ok(&["display-message", "-a", "-t", "d:0"]);
    assert!(all.contains("session_name=d\n") && all.ends_with("command=display-message\n"), "{all}");
    for absent in ["pane_dead_status=", "pane_dead_signal=", "pane_mode=", "buffer_name=", "session_group=", "window_offset_x="] {
        assert!(!all.contains(&format!("\n{absent}")), "{absent} in {all}");
    }
    assert_eq!(ctx.fmt("d:0", "#{command}"), "display-message");
    // cmd_stringify_argv: each argument escaped
    assert_eq!(ctx.fmt("d:0", "#{pane_start_command}"), "\"sleep 100\"");
    assert_eq!(ctx.fmt("d:1", "#{pane_start_command}"), "sleep 99");
    // paste_make_sample: 200 bytes, vis-encoded, "..." when cut
    let long = format!("a\tb\u{1}\"c\" {}", "0".repeat(300));
    ctx.ok(&["set-buffer", "-b", "long", &long]);
    let sample = ctx.ok(&["list-buffers", "-F", "#{buffer_name}=#{buffer_sample}", "-f", "#{==:#{buffer_name},long}"]);
    let want = format!("a\\tb\\001\"c\" {}...", "0".repeat(200 - 12));
    assert_eq!(sample.trim_end(), format!("long={want}"));
    // list-keys and show-hooks print commands as tmux does
    ctx.ok(&["bind", "x", "display", "hi;there"]);
    ctx.ok(&["bind", "y", "display a ; display b"]);
    // blocks of a parsed command stay blocks; "{" and "}" arguments are plain strings,
    // and the command is checked when it is bound
    ctx.ok(&["if", "-F", "1", "bind z if -F 1 { display a } { display b ; display c }"]);
    assert_eq!(ctx.fails(&["bind", "v", "if", "-F", "1", "{", "display", "a", "}"]).trim(), "command if-shell: too many arguments (need at most 3)");
    assert_eq!(ctx.fails(&["bind", "v", "nosuchcmd"]).trim(), "unknown command: nosuchcmd");
    assert_eq!(ctx.fails(&["bind", "v", "splitw -Q"]).trim(), "command split-window: unknown flag -Q");
    assert_eq!(ctx.fails(&["bind", "v", "kill-session", "-t", "x", "y"]).trim(), "command kill-session: too many arguments (need at most 0)");
    assert!(ctx.fails(&["list-keys", "-T", "prefix", "v"]).contains("unknown key: v"));
    // an abbreviated bind-key keeps its blocks too
    ctx.ok(&["if", "-F", "1", "bind-k u if -F 1 { display a } b"]);
    assert_eq!(ctx.ok(&["list-keys", "-T", "prefix", "u"]).trim_end(), "bind-key  -T prefix u if-shell -F 1 { display-message a } b");
    ctx.ok(&["if", "-F", "1", "bind -n F3 { display x ; display y }"]);
    assert_eq!(ctx.ok(&["list-keys", "-T", "root", "F3"]).trim_end(), "bind-key  -T root F3 display-message x \\; display-message y");
    ctx.ok(&["bind", "w", "send-keys", "-X", "copy-pipe", "cat >/dev/null"]);
    let keys = |k: &str| ctx.ok(&["list-keys", "-T", "prefix", k]).trim_end().to_string();
    assert_eq!(keys("x"), "bind-key  -T prefix x display-message \"hi;there\"");
    assert_eq!(keys("y"), "bind-key  -T prefix y display-message a \\; display-message b");
    assert_eq!(keys("z"), "bind-key  -T prefix z if-shell -F 1 { display-message a } { display-message b ; display-message c }");
    assert_eq!(keys("w"), "bind-key  -T prefix w send-keys -X copy-pipe \"cat >/dev/null\"");
    assert_eq!(keys("$"), "bind-key  -T prefix \\$ command-prompt -I \"#S\" { rename-session \"%%\" }");
    assert_eq!(ctx.ok(&["list-keys", "-T", "prefix", "-F", "#{key_command}", "y"]).trim_end(), "display-message a \\; display-message b");
    ctx.ok(&["set-hook", "-g", "after-new-window", "display \"hi #{window_name}\" ; set -g @x 1"]);
    ctx.ok(&["set-hook", "-g", "-a", "after-new-window", "if -F 1 { display a ; display b } { display c }"]);
    let hook0 = "after-new-window[0] display-message \"hi #{window_name}\" ; set-option -g @x 1";
    let hook1 = "after-new-window[1] if-shell -F 1 { display-message a ; display-message b } { display-message c }";
    assert_eq!(ctx.ok(&["show-hooks", "-g", "after-new-window"]), format!("{hook0}\n{hook1}\n"));
    assert_eq!(ctx.ok(&["show-hooks", "-g", "after-new-window[1]"]), format!("{hook1}\n"));
    assert_eq!(ctx.ok(&["show-options", "-gH", "after-new-window"]), format!("{hook0}\n{hook1}\n"));
    // OPTIONS_IS_STRING: hooks, colours and numbers are not strings
    let is_string = |args: &[&str]| ctx.ok(&[&["show-options", "-F", "#{option_is_string}"], args].concat()).lines().next().unwrap_or("").to_string();
    assert_eq!(is_string(&["-gH", "after-new-window"]), "0");
    assert_eq!(is_string(&["-gw", "pane-colours"]), "0");
    assert_eq!(is_string(&["-g", "update-environment"]), "1");
    assert_eq!(is_string(&["-g", "status-style"]), "1");
    assert_eq!(is_string(&["-g", "display-time"]), "0");
}

#[test]
fn hidden_environment_variables() {
    let ctx = Ctx::new("p1fix-env");
    ctx.ok(&["new-session", "-d", "-s", "e"]);
    ctx.ok(&["set-environment", "-g", "-h", "P1FIX_HID", "sec$ret \"x\" `y` \\z"]);
    ctx.ok(&["set-environment", "-g", "P1FIX_VIS", "visible"]);
    ctx.ok(&["set-environment", "-t", "e", "-h", "P1FIX_HID2", "two"]);
    // hidden variables show only with -h, and -h shows only hidden ones
    assert_eq!(ctx.ok(&["show-environment", "-g", "P1FIX_HID"]), "");
    assert_eq!(ctx.ok(&["show-environment", "-gh"]), "P1FIX_HID=sec$ret \"x\" `y` \\z\n");
    assert_eq!(ctx.ok(&["show-environment", "-gh", "-s"]), "P1FIX_HID=\"sec\\$ret \\\"x\\\" \\`y\\` \\\\z\"; export P1FIX_HID;\n");
    assert_eq!(ctx.ok(&["show-environment", "-t", "e", "-h"]), "P1FIX_HID2=two\n");
    assert!(!ctx.ok(&["show-environment", "-t", "e"]).contains("P1FIX_HID2"));
    assert!(ctx.fails(&["show-environment", "-t", "e", "NOPE"]).contains("unknown variable: NOPE"));
    // still a format variable, and #{environ_hidden}
    assert_eq!(ctx.fmt("e:", "#{P1FIX_HID2}"), "two");
    assert_eq!(ctx.fmt("e:", "#{V/g:#{?#{m:P1FIX_*,#{environ_name}},#{environ_name}=#{environ_hidden};,}}"), "P1FIX_HID=1;P1FIX_VIS=0;");
    // not passed to new processes
    let out = std::env::temp_dir().join(format!("p1fix-env-{}", std::process::id()));
    let cmd = format!("env > '{}'; sleep 30", out.display());
    #[cfg(unix)]
    {
        ctx.ok(&["new-window", "-d", "-t", "e", &cmd]);
        wait_for("child environment", || std::fs::read_to_string(&out).is_ok_and(|s| s.contains("P1FIX_VIS=")));
        let env = std::fs::read_to_string(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        assert!(!env.contains("P1FIX_HID"), "{env}");
    }
    let _ = cmd;
    // setting without -h shows it again; errors as tmux reports them
    ctx.ok(&["set-environment", "-g", "P1FIX_HID", "shown"]);
    assert_eq!(ctx.ok(&["show-environment", "-g", "P1FIX_HID"]), "P1FIX_HID=shown\n");
    assert!(ctx.fails(&["set-environment", "-g", "", "x"]).contains("empty variable name"));
    assert!(ctx.fails(&["set-environment", "-g", "A=B", "x"]).contains("variable name contains ="));
    assert!(ctx.fails(&["set-environment", "-g", "-u", "P1FIX_VIS", "x"]).contains("can't specify a value with -u"));
}

#[test]
fn message_log_jobs_and_prompt_history_types() {
    let ctx = Ctx::new("p1fix-msgs");
    ctx.ok(&["new-session", "-d", "-s", "m"]);
    // without -p and with no client to show it on, the message goes nowhere
    assert_eq!(ctx.ok(&["display-message", "hello msg"]), "");
    // commands from clients are logged, newest first
    let log = ctx.ok(&["show-messages"]);
    let lines: Vec<&str> = log.lines().collect();
    assert!(lines[0].ends_with(" command: show-messages"), "{log}");
    assert!(lines[1].ends_with(" command: display-message \"hello msg\""), "{log}");
    // -J: running jobs
    ctx.ok(&["run-shell", "-b", "sleep 5"]);
    let jobs = ctx.ok(&["show-messages", "-J"]);
    assert!(jobs.starts_with("Job 0: sleep 5 [fd="), "{jobs}");
    // the descriptor its output is read from
    let fd: i64 = jobs.split("fd=").nth(1).and_then(|r| r.split(',').next()).and_then(|n| n.parse().ok()).unwrap();
    assert!(if cfg!(unix) { fd >= 0 } else { fd == -1 }, "{jobs}");
    // no attached clients: -T lists no terminals
    assert_eq!(ctx.ok(&["show-messages", "-T"]), "");
    // tmux 3.8 has a command and a search history
    assert_eq!(ctx.ok(&["show-prompt-history"]), "History for command:\n\n\nHistory for search:\n\n\n");
    assert_eq!(ctx.ok(&["show-prompt-history", "-T", "search"]), "History for search:\n\n\n");
    assert!(ctx.fails(&["show-prompt-history", "-T", "window-target"]).contains("invalid type: window-target"));
    ctx.ok(&["clear-prompt-history", "-T", "command"]);
    assert!(ctx.fails(&["clear-prompt-history", "-T", "bogus"]).contains("invalid type: bogus"));
}

#[cfg(unix)]
#[test]
fn capture_pane_history_times() {
    let ctx = Ctx::new("p1fix-capi");
    let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    ctx.ok(&["new-session", "-d", "-s", "c", "-x", "40", "-y", "10", "seq 1 30; sleep 100"]);
    wait_for("output", || ctx.capture("c").contains("30"));
    let out = ctx.ok(&["capture-pane", "-p", "-I", "-L", "-S", "-", "-t", "c"]);
    let rows: Vec<(i64, u64, String)> = out.lines().map(|l| {
        let mut w = l.splitn(3, ' ');
        (w.next().unwrap().parse().unwrap(), w.next().unwrap().parse().unwrap(), w.next().unwrap_or("").to_string())
    }).collect();
    // history lines carry the time they entered the history, screen lines 0
    assert_eq!(rows[0].0, -21);
    assert_eq!(rows[0].2, "1");
    for (n, time, _) in &rows {
        if *n < 0 { assert!(*time >= before && *time <= before + 60, "{out}"); } else { assert_eq!(*time, 0, "{out}"); }
    }
}

#[test]
fn display_message_verbose() {
    let ctx = Ctx::new("p1fix-dv");
    ctx.ok(&["new-session", "-d", "-s", "v", "sleep 100"]);
    // -v: the expansion steps as tmux's format_log prints them, then the result
    let out = ctx.ok(&["display-message", "-p", "-v", "-t", "v", "#{session_name} #{?#{==:a,a},y,n}"]);
    let want = "# expanding format: #{session_name} #{?#{==:a,a},y,n}\n\
# found #{}: session_name\n\
# format 'session_name' found: v\n\
# replaced 'session_name' with 'v'\n\
# found #{}: ?#{==:a,a},y,n\n\
# condition is: #{==:a,a}\n\
#  expanding format: #{==:a,a}\n\
#  found #{}: ==:a,a\n\
#  modifier 0 is ==\n\
#   expanding format: a\n\
#   result is: a\n\
#   expanding format: a\n\
#   result is: a\n\
#  compare == left is: a\n\
#  compare == right is: a\n\
#  replaced '==:a,a' with '1'\n\
#  result is: 1\n\
# condition '#{==:a,a}' is true\n";
    assert!(out.starts_with(want), "{out}");
    assert!(out.ends_with("# result is: v y\nv y\n"), "{out}");
}

#[test]
fn current_session_is_the_last_used() {
    let ctx = Ctx::new("p1fix-cur");
    ctx.ok(&["new-session", "-d", "-s", "a", "sleep 100"]);
    ctx.ok(&["new-session", "-d", "-s", "b", "sleep 100"]);
    // created in the same second: the newer session is the current one
    assert_eq!(ctx.ok(&["display-message", "-p", "#S"]), "b\n");
    // selecting a window is not activity (session_set_current)
    ctx.ok(&["select-window", "-t", "a:0"]);
    assert_eq!(ctx.ok(&["display-message", "-p", "#S"]), "b\n");
    assert_eq!(ctx.ok(&["list-sessions", "-O", "activity", "-F", "#S"]), "b\na\n");
}

#[cfg(unix)]
#[test]
fn capture_pane_line_flags() {
    let ctx = Ctx::new("p1fix-capf");
    ctx.ok(&["new-session", "-d", "-s", "f", "-x", "20", "-y", "8",
        "printf 'plain\\n\\033]133;A\\033\\\\$ cmd\\n\\033]133;C\\033\\\\out\\n\\033[38;2;1;2;3mrgb\\033[m\\n%025d\\nend'; sleep 100"]);
    wait_for("output", || ctx.capture("f").contains("end"));
    // -F: start of prompt (P) and output (O) from OSC 133, wrapped (W), extended cells (X)
    let out = ctx.ok(&["capture-pane", "-p", "-F", "-E", "6", "-t", "f"]);
    assert_eq!(out, format!("- plain\nP $ cmd\nO out\nX rgb\nW {}\n- 00000\n- end\n", "0".repeat(20)));
}

#[test]
fn capture_pane_marks_survive_erasing_the_line() {
    let ctx = Ctx::new("p1fix-capf2");
    ctx.ok(&["new-session", "-d", "-s", "f", "-x", "20", "-y", "4",
        "printf '\\033]133;C\\033\\\\c\\033[2Kd'; sleep 100"]);
    wait_for("output", || ctx.capture("f").contains('d'));
    // screen_write_clearline keeps the line's OSC 133 flags
    assert_eq!(ctx.ok(&["capture-pane", "-p", "-F", "-E", "0", "-t", "f"]), "O  d\n");
}

#[test]
fn option_loop_hooks_and_session_default_size() {
    let ctx = Ctx::new("p1fix-oloop");
    // -x/-y set the session's default-size, its only local option here
    ctx.ok(&["new-session", "-d", "-s", "m", "-x", "80", "-y", "24", "sleep 100"]);
    assert_eq!(ctx.ok(&["show-options", "-t", "m"]), "default-size 80x24\n");
    ctx.ok(&["set-hook", "-g", "after-new-window", "display a"]);
    ctx.ok(&["set-hook", "-t", "m", "session-renamed", "display c"]);
    // O loops over hooks too: a session's set ones, every global one of the scope
    assert_eq!(ctx.fmt("m", "#{O:#{option_name}=#{option_value}|}"), "default-size=80x24|session-renamed=display-message c|");
    let g = ctx.fmt("m", "#{O/g:#{option_name}[#{option_array_key}]#{option_is_hook}#{option_array_count}=#{option_value}|}");
    assert!(g.starts_with("activity-action[]00=other|after-bind-key[]10=|"), "{g}");
    assert!(g.contains("|after-new-window[0]11=display-message a|"), "{g}");
    // an array with items is listed by its items only
    assert!(g.contains("|update-environment[0]013=DISPLAY|") && !g.contains("update-environment[]"), "{g}");
    let w = ctx.fmt("m", "#{O/gw:#{option_name}|}");
    assert!(w.contains("|pane-activity|") && w.contains("|window-zoomed|") && !w.contains("after-bind-key"), "{w}");
}

#[test]
fn argument_counts_and_parse_errors() {
    let ctx = Ctx::new("p1fix-argc");
    ctx.ok(&["new-session", "-d", "-s", "a", "sleep 100"]);
    // args_parse limits, reported as cmd_parse does
    assert_eq!(ctx.fails(&["show-options", "-g", "base-index", "status"]).trim(), "command show-options: too many arguments (need at most 1)");
    assert_eq!(ctx.fails(&["set-environment"]).trim(), "command set-environment: too few arguments (need at least 1)");
    assert_eq!(ctx.fails(&["if-shell", "true"]).trim(), "command if-shell: too few arguments (need at least 2)");
    assert_eq!(ctx.fails(&["show-hooks", "-v"]).trim(), "command show-hooks: unknown flag -v");
    assert_eq!(ctx.fails(&["display-message", "-t"]).trim(), "command display-message: -t expects an argument");
    assert_eq!(ctx.ok(&["display-message", "-p", "-t", "a", "#S"]), "a\n");
}

#[test]
fn cmd_find_errors_and_canfail_targets() {
    let ctx = Ctx::new("p1fix-find");
    ctx.ok(&["new-session", "-d", "-s", "s0", "-x", "80", "-y", "24", "sleep 100"]);
    // an unknown @id used to recurse until the server's stack overflowed
    for cmd in ["select-pane", "select-window", "has-session", "list-windows"] {
        assert_eq!(ctx.fails(&[cmd, "-t", "@99"]).trim(), "can't find window: @99", "{cmd}");
    }
    assert_eq!(ctx.ok(&["display-message", "-p", "-t", "@99", "[#S]"]), "[]\n");
    ctx.ok(&["has-session", "-t", "s0"]);
    // the failing part, as cmd_find_target reports it
    for (cmd, t, e) in [
        ("select-pane", "nosuch", "can't find pane: nosuch"),
        ("select-window", "nosuch", "can't find window: nosuch"),
        ("has-session", "nosuch", "can't find session: nosuch"),
        ("has-session", "nosuch:", "can't find session: nosuch"),
        ("has-session", ":nosuch", "can't find window: nosuch"),
        ("has-session", "s0:9", "can't find window: 9"),
        ("select-window", "s0:0.9", "can't find pane: 9"),
        ("select-window", "%99", "can't find pane: %99"),
        ("has-session", "$99", "can't find session: $99"),
        ("select-pane", "nosuch.1", "can't find window: nosuch"),
        ("select-pane", "=s", "can't find pane: =s"),
        ("has-session", "=s", "can't find session: s"),
    ] {
        assert_eq!(ctx.fails(&[cmd, "-t", t]).trim(), e, "{cmd} -t {t}");
    }
    ctx.ok(&["select-window", "-t", "=s"]);
    // display-message's target can fail: what was found stays, the rest is empty
    assert_eq!(ctx.ok(&["display-message", "-p", "-t", "nosuch", "[#S:#I]"]), "[:]\n");
    assert_eq!(ctx.ok(&["display-message", "-p", "-t", "s0:7", "#{session_format}#{window_format}#{pane_format} [#S:#I.#P]"]), "100 [s0:0.0]\n");
    assert_eq!(ctx.ok(&["display-message", "-p", "-t", "s0:0.5", "#{session_format}#{window_format}#{pane_format} [#S:#I.#P]"]), "010 [s0:0.0]\n");
    assert_eq!(ctx.fails(&["display-message", "-d", "abc", "hi"]).trim(), "delay invalid");
    assert_eq!(ctx.fails(&["display-message", "-d", "-1", "-p", "hi"]).trim(), "delay too small");
}

#[test]
fn format_type_follows_the_target() {
    let ctx = Ctx::new("p1fix-ftype");
    ctx.ok(&["new-session", "-d", "-s", "s0", "sleep 100"]);
    ctx.ok(&["new-window", "-d", "sleep 100"]);
    let f = "#{session_format}#{window_format}#{pane_format}";
    assert_eq!(ctx.ok(&["display-message", "-p", f]), "001\n");
    assert_eq!(ctx.ok(&["list-windows", "-F", f]), "010\n010\n");
    assert_eq!(ctx.ok(&["list-sessions", "-F", f]), "100\n");
    assert_eq!(ctx.ok(&["list-panes", "-F", f]), "001\n");
    assert_eq!(ctx.ok(&["display-message", "-p", &format!("#{{S:{f}}} #{{W:{f}}} #{{P:{f}}}")]), "100 010010 001\n");
    // format_defaults fills in the session's current pane
    assert_eq!(ctx.ok(&["list-sessions", "-F", "#{pane_index}"]), "0\n");
}

#[test]
fn bind_key_splits_separate_arguments_like_cmd_parse_from_arguments() {
    let ctx = Ctx::new("p1fix-bindsplit");
    ctx.ok(&["new-session", "-d", "-s", "s1", "sleep 100"]);
    let dir = std::env::temp_dir().join(format!("pt-bindsplit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let conf = dir.join("c.conf");
    std::fs::write(&conf, concat!(
        "bind F4 resize-pane -U 5 \\; select-pane -t '{last}'\n",
        "bind F5 display a \\; display b\n",
        "bind F6 send -l '\\;'\n",
        "bind F7 send -l ';'\n",
        "bind F8 send -l 'a;'\n",
        "bind F9 send -l \\;x\n",
        "bind -n M-z resize-pane -Z \\; set -g @z 1\n",
        "bind F10 source-file /dev/null \\; display \"Reloaded!\"\n",
    )).unwrap();
    // the config's \; reaches bind-key as a ";" argument, which ends a command
    assert_eq!(ctx.ok(&["source-file", conf.to_str().unwrap()]), "");
    for (k, want) in [
        ("F4", "resize-pane -U 5 \\; select-pane -t \"{last}\""),
        ("F5", "display-message a \\; display-message b"),
        ("F6", "send-keys -l \\;"),
        ("F7", "send-keys -l"),
        ("F8", "send-keys -l a"),
        ("F9", "send-keys -l \";x\""),
        ("F10", "source-file /dev/null \\; display-message Reloaded!"),
    ] {
        assert_eq!(ctx.ok(&["list-keys", k]).trim(), format!("bind-key  -T prefix {k} {want}"), "{k}");
    }
    assert_eq!(ctx.ok(&["list-keys", "-T", "root", "M-z"]).trim(), r"bind-key  -T root M-z resize-pane -Z \; set-option -g @z 1");
    // both commands run
    ctx.ok(&["bind-key", "F3", "set", "-g", "@a", "1;", "set", "-g", "@b", "2"]);
    assert_eq!(ctx.ok(&["list-keys", "F3"]).trim(), r"bind-key  -T prefix F3 set-option -g @a 1 \; set-option -g @b 2");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn set_hook_is_set_option_and_show_hooks_scopes() {
    let ctx = Ctx::new("p1fix-hooks");
    ctx.ok(&["new-session", "-d", "-s", "m", "sleep 100"]);
    ctx.ok(&["new-session", "-d", "-s", "f", "sleep 100"]);
    // name[N] is that element; -a uses the first free index; -u name[N] removes it
    ctx.ok(&["set-hook", "-g", "pane-died[3]", "run \"echo hi\""]);
    ctx.ok(&["set-hook", "-g", "pane-died[1]", "display died"]);
    ctx.ok(&["set-hook", "-ag", "pane-died", "display app"]);
    assert_eq!(ctx.ok(&["show-hooks", "-g", "pane-died"]), "pane-died[0] display-message app\npane-died[1] display-message died\npane-died[3] run-shell \"echo hi\"\n");
    ctx.ok(&["set-hook", "-ug", "pane-died[3]"]);
    assert_eq!(ctx.ok(&["show-hooks", "-g", "pane-died"]), "pane-died[0] display-message app\npane-died[1] display-message died\n");
    ctx.ok(&["set-hook", "-t", "f", "session-renamed[3]", "neww"]);
    assert_eq!(ctx.ok(&["show-hooks", "-t", "f", "session-renamed"]), "session-renamed[3] new-window\n");
    // names are checked as option names; values as option values
    assert_eq!(ctx.fails(&["set-hook", "-g", "nosuch-hook", "display x"]).trim(), "invalid option: nosuch-hook");
    assert_eq!(ctx.fails(&["set-hook", "-g", "status", "display x"]).trim(), "unknown value: display x");
    assert_eq!(ctx.fails(&["set-hook", "-g", "client-attached"]).trim(), "empty value");
    assert_eq!(ctx.fails(&["show-hooks", "-g", "nosuch"]).trim(), "invalid option: nosuch");
    assert_eq!(ctx.fails(&["show-hooks", "-g", "@x"]).trim(), "invalid option: @x");
    assert_eq!(ctx.ok(&["show-hooks", "-g", "status"]), "status on\n");
    assert_eq!(ctx.ok(&["show-hooks", "-g", "after-new-window[2]"]), "after-new-window[2] \n");
    // an @ name is a user option, listed by show-hooks (and show-options only with -H)
    ctx.ok(&["set-hook", "-g", "@u", "neww"]);
    assert_eq!(ctx.fmt("m", "#{@u}"), "neww");
    assert_eq!(ctx.ok(&["show-options", "-gv", "@u"]), "neww\n");
    assert_eq!(ctx.ok(&["show-hooks", "-g", "@u"]), "@u neww\n");
    assert!(ctx.ok(&["show-hooks", "-g"]).starts_with("@u neww\n"));
    assert!(!ctx.ok(&["show-options", "-g"]).contains("@u"));
    assert!(ctx.ok(&["show-options", "-gH"]).contains("@u neww"));
    // pane hooks live on the pane (-p ignores -g), window hooks on the window
    ctx.ok(&["set-hook", "-p", "-t", "m", "pane-exited", "display x"]);
    assert_eq!(ctx.ok(&["show-hooks", "-w", "-t", "m"]), "");
    assert_eq!(ctx.ok(&["show-hooks", "-p", "-t", "m"]), "pane-exited[0] display-message x\n");
    assert_eq!(ctx.ok(&["show-hooks", "-g", "-p", "-t", "m"]), "pane-exited[0] display-message x\n");
    assert_eq!(ctx.fmt("m", "#{O/p:#{option_name};}"), "pane-exited;");
    assert_eq!(ctx.ok(&["show-hooks", "-p", "-t", "f"]), "");
    ctx.ok(&["set-hook", "-w", "-t", "m", "window-renamed", "set -g @wr yes"]);
    assert_eq!(ctx.ok(&["show-hooks", "-w", "-t", "m"]), "window-renamed[0] set-option -g @wr yes\n");
    assert_eq!(ctx.ok(&["show-hooks", "-w", "-t", "f"]), "");
    ctx.ok(&["rename-window", "-t", "f:0", "other"]);
    ctx.fails(&["show-options", "-gv", "@wr"]);
    ctx.ok(&["rename-window", "-t", "m:0", "zz"]);
    wait_for("window hook", || ctx.cmd(&["show-options", "-gv", "@wr"]).1 == "yes\n");
}

#[test]
fn sessions_windows_and_panes_have_their_own_ids() {
    let ctx = Ctx::new("p1fix-ids");
    ctx.ok(&["new-session", "-d", "-s", "s1", "sleep 100"]);
    assert_eq!(ctx.ok(&["list-panes", "-a", "-F", "#{pane_id} #{window_id} #{session_id}"]), "%0 @0 $0\n");
    assert_eq!(ctx.fmt("s1", "#{next_session_id}"), "$1");
    ctx.ok(&["new-window", "-d", "sleep 100"]);
    ctx.ok(&["split-window", "-d", "-t", "s1:1", "sleep 100"]);
    assert_eq!(ctx.ok(&["list-panes", "-a", "-F", "#{pane_id} #{window_id}"]), "%0 @0\n%1 @1\n%2 @1\n");
    ctx.ok(&["new-session", "-d", "-s", "s2", "sleep 100"]);
    assert_eq!(ctx.fmt("s1", "#{next_session_id}"), "$2");
    // an unnamed session is named by its id; a grouped one by its group and id
    ctx.ok(&["new-session", "-d", "sleep 100"]);
    ctx.ok(&["new-session", "-d", "-t", "s1"]);
    assert_eq!(ctx.ok(&["list-sessions", "-F", "#{session_name} #{session_id}"]), "2 $2\ns1 $0\ns1-3 $3\ns2 $1\n");
    // the global environment is not session $0's
    ctx.ok(&["set-environment", "-g", "G1", "x"]);
    ctx.ok(&["set-environment", "-t", "s1", "S1", "y"]);
    assert_eq!(ctx.ok(&["show-environment", "-g", "G1"]), "G1=x\n");
    ctx.fails(&["show-environment", "-g", "S1"]);
    assert_eq!(ctx.ok(&["show-environment", "-t", "s1", "S1"]), "S1=y\n");
}

#[test]
fn client_lookup_send_keys_and_refresh_client_errors() {
    let ctx = Ctx::new("p1fix-clients");
    ctx.ok(&["new-session", "-d", "-s", "s1", "sleep 100"]);
    // CMD_CLIENT_CANFAIL: an unknown client is no error (a session name is not a client)
    assert_eq!(ctx.ok(&["display-message", "-p", "-c", "nosuch", "#{session_name}"]), "s1\n");
    assert_eq!(ctx.ok(&["display-message", "-p", "-c", "s1", "#{session_name}"]), "s1\n");
    ctx.ok(&["display-message", "-c", "bogus", "msg"]);
    ctx.ok(&["show-messages", "-t", "bogus"]);
    ctx.ok(&["send-keys", "-c", "bogus", "-t", "s1", "x"]);
    assert_eq!(ctx.fails(&["switch-client", "-c", "s1", "-t", "s1"]).trim(), "can't find client: s1");
    assert_eq!(ctx.fails(&["refresh-client", "-t", "s1:"]).trim(), "can't find client: s1:");
    // cmd_send_keys_exec's checks
    assert_eq!(ctx.fails(&["send-keys", "-t", "s1", "-X", "cancel"]).trim(), "not in a mode");
    assert_eq!(ctx.fails(&["send-keys", "-t", "s1", "-N", "0", "x"]).trim(), "repeat count too small");
    assert_eq!(ctx.fails(&["send-keys", "-t", "s1", "-N", "abc", "x"]).trim(), "repeat count invalid");
    ctx.ok(&["send-keys", "-t", "s1", "-N", "2"]);
    ctx.ok(&["copy-mode", "-t", "s1"]);
    ctx.ok(&["send-keys", "-t", "s1", "-N", "2", "-X", "cursor-up"]);
    ctx.ok(&["send-keys", "-t", "s1", "-X", "cancel"]);
    assert_eq!(ctx.fmt("s1", "#{pane_in_mode}"), "0");
}

#[test]
fn whole_command_lists_are_checked_before_running() {
    let ctx = Ctx::new("p1fix-precheck");
    ctx.ok(&["new-session", "-d", "-s", "s1", "sleep 100"]);
    // from the command line: nothing runs when any command is bad
    assert_eq!(ctx.fails(&["set", "-g", "@x", "1", ";", "neww", "-Q"]).trim(), "command new-window: unknown flag -Q");
    assert_eq!(ctx.fails(&["set", "-g", "@y", "1", ";", "nosuch"]).trim(), "unknown command: nosuch");
    ctx.fails(&["show-options", "-gv", "@x"]);
    ctx.fails(&["show-options", "-gv", "@y"]);
    // a file: the error has its file and line, and none of the file runs
    let dir = std::env::temp_dir().join(format!("pt-precheck-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let conf = dir.join("c.conf");
    std::fs::write(&conf, "set -g @a 1\n\nneww -Q\nset -g @b 1\n").unwrap();
    let path = conf.to_str().unwrap();
    assert_eq!(ctx.fails(&["source-file", path]).trim(), format!("{path}:3: command new-window: unknown flag -Q"));
    ctx.fails(&["show-options", "-gv", "@a"]);
    ctx.fails(&["show-options", "-gv", "@b"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_shell_status_and_new_session_sizes() {
    let ctx = Ctx::new("p1fix-rsds");
    ctx.ok(&["new-session", "-d", "-s", "f", "-x", "80", "-y", "24", "sleep 100"]);
    // the last run-shell gives the exit status
    assert_eq!(ctx.cmd(&["run-shell", "exit 3", ";", "run-shell", "true"]), (true, "'exit 3' returned 3\n".to_string()));
    let (ok, out) = ctx.cmd(&["run-shell", "true", ";", "run-shell", "exit 4"]);
    assert!(!ok && out == "'exit 4' returned 4\n", "{out}");
    // a -b job's failure is shown in a pane too
    ctx.ok(&["run-shell", "-b", "exit 5"]);
    wait_for("view mode", || ctx.fmt("f", "#{pane_in_mode}") == "1");
    // -x/-y: the default size as given, the other side from default-size
    ctx.ok(&["new-session", "-d", "-s", "d1", "-x", "50", "sleep 100"]);
    assert_eq!(ctx.ok(&["show-options", "-t", "d1", "default-size"]), "default-size 50x24\n");
    assert_eq!(ctx.fmt("d1", "#{window_width}x#{window_height}"), "50x24");
    assert_eq!(ctx.fails(&["new-session", "-d", "-y", "0"]).trim(), "height too small");
    assert_eq!(ctx.fails(&["new-session", "-d", "-x", "abc"]).trim(), "width invalid");
}

#[test]
fn copy_mode_format_variables() {
    let ctx = Ctx::new("p1fix-copyvars");
    ctx.ok(&["new-session", "-d", "-s", "s1", "-x", "60", "-y", "16", "printf 'hello world\\nworld two\\n'; sleep 100"]);
    wait_for("output", || ctx.capture("s1").contains("two"));
    // not in copy mode: no values, and display -a has none of them
    assert_eq!(ctx.fmt("s1", "[#{copy_cursor_word}#{selection_mode}]"), "[]");
    ctx.ok(&["copy-mode", "-t", "s1"]);
    ctx.ok(&["send-keys", "-t", "s1", "-X", "history-top"]);
    assert_eq!(ctx.fmt("s1", "#{copy_cursor_word}|#{copy_cursor_line}|#{selection_mode}|#{search_present}|#{search_count}"), "hello|hello world|char|0|0");
    let all = ctx.ok(&["display-message", "-a", "-t", "s1"]);
    for v in ["copy_cursor_hyperlink=", "copy_position=0", "copy_position_limit=0", "rectangle_toggle=0", "search_match=", "selection_active=0", "top_line_time=0"] {
        assert!(all.lines().any(|l| l == v), "{v} in display -a");
    }
    ctx.ok(&["send-keys", "-t", "s1", "-X", "search-forward", "wor"]);
    assert_eq!(ctx.fmt("s1", "#{search_present}|#{search_count}|#{search_match}"), "1|2|wor");
    ctx.ok(&["send-keys", "-t", "s1", "-X", "select-word"]);
    assert_eq!(ctx.fmt("s1", "#{selection_mode}|#{selection_active}|#{selection_present}"), "word|1|1");
    ctx.ok(&["send-keys", "-t", "s1", "-X", "rectangle-toggle"]);
    ctx.ok(&["send-keys", "-t", "s1", "-X", "select-line"]);
    assert_eq!(ctx.fmt("s1", "#{selection_mode}|#{rectangle_toggle}"), "line|0");
}

#[test]
fn display_message_verbose_loops_aliases_and_modifiers() {
    let ctx = Ctx::new("p1fix-dv2");
    ctx.ok(&["new-session", "-d", "-s", "m", "sleep 100"]);
    ctx.ok(&["set", "-g", "@a", "#{session_name}"]);
    // the whole format_log trace of tmux 3.8 for these
    let out = ctx.ok(&["display-message", "-v", "-p", "-t", "m", "#{E:@a}|#{W:#I}|#{=3:session_name}|#{s/m/M/:session_name}|#{nosuch}"]);
    assert_eq!(out, "# expanding format: #{E:@a}|#{W:#I}|#{=3:session_name}|#{s/m/M/:session_name}|#{nosuch}\n\
# found #{}: E:@a\n\
# modifier 0 is E\n\
# format '@a' found: #{session_name}\n\
#  expanding format: #{session_name}\n\
#  found #{}: session_name\n\
#  format 'session_name' found: m\n\
#  replaced 'session_name' with 'm'\n\
#  result is: m\n\
# replaced 'E:@a' with 'm'\n\
# found #{}: W:#I\n\
# modifier 0 is W\n\
# window loop: 0 @0\n\
#  expanding format: #I\n\
#  found #I: window_index\n\
#  format 'window_index' found: 0\n\
#  replaced 'window_index' with '0'\n\
#  result is: 0\n\
# replaced 'W:#I' with '0'\n\
# found #{}: =3:session_name\n\
#  expanding format: 3\n\
#  result is: 3\n\
# modifier 0 is =\n\
# modifier 0 argument 0: 3\n\
# format 'session_name' found: m\n\
# applied length limit 3: m\n\
# replaced '=3:session_name' with 'm'\n\
# found #{}: s/m/M/:session_name\n\
#  expanding format: m\n\
#  result is: m\n\
#  expanding format: M\n\
#  result is: M\n\
# modifier 0 is s\n\
# modifier 0 argument 0: m\n\
# modifier 0 argument 1: M\n\
# format 'session_name' found: m\n\
#  expanding format: m\n\
#  result is: m\n\
#  expanding format: M\n\
#  result is: M\n\
# substitute 'm' to 'M': M\n\
# replaced 's/m/M/:session_name' with 'M'\n\
# found #{}: nosuch\n\
# format 'nosuch' not found\n\
# replaced 'nosuch' with ''\n\
# result is: m|0|m|M|\n\
m|0|m|M|\n");
    let out = ctx.ok(&["display-message", "-v", "-p", "-t", "m", "#{l:#{x}}|#{?#{==:1,2},a,#{?1,b,c}}|#{p7:session_name}|#{n:session_name}"]);
    for want in ["# literal string is '#{x}'", "# no condition matched in '#{==:1,2},a,#{?1,b,c}'; using last arg",
        "#  condition '1' not found; assuming false", "# applied padding width 7: m      ", "# replacing with length: 1"] {
        assert!(out.lines().any(|l| l == want), "{want}: {out}");
    }
}

#[test]
fn global_colour_flags_accumulate_in_both_orders() {
    let ctx = Ctx::new("theme-global-flags");
    ctx.ok(&["new-session", "-d", "-s", "c"]);
    for flags in [["-2", "-T", "RGB", "-T", "sync"], ["-T", "sync", "-T", "RGB", "-2"]] {
        let mut child = ctx.command().args(flags).args(["-C", "attach", "-t", "c"])
            .env("TERM", "dumb").env_remove("COLORTERM")
            .stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
        wait_for("accumulated global features", || {
            let features = ctx.ok(&["list-clients", "-F", "#{client_termfeatures}"]);
            features.lines().any(|line| ["256", "RGB", "sync"].iter().all(|f| line.split(',').any(|v| v == *f)))
        });
        child.stdin.take().unwrap().write_all(b"\n").unwrap();
        wait_for("control client exit", || child.try_wait().unwrap().is_some());
    }
}

#[test]
fn display_defaults_match_tmux_38_source() {
    // The bindings fixture is the key-bindings.c string array with DEFAULT_* macros
    // expanded.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    #[cfg(unix)]
    {
        let fixture = root.join("src/server/display-defaults.conf");
        let reference = root.join("../ref/tmux-3.8/key-bindings.c");
        if reference.exists() {
            let generated = std::process::Command::new("python3").arg("tools/extract-display-defaults.py").arg(reference).output().unwrap();
            assert!(generated.status.success());
            assert_eq!(generated.stdout, std::fs::read(&fixture).unwrap(), "regenerate the fixture from tmux 3.8");
        }
    }
    // list-keys of a tmux 3.8 server started with -f /dev/null
    let expected = std::fs::read_to_string(root.join("tests/fixtures/tmux38-default-keys.txt")).unwrap();
    let mut sections: Vec<Vec<&str>> = Vec::new();
    for l in expected.lines() {
        if l.starts_with('#') { sections.push(Vec::new()) } else { sections.last_mut().unwrap().push(l) }
    }
    let ctx = Ctx::new("display-defaults");
    ctx.ok(&["new-session", "-d", "-s", "s"]);
    let actual = [ctx.ok(&["list-keys", "-T", "prefix"]), ctx.ok(&["list-keys", "-T", "root"]), ctx.ok(&["list-keys", "-N"]), ctx.ok(&["list-keys", "-T", "move"])];
    assert_eq!(sections.len(), actual.len());
    let key = |l: &str| -> String {
        let w: Vec<&str> = l.split_whitespace().collect();
        if w[0] == "bind-key" { w[if w[1] == "-r" { 4 } else { 3 }].to_string() } else { w[1].to_string() }
    };
    for (want, got) in sections.iter().zip(&actual) {
        let got: Vec<&str> = got.lines().collect();
        assert_eq!(want.iter().map(|l| key(l)).collect::<Vec<_>>(), got.iter().map(|l| key(l)).collect::<Vec<_>>());
        for (w, g) in want.iter().zip(&got) {
            assert_eq!(w, g);
        }
    }
    let prefix = &actual[0];
    assert!(prefix.contains("bind-key    -T prefix T       command-prompt -I \"#T\" { select-pane -T \"%%\" }"), "{prefix}");
    assert!(actual[1].contains("'Detach' 'd' {detach-client}"));
    assert!(ctx.ok(&["list-keys", "-T", "root", "MouseDown3Pane"]).contains("Paste #[underscore]#{=/9/...:buffer_sample},}"));
}

#[test]
fn display_formats_see_the_top_buffer_and_drop_list_markers() {
    let ctx = Ctx::new("display-buffer-vars");
    ctx.ok(&["new-session", "-d", "-s", "s", "-x", "80", "-y", "24"]);
    assert_eq!(ctx.fmt("s", "[#{buffer_size}]"), "[]");
    // format_defaults: buffer_* describe the most recent automatic buffer
    ctx.ok(&["set-buffer", "hello buffer"]);
    ctx.ok(&["set-buffer", "-b", "named", "not automatic"]);
    assert_eq!(ctx.fmt("s", "#{buffer_size}:#{buffer_sample}:#{buffer_name}"), "12:hello buffer:buffer0");
    assert_eq!(ctx.fmt("s", "#{?#{&&:#{buffer_size},#{!:#{pane_in_mode}}},Paste,}"), "Paste");
}

#[test]
fn display_layout_options_and_session_styles() {
    let ctx = Ctx::new("display-layouts");
    ctx.ok(&["new-session", "-d", "-s", "s", "-x", "120", "-y", "50"]);
    for _ in 0..5 { ctx.ok(&["split-window", "-h", "-t", "s"]); ctx.ok(&["select-layout", "-t", "s", "tiled"]); }
    ctx.ok(&["setw", "-t", "s", "tiled-layout-max-columns", "1"]);
    ctx.ok(&["select-layout", "-t", "s", "tiled"]);
    let rows = ctx.ok(&["list-panes", "-t", "s", "-F", "#{pane_left}:#{pane_width}"]);
    assert!(rows.lines().all(|s| s == "0:120"), "{rows}");
    ctx.ok(&["setw", "-t", "s", "main-pane-width", "30%"]);
    ctx.ok(&["setw", "-t", "s", "other-pane-width", "25%"]);
    ctx.ok(&["select-layout", "-t", "s", "main-vertical"]);
    assert_eq!(ctx.fmt("s.0", "#{pane_width}"), "90");
    ctx.ok(&["setw", "-t", "s", "main-pane-height", "10"]);
    ctx.ok(&["setw", "-t", "s", "other-pane-height", "20%"]);
    ctx.ok(&["select-layout", "-t", "s", "main-horizontal"]);
    assert_eq!(ctx.fmt("s.0", "#{pane_height}"), "40");
    // a size over the available space is invalid and the default 24 is used
    ctx.ok(&["setw", "-t", "s", "main-pane-height", "100"]);
    ctx.ok(&["setw", "-t", "s", "other-pane-height", "0"]);
    ctx.ok(&["select-layout", "-t", "s", "main-horizontal"]);
    assert_eq!(ctx.fmt("s.0", "#{pane_height}"), "24");
    ctx.ok(&["setw", "-t", "s", "session-status-style", "fg=red"]);
    ctx.ok(&["setw", "-t", "s", "session-status-current-style", "bold"]);
    assert_eq!(ctx.fmt("s", "#{E:session-status-style}/#{E:session-status-current-style}"), "fg=red/bold");
}

#[test]
fn remain_on_exit_format_is_styled_on_bottom_line_and_empty_is_silent() {
    let ctx = Ctx::new("display-dead");
    ctx.ok(&["new-session", "-d", "-s", "s", "-x", "60", "-y", "10"]);
    ctx.ok(&["set", "-p", "-t", "s", "remain-on-exit", "on"]);
    ctx.ok(&["set", "-p", "-t", "s", "remain-on-exit-format", "#[fg=red]EXIT=#{pane_dead_status}"]);
    ctx.ok(&["respawn-pane", "-k", "-t", "s", "sh", "-c", "exit 7"]);
    wait_for("dead pane", || ctx.fmt("s", "#{pane_dead}") == "1");
    let captured = ctx.ok(&["capture-pane", "-pe", "-t", "s"]);
    assert!(captured.lines().last().unwrap().contains("EXIT=7"), "{captured:?}");
    assert!(!captured.contains("#[fg="));
    assert!(captured.contains("\x1b["));
    ctx.ok(&["set", "-p", "-t", "s", "remain-on-exit-format", ""]);
    ctx.ok(&["respawn-pane", "-k", "-t", "s", "sh", "-c", "printf marker; exit 0"]);
    // the exit can be seen before the pane's last output is read
    wait_for("empty dead format", || ctx.fmt("s", "#{pane_dead}") == "1" && ctx.capture("s").starts_with("marker"));
    let captured = ctx.capture("s");
    assert!(captured.starts_with("marker"), "{captured:?}");
    assert!(!captured.contains("Pane is dead"));
}

#[cfg(unix)]
#[test]
fn pane_dead_signal_and_status_like_tmux_3_8() {
    let ctx = Ctx::new("dead-signal");
    ctx.ok(&["new-session", "-d", "-s", "s", "-x", "80", "-y", "24"]);
    ctx.wait_prompt("s");
    ctx.ok(&["set", "-g", "remain-on-exit", "on"]);
    ctx.ok(&["new-window", "-d", "-n", "k", "-t", "s", "kill -TERM $$"]);
    ctx.ok(&["new-window", "-d", "-n", "e", "-t", "s", "exit 7"]);
    let f = "#{pane_dead}:[#{pane_dead_status}]:[#{pane_dead_signal}]";
    wait_for("dead panes", || ctx.fmt("s:k", "#{pane_dead}") == "1" && ctx.fmt("s:e", "#{pane_dead}") == "1");
    assert_eq!(ctx.fmt("s:k", f), "1:[]:[15]");
    assert_eq!(ctx.fmt("s:e", f), "1:[7]:[]");
    wait_for("default format", || ctx.capture("s:k").contains("Pane is dead (signal 15, "));
}

#[test]
fn zoom_keeps_hidden_panes_beside_their_left_scrollbars() {
    let ctx = Ctx::new("zoom-sb-left");
    ctx.ok(&["new-session", "-d", "-s", "s", "-x", "80", "-y", "24", "sleep 300"]);
    ctx.ok(&["split-window", "-h", "-t", "s", "sleep 300"]);
    ctx.ok(&["split-window", "-v", "-t", "s", "sleep 300"]);
    ctx.ok(&["set", "-g", "pane-scrollbars", "on"]);
    ctx.ok(&["set", "-g", "pane-scrollbars-position", "left"]);
    let g = "#{P:#{pane_index}=#{pane_width}x#{pane_height}+#{pane_left}+#{pane_top} }";
    let before = ctx.fmt("s", g);
    assert_eq!(before, "0=39x24+1+0 1=38x12+42+0 2=38x11+42+13 ");
    ctx.ok(&["resize-pane", "-Z", "-t", "s:0.0"]);
    assert_eq!(ctx.fmt("s", g), "0=79x24+1+0 1=38x12+42+0 2=38x11+42+13 ");
}

#[test]
#[cfg(unix)]
fn copy_mode_live_word_separators_and_kill_flag() {
    let ctx = Ctx::new("copyopts");
    ctx.ok(&["new", "-d", "-s", "c", "-x", "40", "-y", "8", "printf 'one-two three\n'; sleep 100"]);
    wait_for("copy text", || ctx.capture("c").contains("one-two"));
    ctx.ok(&["set", "-w", "-t", "c", "mode-keys", "vi"]);
    ctx.ok(&["set", "-t", "c", "word-separators", " "]);
    ctx.ok(&["copy-mode", "-t", "c"]);
    ctx.ok(&["send", "-t", "c", "-X", "history-top"]);
    ctx.ok(&["send", "-t", "c", "-X", "next-word"]);
    assert_eq!(ctx.fmt("c", "#{copy_cursor_x}"), "8");
    ctx.ok(&["send", "-t", "c", "-X", "history-top"]);
    ctx.ok(&["set", "-t", "c", "word-separators", " -"]);
    ctx.ok(&["send", "-t", "c", "-X", "next-word"]);
    assert_eq!(ctx.fmt("c", "#{copy_cursor_x} #{copy_cursor_word}"), "3 two");
    ctx.ok(&["splitw", "-d", "-t", "c", "sleep 100"]);
    ctx.ok(&["copy-mode", "-q", "-t", "c:0.0"]);
    ctx.ok(&["copy-mode", "-k", "-t", "c:0.0"]);
    ctx.ok(&["copy-mode", "-q", "-t", "c:0.0"]);
    assert_eq!(ctx.fmt("c", "#{window_panes}"), "1");
}

#[test]
fn copy_mode_command_arguments_like_tmux() {
    let ctx = Ctx::new("copyargs");
    ctx.ok(&["new", "-d", "-s", "c", "-x", "30", "-y", "6", "printf 'foo bar\n-baz foo\nfob\nend\n'; sleep 100"]);
    wait_for("copy text", || ctx.capture("c").contains("end"));
    // send -X outside a mode fails (cmd-send-keys.c), and -N is checked first
    let e = ctx.fails(&["send", "-t", "c", "-X", "cursor-up"]);
    assert!(e.contains("not in a mode"), "{e}");
    let e = ctx.fails(&["send", "-t", "c", "-N", "0", "-X", "cursor-up"]);
    assert!(e.contains("repeat count too small"), "{e}");
    ctx.ok(&["copy-mode", "-t", "c"]);
    let at = || ctx.fmt("c", "#{copy_cursor_x},#{copy_cursor_y}");
    // an argument that looks like flags is parsed as flags, so the command does not run
    ctx.ok(&["send", "-t", "c", "-X", "search-backward", "-baz"]);
    assert_eq!(at(), "0,4");
    ctx.ok(&["send", "-t", "c", "-X", "search-backward", "--", "-baz"]);
    assert_eq!(at(), "0,1");
    ctx.ok(&["send", "-t", "c", "-X", "search-backward-incremental", "=foo"]);
    ctx.ok(&["send", "-t", "c", "-X", "search-backward-incremental", "-foo"]);
    assert_eq!(at(), "0,0");
    // the buffer name prefix and the pipe command are formats
    ctx.ok(&["send", "-t", "c", "-X", "copy-line", "P#{pane_index}x"]);
    ctx.ok(&["send", "-t", "c", "-X", "copy-line", "-X"]);
    ctx.ok(&["send", "-t", "c", "-X", "copy-pipe-line", "cat", "Q#{window_index}y"]);
    let names = ctx.ok(&["list-buffers", "-F", "#{buffer_name}"]);
    assert_eq!(names.lines().collect::<Vec<_>>(), ["Q0y1", "P0x0"], "{names}");
}

/// Checked against tmux 3.8: a copy with a buffer prefix is announced under its final
/// name, and append-selection replaces the top automatic buffer (paste_get_top) by its
/// data followed by the selection, under the same name and no longer automatic.
#[test]
fn copy_mode_buffer_prefix_and_append_selection() {
    use std::io::{BufRead, BufReader};
    use std::sync::{Arc, Mutex};
    let ctx = Ctx::new("copyappend");
    ctx.ok(&["new", "-d", "-s", "c", "-x", "30", "-y", "6", "printf 'foo bar\nbaz\n'; sleep 100"]);
    wait_for("copy text", || ctx.capture("c").contains("baz"));
    let mut child = ctx.command().args(["-C", "attach", "-t", "c"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let out = Arc::new(Mutex::new(String::new()));
    let stdout = child.stdout.take().unwrap();
    let o = out.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.starts_with("%paste-buffer") {
                o.lock().unwrap().push_str(&(line + "\n"));
            }
        }
    });
    wait_for("attached", || !ctx.ok(&["list-clients"]).is_empty());
    ctx.ok(&["copy-mode", "-t", "c"]);
    ctx.ok(&["send", "-t", "c", "-X", "history-top"]);
    ctx.ok(&["send", "-t", "c", "-X", "copy-line", "P#{pane_index}x"]);
    ctx.ok(&["set-buffer", "-b", "named", "zzz"]);
    ctx.ok(&["send", "-t", "c", "-X", "select-line"]);
    ctx.ok(&["send", "-t", "c", "-X", "append-selection"]);
    ctx.ok(&["send", "-t", "c", "-X", "cursor-down"]);
    ctx.ok(&["send", "-t", "c", "-X", "select-line"]);
    // the appended buffer is no longer automatic, so this one makes a new buffer
    ctx.ok(&["send", "-t", "c", "-X", "append-selection-and-cancel"]);
    assert_eq!(ctx.ok(&["list-buffers", "-F", "#{buffer_name}=#{buffer_sample}"]), "buffer1=baz\nP0x0=foo barfoo bar\nnamed=zzz\n");
    let want = "%paste-buffer-changed P0x0\n%paste-buffer-changed named\n%paste-buffer-deleted P0x0\n%paste-buffer-changed P0x0\n%paste-buffer-changed buffer1\n";
    wait_for("buffer notifications", || out.lock().unwrap().len() >= want.len());
    assert_eq!(out.lock().unwrap().as_str(), want);
    child.kill().unwrap();
    let _ = child.wait();
}

#[test]
#[cfg(unix)]
fn copy_mode_tables_counts_and_formats_with_the_server() {
    // checked against tmux 3.8: the default tables print as list-keys does, -N is the
    // mode's count for one -X command, display -a lists the mode's variables (callbacks
    // without a value empty) and a copy by a command writes no clipboard to it
    let ctx = Ctx::new("copyint");
    ctx.ok(&["new", "-d", "-s", "c", "-x", "30", "-y", "6", "printf 'a\nb\nc\nd\ne\n'; sleep 100"]);
    wait_for("copy text", || ctx.capture("c").contains("e"));
    let keys = ctx.ok(&["list-keys", "-T", "copy-mode-vi"]);
    for want in [
        r#"bind-key  -T copy-mode-vi F                 command-prompt -1P -p "(jump backward)" { send-keys -X jump-backward -- "%%" }"#,
        r"bind-key  -T copy-mode-vi MouseDrag1Pane    select-pane \; send-keys -X begin-selection",
        r##"bind-key  -T copy-mode-vi \#                send-keys -FX search-backward -- "#{copy_cursor_word}""##,
    ] {
        assert!(keys.lines().any(|l| l == want), "{want} not in {keys}");
    }
    ctx.ok(&["copy-mode", "-t", "c"]);
    ctx.ok(&["send", "-t", "c", "-N", "3", "-X", "cursor-up"]);
    assert_eq!(ctx.fmt("c", "#{copy_cursor_y}"), "2");
    ctx.ok(&["send", "-t", "c", "-X", "select-line"]);
    assert_eq!(ctx.ok(&["send", "-t", "c", "-X", "copy-selection"]), "");
    let all = ctx.ok(&["display-message", "-a", "-t", "c"]);
    for want in ["copy_cursor_hyperlink=\n", "copy_cursor_word=\n", "refresh_active=0\n", "search_count=0\n", "search_match=\n"] {
        assert!(all.contains(want), "{want} in {all}");
    }
    assert_eq!(ctx.ok(&["show-buffer"]), "c");
    // run-shell -b output is shown in view-mode
    ctx.ok(&["new-window", "-d", "-t", "c:1", "sleep 100"]);
    ctx.ok(&["run-shell", "-b", "-t", "c:1", "echo hi"]);
    wait_for("view mode", || ctx.fmt("c:1", "#{pane_in_mode} #{pane_mode}") == "1 view-mode");
}

#[test]
#[cfg(unix)]
fn copy_mode_automatic_refresh_waits_for_selection() {
    let ctx = Ctx::new("copyrefresh");
    let marker = std::env::temp_dir().join(&ctx.socket);
    let command = format!("printf 'ready\n'; for item in first second; do while [ ! -f '{}'.$item ]; do sleep 0.02; done; printf 'out:%s\n' \"$item\"; done; sleep 100", marker.display());
    ctx.ok(&["new", "-d", "-s", "c", "-x", "40", "-y", "8", &command]);
    wait_for("ready", || ctx.capture("c").contains("ready"));
    ctx.ok(&["copy-mode", "-t", "c"]);
    assert_eq!(ctx.fmt("c", "#{refresh_active}"), "0");
    std::fs::write(marker.with_extension("first"), "").unwrap();
    wait_for("first output", || ctx.capture("c").contains("out:first"));
    assert!(!ctx.ok(&["capturep", "-p", "-M", "-t", "c"]).contains("out:first"));
    ctx.ok(&["send", "-t", "c", "-X", "refresh-now"]);
    assert!(ctx.ok(&["capturep", "-p", "-M", "-t", "c"]).contains("out:first"));
    ctx.ok(&["send", "-t", "c", "-X", "refresh-on"]);
    assert_eq!(ctx.fmt("c", "#{refresh_active}"), "1");
    ctx.ok(&["send", "-t", "c", "-X", "begin-selection"]);
    std::fs::write(marker.with_extension("second"), "").unwrap();
    wait_for("second output", || ctx.capture("c").contains("out:second"));
    std::thread::sleep(Duration::from_millis(150));
    assert!(!ctx.ok(&["capturep", "-p", "-M", "-t", "c"]).contains("out:second"));
    ctx.ok(&["send", "-t", "c", "-X", "clear-selection"]);
    wait_for("automatic refresh", || ctx.ok(&["capturep", "-p", "-M", "-t", "c"]).contains("out:second"));
    ctx.ok(&["send", "-t", "c", "-X", "refresh-toggle"]);
    assert_eq!(ctx.fmt("c", "#{refresh_active}"), "0");
    let _ = std::fs::remove_file(marker.with_extension("first"));
    let _ = std::fs::remove_file(marker.with_extension("second"));
}

/// tmux 3.8 hooks are events with a payload: hook_* formats, where each kind of hook
/// runs, and set-hook -E / -R.
#[test]
fn tmux_38_hook_events_and_formats() {
    let ctx = Ctx::new("hookev");
    ctx.ok(&["new-session", "-d", "-s", "e", "-n", "first"]);
    // event hooks run after the command list, with the payload as formats
    ctx.ok(&["set-hook", "-g", "window-renamed", "set -gF @wr '#{hook}|#{hook_old_name}|#{hook_new_name}|#{hook_window_name}|#{hook_event}|#{window_name}'"]);
    assert_eq!(ctx.ok(&["rename-window", "-t", "e:0", "second", ";", "show", "-gqv", "@wr"]), "");
    assert_eq!(ctx.ok(&["show", "-gv", "@wr"]).trim(), "window-renamed|first|second|second|window-renamed|second");
    assert_eq!(ctx.ok(&["show-hooks", "-gw", "-F", "#{hook_fire_count}", "window-renamed"]).trim(), "1");
    // what a hook does fires no hooks itself
    ctx.ok(&["set-hook", "-g", "window-renamed", "rename-window -t e:0 'x#{hook_new_name}'"]);
    ctx.ok(&["rename-window", "-t", "e:0", "r"]);
    assert_eq!(ctx.fmt("e:0", "#W"), "xr");
    ctx.ok(&["set-hook", "-gu", "window-renamed"]);
    // after hooks run straight after the command, for its client: output reaches it
    ctx.ok(&["set-hook", "-g", "after-rename-window", "display -p 'after #{hook}|#{hook_arguments}|#{hook_argument_0}|#{hook_flag_t}'"]);
    assert_eq!(ctx.ok(&["rename-window", "-t", "e:0", "y"]).trim(), "after after-rename-window|-t e:0 y|y|e:0");
    ctx.ok(&["set-hook", "-gu", "after-rename-window"]);
    // command-error runs in place of the rest of the command list
    ctx.ok(&["set-hook", "-g", "command-error", "display -p 'failed #{hook_argument_0}'"]);
    let out = ctx.fails(&["rename-window", "-t", "nosuch:0", "zz", ";", "set", "-g", "@not", "run"]);
    assert!(out.contains("failed zz") && out.contains("can't find session: nosuch"), "{out}");
    // (show of a missing option is a command error too)
    assert!(ctx.fails(&["show", "-gv", "@not"]).contains("invalid option"));
    assert_eq!(ctx.ok(&["show-hooks", "-g", "-F", "#{hook_fire_count} #{?hook_fire_time,set,}", "command-error"]).trim(), "2 set");
    ctx.ok(&["set-hook", "-gu", "command-error"]);
    // set-hook -R runs a hook now
    ctx.ok(&["set-hook", "-g", "session-renamed", "display -p 'R #{hook}'"]);
    assert_eq!(ctx.ok(&["set-hook", "-R", "session-renamed"]).trim(), "R session-renamed");
    // set-hook -E fires a user event; @ hooks are user options shown by show-hooks
    ctx.ok(&["set-hook", "-g", "@ev", "set -gF @evo '#{hook}|#{hook_session_name}|#{hook_window_index}|#{hook_window_name}'"]);
    ctx.ok(&["set-hook", "-E", "-t", "e:0", "@ev"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@evo"]).trim(), "@ev|e|0|y");
    assert!(ctx.ok(&["show-hooks", "-g"]).starts_with("@ev \"set -gF @evo"));
    assert!(!ctx.ok(&["show", "-g"]).contains("@ev "));
    assert!(ctx.ok(&["show", "-gH"]).contains("@ev "));
    assert_eq!(ctx.fails(&["set-hook", "-E", "ev"]).trim(), "event name must start with @");
    assert_eq!(ctx.fails(&["set-hook", "-E"]).trim(), "missing argument");
    // window and pane hooks live at their own levels
    ctx.ok(&["new-window", "-d", "-t", "e:1", "-n", "other"]);
    ctx.ok(&["set-hook", "-w", "-t", "e:0", "window-renamed", "set -gF @w0 '#{hook_new_name}'"]);
    ctx.ok(&["rename-window", "-t", "e:1", "nope"]);
    assert!(ctx.fails(&["show", "-gv", "@w0"]).contains("invalid option"));
    ctx.ok(&["rename-window", "-t", "e:0", "yes"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@w0"]).trim(), "yes");
    assert_eq!(ctx.ok(&["show-hooks", "-w", "-t", "e:0"]).trim(), "window-renamed[0] set-option -Fg @w0 \"#{hook_new_name}\"");
    assert_eq!(ctx.ok(&["show-hooks", "-w", "-t", "e:1"]), "");
    ctx.ok(&["set-hook", "-p", "-t", "e:1", "pane-title-changed", "set -gF @pt '#{hook_new_title}'"]);
    ctx.ok(&["select-pane", "-t", "e:1", "-T", "titled"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@pt"]).trim(), "titled");
    // marked-pane-changed, window-pane-changed and pane-moved
    ctx.ok(&["set-hook", "-g", "marked-pane-changed", "set -gF @mk '#{hook_marked}#{?hook_new_pane,N,}#{?hook_old_pane,O,}'"]);
    ctx.ok(&["select-pane", "-m", "-t", "e:0"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@mk"]).trim(), "1N");
    ctx.ok(&["select-pane", "-M"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@mk"]).trim(), "0O");
    ctx.ok(&["set-hook", "-g", "pane-moved", "set -gF @mv '#{hook_old_window_index}>#{hook_window_index}'"]);
    ctx.ok(&["set-hook", "-g", "window-pane-changed", "set -gaF @wpc '#{?hook_old_pane,o,}#{?hook_new_pane,n,}'"]);
    ctx.ok(&["split-window", "-t", "e:0"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@wpc"]).trim(), "on");
    ctx.ok(&["break-pane", "-d", "-s", "e:0.1", "-t", "e:5"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@mv"]).trim(), "0>5");
    // Cross-window swaps report source before destination, independent of window order.
    ctx.ok(&["new-window", "-d", "-t", "e:6"]);
    ctx.ok(&["new-window", "-d", "-t", "e:7"]);
    let source = ctx.fmt("e:7", "#{pane_id}");
    let destination = ctx.fmt("e:6", "#{pane_id}");
    ctx.ok(&["set-hook", "-g", "pane-moved", "set -gaF @swap '[#{hook_pane}]'"]);
    ctx.ok(&["swap-pane", "-d", "-s", &source, "-t", &destination]);
    assert_eq!(ctx.ok(&["show", "-gv", "@swap"]).trim(), format!("[{source}][{destination}]"));
    // the names hooks accept as events
    assert_eq!(ctx.fails(&["wait-for", "-E", "nosuch"]).trim(), "invalid event: nosuch");
    assert_eq!(ctx.fails(&["wait-for", "-E", "after-has-session"]).trim(), "invalid event: after-has-session");
}

/// wait-for -E waits for events (-F a true format, -v prints the payload); -l lists
/// waiters and -w wakes one, for channels too.
#[test]
fn tmux_38_wait_for_events() {
    let ctx = Ctx::new("waitev");
    ctx.ok(&["new-session", "-d", "-s", "w", "-n", "start"]);
    let child = ctx.command().args(["wait-for", "-v", "-E", "-F", "#{==:#{new_name},yes}", "window-renamed"]).stdout(Stdio::piped()).spawn().unwrap();
    wait_for("event waiter", || ctx.ok(&["wait-for", "-E", "-l", "window-renamed"]).starts_with("client-"));
    ctx.ok(&["rename-window", "-t", "w:0", "no"]);
    std::thread::sleep(Duration::from_millis(300));
    assert!(ctx.ok(&["wait-for", "-E", "-l", "window-renamed"]).starts_with("client-"), "woken by a false format");
    ctx.ok(&["rename-window", "-t", "w:0", "yes"]);
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let wid = ctx.fmt("w:0", "#{window_id}");
    assert!(text.contains(&format!("event=window-renamed\nnew_name=no\nold_name=start\nwindow={wid}\n")), "{text}");
    assert!(text.ends_with(&format!("event=window-renamed\nnew_name=yes\nold_name=no\nwindow={wid}\n")), "{text}");
    // -F sees the payload but not tmux's own variables, which have no target there
    let mut child = ctx.command().args(["wait-for", "-E", "-F", "#{==:#{window_name},again}", "window-renamed"]).spawn().unwrap();
    wait_for("shadowed waiter", || !ctx.ok(&["wait-for", "-E", "-l", "window-renamed"]).is_empty());
    ctx.ok(&["rename-window", "-t", "w:0", "again"]);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!ctx.ok(&["wait-for", "-E", "-l", "window-renamed"]).is_empty());
    child.kill().unwrap();
    let _ = child.wait();
    wait_for("shadowed waiter gone", || ctx.ok(&["wait-for", "-E", "-l", "window-renamed"]).is_empty());
    assert_eq!(ctx.fails(&["wait-for", "-l"]).trim(), "command wait-for: too few arguments (need at least 1)");
    // set-hook -E: the event carries the client and the target
    let child = ctx.command().args(["wait-for", "-v", "-E", "@user"]).stdout(Stdio::piped()).spawn().unwrap();
    wait_for("user event waiter", || !ctx.ok(&["wait-for", "-E", "-l", "@user"]).is_empty());
    ctx.ok(&["set-hook", "-E", "-t", "w:0", "@user"]);
    let text = String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into_owned();
    assert!(text.starts_with("client=client-") && text.contains("event=@user\n") && text.contains("window_index=0\n"), "{text}");
    // -w wakes a waiter by name
    let mut child = ctx.command().args(["wait-for", "-E", "@other"]).spawn().unwrap();
    wait_for("waiter listed", || !ctx.ok(&["wait-for", "-E", "-l", "@other"]).is_empty());
    let name = ctx.ok(&["wait-for", "-E", "-l", "@other"]).trim().to_string();
    assert_eq!(ctx.fails(&["wait-for", "-E", "-w", "nobody", "@other"]).trim(), "waiter nobody not found");
    ctx.ok(&["wait-for", "-E", "-w", &name, "@other"]);
    wait_for("woken waiter", || child.try_wait().unwrap().is_some());
    let mut child = ctx.command().args(["wait-for", "chan"]).spawn().unwrap();
    wait_for("channel waiter", || !ctx.ok(&["wait-for", "-l", "chan"]).is_empty());
    let name = ctx.ok(&["wait-for", "-l", "chan"]).trim().to_string();
    ctx.ok(&["wait-for", "-w", "nobody", "chan"]);
    ctx.ok(&["wait-for", "-w", &name, "chan"]);
    wait_for("woken channel waiter", || child.try_wait().unwrap().is_some());
    // a waiter that goes away does not run the rest of its commands
    let mut child = ctx.command().args(["wait-for", "gone", ";", "set", "-g", "@ran", "yes"]).spawn().unwrap();
    wait_for("waiter on gone", || !ctx.ok(&["wait-for", "-l", "gone"]).is_empty());
    child.kill().unwrap();
    let _ = child.wait();
    wait_for("waiter dropped", || ctx.ok(&["wait-for", "-l", "gone"]).is_empty());
    ctx.ok(&["wait-for", "-S", "gone"]);
    std::thread::sleep(Duration::from_millis(200));
    assert!(ctx.fails(&["show", "-gv", "@ran"]).contains("invalid option"));
    // hooks run from the global queue cannot wait
    ctx.ok(&["set-hook", "-g", "session-renamed", "wait-for -E @inner"]);
    ctx.ok(&["rename-session", "-t", "w", "w2"]);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(ctx.ok(&["wait-for", "-E", "-l", "@inner"]), "");
}

/// set-hook -B: monitor hooks check a format every second and fire when it changes
/// (-T: when it becomes true).
#[test]
fn tmux_38_monitor_hooks() {
    let ctx = Ctx::new("monhook");
    ctx.ok(&["new-session", "-d", "-s", "m"]);
    ctx.ok(&["set", "-g", "@v", "1"]);
    ctx.ok(&["set-hook", "-g", "-B", "@mon::#{@v}", "set -gF @got '#{hook}|#{hook_value}|#{hook_last}|#{hook_session_name}'"]);
    ctx.ok(&["set-hook", "-g", "-T", "-B", "@t:@*:#{==:#{@v},3}", "set -gaF @true '[#{hook_value}|#{hook_window_index}]'"]);
    assert_eq!(ctx.ok(&["show-hooks", "-gB"]), "@mon::#{@v}\n@t:@*:#{==:#{@v},3}\n");
    assert!(ctx.ok(&["show", "-g", "@mon"]).starts_with("@mon \"set -gF @got"));
    // the first value is only recorded
    std::thread::sleep(Duration::from_millis(1500));
    assert!(ctx.fails(&["show", "-gv", "@got"]).contains("invalid option"));
    ctx.ok(&["set", "-g", "@v", "2"]);
    wait_for("monitor fired", || ctx.cmd(&["show", "-gv", "@got"]).1.trim() == "@mon|2|1|m");
    assert_eq!(ctx.ok(&["show-hooks", "-gB", "-F", "#{option_name}|#{hook_monitor_target}|#{hook_fire_count}|#{?hook_fire_time,set,}", "@mon"]).trim(), "@mon||1|set");
    ctx.ok(&["set", "-g", "@v", "3"]);
    wait_for("true monitor fired", || ctx.cmd(&["show", "-gv", "@true"]).1.trim() == "[1|0]");
    ctx.ok(&["set", "-g", "@v", "4"]);
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(ctx.ok(&["show", "-gv", "@true"]).trim(), "[1|0]");
    assert_eq!(ctx.fails(&["set-hook", "-g", "-B", "mon::x"]).trim(), "monitor hook name must start with @");
    assert_eq!(ctx.fails(&["set-hook", "-g", "-B", "bad"]).trim(), "invalid subscription: bad");
    assert_eq!(ctx.fails(&["set-hook", "-g", "-B", "@x:zz:y"]).trim(), "invalid subscription: @x:zz:y");
    ctx.ok(&["set-hook", "-gu", "-B", "@mon"]);
    assert_eq!(ctx.ok(&["show-hooks", "-gB"]), "@t:@*:#{==:#{@v},3}\n");
    // the option stays: unsetting it removes the monitor too
    assert!(ctx.ok(&["show", "-g", "@mon"]).starts_with("@mon "));
    ctx.ok(&["set-hook", "-gu", "@t"]);
    assert_eq!(ctx.ok(&["show-hooks", "-gB"]), "");
}

/// Control mode notifications are events: other sessions' windows, paste buffers and
/// clients are reported, after the reply of the command that caused them.
#[test]
fn tmux_38_control_notifications() {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::{Arc, Mutex};
    let ctx = Ctx::new("ctlnotify");
    ctx.ok(&["new-session", "-d", "-s", "c"]);
    ctx.ok(&["new-session", "-d", "-s", "other"]);
    let mut child = ctx.command().args(["-C", "attach", "-t", "c"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let out = Arc::new(Mutex::new(String::new()));
    {
        let out = out.clone();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                out.lock().unwrap().push_str(&(line + "\n"));
            }
        });
    }
    let mut stdin = child.stdin.take().unwrap();
    let mut send = |l: &str| {
        stdin.write_all(format!("{l}\n").as_bytes()).unwrap();
        stdin.flush().unwrap();
    };
    let text = || out.lock().unwrap().clone();
    wait_for("attached", || text().contains("%session-changed"));
    // the attach command came from the command line (flags 0), after its %end
    let t = text();
    let first = t.lines().next().unwrap_or("");
    assert!(first.starts_with("%begin ") && first.ends_with(" 0"), "{t}");
    assert!(t.find("%end ").unwrap() < t.find("%session-changed").unwrap(), "{t}");
    send("new-window -n late");
    wait_for("%window-add", || text().contains("%window-add @"));
    let t = text();
    let begin = t.rfind("%begin ").unwrap();
    assert!(t[begin..].lines().next().unwrap().ends_with(" 1"), "{t}");
    assert!(t.rfind("%window-add").unwrap() > begin + t[begin..].find("%end ").unwrap(), "{t}");
    send("display -p first-item; display -p second-item");
    wait_for("individual command replies", || text().contains("second-item\n%end "));
    let t = text();
    let first = t.find("first-item\n").unwrap();
    let second = t.find("second-item\n").unwrap();
    assert!(t[first..second].contains("%end ") && t[first..second].contains("%begin "), "{t}");
    send("display-message hello");
    wait_for("control message", || text().contains("%message hello\n%end "));
    let pane = ctx.fmt("c:", "#{pane_id}");
    send(&format!("refresh-client -A '{pane}:pause'"));
    wait_for("pause notification", || text().contains(&format!("%pause {pane}\n")));
    let t = text();
    assert!(t.rfind("%end ").unwrap() < t.rfind("%pause ").unwrap(), "{t}");
    send(&format!("refresh-client -A '{pane}:continue'"));
    wait_for("continue notification", || text().contains(&format!("%continue {pane}\n")));
    let t = text();
    assert!(t.rfind("%end ").unwrap() < t.rfind("%continue ").unwrap(), "{t}");
    send("wait-for control-paused");
    send("display -p after-control-wait");
    wait_for("control waiter registered", || !ctx.ok(&["wait", "-l", "control-paused"]).is_empty());
    assert!(!text().contains("after-control-wait"));
    ctx.ok(&["wait", "-S", "control-paused"]);
    wait_for("ordered control continuation", || text().contains("after-control-wait\n%end "));
    let other = ctx.fmt("other:", "#{session_id}");
    ctx.ok(&["new-window", "-d", "-t", "other:", "-n", "elsewhere"]);
    wait_for("%unlinked-window-add", || text().contains("%unlinked-window-add @"));
    ctx.ok(&["rename-window", "-t", "other:0", "away"]);
    wait_for("%unlinked-window-renamed", || text().contains(" away\n") && text().contains("%unlinked-window-renamed @"));
    ctx.ok(&["rename-session", "-t", "other", "oth"]);
    wait_for("%session-renamed", || text().contains(&format!("%session-renamed {other} oth\n")));
    ctx.ok(&["set-buffer", "-b", "nb", "data"]);
    wait_for("%paste-buffer-changed", || text().contains("%paste-buffer-changed nb\n"));
    ctx.ok(&["delete-buffer", "-b", "nb"]);
    wait_for("%paste-buffer-deleted", || text().contains("%paste-buffer-deleted nb\n"));
    ctx.ok(&["select-window", "-t", "oth:1"]);
    wait_for("%session-window-changed", || text().contains(&format!("%session-window-changed {other} @")));
    send("refresh-client -f new-layouts");
    // detaching a control client prints a bare %exit, after the reply
    send("detach-client");
    wait_for("client exit", || child.try_wait().unwrap().is_some());
    wait_for("%exit", || text().contains("%exit"));
    let t = text();
    let lines: Vec<&str> = t.lines().rev().take(3).collect();
    assert!(lines[0] == "%exit" && lines[1].starts_with("%end ") && lines[2].starts_with("%begin "), "{t}");
}

/// The order of tmux 3.8's window and session events, as hooks see them: swap-window
/// links nothing, session groups link every window again when one member changes,
/// windows close after their last unlink and sessions close after what the command did.
#[test]
fn tmux_38_event_order() {
    let ctx = Ctx::new("evorder");
    ctx.ok(&["new-session", "-d", "-s", "a", "-x", "80", "-y", "24"]);
    for e in ["window-created", "window-closed", "window-linked", "window-unlinked", "session-window-changed", "session-created", "session-closed", "session-added-to-group", "session-removed-from-group", "window-layout-changed", "window-zoomed", "window-unzoomed", "window-resized", "pane-created", "pane-moved"] {
        ctx.ok(&["set-hook", "-g", e, "set -gaF @log '#{hook}<#{hook_session_name}:#{hook_window_index}> '"]);
    }
    let log = |ctx: &Ctx| -> String {
        let (_, out) = ctx.cmd(&["show", "-gqv", "@log"]);
        ctx.ok(&["set", "-gu", "@log"]);
        out.trim().to_string()
    };
    // (as tmux 3.8 prints them)
    ctx.ok(&["new-window", "-t", "a:1"]);
    assert_eq!(log(&ctx), "pane-created<a:1> session-window-changed<a:1> window-created<:> window-linked<a:1>");
    ctx.ok(&["new-window", "-d", "-t", "a:2"]);
    assert_eq!(log(&ctx), "pane-created<a:2> window-created<:> window-linked<a:2>");
    // the current winlink stays current with another window in it
    let before = ctx.fmt("a", "#{window_id}");
    ctx.ok(&["swap-window", "-s", "a:1", "-t", "a:2"]);
    assert_eq!(log(&ctx), "");
    assert_eq!(ctx.fmt("a", "#{window_index}"), "1");
    assert_ne!(ctx.fmt("a", "#{window_id}"), before);
    // -d selects the target index, keeping the window
    ctx.ok(&["swap-window", "-d", "-s", "a:1", "-t", "a:0"]);
    assert_eq!(log(&ctx), "session-window-changed<a:0>");
    assert_eq!(ctx.fmt("a", "#{window_index}"), "0");
    // a group: the new session spawns a window of its own, joins the group, gets
    // every window and loses its own (session_group_synchronize_to)
    ctx.ok(&["new-session", "-d", "-s", "b", "-t", "a"]);
    assert_eq!(log(&ctx), "pane-created<b:0> window-created<:> window-linked<b:0> session-added-to-group<a:> session-added-to-group<b:> window-linked<b:0> window-linked<b:1> window-linked<b:2> window-unlinked<b:0> window-closed<:> session-created<b:>");
    ctx.ok(&["new-window", "-d", "-t", "a:3"]);
    assert_eq!(log(&ctx), "pane-created<a:3> window-created<:> window-linked<a:3> window-linked<b:0> window-linked<b:1> window-linked<b:2> window-linked<b:3>");
    ctx.ok(&["kill-window", "-t", "a:3"]);
    assert_eq!(log(&ctx), "window-unlinked<a:3> window-linked<b:0> window-linked<b:1> window-linked<b:2> window-unlinked<b:3> window-closed<:>");
    ctx.ok(&["kill-session", "-t", "b"]);
    assert_eq!(log(&ctx), "session-closed<b:> session-removed-from-group<b:> window-unlinked<b:0> window-unlinked<b:1> window-unlinked<b:2>");
    // a session closed by moving its last window away goes after the move
    ctx.ok(&["new-session", "-d", "-s", "c"]);
    assert_eq!(log(&ctx), "pane-created<c:0> window-created<:> window-linked<c:0> session-created<c:>");
    ctx.ok(&["move-window", "-s", "c:0", "-t", "a:7"]);
    assert_eq!(log(&ctx), "window-linked<a:7> session-window-changed<a:7> session-closed<c:> window-unlinked<c:0>");
    // linking over a window closes it before the new link
    ctx.ok(&["new-session", "-d", "-s", "d"]);
    log(&ctx);
    ctx.ok(&["link-window", "-k", "-s", "a:1", "-t", "d:0"]);
    assert_eq!(log(&ctx), "window-unlinked<d:0> window-closed<:> window-linked<d:0> session-window-changed<d:0>");
    // layouts: select-layout and resize-pane fire it themselves too; rotate-window does not
    ctx.ok(&["split-window", "-d", "-t", "a:0"]);
    assert_eq!(log(&ctx), "pane-created<a:0> window-layout-changed<:>");
    ctx.ok(&["select-layout", "-t", "a:0", "even-horizontal"]);
    assert_eq!(log(&ctx), "window-layout-changed<:> window-layout-changed<:>");
    ctx.ok(&["select-layout", "-t", "a:0", "even-horizontal"]);
    assert_eq!(log(&ctx), "window-layout-changed<:> window-layout-changed<:>");
    ctx.ok(&["select-layout", "-t", "a:1", "tiled"]);
    assert_eq!(log(&ctx), "window-layout-changed<:>");
    ctx.ok(&["rotate-window", "-t", "a:0"]);
    assert_eq!(log(&ctx), "");
    // resizing a zoomed window unzooms and zooms it again
    ctx.ok(&["resize-pane", "-Z", "-t", "a:0"]);
    assert_eq!(log(&ctx), "window-zoomed<:> window-layout-changed<:>");
    ctx.ok(&["resize-window", "-t", "a:0", "-x", "60", "-y", "20"]);
    assert_eq!(log(&ctx), "window-unzoomed<:> window-layout-changed<:> window-zoomed<:> window-layout-changed<:> window-layout-changed<:> window-resized<:>");
    ctx.ok(&["resize-pane", "-Z", "-t", "a:0"]);
    log(&ctx);
    // swap-pane across windows moves both panes
    ctx.ok(&["swap-pane", "-d", "-s", "a:0.0", "-t", "a:7.0"]);
    let out = log(&ctx);
    assert!(out.contains("pane-moved<:7>") && out.contains("pane-moved<:0>"), "{out}");
}

/// A client whose session is destroyed is detached as it goes (client-detached after
/// session-closed); detaching takes the pane's focus first.
#[test]
fn tmux_38_client_detach_events() {
    let ctx = Ctx::new("clidet");
    ctx.ok(&["new-session", "-d", "-s", "k"]);
    ctx.ok(&["new-session", "-d", "-s", "other"]);
    ctx.ok(&["set", "-g", "focus-events", "on"]);
    for e in ["client-detached", "session-closed", "pane-focus-in", "pane-focus-out"] {
        ctx.ok(&["set-hook", "-g", e, "set -gaF @log '#{hook} '"]);
    }
    let attach = |ctx: &Ctx| {
        let child = ctx.command().args(["-C", "attach", "-t", "k"]).stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
        wait_for("attached", || ctx.ok(&["list-clients"]).contains("k"));
        child
    };
    let mut child = attach(&ctx);
    ctx.ok(&["detach-client", "-s", "k"]);
    wait_for("detached", || child.try_wait().unwrap().is_some());
    wait_for("hooks", || ctx.cmd(&["show", "-gv", "@log"]).1.contains("client-detached"));
    assert_eq!(ctx.ok(&["show", "-gv", "@log"]).trim(), "pane-focus-in pane-focus-out client-detached");
    ctx.ok(&["set", "-gu", "@log"]);
    let mut child = attach(&ctx);
    ctx.ok(&["kill-session", "-t", "k"]);
    wait_for("exited", || child.try_wait().unwrap().is_some());
    wait_for("hooks", || ctx.cmd(&["show", "-gv", "@log"]).1.contains("client-detached"));
    assert_eq!(ctx.ok(&["show", "-gv", "@log"]).trim(), "pane-focus-in session-closed client-detached");
}

/// Changing the active pane updates focus (if focus-events is on) before
/// window-pane-changed fires.
#[test]
fn tmux_38_focus_then_pane_changed() {
    let ctx = Ctx::new("focusord");
    ctx.ok(&["new-session", "-d", "-s", "f"]);
    ctx.ok(&["split-window", "-d", "-t", "f"]);
    for e in ["pane-focus-in", "pane-focus-out", "window-pane-changed"] {
        ctx.ok(&["set-hook", "-g", e, "set -gaF @log '#{hook} '"]);
    }
    let mut child = ctx.command().args(["-C", "attach", "-t", "f"]).stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
    wait_for("focused", || ctx.cmd(&["show", "-gv", "@log"]).1.contains("pane-focus-in"));
    ctx.ok(&["set", "-gu", "@log"]);
    ctx.ok(&["select-pane", "-t", "f:0.1"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@log"]).trim(), "window-pane-changed");
    ctx.ok(&["set", "-gu", "@log"]);
    // (the first pane kept its focus: nothing changes going back to it)
    ctx.ok(&["set", "-g", "focus-events", "on"]);
    ctx.ok(&["select-pane", "-t", "f:0.0"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@log"]).trim(), "window-pane-changed");
    ctx.ok(&["set", "-gu", "@log"]);
    ctx.ok(&["select-pane", "-t", "f:0.1"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@log"]).trim(), "pane-focus-out pane-focus-in window-pane-changed");
    child.kill().unwrap();
    let _ = child.wait();
}

/// Checked against tmux 3.8: a window with an open menu has no focused pane
/// (window_pane_update_focus); opening a menu over another closes that one first.
#[test]
fn tmux_38_menu_takes_pane_focus() {
    let ctx = Ctx::new("menufocus");
    ctx.ok(&["new-session", "-d", "-s", "f", "-x", "80", "-y", "24"]);
    ctx.ok(&["set", "-g", "focus-events", "on"]);
    for e in ["pane-focus-in", "pane-focus-out"] {
        ctx.ok(&["set-hook", "-g", e, "set -gaF @log '#{hook} '"]);
    }
    let mut child = ctx.command().args(["-C", "attach", "-t", "f"]).stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
    wait_for("focused", || ctx.cmd(&["show", "-gv", "@log"]).1.contains("pane-focus-in"));
    let log = || {
        let l = ctx.cmd(&["show", "-gv", "@log"]).1.trim().to_string();
        ctx.ok(&["set", "-gu", "@log"]);
        l
    };
    log();
    ctx.ok(&["display-menu", "-t", "f", "-x", "0", "-y", "0", "A", "a", "display a", "B", "b", "display b"]);
    assert_eq!(log(), "pane-focus-out");
    ctx.ok(&["display-menu", "-t", "f", "-x", "0", "-y", "0", "C", "c", "display c"]);
    assert_eq!(log(), "pane-focus-in pane-focus-out");
    let client = ctx.ok(&["list-clients", "-F", "#{client_name}"]).lines().next().unwrap().to_string();
    ctx.ok(&["send-keys", "-K", "-c", &client, "Escape"]);
    wait_for("menu closed", || ctx.cmd(&["show", "-gv", "@log"]).1.contains("pane-focus-in"));
    assert_eq!(log(), "pane-focus-in");
    child.kill().unwrap();
    let _ = child.wait();
}

/// A channel's waiters and lockers are independent; -w does not release a lock.
#[test]
fn tmux_38_wait_channel_lockers() {
    let ctx = Ctx::new("waitlock38");
    ctx.ok(&["new-session", "-d", "-s", "w"]);
    ctx.ok(&["wait", "-L", "mixed"]);
    let mut locker = ctx.command().args(["wait", "-L", "mixed"]).spawn().unwrap();
    wait_for("locker", || !ctx.ok(&["wait", "-l", "mixed"]).is_empty());
    let mut waiter = ctx.command().args(["wait", "mixed"]).spawn().unwrap();
    wait_for("both channel queues", || ctx.ok(&["wait", "-l", "mixed"]).lines().count() == 2);
    // -l prints waiters first, even though the locker arrived first.
    let names = ctx.ok(&["wait", "-l", "mixed"]);
    assert_eq!(names.lines().nth(1).unwrap(), format!("client-{}", locker.id()));
    ctx.ok(&["wait", "-S", "mixed"]);
    wait_for("signal wakes waiter", || waiter.try_wait().unwrap().is_some());
    assert!(locker.try_wait().unwrap().is_none());
    assert_eq!(ctx.ok(&["wait", "-l", "mixed"]).trim(), names.lines().nth(1).unwrap());
    ctx.ok(&["wait", "-w", names.lines().nth(1).unwrap(), "mixed"]);
    wait_for("explicitly woken locker", || locker.try_wait().unwrap().is_some());
    ctx.ok(&["wait", "-U", "mixed"]);
    assert_eq!(ctx.fails(&["wait", "-U", "mixed"]).trim(), "channel mixed not locked");
    ctx.ok(&["wait", "-w", "missing", "mixed"]);
}

#[test]
fn tmux_38_empty_event_filter_and_closed_window() {
    let ctx = Ctx::new("eventedge38");
    ctx.ok(&["new-session", "-d", "-s", "e"]);
    assert_eq!(ctx.fails(&["wait", "-El", "pane-exited[0]"]).trim(), "invalid event: pane-exited[0]");
    let mut waiter = ctx.command().args(["wait", "-EF", "", "@empty"]).spawn().unwrap();
    wait_for("empty-filter waiter", || !ctx.ok(&["wait", "-El", "@empty"]).is_empty());
    ctx.ok(&["set-hook", "-E", "@empty"]);
    let name = ctx.ok(&["wait", "-El", "@empty"]);
    assert!(!name.is_empty());
    assert!(waiter.try_wait().unwrap().is_none());
    ctx.ok(&["wait", "-Ew", name.trim(), "@empty"]);
    wait_for("wake filtered waiter", || waiter.try_wait().unwrap().is_some());
    ctx.ok(&["set-hook", "-g", "window-closed", "set -gF @closed '#{hook_window_name}'"]);
    ctx.ok(&["new-window", "-d", "-t", "e:1", "-n", "gone"]);
    ctx.ok(&["kill-window", "-t", "e:1"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@closed"]).trim(), "gone");
}

#[test]
fn tmux_38_hook_groups_and_suspended_context() {
    let ctx = Ctx::new("hookgroups38");
    ctx.ok(&["new-session", "-d", "-s", "h"]);
    ctx.ok(&["set-hook", "-g", "after-new-window[0]", "display -p first; has-session -t absent; display -p skipped"]);
    ctx.ok(&["set-hook", "-g", "after-new-window[1]", "display -p second"]);
    let out = ctx.fails(&["new-window", "-d"]);
    assert!(out.contains("first\nsecond\n") && out.contains("can't find session: absent") && !out.contains("skipped"), "{out}");
    ctx.ok(&["set-hook", "-gu", "after-new-window"]);
    ctx.ok(&["set-hook", "-g", "after-rename-window", "wait-for paused; display -p 'hook:#{hook}'"]);
    let child = ctx.command().args(["rename-window", "-t", "h:0", "changed", ";", "display", "-p", "after:#{hook}"])
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    wait_for("hook waiting", || !ctx.ok(&["wait", "-l", "paused"]).is_empty());
    ctx.ok(&["wait", "-S", "paused"]);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hook:after-rename-window\nafter:\n");
}

/// A read can contain several titles and OSC commands; their payloads keep parsing order.
#[cfg(unix)]
#[test]
fn tmux_38_interleaved_pane_output_events() {
    let ctx = Ctx::new("outputevents38");
    ctx.ok(&["new-session", "-d", "-s", "h", "sleep 100"]);
    for event in ["pane-title-changed", "pane-bell", "pane-shell-prompt", "pane-command-started", "pane-command-finished"] {
        ctx.ok(&["set-hook", "-g", event, "set -gaF @out '#{hook}:#{hook_new_title}:#{hook_command_status} '"]);
    }
    ctx.ok(&["new-window", "-d", "-t", "h:1", "sh", "-c",
        r"sleep .3; printf '\033]2;one\007\033]133;A\007\033]133;C\007\007\033]2;two\007\033]133;D;7\007'; sleep 100"]);
    wait_for("command finish event", || ctx.cmd(&["show", "-gv", "@out"]).1.contains("pane-command-finished"));
    assert_eq!(ctx.ok(&["show", "-gv", "@out"]).trim(),
        "pane-title-changed:one: pane-shell-prompt:: pane-command-started:: pane-bell:: pane-title-changed:two: pane-command-finished::7");
    ctx.ok(&["set", "-p", "-t", "h:1", "allow-set-title", "off"]);
    ctx.ok(&["set", "-g", "@out", ""]);
    ctx.ok(&["respawn-pane", "-k", "-t", "h:1", "sh", "-c", r"sleep .3; printf '\033]2;ignored\007\007'; sleep 100"]);
    wait_for("bell after suppressed title", || ctx.cmd(&["show", "-gv", "@out"]).1.contains("pane-bell"));
    assert_eq!(ctx.ok(&["show", "-gv", "@out"]).trim(), "pane-bell::");
}

#[cfg(unix)]
#[test]
fn tmux_38_split_title_and_title_stack_events() {
    let ctx = Ctx::new("titlestack38");
    ctx.ok(&["new-session", "-d", "-s", "h", "sleep 100"]);
    ctx.ok(&["set-hook", "-g", "pane-title-changed", "set -gaF @titles '|#{hook_new_title}'"]);
    ctx.ok(&["split-window", "-d", "-T", "explicit", "sh", "-c",
        r"sleep .3; printf '\033]2;base\007\033[22;0t\033]2;temp\007\033[23;0t\033]2;base\007'; sleep 100"]);
    wait_for("title stack events", || ctx.cmd(&["show", "-gv", "@titles"]).1.trim() == "|explicit|base|temp|base|base");
}

#[test]
fn tmux_38_group_removal_and_monitor_session_payload() {
    let ctx = Ctx::new("groupmonitor38");
    ctx.ok(&["new-session", "-d", "-s", "a"]);
    ctx.ok(&["new-session", "-d", "-s", "b", "-t", "a"]);
    ctx.ok(&["set-hook", "-g", "session-removed-from-group", "set -gF @removed '#{hook_session_name}|#{hook_group}|#{hook_group_size}'"]);
    ctx.ok(&["kill-session", "-t", "a"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@removed"]).trim(), "a|a|2");
    assert_eq!(ctx.fmt("b", "#{session_grouped}|#{session_group}|#{session_group_size}"), "1|a|1");
    ctx.ok(&["set", "-g", "@v", "initial"]);
    ctx.ok(&["set-hook", "-gB", "@pane:%*:#{@v}", "set -g @mon \"#{hook_session}|#{hook_window}|#{hook_pane}|#{hook_value}\""]);
    // The monitor records its first value, then expands the hook before parsing it.
    std::thread::sleep(Duration::from_millis(2100));
    ctx.ok(&["set", "-g", "@v", "changed"]);
    let expected = ctx.fmt("b", "#{session_id}|#{window_id}|#{pane_id}|changed");
    wait_for("monitor session ID survives parsing", || ctx.cmd(&["show", "-gv", "@mon"]).1.trim() == expected);
}

#[test]
fn tmux_38_selection_after_hooks() {
    let ctx = Ctx::new("selectafter38");
    ctx.ok(&["new-session", "-d", "-s", "h", "sleep 100"]);
    ctx.ok(&["split-window", "-d", "-t", "h:0"]);
    ctx.ok(&["set-hook", "-g", "after-select-pane", "set -ga @sel P"]);
    ctx.ok(&["set-hook", "-g", "after-select-window", "set -ga @sel W"]);
    ctx.ok(&["set-hook", "-g", "pane-title-changed", "set -ga @title T"]);
    ctx.ok(&["select-pane", "-t", "h:0.0"]);
    ctx.ok(&["select-pane", "-t", "h:0.0", "-T", "same"]);
    ctx.ok(&["select-pane", "-t", "h:0.0", "-T", "same"]);
    ctx.ok(&["select-pane", "-t", "h:0.1"]);
    ctx.ok(&["last-pane", "-t", "h:0"]);
    ctx.ok(&["new-window", "-d", "-t", "h:1", "sleep 100"]);
    ctx.ok(&["next-window", "-t", "h"]);
    ctx.ok(&["previous-window", "-t", "h"]);
    ctx.ok(&["last-window", "-t", "h"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@sel"]).trim(), "PWWW");
    assert_eq!(ctx.ok(&["show", "-gv", "@title"]).trim(), "TT");
}

/// Hook commands inserted into a sourced file take none of its error allowance, and
/// after hooks need a session: config lines before the first session fire none.
#[test]
fn tmux_38_hooks_in_sourced_files() {
    let ctx = Ctx::new("hooksrc");
    let dir = std::env::temp_dir().join(format!("{}-files", ctx.socket));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("lines.conf");
    std::fs::write(&file, "set -g @a 1\nset -g @b 1\nset -g @c 1\nselectw -t :99\nset -g @z 1\n").unwrap();
    let startup = dir.join("startup.conf");
    std::fs::write(&startup, "set-hook -g after-set-option 'set -Fga @log \"aso:#{hook_argument_0} \"'\nset -g @a 1\nselectw -t :98\nselectw -t :97\nnew -d -s cfg\nset -g @b 1\n").unwrap();
    let file = file.to_str().unwrap().to_string();
    ctx.ok(&["new-session", "-d", "-s", "s"]);
    ctx.ok(&["set-hook", "-g", "after-set-option", "set -ga @log 'aso '"]);
    let _ = ctx.cmd(&["source-file", &file]);
    assert_eq!(ctx.ok(&["show", "-gv", "@z"]).trim(), "1");
    ctx.ok(&["set-hook", "-gu", "after-set-option"]);
    ctx.ok(&["set-hook", "-g", "command-error", "set -ga @log 'err '"]);
    std::fs::write(&file, "selectw -t :91\nselectw -t :92\nselectw -t :93\nselectw -t :94\nselectw -t :95\nset -g @x 1\nset -g @y 1\n").unwrap();
    let _ = ctx.cmd(&["source-file", &file]);
    assert_eq!(ctx.ok(&["show", "-gv", "@x"]).trim(), "1");
    assert_eq!(ctx.ok(&["show", "-gv", "@y"]).trim(), "1");
    ctx.ok(&["kill-server"]);
    let ctx = Ctx::new("hookstart");
    let out = ctx.command().args(["-f", startup.to_str().unwrap(), "new-session", "-d", "-s", "main"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let log = ctx.ok(&["show", "-gv", "@log"]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(log.trim(), "aso:@b");
}

/// set-hook with a command that does not parse still empties the hook; a second
/// wait-for -S uses up the remembered signal; replacing or renaming over a named
/// buffer frees the old one first.
#[test]
fn tmux_38_hook_arrays_channels_and_buffers() {
    let ctx = Ctx::new("hookarr");
    ctx.ok(&["new-session", "-d", "-s", "a"]);
    ctx.ok(&["set-hook", "-g", "window-renamed[0]", "display -p good"]);
    ctx.ok(&["set-hook", "-g", "window-renamed[5]", "display -p five"]);
    assert!(ctx.fails(&["set-hook", "-g", "window-renamed", "nosuchcmd"]).contains("unknown command: nosuchcmd"));
    assert_eq!(ctx.ok(&["show-hooks", "-g", "window-renamed"]).trim(), "window-renamed");
    ctx.ok(&["set-hook", "-g", "window-renamed[0]", "display -p good"]);
    assert!(ctx.fails(&["set", "-g", "window-renamed", "nosuchcmd"]).contains("unknown command"));
    assert_eq!(ctx.ok(&["show-hooks", "-g", "window-renamed"]).trim(), "window-renamed");
    // -a keeps what is there
    ctx.ok(&["set-hook", "-g", "window-renamed[0]", "display -p good"]);
    assert!(ctx.fails(&["set-hook", "-ga", "window-renamed", "nosuchcmd"]).contains("unknown command"));
    assert_eq!(ctx.ok(&["show-hooks", "-g", "window-renamed"]).trim(), "window-renamed[0] display-message -p good");

    ctx.ok(&["wait-for", "-S", "c1"]);
    ctx.ok(&["wait-for", "-S", "c1"]);
    let mut child = ctx.command().args(["wait-for", "c1"]).spawn().unwrap();
    wait_for("waiting after two signals", || !ctx.ok(&["wait-for", "-l", "c1"]).is_empty());
    ctx.ok(&["wait-for", "-S", "c1"]);
    wait_for("woken", || child.try_wait().unwrap().is_some());

    {
        use std::io::{BufRead, BufReader};
        use std::sync::{Arc, Mutex};
        let mut child = ctx.command().args(["-C", "attach", "-t", "a"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let out = Arc::new(Mutex::new(String::new()));
        let stdout = child.stdout.take().unwrap();
        let o = out.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line.starts_with("%paste-buffer") {
                    o.lock().unwrap().push_str(&(line + "\n"));
                }
            }
        });
        wait_for("attached", || ctx.ok(&["list-clients"]).contains("a"));
        ctx.ok(&["set-buffer", "-b", "b1", "one"]);
        ctx.ok(&["set-buffer", "-b", "b1", "two"]);
        ctx.ok(&["set-buffer", "-a", "-b", "b1", "three"]);
        ctx.ok(&["set-buffer", "-b", "b2", "other"]);
        ctx.ok(&["set-buffer", "-b", "b1", "-n", "b2"]);
        let want = "%paste-buffer-changed b1\n%paste-buffer-deleted b1\n%paste-buffer-changed b1\n%paste-buffer-deleted b1\n%paste-buffer-changed b1\n%paste-buffer-changed b2\n%paste-buffer-deleted b2\n%paste-buffer-deleted b1\n%paste-buffer-changed b2\n";
        wait_for("buffer notifications", || out.lock().unwrap().len() >= want.len());
        assert_eq!(out.lock().unwrap().as_str(), want);
        child.kill().unwrap();
        let _ = child.wait();
    }
    // without -b, -a adds a new automatic buffer
    ctx.ok(&["set-buffer", "auto1"]);
    ctx.ok(&["set-buffer", "-a", "auto2"]);
    assert_eq!(ctx.ok(&["list-buffers", "-F", "#{buffer_sample}"]), "auto2\nauto1\ntwothree\n");
    // pane-created reports a single empty argument as ''
    ctx.ok(&["set-hook", "-g", "pane-created", "set -gF @pc '#{hook_pane_command}|#{hook_created_empty}'"]);
    ctx.ok(&["split-window", "-d", "-t", "a", ""]);
    assert_eq!(ctx.ok(&["show", "-gv", "@pc"]).trim(), "''|1");
}

fn log_hooks(ctx: &Ctx, hooks: &[&str]) {
    for h in hooks {
        ctx.ok(&["set-hook", "-g", h, &format!("set -gaF @log '{h} '")]);
    }
}

fn take_log(ctx: &Ctx) -> String {
    std::thread::sleep(Duration::from_millis(150));
    let out = ctx.cmd(&["show", "-gqv", "@log"]).1.trim().to_string();
    ctx.ok(&["set", "-g", "@log", ""]);
    out
}

/// Events of commands that change layouts in steps fire in tmux's order: an unzoom
/// first, the closed or split cell, the new active pane, links before unlinks.
#[test]
fn tmux_38_layout_event_order() {
    let ctx = Ctx::new("layord");
    ctx.ok(&["new-session", "-d", "-s", "s", "-n", "w0", "-x", "80", "-y", "24"]);
    ctx.ok(&["split-window", "-d", "-t", "s:0"]);
    ctx.ok(&["split-window", "-d", "-t", "s:0"]);
    log_hooks(&ctx, &["window-zoomed", "window-unzoomed", "window-layout-changed", "pane-resized", "window-pane-changed"]);
    ctx.ok(&["resize-pane", "-Z", "-t", "s:0.0"]);
    assert_eq!(take_log(&ctx), "pane-resized window-zoomed window-layout-changed");
    ctx.ok(&["select-pane", "-t", "s:0.1"]);
    assert_eq!(take_log(&ctx), "pane-resized window-unzoomed window-layout-changed window-pane-changed");
    ctx.ok(&["resize-pane", "-Z", "-t", "s:0.1"]);
    take_log(&ctx);
    // -Z keeps the window zoomed (window_pop_zoom after window-pane-changed)
    ctx.ok(&["select-pane", "-Z", "-t", "s:0.2"]);
    assert_eq!(take_log(&ctx), "pane-resized window-unzoomed window-layout-changed window-pane-changed pane-resized window-zoomed window-layout-changed");
    assert_eq!(ctx.fmt("s:0", "#{window_zoomed_flag}"), "1");
    // the zoomed pane killed: unzoom, its cell closed, then the new active pane
    ctx.ok(&["kill-pane", "-t", "s:0.2"]);
    let log = take_log(&ctx);
    assert!(log.starts_with("pane-resized window-unzoomed window-layout-changed pane-resized") && log.ends_with("pane-resized window-layout-changed window-pane-changed"), "{log}");
    // split-window resizes the new pane from the window's size as well
    ctx.ok(&["split-window", "-d", "-t", "s:0.0"]);
    assert_eq!(take_log(&ctx), "pane-resized pane-resized window-layout-changed");
    ctx.ok(&["resize-pane", "-Z", "-t", "s:0.0"]);
    take_log(&ctx);
    ctx.ok(&["select-layout", "-t", "s:0", "even-vertical"]);
    assert!(take_log(&ctx).starts_with("pane-resized window-unzoomed window-layout-changed pane-resized"));
    // rotate-window keeps the active position: the pane moved there is active
    let first = ctx.fmt("s:0.0", "#{pane_id}");
    ctx.ok(&["select-pane", "-t", "s:0.0"]);
    take_log(&ctx);
    ctx.ok(&["rotate-window", "-t", "s:0"]);
    assert!(take_log(&ctx).ends_with("window-pane-changed"));
    assert_eq!(ctx.fmt("s:0", "#{pane_index}"), "0");
    assert_ne!(ctx.fmt("s:0", "#{pane_id}"), first);
    // select-pane does not select the pane's window
    ctx.ok(&["new-window", "-d", "-t", "s:1"]);
    ctx.ok(&["split-window", "-d", "-t", "s:1"]);
    ctx.ok(&["set-hook", "-g", "session-window-changed", "set -gaF @log 'swc '"]);
    take_log(&ctx);
    ctx.ok(&["select-pane", "-t", "s:1.1"]);
    assert_eq!(take_log(&ctx), "window-pane-changed");
    assert_eq!(ctx.fmt("s", "#{window_index}"), "0");
    // a window's last pane exiting closes its cell first
    log_hooks(&ctx, &["pane-exited", "window-unlinked", "window-closed"]);
    ctx.ok(&["new-window", "-d", "-t", "s:5", "sleep 0.3"]);
    wait_for("window gone", || !ctx.ok(&["list-windows", "-t", "s", "-F", "#{window_index}"]).lines().any(|l| l == "5"));
    assert_eq!(take_log(&ctx), "pane-exited window-layout-changed window-unlinked window-closed");
}

/// break-pane, join-pane, move-window and new-session -t fire their events in the
/// order of their tmux functions.
#[test]
fn tmux_38_window_event_order() {
    let ctx = Ctx::new("winord");
    ctx.ok(&["new-session", "-d", "-s", "s", "-n", "w0", "-x", "80", "-y", "24"]);
    ctx.ok(&["new-window", "-d", "-t", "s:1", "-n", "w1"]);
    ctx.ok(&["split-window", "-d", "-t", "s:w1"]);
    for h in ["window-layout-changed", "pane-resized", "window-pane-changed", "pane-moved", "window-linked", "window-unlinked", "window-created", "window-closed", "session-window-changed", "pane-created", "session-added-to-group", "session-created"] {
        ctx.ok(&["set-hook", "-g", h, &format!("set -gaF @log '{h}:#{{hook_window_name}} '")]);
    }
    ctx.ok(&["set-option", "-g", "automatic-rename", "off"]);
    ctx.ok(&["break-pane", "-d", "-s", "s:w1.1", "-n", "bp"]);
    assert_eq!(take_log(&ctx), "pane-resized:w1 window-layout-changed:w1 window-linked:bp pane-resized:bp window-created:bp pane-moved:bp");
    ctx.ok(&["join-pane", "-s", "s:bp", "-t", "s:w0", "-d"]);
    assert_eq!(take_log(&ctx), "window-layout-changed:bp pane-resized:w0 pane-resized:w0 pane-moved:w0 window-unlinked:bp window-closed:bp window-layout-changed:w0");
    ctx.ok(&["move-window", "-s", "s:w1", "-t", "s:7"]);
    // (server_link_window selects it without -d)
    assert_eq!(take_log(&ctx), "window-linked:w1 session-window-changed:w1 window-unlinked:w1");
    ctx.ok(&["select-window", "-t", "s:w0"]);
    take_log(&ctx);
    ctx.ok(&["move-window", "-s", "s:w0", "-t", "s:8"]);
    assert_eq!(take_log(&ctx), "window-linked:w0 session-window-changed:w0 window-unlinked:w0");
    // the first pane's window has no name yet when pane-created fires
    ctx.ok(&["new-window", "-d", "-t", "s:3", "-n", "named"]);
    assert_eq!(take_log(&ctx), "pane-created: window-created:named window-linked:named");
    // new-session -t creates its own first window, then replaces it with the group's
    ctx.ok(&["new-session", "-d", "-s", "g", "-t", "s"]);
    let log = take_log(&ctx);
    let n = log.split(' ').nth(1).and_then(|w| w.strip_prefix("window-created:")).unwrap_or("?").to_string();
    assert_eq!(log, format!("pane-created: window-created:{n} window-linked:{n} session-added-to-group: session-added-to-group: window-linked:named window-linked:w1 window-linked:w0 window-unlinked:{n} window-closed:{n} session-created:"));
    assert_eq!(ctx.ok(&["list-windows", "-t", "g", "-F", "#{window_name}"]), ctx.ok(&["list-windows", "-t", "s", "-F", "#{window_name}"]));
    assert_eq!(ctx.fmt("g", "#{window_index}"), "3");
}

/// wait-for -F filters see no client; a control client's wait-for block ends at once
/// and -v payloads follow as plain lines; monitors skip dead panes; panes killed by a
/// signal report exit_signal.
#[test]
fn tmux_38_waiters_monitors_and_exit_signals() {
    use std::io::{BufRead, BufReader};
    use std::sync::{Arc, Mutex};
    let ctx = Ctx::new("waitmon");
    ctx.ok(&["new-session", "-d", "-s", "w", "-n", "w0"]);
    ctx.ok(&["set", "-g", "automatic-rename", "off"]);
    let mut child = ctx.command().args(["wait-for", "-E", "-F", "#{client_name}", "window-renamed"]).spawn().unwrap();
    wait_for("waiter", || !ctx.ok(&["wait-for", "-E", "-l", "window-renamed"]).is_empty());
    ctx.ok(&["rename-window", "-t", "w:0", "n1"]);
    std::thread::sleep(Duration::from_millis(300));
    assert!(child.try_wait().unwrap().is_none(), "client_name is empty for the filter");
    child.kill().unwrap();
    let _ = child.wait();

    let mut child = ctx.command().args(["-C", "attach", "-t", "w"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let out = Arc::new(Mutex::new(String::new()));
    let stdout = child.stdout.take().unwrap();
    let o = out.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            o.lock().unwrap().push_str(&(line + "\n"));
        }
    });
    let mut stdin = child.stdin.take().unwrap();
    wait_for("attached", || out.lock().unwrap().contains("%session-changed"));
    stdin.write_all(b"wait-for -E -v @ctl\ndisplay -p after\n").unwrap();
    stdin.flush().unwrap();
    wait_for("block ended", || out.lock().unwrap().matches("%end").count() >= 2);
    assert!(!out.lock().unwrap().contains("after"));
    ctx.ok(&["set-hook", "-E", "-t", "w:0", "@ctl"]);
    wait_for("woken", || out.lock().unwrap().contains("after"));
    let text = out.lock().unwrap().clone();
    let end = text.match_indices("%end").nth(1).unwrap().0;
    let payload = text.find("event=@ctl\n").unwrap();
    let begin = text.rfind("%begin").unwrap();
    assert!(end < payload && payload < begin && text[begin..].contains("after"), "{text}");
    child.kill().unwrap();
    let _ = child.wait();

    // dead panes are not checked
    ctx.ok(&["set", "-g", "remain-on-exit", "on"]);
    ctx.ok(&["split-window", "-d", "-t", "w:0", "sleep 1.2"]);
    let pane = ctx.fmt("w:0.1", "#{pane_id}");
    ctx.ok(&["set-hook", "-B", &format!("@dead:{pane}:#{{pane_dead}}"), "set -g @deadfired 1"]);
    wait_for("dead", || ctx.fmt(&pane, "#{pane_dead}") == "1");
    std::thread::sleep(Duration::from_millis(2200));
    assert_eq!(ctx.ok(&["show-hooks", "-B", "-F", "#{hook_fire_count}"]).trim(), "0");
    #[cfg(unix)]
    {
        ctx.ok(&["set", "-g", "default-shell", "/bin/sh"]);
        ctx.ok(&["set-hook", "-g", "pane-died", "set -gF @died '#{hook_exit_status}|#{hook_exit_signal}'"]);
        ctx.ok(&["split-window", "-d", "-t", "w:0", "sleep 0.2; kill -TERM $$"]);
        wait_for("signal", || ctx.cmd(&["show", "-gqv", "@died"]).1.trim() == "|15");
        let dead = ctx.ok(&["list-panes", "-t", "w:0", "-F", "#{pane_dead_status}|#{pane_dead_signal}"]);
        assert!(dead.lines().any(|l| l == "|15"), "{dead}");
    }
}

/// Subscriptions are checked as tmux's monitor_timer does: the session ones, single
/// panes and windows, then every pane and every window.
#[test]
fn tmux_38_subscription_order() {
    use std::io::{BufRead, BufReader};
    use std::sync::{Arc, Mutex};
    let ctx = Ctx::new("suborder");
    ctx.ok(&["new-session", "-d", "-s", "s", "-n", "w0"]);
    let mut child = ctx.command().args(["-C", "attach", "-t", "s"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let out = Arc::new(Mutex::new(Vec::new()));
    let stdout = child.stdout.take().unwrap();
    let o = out.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(rest) = line.strip_prefix("%subscription-changed ") {
                o.lock().unwrap().push(rest.split(' ').next().unwrap_or("").to_string());
            }
        }
    });
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"refresh-client -B \"wn:@*:#{window_name}\"\nrefresh-client -B \"pn:%*:#{pane_index}\"\nrefresh-client -B \"sn::#{session_name}\"\n").unwrap();
    stdin.flush().unwrap();
    wait_for("subscriptions", || out.lock().unwrap().len() >= 3);
    assert_eq!(*out.lock().unwrap(), ["sn", "pn", "wn"]);
    child.kill().unwrap();
    let _ = child.wait();
}

/// Errors of a sourced file's lines go to stderr with exit status 1; respawn-window
/// selects the window; a control client's line with an unknown command is a parse
/// error.
#[test]
fn tmux_38_source_errors_respawn_and_control_parse() {
    use std::io::{BufRead, BufReader};
    use std::sync::{Arc, Mutex};
    let ctx = Ctx::new("srcerr");
    ctx.ok(&["new-session", "-d", "-s", "s"]);
    let file = std::env::temp_dir().join(format!("{}.conf", ctx.socket));
    std::fs::write(&file, "selectw -t :91\nset -g @x 1\n").unwrap();
    let out = ctx.command().args(["source-file", file.to_str().unwrap()]).output().unwrap();
    std::fs::remove_file(&file).unwrap();
    assert!(!out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert!(String::from_utf8_lossy(&out.stderr).contains("can't find window: 91"));
    assert_eq!(ctx.ok(&["show", "-gv", "@x"]).trim(), "1");

    ctx.ok(&["new-window", "-d", "-t", "s:3"]);
    ctx.ok(&["respawn-window", "-k", "-t", "s:3"]);
    assert_eq!(ctx.fmt("s", "#{window_index}"), "3");

    let mut child = ctx.command().args(["-C", "attach", "-t", "s"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let text = Arc::new(Mutex::new(String::new()));
    let stdout = child.stdout.take().unwrap();
    let t = text.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            t.lock().unwrap().push_str(&(line + "\n"));
        }
    });
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"set -g @ran 1 ; nosuchcmd\n").unwrap();
    stdin.flush().unwrap();
    wait_for("error", || text.lock().unwrap().contains("%error"));
    assert!(text.lock().unwrap().contains("\nparse error: unknown command: nosuchcmd\n%error"), "{}", text.lock().unwrap());
    // nothing on the line ran
    assert!(ctx.fails(&["show", "-gv", "@ran"]).contains("invalid option"));
    child.kill().unwrap();
    let _ = child.wait();
}

#[test]
fn floating_panes_from_the_cli() {
    let ctx = Ctx::new("float");
    ctx.ok(&["new-session", "-d", "-s", "f", "-x", "80", "-y", "24"]);
    let tiled = ctx.fmt("f", "#{pane_id}");
    // new-pane: half the width, a quarter of the height, cascaded from the top left
    let fp = ctx.ok(&["new-pane", "-d", "-P", "-F", "#{pane_id}", "-t", "f"]).trim().to_string();
    assert_eq!(ctx.fmt(&fp, "#{pane_floating_flag} #{pane_width}x#{pane_height} #{pane_x},#{pane_y} #{pane_z} #{pane_flags}"), "1 40x6 4,2 0 F");
    assert_eq!(ctx.fmt(&tiled, "#{pane_z} #{pane_active}"), "2 1");
    let layout = ctx.fmt("f", "#{window_layout}");
    assert!(layout.starts_with(r#"{"V":2,"L":{"t":"v","w":80,"h":24,"x":0,"y":0,"c":["#), "{layout}");
    assert!(layout.contains(&format!(r#""w":40,"h":6,"x":4,"y":2,"i":1,"z":0,"I":"{fp}""#)), "{layout}");
    // the layout restores sizes, positions and the z-order
    ctx.ok(&["move-pane", "-t", &fp, "-P", "centre"]);
    ctx.ok(&["select-layout", "-t", "f", &layout]);
    assert_eq!(ctx.fmt(&fp, "#{pane_x},#{pane_y}"), "4,2");
    // float the tiled pane and tile it back
    ctx.ok(&["split-window", "-d", "-h", "-t", &tiled]);
    ctx.ok(&["break-pane", "-W", "-s", &tiled, "-x", "30", "-y", "10"]);
    assert_eq!(ctx.fmt(&tiled, "#{pane_floating_flag} #{pane_width}x#{pane_height} #{pane_active}"), "1 28x8 1");
    ctx.ok(&["join-pane"]);
    assert_eq!(ctx.fmt(&tiled, "#{pane_floating_flag} #{pane_width}"), "0 40");
    // modal panes
    let m = ctx.ok(&["new-pane", "-O", "-P", "-F", "#{pane_id}", "-t", "f"]).trim().to_string();
    assert_eq!(ctx.fmt("f", "#{window_modal_pane} #{window_raw_flags}"), format!("{m} *O"));
    ctx.ok(&["select-pane", "-t", &tiled]);
    assert_eq!(ctx.fmt("f", "#{pane_id}"), m);
    assert!(ctx.fails(&["new-pane", "-O", "-t", "f"]).contains("window already has a modal pane"));
    ctx.ok(&["kill-pane", "-t", &m]);
    assert_eq!(ctx.fmt("f", "#{window_modal_pane}"), "");
    // new-pane -W returns the command's exit status
    #[cfg(unix)]
    {
        let out = ctx.command().args(["new-pane", "-W", "-t", "f", "exit 3"]).output().unwrap();
        assert_eq!(out.status.code(), Some(3));
        // it waits silently: no (empty) error line on stderr
        assert!(out.stderr.is_empty(), "{:?}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn spawn_without_c_uses_the_command_clients_directory() {
    // server_client_get_cwd: a command client without a session gives its cwd to
    // new-window, split-window, new-pane and run-shell; -c may be relative to it;
    // respawn-pane keeps the pane's directory
    let ctx = Ctx::new("spawncwd");
    let base = std::env::temp_dir().join(format!("pt-spawncwd-{}", std::process::id()));
    let (a, b) = (base.join("a"), base.join("b"));
    std::fs::create_dir_all(a.join("sub")).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let canon = |p: &std::path::Path| p.canonicalize().unwrap().to_string_lossy().into_owned();
    let run = |dir: &std::path::Path, args: &[&str]| {
        let out = ctx.command().current_dir(dir).args(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    run(&std::env::temp_dir(), &["new-session", "-d", "-s", "s", "-x", "80", "-y", "24"]);
    run(&a, &["new-window", "-d", "-t", "s:6"]);
    run(&b, &["split-window", "-d", "-t", "s:6"]);
    run(&a, &["new-pane", "-d", "-t", "s:6"]);
    run(&a, &["split-window", "-d", "-t", "s:6", "-c", "sub"]);
    run(&b, &["respawn-pane", "-k", "-t", "s:6.0"]);
    let want = format!("0 {}\n1 {}\n2 {}\n3 {}", canon(&a), canon(&a.join("sub")), canon(&b), canon(&a));
    wait_for("pane paths", || {
        let got = ctx.ok(&["list-panes", "-t", "s:6", "-F", "#{pane_index} #{pane_current_path}"]);
        got.trim_end() == want
    });
    #[cfg(unix)]
    assert_eq!(run(&b, &["run-shell", "pwd"]).trim_end(), canon(&b));
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn run_shell_t_shows_waited_output_in_view_mode() {
    // cmd_run_shell_print: with -t the output goes to the pane in view mode, also
    // without -b (the command-line client prints nothing but keeps the exit status)
    let ctx = Ctx::new("runshellt");
    ctx.ok(&["new-session", "-d", "-s", "s", "-x", "80", "-y", "24"]);
    assert_eq!(ctx.ok(&["run-shell", "-t", "s:0", "echo hello"]), "");
    assert_eq!(ctx.fmt("s:0", "#{pane_mode}"), "view-mode");
    let mode = || ctx.ok(&["capture-pane", "-p", "-M", "-t", "s:0"]);
    assert!(mode().contains("hello"));
    let (ok, text) = ctx.cmd(&["run-shell", "-t", "s:0", "echo two; exit 3"]);
    assert!(!ok && text.is_empty(), "{text}");
    // window_copy_add: added after the output already in view
    wait_for("failure shown", || mode().contains("hello\ntwo\n'echo two; exit 3' returned 3"));
    assert_eq!(ctx.ok(&["run-shell", "echo cli"]).trim_end(), "cli");
}

#[test]
fn startup_config_causes_go_to_the_first_attach() {
    // cmdq_error without a client while the configuration loads is a config cause
    // (display-message too, even with -p); a control client gets %config-error after
    // %session-changed, once
    use std::io::{BufRead, BufReader, Write};
    use std::sync::{Arc, Mutex};
    let ctx = Ctx::new("cfgcause");
    let conf = std::env::temp_dir().join(format!("{}.conf", ctx.socket));
    std::fs::write(&conf, "display \"from config\"\nset -g @cfg 1\nset -g nosuchopt 1\ndisplay -p printed\n").unwrap();
    let conf_s = conf.to_string_lossy().into_owned();
    ctx.ok(&["-f", &conf_s, "new-session", "-d", "-s", "s"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@cfg"]).trim_end(), "1");
    // nobody is attached yet: the causes wait
    assert_eq!(ctx.fmt("s", "#{pane_mode}"), "");
    let attach = || {
        let mut child = ctx.command().args(["-C", "attach", "-t", "s"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
        let out = Arc::new(Mutex::new(String::new()));
        let o = out.clone();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                o.lock().unwrap().push_str(&(line + "\n"));
            }
        });
        wait_for("attached", || out.lock().unwrap().contains("%session-changed $"));
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"display -p done\n").unwrap();
        stdin.flush().unwrap();
        wait_for("reply", || out.lock().unwrap().contains("\ndone\n"));
        drop(stdin);
        let _ = child.wait();
        let text = out.lock().unwrap().clone();
        text.lines().filter(|l| l.starts_with("%config-error") || l.starts_with("%session-changed")).map(String::from).collect::<Vec<_>>()
    };
    let f = conf_s.as_str();
    assert_eq!(attach(), vec![
        "%session-changed $0 s".to_string(),
        format!("%config-error {f}:1: from config"),
        format!("%config-error {f}:3: invalid option: nosuchopt"),
        format!("%config-error {f}:4: printed"),
    ]);
    assert_eq!(attach(), vec!["%session-changed $0 s".to_string()]);
    let _ = std::fs::remove_file(&conf);
}

#[cfg(unix)]
#[test]
fn respawn_keeps_the_pane_history() {
    // screen_reinit clears only the visible lines: the history stays
    let ctx = Ctx::new("respawnhist");
    ctx.ok(&["new-session", "-d", "-s", "s", "-x", "40", "-y", "6", "seq 20; sleep 100"]);
    wait_for("history", || ctx.fmt("s", "#{history_size}") == "15");
    for cmd in ["respawn-pane", "respawn-window"] {
        ctx.ok(&[cmd, "-k", "-t", "s", "echo x; sleep 100"]);
        wait_for("new output", || ctx.capture("s").starts_with("x\n"));
        assert_eq!(ctx.fmt("s", "#{history_size}"), "15", "{cmd}");
        assert!(ctx.ok(&["capture-pane", "-p", "-S", "-", "-t", "s"]).starts_with("1\n2\n3\n"), "{cmd}");
    }
}
