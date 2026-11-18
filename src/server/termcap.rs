//! Parameter expansion for the terminfo strings used by the VT backend.
use std::collections::BTreeMap;

pub fn expand(cap: &str, args: &[&str]) -> Option<String> {
    let mut params: Vec<String> = (0..9).map(|i| args.get(i).copied().unwrap_or("0").to_string()).collect();
    let mut stack: Vec<String> = Vec::new();
    let mut vars = BTreeMap::new();
    let mut branches = Vec::new();
    let mut active = true;
    let mut out = String::new();
    let mut chars = cap.chars();
    let number = |s: String| s.parse::<i64>().unwrap_or(0);
    while let Some(ch) = chars.next() {
        if ch != '%' { if active { out.push(ch); } continue; }
        let op = chars.next()?;
        match op {
            // (enclosing branch active, a branch of this %? already taken)
            '?' => branches.push((active, false)),
            // A condition after a taken branch was never evaluated, so there is nothing to pop.
            't' => {
                let b = branches.last_mut()?;
                if b.0 && !b.1 { b.1 = number(stack.pop()?) != 0; active = b.1; } else { active = false; }
            }
            'e' => { let b = branches.last()?; active = b.0 && !b.1; }
            ';' => { active = branches.pop()?.0; }
            'p' => { let n = chars.next()?.to_digit(10)? as usize; if active { stack.push(params.get(n.checked_sub(1)?)?.clone()); } }
            'P' | 'g' => {
                let name = chars.next()?;
                if active { if op == 'P' { vars.insert(name, stack.pop()?); } else { stack.push(vars.get(&name).cloned().unwrap_or_else(|| "0".into())); } }
            }
            '{' => {
                let mut value = String::new();
                loop { let c = chars.next()?; if c == '}' { break; } value.push(c); }
                if active { stack.push(value.parse::<i64>().ok()?.to_string()); }
            }
            '\'' => {
                let c = chars.next()?;
                if chars.next()? != '\'' { return None; }
                if active { stack.push((c as u32).to_string()); }
            }
            'i' if active => { for p in params.iter_mut().take(2) { *p = number(p.clone()).saturating_add(1).to_string(); } }
            'r' if active => params.swap(0, 1),
            'l' if active => { let n = stack.pop()?.len(); stack.push(n.to_string()); }
            '+' | '-' | '*' | '/' | 'm' | '&' | '|' | '^' | '=' | '>' | '<' | 'A' | 'O' if active => {
                let b = number(stack.pop()?); let a = number(stack.pop()?);
                let n = match op {
                    '+' => a.saturating_add(b), '-' => a.saturating_sub(b), '*' => a.saturating_mul(b),
                    '/' => a.checked_div(b)?, 'm' => a.checked_rem(b)?, '&' => a & b, '|' => a | b, '^' => a ^ b,
                    '=' => (a == b) as i64, '>' => (a > b) as i64, '<' => (a < b) as i64,
                    'A' => (a != 0 && b != 0) as i64, 'O' => (a != 0 || b != 0) as i64, _ => unreachable!(),
                }; stack.push(n.to_string());
            }
            '!' | '~' if active => { let n = number(stack.pop()?); stack.push((if op == '!' { (n == 0) as i64 } else { !n }).to_string()); }
            '%' if active => out.push('%'),
            'd' | 'o' | 'x' | 'X' | 's' | 'c' | ':' | '0'..='9' | '.' => {
                let mut spec = String::new(); let mut conversion = op;
                if !matches!(op, 'd' | 'o' | 'x' | 'X' | 's' | 'c') {
                    if op != ':' { spec.push(op); }
                    loop { let c = chars.next()?; if matches!(c, 'd' | 'o' | 'x' | 'X' | 's' | 'c') { conversion = c; break; } spec.push(c); }
                }
                if !active { continue; }
                let value = stack.pop().or_else(|| params.first().cloned())?;
                let n = number(value.clone());
                let value = match conversion {
                    's' => value, 'c' => char::from_u32(n as u32)?.to_string(), 'o' => format!("{n:o}"),
                    'x' => format!("{n:x}"), 'X' => format!("{n:X}"), _ => n.to_string(),
                };
                let width = spec.trim_start_matches(['-', '+', '#', ' ']).split('.').next().unwrap_or("").parse::<usize>().unwrap_or(0).min(4096);
                let pad = width.saturating_sub(value.len());
                if !spec.starts_with('-') { out.extend(std::iter::repeat_n(if spec.starts_with('0') { '0' } else { ' ' }, pad)); }
                out.push_str(&value);
                if spec.starts_with('-') { out.extend(std::iter::repeat_n(' ', pad)); }
            }
            _ => {}
        }
        if stack.len() > 128 || branches.len() > 64 || out.len() > 1 << 20 { return None; }
    }
    if branches.is_empty() { Some(out) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expands_cursor_and_clipboard_capabilities() {
        assert_eq!(expand("\x1b[%p1%{1}%+%d q", &["4"]).unwrap(), "\x1b[5 q");
        assert_eq!(expand("\x1b]52;%p1%s;%p2%s\x07", &["c", "YWJj"]).unwrap(), "\x1b]52;c;YWJj\x07");
        assert_eq!(expand("%i%p1%02d;%p2%d", &["1", "2"]).unwrap(), "02;3");
    }
    #[test]
    fn conditionals_and_variables() {
        let cap = "%?%p1%{2}%<%tlow%e%?%p1%{4}%<%tmid%ehigh%;%;";
        assert_eq!(expand(cap, &["1"]).unwrap(), "low");
        assert_eq!(expand(cap, &["3"]).unwrap(), "mid");
        assert_eq!(expand(cap, &["5"]).unwrap(), "high");
        assert_eq!(expand("%p1%Pa%ga%{2}%*%d", &["3"]).unwrap(), "6");
        assert!(expand("%p1%{0}%/%d", &["1"]).is_none());
    }
    #[test]
    fn flat_else_if_chains() {
        let cap = "%?%p1%{2}%<%tlow%e%p1%{4}%<%tmid%ehigh%;";
        assert_eq!(expand(cap, &["1"]).unwrap(), "low");
        assert_eq!(expand(cap, &["3"]).unwrap(), "mid");
        assert_eq!(expand(cap, &["5"]).unwrap(), "high");
        let setaf = "\x1b[%?%p1%{8}%<%t3%p1%d%e%p1%{16}%<%t9%p1%{8}%-%d%e38;5;%p1%d%;m";
        assert_eq!(expand(setaf, &["1"]).unwrap(), "\x1b[31m");
        assert_eq!(expand(setaf, &["9"]).unwrap(), "\x1b[91m");
        assert_eq!(expand(setaf, &["100"]).unwrap(), "\x1b[38;5;100m");
        assert_eq!(expand("%?%p1%{1}%=%tone%e%p1%{2}%=%ttwo%eother%;", &["1"]).unwrap(), "one");
        // a nested %? inside a skipped branch
        assert_eq!(expand("%?%p1%t%?%p2%tA%eB%;%eC%;", &["0", "1"]).unwrap(), "C");
        assert_eq!(expand("%?%p1%t%?%p2%tA%eB%;%eC%;", &["1", "0"]).unwrap(), "B");
    }
}
