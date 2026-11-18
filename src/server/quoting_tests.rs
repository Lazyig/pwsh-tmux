//! Prompt answers put into templates (cmd_template_replace): the escapes chosen for the
//! shells that may run them. The Windows choices are pure functions of the shells' names,
//! so they run here on every platform, checked against models of how PowerShell, cmd.exe
//! and the command parser read the result.
use crate::cmdparse;
use super::input::{fill_block_value, fill_template, Quoting, Reader};
use super::procinfo::{shell_kind, ShellKind};

const TMUX: Quoting = Quoting::TMUX;
const PS: Quoting = Quoting { powershell: true, cmd: false };
const CMD: Quoting = Quoting { powershell: false, cmd: true };
const PS_CMD: Quoting = Quoting { powershell: true, cmd: true };

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
fn the_escapes_are_safe_for_every_shell_that_may_run_the_command() {
    assert_eq!(Quoting::for_shells(["/bin/sh", "bash.exe"]), TMUX);
    assert_eq!(Quoting::for_shells(["pwsh.exe", "pwsh.exe"]), PS);
    // jobs under PowerShell, panes under Git Bash: PowerShell's escapes (safe in sh too)
    assert_eq!(Quoting::for_shells(["powershell.exe", "C:\\Program Files\\Git\\bin\\bash.exe"]), PS);
    assert_eq!(Quoting::for_shells(["cmd.exe", "sh"]), CMD);
    assert_eq!(Quoting::for_shells(["pwsh.exe", "cmd.exe"]), PS_CMD);
    // a shell whose quoting is not known gets PowerShell's escapes
    assert_eq!(Quoting::for_shells(["sh", "nu"]), PS);
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
/// one quote; ` escapes the next character): its text and what follows, or an error
/// when a $ would expand a variable or a subexpression.
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
                Some((_, n)) => text.push(n),
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

/// What cmd.exe would act on in `s` (the command line of cmd /c): a %VAR% it expands, a
/// newline, and an operator outside double quotes without a caret. `inside` starts the
/// scan inside double quotes.
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

#[test]
fn powershell_reads_a_single_quoted_answer_exactly() {
    for q in [PS, PS_CMD] {
        for a in ADVERSARIAL {
            // { run-shell "echo '%%'" }: the value goes to the shell as it is
            let cmd = fill_block_value("echo '%%'", &[a.to_string()], Reader::Direct, q);
            let rest = cmd.strip_prefix("echo ").unwrap();
            let (text, after) = ps_single_quoted(rest).unwrap_or_else(|| panic!("{cmd:?}"));
            assert_eq!(after, "", "{a:?} -> {cmd:?}");
            if q.cmd {
                // the carets (and a newline turned into a space) stay in the quotes
                assert!(!text.contains('\n') && !text.contains('\r'), "{cmd:?}");
            } else {
                assert_eq!(text, *a, "{cmd:?}");
            }
        }
    }
}

#[test]
fn powershell_reads_a_double_quoted_answer_without_expanding_it() {
    for q in [PS, PS_CMD] {
        for a in ADVERSARIAL {
            // %%% in double quotes: " doubled, ` and $ escaped with `, \ doubled
            let cmd = fill_block_value("echo \"%%%\"", &[a.to_string()], Reader::Direct, q);
            let rest = cmd.strip_prefix("echo ").unwrap();
            let (text, after) = ps_double_quoted(rest).unwrap_or_else(|e| panic!("{a:?} -> {cmd:?}: {e}"));
            assert_eq!(after, "", "{a:?} -> {cmd:?}");
            if !q.cmd {
                assert_eq!(text, a.replace('\\', "\\\\"), "{cmd:?}");
            }
            // %1% escapes as %%% does
            let one = fill_block_value("echo \"%1%\"", &[a.to_string()], Reader::Direct, q);
            assert_eq!(one, cmd);
        }
    }
}

#[test]
fn cmd_exe_acts_on_nothing_an_answer_has() {
    for q in [CMD, PS_CMD] {
        for a in ADVERSARIAL {
            for (template, inside) in [("echo '%%'", false), ("type \"%%\"", false), ("echo %%", false), ("echo %%", true), ("type \"%%%\"", false), ("echo '%1%'", false)] {
                let cmd = fill_block_value(template, &[a.to_string()], Reader::Direct, q);
                assert_eq!(cmd_actions(&cmd, inside), Vec::<String>::new(), "{a:?} in {template:?} -> {cmd:?}");
            }
        }
        // a caret for each special character, a doubled " keeps cmd.exe's quotes as they were
        assert_eq!(fill_block_value("echo %%", &["a&b|c%PATH%!x\"y".into()], Reader::Direct, CMD), "echo a^&b^|c^%PATH^%^!x\"\"y");
        assert_eq!(fill_block_value("echo '%%'", &["it's\nx".into()], Reader::Direct, CMD), "echo 'it'\\''s x'");
    }
}

#[test]
fn the_command_parser_reads_a_command_strings_answer_exactly() {
    for q in [TMUX, PS, CMD, PS_CMD] {
        for a in ADVERSARIAL {
            // (the parser reads CR LF as a newline, also inside quotes: COMPATIBILITY.md)
            let want = vec![vec!["set".to_string(), "-g".into(), "@c".into(), a.replace("\r\n", "\n")]];
            // a string template, and a block value that is a command string (if-shell's
            // commands): the parser's single quotes get tmux's escape whatever the shell
            assert_eq!(cmdparse::parse(&fill_template("set -g @c '%%'", &[a.to_string()], q)).ok(), Some(want.clone()), "{a:?} {q:?}");
            assert_eq!(cmdparse::parse(&fill_block_value("set -g @c '%%'", &[a.to_string()], Reader::Parser, q)).ok(), Some(want.clone()), "{a:?} {q:?}");
            // %%% in the parser's double quotes gets tmux's escapes (" \ $ ; ~)
            if !a.contains(['\n', '\r']) {
                assert_eq!(cmdparse::parse(&fill_template("set -g @c \"%%%\"", &[a.to_string()], q)).ok(), Some(want.clone()), "{a:?} {q:?}");
            }
            // a shell's escape read by the parser (a value taken for a shell command, as
            // an unknown command's) stays one word: the answer cannot end the quotes
            let parsed = cmdparse::parse(&fill_block_value("set -g @c '%%'", &[a.to_string()], Reader::Direct, q)).unwrap_or_else(|e| panic!("{a:?} {q:?}: {e}"));
            assert_eq!((parsed.len(), parsed[0].len()), (1, 4), "{a:?} {q:?}: {parsed:?}");
        }
    }
}

#[test]
fn quotes_inside_the_parsers_quotes_are_the_next_shells() {
    for a in ADVERSARIAL {
        // command-prompt "run-shell \"echo '%%'\"": on Windows the single quotes inside
        // the parser's double quotes are PowerShell's
        for q in [PS, PS_CMD] {
            let parsed = cmdparse::parse(&fill_template("run-shell \"echo '%%'\"", &[a.to_string()], q)).unwrap_or_else(|e| panic!("{a:?}: {e}"));
            assert_eq!(parsed.len(), 1, "{a:?}: {parsed:?}");
            let [name, shell] = &parsed[0][..] else { panic!("{a:?}: {parsed:?}") };
            assert_eq!(name, "run-shell");
            let (text, after) = ps_single_quoted(shell.strip_prefix("echo ").unwrap()).unwrap_or_else(|| panic!("{shell:?}"));
            assert_eq!(after, "", "{a:?} -> {shell:?}");
            if !q.cmd {
                assert_eq!(text, *a);
            }
            // and the block value gives the shell the same
            assert_eq!(*shell, fill_block_value("echo '%%'", &[a.to_string()], Reader::Direct, q));
            // command-prompt "run-shell 'echo \"%%%\"'": double quotes inside the
            // parser's single quotes
            let parsed = cmdparse::parse(&fill_template("run-shell 'echo \"%%%\"'", &[a.to_string()], q)).unwrap_or_else(|e| panic!("{a:?}: {e}"));
            assert_eq!(parsed.len(), 1, "{a:?}: {parsed:?}");
            let shell = &parsed[0][1];
            let (_, after) = ps_double_quoted(shell.strip_prefix("echo ").unwrap()).unwrap_or_else(|e| panic!("{a:?} -> {shell:?}: {e}"));
            assert_eq!(after, "", "{a:?} -> {shell:?}");
        }
        let parsed = cmdparse::parse(&fill_template("run-shell \"echo '%%'\"", &[a.to_string()], CMD)).unwrap();
        assert_eq!(cmd_actions(&parsed[0][1], false), Vec::<String>::new(), "{a:?}: {parsed:?}");
    }
    // tmux 3.8 (Unix) is kept: there the parser takes the backslash of '\'' (so tmux 3.8
    // gives sh the answer's quote unescaped, as here)
    assert_eq!(fill_template("run-shell \"echo '%%'\"", &["it's".into()], TMUX), "run-shell \"echo 'it'\\''s'\"");
    assert_eq!(fill_template("run-shell 'echo \"%%%\"'", &["a\"b".into()], TMUX), "run-shell 'echo \"a\\\"b\"'");
    // the parser's quote state follows escapes and the answers put in before
    assert_eq!(fill_template("x \"\\\"'%%'\"", &["'".into()], PS), "x \"\\\"''''\"");
    assert_eq!(fill_template("x '\"' '%%'", &["'".into()], PS), "x '\"' ''\\'''");
    assert_eq!(fill_template("x %1 \"'%2'\"", &["\"".into(), "'".into()], PS), "x \" \"'''\"");
}

#[test]
fn a_whole_answer_and_n_are_not_escaped_for_any_shell() {
    for q in [TMUX, PS, CMD, PS_CMD] {
        for a in ADVERSARIAL {
            // a value that is %% alone is the answer (rename-window -- '%%'), %1 is as typed
            assert_eq!(fill_block_value("%%", &[a.to_string()], Reader::Direct, q), *a);
            assert_eq!(fill_block_value("%%", &[a.to_string()], Reader::Parser, q), *a);
            assert_eq!(fill_block_value("x %1", &[a.to_string()], Reader::Direct, q), format!("x {a}"));
            assert_eq!(fill_template("x %1", &[a.to_string()], q), format!("x {a}"));
        }
    }
}

#[test]
fn unix_keeps_the_escapes_of_tmux_3_8() {
    // cmd_template_replace's SQ and DQ escapes, for the parser and for the shell alike
    for reader in [Reader::Parser, Reader::Direct] {
        assert_eq!(fill_block_value("echo '%%'", &["x'; echo pwned; '".into()], reader, TMUX), "echo 'x'\\''; echo pwned; '\\'''");
        assert_eq!(fill_block_value("echo \"%%%\"", &["a\"b\\c$d;e~f`g".into()], reader, TMUX), "echo \"a\\\"b\\\\c\\$d\\;e\\~f`g\"");
        assert_eq!(fill_block_value("echo '%%'", &["\u{2019}".into()], reader, TMUX), "echo '\u{2019}'");
    }
}
