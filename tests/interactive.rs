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
fn floating_and_modal_panes_on_screen() {
    let ctx = Ctx::new("float-ui");
    let t = start(&ctx, "fu");
    ctx.ok(&["set", "-g", "pane-border-status", "top-floating"]);
    let f = ctx.ok(&["new-pane", "-P", "-F", "#{pane_id}", "-t", "fu", "-x", "30", "-y", "8", "-X", "10", "-Y", "5", "-T", "flt-title", "-B", "double", "cat"]).trim().to_string();
    // the border is drawn around the content with the pane's lines and its title on top
    t.wait_screen("floating pane border", |s| s.contains("flt-title"));
    let top = t.row(5);
    assert!(top.contains('╔') && top.contains('╗') && top.contains("flt-title"), "{top:?}");
    assert!(t.row(12).contains('╚'), "{:?}", t.row(12));
    // typed text goes to the floating pane, which is active
    t.send(b"typed-into-float\r");
    wait_for("text in the floating pane", || ctx.capture(&f).contains("typed-into-float"));
    // a modal pane with -K gets the prefix key too; -D closes it with Escape
    let m = ctx.ok(&["new-pane", "-O", "-K", "-D", "-P", "-F", "#{pane_id}", "-t", "fu", "cat -v"]).trim().to_string();
    wait_for("modal pane", || ctx.fmt("fu", "#{window_modal_pane}") == m);
    t.send(b"\x02x");
    wait_for("prefix sent to the modal pane", || ctx.capture(&m).contains("^Bx"));
    assert_eq!(ctx.fmt("fu", "#{window_panes}"), "3");
    t.send(b"\x1b");
    wait_for("modal pane closed", || ctx.fmt("fu", "#{window_modal_pane}").is_empty() && ctx.fmt("fu", "#{window_panes}") == "2");
    // the pane active before the modal one is active again
    assert_eq!(ctx.fmt("fu", "#{pane_id}"), f);
    // a click on the tiled pane makes it active; the floating pane is still drawn above it
    ctx.ok(&["set", "-g", "mouse", "on"]);
    t.mouse(0, 60, 20, false);
    t.mouse(0, 60, 20, true);
    wait_for("click selects the tiled pane", || ctx.fmt("fu", "#{pane_floating_flag}") == "0");
    t.mouse(0, 15, 8, false);
    t.mouse(0, 15, 8, true);
    wait_for("click selects the floating pane", || ctx.fmt("fu", "#{pane_id}") == f);
}

#[test]
fn floating_panes_follow_mouse_drags() {
    let ctx = Ctx::new("float-drag");
    let t = start(&ctx, "fd");
    ctx.ok(&["set", "-g", "mouse", "on"]);
    ctx.ok(&["bind", "-n", "M-MouseDrag1Pane", "move-pane", "-M"]);
    let f = ctx.ok(&["new-pane", "-P", "-F", "#{pane_id}", "-t", "fd", "-x", "30", "-y", "8", "-X", "10", "-Y", "5", "cat"]).trim().to_string();
    t.wait_screen("floating pane", |s| s.contains('┌'));
    // dragging the right border (column 39) resizes it (MouseDrag1Border runs resize-pane -M)
    t.mouse(0, 39, 8, false);
    t.mouse(32, 43, 8, false);
    t.mouse(0, 43, 8, true);
    wait_for("resized by the border", || ctx.fmt(&f, "#{pane_width}x#{pane_height} #{pane_x},#{pane_y}") == "32x6 11,6");
    // the top border moves it
    t.mouse(0, 20, 5, false);
    t.mouse(32, 22, 7, false);
    t.mouse(0, 22, 7, true);
    wait_for("moved by the top border", || ctx.fmt(&f, "#{pane_x},#{pane_y}") == "13,8");
    // M-drag inside moves it too (move-pane -M)
    t.mouse(8, 20, 12, false);
    t.mouse(40, 21, 11, false);
    t.mouse(8, 21, 11, true);
    wait_for("moved by move-pane -M", || ctx.fmt(&f, "#{pane_x},#{pane_y}") == "14,7");
}

#[test]
fn floating_pane_above_the_active_floating_pane_hides_its_cursor() {
    let ctx = Ctx::new("float-cursor");
    let t = start(&ctx, "fc");
    let hidden = || t.screen.lock().unwrap().screen().hide_cursor();
    ctx.ok(&["new-pane", "-t", "fc", "-x", "20", "-y", "5", "-X", "10", "-Y", "4", "printf xy; exec cat"]);
    t.wait_screen("active floating pane", |s| s.contains("xy"));
    wait_for("cursor shown", || !hidden() && t.screen.lock().unwrap().screen().cursor_position() == (5, 13));
    // a pane stacked above (new-pane -d) covers the cursor cell
    let d = ctx.ok(&["new-pane", "-d", "-P", "-F", "#{pane_id}", "-t", "fc", "-x", "20", "-y", "5", "-X", "8", "-Y", "3", "cat"]).trim().to_string();
    assert_eq!(ctx.fmt(&d, "#{pane_z} #{pane_active}"), "0 0");
    wait_for("cursor hidden under the floating pane", hidden);
    ctx.ok(&["kill-pane", "-t", &d]);
    wait_for("cursor shown again", || !hidden());
}

#[test]
fn pane_status_lines_fit_what_floating_panes_leave_visible() {
    let ctx = Ctx::new("float-status-width");
    let t = start(&ctx, "sw");
    ctx.ok(&["set", "-g", "status", "off"]);
    ctx.ok(&["set", "-g", "pane-border-status", "top"]);
    ctx.ok(&["set", "-g", "pane-border-format", "#[align=left]L#{pane_index}#[align=right]R#{pane_index}"]);
    // clipped by the window edge: formatted to the last column but one (its border)
    ctx.ok(&["new-pane", "-t", "sw", "-x", "16", "-y", "4", "-X", "90", "-Y", "4", "cat"]);
    t.wait_screen("clipped floating status", |_| t.row(4).ends_with("┌──L1──R1─"));
    // covering the right end of the tiled status line: R0 moves left of it
    ctx.ok(&["move-pane", "-t", "sw", "-X", "90", "-Y", "0"]);
    t.wait_screen("covered tiled status", |_| t.row(0).ends_with("──R0┌──L1──R1─"));
    assert!(t.row(0).starts_with("──L0──"), "{:?}", t.row(0));
    // the default format's controls are clicked where they are drawn
    ctx.ok(&["set", "-gu", "pane-border-format"]);
    ctx.ok(&["set", "-g", "mouse", "on"]);
    ctx.ok(&["bind", "-n", "MouseDown1Control8", "set -gF @hit '8 #{pane_index}'"]);
    t.wait_screen("controls left of the floating pane", |_| t.row(0).contains("[f][z][x]┌"));
    t.wait_screen("mouse mode on", |_| t.raw_text().contains("\x1b[?1006h"));
    let row = t.row(0);
    let col = row[..row.find("[z]").unwrap()].chars().count() as u16;
    t.mouse(0, col + 1, 0, false);
    t.mouse(0, col + 1, 0, true);
    wait_for("[z] hit", || ctx.ok(&["show", "-gqv", "@hit"]).trim() == "8 0");
}

#[test]
fn modal_panes_keep_their_drags_and_status_clicks() {
    let ctx = Ctx::new("float-modal-mouse");
    let t = start(&ctx, "mm");
    ctx.ok(&["set", "-g", "mouse", "on"]);
    t.wait_screen("mouse mode on", |_| t.raw_text().contains("\x1b[?1006h"));
    ctx.ok(&["new-window", "-d", "-t", "mm:1", "-n", "other", "cat"]);
    let m = ctx.ok(&["new-pane", "-O", "-C", "-P", "-F", "#{pane_id}", "-t", "mm:0", "-x", "30", "-y", "8", "-X", "20", "-Y", "6", "cat"]).trim().to_string();
    t.wait_screen("modal pane", |s| s.contains('┌'));
    // a fast drag of the top border leaves the modal pane but still moves it (modal_drag)
    t.mouse(0, 29, 6, false);
    t.mouse(32, 30, 6, false);
    t.mouse(32, 59, 17, false);
    t.mouse(0, 59, 17, true);
    wait_for("modal pane dragged", || ctx.fmt(&m, "#{pane_width}x#{pane_height} #{pane_x},#{pane_y}") == "28x6 51,18");
    // a status line click is no click outside the modal pane: it selects the window
    std::thread::sleep(Duration::from_millis(400));
    let status = t.row(29);
    let col = status.find("1:other").unwrap_or_else(|| panic!("{status:?}")) as u16;
    t.mouse(0, col, 29, false);
    t.mouse(0, col, 29, true);
    wait_for("window selected from the status line", || ctx.fmt("mm", "#{window_index}") == "1");
    assert_eq!(ctx.fmt("mm:0", "#{window_modal_pane}"), m);
    // a press in the window outside closes the -C pane
    ctx.ok(&["select-window", "-t", "mm:0"]);
    std::thread::sleep(Duration::from_millis(400));
    t.mouse(0, 5, 5, false);
    t.mouse(0, 5, 5, true);
    wait_for("modal pane closed by the click", || ctx.fmt("mm:0", "#{window_modal_pane}").is_empty());
}

#[test]
fn floating_pane_default_mouse_bindings_and_pane_list() {
    let ctx = Ctx::new("float-bind");
    let t = start(&ctx, "fb");
    ctx.ok(&["set", "-g", "mouse", "on"]);
    t.wait_screen("mouse mode on", |_| t.raw_text().contains("\x1b[?1006h"));
    ctx.ok(&["select-pane", "-T", "t0"]);
    ctx.ok(&["split-window", "-h", "-t", "fb", "cat"]);
    ctx.ok(&["select-pane", "-T", "t1"]);
    assert_eq!(ctx.fmt("fb", "#{pane_width} #{pane_x}"), "49 51");
    // C-drag (C-MouseDrag1Pane: new-pane -M) makes a floating pane between the two corners
    t.mouse(16, 4, 2, false);
    t.mouse(48, 5, 3, false);
    t.mouse(48, 19, 8, false);
    t.mouse(16, 19, 8, true);
    wait_for("dragged floating pane", || ctx.fmt("fb", "#{pane_floating_flag} #{pane_width}x#{pane_height} #{pane_x},#{pane_y}") == "1 14x5 5,3");
    let f = ctx.fmt("fb", "#{pane_id}");
    ctx.ok(&["kill-pane", "-t", &f]);
    // the [f][z][x] controls of the default pane-border-format: [z] of pane 1 zooms it
    ctx.ok(&["set", "-g", "pane-border-status", "top"]);
    t.wait_screen("pane status lines", |s| s.lines().next().is_some_and(|l| l.contains("[f][z][x]┬")));
    let top = t.row(0);
    assert!(top.contains("──1 \"t1\"──") && top.ends_with("─[f][z][x]"), "{top:?}");
    t.mouse(0, 95, 0, false);
    t.mouse(0, 95, 0, true);
    wait_for("zoomed by [z]", || ctx.fmt("fb", "#{window_zoomed_flag} #P") == "1 1");
    // (after the double click time, or this is SecondClick1Control8)
    std::thread::sleep(Duration::from_millis(400));
    t.mouse(0, 95, 0, false);
    t.mouse(0, 95, 0, true);
    wait_for("unzoomed by [u]", || ctx.fmt("fb", "#{window_zoomed_flag}") == "0");
    // [f] of pane 0 floats it
    std::thread::sleep(Duration::from_millis(400));
    t.mouse(0, 42, 0, false);
    t.mouse(0, 42, 0, true);
    wait_for("floated by [f]", || ctx.fmt("fb.0", "#{pane_floating_flag}") == "1");
    ctx.ok(&["join-pane", "-s", "fb.0"]);
    // status 2 shows the panes; a click on one selects it
    ctx.ok(&["set", "-g", "status", "2"]);
    t.wait_screen("pane list", |s| s.contains("P: "));
    let list = t.row(29);
    let other = if ctx.fmt("fb", "#P") == "0" { "1" } else { "0" };
    let col = list.find(&format!("{other}:[t{other}]")).unwrap_or_else(|| panic!("{list:?}")) as u16;
    std::thread::sleep(Duration::from_millis(400));
    t.mouse(0, col + 1, 29, false);
    t.mouse(0, col + 1, 29, true);
    wait_for("pane selected from the status line", || ctx.fmt("fb", "#P") == other);
}

#[test]
fn kept_dead_panes_close_on_a_key() {
    let ctx = Ctx::new("float-dead-key");
    let t = start(&ctx, "dk");
    // new-pane -k keeps the pane once its command exits, until a key is pressed (tmux
    // remain-on-exit key and server_client_handle_dead_key); the key goes nowhere else
    let f = ctx.ok(&["new-pane", "-k", "-P", "-F", "#{pane_id}", "-t", "dk", "-x", "40", "-y", "8", "true"]).trim().to_string();
    wait_for("dead floating pane", || ctx.fmt(&f, "#{pane_dead}") == "1");
    assert_eq!(ctx.fmt(&f, "#{remain-on-exit} #{pane_floating_flag}"), "key 1");
    t.send(b"x");
    wait_for("closed by a key", || ctx.fmt("dk", "#{window_panes}") == "1");
    // -m sets the message, drawn on the last line of the pane like server_destroy_pane
    let f = ctx.ok(&["new-pane", "-m", "bye #{pane_dead_status}", "-P", "-F", "#{pane_id}", "-t", "dk", "-x", "40", "-y", "8", "-X", "2", "-Y", "2", "exit 3"]).trim().to_string();
    wait_for("dead pane with a message", || ctx.fmt(&f, "#{pane_dead}") == "1");
    t.wait_screen("message on the last line", |_| t.row(8).contains("│bye 3"));
    // the prefix key closes it too, without starting the prefix table
    t.send(b"\x02");
    wait_for("closed by the prefix", || ctx.fmt("dk", "#{window_panes}") == "1");
    t.send(b"c");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(ctx.fmt("dk", "#{session_windows}"), "1");
}
