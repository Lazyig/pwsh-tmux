//! tmux command / configuration-file language parser.
//!
//! Turns a config file or command string into a list of commands, each an
//! argv (`Vec<String>`). Supports quoting, escapes, environment expansion,
//! `~` expansion, comments, line continuation, `;` separators, `{ ... }`
//! command blocks, `%if`/`%elif`/`%else`/`%endif`, `%hidden` and
//! `NAME=value` environment assignments.

/// Parse a config file or command string into a list of commands (each an argv).
/// Commands are separated by newlines or by a standalone unquoted `;`.
///
/// `%if` conditions are treated as true when non-empty and not "0".
pub fn parse(input: &str) -> Result<Vec<Vec<String>>, String> {
    parse_with(input, &|c: &str| !c.is_empty() && c != "0")
}

/// Like [`parse`], but `%if` / `%elif` conditions are evaluated by `cond`
/// (which receives the condition text, e.g. an unexpanded format).
pub fn parse_with(input: &str, cond: &dyn Fn(&str) -> bool) -> Result<Vec<Vec<String>>, String> {
    parse_with_env(input, cond, &|name: &str| std::env::var(name).ok())
}

/// Like [`parse_with`], but `$NAME` / `${NAME}` are looked up with `env` (the server's
/// global environment) instead of the process environment. As in tmux, a `NAME=value`
/// or `%hidden NAME=value` line takes effect for the rest of the parse at once, so a
/// later `${NAME}` in the same file sees it (the `set-environment` commands are still
/// returned so the server's environment ends up the same).
pub fn parse_with_env(input: &str, cond: &dyn Fn(&str) -> bool, env: &dyn Fn(&str) -> Option<String>) -> Result<Vec<Vec<String>>, String> {
    parse_with_env_lines(input, cond, env).map(|(cmds, _)| cmds).map_err(bare)
}

/// A parse error without its line number: what cmd_parse_from_string reports (there is
/// no file, so cmd_parse_get_error adds nothing).
fn bare(e: String) -> String {
    match e.split_once(": ") {
        Some((n, m)) if n.parse::<usize>().is_ok() => m.to_string(),
        _ => e,
    }
}

/// Like [`parse_with_env`], also giving the line each command starts on. An error is
/// "N: message" (the line the parser had reached), so a file's errors read "file:N: ...".
pub fn parse_with_env_lines(input: &str, cond: &dyn Fn(&str) -> bool, env: &dyn Fn(&str) -> Option<String>) -> Result<(Vec<Vec<String>>, Vec<usize>), String> {
    let normalized = input.replace("\r\n", "\n");
    let mut lx = Lexer {
        s: normalized.chars().collect(),
        pos: 0,
        line: 1,
        env,
        assigned: std::collections::HashMap::new(),
        braces: Vec::new(),
        lines: Vec::new(),
    };
    let mut cmds = lx.run(cond)?;
    // bind-key keeps its { } blocks as blocks (list-keys prints them so): they reach
    // it with a mark in front. Any unique abbreviation ("bi", "bind-k", ...) is bind-key.
    for (c, w) in std::mem::take(&mut lx.braces) {
        let name = cmds[c][0].as_str();
        if name.len() >= 2 && "bind-key".starts_with(name) {
            cmds[c][w].insert_str(0, BLOCK_MARK);
        }
    }
    Ok((cmds, lx.lines))
}

/// Put in front of a `{ ... }` block argument of bind-key by [`parse_with_env`]
/// (never typed: it starts with NUL).
pub const BLOCK_MARK: &str = "\0{";

/// Like [`parse`], but each argument also says whether it was a `{ ... }`
/// block (its text is then the block's body), as tmux keeps command blocks
/// apart from strings.
pub fn parse_blocks(input: &str) -> Result<Vec<Vec<(String, bool)>>, String> {
    let normalized = input.replace("\r\n", "\n");
    let env = |name: &str| std::env::var(name).ok();
    let mut lx = Lexer {
        s: normalized.chars().collect(),
        pos: 0,
        line: 1,
        env: &env,
        assigned: std::collections::HashMap::new(),
        braces: Vec::new(),
        lines: Vec::new(),
    };
    let cmds = lx.run(&|c: &str| !c.is_empty() && c != "0").map_err(bare)?;
    let mut out: Vec<Vec<(String, bool)>> = cmds.into_iter().map(|c| c.into_iter().map(|w| (w, false)).collect()).collect();
    for (c, w) in lx.braces {
        out[c][w].1 = true;
    }
    Ok(out)
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
    for c in arg.chars() {
        match c {
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
    String::new()
}

/// One lexed word.
struct Word {
    text: String,
    /// True if anything (including an empty quoted string) was produced.
    present: bool,
    /// True if the word was terminated by an unescaped `;`.
    ends_cmd: bool,
    /// True if the word starts with an unquoted `NAME=`.
    assign: bool,
}

/// Conditional (`%if`) stack frame.
struct Frame {
    parent_active: bool,
    taken: bool,
    active: bool,
    seen_else: bool,
}

struct Lexer<'a> {
    s: Vec<char>,
    pos: usize,
    line: usize,
    /// Variable lookup for `$NAME` (the server's global environment).
    env: &'a dyn Fn(&str) -> Option<String>,
    /// Assignments made earlier in this parse; they shadow `env`.
    assigned: std::collections::HashMap<String, String>,
    /// (command, argument) positions of `{ ... }` blocks.
    braces: Vec<(usize, usize)>,
    /// The line each command starts on.
    lines: Vec<usize>,
}

fn is_blank(c: Option<char>) -> bool {
    matches!(c, None | Some(' ') | Some('\t') | Some('\n'))
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.s.get(self.pos + n).copied()
    }

    fn err(&self, line: usize, msg: &str) -> String {
        format!("{line}: {msg}")
    }

    /// yacc's "syntax error" at the line the lexer has reached.
    fn syntax(&self) -> String {
        self.err(self.line, "syntax error")
    }

    /// Skip spaces, tabs and backslash-newline continuations.
    fn skip_blanks(&mut self) {
        loop {
            match self.peek() {
                Some(' ') | Some('\t') => self.pos += 1,
                Some('\\') if self.peek_at(1) == Some('\n') => {
                    self.pos += 2;
                    self.line += 1;
                }
                _ => break,
            }
        }
    }

    fn skip_comment(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            self.pos += 1;
        }
    }

    fn run(&mut self, cond: &dyn Fn(&str) -> bool) -> Result<Vec<Vec<String>>, String> {
        let mut cmds: Vec<Vec<String>> = Vec::new();
        let mut cur: Vec<(String, bool, bool)> = Vec::new();
        let mut stack: Vec<Frame> = Vec::new();
        let mut line_start = true;
        let mut start = self.line;

        loop {
            while self.lines.len() < cmds.len() {
                self.lines.push(start);
            }
            self.skip_blanks();
            let Some(c) = self.peek() else { break };
            if cur.is_empty() {
                start = self.line;
            }
            let active = stack.last().is_none_or(|f| f.active);
            match c {
                '\n' => {
                    finish(&mut cmds, &mut cur, active, &mut self.assigned, &mut self.braces);
                    self.pos += 1;
                    self.line += 1;
                    line_start = true;
                    continue;
                }
                '#' if self.peek_at(1) != Some('{') => {
                    self.skip_comment();
                    continue;
                }
                // a word of only % and digits is a token, not a condition
                '%' if line_start && cur.is_empty() && !self.percent_token() => {
                    self.directive(&mut stack, &mut cmds, cond)?;
                    continue;
                }
                // a } that closes no block
                '}' => return Err(self.syntax()),
                '{' => {
                    let body = self.read_brace()?;
                    cur.push((body, false, true));
                    // `};` ends the command even without a following blank.
                    if self.peek() == Some(';') {
                        self.pos += 1;
                        finish(&mut cmds, &mut cur, active, &mut self.assigned, &mut self.braces);
                    }
                }
                _ => {
                    let w = self.read_word()?;
                    if w.present {
                        cur.push((w.text, w.assign, false));
                    }
                    if w.ends_cmd {
                        finish(&mut cmds, &mut cur, active, &mut self.assigned, &mut self.braces);
                    }
                }
            }
            line_start = false;
        }
        let active = stack.last().is_none_or(|f| f.active);
        finish(&mut cmds, &mut cur, active, &mut self.assigned, &mut self.braces);
        while self.lines.len() < cmds.len() {
            self.lines.push(start);
        }
        // %if without %endif
        if !stack.is_empty() {
            return Err(self.syntax());
        }
        Ok(cmds)
    }

    /// Whether the word at `%` is only % signs and digits (yylex returns a TOKEN for it).
    fn percent_token(&self) -> bool {
        self.s[self.pos..].iter().take_while(|c| !matches!(c, ' ' | '\t' | '\n')).all(|c| *c == '%' || c.is_ascii_digit())
    }

    /// Handle a `%directive` line. Consumes up to (not including) the newline.
    fn directive(
        &mut self,
        stack: &mut Vec<Frame>,
        cmds: &mut Vec<Vec<String>>,
        cond: &dyn Fn(&str) -> bool,
    ) -> Result<(), String> {
        let line = self.line;
        self.pos += 1; // '%'
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if !c.is_ascii_alphabetic() {
                break;
            }
            name.push(c);
            self.pos += 1;
        }
        // Rest of the line as words.
        let mut words: Vec<(String, bool)> = Vec::new();
        loop {
            self.skip_blanks();
            match self.peek() {
                None | Some('\n') => break,
                Some('#') if self.peek_at(1) != Some('{') => {
                    self.skip_comment();
                    break;
                }
                _ => {
                    let w = self.read_word()?;
                    if w.present {
                        words.push((w.text, w.assign));
                    }
                }
            }
        }
        let text = words
            .iter()
            .map(|(w, _)| w.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let active = stack.last().is_none_or(|f| f.active);
        // the grammar: "%if"/"%elif" take one condition, "%else"/"%endif" nothing
        let argc = match name.as_str() { "if" | "elif" | "hidden" => 1, _ => 0 };
        if words.len() != argc {
            return Err(self.err(line, "syntax error"));
        }
        match name.as_str() {
            "if" => {
                let v = active && cond(&text);
                stack.push(Frame {
                    parent_active: active,
                    taken: v,
                    active: v,
                    seen_else: false,
                });
            }
            "elif" => {
                let f = stack
                    .last_mut()
                    .ok_or_else(|| self.err(line, "syntax error"))?;
                if f.seen_else {
                    return Err(self.err(line, "syntax error"));
                }
                if f.parent_active && !f.taken && cond(&text) {
                    f.active = true;
                    f.taken = true;
                } else {
                    f.active = false;
                }
            }
            "else" => {
                let f = stack
                    .last_mut()
                    .ok_or_else(|| self.err(line, "syntax error"))?;
                if f.seen_else {
                    return Err(self.err(line, "syntax error"));
                }
                f.seen_else = true;
                f.active = f.parent_active && !f.taken;
                f.taken = true;
            }
            "endif" => {
                if stack.pop().is_none() {
                    return Err(self.err(line, "syntax error"));
                }
            }
            "hidden" => {
                if words.len() != 1 || !words[0].1 {
                    return Err(self.err(line, "syntax error"));
                }
                if active {
                    let (n, v) = words[0].0.split_once('=').unwrap();
                    self.assigned.insert(n.to_string(), v.to_string());
                    cmds.push(vec![
                        "set-environment".into(),
                        "-gh".into(),
                        n.into(),
                        v.into(),
                    ]);
                }
            }
            _ => return Err(self.err(line, "syntax error")),
        }
        Ok(())
    }

    /// Read a `{ ... }` block starting at `{`. Returns the trimmed inner text.
    fn read_brace(&mut self) -> Result<String, String> {
        self.pos += 1; // '{'
        let mut depth = 1usize;
        let mut buf = String::new();
        loop {
            // the end of the input inside a block (or a quote in it)
            let Some(c) = self.peek() else {
                return Err(self.syntax());
            };
            self.pos += 1;
            match c {
                '\\' => {
                    buf.push(c);
                    if let Some(n) = self.peek() {
                        self.pos += 1;
                        if n == '\n' {
                            self.line += 1;
                        }
                        buf.push(n);
                    }
                }
                '\'' | '"' => {
                    buf.push(c);
                    loop {
                        let Some(q) = self.peek() else {
                            return Err(self.syntax());
                        };
                        self.pos += 1;
                        buf.push(q);
                        if q == '\n' {
                            self.line += 1;
                        }
                        if q == c {
                            break;
                        }
                        if c == '"'
                            && q == '\\'
                            && let Some(n) = self.peek()
                        {
                            self.pos += 1;
                            if n == '\n' {
                                self.line += 1;
                            }
                            buf.push(n);
                        }
                    }
                }
                '#' if self.peek() != Some('{')
                    && buf
                        .chars()
                        .last()
                        .is_none_or(|p| p == ' ' || p == '\t' || p == '\n' || p == ';') =>
                {
                    // Comment: keep it out of the block text.
                    self.skip_comment();
                }
                '{' => {
                    depth += 1;
                    buf.push(c);
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    buf.push(c);
                }
                '\n' => {
                    self.line += 1;
                    buf.push(c);
                }
                _ => buf.push(c),
            }
        }
        // Strip trailing whitespace from each line (comments removed leave some).
        let text = buf
            .split('\n')
            .map(|l| l.trim_end())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(text.trim().to_string())
    }

    fn lookup(&self, name: &str) -> String {
        self.assigned.get(name).cloned().or_else(|| (self.env)(name)).unwrap_or_default()
    }

    /// Expand `$NAME` / `${NAME}` at `$` (yylex_token_variable). A `$` not followed by a
    /// name is literal; `${` must be a name closed by `}`.
    fn expand_var(&mut self, out: &mut String) -> Result<(), String> {
        self.pos += 1; // '$'
        match self.peek() {
            None => return Err(self.syntax()),
            Some('{') => {
                self.pos += 1;
                let mut name = String::new();
                loop {
                    match self.peek() {
                        Some('}') => break,
                        Some(c) if is_name_char(c) => name.push(c),
                        _ => return Err(self.err(self.line, "invalid environment variable")),
                    }
                    self.pos += 1;
                }
                self.pos += 1;
                out.push_str(&self.lookup(&name));
            }
            // A session ID ($N) is literal text, including in expanded monitor hooks.
            Some(c) if is_name_char(c) && !c.is_ascii_digit() => {
                let mut name = String::new();
                while let Some(c) = self.peek().filter(|c| is_name_char(*c)) {
                    name.push(c);
                    self.pos += 1;
                }
                out.push_str(&self.lookup(&name));
            }
            _ => out.push('$'),
        }
        Ok(())
    }

    /// Process an escape outside single quotes (yylex_token_escape); `pos` is just after
    /// the `\`.
    fn escape(&mut self, out: &mut String) -> Result<(), String> {
        let Some(c) = self.peek() else {
            return Err(self.syntax());
        };
        self.pos += 1;
        match c {
            '\n' => self.line += 1, // continuation
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'e' => out.push('\x1b'),
            'f' => out.push('\x0c'),
            's' => out.push(' '),
            'v' => out.push('\x0b'),
            'r' => out.push('\r'),
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'u' | 'U' => {
                let n = if c == 'u' { 4 } else { 8 };
                let digits: String = (0..n).filter_map(|i| self.peek_at(i)).collect();
                let cp = (digits.len() == n)
                    .then(|| u32::from_str_radix(&digits, 16).ok())
                    .flatten()
                    .and_then(char::from_u32)
                    .ok_or_else(|| self.err(self.line, &format!("invalid \\{c} argument")))?;
                self.pos += n;
                out.push(cp);
            }
            // three octal digits, the first at most 3
            '0'..='3' => match (self.peek(), self.peek_at(1)) {
                (Some(a @ '0'..='7'), Some(b @ '0'..='7')) => {
                    let v = (c as u32 - 48) * 64 + (a as u32 - 48) * 8 + (b as u32 - 48);
                    self.pos += 2;
                    out.push(char::from_u32(v).unwrap());
                }
                _ => return Err(self.err(self.line, "invalid octal escape")),
            },
            '4'..='7' => return Err(self.err(self.line, "invalid octal escape")),
            other => out.push(other),
        }
        Ok(())
    }

    /// After a newline inside quotes tmux drops the next line's indentation, and a
    /// comment line there (# not followed by one of ,#{}:) is dropped up to its newline.
    fn skip_quoted_indent(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.pos += 1;
        }
        if self.peek() == Some('#') && !self.peek_at(1).is_some_and(|c| ",#{}:".contains(c)) {
            self.skip_comment();
        }
    }

    /// Read one word starting at the current (non-blank) position. Quotes left open at
    /// the end of the input end there (yylex_token stops at EOF).
    fn read_word(&mut self) -> Result<Word, String> {
        let mut w = Word {
            text: String::new(),
            present: false,
            ends_cmd: false,
            assign: false,
        };
        let mut name_ok = true; // all chars so far are unquoted name chars
        let mut assign_decided = false;
        let mut first = true;
        while let Some(c) = self.peek() {
            let is_first = first;
            first = false;
            match c {
                ' ' | '\t' | '\n' => break,
                // } ends a word (and then closes a block or is an error)
                '}' => break,
                '\\' => {
                    self.pos += 1;
                    self.escape(&mut w.text)?;
                    w.present = true;
                    name_ok = false;
                }
                '\'' => {
                    self.pos += 1;
                    while let Some(q) = self.peek() {
                        self.pos += 1;
                        match q {
                            '\'' => break,
                            // backslash-newline is a continuation even here (yylex_getc)
                            '\\' => {
                                let run = 1 + self.s[self.pos..].iter().take_while(|c| **c == '\\').count();
                                self.pos += run - 1;
                                let cont = run % 2 == 1 && self.peek() == Some('\n');
                                w.text.extend(std::iter::repeat_n('\\', run - cont as usize));
                                if cont {
                                    self.pos += 1;
                                    self.line += 1;
                                }
                            }
                            '\n' => {
                                self.line += 1;
                                w.text.push(q);
                                self.skip_quoted_indent();
                            }
                            _ => w.text.push(q),
                        }
                    }
                    w.present = true;
                    name_ok = false;
                }
                '"' => {
                    self.pos += 1;
                    while let Some(q) = self.peek() {
                        self.pos += 1;
                        match q {
                            '"' => break,
                            '\\' => self.escape(&mut w.text)?,
                            '$' => {
                                self.pos -= 1;
                                self.expand_var(&mut w.text)?;
                            }
                            '\n' => {
                                self.line += 1;
                                w.text.push(q);
                                self.skip_quoted_indent();
                            }
                            _ => w.text.push(q),
                        }
                    }
                    w.present = true;
                    name_ok = false;
                }
                '$' => {
                    self.expand_var(&mut w.text)?;
                    w.present = true;
                    name_ok = false;
                }
                // an unquoted ; always ends the word and the command
                ';' => {
                    self.pos += 1;
                    w.ends_cmd = true;
                    break;
                }
                '~' if is_first
                    && (is_blank(self.peek_at(1))
                        || self.peek_at(1) == Some('/')
                        || self.peek_at(1) == Some(';')) =>
                {
                    self.pos += 1;
                    w.text.push_str(&home_dir());
                    w.present = true;
                    name_ok = false;
                }
                '#' if is_first && self.peek_at(1) == Some('{') => {
                    // Unquoted format: `#{...}` may contain spaces; match braces.
                    let fline = self.line;
                    self.pos += 2;
                    w.text.push_str("#{");
                    let mut depth = 1usize;
                    while depth > 0 {
                        let Some(f) = self.peek() else {
                            return Err(self.err(fline, "syntax error"));
                        };
                        self.pos += 1;
                        match f {
                            '{' => depth += 1,
                            '}' => depth -= 1,
                            '\n' => return Err(self.err(fline, "syntax error")),
                            _ => {}
                        }
                        w.text.push(f);
                    }
                    w.present = true;
                    name_ok = false;
                }
                '=' => {
                    if !assign_decided {
                        assign_decided = true;
                        w.assign = name_ok
                            && !w.text.is_empty()
                            && !w.text.starts_with(|d: char| d.is_ascii_digit());
                    }
                    self.pos += 1;
                    w.text.push(c);
                    w.present = true;
                }
                _ => {
                    if !is_name_char(c) {
                        name_ok = false;
                    }
                    self.pos += 1;
                    w.text.push(c);
                    w.present = true;
                }
            }
        }
        Ok(w)
    }
}

/// Finish the current command: push it (if active and non-empty), mapping a
/// lone `NAME=value` to `set-environment -g NAME value`.
fn finish(cmds: &mut Vec<Vec<String>>, cur: &mut Vec<(String, bool, bool)>, active: bool, assigned: &mut std::collections::HashMap<String, String>, braces: &mut Vec<(usize, usize)>) {
    let words = std::mem::take(cur);
    if !active || words.is_empty() {
        return;
    }
    if words.len() == 1 && words[0].1 {
        let (n, v) = words[0].0.split_once('=').unwrap();
        assigned.insert(n.to_string(), v.to_string());
        cmds.push(vec![
            "set-environment".into(),
            "-g".into(),
            n.into(),
            v.into(),
        ]);
        return;
    }
    braces.extend(words.iter().enumerate().filter(|(_, w)| w.2).map(|(i, _)| (cmds.len(), i)));
    cmds.push(words.into_iter().map(|(w, _, _)| w).collect());
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
    fn unquoted_format_is_not_comment() {
        assert_eq!(
            p("display -p #{pane_current_path}"),
            vec![v(&["display", "-p", "#{pane_current_path}"])]
        );
        assert_eq!(
            p("display -p #{?pane_active,a b,c}x more"),
            vec![v(&["display", "-p", "#{?pane_active,a b,c}x", "more"])]
        );
        assert_eq!(p("foo #[fg=red] bar"), vec![v(&["foo"])]);
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
                v(&["set", "-g", "@ctp_session_icon", "x"]),
                v(&["set-environment", "-g", "A", "1"]),
                v(&["set", "@a", "1"]),
                v(&["set-environment", "-g", "B", "2"]),
                v(&["set", "@b", "2from-server"]),
            ]
        );
        // a later assignment replaces an earlier one; inactive branches assign nothing
        assert_eq!(pe("A=1\nA=2\nset @a $A")[2], v(&["set", "@a", "2"]));
        assert_eq!(pe("%if 0\n%hidden C=no\nD=no\n%endif\nset @c [$C$D]"), vec![v(&["set", "@c", "[]"])]);
        // the lookup is the server's environment, not the process's
        unsafe { std::env::set_var("CMDPARSE_PROC_ONLY", "proc") };
        assert_eq!(pe("set @p [$CMDPARSE_PROC_ONLY]"), vec![v(&["set", "@p", "[]"])]);
    }

    #[test]
    fn tilde_expansion() {
        let home = home_dir();
        assert_eq!(
            p("cd ~ ~/x a~ '~' ~user \"~\""),
            vec![v(&[
                "cd",
                &home,
                &format!("{home}/x"),
                "a~",
                "~",
                "~user",
                "~",
            ])]
        );
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
            vec![v(&["bind", "x", "\0{split-window; select-pane -L"])]
        );
        // only bind-key's blocks are marked
        assert_eq!(p("unbind x { y }"), vec![v(&["unbind", "x", "y"])]);
        let cmds = p("bind x {\n  split-window -h # comment }\n  select-pane -L\n}\nnext");
        assert_eq!(
            cmds,
            vec![
                v(&["bind", "x", "\0{split-window -h\n  select-pane -L"]),
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
                "if -F 1 { display '}' } ; display \"#{pane_id}\"",
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

    #[test]
    fn errors_have_line_numbers() {
        // tmux 3.8 (yacc): errors in the structure are "syntax error" at the line the lexer
        // reached; a string parse has no line, a file's is "N: "
        let lines = |s: &str| parse_with_env_lines(s, &|c: &str| c == "1", &|_: &str| None).unwrap_err();
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
        assert!(parse("x #{abc").is_err());
        // % and digits alone are a word
        assert_eq!(p("%1 %%"), vec![v(&["%1", "%%"])]);
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
        // Conditions inside inactive branches are not evaluated.
        seen.borrow_mut().clear();
        let src = "%if nope\n%if inner\nz\n%endif\n%endif";
        assert!(parse_with(src, &cond).unwrap().is_empty());
        assert_eq!(*seen.borrow(), vec!["nope".to_string()]);
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
                v(&["set", "x=y"]),
                v(&["Q=1"]),
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
                v(&[
                    "bind",
                    "-",
                    "split-window",
                    "-v",
                    "-c",
                    "#{pane_current_path}"
                ]),
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
                    "set -g status-bg red\n    if-shell \"true\" { display 'nested {ok}' }",
                    "set -g status-bg green"
                ]),
            ]
        );
        let inner = p("set -g status-bg red\n    if-shell \"true\" { display 'nested {ok}' }");
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
            let got = p(&q);
            assert_eq!(got, vec![v(&[c])], "first-word quote({c:?}) = {q}");
        }
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote("-g"), "-g");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote(""), "''");
    }
}
