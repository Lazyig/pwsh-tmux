//! Terminal features of a client's outer terminal (tmux `terminal-features` / `-T`):
//! what the renderer may send it.

use std::collections::BTreeSet;

/// tmux's tty_features table, in its order (client_termfeatures lists features this way).
pub const KNOWN: &[&str] = &[
    "256", "appesc", "bpaste", "ccolour", "clipboard", "hyperlinks", "cstyle", "extkeys", "focus", "ignorefkeys", "margins", "mouse", "osc7",
    "overline", "progressbar", "rectfill", "RGB", "sixel", "strikethrough", "sync", "title", "usstyle", "utf8",
];

/// Enabled features and the ones removed with `feature@` (term_features / term_nofeatures).
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Features(pub BTreeSet<String>, pub BTreeSet<String>);

impl Features {
    pub fn has(&self, f: &str) -> bool {
        // RGB implies 256 colours (TERM_256COLOURS) without listing the 256 feature
        self.0.contains(f) || (f == "256" && self.0.contains("RGB"))
    }

    pub(super) fn add(&mut self, list: &str) {
        self.parse(list, &[',', ':']);
    }

    /// The terminfo colors stand-ins ("t256", "16", "mono"): not features, so never listed.
    fn set_colours(&mut self, cap: &str) {
        self.0.insert(cap.to_string());
    }

    /// tty_parse_features: a trailing `@` removes a feature and keeps later lists from adding it
    /// back; an unknown name ends the list.
    pub fn parse(&mut self, list: &str, sep: &[char]) {
        for f in list.split(sep) {
            let (name, remove) = match f.strip_suffix('@') { Some(n) => (n, true), None => (f, false) };
            // pwsh-tmux aliases for RGB and 256
            let name = match name { "Tc" | "24bit" | "truecolor" => "RGB", "256color" => "256", other => other };
            let Some(known) = KNOWN.iter().find(|k| k.eq_ignore_ascii_case(name)) else { break };
            if remove {
                self.0.remove(*known);
                self.1.insert(known.to_string());
            } else if !self.1.contains(*known) {
                self.0.insert(known.to_string());
            }
        }
    }

    /// tty_keys_device_attributes2 / tty_keys_extended_device_attributes.
    pub fn device_report(&mut self, raw: &[u8]) -> bool {
        let name = if let Some(body) = raw.strip_prefix(b"\x1bP>|").and_then(|s| s.strip_suffix(b"\x1b\\")) {
            let body = String::from_utf8_lossy(body);
            [("iTerm2 ", "iTerm2"), ("tmux ", "tmux"), ("XTerm(", "XTerm"), ("mintty ", "mintty"),
                ("foot(", "foot"), ("WezTerm ", "WezTerm"), ("ghostty ", "ghostty"), ("Rio ", "Rio")]
                .into_iter().find(|(prefix, _)| body.starts_with(prefix)).map(|(_, name)| name)
        } else if let Some(body) = raw.strip_prefix(b"\x1b[>").and_then(|s| s.strip_suffix(b"c")) {
            match body.split(|b| *b == b';').next() {
                Some(b"77") => Some("mintty"), Some(b"84") => Some("tmux"), Some(b"85") => Some("rxvt-unicode"), _ => None,
            }
        } else { return false };
        for list in name.map(default_features).unwrap_or(&[]) {
            self.add(list);
        }
        true
    }

    /// client_colours (format_cb_client_colours): only the RGB and 256 features count as
    /// more than 16 colours; otherwise terminfo colors, which is capped at 16.
    pub fn colours(&self) -> u32 {
        if self.has("RGB") { 16777216 } else if self.has("256") { 256 }
        else if self.has("16") || self.has("t256") { 16 } else if self.has("mono") { 2 } else { 8 }
    }

    /// Whether 256-colour cells can be drawn: the 256 feature or terminfo colors#256
    /// (tty_check_fg/tty_check_bg).
    pub fn render_256(&self) -> bool {
        self.has("256") || self.has("t256")
    }

    pub fn list(&self) -> String {
        KNOWN.iter().filter(|f| self.0.contains(**f)).copied().collect::<Vec<_>>().join(",")
    }
}

/// tty_default_features: what tmux enables for a terminal it identifies, by its secondary or
/// extended device attributes (device_report) or, before any reply, by TERM_PROGRAM or TERM.
fn default_features(name: &str) -> &'static [&'static str] {
    const BASE: &str = "256,RGB,bpaste,clipboard,mouse,strikethrough,title";
    match name {
        "mintty" => &[BASE, "appesc,ccolour,cstyle,extkeys,margins,overline,usstyle"],
        "tmux" => &[BASE, "ccolour,cstyle,extkeys,focus,overline,usstyle,hyperlinks,progressbar"],
        "rxvt-unicode" => &["256,bpaste,ccolour,cstyle,mouse,title,ignorefkeys"],
        "iTerm2" | "iTerm.app" => &[BASE, "cstyle,extkeys,margins,usstyle,sync,osc7,hyperlinks,progressbar"],
        "foot" => &[BASE, "ccolour,cstyle,extkeys,usstyle,sync,osc7,hyperlinks"],
        "WezTerm" => &[BASE, "ccolour,cstyle,extkeys,focus,hyperlinks,margins,usstyle"],
        "ghostty" => &[BASE, "ccolour,cstyle,extkeys,focus,margins,overline,hyperlinks,osc7,sync,usstyle,progressbar"],
        "Rio" | "rio" => &[BASE, "ccolour,cstyle,focus,overline,hyperlinks,osc7,sync,usstyle,progressbar"],
        "XTerm" => &[BASE, "ccolour,cstyle,extkeys,focus"],
        _ => &[],
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
/// Without terminfo, the TERM name stands in for its colors capability: "t256" (colors#256,
/// which is not the 256 feature), "16" (16 or 88 colours) or "mono" (none, as dumb and vt100).
#[cfg(test)]
pub fn compute(term: &str, colorterm: &str, table: &[String], extra: &str) -> Features {
    compute_named(term, "", colorterm, table, extra)
}

pub fn compute_named(term: &str, program: &str, colorterm: &str, table: &[String], extra: &str) -> Features {
    // tmux order: -T, terminal-features, COLORTERM, then what the terminal is known to support;
    // a `feature@` removal keeps the later additions out.
    let mut f = Features::default();
    f.parse(extra, &[':', ',']);
    for entry in table {
        // the pattern is read like an override (`::` is a colon), the features split at `:`
        let pat = override_tokens(entry).swap_remove(0);
        let rest = entry.get(pat.len() + pat.matches(':').count() + 1..).unwrap_or("");
        if !pat.is_empty() && fnmatch(&pat, term) && !rest.is_empty() {
            f.parse(rest, &[':']);
        }
    }
    // tty_term_create: COLORTERM truecolor/24bit is RGB, any other value with 256 in it is 256
    if colorterm.eq_ignore_ascii_case("truecolor") || colorterm.eq_ignore_ascii_case("24bit") {
        f.add("RGB");
    } else if colorterm.contains("256") {
        f.add("256");
    }
    let vt = term.strip_prefix("vt").is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()));
    if term == "dumb" || (vt && !term.contains("color")) { f.set_colours("mono"); }
    if term.contains("16color") || term.contains("88color") || term == "rxvt-unicode" || term == "xterm" { f.set_colours("16"); }
    if term.is_empty() {
        // Windows console / Windows Terminal: a modern VT implementation
        f.add("RGB,mouse,bpaste,title,ccolour,cstyle,focus,clipboard,strikethrough,sync,usstyle,overline,hyperlinks");
    } else if term != "dumb" {
        if term != "linux" && !term.starts_with("vt1") {
            f.add("mouse,bpaste,strikethrough");
        }
        if term.contains("256col") || term.contains("direct") || term.starts_with("tmux") || term.starts_with("alacritty") || term.starts_with("xterm-kitty") || term.starts_with("foot") || term.starts_with("wezterm") || term.starts_with("xterm-ghostty") {
            f.set_colours("t256");
        }
        if term.contains("direct") {
            f.add("RGB");
        }
    }
    let lower = term.to_ascii_lowercase();
    let name = if lower.contains("wezterm") || program.eq_ignore_ascii_case("wezterm") { "WezTerm" } else if lower.contains("ghostty") { "ghostty" } else { program };
    for list in default_features(name) {
        f.add(list);
    }
    f
}

/// Nearest xterm-256 colour for an RGB value.
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    let cube = |v: u8| -> u8 { if v < 48 { 0 } else if v < 114 { 1 } else { (v - 35) / 40 } };
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
    (0, 0, 0), (128, 0, 0), (0, 128, 0), (128, 128, 0), (0, 0, 128), (128, 0, 128), (0, 128, 128), (192, 192, 192),
    (128, 128, 128), (255, 0, 0), (0, 255, 0), (255, 255, 0), (0, 0, 255), (255, 0, 255), (0, 255, 255), (255, 255, 255),
];

pub(super) fn idx_to_rgb(i: u8) -> (u8, u8, u8) {
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

/// colour_force_rgb with tmux's colour_256toRGB table; None for the default colour.
pub fn colour_force_rgb(c: vt100::Color) -> Option<(u8, u8, u8)> {
    match c {
        vt100::Color::Rgb(r, g, b) => Some((r, g, b)),
        vt100::Color::Idx(i) | vt100::Color::Palette(i) => Some(idx_to_rgb(i)),
        vt100::Color::Default => None,
    }
}

#[cfg(test)]
pub fn rgb_to_16(r: u8, g: u8, b: u8) -> u8 {
    (0..16u8).min_by_key(|i| dist((r, g, b), ANSI16[*i as usize])).unwrap()
}

/// tmux colour_256to16 table.
pub fn idx_to_16(i: u8) -> u8 {
    const TABLE: [u8; 256] = [
        0,  1,  2,  3,  4,  5,  6,  7,  8,  9, 10, 11, 12, 13, 14, 15,
        0,  4,  4,  4, 12, 12,  2,  6,  4,  4, 12, 12,  2,  2,  6,  4,
		12, 12,  2,  2,  2,  6, 12, 12, 10, 10, 10, 10, 14, 12, 10, 10,
		10, 10, 10, 14,  1,  5,  4,  4, 12, 12,  3,  8,  4,  4, 12, 12,
        2,  2,  6,  4, 12, 12,  2,  2,  2,  6, 12, 12, 10, 10, 10, 10,
		14, 12, 10, 10, 10, 10, 10, 14,  1,  1,  5,  4, 12, 12,  1,  1,
        5,  4, 12, 12,  3,  3,  8,  4, 12, 12,  2,  2,  2,  6, 12, 12,
		10, 10, 10, 10, 14, 12, 10, 10, 10, 10, 10, 14,  1,  1,  1,  5,
		12, 12,  1,  1,  1,  5, 12, 12,  1,  1,  1,  5, 12, 12,  3,  3,
        3,  7, 12, 12, 10, 10, 10, 10, 14, 12, 10, 10, 10, 10, 10, 14,
        9,  9,  9,  9, 13, 12,  9,  9,  9,  9, 13, 12,  9,  9,  9,  9,
		13, 12,  9,  9,  9,  9, 13, 12, 11, 11, 11, 11,  7, 12, 10, 10,
		10, 10, 10, 14,  9,  9,  9,  9,  9, 13,  9,  9,  9,  9,  9, 13,
        9,  9,  9,  9,  9, 13,  9,  9,  9,  9,  9, 13,  9,  9,  9,  9,
        9, 13, 11, 11, 11, 11, 11, 15,  0,  0,  0,  0,  0,  0,  8,  8,
        8,  8,  8,  8,  7,  7,  7,  7,  7,  7, 15, 15, 15, 15, 15, 15
    ];
    TABLE[i as usize]
}

/// Downgrades a colour to what the terminal can show.
pub fn fit_color(c: vt100::Color, f: &Features) -> vt100::Color {
    use vt100::Color::*;
    match c {
        Rgb(r, g, b) if !f.has("RGB") => {
            if f.render_256() {
                Idx(rgb_to_256(r, g, b))
            } else {
                Idx(idx_to_16(rgb_to_256(r, g, b)))
            }
        }
        Palette(i) if !f.render_256() => Idx(idx_to_16(i)),
        Idx(i) if i >= 16 && !f.render_256() => {
            Idx(idx_to_16(i))
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
        // terminfo colors#256 draws 256 colours but is not the 256 feature
        assert!(x.render_256() && !x.has("256") && x.has("clipboard") && x.has("mouse") && !x.has("RGB"));
        assert_eq!((x.colours(), x.list().contains("256")), (16, false));
        assert_eq!(compute("xterm-256color", "rxvt-256color", &table, "").colours(), 256);
        assert_eq!(compute("xterm", "TrueColor", &table, "").colours(), 16777216);
        assert_eq!(compute("screen-256color", "", &table, "256").colours(), 256);
        // terminfo colors: none (vt100) is 2, 8 stays 8, 16 and 88 are 16
        for (term, n) in [("vt100", 2), ("vt220", 2), ("dumb", 2), ("vte-256color", 16), ("linux", 8), ("screen", 8), ("xterm-16color", 16), ("rxvt-unicode", 16), ("xterm-88color", 16)] {
            assert_eq!(compute(term, "", &table, "").colours(), n, "{term}");
        }
        assert!(compute("xterm-256color", "truecolor", &table, "").has("RGB"));
        let l = compute("linux", "", &table, "");
        assert!(!l.has("256") && !l.has("mouse"));
        assert!(compute("linux", "", &table, "RGB,sync").has("sync"));
        let w = compute("", "", &table, "");
        assert!(w.has("RGB") && w.has("sync"));
        assert!(compute("screen", "", &table, "Tc").has("RGB"));
    }

    #[test]
    fn colors_override_sets_terminfo_colours_not_the_256_feature() {
        let mut f = compute("screen", "", &[], "");
        overrides("screen", &["screen:colors=256".into()], &mut f);
        assert!(f.render_256() && !f.has("256") && f.colours() == 16);
        overrides("screen", &["screen:colors=8".into()], &mut f);
        assert!(!f.render_256() && f.colours() == 8);
        overrides("screen", &["screen:colors@".into()], &mut f);
        assert_eq!(f.colours(), 2);
        let mut f = compute("xterm-256color", "", &[], "256");
        overrides("xterm-256color", &["xterm*:colors=8".into()], &mut f);
        assert!(f.has("256") && f.colours() == 256);
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
        let parts = override_tokens(entry);
        let mut parts = parts.iter().map(String::as_str);
        if !fnmatch(parts.next().unwrap_or(""), term) { continue; }
        for cap in parts.filter(|c| !c.is_empty()) {
            let (name, value) = cap.split_once('=').map(|(n, v)| (n, v.to_string()))
                .unwrap_or_else(|| (cap.trim_end_matches('@'), if cap.ends_with('@') { String::new() } else { "1".into() }));
            if name == "colors" {
                // terminfo colors, not the 256 feature: what tty_check_fg/bg and client_colours
                // read when neither 256 nor RGB is set
                for c in ["t256", "16", "mono"] { features.0.remove(c); }
                match value.parse::<u32>().unwrap_or(0) {
                    n if n >= 256 => features.set_colours("t256"),
                    n if n >= 16 => features.set_colours("16"),
                    n if n >= 8 => {}
                    _ => features.set_colours("mono"),
                }
            }
            let feature = match name {
                "Tc" | "RGB" => "RGB", "Ss" | "Se" => "cstyle", "Cs" | "Cr" => "ccolour",
                "Ms" => "clipboard", "Enbp" | "Dsbp" => "bpaste", "XT" => "title", "Dsfcs" | "Enfcs" => "focus",
                _ => "",
            };
            if !feature.is_empty() {
                if value.is_empty() { features.0.remove(feature); }
                else { features.add(feature); }
            }
            caps.insert(name.to_string(), decode_capability(&value));
        }
    }
    caps
}

/// tty_term_override_next: split at `:`, where `::` is a literal colon.
pub fn override_tokens(s: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != ':' { out.last_mut().unwrap().push(c); }
        else if chars.peek() == Some(&':') { chars.next(); out.last_mut().unwrap().push(':'); }
        else { out.push(String::new()); }
    }
    out
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
