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
pub fn binary() -> std::ffi::OsString {
    std::env::var_os("PT_TEST_BIN").unwrap_or_else(|| BIN.into())
}
static SEQ: AtomicU32 = AtomicU32::new(0);

pub struct Ctx {
    pub socket: String,
}

impl Ctx {
    pub fn new(name: &str) -> Ctx {
        Ctx { socket: format!("t-{name}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)) }
    }

    pub fn command(&self) -> Command {
        let mut c = Command::new(binary());
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

/// Pane commands for the shell that runs them (default-shell -c): sh on Unix, PowerShell
/// (powershell.exe or pwsh -Command) on Windows, where `cat` is Get-Content and there is no
/// printf. Windows commands use single quotes only: the command line quotes the argument.
pub fn sh<'a>(unix: &'a str, windows: &'a str) -> &'a str {
    if cfg!(windows) { windows } else { unix }
}

/// argv that runs a script without default-shell: separate words are run as a program
/// (`sh -c` on Unix; Windows has no POSIX programs, but always has Windows PowerShell).
pub fn shell_argv<'a>(unix: &'a str, windows: &'a str) -> Vec<&'a str> {
    if cfg!(windows) { vec!["powershell", "-NoProfile", "-Command", windows] } else { vec!["sh", "-c", unix] }
}

/// argv that sleeps `secs` seconds, run without a shell; each word is a plain one, so
/// #{pane_start_command} is the words joined by spaces.
pub fn sleep_argv(secs: &str) -> Vec<&str> {
    if cfg!(windows) { vec!["powershell", "-NoProfile", "-Command", "Start-Sleep", secs] } else { vec!["sleep", secs] }
}

/// Like `cat` on a terminal: shows what is typed, control keys as ^X (a tty's echoctl).
pub const CAT: &str = if cfg!(windows) { ECHO_KEYS } else { "cat" };
/// Like `cat -v`: the same for the keys these tests type.
pub const CAT_V: &str = if cfg!(windows) { ECHO_KEYS } else { "cat -v" };
const ECHO_KEYS: &str = "while ($true) { $c = [int][Console]::ReadKey($true).KeyChar; if ($c -eq 13) { [Console]::WriteLine() } elseif ($c -lt 32) { [Console]::Write('^' + [char]($c + 64)) } else { [Console]::Write([char]$c) } }";

/// Prints `text` (plain ASCII, no quotes) without a newline, then keeps the pane open.
pub fn print_then_sleep(text: &str, secs: u32) -> String {
    if cfg!(windows) { format!("[Console]::Write('{text}'); Start-Sleep {secs}") } else { format!("printf {text}; sleep {secs}") }
}

/// What to type at the interactive shell's prompt for `n` lines of "line" (history).
pub fn typed_lines(n: u32) -> String {
    if cfg!(windows) { format!("1..{n} | ForEach-Object {{ 'line' }}\r") } else { format!("printf 'line\\n%.0s' {{1..{n}}}\r") }
}

fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v).unwrap()
}

/// An attached client running in a pseudo terminal, with its screen emulated.
pub struct Term {
    pub screen: Arc<Mutex<vt100::Parser>>,
    pub raw: Arc<Mutex<Vec<u8>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub master: Option<Box<dyn MasterPty + Send>>,
    pub child: Box<dyn Child + Send + Sync>,
    /// The client's copy of what it writes to the terminal (PWSH_TMUX_OUTPUT_COPY).
    output_copy: Option<std::path::PathBuf>,
}

impl Term {
    pub fn attach(ctx: &Ctx, args: &[&str], cols: u16, rows: u16) -> Term {
        Term::attach_answering(ctx, args, cols, rows, &[])
    }

    /// attach, in a terminal that answers each query in `answers` with its reply, in the
    /// order the queries came (as a real terminal does).
    pub fn attach_answering(ctx: &Ctx, args: &[&str], cols: u16, rows: u16, answers: &[(&[u8], &[u8])]) -> Term {
        let mut cb = CommandBuilder::new(binary());
        cb.args(args);
        cb.env("PWSH_TMUX_SOCKET", &ctx.socket);
        cb.env_remove("PWSH_TMUX");
        cb.env_remove("PWSH_TMUX_PANE");
        cb.env("TERM", "xterm-256color");
        // a known terminal: 256 colours, no truecolor unless a test asks for it
        cb.env_remove("COLORTERM");
        // TERM_PROGRAM names the outer terminal's tmux default features
        cb.env_remove("TERM_PROGRAM");
        let copy = std::env::temp_dir().join(format!("pwsh-tmux-out-{}-{}", ctx.socket, SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_file(&copy);
        cb.env("PWSH_TMUX_OUTPUT_COPY", &copy);
        let mut t = Term::spawn_answering(cb, cols, rows, answers);
        t.output_copy = Some(copy);
        t
    }

    /// Runs any program (e.g. `ssh -tt host pwsh-tmux ...`) in a pty.
    pub fn spawn(cb: CommandBuilder, cols: u16, rows: u16) -> Term {
        Term::spawn_answering(cb, cols, rows, &[])
    }

    fn spawn_answering(cb: CommandBuilder, cols: u16, rows: u16, answers: &[(&[u8], &[u8])]) -> Term {
        let answers: Vec<(Vec<u8>, Vec<u8>)> = answers.iter().map(|(q, r)| (q.to_vec(), r.to_vec())).collect();
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
                    let mut replies: Vec<(usize, &[u8])> = answers.iter().flat_map(|(q, r)| {
                        buf[..n].windows(q.len()).enumerate().filter(move |(_, w)| *w == &q[..]).map(move |(i, _)| (i, &r[..]))
                    }).collect();
                    replies.sort_by_key(|(i, _)| *i);
                    if !replies.is_empty() {
                        let _ = writer.lock().unwrap().write_all(&replies.into_iter().flat_map(|(_, r)| r.to_vec()).collect::<Vec<u8>>());
                    }
                    screen.lock().unwrap().process(&buf[..n]);
                    raw.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            });
        }
        Term { screen, raw, writer, master: Some(pair.master), child, output_copy: None }
    }

    /// The screen's rows, one line each. (vt100's contents() joins a row marked as soft
    /// wrapped with the next one: ConPTY marks every row whose last column was written as
    /// wrapped and repaints it so the terminal wraps it, so full-width rows such as a
    /// highlighted list item or a box edge would run into the next row on Windows.)
    pub fn contents(&self) -> String {
        let p = self.screen.lock().unwrap();
        let (_, cols) = p.screen().size();
        let mut s = p.screen().rows(0, cols.get()).collect::<Vec<_>>().join("\n");
        s.truncate(s.trim_end_matches('\n').len());
        s
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
        // an Escape at the end is a key only once the server's escape delay passes:
        // at least 500ms while the terminal's replies to the attach requests are awaited
        // (tmux 3.8 tty_keys_next; this terminal does not answer them, so for the first
        // five seconds), so the next key is not read as that key with Meta
        let pause = if b.ends_with(b"\x1b") { 650 } else { 120 };
        std::thread::sleep(Duration::from_millis(pause));
    }

    /// Writes the bytes as typed, without waiting after them.
    pub fn type_now(&self, b: &[u8]) {
        let mut w = self.writer.lock().unwrap();
        w.write_all(b).unwrap();
        w.flush().unwrap();
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

    /// Closes the terminal under the client (like closing the terminal window).
    pub fn hangup(&mut self) {
        // Unix: what the kernel does when a terminal closes. (Dropping portable-pty's
        // writer there would type ^D into the terminal first.)
        #[cfg(unix)]
        if let Some(pid) = self.child.process_id() {
            let _ = std::process::Command::new("kill").args(["-HUP", &pid.to_string()]).status();
        }
        #[cfg(windows)]
        {
            *self.writer.lock().unwrap() = Box::new(std::io::sink());
            self.master = None;
        }
    }

    pub fn raw_text(&self) -> String {
        String::from_utf8_lossy(&self.raw.lock().unwrap()).into_owned()
    }

    /// What the client wrote to its terminal. On Unix that is what the terminal got; on
    /// Windows ConPTY repaints it for the terminal and drops sequences it handles itself
    /// (such as OSC 52 queries), so the client's own copy is read.
    pub fn sent_text(&self) -> String {
        match self.output_copy.as_ref().filter(|_| cfg!(windows)) {
            Some(p) => String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).into_owned(),
            None => self.raw_text(),
        }
    }

    /// wait_for that shows the screen, the cursor and the end of the terminal output when
    /// it times out.
    pub fn wait(&self, what: &str, mut f: impl FnMut() -> bool) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(20) {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let raw = self.raw_text();
        let tail: String = raw.chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
        let cursor = self.screen.lock().unwrap().screen().cursor_position();
        panic!("timed out waiting for {what}; cursor {cursor:?}; screen:\n{}\noutput tail: {tail:?}", self.contents());
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        let _ = self.child.kill();
        if let Some(p) = &self.output_copy {
            let _ = std::fs::remove_file(p);
        }
    }
}
