//! The JSON subset tmux parses for display-message -j (json.c): the top level is an object,
//! arrays hold only objects, numbers are 64-bit integers, there is no null, strings are not
//! empty and escapes are checked but kept as written. Output has the object keys sorted.

use std::collections::BTreeMap;

const ERROR_CTX_LEN: usize = 8;
const PARSE_DEPTH_MAX: usize = 200;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tok {
    OpenObject,
    CloseObject,
    OpenArray,
    CloseArray,
    Comma,
    Colon,
    Quote,
    Value,
    Eof,
}

pub enum Node {
    Str(String),
    Num(i64),
    Bool(bool),
    Object(BTreeMap<String, Node>),
    Array(Vec<Node>),
}

fn error(reason: &str, input: &str, at: usize) -> String {
    let loc = input.get(at..).unwrap_or("");
    if loc.is_empty() {
        return reason.to_string();
    }
    let ctx: String = loc.bytes().take(ERROR_CTX_LEN).map(|b| b as char).collect();
    let ellipsis = if loc.len() > ERROR_CTX_LEN { "..." } else { "" };
    format!("{reason}: {ctx}{ellipsis}")
}

fn tokenize(input: &str) -> Result<Vec<(Tok, usize, usize)>, String> {
    let b = input.as_bytes();
    let mut toks: Vec<(Tok, usize, usize)> = Vec::new();
    let (mut i, mut in_string) = (0, false);
    let mut last = 0;
    while i < b.len() {
        last = i;
        let ty = if in_string && b[i] != b'"' {
            Tok::Value
        } else {
            match b[i] {
                b' ' | b'\t' | b'\n' | b'\r' => {
                    i += 1;
                    continue;
                }
                b'{' => Tok::OpenObject,
                b'}' => Tok::CloseObject,
                b'[' => Tok::OpenArray,
                b']' => Tok::CloseArray,
                b'"' => Tok::Quote,
                b':' => Tok::Colon,
                b',' => Tok::Comma,
                _ => Tok::Value,
            }
        };
        let mut len = 1;
        if ty == Tok::Value {
            len = value_len(toks.last().map(|t| t.0), &b[i..]).ok_or_else(|| error("tokenization error", input, i))?;
        }
        toks.push((ty, i, len));
        if ty == Tok::Quote {
            in_string = !in_string;
        }
        i += len;
    }
    toks.push((Tok::Eof, last, 0));
    Ok(toks)
}

/// Strings run to the closing quote; numbers and booleans to `,`, `]`, `}` or space.
fn value_len(prev: Option<Tok>, s: &[u8]) -> Option<usize> {
    let mut n = 0;
    match prev? {
        Tok::Quote => {
            while *s.get(n)? != b'"' {
                if s[n] < 0x20 {
                    return None;
                }
                if s[n] != b'\\' {
                    n += 1;
                    continue;
                }
                n += 1;
                match *s.get(n)? {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => n += 1,
                    b'u' => {
                        if !(1..=4).all(|k| s.get(n + k).is_some_and(|c| c.is_ascii_hexdigit())) {
                            return None;
                        }
                        n += 5;
                    }
                    _ => return None,
                }
            }
        }
        Tok::Colon => loop {
            s.get(n)?;
            n += 1;
            match s.get(n) {
                Some(b']' | b'}' | b',') => break,
                Some(c) if c.is_ascii_whitespace() => break,
                _ => {}
            }
        },
        _ => return None,
    }
    Some(n)
}

struct Parser<'a> {
    input: &'a str,
    toks: Vec<(Tok, usize, usize)>,
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn ty(&self) -> Tok {
        self.toks[self.pos].0
    }
    fn err(&self, reason: &str) -> String {
        error(reason, self.input, self.toks[self.pos].1)
    }
    fn text(&self) -> &str {
        let (_, o, l) = self.toks[self.pos];
        &self.input[o..o + l]
    }

    /// A quoted string (key or value): quote, value, quote.
    fn quoted(&mut self, what: &str) -> Result<String, String> {
        let start = self.toks[self.pos].1;
        let fail = || error(what, self.input, start);
        if self.ty() != Tok::Quote {
            return Err(fail());
        }
        self.pos += 1;
        if self.ty() != Tok::Value {
            return Err(fail());
        }
        let s = self.text().to_string();
        self.pos += 1;
        if self.ty() != Tok::Quote {
            return Err(fail());
        }
        self.pos += 1;
        Ok(s)
    }

    fn object(&mut self) -> Result<Node, String> {
        self.depth += 1;
        if self.depth > PARSE_DEPTH_MAX {
            return Err(self.err("parse depth exceeded"));
        }
        self.pos += 1;
        let mut fields = BTreeMap::new();
        while self.ty() != Tok::CloseObject {
            let key = self.quoted("invalid key")?;
            if fields.contains_key(&key) {
                return Err(self.err("duplicate key"));
            }
            if self.ty() != Tok::Colon {
                return Err(self.err("missing colon"));
            }
            self.pos += 1;
            let value = match self.ty() {
                Tok::Quote => Node::Str(self.quoted("invalid string")?),
                Tok::Value => {
                    let v = self.text();
                    let b = v.as_bytes();
                    if b[0].is_ascii_digit() || (b[0] == b'-' && b.get(1).is_some_and(|c| c.is_ascii_digit())) {
                        if (b[0] == b'0' && b.len() != 1) || (b[0] == b'-' && b[1] == b'0' && b.len() != 2) {
                            return Err(self.err("invalid number"));
                        }
                        let n = v.parse::<i64>().map_err(|_| self.err("invalid number"))?;
                        self.pos += 1;
                        Node::Num(n)
                    } else {
                        let b = match v {
                            "true" => true,
                            "false" => false,
                            _ => return Err(self.err("invalid boolean")),
                        };
                        self.pos += 1;
                        Node::Bool(b)
                    }
                }
                Tok::OpenObject => self.object()?,
                Tok::OpenArray => self.array()?,
                _ => return Err(self.err("unexpected value when parsing object")),
            };
            fields.insert(key, value);
            self.separator(Tok::CloseObject, "invalid object")?;
        }
        self.pos += 1;
        self.depth -= 1;
        Ok(Node::Object(fields))
    }

    fn array(&mut self) -> Result<Node, String> {
        self.pos += 1;
        let mut members = Vec::new();
        while self.ty() != Tok::CloseArray {
            if self.ty() != Tok::OpenObject {
                return Err(self.err("invalid array member"));
            }
            members.push(self.object()?);
            self.separator(Tok::CloseArray, "invalid array")?;
        }
        self.pos += 1;
        Ok(Node::Array(members))
    }

    /// A comma (not before the closing token) or the closing token itself.
    fn separator(&mut self, close: Tok, what: &str) -> Result<(), String> {
        if self.ty() == Tok::Comma {
            if self.toks[self.pos + 1].0 == close {
                return Err(self.err(what));
            }
            self.pos += 1;
        } else if self.ty() != close {
            return Err(self.err(what));
        }
        Ok(())
    }
}

fn append(out: &mut String, n: &Node) {
    match n {
        Node::Str(s) => {
            out.push('"');
            out.push_str(s);
            out.push('"');
        }
        Node::Num(v) => out.push_str(&v.to_string()),
        Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Node::Object(fields) => {
            out.push('{');
            for (i, (k, v)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&format!("\"{k}\":"));
                append(out, v);
            }
            out.push('}');
        }
        Node::Array(members) => {
            out.push('[');
            for (i, m) in members.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                append(out, m);
            }
            out.push(']');
        }
    }
}

/// Parses `input` (tmux `json_parse`); the top level is an object.
pub fn parse(input: &str) -> Result<Node, String> {
    if input.is_empty() {
        return Err("empty input".into());
    }
    let toks = tokenize(input)?;
    let mut p = Parser { input, toks, pos: 0, depth: 0 };
    if p.ty() != Tok::OpenObject {
        return Err(p.err("expected object"));
    }
    let node = p.object()?;
    if p.ty() != Tok::Eof {
        return Err(p.err("unexpected trailing data"));
    }
    Ok(node)
}

/// Parses `input` and prints it back in tmux's canonical form.
pub fn normalize(input: &str) -> Result<String, String> {
    let node = parse(input)?;
    let mut out = String::new();
    append(&mut out, &node);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn json_subset() {
        assert_eq!(normalize(r#" { "b": 1, "a": "x\"y", "c": [ {"z": true}, {} ], "d": {"e": -5} } "#).unwrap(), r#"{"a":"x\"y","b":1,"c":[{"z":true},{}],"d":{"e":-5}}"#);
        assert_eq!(normalize("").unwrap_err(), "empty input");
        assert_eq!(normalize("[]").unwrap_err(), "expected object: []");
        assert_eq!(normalize(r#"{"a":1,}"#).unwrap_err(), "invalid object: ,}");
        assert_eq!(normalize(r#"{"a":1,"a":2}"#).unwrap_err(), "duplicate key: :2}");
        assert_eq!(normalize(r#"{"a":01}"#).unwrap_err(), "invalid number: 01}");
        assert_eq!(normalize(r#"{"a":null}"#).unwrap_err(), "invalid boolean: null}");
        assert_eq!(normalize(r#"{"a":[1]}"#).unwrap_err(), "tokenization error: 1]}");
        assert_eq!(normalize(r#"{"a":""}"#).unwrap_err(), "invalid string: \"\"}");
        assert_eq!(normalize(r#"{} x"#).unwrap_err(), "tokenization error: x");
    }
}
