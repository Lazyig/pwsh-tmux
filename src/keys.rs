//! Terminal input decoding and encoding.
//!
//! * [`Parser`] splits the raw byte stream coming from the client's terminal (UTF-8 plus xterm
//!   VT input sequences, including Windows Terminal / conhost VT input and win32-input-mode)
//!   into [`Input`]s: keys, mouse events, bracketed pastes and unknown sequences.
//! * [`parse_key_name`] / [`key_name`] convert between [`Key`] and tmux key names.
//! * [`encode_key`] / [`encode_key_mode`] / [`encode_mouse`] re-encode input for a pane
//!   according to its terminal modes.
//! * [`mouse_key_name`] / [`click_key_name`] give tmux mouse binding names.

use std::fmt::Write as _;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Mods {
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        ctrl: false,
        meta: false,
        shift: false,
    };
    pub const CTRL: Mods = Mods {
        ctrl: true,
        meta: false,
        shift: false,
    };
    pub const META: Mods = Mods {
        ctrl: false,
        meta: true,
        shift: false,
    };
    pub const SHIFT: Mods = Mods {
        ctrl: false,
        meta: false,
        shift: true,
    };

    pub fn is_empty(&self) -> bool {
        !(self.ctrl || self.meta || self.shift)
    }

    /// Decode an xterm modifier parameter (`1 + bits`, bits: 1 shift, 2 alt, 4 ctrl, 8 meta).
    fn from_xterm(p: u32) -> Mods {
        let m = p.saturating_sub(1);
        Mods {
            shift: m & 1 != 0,
            meta: m & 2 != 0 || m & 8 != 0,
            ctrl: m & 4 != 0,
        }
    }

    /// The xterm modifier parameter for these modifiers (1 = none).
    fn xterm_param(&self) -> u32 {
        1 + self.shift as u32 + 2 * self.meta as u32 + 4 * self.ctrl as u32
    }

    pub fn prefix(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("C-");
        }
        if self.meta {
            s.push_str("M-");
        }
        if self.shift {
            s.push_str("S-");
        }
        s
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum KeyCode {
    Char(char),
    Enter,
    Tab,
    BTab,
    Escape,
    BSpace,
    Space,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
    /// Keypad key: '0'-'9', '/', '*', '-', '+', '.', ',', '=' and '\r' for KPEnter.
    KP(char),
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    pub code: KeyCode,
    pub mods: Mods,
}

impl Key {
    pub fn new(code: KeyCode, mods: Mods) -> Key {
        Key { code, mods }
    }
    pub fn plain(code: KeyCode) -> Key {
        Key {
            code,
            mods: Mods::NONE,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseKind {
    Down,
    Up,
    Drag,
    WheelUp,
    WheelDown,
    Move,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mouse {
    pub kind: MouseKind,
    /// 1 left, 2 middle, 3 right; 0 when not applicable (wheel, move, release of unknown button).
    pub button: u8,
    /// 0-based column.
    pub x: u16,
    /// 0-based row.
    pub y: u16,
    pub mods: Mods,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Input {
    /// A key and the raw bytes it came from.
    Key(Key, Vec<u8>),
    /// A mouse event and the raw bytes it came from.
    Mouse(Mouse, Vec<u8>),
    /// Bracketed paste content (without the `ESC[200~` / `ESC[201~` markers).
    Paste(Vec<u8>),
    /// Anything else (focus events, unrecognised sequences, invalid UTF-8, key-up events...).
    Unknown(Vec<u8>),
}

const ESC: u8 = 0x1b;
const PASTE_END: &[u8] = b"\x1b[201~";
/// A CSI longer than this without a final byte is treated as garbage.
const MAX_CSI: usize = 64;
const MAX_PASTE: usize = 4 << 20;

/// Incremental parser: feed bytes, get complete inputs. A lone ESC (or an incomplete sequence)
/// is held back; `flush_pending()` returns it as a key once the caller's escape-time expires.
pub struct Parser {
    buf: Vec<u8>,
    in_paste: bool,
    paste: Vec<u8>,
    paste_overflow: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Parser {
            buf: Vec::new(),
            in_paste: false,
            paste: Vec::new(),
            paste_overflow: false,
        }
    }

    /// A partial terminal theme/colour response needs a longer deadline than a bare Escape.
    pub fn theme_reply_pending(&self) -> bool {
        [b"\x1b]10;".as_slice(), b"\x1b]11;", b"\x1b[?997;"].iter()
            .any(|prefix| self.buf.starts_with(prefix) || (self.buf.len() >= 3 && prefix.starts_with(&self.buf)))
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<Input> {
        self.buf.extend_from_slice(data);
        self.run(false)
    }

    /// True when bytes are held back waiting for the rest of an escape sequence (the caller
    /// should arm its escape-time timer). Not set while inside a bracketed paste.
    pub fn has_pending(&self) -> bool {
        !self.in_paste && !self.buf.is_empty()
    }

    /// True while inside a bracketed paste whose end marker has not arrived yet.
    pub fn in_paste(&self) -> bool {
        self.in_paste
    }

    /// Resolve held-back bytes: a lone ESC becomes Escape, `ESC [` becomes M-[, partial UTF-8
    /// becomes Unknown, and so on. Does nothing while inside a bracketed paste.
    pub fn flush_pending(&mut self) -> Vec<Input> {
        if self.in_paste {
            return Vec::new();
        }
        self.run(true)
    }

    fn run(&mut self, force: bool) -> Vec<Input> {
        let mut out = Vec::new();
        loop {
            if self.in_paste {
                if let Some(pos) = find(&self.buf, PASTE_END) {
                    if self.paste.len().saturating_add(pos) > MAX_PASTE { self.paste_overflow = true; }
                    if !self.paste_overflow { self.paste.extend_from_slice(&self.buf[..pos]); }
                    self.buf.drain(..pos + PASTE_END.len());
                    if !self.paste_overflow { out.push(Input::Paste(std::mem::take(&mut self.paste))); }
                    self.paste.clear();
                    self.paste_overflow = false;
                    self.in_paste = false;
                    continue;
                }
                // Keep a suffix that could be the start of the end marker.
                let keep = (1..PASTE_END.len())
                    .rev()
                    .find(|&k| k <= self.buf.len() && self.buf.ends_with(&PASTE_END[..k]))
                    .unwrap_or(0);
                let take = self.buf.len() - keep;
                if self.paste.len().saturating_add(take) > MAX_PASTE { self.paste_overflow = true; }
                if !self.paste_overflow { self.paste.extend_from_slice(&self.buf[..take]); }
                else { self.paste.clear(); }
                self.buf.drain(..take);
                break;
            }
            if self.buf.is_empty() {
                break;
            }
            match parse_one(&self.buf, force) {
                Res::Incomplete => break,
                Res::Done(tok, n) => {
                    self.buf.drain(..n);
                    match tok {
                        Tok::In(i) => out.push(i),
                        Tok::PasteStart => self.in_paste = true,
                    }
                }
            }
        }
        out
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

enum Tok {
    In(Input),
    PasteStart,
}

enum Res {
    Done(Tok, usize),
    Incomplete,
}

fn key_tok(key: Key, raw: &[u8]) -> Tok {
    Tok::In(Input::Key(normalize(key), raw.to_vec()))
}

fn unknown_tok(raw: &[u8]) -> Tok {
    Tok::In(Input::Unknown(raw.to_vec()))
}

fn parse_one(buf: &[u8], force: bool) -> Res {
    let b = buf[0];
    if b == ESC {
        parse_esc(buf, force)
    } else if b < 0x20 || b == 0x7f {
        Res::Done(key_tok(control_key(b), &buf[..1]), 1)
    } else {
        parse_utf8(buf, force)
    }
}

/// Key for a C0 control byte or DEL.
fn control_key(b: u8) -> Key {
    let ctrl = |c: char| Key::new(KeyCode::Char(c), Mods::CTRL);
    match b {
        0x00 => Key::new(KeyCode::Space, Mods::CTRL),
        0x09 => Key::plain(KeyCode::Tab),
        0x0d => Key::plain(KeyCode::Enter),
        0x1b => Key::plain(KeyCode::Escape),
        0x7f => Key::plain(KeyCode::BSpace),
        0x01..=0x1a => ctrl((b'a' + b - 1) as char),
        0x1c => ctrl('\\'),
        0x1d => ctrl(']'),
        0x1e => ctrl('^'),
        0x1f => ctrl('_'),
        _ => Key::plain(KeyCode::Char(b as char)),
    }
}

fn parse_utf8(buf: &[u8], force: bool) -> Res {
    let b = buf[0];
    let need = match b {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return Res::Done(unknown_tok(&buf[..1]), 1),
    };
    let have = buf.len().min(need);
    if buf[1..have].iter().any(|&c| c & 0xc0 != 0x80) {
        return Res::Done(unknown_tok(&buf[..1]), 1);
    }
    if have < need {
        return if force {
            Res::Done(unknown_tok(&buf[..have]), have)
        } else {
            Res::Incomplete
        };
    }
    match std::str::from_utf8(&buf[..need])
        .ok()
        .and_then(|s| s.chars().next())
    {
        Some(c) => Res::Done(key_tok(Key::plain(KeyCode::Char(c)), &buf[..need]), need),
        None => Res::Done(unknown_tok(&buf[..1]), 1),
    }
}

/// `ESC <c>` that turned out not to be a sequence: M-<c>.
fn meta_fallback(buf: &[u8]) -> Res {
    let k = Key::new(KeyCode::Char(buf[1] as char), Mods::META);
    Res::Done(key_tok(k, &buf[..2]), 2)
}

fn parse_esc(buf: &[u8], force: bool) -> Res {
    if buf.len() == 1 {
        return if force {
            Res::Done(key_tok(Key::plain(KeyCode::Escape), &buf[..1]), 1)
        } else {
            Res::Incomplete
        };
    }
    let escape = || Res::Done(key_tok(Key::plain(KeyCode::Escape), &buf[..1]), 1);
    match buf[1] {
        b'[' => match parse_csi(buf) {
            Csi::Done(tok, n) => Res::Done(tok, n),
            Csi::Incomplete if !force => Res::Incomplete,
            Csi::Incomplete | Csi::Invalid => meta_fallback(buf),
        },
        b']' if buf.starts_with(b"\x1b]10;") || buf.starts_with(b"\x1b]11;") || b"\x1b]10;".starts_with(buf) || b"\x1b]11;".starts_with(buf) => {
            // tty_keys_colours: the reply ends at its first BEL or ST, within 127 bytes
            let end = (5..buf.len().min(5 + 127)).find(|&j| buf[j] == 7 || (buf[j - 1] == ESC && buf[j] == b'\\')).map(|j| j + 1);
            match end {
                Some(n) => Res::Done(unknown_tok(&buf[..n]), n),
                None if !force && buf.len() < 5 + 127 => Res::Incomplete,
                None => meta_fallback(buf),
            }
        }
        b'O' => parse_ss3(buf, force),
        ESC => {
            if buf.len() == 2 {
                return if force {
                    Res::Done(key_tok(Key::new(KeyCode::Escape, Mods::META), &buf[..2]), 2)
                } else {
                    Res::Incomplete
                };
            }
            if buf[2] != b'[' && buf[2] != b'O' {
                return escape();
            }
            match parse_one(&buf[1..], force) {
                Res::Incomplete => Res::Incomplete,
                // n > 2: a real sequence, not the "M-[" / "M-O" fallback.
                Res::Done(Tok::In(Input::Key(mut k, _)), n) if n > 2 => {
                    k.mods.meta = true;
                    Res::Done(key_tok(k, &buf[..n + 1]), n + 1)
                }
                _ => escape(),
            }
        }
        _ => match parse_one(&buf[1..], force) {
            Res::Incomplete => Res::Incomplete,
            Res::Done(Tok::In(Input::Key(mut k, _)), n) => {
                k.mods.meta = true;
                Res::Done(key_tok(k, &buf[..n + 1]), n + 1)
            }
            _ => escape(),
        },
    }
}

fn parse_ss3(buf: &[u8], force: bool) -> Res {
    let mut i = 2;
    while i < buf.len() && i < 5 && buf[i].is_ascii_digit() {
        i += 1;
    }
    if i >= buf.len() {
        return if force || i >= 5 {
            meta_fallback(buf)
        } else {
            Res::Incomplete
        };
    }
    let mods = match std::str::from_utf8(&buf[2..i])
        .ok()
        .and_then(|s| s.parse().ok())
    {
        Some(p) => Mods::from_xterm(p),
        None => Mods::NONE,
    };
    let code = match buf[i] {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        f @ b'P'..=b'S' => KeyCode::F(f - b'P' + 1),
        f @ b'p'..=b'y' => KeyCode::KP((b'0' + f - b'p') as char),
        b'j' => KeyCode::KP('*'),
        b'k' => KeyCode::KP('+'),
        b'l' => KeyCode::KP(','),
        b'm' => KeyCode::KP('-'),
        b'n' => KeyCode::KP('.'),
        b'o' => KeyCode::KP('/'),
        b'M' => KeyCode::KP('\r'),
        b'X' => KeyCode::KP('='),
        _ => return meta_fallback(buf),
    };
    Res::Done(key_tok(Key::new(code, mods), &buf[..i + 1]), i + 1)
}

enum Csi {
    Done(Tok, usize),
    Incomplete,
    Invalid,
}

fn parse_csi(buf: &[u8]) -> Csi {
    let mut i = 2;
    while i < buf.len() && (0x30..=0x3f).contains(&buf[i]) {
        i += 1;
    }
    let pend = i;
    while i < buf.len() && (0x20..=0x2f).contains(&buf[i]) {
        i += 1;
    }
    if i >= buf.len() {
        return if i > MAX_CSI {
            Csi::Invalid
        } else {
            Csi::Incomplete
        };
    }
    let f = buf[i];
    if !(0x40..=0x7e).contains(&f) {
        return Csi::Invalid;
    }
    let n = i + 1;
    let params = &buf[2..pend];
    let inter = &buf[pend..i];
    if f == b'M' && params.is_empty() && inter.is_empty() {
        // Legacy X10 mouse: ESC [ M Cb Cx Cy (raw bytes).
        if buf.len() < n + 3 {
            return Csi::Incomplete;
        }
        let raw = &buf[..n + 3];
        let tok = match decode_mouse(buf[n].wrapping_sub(32) as u32, false) {
            Some((kind, button, mods)) if buf[n] >= 32 => Tok::In(Input::Mouse(
                Mouse {
                    kind,
                    button,
                    x: buf[n + 1].saturating_sub(33) as u16,
                    y: buf[n + 2].saturating_sub(33) as u16,
                    mods,
                },
                raw.to_vec(),
            )),
            _ => unknown_tok(raw),
        };
        return Csi::Done(tok, n + 3);
    }
    Csi::Done(dispatch_csi(params, inter, f, &buf[..n]), n)
}

/// Split CSI parameters on ';', taking the first ':' sub-parameter. Empty → None.
fn parse_params(params: &[u8]) -> Option<Vec<Option<u32>>> {
    let s = std::str::from_utf8(params).ok()?;
    if s.is_empty() {
        return Some(Vec::new());
    }
    s.split(';')
        .map(|p| {
            let p = p.split(':').next().unwrap_or("");
            if p.is_empty() {
                Some(None)
            } else {
                p.parse().ok().map(Some)
            }
        })
        .collect()
}

fn dispatch_csi(params: &[u8], inter: &[u8], f: u8, raw: &[u8]) -> Tok {
    if !inter.is_empty() {
        return unknown_tok(raw);
    }
    if params.first() == Some(&b'<') {
        if (f == b'M' || f == b'm')
            && let Some(m) = sgr_mouse(&params[1..], f == b'm')
        {
            return Tok::In(Input::Mouse(m, raw.to_vec()));
        }
        return unknown_tok(raw);
    }
    if matches!(params.first(), Some(b'=' | b'>' | b'?')) {
        return unknown_tok(raw);
    }
    let Some(p) = parse_params(params) else {
        return unknown_tok(raw);
    };
    let get = |i: usize| p.get(i).copied().flatten();
    let mods = |i: usize| Mods::from_xterm(get(i).unwrap_or(1));
    let key = match f {
        b'~' => match get(0) {
            Some(200) => return Tok::PasteStart,
            Some(27) => get(2).and_then(|c| key_from_codepoint(c, mods(1))),
            Some(n) => tilde_key(n).map(|c| Key::new(c, mods(1))),
            None => None,
        },
        b'u' => get(0).and_then(|c| key_from_codepoint(c, mods(1))),
        b'A' | b'B' | b'C' | b'D' | b'H' | b'F' | b'P' | b'Q' | b'R' | b'S' => {
            let code = match f {
                b'A' => KeyCode::Up,
                b'B' => KeyCode::Down,
                b'C' => KeyCode::Right,
                b'D' => KeyCode::Left,
                b'H' => KeyCode::Home,
                b'F' => KeyCode::End,
                _ => KeyCode::F(f - b'P' + 1),
            };
            Some(Key::new(code, mods(1)))
        }
        b'Z' => {
            let mut m = mods(1);
            m.shift = false;
            Some(Key::new(KeyCode::BTab, m))
        }
        b'_' => win32_key(&p),
        _ => None,
    };
    match key {
        Some(k) => key_tok(k, raw),
        None => unknown_tok(raw),
    }
}

fn tilde_key(n: u32) -> Option<KeyCode> {
    Some(match n {
        1 | 7 => KeyCode::Home,
        2 => KeyCode::Insert,
        3 => KeyCode::Delete,
        4 | 8 => KeyCode::End,
        5 => KeyCode::PageUp,
        6 => KeyCode::PageDown,
        11..=15 => KeyCode::F((n - 10) as u8),
        17..=21 => KeyCode::F((n - 11) as u8),
        23 | 24 => KeyCode::F((n - 12) as u8),
        _ => return None,
    })
}

/// Key for a Unicode code point reported by CSI u / modifyOtherKeys.
fn key_from_codepoint(cp: u32, mut mods: Mods) -> Option<Key> {
    let code = match cp {
        9 if mods.shift => {
            mods.shift = false;
            KeyCode::BTab
        }
        9 => KeyCode::Tab,
        13 => KeyCode::Enter,
        27 => KeyCode::Escape,
        8 | 127 => KeyCode::BSpace,
        32 => KeyCode::Space,
        0..=31 => {
            let mut k = control_key(cp as u8);
            k.mods.meta |= mods.meta;
            k.mods.shift |= mods.shift;
            return Some(k);
        }
        _ => KeyCode::Char(char::from_u32(cp)?),
    };
    Some(Key::new(code, mods))
}

/// win32-input-mode: `ESC [ Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`. Key-up and modifier-only events → None.
fn win32_key(p: &[Option<u32>]) -> Option<Key> {
    let g = |i: usize| p.get(i).copied().flatten().unwrap_or(0);
    let (vk, uc, kd, cs) = (g(0), g(2), g(3), g(4));
    if kd == 0 {
        return None;
    }
    let alt = cs & 0x3 != 0; // RIGHT_ALT | LEFT_ALT
    let ctrl = cs & 0xc != 0; // RIGHT_CTRL | LEFT_CTRL
    let shift = cs & 0x10 != 0;
    let mods = Mods {
        ctrl,
        meta: alt,
        shift,
    };
    let special = match vk {
        0x08 => Some(KeyCode::BSpace),
        0x09 => Some(KeyCode::Tab),
        0x0d => Some(KeyCode::Enter),
        0x1b => Some(KeyCode::Escape),
        0x20 => Some(KeyCode::Space),
        0x21 => Some(KeyCode::PageUp),
        0x22 => Some(KeyCode::PageDown),
        0x23 => Some(KeyCode::End),
        0x24 => Some(KeyCode::Home),
        0x25 => Some(KeyCode::Left),
        0x26 => Some(KeyCode::Up),
        0x27 => Some(KeyCode::Right),
        0x28 => Some(KeyCode::Down),
        0x2d => Some(KeyCode::Insert),
        0x2e => Some(KeyCode::Delete),
        0x70..=0x7b => Some(KeyCode::F((vk - 0x6f) as u8)),
        _ => None,
    };
    if let Some(code) = special {
        let mut m = mods;
        let code = if code == KeyCode::Tab && shift {
            m.shift = false;
            KeyCode::BTab
        } else {
            code
        };
        if code == KeyCode::Space && ctrl && alt && uc != 0 && uc != 0x20 {
            // AltGr+Space producing some character.
            return char::from_u32(uc).map(|c| Key::plain(KeyCode::Char(c)));
        }
        return Some(Key::new(code, m));
    }
    if uc != 0 {
        if uc < 0x20 || uc == 0x7f {
            let mut k = control_key(uc as u8);
            k.mods.meta |= alt;
            return Some(k);
        }
        let c = char::from_u32(uc)?;
        // Ctrl+Alt together on a printable character is AltGr.
        let m = if ctrl && alt {
            Mods::NONE
        } else {
            Mods {
                ctrl,
                meta: alt,
                shift: false,
            }
        };
        return Some(Key::new(KeyCode::Char(c), m));
    }
    match vk {
        0x30..=0x39 | 0x41..=0x5a => {
            let c = (vk as u8).to_ascii_lowercase() as char;
            Some(Key::new(KeyCode::Char(c), mods))
        }
        _ => None,
    }
}

/// Decode xterm mouse button bits. Returns None for unsupported buttons (horizontal wheel, 8-11).
fn decode_mouse(b: u32, release: bool) -> Option<(MouseKind, u8, Mods)> {
    if b & 128 != 0 {
        return None;
    }
    let mods = Mods {
        shift: b & 4 != 0,
        meta: b & 8 != 0,
        ctrl: b & 16 != 0,
    };
    let low = (b & 3) as u8;
    let (kind, button) = if b & 64 != 0 {
        match low {
            0 => (MouseKind::WheelUp, 0),
            1 => (MouseKind::WheelDown, 0),
            _ => return None,
        }
    } else if b & 32 != 0 {
        if low == 3 {
            (MouseKind::Move, 0)
        } else {
            (MouseKind::Drag, low + 1)
        }
    } else if low == 3 {
        (MouseKind::Up, 0)
    } else if release {
        (MouseKind::Up, low + 1)
    } else {
        (MouseKind::Down, low + 1)
    };
    Some((kind, button, mods))
}

fn sgr_mouse(params: &[u8], release: bool) -> Option<Mouse> {
    let p = parse_params(params)?;
    if p.len() != 3 {
        return None;
    }
    let (b, x, y) = (p[0]?, p[1]?, p[2]?);
    let (kind, button, mods) = decode_mouse(b, release)?;
    let clamp = |v: u32| v.saturating_sub(1).min(u16::MAX as u32) as u16;
    Some(Mouse {
        kind,
        button,
        x: clamp(x),
        y: clamp(y),
        mods,
    })
}

/// Canonicalise a key: Char(' ') → Space; C-i → Tab, C-m → Enter, C-[ → Escape, C-@ → C-Space,
/// C-? → BSpace; C-<letter> → lowercase; S-<letter> (no ctrl) → uppercase without shift.
pub fn normalize(k: Key) -> Key {
    let Key { mut code, mut mods } = k;
    if code == KeyCode::Char(' ') {
        code = KeyCode::Space;
    }
    if let KeyCode::Char(c) = code
        && mods.ctrl
        && !mods.shift
    {
        let (new, keep_ctrl) = match c {
            'i' | 'I' => (KeyCode::Tab, false),
            'm' | 'M' => (KeyCode::Enter, false),
            '[' => (KeyCode::Escape, false),
            '?' => (KeyCode::BSpace, false),
            '@' => (KeyCode::Space, true),
            _ => (code.clone(), true),
        };
        code = new;
        mods.ctrl = keep_ctrl;
    }
    if let KeyCode::Char(c) = code
        && c.is_ascii_alphabetic()
    {
        if mods.ctrl {
            code = KeyCode::Char(c.to_ascii_lowercase());
        } else if mods.shift {
            code = KeyCode::Char(c.to_ascii_uppercase());
            mods.shift = false;
        }
    }
    Key { code, mods }
}

const NAMES: &[(&str, KeyCode)] = &[
    ("Enter", KeyCode::Enter),
    ("Tab", KeyCode::Tab),
    ("BTab", KeyCode::BTab),
    ("Escape", KeyCode::Escape),
    ("BSpace", KeyCode::BSpace),
    ("Space", KeyCode::Space),
    ("Up", KeyCode::Up),
    ("Down", KeyCode::Down),
    ("Left", KeyCode::Left),
    ("Right", KeyCode::Right),
    ("Home", KeyCode::Home),
    ("End", KeyCode::End),
    ("PPage", KeyCode::PageUp),
    ("PageUp", KeyCode::PageUp),
    ("PgUp", KeyCode::PageUp),
    ("NPage", KeyCode::PageDown),
    ("PageDown", KeyCode::PageDown),
    ("PgDn", KeyCode::PageDown),
    ("IC", KeyCode::Insert),
    ("Insert", KeyCode::Insert),
    ("DC", KeyCode::Delete),
    ("Delete", KeyCode::Delete),
    ("KPEnter", KeyCode::KP('\r')),
];

/// Parse a tmux key name ("C-b", "^B", "M-Left", "S-Up", "C-M-x", "F12", "PPage", ...) into a
/// normalised Key. Mouse key names and "Any" return None (handled by the binding layer).
pub fn parse_key_name(s: &str) -> Option<Key> {
    let mut mods = Mods::NONE;
    let mut rest = s;
    loop {
        let b = rest.as_bytes();
        if b.len() >= 3 && b[1] == b'-' {
            match b[0] {
                b'C' | b'c' => mods.ctrl = true,
                b'M' | b'm' => mods.meta = true,
                b'S' | b's' => mods.shift = true,
                _ => break,
            }
            rest = &rest[2..];
        } else {
            break;
        }
    }
    if rest.len() == 2 && rest.starts_with('^') {
        mods.ctrl = true;
        rest = &rest[1..];
    }
    let mut chars = rest.chars();
    let code = match (chars.next(), chars.next()) {
        (None, _) => return None,
        (Some(c), None) => KeyCode::Char(c),
        _ => lookup_name(rest)?,
    };
    Some(normalize(Key::new(code, mods)))
}

fn lookup_name(name: &str) -> Option<KeyCode> {
    if let Some((_, c)) = NAMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        return Some(c.clone());
    }
    let b = name.as_bytes();
    if b.len() >= 2 && (b[0] == b'F' || b[0] == b'f') {
        let n: u8 = name[1..].parse().ok()?;
        return (1..=12).contains(&n).then_some(KeyCode::F(n));
    }
    if b.len() == 3 && b[..2].eq_ignore_ascii_case(b"KP") {
        let c = b[2] as char;
        if c.is_ascii_digit() || "/*-+.,=".contains(c) {
            return Some(KeyCode::KP(c));
        }
    }
    None
}

/// Canonical tmux name for a key ("C-b", "M-Left", "Space", "F5", "\"" etc.) — inverse of
/// [`parse_key_name`] for normalised keys.
pub fn key_name(k: &Key) -> String {
    let mut s = k.mods.prefix();
    match &k.code {
        KeyCode::Char(c) => s.push(*c),
        KeyCode::Enter => s.push_str("Enter"),
        KeyCode::Tab => s.push_str("Tab"),
        KeyCode::BTab => s.push_str("BTab"),
        KeyCode::Escape => s.push_str("Escape"),
        KeyCode::BSpace => s.push_str("BSpace"),
        KeyCode::Space => s.push_str("Space"),
        KeyCode::Up => s.push_str("Up"),
        KeyCode::Down => s.push_str("Down"),
        KeyCode::Left => s.push_str("Left"),
        KeyCode::Right => s.push_str("Right"),
        KeyCode::Home => s.push_str("Home"),
        KeyCode::End => s.push_str("End"),
        KeyCode::PageUp => s.push_str("PPage"),
        KeyCode::PageDown => s.push_str("NPage"),
        KeyCode::Insert => s.push_str("IC"),
        KeyCode::Delete => s.push_str("DC"),
        KeyCode::F(n) => {
            let _ = write!(s, "F{n}");
        }
        KeyCode::KP('\r') => s.push_str("KPEnter"),
        KeyCode::KP(c) => {
            let _ = write!(s, "KP{c}");
        }
    }
    s
}

/// Control byte for Ctrl+c, if there is one.
fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c {
        'a'..='z' | 'A'..='Z' => c as u8 & 0x1f,
        '@' | ' ' | '2' => 0x00,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' | '~' => 0x1e,
        '_' | '7' | '/' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

/// Extended-key encoding (tmux `extended-keys`) for keys whose modifiers the legacy encoding
/// can't carry (C-Enter, S-Tab-like combos, C-1, C-S-a ...), or for every modified key when
/// `all` (extended-keys always). `csi_u`: `ESC [ code ; mod u`, else xterm's
/// `ESC [ 27 ; mod ; code ~`. None means the legacy encoding is fine.
pub fn encode_extended(k: &Key, all: bool, csi_u: bool) -> Option<Vec<u8>> {
    let k = normalize(k.clone());
    let m = k.mods;
    let n = m.xterm_param();
    if n <= 1 {
        return None;
    }
    let code: u32 = match k.code {
        KeyCode::Char(c) => c as u32,
        KeyCode::Enter => 13,
        KeyCode::Tab => 9,
        KeyCode::BSpace => 127,
        KeyCode::Escape => 27,
        KeyCode::Space => 32,
        // arrows, F-keys, paging keys already carry modifiers (CSI 1;N X)
        _ => return None,
    };
    let lossy = match k.code {
        // C-letter and M-anything encode fine; C-S-letter, C-digit, C-punctuation don't
        KeyCode::Char(c) => (m.ctrl && (m.shift || !(c.is_ascii_alphabetic() || "[\\]^_@ ".contains(c)))) || (m.shift && !m.ctrl && !m.meta),
        _ => m.ctrl || m.shift,
    };
    if !lossy && !all {
        return None;
    }
    Some(if csi_u { format!("\x1b[{code};{n}u") } else { format!("\x1b[27;{n};{code}~") }.into_bytes())
}

/// Encode a key for an application: respects `app_cursor` (DECCKM: ESC O A vs ESC [ A).
/// Ctrl+letter → control byte, Meta → ESC prefix, modified arrows/F-keys → xterm CSI 1;N forms,
/// Enter → "\r", BSpace → 0x7f. Keypad keys are sent as their plain characters.
pub fn encode_key(k: &Key, app_cursor: bool) -> Vec<u8> {
    encode_key_mode(k, app_cursor, false)
}

/// [`encode_key`] with application keypad mode (DECKPAM): keypad keys become `ESC O <x>`.
pub fn encode_key_mode(k: &Key, app_cursor: bool, app_keypad: bool) -> Vec<u8> {
    let k = normalize(k.clone());
    let m = k.mods;
    let n = m.xterm_param();
    let modified = n > 1;
    let cursor = |f: char| -> Vec<u8> {
        if modified {
            format!("\x1b[1;{n}{f}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{f}").into_bytes()
        } else {
            format!("\x1b[{f}").into_bytes()
        }
    };
    let fkey_ss3 = |f: char| -> Vec<u8> {
        if modified {
            format!("\x1b[1;{n}{f}").into_bytes()
        } else {
            format!("\x1bO{f}").into_bytes()
        }
    };
    let tilde = |num: u32| -> Vec<u8> {
        if modified {
            format!("\x1b[{num};{n}~").into_bytes()
        } else {
            format!("\x1b[{num}~").into_bytes()
        }
    };
    let simple: Vec<u8> = match k.code {
        KeyCode::Up => return cursor('A'),
        KeyCode::Down => return cursor('B'),
        KeyCode::Right => return cursor('C'),
        KeyCode::Left => return cursor('D'),
        KeyCode::Home => return cursor('H'),
        KeyCode::End => return cursor('F'),
        KeyCode::Insert => return tilde(2),
        KeyCode::Delete => return tilde(3),
        KeyCode::PageUp => return tilde(5),
        KeyCode::PageDown => return tilde(6),
        KeyCode::F(f @ 1..=4) => return fkey_ss3((b'P' + f - 1) as char),
        KeyCode::F(f @ 5..=12) => {
            const NUMS: [u32; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
            return tilde(NUMS[(f - 5) as usize]);
        }
        KeyCode::F(_) => return Vec::new(),
        KeyCode::BTab => b"\x1b[Z".to_vec(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::Escape => vec![ESC],
        KeyCode::BSpace => vec![if m.ctrl { 0x08 } else { 0x7f }],
        KeyCode::Space => vec![if m.ctrl { 0x00 } else { b' ' }],
        KeyCode::Char(c) => match (m.ctrl, ctrl_byte(c)) {
            (true, Some(b)) => vec![b],
            _ => c.to_string().into_bytes(),
        },
        KeyCode::KP(c) => {
            if app_keypad {
                let f = match c {
                    '0'..='9' => (b'p' + (c as u8 - b'0')) as char,
                    '.' => 'n',
                    '/' => 'o',
                    '*' => 'j',
                    '-' => 'm',
                    '+' => 'k',
                    ',' => 'l',
                    '=' => 'X',
                    _ => 'M',
                };
                format!("\x1bO{f}").into_bytes()
            } else {
                c.to_string().into_bytes()
            }
        }
    };
    if m.meta {
        let mut v = vec![ESC];
        v.extend(simple);
        v
    } else {
        simple
    }
}

/// Encode a mouse event (already translated to pane-relative 0-based coordinates) for a pane
/// application that enabled mouse reporting, in SGR (1006) or legacy X10 (1000) encoding.
/// Returns None if the event is not reportable in `mode` (or not encodable in legacy form).
pub fn encode_mouse(m: &Mouse, mode: vt100::MouseProtocolMode, sgr: bool) -> Option<Vec<u8>> {
    use vt100::MouseProtocolMode as P;
    let ok = match m.kind {
        MouseKind::Down | MouseKind::WheelUp | MouseKind::WheelDown => mode != P::None,
        MouseKind::Up => matches!(mode, P::PressRelease | P::ButtonMotion | P::AnyMotion),
        MouseKind::Drag => matches!(mode, P::ButtonMotion | P::AnyMotion),
        MouseKind::Move => mode == P::AnyMotion,
    };
    if !ok {
        return None;
    }
    let btn = |b: u8| -> u32 { b.clamp(1, 3) as u32 - 1 };
    let mut code = match m.kind {
        MouseKind::Down => btn(m.button),
        MouseKind::Up => {
            if sgr && m.button != 0 {
                btn(m.button)
            } else {
                3
            }
        }
        MouseKind::Drag => 32 + btn(m.button),
        MouseKind::Move => 35,
        MouseKind::WheelUp => 64,
        MouseKind::WheelDown => 65,
    };
    if mode != P::Press {
        // X10 (9) mode reports no modifiers.
        code += 4 * m.mods.shift as u32 + 8 * m.mods.meta as u32 + 16 * m.mods.ctrl as u32;
    }
    let (x, y) = (m.x as u32 + 1, m.y as u32 + 1);
    if sgr {
        let f = if m.kind == MouseKind::Up { 'm' } else { 'M' };
        Some(format!("\x1b[<{code};{x};{y}{f}").into_bytes())
    } else {
        if x + 32 > 255 || y + 32 > 255 {
            return None;
        }
        Some(vec![
            ESC,
            b'[',
            b'M',
            (32 + code) as u8,
            (32 + x) as u8,
            (32 + y) as u8,
        ])
    }
}

/// tmux mouse key name for bindings, e.g. MouseDown1Pane, MouseUp1Status, MouseDrag1Border,
/// WheelUpPane, MouseMovePane, M-MouseDown3Pane. `area` is "Pane", "Status", "Border",
/// "StatusLeft", "StatusRight", ...
pub fn mouse_key_name(m: &Mouse, area: &str) -> String {
    let mut s = m.mods.prefix();
    let _ = match m.kind {
        MouseKind::Down => write!(s, "MouseDown{}{area}", m.button),
        MouseKind::Up => write!(s, "MouseUp{}{area}", m.button),
        MouseKind::Drag => write!(s, "MouseDrag{}{area}", m.button),
        MouseKind::WheelUp => write!(s, "WheelUp{area}"),
        MouseKind::WheelDown => write!(s, "WheelDown{area}"),
        MouseKind::Move => write!(s, "MouseMove{area}"),
    };
    s
}

/// Multi-click names: count 2 → DoubleClick1Pane, 3 → TripleClick1Pane, otherwise the
/// plain [`mouse_key_name`]. Click counting / timing is left to the caller.
pub fn click_key_name(count: u8, m: &Mouse, area: &str) -> String {
    let what = match count {
        2 => "DoubleClick",
        3 => "TripleClick",
        _ => return mouse_key_name(m, area),
    };
    format!("{}{what}{}{area}", m.mods.prefix(), m.button)
}

/// End-of-drag name (tmux "MouseDragEnd1Pane").
pub fn drag_end_key_name(m: &Mouse, area: &str) -> String {
    format!("{}MouseDragEnd{}{area}", m.mods.prefix(), m.button)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vt100::MouseProtocolMode as P;

    fn k(code: KeyCode) -> Key {
        Key::plain(code)
    }
    fn km(code: KeyCode, ctrl: bool, meta: bool, shift: bool) -> Key {
        Key::new(code, Mods { ctrl, meta, shift })
    }
    fn ch(c: char) -> Key {
        k(KeyCode::Char(c))
    }
    fn ctrl(c: char) -> Key {
        Key::new(KeyCode::Char(c), Mods::CTRL)
    }
    fn meta(code: KeyCode) -> Key {
        Key::new(code, Mods::META)
    }

    /// Feed all bytes at once, flush, and return keys only (panics on non-keys).
    fn keys(data: &[u8]) -> Vec<Key> {
        let mut p = Parser::new();
        let mut v = p.feed(data);
        v.extend(p.flush_pending());
        v.into_iter()
            .map(|i| match i {
                Input::Key(k, _) => k,
                other => panic!("expected key, got {other:?} for {data:?}"),
            })
            .collect()
    }
    fn key1(data: &[u8]) -> Key {
        let v = keys(data);
        assert_eq!(v.len(), 1, "{data:?} -> {v:?}");
        v.into_iter().next().unwrap()
    }
    fn all(data: &[u8]) -> Vec<Input> {
        let mut p = Parser::new();
        let mut v = p.feed(data);
        v.extend(p.flush_pending());
        v
    }

    #[test]
    fn printable_and_utf8() {
        assert_eq!(keys(b"aZ%\""), vec![ch('a'), ch('Z'), ch('%'), ch('"')]);
        assert_eq!(keys("한글".as_bytes()), vec![ch('한'), ch('글')]);
        assert_eq!(keys("é😀".as_bytes()), vec![ch('é'), ch('😀')]);
        assert_eq!(key1(b" "), k(KeyCode::Space));
    }

    #[test]
    fn raw_bytes_preserved() {
        let mut p = Parser::new();
        let v = p.feed("가\x1b[1;5A".as_bytes());
        assert_eq!(v[0], Input::Key(ch('가'), "가".as_bytes().to_vec()));
        assert_eq!(
            v[1],
            Input::Key(km(KeyCode::Up, true, false, false), b"\x1b[1;5A".to_vec())
        );
    }

    #[test]
    fn utf8_split_across_feeds() {
        let bytes = "한".as_bytes();
        let mut p = Parser::new();
        assert!(p.feed(&bytes[..1]).is_empty());
        assert!(p.has_pending());
        assert!(p.feed(&bytes[1..2]).is_empty());
        assert_eq!(
            p.feed(&bytes[2..]),
            vec![Input::Key(ch('한'), bytes.to_vec())]
        );
        assert!(!p.has_pending());
    }

    #[test]
    fn invalid_utf8() {
        assert_eq!(
            all(b"\x80a"),
            vec![
                Input::Unknown(vec![0x80]),
                Input::Key(ch('a'), b"a".to_vec())
            ]
        );
        assert_eq!(
            all(b"\xe0a"),
            vec![
                Input::Unknown(vec![0xe0]),
                Input::Key(ch('a'), b"a".to_vec())
            ]
        );
        // Truncated sequence flushed.
        assert_eq!(all(b"\xed\x95"), vec![Input::Unknown(vec![0xed, 0x95])]);
    }

    #[test]
    fn control_chars() {
        assert_eq!(key1(b"\x00"), km(KeyCode::Space, true, false, false));
        assert_eq!(key1(b"\x01"), ctrl('a'));
        assert_eq!(key1(b"\x02"), ctrl('b'));
        assert_eq!(key1(b"\x1a"), ctrl('z'));
        assert_eq!(key1(b"\x08"), ctrl('h'));
        assert_eq!(key1(b"\x0a"), ctrl('j'));
        assert_eq!(key1(b"\x09"), k(KeyCode::Tab));
        assert_eq!(key1(b"\x0d"), k(KeyCode::Enter));
        assert_eq!(key1(b"\x1c"), ctrl('\\'));
        assert_eq!(key1(b"\x1d"), ctrl(']'));
        assert_eq!(key1(b"\x1e"), ctrl('^'));
        assert_eq!(key1(b"\x1f"), ctrl('_'));
        assert_eq!(key1(b"\x7f"), k(KeyCode::BSpace));
    }

    #[test]
    fn meta_keys() {
        assert_eq!(key1(b"\x1bx"), meta(KeyCode::Char('x')));
        assert_eq!(key1(b"\x1bX"), meta(KeyCode::Char('X')));
        assert_eq!(key1(b"\x1b\x01"), km(KeyCode::Char('a'), true, true, false));
        assert_eq!(key1(b"\x1b\x7f"), meta(KeyCode::BSpace));
        assert_eq!(key1(b"\x1b\r"), meta(KeyCode::Enter));
        assert_eq!(key1(b"\x1b "), meta(KeyCode::Space));
        assert_eq!(key1("\x1b가".as_bytes()), meta(KeyCode::Char('가')));
        assert_eq!(key1(b"\x1b\x1b[A"), meta(KeyCode::Up));
        assert_eq!(key1(b"\x1b\x1bOA"), meta(KeyCode::Up));
        assert_eq!(key1(b"\x1b\x1b[5~"), meta(KeyCode::PageUp));
        assert_eq!(
            keys(b"\x1b\x1bx"),
            vec![k(KeyCode::Escape), meta(KeyCode::Char('x'))]
        );
        let v = all(b"\x1bx");
        assert_eq!(
            v,
            vec![Input::Key(meta(KeyCode::Char('x')), b"\x1bx".to_vec())]
        );
    }

    #[test]
    fn pending_escape_and_flush() {
        let mut p = Parser::new();
        assert!(p.feed(b"\x1b").is_empty());
        assert!(p.has_pending());
        assert_eq!(
            p.flush_pending(),
            vec![Input::Key(k(KeyCode::Escape), vec![ESC])]
        );
        assert!(!p.has_pending());
        assert!(p.flush_pending().is_empty());

        // ESC then a later byte that completes a meta key.
        assert!(p.feed(b"\x1b").is_empty());
        assert_eq!(
            p.feed(b"a"),
            vec![Input::Key(meta(KeyCode::Char('a')), b"\x1ba".to_vec())]
        );

        // ESC ESC flushed → M-Escape.
        assert!(p.feed(b"\x1b\x1b").is_empty());
        assert_eq!(
            p.flush_pending(),
            vec![Input::Key(meta(KeyCode::Escape), b"\x1b\x1b".to_vec())]
        );

        // Incomplete CSI flushed → M-[ followed by the rest.
        assert!(p.feed(b"\x1b[").is_empty());
        assert_eq!(
            p.flush_pending(),
            vec![Input::Key(meta(KeyCode::Char('[')), b"\x1b[".to_vec())]
        );
        assert!(p.feed(b"\x1b[1;").is_empty());
        let v: Vec<Key> = p
            .flush_pending()
            .into_iter()
            .map(|i| {
                if let Input::Key(k, _) = i {
                    k
                } else {
                    panic!()
                }
            })
            .collect();
        assert_eq!(v, vec![meta(KeyCode::Char('[')), ch('1'), ch(';')]);

        // ESC O alone flushed → M-O.
        assert!(p.feed(b"\x1bO").is_empty());
        assert_eq!(
            p.flush_pending(),
            vec![Input::Key(meta(KeyCode::Char('O')), b"\x1bO".to_vec())]
        );

        // Three escapes: Escape, then M-Escape on flush.
        assert_eq!(
            p.feed(b"\x1b\x1b\x1b"),
            vec![Input::Key(k(KeyCode::Escape), vec![ESC])]
        );
        assert_eq!(
            p.flush_pending(),
            vec![Input::Key(meta(KeyCode::Escape), b"\x1b\x1b".to_vec())]
        );
    }

    #[test]
    fn escape_followed_by_keys_without_timeout() {
        assert_eq!(keys(b"\x1b\x1b"), vec![meta(KeyCode::Escape)]);
        assert_eq!(keys(b"a\x1b"), vec![ch('a'), k(KeyCode::Escape)]);
    }

    #[test]
    fn cursor_keys() {
        for (seq, code) in [
            (&b"\x1b[A"[..], KeyCode::Up),
            (b"\x1b[B", KeyCode::Down),
            (b"\x1b[C", KeyCode::Right),
            (b"\x1b[D", KeyCode::Left),
            (b"\x1bOA", KeyCode::Up),
            (b"\x1bOB", KeyCode::Down),
            (b"\x1bOC", KeyCode::Right),
            (b"\x1bOD", KeyCode::Left),
        ] {
            assert_eq!(key1(seq), k(code));
        }
    }

    #[test]
    fn xterm_modifiers() {
        let cases = [
            (2, false, false, true),
            (3, false, true, false),
            (4, false, true, true),
            (5, true, false, false),
            (6, true, false, true),
            (7, true, true, false),
            (8, true, true, true),
        ];
        for (n, c, m, s) in cases {
            assert_eq!(
                key1(format!("\x1b[1;{n}A").as_bytes()),
                km(KeyCode::Up, c, m, s)
            );
            assert_eq!(
                key1(format!("\x1b[1;{n}D").as_bytes()),
                km(KeyCode::Left, c, m, s)
            );
            assert_eq!(
                key1(format!("\x1b[1;{n}H").as_bytes()),
                km(KeyCode::Home, c, m, s)
            );
            assert_eq!(
                key1(format!("\x1b[5;{n}~").as_bytes()),
                km(KeyCode::PageUp, c, m, s)
            );
            assert_eq!(
                key1(format!("\x1b[3;{n}~").as_bytes()),
                km(KeyCode::Delete, c, m, s)
            );
            assert_eq!(
                key1(format!("\x1b[1;{n}P").as_bytes()),
                km(KeyCode::F(1), c, m, s)
            );
            assert_eq!(
                key1(format!("\x1b[24;{n}~").as_bytes()),
                km(KeyCode::F(12), c, m, s)
            );
        }
        // SS3 with modifier digit (old xterm).
        assert_eq!(key1(b"\x1bO5A"), km(KeyCode::Up, true, false, false));
    }

    #[test]
    fn home_end_ins_del_pages() {
        for (seq, code) in [
            (&b"\x1b[H"[..], KeyCode::Home),
            (b"\x1b[F", KeyCode::End),
            (b"\x1bOH", KeyCode::Home),
            (b"\x1bOF", KeyCode::End),
            (b"\x1b[1~", KeyCode::Home),
            (b"\x1b[4~", KeyCode::End),
            (b"\x1b[7~", KeyCode::Home),
            (b"\x1b[8~", KeyCode::End),
            (b"\x1b[2~", KeyCode::Insert),
            (b"\x1b[3~", KeyCode::Delete),
            (b"\x1b[5~", KeyCode::PageUp),
            (b"\x1b[6~", KeyCode::PageDown),
        ] {
            assert_eq!(key1(seq), k(code), "{seq:?}");
        }
    }

    #[test]
    fn function_keys() {
        assert_eq!(key1(b"\x1bOP"), k(KeyCode::F(1)));
        assert_eq!(key1(b"\x1bOQ"), k(KeyCode::F(2)));
        assert_eq!(key1(b"\x1bOR"), k(KeyCode::F(3)));
        assert_eq!(key1(b"\x1bOS"), k(KeyCode::F(4)));
        assert_eq!(key1(b"\x1b[11~"), k(KeyCode::F(1)));
        assert_eq!(key1(b"\x1b[14~"), k(KeyCode::F(4)));
        let nums = [15, 17, 18, 19, 20, 21, 23, 24];
        for (i, n) in nums.iter().enumerate() {
            assert_eq!(
                key1(format!("\x1b[{n}~").as_bytes()),
                k(KeyCode::F(5 + i as u8))
            );
        }
        assert_eq!(key1(b"\x1b[15;5~"), km(KeyCode::F(5), true, false, false));
        assert_eq!(all(b"\x1b[16~"), vec![Input::Unknown(b"\x1b[16~".to_vec())]);
    }

    #[test]
    fn keypad_ss3() {
        assert_eq!(key1(b"\x1bOp"), k(KeyCode::KP('0')));
        assert_eq!(key1(b"\x1bOy"), k(KeyCode::KP('9')));
        assert_eq!(key1(b"\x1bOM"), k(KeyCode::KP('\r')));
        assert_eq!(key1(b"\x1bOo"), k(KeyCode::KP('/')));
        assert_eq!(key1(b"\x1bOj"), k(KeyCode::KP('*')));
        assert_eq!(key1(b"\x1bOm"), k(KeyCode::KP('-')));
        assert_eq!(key1(b"\x1bOk"), k(KeyCode::KP('+')));
        assert_eq!(key1(b"\x1bOn"), k(KeyCode::KP('.')));
        // Unknown SS3 final → M-O followed by the char.
        assert_eq!(keys(b"\x1bOz"), vec![meta(KeyCode::Char('O')), ch('z')]);
    }

    #[test]
    fn btab() {
        assert_eq!(key1(b"\x1b[Z"), k(KeyCode::BTab));
        assert_eq!(key1(b"\x1b[1;2Z"), k(KeyCode::BTab));
        assert_eq!(key1(b"\x1b\x1b[Z"), meta(KeyCode::BTab));
    }

    #[test]
    fn focus_and_unknown_csi() {
        assert_eq!(all(b"\x1b[I"), vec![Input::Unknown(b"\x1b[I".to_vec())]);
        assert_eq!(all(b"\x1b[O"), vec![Input::Unknown(b"\x1b[O".to_vec())]);
        assert_eq!(
            all(b"\x1b[?1;2c"),
            vec![Input::Unknown(b"\x1b[?1;2c".to_vec())]
        );
        assert_eq!(all(b"\x1b[1 q"), vec![Input::Unknown(b"\x1b[1 q".to_vec())]);
        // Invalid byte inside a CSI → M-[ and reparse.
        assert_eq!(
            keys(b"\x1b[1\x01"),
            vec![meta(KeyCode::Char('[')), ch('1'), ctrl('a')]
        );
    }

    #[test]
    fn extended_encoding() {
        let k = |s: &str| parse_key_name(s).unwrap();
        assert_eq!(encode_extended(&k("C-Enter"), false, true), Some(b"\x1b[13;5u".to_vec()));
        assert_eq!(encode_extended(&k("C-Enter"), false, false), Some(b"\x1b[27;5;13~".to_vec()));
        assert_eq!(encode_extended(&k("C-1"), false, true), Some(b"\x1b[49;5u".to_vec()));
        assert_eq!(encode_extended(&k("C-a"), false, true), None);
        assert_eq!(encode_extended(&k("C-a"), true, true), Some(b"\x1b[97;5u".to_vec()));
        assert_eq!(encode_extended(&k("C-Up"), true, true), None);
        assert_eq!(encode_extended(&k("Enter"), true, true), None);
        // what we send parses back to the same key
        for name in ["C-Enter", "C-Tab", "C-1", "C-S-a", "M-C-Enter"] {
            let bytes = encode_extended(&k(name), true, true).unwrap();
            let mut p = Parser::new();
            match p.feed(&bytes).first() {
                Some(Input::Key(key, _)) => assert_eq!(key_name(key), key_name(&k(name)), "{name}"),
                other => panic!("{name}: {other:?}"),
            }
        }
    }

    #[test]
    fn csi_u_and_modify_other_keys() {
        assert_eq!(key1(b"\x1b[97;5u"), ctrl('a'));
        assert_eq!(key1(b"\x1b[97;3u"), meta(KeyCode::Char('a')));
        assert_eq!(key1(b"\x1b[97;2u"), ch('A'));
        assert_eq!(
            key1(b"\x1b[97;6u"),
            km(KeyCode::Char('a'), true, false, true)
        );
        assert_eq!(key1(b"\x1b[13;2u"), km(KeyCode::Enter, false, false, true));
        assert_eq!(key1(b"\x1b[9;2u"), k(KeyCode::BTab));
        assert_eq!(
            key1(b"\x1b[127;5u"),
            km(KeyCode::BSpace, true, false, false)
        );
        assert_eq!(key1(b"\x1b[32;5u"), km(KeyCode::Space, true, false, false));
        assert_eq!(key1(b"\x1b[27u"), k(KeyCode::Escape));
        assert_eq!(key1(b"\x1b[97u"), ch('a'));
        assert_eq!(key1(b"\x1b[97:65;5u"), ctrl('a'));
        assert_eq!(key1(b"\x1b[105;5u"), k(KeyCode::Tab)); // C-i == Tab
        assert_eq!(key1(b"\x1b[27;5;97~"), ctrl('a'));
        assert_eq!(key1(b"\x1b[27;5;49~"), ctrl('1'));
        assert_eq!(key1(b"\x1b[27;3;44~"), meta(KeyCode::Char(',')));
        assert_eq!(key1(b"\x1b[44032;3u"), meta(KeyCode::Char('가')));
    }

    fn win32(vk: u32, uc: u32, kd: u32, cs: u32) -> Vec<u8> {
        format!("\x1b[{vk};0;{uc};{kd};{cs};1_").into_bytes()
    }

    #[test]
    fn win32_input_mode() {
        assert_eq!(key1(&win32(0x41, 'a' as u32, 1, 0)), ch('a'));
        assert_eq!(key1(&win32(0x41, 'A' as u32, 1, 0x10)), ch('A'));
        assert_eq!(key1(&win32(0x41, 1, 1, 0x8)), ctrl('a'));
        assert_eq!(key1(&win32(0x42, 2, 1, 0x4)), ctrl('b'));
        assert_eq!(
            key1(&win32(0x58, 'x' as u32, 1, 0x2)),
            meta(KeyCode::Char('x'))
        );
        assert_eq!(
            key1(&win32(0x58, 0x18, 1, 0xa)),
            km(KeyCode::Char('x'), true, true, false)
        );
        // AltGr (RightAlt + LeftCtrl) producing '@'.
        assert_eq!(key1(&win32(0x51, '@' as u32, 1, 0x9)), ch('@'));
        assert_eq!(key1(&win32(0x26, 0, 1, 0)), k(KeyCode::Up));
        assert_eq!(
            key1(&win32(0x26, 0, 1, 0x8)),
            km(KeyCode::Up, true, false, false)
        );
        assert_eq!(
            key1(&win32(0x25, 0, 1, 0x10)),
            km(KeyCode::Left, false, false, true)
        );
        assert_eq!(key1(&win32(0x0d, 13, 1, 0)), k(KeyCode::Enter));
        assert_eq!(key1(&win32(0x08, 8, 1, 0)), k(KeyCode::BSpace));
        assert_eq!(key1(&win32(0x09, 9, 1, 0x10)), k(KeyCode::BTab));
        assert_eq!(key1(&win32(0x1b, 27, 1, 0)), k(KeyCode::Escape));
        assert_eq!(
            key1(&win32(0x20, 0, 1, 0x8)),
            km(KeyCode::Space, true, false, false)
        );
        assert_eq!(key1(&win32(0x70, 0, 1, 0)), k(KeyCode::F(1)));
        assert_eq!(key1(&win32(0x7b, 0, 1, 0)), k(KeyCode::F(12)));
        assert_eq!(key1(&win32(0x22, 0, 1, 0)), k(KeyCode::PageDown));
        assert_eq!(key1(&win32(0x32, 0, 1, 0x8)), ctrl('2'));
        assert_eq!(key1(&win32(0, 0xAC00, 1, 0)), ch('가'));
        // Key up and modifier-only presses are Unknown.
        let up = win32(0x41, 'a' as u32, 0, 0);
        assert_eq!(all(&up), vec![Input::Unknown(up.clone())]);
        let shift = win32(0x10, 0, 1, 0x10);
        assert_eq!(all(&shift), vec![Input::Unknown(shift.clone())]);
    }

    #[test]
    fn split_sequences() {
        let seqs: Vec<(&[u8], Key)> = vec![
            (b"\x1b[1;5A", km(KeyCode::Up, true, false, false)),
            (b"\x1b[24;2~", km(KeyCode::F(12), false, false, true)),
            (b"\x1bOP", k(KeyCode::F(1))),
            (b"\x1b\x1b[D", meta(KeyCode::Left)),
            (b"\x1b[97;5u", ctrl('a')),
        ];
        for (seq, want) in seqs {
            for split in 1..seq.len() {
                let mut p = Parser::new();
                let mut v = p.feed(&seq[..split]);
                assert!(v.is_empty(), "{seq:?} split {split}: {v:?}");
                v.extend(p.feed(&seq[split..]));
                assert_eq!(
                    v,
                    vec![Input::Key(want.clone(), seq.to_vec())],
                    "split {split}"
                );
                assert!(!p.has_pending());
            }
            // Byte by byte.
            let mut p = Parser::new();
            let mut v = Vec::new();
            for b in seq {
                v.extend(p.feed(&[*b]));
            }
            assert_eq!(v, vec![Input::Key(want.clone(), seq.to_vec())]);
        }
    }

    #[test]
    fn mixed_stream() {
        let v = keys(b"ab\x1b[A\x02c\x1bOB\r");
        assert_eq!(
            v,
            vec![
                ch('a'),
                ch('b'),
                k(KeyCode::Up),
                ctrl('b'),
                ch('c'),
                k(KeyCode::Down),
                k(KeyCode::Enter)
            ]
        );
    }

    #[test]
    fn bracketed_paste() {
        let v = all(b"x\x1b[200~hello\x1b[Aworld\x1b[201~y");
        assert_eq!(
            v,
            vec![
                Input::Key(ch('x'), b"x".to_vec()),
                Input::Paste(b"hello\x1b[Aworld".to_vec()),
                Input::Key(ch('y'), b"y".to_vec()),
            ]
        );
        assert_eq!(all(b"\x1b[200~\x1b[201~"), vec![Input::Paste(Vec::new())]);
    }

    #[test]
    fn oversized_paste_is_discarded_until_end_without_becoming_keystrokes() {
        let mut p = Parser::new();
        assert!(p.feed(b"\x1b[200~").is_empty());
        let chunk = vec![b'x'; 1 << 20];
        for _ in 0..6 {
            assert!(p.feed(&chunk).is_empty());
            assert!(p.paste.len() <= MAX_PASTE);
        }
        assert_eq!(p.feed(b"echo forbidden\r\x1b[201~z"), vec![Input::Key(ch('z'), b"z".to_vec())]);
        assert!(!p.in_paste());
    }

    #[test]
    fn bracketed_paste_across_chunks() {
        let data: &[u8] = "\x1b[200~안녕 paste\x1b\x1b[201\x1b[201~z".as_bytes();
        let want_paste = "안녕 paste\x1b\x1b[201".as_bytes().to_vec();
        for split in 1..data.len() {
            let mut p = Parser::new();
            let mut v = p.feed(&data[..split]);
            v.extend(p.feed(&data[split..]));
            assert_eq!(
                v,
                vec![
                    Input::Paste(want_paste.clone()),
                    Input::Key(ch('z'), b"z".to_vec())
                ],
                "split {split}"
            );
        }
        // Byte by byte, with flushes in the middle of the paste (no effect).
        let mut p = Parser::new();
        let mut v = Vec::new();
        for (i, b) in data.iter().enumerate() {
            v.extend(p.feed(&[*b]));
            if i > 6 && i < data.len() - 3 {
                assert!(p.in_paste());
                assert!(!p.has_pending());
                assert!(p.flush_pending().is_empty());
            }
        }
        assert_eq!(
            v,
            vec![Input::Paste(want_paste), Input::Key(ch('z'), b"z".to_vec())]
        );
    }

    #[test]
    fn stray_paste_end_is_unknown() {
        assert_eq!(
            all(b"\x1b[201~"),
            vec![Input::Unknown(b"\x1b[201~".to_vec())]
        );
    }

    fn mouse(kind: MouseKind, button: u8, x: u16, y: u16) -> Mouse {
        Mouse {
            kind,
            button,
            x,
            y,
            mods: Mods::NONE,
        }
    }
    fn mouse1(data: &[u8]) -> Mouse {
        let v = all(data);
        assert_eq!(v.len(), 1, "{v:?}");
        match &v[0] {
            Input::Mouse(m, raw) => {
                assert_eq!(raw, data);
                *m
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sgr_mouse_parse() {
        assert_eq!(mouse1(b"\x1b[<0;1;1M"), mouse(MouseKind::Down, 1, 0, 0));
        assert_eq!(mouse1(b"\x1b[<0;10;5m"), mouse(MouseKind::Up, 1, 9, 4));
        assert_eq!(mouse1(b"\x1b[<1;3;4M"), mouse(MouseKind::Down, 2, 2, 3));
        assert_eq!(
            mouse1(b"\x1b[<2;300;200M"),
            mouse(MouseKind::Down, 3, 299, 199)
        );
        assert_eq!(mouse1(b"\x1b[<32;5;6M"), mouse(MouseKind::Drag, 1, 4, 5));
        assert_eq!(mouse1(b"\x1b[<34;5;6M"), mouse(MouseKind::Drag, 3, 4, 5));
        assert_eq!(mouse1(b"\x1b[<35;5;6M"), mouse(MouseKind::Move, 0, 4, 5));
        assert_eq!(mouse1(b"\x1b[<64;5;6M"), mouse(MouseKind::WheelUp, 0, 4, 5));
        assert_eq!(
            mouse1(b"\x1b[<65;5;6M"),
            mouse(MouseKind::WheelDown, 0, 4, 5)
        );
        let m = mouse1(b"\x1b[<28;1;1M"); // 4+8+16 = shift+meta+ctrl, button 1
        assert_eq!(
            m.mods,
            Mods {
                ctrl: true,
                meta: true,
                shift: true
            }
        );
        assert_eq!(m.kind, MouseKind::Down);
        let m = mouse1(b"\x1b[<10;1;1M"); // meta + right
        assert_eq!((m.button, m.mods), (3, Mods::META));
        // Horizontal wheel → Unknown.
        assert_eq!(
            all(b"\x1b[<66;1;1M"),
            vec![Input::Unknown(b"\x1b[<66;1;1M".to_vec())]
        );
        assert_eq!(
            all(b"\x1b[<0;1M"),
            vec![Input::Unknown(b"\x1b[<0;1M".to_vec())]
        );
    }

    #[test]
    fn sgr_mouse_split() {
        let seq = b"\x1b[<0;120;45M";
        for split in 1..seq.len() {
            let mut p = Parser::new();
            let mut v = p.feed(&seq[..split]);
            v.extend(p.feed(&seq[split..]));
            assert_eq!(
                v,
                vec![Input::Mouse(
                    mouse(MouseKind::Down, 1, 119, 44),
                    seq.to_vec()
                )]
            );
        }
    }

    #[test]
    fn x10_mouse_parse() {
        assert_eq!(
            mouse1(&[0x1b, b'[', b'M', 32, 33, 33]),
            mouse(MouseKind::Down, 1, 0, 0)
        );
        assert_eq!(
            mouse1(&[0x1b, b'[', b'M', 35, 43, 38]),
            mouse(MouseKind::Up, 0, 10, 5)
        );
        assert_eq!(
            mouse1(&[0x1b, b'[', b'M', 64, 43, 38]),
            mouse(MouseKind::Drag, 1, 10, 5)
        );
        assert_eq!(
            mouse1(&[0x1b, b'[', b'M', 96, 43, 38]),
            mouse(MouseKind::WheelUp, 0, 10, 5)
        );
        assert_eq!(
            mouse1(&[0x1b, b'[', b'M', 97, 255, 255]),
            mouse(MouseKind::WheelDown, 0, 222, 222)
        );
        // Split across feeds.
        let seq = [0x1b, b'[', b'M', 34, 50, 60];
        let mut p = Parser::new();
        assert!(p.feed(&seq[..4]).is_empty());
        assert_eq!(
            p.feed(&seq[4..]),
            vec![Input::Mouse(
                mouse(MouseKind::Down, 3, 17, 27),
                seq.to_vec()
            )]
        );
    }

    #[test]
    fn mouse_encode_sgr() {
        let m = mouse(MouseKind::Down, 1, 4, 9);
        assert_eq!(
            encode_mouse(&m, P::PressRelease, true).unwrap(),
            b"\x1b[<0;5;10M"
        );
        let m = mouse(MouseKind::Up, 3, 4, 9);
        assert_eq!(
            encode_mouse(&m, P::PressRelease, true).unwrap(),
            b"\x1b[<2;5;10m"
        );
        let m = mouse(MouseKind::WheelDown, 0, 0, 0);
        assert_eq!(encode_mouse(&m, P::Press, true).unwrap(), b"\x1b[<65;1;1M");
        let mut m = mouse(MouseKind::Drag, 1, 1, 1);
        m.mods = Mods::CTRL;
        assert_eq!(
            encode_mouse(&m, P::ButtonMotion, true).unwrap(),
            b"\x1b[<48;2;2M"
        );
    }

    #[test]
    fn mouse_encode_legacy() {
        let m = mouse(MouseKind::Down, 1, 4, 9);
        assert_eq!(
            encode_mouse(&m, P::PressRelease, false).unwrap(),
            vec![0x1b, b'[', b'M', 32, 37, 42]
        );
        let m = mouse(MouseKind::Up, 1, 4, 9);
        assert_eq!(
            encode_mouse(&m, P::PressRelease, false).unwrap(),
            vec![0x1b, b'[', b'M', 35, 37, 42]
        );
        let m = mouse(MouseKind::Down, 1, 222, 0);
        assert!(encode_mouse(&m, P::PressRelease, false).is_some());
        let m = mouse(MouseKind::Down, 1, 223, 0);
        assert!(encode_mouse(&m, P::PressRelease, false).is_none());
        assert!(encode_mouse(&m, P::PressRelease, true).is_some());
    }

    #[test]
    fn mouse_encode_mode_filtering() {
        let down = mouse(MouseKind::Down, 1, 0, 0);
        let up = mouse(MouseKind::Up, 1, 0, 0);
        let drag = mouse(MouseKind::Drag, 1, 0, 0);
        let mv = mouse(MouseKind::Move, 0, 0, 0);
        let wheel = mouse(MouseKind::WheelUp, 0, 0, 0);
        let ok = |m: &Mouse, mode| encode_mouse(m, mode, true).is_some();
        for m in [&down, &up, &drag, &mv, &wheel] {
            assert!(!ok(m, P::None));
        }
        assert!(ok(&down, P::Press) && ok(&wheel, P::Press));
        assert!(!ok(&up, P::Press) && !ok(&drag, P::Press) && !ok(&mv, P::Press));
        assert!(
            ok(&up, P::PressRelease) && !ok(&drag, P::PressRelease) && !ok(&mv, P::PressRelease)
        );
        assert!(ok(&drag, P::ButtonMotion) && !ok(&mv, P::ButtonMotion));
        assert!(ok(&drag, P::AnyMotion) && ok(&mv, P::AnyMotion) && ok(&up, P::AnyMotion));
        // X10 (Press) mode strips modifiers.
        let mut m = down;
        m.mods = Mods::SHIFT;
        assert_eq!(encode_mouse(&m, P::Press, true).unwrap(), b"\x1b[<0;1;1M");
        assert_eq!(
            encode_mouse(&m, P::PressRelease, true).unwrap(),
            b"\x1b[<4;1;1M"
        );
    }

    #[test]
    fn mouse_round_trip() {
        let kinds = [
            (MouseKind::Down, 1),
            (MouseKind::Down, 2),
            (MouseKind::Down, 3),
            (MouseKind::Up, 1),
            (MouseKind::Up, 3),
            (MouseKind::Drag, 1),
            (MouseKind::Drag, 2),
            (MouseKind::Move, 0),
            (MouseKind::WheelUp, 0),
            (MouseKind::WheelDown, 0),
        ];
        let mods = [
            Mods::NONE,
            Mods::CTRL,
            Mods::META,
            Mods::SHIFT,
            Mods {
                ctrl: true,
                meta: true,
                shift: true,
            },
        ];
        for (kind, button) in kinds {
            for m in mods {
                for (x, y) in [(0, 0), (79, 23), (150, 100), (500, 300)] {
                    let ev = Mouse {
                        kind,
                        button,
                        x,
                        y,
                        mods: m,
                    };
                    for sgr in [true, false] {
                        let Some(bytes) = encode_mouse(&ev, P::AnyMotion, sgr) else {
                            assert!(!sgr && x > 222);
                            continue;
                        };
                        let mut want = ev;
                        if !sgr && kind == MouseKind::Up {
                            want.button = 0; // legacy release loses the button
                        }
                        assert_eq!(mouse1(&bytes), want, "sgr={sgr} {bytes:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn mouse_names() {
        let mut m = mouse(MouseKind::Down, 1, 0, 0);
        assert_eq!(mouse_key_name(&m, "Pane"), "MouseDown1Pane");
        assert_eq!(mouse_key_name(&m, "StatusLeft"), "MouseDown1StatusLeft");
        m.kind = MouseKind::Up;
        assert_eq!(mouse_key_name(&m, "Status"), "MouseUp1Status");
        m.kind = MouseKind::Drag;
        assert_eq!(mouse_key_name(&m, "Border"), "MouseDrag1Border");
        assert_eq!(drag_end_key_name(&m, "Pane"), "MouseDragEnd1Pane");
        m.kind = MouseKind::WheelUp;
        assert_eq!(mouse_key_name(&m, "Pane"), "WheelUpPane");
        m.kind = MouseKind::WheelDown;
        assert_eq!(mouse_key_name(&m, "Status"), "WheelDownStatus");
        m.kind = MouseKind::Move;
        assert_eq!(mouse_key_name(&m, "Pane"), "MouseMovePane");
        let mut m = mouse(MouseKind::Down, 3, 0, 0);
        m.mods = Mods::META;
        assert_eq!(mouse_key_name(&m, "Pane"), "M-MouseDown3Pane");
        let m = mouse(MouseKind::Down, 1, 0, 0);
        assert_eq!(click_key_name(2, &m, "Pane"), "DoubleClick1Pane");
        assert_eq!(click_key_name(3, &m, "Pane"), "TripleClick1Pane");
        assert_eq!(click_key_name(1, &m, "Pane"), "MouseDown1Pane");
    }

    #[test]
    fn key_name_parsing() {
        let p = |s| parse_key_name(s).unwrap_or_else(|| panic!("{s}"));
        assert_eq!(p("C-b"), ctrl('b'));
        assert_eq!(p("C-B"), ctrl('b'));
        assert_eq!(p("^B"), ctrl('b'));
        assert_eq!(p("^b"), ctrl('b'));
        assert_eq!(p("c-b"), ctrl('b'));
        assert_eq!(p("M-Left"), meta(KeyCode::Left));
        assert_eq!(p("S-Up"), km(KeyCode::Up, false, false, true));
        assert_eq!(p("C-M-x"), km(KeyCode::Char('x'), true, true, false));
        assert_eq!(p("M-C-x"), km(KeyCode::Char('x'), true, true, false));
        assert_eq!(p("C-M-S-Up"), km(KeyCode::Up, true, true, true));
        assert_eq!(p("F12"), k(KeyCode::F(12)));
        assert_eq!(p("f1"), k(KeyCode::F(1)));
        assert_eq!(p("PageUp"), k(KeyCode::PageUp));
        assert_eq!(p("PPage"), k(KeyCode::PageUp));
        assert_eq!(p("PgUp"), k(KeyCode::PageUp));
        assert_eq!(p("NPage"), k(KeyCode::PageDown));
        assert_eq!(p("PageDown"), k(KeyCode::PageDown));
        assert_eq!(p("BSpace"), k(KeyCode::BSpace));
        assert_eq!(p("Enter"), k(KeyCode::Enter));
        assert_eq!(p("enter"), k(KeyCode::Enter));
        assert_eq!(p("Escape"), k(KeyCode::Escape));
        assert_eq!(p("Space"), k(KeyCode::Space));
        assert_eq!(p(" "), k(KeyCode::Space));
        assert_eq!(p("Tab"), k(KeyCode::Tab));
        assert_eq!(p("BTab"), k(KeyCode::BTab));
        assert_eq!(p("IC"), k(KeyCode::Insert));
        assert_eq!(p("DC"), k(KeyCode::Delete));
        assert_eq!(p("Home"), k(KeyCode::Home));
        assert_eq!(p("End"), k(KeyCode::End));
        assert_eq!(p("a"), ch('a'));
        assert_eq!(p("A"), ch('A'));
        assert_eq!(p("S-a"), ch('A'));
        assert_eq!(p("%"), ch('%'));
        assert_eq!(p("\""), ch('"'));
        assert_eq!(p("-"), ch('-'));
        assert_eq!(p("^"), ch('^'));
        assert_eq!(p("C--"), ctrl('-'));
        assert_eq!(p("M--"), meta(KeyCode::Char('-')));
        assert_eq!(p("C-^"), ctrl('^'));
        assert_eq!(p("C-\\"), ctrl('\\'));
        assert_eq!(p("가"), ch('가'));
        assert_eq!(p("M-가"), meta(KeyCode::Char('가')));
        assert_eq!(p("C-Space"), km(KeyCode::Space, true, false, false));
        assert_eq!(p("C-@"), km(KeyCode::Space, true, false, false));
        assert_eq!(p("C-i"), k(KeyCode::Tab));
        assert_eq!(p("C-m"), k(KeyCode::Enter));
        assert_eq!(p("C-["), k(KeyCode::Escape));
        assert_eq!(p("^?"), k(KeyCode::BSpace));
        assert_eq!(p("KP0"), k(KeyCode::KP('0')));
        assert_eq!(p("KPEnter"), k(KeyCode::KP('\r')));
        assert_eq!(p("KP/"), k(KeyCode::KP('/')));
        for bad in [
            "",
            "MouseDown1Pane",
            "WheelUpPane",
            "DoubleClick1Pane",
            "Any",
            "F13",
            "F0",
            "Foo",
            "C-",
            "M-",
        ] {
            assert_eq!(parse_key_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn key_name_round_trip() {
        let mut codes: Vec<KeyCode> = vec![
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::BTab,
            KeyCode::Escape,
            KeyCode::BSpace,
            KeyCode::Space,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Insert,
            KeyCode::Delete,
            KeyCode::KP('\r'),
        ];
        codes.extend((1..=12).map(KeyCode::F));
        codes.extend("0123456789/*-+.,=".chars().map(KeyCode::KP));
        codes.extend(
            "abcxyzABZ0189%\"'-^;:,.!@#$&*()[]{}\\|/?<>~`_=+가é"
                .chars()
                .map(KeyCode::Char),
        );
        let mut n = 0;
        for code in codes {
            for bits in 0..8u8 {
                let mods = Mods {
                    ctrl: bits & 1 != 0,
                    meta: bits & 2 != 0,
                    shift: bits & 4 != 0,
                };
                let key = normalize(Key::new(code.clone(), mods));
                let name = key_name(&key);
                assert_eq!(parse_key_name(&name), Some(key.clone()), "name {name:?}");
                n += 1;
            }
        }
        assert!(n > 500);
        assert_eq!(key_name(&ctrl('b')), "C-b");
        assert_eq!(key_name(&meta(KeyCode::Left)), "M-Left");
        assert_eq!(key_name(&km(KeyCode::Up, true, true, true)), "C-M-S-Up");
        assert_eq!(key_name(&k(KeyCode::PageUp)), "PPage");
        assert_eq!(key_name(&k(KeyCode::PageDown)), "NPage");
        assert_eq!(key_name(&k(KeyCode::Insert)), "IC");
        assert_eq!(key_name(&k(KeyCode::Space)), "Space");
        assert_eq!(key_name(&k(KeyCode::F(5))), "F5");
        assert_eq!(key_name(&ch('"')), "\"");
        assert_eq!(key_name(&k(KeyCode::KP('\r'))), "KPEnter");
    }

    #[test]
    fn parsed_keys_round_trip_names() {
        // Every key the parser produces should have a name that parses back to it.
        let inputs: &[&[u8]] = &[
            b"\x00",
            b"\x01",
            b"\x08",
            b"\x0a",
            b"\x1c",
            b"\x1f",
            b"\x7f",
            b"\x1b[1;8A",
            b"\x1b[6;3~",
            b"\x1b[Z",
            b"\x1bx",
            b"\x1b\x02",
            b"\x1bOp",
            b"\x1b[1;6P",
        ];
        for i in inputs {
            let key = key1(i);
            assert_eq!(parse_key_name(&key_name(&key)), Some(key.clone()), "{i:?}");
        }
    }

    #[test]
    fn encode_basic() {
        let e = |s: &str| encode_key(&parse_key_name(s).unwrap(), false);
        assert_eq!(e("a"), b"a");
        assert_eq!(e("A"), b"A");
        assert_eq!(e("가"), "가".as_bytes());
        assert_eq!(e("C-a"), b"\x01");
        assert_eq!(e("C-b"), b"\x02");
        assert_eq!(e("C-z"), b"\x1a");
        assert_eq!(e("C-S-a"), b"\x01");
        assert_eq!(e("C-Space"), b"\x00");
        assert_eq!(e("C-\\"), b"\x1c");
        assert_eq!(e("C-]"), b"\x1d");
        assert_eq!(e("C-^"), b"\x1e");
        assert_eq!(e("C-_"), b"\x1f");
        assert_eq!(e("C-1"), b"1"); // no control code: ctrl dropped
        assert_eq!(e("M-a"), b"\x1ba");
        assert_eq!(e("M-A"), b"\x1bA");
        assert_eq!(e("C-M-a"), b"\x1b\x01");
        assert_eq!(e("M-가"), "\x1b가".as_bytes());
        assert_eq!(e("Enter"), b"\r");
        assert_eq!(e("M-Enter"), b"\x1b\r");
        assert_eq!(e("Tab"), b"\t");
        assert_eq!(e("BTab"), b"\x1b[Z");
        assert_eq!(e("Escape"), b"\x1b");
        assert_eq!(e("M-Escape"), b"\x1b\x1b");
        assert_eq!(e("BSpace"), b"\x7f");
        assert_eq!(e("C-BSpace"), b"\x08");
        assert_eq!(e("M-BSpace"), b"\x1b\x7f");
        assert_eq!(e("Space"), b" ");
        assert_eq!(e("IC"), b"\x1b[2~");
        assert_eq!(e("DC"), b"\x1b[3~");
        assert_eq!(e("PPage"), b"\x1b[5~");
        assert_eq!(e("NPage"), b"\x1b[6~");
        assert_eq!(e("C-NPage"), b"\x1b[6;5~");
        assert_eq!(e("F1"), b"\x1bOP");
        assert_eq!(e("F4"), b"\x1bOS");
        assert_eq!(e("F5"), b"\x1b[15~");
        assert_eq!(e("F12"), b"\x1b[24~");
        assert_eq!(e("S-F1"), b"\x1b[1;2P");
        assert_eq!(e("C-F5"), b"\x1b[15;5~");
        assert_eq!(e("KP5"), b"5");
        assert_eq!(e("KPEnter"), b"\r");
        assert_eq!(e("C-i"), b"\t");
    }

    #[test]
    fn encode_cursor_modes() {
        let e = |s: &str, app| encode_key(&parse_key_name(s).unwrap(), app);
        for (name, f) in [
            ("Up", 'A'),
            ("Down", 'B'),
            ("Right", 'C'),
            ("Left", 'D'),
            ("Home", 'H'),
            ("End", 'F'),
        ] {
            assert_eq!(e(name, false), format!("\x1b[{f}").as_bytes());
            assert_eq!(e(name, true), format!("\x1bO{f}").as_bytes());
            // Modified keys always use the CSI 1;N form.
            for (pre, n) in [
                ("S-", 2),
                ("M-", 3),
                ("S-M-", 4),
                ("C-", 5),
                ("C-S-", 6),
                ("C-M-", 7),
                ("C-M-S-", 8),
            ] {
                let want = format!("\x1b[1;{n}{f}");
                assert_eq!(e(&format!("{pre}{name}"), false), want.as_bytes());
                assert_eq!(e(&format!("{pre}{name}"), true), want.as_bytes());
            }
        }
    }

    #[test]
    fn encode_keypad_modes() {
        let kp = |c| k(KeyCode::KP(c));
        assert_eq!(encode_key_mode(&kp('0'), false, true), b"\x1bOp");
        assert_eq!(encode_key_mode(&kp('9'), false, true), b"\x1bOy");
        assert_eq!(encode_key_mode(&kp('\r'), false, true), b"\x1bOM");
        assert_eq!(encode_key_mode(&kp('/'), false, true), b"\x1bOo");
        assert_eq!(encode_key_mode(&kp('/'), false, false), b"/");
    }

    #[test]
    fn encode_then_parse_round_trip() {
        // Keys that have an unambiguous encoding should parse back to themselves.
        let names = [
            "a",
            "Z",
            "가",
            "C-a",
            "C-z",
            "C-Space",
            "C-\\",
            "C-]",
            "C-^",
            "C-_",
            "M-a",
            "M-Z",
            "C-M-b",
            "Enter",
            "M-Enter",
            "Tab",
            "BTab",
            "BSpace",
            "M-BSpace",
            "Space",
            "M-Space",
            "Up",
            "Down",
            "Left",
            "Right",
            "Home",
            "End",
            "IC",
            "DC",
            "PPage",
            "NPage",
            "S-Up",
            "C-Left",
            "M-Right",
            "C-M-S-Down",
            "C-Home",
            "S-End",
            "C-PPage",
            "M-DC",
            "F1",
            "F2",
            "F3",
            "F4",
            "F5",
            "F6",
            "F7",
            "F8",
            "F9",
            "F10",
            "F11",
            "F12",
            "S-F3",
            "C-F8",
            "C-M-F12",
            "M-Up",
            "M-F1",
        ];
        for app in [false, true] {
            for n in names {
                let key = parse_key_name(n).unwrap();
                let bytes = encode_key(&key, app);
                assert_eq!(key1(&bytes), key, "{n} app={app} {bytes:?}");
            }
        }
    }
}


#[cfg(test)]
mod terminal_reply_tests {
    use super::*;

    #[test]
    fn query_delay_does_not_capture_a_prompt_cancel_escape() {
        let mut parser = Parser::new();
        parser.feed(b"\x1b");
        assert!(!parser.theme_reply_pending());
        parser.feed(b"]11;rgb:ff");
        assert!(parser.theme_reply_pending());
        let replies = parser.feed(b"/ff/ff\x1b\\");
        assert_eq!(replies, vec![Input::Unknown(b"\x1b]11;rgb:ff/ff/ff\x1b\\".to_vec())]);
        assert!(!parser.theme_reply_pending());
    }

    #[test]
    fn colour_replies_end_at_their_first_terminator() {
        let mut parser = Parser::new();
        let got = parser.feed(b"A\x1b]11;rgb:ffff/ffff/ffff\x1b\\B\x1b]11;rgba:0/0/0/f\x1b\\C\x1b]10;rgb:fff/fff/fff\x07D");
        let unknown: Vec<_> = got.iter().filter_map(|i| match i { Input::Unknown(r) => Some(r.clone()), _ => None }).collect();
        assert_eq!(unknown, vec![b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\".to_vec(), b"\x1b]11;rgba:0/0/0/f\x1b\\".to_vec(), b"\x1b]10;rgb:fff/fff/fff\x07".to_vec()]);
        assert_eq!(got.iter().filter(|i| matches!(i, Input::Key(..))).count(), 4); // A B C D
        // no terminator within 127 bytes: not a colour reply
        let mut long = b"\x1b]11;".to_vec();
        long.extend(std::iter::repeat_n(b'x', 130));
        assert!(!matches!(parser.feed(&long).first(), Some(Input::Unknown(_))));
    }
}
