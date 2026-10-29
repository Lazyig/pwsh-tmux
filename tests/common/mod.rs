//! Shared harness: a private server per test, CLI helpers and a pty-driven attached client.
#![allow(dead_code)]

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const BIN: &str = env!("CARGO_BIN_EXE_pwsh-tmux");
static SEQ: AtomicU32 = AtomicU32::new(0);

pub struct Ctx {
    pub socket: String,
}

impl Ctx {
    pub fn new(name: &str) -> Ctx {
        Ctx { socket: format!("t-{name}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)) }
    }

    pub fn command(&self) -> Command {
        let mut c = Command::new(BIN);
        c.env("PWSH_TMUX_SOCKET", &self.socket).env_remove("PWSH_TMUX").env_remove("PWSH_TMUX_PANE");
        c
    }

    pub fn cmd(&self, args: &[&str]) -> (bool, String) {
        let out = self.command().args(args).output().expect("run pwsh-tmux");
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        (out.status.success(), text)
    }

    pub fn ok(&self, args: &[&str]) -> String {
        let (ok, text) = self.cmd(args);
        assert!(ok, "{args:?} failed: {text}");
        text
    }

    pub fn fails(&self, args: &[&str]) -> String {
        let (ok, text) = self.cmd(args);
        assert!(!ok, "{args:?} should fail but printed: {text}");
        text
    }

    /// display-message -p
    pub fn fmt(&self, target: &str, f: &str) -> String {
        self.ok(&["display-message", "-p", "-t", target, f]).trim_end_matches(['\r', '\n']).to_string()
    }

    /// Waits until the pane shows a shell prompt (any output).
    pub fn wait_prompt(&self, target: &str) {
        wait_for(&format!("prompt in {target}"), || !self.ok(&["capture-pane", "-p", "-t", target]).trim().is_empty());
    }

    pub fn capture(&self, target: &str) -> String {
        self.ok(&["capture-pane", "-p", "-t", target])
    }
}

impl Drop for Ctx {
    fn drop(&mut self) {
        let _ = self.cmd(&["kill-server"]);
    }
}

pub fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
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

/// An attached client running in a pseudo terminal, with its screen emulated.
pub struct Term {
    pub screen: Arc<Mutex<vt100::Parser>>,
    pub raw: Arc<Mutex<Vec<u8>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
}

impl Term {
    pub fn attach(ctx: &Ctx, args: &[&str], cols: u16, rows: u16) -> Term {
        let mut cb = CommandBuilder::new(BIN);
        cb.args(args);
        cb.env("PWSH_TMUX_SOCKET", &ctx.socket);
        cb.env_remove("PWSH_TMUX");
        cb.env_remove("PWSH_TMUX_PANE");
        cb.env("TERM", "xterm-256color");
        Term::spawn(cb, cols, rows)
    }

    /// Runs any program (e.g. `ssh -tt host pwsh-tmux ...`) in a pty.
    pub fn spawn(cb: CommandBuilder, cols: u16, rows: u16) -> Term {
        let pty = native_pty_system();
        let pair = pty.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).unwrap();
        let child = pair.slave.spawn_command(cb).unwrap();
        drop(pair.slave);
        let screen = Arc::new(Mutex::new(vt100::Parser::new(nz(rows), nz(cols), 0)));
        let raw = Arc::new(Mutex::new(Vec::<u8>::new()));
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
        {
            let (screen, raw, writer) = (screen.clone(), raw.clone(), writer.clone());
            std::thread::spawn(move || {
                let mut buf = [0u8; 16384];
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
        Term { screen, raw, writer, master: pair.master, child }
    }

    pub fn contents(&self) -> String {
        self.screen.lock().unwrap().screen().contents()
    }

    pub fn row(&self, r: u16) -> String {
        let p = self.screen.lock().unwrap();
        let (_, cols) = p.screen().size();
        p.screen().contents_between(r, 0, r, cols.get())
    }

    pub fn send(&self, b: &[u8]) {
        let mut w = self.writer.lock().unwrap();
        w.write_all(b).unwrap();
        w.flush().unwrap();
        drop(w);
        std::thread::sleep(Duration::from_millis(120));
    }

    /// Prefix (C-b) then keys.
    pub fn prefix(&self, keys: &[u8]) {
        self.send(b"\x02");
        self.send(keys);
    }

    /// SGR mouse event at 0-based (x, y). `b`: button code (0 left, 64 wheel up, 32+ drag).
    pub fn mouse(&self, b: u8, x: u16, y: u16, release: bool) {
        let s = format!("\x1b[<{};{};{}{}", b, x + 1, y + 1, if release { 'm' } else { 'M' });
        self.send(s.as_bytes());
    }

    pub fn wait_screen(&self, what: &str, f: impl Fn(&str) -> bool) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(20) {
            if f(&self.contents()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("timed out waiting for {what}; screen:\n{}", self.contents());
    }

    pub fn wait_exit(&mut self) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(20) {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("client did not exit");
    }

    pub fn raw_text(&self) -> String {
        String::from_utf8_lossy(&self.raw.lock().unwrap()).into_owned()
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}
