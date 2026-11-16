//! Minimal getopt for tmux-style command arguments.

use std::collections::HashMap;

pub struct Opts {
    pub flags: HashMap<char, String>,
    pub rest: Vec<String>,
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
            if bools.contains(c) {
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
    Ok(Opts { flags, rest: args[i.min(args.len())..].to_vec() })
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
    }
}
