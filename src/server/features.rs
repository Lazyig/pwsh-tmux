//! Terminal features of a client's outer terminal (tmux `terminal-features` / `-T`):
//! what the renderer may send it.

use std::collections::BTreeSet;

#[allow(dead_code)]
pub const KNOWN: &[&str] = &[
    "256", "RGB", "bpaste", "ccolour", "clipboard", "cstyle", "extkeys", "focus", "hyperlinks", "ignorefkeys", "margins", "mouse", "osc7", "overline",
    "rectfill", "strikethrough", "sync", "title", "usstyle", "utf8", "appesc",
];

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Features(pub BTreeSet<String>);

impl Features {
    pub fn has(&self, f: &str) -> bool {
        self.0.contains(f)
    }

    fn add(&mut self, list: &str) {
        for f in list.split([',', ':']).map(str::trim).filter(|f| !f.is_empty()) {
            if let Some(name) = f.strip_suffix('@').or_else(|| f.strip_prefix('@')) {
                self.0.remove(name);
                continue;
            }
            // tmux aliases
            let f = match f {
                "Tc" | "24bit" | "truecolor" => "RGB",
                "256color" => "256",
                other => other,
            };
            if f == "RGB" {
                self.0.insert("256".into());
            }
            self.0.insert(f.to_string());
        }
    }

    pub fn list(&self) -> String {
        self.0.iter().cloned().collect::<Vec<_>>().join(",")
    }
}

pub fn fnmatch(pat: &str, s: &str) -> bool {
    fn m(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => m(&p[1..], s) || (!s.is_empty() && m(p, &s[1..])),
            (Some('?'), Some(_)) => m(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => m(&p[1..], &s[1..]),
            _ => false,
        }
    }
    m(&pat.chars().collect::<Vec<_>>(), &s.chars().collect::<Vec<_>>())
}

/// `term`: the client's TERM (empty for a Windows console client), `colorterm`: its COLORTERM,
/// `table`: terminal-features entries ("pattern:feature:feature"), `extra`: the `-T` list.
#[cfg(test)]
pub fn compute(term: &str, colorterm: &str, table: &[String], extra: &str) -> Features {
    compute_named(term, "", colorterm, table, extra)
}

pub fn compute_named(term: &str, program: &str, colorterm: &str, table: &[String], extra: &str) -> Features {
    let mut f = Features::default();
    if term.is_empty() {
        // Windows console / Windows Terminal: a modern VT implementation
        f.add("utf8,RGB,mouse,bpaste,title,ccolour,cstyle,focus,clipboard,strikethrough,sync,usstyle,overline,hyperlinks");
    } else if term != "dumb" {
        f.add("utf8");
        if term != "linux" && !term.starts_with("vt1") {
            f.add("mouse,bpaste,strikethrough");
        }
        if term.contains("256col") || term.contains("direct") || term.starts_with("tmux") || term.starts_with("alacritty") || term.starts_with("xterm-kitty") {
            f.add("256");
        }
        if term.contains("direct") {
            f.add("RGB");
        }
    }
    if matches!(colorterm, "truecolor" | "24bit") {
        f.add("RGB");
    }
    if term.to_ascii_lowercase().contains("wezterm") || term.contains("ghostty") || matches!(program.to_ascii_lowercase().as_str(), "wezterm" | "ghostty") {
        f.add("RGB,clipboard,cstyle,ccolour,extkeys,focus,margins,usstyle,hyperlinks,utf8");
    }
    for entry in table {
        let mut parts = entry.splitn(2, ':');
        let pat = parts.next().unwrap_or("");
        if !pat.is_empty() && fnmatch(pat, term) {
            f.add(parts.next().unwrap_or(""));
        }
    }
    f.add(extra);
    f
}

/// Nearest xterm-256 colour for an RGB value.
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    let cube = |v: u8| -> u8 { if v < 48 { 0 } else if v < 115 { 1 } else { (v - 35) / 40 } };
    let (cr, cg, cb) = (cube(r), cube(g), cube(b));
    let level = |c: u8| -> u8 { if c == 0 { 0 } else { 55 + c * 40 } };
    let cube_idx = 16 + 36 * cr + 6 * cg + cb;
    let cube_dist = dist((r, g, b), (level(cr), level(cg), level(cb)));
    let avg = (r as u32 + g as u32 + b as u32) / 3;
    let gray_i = if avg > 238 { 23 } else { (avg.saturating_sub(3) / 10) as u8 };
    let gv = 8 + 10 * gray_i;
    if dist((r, g, b), (gv, gv, gv)) < cube_dist { 232 + gray_i } else { cube_idx }
}

fn dist(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2) as u32;
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

const ANSI16: [(u8, u8, u8); 16] = [
    (0, 0, 0), (205, 0, 0), (0, 205, 0), (205, 205, 0), (0, 0, 238), (205, 0, 205), (0, 205, 205), (229, 229, 229),
    (127, 127, 127), (255, 0, 0), (0, 255, 0), (255, 255, 0), (92, 92, 255), (255, 0, 255), (0, 255, 255), (255, 255, 255),
];

pub fn idx_to_rgb(i: u8) -> (u8, u8, u8) {
    match i {
        0..=15 => ANSI16[i as usize],
        16..=231 => {
            let i = i - 16;
            let l = |c: u8| if c == 0 { 0 } else { 55 + c * 40 };
            (l(i / 36), l((i / 6) % 6), l(i % 6))
        }
        _ => {
            let v = 8 + 10 * (i - 232);
            (v, v, v)
        }
    }
}

pub fn rgb_to_16(r: u8, g: u8, b: u8) -> u8 {
    (0..16u8).min_by_key(|i| dist((r, g, b), ANSI16[*i as usize])).unwrap()
}

/// Downgrades a colour to what the terminal can show.
pub fn fit_color(c: vt100::Color, f: &Features) -> vt100::Color {
    use vt100::Color::*;
    match c {
        Rgb(r, g, b) if !f.has("RGB") => {
            if f.has("256") {
                Idx(rgb_to_256(r, g, b))
            } else {
                Idx(rgb_to_16(r, g, b))
            }
        }
        Idx(i) if i >= 16 && !f.has("256") => {
            let (r, g, b) = idx_to_rgb(i);
            Idx(rgb_to_16(r, g, b))
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_features() {
        let table = vec!["xterm*:clipboard:ccolour:cstyle:focus:title".to_string(), "screen*:title".to_string()];
        let x = compute("xterm-256color", "", &table, "");
        assert!(x.has("256") && x.has("clipboard") && x.has("mouse") && !x.has("RGB"));
        assert!(compute("xterm-256color", "truecolor", &table, "").has("RGB"));
        let l = compute("linux", "", &table, "");
        assert!(!l.has("256") && !l.has("mouse"));
        assert!(compute("linux", "", &table, "RGB,sync").has("sync"));
        let w = compute("", "", &table, "");
        assert!(w.has("RGB") && w.has("sync"));
        assert!(compute("screen", "", &table, "Tc").has("RGB"));
    }

    #[test]
    fn colour_downgrade() {
        assert_eq!(rgb_to_256(255, 0, 0), 196);
        assert_eq!(rgb_to_256(128, 128, 128), 244);
        assert_eq!(rgb_to_16(250, 10, 10), 9);
        let only16 = Features::default();
        assert_eq!(fit_color(vt100::Color::Idx(196), &only16), vt100::Color::Idx(9));
        let mut c256 = Features::default();
        c256.add("256");
        assert_eq!(fit_color(vt100::Color::Rgb(0, 0, 0), &c256), vt100::Color::Idx(16));
    }
}

/// Apply terminfo overrides to the capabilities used by our VT renderer.
/// String capabilities are retained so an explicit Ss/Se is never replaced by cstyle.
pub fn overrides(term: &str, table: &[String], features: &mut Features) -> std::collections::BTreeMap<String, String> {
    let mut caps = terminfo_capabilities(term);
    for entry in table {
        let mut parts = entry.split(':');
        if !fnmatch(parts.next().unwrap_or(""), term) { continue; }
        for cap in parts {
            let (name, value) = cap.split_once('=').map(|(n, v)| (n, v.to_string()))
                .unwrap_or_else(|| (cap.trim_end_matches('@'), if cap.ends_with('@') { String::new() } else { "1".into() }));
            let feature = match name {
                "Tc" | "RGB" => "RGB", "colors" => "256", "Ss" | "Se" => "cstyle", "Cs" | "Cr" => "ccolour",
                "Ms" => "clipboard", "Enbp" | "Dsbp" => "bpaste", "XT" => "title", "Dsfcs" | "Enfcs" => "focus",
                _ => "",
            };
            if !feature.is_empty() {
                if value.is_empty() || (name == "colors" && value.parse::<u32>().unwrap_or(0) < 256) { features.0.remove(feature); }
                else { features.add(feature); }
            }
            caps.insert(name.to_string(), decode_capability(&value));
        }
    }
    caps
}

fn decode_capability(s: &str) -> String {
    let mut out = String::new(); let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some('E' | 'e') => out.push('\x1b'), Some('n') => out.push('\n'), Some('r') => out.push('\r'),
                Some('t') => out.push('\t'), Some(c @ '0'..='7') => {
                    let mut n = c.to_digit(8).unwrap();
                    for _ in 0..2 { if chars.clone().next().is_some_and(|c| ('0'..='7').contains(&c)) { n = n * 8 + chars.next().unwrap().to_digit(8).unwrap(); } }
                    if let Some(c) = char::from_u32(n) { out.push(c); }
                }
                Some(c) => out.push(c), None => out.push('\\'),
            },
            '^' => if let Some(c) = chars.next() { out.push(if c == '?' { '\x7f' } else { ((c as u8) & 31) as char }); },
            c => out.push(c),
        }
    }
    out
}

pub fn cursor_style(s: &str) -> u8 {
    match s { "blinking-block" => 1, "block" => 2, "blinking-underline" => 3, "underline" => 4, "blinking-bar" => 5, "bar" => 6, _ => 0 }
}

pub fn cursor_sequence(caps: &std::collections::BTreeMap<String, String>, style: u8) -> String {
    if style == 0 { if let Some(se) = caps.get("Se") { return se.clone(); } }
    string_capability(caps, "Ss", "\x1b[%p1%d q", &[&style.to_string()])
}

/// tmux ignores malformed width entries, including reversed or invalid ranges.
pub fn codepoint_widths(entries: &[String]) -> Vec<(u32, u32, u16)> {
    entries.iter().filter_map(|s| {
        let (range, width) = s.split_once('=')?;
        let width = width.parse::<u16>().ok().filter(|n| *n <= 2)?;
        let cp = |s: &str| s.strip_prefix("U+").and_then(|s| u32::from_str_radix(s, 16).ok()).filter(|n| *n > 0 && *n <= 0x10ffff);
        let (a, b) = if range.starts_with("U+") {
            if let Some((a, b)) = range.split_once('-') { (cp(a)?, cp(b)?) } else { let n = cp(range)?; (n, n) }
        } else { let mut chars = range.chars(); let ch = chars.next()?; if chars.next().is_some() { return None; } (ch as u32, ch as u32) };
        (a <= b).then_some((a, b, width))
    }).collect()
}

/// Windows consoles have no terminfo database; use their native VT capabilities.
#[cfg(windows)]
fn terminfo_capabilities(_: &str) -> std::collections::BTreeMap<String, String> { Default::default() }

#[cfg(unix)]
fn terminfo_capabilities(term: &str) -> std::collections::BTreeMap<String, String> {
    use std::collections::BTreeMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<BTreeMap<String, BTreeMap<String, String>>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
    if let Some(caps) = cache.get(term) { return caps.clone(); }
    let mut caps = BTreeMap::new();
    if !term.is_empty() && !term.starts_with('-') {
        if let Ok(output) = std::process::Command::new("infocmp").args(["-1", "-x", term]).output() {
            if output.status.success() {
                for line in String::from_utf8_lossy(&output.stdout).lines() {
                    let line = line.trim().trim_end_matches(',');
                    if let Some((name, value)) = line.split_once('=') {
                        caps.insert(name.to_string(), decode_capability(value));
                    }
                }
            }
        }
    }
    if cache.len() < 128 { cache.insert(term.to_string(), caps.clone()); }
    caps
}

/// Expand the parameter forms used by VT cursor and selection capabilities.
pub fn string_capability(caps: &std::collections::BTreeMap<String, String>, name: &str, fallback: &str, params: &[&str]) -> String {
    super::termcap::expand(caps.get(name).map(String::as_str).unwrap_or(fallback), params).unwrap_or_default()
}
