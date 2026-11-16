//! Fuzzy matching in the style of fzf, after tmux 3.8 fuzzy.c (used by switch-mode).
//!
//! The pattern is split into groups by `|` and each group into terms on spaces. A text matches
//! if any group matches; within a group every positive term must match and every inverse term
//! must not. Plain terms are fuzzy subsequences, `'` makes a term an exact substring, `^`
//! anchors it at the start and `$` at the end, and `!` inverts it (inverse terms are exact).
//! Matching is smart-case. The text may hold `#[...]` styles: they are invisible to matching,
//! but `align=` moves text exactly as it is drawn.

use unicode_width::UnicodeWidthChar;

const BONUS_EXACT: i64 = 1000;
const BONUS_PREFIX: i64 = 200;
const BONUS_SUFFIX: i64 = 100;
const BONUS_START: i64 = 12;
const BONUS_BOUNDARY: i64 = 8;
const BONUS_CONSECUTIVE: i64 = 6;
const PENALTY_LEADING: i64 = 1;
const PENALTY_LEADING_MAX: i64 = 10;
const PENALTY_GAP: i64 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Align {
    Left,
    Centre,
    Right,
    Absolute,
}

struct Char {
    align: Align,
    c: char,
    width: usize,
    offset: usize,
}

fn is_boundary(c: char) -> bool {
    " -_/.:".contains(c)
}

fn equal(a: char, b: char, fold: bool) -> bool {
    if fold && a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(&b);
    }
    a == b
}

fn align_of(style: &str, current: Align) -> Align {
    let mut a = current;
    for w in style.split([',', ' ']).filter(|w| !w.is_empty()) {
        match w.to_ascii_lowercase().as_str() {
            "align=left" => a = Align::Left,
            "align=centre" | "align=center" => a = Align::Centre,
            "align=right" => a = Align::Right,
            "align=absolute-centre" | "align=absolute-center" => a = Align::Absolute,
            _ => {}
        }
    }
    a
}

/// The visible characters of the text and the width of each alignment (fuzzy_scan).
fn scan(text: &str) -> (Vec<Char>, [usize; 4]) {
    let mut out = Vec::new();
    let mut widths = [0usize; 4];
    let mut current = Align::Left;
    let add = |out: &mut Vec<Char>, widths: &mut [usize; 4], a: Align, c: char| {
        let width = c.width().unwrap_or(0);
        out.push(Char { align: a, c, width, offset: widths[a as usize] });
        widths[a as usize] += width;
    };
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'#' {
            let n = b[i..].iter().take_while(|c| **c == b'#').count();
            if b.get(i + n) != Some(&b'[') {
                for _ in 0..n.div_ceil(2) {
                    add(&mut out, &mut widths, current, '#');
                }
                i += n;
                continue;
            }
            for _ in 0..n / 2 {
                add(&mut out, &mut widths, current, '#');
            }
            if n % 2 == 0 {
                add(&mut out, &mut widths, current, '[');
                i += n + 1;
                continue;
            }
            let Some(end) = text[i + n + 1..].find(']') else { break };
            current = align_of(&text[i + n + 1..i + n + 1 + end], current);
            i += n + 1 + end + 1;
            continue;
        }
        let c = text[i..].chars().next().unwrap();
        i += c.len_utf8();
        if c.is_control() {
            continue;
        }
        add(&mut out, &mut widths, current, c);
    }
    (out, widths)
}

struct Term<'a> {
    inverse: bool,
    exact: bool,
    prefix: bool,
    suffix: bool,
    text: &'a str,
}

fn parse_term(mut t: &str) -> Option<Term<'_>> {
    let mut term = Term { inverse: false, exact: false, prefix: false, suffix: false, text: "" };
    if let Some(r) = t.strip_prefix('!') {
        term.inverse = true;
        t = r;
    }
    if t.is_empty() {
        return None;
    }
    if let Some(r) = t.strip_prefix('\'') {
        term.exact = true;
        t = r;
    } else if let Some(r) = t.strip_prefix('^') {
        term.exact = true;
        term.prefix = true;
        t = r;
    }
    if t.is_empty() {
        return None;
    }
    if let Some(r) = t.strip_suffix('$') {
        term.exact = true;
        term.suffix = true;
        t = r;
    }
    if t.is_empty() {
        return None;
    }
    if term.inverse {
        term.exact = true;
    }
    term.text = t;
    Some(term)
}

fn score_positions(pos: &[usize], cs: &[Char]) -> i64 {
    let Some(&first) = pos.first() else { return 0 };
    let mut score = 0;
    if first == 0 {
        score += BONUS_START;
    } else {
        if is_boundary(cs[first - 1].c) {
            score += BONUS_BOUNDARY;
        }
        score -= (first as i64).min(PENALTY_LEADING_MAX) * PENALTY_LEADING;
    }
    for i in 1..pos.len() {
        if pos[i] == pos[i - 1] + 1 {
            score += BONUS_CONSECUTIVE;
        } else if is_boundary(cs[pos[i] - 1].c) {
            score += BONUS_BOUNDARY;
        }
    }
    let span = pos[pos.len() - 1] - first + 1;
    score - (span - pos.len()) as i64 * PENALTY_GAP
}

fn match_fuzzy(tok: &[char], cs: &[Char], fold: bool, matched: Option<&mut [bool]>) -> Option<i64> {
    if tok.is_empty() || cs.is_empty() {
        return None;
    }
    let mut pos = vec![0usize; tok.len()];
    let mut ci = 0;
    for (pi, t) in tok.iter().enumerate() {
        while ci < cs.len() && !equal(*t, cs[ci].c, fold) {
            ci += 1;
        }
        if ci == cs.len() {
            return None;
        }
        pos[pi] = ci;
        ci += 1;
    }
    // compact it backwards to prefer a shorter span
    let mut ci = pos[tok.len() - 1];
    for pi in (1..=tok.len()).rev() {
        loop {
            if equal(tok[pi - 1], cs[ci].c, fold) {
                pos[pi - 1] = ci;
                break;
            }
            if ci == 0 {
                return None;
            }
            ci -= 1;
        }
        if pi != 1 {
            if ci == 0 {
                return None;
            }
            ci -= 1;
        }
    }
    if let Some(m) = matched {
        for p in &pos {
            m[*p] = true;
        }
    }
    Some(score_positions(&pos, cs))
}

fn score_exact(start: usize, len: usize, cs: &[Char], prefix: bool, suffix: bool) -> i64 {
    let mut score = BONUS_EXACT + len as i64 * BONUS_CONSECUTIVE;
    if prefix {
        score += BONUS_PREFIX;
    }
    if suffix {
        score += BONUS_SUFFIX;
    }
    if start == 0 {
        score += BONUS_START;
    } else if is_boundary(cs[start - 1].c) {
        score += BONUS_BOUNDARY;
    }
    score -= (start as i64).min(PENALTY_LEADING_MAX) * PENALTY_LEADING;
    if !prefix && !suffix {
        score -= (cs.len() - (start + len)) as i64;
    }
    score
}

fn match_exact(tok: &[char], cs: &[Char], fold: bool, prefix: bool, suffix: bool, matched: Option<&mut [bool]>) -> Option<i64> {
    let n = cs.len();
    if tok.is_empty() || tok.len() > n {
        return None;
    }
    let range = match (prefix, suffix) {
        (true, true) if tok.len() != n => return None,
        (true, _) => 0..1,
        (false, true) => n - tok.len()..n - tok.len() + 1,
        (false, false) => 0..n - tok.len() + 1,
    };
    let mut best: Option<(usize, i64)> = None;
    for i in range {
        if !tok.iter().enumerate().all(|(j, t)| equal(*t, cs[i + j].c, fold)) {
            continue;
        }
        let v = score_exact(i, tok.len(), cs, prefix, suffix);
        if best.is_none_or(|(_, b)| v > b) {
            best = Some((i, v));
        }
    }
    let (start, score) = best?;
    if let Some(m) = matched {
        for k in 0..tok.len() {
            m[start + k] = true;
        }
    }
    Some(score)
}

fn match_group(group: &str, cs: &[Char], fold: bool, matched: &mut [bool]) -> Option<i64> {
    let mut score = 0;
    let mut any = false;
    for t in group.split(' ').filter(|t| !t.is_empty()) {
        let term = parse_term(t)?;
        any = true;
        let tok: Vec<char> = term.text.chars().collect();
        if tok.len() > cs.len() {
            if term.inverse {
                continue;
            }
            return None;
        }
        let m = if term.inverse { None } else { Some(&mut *matched) };
        let r = if term.exact { match_exact(&tok, cs, fold, term.prefix, term.suffix, m) } else { match_fuzzy(&tok, cs, fold, m) };
        match (term.inverse, r) {
            (true, Some(_)) => return None,
            (true, None) => {}
            (false, None) => return None,
            (false, Some(v)) => score += v,
        }
    }
    any.then_some(score)
}

/// Match `pattern` against `text` drawn into `width` columns: the columns of the matched
/// characters and the score (higher is better), or None when it does not match.
pub fn fuzzy_match(pattern: &str, text: &str, width: usize) -> Option<(Vec<bool>, u32)> {
    if width == 0 {
        return None;
    }
    if pattern.chars().all(|c| c == ' ' || c == '|') {
        return Some((vec![false; width], 0));
    }
    let fold = !pattern.chars().any(|c| c.is_ascii_uppercase());
    let (cs, widths) = scan(text);
    let mut best: Option<(i64, Vec<bool>)> = None;
    for group in pattern.split('|') {
        if group.trim_matches(' ').is_empty() {
            continue;
        }
        let mut matched = vec![false; cs.len()];
        if let Some(score) = match_group(group, &cs, fold, &mut matched) {
            if best.as_ref().is_none_or(|(b, _)| score > *b) {
                best = Some((score, matched));
            }
        }
    }
    let (score, matched) = best?;

    // trimmed widths and start columns of each alignment, as the text is laid out
    let (mut wl, mut wc, mut wr) = (widths[0], widths[1], widths[2]);
    let wa = widths[3].min(width);
    while wl + wc + wr > width {
        if wc > 0 {
            wc -= 1;
        } else if wr > 0 {
            wr -= 1;
        } else {
            wl -= 1;
        }
    }
    let start = [0, wl + (width - wr - wl) / 2 - wc / 2, width - wr, (width - wa) / 2];
    let src = [0, widths[1] / 2 - wc / 2, widths[2] - wr, 0];
    let vis = [wl, wc, wr, wa];
    let mut mask = vec![false; width];
    for (i, c) in cs.iter().enumerate() {
        let a = c.align as usize;
        if !matched[i] || c.offset < src[a] || c.offset >= src[a] + vis[a] {
            continue;
        }
        let column = start[a] + (c.offset - src[a]);
        for j in 0..c.width {
            if column + j < width {
                mask[column + j] = true;
            }
        }
    }
    Some((mask, score.max(0) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(m: &[bool]) -> Vec<usize> {
        m.iter().enumerate().filter(|(_, b)| **b).map(|(i, _)| i).collect()
    }

    #[test]
    fn subsequence_and_styles() {
        let (m, _) = fuzzy_match("ed", "editor #[dim]alpha:1#[default]", 30).unwrap();
        assert_eq!(cols(&m), [0, 1]);
        assert!(fuzzy_match("ed", "sh #[dim]alpha:0*#[default] sleep", 30).is_none());
        // styles take no columns
        let (m, _) = fuzzy_match("al", "x #[dim]alpha", 30).unwrap();
        assert_eq!(cols(&m), [2, 3]);
        // the backwards pass prefers the shortest span
        let (m, _) = fuzzy_match("ab", "a-xab", 10).unwrap();
        assert_eq!(cols(&m), [3, 4]);
    }

    #[test]
    fn smart_case_and_terms() {
        assert!(fuzzy_match("ED", "editor", 10).is_none());
        assert!(fuzzy_match("ed", "EDITOR", 10).is_some());
        assert!(fuzzy_match("^ed", "editor", 10).is_some());
        assert!(fuzzy_match("^di", "editor", 10).is_none());
        assert!(fuzzy_match("or$", "editor", 10).is_some());
        assert!(fuzzy_match("!vim", "editor", 10).is_some());
        assert!(fuzzy_match("!edit", "editor", 10).is_none());
        assert!(fuzzy_match("xyz | edi", "editor", 10).is_some());
        assert!(fuzzy_match("ed tor", "editor", 10).is_some());
        assert!(fuzzy_match("ed xyz", "editor", 10).is_none());
        // an empty pattern matches with nothing highlighted
        assert_eq!(fuzzy_match(" ", "editor", 4), Some((vec![false; 4], 0)));
    }

    #[test]
    fn scores_rank_better_matches_first() {
        let start = fuzzy_match("ed", "editor", 20).unwrap().1;
        let middle = fuzzy_match("ed", "xxxxe-d", 20).unwrap().1;
        assert!(start > middle);
        let exact = fuzzy_match("'dit", "editor", 20).unwrap().1;
        assert!(exact > start);
    }

    #[test]
    fn right_aligned_columns() {
        let (m, _) = fuzzy_match("ab", "#[align=right]ab", 10).unwrap();
        assert_eq!(cols(&m), [8, 9]);
    }
}
