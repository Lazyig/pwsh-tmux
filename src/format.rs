//! tmux FORMATS (`#{...}` expansion) and style strings (`fg=red,bold`, `#[...]` directives),
//! plus laying styled status-line text out into cells.

use crate::cells::{self, Cell, Style};
use chrono::{DateTime, Datelike, Local, TimeZone};
use std::collections::HashMap;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use vt100::Color;

pub trait Vars {
    /// Value of a format variable such as "session_name", "window_index", "pane_current_path".
    fn get(&self, name: &str) -> Option<String>;
    /// Output of a `#(command)` (already format-expanded). Callers usually run it
    /// asynchronously and return the last cached result; None expands to nothing.
    fn shell(&self, _command: &str) -> Option<String> {
        None
    }
    /// Contexts for S/W/P loops, paired with their active state.
    fn contexts(&self, _kind: char, _flags: &str) -> Vec<(Box<dyn Vars + '_>, bool)> {
        Vec::new()
    }
    /// First matching visible pane row (one-based), or zero.
    fn search(&self, _pattern: &str, _regex: bool, _ignore_case: bool) -> usize {
        0
    }
    /// Rows for the O (options, `option_*`) and V (environment, `environ_*`) loops, in order.
    fn entries(&self, _kind: char, _flags: &str) -> Vec<HashMap<String, String>> {
        Vec::new()
    }
    /// Contexts for the L loop: one per attached client, sorted as `flags` asks (i, n, t, r).
    fn client_contexts(&self, _flags: &str) -> Vec<Box<dyn Vars + '_>> {
        Vec::new()
    }
    /// The I modifier: `kind` is 'c' (terminfo capability), 'f' (terminal feature) or 'e'
    /// (client environment). None when there is no attached client terminal.
    fn client_info(&self, _kind: char, _name: &str) -> Option<String> {
        None
    }
    /// Called when a status format shows an animation (A) of more than one frame, so the
    /// client's status line is redrawn after one animation period.
    fn schedule_animation(&self) {}
    /// Window user options (`@name`, value) of this context, for prev_/next_ in W loops.
    fn user_options(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// The client's resolved colour for a theme name (themeblack ... thememagenta).
    /// None if unknown: styles then use the default theme and colour escapes the terminal colour.
    fn theme_color(&self, _name: &str) -> Option<Color> {
        None
    }
    /// The session environment, then the global one: format_find's last resort for a name
    /// that is not a variable or option. A name removed (-r) in the session has no value and
    /// is not looked up globally.
    fn environ(&self, _name: &str) -> Option<String> {
        None
    }
}

impl Vars for HashMap<String, String> {
    fn get(&self, name: &str) -> Option<String> {
        HashMap::get(self, name).cloned()
    }
}

impl Vars for HashMap<&str, &str> {
    fn get(&self, name: &str) -> Option<String> {
        HashMap::get(self, name).map(|s| s.to_string())
    }
}

/// Recursion limit for nested expansion (`E:` of self-referencing values etc).
const MAX_DEPTH: usize = 64;
const MAX_FORMAT_BYTES: usize = 1 << 20;
const MAX_FORMAT_WORK: usize = 1 << 20;

/// Expand a format string.
pub fn expand(template: &str, vars: &dyn Vars) -> String {
    Expander::top(vars, Local::now(), None, &std::cell::Cell::new(MAX_FORMAT_WORK)).expand(template, 0)
}

/// Like expand, but also applies strftime(3) `%` sequences (status-left/right, display-message).
/// As in tmux, strftime is applied to the template before format expansion, so `%` characters
/// inside variable values (pane titles etc) are left alone.
pub fn expand_time(template: &str, vars: &dyn Vars) -> String {
    expand_time_at(template, vars, Local::now())
}

/// expand_time for status line formats (tmux's FORMAT_STATUS): only these show the frames
/// of an `A` animation; elsewhere it expands to nothing.
pub fn expand_status_time(template: &str, vars: &dyn Vars) -> String {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let ms = START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64;
    let budget = std::cell::Cell::new(MAX_FORMAT_WORK);
    Expander { time: true, ..Expander::top(vars, Local::now(), Some(ms), &budget) }.expand(template, 0)
}

fn expand_time_at(template: &str, vars: &dyn Vars, now: DateTime<Local>) -> String {
    let budget = std::cell::Cell::new(MAX_FORMAT_WORK);
    Expander { time: true, ..Expander::top(vars, now, None, &budget) }.expand(template, 0)
}

// ---------------------------------------------------------------------------------------------
// Format expansion
// ---------------------------------------------------------------------------------------------

struct Expander<'a> {
    budget: &'a std::cell::Cell<usize>,
    vars: &'a dyn Vars,
    now: DateTime<Local>,
    /// Animation clock (ms) in status formats; None elsewhere and inside `#()` (NOCYCLE).
    cycle: Option<u64>,
    /// `#()` is not run (expanding a command or its output, tmux's NOJOBS).
    nojobs: bool,
    /// strftime(3) is applied to every format expanded (tmux's FORMAT_EXPAND_TIME), so
    /// also to E: values and other nested formats.
    time: bool,
}

/// Animation frame period in milliseconds (FORMAT_CYCLE_PERIOD).
const CYCLE_PERIOD: u64 = 100;

fn alias(c: u8) -> Option<&'static str> {
    Some(match c {
        b'H' => "host",
        b'h' => "host_short",
        b'D' => "pane_id",
        b'F' => "window_flags",
        b'I' => "window_index",
        b'P' => "pane_index",
        b'S' => "session_name",
        b'T' => "pane_title",
        b'W' => "window_name",
        _ => return None,
    })
}

/// tmux's format_skip: from `start`, find the first byte in `ends` at bracket depth 0.
/// `#{` opens a level, `}` closes one, and `#,` `##` `#{` `#}` `#:` are escapes.
fn skip(s: &str, start: usize, ends: &[u8]) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0i32;
    let mut i = start;
    while i < b.len() {
        if b[i] == b'#' && i + 1 < b.len() {
            if b[i + 1] == b'{' {
                depth += 1;
            }
            if b",#{}:".contains(&b[i + 1]) {
                i += 2;
                continue;
            }
        }
        if b[i] == b'}' {
            depth -= 1;
        }
        if ends.contains(&b[i]) && depth == 0 {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Split at top-level commas.
fn split_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(i) = skip(s, start, b",") {
        out.push(&s[start..i]);
        start = i + 1;
    }
    out.push(&s[start..]);
    out
}

/// Split into two at the first top-level comma.
fn split2(s: &str) -> Option<(&str, &str)> {
    skip(s, 0, b",").map(|i| (&s[..i], &s[i + 1..]))
}

#[derive(Debug, PartialEq)]
struct Modifier {
    name: &'static str,
    args: Vec<String>,
}

const MODIFIERS2: [&str; 7] = ["||", "&&", "!=", "==", "<=", ">=", "!!"];
const MODIFIERS1: &str = "labdEeTtmnpqswCSWP=!<>OVILNRAc";

/// Parse a modifier prefix such as `=10;b:` returning the modifiers and the remaining text.
/// Returns None when the text does not start with a valid modifier list ending in `:`.
fn parse_modifiers(s: &str) -> Option<(Vec<Modifier>, &str)> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut mods = Vec::new();
    loop {
        // Modifier name.
        let name: &'static str = if let Some(m) = MODIFIERS2.iter().find(|m| s[i..].starts_with(**m)) {
            m
        } else if i < b.len() && MODIFIERS1.as_bytes().contains(&b[i]) {
            let p = MODIFIERS1.find(b[i] as char).unwrap();
            &MODIFIERS1[p..p + 1]
        } else {
            return None;
        };
        i += name.len();
        // Arguments.
        let mut args = Vec::new();
        let c = *b.get(i)?;
        if c == b';' || c == b':' {
            // none
        } else if !"ImCLNPSOVst=pReqWcA".contains(name) {
            // Only these take arguments.
            return None;
        } else if !c.is_ascii_punctuation() || c == b'-' {
            // A single argument, unless the next character is punctuation to separate several.
            let e = skip(s, i, b";:")?;
            args.push(s[i..e].to_string());
            i = e;
        } else {
            let sep = c;
            i += 1;
            loop {
                let e = skip(s, i, &[sep, b';', b':'])?;
                if b[e] == sep {
                    args.push(s[i..e].to_string());
                    i = e + 1;
                    if i < b.len() && (b[i] == b';' || b[i] == b':') {
                        break;
                    }
                } else {
                    if e > i {
                        args.push(s[i..e].to_string());
                    }
                    i = e;
                    break;
                }
            }
        }
        mods.push(Modifier { name, args });
        match b.get(i) {
            Some(b':') => return Some((mods, &s[i + 1..])),
            Some(b';') => i += 1,
            _ => return None,
        }
    }
}

/// strtonum(3): an integer with optional leading space and sign, nothing after it.
fn strtonum(s: &str) -> Option<i64> {
    s.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']).parse().ok()
}

fn truthy(s: &str) -> bool {
    !s.is_empty() && s != "0"
}

fn bool_str(b: bool) -> String {
    if b { "1".into() } else { "0".into() }
}

impl<'a> Expander<'a> {
    fn top(vars: &'a dyn Vars, now: DateTime<Local>, cycle: Option<u64>, budget: &'a std::cell::Cell<usize>) -> Self {
        Expander { vars, now, budget, cycle, nojobs: false, time: false }
    }

    /// An expander for a nested context (loop row) sharing this one's budget and state.
    fn with<'b>(&'b self, vars: &'b dyn Vars) -> Expander<'b> {
        Expander { vars, now: self.now, budget: self.budget, cycle: self.cycle, nojobs: self.nojobs, time: self.time }
    }

    /// format_find: a variable or option, then (unless for the t modifier) the session and
    /// global environment.
    fn lookup(&self, name: &str, time: bool) -> Option<String> {
        if let Some(v) = self.vars.get(name) {
            return Some(v);
        }
        if name == "host_short" {
            return self.vars.get("host").map(|h| h.split('.').next().unwrap_or("").to_string());
        }
        if time { None } else { self.vars.environ(name) }
    }

    fn expand(&self, s: &str, depth: usize) -> String {
        if depth > MAX_DEPTH || s.len() > MAX_FORMAT_BYTES || s.len().saturating_add(1) > self.budget.get() {
            return String::new();
        }
        self.budget.set(self.budget.get() - s.len() - 1);
        let timed;
        let s = if self.time && s.contains('%') {
            timed = strftime(s, &self.now);
            timed.as_str()
        } else {
            s
        };
        let b = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        let mut lit = 0;
        // End (the `]`) of the last `#[` style seen; single-letter aliases are not expanded
        // inside one, so colours such as #DDDDDD are left alone.
        let mut style_end: Option<usize> = None;
        while i < b.len() {
            if b[i] != b'#' {
                i += 1;
                continue;
            }
            out.push_str(&s[lit..i]);
            if i + 1 >= b.len() {
                // As format_expand1, a trailing `#` is dropped.
                lit = b.len();
                break;
            }
            let c = b[i + 1];
            match c {
                b'{' => match skip(s, i, b"}") {
                    Some(end) => {
                        let Some(value) = self.replace(&s[i + 2..end], depth + 1) else { return out };
                        out.push_str(&value);
                        i = end + 1;
                    }
                    None => {
                        // Like tmux, stop at an unterminated format.
                        return out;
                    }
                },
                b'(' => {
                    let mut d = 0;
                    let mut j = i + 1;
                    while j < b.len() {
                        match b[j] {
                            b'(' => d += 1,
                            b')' => {
                                d -= 1;
                                if d == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                    if j < b.len() && !self.nojobs {
                        // As format_job_get, the command and then its output are expanded, with
                        // no further jobs and no animation.
                        let inner = Expander { cycle: None, nojobs: true, time: false, ..self.with(self.vars) };
                        let command = inner.expand(&s[i + 2..j], depth + 1);
                        let output = self.vars.shell(&command).unwrap_or_default();
                        out.push_str(&inner.expand(output.trim_end_matches(['\r', '\n']), depth + 1));
                    }
                    i = (j + 1).min(b.len());
                }
                b'[' | b'#' => {
                    let run = b[i..].iter().take_while(|&&c| c == b'#').count();
                    if b.get(i + run) == Some(&b'[') {
                        // `#[`, `##[`, `###[` ...: a style, left (with all its `#`s) for
                        // format_draw.
                        style_end = skip(s, i, b"]");
                        out.push_str(&s[i..=i + run]);
                        i += run + 1;
                    } else {
                        // `##` is `#`; an odd `#` at the end of the run starts the next item.
                        out.extend(std::iter::repeat_n('#', run / 2));
                        i += run - run % 2;
                    }
                }
                b',' | b'}' => {
                    out.push(c as char);
                    i += 2;
                }
                _ => {
                    match alias(c).filter(|_| style_end.is_none_or(|e| i + 2 > e)) {
                        Some(name) => {
                            out.push_str(&self.lookup(name, false).unwrap_or_default());
                            i += 2;
                        }
                        None => {
                            // Anything else (`#:` too) passes through.
                            out.push('#');
                            i += 1;
                        }
                    }
                }
            }
            if out.len() > MAX_FORMAT_BYTES { return String::new(); }
            lit = i;
        }
        if out.len().saturating_add(s.len() - lit) > MAX_FORMAT_BYTES { return String::new(); }
        out.push_str(&s[lit..]);
        out
    }

    /// Evaluate the contents of one `#{...}`, following format_replace: the first matching
    /// kind (client information, animation, literal, character, colour, loops, name checks,
    /// search, repeat, boolean operators, comparisons, conditionals, expressions, then a
    /// variable) gives the value, which E/T, s, =, p, n and w then modify in that order.
    fn replace(&self, content: &str, depth: usize) -> Option<String> {
        let (mut mods, copy) = parse_modifiers(content).unwrap_or((Vec::new(), content));
        // Arguments are unescaped and expanded once, when the modifiers are built.
        for m in &mut mods {
            for a in &mut m.args {
                *a = self.expand(&unescape_format(a), depth + 1);
            }
        }
        // format_replace walks the modifiers in order, so of a repeated modifier the last one
        // counts; most ignore an occurrence without arguments, and flags of I, c and N add up.
        let get = |n: &str| mods.iter().find(|m| m.name == n);
        let last = |n: &str| mods.iter().rev().find(|m| m.name == n);
        let last_args = |n: &str| mods.iter().rev().find(|m| m.name == n && !m.args.is_empty());
        let all_flags = |n: &str| mods.iter().filter(|m| m.name == n).filter_map(|m| m.args.first()).map(String::as_str).collect::<String>();
        let ex = |s: &str| self.expand(s, depth + 1);
        // I/e wins over I/f over I/c, as the later checks in tmux overwrite the value.
        let info = get("I").map(|_| all_flags("I")).and_then(|f| ['e', 'f', 'c'].into_iter().find(|k| f.contains(*k)));
        let loop_kind = ['S', 'W', 'P', 'L', 'O', 'V'].into_iter().find(|c| get(&c.to_string()).is_some());
        let name_kind = if mods.iter().any(|m| m.name == "N" && m.args.first().is_none_or(|f| f.contains('w'))) {
            Some('W')
        } else if mods.iter().any(|m| m.name == "N" && m.args.first().is_some_and(|f| f.contains('s'))) {
            Some('S')
        } else {
            None
        };
        let compare = mods.iter().rev().find(|m| ["==", "!=", "<", ">", "<=", ">=", "m"].contains(&m.name));
        let time = get("t").is_some();

        let mut value = if let Some(kind) = info {
            self.vars.client_info(kind, copy).unwrap_or_default()
        } else if get("A").is_some() {
            let count = last_args("A").and_then(|m| strtonum(&m.args[0])).filter(|n| (1..=100).contains(n)).unwrap_or(1) as u64;
            self.cycle(copy, count)
        } else if get("l").is_some() {
            unescape_format(copy)
        } else if get("a").is_some() {
            strtonum(&ex(copy))
                .filter(|n| (32..127).contains(n))
                .and_then(|n| char::from_u32(n as u32))
                .map(String::from)
                .unwrap_or_default()
        } else if get("c").is_some() {
            colour_format(&ex(copy), &all_flags("c"), self.vars)
        } else if let Some(kind) = loop_kind {
            // The sort flags of S, W, P and L come from the last one; O and V take one argument.
            let name = kind.to_string();
            let m = if "OV".contains(kind) { mods.iter().rev().find(|m| m.name == name && m.args.len() == 1) } else { last(&name) };
            let flags = m.and_then(|m| m.args.first()).map(String::as_str).unwrap_or("");
            self.expand_loop(kind, flags, copy, depth)?
        } else if let Some(kind) = name_kind {
            let name = ex(copy);
            let key = if kind == 'S' { "session_name" } else { "window_name" };
            bool_str(self.vars.contexts(kind, "").iter().any(|(v, _)| v.get(key).as_deref() == Some(&name)))
        } else if let Some(m) = last("C") {
            let flags = m.args.first().map(String::as_str).unwrap_or("");
            self.vars.search(&ex(copy), flags.contains('r'), flags.contains('i')).to_string()
        } else if get("R").is_some() {
            // Repeat: at most 10000 times and 65536 bytes.
            let (text, count) = split2(copy)?;
            let text = ex(text);
            match strtonum(&ex(count)).filter(|n| (1..=10000).contains(n) && text.len().saturating_mul(*n as usize) <= 65536) {
                Some(n) => text.repeat(n as usize),
                None => String::new(),
            }
        } else if get("!").is_some() {
            bool_str(!truthy(&ex(copy)))
        } else if get("!!").is_some() {
            bool_str(truthy(&ex(copy)))
        } else if let Some(m) = mods.iter().rev().find(|m| m.name == "||" || m.name == "&&") {
            // n-ary, short-circuiting: #{||:a,b,c}
            let mut parts = Vec::new();
            let mut rest = copy;
            while let Some((a, b)) = split2(rest) {
                parts.push(a);
                rest = b;
            }
            parts.push(rest);
            let or = m.name == "||";
            bool_str(if or { parts.iter().any(|p| truthy(&ex(p))) } else { parts.iter().all(|p| truthy(&ex(p))) })
        } else if let Some(m) = compare {
            let (a, b) = split2(copy)?;
            let (a, b) = (ex(a), ex(b));
            match m.name {
                "==" => bool_str(a == b),
                "!=" => bool_str(a != b),
                "<" => bool_str(a < b),
                ">" => bool_str(a > b),
                "<=" => bool_str(a <= b),
                ">=" => bool_str(a >= b),
                _ => {
                    let flags = m.args.first().map(String::as_str).unwrap_or("");
                    if flags.contains('p') || flags.contains('z') {
                        fuzzy_match(&a, &b, flags.contains('p'))
                    } else if flags.contains('r') {
                        bool_str(regex_matches(&a, &b, flags.contains('i')))
                    } else {
                        bool_str(glob_match(&a, &b, flags.contains('i')))
                    }
                }
            }
        } else if let Some(rest) = copy.strip_prefix('?') {
            let parts = split_commas(rest);
            let mut i = 0;
            let mut chosen = None;
            while i + 1 < parts.len() {
                let cond = match self.lookup(parts[i], time) {
                    Some(v) => v,
                    None => {
                        let e = ex(parts[i]);
                        if e == parts[i] { String::new() } else { e }
                    }
                };
                if truthy(&cond) {
                    chosen = Some(parts[i + 1]);
                    break;
                }
                i += 2;
            }
            if chosen.is_none() && i < parts.len() && parts.len() % 2 == 1 {
                chosen = Some(parts[parts.len() - 1]);
            }
            chosen.map(ex).unwrap_or_default()
        } else if let Some(m) = mods.iter().rev().find(|m| m.name == "e" && (1..=3).contains(&m.args.len())) {
            // A bad expression (no comma too) is empty but does not stop the expansion.
            split2(copy).and_then(|(a, b)| arith(&m.args, &ex(a), &ex(b))).unwrap_or_default()
        } else if copy.contains("#{") {
            // As tmux, a name containing a format is expanded instead, and t, b, d and q do
            // not apply to it.
            ex(copy)
        } else {
            match self.lookup(copy, time) {
                None => String::new(),
                Some(v) if time => self.format_time(&v, &mods).unwrap_or_default(),
                Some(mut v) => {
                    // b, d and q only change a value that was found.
                    if get("b").is_some() {
                        v = basename(&v);
                    }
                    if get("d").is_some() {
                        v = dirname(&v);
                    }
                    // Each q sets one kind of quoting; tmux applies all that are set, in order.
                    let mut quote = [false; 4];
                    for m in mods.iter().filter(|m| m.name == "q") {
                        let f = m.args.first().map(String::as_str);
                        match f {
                            None => quote[0] = true,
                            Some(f) if f.contains('s') => quote[1] = true,
                            Some(f) if f.contains('e') || f.contains('h') => quote[2] = true,
                            Some(f) if f.contains('a') => quote[3] = true,
                            _ => {}
                        }
                    }
                    if quote[0] {
                        v = shell_quote(&v);
                    }
                    if quote[1] {
                        v = format!("'{}'", v.replace('\'', "'\\''"));
                    }
                    if quote[2] {
                        // Escape # for styles.
                        v = v.replace('#', "##");
                    }
                    if quote[3] {
                        v = quote_argument(&v);
                    }
                    v
                }
            }
        };

        if get("E").is_some() {
            value = ex(&value);
        } else if get("T").is_some() {
            value = Expander { time: true, ..self.with(self.vars) }.expand(&value, depth + 1);
        }
        for m in mods.iter().filter(|m| m.name == "s" && m.args.len() >= 2) {
            // tmux expands substitution arguments a second time.
            let flags = m.args.get(2).map(String::as_str).unwrap_or("");
            value = regex_substitute(&ex(&m.args[0]), &ex(&m.args[1]), &value, flags.contains('i'));
        }
        // Widths outside -10000..10000 are ignored.
        let width_arg = |n: &str| last_args(n).and_then(|m| strtonum(&m.args[0])).filter(|n| n.unsigned_abs() <= 10000).unwrap_or(0);
        let limit = width_arg("=");
        if limit != 0 {
            // The marker is kept from the last = that has one.
            let marker = mods.iter().rev().filter(|m| m.name == "=" && !m.args.is_empty()).find_map(|m| m.args.get(1)).map(String::as_str).unwrap_or("");
            let n = limit.unsigned_abs() as usize;
            if fmt_width(&value) > n {
                value = if limit > 0 { trim_left(&value, n) + marker } else { marker.to_string() + &trim_right(&value, n) };
            }
        }
        // utf8_padcstr measures the raw string: `#[...]` and `##` count as text here.
        let pad = width_arg("p");
        let w = cstr_width(&value);
        if (pad.unsigned_abs() as usize) > w {
            let fill = " ".repeat(pad.unsigned_abs() as usize - w);
            value = if pad > 0 { value + &fill } else { fill + &value };
        }
        if get("n").is_some() {
            value = value.len().to_string();
        }
        if get("w").is_some() {
            value = fmt_width(&value).to_string();
        }
        if value.len() > MAX_FORMAT_BYTES { return None; }
        Some(value)
    }

    /// The A modifier (format_cycle): one of the comma-separated frames, changing every
    /// `count` periods. Only in status formats and never inside `#()`; frames are not expanded.
    fn cycle(&self, frames: &str, count: u64) -> String {
        let Some(time) = self.cycle.filter(|_| !frames.is_empty()) else { return String::new() };
        let frames: Vec<&str> = frames.split(',').collect();
        if frames.len() > 1 {
            self.vars.schedule_animation();
        }
        frames[(time / (count * CYCLE_PERIOD) % frames.len() as u64) as usize].to_string()
    }

    /// S, W, P, L, O and V loops: expand `fmt` once per item with loop_index and
    /// loop_last_flag set. S, W and P take an alternative format for the current item
    /// after a comma; W also sets the window_*_active and prev_/next_ neighbour variables.
    fn expand_loop(&self, kind: char, flags: &str, fmt: &str, depth: usize) -> Option<String> {
        let (all, active) = match kind {
            'S' | 'W' | 'P' => split2(fmt).map_or((fmt, None), |(a, b)| (a, Some(b))),
            _ => (fmt, None),
        };
        let owned: Vec<Box<dyn Vars + '_>>;
        let rows: Vec<(&dyn Vars, bool, HashMap<String, String>)> = match kind {
            'S' | 'W' | 'P' => {
                let contexts = self.vars.contexts(kind, flags);
                // The current session is that of the client the format is for.
                let client_session = self.vars.get("client_session").filter(|s| !s.is_empty());
                let actives: Vec<bool> = contexts.iter().map(|(v, active)| {
                    if kind == 'S' { client_session.is_some() && v.get("session_name") == client_session } else { *active }
                }).collect();
                let mut rows = Vec::new();
                for i in 0..contexts.len() {
                    let mut extra = HashMap::new();
                    if kind == 'W' {
                        extra.insert("window_after_active".into(), bool_str(i > 0 && actives[i - 1]));
                        extra.insert("window_before_active".into(), bool_str(actives.get(i + 1) == Some(&true)));
                        for (prefix, n) in [("prev", i.checked_sub(1)), ("next", Some(i + 1))] {
                            let Some((nctx, _)) = n.and_then(|n| contexts.get(n)) else { continue };
                            extra.insert(format!("{prefix}_window_index"), nctx.get("window_index").unwrap_or_default());
                            extra.insert(format!("{prefix}_window_active"), bool_str(actives[n.unwrap()]));
                            for (key, value) in nctx.user_options() {
                                extra.insert(format!("{prefix}_{key}"), value);
                            }
                        }
                    }
                    rows.push((i, actives[i], extra));
                }
                owned = contexts.into_iter().map(|(v, _)| v).collect();
                rows.into_iter().map(|(i, active, extra)| (owned[i].as_ref(), active, extra)).collect()
            }
            'L' => {
                owned = self.vars.client_contexts(flags);
                owned.iter().map(|v| (v.as_ref(), false, HashMap::new())).collect()
            }
            _ => {
                self.vars.entries(kind, flags).into_iter().map(|extra| (self.vars, false, extra)).collect()
            }
        };
        let n = rows.len();
        let mut out = String::new();
        for (i, (base, is_active, mut extra)) in rows.into_iter().enumerate() {
            if self.budget.get() == 0 { break; }
            extra.insert("loop_index".into(), i.to_string());
            extra.insert("loop_last_flag".into(), bool_str(i + 1 == n));
            let ctx = Overlay { base, extra };
            let template = if is_active { active.unwrap_or(all) } else { all };
            let value = self.with(&ctx).expand(template, depth + 1);
            if out.len().saturating_add(value.len()) > MAX_FORMAT_BYTES { return None; }
            out.push_str(&value);
        }
        Some(out)
    }

    /// `t:` modifier: format a unix timestamp (format_find). Each t adds one of p (pretty),
    /// r (relative) and d (difference), or with f sets the strftime format; relative wins
    /// over difference over pretty over the format, and the default is ctime(3). None for a
    /// value that is not a positive number.
    fn format_time(&self, v: &str, mods: &[Modifier]) -> Option<String> {
        let (mut pretty, mut relative, mut difference, mut format) = (false, false, false, None);
        for m in mods.iter().filter(|m| m.name == "t") {
            let Some(f) = m.args.first() else { continue };
            if f.contains('p') {
                pretty = true;
            } else if f.contains('r') {
                relative = true;
            } else if f.contains('d') {
                difference = true;
            } else if f.contains('f') && m.args.len() >= 2 {
                format = Some(m.args[1].as_str());
            }
        }
        let secs = strtonum(v).filter(|n| *n > 0)?;
        let t = Local.timestamp_opt(secs, 0).single()?;
        let age = self.now.timestamp() - secs;
        Some(if relative {
            relative_time(age)
        } else if difference {
            age.to_string()
        } else if pretty {
            let age = self.now.signed_duration_since(t);
            if age.num_hours() < 24 {
                t.format("%H:%M").to_string()
            } else if (t.year() == self.now.year() && t.month() == self.now.month()) || age.num_days() < 28 {
                t.format("%a%d").to_string()
            } else if (t.year() == self.now.year() && t.month() < self.now.month()) || (t.year() == self.now.year() - 1 && t.month() > self.now.month()) {
                t.format("%d%b").to_string()
            } else {
                t.format("%b%y").to_string()
            }
        } else if let Some(f) = format {
            strftime(f, &t)
        } else {
            t.format("%a %b %e %H:%M:%S %Y").to_string()
        })
    }
}

/// format_unescape: drop the `#` of `#,` `##` `#{` `#}` `#:` outside nested formats.
fn unescape_format(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut depth = 0i32;
    while let Some(c) = chars.next() {
        if c == '#' && chars.peek() == Some(&'{') {
            depth += 1;
        }
        if depth == 0 && c == '#' && chars.peek().is_some_and(|c| ",#{}:".contains(*c)) {
            out.push(chars.next().unwrap());
            continue;
        }
        if c == '}' {
            depth -= 1;
        }
        out.push(c);
    }
    out
}

/// A loop row: the row's variables over a context, which provides everything else.
struct Overlay<'a> {
    base: &'a dyn Vars,
    extra: HashMap<String, String>,
}

impl Vars for Overlay<'_> {
    fn get(&self, name: &str) -> Option<String> {
        self.extra.get(name).cloned().or_else(|| self.base.get(name))
    }
    fn shell(&self, command: &str) -> Option<String> {
        self.base.shell(command)
    }
    fn contexts(&self, kind: char, flags: &str) -> Vec<(Box<dyn Vars + '_>, bool)> {
        self.base.contexts(kind, flags)
    }
    fn search(&self, pattern: &str, regex: bool, ignore_case: bool) -> usize {
        self.base.search(pattern, regex, ignore_case)
    }
    fn entries(&self, kind: char, flags: &str) -> Vec<HashMap<String, String>> {
        self.base.entries(kind, flags)
    }
    fn client_contexts(&self, flags: &str) -> Vec<Box<dyn Vars + '_>> {
        self.base.client_contexts(flags)
    }
    fn client_info(&self, kind: char, name: &str) -> Option<String> {
        self.base.client_info(kind, name)
    }
    fn schedule_animation(&self) {
        self.base.schedule_animation()
    }
    fn user_options(&self) -> Vec<(String, String)> {
        self.base.user_options()
    }
    fn theme_color(&self, name: &str) -> Option<Color> {
        self.base.theme_color(name)
    }
    fn environ(&self, name: &str) -> Option<String> {
        self.base.environ(name)
    }
}

/// q/a: quote as a command argument, as args_escape does (vis(3) C-style escapes; double
/// quotes if there is a space or one of #';${}%, single quotes for a double quote).
fn quote_argument(value: &str) -> String {
    if value.is_empty() {
        return "''".into();
    }
    let quote = if value.contains([' ', '#', '\'', ';', '$', '{', '}', '%']) {
        Some('"')
    } else if value.contains('"') {
        Some('\'')
    } else {
        None
    };
    if value.len() == 1 && value != " " && (quote.is_some() || value == "~") {
        return format!("\\{value}");
    }
    let mut escaped = String::new();
    let chars: Vec<char> = value.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\x07' => escaped.push_str("\\a"),
            '\x08' => escaped.push_str("\\b"),
            '\x0b' => escaped.push_str("\\v"),
            '\x0c' => escaped.push_str("\\f"),
            '\\' => escaped.push_str("\\\\"),
            c if c.is_ascii_control() => escaped.push_str(&format!("\\{:03o}", c as u32)),
            '"' if quote == Some('"') => escaped.push_str("\\\""),
            // In double quotes only a $ that would start a variable is escaped.
            '$' if quote == Some('"') && chars.get(i + 1).is_some_and(|n| n.is_ascii_alphabetic() || *n == '_' || *n == '{') => escaped.push_str("\\$"),
            _ => escaped.push(c),
        }
    }
    if escaped.starts_with('~') && quote != Some('\'') {
        escaped.insert(0, '\\');
    }
    match quote {
        Some(q) => format!("{q}{escaped}{q}"),
        None => escaped,
    }
}

/// t/r: the age of a time as "1d2h", "3h4m", "5m6s" or "7s"; nothing for a future time.
fn relative_time(age: i64) -> String {
    if age < 0 {
        return String::new();
    }
    let (d, h, m, s) = (age / 86400, age % 86400 / 3600, age % 3600 / 60, age % 60);
    let (a, unit, b, next) = if d != 0 {
        (d, "d", h, "h")
    } else if h != 0 {
        (h, "h", m, "m")
    } else if m != 0 {
        (m, "m", s, "s")
    } else {
        (s, "s", 0, "")
    };
    if b == 0 { format!("{a}{unit}") } else { format!("{a}{unit}{b}{next}") }
}

/// The RGB of a colour, None for the default.
pub fn color_rgb(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Idx(n) => Some(palette_rgb(n)),
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Default => None,
    }
}

/// RGB of a 256-colour palette entry (colour_256toRGB).
fn palette_rgb(n: u8) -> (u8, u8, u8) {
    const BASIC: [u32; 16] = [
        0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xc0c0c0,
        0x808080, 0xff0000, 0x00ff00, 0xffff00, 0x0000ff, 0xff00ff, 0x00ffff, 0xffffff,
    ];
    if n < 16 {
        let c = BASIC[n as usize];
        return ((c >> 16) as u8, (c >> 8) as u8, c as u8);
    }
    if n >= 232 {
        let c = 8 + (n - 232) * 10;
        return (c, c, c);
    }
    let n = n - 16;
    let component = |v| if v == 0 { 0 } else { 55 + v * 40 };
    (component(n / 36), component(n / 6 % 6), component(n % 6))
}

/// Dim a colour by a percentage (colour_dim), for styles with dim=. Default colours are
/// left alone.
#[allow(dead_code)] // for the renderer of window-style dim=
pub fn dim_color(color: Color, dim: u8) -> Color {
    let (r, g, b) = match color {
        _ if dim == 0 => return color,
        Color::Default => return color,
        Color::Idx(n) => palette_rgb(n),
        Color::Rgb(r, g, b) => (r, g, b),
    };
    let scale = |v: u8| (v as u32 * (100 - dim.min(100) as u32) / 100) as u8;
    Color::Rgb(scale(r), scale(g), scale(b))
}

/// The c modifier: a colour as hex RGB (c), or as an SGR escape for the foreground (c/f)
/// or background (c/b). Theme colours have no RGB value; as escapes they use the client's
/// theme or else the terminal colour.
fn colour_format(s: &str, flags: &str, vars: &dyn Vars) -> String {
    let (fg, bg) = (flags.contains('f'), flags.contains('b'));
    let lower = s.to_ascii_lowercase();
    let theme = THEMES.iter().position(|t| t.0 == lower);
    if !fg && !bg {
        let rgb = match theme.map_or_else(|| parse_color(s), |_| None) {
            Some(Color::Rgb(r, g, b)) => (r, g, b),
            Some(Color::Idx(n)) => palette_rgb(n),
            _ => return String::new(),
        };
        return format!("{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2);
    }
    if lower == "none" {
        return "\x1b[0m".into();
    }
    let base = if bg { 40 } else { 30 };
    let (color, is256) = match theme {
        Some(i) => match vars.theme_color(&lower) {
            Some(c) => (c, true),
            None => (Color::Idx(THEMES[i].3), false),
        },
        None => match parse_color(s) {
            Some(c) => (c, lower.starts_with("colo")),
            None => return String::new(),
        },
    };
    match color {
        Color::Default => format!("\x1b[{}m", base + 9),
        Color::Rgb(r, g, b) => format!("\x1b[{};2;{r};{g};{b}m", base + 8),
        Color::Idx(n) if is256 => format!("\x1b[{};5;{n}m", base + 8),
        Color::Idx(n) if n < 8 => format!("\x1b[{}m", base + n as u32),
        Color::Idx(n) => format!("\x1b[{}m", base + 60 + n as u32 - 8),
    }
}

/// One visible character of fuzzy-matched text: the character, its alignment group
/// (left, centre, right, absolute-centre), column offset within the group and width.
struct FuzzyChar {
    c: char,
    align: usize,
    offset: usize,
    width: usize,
}

/// fuzzy_scan: the visible characters of text, skipping `#[...]` styles but following
/// their align= to place characters as format_draw would.
fn fuzzy_scan(text: &str) -> (Vec<FuzzyChar>, [usize; 4]) {
    let mut cs = Vec::new();
    let mut widths = [0usize; 4];
    let mut align = Align::Left;
    let add = |cs: &mut Vec<FuzzyChar>, c: char, align: Align, widths: &mut [usize; 4]| {
        let a = match align { Align::Left => 0, Align::Centre => 1, Align::Right => 2, Align::AbsoluteCentre => 3 };
        let width = UnicodeWidthChar::width(c).unwrap_or(0);
        cs.push(FuzzyChar { c, align: a, offset: widths[a], width });
        widths[a] += width;
    };
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'#' {
            let n = b[i..].iter().take_while(|&&c| c == b'#').count();
            if b.get(i + n) != Some(&b'[') {
                // ## is #: half the run, rounded up.
                for _ in 0..n.div_ceil(2) { add(&mut cs, '#', align, &mut widths); }
                i += n;
                continue;
            }
            for _ in 0..n / 2 { add(&mut cs, '#', align, &mut widths); }
            if n % 2 == 0 {
                add(&mut cs, '[', align, &mut widths);
                i += n + 1;
                continue;
            }
            let Some(end) = skip(text, i + n + 1, b"]") else { break };
            // As format_draw, an invalid style changes nothing.
            let (mut st, mut a) = (Style::default(), align);
            if style_words(&text[i + n + 1..end]).all(|w| apply_word(w, &mut st, &mut a, Style::default(), None).is_ok()) {
                align = a;
            }
            i = end + 1;
            continue;
        }
        let c = text[i..].chars().next().unwrap();
        i += c.len_utf8();
        if !c.is_ascii_control() {
            add(&mut cs, c, align, &mut widths);
        }
    }
    (cs, widths)
}

/// The m/z and m/p modifiers (format_match_fuzzy and fuzzy.c): fzf-style matching where
/// `|` separates alternative groups and spaces separate terms that must all match. A term
/// is a fuzzy subsequence unless it starts with ' (substring), ^ (prefix) or ends with $
/// (suffix); a leading ! inverts it (as a substring). Case is ignored unless the pattern
/// has an upper case letter. m/z gives 1 or 0; m/p gives the matched display columns of
/// the best group, laid out as the status line would draw the text.
fn fuzzy_match(pattern: &str, text: &str, positions: bool) -> String {
    const EXACT: i64 = 1000;
    const PREFIX: i64 = 200;
    const SUFFIX: i64 = 100;
    const START: i64 = 12;
    const BOUNDARY: i64 = 8;
    const CONSECUTIVE: i64 = 6;
    const LEADING_MAX: usize = 10;
    let width = fmt_width(text).max(1);
    // An empty query matches everything, with nothing highlighted.
    if pattern.chars().all(|c| c == ' ' || c == '|') {
        return if positions { String::new() } else { "1".into() };
    }
    let fold = !pattern.chars().any(|c| c.is_ascii_uppercase());
    let (cs, widths) = fuzzy_scan(text);
    let n = cs.len();
    let equal = |a: char, b: char| a == b || (fold && a.is_ascii() && b.is_ascii() && a.eq_ignore_ascii_case(&b));
    let boundary = |i: usize| " -_/.:".contains(cs[i].c);
    let leading = |i: usize| i.min(LEADING_MAX) as i64;

    let exact = |tok: &[char], prefix: bool, suffix: bool| -> Option<(i64, usize)> {
        if tok.is_empty() || tok.len() > n || (prefix && suffix && tok.len() != n) {
            return None;
        }
        let range = if prefix { 0..1 } else if suffix { n - tok.len()..n - tok.len() + 1 } else { 0..n - tok.len() + 1 };
        let mut best: Option<(i64, usize)> = None;
        for i in range {
            if !tok.iter().enumerate().all(|(j, t)| equal(*t, cs[i + j].c)) {
                continue;
            }
            let mut score = EXACT + tok.len() as i64 * CONSECUTIVE;
            if prefix { score += PREFIX; }
            if suffix { score += SUFFIX; }
            if i == 0 { score += START; } else if boundary(i - 1) { score += BOUNDARY; }
            score -= leading(i);
            if !prefix && !suffix { score -= (n - i - tok.len()) as i64; }
            if best.is_none_or(|(s, _)| score > s) { best = Some((score, i)); }
        }
        best
    };
    let fuzzy = |tok: &[char]| -> Option<(i64, Vec<usize>)> {
        if tok.is_empty() || n == 0 {
            return None;
        }
        // The first subsequence from the start, then compacted backwards.
        let mut pos = Vec::with_capacity(tok.len());
        let mut ci = 0;
        for t in tok {
            while ci < n && !equal(*t, cs[ci].c) { ci += 1; }
            if ci == n { return None; }
            pos.push(ci);
            ci += 1;
        }
        let mut ci = pos[tok.len() - 1];
        for pi in (0..tok.len()).rev() {
            while !equal(tok[pi], cs[ci].c) { ci -= 1; }
            pos[pi] = ci;
            if pi != 0 { ci -= 1; }
        }
        let mut score = if pos[0] == 0 { START } else { (if boundary(pos[0] - 1) { BOUNDARY } else { 0 }) - leading(pos[0]) };
        for w in pos.windows(2) {
            if w[1] == w[0] + 1 { score += CONSECUTIVE; } else if boundary(w[1] - 1) { score += BOUNDARY; }
        }
        score -= (pos[tok.len() - 1] - pos[0] + 1 - tok.len()) as i64;
        Some((score, pos))
    };

    let mut best: Option<(i64, Vec<usize>)> = None;
    for group in pattern.split('|') {
        let terms: Vec<&str> = group.split(' ').filter(|t| !t.is_empty()).collect();
        if terms.is_empty() {
            continue;
        }
        let mut score = 0;
        let mut matched = Vec::new();
        let ok = terms.iter().all(|term| {
            let inverse = term.starts_with('!');
            let t = if inverse { &term[1..] } else { term };
            let (quoted, prefix) = (t.starts_with('\''), t.starts_with('^'));
            let t = if quoted || prefix { &t[1..] } else { t };
            let suffix = t.ends_with('$') && !t.is_empty();
            let t = if suffix { &t[..t.len() - 1] } else { t };
            if t.is_empty() {
                return false;
            }
            let tok: Vec<char> = t.chars().collect();
            let found = if inverse || quoted || prefix || suffix {
                exact(&tok, prefix, suffix).map(|(s, i)| (s, (i..i + tok.len()).collect()))
            } else {
                fuzzy(&tok)
            };
            match found {
                _ if inverse => found.is_none(),
                Some((s, pos)) => {
                    score += s;
                    matched.extend(pos);
                    true
                }
                None => false,
            }
        });
        if ok && best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, matched));
        }
    }
    let Some((_, matched)) = best else { return if positions { String::new() } else { "0".into() } };
    if !positions {
        return "1".into();
    }
    // Columns as format_draw_none lays out the alignment groups.
    let (mut wl, mut wc, mut wr) = (widths[0], widths[1], widths[2]);
    while wl + wc + wr > width {
        if wc > 0 { wc -= 1; } else if wr > 0 { wr -= 1; } else { wl -= 1; }
    }
    let wa = widths[3].min(width);
    let start = [0, wl + (width - wr - wl) / 2 - wc / 2, width - wr, (width - wa) / 2];
    let src = [0, widths[1] / 2 - wc / 2, widths[2] - wr, 0];
    let vis = [wl, wc, wr, wa];
    let mut columns = vec![false; width];
    for i in matched {
        let fc = &cs[i];
        let a = fc.align;
        if fc.offset < src[a] || fc.offset >= src[a] + vis[a] {
            continue;
        }
        let column = start[a] + fc.offset - src[a];
        for c in column..(column + fc.width).min(width) {
            columns[c] = true;
        }
    }
    columns.iter().enumerate().filter(|(_, on)| **on).map(|(i, _)| i.to_string()).collect::<Vec<_>>().join(",")
}

fn arith(args: &[String], a: &str, b: &str) -> Option<String> {
    let op = args.first()?.as_str();
    let float = args.get(1).is_some_and(|f| f.contains('f'));
    let prec = match args.get(2) {
        // A negative precision is the printf default.
        Some(p) => match strtonum(p).filter(|n| (-100..=100).contains(n))? { n if n < 0 => 6, n => n as usize },
        None => if float { 2 } else { 0 },
    };
    let mut x = strtod(a)?;
    let mut y = strtod(b)?;
    if !float {
        x = x.trunc();
        y = y.trunc();
    }
    let r = match op {
        "+" => x + y,
        "-" => x - y,
        "*" => x * y,
        "/" => x / y,
        "%" | "m" => x % y,
        "==" => ((x - y).abs() < 1e-9) as i32 as f64,
        "!=" => ((x - y).abs() > 1e-9) as i32 as f64,
        "<" => (x < y) as i32 as f64,
        ">" => (x > y) as i32 as f64,
        "<=" => (x <= y) as i32 as f64,
        ">=" => (x >= y) as i32 as f64,
        _ => return None,
    };
    if float {
        // As printf, without precision for inf and nan.
        return Some(match r {
            _ if r.is_finite() => format!("{r:.prec$}"),
            _ if r.is_nan() => (if r.is_sign_negative() { "-nan" } else { "nan" }).into(),
            _ => (if r > 0.0 { "inf" } else { "-inf" }).into(),
        });
    }
    // An integer result out of range (such as division by zero) is converted as x86 does.
    let r = if r.is_finite() && r.abs() < 9.2e18 { r.trunc() } else { i64::MIN as f64 };
    Some(format!("{r:.prec$}"))
}

/// strtod(3) that must use the whole string, as format_replace_expression checks: leading
/// (not trailing) space, a sign, decimal or hexadecimal (0x, with p exponent) numbers, inf,
/// infinity and nan. An empty string is 0.
fn strtod(s: &str) -> Option<f64> {
    if s.is_empty() {
        return Some(0.0);
    }
    let t = s.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let lower = body.to_ascii_lowercase();
    // Length of digits[.digits] (at least one digit) at the start of m.
    let mantissa = |m: &str, radix: u32| -> Option<usize> {
        let (mut digits, mut dot, mut n) = (0, false, 0);
        for c in m.chars() {
            match c {
                '.' if !dot => dot = true,
                c if c.is_digit(radix) => digits += 1,
                _ => break,
            }
            n += 1;
        }
        (digits > 0).then_some(n)
    };
    // An exponent: the marker, an optional sign and at least one digit.
    let exponent = |e: &str, marker: char| -> Option<i32> {
        let e = e.strip_prefix(marker)?;
        let digits = e.strip_prefix(['+', '-']).unwrap_or(e);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(e.parse::<i32>().unwrap_or(if e.starts_with('-') { -100000 } else { 100000 }))
    };
    let v = if lower == "inf" || lower == "infinity" {
        f64::INFINITY
    } else if lower == "nan" || (lower.starts_with("nan(") && lower.ends_with(')') && lower[4..lower.len() - 1].chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
        f64::NAN
    } else if let Some(n) = lower.strip_prefix("0x").and_then(|h| mantissa(h, 16)) {
        let h = &lower[2..];
        let (int, frac) = h[..n].split_once('.').unwrap_or((&h[..n], ""));
        let v = int.chars().chain(frac.chars()).fold(0f64, |v, c| v * 16.0 + c.to_digit(16).unwrap_or(0) as f64);
        let exp = if n < h.len() { exponent(&h[n..], 'p')? } else { 0 };
        v * 2f64.powi(exp.saturating_sub(4 * frac.len() as i32))
    } else {
        let n = mantissa(&lower, 10)?;
        if n < lower.len() {
            exponent(&lower[n..], 'e')?;
        }
        lower.parse::<f64>().ok()?
    };
    Some(if neg { -v } else { v })
}

/// A path separator for b and d: as POSIX basename(3) and dirname(3) only `/`, and on
/// Windows `\` too.
fn is_sep(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
}

fn basename(p: &str) -> String {
    let t = p.trim_end_matches(is_sep);
    if t.is_empty() {
        return if p.is_empty() { ".".into() } else { p[..1].to_string() };
    }
    t.rsplit(is_sep).next().unwrap_or(t).to_string()
}

fn dirname(p: &str) -> String {
    let t = p.trim_end_matches(is_sep);
    if t.is_empty() {
        return if p.is_empty() { ".".into() } else { p[..1].to_string() };
    }
    match t.rfind(is_sep) {
        None => ".".into(),
        Some(i) => {
            let d = t[..i].trim_end_matches(is_sep);
            if d.is_empty() { t[..1].to_string() } else { d.to_string() }
        }
    }
}

fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "|&;<>(){}$`\\\"'*?[# =%\n\t".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Style-aware width / trimming (`#[...]` is zero width, `##` is one column)
// ---------------------------------------------------------------------------------------------

enum Unit<'a> {
    Style(&'a str),
    Text(&'a str, usize),
}

fn units(s: &str) -> Vec<Unit<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if rest.starts_with("#[")
            && let Some(e) = rest.find(']')
        {
            out.push(Unit::Style(&rest[..=e]));
            i += e + 1;
        } else if rest.starts_with("##") {
            out.push(Unit::Text(&rest[..2], 1));
            i += 2;
        } else {
            let c = rest.chars().next().unwrap();
            let n = c.len_utf8();
            out.push(Unit::Text(&rest[..n], c.width().unwrap_or(0)));
            i += n;
        }
    }
    out
}

fn fmt_width(s: &str) -> usize {
    units(s).iter().map(|u| if let Unit::Text(_, w) = u { *w } else { 0 }).sum()
}

/// utf8_cstrwidth: the width of every printable character, styles and `##` included.
fn cstr_width(s: &str) -> usize {
    s.chars().map(|c| if c.is_ascii() { usize::from(c > '\x1f' && c != '\x7f') } else { c.width().unwrap_or(0) }).sum()
}

fn trim_left(s: &str, limit: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for u in units(s) {
        match u {
            Unit::Style(t) => out.push_str(t),
            Unit::Text(t, cw) => {
                if w + cw <= limit {
                    out.push_str(t);
                    w += cw;
                } else {
                    w = usize::MAX / 2;
                }
            }
        }
    }
    out
}

fn trim_right(s: &str, limit: usize) -> String {
    let total = fmt_width(s);
    let skip = total.saturating_sub(limit);
    let mut out = String::new();
    let mut w = 0;
    for u in units(s) {
        match u {
            Unit::Style(t) => out.push_str(t),
            Unit::Text(t, cw) => {
                if w >= skip {
                    out.push_str(t);
                }
                w += cw;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Globs and a small regex engine (no groups/alternation)
// ---------------------------------------------------------------------------------------------

fn ceq(a: char, b: char, icase: bool) -> bool {
    a == b || (icase && a.to_lowercase().eq(b.to_lowercase()))
}

/// Character ranges of a `[...]` class.
type Ranges = Vec<(char, char)>;

/// Parse a `[...]` class starting after the `[`; returns (ranges, negated, index after `]`).
fn parse_class(p: &[char], mut i: usize) -> Option<(Ranges, bool, usize)> {
    let neg = matches!(p.get(i), Some('!') | Some('^'));
    if neg {
        i += 1;
    }
    let mut ranges = Vec::new();
    let mut first = true;
    loop {
        let c = *p.get(i)?;
        if c == ']' && !first {
            return Some((ranges, neg, i + 1));
        }
        first = false;
        let lo = if c == '\\' {
            i += 1;
            *p.get(i)?
        } else {
            c
        };
        i += 1;
        if p.get(i) == Some(&'-') && p.get(i + 1).is_some_and(|&c| c != ']') {
            ranges.push((lo, p[i + 1]));
            i += 2;
        } else {
            ranges.push((lo, lo));
        }
    }
}

fn class_has(ranges: &[(char, char)], neg: bool, c: char, icase: bool) -> bool {
    let test = |c: char| ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi);
    let hit = test(c) || (icase && (c.to_lowercase().any(test) || c.to_uppercase().any(test)));
    hit != neg
}

/// fnmatch(3)-style glob: `*`, `?`, `[...]`, `\` escapes.
pub fn glob_match(pat: &str, text: &str, icase: bool) -> bool {
    if pat.len() > 16384 || text.len() > MAX_FORMAT_BYTES { return false; }
    enum Token { Star, Any, Class(Ranges, bool), Literal(char) }
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < p.len() {
        let token = match p[i] {
            '*' => Token::Star,
            '?' => Token::Any,
            '[' => match parse_class(&p, i + 1) {
                Some((r, neg, end)) => { i = end - 1; Token::Class(r, neg) }
                None => Token::Literal('['),
            },
            '\\' if i + 1 < p.len() => { i += 1; Token::Literal(p[i]) }
            c => Token::Literal(c),
        };
        tokens.push(token);
        i += 1;
    }
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut retry) = (None, 0);
    let mut work = MAX_FORMAT_WORK;
    while ti < t.len() {
        let cost = match tokens.get(pi) { Some(Token::Class(r, _)) => r.len().saturating_add(1), _ => 1 };
        if work < cost { return false; }
        work -= cost;
        let matched = match tokens.get(pi) {
            Some(Token::Star) => { star = Some(pi); retry = ti; pi += 1; continue; }
            Some(Token::Any) => true,
            Some(Token::Class(r, neg)) => class_has(r, *neg, t[ti], icase),
            Some(Token::Literal(c)) => ceq(*c, t[ti], icase),
            None => false,
        };
        if matched { pi += 1; ti += 1; }
        else if let Some(s) = star { retry += 1; ti = retry; pi = s + 1; }
        else { return false; }
    }
    while matches!(tokens.get(pi), Some(Token::Star)) { pi += 1; }
    pi == tokens.len()
}

/// tmux uses leftmost-longest matching and UTF-8 characters, including captures.
pub(crate) fn compile_regex(pattern: &str, ignore_case: bool) -> Option<fancy_regex::Regex> {
    // (?...) lookarounds/flags/named groups are not tmux's ERE syntax.
    let mut chars = pattern.chars().peekable();
    let mut class = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' => { chars.next(); }
            '[' => class = true,
            ']' => class = false,
            '(' if !class && chars.peek() == Some(&'?') => return None,
            _ => {}
        }
    }
    fancy_regex::RegexBuilder::new(pattern).case_insensitive(ignore_case)
        .dot_matches_new_line(true).leftmost_longest(true).build().ok()
}

pub fn regex_matches(pattern: &str, text: &str, ignore_case: bool) -> bool {
    compile_regex(pattern, ignore_case).is_some_and(|re| re.is_match(text).unwrap_or(false))
}

/// regsub: replace every match of `pattern` in `text`, with `\0` to `\9` for the groups.
/// Empty text stays empty and an empty pattern changes nothing. As in tmux, each search
/// starts afresh at the end of the last match (so `^` can match there), a pattern starting
/// with `^` replaces only the first match, an empty match right after another match is
/// skipped, and `\N` for a group that matched nothing is the digit itself.
fn regex_substitute(pattern: &str, replacement: &str, text: &str, ignore_case: bool) -> String {
    if text.is_empty() {
        return String::new();
    }
    if pattern.is_empty() {
        return text.to_string();
    }
    let Some(re) = compile_regex(pattern, ignore_case) else { return text.to_string() };
    let mut out = String::new();
    let expand = |out: &mut String, at: usize, captures: &fancy_regex::Captures<str>| {
        let mut chars = replacement.chars();
        while let Some(c) = chars.next() {
            let c = match c {
                '\\' => match chars.next() {
                    Some(d @ '0'..='9') => match captures.get(d as usize - '0' as usize).filter(|m| !m.as_str().is_empty()) {
                        Some(m) => {
                            out.push_str(&text[at + m.start()..at + m.end()]);
                            continue;
                        }
                        None => d,
                    },
                    Some(c) => c,
                    None => '\\',
                },
                c => c,
            };
            out.push(c);
        }
    };
    let (mut start, mut last, mut empty) = (0, 0, false);
    while start <= text.len() {
        let captures = match re.captures(&text[start..]) {
            Ok(Some(c)) => c,
            Ok(None) => {
                out.push_str(&text[last..]);
                break;
            }
            Err(_) => return text.to_string(),
        };
        let whole = captures.get(0).unwrap();
        let (so, eo) = (start + whole.start(), start + whole.end());
        out.push_str(&text[last..so]);
        if out.len() > MAX_FORMAT_BYTES { return String::new(); }
        if pattern.starts_with('^') {
            expand(&mut out, start, &captures);
            out.push_str(&text[eo..]);
            break;
        }
        if empty || so != last || so != eo {
            expand(&mut out, start, &captures);
            last = eo;
            start = eo;
            empty = false;
        } else {
            // An empty match where the last one ended: try again one character on.
            last = eo;
            start = eo + text[eo..].chars().next().map_or(1, char::len_utf8);
            empty = true;
        }
    }
    if out.len() > MAX_FORMAT_BYTES { return String::new(); }
    out
}

// ---------------------------------------------------------------------------------------------
// strftime
// ---------------------------------------------------------------------------------------------

const TIME_SPECS: &str = "aAbBcCdDeFgGhHIjklmMnprRsStTuUVwWxXyYzZP+";
const NUMERIC_SPECS: &str = "CdegGHIjklmMsSuUVwWyY";

/// Apply strftime(3) sequences. Unknown sequences are left as-is; `%%` is a literal `%`.
fn strftime(fmt: &str, t: &DateTime<Local>) -> String {
    let mut out = String::with_capacity(fmt.len());
    let mut it = fmt.char_indices().peekable();
    while let Some((start, c)) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut flag = None;
        if let Some(&(_, f)) = it.peek()
            && matches!(f, '-' | '_' | '0')
        {
            flag = Some(f);
            it.next();
        }
        if let Some(&(_, 'E' | 'O')) = it.peek() {
            it.next();
        }
        let Some((end, spec)) = it.next() else {
            out.push_str(&fmt[start..]);
            break;
        };
        let raw = &fmt[start..end + spec.len_utf8()];
        if spec == '%' && flag.is_none() {
            out.push('%');
        } else if TIME_SPECS.contains(spec) && (flag.is_none() || NUMERIC_SPECS.contains(spec)) {
            let f = match flag {
                Some(f) => format!("%{f}{spec}"),
                None => format!("%{spec}"),
            };
            out.push_str(&t.format(&f).to_string());
        } else {
            out.push_str(raw);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Align {
    Left,
    Centre,
    Right,
    AbsoluteCentre,
}

/// Theme colour names (colour.c): name, dark and light theme defaults (the 256-colour
/// values of the dark-theme-* and light-theme-* options) and the terminal colour used
/// when there is no theme.
const THEMES: [(&str, &str, &str, u8); 10] = [
    ("themeblack", "gray5", "gray10", 0),
    ("themewhite", "gray90", "gray95", 7),
    ("themelightgrey", "gray70", "gray80", 7),
    ("themedarkgrey", "gray15", "gray45", 0),
    ("themegreen", "yellowgreen", "seagreen", 2),
    ("themeyellow", "darkgoldenrod", "darkgoldenrod", 3),
    ("themered", "indianred", "indianred4", 1),
    ("themeblue", "skyblue3", "steelblue", 4),
    ("themecyan", "cadetblue", "darkcyan", 6),
    ("thememagenta", "mediumpurple", "purple4", 5),
];

/// Which colours theme names resolve to when there is no client theme to ask.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Dark,
    Light,
    /// The terminal's own colours (theme option "terminal").
    Terminal,
}

static DEFAULT_THEME: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Set the theme used for theme colour names in styles parsed without a client (default dark).
#[allow(dead_code)] // for the theme option
pub fn set_default_theme(theme: Theme) {
    DEFAULT_THEME.store(theme as u8, std::sync::atomic::Ordering::Relaxed);
}

fn default_theme() -> Theme {
    match DEFAULT_THEME.load(std::sync::atomic::Ordering::Relaxed) {
        1 => Theme::Light,
        2 => Theme::Terminal,
        _ => Theme::Dark,
    }
}

/// The colour of a theme name (themeblack, themewhite, themelightgrey, themedarkgrey,
/// themegreen, themeyellow, themered, themeblue, themecyan, thememagenta) in a theme.
pub fn theme_color(name: &str, theme: Theme) -> Option<Color> {
    let (_, dark, light, terminal) = THEMES.iter().find(|t| name.eq_ignore_ascii_case(t.0))?;
    match theme {
        Theme::Dark => parse_color(dark),
        Theme::Light => parse_color(light),
        Theme::Terminal => Some(Color::Idx(*terminal)),
    }
}

/// parse_color, resolving theme names through the client of a format context if any.
pub fn parse_color_with(s: &str, vars: Option<&dyn Vars>) -> Option<Color> {
    if THEMES.iter().any(|t| s.eq_ignore_ascii_case(t.0)) {
        return vars.and_then(|v| v.theme_color(&s.to_ascii_lowercase())).or_else(|| theme_color(s, default_theme()));
    }
    parse_color(s)
}

/// colour_fromstring: names, brightNAME, 0-7 and 90-97, colourN, #rrggbb, theme names,
/// grayN/greyN (0-100) and the X11 colour names. As tmux, the string is not trimmed, but the
/// numbers of colourN and greyN may have leading space (strtonum).
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.to_ascii_lowercase();
    const NAMES: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
    if s == "default" || s == "terminal" {
        return Some(Color::Default);
    }
    if THEMES.iter().any(|t| t.0 == s) {
        return theme_color(&s, default_theme());
    }
    if let Some(i) = NAMES.iter().position(|n| *n == s) {
        return Some(Color::Idx(i as u8));
    }
    if let Some(i) = s.strip_prefix("bright").and_then(|r| NAMES.iter().position(|n| *n == r)) {
        return Some(Color::Idx(8 + i as u8));
    }
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(hex, 16).ok()?;
        return Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
    }
    if let Some(num) = s.strip_prefix("colour").or_else(|| s.strip_prefix("color")) {
        return strtonum(num).filter(|n| (0..=255).contains(n)).map(|n| Color::Idx(n as u8));
    }
    match s.as_str() {
        "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" => return s.parse().ok().map(Color::Idx),
        "90" | "91" | "92" | "93" | "94" | "95" | "96" | "97" => return s.parse::<u8>().ok().map(|n| Color::Idx(n - 82)),
        _ => {}
    }
    if let Some(n) = s.strip_prefix("gray").or_else(|| s.strip_prefix("grey")) {
        let c = if n.is_empty() {
            0xbe
        } else {
            let n = strtonum(n).filter(|n| (0..=100).contains(n))?;
            (2.55 * n as f64).round() as u8
        };
        return Some(Color::Rgb(c, c, c));
    }
    X11_COLOURS.iter().find(|(name, _)| name.eq_ignore_ascii_case(&s)).map(|(_, c)| Color::Rgb((c >> 16) as u8, (c >> 8) as u8, *c as u8))
}

const ATTRS: [(&str, u8); 15] = [
    ("bright", cells::BOLD),
    ("bold", cells::BOLD),
    ("dim", cells::DIM),
    ("underscore", cells::UNDERLINE),
    ("double-underscore", cells::UNDERLINE),
    ("curly-underscore", cells::UNDERLINE),
    ("dotted-underscore", cells::UNDERLINE),
    ("dashed-underscore", cells::UNDERLINE),
    ("blink", cells::BLINK),
    ("reverse", cells::INVERSE),
    ("italics", cells::ITALIC),
    ("strikethrough", cells::STRIKE),
    // Accepted but not representable in Style.
    ("hidden", 0),
    ("overline", 0),
    ("acs", 0),
];

/// Apply one style word. Returns Err for unknown words or bad values. Theme colours are
/// resolved through `vars` when given.
fn apply_word(word: &str, st: &mut Style, align: &mut Align, base: Style, vars: Option<&dyn Vars>) -> Result<(), String> {
    let w = word.to_ascii_lowercase();
    if w.is_empty() {
        return Ok(());
    }
    if let Some((key, val)) = w.split_once('=') {
        let color = || parse_color_with(val, vars).ok_or_else(|| format!("bad colour: {val}"));
        match key {
            // fg=default and bg=default are the base colours.
            "fg" => st.fg = if val == "default" { base.fg } else { color()? },
            "bg" => st.bg = if val == "default" { base.bg } else { color()? },
            "us" | "fill" => {
                color()?;
            }
            "align" => {
                *align = match val {
                    "left" => Align::Left,
                    "centre" => Align::Centre,
                    "absolute-centre" => Align::AbsoluteCentre,
                    "right" => Align::Right,
                    _ => return Err(format!("bad align: {val}")),
                }
            }
            "dim" => {
                parse_dim(val)?;
            }
            // Kept by spans and parse_style_with; Style has no room for it.
            "link" => {}
            "list" => {
                if !["on", "focus", "left-marker", "right-marker"].contains(&val) {
                    return Err(format!("bad list: {val}"));
                }
            }
            "range" => {
                // As style_parse, an empty argument after `|` is an error and an unknown kind
                // is accepted but changes nothing.
                let (kind, arg) = val.split_once('|').map_or((val, None), |(k, a)| (k, Some(a)));
                if val.is_empty() || arg == Some("") {
                    return Err(format!("bad range: {val}"));
                }
                if ["left", "right", "control", "pane", "window", "session", "user"].contains(&kind) {
                    Range::parse(val)?;
                }
            }
            "width" | "pad" => {
                // width=N or width=N% (at most 100%).
                let pct = if key == "width" { val.strip_suffix('%').filter(|v| !v.is_empty()) } else { None };
                let n: u32 = pct.unwrap_or(val).parse().map_err(|_| format!("bad {key}: {val}"))?;
                if pct.is_some() && n > 100 {
                    return Err(format!("bad width: {val}"));
                }
            }
            _ => return Err(format!("unknown style: {word}")),
        }
        return Ok(());
    }
    match w.as_str() {
        "default" => *st = base,
        "none" => st.attrs = 0,
        "push-default" | "pop-default" | "set-default" | "nolist" | "norange" | "nolink" | "ignore" | "noignore" | "noattr" => {}
        "noalign" => *align = Align::Left,
        _ => {
            if let Some(&(_, bit)) = ATTRS.iter().find(|(n, _)| *n == w) {
                st.attrs |= bit;
            } else if let Some(&(_, bit)) = w.strip_prefix("no").and_then(|r| ATTRS.iter().find(|(n, _)| *n == r)) {
                st.attrs &= !bit;
            } else {
                return Err(format!("unknown style: {word}"));
            }
        }
    }
    Ok(())
}

/// dim=N or dim=N%, 0 to 100.
fn parse_dim(val: &str) -> Result<u8, String> {
    val.strip_suffix('%').unwrap_or(val).parse::<u8>().ok().filter(|n| *n <= 100).ok_or_else(|| format!("bad dim: {val}"))
}

/// The words of a style, split at style_parse's delimiters (space, comma and newline).
fn style_words(s: &str) -> impl Iterator<Item = &str> {
    s.split([',', ' ', '\n']).filter(|w| !w.is_empty())
}

/// Parse a style string like "fg=red,bg=colour235,bold,noitalics" applying it on top of `base`.
/// `default` resets to `base`. `align=`, `fill=`, `list=` etc are validated but have no effect.
pub fn parse_style(s: &str, base: Style) -> Result<Style, String> {
    parse_style_with(s, base, None).map(|p| p.style)
}

/// A parsed style with the parts Style cannot hold.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedStyle {
    pub style: Style,
    /// dim= percentage (tmux dims pane colours by it for window-style and window-active-style;
    /// see dim_color).
    pub dim: u8,
    /// link= hyperlink URI.
    pub link: Option<String>,
}

/// Parse a style with a format context, as tmux does for style options: a style containing
/// `#{` is expanded as a format first (so colours can be formats), and theme colour names
/// resolve through the context's client.
pub fn parse_style_with(s: &str, base: Style, vars: Option<&dyn Vars>) -> Result<ParsedStyle, String> {
    let expanded = vars.filter(|_| s.contains("#{")).map(|v| expand(s, v));
    let s = expanded.as_deref().unwrap_or(s);
    let mut result = ParsedStyle { style: base, dim: 0, link: None };
    let mut align = Align::Left;
    for word in style_words(s) {
        apply_word(word, &mut result.style, &mut align, base, vars)?;
        let (key, value) = word.split_once('=').unwrap_or((word, ""));
        if key.eq_ignore_ascii_case("link") && word.contains('=') {
            result.link = Some(value.to_string()).filter(|v| !v.is_empty());
        } else if word.eq_ignore_ascii_case("nolink") || word.eq_ignore_ascii_case("default") {
            result.link = None;
        } else if key.eq_ignore_ascii_case("dim") && word.contains('=') {
            result.dim = parse_dim(value)?;
        }
    }
    Ok(result)
}

// ---------------------------------------------------------------------------------------------
// Spans and layout
// ---------------------------------------------------------------------------------------------

/// A mouse range set with `#[range=...]` (tmux style ranges), used to find what a click
/// on a status line hit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Range {
    Left,
    Right,
    Window(u32),
    Pane(u32),
    Session(u32),
    User(String),
    /// control|0 to control|9 (3.8), as in the default pane-border-format.
    Control(u32),
}

impl Range {
    /// Parses the value of `range=`: left, right, window|N, pane|%N, session|$N, user|X,
    /// control|N (N from 0 to 9).
    /// `Ok(None)` is tmux's STYLE_RANGE_NONE.
    pub fn parse(v: &str) -> Result<Option<Range>, String> {
        let (kind, arg) = match v.split_once('|') {
            Some((k, a)) => (k, Some(a)),
            None => (v, None),
        };
        let num = |a: Option<&str>, prefix: &str| -> Result<u32, String> {
            let a = a.ok_or_else(|| format!("bad range: {v}"))?;
            a.strip_prefix(prefix).unwrap_or(a).parse().map_err(|_| format!("bad range: {v}"))
        };
        Ok(Some(match kind.to_ascii_lowercase().as_str() {
            "left" if arg.is_none() => Range::Left,
            "right" if arg.is_none() => Range::Right,
            "window" => Range::Window(num(arg, "@")?),
            "pane" => Range::Pane(num(arg, "%")?),
            "session" => Range::Session(num(arg, "$")?),
            "control" => Range::Control(arg.and_then(strtonum).filter(|n| (0..=9).contains(n)).ok_or_else(|| format!("bad range: {v}"))? as u32),
            "user" => match arg {
                Some(a) if !a.is_empty() => Range::User(a.to_string()),
                _ => return Err(format!("bad range: {v}")),
            },
            "none" if arg.is_none() => return Ok(None),
            _ => return Err(format!("bad range: {v}")),
        }))
    }

    pub fn name(&self) -> &'static str {
        match self {
            Range::Left => "left",
            Range::Right => "right",
            Range::Window(_) => "window",
            Range::Pane(_) => "pane",
            Range::Session(_) => "session",
            Range::User(_) => "user",
            Range::Control(_) => "control",
        }
    }
}

/// A run of text with one style, as produced from embedded `#[...]` style directives.
#[derive(Clone, PartialEq, Debug)]
pub struct Span {
    pub text: String,
    /// URI from link=, retained for renderers which support OSC 8.
    pub link: Option<String>,
    pub style: Style,
    pub align: Align,
    /// Mouse range. As in tmux, a range only counts once a later `#[...]` ends it.
    pub range: Option<Range>,
    /// The last span of a range: tmux's range then covers one more cell.
    pub range_end: bool,
}

/// Split already-expanded text into styled spans: `#[fg=red]a#[default]b`.
/// `#[default]` resets to the current default (initially `base`), `#[push-default]` makes the
/// current style the default, `#[pop-default]` restores the previous one. A directive with an
/// invalid word is ignored.
pub fn spans(text: &str, base: Style) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut cur = base;
    let mut link = None;
    let mut default = base;
    let mut defaults: Vec<Style> = Vec::new();
    let mut align = Align::Left;
    let mut range: Option<Range> = None;
    let mut range_start = 0;
    let mut buf = String::new();
    // list=left-marker/right-marker text is only shown by tmux when the list is
    // cut; lists are never cut here, so it is dropped
    let mut in_marker = false;

    fn flush(out: &mut Vec<Span>, buf: &mut String, style: Style, align: Align, range: &Option<Range>, link: &Option<String>) {
        if buf.is_empty() {
            return;
        }
        if let Some(last) = out.last_mut()
            && last.style == style
            && last.link == *link
            && last.align == align
            && last.range == *range
            && !last.range_end
        {
            last.text.push_str(buf);
        } else {
            out.push(Span { text: buf.clone(), link: link.clone(), style, align, range: range.clone(), range_end: false });
        }
        buf.clear();
    }

    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with("##") {
            if !in_marker {
                buf.push('#');
            }
            i += 2;
        } else if rest.starts_with("#[")
            && let Some(e) = rest.find(']')
        {
            // As format_draw, a directive with any invalid word is ignored as a whole.
            let (mut st, mut al) = (cur, align);
            if !style_words(&rest[2..e]).all(|w| apply_word(w, &mut st, &mut al, default, None).is_ok()) {
                i += e + 1;
                continue;
            }
            flush(&mut out, &mut buf, cur, align, &range, &link);
            let before = range.clone();
            let saved = cur;
            for w in style_words(&rest[2..e]) {
                let lower = w.to_ascii_lowercase();
                match lower.as_str() {
                    // link= continues until nolink, default or an empty link=.
                    "nolink" => link = None,
                    _ if lower.starts_with("link=") => link = Some(w[5..].to_string()).filter(|l| !l.is_empty()),
                    // The style before this directive becomes the default.
                    "set-default" => {
                        defaults.clear();
                        default = saved;
                    }
                    "push-default" => {
                        defaults.push(default);
                        default = cur;
                    }
                    "pop-default" => default = defaults.pop().unwrap_or(base),
                    "norange" => range = None,
                    "list=left-marker" | "list=right-marker" => in_marker = true,
                    "list=on" | "list=focus" | "nolist" => in_marker = false,
                    _ if lower.starts_with("range=") => {
                        if let Ok(r) = Range::parse(&w[6..]) {
                            range = r;
                        }
                    }
                    "default" => {
                        link = None;
                        range = None;
                        let _ = apply_word(w, &mut cur, &mut align, default, None);
                    }
                    _ => {
                        let _ = apply_word(w, &mut cur, &mut align, default, None);
                    }
                }
            }
            // Control ranges all match (format_is_type), so a new one continues the open one.
            if matches!((&before, &range), (Some(Range::Control(_)), Some(Range::Control(_)))) {
                range = before.clone();
            }
            if range != before {
                // the open range ends here (format_draw adds it to the list)
                if before.is_some() && out.len() > range_start {
                    out.last_mut().unwrap().range_end = true;
                }
                range_start = out.len();
            }
            i += e + 1;
        } else {
            let c = rest.chars().next().unwrap();
            if !in_marker {
                buf.push(c);
            }
            i += c.len_utf8();
        }
    }
    flush(&mut out, &mut buf, cur, align, &range, &link);
    // a range still open at the end is dropped, as tmux does
    if range.is_some() {
        for sp in &mut out[range_start..] {
            sp.range = None;
        }
    }
    out
}

/// Terminal display width of a string.
pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Cells for a group of spans (wide characters occupy a lead cell plus an empty
/// continuation) and the mouse range of every cell.
fn group_cells_ranged<'a>(spans: impl Iterator<Item = &'a Span>) -> (Vec<Cell>, Vec<Option<Range>>) {
    let mut cells: Vec<Cell> = Vec::new();
    let mut tags: Vec<Option<Range>> = Vec::new();
    let mut extend: Option<Range> = None;
    for sp in spans {
        let tag = sp.range.clone();
        for c in sp.text.chars() {
            if c.is_control() {
                continue;
            }
            match c.width() {
                Some(0) | None => {
                    if let Some(last) = cells.iter_mut().rev().find(|c| !c.is_continuation()) {
                        last.text.push(c);
                    }
                }
                Some(w) => {
                    cells.push(Cell { text: c.to_string(), style: sp.style });
                    // a range that ended just before covers this cell too
                    tags.push(extend.take().or_else(|| tag.clone()));
                    for _ in 1..w {
                        cells.push(Cell { text: String::new(), style: sp.style });
                        tags.push(tag.clone());
                    }
                }
            }
        }
        if sp.range_end {
            extend = sp.range.clone();
        }
    }
    (cells, tags)
}

/// Copy `cells[from..from+n]` to `out[at..]`, blanking wide characters cut at either edge.
fn put(out: &mut [Cell], at: usize, cells: &[Cell], from: usize, n: usize, fill: Style) {
    let blank = Cell { text: " ".into(), style: fill };
    for k in 0..n {
        let src = &cells[from + k];
        let cut_start = k == 0 && src.is_continuation();
        let cut_end = k == n - 1 && cells.get(from + k + 1).is_some_and(Cell::is_continuation);
        out[at + k] = if cut_start || cut_end { blank.clone() } else { src.clone() };
    }
}

/// Lay spans out into exactly `width` cells: left group at the start, right group at the end,
/// centre group halfway between them. When too wide, the centre shrinks first, then the right,
/// then the left (as tmux does); wide characters are never split.
pub fn layout_line(spans: &[Span], width: usize, fill: Style) -> Vec<Cell> {
    layout_line_ranged(spans, width, fill).0
}

/// [`layout_line`] plus the mouse range (`#[range=...]`) under every cell.
pub fn layout_line_ranged(spans: &[Span], width: usize, fill: Style) -> (Vec<Cell>, Vec<Option<Range>>) {
    let mut out = vec![Cell { text: " ".into(), style: fill }; width];
    let mut tags: Vec<Option<Range>> = vec![None; width];
    let group = |a: Align| group_cells_ranged(spans.iter().filter(move |s| s.align == a));
    let ((left, ltags), (centre, ctags), (right, rtags)) = (group(Align::Left), group(Align::Centre), group(Align::Right));
    let (mut lw, mut cw, mut rw) = (left.len(), centre.len(), right.len());
    while lw + cw + rw > width {
        if cw > 0 {
            cw -= 1;
        } else if rw > 0 {
            rw -= 1;
        } else {
            lw -= 1;
        }
    }
    if lw > 0 {
        put(&mut out, 0, &left, 0, lw, fill);
        tags[..lw].clone_from_slice(&ltags[..lw]);
    }
    if rw > 0 {
        put(&mut out, width - rw, &right, right.len() - rw, rw, fill);
        tags[width - rw..].clone_from_slice(&rtags[right.len() - rw..]);
    }
    if cw > 0 {
        let at = lw + (width - rw - lw) / 2 - cw / 2;
        let from = centre.len() / 2 - cw / 2;
        put(&mut out, at, &centre, from, cw, fill);
        tags[at..at + cw].clone_from_slice(&ctags[from..from + cw]);
    }
    let (absolute, atags) = group(Align::AbsoluteCentre);
    let aw = absolute.len().min(width);
    if aw > 0 {
        let at = (width - aw) / 2;
        put(&mut out, at, &absolute, 0, aw, fill);
        tags[at..at + aw].clone_from_slice(&atags[..aw]);
    }
    (out, tags)
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cells::{BLINK, BOLD, DIM, INVERSE, ITALIC, STRIKE, UNDERLINE};

    #[test]
    fn mouse_ranges_follow_tmux_format_draw() {
        let base = Style::default();
        let tags = |s: &str| layout_line_ranged(&spans(s, base), 8, base).1;
        let w = |n| Some(Range::Window(n));
        // a closed range covers one cell more; one still open at the end counts for nothing
        assert_eq!(tags("#[range=window|3]ab#[norange]cd#[range=left]ef"), vec![w(3), w(3), w(3), None, None, None, None, None]);
        // #[default] ends a range too; where ranges overlap the earlier one wins
        assert_eq!(tags("#[range=window|1]a#[default]#[range=window|2]b#[norange]"), vec![w(1), w(1), None, None, None, None, None, None]);
        assert_eq!(tags("#[range=window|1]a#[default]x#[range=window|2]b#[norange]y"), vec![w(1), w(1), w(2), w(2), None, None, None, None]);
        // ranges keep to their alignment group
        let right = tags("x#[align=right]#[range=right]RR#[norange]");
        assert_eq!(right[6..], [Some(Range::Right), Some(Range::Right)]);
        assert_eq!(right[..6], [None, None, None, None, None, None]);
        assert_eq!(Range::parse("user|abc"), Ok(Some(Range::User("abc".into()))));
        assert_eq!(Range::parse("pane|%7"), Ok(Some(Range::Pane(7))));
        assert_eq!(Range::parse("session|$2"), Ok(Some(Range::Session(2))));
        assert_eq!(Range::parse("none"), Ok(None));
        assert!(Range::parse("window").is_err());
    }

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn x(t: &str, v: &[(&str, &str)]) -> String {
        expand(t, &vars(v))
    }

    fn fixed_time() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 9, 7, 5, 3).single().unwrap()
    }

    // ----- basic expansion -----

    #[test]
    fn variables_and_unknown() {
        let v = [("session_name", "main"), ("window_index", "3")];
        assert_eq!(x("[#{session_name}] ", &v), "[main] ");
        assert_eq!(x("#{window_index}:#{nope}.", &v), "3:.");
        assert_eq!(x("plain text", &v), "plain text");
        assert_eq!(x("ünï #{session_name} 漢字", &v), "ünï main 漢字");
    }

    #[test]
    fn short_aliases() {
        let v = [
            ("host", "box.example.com"),
            ("pane_id", "%1"),
            ("window_flags", "*"),
            ("window_index", "0"),
            ("pane_index", "2"),
            ("session_name", "s"),
            ("pane_title", "t"),
            ("window_name", "w"),
        ];
        assert_eq!(x("#H|#h|#D|#F|#I|#P|#S|#T|#W", &v), "box.example.com|box|%1|*|0|2|s|t|w");
        assert_eq!(x("#h", &[("host", "a"), ("host_short", "b")]), "b");
    }

    #[test]
    fn escapes() {
        // As format_expand1: only #, and #} are single escapes, #: stays, a trailing # is
        // dropped and a run of # before [ is kept whole for the style.
        assert_eq!(x("a##b #, #} #:", &[]), "a#b , } #:");
        assert_eq!(x("#", &[]), "");
        assert_eq!(x("x#", &[]), "x");
        assert_eq!(x("#x#", &[]), "#x");
        assert_eq!(x("#####", &[]), "##");
        assert_eq!(x("###a", &[]), "##a");
        assert_eq!(x("###[x]|####[x]|#####[x]", &[]), "###[x]|####[x]|#####[x]");
        assert_eq!(x("#z", &[]), "#z");
        // `##[` stays escaped for the style parser.
        assert_eq!(x("##[x]", &[]), "##[x]");
        assert_eq!(x("#[fg=red]a", &[]), "#[fg=red]a");
        assert_eq!(x("#[fg=#{c}]a", &[("c", "blue")]), "#[fg=blue]a");
    }

    #[test]
    fn shell_command_is_empty() {
        assert_eq!(x("a#(echo (hi))b", &[]), "ab");
        assert_eq!(x("a#(unterminated", &[]), "a");
    }

    #[test]
    fn unterminated_brace_stops_expansion() {
        assert_eq!(x("a#{session_name", &[("session_name", "s")]), "a");
    }

    // ----- conditionals -----

    #[test]
    fn conditionals() {
        let v = [("one", "1"), ("zero", "0"), ("empty", ""), ("name", "x")];
        assert_eq!(x("#{?one,yes,no}", &v), "yes");
        assert_eq!(x("#{?zero,yes,no}", &v), "no");
        assert_eq!(x("#{?empty,yes,no}", &v), "no");
        assert_eq!(x("#{?missing,yes,no}", &v), "no");
        assert_eq!(x("#{?name,yes,no}", &v), "yes");
        assert_eq!(x("#{?one,yes}", &v), "yes");
        assert_eq!(x("#{?zero,yes}", &v), "");
        // Nested formats as condition and branches; commas in nested braces don't split.
        assert_eq!(x("#{?#{==:#{name},x},#{?one,a#,b,c},d}", &v), "a,b");
        assert_eq!(x("#{?zero,#{?one,a,b},#{name}}", &v), "x");
        // Condition that is a literal format.
        assert_eq!(x("#{?#{name},y,n}", &v), "y");
        assert_eq!(x("#{?#{missing},y,n}", &v), "n");
        // Chained conditions (tmux 3.2+).
        assert_eq!(x("#{?zero,a,one,b,c}", &v), "b");
        assert_eq!(x("#{?zero,a,empty,b,c}", &v), "c");
    }

    #[test]
    fn conditional_with_style() {
        let t = "#{?window_zoomed_flag,#[fg=red]Z,}";
        assert_eq!(x(t, &[("window_zoomed_flag", "1")]), "#[fg=red]Z");
        assert_eq!(x(t, &[("window_zoomed_flag", "0")]), "");
        let sp = spans(&x(t, &[("window_zoomed_flag", "1")]), Style::default());
        assert_eq!(sp.len(), 1);
        assert_eq!(sp[0].text, "Z");
        assert_eq!(sp[0].style.fg, Color::Idx(1));
    }

    // ----- comparisons / logic -----

    #[test]
    fn comparisons() {
        let v = [("a", "abc"), ("b", "abd"), ("one", "1"), ("zero", "0")];
        assert_eq!(x("#{==:#{a},abc}", &v), "1");
        assert_eq!(x("#{==:#{a},#{b}}", &v), "0");
        assert_eq!(x("#{!=:#{a},#{b}}", &v), "1");
        assert_eq!(x("#{<:#{a},#{b}}", &v), "1");
        assert_eq!(x("#{>:#{a},#{b}}", &v), "0");
        assert_eq!(x("#{<=:#{a},abc}", &v), "1");
        assert_eq!(x("#{>=:#{a},abd}", &v), "0");
        assert_eq!(x("#{||:#{zero},#{one}}", &v), "1");
        assert_eq!(x("#{||:#{zero},}", &v), "0");
        assert_eq!(x("#{&&:#{one},#{a}}", &v), "1");
        assert_eq!(x("#{&&:#{one},#{zero}}", &v), "0");
        assert_eq!(x("#{!:#{zero}}", &v), "1");
        assert_eq!(x("#{!:#{one}}", &v), "0");
        assert_eq!(x("#{!!:#{a}}", &v), "1");
        assert_eq!(x("#{==:a,b,c}", &v), "0"); // right operand is "b,c"
    }

    // ----- modifiers -----

    #[test]
    fn parse_modifier_lists() {
        let (m, rest) = parse_modifiers("=10;b:pane_current_path").unwrap();
        assert_eq!(rest, "pane_current_path");
        assert_eq!(m, vec![Modifier { name: "=", args: vec!["10".into()] }, Modifier { name: "b", args: vec![] }]);
        let (m, rest) = parse_modifiers("s/a/b/:x").unwrap();
        assert_eq!((m[0].args.clone(), rest), (vec!["a".to_string(), "b".to_string()], "x"));
        let (m, _) = parse_modifiers("e|*|f|4:1,2").unwrap();
        assert_eq!(m[0].args, vec!["*", "f", "4"]);
        let (m, _) = parse_modifiers("s/a//:x").unwrap();
        assert_eq!(m[0].args, vec!["a", ""]);
        assert!(parse_modifiers("session_name").is_none());
        assert!(parse_modifiers("pane_current_path").is_none());
        assert!(parse_modifiers("?a,b,c").is_none());
    }

    #[test]
    fn truncate() {
        let v = [("t", "abcdefghij"), ("w", "漢字漢字")];
        assert_eq!(x("#{=3:t}", &v), "abc");
        assert_eq!(x("#{=-3:t}", &v), "hij");
        assert_eq!(x("#{=20:t}", &v), "abcdefghij");
        assert_eq!(x("#{=/3/...:t}", &v), "abc...");
        assert_eq!(x("#{||:0,0,1}", &v), "1");
        assert_eq!(x("#{||:0,,0}", &v), "0");
        assert_eq!(x("#{&&:1,1,1}", &v), "1");
        assert_eq!(x("#{&&:1,0,1}", &v), "0");
        assert_eq!(x("#{=/-3/...:t}", &v), "...hij");
        assert_eq!(x("#{=/20/...:t}", &v), "abcdefghij");
        // Display columns, never splitting a wide char.
        assert_eq!(x("#{=3:w}", &v), "漢");
        assert_eq!(x("#{=4:w}", &v), "漢字");
        assert_eq!(x("#{=-3:w}", &v), "字");
        // Style directives are zero width and kept.
        assert_eq!(x("#{=2:s}", &[("s", "#[fg=red]abc#[default]")]), "#[fg=red]ab#[default]");
    }

    #[test]
    fn padding() {
        let v = [("t", "ab"), ("w", "漢")];
        assert_eq!(x("#{p5:t}|", &v), "ab   |");
        assert_eq!(x("#{p-5:t}|", &v), "   ab|");
        assert_eq!(x("#{p 5:t}|", &v), "ab   |");
        assert_eq!(x("#{p1:t}|", &v), "ab|");
        assert_eq!(x("#{p4:w}|", &v), "漢  |");
        assert_eq!(x("#{p-4;=1:t}|", &v), "   a|");
    }

    #[test]
    fn excessive_format_allocations_are_rejected() {
        assert_eq!(x("#{p9223372036854775807:t}", &[("t", "x")]), "x");
        assert_eq!(x("#{p-9223372036854775808:t}", &[("t", "x")]), "x");
        assert_eq!(x("#{e|+|f|18446744073709551615:1,2}", &[]), "");
        assert_eq!(regex_substitute("a", &"x".repeat(1024), &"a".repeat(2048), false), "");
    }

    #[test]
    fn recursive_format_amplification_has_a_shared_work_limit() {
        let v = [("x", "#{E:x}#{E:x}")];
        assert!(x("#{E:x}", &v).len() <= MAX_FORMAT_BYTES);
    }

    #[test]
    fn adversarial_globs_do_not_recurse_or_explode() {
        let literal = "a".repeat(8000);
        assert!(glob_match(&literal, &literal, false));
        assert!(!glob_match(&("*a".repeat(1000) + "b"), &("a".repeat(2000) + "c"), false));
    }

    #[test]
    fn literal() {
        assert_eq!(x("#{l:#{session_name}}", &[("session_name", "s")]), "#{session_name}");
        assert_eq!(x("#{l:abc}", &[]), "abc");
    }

    #[test]
    fn time_var() {
        let ts = fixed_time().timestamp().to_string();
        let v = [("created", ts.as_str())];
        assert_eq!(x("#{t:created}", &v), "Fri Oct  9 07:05:03 2026");
        assert_eq!(x("#{t/f/%%Y-%%m:created}", &v), "%Y-%m");
        assert_eq!(x("#{t/f/%Y-%m:created}", &v), "2026-10");
        assert_eq!(x("#{t:nope}", &v), "");
        assert!(!x("#{t/p:created}", &v).is_empty());
    }

    #[test]
    fn strftime_modifier() {
        let v = vars(&[("fmt", "%H:%M #{x}"), ("x", "y")]);
        let budget = std::cell::Cell::new(MAX_FORMAT_WORK);
        let e = Expander::top(&v, fixed_time(), None, &budget);
        assert_eq!(e.expand("#{T:fmt}", 0), "07:05 y");
        assert_eq!(e.expand("#{E:fmt}", 0), "%H:%M y");
    }

    #[test]
    fn basename_dirname() {
        let v = [("p", "/home/user/src/"), ("w", r"C:\Users\me\proj"), ("r", "/"), ("f", "file")];
        assert_eq!(x("#{b:p}", &v), "src");
        assert_eq!(x("#{d:p}", &v), "/home/user");
        // A backslash separates only on Windows, as POSIX basename and dirname on Unix.
        if cfg!(windows) {
            assert_eq!(x("#{b:w}", &v), "proj");
            assert_eq!(x("#{d:w}", &v), r"C:\Users\me");
        } else {
            assert_eq!(x("#{b:w}|#{d:w}", &v), r"C:\Users\me\proj|.");
            assert_eq!(x(r"#{b:x}|#{d:x}", &[("x", r"a\b")]), r"a\b|.");
        }
        assert_eq!(x("#{b:r}|#{d:r}", &v), "/|/");
        assert_eq!(x("#{b:f}|#{d:f}", &v), "file|.");
        assert_eq!(x("#{d:x}", &[("x", "/a")]), "/");
        // As tmux, b and d do not apply to a format inside: it is only expanded.
        assert_eq!(x("#{b:#{p}}|#{d:#{p}}", &v), "/home/user/src/|/home/user/src/");
        assert_eq!(x("#{q:#{q}}|#{t:#{n}}", &[("q", "a b"), ("n", "42")]), "a b|42");
    }

    #[test]
    fn chained_modifiers() {
        let v = [("pane_current_path", "/home/user/a-very-long-directory-name")];
        assert_eq!(x("#{=10;b:pane_current_path}", &v), "a-very-lon");
        assert_eq!(x("#{b;=-4:pane_current_path}", &v), "name");
        assert_eq!(x("#{n;b:pane_current_path}", &v), "26");
    }

    #[test]
    fn substitution() {
        let v = [("t", "hello world"), ("p", "/home/me/x"), ("d", "a.b.c")];
        assert_eq!(x("#{s/o/0/:t}", &v), "hell0 w0rld");
        assert_eq!(x("#{s/^hello/bye/:t}", &v), "bye world");
        assert_eq!(x("#{s/^world/bye/:t}", &v), "hello world");
        assert_eq!(x("#{s/world$/all/:t}", &v), "hello all");
        // `/` in the pattern splits arguments: pattern "^", replacement "home", flags "me".
        assert_eq!(x("#{s/^/home/me/~/:p}", &v), "home/home/me/x");
        assert_eq!(x("#{s|^/home/me|~|:p}", &v), "~/x");
        assert_eq!(x("#{s/\\./-/:d}", &v), "a-b-c");
        assert_eq!(x("#{s/./X/:d}", &v), "XXXXX");
        assert_eq!(x("#{s/[ab]/_/:d}", &v), "_._.c");
        assert_eq!(x("#{s/l+/L/:t}", &v), "heLo worLd");
        assert_eq!(x("#{s/HELLO/hi/i:t}", &v), "hi world");
        assert_eq!(x("#{s/o//:t}", &v), "hell wrld");
        assert_eq!(x("#{s/#{l:o}/#{t}/:d}", &v), "a.b.c");
    }

    #[test]
    fn regex_captures_longest_utf8_and_chaining() {
        let v = [("text", "abc-123 abc-456"), ("unicode", "가나")];
        assert_eq!(x(r"#{s/([a-z]+)-([0-9]+)/\2_\1/:text}", &v), "123_abc 456_abc");
        assert_eq!(x("#{s/a/A/;s/b/B/:text}", &v), "ABc-123 ABc-456");
        assert_eq!(x("#{s/(a|ab)/X/:#{l:ab}}", &[]), "X");
        assert_eq!(x("#{s/a*/X/:#{l:ab}}", &[]), "XbX");
        assert_eq!(x("#{s/./X/:unicode}", &v), "XX");
        assert_eq!(x("#{n:unicode}", &v), "6");
        assert_eq!(x("#{m/r:[[:digit:]]+,abc123}", &[]), "1");
        assert_eq!(x("before#{m/r:bad}after", &[]), "before", "format syntax errors stop expansion");
        assert!(!regex_matches("[", "text", false));
        assert!(!regex_matches("(?=a)", "a", false));
    }

    #[test]
    fn match_glob() {
        let v = [("name", "vim-main")];
        assert_eq!(x("#{m:vim*,#{name}}", &v), "1");
        assert_eq!(x("#{m:*main,#{name}}", &v), "1");
        assert_eq!(x("#{m:vi?-*,#{name}}", &v), "1");
        assert_eq!(x("#{m:emacs*,#{name}}", &v), "0");
        assert_eq!(x("#{m:[uv]im*,#{name}}", &v), "1");
        assert_eq!(x("#{m:VIM*,#{name}}", &v), "0");
        assert_eq!(x("#{m/i:VIM*,#{name}}", &v), "1");
        assert_eq!(x("#{m/r:^v.m-,#{name}}", &v), "1");
        assert_eq!(x("#{m/r:^main,#{name}}", &v), "0");
        assert!(glob_match("a\\*b", "a*b", false));
        assert!(!glob_match("a\\*b", "axb", false));
        assert!(glob_match("[!a]b", "cb", false));
    }

    #[test]
    fn c_modifier() {
        assert_eq!(x("#{C:foo}", &[]), "0");
    }

    #[test]
    fn arithmetic() {
        let v = [("a", "7"), ("b", "2")];
        assert_eq!(x("#{e|+|:#{a},#{b}}", &v), "9");
        assert_eq!(x("#{e|-|:#{a},#{b}}", &v), "5");
        assert_eq!(x("#{e|*|:#{a},#{b}}", &v), "14");
        assert_eq!(x("#{e|/|:#{a},#{b}}", &v), "3");
        assert_eq!(x("#{e|%|:#{a},#{b}}", &v), "1");
        assert_eq!(x("#{e|m|:#{a},#{b}}", &v), "1");
        assert_eq!(x("#{e|/|f|2:#{a},#{b}}", &v), "3.50");
        assert_eq!(x("#{e|*|f|4:5.5,3}", &v), "16.5000");
        // Division by zero as tmux 3.8 on x86.
        assert_eq!(x("#{e|/|:1,0}|#{e|/|f:1,0}|#{e|/|f:-1,0}", &v), "-9223372036854775808|inf|-inf");
        assert!(x("#{e|%|f:1,0}", &v).ends_with("nan"));
        assert_eq!(x("#{e|+|:x,1}", &v), "");
        assert_eq!(x("#{e|<|:1,2}", &v), "1");
        assert_eq!(x("#{e|==|:2,2}", &v), "1");
        assert_eq!(x("#{e|-|:1,5}", &v), "-4");
    }

    #[test]
    fn length_width_ascii_quote() {
        let v = [("w", "漢字ab"), ("q", "it's a $x")];
        assert_eq!(x("#{n:w}", &v), "8");
        assert_eq!(x("#{w:w}", &v), "6");
        assert_eq!(x("#{a:98}", &v), "b");
        assert_eq!(x("#{a:#{e|+|:60,5}}", &v), "A");
        assert_eq!(x("#{a:7}", &v), "");
        assert_eq!(x("#{q:q}", &v), r"it\'s\ a\ \$x");
    }

    #[test]
    fn recursion_is_bounded() {
        let v = [("loop", "#{E:loop}")];
        assert_eq!(x("#{E:loop}", &v), "");
    }

    // ----- strftime -----

    #[test]
    fn strftime_specs() {
        let t = fixed_time();
        assert_eq!(strftime("%H:%M:%S", &t), "07:05:03");
        assert_eq!(strftime("%d-%b-%y", &t), "09-Oct-26");
        assert_eq!(strftime("%Y/%m/%e", &t), "2026/10/ 9");
        assert_eq!(strftime("%a %A %b %B", &t), "Fri Friday Oct October");
        assert_eq!(strftime("%p %I %-d %-H", &t), "AM 07 9 7");
        assert_eq!(strftime("100%% %q %", &t), "100% %q %");
        assert!(!strftime("%Z", &t).is_empty());
        assert_eq!(strftime("%j %u %w", &t), "282 5 5");
    }

    #[test]
    fn expand_time_applies_strftime_first() {
        let v = vars(&[("pane_title", "50% done %H")]);
        let s = expand_time_at("#{pane_title} %H:%M", &v, fixed_time());
        assert_eq!(s, "50% done %H 07:05");
        // The public entry point works too.
        assert_eq!(expand_time("%%", &v), "%");
    }

    // ----- default tmux formats -----

    #[test]
    fn default_status_formats() {
        let v = vars(&[
            ("session_name", "0"),
            ("window_index", "1"),
            ("window_name", "pwsh"),
            ("window_flags", "*"),
            ("window_bigger", "0"),
            ("window_offset_x", "3"),
            ("window_offset_y", "4"),
            ("pane_title", "a pane title that is much too long"),
        ]);
        assert_eq!(expand_time("[#{session_name}] ", &v), "[0] ");
        let wsf = "#I:#W#{?window_flags,#{window_flags}, }";
        assert_eq!(expand(wsf, &v), "1:pwsh*");
        let mut v2 = v.clone();
        v2.insert("window_flags".into(), String::new());
        assert_eq!(expand(wsf, &v2), "1:pwsh ");

        let sr = "#{?window_bigger,[#{window_offset_x}#,#{window_offset_y}] ,}\"#{=21:pane_title}\" %H:%M %d-%b-%y";
        assert_eq!(expand_time_at(sr, &v, fixed_time()), "\"a pane title that is \" 07:05 09-Oct-26");
        v2.insert("window_bigger".into(), "1".into());
        v2.insert("pane_title".into(), "host".into());
        assert_eq!(expand_time_at(sr, &v2, fixed_time()), "[3,4] \"host\" 07:05 09-Oct-26");
    }

    // ----- colours and styles -----

    #[test]
    fn colors() {
        assert_eq!(parse_color("red"), Some(Color::Idx(1)));
        assert_eq!(parse_color("White"), Some(Color::Idx(7)));
        assert_eq!(parse_color("brightred"), Some(Color::Idx(9)));
        assert_eq!(parse_color("brightwhite"), Some(Color::Idx(15)));
        assert_eq!(parse_color("colour235"), Some(Color::Idx(235)));
        assert_eq!(parse_color("color0"), Some(Color::Idx(0)));
        assert_eq!(parse_color("colour256"), None);
        assert_eq!(parse_color("#ff8000"), Some(Color::Rgb(255, 128, 0)));
        assert_eq!(parse_color("#ff80"), None);
        assert_eq!(parse_color("default"), Some(Color::Default));
        assert_eq!(parse_color("terminal"), Some(Color::Default));
        assert_eq!(parse_color("12"), None);
        assert_eq!(parse_color("mauve"), None);
        assert_eq!(parse_color(""), None);
    }

    #[test]
    fn styles() {
        let base = Style { fg: Color::Idx(2), bg: Color::Idx(0), attrs: 0 };
        let s = parse_style("fg=red,bg=colour235,bold,noitalics", base).unwrap();
        assert_eq!(s, Style { fg: Color::Idx(1), bg: Color::Idx(235), attrs: BOLD });
        let s = parse_style("bold dim italics underscore blink reverse strikethrough", base).unwrap();
        assert_eq!(s.attrs, BOLD | DIM | ITALIC | UNDERLINE | BLINK | INVERSE | STRIKE);
        let s2 = parse_style("nobold,nodim,noitalics,nounderscore,noblink,noreverse,nostrikethrough", s).unwrap();
        assert_eq!(s2.attrs, 0);
        assert_eq!(parse_style("none", s).unwrap().attrs, 0);
        assert_eq!(parse_style("none", s).unwrap().fg, s.fg);
        assert_eq!(parse_style("fg=red,default", base).unwrap(), base);
        assert_eq!(parse_style("fg=default", base).unwrap().fg, base.fg);
        assert_eq!(parse_style("", base).unwrap(), base);
        assert_eq!(parse_style("bright,hidden,double-underscore", base).unwrap().attrs, BOLD | UNDERLINE);
        assert_eq!(parse_style("align=right,fill=blue,list=on,FG=Blue", base).unwrap().fg, Color::Idx(4));
        assert!(parse_style("fg=mauve", base).is_err());
        assert!(parse_style("sparkly", base).is_err());
        assert!(parse_style("align=diagonal", base).is_err());
        assert!(parse_style("frobnicate=1", base).is_err());
    }

    // Expected values below were checked against tmux 3.8 (display -p).

    #[test]
    fn modifier_arguments_and_limits() {
        assert_eq!(x("#{R:ab,3}|#{R:ab,0}|#{R:x,10001}", &[]), "ababab||");
        assert_eq!(x("#{R:#{l:x},#{e|+:1,2}}", &[]), "xxx");
        assert_eq!(x("#{R:x,65537}|#{R:ab,32769}", &[]), "|");
        assert_eq!(x("#{e|/|f|5:1,3}|#{e|+||3:1,2}|#{e|+|f|101:1,2}|#{e|+|f|-1:1,2}", &[]), "0.33333|3.000||3.000000");
        assert_eq!(x("#{e|/|f|100:1,2}", &[]), format!("0.5{}", "0".repeat(99)));
        assert_eq!(x("#{e|==|f:1,1.0000000001}|#{e|!=|f:1,1.0000000001}", &[]), "1.00|0.00");
        assert_eq!(x("#{e:v}|#{e|+|f|2|x:1,2}", &[("v", "x")]), "x|");
        assert_eq!(x("#{||:1}|#{&&:1}|#{||:}|#{&&:}", &[]), "1|1|0|0");
        assert_eq!(x("#{=0:v}|#{=10001:v}|#{p10001;n:v}|#{p-5:v}|#{p5:v}|", &[("v", "abc")]), "abc|abc|3|  abc|abc  |");
        assert_eq!(x("#{=-2;p5:v}x|#{p-5;=2:v}x", &[("v", "abcdefghij")]), "ij   x|   abx");
        assert_eq!(x("#{=/3/...:v}|#{=/-3/...:v}|#{=3:v}|#{=/3/...:s}", &[("v", "abcdefghij"), ("s", "abc")]), "abc...|...hij|abc|abc");
        assert_eq!(x("#{l:a#,b#:#}##}", &[]), "a,b:}#");
        // Arguments are expanded once; s/ expands its pattern and replacement again.
        assert_eq!(x("#{=/#{n}/:v}|#{s|a|#{l:X}|:v}|#{=#{n}:v}", &[("n", "2"), ("v", "abca")]), "ab|XbcX|=2:v");
        // A single argument may start with any non-punctuation character.
        assert_eq!(parse_modifiers("p 3:v").unwrap().0[0].args, vec![" 3".to_string()]);
        assert!(parse_modifiers("literal:value").is_none());
        assert!(parse_modifiers("b/arg:value").is_none());
    }

    #[test]
    fn modifier_precedence_follows_format_replace() {
        assert_eq!(x("#{a:65}|#{a:31}|#{l;c:red}", &[]), "A||red");
        assert_eq!(x("#{!;==:a,a}|#{!!:0}|#{!:}", &[]), "0|0|1");
        assert_eq!(x("#{b:p}|#{d:p}|#{b;d:p}", &[("p", "/a/b/c/")]), "c|/a/b|.");
        assert_eq!(x("#{b:#S}|#{b:x}|#{w:#[fg=red]ab}|#{=1:#[fg=red]ab}", &[("S", "s")]), "||0|");
        assert_eq!(x("#{session_name&}|#{?,a,b}|#{?z,a,b}|#{?unset,a,b,c}", &[("z", "0")]), "|b|b|");
        // I without c, e or f is an ordinary variable.
        assert_eq!(x("#{I/x:v}|#{I:v}", &[("v", "val")]), "val|val");
    }

    #[test]
    fn quoting_variants_and_time_expansion_order() {
        let v = [("v", "a'b #[red]"), ("hash", "#[fg=red]"), ("fmt", "%H #{hour}"), ("hour", "%H")];
        assert_eq!(x("#{q/s:v}|#{q/e:v}|#{q/a:v}|#{q:v}", &v), "'a'\\''b #[red]'|a'b ##[red]|\"a'b #[red]\"|a\\'b\\ \\#\\[red]");
        assert_eq!(x("#{q/e:hash}|#{q/h:hash}", &v), "##[fg=red]|##[fg=red]");
        assert_eq!(x("#{q/a:d}", &[("d", "say \"hi\" $HOME `x` ${y} $")]), "\"say \\\"hi\\\" \\$HOME `x` \\${y} $\"");
        assert_eq!(x("#{q/a:t}|#{q/a:s}|#{q/a:o}|#{q/a:h}|#{q/a:e}", &[("t", "tab\there"), ("s", "it\"s"), ("o", ";"), ("h", "~x"), ("e", "")]),
            "tab\\there|'it\"s'|\\;|\\~x|''");
        assert_eq!(x("#{q:nl}", &[("nl", "a{b}\n")]), "a\\{b\\}\\\n");
        let v = vars(&v);
        let budget = std::cell::Cell::new(MAX_FORMAT_WORK);
        let e = Expander::top(&v, fixed_time(), None, &budget);
        assert_eq!(e.expand("#{T:fmt}", 0), "07 %H");
        // Status and display formats apply strftime to nested formats too, as E: values.
        let v = vars(&[("fmt", "%H:%M"), ("pct", "%%")]);
        assert_eq!(expand_time_at("#{E:fmt}|#{fmt}|%%|#{E:pct}|#(x %H)", &v, fixed_time()), "07:05|%H:%M|%|%|");
    }

    #[test]
    fn relative_and_difference_time() {
        let now = fixed_time();
        let values = HashMap::from([("past".to_string(), (now.timestamp() - 90061).to_string()),
            ("future".into(), (now.timestamp() + 30).to_string()), ("zero".into(), "0".into()),
            ("recent".into(), (now.timestamp() - 86400 * 2).to_string()), ("month".into(), (now.timestamp() - 86400 * 40).to_string()),
            ("old".into(), (now.timestamp() - 86400 * 400).to_string())]);
        let budget = std::cell::Cell::new(MAX_FORMAT_WORK);
        let e = Expander::top(&values, now, None, &budget);
        assert_eq!(e.expand("#{t/d:past}|#{t/d:future}|#{t/r:past}|#{t/r:future}|#{t:zero}", 0), "90061|-30|1d1h||");
        // p wins over r, d and f.
        assert_eq!(e.expand("#{t/p:recent}|#{t/pr:recent}|#{t/p:future}|#{t/p:month}|#{t/p:old}", 0), "Wed07|Wed07|07:05|30Aug|Sep25");
        assert_eq!(relative_time(3661), "1h1m");
        assert_eq!(relative_time(3600), "1h");
        assert_eq!(relative_time(61), "1m1s");
        assert_eq!(relative_time(0), "0s");
    }

    struct LoopVars;
    impl Vars for LoopVars {
        fn get(&self, name: &str) -> Option<String> {
            (name == "client_session").then(|| "two".into())
        }
        fn contexts(&self, kind: char, _: &str) -> Vec<(Box<dyn Vars + '_>, bool)> {
            if kind == 'W' {
                vec![(Box::new(UserVars(vars(&[("window_index", "2"), ("window_name", "one")]))), true),
                    (Box::new(UserVars(vars(&[("window_index", "4"), ("window_name", "two"), ("@c", "red")]))), false)]
            } else if kind == 'S' {
                vec![(Box::new(vars(&[("session_name", "one")])), false), (Box::new(vars(&[("session_name", "two")])), false)]
            } else {
                Vec::new()
            }
        }
        fn entries(&self, kind: char, flags: &str) -> Vec<HashMap<String, String>> {
            match kind {
                'O' if flags == "gw" => vec![vars(&[("option_name", "@x"), ("option_value", "yes"), ("option_is_user", "1")])],
                'V' => vec![vars(&[("environ_name", "X"), ("environ_value", "42"), ("environ_hidden", "0"), ("environ_removed", "0")])],
                _ => Vec::new(),
            }
        }
        fn client_contexts(&self, _: &str) -> Vec<Box<dyn Vars + '_>> {
            vec![Box::new(vars(&[("client_name", "c1")])), Box::new(vars(&[("client_name", "c2")]))]
        }
        fn client_info(&self, kind: char, name: &str) -> Option<String> {
            Some(if kind == 'e' { (if name == "HOME" { "/home/me" } else { "" }).into() } else { bool_str(name == "RGB") })
        }
        fn shell(&self, command: &str) -> Option<String> {
            Some(format!("ran {command} #{{l:x}} #(y)"))
        }
    }

    /// Vars whose @ variables are window user options.
    struct UserVars(HashMap<String, String>);
    impl Vars for UserVars {
        fn get(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
        fn user_options(&self) -> Vec<(String, String)> {
            self.0.iter().filter(|(k, _)| k.starts_with('@')).map(|(k, v)| (k.clone(), v.clone())).collect()
        }
    }

    #[test]
    fn option_environment_client_loops_and_information() {
        assert_eq!(expand("#{O/gw:#{loop_index}:#{option_name}=#{option_value}/#{option_is_user}/#{loop_last_flag}}", &LoopVars), "0:@x=yes/1/1");
        assert_eq!(expand("#{V:#{environ_name}=#{environ_value}:#{environ_hidden}:#{environ_removed}}", &LoopVars), "X=42:0:0");
        assert_eq!(expand("#{L:#{client_name}:#{loop_index}:#{loop_last_flag};}", &LoopVars), "c1:0:0;c2:1:1;");
        assert_eq!(expand("#{I/f:RGB}|#{I/c:missing}|#{I/e:HOME}|#{I/fe:HOME}", &LoopVars), "1|0|/home/me|/home/me");
        assert_eq!(x("#{O:x}|#{V:x}|#{L:x}|#{I/f:RGB}", &[]), "|||");
        assert_eq!(expand("#{N:one}|#{N/w:missing}|#{N/s:two}|#{N/x:two}", &LoopVars), "1|0|1|");
        // The current session is the client's.
        assert_eq!(expand("#{S:#{session_name},*#{session_name}} ", &LoopVars), "one*two ");
    }

    #[test]
    fn window_loop_neighbours_and_indices() {
        assert_eq!(expand("#{W:#{loop_index}/#{loop_last_flag}/#{prev_window_index}/#{next_window_index}/#{window_after_active}/#{window_before_active}/#{next_window_active}/#{next_@c};}", &LoopVars),
            "0/0//4/0/0/0/red;1/1/2//1/0//;");
    }

    #[test]
    fn jobs_expand_their_output_without_jobs_or_animation() {
        assert_eq!(expand("#(echo #{A:a,b})", &LoopVars), "ran echo  x ");
        let budget = std::cell::Cell::new(MAX_FORMAT_WORK);
        assert_eq!(Expander::top(&LoopVars, fixed_time(), Some(0), &budget).expand("#(#{A:a,b})", 0), "ran  x ");
    }

    #[test]
    fn animation_status_only_count_and_raw_frames() {
        let at = |ms, f: &str| Expander::top(&LoopVars, fixed_time(), Some(ms), &std::cell::Cell::new(MAX_FORMAT_WORK)).expand(f, 0);
        assert_eq!(x("#{A:first,second}", &[]), "");
        assert_eq!(at(500, "#{A:first,second,third}"), "third");
        assert_eq!(at(500, "#{A2:first,second,third}"), "third");
        assert_eq!(at(500, "#{A3:first,second,third}"), "second");
        assert_eq!(at(500, "#{A0:first,second,third}|#{A101:a,b}"), "third|b");
        assert_eq!(at(500, "#{A:#{l:raw}}|#{A:}"), "#{l:raw}|");
        assert_eq!(at(0, "#{W:#{A:a,b}}"), "aa");
        assert_eq!(expand_status_time("#{A:frame}", &vars(&[])), "frame");
    }

    #[test]
    fn fuzzy_terms_positions_and_smart_case() {
        assert_eq!(x("#{m/z:fb,foo bar}|#{m/p:fb,foo bar}|#{m/z:FB,foo bar}", &[]), "1|0,4|0");
        assert_eq!(x("#{m/z:fb !baz,foo bar}|#{m/z:fb !bar,foo bar}", &[]), "1|0");
        assert_eq!(x("#{m/z:^foo bar$,foo bar}|#{m/z:'ob,foo bar}|#{m/z:missing | ^foo,foo bar}", &[]), "1|0|1");
        assert_eq!(x("#{m/p:ab,a--ab}|#{m/z:,}|#{m/p:,}", &[]), "3,4|1|");
        assert_eq!(x("#{m/p:ab,abc#[align=right]def}|#{m/p:ef,abc#[align=right]def}", &[]), "0,1|4,5");
        assert_eq!(x("#{m/p:c,ab#[align=centre]cd}|#{m/p:x,ab#[align=absolute-centre]x}", &[]), "2|1");
        assert_eq!(x("#{m/p:a#,b,x a#,b y}", &[]), "2,3,4");
        assert_eq!(x("#{m/p:漢b,#[fg=red]漢ab}|#{m/p:#[,a##[b}", &[]), "0,1,3|1,2");
        assert_eq!(x("#{m/z:'foo$ !^bar,foo}|#{m/z:!^foo,foo}|#{m/z:^,foo}|#{m/z:fo |,foo}", &[]), "1|0|0|1");
    }

    #[test]
    fn colour_modifiers_and_names() {
        assert_eq!(x("#{c:red}|#{c:colour196}|#{c:#0102ff}|#{c:default}|#{c:themegreen}", &[]), "800000|ff0000|0102ff||");
        assert_eq!(x("#{c:brightred}|#{c:gray50}|#{c:8}|#{c:12}|#{c:97}|#{c:grey}", &[]), "ff0000|7f7f7f|||ffffff|bebebe");
        assert_eq!(x("#{c/f:red}|#{c/b:brightblue}|#{c/f:colour9}|#{c/b:#010203}", &[]), "\x1b[31m|\x1b[104m|\x1b[38;5;9m|\x1b[48;2;1;2;3m");
        assert_eq!(x("#{c/f:none}|#{c/b:default}|#{c:bad}|#{c/f:themegreen}", &[]), "\x1b[0m|\x1b[49m||\x1b[32m");
        assert_eq!(parse_color("90"), Some(Color::Idx(8)));
        assert_eq!(parse_color("8"), None);
        assert_eq!(parse_color("12"), None);
        assert_eq!(parse_color("gray50"), Some(Color::Rgb(127, 127, 127)));
        assert_eq!(parse_color("AliceBlue"), Some(Color::Rgb(240, 248, 255)));
        assert_eq!(parse_color("grey101"), None);
        for (name, ..) in THEMES {
            assert!(parse_color(name).is_some());
            assert!(theme_color(name, Theme::Light).is_some());
        }
        assert_eq!(parse_color("ThemeGreen"), parse_color("yellowgreen"));
        assert_eq!(theme_color("themegreen", Theme::Light), parse_color("seagreen"));
        assert_eq!(theme_color("themegreen", Theme::Terminal), Some(Color::Idx(2)));
    }

    /// A client whose theme resolves themered to bright red.
    struct ThemeVars(HashMap<String, String>);
    impl Vars for ThemeVars {
        fn get(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
        fn theme_color(&self, name: &str) -> Option<Color> {
            (name == "themered").then_some(Color::Idx(9))
        }
    }

    #[test]
    fn dim_links_and_styles_with_format_context() {
        let base = Style::default();
        let parsed = parse_style_with("fg=#804020,dim=50%,link=https://Example.test", base, None).unwrap();
        assert_eq!((parsed.dim, parsed.link.as_deref()), (50, Some("https://Example.test")));
        assert_eq!(parsed.style.fg, Color::Rgb(128, 64, 32));
        assert_eq!(dim_color(Color::Rgb(128, 64, 32), 50), Color::Rgb(64, 32, 16));
        assert_eq!(dim_color(Color::Idx(1), 100), Color::Rgb(0, 0, 0));
        assert_eq!(dim_color(Color::Default, 50), Color::Default);
        for s in ["dim=101", "dim=-1", "dim=xx", "dim=50%%", "dim=", "width=101%", "width=%"] {
            assert!(parse_style(s, base).is_err(), "{s}");
        }
        assert!(parse_style("width=50%,pad=2,dim=0,ignore,noignore,set-default,noattr", base).is_ok());
        assert_eq!(parse_style_with("link=x,nolink", base, None).unwrap().link, None);
        assert_eq!(parse_style_with("link=x,link=", base, None).unwrap().link, None);
        assert_eq!(parse_style_with("link=x,default", base, None).unwrap().link, None);
        // Styles with #{ are expanded as formats when there is a context.
        let v = ThemeVars(vars(&[("colour", "themered"), ("on", "1"), ("D", "%1")]));
        assert_eq!(parse_style_with("fg=#{?on,#{colour},red}", base, Some(&v)).unwrap().style.fg, Color::Idx(9));
        assert_eq!(parse_style_with("bg=themered", base, Some(&v)).unwrap().style.bg, Color::Idx(9));
        assert_eq!(parse_style_with("bg=themegreen", base, Some(&v)).unwrap().style.bg, parse_color("yellowgreen").unwrap());
        assert_eq!(parse_style_with("fg=#D0D0D0", base, Some(&v)).unwrap().style.fg, Color::Rgb(0xd0, 0xd0, 0xd0));
        assert!(parse_style("fg=#{x}", base).is_err());
        // Links are kept on spans; dim= does not change status line colours.
        let spans = spans("#[fg=#804020,dim=50,link=https://Example.test]a#[nolink]b#[link=u]c#[default]d", base);
        assert_eq!(spans.iter().map(|s| s.link.as_deref()).collect::<Vec<_>>(), [Some("https://Example.test"), None, Some("u"), None]);
        assert_eq!(spans[0].style.fg, Color::Rgb(128, 64, 32));
    }

    #[test]
    fn set_default_and_absolute_centre() {
        let base = Style::default();
        let sp = spans("#[fg=red]#[set-default,bold]a#[default]b", base);
        assert_eq!(sp[1].style, st(Color::Idx(1), 0));
        let (cells, _) = layout_line_ranged(&spans("ab#[align=absolute-centre]X", base), 9, base);
        assert_eq!(cells.iter().map(|c| c.text.as_str()).collect::<String>(), "ab  X    ");
        assert_eq!(parse_style("noalign", base), Ok(base));
    }

    // ----- spans -----

    fn st(fg: Color, attrs: u8) -> Style {
        Style { fg, bg: Color::Default, attrs }
    }

    #[test]
    fn spans_basic() {
        let base = Style::default();
        let sp = spans("#[fg=red]a#[default]b", base);
        assert_eq!(
            sp,
            vec![
                Span { text: "a".into(), link: None, style: st(Color::Idx(1), 0), align: Align::Left, range: None, range_end: false },
                Span { text: "b".into(), link: None, style: base, align: Align::Left, range: None, range_end: false },
            ]
        );
        // Merging of identical styles, `##` → `#`, `##[` is literal.
        let sp = spans("a#[bold]#[]b##c##[x]", base);
        assert_eq!(sp.len(), 2);
        assert_eq!(sp[0].text, "a");
        assert_eq!(sp[1].text, "b#c#[x]");
        assert_eq!(sp[1].style.attrs, BOLD);
        // none clears attrs only.
        let sp = spans("#[fg=blue,bold]x#[none]y", base);
        assert_eq!(sp[0].style, st(Color::Idx(4), BOLD));
        assert_eq!(sp[1].style, st(Color::Idx(4), 0));
        // As format_draw, a directive with an unknown word is ignored as a whole.
        let sp = spans("#[fg=blue,bold,sparkly]x#[fg=red,bogus]y", base);
        assert_eq!((sp.len(), sp[0].text.as_str(), sp[0].style), (1, "xy", base));
        // Unterminated directive is text.
        assert_eq!(spans("#[fg=red", base)[0].text, "#[fg=red");
        assert!(spans("", base).is_empty());
    }

    #[test]
    fn list_markers_are_not_drawn() {
        let base = Style::default();
        let text: String = spans("P: #[list=on]#[list=left-marker]<#[list=right-marker]>#[list=on]a b#[nolist] R", base).iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "P: a b R");
    }

    #[test]
    fn spans_align_and_defaults() {
        let base = st(Color::Idx(2), 0);
        let sp = spans("L#[align=centre]C#[align=right,fill=red,list=on]R", base);
        let al: Vec<_> = sp.iter().map(|s| (s.text.as_str(), s.align)).collect();
        assert_eq!(al, vec![("L", Align::Left), ("C", Align::Centre), ("R", Align::Right)]);
        let sp = spans("#[fg=red]#[push-default]a#[fg=blue]b#[default]c#[pop-default]#[default]d", base);
        let fgs: Vec<_> = sp.iter().map(|s| (s.text.as_str(), s.style.fg)).collect();
        assert_eq!(fgs, vec![("a", Color::Idx(1)), ("b", Color::Idx(4)), ("c", Color::Idx(1)), ("d", Color::Idx(2))]);
    }

    // ----- width / layout -----

    #[test]
    fn widths() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("漢字"), 4);
        assert_eq!(width("e\u{301}"), 1);
        assert_eq!(width(""), 0);
    }

    fn text(cells: &[Cell]) -> String {
        cells.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn layout_groups() {
        let fill = st(Color::Idx(7), 0);
        let sp = spans("ab#[align=centre]mid#[align=right]yz", Style::default());
        let cells = layout_line(&sp, 13, fill);
        assert_eq!(cells.len(), 13);
        assert_eq!(text(&cells), "ab   mid   yz");
        assert_eq!(cells[2].style, fill);
        assert_eq!(cells[0].style, Style::default());
        // Only right.
        let sp = spans("#[align=right]r", Style::default());
        assert_eq!(text(&layout_line(&sp, 4, fill)), "   r");
        // Empty / zero width.
        assert_eq!(text(&layout_line(&[], 3, fill)), "   ");
        assert!(layout_line(&sp, 0, fill).is_empty());
    }

    #[test]
    fn layout_truncation_priority() {
        let fill = Style::default();
        let sp = spans("left#[align=centre]centre#[align=right]right", fill);
        // Centre goes first.
        assert_eq!(text(&layout_line(&sp, 9, fill)), "leftright");
        // Then right (keeping its end), then left.
        assert_eq!(text(&layout_line(&sp, 7, fill)), "leftght");
        assert_eq!(text(&layout_line(&sp, 3, fill)), "lef");
        // Centre truncated keeps its middle.
        assert_eq!(text(&layout_line(&sp, 12, fill)), "leftntrright");
        let sp = spans("abcdef", fill);
        assert_eq!(text(&layout_line(&sp, 4, fill)), "abcd");
    }

    #[test]
    fn layout_wide_chars() {
        let fill = Style::default();
        let sp = spans("漢字x", fill);
        let cells = layout_line(&sp, 5, fill);
        assert_eq!(cells.len(), 5);
        assert_eq!(cells[0].text, "漢");
        assert!(cells[1].is_continuation());
        assert_eq!(cells[2].text, "字");
        assert!(cells[3].is_continuation());
        assert_eq!(cells[4].text, "x");
        // Cut in the middle of a wide char → blank, never half a char.
        let cells = layout_line(&sp, 3, fill);
        assert_eq!(cells.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), vec!["漢", "", " "]);
        let sp = spans("#[align=right]漢字", fill);
        let cells = layout_line(&sp, 3, fill);
        assert_eq!(cells.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), vec![" ", "字", ""]);
        // Combining characters stay in the cell before them.
        let sp = spans("e\u{301}x", fill);
        let cells = layout_line(&sp, 2, fill);
        assert_eq!(cells[0].text, "e\u{301}");
        assert_eq!(cells[1].text, "x");
    }

    #[test]
    fn full_status_line_pipeline() {
        let v = vars(&[("session_name", "work"), ("window_zoomed_flag", "1")]);
        let fmt = "#[fg=green][#{session_name}]#[default] #{?window_zoomed_flag,#[fg=red]Z,}#[align=right]end";
        let line = expand(fmt, &v);
        let sp = spans(&line, Style::default());
        let cells = layout_line(&sp, 12, Style::default());
        assert_eq!(text(&cells), "[work] Z end");
        assert_eq!(cells[0].style.fg, Color::Idx(2));
        assert_eq!(cells[7].style.fg, Color::Idx(1));
        assert_eq!(cells[8].style, Style::default());
        assert_eq!(cells[11].style.fg, Color::Idx(1));
    }

    // ----- tmux 3.8 comparisons (outputs checked against tmux 3.8) -----

    #[test]
    fn aliases_are_not_expanded_inside_styles() {
        let v = [("pane_id", "%3"), ("window_flags", "*"), ("num", "42")];
        assert_eq!(x("#[fg=#DDDDDD]x", &v), "#[fg=#DDDDDD]x");
        assert_eq!(x("#[fg=#FFFFFF,bg=#000000]y", &v), "#[fg=#FFFFFF,bg=#000000]y");
        assert_eq!(x("##[fg=#DDD]", &v), "##[fg=#DDD]");
        // After the `]` they are expanded again; #{} is expanded inside too.
        assert_eq!(x("#[fg=red]#D#[x]", &v), "#[fg=red]%3#[x]");
        assert_eq!(x("#[#{num}#D]#D", &v), "#[42#D]%3");
        // Without a `]` there is no style to skip.
        assert_eq!(x("#[fg=red #D", &v), "#[fg=red %3");
    }

    #[test]
    fn expressions_parse_operands_like_strtod() {
        assert_eq!(x("#{e|+|:5}after", &[]), "after");
        assert_eq!(x("#{e|+|:,5}|#{e|+|:#{unset},1}|#{e|+|:1,}", &[]), "5|1|1");
        assert_eq!(x("#{e|+|:0x10,1}|#{e|+|: 1,1}|#{e|+|:1 ,1}|#{e|+|:1e,1}", &[]), "17|2||");
        assert_eq!(x("#{e|*|f|:.5,4}|#{e|*|f|:5.,4}|#{e|+|f|:1e1,0X1P1}", &[]), "2.00|20.00|12.00");
        assert_eq!(x("#{e|+|:+,1}|#{e|+|: ,1}|#{e|+|f|:nan,1}|#{e|+|:inf,1}", &[]), "||nan|-9223372036854775808");
    }

    #[test]
    fn repeated_modifiers_use_the_last() {
        let v = [("path", "/usr/local/bin/"), ("num", "42")];
        assert_eq!(x("#{=3;=5:path}|#{=5;=3:path}", &v), "/usr/|/us");
        assert_eq!(x("#{=/3/..;=5:path}|#{=/5/..;=4:path}", &v), "/usr/..|/usr..");
        assert_eq!(x("#{p8;p3:num}|", &v), "42 |");
        assert_eq!(x("#{==;!=:a,a}|#{m;==:a*,ab}|#{<;>:a,b}", &v), "0|0|0");
        assert_eq!(x("#{e|+|;e|*|:2,3}", &v), "6");
        // t: relative wins over pretty whatever the order.
        let secs = fixed_time().timestamp() - 90;
        let t = vars(&[("t", &secs.to_string())]);
        let budget = std::cell::Cell::new(MAX_FORMAT_WORK);
        let e = Expander::top(&t, fixed_time(), None, &budget);
        assert_eq!(e.expand("#{t/r;t/p:t}|#{t/p;t/r:t}", 0), "1m30s|1m30s");
        assert_eq!(e.expand("#{t/f/%H;t/p:t}", 0), e.expand("#{t/p:t}", 0));
    }

    #[test]
    fn substitution_follows_regsub() {
        let v = [("num", "42"), ("empty", ""), ("abc", "abc")];
        assert_eq!(x("#{s//x/:num}", &v), "42");
        assert_eq!(x(r"#{s/(4)/[\2]/:num}|#{s/(4)(x?)/[\2]/:num}", &v), "[2]2|[2]2");
        assert_eq!(x("#{s/^/>/:empty}|#{s/$/</:nope}", &v), "|");
        assert_eq!(x("#{s/x*/-/:abc}|#{s/b*/-/:abc}", &v), "a-b-c-|a-c-");
        assert_eq!(x("#{s/(^a|c)/X/:abc}|#{s/^a|c/X/:abc}|#{s/b|$/Y/:abc}", &v), "XbX|Xbc|aYcY");
        assert_eq!(x(r"#{s/.?/<\0>/:abc}|#{s/b/[\0\q\]/:abc}|#{s/b/x\/:abc}", &v), r"<a><b><c>|a[bq]c|ax\c");
    }

    #[test]
    fn padding_counts_styles_as_text() {
        assert_eq!(x("#{p10:#{l:#[fg=red]ab}}|", &[]), "#[fg=red]ab|");
        assert_eq!(x("#{p-6:#{l:##x}}|", &[]), "    #x|");
        assert_eq!(cstr_width("a\u{7f}\u{1}漢"), 3);
    }

    #[test]
    fn environment_is_the_last_lookup() {
        struct Env(HashMap<String, String>);
        impl Vars for Env {
            fn get(&self, name: &str) -> Option<String> {
                self.0.get(name).cloned()
            }
            fn environ(&self, name: &str) -> Option<String> {
                (name == "VFMTVAR" || name == "num").then(|| "env".to_string())
            }
        }
        let v = Env(vars(&[("num", "42")]));
        assert_eq!(expand("#{VFMTVAR}|#{q:VFMTVAR}|#{?VFMTVAR,yes,no}|#{num}|#{nope}|", &v), "env|env|yes|42||");
        // Not for t, which wants a time.
        assert_eq!(expand("#{t:VFMTVAR}|", &v), "|");
    }

    #[test]
    fn colour_names_are_not_trimmed() {
        assert_eq!(x("#{c:colour 5}|#{c:GREY 50}|#{c:colour+5}", &[]), "800080|7f7f7f|800080");
        assert_eq!(x("#{c: red}|#{c:red }", &[]), "|");
        assert_eq!(parse_color(" red"), None);
        assert_eq!(parse_color("colour 300"), None);
    }

    #[test]
    fn styles_are_as_strict_as_style_parse() {
        let base = Style::default();
        for s in ["align=center", "align=absolute-center", "fg=red\tbold", "fg=red,bogus", "fg= red", "fg=colour 5x", "range=control", "range=control|", "range=control|10"] {
            assert!(parse_style(s, base).is_err(), "{s}");
        }
        assert!(parse_style("align=centre,align=absolute-centre", base).is_ok());
        assert_eq!(Range::parse("control|7"), Ok(Some(Range::Control(7))));
        assert_eq!(Range::parse("control|0"), Ok(Some(Range::Control(0))));
        assert!(parse_style("fg=red,range=control|9", base).is_ok());
        assert_eq!(Range::Control(3).name(), "control");
        let sp = spans("#[fg=red,range=control|7]y#[norange]z", base);
        assert_eq!((sp[0].range.clone(), sp[0].style.fg), (Some(Range::Control(7)), Color::Idx(1)));
        // Control ranges match each other (format_is_type), so the first one continues.
        let sp = spans("#[range=control|7]a#[range=control|8]b#[norange]c", base);
        assert_eq!(sp[0].text, "ab");
        assert_eq!(sp[0].range, Some(Range::Control(7)));
        // An invalid directive changes nothing; an unknown range kind is no error.
        let sp = spans("#[fg=red,bogus]x#[fg=blue,align=center]y#[fg=red,range=foo|x]z", base);
        assert_eq!((sp.len(), sp[1].text.as_str(), sp[1].style.fg, sp[1].range.clone()), (2, "z", Color::Idx(1), None));
        assert!(parse_style("range=foo", base).is_ok());
        assert!(parse_style("range=", base).is_err() && parse_style("range=foo|", base).is_err());
    }

}

// X11 colour data from tmux 3.8 colour.c (Nicholas Marriott, ISC licensed).
const X11_COLOURS: &[(&str, u32)] = &[
    ("AliceBlue", 0xf0f8ff),
    ("AntiqueWhite", 0xfaebd7),
    ("AntiqueWhite1", 0xffefdb),
    ("AntiqueWhite2", 0xeedfcc),
    ("AntiqueWhite3", 0xcdc0b0),
    ("AntiqueWhite4", 0x8b8378),
    ("BlanchedAlmond", 0xffebcd),
    ("BlueViolet", 0x8a2be2),
    ("CadetBlue", 0x5f9ea0),
    ("CadetBlue1", 0x98f5ff),
    ("CadetBlue2", 0x8ee5ee),
    ("CadetBlue3", 0x7ac5cd),
    ("CadetBlue4", 0x53868b),
    ("CornflowerBlue", 0x6495ed),
    ("DarkBlue", 0x00008b),
    ("DarkCyan", 0x008b8b),
    ("DarkGoldenrod", 0xb8860b),
    ("DarkGoldenrod1", 0xffb90f),
    ("DarkGoldenrod2", 0xeead0e),
    ("DarkGoldenrod3", 0xcd950c),
    ("DarkGoldenrod4", 0x8b6508),
    ("DarkGray", 0xa9a9a9),
    ("DarkGreen", 0x006400),
    ("DarkGrey", 0xa9a9a9),
    ("DarkKhaki", 0xbdb76b),
    ("DarkMagenta", 0x8b008b),
    ("DarkOliveGreen", 0x556b2f),
    ("DarkOliveGreen1", 0xcaff70),
    ("DarkOliveGreen2", 0xbcee68),
    ("DarkOliveGreen3", 0xa2cd5a),
    ("DarkOliveGreen4", 0x6e8b3d),
    ("DarkOrange", 0xff8c00),
    ("DarkOrange1", 0xff7f00),
    ("DarkOrange2", 0xee7600),
    ("DarkOrange3", 0xcd6600),
    ("DarkOrange4", 0x8b4500),
    ("DarkOrchid", 0x9932cc),
    ("DarkOrchid1", 0xbf3eff),
    ("DarkOrchid2", 0xb23aee),
    ("DarkOrchid3", 0x9a32cd),
    ("DarkOrchid4", 0x68228b),
    ("DarkRed", 0x8b0000),
    ("DarkSalmon", 0xe9967a),
    ("DarkSeaGreen", 0x8fbc8f),
    ("DarkSeaGreen1", 0xc1ffc1),
    ("DarkSeaGreen2", 0xb4eeb4),
    ("DarkSeaGreen3", 0x9bcd9b),
    ("DarkSeaGreen4", 0x698b69),
    ("DarkSlateBlue", 0x483d8b),
    ("DarkSlateGray", 0x2f4f4f),
    ("DarkSlateGray1", 0x97ffff),
    ("DarkSlateGray2", 0x8deeee),
    ("DarkSlateGray3", 0x79cdcd),
    ("DarkSlateGray4", 0x528b8b),
    ("DarkSlateGrey", 0x2f4f4f),
    ("DarkTurquoise", 0x00ced1),
    ("DarkViolet", 0x9400d3),
    ("DeepPink", 0xff1493),
    ("DeepPink1", 0xff1493),
    ("DeepPink2", 0xee1289),
    ("DeepPink3", 0xcd1076),
    ("DeepPink4", 0x8b0a50),
    ("DeepSkyBlue", 0x00bfff),
    ("DeepSkyBlue1", 0x00bfff),
    ("DeepSkyBlue2", 0x00b2ee),
    ("DeepSkyBlue3", 0x009acd),
    ("DeepSkyBlue4", 0x00688b),
    ("DimGray", 0x696969),
    ("DimGrey", 0x696969),
    ("DodgerBlue", 0x1e90ff),
    ("DodgerBlue1", 0x1e90ff),
    ("DodgerBlue2", 0x1c86ee),
    ("DodgerBlue3", 0x1874cd),
    ("DodgerBlue4", 0x104e8b),
    ("FloralWhite", 0xfffaf0),
    ("ForestGreen", 0x228b22),
    ("GhostWhite", 0xf8f8ff),
    ("GreenYellow", 0xadff2f),
    ("HotPink", 0xff69b4),
    ("HotPink1", 0xff6eb4),
    ("HotPink2", 0xee6aa7),
    ("HotPink3", 0xcd6090),
    ("HotPink4", 0x8b3a62),
    ("IndianRed", 0xcd5c5c),
    ("IndianRed1", 0xff6a6a),
    ("IndianRed2", 0xee6363),
    ("IndianRed3", 0xcd5555),
    ("IndianRed4", 0x8b3a3a),
    ("LavenderBlush", 0xfff0f5),
    ("LavenderBlush1", 0xfff0f5),
    ("LavenderBlush2", 0xeee0e5),
    ("LavenderBlush3", 0xcdc1c5),
    ("LavenderBlush4", 0x8b8386),
    ("LawnGreen", 0x7cfc00),
    ("LemonChiffon", 0xfffacd),
    ("LemonChiffon1", 0xfffacd),
    ("LemonChiffon2", 0xeee9bf),
    ("LemonChiffon3", 0xcdc9a5),
    ("LemonChiffon4", 0x8b8970),
    ("LightBlue", 0xadd8e6),
    ("LightBlue1", 0xbfefff),
    ("LightBlue2", 0xb2dfee),
    ("LightBlue3", 0x9ac0cd),
    ("LightBlue4", 0x68838b),
    ("LightCoral", 0xf08080),
    ("LightCyan", 0xe0ffff),
    ("LightCyan1", 0xe0ffff),
    ("LightCyan2", 0xd1eeee),
    ("LightCyan3", 0xb4cdcd),
    ("LightCyan4", 0x7a8b8b),
    ("LightGoldenrod", 0xeedd82),
    ("LightGoldenrod1", 0xffec8b),
    ("LightGoldenrod2", 0xeedc82),
    ("LightGoldenrod3", 0xcdbe70),
    ("LightGoldenrod4", 0x8b814c),
    ("LightGoldenrodYellow", 0xfafad2),
    ("LightGray", 0xd3d3d3),
    ("LightGreen", 0x90ee90),
    ("LightGrey", 0xd3d3d3),
    ("LightPink", 0xffb6c1),
    ("LightPink1", 0xffaeb9),
    ("LightPink2", 0xeea2ad),
    ("LightPink3", 0xcd8c95),
    ("LightPink4", 0x8b5f65),
    ("LightSalmon", 0xffa07a),
    ("LightSalmon1", 0xffa07a),
    ("LightSalmon2", 0xee9572),
    ("LightSalmon3", 0xcd8162),
    ("LightSalmon4", 0x8b5742),
    ("LightSeaGreen", 0x20b2aa),
    ("LightSkyBlue", 0x87cefa),
    ("LightSkyBlue1", 0xb0e2ff),
    ("LightSkyBlue2", 0xa4d3ee),
    ("LightSkyBlue3", 0x8db6cd),
    ("LightSkyBlue4", 0x607b8b),
    ("LightSlateBlue", 0x8470ff),
    ("LightSlateGray", 0x778899),
    ("LightSlateGrey", 0x778899),
    ("LightSteelBlue", 0xb0c4de),
    ("LightSteelBlue1", 0xcae1ff),
    ("LightSteelBlue2", 0xbcd2ee),
    ("LightSteelBlue3", 0xa2b5cd),
    ("LightSteelBlue4", 0x6e7b8b),
    ("LightYellow", 0xffffe0),
    ("LightYellow1", 0xffffe0),
    ("LightYellow2", 0xeeeed1),
    ("LightYellow3", 0xcdcdb4),
    ("LightYellow4", 0x8b8b7a),
    ("LimeGreen", 0x32cd32),
    ("MediumAquamarine", 0x66cdaa),
    ("MediumBlue", 0x0000cd),
    ("MediumOrchid", 0xba55d3),
    ("MediumOrchid1", 0xe066ff),
    ("MediumOrchid2", 0xd15fee),
    ("MediumOrchid3", 0xb452cd),
    ("MediumOrchid4", 0x7a378b),
    ("MediumPurple", 0x9370db),
    ("MediumPurple1", 0xab82ff),
    ("MediumPurple2", 0x9f79ee),
    ("MediumPurple3", 0x8968cd),
    ("MediumPurple4", 0x5d478b),
    ("MediumSeaGreen", 0x3cb371),
    ("MediumSlateBlue", 0x7b68ee),
    ("MediumSpringGreen", 0x00fa9a),
    ("MediumTurquoise", 0x48d1cc),
    ("MediumVioletRed", 0xc71585),
    ("MidnightBlue", 0x191970),
    ("MintCream", 0xf5fffa),
    ("MistyRose", 0xffe4e1),
    ("MistyRose1", 0xffe4e1),
    ("MistyRose2", 0xeed5d2),
    ("MistyRose3", 0xcdb7b5),
    ("MistyRose4", 0x8b7d7b),
    ("NavajoWhite", 0xffdead),
    ("NavajoWhite1", 0xffdead),
    ("NavajoWhite2", 0xeecfa1),
    ("NavajoWhite3", 0xcdb38b),
    ("NavajoWhite4", 0x8b795e),
    ("NavyBlue", 0x000080),
    ("OldLace", 0xfdf5e6),
    ("OliveDrab", 0x6b8e23),
    ("OliveDrab1", 0xc0ff3e),
    ("OliveDrab2", 0xb3ee3a),
    ("OliveDrab3", 0x9acd32),
    ("OliveDrab4", 0x698b22),
    ("OrangeRed", 0xff4500),
    ("OrangeRed1", 0xff4500),
    ("OrangeRed2", 0xee4000),
    ("OrangeRed3", 0xcd3700),
    ("OrangeRed4", 0x8b2500),
    ("PaleGoldenrod", 0xeee8aa),
    ("PaleGreen", 0x98fb98),
    ("PaleGreen1", 0x9aff9a),
    ("PaleGreen2", 0x90ee90),
    ("PaleGreen3", 0x7ccd7c),
    ("PaleGreen4", 0x548b54),
    ("PaleTurquoise", 0xafeeee),
    ("PaleTurquoise1", 0xbbffff),
    ("PaleTurquoise2", 0xaeeeee),
    ("PaleTurquoise3", 0x96cdcd),
    ("PaleTurquoise4", 0x668b8b),
    ("PaleVioletRed", 0xdb7093),
    ("PaleVioletRed1", 0xff82ab),
    ("PaleVioletRed2", 0xee799f),
    ("PaleVioletRed3", 0xcd6889),
    ("PaleVioletRed4", 0x8b475d),
    ("PapayaWhip", 0xffefd5),
    ("PeachPuff", 0xffdab9),
    ("PeachPuff1", 0xffdab9),
    ("PeachPuff2", 0xeecbad),
    ("PeachPuff3", 0xcdaf95),
    ("PeachPuff4", 0x8b7765),
    ("PowderBlue", 0xb0e0e6),
    ("RebeccaPurple", 0x663399),
    ("RosyBrown", 0xbc8f8f),
    ("RosyBrown1", 0xffc1c1),
    ("RosyBrown2", 0xeeb4b4),
    ("RosyBrown3", 0xcd9b9b),
    ("RosyBrown4", 0x8b6969),
    ("RoyalBlue", 0x4169e1),
    ("RoyalBlue1", 0x4876ff),
    ("RoyalBlue2", 0x436eee),
    ("RoyalBlue3", 0x3a5fcd),
    ("RoyalBlue4", 0x27408b),
    ("SaddleBrown", 0x8b4513),
    ("SandyBrown", 0xf4a460),
    ("SeaGreen", 0x2e8b57),
    ("SeaGreen1", 0x54ff9f),
    ("SeaGreen2", 0x4eee94),
    ("SeaGreen3", 0x43cd80),
    ("SeaGreen4", 0x2e8b57),
    ("SkyBlue", 0x87ceeb),
    ("SkyBlue1", 0x87ceff),
    ("SkyBlue2", 0x7ec0ee),
    ("SkyBlue3", 0x6ca6cd),
    ("SkyBlue4", 0x4a708b),
    ("SlateBlue", 0x6a5acd),
    ("SlateBlue1", 0x836fff),
    ("SlateBlue2", 0x7a67ee),
    ("SlateBlue3", 0x6959cd),
    ("SlateBlue4", 0x473c8b),
    ("SlateGray", 0x708090),
    ("SlateGray1", 0xc6e2ff),
    ("SlateGray2", 0xb9d3ee),
    ("SlateGray3", 0x9fb6cd),
    ("SlateGray4", 0x6c7b8b),
    ("SlateGrey", 0x708090),
    ("SpringGreen", 0x00ff7f),
    ("SpringGreen1", 0x00ff7f),
    ("SpringGreen2", 0x00ee76),
    ("SpringGreen3", 0x00cd66),
    ("SpringGreen4", 0x008b45),
    ("SteelBlue", 0x4682b4),
    ("SteelBlue1", 0x63b8ff),
    ("SteelBlue2", 0x5cacee),
    ("SteelBlue3", 0x4f94cd),
    ("SteelBlue4", 0x36648b),
    ("VioletRed", 0xd02090),
    ("VioletRed1", 0xff3e96),
    ("VioletRed2", 0xee3a8c),
    ("VioletRed3", 0xcd3278),
    ("VioletRed4", 0x8b2252),
    ("WebGray", 0x808080),
    ("WebGreen", 0x008000),
    ("WebGrey", 0x808080),
    ("WebMaroon", 0x800000),
    ("WebPurple", 0x800080),
    ("WhiteSmoke", 0xf5f5f5),
    ("X11Gray", 0xbebebe),
    ("X11Green", 0x00ff00),
    ("X11Grey", 0xbebebe),
    ("X11Maroon", 0xb03060),
    ("X11Purple", 0xa020f0),
    ("YellowGreen", 0x9acd32),
    ("alice blue", 0xf0f8ff),
    ("antique white", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("aquamarine1", 0x7fffd4),
    ("aquamarine2", 0x76eec6),
    ("aquamarine3", 0x66cdaa),
    ("aquamarine4", 0x458b74),
    ("azure", 0xf0ffff),
    ("azure1", 0xf0ffff),
    ("azure2", 0xe0eeee),
    ("azure3", 0xc1cdcd),
    ("azure4", 0x838b8b),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("bisque1", 0xffe4c4),
    ("bisque2", 0xeed5b7),
    ("bisque3", 0xcdb79e),
    ("bisque4", 0x8b7d6b),
    ("black", 0x000000),
    ("blanched almond", 0xffebcd),
    ("blue violet", 0x8a2be2),
    ("blue", 0x0000ff),
    ("blue1", 0x0000ff),
    ("blue2", 0x0000ee),
    ("blue3", 0x0000cd),
    ("blue4", 0x00008b),
    ("brown", 0xa52a2a),
    ("brown1", 0xff4040),
    ("brown2", 0xee3b3b),
    ("brown3", 0xcd3333),
    ("brown4", 0x8b2323),
    ("burlywood", 0xdeb887),
    ("burlywood1", 0xffd39b),
    ("burlywood2", 0xeec591),
    ("burlywood3", 0xcdaa7d),
    ("burlywood4", 0x8b7355),
    ("cadet blue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chartreuse1", 0x7fff00),
    ("chartreuse2", 0x76ee00),
    ("chartreuse3", 0x66cd00),
    ("chartreuse4", 0x458b00),
    ("chocolate", 0xd2691e),
    ("chocolate1", 0xff7f24),
    ("chocolate2", 0xee7621),
    ("chocolate3", 0xcd661d),
    ("chocolate4", 0x8b4513),
    ("coral", 0xff7f50),
    ("coral1", 0xff7256),
    ("coral2", 0xee6a50),
    ("coral3", 0xcd5b45),
    ("coral4", 0x8b3e2f),
    ("cornflower blue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("cornsilk1", 0xfff8dc),
    ("cornsilk2", 0xeee8cd),
    ("cornsilk3", 0xcdc8b1),
    ("cornsilk4", 0x8b8878),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("cyan1", 0x00ffff),
    ("cyan2", 0x00eeee),
    ("cyan3", 0x00cdcd),
    ("cyan4", 0x008b8b),
    ("dark blue", 0x00008b),
    ("dark cyan", 0x008b8b),
    ("dark goldenrod", 0xb8860b),
    ("dark gray", 0xa9a9a9),
    ("dark green", 0x006400),
    ("dark grey", 0xa9a9a9),
    ("dark khaki", 0xbdb76b),
    ("dark magenta", 0x8b008b),
    ("dark olive green", 0x556b2f),
    ("dark orange", 0xff8c00),
    ("dark orchid", 0x9932cc),
    ("dark red", 0x8b0000),
    ("dark salmon", 0xe9967a),
    ("dark sea green", 0x8fbc8f),
    ("dark slate blue", 0x483d8b),
    ("dark slate gray", 0x2f4f4f),
    ("dark slate grey", 0x2f4f4f),
    ("dark turquoise", 0x00ced1),
    ("dark violet", 0x9400d3),
    ("deep pink", 0xff1493),
    ("deep sky blue", 0x00bfff),
    ("dim gray", 0x696969),
    ("dim grey", 0x696969),
    ("dodger blue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("firebrick1", 0xff3030),
    ("firebrick2", 0xee2c2c),
    ("firebrick3", 0xcd2626),
    ("firebrick4", 0x8b1a1a),
    ("floral white", 0xfffaf0),
    ("forest green", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghost white", 0xf8f8ff),
    ("gold", 0xffd700),
    ("gold1", 0xffd700),
    ("gold2", 0xeec900),
    ("gold3", 0xcdad00),
    ("gold4", 0x8b7500),
    ("goldenrod", 0xdaa520),
    ("goldenrod1", 0xffc125),
    ("goldenrod2", 0xeeb422),
    ("goldenrod3", 0xcd9b1d),
    ("goldenrod4", 0x8b6914),
    ("green yellow", 0xadff2f),
    ("green", 0x00ff00),
    ("green1", 0x00ff00),
    ("green2", 0x00ee00),
    ("green3", 0x00cd00),
    ("green4", 0x008b00),
    ("honeydew", 0xf0fff0),
    ("honeydew1", 0xf0fff0),
    ("honeydew2", 0xe0eee0),
    ("honeydew3", 0xc1cdc1),
    ("honeydew4", 0x838b83),
    ("hot pink", 0xff69b4),
    ("indian red", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("ivory1", 0xfffff0),
    ("ivory2", 0xeeeee0),
    ("ivory3", 0xcdcdc1),
    ("ivory4", 0x8b8b83),
    ("khaki", 0xf0e68c),
    ("khaki1", 0xfff68f),
    ("khaki2", 0xeee685),
    ("khaki3", 0xcdc673),
    ("khaki4", 0x8b864e),
    ("lavender blush", 0xfff0f5),
    ("lavender", 0xe6e6fa),
    ("lawn green", 0x7cfc00),
    ("lemon chiffon", 0xfffacd),
    ("light blue", 0xadd8e6),
    ("light coral", 0xf08080),
    ("light cyan", 0xe0ffff),
    ("light goldenrod yellow", 0xfafad2),
    ("light goldenrod", 0xeedd82),
    ("light gray", 0xd3d3d3),
    ("light green", 0x90ee90),
    ("light grey", 0xd3d3d3),
    ("light pink", 0xffb6c1),
    ("light salmon", 0xffa07a),
    ("light sea green", 0x20b2aa),
    ("light sky blue", 0x87cefa),
    ("light slate blue", 0x8470ff),
    ("light slate gray", 0x778899),
    ("light slate grey", 0x778899),
    ("light steel blue", 0xb0c4de),
    ("light yellow", 0xffffe0),
    ("lime green", 0x32cd32),
    ("lime", 0x00ff00),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("magenta1", 0xff00ff),
    ("magenta2", 0xee00ee),
    ("magenta3", 0xcd00cd),
    ("magenta4", 0x8b008b),
    ("maroon", 0xb03060),
    ("maroon1", 0xff34b3),
    ("maroon2", 0xee30a7),
    ("maroon3", 0xcd2990),
    ("maroon4", 0x8b1c62),
    ("medium aquamarine", 0x66cdaa),
    ("medium blue", 0x0000cd),
    ("medium orchid", 0xba55d3),
    ("medium purple", 0x9370db),
    ("medium sea green", 0x3cb371),
    ("medium slate blue", 0x7b68ee),
    ("medium spring green", 0x00fa9a),
    ("medium turquoise", 0x48d1cc),
    ("medium violet red", 0xc71585),
    ("midnight blue", 0x191970),
    ("mint cream", 0xf5fffa),
    ("misty rose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajo white", 0xffdead),
    ("navy blue", 0x000080),
    ("navy", 0x000080),
    ("old lace", 0xfdf5e6),
    ("olive drab", 0x6b8e23),
    ("olive", 0x808000),
    ("orange red", 0xff4500),
    ("orange", 0xffa500),
    ("orange1", 0xffa500),
    ("orange2", 0xee9a00),
    ("orange3", 0xcd8500),
    ("orange4", 0x8b5a00),
    ("orchid", 0xda70d6),
    ("orchid1", 0xff83fa),
    ("orchid2", 0xee7ae9),
    ("orchid3", 0xcd69c9),
    ("orchid4", 0x8b4789),
    ("pale goldenrod", 0xeee8aa),
    ("pale green", 0x98fb98),
    ("pale turquoise", 0xafeeee),
    ("pale violet red", 0xdb7093),
    ("papaya whip", 0xffefd5),
    ("peach puff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("pink1", 0xffb5c5),
    ("pink2", 0xeea9b8),
    ("pink3", 0xcd919e),
    ("pink4", 0x8b636c),
    ("plum", 0xdda0dd),
    ("plum1", 0xffbbff),
    ("plum2", 0xeeaeee),
    ("plum3", 0xcd96cd),
    ("plum4", 0x8b668b),
    ("powder blue", 0xb0e0e6),
    ("purple", 0xa020f0),
    ("purple1", 0x9b30ff),
    ("purple2", 0x912cee),
    ("purple3", 0x7d26cd),
    ("purple4", 0x551a8b),
    ("rebecca purple", 0x663399),
    ("red", 0xff0000),
    ("red1", 0xff0000),
    ("red2", 0xee0000),
    ("red3", 0xcd0000),
    ("red4", 0x8b0000),
    ("rosy brown", 0xbc8f8f),
    ("royal blue", 0x4169e1),
    ("saddle brown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("salmon1", 0xff8c69),
    ("salmon2", 0xee8262),
    ("salmon3", 0xcd7054),
    ("salmon4", 0x8b4c39),
    ("sandy brown", 0xf4a460),
    ("sea green", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("seashell1", 0xfff5ee),
    ("seashell2", 0xeee5de),
    ("seashell3", 0xcdc5bf),
    ("seashell4", 0x8b8682),
    ("sienna", 0xa0522d),
    ("sienna1", 0xff8247),
    ("sienna2", 0xee7942),
    ("sienna3", 0xcd6839),
    ("sienna4", 0x8b4726),
    ("silver", 0xc0c0c0),
    ("sky blue", 0x87ceeb),
    ("slate blue", 0x6a5acd),
    ("slate gray", 0x708090),
    ("slate grey", 0x708090),
    ("snow", 0xfffafa),
    ("snow1", 0xfffafa),
    ("snow2", 0xeee9e9),
    ("snow3", 0xcdc9c9),
    ("snow4", 0x8b8989),
    ("spring green", 0x00ff7f),
    ("steel blue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("tan1", 0xffa54f),
    ("tan2", 0xee9a49),
    ("tan3", 0xcd853f),
    ("tan4", 0x8b5a2b),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("thistle1", 0xffe1ff),
    ("thistle2", 0xeed2ee),
    ("thistle3", 0xcdb5cd),
    ("thistle4", 0x8b7b8b),
    ("tomato", 0xff6347),
    ("tomato1", 0xff6347),
    ("tomato2", 0xee5c42),
    ("tomato3", 0xcd4f39),
    ("tomato4", 0x8b3626),
    ("turquoise", 0x40e0d0),
    ("turquoise1", 0x00f5ff),
    ("turquoise2", 0x00e5ee),
    ("turquoise3", 0x00c5cd),
    ("turquoise4", 0x00868b),
    ("violet red", 0xd02090),
    ("violet", 0xee82ee),
    ("web gray", 0x808080),
    ("web green", 0x008000),
    ("web grey", 0x808080),
    ("web maroon", 0x800000),
    ("web purple", 0x800080),
    ("wheat", 0xf5deb3),
    ("wheat1", 0xffe7ba),
    ("wheat2", 0xeed8ae),
    ("wheat3", 0xcdba96),
    ("wheat4", 0x8b7e66),
    ("white smoke", 0xf5f5f5),
    ("white", 0xffffff),
    ("x11 gray", 0xbebebe),
    ("x11 green", 0x00ff00),
    ("x11 grey", 0xbebebe),
    ("x11 maroon", 0xb03060),
    ("x11 purple", 0xa020f0),
    ("yellow green", 0x9acd32),
    ("yellow", 0xffff00),
    ("yellow1", 0xffff00),
    ("yellow2", 0xeeee00),
    ("yellow3", 0xcdcd00),
    ("yellow4", 0x8b8b00),
];
