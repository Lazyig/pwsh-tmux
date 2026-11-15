//! Attached-client behaviour: keys, prompts, modes, overlays and the mouse.

mod common;
use common::*;
use std::time::Duration;

fn start(ctx: &Ctx, name: &str) -> Term {
    let t = Term::attach(ctx, &["new-session", "-s", name], 100, 30);
    t.wait_screen("status line", |s| s.contains(&format!("[{name}]")));
    ctx.wait_prompt(name);
    t
}

#[test]
fn readonly_terminal_blocks_keys_paste_mouse_and_shared_resize() {
    let ctx = Ctx::new("readonly-input");
    ctx.ok(&["new-session", "-d", "-s", "ro", "-x", "100", "-y", "30"]);
    ctx.wait_prompt("ro");
    ctx.ok(&["split-window", "-h", "-t", "ro"]);
    ctx.ok(&["set-option", "-g", "mouse", "on"]);
    let before = ctx.fmt("ro", "#{pane_id}:#{window_width}x#{window_height}:#{session_windows}");
    let mut t = Term::attach(&ctx, &["attach-session", "-r", "-t", "ro"], 60, 20);
    t.wait_screen("readonly attach", |s| s.contains("[ro]"));
    t.send(b"echo READONLY_TYPED\r");
    t.send(b"\x1b[200~echo READONLY_PASTED\r\x1b[201~");
    t.send(b"\x1b[777~");
    t.prefix(b"c");
    t.prefix(b"%");
    t.mouse(0, 1, 1, false);
    t.mouse(0, 1, 1, true);
    // A writable input/output barrier ensures these checks run after the input.
    // -J: a long shell prompt wraps the command line in the half-width pane.
    ctx.ok(&["send-keys", "-t", "ro", "echo WRITABLE_BARRIER", "Enter"]);
    wait_for("writable input barrier", || ctx.ok(&["capture-pane", "-p", "-J", "-t", "ro"]).matches("WRITABLE_BARRIER").count() >= 2);
    let capture = ctx.ok(&["capture-pane", "-p", "-t", "ro", "-S", "-"]);
    assert!(!capture.contains("READONLY_TYPED") && !capture.contains("READONLY_PASTED"), "{capture}");
    assert_eq!(ctx.fmt("ro", "#{pane_id}:#{window_width}x#{window_height}:#{session_windows}"), before);
    assert_eq!(ctx.fmt("ro", "#{window_panes}"), "2");
    t.prefix(b"d");
    t.wait_exit();
}

#[test]
fn prompt_rename_and_status() {
    let ctx = Ctx::new("prompt");
    let t = start(&ctx, "p");
    // C-b : runs a command
    t.prefix(b":");
    wait_for("prompt", || t.row(29).starts_with(':'));
    t.send(b"rename-window from-prompt\r");
    wait_for("rename via prompt", || ctx.fmt("p", "#W") == "from-prompt");
    t.wait_screen("status shows name", |s| s.contains("0:from-prompt*"));
    // C-b , prompts with the current name pre-filled; C-u clears it
    t.prefix(b",");
    t.send(b"\x15renamed\r");
    wait_for("rename via ,", || ctx.fmt("p", "#W") == "renamed");
    // C-b $ renames the session
    t.prefix(b"$");
    t.send(b"\x15sess2\r");
    wait_for("session rename", || ctx.ok(&["ls", "-F", "#S"]).trim() == "sess2");
    // Escape cancels a prompt
    t.prefix(b":");
    t.send(b"kill-server");
    t.send(b"\x1b");
    std::thread::sleep(Duration::from_millis(200));
    ctx.ok(&["has-session", "-t", "sess2"]);
    // confirm-before: n keeps, y kills
    ctx.ok(&["split-window", "-t", "sess2"]);
    t.prefix(b"x");
    t.wait_screen("confirm", |s| s.contains("kill-pane 1? (y/n)"));
    t.send(b"n");
    assert_eq!(ctx.fmt("sess2", "#{window_panes}"), "2");
    t.prefix(b"x");
    t.send(b"y");
    wait_for("pane killed", || ctx.fmt("sess2", "#{window_panes}") == "1");
    // display-message shows on the status line
    ctx.ok(&["set", "-g", "display-time", "5000"]);
    let client = ctx.ok(&["list-clients", "-F", "#{client_name}"]).trim().to_string();
    ctx.ok(&["display-message", "-c", &client, "hello-msg"]);
    t.wait_screen("message", |s| s.contains("hello-msg"));
}

#[test]
fn incremental_copy_search_updates_before_enter_and_can_cancel() {
    let ctx = Ctx::new("isearch");
    let t = start(&ctx, "s");
    t.send(b"echo incremental-needle-321\r");
    wait_for("search fixture", || ctx.capture("s").matches("incremental-needle-321").count() >= 2);
    t.prefix(b"[");
    wait_for("copy mode", || ctx.fmt("s", "#{pane_in_mode}") == "1");
    let origin = ctx.fmt("s", "#{copy_cursor_x},#{copy_cursor_y}");
    t.send(b"\x12");
    t.send(b"incremental-needle");
    wait_for("preview before enter", || ctx.fmt("s", "#{copy_cursor_x},#{copy_cursor_y}") != origin);
    let preview = ctx.fmt("s", "#{copy_cursor_x},#{copy_cursor_y}");
    t.send(b"\x07");
    wait_for("cancel keeps preview cursor", || ctx.fmt("s", "#{copy_cursor_x},#{copy_cursor_y}") == preview);
    ctx.ok(&["send-keys", "-X", "-t", "s", "history-bottom"]);
    let bottom = ctx.fmt("s", "#{copy_cursor_x},#{copy_cursor_y}");
    t.send(b"\x12");
    t.send(b"incremental-needle");
    wait_for("preview again", || ctx.fmt("s", "#{copy_cursor_x},#{copy_cursor_y}") != bottom);
    let preview = ctx.fmt("s", "#{copy_cursor_x},#{copy_cursor_y}");
    t.send(b"\r");
    assert_eq!(ctx.fmt("s", "#{copy_cursor_x},#{copy_cursor_y}"), preview);
    t.send(b"q");
    wait_for("leave copy mode", || ctx.fmt("s", "#{pane_in_mode}") == "0");
    t.prefix(b"d");
}

#[test]
fn copy_mode_keys_and_paste() {
    let ctx = Ctx::new("copykeys");
    let t = start(&ctx, "c");
    ctx.ok(&["set", "-g", "mode-keys", "vi"]);
    t.send(b"echo alpha-beta-gamma\r");
    wait_for("output", || ctx.capture("c").matches("alpha-beta-gamma").count() >= 2);
    t.prefix(b"[");
    wait_for("copy mode", || ctx.fmt("c", "#{pane_in_mode}") == "1");
    t.wait_screen("indicator", |s| s.contains("[0/"));
    t.send(b"?alpha-beta\r");
    t.send(b" ");
    t.send(b"e");
    t.send(b"\r");
    wait_for("left copy mode", || ctx.fmt("c", "#{pane_in_mode}") == "0");
    assert_eq!(ctx.ok(&["show-buffer"]), "alpha");
    // OSC 52 was sent to the outer terminal
    assert!(t.raw_text().contains("\x1b]52;c;YWxwaGE="));
    // C-b ] pastes it
    t.send(b"\x15echo ");
    t.prefix(b"]");
    t.send(b"-pasted\r");
    wait_for("pasted", || ctx.capture("c").contains("alpha-pasted"));
    // PageUp binding enters copy mode scrolled
    t.prefix(b"\x1b[5~");
    wait_for("copy mode again", || ctx.fmt("c", "#{pane_in_mode}") == "1");
    t.send(b"q");
    wait_for("quit", || ctx.fmt("c", "#{pane_in_mode}") == "0");
}

#[test]
fn tree_panes_and_clock() {
    let ctx = Ctx::new("tree");
    let t = start(&ctx, "one");
    ctx.ok(&["new-session", "-d", "-s", "two"]);
    // choose-tree -s, pick the other session
    t.prefix(b"s");
    t.wait_screen("tree", |s| s.contains("one: 1 windows") && s.contains("two: 1 windows"));
    t.send(b"\x1b[B");
    t.send(b"\r");
    wait_for("switched", || ctx.ok(&["list-clients", "-F", "#{client_session}"]).trim() == "two");
    t.wait_screen("status two", |s| s.contains("[two]"));
    // switch back with C-b L (last session)
    t.prefix(b"L");
    wait_for("switched back", || ctx.ok(&["list-clients", "-F", "#{client_session}"]).trim() == "one");
    // display-panes then a digit selects that pane
    ctx.ok(&["split-window", "-h", "-t", "one"]);
    ctx.ok(&["set", "-g", "display-panes-time", "5000"]);
    assert_eq!(ctx.fmt("one", "#P"), "1");
    t.prefix(b"q");
    std::thread::sleep(Duration::from_millis(200));
    t.send(b"0");
    wait_for("display-panes select", || ctx.fmt("one", "#P") == "0");
    // clock mode
    t.prefix(b"t");
    wait_for("clock", || ctx.fmt("one", "#{pane_mode}") == "clock-mode");
    t.send(b"q");
    wait_for("clock off", || ctx.fmt("one", "#{pane_in_mode}") == "0");
    // zoom shows Z in the status flags
    t.prefix(b"z");
    t.wait_screen("zoom flag", |s| s.contains("*Z"));
    t.prefix(b"z");
    // synchronize-panes: typing goes to both panes
    ctx.ok(&["setw", "-t", "one", "synchronize-panes", "on"]);
    ctx.wait_prompt("one.1");
    t.send(b"echo sync-123\r");
    wait_for("sync both", || ctx.capture("one.0").contains("sync-123") && ctx.capture("one.1").contains("sync-123"));
}

#[test]
fn mouse_menu_and_popup() {
    let ctx = Ctx::new("mouse");
    let t = start(&ctx, "m");
    ctx.ok(&["set", "-g", "mouse", "on"]);
    t.wait_screen("mouse mode on", |_| t.raw_text().contains("\x1b[?1006h"));
    ctx.ok(&["split-window", "-h", "-t", "m"]);
    assert_eq!(ctx.fmt("m", "#P"), "1");
    // click in the left pane selects it
    t.mouse(0, 5, 5, false);
    t.mouse(0, 5, 5, true);
    wait_for("click select", || ctx.fmt("m", "#P") == "0");
    // drag the border to resize
    let w0: i32 = ctx.fmt("m.0", "#{pane_width}").parse().unwrap();
    t.mouse(0, w0 as u16, 10, false);
    t.mouse(32, w0 as u16 + 6, 10, false);
    t.mouse(0, w0 as u16 + 6, 10, true);
    wait_for("border drag", || ctx.fmt("m.0", "#{pane_width}").parse::<i32>().unwrap() == w0 + 6);
    // wheel up enters copy mode, with -e
    t.mouse(64, 5, 5, false);
    wait_for("wheel copy mode", || ctx.fmt("m.0", "#{pane_in_mode}") == "1");
    t.send(b"q");
    // click on the status line selects a window
    ctx.ok(&["new-window", "-t", "m"]);
    assert_eq!(ctx.fmt("m", "#I"), "1");
    t.wait_screen("two windows", |s| s.contains("1:"));
    let status = t.row(29);
    let col = status.find("0:").unwrap() as u16;
    t.mouse(0, col, 29, false);
    t.mouse(0, col, 29, true);
    wait_for("status click", || ctx.fmt("m", "#I") == "0");
    // pane menu via C-b >, Escape closes it
    t.prefix(b">");
    t.wait_screen("menu", |s| s.contains("Horizontal Split"));
    t.send(b"\x1b");
    t.wait_screen("menu closed", |s| !s.contains("Horizontal Split"));
    // menu item key runs its command
    t.prefix(b">");
    t.wait_screen("menu", |s| s.contains("Horizontal Split"));
    t.send(b"z");
    wait_for("zoom from menu", || ctx.fmt("m", "#{window_zoomed_flag}") == "1");
    ctx.ok(&["resize-pane", "-Z", "-t", "m"]);
    // popup runs a command and closes when it exits
    let client = ctx.ok(&["list-clients", "-F", "#{client_name}"]).trim().to_string();
    let mut popup = ctx.command().args(["display-popup", "-c", &client, "-w", "40", "-h", "10", "-T", "pop", "-E", "echo in-popup; sleep 2"]).spawn().unwrap();
    t.wait_screen("popup", |s| s.contains("in-popup") && s.contains("pop"));
    assert!(popup.try_wait().unwrap().is_none(), "popup CLI waits for close");
    t.wait_screen("popup closed", |s| !s.contains("in-popup"));
    assert!(popup.wait().unwrap().success());
}

#[test]
fn detach_attach_and_nesting() {
    let ctx = Ctx::new("detach");
    let mut t = start(&ctx, "d");
    t.prefix(b"d");
    t.wait_exit();
    assert!(t.raw_text().contains("[detached (from session d)]"));
    assert!(!ctx.ok(&["ls"]).contains("attached"));
    // attach again, then a second client with -d detaches the first
    let mut a = Term::attach(&ctx, &["attach", "-t", "d"], 90, 25);
    a.wait_screen("attached", |s| s.contains("[d]"));
    assert!(ctx.ok(&["ls"]).contains("(attached)"));
    let b = Term::attach(&ctx, &["attach", "-d", "-t", "d"], 80, 24);
    b.wait_screen("second attached", |s| s.contains("[d]"));
    a.wait_exit();
    assert_eq!(ctx.ok(&["list-clients"]).lines().count(), 1);
    // attaching from inside a pane is refused
    let inner = ctx.command().args(["attach", "-t", "d"]).env("PWSH_TMUX", "x").env("PWSH_TMUX_PANE", "%0").output().unwrap();
    assert!(!inner.status.success());
    assert!(String::from_utf8_lossy(&inner.stderr).contains("nested"));
    // kill-server ends the client
    drop(b);
    let c = Term::attach(&ctx, &["attach"], 80, 24);
    c.wait_screen("attached", |s| s.contains("[d]"));
    ctx.ok(&["kill-server"]);
    c.wait_screen("server exited", |_| c.raw_text().contains("[server exited]") || c.raw_text().contains("[exited]"));
}

#[test]
fn terminal_closed_under_client() {
    let ctx = Ctx::new("hangup");
    let mut t = start(&ctx, "h");
    t.hangup();
    // the client exits (no busy loop on a dead console) and the session stays, detached
    t.wait_exit();
    wait_for("detached", || !ctx.ok(&["ls"]).contains("attached"));
    ctx.ok(&["has-session", "-t", "h"]);
}

#[test]
fn terminal_features_and_extended_keys() {
    let ctx = Ctx::new("features");
    ctx.ok(&["new-session", "-d", "-s", "f"]);
    ctx.ok(&["set", "-g", "status-style", "bg=#ff0000,fg=#00ff00"]);
    // xterm-256color without COLORTERM: truecolor is mapped to the 256-colour palette
    let t = Term::attach(&ctx, &["attach", "-t", "f"], 80, 24);
    t.wait_screen("status", |s| s.contains("[f]"));
    let feats = ctx.ok(&["list-clients", "-F", "#{client_termfeatures}"]);
    assert!(feats.contains("256") && feats.contains("clipboard") && !feats.contains("RGB"), "{feats}");
    wait_for("256-colour status", || t.raw_text().contains("48;5;196"));
    assert!(!t.raw_text().contains("48;2;255;0;0"));
    drop(t);
    // -T adds features: RGB keeps truecolor, sync wraps frames, extkeys lets extended-keys ask
    ctx.ok(&["set", "-s", "extended-keys", "always"]);
    let t = Term::attach(&ctx, &["-T", "RGB,sync,extkeys", "attach", "-t", "f"], 80, 24);
    t.wait_screen("status", |s| s.contains("[f]"));
    wait_for("truecolor status", || t.raw_text().contains("48;2;255;0;0"));
    assert!(t.raw_text().contains("\x1b[?2026h"), "synchronized output");
    assert!(t.raw_text().contains("\x1b[>4;2m"), "modifyOtherKeys requested");
}

#[test]
fn style_options_are_expanded_as_formats() {
    // 3.8: a style option with #{ is expanded for the client before it is parsed, so the
    // comma inside the conditional does not split the style.
    let ctx = Ctx::new("styleformat");
    ctx.ok(&["new-session", "-d", "-s", "sf"]);
    ctx.ok(&["set", "-g", "status-style", "bg=#{?#{==:#{session_name},sf},colour196,colour21}"]);
    ctx.ok(&["set", "-g", "window-status-current-style", "fg=#{@wfg}"]);
    ctx.ok(&["set", "-w", "-t", "sf:0", "@wfg", "colour46"]);
    let t = Term::attach(&ctx, &["attach", "-t", "sf"], 80, 24);
    t.wait_screen("status", |s| s.contains("[sf]"));
    wait_for("status-style from a format", || t.raw_text().contains("48;5;196"));
    wait_for("window style from a window option", || t.raw_text().contains("38;5;46"));
    assert!(!t.raw_text().contains("48;5;21m"));
}

#[test]
fn progress_bar_preserves_dark_truecolor_background_on_blank_cells() {
    let ctx = Ctx::new("progress-colors");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ui_fixture.py");
    let python = if cfg!(windows) { "python" } else { "python3" };
    ctx.ok(&["new-session", "-d", "-s", "colors", python,
        fixture.to_str().unwrap(), "--ascii", "--progress-colors"]);
    let mut cb = portable_pty::CommandBuilder::new(binary());
    cb.args(["attach-session", "-t", "colors"]);
    cb.env("PWSH_TMUX_SOCKET", &ctx.socket);
    cb.env_remove("PWSH_TMUX");
    cb.env_remove("PWSH_TMUX_PANE");
    cb.env_remove("COLORTERM");
    cb.env("TERM", if cfg!(windows) { "dumb" } else { "xterm-direct" });
    let t = Term::spawn(cb, 100, 30);
    t.wait_screen("usage progress bar", |s| s.contains("2% used"));
    let screen = t.screen.lock().unwrap();
    let bar = screen.screen().cell(8, 10).unwrap();
    // a blank (written, or erased with the background colour by ConPTY)
    assert!(matches!(bar.contents(), "" | " "), "{:?}", bar.contents());
    assert_eq!(bar.bgcolor(), vt100::Color::Rgb(82, 82, 112));
    let filled = screen.screen().cell(8, 0).unwrap();
    assert_eq!(filled.fgcolor(), vt100::Color::Rgb(180, 180, 255));
    assert_eq!(filled.bgcolor(), vt100::Color::Rgb(82, 82, 112));
    assert!(ctx.ok(&["list-clients", "-F", "#{client_termfeatures}"]).contains("RGB"));
}

#[test]
fn current_target_sorted_switch_and_prompt_flags() {
    let ctx = Ctx::new("cur-target");
    ctx.ok(&["new-session", "-d", "-s", "c"]);
    ctx.ok(&["new-session", "-d", "-s", "b"]);
    let t = start(&ctx, "a");
    // {current} / @ name the attached client's window
    ctx.ok(&["bind-key", "-n", "F5", "set-option", "-t", "{current}", "@cur", "yes"]);
    t.send(b"\x1b[15~");
    wait_for("{current}", || ctx.ok(&["show-options", "-qv", "-t", "a", "@cur"]).trim() == "yes");
    ctx.ok(&["bind-key", "-n", "F6", "rename-window", "-t", "@", "via-at"]);
    t.send(b"\x1b[17~");
    wait_for("@ target", || ctx.fmt("a", "#W") == "via-at");
    // switch-client -n by name (the default) and by -O index (creation order: c, b, a)
    ctx.ok(&["bind-key", "-n", "F7", "switch-client", "-n"]);
    ctx.ok(&["bind-key", "-n", "F8", "switch-client", "-n", "-O", "index"]);
    let session = || ctx.ok(&["list-clients", "-F", "#{session_name}"]).trim().to_string();
    t.send(b"\x1b[18~");
    wait_for("next by name", || session() == "b");
    t.send(b"\x1b[19~");
    wait_for("next by index", || session() == "a");
    // command-prompt -l: one prompt and input, commas included
    ctx.ok(&["bind-key", "-n", "F9", "command-prompt", "-l", "-p", "one,two", "-I", "x,y", "set -g @got '%1'"]);
    t.send(b"\x1b[20~");
    wait_for("literal prompt", || t.row(29).starts_with("one,two"));
    t.send(b"\r");
    wait_for("literal input", || ctx.ok(&["show-options", "-gqv", "@got"]).trim() == "x,y");
    // backspace on an empty prompt closes it only with -e
    ctx.ok(&["bind-key", "-n", "F10", "command-prompt", "-p", "plain", "set -g @plain '%1'"]);
    t.send(b"\x1b[21~");
    wait_for("plain prompt", || t.row(29).starts_with("plain"));
    t.send(b"\x7f");
    std::thread::sleep(Duration::from_millis(300));
    assert!(t.row(29).starts_with("plain"), "{}", t.row(29));
    t.send(b"\x1b");
    ctx.ok(&["bind-key", "-n", "F11", "command-prompt", "-e", "-p", "exits", "set -g @exits '%1'"]);
    t.send(b"\x1b[23~");
    wait_for("-e prompt", || t.row(29).starts_with("exits"));
    t.send(b"\x7f");
    wait_for("-e closes", || !t.row(29).starts_with("exits"));
    // confirm-before -c sets the key, -y makes Enter confirm; the prompt names the command
    ctx.ok(&["bind-key", "-n", "F12", "confirm-before", "-y", "-c", "x", "set -g @conf yes"]);
    t.send(b"\x1b[24~");
    t.wait_screen("confirm prompt", |s| s.contains("Confirm 'set-option'? (x/n)"));
    t.send(b"\r");
    wait_for("-y confirms", || ctx.ok(&["show-options", "-gqv", "@conf"]).trim() == "yes");
    ctx.ok(&["bind-key", "-n", "F12", "confirm-before", "-c", "x", "set -g @conf2 yes"]);
    t.send(b"\x1b[24~");
    t.wait_screen("confirm prompt", |s| s.contains("(x/n)"));
    t.send(b"y");
    std::thread::sleep(Duration::from_millis(300));
    assert!(ctx.ok(&["show-options", "-gqv", "@conf2"]).trim().is_empty());
}

#[test]
fn attach_flags_environment_and_destroy() {
    let ctx = Ctx::new("attach-flags");
    ctx.ok(&["new-session", "-d", "-s", "a1"]);
    ctx.ok(&["new-session", "-d", "-s", "a2"]);
    ctx.ok(&["set", "-g", "update-environment", "P1FIX_*"]);
    let mut cb = portable_pty::CommandBuilder::new(common::binary());
    cb.args(["attach-session", "-E", "-f", "ignore-size,no-detach-on-destroy,bogus", "-t", "a1"]);
    cb.env("PWSH_TMUX_SOCKET", &ctx.socket);
    cb.env_remove("PWSH_TMUX");
    cb.env_remove("PWSH_TMUX_PANE");
    cb.env("TERM", "xterm-256color");
    cb.env("P1FIX_VAR", "yes");
    let t = Term::spawn(cb, 80, 24);
    t.wait_screen("attach", |s| s.contains("[a1]"));
    // server_client_get_flags; unknown flags are ignored
    assert_eq!(ctx.ok(&["list-clients", "-F", "#{client_flags}|#{session_name}"]).trim_end(), "attached,focused,ignore-size,no-detach-on-destroy,UTF-8|a1");
    // -E: no update-environment; switch-client without -E applies it (patterns match)
    assert!(ctx.fails(&["show-environment", "-t", "a1", "P1FIX_VAR"]).contains("unknown variable: P1FIX_VAR"));
    let client = ctx.ok(&["list-clients", "-F", "#{client_name}"]).trim().to_string();
    ctx.ok(&["switch-client", "-c", &client, "-t", "a2"]);
    assert_eq!(ctx.ok(&["show-environment", "-t", "a2", "P1FIX_VAR"]), "P1FIX_VAR=yes\n");
    // detach-on-destroy is on, but this client moves to another session
    ctx.ok(&["kill-session", "-t", "a2"]);
    wait_for("moved to a1", || ctx.ok(&["list-clients", "-F", "#{session_name}"]).trim() == "a1");
    ctx.ok(&["refresh-client", "-t", &client, "-f", "!ignore-size,!no-detach-on-destroy"]);
    assert_eq!(ctx.ok(&["list-clients", "-F", "#{client_flags}"]).trim_end(), "attached,focused,UTF-8");
    // switch-client -r toggles read-only and ignore-size; the server's user may turn it off
    ctx.ok(&["switch-client", "-c", &client, "-r"]);
    assert_eq!(ctx.ok(&["list-clients", "-F", "#{client_flags}"]).trim_end(), "attached,focused,ignore-size,read-only,UTF-8");
    ctx.ok(&["switch-client", "-c", &client, "-r"]);
    assert_eq!(ctx.ok(&["list-clients", "-F", "#{client_flags}"]).trim_end(), "attached,focused,UTF-8");
}

#[test]
fn prompt_types_keep_separate_histories() {
    let ctx = Ctx::new("prompt-types");
    let t = start(&ctx, "h");
    let client = ctx.ok(&["list-clients", "-F", "#{client_name}"]).trim().to_string();
    ctx.ok(&["command-prompt", "-t", &client, "-T", "search", "set -g @searched '%%'"]);
    t.send(b"needle\r");
    wait_for("search prompt ran", || ctx.fmt("h", "#{@searched}") == "needle");
    ctx.ok(&["command-prompt", "-t", &client, "set -g @typed '%%'"]);
    t.send(b"typed\r");
    wait_for("command prompt ran", || ctx.fmt("h", "#{@typed}") == "typed");
    assert_eq!(ctx.ok(&["show-prompt-history", "-T", "search"]), "History for search:\n\n1: needle\n\n");
    assert_eq!(ctx.ok(&["show-prompt-history", "-T", "command"]), "History for command:\n\n1: typed\n\n");
    assert!(ctx.fails(&["command-prompt", "-t", &client, "-T", "window-target", "x"]).contains("unknown type: window-target"));
    // commands run from the prompt are logged for its client
    assert!(ctx.ok(&["show-messages"]).contains(&format!("{client} command: set-option -g @typed typed")));
}

#[test]
fn send_keys_as_typed_and_detach_exec() {
    let ctx = Ctx::new("sendk");
    let mut t = start(&ctx, "k");
    let client = ctx.ok(&["list-clients", "-F", "#{client_name}"]).trim().to_string();
    ctx.ok(&["bind-key", "-n", "F9", "set", "-ga", "@order", "a"]);
    // -K: through the client's key tables; the binding runs before the next command
    ctx.ok(&["send-keys", "-K", "-c", &client, "F9", ";", "set", "-ga", "@order", "b"]);
    assert_eq!(ctx.fmt("k", "#{@order}"), "ab");
    // the prefix and a prefix binding
    ctx.ok(&["send-keys", "-K", "-c", &client, "C-b", "c"]);
    wait_for("new window", || ctx.fmt("k", "#{session_windows}") == "2");
    // without a client there is nothing to type into
    let ctx2 = Ctx::new("sendk-none");
    ctx2.ok(&["new-session", "-d", "-s", "n"]);
    ctx2.ok(&["send-keys", "-K", "-t", "n", "x"]);
    // detach-client -E: the command replaces the client
    ctx.ok(&["detach-client", "-t", &client, "-E", "echo EXEC-REPLACED-42"]);
    t.wait_screen("exec output", |s| s.contains("EXEC-REPLACED-42"));
    t.wait_exit();
    assert_eq!(ctx.ok(&["list-clients"]), "");
}

#[test]
fn menu_and_popup_lines_styles_and_closing_keys() {
    let ctx = Ctx::new("menu-look");
    let t = start(&ctx, "m");
    let client = ctx.ok(&["list-clients", "-F", "#{client_name}"]).trim().to_string();
    // menus and popups make the command wait until they close
    let bg = |args: &[&str]| ctx.command().args(args).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    assert!(ctx.fails(&["display-menu", "-c", &client, "-b", "wavy", "x", "x", "display x"]).contains("menu-border-lines unknown value: wavy"));
    let mut w1 = bg(&["display-menu", "-c", &client, "-b", "double", "-x", "0", "-y", "10", "-T", "T", "First", "f", "display first"]);
    t.wait_screen("double menu border", |s| s.contains('╔') && s.contains("First"));
    t.send(b"\x1b");
    t.wait_screen("menu closed", |s| !s.contains('╔'));
    assert!(ctx.fails(&["display-popup", "-c", &client, "-b", "wavy", "true"]).contains("popup-border-lines unknown value: wavy"));
    // -b rounded; the command exits; without -k only Escape or C-c close it
    let mut w2 = bg(&["display-popup", "-c", &client, "-b", "rounded", "-w", "30", "-h", "8", "echo POPUP-DONE"]);
    t.wait_screen("rounded popup", |s| s.contains('╭') && s.contains("POPUP-DONE"));
    t.send(b"x");
    std::thread::sleep(Duration::from_millis(300));
    assert!(t.contents().contains('╭'), "a key other than Escape closed the popup");
    t.send(b"\x1b");
    t.wait_screen("popup closed", |s| !s.contains('╭'));
    // -k: any key closes it once the command has exited
    let mut w3 = bg(&["display-popup", "-c", &client, "-k", "-b", "heavy", "-w", "30", "-h", "8", "echo POPUP-K"]);
    t.wait_screen("heavy popup", |s| s.contains('┏') && s.contains("POPUP-K"));
    t.send(b"x");
    t.wait_screen("popup closed by any key", |s| !s.contains('┏'));
    for w in [&mut w1, &mut w2, &mut w3] {
        wait_for("waiting command", || w.try_wait().unwrap().is_some());
    }
}

#[test]
fn attaching_and_typing_make_the_session_current() {
    let ctx = Ctx::new("p1fix-attcur");
    ctx.ok(&["new-session", "-d", "-s", "a", "sleep 100"]);
    ctx.ok(&["new-session", "-d", "-s", "b", "sleep 100"]);
    assert_eq!(ctx.ok(&["display-message", "-p", "#S"]), "b\n");
    // attaching updates the session's activity: commands from outside now use it,
    // with or without -c (cmd_find_best_session)
    let t = Term::attach(&ctx, &["attach-session", "-t", "a"], 80, 24);
    t.wait_screen("attach", |s| s.contains("[a]"));
    assert_eq!(ctx.ok(&["display-message", "-p", "#S"]), "a\n");
    let client = ctx.ok(&["list-clients", "-F", "#{client_name}"]);
    assert_eq!(ctx.ok(&["display-message", "-p", "-c", client.trim(), "#S #{client_session}"]), "a a\n");
    ctx.ok(&["new-session", "-d", "-s", "c", "sleep 100"]);
    assert_eq!(ctx.ok(&["display-message", "-p", "-c", client.trim(), "#S #{client_session}"]), "c a\n");
    // so do keys from its client
    t.send(b"x");
    wait_for("key activity", || ctx.ok(&["display-message", "-p", "#S"]) == "a\n");
}

#[test]
fn refresh_client_l_reads_the_terminal_clipboard() {
    let ctx = Ctx::new("p1fix-clip");
    let t = start(&ctx, "clip");
    // replies nobody asked for are dropped
    t.send(b"\x1b]52;c;aWdub3JlZA==\x07");
    ctx.ok(&["set-option", "-g", "@sync", "1"]);
    assert_eq!(ctx.ok(&["list-buffers"]), "");
    let client = ctx.ok(&["list-clients", "-F", "#{client_name}"]);
    ctx.ok(&["refresh-client", "-l", "-t", client.trim()]);
    // tty_clipboard_query: Ms with "?"
    wait_for("clipboard query", || t.raw_text().contains("\x1b]52;;?\x07"));
    t.send(b"\x1b]52;c;aGVsbG8gY2xpcA==\x07");
    wait_for("clipboard buffer", || ctx.ok(&["list-buffers", "-F", "#{buffer_sample}"]) == "hello clip\n");
    // the reply is not typed into the pane
    assert!(!ctx.capture("clip").contains("52;"));
}

#[test]
fn client_focus_flag_follows_focus_reports() {
    let ctx = Ctx::new("p1fix-focus");
    let t = start(&ctx, "fo");
    let flags = || ctx.ok(&["list-clients", "-F", "#{client_flags}"]);
    assert_eq!(flags(), "attached,focused,UTF-8\n");
    t.send(b"\x1b[O");
    wait_for("focus out", || flags() == "attached,UTF-8\n");
    t.send(b"\x1b[I");
    wait_for("focus in", || flags() == "attached,focused,UTF-8\n");
}
