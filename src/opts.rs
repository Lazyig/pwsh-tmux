//! Minimal getopt for tmux-style command arguments.

use std::collections::HashMap;

pub struct Opts {
    pub flags: HashMap<char, String>,
    pub rest: Vec<String>,
    /// Which of `rest` were { } blocks (ARGS_COMMANDS values), set by the server's
    /// dispatcher; empty when none were.
    pub rest_blocks: Vec<bool>,
}

impl Opts {
    /// Whether argument `i` of `rest` was a { } block.
    pub fn is_block(&self, i: usize) -> bool {
        self.rest_blocks.get(i).copied().unwrap_or(false)
    }
}

impl Opts {
    pub fn has(&self, c: char) -> bool {
        self.flags.contains_key(&c)
    }
    /// How many times a flag without a value was given.
    pub fn count(&self, c: char) -> usize {
        self.flags.get(&c).map_or(0, |v| v.len() + 1)
    }
    pub fn get(&self, c: char) -> Option<&str> {
        self.flags.get(&c).map(String::as_str)
    }
}

/// `bools` are flags without a value, `vals` take one (`-t x` or `-tx`).
/// Parsing stops at the first non-flag argument or `--`.
pub fn getopt(args: &[String], bools: &str, vals: &str) -> Result<Opts, String> {
    getopt_optional(args, bools, vals, "")
}

/// [`getopt`] where the flags in `optional` (also listed in `bools`) take an optional value
/// (tmux `D::`): attached (`-D5`), or the next argument unless it looks like a flag.
/// Without one their value is empty.
/// The flag tmux names in "invalid flag -%c": the first byte of the character (given
/// back to the client as that byte).
pub fn flag_byte(c: char) -> char {
    match c.encode_utf8(&mut [0; 4]).as_bytes() {
        [b] => *b as char,
        [b, ..] => char::from_u32(0x10FF00 + *b as u32).unwrap_or(c),
        [] => c,
    }
}

/// The error of [`getopt_optional`] for -?: the caller prints the command's usage.
pub const USAGE_ERROR: &str = "\0usage";

pub fn getopt_optional(args: &[String], bools: &str, vals: &str, optional: &str) -> Result<Opts, String> {
    let mut flags = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            i += 1;
            break;
        }
        if !a.starts_with('-') || a.len() < 2 {
            break;
        }
        let chars: Vec<char> = a[1..].chars().collect();
        let mut j = 0;
        while j < chars.len() {
            let c = chars[j];
            // args_parse_flags: -? asks for the usage, other flags are letters or digits
            if c == '?' {
                return Err(USAGE_ERROR.into());
            }
            if !c.is_ascii_alphanumeric() {
                return Err(format!("invalid flag -{}", flag_byte(c)));
            }
            if optional.contains(c) {
                let inline: String = chars[j + 1..].iter().collect();
                let v = if !inline.is_empty() {
                    inline
                } else {
                    match args.get(i + 1) {
                        Some(n) if !(n.starts_with('-') && n[1..].starts_with(|c: char| c == '-' || c.is_ascii_alphabetic())) => {
                            i += 1;
                            n.clone()
                        }
                        _ => String::new(),
                    }
                };
                flags.insert(c, v);
                break;
            } else if bools.contains(c) {
                // a repeated flag (-NN) is counted by its value's length
                flags.entry(c).and_modify(|v: &mut String| v.push(c)).or_insert_with(String::new);
                j += 1;
            } else if vals.contains(c) {
                let inline: String = chars[j + 1..].iter().collect();
                let v = if inline.is_empty() {
                    i += 1;
                    args.get(i).cloned().ok_or_else(|| format!("-{c} expects an argument"))?
                } else {
                    inline
                };
                flags.insert(c, v);
                break;
            } else {
                return Err(format!("unknown flag -{c}"));
            }
        }
        i += 1;
    }
    Ok(Opts { flags, rest: args[i.min(args.len())..].to_vec(), rest_blocks: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses() {
        let o = getopt(&s(&["-dh", "-tfoo:1", "-c", "/x", "vim", "-h"]), "dh", "tc").unwrap();
        assert!(o.has('d') && o.has('h'));
        assert_eq!(o.get('t'), Some("foo:1"));
        assert_eq!(o.get('c'), Some("/x"));
        assert_eq!(o.rest, s(&["vim", "-h"]));
        assert!(getopt(&s(&["-q"]), "d", "t").is_err());
        assert!(getopt(&s(&["-t"]), "", "t").is_err());
        assert_eq!(getopt(&s(&["--", "-d"]), "d", "").unwrap().rest, s(&["-d"]));
        // optional values: attached, separate unless a flag, or none
        let o = getopt_optional(&s(&["-D-5", "-L", "3", "-U", "-t", "x", "-R"]), "DLRU", "t", "DLRU").unwrap();
        assert_eq!((o.get('D'), o.get('L'), o.get('U'), o.get('R'), o.get('t')), (Some("-5"), Some("3"), Some(""), Some(""), Some("x")));
    }
}
