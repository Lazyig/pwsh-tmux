//! The server process: sessions, windows, panes (PTY + terminal emulator), clients,
//! key bindings, options, buffers and the command queue.
//!
//! All state lives on the main loop thread; PTY readers, child waiters, shell jobs
//! and client connections run on helper threads that only send `Event`s.

mod cmds;
mod control;
mod copy;
mod events;
mod float;
/// The JSON subset tmux parses (layouts, display-message -j).
pub use cmds::json::{parse as json_parse, Node as Json};
pub use cmds::strerror;
pub mod features;
mod termcap;
mod terminfo;
mod input;
mod modes;
mod mouse;
pub mod procinfo;
mod display;
mod pan;
mod render;
mod vars;
mod transport;
mod output;
mod theme;

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

/// Largest window/client dimension accepted (tmux WINDOW_MAXIMUM). Screen rows are
/// allocated as they are written, so a large but empty window costs little.
pub const MAX_SIZE: u16 = 10000;
/// A message without a timer (delay 0) stays until a key press.
pub const MESSAGE_FOREVER: Duration = Duration::from_secs(86_400 * 365);

pub type PaneId = u32;
pub type WinId = u32;
pub type SessId = u32;
pub type ClientId = u32;
/// The environment level of the global environment (session ids start at 0).
pub const GLOBAL_ENV: SessId = SessId::MAX - 1;

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

/// A counter ordering session activity updates made within the same second (tmux
/// compares the timevals, so the session used last is the current one).
pub fn activity_seq() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

/// Microseconds since the epoch, for activity times tmux keeps as a timeval.
pub fn now_us() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0)
}

pub enum Event {
    Conn(Stream),
    ClientMsg(ClientId, u8, Vec<u8>),
    ClientGone(ClientId),
    PaneData(PaneId, u32, Vec<u8>),
    /// The exit status: negative for a signal (pane_dead_signal), see [`exit_status`].
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

/// screen_reinit as terminal input: any sequence in progress is cancelled, the alternate
/// screen left, the scroll region, modes (origin, insert, cursor keys, keypad, cursor,
/// wrap, bracketed paste, mouse, focus) and attributes reset and the visible lines
/// cleared with the cursor at the top left; the history stays.
const SCREEN_REINIT: &[u8] = b"\x18\x1b[?1049l\x1b[r\x1b[?6l\x1b[4l\x1b[?1l\x1b>\x1b[?25h\x1b[?7h\x1b[?2004l\
\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1004l\x1b[0m\x1b[H\x1b[2J";

/// Answers terminal queries and collects title / bell / cwd reports from a pane.
#[derive(Default)]
pub struct PaneCb {
    pub colours: theme::PaneColours,
    pub replies: Vec<u8>,
    pub bell: bool,
    pub title: Option<String>,
    /// ESC k name ST, for allow-rename.
    pub rename: Option<String>,
    pub current_title: String,
    pub title_stack: VecDeque<String>,
    pub suppress_title: bool,
    pub cwd: Option<String>,
    /// The pane's extended key mode (0, 1 or 2: tmux's MODE_KEYS_EXTENDED and _2), set by
    /// CSI > 4 ; n m and CSI > 4 n, and by extended-keys always at a reset.
    pub ext_keys: u8,
    /// extended-keys as the parser last saw it (0 off, 1 on, 2 always).
    pub ext_option: u8,
    /// The pane's cursor-style option (DECSCUSR number), for DECRQM 12 and DECRQSS.
    pub cursor_style_opt: u16,
    pub clipboard_queries: Vec<(Vec<u8>, bool)>,
    /// OSC 52 selections set by the application (selector, base64 data).
    pub clipboard_sets: Vec<(Vec<u8>, Vec<u8>)>,
    pub passthrough: Vec<Vec<u8>>,
    /// State reported for formats (pane_path, cursor_colour, pane_pb_*, pane_command_*).
    pub osc: OscState,
    /// Output events in parsing order, with the state or data each carried.
    pub events: Vec<events::OutputEvent>,
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
            // screen_set_path(untrusted): clean_name keeps the old path for one that is not
            // valid and turns "#(" into "_(" (on Windows a backslash stays, as in titles)
            b"7" => {
                let raw = rest.join(&b';');
                let clean = |p: &str| if cfg!(windows) { cmds::check_name(p).then(|| p.to_string()) } else { cmds::clean_title(p) };
                if let Some(p) = std::str::from_utf8(&raw).ok().and_then(clean) {
                    self.path = p.replace("#(", "_(");
                }
            }
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

/// The signal number in a pane's exit status (see exit_status).
pub fn exit_signal(status: i32) -> Option<i32> {
    (cfg!(unix) && status < 0).then_some(-status)
}

/// The exit status in OSC 133 "D;status[;key=value...]": 0 when absent or a key=value, 255
/// when not a number from 0 to 255 (tmux's input_osc_133_exit_status).
fn osc133_exit_status(arg: &str) -> i32 {
    let Some(rest) = arg.get(1..).and_then(|r| r.strip_prefix(';')) else { return 0 };
    let field = rest.split(';').next().unwrap_or("");
    if field.is_empty() || field.contains('=') {
        return 0;
    }
    // strtonum (strtoll) skips leading white space
    let field = field.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
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
        vt100::Color::Idx(i) | vt100::Color::Palette(i) => format!("colour{i}"),
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
        // its place among the output events (pane-set-clipboard); input_osc_52 sets it
        if self.clipboard_sets.len() < 16 {
            self.clipboard_sets.push((ty.to_vec(), data.to_vec()));
            self.events.push(events::OutputEvent::Clipboard);
        }
    }
    fn dcs(&mut self, screen: &mut vt100::Screen, payload: &[u8]) {
        // DECRQSS (DCS $ q Pt ST, input_handle_decrqss): only the cursor style (SP q) is
        // known; anything else is answered as not recognized
        let data = payload.iter().position(|b| !(0x30..=0x3f).contains(b)).map_or(&payload[payload.len()..], |i| &payload[i..]);
        if let Some(pt) = data.strip_prefix(b"$q") {
            if pt.starts_with(b" q") {
                use vt100::CursorStyle as C;
                let blink = screen.cursor_blinking().0;
                let ps = match screen.cursor_style() {
                    C::BlinkBlock | C::SteadyBlock => if blink { 1 } else { 2 },
                    C::BlinkUnderline | C::SteadyUnderline => if blink { 3 } else { 4 },
                    C::BlinkVerticalBar | C::SteadyVerticalBar => if blink { 5 } else { 6 },
                    // no style set: the cursor-style option
                    C::Default => self.cursor_style_opt.min(6),
                };
                self.replies.extend(format!("\x1bP1$r q{ps} q\x1b\\").bytes());
            } else {
                self.replies.extend(b"\x1bP0$r\x1b\\");
            }
            return;
        }
        // the parser already turned the doubled ESC bytes back into one
        if let Some(payload) = payload.strip_prefix(b"tmux;") {
            self.passthrough.push(payload.to_vec());
        }
    }
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
        self.events.push(events::OutputEvent::Osc("pane-bell", self.osc.clone()));
    }
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        if self.suppress_title {
            return;
        }
        let title = String::from_utf8_lossy(title).into_owned();
        self.current_title = title.clone();
        self.events.push(events::OutputEvent::Title(title.clone()));
        self.title = Some(title);
    }
    fn reset(&mut self, _: &mut vt100::Screen) {
        // screen_write_reset: extended-keys always puts the pane back in mode 1
        self.ext_keys = (self.ext_option == 2) as u8;
        // colour_palette_clear: the pane's OSC 10/11 colours go back to the default
        self.colours.reset();
    }
    fn theme_subscription(&mut self, _: &mut vt100::Screen) { self.colours.subscribe(); }
    fn rename_window(&mut self, _: &mut vt100::Screen, name: &[u8]) {
        self.rename = Some(String::from_utf8_lossy(name).into_owned());
    }
    fn unhandled_osc_terminated(&mut self, _screen: &mut vt100::Screen, params: &[&[u8]], bel: bool) {
        self.colours.osc(params, bel, &mut self.replies);
        self.osc.osc(params);
        match params {
            // input_osc_133: vt100 marks the cursor's line (prompt, command, output start
            // and end); A/N, C and D fire pane-shell-prompt, pane-command-started and -finished
            [b"133", arg, ..] => {
                let name = match arg.first() {
                    Some(b'A' | b'N') => Some("pane-shell-prompt"),
                    Some(b'C') => Some("pane-command-started"),
                    Some(b'D') => Some("pane-command-finished"),
                    _ => None,
                };
                if let Some(name) = name {
                    self.events.push(events::OutputEvent::Osc(name, self.osc.clone()));
                }
            }
            [b"7", url, ..] => self.cwd = decode_file_url(&String::from_utf8_lossy(url)),
            [b"9", b"9", path, ..] => self.cwd = Some(String::from_utf8_lossy(path).trim_matches('"').to_string()),
            _ => {}
        }
    }
    fn unhandled_csi(&mut self, screen: &mut vt100::Screen, i1: Option<u8>, _i2: Option<u8>, params: &[&[u16]], c: char) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c, first) {
            // DECRQM for DEC private modes (INPUT_CSI_QUERY_PRIVATE): 1 set, 2 reset,
            // 4 permanently reset, 0 unknown
            (Some(b'?'), 'p', m) if _i2 == Some(b'$') && m > 0 => {
                use vt100::MouseProtocolMode as M;
                let on = |v: bool| if v { 1 } else { 2 };
                let mouse = screen.mouse_protocol_mode();
                let n = match m {
                    1 => on(screen.application_cursor()),
                    3 => 4,
                    6 => on(screen.origin_mode()),
                    7 => on(screen.wrap_mode()),
                    12 => {
                        let (blink, set) = screen.cursor_blinking();
                        if screen.cursor_style() != vt100::CursorStyle::Default || set {
                            on(blink)
                        } else {
                            // blinking for the odd cursor-style values
                            on(self.cursor_style_opt % 2 == 1)
                        }
                    }
                    25 => on(!screen.hide_cursor()),
                    47 | 1047 | 1049 => on(screen.alternate_screen()),
                    1000 => on(matches!(mouse, M::PressRelease | M::Press)),
                    1002 => on(mouse == M::ButtonMotion),
                    1003 => on(mouse == M::AnyMotion),
                    1004 => on(screen.focus_events()),
                    1005 => on(screen.mouse_encodings().0),
                    1006 => on(screen.mouse_encodings().1),
                    2004 => on(screen.bracketed_paste()),
                    2026 => on(screen.synchronized_output()),
                    2031 => on(screen.theme_updates()),
                    _ => 0,
                };
                self.replies.extend(format!("\x1b[?{m};{n}$y").bytes());
            }
            // DECRQM for ANSI modes (INPUT_CSI_QUERY): only IRM is known
            (Some(b'$'), 'p', m) if _i2.is_none() && m > 0 => {
                let n = if m == 4 { if screen.insert_mode() { 1 } else { 2 } } else { 0 };
                self.replies.extend(format!("\x1b[{m};{n}$y").bytes());
            }
            // XTVERSION
            (Some(b'>'), 'q', 0) => self.replies.extend(format!("\x1bP>|tmux {}\x1b\\", TMUX_VERSION).bytes()),
            (Some(b'?'), 'n', 996) => self.colours.report(&mut self.replies),
            // window operations (input_csi_dispatch_winops), each with its arguments;
            // vt100 itself handles 8 (resize)
            (None, 't', _) => {
                let arg = |i: usize| params.get(i).and_then(|p| p.first()).copied();
                let (rows, cols) = screen.size();
                let (y, x) = (u32::from(rows.get()), u32::from(cols.get()));
                // tmux's default cell size when the terminal does not report pixels
                let (xpixel, ypixel) = (16, 32);
                let mut m = 0;
                while let Some(n) = arg(m) {
                    match n {
                        3 | 4 | 8 => {
                            m += 2;
                            if arg(m - 1).is_none() || arg(m).is_none() {
                                return;
                            }
                        }
                        9 | 10 => {
                            m += 1;
                            if arg(m).is_none() {
                                return;
                            }
                        }
                        14 => self.replies.extend(format!("\x1b[4;{};{}t", y * ypixel, x * xpixel).bytes()),
                        15 => self.replies.extend(format!("\x1b[5;{};{}t", y * ypixel, x * xpixel).bytes()),
                        16 => self.replies.extend(format!("\x1b[6;{ypixel};{xpixel}t").bytes()),
                        18 => self.replies.extend(format!("\x1b[8;{y};{x}t").bytes()),
                        19 => self.replies.extend(format!("\x1b[9;{y};{x}t").bytes()),
                        22 | 23 => {
                            m += 1;
                            match arg(m) {
                                None => return,
                                Some(0 | 2) if n == 22 => {
                                    if self.title_stack.len() == 10 {
                                        self.title_stack.pop_front();
                                    }
                                    self.title_stack.push_back(self.current_title.clone());
                                }
                                Some(0 | 2) => {
                                    if let Some(title) = self.title_stack.pop_back() {
                                        self.current_title = title;
                                    }
                                    let title = self.current_title.clone();
                                    self.title = Some(title.clone());
                                    self.events.push(events::OutputEvent::Title(title));
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                    m += 1;
                }
            }
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
    Switch(Box<modes::Switch>),
    Panes(Box<modes::Panes>),
}

impl PaneMode {
    /// The mode's name, as pane_mode shows it.
    pub fn name(&self) -> &'static str {
        match self {
            PaneMode::Copy(c) if c.is_view_mode() => "view-mode",
            PaneMode::Copy(_) => "copy-mode",
            PaneMode::Clock => "clock-mode",
            PaneMode::Tree(t) => match t.kind {
                modes::TreeKind::Tree => "tree-mode",
                modes::TreeKind::Buffers => "buffer-mode",
                modes::TreeKind::Clients => "client-mode",
                modes::TreeKind::Options => "options-mode",
            },
            PaneMode::Switch(_) => "switch-mode",
            PaneMode::Panes(_) => "panes-mode",
        }
    }

    /// Set -k again when the mode is raised from the stack (window_pane_set_mode).
    pub fn set_kill(&mut self, kill: bool) {
        match self {
            PaneMode::Tree(t) => t.kill = kill,
            PaneMode::Switch(s) => s.kill = kill,
            PaneMode::Panes(p) => p.kill = kill,
            PaneMode::Copy(c) => c.kill = kill,
            _ => {}
        }
    }

    /// -k: the pane is killed when the mode exits.
    pub fn kill_on_exit(&self) -> bool {
        match self {
            PaneMode::Tree(t) => t.kill,
            PaneMode::Switch(s) => s.kill,
            PaneMode::Panes(p) => p.kill,
            PaneMode::Copy(c) => c.kill,
            _ => false,
        }
    }

    /// Whether the mode zoomed the window itself (so leaving it unzooms).
    pub fn zoomed_window(&self) -> bool {
        match self {
            PaneMode::Tree(t) => t.zoomed == Some(false),
            PaneMode::Switch(s) => s.zoomed == Some(false),
            PaneMode::Panes(p) => p.zoomed == Some(false),
            _ => false,
        }
    }
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
    pub start_path: String,
    pub title: String,
    pub dead: Option<(Option<i32>, u64)>,
    pub scrollbar_until: Option<Instant>,
    /// The mode on top of the pane's mode stack (what is shown and gets keys).
    pub mode: Option<PaneMode>,
    /// The modes under it (wp->modes after the first), the next one last: leaving the
    /// top mode goes back to the one below.
    pub mode_stack: Vec<PaneMode>,
    /// command-prompt -P: a prompt owned by the pane (wp->prompt), drawn over its last line
    /// on every client showing it; keys from any of them go to it.
    pub prompt: Option<Prompt>,
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
    /// Floating pane flags (float::OVER_ZOOM and friends; tmux PANE_FLOATOVERZOOM...).
    pub fflags: u8,
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
        for m in self.mode.iter_mut().chain(self.mode_stack.iter_mut()) {
            if let PaneMode::Copy(cm) = m {
                cm.resize(cols, rows);
            }
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
    /// Previously active panes, most recent first (tmux `last_panes`).
    pub last_panes: Vec<PaneId>,
    pub zoomed: bool,
    /// The zoomed pane while a pane floating above the zoom is active.
    pub zoom_pane: Option<PaneId>,
    /// Panes front to back (tmux `z_index`): floating panes come before tiled ones.
    pub zorder: Vec<PaneId>,
    /// The modal pane and the pane active before it.
    pub modal: Option<PaneId>,
    pub modal_last: Option<PaneId>,
    /// Where the last new floating pane was put, for cascading.
    pub last_new_pane: (i32, i32),
    /// The cell a pane had when it last floated (tmux `layout_cell.fg`), for break-pane -W.
    pub float_saved: HashMap<PaneId, crate::layout::FGeom>,
    pub options: Options,
    /// tmux w->flags: bell, activity and silence seen but not yet checked
    /// (alerts_check_session finds them later if monitoring is turned on).
    pub alerts: u8,
    /// When the silence timer (alerts_timer) was last reset, in microseconds.
    pub silence_reset_us: u64,
    pub layout_cycle: Option<usize>,
    pub old_layout: Option<String>,
    pub created: u64,
    pub activity: u64,
    /// activity_time to the microsecond (sort orders).
    pub activity_us: u64,
    /// Width/height the window is laid out at (follows the attached session size).
    pub sx: u16,
    pub sy: u16,
    /// The size window-size manual gives it: its first size, then resize-window's
    /// (manual_sx, manual_sy).
    pub manual_sx: u16,
    pub manual_sy: u16,
    /// A size waiting until the window is the current window of an attached session
    /// (WINDOW_RESIZE with new_sx/new_sy).
    pub new_size: Option<(u16, u16)>,
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

    /// Place a new pane in the pane list like tmux window_add_pane: -b goes before the target
    /// (at the head with -f), otherwise after it (at the tail with -f or for a floating pane).
    pub fn add_pane_order(&mut self, target: PaneId, new: PaneId, before: bool, full: bool, floating: bool) {
        let mut order = self.panes();
        order.retain(|p| *p != new);
        if before && full {
            order.insert(0, new);
        } else if !before && (full || floating) {
            order.push(new);
        } else {
            let idx = order.iter().position(|p| *p == target).unwrap_or(order.len());
            order.insert((idx + usize::from(!before)).min(order.len()), new);
        }
        self.pane_order = order;
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

    /// The last pane (top of the last-panes stack).
    pub fn last(&self) -> Option<PaneId> {
        let live = self.layout.panes();
        self.last_panes.iter().copied().find(|p| live.contains(p) && *p != self.active)
    }

    /// Make `pane` active, pushing the old active pane on the last-panes stack.
    pub fn set_active(&mut self, pane: PaneId) {
        if pane == self.active {
            return;
        }
        let old = self.active;
        self.last_panes.retain(|p| *p != pane && *p != old);
        self.last_panes.insert(0, old);
        self.active = pane;
    }

    /// Live panes front to back: floating panes in their z-order, then the tiled ones.
    pub fn zlist(&self) -> Vec<PaneId> {
        let live = self.layout.panes();
        let mut z: Vec<PaneId> = self.zorder.iter().copied().filter(|p| live.contains(p)).collect();
        for p in self.panes() {
            // a pane that was never ordered goes in front if floating, else at the back
            if !z.contains(&p) && self.layout.is_float(p) { z.insert(0, p) }
        }
        // tiled panes go on the back of the z-order as they are made (spawn_pane), so
        // in id order
        let mut made: Vec<PaneId> = self.panes().into_iter().filter(|p| !z.contains(p)).collect();
        made.sort_unstable();
        z.extend(made);
        z
    }

    pub fn is_floating(&self, pane: PaneId) -> bool {
        self.layout.is_float(pane)
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
    pub created: u64,
    pub activity: u64,
    /// activity_seq() at the last activity update: the activity order (tmux compares
    /// activity timevals).
    pub activity_seq: u64,
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

impl Identity {
    /// server_client_check_nested: $TMUX is set and the client runs in one of this
    /// server's panes (here: its pane and the server pid in $PWSH_TMUX), so a client in
    /// another server's pane is not nested.
    pub fn nested(&self) -> bool {
        let ours = self.env.iter().find(|(k, _)| k == "PWSH_TMUX").and_then(|(_, v)| v.rsplit(',').nth(1)?.parse::<u32>().ok()) == Some(std::process::id());
        ours && self.ctx_pane.is_some()
    }
}

pub enum PromptKind {
    Command,
    Confirm,
    Key,
}

pub struct Prompt {
    pub vi_command: bool,
    /// C-v: the next key is inserted as it is
    pub quote_next: bool,
    pub copied: Vec<char>,
    pub kind: PromptKind,
    pub prompts: Vec<(String, String)>,
    pub answers: Vec<String>,
    pub label: String,
    pub input: Vec<char>,
    pub cursor: usize,
    pub template: String,
    /// The template was a { } block (ARGS_COMMANDS): the answers go into its parsed
    /// values, which are not parsed again (args_make_commands, cmd_list_copy).
    pub block: bool,
    pub history_pos: Option<usize>,
    /// The history this prompt reads and adds to (command-prompt -T, an index of
    /// input::PROMPT_TYPES).
    pub history_type: usize,
    /// Set when the prompt is opened (Server::next_prompt_id): a waiting
    /// command-prompt or confirm-before goes on once no prompt has it.
    pub id: u64,
    pub pane: Option<PaneId>,
    pub numeric: bool,
    pub single_key: bool,
    /// command-prompt -i: the template runs on every change (PROMPT_INCREMENTAL);
    /// `last` is the -I text, restored by C-r/C-s on an empty prompt.
    pub incremental: bool,
    pub last: String,
    /// Inline Tab completion matches drawn after the input (complete_list).
    pub complete: Vec<String>,
    /// The prompt finished (prompt_closed).
    pub closed: bool,
    /// The pane a command-prompt -P prompt belongs to.
    pub host: Option<PaneId>,
    pub copy_incremental: Option<(PaneId, bool)>,
    /// command-prompt -e: backspace on an empty prompt closes it.
    pub bspace_exit: bool,
    /// confirm-before: the key that confirms (-c) and whether Enter does too (-y).
    pub confirm: (char, bool),
}

impl Prompt {
    /// A prompt showing the first of `prompts` (label, initial input).
    pub fn new(kind: PromptKind, prompts: Vec<(String, String)>, template: String, pane: Option<PaneId>) -> Prompt {
        let (label, init) = prompts.first().cloned().unwrap_or_default();
        let input: Vec<char> = init.chars().collect();
        Prompt {
            vi_command: false, quote_next: false, copied: Vec::new(), kind, prompts, answers: Vec::new(), label, cursor: input.len(), input,
            template, block: false, history_pos: None, history_type: 0, id: 0, pane, numeric: false, single_key: false, incremental: false, last: String::new(),
            complete: Vec::new(), closed: false, host: None, copy_incremental: None, bspace_exit: false, confirm: ('y', false),
        }
    }
}

pub enum Overlay {
    Popup(modes::Popup),
}

pub struct RenderState {
    pub frame: render::Frame,
    pub cursor: Option<(u16, u16)>,
    /// The hidden cursor goes to the top left (the active pane's cursor is outside the
    /// part of a bigger window shown).
    pub park: bool,
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
    /// A scrollbar slider drag (copy-mode -S or scroll-to-mouse, with -e).
    Scrollbar { pane: PaneId, exit: bool },
    /// A floating pane: resize-pane -M resizes it by a border or corner (moves it by the top
    /// border), move-pane -M (`moving`) moves it.
    Float { pane: PaneId, moving: bool },
    /// new-pane -M: the new floating pane spans from where the drag started to the mouse.
    NewFloat { pane: PaneId, x: i32, y: i32 },
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
    /// terminal-overrides as they were when the terminal description was made
    /// (tty_term_create) or its features last changed (tty_update_features).
    pub term_overrides: Option<Vec<String>>,
    /// What the terminal called itself in its XTVERSION reply (c->term_type).
    pub term_type: String,
    pub esc_deadline: Option<Instant>,
    pub prompt: Option<Prompt>,
    pub overlay: Option<Overlay>,
    pub overlay_epoch: u64,
    pub message: Option<(String, Instant)>,
    /// display-message -N: keys do not clear the message (only with a timer)
    pub message_ignore_keys: bool,
    /// The message is text: its # are not styles (all but display-message).
    pub message_ignore_styles: bool,
    pub prev: Option<RenderState>,
    pub dirty: bool,
    pub last_session: Option<SessId>,
    pub created: u64,
    pub activity: u64,
    /// activity_seq() at the last key: tmux compares activity timevals, finer than seconds.
    pub activity_seq: u64,
    pub drag: Option<Drag>,
    /// Where in the scrollbar slider a drag started (mouse_slider_mpos).
    pub slider_mpos: u16,
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
    /// What the client's terminal supports (terminal-features, -T).
    pub features: features::Features,
    /// refresh-client -l: until when a clipboard reply from the terminal makes a buffer
    /// (TTY_OSC52QUERY).
    pub clipboard_query: Option<Instant>,
    /// CLIENT_FOCUSED: set when the client gets a session, then follows the terminal's
    /// focus reports.
    pub focused: bool,
    pub terminal: theme::Terminal,
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
    /// refresh-client -L/-R/-U/-D: the window panned and the view's top left in it
    /// (pan_window, pan_ox, pan_oy).
    pub pan: Option<(WinId, u16, u16)>,
}

#[derive(Default)]
pub struct ClientIo {
    pub written: std::sync::atomic::AtomicU64,
    pub discarded: std::sync::atomic::AtomicU64,
    /// The client has been sent its reply or exit (CLIENT_EXIT).
    pub exit_sent: std::sync::atomic::AtomicBool,
}

#[cfg(unix)]
fn process_exit_signal(s: &std::process::ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(s)
}

#[cfg(not(unix))]
fn process_exit_signal(_: &std::process::ExitStatus) -> Option<i32> {
    None
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
        if kind == S_REPLY || kind == S_EXIT {
            self.io.exit_sent.store(true, Ordering::SeqCst);
        }
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

    /// Ends an attached client; control clients get %exit first, after the reply
    /// of the command that is running (server_client_check_exit).
    pub fn send_exit(&self, msg: &str) {
        if self.control > 0 {
            self.ctl.deferred_exit.borrow_mut().get_or_insert_with(|| msg.to_string());
            return;
        }
        // detach and session exit clear `attached` first; the reset depends only on what the
        // terminal was given, which is nothing for a command client
        let reset = self.tty_stop_sequence();
        if !reset.is_empty() { self.send(S_OUTPUT, reset.as_bytes()); }
        self.send(S_EXIT, msg.as_bytes());
    }

    /// Sends a control client's deferred exit.
    pub fn finish_exit(&self) {
        let Some(msg) = self.ctl.deferred_exit.borrow_mut().take() else { return };
        // a control client is never attached in its client process: detaching and
        // other exits print a bare %exit; only messages the server provides
        // (client_dispatch_exit_message) are shown
        let reason = msg.trim_matches(['[', ']']);
        let reason = if matches!(reason, "access not allowed" | "too far behind") { reason } else { "" };
        let line = if reason.is_empty() { "%exit\n".to_string() } else { format!("%exit {reason}\n") };
        self.send(S_OUTPUT, line.as_bytes());
        if self.ctl.wait_exit {
            // wait-exit: the client leaves after its next line
            self.ctl.exiting.set(true);
            return;
        }
        self.send(S_EXIT, &self.control_exit_payload(&msg));
    }

    /// A control client's S_EXIT: the message, then NUL and its exit status (c->retval).
    pub fn control_exit_payload(&self, msg: &str) -> Vec<u8> {
        format!("{msg}\0{}", self.ctl.retval.get()).into_bytes()
    }
    pub fn name(&self) -> String {
        let id = self.ident.as_ref().filter(|i| !i.tty && i.pid != 0).map_or(self.id, |i| i.pid);
        format!("client-{id}")
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
    /// Errors of config file lines for a command client: its stderr, exit status 1
    /// (cmdq_error sets the client's retval).
    pub soft_err: String,
    /// 1: commands of a hook (no after-* hooks); 2: run from the global queue, so the
    /// events they cause run no hooks at all (CMDQ_STATE_NOHOOKS).
    pub hook_depth: u8,
    /// True when run from a key binding (output goes to view mode).
    pub from_key: bool,
    /// Command number of a control-mode command (reply goes in a %begin/%end block).
    pub control_seq: Option<u64>,
    /// The originating request's restriction, retained across hooks and parked queues.
    pub readonly: bool,
    /// Files being sourced (innermost last), for #{current_file}.
    pub files: Vec<String>,
    /// The line of the command running in each of `files` (0 before the first).
    pub lines: Vec<usize>,
    /// Which of `files` source-file opened (its nesting limit does not count the
    /// startup configuration).
    pub sourced: Vec<bool>,
    /// The startup configuration's queue (the global queue: errors never end it).
    pub cfg: bool,
    /// The exit status for the command-line client (c->retval: run-shell and new-pane -W
    /// set it).
    pub retval: i32,
    /// The key a bound command runs for (the message log names it).
    pub key: Option<String>,
    /// The client the message log names when it is not `client` (send-keys -K: the
    /// client that ran send-keys).
    pub log_client: Option<ClientId>,
    /// Formats added by a hook (hook, hook_*), seen by its commands.
    pub formats: Option<Arc<HashMap<String, String>>>,
    /// Typed by a control client (its reply's %begin flags are 1).
    pub control_line: bool,
    /// A hook's current state (cmd_find_state): its session and winlink as well as
    /// `pane`, used while `pane` is still that pane.
    pub target: Option<vars::Target>,
    /// A control client's reply blocks were written item by item.
    pub control_done: bool,
    /// The queue waits inside a control client's command: its block is not written yet.
    pub control_open: bool,
    /// Hook context stack survives async commands that park the queue.
    pub hook_saved: events::HookSaved,
    pub command_error: Option<String>,
    pub hook_error: Option<String>,
    pub skip_after: bool,
    /// How many key bindings' command groups (KEY_GROUP) the queue is in.
    pub key_depth: usize,
    /// What each open KEY_GROUP replaced: the client, pane, key and logged client.
    pub key_saved: Vec<(Option<ClientId>, Option<PaneId>, Option<String>, Option<ClientId>)>,
    /// The next command is run-shell resumed after its -d delay: it is in the message
    /// log already.
    pub resumed: bool,
    /// What a background job (run-shell -b, if-shell -b) runs when it is done: no
    /// client's queue waits for it.
    pub detached: bool,
}

/// Queue markers source-file puts around a file's commands. A parsed command can hold any
/// bytes (a NUL in a config file too), so each marker carries a nonce chosen at startup.
pub fn internal_mark(name: &str) -> String {
    use std::hash::{BuildHasher, Hasher};
    static NONCE: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        h.write_u32(std::process::id());
        h.finish()
    });
    format!("\0{name}\0{:016x}", *NONCE)
}
pub static FILE_PUSH: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| internal_mark("file-push"));
pub static FILE_POP: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| internal_mark("file-pop"));
/// Before each command of a file: its line (cmd_get_source), which also makes its group.
pub static LINE_MARK: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| internal_mark("line"));
/// The end of the startup configuration (cfg_done).
/// An error without a message (cmdq_error not called): it ends the command's group only.
pub const QUIET_ERROR: &str = "\0quiet-error";
pub static CFG_DONE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| internal_mark("cfg-done"));
/// A key send-keys -K typed (server_client_key_callback): [KEY_EVENT, token of
/// Server::key_events]. It is handled when the queue reaches it, so keys and the commands
/// their bindings run happen in order, after any earlier command waits (a prompt).
pub static KEY_EVENT: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| internal_mark("key"));
/// [KEY_GROUP, "1", client, pane, key] and [KEY_GROUP, "0"] are put around the commands a
/// KEY_EVENT's binding runs: they are a group of their own (key_bindings_dispatch makes a
/// new cmdq item), so an error ends only the rest of that binding, and they run with the
/// state of the key (its client, that client's pane and the key, for formats and the
/// message log), while the queue's own client stays the one the log names.
pub static KEY_GROUP: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| internal_mark("key-group"));

/// A key waiting in a queue as a KEY_EVENT item.
pub struct KeyEvent {
    pub client: ClientId,
    /// send-keys' target pane, whose cursor key mode encodes the key.
    pub pane: PaneId,
    /// The client that ran send-keys (key_origin while the key is handled).
    pub origin: ClientId,
    pub key: keys::Key,
    /// A mouse key sent by name (MouseDown1Pane), in place of `key`.
    pub mouse: Option<String>,
}

/// An item of an attached client's command queue (c->queue) waiting behind a command
/// that waits (a prompt, a popup, run-shell): a key it typed, which goes through the key
/// tables when reached (server_client_key_callback), or commands appended for it
/// (cmdq_append: a menu item, a mode's choice, a prompt without a waiting command).
pub enum ClientItem {
    Key(keys::Key, Vec<u8>),
    Mouse(String, input::MouseCtx),
    /// A bracketed paste, and bytes the key parser did not know (both for the pane).
    Paste(Vec<u8>),
    Raw(Vec<u8>),
    Queue(Queue),
    /// A parse error (cmdq_get_error): reported when the queue reaches it.
    Error(String),
}

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
    /// command-prompt and confirm-before without -b: the queue waits for the prompt.
    WaitPrompt(ClientId, u64),
    /// Run these commands next, before the rest of the queue (source-file, if-shell -F, run-shell -C).
    /// With `soft`, their errors are reported but don't stop the queue (like config files).
    Inject(Vec<Vec<String>>, bool),
}

pub enum JobKind {
    /// `stderr`: run-shell -E shows the command's stderr (tmux discards it otherwise).
    RunShell { pane: Option<PaneId>, background: bool, stderr: bool },
    IfShell { then: String, otherwise: Option<String> },
}

/// check_window_name's state of a window: when its name was last checked (name_time),
/// its pending name timer, what its active pane was then (window_set_active_pane, a mode
/// set or reset and a pane exit set PANE_CHANGED) and whether automatic-rename was on
/// (turning it on sets PANE_CHANGED).
#[derive(Default)]
pub struct NameState {
    pub time: Option<Instant>,
    pub timer: Option<Instant>,
    pub seen: Option<(PaneId, Option<std::mem::Discriminant<PaneMode>>, bool)>,
    pub auto: bool,
}

/// NAME_INTERVAL: a window's name is checked at most this often.
pub const NAME_INTERVAL: Duration = Duration::from_millis(500);

pub struct Parked {
    pub queue: Queue,
    pub kind: JobKind,
    /// The job's command and process (show-messages -J), and when it started.
    pub cmd: String,
    pub pid: Option<u32>,
    /// The descriptor the server reads the job's output from (-1 where there is none).
    pub fd: i64,
    pub started: u64,
}

pub struct Server {
    pub tx: mpsc::SyncSender<Event>,
    pub pty: Box<dyn PtySystem + Send>,
    pub sessions: BTreeMap<SessId, Session>,
    pub windows: HashMap<WinId, Window>,
    pub panes: HashMap<PaneId, Pane>,
    pub clients: BTreeMap<ClientId, Client>,
    pub next_id: u32,
    /// Sessions, windows and panes are numbered apart, each from 0 (next_session_id,
    /// next_window_id, next_window_pane_id).
    pub next_session_id: SessId,
    pub next_window_id: WinId,
    pub next_pane_id: PaneId,
    pub opts_server: Options,
    pub opts_session: Options,
    pub opts_window: Options,
    pub env: BTreeMap<String, Option<String>>,
    /// Hidden environment variables (set-environment -h, %hidden): (session or 0 for
    /// the global environment, name). Not given to new processes.
    pub env_hidden: std::collections::HashSet<(SessId, String)>,
    /// The configuration has been loaded (cfg_finished): commands are logged from then.
    pub cfg_finished: bool,
    /// While send-keys -K types keys: the client that ran it (key_bindings_dispatch
    /// queues the bound commands for that client).
    pub key_origin: Option<ClientId>,
    /// send-keys -K: the bound commands of the keys it handles, which go in its own
    /// queue after it (server_client_handle_key_after) instead of a queue of their own.
    pub key_capture: Option<Vec<Vec<String>>>,
    /// send-keys without -K to a pane in copy mode: the bindings of its keys go in its
    /// queue (key_capture) for the queue's own client, without a key event.
    pub key_plain: bool,
    /// send-keys -K keys waiting in a queue (KEY_EVENT items), by token.
    pub key_events: HashMap<u64, KeyEvent>,
    pub key_event_seq: u64,
    /// How many queues are running (run_queue nesting).
    pub queue_depth: usize,
    /// Each attached client's queue (c->queue) behind its running command: keys it typed
    /// and commands appended for it. They run in order once no command of the client's
    /// waits and no queue is running (a menu item chosen by a key send-keys -K hands over
    /// runs when that queue is done or waits).
    pub client_items: HashMap<ClientId, VecDeque<ClientItem>>,
    /// The client whose terminal input is being read: its keys are all queued before any
    /// of them is handled (tty_keys_next runs for the whole read before cmdq_next).
    pub reading: Option<ClientId>,
    /// resume_waits is running.
    pub resuming: bool,
    /// drain_clients is running.
    pub draining: bool,
    /// Commands a prompt's answer gives the command waiting for it (cmdq_insert_after the
    /// waiting item), by prompt id, run before the rest of its queue.
    pub prompt_inserts: HashMap<u64, Vec<Vec<String>>>,
    /// Events: hook bookkeeping, wait-for -E, monitors, change detection.
    pub ev: events::State,
    pub buffers: Vec<Buffer>,
    pub buffer_seq: u32,
    pub bindings: BTreeMap<String, BTreeMap<String, Binding>>,
    /// The key tables that still have their default_key_bindings (a table removed and
    /// made again has none).
    pub default_tables: std::collections::BTreeSet<String>,
    pub messages: VecDeque<(u64, String)>,
    /// Prompt history per type (command, search).
    pub prompt_history: [Vec<String>; 2],
    pub jobs: HashMap<u32, Parked>,
    pub waits: HashMap<String, events::Channel>,
    /// Queues waiting for an overlay to close, with the command's argv (its after hook).
    pub overlay_waits: Vec<(ClientId, u64, Vec<String>, Queue)>,
    /// The pane of each popup a queue waits for (by overlay epoch), and how the popup's
    /// command exited, for the command-line client's exit status.
    pub popup_panes: HashMap<u64, PaneId>,
    pub popup_status: HashMap<PaneId, i32>,
    /// Queues waiting for a prompt to close: (client, prompt id, the command's argv, queue).
    pub prompt_waits: Vec<(ClientId, u64, Vec<String>, Queue)>,
    /// The last prompt id given out (Server::next_prompt_id).
    pub prompt_ids: u64,
    /// Open menus by window (w->menu).
    pub menus: HashMap<WinId, modes::Menu>,
    /// Copy-mode panes scrolling while a selection drag rests at an edge.
    pub copy_drag_timers: HashMap<PaneId, Instant>,
    /// A lookup of what the panes run is in progress (at most one at a time).
    pub pane_info_busy: bool,
    /// Panes changed since their window's name was last checked (PANE_CHANGED).
    pub name_changed: std::collections::HashSet<PaneId>,
    /// Each window's automatic-rename state (check_window_name).
    pub name_state: HashMap<WinId, NameState>,
    /// Panes (and their generation) whose program a lookup has found: tmux reads it as it
    /// checks the name, so a pane's name is not checked before that.
    pub name_known: std::collections::HashSet<(PaneId, u32)>,
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
    /// The working directory of the client that started the server: the configuration
    /// loads before any client connects, so this stands for cfg_client's cwd until one
    /// does (relative paths in the startup configuration resolve against it).
    pub cfg_cwd: Option<String>,
    /// Errors (and display-message output) of commands run without a client while the
    /// configuration loads (cfg_causes), until they are shown.
    pub cfg_causes: Vec<String>,
    /// The first client while the configuration loads (cfg_client): its commands wait for
    /// cfg_done, and spawns without -c use its directory.
    pub cfg_client: Option<ClientId>,
    /// The messages of cfg_client held until cfg_done (its cfg_client_done item).
    pub cfg_held: Vec<(ClientId, u8, Vec<u8>)>,
    /// load_config has run, and whether a client has sent a command since: the first
    /// one is the client cfg_done would have seen (start_cfg runs once it identifies).
    pub cfg_started: bool,
    pub cfg_client_seen: bool,
    /// Jobs of the configuration that finished before the first client's command came
    /// (tmux has that command queued before any job can finish), and until when to wait.
    pub cfg_jobs_held: Vec<(u32, i32, String)>,
    pub cfg_jobs_until: Option<Instant>,
    /// Queues waiting for a pane's command to exit (new-pane -W).
    pub pane_waits: HashMap<PaneId, Vec<(Vec<String>, Queue)>>,
    /// Panes running the editor of a mode (wp->editor) and the client it was opened for.
    pub editors: HashMap<PaneId, (ClientId, modes::PopupEditor)>,
}

pub fn run(config: Option<String>, cwd: Option<String>) -> io::Result<()> {
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
    srv.cfg_cwd = cwd.filter(|d| !d.is_empty());
    srv.load_config(config);
    let allowed: Vec<String> = srv.acl.iter().map(|a| a.0.clone()).collect();
    let Some(listener) = ipc::listen_exclusive(&allowed)? else {
        return Ok(()); // another server won the race
    };
    srv.pipe_acl = Some(allowed);
    if !srv.acl.is_empty() {
        ipc::set_shared(true);
    }
    log(&format!("server started on {}", ipc::describe()));
    ipc::serve(listener, move |conn| match conn {
        Ok(s) => atx.send(Event::Conn(s)).is_ok(),
        Err(e) => {
            log(&format!("accept failed: {e}"));
            true
        }
    });

    let started = Instant::now();
    let mut last_detect = Instant::now();
    loop {
        let timeout = srv.next_timeout();
        // a turn with only pane output changes nothing change detection looks at (its
        // events fire where the output is read); others are checked every turn
        let mut output_only = true;
        match rx.recv_timeout(timeout) {
            Ok(ev) => {
                output_only &= matches!(ev, Event::PaneData(..));
                srv.handle(ev);
                // Coalesce bursts of output into one render.
                let deadline = Instant::now() + Duration::from_millis(4);
                while let Ok(ev) = rx.try_recv() {
                    output_only &= matches!(ev, Event::PaneData(..));
                    srv.handle(ev);
                    if Instant::now() > deadline {
                        break;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => output_only = false,
            Err(RecvTimeoutError::Disconnected) => break,
        }
        srv.sync_groups();
        srv.timers();
        srv.check_window_resizes();
        if !output_only || last_detect.elapsed() >= Duration::from_millis(50) {
            last_detect = Instant::now();
            srv.detect_changes(None);
        }
        srv.drain_hooks();
        // server_client_loop: check_window_name
        srv.automatic_rename();
        srv.ev.activity.clear();
        srv.control_notify();
        srv.render_all();
        // server_loop: the server goes only once every client has gone (here: has its
        // reply), so a command that connected as the last session went is still served
        if srv.quit || srv.would_exit(None, started.elapsed() > Duration::from_secs(30)) {
            match rx.try_recv() {
                Ok(ev) if !srv.quit => srv.handle(ev),
                _ => break,
            }
        }
    }
    // no new clients from here (kill-server has stopped listening already): one started
    // now finds no server and starts its own
    ipc::cleanup();
    srv.shutdown();
    srv.save_prompt_history();
    log("server exiting");
    Ok(())
}

impl Server {
    /// server_loop's exit test: no sessions (exit-empty) or no attached client
    /// (exit-unattached), and no client other than `except` still waiting for its reply.
    pub fn would_exit(&self, except: Option<ClientId>, timed_out: bool) -> bool {
        let exit_empty = options::is_on(self.opts_server.get("exit-empty").unwrap_or("on"));
        let idle = self.sessions.is_empty() && exit_empty && (self.had_session || timed_out);
        let unattached = options::is_on(self.opts_server.get("exit-unattached").unwrap_or("off"))
            && self.had_session
            && !self.clients.values().any(|c| c.attached);
        (idle || unattached) && self.clients.iter().all(|(id, c)| Some(*id) == except || c.io.exit_sent.load(std::sync::atomic::Ordering::SeqCst))
    }

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
            next_session_id: 0,
            next_window_id: 0,
            next_pane_id: 0,
            opts_server: Options::defaults(Scope::Server),
            opts_session: Options::defaults(Scope::Session),
            opts_window: Options::defaults(Scope::Window),
            env,
            env_hidden: std::collections::HashSet::new(),
            cfg_finished: false,
            key_origin: None,
            key_capture: None,
            key_plain: false,
            key_events: HashMap::new(),
            key_event_seq: 0,
            queue_depth: 0,
            client_items: HashMap::new(),
            reading: None,
            resuming: false,
            draining: false,
            prompt_inserts: HashMap::new(),
            ev: Default::default(),
            buffers: Vec::new(),
            buffer_seq: 0,
            bindings: input::default_bindings(),
            default_tables: input::default_bindings().into_keys().collect(),
            messages: VecDeque::new(),
            prompt_history: Default::default(),
            jobs: HashMap::new(),
            waits: HashMap::new(),
            overlay_waits: Vec::new(),
            popup_panes: HashMap::new(),
            popup_status: HashMap::new(),
            prompt_waits: Vec::new(),
            prompt_ids: 0,
            menus: HashMap::new(),
            copy_drag_timers: HashMap::new(),
            pane_info_busy: false,
            name_changed: Default::default(),
            name_state: HashMap::new(),
            name_known: Default::default(),
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
            cfg_cwd: None,
            cfg_causes: Vec::new(),
            cfg_client: None,
            cfg_held: Vec::new(),
            cfg_started: false,
            cfg_client_seen: false,
            cfg_jobs_held: Vec::new(),
            cfg_jobs_until: None,
            pane_waits: HashMap::new(),
            editors: HashMap::new(),
        };
        srv.opts_session.set("default-shell", procinfo::default_shell());
        srv
    }

    pub fn id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    pub fn new_session_id(&mut self) -> SessId {
        self.next_session_id += 1;
        self.next_session_id - 1
    }

    pub fn new_window_id(&mut self) -> WinId {
        self.next_window_id += 1;
        self.next_window_id - 1
    }

    pub fn new_pane_id(&mut self) -> PaneId {
        self.next_pane_id += 1;
        self.next_pane_id - 1
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
        // start_cfg/load_cfg: each file is parsed now and its commands go in the global
        // queue without a client, which goes on after errors, then cfg_done; a command
        // that waits (run-shell, if-shell) holds back the rest, and the first client's
        // command waits for cfg_done. Errors name the file as given and wait in
        // cfg_causes.
        let ctx = Ctx { cfg: true, ..Ctx::default() };
        let mut cmds = VecDeque::new();
        for (f, quiet) in files {
            let path = match &self.cfg_cwd {
                Some(d) if std::path::Path::new(&f).is_relative() => std::path::Path::new(d).join(&f).to_string_lossy().into_owned(),
                _ => f.clone(),
            };
            // load_cfg: a file that cannot be opened is a cause "path: strerror" (a missing
            // one is fine when quiet)
            if let Err(e) = std::fs::File::open(&path) {
                if !(quiet && e.kind() == io::ErrorKind::NotFound) {
                    self.cfg_causes.push(format!("{f}: {}", cmds::strerror(&e)));
                }
                continue;
            }
            match self.load_file(&path, &f, false, false, &ctx) {
                Ok((c, _)) => cmds.extend(c),
                Err(e) => self.cfg_causes.push(e),
            }
        }
        cmds.push_back(vec![CFG_DONE.to_string()]);
        let soft = cmds.len();
        self.cfg_started = true;
        self.cfg_jobs_until = Some(Instant::now() + Duration::from_secs(5));
        self.run_queue(Queue { cmds, ctx, soft });
    }

    /// The configuration's jobs held for the first client's command finish now.
    fn cfg_release_jobs(&mut self) {
        self.cfg_jobs_until = None;
        for (id, status, output) in std::mem::take(&mut self.cfg_jobs_held) {
            self.job_done(id, status, output);
        }
    }

    /// cfg_done: the configuration has loaded; its causes are shown (cfg_show_causes),
    /// the first client's held commands run and the prompt history is loaded.
    pub fn cfg_done(&mut self) {
        if self.cfg_finished {
            return;
        }
        self.cfg_finished = true;
        self.cfg_show_causes(None);
        self.cfg_client = None;
        for (cid, kind, payload) in std::mem::take(&mut self.cfg_held) {
            self.client_msg(cid, kind, payload);
        }
        self.load_prompt_history();
    }

    /// cmdq_error for a command without a client: during the configuration its error
    /// waits in cfg_causes, later it goes in the message log, with the command's file
    /// and line when it came from a file.
    pub fn no_client_error(&mut self, ctx: &Ctx, e: &str) {
        let msg = match (ctx.files.last(), ctx.lines.last()) {
            (Some(f), Some(&l)) if l > 0 => format!("{f}:{l}: {e}"),
            _ => e.to_string(),
        };
        if self.cfg_finished {
            self.add_message(&format!("message: {msg}"));
        } else {
            self.cfg_causes.push(msg);
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

    /// links_of in w->winlinks order: the order the links were made.
    pub fn winlinks_of(&self, wid: WinId) -> Vec<(SessId, u32)> {
        let mut v = self.links_of(wid);
        let seq = &self.ev.seen.link_seq;
        v.sort_by_key(|k| seq.get(k).filter(|(w, _)| *w == wid).map_or(u64::MAX, |(_, n)| *n));
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
        self.sessions.values().max_by_key(|s| s.activity_seq).map(|s| s.id)
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

    /// A status message that is text, not a format (status_message_set with ignore_styles).
    pub fn set_message(&mut self, cid: ClientId, msg: &str) {
        self.set_message_for(cid, msg, None, false);
        if let Some(c) = self.clients.get_mut(&cid) {
            c.message_ignore_styles = true;
        }
    }

    /// A command error for an attached client (cmdq_error): the first letter is upper case.
    pub fn set_error(&mut self, cid: ClientId, msg: &str) {
        let mut chars = msg.chars();
        let msg: String = chars.next().map(|c| c.to_ascii_uppercase()).into_iter().chain(chars).collect();
        self.set_message(cid, &msg);
    }

    /// tmux status_message_set: `delay` None uses display-time; zero waits for a
    /// key press (which clears any message unless `ignore_keys` and a timer is set).
    pub fn set_message_for(&mut self, cid: ClientId, msg: &str, delay: Option<u64>, ignore_keys: bool) {
        let ms = delay.unwrap_or_else(|| self.client_session(cid).map(|s| self.opt_session_num(Some(s), "display-time")).unwrap_or(750).max(0) as u64);
        // status_message_set logs it with the client's name
        match self.clients.get(&cid).map(|c| c.name()) {
            Some(n) => self.add_message(&format!("{n} message: {msg}")),
            None => self.add_message(&format!("message: {msg}")),
        }
        if let Some(c) = self.clients.get_mut(&cid) {
            let until = if ms == 0 { MESSAGE_FOREVER } else { Duration::from_millis(ms) };
            c.message = Some((msg.to_string(), Instant::now() + until));
            c.message_ignore_styles = false;
            // as tmux, a message without a timer keeps the previous setting
            if ms != 0 {
                c.message_ignore_keys = ignore_keys;
            }
            c.dirty = true;
        }
    }

    // ---- events -------------------------------------------------------------------

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Conn(stream) => self.accept(stream),
            Event::ClientMsg(cid, kind, payload) => {
                let first = !self.cfg_client_seen;
                self.client_msg(cid, kind, payload);
                if first && self.cfg_client_seen {
                    self.cfg_release_jobs();
                }
            }
            Event::ClientGone(cid) => self.client_gone(cid),
            Event::PaneData(id, generation, data) => self.pane_data(id, generation, &data),
            Event::PaneExit(id, generation, status) => {
                if self.panes.get(&id).is_some_and(|p| p.generation == generation) {
                    self.pane_exited(id, status);
                }
            }
            Event::Job(id, status, output) if self.cfg_started && !self.cfg_client_seen && self.cfg_jobs_until.is_some_and(|t| Instant::now() < t) => {
                self.cfg_jobs_held.push((id, status, output));
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
                term_overrides: None,
                term_type: String::new(),
                esc_deadline: None,
                prompt: None,
                overlay: None,
                overlay_epoch: 0,
                message: None,
                message_ignore_keys: false,
                message_ignore_styles: true,
                prev: None,
                dirty: false,
                last_session: None,
                created: now,
                activity: now,
                activity_seq: activity_seq(),
                drag: None,
                slider_mpos: 0,
                title: String::new(),
                suspended: false,
                click: ClickState::default(),
                mouse_last: (0, 0, 0),
                mouse_drag_flag: 0,
                status_ranges: Vec::new(),
                control: 0,
                control_size: None,
                control_seq: 0,
                features: Default::default(),
                clipboard_query: None,
                focused: false,
                terminal: Default::default(),
                pending,
                queued_bytes,
                closed,
                backlog_since,
                ctl: Default::default(),
                user: None,
                denied: false,
                acl_readonly: false,
                io,
                pan: None,
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
        let Some(c) = self.clients.get(&cid) else { return };
        // server_client_lost: client-detached if attached, client-closed for terminals
        let detached = ((c.attached && c.session.is_some()) || self.ev.lost_attached.remove(&cid)).then(|| self.payload_client(cid));
        let closed = (c.control > 0 || c.ident.as_ref().is_some_and(|i| i.tty)).then(|| self.payload_client(cid));
        let Some(c) = self.clients.remove(&cid) else { return };
        c.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        if self.cfg_client == Some(cid) {
            // server_client_lost clears cfg_client; its held commands go with it
            self.cfg_client = None;
            self.cfg_cwd = None;
            self.cfg_held.clear();
        }
        let readonly = c.is_readonly();
        if c.attached {
            self.latest_lost(cid);
        }
        self.wait_for_client_lost(cid);
        self.ev.ctl_queue.remove(&cid);
        if let Some(Overlay::Popup(p)) = c.overlay {
            self.destroy_pane(p.pane);
        }
        if let Some(p) = detached {
            self.fire("client-detached", events::Payload { readonly, ..p });
        }
        if let Some(p) = closed {
            self.fire("client-closed", events::Payload { readonly, ..p });
        }
        if c.attached {
            if let Some(sid) = c.session {
                self.update_session_size(sid);
            }
        }
        // any client lost, a command client too (server_client_lost)
        self.check_destroy_unattached();
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
        // cfg_client_done: the first client's commands wait until the configuration has
        // loaded; later clients go on (they may have been started by it)
        if self.cfg_started && kind == C_COMMAND && !self.cfg_client_seen {
            self.cfg_client_seen = true;
            if self.cfg_finished {
                // the configuration loaded before any client connected: its causes go
                // where cfg_done would have put them with this client first
                self.cfg_show_causes(None);
            } else {
                self.cfg_client = Some(cid);
            }
        }
        if self.cfg_started && !self.cfg_finished && self.cfg_client == Some(cid) && matches!(kind, C_COMMAND | C_CONTROL_LINE) {
            self.cfg_held.push((cid, kind, payload));
            return;
        }
        match kind {
            C_IDENTIFY => {
                let a = decode_args(&payload);
                let get = |i: usize| a.get(i).cloned().unwrap_or_default();
                let env: Vec<(String, String)> = a.iter().skip(10).filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).collect();
                let ident = Identity {
                    cols: get(0).parse::<u16>().unwrap_or(80).min(MAX_SIZE),
                    rows: get(1).parse::<u16>().unwrap_or(24).min(MAX_SIZE),
                    tty: get(2) == "1",
                    cwd: get(3),
                    term: get(4),
                    pid: get(5).parse().unwrap_or(0),
                    // cmd_find_inside_pane falls back to $TMUX_PANE whichever server set it
                    ctx_pane: get(6).strip_prefix('%').and_then(|s| s.parse().ok()),
                    utf8: get(9) != "0",
                    env,
                };
                let control = get(7).parse().unwrap_or(0);
                let colorterm = ident.env.iter().find(|(k, _)| k == "COLORTERM").map(|(_, v)| v.as_str()).unwrap_or("");
                let table: Vec<String> = self.opts_server.0.iter().filter(|(k, _)| k.starts_with("terminal-features[")).map(|(_, v)| v.clone()).collect();
                let program = ident.env.iter().find(|(k, _)| k == "TERM_PROGRAM").map(|(_, v)| v.as_str()).unwrap_or("");
                let mut features = features::compute_named(&ident.term, program, colorterm, &table, &get(8));
                let overrides = self.opts_server.array_items("terminal-overrides").into_iter().map(|(_, v)| v).collect::<Vec<_>>();
                let caps = features::overrides(&ident.term, &overrides, &mut features);
                let vt100 = theme::vt100_like(&ident.term, &overrides);
                let terminal = control > 0 || ident.tty;
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.term_overrides = Some(overrides.clone());
                    c.term_caps = caps;
                    c.control = control;
                    c.features = features;
                    c.terminal.vt100 = vt100;
                    c.size = (ident.cols.max(1), ident.rows.max(1));
                    c.ident = Some(ident);
                }
                if terminal {
                    self.fire_client("client-created", cid);
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
                    match self.parse_string(&self.opt_server("default-client-command")) {
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
                // cmd_parse_from_arguments: every command is checked before any runs
                if let Some(e) = cmds.iter().find_map(|c| self.check_parsed(c).err()) {
                    self.finish_queue(Queue { cmds: VecDeque::new(), ctx, soft: 0 }, Some(e));
                    return;
                }
                self.run_queue(Queue { cmds: cmds.into_iter().filter(|c| !c.is_empty()).collect(), ctx , soft: 0 });
            }
            C_INPUT => {
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.activity = now_secs();
                    c.activity_seq = activity_seq();
                }
                // so does the session's (server_client_handle_key)
                if let Some(s) = self.clients.get(&cid).filter(|c| c.attached).and_then(|c| c.session).and_then(|s| self.sessions.get_mut(&s)) {
                    s.activity = now_secs();
                    s.activity_seq = activity_seq();
                }
                self.handle_input(cid, &payload);
                // a focus-out report does not make the client the latest
                if payload != b"\x1b[O" && !self.client_is_readonly(Some(cid)) {
                    self.update_latest(cid);
                }
            }
            C_RESIZE => {
                let Some((cols, rows)) = decode_size(&payload) else { return };
                let Some(c) = self.clients.get_mut(&cid) else { return };
                let old = c.size;
                c.size = (cols.clamp(1, MAX_SIZE), rows.clamp(1, MAX_SIZE));
                c.prev = None;
                c.dirty = true;
                let (new, control, session) = (c.size, c.control > 0, c.session);
                if control {
                    return;
                }
                // the resize a client sends once attached changes nothing (but fires)
                if new != old {
                    self.update_latest(cid);
                    if let Some(sid) = session {
                        self.update_session_size(sid);
                    }
                }
                // MSG_RESIZE fires it even when the size is the same (as after attach)
                if session.is_some() {
                    let mut p = self.payload_client(cid);
                    p.int("width", new.0 as i64).int("height", new.1 as i64).int("old_width", old.0 as i64).int("old_height", old.1 as i64);
                    self.fire("client-resized", p);
                }
                self.repeat_terminal_queries(cid);
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
                    // tty_start_tty: subscribe to theme changes and ask for the theme again;
                    // colours are asked for (and waited on) only once a theme report arrives
                    if c.control == 0 && c.terminal.vt100 {
                        c.send(S_OUTPUT, b"\x1b[?2031h\x1b[?996n");
                    }
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
        let colours = self.pane_colours(id);
        let allow_title = options::is_on(&self.opt_pane(id, "allow-set-title"));
        // the DECSCUSR number of the cursor-style choice
        let cursor_style = ["default", "blinking-block", "block", "blinking-underline", "underline", "blinking-bar", "bar"]
            .iter().position(|v| *v == self.opt_pane(id, "cursor-style")).unwrap_or(0) as u16;
        let p = self.panes.get_mut(&id).unwrap();
        p.parser.callbacks_mut().ext_option = ext_option;
        p.parser.callbacks_mut().cursor_style_opt = cursor_style;
        p.parser.set_input_buffer_size(input_limit);
        p.parser.screen_mut().set_alternate_screen_allowed(alternate);
        p.parser.screen_mut().set_codepoint_widths(widths, variation_wide);
        p.parser.callbacks_mut().colours.update(colours);
        let was_alternate = p.screen().alternate_screen();
        p.parser.screen_mut().set_scroll_on_clear(scroll_on_clear);
        // capture-pane -I: rows entering the history record when
        p.parser.screen_mut().set_time(now_secs());
        p.parser.callbacks_mut().current_title = p.title.clone();
        p.parser.callbacks_mut().suppress_title = !allow_title;
        p.parser.process(data);
        p.last_output = Instant::now();
        if !data.is_empty() {
            // input_parse_buffer: PANE_CHANGED
            self.name_changed.insert(id);
            let p = self.panes.get_mut(&id).unwrap();
            p.track.last_output_time = now_secs();
            p.track.output_generation += 1;
            p.track.unseen_changes |= p.mode.is_some();
        }
        let p = self.panes.get_mut(&id).unwrap();
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
        let enabled = p.screen().theme_updates();
        let cb = p.parser.callbacks_mut();
        cb.colours.notify(enabled, &mut cb.replies);
        let replies = std::mem::take(&mut cb.replies);
        let bell = std::mem::take(&mut cb.bell);
        let title = cb.title.take();
        let rename = cb.rename.take();
        let queries = std::mem::take(&mut cb.clipboard_queries);
        let sets = std::mem::take(&mut cb.clipboard_sets);
        let passthrough = std::mem::take(&mut cb.passthrough);
        let osc_events = std::mem::take(&mut cb.events);
        if let Some(cwd) = cb.cwd.take() {
            p.current_path = cwd;
            p.cwd_seq += 1;
        }
        p.send(replies);
        let clip_set = self.pane_input_requests(id, queries, sets, passthrough);
        if let Some(t) = title.filter(|_| options::is_on(&self.opt_pane(id, "allow-set-title"))) {
            self.panes.get_mut(&id).unwrap().title = t;
        }
        // allow-rename covers only ESC k (input_exit_rename); an empty name goes back to
        // automatic-rename's inherited value, or to an empty name when that is off
        if let Some(n) = rename.filter(|_| options::is_on(&self.opt_pane(id, "allow-rename"))) {
            if let Some(wid) = self.window_of(id) {
                *self.ev.renamed.entry(wid).or_default() += 1;
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
        if was_alternate != self.panes[&id].screen().alternate_screen() && self.opt_window(Some(wid), "pane-scrollbars") == "on" {
            self.apply_layout(wid);
        }
        if let Some(w) = self.windows.get_mut(&wid) {
            w.activity = now_secs();
            w.activity_us = now_us();
            // alerts_queue sets the window flags even when nothing monitors them, and
            // alerts_reset restarts the silence timer
            w.alerts |= ALERT_ACTIVITY | if bell { ALERT_BELL } else { 0 };
            w.alerts &= !ALERT_SILENCE;
            w.silence_reset_us = now_us();
        }
        self.pane_output_events(id, osc_events, clip_set);
        // alerts_enabled for the flags queued: then alerts_callback checks all of them
        if (bell && self.opt_window_on(Some(wid), "monitor-bell")) || self.opt_window_on(Some(wid), "monitor-activity") {
            self.alerts_callback(wid);
        }
        self.mark_window_dirty(wid);
    }

    /// alerts_check_all: each flag the window has whose monitor option is on.
    pub fn alerts_check_all(&mut self, wid: WinId) {
        let pending = self.windows.get(&wid).map_or(0, |w| w.alerts);
        if pending & ALERT_BELL != 0 && self.opt_window_on(Some(wid), "monitor-bell") {
            self.alert(wid, ALERT_BELL);
        }
        if pending & ALERT_ACTIVITY != 0 && self.opt_window_on(Some(wid), "monitor-activity") {
            self.alert(wid, ALERT_ACTIVITY);
        }
        if pending & ALERT_SILENCE != 0 && self.windows.contains_key(&wid) && self.opt_window_num(Some(wid), "monitor-silence") != 0 {
            self.alert(wid, ALERT_SILENCE);
        }
    }

    /// alerts_callback: the window is checked, then its flags are cleared.
    fn alerts_callback(&mut self, wid: WinId) {
        self.alerts_check_all(wid);
        if let Some(w) = self.windows.get_mut(&wid) {
            w.alerts &= !(ALERT_BELL | ALERT_ACTIVITY | ALERT_SILENCE);
        }
    }

    /// alerts_reset_all: monitor-silence changed, every silence timer starts again.
    pub fn alerts_reset_all(&mut self) {
        let now = now_us();
        for w in self.windows.values_mut() {
            w.alerts &= !ALERT_SILENCE;
            w.silence_reset_us = now;
        }
    }

    /// tmux alerts_check_{bell,activity,silence}: flags the window in every session where it is
    /// not the current window of an attached session, then, when {bell,activity,silence}-action
    /// applies (any, current or other), fires the hook and tells the session's clients once.
    pub fn alert(&mut self, wid: WinId, flag: u8) {
        let (opt, visual, hook, what) = match flag {
            ALERT_BELL => ("bell-action", "visual-bell", "alert-bell", "Bell"),
            ALERT_ACTIVITY => ("activity-action", "visual-activity", "alert-activity", "Activity"),
            _ => ("silence-action", "visual-silence", "alert-silence", "Silence"),
        };
        let mut alerted: Vec<SessId> = Vec::new();
        for (sid, idx) in self.winlinks_of(wid) {
            let attached = self.clients.values().any(|c| c.attached && c.session == Some(sid));
            let Some(s) = self.sessions.get_mut(&sid) else { continue };
            // bells are allowed even with an existing bell; activity and silence are not
            if flag != ALERT_BELL && s.alerts.get(&idx).is_some_and(|a| a & flag != 0) {
                continue;
            }
            let current = s.active == idx;
            if !current || !attached {
                *s.alerts.entry(idx).or_insert(0) |= flag;
                self.mark_session_dirty(sid);
            }
            let applies = match self.opt_session(Some(sid), opt).as_str() {
                "any" => true,
                "current" => current,
                "other" => !current,
                _ => false,
            };
            if !applies {
                continue;
            }
            if let Some(w) = self.sessions.get(&sid).and_then(|s| s.links.get(&idx)).copied() {
                self.fire_winlink(hook, sid, idx, w);
            }
            if alerted.contains(&sid) {
                continue;
            }
            alerted.push(sid);
            // alerts_set_message: off rings, on shows a message instead, both does both
            let vis = self.opt_session(Some(sid), visual);
            let msg = if current { format!("{what} in current window") } else { format!("{what} in window {idx}") };
            let cids: Vec<ClientId> = self.clients.values().filter(|c| c.attached && c.session == Some(sid) && c.control == 0).map(|c| c.id).collect();
            for cid in cids {
                if vis == "off" || vis == "both" {
                    if let Some(c) = self.clients.get(&cid) {
                        c.send(S_OUTPUT, b"\x07");
                    }
                }
                if vis != "off" {
                    self.set_message(cid, &msg);
                }
            }
            self.mark_session_dirty(sid);
        }
    }

    fn pane_exited(&mut self, id: PaneId, status: Option<i32>) {
        log(&format!("pane %{id} exited with {status:?}"));
        // new-pane -W continues after the pane is handled: tmux's cmdq_continue only wakes
        // the client queue, which runs after the global queue (pane-exited, the layout hooks)
        let waits = self.pane_waits.remove(&id);
        self.pane_exited_now(id, status);
        if let Some(q) = waits {
            self.resume_pane_waits(q, Some(status.unwrap_or(129)));
        }
    }

    fn pane_exited_now(&mut self, id: PaneId, status: Option<i32>) {
        // popup?
        let popup_owner = self.clients.values().find(|c| matches!(&c.overlay, Some(Overlay::Popup(p)) if p.pane == id)).map(|c| c.id);
        if let Some(cid) = popup_owner {
            if self.popup_panes.values().any(|p| *p == id) {
                // popup_job_complete_cb: WEXITSTATUS, or WTERMSIG for a command a signal
                // ended (Windows has no signals: a process ended by TerminateProcess
                // reports the exit code it was given)
                self.popup_status.insert(id, status.map_or(129, |s| exit_signal(s).unwrap_or(s)));
            }
            // popup_job_complete_cb: -E closes it, -EE only for status 0
            let close = matches!(&self.clients[&cid].overlay, Some(Overlay::Popup(p)) if p.close_on_exit || (p.close_exit_zero && status == Some(0)));
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
        // server_child_exited: spawn_editor_finish before the pane is destroyed
        if let Some((cid, ed)) = self.editors.remove(&id) {
            self.editor_done(cid, ed, status);
        }
        let remain = self.opt_pane(id, "remain-on-exit");
        // "key" and "failed-key" keep it until a key is pressed (see Server::dead_key)
        let keep = matches!(remain.as_str(), "on" | "key") || (matches!(remain.as_str(), "failed" | "failed-key") && status.unwrap_or(0) != 0);
        if keep {
            let fmt = self.opt_pane(id, "remain-on-exit-format");
            if let Some(p) = self.panes.get_mut(&id) {
                p.dead = Some((status, now_secs()));
            }
            self.fire_pane_exit("pane-died", id, status);
            let msg = self.expand_for_pane(id, &fmt);
            if let Some(p) = self.panes.get_mut(&id) {
                if !fmt.is_empty() {
                    let cells = crate::format::layout_line(&crate::format::spans(&msg, Default::default()), p.size.0 as usize, Default::default());
                    let line = render::line_for_parser(&Line { cells, wrapped: false, ..Default::default() });
                    // format_draw leaves the cursor where it started, at the line's start
                    p.parser.process(format!("\x1b[r\x1b[{};1H\r\n\x1b[0m{line}\x1b[0m\r", p.size.1).as_bytes());
                }
                p.parser.process(b"\x1b[?25l");
            }
            if let Some(wid) = self.window_of(id) {
                self.mark_window_dirty(wid);
            }
            return;
        }
        self.fire_pane_exit("pane-exited", id, status);
        // server_destroy_pane closes the pane's layout cell (window-layout-changed)
        // before killing a window left empty; other windows' changes are found later
        if let Some(wid) = self.window_of(id).filter(|w| self.windows.get(w).is_some_and(|w| w.panes() == [id])) {
            self.fire_window("window-layout-changed", wid);
        }
        self.kill_pane(id);
    }

    fn job_done(&mut self, id: u32, status: i32, output: String) {
        let Some(parked) = self.jobs.remove(&id) else { return };
        let mut queue = parked.queue;
        match parked.kind {
            JobKind::RunShell { pane, background, .. } => {
                // cmd_run_shell_callback: a failure is printed after the output, and
                // a command-line client exits with the status
                let mut output = output;
                if status != 0 {
                    let msg = if status < 0 { format!("'{}' terminated by signal {}", parked.cmd, -status) } else { format!("'{}' returned {status}", parked.cmd) };
                    if !output.is_empty() && !output.ends_with('\n') {
                        output.push('\n');
                    }
                    output.push_str(&msg);
                    output.push('\n');
                }
                // each run-shell sets the exit status (the last one wins)
                if !background {
                    queue.ctx.retval = if status < 0 { 128 - status } else { status };
                }
                // cmd_run_shell_print: a -t pane always gets the output in view mode,
                // even when the command waits (a command-line client prints nothing)
                let target = pane.filter(|p| self.panes.contains_key(p));
                if let Some(p) = target.filter(|_| !output.is_empty()) {
                    self.view_output(p, &output);
                } else if !output.is_empty() && !background {
                    if queue.ctx.reply.is_some_and(|c| !self.clients.get(&c).is_some_and(|c| c.attached)) {
                        queue.ctx.out.push_str(&output);
                    } else if let (Some(cid), Some(_)) = (queue.ctx.client, queue.ctx.control_seq) {
                        // cmdq_print to a control client: bare lines after its %end
                        let mut text = output.clone();
                        if !text.ends_with('\n') {
                            text.push('\n');
                        }
                        self.ev.ctl_queue.entry(cid).or_default().push_str(&text);
                    } else if let Some(p) = queue.ctx.pane {
                        self.view_output(p, &output);
                    }
                } else if !output.is_empty() {
                    // cmd_run_shell_print without a queue item: the target client's
                    // current pane, else the current one
                    let tc = queue.ctx.client.filter(|c| self.clients.get(c).is_some_and(|c| c.attached))
                        .or_else(|| self.resolve_client(&queue.ctx, None).ok());
                    let p = tc.and_then(|c| self.client_active_pane(c))
                        .or_else(|| self.current(&queue.ctx).pane);
                    if let Some(p) = p {
                        self.view_output(p, &output);
                    }
                }
            }
            JobKind::IfShell { then, otherwise } => {
                let branch = if status == 0 { Some(then) } else { otherwise };
                if let Some(b) = branch {
                    match self.parse_string(&b) {
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

    /// Shows command output in a pane in view mode (copy mode over the text), like tmux:
    /// window_copy_add with parse, so each line's escape sequences, CR and BS apply.
    pub fn view_output(&mut self, pane: PaneId, text: &str) {
        self.view_add(pane, text, true);
    }

    /// window_copy_add(wp, parse, ...): `parse` runs each line through the terminal parser
    /// (input_parse_screen); without it the text is written as it is (cfg_show_causes).
    pub fn view_add(&mut self, pane: PaneId, text: &str, parse: bool) {
        let Some(p) = self.panes.get_mut(&pane) else { return };
        let (w, h) = p.size;
        // window_copy_add: output for a pane already in view mode goes after what it shows
        let mut lines: Vec<Line> = match &p.mode {
            Some(PaneMode::Copy(c)) if c.is_view_mode() => {
                let mut old = c.backing_screen().0.to_vec();
                while old.last().is_some_and(|l| !l.wrapped && l.cells.iter().all(|c| c.text.is_empty() || c.text == " ")) {
                    old.pop();
                }
                old
            }
            _ => Vec::new(),
        };
        if parse {
            lines.extend(Self::parse_view_lines(text.trim_end_matches('\n'), w));
        }
        for l in text.trim_end_matches('\n').split('\n').filter(|_| !parse) {
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
            lines.push(Line { cells, wrapped: false, ..Default::default() });
        }
        while lines.len() < h as usize {
            lines.push(Line { cells: vec![crate::cells::Cell::default(); w as usize], wrapped: false, ..Default::default() });
        }
        let vi = self.opt_window(self.window_of(pane), "mode-keys") == "vi";
        let mut cm = CopyMode::new(lines, w, h, (0, 0), vi);
        cm.set_view_mode();
        // Start at the top of the output.
        cm.run("history-top", &[]);
        // output replaces the view on top; any other mode stays under it
        if let Some(p) = self.panes.get_mut(&pane).filter(|p| matches!(&p.mode, Some(PaneMode::Copy(c)) if c.is_view_mode())) {
            p.mode = Some(PaneMode::Copy(Box::new(cm)));
            if let Some(wid) = self.window_of(pane) {
                self.mark_window_dirty(wid);
            }
        } else {
            self.set_pane_mode(pane, PaneMode::Copy(Box::new(cm)));
        }
    }

    /// The rows window_copy_vadd writes for `text` with parse: a CR and LF before each line
    /// after the first, then the line's bytes through the parser, up to the cursor's row.
    fn parse_view_lines(text: &str, w: u16) -> Vec<Line> {
        let nz = |n: u16| std::num::NonZeroU16::new(n.max(1)).unwrap();
        let rows = text.split('\n').count();
        let mut parser = vt100::Parser::new(nz(1), nz(w), text.len() + rows + 1);
        for (i, l) in text.split('\n').enumerate() {
            if i > 0 {
                parser.process(b"\r\n");
            }
            parser.process(l.as_bytes());
        }
        let screen = parser.screen();
        let last = screen.history_rows_len() - screen.size().0.get() as usize + screen.cursor_position().0 as usize;
        (0..=last).map(|i| crate::cells::line_at(screen, i)).collect()
    }

    pub fn next_timeout(&self) -> Duration {
        let mut t = Duration::from_millis(1000);
        let now = Instant::now();
        for d in self.panes.values().filter_map(|p| p.scrollbar_until) { t = t.min(d.saturating_duration_since(now)); }
        for d in self.copy_drag_timers.values().chain(self.delayed.iter().map(|(d, _)| d)) {
            t = t.min(d.saturating_duration_since(now));
        }
        for c in self.clients.values() {
            for d in [c.esc_deadline, c.repeat_until, c.message.as_ref().map(|m| m.1), c.click.deadline].into_iter().flatten() {
                t = t.min(d.saturating_duration_since(now));
            }
        }
        // display-panes closes by itself
        for p in self.panes.values() {
            if let Some(PaneMode::Panes(m)) = &p.mode {
                if let Some(until) = m.until {
                    t = t.min(until.saturating_duration_since(now));
                }
            }
        }
        for d in self.animation_deadlines.borrow().values() {
            t = t.min(d.saturating_duration_since(now));
        }
        if let Some(d) = self.copy_refresh_deadline() {
            t = t.min(d.saturating_duration_since(now));
        }
        for d in self.name_state.values().filter_map(|n| n.timer) {
            t = t.min(d.saturating_duration_since(now));
        }
        // a closed prompt's or popup's queue goes on in the next turn, at once
        if self.waits_resumable() {
            t = Duration::ZERO;
        }
        let now_us = now_us();
        for (_, deadline, _) in self.silence_timers() {
            t = t.min(Duration::from_micros(deadline.saturating_sub(now_us)));
        }
        t.max(Duration::from_millis(5))
    }

    /// The windows with a silence timer (monitor-silence) and when it expires, in
    /// microseconds, in window order.
    fn silence_timers(&self) -> Vec<(WinId, u64, u64)> {
        let mut v: Vec<(WinId, u64, u64)> = self.windows.values().filter_map(|w| {
            let secs = self.opt_window_num(Some(w.id), "monitor-silence");
            let period = (secs > 0).then(|| secs as u64 * 1_000_000)?;
            Some((w.id, w.silence_reset_us.saturating_add(period), period))
        }).collect();
        v.sort_unstable();
        v
    }

    /// alerts_timer: monitor-silence seconds after the last reset (output or the timer
    /// itself) the window is queued for silence and the timer starts again, so the
    /// current window of an attached session (never flagged) alerts again.
    fn silence_check(&mut self) {
        let now = now_us();
        for (wid, deadline, period) in self.silence_timers() {
            // (alert hooks may have removed windows)
            let Some(w) = self.windows.get_mut(&wid).filter(|_| now >= deadline) else { continue };
            w.silence_reset_us = if now - deadline < period { deadline } else { now };
            w.alerts |= ALERT_SILENCE;
            self.alerts_callback(wid);
        }
    }

    fn timers(&mut self) {
        self.input_timers();
        self.silence_check();
        if self.cfg_jobs_until.is_some_and(|t| Instant::now() >= t) {
            self.cfg_release_jobs();
        }
        let now = Instant::now();
        let expired: Vec<_> = self.panes.iter_mut().filter_map(|(id, p)| {
            if p.scrollbar_until.is_some_and(|d| d <= now) { p.scrollbar_until = None; Some(*id) } else { None }
        }).collect();
        for id in expired { if let Some(w) = self.window_of(id) { self.mark_window_dirty(w); } }
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
            // server_client_repeat_timer: back to the default table (the session's key-table)
            let default_table = self.clients.get(&cid).and_then(|c| c.session).map(|s| self.default_key_table(s)).unwrap_or_else(|| "root".into());
            let Some(c) = self.clients.get_mut(&cid) else { continue };
            if c.repeat_until.is_some_and(|d| d <= now) {
                c.repeat_until = None;
                if c.key_table != default_table {
                    c.set_key_table(&default_table);
                    c.dirty = true;
                }
            }
            if c.message.as_ref().is_some_and(|m| m.1 <= now) {
                c.message = None;
                c.dirty = true;
            }
        }
        let expired: Vec<PaneId> = self.panes.values().filter(|p| matches!(&p.mode, Some(PaneMode::Panes(m)) if m.until.is_some_and(|u| u <= now))).map(|p| p.id).collect();
        for p in expired {
            self.exit_mode(p);
        }
        if !self.copy_drag_timers.is_empty() {
            self.copy_drag_timers_due(now);
        }
        self.copy_refresh_timers(now);
        if self.delayed.iter().any(|(d, _)| *d <= now) {
            let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.delayed).into_iter().partition(|(d, _)| *d <= now);
            self.delayed = rest;
            for (_, q) in due {
                self.run_queue(q);
            }
        }
        if !self.menus.is_empty() {
            let windows = &self.windows;
            self.menus.retain(|w, _| windows.contains_key(w));
        }
        self.drain_clients();
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
        self.check_monitors();
        self.request_pane_info();
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
            // (tmux reads these as it checks the name: a change found here is checked
            // like output)
            let mut changed = self.name_known.insert((r.id, r.generation));
            if let Some(c) = r.cmd.filter(|c| *c != p.current_cmd) {
                p.current_cmd = c;
                // (on Unix the name check reads the program itself, as tmux does)
                changed |= !cfg!(unix);
            }
            // a directory the pane reported meanwhile is newer than this lookup
            if let Some(path) = r.path.filter(|path| p.cwd_seq == r.cwd_seq && *path != p.current_path) {
                p.current_path = path;
                changed = true;
            }
            if changed {
                self.name_changed.insert(r.id);
            }
        }
    }

    /// osdep_get_name for a pane's name check: on Unix its foreground program, read now
    /// (quick). False where it could not be read (Windows uses the background lookup).
    fn read_program_now(&mut self, pane: PaneId) -> bool {
        #[cfg(unix)]
        {
            let Some(p) = self.panes.get(&pane).filter(|p| p.dead.is_none()) else { return false };
            let Some(cmd) = procinfo::pane_programs(&[(p.pid, p.fg_pgrp())]).pop().and_then(|r| r.0) else { return false };
            let generation = p.generation;
            self.panes.get_mut(&pane).unwrap().current_cmd = cmd;
            self.name_known.insert((pane, generation));
            true
        }
        #[cfg(not(unix))]
        {
            let _ = pane;
            false
        }
    }

    /// check_window_name for every window, each time round the loop: a window whose
    /// active pane changed is renamed at once, or when NAME_INTERVAL has passed since its
    /// name was last checked (its timer).
    pub(crate) fn automatic_rename(&mut self) {
        let now = Instant::now();
        let wids: Vec<WinId> = self.windows.keys().copied().collect();
        self.name_state.retain(|w, _| wids.contains(w));
        let panes = &self.panes;
        self.name_changed.retain(|p| panes.contains_key(p));
        self.name_known.retain(|(p, g)| panes.get(p).is_some_and(|p| p.generation == *g));
        for wid in wids {
            let Some(active) = self.windows.get(&wid).map(|w| w.active) else { continue };
            let seen = self.panes.get(&active).map(|p| (active, p.mode.as_ref().map(std::mem::discriminant), p.dead.is_some()));
            let auto = self.opt_window_on(Some(wid), "automatic-rename");
            let st = self.name_state.entry(wid).or_default();
            if st.seen != seen || (auto && !st.auto) {
                st.seen = seen;
                self.name_changed.insert(active);
            }
            st.auto = auto;
            // (a timer that has fired is gone: it is a one-shot evtimer)
            if st.timer.is_some_and(|t| t <= now) {
                st.timer = None;
            }
            if !auto || !self.name_changed.contains(&active) {
                continue;
            }
            // name_time_expired: checked less than NAME_INTERVAL ago, the timer does it
            if let Some(t) = st.time.filter(|t| now.duration_since(*t) <= NAME_INTERVAL) {
                st.timer.get_or_insert(t + NAME_INTERVAL);
                continue;
            }
            // tmux reads what the pane runs as it checks the name (osdep_get_name); where
            // that is slow (Windows) the name waits for the background lookup to find it
            let known = self.read_program_now(active)
                || self.panes.get(&active).is_none_or(|p| p.dead.is_some() || self.name_known.contains(&(active, p.generation)));
            if !known {
                continue;
            }
            let st = self.name_state.entry(wid).or_default();
            st.time = Some(now);
            st.timer = None;
            self.name_changed.remove(&active);
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
                c.finish_exit();
            } else {
                c.send(S_REPLY, &encode_reply(0, ""));
            }
        }
        thread::sleep(Duration::from_millis(150));
    }

    // ---- panes ------------------------------------------------------------------------

    /// environ_for_session: the global environment then the session's, without hidden
    /// variables.
    pub fn session_env(&self, sid: Option<SessId>) -> Vec<(String, Option<String>)> {
        let mut env: BTreeMap<String, Option<String>> = BTreeMap::new();
        for (k, v) in &self.env {
            if !self.env_hidden.contains(&(GLOBAL_ENV, k.clone())) {
                env.insert(k.clone(), v.clone());
            }
        }
        if let Some(s) = sid.and_then(|s| self.sessions.get(&s)) {
            for (k, v) in &s.env {
                if self.env_hidden.contains(&(s.id, k.clone())) {
                    env.remove(k);
                } else {
                    env.insert(k.clone(), v.clone());
                }
            }
        }
        env.into_iter().collect()
    }

    /// Spawns a process in a new pty. `cmd`: empty = default-command / shell; one arg = via shell; more = argv.
    pub fn spawn_pane(&mut self, sid: Option<SessId>, cols: u16, rows: u16, cmd: &[String], cwd: Option<&str>, extra_env: &[(String, String)]) -> Result<PaneId, String> {
        let id = self.new_pane_id();
        self.spawn_into(id, 0, sid, cols, rows, cmd, cwd, extra_env)?;
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_into(&mut self, id: PaneId, generation: u32, sid: Option<SessId>, cols: u16, rows: u16, cmd: &[String], cwd: Option<&str>, extra_env: &[(String, String)]) -> Result<(), String> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        // an empty pane (new-window -E and friends) gets a terminal but no process
        let empty = cmds::is_empty_pane(cmd);
        let empty_arg = empty && cmd[0] == cmds::EMPTY_PANE_ARG;
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
        // spawn: a directory that cannot be entered gives home, then the root
        let is_dir = |d: &String| !d.is_empty() && std::path::Path::new(d).is_dir();
        let wanted = cwd.map(String::from).filter(|d| !d.is_empty()).or_else(|| sid.and_then(|s| self.sessions.get(&s)).map(|s| s.cwd.clone()));
        let dir = wanted
            .filter(is_dir)
            .or_else(|| procinfo::home_dir().filter(is_dir))
            .unwrap_or_else(|| if cfg!(windows) { ".".into() } else { "/".into() });
        cb.cwd(&dir);
        let dir_for_event = dir.clone();
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
                let status = child.wait().ok().map(|s| exit_status(&s));
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
                start_path: dir.clone(),
                title,
                dead: None,
                scrollbar_until: None,
                mode: None,
                mode_stack: Vec::new(),
                prompt: None,
                options: existing_opts,
                pipe: None,
                last_output: Instant::now(),
                created: now,
                current_cmd: prog,
                // the process's directory (osdep_get_cwd): "sub/.." or "/." resolved
                current_path: if cfg!(unix) { std::fs::canonicalize(&dir).map(|p| p.to_string_lossy().into_owned()).unwrap_or(dir) } else { dir },
                cwd_seq: 0,
                host_pid,
                track: PaneTrack { argv: start_argv, ..Default::default() },
                fflags: self.panes.get(&id).map_or(0, |p| p.fflags),
            },
        );
        let respawn = previous.is_some();
        if let Some(mut old) = previous {
            // screen_reinit: a respawned pane keeps its screen with the history; only the
            // visible lines are cleared and the modes, scroll region and cursor reset
            // (the input parser and its state start again)
            if let Some(p) = self.panes.get_mut(&id) {
                std::mem::swap(&mut p.parser, &mut old.parser);
                let cb = std::mem::replace(old.parser.callbacks_mut(), PaneCb::default());
                *p.parser.callbacks_mut() = cb;
                if p.parser.screen().size() != (nz(rows), nz(cols)) {
                    p.parser.set_size(nz(rows), nz(cols));
                }
                p.parser.screen_mut().set_scroll_on_clear(false);
                p.parser.process(SCREEN_REINIT);
                p.parser.callbacks_mut().replies.clear();
                p.parser.callbacks_mut().events.clear();
            }
            release(old); // respawn: the old pty
        }
        // pane-created fires once the pane is in a window (popups never are)
        let command = if empty_arg {
            events::stringify_argv(&[String::new()])
        } else if !cmd.is_empty() {
            events::stringify_argv(cmd)
        } else if !default_cmd.is_empty() {
            default_cmd
        } else {
            shell
        };
        self.ev.spawned.push(events::Spawned { pane: id, respawn, empty, command, cwd: dir_for_event });
        Ok(())
    }

    /// Removes a pane's process and state without touching layouts.
    pub fn destroy_pane(&mut self, id: PaneId) {
        // new-pane -W waiting for a pane that never exited: 128 + SIGHUP
        let status = self.panes.get(&id).and_then(|p| p.dead).map(|d| d.0.unwrap_or(129));
        self.finish_pane_waits(id, status);
        if let Some(mut p) = self.panes.remove(&id) {
            p.kill();
            release(p);
        }
        // window_pane_destroy: an editor killed with its pane changes nothing
        if let Some((_, ed)) = self.editors.remove(&id) {
            let _ = std::fs::remove_file(&ed.path);
        }
        if self.marked == Some(id) {
            self.marked = None;
        }
    }

    /// Kills a pane and removes it from its window (closing the window/session if empty).
    pub fn kill_pane(&mut self, id: PaneId) {
        let wid = self.window_of(id);
        // server_kill_pane: window_push_zoom unzooms (with its events) before closing the
        // pane's cell, and window_pop_zoom zooms again a window the pane floated above
        let mut rezoom = None;
        if let Some(wid) = wid.filter(|w| self.windows.get(w).is_some_and(|w| w.panes().len() > 1)) {
            let was = self.zoomed_pane(wid);
            if self.windows[&wid].is_floating(id) && self.over_zoom(id) && was.is_some_and(|z| z != id) {
                rezoom = was;
            }
            self.unzoom_window(wid);
        }
        self.destroy_pane(id);
        let Some(wid) = wid else { return };
        if !self.windows.contains_key(&wid) {
            return;
        }
        if self.detach_pane(wid, id) {
            self.apply_layout(wid);
            self.mark_window_dirty(wid);
            // layout_close_pane fires window-layout-changed before window_remove_pane
            // picks the new active pane
            self.flush_window_layout(wid);
            if rezoom.is_some() {
                self.rezoom_window(wid, rezoom);
            }
        } else {
            self.kill_window(wid);
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
        }
        if self.links_of(wid).is_empty() {
            if let Some(w) = self.windows.remove(&wid) {
                for p in w.panes() {
                    self.destroy_pane(p);
                }
            }
        } else if self.sessions.contains_key(&sid) {
            // server_unlink_window: a window still linked elsewhere is sized for the
            // clients that show it now
            self.update_session_size(sid);
        }
    }

    /// Propagates window-list changes between the sessions of each group: the member
    /// whose links differ from the group's last synced list is the one that changed.
    pub fn sync_groups(&mut self) {
        let gids: Vec<u32> = self.groups.keys().copied().collect();
        for gid in gids {
            let members: Vec<SessId> = self.sessions.values().filter(|s| s.group == Some(gid)).map(|s| s.id).collect();
            if members.is_empty() {
                self.groups.remove(&gid);
                continue;
            }
            if members.len() == 1 {
                self.groups.insert(gid, self.sessions[&members[0]].links.clone());
                continue;
            }
            let canonical = self.groups[&gid].clone();
            let Some(changed) = members.iter().find(|m| self.sessions[m].links != canonical).copied() else { continue };
            let links = self.sessions[&changed].links.clone();
            // session_group_synchronize_from: the others are linked again, oldest first
            let mut others: Vec<(u64, SessId)> = members.iter().filter(|m| **m != changed).map(|m| (self.sessions[m].created, *m)).collect();
            others.sort_unstable();
            self.ev.synced.push((changed, others.into_iter().map(|o| o.1).collect()));
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

    /// server_destroy_session: where the clients of a destroyed session go by its
    /// detach-on-destroy (off: the newest other session; no-detached: the newest one
    /// with no clients; previous and next: by name), and where a client that must not
    /// detach goes when that finds none (the newest other session, with on or no-detached).
    fn destroy_session_targets(&self, sid: SessId) -> (Option<SessId>, Option<SessId>) {
        let Some(s) = self.sessions.get(&sid) else { return (None, None) };
        let mode = s.options.get("detach-on-destroy").map(String::from).unwrap_or_else(|| self.opt_session(None, "detach-on-destroy"));
        let newer = |detached: bool| {
            self.sessions
                .values()
                .filter(|o| o.id != sid && !(detached && self.clients.values().any(|c| c.attached && c.session == Some(o.id))))
                .max_by_key(|o| o.activity_seq)
                .map(|o| o.id)
        };
        let by_name = |step: isize| {
            let mut names: Vec<&Session> = self.sessions.values().collect();
            names.sort_by(|a, b| a.name.cmp(&b.name));
            let i = names.iter().position(|o| o.id == sid)? as isize;
            let n = names[(i + step).rem_euclid(names.len() as isize) as usize].id;
            (n != sid).then_some(n)
        };
        let s_new = match mode.as_str() {
            "off" => newer(false),
            "no-detached" => newer(true),
            "previous" => by_name(-1),
            "next" => by_name(1),
            _ => None,
        };
        let cs_new = if s_new.is_none() && matches!(mode.as_str(), "on" | "no-detached") { newer(false) } else { None };
        (s_new, cs_new)
    }

    pub fn kill_session(&mut self, sid: SessId, _empty: bool) {
        // session-closed fires with the session's unlinks once the change is seen
        // (session_destroy), after what the command did before
        let closed = self.sessions.contains_key(&sid).then(|| self.payload_session(sid));
        let (s_new, cs_new) = self.destroy_session_targets(sid);
        let Some(s) = self.sessions.remove(&sid) else { return };
        if let Some(mut p) = closed {
            p.target = vars::Target::default();
            self.ev.closed.push((sid, p));
        }
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
            // a no-detach-on-destroy client goes to any other session
            let next = s_new.or(cs_new.filter(|_| self.clients.get(&cid).is_some_and(|c| c.ctl.no_detach_on_destroy))).filter(|n| self.sessions.contains_key(n));
            match next {
                Some(n) => self.switch_client(cid, n),
                None => {
                    if let Some(c) = self.clients.get_mut(&cid) {
                        c.session = None;
                        // still attached when it is lost: client-detached then
                        if std::mem::replace(&mut c.attached, false) {
                            self.ev.lost_attached.insert(cid);
                        }
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
    }

    /// server_check_unattached: every unattached session (in name order) whose
    /// destroy-unattached says so goes. keep-last keeps the last session of a group and
    /// sessions not in a group; keep-group keeps only the last session of a group.
    fn check_destroy_unattached(&mut self) {
        let mut order: Vec<(String, SessId)> = self.sessions.values().map(|s| (s.name.clone(), s.id)).collect();
        order.sort();
        for (_, sid) in order {
            let Some(group) = self.sessions.get(&sid).map(|s| s.group) else { continue };
            if self.clients.values().any(|c| c.attached && c.session == Some(sid)) {
                continue;
            }
            let count = group.map(|g| self.sessions.values().filter(|s| s.group == Some(g)).count());
            let destroy = match self.opt_session(Some(sid), "destroy-unattached").as_str() {
                "on" => true,
                "keep-last" => count.is_some_and(|n| n > 1),
                "keep-group" => count != Some(1),
                _ => false,
            };
            if destroy {
                self.kill_session(sid, false);
            }
        }
    }

    pub fn attach_client(&mut self, cid: ClientId, sid: SessId) {
        self.attach_client_env(cid, sid, true);
    }

    /// attach_client; `update_env` false (attach-session/switch-client -E) skips
    /// update-environment.
    pub fn attach_client_env(&mut self, cid: ClientId, sid: SessId, update_env: bool) {
        let clear = options::is_on(&self.opt_server("clear-on-attach"));
        // server_client_set_key_table(c, NULL): the session's key-table
        let default_table = self.default_key_table(sid);
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let was_attached = c.attached;
        c.focused = true;
        let old_session = c.session.filter(|_| was_attached);
        if c.session != Some(sid) {
            c.last_session = c.session;
        }
        c.session = Some(sid);
        c.attached = true;
        c.prev = None;
        c.dirty = true;
        c.set_key_table(&default_table);
        if !was_attached {
            c.send(S_ATTACHED, if clear { b"" } else { b"no-clear" });
            self.start_terminal_queries(cid);
        }
        let writable = !self.client_is_readonly(Some(cid));
        let now = now_secs();
        // update-environment
        let names: Vec<String> = self.opt_array(Some(sid), "update-environment");
        let env: Vec<(String, String)> = self.clients.get(&cid).and_then(|c| c.ident.as_ref()).map(|i| i.env.clone()).unwrap_or_default();
        if let Some(s) = self.sessions.get_mut(&sid) {
            s.last_attached = now;
            s.activity = now;
            s.activity_seq = activity_seq();
            if writable && update_env {
                s.env.extend(cmds::environ_update(&names, &env));
            }
            let active = s.active;
            if writable { s.alerts.remove(&active); }
        }
        // alerts_check_session: flags left on the session's windows
        let wids: Vec<WinId> = self.sessions.get(&sid).map(|s| s.links.values().copied().collect()).unwrap_or_default();
        for wid in wids {
            self.alerts_check_all(wid);
        }
        // server_client_set_session: the client is focused (set above) and its window's
        // latest, then the sizes are recalculated
        if let Some((_, wid)) = self.session_window(sid) {
            self.ev.latest.insert(wid, cid);
        }
        self.update_session_size(sid);
        // the old session's current window, then the new one's
        for s in [old_session, Some(sid)].into_iter().flatten() {
            if let Some((_, wid)) = self.session_window(s) {
                self.update_window_focus(wid);
            }
        }
        let mut p = self.payload_client(cid);
        p.set("new_session", p.get("session").cloned().unwrap_or(events::Val::Session(sid, String::new())));
        if let Some(old) = old_session.filter(|o| self.sessions.contains_key(o)) {
            let name = self.sessions[&old].name.clone();
            p.set("old_session", events::Val::Session(old, name));
        }
        self.fire("client-session-changed", p);
        if !was_attached {
            self.fire_client("client-attached", cid);
            // server_client_set_session checks every session (switch_client_env does
            // this after the old session's size is updated)
            self.check_destroy_unattached();
        }
    }

    pub fn opt_array(&self, sid: Option<SessId>, name: &str) -> Vec<String> {
        let prefix = format!("{name}[");
        let src = sid.and_then(|s| self.sessions.get(&s)).map(|s| &s.options).filter(|o| o.has(name)).unwrap_or(&self.opts_session);
        src.0.iter().filter(|(k, _)| k.starts_with(&prefix)).flat_map(|(_, v)| v.split_whitespace().map(String::from).collect::<Vec<_>>()).collect()
    }

    pub fn switch_client(&mut self, cid: ClientId, sid: SessId) {
        self.switch_client_env(cid, sid, true);
    }

    pub fn switch_client_env(&mut self, cid: ClientId, sid: SessId, update_env: bool) {
        let old = self.clients.get(&cid).and_then(|c| c.session);
        self.attach_client_env(cid, sid, update_env);
        if let Some(o) = old.filter(|o| *o != sid) {
            self.update_session_size(o);
            self.check_destroy_unattached();
        }
    }

    pub fn detach_client(&mut self, cid: ClientId, msg: &str) {
        self.detach_client_exec(cid, msg, None);
    }

    /// detach_client; with `exec` (detach-client -E) the client runs that command with
    /// the session's default-shell instead of printing the message.
    pub fn detach_client_exec(&mut self, cid: ClientId, msg: &str, exec: Option<&str>) {
        let shell = self.clients.get(&cid).and_then(|c| c.session).map(|s| self.opt_session(Some(s), "default-shell")).unwrap_or_default();
        let Some(c) = self.clients.get_mut(&cid) else { return };
        if !c.attached {
            return;
        }
        let sid = c.session;
        let readonly = c.is_readonly();
        let detached = self.payload_client(cid);
        let Some(c) = self.clients.get_mut(&cid) else { return };
        c.attached = false;
        c.session = None;
        match exec.filter(|e| !e.is_empty() && c.control == 0) {
            Some(cmd) => c.send(S_EXEC, format!("{cmd}\0{shell}").as_bytes()),
            None => c.send_exit(msg),
        }
        // a detached client answers MSG_EXITING, which takes its focus away; a
        // control client's empty line just exits
        if !msg.is_empty() || exec.is_some_and(|e| !e.is_empty()) {
            self.update_focus();
        }
        if let Some(sid) = sid {
            self.latest_lost(cid);
            self.update_session_size(sid);
            self.fire("client-detached", events::Payload { readonly, ..detached });
            self.check_destroy_unattached();
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
        if let Some(z) = self.zoomed_pane(wid) {
            return (vec![(z, area)], vec![]);
        }
        let (mut panes, mut seps) = (vec![], vec![]);
        w.layout.layout(area, &mut panes, &mut seps);
        (panes, seps)
    }

    /// Content rect of a pane (minus a pane-border-status line).
    pub fn content_rect(&self, wid: WinId, r: Rect) -> Rect {
        match self.rects(wid).0.into_iter().find(|(_, rect)| *rect == r) {
            Some((id, _)) => self.pane_content_rect(wid, id, r),
            None => self.scrollbar_area(wid, r),
        }
    }

    /// The content of pane `id` in layout cell `r` (also a cell of the unzoomed
    /// layout for a pane hidden by zoom): less a pane status line and a reserved
    /// scrollbar.
    pub fn pane_content_rect(&self, wid: WinId, id: PaneId, r: Rect) -> Rect {
        let mut cr = self.scrollbar_area(wid, r);
        if self.opt_window(Some(wid), "pane-scrollbars") == "on" {
            if let Some(sb) = self.scrollbar(wid, id, cr) {
                let width = sb.width.saturating_add(sb.pad).min(cr.w.saturating_sub(1));
                cr.w -= width;
                if self.opt_window(Some(wid), "pane-scrollbars-position") == "left" { cr.x += width; }
            }
        }
        cr
    }

    pub fn apply_layout(&mut self, wid: WinId) {
        if !self.windows.contains_key(&wid) {
            return;
        }
        let zoomed = self.zoomed_pane(wid);
        for (id, g) in self.windows[&wid].layout.floats() {
            if Some(id) != zoomed {
                let g = self.float_content(wid, id, g);
                if let Some(p) = self.panes.get_mut(&id) {
                    p.resize(g.w, g.h);
                }
            }
        }
        let (rects, _) = self.rects(wid);
        for (id, r) in rects {
            let r = self.content_rect(wid, r);
            if let Some(p) = self.panes.get_mut(&id) {
                p.resize(r.w, r.h);
            }
        }
    }

    /// The attached clients that size windows (ignore_client_size): not suspended,
    /// ignore-size ones only while no other client is attached, and control clients only
    /// once they set a size (refresh-client -C). Each with its session and the size it
    /// gives a window: its terminal less its session's status lines (status_line_size;
    /// control clients have none).
    fn sizing_clients(&self) -> Vec<(ClientId, SessId, (u16, u16))> {
        let attached = |c: &&Client| c.attached && !c.suspended && c.session.is_some_and(|s| self.sessions.contains_key(&s));
        let unflagged = self.clients.values().filter(attached).any(|c| !c.ctl.ignore_size);
        self.clients
            .values()
            .filter(attached)
            .filter(|c| !(unflagged && c.ctl.ignore_size))
            .filter_map(|c| Some((c.id, c.session?, self.client_window_size(c.id)?)))
            .collect()
    }

    /// The size a client gives a window (clients_calculate_size): its terminal less its
    /// session's status lines, or a control client's refresh-client -C size.
    fn client_window_size(&self, cid: ClientId) -> Option<(u16, u16)> {
        let c = self.clients.get(&cid)?;
        if c.control > 0 {
            return c.control_size;
        }
        let status = self.status_lines(c.session);
        // CLIENT_STATUSOFF: a terminal no taller than the status lines has none
        Some((c.size.0, if c.size.1 <= status { c.size.1 } else { c.size.1 - status }))
    }

    /// recalculate_size: the size window-size picks for a window from the clients that
    /// show it (clients_calculate_size). Only clients whose session has the window count,
    /// or with aggressive-resize only those whose current window it is; latest takes the
    /// window's latest client when more than one client has the window; manual is the
    /// window's manual size. None with no client.
    fn window_client_size(&self, wid: WinId, clients: &[(ClientId, SessId, (u16, u16))]) -> Option<(u16, u16)> {
        let mode = self.opt_window(Some(wid), "window-size");
        if mode == "manual" {
            return self.windows.get(&wid).map(|w| (w.manual_sx, w.manual_sy));
        }
        let current = self.opt_window_on(Some(wid), "aggressive-resize");
        let has = |sid: &SessId| self.sessions.get(sid).is_some_and(|s| s.links.values().any(|w| *w == wid));
        // clients_with_window: only whether there is more than one matters
        let shared = clients.iter().filter(|(_, sid, _)| has(sid)).count() > 1;
        let latest = self.ev.latest.get(&wid).copied();
        let sizes = clients.iter().filter(|(cid, sid, _)| {
            let shown = if current { self.session_window(*sid).map(|(_, w)| w) == Some(wid) } else { has(sid) };
            shown && (mode != "latest" || !shared || latest == Some(*cid))
        }).map(|(_, _, size)| *size);
        match mode.as_str() {
            "largest" => sizes.reduce(|a, b| (a.0.max(b.0), a.1.max(b.1))),
            _ => sizes.reduce(|a, b| (a.0.min(b.0), a.1.min(b.1))),
        }
    }

    /// default_window_size: the size of a new window for session `sid` made by client
    /// `cid` (`wid` None), or of window `wid` for resize-window -A/-a (`kind` largest or
    /// smallest). The global window-size decides by default: latest takes the client,
    /// manual the session's default-size; otherwise the largest or smallest of the
    /// clients with the session (or the window), and default-size without any.
    pub fn default_window_size(&self, cid: Option<ClientId>, sid: SessId, wid: Option<WinId>, kind: Option<&str>) -> (u16, u16) {
        let kind = kind.map_or_else(|| self.opt_window(None, "window-size"), String::from);
        let clients = self.sizing_clients();
        let size = if let Some(c) = cid.filter(|_| kind == "latest").and_then(|c| clients.iter().find(|x| x.0 == c)) {
            Some(c.2)
        } else if kind == "manual" {
            None
        } else {
            // the given client always counts, unless it is a control client
            let given = cid.filter(|c| !self.is_control(*c)).and_then(|c| self.client_window_size(c).map(|size| (c, size)));
            let has = |s: &SessId| match wid {
                Some(w) => self.sessions.get(s).is_some_and(|s| s.links.values().any(|x| *x == w)),
                None => *s == sid,
            };
            let sizes = clients.iter().filter(|(c, s, _)| Some(*c) != given.map(|g| g.0) && has(s)).map(|x| x.2).chain(given.map(|g| g.1));
            if kind == "largest" { sizes.reduce(|a, b| (a.0.max(b.0), a.1.max(b.1))) } else { sizes.reduce(|a, b| (a.0.min(b.0), a.1.min(b.1))) }
        };
        let (sx, sy) = size.unwrap_or_else(|| {
            let v = self.opt_session(Some(sid), "default-size");
            v.split_once('x').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))).unwrap_or((80, 24))
        });
        (sx.clamp(1, MAX_SIZE), sy.clamp(1, MAX_SIZE))
    }

    /// Window sizes follow the attached clients (recalculate_sizes): every window is
    /// sized for itself from the clients that show it. The session's own size is picked
    /// from its clients.
    pub fn update_session_size(&mut self, sid: SessId) {
        self.recalculate_session_sizes(sid, false);
    }

    /// recalculate_sizes_now(1) (refresh-client -C): windows no attached session shows
    /// are resized at once too.
    pub fn update_session_size_now(&mut self, sid: SessId) {
        self.recalculate_session_sizes(sid, true);
    }

    fn recalculate_session_sizes(&mut self, sid: SessId, now: bool) {
        let clients = self.sizing_clients();
        let wids: Vec<WinId> = self.windows.keys().copied().collect();
        for wid in wids {
            self.recalculate_window_size(wid, &clients, now);
        }
        // the server loop applies waiting sizes after every command
        self.check_window_resizes();
        let Some(s) = self.sessions.get(&sid) else { return };
        let mode = self.opt_window(Some(s.active_window()), "window-size");
        // default_window_size without a window: the session's clients
        let status = self.status_lines(Some(sid));
        let own: Vec<(u64, (u16, u16))> = clients.iter().filter(|c| c.1 == sid).filter_map(|c| self.clients.get(&c.0).map(|cl| (cl.activity_seq, c.2))).collect();
        let size = match mode.as_str() {
            "manual" => None,
            "largest" => own.iter().map(|c| c.1).reduce(|a, b| (a.0.max(b.0), a.1.max(b.1))),
            "smallest" => own.iter().map(|c| c.1).reduce(|a, b| (a.0.min(b.0), a.1.min(b.1))),
            _ => own.iter().max_by_key(|c| c.0).map(|c| c.1),
        };
        if let (Some((c, r)), Some(s)) = (size, self.sessions.get_mut(&sid)) {
            s.cols = c;
            s.rows = r.saturating_add(status);
        }
        self.mark_session_dirty(sid);
    }

    /// recalculate_size: a new size is applied at once with `now` or for a manual size,
    /// and to a window that is the current window of an attached session; any other
    /// window waits for it (WINDOW_RESIZE) until it is shown (check_window_resizes).
    /// Without a suitable client the window keeps its size and any size it waits for.
    fn recalculate_window_size(&mut self, wid: WinId, clients: &[(ClientId, SessId, (u16, u16))], now: bool) {
        let Some(size) = self.window_client_size(wid, clients) else { return };
        let size = (size.0.clamp(1, MAX_SIZE), size.1.clamp(1, MAX_SIZE));
        let manual = self.opt_window(Some(wid), "window-size") == "manual";
        let shown = self.window_shown(wid);
        let Some(w) = self.windows.get_mut(&wid) else { return };
        // unchanged: the size it waits for, else its own
        if !now && w.new_size.unwrap_or((w.sx, w.sy)) == size {
            return;
        }
        if now || manual || shown {
            self.resize_window(wid, size);
        } else {
            w.new_size = Some(size);
        }
    }

    /// The window is the current window of a session with an attached client.
    fn window_shown(&self, wid: WinId) -> bool {
        self.clients.values().filter(|c| c.attached).filter_map(|c| c.session).any(|s| self.session_window(s).map(|(_, w)| w) == Some(wid))
    }

    /// server_client_check_window_resize: a waiting size is applied once the window is
    /// the current window of an attached session.
    pub fn check_window_resizes(&mut self) {
        let waiting: Vec<(WinId, (u16, u16))> = self.windows.iter().filter_map(|(id, w)| w.new_size.map(|s| (*id, s))).collect();
        for (wid, size) in waiting {
            if self.window_shown(wid) {
                self.resize_window(wid, size);
            }
        }
    }

    /// resize_window: lay a window out at a new size; it can be no smaller than the
    /// layout, and any size it waited for is dropped.
    pub fn resize_window(&mut self, wid: WinId, (cols, rows): (u16, u16)) {
        let Some(w) = self.windows.get_mut(&wid) else { return };
        w.new_size = None;
        let (mw, mh) = w.layout.min_size();
        let (sx, sy) = (cols.max(1).max(mw), rows.max(1).max(mh));
        if (w.sx, w.sy) != (sx, sy) {
            // layout_resize (the zoomed layout is the one pane)
            if w.sx > 0 && w.sy > 0 && !w.zoomed {
                let (old, new) = (Rect { x: 0, y: 0, w: w.sx, h: w.sy }, Rect { x: 0, y: 0, w: sx, h: sy });
                w.layout.tmux_resize(old, new, 1);
            }
            let w = self.windows.get_mut(&wid).unwrap();
            w.sx = sx;
            w.sy = sy;
            self.clamp_floats(wid);
            self.apply_layout(wid);
            self.mark_window_dirty(wid);
        }
    }

    /// server_client_attached_lost: a window whose latest client went away takes the most
    /// active other client whose current window it is.
    fn latest_lost(&mut self, cid: ClientId) {
        let wids: Vec<WinId> = self.ev.latest.iter().filter(|(_, c)| **c == cid).map(|(w, _)| *w).collect();
        for wid in wids {
            let found = self.clients.values()
                .filter(|c| c.id != cid && c.attached && c.session.and_then(|s| self.session_window(s)).map(|(_, w)| w) == Some(wid))
                .max_by_key(|c| c.activity_seq).map(|c| c.id);
            if let Some(found) = found {
                self.update_latest(found);
            }
        }
    }

    // ---- hooks & queue ---------------------------------------------------------------------

    /// After a command: the after-<command> hook, inserted after it (cmdq_insert_hook).
    fn after_command(&mut self, argv: &[String], ctx: &Ctx) {
        self.sync_groups();
        if ctx.hook_depth == 0 && !ctx.skip_after {
            if let Some(full) = argv.first().and_then(|n| cmds::canonical(n)) {
                let mut argv = argv.to_vec();
                argv[0] = full.to_string();
                let hook = match full {
                    "next-window" | "previous-window" | "last-window" => "select-window",
                    // new-pane runs split-window's hook (tmux cmd_split_window_exec)
                    "new-pane" => "split-window",
                    _ => full,
                };
                self.command_hook(&format!("after-{hook}"), &argv, ctx);
            }
        }
    }

    /// Runs a command queue, then the hooks its commands' events queued.
    pub fn run_queue(&mut self, q: Queue) {
        if q.ctx.control_seq.is_some() {
            if let Some(c) = q.ctx.client.and_then(|c| self.clients.get_mut(&c)) {
                c.ctl.command_waiting = true;
            }
        }
        self.queue_depth += 1;
        self.run_queue_inner(q);
        self.drain_hooks();
        self.queue_depth -= 1;
        if self.queue_depth == 0 {
            self.drain_clients();
        }
    }

    /// The attached client whose queue (c->queue) a queue is: none for the global queue
    /// (the configuration, event hooks) and background jobs.
    pub(crate) fn queue_owner(&self, ctx: &Ctx) -> Option<ClientId> {
        if ctx.cfg || ctx.detached || ctx.hook_depth > 1 {
            return None;
        }
        self.queue_client(ctx)
    }

    /// A command of the client's queue waits (CMDQ_WAITING): a prompt, a popup, a job,
    /// a delay, wait-for, a pane.
    pub(crate) fn client_blocked(&self, cid: ClientId) -> bool {
        let own = |q: &Queue| self.queue_owner(&q.ctx) == Some(cid);
        self.prompt_waits.iter().any(|w| own(&w.3))
            || self.overlay_waits.iter().any(|w| own(&w.3))
            || self.jobs.values().any(|j| own(&j.queue))
            || self.delayed.iter().any(|d| own(&d.1))
            || self.waits.values().any(|c| c.waiters.iter().chain(&c.lockers).any(own))
            || self.ev.waiters.iter().any(|w| own(&w.queue))
            || self.pane_waits.values().flatten().any(|w| own(&w.1))
    }

    /// cmdq_append for an attached client: runs at once unless the client's queue has
    /// items or waits, a queue is running or its input is being read.
    pub(crate) fn client_append(&mut self, cid: ClientId, item: ClientItem) {
        self.client_items.entry(cid).or_default().push_back(item);
        self.drain_clients();
    }

    /// Whether a key for the client would wait behind items of its queue.
    pub(crate) fn client_busy(&self, cid: ClientId) -> bool {
        self.queue_depth > 0 || self.client_items.get(&cid).is_some_and(|q| !q.is_empty()) || self.client_blocked(cid)
    }

    /// server_loop's cmdq_next for each client: every client's items in order, each
    /// until one of its commands waits. (Not while a queue runs or input is read: the
    /// outermost caller does it, so the items are not handled recursively.)
    pub(crate) fn drain_clients(&mut self) {
        if self.queue_depth > 0 || self.reading.is_some() || self.draining {
            return;
        }
        self.draining = true;
        loop {
            self.resume_waits();
            let mut ran = false;
            let cids: Vec<ClientId> = self.client_items.keys().copied().collect();
            for cid in cids {
                // (the items of a client that has gone go with it)
                if !self.clients.contains_key(&cid) {
                    self.client_items.remove(&cid);
                    continue;
                }
                if self.client_blocked(cid) {
                    continue;
                }
                let Some(item) = self.client_items.get_mut(&cid).and_then(|q| q.pop_front()) else { continue };
                if self.client_items.get(&cid).is_some_and(|q| q.is_empty()) {
                    self.client_items.remove(&cid);
                }
                ran = true;
                match item {
                    ClientItem::Key(k, raw) => {
                        let name = keys::key_name(&k);
                        self.handle_key_queued(cid, Some(k), &name, raw, false);
                    }
                    ClientItem::Mouse(key, ctx) => self.mouse_key_now(cid, key, ctx),
                    ClientItem::Paste(data) => self.paste_now(cid, data),
                    ClientItem::Raw(raw) => self.raw_input_now(cid, raw),
                    ClientItem::Error(e) => self.set_error(cid, &e),
                    ClientItem::Queue(q) => self.run_queue(q),
                }
            }
            if !ran {
                break;
            }
        }
        self.draining = false;
    }

    /// The queues whose prompt or popup has closed go on (cmdq_continue), at once.
    pub(crate) fn resume_waits(&mut self) {
        if self.resuming || self.queue_depth > 0 {
            return;
        }
        self.resuming = true;
        let waiting = std::mem::take(&mut self.prompt_waits);
        for (cid, epoch, argv, queue) in waiting {
            if !self.prompt_open(epoch) {
                let mut queue = queue;
                self.after_command(&argv, &queue.ctx);
                for c in std::mem::take(&mut self.ev.inject).into_iter().rev() {
                    queue.cmds.push_front(c);
                }
                // the answer's commands come straight after the prompt, before its hook
                for c in self.prompt_inserts.remove(&epoch).unwrap_or_default().into_iter().rev() {
                    queue.cmds.push_front(c);
                }
                self.run_queue(queue);
            } else {
                self.prompt_waits.push((cid, epoch, argv, queue));
            }
        }
        let waiting = std::mem::take(&mut self.overlay_waits);
        for (cid, epoch, argv, queue) in waiting {
            if self.clients.get(&cid).is_none_or(|c| c.overlay.is_none() || c.overlay_epoch != epoch) {
                let mut queue = queue;
                // popup_free_cb: a command-line client exits with the popup's status, 129
                // (128 + SIGHUP) when it was closed before its command exited
                if let Some(pane) = self.popup_panes.remove(&epoch) {
                    let status = self.popup_status.remove(&pane).unwrap_or(129);
                    if queue.ctx.reply.and_then(|r| self.clients.get(&r)).is_some_and(|c| c.session.is_none()) {
                        queue.ctx.retval = status;
                    }
                }
                self.after_command(&argv, &queue.ctx);
                for c in std::mem::take(&mut self.ev.inject).into_iter().rev() {
                    queue.cmds.push_front(c);
                }
                self.run_queue(queue);
            } else {
                self.overlay_waits.push((cid, epoch, argv, queue));
            }
        }
        self.resuming = false;
    }

    /// Whether a waiting prompt or popup has closed (its queue goes on at once).
    fn waits_resumable(&self) -> bool {
        self.prompt_waits.iter().any(|w| !self.prompt_open(w.1))
            || self.overlay_waits.iter().any(|(cid, epoch, ..)| self.clients.get(cid).is_none_or(|c| c.overlay.is_none() || c.overlay_epoch != *epoch))
    }

    /// A control client's reply for the item just run (cmdq_guard): each command,
    /// including those of hooks, gets its own %begin/%end (or %error) block, and the
    /// notifications it caused are written before the next one.
    fn control_item_done(&mut self, q: &mut Queue, err: Option<&str>) {
        // (a key group's commands still answer the control client whose queue it is)
        let (Some(seq), Some(cid)) = (q.ctx.control_seq, self.queue_client(&q.ctx)) else { return };
        q.ctx.control_open = false;
        let out = std::mem::take(&mut q.ctx.out);
        // hook and key binding commands run with a new state, without CMDQ_STATE_CONTROL
        let typed = q.ctx.control_line && q.ctx.hook_depth == 0 && q.ctx.key_depth == 0;
        self.control_block(cid, seq, typed, &out, err);
        // cmdq_error for a control client sets its exit status (c->retval)
        if let Some(c) = self.clients.get(&cid).filter(|_| err.is_some_and(|e| !e.is_empty())) {
            c.ctl.retval.set(1);
        }
        self.control_flush(cid);
        self.control_after_block(cid);
        q.ctx.control_done = true;
        if let Some(c) = self.clients.get_mut(&cid) {
            c.control_seq += 1;
            q.ctx.control_seq = Some(c.control_seq);
        }
    }

    /// Drops the rest of the hook command list an error happened in (cmdq_remove_group
    /// for the hook's item): up to and including its closing mark.
    fn skip_hook_list(q: &mut Queue, saved: &mut events::HookSaved) {
        let mut depth = 0usize;
        while let Some(cmd) = q.cmds.pop_front() {
            match events::hook_mark_kind(&cmd) {
                Some(true) => depth += 1,
                Some(false) if depth == 0 => {
                    events::apply_hook_mark(&cmd, &mut q.ctx, saved, &mut q.soft);
                    return;
                }
                Some(false) => depth -= 1,
                None => q.soft = q.soft.saturating_sub(1),
            }
        }
    }

    /// The group the next command is in, from the first group end without a start before
    /// it: Some(true) for a key binding's (KEY_GROUP), Some(false) for a hook's.
    fn innermost_group(q: &Queue) -> Option<bool> {
        let mut depth = 0usize;
        for c in &q.cmds {
            let open = if let Some(open) = events::hook_mark_kind(c) {
                (open, false)
            } else if c.first() == Some(&*KEY_GROUP) {
                (c.get(1).map(String::as_str) == Some("1"), true)
            } else {
                continue;
            };
            match open {
                (true, _) => depth += 1,
                (false, key) if depth == 0 => return Some(key),
                (false, _) => depth -= 1,
            }
        }
        None
    }

    /// cmdq_remove_group for a key binding's command: the rest of the binding, up to and
    /// including its end mark.
    fn skip_key_group(q: &mut Queue) {
        let mut depth = 0usize;
        while let Some(c) = q.cmds.pop_front() {
            let mark = events::hook_mark_kind(&c).or_else(|| (c.first() == Some(&*KEY_GROUP)).then(|| c.get(1).map(String::as_str) == Some("1")));
            match mark {
                Some(true) => depth += 1,
                Some(false) if depth == 0 => {
                    Self::close_key_group(&mut q.ctx);
                    return;
                }
                Some(false) => depth -= 1,
                None => q.soft = q.soft.saturating_sub(1),
            }
        }
    }

    /// The client whose queue this is (cmdq item->client): the command-line client, the
    /// attached client a binding runs for, or the one a key group's queue belongs to.
    pub(crate) fn queue_client(&self, ctx: &Ctx) -> Option<ClientId> {
        ctx.log_client.or(ctx.reply).or(ctx.client)
    }

    /// cmdq_error for a command that goes on (or whose group alone ends): shown at once,
    /// on a command-line client's stderr (exit status 1) or an attached client's status
    /// line. False when neither applies and the caller reports it.
    pub(crate) fn report_error_now(&mut self, ctx: &mut Ctx, e: &str) -> bool {
        if ctx.control_seq.is_some() {
            return false;
        }
        let Some(owner) = self.queue_client(ctx) else { return false };
        let Some(c) = self.clients.get(&owner) else { return false };
        if c.control != 0 {
            return false;
        }
        if !c.attached || c.session.is_none() {
            self.log_error(Some(owner), e);
            ctx.soft_err.push_str(e);
            ctx.soft_err.push('\n');
        } else {
            self.set_error(owner, e);
        }
        true
    }

    /// A KEY_GROUP start: its commands run for the key's client, pane and key.
    fn open_key_group(&self, ctx: &mut Ctx, mark: &[String]) {
        ctx.key_depth += 1;
        ctx.key_saved.push((ctx.client, ctx.pane, ctx.key.clone(), ctx.log_client));
        // the queue's client (item->client) is the one the message log names
        ctx.log_client = ctx.log_client.or(ctx.client);
        // (no client: send-keys without -K, whose bindings keep the queue's client)
        if mark.get(2).is_some_and(|c| c.is_empty()) {
            ctx.pane = mark.get(3).and_then(|p| p.parse().ok()).or(ctx.pane);
        } else if let Some(c) = mark.get(2).and_then(|c| c.parse().ok()).filter(|c| self.clients.contains_key(c)) {
            ctx.client = Some(c);
            ctx.pane = mark.get(3).and_then(|p| p.parse().ok());
        }
        ctx.key = mark.get(4).filter(|k| !k.is_empty()).cloned();
    }

    /// A KEY_GROUP end: back to what its start replaced.
    fn close_key_group(ctx: &mut Ctx) {
        ctx.key_depth = ctx.key_depth.saturating_sub(1);
        if let Some((client, pane, key, log)) = ctx.key_saved.pop() {
            ctx.client = client;
            ctx.pane = pane;
            ctx.key = key;
            ctx.log_client = log;
        }
    }

    /// When a command list fails, the items that are not of its group stay: the startup
    /// configuration's end, send-keys -K keys and the key bindings' command groups.
    fn keep_other_groups(q: &mut Queue) {
        let mut depth = 0usize;
        q.cmds.retain(|c| {
            if c.first() == Some(&*KEY_GROUP) {
                if c.get(1).map(String::as_str) == Some("1") {
                    depth += 1;
                } else {
                    depth = depth.saturating_sub(1);
                }
                return true;
            }
            depth > 0 || c.first().is_some_and(|n| *n == *CFG_DONE || *n == *KEY_EVENT)
        });
    }

    /// Drops the commands left of the line (group) of a file whose command failed:
    /// those up to the next line's mark.
    fn skip_group(q: &mut Queue) {
        let Some(&line) = q.ctx.lines.last() else { return };
        if line == 0 {
            return;
        }
        loop {
            match q.cmds.front() {
                Some(c) if c.first() == Some(&*LINE_MARK) && c.get(1).and_then(|n| n.parse().ok()) == Some(line) => {}
                Some(c) if c.first().is_some_and(|n| n.starts_with('\0') && !n.starts_with("\0alias:")) => return,
                Some(_) => {}
                None => return,
            }
            q.cmds.pop_front();
            q.soft = q.soft.saturating_sub(1);
        }
    }

    fn run_queue_inner(&mut self, mut q: Queue) {
        let mut hook_saved = std::mem::take(&mut q.ctx.hook_saved);
        // an error whose command-error hook runs first
        let mut failed = q.ctx.command_error.take();
        // the first error of a hook's commands: it ends only that hook's list
        let mut hook_failed = q.ctx.hook_error.take();
        if q.ctx.control_open {
            // the command the queue waited in is done
            self.control_item_done(&mut q, None);
        }
        while let Some(cmd) = q.cmds.pop_front() {
            let name = cmd.first().map(|s| s.strip_prefix("\0alias:").unwrap_or(s).to_string()).unwrap_or_default();
            if events::hook_mark_kind(&cmd).is_some() {
                events::apply_hook_mark(&cmd, &mut q.ctx, &mut hook_saved, &mut q.soft);
                continue;
            }
            if name == *KEY_GROUP {
                if cmd.get(1).map(String::as_str) == Some("1") {
                    self.open_key_group(&mut q.ctx, &cmd);
                } else {
                    Self::close_key_group(&mut q.ctx);
                }
                continue;
            }
            // errors never end the global queue the configuration runs in
            let soft = q.soft > 0 || q.ctx.cfg;
            q.soft = q.soft.saturating_sub(1);
            if name == *FILE_PUSH {
                q.ctx.files.push(cmd.get(1).cloned().unwrap_or_default());
                q.ctx.lines.push(0);
                q.ctx.sourced.push(cmd.get(2).is_some());
                continue;
            }
            if name == *FILE_POP {
                q.ctx.files.pop();
                q.ctx.lines.pop();
                q.ctx.sourced.pop();
                continue;
            }
            if name == *LINE_MARK {
                if let Some(l) = q.ctx.lines.last_mut() {
                    *l = cmd.get(1).and_then(|n| n.parse().ok()).unwrap_or(0);
                }
                continue;
            }
            if name == *CFG_DONE {
                self.cfg_done();
                continue;
            }
            if name == *KEY_EVENT {
                // server_client_key_callback: the bound commands go after this item. The
                // queue is its client's, so once that client has gone its keys go too.
                let ev = cmd.get(1).and_then(|t| t.parse().ok()).and_then(|t| self.key_events.remove(&t));
                let gone = q.ctx.reply.or(q.ctx.client).is_some_and(|c| !self.clients.contains_key(&c));
                let bound = ev.filter(|_| !gone).map(|ev| self.key_event(ev)).unwrap_or_default();
                // (each binding in its KEY_GROUP marks)
                for c in bound.into_iter().rev() {
                    q.cmds.push_front(c);
                }
                continue;
            }
            // cmdq_fire_command: the items of a client that has gone (CLIENT_DEAD) fail
            if q.ctx.reply.is_some_and(|r| !self.clients.contains_key(&r)) {
                continue;
            }
            // a key group's client that has gone or detached: its commands find their
            // client from the queue's (cmd_find_client), as with no key
            if q.ctx.key_depth > 0 && q.ctx.client.is_some_and(|c| !self.clients.get(&c).is_some_and(|c| c.attached)) {
                q.ctx.client = q.ctx.log_client;
            }
            let delay_cmd = (cmds::canonical(&name) == Some("run-shell")).then(|| cmd.clone());
            // the command's arguments for its hooks: an alias's command under its own name
            let mut argv = cmd.clone();
            if let Some(first) = argv.first_mut() {
                *first = name.clone();
            }
            crate::cmdparse::unmark_blocks(&mut argv);
            let saved_mouse = std::mem::replace(&mut self.cur_mouse, q.ctx.mouse.clone());
            let saved_file = std::mem::replace(&mut self.cur_file, q.ctx.files.last().cloned());
            let saved_formats = std::mem::replace(&mut self.ev.cur_formats, q.ctx.formats.clone());
            let saved_hook = std::mem::replace(&mut self.ev.in_hook, q.ctx.hook_depth > 1);
            let result = self.exec(cmd, &mut q.ctx);
            // events for what the command changed, as its own (groups synchronized)
            self.sync_groups();
            let readonly = self.readonly_context.replace(q.ctx.readonly);
            self.detect_changes(cmds::canonical(&name));
            self.readonly_context.set(readonly);
            if let Err(e) = &result && !soft && (e == QUIET_ERROR || !e.starts_with('\0')) && Self::innermost_group(&q) == Some(true) {
                // a command of a send-keys -K key's binding: cmdq_error reports it, then
                // command-error runs and the rest of that binding goes (cmdq_remove_group);
                // the queue goes on after it
                self.cur_mouse = saved_mouse;
                self.cur_file = saved_file;
                self.ev.cur_formats = saved_formats;
                self.ev.in_hook = saved_hook;
                let quiet = e == QUIET_ERROR;
                let cli = q.ctx.control_seq.is_none() && q.ctx.reply.and_then(|r| self.clients.get(&r)).is_some_and(|c| !c.attached);
                if q.ctx.control_seq.is_some() {
                    self.control_item_done(&mut q, Some(if quiet { "" } else { e }));
                } else if quiet {
                } else if cli {
                    self.log_error(q.ctx.reply, e);
                    q.ctx.soft_err.push_str(e);
                    q.ctx.soft_err.push('\n');
                } else if !self.report_error_now(&mut q.ctx, e) && hook_failed.is_none() {
                    // (an attached client's queue: its status line at once, which a later
                    // message replaces)
                    hook_failed = Some(e.clone());
                }
                self.command_hook("command-error", &argv, &q.ctx);
                let hooks = std::mem::take(&mut self.ev.inject);
                Self::skip_key_group(&mut q);
                for c in hooks.into_iter().rev() {
                    q.cmds.push_front(c);
                }
                continue;
            }
            if matches!(&result, Err(e) if e == QUIET_ERROR) {
                // a command that failed without cmdq_error (unbind-key -q): command-error
                // still runs and the rest of its group goes, but the exit status stays 0
                self.cur_mouse = saved_mouse;
                self.cur_file = saved_file;
                self.ev.cur_formats = saved_formats;
                self.ev.in_hook = saved_hook;
                if !soft && !hook_saved.is_empty() {
                    self.control_item_done(&mut q, Some(""));
                    Self::skip_hook_list(&mut q, &mut hook_saved);
                    continue;
                }
                self.command_hook("command-error", &argv, &q.ctx);
                let hooks = std::mem::take(&mut self.ev.inject);
                if soft {
                    Self::skip_group(&mut q);
                } else {
                    Self::keep_other_groups(&mut q);
                    q.soft = 0;
                }
                self.control_item_done(&mut q, Some(""));
                for c in hooks.into_iter().rev() {
                    q.cmds.push_front(c);
                }
                continue;
            }
            if let Err(e) = &result && !e.starts_with('\0') && !soft && !hook_saved.is_empty() {
                // a hook command: the error ends that hook's list, the rest goes on
                self.cur_mouse = saved_mouse;
                self.cur_file = saved_file;
                self.ev.cur_formats = saved_formats;
                self.ev.in_hook = saved_hook;
                if q.ctx.control_seq.is_some() {
                    self.control_item_done(&mut q, Some(e));
                } else if !self.report_error_now(&mut q.ctx, e) && hook_failed.is_none() {
                    hook_failed = Some(e.clone());
                }
                Self::skip_hook_list(&mut q, &mut hook_saved);
                continue;
            }
            if let Err(e) = &result && !e.starts_with('\0') {
                self.command_hook("command-error", &argv, &q.ctx);
                let hooks = std::mem::take(&mut self.ev.inject);
                if q.ctx.control_seq.is_some() && !soft {
                    // %error first, then the hook as items of their own; the rest of
                    // the command list is removed
                    self.cur_mouse = saved_mouse;
                    self.cur_file = saved_file;
                    self.ev.cur_formats = saved_formats;
                    self.ev.in_hook = saved_hook;
                    self.control_item_done(&mut q, Some(e));
                    // (keys send-keys -K left are items of their own too)
                    Self::keep_other_groups(&mut q);
                    q.cmds.retain(|c| c.first() != Some(&*CFG_DONE));
                    q.soft = 0;
                    for c in hooks.into_iter().rev() {
                        q.cmds.push_front(c);
                    }
                    continue;
                }
                if !hooks.is_empty() && failed.is_none() {
                    // the hook runs in place of the rest of the command list
                    if !soft {
                        // (the configuration's cfg_done stays)
                        Self::keep_other_groups(&mut q);
                        failed = Some(e.clone());
                    }
                    for c in hooks.into_iter().rev() {
                        q.cmds.push_front(c);
                    }
                    if !soft {
                        self.cur_mouse = saved_mouse;
                        self.cur_file = saved_file;
                        self.ev.cur_formats = saved_formats;
                        self.ev.in_hook = saved_hook;
                        continue;
                    }
                }
            }
            self.cur_mouse = saved_mouse;
            self.cur_file = saved_file;
            self.ev.cur_formats = saved_formats;
            self.ev.in_hook = saved_hook;
            match result {
                Ok(Flow::Inject(cmds, soft_errors)) => {
                    let n = cmds.len();
                    for c in cmds.into_iter().rev() {
                        q.cmds.push_front(c);
                    }
                    q.soft = if soft_errors { q.soft + n } else { q.soft };
                    // the after hook is inserted straight after the command, before these
                    self.after_command(&argv, &q.ctx);
                    for c in std::mem::take(&mut self.ev.inject).into_iter().rev() {
                        q.cmds.push_front(c);
                    }
                    self.control_item_done(&mut q, None);
                }
                Err(e) if soft && !e.starts_with('\0') => {
                    log(&format!("{e}"));
                    // cmdq_remove_group: the rest of the failed command's line is dropped
                    Self::skip_group(&mut q);
                    let cli = q.ctx.control_seq.is_none() && q.ctx.reply.and_then(|r| self.clients.get(&r)).is_some_and(|c| !c.attached);
                    let owner = self.queue_client(&q.ctx).filter(|_| q.ctx.control_seq.is_none() && !q.ctx.cfg);
                    if !cli && owner.is_some_and(|c| self.clients.get(&c).is_some_and(|c| c.attached && c.session.is_some() && c.control == 0)) {
                        // cmdq_error for an attached client: its status line
                        self.set_error(owner.unwrap(), &e);
                    } else {
                        if q.ctx.client.or(q.ctx.reply).is_some() {
                            self.log_error(q.ctx.log_client.or(q.ctx.reply).or(q.ctx.client), &e);
                        } else {
                            self.no_client_error(&q.ctx, &e);
                        }
                        if cli {
                            q.ctx.soft_err.push_str(&e);
                            q.ctx.soft_err.push('\n');
                        } else {
                            q.ctx.out.push_str(&e);
                            q.ctx.out.push('\n');
                        }
                    }
                    self.control_item_done(&mut q, None);
                }
                Ok(Flow::WaitOverlay(cid, epoch)) => {
                    q.ctx.control_open = q.ctx.control_seq.is_some();
                    q.ctx.hook_saved = hook_saved;
                    q.ctx.command_error = failed;
                    q.ctx.hook_error = hook_failed;
                    self.overlay_waits.push((cid, epoch, argv, q));
                    return;
                }
                Ok(Flow::WaitPrompt(cid, epoch)) => {
                    q.ctx.control_open = q.ctx.control_seq.is_some();
                    q.ctx.hook_saved = hook_saved;
                    q.ctx.command_error = failed;
                    q.ctx.hook_error = hook_failed;
                    self.prompt_waits.push((cid, epoch, argv, q));
                    return;
                }
                Ok(Flow::Next) => {
                    let saved_hook = std::mem::replace(&mut self.ev.in_hook, q.ctx.hook_depth > 1);
                    self.after_command(&argv, &q.ctx);
                    self.ev.in_hook = saved_hook;
                    for c in std::mem::take(&mut self.ev.inject).into_iter().rev() {
                        q.cmds.push_front(c);
                    }
                    self.control_item_done(&mut q, None);
                }
                Err(e) if e.starts_with('\0') => {
                    // async commands: park the rest of the queue
                    let parts: Vec<String> = e.split('\0').map(String::from).collect();
                    let get = |i: usize| parts.get(i).cloned().unwrap_or_default();
                    match get(1).as_str() {
                        "job-if" => {
                            let kind = JobKind::IfShell { then: get(3), otherwise: Some(get(4)).filter(|s| !s.is_empty()) };
                            if get(5) == "b" {
                                let sub = Queue { cmds: VecDeque::new(), ctx: Ctx { reply: None, out: Default::default(), control_seq: None, detached: true, ..q.ctx.clone() }, soft: 0 };
                                self.spawn_job(&get(2), None, kind, sub);
                                self.control_item_done(&mut q, None);
                                continue;
                            }
                            // CMD_RETURN_WAIT: the reply block ends now (cmdq_fire_command)
                            self.control_item_done(&mut q, None);
                            q.ctx.hook_saved = hook_saved;
                            q.ctx.command_error = failed;
                            q.ctx.hook_error = hook_failed;
                            self.spawn_job(&get(2), None, kind, q);
                            return;
                        }
                        "job-run" => {
                            let background = get(4).contains('b');
                            let pane = get(5).parse().ok();
                            let cwd = Some(get(3)).filter(|s| !s.is_empty());
                            let kind = JobKind::RunShell { pane, background, stderr: get(4).contains('E') };
                            if background {
                                let sub = Queue { cmds: VecDeque::new(), ctx: Ctx { reply: None, out: Default::default(), control_seq: None, detached: true, ..q.ctx.clone() }, soft: 0 };
                                self.spawn_job(&get(2), cwd, kind, sub);
                                self.control_item_done(&mut q, None);
                                continue;
                            }
                            // CMD_RETURN_WAIT: the reply block ends now (cmdq_fire_command)
                            self.control_item_done(&mut q, None);
                            q.ctx.hook_saved = hook_saved;
                            q.ctx.command_error = failed;
                            q.ctx.hook_error = hook_failed;
                            self.spawn_job(&get(2), cwd, kind, q);
                            return;
                        }
                        "wait" => {
                            // cmdq_fire_command writes %end as soon as wait-for waits
                            self.control_item_done(&mut q, None);
                            q.ctx.hook_saved = hook_saved;
                            q.ctx.command_error = failed;
                            q.ctx.hook_error = hook_failed;
                            self.waits.entry(get(2)).or_default().waiters.push(q);
                            return;
                        }
                        "lock" => {
                            // cmdq_fire_command writes %end as soon as wait-for waits
                            self.control_item_done(&mut q, None);
                            q.ctx.hook_saved = hook_saved;
                            q.ctx.command_error = failed;
                            q.ctx.hook_error = hook_failed;
                            self.waits.entry(get(2)).or_default().lockers.push(q);
                            return;
                        }
                        "wait-event" => {
                            // cmdq_fire_command writes %end as soon as wait-for waits
                            self.control_item_done(&mut q, None);
                            q.ctx.hook_saved = hook_saved;
                            q.ctx.command_error = failed;
                            q.ctx.hook_error = hook_failed;
                            self.park_event_waiter(get(2), (get(5) == "F").then(|| get(3)), get(4) == "v", q);
                            return;
                        }
                        "pane-wait" => {
                            // new-pane -W: continued when the pane's command exits
                            match get(2).parse::<PaneId>() {
                                Ok(id) if self.panes.contains_key(&id) => {
                                    q.ctx.hook_saved = hook_saved;
                                    q.ctx.command_error = failed;
                                    q.ctx.hook_error = hook_failed;
                                    self.pane_waits.entry(id).or_default().push((argv, q));
                                }
                                _ => self.finish_queue(q, None),
                            }
                            return;
                        }
                        "delay" if get(3) == "b" => {
                            // run-shell -b -d: only the command waits for its timer, in a
                            // queue of no client's (JOB_NOWAIT); the rest goes on now
                            if let Some(c) = delay_cmd.as_ref().map(|c| strip_delay(c)) {
                                let ms: u64 = get(2).parse().unwrap_or(0);
                                // (a command-line client that has gone by then is not the
                                // queue's client: cdata->client is the target client)
                                let client = q.ctx.client.filter(|c| self.clients.get(c).is_some_and(|c| c.attached));
                                let ctx = Ctx { client, reply: None, out: Default::default(), control_seq: None, detached: true, resumed: true, ..q.ctx.clone() };
                                self.delayed.push((Instant::now() + Duration::from_millis(ms), Queue { cmds: VecDeque::from([c]), ctx, soft: 0 }));
                            }
                            self.control_item_done(&mut q, None);
                            continue;
                        }
                        "delay" => {
                            // run-shell -d: the rest of the queue (and the command) runs later
                            q.ctx.hook_saved = hook_saved;
                            q.ctx.command_error = failed;
                            q.ctx.hook_error = hook_failed;
                            let ms: u64 = get(2).parse().unwrap_or(0);
                            if let Some(c) = delay_cmd.as_ref().map(|c| strip_delay(c)) {
                                q.cmds.push_front(c);
                                q.ctx.resumed = true;
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
                    self.finish_queue(q, Some(failed.unwrap_or(e)));
                    return;
                }
            }
        }
        self.finish_queue(q, failed.or(hook_failed));
    }

    pub fn finish_queue(&mut self, q: Queue, mut err: Option<String>) {
        // send-keys -K keys the queue did not reach
        for c in q.cmds.iter().filter(|c| c.first() == Some(&*KEY_EVENT)) {
            if let Some(t) = c.get(1).and_then(|t| t.parse().ok()) {
                self.key_events.remove(&t);
            }
        }
        // the configuration's queue ended early: cfg_done still runs
        let cfg_done = q.cmds.iter().any(|c| c.first() == Some(&*CFG_DONE));
        let ctx = q.ctx;
        if cfg_done {
            if let Some(e) = err.take().filter(|e| !e.starts_with('\0')) {
                self.no_client_error(&ctx, &e);
            }
            self.cfg_done();
            return;
        }
        if ctx.out.overflowed() { err = Some("command output limit exceeded".into()); }
        if let (Some(seq), Some(cid)) = (ctx.control_seq, ctx.client) {
            if let Some(c) = self.clients.get_mut(&cid) {
                c.ctl.command_waiting = false;
            }
            if !ctx.control_done || !ctx.out.is_empty() || err.is_some() {
                self.control_block(cid, seq, ctx.control_line && ctx.hook_depth == 0, &ctx.out, err.as_deref());
                if let Some(c) = self.clients.get(&cid).filter(|_| err.as_deref().is_some_and(|e| !e.is_empty())) {
                    c.ctl.retval.set(1);
                }
            }
            self.control_flush(cid);
            self.control_after_block(cid);
            if let Some(c) = self.clients.get(&cid) {
                c.finish_exit();
            }
            // a one-shot `pwsh-tmux -C command` that didn't attach ends here
            if let Some(c) = ctx.reply.and_then(|r| self.clients.get(&r)).filter(|c| !c.attached) {
                c.send(S_OUTPUT, b"%exit\n");
                c.send(S_REPLY, &encode_reply(if err.is_some() { 1 } else { 0 }, ""));
            }
            return;
        }
        let attached = ctx.client.and_then(|c| self.clients.get(&c)).is_some_and(|c| c.attached);
        if ctx.reply.is_none() && ctx.client.is_some_and(|c| self.is_control(c)) {
            // a control client outside a command block (a hook's best client): bare
            // lines, after the notifications so far (server_client_print, cmdq_error)
            let cid = ctx.client.unwrap();
            self.control_flush(cid);
            let mut text = ctx.out.to_string();
            if let Some(e) = &err {
                self.add_message(&format!("{} message: {e}", self.clients[&cid].name()));
                text.push_str(e);
            }
            if !text.is_empty() {
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                self.control_write(cid, &text);
            }
            return;
        }
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
                    self.set_error(rc, &e);
                }
                return;
            }
            if let Some(e) = &err {
                let msg = format!("{} message: {e}", c.name());
                self.add_message(&msg);
            }
            // the command client is lost next (server_client_lost): check now so the next
            // command sees the result, and if the server goes with it, stop listening
            // before the reply so that command starts a server of its own
            self.check_destroy_unattached();
            // (not while hooks, jobs or delayed commands could still make a session)
            if self.would_exit(Some(rc), false) && self.ev.pending.is_empty() && self.jobs.is_empty() && self.delayed.is_empty() {
                ipc::cleanup();
                // a Windows pipe lasts until the process exits: the client waits (kill-server)
                if cfg!(windows) && let Some(c) = self.clients.get(&rc) {
                    c.send(S_SHUTDOWN, b"");
                }
            }
            let Some(c) = self.clients.get(&rc) else { return };
            // config file errors go to stderr too and make the exit status 1
            let err = match (err, ctx.soft_err.trim_end_matches('\n')) {
                (e, "") => e,
                (Some(e), soft) => Some(format!("{soft}\n{e}")),
                (None, soft) => Some(soft.to_string()),
            };
            match err {
                Some(e) => {
                    // output goes to stdout, the error to stderr
                    if !ctx.out.is_empty() {
                        c.send(S_STDOUT, ctx.out.as_bytes());
                    }
                    c.send(S_REPLY, &encode_reply(1, &e));
                }
                None if ctx.retval != 0 => {
                    c.send(S_STDOUT, ctx.out.as_bytes());
                    c.send(S_REPLY, &encode_reply(ctx.retval as u8, ""));
                }
                None => c.send(S_REPLY, &encode_reply(0, &ctx.out)),
            }
            return;
        }
        if let Some(e) = &err {
            log(&format!("command error: {e}"));
            if ctx.client.is_none() {
                self.no_client_error(&ctx, e);
            } else if !attached {
                self.log_error(None, e);
            }
        }
        if attached {
            let cid = ctx.client.unwrap();
            if let Some(e) = err {
                self.set_error(cid, &e);
            } else if !ctx.out.is_empty() {
                if ctx.readonly || self.client_is_readonly(Some(cid)) {
                    self.set_message(cid, &ctx.out);
                } else if let Some(p) = self.client_active_pane(cid) {
                    self.view_output(p, &ctx.out);
                }
            }
        }
    }

    /// cfg_show_causes: the configuration's causes go to the first client if it is a
    /// control client (%config-error), else into view mode in the active pane of the
    /// session (or the first client's, or the first one) once it is attached.
    pub fn cfg_show_causes(&mut self, sid: Option<SessId>) {
        if self.cfg_causes.is_empty() {
            return;
        }
        // TAILQ_FIRST(&clients): a command client that has been told to exit is gone
        let first = self.clients.iter().find(|(_, c)| c.ident.is_some() && !c.io.exit_sent.load(std::sync::atomic::Ordering::SeqCst)).map(|(id, _)| *id);
        if let Some(c) = first.filter(|c| self.is_control(*c)) {
            for cause in std::mem::take(&mut self.cfg_causes) {
                self.control_notify_write(c, &format!("%config-error {cause}"));
            }
            return;
        }
        let sid = sid
            .or_else(|| first.and_then(|c| self.clients.get(&c)).and_then(|c| c.session))
            .or_else(|| self.sessions.keys().next().copied());
        let Some(sid) = sid.filter(|s| self.clients.values().any(|c| c.attached && c.session == Some(*s))) else { return };
        let Some(pane) = self.sessions.get(&sid).map(|s| self.windows[&s.active_window()].active) else { return };
        let text = std::mem::take(&mut self.cfg_causes).join("\n");
        self.view_add(pane, &text, false);
    }

    /// cmdq_error's message log entry for a client that shows no status line (or none).
    pub fn log_error(&mut self, client: Option<ClientId>, e: &str) {
        match client.and_then(|c| self.clients.get(&c)).map(|c| c.name()) {
            Some(n) => self.add_message(&format!("{n} message: {e}")),
            None => self.add_message(&format!("message: {e}")),
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
        cmd.stdout(std::process::Stdio::piped());
        if matches!(kind, JobKind::RunShell { stderr: true, .. }) {
            cmd.stderr(std::process::Stdio::piped());
        } else {
            cmd.stderr(std::process::Stdio::null());
        }
        // started here so the job's process id is known while it runs
        let child = cmd.spawn();
        let pid = child.as_ref().ok().map(|c| c.id());
        #[cfg(unix)]
        let fd = child.as_ref().ok().and_then(|c| c.stdout.as_ref()).map_or(-1, |o| std::os::fd::AsRawFd::as_raw_fd(o) as i64);
        #[cfg(not(unix))]
        let fd = -1;
        thread::spawn(move || {
            let (status, out) = match child.and_then(|c| c.wait_with_output()) {
                // a signal is passed on as its negated number
                Ok(o) => (o.status.code().or_else(|| process_exit_signal(&o.status).map(|s| -s)).unwrap_or(1), String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr)),
                Err(e) => (127, format!("{e}\n")),
            };
            let _ = tx.send(Event::Job(id, status, out));
        });
        self.jobs.insert(id, Parked { queue, kind, cmd: command.to_string(), pid, fd, started: now_ms() });
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

#[cfg(test)]
mod surface_tests;

#[cfg(test)]
mod audit_tests;

#[cfg(test)]
mod size_tests {
    use super::*;

    // select-window and swap-window redraw through update_session_size alone
    #[test]
    fn session_size_recalculation_marks_its_clients_dirty() {
        let (mut srv, _events) = super::security_tests::fixture();
        let c = srv.clients.get_mut(&1).unwrap();
        c.readonly = false;
        c.acl_readonly = false;
        srv.exec(vec!["new-session".into(), "-d".into(), "-s".into(), "size".into()], &mut Ctx::default()).unwrap();
        let sid = *srv.sessions.keys().next().unwrap();
        srv.attach_client(1, sid);
        srv.clients.get_mut(&1).unwrap().dirty = false;
        // the size does not change, so no window is marked dirty by a resize
        srv.update_session_size(sid);
        assert!(srv.clients[&1].dirty);
        srv.clients.get_mut(&1).unwrap().dirty = false;
        srv.update_session_size_now(sid);
        assert!(srv.clients[&1].dirty);
        for p in srv.panes.values_mut() {
            p.kill();
        }
    }
}

/// A child's exit status as tmux reports it: the exit code, or minus the signal
/// number when a signal ended it (pane_dead_signal, pane_dead_status empty).
fn exit_status(s: &portable_pty::ExitStatus) -> i32 {
    match s.signal().and_then(signal_number) {
        Some(n) => -n,
        None => s.exit_code() as i32,
    }
}

/// The number of a signal from the strsignal(3) text portable-pty reports.
#[cfg(unix)]
fn signal_number(name: &str) -> Option<i32> {
    if let Some(n) = name.strip_prefix("Signal ").and_then(|n| n.parse().ok()) {
        return Some(n);
    }
    (1..65).find(|n| {
        let s = unsafe { libc::strsignal(*n) };
        !s.is_null() && unsafe { std::ffi::CStr::from_ptr(s) }.to_string_lossy() == name
    })
}

#[cfg(not(unix))]
fn signal_number(_name: &str) -> Option<i32> {
    None
}

