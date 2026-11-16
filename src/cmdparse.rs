//! tmux command / configuration-file language parser: a port of cmd-parse.y.
//!
//! The lexer follows yylex and its helpers byte by byte (quotes, escapes, `$NAME`, `~`,
//! comments, backslash-newline continuations, `%if`/`%elif`/`%else`/`%endif`/`%hidden`);
//! the grammar is parsed by recursive descent with one token of lookahead, so errors
//! ("syntax error" or a lexer error) and the line of each command are found at the same
//! tokens as by tmux's yacc parser. The result is a tree: a `{ ... }` argument is a list
//! of commands. `NAME=value` assignments take effect while parsing (a later `$NAME`
//! sees them) and are returned so the caller can put them in the global environment.

use std::collections::HashMap;

/// One argument of a parsed command: a string or a `{ ... }` block of commands.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Str(String),
    Block(Vec<Cmd>),
}

/// A parsed command: its words and the line it ends on (cmd_parse_command's line: the
/// lexer's line when the command was reduced, after the token following it).
#[derive(Clone, Debug, PartialEq)]
pub struct Cmd {
    pub args: Vec<Arg>,
    pub line: usize,
}

/// The commands of a parse and the environment assignments made by it, in order
/// (name, value, hidden).
#[derive(Debug, Default)]
pub struct Parsed {
    pub cmds: Vec<Cmd>,
    pub env: Vec<(String, String, bool)>,
}

/// Parse a config file or command string (cmd_parse_run_parser). `cond` decides `%if` and
/// `%elif` conditions (it gets the unexpanded condition), `env` looks up `$NAME` and
/// `~` (the server's global environment). An error is (line, message).
pub fn parse_tree(input: &str, cond: &dyn Fn(&str) -> bool, env: &dyn Fn(&str) -> Option<String>) -> Result<Parsed, (usize, String)> {
    // pwsh-tmux reads Windows files: \r\n is a newline everywhere
    let normalized = input.replace("\r\n", "\n");
    let mut p = Parser {
        lx: Lexer {
            buf: normalized.into_bytes(),
            off: 0,
            escapes: 0,
            line: 1,
            eol: false,
            eof: false,
            condition: false,
            error: None,
            env,
            assigned: HashMap::new(),
        },
        la: None,
        cond,
        scope: None,
        stack: Vec::new(),
        env: Vec::new(),
    };
    match p.lines() {
        Ok(cmds) => Ok(Parsed { cmds, env: p.env }),
        Err(()) => Err(p.lx.error.take().unwrap_or((p.lx.line, "syntax error".into()))),
    }
}

fn default_cond(c: &str) -> bool {
    !c.is_empty() && c != "0"
}

fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Parse a config file or command string into a list of commands (each an argv).
/// `{ ... }` blocks are given as text (their commands quoted and joined by " ; ") and
/// assignments become `set-environment` commands in front.
///
/// `%if` conditions are treated as true when non-empty and not "0".
pub fn parse(input: &str) -> Result<Vec<Vec<String>>, String> {
    parse_with(input, &default_cond)
}

/// Like [`parse`], but `%if` / `%elif` conditions are evaluated by `cond`
/// (which receives the condition text, e.g. an unexpanded format).
pub fn parse_with(input: &str, cond: &dyn Fn(&str) -> bool) -> Result<Vec<Vec<String>>, String> {
    parse_with_env(input, cond, &process_env)
}

/// Like [`parse_with`], but `$NAME` / `${NAME}` are looked up with `env` (the server's
/// global environment) instead of the process environment.
pub fn parse_with_env(input: &str, cond: &dyn Fn(&str) -> bool, env: &dyn Fn(&str) -> Option<String>) -> Result<Vec<Vec<String>>, String> {
    let p = parse_tree(input, cond, env).map_err(|e| e.1)?;
    Ok(flatten(p))
}

/// Put in front of a `{ ... }` block argument of bind-key by [`parse`]
/// (never typed: it starts with NUL).
pub const BLOCK_MARK: &str = "\0{";

/// Like [`parse`], but each argument also says whether it was a `{ ... }`
/// block (its text is then the block's commands), as tmux keeps command blocks
/// apart from strings.
pub fn parse_blocks(input: &str) -> Result<Vec<Vec<(String, bool)>>, String> {
    let p = parse_tree(input, &default_cond, &process_env).map_err(|e| e.1)?;
    Ok(flatten_blocks(p))
}

/// The assignments as set-environment commands, then the commands with their blocks as
/// text, each argument saying whether it was a block.
pub fn flatten_blocks(p: Parsed) -> Vec<Vec<(String, bool)>> {
    let mut out: Vec<Vec<(String, bool)>> = p.env.into_iter().map(|(n, v, hidden)| {
        vec![("set-environment".into(), false), ((if hidden { "-gh" } else { "-g" }).into(), false), (n, false), (v, false)]
    }).collect();
    out.extend(p.cmds.iter().map(|c| c.args.iter().map(|a| match a {
        Arg::Str(s) => (s.clone(), false),
        Arg::Block(b) => (block_text(b), true),
    }).collect()));
    out
}

/// [`flatten_blocks`] with bind-key's blocks marked with [`BLOCK_MARK`] (list-keys prints
/// them as blocks; any unique abbreviation, "bi", "bind-k", ..., is bind-key).
fn flatten(p: Parsed) -> Vec<Vec<String>> {
    flatten_blocks(p).into_iter().map(|c| {
        let bind = c.first().is_some_and(|(n, _)| is_bind_key(n));
        c.into_iter().map(|(w, b)| if b && bind { format!("{BLOCK_MARK}{w}") } else { w }).collect()
    }).collect()
}

/// Whether a command name is bind-key or an abbreviation of it.
pub fn is_bind_key(name: &str) -> bool {
    name.len() >= 2 && "bind-key".starts_with(name)
}

/// A block's commands as text that parses back to them: arguments quoted, nested blocks
/// in braces, commands separated by " ; ".
pub fn block_text(cmds: &[Cmd]) -> String {
    cmds.iter()
        .map(|c| c.args.iter().map(|a| match a {
            Arg::Str(s) => quote(s),
            Arg::Block(b) => format!("{{ {} }}", block_text(b)),
        }).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join(" ; ")
}

/// Split an already-tokenized argv (the OS command line, or a stored command) into
/// commands as cmd_parse_from_arguments does: an argument ending in ";" ends a command
/// (an argument that was only ";" goes away), and a trailing "\;" is a literal ";" (so
/// `tmux bind K send -X a '\;' send -X b` gives bind-key a ";" argument, which its own
/// split then treats as a separator). Empty commands are dropped.
pub fn split_commands(args: &[String]) -> Vec<Vec<String>> {
    split_arguments(args)
}

/// Split already-separated arguments into commands as cmd_parse_from_arguments does
/// (bind-key with several arguments): an argument ending in ";" ends a command (the
/// ";" is dropped, and an argument that was only ";" goes away), but "\;" at the end
/// is a literal ";". Arguments starting with [`BLOCK_MARK`] are { } blocks and are kept.
pub fn split_arguments(args: &[String]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    for a in args {
        let mut end = false;
        let mut a = a.clone();
        if !a.starts_with(BLOCK_MARK) && a.ends_with(';') {
            a.pop();
            if a.ends_with('\\') {
                a.pop();
                a.push(';');
            } else {
                end = true;
            }
        }
        if !end || !a.is_empty() {
            cur.push(a);
        }
        if end {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.retain(|c| !c.is_empty());
    out
}

/// Quote one argument so `parse` returns it unchanged (used by list-keys / show-options output).
pub fn quote(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    let needs = arg.starts_with('%')
        || looks_like_assignment(arg)
        || arg
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "'\"\\$;{}#~".contains(c));
    if !needs {
        return arg.to_string();
    }
    if !arg.contains('\'') && !arg.chars().any(|c| c.is_control()) {
        return format!("'{arg}'");
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for (i, c) in arg.chars().enumerate() {
        match c {
            // ~ just after the opening quote is expanded (yylex_token)
            '~' if i == 0 => out.push_str("\\~"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\x1b' => out.push_str("\\e"),
            c if c.is_control() && (c as u32) < 0x80 => {
                out.push_str(&format!("\\{:03o}", c as u32));
            }
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn looks_like_assignment(s: &str) -> bool {
    match s.find('=') {
        Some(i) if i > 0 => {
            let name = &s[..i];
            !name.starts_with(|c: char| c.is_ascii_digit()) && name.chars().all(is_name_char)
        }
        _ => false,
    }
}

fn home_dir() -> String {
    if let Ok(h) = std::env::var("HOME")
        && !h.is_empty()
    {
        return h;
    }
    #[cfg(windows)]
    if let Ok(h) = std::env::var("USERPROFILE") {
        return h;
    }
    #[cfg(unix)]
    if let Some(h) = user_home(None) {
        return h;
    }
    String::new()
}

/// getpwuid(getuid()) (None) or getpwnam(name): the user's home directory.
#[cfg(unix)]
fn user_home(name: Option<&[u8]>) -> Option<String> {
    let name = match name {
        Some(n) => Some(std::ffi::CString::new(n).ok()?),
        None => None,
    };
    // SAFETY: getpwnam/getpwuid return NULL or a pointer to a static passwd entry that is
    // read before any other call could replace it.
    unsafe {
        let pw = match &name {
            Some(n) => libc::getpwnam(n.as_ptr()),
            None => libc::getpwuid(libc::getuid()),
        };
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned())
    }
}

/// There is no user database to look `~name` up in.
#[cfg(not(unix))]
fn user_home(name: Option<&[u8]>) -> Option<String> {
    name.is_none().then(home_dir).filter(|h| !h.is_empty())
}

const EOF: i32 = -1;

const fn ch(b: u8) -> i32 {
    b as i32
}

/// yylex_is_var
fn is_var(c: i32, first: bool) -> bool {
    if c == EOF || c == ch(b'=') {
        return false;
    }
    let c = c as u8;
    !(first && c.is_ascii_digit()) && (c.is_ascii_alphanumeric() || c == b'_')
}

fn lossy(b: Vec<u8>) -> String {
    String::from_utf8(b).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// The lexer's tokens (the yacc %token list plus its single characters).
#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// yylex returned 0: the input is done.
    End,
    Nl,
    Semi,
    Open,
    Close,
    Token(String),
    Equals(String),
    Format(String),
    Hidden,
    If,
    Else,
    Elif,
    Endif,
    Error,
}

struct Lexer<'a> {
    buf: Vec<u8>,
    off: usize,
    /// Backslashes read ahead by getc (ps->escapes).
    escapes: usize,
    line: usize,
    /// A newline was returned: the line goes up with the next token.
    eol: bool,
    eof: bool,
    /// The previous token was a % directive: #{ starts a format.
    condition: bool,
    /// The first error (yyerror keeps only that).
    error: Option<(usize, String)>,
    env: &'a dyn Fn(&str) -> Option<String>,
    /// Assignments made earlier in this parse; they shadow `env`.
    assigned: HashMap<String, String>,
}

impl Lexer<'_> {
    fn yyerror(&mut self, msg: &str) {
        if self.error.is_none() {
            self.error = Some((self.line, msg.to_string()));
        }
    }

    fn lookup(&self, name: &str) -> Option<String> {
        self.assigned.get(name).cloned().or_else(|| (self.env)(name))
    }

    fn getc1(&mut self) -> i32 {
        match self.buf.get(self.off) {
            Some(&b) => {
                self.off += 1;
                ch(b)
            }
            None => EOF,
        }
    }

    fn ungetc(&mut self, c: i32) {
        if self.off > 0 && c != EOF {
            self.off -= 1;
        }
    }

    /// yylex_getc: an odd run of backslashes before a newline continues the line.
    fn getc(&mut self) -> i32 {
        if self.escapes != 0 {
            self.escapes -= 1;
            return ch(b'\\');
        }
        loop {
            let c = self.getc1();
            if c == ch(b'\\') {
                self.escapes += 1;
                continue;
            }
            if c == ch(b'\n') && self.escapes % 2 == 1 {
                self.line += 1;
                self.escapes -= 1;
                continue;
            }
            if self.escapes != 0 {
                self.ungetc(c);
                self.escapes -= 1;
                return ch(b'\\');
            }
            return c;
        }
    }

    /// yylex_get_word: up to a blank.
    fn get_word(&mut self, mut c: i32) -> Vec<u8> {
        let mut buf = Vec::new();
        loop {
            buf.push(c as u8);
            c = self.getc();
            if c == EOF || matches!(c as u8, b' ' | b'\t' | b'\n') {
                break;
            }
        }
        self.ungetc(c);
        buf
    }

    /// yylex
    fn lex(&mut self) -> Tok {
        if self.eol {
            self.line += 1;
        }
        self.eol = false;
        let condition = std::mem::take(&mut self.condition);
        loop {
            let mut c = self.getc();
            if c == EOF {
                // every file or string ends with a newline
                if self.eof {
                    return Tok::End;
                }
                self.eof = true;
                return Tok::Nl;
            }
            if c == ch(b' ') || c == ch(b'\t') {
                continue;
            }
            if c == ch(b'\r') {
                c = self.getc();
                if c != ch(b'\n') {
                    self.ungetc(c);
                    c = ch(b'\r');
                }
            }
            if c == ch(b'\n') {
                self.eol = true;
                return Tok::Nl;
            }
            match c as u8 {
                b';' => return Tok::Semi,
                b'{' => return Tok::Open,
                b'}' => return Tok::Close,
                b'#' => {
                    // #{ after a condition opens a format; anything else is a comment
                    let mut next = self.getc();
                    if condition && next == ch(b'{') {
                        return self.format().map_or(Tok::Error, Tok::Format);
                    }
                    while next != ch(b'\n') && next != EOF {
                        next = self.getc();
                    }
                    if next == ch(b'\n') {
                        self.line += 1;
                        return Tok::Nl;
                    }
                    continue;
                }
                b'%' => {
                    // a condition unless it is all % or all digits
                    let word = self.get_word(c);
                    if word.iter().all(|b| *b == b'%' || b.is_ascii_digit()) {
                        return Tok::Token(lossy(word));
                    }
                    self.condition = true;
                    return match &word[..] {
                        b"%hidden" => Tok::Hidden,
                        b"%if" => Tok::If,
                        b"%else" => Tok::Else,
                        b"%elif" => Tok::Elif,
                        b"%endif" => Tok::Endif,
                        _ => Tok::Error,
                    };
                }
                _ => {}
            }
            let Some(token) = self.token(c) else { return Tok::Error };
            if let Some(eq) = token.iter().position(|b| *b == b'=')
                && is_var(ch(token[0]), true)
                && token[1..eq].iter().all(|b| is_var(ch(*b), false))
            {
                return Tok::Equals(lossy(token));
            }
            return Tok::Token(lossy(token));
        }
    }

    /// yylex_format: #{ ... } with nested #{ up to the matching }, on one line.
    fn format(&mut self) -> Option<String> {
        let mut buf = b"#{".to_vec();
        let mut brackets = 1;
        loop {
            let mut c = self.getc();
            if c == EOF || c == ch(b'\n') {
                return None;
            }
            if c == ch(b'#') {
                c = self.getc();
                if c == EOF || c == ch(b'\n') {
                    return None;
                }
                if c == ch(b'{') {
                    brackets += 1;
                }
                buf.push(b'#');
            } else if c == ch(b'}') && brackets != 0 {
                brackets -= 1;
                if brackets == 0 {
                    buf.push(b'}');
                    break;
                }
            }
            buf.push(c as u8);
        }
        Some(lossy(buf))
    }

    /// yylex_token_escape, after the backslash.
    fn escape(&mut self, buf: &mut Vec<u8>) -> bool {
        let c = self.getc();
        if (ch(b'4')..=ch(b'7')).contains(&c) {
            self.yyerror("invalid octal escape");
            return false;
        }
        if (ch(b'0')..=ch(b'3')).contains(&c) {
            let o2 = self.getc();
            if (ch(b'0')..=ch(b'7')).contains(&o2) {
                let o3 = self.getc();
                if (ch(b'0')..=ch(b'7')).contains(&o3) {
                    buf.push((64 * (c - ch(b'0')) + 8 * (o2 - ch(b'0')) + (o3 - ch(b'0'))) as u8);
                    return true;
                }
            }
            self.yyerror("invalid octal escape");
            return false;
        }
        if c == EOF {
            return false;
        }
        let b = match c as u8 {
            b'a' => 0x07,
            b'b' => 0x08,
            b'e' => 0x1b,
            b'f' => 0x0c,
            b's' => b' ',
            b'v' => 0x0b,
            b'r' => b'\r',
            b'n' => b'\n',
            b't' => b'\t',
            t @ (b'u' | b'U') => return self.unicode(buf, t),
            other => other,
        };
        buf.push(b);
        true
    }

    fn unicode(&mut self, buf: &mut Vec<u8>, t: u8) -> bool {
        let size = if t == b'u' { 4 } else { 8 };
        let mut s = String::new();
        for _ in 0..size {
            let c = self.getc();
            if c == EOF || c == ch(b'\n') {
                return false;
            }
            if !(c as u8).is_ascii_hexdigit() {
                self.yyerror(&format!("invalid \\{} argument", t as char));
                return false;
            }
            s.push(c as u8 as char);
        }
        match u32::from_str_radix(&s, 16).ok().and_then(char::from_u32) {
            Some(u) => {
                buf.extend(u.encode_utf8(&mut [0; 4]).as_bytes());
                true
            }
            None => {
                self.yyerror(&format!("invalid \\{} argument", t as char));
                false
            }
        }
    }

    /// yylex_token_variable, after the $: a $ not followed by a name is itself, ${ must be
    /// a name closed by }.
    fn variable(&mut self, buf: &mut Vec<u8>) -> bool {
        let mut c = self.getc();
        if c == EOF {
            return false;
        }
        let mut name = Vec::new();
        let brackets = c == ch(b'{');
        if !brackets {
            if !is_var(c, true) {
                buf.push(b'$');
                self.ungetc(c);
                return true;
            }
            name.push(c as u8);
        }
        loop {
            c = self.getc();
            if brackets && c == ch(b'}') {
                break;
            }
            if c == EOF || !is_var(c, false) {
                if !brackets {
                    self.ungetc(c);
                    break;
                }
                self.yyerror("invalid environment variable");
                return false;
            }
            if name.len() == 1022 {
                self.yyerror("environment variable is too long");
                return false;
            }
            name.push(c as u8);
        }
        if let Some(v) = self.lookup(&lossy(name)) {
            buf.extend(v.as_bytes());
        }
        true
    }

    /// yylex_token_tilde, after the ~: ~ is $HOME (or the user's home), ~name that
    /// user's; an unknown user is an error.
    fn tilde(&mut self, buf: &mut Vec<u8>) -> bool {
        let mut name = Vec::new();
        loop {
            let c = self.getc();
            if c == EOF || b"/ \t\n\"'".contains(&(c as u8)) {
                self.ungetc(c);
                break;
            }
            if name.len() == 1022 {
                self.yyerror("user name is too long");
                return false;
            }
            name.push(c as u8);
        }
        let home = if name.is_empty() {
            self.lookup("HOME").filter(|h| !h.is_empty()).or_else(|| Some(home_dir()).filter(|h| !h.is_empty()))
        } else {
            user_home(Some(&name))
        };
        match home {
            Some(h) => {
                buf.extend(h.as_bytes());
                true
            }
            None => false,
        }
    }

    /// yylex_token: one word, with quotes, escapes, $ and ~ expanded. None is ERROR.
    fn token(&mut self, mut c: i32) -> Option<Vec<u8>> {
        #[derive(Clone, Copy, PartialEq)]
        enum St {
            Start,
            None,
            Double,
            Single,
        }
        let (mut state, mut last) = (St::None, St::Start);
        let mut buf = Vec::new();
        loop {
            // EOF or \n are always the end of the token
            if c == EOF {
                break;
            }
            if state == St::None && c == ch(b'\r') {
                c = self.getc();
                if c != ch(b'\n') {
                    self.ungetc(c);
                    c = ch(b'\r');
                }
            }
            if c == ch(b'\n') {
                if state == St::None {
                    break;
                }
                self.line += 1;
            }
            // whitespace, ; or } end a token unless inside quotes
            if state == St::None && (c == ch(b' ') || c == ch(b'\t') || c == ch(b';') || c == ch(b'}')) {
                break;
            }
            // spaces and comments after \n inside quotes are removed but the \n is left
            if c == ch(b'\n') && state != St::None {
                buf.push(b'\n');
                loop {
                    c = self.getc();
                    if c != ch(b' ') && c != ch(b'\t') {
                        break;
                    }
                }
                if c != ch(b'#') {
                    continue;
                }
                c = self.getc();
                if c != EOF && b",#{}:".contains(&(c as u8)) {
                    self.ungetc(c);
                    c = ch(b'#');
                } else {
                    while c != ch(b'\n') && c != EOF {
                        c = self.getc();
                    }
                }
                continue;
            }
            // \ ~ and $ are expanded except in single quotes
            let expanded = if c == ch(b'\\') && state != St::Single {
                Some(self.escape(&mut buf))
            } else if c == ch(b'~') && last != state && state != St::Single {
                Some(self.tilde(&mut buf))
            } else if c == ch(b'$') && state != St::Single {
                Some(self.variable(&mut buf))
            } else {
                None
            };
            match expanded {
                Some(false) => return None,
                Some(true) => {}
                None => {
                    // ' and " start or end quotes (and are consumed)
                    let toggle = match (c as u8, state) {
                        (b'\'', St::None) => Some(St::Single),
                        (b'\'', St::Single) => Some(St::None),
                        (b'"', St::None) => Some(St::Double),
                        (b'"', St::Double) => Some(St::None),
                        _ => None,
                    };
                    if let Some(s) = toggle {
                        state = s;
                        c = self.getc();
                        continue;
                    }
                    buf.push(c as u8);
                }
            }
            last = state;
            c = self.getc();
        }
        self.ungetc(c);
        Some(buf)
    }
}

/// The grammar of cmd-parse.y, by recursive descent. Each function stops at the token
/// yacc would stop at, so a "syntax error" is reported at the same line.
struct Parser<'a> {
    lx: Lexer<'a>,
    la: Option<Tok>,
    cond: &'a dyn Fn(&str) -> bool,
    /// ps->scope (the innermost %if) and ps->stack (the ones around it).
    scope: Option<bool>,
    stack: Vec<bool>,
    env: Vec<(String, String, bool)>,
}

type R<T> = Result<T, ()>;

impl Parser<'_> {
    fn peek(&mut self) -> &Tok {
        if self.la.is_none() {
            self.la = Some(self.lx.lex());
        }
        self.la.as_ref().unwrap()
    }

    fn take(&mut self) -> Tok {
        self.peek();
        self.la.take().unwrap()
    }

    fn syntax<T>(&mut self) -> R<T> {
        self.lx.yyerror("syntax error");
        Err(())
    }

    fn expect(&mut self, t: Tok) -> R<()> {
        if self.take() == t { Ok(()) } else { self.syntax() }
    }

    /// Commands are kept when the innermost %if is true (or there is none).
    fn flag(&self) -> bool {
        self.scope.unwrap_or(true)
    }

    /// assignment / hidden_assignment: only when every enclosing %if is true.
    fn assign(&mut self, s: String, hidden: bool) {
        if self.flag() && self.stack.iter().all(|f| *f) {
            let (n, v) = s.split_once('=').unwrap();
            self.lx.assigned.insert(n.to_string(), v.to_string());
            self.env.push((n.to_string(), v.to_string(), hidden));
        }
    }

    /// lines: statement '\n' ...
    fn lines(&mut self) -> R<Vec<Cmd>> {
        let mut out = Vec::new();
        while *self.peek() != Tok::End {
            let s = self.statement()?;
            self.expect(Tok::Nl)?;
            out.extend(s);
        }
        Ok(out)
    }

    /// statement: empty, %hidden NAME=value, a multi-line %if, or commands. The caller
    /// takes the '\n' (or '}') after it.
    fn statement(&mut self) -> R<Vec<Cmd>> {
        let list = match self.peek() {
            Tok::Nl | Tok::Close => return Ok(Vec::new()),
            Tok::Hidden => {
                self.take();
                return match self.take() {
                    Tok::Equals(s) => {
                        self.assign(s, true);
                        Ok(Vec::new())
                    }
                    _ => self.syntax(),
                };
            }
            Tok::If => {
                self.take();
                let c = self.if_open()?;
                if *self.peek() == Tok::Nl {
                    self.condition(c)?
                } else {
                    let l = self.condition1(c)?;
                    self.commands_rest(l)?
                }
            }
            Tok::Token(_) | Tok::Equals(_) => self.commands()?,
            _ => return self.syntax(),
        };
        Ok(if self.flag() { list } else { Vec::new() })
    }

    /// statements in a %if: at least one statement '\n', up to %elif, %else or %endif.
    fn statements(&mut self) -> R<Vec<Cmd>> {
        let mut out = Vec::new();
        let mut first = true;
        loop {
            if !first && matches!(self.peek(), Tok::Else | Tok::Elif | Tok::Endif) {
                return Ok(out);
            }
            first = false;
            let s = self.statement()?;
            self.expect(Tok::Nl)?;
            out.extend(s);
        }
    }

    /// expanded: the condition after %if or %elif (a format or a token).
    fn expanded(&mut self) -> R<bool> {
        match self.take() {
            Tok::Format(s) | Tok::Token(s) => Ok((self.cond)(&s)),
            _ => self.syntax(),
        }
    }

    fn if_open(&mut self) -> R<bool> {
        let v = self.expanded()?;
        if let Some(s) = self.scope {
            self.stack.push(s);
        }
        self.scope = Some(v);
        Ok(v)
    }

    fn if_elif(&mut self) -> R<bool> {
        let v = self.expanded()?;
        self.scope = Some(v);
        Ok(v)
    }

    fn if_else(&mut self) {
        self.scope = Some(!self.flag());
    }

    fn if_close(&mut self) {
        self.scope = self.stack.pop();
    }

    /// condition: %if over several lines, after if_open.
    fn condition(&mut self, c1: bool) -> R<Vec<Cmd>> {
        self.expect(Tok::Nl)?;
        let body = self.statements()?;
        let mut chosen = c1.then_some(body);
        loop {
            match self.take() {
                Tok::Elif => {
                    let c = self.if_elif()?;
                    self.expect(Tok::Nl)?;
                    let b = self.statements()?;
                    if chosen.is_none() && c {
                        chosen = Some(b);
                    }
                }
                Tok::Else => {
                    self.if_else();
                    self.expect(Tok::Nl)?;
                    let b = self.statements()?;
                    self.expect(Tok::Endif)?;
                    self.if_close();
                    return Ok(chosen.unwrap_or(b));
                }
                Tok::Endif => {
                    self.if_close();
                    return Ok(chosen.unwrap_or_default());
                }
                _ => return self.syntax(),
            }
        }
    }

    /// condition1: %if ... %endif inside one line, after if_open.
    fn condition1(&mut self, c1: bool) -> R<Vec<Cmd>> {
        let body = self.commands()?;
        let mut chosen = c1.then_some(body);
        loop {
            match self.take() {
                Tok::Elif => {
                    let c = self.if_elif()?;
                    let b = self.commands()?;
                    if chosen.is_none() && c {
                        chosen = Some(b);
                    }
                }
                Tok::Else => {
                    self.if_else();
                    let b = self.commands()?;
                    self.expect(Tok::Endif)?;
                    self.if_close();
                    return Ok(chosen.unwrap_or(b));
                }
                Tok::Endif => {
                    self.if_close();
                    return Ok(chosen.unwrap_or_default());
                }
                _ => return self.syntax(),
            }
        }
    }

    /// commands: command or condition1, then more after each ';'.
    fn commands(&mut self) -> R<Vec<Cmd>> {
        let list = match self.peek() {
            Tok::If => {
                self.take();
                let c = self.if_open()?;
                self.condition1(c)?
            }
            Tok::Token(_) | Tok::Equals(_) => {
                let cmd = self.command()?;
                if !cmd.args.is_empty() && self.flag() { vec![cmd] } else { Vec::new() }
            }
            _ => return self.syntax(),
        };
        self.commands_rest(list)
    }

    fn commands_rest(&mut self, mut list: Vec<Cmd>) -> R<Vec<Cmd>> {
        while *self.peek() == Tok::Semi {
            self.take();
            match self.peek() {
                Tok::If => {
                    self.take();
                    let c = self.if_open()?;
                    let l = self.condition1(c)?;
                    list.extend(l);
                }
                Tok::Token(_) | Tok::Equals(_) => {
                    let cmd = self.command()?;
                    // as in cmd-parse.y, a command that is dropped (an assignment, or
                    // one in a false %if) drops those before it too
                    if !cmd.args.is_empty() && self.flag() {
                        list.push(cmd);
                    } else {
                        list.clear();
                    }
                }
                _ => {}
            }
        }
        Ok(list)
    }

    /// command: NAME=value, or [NAME=value] name arguments... An assignment takes effect
    /// at once (before the next token is read).
    fn command(&mut self) -> R<Cmd> {
        if let Tok::Equals(_) = self.peek() {
            let Tok::Equals(s) = self.take() else { unreachable!() };
            self.assign(s, false);
            if !matches!(self.peek(), Tok::Token(_)) {
                return Ok(Cmd { args: Vec::new(), line: self.lx.line });
            }
        }
        let Tok::Token(name) = self.take() else { return self.syntax() };
        let mut args = vec![Arg::Str(name)];
        loop {
            match self.peek() {
                Tok::Token(_) | Tok::Equals(_) => {
                    let (Tok::Token(s) | Tok::Equals(s)) = self.take() else { unreachable!() };
                    args.push(Arg::Str(s));
                }
                Tok::Open => {
                    self.take();
                    let b = self.block()?;
                    args.push(Arg::Block(b));
                }
                _ => break,
            }
        }
        Ok(Cmd { args, line: self.lx.line })
    }

    /// argument_statements: statement '\n' ... statement '}', after the '{'.
    fn block(&mut self) -> R<Vec<Cmd>> {
        let mut out = Vec::new();
        loop {
            let s = self.statement()?;
            out.extend(s);
            match self.take() {
                Tok::Nl => {}
                Tok::Close => return Ok(out),
                _ => return self.syntax(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn p(s: &str) -> Vec<Vec<String>> {
        parse(s).unwrap_or_else(|e| panic!("parse failed for {s:?}: {e}"))
    }

    #[test]
    fn blocks_are_marked() {
        let c = parse_blocks("A=1\nif -F 1 { display a } 'b'; bind x {\n  neww\n}").unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!(c[1][3], ("display a".to_string(), true));
        assert_eq!(c[1][4], ("b".to_string(), false));
        assert!(c[2][2].1 && !c[2][1].1 && !c[0].iter().any(|w| w.1));
    }

    #[test]
    fn simple_words_and_lines() {
        assert_eq!(
            p("new-session -d\nsplit-window   -h\t-p 30\n"),
            vec![
                v(&["new-session", "-d"]),
                v(&["split-window", "-h", "-p", "30"])
            ]
        );
        assert!(p("").is_empty());
        assert!(p("\n\n   \n\t\n").is_empty());
    }

    #[test]
    fn comments() {
        assert_eq!(
            p("# full line\nset -g x 1 # trailing\n  # indented\nfoo a#b"),
            vec![v(&["set", "-g", "x", "1"]), v(&["foo", "a#b"])]
        );
        assert_eq!(
            p("set x '# not a comment' \"#also not\""),
            vec![v(&["set", "x", "# not a comment", "#also not"])]
        );
    }

    #[test]
    fn unquoted_format_is_a_comment() {
        // yylex: # at the start of a token is a comment unless a condition comes next
        assert_eq!(p("display -p #{pane_current_path}"), vec![v(&["display", "-p"])]);
        // and an unquoted } ends a token, so a format inside one is an error
        assert_eq!(parse("display -p x#{pane_id}").unwrap_err(), "syntax error");
        assert_eq!(p("display -p 'x#{pane_id}'"), vec![v(&["display", "-p", "x#{pane_id}"])]);
        assert_eq!(p("foo #[fg=red] bar"), vec![v(&["foo"])]);
        assert_eq!(p("%if #{==:a b,a b}\nx\n%endif"), vec![v(&["x"])]);
    }

    #[test]
    fn single_quotes_literal() {
        assert_eq!(
            p(r#"echo 'a b\n $HOME "x"' 'it''s'"#),
            vec![v(&["echo", r#"a b\n $HOME "x""#, "its"])]
        );
        assert_eq!(p("x ''"), vec![v(&["x", ""])]);
        assert_eq!(p("x a'b c'd"), vec![v(&["x", "ab cd"])]);
    }

    #[test]
    fn double_quote_escapes() {
        assert_eq!(
            p(r#"x "q\"b\\s\n\t\e\$\u00e9\U0001F600\101\102""#),
            vec![v(&["x", "q\"b\\s\n\t\x1b$\u{e9}\u{1F600}AB"])]
        );
        assert_eq!(p(r#"x "a\zb""#), vec![v(&["x", "azb"])]);
        assert!(parse(r#"x "\uZZZZ""#).is_err());
        assert!(parse(r#"x "\18""#).is_err());
    }

    #[test]
    fn env_expansion() {
        assert_eq!(p(r#"display "$2:@4:%3" $12"#), vec![v(&["display", "$2:@4:%3", "$12"])]);
        unsafe {
            std::env::set_var("CMDPARSE_T1", "val one");
            std::env::remove_var("CMDPARSE_UNSET");
        }
        assert_eq!(
            p(
                r#"a $CMDPARSE_T1 x${CMDPARSE_T1}y "$CMDPARSE_T1!" '$CMDPARSE_T1' \$CMDPARSE_T1 [$CMDPARSE_UNSET] $ ${}"#
            ),
            vec![v(&[
                "a",
                "val one",
                "xval oney",
                "val one!",
                "$CMDPARSE_T1",
                "$CMDPARSE_T1",
                "[]",
                "$",
                "",
            ])]
        );
        // yylex_token_variable: ${ needs a name and }; a $ at the very end is an error
        assert_eq!(parse("a ${X").unwrap_err(), "invalid environment variable");
        assert_eq!(parse("a ${X-}").unwrap_err(), "invalid environment variable");
        assert_eq!(parse("a $").unwrap_err(), "syntax error");
        assert_eq!(p(r#"a "\$CMDPARSE_T1""#), vec![v(&["a", "$CMDPARSE_T1"])]);
    }

    #[test]
    fn assignments_apply_during_the_parse() {
        let env = |n: &str| (n == "OUTER").then(|| "from-server".to_string());
        let pe = |s: &str| parse_with_env(s, &|c: &str| !c.is_empty() && c != "0", &env).unwrap();
        // tmux: an assignment is visible to ${NAME} on the following lines at once
        assert_eq!(
            pe("%hidden MOD=\"session\"\nset -g \"@ctp_${MOD}_icon\" x\nA=1\nset @a $A ; B=2 ; set @b ${B}${OUTER}"),
            vec![
                v(&["set-environment", "-gh", "MOD", "session"]),
                v(&["set-environment", "-g", "A", "1"]),
                v(&["set-environment", "-g", "B", "2"]),
                v(&["set", "-g", "@ctp_session_icon", "x"]),
                // cmd-parse.y: "commands ';' command" with an assignment drops the
                // commands before it
                v(&["set", "@b", "2from-server"]),
            ]
        );
        // a later assignment replaces an earlier one; inactive branches assign nothing
        assert_eq!(pe("A=1\nA=2\nset @a $A")[2], v(&["set", "@a", "2"]));
        // an assignment in front of a command (optional_assignment TOKEN)
        assert_eq!(pe("V=1 set @v $V"), vec![v(&["set-environment", "-g", "V", "1"]), v(&["set", "@v", "1"])]);
        assert_eq!(pe("%if 0\n%hidden C=no\nD=no\n%endif\nset @c [$C$D]"), vec![v(&["set", "@c", "[]"])]);
        // the lookup is the server's environment, not the process's
        unsafe { std::env::set_var("CMDPARSE_PROC_ONLY", "proc") };
        assert_eq!(pe("set @p [$CMDPARSE_PROC_ONLY]"), vec![v(&["set", "@p", "[]"])]);
    }

    #[test]
    fn tilde_expansion() {
        let home = home_dir();
        assert_eq!(
            p("cd ~ ~/x a~ '~' \"~\" \"~/y\" a\"~\" \"a~\""),
            vec![v(&["cd", &home, &format!("{home}/x"), "a~", "~", &home, &format!("{home}/y"), &format!("a{home}"), "a~"])]
        );
        // an unknown user is an error (yylex_token_tilde)
        assert_eq!(parse("cd ~no-such-user-here").unwrap_err(), "syntax error");
    }

    #[test]
    fn unquoted_backslash() {
        assert_eq!(p(r"x a\ b \# \'q"), vec![v(&["x", "a b", "#", "'q"])]);
        // escapes work outside quotes as well (yylex_token_escape)
        assert_eq!(p(r"x a\nb \e\s\101 \u00e9"), vec![v(&["x", "a\nb", "\x1b A", "\u{e9}"])]);
    }

    #[test]
    fn line_continuation() {
        assert_eq!(
            p("bind x \\\n  split-window \\\n  -h\nnext"),
            vec![v(&["bind", "x", "split-window", "-h"]), v(&["next"])]
        );
        assert_eq!(p("ab\\\ncd"), vec![v(&["abcd"])]);
        assert_eq!(p("x \"ab\\\ncd\""), vec![v(&["x", "abcd"])]);
        assert_eq!(p("a \\\r\n b"), vec![v(&["a", "b"])]);
    }

    #[test]
    fn semicolons() {
        assert_eq!(
            p("new -d ; split-window -h;   ; select-pane -L;"),
            vec![
                v(&["new", "-d"]),
                v(&["split-window", "-h"]),
                v(&["select-pane", "-L"])
            ]
        );
        assert_eq!(
            p(r"bind-key x split-window \; select-pane -L"),
            vec![v(&[
                "bind-key",
                "x",
                "split-window",
                ";",
                "select-pane",
                "-L"
            ])]
        );
        // an unquoted ; ends the word and the command even without a blank (yylex_token)
        assert_eq!(
            p("x a;b ';' \";\" foo\\;"),
            vec![v(&["x", "a"]), v(&["b", ";", ";", "foo;"])]
        );
    }

    #[test]
    fn braces() {
        assert_eq!(
            p("bind x { split-window; select-pane -L }"),
            vec![v(&["bind", "x", "\0{split-window ; select-pane -L"])]
        );
        // only bind-key's blocks are marked
        assert_eq!(p("unbind x { y }"), vec![v(&["unbind", "x", "y"])]);
        let cmds = p("bind x {\n  split-window -h # comment }\n  select-pane -L\n}\nnext");
        assert_eq!(
            cmds,
            vec![
                v(&["bind", "x", "\0{split-window -h ; select-pane -L"]),
                v(&["next"])
            ]
        );
        // Nested, quotes with braces inside, formats inside.
        let cmds = p("if -F 1 { if -F 1 { display '}' } ; display \"#{pane_id}\" } { x }");
        assert_eq!(
            cmds,
            vec![v(&[
                "if",
                "-F",
                "1",
                "if -F 1 { display '}' } ; display '#{pane_id}'",
                "x"
            ])]
        );
        // Re-parse of the block.
        assert_eq!(
            p(&cmds[0][3]),
            vec![
                v(&["if", "-F", "1", "display '}'"]),
                v(&["display", "#{pane_id}"])
            ]
        );
        // `}` followed by `;` separates.
        assert_eq!(p("a { b };c"), vec![v(&["a", "b"]), v(&["c"])]);
        // A leading brace opens a command block without requiring whitespace.
        assert_eq!(p("x {a} a{"), vec![v(&["x", "a", "a{"])]);
        assert_eq!(p("x {}"), vec![v(&["x", ""])]);
        assert_eq!(p("x { }"), vec![v(&["x", ""])]);
        let menu = p("display-menu Kill X {kill-pane} Split h {splitw -h} Paste p {copy-mode -q; paste-buffer}");
        assert_eq!(menu[0][3], "kill-pane");
        assert_eq!(menu[0][6], "splitw -h");
        assert_eq!(p(&menu[0][9]), vec![v(&["copy-mode", "-q"]), v(&["paste-buffer"])]);
    }

    fn lines(s: &str) -> String {
        match parse_tree(s, &|c: &str| c == "1", &|_: &str| None) {
            Ok(_) => "ok".into(),
            Err((n, e)) => format!("{n}: {e}"),
        }
    }

    fn cmd_lines(s: &str) -> Vec<usize> {
        parse_tree(s, &|c: &str| c == "1", &|_: &str| None).unwrap().cmds.iter().map(|c| c.line).collect()
    }

    #[test]
    fn errors_have_line_numbers() {
        // tmux 3.8 (yacc): errors in the structure are "syntax error" at the line the lexer
        // reached; a string parse has no line, a file's is "N: "
        assert_eq!(lines("a\nbind x {\n split\n"), "4: syntax error");
        assert_eq!(lines("if -F 1 { display a"), "1: syntax error");
        assert_eq!(parse("a\nbind x {\n split\n").unwrap_err(), "syntax error");
        assert_eq!(lines("display a }\n"), "1: syntax error");
        assert_eq!(lines("display a}b\n"), "1: syntax error");
        assert_eq!(lines("%if\n"), "1: syntax error");
        assert_eq!(lines("%if 1\ndisplay a\n"), "3: syntax error");
        assert_eq!(lines("%if 1\n"), "2: syntax error");
        assert_eq!(lines("%if 1\na\n%else\n%else\n%endif"), "4: syntax error");
        assert_eq!(lines("%endif"), "1: syntax error");
        assert_eq!(lines("x\n%else"), "2: syntax error");
        assert_eq!(lines("%bogus"), "1: syntax error");
        assert_eq!(lines("%hidden x"), "1: syntax error");
        assert_eq!(lines("x\nx \"\\uZZ\""), "2: invalid \\u argument");
        assert_eq!(lines("x \"\\9\" \"\\4\""), "1: invalid octal escape");
        // a { cannot start a command; it is an error where it is found
        assert_eq!(lines("{"), "1: syntax error");
        assert_eq!(lines("\n\n{"), "3: syntax error");
        // a line of only ;
        assert_eq!(lines("a\n;\n"), "2: syntax error");
        // two assignments
        assert_eq!(lines("X=1 Y=2"), "1: syntax error");
        // a backslash at the very end
        assert_eq!(lines("x\\"), "1: syntax error");
        // % and digits alone are a word
        assert_eq!(p("%1 %%"), vec![v(&["%1", "%%"])]);
        // any other % word is a directive, also mid-line
        assert_eq!(lines("set -g @a %H:%M"), "1: syntax error");
        assert_eq!(lines("set -g @a 1 ; %if 1"), "1: syntax error");
        assert_eq!(lines("%1;"), "1: syntax error");
        // %if inside one line
        assert_eq!(p("%if 1 a ; b %else c %endif ; d"), vec![v(&["a"]), v(&["b"]), v(&["d"])]);
        assert_eq!(p("x ; %if 0 a %elif 1 b %endif"), vec![v(&["x"]), v(&["b"])]);
        // @x=1 is not an assignment
        assert_eq!(p("@x=1"), vec![v(&["@x=1"])]);
    }

    #[test]
    fn empty_condition_bodies_are_errors() {
        // the grammar needs a statement (a blank line or comment will do) in each part
        assert_eq!(lines("%if 0\n%else\nset -g @a 1\n%endif"), "2: syntax error");
        assert_eq!(lines("%if 1\n%endif\nset -g @a 1"), "2: syntax error");
        assert_eq!(lines("%if 1\nset -g @a 1\n%elif 0\n%endif"), "4: syntax error");
        assert_eq!(lines("set -g @a 1\n%if 1\n%else\n%endif"), "3: syntax error");
        assert_eq!(lines("%if 1\n%endif\n%endif"), "2: syntax error");
        assert_eq!(lines("%if 1\n\n%else\n# c\n%endif"), "ok");
    }

    #[test]
    fn command_lines_are_where_they_end() {
        // cmd->line is the lexer's line when the command is reduced: after a quoted
        // newline or a continuation, the last line; a comment after it counts its newline
        assert_eq!(cmd_lines("set -g @a \"x\ny\" extra\nb"), vec![2, 3]);
        assert_eq!(cmd_lines("set -g @a x \\\n y \\\n z"), vec![3]);
        assert_eq!(cmd_lines("display \"a\n\nb\" c d"), vec![3]);
        assert_eq!(cmd_lines("display a ; nosuch \\\n b"), vec![1, 2]);
        assert_eq!(cmd_lines("display a b c # comment\nx"), vec![2, 2]);
        assert_eq!(cmd_lines("set -g @a 1\nbind x {\n display a\n display b c d\n}\nset -g @b 2"), vec![1, 5, 6]);
        let t = parse_tree("bind x {\n display a\n display b c d\n}", &|_: &str| true, &|_: &str| None).unwrap();
        let Arg::Block(b) = &t.cmds[0].args[2] else { panic!("not a block") };
        assert_eq!(b.iter().map(|c| c.line).collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn quotes_may_span_lines() {
        assert_eq!(p("x 'a\nb'\ny"), vec![v(&["x", "a\nb"]), v(&["y"])]);
        // a quote left open at the end of the input ends there
        assert_eq!(p("x 'a\nb\nc"), vec![v(&["x", "a\nb\nc"])]);
        assert_eq!(p("display \"abc\n"), vec![v(&["display", "abc\n"])]);
        // the indentation after a newline in quotes goes, and so do comment lines
        assert_eq!(p("display \"a\n   # comment\n   b\" '1\n  #{x}'"), vec![v(&["display", "a\n\nb", "1\n#{x}"])]);
        // backslash-newline continues a line inside single quotes too
        assert_eq!(p("x 'a\\\nb' 'c\\\\\nd'"), vec![v(&["x", "ab", "c\\\\\nd"])]);
    }

    #[test]
    fn conditionals_default() {
        let src = "\
%if 1
a
%elif 1
b
%else
c
%endif
%if 0
d
%elif yes
e
%else
f
%endif
%if ''
g
%else
h
%endif
%if 0
  %if 1
  i
  %else
  j
  %endif
%endif
";
        assert_eq!(p(src), vec![v(&["a"]), v(&["e"]), v(&["h"])]);
    }

    #[test]
    fn conditionals_custom() {
        let seen = std::cell::RefCell::new(Vec::new());
        let cond = |c: &str| {
            seen.borrow_mut().push(c.to_string());
            c == "#{==:#{host},box}"
        };
        let src = "%if \"#{==:#{host},other}\"\nx\n%elif #{==:#{host},box}\ny\n%endif\n";
        assert_eq!(parse_with(src, &cond).unwrap(), vec![v(&["y"])]);
        assert_eq!(
            *seen.borrow(),
            vec![
                "#{==:#{host},other}".to_string(),
                "#{==:#{host},box}".to_string()
            ]
        );
        // if_open evaluates every condition, also inside false branches
        seen.borrow_mut().clear();
        let src = "%if nope\n%if inner\nz\n%endif\n%endif";
        assert!(parse_with(src, &cond).unwrap().is_empty());
        assert_eq!(*seen.borrow(), vec!["nope".to_string(), "inner".to_string()]);
    }

    #[test]
    fn env_assignments() {
        assert_eq!(
            p("FOO=bar\nBAZ=\"a b\"\n%hidden SECRET=x\nE=\nset x=y\n'Q=1'\n1A=2"),
            vec![
                v(&["set-environment", "-g", "FOO", "bar"]),
                v(&["set-environment", "-g", "BAZ", "a b"]),
                v(&["set-environment", "-gh", "SECRET", "x"]),
                v(&["set-environment", "-g", "E", ""]),
                // the token is classified after its quotes are gone
                v(&["set-environment", "-g", "Q", "1"]),
                v(&["set", "x=y"]),
                v(&["1A=2"]),
            ]
        );
        assert!(parse("%hidden notassign").is_err());
        assert!(p("%if 0\n%hidden A=1\nB=2\n%endif").is_empty());
    }

    #[test]
    fn realistic_config() {
        let conf = r##"
# ~/.tmux.conf
set -g prefix C-a
unbind C-b
bind C-a send-prefix

bind | split-window -h -c "#{pane_current_path}"
bind - split-window -v -c #{pane_current_path}
bind -n M-Left select-pane -L   # no prefix
set -g status-right '#[fg=yellow]%H:%M'
set -g mouse on; set -g base-index 1

bind r source-file ~/.tmux.conf \; display "Reloaded!"
if-shell 'test -n "$SSH_CLIENT"' {
    set -g status-bg red
    if-shell "true" { display 'nested {ok}' }
} {
    set -g status-bg green
}
"##;
        let home = home_dir();
        assert_eq!(
            p(conf),
            vec![
                v(&["set", "-g", "prefix", "C-a"]),
                v(&["unbind", "C-b"]),
                v(&["bind", "C-a", "send-prefix"]),
                v(&[
                    "bind",
                    "|",
                    "split-window",
                    "-h",
                    "-c",
                    "#{pane_current_path}"
                ]),
                v(&["bind", "-", "split-window", "-v", "-c"]),
                v(&["bind", "-n", "M-Left", "select-pane", "-L"]),
                v(&["set", "-g", "status-right", "#[fg=yellow]%H:%M"]),
                v(&["set", "-g", "mouse", "on"]),
                v(&["set", "-g", "base-index", "1"]),
                v(&[
                    "bind",
                    "r",
                    "source-file",
                    &format!("{home}/.tmux.conf"),
                    ";",
                    "display",
                    "Reloaded!"
                ]),
                v(&[
                    "if-shell",
                    "test -n \"$SSH_CLIENT\"",
                    "set -g status-bg red ; if-shell true { display 'nested {ok}' }",
                    "set -g status-bg green"
                ]),
            ]
        );
        let inner = p("set -g status-bg red ; if-shell true { display 'nested {ok}' }");
        assert_eq!(
            inner,
            vec![
                v(&["set", "-g", "status-bg", "red"]),
                v(&["if-shell", "true", "display 'nested {ok}'"]),
            ]
        );
    }

    #[test]
    fn split_commands_rules() {
        let a = v(&[
            "new",
            "-d",
            ";",
            "split-window",
            "-h",
            ";",
            ";",
            "select-pane",
            "-L",
            ";",
        ]);
        assert_eq!(
            split_commands(&a),
            vec![
                v(&["new", "-d"]),
                v(&["split-window", "-h"]),
                v(&["select-pane", "-L"])
            ]
        );
        // "x;" ends a command; a trailing "\;" is a literal ";" (cmd_parse_from_arguments)
        let a = v(&["new", "-d;", "split", "\\;", "x"]);
        assert_eq!(
            split_commands(&a),
            vec![v(&["new", "-d"]), v(&["split", ";", "x"])]
        );
        let a = v(&["bind", "-T", "copy-mode-vi", "K", "send", "-X", "cursor-up", "\\;", "send", "-X", "cursor-up"]);
        let cmds = split_commands(&a);
        assert_eq!(cmds.len(), 1);
        assert_eq!(split_arguments(&cmds[0][4..]), vec![v(&["send", "-X", "cursor-up"]), v(&["send", "-X", "cursor-up"])]);
        assert!(split_commands(&[]).is_empty());
        // Round trip with bind-key style stored command.
        let bound = &p(r"bind x split-window \; select-pane -L")[0][2..];
        assert_eq!(
            split_commands(bound),
            vec![v(&["split-window"]), v(&["select-pane", "-L"])]
        );
    }

    #[test]
    fn split_arguments_like_cmd_parse_from_arguments() {
        let a = |x: &[&str]| split_arguments(&v(x));
        assert_eq!(a(&["a", "1", ";", "b"]), vec![v(&["a", "1"]), v(&["b"])]);
        assert_eq!(a(&["a", "1;", "b"]), vec![v(&["a", "1"]), v(&["b"])]);
        // a trailing \\; is a literal ";"
        assert_eq!(a(&["send", "-l", "\\;"]), vec![v(&["send", "-l", ";"])]);
        assert_eq!(a(&["send", "-l", ";"]), vec![v(&["send", "-l"])]);
        assert_eq!(a(&["send", ";x"]), vec![v(&["send", ";x"])]);
        // blocks are kept as they are
        let blk = format!("{BLOCK_MARK}x;");
        assert_eq!(a(&["if", "1", &blk]), vec![v(&["if", "1", &blk])]);
    }

    #[test]
    fn quote_round_trip() {
        let cases = [
            "plain",
            "",
            "a b",
            "it's",
            "say \"hi\"",
            "back\\slash",
            "$HOME",
            "${X}",
            ";",
            "a;",
            "{",
            "}",
            "{ x }",
            "#comment",
            "#{pane_id}",
            "#[fg=red]%H:%M",
            "~",
            "~/x",
            "%if",
            "A=b",
            "tab\there",
            "nl\nhere",
            "esc\x1b[0m",
            "bell\x07",
            "del\x7f",
            "c1\u{85}",
            "héllo 世界",
            "it's \"both\" $x\n",
            "C:\\Users\\me",
            "trailing\\",
        ];
        for c in cases {
            let q = quote(c);
            let line = format!("cmd {q}");
            assert_eq!(p(&line), vec![v(&["cmd", c])], "quote({c:?}) = {q}");
            // Also as the first word of a command.
            // NAME=value as the first word is always an assignment
            if !looks_like_assignment(c) {
                let got = p(&q);
                assert_eq!(got, vec![v(&[c])], "first-word quote({c:?}) = {q}");
            }
        }
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote("-g"), "-g");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote(""), "''");
    }
}
