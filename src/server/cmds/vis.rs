//! vis(3) encoding as tmux uses it (compat/vis.c and utf8_strvis): paste sanitising,
//! argument escaping and the octal escapes of capture-pane -C.

pub const OCTAL: u32 = 0x01;
pub const CSTYLE: u32 = 0x02;
pub const TAB: u32 = 0x08;
pub const NL: u32 = 0x10;
pub const SAFE: u32 = 0x20;
pub const NOSLASH: u32 = 0x40;
pub const DQ: u32 = 0x200;

fn visible(c: u8, flag: u32) -> bool {
    (c.is_ascii_graphic())
        || (c == b' ')
        || (flag & TAB == 0 && c == b'\t')
        || (flag & NL == 0 && c == b'\n')
        || (flag & SAFE != 0 && matches!(c, 0x08 | 0x07 | b'\r'))
}

/// Encodes one byte (vis()).
pub fn vis(out: &mut Vec<u8>, c: u8, flag: u32, next: u8) {
    if visible(c, flag) {
        if (c == b'"' && flag & DQ != 0) || (c == b'\\' && flag & NOSLASH == 0) {
            out.push(b'\\');
        }
        out.push(c);
        return;
    }
    if flag & CSTYLE != 0 {
        let e = match c {
            b'\n' => Some(b'n'),
            b'\r' => Some(b'r'),
            0x08 => Some(b'b'),
            0x07 => Some(b'a'),
            0x0b => Some(b'v'),
            b'\t' => Some(b't'),
            0x0c => Some(b'f'),
            b' ' => Some(b's'),
            _ => None,
        };
        if let Some(e) = e {
            out.extend([b'\\', e]);
            return;
        }
        if c == 0 {
            out.extend(b"\\0");
            if (b'0'..=b'7').contains(&next) {
                out.extend(b"00");
            }
            return;
        }
    }
    if c & 0x7f == b' ' || flag & OCTAL != 0 {
        out.extend([b'\\', b'0' + (c >> 6 & 7), b'0' + (c >> 3 & 7), b'0' + (c & 7)]);
        return;
    }
    if flag & NOSLASH == 0 {
        out.push(b'\\');
    }
    let mut c = c;
    if c & 0x80 != 0 {
        c &= 0x7f;
        out.push(b'M');
    }
    if c < 0x20 || c == 0x7f {
        out.push(b'^');
        out.push(if c == 0x7f { b'?' } else { c + b'@' });
    } else {
        out.extend([b'-', c]);
    }
}

/// utf8_strvis: complete UTF-8 characters pass through, every other byte goes through vis().
pub fn strvis(src: &[u8], flag: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        let n = utf8_len(&src[i..]);
        if n > 1 {
            out.extend_from_slice(&src[i..i + n]);
            i += n;
            continue;
        }
        let c = src[i];
        if flag & DQ != 0 && c == b'$' && i + 1 < src.len() {
            if src[i + 1].is_ascii_alphabetic() || src[i + 1] == b'_' || src[i + 1] == b'{' {
                out.push(b'\\');
            }
            out.push(b'$');
        } else {
            vis(&mut out, c, flag, src.get(i + 1).copied().unwrap_or(0));
        }
        i += 1;
    }
    out
}

/// Length of the valid multi-byte UTF-8 character at the start of `b`, else 0/1.
fn utf8_len(b: &[u8]) -> usize {
    let n = match b.first() {
        Some(0xc2..=0xdf) => 2,
        Some(0xe0..=0xef) => 3,
        Some(0xf0..=0xf4) => 4,
        _ => return 1,
    };
    if b.len() >= n && std::str::from_utf8(&b[..n]).is_ok() { n } else { 1 }
}

/// tmux's args_escape: how a single argument (a key name in list-keys) is quoted.
pub fn args_escape(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    let quotes = if s.contains([' ', '#', '\'', ';', '$', '{', '}', '%']) {
        Some('"')
    } else if s.contains([' ', '"']) {
        Some('\'')
    } else {
        None
    };
    let mut chars = s.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c != ' ' && (quotes.is_some() || c == '~') {
            return format!("\\{c}");
        }
    }
    let flags = OCTAL | CSTYLE | TAB | NL | if quotes == Some('"') { DQ } else { 0 };
    let escaped = String::from_utf8_lossy(&strvis(s.as_bytes(), flags)).into_owned();
    let tilde = if escaped.starts_with('~') { "\\" } else { "" };
    match quotes {
        Some('\'') => format!("'{escaped}'"),
        Some(_) => format!("\"{tilde}{escaped}\""),
        None => format!("{tilde}{escaped}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_tmux() {
        // paste-buffer: VIS_SAFE|VIS_NOSLASH
        let paste = |s: &[u8]| String::from_utf8(strvis(s, SAFE | NOSLASH)).unwrap();
        assert_eq!(paste(b"a\x1b[201~b\tc\rd\\"), "a^[[201~b\tc\rd\\");
        assert_eq!(paste("한글\x7f\x00".as_bytes()), "한글^?^@");
        assert_eq!(paste(b"\xc3x\xa0"), "M-Cx\\240");
        // capture-pane -C style octal
        assert_eq!(String::from_utf8(strvis(b"\x01a", OCTAL)).unwrap(), "\\001a");
        for (k, q) in [(";", "\\;"), ("\"", "\\\""), ("#", "\\#"), ("~", "\\~"), ("a", "a"), ("C-\\", "C-\\\\"), ("a b", "\"a b\""), ("M-$", "\"M-$\""), ("'", "\\'")] {
            assert_eq!(args_escape(k), q, "{k}");
        }
    }
}
