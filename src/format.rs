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

/// Expand a format string.
pub fn expand(template: &str, vars: &dyn Vars) -> String {
    Expander { vars, now: Local::now() }.expand(template, 0)
}

/// Like expand, but also applies strftime(3) `%` sequences (status-left/right, display-message).
/// As in tmux, strftime is applied to the template before format expansion, so `%` characters
/// inside variable values (pane titles etc) are left alone.
pub fn expand_time(template: &str, vars: &dyn Vars) -> String {
    expand_time_at(template, vars, Local::now())
}

fn expand_time_at(template: &str, vars: &dyn Vars, now: DateTime<Local>) -> String {
    let t = strftime(template, &now);
    Expander { vars, now }.expand(&t, 0)
}

// ---------------------------------------------------------------------------------------------
// Format expansion
// ---------------------------------------------------------------------------------------------

struct Expander<'a> {
    vars: &'a dyn Vars,
    now: DateTime<Local>,
}

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
const MODIFIERS1: &str = "labdEeTtmnpqswC=!<>";
const COMPARE: [&str; 8] = ["==", "!=", "<", ">", "<=", ">=", "||", "&&"];

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
        } else if c.is_ascii_alphanumeric() || c == b'-' || c == b'.' {
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

fn truthy(s: &str) -> bool {
    !s.is_empty() && s != "0"
}

fn bool_str(b: bool) -> String {
    if b { "1".into() } else { "0".into() }
}

impl Expander<'_> {
    fn lookup(&self, name: &str) -> Option<String> {
        if let Some(v) = self.vars.get(name) {
            return Some(v);
        }
        if name == "host_short" {
            return self.vars.get("host").map(|h| h.split('.').next().unwrap_or("").to_string());
        }
        None
    }

    fn expand(&self, s: &str, depth: usize) -> String {
        if depth > MAX_DEPTH {
            return String::new();
        }
        let b = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        let mut lit = 0;
        while i < b.len() {
            if b[i] != b'#' || i + 1 >= b.len() {
                i += 1;
                continue;
            }
            out.push_str(&s[lit..i]);
            let c = b[i + 1];
            match c {
                b'{' => match skip(s, i, b"}") {
                    Some(end) => {
                        out.push_str(&self.replace(&s[i + 2..end], depth + 1));
                        i = end + 1;
                    }
                    None => {
                        out.push_str(&s[i..]);
                        i = b.len();
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
                    if j < b.len() {
                        let command = self.expand(&s[i + 2..j], depth + 1);
                        out.push_str(self.vars.shell(&command).unwrap_or_default().trim_end_matches(['\r', '\n']));
                    }
                    i = (j + 1).min(b.len());
                }
                b'#' => {
                    // Keep `##[` so the style parser sees an escaped `#[`.
                    out.push_str(if b.get(i + 2) == Some(&b'[') { "##" } else { "#" });
                    i += 2;
                }
                b',' | b'}' | b':' => {
                    out.push(c as char);
                    i += 2;
                }
                _ => {
                    if let Some(name) = alias(c) {
                        out.push_str(&self.lookup(name).unwrap_or_default());
                        i += 2;
                    } else {
                        // `#[` style directives and anything else pass through.
                        out.push('#');
                        i += 1;
                    }
                }
            }
            lit = i;
        }
        out.push_str(&s[lit..]);
        out
    }

    /// Evaluate the contents of one `#{...}`.
    fn replace(&self, content: &str, depth: usize) -> String {
        let (mods, copy) = parse_modifiers(content).unwrap_or((Vec::new(), content));
        let get = |n: &str| mods.iter().find(|m| m.name == n);
        let ex = |s: &str| self.expand(s, depth + 1);

        let mut value = if get("l").is_some() {
            copy.to_string()
        } else if get("C").is_some() {
            // TODO: pane content search is not supported.
            "0".into()
        } else if let Some(m) = get("m") {
            let Some((a, b)) = split2(copy) else { return String::new() };
            let flags = m.args.first().map(String::as_str).unwrap_or("");
            let icase = flags.contains('i');
            let (pat, text) = (ex(a), ex(b));
            let matched = if flags.contains('r') {
                Regex::new(&pat).is_some_and(|re| re.find(&text.chars().collect::<Vec<_>>(), 0, icase).is_some())
            } else {
                glob_match(&pat, &text, icase)
            };
            bool_str(matched)
        } else if let Some(m) = mods.iter().find(|m| COMPARE.contains(&m.name)) {
            let Some((a, b)) = split2(copy) else { return String::new() };
            let (a, b) = (ex(a), ex(b));
            bool_str(match m.name {
                "==" => a == b,
                "!=" => a != b,
                "<" => a < b,
                ">" => a > b,
                "<=" => a <= b,
                ">=" => a >= b,
                "||" => truthy(&a) || truthy(&b),
                _ => truthy(&a) && truthy(&b),
            })
        } else if get("!!").is_some() {
            bool_str(truthy(&ex(copy)))
        } else if get("!").is_some() {
            bool_str(!truthy(&ex(copy)))
        } else if let Some(m) = get("e") {
            let Some((a, b)) = split2(copy) else { return String::new() };
            match arith(&m.args, &ex(a), &ex(b)) {
                Some(v) => v,
                None => return String::new(),
            }
        } else if get("a").is_some() {
            ex(copy)
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|n| (32..127).contains(n))
                .and_then(char::from_u32)
                .map(String::from)
                .unwrap_or_default()
        } else if let Some(rest) = copy.strip_prefix('?') {
            let parts = split_commas(rest);
            let mut i = 0;
            let mut chosen = None;
            while i + 1 < parts.len() {
                let cond = match self.lookup(parts[i]) {
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
        } else {
            let mut v = match self.lookup(copy) {
                Some(v) => v,
                // Extension: allow `#{b:#{pane_current_path}}`-style nested formats.
                None if copy.contains('#') => ex(copy),
                None => String::new(),
            };
            if let Some(m) = get("t") {
                v = self.format_time(&v, &m.args);
            }
            if get("b").is_some() {
                v = basename(&v);
            }
            if get("d").is_some() {
                v = dirname(&v);
            }
            if get("q").is_some() {
                v = shell_quote(&v);
            }
            v
        };

        if get("E").is_some() {
            value = ex(&value);
        }
        if get("T").is_some() {
            value = strftime(&ex(&value), &self.now);
        }
        if let Some(m) = get("s")
            && m.args.len() >= 2
        {
            let flags = m.args.get(2).map(String::as_str).unwrap_or("");
            if let Some(re) = Regex::new(&ex(&m.args[0])) {
                value = re.replace_all(&value, &ex(&m.args[1]), flags.contains('i'));
            }
        }
        if let Some(m) = get("=")
            && let Some(n) = m.args.first().and_then(|a| ex(a).trim().parse::<i64>().ok())
        {
            let marker = m.args.get(1).map(|a| ex(a)).unwrap_or_default();
            let limit = n.unsigned_abs() as usize;
            if fmt_width(&value) > limit {
                value = if n >= 0 {
                    trim_left(&value, limit) + &marker
                } else {
                    marker + &trim_right(&value, limit)
                };
            }
        }
        if let Some(m) = get("p")
            && let Some(n) = m.args.first().and_then(|a| ex(a).trim().parse::<i64>().ok())
        {
            let w = fmt_width(&value);
            let target = n.unsigned_abs() as usize;
            if w < target {
                let pad = " ".repeat(target - w);
                value = if n > 0 { value + &pad } else { pad + &value };
            }
        }
        if get("n").is_some() {
            value = value.chars().count().to_string();
        }
        if get("w").is_some() {
            value = fmt_width(&value).to_string();
        }
        value
    }

    /// `t:` modifier: format a unix timestamp. `t/p` = pretty (short), `t/f/<fmt>` = strftime.
    fn format_time(&self, v: &str, args: &[String]) -> String {
        let Ok(secs) = v.trim().parse::<i64>() else { return String::new() };
        let Some(t) = Local.timestamp_opt(secs, 0).single() else { return String::new() };
        match args.first().map(String::as_str) {
            Some("f") => strftime(args.get(1).map(String::as_str).unwrap_or(""), &t),
            Some("p") => {
                let age = self.now.signed_duration_since(t);
                if age.num_seconds() >= 0 && age.num_hours() < 24 {
                    t.format("%H:%M").to_string()
                } else if t.year() == self.now.year() {
                    t.format("%d%b").to_string()
                } else {
                    t.format("%b%y").to_string()
                }
            }
            _ => t.format("%a %b %e %H:%M:%S %Y").to_string(),
        }
    }
}

fn arith(args: &[String], a: &str, b: &str) -> Option<String> {
    let op = args.first()?.as_str();
    let float = args.get(1).is_some_and(|f| f == "f");
    let prec: usize = if float { args.get(2).and_then(|p| p.parse().ok()).unwrap_or(2) } else { 0 };
    let mut x: f64 = a.trim().parse().ok()?;
    let mut y: f64 = b.trim().parse().ok()?;
    if !float {
        x = x.trunc();
        y = y.trunc();
    }
    let r = match op {
        "+" => x + y,
        "-" => x - y,
        "*" => x * y,
        "/" | "%" if y == 0.0 => return None,
        "/" => x / y,
        "%" => x % y,
        "==" => (x == y) as i32 as f64,
        "!=" => (x != y) as i32 as f64,
        "<" => (x < y) as i32 as f64,
        ">" => (x > y) as i32 as f64,
        "<=" => (x <= y) as i32 as f64,
        ">=" => (x >= y) as i32 as f64,
        _ => return None,
    };
    let r = if float { r } else { r.trunc() };
    Some(format!("{r:.prec$}"))
}

fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
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
        if "|&;<>()$`\\\"' *?[#=%".contains(c) {
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
fn glob_match(pat: &str, text: &str, icase: bool) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = text.chars().collect();
    fn go(p: &[char], t: &[char], icase: bool) -> bool {
        let Some(&c) = p.first() else { return t.is_empty() };
        match c {
            '*' => (0..=t.len()).any(|k| go(&p[1..], &t[k..], icase)),
            '?' => !t.is_empty() && go(&p[1..], &t[1..], icase),
            '[' => match parse_class(p, 1) {
                Some((r, neg, end)) => {
                    !t.is_empty() && class_has(&r, neg, t[0], icase) && go(&p[end..], &t[1..], icase)
                }
                None => !t.is_empty() && t[0] == '[' && go(&p[1..], &t[1..], icase),
            },
            '\\' if p.len() > 1 => !t.is_empty() && ceq(p[1], t[0], icase) && go(&p[2..], &t[1..], icase),
            _ => !t.is_empty() && ceq(c, t[0], icase) && go(&p[1..], &t[1..], icase),
        }
    }
    go(&p, &t, icase)
}

#[derive(Debug)]
enum Atom {
    Char(char),
    Any,
    Class(Vec<(char, char)>, bool),
    Start,
    End,
}

#[derive(Debug)]
struct Piece {
    atom: Atom,
    min: usize,
    max: usize,
}

/// Minimal POSIX-ish regex: literals, `.`, `[...]`, `^`, `$`, `*`, `+`, `?`, `\d \w \s`, `\x` escapes.
struct Regex(Vec<Piece>);

impl Regex {
    fn new(pat: &str) -> Option<Regex> {
        let p: Vec<char> = pat.chars().collect();
        let mut pieces: Vec<Piece> = Vec::new();
        let mut i = 0;
        while i < p.len() {
            let atom = match p[i] {
                '.' => Atom::Any,
                '^' if i == 0 => Atom::Start,
                '$' if i == p.len() - 1 => Atom::End,
                '[' => {
                    let (r, neg, end) = parse_class(&p, i + 1)?;
                    i = end - 1;
                    Atom::Class(r, neg)
                }
                '\\' => {
                    i += 1;
                    match *p.get(i)? {
                        'd' => Atom::Class(vec![('0', '9')], false),
                        'D' => Atom::Class(vec![('0', '9')], true),
                        'w' => Atom::Class(vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')], false),
                        'W' => Atom::Class(vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')], true),
                        's' => Atom::Class(vec![(' ', ' '), ('\t', '\r')], false),
                        'S' => Atom::Class(vec![(' ', ' '), ('\t', '\r')], true),
                        'n' => Atom::Char('\n'),
                        't' => Atom::Char('\t'),
                        c => Atom::Char(c),
                    }
                }
                '*' | '+' | '?' => {
                    // Quantifier on the previous atom.
                    let last = pieces.last_mut().filter(|l| l.min == 1 && l.max == 1)?;
                    if matches!(last.atom, Atom::Start | Atom::End) {
                        return None;
                    }
                    match p[i] {
                        '*' => (last.min, last.max) = (0, usize::MAX),
                        '+' => last.max = usize::MAX,
                        _ => last.min = 0,
                    }
                    i += 1;
                    continue;
                }
                c => Atom::Char(c),
            };
            pieces.push(Piece { atom, min: 1, max: 1 });
            i += 1;
        }
        Some(Regex(pieces))
    }

    fn atom_matches(atom: &Atom, c: char, icase: bool) -> bool {
        match atom {
            Atom::Char(a) => ceq(*a, c, icase),
            Atom::Any => true,
            Atom::Class(r, neg) => class_has(r, *neg, c, icase),
            Atom::Start | Atom::End => false,
        }
    }

    fn match_here(pieces: &[Piece], t: &[char], pos: usize, icase: bool) -> Option<usize> {
        let Some(pc) = pieces.first() else { return Some(pos) };
        match pc.atom {
            Atom::Start => return if pos == 0 { Self::match_here(&pieces[1..], t, pos, icase) } else { None },
            Atom::End => return if pos == t.len() { Some(pos) } else { None },
            _ => {}
        }
        let mut n = 0;
        while n < pc.max && pos + n < t.len() && Self::atom_matches(&pc.atom, t[pos + n], icase) {
            n += 1;
        }
        loop {
            if n < pc.min {
                return None;
            }
            if let Some(e) = Self::match_here(&pieces[1..], t, pos + n, icase) {
                return Some(e);
            }
            if n == 0 {
                return None;
            }
            n -= 1;
        }
    }

    fn find(&self, t: &[char], from: usize, icase: bool) -> Option<(usize, usize)> {
        (from..=t.len()).find_map(|s| Self::match_here(&self.0, t, s, icase).map(|e| (s, e)))
    }

    fn replace_all(&self, text: &str, rep: &str, icase: bool) -> String {
        let t: Vec<char> = text.chars().collect();
        let mut out = String::new();
        let mut pos = 0;
        while pos <= t.len() {
            let Some((s, e)) = self.find(&t, pos, icase) else { break };
            out.extend(&t[pos..s]);
            out.push_str(rep);
            if e == s {
                if s < t.len() {
                    out.push(t[s]);
                }
                pos = s + 1;
            } else {
                pos = e;
            }
        }
        if pos < t.len() {
            out.extend(&t[pos..]);
        }
        out
    }
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
}

pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim().to_ascii_lowercase();
    const NAMES: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
    if s == "default" || s == "terminal" {
        return Some(Color::Default);
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
    let num = s.strip_prefix("colour").or_else(|| s.strip_prefix("color")).unwrap_or(&s);
    if !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()) {
        return num.parse::<u8>().ok().map(Color::Idx);
    }
    None
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

/// Apply one style word. Returns Err for unknown words or bad values.
fn apply_word(word: &str, st: &mut Style, align: &mut Align, base: Style) -> Result<(), String> {
    let w = word.trim().to_ascii_lowercase();
    if w.is_empty() {
        return Ok(());
    }
    if let Some((key, val)) = w.split_once('=') {
        let color = || parse_color(val).ok_or_else(|| format!("bad colour: {val}"));
        match key {
            "fg" => st.fg = color()?,
            "bg" => st.bg = color()?,
            "us" | "fill" => {
                color()?;
            }
            "align" => {
                *align = match val {
                    "left" => Align::Left,
                    "centre" | "center" | "absolute-centre" | "absolute-center" => Align::Centre,
                    "right" => Align::Right,
                    _ => return Err(format!("bad align: {val}")),
                }
            }
            "list" | "range" | "width" | "pad" => {}
            _ => return Err(format!("unknown style: {word}")),
        }
        return Ok(());
    }
    match w.as_str() {
        "default" => *st = base,
        "none" => st.attrs = 0,
        "push-default" | "pop-default" | "nolist" | "norange" | "noalign" => {}
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

fn style_words(s: &str) -> impl Iterator<Item = &str> {
    s.split([',', ' ', '\t']).filter(|w| !w.is_empty())
}

/// Parse a style string like "fg=red,bg=colour235,bold,noitalics" applying it on top of `base`.
/// `default` resets to `base`. `align=`, `fill=`, `list=` etc are validated but have no effect.
pub fn parse_style(s: &str, base: Style) -> Result<Style, String> {
    let mut st = base;
    let mut align = Align::Left;
    for w in style_words(s) {
        apply_word(w, &mut st, &mut align, base)?;
    }
    Ok(st)
}

// ---------------------------------------------------------------------------------------------
// Spans and layout
// ---------------------------------------------------------------------------------------------

/// A run of text with one style, as produced from embedded `#[...]` style directives.
#[derive(Clone, PartialEq, Debug)]
pub struct Span {
    pub text: String,
    pub style: Style,
    pub align: Align,
}

/// Split already-expanded text into styled spans: `#[fg=red]a#[default]b`.
/// `#[default]` resets to the current default (initially `base`), `#[push-default]` makes the
/// current style the default, `#[pop-default]` restores the previous one. Unknown words are ignored.
pub fn spans(text: &str, base: Style) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut cur = base;
    let mut default = base;
    let mut defaults: Vec<Style> = Vec::new();
    let mut align = Align::Left;
    let mut buf = String::new();

    fn flush(out: &mut Vec<Span>, buf: &mut String, style: Style, align: Align) {
        if buf.is_empty() {
            return;
        }
        if let Some(last) = out.last_mut()
            && last.style == style
            && last.align == align
        {
            last.text.push_str(buf);
        } else {
            out.push(Span { text: buf.clone(), style, align });
        }
        buf.clear();
    }

    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with("##") {
            buf.push('#');
            i += 2;
        } else if rest.starts_with("#[")
            && let Some(e) = rest.find(']')
        {
            flush(&mut out, &mut buf, cur, align);
            for w in style_words(&rest[2..e]) {
                match w.to_ascii_lowercase().as_str() {
                    "push-default" => {
                        defaults.push(default);
                        default = cur;
                    }
                    "pop-default" => default = defaults.pop().unwrap_or(base),
                    _ => {
                        let _ = apply_word(w, &mut cur, &mut align, default);
                    }
                }
            }
            i += e + 1;
        } else {
            let c = rest.chars().next().unwrap();
            buf.push(c);
            i += c.len_utf8();
        }
    }
    flush(&mut out, &mut buf, cur, align);
    out
}

/// Terminal display width of a string.
pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Cells for a group of spans; wide characters occupy a lead cell plus an empty continuation.
fn group_cells<'a>(spans: impl Iterator<Item = &'a Span>) -> Vec<Cell> {
    let mut cells: Vec<Cell> = Vec::new();
    for sp in spans {
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
                    for _ in 1..w {
                        cells.push(Cell { text: String::new(), style: sp.style });
                    }
                }
            }
        }
    }
    cells
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
    let mut out = vec![Cell { text: " ".into(), style: fill }; width];
    let group = |a: Align| group_cells(spans.iter().filter(move |s| s.align == a));
    let (left, centre, right) = (group(Align::Left), group(Align::Centre), group(Align::Right));
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
    }
    if rw > 0 {
        put(&mut out, width - rw, &right, right.len() - rw, rw, fill);
    }
    if cw > 0 {
        let at = lw + (width - rw - lw) / 2 - cw / 2;
        let from = centre.len() / 2 - cw / 2;
        put(&mut out, at, &centre, from, cw, fill);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cells::{BLINK, BOLD, DIM, INVERSE, ITALIC, STRIKE, UNDERLINE};

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
        assert_eq!(x("a##b #, #} #:", &[]), "a#b , } :");
        assert_eq!(x("#", &[]), "#");
        assert_eq!(x("x#", &[]), "x#");
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
    fn unterminated_brace_is_literal() {
        assert_eq!(x("a#{session_name", &[("session_name", "s")]), "a#{session_name");
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
        let e = Expander { vars: &v, now: fixed_time() };
        assert_eq!(e.expand("#{T:fmt}", 0), "07:05 y");
        assert_eq!(e.expand("#{E:fmt}", 0), "%H:%M y");
    }

    #[test]
    fn basename_dirname() {
        let v = [("p", "/home/user/src/"), ("w", r"C:\Users\me\proj"), ("r", "/"), ("f", "file")];
        assert_eq!(x("#{b:p}", &v), "src");
        assert_eq!(x("#{d:p}", &v), "/home/user");
        assert_eq!(x("#{b:w}", &v), "proj");
        assert_eq!(x("#{d:w}", &v), r"C:\Users\me");
        assert_eq!(x("#{b:r}|#{d:r}", &v), "/|/");
        assert_eq!(x("#{b:f}|#{d:f}", &v), "file|.");
        assert_eq!(x("#{d:x}", &[("x", "/a")]), "/");
        assert_eq!(x("#{b:#{p}}", &v), "src");
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
        assert_eq!(x("#{e|/|f|2:#{a},#{b}}", &v), "3.50");
        assert_eq!(x("#{e|*|f|4:5.5,3}", &v), "16.5000");
        assert_eq!(x("#{e|/|:1,0}", &v), "");
        assert_eq!(x("#{e|+|:x,1}", &v), "");
        assert_eq!(x("#{e|<|:1,2}", &v), "1");
        assert_eq!(x("#{e|==|:2,2}", &v), "1");
        assert_eq!(x("#{e|-|:1,5}", &v), "-4");
    }

    #[test]
    fn length_width_ascii_quote() {
        let v = [("w", "漢字ab"), ("q", "it's a $x")];
        assert_eq!(x("#{n:w}", &v), "4");
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
        assert_eq!(parse_color("12"), Some(Color::Idx(12)));
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
        assert_eq!(parse_style("fg=default", base).unwrap().fg, Color::Default);
        assert_eq!(parse_style("", base).unwrap(), base);
        assert_eq!(parse_style("bright,hidden,double-underscore", base).unwrap().attrs, BOLD | UNDERLINE);
        assert_eq!(parse_style("align=right,fill=blue,list=on,FG=Blue", base).unwrap().fg, Color::Idx(4));
        assert!(parse_style("fg=mauve", base).is_err());
        assert!(parse_style("sparkly", base).is_err());
        assert!(parse_style("align=diagonal", base).is_err());
        assert!(parse_style("frobnicate=1", base).is_err());
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
                Span { text: "a".into(), style: st(Color::Idx(1), 0), align: Align::Left },
                Span { text: "b".into(), style: base, align: Align::Left },
            ]
        );
        // Merging of identical styles, `##` → `#`, `##[` is literal.
        let sp = spans("a#[bold]#[]b##c##[x]", base);
        assert_eq!(sp.len(), 2);
        assert_eq!(sp[0].text, "a");
        assert_eq!(sp[1].text, "b#c#[x]");
        assert_eq!(sp[1].style.attrs, BOLD);
        // none clears attrs only; unknown words ignored.
        let sp = spans("#[fg=blue,bold,sparkly]x#[none]y", base);
        assert_eq!(sp[0].style, st(Color::Idx(4), BOLD));
        assert_eq!(sp[1].style, st(Color::Idx(4), 0));
        // Unterminated directive is text.
        assert_eq!(spans("#[fg=red", base)[0].text, "#[fg=red");
        assert!(spans("", base).is_empty());
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
}
