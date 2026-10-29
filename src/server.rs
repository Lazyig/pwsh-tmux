//! The server process: owns sessions, windows and panes (PTY + terminal
//! emulator), interprets the prefix key and renders frames for attached clients.
//!
//! All state lives on the main loop thread; PTY readers, child waiters and
//! client connections run on helper threads that only send `Event`s.

use crate::ipc;
use crate::layout::{neighbour, Dir, Node, PaneId, Rect, Sep};
use crate::opts::getopt;
use crate::protocol::*;
use interprocess::local_socket::{prelude::*, Stream};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize, PtySystem};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::num::NonZeroU16;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

type ClientId = u32;

const PREFIX: u8 = 0x02; // Ctrl+b
const SCROLLBACK: usize = 2000;

pub fn log(msg: &str) {
    if let Ok(dir) = ipc::runtime_dir() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("server.log")) {
            let _ = writeln!(f, "[{}] {msg}", std::process::id());
        }
    }
}

enum Event {
    Conn(Stream),
    ClientMsg(ClientId, u8, Vec<u8>),
    ClientGone(ClientId),
    PaneData(PaneId, Vec<u8>),
    PaneExit(PaneId),
}

/// Answers terminal queries the child expects a terminal to reply to.
/// ConPTY in particular blocks at startup until its cursor-position query is answered.
#[derive(Default)]
struct Replies(Vec<u8>);

impl vt100::Callbacks for Replies {
    fn unhandled_csi(&mut self, screen: &mut vt100::Screen, i1: Option<u8>, _i2: Option<u8>, params: &[&[u16]], c: char) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c, first) {
            (None, 'n', 6) => {
                let (r, c) = screen.cursor_position();
                self.0.extend(format!("\x1b[{};{}R", r + 1, c + 1).bytes());
            }
            (None, 'n', 5) => self.0.extend(b"\x1b[0n"),
            (None, 'c', 0) => self.0.extend(b"\x1b[?1;2c"),
            _ => {}
        }
    }
}

struct Pane {
    parser: vt100::Parser<Replies>,
    input: Sender<Vec<u8>>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    size: (u16, u16),
}

impl Pane {
    fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if self.size == (cols, rows) {
            return;
        }
        self.size = (cols, rows);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.parser.set_size(nz(rows), nz(cols));
    }

    fn send(&self, data: Vec<u8>) {
        if !data.is_empty() {
            let _ = self.input.send(data);
        }
    }
}

fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v).unwrap_or(NonZeroU16::MIN)
}

struct Window {
    name: String,
    layout: Node,
    active: PaneId,
    last_active: Option<PaneId>,
    zoomed: bool,
}

struct Session {
    id: u32,
    name: String,
    windows: Vec<Window>,
    active: usize,
    last: Option<usize>,
    cols: u16,
    rows: u16,
    cwd: Option<String>,
    last_used: u64,
}

impl Session {
    /// Pane area excludes the status line.
    fn area(&self) -> Rect {
        Rect { x: 0, y: 0, w: self.cols.max(1), h: self.rows.saturating_sub(1).max(1) }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct Style {
    fg: vt100::Color,
    bg: vt100::Color,
    attrs: u8,
}

const BOLD: u8 = 1;
const DIM: u8 = 2;
const ITALIC: u8 = 4;
const UNDERLINE: u8 = 8;
const INVERSE: u8 = 16;

#[derive(Clone, PartialEq)]
struct Cell {
    /// Empty for the right half of a wide character.
    text: String,
    style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { text: " ".into(), style: Style::default() }
    }
}

struct Frame {
    w: u16,
    h: u16,
    cells: Vec<Cell>,
}

impl Frame {
    fn new(w: u16, h: u16) -> Frame {
        Frame { w, h, cells: vec![Cell::default(); w as usize * h as usize] }
    }
    fn set(&mut self, x: u16, y: u16, cell: Cell) {
        if x < self.w && y < self.h {
            self.cells[y as usize * self.w as usize + x as usize] = cell;
        }
    }
    fn row(&self, y: u16) -> &[Cell] {
        let w = self.w as usize;
        &self.cells[y as usize * w..(y as usize + 1) * w]
    }
    /// Writes `text` from (x, y), returns the next column.
    fn text(&mut self, mut x: u16, y: u16, text: &str, style: Style, limit: u16) -> u16 {
        for ch in text.chars() {
            let cw = ch.width().unwrap_or(0) as u16;
            if cw == 0 {
                continue;
            }
            if x + cw > limit.min(self.w) {
                break;
            }
            self.set(x, y, Cell { text: ch.to_string(), style });
            if cw == 2 {
                self.set(x + 1, y, Cell { text: String::new(), style });
            }
            x += cw;
        }
        x
    }
}

struct RenderState {
    frame: Frame,
    cursor: Option<(u16, u16)>,
    modes: (bool, bool),
}

struct Client {
    tx: Sender<Vec<u8>>,
    session: Option<u32>,
    size: (u16, u16),
    prefix: bool,
    prev: Option<RenderState>,
    dirty: bool,
}

impl Client {
    fn send(&self, kind: u8, payload: &[u8]) {
        let _ = self.tx.send(frame(kind, payload));
    }
}

#[derive(Clone, Copy)]
struct Target {
    si: usize,
    wi: usize,
    pane: PaneId,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Key {
    Char(char),
    Arrow(Dir, u8),
    Ctrl(u8),
    Esc,
    Other,
}

struct Server {
    tx: Sender<Event>,
    pty: Box<dyn PtySystem + Send>,
    sessions: Vec<Session>,
    panes: HashMap<PaneId, Pane>,
    clients: HashMap<ClientId, Client>,
    next_id: u32,
    next_pane: u32,
    clock: u64,
    had_session: bool,
    quit: bool,
    host: String,
}

pub fn run() -> io::Result<()> {
    if ipc::connect().is_ok() {
        return Ok(()); // another server already owns the endpoint
    }
    let listener = ipc::listen()?;
    log(&format!("server started on {}", ipc::describe()));
    let (tx, rx) = mpsc::channel();
    let atx = tx.clone();
    thread::spawn(move || {
        for conn in listener.incoming() {
            match conn {
                Ok(s) => {
                    if atx.send(Event::Conn(s)).is_err() {
                        break;
                    }
                }
                Err(e) => log(&format!("accept failed: {e}")),
            }
        }
    });

    let mut srv = Server {
        tx,
        pty: native_pty_system(),
        sessions: Vec::new(),
        panes: HashMap::new(),
        clients: HashMap::new(),
        next_id: 0,
        next_pane: 0,
        clock: 0,
        had_session: false,
        quit: false,
        host: hostname(),
    };
    let started = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(ev) => {
                srv.handle(ev);
                // Coalesce bursts of output into one render.
                let deadline = Instant::now() + Duration::from_millis(4);
                while let Ok(ev) = rx.try_recv() {
                    srv.handle(ev);
                    if Instant::now() > deadline {
                        break;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        srv.render_all();
        let idle = srv.sessions.is_empty() && (srv.had_session || started.elapsed() > Duration::from_secs(30));
        if srv.quit || idle {
            break;
        }
    }
    srv.shutdown();
    ipc::cleanup();
    log("server exiting");
    Ok(())
}

fn hostname() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            return String::from_utf8_lossy(&buf[..end]).into_owned();
        }
        String::new()
    }
}

fn which(prog: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(prog).is_file()))
        .unwrap_or(false)
}

fn default_command() -> Vec<String> {
    if let Ok(sh) = std::env::var("PWSH_TMUX_SHELL") {
        if !sh.is_empty() {
            return vec![sh];
        }
    }
    #[cfg(windows)]
    {
        let prog = if which("pwsh.exe") { "pwsh.exe" } else { "powershell.exe" };
        vec![prog.into(), "-NoLogo".into()]
    }
    #[cfg(unix)]
    {
        if which("pwsh") {
            return vec!["pwsh".into(), "-NoLogo".into()];
        }
        vec![std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())]
    }
}

fn home_dir() -> Option<String> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(var).ok().filter(|s| !s.is_empty())
}

fn program_name(cmd: &[String]) -> String {
    let prog = cmd.first().map(String::as_str).unwrap_or("shell");
    std::path::Path::new(prog)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| prog.to_string())
}

fn parse_key(d: &[u8]) -> (Key, usize) {
    let b = d[0];
    if b == 0x1b {
        if d.len() >= 3 && (d[1] == b'[' || d[1] == b'O') {
            let mut j = 2;
            while j < d.len() && !(0x40..=0x7e).contains(&d[j]) {
                j += 1;
            }
            if j >= d.len() {
                return (Key::Other, d.len());
            }
            let modifier = std::str::from_utf8(&d[2..j])
                .ok()
                .and_then(|s| s.split(';').nth(1))
                .and_then(|m| m.parse().ok())
                .unwrap_or(1);
            let dir = match d[j] {
                b'A' => Some(Dir::Up),
                b'B' => Some(Dir::Down),
                b'C' => Some(Dir::Right),
                b'D' => Some(Dir::Left),
                _ => None,
            };
            return (dir.map_or(Key::Other, |dir| Key::Arrow(dir, modifier)), j + 1);
        }
        if d.len() >= 2 && d[1] == 0x1b {
            // Alt+arrow as ESC ESC [ A
            let (k, n) = parse_key(&d[1..]);
            return match k {
                Key::Arrow(dir, 1) => (Key::Arrow(dir, 3), n + 1),
                _ => (Key::Other, n + 1),
            };
        }
        if d.len() >= 2 {
            return (Key::Other, 2); // Alt+key
        }
        return (Key::Esc, 1);
    }
    if b < 0x20 || b == 0x7f {
        return (Key::Ctrl(b), 1);
    }
    let len = match b {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
    .min(d.len());
    match std::str::from_utf8(&d[..len]).ok().and_then(|s| s.chars().next()) {
        Some(c) => (Key::Char(c), len),
        None => (Key::Other, len),
    }
}

fn key_bytes(k: &str) -> Option<Vec<u8>> {
    let s: &str = match k {
        "Enter" => "\r",
        "Tab" => "\t",
        "Space" => " ",
        "Escape" | "Esc" => "\x1b",
        "BSpace" => "\x7f",
        "Up" => "\x1b[A",
        "Down" => "\x1b[B",
        "Right" => "\x1b[C",
        "Left" => "\x1b[D",
        "Home" => "\x1b[H",
        "End" => "\x1b[F",
        "PageUp" | "PPage" => "\x1b[5~",
        "PageDown" | "NPage" => "\x1b[6~",
        "Delete" | "DC" => "\x1b[3~",
        _ => {
            let mut it = k.chars();
            return match (it.next(), it.next(), it.next(), it.next()) {
                (Some('C'), Some('-'), Some(c), None) => {
                    let c = c.to_ascii_lowercase();
                    match c {
                        'a'..='z' => Some(vec![c as u8 - b'a' + 1]),
                        '@' | ' ' => Some(vec![0]),
                        '[' => Some(vec![0x1b]),
                        '\\' => Some(vec![0x1c]),
                        ']' => Some(vec![0x1d]),
                        _ => None,
                    }
                }
                (Some('M'), Some('-'), Some(c), None) => {
                    let mut v = vec![0x1b];
                    v.extend(c.to_string().bytes());
                    Some(v)
                }
                _ => None,
            };
        }
    };
    Some(s.as_bytes().to_vec())
}

fn push_color(out: &mut String, c: vt100::Color, base: u8) {
    match c {
        vt100::Color::Default => {}
        vt100::Color::Idx(i) if i < 8 => {
            let _ = write!(out, ";{}", base + i);
        }
        vt100::Color::Idx(i) if i < 16 => {
            let _ = write!(out, ";{}", base + 60 + i - 8);
        }
        vt100::Color::Idx(i) => {
            let _ = write!(out, ";{};5;{}", base + 8, i);
        }
        vt100::Color::Rgb(r, g, b) => {
            let _ = write!(out, ";{};2;{};{};{}", base + 8, r, g, b);
        }
    }
}

fn push_sgr(out: &mut String, st: &Style) {
    out.push_str("\x1b[0");
    for (bit, code) in [(BOLD, ";1"), (DIM, ";2"), (ITALIC, ";3"), (UNDERLINE, ";4"), (INVERSE, ";7")] {
        if st.attrs & bit != 0 {
            out.push_str(code);
        }
    }
    push_color(out, st.fg, 30);
    push_color(out, st.bg, 40);
    out.push('m');
}

fn cell_style(c: &vt100::Cell) -> Style {
    let mut attrs = 0;
    for (on, bit) in [(c.bold(), BOLD), (c.dim(), DIM), (c.italic(), ITALIC), (c.underline(), UNDERLINE), (c.inverse(), INVERSE)] {
        if on {
            attrs |= bit;
        }
    }
    Style { fg: c.fgcolor(), bg: c.bgcolor(), attrs }
}

impl Server {
    fn id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Conn(stream) => self.accept(stream),
            Event::ClientMsg(cid, kind, payload) => self.client_msg(cid, kind, payload),
            Event::ClientGone(cid) => {
                if let Some(c) = self.clients.remove(&cid) {
                    if let Some(si) = c.session.and_then(|id| self.session_index(id)) {
                        self.update_session_size(si);
                    }
                }
            }
            Event::PaneData(id, data) => {
                let Some(p) = self.panes.get_mut(&id) else { return };
                p.parser.process(&data);
                let replies = std::mem::take(&mut p.parser.callbacks_mut().0);
                p.send(replies);
                if let Some((si, _)) = self.locate(id) {
                    self.mark_dirty(si);
                }
            }
            Event::PaneExit(id) => self.remove_pane(id),
        }
    }

    fn accept(&mut self, stream: Stream) {
        let cid = self.id();
        let stream = Arc::new(stream);
        let recv = stream.clone();
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            let mut send = &*stream;
            for buf in rx {
                if send.write_all(&buf).and_then(|_| send.flush()).is_err() {
                    break;
                }
            }
        });
        let etx = self.tx.clone();
        thread::spawn(move || {
            while let Ok((kind, payload)) = read_frame(&mut &*recv) {
                if etx.send(Event::ClientMsg(cid, kind, payload)).is_err() {
                    return;
                }
            }
            let _ = etx.send(Event::ClientGone(cid));
        });
        self.clients.insert(cid, Client { tx, session: None, size: (80, 24), prefix: false, prev: None, dirty: false });
    }

    fn client_msg(&mut self, cid: ClientId, kind: u8, payload: Vec<u8>) {
        match kind {
            C_COMMAND => {
                let mut args = decode_args(&payload);
                let ctx = if args.is_empty() { String::new() } else { args.remove(0) };
                let ctx = ctx.strip_prefix('%').and_then(|s| s.parse().ok()).filter(|p| self.panes.contains_key(p));
                let reply = match self.command(args, ctx) {
                    Ok(text) => encode_reply(0, &text),
                    Err(text) => encode_reply(1, &text),
                };
                if let Some(c) = self.clients.get(&cid) {
                    c.send(S_REPLY, &reply);
                }
            }
            C_ATTACH => {
                let Some(size) = decode_size(&payload) else { return };
                let target = String::from_utf8_lossy(&payload[4..]).into_owned();
                let t = if target.is_empty() { None } else { Some(target.as_str()) };
                match self.resolve(t, None) {
                    Ok(t) => {
                        let now = self.tick();
                        let s = &mut self.sessions[t.si];
                        s.last_used = now;
                        if t.wi != s.active {
                            s.last = Some(s.active);
                            s.active = t.wi;
                        }
                        let sid = s.id;
                        if let Some(c) = self.clients.get_mut(&cid) {
                            c.session = Some(sid);
                            c.size = size;
                            c.prev = None;
                            c.dirty = true;
                        }
                        self.update_session_size(t.si);
                    }
                    Err(e) => {
                        if let Some(c) = self.clients.get(&cid) {
                            c.send(S_REPLY, &encode_reply(1, &e));
                        }
                    }
                }
            }
            C_INPUT => self.handle_input(cid, &payload),
            C_RESIZE => {
                let Some(size) = decode_size(&payload) else { return };
                let Some(c) = self.clients.get_mut(&cid) else { return };
                c.size = size;
                c.prev = None;
                c.dirty = true;
                if let Some(si) = c.session.and_then(|id| self.session_index(id)) {
                    self.update_session_size(si);
                }
            }
            _ => {}
        }
    }

    // ---- lookup helpers -------------------------------------------------

    fn session_index(&self, id: u32) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == id)
    }

    fn locate(&self, pane: PaneId) -> Option<(usize, usize)> {
        for (si, s) in self.sessions.iter().enumerate() {
            for (wi, w) in s.windows.iter().enumerate() {
                if w.layout.panes().contains(&pane) {
                    return Some((si, wi));
                }
            }
        }
        None
    }

    fn client_target(&self, cid: ClientId) -> Option<Target> {
        let si = self.session_index(self.clients.get(&cid)?.session?)?;
        let s = &self.sessions[si];
        Some(Target { si, wi: s.active, pane: s.windows[s.active].active })
    }

    fn current_session(&self) -> Option<usize> {
        self.sessions.iter().enumerate().max_by_key(|(_, s)| s.last_used).map(|(i, _)| i)
    }

    /// Target syntax: `%pane`, `session`, `session:window`, `session:window.pane`
    /// (empty parts mean "current").
    fn resolve(&self, t: Option<&str>, ctx: Option<PaneId>) -> Result<Target, String> {
        if self.sessions.is_empty() {
            return Err("no sessions".into());
        }
        let ctx_loc = ctx.and_then(|p| self.locate(p).map(|(si, wi)| Target { si, wi, pane: p }));
        let Some(t) = t else {
            if let Some(loc) = ctx_loc {
                return Ok(loc);
            }
            let si = self.current_session().ok_or("no sessions")?;
            let s = &self.sessions[si];
            return Ok(Target { si, wi: s.active, pane: s.windows[s.active].active });
        };
        if let Some(id) = t.strip_prefix('%') {
            let pane: PaneId = id.parse().map_err(|_| format!("bad pane id: {t}"))?;
            let (si, wi) = self.locate(pane).ok_or_else(|| format!("can't find pane: {t}"))?;
            return Ok(Target { si, wi, pane });
        }
        let (sess, rest) = match t.split_once(':') {
            Some((s, r)) => (s, Some(r)),
            None => (t, None),
        };
        let si = if sess.is_empty() {
            match ctx_loc {
                Some(l) => l.si,
                None => self.current_session().ok_or("no sessions")?,
            }
        } else {
            match self.sessions.iter().position(|s| s.name == sess) {
                Some(i) => i,
                None => {
                    let m: Vec<usize> = (0..self.sessions.len()).filter(|&i| self.sessions[i].name.starts_with(sess)).collect();
                    match m.as_slice() {
                        [i] => *i,
                        [] => return Err(format!("can't find session: {sess}")),
                        _ => return Err(format!("ambiguous session: {sess}")),
                    }
                }
            }
        };
        let s = &self.sessions[si];
        let (win, pane) = match rest.map(|r| r.split_once('.').map_or((r, None), |(w, p)| (w, Some(p)))) {
            Some((w, p)) => (Some(w).filter(|w| !w.is_empty()), p.filter(|p| !p.is_empty())),
            None => (None, None),
        };
        let wi = match win {
            Some(w) => w
                .parse::<usize>()
                .ok()
                .filter(|&i| i < s.windows.len())
                .or_else(|| s.windows.iter().position(|x| x.name == w))
                .ok_or_else(|| format!("can't find window: {w}"))?,
            None => match ctx_loc {
                Some(l) if l.si == si => l.wi,
                _ => s.active,
            },
        };
        let w = &s.windows[wi];
        let pane = match pane {
            Some(p) => p
                .parse::<usize>()
                .ok()
                .and_then(|i| w.layout.panes().get(i).copied())
                .ok_or_else(|| format!("can't find pane: {p}"))?,
            None => match ctx_loc {
                Some(l) if l.si == si && l.wi == wi => l.pane,
                _ => w.active,
            },
        };
        Ok(Target { si, wi, pane })
    }

    // ---- pane / window / session lifecycle -----------------------------

    fn spawn_pane(&mut self, cols: u16, rows: u16, cmd: &[String], cwd: Option<&str>) -> Result<PaneId, String> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let pair = self
            .pty
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| format!("openpty: {e}"))?;
        let default;
        let cmd = if cmd.is_empty() {
            default = default_command();
            &default[..]
        } else {
            cmd
        };
        let mut cb = CommandBuilder::new(&cmd[0]);
        cb.args(&cmd[1..]);
        if let Some(dir) = cwd.filter(|d| std::path::Path::new(d).is_dir()).map(String::from).or_else(home_dir) {
            cb.cwd(dir);
        }
        let id = self.next_pane;
        self.next_pane += 1;
        cb.env("PWSH_TMUX", ipc::describe());
        cb.env("PWSH_TMUX_PANE", format!("%{id}"));
        if cfg!(unix) {
            cb.env("TERM", "xterm-256color");
            cb.env("COLORTERM", "truecolor");
        }
        let mut child = pair.slave.spawn_command(cb).map_err(|e| format!("spawn {}: {e}", cmd[0]))?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let killer = child.clone_killer();

        let tx = self.tx.clone();
        thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(Event::PaneData(id, buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = child.wait();
            let _ = tx.send(Event::PaneExit(id));
        });
        // Writes go through a thread so a child that stops reading can't stall the server.
        let (itx, irx) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            for data in irx {
                if writer.write_all(&data).and_then(|_| writer.flush()).is_err() {
                    break;
                }
            }
        });
        self.panes.insert(
            id,
            Pane {
                parser: vt100::Parser::new_with_callbacks(nz(rows), nz(cols), SCROLLBACK, Replies::default()),
                input: itx,
                master: pair.master,
                killer,
                size: (cols, rows),
            },
        );
        Ok(id)
    }

    fn kill_pane_process(&mut self, id: PaneId) {
        if let Some(mut p) = self.panes.remove(&id) {
            let _ = p.killer.kill();
        }
    }

    fn create_session(&mut self, name: Option<String>, cols: u16, rows: u16, cwd: Option<String>, win_name: Option<String>, cmd: &[String]) -> Result<String, String> {
        let name = match name {
            Some(n) if n.is_empty() || n.contains([':', '.']) => return Err(format!("bad session name: {n}")),
            Some(n) if self.sessions.iter().any(|s| s.name == n) => return Err(format!("duplicate session: {n}")),
            Some(n) => n,
            None => (0..).map(|i: u32| i.to_string()).find(|n| !self.sessions.iter().any(|s| &s.name == n)).unwrap(),
        };
        let (cols, rows) = (cols.max(2), rows.max(2));
        let pane = self.spawn_pane(cols, rows - 1, cmd, cwd.as_deref())?;
        let id = self.id();
        let now = self.tick();
        let wname = win_name.unwrap_or_else(|| window_name(cmd));
        self.sessions.push(Session {
            id,
            name: name.clone(),
            windows: vec![Window { name: wname, layout: Node::Leaf(pane), active: pane, last_active: None, zoomed: false }],
            active: 0,
            last: None,
            cols,
            rows,
            cwd,
            last_used: now,
        });
        self.had_session = true;
        Ok(name)
    }

    fn new_window(&mut self, si: usize, name: Option<String>, cwd: Option<String>, cmd: &[String]) -> Result<(), String> {
        let s = &self.sessions[si];
        let area = s.area();
        let cwd = cwd.or_else(|| s.cwd.clone());
        let pane = self.spawn_pane(area.w, area.h, cmd, cwd.as_deref())?;
        let wname = name.unwrap_or_else(|| window_name(cmd));
        let s = &mut self.sessions[si];
        s.windows.push(Window { name: wname, layout: Node::Leaf(pane), active: pane, last_active: None, zoomed: false });
        s.last = Some(s.active);
        s.active = s.windows.len() - 1;
        self.mark_dirty(si);
        Ok(())
    }

    fn split(&mut self, t: Target, horizontal: bool, cwd: Option<String>, cmd: &[String]) -> Result<(), String> {
        let (rects, _) = self.rects(t.si, t.wi);
        let r = rects.iter().find(|(p, _)| *p == t.pane).map(|(_, r)| *r).ok_or("pane not visible")?;
        if (horizontal && r.w < 3) || (!horizontal && r.h < 3) {
            return Err("pane too small".into());
        }
        let (w, h) = if horizontal { ((r.w - 1) / 2, r.h) } else { (r.w, (r.h - 1) / 2) };
        let cwd = cwd.or_else(|| self.sessions[t.si].cwd.clone());
        let new = self.spawn_pane(w, h, cmd, cwd.as_deref())?;
        let win = &mut self.sessions[t.si].windows[t.wi];
        win.layout.split(t.pane, new, horizontal);
        win.zoomed = false;
        win.last_active = Some(win.active);
        win.active = new;
        self.apply_layout(t.si, t.wi);
        self.mark_dirty(t.si);
        Ok(())
    }

    fn remove_pane(&mut self, id: PaneId) {
        self.kill_pane_process(id);
        let Some((si, wi)) = self.locate(id) else { return };
        let win = &mut self.sessions[si].windows[wi];
        // Placeholder id never belongs to a live pane.
        let layout = std::mem::replace(&mut win.layout, Node::Leaf(PaneId::MAX));
        match layout.remove(id) {
            Some(n) => {
                win.layout = n;
                let panes = win.layout.panes();
                win.last_active = win.last_active.filter(|p| panes.contains(p));
                if win.active == id {
                    win.active = win.last_active.take().unwrap_or(panes[0]);
                    win.zoomed = false;
                }
                self.apply_layout(si, wi);
                self.mark_dirty(si);
            }
            None => self.remove_window(si, wi),
        }
    }

    fn remove_window(&mut self, si: usize, wi: usize) {
        let w = self.sessions[si].windows.remove(wi);
        for p in w.layout.panes() {
            self.kill_pane_process(p);
        }
        let s = &mut self.sessions[si];
        if s.windows.is_empty() {
            self.remove_session(si, "exited");
            return;
        }
        let fix = |i: usize| if i > wi { i - 1 } else { i };
        s.last = s.last.filter(|&l| l != wi).map(fix);
        s.active = if s.active == wi { s.last.take().unwrap_or(wi.saturating_sub(1)) } else { fix(s.active) };
        s.active = s.active.min(s.windows.len() - 1);
        if s.last == Some(s.active) {
            s.last = None;
        }
        self.mark_dirty(si);
    }

    fn remove_session(&mut self, si: usize, why: &str) {
        let s = self.sessions.remove(si);
        for w in &s.windows {
            for p in w.layout.panes() {
                self.kill_pane_process(p);
            }
        }
        for c in self.clients.values_mut() {
            if c.session == Some(s.id) {
                c.session = None;
                c.send(S_EXIT, format!("[{why}]").as_bytes());
            }
        }
    }

    fn detach_session_clients(&mut self, si: usize) {
        let s = &self.sessions[si];
        for c in self.clients.values_mut() {
            if c.session == Some(s.id) {
                c.session = None;
                c.send(S_EXIT, format!("[detached (from session {})]", s.name).as_bytes());
            }
        }
    }

    fn shutdown(&mut self) {
        let ids: Vec<PaneId> = self.panes.keys().copied().collect();
        for id in ids {
            self.kill_pane_process(id);
        }
        for c in self.clients.values() {
            if c.session.is_some() {
                c.send(S_EXIT, b"[server exited]");
            }
        }
        // Let writer threads flush the final messages.
        thread::sleep(Duration::from_millis(150));
    }

    // ---- layout ----------------------------------------------------------

    fn rects(&self, si: usize, wi: usize) -> (Vec<(PaneId, Rect)>, Vec<Sep>) {
        let s = &self.sessions[si];
        let w = &s.windows[wi];
        if w.zoomed {
            return (vec![(w.active, s.area())], vec![]);
        }
        let (mut panes, mut seps) = (vec![], vec![]);
        w.layout.layout(s.area(), &mut panes, &mut seps);
        (panes, seps)
    }

    fn apply_layout(&mut self, si: usize, wi: usize) {
        let (rects, _) = self.rects(si, wi);
        for (id, r) in rects {
            if let Some(p) = self.panes.get_mut(&id) {
                p.resize(r.w, r.h);
            }
        }
    }

    /// Session size follows the smallest attached client.
    fn update_session_size(&mut self, si: usize) {
        let sid = self.sessions[si].id;
        let sizes = self.clients.values().filter(|c| c.session == Some(sid)).map(|c| c.size);
        let Some(size) = sizes.reduce(|a, b| (a.0.min(b.0), a.1.min(b.1))) else { return };
        let size = (size.0.max(2), size.1.max(2));
        let s = &mut self.sessions[si];
        if (s.cols, s.rows) != size {
            (s.cols, s.rows) = size;
            for wi in 0..s.windows.len() {
                self.apply_layout(si, wi);
            }
        }
        self.mark_dirty(si);
    }

    fn mark_dirty(&mut self, si: usize) {
        let sid = self.sessions[si].id;
        for c in self.clients.values_mut() {
            if c.session == Some(sid) {
                c.dirty = true;
            }
        }
    }

    fn select_window(&mut self, si: usize, wi: usize) {
        let s = &mut self.sessions[si];
        if wi < s.windows.len() && wi != s.active {
            s.last = Some(s.active);
            s.active = wi;
            self.mark_dirty(si);
        }
    }

    fn select_pane(&mut self, t: Target) {
        let s = &mut self.sessions[t.si];
        if s.active != t.wi {
            s.last = Some(s.active);
            s.active = t.wi;
        }
        let w = &mut s.windows[t.wi];
        if w.active != t.pane {
            w.last_active = Some(w.active);
            w.active = t.pane;
            if w.zoomed {
                w.zoomed = false;
                self.apply_layout(t.si, t.wi);
            }
        }
        self.mark_dirty(t.si);
    }

    fn select_dir(&mut self, t: Target, dir: Dir) {
        let (rects, _) = self.rects(t.si, t.wi);
        let Some(r) = rects.iter().find(|(p, _)| *p == t.pane).map(|(_, r)| *r) else { return };
        let (cy, cx) = self.panes.get(&t.pane).map(|p| p.parser.screen().cursor_position()).unwrap_or((0, 0));
        if let Some(n) = neighbour(&rects, t.pane, dir, (r.x + cx, r.y + cy)) {
            self.select_pane(Target { pane: n, ..t });
        }
    }

    fn resize_pane(&mut self, t: Target, dir: Dir, cells: u16) {
        let area = self.sessions[t.si].area();
        let w = &mut self.sessions[t.si].windows[t.wi];
        if w.layout.resize(t.pane, dir, cells, area) {
            self.apply_layout(t.si, t.wi);
            self.mark_dirty(t.si);
        }
    }

    fn toggle_zoom(&mut self, t: Target) {
        let w = &mut self.sessions[t.si].windows[t.wi];
        if w.layout.panes().len() > 1 {
            w.zoomed = !w.zoomed;
            self.apply_layout(t.si, t.wi);
            self.mark_dirty(t.si);
        }
    }

    // ---- input -------------------------------------------------------------

    fn handle_input(&mut self, cid: ClientId, data: &[u8]) {
        let mut pass = Vec::new();
        let mut i = 0;
        while i < data.len() {
            let Some(c) = self.clients.get_mut(&cid) else { return };
            if c.prefix {
                c.prefix = false;
                self.send_to_active(cid, std::mem::take(&mut pass));
                let (key, n) = parse_key(&data[i..]);
                i += n;
                self.prefix_command(cid, key);
            } else if data[i] == PREFIX {
                c.prefix = true;
                i += 1;
            } else {
                pass.push(data[i]);
                i += 1;
            }
        }
        self.send_to_active(cid, pass);
    }

    fn send_to_active(&self, cid: ClientId, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        if let Some(t) = self.client_target(cid) {
            if let Some(p) = self.panes.get(&t.pane) {
                p.send(data);
            }
        }
    }

    fn prefix_command(&mut self, cid: ClientId, key: Key) {
        let Some(t) = self.client_target(cid) else { return };
        let result = match key {
            Key::Char('"') => self.split(t, false, None, &[]),
            Key::Char('%') => self.split(t, true, None, &[]),
            Key::Char('c') => self.new_window(t.si, None, None, &[]),
            Key::Char('n') => {
                let n = self.sessions[t.si].windows.len();
                self.select_window(t.si, (t.wi + 1) % n);
                Ok(())
            }
            Key::Char('p') => {
                let n = self.sessions[t.si].windows.len();
                self.select_window(t.si, (t.wi + n - 1) % n);
                Ok(())
            }
            Key::Char('l') => {
                if let Some(l) = self.sessions[t.si].last {
                    self.select_window(t.si, l);
                }
                Ok(())
            }
            Key::Char(c @ '0'..='9') => {
                self.select_window(t.si, c as usize - '0' as usize);
                Ok(())
            }
            Key::Char('o') => {
                let panes = self.sessions[t.si].windows[t.wi].layout.panes();
                let i = panes.iter().position(|&p| p == t.pane).unwrap_or(0);
                self.select_pane(Target { pane: panes[(i + 1) % panes.len()], ..t });
                Ok(())
            }
            Key::Char(';') => {
                if let Some(p) = self.sessions[t.si].windows[t.wi].last_active {
                    self.select_pane(Target { pane: p, ..t });
                }
                Ok(())
            }
            Key::Char('x') => {
                self.remove_pane(t.pane);
                Ok(())
            }
            Key::Char('&') => {
                self.remove_window(t.si, t.wi);
                Ok(())
            }
            Key::Char('d') => {
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.session = None;
                    c.send(S_EXIT, format!("[detached (from session {})]", self.sessions[t.si].name).as_bytes());
                }
                self.update_session_size(t.si);
                Ok(())
            }
            Key::Char('z') => {
                self.toggle_zoom(t);
                Ok(())
            }
            Key::Arrow(dir, 1) => {
                self.select_dir(t, dir);
                Ok(())
            }
            Key::Arrow(dir, 5) => {
                self.resize_pane(t, dir, 1);
                Ok(())
            }
            Key::Arrow(dir, 3) => {
                self.resize_pane(t, dir, 5);
                Ok(())
            }
            Key::Ctrl(PREFIX) => {
                if let Some(p) = self.panes.get(&t.pane) {
                    p.send(vec![PREFIX]);
                }
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(e) = result {
            log(&format!("key {key:?}: {e}"));
        }
    }

    // ---- commands ----------------------------------------------------------

    fn command(&mut self, args: Vec<String>, ctx: Option<PaneId>) -> Result<String, String> {
        let Some((cmd, args)) = args.split_first() else { return Err("no command".into()) };
        let cmd = cmd.as_str();
        match cmd {
            "new-session" | "new" => {
                let o = getopt(args, "dAD", "sxycnt")?;
                let num = |c: char, d: u16| o.get(c).and_then(|v| v.parse().ok()).unwrap_or(d);
                self.create_session(
                    o.get('s').map(String::from),
                    num('x', 80),
                    num('y', 24),
                    o.get('c').map(String::from),
                    o.get('n').map(String::from),
                    &o.rest,
                )
            }
            "list-sessions" | "ls" => {
                let mut out = String::new();
                for s in &self.sessions {
                    let attached = self.clients.values().filter(|c| c.session == Some(s.id)).count();
                    let _ = write!(out, "{}: {} windows [{}x{}]", s.name, s.windows.len(), s.cols, s.rows);
                    if attached > 0 {
                        out.push_str(" (attached)");
                    }
                    out.push('\n');
                }
                Ok(out)
            }
            "has-session" | "has" => {
                let o = getopt(args, "", "t")?;
                self.resolve(o.get('t'), ctx).map(|_| String::new())
            }
            "kill-session" => {
                let o = getopt(args, "", "t")?;
                let t = self.resolve(o.get('t'), ctx)?;
                self.remove_session(t.si, "exited");
                Ok(String::new())
            }
            "kill-server" => {
                self.quit = true;
                Ok(String::new())
            }
            "list-windows" | "lsw" => {
                let o = getopt(args, "", "t")?;
                let t = self.resolve(o.get('t'), ctx)?;
                let s = &self.sessions[t.si];
                let mut out = String::new();
                for (i, w) in s.windows.iter().enumerate() {
                    let flag = if i == s.active { "*" } else if Some(i) == s.last { "-" } else { "" };
                    let _ = writeln!(out, "{i}: {}{flag} ({} panes) [{}x{}]", w.name, w.layout.panes().len(), s.cols, s.rows.saturating_sub(1));
                }
                Ok(out)
            }
            "list-panes" | "lsp" => {
                let o = getopt(args, "", "t")?;
                let t = self.resolve(o.get('t'), ctx)?;
                let (rects, _) = self.rects(t.si, t.wi);
                let w = &self.sessions[t.si].windows[t.wi];
                let mut out = String::new();
                for (i, p) in w.layout.panes().iter().enumerate() {
                    let size = self.panes.get(p).map(|p| p.size).unwrap_or((0, 0));
                    let pos = rects.iter().find(|(id, _)| id == p).map(|(_, r)| (r.x, r.y));
                    let _ = write!(out, "{i}: [{}x{}] %{p}", size.0, size.1);
                    if let Some((x, y)) = pos {
                        let _ = write!(out, " @{x},{y}");
                    }
                    if *p == w.active {
                        out.push_str(" (active)");
                    }
                    out.push('\n');
                }
                Ok(out)
            }
            "new-window" | "neww" => {
                let o = getopt(args, "d", "tnc")?;
                let t = self.resolve(o.get('t'), ctx)?;
                let prev = self.sessions[t.si].active;
                self.new_window(t.si, o.get('n').map(String::from), o.get('c').map(String::from), &o.rest)?;
                if o.has('d') {
                    let s = &mut self.sessions[t.si];
                    s.last = Some(s.active);
                    s.active = prev;
                }
                Ok(String::new())
            }
            "split-window" | "splitw" => {
                let o = getopt(args, "hvd", "tc")?;
                let t = self.resolve(o.get('t'), ctx)?;
                self.split(t, o.has('h'), o.get('c').map(String::from), &o.rest)?;
                if o.has('d') {
                    let w = &mut self.sessions[t.si].windows[t.wi];
                    w.last_active = Some(w.active);
                    w.active = t.pane;
                }
                Ok(String::new())
            }
            "kill-window" | "killw" => {
                let o = getopt(args, "", "t")?;
                let t = self.resolve(o.get('t'), ctx)?;
                self.remove_window(t.si, t.wi);
                Ok(String::new())
            }
            "kill-pane" | "killp" => {
                let o = getopt(args, "", "t")?;
                let t = self.resolve(o.get('t'), ctx)?;
                self.remove_pane(t.pane);
                Ok(String::new())
            }
            "select-window" | "selectw" => {
                let o = getopt(args, "lnp", "t")?;
                let t = self.resolve(o.get('t'), ctx)?;
                let n = self.sessions[t.si].windows.len();
                let wi = if o.has('n') {
                    (t.wi + 1) % n
                } else if o.has('p') {
                    (t.wi + n - 1) % n
                } else if o.has('l') {
                    self.sessions[t.si].last.ok_or("no last window")?
                } else {
                    t.wi
                };
                self.select_window(t.si, wi);
                Ok(String::new())
            }
            "next-window" | "next" | "previous-window" | "prev" | "last-window" | "last" => {
                let o = getopt(args, "", "t")?;
                let flag = match cmd {
                    "next-window" | "next" => "-n",
                    "previous-window" | "prev" => "-p",
                    _ => "-l",
                };
                let mut a = vec!["select-window".to_string(), flag.to_string()];
                if let Some(t) = o.get('t') {
                    a.extend(["-t".to_string(), t.split(':').next().unwrap_or("").to_string() + ":"]);
                }
                self.command(a, ctx)
            }
            "select-pane" | "selectp" => {
                let o = getopt(args, "LRUDlZ", "t")?;
                let t = self.resolve(o.get('t'), ctx)?;
                let dir = [('L', Dir::Left), ('R', Dir::Right), ('U', Dir::Up), ('D', Dir::Down)]
                    .into_iter()
                    .find(|(c, _)| o.has(*c))
                    .map(|(_, d)| d);
                if let Some(dir) = dir {
                    self.select_dir(t, dir);
                } else if o.has('l') {
                    let p = self.sessions[t.si].windows[t.wi].last_active.ok_or("no last pane")?;
                    self.select_pane(Target { pane: p, ..t });
                } else {
                    self.select_pane(t);
                }
                Ok(String::new())
            }
            "resize-pane" | "resizep" => {
                let o = getopt(args, "LRUDZ", "t")?;
                let t = self.resolve(o.get('t'), ctx)?;
                if o.has('Z') {
                    self.toggle_zoom(t);
                    return Ok(String::new());
                }
                let n = o.rest.first().and_then(|v| v.parse().ok()).unwrap_or(1);
                let dir = [('L', Dir::Left), ('R', Dir::Right), ('U', Dir::Up), ('D', Dir::Down)]
                    .into_iter()
                    .find(|(c, _)| o.has(*c))
                    .map(|(_, d)| d)
                    .ok_or("resize-pane needs -L, -R, -U, -D or -Z")?;
                self.resize_pane(t, dir, n);
                Ok(String::new())
            }
            "rename-window" | "renamew" => {
                let o = getopt(args, "", "t")?;
                let t = self.resolve(o.get('t'), ctx)?;
                let name = o.rest.first().ok_or("rename-window needs a name")?;
                self.sessions[t.si].windows[t.wi].name = name.clone();
                self.mark_dirty(t.si);
                Ok(String::new())
            }
            "rename-session" | "rename" => {
                let o = getopt(args, "", "t")?;
                let t = self.resolve(o.get('t'), ctx)?;
                let name = o.rest.first().ok_or("rename-session needs a name")?;
                if name.is_empty() || name.contains([':', '.']) {
                    return Err(format!("bad session name: {name}"));
                }
                if self.sessions.iter().enumerate().any(|(i, s)| i != t.si && &s.name == name) {
                    return Err(format!("duplicate session: {name}"));
                }
                self.sessions[t.si].name = name.clone();
                self.mark_dirty(t.si);
                Ok(String::new())
            }
            "send-keys" | "send" => {
                let o = getopt(args, "l", "t")?;
                let t = self.resolve(o.get('t'), ctx)?;
                let mut data = Vec::new();
                for k in &o.rest {
                    match key_bytes(k).filter(|_| !o.has('l')) {
                        Some(b) => data.extend(b),
                        None => data.extend(k.as_bytes()),
                    }
                }
                if let Some(p) = self.panes.get(&t.pane) {
                    p.send(data);
                }
                Ok(String::new())
            }
            "capture-pane" | "capturep" => {
                let o = getopt(args, "pJ", "t")?;
                let t = self.resolve(o.get('t'), ctx)?;
                let p = self.panes.get(&t.pane).ok_or("pane has no terminal")?;
                let mut text = p.parser.screen().contents();
                text.push('\n');
                Ok(text)
            }
            "detach-client" | "detach" => {
                let o = getopt(args, "a", "st")?;
                let t = self.resolve(o.get('s').or(o.get('t')), ctx)?;
                self.detach_session_clients(t.si);
                self.update_session_size(t.si);
                Ok(String::new())
            }
            "display-message" | "display" => {
                let o = getopt(args, "p", "t")?;
                let t = self.resolve(o.get('t'), ctx)?;
                let fmt = o.rest.join(" ");
                Ok(self.format(t, &fmt) + "\n")
            }
            _ => Err(format!("unknown command: {cmd}")),
        }
    }

    fn format(&self, t: Target, fmt: &str) -> String {
        let s = &self.sessions[t.si];
        let w = &s.windows[t.wi];
        let pane_index = w.layout.panes().iter().position(|&p| p == t.pane).unwrap_or(0);
        let size = self.panes.get(&t.pane).map(|p| p.size).unwrap_or((0, 0));
        [
            ("#{session_name}", s.name.clone()),
            ("#{window_index}", t.wi.to_string()),
            ("#{window_name}", w.name.clone()),
            ("#{window_panes}", w.layout.panes().len().to_string()),
            ("#{window_zoomed_flag}", (w.zoomed as u8).to_string()),
            ("#{pane_index}", pane_index.to_string()),
            ("#{pane_id}", format!("%{}", t.pane)),
            ("#{pane_width}", size.0.to_string()),
            ("#{pane_height}", size.1.to_string()),
            ("#{session_windows}", s.windows.len().to_string()),
        ]
        .iter()
        .fold(fmt.to_string(), |acc, (k, v)| acc.replace(k, v))
    }

    // ---- rendering ---------------------------------------------------------

    fn render_all(&mut self) {
        let ids: Vec<ClientId> = self.clients.iter().filter(|(_, c)| c.dirty && c.session.is_some()).map(|(id, _)| *id).collect();
        for id in ids {
            self.render(id);
        }
    }

    fn compose(&self, si: usize, w: u16, h: u16) -> RenderState {
        let mut f = Frame::new(w, h);
        let s = &self.sessions[si];
        let win = &s.windows[s.active];
        let (rects, seps) = self.rects(si, s.active);
        let area = s.area();
        let mut cursor = None;
        let mut modes = (false, false);
        let active_rect = rects.iter().find(|(p, _)| *p == win.active).map(|(_, r)| *r);

        for (id, r) in &rects {
            let Some(p) = self.panes.get(id) else { continue };
            let scr = p.parser.screen();
            for y in 0..r.h {
                for x in 0..r.w {
                    let (fx, fy) = (r.x + x, r.y + y);
                    let Some(c) = scr.cell(y, x) else { continue };
                    let style = cell_style(c);
                    let text = if c.is_wide_continuation() {
                        String::new()
                    } else if c.is_wide() && (x + 1 >= r.w || fx + 1 >= w) {
                        " ".into()
                    } else if c.has_contents() {
                        c.contents().to_string()
                    } else {
                        " ".into()
                    };
                    f.set(fx, fy, Cell { text, style });
                }
            }
            if *id == win.active {
                modes = (scr.application_cursor(), scr.bracketed_paste());
                if !scr.hide_cursor() {
                    let (cy, cx) = scr.cursor_position();
                    if cx < r.w && cy < r.h {
                        cursor = Some((r.x + cx, r.y + cy));
                    }
                }
            }
        }

        let green = Style { fg: vt100::Color::Idx(2), ..Style::default() };
        for sep in &seps {
            for k in 0..sep.len {
                let (x, y) = if sep.vertical { (sep.x, sep.y + k) } else { (sep.x + k, sep.y) };
                let near = active_rect.is_some_and(|a| x + 1 >= a.x && x <= a.right() && y + 1 >= a.y && y <= a.bottom());
                let text = if sep.vertical { "│" } else { "─" };
                f.set(x, y, Cell { text: text.into(), style: if near { green } else { Style::default() } });
            }
        }

        // status line
        let sy = area.h.min(h.saturating_sub(1));
        let bar = Style { fg: vt100::Color::Idx(0), bg: vt100::Color::Idx(2), attrs: 0 };
        let width = s.cols.min(w);
        for x in 0..width {
            f.set(x, sy, Cell { text: " ".into(), style: bar });
        }
        let right = format!(" \"{}\" ", self.host);
        let right_w: u16 = right.chars().map(|c| c.width().unwrap_or(0) as u16).sum();
        let left_limit = width.saturating_sub(right_w);
        let mut x = f.text(0, sy, &format!("[{}] ", s.name), bar, left_limit);
        for (i, wnd) in s.windows.iter().enumerate() {
            let flag = if i == s.active {
                if wnd.zoomed { "*Z" } else { "*" }
            } else if Some(i) == s.last {
                "-"
            } else {
                ""
            };
            x = f.text(x, sy, &format!("{i}:{}{flag} ", wnd.name), bar, left_limit);
        }
        if right_w <= width.saturating_sub(x) {
            f.text(width - right_w, sy, &right, bar, width);
        }

        RenderState { frame: f, cursor, modes }
    }

    fn render(&mut self, cid: ClientId) {
        let Some(c) = self.clients.get(&cid) else { return };
        let Some(si) = c.session.and_then(|id| self.session_index(id)) else { return };
        let (w, h) = c.size;
        let next = self.compose(si, w.max(1), h.max(1));
        let c = self.clients.get_mut(&cid).unwrap();
        c.dirty = false;

        let prev = c.prev.as_ref().filter(|p| p.frame.w == next.frame.w && p.frame.h == next.frame.h);
        let mut out = String::new();
        if prev.is_none() {
            out.push_str("\x1b[0m\x1b[H\x1b[2J");
        }
        let mut changed = false;
        for y in 0..next.frame.h {
            let row = next.frame.row(y);
            if prev.is_some_and(|p| p.frame.row(y) == row) {
                continue;
            }
            changed = true;
            let _ = write!(out, "\x1b[{};1H", y + 1);
            let mut cur: Option<Style> = None;
            for cell in row {
                if cell.text.is_empty() {
                    continue;
                }
                if cur != Some(cell.style) {
                    push_sgr(&mut out, &cell.style);
                    cur = Some(cell.style);
                }
                out.push_str(&cell.text);
            }
        }
        let modes_changed = prev.is_none_or(|p| p.modes != next.modes);
        let cursor_changed = prev.is_none_or(|p| p.cursor != next.cursor);
        if !changed && !modes_changed && !cursor_changed {
            c.prev = Some(next);
            return;
        }
        out.insert_str(0, "\x1b[?25l");
        out.push_str("\x1b[0m");
        if modes_changed {
            out.push_str(if next.modes.0 { "\x1b[?1h" } else { "\x1b[?1l" });
            out.push_str(if next.modes.1 { "\x1b[?2004h" } else { "\x1b[?2004l" });
        }
        if let Some((x, y)) = next.cursor {
            let _ = write!(out, "\x1b[{};{}H\x1b[?25h", y + 1, x + 1);
        }
        c.send(S_OUTPUT, out.as_bytes());
        c.prev = Some(next);
    }
}

fn window_name(cmd: &[String]) -> String {
    if cmd.is_empty() { program_name(&default_command()) } else { program_name(cmd) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(parse_key(b"\x1b[A"), (Key::Arrow(Dir::Up, 1), 3));
        assert_eq!(parse_key(b"\x1bOD"), (Key::Arrow(Dir::Left, 1), 3));
        assert_eq!(parse_key(b"\x1b[1;5Cxyz"), (Key::Arrow(Dir::Right, 5), 6));
        assert_eq!(parse_key(b"\x1b\x1b[B"), (Key::Arrow(Dir::Down, 3), 4));
        assert_eq!(parse_key(b"%rest"), (Key::Char('%'), 1));
        assert_eq!(parse_key("한".as_bytes()), (Key::Char('한'), 3));
        assert_eq!(parse_key(b"\x02"), (Key::Ctrl(2), 1));
        assert_eq!(parse_key(b"\x1b"), (Key::Esc, 1));
        assert_eq!(key_bytes("C-c"), Some(vec![3]));
        assert_eq!(key_bytes("Enter"), Some(b"\r".to_vec()));
        assert_eq!(key_bytes("hello"), None);
    }

    #[test]
    fn replies_to_cursor_query() {
        let mut p = vt100::Parser::new_with_callbacks(nz(5), nz(10), 0, Replies::default());
        p.process(b"ab\x1b[6n");
        assert_eq!(p.callbacks().0, b"\x1b[1;3R");
    }
}
