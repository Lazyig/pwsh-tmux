//! The description tmux keeps of a client's terminal (struct tty_term): every capability
//! of tty_term_codes, filled from terminfo, then the terminal's features and
//! terminal-overrides (tty_term_create), as show-messages -T lists it.

use super::features::{fnmatch, override_tokens, Features, KNOWN};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    S,
    N,
    F,
}
use Kind::{F, N, S};

/// A capability's value (TTYCODE_NONE/STRING/NUMBER/FLAG).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Value {
    Missing,
    Str(String),
    Num(i64),
    Flag(bool),
}

/// tty_term_codes, in the order of tmux's enum tty_code_code (show-messages -T numbers them).
pub const CODES: &[(&str, Kind)] = &[
    ("acsc", S), ("am", F), ("AX", F), ("bce", F), ("bel", S), ("Bidi", S), ("blink", S),
    ("bold", S), ("civis", S), ("clear", S), ("Clmg", S), ("Cmg", S), ("cnorm", S), ("colors", N),
    ("Cr", S), ("Cs", S), ("csr", S), ("cub", S), ("cub1", S), ("cud", S), ("cud1", S), ("cuf", S),
    ("cuf1", S), ("cup", S), ("cuu", S), ("cuu1", S), ("cvvis", S), ("dch", S), ("dch1", S),
    ("dim", S), ("dl", S), ("dl1", S), ("Dsbp", S), ("Dseks", S), ("Dsesc", S), ("Dsfcs", S),
    ("Dsmg", S), ("E3", S), ("ech", S), ("ed", S), ("el", S), ("el1", S), ("enacs", S),
    ("Enbp", S), ("Eneks", S), ("Enesc", S), ("Enfcs", S), ("Enmg", S), ("fsl", S), ("Hls", S),
    ("home", S), ("hpa", S), ("ich", S), ("ich1", S), ("il", S), ("il1", S), ("ind", S),
    ("indn", S), ("invis", S), ("kcbt", S), ("kcub1", S), ("kcud1", S), ("kcuf1", S), ("kcuu1", S),
    ("kDC", S), ("kDC3", S), ("kDC4", S), ("kDC5", S), ("kDC6", S), ("kDC7", S), ("kdch1", S),
    ("kDN", S), ("kDN3", S), ("kDN4", S), ("kDN5", S), ("kDN6", S), ("kDN7", S), ("kend", S),
    ("kEND", S), ("kEND3", S), ("kEND4", S), ("kEND5", S), ("kEND6", S), ("kEND7", S), ("kf1", S),
    ("kf10", S), ("kf11", S), ("kf12", S), ("kf13", S), ("kf14", S), ("kf15", S), ("kf16", S),
    ("kf17", S), ("kf18", S), ("kf19", S), ("kf2", S), ("kf20", S), ("kf21", S), ("kf22", S),
    ("kf23", S), ("kf24", S), ("kf25", S), ("kf26", S), ("kf27", S), ("kf28", S), ("kf29", S),
    ("kf3", S), ("kf30", S), ("kf31", S), ("kf32", S), ("kf33", S), ("kf34", S), ("kf35", S),
    ("kf36", S), ("kf37", S), ("kf38", S), ("kf39", S), ("kf4", S), ("kf40", S), ("kf41", S),
    ("kf42", S), ("kf43", S), ("kf44", S), ("kf45", S), ("kf46", S), ("kf47", S), ("kf48", S),
    ("kf49", S), ("kf5", S), ("kf50", S), ("kf51", S), ("kf52", S), ("kf53", S), ("kf54", S),
    ("kf55", S), ("kf56", S), ("kf57", S), ("kf58", S), ("kf59", S), ("kf6", S), ("kf60", S),
    ("kf61", S), ("kf62", S), ("kf63", S), ("kf7", S), ("kf8", S), ("kf9", S), ("kHOM", S),
    ("kHOM3", S), ("kHOM4", S), ("kHOM5", S), ("kHOM6", S), ("kHOM7", S), ("khome", S), ("kIC", S),
    ("kIC3", S), ("kIC4", S), ("kIC5", S), ("kIC6", S), ("kIC7", S), ("kich1", S), ("kind", S),
    ("kLFT", S), ("kLFT3", S), ("kLFT4", S), ("kLFT5", S), ("kLFT6", S), ("kLFT7", S),
    ("kmous", S), ("knp", S), ("kNXT", S), ("kNXT3", S), ("kNXT4", S), ("kNXT5", S), ("kNXT6", S),
    ("kNXT7", S), ("kpp", S), ("kPRV", S), ("kPRV3", S), ("kPRV4", S), ("kPRV5", S), ("kPRV6", S),
    ("kPRV7", S), ("kri", S), ("kRIT", S), ("kRIT3", S), ("kRIT4", S), ("kRIT5", S), ("kRIT6", S),
    ("kRIT7", S), ("kUP", S), ("kUP3", S), ("kUP4", S), ("kUP5", S), ("kUP6", S), ("kUP7", S),
    ("Ms", S), ("Nobr", S), ("ol", S), ("op", S), ("Rect", S), ("rev", S), ("RGB", F), ("ri", S),
    ("rin", S), ("rmacs", S), ("rmcup", S), ("rmkx", S), ("Se", S), ("setab", S), ("setaf", S),
    ("setal", S), ("setrgbb", S), ("setrgbf", S), ("Setulc", S), ("Setulc1", S), ("sgr0", S),
    ("sitm", S), ("smacs", S), ("smcup", S), ("smkx", S), ("Smol", S), ("smso", S), ("smul", S),
    ("Smulx", S), ("smxx", S), ("Spb", S), ("Sxl", F), ("Ss", S), ("Swd", S), ("Sync", S),
    ("Tc", F), ("tsl", S), ("U8", N), ("vpa", S), ("XT", F),

];

/// The capabilities each terminal feature adds (tty_features, in its order: features::KNOWN);
/// cstyle (TERM_NOREPLACE) keeps what terminfo has.
fn feature_capabilities(name: &str) -> &'static [&'static str] {
    const SETAB: &str = "setab=\\E[%?%p1%{8}%<%t4%p1%d%e%p1%{16}%<%t10%p1%{8}%-%d%e48;5;%p1%d%;m";
    const SETAF: &str = "setaf=\\E[%?%p1%{8}%<%t3%p1%d%e%p1%{16}%<%t9%p1%{8}%-%d%e38;5;%p1%d%;m";
    const KF: &[&str] = &[
        "kf0@", "kf1@", "kf2@", "kf3@", "kf4@", "kf5@", "kf6@", "kf7@", "kf8@", "kf9@", "kf10@", "kf11@", "kf12@", "kf13@", "kf14@",
        "kf15@", "kf16@", "kf17@", "kf18@", "kf19@", "kf20@", "kf21@", "kf22@", "kf23@", "kf24@", "kf25@", "kf26@", "kf27@", "kf28@",
        "kf29@", "kf30@", "kf31@", "kf32@", "kf33@", "kf34@", "kf35@", "kf36@", "kf37@", "kf38@", "kf39@", "kf40@", "kf41@", "kf42@",
        "kf43@", "kf44@", "kf45@", "kf46@", "kf47@", "kf48@", "kf49@", "kf50@", "kf51@", "kf52@", "kf53@", "kf54@", "kf55@", "kf56@",
        "kf57@", "kf58@", "kf59@", "kf60@", "kf61@", "kf62@", "kf63@",
    ];
    match name {
        "256" => &["AX", SETAB, SETAF],
        "appesc" => &["Enesc=\\E[?7727h", "Dsesc=\\E[?7727l"],
        "bpaste" => &["Enbp=\\E[?2004h", "Dsbp=\\E[?2004l"],
        "ccolour" => &["Cs=\\E]12;%p1%s\\a", "Cr=\\E]112\\a"],
        "clipboard" => &["Ms=\\E]52;%p1%s;%p2%s\\a"],
        "hyperlinks" => &["Hls=\\E]8;%?%p1%l%tid=%p1%s%;;%p2%s\\E\\\\"],
        "cstyle" => &["Ss=\\E[%p1%d q", "Se=\\E[2 q"],
        "extkeys" => &["Eneks=\\E[>4;2m", "Dseks=\\E[>4m"],
        "focus" => &["Enfcs=\\E[?1004h", "Dsfcs=\\E[?1004l"],
        "ignorefkeys" => KF,
        "margins" => &["Enmg=\\E[?69h", "Dsmg=\\E[?69l", "Clmg=\\E[s", "Cmg=\\E[%i%p1%d;%p2%ds"],
        "mouse" => &["kmous=\\E[M"],
        "osc7" => &["Swd=\\E]7;", "fsl=\\a"],
        "overline" => &["Smol=\\E[53m"],
        "progressbar" => &["Spb=\\E]9;4;%p1%d;%p2%d\\E\\\\"],
        "rectfill" => &["Rect"],
        "RGB" => &["AX", "setrgbf=\\E[38;2;%p1%d;%p2%d;%p3%dm", "setrgbb=\\E[48;2;%p1%d;%p2%d;%p3%dm", SETAB, SETAF],
        "sixel" => &["Sxl"],
        "strikethrough" => &["smxx=\\E[9m"],
        "sync" => &["Sync=\\E[?2026%?%p1%{1}%-%tl%eh%;"],
        "title" => &["tsl=\\E]0;", "fsl=\\a"],
        "usstyle" => &[
            "Smulx=\\E[4::%p1%dm",
            "Setulc=\\E[58::2::%p1%{65536}%/%d::%p1%{256}%/%{255}%&%d::%p1%{255}%&%d%;m",
            "Setulc1=\\E[58::5::%p1%dm",
            "ol=\\E[59m",
        ],
        _ => &[],
    }
}

/// The capabilities of `term` after tty_term_create: terminfo (what tty_term_read_list
/// sends), terminal-overrides, the features' capabilities, terminal-overrides again.
pub fn describe(term: &str, overrides: &[String], features: &Features) -> Vec<Value> {
    let mut codes = vec![Value::Missing; CODES.len()];
    for (name, value) in terminfo(term) {
        if let Some(i) = CODES.iter().position(|c| c.0 == name) {
            codes[i] = match (CODES[i].1, value) {
                (S, Value::Str(s)) => Value::Str(strip_padding(&s)),
                (N, v @ Value::Num(_)) | (F, v @ Value::Flag(_)) => v,
                _ => Value::Missing,
            };
        }
    }
    let apply_overrides = |codes: &mut Vec<Value>| {
        for entry in overrides {
            let parts = override_tokens(entry);
            if parts.first().is_some_and(|p| fnmatch(p, term)) {
                apply(codes, &parts[1..], false);
            }
        }
    };
    apply_overrides(&mut codes);
    for f in KNOWN.iter().filter(|f| features.0.contains(**f)) {
        let caps: Vec<String> = feature_capabilities(f).iter().map(|c| override_tokens(c).concat()).collect();
        apply(&mut codes, &caps, *f == "cstyle");
    }
    apply_overrides(&mut codes);
    codes
}

/// tty_term_apply: `name=value` (vis-decoded), `name@` removes, `name` alone sets a flag
/// (or an empty string).
fn apply(codes: &mut [Value], caps: &[String], noreplace: bool) {
    for cap in caps.iter().filter(|c| !c.is_empty()) {
        let (name, value) = match cap.split_once('=') {
            Some((n, v)) => (n, Some(unvis(v))),
            None if cap.ends_with('@') => (&cap[..cap.len() - 1], None),
            None => (cap.as_str(), Some(String::new())),
        };
        let Some(i) = CODES.iter().position(|c| c.0 == name) else { continue };
        if noreplace && codes[i] != Value::Missing {
            continue;
        }
        let Some(value) = value else {
            codes[i] = Value::Missing;
            continue;
        };
        match CODES[i].1 {
            S => codes[i] = Value::Str(value),
            N => {
                if let Ok(n) = value.parse::<i64>().map_err(drop).and_then(|n| if (0..=i32::MAX as i64).contains(&n) { Ok(n) } else { Err(()) }) {
                    codes[i] = Value::Num(n);
                }
            }
            F => codes[i] = Value::Flag(true),
        }
    }
}

/// strunvis for override values: \E and \e are ESC, the C escapes, \^X and octal.
fn unvis(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('E' | 'e') => out.push('\x1b'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('a') => out.push('\x07'),
            Some('b') => out.push('\x08'),
            Some('f') => out.push('\x0c'),
            Some('v') => out.push('\x0b'),
            Some('s') => out.push(' '),
            Some('^') => match chars.next() {
                Some('?') => out.push('\x7f'),
                Some(c) => out.push(((c as u8) & 31) as char),
                None => {}
            },
            Some(c @ '0'..='7') => {
                let mut n = c.to_digit(8).unwrap();
                for _ in 0..2 {
                    match chars.peek().and_then(|c| c.to_digit(8)) {
                        Some(d) => {
                            n = n * 8 + d;
                            chars.next();
                        }
                        None => break,
                    }
                }
                out.extend(char::from_u32(n & 0xff));
            }
            Some(c) => out.push(c),
            None => out.push('\\'),
        }
    }
    out
}

/// tty_term_strip: terminfo $<..> padding goes.
fn strip_padding(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("$<") {
        out.push_str(&rest[..i]);
        rest = match rest[i..].find('>') {
            Some(j) => &rest[i + j + 1..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// A terminfo entry's capabilities (infocmp -1 -x), cached per terminal name.
#[cfg(unix)]
fn terminfo(term: &str) -> Vec<(String, Value)> {
    use std::collections::BTreeMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<BTreeMap<String, Vec<(String, Value)>>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
    if let Some(caps) = cache.get(term) {
        return caps.clone();
    }
    let mut caps = Vec::new();
    if !term.is_empty() && !term.starts_with('-') {
        if let Ok(output) = std::process::Command::new("infocmp").args(["-1", "-x", term]).output() {
            if output.status.success() {
                caps = parse_infocmp(&String::from_utf8_lossy(&output.stdout));
            }
        }
    }
    if cache.len() < 128 {
        cache.insert(term.to_string(), caps.clone());
    }
    caps
}

/// Windows consoles have no terminfo database: only features and overrides apply.
#[cfg(windows)]
fn terminfo(_: &str) -> Vec<(String, Value)> {
    Vec::new()
}

/// The capability lines of `infocmp -1` output (each indented, ending in ",").
#[cfg_attr(windows, allow(dead_code))]
fn parse_infocmp(text: &str) -> Vec<(String, Value)> {
    let mut caps = Vec::new();
    for line in text.lines().filter(|l| l.starts_with([' ', '\t'])) {
        let line = line.trim();
        let line = line.strip_suffix(',').unwrap_or(line);
        if let Some((name, value)) = line.split_once('=') {
            caps.push((name.to_string(), Value::Str(decode_terminfo(value))));
        } else if let Some((name, n)) = line.split_once('#') {
            let n = if let Some(h) = n.strip_prefix("0x") {
                i64::from_str_radix(h, 16).ok()
            } else if n.len() > 1 && n.starts_with('0') {
                i64::from_str_radix(&n[1..], 8).ok()
            } else {
                n.parse().ok()
            };
            if let Some(n) = n {
                caps.push((name.to_string(), Value::Num(n)));
            }
        } else if !line.ends_with('@') && !line.is_empty() {
            caps.push((line.to_string(), Value::Flag(true)));
        }
    }
    caps
}

/// terminfo(5) string escapes as infocmp prints them.
#[cfg_attr(windows, allow(dead_code))]
fn decode_terminfo(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('E' | 'e') => out.push('\x1b'),
                Some('n' | 'l') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('b') => out.push('\x08'),
                Some('f') => out.push('\x0c'),
                Some('s') => out.push(' '),
                Some(c @ '0'..='7') => {
                    let mut n = c.to_digit(8).unwrap();
                    for _ in 0..2 {
                        match chars.peek().and_then(|c| c.to_digit(8)) {
                            Some(d) => {
                                n = n * 8 + d;
                                chars.next();
                            }
                            None => break,
                        }
                    }
                    // terminfo stores NUL as \200
                    out.extend(char::from_u32(if n == 0 { 0x80 } else { n & 0xff }));
                }
                Some(c) => out.push(c),
                None => out.push('\\'),
            },
            '^' => match chars.next() {
                Some('?') => out.push('\x7f'),
                Some(c) => out.push(((c as u8) & 31) as char),
                None => out.push('^'),
            },
            c => out.push(c),
        }
    }
    out
}

/// The terminal a client runs on, as tmux names a client (ttyname of its stdin).
pub fn tty_path(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let p = std::fs::read_link(format!("/proc/{pid}/fd/0")).ok()?;
        let p = p.to_string_lossy().into_owned();
        return p.starts_with("/dev/").then_some(p);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_like_tty_term_create() {
        assert_eq!(CODES.len(), 236);
        assert_eq!((CODES[5].0, CODES[13].0, CODES[235].0), ("Bidi", "colors", "XT"));
        let info = "#\tReconstructed\nxterm-x|test,\n\tam,\n\tbce@,\n\tcolors#0x100,\n\tcols#80,\n\tSe=\\E[2 q,\n\tclear=\\E[H\\E[2J$<50>,\n\tacsc=``aa,\n\tkbs=^H,\n";
        let caps = parse_infocmp(info);
        assert!(caps.contains(&("am".into(), Value::Flag(true))) && !caps.iter().any(|c| c.0 == "bce"));
        assert!(caps.contains(&("colors".into(), Value::Num(256))));
        assert!(caps.contains(&("kbs".into(), Value::Str("\x08".into()))));
        let mut codes = vec![Value::Missing; CODES.len()];
        let at = |n: &str| CODES.iter().position(|c| c.0 == n).unwrap();
        codes[at("Se")] = Value::Str("\x1b[2 q".into());
        // cstyle does not replace, ccolour does; overrides: = string, @ removes, bare flag
        apply(&mut codes, &["Ss=\\E[%p1%d q".into(), "Se=x".into()], true);
        assert_eq!(codes[at("Se")], Value::Str("\x1b[2 q".into()));
        assert_eq!(codes[at("Ss")], Value::Str("\x1b[%p1%d q".into()));
        apply(&mut codes, &["Se@".into(), "Tc".into(), "colors=16".into(), "colors=x".into()], false);
        assert_eq!((&codes[at("Se")], &codes[at("Tc")], &codes[at("colors")]), (&Value::Missing, &Value::Flag(true), &Value::Num(16)));
        assert_eq!(strip_padding("\x1b[H$<50>\x1b[2J$<1/>"), "\x1b[H\x1b[2J");
        // the features' capabilities ("::" is a literal colon)
        let mut f = Features::default();
        f.add("usstyle,mouse");
        let d = describe("pwsh-tmux-test-none", &["pwsh-tmux-test-*:Smol=\\E[53m:kmous@".into()], &f);
        assert_eq!(d[at("Smulx")], Value::Str("\x1b[4:%p1%dm".into()));
        assert_eq!(d[at("Smol")], Value::Str("\x1b[53m".into()));
        assert_eq!(d[at("kmous")], Value::Missing);
    }
}
