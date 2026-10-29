//! End-to-end test: runs `pwsh-tmux attach` inside a pseudo terminal, drives it
//! with keystrokes and checks the rendered screen and server state.

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_pwsh-tmux");

struct Ctx {
    socket: String,
}

impl Ctx {
    fn new(name: &str) -> Ctx {
        Ctx { socket: format!("test-{name}-{}", std::process::id()) }
    }

    fn cmd(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new(BIN)
            .args(args)
            .env("PWSH_TMUX_SOCKET", &self.socket)
            .env_remove("PWSH_TMUX")
            .env_remove("PWSH_TMUX_PANE")
            .output()
            .expect("run pwsh-tmux");
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        (out.status.success(), text)
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, text) = self.cmd(args);
        assert!(ok, "{args:?} failed: {text}");
        text
    }
}

impl Drop for Ctx {
    fn drop(&mut self) {
        let _ = self.cmd(&["kill-server"]);
    }
}

fn wait_for(what: &str, timeout: Duration, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {what}");
}

fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v).unwrap()
}

#[test]
fn attach_split_type_detach() {
    let ctx = Ctx::new("attach");
    let t = Duration::from_secs(20);
    ctx.ok(&["new-session", "-d", "-s", "t", "-x", "100", "-y", "30"]);

    let pty = native_pty_system();
    let pair = pty.openpty(PtySize { rows: 30, cols: 100, pixel_width: 0, pixel_height: 0 }).unwrap();
    let mut cb = CommandBuilder::new(BIN);
    cb.args(["attach", "-t", "t"]);
    cb.env("PWSH_TMUX_SOCKET", &ctx.socket);
    cb.env_remove("PWSH_TMUX");
    cb.env_remove("PWSH_TMUX_PANE");
    cb.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(cb).unwrap();
    drop(pair.slave);

    let screen = Arc::new(Mutex::new(vt100::Parser::new(nz(30), nz(100), 0)));
    let raw = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut reader = pair.master.try_clone_reader().unwrap();
    let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
    {
        let (screen, raw, writer) = (screen.clone(), raw.clone(), writer.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                // Act like a real terminal: ConPTY waits for a cursor position report.
                if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                    let _ = writer.lock().unwrap().write_all(b"\x1b[1;1R");
                }
                screen.lock().unwrap().process(&buf[..n]);
                raw.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
    }
    let contents = || screen.lock().unwrap().screen().contents();
    let send = |b: &[u8]| {
        let mut w = writer.lock().unwrap();
        w.write_all(b).unwrap();
        w.flush().unwrap();
        drop(w);
        std::thread::sleep(Duration::from_millis(150));
    };

    let dump = || format!("screen:\n{}\nraw: {:?}", contents(), String::from_utf8_lossy(&raw.lock().unwrap()));
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wait_for("status line", t, || contents().contains("[t] 0:")))).is_err() {
        panic!("no status line; {}", dump());
    }

    // typing reaches the shell, including wide (Korean) characters
    send("echo hi-from-pane 한글테스트\r".as_bytes());
    wait_for("echo output", t, || contents().matches("hi-from-pane").count() >= 2);
    assert!(ctx.ok(&["capture-pane", "-p", "-t", "t"]).contains("한글테스트"));
    wait_for("korean on screen", t, || contents().contains("한글테스트"));

    // prefix % splits left/right; a separator appears
    send(b"\x02%");
    wait_for("split", t, || ctx.ok(&["display-message", "-p", "-t", "t", "#{window_panes}"]).trim() == "2");
    wait_for("separator", t, || contents().contains('│'));
    assert_eq!(ctx.ok(&["display-message", "-p", "-t", "t", "#{pane_index}"]).trim(), "1");

    // prefix Left moves to the left pane
    send(b"\x02\x1b[D");
    wait_for("select left", t, || ctx.ok(&["display-message", "-p", "-t", "t", "#{pane_index}"]).trim() == "0");
    // arrow keys repeat (-r): let repeat-time pass before the next prefix
    std::thread::sleep(Duration::from_millis(700));

    // prefix z zooms, again unzooms
    send(b"\x02z");
    wait_for("zoom", t, || ctx.ok(&["display-message", "-p", "-t", "t", "#{window_zoomed_flag}"]).trim() == "1");
    send(b"\x02z");
    wait_for("unzoom", t, || ctx.ok(&["display-message", "-p", "-t", "t", "#{window_zoomed_flag}"]).trim() == "0");

    // prefix c creates a window shown in the status line
    send(b"\x02c");
    wait_for("new window", t, || ctx.ok(&["lsw", "-t", "t"]).lines().count() == 2);
    wait_for("status shows window 1", t, || contents().contains("1:") && contents().contains("*"));
    send(b"\x02p");
    wait_for("previous window", t, || ctx.ok(&["display-message", "-p", "-t", "t", "#{window_index}"]).trim() == "0");

    // terminal resize propagates to the session
    pair.master.resize(PtySize { rows: 40, cols: 120, pixel_width: 0, pixel_height: 0 }).unwrap();
    screen.lock().unwrap().set_size(nz(40), nz(120));
    wait_for("resize", t, || ctx.ok(&["display-message", "-p", "-t", "t", "#{session_width}x#{session_height} #{window_height}"]).trim() == "120x40 39");

    // prefix d detaches; the session survives
    send(b"\x02d");
    wait_for("client exit", t, || child.try_wait().ok().flatten().is_some());
    wait_for("detach message", t, || String::from_utf8_lossy(&raw.lock().unwrap()).contains("[detached (from session t)]"));
    let ls = ctx.ok(&["ls"]);
    assert!(ls.contains("t: 2 windows") && !ls.contains("attached"), "{ls}");
}

#[test]
fn pane_exit_closes_session() {
    let ctx = Ctx::new("exit");
    ctx.ok(&["new-session", "-d", "-s", "e"]);
    ctx.ok(&["split-window", "-v", "-t", "e"]);
    assert_eq!(ctx.ok(&["lsp", "-t", "e"]).lines().count(), 2);
    // Shells such as pwsh drop type-ahead sent before their prompt is up.
    let prompt = |t: &str| !ctx.ok(&["capture-pane", "-p", "-t", t]).trim().is_empty();
    wait_for("prompts", Duration::from_secs(20), || prompt("e:0.0") && prompt("e:0.1"));
    ctx.ok(&["send-keys", "-t", "e", "exit", "Enter"]);
    wait_for("pane exit", Duration::from_secs(20), || ctx.ok(&["lsp", "-t", "e"]).lines().count() == 1);
    ctx.ok(&["send-keys", "-t", "e", "exit", "Enter"]);
    wait_for("session gone", Duration::from_secs(20), || !ctx.cmd(&["has-session", "-t", "e"]).0);
}

#[test]
fn targets_and_errors() {
    let ctx = Ctx::new("targets");
    ctx.ok(&["new-session", "-d", "-s", "alpha", "-n", "first"]);
    ctx.ok(&["new-window", "-t", "alpha", "-n", "second"]);
    assert_eq!(ctx.ok(&["display-message", "-p", "-t", "alpha:first", "#{window_index}"]).trim(), "0");
    assert_eq!(ctx.ok(&["display-message", "-p", "-t", "al:1.0", "#{window_name}"]).trim(), "second");
    assert!(!ctx.cmd(&["has-session", "-t", "nope"]).0);
    assert!(!ctx.cmd(&["new-session", "-d", "-s", "alpha"]).0, "duplicate name must fail");
    assert!(!ctx.cmd(&["bogus-command"]).0);
    ctx.ok(&["rename-window", "-t", "alpha:1", "renamed"]);
    assert!(ctx.ok(&["lsw", "-t", "alpha"]).contains("1: renamed"));
    ctx.ok(&["kill-window", "-t", "alpha:0"]);
    assert_eq!(ctx.ok(&["lsw", "-t", "alpha"]).lines().count(), 1);
    ctx.ok(&["kill-session", "-t", "alpha"]);
    // last session gone -> server exits by itself
    wait_for("server exit", Duration::from_secs(10), || !ctx.cmd(&["ls"]).0);
}
