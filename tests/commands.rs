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
    ctx.ok(&["send-keys", "-t", "c", "q"]);
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
    // Restoring a layout string assigns panes in list order, like tmux.
    assert_eq!(ctx.fmt("s:0.2", "#{pane_left} #{pane_width}"), "41 80");
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
    ctx.ok(&["find-window", "-t", "a", "-N", "zero"]);
    assert_eq!(ctx.fmt("a", "#{window_name}"), "zero");
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
        "# comment\nset -g @conf loaded\nbind -n F5 { set -g @f5 pressed ; display done }\nset -g status-right '#[fg=yellow]%H:%M'\n%if #{==:#{host},#{host}}\nset -g @cond yes\n%endif\n",
    )
    .unwrap();
    ctx.ok(&["source-file", &conf.to_string_lossy()]);
    let _ = std::fs::remove_file(&conf);
    assert_eq!(ctx.fmt("k", "#{@conf} #{@cond}"), "loaded yes");
    assert!(ctx.ok(&["list-keys", "-T", "root", "F5"]).contains("set-option -g @f5 pressed"));
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
    std::fs::write(&conf, "if-shell 'exit 0' 'set -g @rv yes' 'set -g @rv no'\nset -g @after 1\nnot-a-command\nset -g @tail ok\n").unwrap();
    let (_, out) = ctx.cmd(&["source-file", &conf.to_string_lossy()]);
    let _ = std::fs::remove_file(&conf);
    assert!(out.contains("unknown command"), "{out}");
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
    assert_eq!(ctx.fmt("g2", "#{session_grouped}"), "0");
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
    send("kill-window");
    wait_for("%window-close", || text().contains("%window-close @"));
    send("no-such-command");
    wait_for("%error", || text().contains("%error "));
    send("refresh-client -C 120x40");
    wait_for("control size", || ctx.fmt("c", "#{window_width}x#{window_height}") == "120x39");
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
    // About 1MB per reply; tmux limits each padding to 10000 columns.
    let oversized = vec![format!("display-message -p '{}'", "#{p10000:#{l:x}}".repeat(100)); 16].join(" ; ");
    writeln!(stdin, "{oversized}").unwrap();
    stdin.flush().unwrap();
    wait_for("combined reply rejected", || text().contains("command output limit exceeded"));
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
    assert_eq!(ctx.ok(&["show-hooks", "-g", "client-attached"]), "");
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
    assert!(ctx.fails(&["display-message", "-p", "-t", "p", "-c", "nope", "x"]).contains("can't find client"));
    assert!(ctx.fails(&["select-window", "-t", "{current}"]).contains("no current client"));
    assert!(ctx.fails(&["kill-pane", "-t", "@"]).contains("no current client"));
    assert_eq!(ctx.ok(&["list-commands", "-F", "#{command_list_name}|#{command_list_alias}", "list-keys"]), "list-keys|lsk\n");
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
    ctx.ok(&["bind", "z", "if", "-F", "1", "{", "display", "a", "}", "{", "display", "b", "}"]);
    ctx.ok(&["bind", "w", "send-keys", "-X", "copy-pipe", "cat >/dev/null"]);
    let keys = |k: &str| ctx.ok(&["list-keys", "-T", "prefix", k]).trim_end().to_string();
    assert_eq!(keys("x"), "bind-key  -T prefix x display-message \"hi;there\"");
    assert_eq!(keys("y"), "bind-key  -T prefix y display-message a \\; display-message b");
    assert_eq!(keys("z"), "bind-key  -T prefix z if-shell -F 1 { display-message a } { display-message b }");
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
    assert!(jobs.starts_with("Job 0: sleep 5 [fd=-1, pid="), "{jobs}");
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
