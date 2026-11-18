//! vt100-diff: runs identical input through two copies of vt100 (`vt100_base`,
//! the base commit, and `vt100`, the working tree) and compares everything the
//! application can observe: the screen, every history row (cells through the
//! public getters, wrapped, flags, time, used length, metadata, links), modes,
//! the formatted output, and every callback with the cursor at the time of the
//! call. See README.md; run it through run.sh.

use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::panic::AssertUnwindSafe;
use std::rc::Rc;

// ---------------------------------------------------------------- test cases

#[derive(Clone, Debug)]
enum Op {
    /// Feed `data[start..end]`.
    Process(usize, usize),
    /// Resize; a zero column count keeps the current width.
    SetSize(u16, u16),
    SetTime(u64),
    ClearScrollback,
    /// Write the full state into the log.
    Dump,
}

/// codepoint-widths ranges (first, last, width) and variation-selector-always-wide.
type Widths = (Vec<(u32, u32, u16)>, bool);

#[derive(Clone, Debug)]
struct Case {
    rows: u16,
    cols: u16,
    hist: usize,
    alt_allowed: bool,
    scroll_on_clear: bool,
    widths: Option<Widths>,
    input_limit: Option<usize>,
    /// Log cursor, size and history length after every operation.
    probe: bool,
    data: Vec<u8>,
    ops: Vec<Op>,
}

// ---------------------------------------------------------------- log

/// Everything one side observed. The text is kept up to `cap` bytes for
/// reporting; the hash and length cover all of it.
struct Log {
    text: String,
    cap: usize,
    hash: u64,
    len: u64,
    /// Hyperlink identities (a process-wide counter in vt100) renumbered in
    /// order of first appearance, so the two copies' counters can differ.
    links: std::collections::HashMap<u64, usize>,
}

impl Log {
    fn new(cap: usize) -> Self {
        Log { text: String::new(), cap, hash: 0xcbf2_9ce4_8422_2325, len: 0, links: Default::default() }
    }
    fn link(&mut self, inner: u64) -> usize {
        let n = self.links.len();
        *self.links.entry(inner).or_insert(n)
    }
    fn same(&self, o: &Log) -> bool {
        self.hash == o.hash && self.len == o.len && self.text == o.text
    }
}

impl std::fmt::Write for Log {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let mut h = self.hash;
        for &b in s.as_bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
        }
        self.hash = h;
        self.len += s.len() as u64;
        if self.text.len() < self.cap {
            let room = self.cap - self.text.len();
            let mut n = s.len().min(room);
            while !s.is_char_boundary(n) {
                n -= 1;
            }
            self.text.push_str(&s[..n]);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- panics

#[derive(Clone, Debug, PartialEq)]
struct PanicAt {
    op: usize,
    msg: String,
    file: String,
    line: u32,
}

thread_local! {
    static LAST_PANIC: RefCell<Option<(String, String, u32)>> = const { RefCell::new(None) };
    static CUR_OP: Cell<usize> = const { Cell::new(0) };
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let p = info.payload();
        let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
        let (file, line) = info.location().map_or((String::new(), 0), |l| (l.file().to_string(), l.line()));
        LAST_PANIC.with(|c| *c.borrow_mut() = Some((msg, file, line)));
    }));
}

/// The file a panic came from, relative to the crate (the two copies live in
/// different directories).
fn short_file(f: &str) -> &str {
    f.rfind("/src/").or_else(|| f.rfind("\\src\\")).map_or(f, |i| &f[i + 1..])
}

struct Outcome {
    log: Log,
    panic: Option<PanicAt>,
}

fn nz(n: u16) -> core::num::NonZeroU16 {
    core::num::NonZeroU16::new(n.max(1)).unwrap()
}

// ---------------------------------------------------------------- one side
//
// The same code is compiled against both crates; only public API is used, so
// internal representation changes (for example stale bytes in a reused cell)
// do not count as differences, but anything the application can read does.

macro_rules! side {
    ($name:ident, $vt:ident) => {
        mod $name {
            use super::*;
            use ::$vt as vt;

            pub struct Rec(pub Rc<RefCell<Log>>);

            impl Rec {
                fn w(&self, s: &vt::Screen, args: std::fmt::Arguments<'_>) {
                    let mut l = self.0.borrow_mut();
                    let _ = l.write_fmt(args);
                    let _ = writeln!(l, " @{:?}", s.cursor_position());
                }
            }

            impl vt::Callbacks for Rec {
                fn dcs(&mut self, s: &mut vt::Screen, p: &[u8]) { self.w(s, format_args!("cb dcs {p:?}")) }
                fn audible_bell(&mut self, s: &mut vt::Screen) { self.w(s, format_args!("cb bell")) }
                fn visual_bell(&mut self, s: &mut vt::Screen) { self.w(s, format_args!("cb vbell")) }
                fn resize(&mut self, s: &mut vt::Screen, r: (u16, u16)) { self.w(s, format_args!("cb resize {r:?}")) }
                fn set_window_icon_name(&mut self, s: &mut vt::Screen, p: &[u8]) { self.w(s, format_args!("cb icon {p:?}")) }
                fn set_window_title(&mut self, s: &mut vt::Screen, p: &[u8]) { self.w(s, format_args!("cb title {p:?}")) }
                fn reset(&mut self, s: &mut vt::Screen) { self.w(s, format_args!("cb reset")) }
                fn rename_window(&mut self, s: &mut vt::Screen, p: &[u8]) { self.w(s, format_args!("cb rename {p:?}")) }
                fn copy_to_clipboard(&mut self, s: &mut vt::Screen, t: &[u8], d: &[u8]) { self.w(s, format_args!("cb clip {t:?} {d:?}")) }
                fn clipboard_query(&mut self, s: &mut vt::Screen, t: &[u8], b: bool) { self.w(s, format_args!("cb clipq {t:?} {b}")) }
                fn unhandled_char(&mut self, s: &mut vt::Screen, c: char) { self.w(s, format_args!("cb uchar {c:?}")) }
                fn unhandled_control(&mut self, s: &mut vt::Screen, b: u8) { self.w(s, format_args!("cb uctl {b}")) }
                fn unhandled_escape(&mut self, s: &mut vt::Screen, a: Option<u8>, b: Option<u8>, c: u8) { self.w(s, format_args!("cb uesc {a:?} {b:?} {c}")) }
                fn unhandled_csi(&mut self, s: &mut vt::Screen, a: Option<u8>, b: Option<u8>, p: &[&[u16]], c: char) {
                    self.w(s, format_args!("cb ucsi {a:?} {b:?} {p:?} {c:?}"))
                }
                fn theme_subscription(&mut self, s: &mut vt::Screen) { self.w(s, format_args!("cb theme")) }
                fn unhandled_osc_terminated(&mut self, s: &mut vt::Screen, p: &[&[u8]], b: bool) { self.w(s, format_args!("cb osc {p:?} {b}")) }
                fn on_scroll(&mut self, c: vt::capture::RowContents<'_>, alt: bool) {
                    let mut l = self.0.borrow_mut();
                    let _ = write!(l, "cb scroll {alt} ");
                    row(&mut l, &c);
                    l.write_char('\n').ok();
                }
            }

            fn cell(o: &mut Log, c: &vt::Cell) {
                let t = c.contents();
                if !t.is_empty() && t.bytes().all(|b| (0x21..0x7f).contains(&b) && b != b'|' && b != b'"' && b != b'\\') {
                    let _ = o.write_str(t);
                } else {
                    let _ = write!(o, "{t:?}");
                }
                let bits = u32::from(c.has_contents())
                    | u32::from(c.is_wide()) << 1
                    | u32::from(c.is_wide_continuation()) << 2
                    | u32::from(c.bold()) << 3
                    | u32::from(c.dim()) << 4
                    | u32::from(c.italic()) << 5
                    | u32::from(c.underline()) << 6
                    | u32::from(c.inverse()) << 7
                    | u32::from(c.blink()) << 8
                    | u32::from(c.hidden()) << 9
                    | u32::from(c.strikethrough()) << 10
                    | u32::from(c.overline()) << 11
                    | u32::from(c.charset()) << 12
                    | u32::from(c.underline_style()) << 16;
                let d = vt::Color::Default;
                let (fg, bg, ul) = (c.fgcolor(), c.bgcolor(), c.underline_color());
                if bits != 1 || fg != d || bg != d || ul != d {
                    let _ = write!(o, "\u{b7}{bits:x}");
                    if fg != d { let _ = write!(o, "f{fg:?}"); }
                    if bg != d { let _ = write!(o, "b{bg:?}"); }
                    if ul != d { let _ = write!(o, "u{ul:?}"); }
                }
                let _ = o.write_char('|');
            }

            /// An empty cell with default attributes. The application reads a
            /// missing cell (past a row's stored width) as one of these, so
            /// trailing blanks are left out and stored width does not count.
            fn blank(c: &vt::Cell) -> bool {
                let d = vt::Color::Default;
                !c.has_contents() && c.contents().is_empty() && !c.is_wide() && !c.is_wide_continuation() && !c.bold() && !c.dim()
                    && !c.italic() && !c.underline() && !c.inverse() && !c.blink() && !c.hidden() && !c.strikethrough()
                    && !c.overline() && !c.charset() && c.underline_style() == 0 && c.fgcolor() == d && c.bgcolor() == d
                    && c.underline_color() == d
            }

            fn row(o: &mut Log, r: &vt::capture::RowContents<'_>) {
                let _ = write!(o, "w{} f{} t{} u{}", u8::from(r.wrapped()), r.flags(), r.time(), r.used_len());
                let md: Vec<_> = r.metadata().collect();
                for (col, m) in &md {
                    let _ = write!(o, " m{col}/{}", m.tab_width);
                    if let Some(h) = &m.hyperlink {
                        let n = o.link(h.inner);
                        let _ = write!(o, "/L{n}{:?}{:?}{:?}", h.id, h.external_id, h.uri);
                    }
                }
                let _ = o.write_char(':');
                let mut end = 0u16;
                let mut c = 0u16;
                while let Some(x) = r.cell(c) {
                    c += 1;
                    if !blank(x) {
                        end = c;
                    }
                }
                for c in 0..end {
                    cell(o, r.cell(c).unwrap());
                    if !md.is_empty() {
                        if let Some(h) = r.hyperlink_at(c) {
                            let n = o.link(h.inner);
                            let _ = write!(o, "@L{n}");
                        }
                    }
                }
            }

            pub fn dump(s: &vt::Screen, o: &mut Log) {
                let (rows, cols) = (s.size().0.get(), s.size().1.get());
                let _ = writeln!(o, "dump op{} size {rows}x{cols} cursor {:?} view {} sb {}/{} base {:?} counters {:?} hist {}",
                    CUR_OP.with(Cell::get), s.cursor_position(), s.scrollback(), s.scrollback_len(), s.scrollback_capacity(), s.base_scrollback(),
                    s.history_counters(), s.history_rows_len());
                let _ = writeln!(o, "modes alt {} kp {} ckeys {} hide {} paste {} mouse {:?} {:?} {:?} style {:?} ins {} origin {} wrap {} blink {:?} vv {} focus {} sync {} theme {} altsave {:?} region {:?} tabs {:?}",
                    s.alternate_screen(), s.application_keypad(), s.application_cursor(), s.hide_cursor(), s.bracketed_paste(),
                    s.mouse_protocol_mode(), s.mouse_protocol_encoding(), s.mouse_encodings(), s.cursor_style(), s.insert_mode(),
                    s.origin_mode(), s.wrap_mode(), s.cursor_blinking(), s.cursor_very_visible(), s.focus_events(),
                    s.synchronized_output(), s.theme_updates(), s.alternate_saved_cursor(), s.scroll_region(), s.tab_stops());
                let _ = writeln!(o, "pen {:?} {:?} {} {} {} {} {} {:?}", s.fgcolor(), s.bgcolor(), s.bold(), s.dim(), s.italic(),
                    s.underline(), s.inverse(), s.attributes_formatted());
                let _ = writeln!(o, "input {:?} cursor {:?}", s.input_mode_formatted(), s.cursor_state_formatted());
                let _ = writeln!(o, "fmt {:?}", s.contents_formatted());
                let _ = writeln!(o, "text {:?}", s.contents());
                for i in 0..s.history_rows_len() {
                    let _ = write!(o, "h{i} {:?} ", s.history_row_flags(i));
                    match s.history_row(i) {
                        Some(r) => row(o, &r),
                        None => { let _ = o.write_str("none"); }
                    }
                    let _ = o.write_char('\n');
                }
                for y in 0..rows {
                    if let Some(r) = s.saved_row(y) {
                        let _ = write!(o, "s{y} ");
                        row(o, &r);
                        let _ = o.write_char('\n');
                    }
                }
                for y in 0..rows {
                    let _ = write!(o, "v{y} {} ", u8::from(s.row_wrapped(y)));
                    let end = (0..cols).rev().find(|&x| s.cell(y, x).is_some_and(|c| !blank(c))).map_or(0, |x| x + 1);
                    for x in 0..end {
                        match s.cell(y, x) {
                            Some(c) => cell(o, c),
                            // read as a blank, like a missing history cell
                            None => { let _ = o.write_str("\"\"\u{b7}0|"); }
                        }
                    }
                    let _ = o.write_char('\n');
                }
            }

            pub fn run(case: &Case, cap: usize) -> Outcome {
                let log = Rc::new(RefCell::new(Log::new(cap)));
                LAST_PANIC.with(|c| *c.borrow_mut() = None);
                let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    let mut p = vt::Parser::new_with_callbacks(nz(case.rows), nz(case.cols), case.hist, Rec(log.clone()));
                    p.screen_mut().set_alternate_screen_allowed(case.alt_allowed);
                    p.screen_mut().set_scroll_on_clear(case.scroll_on_clear);
                    if let Some((w, v)) = &case.widths {
                        p.screen_mut().set_codepoint_widths(w.clone(), *v);
                    }
                    if let Some(n) = case.input_limit {
                        p.set_input_buffer_size(n);
                    }
                    for (i, op) in case.ops.iter().enumerate() {
                        CUR_OP.with(|c| c.set(i));
                        match *op {
                            Op::Process(a, b) => p.process(&case.data[a..b]),
                            Op::SetSize(r, c) => {
                                let c = if c == 0 { p.screen().size().1.get() } else { c };
                                p.set_size(nz(r), nz(c));
                            }
                            Op::SetTime(t) => p.screen_mut().set_time(t),
                            Op::ClearScrollback => p.screen_mut().clear_scrollback(),
                            Op::Dump => dump(p.screen(), &mut log.borrow_mut()),
                        }
                        if case.probe {
                            let s = p.screen();
                            let _ = writeln!(log.borrow_mut(), "op{i} {:?} {:?} {}", s.cursor_position(), s.size(), s.history_rows_len());
                        }
                    }
                }));
                let panic = r.err().map(|_| {
                    let (msg, file, line) = LAST_PANIC.with(|c| c.borrow_mut().take()).unwrap_or_default();
                    PanicAt { op: CUR_OP.with(Cell::get), msg, file, line }
                });
                let log = Rc::try_unwrap(log).ok().map(RefCell::into_inner).unwrap_or_else(|| Log::new(0));
                Outcome { log, panic }
            }

            /// CPU seconds to feed `data` in `chunk`-byte pieces, then for each resize.
            pub fn bench(data: &[u8], rows: u16, cols: u16, hist: usize, chunk: usize, resizes: &[(u16, u16)]) -> Vec<f64> {
                let mut p = vt::Parser::new(nz(rows), nz(cols), hist);
                let mut out = Vec::new();
                let t = cpu();
                for c in data.chunks(chunk) {
                    p.process(c);
                }
                std::hint::black_box(p.screen().cursor_position());
                out.push(cpu() - t);
                for &(r, c) in resizes {
                    let t = cpu();
                    p.set_size(nz(r), nz(c));
                    std::hint::black_box(p.screen().cursor_position());
                    out.push(cpu() - t);
                }
                out
            }
        }
    };
}

side!(base, vt100_base);
side!(work, vt100);

// ---------------------------------------------------------------- timing

#[cfg(target_os = "linux")]
fn cpu() -> f64 {
    #[repr(C)]
    struct Tv { sec: i64, usec: i64 }
    #[repr(C)]
    struct Ru { u: Tv, s: Tv, rest: [i64; 14] }
    unsafe extern "C" {
        fn getrusage(who: i32, r: *mut Ru) -> i32;
    }
    // RUSAGE_THREAD
    let mut r = Ru { u: Tv { sec: 0, usec: 0 }, s: Tv { sec: 0, usec: 0 }, rest: [0; 14] };
    unsafe { getrusage(1, &mut r) };
    r.u.sec as f64 + r.u.usec as f64 / 1e6 + r.s.sec as f64 + r.s.usec as f64 / 1e6
}

#[cfg(not(target_os = "linux"))]
fn cpu() -> f64 {
    use std::sync::OnceLock;
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    T0.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
}

// ---------------------------------------------------------------- generators

struct Rng(u64);
impl Rng {
    fn n(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn r(&mut self, k: u64) -> u64 {
        self.n() % k
    }
}

const ATOMS: &[&[u8]] = &[b"a", b"Z", b" ", b"hello ", b"\r\n", b"\n", b"\r", b"\t", b"\x08", b"\x07", b"\x1b", b"\x1b[", b"[", b"m", b"\x1b[m", b"\x1b[0m", b"\x1b[1;31m",
    b"\x1b[38;2;1;2;3m", b"\x1b[48;5;200m", b"\x1b[4:3m", b"\x1b[K", b"\x1b[2J", b"\x1b[H", b"\x1b[5;10H", b"\x1b[3@", b"\x1b[2P", b"\x1b[4h", b"\x1b[4l", b"\x1b[?7l", b"\x1b[?7h",
    b"\x1b[3;8r", b"\x1b[r", b"\x1bM", b"\x1bD", b"\x1b7", b"\x1b8", b"\x1b(0", b"\x1b(B", b"lqk", b"\x1b]0;title\x07", b"\x1b]2;t\x1b\\", b"\x1b]8;;http://x\x1b\\", b"\x1b]8;;\x1b\\",
    b"\x1b]133;A\x07", b"\x1b]52;c;aGk=\x07", b"\x1b]52;c;?\x07", b"\x1bPtmux;\x1b\x1bx\x1b\\", b"\x1bP$q q\x1b\\", b"\x1b_apc\x1b\\", b"\x1bkname\x1b\\", b"\x1b^pm\x1b\\",
    "한".as_bytes(), "글".as_bytes(), "e\u{301}".as_bytes(), "\u{2764}\u{fe0f}".as_bytes(), "👩\u{200d}💻".as_bytes(), "🇰🇷".as_bytes(), "👍🏽".as_bytes(), "\u{200d}".as_bytes(),
    b"\xe4", b"\xb8", b"\xad", b"\xff", b"\x80", b"\x9b", b"\xc2\x9b", b"\xf0\x9f", b"\x18", b"\x1a", b"\x00", b"\x7f", b"\x1b[?1049h", b"\x1b[?1049l", b"\x1bc", b"\x1b[6n", b"\x1b[c",
    b"\x1b[>4;2m", b"\x1b[?2026h", b"\x1b[?2026l", b"\x1b[3g", b"\x1bH", b"\x1b[1;5r", b"\x1b[S", b"\x1b[T", b"\x1b[2L", b"\x1b[2M", b"\x1b[10X", b"\x1b[b", b"\x1b#8", b"\x1b[?1h"];

/// `fuzz` case: random escapes, broken UTF-8, wide and combining characters,
/// control strings, random chunk boundaries, sizes and history, plus the pane
/// settings the server applies (alternate screen policy, scroll-on-clear,
/// codepoint-widths, input-buffer-size, history time) and occasional resizes
/// and clear-history between chunks.
fn fuzz_case(seed: u64) -> Case {
    let mut g = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut data = Vec::new();
    for _ in 0..g.r(400) + 1 {
        if g.r(4) == 0 {
            for _ in 0..g.r(130) {
                data.push(b'a' + g.r(26) as u8)
            }
        } else {
            data.extend_from_slice(ATOMS[g.r(ATOMS.len() as u64) as usize])
        }
    }
    let chunks: Vec<usize> = (0..g.r(20)).map(|_| g.r(64) as usize + 1).collect();
    let (rows, cols, hist) = (g.r(8) as u16 + 1, g.r(30) as u16 + 1, g.r(3) as usize * 7);
    let alt_allowed = g.r(8) != 0;
    let scroll_on_clear = g.r(4) == 0;
    let widths = (g.r(6) == 0).then(|| {
        const RANGES: [(u32, u32); 6] = [(0x1F1E6, 0x1F1FF), (0x2764, 0x2764), (0xAC00, 0xD7A3), (0x1F469, 0x1F469), (0x301, 0x301), (0x61, 0x63)];
        let w = (0..g.r(3) + 1).map(|_| { let (a, b) = RANGES[g.r(6) as usize]; (a, b, g.r(3) as u16) }).collect();
        (w, g.r(2) == 0)
    });
    let input_limit = (g.r(5) == 0).then(|| 32 + g.r(300) as usize);
    let timed = g.r(3) != 0;
    let mut ops = Vec::new();
    let mut i = 0;
    for (k, &c) in chunks.iter().enumerate() {
        if timed {
            ops.push(Op::SetTime(1000 + k as u64));
        }
        let e = (i + c).min(data.len());
        ops.push(Op::Process(i, e));
        i = e;
        match g.r(16) {
            0 => ops.push(Op::SetSize(g.r(8) as u16 + 1, g.r(30) as u16 + 1)),
            1 => ops.push(Op::SetSize(g.r(8) as u16 + 1, 0)),
            2 => ops.push(Op::ClearScrollback),
            _ => {}
        }
    }
    if timed {
        ops.push(Op::SetTime(2000));
    }
    ops.push(Op::Process(i, data.len()));
    ops.push(Op::Dump);
    Case { rows, cols, hist, alt_allowed, scroll_on_clear, widths, input_limit, probe: true, data, ops }
}

/// `resize-fuzz` case: 60 steps of random text then a random resize (a third
/// of them change only the height) on a 10x30 pane with 300 lines of history,
/// with the full state compared after every write and every resize.
fn resize_case(seed: u64, steps: usize) -> Case {
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) | 1;
    let mut rnd = move |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n
    };
    const PIECES: [&str; 30] = ["\x1b[?1049h", "\x1b[?1049l", "\x1b[2;5r", "\x1b[r", "\x1b[K", "\x1b[2J", "\x1b[3J", "\x1b[2@", "\x1b[2P", "\x1b[H", "\x1b[1L", "\x1b[1M", "abc", "한글", "\t",
        "\x1b[31m", "\x1b[0m", "\x1b]8;;http://x\x07L\x1b]8;;\x07", "\r\n", "\r\n", "\r\n", "e\u{301}", "\x1b[38;2;1;2;3mrgb", "  ", "\x1b[5A", "\x1b[3C", "\u{2764}\u{fe0f}",
        "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", "\x1b]133;A\x07", "\x1b]133;C\x07"];
    let mut data = Vec::new();
    let mut ops = Vec::new();
    for step in 0..steps {
        let a = data.len();
        for _ in 0..rnd(200) {
            data.extend_from_slice(PIECES[rnd(PIECES.len() as u64) as usize].as_bytes());
        }
        ops.push(Op::SetTime(1000 + step as u64));
        ops.push(Op::Process(a, data.len()));
        ops.push(Op::Dump);
        let r = 1 + rnd(40) as u16;
        let c = if rnd(3) == 0 { 0 } else { 1 + rnd(90) as u16 };
        ops.push(Op::SetSize(r, c));
        ops.push(Op::Dump);
    }
    Case { rows: 10, cols: 30, hist: 300, alt_allowed: true, scroll_on_clear: false, widths: None, input_limit: None, probe: false, data, ops }
}

// ---------------------------------------------------------------- comparison

struct Opts {
    jobs: usize,
    cap: usize,
    out: std::path::PathBuf,
    deny_panic: bool,
    strict_panic: bool,
    quiet: bool,
}

enum Verdict {
    Same { panicked: Option<PanicAt> },
    Differ { base: Box<Outcome>, work: Box<Outcome> },
}

fn compare(case: &Case, o: &Opts) -> Verdict {
    let b = base::run(case, o.cap);
    let w = work::run(case, o.cap);
    let panics_same = match (&b.panic, &w.panic) {
        (None, None) => true,
        (Some(x), Some(y)) => x.op == y.op && x.msg == y.msg && short_file(&x.file) == short_file(&y.file) && (!o.strict_panic || x.line == y.line),
        _ => false,
    };
    if panics_same && b.log.same(&w.log) {
        Verdict::Same { panicked: b.panic }
    } else {
        Verdict::Differ { base: Box::new(b), work: Box::new(w) }
    }
}

fn report(name: &str, case: &Case, b: &Outcome, w: &Outcome, o: &Opts) {
    let _ = std::fs::create_dir_all(&o.out);
    let p = |f: &str| o.out.join(f);
    let _ = std::fs::write(p("base.txt"), &b.log.text);
    let _ = std::fs::write(p("work.txt"), &w.log.text);
    let _ = std::fs::write(p("input.bin"), &case.data);
    let mut c = case.clone();
    c.data = Vec::new();
    let _ = std::fs::write(p("case.txt"), format!("{c:#?}\n"));
    eprintln!("MISMATCH {name}: rows {} cols {} hist {} ops {}", case.rows, case.cols, case.hist, case.ops.len());
    if b.panic != w.panic {
        eprintln!("  base panic: {:?}", b.panic);
        eprintln!("  work panic: {:?}", w.panic);
    }
    let (bl, wl): (Vec<&str>, Vec<&str>) = (b.log.text.lines().collect(), w.log.text.lines().collect());
    match (0..bl.len().max(wl.len())).find(|&i| bl.get(i) != wl.get(i)) {
        Some(i) => {
            // the last probe ("opN ...") or dump ("dump opN ...") line before the difference
            let op = bl[..i.min(bl.len())].iter().rev().find_map(|l| {
                l.strip_prefix("op").or_else(|| l.strip_prefix("dump op")).and_then(|r| r.split(' ').next()).map(str::to_string)
            });
            eprintln!("  first difference at log line {} (after op {})", i + 1, op.as_deref().unwrap_or("-"));
            let cut = |s: &str| if s.len() > 600 { format!("{}...", &s[..s.floor_char_boundary(600)]) } else { s.to_string() };
            eprintln!("  base: {}", bl.get(i).map_or("<end>".into(), |s| cut(s)));
            eprintln!("  work: {}", wl.get(i).map_or("<end>".into(), |s| cut(s)));
        }
        None if b.log.len != w.log.len || b.log.hash != w.log.hash => {
            eprintln!("  logs differ past the first {} bytes kept (raise --log-cap)", o.cap)
        }
        None => {}
    }
    eprintln!("  wrote {}/{{base.txt,work.txt,input.bin,case.txt}}", o.out.display());
}

/// Runs the seeds `start..start + n` that belong to shard `k` of `of`, one
/// at a time: vt100 numbers hyperlinks with a process-wide counter, so the two
/// copies only stay in step when every case runs on both, in order, in one
/// thread. Stops at the first mismatch.
fn run_seeds(mode: &str, start: u64, n: u64, (k, of): (u64, u64), o: &Opts, gen_case: &dyn Fn(u64) -> Case) -> i32 {
    let mut panics = Vec::<(u64, PanicAt)>::new();
    let mut done = 0u64;
    let mut seed = start + k;
    while seed < start + n {
        let case = gen_case(seed);
        match compare(&case, o) {
            Verdict::Same { panicked } => {
                if let Some(p) = panicked {
                    panics.push((seed, p));
                }
            }
            Verdict::Differ { base, work } => {
                report(&format!("{mode} seed {seed}"), &case, &base, &work, o);
                // if this one matches, the difference came from state kept between
                // cases in one process (such as the hyperlink counter)
                eprintln!("  rerun alone: run.sh {mode} 1 --start {seed}");
                return 1;
            }
        }
        done += 1;
        if !o.quiet && done.is_multiple_of(20000) {
            eprintln!("  {mode}{}: {done} cases", if of > 1 { format!(" shard {k}/{of}") } else { String::new() });
        }
        seed += of;
    }
    let mut sites: Vec<String> = panics.iter().map(|(_, x)| format!("{}:{} {}", short_file(&x.file), x.line, x.msg)).collect();
    sites.sort();
    sites.dedup();
    let shard = if of > 1 { format!(" (shard {k}/{of})") } else { String::new() };
    println!("{mode} ok{shard}: {done} cases from seed {start} match; {} panicked identically in both", panics.len());
    for s in &sites {
        println!("  panic site (base line numbers): {s}");
    }
    if !panics.is_empty() {
        let seeds: Vec<String> = panics.iter().take(10).map(|x| x.0.to_string()).collect();
        println!("  panicking seeds: {}{}", seeds.join(" "), if panics.len() > 10 { " ..." } else { "" });
        if o.deny_panic {
            eprintln!("{} cases panicked (identically in both; --allow-panic accepts that)", panics.len());
            return 3;
        }
    }
    0
}

/// Splits a seed run over `jobs` child processes (see `run_seeds` for why not
/// threads); the exit status is the worst of theirs.
fn run_sharded(mode: &str, args: &[String], o: &Opts, shard: Option<(u64, u64)>, start: u64, n: u64, gen_case: &dyn Fn(u64) -> Case) -> i32 {
    if let Some(s) = shard {
        return run_seeds(mode, start, n, s, o, gen_case);
    }
    let jobs = (o.jobs.max(1) as u64).min(n.max(1));
    if jobs == 1 {
        return run_seeds(mode, start, n, (0, 1), o, gen_case);
    }
    let exe = std::env::current_exe().expect("current_exe");
    let kids: Vec<_> = (0..jobs)
        .map(|k| {
            std::process::Command::new(&exe)
                .args(args)
                .args(["--shard", &format!("{k}/{jobs}"), "--out", &o.out.join(format!("shard{k}")).to_string_lossy()])
                .spawn()
                .expect("spawn shard")
        })
        .collect();
    let codes: Vec<i32> = kids.into_iter().map(|mut c| c.wait().ok().and_then(|s| s.code()).unwrap_or(1)).collect();
    let code = if codes.contains(&1) { 1 } else { codes.into_iter().max().unwrap_or(0) };
    if code == 0 {
        println!("{mode}: all {n} cases from seed {start} match ({jobs} shards)");
    }
    code
}

// ---------------------------------------------------------------- main

const USAGE: &str = "usage (normally through run.sh, which prepares the base copy):
  vt100-diff fuzz N [--start S] [common]          random input, sizes and settings, N cases
  vt100-diff resize-fuzz [N] [--start S] [--steps K] [common]
                                                  write/resize sequences (default 400 seeds x 60 steps)
  vt100-diff cmp FILE [file options] [common]     one file, final state and each --resize compared
  vt100-diff bench FILE [file options] [--reps N] CPU time of base and work (feed, then each --resize)
file options: --size ROWSxCOLS (39x120) --hist N (2000) --chunk N (4095) --onlcr (\\n -> \\r\\n)
              --time (stamp history time per chunk) --resize RxC,RxC,...
common: -j N (2 processes) --log-cap BYTES (64 MiB) --out DIR (target/vt100-diff-out)
        --allow-panic (identical panics on both sides count as a match; by default any panic fails)
        --strict-panic (with --allow-panic: panic line numbers must match too) --quiet
exit status: 0 same, 1 difference, 2 usage, 3 a case panicked on both sides (not with --allow-panic)";

fn parse_size(s: &str) -> Option<(u16, u16)> {
    let (r, c) = s.split_once('x')?;
    Some((r.parse().ok()?, c.parse().ok()?))
}

fn main() {
    install_panic_hook();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut pos = Vec::new();
    let mut o = Opts { jobs: 2, cap: 64 << 20, out: std::env::var_os("VT100_DIFF_OUT").map_or_else(|| "vt100-diff-out".into(), Into::into), deny_panic: true, strict_panic: false, quiet: false };
    let (mut start, mut steps, mut reps, mut shard) = (None, 60usize, 5usize, None);
    let (mut size, mut hist, mut chunk, mut onlcr, mut timed, mut resizes) = ((39u16, 120u16), 2000usize, 4095usize, false, false, Vec::new());
    let usage = |m: &str| -> ! {
        eprintln!("{m}\n{USAGE}");
        std::process::exit(2)
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |what: &str| it.next().cloned().unwrap_or_else(|| usage(&format!("{what} needs a value")));
        let num = |s: String| -> u64 { s.parse().unwrap_or_else(|_| usage(&format!("bad number {s}"))) };
        match a.as_str() {
            "-j" => o.jobs = num(val("-j")) as usize,
            "--log-cap" => o.cap = num(val("--log-cap")) as usize,
            "--out" => o.out = val("--out").into(),
            // panics fail by default since the crash fix (ebc077c); --allow-panic
            // accepts identical panics, for a base from before that fix
            "--deny-panic" => o.deny_panic = true,
            "--allow-panic" => o.deny_panic = false,
            "--strict-panic" => o.strict_panic = true,
            "--quiet" => o.quiet = true,
            "--start" => start = Some(num(val("--start"))),
            "--shard" => {
                let v = val("--shard");
                shard = v.split_once('/').and_then(|(k, n)| Some((k.parse::<u64>().ok()?, n.parse::<u64>().ok()?))).filter(|&(k, n)| k < n);
                if shard.is_none() { usage("bad --shard") }
            }
            "--steps" => steps = num(val("--steps")) as usize,
            "--reps" => reps = num(val("--reps")) as usize,
            "--size" => size = parse_size(&val("--size")).unwrap_or_else(|| usage("bad --size")),
            "--hist" => hist = num(val("--hist")) as usize,
            "--chunk" => chunk = (num(val("--chunk")) as usize).max(1),
            "--onlcr" => onlcr = true,
            "--time" => timed = true,
            "--resize" => {
                resizes = val("--resize").split(',').map(|s| parse_size(s).unwrap_or_else(|| usage("bad --resize"))).collect()
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            s if s.starts_with('-') && s.len() > 1 => usage(&format!("unknown option {s}")),
            _ => pos.push(a.clone()),
        }
    }
    let base_name = std::env::var("VT100_DIFF_BASE").unwrap_or_else(|_| "base".into());
    let load = |f: &str| -> Vec<u8> {
        let d = std::fs::read(f).unwrap_or_else(|e| usage(&format!("{f}: {e}")));
        if !onlcr {
            return d;
        }
        let mut v = Vec::with_capacity(d.len() + d.len() / 16);
        for b in d {
            if b == b'\n' {
                v.push(b'\r');
            }
            v.push(b);
        }
        v
    };
    let code = match pos.first().map(String::as_str) {
        Some("fuzz") => {
            let n: u64 = pos.get(1).and_then(|s| s.parse().ok()).unwrap_or_else(|| usage("fuzz needs N"));
            if shard.is_none() { eprintln!("fuzz: {n} cases, base {base_name} vs working tree, {} processes", o.jobs) }
            run_sharded("fuzz", &args, &o, shard, start.unwrap_or(1), n, &fuzz_case)
        }
        Some("resize-fuzz") => {
            let n: u64 = pos.get(1).map_or(400, |s| s.parse().unwrap_or_else(|_| usage("bad N")));
            if shard.is_none() { eprintln!("resize-fuzz: {n} seeds x {steps} steps, base {base_name} vs working tree, {} processes", o.jobs) }
            run_sharded("resize-fuzz", &args, &o, shard, start.unwrap_or(0), n, &|s| resize_case(s, steps))
        }
        Some("cmp") => {
            let f = pos.get(1).unwrap_or_else(|| usage("cmp needs FILE"));
            let data = load(f);
            let mut ops = Vec::new();
            let mut i = 0;
            for (k, c) in data.chunks(chunk).enumerate() {
                if timed {
                    ops.push(Op::SetTime(1000 + k as u64));
                }
                ops.push(Op::Process(i, i + c.len()));
                i += c.len();
            }
            ops.push(Op::Dump);
            for &(r, c) in &resizes {
                ops.push(Op::SetSize(r, c));
                ops.push(Op::Dump);
            }
            let case = Case { rows: size.0, cols: size.1, hist, alt_allowed: true, scroll_on_clear: false, widths: None, input_limit: None, probe: false, data, ops };
            match compare(&case, &o) {
                Verdict::Same { panicked } => {
                    println!("cmp same: {f} ({} bytes, {}x{} hist {hist}, {} resizes){}", case.data.len(), size.0, size.1, resizes.len(),
                        panicked.as_ref().map_or(String::new(), |p| format!("; both panicked at {}:{} {}", short_file(&p.file), p.line, p.msg)));
                    if panicked.is_some() && o.deny_panic { 3 } else { 0 }
                }
                Verdict::Differ { base, work } => {
                    report(&format!("cmp {f}"), &case, &base, &work, &o);
                    1
                }
            }
        }
        Some("bench") => {
            let f = pos.get(1).unwrap_or_else(|| usage("bench needs FILE"));
            let data = load(f);
            let mut tb: Vec<Vec<f64>> = Vec::new();
            let mut tw: Vec<Vec<f64>> = Vec::new();
            for rep in 0..reps.max(1) {
                // alternate the order so neither side always runs on a warm cache
                if rep % 2 == 0 {
                    tb.push(base::bench(&data, size.0, size.1, hist, chunk, &resizes));
                    tw.push(work::bench(&data, size.0, size.1, hist, chunk, &resizes));
                } else {
                    tw.push(work::bench(&data, size.0, size.1, hist, chunk, &resizes));
                    tb.push(base::bench(&data, size.0, size.1, hist, chunk, &resizes));
                }
            }
            let med = |v: &[Vec<f64>], k: usize| {
                let mut x: Vec<f64> = v.iter().map(|r| r[k]).collect();
                x.sort_by(f64::total_cmp);
                (x[x.len() / 2], x[0])
            };
            println!("bench {f}: {} bytes, {}x{} hist {hist}, chunk {chunk}, {reps} reps (CPU s, median [min])", data.len(), size.0, size.1);
            let label = |k: usize| if k == 0 { "feed".to_string() } else { format!("resize {}x{}", resizes[k - 1].0, resizes[k - 1].1) };
            for k in 0..=resizes.len() {
                let (bm, bn) = med(&tb, k);
                let (wm, wn) = med(&tw, k);
                println!("  {:<16} base {bm:.4} [{bn:.4}]  work {wm:.4} [{wn:.4}]  work/base {:.3}", label(k), wm / bm.max(1e-9));
            }
            0
        }
        _ => usage("no mode given"),
    };
    std::process::exit(code);
}
