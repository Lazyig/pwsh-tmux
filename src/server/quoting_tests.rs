//! Prompt answers put into templates (cmd_template_replace): the escapes chosen for the
//! shells that may run them. The Windows choices are pure functions of the shells' names,
//! so they run here on every platform, checked against models of how PowerShell, POSIX
//! sh, cmd.exe and the command parser read the result (and, on Unix, against a real sh).
use crate::cmdparse;
use super::input::{fill_block, fill_block_value, fill_template, Quoting, Reader};
use super::procinfo::{shell_kind, ShellKind};

const TMUX: Quoting = Quoting::TMUX;
const fn shells(powershell: bool, cmd: bool, posix: bool) -> Quoting {
    Quoting { powershell, cmd, posix, tmux: false }
}
/// POSIX shells only (on Windows, say Git Bash as PWSH_TMUX_SHELL and default-shell).
const SH: Quoting = shells(false, false, true);
const PS: Quoting = shells(true, false, false);
const CMD: Quoting = shells(false, true, false);
const PS_CMD: Quoting = shells(true, true, false);
/// PowerShell and a POSIX shell (jobs under PowerShell, default-shell Git Bash), or a
/// shell whose quoting is not known.
const PS_SH: Quoting = shells(true, false, true);
const CMD_SH: Quoting = shells(false, true, true);
const PS_SH_CMD: Quoting = shells(true, true, true);
/// Every set of shells Windows may have.
const WINDOWS: [Quoting; 7] = [SH, PS, CMD, PS_CMD, PS_SH, CMD_SH, PS_SH_CMD];

/// Answers that try to leave their quotes in each shell: quotes of every kind PowerShell
/// reads (' ‘ ’ ‚ ‛ and " “ ” „), backslashes before quotes, command substitution,
/// variables, cmd.exe's operators and variables, and line ends.
const ADVERSARIAL: &[&str] = &[
    "x'; echo pwned; '",
    "it's",
    "'",
    "''",
    "\\'",
    "a\\",
    "\\",
    "`whoami`",
    "$(whoami)",
    "${HOME}",
    "$env:USERPROFILE",
    "$null",
    "%PATH%",
    "%%",
    "!PATH!",
    "a & b | c ; d < e > f ^ g ( h )",
    "\"; echo pwned; \"",
    "x\" & echo pwned & \"",
    "line1\nline2; echo pwned",
    "a\r\nb",
    "\u{2018}x\u{2019}; echo pwned; \u{2018}",
    "\u{201a}\u{201b}",
    "\u{201c}q\u{201d}\u{201e}; echo pwned",
    "`$(echo pwned)`'\"\\$x",
    "C:\\Users\\x y\\",
];

/// The adversarial answers, every ASCII punctuation character, the Unicode quotes
/// (U+2018 to U+201F: PowerShell reads all but U+201F as quotes) and line ends, each
/// alone, between letters, doubled, next to a backslash and before what would expand
/// after it, and each pair of the characters special to some shell.
fn answers() -> Vec<String> {
    let mut v: Vec<String> = ADVERSARIAL.iter().map(|s| s.to_string()).collect();
    let punct = (0x21u8..0x7f).map(char::from).filter(|c| c.is_ascii_punctuation());
    let unicode = '\u{2018}'..='\u{201f}';
    for c in punct.chain(unicode).chain(['\n', '\r', '\t', ' ']) {
        v.extend([
            c.to_string(),
            format!("a{c}b"),
            format!("{c}{c}"),
            format!("\\{c}"),
            format!("{c}\\"),
            format!("{c}x{c}"),
            format!("{c}{{HOME}}"),
            format!("{c}(echo pwned)"),
            format!("{c}PATH{c}"),
        ]);
    }
    let special = ['\'', '"', '$', '`', '\\', '%', '!', '^', '&', '|', '(', ';', '~', '{', '#', '\u{2018}', '\u{201c}', '\n', '\r', ' '];
    for a in special {
        for b in special {
            v.extend([format!("{a}{b}"), format!("x{a}{b}y")]);
        }
    }
    v.extend(["", "C:\\Users\\x", "\\\\server\\share\\", "$env:PATH", "${env:PATH}", "$$", "$?", "$_", "`n", "a\\\r\nb"].map(String::from));
    v.sort();
    v.dedup();
    v
}

#[test]
fn shell_kinds_from_names_and_paths() {
    for (shell, kind) in [
        ("pwsh", ShellKind::PowerShell),
        ("pwsh.exe", ShellKind::PowerShell),
        ("C:\\Program Files\\PowerShell\\7\\pwsh.exe", ShellKind::PowerShell),
        ("PowerShell.EXE", ShellKind::PowerShell),
        ("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", ShellKind::PowerShell),
        ("/usr/bin/pwsh", ShellKind::PowerShell),
        ("cmd", ShellKind::Cmd),
        ("CMD.EXE", ShellKind::Cmd),
        ("C:\\Windows\\System32\\cmd.exe", ShellKind::Cmd),
        ("/bin/sh", ShellKind::Posix),
        ("bash.exe", ShellKind::Posix),
        ("C:\\Program Files\\Git\\bin\\bash.exe", ShellKind::Posix),
        ("C:/msys64/usr/bin/zsh.exe", ShellKind::Posix),
        ("/usr/bin/dash", ShellKind::Posix),
        ("fish", ShellKind::Other),
        ("nu.exe", ShellKind::Other),
        ("wsl.exe", ShellKind::Other),
        ("", ShellKind::Other),
    ] {
        assert_eq!(shell_kind(shell), kind, "{shell}");
    }
}

#[test]
fn the_escapes_are_for_every_shell_that_may_run_the_command() {
    assert_eq!(Quoting::for_shells(["/bin/sh", "bash.exe"]), SH);
    assert_eq!(Quoting::for_shells(["pwsh.exe", "pwsh.exe"]), PS);
    // jobs under PowerShell, panes under Git Bash
    assert_eq!(Quoting::for_shells(["powershell.exe", "C:\\Program Files\\Git\\bin\\bash.exe"]), PS_SH);
    assert_eq!(Quoting::for_shells(["cmd.exe"]), CMD);
    assert_eq!(Quoting::for_shells(["cmd.exe", "sh"]), CMD_SH);
    assert_eq!(Quoting::for_shells(["pwsh.exe", "cmd.exe"]), PS_CMD);
    assert_eq!(Quoting::for_shells(["pwsh.exe", "cmd.exe", "zsh"]), PS_SH_CMD);
    // a shell whose quoting is not known counts as PowerShell and as sh
    assert_eq!(Quoting::for_shells(["pwsh.exe", "nu"]), PS_SH);
    assert_eq!(Quoting::for_shells(["fish"]), PS_SH);
    assert_eq!(Quoting::for_shells([]), SH);
    // only Unix keeps tmux's escapes exactly
    assert!(TMUX.tmux && WINDOWS.iter().all(|q| !q.tmux));
}

/// PowerShell's verbatim string at the start of `s` (single quotes: ' ‘ ’ ‚ ‛, a pair
/// of them is one quote): its text and what follows it.
fn ps_single_quoted(s: &str) -> Option<(String, &str)> {
    let q = |c: char| matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}');
    let mut it = s.char_indices();
    if !it.next().is_some_and(|(_, c)| q(c)) {
        return None;
    }
    let mut text = String::new();
    while let Some((i, c)) = it.next() {
        if q(c) {
            match it.clone().next() {
                Some((_, n)) if q(n) => {
                    it.next();
                    text.push(n);
                }
                _ => return Some((text, &s[i + c.len_utf8()..])),
            }
        } else {
            text.push(c);
        }
    }
    None
}

/// PowerShell's expandable string at the start of `s` (double quotes: " “ ” „, a pair is
/// one quote; ` escapes the next character, `0 `a `b `e `f `n `r `t `v being control
/// characters): its text and what follows, or an error when a $ would expand a variable
/// or a subexpression.
fn ps_double_quoted(s: &str) -> Result<(String, &str), String> {
    let q = |c: char| matches!(c, '"' | '\u{201c}' | '\u{201d}' | '\u{201e}');
    let mut it = s.char_indices().peekable();
    if !it.next().is_some_and(|(_, c)| q(c)) {
        return Err("no quote".into());
    }
    let mut text = String::new();
    while let Some((i, c)) = it.next() {
        match c {
            '`' => match it.next() {
                Some((_, 'u')) => return Err("`u{} is not modelled".into()),
                Some((_, n)) => text.push(match n {
                    '0' => '\0',
                    'a' => '\u{7}',
                    'b' => '\u{8}',
                    'e' => '\u{1b}',
                    'f' => '\u{c}',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'v' => '\u{b}',
                    n => n,
                }),
                None => return Err("unterminated".into()),
            },
            '$' if it.peek().is_some_and(|(_, n)| n.is_alphanumeric() || "_({?^$:".contains(*n)) => return Err(format!("$ expands in {s:?}")),
            c if q(c) => match it.peek() {
                Some(&(_, n)) if q(n) => {
                    it.next();
                    text.push(n);
                }
                _ => return Ok((text, &s[i + c.len_utf8()..])),
            },
            c => text.push(c),
        }
    }
    Err("unterminated".into())
}

/// PowerShell's reading of `s` as one quoted argument: its text, or an error when it
/// expands something or is not exactly one string.
fn ps_word(s: &str) -> Result<String, String> {
    let (text, rest) = match ps_single_quoted(s) {
        Some(r) => r,
        None => ps_double_quoted(s)?,
    };
    if !rest.is_empty() {
        return Err(format!("{rest:?} after the string"));
    }
    Ok(text)
}

/// sh's reading of `s` as one word (POSIX quoting, as dash and bash have it): single
/// quotes keep everything; in double quotes \ escapes $ ` " \ and removes a newline,
/// else stays; outside quotes \ escapes any character. An error for anything that would
/// expand, run a command, glob or end the word.
fn sh_word(s: &str) -> Result<String, String> {
    let mut text = String::new();
    let mut it = s.chars().peekable();
    let mut start = true;
    let expands = |n: Option<&char>| n.is_some_and(|&n| n.is_ascii_alphanumeric() || "_{(@*#?-$!".contains(n));
    while let Some(c) = it.next() {
        match c {
            '\'' => loop {
                match it.next() {
                    Some('\'') => break,
                    Some(c) => text.push(c),
                    None => return Err("unterminated '".into()),
                }
            },
            '"' => loop {
                match it.next() {
                    Some('"') => break,
                    Some('\\') => match it.next() {
                        Some(n @ ('$' | '`' | '"' | '\\')) => text.push(n),
                        Some('\n') => {}
                        Some(n) => text.extend(['\\', n]),
                        None => return Err("unterminated \"".into()),
                    },
                    Some('`') => return Err("` runs a command".into()),
                    Some('$') if expands(it.peek()) => return Err("$ expands".into()),
                    Some(c) => text.push(c),
                    None => return Err("unterminated \"".into()),
                }
            },
            '\\' => match it.next() {
                Some('\n') => {}
                Some(n) => text.push(n),
                None => return Err("\\ at the end".into()),
            },
            c if c.is_ascii_whitespace() => return Err("the word ends".into()),
            '|' | '&' | ';' | '<' | '>' | '(' | ')' | '$' | '`' | '*' | '?' | '[' | '{' => return Err(format!("{c} outside quotes")),
            '~' | '#' if start => return Err(format!("{c} at the start")),
            c => text.push(c),
        }
        start = false;
    }
    Ok(text)
}

/// What cmd.exe would act on in `s` (the command line of cmd /c): a %VAR% it expands, a
/// newline, and an operator outside double quotes without a caret. `inside` starts the
/// scan inside double quotes. (Delayed expansion, !VAR!, is off for cmd /c.)
fn cmd_actions(s: &str, inside: bool) -> Vec<String> {
    let mut acts = Vec::new();
    // phase 1: %name% is expanded wherever it is; a caret in the name finds nothing
    let parts: Vec<&str> = s.split('%').collect();
    let mut k = 1;
    while k + 1 < parts.len() {
        if !parts[k].is_empty() && !parts[k].contains('^') {
            acts.push(format!("%{}%", parts[k]));
            k += 2;
        } else {
            k += 1;
        }
    }
    // phase 2: quotes, carets and operators
    let mut quoted = inside;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\n' | '\r' => acts.push("newline".into()),
            '"' => quoted = !quoted,
            '^' if !quoted => {
                chars.next();
            }
            '&' | '|' | '<' | '>' | '(' | ')' if !quoted => acts.push(c.to_string()),
            _ => {}
        }
    }
    acts
}

/// Whether answer `a` must be refused in single (`'%%'`) or double (`"%%%"`) quotes for
/// the shells of `q`: no form of one of its characters is read as typed by all of them.
fn refused(a: &str, double: bool, q: Quoting) -> bool {
    let chars: Vec<char> = a.chars().collect();
    let ps_sh = chars.iter().enumerate().any(|(i, &c)| match double {
        false => matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}'),
        true => matches!(c, '"' | '\u{201c}' | '\u{201d}' | '\u{201e}' | '$' | '`') || (c == '\\' && matches!(chars.get(i + 1), None | Some('\\' | '\n' | '\r'))),
    });
    (q.powershell && q.posix && ps_sh) || (double && q.cmd && q.posix && a.contains('"'))
}

/// The literal-text property for a value a shell runs: each answer either is refused
/// (only when no form works for all the shells) or reaches PowerShell and sh exactly as
/// typed and cmd.exe acts on nothing in it. With cmd.exe in the set the other shells see
/// its carets and doubled quotes too, but expand nothing.
#[test]
fn every_shell_reads_an_answer_as_typed_or_it_is_refused() {
    let answers = answers();
    let mut counts = [0usize; 2];
    for q in WINDOWS {
        for a in &answers {
            for (template, double) in [("'%%'", false), ("\"%%%\"", true), ("\"%1%\"", true)] {
                let filled = fill_block_value(template, std::slice::from_ref(a), Reader::Direct, q);
                assert_eq!(filled.is_err(), refused(a, double, q), "{q:?} {a:?} in {template} -> {filled:?}");
                counts[filled.is_err() as usize] += 1;
                let Ok(word) = filled else { continue };
                if q.powershell {
                    let text = ps_word(&word).unwrap_or_else(|e| panic!("PowerShell: {q:?} {a:?} -> {word:?}: {e}"));
                    if !q.cmd {
                        assert_eq!(text, *a, "PowerShell: {q:?} -> {word:?}");
                    }
                }
                if q.posix {
                    let text = sh_word(&word).unwrap_or_else(|e| panic!("sh: {q:?} {a:?} -> {word:?}: {e}"));
                    if !q.cmd {
                        assert_eq!(text, *a, "sh: {q:?} -> {word:?}");
                    }
                }
                if q.cmd {
                    for inside in [false, true] {
                        assert_eq!(cmd_actions(&word, inside), Vec::<String>::new(), "cmd.exe: {q:?} {a:?} -> {word:?}");
                    }
                }
            }
        }
    }
    // (most answers are put in: a refusal needs both PowerShell and sh, or cmd.exe and sh)
    assert!(counts[0] > 4 * counts[1], "{counts:?}");
    // PowerShell or sh alone never refuses
    for q in [SH, PS, CMD, PS_CMD] {
        for a in &answers {
            assert!(fill_block_value("'%%' \"%%%\"", std::slice::from_ref(a), Reader::Direct, q).is_ok(), "{q:?} {a:?}");
        }
    }
}

#[test]
fn the_escapes_for_each_set_of_shells() {
    let fill = |t: &str, a: &str, q: Quoting| fill_block_value(t, &[a.to_string()], Reader::Direct, q);
    // PowerShell: '' and ‘‘ in single quotes; "" and `$ in double quotes, a Windows path
    // as it is
    assert_eq!(fill("echo '%%'", "x'y\u{2019}z", PS).unwrap(), "echo 'x''y\u{2019}\u{2019}z'");
    assert_eq!(fill("echo \"%%%\"", "a\"b$c`d\u{201c}", PS).unwrap(), "echo \"a\"\"b`$c``d\u{201c}\u{201c}\"");
    assert_eq!(fill("Get-Item \"%%%\"", "C:\\Users\\x", PS).unwrap(), "Get-Item \"C:\\Users\\x\"");
    // sh alone on Windows: sh's own escapes ('\'' and a backslash before \ " $ `), not
    // tmux's (which leaves ` and puts a backslash before ; and ~)
    assert_eq!(fill("echo '%%'", "it's", SH).unwrap(), "echo 'it'\\''s'");
    assert_eq!(fill("echo \"%%%\"", "a\\\"$`;~", SH).unwrap(), "echo \"a\\\\\\\"\\$\\`;~\"");
    // PowerShell and sh: what has no form for both is refused, the rest is as typed
    assert_eq!(fill("echo \"%%%\"", "C:\\Users\\x y", PS_SH).unwrap(), "echo \"C:\\Users\\x y\"");
    assert_eq!(fill("echo '%%'", "a\"$`\\b", PS_SH).unwrap(), "echo 'a\"$`\\b'");
    for (t, a) in [("echo '%%'", "it's"), ("echo '%%'", "\u{2019}"), ("echo \"%%%\"", "$$(echo PWNED)"), ("echo \"%%%\"", "`id`"), ("echo \"%%%\"", "a\"b"), ("echo \"%%%\"", "C:\\x\\"), ("echo \"%%%\"", "\\\\s")] {
        let e = fill(t, a, PS_SH).unwrap_err();
        assert!(e.contains("cannot be quoted for both PowerShell and sh"), "{e}");
    }
    // cmd.exe: carets and doubled quotes, nothing for sh's or tmux's quotes
    assert_eq!(fill("echo %%", "a&b|c%PATH%!x\"y", CMD).unwrap(), "echo a^&b^|c^%PATH^%^!x\"\"y");
    assert_eq!(fill("echo '%%'", "it's\nx", CMD).unwrap(), "echo 'it'\\''s x'");
    assert_eq!(fill("type \"%%%\"", "a;b~c$d", CMD).unwrap(), "type \"a;b~c$d\"");
    assert_eq!(fill("type \"%%%\"", "a\"b", PS_CMD).unwrap(), "type \"a\"\"b\"");
    // (cmd.exe reads PowerShell's escapes as text: its doubled curly quotes stay doubled)
    assert_eq!(fill("echo \"%%%\"", "\u{201c}q\u{201d}\u{201e}", PS_CMD).unwrap(), "echo \"\u{201c}\u{201c}q\u{201d}\u{201d}\u{201e}\u{201e}\"");
    assert_eq!(fill("echo '%%'", "it\u{2019}s '\u{2018}", PS_CMD).unwrap(), "echo 'it\u{2019}\u{2019}s ''\u{2018}\u{2018}'");
    assert!(fill("type \"%%%\"", "a\"b", CMD_SH).unwrap_err().contains("cmd.exe and sh"));
}

/// Whether the command parser keeps answer `a` inside its quotes: it removes spaces and
/// a comment after a newline there (yylex_token, cmd-parse.y:1683) and a backslash and
/// newline anywhere (yylex_getc), as tmux 3.8 does.
fn parser_keeps(a: &str) -> bool {
    let a = a.replace("\r\n", "\n");
    !["\n ", "\n\t", "\n#", "\\\n"].iter().any(|s| a.contains(s))
}

#[test]
fn the_command_parser_reads_a_command_strings_answer_exactly() {
    for q in [TMUX].into_iter().chain(WINDOWS) {
        for a in answers().iter().filter(|a| parser_keeps(a)) {
            // (the parser reads CR LF as a newline, also inside quotes: COMPATIBILITY.md)
            let want = vec![vec!["set".to_string(), "-g".into(), "@c".into(), a.replace("\r\n", "\n")]];
            let a = std::slice::from_ref(a);
            // a string template, and a block value that is a command string (if-shell's
            // commands): the parser's single quotes get tmux's escape whatever the shell
            assert_eq!(cmdparse::parse(&fill_template("set -g @c '%%'", a, q).unwrap()).ok(), Some(want.clone()), "{a:?} {q:?}");
            assert_eq!(cmdparse::parse(&fill_block_value("set -g @c '%%'", a, Reader::Parser, q).unwrap()).ok(), Some(want.clone()), "{a:?} {q:?}");
            // %%% in the parser's double quotes gets tmux's escapes (" \ $ ; ~)
            if !a[0].contains(['\n', '\r']) {
                assert_eq!(cmdparse::parse(&fill_template("set -g @c \"%%%\"", a, q).unwrap()).ok(), Some(want.clone()), "{a:?} {q:?}");
            }
            // a shell's escape read by the parser (a value taken for a shell command, as
            // an unknown command's) stays one word: the answer cannot end the quotes
            if let Ok(value) = fill_block_value("set -g @c '%%'", a, Reader::Direct, q) {
                let parsed = cmdparse::parse(&value).unwrap_or_else(|e| panic!("{a:?} {q:?}: {e}"));
                assert_eq!((parsed.len(), parsed[0].len()), (1, 4), "{a:?} {q:?}: {parsed:?}");
            }
        }
    }
}

#[test]
fn quotes_inside_the_parsers_quotes_are_the_next_shells() {
    for q in WINDOWS {
        for a in &answers() {
            // command-prompt "run-shell \"echo '%%'\"": the single quotes inside the
            // parser's double quotes are the shell's, and it gets what a block value gets
            // (or the same refusal)
            let one = std::slice::from_ref(a);
            match fill_template("run-shell \"echo '%%'\"", one, q) {
                Ok(t) => {
                    let parsed = cmdparse::parse(&t).unwrap_or_else(|e| panic!("{a:?}: {e}"));
                    assert_eq!(parsed, [vec!["run-shell".to_string(), fill_block_value("echo '%%'", one, Reader::Direct, q).unwrap()]], "{q:?} {a:?}");
                }
                Err(_) => assert!(refused(a, false, q), "{q:?} {a:?}"),
            }
            // command-prompt "run-shell 'echo \"%%%\"'": double quotes inside the parser's
            // single quotes (where the parser reads CR LF as a newline; for cmd.exe both
            // are spaces already)
            let read = [if q.cmd { a.clone() } else { a.replace("\r\n", "\n") }];
            if !q.cmd && !parser_keeps(a) {
                continue;
            }
            match fill_template("run-shell 'echo \"%%%\"'", one, q) {
                Ok(t) => {
                    let parsed = cmdparse::parse(&t).unwrap_or_else(|e| panic!("{a:?}: {e}"));
                    assert_eq!(parsed, [vec!["run-shell".to_string(), fill_block_value("echo \"%%%\"", &read, Reader::Direct, q).unwrap()]], "{q:?} {a:?}");
                }
                Err(_) => assert!(refused(a, true, q) || refused(&read[0], true, q), "{q:?} {a:?}"),
            }
        }
    }
    // tmux 3.8 (Unix) is kept: there the parser takes the backslash of '\'' (so tmux 3.8
    // gives sh the answer's quote unescaped, as here)
    assert_eq!(fill_template("run-shell \"echo '%%'\"", &["it's".into()], TMUX).unwrap(), "run-shell \"echo 'it'\\''s'\"");
    assert_eq!(fill_template("run-shell 'echo \"%%%\"'", &["a\"b".into()], TMUX).unwrap(), "run-shell 'echo \"a\\\"b\"'");
    // the parser's quote state follows escapes and the answers put in before
    assert_eq!(fill_template("x \"\\\"'%%'\"", &["'".into()], PS).unwrap(), "x \"\\\"''''\"");
    assert_eq!(fill_template("x '\"' '%%'", &["'".into()], PS).unwrap(), "x '\"' ''\\'''");
    assert_eq!(fill_template("x %1 \"'%2'\"", &["\"".into(), "'".into()], PS).unwrap(), "x \" \"'''\"");
}

#[test]
fn a_whole_answer_and_n_are_not_escaped_for_any_shell() {
    for q in [TMUX].into_iter().chain(WINDOWS) {
        for a in ADVERSARIAL {
            // a value that is %% alone is the answer (rename-window -- '%%'), %1 is as typed
            let a = [a.to_string()];
            assert_eq!(fill_block_value("%%", &a, Reader::Direct, q).unwrap(), a[0]);
            assert_eq!(fill_block_value("%%", &a, Reader::Parser, q).unwrap(), a[0]);
            assert_eq!(fill_block_value("x %1", &a, Reader::Direct, q).unwrap(), format!("x {}", a[0]));
            assert_eq!(fill_template("x %1", &a, q).unwrap(), format!("x {}", a[0]));
        }
    }
}

#[test]
fn unix_keeps_the_escapes_of_tmux_3_8() {
    // cmd_template_replace's SQ and DQ escapes, for the parser and for the shell alike,
    // and nothing is refused
    for reader in [Reader::Parser, Reader::Direct, Reader::Job] {
        assert_eq!(fill_block_value("echo '%%'", &["x'; echo pwned; '".into()], reader, TMUX).unwrap(), "echo 'x'\\''; echo pwned; '\\'''");
        assert_eq!(fill_block_value("echo \"%%%\"", &["a\"b\\c$d;e~f`g".into()], reader, TMUX).unwrap(), "echo \"a\\\"b\\\\c\\$d\\;e\\~f`g\"");
        assert_eq!(fill_block_value("echo '%%'", &["\u{2019}".into()], reader, TMUX).unwrap(), "echo '\u{2019}'");
        for a in answers() {
            assert!(fill_block_value("'%%' \"%%%\"", &[a], reader, TMUX).is_ok());
        }
    }
}

/// Commands the shell of jobs runs (run-shell, if-shell's test, pipe-pane) are escaped
/// for that shell alone, so with jobs under PowerShell and panes under Git Bash an answer
/// with a quote still reaches run-shell; a pane's command is for every shell and is
/// refused.
#[test]
fn a_jobs_command_is_escaped_for_the_shell_of_jobs() {
    let a = ["it's $(x)".to_string()];
    let cmds = fill_block("run-shell \"echo '%%'\" ; if-shell \"test '%%'\" \"set @a '%%'\" ; pipe-pane \"cat '%%'\"", &a, PS_SH, PS).unwrap();
    for (c, n) in cmds.iter().zip([2, 2, 2]) {
        let (text, after) = ps_single_quoted(c[n].split_once(' ').unwrap().1).unwrap();
        assert_eq!((text.as_str(), after), (a[0].as_str(), ""), "{c:?}");
    }
    // (if-shell's command is parsed: tmux's escape)
    assert_eq!(cmds[1][3], "set @a 'it'\\''s $(x)'");
    let e = fill_block("run-shell \"echo '%%'\" ; split-window \"less '%%'\"", &a, PS_SH, PS).unwrap_err();
    assert!(e.contains("PowerShell and sh"), "{e}");
    // nothing runs when one value is refused
    assert!(fill_block("split-window \"less '%%'\" ; run-shell \"echo '%%'\"", &a, PS_SH, PS).is_err());
    // if-shell -F's first argument is a format
    let cmds = fill_block("if-shell -F \"#{==:'%%',x}\" { set @a 1 }", &["'".into()], PS_SH, PS);
    assert!(cmds.is_err());
}

/// A binding made in a prompt's block (`{ bind -n F3 run-shell "echo '%%'" }`): the bound
/// run-shell's command is a shell's, so it gets the shell's escape (review: it was taken
/// for a string the parser reads, and PowerShell ran the answer).
#[test]
fn a_binding_of_several_words_gives_its_command_the_shells_escape() {
    let a = ["x'; echo pwned; '".to_string()];
    for q in [TMUX, SH, PS, PS_CMD] {
        for all in [q, PS_SH] {
            let block = fill_block("bind-key -n F3 run-shell \"echo '%%'\"", &a, if q.tmux { q } else { all }, q).unwrap();
            let bound = super::cmds::binding_command(&block[0][4..]);
            let parsed = cmdparse::parse(&bound).unwrap();
            assert_eq!(parsed, [vec!["run-shell".to_string(), fill_block_value("echo '%%'", &a, Reader::Job, q).unwrap()]], "{q:?}");
            let word = parsed[0][1].strip_prefix("echo ").unwrap();
            if q.powershell {
                assert_eq!(ps_word(word).unwrap(), a[0], "{q:?}");
            } else if !q.tmux {
                assert_eq!(sh_word(word).unwrap(), a[0], "{q:?}");
            }
        }
    }
    // one string after the key is parsed when the key is pressed: tmux's escape for it
    let block = fill_block("bind-key -n F3 \"run-shell \\\"echo '%%'\\\"\"", &a, PS, PS).unwrap();
    let parsed = cmdparse::parse(&super::cmds::binding_command(&block[0][4..])).unwrap();
    assert_eq!(ps_word(parsed[0][1].strip_prefix("echo ").unwrap()).unwrap(), a[0]);
}

/// Each word the escapes give sh, read by real shells (dash as /bin/sh, and bash when
/// there is one) in one script per set of shells: what the model says they read.
#[cfg(unix)]
#[test]
fn real_shells_read_the_words_as_the_model_does() {
    let answers = answers();
    for shell in ["/bin/sh", "/bin/bash"] {
        if !std::path::Path::new(shell).exists() {
            continue;
        }
        for q in [SH, PS_SH, CMD_SH] {
            let words: Vec<String> = answers
                .iter()
                .flat_map(|a| ["'%%'", "\"%%%\""].map(|t| fill_block_value(t, std::slice::from_ref(a), Reader::Direct, q)))
                .filter_map(Result::ok)
                .collect();
            let script: String = words.iter().map(|w| format!("printf '%s\\0' {w}\n")).collect();
            let out = std::process::Command::new(shell).arg("-c").arg(&script).output().unwrap();
            assert!(out.status.success(), "{shell}: {out:?}");
            let read: Vec<String> = String::from_utf8(out.stdout).unwrap().split_terminator('\0').map(String::from).collect();
            let model: Vec<String> = words.iter().map(|w| sh_word(w).unwrap()).collect();
            assert_eq!(read.len(), model.len(), "{shell} {q:?}");
            for ((r, m), w) in read.iter().zip(&model).zip(&words) {
                assert_eq!(r, m, "{shell} {q:?}: {w:?}");
            }
        }
    }
}

/// The answers of the review's cases (and the Windows checklist's).
const LAYERED: &[&str] = &[
    "$(echo pwned)",
    "a\"b",
    "x`y",
    "it's",
    "x'; echo pwned; '",
    "x\" ; run-shell pwned ; \"",
    "\u{2018}q\u{2019} \u{201c}d\u{201d}",
    "$env:USERPROFILE",
    "%PATH%",
    "C:\\Users\\x",
    "a & b | c",
];

/// `%%%` in the shell's double quotes written `\"` inside the parser's double quotes
/// (`command-prompt "run-shell \"echo \\\"%%%\\\"\""`): the double quotes around the
/// answer are the shell's, so it gets the shell's escape, or the refusal, as a block
/// value does (review: it got the parser's, and PowerShell and sh ran `$(echo pwned)`).
#[test]
fn shell_double_quotes_inside_the_parsers_get_the_shells_escape() {
    for q in WINDOWS {
        for a in LAYERED {
            let one = [a.to_string()];
            let want = fill_block_value("echo \"%%%\"", &one, Reader::Job, q);
            let got = fill_template("run-shell \"echo \\\"%%%\\\"\"", &one, q);
            let block = fill_block("if-shell true \"run-shell \\\"echo \\\\\\\"%%%\\\\\\\"\\\"\"", &one, q, q);
            let Ok(want) = want else {
                assert!(got.is_err() && block.is_err(), "{q:?} {a:?}");
                continue;
            };
            let got = cmdparse::parse(&got.unwrap()).unwrap();
            assert_eq!(got, [vec!["run-shell".to_string(), want.clone()]], "{q:?} {a:?}");
            let block = block.unwrap();
            assert_eq!(cmdparse::parse(block[0].last().unwrap()).unwrap(), [vec!["run-shell".to_string(), want.clone()]], "{q:?} {a:?}");
            if q.powershell && !q.cmd {
                assert_eq!(ps_word(want.strip_prefix("echo ").unwrap()).as_deref(), Ok(*a), "{q:?}");
            } else if q.posix && !q.cmd {
                assert_eq!(sh_word(want.strip_prefix("echo ").unwrap()).as_deref(), Ok(*a), "{q:?}");
            }
        }
    }
    let one = ["$(echo pwned)".to_string()];
    assert_eq!(fill_template("run-shell \"echo \\\"%%%\\\"\"", &one, PS).unwrap(), "run-shell \"echo \\\"`\\$(echo pwned)\\\"\"");
    assert!(fill_template("run-shell \"echo \\\"%%%\\\"\"", &one, PS_SH).is_err());
    // Unix: tmux 3.8's escape for the parser's double quotes
    assert_eq!(fill_template("run-shell \"echo \\\"%%%\\\"\"", &one, TMUX).unwrap(), "run-shell \"echo \\\"\\$(echo pwned)\\\"\"");
}

/// A command string in a command string (`if-shell true "run-shell \"echo '%%'\""`): each
/// parser the answer goes through gets its escape, down to the shell (review: only the
/// first parser's quotes were kept, and if-shell's parse ran `run-shell pwned`).
#[test]
fn every_parser_an_answer_goes_through_is_escaped_for() {
    for q in WINDOWS {
        for a in LAYERED {
            let one = [a.to_string()];
            let want = fill_block_value("echo '%%'", &one, Reader::Job, q);
            let string = fill_template("if-shell true \"run-shell \\\"echo '%%'\\\"\"", &one, q);
            let block = fill_block("if-shell true \"if-shell true \\\"run-shell \\\\\\\"echo '%%'\\\\\\\"\\\"\"", &one, q, q);
            let Ok(want) = want else {
                assert!(string.is_err() && block.is_err(), "{q:?} {a:?}");
                continue;
            };
            let outer = cmdparse::parse(&string.unwrap()).unwrap();
            assert_eq!(outer.len(), 1, "{q:?} {a:?}: {outer:?}");
            assert_eq!(cmdparse::parse(&outer[0][2]).unwrap(), [vec!["run-shell".to_string(), want.clone()]], "{q:?} {a:?}");
            let block = block.unwrap();
            let first = cmdparse::parse(block[0].last().unwrap()).unwrap();
            assert_eq!(first.len(), 1, "{q:?} {a:?}: {first:?}");
            assert_eq!(cmdparse::parse(&first[0][2]).unwrap(), [vec!["run-shell".to_string(), want.clone()]], "{q:?} {a:?}");
            if q.powershell && !q.cmd {
                assert_eq!(ps_word(want.strip_prefix("echo ").unwrap()).as_deref(), Ok(*a), "{q:?}");
            }
        }
    }
    // the answer outside the next reader's quotes reaches it as typed
    assert_eq!(cmdparse::parse(&fill_template("rename-window \"%%\"", &["it's".into()], PS).unwrap()).unwrap(), [vec!["rename-window", "it's"]]);
    assert_eq!(cmdparse::parse(&fill_template("display '[%%%]'", &["a\"b$c".into()], PS).unwrap()).unwrap(), [vec!["display", "[a\"b$c]"]]);
    // Unix: tmux 3.8's escapes, for the first parser
    assert_eq!(
        fill_template("if-shell true \"run-shell \\\"echo '%%'\\\"\"", &["x\" ; run-shell pwned ; \"".into()], TMUX).unwrap(),
        "if-shell true \"run-shell \\\"echo 'x\" ; run-shell pwned ; \"'\\\"\"",
    );
}

/// An answer that ends a bound command (`x;`): who reads the words after it is decided
/// where the filled words end the commands, as bind-key splits them (review: the
/// if-shell string was escaped as send-keys' word, then parsed by if-shell).
#[test]
fn a_bound_command_an_answer_ends_is_escaped_for_who_reads_it() {
    let a = ["x\" ; run-shell \"echo PWNED >> o\" ; display \"y;".to_string()];
    let t = "bind -n F3 send-keys '%%' if-shell true \"run-shell \\\"echo '%%'\\\"\"";
    for q in [PS, PS_SH, PS_CMD, SH, CMD] {
        let block = fill_block(t, &a, q, q).unwrap_or_else(|e| panic!("{q:?}: {e}"));
        let bound = cmdparse::parse(&super::cmds::binding_command(&block[0][4..])).unwrap();
        assert_eq!(bound.len(), 2, "{q:?}: {bound:?}");
        assert_eq!(bound[0], ["send-keys", a[0].strip_suffix(';').unwrap()]);
        let inner = cmdparse::parse(&bound[1][2]).unwrap();
        assert_eq!(inner, [vec!["run-shell".to_string(), fill_block_value("echo '%%'", &a, Reader::Job, q).unwrap()]], "{q:?}");
        if q.powershell && !q.cmd {
            assert_eq!(ps_word(inner[0][1].strip_prefix("echo ").unwrap()).as_deref(), Ok(a[0].as_str()));
        }
    }
    // Unix: the words as tmux 3.8 fills them
    let block = fill_block(t, &a, TMUX, TMUX).unwrap();
    assert_eq!(block[0][4..], ["send-keys", &a[0], "if-shell", "true", &format!("run-shell \"echo '{}'\"", a[0])]);
}
