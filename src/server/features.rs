//! Terminal features of a client's outer terminal (tmux `terminal-features` / `-T`):
//! what the renderer may send it.

use std::collections::BTreeSet;

#[allow(dead_code)]
pub const KNOWN: &[&str] = &[
    "256", "RGB", "bpaste", "ccolour", "clipboard", "cstyle", "extkeys", "focus", "hyperlinks", "ignorefkeys", "margins", "mouse", "osc7", "overline",
    "rectfill", "strikethrough", "sync", "title", "usstyle",
];

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Features(pub BTreeSet<String>);

impl Features {
    pub fn has(&self, f: &str) -> bool {
        self.0.contains(f)
    }

    fn add(&mut self, list: &str) {
        for f in list.split([',', ':']).map(str::trim).filter(|f| !f.is_empty()) {
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

    pub fn colours(&self) -> u32 {
        if self.has("RGB") { 16777216 } else if self.has("256") { 256 }
        else if self.has("16") { 16 } else if self.has("mono") { 2 } else { 8 }
    }

    pub fn list(&self) -> String {
        self.0.iter().filter(|f| !matches!(f.as_str(), "16" | "mono")).cloned().collect::<Vec<_>>().join(",")
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
pub fn compute(term: &str, colorterm: &str, table: &[String], extra: &str) -> Features {
    let mut f = Features::default();
    if term == "dumb" { f.add("mono"); }
    if term.contains("16color") { f.add("16"); }
    if term.is_empty() {
        // Windows console / Windows Terminal: a modern VT implementation
        f.add("RGB,mouse,bpaste,title,ccolour,cstyle,focus,clipboard,strikethrough,sync,usstyle,overline,hyperlinks");
    } else if term != "dumb" {
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
            if f.has("256") {
                Idx(rgb_to_256(r, g, b))
            } else {
                Idx(idx_to_16(rgb_to_256(r, g, b)))
            }
        }
        Idx(i) if i >= 16 && !f.has("256") => {
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
