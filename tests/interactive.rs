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
    // -J: the echoed command line may wrap in the half-width pane (a shell's ESC k title
    // used to leave a second copy of it on the screen).
    // (send-keys itself is refused: its target client, the current one, is read-only)
    assert_eq!(ctx.fails(&["send-keys", "-t", "ro", "x"]).trim(), "client is read-only");
    ctx.ok(&["set-buffer", "-b", "barrier", "echo WRITABLE_BARRIER\n"]);
    ctx.ok(&["paste-buffer", "-d", "-b", "barrier", "-t", "ro"]);
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
fn copy_mode_count_then_rebound_or_unbound_key() {
    // the key after a repeat count goes through the copy-mode table (tmux closes the
    // numeric prompt and dispatches the key with the count as its prefix)
    let ctx = Ctx::new("copycount");
    let t = start(&ctx, "n");
    ctx.ok(&["set", "-g", "mode-keys", "vi"]);
    t.send(b"clear; seq 1 60\r");
    wait_for("output", || ctx.capture("n").contains("60"));
    ctx.ok(&["bind", "-T", "copy-mode-vi", "J", "send", "-X", "cursor-down"]);
    ctx.ok(&["unbind", "-T", "copy-mode-vi", "K"]);
    t.prefix(b"[");
    wait_for("copy mode", || ctx.fmt("n", "#{pane_in_mode}") == "1");
    ctx.ok(&["send-keys", "-X", "-t", "n", "history-top"]);
    t.send(b"12J");
    wait_for("rebound J moves the cursor", || ctx.fmt("n", "#{copy_cursor_y} #{scroll_position}").starts_with("12 "));
    let before = ctx.fmt("n", "#{copy_cursor_y} #{scroll_position}");
    t.send(b"3K");
    t.send(b"j");
    wait_for("unbound K does nothing, j moves 3", || ctx.fmt("n", "#{copy_cursor_y}") == "15");
    assert!(before.ends_with(&ctx.fmt("n", "#{scroll_position}")));
    t.send(b"q");
    wait_for("quit", || ctx.fmt("n", "#{pane_in_mode}") == "0");
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
    // terminfo colors#256 is not the 256 feature (tmux adds that from DA replies or -2)
    assert!(!feats.contains("256") && feats.contains("clipboard") && !feats.contains("RGB"), "{feats}");
    assert_eq!(ctx.ok(&["list-clients", "-F", "#{client_colours}"]).trim(), "16");
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
    ctx.ok(&["command-prompt", "-b", "-t", &client, "-T", "search", "set -g @searched '%%'"]);
    t.send(b"needle\r");
    wait_for("search prompt ran", || ctx.fmt("h", "#{@searched}") == "needle");
    // without -b the command waits for the prompt (CMD_RETURN_WAIT)
    let mut waiting = ctx.command().args(["command-prompt", "-t", &client, "set -g @typed '%%'"]).stdout(std::process::Stdio::null()).spawn().unwrap();
    wait_for("prompt open", || t.row(29).starts_with("(set)"));
    std::thread::sleep(Duration::from_millis(300));
    assert!(waiting.try_wait().unwrap().is_none(), "command-prompt returned before the prompt closed");
    t.send(b"typed\r");
    wait_for("command prompt ran", || ctx.fmt("h", "#{@typed}") == "typed");
    wait_for("command-prompt returns", || waiting.try_wait().unwrap().is_some());
    // copy-mode searches go in the search history too
    ctx.ok(&["copy-mode", "-t", "h"]);
    t.send(b"\x13");
    t.wait_screen("search prompt", |s| s.contains("(search down)"));
    t.send(b"other\r");
    wait_for("search history", || ctx.ok(&["show-prompt-history", "-T", "search"]).contains("2: other"));
    ctx.ok(&["send-keys", "-t", "h", "-X", "cancel"]);
    assert_eq!(ctx.ok(&["show-prompt-history", "-T", "search"]), "History for search:\n\n1: needle\n2: other\n\n");
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
    // popups make the command wait until they close; display-menu returns at once
    let bg = |args: &[&str]| ctx.command().args(args).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    assert!(ctx.fails(&["display-menu", "-c", &client, "-b", "wavy", "x", "x", "display x"]).contains("menu-border-lines unknown value: wavy"));
    let mut w1 = bg(&["display-menu", "-c", &client, "-b", "double", "-x", "0", "-y", "10", "-T", "T", "First", "f", "display first"]);
    t.wait_screen("double menu border", |s| s.contains('╔') && s.contains("First"));
    wait_for("display-menu returns while the menu is open", || w1.try_wait().unwrap().is_some());
    assert!(t.contents().contains('╔'));
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
    for w in [&mut w2, &mut w3] {
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

#[test]
fn readonly_client_send_keys_and_refresh_client_control_flags() {
    let ctx = Ctx::new("p1fix-rosend");
    ctx.ok(&["new-session", "-d", "-s", "s1", "-x", "80", "-y", "24", "cat"]);
    let t = Term::attach(&ctx, &["attach-session", "-r", "-t", "s1"], 80, 24);
    t.wait_screen("readonly attach", |s| s.contains("[s1]"));
    let client = ctx.ok(&["list-clients", "-F", "#{client_name}"]).trim().to_string();
    // the target client of send-keys may not be read-only (except for -X)
    assert_eq!(ctx.fails(&["send-keys", "-c", &client, "-t", "s1", "abc"]).trim(), "client is read-only");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!ctx.capture("s1").contains("abc"));
    // a trailing ':' is trimmed from a client name
    assert_eq!(ctx.ok(&["display-message", "-p", "-c", &format!("{client}:"), "#{client_name}"]).trim(), client);
    // -A, -B and -C are for control clients, checked first
    for args in [["-C", "90x20"], ["-B", "x"], ["-A", "%0:on"]] {
        assert_eq!(ctx.fails(&["refresh-client", "-t", &client, args[0], args[1]]).trim(), "not a control client", "{args:?}");
    }
    drop(t);
}

#[test]
fn status_line_formats_have_no_pane_type() {
    let ctx = Ctx::new("p1fix-stype");
    ctx.ok(&["new-session", "-d", "-s", "s1", "-x", "60", "-y", "10", "sleep 100"]);
    let f = "#{session_format}#{window_format}#{pane_format}";
    ctx.ok(&["set", "-g", "status-left", &format!("L[{f}]")]);
    ctx.ok(&["set", "-g", "status-right", &format!("R[{f}]")]);
    ctx.ok(&["set", "-g", "window-status-current-format", &format!("W[{f}]")]);
    let t = Term::attach(&ctx, &["attach-session", "-t", "s1"], 60, 10);
    // format_defaults(ft, c, NULL, NULL, NULL); window entries are window formats
    t.wait_screen("status formats", |s| s.contains("L[000]W[010]") && s.contains("R[000]"));
}

#[test]
fn message_log_names_keys_messages_and_errors() {
    let ctx = Ctx::new("p1fix-msglog");
    let t = start(&ctx, "s1");
    ctx.ok(&["bind-key", "-n", "F5", "set", "-g", "@f5", "1"]);
    t.send(b"\x1b[15~");
    wait_for("F5 binding", || ctx.cmd(&["show-options", "-gv", "@f5"]).1 == "1\n");
    ctx.ok(&["display-message", "hello"]);
    ctx.fails(&["nosuch"]);
    ctx.fails(&["new-window", "-Q"]);
    let client = ctx.ok(&["list-clients", "-F", "#{client_name}"]).trim().to_string();
    // send-keys -K: the bound commands are logged for the client that sent the keys
    ctx.ok(&["bind-key", "-n", "F6", "set", "-g", "@f6", "1"]);
    ctx.ok(&["send-keys", "-K", "-c", &client, "F6"]);
    let log: Vec<String> = ctx.ok(&["show-messages"]).lines().map(|l| l.split_once(' ').map_or(l, |x| x.1).to_string()).collect();
    let has = |want: &str| log.iter().any(|l| l == want);
    assert!(has(&format!("{client} key F5: set-option -g @f5 1")), "{log:?}");
    assert!(has(&format!("{client} message: hello")), "{log:?}");
    assert!(log.iter().any(|l| l.ends_with(" message: unknown command: nosuch") && l.starts_with("client-")), "{log:?}");
    assert!(log.iter().any(|l| l.ends_with(" message: command new-window: unknown flag -Q")), "{log:?}");
    let f6 = log.iter().find(|l| l.ends_with("command: set-option -g @f6 1")).expect("F6 logged");
    assert!(!f6.starts_with(&format!("{client} ")), "{f6}");
    // -T: the terminal and its tty_term flags (VT100-like, 0x20)
    let term = ctx.ok(&["show-messages", "-T"]);
    let flags = term.split("flags=0x").nth(1).and_then(|r| r.split(':').next()).and_then(|h| u32::from_str_radix(h, 16).ok()).unwrap();
    assert!(term.starts_with("Terminal 0: ") && flags & 0x20 != 0, "{term}");
    drop(t);
}

#[cfg(unix)]
#[test]
fn client_theme_detection_and_pane_colours() {
    // tmux 3.8: the client asks its terminal for the theme (mode 2031, DSR 996) and colours
    // (OSC 10/11); builtin UI colours follow the detected theme on 256+ colour terminals.
    let ctx = Ctx::new("theme");
    ctx.ok(&["new-session", "-d", "-s", "th", "-x", "80", "-y", "24", "printf '\\033]11;#00ff00\\007'; sleep 30"]);
    let t = Term::attach(&ctx, &["-T", "RGB", "attach", "-t", "th"], 80, 24);
    t.wait_screen("status", |s| s.contains("[th]"));
    assert!(t.raw_text().contains("\x1b[?2031h\x1b[?996n"), "theme subscription and query");
    assert!(t.raw_text().contains("\x1b]10;?\x1b\\\x1b]11;?\x1b\\"), "colour queries");
    // nothing reported yet: the dark theme (status bg yellowgreen)
    wait_for("dark status", || t.raw_text().contains("48;2;154;205;50"));
    // the pane's own OSC 11 colour is its default background
    assert!(t.raw_text().contains("48;2;0;255;0"), "pane background");
    // a light background reply infers the light theme without setting client_theme
    t.send(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\");
    wait_for("light status", || t.raw_text().contains("48;2;46;139;87"));
    assert_eq!(ctx.ok(&["list-clients", "-F", "[#{client_theme}]"]).trim(), "[]");
    // a theme report wins over the background
    t.send(b"\x1b[?997;1n");
    wait_for("reported theme", || ctx.ok(&["list-clients", "-F", "#{client_theme}"]).trim() == "dark");
    ctx.ok(&["set", "-s", "theme", "terminal"]);
    // theme terminal: the old ANSI colours (black on green)
    wait_for("terminal colours", || t.raw_text().contains("\x1b[0;30;42m"));
}

#[test]
fn display_message_line_partial_format_and_vi_prompt() {
    let ctx = Ctx::new("display-messages");
    let t = start(&ctx, "s");
    ctx.ok(&["set", "-t", "s", "status", "2"]);
    ctx.ok(&["set", "-t", "s", "status-format[0]", "FIRST"]);
    ctx.ok(&["set", "-t", "s", "status-format[1]", "SECOND preserved"]);
    ctx.ok(&["set", "-t", "s", "message-line", "4"]);
    ctx.ok(&["set", "-t", "s", "message-style", "fg=red"]);
    ctx.ok(&["set", "-t", "s", "message-format", "#[#{E:message-style}]#{message}"]);
    ctx.ok(&["display-message", "-d", "5000", "Hi"]);
    wait_for("partial message", || t.row(29).starts_with("HiCOND preserved"));
    assert!(t.row(28).starts_with("FIRST"));
    t.send(b"\x1b");
    ctx.ok(&["set", "-t", "s", "message-style", "fg=red,fill=blue"]);
    ctx.ok(&["display-message", "-d", "5000", "Hi"]);
    wait_for("filled message", || t.row(29).trim_end() == "Hi");
    t.send(b"\x1b");
    ctx.ok(&["set", "-t", "s", "message-format", "#[#{?#{command_prompt},#{E:message-command-style},#{E:message-style}}]#{message}"]);
    ctx.ok(&["set", "-t", "s", "message-command-style", "fg=green,fill=black"]);
    ctx.ok(&["set", "-t", "s", "status-keys", "vi"]);
    t.prefix(b"T");
    t.send(b"\x15abc");
    t.send(b"\x1b");
    wait_for("vi command style", || t.screen.lock().unwrap().screen().cell(29, 0).unwrap().fgcolor() == vt100::Color::Idx(2));
    // Escape moves left once: x removes c, then append replaces it with Z.
    t.send(b"xaZ\r");
    wait_for("vi title editing", || ctx.fmt("s", "#{pane_title}") == "abZ");
    ctx.ok(&["set", "-t", "s", "status-keys", "emacs"]);
    t.prefix(b"T"); t.send(b"\x15changed\x1b");
    assert_eq!(ctx.fmt("s", "#{pane_title}"), "abZ");
}

#[test]
fn display_scrollbars_reserve_overlay_timeout_hover_and_mouse() {
    let ctx = Ctx::new("display-scrollbar");
    let t = start(&ctx, "s");
    ctx.ok(&["set", "-p", "-t", "s", "pane-scrollbars-style", "fg=red,bg=blue,width=2,pad=1"]);
    ctx.ok(&["setw", "-t", "s", "pane-scrollbars", "on"]);
    wait_for("reserved scrollbar", || ctx.fmt("s", "#{pane_width}") == "97");
    wait_for("right scrollbar", || t.screen.lock().unwrap().screen().cell(27, 99).unwrap().bgcolor() == vt100::Color::Idx(1));
    ctx.ok(&["setw", "-t", "s", "pane-scrollbars-position", "left"]);
    wait_for("left scrollbar", || t.screen.lock().unwrap().screen().cell(27, 0).unwrap().bgcolor() == vt100::Color::Idx(1));
    ctx.ok(&["setw", "-t", "s", "pane-scrollbars-timeout", "200"]);
    ctx.ok(&["setw", "-t", "s", "pane-scrollbars", "auto-hide"]);
    wait_for("overlay width", || ctx.fmt("s", "#{pane_width}") == "100");
    t.send(b"\x1b[<35;1;2M");
    wait_for("hover shows scrollbar", || t.screen.lock().unwrap().screen().cell(27, 0).unwrap().bgcolor() == vt100::Color::Idx(1));
    t.send(b"\x1b[<35;50;2M");
    wait_for("timeout hides scrollbar", || t.screen.lock().unwrap().screen().cell(27, 0).unwrap().bgcolor() != vt100::Color::Idx(1));
    t.prefix(b"[");
    ctx.ok(&["send-keys", "-t", "s", "-X", "page-up"]);
    wait_for("scroll shows scrollbar", || matches!(t.screen.lock().unwrap().screen().cell(27, 0).unwrap().bgcolor(), vt100::Color::Idx(1 | 4)));
    ctx.ok(&["copy-mode", "-q", "-t", "s"]);
    ctx.ok(&["setw", "-t", "s", "pane-scrollbars", "modal"]);
    t.send(b"\x1b[<35;1;2M");
    assert_eq!(ctx.fmt("s", "#{pane_width}"), "100");
    ctx.ok(&["setw", "-t", "s", "pane-scrollbars", "on"]);
    t.send(b"printf 'line\\n%.0s' {1..120}\r");
    wait_for("history for scrollbar click", || ctx.fmt("s", "#{history_size}").parse::<usize>().unwrap_or(0) > 30);
    t.mouse(0, 0, 1, false);
    t.mouse(0, 0, 1, true);
    wait_for("scrollbar page-up binding", || ctx.fmt("s", "#{pane_in_mode}") == "1");
    assert!(ctx.fmt("s", "#{scroll_position}").parse::<usize>().unwrap() > 0);
}

#[test]
fn display_border_indicators_and_fill_inside_outside_window() {
    let ctx = Ctx::new("display-fill");
    let t = start(&ctx, "s");
    ctx.ok(&["setw", "-t", "s", "window-size", "manual"]);
    ctx.ok(&["resize-window", "-t", "s", "-x", "70", "-y", "20"]);
    ctx.ok(&["setw", "-t", "s", "fill-character", "#{?is_inside,.,~}"]);
    wait_for("outside fill", || t.row(22).starts_with("~~~~"));
    ctx.ok(&["split-window", "-h", "-t", "s"]);
    ctx.ok(&["setw", "-t", "s", "pane-border-indicators", "both"]);
    t.wait_screen("border arrows with both", |s| s.contains('→'));
    ctx.ok(&["setw", "-t", "s", "pane-border-indicators", "off"]);
    wait_for("arrows off", || !t.contents().contains('→'));
    ctx.ok(&["setw", "-t", "s", "pane-border-indicators", "arrows"]);
    t.wait_screen("border arrows", |s| s.contains('→'));
}

#[test]
fn display_menu_and_popup_use_configured_border_lines() {
    let ctx = Ctx::new("display-boxes");
    let t = start(&ctx, "s");
    ctx.ok(&["setw", "-t", "s", "menu-border-lines", "double"]);
    t.prefix(b">");
    t.wait_screen("double menu border", |s| s.contains('╔') && s.contains("Horizontal Split"));
    t.send(b"\x1b");
    ctx.ok(&["setw", "-t", "s", "popup-border-lines", "rounded"]);
    let client = ctx.ok(&["list-clients", "-F", "#{client_name}"]).trim().to_string();
    let mut popup = ctx.command().args(["display-popup", "-c", &client, "-w", "30", "-h", "8", "-E", "echo popup; sleep 10"]).spawn().unwrap();
    t.wait_screen("rounded popup border", |s| s.contains('╭') && s.contains("popup"));
    t.send(b"\x1b");
    assert!(popup.wait().unwrap().success());
}

#[test]
fn display_prompt_label_then_input_and_command_mode_base_style() {
    let ctx = Ctx::new("display-prompt");
    let t = start(&ctx, "s");
    let cursor = || t.screen.lock().unwrap().screen().cursor_position();
    // tmux 3.8 prompt_layout: message-format is the label, the input follows it
    ctx.ok(&["set", "-t", "s", "message-format", "[#{message}]"]);
    t.prefix(b":");
    t.send(b"abc");
    wait_for("label then input", || t.row(29).starts_with("[:]abc") && cursor() == (29, 6));
    t.send(b"\x03");
    // aligned in the message-style area, the rest of the area filled
    ctx.ok(&["set", "-t", "s", "message-style", "bg=red,width=30,align=right,fill=blue"]);
    t.prefix(b":");
    t.send(b"abc");
    wait_for("right aligned prompt", || t.row(29).ends_with("[:]abc") && cursor() == (29, 99));
    assert_eq!(t.screen.lock().unwrap().screen().cell(29, 70).unwrap().bgcolor(), vt100::Color::Idx(4));
    assert!(t.row(29).starts_with("[s]"));
    t.send(b"\x03");
    // long input scrolls so the cursor stays in the area
    ctx.ok(&["set", "-u", "-t", "s", "message-style"]);
    ctx.ok(&["set", "-u", "-t", "s", "message-format"]);
    t.prefix(b":");
    t.send(&[b'x'; 120]);
    wait_for("scrolled input", || cursor() == (29, 99) && t.row(29).starts_with(":xxx"));
    t.send(b"\x03");
    // vi command mode: message-command-style is the base, not message-style
    ctx.ok(&["set", "-t", "s", "status-keys", "vi"]);
    ctx.ok(&["set", "-t", "s", "message-command-style", "bg=green"]);
    t.prefix(b":");
    t.send(b"abc");
    std::thread::sleep(Duration::from_millis(100));
    t.send(b"\x1b");
    wait_for("command mode", || t.screen.lock().unwrap().screen().cell(29, 0).unwrap().bgcolor() == vt100::Color::Idx(2) && cursor() == (29, 3));
    assert_eq!(t.screen.lock().unwrap().screen().cell(29, 0).unwrap().fgcolor(), vt100::Color::Default);
}

#[test]
fn display_window_edges_fill_style_and_arrows_of_every_pane() {
    let ctx = Ctx::new("display-edges");
    let t = start(&ctx, "s");
    ctx.ok(&["setw", "-t", "s", "window-size", "manual"]);
    ctx.ok(&["resize-window", "-t", "s", "-x", "70", "-y", "20"]);
    // the window's right and bottom edges are borders, ┘ at the corner
    wait_for("window edges", || t.row(20).starts_with("──") && t.row(20).chars().nth(70) == Some('┘') && t.row(0).chars().nth(70) == Some('│'));
    // fill colours go over pane-border-style
    ctx.ok(&["setw", "-t", "s", "pane-border-style", "fg=red"]);
    ctx.ok(&["setw", "-t", "s", "fill-character", "a"]);
    wait_for("fill over border style", || t.row(25).starts_with("aaa"));
    assert_eq!(t.screen.lock().unwrap().screen().cell(25, 0).unwrap().fgcolor(), vt100::Color::Idx(1));
    ctx.ok(&["setw", "-t", "s", "window-size", "latest"]);
    // every pane marks arrow cells; those next to the active pane point at it
    ctx.ok(&["setw", "-t", "s", "pane-border-indicators", "arrows"]);
    ctx.ok(&["split-window", "-h", "-t", "s"]);
    ctx.ok(&["split-window", "-v", "-t", "s"]);
    ctx.ok(&["select-pane", "-t", "s.0"]);
    t.wait_screen("two arrows at the left pane", |s| s.matches('←').count() == 2 && !s.contains('↓'));
}

#[test]
fn display_auto_hide_scrollbar_shows_only_when_scrolling_and_draws_pad() {
    let ctx = Ctx::new("display-autohide");
    let t = start(&ctx, "s");
    let bg = |y: u16, x: u16| t.screen.lock().unwrap().screen().cell(y, x).unwrap().bgcolor();
    t.send(b"printf 'line\\n%.0s' {1..120}\r");
    wait_for("history", || ctx.fmt("s", "#{history_size}").parse::<usize>().unwrap_or(0) > 30);
    ctx.ok(&["setw", "-t", "s", "pane-scrollbars-style", "fg=red,bg=blue,width=2,pad=2"]);
    ctx.ok(&["setw", "-t", "s", "pane-scrollbars-timeout", "300"]);
    ctx.ok(&["setw", "-t", "s", "pane-scrollbars", "auto-hide"]);
    // entering copy mode or moving without scrolling does not show it
    ctx.ok(&["copy-mode", "-t", "s"]);
    ctx.ok(&["send-keys", "-t", "s", "-X", "cursor-up"]);
    std::thread::sleep(Duration::from_millis(150));
    assert_ne!(bg(20, 99), vt100::Color::Idx(4));
    // scrolling shows it over the pane: the bar, then the pad in pane colours
    ctx.ok(&["send-keys", "-t", "s", "-X", "history-top"]);
    wait_for("scrolled scrollbar", || bg(20, 99) == vt100::Color::Idx(4) && bg(20, 98) == vt100::Color::Idx(4));
    assert_eq!(bg(20, 97), vt100::Color::Default);
    assert_eq!(t.row(20).chars().nth(96), Some(' '));
    assert_eq!(bg(0, 99), vt100::Color::Idx(1));
    wait_for("hidden after the timeout", || bg(20, 99) != vt100::Color::Idx(4));
    ctx.ok(&["send-keys", "-t", "s", "-X", "cancel"]);
    // width below 1 is 1, colours from the default style
    ctx.ok(&["setw", "-t", "s", "pane-scrollbars-style", "width=0"]);
    ctx.ok(&["setw", "-t", "s", "pane-scrollbars", "on"]);
    wait_for("default width", || ctx.fmt("s", "#{pane_width}") == "99" && bg(20, 99) != vt100::Color::Default);
}

#[test]
fn display_message_keys_prompt_words_and_menu_flags() {
    let ctx = Ctx::new("display-keys");
    let t = start(&ctx, "s");
    let cursor = || t.screen.lock().unwrap().screen().cursor_position();
    let client = ctx.ok(&["list-clients", "-F", "#{client_name}"]).trim().to_string();
    // display-time 0: the message stays until a key, which is then handled
    ctx.ok(&["set", "-t", "s", "display-time", "0"]);
    ctx.ok(&["display-message", "-c", &client, "zero-time"]);
    wait_for("message", || t.row(29).starts_with("zero-time"));
    std::thread::sleep(Duration::from_millis(900));
    assert!(t.row(29).starts_with("zero-time"));
    t.send(b"echo keyed-1");
    wait_for("key clears message and reaches the pane", || !t.row(29).starts_with("zero-time") && ctx.capture("s").contains("keyed-1"));
    t.send(b"\x15");
    // -N with a timer: keys are ignored while it shows
    ctx.ok(&["display-message", "-c", &client, "-N", "-d", "3000", "ignoring"]);
    wait_for("-N message", || t.row(29).starts_with("ignoring"));
    t.send(b"echo dropped-2");
    std::thread::sleep(Duration::from_millis(300));
    assert!(t.row(29).starts_with("ignoring"));
    assert!(!ctx.capture("s").contains("dropped-2"));
    wait_for("-N message times out", || !t.row(29).starts_with("ignoring"));
    // vi words: separators are words of their own; j is history down (empty)
    ctx.ok(&["set", "-t", "s", "status-keys", "vi"]);
    t.prefix(b":");
    t.send(b"set foo-bar.baz qux");
    std::thread::sleep(Duration::from_millis(100));
    t.send(b"\x1b");
    t.send(b"0w");
    wait_for("w to the word", || cursor() == (29, 5));
    t.send(b"w");
    wait_for("w stops at a separator", || cursor() == (29, 8));
    t.send(b"e");
    wait_for("e to the end of bar", || cursor() == (29, 11));
    t.send(b"j");
    wait_for("j empties the line", || t.row(29).trim_end() == ":");
    t.send(b"q");
    // emacs: C-v quotes the next key, M-f stops at a separator
    ctx.ok(&["set", "-t", "s", "status-keys", "emacs"]);
    t.prefix(b":");
    t.send(b"ab-cd\x01\x1bf");
    wait_for("M-f to the end of ab", || cursor() == (29, 3));
    t.send(b"\x05\x16\x01");
    wait_for("quoted C-a", || t.row(29).starts_with(":ab-cd^A"));
    t.send(b"\x03");
    // display-menu -b and -S override the options
    ctx.ok(&["display-menu", "-c", &client, "-b", "double", "-S", "fg=red", "-x", "0", "-y", "10", "Item", "i", ""]);
    t.wait_screen("double menu", |s| s.contains('╔') && s.contains("Item"));
    let top = t.contents().lines().position(|l| l.contains('╔')).unwrap() as u16;
    assert_eq!(t.screen.lock().unwrap().screen().cell(top, 0).unwrap().fgcolor(), vt100::Color::Idx(1));
    t.send(b"\x1b");
    wait_for("menu closed", || !t.contents().contains('╔'));
    assert!(ctx.command().args(["display-menu", "-b", "bad", "Item", "i", ""]).output().unwrap().stderr.starts_with(b"menu-border-lines unknown value: bad"));
}

#[test]
fn display_prompt_flags_completion_and_mouse_like_tmux_3_8() {
    let ctx = Ctx::new("display-prompt38");
    let t = start(&ctx, "s");
    let cursor = || t.screen.lock().unwrap().screen().cursor_position();
    let client = ctx.ok(&["list-clients", "-F", "#{client_name}"]).trim().to_string();
    ctx.ok(&["set", "-g", "display-time", "5000"]);
    let prompt = |args: &[&str]| {
        let mut a = vec!["command-prompt", "-b", "-t", &client];
        a.extend_from_slice(args);
        ctx.ok(&a);
        std::thread::sleep(Duration::from_millis(150));
    };
    // -N: a key that is not a digit runs the command and then goes to the pane
    t.send(b"echo ");
    prompt(&["-N", "-p", "num:", "display \"got [%%]\""]);
    t.send(b"12z");
    wait_for("numeric done", || t.row(29).starts_with("got [12]"));
    wait_for("z typed into the pane", || ctx.capture("s").contains("echo z"));
    t.send(b"\x15");
    // -1 takes control keys and BSpace as one character
    prompt(&["-1", "-p", "one:", "set -g @one '%%'"]);
    t.send(b"\x01");
    wait_for("C-a", || ctx.ok(&["show", "-gv", "@one"]) == "\x01\n");
    prompt(&["-1", "-p", "one:", "set -g @one '%%'"]);
    t.send(b"\x7f");
    wait_for("BSpace", || ctx.ok(&["show", "-gv", "@one"]) == "\x7f\n");
    // -1 with -I text closes without running anything
    prompt(&["-1", "-I", "ab", "-p", "one:", "set -g @one '%%'"]);
    t.send(b"q");
    wait_for("closed", || !t.row(29).starts_with("one:"));
    assert_eq!(ctx.ok(&["show", "-gv", "@one"]), "\x7f\n");
    // -i runs the template on every change after '=', C-s/C-r give '+'/'-'
    prompt(&["-i", "-I", "zz", "-p", "inc:", "set -g @inc '%%'"]);
    wait_for("start", || ctx.ok(&["show", "-gv", "@inc"]) == "=\n");
    t.send(b"\x13");
    wait_for("C-s on empty takes -I", || ctx.ok(&["show", "-gv", "@inc"]) == "=zz\n");
    t.send(b"y\x12");
    wait_for("C-r", || ctx.ok(&["show", "-gv", "@inc"]) == "-zzy\n");
    t.send(b"\x7f");
    wait_for("BSpace", || ctx.ok(&["show", "-gv", "@inc"]) == "=zz\n");
    t.send(b"\r");
    wait_for("Enter closes", || !t.row(29).starts_with("inc:"));
    assert_eq!(ctx.ok(&["show", "-gv", "@inc"]), "=zz\n");
    // %%% escapes double quotes; empty input runs the command too
    prompt(&["-p", "q:", "display \"got [%%%]\""]);
    t.send(b"a\"b\r");
    wait_for("quote3", || t.row(29).starts_with("got [a\"b]"));
    prompt(&["-p", "q:", "display \"empty [%%]\""]);
    t.send(b"\r");
    wait_for("empty input", || t.row(29).starts_with("empty []"));
    // confirm-before puts a space after the prompt; -l adds none
    ctx.ok(&["confirm-before", "-b", "-t", &client, "display yes"]);
    wait_for("confirm", || t.row(29).starts_with("Confirm 'display-message'? (y/n)"));
    assert_eq!(cursor(), (29, 33));
    t.send(b"n");
    prompt(&["-l", "-p", "a:,b:", "-I", "x,y", "display \"%1\""]);
    wait_for("literal", || t.row(29).starts_with("a:,b:x,y"));
    assert_eq!(cursor(), (29, 8));
    t.send(b"\x03");
    // Tab completes to the common prefix, then lists the matches after the input
    t.prefix(b":");
    t.send(b"sel\t");
    wait_for("common prefix", || t.row(29).trim_end() == ":select-");
    t.send(b"\t");
    wait_for("matches", || t.row(29).trim_end() == ":select- select-layout select-pane select-window");
    assert_eq!(cursor(), (29, 8));
    // a click on a match picks it, a click in the input moves the cursor
    ctx.ok(&["set", "-g", "mouse", "on"]);
    t.mouse(0, 25, 29, false);
    t.mouse(0, 25, 29, true);
    wait_for("picked", || t.row(29).trim_end() == ":select-pane");
    t.send(b"\x15display hello world");
    t.mouse(0, 4, 29, false);
    t.mouse(0, 4, 29, true);
    wait_for("cursor moved", || cursor() == (29, 4));
    t.send(b"X");
    wait_for("typed at the cursor", || t.row(29).starts_with(":disXplay hello world"));
    t.send(b"\x03");
    // errors start with a capital and their # are text; a click clears a message
    t.prefix(b":");
    t.send(b"x#[reverse]zz##q\r");
    wait_for("error", || t.row(29).starts_with("Unknown command: x#[reverse]zz##q"));
    t.mouse(0, 10, 5, false);
    t.mouse(0, 10, 5, true);
    wait_for("click clears the message", || !t.row(29).starts_with("Can't"));
}

#[test]
fn display_menu_popup_positions_and_pane_prompt_like_tmux_3_8() {
    let ctx = Ctx::new("display-pos38");
    let t = start(&ctx, "s");
    let client = ctx.ok(&["list-clients", "-F", "#{client_name}"]).trim().to_string();
    let rows = || t.contents().lines().map(str::to_string).collect::<Vec<_>>();
    let row_of = |pat: &str| rows().iter().position(|l| l.contains(pat));
    // display-popup waits for the popup to close
    let popup = |args: &[&str]| {
        let mut a = vec!["display-popup", "-c", &client];
        a.extend_from_slice(args);
        ctx.command().args(&a).spawn().unwrap()
    };
    // menus are placed inside the window, above a bottom status line
    ctx.ok(&["display-menu", "-c", &client, "-x", "10", "-y", "100%", "-T", "T", "First", "a", "display 1"]);
    wait_for("menu", || row_of("First").is_some());
    assert_eq!(row_of("└"), Some(28));
    assert!(t.row(29).contains("[s]"));
    t.send(b"q");
    wait_for("menu closed", || row_of("First").is_none());
    // and below one at the top
    ctx.ok(&["set", "-g", "status-position", "top"]);
    ctx.ok(&["display-menu", "-c", &client, "-x", "10", "-y", "0", "-T", "T", "First", "a", "display 1"]);
    wait_for("top menu", || row_of("First").is_some());
    assert_eq!(row_of("┌"), Some(1));
    t.send(b"q");
    wait_for("menu closed", || row_of("First").is_none());
    ctx.ok(&["set", "-g", "status-position", "bottom"]);
    // popup -x/-y take letters and formats; y is the bottom row
    let _p1 = popup(&["-x", "#{e|+:2,3}", "-y", "6", "-w", "12", "-h", "4", "printf hi; sleep 10"]);
    wait_for("popup", || row_of("hi").is_some());
    assert_eq!(row_of("┌"), Some(2));
    assert_eq!(rows()[2].find('┌'), Some(5));
    // Escape closes a popup without -E even while its command runs
    t.send(b"\x1b");
    wait_for("popup closed", || row_of("┌").is_none());
    // popups are drawn over the status line
    let _p2 = popup(&["-x", "R", "-y", "100%", "-w", "12", "-h", "4", "printf hi; sleep 10"]);
    wait_for("popup over status", || t.row(29).contains('└'));
    assert_eq!(t.row(29).find('└'), Some(88));
    t.send(b"\x03");
    wait_for("popup closed", || row_of("┌").is_none());
    // after its command exits, a key that is not Escape leaves it open
    let _p3 = popup(&["-x", "0", "-y", "0", "-w", "12", "-h", "4", "printf noE"]);
    wait_for("popup", || row_of("noE").is_some());
    std::thread::sleep(Duration::from_millis(300));
    t.send(b"q");
    std::thread::sleep(Duration::from_millis(300));
    assert!(row_of("noE").is_some());
    t.send(b"\x1b");
    wait_for("popup closed", || row_of("noE").is_none());
    // command-prompt -P prompts on the pane's last line, the status line stays
    ctx.ok(&["command-prompt", "-b", "-P", "-t", &client, "-p", "pane:", "set -g @pp '%%'"]);
    wait_for("pane prompt", || t.row(28).starts_with("pane: "));
    assert!(t.row(29).contains("[s]"));
    t.send(b"hi\r");
    wait_for("pane prompt done", || ctx.ok(&["show", "-gv", "@pp"]) == "hi\n");
    assert!(!t.row(28).starts_with("pane:"));
    // copy-mode searches prompt in the pane too
    t.prefix(b"[");
    t.send(b"\x12");
    wait_for("search prompt in the pane", || t.row(28).starts_with("(search up) "));
    t.send(b"\x1b");
    wait_for("search prompt closed", || !t.row(28).starts_with("(search up)"));
    t.send(b"q");
}

#[test]
fn display_border_lines_spaces_none_and_numbers() {
    let ctx = Ctx::new("display-lines38");
    let t = start(&ctx, "s");
    ctx.ok(&["set", "-g", "status", "off"]);
    ctx.ok(&["split-window", "-h", "-t", "s"]);
    ctx.ok(&["split-window", "-v", "-t", "s"]);
    let x = ctx.fmt("s", "#{pane_left}").trim().parse::<usize>().unwrap() - 1;
    let y = ctx.fmt("s", "#{pane_top}").trim().parse::<u16>().unwrap() - 1;
    wait_for("borders", || t.row(0).chars().nth(x) == Some('│'));
    for lines in ["spaces", "none"] {
        ctx.ok(&["set", "-g", "pane-border-lines", lines]);
        wait_for(lines, || !t.contents().contains('│') && !t.contents().contains('─'));
    }
    // number: the pane's index; a junction next to the active pane is its number
    ctx.ok(&["set", "-g", "pane-border-lines", "number"]);
    wait_for("numbers", || t.row(1).chars().nth(x) == Some('0'));
    assert_eq!(t.row(y).chars().nth(x), Some('2'));
    assert_eq!(t.row(y).chars().nth(x + 1), Some('2'));
    assert_eq!(t.row(y + 1).chars().nth(x), Some('2'));
}

#[test]
fn display_scrollbar_clicks_slider_drag_and_modes_like_tmux_3_8() {
    let ctx = Ctx::new("display-sb38");
    let t = start(&ctx, "s");
    let bg = |y: u16, x: u16| t.screen.lock().unwrap().screen().cell(y, x).unwrap().bgcolor();
    ctx.ok(&["set", "-g", "status", "off"]);
    ctx.ok(&["set", "-g", "mouse", "on"]);
    t.send(b"printf 'line\\n%.0s' {1..120}\r");
    wait_for("history", || ctx.fmt("s", "#{history_size}").parse::<usize>().unwrap_or(0) > 30);
    ctx.ok(&["setw", "-t", "s", "pane-scrollbars-style", "fg=red,bg=blue"]);
    ctx.ok(&["setw", "-t", "s", "pane-scrollbars-timeout", "300"]);
    ctx.ok(&["setw", "-t", "s", "pane-scrollbars", "auto-hide"]);
    // history-bottom at the bottom does not scroll, so the bar stays hidden
    ctx.ok(&["copy-mode", "-t", "s"]);
    ctx.ok(&["send-keys", "-t", "s", "-X", "history-bottom"]);
    std::thread::sleep(Duration::from_millis(150));
    assert_ne!(bg(20, 99), vt100::Color::Idx(4));
    ctx.ok(&["send-keys", "-t", "s", "-X", "cancel"]);
    // a hidden auto-hide bar takes no clicks
    wait_for("out of copy mode", || ctx.fmt("s", "#{pane_in_mode}") == "0");
    t.mouse(0, 99, 10, false);
    t.mouse(0, 99, 10, true);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(ctx.fmt("s", "#{pane_in_mode}"), "0");
    // with a pane status line at the top, its row is a border, not the bar
    ctx.ok(&["setw", "-t", "s", "pane-scrollbars", "on"]);
    ctx.ok(&["setw", "-t", "s", "pane-border-status", "top"]);
    wait_for("bar", || bg(10, 99) != vt100::Color::Default);
    t.mouse(0, 99, 0, false);
    t.mouse(0, 99, 0, true);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(ctx.fmt("s", "#{pane_in_mode}"), "0");
    ctx.ok(&["setw", "-t", "s", "pane-border-status", "off"]);
    // dragging the slider keeps the grabbed point under the mouse
    ctx.ok(&["copy-mode", "-t", "s"]);
    let hsize: i64 = ctx.fmt("s", "#{history_size}").parse().unwrap();
    let h = 30i64;
    let (slider_h, slider_y) = ((h * h / (hsize + h)).max(1), (h + 1) * hsize / (hsize + h));
    let grab = (slider_y + slider_h - 1) as u16;
    wait_for("slider drawn", || bg(grab, 99) == vt100::Color::Idx(1));
    t.mouse(0, 99, grab, false);
    t.mouse(32, 99, grab - 1, false);
    let top = ((slider_y - 1) as f64 * ((hsize + h) as f64 / h as f64)) as i64;
    wait_for("dragged one row", || ctx.fmt("s", "#{scroll_position}") == (hsize - top).to_string());
    t.mouse(32, 99, 0, false);
    t.mouse(0, 99, 0, true);
    wait_for("dragged to the top", || ctx.fmt("s", "#{scroll_position}") == hsize.to_string());
    ctx.ok(&["send-keys", "-t", "s", "-X", "cancel"]);
    // clock mode keeps the bar, with a slider the full height (no history)
    ctx.ok(&["clock-mode", "-t", "s"]);
    wait_for("clock bar", || bg(0, 99) == vt100::Color::Idx(1) && bg(29, 99) == vt100::Color::Idx(1));
    assert_eq!(ctx.fmt("s", "#{pane_width}"), "99");
}

#[test]
fn display_pane_status_alignment_scrollbar_offset_and_arrows() {
    let ctx = Ctx::new("display-bstatus38");
    let t = start(&ctx, "s");
    ctx.ok(&["set", "-g", "status", "off"]);
    ctx.ok(&["set", "-g", "mouse", "on"]);
    ctx.ok(&["split-window", "-h", "-t", "s"]);
    ctx.ok(&["select-pane", "-t", "s:0.0", "-T", "t0"]);
    ctx.ok(&["select-pane", "-t", "s:0.1", "-T", "t1"]);
    let sep = ctx.fmt("s", "#{pane_left}").trim().parse::<usize>().unwrap() - 1;
    ctx.ok(&["set", "-g", "pane-border-status", "top"]);
    // #[align=right] in pane-border-format puts the buttons at the right end
    wait_for("pane status", || t.row(0).starts_with("──0 \"t0\"─"));
    let row: Vec<char> = t.row(0).chars().collect();
    assert_eq!(row[sep - 3..sep].iter().collect::<String>(), "[x]", "{}", t.row(0));
    assert_eq!(row[sep], '┬');
    // with the scrollbar on the left the line moves with the pane content
    ctx.ok(&["set", "-g", "pane-scrollbars", "on"]);
    ctx.ok(&["set", "-g", "pane-scrollbars-position", "left"]);
    wait_for("shifted", || t.row(0).starts_with("───0 \"t0\"─"));
    // arrows: on the status line before the active pane's text, and one row
    // below the pane's first row on the side border
    ctx.ok(&["set", "-g", "pane-border-indicators", "arrows"]);
    wait_for("status arrow", || t.row(0).chars().nth(sep + 3) == Some('↓'));
    assert_eq!(t.row(2).chars().nth(sep), Some('→'));
    assert_eq!(t.row(1).chars().nth(sep), Some('│'));
}

#[test]
fn display_prompt_cursor_style_and_colour() {
    let ctx = Ctx::new("display-pcursor");
    let t = start(&ctx, "s");
    ctx.ok(&["set", "-g", "prompt-cursor-style", "bar"]);
    ctx.ok(&["set", "-g", "prompt-cursor-colour", "red"]);
    t.prefix(b":");
    wait_for("prompt cursor", || t.raw_text().contains("\x1b[6 q") && t.raw_text().contains("\x1b]12;rgb:80/00/00\x07"));
    t.send(b"\x03");
    wait_for("cursor colour reset", || t.raw_text().contains("\x1b]112\x07"));
    // vi command mode uses the prompt-command-* options
    ctx.ok(&["set", "-g", "status-keys", "vi"]);
    ctx.ok(&["set", "-g", "prompt-command-cursor-style", "blinking-underline"]);
    ctx.ok(&["set", "-g", "prompt-command-cursor-colour", "blue"]);
    t.prefix(b":");
    t.send(b"a\x1b");
    wait_for("command mode cursor", || t.raw_text().contains("\x1b[3 q") && t.raw_text().contains("\x1b]12;rgb:00/00/80\x07"));
    t.send(b"\x03");
}

/// A terminal client resizes once attached (client-resized with the same size), and
/// select-window updates focus before session-window-changed; only the windows told
/// about change their focus (a stale focused pane is not reported).
#[test]
fn attach_resize_and_focus_order() {
    let ctx = Ctx::new("attfocus");
    ctx.ok(&["new-session", "-d", "-s", "f", "-x", "70", "-y", "20"]);
    ctx.ok(&["split-window", "-d", "-t", "f:0"]);
    ctx.ok(&["new-window", "-d", "-t", "f:1"]);
    ctx.ok(&["set", "-g", "focus-events", "on"]);
    for h in ["client-resized", "pane-focus-in", "pane-focus-out", "session-window-changed", "window-pane-changed"] {
        ctx.ok(&["set-hook", "-g", h, &format!("set -gaF @log '{h} '")]);
    }
    let mut t = Term::attach(&ctx, &["attach", "-t", "f"], 70, 21);
    t.wait_screen("attached", |s| s.contains("[f]"));
    wait_for("resized", || ctx.cmd(&["show", "-gqv", "@log"]).1.contains("client-resized"));
    ctx.ok(&["set", "-g", "@log", ""]);
    ctx.ok(&["select-window", "-t", "f:1"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@log"]).trim(), "pane-focus-out pane-focus-in session-window-changed");
    // focus-events off: selecting updates no focus; on again, only the two panes
    // select-pane changes are updated (f:0's pane keeps its stale focus)
    ctx.ok(&["set", "-g", "focus-events", "off"]);
    ctx.ok(&["select-window", "-t", "f:0"]);
    ctx.ok(&["set", "-g", "focus-events", "on"]);
    ctx.ok(&["set", "-g", "@log", ""]);
    ctx.ok(&["select-pane", "-t", "f:0.1"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@log"]).trim(), "pane-focus-in window-pane-changed");
    drop(t.child.kill());
    t.wait_exit();
}
