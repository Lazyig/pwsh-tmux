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
    ctx.ok(&["send-keys", "-t", "ro", "echo WRITABLE_BARRIER", "Enter"]);
    wait_for("writable input barrier", || ctx.capture("ro").matches("WRITABLE_BARRIER").count() >= 2);
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
