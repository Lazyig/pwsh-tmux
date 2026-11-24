//! Client input: key tables, prompts, overlays, pane modes and the mouse.

use super::modes::{MenuAction, ModeAction};
use super::*;
use crate::{cmdparse, format, opts};
use crate::keys::{Input, Key, KeyCode};

pub use super::mouse::MouseCtx;

/// KEYC_PASTE_START and KEYC_PASTE_END, the keys a bracketed paste starts and ends with
/// (handle_paste): user keys past those user-keys can name, never queued or bound.
const PASTE_START: KeyCode = KeyCode::User(u16::MAX - 1);
const PASTE_END: KeyCode = KeyCode::User(u16::MAX);

impl Client {
    /// tty_stop_tty: the reset sequences that depend on what the terminal was given (Se, or Ss 0,
    /// after a cursor style; Cr after a cursor colour; Dsesc for appesc). The client itself
    /// restores the fixed modes.
    pub(super) fn tty_stop_sequence(&self) -> String {
        let mut out = String::new();
        let cap = |name: &str| self.term_caps.get(name).filter(|v| !v.is_empty());
        if self.input_state.tty_cstyle != 0 {
            match cap("Se") {
                Some(se) => out.push_str(se),
                None => out.push_str(&features::string_capability(&self.term_caps, "Ss", "\x1b[%p1%d q", &["0"])),
            }
        }
        if self.input_state.tty_ccolour.is_some() {
            out.push_str(&features::string_capability(&self.term_caps, "Cr", "\x1b]112\x07", &[]));
        }
        if self.input_state.tty_appesc {
            out.push_str(&features::string_capability(&self.term_caps, "Dsesc", "\x1b[?7727l", &[]));
        }
        // Dsbp, Dsfcs and Dseks: the client always sends the usual sequences, so only an
        // override changes what goes out
        for (name, usual) in [("Dsbp", "\x1b[?2004l"), ("Dsfcs", "\x1b[?1004l"), ("Dseks", "\x1b[>4m")] {
            if let Some(v) = self.term_caps.get(name).filter(|v| !v.is_empty() && *v != usual) {
                out.push_str(v);
            }
        }
        out
    }

    /// server_client_set_key_table: the table and when it was set, which prefix-timeout
    /// measures from.
    pub fn set_key_table(&mut self, name: &str) {
        self.key_table = name.to_string();
        self.input_state.table_at = Some(Instant::now());
    }

    /// Before the client gives up the terminal (exit, suspend, lock): reset it and forget what
    /// it was given, so a later redraw sends it again.
    pub(super) fn tty_stop(&mut self) {
        if self.control != 0 { return; }
        let out = self.tty_stop_sequence();
        if !out.is_empty() { self.send(S_OUTPUT, out.as_bytes()); }
        self.input_state.tty_cstyle = 0;
        self.input_state.tty_ccolour = None;
        self.input_state.tty_appesc = false;
    }
}

/// Per-client key timing. Monotonic time avoids wall clock changes during a paste.
#[derive(Default)]
pub struct InputState {
    /// When the key table was last set (keytable->activity_time), for prefix-timeout.
    pub(super) table_at: Option<Instant>,
    /// The key of the last binding run (c->last_key: Any for the Any binding).
    pub(super) last_key: String,
    key_at: Option<Instant>,
    assume_paste: bool,
    guess_at: Option<Instant>,
    paste_at: Option<Instant>,
    clipboard: Vec<(PaneId, Instant, bool)>,
    /// What the outer terminal was last given (tty->cstyle, tty->ccolour).
    pub(super) tty_cstyle: u8,
    pub(super) tty_ccolour: Option<(u8, u8, u8)>,
    /// Application escape mode was turned on (Enesc).
    pub(super) tty_appesc: bool,
}

pub fn default_bindings() -> BTreeMap<String, BTreeMap<String, Binding>> {
    let mut t: BTreeMap<String, BTreeMap<String, Binding>> = BTreeMap::new();
    for words in cmdparse::parse(include_str!("display-defaults.conf")).expect("tmux default bindings") {
        let o = opts::getopt(&words[1..], "nr", "TN").expect("default binding options");
        let table = if o.has('n') { "root" } else { o.get('T').unwrap_or("prefix") };
        let key = &o.rest[0];
        let key = keys::parse_key_name(key).map(|k| keys::key_name(&k)).unwrap_or_else(|| key.clone());
        let cmd = super::cmds::binding_command(&o.rest[1..]).trim().to_string();
        t.entry(table.into()).or_default().insert(key, Binding {
            cmd, repeat: o.has('r'), note: o.get('N').unwrap_or("").into(),
        });
    }
    // the copy mode tables of tmux 3.8, run by the copy mode engine while unchanged
    for (table, vi) in [("copy-mode", false), ("copy-mode-vi", true)] {
        for (k, c) in CopyMode::default_bindings(vi) {
            let key = keys::parse_key_name(k).map(|k| keys::key_name(&k)).unwrap_or_else(|| k.to_string());
            t.entry(table.into()).or_default().insert(key, Binding { cmd: super::copy::default_binding_command(c), repeat: false, note: String::new() });
        }
    }
    t
}

/// How a prompt's answer is escaped where a template puts it (cmd_template_replace).
/// Unix uses tmux 3.8's escapes, made for POSIX shells and for the command parser, unless
/// a default-shell is PowerShell or cmd.exe. On Windows the commands run under
/// PowerShell, cmd.exe or another shell, so the escapes follow every shell that may run
/// them ([`Server::answer_quoting`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quoting {
    /// PowerShell (or a shell not known): its quotes ' ‘ ’ ‚ ‛ and " “ ” „, and $ and `
    /// in double quotes.
    pub powershell: bool,
    /// cmd.exe, which has no single quotes: its special characters get a caret.
    pub cmd: bool,
    /// A POSIX shell (or a shell not known): ' in single quotes, and \ " $ and ` in
    /// double quotes.
    pub posix: bool,
    /// tmux 3.8's escapes exactly (Unix), not the shells' own.
    pub tmux: bool,
}

impl Quoting {
    /// tmux 3.8's escapes.
    pub const TMUX: Quoting = Quoting { powershell: false, cmd: false, posix: true, tmux: true };

    /// The escapes for all of `shells` (see [`procinfo::ShellKind`]): each shell reads
    /// the answer as typed, a shell whose quoting is not known counts as PowerShell and
    /// POSIX, and no shell at all as POSIX.
    pub fn for_shells<'a>(shells: impl IntoIterator<Item = &'a str>) -> Quoting {
        let mut q = Quoting { powershell: false, cmd: false, posix: false, tmux: false };
        for shell in shells {
            match procinfo::shell_kind(shell) {
                procinfo::ShellKind::Posix => q.posix = true,
                procinfo::ShellKind::Cmd => q.cmd = true,
                procinfo::ShellKind::PowerShell => q.powershell = true,
                procinfo::ShellKind::Other => (q.powershell, q.posix) = (true, true),
            }
        }
        if !(q.powershell || q.cmd) {
            q.posix = true;
        }
        q
    }
}

/// The escapes for the two sets of shells an answer may reach ([`Server::answer_shells`]):
/// every shell that may run a value, and the shell of jobs alone, for a command it runs
/// ([`Reader::Job`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shells {
    pub any: Quoting,
    pub job: Quoting,
}

impl From<Quoting> for Shells {
    fn from(q: Quoting) -> Shells {
        Shells { any: q, job: q }
    }
}

/// The quote a replacement is escaped for (cmd_template_replace's NQ, SQ and DQ).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    Single,
    Double,
}

/// PowerShell's single quotes (CharExtensions.IsSingleQuote): ' ‘ ’ ‚ ‛.
fn ps_single_quote(c: char) -> bool {
    matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}')
}

/// PowerShell's double quotes (CharExtensions.IsDoubleQuote): " “ ” „.
fn ps_double_quote(c: char) -> bool {
    matches!(c, '"' | '\u{201c}' | '\u{201d}' | '\u{201e}')
}

/// The characters cmd.exe acts on (outside double quotes, after a caret they are plain;
/// % takes the caret into a variable's name, which then does not exist).
const CMD_SPECIAL: &[char] = &['^', '&', '|', '<', '>', '(', ')', '%', '!'];

/// Whether `c` (followed by `next`) has no form that PowerShell and a POSIX shell both
/// read as typed inside `quote`: PowerShell's quotes of that kind (a doubled one is one
/// quote for PowerShell, an empty pair for sh, and sh's escapes end PowerShell's quotes),
/// and in double quotes $ and ` (` is PowerShell's escape and starts a command in sh, \
/// is sh's escape and plain for PowerShell). A \ is plain in sh's double quotes unless
/// another \, a line end (the parser may make CR LF a newline, which a \ removes in sh)
/// or what follows the answer (maybe the closing quote) comes next.
/// Quoting the pieces apart (`"a"'$'"b"`) would not do: PowerShell takes an argument
/// that starts with a quoted string and goes on as separate arguments, and refuses
/// them in an expression.
fn no_common_form(c: char, next: Option<char>, quote: Quote) -> bool {
    match quote {
        Quote::None => false,
        Quote::Single => ps_single_quote(c),
        Quote::Double => ps_double_quote(c) || c == '$' || c == '`' || (c == '\\' && matches!(next, None | Some('\\' | '\n' | '\r'))),
    }
}

/// An answer escaped for `quote` (cmd_template_replace). With [`Quoting::TMUX`], tmux
/// 3.8's: a single quote becomes '\'' for single quotes, and " \ $ ; ~ get a backslash
/// for double quotes. Otherwise each shell of `q` reads the answer as typed (cmd.exe:
/// acts on nothing in it), else the answer is refused:
/// - single quotes (`'%%'`): PowerShell's single quotes are doubled, else ' becomes '\''
///   (both keep the value one word for the parser too);
/// - double quotes (`"%%%"`): for PowerShell " “ ” „ are doubled and $ and ` get its
///   backtick, for a POSIX shell \ " $ and ` get a backslash;
/// - with both PowerShell and a POSIX shell, the characters for which no form works for
///   both ([`no_common_form`]) refuse the answer;
/// - cmd.exe: each of ^ & | < > ( ) % ! gets a caret and " is doubled, so whether the
///   answer is inside cmd.exe's double quotes or not, nothing in it is acted on (inside
///   them the carets stay, and the other shells of `q` read them too); a newline (which
///   ends the command) becomes a space. With a POSIX shell, " in double quotes has no
///   form for both (\" ends cmd.exe's quotes) and refuses the answer.
fn escape_answer(out: &mut String, s: &str, quote: Quote, q: Quoting) -> Result<(), String> {
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if quote == Quote::None {
            out.push(c);
            continue;
        }
        if q.tmux {
            match quote {
                Quote::Single if c == '\'' => {
                    out.push_str("'\\''");
                    continue;
                }
                Quote::Double if "\"\\$;~".contains(c) => out.push('\\'),
                _ => {}
            }
            out.push(c);
            continue;
        }
        let both = |what: &str| format!("an answer with {} cannot be quoted for both {what}", shown_char(c));
        if q.powershell && q.posix && no_common_form(c, chars.peek().copied(), quote) {
            return Err(both("PowerShell and sh"));
        }
        if q.cmd && (c == '\n' || c == '\r') {
            out.push(' ');
            continue;
        }
        match quote {
            Quote::Single if q.powershell && ps_single_quote(c) => {
                out.extend([c, c]);
                continue;
            }
            // (cmd.exe has no single quotes, but a value in them stays one word if the
            // parser reads it)
            Quote::Single if c == '\'' => {
                out.push_str("'\\''");
                continue;
            }
            Quote::Single if q.cmd && c == '"' => {
                out.push_str("\"\"");
                continue;
            }
            Quote::Double if q.powershell && ps_double_quote(c) => {
                out.extend([c, c]);
                continue;
            }
            Quote::Double if q.cmd && c == '"' => {
                if q.posix {
                    return Err(both("cmd.exe and sh"));
                }
                out.push_str("\"\"");
                continue;
            }
            // (with PowerShell too a \ is plain for both, see no_common_form)
            Quote::Double if q.posix && !q.powershell && matches!(c, '\\' | '"' | '$' | '`') => out.push('\\'),
            Quote::Double if q.powershell && matches!(c, '$' | '`') => out.push('`'),
            _ => {}
        }
        if q.cmd && CMD_SPECIAL.contains(&c) {
            out.push('^');
        }
        out.push(c);
    }
    Ok(())
}

/// A character of an answer as an error shows it: itself, and its code point when it is
/// not ASCII, so a curly quote (`‘ (U+2018)`) is not taken for `'`.
fn shown_char(c: char) -> String {
    if c.is_ascii() { c.to_string() } else { format!("{c} (U+{:04X})", c as u32) }
}

/// `inner` (an answer escaped for the next shell) put inside the command parser's
/// `quote`, so parsing gives `inner` back: " \ $ ~ and a newline get a backslash inside
/// double quotes, a ' becomes '\'' inside single quotes.
fn encode_for_parser(out: &mut String, inner: &str, quote: Quote) {
    for c in inner.chars() {
        match (quote, c) {
            (Quote::Double, '"' | '\\' | '$' | '~') => out.extend(['\\', c]),
            (Quote::Double, '\n') => out.push_str("\\n"),
            (Quote::Double, '\r') => out.push_str("\\r"),
            (Quote::Single, '\'') => out.push_str("'\\''"),
            _ => out.push(c),
        }
    }
}

/// What reads a template once the answers are in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader {
    /// The command parser: a string template, or a { } block value that is a command
    /// string (if-shell's commands, run-shell -C and the like).
    Parser,
    /// Whatever the value is given to as it is: a shell command (split-window,
    /// display-popup) or a plain value, which may reach any shell (`#()`).
    Direct,
    /// A command the shell of jobs runs (run-shell's, if-shell's test, pipe-pane's),
    /// with the `#()` in it: escaped for that shell alone ([`Server::job_answer_quoting`]).
    Job,
}

/// The quote of the command parser (yylex_token's states) at byte `at` of `text`.
fn parser_quote_at(text: &str, at: usize) -> Quote {
    let (mut state, mut escaped) = (Quote::None, false);
    for ch in text[..at].chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match (state, ch) {
            (Quote::None, '\'') => state = Quote::Single,
            (Quote::None, '"') => state = Quote::Double,
            (Quote::Single, '\'') | (Quote::Double, '"') => state = Quote::None,
            (Quote::None | Quote::Double, '\\') => escaped = true,
            _ => {}
        }
    }
    state
}

/// Stands for the answer while a template is parsed to see who reads it (private use
/// characters, plain in any quotes).
const ANSWER_MARK: &str = "\u{f8fe}\u{f8ff}";

/// The answer `s` escaped for `quote` where [`ANSWER_MARK`] is in `text`, which the
/// command parser reads, so that whatever reads the answer after it reads it as typed:
/// - outside the parser's quotes the answer is escaped for them as tmux 3.8 does (the
///   parser reads the answer's syntax: not made safe);
/// - inside them, the parsed word with the answer says who reads it next ([`copy_block`]
///   tells): where the answer is inside that reader's quotes, it gets that reader's
///   escape (another parser's, so a command string in a command string is escaped
///   layer by layer, or a shell's, for the shells of `sh`) encoded for the parser's
///   quotes; where it is not, the answer as typed, encoded for the parser's quotes, so
///   the parsed word has it as typed;
/// - when the text does not parse (or the answer is in no word), as if the parser's
///   quotes are the answer's when they are of `quote`'s kind, else a shell's.
/// An error when no escape keeps the answer as typed for those shells.
///
/// [`copy_block`]: super::cmds::copy_block
fn parser_escape(text: &str, s: &str, quote: Quote, sh: Shells, depth: usize) -> Result<String, String> {
    let mut out = String::new();
    let Some(at) = text.find(ANSWER_MARK) else { return Ok(out) };
    let state = parser_quote_at(text, at);
    if state == Quote::None || quote == Quote::None {
        escape_answer(&mut out, s, quote, Quoting::TMUX)?;
        return Ok(out);
    }
    let found = std::cell::RefCell::new(None);
    let parsed = super::cmds::copy_block(text, &|v, reader| {
        if v.contains(ANSWER_MARK) {
            found.borrow_mut().get_or_insert((v.to_string(), reader));
        }
        v.to_string()
    });
    let found = parsed.ok().and(found.into_inner());
    let inner = match found {
        Some((word, reader)) => {
            let at = word.find(ANSWER_MARK).unwrap_or(0);
            match (parser_quote_at(&word, at), reader) {
                (Quote::None, _) => s.to_string(),
                (_, Reader::Parser) if depth < 8 => parser_escape(&word, s, quote, sh, depth + 1)?,
                (_, Reader::Parser) => return Err("an answer is nested in too many command strings".into()),
                (_, Reader::Job) => escaped(s, quote, sh.job)?,
                (_, Reader::Direct) => escaped(s, quote, sh.any)?,
            }
        }
        None if state == quote => {
            escape_answer(&mut out, s, quote, Quoting::TMUX)?;
            return Ok(out);
        }
        None => escaped(s, quote, sh.any)?,
    };
    encode_for_parser(&mut out, &inner, state);
    Ok(out)
}

/// [`escape_answer`] into a new string.
fn escaped(s: &str, quote: Quote, q: Quoting) -> Result<String, String> {
    let mut out = String::new();
    escape_answer(&mut out, s, quote, q)?;
    Ok(out)
}

/// cmd_template_replace: in `template` replace %N (and %N%) with `s`, and the first %%
/// (or %%%) left. %% escapes single quotes, %%% and %N% escape characters special inside
/// double quotes; `sq` false leaves a %% answer as it is.
///
/// With [`Quoting::TMUX`] (Unix) the escapes are tmux 3.8's wherever the replacement is.
/// Otherwise a value read by the parser gets [`parser_escape`]'s escapes, layer by
/// layer down to who finally reads the answer, and a value given to a shell (or to
/// anything) the escapes of the shells that may read it. An error when no escape keeps
/// the answer as typed for all those shells.
fn template_replace(template: &str, s: &str, idx: usize, sq: bool, reader: Reader, sh: Shells) -> Result<String, String> {
    if !template.contains('%') {
        return Ok(template.to_string());
    }
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::with_capacity(template.len() + s.len());
    let (mut i, mut replaced) = (0, false);
    while i < chars.len() {
        let ch = chars[i];
        let from = i;
        i += 1;
        if ch == '%' {
            let quote = if i < chars.len() && chars[i].to_digit(10).is_some_and(|d| d >= 1 && d as usize == idx) {
                i += 1;
                Some(if chars.get(i) == Some(&'%') { i += 1; Quote::Double } else { Quote::None })
            } else if chars.get(i) == Some(&'%') && !replaced {
                replaced = true;
                i += 1;
                Some(if chars.get(i) == Some(&'%') { i += 1; Quote::Double } else { Quote::Single })
            } else {
                None
            };
            if let Some(quote) = quote {
                let quote = if quote == Quote::Single && !sq { Quote::None } else { quote };
                match reader {
                    Reader::Parser if sh.any.tmux => escape_answer(&mut out, s, quote, Quoting::TMUX)?,
                    Reader::Parser => {
                        // the template as the parser reads it, the answer marked where it is
                        let marked: String = chars[..from].iter().chain(ANSWER_MARK.chars().collect::<Vec<_>>().iter()).chain(chars[i..].iter()).collect();
                        out.push_str(&parser_escape(&marked, s, quote, sh, 0)?);
                    }
                    Reader::Job => escape_answer(&mut out, s, quote, sh.job)?,
                    Reader::Direct => escape_answer(&mut out, s, quote, sh.any)?,
                }
                continue;
            }
        }
        out.push(ch);
    }
    Ok(out)
}

/// args_make_commands for a string template: each answer replaces %N and the next %%,
/// and the result is parsed. An error when an answer cannot be quoted for the shells.
pub fn fill_template(template: &str, answers: &[String], sh: impl Into<Shells>) -> Result<String, String> {
    let sh = sh.into();
    let mut out = template.to_string();
    for (i, a) in answers.iter().enumerate() {
        out = template_replace(&out, a, i + 1, true, Reader::Parser, sh)?;
    }
    Ok(out)
}

/// args_copy_copy_value: the answers put into one string value of a { } block's
/// command, which is not parsed again. %% escapes a single quote for single quotes
/// (cmd_template_replace's SQ mode) as in tmux 3.8, since such a value may be a
/// string parsed later (an if-shell or run-shell command, a shell command): only a
/// value that is `%%` alone, so the whole answer (`rename-window -- '%%'`, the
/// default `prefix ,`), keeps its single quotes, where tmux 3.8 leaves "it'\''s"
/// (COMPATIBILITY.md, Intentional differences). %%% and %N% keep tmux's escapes on
/// Unix. `reader` is who reads the value ([`super::cmds::copy_block`]), which on
/// Windows decides the escapes with `q` ([`template_replace`]).
pub fn fill_block_value(value: &str, answers: &[String], reader: Reader, sh: impl Into<Shells>) -> Result<String, String> {
    let sh = sh.into();
    let mut out = value.to_string();
    for (i, a) in answers.iter().enumerate() {
        let sq = out != "%%";
        out = template_replace(&out, a, i + 1, sq, reader, sh)?;
    }
    Ok(out)
}

/// args_make_commands for a { } block template: its commands with the answers in their
/// values ([`super::cmds::copy_block`], [`fill_block_value`]), escaped with `job` for the
/// commands the shell of jobs runs and `q` for the rest. An error, and no commands, when
/// an answer cannot be quoted for the shells.
pub fn fill_block(template: &str, answers: &[String], q: Quoting, job: Quoting) -> Result<Vec<Vec<String>>, String> {
    let refused = std::cell::RefCell::new(None);
    let cmds = super::cmds::copy_block(template, &|v, reader| {
        fill_block_value(v, answers, reader, Shells { any: q, job }).unwrap_or_else(|e| {
            refused.borrow_mut().get_or_insert(e);
            String::new()
        })
    })?;
    match refused.into_inner() {
        Some(e) => Err(e),
        None => Ok(cmds),
    }
}

impl Server {
    /// The escapes for answers put into templates ([`Quoting`]), for every shell that may
    /// run them: the shell of jobs (run-shell, if-shell, #(), pipe-pane, copy-pipe) and
    /// default-shell, globally and in each session, which runs new panes, popups and
    /// detach-client -E. On Windows jobs run under procinfo's default_shell
    /// (PWSH_TMUX_SHELL, or else pwsh or powershell, which default-shell does not
    /// change). On Unix they run under /bin/sh, and the escapes are tmux 3.8's unless a
    /// default-shell is PowerShell (the default when pwsh is installed) or cmd.exe, whose
    /// quotes tmux's escapes do not keep.
    pub(crate) fn answer_quoting(&self) -> Quoting {
        let shell = |sid: Option<SessId>| Some(self.opt_session(sid, "default-shell")).filter(|s| !s.is_empty()).unwrap_or_else(procinfo::default_shell);
        let defaults = std::iter::once(shell(None)).chain(self.sessions.keys().map(|s| shell(Some(*s))));
        if cfg!(windows) {
            let shells: Vec<String> = std::iter::once(procinfo::default_shell()).chain(defaults).collect();
            return Quoting::for_shells(shells.iter().map(String::as_str));
        }
        let shells: Vec<String> = defaults.collect();
        if shells.iter().all(|s| !matches!(procinfo::shell_kind(s), procinfo::ShellKind::PowerShell | procinfo::ShellKind::Cmd)) {
            return Quoting::TMUX;
        }
        Quoting::for_shells(std::iter::once("/bin/sh").chain(shells.iter().map(String::as_str)))
    }

    /// The escapes for a command the shell of jobs runs ([`Reader::Job`]): that shell's
    /// alone on Windows, tmux's on Unix (/bin/sh).
    pub(crate) fn job_answer_quoting(&self) -> Quoting {
        if !cfg!(windows) {
            return Quoting::TMUX;
        }
        Quoting::for_shells([procinfo::default_shell().as_str()])
    }

    /// [`Self::answer_quoting`] and [`Self::job_answer_quoting`].
    pub(crate) fn answer_shells(&self) -> Shells {
        Shells { any: self.answer_quoting(), job: self.job_answer_quoting() }
    }

    pub(crate) fn width_context(&self) -> crate::width::Guard {
        let entries = self.opts_server.array_items("codepoint-widths").into_iter().map(|(_, v)| v).collect::<Vec<_>>();
        crate::width::enter(features::codepoint_widths(&entries), options::is_on(&self.opt_server("variation-selector-always-wide")))
    }

    pub(crate) fn input_timers(&mut self) {
        let now = Instant::now();
        for c in self.clients.values_mut() {
            if c.input_state.guess_at.is_some_and(|t| now.duration_since(t) >= Duration::from_secs(5)) {
                c.input_state.assume_paste = false; c.input_state.guess_at = None;
            }
            c.input_state.clipboard.retain(|(_, t, _)| now.duration_since(*t) < Duration::from_secs(5));
        }
        let expired: Vec<_> = self.clients.iter().filter(|(_, c)| c.input_state.paste_at.is_some_and(|t| now.duration_since(t) >= Duration::from_secs(5))).map(|(id, _)| *id).collect();
        for cid in expired {
            let c = self.clients.get_mut(&cid).unwrap();
            c.input_state.paste_at = None;
            c.input_state.assume_paste = false;
            let inputs = c.keyp.expire_paste();
            for input in inputs { self.dispatch_input(cid, input); }
        }
        let expired: Vec<_> = self.sessions.values().filter_map(|s| {
            let seconds = self.opt_session_num(Some(s.id), "lock-after-time");
            (seconds > 0 && now_secs().saturating_sub(s.activity) >= seconds as u64).then_some(s.id)
        }).collect();
        for sid in expired {
            let ids: Vec<_> = self.clients.values().filter(|c| c.session == Some(sid)).map(|c| c.id).collect();
            for id in ids { self.lock_input_client(id); }
        }
    }

    pub(crate) fn lock_input_client(&mut self, cid: ClientId) {
        let Some(c) = self.clients.get(&cid) else { return };
        if !c.attached || c.suspended || c.control != 0 { return; }
        let command = self.opt_session(c.session, "lock-command");
        if command.is_empty() { return; }
        let c = self.clients.get_mut(&cid).unwrap();
        c.tty_stop();
        c.suspended = true;
        c.activity = now_secs();
        c.send(S_SHELL, command.as_bytes());
    }

    /// prompt_find_history_file: an absolute path or one under ~/; anything else is ignored.
    fn history_path(&self) -> Option<std::path::PathBuf> {
        let path = self.opt_server("history-file");
        if let Some(rest) = path.strip_prefix("~/").or_else(|| if cfg!(windows) { path.strip_prefix("~\\") } else { None }) {
            return procinfo::home_dir().map(|home| std::path::PathBuf::from(home).join(rest));
        }
        let path = std::path::PathBuf::from(path);
        (path.is_absolute() || path.starts_with("/")).then_some(path)
    }

    /// prompt_add_history: a line equal to the newest entry is not added again; the oldest
    /// entries go once prompt-history-limit is reached.
    pub(crate) fn add_prompt_history(&mut self, line: &str, ty: usize) {
        let limit = self.opt_server_num("prompt-history-limit").max(0) as usize;
        let Some(history) = self.prompt_history.get_mut(ty) else { return };
        if history.last().is_none_or(|last| last != line) { history.push(line.to_string()); }
        let excess = history.len().saturating_sub(limit);
        history.drain(..excess);
    }

    /// prompt_load_history: `type:line` entries; a line without a known type is command history.
    pub(crate) fn load_prompt_history(&mut self) {
        let Some(text) = self.history_path().and_then(|path| std::fs::read(path).ok()) else { return };
        let text = String::from_utf8_lossy(&text).into_owned();
        for line in text.split_inclusive('\n') {
            let line = line.strip_suffix('\n').unwrap_or(line);
            match line.split_once(':').and_then(|(ty, rest)| Some((prompt_type(ty)?, rest))) {
                Some((ty, rest)) => self.add_prompt_history(rest, ty),
                None => self.add_prompt_history(line, 0),
            }
        }
    }

    /// prompt_save_history: written once, when the server exits.
    pub(crate) fn save_prompt_history(&self) {
        let Some(path) = self.history_path() else { return };
        let mut text = String::new();
        for (ty, history) in self.prompt_history.iter().enumerate() {
            for line in history { text.push_str(&format!("{}:{line}\n", PROMPT_TYPES[ty])); }
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        if let Ok(mut file) = options.open(path) { let _ = file.write_all(text.as_bytes()); }
    }

    /// tty_set_selection: write the client's Ms capability (default OSC 52) with `clip` as the
    /// selection parameter.
    pub(crate) fn set_selection(&self, cid: ClientId, clip: &str, data: &[u8]) {
        let Some(c) = self.clients.get(&cid) else { return };
        if !c.attached || c.suspended || c.control != 0 || !c.features.has("clipboard") { return; }
        let encoded = super::cmds::base64(data);
        // dropped while blocked (tty_add), TTY_NOBLOCK otherwise
        c.send_noblock(features::string_capability(&c.term_caps, "Ms", "\x1b]52;%p1%s;%p2%s\x07", &[clip, &encoded]).as_bytes());
    }

    /// The OSC 52 queries, sets and passthrough of one read of output; returns
    /// whether each set reached the clipboard (pane-set-clipboard fires for those).
    pub(crate) fn pane_input_requests(&mut self, pane: PaneId, queries: Vec<(Vec<u8>, bool)>, sets: Vec<(Vec<u8>, Vec<u8>)>, passthrough: Vec<Vec<u8>>) -> Vec<bool> {
        let mut set = vec![false; sets.len()];
        // Clients showing the pane (screen_write_set_client_cb).
        let showing = |srv: &Self| -> Vec<ClientId> {
            srv.clients.values().filter(|c| c.attached && !c.suspended && c.control == 0 && c.session.is_some_and(|s| {
                let w = &srv.windows[&srv.sessions[&s].active_window()];
                w.panes().contains(&pane) && (!w.zoomed || w.active == pane)
            })).map(|c| c.id).collect()
        };
        // input_osc_52_parse: only with set-clipboard on; the selector keeps its allowed characters.
        if self.opt_server("set-clipboard") == "on" {
            for (i, (selector, data)) in sets.into_iter().enumerate() {
                if data.is_empty() { continue; }
                let Ok(decoded) = decode_osc52(&data) else { continue };
                let clip = clipboard_selector(&selector);
                for cid in showing(self) { self.set_selection(cid, &clip, &decoded); }
                set[i] = true;
                self.add_buffer(None, decoded, false);
            }
            for (selector, bel) in queries {
                let clip = clipboard_selector(&selector).bytes().next();
                match self.opt_server("get-clipboard").as_str() {
                    "buffer" => {
                        // paste_get_top: the newest automatic buffer only
                        if let Some(buffer) = self.buffers.iter().find(|b| b.automatic) {
                            if let Some(p) = self.panes.get(&pane) { p.send(clipboard_reply_bytes(clip, &buffer.data, bel)); }
                        }
                    }
                    "request" | "both" => {
                        // input_add_request: the most recently active attached client (not suspended or
                        // locked) whose session has the window; Ms goes to it if it has the feature
                        let cid = self.clients.values()
                            .filter(|c| c.attached && !c.suspended && c.control == 0 && c.session.is_some_and(|s| self.sessions[&s].links.values().any(|w| self.windows[w].panes().contains(&pane))))
                            .max_by_key(|c| (c.activity, c.input_state.key_at, std::cmp::Reverse(c.id)))
                            .filter(|c| c.features.has("clipboard")).map(|c| c.id);
                        if let Some(cid) = cid {
                            let c = self.clients.get_mut(&cid).unwrap();
                            if c.input_state.clipboard.len() < 64 {
                                c.input_state.clipboard.push((pane, Instant::now(), bel));
                                // tty_clipboard_query asks with an empty selection parameter
                                c.send_tty(features::string_capability(&c.term_caps, "Ms", "\x1b]52;%p1%s;%p2%s\x07", &["", "?"]).as_bytes());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let allow = self.opt_pane(pane, "allow-passthrough");
        if allow != "off" {
            for c in self.clients.values().filter(|c| c.attached && !c.suspended && c.control == 0) {
                let linked = c.session.is_some_and(|s| self.sessions[&s].links.values().any(|w| self.windows[w].panes().contains(&pane)));
                let visible = c.session.is_some_and(|s| { let w = &self.windows[&self.sessions[&s].active_window()]; w.panes().contains(&pane) && (!w.zoomed || w.active == pane) });
                if !linked || (allow != "all" && !visible) { continue; }
                if c.block.active() {
                    // tty_add drops it while blocked; output of a pane drawn to the client
                    // is counted with the pane's other output (block_count)
                    if !self.pane_drawn(c, pane) {
                        let n: usize = passthrough.iter().map(|d| d.len()).sum();
                        c.io.discarded.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
                    }
                    continue;
                }
                for data in &passthrough { c.send(S_OUTPUT, data); }
                // tty_cmd_rawstring: TTY_NOBLOCK
                if !passthrough.is_empty() { c.noblock(); }
            }
        }
        set
    }

    /// tty_keys_clipboard: the terminal's OSC 52 reply answers the panes' get-clipboard
    /// requests, and makes a buffer if refresh-client -l asked for it in the last few seconds.
    fn clipboard_reply(&mut self, cid: ClientId, raw: &[u8]) {
        let Some(body) = raw.strip_prefix(b"\x1b]52;").and_then(|s| s.strip_suffix(b"\x07").or_else(|| s.strip_suffix(b"\x1b\\"))) else { return };
        let Some(at) = body.iter().position(|b| *b == b';') else { return };
        // tty_keys_clipboard keeps the selection only when exactly one was given
        let clip = (at == 1).then(|| body[0]);
        let Ok(data) = decode_osc52(&body[at + 1..]) else { return };
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let requests = std::mem::take(&mut c.input_state.clipboard);
        let query = c.clipboard_query.take().is_some_and(|d| Instant::now() < d);
        let setting = self.opt_server("get-clipboard");
        if !requests.is_empty() && matches!(setting.as_str(), "request" | "both") {
            for (pane, at, bel) in requests {
                if at.elapsed() <= Duration::from_secs(5) {
                    if let Some(p) = self.panes.get(&pane) { p.send(clipboard_reply_bytes(clip, &data, bel)); }
                    // input_request_clipboard_reply: both also makes a buffer for each request
                    if setting == "both" { self.add_buffer(None, data.clone(), false); }
                }
            }
        }
        if query && !data.is_empty() { self.add_buffer(None, data, false); }
    }

    pub fn handle_input(&mut self, cid: ClientId, data: &[u8]) {
        let user_keys = self.opts_server.array_items("user-keys").into_iter()
            .filter_map(|(n, s)| n.strip_prefix("user-keys[").and_then(|s| s.strip_suffix(']')).and_then(|s| s.parse::<u16>().ok()).filter(|n| *n <= 1000).map(|n| (n, s.into_bytes()))).collect();
        let Some(c) = self.clients.get_mut(&cid) else { return };
        if c.suspended { return; }
        c.keyp.set_user_keys(user_keys);
        let inputs = c.keyp.feed(data);
        if c.keyp.in_paste() && c.input_state.paste_at.is_none() { c.input_state.paste_at = Some(Instant::now()); }
        if !c.keyp.in_paste() { c.input_state.paste_at = None; }
        // a partial paste end or theme/colour reply restarts the deadline at 500ms or more,
        // as does a reply that may still come after a later reply settled its request,
        // from its second byte (ESC [ or ESC P)
        let late = c.terminal.late_replies();
        let reply = c.keyp.partial_paste_end() || c.keyp.theme_reply_pending()
            || theme::REPLY_PREFIXES.iter().any(|(bit, prefix)| late & bit != 0 && c.keyp.reply_pending(prefix, 2));
        c.esc_deadline = if c.keyp.has_pending() || c.keyp.partial_paste_end() {
            let mut ms = self.opts_server.get("escape-time").and_then(|v| v.parse().ok()).unwrap_or(10u64).max(1);
            // tty_keys_next: also while a clipboard request (TTY_OSC52QUERY) or a reply to
            // the terminal requests (device attributes, DECRPM, colours) is outstanding;
            // not for a request a reply to a later one settled (COMPATIBILITY.md)
            let clipboard = !c.input_state.clipboard.is_empty() || c.clipboard_query.is_some_and(|d| Instant::now() < d);
            if reply || clipboard || c.terminal.replies_pending() { ms = ms.max(500); }
            if reply { Some(Instant::now() + Duration::from_millis(ms)) }
            else { c.esc_deadline.or_else(|| Some(Instant::now() + Duration::from_millis(ms))) }
        } else {
            None
        };
        // tty_keys_next takes every key of the read (server_client_handle_key0) before
        // the client's queue runs them, so a prompt a key's binding opens does not get
        // the keys after it in the same read: they wait behind the prompt in the queue
        let reading = self.reading.replace(cid);
        for i in inputs {
            self.dispatch_input(cid, i);
        }
        self.reading = reading;
        self.drain_clients();
    }

    pub fn dispatch_input(&mut self, cid: ClientId, input: Input) {
        if let Input::Unknown(raw) = &input {
            if raw.starts_with(b"\x1b]52;") {
                self.clipboard_reply(cid, raw);
                return;
            }
            // terminal replies (device attributes, DECRPM 2026, theme and colours) are
            // read for every client, read-only ones too (tty_keys_next)
            if self.terminal_report(cid, raw) { return; }
            // focus reports set the client's focus (also for read-only clients)
            if raw == b"\x1b[I" || raw == b"\x1b[O" {
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.focused = raw == b"\x1b[I";
                }
            }
        }
        if self.client_is_readonly(Some(cid)) && !matches!(&input, Input::Key(_, _)) {
            return;
        }
        match input {
            Input::Key(k, raw) => self.handle_key(cid, k, raw),
            Input::Mouse(m, _) => {
                if m.kind == keys::MouseKind::Move && self.clients.get(&cid).is_some_and(|c| c.prompt.is_none() && c.overlay.is_none()) {
                    if let Some(sid) = self.client_session(cid).filter(|s| self.opt_session_on(Some(*s), "focus-follows-mouse")) {
                        let wid = self.sessions[&sid].active_window();
                        let top = self.opt_session(Some(sid), "status-position") == "top";
                        let offset = if top { self.status_lines(Some(sid)) } else { 0 };
                        if let Some(y) = m.y.checked_sub(offset) {
                            if let Some((pane, _)) = self.rects(wid).0.iter().find(|(_, r)| {
                                let r = self.content_rect(wid, *r); m.x >= r.x && m.x < r.right() && y >= r.y && y < r.bottom()
                            }).copied() { self.select_pane(wid, pane); }
                        }
                    }
                }
                self.handle_mouse(cid, m)
            },
            Input::Paste(data) => self.handle_paste(cid, data),
            Input::Unknown(raw) => {
                // focus reports reach panes through pane-focus-in/out (only those that asked)
                if raw == b"\x1b[I" || raw == b"\x1b[O" {
                    self.client_focus(cid, raw == b"\x1b[I");
                    return;
                }
                // a CSI sequence tmux does not know is read as M-[ and then a key for
                // each byte (tty_keys_next), so a prompt, a menu or a binding gets them
                if let Some(keys) = unknown_csi_keys(&raw) {
                    for (k, raw) in keys {
                        self.handle_key(cid, k, raw);
                    }
                    return;
                }
                // what cannot wait is done now, the rest goes in the client's queue
                if !self.raw_input_taken_now(cid, &raw) {
                    self.client_append(cid, ClientItem::Raw(raw));
                }
            }
        }
    }

    /// A bracketed paste. tmux reads it as keys, PasteStart, each pasted key and
    /// PasteEnd, and each goes through server_client_handle_key0: what an overlay, a
    /// dead or capture-keys pane, a menu or a prompt takes is done at once (the queue may
    /// be blocked), the rest goes in the client's queue. The keys queued are pasted
    /// (paste_key) when PasteStart was queued too: CLIENT_BRACKETPASTING is set only as
    /// the queue reaches it, so after a PasteStart something took they are plain keys.
    fn handle_paste(&mut self, cid: ClientId, data: Vec<u8>) {
        if !self.keys_taken_now(cid) {
            self.client_append(cid, ClientItem::Paste(data));
            return;
        }
        // a running popup or a pane capturing all keys takes the whole paste: in one
        // write, unless a key in it could close the popup or the pane
        if let Some(pane) = self.paste_sink(cid, &data) {
            self.send_paste_marker(cid, pane, true);
            if self.panes.get(&pane).is_some_and(|p| p.options.get("@input-disabled") != Some("on")) {
                let mut targets = vec![pane];
                targets.extend(self.sync_peers(pane));
                for q in targets {
                    if let Some(p) = self.panes.get(&q) { p.send(data.clone()); }
                }
            }
            self.send_paste_marker(cid, pane, false);
            return;
        }
        let mut parser = keys::Parser::new();
        let mut inputs = parser.feed(&data);
        inputs.extend(parser.flush_pending());
        let inputs: Vec<Input> = inputs.into_iter().flat_map(|i| match &i {
            Input::Unknown(raw) => match unknown_csi_keys(raw) {
                Some(keys) => keys.into_iter().map(|(k, raw)| Input::Key(k, raw)).collect(),
                None => vec![i],
            },
            _ => vec![i],
        }).collect();
        let pasting = self.handle_key_now(cid, Key::plain(PASTE_START), Vec::new(), false).is_some();
        let mut pasted = Vec::new();
        for i in inputs {
            // (once nothing takes keys at once, the rest of a paste is queued as it is)
            if pasting && !self.keys_taken_now(cid) {
                let (Input::Key(_, raw) | Input::Mouse(_, raw) | Input::Unknown(raw) | Input::Paste(raw)) = i;
                pasted.extend(raw);
                continue;
            }
            match i {
                Input::Key(k, raw) => {
                    if let Some((k, raw)) = self.handle_key_now(cid, k, raw, false) {
                        if pasting { pasted.extend(raw); } else { self.client_append(cid, ClientItem::Key(k, raw)); }
                    }
                }
                Input::Mouse(_, raw) | Input::Unknown(raw) | Input::Paste(raw) => {
                    if !self.raw_input_taken_now(cid, &raw) {
                        if pasting { pasted.extend(raw); } else { self.client_append(cid, ClientItem::Raw(raw)); }
                    }
                }
            }
        }
        // (a PasteEnd nothing took is dropped after a PasteStart something took: there
        // was no paste start for the pane either)
        self.handle_key_now(cid, Key::plain(PASTE_END), Vec::new(), false);
        if pasting {
            self.client_append(cid, ClientItem::Paste(pasted));
        }
    }

    /// Whether server_client_handle_key0 could take a key at once: a message, an
    /// overlay, a dead, modal or capture-keys active pane, the window's menu, the status
    /// prompt, a pane prompt or a copy-mode jump waiting for its character.
    fn keys_taken_now(&self, cid: ClientId) -> bool {
        let Some(c) = self.clients.get(&cid) else { return false };
        if c.message.is_some() || c.overlay.is_some() || c.prompt.is_some() {
            return true;
        }
        if self.client_window(cid).is_some_and(|w| self.menus.contains_key(&w)) || self.pane_prompt_target(cid).is_some() {
            return true;
        }
        let Some(pane) = self.client_active_pane(cid) else { return false };
        let Some(p) = self.panes.get(&pane) else { return false };
        p.dead.is_some()
            || p.fflags & (super::float::CAPTURE_KEYS | super::float::CLOSE_ON_CANCEL) != 0
            || matches!(&p.mode, Some(PaneMode::Copy(cm)) if cm.waiting_for_key())
    }

    /// The pane that takes a whole paste at once: a popup's running command, unless the
    /// paste has an Escape or C-c that closes it, or an active pane capturing all keys
    /// (not one Escape or C-c closes).
    fn paste_sink(&self, cid: ClientId, data: &[u8]) -> Option<PaneId> {
        let c = self.clients.get(&cid)?;
        if c.message.is_some() || c.is_readonly() {
            return None;
        }
        let cancel = data.iter().any(|b| *b == 0x1b || *b == 0x03);
        if let Some(Overlay::Popup(p)) = &c.overlay {
            let running = self.panes.get(&p.pane).is_some_and(|x| x.dead.is_none());
            return (running && (!cancel || p.close_on_exit || p.close_exit_zero)).then_some(p.pane);
        }
        let pane = self.client_active_pane(cid)?;
        let p = self.panes.get(&pane)?;
        let modal = self.window_of(pane).is_some_and(|w| self.windows[&w].modal == Some(pane));
        let closes = modal && p.fflags & super::float::CLOSE_ON_CANCEL != 0 && cancel;
        (p.fflags & super::float::CAPTURE_KEYS != 0 && p.mode.is_none() && p.dead.is_none() && !closes).then_some(pane)
    }

    /// The paste start or end key for a pane (input_key_pane): its bracket marker, when
    /// the application asked for bracketed paste.
    fn send_paste_marker(&mut self, cid: ClientId, pane: PaneId, start: bool) {
        if self.client_is_readonly(Some(cid)) { return; }
        let Some(p) = self.panes.get(&pane).filter(|p| p.options.get("@input-disabled") != Some("on")) else { return };
        if p.screen().bracketed_paste() {
            p.send(if start { b"\x1b[200~".to_vec() } else { b"\x1b[201~".to_vec() });
        }
    }

    /// Bytes the key parser did not know, for what takes keys at once (as
    /// handle_key_now): a popup's command, a dead pane waiting for a key, a pane
    /// capturing all keys; a menu or a prompt takes them without using them. False when
    /// they go in the client's queue.
    fn raw_input_taken_now(&mut self, cid: ClientId, raw: &[u8]) -> bool {
        let Some(c) = self.clients.get(&cid) else { return true };
        if c.is_readonly() { return true; }
        if c.message.is_some() {
            if c.message_ignore_keys { return true; }
            let c = self.clients.get_mut(&cid).unwrap();
            c.message = None;
            c.dirty = true;
        }
        if let Some(Overlay::Popup(p)) = &self.clients[&cid].overlay {
            let (pane, any_key) = (p.pane, p.close_any_key);
            match self.panes.get(&pane).filter(|x| x.dead.is_none()) {
                Some(x) => if x.options.get("@input-disabled") != Some("on") { x.send(raw.to_vec()); },
                None => if any_key { self.close_popup(cid, pane); },
            }
            return true;
        }
        if self.client_session(cid).is_none() {
            return true;
        }
        if let Some(pane) = self.client_active_pane(cid) {
            if self.dead_key(pane) {
                return true;
            }
            if let Some(p) = self.panes.get(&pane).filter(|p| p.fflags & super::float::CAPTURE_KEYS != 0 && p.mode.is_none() && p.dead.is_none()) {
                if p.options.get("@input-disabled") != Some("on") { p.send(raw.to_vec()); }
                return true;
            }
        }
        self.keys_taken_now(cid)
    }

    /// popup_key_cb returned 1: the popup closes (server_client_clear_overlay).
    fn close_popup(&mut self, cid: ClientId, pane: PaneId) {
        if let Some(c) = self.clients.get_mut(&cid) {
            c.overlay = None;
            c.dirty = true;
        }
        self.destroy_pane(pane);
    }

    /// A bracketed paste the client's queue reached (paste_key): to the active pane
    /// (cmd_find_from_client, never a popup), as it is.
    pub(super) fn paste_now(&mut self, cid: ClientId, data: Vec<u8>) {
        if self.client_is_readonly(Some(cid)) || self.client_session(cid).is_none() {
            return;
        }
        if let Some(target) = self.client_active_pane(cid) {
            // the paste-start key reaches the pane's mode first: it ends clock mode
            // like any key; in another mode the pasted text is dropped, also in the
            // mode clock mode was stacked on, which ending it brings back
            if matches!(self.panes.get(&target).map(|p| &p.mode), Some(Some(PaneMode::Clock))) {
                self.exit_mode(target);
            }
            if self.panes.get(&target).is_some_and(|p| p.mode.is_some()) {
                return;
            }
            if self.panes.get(&target).is_some_and(|p| p.options.get("@input-disabled") == Some("on")) { return; }
            self.paste(target, &data, true, true, None);
        }
    }

    /// Bytes the key parser did not know, reached in the client's queue: to the pane
    /// when it is not in a mode.
    pub(super) fn raw_input_now(&mut self, cid: ClientId, raw: Vec<u8>) {
        if self.client_is_readonly(Some(cid)) {
            return;
        }
        if let Some(t) = self.client_active_pane(cid) {
            if self.panes.get(&t).is_some_and(|p| p.mode.is_none() && p.dead.is_none() && p.options.get("@input-disabled") != Some("on")) {
                self.panes[&t].send(raw);
            }
        }
    }

    /// Where typed input goes: a popup's pane, else the active pane.
    fn input_target(&self, cid: ClientId) -> Option<PaneId> {
        if let Some(Overlay::Popup(p)) = self.clients.get(&cid).and_then(|c| c.overlay.as_ref()) {
            return Some(p.pane);
        }
        self.client_active_pane(cid)
    }

    fn send_key_to_pane(&mut self, cid: ClientId, pane: PaneId, k: &Key, raw: Vec<u8>) {
        if self.client_is_readonly(Some(cid)) {
            return;
        }
        if self.panes.get(&pane).is_none_or(|p| p.options.get("@input-disabled") == Some("on")) { return; }
        let Some(bytes) = self.encode_pane_key(pane, k, raw.clone()) else { return };
        if let Some(p) = self.panes.get(&pane) {
            p.send(bytes);
        }
        for q in self.sync_peers(pane) {
            if let Some(bytes) = self.encode_pane_key(q, k, raw.clone()) {
                self.panes[&q].send(bytes);
            }
        }
    }

    /// window_pane_copy_key: with synchronize-panes a key also goes to the window's other
    /// visible panes that have it on and are not in a mode, encoded for each of them
    /// (input_key_pane: its own cursor, keypad and extended key modes).
    pub(crate) fn sync_peers(&self, pane: PaneId) -> Vec<PaneId> {
        let Some(wid) = self.window_of(pane) else { return Vec::new() };
        if self.opt_pane(pane, "synchronize-panes") != "on" {
            return Vec::new();
        }
        self.windows[&wid].panes().into_iter().filter(|q| {
            *q != pane
                && self.panes.get(q).is_some_and(|pp| pp.mode.is_none() && pp.dead.is_none() && pp.options.get("@input-disabled") != Some("on"))
                && self.pane_visible(wid, *q)
                && self.opt_pane(*q, "synchronize-panes") == "on"
        }).collect()
    }

    /// window_pane_paste: pasted bytes go to the pane as they are, but not to a pane in a mode
    /// or with input disabled.
    fn paste_bytes(&mut self, cid: ClientId, pane: PaneId, data: &[u8]) {
        if self.client_is_readonly(Some(cid)) { return; }
        let ok = |p: &Pane| p.mode.is_none() && p.dead.is_none() && p.options.get("@input-disabled") != Some("on");
        if !self.panes.get(&pane).is_some_and(ok) { return; }
        let mut targets = vec![pane];
        if let Some(w) = self.window_of(pane).filter(|w| self.opt_window_on(Some(*w), "synchronize-panes")) {
            targets = self.windows[&w].panes();
        }
        for q in targets {
            if let Some(p) = self.panes.get(&q).filter(|p| ok(p)) { p.send(data.to_vec()); }
        }
    }

    /// input_key(): the bytes a key writes to a pane, or None where tmux returns -1 (send-keys
    /// then sends the key name as text).
    pub(crate) fn encode_pane_key(&self, pane: PaneId, k: &Key, raw: Vec<u8>) -> Option<Vec<u8>> {
        let p = self.panes.get(&pane)?;
        // internal function keys such as UserN are never written to a pane
        if matches!(k.code, KeyCode::User(_)) { return Some(Vec::new()); }
        let app = p.screen().application_cursor();
        let keypad = p.screen().application_keypad();
        // the pane's extended key mode (MODE_KEYS_EXTENDED, _2): set when the application asked
        // (or by extended-keys always at a reset), not read from the option for each key
        let mode = p.parser.callbacks().ext_keys;
        let csi_u = self.opt_server("extended-keys-format") == "csi-u";
        let char_code = |code: &KeyCode| -> Option<u32> {
            Some(match code {
                KeyCode::Char(c) => *c as u32,
                KeyCode::Space => 32,
                KeyCode::Enter => 13,
                KeyCode::Tab => 9,
                KeyCode::Escape => 27,
                _ => return None,
            })
        };
        let mut mapped = k.clone();
        let mut remapped = false;
        if k.code == KeyCode::BSpace {
            let option = self.opt_server("backspace");
            if k.mods.is_empty() {
                // an unmodified BSpace writes one byte, and only for a plain or Ctrl key
                return Some(backspace_byte(&option).map(|b| vec![b]).unwrap_or_default());
            }
            if let Some(mut backspace) = backspace_key(&option) {
                // key = newkey | modifiers: BSpace itself has no sequence, a character goes
                // through the usual modes, and a special key found only without the Meta
                // (no implied Meta) gets ESC before its unmodified form
                if backspace.code == KeyCode::BSpace && option.eq_ignore_ascii_case("BSpace") {
                    return Some(Vec::new());
                }
                backspace.mods.ctrl |= k.mods.ctrl;
                backspace.mods.meta |= k.mods.meta;
                backspace.mods.shift |= k.mods.shift;
                if backspace.code != KeyCode::BSpace {
                    if let Some(cp) = char_code(&backspace.code) {
                        return Some(keys::input_key_char(cp, false, backspace.mods, mode, csi_u));
                    }
                    let meta = std::mem::replace(&mut backspace.mods.meta, false);
                    let mut out = if meta { vec![0x1b] } else { Vec::new() };
                    out.extend(keys::encode_key_mode(&backspace, app, keypad));
                    return Some(out);
                }
                mapped = backspace;
                remapped = true;
            }
        } else if k.code == KeyCode::BTab {
            // KEYC_BTAB: S-Tab in extended mode 2, else ESC [ Z without modifiers
            return Some(keys::input_key_char(9, true, k.mods, mode, csi_u));
        } else if let Some((cp, btab, mods)) = keys::extended_input(&raw) {
            // a key the terminal sent in extended form: encode the exact key tmux parsed, so
            // C-i, C-m, C-@, C-[ and C-S-a stay distinct from Tab, Enter, C-Space, Escape, C-a
            return Some(keys::input_key_char(cp, btab, mods, mode, csi_u));
        }
        let k = &mapped;
        let extended = if mode > 0 { keys::encode_extended(k, mode >= 2, csi_u) } else { None };
        let special = matches!(k.code, KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
            | KeyCode::Insert | KeyCode::Delete | KeyCode::PageUp | KeyCode::PageDown | KeyCode::F(_));
        Some(if let Some(e) = extended {
            e
        } else if k.code == KeyCode::BSpace && k.mods.ctrl {
            // DEL has no Ctrl form in standard mode (input_key_vt10x returns -1)
            return None;
        } else if remapped || matches!(k.code, KeyCode::KP(_)) {
            keys::encode_key_mode(k, app, keypad)
        } else if special && k.mods.is_empty() {
            keys::encode_key(k, app)
        } else if special && raw.first() == Some(&0x1b) {
            // a modified special key is sent in tmux's own xterm form (M-Up from CSI 1;9 A is
            // CSI 1;3 A); ESC before an unmodified one is a Meta without the implied flag
            let mut plain = k.clone();
            if raw.starts_with(b"\x1b\x1b") && plain.mods.meta {
                plain.mods.meta = false;
                let mut out = vec![0x1b];
                out.extend(keys::encode_key_mode(&plain, app, keypad));
                out
            } else {
                keys::encode_key_mode(&plain, app, keypad)
            }
        } else if raw.starts_with(b"\x1b[") && (raw.ends_with(b"u") || raw.starts_with(b"\x1b[27;")) {
            // the outer terminal sent an extended key but this pane didn't ask for them
            keys::encode_key(k, app)
        } else {
            raw
        })
    }

    /// server_client_handle_key: what cannot wait is done now, the rest of the key goes
    /// in the client's queue (cmdq_append), after any commands of earlier keys.
    pub(super) fn handle_key(&mut self, cid: ClientId, k: Key, raw: Vec<u8>) {
        if let Some((k, raw)) = self.handle_key_now(cid, k, raw, false) {
            self.client_append(cid, ClientItem::Key(k, raw));
        }
    }

    /// The part of server_client_handle_key0 that cannot wait in a queue (which may be
    /// blocked): the message, the overlay, the active pane (dead, modal cancel,
    /// capture-keys), the window's menu, the status prompt and a pane prompt. Returns the
    /// key when none of them took it; it then goes to handle_key_queued
    /// (server_client_key_callback), which send-keys -K runs only when its queue reaches
    /// the key, so a menu or prompt a binding opened in between does not get it.
    /// A key send-keys -K `sent` has KEYC_SENT, so it is never the Escape or C-c that
    /// closes a popup or a modal pane (popup_key_cb and the modal check compare the
    /// whole key): those go to the popup's program or the pane.
    pub(super) fn handle_key_now(&mut self, cid: ClientId, k: Key, raw: Vec<u8>, sent: bool) -> Option<(Key, Vec<u8>)> {
        // (the paste start and end keys of handle_paste)
        let marker = match &k.code { c if *c == PASTE_START => Some(true), c if *c == PASTE_END => Some(false), _ => None };
        let name = match marker { Some(true) => "PasteStart".into(), Some(false) => "PasteEnd".into(), None => keys::key_name(&k) };
        let c = self.clients.get(&cid)?;
        let menu = if c.overlay.is_none() { self.client_window(cid).filter(|w| self.menus.contains_key(w)) } else { None };
        if c.is_readonly() && (c.prompt.is_some() || c.overlay.is_some() || menu.is_some()) { return None; }
        // a key clears a message (and is then handled), unless display -N
        if c.message.is_some() && !c.is_readonly() {
            if c.message_ignore_keys { return None; }
            let c = self.clients.get_mut(&cid).unwrap();
            c.message = None;
            c.dirty = true;
        }
        // server_client_handle_key0 order: the overlay, the active pane (dead, modal
        // cancel, capture-keys), the window's menu, the status prompt, then a pane prompt
        let Some(c) = self.clients.get(&cid) else { return None };
        match &c.overlay {
            Some(Overlay::Popup(p)) => {
                // popup_key_cb: Escape and C-c close the popup unless -E leaves them
                // to its running command; after the command exits only they (or with
                // -k any key) do
                let (pane, close_on_exit, any_key) = (p.pane, p.close_on_exit || p.close_exit_zero, p.close_any_key);
                let exited = self.panes.get(&pane).is_none_or(|x| x.dead.is_some());
                let cancel = !sent && (name == "Escape" || name == "C-c");
                // (-k: any key but the paste start and end, KEYC_IS_PASTE)
                if ((!close_on_exit || exited) && cancel) || (exited && any_key && marker.is_none()) {
                    self.close_popup(cid, pane);
                    return None;
                }
                if !exited {
                    match marker {
                        Some(start) => self.send_paste_marker(cid, pane, start),
                        None => self.send_key_to_pane(cid, pane, &k, raw),
                    }
                }
                return None;
            }
            None => {}
        }
        if self.client_session(cid).is_none() {
            return None;
        }
        // a modal pane closes on Escape or C-c with -D; with -K it gets every key, the
        // prefix included, unless it is in a mode (tmux server_client_handle_key, not for
        // read-only clients)
        if let Some(pane) = self.client_active_pane(cid).filter(|_| !self.client_is_readonly(Some(cid))) {
            // (not the paste start or end key: server_client_handle_dead_key)
            if marker.is_none() && self.dead_key(pane) {
                return None;
            }
            let fl = self.panes.get(&pane).map_or(0, |p| p.fflags);
            let modal = self.window_of(pane).is_some_and(|w| self.windows[&w].modal == Some(pane));
            if modal && !sent && fl & super::float::CLOSE_ON_CANCEL != 0 && (name == "Escape" || name == "C-c") {
                self.kill_pane(pane);
                return None;
            }
            if fl & super::float::CAPTURE_KEYS != 0 && self.panes.get(&pane).is_some_and(|p| p.mode.is_none() && p.dead.is_none()) {
                match marker {
                    Some(start) => self.send_paste_marker(cid, pane, start),
                    None => self.send_key_to_pane(cid, pane, &k, raw),
                }
                return None;
            }
        }
        // the menu of the client's window comes before the prompt (server_client_handle_menu_key)
        if let Some(wid) = menu {
            let action = match self.menus.get_mut(&wid) {
                Some(m) => m.key(&name),
                None => MenuAction::None,
            };
            self.menu_action(cid, wid, action);
            return None;
        }
        if self.clients[&cid].prompt.is_some() {
            let prompt_key = match k.code { KeyCode::KP('\r') => Key::plain(KeyCode::Enter), KeyCode::KP(ch) => Key::plain(KeyCode::Char(ch)), _ => k.clone() };
            if self.prompt_key(cid, &prompt_key, &name) {
                return None;
            }
        }
        // a pane prompt in the window; read-only clients skip it (server_client_handle_key)
        if let Some(pane) = self.pane_prompt_target(cid).filter(|_| !self.client_is_readonly(Some(cid))) {
            if self.pane_prompt_key(cid, pane, &k, &name) {
                return None;
            }
        }
        // a copy-mode jump waiting for its character (tmux's command-prompt -1 is a status
        // prompt, which takes the key here)
        if let Some(pane) = self.client_active_pane(cid) {
            if matches!(self.panes.get(&pane).and_then(|p| p.mode.as_ref()), Some(PaneMode::Copy(cm)) if cm.waiting_for_key()) {
                // (a single key prompt ignores the paste start and end keys)
                if marker.is_none() {
                    self.copy_key(pane, Some(cid), &name);
                }
                return None;
            }
        }
        Some((k, raw))
    }

    /// handle_key_now for a mouse key send-keys -K sent by name (MouseDown1Pane): its
    /// mouse event is not valid (no position), so a popup or a menu takes it without
    /// doing anything, a status prompt does not want it and a pane prompt drops it.
    /// True when the key was taken.
    pub(super) fn handle_mouse_key_now(&mut self, cid: ClientId) -> bool {
        let Some(c) = self.clients.get(&cid) else { return true };
        let menu = if c.overlay.is_none() { self.client_window(cid).filter(|w| self.menus.contains_key(w)) } else { None };
        if c.is_readonly() && (c.prompt.is_some() || c.overlay.is_some() || menu.is_some()) { return true; }
        if c.message.is_some() && !c.is_readonly() {
            if c.message_ignore_keys { return true; }
            let c = self.clients.get_mut(&cid).unwrap();
            c.message = None;
            c.dirty = true;
        }
        if self.clients[&cid].overlay.is_some() || self.client_session(cid).is_none() {
            return true;
        }
        if let Some(wid) = menu {
            self.menu_action(cid, wid, MenuAction::None);
            return true;
        }
        self.pane_prompt_target(cid).is_some() && !self.client_is_readonly(Some(cid))
    }

    /// The default key table (server_client_get_key_table): the session's key-table.
    pub(super) fn default_key_table(&self, sid: SessId) -> String {
        let t = self.opt_session(Some(sid), "key-table");
        if t.is_empty() { "root".into() } else { t }
    }

    /// server_client_key_callback: assume-paste, a pane capturing all keys, the key tables
    /// and the prefix, then the pane or its mode. A key send-keys -K `sent` (KEYC_SENT) is
    /// never taken for a paste. `k` is None for a mouse key sent by name, whose event
    /// has no position.
    pub(super) fn handle_key_queued(&mut self, cid: ClientId, k: Option<Key>, name: &str, raw: Vec<u8>, sent: bool) {
        let name = name.to_string();
        let mouse = k.is_none();
        if !self.clients.contains_key(&cid) { return; }
        let Some(sid) = self.client_session(cid) else { return };
        let pane = self.client_active_pane(cid);
        // mouse keys go to the pane when the mouse is off (an event without a position
        // reaches none)
        if mouse && !self.opt_session_on(Some(sid), "mouse") {
            return;
        }
        let now = Instant::now();
        let threshold = self.opt_session_num(Some(sid), "assume-paste-time").max(0) as u64;
        let c = self.clients.get_mut(&cid).unwrap();
        let mut paste = false;
        if !sent && !mouse {
            let quick = threshold > 0 && !c.features.has("bpaste") && c.input_state.key_at.is_some_and(|t| now.duration_since(t) < Duration::from_millis(threshold));
            paste = quick && c.input_state.assume_paste;
            if quick && !c.input_state.assume_paste { c.input_state.guess_at = Some(now); }
            c.input_state.assume_paste = quick;
        }
        c.input_state.key_at = Some(now);
        if paste {
            // paste_key: the bytes as they came (window_pane_paste), never to a pane in a mode
            // and without the dead-key check
            if let Some(pane) = self.input_target(cid) { self.paste_bytes(cid, pane, &raw); }
            return;
        }
        // a pane capturing all keys (new-pane -K) gets them before the key tables
        if let (Some(p), Some(k)) = (pane, k.as_ref()) {
            if self.panes.get(&p).is_some_and(|x| x.fflags & super::float::CAPTURE_KEYS != 0 && x.dead.is_none() && x.mode.is_none()) {
                if !self.dead_key(p) {
                    self.send_key_to_pane(cid, p, k, raw);
                }
                return;
            }
        }
        // the key table: a pane's mode table (copy mode) while the client is in its
        // default table, else the client's
        let default_table = self.default_key_table(sid);
        let client_table = self.clients[&cid].key_table.clone();
        let mode_table = pane.and_then(|p| self.mode_key_table(p));
        let mut table = if client_table == default_table { mode_table.clone().unwrap_or(client_table) } else { client_table };
        let mut first = table.clone();
        let mut repeating = self.clients[&cid].repeat_until.is_some_and(|d| d > Instant::now());
        let prefix_ms = self.opt_server_num("prefix-timeout").max(0) as u64;
        let reset = |srv: &mut Self, t: &str| {
            let c = srv.clients.get_mut(&cid).unwrap();
            c.set_key_table(t);
            c.repeat_until = None;
            c.dirty = true;
        };
        loop {
            // table_changed: the prefix always switches to the prefix table, from any other
            if !mouse && table != "prefix" {
                let prefix = self.opt_session(Some(sid), "prefix");
                let prefix2 = self.opt_session(Some(sid), "prefix2");
                let is_prefix = |p: &str| p != "None" && keys::parse_key_name(p).is_some_and(|pk| keys::key_name(&pk) == name);
                if is_prefix(&prefix) || is_prefix(&prefix2) {
                    reset(self, "prefix");
                    return;
                }
            }
            let was_repeating = repeating;
            let found = self.bindings.get(&table).and_then(|t| t.get(&name)).map(|b| (b.cmd.clone(), b.repeat, name.clone()));
            // prefix-timeout counts from when the table was set, also by switch-client -T
            if table == "prefix" && prefix_ms > 0 && !(repeating && found.as_ref().is_some_and(|b| b.1))
                && self.clients[&cid].input_state.table_at.is_some_and(|t| now.duration_since(t) > Duration::from_millis(prefix_ms)) {
                reset(self, &default_table);
                table = default_table.clone();
                first = table.clone();
                continue;
            }
            // no match: the Any key of the same table (KEYC_ANY, try_again)
            // (the binding's key, KEYC_ANY, is what repeat compares with c->last_key)
            let found = found.or_else(|| self.bindings.get(&table).and_then(|t| t.get("Any")).map(|b| (b.cmd.clone(), b.repeat, "Any".to_string())));
            match found {
                Some((_, false, _)) if repeating => {
                    // a binding that does not repeat ends the repeat; the key is looked up
                    // again from the default table, where it may be the prefix
                    reset(self, &default_table);
                    repeating = false;
                    table = default_table.clone();
                    first = table.clone();
                }
                Some((cmd, repeat, bkey)) => {
                    let mut ms = self.opt_session_num(Some(sid), "repeat-time").max(0) as u64;
                    let initial = self.opt_session_num(Some(sid), "initial-repeat-time").max(0) as u64;
                    if ms > 0 && initial > 0 && (!repeating || self.clients[&cid].input_state.last_key != bkey) { ms = initial; }
                    let c = self.clients.get_mut(&cid).unwrap();
                    c.input_state.last_key = bkey;
                    if repeat && ms > 0 && table != default_table {
                        c.repeat_until = Some(Instant::now() + Duration::from_millis(ms));
                    } else {
                        c.repeat_until = None;
                        c.set_key_table(&default_table);
                    }
                    c.dirty = true;
                    // copy mode's own table: copy mode runs it (its default bindings
                    // directly)
                    match pane.filter(|_| mode_table.as_deref() == Some(table.as_str())) {
                        Some(p) => self.copy_key(p, Some(cid), &name),
                        None => self.run_key_string(cid, &cmd, pane, &name),
                    }
                    return;
                }
                // mouse movement is only reported when asked for: it leaves no table
                None if mouse && name.contains("MouseMove") => break,
                None if table != default_table || repeating => {
                    // not found: back to the default table and try again
                    reset(self, &default_table);
                    table = default_table.clone();
                    if repeating {
                        first = table.clone();
                    }
                    repeating = false;
                }
                // found in neither: a key that came through another table is dropped
                None if first != table && !was_repeating => {
                    reset(self, &default_table);
                    return;
                }
                None => break,
            }
        }
        // forward_key: to the pane (or its mode)
        let Some(pane) = pane else { return };
        // a mouse key without a position reaches no pane or mode
        let Some(k) = k else { return };
        // before the read-only check, so read-only clients may dismiss it too
        if self.dead_key(pane) { return; }
        match self.panes.get(&pane).and_then(|p| p.mode.as_ref()) {
            // copy and view mode take keys only through their key table (window-copy.c
            // has no .key callback), so a key that got here is dropped
            Some(PaneMode::Copy(_)) => {}
            Some(_) => self.mode_key(pane, Some(cid), &name),
            None => self.send_key_to_pane(cid, pane, &k, raw),
        }
    }

    /// A send-keys -K key the queue reached (server_client_key_callback): the key tables
    /// and the pane, as the client typing it, encoded for its pane as it is now (menus and
    /// prompts had it at send-keys time, handle_key_now). Returns the commands of the
    /// binding it ran, which go in the queue after it.
    pub(super) fn key_event(&mut self, ev: KeyEvent) -> Vec<Vec<String>> {
        if !self.clients.contains_key(&ev.client) {
            return Vec::new();
        }
        let app = self.panes.get(&ev.pane).is_some_and(|p| p.screen().application_cursor());
        let saved = self.key_origin.replace(ev.origin);
        let saved_capture = self.key_capture.replace(Vec::new());
        match ev.mouse {
            Some(name) => self.handle_key_queued(ev.client, None, &name, Vec::new(), true),
            None => {
                let raw = keys::encode_key(&ev.key, app);
                let name = keys::key_name(&ev.key);
                self.handle_key_queued(ev.client, Some(ev.key), &name, raw, true);
            }
        }
        self.key_origin = saved;
        std::mem::replace(&mut self.key_capture, saved_capture).unwrap_or_default()
    }

    /// Runs a key binding's commands for a client: the message log names the key (or,
    /// for send-keys -K, the client that sent it). Under send-keys -K the commands go
    /// into its queue as a group (KEY_GROUP marks) that runs for this client, its pane
    /// and the key, as key_bindings_dispatch's new item and state.
    pub fn run_key_string(&mut self, cid: ClientId, cmd: &str, pane: Option<PaneId>, key: &str) {
        match self.parse_string(cmd) {
            Ok(cmds) if self.key_capture.is_some() => {
                // send-keys without -K (key_bindings_dispatch with no event): the queue's
                // own client and no key, only the target pane
                let (client, key) = if self.key_plain { (String::new(), String::new()) } else { (cid.to_string(), key.to_string()) };
                let capture = self.key_capture.as_mut().unwrap();
                capture.push(vec![KEY_GROUP.clone(), "1".into(), client, pane.map(|p| p.to_string()).unwrap_or_default(), key]);
                capture.extend(cmds);
                capture.push(vec![KEY_GROUP.clone(), "0".into()]);
            }
            Ok(cmds) => {
                let ctx = Ctx { client: Some(cid), pane, from_key: true, key: Some(key.to_string()), log_client: self.key_origin, ..Ctx::default() };
                self.run_queue(Queue { cmds: cmds.into(), ctx , soft: 0 });
            }
            Err(e) => self.set_message(cid, &e),
        }
    }

    /// Runs a command string for a client (prompts, menus, modes): cmdq_append to the
    /// client's queue, so the commands wait behind a command of the client that waits
    /// (a prompt a key opened), and while a queue is running (send-keys -K handing a menu
    /// or a prompt its key) until that queue is done or waits.
    pub fn run_string(&mut self, cid: ClientId, cmd: &str, pane: Option<PaneId>) {
        // cmd_parse_from_string: the whole list is checked before any of it runs
        let parsed = self.parse_string(cmd);
        self.run_parsed(cid, parsed, pane);
    }

    /// [`Server::run_string`] for commands already parsed (or the parse's error).
    fn run_parsed(&mut self, cid: ClientId, parsed: Result<Vec<Vec<String>>, String>, pane: Option<PaneId>) {
        match parsed {
            Ok(cmds) => {
                let ctx = Ctx { client: Some(cid), pane, from_key: true, ..Ctx::default() };
                self.client_append(cid, ClientItem::Queue(Queue { cmds: cmds.into(), ctx , soft: 0 }));
            }
            // cmdq_get_error: an item of the client's queue
            Err(e) => self.client_append(cid, ClientItem::Error(e)),
        }
    }

    /// A prompt's answer (cmd_command_prompt_callback, cmd_confirm_before_callback):
    /// with a command waiting for the prompt, its commands go straight after it, with
    /// its state (client, key) and as a group of their own (cmdq_insert_after with
    /// cmdq_get_state); else they are appended to the client's queue with none.
    fn run_prompt_answer(&mut self, cid: ClientId, pr: &Prompt, cmd: &str) {
        let parsed = self.parse_string(cmd);
        self.run_prompt_commands(cid, pr, parsed);
    }

    /// The commands of a prompt's answer: a string template filled in and parsed, or
    /// a { } block's commands with the answers in their values (args_make_commands).
    fn run_prompt_template(&mut self, cid: ClientId, pr: &Prompt, answers: &[String]) {
        if pr.block {
            let sh = self.answer_shells();
            let cmds = fill_block(&pr.template, answers, sh.any, sh.job);
            self.run_prompt_commands(cid, pr, cmds);
        } else {
            match fill_template(&pr.template, answers, self.answer_shells()) {
                Ok(cmd) => self.run_prompt_answer(cid, pr, &cmd),
                Err(e) => self.run_prompt_commands(cid, pr, Err(e)),
            }
        }
    }

    fn run_prompt_commands(&mut self, cid: ClientId, pr: &Prompt, parsed: Result<Vec<Vec<String>>, String>) {
        let Some(ctx) = self.prompt_waits.iter().find(|w| w.1 == pr.id).map(|w| w.3.ctx.clone()) else {
            self.run_parsed(cid, parsed, pr.pane);
            return;
        };
        match parsed {
            Ok(cmds) => {
                let pane = pr.pane.or(ctx.pane).map(|p| p.to_string()).unwrap_or_default();
                let client = ctx.client.map(|c| c.to_string()).unwrap_or_default();
                let mut group = vec![vec![KEY_GROUP.clone(), "1".into(), client, pane, ctx.key.clone().unwrap_or_default()]];
                group.extend(cmds);
                group.push(vec![KEY_GROUP.clone(), "0".into()]);
                self.prompt_inserts.entry(pr.id).or_default().extend(group);
            }
            // cmdq_get_error: appended to the client's queue
            Err(e) => self.client_append(cid, ClientItem::Error(e)),
        }
    }

    /// The window a client shows.
    pub fn client_window(&self, cid: ClientId) -> Option<WinId> {
        self.client_session(cid).and_then(|s| self.sessions.get(&s)).map(|s| s.active_window())
    }

    /// The rows above the window on a client (status lines at the top).
    pub fn client_window_top(&self, cid: ClientId) -> u16 {
        let Some(c) = self.clients.get(&cid) else { return 0 };
        if c.session.is_none() || self.opt_session(c.session, "status-position") != "top" {
            return 0;
        }
        self.status_lines(c.session).min(c.size.1.saturating_sub(1))
    }

    /// A menu belongs to a window (tmux 3.8 w->menu): every client showing the window sees
    /// and uses it, and opening another menu there replaces it (menu_close).
    /// menu_display / menu_close: a window with a menu has no focused pane, so focus is
    /// updated whenever one opens or closes (a new menu first closes the old one).
    pub fn set_window_menu(&mut self, wid: WinId, menu: Option<super::modes::Menu>) {
        let live = self.windows.contains_key(&wid);
        if self.menus.remove(&wid).is_some() && live {
            self.update_window_focus(wid);
        }
        if let Some(m) = menu {
            self.menus.insert(wid, m);
            if live {
                self.update_window_focus(wid);
            }
        }
        if live {
            self.mark_window_dirty(wid);
        }
    }

    pub(crate) fn menu_action(&mut self, cid: ClientId, wid: WinId, action: MenuAction) {
        match action {
            MenuAction::None => {
                if self.windows.contains_key(&wid) {
                    self.mark_window_dirty(wid);
                }
            }
            MenuAction::Cancel => self.set_window_menu(wid, None),
            MenuAction::Run(cmd) => {
                let pane = self.menus.get(&wid).and_then(|m| m.pane).filter(|p| self.panes.contains_key(p));
                self.set_window_menu(wid, None);
                self.run_string(cid, &cmd, pane.or_else(|| self.client_active_pane(cid)));
            }
        }
    }

    /// A key for a pane that is in a mode (copy, tree, clock). Read-only clients may use
    /// copy mode (but not copy), not the other modes.
    pub fn mode_key(&mut self, pane: PaneId, cid: Option<ClientId>, name: &str) {
        let copy = matches!(self.panes.get(&pane).and_then(|p| p.mode.as_ref()), Some(PaneMode::Copy(_)));
        if self.client_is_readonly(cid) && !copy { return; }
        let Some(p) = self.panes.get_mut(&pane) else { return };
        match &mut p.mode {
            Some(PaneMode::Clock) => self.exit_mode(pane),
            Some(PaneMode::Tree(_) | PaneMode::Switch(_) | PaneMode::Panes(_)) => {
                let (w, h) = (p.size.0 as usize, p.size.1 as usize);
                let Some(mut mode) = p.mode.take() else { return };
                let action = match &mut mode {
                    PaneMode::Tree(t) => {
                        let a = t.key(name, self, w, h);
                        for (ty, line) in t.take_history() {
                            self.add_prompt_history(&line, ty);
                        }
                        a
                    }
                    PaneMode::Switch(s) => s.key(name, self, w, h),
                    PaneMode::Panes(m) => m.key(name, self),
                    _ => ModeAction::None,
                };
                if let Some(p) = self.panes.get_mut(&pane) {
                    p.mode = Some(mode);
                }
                self.mode_action(pane, cid, action);
            }
            Some(PaneMode::Copy(_)) => self.copy_key(pane, cid, name),
            None => {}
        }
    }

    /// The client a mode's commands run for: the one whose key or click it was (tmux passes
    /// that client to mode_tree_run_command and friends), else the one that opened the mode.
    fn mode_client(&self, pane: PaneId, cid: Option<ClientId>) -> Option<ClientId> {
        let attached = |c: &ClientId| self.clients.get(c).is_some_and(|c| c.attached);
        if let Some(c) = cid.filter(attached) {
            return Some(c);
        }
        let opened = match self.panes.get(&pane).and_then(|p| p.mode.as_ref()) {
            Some(PaneMode::Tree(t)) => t.client,
            Some(PaneMode::Switch(s)) => s.client,
            Some(PaneMode::Panes(m)) => m.client,
            _ => None,
        };
        opened.filter(attached).or(cid)
    }

    fn run_mode_command(&mut self, client: Option<ClientId>, cmd: &str, pane: PaneId) {
        let pane = self.panes.contains_key(&pane).then_some(pane);
        match client {
            Some(c) => self.run_string(c, cmd, pane),
            None => match self.parse_string(cmd) {
                Ok(cmds) => self.run_queue(Queue { cmds: cmds.into(), ctx: Ctx { pane, ..Ctx::default() }, soft: 0 }),
                Err(e) => log(&format!("mode command: {e}")),
            },
        }
    }

    /// Carry out what a mode asked for after a key or a click.
    pub fn mode_action(&mut self, pane: PaneId, cid: Option<ClientId>, action: ModeAction) {
        let client = self.mode_client(pane, cid);
        match action {
            ModeAction::None => {}
            ModeAction::Exit => self.exit_mode(pane),
            ModeAction::Run(cmd) => {
                // choosing a pane in panes mode unzooms its window (window_panes_key)
                if matches!(self.panes.get(&pane).and_then(|p| p.mode.as_ref()), Some(PaneMode::Panes(_))) {
                    if let Some(wid) = self.window_of(pane) {
                        self.unzoom_window(wid);
                    }
                }
                // like tmux, the mode is gone before the queued command runs
                self.exit_mode(pane);
                self.run_mode_command(client, &cmd, pane);
            }
            ModeAction::RunKeep(cmd) => self.run_mode_command(client, &cmd, pane),
            ModeAction::Error(e) => match client {
                Some(c) => self.run_parsed(c, Err(e), None),
                None => log(&format!("mode command: {e}")),
            },
            ModeAction::Menu { title, items, x, y } => {
                // the items send their key to the mode (mode_tree_menu_callback)
                let mode = self.panes.get(&pane).and_then(|p| p.mode.as_ref()).map(|m| m.name().to_string()).unwrap_or_default();
                let items: Vec<super::modes::MenuItem> = items
                    .into_iter()
                    .map(|(name, key, send)| super::modes::MenuItem {
                        name: name.to_string(),
                        key: key.to_string(),
                        cmd: if name.is_empty() { String::new() } else { format!("if-shell -F -t '%{pane}' '#{{==:#{{pane_mode}},{mode}}}' \"send-keys -t '%{pane}' {send}\"") },
                    })
                    .collect();
                if let (Some(wid), Some(r)) = (self.window_of(pane), self.pane_rect(pane)) {
                    let (w, h) = super::modes::Menu::box_size(&title, &items);
                    let x = if x >= w / 2 { x - w / 2 } else { 0 } + r.x;
                    let y = y + r.y;
                    // menu_prepare: inside the window
                    let (wsx, wsy) = self.windows.get(&wid).map_or((0, 0), |w| (w.sx, w.sy));
                    let fit = |p: u16, size: u16, total: u16| if size >= total { 0 } else if p + size > total { total - size } else { p };
                    let menu = super::modes::Menu::at(title, items, fit(x, w, wsx), fit(y, h, wsy), Some(pane), false, false, None);
                    self.set_window_menu(wid, Some(menu));
                }
            }
            ModeAction::Edit { value, command, buffer } => {
                if let Some(c) = client {
                    if let Err(e) = self.open_editor(c, &value, command, buffer) {
                        self.set_message(c, &e);
                    }
                }
            }
        }
        if let Some(wid) = self.window_of(pane) {
            self.mark_window_dirty(wid);
        }
    }

    /// spawn_editor: edit `value` in the editor option's program, in a modal floating pane
    /// 9/10 of the client's window in its middle; `command` runs with %% replaced by the
    /// text when the editor exits successfully.
    pub fn open_editor(&mut self, cid: ClientId, value: &[u8], command: String, buffer: Option<String>) -> Result<(), String> {
        let wid = self.client_window(cid).ok_or("no current window")?;
        let w = &self.windows[&wid];
        if w.modal.is_some() {
            return Err("window already has a modal pane".into());
        }
        let (wsx, wsy, active) = (w.sx, w.sy, w.active);
        let text: &[u8] = if value.is_empty() { b"\n" } else { value };
        let path = editor_file(text).map_err(|e| format!("editor: {e}"))?;
        let editor = self.opt_server("editor");
        // the editor option names the program (notepad.exe on Windows); the file is one argument
        let quoted = if cfg!(windows) { format!("\"{}\"", path.display()) } else { crate::cmdparse::quote(&path.display().to_string()) };
        let sid = self.client_session(cid);
        let shell = self.opt_session(sid, "default-shell");
        let shell = if shell.is_empty() { procinfo::default_shell() } else { shell };
        let cmd = editor_command(&editor, &quoted, cfg!(windows) && procinfo::is_powershell(&shell));
        // the layout cell: sx * 9 / 10 by sy * 9 / 10 at sx / 2 - w / 2, sy / 2 - h / 2
        // (new-pane's -x/-y count the border, -X/-Y place the border)
        let (pw, ph) = (((wsx as u32 * 9 / 10) as i64).max(1), ((wsy as u32 * 9 / 10) as i64).max(1));
        let (xoff, yoff) = (wsx as i64 / 2 - pw / 2, wsy as i64 / 2 - ph / 2);
        let b = i64::from(self.opt_window(Some(wid), "pane-border-lines") != "none");
        let args: Vec<String> = [
            "-O".to_string(), "-x".into(), (pw + 2 * b).to_string(), "-y".into(), (ph + 2 * b).to_string(),
            "-X".into(), (xoff - b).to_string(), "-Y".into(), (yoff - b).to_string(),
            "-c".into(), std::env::temp_dir().display().to_string(), "-t".into(), format!("%{active}"), cmd,
        ].into();
        let (bools, vals) = cmds::command_flags("new-pane");
        let o = crate::opts::getopt(&args, bools, vals)?;
        let mut ctx = Ctx { client: Some(cid), ..Default::default() };
        if let Err(e) = self.cmd_new_pane("new-pane", &o, &[], &mut ctx) {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        let Some(pane) = ctx.pane else { return Ok(()) };
        if let Some(p) = self.panes.get_mut(&pane) {
            p.options.set("remain-on-exit", "off".into());
        }
        self.editors.insert(pane, (cid, super::modes::PopupEditor { path, command, buffer }));
        Ok(())
    }

    /// The editor pane exited (spawn_editor_finish): nothing for a failed editor or an
    /// empty file, else the paste buffer gets the text (window_buffer_edit_close_cb) or the
    /// command runs with it, less a final newline.
    pub fn editor_done(&mut self, cid: ClientId, ed: super::modes::PopupEditor, status: Option<i32>) {
        let data = std::fs::read(&ed.path).ok();
        let _ = std::fs::remove_file(&ed.path);
        let Some(mut data) = data.filter(|d| status.unwrap_or(0) == 0 && !d.is_empty()) else { return };
        let strip = |d: &mut Vec<u8>| {
            if d.last() == Some(&b'\n') {
                d.pop();
                if cfg!(windows) && d.last() == Some(&b'\r') {
                    d.pop();
                }
            }
        };
        if let Some(name) = &ed.buffer {
            // only a buffer that is still there; its old final newline decides
            let Some(b) = self.buffers.iter_mut().find(|b| &b.name == name) else { return };
            if !b.data.is_empty() && b.data.last() != Some(&b'\n') {
                strip(&mut data);
            }
            if !data.is_empty() {
                b.data = data;
            }
            return;
        }
        strip(&mut data);
        match fill_template(&ed.command, &[String::from_utf8_lossy(&data).into_owned()], self.answer_shells()) {
            Ok(cmd) => self.run_string(cid, &cmd, None),
            Err(e) => self.run_parsed(cid, Err(e), None),
        }
    }

    // ---- prompt ----------------------------------------------------------------------

    /// A key for the status line prompt (status_prompt_key): false when the
    /// prompt does not use it and it goes on to the key tables.
    fn prompt_key(&mut self, cid: ClientId, k: &Key, name: &str) -> bool {
        let Some(c) = self.clients.get_mut(&cid) else { return false };
        let Some(mut pr) = c.prompt.take() else { return false };
        c.dirty = true;
        let result = self.prompt_process(cid, &mut pr, k, name);
        self.prompt_done_key(cid, pr);
        matches!(result, PromptResult::Handled | PromptResult::Close)
    }

    /// Puts an edited prompt back unless it finished, and follows a copy-mode
    /// incremental search.
    fn prompt_done_key(&mut self, cid: ClientId, pr: Prompt) {
        let incremental = pr.copy_incremental;
        let text = (!pr.closed).then(|| pr.input.iter().collect::<String>());
        if let Some(host) = pr.host {
            if !pr.closed {
                if let Some(p) = self.panes.get_mut(&host) {
                    if p.prompt.is_none() { p.prompt = Some(pr); }
                }
            } else {
                // window_pane_clear_prompt
                let ty = PROMPT_TYPES.get(pr.history_type).copied().unwrap_or("command");
                self.fire_pane_prompt("pane-prompt-closed", host, ty);
            }
            if let Some(wid) = self.window_of(host) { self.mark_window_dirty(wid); }
        } else if !pr.closed {
            if let Some(c) = self.clients.get_mut(&cid) {
                if c.prompt.is_none() { c.prompt = Some(pr); }
                c.dirty = true;
            }
        }
        if let Some((pane, forward)) = incremental {
            // options copy mode reads as it runs (wrap-search, mode-keys, word-separators)
            self.copy_sync_options(pane);
            if let Some(PaneMode::Copy(cm)) = self.panes.get_mut(&pane).and_then(|p| p.mode.as_mut()) {
                match text {
                    Some(text) => cm.preview_search(&text, forward),
                    None => cm.finish_incremental_search(false),
                }
            }
            if let Some(wid) = self.window_of(pane) { self.mark_window_dirty(wid); }
        }
    }

    /// A prompt mouse event (prompt_mouse): button 1 presses on the prompt
    /// line move the cursor or pick an inline completion. Others are not used.
    pub(crate) fn prompt_mouse(&mut self, cid: ClientId, m: &keys::Mouse) -> bool {
        if m.kind != keys::MouseKind::Down || m.button != 1 || self.message_row(cid) != Some(m.y) {
            return false;
        }
        let Some(sid) = self.client_session(cid) else { return false };
        let cw = self.clients[&cid].size.0.max(1);
        let style_text = self.expand_time(self.current(&Ctx { client: Some(cid), ..Ctx::default() }), Some(cid), &self.opt_session(Some(sid), "message-style"));
        let (ax, aw) = super::render::message_area(cw, &style_text);
        let Some(mut pr) = self.clients.get_mut(&cid).and_then(|c| c.prompt.take()) else { return false };
        let used = self.prompt_mouse_at(cid, &mut pr, m.x as usize, ax, aw);
        if let Some(c) = self.clients.get_mut(&cid) {
            if c.prompt.is_none() { c.prompt = Some(pr); }
            c.dirty = true;
        }
        used
    }

    fn prompt_mouse_at(&mut self, cid: ClientId, pr: &mut Prompt, x: usize, ax: u16, aw: u16) -> bool {
        if x < ax as usize || x >= (ax + aw) as usize {
            return false;
        }
        let l = self.prompt_layout(cid, pr, ax, aw);
        if l.input_width == 0 {
            return true;
        }
        let pwidth = super::render::prompt_width(&pr.input) + pr.quote_next as usize;
        // prompt_mouse_complete: a click on one of the inline matches
        let (cx, cax, caw) = (l.cursor_x, l.content_x, l.content_width);
        if !pr.complete.is_empty() && pr.cursor == pr.input.len() && cx >= cax && cx - cax < caw && x >= cx {
            let avail = caw - (cx - cax);
            let clicked = x - cx;
            let width = format::width(&super::render::prompt_complete_display(&pr.complete)).min(avail);
            if clicked < width {
                let mut end = 0;
                for i in 0..pr.complete.len() {
                    let start = end + 1;
                    end = start + format::width(&pr.complete[i]);
                    if clicked >= start && clicked < end {
                        let replace = format!("{} ", pr.complete[i]);
                        if prompt_replace_word(pr, &replace) {
                            pr.complete.clear();
                        }
                        break;
                    }
                }
                return true;
            }
        }
        let target = if x <= l.input_x { l.input_offset } else { l.input_offset + x - l.input_x }.min(pwidth);
        let mut width = 0;
        let mut idx = 0;
        while idx < pr.input.len() && width < target {
            width += super::render::prompt_width(&pr.input[idx..idx + 1]);
            idx += 1;
        }
        if idx != pr.cursor {
            pr.cursor = idx;
            pr.complete.clear();
        }
        true
    }

    /// The prompt's input callback (cmd_command_prompt_callback,
    /// cmd_confirm_before_callback): `s` None cancels. True when the prompt closes.
    fn prompt_fire(&mut self, cid: ClientId, pr: &mut Prompt, s: Option<&str>, fire: Fire) -> bool {
        // the command-line client waiting for this prompt has gone: its queue (where the
        // commands would go) runs nothing more
        let gone = self.prompt_waits.iter().any(|(_, id, _, q)| *id == pr.id && q.ctx.reply.is_some_and(|r| !self.clients.contains_key(&r)));
        let s = s.filter(|_| !gone);
        let close = match pr.kind {
            PromptKind::Key => {
                if let Some(s) = s {
                    self.run_prompt_template(cid, pr, &[s.to_string()]);
                }
                true
            }
            PromptKind::Confirm => {
                let first = s.and_then(|s| s.chars().next());
                if first == Some(pr.confirm.0) || (first == Some('\r') && pr.confirm.1) {
                    let template = pr.template.clone();
                    self.run_prompt_answer(cid, pr, &template);
                }
                true
            }
            PromptKind::Command => match s {
                None => true,
                Some(_) if fire == Fire::Move => true,
                // a copy-mode search ends where the incremental search got to
                Some(_) if pr.copy_incremental.is_some() => fire == Fire::Close,
                Some(_) if fire == Fire::Close && pr.incremental => true,
                Some(s) => {
                    let mut argv = pr.answers.clone();
                    argv.push(s.to_string());
                    if fire == Fire::Close {
                        pr.answers.push(s.to_string());
                        if pr.answers.len() < pr.prompts.len() {
                            let (label, init) = pr.prompts[pr.answers.len()].clone();
                            pr.label = label;
                            pr.input = init.chars().collect();
                            pr.cursor = pr.input.len();
                            pr.history_pos = None;
                            pr.complete.clear();
                            return false;
                        }
                    }
                    self.run_prompt_template(cid, pr, &argv);
                    // an incremental prompt stays open for more typing
                    !pr.incremental
                }
            },
        };
        if close {
            pr.closed = true;
        }
        close
    }

    /// tmux 3.8 prompt_key: edit the prompt with a key, firing the callback.
    fn prompt_process(&mut self, cid: ClientId, pr: &mut Prompt, k: &Key, name: &str) -> PromptResult {
        pr.closed = false;
        // inline completion matches are dropped by any key (Tab rebuilds them)
        pr.complete.clear();
        if matches!(pr.kind, PromptKind::Key) {
            self.prompt_fire(cid, pr, Some(name), Fire::Close);
            pr.closed = true;
            return PromptResult::Close;
        }
        let k = &prompt_keypad_key(k);
        if pr.numeric {
            // a digit is typed, anything else runs the command and is then handled
            if let KeyCode::Char(d @ '0'..='9') = k.code {
                if k.mods.is_empty() {
                    return self.prompt_append(cid, pr, d);
                }
            }
            let s: String = pr.input.iter().collect();
            self.prompt_fire(cid, pr, Some(&s), Fire::Close);
            pr.closed = true;
            return PromptResult::NotHandled;
        }
        if pr.single_key || pr.quote_next || matches!(pr.kind, PromptKind::Confirm) {
            let Some(ch) = prompt_key_char(k) else { return PromptResult::Handled };
            pr.quote_next = false;
            return self.prompt_append(cid, pr, ch);
        }
        let vi = self.opt_session(self.client_session(cid), "status-keys") == "vi";
        let op = if vi { prompt_vi_op(pr, k) } else { prompt_emacs_op(k) };
        // prompt_check_move: cursor keys end an incremental prompt
        if pr.incremental && matches!(op, PromptOp::Up | PromptOp::Down | PromptOp::Page | PromptOp::Left | PromptOp::Right) {
            let s: String = pr.input.iter().collect();
            if self.prompt_fire(cid, pr, Some(&s), Fire::Move) {
                return PromptResult::Close;
            }
            return PromptResult::Move;
        }
        let seps: Vec<char> = self.opt_session(self.client_session(cid), "word-separators").chars().collect();
        let n = pr.input.len();
        let mut prefix = '=';
        let changed = match op {
            PromptOp::None | PromptOp::Page => false,
            PromptOp::Insert(ch) => return self.prompt_append(cid, pr, ch),
            PromptOp::Enter => {
                let input: String = pr.input.iter().collect();
                // every non-empty answer goes in the history of the prompt's type,
                // copy-mode searches too (prompt_add_history)
                if !input.is_empty() { self.add_prompt_history(&input, pr.history_type); }
                return self.prompt_done(cid, pr, Some(&input));
            }
            PromptOp::Cancel => return self.prompt_done(cid, pr, None),
            PromptOp::BSpace if pr.bspace_exit && n == 0 => return self.prompt_done(cid, pr, None),
            PromptOp::BSpace => {
                let moved = pr.cursor > 0;
                if moved {
                    pr.cursor -= 1;
                    pr.input.remove(pr.cursor);
                }
                moved
            }
            PromptOp::Delete => {
                if pr.cursor < n {
                    pr.input.remove(pr.cursor);
                }
                pr.cursor < n
            }
            PromptOp::Left => {
                pr.cursor = pr.cursor.saturating_sub(1);
                false
            }
            PromptOp::Right => {
                pr.cursor = (pr.cursor + 1).min(n);
                false
            }
            PromptOp::Home => {
                pr.cursor = 0;
                false
            }
            PromptOp::End => {
                pr.cursor = n;
                false
            }
            // C-u and C-k don't fill the C-y buffer in tmux; only C-w does
            PromptOp::ClearLine => {
                pr.input.clear();
                pr.cursor = 0;
                true
            }
            PromptOp::KillLine => {
                pr.input.truncate(pr.cursor);
                pr.cursor < n
            }
            PromptOp::DeleteWord => {
                let i = prompt_backward_word(&pr.input, pr.cursor, &seps);
                pr.copied = Some(pr.input.drain(i..pr.cursor).collect());
                pr.cursor = i;
                true
            }
            PromptOp::ForwardWord => {
                pr.cursor = prompt_forward_word(&pr.input, pr.cursor, false, &seps);
                true
            }
            PromptOp::ViForward(big) => {
                pr.cursor = prompt_forward_word(&pr.input, pr.cursor, true, if big { &[] } else { &seps });
                true
            }
            PromptOp::ViEnd(big) => {
                pr.cursor = prompt_end_word(&pr.input, pr.cursor, if big { &[] } else { &seps });
                true
            }
            PromptOp::BackWord(big) => {
                pr.cursor = prompt_backward_word(&pr.input, pr.cursor, if big { &[] } else { &seps });
                true
            }
            PromptOp::Up => {
                let history = self.prompt_history.get(pr.history_type).cloned().unwrap_or_default();
                let hist_len = history.len();
                if hist_len > 0 && pr.history_pos != Some(0) {
                    let pos = pr.history_pos.map(|p| p.min(hist_len).saturating_sub(1)).unwrap_or(hist_len - 1);
                    pr.history_pos = Some(pos);
                    pr.input = history[pos].chars().collect();
                    pr.cursor = pr.input.len();
                    true
                } else {
                    false
                }
            }
            PromptOp::Down => {
                // past the newest entry the line is empty (prompt_down_history)
                let history = self.prompt_history.get(pr.history_type).cloned().unwrap_or_default();
                let hist_len = history.len();
                match pr.history_pos.map(|p| p.min(hist_len)) {
                    Some(p) if p + 1 < hist_len => {
                        pr.history_pos = Some(p + 1);
                        pr.input = history[p + 1].chars().collect();
                    }
                    _ => {
                        pr.history_pos = None;
                        pr.input.clear();
                    }
                }
                pr.cursor = pr.input.len();
                true
            }
            PromptOp::Yank => {
                // prompt_paste: the C-w text (once there is any, even an empty one), else
                // the top buffer up to a control character
                let value: Option<Vec<char>> = if let Some(copied) = &pr.copied {
                    Some(copied.clone())
                } else {
                    self.buffers.iter().find(|b| b.automatic).map(|b| prompt_paste_text(&b.data))
                };
                match value {
                    Some(value) if !value.is_empty() => {
                        let len = value.len();
                        pr.input.splice(pr.cursor..pr.cursor, value);
                        pr.cursor += len;
                        true
                    }
                    _ => false,
                }
            }
            PromptOp::Transpose => {
                let i = if pr.cursor < n { pr.cursor + 1 } else { pr.cursor };
                if i >= 2 {
                    pr.input.swap(i - 2, i - 1);
                    pr.cursor = i;
                }
                i >= 2
            }
            PromptOp::QuoteNext => {
                pr.quote_next = true;
                false
            }
            PromptOp::Tab => self.prompt_complete(pr),
            // C-r/C-s in an incremental prompt: search again, an empty prompt takes the -I text
            PromptOp::Search(back) if pr.incremental => {
                if pr.input.is_empty() {
                    pr.input = pr.last.chars().collect();
                    pr.cursor = pr.input.len();
                } else {
                    prefix = if back { '-' } else { '+' };
                }
                true
            }
            PromptOp::Search(_) => false,
        };
        if changed {
            self.prompt_changed(cid, pr, prefix);
        }
        PromptResult::Handled
    }

    /// prompt_done: fire the callback for a finished prompt.
    fn prompt_done(&mut self, cid: ClientId, pr: &mut Prompt, s: Option<&str>) -> PromptResult {
        if self.prompt_fire(cid, pr, s, Fire::Close) { PromptResult::Close } else { PromptResult::Handled }
    }

    /// The append_key path of prompt_key: insert a character; a single key
    /// prompt (-1, confirm-before) then finishes.
    fn prompt_append(&mut self, cid: ClientId, pr: &mut Prompt, ch: char) -> PromptResult {
        pr.input.insert(pr.cursor.min(pr.input.len()), ch);
        pr.cursor += 1;
        let mut result = PromptResult::Handled;
        if pr.single_key || matches!(pr.kind, PromptKind::Confirm) {
            if pr.input.len() != 1 {
                // with -I text there is no single key to report
                pr.closed = true;
                result = PromptResult::Close;
            } else {
                let s: String = pr.input.iter().collect();
                result = self.prompt_done(cid, pr, Some(&s));
            }
        }
        self.prompt_changed(cid, pr, '=');
        result
    }

    /// An incremental prompt runs its template on every change, the input
    /// after a '=', '+' (C-s) or '-' (C-r).
    fn prompt_changed(&mut self, cid: ClientId, pr: &mut Prompt, prefix: char) {
        if pr.incremental && !pr.closed {
            let s = format!("{prefix}{}", pr.input.iter().collect::<String>());
            self.prompt_fire(cid, pr, Some(&s), Fire::Handled);
        }
    }

    /// The pane prompt a client's keys go to (server_client_handle_key): the
    /// active pane's, else that of the first visible pane in the window's pane
    /// list, floating panes included (window_pane_is_visible).
    pub(crate) fn pane_prompt_target(&self, cid: ClientId) -> Option<PaneId> {
        let sid = self.client_session(cid)?;
        let wid = self.sessions[&sid].active_window();
        let win = self.windows.get(&wid)?;
        let has = |p: &PaneId| self.panes.get(p).is_some_and(|p| p.prompt.is_some());
        if has(&win.active) {
            return self.pane_visible(wid, win.active).then_some(win.active);
        }
        win.panes().into_iter().find(|p| has(p) && self.pane_visible(wid, *p))
    }

    /// A key for a pane prompt (window_pane_prompt_key): false when not used.
    fn pane_prompt_key(&mut self, cid: ClientId, pane: PaneId, k: &Key, name: &str) -> bool {
        let Some(mut pr) = self.panes.get_mut(&pane).and_then(|p| p.prompt.take()) else { return false };
        let result = self.prompt_process(cid, &mut pr, k, name);
        self.prompt_done_key(cid, pr);
        result != PromptResult::NotHandled
    }

    /// A mouse event while a pane prompt shows: a button 1 press on its line
    /// moves the cursor; every mouse event is used.
    pub(crate) fn pane_prompt_mouse(&mut self, cid: ClientId, pane: PaneId, m: &keys::Mouse) {
        let Some(wid) = self.window_of(pane) else { return };
        let Some((_, r)) = self.rects(wid).0.into_iter().find(|(p, _)| *p == pane) else { return };
        let r = self.content_rect(wid, r);
        let sid = self.client_session(cid);
        let oy = if self.opt_session(sid, "status-position") == "top" { self.status_lines(sid) } else { 0 };
        let row = oy + if oy > 0 { r.y } else { r.bottom().saturating_sub(1) };
        if m.kind != keys::MouseKind::Down || m.button != 1 || m.y != row || m.x < r.x || m.x >= r.right() {
            return;
        }
        let Some(mut pr) = self.panes.get_mut(&pane).and_then(|p| p.prompt.take()) else { return };
        self.prompt_mouse_at(cid, &mut pr, (m.x - r.x) as usize, 0, r.w);
        self.prompt_done_key(cid, pr);
    }

    /// prompt_incremental_start: an incremental prompt fires once when opened.
    pub(crate) fn prompt_incremental_start(&mut self, cid: ClientId, host: Option<PaneId>) {
        let pr = match host {
            Some(p) => self.panes.get_mut(&p).and_then(|p| p.prompt.take()),
            None => self.clients.get_mut(&cid).and_then(|c| c.prompt.take()),
        };
        let Some(mut pr) = pr else { return };
        if pr.incremental {
            let s = format!("={}", pr.input.iter().collect::<String>());
            self.prompt_fire(cid, &mut pr, Some(&s), Fire::Handled);
        }
        self.prompt_done_key(cid, pr);
    }

    /// prompt_replace_complete: Tab completes a command name at the start of
    /// the prompt to the longest common prefix of the matches, or keeps the
    /// matches for drawing after the input when there is nothing to add.
    fn prompt_complete(&self, pr: &mut Prompt) -> bool {
        let Some((first, last)) = prompt_word_bounds(pr) else { return false };
        let word: String = pr.input[first..last].iter().collect();
        if word.len() >= 64 || pr.history_type != 0 || first != 0 || word.is_empty() {
            return false;
        }
        let mut list: Vec<String> = super::cmds::command_names().filter(|n| n.starts_with(&word)).map(String::from).collect();
        for (_, alias) in self.opts_server.array_items("command-alias") {
            if let Some((name, _)) = alias.split_once('=') {
                if name.starts_with(&word) && !list.iter().any(|l| l == name) {
                    list.push(name.to_string());
                }
            }
        }
        if list.is_empty() {
            return false;
        }
        list.sort();
        let out = if list.len() == 1 {
            format!("{} ", list[0])
        } else {
            let mut prefix = list[0].clone();
            for item in &list[1..] {
                let common = prefix.chars().zip(item.chars()).take_while(|(a, b)| a == b).count();
                prefix = prefix.chars().take(common).collect();
            }
            prefix
        };
        if out == word {
            if list.len() > 1 {
                pr.complete = list;
            }
            return false;
        }
        prompt_replace_word(pr, &out)
    }
}

/// A command prompt editing operation (the emacs keys of tmux prompt_key).
#[derive(Clone, Copy, PartialEq, Debug)]
enum PromptOp {
    None, Left, Right, Home, End, Tab, BSpace, Delete, ClearLine, KillLine, DeleteWord,
    ForwardWord, ViForward(bool), ViEnd(bool), BackWord(bool), Up, Down, Yank, Transpose,
    Enter, Cancel, QuoteNext, Insert(char), Page, Search(bool),
}

/// What a prompt did with a key (enum prompt_key_result).
#[derive(Clone, Copy, PartialEq, Debug)]
enum PromptResult {
    Handled,
    Close,
    NotHandled,
    Move,
}

/// Why the prompt callback runs (PROMPT_KEY_CLOSE, _HANDLED, _MOVE).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Fire {
    Close,
    Handled,
    Move,
}

/// prompt_keypad_key: unmodified keypad keys are their characters.
fn prompt_keypad_key(k: &Key) -> Key {
    match k.code {
        KeyCode::KP('\r') if k.mods.is_empty() => Key { code: KeyCode::Enter, mods: k.mods },
        KeyCode::KP(c) if k.mods.is_empty() => Key { code: KeyCode::Char(c), mods: k.mods },
        _ => k.clone(),
    }
}

/// A key as the character a single key prompt (or C-v) takes: BSpace is
/// 0x7f, C-x the control character and other modifiers are dropped; keys
/// without a character give None and are ignored.
fn prompt_key_char(k: &Key) -> Option<char> {
    let c = match k.code {
        KeyCode::BSpace => return Some('\x7f'),
        KeyCode::Char(c) => c,
        KeyCode::Space => ' ',
        KeyCode::Enter => '\r',
        KeyCode::Tab => '\t',
        KeyCode::Escape => '\x1b',
        _ => return None,
    };
    Some(if k.mods.ctrl && c.is_ascii() { ((c as u8) & 0x1f) as char } else { c })
}

/// The top buffer as prompt_paste inserts it: up to the first control character.
fn prompt_paste_text(data: &[u8]) -> Vec<char> {
    let end = data.iter().position(|b| *b <= 31 || *b == 127).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).chars().collect()
}

/// The word around the cursor that completion replaces (prompt_replace_complete).
fn prompt_word_bounds(pr: &Prompt) -> Option<(usize, usize)> {
    let n = pr.input.len();
    let sp = |i: usize| i < n && pr.input[i] == ' ';
    let idx = pr.cursor.min(n).saturating_sub(1);
    let mut first = idx;
    while first > 0 && !sp(first) {
        first -= 1;
    }
    while first < n && sp(first) {
        first += 1;
    }
    let mut last = idx;
    while last < n && !sp(last) {
        last += 1;
    }
    while last > 0 && sp(last) {
        last -= 1;
    }
    if last < n {
        last += 1;
    }
    (last >= first).then_some((first, last))
}

/// Replaces the word around the cursor with `s`, the cursor after it.
fn prompt_replace_word(pr: &mut Prompt, s: &str) -> bool {
    let Some((first, last)) = prompt_word_bounds(pr) else { return false };
    let new: Vec<char> = s.chars().collect();
    let len = new.len();
    pr.input.splice(first..last, new);
    pr.cursor = first + len;
    true
}

fn prompt_emacs_op(k: &Key) -> PromptOp {
    let m = k.mods;
    let plain = !m.ctrl && !m.meta;
    match &k.code {
        KeyCode::Char(c) if m.ctrl && !m.meta => match c {
            'b' => PromptOp::Left,
            'f' => PromptOp::Right,
            'a' => PromptOp::Home,
            'e' => PromptOp::End,
            'h' => PromptOp::BSpace,
            'd' => PromptOp::Delete,
            'u' => PromptOp::ClearLine,
            'k' => PromptOp::KillLine,
            'w' => PromptOp::DeleteWord,
            'p' => PromptOp::Up,
            'n' => PromptOp::Down,
            'y' => PromptOp::Yank,
            't' => PromptOp::Transpose,
            // only CR (Enter) confirms: a typed LF is C-j (tty_keys_next) and neither C-j
            // nor an extended C-m has a case in prompt_key, so they go nowhere
            'c' | 'g' | '[' => PromptOp::Cancel,
            'v' => PromptOp::QuoteNext,
            'i' => PromptOp::Tab,
            'r' => PromptOp::Search(true),
            's' => PromptOp::Search(false),
            _ => PromptOp::None,
        },
        KeyCode::Char('f') if m.meta && !m.ctrl => PromptOp::ForwardWord,
        KeyCode::Char('b') if m.meta && !m.ctrl => PromptOp::BackWord(false),
        KeyCode::Right if m.ctrl && !m.meta => PromptOp::ForwardWord,
        KeyCode::Left if m.ctrl && !m.meta => PromptOp::BackWord(false),
        _ if !plain || m.shift && !matches!(k.code, KeyCode::Char(_)) => PromptOp::None,
        KeyCode::Char(c) => PromptOp::Insert(*c),
        KeyCode::KP('\r') | KeyCode::Enter => PromptOp::Enter,
        KeyCode::KP(c) => PromptOp::Insert(*c),
        KeyCode::Space => PromptOp::Insert(' '),
        KeyCode::Left => PromptOp::Left,
        KeyCode::Right => PromptOp::Right,
        KeyCode::Home => PromptOp::Home,
        KeyCode::End => PromptOp::End,
        KeyCode::Tab => PromptOp::Tab,
        KeyCode::BSpace => PromptOp::BSpace,
        KeyCode::Delete => PromptOp::Delete,
        KeyCode::Up => PromptOp::Up,
        KeyCode::Down => PromptOp::Down,
        KeyCode::PageUp | KeyCode::PageDown => PromptOp::Page,
        KeyCode::Escape => PromptOp::Cancel,
        _ => PromptOp::None,
    }
}

/// tmux prompt_translate_key: vi keys as emacs operations, switching between
/// insert and command mode. Escape in insert mode moves the cursor left.
fn prompt_vi_op(pr: &mut Prompt, k: &Key) -> PromptOp {
    let name = keys::key_name(k);
    if !pr.vi_command {
        return match name.as_str() {
            "C-a" | "C-c" | "C-e" | "C-g" | "C-h" | "Tab" | "C-k" | "C-n" | "C-p" | "C-t" | "C-u" | "C-v" | "C-w" | "C-y" | "Enter" | "KPEnter"
            | "C-Left" | "C-Right" | "BSpace" | "DC" | "Down" | "End" | "Home" | "Left" | "Right" | "Up" => prompt_emacs_op(k),
            "Escape" | "C-[" => {
                pr.vi_command = true;
                pr.cursor = pr.cursor.saturating_sub(1);
                PromptOp::None
            }
            // anything else is typed (only plain characters insert anything)
            _ => match prompt_emacs_op(k) {
                op @ PromptOp::Insert(_) => op,
                _ => PromptOp::None,
            },
        };
    }
    match name.as_str() {
        "BSpace" => return PromptOp::Left,
        "A" | "I" | "C" | "s" | "a" => pr.vi_command = false,
        "S" => {
            pr.vi_command = false;
            return PromptOp::ClearLine;
        }
        "i" => {
            pr.vi_command = false;
            return PromptOp::None;
        }
        _ => {}
    }
    match name.as_str() {
        "A" | "$" => PromptOp::End,
        "I" | "0" | "^" => PromptOp::Home,
        "C" | "D" => PromptOp::KillLine,
        "X" => PromptOp::BSpace,
        "b" => PromptOp::BackWord(false),
        "B" => PromptOp::BackWord(true),
        "d" => PromptOp::ClearLine,
        "e" => PromptOp::ViEnd(false),
        "E" => PromptOp::ViEnd(true),
        "w" => PromptOp::ViForward(false),
        "W" => PromptOp::ViForward(true),
        "p" => PromptOp::Yank,
        "q" | "C-c" => PromptOp::Cancel,
        "s" | "DC" | "x" => PromptOp::Delete,
        "Down" | "j" => PromptOp::Down,
        "Left" | "h" => PromptOp::Left,
        "a" | "Right" | "l" => PromptOp::Right,
        "Up" | "k" => PromptOp::Up,
        "C-h" => PromptOp::BSpace,
        "Enter" | "KPEnter" => PromptOp::Enter,
        _ => PromptOp::None,
    }
}

// Word movement over the prompt input as tmux prompt.c: separators (single
// characters from word-separators) form words of their own, spaces end words.
fn prompt_space(s: &[char], i: usize) -> bool {
    s.get(i) == Some(&' ')
}

fn prompt_in_list(s: &[char], i: usize, seps: &[char]) -> bool {
    s.get(i).is_some_and(|c| c.is_ascii() && seps.contains(c))
}

/// prompt_forward_word: vi stops at the start of the next word, emacs at the end.
fn prompt_forward_word(s: &[char], mut i: usize, vi: bool, seps: &[char]) -> usize {
    let n = s.len();
    if !vi {
        while i < n && prompt_space(s, i) {
            i += 1;
        }
    }
    if i >= n {
        return n;
    }
    let class = prompt_in_list(s, i, seps) && !prompt_space(s, i);
    loop {
        i += 1;
        if prompt_space(s, i) {
            if vi {
                while i < n && prompt_space(s, i) {
                    i += 1;
                }
            }
            break;
        }
        if i >= n || class != prompt_in_list(s, i, seps) {
            break;
        }
    }
    i
}

/// prompt_end_word: to the last character of this or the next word.
fn prompt_end_word(s: &[char], mut i: usize, seps: &[char]) -> usize {
    let n = s.len();
    if i >= n {
        return i;
    }
    loop {
        i += 1;
        if i >= n {
            return n;
        }
        if !prompt_space(s, i) {
            break;
        }
    }
    let class = prompt_in_list(s, i, seps);
    loop {
        i += 1;
        if i >= n || prompt_space(s, i) || class != prompt_in_list(s, i, seps) {
            break;
        }
    }
    i - 1
}

/// prompt_backward_word (and the start of the word C-w deletes).
fn prompt_backward_word(s: &[char], mut i: usize, seps: &[char]) -> usize {
    while i != 0 {
        i -= 1;
        if !prompt_space(s, i) {
            break;
        }
    }
    let class = prompt_in_list(s, i, seps);
    while i != 0 {
        i -= 1;
        if prompt_space(s, i) || class != prompt_in_list(s, i, seps) {
            i += 1;
            break;
        }
    }
    i
}

/// The keys tmux reads an unknown CSI sequence as: tty_keys_next finds no key, reply
/// or mouse event for it, so it takes ESC and the next byte as M-[ and each byte
/// after that as a key of its own. Sequences tmux does take as something else are
/// left alone: private ones (<, =, >, ? after the CSI: mouse and terminal replies),
/// mouse reports (M, m), extended keys (u, and 27;...~) and ones with sub-parameters.
pub(super) fn unknown_csi_keys(raw: &[u8]) -> Option<Vec<(Key, Vec<u8>)>> {
    let body = raw.strip_prefix(b"\x1b[")?;
    let (&f, mid) = body.split_last()?;
    if !(0x40..=0x7e).contains(&f) || matches!(f, b'u' | b'M' | b'm') || body.first().is_some_and(|b| (0x3c..=0x3f).contains(b)) {
        return None;
    }
    let p = mid.iter().position(|b| !(0x30..=0x3f).contains(b)).unwrap_or(mid.len());
    let (params, inter) = mid.split_at(p);
    if params.contains(&b':') || !inter.iter().all(|b| (0x20..=0x2f).contains(b)) || (f == b'~' && params.starts_with(b"27;")) {
        return None;
    }
    let mut keys = vec![(Key::new(KeyCode::Char('['), keys::Mods::META), raw[..2].to_vec())];
    // (each byte is a printable character, read as the parser reads a typed one)
    let mut parser = keys::Parser::new();
    let mut rest = parser.feed(body);
    rest.extend(parser.flush_pending());
    for i in rest {
        let Input::Key(k, raw) = i else { return None };
        keys.push((k, raw));
    }
    Some(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_words_like_tmux() {
        // positions from tmux 3.8 with the default word-separators
        let seps: Vec<char> = "!\"#$%&'()*+,-./:;<=>?@[\\]^`{|}~".chars().collect();
        let s: Vec<char> = "set foo-bar.baz qux/quux end".chars().collect();
        assert_eq!(prompt_forward_word(&s, 4, true, &seps), 7);
        assert_eq!(prompt_forward_word(&s, 7, true, &seps), 8);
        assert_eq!(prompt_forward_word(&s, 4, true, &[]), 16);
        assert_eq!(prompt_end_word(&s, 4, &seps), 6);
        assert_eq!(prompt_end_word(&s, 6, &seps), 7);
        assert_eq!(prompt_end_word(&s, 4, &[]), 14);
        assert_eq!(prompt_backward_word(&s, 27, &seps), 25);
        assert_eq!(prompt_backward_word(&s, 25, &seps), 20);
        assert_eq!(prompt_backward_word(&s, 20, &seps), 19);
        assert_eq!(prompt_backward_word(&s, 25, &[]), 16);
        // emacs M-f goes to the end of the word
        assert_eq!(prompt_forward_word(&s, 0, false, &seps), 3);
        assert_eq!(prompt_forward_word(&s, 3, false, &seps), 7);
        assert_eq!(prompt_forward_word(&s, 28, false, &seps), 28);
        assert_eq!(prompt_backward_word(&[], 0, &seps), 0);
    }

    #[test]
    fn prompt_vi_translation() {
        let mut pr = Prompt::new(PromptKind::Command, vec![(String::new(), "ab".into())], String::new(), None);
        let key = |n: &str| keys::parse_key_name(n).unwrap();
        // insert mode: Escape is command mode and one to the left; M-f is dropped
        assert_eq!(prompt_vi_op(&mut pr, &key("M-f")), PromptOp::None);
        assert_eq!(prompt_vi_op(&mut pr, &key("Escape")), PromptOp::None);
        assert!(pr.vi_command && pr.cursor == 1);
        // command mode: Escape does nothing, BSpace is left, S clears and inserts
        assert_eq!(prompt_vi_op(&mut pr, &key("Escape")), PromptOp::None);
        assert_eq!(prompt_vi_op(&mut pr, &key("BSpace")), PromptOp::Left);
        assert_eq!(prompt_vi_op(&mut pr, &key("j")), PromptOp::Down);
        assert_eq!(prompt_vi_op(&mut pr, &key("C-a")), PromptOp::None);
        assert_eq!(prompt_vi_op(&mut pr, &key("S")), PromptOp::ClearLine);
        assert!(!pr.vi_command);
        assert_eq!(prompt_vi_op(&mut pr, &key("C-a")), PromptOp::Home);
        assert_eq!(prompt_emacs_op(&key("M-BSpace")), PromptOp::None);
        assert_eq!(prompt_emacs_op(&key("C-Left")), PromptOp::BackWord(false));
        assert_eq!(prompt_emacs_op(&key("C-r")), PromptOp::Search(true));
        // single keys: C-x is the control character, BSpace 0x7f, special keys none
        assert_eq!(prompt_key_char(&key("C-a")), Some('\x01'));
        assert_eq!(prompt_key_char(&key("BSpace")), Some('\x7f'));
        assert_eq!(prompt_key_char(&key("Escape")), Some('\x1b'));
        assert_eq!(prompt_key_char(&key("M-y")), Some('y'));
        assert_eq!(prompt_key_char(&key("Up")), None);
    }

    #[test]
    fn prompt_completion_word() {
        let mut pr = Prompt::new(PromptKind::Command, vec![(String::new(), "kill-se".into())], String::new(), None);
        assert_eq!(prompt_word_bounds(&pr), Some((0, 7)));
        pr.input = "set -g sta".chars().collect();
        pr.cursor = 10;
        assert_eq!(prompt_word_bounds(&pr), Some((7, 10)));
        pr.input = "kill-server ".chars().collect();
        pr.cursor = 12;
        assert_eq!(prompt_word_bounds(&pr), None);
        pr.input = "sel".chars().collect();
        pr.cursor = 3;
        assert!(prompt_replace_word(&mut pr, "select-"));
        assert_eq!((pr.input.iter().collect::<String>(), pr.cursor), ("select-".to_string(), 7));
    }

    #[test]
    fn editor_files_are_new_and_private() {
        let a = editor_file(b"one").unwrap();
        let b = editor_file(b"two").unwrap();
        assert_ne!(a, b);
        assert!(a.file_name().unwrap().to_string_lossy().starts_with("tmux."));
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }

    #[test]
    fn editor_waits_under_powershell() {
        assert_eq!(editor_command("vi", "'/tmp/tmux.x'", false), "vi '/tmp/tmux.x'");
        assert_eq!(editor_command("notepad.exe", "\"C:\\t\\tmux.x\"", true), "notepad.exe \"C:\\t\\tmux.x\" | Out-Null");
    }

    #[test]
    fn templates() {
        // (tmux 3.8's escapes, which Unix always uses)
        let q = Quoting::TMUX;
        assert_eq!(fill_template("rename-window -- '%%'", &["it's".into()], q).unwrap(), "rename-window -- 'it'\\''s'");
        assert_eq!(fill_template("a %1 b %2", &["x".into(), "y z".into()], q).unwrap(), "a x b y z");
        assert_eq!(fill_template("%%", &["new-window -n x".into()], q).unwrap(), "new-window -n x");
        // cmd_template_replace: %%% and %N% escape for double quotes, each answer takes the next %%
        assert_eq!(fill_template("display \"[%%%]\"", &["a\"b$c".into()], q).unwrap(), "display \"[a\\\"b\\$c]\"");
        assert_eq!(fill_template("x %1% %1", &["a;b".into()], q).unwrap(), "x a\\;b a;b");
        assert_eq!(fill_template("\"%%\"", &["it's".into()], q).unwrap(), "\"it'\\''s\"");
        assert_eq!(fill_template("%% %%", &["q".into()], q).unwrap(), "q %%");
        assert_eq!(fill_template("%% %%", &["q".into(), "r".into()], q).unwrap(), "q r");
        assert_eq!(fill_template("100%", &["q".into()], q).unwrap(), "100%");
        let b = default_bindings();
        assert!(b["prefix"].contains_key("%"));
        assert!(b["prefix"].contains_key("\""));
        assert!(b["copy-mode-vi"].len() > 20);
    }
}

/// The byte tmux writes for an unmodified BSpace: the backspace key's code truncated to a byte
/// when it has no modifiers, or its C0 control code when it has only Ctrl.
/// The backspace option's key. Its default (tmux's '\177', shown as C-?) is the
/// backspace key itself, which this server cannot tell apart from C-? set by name.
fn backspace_key(option: &str) -> Option<Key> {
    if option == "C-?" {
        return Some(Key::plain(KeyCode::BSpace));
    }
    keys::parse_key_name(option)
}

fn backspace_byte(option: &str) -> Option<u8> {
    let key = backspace_key(option)?;
    // tmux function key codes start at KEYC_NONE; their low byte is the enum position
    let code: u32 = match &key.code {
        KeyCode::BSpace if option.eq_ignore_ascii_case("BSpace") => 7,
        KeyCode::BSpace => 0x7f,
        KeyCode::Char(c) => { let mut b = [0; 4]; u32::from(c.encode_utf8(&mut b).as_bytes()[0]) }
        KeyCode::Space => 32,
        KeyCode::Enter => 13,
        KeyCode::Tab => 9,
        KeyCode::Escape => 27,
        KeyCode::F(n) => 7 + u32::from(*n),
        KeyCode::Insert => 20, KeyCode::Delete => 21, KeyCode::Home => 22, KeyCode::End => 23,
        KeyCode::PageDown => 24, KeyCode::PageUp => 25, KeyCode::BTab => 26,
        KeyCode::Up => 27, KeyCode::Down => 28, KeyCode::Left => 29, KeyCode::Right => 30,
        KeyCode::KP(c) => 31 + "/*-789+456123\r0.".find(*c)? as u32,
        _ => return None,
    };
    let m = key.mods;
    if !m.ctrl && !m.meta && !m.shift { return Some(code as u8); }
    if !m.ctrl || m.meta || m.shift { return None; }
    match code {
        0x3f => Some(0x7f),
        0x20 => option.ends_with('@').then_some(0),
        0x40..=0x5f => Some((code - 0x40) as u8),
        0x61..=0x7a => Some((code - 0x60) as u8),
        _ => None,
    }
}

/// The selection characters tmux accepts from an OSC 52 selector, each once.
fn clipboard_selector(selector: &[u8]) -> String {
    let mut clip = String::new();
    for &b in selector {
        if b"cpqs01234567".contains(&b) && !clip.contains(b as char) { clip.push(b as char); }
    }
    clip
}

/// OSC 52 data is decoded with b64_pton (whitespace skipped).
fn decode_osc52(data: &[u8]) -> Result<Vec<u8>, String> {
    super::cmds::base64_decode(data).ok_or_else(|| "invalid base64".to_string())
}

/// input_reply_clipboard: `ESC ] 52 ; [clip] ; base64 terminator`.
fn clipboard_reply_bytes(clip: Option<u8>, data: &[u8], bel: bool) -> Vec<u8> {
    let mut reply = b"\x1b]52;".to_vec();
    reply.extend(clip);
    reply.push(b';');
    if !data.is_empty() { reply.extend(super::cmds::base64(data).into_bytes()); }
    reply.extend(if bel { &b"\x07"[..] } else { &b"\x1b\\"[..] });
    reply
}

/// tmux 3.8 prompt history types (prompt_type_string).
pub const PROMPT_TYPES: [&str; 2] = ["command", "search"];

pub fn prompt_type(name: &str) -> Option<usize> {
    PROMPT_TYPES.iter().position(|t| *t == name)
}

/// "editor file" (spawn_editor). PowerShell does not wait for a GUI program such as the
/// default notepad.exe to exit, so the file would be read (and removed) while it is still
/// being edited; piping the program to Out-Null makes PowerShell wait for it.
fn editor_command(editor: &str, quoted: &str, powershell: bool) -> String {
    if powershell {
        format!("{editor} {quoted} | Out-Null")
    } else {
        format!("{editor} {quoted}")
    }
}

/// mkstemp(_PATH_TMP "tmux.XXXXXXXX"): a new file only this user can read, created
/// exclusively under a random name (never through an existing file or link).
fn editor_file(data: &[u8]) -> std::io::Result<std::path::PathBuf> {
    use std::hash::{BuildHasher, Hasher};
    use std::io::Write;
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let dir = std::env::temp_dir();
    let mut last = None;
    for attempt in 0..100u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(now_ms() ^ attempt);
        h.write_u32(std::process::id());
        let mut n = h.finish();
        let name: String = (0..8)
            .map(|_| {
                let c = CHARS[(n % CHARS.len() as u64) as usize] as char;
                n /= CHARS.len() as u64;
                c
            })
            .collect();
        let path = dir.join(format!("tmux.{name}"));
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        match o.open(&path) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(data) {
                    let _ = std::fs::remove_file(&path);
                    return Err(e);
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no temporary file")))
}
