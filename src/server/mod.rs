//! The server process: sessions, windows, panes (PTY + terminal emulator), clients,
//! key bindings, options, buffers and the command queue.
//!
//! All state lives on the main loop thread; PTY readers, child waiters, shell jobs
//! and client connections run on helper threads that only send `Event`s.

mod cmds;
mod control;
pub mod features;
mod termcap;
mod input;
mod modes;
mod mouse;
pub mod procinfo;
mod render;
mod vars;
mod transport;
mod output;

use crate::cells::Line;
use crate::copymode::CopyMode;
use crate::ipc;
use crate::keys;
use crate::layout::{Node, Rect};
use crate::options::{self, Options, Scope};
use crate::protocol::*;
use interprocess::local_socket::{prelude::*, Stream};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize, PtySystem};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::num::NonZeroU16;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// Largest window/client dimension accepted. tmux allows 10000, but vt100 allocates
/// every cell up front, so a smaller bound keeps a hostile size from exhausting memory.
pub const MAX_SIZE: u16 = 1000;

pub type PaneId = u32;
pub type WinId = u32;
pub type SessId = u32;
pub type ClientId = u32;

pub fn log(msg: &str) {
    if let Ok(dir) = ipc::runtime_dir() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("server.log")) {
            let _ = writeln!(f, "[{}] {msg}", std::process::id());
        }
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub enum Event {
    Conn(Stream),
    ClientMsg(ClientId, u8, Vec<u8>),
    ClientGone(ClientId),
    PaneData(PaneId, u32, Vec<u8>),
    PaneExit(PaneId, u32, Option<i32>),
    Job(u32, i32, String),
    /// A `#(command)` from a format finished: (command, output).
    FormatShell(String, String),
    /// Who is on the other end of a connection (uid / SID), known after its first frame.
    ClientUser(ClientId, Option<String>),
    /// Foreground programs looked up off the main thread.
    PaneInfo(Vec<PaneInfo>),
}

/// What a pane was running when looked up, and which pane state the lookup was for.
pub struct PaneInfo {
    pub id: PaneId,
    pub generation: u32,
    pub pid: Option<u32>,
    /// The pane's cwd_seq at the request: a cwd reported by the pane since then is newer.
    pub cwd_seq: u64,
    pub cmd: Option<String>,
    pub path: Option<String>,
}

/// Answers terminal queries and collects title / bell / cwd reports from a pane.
#[derive(Default)]
pub struct PaneCb {
    pub replies: Vec<u8>,
    pub bell: bool,
    pub title: Option<String>,
    /// ESC k name ST, for allow-rename.
    pub rename: Option<String>,
    pub cwd: Option<String>,
    /// The pane's extended key mode (0, 1 or 2: tmux's MODE_KEYS_EXTENDED and _2), set by
    /// CSI > 4 ; n m and CSI > 4 n, and by extended-keys always at a reset.
    pub ext_keys: u8,
    /// extended-keys as the parser last saw it (0 off, 1 on, 2 always).
    pub ext_option: u8,
    pub clipboard_queries: Vec<(Vec<u8>, bool)>,
    /// OSC 52 selections set by the application (selector, base64 data).
    pub clipboard_sets: Vec<(Vec<u8>, Vec<u8>)>,
    pub passthrough: Vec<Vec<u8>>,
    /// State reported for formats (pane_path, cursor_colour, pane_pb_*, pane_command_*).
    pub osc: OscState,
}

/// What a pane's application reported through OSC sequences that tmux keeps.
#[derive(Default, Clone)]
pub struct OscState {
    /// OSC 7 as sent (tmux's pane_path).
    pub path: String,
    /// OSC 12 cursor colour as tmux prints it; None after OSC 112.
    pub cursor_colour: Option<String>,
    /// OSC 9;4 progress bar: state (0 hidden .. 4 paused) and percent.
    pub pb_state: u8,
    pub pb_progress: u8,
    /// OSC 133: last prompt (A/N), command start (C) and end (D) times, and exit status.
    pub prompt_time: u64,
    pub cmd_start: u64,
    pub cmd_end: u64,
    pub cmd_running: bool,
    pub cmd_status: Option<i32>,
}

impl OscState {
    fn osc(&mut self, params: &[&[u8]]) {
        let Some((first, rest)) = params.split_first() else { return };
        // tmux sees the text after "N;" with its separators
        let arg = rest.iter().map(|p| String::from_utf8_lossy(p)).collect::<Vec<_>>().join(";");
        match *first {
            b"7" => self.path = arg.chars().filter(|c| !c.is_control()).collect(),
            b"9" => self.progress(&arg),
            b"12" if arg != "?" => {
                if let Some(c) = parse_x11_colour(&arg) {
                    self.cursor_colour = Some(c);
                }
            }
            b"112" if arg.is_empty() => self.cursor_colour = None,
            b"133" => self.osc133(&arg),
            _ => {}
        }
    }

    /// OSC 9;4;state[;progress], as tmux's input_osc_9.
    fn progress(&mut self, arg: &str) {
        let Some(rest) = arg.strip_prefix("4;") else { return };
        let mut it = rest.splitn(2, ';');
        let state = it.next().unwrap_or("");
        let Some(state) = state.parse::<u8>().ok().filter(|s| *s <= 4 && state.len() == 1) else { return };
        let progress = match it.next() {
            None | Some("") => None,
            Some(p) => match p.parse::<u8>() {
                Ok(n) if n <= 100 && p.bytes().all(|b| b.is_ascii_digit()) => Some(n),
                _ => return,
            },
        };
        self.pb_state = state;
        // indeterminate keeps the last percentage
        if let Some(n) = progress.filter(|_| state != 3) {
            self.pb_progress = n;
        }
    }

    fn osc133(&mut self, arg: &str) {
        let now = now_secs();
        match arg.as_bytes().first() {
            Some(b'A' | b'N') => self.prompt_time = now,
            Some(b'C') => {
                self.cmd_start = now;
                self.cmd_end = 0;
                self.cmd_running = true;
                self.cmd_status = None;
            }
            Some(b'D') => {
                self.cmd_end = now;
                self.cmd_running = false;
                self.cmd_status = Some(osc133_exit_status(arg));
            }
            _ => {}
        }
    }
}

/// extended-keys as a number: 0 off, 1 on, 2 always.
pub fn ext_keys_option(v: &str) -> u8 {
    match v { "always" => 2, "on" => 1, _ => 0 }
}

/// The exit status in OSC 133 "D;status[;key=value...]": 0 when absent or a key=value, 255
/// when not a number from 0 to 255 (tmux's input_osc_133_exit_status).
fn osc133_exit_status(arg: &str) -> i32 {
    let Some(rest) = arg.get(1..).and_then(|r| r.strip_prefix(';')) else { return 0 };
    let field = rest.split(';').next().unwrap_or("");
    if field.is_empty() || field.contains('=') {
        return 0;
    }
    field.parse::<i32>().ok().filter(|n| (0..=255).contains(n)).unwrap_or(255)
}

/// An OSC colour (rgb:RR/GG/BB, #RRGGBB, R,G,B or a name) as tmux's colour_tostring prints it.
pub fn parse_x11_colour(s: &str) -> Option<String> {
    let hex = |v: &str| u16::from_str_radix(v, 16).ok().filter(|_| !v.is_empty() && v.len() <= 4);
    let rgb = |r: u16, g: u16, b: u16| format!("#{:02x}{:02x}{:02x}", r, g, b);
    if let Some(v) = s.strip_prefix("rgb:") {
        let parts: Vec<&str> = v.split('/').collect();
        if let [r, g, b] = parts[..] {
            if r.len() == g.len() && g.len() == b.len() && (r.len() == 2 || r.len() == 4) {
                let shift = if r.len() == 4 { 8 } else { 0 };
                return Some(rgb(hex(r)? >> shift, hex(g)? >> shift, hex(b)? >> shift));
            }
        }
        return None;
    }
    if let Some(v) = s.strip_prefix('#') {
        let n = match v.len() { 6 => 2, 12 => 4, _ => return None };
        let shift = if n == 4 { 8 } else { 0 };
        let c = |i: usize| v.get(i * n..(i + 1) * n).and_then(hex).map(|x| x >> shift);
        return Some(rgb(c(0)?, c(1)?, c(2)?));
    }
    let parts: Vec<&str> = s.split(',').collect();
    if let [r, g, b] = parts[..] {
        let n = |v: &str| v.trim().parse::<u16>().ok().filter(|n| *n < 256);
        return Some(rgb(n(r)?, n(g)?, n(b)?));
    }
    let cmyk = s.strip_prefix("cmyk:").map(|v| (v, 4)).or_else(|| s.strip_prefix("cmy:").map(|v| (v, 3)));
    if let Some((v, n)) = cmyk {
        let f: Vec<f64> = v.split('/').map_while(|x| x.parse::<f64>().ok()).collect();
        if f.len() != n || f.iter().any(|x| !(0.0..=1.0).contains(x)) {
            return None;
        }
        let k = if n == 4 { f[3] } else { 0.0 };
        let c = |x: f64| ((1.0 - x) * (1.0 - k) * 255.0) as u16;
        return Some(rgb(c(f[0]), c(f[1]), c(f[2])));
    }
    // colour_parseX11 looks a name up in the X11 table, so "blue" is #0000ff, not colour 4
    crate::format::colour_byname(s.trim_matches(' ')).map(colour_name)
}

/// tmux's colour_tostring.
pub fn colour_name(c: vt100::Color) -> String {
    const NAMES: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
    match c {
        vt100::Color::Default => "default".into(),
        vt100::Color::Idx(i) if i < 8 => NAMES[i as usize].into(),
        vt100::Color::Idx(i) if i < 16 => format!("bright{}", NAMES[i as usize - 8]),
        vt100::Color::Idx(i) => format!("colour{i}"),
        vt100::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

fn decode_file_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let (host, path) = (&rest[..slash], &rest[slash..]);
    let mut bytes = Vec::new();
    let b = path.as_bytes();
    let mut i = 0;
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    while i < b.len() {
        // decode %XX from the bytes: slicing the str here could split a character
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(hi), Some(lo)) = (hex(b[i + 1]), hex(b[i + 2])) {
                bytes.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        bytes.push(b[i]);
        i += 1;
    }
    let p = String::from_utf8_lossy(&bytes).into_owned();
    // file://host/C:/dir on Windows
    if p.len() >= 3 && p.as_bytes()[0] == b'/' && p.as_bytes()[2] == b':' {
        return Some(p[1..].replace('/', "\\"));
    }
    // A UNC share as the PowerShell hook sends it: file://localhost//server/share. A
    // file://server/... URL is left alone: from an ssh session in a pane it names a
    // directory on that host, not a share.
    if cfg!(windows) && (host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
        if let Some(unc) = p.strip_prefix("//").map(|u| u.trim_start_matches('/')).filter(|u| !u.is_empty()) {
            return Some(format!("\\\\{}", unc.replace('/', "\\")));
        }
    }
    Some(p)
}

impl vt100::Callbacks for PaneCb {
    fn clipboard_query(&mut self, _: &mut vt100::Screen, ty: &[u8], bel: bool) {
        self.clipboard_queries.push((ty.to_vec(), bel));
    }
    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, ty: &[u8], data: &[u8]) {
        if self.clipboard_sets.len() < 16 { self.clipboard_sets.push((ty.to_vec(), data.to_vec())); }
    }
    fn dcs(&mut self, _: &mut vt100::Screen, payload: &[u8]) {
        // the parser already turned the doubled ESC bytes back into one
        if let Some(payload) = payload.strip_prefix(b"tmux;") {
            self.passthrough.push(payload.to_vec());
        }
    }
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = Some(String::from_utf8_lossy(title).into_owned());
    }
    fn reset(&mut self, _: &mut vt100::Screen) {
        // screen_write_reset: extended-keys always puts the pane back in mode 1
        self.ext_keys = (self.ext_option == 2) as u8;
    }
    fn rename_window(&mut self, _: &mut vt100::Screen, name: &[u8]) {
        self.rename = Some(String::from_utf8_lossy(name).into_owned());
    }
    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        self.osc.osc(params);
        match params {
            [b"7", url, ..] => self.cwd = decode_file_url(&String::from_utf8_lossy(url)),
            [b"9", b"9", path, ..] => self.cwd = Some(String::from_utf8_lossy(path).trim_matches('"').to_string()),
            _ => {}
        }
    }
    fn unhandled_csi(&mut self, screen: &mut vt100::Screen, i1: Option<u8>, _i2: Option<u8>, params: &[&[u16]], c: char) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c, first) {
            (None, 'n', 6) => {
                let (r, c) = screen.cursor_position();
                self.replies.extend(format!("\x1b[{};{}R", r + 1, c + 1).bytes());
            }
            (None, 'n', 5) => self.replies.extend(b"\x1b[0n"),
            (None, 'c', 0) => self.replies.extend(b"\x1b[?1;2c"),
            (Some(b'>'), 'c', 0) => self.replies.extend(b"\x1b[>84;0;0c"),
            // INPUT_CSI_MODSET: ignored with extended-keys off; always keeps at least mode 1
            (Some(b'>'), 'm', 4) if self.ext_option != 0 => {
                let m = params.get(1).and_then(|p| p.first()).copied().unwrap_or(0);
                self.ext_keys = if m == 2 { 2 } else if m == 1 || self.ext_option == 2 { 1 } else { 0 };
            }
            // CSI > 4 n turns modifyOtherKeys off again
            (Some(b'>'), 'n', 4) => self.ext_keys = (self.ext_option == 2) as u8,
            _ => {}
        }
    }
}

pub enum PaneMode {
    Copy(Box<CopyMode>),
    Clock,
    Tree(Box<modes::Tree>),
}

pub struct Pane {
    pub id: PaneId,
    pub generation: u32,
    pub parser: vt100::Parser<PaneCb>,
    pub input: Sender<Vec<u8>>,
    pub master: Box<dyn MasterPty + Send>,
    pub killer: Option<Box<dyn ChildKiller + Send + Sync>>,
    pub size: (u16, u16),
    pub pid: Option<u32>,
    pub start_cmd: String,
    pub start_path: String,
    pub title: String,
    pub dead: Option<(Option<i32>, u64)>,
    pub mode: Option<PaneMode>,
    pub options: Options,
    pub pipe: Option<Sender<Vec<u8>>>,
    pub last_output: Instant,
    pub created: u64,
    pub current_cmd: String,
    pub current_path: String,
    /// Counts working-directory reports from the pane (OSC 7).
    pub cwd_seq: u64,
    /// The conhost.exe hosting this pane's pseudoconsole (Windows).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub host_pid: Option<u32>,
    pub track: PaneTrack,
}

/// Pane state tmux keeps for format variables.
#[derive(Default)]
pub struct PaneTrack {
    /// The command as given (tmux's argv), for pane_start_command_list.
    pub argv: Vec<String>,
    /// When output last arrived (0 before any).
    pub last_output_time: u64,
    /// Counts output reads.
    pub output_generation: u64,
    /// Output arrived while the pane was in a mode.
    pub unseen_changes: bool,
    /// The pipe-pane command.
    pub pipe_pid: Option<u32>,
    /// The last copy-mode search (kept after copy mode exits).
    pub search: String,
    /// Since when synchronized output (DECSET 2026) has been on: tmux ends it after 1s.
    pub sync_since: Option<Instant>,
}

impl Pane {
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if self.size == (cols, rows) {
            return;
        }
        self.size = (cols, rows);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.parser.set_size(nz(rows), nz(cols));
        if let Some(PaneMode::Copy(cm)) = &mut self.mode {
            cm.resize(cols, rows);
        }
    }

    pub fn send(&self, data: Vec<u8>) {
        if !data.is_empty() && self.dead.is_none() {
            let _ = self.input.send(data);
        }
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    pub fn snapshot(&self) -> Vec<Line> {
        crate::cells::snapshot(self.parser.screen())
    }

    pub fn kill(&mut self) {
        if let Some(k) = self.killer.as_mut() {
            let _ = k.kill();
        }
    }

    /// Ends the pseudoconsole host too: older conhost builds (Windows 10, Server 2022)
    /// can block ClosePseudoConsole or spin when their client goes away.
    pub fn kill_host(&mut self) {
        #[cfg(windows)]
        if let Some(pid) = self.host_pid.take() {
            procinfo::win::terminate(pid);
        }
    }

    pub fn fg_pgrp(&self) -> Option<i32> {
        #[cfg(unix)]
        {
            self.master.process_group_leader()
        }
        #[cfg(windows)]
        {
            None
        }
    }
}

/// Drops a pane off the main loop: on older Windows (10, Server 2022) ClosePseudoConsole
/// blocks until conhost exits, which would freeze every client.
fn release(mut p: Pane) {
    p.kill_host();
    thread::spawn(move || drop(p));
}

pub fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v).unwrap_or(NonZeroU16::MIN)
}

pub const ALERT_ACTIVITY: u8 = 1;
pub const ALERT_BELL: u8 = 2;
pub const ALERT_SILENCE: u8 = 4;

pub struct Window {
    pub id: WinId,
    pub name: String,
    pub layout: Node,
    /// Pane indices follow the pane list, independently of layout geometry.
    pub pane_order: Vec<PaneId>,
    pub active: PaneId,
    pub last_active: Option<PaneId>,
    pub zoomed: bool,
    pub options: Options,
    #[allow(dead_code)]
    pub alerts: u8,
    pub layout_cycle: Option<usize>,
    pub old_layout: Option<String>,
    pub created: u64,
    pub activity: u64,
    /// Width/height the window is laid out at (follows the attached session size).
    pub sx: u16,
    pub sy: u16,
}

impl Window {
    pub fn panes(&self) -> Vec<PaneId> {
        let live = self.layout.panes();
        let mut order: Vec<_> = self.pane_order.iter().copied().filter(|p| live.contains(p)).collect();
        for p in live {
            if !order.contains(&p) { order.push(p); }
        }
        order
    }

    fn insert_pane(&mut self, target: PaneId, new: PaneId, before: bool) {
        let mut order = self.panes();
        order.retain(|p| *p != new);
        let idx = order.iter().position(|p| *p == target).unwrap_or(order.len());
        order.insert((idx + usize::from(!before)).min(order.len()), new);
        self.pane_order = order;
    }

    fn swap_pane_order(&mut self, a: PaneId, b: PaneId) {
        for p in &mut self.pane_order {
            if *p == a { *p = b; } else if *p == b { *p = a; }
        }
    }
}

pub struct Session {
    pub id: SessId,
    pub name: String,
    pub links: BTreeMap<u32, WinId>,
    pub active: u32,
    pub last: Vec<u32>,
    pub cols: u16,
    pub rows: u16,
    pub cwd: String,
    pub options: Options,
    pub env: BTreeMap<String, Option<String>>,
    pub hooks: BTreeMap<String, Vec<String>>,
    pub created: u64,
    pub activity: u64,
    pub last_attached: u64,
    pub alerts: BTreeMap<u32, u8>,
    /// Session group (new-session -t): members share one window list.
    pub group: Option<u32>,
}

impl Session {
    pub fn active_window(&self) -> WinId {
        // a session always has at least one link; fall back if `active` is stale
        self.links.get(&self.active).or_else(|| self.links.values().next()).copied().expect("session without windows")
    }
}

pub struct Identity {
    pub cols: u16,
    pub rows: u16,
    pub tty: bool,
    pub cwd: String,
    pub term: String,
    pub pid: u32,
    pub ctx_pane: Option<PaneId>,
    /// CLIENT_UTF8 as decided by the client (-u, locale or $TMUX)
    pub utf8: bool,
    pub env: Vec<(String, String)>,
}

pub enum PromptKind {
    Command,
    Confirm,
    Key,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub prompts: Vec<(String, String)>,
    pub answers: Vec<String>,
    pub label: String,
    pub input: Vec<char>,
    pub cursor: usize,
    pub template: String,
    pub history_pos: Option<usize>,
    /// The history this prompt reads and adds to (command-prompt -T).
    pub history_type: usize,
    /// vi command mode (status-keys vi after Escape).
    pub command_mode: bool,
    /// Text cut with C-w, pasted again by C-y.
    pub copied: Option<Vec<char>>,
    pub pane: Option<PaneId>,
    pub numeric: bool,
    pub single_key: bool,
    pub copy_incremental: Option<(PaneId, bool)>,
    /// command-prompt -e: backspace on an empty prompt closes it.
    pub bspace_exit: bool,
    /// confirm-before: the key that confirms (-c) and whether Enter does too (-y).
    pub confirm: (char, bool),
}

pub enum Overlay {
    Menu(modes::Menu),
    Popup(modes::Popup),
    DisplayPanes { until: Instant, template: String },
}

pub struct RenderState {
    pub frame: render::Frame,
    pub cursor: Option<(u16, u16)>,
    pub modes: render::Modes,
    /// Mouse range of every cell of each status line (tmux status entry ranges).
    pub status_ranges: Vec<Vec<Option<crate::format::Range>>>,
}

/// What a mouse drag in progress updates (tmux's mouse_drag_update callback).
pub struct Drag {
    pub kind: DragKind,
}

pub enum DragKind {
    /// resize-pane -M: borders next to where the drag started follow the mouse.
    Border,
    /// A copy-mode selection (copy-mode -M, begin-selection from the mouse).
    Copy { pane: PaneId },
}

/// Multi-click detection, as tmux's CLIENT_DOUBLECLICK / CLIENT_TRIPLECLICK and click timer.
#[derive(Clone, Default)]
pub struct ClickState {
    /// 0: none, 1: waiting for a second click, 2: waiting for a third click.
    pub waiting: u8,
    pub button: u8,
    pub event: Option<keys::Mouse>,
    pub deadline: Option<Instant>,
}

pub struct Client {
    pub id: ClientId,
    pub tx: Sender<Vec<u8>>,
    pub ident: Option<Identity>,
    pub session: Option<SessId>,
    pub attached: bool,
    pub readonly: bool,
    pub size: (u16, u16),
    pub key_table: String,
    pub repeat_until: Option<Instant>,
    pub keyp: keys::Parser,
    pub input_state: input::InputState,
    pub term_caps: BTreeMap<String, String>,
    pub esc_deadline: Option<Instant>,
    pub prompt: Option<Prompt>,
    pub overlay: Option<Overlay>,
    pub overlay_epoch: u64,
    pub message: Option<(String, Instant)>,
    pub prev: Option<RenderState>,
    pub dirty: bool,
    pub last_session: Option<SessId>,
    pub created: u64,
    pub activity: u64,
    pub drag: Option<Drag>,
    pub title: String,
    pub suspended: bool,
    pub click: ClickState,
    /// Position and button of the previous mouse event (tmux's lx, ly, lb).
    pub mouse_last: (u16, u16, u8),
    /// Button + 1 of the drag in progress, 0 when not dragging (tmux's mouse_drag_flag).
    pub mouse_drag_flag: u8,
    /// Mouse ranges of the status lines as last drawn.
    pub status_ranges: Vec<Vec<Option<crate::format::Range>>>,
    /// Control mode: 0 = normal client, 1 = -C, 2 = -CC.
    pub control: u8,
    /// Size set with refresh-client -C (control clients don't size windows otherwise).
    pub control_size: Option<(u16, u16)>,
    pub control_seq: u64,
    pub seen: control::Seen,
    /// What the client's terminal supports (terminal-features, -T).
    pub features: features::Features,
    /// Frames queued for the writer thread, and since when the queue has been non-empty
    /// (ms since the epoch): how far behind the client is (control mode pause-after).
    pub pending: Arc<std::sync::atomic::AtomicUsize>,
    pub queued_bytes: Arc<std::sync::atomic::AtomicUsize>,
    pub closed: Arc<std::sync::atomic::AtomicBool>,
    pub backlog_since: Arc<std::sync::atomic::AtomicU64>,
    pub ctl: control::Flags,
    /// Connecting user (uid / SID) and whether server-access lets it in / makes it read-only.
    pub user: Option<String>,
    pub denied: bool,
    pub acl_readonly: bool,
    /// Bytes written to the client and dropped for it (client_written, client_discarded).
    pub io: Arc<ClientIo>,
}

#[derive(Default)]
pub struct ClientIo {
    pub written: std::sync::atomic::AtomicU64,
    pub discarded: std::sync::atomic::AtomicU64,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Client {
    /// CLIENT_UTF8: set by the client, or by applying the utf8 terminal feature (never cleared
    /// by utf8@).
    pub fn utf8(&self) -> bool {
        self.ident.as_ref().is_none_or(|i| i.utf8) || self.features.has("utf8")
    }

    pub fn is_readonly(&self) -> bool {
        self.readonly || self.acl_readonly
    }

    pub fn send(&self, kind: u8, payload: &[u8]) {
        use std::sync::atomic::Ordering;
        let len = payload.len().saturating_add(5);
        if self.closed.load(Ordering::SeqCst) {
            self.io.discarded.fetch_add(len as u64, Ordering::Relaxed);
            return;
        }
        #[allow(deprecated)]
        let reserved = self.queued_bytes.fetch_update(Ordering::SeqCst, Ordering::SeqCst,
            |n| n.checked_add(len).filter(|n| *n <= transport::MAX_OUTPUT_BACKLOG));
        if reserved.is_err() {
            self.io.discarded.fetch_add(len as u64, Ordering::Relaxed);
            self.closed.store(true, Ordering::SeqCst);
            return;
        }
        if self.pending.fetch_add(1, Ordering::SeqCst) == 0 {
            self.backlog_since.store(now_ms(), Ordering::SeqCst);
        }
        if self.tx.send(frame(kind, payload)).is_err() {
            self.queued_bytes.fetch_sub(len, Ordering::SeqCst);
            self.pending.fetch_sub(1, Ordering::SeqCst);
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    /// How long output has been waiting to be written to this client.
    pub fn lag(&self) -> Duration {
        use std::sync::atomic::Ordering;
        if self.pending.load(Ordering::SeqCst) == 0 {
            return Duration::ZERO;
        }
        Duration::from_millis(now_ms().saturating_sub(self.backlog_since.load(Ordering::SeqCst)))
    }

    /// Ends an attached client; control clients get %exit first.
    pub fn send_exit(&self, msg: &str) {
        if self.control > 0 {
            let reason = msg.trim_matches(['[', ']']);
            let line = if reason.is_empty() { "%exit\n".to_string() } else { format!("%exit {reason}\n") };
            self.send(S_OUTPUT, line.as_bytes());
            if self.ctl.wait_exit {
                // wait-exit: the client leaves after its next line
                self.ctl.exiting.set(true);
                return;
            }
        }
        // detach and session exit clear `attached` first; the reset depends only on what the
        // terminal was given, which is nothing for a command client
        if self.control == 0 {
            let reset = self.tty_stop_sequence();
            if !reset.is_empty() { self.send(S_OUTPUT, reset.as_bytes()); }
        }
        self.send(S_EXIT, msg.as_bytes());
    }
    pub fn name(&self) -> String {
        format!("client-{}", self.id)
    }
}

pub struct Buffer {
    pub name: String,
    pub data: Vec<u8>,
    pub automatic: bool,
    pub created: u64,
}

pub struct Binding {
    pub cmd: String,
    pub repeat: bool,
    pub note: String,
}

#[derive(Default, Clone)]
pub struct Ctx {
    /// The attached client the command acts for (key bindings, prompts, or the CLI client).
    pub client: Option<ClientId>,
    /// CLI client waiting for S_REPLY.
    pub reply: Option<ClientId>,
    /// Default target pane.
    pub pane: Option<PaneId>,
    pub mouse: Option<input::MouseCtx>,
    pub out: output::Output,
    pub hook_depth: u8,
    /// True when run from a key binding (output goes to view mode).
    pub from_key: bool,
    /// Command number of a control-mode command (reply goes in a %begin/%end block).
    pub control_seq: Option<u64>,
    /// The originating request's restriction, retained across hooks and parked queues.
    pub readonly: bool,
    /// Files being sourced (innermost last), for #{current_file}.
    pub files: Vec<String>,
}

/// Queue markers source-file puts around a file's commands (never typed: they start with NUL).
pub const FILE_PUSH: &str = "\0file-push";
pub const FILE_POP: &str = "\0file-pop";

/// The tmux release whose behaviour this server matches (`#{version}`).
pub const TMUX_VERSION: &str = "3.8";

pub struct Queue {
    pub cmds: VecDeque<Vec<String>>,
    pub ctx: Ctx,
    /// How many commands at the front have soft errors.
    pub soft: usize,
}

pub enum Flow {
    Next,
    WaitOverlay(ClientId, u64),
    /// Run these commands next, before the rest of the queue (source-file, if-shell -F, run-shell -C).
    /// With `soft`, their errors are reported but don't stop the queue (like config files).
    Inject(Vec<Vec<String>>, bool),
}

pub enum JobKind {
    /// `stderr`: run-shell -E shows the command's stderr (tmux discards it otherwise).
    RunShell { pane: Option<PaneId>, background: bool, stderr: bool },
    IfShell { then: String, otherwise: Option<String> },
}

pub struct Parked {
    pub queue: Queue,
    pub kind: JobKind,
}

pub struct Server {
    pub tx: mpsc::SyncSender<Event>,
    pub pty: Box<dyn PtySystem + Send>,
    pub sessions: BTreeMap<SessId, Session>,
    pub windows: HashMap<WinId, Window>,
    pub panes: HashMap<PaneId, Pane>,
    pub clients: BTreeMap<ClientId, Client>,
    pub next_id: u32,
    pub opts_server: Options,
    pub opts_session: Options,
    pub opts_window: Options,
    pub env: BTreeMap<String, Option<String>>,
    pub hooks: BTreeMap<String, Vec<String>>,
    pub buffers: Vec<Buffer>,
    pub buffer_seq: u32,
    pub bindings: BTreeMap<String, BTreeMap<String, Binding>>,
    pub messages: VecDeque<(u64, String)>,
    /// Prompt history per type (command, search).
    pub prompt_history: [Vec<String>; 2],
    pub jobs: HashMap<u32, Parked>,
    pub waits: HashMap<String, (bool, Vec<Queue>)>,
    /// wait-for channels signalled while nobody waited (the next wait returns at once).
    pub waits_woken: std::collections::HashSet<String>,
    pub overlay_waits: Vec<(ClientId, u64, String, Queue)>,
    /// Copy-mode panes scrolling while a selection drag rests at an edge.
    pub copy_drag_timers: HashMap<PaneId, Instant>,
    /// A lookup of what the panes run is in progress (at most one at a time).
    pub pane_info_busy: bool,
    /// Queues paused by `run-shell -d`.
    pub delayed: Vec<(Instant, Queue)>,
    /// Mouse event of the command being run, for the mouse_* formats.
    pub cur_mouse: Option<input::MouseCtx>,
    /// The file the running command came from (#{current_file}).
    pub cur_file: Option<String>,
    pub marked: Option<PaneId>,
    pub start_time: u64,
    pub host: String,
    pub quit: bool,
    pub had_session: bool,
    pub last_tick: Instant,
    /// One-shot deadlines armed by status animation formats.
    pub animation_deadlines: std::cell::RefCell<HashMap<ClientId, Instant>>,
    /// `#(command)` results: last output, when it last started, whether it is running.
    pub shell_cache: std::cell::RefCell<HashMap<String, (String, Instant, bool)>>,
    /// Restriction of the current request, independent of format/command targets.
    pub readonly_context: std::cell::Cell<bool>,
    /// Last synced window list of each session group.
    pub groups: HashMap<u32, BTreeMap<u32, WinId>>,
    /// The user running the server (always allowed).
    pub owner: String,
    /// server-access: (user id, may write)
    pub acl: Vec<(String, bool)>,
    /// Windows: users the named pipe's ACL was created for (None until listening).
    pub pipe_acl: Option<Vec<String>>,
    /// Configuration files tried at startup (config_files).
    pub cfg_files: Vec<String>,
}

pub fn run(config: Option<String>) -> io::Result<()> {
    // stderr is /dev/null for the detached server: keep panics in the log
    std::panic::set_hook(Box::new(|info| log(&format!("panic: {info}"))));
    if ipc::connect().is_ok() {
        return Ok(()); // another server already owns the endpoint
    }
    let (tx, rx) = mpsc::sync_channel(64);
    let atx = tx.clone();

    let mut srv = Server::new(tx);
    if srv.owner.is_empty() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "cannot determine server owner"));
    }
    // config first: its server-access lines decide who may open the Windows pipe
    srv.load_config(config);
    srv.load_prompt_history();
    let allowed: Vec<String> = srv.acl.iter().map(|a| a.0.clone()).collect();
    let Some(listener) = ipc::listen_exclusive(&allowed)? else {
        return Ok(()); // another server won the race
    };
    srv.pipe_acl = Some(allowed);
    if !srv.acl.is_empty() {
        ipc::set_shared(true);
    }
    log(&format!("server started on {}", ipc::describe()));
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

    let started = Instant::now();
    loop {
        let timeout = srv.next_timeout();
        match rx.recv_timeout(timeout) {
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
        srv.sync_groups();
        srv.timers();
        srv.control_notify();
        srv.render_all();
        let exit_empty = options::is_on(srv.opts_server.get("exit-empty").unwrap_or("on"));
        let idle = srv.sessions.is_empty() && exit_empty && (srv.had_session || started.elapsed() > Duration::from_secs(30));
        let unattached = options::is_on(srv.opts_server.get("exit-unattached").unwrap_or("off"))
            && srv.had_session
            && !srv.clients.values().any(|c| c.attached);
        if srv.quit || idle || unattached {
            break;
        }
    }
    srv.shutdown();
    srv.save_prompt_history();
    ipc::cleanup();
    log("server exiting");
    Ok(())
}

impl Server {
    fn new(tx: mpsc::SyncSender<Event>) -> Self {
        let mut env = BTreeMap::new();
        for (k, v) in std::env::vars() {
            if k != "PWSH_TMUX" && k != "PWSH_TMUX_PANE" {
                env.insert(k, Some(v));
            }
        }
        let mut srv = Self {
            tx,
            pty: native_pty_system(),
            sessions: BTreeMap::new(),
            windows: HashMap::new(),
            panes: HashMap::new(),
            clients: BTreeMap::new(),
            next_id: 0,
            opts_server: Options::defaults(Scope::Server),
            opts_session: Options::defaults(Scope::Session),
            opts_window: Options::defaults(Scope::Window),
            env,
            hooks: BTreeMap::new(),
            buffers: Vec::new(),
            buffer_seq: 0,
            bindings: input::default_bindings(),
            messages: VecDeque::new(),
            prompt_history: Default::default(),
            jobs: HashMap::new(),
            waits: HashMap::new(),
            waits_woken: Default::default(),
            overlay_waits: Vec::new(),
            copy_drag_timers: HashMap::new(),
            pane_info_busy: false,
            delayed: Vec::new(),
            cur_mouse: None,
            cur_file: None,
            marked: None,
            start_time: now_secs(),
            host: procinfo::hostname(),
            quit: false,
            had_session: false,
            last_tick: Instant::now(),
            animation_deadlines: Default::default(),
            shell_cache: Default::default(),
            readonly_context: Default::default(),
            groups: HashMap::new(),
            owner: ipc::own_user_id(),
            acl: Vec::new(),
            pipe_acl: None,
            cfg_files: Vec::new(),
        };
        srv.opts_session.set("default-shell", procinfo::default_shell());
        srv
    }

    pub fn id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    fn load_config(&mut self, explicit: Option<String>) {
        let mut files = Vec::new();
        if let Some(f) = explicit {
            files.push((f, false));
        } else {
            if let Some(h) = procinfo::home_dir() {
                files.push((format!("{h}/.pwsh-tmux.conf"), true));
            }
            let xdg = std::env::var("XDG_CONFIG_HOME").ok().filter(|s| !s.is_empty()).or_else(|| procinfo::home_dir().map(|h| format!("{h}/.config")));
            if let Some(x) = xdg {
                files.push((format!("{x}/pwsh-tmux/pwsh-tmux.conf"), true));
            }
        }
        self.cfg_files = files.iter().map(|f| f.0.clone()).collect();
        for (f, quiet) in files {
            let mut args = vec!["source-file".to_string()];
            if quiet {
                args.push("-q".into());
            }
            args.push(f);
            let ctx = Ctx::default();
            self.run_queue(Queue { cmds: VecDeque::from([args]), ctx , soft: 0 });
        }
    }

    pub fn add_message(&mut self, msg: &str) {
        self.messages.push_back((now_secs(), msg.to_string()));
        let limit = self.opt_server_num("message-limit").max(1) as usize;
        while self.messages.len() > limit {
            self.messages.pop_front();
        }
    }

    // ---- options -----------------------------------------------------------

    pub fn opt_server(&self, name: &str) -> String {
        self.opts_server.get(name).unwrap_or("").to_string()
    }
    pub fn opt_server_num(&self, name: &str) -> i64 {
        self.opts_server.get(name).and_then(|v| v.parse().ok()).unwrap_or(0)
    }
    pub fn opt_session(&self, sid: Option<SessId>, name: &str) -> String {
        sid.and_then(|s| self.sessions.get(&s))
            .and_then(|s| s.options.lookup(name))
            .or_else(|| self.opts_session.get(name))
            .unwrap_or("")
            .to_string()
    }
    pub fn opt_session_num(&self, sid: Option<SessId>, name: &str) -> i64 {
        self.opt_session(sid, name).parse().unwrap_or(0)
    }
    pub fn opt_session_on(&self, sid: Option<SessId>, name: &str) -> bool {
        options::is_on(&self.opt_session(sid, name))
    }
    pub fn opt_window(&self, wid: Option<WinId>, name: &str) -> String {
        wid.and_then(|w| self.windows.get(&w))
            .and_then(|w| w.options.lookup(name))
            .or_else(|| self.opts_window.get(name))
            .unwrap_or("")
            .to_string()
    }
    pub fn opt_window_num(&self, wid: Option<WinId>, name: &str) -> i64 {
        self.opt_window(wid, name).parse().unwrap_or(0)
    }
    pub fn opt_window_on(&self, wid: Option<WinId>, name: &str) -> bool {
        options::is_on(&self.opt_window(wid, name))
    }
    pub fn opt_pane(&self, pid: PaneId, name: &str) -> String {
        if let Some(v) = self.panes.get(&pid).and_then(|p| p.options.lookup(name)) {
            return v.to_string();
        }
        self.opt_window(self.window_of(pid), name)
    }

    // ---- lookup helpers --------------------------------------------------------

    pub fn window_of(&self, pane: PaneId) -> Option<WinId> {
        self.windows.values().find(|w| w.panes().contains(&pane)).map(|w| w.id)
    }

    /// Sessions (and index) a window is linked into.
    pub fn links_of(&self, wid: WinId) -> Vec<(SessId, u32)> {
        let mut v = Vec::new();
        for s in self.sessions.values() {
            for (idx, w) in &s.links {
                if *w == wid {
                    v.push((s.id, *idx));
                }
            }
        }
        v
    }

    pub fn session_of_pane(&self, pane: PaneId) -> Option<(SessId, u32)> {
        let wid = self.window_of(pane)?;
        let links = self.links_of(wid);
        // Prefer a session where this window is current.
        links.iter().find(|(s, i)| self.sessions[s].active == *i).or(links.first()).copied()
    }

    pub fn client_session(&self, cid: ClientId) -> Option<SessId> {
        self.clients.get(&cid)?.session.filter(|s| self.sessions.contains_key(s))
    }

    pub fn current_session(&self) -> Option<SessId> {
        self.sessions.values().max_by_key(|s| (s.activity, s.id)).map(|s| s.id)
    }

    pub fn mark_session_dirty(&mut self, sid: SessId) {
        for c in self.clients.values_mut() {
            if c.session == Some(sid) {
                c.dirty = true;
            }
        }
    }

    pub fn mark_window_dirty(&mut self, wid: WinId) {
        for (sid, _) in self.links_of(wid) {
            self.mark_session_dirty(sid);
        }
    }

    pub fn mark_all_dirty(&mut self) {
        for c in self.clients.values_mut() {
            c.dirty = true;
        }
    }

    pub fn set_message(&mut self, cid: ClientId, msg: &str) {
        let ms = self.client_session(cid).map(|s| self.opt_session_num(Some(s), "display-time")).unwrap_or(750).max(1) as u64;
        self.add_message(msg);
        if let Some(c) = self.clients.get_mut(&cid) {
            c.message = Some((msg.to_string(), Instant::now() + Duration::from_millis(ms)));
            c.dirty = true;
        }
    }

    // ---- events -------------------------------------------------------------------

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Conn(stream) => self.accept(stream),
            Event::ClientMsg(cid, kind, payload) => self.client_msg(cid, kind, payload),
            Event::ClientGone(cid) => self.client_gone(cid),
            Event::PaneData(id, generation, data) => self.pane_data(id, generation, &data),
            Event::PaneExit(id, generation, status) => {
                if self.panes.get(&id).is_some_and(|p| p.generation == generation) {
                    self.pane_exited(id, status);
                }
            }
            Event::Job(id, status, output) => self.job_done(id, status, output),
            Event::ClientUser(cid, user) => self.client_user(cid, user),
            Event::PaneInfo(info) => self.pane_info(info),
            Event::FormatShell(command, output) => {
                if let Some(e) = self.shell_cache.borrow_mut().get_mut(&command) {
                    e.0 = output;
                    e.2 = false;
                }
                self.mark_all_dirty();
            }
        }
    }

    fn accept(&mut self, stream: Stream) {
        if self.clients.len() >= transport::MAX_CLIENTS || stream.set_nonblocking(true).is_err() {
            return;
        }
        let cid = self.id();
        let stream = Arc::new(stream);
        let recv = stream.clone();
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let backlog_since = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let writer_pending = pending.clone();
        let writer_since = backlog_since.clone();
        let queued_bytes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let writer_bytes = queued_bytes.clone();
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer_closed = closed.clone();
        let reader_closed = closed.clone();
        let io = Arc::new(ClientIo::default());
        let writer_io = io.clone();
        thread::spawn(move || {
            use std::sync::atomic::Ordering;
            loop {
                if writer_closed.load(Ordering::SeqCst) { break; }
                let buf = match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(buf) => buf,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                let mut send = transport::Io { stream: &stream, closed: &writer_closed, deadline: Some(Instant::now() + transport::IO_TIMEOUT) };
                if send.write_all(&buf).is_err() {
                    break;
                }
                writer_bytes.fetch_sub(buf.len(), Ordering::SeqCst);
                writer_io.written.fetch_add(buf.len() as u64, Ordering::Relaxed);
                // still behind: the backlog now starts at this write
                if writer_pending.fetch_sub(1, Ordering::SeqCst) > 1 {
                    writer_since.store(now_ms(), Ordering::SeqCst);
                }
            }
            writer_closed.store(true, Ordering::SeqCst);
        });
        let etx = self.tx.clone();
        let owner = self.owner.clone();
        let allowed_users: Vec<String> = self.acl.iter().map(|a| a.0.clone()).collect();
        thread::spawn(move || {
            let mut first = true;
            let mut reader = transport::Io { stream: &recv, closed: &reader_closed, deadline: Some(Instant::now() + transport::IO_TIMEOUT) };
            while let Ok((kind, payload)) = read_frame_checked(&mut reader, |kind, len| {
                if len > transport::MAX_CLIENT_FRAME {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "client frame too large"));
                }
                if first {
                    // Windows impersonation needs a pipe read. Reading just the
                    // header lets us authenticate before allocating the payload.
                    first = false;
                    let user = ipc::peer_user_id(&recv);
                    let allowed = user.as_ref().is_some_and(|id| !id.is_empty()
                        && (id == &owner || ipc::is_superuser(id) || allowed_users.contains(id)
                            || ipc::group_id_of_user(id).is_some_and(|g| allowed_users.contains(&g))));
                    if !allowed {
                        // No writer output exists before identification. Send the
                        // refusal here so closing the cancelled connection cannot
                        // discard an asynchronously queued error reply.
                        let mut reject = transport::Io { stream: &recv, closed: &reader_closed,
                            deadline: Some(Instant::now() + Duration::from_millis(100)) };
                        let _ = write_frame(&mut reject, S_REPLY, &encode_reply(1, "access not allowed"));
                        log(&format!("refused client from user {}", user.as_deref().unwrap_or("unknown")));
                        return Err(io::ErrorKind::PermissionDenied.into());
                    }
                    etx.send(Event::ClientUser(cid, user)).map_err(|_| io::ErrorKind::BrokenPipe)?;
                    if kind != C_IDENTIFY {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid identify frame"));
                    }
                }
                Ok(())
            }) {
                reader.deadline = None;
                if etx.send(Event::ClientMsg(cid, kind, payload)).is_err() {
                    return;
                }
            }
            reader_closed.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = etx.send(Event::ClientGone(cid));
        });
        let now = now_secs();
        self.clients.insert(
            cid,
            Client {
                id: cid,
                tx,
                ident: None,
                session: None,
                attached: false,
                readonly: false,
                size: (80, 24),
                key_table: "root".into(),
                repeat_until: None,
                keyp: keys::Parser::new(),
                input_state: Default::default(),
                term_caps: Default::default(),
                esc_deadline: None,
                prompt: None,
                overlay: None,
                overlay_epoch: 0,
                message: None,
                prev: None,
                dirty: false,
                last_session: None,
                created: now,
                activity: now,
                drag: None,
                title: String::new(),
                suspended: false,
                click: ClickState::default(),
                mouse_last: (0, 0, 0),
                mouse_drag_flag: 0,
                status_ranges: Vec::new(),
                control: 0,
                control_size: None,
                control_seq: 0,
                seen: Default::default(),
                features: Default::default(),
                pending,
                queued_bytes,
                closed,
                backlog_since,
                ctl: Default::default(),
                user: None,
                denied: false,
                acl_readonly: false,
                io,
            },
        );
    }

    /// Replacing an overlay also ends the PTY/process owned by an old popup.
    pub fn replace_overlay(&mut self, cid: ClientId, overlay: Option<Overlay>) -> Result<u64, String> {
        let (old, epoch) = {
            let c = self.clients.get_mut(&cid).ok_or("no client")?;
            let old = std::mem::replace(&mut c.overlay, overlay);
            c.overlay_epoch = c.overlay_epoch.wrapping_add(1);
            c.dirty = true;
            (old, c.overlay_epoch)
        };
        if let Some(Overlay::Popup(popup)) = old { self.destroy_pane(popup.pane); }
        Ok(epoch)
    }

    fn client_gone(&mut self, cid: ClientId) {
        let Some(c) = self.clients.remove(&cid) else { return };
        c.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        let readonly = c.is_readonly();
        if let Some(Overlay::Popup(p)) = c.overlay {
            self.destroy_pane(p.pane);
        }
        if c.attached {
            if let Some(sid) = c.session {
                self.run_hook(Some(sid), "client-detached", Ctx { readonly, ..Ctx::default() });
                self.update_session_size(sid);
                self.check_destroy_unattached(sid);
            }
        }
    }

    /// server-access: let the owner, root/SYSTEM and allowed users in.
    fn client_user(&mut self, cid: ClientId, user: Option<String>) {
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let id = user.unwrap_or_default();
        // a user entry takes precedence over the user's group entry (server-access -g)
        let group = ipc::group_id_of_user(&id).filter(|_| id != self.owner && !ipc::is_superuser(&id));
        let entry = self.acl.iter().find(|a| a.0 == id).or_else(|| self.acl.iter().find(|a| Some(&a.0) == group.as_ref())).cloned();
        let allowed = !id.is_empty() && (id == self.owner || ipc::is_superuser(&id) || entry.is_some());
        c.user = Some(id.clone());
        if !allowed {
            c.denied = true;
            c.send(S_REPLY, &encode_reply(1, "access not allowed"));
            log(&format!("refused client from user {id}"));
            return;
        }
        c.acl_readonly = entry.is_some_and(|e| !e.1);
        c.readonly |= c.acl_readonly;
    }

    fn client_msg(&mut self, cid: ClientId, kind: u8, payload: Vec<u8>) {
        if self.clients.get(&cid).is_some_and(|c| c.denied) {
            return;
        }
        match kind {
            C_IDENTIFY => {
                let a = decode_args(&payload);
                let get = |i: usize| a.get(i).cloned().unwrap_or_default();
                let ident = Identity {
                    cols: get(0).parse::<u16>().unwrap_or(80).min(MAX_SIZE),
                    rows: get(1).parse::<u16>().unwrap_or(24).min(MAX_SIZE),
                    tty: get(2) == "1",
                    cwd: get(3),
                    term: get(4),
                    pid: get(5).parse().unwrap_or(0),
                    ctx_pane: get(6).strip_prefix('%').and_then(|s| s.parse().ok()),
                    utf8: get(9) != "0",
                    env: a.iter().skip(10).filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).collect(),
                };
                let control = get(7).parse().unwrap_or(0);
                let colorterm = ident.env.iter().find(|(k, _)| k == "COLORTERM").map(|(_, v)| v.as_str()).unwrap_or("");
                let table: Vec<String> = self.opts_server.0.iter().filter(|(k, _)| k.starts_with("terminal-features[")).map(|(_, v)| v.clone()).collect();
                let program = ident.env.iter().find(|(k, _)| k == "TERM_PROGRAM").map(|(_, v)| v.as_str()).unwrap_or("");
                let mut features = features::compute_named(&ident.term, program, colorterm, &table, &get(8));
                let table = self.opts_server.array_items("terminal-overrides").into_iter().map(|(_, v)| v).collect::<Vec<_>>();
                let caps = features::overrides(&ident.term, &table, &mut features);
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.term_caps = caps;
                    c.control = control;
                    c.features = features;
                    c.size = (ident.cols.max(1), ident.rows.max(1));
                    c.ident = Some(ident);
                }
            }
            C_COMMAND => {
                let argv = decode_args(&payload);
                // Older clients put unescaped stdin bytes into NUL-delimited
                // argv. Reject the entire request before splitting commands.
                if argv.iter().any(|arg| arg.starts_with("\x01stdin:")) {
                    if let Some(c) = self.clients.get(&cid) {
                        c.send(S_REPLY, &encode_reply(1, "unsafe legacy stdin buffer encoding; update the client"));
                    }
                    return;
                }
                let cmds = if argv.is_empty() {
                    match crate::cmdparse::parse(&self.opt_server("default-client-command")) {
                        Ok(cmds) => cmds,
                        Err(e) => { if let Some(c) = self.clients.get(&cid) { c.send(S_REPLY, &encode_reply(1, &e)); } return; }
                    }
                } else { crate::cmdparse::split_commands(&argv) };
                let pane = self.clients.get(&cid).and_then(|c| c.ident.as_ref()).and_then(|i| i.ctx_pane).filter(|p| self.panes.contains_key(p));
                // control clients get the reply to their command line in a %begin block too
                let control_seq = self.clients.get_mut(&cid).filter(|c| c.control > 0).map(|c| {
                    c.control_seq += 1;
                    c.control_seq
                });
                let ctx = Ctx { client: Some(cid), reply: Some(cid), pane, control_seq, ..Ctx::default() };
                self.run_queue(Queue { cmds: cmds.into_iter().filter(|c| !c.is_empty()).collect(), ctx , soft: 0 });
            }
            C_INPUT => {
                if let Some(sid) = self.client_session(cid) { if let Some(s) = self.sessions.get_mut(&sid) { s.activity = now_secs(); } }
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.activity = now_secs();
                }
                self.handle_input(cid, &payload);
            }
            C_RESIZE => {
                let Some((cols, rows)) = decode_size(&payload) else { return };
                let Some(c) = self.clients.get_mut(&cid) else { return };
                c.size = (cols.clamp(1, MAX_SIZE), rows.clamp(1, MAX_SIZE));
                c.prev = None;
                c.dirty = true;
                if let Some(sid) = c.session {
                    self.update_session_size(sid);
                }
            }
            C_CONTROL_LINE => {
                let line = String::from_utf8_lossy(&payload).into_owned();
                self.control_line(cid, &line);
            }
            C_RESUMED => {
                if let Some(sid) = self.client_session(cid) { if let Some(s) = self.sessions.get_mut(&sid) { s.activity = now_secs(); } }
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.suspended = false;
                    c.activity = now_secs();
                    c.prev = None;
                    c.dirty = true;
                }
            }
            _ => {}
        }
    }

    fn pane_data(&mut self, id: PaneId, generation: u32, data: &[u8]) {
        if !self.panes.get(&id).is_some_and(|p| p.generation == generation) {
            return;
        }
        self.control_output(id, data);
        let scroll_on_clear = options::is_on(&self.opt_pane(id, "scroll-on-clear"));
        let alternate = options::is_on(&self.opt_pane(id, "alternate-screen"));
        let widths = features::codepoint_widths(&self.opts_server.array_items("codepoint-widths").into_iter().map(|(_, v)| v).collect::<Vec<_>>());
        let variation_wide = options::is_on(&self.opt_server("variation-selector-always-wide"));
        let input_limit = self.opt_server_num("input-buffer-size") as usize;
        let ext_option = ext_keys_option(&self.opt_server("extended-keys"));
        let p = self.panes.get_mut(&id).unwrap();
        p.parser.callbacks_mut().ext_option = ext_option;
        p.parser.set_input_buffer_size(input_limit);
        p.parser.screen_mut().set_alternate_screen_allowed(alternate);
        p.parser.screen_mut().set_codepoint_widths(widths, variation_wide);
        p.parser.screen_mut().set_scroll_on_clear(scroll_on_clear);
        p.parser.process(data);
        p.last_output = Instant::now();
        if !data.is_empty() {
            p.track.last_output_time = now_secs();
            p.track.output_generation += 1;
            p.track.unseen_changes |= p.mode.is_some();
        }
        let sync = p.parser.screen().synchronized_output();
        if sync != p.track.sync_since.is_some() {
            p.track.sync_since = sync.then(Instant::now);
        }
        if let Some(pipe) = &p.pipe {
            if pipe.send(data.to_vec()).is_err() {
                p.pipe = None;
                p.track.pipe_pid = None;
            }
        }
        let cb = p.parser.callbacks_mut();
        let replies = std::mem::take(&mut cb.replies);
        let bell = std::mem::take(&mut cb.bell);
        let title = cb.title.take();
        let rename = cb.rename.take();
        let queries = std::mem::take(&mut cb.clipboard_queries);
        let sets = std::mem::take(&mut cb.clipboard_sets);
        let passthrough = std::mem::take(&mut cb.passthrough);
        if let Some(cwd) = cb.cwd.take() {
            p.current_path = cwd;
            p.cwd_seq += 1;
        }
        p.send(replies);
        self.pane_input_requests(id, queries, sets, passthrough);
        if let Some(t) = title.filter(|_| options::is_on(&self.opt_pane(id, "allow-set-title"))) {
            self.panes.get_mut(&id).unwrap().title = t;
        }
        // allow-rename covers only ESC k (input_exit_rename); an empty name goes back to
        // automatic-rename's inherited value, or to an empty name when that is off
        if let Some(n) = rename.filter(|_| options::is_on(&self.opt_pane(id, "allow-rename"))) {
            if let Some(wid) = self.window_of(id) {
                if n.is_empty() {
                    if let Some(w) = self.windows.get_mut(&wid) { w.options.unset("automatic-rename"); }
                    if !self.opt_window_on(Some(wid), "automatic-rename") {
                        if let Some(w) = self.windows.get_mut(&wid) { w.name = n; }
                    }
                } else if let Some(w) = self.windows.get_mut(&wid) {
                    w.name = n;
                    w.options.set("automatic-rename", "off".into());
                }
            }
        }
        let Some(wid) = self.window_of(id) else {
            // popup pane
            for c in self.clients.values_mut() {
                if matches!(&c.overlay, Some(Overlay::Popup(pp)) if pp.pane == id) {
                    c.dirty = true;
                }
            }
            return;
        };
        if let Some(w) = self.windows.get_mut(&wid) {
            w.activity = now_secs();
        }
        if bell && self.opt_window_on(Some(wid), "monitor-bell") {
            self.alert(wid, ALERT_BELL);
        }
        if self.opt_window_on(Some(wid), "monitor-activity") {
            self.alert(wid, ALERT_ACTIVITY);
        }
        self.mark_window_dirty(wid);
    }

    /// Flags a window in every session where it is not the current window.
    pub fn alert(&mut self, wid: WinId, flag: u8) {
        let links = self.links_of(wid);
        let mut hooks = Vec::new();
        for (sid, idx) in links {
            let s = self.sessions.get_mut(&sid).unwrap();
            let attached_here = self.clients.values().any(|c| c.attached && c.session == Some(sid));
            if s.active == idx && attached_here {
                continue;
            }
            let e = s.alerts.entry(idx).or_insert(0);
            if *e & flag == 0 {
                *e |= flag;
                let (opt, visual, hook) = match flag {
                    ALERT_BELL => ("bell-action", "visual-bell", "alert-bell"),
                    ALERT_ACTIVITY => ("activity-action", "visual-activity", "alert-activity"),
                    _ => ("silence-action", "visual-silence", "alert-silence"),
                };
                hooks.push((sid, idx, opt, visual, hook));
            }
        }
        for (sid, idx, opt, visual, hook) in hooks {
            let action = self.opt_session(Some(sid), opt);
            if action == "none" {
                continue;
            }
            let vis = self.opt_session(Some(sid), visual);
            let what = match flag {
                ALERT_BELL => "Bell",
                ALERT_ACTIVITY => "Activity",
                _ => "Silence",
            };
            let cids: Vec<ClientId> = self.clients.values().filter(|c| c.attached && c.session == Some(sid)).map(|c| c.id).collect();
            for cid in cids {
                if vis == "on" || vis == "both" {
                    self.set_message(cid, &format!("{what} in window {idx}"));
                }
                if flag == ALERT_BELL && vis != "on" {
                    if let Some(c) = self.clients.get(&cid).filter(|c| c.control == 0) {
                        c.send(S_OUTPUT, b"\x07");
                    }
                }
            }
            self.run_hook(Some(sid), hook, Ctx::default());
            self.mark_session_dirty(sid);
        }
    }

    fn pane_exited(&mut self, id: PaneId, status: Option<i32>) {
        log(&format!("pane %{id} exited with {status:?}"));
        // popup?
        let popup_owner = self.clients.values().find(|c| matches!(&c.overlay, Some(Overlay::Popup(p)) if p.pane == id)).map(|c| c.id);
        if let Some(cid) = popup_owner {
            let close = matches!(&self.clients[&cid].overlay, Some(Overlay::Popup(p)) if p.close_on_exit);
            if let Some(p) = self.panes.get_mut(&id) {
                p.dead = Some((status, now_secs()));
            }
            if close {
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.overlay = None;
                    c.dirty = true;
                }
                self.destroy_pane(id);
            }
            return;
        }
        let remain = self.opt_pane(id, "remain-on-exit");
        let keep = matches!(remain.as_str(), "on" | "key") || (matches!(remain.as_str(), "failed" | "failed-key") && status.unwrap_or(0) != 0);
        if keep {
            let fmt = self.opt_pane(id, "remain-on-exit-format");
            if let Some(p) = self.panes.get_mut(&id) {
                p.dead = Some((status, now_secs()));
            }
            let msg = self.expand_for_pane(id, &fmt);
            if let Some(p) = self.panes.get_mut(&id) {
                p.parser.process(format!("\r\n\x1b[0m{msg}").as_bytes());
            }
            if let Some(wid) = self.window_of(id) {
                self.mark_window_dirty(wid);
            }
            return;
        }
        let ctx = Ctx { pane: Some(id), ..Ctx::default() };
        let sid = self.session_of_pane(id).map(|(s, _)| s);
        self.run_hook(sid, "pane-exited", ctx);
        self.kill_pane(id);
    }

    fn job_done(&mut self, id: u32, status: i32, output: String) {
        let Some(parked) = self.jobs.remove(&id) else { return };
        let mut queue = parked.queue;
        match parked.kind {
            JobKind::RunShell { pane, background, .. } => {
                if !output.is_empty() && !background {
                    if queue.ctx.reply.is_some_and(|c| !self.clients.get(&c).is_some_and(|c| c.attached)) {
                        queue.ctx.out.push_str(&output);
                    } else if let Some(p) = pane.or(queue.ctx.pane) {
                        self.view_output(p, &output);
                    }
                }
                if status != 0 && !background {
                    let msg = format!("'{}' returned {status}", "run-shell");
                    if let Some(cid) = queue.ctx.client.filter(|c| self.clients.get(c).is_some_and(|c| c.attached)) {
                        self.set_message(cid, &msg);
                    }
                }
            }
            JobKind::IfShell { then, otherwise } => {
                let branch = if status == 0 { Some(then) } else { otherwise };
                if let Some(b) = branch {
                    match crate::cmdparse::parse(&b) {
                        Ok(cmds) => {
                            for c in cmds.into_iter().rev() {
                                queue.cmds.push_front(c);
                            }
                        }
                        Err(e) => {
                            self.finish_queue(queue, Some(e));
                            return;
                        }
                    }
                }
            }
        }
        self.run_queue(queue);
    }

    /// Shows command output in a pane in view mode (copy mode over the text), like tmux.
    pub fn view_output(&mut self, pane: PaneId, text: &str) {
        let Some(p) = self.panes.get_mut(&pane) else { return };
        let (w, h) = p.size;
        let mut lines: Vec<Line> = match &mut p.mode {
            Some(PaneMode::Copy(_)) => Vec::new(),
            _ => Vec::new(),
        };
        for l in text.trim_end_matches('\n').split('\n') {
            let mut cells: Vec<crate::cells::Cell> = Vec::new();
            for ch in l.chars() {
                let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if cw == 0 {
                    continue;
                }
                cells.push(crate::cells::Cell { text: ch.to_string(), style: Default::default() });
                if cw == 2 {
                    cells.push(crate::cells::Cell { text: String::new(), style: Default::default() });
                }
            }
            cells.resize(w as usize, crate::cells::Cell::default());
            cells.truncate(w as usize);
            lines.push(Line { cells, wrapped: false });
        }
        while lines.len() < h as usize {
            lines.push(Line { cells: vec![crate::cells::Cell::default(); w as usize], wrapped: false });
        }
        let vi = self.opt_window(self.window_of(pane), "mode-keys") == "vi";
        let mut cm = CopyMode::new(lines, w, h, (0, 0), vi);
        // Start at the top of the output.
        cm.run("history-top", &[]);
        if let Some(p) = self.panes.get_mut(&pane) {
            p.mode = Some(PaneMode::Copy(Box::new(cm)));
        }
        if let Some(wid) = self.window_of(pane) {
            self.mark_window_dirty(wid);
        }
    }

    pub fn next_timeout(&self) -> Duration {
        let mut t = Duration::from_millis(1000);
        let now = Instant::now();
        for d in self.copy_drag_timers.values().chain(self.delayed.iter().map(|(d, _)| d)) {
            t = t.min(d.saturating_duration_since(now));
        }
        for c in self.clients.values() {
            for d in [c.esc_deadline, c.repeat_until, c.message.as_ref().map(|m| m.1), c.click.deadline].into_iter().flatten() {
                t = t.min(d.saturating_duration_since(now));
            }
            if let Some(Overlay::DisplayPanes { until, .. }) = &c.overlay {
                t = t.min(until.saturating_duration_since(now));
            }
        }
        for d in self.animation_deadlines.borrow().values() {
            t = t.min(d.saturating_duration_since(now));
        }
        t.max(Duration::from_millis(5))
    }

    fn timers(&mut self) {
        self.input_timers();
        let now = Instant::now();
        let cids: Vec<ClientId> = self.clients.keys().copied().collect();
        for cid in cids {
            let Some(c) = self.clients.get_mut(&cid) else { continue };
            if c.esc_deadline.is_some_and(|d| d <= now) {
                c.esc_deadline = None;
                c.keyp.expire_partial_paste_end();
                let pending = c.keyp.flush_pending();
                for i in pending {
                    self.dispatch_input(cid, i);
                }
            }
            if self.clients.get(&cid).is_some_and(|c| c.click.deadline.is_some_and(|d| d <= now)) {
                self.mouse_click_timer(cid);
            }
            let Some(c) = self.clients.get_mut(&cid) else { continue };
            if c.repeat_until.is_some_and(|d| d <= now) {
                c.repeat_until = None;
                if c.key_table != "root" {
                    c.key_table = "root".into();
                    c.dirty = true;
                }
            }
            if c.message.as_ref().is_some_and(|m| m.1 <= now) {
                c.message = None;
                c.dirty = true;
            }
            if matches!(&c.overlay, Some(Overlay::DisplayPanes { until, .. }) if *until <= now) {
                c.overlay = None;
                c.dirty = true;
            }
        }
        if !self.copy_drag_timers.is_empty() {
            self.copy_drag_timers_due(now);
        }
        if self.delayed.iter().any(|(d, _)| *d <= now) {
            let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.delayed).into_iter().partition(|(d, _)| *d <= now);
            self.delayed = rest;
            for (_, q) in due {
                self.run_queue(q);
            }
        }
        let waiting = std::mem::take(&mut self.overlay_waits);
        for (cid, epoch, name, queue) in waiting {
            if self.clients.get(&cid).is_none_or(|c| c.overlay.is_none() || c.overlay_epoch != epoch) {
                self.after_command(&name, &queue.ctx);
                self.run_queue(queue);
            } else {
                self.overlay_waits.push((cid, epoch, name, queue));
            }
        }
        // Animation cycles request a one-shot status redraw every 100ms.
        self.animation_deadlines.borrow_mut().retain(|cid, deadline| {
            if *deadline > now { return self.clients.contains_key(cid); }
            if let Some(c) = self.clients.get_mut(cid) {
                if c.message.is_none() && c.prompt.is_none() { c.dirty = true; }
            }
            false
        });
        // once a second: status refresh, automatic-rename, silence
        if self.last_tick.elapsed() >= Duration::from_secs(1) {
            self.last_tick = Instant::now();
            self.tick_second();
        }
    }

    fn tick_second(&mut self) {
        self.control_subscriptions();
        self.request_pane_info();
        self.automatic_rename();
        let wids: Vec<WinId> = self.windows.keys().copied().collect();
        for wid in wids {
            // silence (alert hooks run below may have removed windows)
            if !self.windows.contains_key(&wid) {
                continue;
            }
            let secs = self.opt_window_num(Some(wid), "monitor-silence");
            if secs > 0 {
                let Some(w) = self.windows.get(&wid) else { continue };
                let quiet = w
                    .layout
                    .panes()
                    .iter()
                    .filter_map(|p| self.panes.get(p))
                    .all(|p| p.last_output.elapsed() >= Duration::from_secs(secs as u64));
                if quiet {
                    self.alert(wid, ALERT_SILENCE);
                }
            }
        }
        // status-interval redraws (clock, status-right time)
        let now = now_secs();
        let sids: Vec<SessId> = self.sessions.keys().copied().collect();
        for sid in sids {
            let iv = self.opt_session_num(Some(sid), "status-interval");
            if iv > 0 && now % iv as u64 == 0 {
                self.mark_session_dirty(sid);
            }
        }
        // clock mode panes refresh every second
        let clock: Vec<WinId> = self.panes.values().filter(|p| matches!(p.mode, Some(PaneMode::Clock))).filter_map(|p| self.window_of(p.id)).collect();
        for w in clock {
            self.mark_window_dirty(w);
        }
    }

    /// Look up what every live pane runs on a helper thread: on Windows that takes a
    /// snapshot of all processes, which can take long enough to stall input if done
    /// here. A lookup still running is not started again.
    fn request_pane_info(&mut self) {
        if self.pane_info_busy {
            return;
        }
        let mut keys = Vec::new();
        let mut req = Vec::new();
        for p in self.panes.values().filter(|p| p.dead.is_none()) {
            keys.push((p.pid, p.fg_pgrp()));
            req.push(PaneInfo { id: p.id, generation: p.generation, pid: p.pid, cwd_seq: p.cwd_seq, cmd: None, path: None });
        }
        if req.is_empty() {
            return;
        }
        let tx = self.tx.clone();
        let spawned = thread::Builder::new().name("pane-info".into()).spawn(move || {
            // always answer, so a failed lookup does not stop later ones
            let found = std::panic::catch_unwind(|| procinfo::pane_programs(&keys)).unwrap_or_default();
            for (r, (cmd, path)) in req.iter_mut().zip(found) {
                r.cmd = cmd;
                r.path = path;
            }
            let _ = tx.send(Event::PaneInfo(req));
        });
        self.pane_info_busy = spawned.is_ok();
    }

    fn pane_info(&mut self, info: Vec<PaneInfo>) {
        self.pane_info_busy = false;
        for r in info {
            // the pane may have gone or been respawned since the lookup started
            let Some(p) = self.panes.get_mut(&r.id).filter(|p| p.generation == r.generation && p.pid == r.pid && p.dead.is_none()) else {
                continue;
            };
            if let Some(c) = r.cmd {
                p.current_cmd = c;
            }
            // a directory the pane reported meanwhile is newer than this lookup
            if let Some(path) = r.path.filter(|_| p.cwd_seq == r.cwd_seq) {
                p.current_path = path;
            }
        }
        self.automatic_rename();
    }

    fn automatic_rename(&mut self) {
        let wids: Vec<WinId> = self.windows.keys().copied().collect();
        for wid in wids {
            let Some(active) = self.windows.get(&wid).map(|w| w.active) else { continue };
            if !self.opt_window_on(Some(wid), "automatic-rename") {
                continue;
            }
            let fmt = self.opt_window(Some(wid), "automatic-rename-format");
            let name = self.expand_for_pane(active, &fmt);
            if !name.is_empty() && self.windows.get(&wid).is_some_and(|w| w.name != name) {
                self.windows.get_mut(&wid).unwrap().name = name;
                self.mark_window_dirty(wid);
            }
        }
    }

    fn shutdown(&mut self) {
        let ids: Vec<PaneId> = self.panes.keys().copied().collect();
        for id in ids {
            if let Some(p) = self.panes.get_mut(&id) {
                p.kill();
                p.kill_host();
            }
        }
        for c in self.clients.values() {
            if c.attached {
                c.send_exit("[server exited]");
            } else {
                c.send(S_REPLY, &encode_reply(0, ""));
            }
        }
        thread::sleep(Duration::from_millis(150));
    }

    // ---- panes ------------------------------------------------------------------------

    pub fn session_env(&self, sid: Option<SessId>) -> Vec<(String, Option<String>)> {
        let mut env: BTreeMap<String, Option<String>> = self.env.clone();
        if let Some(s) = sid.and_then(|s| self.sessions.get(&s)) {
            for (k, v) in &s.env {
                env.insert(k.clone(), v.clone());
            }
        }
        env.into_iter().collect()
    }

    /// Spawns a process in a new pty. `cmd`: empty = default-command / shell; one arg = via shell; more = argv.
    pub fn spawn_pane(&mut self, sid: Option<SessId>, cols: u16, rows: u16, cmd: &[String], cwd: Option<&str>, extra_env: &[(String, String)]) -> Result<PaneId, String> {
        let id = self.id();
        self.spawn_into(id, 0, sid, cols, rows, cmd, cwd, extra_env)?;
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_into(&mut self, id: PaneId, generation: u32, sid: Option<SessId>, cols: u16, rows: u16, cmd: &[String], cwd: Option<&str>, extra_env: &[(String, String)]) -> Result<(), String> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        // an empty pane (new-window -E and friends) gets a terminal but no process
        let empty = matches!(cmd, [one] if one == cmds::EMPTY_PANE);
        let cmd: &[String] = if empty { &[] } else { cmd };
        let shell = self.opt_session(sid, "default-shell");
        let shell = if shell.is_empty() { procinfo::default_shell() } else { shell };
        let default_cmd = self.opt_session(sid, "default-command");
        let argv: Vec<String> = match cmd {
            [] if default_cmd.is_empty() => procinfo::interactive(&shell),
            [] => procinfo::via_shell(&shell, &default_cmd),
            [one] => procinfo::via_shell(&shell, one),
            many => many.to_vec(),
        };
        let start_cmd = cmd.join(" ");
        // tmux keeps the command given, else default-command; respawn without one keeps the old
        let start_argv = match cmd {
            [] if default_cmd.is_empty() => self.panes.get(&id).map(|p| p.track.argv.clone()).unwrap_or_default(),
            [] => vec![default_cmd.clone()],
            many => many.to_vec(),
        };
        #[cfg(windows)]
        let hosts_before = procinfo::win::children_named(std::process::id(), "conhost.exe");
        let pair = self.pty.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).map_err(|e| format!("openpty: {e}"))?;
        // panes are spawned one at a time on this thread, so the new conhost is ours
        #[cfg(windows)]
        let host_pid = procinfo::win::children_named(std::process::id(), "conhost.exe").into_iter().find(|p| !hosts_before.contains(p));
        #[cfg(not(windows))]
        let host_pid = None;
        let mut cb = CommandBuilder::new(&argv[0]);
        cb.args(&argv[1..]);
        cb.env_clear();
        for (k, v) in self.session_env(sid) {
            if let Some(v) = v {
                cb.env(k, v);
            }
        }
        let dir = cwd
            .filter(|d| !d.is_empty() && std::path::Path::new(d).is_dir())
            .map(String::from)
            .or_else(|| sid.and_then(|s| self.sessions.get(&s)).map(|s| s.cwd.clone()).filter(|d| std::path::Path::new(d).is_dir()))
            .or_else(procinfo::home_dir)
            .unwrap_or_else(|| ".".into());
        cb.cwd(&dir);
        let label = format!("{},{},{}", ipc::describe(), std::process::id(), sid.unwrap_or(0));
        cb.env("PWSH_TMUX", label);
        cb.env("PWSH_TMUX_PANE", format!("%{id}"));
        // Describe the pane's virtual terminal, independently of the program
        // that launched the server (which may have TERM=dumb on Windows).
        cb.env("TERM", self.opt_server("default-terminal"));
        cb.env("COLORTERM", "truecolor");
        cb.env("TERM_PROGRAM", "pwsh-tmux");
        cb.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        for (k, v) in extra_env {
            cb.env(k, v);
        }
        let child = if empty { None } else { Some(pair.slave.spawn_command(cb).map_err(|e| format!("create pane failed: {} ({e})", argv[0]))?) };
        drop(pair.slave);
        let pid = child.as_ref().and_then(|c| c.process_id());
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let killer = child.as_ref().map(|c| c.clone_killer());

        let tx = self.tx.clone();
        thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(Event::PaneData(id, generation, buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        if let Some(mut child) = child {
            let tx = self.tx.clone();
            thread::spawn(move || {
                let status = child.wait().ok().map(|s| s.exit_code() as i32);
                let _ = tx.send(Event::PaneExit(id, generation, status));
            });
        }
        let (itx, irx) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            for data in irx {
                if writer.write_all(&data).and_then(|_| writer.flush()).is_err() {
                    break;
                }
            }
        });
        let history = self.opt_session_num(sid, "history-limit").max(0) as usize;
        let now = now_secs();
        let prog = procinfo::program_name(&argv[0]);
        let existing_opts = self.panes.get(&id).map(|p| p.options.clone()).unwrap_or_default();
        let title = self.panes.get(&id).map(|p| p.title.clone()).unwrap_or_else(|| self.host.clone());
        let previous = self.panes.insert(
            id,
            Pane {
                id,
                generation,
                parser: vt100::Parser::new_with_callbacks(nz(rows), nz(cols), history, {
                    // screen_reinit: a new screen starts in mode 1 with extended-keys always
                    let ext_option = ext_keys_option(&self.opt_server("extended-keys"));
                    PaneCb { ext_option, ext_keys: (ext_option == 2) as u8, ..PaneCb::default() }
                }),
                input: itx,
                master: pair.master,
                killer,
                size: (cols, rows),
                pid,
                start_cmd,
                start_path: dir.clone(),
                title,
                dead: None,
                mode: None,
                options: existing_opts,
                pipe: None,
                last_output: Instant::now(),
                created: now,
                current_cmd: prog,
                current_path: dir,
                cwd_seq: 0,
                host_pid,
                track: PaneTrack { argv: start_argv, ..Default::default() },
            },
        );
        if let Some(old) = previous {
            release(old); // respawn: the old pty
        }
        Ok(())
    }

    /// Removes a pane's process and state without touching layouts.
    pub fn destroy_pane(&mut self, id: PaneId) {
        if let Some(mut p) = self.panes.remove(&id) {
            p.kill();
            release(p);
        }
        if self.marked == Some(id) {
            self.marked = None;
        }
    }

    /// Kills a pane and removes it from its window (closing the window/session if empty).
    pub fn kill_pane(&mut self, id: PaneId) {
        let wid = self.window_of(id);
        self.destroy_pane(id);
        let Some(wid) = wid else { return };
        let w = self.windows.get_mut(&wid).unwrap();
        let layout = std::mem::replace(&mut w.layout, Node::Leaf(PaneId::MAX));
        match layout.remove(id) {
            Some(n) => {
                w.layout = n;
                let panes = w.panes();
                w.last_active = w.last_active.filter(|p| panes.contains(p));
                if w.active == id {
                    w.active = w.last_active.take().unwrap_or(panes[0]);
                }
                w.zoomed = false;
                self.apply_layout(wid);
                self.mark_window_dirty(wid);
            }
            None => self.kill_window(wid),
        }
    }

    /// Kills a window everywhere it is linked.
    pub fn kill_window(&mut self, wid: WinId) {
        // re-query: unlink may renumber the remaining links
        while let Some(&(sid, idx)) = self.links_of(wid).first() {
            self.unlink(sid, idx);
        }
        if let Some(w) = self.windows.remove(&wid) {
            for p in w.panes() {
                self.destroy_pane(p);
            }
        }
    }

    /// Removes a window link from a session; destroys the session if it becomes empty,
    /// and the window if it is no longer linked anywhere.
    pub fn unlink(&mut self, sid: SessId, idx: u32) {
        let Some(s) = self.sessions.get_mut(&sid) else { return };
        let Some(wid) = s.links.remove(&idx) else { return };
        s.alerts.remove(&idx);
        s.last.retain(|&i| i != idx);
        if s.links.is_empty() {
            self.kill_session(sid, true);
        } else {
            if s.active == idx {
                s.active = s.last.first().copied().filter(|i| s.links.contains_key(i)).unwrap_or_else(|| {
                    s.links.range(..idx).next_back().or(s.links.range(idx..).next()).map(|(i, _)| *i).unwrap()
                });
                s.last.retain(|&i| i != s.active);
            }
            if options::is_on(self.sessions[&sid].options.get("renumber-windows").or(self.opts_session.get("renumber-windows")).unwrap_or("off")) {
                self.renumber(sid);
            }
            self.mark_session_dirty(sid);
            let ctx = Ctx::default();
            self.run_hook(Some(sid), "window-unlinked", ctx);
        }
        if self.links_of(wid).is_empty() {
            if let Some(w) = self.windows.remove(&wid) {
                for p in w.panes() {
                    self.destroy_pane(p);
                }
            }
        }
    }

    /// Propagates window-list changes between the sessions of each group: the member
    /// whose links differ from the group's last synced list is the one that changed.
    pub fn sync_groups(&mut self) {
        let gids: Vec<u32> = self.groups.keys().copied().collect();
        for gid in gids {
            let members: Vec<SessId> = self.sessions.values().filter(|s| s.group == Some(gid)).map(|s| s.id).collect();
            if members.len() < 2 {
                for m in &members {
                    self.sessions.get_mut(m).unwrap().group = None;
                }
                self.groups.remove(&gid);
                continue;
            }
            let canonical = self.groups[&gid].clone();
            let Some(changed) = members.iter().find(|m| self.sessions[m].links != canonical).copied() else { continue };
            let links = self.sessions[&changed].links.clone();
            for m in &members {
                let s = self.sessions.get_mut(m).unwrap();
                if s.links == links {
                    continue;
                }
                s.links = links.clone();
                s.last.retain(|i| s.links.contains_key(i));
                s.alerts.retain(|i, _| s.links.contains_key(i));
                if !s.links.contains_key(&s.active) {
                    s.active = s.last.first().copied().or_else(|| s.links.keys().next().copied()).unwrap_or(0);
                }
                let id = s.id;
                self.mark_session_dirty(id);
            }
            self.groups.insert(gid, links.clone());
            // windows that left the group's list and are linked nowhere else
            for wid in canonical.values() {
                if !links.values().any(|w| w == wid) && self.windows.contains_key(wid) && self.links_of(*wid).is_empty() {
                    if let Some(w) = self.windows.remove(wid) {
                        for p in w.panes() {
                            self.destroy_pane(p);
                        }
                    }
                }
            }
            if links.is_empty() {
                for m in members {
                    self.kill_session(m, true);
                }
            } else {
                for m in members {
                    self.update_session_size(m);
                }
            }
        }
    }

    pub fn renumber(&mut self, sid: SessId) {
        let base = self.opt_session_num(Some(sid), "base-index").max(0) as u32;
        let s = self.sessions.get_mut(&sid).unwrap();
        let old: Vec<(u32, WinId)> = s.links.iter().map(|(i, w)| (*i, *w)).collect();
        let mut map = HashMap::new();
        s.links.clear();
        for (n, (i, w)) in old.into_iter().enumerate() {
            let ni = base + n as u32;
            map.insert(i, ni);
            s.links.insert(ni, w);
        }
        s.active = map[&s.active];
        s.last = s.last.iter().filter_map(|i| map.get(i).copied()).collect();
        s.alerts = std::mem::take(&mut s.alerts).into_iter().filter_map(|(i, a)| map.get(&i).map(|n| (*n, a))).collect();
    }

    pub fn kill_session(&mut self, sid: SessId, _empty: bool) {
        let Some(s) = self.sessions.remove(&sid) else { return };
        let detach_mode = s.options.get("detach-on-destroy").map(String::from).unwrap_or_else(|| self.opt_session(None, "detach-on-destroy"));
        for (_, wid) in &s.links {
            if self.links_of(*wid).is_empty() {
                if let Some(w) = self.windows.remove(wid) {
                    for p in w.panes() {
                        self.destroy_pane(p);
                    }
                }
            }
        }
        let cids: Vec<ClientId> = self.clients.values().filter(|c| c.session == Some(sid)).map(|c| c.id).collect();
        for cid in cids {
            let next = match detach_mode.as_str() {
                "off" | "no-detached" | "previous" | "next" => self
                    .clients
                    .get(&cid)
                    .and_then(|c| c.last_session)
                    .filter(|s| self.sessions.contains_key(s))
                    .or_else(|| self.current_session()),
                _ => None,
            };
            match next {
                Some(n) => self.switch_client(cid, n),
                None => {
                    if let Some(c) = self.clients.get_mut(&cid) {
                        c.session = None;
                        c.attached = false;
                        c.send_exit("[exited]");
                    }
                }
            }
        }
        for c in self.clients.values_mut() {
            if c.last_session == Some(sid) {
                c.last_session = None;
            }
        }
        let ctx = Ctx::default();
        self.run_hook(None, "session-closed", ctx);
    }

    fn check_destroy_unattached(&mut self, sid: SessId) {
        if self.opt_session_on(Some(sid), "destroy-unattached") && !self.clients.values().any(|c| c.attached && c.session == Some(sid)) {
            self.kill_session(sid, false);
        }
    }

    pub fn attach_client(&mut self, cid: ClientId, sid: SessId) {
        let clear = options::is_on(&self.opt_server("clear-on-attach"));
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let was_attached = c.attached;
        if c.session != Some(sid) {
            c.last_session = c.session;
        }
        c.session = Some(sid);
        c.attached = true;
        c.prev = None;
        c.dirty = true;
        c.key_table = "root".into();
        if !was_attached {
            c.send(S_ATTACHED, if clear { b"" } else { b"no-clear" });
            // tty_send_requests: ask a VT100-like terminal whether it has synchronized output
            // (DECRQM 2026); the reply adds the sync feature
            let term = c.ident.as_ref().map(|i| i.term.as_str()).unwrap_or("");
            if c.control == 0 && !term.is_empty() && term != "dumb" {
                c.send(S_OUTPUT, b"\x1b[?2026$p");
            }
        }
        let writable = !self.client_is_readonly(Some(cid));
        let now = now_secs();
        // update-environment
        let names: Vec<String> = self.opt_array(Some(sid), "update-environment");
        let env: Vec<(String, String)> = self.clients.get(&cid).and_then(|c| c.ident.as_ref()).map(|i| i.env.clone()).unwrap_or_default();
        if let Some(s) = self.sessions.get_mut(&sid) {
            s.last_attached = now;
            s.activity = now;
            for n in names.iter().filter(|_| writable) {
                match env.iter().find(|(k, _)| k == n) {
                    Some((_, v)) => {
                        s.env.insert(n.clone(), Some(v.clone()));
                    }
                    None => {
                        s.env.insert(n.clone(), None);
                    }
                }
            }
            let active = s.active;
            if writable { s.alerts.remove(&active); }
        }
        self.update_session_size(sid);
        if !was_attached {
            self.run_hook(Some(sid), "client-attached", Ctx { client: Some(cid), ..Ctx::default() });
        } else {
            self.run_hook(Some(sid), "client-session-changed", Ctx { client: Some(cid), ..Ctx::default() });
        }
    }

    pub fn opt_array(&self, sid: Option<SessId>, name: &str) -> Vec<String> {
        let prefix = format!("{name}[");
        let src = sid.and_then(|s| self.sessions.get(&s)).map(|s| &s.options).filter(|o| o.has(name)).unwrap_or(&self.opts_session);
        src.0.iter().filter(|(k, _)| k.starts_with(&prefix)).flat_map(|(_, v)| v.split_whitespace().map(String::from).collect::<Vec<_>>()).collect()
    }

    pub fn switch_client(&mut self, cid: ClientId, sid: SessId) {
        let old = self.clients.get(&cid).and_then(|c| c.session);
        self.attach_client(cid, sid);
        if let Some(o) = old.filter(|o| *o != sid) {
            self.update_session_size(o);
            self.check_destroy_unattached(o);
        }
    }

    pub fn detach_client(&mut self, cid: ClientId, msg: &str) {
        let Some(c) = self.clients.get_mut(&cid) else { return };
        if !c.attached {
            return;
        }
        let sid = c.session;
        c.attached = false;
        c.session = None;
        c.send_exit(msg);
        let readonly = c.is_readonly();
        if let Some(sid) = sid {
            self.update_session_size(sid);
            self.run_hook(Some(sid), "client-detached", Ctx { readonly, ..Ctx::default() });
            self.check_destroy_unattached(sid);
        }
    }

    // ---- layout & sizes -------------------------------------------------------------------

    /// Status lines a session shows.
    pub fn status_lines(&self, sid: Option<SessId>) -> u16 {
        match self.opt_session(sid, "status").as_str() {
            "off" => 0,
            "on" => 1,
            n => n.parse().unwrap_or(1),
        }
    }

    /// Area for panes inside a window of size (sx, sy).
    pub fn window_area(&self, wid: WinId) -> Rect {
        let w = &self.windows[&wid];
        Rect { x: 0, y: 0, w: w.sx.max(1), h: w.sy.max(1) }
    }

    /// Pane rects within the window (zoom aware); border-status rows are reserved inside rects.
    pub fn rects(&self, wid: WinId) -> (Vec<(PaneId, Rect)>, Vec<crate::layout::Sep>) {
        let w = &self.windows[&wid];
        let area = self.window_area(wid);
        if w.zoomed {
            return (vec![(w.active, area)], vec![]);
        }
        let (mut panes, mut seps) = (vec![], vec![]);
        w.layout.layout(area, &mut panes, &mut seps);
        (panes, seps)
    }

    /// Content rect of a pane (minus a pane-border-status line).
    pub fn content_rect(&self, wid: WinId, r: Rect) -> Rect {
        match self.opt_window(Some(wid), "pane-border-status").as_str() {
            "top" if r.y == 0 && r.h > 1 => Rect { y: r.y + 1, h: r.h - 1, ..r },
            "bottom" if r.bottom() == self.windows[&wid].sy && r.h > 1 => Rect { h: r.h - 1, ..r },
            _ => r,
        }
    }

    pub fn apply_layout(&mut self, wid: WinId) {
        if !self.windows.contains_key(&wid) {
            return;
        }
        let (rects, _) = self.rects(wid);
        for (id, r) in rects {
            let r = self.content_rect(wid, r);
            if let Some(p) = self.panes.get_mut(&id) {
                p.resize(r.w, r.h);
            }
        }
    }

    /// Window sizes follow the attached clients (window-size: latest/smallest/largest/manual).
    pub fn update_session_size(&mut self, sid: SessId) {
        let Some(s) = self.sessions.get(&sid) else { return };
        let status = self.status_lines(Some(sid));
        // control clients only count once they set a size (refresh-client -C)
        let clients: Vec<(u64, (u16, u16))> = self
            .clients
            .values()
            .filter(|c| c.attached && c.session == Some(sid) && !c.is_readonly())
            .filter_map(|c| if c.control > 0 { c.control_size.filter(|_| !c.ctl.ignore_size).map(|s| (c.activity, s)) } else { Some((c.activity, c.size)) })
            .collect();
        let latest = clients.iter().max_by_key(|c| c.0).map(|c| c.1);
        let smallest = clients.iter().map(|c| c.1).reduce(|a, b| (a.0.min(b.0), a.1.min(b.1)));
        let largest = clients.iter().map(|c| c.1).reduce(|a, b| (a.0.max(b.0), a.1.max(b.1)));
        let links: Vec<WinId> = s.links.values().copied().collect();
        let mut size = None;
        for wid in links {
            let mode = self.opt_window(Some(wid), "window-size");
            let aggressive = self.opt_window_on(Some(wid), "aggressive-resize");
            let pick = match mode.as_str() {
                "manual" => None,
                "smallest" => smallest,
                "largest" => largest,
                _ => latest,
            };
            let _ = aggressive;
            let Some((cols, rows)) = pick else { continue };
            size = Some((cols, rows));
            let (sx, sy) = (cols.max(1), rows.saturating_sub(status).max(1));
            let w = self.windows.get_mut(&wid).unwrap();
            if (w.sx, w.sy) != (sx, sy) {
                w.sx = sx;
                w.sy = sy;
                self.apply_layout(wid);
            }
        }
        if let (Some((c, r)), Some(s)) = (size, self.sessions.get_mut(&sid)) {
            s.cols = c;
            s.rows = r;
        }
        self.mark_session_dirty(sid);
    }

    // ---- hooks & queue ---------------------------------------------------------------------

    pub fn run_hook(&mut self, sid: Option<SessId>, name: &str, ctx: Ctx) {
        if ctx.hook_depth > 4 || ctx.readonly || self.readonly_context.get() || self.client_is_readonly(ctx.client) {
            return;
        }
        let cmds = sid
            .and_then(|s| self.sessions.get(&s))
            .and_then(|s| s.hooks.get(name))
            .or_else(|| self.hooks.get(name))
            .cloned()
            .unwrap_or_default();
        if cmds.is_empty() {
            return;
        }
        let mut list = VecDeque::new();
        for c in cmds {
            match crate::cmdparse::parse(&c) {
                Ok(v) => list.extend(v),
                Err(e) => log(&format!("hook {name}: {e}")),
            }
        }
        let ctx = Ctx { reply: None, out: Default::default(), hook_depth: ctx.hook_depth + 1, ..ctx };
        self.run_queue(Queue { cmds: list, ctx , soft: 0 });
    }

    fn after_command(&mut self, name: &str, ctx: &Ctx) {
        self.sync_groups();
        if ctx.hook_depth == 0 {
            if let Some(full) = cmds::canonical(name) {
                let sid = ctx.pane.and_then(|p| self.session_of_pane(p)).map(|(s,_)|s)
                    .or_else(|| ctx.client.and_then(|c| self.client_session(c)));
                self.run_hook(sid, &format!("after-{full}"), Ctx {
                    client: ctx.client, pane: ctx.pane, readonly: ctx.readonly, ..Ctx::default()
                });
            }
        }
    }

    pub fn run_queue(&mut self, mut q: Queue) {
        while let Some(cmd) = q.cmds.pop_front() {
            let name = cmd.first().map(|s| s.strip_prefix("\0alias:").unwrap_or(s).to_string()).unwrap_or_default();
            let soft = q.soft > 0;
            q.soft = q.soft.saturating_sub(1);
            if name == FILE_PUSH {
                q.ctx.files.push(cmd.get(1).cloned().unwrap_or_default());
                continue;
            }
            if name == FILE_POP {
                q.ctx.files.pop();
                continue;
            }
            let delay_cmd = (cmds::canonical(&name) == Some("run-shell")).then(|| cmd.clone());
            let saved_mouse = std::mem::replace(&mut self.cur_mouse, q.ctx.mouse.clone());
            let saved_file = std::mem::replace(&mut self.cur_file, q.ctx.files.last().cloned());
            let result = self.exec(cmd, &mut q.ctx);
            self.cur_mouse = saved_mouse;
            self.cur_file = saved_file;
            match result {
                Ok(Flow::Inject(cmds, soft_errors)) => {
                    let n = cmds.len();
                    for c in cmds.into_iter().rev() {
                        q.cmds.push_front(c);
                    }
                    q.soft = if soft_errors { q.soft + n } else { q.soft };
                }
                Err(e) if soft && !e.starts_with('\0') => {
                    log(&format!("{e}"));
                    self.add_message(&e);
                    q.ctx.out.push_str(&e);
                    q.ctx.out.push('\n');
                }
                Ok(Flow::WaitOverlay(cid, epoch)) => {
                    self.overlay_waits.push((cid, epoch, name, q));
                    return;
                }
                Ok(Flow::Next) => self.after_command(&name, &q.ctx),
                Err(e) if e.starts_with('\0') => {
                    // async commands: park the rest of the queue
                    let parts: Vec<String> = e.split('\0').map(String::from).collect();
                    let get = |i: usize| parts.get(i).cloned().unwrap_or_default();
                    match get(1).as_str() {
                        "job-if" => {
                            let kind = JobKind::IfShell { then: get(3), otherwise: Some(get(4)).filter(|s| !s.is_empty()) };
                            if get(5) == "b" {
                                let sub = Queue { cmds: VecDeque::new(), ctx: Ctx { reply: None, out: Default::default(), ..q.ctx.clone() }, soft: 0 };
                                self.spawn_job(&get(2), None, kind, sub);
                                continue;
                            }
                            self.spawn_job(&get(2), None, kind, q);
                            return;
                        }
                        "job-run" => {
                            let background = get(4).contains('b');
                            let pane = get(5).parse().ok();
                            let cwd = Some(get(3)).filter(|s| !s.is_empty());
                            let kind = JobKind::RunShell { pane, background, stderr: get(4).contains('E') };
                            if background {
                                let sub = Queue { cmds: VecDeque::new(), ctx: Ctx { reply: None, out: Default::default(), ..q.ctx.clone() }, soft: 0 };
                                self.spawn_job(&get(2), cwd, kind, sub);
                                continue;
                            }
                            self.spawn_job(&get(2), cwd, kind, q);
                            return;
                        }
                        "wait" => {
                            self.waits.entry(get(2)).or_insert((false, Vec::new())).1.push(q);
                            return;
                        }
                        "delay" => {
                            // run-shell -d: the rest of the queue (and the command) runs later
                            let ms: u64 = get(2).parse().unwrap_or(0);
                            if let Some(c) = delay_cmd.as_ref().map(|c| strip_delay(c)) {
                                q.cmds.push_front(c);
                            }
                            self.delayed.push((Instant::now() + Duration::from_millis(ms), q));
                            return;
                        }
                        _ => {
                            self.finish_queue(q, Some("internal error".into()));
                            return;
                        }
                    }
                }
                Err(e) => {
                    self.finish_queue(q, Some(e));
                    return;
                }
            }
        }
        self.finish_queue(q, None);
    }

    pub fn finish_queue(&mut self, q: Queue, mut err: Option<String>) {
        let ctx = q.ctx;
        if ctx.out.overflowed() { err = Some("command output limit exceeded".into()); }
        if let (Some(seq), Some(cid)) = (ctx.control_seq, ctx.client) {
            self.control_block(cid, seq, &ctx.out, err.as_deref());
            // a one-shot `pwsh-tmux -C command` that didn't attach ends here
            if let Some(c) = ctx.reply.and_then(|r| self.clients.get(&r)).filter(|c| !c.attached) {
                c.send(S_OUTPUT, b"%exit\n");
                c.send(S_REPLY, &encode_reply(if err.is_some() { 1 } else { 0 }, ""));
            }
            return;
        }
        let attached = ctx.client.and_then(|c| self.clients.get(&c)).is_some_and(|c| c.attached);
        if let Some(rc) = ctx.reply {
            let Some(c) = self.clients.get(&rc) else { return };
            if c.attached {
                if !ctx.out.is_empty() {
                    if ctx.readonly || c.is_readonly() {
                        self.set_message(rc, &ctx.out);
                    } else if let Some(p) = ctx.client.and_then(|c| self.client_active_pane(c)) {
                        self.view_output(p, &ctx.out);
                    }
                }
                if let Some(e) = err {
                    self.set_message(rc, &e);
                }
                return;
            }
            match err {
                Some(e) => c.send(S_REPLY, &encode_reply(1, &format!("{}{e}", &*ctx.out))),
                None => c.send(S_REPLY, &encode_reply(0, &ctx.out)),
            }
            return;
        }
        if let Some(e) = &err {
            log(&format!("command error: {e}"));
            self.add_message(e);
        }
        if attached {
            let cid = ctx.client.unwrap();
            if let Some(e) = err {
                self.set_message(cid, &e);
            } else if !ctx.out.is_empty() {
                if ctx.readonly || self.client_is_readonly(Some(cid)) {
                    self.set_message(cid, &ctx.out);
                } else if let Some(p) = self.client_active_pane(cid) {
                    self.view_output(p, &ctx.out);
                }
            }
        }
    }

    pub fn client_active_pane(&self, cid: ClientId) -> Option<PaneId> {
        let sid = self.client_session(cid)?;
        let wid = self.sessions[&sid].active_window();
        Some(self.windows[&wid].active)
    }

    pub fn spawn_job(&mut self, command: &str, cwd: Option<String>, kind: JobKind, queue: Queue) -> u32 {
        let id = self.id();
        let tx = self.tx.clone();
        let mut cmd = procinfo::job_command(command);
        if let Some(d) = cwd.filter(|d| std::path::Path::new(d).is_dir()) {
            cmd.current_dir(d);
        }
        cmd.stdin(std::process::Stdio::null());
        if !matches!(kind, JobKind::RunShell { stderr: true, .. }) {
            cmd.stderr(std::process::Stdio::null());
        }
        thread::spawn(move || {
            let (status, out) = match cmd.output() {
                Ok(o) => (o.status.code().unwrap_or(1), String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr)),
                Err(e) => (127, format!("{e}\n")),
            };
            let _ = tx.send(Event::Job(id, status, out));
        });
        self.jobs.insert(id, Parked { queue, kind });
        id
    }
}

#[cfg(test)]
mod security_tests;

/// A `run-shell` command without its `-d` delay (applied by parking the queue).
fn strip_delay(cmd: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(cmd.len());
    let mut it = cmd.iter();
    if let Some(name) = it.next() {
        out.push(name.clone());
    }
    let mut options = true;
    while let Some(a) = it.next() {
        if options && a == "--" {
            options = false;
            out.push(a.clone());
            continue;
        }
        if options && a.len() > 1 && a.starts_with('-') {
            let flags = &a[1..];
            if let Some(i) = flags.find('d') {
                // flags before d are kept; the value is the rest of this word or the next one
                let before = &flags[..i];
                if !before.is_empty() {
                    out.push(format!("-{before}"));
                }
                if flags.len() == i + 1 {
                    it.next();
                }
                continue;
            }
            let takes_value = flags.ends_with(['c', 's', 't']);
            out.push(a.clone());
            if takes_value && let Some(v) = it.next() {
                out.push(v.clone());
            }
            continue;
        }
        options = false;
        out.push(a.clone());
    }
    out
}

#[cfg(test)]
mod input_tests;
