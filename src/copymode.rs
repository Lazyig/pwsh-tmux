//! tmux-style copy mode: a self-contained engine operating on a snapshot of a pane
//! (history + visible screen). The caller feeds keys or named commands and renders the
//! returned view.
//!
//! Coordinates: `cy` is an absolute line index into `lines` (0 = oldest history line),
//! `top` is the absolute index of the first displayed line. The scroll offset reported to
//! the user (tmux's `oy`) is `hsize - top` where `hsize = lines.len() - height`.

use crate::cells::{Cell, Color, Line, Style};

/// Result of a key or command.
#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    /// State changed (or not); caller re-renders.
    None,
    /// Leave copy mode.
    Cancel,
    /// Put text into a paste buffer (or append to the top one); leave copy mode if `cancel`.
    Copy { text: String, cancel: bool, append: bool },
    /// The pipe commands: give the text to a command without copying it.
    Pipe { text: String, cancel: bool },
    /// Caller asks the user for a string, then calls `run(command, &[input])`.
    Prompt { label: String, command: String },
    /// A key resolved to a copy-mode command: the caller runs it as `send-keys -X` would
    /// (with -F, its arguments are formats).
    Run(String, Vec<String>, bool),
    /// refresh-now (or the refresh timer): the caller supplies the pane's content again.
    Refresh,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    Space,
    Sep,
    Word,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum JumpKind {
    Forward,
    Backward,
    ToForward,
    ToBackward,
}

impl JumpKind {
    fn reverse(self) -> Self {
        match self {
            JumpKind::Forward => JumpKind::Backward,
            JumpKind::Backward => JumpKind::Forward,
            JumpKind::ToForward => JumpKind::ToBackward,
            JumpKind::ToBackward => JumpKind::ToForward,
        }
    }
}

/// tmux selflag: how a mouse drag extends the selection (the selection_mode format).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelMode {
    Char,
    Word,
    Line,
}

impl SelMode {
    pub fn name(self) -> &'static str {
        match self {
            SelMode::Char => "char",
            SelMode::Word => "word",
            SelMode::Line => "line",
        }
    }
}

/// The copy-mode-line-numbers option.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineNumbers {
    Off,
    Default,
    Absolute,
    Relative,
    Hybrid,
}

impl LineNumbers {
    pub fn parse(s: &str) -> LineNumbers {
        match s {
            "default" => LineNumbers::Default,
            "absolute" => LineNumbers::Absolute,
            "relative" => LineNumbers::Relative,
            "hybrid" => LineNumbers::Hybrid,
            _ => LineNumbers::Off,
        }
    }
}

/// Whether search marks are cleared by a command (window_copy_cmd_clear).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Clear {
    Always,
    EmacsOnly,
    Never,
}

/// tmux 3.8 window_copy_cmd_table: name, allowed for read-only clients, search marks.
const COMMANDS: &[(&str, bool, Clear)] = &[
    ("append-selection", false, Clear::Always),
    ("append-selection-and-cancel", false, Clear::Always),
    ("back-to-indentation", true, Clear::Always),
    ("begin-selection", false, Clear::Always),
    ("bottom-line", true, Clear::EmacsOnly),
    ("cancel", true, Clear::Always),
    ("clear-selection", false, Clear::Always),
    ("copy-end-of-line", false, Clear::Always),
    ("copy-end-of-line-and-cancel", false, Clear::Always),
    ("copy-pipe-end-of-line", false, Clear::Always),
    ("copy-pipe-end-of-line-and-cancel", false, Clear::Always),
    ("copy-line", false, Clear::Always),
    ("copy-line-and-cancel", false, Clear::Always),
    ("copy-pipe-line", false, Clear::Always),
    ("copy-pipe-line-and-cancel", false, Clear::Always),
    ("copy-pipe-no-clear", false, Clear::Never),
    ("copy-pipe", false, Clear::Always),
    ("copy-pipe-and-cancel", false, Clear::Always),
    ("copy-selection-no-clear", false, Clear::Never),
    ("copy-selection", false, Clear::Always),
    ("copy-selection-and-cancel", false, Clear::Always),
    ("cursor-down", true, Clear::EmacsOnly),
    ("cursor-down-and-cancel", true, Clear::Always),
    ("cursor-left", true, Clear::EmacsOnly),
    ("cursor-right", true, Clear::EmacsOnly),
    ("cursor-up", true, Clear::EmacsOnly),
    ("cursor-centre-vertical", true, Clear::EmacsOnly),
    ("cursor-centre-horizontal", true, Clear::EmacsOnly),
    ("end-of-line", true, Clear::EmacsOnly),
    ("goto-line", true, Clear::EmacsOnly),
    ("halfpage-down", true, Clear::EmacsOnly),
    ("halfpage-down-and-cancel", true, Clear::Always),
    ("halfpage-up", true, Clear::EmacsOnly),
    ("history-bottom", true, Clear::EmacsOnly),
    ("history-top", true, Clear::EmacsOnly),
    ("jump-again", false, Clear::EmacsOnly),
    ("jump-backward", false, Clear::EmacsOnly),
    ("jump-forward", false, Clear::EmacsOnly),
    ("jump-reverse", false, Clear::EmacsOnly),
    ("jump-to-backward", false, Clear::EmacsOnly),
    ("jump-to-forward", false, Clear::EmacsOnly),
    ("jump-to-mark", true, Clear::Always),
    ("line-numbers-on", true, Clear::Never),
    ("line-numbers-off", true, Clear::Never),
    ("line-numbers-toggle", true, Clear::Never),
    ("next-prompt", true, Clear::Always),
    ("previous-prompt", true, Clear::Always),
    ("middle-line", true, Clear::EmacsOnly),
    ("next-matching-bracket", true, Clear::Always),
    ("next-paragraph", true, Clear::EmacsOnly),
    ("next-space", true, Clear::EmacsOnly),
    ("next-space-end", true, Clear::EmacsOnly),
    ("next-word", true, Clear::EmacsOnly),
    ("next-word-end", true, Clear::EmacsOnly),
    ("other-end", false, Clear::EmacsOnly),
    ("page-down", true, Clear::EmacsOnly),
    ("page-down-and-cancel", true, Clear::Always),
    ("page-up", true, Clear::EmacsOnly),
    ("pipe-no-clear", false, Clear::Never),
    ("pipe", false, Clear::Always),
    ("pipe-and-cancel", false, Clear::Always),
    ("previous-matching-bracket", true, Clear::Always),
    ("previous-paragraph", true, Clear::EmacsOnly),
    ("previous-space", true, Clear::EmacsOnly),
    ("previous-word", true, Clear::EmacsOnly),
    ("recentre-top-bottom", true, Clear::Always),
    ("rectangle-on", false, Clear::Always),
    ("rectangle-off", false, Clear::Always),
    ("rectangle-toggle", false, Clear::Always),
    ("refresh-on", true, Clear::Never),
    ("refresh-off", true, Clear::Never),
    ("refresh-now", true, Clear::Never),
    ("refresh-toggle", true, Clear::Never),
    ("scroll-bottom", true, Clear::Always),
    ("scroll-down", true, Clear::EmacsOnly),
    ("scroll-down-and-cancel", true, Clear::Always),
    ("scroll-exit-on", false, Clear::Always),
    ("scroll-exit-off", false, Clear::Always),
    ("scroll-exit-toggle", false, Clear::Always),
    ("scroll-middle", true, Clear::Always),
    ("scroll-to-mouse", true, Clear::EmacsOnly),
    ("scroll-top", true, Clear::Always),
    ("scroll-up", true, Clear::EmacsOnly),
    ("search-again", false, Clear::Always),
    ("search-backward", false, Clear::Always),
    ("search-backward-text", false, Clear::Always),
    ("search-backward-incremental", false, Clear::Always),
    ("search-forward", false, Clear::Always),
    ("search-forward-text", false, Clear::Always),
    ("search-forward-incremental", false, Clear::Always),
    ("search-reverse", false, Clear::Always),
    ("select-line", false, Clear::Always),
    ("select-word", false, Clear::Always),
    ("selection-mode", false, Clear::Always),
    ("set-mark", true, Clear::Always),
    ("start-of-line", true, Clear::EmacsOnly),
    ("stop-selection", false, Clear::Always),
    ("toggle-position", true, Clear::Never),
    ("top-line", true, Clear::EmacsOnly),
];

/// Whether a copy-mode command exists (tmux ignores unknown ones).
pub fn is_command(name: &str) -> bool {
    COMMANDS.iter().any(|c| c.0 == name)
}

/// Whether a read-only client may run a copy-mode command (WINDOW_COPY_CMD_FLAG_READONLY):
/// moving around is allowed, selecting and copying is not.
pub fn command_allowed_readonly(name: &str) -> bool {
    COMMANDS.iter().any(|c| c.0 == name && c.1)
}

/// The default copy-mode-vi table of tmux 3.8 (key-bindings.c), commands as list-keys
/// prints them. Keys bound to commands with a mouse event are run by the caller.
pub const VI_BINDINGS: &[(&str, &str)] = &[
    ("Enter", "send-keys -X copy-pipe-and-cancel"),
    ("Escape", "send-keys -X clear-selection"),
    ("Space", "send-keys -X begin-selection"),
    ("#", "send-keys -FX search-backward -- \"#{copy_cursor_word}\""),
    ("$", "send-keys -X end-of-line"),
    ("%", "send-keys -X next-matching-bracket"),
    ("*", "send-keys -FX search-forward -- \"#{copy_cursor_word}\""),
    (",", "send-keys -X jump-reverse"),
    ("/", "command-prompt -P -T search -p \"(search down)\" { send-keys -X search-forward -- \"%%\" }"),
    ("0", "send-keys -X start-of-line"),
    ("1", "command-prompt -NP -I 1 -p (repeat) { send-keys -N \"%%\" }"),
    ("2", "command-prompt -NP -I 2 -p (repeat) { send-keys -N \"%%\" }"),
    ("3", "command-prompt -NP -I 3 -p (repeat) { send-keys -N \"%%\" }"),
    ("4", "command-prompt -NP -I 4 -p (repeat) { send-keys -N \"%%\" }"),
    ("5", "command-prompt -NP -I 5 -p (repeat) { send-keys -N \"%%\" }"),
    ("6", "command-prompt -NP -I 6 -p (repeat) { send-keys -N \"%%\" }"),
    ("7", "command-prompt -NP -I 7 -p (repeat) { send-keys -N \"%%\" }"),
    ("8", "command-prompt -NP -I 8 -p (repeat) { send-keys -N \"%%\" }"),
    ("9", "command-prompt -NP -I 9 -p (repeat) { send-keys -N \"%%\" }"),
    (":", "command-prompt -P -p \"(goto line)\" { send-keys -X goto-line -- \"%%\" }"),
    (";", "send-keys -X jump-again"),
    ("?", "command-prompt -P -T search -p \"(search up)\" { send-keys -X search-backward -- \"%%\" }"),
    ("A", "send-keys -X append-selection-and-cancel"),
    ("B", "send-keys -X previous-space"),
    ("D", "send-keys -X copy-pipe-end-of-line-and-cancel"),
    ("E", "send-keys -X next-space-end"),
    ("F", "command-prompt -1P -p \"(jump backward)\" { send-keys -X jump-backward -- \"%%\" }"),
    ("G", "send-keys -X history-bottom"),
    ("H", "send-keys -X top-line"),
    ("J", "send-keys -X scroll-down"),
    ("K", "send-keys -X scroll-up"),
    ("L", "send-keys -X bottom-line"),
    ("M", "send-keys -X middle-line"),
    ("N", "send-keys -X search-reverse"),
    ("P", "send-keys -X toggle-position"),
    ("T", "command-prompt -1P -p \"(jump to backward)\" { send-keys -X jump-to-backward -- \"%%\" }"),
    ("V", "send-keys -X select-line"),
    ("W", "send-keys -X next-space"),
    ("X", "send-keys -X set-mark"),
    ("^", "send-keys -X back-to-indentation"),
    ("b", "send-keys -X previous-word"),
    ("e", "send-keys -X next-word-end"),
    ("f", "command-prompt -1P -p \"(jump forward)\" { send-keys -X jump-forward -- \"%%\" }"),
    ("g", "send-keys -X history-top"),
    ("h", "send-keys -X cursor-left"),
    ("j", "send-keys -X cursor-down"),
    ("k", "send-keys -X cursor-up"),
    ("l", "send-keys -X cursor-right"),
    ("n", "send-keys -X search-again"),
    ("o", "send-keys -X other-end"),
    ("q", "send-keys -X cancel"),
    ("r", "send-keys -X refresh-now"),
    ("t", "command-prompt -1P -p \"(jump to forward)\" { send-keys -X jump-to-forward -- \"%%\" }"),
    ("v", "send-keys -X rectangle-toggle"),
    ("w", "send-keys -X next-word"),
    ("z", "send-keys -X scroll-middle"),
    ("{", "send-keys -X previous-paragraph"),
    ("}", "send-keys -X next-paragraph"),
    ("BSpace", "send-keys -X cursor-left"),
    ("Home", "send-keys -X start-of-line"),
    ("End", "send-keys -X end-of-line"),
    ("NPage", "send-keys -X page-down"),
    ("PPage", "send-keys -X page-up"),
    ("Up", "send-keys -X cursor-up"),
    ("Down", "send-keys -X cursor-down"),
    ("Left", "send-keys -X cursor-left"),
    ("Right", "send-keys -X cursor-right"),
    ("MouseDown1Pane", "select-pane"),
    ("MouseDrag1Pane", "select-pane \\; send-keys -X begin-selection"),
    ("MouseDragEnd1Pane", "send-keys -X copy-pipe-and-cancel"),
    ("WheelDownPane", "select-pane \\; send-keys -X -N 5 scroll-down"),
    ("WheelUpPane", "select-pane \\; send-keys -X -N 5 scroll-up"),
    ("DoubleClick1Pane", "select-pane \\; send-keys -X select-word \\; run-shell -d 0.3 \\; send-keys -X copy-pipe-and-cancel"),
    ("TripleClick1Pane", "select-pane \\; send-keys -X select-line \\; run-shell -d 0.3 \\; send-keys -X copy-pipe-and-cancel"),
    ("M-x", "send-keys -X jump-to-mark"),
    ("C-[", "send-keys -X clear-selection"),
    ("C-b", "send-keys -X page-up"),
    ("C-c", "send-keys -X cancel"),
    ("C-d", "send-keys -X halfpage-down"),
    ("C-e", "send-keys -X scroll-down"),
    ("C-f", "send-keys -X page-down"),
    ("C-h", "send-keys -X cursor-left"),
    ("C-j", "send-keys -X copy-pipe-and-cancel"),
    ("C-u", "send-keys -X halfpage-up"),
    ("C-v", "send-keys -X rectangle-toggle"),
    ("C-y", "send-keys -X scroll-up"),
    ("C-Up", "send-keys -X scroll-up"),
    ("C-Down", "send-keys -X scroll-down"),
];

/// The default copy-mode (emacs) table of tmux 3.8.
pub const EMACS_BINDINGS: &[(&str, &str)] = &[
    ("Escape", "send-keys -X cancel"),
    ("Space", "send-keys -X page-down"),
    (",", "send-keys -X jump-reverse"),
    (";", "send-keys -X jump-again"),
    ("F", "command-prompt -1P -p \"(jump backward)\" { send-keys -X jump-backward -- \"%%\" }"),
    ("L", "send-keys -X line-numbers-toggle"),
    ("N", "send-keys -X search-reverse"),
    ("P", "send-keys -X toggle-position"),
    ("R", "send-keys -X rectangle-toggle"),
    ("T", "command-prompt -1P -p \"(jump to backward)\" { send-keys -X jump-to-backward -- \"%%\" }"),
    ("X", "send-keys -X set-mark"),
    ("f", "command-prompt -1P -p \"(jump forward)\" { send-keys -X jump-forward -- \"%%\" }"),
    ("g", "command-prompt -P -p \"(goto line)\" { send-keys -X goto-line -- \"%%\" }"),
    ("n", "send-keys -X search-again"),
    ("q", "send-keys -X cancel"),
    ("r", "send-keys -X refresh-now"),
    ("t", "command-prompt -1P -p \"(jump to forward)\" { send-keys -X jump-to-forward -- \"%%\" }"),
    ("Home", "send-keys -X start-of-line"),
    ("End", "send-keys -X end-of-line"),
    ("NPage", "send-keys -X page-down"),
    ("PPage", "send-keys -X page-up"),
    ("Up", "send-keys -X cursor-up"),
    ("Down", "send-keys -X cursor-down"),
    ("Left", "send-keys -X cursor-left"),
    ("Right", "send-keys -X cursor-right"),
    ("MouseDown1Pane", "select-pane"),
    ("MouseDrag1Pane", "select-pane \\; send-keys -X begin-selection"),
    ("MouseDragEnd1Pane", "send-keys -X copy-pipe-and-cancel"),
    ("WheelDownPane", "select-pane \\; send-keys -X -N 5 scroll-down"),
    ("WheelUpPane", "select-pane \\; send-keys -X -N 5 scroll-up"),
    ("DoubleClick1Pane", "select-pane \\; send-keys -X select-word \\; run-shell -d 0.3 \\; send-keys -X copy-pipe-and-cancel"),
    ("TripleClick1Pane", "select-pane \\; send-keys -X select-line \\; run-shell -d 0.3 \\; send-keys -X copy-pipe-and-cancel"),
    ("M-1", "command-prompt -NP -I 1 -p (repeat) { send-keys -N \"%%\" }"),
    ("M-2", "command-prompt -NP -I 2 -p (repeat) { send-keys -N \"%%\" }"),
    ("M-3", "command-prompt -NP -I 3 -p (repeat) { send-keys -N \"%%\" }"),
    ("M-4", "command-prompt -NP -I 4 -p (repeat) { send-keys -N \"%%\" }"),
    ("M-5", "command-prompt -NP -I 5 -p (repeat) { send-keys -N \"%%\" }"),
    ("M-6", "command-prompt -NP -I 6 -p (repeat) { send-keys -N \"%%\" }"),
    ("M-7", "command-prompt -NP -I 7 -p (repeat) { send-keys -N \"%%\" }"),
    ("M-8", "command-prompt -NP -I 8 -p (repeat) { send-keys -N \"%%\" }"),
    ("M-9", "command-prompt -NP -I 9 -p (repeat) { send-keys -N \"%%\" }"),
    ("M-<", "send-keys -X history-top"),
    ("M->", "send-keys -X history-bottom"),
    ("M-R", "send-keys -X top-line"),
    ("M-b", "send-keys -X previous-word"),
    ("M-f", "send-keys -X next-word-end"),
    ("M-l", "send-keys -X cursor-centre-horizontal"),
    ("M-m", "send-keys -X back-to-indentation"),
    ("M-r", "send-keys -X middle-line"),
    ("M-v", "send-keys -X page-up"),
    ("M-w", "send-keys -X copy-pipe-and-cancel"),
    ("M-x", "send-keys -X jump-to-mark"),
    ("M-{", "send-keys -X previous-paragraph"),
    ("M-}", "send-keys -X next-paragraph"),
    ("M-Up", "send-keys -X halfpage-up"),
    ("M-Down", "send-keys -X halfpage-down"),
    ("C-Space", "send-keys -X begin-selection"),
    ("C-[", "send-keys -X cancel"),
    ("C-a", "send-keys -X start-of-line"),
    ("C-b", "send-keys -X cursor-left"),
    ("C-c", "send-keys -X cancel"),
    ("C-e", "send-keys -X end-of-line"),
    ("C-f", "send-keys -X cursor-right"),
    ("C-g", "send-keys -X clear-selection"),
    ("C-k", "send-keys -X copy-pipe-end-of-line-and-cancel"),
    ("C-l", "send-keys -X recentre-top-bottom"),
    ("C-n", "send-keys -X cursor-down"),
    ("C-p", "send-keys -X cursor-up"),
    ("C-r", "command-prompt -Pi -I \"#{pane_search_string}\" -T search -p \"(search up)\" { send-keys -X search-backward-incremental -- \"%%\" }"),
    ("C-s", "command-prompt -Pi -I \"#{pane_search_string}\" -T search -p \"(search down)\" { send-keys -X search-forward-incremental -- \"%%\" }"),
    ("C-v", "send-keys -X page-down"),
    ("C-w", "send-keys -X copy-pipe-and-cancel"),
    ("C-Up", "send-keys -X scroll-up"),
    ("C-Down", "send-keys -X scroll-down"),
    ("C-M-b", "send-keys -X previous-matching-bracket"),
    ("C-M-f", "send-keys -X next-matching-bracket"),
    ("C-M-Up", "send-keys -X previous-prompt"),
    ("C-M-Down", "send-keys -X next-prompt"),
];

/// What a default binding does when the engine runs it itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Internal<'a> {
    /// send-keys -X <command>
    Command(&'a str),
    /// send-keys -FX search-{forward,backward} -- "#{copy_cursor_word}"
    CursorWord(bool),
    /// command-prompt -N ... -I <digit>: starts a repeat count
    Count(usize),
    /// command-prompt ... { send-keys -X <command> -- "%%" }: asks for the argument
    Prompt(&'a str),
}

/// The engine's reading of a default binding's command (None for mouse bindings, which
/// are run as commands by the caller).
fn internal(cmd: &str) -> Option<Internal<'_>> {
    if let Some(name) = cmd.strip_prefix("send-keys -X ").filter(|n| !n.contains(' ')) {
        return Some(Internal::Command(name));
    }
    if cmd.contains("#{copy_cursor_word}") {
        return Some(Internal::CursorWord(cmd.contains("search-forward")));
    }
    let rest = cmd.strip_prefix("command-prompt ")?;
    if let Some(i) = rest.find("-I ").filter(|_| rest.starts_with("-NP")) {
        return rest[i + 3..].chars().next()?.to_digit(10).map(|d| Internal::Count(d as usize));
    }
    let inner = &rest[rest.find("send-keys -X ")? + 13..];
    Some(Internal::Prompt(inner.split(' ').next()?))
}

type Pos = (usize, usize);

/// tmux WHITESPACE.
const WHITESPACE: &str = "\t ";

/// tmux's default `word-separators` (tmux 3.0 - 3.2).
pub const DEFAULT_WORD_SEPARATORS: &str = " -_@";

const BLACK: Color = Color::Idx(0);
const MAGENTA: Color = Color::Idx(5);
const CYAN: Color = Color::Idx(6);
const YELLOW: Color = Color::Idx(3);

/// The styles copy mode draws with (copy-mode-*-style options), and what the position
/// indicator shows (copy-mode-position-format, already expanded).
#[derive(Clone, Debug)]
pub struct RenderStyle {
    pub selection: Style,
    /// copy-mode-selection-style has noattr: the cells' own attributes are not kept.
    pub selection_noattr: bool,
    pub matched: Style,
    pub current_match: Style,
    pub mark: Style,
    /// copy-mode-current-line-style; None when it is "default".
    pub current_line: Option<Style>,
    pub line_number: Style,
    pub current_line_number: Style,
    pub position: Style,
    /// The expanded copy-mode-position-format; None draws tmux's default "[oy/hsize]".
    pub position_text: Option<String>,
}

impl Default for RenderStyle {
    fn default() -> Self {
        let mode = Style { fg: BLACK, bg: YELLOW, attrs: 0, ..Style::default() };
        RenderStyle {
            selection: mode,
            selection_noattr: true,
            matched: Style { fg: BLACK, bg: CYAN, attrs: 0, ..Style::default() },
            current_match: Style { fg: BLACK, bg: MAGENTA, attrs: 0, ..Style::default() },
            mark: Style { fg: BLACK, bg: YELLOW, attrs: 0, ..Style::default() },
            current_line: None,
            line_number: Style { fg: Color::Idx(7), attrs: crate::cells::DIM, ..Style::default() },
            current_line_number: Style { fg: YELLOW, ..Style::default() },
            position: mode,
            position_text: None,
        }
    }
}

/// The lines copy mode works on: lines it was given (view mode, a rewrapped copy), or a
/// frozen copy of a pane's screen whose rows become lines ([`crate::cells::snapshot_line`])
/// when they are first read. Rows read with [`Backing::get`] stay lines until
/// [`Backing::trim`]; the row helpers and [`Backing::peek`] read a row without keeping it,
/// for scans over the whole history.
#[derive(Default)]
pub struct Backing {
    frozen: Option<Frozen>,
    /// The lines after the frozen rows (every line when there is no screen).
    lines: Vec<Line>,
}

struct Frozen {
    screen: Box<vt100::Screen>,
    /// How many of the screen's rows (history first) are lines of the backing.
    rows: usize,
    cache: Vec<std::cell::OnceCell<Box<Line>>>,
    /// The rows with a line in `cache`.
    cached: std::cell::RefCell<Vec<usize>>,
}

impl From<Vec<Line>> for Backing {
    fn from(lines: Vec<Line>) -> Self {
        Backing { frozen: None, lines }
    }
}

#[cfg(test)]
impl std::ops::Index<usize> for Backing {
    type Output = Line;
    fn index(&self, i: usize) -> &Line {
        self.get(i).expect("line index")
    }
}

#[cfg(test)]
impl Backing {
    fn iter(&self) -> impl Iterator<Item = &Line> {
        (0..self.len()).filter_map(|i| self.get(i))
    }
}

impl Backing {
    /// The rows of `screen` with its history, oldest first, as [`crate::cells::snapshot`]
    /// gives them.
    pub fn frozen(screen: vt100::Screen) -> Self {
        let rows = screen.history_rows_len();
        let cache = std::iter::repeat_with(std::cell::OnceCell::new).take(rows).collect();
        Backing { frozen: Some(Frozen { screen: Box::new(screen), rows, cache, cached: Default::default() }), lines: Vec::new() }
    }

    fn frozen_rows(&self) -> usize {
        self.frozen.as_ref().map_or(0, |f| f.rows)
    }

    /// The frozen screen and row `i` when that line comes from it and is not built yet.
    fn unbuilt(&self, i: usize) -> Option<(&vt100::Screen, usize)> {
        self.frozen.as_ref().filter(|f| i < f.rows && f.cache[i].get().is_none()).map(|f| (&*f.screen, i))
    }

    pub fn len(&self) -> usize {
        self.frozen_rows() + self.lines.len()
    }

    /// Line `i`, built from the screen (and kept) the first time it is read.
    pub fn get(&self, i: usize) -> Option<&Line> {
        match &self.frozen {
            Some(f) if i < f.rows => Some(f.cache[i].get_or_init(|| {
                f.cached.borrow_mut().push(i);
                Box::new(crate::cells::snapshot_line(&f.screen, i))
            })),
            _ => self.lines.get(i - self.frozen_rows()),
        }
    }

    /// `f` of line `i`, which is not kept if it had to be built.
    pub fn peek<R>(&self, i: usize, f: impl FnOnce(&Line) -> R) -> Option<R> {
        match self.unbuilt(i) {
            Some((screen, i)) => Some(f(&crate::cells::snapshot_line(screen, i))),
            None => self.get(i).map(f),
        }
    }

    /// Line `i` soft-wraps onto the next one.
    pub fn wrapped(&self, i: usize) -> bool {
        match self.unbuilt(i) {
            Some((screen, i)) => screen.history_row(i).is_some_and(|r| r.wrapped()),
            None => self.get(i).is_some_and(|l| l.wrapped),
        }
    }

    /// The ROW_* flags of line `i`.
    pub fn flags(&self, i: usize) -> u8 {
        match self.unbuilt(i) {
            Some((screen, i)) => screen.history_row(i).map_or(0, |r| r.flags()),
            None => self.get(i).map_or(0, |l| l.flags),
        }
    }

    /// [`Line::text_len`] of line `i` (0 past the end).
    pub fn text_len(&self, i: usize) -> usize {
        match self.unbuilt(i) {
            Some((screen, i)) => crate::cells::snapshot_text_len(screen, i),
            None => self.get(i).map_or(0, Line::text_len),
        }
    }

    /// How many cells line `i` has (0 past the end).
    pub fn cells_len(&self, i: usize) -> usize {
        match self.unbuilt(i) {
            Some((screen, i)) => crate::cells::snapshot_cells_len(screen, i),
            None => self.get(i).map_or(0, |l| l.cells.len()),
        }
    }

    /// Line `i` as an owned line (built and not kept when it is not built yet).
    pub fn cloned(&self, i: usize) -> Option<Line> {
        match self.unbuilt(i) {
            Some((screen, i)) => Some(crate::cells::snapshot_line(screen, i)),
            None => self.get(i).cloned(),
        }
    }

    /// `f` of the [`Cell::text`] of cell `x` of line `i` (None past its cells or the end).
    pub fn with_cell<R>(&self, i: usize, x: usize, f: impl FnOnce(Option<&str>) -> R) -> R {
        match self.unbuilt(i) {
            Some((screen, i)) => crate::cells::with_snapshot_cell(screen, i, x, f),
            None => f(self.get(i).and_then(|l| l.cells.get(x)).map(|c| c.text.as_str())),
        }
    }

    /// [`Line::is_padding`] of cell `x` of line `i` (false past the end).
    pub fn is_padding(&self, i: usize, x: usize) -> bool {
        match self.unbuilt(i) {
            Some((screen, i)) => crate::cells::snapshot_is_padding(screen, i, x),
            None => self.get(i).is_some_and(|l| l.is_padding(x)),
        }
    }

    /// Appends [`Line::cell_text`] of each of cells `xs` of line `i` to `out` (nothing past
    /// the end).
    pub fn push_cell_texts(&self, out: &mut String, i: usize, xs: std::ops::Range<usize>) {
        match self.unbuilt(i) {
            Some((screen, i)) => crate::cells::push_snapshot_cell_texts(out, screen, i, xs),
            None => {
                if let Some(l) = self.get(i) {
                    xs.for_each(|x| out.push_str(l.cell_text(x)));
                }
            }
        }
    }

    /// `f` of the text a search reads in each of columns `0..width` of line `i`
    /// ([`Line::cell_text`], a blank past the cells or the end).
    pub fn texts(&self, i: usize, width: usize, mut f: impl FnMut(&str)) {
        match self.unbuilt(i) {
            Some((screen, i)) => crate::cells::snapshot_texts(screen, i, width, f),
            None => {
                let line = self.get(i);
                for x in 0..width {
                    f(match line {
                        Some(l) if x < l.cells.len() => l.cell_text(x),
                        _ => " ",
                    });
                }
            }
        }
    }

    pub fn push(&mut self, line: Line) {
        self.lines.push(line);
    }

    pub fn pop(&mut self) {
        if self.lines.pop().is_some() {
            return;
        }
        if let Some(f) = self.frozen.as_mut().filter(|f| f.rows > 0) {
            f.rows -= 1;
            if f.cache[f.rows].take().is_some() {
                let last = f.rows;
                f.cached.get_mut().retain(|&i| i != last);
            }
        }
    }

    /// Every line.
    pub fn to_vec(&self) -> Vec<Line> {
        (0..self.len()).filter_map(|i| self.cloned(i)).collect()
    }

    /// Every line, built from the screen where not built yet.
    pub fn into_vec(self) -> Vec<Line> {
        let Some(f) = self.frozen else { return self.lines };
        let mut out: Vec<Line> = f.cache.into_iter().take(f.rows).enumerate()
            .map(|(i, c)| c.into_inner().map_or_else(|| crate::cells::snapshot_line(&f.screen, i), |l| *l)).collect();
        out.extend(self.lines);
        out
    }

    /// Lets the built lines outside `keep` go once more than `limit` are kept.
    pub fn trim(&mut self, keep: std::ops::Range<usize>, limit: usize) {
        let Some(Frozen { cache, cached, .. }) = self.frozen.as_mut() else { return };
        let cached = cached.get_mut();
        if cached.len() > limit {
            cached.retain(|&i| keep.contains(&i) || cache[i].take().is_none());
        }
    }
}

pub struct CopyMode {
    lines: Backing,
    width: usize,
    height: usize,
    /// Absolute index of the first displayed line.
    top: usize,
    cy: usize,
    cx: usize,
    /// Desired column for vertical motion.
    lastcx: usize,
    vi: bool,
    /// Selection anchor; the other end is the cursor, or `frozen` once the selection
    /// stopped following it (tmux CURSORDRAG_NONE).
    anchor: Option<Pos>,
    frozen: Option<Pos>,
    line_mode: bool,
    /// The cursor drags the selection's start (tmux's CURSORDRAG_SEL, after other-end or a
    /// drag from near the start): selection_start_* is then the cursor's end.
    ends_swapped: bool,
    /// tmux's lineflag after select-line or select-word: the cursor is on the end side
    /// (LINE_SEL_LEFT_RIGHT, true) or the start side (LINE_SEL_RIGHT_LEFT) of the lines.
    lineflag: Option<bool>,
    /// A word selection (SEL_WORD): the word select-word chose and where the cursor was.
    word_origin: Option<(Pos, Pos, Pos)>,
    /// Reset endpoints and direction origin survive selection-mode changes (selr/endselr/d).
    selection_reset: (Pos, Pos, Pos),
    /// The selection's ends as tmux last set them, while the cursor stays at a position
    /// (moves that do not update the selection, or update it with no_reset).
    pinned: Option<(Pos, Pos, Pos)>,
    rect: bool,
    sel_mode: SelMode,
    /// A mouse drag is in progress (tty.mouse_drag_flag).
    dragging: bool,
    /// Last search: (text, forward).
    search: Option<(String, bool)>,
    /// The open incremental prompt's last text and direction (what it last sent).
    incremental_prompt: Option<(String, bool)>,
    /// tmux's searchx/searchy/searcho: where incremental searches restart from.
    incremental_command_origin: Option<(Pos, usize)>,
    search_regex: bool,
    search_cache: std::cell::RefCell<(String, bool, Option<fancy_regex::Regex>)>,
    /// Last jump: (kind, character).
    jump: Option<(JumpKind, String)>,
    /// A jump command waiting for its character argument.
    pending_jump: Option<JumpKind>,
    count: Option<usize>,
    /// The count was started with an emacs M-digit (plain digits then add to it).
    count_open: bool,
    separators: String,
    /// The mark (set-mark): position and whether it is shown.
    mark: Pos,
    show_mark: bool,
    show_position: bool,
    search_marks: bool,
    /// tmux's searchcount: None once the marks are cleared (-1), 0 before any search.
    found_count: Option<usize>,
    /// tmux's searchall: the first search counts every match.
    search_all: bool,
    search_timed_out: bool,
    search_count_partial: bool,
    /// The pane's last search (wp->searchstr): searching for it again only marks.
    pane_search: Option<String>,
    pane_search_regex: bool,
    wrap_search: bool,
    /// tmux's searchdirection: the last search went down.
    searched_down: bool,
    /// An emacs forward search found a match ending in the grid's last cell and, with
    /// wrap-search, tmux moved its cursor on to the first cell (above the view). The
    /// cursor stays drawn on the last cell; searches and their highlights use the first
    /// cell while it is there (search_cursor).
    search_wrapped: bool,
    exit_on_bottom: bool,
    /// tmux's lastcx / lastsx: the column to return to and the length of the line it was on,
    /// for vertical motion and scrolling (both start at 0).
    tlastcx: usize,
    tlastsx: usize,
    /// line_numbers: 0 off, 1 from the option, 2 shown (default style) though the option is off.
    line_numbers: u8,
    ln_option: LineNumbers,
    /// copy-mode-current-line-style is not "default" (cursor line drawn differently).
    refresh_active: bool,
    /// recentre-top-bottom: next position (0 middle, 1 top, 2 bottom) and its line.
    recentre: (u8, usize),
    /// View mode (command output) or a copy of another pane: no refresh from the pane.
    view: bool,
    pub from_other: bool,
    /// copy-mode -k: kill the pane when the mode exits.
    pub kill: bool,
    /// When the automatic refresh next looks at the pane (kept by the caller).
    pub refresh_due: Option<std::time::Instant>,
}

impl CopyMode {
    /// `lines`: snapshot (history + screen). The view initially shows the bottom `height`
    /// lines; `cursor` is (row within that bottom view, col).
    pub fn new(lines: impl Into<Backing>, width: u16, height: u16, cursor: (u16, u16), vi: bool) -> Self {
        let mut cm = CopyMode {
            lines: lines.into(),
            width: (width as usize).max(1),
            height: (height as usize).max(1),
            top: 0,
            cy: 0,
            cx: 0,
            lastcx: 0,
            vi,
            anchor: None,
            frozen: None,
            line_mode: false,
            ends_swapped: false,
            lineflag: None,
            word_origin: None,
            selection_reset: ((0, 0), (0, 0), (0, 0)),
            pinned: None,
            rect: false,
            sel_mode: SelMode::Char,
            dragging: false,
            search: None,
            incremental_prompt: None,
            incremental_command_origin: None,
            search_regex: true,
            search_cache: std::cell::RefCell::new((String::new(), false, None)),
            jump: None,
            pending_jump: None,
            count: None,
            count_open: false,
            separators: DEFAULT_WORD_SEPARATORS.to_string(),
            mark: (0, 0),
            show_mark: false,
            show_position: true,
            search_marks: false,
            found_count: Some(0),
            search_all: true,
            search_timed_out: false,
            search_count_partial: false,
            pane_search: None,
            pane_search_regex: true,
            wrap_search: true,
            searched_down: false,
            search_wrapped: false,
            exit_on_bottom: false,
            tlastcx: 0,
            tlastsx: 0,
            line_numbers: 1,
            ln_option: LineNumbers::Off,
            refresh_active: false,
            recentre: (0, 0),
            view: false,
            from_other: false,
            kill: false,
            refresh_due: None,
        };
        cm.pad();
        cm.top = cm.hsize();
        let row = (cursor.0 as usize).min(cm.height - 1);
        cm.cy = cm.top + row;
        cm.cx = cm.snap(cm.cy, cursor.1 as usize);
        cm.lastcx = cm.cx;
        cm.mark = (cm.cy, cm.cx);
        cm
    }

    /// window_copy_clone_screen and window_copy_init: the source pane's lines (`src` is its
    /// size, `cursor` its cursor as (row, column)) shown in a pane of `size`. With `trim`
    /// (copy-mode -s) empty lines at the bottom are dropped first; a different width
    /// rewraps the lines (grid_reflow) and the cursor stays on its character.
    pub fn clone_from(lines: impl Into<Backing>, src: (u16, u16), cursor: (u16, u16), size: (u16, u16), vi: bool, trim: bool) -> Self {
        let mut lines = lines.into();
        let (sw, sh) = ((src.0 as usize).max(1), (src.1 as usize).max(1));
        while lines.len() < sh {
            lines.push(Line::default());
        }
        let hsize = lines.len() - sh;
        if trim {
            let blank = |l: &Line| !l.wrapped && l.cells.iter().all(|c| *c == Cell::default());
            while lines.len() > hsize + 1 && lines.peek(lines.len() - 1, blank) == Some(true) {
                lines.pop();
            }
        }
        let rows = lines.len() - hsize;
        let (mut cx, cy) = if cursor.0 as usize > rows - 1 { (0, rows - 1) } else { (cursor.1 as usize, cursor.0 as usize) };
        let mut cy = hsize + cy;
        let width = (size.0 as usize).max(1);
        // screen_resize_cursor: the height first (history lines come down into a taller
        // screen, then blank lines are added below), then the reflow
        while lines.len() < (size.1 as usize).max(1) {
            lines.push(Line::default());
        }
        if width != sw {
            let old = lines.into_vec();
            let (wx, wy) = wrap_position(&old, cx, cy);
            let new = reflow(&old, width);
            (cx, cy) = unwrap_position(&new, wx, wy);
            lines = new.into();
        }
        let mut cm = CopyMode::new(lines, size.0, size.1, (0, 0), vi);
        cm.place_cursor(cx, cy);
        cm.mark = (cm.cy, cm.cx);
        cm
    }

    /// The view after the backing changed (window_copy_init, window_copy_resize): a
    /// cursor in the history is on the top row, otherwise the view is at the bottom.
    fn place_cursor(&mut self, cx: usize, cy: usize) {
        let cy = cy.min(self.lines.len() - 1);
        let hsize = self.hsize();
        self.top = if cy < hsize { cy } else { hsize };
        self.cy = cy;
        self.cx = self.snap(cy, cx);
        self.lastcx = self.cx;
    }

    /// top_line_time: when the top line of the view entered the history (0 if it did not).
    pub fn top_line_time(&self) -> u64 {
        self.lines.get(self.top).map_or(0, |l| l.time)
    }

    /// copy_cursor_hyperlink: window_copy_cursor_hyperlink_cb reads the drawn copy-mode
    /// screen (line numbers and the position indicator included) at the cursor's column
    /// in the backing line, so an indicator over the link hides it and line numbers
    /// shift what is read.
    pub fn cursor_hyperlink(&self, style: &RenderStyle) -> Option<String> {
        let (lines, _) = self.render_with(style);
        lines.get(self.cy.checked_sub(self.top)?)?.hyperlink_at(self.cx).map(|l| l.uri.clone())
    }

    /// window_copy_view_init: command output, no line numbers and no refresh.
    pub fn set_view_mode(&mut self) {
        self.view = true;
        self.line_numbers = 0;
    }

    /// The mode is view-mode (window_view_mode), not copy-mode.
    pub fn is_view_mode(&self) -> bool {
        self.view
    }

    /// `copy-mode -e`: leave copy mode once scrolled back to the bottom.
    pub fn set_exit_on_bottom(&mut self, on: bool) {
        self.exit_on_bottom = on;
    }

    /// `copy-mode -H`: hide the position indicator.
    pub fn set_show_position(&mut self, on: bool) {
        self.show_position = on;
    }

    /// The copy-mode-line-numbers option for this pane's window.
    pub fn set_line_number_option(&mut self, opt: LineNumbers) {
        self.ln_option = opt;
    }

    /// window_copy_set_line_numbers: copy-mode entered with the mouse keeps them off.
    pub fn set_line_numbers(&mut self, enabled: bool) {
        self.set_line_numbers1(enabled, false);
    }

    fn set_line_numbers1(&mut self, enabled: bool, force: bool) {
        self.line_numbers = if !enabled {
            0
        } else if force && self.ln_option == LineNumbers::Off {
            2
        } else {
            1
        };
    }

    /// window_copy_line_number_mode.
    pub fn line_number_mode(&self) -> LineNumbers {
        match self.line_numbers {
            0 => LineNumbers::Off,
            2 if self.ln_option == LineNumbers::Off => LineNumbers::Default,
            _ => self.ln_option,
        }
    }

    pub fn line_numbers_active(&self) -> bool {
        self.line_number_mode() != LineNumbers::Off
    }

    fn line_numbers_absolute(&self) -> bool {
        matches!(self.line_number_mode(), LineNumbers::Absolute | LineNumbers::Relative | LineNumbers::Hybrid)
    }

    /// window_copy_line_number_width: digits for every line (at least 3) and a space.
    pub fn line_number_width(&self) -> usize {
        if !self.line_numbers_active() {
            return 0;
        }
        let mut lines = self.lines.len() + 1;
        let mut digits = 1;
        while lines >= 10 {
            lines /= 10;
            digits += 1;
        }
        digits.max(3) + 1
    }

    /// Columns left for the text beside the line numbers.
    fn content_width(&self) -> usize {
        let w = self.line_number_width();
        if w == 0 { self.width } else if w >= self.width { 1 } else { self.width - w }
    }

    /// window_copy_cursor_offset: screen column of text column `cx`.
    pub fn cursor_offset(&self, cx: usize) -> usize {
        let w = self.line_number_width();
        if w == 0 {
            return cx;
        }
        if cx >= self.content_width() { self.width - 1 } else { w + cx }
    }

    /// window_copy_cursor_unoffset: text column of screen column `vx`.
    pub fn cursor_unoffset(&self, vx: usize) -> usize {
        let w = self.line_number_width();
        if w == 0 {
            return vx;
        }
        if vx < w { 0 } else { (vx - w).min(self.content_width() - 1) }
    }

    /// copy_position / copy_position_limit: absolute line numbers when they are shown
    /// that way, else the scroll position and history size.
    pub fn position(&self) -> (usize, usize) {
        if self.line_numbers_absolute() {
            (self.top + 1, self.lines.len())
        } else {
            (self.scroll_offset(), self.hsize())
        }
    }

    /// A jump waits for its character or the repeat count is being typed: the next key
    /// goes to the engine whatever it is bound to.
    pub fn waiting_for_key(&self) -> bool {
        self.pending_jump.is_some() || self.count_open
    }

    /// The repeat count is being typed (tmux's numeric command-prompt is open).
    pub fn counting(&self) -> bool {
        self.count_open && self.pending_jump.is_none()
    }

    /// A key that is not a digit closes the numeric prompt: the count is kept for the
    /// command that key runs from the copy-mode table (send -N, then the key again).
    pub fn end_count(&mut self) {
        self.count_open = false;
    }

    /// The position indicator is shown (copy-mode -H, toggle-position).
    pub fn position_shown(&self) -> bool {
        self.show_position
    }

    pub fn refresh_active(&self) -> bool {
        self.refresh_active
    }

    /// window_copy_refresh_allowed: not for view mode or a copy of another pane.
    pub fn refresh_allowed(&self) -> bool {
        !self.view && !self.from_other
    }

    /// The automatic refresh waits while a selection is made or the mouse drags.
    pub fn refresh_ready(&self) -> bool {
        self.refresh_active && self.anchor.is_none() && !self.dragging
    }

    /// window_copy_do_refresh: new content from the pane; follow new output when the cursor
    /// was on the bottom line, otherwise keep the same lines (counted from the top) in view.
    pub fn refresh(&mut self, lines: impl Into<Backing>) {
        let follow = self.scroll_offset() == 0 && self.cy - self.top == self.height - 1;
        let from_top = self.top;
        let row = self.cy - self.top;
        self.lines = lines.into();
        self.pad();
        let hsize = self.hsize();
        if follow {
            self.top = hsize;
            self.cy = self.top + self.height - 1;
            self.cx = self.limit(self.cy, false);
        } else if from_top <= hsize {
            self.top = from_top;
            self.cy = self.top + row;
        } else {
            self.top = hsize;
            self.cy = self.top;
        }
        self.cy = self.cy.min(self.lines.len() - 1);
        self.size_changed();
    }

    /// window_copy_size_changed: the selection goes; search marks are found again in the
    /// new content (unless the last search timed out), and incremental searches restart
    /// from here.
    fn size_changed(&mut self) {
        self.search_wrapped = false;
        let search = self.search_marks;
        self.clear_selection();
        self.clear_marks();
        self.mark.0 = self.mark.0.min(self.lines.len() - 1);
        if let Some((needle, _)) = self.search.clone().filter(|(t, _)| search && !t.is_empty() && !self.search_timed_out) {
            self.search_marks = true;
            let started = std::time::Instant::now();
            self.count_search_matches(&needle, false, || started.elapsed().as_millis());
        }
        self.incremental_command_origin = Some(((self.cy, self.cx), self.top));
    }

    /// Cursor position within the view: (row, column).
    pub fn view_cursor(&self) -> (u16, u16) {
        ((self.cy - self.top) as u16, self.cx as u16)
    }

    pub fn view_height(&self) -> usize {
        self.height
    }

    /// Line `i` of the content (0 = oldest history line) and how many there are.
    pub fn line(&self, i: usize) -> Option<&Line> {
        self.lines.get(i)
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The copied pane (window_copy_get_screen): its lines and how many are history,
    /// whatever is in view.
    pub fn backing_screen(&self) -> (&Backing, usize) {
        (&self.lines, self.hsize())
    }

    /// Absolute index of the first line in view.
    pub fn view_top(&self) -> usize {
        self.top
    }

    /// window_copy_start_drag: a click inside an existing selection adjusts it (the end
    /// nearest the click follows the mouse), anywhere else starts a new one. `row`/`col`
    /// are screen coordinates in the pane. A drag always selects characters (tmux resets
    /// the selection mode here).
    pub fn start_drag(&mut self, row: u16, col: u16) {
        self.dragging = true;
        self.leave_word_mode();
        self.pinned = None;
        self.lineflag = None;
        let inside = self.mouse_in_selection(row as usize, col as usize);
        let bounds = self.sel_bounds();
        self.sel_mode = SelMode::Char;
        self.move_to_view(row, col);
        self.line_mode = false;
        self.frozen = None;
        // keep the far end fixed, the near end follows the mouse; dragging the
        // selection's start (CURSORDRAG_SEL) reports the mouse's end as its start
        let anchor = self.anchor;
        let near_anchor = |p: Pos| anchor == Some(p);
        self.anchor = Some(match (inside, bounds) {
            (Some(true), Some((s, e))) => {
                self.ends_swapped ^= near_anchor(s);
                e
            }
            (Some(false), Some((s, e))) => {
                self.ends_swapped ^= near_anchor(e);
                s
            }
            _ => {
                self.ends_swapped = false;
                (self.cy, self.cx)
            }
        });
    }

    /// window_copy_mouse_in_selection: whether a screen cell is in the selection and
    /// whether it is nearer the start (true) or the end (false).
    fn mouse_in_selection(&self, row: usize, col: usize) -> Option<bool> {
        let (s, e) = self.sel_bounds()?;
        let y = self.top + row;
        let x = self.cursor_unoffset(col);
        if (y, x) == s {
            return Some(true);
        }
        if (y, x) == e {
            return Some(false);
        }
        let cursor = (self.cy == y && self.cursor_offset(self.cx) == col).then_some(());
        if cursor.is_none() && !self.screen_selection()?.contains(col, row) {
            return None;
        }
        let w = self.width as i64 + 1;
        let at = |p: Pos| p.0 as i64 * w + p.1 as i64;
        let m = at((y, x));
        Some((m - at(s)).abs() <= (m - at(e)).abs())
    }

    /// The other end of a mouse selection moves here.
    pub fn drag_to(&mut self, row: u16, col: u16) {
        self.move_to_view(row, col);
    }

    /// The mouse button was released.
    pub fn end_drag(&mut self) {
        self.dragging = false;
    }

    /// `copy-mode -d`: a page down; true when that should leave copy mode (with -e).
    pub fn page_down_exit(&mut self, exit: bool) -> bool {
        self.trim_cache();
        self.page_down(false);
        exit && self.scroll_offset() == 0 && self.anchor.is_none()
    }

    /// `copy-mode -u`.
    pub fn page_up_once(&mut self) {
        self.trim_cache();
        self.page_up(false);
    }

    /// window_copy_cursor_up/down(scroll_only): scroll the view one line while the cursor
    /// keeps its row on the screen, sticking to the end of lines as tmux does. With vi keys
    /// the cursor first moves a row so it stays on the same text.
    pub fn scroll_line(&mut self, up: bool) {
        self.trim_cache();
        let norect = self.anchor.is_none() || !self.rect;
        // window_copy_scroll_up/down update the selection only when the view moves: at the
        // top or bottom the cursor moves alone and the selection keeps its ends
        let can_scroll = if up { self.top > 0 } else { self.top < self.hsize() };
        let mut held = if can_scroll { None } else { self.slots().filter(|_| self.frozen.is_none()) };
        self.sticky_begin(norect);
        let row = self.cy - self.top;
        if self.vi {
            if up && row < self.height - 1 {
                self.update_cursor(self.cy + 1, self.cx);
            } else if !up && row > 0 {
                self.update_cursor(self.cy - 1, self.cx);
            }
        }
        let row = self.cy - self.top;
        if norect {
            self.cx = self.tlastcx;
        }
        if up {
            self.top = self.top.saturating_sub(1);
        } else {
            self.top = (self.top + 1).min(self.hsize());
        }
        self.cy = (self.top + row).min(self.lines.len() - 1);
        // the remembered column is used as it is (window_copy_scroll_up does not clamp
        // it), unless it is past the line or the cursor was at a line end
        if norect {
            let px = self.len(self.cy);
            if (self.cx >= self.tlastsx && self.cx != px) || self.cx > px {
                self.cx = self.clamp_x(self.cy, px);
                held = None;
            }
        } else {
            self.cx = self.clamp_x(self.cy, self.cx);
        }
        self.cx = self.snap(self.cy, self.cx);
        if let Some((s, e)) = held {
            self.pinned = Some(((self.cy, self.cx), s, e));
        }
    }

    /// window_copy_move_mouse: the cursor goes to a cell of the view (before a command from
    /// a mouse binding) without updating the selection.
    pub fn move_mouse(&mut self, row: u16, col: u16) {
        self.trim_cache();
        let held = self.slots().filter(|_| self.frozen.is_none());
        let row = (row as usize).min(self.height - 1);
        let y = (self.top + row).min(self.lines.len().saturating_sub(1));
        self.update_cursor(y, self.cursor_unoffset(col as usize));
        if let Some((s, e)) = held {
            self.pinned = Some(((self.cy, self.cx), s, e));
        }
    }

    /// Before a vertical move: remember the column unless the cursor is at the end of
    /// its line, then return to the remembered column.
    fn sticky_begin(&mut self, norect: bool) {
        let ox = self.len(self.cy);
        if norect && self.cx != ox {
            self.tlastcx = self.cx;
            self.tlastsx = ox;
        }
        if norect {
            self.cx = self.tlastcx;
        }
    }

    /// After a vertical move: a cursor that was at a line end stays at the line end
    /// (window_copy_update_cursor clamps to the cursor limit first).
    fn sticky_end(&mut self, norect: bool) {
        self.cx = self.clamp_x(self.cy, self.cx);
        if norect {
            let px = self.len(self.cy);
            if (self.cx >= self.tlastsx && self.cx != px) || self.cx > px {
                self.cx = self.clamp_x(self.cy, px);
            }
        }
        self.cx = self.snap(self.cy, self.cx);
    }

    /// Moves the cursor to a cell of the current view (mouse clicks and drags); `col` is
    /// the screen column (line numbers are skipped).
    pub fn move_to_view(&mut self, row: u16, col: u16) {
        self.trim_cache();
        let row = (row as usize).min(self.height - 1);
        let y = (self.top + row).min(self.lines.len().saturating_sub(1));
        self.update_cursor(y, self.cursor_unoffset(col as usize));
        self.lastcx = self.cx;
    }

    /// window_copy_update_cursor: unless in rectangle mode the cursor stays within the
    /// line (with vi keys, on its last character).
    fn update_cursor(&mut self, y: usize, x: usize) {
        self.cy = y;
        self.cx = self.snap(y, self.clamp_x(y, x));
    }

    fn clamp_pos(&self, (y, x): Pos) -> Pos {
        (y, self.snap(y, self.clamp_x(y, x)))
    }

    fn clamp_x(&self, y: usize, x: usize) -> usize {
        if self.rect { x } else { x.min(self.limit(y, false)) }
    }

    pub fn set_word_separators(&mut self, s: &str) {
        self.separators = s.to_string();
    }

    pub fn scroll_offset(&self) -> usize {
        self.hsize() - self.top
    }

    pub fn hsize_lines(&self) -> usize {
        self.hsize()
    }

    pub fn selection_active(&self) -> bool {
        self.anchor.is_some()
    }

    /// selection_active: the selection still follows the cursor.
    pub fn selection_following(&self) -> bool {
        self.anchor.is_some() && self.frozen.is_none()
    }

    /// selection_present: a selection of more than one position.
    pub fn selection_present(&self) -> bool {
        self.selection_ends().is_some_and(|(s, e)| s != e)
    }

    pub fn selection_mode(&self) -> SelMode {
        self.sel_mode
    }

    pub fn rectangle(&self) -> bool {
        self.rect
    }

    /// copy_cursor_x / copy_cursor_y: the cursor on the screen (column in the text).
    pub fn cursor(&self) -> (usize, usize) {
        (self.cx, self.cy - self.top)
    }

    /// The last search text (pane_search_string).
    pub fn last_search(&self) -> Option<&str> {
        self.search.as_ref().map(|s| s.0.as_str())
    }

    /// search_present: search matches are marked.
    pub fn search_present(&self) -> bool {
        self.search_marks && self.search.as_ref().is_some_and(|s| !s.0.is_empty())
    }

    /// window_copy_clear_marks.
    fn clear_marks(&mut self) {
        self.search_marks = false;
        self.found_count = None;
        self.search_count_partial = false;
    }

    /// search_count: how many matches the last full search found; 0 before any search and
    /// none once the marks are cleared.
    pub fn search_count(&self) -> Option<usize> {
        self.found_count
    }

    pub fn search_timed_out(&self) -> bool { self.search_timed_out }

    pub fn search_count_partial(&self) -> bool { self.search_count_partial }

    /// The pane's last search, which search-again repeats upwards (window_copy_common_init).
    pub fn set_pane_search(&mut self, text: &str) {
        if !text.is_empty() {
            self.search = Some((text.into(), false));
            self.pane_search = Some(text.into());
        }
    }

    /// mode-keys, which tmux looks up as copy mode runs.
    pub fn set_vi(&mut self, vi: bool) {
        self.vi = vi;
    }

    /// The wrap-search option.
    pub fn set_wrap_search(&mut self, on: bool) {
        self.wrap_search = on;
    }

    /// search_match: the match under the cursor.
    pub fn search_match(&self) -> Option<String> {
        let (cy, cx) = self.search_cursor();
        if !self.search_present() || cy < self.top || cy >= self.top + self.height {
            return None;
        }
        let needle = &self.search.as_ref()?.0;
        let at = cy * self.width + cx;
        let (start, end) = self.visible_matches(cy, needle).into_iter()
            .find(|&(a, b)| at >= a && at < b)
            .or_else(|| at.checked_sub(1).and_then(|prev| self.visible_matches(prev / self.width, needle).into_iter()
                .find(|&(a, b)| prev >= a && prev < b)))?;
        Some((start..end).filter_map(|i| self.lines.get(i / self.width).map(|l| l.cell_text(i % self.width))).collect())
    }

    /// copy_cursor_word: the word under the cursor.
    pub fn cursor_word(&self) -> String {
        self.word_under_cursor()
    }

    /// copy_cursor_line: the cursor's row (format_grid_line: one grid line, without its
    /// trailing blanks, even when it wraps).
    pub fn cursor_line(&self) -> String {
        let Some(l) = self.lines.get(self.cy) else { return String::new() };
        (0..self.len(self.cy)).map(|x| l.cell_text(x)).collect()
    }

    /// selection_start_* / selection_end_*: the anchor and the other end as (x, y), y
    /// counted from the oldest history line.
    pub fn selection_ends(&self) -> Option<((usize, usize), (usize, usize))> {
        let ((sy, sx), (ey, ex)) = self.slots()?;
        Some(((sx, sy), (ex, ey)))
    }

    /// The selection's ends as tmux keeps them (selx/sely and endselx/endsely).
    fn slots(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        if let Some(p) = self.pinned_slots() {
            return Some(p);
        }
        if let Some(w) = self.word_sel() {
            return Some(w);
        }
        let o = self.sel_other();
        if self.line_mode && self.sel_mode == SelMode::Line {
            // whole lines: from the start of the first to the end of the last
            let (y0, y1) = (a.0.min(o.0), a.0.max(o.0));
            return Some(((y0, 0), (y1, self.len(y1))));
        }
        Some(if self.ends_swapped { (o, a) } else { (a, o) })
    }

    fn pinned_slots(&self) -> Option<(Pos, Pos)> {
        self.pinned.filter(|p| p.0 == (self.cy, self.cx) && self.anchor.is_some() && self.frozen.is_none()).map(|p| (p.1, p.2))
    }

    /// prompt_incremental_start: the prompt opens empty and sends "=".
    pub fn begin_incremental_search(&mut self, forward: bool) {
        self.incremental_prompt = Some((String::new(), forward));
        self.run_incremental(forward, "=");
    }

    /// An edit in the incremental prompt sends "=" and its text (prompt_key's changed:).
    pub fn preview_search(&mut self, text: &str, forward: bool) {
        if self.incremental_prompt.as_ref().is_some_and(|(t, f)| t == text && *f == forward) {
            return;
        }
        self.incremental_prompt = Some((text.to_string(), forward));
        self.run_incremental(forward, &format!("={text}"));
    }

    /// Enter or cancel closes the prompt; nothing is sent (cmd_command_prompt_callback
    /// ignores PROMPT_KEY_CLOSE for incremental prompts), so the cursor stays.
    pub fn finish_incremental_search(&mut self, _cancel: bool) {
        self.incremental_prompt = None;
    }

    fn run_incremental(&mut self, forward: bool, arg: &str) {
        let name = if forward { "search-forward-incremental" } else { "search-backward-incremental" };
        self.run(name, &[arg.to_string()]);
    }

    /// window_copy_resize: the backing is rewrapped for a new width with the cursor kept
    /// on its character, then the view goes to the cursor and size_changed runs.
    pub fn resize(&mut self, width: u16, height: u16) {
        let (width, height) = ((width as usize).max(1), (height as usize).max(1));
        if (width, height) == (self.width, self.height) {
            return;
        }
        let (mut cx, mut cy) = (self.cx, self.cy);
        // screen_resize_cursor: the height first, then the reflow
        self.height = height;
        self.pad();
        if width != self.width {
            let old = std::mem::take(&mut self.lines).into_vec();
            let (wx, wy) = wrap_position(&old, cx, cy);
            let new = reflow(&old, width);
            (cx, cy) = unwrap_position(&new, wx, wy);
            self.lines = new.into();
        }
        self.width = width;
        self.pad();
        self.place_cursor(cx, cy);
        self.size_changed();
        self.trim_cache();
    }

    /// Replace the content keeping the position relative to the bottom.
    pub fn set_lines(&mut self, lines: impl Into<Backing>) {
        let old = self.lines.len();
        self.lines = lines.into();
        self.pad();
        let new = self.lines.len();
        let remap = |y: usize| (new + y).saturating_sub(old).min(new - 1);
        self.top = remap(self.top).min(self.hsize());
        self.cy = remap(self.cy);
        self.anchor = self.anchor.map(|(y, x)| (remap(y), x));
        self.frozen = self.frozen.map(|(y, x)| (remap(y), x));
        self.mark = (remap(self.mark.0), self.mark.1);
        self.cx = self.snap(self.cy, self.cx);
        self.ensure_visible();
    }

    /// window_copy_scroll1 (copy-mode -S, scroll-to-mouse): the scrollbar slider is dragged
    /// so its top is at `my - sl_mpos` (rows from the pane top). True when copy mode should
    /// exit (with `exit`, back at the bottom and nothing selected).
    pub fn scroll_to_slider(&mut self, sl_mpos: usize, my: i64, exit: bool) -> bool {
        self.trim_cache();
        let sb_height = self.height;
        let size = self.hsize();
        let total = size + sb_height;
        let slider_h = ((sb_height as f64 * (sb_height as f64 / total as f64)) as usize).max(1);
        let new_slider_y = if my <= sl_mpos as i64 {
            0
        } else if my - sl_mpos as i64 > (sb_height - slider_h) as i64 {
            (sb_height - slider_h) as i64
        } else {
            my - sl_mpos as i64
        };
        let new_offset = (new_slider_y as f32 * (total as f32 / sb_height as f32)) as i64;
        let offset = self.top as i64;
        let delta = offset - new_offset;
        let norect = self.anchor.is_none() || !self.rect;
        let ox = self.len(self.cy);
        if self.cx != ox {
            self.tlastcx = self.cx;
            self.tlastsx = ox;
        }
        self.cx = self.tlastcx;
        let mut row = self.cy - self.top;
        if delta >= 0 {
            let n = delta as usize;
            if self.top < n {
                self.top = 0;
                row = row.saturating_sub(n);
            } else {
                self.top -= n;
            }
        } else {
            let n = (-delta) as usize;
            let hsize = self.hsize();
            if self.top + n > hsize {
                self.top = hsize;
                row = (row + n).min(self.height - 1);
            } else {
                self.top += n;
            }
        }
        self.cy = (self.top + row).min(self.lines.len() - 1);
        // the selection does not follow the cursor while the slider is dragged
        self.freeze();
        if norect {
            let px = self.len(self.cy);
            if (self.cx >= self.tlastsx && self.cx != px) || self.cx > px {
                self.end_of_line();
            }
        }
        exit && self.scroll_offset() == 0 && self.anchor.is_none()
    }

    /// The scrollbar slider for the current view: (top row, height) within `height` rows.
    pub fn slider(&self) -> (usize, usize) {
        let total = self.hsize() + self.height;
        let slider_h = ((self.height as f64 * (self.height as f64 / total as f64)) as usize).max(1);
        let y = (((self.height + 1) as f64) * (self.top as f64 / total as f64)) as usize;
        (y.min(self.height - 1), slider_h)
    }

    // ----------------------------------------------------------------- bindings

    /// Default bindings for the copy-mode-vi (`vi`) or copy-mode table, as tmux 3.8 lists them.
    pub fn default_bindings(vi: bool) -> &'static [(&'static str, &'static str)] {
        if vi { VI_BINDINGS } else { EMACS_BINDINGS }
    }

    /// Handle one key (tmux key name) as the default table binds it. Commands come back
    /// as `Action::Run` for the caller to run (`send-keys -X`); a repeat count started with
    /// digits (vi) or M-digits (emacs) is kept for them, like tmux's numeric prompt.
    pub fn key(&mut self, key: &str) -> Action {
        let key = normalize_key(key);
        if let Some(kind) = self.pending_jump.take() {
            return match key_char(key) {
                Some(c) => {
                    let name = match kind {
                        JumpKind::Forward => "jump-forward",
                        JumpKind::Backward => "jump-backward",
                        JumpKind::ToForward => "jump-to-forward",
                        JumpKind::ToBackward => "jump-to-backward",
                    };
                    Action::Run(name.into(), vec![c], false)
                }
                None => {
                    self.count = None;
                    Action::None
                }
            };
        }
        // the numeric prompt is open: digits add to the count
        if self.count_open
            && let Some(d) = key.parse::<usize>().ok().filter(|_| key.len() == 1)
        {
            self.count = Some((self.count.unwrap_or(0) * 10 + d).min(100_000));
            return Action::None;
        }
        self.count_open = false;
        let bound = Self::default_bindings(self.vi).iter().find(|(k, _)| *k == key).and_then(|(_, c)| internal(c));
        match bound {
            Some(Internal::Count(d)) => {
                self.count = Some(d);
                self.count_open = true;
                Action::None
            }
            Some(Internal::Command(name)) => Action::Run(name.into(), Vec::new(), false),
            Some(Internal::CursorWord(forward)) => {
                let name = if forward { "search-forward" } else { "search-backward" };
                Action::Run(name.into(), vec!["#{copy_cursor_word}".into()], true)
            }
            Some(Internal::Prompt(name)) => Action::Run(format!("{name}-prompt"), Vec::new(), false),
            None => {
                self.count = None;
                Action::None
            }
        }
    }

    /// `send-keys -N`: the repeat count for the next command.
    pub fn set_count(&mut self, n: usize) {
        self.count = Some(n.clamp(1, 100_000));
        self.count_open = false;
    }

    /// Run a named copy-mode command (`send-keys -X name args`). A pending repeat count
    /// (from digit keys or `send-keys -N`) is applied and consumed.
    pub fn run(&mut self, command: &str, args: &[String]) -> Action {
        self.count_open = false;
        if !command.starts_with("search-") {
            self.search_wrapped = false;
        }
        // most commands update the selection from the cursor again; these leave it (or
        // update it only when they move the cursor)
        const KEEP: &[&str] = &[
            "jump-", "search-", "set-mark", "toggle-position", "line-numbers-", "refresh-", "scroll-exit-", "copy-",
            "append-", "pipe", "selection-mode", "next-matching-bracket", "previous-matching-bracket", "scroll-up",
            "scroll-down", "other-end", "cancel", "next-prompt", "previous-prompt", "stop-selection",
        ];
        if !KEEP.iter().any(|k| command.starts_with(k)) {
            self.pinned = None;
        }
        let action = self.run_inner(command, args);
        // window_copy_command: most commands clear the search marks (emacs only for motion)
        if !command.starts_with("search-") && self.search_marks {
            let clear = COMMANDS.iter().find(|c| c.0 == command).map_or(Clear::Never, |c| c.2);
            if clear == Clear::Always || (clear == Clear::EmacsOnly && !self.vi) {
                self.clear_marks();
                // searchx = searchy = -1
                self.incremental_command_origin = None;
            }
        }
        self.trim_cache();
        action
    }

    fn run_inner(&mut self, command: &str, args: &[String]) -> Action {
        self.pending_jump = None;
        let n = self.take_count();
        let arg = args.first().map(String::as_str);
        let seps = self.separators.clone();
        let exit = self.exit_on_bottom;
        match command {
            "cancel" => return Action::Cancel,
            "cursor-up" => self.vertical(-(n as isize)),
            "cursor-down" => self.vertical(n as isize),
            "cursor-down-and-cancel" => {
                let row = self.cy - self.top;
                self.vertical(n as isize);
                if self.cy - self.top == row && self.scroll_offset() == 0 {
                    return Action::Cancel;
                }
            }
            // window_copy_cursor_left: wraps to the end of the line above
            // grid_reader_cursor_left steps onto the padding of a wide character or tab (the
            // next step goes past its start), it does not snap to the start
            "cursor-left" => {
                let mut p = (self.cy, self.cx);
                for _ in 0..n {
                    p = self.reader_left(p, true);
                }
                let y = p.0.min(self.lines.len() - 1);
                self.cy = y;
                self.cx = self.clamp_x(y, p.1);
                self.lastcx = self.cx;
                self.ensure_visible();
            }
            "cursor-right" => {
                let all = self.anchor.is_some() && self.rect;
                self.repeat_move(n, |s, p| s.cursor_right(p, all))
            }
            "cursor-centre-vertical" => {
                let y = (self.top + self.height / 2).min(self.lines.len() - 1);
                self.update_cursor(y, self.cx);
            }
            "cursor-centre-horizontal" => self.update_cursor(self.cy, self.width / 2),
            "start-of-line" => {
                let y = self.logical_start(self.cy);
                self.set_cursor(y, 0)
            }
            "end-of-line" => self.end_of_line(),
            "back-to-indentation" => {
                if let Some((y, x)) = self.indentation(self.cy) {
                    self.set_cursor(y, x)
                }
            }
            // each step lands where window_copy_update_cursor puts the cursor
            "next-word" => self.repeat_move(n, |s, p| Some(s.clamp_pos(s.next_word(p, &seps)))),
            "next-space" => self.repeat_move(n, |s, p| Some(s.clamp_pos(s.next_word(p, "")))),
            "next-word-end" => self.repeat_move(n, |s, p| Some(s.clamp_pos(s.next_word_end(p, &seps)))),
            "next-space-end" => self.repeat_move(n, |s, p| Some(s.clamp_pos(s.next_word_end(p, "")))),
            "previous-word" => self.repeat_move(n, |s, p| Some(s.previous_word(p, &seps))),
            "previous-space" => self.repeat_move(n, |s, p| Some(s.previous_word(p, ""))),
            "next-matching-bracket" => {
                for _ in 0..n {
                    if !self.next_matching_bracket() {
                        break;
                    }
                }
            }
            "previous-matching-bracket" => (0..n).for_each(|_| self.previous_matching_bracket()),
            "top-line" => self.set_row(0),
            "middle-line" => self.set_row((self.height - 1) / 2),
            "bottom-line" => self.set_row(self.height - 1),
            "history-top" => {
                self.top = 0;
                self.set_cursor(0, 0)
            }
            "history-bottom" => {
                self.top = self.hsize();
                self.cy = self.lines.len() - 1;
                self.cx = self.limit(self.cy, false);
                self.lastcx = self.cx;
            }
            "page-up" => (0..n).for_each(|_| self.page_up(false)),
            "halfpage-up" => (0..n).for_each(|_| self.page_up(true)),
            "page-down" | "halfpage-down" | "page-down-and-cancel" | "halfpage-down-and-cancel" => {
                let half = command.starts_with("half");
                let exit = exit || command.ends_with("-and-cancel");
                for _ in 0..n {
                    self.page_down(half);
                    if exit && self.scroll_offset() == 0 && self.anchor.is_none() {
                        return Action::Cancel;
                    }
                }
            }
            "scroll-up" => {
                // at the top nothing can change
                if self.scroll_offset() == self.hsize() {
                    return Action::None;
                }
                if self.anchor.is_some() && !self.dragging {
                    // with a selection (and no mouse drag) only the view moves
                    self.freeze();
                    self.view_scroll(-(n as isize));
                    return Action::None;
                }
                (0..n).for_each(|_| self.scroll_line(true));
            }
            "scroll-down" | "scroll-down-and-cancel" => {
                let and_cancel = command == "scroll-down-and-cancel";
                if !and_cancel && self.scroll_offset() == 0 {
                    return if exit && self.anchor.is_none() { Action::Cancel } else { Action::None };
                }
                if self.anchor.is_some() && !self.dragging {
                    self.freeze();
                    self.view_scroll(n as isize);
                    return Action::None;
                }
                (0..n).for_each(|_| self.scroll_line(false));
                if self.scroll_offset() == 0 && (and_cancel || (exit && self.anchor.is_none())) {
                    return Action::Cancel;
                }
            }
            "scroll-top" => self.scroll_row_to(0),
            "scroll-middle" => self.scroll_row_to((self.height - 1) / 2),
            "scroll-bottom" => self.scroll_row_to(self.height - 1),
            "recentre-top-bottom" => self.recentre_top_bottom(),
            "scroll-exit-on" => self.exit_on_bottom = true,
            "scroll-exit-off" => self.exit_on_bottom = false,
            "scroll-exit-toggle" => self.exit_on_bottom = !self.exit_on_bottom,
            "begin-selection" => {
                self.anchor = Some((self.cy, self.cx));
                self.ends_swapped = false;
                self.lineflag = None;
                self.word_origin = None;
                self.pinned = None;
                self.frozen = None;
                self.line_mode = false;
                self.sel_mode = SelMode::Char;
            }
            "stop-selection" => {
                self.freeze();
                self.line_mode = false;
                self.lineflag = None;
                self.sel_mode = SelMode::Char;
            }
            "select-line" => {
                let origin = (self.cy, self.cx);
                self.rect = false;
                let y = self.logical_start(self.cy);
                self.line_mode = true;
                self.frozen = None;
                self.ends_swapped = false;
                self.lineflag = Some(true);
                self.word_origin = None;
                self.pinned = None;
                self.anchor = Some((y, 0));
                self.cy = y;
                self.end_of_line();
                for _ in 1..n {
                    self.vertical(1);
                    self.end_of_line();
                }
                self.sel_mode = SelMode::Line;
                self.selection_reset = ((y, 0), (self.cy, self.len(self.cy)), origin);
            }
            "select-word" => {
                // window_copy_cmd_select_word: back to the start of the word, then to its end
                self.rect = false;
                let start = self.reader_previous_word((self.cy, self.cx), &seps, false, !self.vi);
                let (py, px) = start;
                let (mut ny, mut nx) = (py, px + 1);
                if self.wrapped(ny) && nx > self.width - 1 && ny + 1 < self.lines.len() {
                    nx = 0;
                    ny += 1;
                }
                let single = px < self.len(py) && self.in_set((ny, nx), WHITESPACE);
                let before = (self.cy, self.cx);
                self.line_mode = false;
                self.frozen = None;
                self.ends_swapped = false;
                self.lineflag = Some(true);
                self.anchor = Some(start);
                // a single character word: the cursor stays on it
                let end = if single { start } else { self.next_word_end(start, &seps) };
                self.set_cursor(end.0, end.1);
                let end = (self.cy, self.cx);
                self.sel_mode = SelMode::Word;
                // where select-word was run, no further than the word's end
                let d = if before.0 > end.0 {
                    end
                } else {
                    (before.0, before.1.min(end.1))
                };
                self.selection_reset = (start, end, d);
                self.word_origin = Some(self.selection_reset);
                self.pinned = Some((end, start, end));
            }
            "selection-mode" => match arg.map(str::to_ascii_lowercase).as_deref() {
                None | Some("char") | Some("c") => {
                    self.materialize();
                    self.sel_mode = SelMode::Char;
                }
                Some("word") | Some("w") => {
                    let slots = self.slots();
                    self.materialize();
                    self.sel_mode = SelMode::Word;
                    self.word_origin = Some(self.selection_reset);
                    self.pinned = slots.map(|(s, e)| ((self.cy, self.cx), s, e));
                },
                Some("line") | Some("l") => {
                    self.materialize_word();
                    self.sel_mode = SelMode::Line;
                    // an existing selection becomes whole lines
                    if let Some((s, e)) = self.sel_bounds() {
                        self.rect = false;
                        self.line_mode = true;
                        let (sy, ey) = (self.logical_start(s.0), self.logical_end(e.0));
                        let fixed = if self.ends_swapped { e } else { s };
                        let cursor_first = (self.cy, self.cx) < fixed && self.frozen.is_none();
                        self.selection_reset = ((sy, 0), (ey, self.len(ey)), fixed);
                        self.lineflag = Some(!cursor_first);
                        if self.frozen.is_some() {
                            self.anchor = Some((sy, 0));
                            self.frozen = Some((ey, self.max_x(ey)));
                        } else if cursor_first {
                            self.anchor = Some((ey, self.len(ey)));
                            self.set_cursor(sy, 0);
                        } else {
                            self.anchor = Some((sy, 0));
                            self.cy = ey;
                            self.cx = self.limit(ey, false);
                            self.ensure_visible();
                        }
                        self.word_origin = None;
                        self.pinned = None;
                    }
                }
                Some(_) => {}
            },
            "rectangle-toggle" => self.rectangle_set(!self.rect),
            "rectangle-on" => self.rectangle_set(true),
            "rectangle-off" => self.rectangle_set(false),
            "clear-selection" => {
                self.clear_selection();
                let px = self.limit(self.cy, self.rect);
                if self.cx > px {
                    self.cx = px;
                }
            }
            "other-end" => {
                // window_copy_cmd_other_end: the selection mode goes back to char
                self.materialize();
                self.word_origin = None;
                self.sel_mode = SelMode::Char;
                if (n % 2) != 0 {
                    self.other_end();
                }
            }
            "copy-selection" | "copy-pipe" => return self.copy_selection(true, false, false),
            "copy-selection-no-clear" | "copy-pipe-no-clear" => return self.copy_selection(false, false, false),
            "copy-selection-and-cancel" | "copy-pipe-and-cancel" => return self.copy_selection(true, true, false),
            "append-selection" => return self.copy_selection(true, false, true),
            "append-selection-and-cancel" => return self.copy_selection(true, true, true),
            "pipe" | "pipe-no-clear" | "pipe-and-cancel" => {
                let text = self.selection_text();
                if command != "pipe-no-clear" {
                    self.clear_selection();
                }
                let cancel = command == "pipe-and-cancel";
                if text.is_empty() {
                    return if cancel { Action::Cancel } else { Action::None };
                }
                return Action::Pipe { text, cancel };
            }
            "copy-line" | "copy-line-and-cancel" | "copy-pipe-line" | "copy-pipe-line-and-cancel" => {
                return self.copy_lines(n, false, command.ends_with("-and-cancel"));
            }
            "copy-end-of-line"
            | "copy-end-of-line-and-cancel"
            | "copy-pipe-end-of-line"
            | "copy-pipe-end-of-line-and-cancel" => {
                return self.copy_lines(n, true, command.ends_with("-and-cancel"));
            }
            "search-forward-prompt" => return prompt("(search down)", "search-forward"),
            "search-backward-prompt" => return prompt("(search up)", "search-backward"),
            "search-forward-incremental-prompt" => return prompt("(search down)", "search-forward-incremental"),
            "search-backward-incremental-prompt" => return prompt("(search up)", "search-backward-incremental"),
            "goto-line-prompt" => return prompt("(goto line)", "goto-line"),
            "search-forward" | "search-backward" => {
                // an empty search string does nothing
                if let Some(t) = arg.filter(|t| !t.is_empty()) {
                    let forward = command == "search-forward";
                    self.search_timed_out = false;
                    self.search_regex = true;
                    self.search = Some((t.to_string(), forward));
                    self.do_search(t, forward, n);
                }
            }
            "search-forward-incremental" | "search-backward-incremental" => {
                self.search_timed_out = false;
                if let Some(arg) = arg.filter(|a| !a.is_empty()) {
                    let (prefix, text) = arg.split_at(arg.chars().next().unwrap().len_utf8());
                    let pattern = literal_search_pattern(text);
                    // a changed string starts again from the saved row, at its end
                    let ss = if self.search_regex { text } else { pattern.as_str() };
                    if let Some((pos, top)) = self.incremental_command_origin {
                        if self.last_search().is_some_and(|l| l != ss) {
                            self.top = top;
                            self.cy = pos.0.min(self.lines.len() - 1);
                            self.cx = self.limit(self.cy, false);
                        }
                    } else {
                        self.incremental_command_origin = Some(((self.cy, self.cx), self.top));
                    }
                    if text.is_empty() { self.clear_marks(); }
                    else if ["=", "+", "-"].contains(&prefix) {
                        let forward = if prefix == "=" { command.starts_with("search-forward") } else { prefix == "+" };
                        self.search_regex = false;
                        self.search = Some((pattern.clone(), forward));
                        if !self.do_search(&pattern, forward, n) { self.clear_marks(); }
                    }
                }
            }
            "search-forward-text" | "search-backward-text" => {
                let forward = command == "search-forward-text";
                let text = match arg {
                    Some(t) => t.to_string(),
                    None => self.word_under_cursor(),
                };
                if !text.is_empty() {
                    let pattern = literal_search_pattern(&text);
                    self.search_timed_out = false;
                    self.search_regex = false;
                    self.search = Some((pattern.clone(), forward));
                    self.do_search(&pattern, forward, n);
                }
            }
            "search-again" | "search-reverse" => {
                if let Some((t, f)) = self.search.clone() {
                    let forward = if command == "search-again" { f } else { !f };
                    self.do_search(&t, forward, n);
                }
            }
            "jump-forward" | "jump-backward" | "jump-to-forward" | "jump-to-backward" | "jump-forward-prompt"
            | "jump-backward-prompt" | "jump-to-forward-prompt" | "jump-to-backward-prompt" => {
                let kind = match command.trim_end_matches("-prompt") {
                    "jump-forward" => JumpKind::Forward,
                    "jump-backward" => JumpKind::Backward,
                    "jump-to-forward" => JumpKind::ToForward,
                    _ => JumpKind::ToBackward,
                };
                match arg.filter(|a| !a.is_empty()) {
                    Some(c) => {
                        let c = c.chars().next().unwrap().to_string();
                        self.jump = Some((kind, c.clone()));
                        self.do_jump(kind, &c, n);
                    }
                    None => {
                        // Wait for the character; keep the count for it.
                        self.pending_jump = Some(kind);
                        if n > 1 {
                            self.count = Some(n);
                        }
                    }
                }
            }
            "jump-again" | "jump-reverse" => {
                if let Some((kind, c)) = self.jump.clone() {
                    let kind = if command == "jump-again" { kind } else { kind.reverse() };
                    self.do_jump(kind, &c, n);
                }
            }
            "goto-line" => {
                // window_copy_goto_line: the view moves, the cursor keeps its place on screen
                if let Some(l) = arg.and_then(|a| a.trim().parse::<i64>().ok()).filter(|l| (-1..=i32::MAX as i64).contains(l)) {
                    let row = self.cy - self.top;
                    let hsize = self.hsize();
                    let oy = if self.line_numbers_absolute() {
                        // absolute line numbers: the first history line is 1
                        let line = l.clamp(1, hsize as i64 + 1) as usize;
                        hsize - (line - 1)
                    } else if l < 0 || l as usize > hsize {
                        hsize
                    } else {
                        l as usize
                    };
                    self.top = hsize - oy;
                    self.cy = (self.top + row).min(self.lines.len() - 1);
                }
            }
            "next-prompt" | "previous-prompt" => {
                // window_copy_cursor_prompt: the next line marked by OSC 133 A (or C with
                // -o) goes to the top of the view; the repeat count is not used
                let flag = if args.iter().any(|s| s == "-o") { vt100::ROW_START_OUTPUT } else { vt100::ROW_START_PROMPT };
                let found = if command == "next-prompt" {
                    (self.cy + 1..self.lines.len()).find(|&y| self.lines.flags(y) & flag != 0)
                } else {
                    (0..self.cy).rev().find(|&y| self.lines.flags(y) & flag != 0)
                };
                if let Some(y) = found {
                    self.pinned = None;
                    self.place_cursor(0, y);
                }
            }
            "previous-paragraph" => (0..n).for_each(|_| self.previous_paragraph()),
            "next-paragraph" => (0..n).for_each(|_| self.next_paragraph()),
            "set-mark" => {
                self.mark = (self.cy, self.cx);
                self.show_mark = true;
            }
            "jump-to-mark" => {
                let m = (self.mark.0.min(self.lines.len() - 1), self.mark.1);
                self.mark = (self.cy, self.cx);
                self.show_mark = true;
                let hsize = self.hsize();
                self.top = m.0.min(hsize);
                self.cy = m.0;
                self.cx = m.1;
                self.lastcx = self.cx;
            }
            "toggle-position" => self.show_position = !self.show_position,
            "line-numbers-on" => self.set_line_numbers1(true, true),
            "line-numbers-off" => self.set_line_numbers1(false, false),
            "line-numbers-toggle" => {
                let on = !self.line_numbers_active();
                self.set_line_numbers1(on, true);
            }
            "refresh-on" => {
                if self.refresh_allowed() {
                    self.refresh_active = true;
                }
            }
            "refresh-off" => self.refresh_active = false,
            "refresh-toggle" => self.refresh_active = !self.refresh_active && self.refresh_allowed(),
            "refresh-now" => {
                if self.refresh_allowed() {
                    return Action::Refresh;
                }
            }
            _ => {}
        }
        Action::None
    }

    /// Stop the selection following the cursor (tmux CURSORDRAG_NONE).
    fn freeze(&mut self) {
        self.lineflag = None;
        // both ends stay where tmux last put them
        if let Some((s, e)) = self.slots().filter(|_| self.frozen.is_none()) {
            self.ends_swapped = false;
            self.anchor = Some(s);
            self.frozen = Some(e);
            self.word_origin = None;
            self.pinned = None;
        }
    }

    /// The ends of a word selection (window_copy_synchronize_cursor_end with SEL_WORD):
    /// with the cursor before where select-word was run the start goes back to the start
    /// of the cursor's word, else the end goes on to the end of its word.
    fn word_sel(&self) -> Option<(Pos, Pos)> {
        let (selr, endselr, d) = self.word_origin.filter(|_| self.sel_mode == SelMode::Word && self.frozen.is_none() && self.anchor.is_some())?;
        let c = (self.cy, self.cx);
        let seps = self.separators.as_str();
        if d > c {
            return Some((self.reader_previous_word(c, seps, false, true), endselr));
        }
        let end = if c.1 >= self.len(c.0) || !self.in_set((c.0, c.1 + 1), WHITESPACE) {
            // window_copy_cursor_next_word_end_pos
            self.next_word_end(c, seps)
        } else {
            c
        };
        Some((selr, end))
    }

    /// The selection stops being a word selection: its ends become the anchor and the
    /// cursor's end (as tmux's selection slots stay).
    fn leave_word_mode(&mut self) {
        self.materialize_word();
        self.word_origin = None;
    }

    /// The selection becomes a character selection between the ends tmux keeps (until
    /// the next update): the end the cursor does not drag becomes the anchor.
    fn materialize(&mut self) {
        let line = self.line_mode && self.sel_mode == SelMode::Line;
        if self.frozen.is_some() || !(line || self.pinned_slots().is_some() || self.word_sel().is_some()) {
            return;
        }
        let Some((s, e)) = self.slots() else { return };
        self.anchor = Some(if self.ends_swapped { e } else { s });
        self.pinned = Some(((self.cy, self.cx), s, e));
        if line {
            self.line_mode = false;
        }
    }

    /// A word selection's fixed end becomes the anchor.
    fn materialize_word(&mut self) {
        if let Some((s, e)) = self.pinned_slots().or_else(|| self.word_sel()) {
            self.anchor = Some(if self.ends_swapped { e } else { s });
        }
    }

    fn clear_selection(&mut self) {
        self.anchor = None;
        self.ends_swapped = false;
        self.lineflag = None;
        self.word_origin = None;
        self.pinned = None;
        self.frozen = None;
        self.line_mode = false;
        self.sel_mode = SelMode::Char;
    }

    /// window_copy_rectangle_set: the cursor may need to come back within the line.
    fn rectangle_set(&mut self, on: bool) {
        self.pinned = None;
        self.rect = on;
        self.lineflag = None;
        let px = self.limit(self.cy, self.rect);
        if self.cx > px {
            self.cx = px;
        }
    }

    /// window_copy_other_end: the cursor goes to the other end of the selection.
    fn other_end(&mut self) {
        let Some((mut s, mut e)) = self.slots() else { return };
        // window_copy_cmd_other_end switches to char before synchronizing the cursor.
        self.ends_swapped = self.frozen.is_none() && !self.ends_swapped;
        let p = if self.ends_swapped { s } else { e };
        self.cy = p.0;
        self.cx = p.1.min(self.limit(p.0, self.rect));
        if self.ends_swapped {
            s = (self.cy, self.cx);
            self.anchor = Some(e);
        } else {
            e = (self.cy, self.cx);
            self.anchor = Some(s);
        }
        self.frozen = None;
        self.pinned = Some(((self.cy, self.cx), s, e));
        if let Some(lr) = self.lineflag.as_mut() {
            *lr = !*lr;
        }
        self.ensure_visible();
        self.lastcx = self.cx;
    }

    /// Cursor to a row of the view at column 0 (top-line, middle-line, bottom-line).
    fn set_row(&mut self, row: usize) {
        self.cy = (self.top + row).min(self.lines.len() - 1);
        self.cx = 0;
        self.lastcx = 0;
    }

    /// window_copy_scroll_up/down: move the view by `ny` lines (positive towards the
    /// bottom) while the cursor keeps its row on the screen.
    fn view_scroll(&mut self, ny: isize) {
        let row = self.cy - self.top;
        if ny >= 0 {
            self.top = (self.top + ny as usize).min(self.hsize());
        } else {
            self.top = self.top.saturating_sub(ny.unsigned_abs());
        }
        self.cy = (self.top + row).min(self.lines.len() - 1);
    }

    /// window_copy_cmd_scroll_to: scroll so the cursor line is on row `to`, if the whole
    /// distance can be scrolled.
    fn scroll_row_to(&mut self, to: usize) {
        let row = self.cy - self.top;
        if row > to && self.scroll_offset() >= row - to {
            self.view_scroll((row - to) as isize);
            self.cy -= row - to;
        } else if row < to && self.top >= to - row {
            self.view_scroll(-((to - row) as isize));
            self.cy += to - row;
        }
    }

    /// recentre-top-bottom (emacs C-l): the cursor line goes to the middle, then the top,
    /// then the bottom of the view.
    fn recentre_top_bottom(&mut self) {
        let row = self.cy - self.top;
        let sy = self.height - 1;
        let sm = sy / 2;
        if self.recentre.1 != self.cy {
            self.recentre = (0, self.cy);
        }
        let target = self.recentre.0;
        self.recentre.0 = (target + 1) % 3;
        let y = self.cy;
        match target {
            0 => {
                if row < sm {
                    self.view_scroll(-((sm - row) as isize));
                } else if row > sm {
                    self.view_scroll((row - sm) as isize);
                }
            }
            1 => self.view_scroll(row as isize),
            _ => self.view_scroll(-((sy - row) as isize)),
        }
        // the cursor stays on its line
        self.cy = y;
        self.ensure_visible();
    }

    // ----------------------------------------------------------------- render

    /// The `height` lines to display plus the cursor position within them.
    pub fn render(&self) -> (Vec<Line>, (u16, u16)) {
        self.render_with(&RenderStyle::default())
    }

    pub fn render_with_style(&self, mode_style: Style) -> (Vec<Line>, (u16, u16)) {
        self.render_with(&RenderStyle { selection: mode_style, position: mode_style, ..RenderStyle::default() })
    }

    /// window_copy_write_line for every row: line numbers, the text with the current line,
    /// mark, search match and selection styles, the position indicator and the cursor.
    pub fn render_with(&self, st: &RenderStyle) -> (Vec<Line>, (u16, u16)) {
        self.render_rows(st, 0..self.height)
    }

    /// render_with drawing only `rows` of the screen; the others are empty lines (a huge
    /// floating pane shows a few of its rows).
    pub fn render_rows(&self, st: &RenderStyle, rows: std::ops::Range<usize>) -> (Vec<Line>, (u16, u16)) {
        let mut out = Vec::with_capacity(self.height);
        let lnw = self.line_number_width();
        let content = self.content_width();
        let mode = self.line_number_mode();
        let needle = self.search.as_ref().map(|(t, _)| t.as_str()).filter(|t| !t.is_empty() && self.search_marks);
        let cur_row = self.cy - self.top;
        let screen_sel = self.screen_selection();
        let text_x = lnw.min(self.width - 1);
        // the cursor line and the current match follow tmux's cursor (search_cursor)
        let (search_cy, search_cx) = self.search_cursor();
        for row in 0..self.height {
            if !rows.contains(&row) {
                out.push(Line::default());
                continue;
            }
            let y = self.top + row;
            let mut line = Line { cells: Vec::with_capacity(self.width), wrapped: false, ..Default::default() };
            if let Some(src) = self.lines.get(y) {
                line.wrapped = src.wrapped;
                line.flags = src.flags;
                line.time = src.time;
                line.metadata = src.metadata.iter().filter(|(x, _)| (**x as usize) < content).map(|(&x,m)| ((x as usize + text_x) as u16,m.clone())).collect();
                line.cells.extend(src.cells.iter().take(content).cloned());
                // A wide char cut at the right edge: blank its lead (also one in the last
                // column, whose second half is off the screen).
                if line.cells.len() == content
                    && src.cells.get(content).map_or_else(|| line.cells.last().is_some_and(|c| crate::width::str_width(&c.text) > 1), Cell::is_continuation)
                    && let Some(last) = line.cells.last_mut()
                {
                    last.text = " ".into();
                }
                // padding a reflow moved off its tab's or wide character's row shows
                // nothing in tmux; drawn as a blank, what follows keeps its column
                for x in 0..line.cells.len() {
                    if line.cells[x].text.is_empty() && (x == 0 || unicode_width::UnicodeWidthStr::width(line.cells[x - 1].text.as_str()) != 2) {
                        line.cells[x].text = " ".into();
                    }
                }
            }
            line.cells.resize(content, Cell::default());
            if y == search_cy
                && let Some(cl) = st.current_line
            {
                for c in &mut line.cells {
                    if cl.fg != Color::Default {
                        c.style.fg = cl.fg;
                    }
                    if cl.bg != Color::Default {
                        c.style.bg = cl.bg;
                    }
                    c.style.attrs |= cl.attrs;
                }
            }
            let mark_row = self.show_mark && y == self.mark.0;
            if mark_row {
                for (x, c) in line.cells.iter_mut().enumerate() {
                    c.style.attrs = st.mark.attrs;
                    let (fg, bg) = if x == self.mark.1 { (st.mark.bg, st.mark.fg) } else { (st.mark.fg, st.mark.bg) };
                    c.style.fg = fg;
                    c.style.bg = bg;
                }
            }
            if let Some(needle) = needle {
                for (start, end) in self.visible_matches(y, needle) {
                    let x0 = start.saturating_sub(y * self.width);
                    let x1 = end.saturating_sub(y * self.width).min(self.width);
                    let at = search_cy * self.width + search_cx;
                    let at = if !self.vi && self.searched_down { at.saturating_sub(1) } else { at };
                    let current = search_cy >= self.top && search_cy < self.top + self.height && at >= start && at < end;
                    let ms = if current { st.current_match } else { st.matched };
                    for (x, c) in line.cells.iter_mut().enumerate().take(x1.min(content)).skip(x0) {
                        let inv = mark_row && x == self.mark.1;
                        c.style.attrs = ms.attrs;
                        let (fg, bg) = if inv { (ms.bg, ms.fg) } else { (ms.fg, ms.bg) };
                        c.style.fg = fg;
                        c.style.bg = bg;
                    }
                }
            }
            if let Some(sel) = &screen_sel {
                // a wide character is selected when its first cell is (screen_write_cell)
                let mut lead = 0;
                for (x, c) in line.cells.iter_mut().enumerate() {
                    if !c.is_continuation() {
                        lead = x;
                    }
                    if sel.contains(if lnw > 0 { self.cursor_offset(lead) } else { lead }, row) {
                        c.style = select_cell(st, c.style);
                    }
                }
            }
            if lnw > 0 {
                let current = row == cur_row;
                let absolute = y + 1;
                let oy = self.scroll_offset();
                let number = match mode {
                    LineNumbers::Default => if row < oy { oy - row } else { row - oy },
                    LineNumbers::Absolute => absolute,
                    LineNumbers::Hybrid if current => absolute,
                    _ => row.abs_diff(cur_row),
                };
                let text = format!("{number:>w$} ", w = lnw - 1);
                let style = if current { st.current_line_number } else { st.line_number };
                // the text starts at column lnw, which screen_write_cursormove keeps on the
                // screen: a gutter as wide as the pane loses its last column to the text
                let mut cells: Vec<Cell> = text.chars().take(text_x).map(|ch| Cell { text: ch.to_string(), style }).collect();
                cells.append(&mut line.cells);
                cells.truncate(self.width);
                line.cells = cells;
            }
            out.push(line);
        }
        // window_copy_write_line: not with a one line screen (rupper < rlower)
        if self.show_position && self.height > 1 && rows.contains(&0) {
            self.draw_position(&mut out[0], st, text_x, content);
        }
        // a cursor past the last column shows as a '$'
        if self.cx >= content {
            let x = self.cursor_offset(self.cx).min(self.width - 1);
            if let Some(c) = out[cur_row].cells.get_mut(x) {
                *c = Cell { text: "$".into(), style: Style::default() };
            }
        }
        let cursor = (cur_row as u16, self.cursor_offset(self.cx).min(self.width - 1) as u16);
        (out, cursor)
    }

    /// The position indicator (copy-mode-position-format) over the first row's text: only
    /// the cells the format draws are replaced, like format_draw.
    fn draw_position(&self, row: &mut Line, st: &RenderStyle, lnw: usize, content: usize) {
        let text = match &st.position_text {
            Some(t) => t.clone(),
            None => {
                let (pos, limit) = self.position();
                let mut ind = format!("#[align=right][{pos}/{limit}]");
                if self.search_timed_out {
                    ind.push_str(" (timed out)");
                } else if let Some(n) = self.search_count().filter(|&n| n > 0) {
                    let more = if self.search_count_partial { "+" } else { "" };
                    ind.push_str(&format!(" ({n}{more} results)"));
                }
                ind
            }
        };
        if text.is_empty() {
            return;
        }
        let spans = crate::format::spans(&text, st.position);
        // cells the format leaves alone keep the fill: lay out twice with different fills
        let a = crate::format::layout_line(&spans, content, Style { fg: Color::Idx(1), ..Style::default() });
        let b = crate::format::layout_line(&spans, content, Style { fg: Color::Idx(2), ..Style::default() });
        let orig = row.cells.clone();
        let mut drawn = vec![false; orig.len()];
        for (x, (ca, cb)) in a.into_iter().zip(b).enumerate() {
            if ca != cb {
                continue;
            }
            let at = lnw + x;
            if let Some(c) = row.cells.get_mut(at) {
                *c = ca;
                row.metadata.remove(&(at as u16));
                drawn[at] = true;
            }
        }
        // screen_write_overwrite: what is left of a wide character the format drew over
        // becomes spaces
        let blank = |c: &mut Cell| c.text = " ".into();
        for at in (0..orig.len()).filter(|&at| drawn[at]) {
            if orig[at].is_continuation() {
                let mut l = at;
                while l > 0 && orig[l].is_continuation() {
                    l -= 1;
                    if !drawn[l] {
                        blank(&mut row.cells[l]);
                    }
                }
            }
            let mut r = at + 1;
            while r < orig.len() && orig[r].is_continuation() {
                if !drawn[r] {
                    blank(&mut row.cells[r]);
                }
                r += 1;
            }
        }
    }

    // ----------------------------------------------------------------- geometry helpers

    fn pad(&mut self) {
        while self.lines.len() < self.height {
            self.lines.push(Line::default());
        }
    }

    /// Keeps the lines built from a frozen screen to the view and a screen either side of
    /// it, once more than a few screens of them are kept.
    fn trim_cache(&mut self) {
        let h = self.height;
        self.lines.trim(self.top.saturating_sub(h)..self.top + 2 * h, 3 * h + 64);
    }

    fn hsize(&self) -> usize {
        self.lines.len().saturating_sub(self.height)
    }

    fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1).max(1)
    }

    /// `f` of the text of a cell (None past the line's cells), read without keeping the
    /// line, so scans over many lines build none.
    fn with_cell<R>(&self, y: usize, x: usize, f: impl FnOnce(Option<&str>) -> R) -> R {
        self.lines.with_cell(y, x, f)
    }

    fn is_cont(&self, y: usize, x: usize) -> bool {
        self.lines.is_padding(y, x)
    }

    fn wrapped(&self, y: usize) -> bool {
        self.lines.wrapped(y)
    }

    /// Length of the line's text (trailing blanks excluded).
    fn len(&self, y: usize) -> usize {
        self.lines.text_len(y)
    }

    /// Number of cells copied from the line: the whole row when it soft-wraps.
    fn copy_len(&self, y: usize) -> usize {
        if self.wrapped(y) { self.lines.cells_len(y).min(self.width) } else { self.len(y) }
    }

    /// Largest column the cursor may occupy on row `y` (tmux allows one past the text).
    fn max_x(&self, y: usize) -> usize {
        if self.wrapped(y) {
            self.lines.cells_len(y).min(self.width).saturating_sub(1)
        } else {
            self.len(y)
        }
    }

    /// Move `x` off the right half of a wide character.
    fn snap(&self, y: usize, mut x: usize) -> usize {
        while x > 0 && self.is_cont(y, x) {
            x -= 1;
        }
        x
    }

    /// window_copy_cursor_limit: the last column the cursor may take on line `y`. With vi
    /// keys that is the last character, otherwise (or with `onemore`) one past the text.
    fn limit(&self, y: usize, onemore: bool) -> usize {
        if onemore || !self.vi || self.wrapped(y) {
            self.max_x(y)
        } else {
            self.snap(y, self.len(y).saturating_sub(1))
        }
    }

    /// First row of the logical (soft-wrapped) line containing row `y`.
    fn logical_start(&self, mut y: usize) -> usize {
        while y > 0 && self.wrapped(y - 1) {
            y -= 1;
        }
        y
    }

    /// Last row of the logical line containing row `y`.
    fn logical_end(&self, mut y: usize) -> usize {
        while y + 1 < self.lines.len() && self.wrapped(y) {
            y += 1;
        }
        y
    }

    /// grid_reader_cursor_back_to_indentation: the first character that is not a space or
    /// tab on the logical line, if there is one.
    fn indentation(&self, y: usize) -> Option<Pos> {
        let mut y = self.logical_start(y);
        loop {
            let len = self.len(y);
            if let Some(x) = (0..len).find(|&x| self.with_cell(y, x, |c| c.is_some_and(|c| c != " " && c != "\t" && !c.is_empty()))) {
                return Some((y, x));
            }
            if !self.wrapped(y) || y + 1 >= self.lines.len() {
                return None;
            }
            y += 1;
        }
    }

    /// window_copy_cursor_end_of_line: to the end of the logical line; in a rectangle
    /// selection to the edge of the screen.
    fn end_of_line(&mut self) {
        let y = self.logical_end(self.cy);
        let rect_sel = self.anchor.is_some() && self.rect;
        let x = if rect_sel { self.width } else { self.limit(y, false) };
        self.cy = y;
        self.cx = x;
        self.lastcx = x;
        self.ensure_visible();
    }

    /// grid_reader_cursor_right with wrapping: `all` lets a rectangle selection go to the
    /// screen edge; with vi keys the cursor stops on the last character.
    fn cursor_right(&self, (y, x): Pos, all: bool) -> Option<Pos> {
        let px = if all { self.width } else { self.limit(y, !self.vi) };
        if x >= px {
            return (y + 1 < self.lines.len()).then_some((y + 1, 0));
        }
        let mut nx = x + 1;
        while nx < px && self.is_cont(y, nx) {
            nx += 1;
        }
        Some((y, nx))
    }

    fn class(&self, (y, x): Pos, seps: &str) -> Class {
        let x = self.snap(y, x);
        self.with_cell(y, x, |c| match c {
            None => Class::Space,
            // any Unicode whitespace separates words
            Some(c) if c.is_empty() || c.chars().any(char::is_whitespace) => Class::Space,
            Some(c) => {
                let mut chars = c.chars();
                match (chars.next(), chars.next()) {
                    (Some(ch), None) if seps.contains(ch) => Class::Sep,
                    _ => Class::Word,
                }
            }
        })
    }

    fn step_right(&self, (y, x): Pos) -> Option<Pos> {
        let max = self.max_x(y);
        let mut nx = x + 1;
        while nx <= max && self.is_cont(y, nx) {
            nx += 1;
        }
        if nx <= max {
            Some((y, nx))
        } else if y + 1 < self.lines.len() {
            Some((y + 1, 0))
        } else {
            None
        }
    }

    fn step_left(&self, (y, x): Pos) -> Option<Pos> {
        if x > 0 {
            Some((y, self.snap(y, x.min(self.max_x(y) + 1) - 1)))
        } else if y > 0 {
            Some((y - 1, self.snap(y - 1, self.max_x(y - 1))))
        } else {
            None
        }
    }

    // ----------------------------------------------------------------- cursor movement

    fn ensure_visible(&mut self) {
        if self.cy < self.top {
            self.top = self.cy;
        } else if self.cy >= self.top + self.height {
            self.top = self.cy + 1 - self.height;
        }
        self.top = self.top.min(self.hsize());
    }

    /// Move the cursor, scrolling minimally (window_copy_update_cursor clamps it to the line).
    fn set_cursor(&mut self, y: usize, x: usize) {
        let y = y.min(self.lines.len() - 1);
        self.update_cursor(y, x);
        self.lastcx = self.cx;
        self.ensure_visible();
    }

    fn set_x(&mut self, x: usize) {
        self.set_cursor(self.cy, x);
    }

    /// window_copy_scroll_to: a line out of view is brought a quarter of the view from
    /// the bottom (or the view goes to the top or bottom when near them).
    fn scroll_to(&mut self, (y, x): Pos) {
        if y < self.top || y >= self.top + self.height {
            let (hsize, sy) = (self.hsize(), self.height);
            let gap = sy / 4;
            self.top = if y < sy {
                0
            } else if y > hsize + sy - gap {
                hsize
            } else {
                y + gap - sy
            };
        }
        // window_copy_scroll_to places the cursor without clamping it to the line
        self.cy = y.min(self.lines.len() - 1);
        self.cx = self.snap(self.cy, x);
        self.lastcx = self.cx;
        self.ensure_visible();
    }

    fn repeat_move(&mut self, n: usize, f: impl Fn(&Self, Pos) -> Option<Pos>) {
        let mut p = (self.cy, self.cx);
        for _ in 0..n {
            match f(self, p) {
                Some(q) => p = q,
                None => break,
            }
        }
        self.set_cursor(p.0, p.1);
    }

    /// window_copy_cursor_up / window_copy_cursor_down, `dy` times.
    fn vertical(&mut self, dy: isize) {
        let last = self.lines.len() - 1;
        for _ in 0..dy.unsigned_abs() {
            let norect = self.anchor.is_none() || !self.rect;
            // a line selection turns round at its first or last line (window_copy_cursor_up
            // and _down)
            let line_sel = self.anchor.filter(|_| self.lineflag.is_some() && self.frozen.is_none());
            if let (Some(_), Some(lr), Some((s, e))) = (line_sel, self.lineflag, self.slots()) {
                if (dy < 0 && lr && self.cy == s.0) || (dy > 0 && !lr && self.cy == e.0) {
                    self.other_end();
                }
            }
            self.sticky_begin(norect);
            if dy < 0 {
                self.cy = self.cy.saturating_sub(1);
            } else {
                self.cy = (self.cy + 1).min(last);
            }
            self.ensure_visible();
            self.sticky_end(norect);
            // a line selection keeps the cursor at the end of the line below its first
            // line and at the start above it (LINE_SEL_LEFT_RIGHT / RIGHT_LEFT)
            if let (Some(_), Some(lr)) = (line_sel, self.lineflag) {
                self.cx = if !lr {
                    0
                } else if self.rect {
                    self.width
                } else {
                    self.limit(self.cy, false)
                };
            }
        }
        self.lastcx = self.cx;
    }

    /// Lines a page (or half a page) moves: two less than the view, as tmux.
    fn page_size(&self, half: bool) -> usize {
        if self.height <= 2 {
            1
        } else if half {
            self.height / 2
        } else {
            self.height - 2
        }
    }

    /// tmux window_copy_pageup1: scroll the view up a page keeping the cursor row.
    fn page_up(&mut self, half: bool) {
        let n = self.page_size(half);
        self.sticky_begin(true);
        let row = self.cy - self.top;
        let row = if self.top < n {
            self.top = 0;
            row.saturating_sub(n)
        } else {
            self.top -= n;
            row
        };
        self.page_end(row);
    }

    /// tmux window_copy_pagedown1 (at the bottom the cursor moves down a page).
    fn page_down(&mut self, half: bool) {
        let n = self.page_size(half);
        self.sticky_begin(true);
        let row = self.cy - self.top;
        let hsize = self.hsize();
        let row = if self.top + n > hsize {
            self.top = hsize;
            (row + n).min(self.height - 1)
        } else {
            self.top += n;
            row
        };
        self.page_end(row);
    }

    fn page_end(&mut self, row: usize) {
        self.cy = (self.top + row).min(self.lines.len() - 1);
        if self.anchor.is_none() || !self.rect {
            let px = self.len(self.cy);
            if (self.cx >= self.tlastsx && self.cx != px) || self.cx > px {
                let top = self.top;
                self.end_of_line();
                self.top = top;
                self.ensure_visible();
            }
        }
    }

    // ----------------------------------------------------------------- tmux grid_reader

    /// grid_in_set: whether the character at a position is in `set` (blank past the end).
    /// A space in `set` matches any Unicode whitespace (tmux 3.8).
    fn in_set(&self, (y, x): Pos, set: &str) -> bool {
        let space = set.contains(' ');
        self.with_cell(y, x, |c| match c {
            None => space,
            // the padding half of a wide character is whitespace when that character is
            Some("") => {
                (space || set.contains('\t')) && (0..x).rev().find_map(|i| self.with_cell(y, i, |c| match c {
                    Some("") => None,
                    None => Some(false),
                    Some(c) => Some(space && c.chars().any(char::is_whitespace)),
                })).unwrap_or(false)
            }
            Some(c) if space && c.chars().any(char::is_whitespace) => true,
            Some(c) => {
                let mut chars = c.chars();
                matches!((chars.next(), chars.next()), (Some(ch), None) if set.contains(ch))
            }
        })
    }

    /// grid_reader_cursor_previous_word.
    fn reader_previous_word(&self, (mut y, mut x): Pos, seps: &str, already: bool, stop_at_eol: bool) -> Pos {
        let word_is_letters;
        if already || self.in_set((y, x), WHITESPACE) {
            loop {
                if x > 0 {
                    x -= 1;
                    if !self.in_set((y, x), WHITESPACE) {
                        word_is_letters = !self.in_set((y, x), seps);
                        break;
                    }
                } else {
                    if y == 0 {
                        return (y, x);
                    }
                    y -= 1;
                    x = self.len(y);
                    if stop_at_eol && x > 0 && self.in_set((y, x - 1), WHITESPACE) {
                        word_is_letters = false;
                        break;
                    }
                }
            }
        } else {
            word_is_letters = !self.in_set((y, x), seps);
        }
        loop {
            let old = (y, x);
            if x == 0 {
                if y == 0 || !self.wrapped(y - 1) {
                    return old;
                }
                y -= 1;
                x = self.width;
            }
            x = x.saturating_sub(1);
            if self.in_set((y, x), WHITESPACE) || word_is_letters == self.in_set((y, x), seps) {
                return old;
            }
        }
    }

    /// grid_reader_cursor_next_word_end (emacs): the position just after the word.
    fn reader_next_word_end(&self, (mut y, mut x): Pos, seps: &str) -> Pos {
        let last = self.lines.len() - 1;
        let line_end = |y: usize| if self.wrapped(y) { self.width - 1 } else { self.len(y) };
        let mut xx = line_end(y);
        let wrap = |x: &mut usize, y: &mut usize, xx: &mut usize| -> bool {
            while *x > *xx {
                if *y == last {
                    return false;
                }
                *x = 0;
                *y += 1;
                *xx = line_end(*y);
            }
            true
        };
        while wrap(&mut x, &mut y, &mut xx) {
            if self.in_set((y, x), WHITESPACE) {
                x += 1;
            } else if self.in_set((y, x), seps) {
                loop {
                    x += 1;
                    if !(wrap(&mut x, &mut y, &mut xx) && self.in_set((y, x), seps) && !self.in_set((y, x), WHITESPACE)) {
                        break;
                    }
                }
                return (y, x);
            } else {
                loop {
                    x += 1;
                    if !(wrap(&mut x, &mut y, &mut xx) && !(self.in_set((y, x), WHITESPACE) || self.in_set((y, x), seps))) {
                        break;
                    }
                }
                return (y, x);
            }
        }
        (y, x)
    }

    // ----------------------------------------------------------------- word motions

    /// grid_reader_handle_wrap: past the end of a line, on to the start of the next; false
    /// past the bottom.
    fn reader_wrap(&self, y: &mut usize, x: &mut usize, xx: &mut usize) -> bool {
        while *x > *xx {
            if *y + 1 >= self.lines.len() {
                return false;
            }
            *x = 0;
            *y += 1;
            *xx = self.reader_line_end(*y);
        }
        true
    }

    /// The last position words look at on a line (wrapped words are not broken up).
    fn reader_line_end(&self, y: usize) -> usize {
        if self.wrapped(y) { self.width - 1 } else { self.len(y) }
    }

    /// grid_reader_cursor_next_word: over the word (or run of separators), then over the
    /// whitespace after it.
    fn next_word(&self, (mut y, mut x): Pos, seps: &str) -> Pos {
        let mut xx = self.reader_line_end(y);
        if !self.reader_wrap(&mut y, &mut x, &mut xx) {
            return (y, x);
        }
        if !self.in_set((y, x), WHITESPACE) {
            let on_sep = self.in_set((y, x), seps);
            loop {
                x += 1;
                if !self.reader_wrap(&mut y, &mut x, &mut xx) {
                    break;
                }
                let (ws, sep) = (self.in_set((y, x), WHITESPACE), self.in_set((y, x), seps));
                if ws || sep != on_sep {
                    break;
                }
            }
        }
        while self.reader_wrap(&mut y, &mut x, &mut xx) && self.in_set((y, x), WHITESPACE) {
            x += 1;
            while self.is_cont(y, x) {
                x += 1;
            }
        }
        (y, x)
    }

    /// window_copy_cursor_next_word_end: emacs stops after the word; vi steps on unless on
    /// whitespace (not past the line's last character), then back onto the word's end.
    fn next_word_end(&self, mut p: Pos, seps: &str) -> Pos {
        if !self.vi {
            return self.reader_next_word_end(p, seps);
        }
        if !self.in_set(p, WHITESPACE) {
            // grid_reader_cursor_right without wrapping
            let px = self.limit(p.0, false);
            if p.1 < px {
                p.1 += 1;
                while p.1 < px && self.is_cont(p.0, p.1) {
                    p.1 += 1;
                }
            }
        }
        self.reader_left(self.reader_next_word_end(p, seps), true)
    }

    /// vi `b`: back to the start of the previous word.
    fn previous_word(&self, mut p: Pos, seps: &str) -> Pos {
        loop {
            match self.step_left(p) {
                Some(q) => {
                    p = q;
                    if self.class(p, seps) != Class::Space {
                        break;
                    }
                }
                None => return p,
            }
        }
        let c = self.class(p, seps);
        while let Some(q) = self.step_left(p) {
            if self.class(q, seps) != c {
                break;
            }
            p = q;
        }
        p
    }

    /// First and last position of the run of same-class cells containing `p`.
    fn word_extent(&self, p: Pos, seps: &str) -> (Pos, Pos) {
        let c = self.class(p, seps);
        let mut start = (p.0, self.snap(p.0, p.1));
        while let Some(q) = self.step_left(start) {
            if self.class(q, seps) != c {
                break;
            }
            start = q;
        }
        let mut end = start;
        while let Some(q) = self.step_right(end) {
            if self.class(q, seps) != c {
                break;
            }
            end = q;
        }
        (start, end)
    }

    fn word_under_cursor(&self) -> String {
        let seps = self.separators.clone();
        if self.class((self.cy, self.cx), &seps) != Class::Word {
            return String::new();
        }
        let (s, e) = self.word_extent((self.cy, self.cx), &seps);
        let e = self.step_right(e).unwrap_or((e.0, e.1 + 1));
        let mut out = String::new();
        let mut p = s;
        while p < e {
            self.with_cell(p.0, p.1, |c| out.push_str(c.unwrap_or("")));
            match self.step_right(p) {
                Some(q) => p = q,
                None => break,
            }
        }
        out
    }

    fn previous_paragraph(&mut self) {
        let mut y = self.cy;
        while y > 0 && self.len(y) == 0 {
            y -= 1;
        }
        while y > 0 && self.len(y) > 0 {
            y -= 1;
        }
        self.scroll_to((y, 0));
    }

    fn next_paragraph(&mut self) {
        let max = self.lines.len() - 1;
        let mut y = self.cy;
        while y < max && self.len(y) == 0 {
            y += 1;
        }
        while y < max && self.len(y) > 0 {
            y += 1;
        }
        let x = self.len(y);
        self.scroll_to((y, x));
    }

    // ----------------------------------------------------------------- jump / search

    /// A single-byte character at a cell (brackets are ASCII).
    fn ascii_at(&self, y: usize, x: usize) -> Option<char> {
        self.with_cell(y, x, |c| {
            let mut chars = c?.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) if ch.is_ascii() => Some(ch),
                _ => None,
            }
        })
    }

    /// window_copy_cmd_next_matching_bracket, once: from the bracket under the cursor (emacs
    /// also tries the next cell, then moves like next-word-end; vi looks on along the line
    /// and goes back from a closing bracket).
    fn next_matching_bracket(&mut self) -> bool {
        const OPEN: &str = "{[(";
        const CLOSE: &str = "}])";
        let (mut px, mut py) = (self.cx, self.cy);
        let mut xx = self.len(py);
        let last = self.lines.len() - 1;
        if xx == 0 {
            return false;
        }
        let mut tried = false;
        let found = loop {
            let ch = self.ascii_at(py, px);
            if ch.is_some_and(|c| CLOSE.contains(c) && self.vi) {
                // vi: a closing bracket goes back to its opening one
                let saved = (self.cy, self.cx);
                self.scroll_to((py, px));
                self.previous_matching_bracket();
                if self.ascii_at(self.cy, self.cx).is_some_and(|c| CLOSE.contains(c)) {
                    self.scroll_to(saved);
                }
                return false;
            }
            match ch.filter(|c| OPEN.contains(*c)) {
                Some(ch) => break ch,
                None if !self.vi => {
                    if !tried && px <= xx {
                        px += 1;
                        tried = true;
                        continue;
                    }
                    let p = self.next_word_end((self.cy, self.cx), OPEN);
                    self.set_cursor(p.0, p.1);
                    return true;
                }
                None => {
                    // vi looks on to the end of the (soft-wrapped) line
                    if px > xx {
                        if py == last || !self.wrapped(py) {
                            return true;
                        }
                        px = 0;
                        py += 1;
                        xx = self.len(py);
                    } else {
                        px += 1;
                    }
                }
            }
        };
        let end = CLOSE.as_bytes()[OPEN.find(found).unwrap()] as char;
        let mut n = 1;
        while n != 0 {
            if px > xx {
                if py == last {
                    return true;
                }
                px = 0;
                py += 1;
                xx = self.len(py);
            } else {
                px += 1;
            }
            match self.ascii_at(py, px) {
                Some(c) if c == found => n += 1,
                Some(c) if c == end => n -= 1,
                _ => {}
            }
        }
        self.scroll_to((py, px));
        true
    }

    /// window_copy_cmd_previous_matching_bracket, once.
    fn previous_matching_bracket(&mut self) {
        const OPEN: &str = "{[(";
        const CLOSE: &str = "}])";
        let (mut px, mut py) = (self.cx, self.cy);
        if self.len(py) == 0 {
            return;
        }
        let mut tried = false;
        let found = loop {
            match self.ascii_at(py, px).filter(|c| CLOSE.contains(*c)) {
                Some(ch) => break ch,
                None if !self.vi => {
                    if !tried && px > 0 {
                        px -= 1;
                        tried = true;
                        continue;
                    }
                    let p = self.reader_previous_word((self.cy, self.cx), CLOSE, true, true);
                    self.set_cursor(p.0, p.1);
                    return;
                }
                None => return,
            }
        };
        let start = OPEN.as_bytes()[CLOSE.find(found).unwrap()] as char;
        let mut n = 1;
        while n != 0 {
            if px == 0 {
                if py == 0 {
                    return;
                }
                let mut xx;
                loop {
                    py -= 1;
                    xx = self.len(py);
                    if xx != 0 || py == 0 {
                        break;
                    }
                }
                if xx == 0 {
                    return;
                }
                px = xx - 1;
            } else {
                px -= 1;
            }
            match self.ascii_at(py, px) {
                Some(c) if c == found => n += 1,
                Some(c) if c == start => n -= 1,
                _ => {}
            }
        }
        self.scroll_to((py, px));
    }

    /// grid_reader_cursor_left: off a wide character, then one cell left, up to the end
    /// of the line above when it wraps into this one (or always with `wrap`).
    fn reader_left(&self, (y, mut x): Pos, wrap: bool) -> Pos {
        while x > 0 && self.is_cont(y, x) {
            x -= 1;
        }
        if x == 0 && y > 0 && (wrap || self.wrapped(y - 1)) {
            (y - 1, self.len(y - 1))
        } else {
            (y, x.saturating_sub(1))
        }
    }

    /// grid_reader_cursor_jump: the character at or after a position, through wrapped lines.
    fn reader_jump(&self, (mut y, mut x): Pos, ch: &str) -> Option<Pos> {
        loop {
            if let Some(x) = (x..self.len(y)).find(|&x| self.with_cell(y, x, |c| c == Some(ch))) {
                return Some((y, x));
            }
            if y + 1 >= self.lines.len() || !self.wrapped(y) {
                return None;
            }
            y += 1;
            x = 0;
        }
    }

    /// grid_reader_cursor_jump_back: the character at or before a position, through
    /// wrapped lines.
    fn reader_jump_back(&self, (mut y, x): Pos, ch: &str) -> Option<Pos> {
        let mut xx = x + 1;
        loop {
            if let Some(x) = (0..xx).rev().find(|&x| self.with_cell(y, x, |c| c == Some(ch))) {
                return Some((y, x));
            }
            if y == 0 || !self.wrapped(y - 1) {
                return None;
            }
            y -= 1;
            xx = self.len(y);
        }
    }

    /// window_copy_cursor_jump, _jump_back, _jump_to and _jump_to_back.
    fn do_jump(&mut self, kind: JumpKind, ch: &str, n: usize) {
        for _ in 0..n {
            let at = (self.cy, self.cx);
            let found = match kind {
                JumpKind::Forward => self.reader_jump((at.0, at.1 + 1), ch),
                JumpKind::ToForward => self.reader_jump((at.0, at.1 + 2), ch).map(|p| self.reader_left(p, true)),
                JumpKind::Backward => self.reader_jump_back(self.reader_left(at, false), ch),
                JumpKind::ToBackward => {
                    let from = self.reader_left(self.reader_left(at, false), false);
                    self.reader_jump_back(from, ch).map(|(y, x)| {
                        // grid_reader_cursor_right with wrapping
                        let px = if self.vi { self.limit(y, false) } else { self.len(y) };
                        if x >= px && y + 1 < self.lines.len() {
                            (y + 1, 0)
                        } else if x < px {
                            let mut nx = x + 1;
                            while nx < px && self.is_cont(y, nx) {
                                nx += 1;
                            }
                            (y, nx)
                        } else {
                            (y, x)
                        }
                    })
                }
            };
            match found {
                Some((y, x)) => self.set_cursor(y, x),
                None => break,
            }
        }
    }

    /// Matches beginning on this physical row; end columns may extend onto wrapped rows.
    fn row_matches(&self, y: usize, needle: &str) -> Vec<(usize, usize)> {
        if y >= self.lines.len() { return Vec::new(); }
        // sized for one row, the usual case
        let mut s = String::with_capacity(self.width);
        let mut starts = Vec::with_capacity(self.width); // (byte offset, cell offset)
        let mut row = y;
        let regex = self.search_regex && needle.contains(['^', '$', '*', '+', '(', ')', '?', '[', ']', '.', '\\']);
        let limit = if regex { 2000 } else { self.width + needle.len() };
        loop {
            // a tab matches a tab (window_copy_search_compare)
            let mut x = 0;
            self.lines.texts(row, self.width, |text| {
                if !text.is_empty() {
                    starts.push((s.len(), (row - y) * self.width + x));
                    s.push_str(text);
                }
                x += 1;
            });
            // window-copy.c limits joined regex subjects to 2000 cells.
            if !self.wrapped(row) || row + 1 == self.lines.len() || (row - y + 1) * self.width >= limit {
                break;
            }
            row += 1;
        }
        let len = (row - y + 1) * self.width;
        let col_at = |b: usize| starts.iter().rev().find(|(o, _)| *o <= b).map_or(0, |(_, x)| *x);
        let col_after = |e: usize| starts.iter().find(|(o, _)| *o >= e).map_or(len, |(_, x)| *x);
        let mut out = Vec::new();
        let mut cache = self.search_cache.borrow_mut();
        let literal = self.search_regex && !regex;
        if cache.0 != needle || cache.1 != literal {
            let pattern = if literal { literal_search_pattern(needle) } else { needle.into() };
            *cache = (needle.to_string(), literal, crate::format::compile_regex(&pattern, !needle.bytes().any(|c| c.is_ascii_uppercase())));
        }
        let Some(re) = &cache.2 else { return out };
        let mut from = 0;
        while from < s.len() {
            let Ok(Some(found)) = re.find_from_pos(&s, from) else { break };
            // tmux discards empty regex matches, including anchors alone.
            if found.start() == found.end() { break; }
            let x = col_at(found.start());
            if x >= self.width { break; }
            out.push((x, col_after(found.end())));
            from = found.end();
        }
        out
    }

    /// Matches starting on rows `first..=y`; where row searches overlap, the marks keep the
    /// first match's cells.
    fn match_spans(&self, first: usize, y: usize, needle: &str) -> Vec<(usize, usize)> {
        self.all_match_spans(first, y, needle).into_iter().filter(|&(a, b)| a < (y + 1) * self.width && b > y * self.width).collect()
    }

    /// Every match starting on rows `first..=last`, as absolute cell offsets.
    fn all_match_spans(&self, first: usize, last: usize, needle: &str) -> Vec<(usize, usize)> {
        let mut marks: Vec<(usize, usize)> = Vec::new();
        for row in first..=last {
            for (x0, x1) in self.row_matches(row, needle) {
                let start = row * self.width + x0;
                let end = row * self.width + x1;
                let start = marks.last().map_or(start, |&(_, prev)| start.max(prev));
                if start < end { marks.push((start, end)); }
            }
        }
        marks
    }

    fn visible_matches(&self, y: usize, needle: &str) -> Vec<(usize, usize)> {
        let start = self.top * self.width;
        let end = (self.top + self.height) * self.width;
        self.match_spans(self.top, y, needle).into_iter().filter(|&(a, _)| a >= start && a < end)
            .map(|(a, b)| (a, b.min(end))).collect()
    }

    /// The all-history budget is 200 ms; the visible retry shares the 10 s deadline.
    /// Keep the clock injectable so boundary and partial-count tests are deterministic.
    fn count_search_matches(&mut self, needle: &str, visible_only: bool, mut elapsed_ms: impl FnMut() -> u128) {
        let mut start = 0;
        let mut end = self.lines.len();
        let mut visible = visible_only;
        let mut stopped = false;
        let mut count = 0;
        loop {
            if visible {
                start = self.top;
                while start > 0 && self.wrapped(start - 1) { start -= 1; }
                end = (self.top + self.height).min(self.lines.len());
            }
            for y in start..end {
                count += self.row_matches(y, needle).len();
                let elapsed = elapsed_ms();
                if elapsed > 10_000 {
                    self.search_timed_out = true;
                    self.clear_marks();
                    return;
                }
                if !visible && elapsed > 200 {
                    stopped = true;
                    break;
                }
            }
            if stopped && !visible { visible = true; continue; }
            break;
        }
        if !visible_only {
            self.search_count_partial = stopped;
            self.found_count = if stopped {
                [1000, 100, 10].into_iter().find(|&n| count > n)
            } else { Some(count) };
        }
    }

    /// Where tmux's cursor is for searching: the first cell after an emacs search wrapped
    /// it there (search_wrapped) while the cursor is still on the last cell, otherwise
    /// the cursor.
    fn search_cursor(&self) -> Pos {
        let last = (self.lines.len() - 1, self.width - 1);
        if self.search_wrapped && (self.cy, self.cx) == last { (0, 0) } else { (self.cy, self.cx) }
    }

    /// window_copy_search_jump: the next match (row and columns). Forward, vi starts after
    /// the match under the cursor (or the next cell), emacs at the cursor; backward starts
    /// left of the cursor. Wraps around with wrap-search.
    fn find_match(&self, needle: &str, forward: bool) -> Option<(usize, (usize, usize))> {
        let total = self.lines.len();
        let (cy, cx) = self.search_cursor();
        let at = cy * self.width + cx;
        let from = if forward && self.vi {
            self.search_present().then(|| self.visible_matches(cy, needle).into_iter()
                .find(|&(a, b)| at >= a && at < b).map(|(_, b)| b)).flatten().unwrap_or(at + 1)
        } else { at };
        // the matches of a whole wrapped line are found once (window_copy_search_jump looks
        // at each row once; per-row searches of long wrapped lines would be quadratic)
        let mut line: Option<(usize, usize, Vec<(usize, usize)>)> = None;
        for i in 0..=total {
            if !self.wrap_search && ((forward && cy + i >= total) || (!forward && i > cy)) { break; }
            let y = if forward { (cy + i) % total } else { (cy + total * 2 - i) % total };
            if !line.as_ref().is_some_and(|(a, b, _)| (*a..=*b).contains(&y)) {
                let (first, last) = (self.logical_start(y), self.logical_end(y));
                line = Some((first, last, self.all_match_spans(first, last, needle)));
            }
            let spans = &line.as_ref().unwrap().2;
            let matches: Vec<(usize, usize)> = spans.iter().copied().filter(|&(a, b)| a < (y + 1) * self.width && b > y * self.width).collect();
            let eligible = |&(a, _): &(usize, usize)| {
                if forward {
                    if i == 0 { a >= from } else if i == total { a < from } else { a >= y * self.width }
                } else if i == 0 { a < at } else if i == total { a >= at } else { true }
            };
            let found = if forward { matches.into_iter().find(eligible) } else { matches.into_iter().rev().find(eligible) };
            if let Some((a, b)) = found {
                let row = a / self.width;
                return Some((row, (a % self.width, b - row * self.width)));
            }
        }
        None
    }

    /// window_copy_search, `n` times: a search for a new text clears the marks and counts
    /// the matches; the pane's last search again only marks them. Emacs forward searches
    /// leave the cursor just after the match.
    fn do_search(&mut self, needle: &str, forward: bool, n: usize) -> bool {
        let mut found = false;
        for _ in 0..n {
            if self.search_timed_out { break; }
            let regex = self.search_regex && needle.contains(['^', '$', '*', '+', '(', ')', '?', '[', ']', '.', '\\']);
            let visible_only = !self.search_all && self.pane_search.as_deref() == Some(needle) && self.pane_search_regex == regex;
            self.search_all = false;
            if !visible_only && self.search_marks {
                self.clear_marks();
            }
            self.pane_search = Some(needle.into());
            self.pane_search_regex = regex;
            self.searched_down = forward;
            let Some((y, (x0, x1))) = self.find_match(needle, forward) else { break };
            found = true;
            self.search_wrapped = false;
            self.scroll_to((y, x0));
            self.search_marks = true;
            let started = std::time::Instant::now();
            self.count_search_matches(needle, visible_only, || started.elapsed().as_millis());
            if forward && !self.vi && self.search_marks {
                // the selection was updated at the match; the cursor moves on without it
                let slots = self.slots().filter(|_| self.frozen.is_none());
                let end = (y * self.width + x1).min((self.top + self.height) * self.width);
                // a match ending in the last cell (the view is then at the bottom) wraps
                // tmux's cursor to the first cell with wrap-search, and tmux leaves the
                // view as it is. With history above the view its cursor is then above
                // the view (copy_cursor_y is -hsize, drawn clamped to the bottom row;
                // tmux 3.4 spins on the next cursor command): the cursor stays on the
                // last cell instead, in view next to the match, and the next search
                // starts from the first cell as tmux's does.
                let last = (self.lines.len() - 1, self.width - 1);
                let wrapped = end >= self.lines.len() * self.width;
                let (y, x) = if !wrapped { (end / self.width, end % self.width) }
                    else if self.wrap_search && self.top == 0 { (0, 0) } else { last };
                self.search_wrapped = wrapped && self.wrap_search && self.top > 0;
                self.cy = y;
                self.cx = x;
                self.lastcx = x;
                self.pinned = slots.map(|(s, e)| ((y, x), s, e));
            }
        }
        found
    }

    // ----------------------------------------------------------------- selection

    /// The end of the selection that is not the anchor: the cursor, unless stopped.
    fn sel_other(&self) -> Pos {
        self.frozen.unwrap_or((self.cy, self.cx))
    }

    /// Ordered (start, end) of the current selection.
    fn sel_bounds(&self) -> Option<(Pos, Pos)> {
        let (s, e) = self.slots()?;
        Some(if s <= e { (s, e) } else { (e, s) })
    }

    /// Exclusive end column for an inclusive (vi) end at `x`, covering a wide char.
    fn incl_end(&self, y: usize, x: usize) -> usize {
        let mut e = x + 1;
        while self.is_cont(y, e) {
            e += 1;
        }
        e
    }

    /// window_copy_set_selection: the selection's ends as screen cells (the view's rows,
    /// columns after the line numbers), ends out of view moved to its edge; None when
    /// nothing is selected or the whole selection is above or below the view.
    fn screen_selection(&self) -> Option<ScreenSel> {
        let (s, e) = self.slots()?;
        let adjust = |(y, x): Pos| -> (usize, usize, i8) {
            if y < self.top {
                (if self.rect { x } else { 0 }, 0, -1)
            } else if y >= self.top + self.height {
                (if self.rect { x } else { self.width - 1 }, self.height - 1, 1)
            } else {
                (x, y - self.top, 0)
            }
        };
        let (sx, sy, sp) = adjust(s);
        let (ex, ey, ep) = adjust(e);
        if sp == ep && sp != 0 {
            return None;
        }
        let lnw = self.line_number_width();
        let (sx, ex) = if lnw > 0 { (self.cursor_offset(sx), self.cursor_offset(ex)) } else { (sx, ex) };
        Some(ScreenSel { sx, sy, ex, ey, rect: self.rect, clipx: lnw.min(self.width - 1), vi: self.vi })
    }

    /// window_copy_get_selection: the text between the selection's ends (a rectangle
    /// between the cursor's column and the other end's), vi keeping the last character.
    fn selection_text(&self) -> String {
        let Some((a, b)) = self.slots() else { return String::new() };
        let ((sy, sx), (ey, ex)) = if b < a { (b, a) } else { (a, b) };
        let ey_last = self.len(ey);
        let ex = ex.min(ey_last);
        let one = usize::from(self.vi);
        let (firstsx, restsx, lastex, restex) = if self.rect {
            // the column of the end the cursor does not drag
            let selx = if !self.ends_swapped && self.frozen.is_none() { a.1 } else { b.1 };
            if selx < self.cx {
                (selx, selx, self.cx + one, self.cx + one)
            } else {
                (self.cx, self.cx, selx + 1, selx + 1)
            }
        } else {
            (sx, 0, ex + one, self.width)
        };
        let mut out = String::new();
        for y in sy..=ey {
            self.copy_line(&mut out, y, if y == sy { firstsx } else { restsx }, if y == ey { lastex } else { restex });
        }
        // the final newline goes (unless vi copied to the end of the line)
        if (!self.vi || lastex <= ey_last) && (!self.wrapped(ey) || lastex != ey_last) && out.ends_with('\n') {
            out.pop();
        }
        out
    }

    /// window_copy_copy_line: cells [sx, ex) of a line and a newline unless it wraps.
    fn copy_line(&self, out: &mut String, y: usize, sx: usize, ex: usize) {
        if sx > ex {
            return;
        }
        let wrapped = self.wrapped(y);
        let xx = self.copy_len(y);
        let (sx, ex) = (sx.min(xx), ex.min(xx));
        // padding is skipped and a tab copied as a tab
        self.lines.push_cell_texts(out, y, sx..ex);
        if !wrapped || ex != xx {
            out.push('\n');
        }
    }

    fn copy_selection(&mut self, clear: bool, cancel: bool, append: bool) -> Action {
        // without a selection, the search match at the cursor
        let text = if self.anchor.is_none() { self.search_match().unwrap_or_default() } else { self.selection_text() };
        if clear {
            self.clear_selection();
        }
        if text.is_empty() {
            return if cancel { Action::Cancel } else { Action::None };
        }
        Action::Copy { text, cancel, append }
    }

    /// window_copy_do_copy_line / _do_copy_end_of_line: a selection from the start of the
    /// line (copy-line, which also goes back to a character selection) or the cursor to the
    /// end of the `n`th line is copied, then cleared, and the cursor goes back. The selection
    /// mode, line and rectangle flags of an earlier selection stay in effect while it is made.
    fn copy_lines(&mut self, n: usize, from_cursor: bool, cancel: bool) -> Action {
        let (ocy, ocx, otop) = (self.cy, self.cx, self.top);
        if !from_cursor {
            self.sel_mode = SelMode::Char;
            let y = self.logical_start(self.cy);
            self.set_cursor(y, 0);
        }
        // window_copy_start_selection
        self.anchor = Some((self.cy, self.cx));
        self.frozen = None;
        self.ends_swapped = false;
        self.pinned = None;
        for _ in 1..n {
            self.vertical(1);
        }
        self.end_of_line();
        let text = self.selection_text();
        if cancel {
            return if text.is_empty() { Action::Cancel } else { Action::Copy { text, cancel, append: false } };
        }
        self.clear_selection();
        (self.cy, self.cx, self.top) = (ocy, ocx, otop);
        if text.is_empty() {
            return Action::None;
        }
        Action::Copy { text, cancel, append: false }
    }
}

/// Cells a row uses (grid_line cellused): all of a wrapped row, up to the last written
/// cell of another.
fn cells_used(l: &Line) -> usize {
    if l.wrapped {
        l.cells.len()
    } else {
        l.cells.iter().rposition(|c| *c != Cell::default()).map_or(0, |i| i + 1)
    }
}

/// grid_wrap_position: a position as (offset in its unwrapped line, None past its end;
/// index of that unwrapped line).
fn wrap_position(lines: &[Line], px: usize, py: usize) -> (Option<usize>, usize) {
    let (mut ax, mut ay) = (0, 0);
    for l in &lines[..py.min(lines.len())] {
        if l.wrapped {
            ax += cells_used(l);
        } else {
            ax = 0;
            ay += 1;
        }
    }
    let used = lines.get(py).map_or(0, cells_used);
    (if px >= used { None } else { Some(ax + px) }, ay)
}

/// grid_unwrap_position.
fn unwrap_position(lines: &[Line], wx: Option<usize>, wy: usize) -> (usize, usize) {
    let last = lines.len().saturating_sub(1);
    let (mut yy, mut ay) = (0, 0);
    while yy < last && ay != wy {
        if !lines[yy].wrapped {
            ay += 1;
        }
        yy += 1;
    }
    match wx {
        None => {
            while yy < last && lines[yy].wrapped {
                yy += 1;
            }
            (lines.get(yy).map_or(0, cells_used), yy)
        }
        Some(mut wx) => {
            while yy < last && lines[yy].wrapped && wx >= cells_used(&lines[yy]) {
                wx -= cells_used(&lines[yy]);
                yy += 1;
            }
            (wx, yy)
        }
    }
}

/// A cell's width as grid_reflow reads it: a wide character's own width, 1 for anything
/// else, including the padding cell after a wide character (it is stored as a compact
/// cell, which grid_get_cell1 reads back as width 1).
fn reflow_width(c: &Cell) -> usize {
    if c.is_continuation() { 1 } else { unicode_width::UnicodeWidthStr::width(c.text.as_str()).max(1) }
}

/// A row as grid_reflow works on it: its cells with their widths, whether it wraps and
/// its gl->cellsize.
type ReflowRow = (Vec<(Cell, usize)>, bool, u16);

/// gl->cellsize of a reflowed row of `sx` columns once cells `from..to` are written to it
/// one at a time (grid_set_cell).
fn reflow_grow(cellsize: u16, from: usize, to: usize, sx: usize) -> u16 {
    let n = |v: usize| u16::try_from(v).unwrap_or(u16::MAX);
    vt100::grown_cellsize(cellsize, n(from), n(to), n(sx))
}

/// grid_reflow, with grid_reflow_move, _split and _join as tmux 3.8 has them: rows too
/// wide are split, rows that wrap take what fits from the rows after them. Each row's
/// gl->cellsize goes as tmux's does: a row moved across keeps its own, the first part
/// of a split row has exactly its cells, the other parts and the cells joined onto a
/// row are written a cell at a time, and the rest of a row a join took part of has
/// exactly what is left.
fn reflow(lines: &[Line], sx: usize) -> Vec<Line> {
    // each cell with its width: a tab is a cell of its width and its padding cells one
    // column each, as grid_reflow counts them
    let mut src: Vec<ReflowRow> = lines.iter().map(|l| {
        let cells = l.cells[..cells_used(l).min(l.cells.len())].iter().enumerate();
        (cells.map(|(x, c)| (c.clone(), l.metadata.get(&(x as u16)).filter(|m| m.tab_width > 0).map_or_else(|| reflow_width(c), |m| usize::from(m.tab_width)))).collect(), l.wrapped, l.cellsize)
    }).collect();
    let mut dead = vec![false; src.len()];
    let mut target: Vec<ReflowRow> = Vec::with_capacity(src.len());
    for yy in 0..src.len() {
        if dead[yy] {
            continue;
        }
        let (mut width, mut at) = (0, 0);
        for (i, (_, w)) in src[yy].0.iter().enumerate() {
            if at == 0 && width + w > sx {
                at = i;
            }
            width += w;
        }
        if width > sx {
            // grid_reflow_split
            let (cells, wrapped, _) = std::mem::take(&mut src[yy]);
            dead[yy] = true;
            target.push((cells[..at].to_vec(), true, u16::try_from(at).unwrap_or(u16::MAX)));
            let (mut line, mut width): (ReflowRow, usize) = ((Vec::new(), false, 0), 0);
            for c in &cells[at..] {
                if width + c.1 > sx {
                    line.1 = true;
                    line.2 = reflow_grow(0, 0, line.0.len(), sx);
                    target.push(std::mem::take(&mut line));
                    width = 0;
                }
                width += c.1;
                line.0.push(c.clone());
            }
            line.1 |= wrapped;
            line.2 = reflow_grow(0, 0, line.0.len(), sx);
            target.push(line);
            if width < sx && wrapped {
                reflow_join(&mut target, &mut src, &mut dead, sx, yy, width);
            }
        } else {
            target.push(std::mem::take(&mut src[yy]));
            dead[yy] = true;
            if width < sx && target.last().unwrap().1 {
                reflow_join(&mut target, &mut src, &mut dead, sx, yy, width);
            }
        }
    }
    let mut output: Vec<Line> = target.into_iter().map(|(cells, wrapped, cellsize)| {
        let mut cells: Vec<Cell> = cells.into_iter().map(|(c, _)| c).collect();
        if !wrapped {
            cells.resize(sx.max(cells.len()), Cell::default());
        }
        Line { cells, wrapped, cellsize, ..Default::default() }
    }).collect();
    // Line flags and times stay on the row a line starts (grid_reflow_move/_split
    // copy the line, rows joined onto another lose theirs); cell metadata follows its
    // cell, and a tab's padding moved past the tab's row is padding there (shows
    // nothing, as tmux's). One pass, rows moving on, except that a tab split by an
    // earlier reflow keeps its metadata on the row of its own cell while its padding
    // is on the rows after: placing that padding goes past where the next row starts,
    // so a position before the current output row is looked for again from the
    // start of its line.
    let (mut cy, mut base, mut offset, mut start) = (0, 0, 0, 0);
    let place = |output: &[Line], cy: &mut usize, base: &mut usize, start: usize, wx: usize| -> (usize, usize) {
        if wx < *base {
            (*cy, *base) = (start, 0);
        }
        while *cy + 1 < output.len() && output[*cy].wrapped && wx >= *base + cells_used(&output[*cy]) {
            *base += cells_used(&output[*cy]);
            *cy += 1;
        }
        (wx - *base, *cy)
    };
    for line in lines {
        if cy >= output.len() {
            break;
        }
        let (x, y) = place(&output, &mut cy, &mut base, start, offset);
        if x == 0 {
            output[y].flags = line.flags;
            output[y].time = line.time;
        }
        for (&col, m) in &line.metadata {
            let (x, y) = place(&output, &mut cy, &mut base, start, offset + usize::from(col));
            let m = m.clone();
            for pad in 1..usize::from(m.tab_width) {
                let (px, py) = place(&output, &mut cy, &mut base, start, offset + usize::from(col) + pad);
                if py != y {
                    if let Some(c) = output[py].cells.get_mut(px) {
                        c.text.clear();
                    }
                }
            }
            if x < output[y].cells.len() {
                // grid_set_cell marks the line the cell lands on
                output[y].flags |= vt100::ROW_EXTENDED | if m.hyperlink.is_some() { vt100::ROW_HYPERLINK } else { 0 };
                if m != vt100::CellMetadata::default() {
                    output[y].metadata.insert(x as u16, m);
                }
            }
        }
        if line.wrapped {
            offset += cells_used(line);
        } else {
            while cy + 1 < output.len() && output[cy].wrapped {
                cy += 1;
            }
            (cy, base, offset) = (cy + 1, 0, 0);
            start = cy;
        }
    }
    output
}

/// grid_reflow_join: the rows after `yy` are joined onto the last target row while they
/// fit; a row taken only in part keeps the rest for later.
fn reflow_join(target: &mut [ReflowRow], src: &mut [ReflowRow], dead: &mut [bool], sx: usize, yy: usize, mut width: usize) {
    let to = target.len() - 1;
    // the joined cells are written after the row's own, a cell at a time
    let at = target[to].0.len();
    let (mut lines, mut wrapped, mut from, mut want) = (0, true, None, 0);
    loop {
        let line = yy + 1 + lines;
        if line >= src.len() {
            break;
        }
        if !src[line].1 {
            wrapped = false;
        }
        if src[line].0.is_empty() {
            if !wrapped {
                break;
            }
            lines += 1;
            continue;
        }
        let w = src[line].0[0].1;
        if width + w > sx {
            break;
        }
        width += w;
        target[to].0.push(src[line].0[0].clone());
        from = Some(line);
        want = 1;
        while want < src[line].0.len() {
            let w = src[line].0[want].1;
            if width + w > sx {
                break;
            }
            width += w;
            target[to].0.push(src[line].0[want].clone());
            want += 1;
        }
        lines += 1;
        if !wrapped || want != src[line].0.len() || width == sx {
            break;
        }
    }
    let Some(from) = from.filter(|_| lines > 0) else { return };
    target[to].2 = reflow_grow(target[to].2, at, target[to].0.len(), sx);
    if want < src[from].0.len() {
        src[from].0.drain(..want);
        src[from].2 = u16::try_from(src[from].0.len()).unwrap_or(u16::MAX);
        lines -= 1;
    } else if !wrapped {
        target[to].1 = false;
    }
    for d in &mut dead[yy + 1..yy + 1 + lines] {
        *d = true;
    }
}

/// A selection as screen_set_selection keeps it: start (selx, sely) and end, in screen cells.
struct ScreenSel {
    sx: usize,
    sy: usize,
    ex: usize,
    ey: usize,
    rect: bool,
    clipx: usize,
    vi: bool,
}

impl ScreenSel {
    /// screen_check_selection: emacs keeps the top-left character and drops the bottom-right
    /// one, vi keeps both; a rectangle includes both columns.
    fn contains(&self, px: usize, py: usize) -> bool {
        let ScreenSel { sx, sy, ex, ey, .. } = *self;
        if px < self.clipx {
            return false;
        }
        if self.rect {
            if py < sy.min(ey) || py > sy.max(ey) {
                return false;
            }
            return px >= sx.min(ex) && px <= sx.max(ex);
        }
        let before_end = |x: usize| if self.vi { x } else { x.saturating_sub(1) };
        if sy < ey {
            if py < sy || py > ey || (py == sy && px < sx) {
                return false;
            }
            !(py == ey && px > before_end(ex))
        } else if sy > ey {
            if py > sy || py < ey || (py == ey && px < ex) {
                return false;
            }
            // tmux's u_int sx - 1 wraps for a start at column 0: that row is not selected
            !(py == sy && (sx == 0 || px > before_end(sx)))
        } else if py != sy {
            false
        } else if ex < sx {
            // tmux computes sx - 1 here even when it wraps (sx is above 0 here)
            px <= before_end(sx) && px >= ex
        } else {
            px >= sx && px <= before_end(ex)
        }
    }
}

/// screen_select_cell: the selection style, with the cell's colours where the style has
/// none and the cell's attributes added unless the style has noattr.
fn select_cell(st: &RenderStyle, cell: Style) -> Style {
    let sel = st.selection;
    Style {
        fg: if sel.fg == Color::Default { cell.fg } else { sel.fg },
        bg: if sel.bg == Color::Default { cell.bg } else { sel.bg },
        us: if sel.us == Color::Default { cell.us } else { sel.us },
        attrs: if st.selection_noattr { sel.attrs } else { sel.attrs | cell.attrs },
    }
}

fn prompt(label: &str, command: &str) -> Action {
    Action::Prompt { label: label.to_string(), command: command.to_string() }
}

fn normalize_key(k: &str) -> &str {
    match k {
        "PageUp" | "PgUp" | "PPage" => "PPage",
        "PageDown" | "PgDn" | "NPage" => "NPage",
        " " => "Space",
        "Esc" => "Escape",
        "Return" | "CR" | "C-m" => "Enter",
        "Backspace" => "BSpace",
        "C-@" => "C-Space",
        other => other,
    }
}

/// The character a key produces when used as a jump argument.
fn key_char(k: &str) -> Option<String> {
    if k == "Space" {
        return Some(" ".into());
    }
    let mut chars = k.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c.to_string()),
        _ => None,
    }
}

fn literal_search_pattern(text: &str) -> String {
    let mut pattern = String::new();
    for c in text.chars() {
        if ".^$*+?()[]{}|\\".contains(c) { pattern.push('\\'); }
        pattern.push(c);
    }
    pattern
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthChar;

    const W: u16 = 20;

    fn line(s: &str, wrapped: bool) -> Line {
        let mut cells = Vec::new();
        for ch in s.chars() {
            cells.push(Cell { text: ch.to_string(), style: Style::default() });
            if ch.width().unwrap_or(1) == 2 {
                cells.push(Cell { text: String::new(), style: Style::default() });
            }
        }
        cells.resize(W as usize, Cell::default());
        Line { cells, wrapped, ..Default::default() }
    }

    fn lines(v: &[&str]) -> Vec<Line> {
        v.iter().map(|s| line(s, false)).collect()
    }

    /// 10 history lines + 5-line screen, cursor at bottom-left.
    fn numbered() -> CopyMode {
        let v: Vec<String> = (0..15).map(|i| format!("line{i}")).collect();
        let v: Vec<&str> = v.iter().map(String::as_str).collect();
        CopyMode::new(lines(&v), W, 5, (4, 0), true)
    }

    impl CopyMode {
        /// A key as the server runs it: commands the key resolves to are run.
        fn press(&mut self, k: &str) -> Action {
            let a = self.key(k);
            self.resolve(a)
        }

        fn resolve(&mut self, a: Action) -> Action {
            match a {
                Action::Run(name, args, format) => {
                    let args: Vec<String> = if format { args.iter().map(|a| a.replace("#{copy_cursor_word}", &self.cursor_word())).collect() } else { args };
                    let a = self.run(&name, &args);
                    self.resolve(a)
                }
                other => other,
            }
        }
    }

    fn keys(cm: &mut CopyMode, ks: &[&str]) -> Action {
        let mut last = Action::None;
        for k in ks {
            last = cm.press(k);
        }
        last
    }

    fn pos(cm: &CopyMode) -> Pos {
        (cm.cy, cm.cx)
    }

    fn copied(a: Action) -> String {
        match a {
            Action::Copy { text, .. } => text,
            other => panic!("expected copy, got {other:?}"),
        }
    }

    fn row_text(l: &Line) -> String {
        l.cells.iter().map(|c| c.text.as_str()).collect::<String>().trim_end().to_string()
    }

    #[test]
    fn initial_view_and_cursor() {
        let cm = numbered();
        assert_eq!(cm.scroll_offset(), 0);
        assert_eq!(pos(&cm), (14, 0));
        let (view, cur) = cm.render();
        assert_eq!(view.len(), 5);
        assert_eq!(row_text(&view[4]), "line14");
        assert_eq!(cur, (4, 0));
    }

    #[test]
    fn short_snapshot_is_padded() {
        let cm = CopyMode::new(lines(&["a"]), W, 4, (0, 0), true);
        assert_eq!(cm.lines.len(), 4);
        assert_eq!(cm.scroll_offset(), 0);
        assert_eq!(cm.render().0.len(), 4);
    }

    #[test]
    fn basic_navigation_and_scrolling() {
        let mut cm = numbered();
        keys(&mut cm, &["k", "k"]);
        assert_eq!(pos(&cm), (12, 0));
        assert_eq!(cm.scroll_offset(), 0);
        keys(&mut cm, &["k", "k", "k"]);
        assert_eq!(cm.cy, 9);
        assert_eq!(cm.scroll_offset(), 1, "cursor above view scrolls by one");
        keys(&mut cm, &["l", "l", "$"]);
        assert_eq!(cm.cx, 4, "vi end-of-line is on the last character (tmux 3.7)");
        keys(&mut cm, &["0"]);
        assert_eq!(cm.cx, 0);
        keys(&mut cm, &["g"]);
        assert_eq!(pos(&cm), (0, 0));
        assert_eq!(cm.scroll_offset(), 10);
        keys(&mut cm, &["G"]);
        assert_eq!(pos(&cm), (14, 5));
        assert_eq!(cm.scroll_offset(), 0);
        keys(&mut cm, &["Up", "Up", "Down"]);
        assert_eq!(cm.cy, 13);
    }

    #[test]
    fn horizontal_wraps_between_lines() {
        let mut cm = CopyMode::new(lines(&["ab", "cd"]), W, 2, (1, 0), true);
        cm.press("h");
        assert_eq!(pos(&cm), (0, 1), "vi: the cursor stays on the text");
        cm.press("l");
        assert_eq!(pos(&cm), (1, 0));
        let mut cm = CopyMode::new(lines(&["ab", "cd"]), W, 2, (1, 0), false);
        cm.press("Left");
        assert_eq!(pos(&cm), (0, 2), "emacs may go one past the text");
    }

    #[test]
    fn vertical_motion_keeps_column() {
        let mut cm = CopyMode::new(lines(&["abcdefgh", "ab", "abcdefgh"]), W, 3, (0, 6), true);
        cm.press("j");
        assert_eq!(pos(&cm), (1, 1));
        // tmux 3.8 remembers the clamped column
        cm.press("j");
        assert_eq!(pos(&cm), (2, 1));
        let mut cm = CopyMode::new(lines(&["abcdefgh", "ab", "abcdefgh"]), W, 3, (0, 6), false);
        cm.press("Down");
        assert_eq!(pos(&cm), (1, 2));
        cm.press("Down");
        assert_eq!(pos(&cm), (2, 6));
    }

    #[test]
    fn top_middle_bottom_and_indentation() {
        let mut cm = numbered();
        cm.press("H");
        assert_eq!(pos(&cm), (10, 0));
        cm.press("M");
        assert_eq!(cm.cy, 12);
        cm.press("L");
        assert_eq!(cm.cy, 14);
        let mut cm = CopyMode::new(lines(&["    indented"]), W, 1, (0, 9), true);
        cm.press("^");
        assert_eq!(cm.cx, 4);
    }

    #[test]
    fn word_motions_vi() {
        let mut cm = CopyMode::new(lines(&["foo bar-baz  qux", "next"]), W, 2, (0, 0), true);
        cm.press("w");
        assert_eq!(cm.cx, 4);
        cm.press("w");
        assert_eq!(cm.cx, 7, "separator run is its own word");
        cm.press("w");
        assert_eq!(cm.cx, 8);
        cm.press("w");
        assert_eq!(cm.cx, 13);
        cm.press("w");
        assert_eq!(pos(&cm), (1, 0), "crosses unwrapped line end");
        cm.press("b");
        assert_eq!(pos(&cm), (0, 13));
        cm.press("b");
        assert_eq!(cm.cx, 8);
        cm.press("b");
        assert_eq!(cm.cx, 7);
        cm.press("b");
        assert_eq!(cm.cx, 4);
        cm.press("0");
        cm.press("e");
        assert_eq!(cm.cx, 2);
        cm.press("e");
        assert_eq!(cm.cx, 6);
        cm.press("0");
        cm.press("W");
        assert_eq!(cm.cx, 4);
        cm.press("W");
        assert_eq!(cm.cx, 13, "space-word skips separators");
        cm.press("B");
        assert_eq!(cm.cx, 4);
        cm.press("E");
        assert_eq!(cm.cx, 10);
    }

    #[test]
    fn word_motion_emacs_end_is_after_word() {
        let mut cm = CopyMode::new(lines(&["foo bar"]), W, 1, (0, 0), false);
        cm.press("M-f");
        assert_eq!(cm.cx, 3);
        cm.press("M-f");
        assert_eq!(cm.cx, 7);
        cm.press("M-b");
        assert_eq!(cm.cx, 4);
    }

    #[test]
    fn custom_word_separators() {
        let mut cm = CopyMode::new(lines(&["a.b c"]), W, 1, (0, 0), true);
        cm.press("w");
        assert_eq!(cm.cx, 4);
        cm.press("0");
        cm.set_word_separators(" .");
        cm.press("w");
        assert_eq!(cm.cx, 1);
    }

    #[test]
    fn words_do_not_break_on_soft_wrap() {
        let mut v = vec![line("aaaaaaaaaaaaaaaaaaab", true), line("cd ef", false)];
        v[0].cells[19].text = "b".into();
        let mut cm = CopyMode::new(v, W, 2, (0, 0), true);
        cm.press("w");
        assert_eq!(pos(&cm), (1, 3), "wrapped word continues on next row");
        cm.press("b");
        cm.press("b");
        assert_eq!(pos(&cm), (0, 0));
        cm.press("e");
        assert_eq!(pos(&cm), (1, 1));
    }

    #[test]
    fn wide_chars_navigation() {
        let mut cm = CopyMode::new(lines(&["가나 다라 x"]), W, 1, (0, 0), true);
        cm.press("l");
        assert_eq!(cm.cx, 2, "skips continuation cell");
        // grid_reader_cursor_left stops on the padding (tmux 3.8 shows x=1), the next
        // step goes past the lead
        cm.press("h");
        assert_eq!(cm.cx, 1);
        cm.press("h");
        assert_eq!(cm.cx, 0);
        cm.press("w");
        assert_eq!(cm.cx, 5);
        cm.press("w");
        assert_eq!(cm.cx, 10);
        cm.press("b");
        assert_eq!(cm.cx, 5);
        // Moving onto a continuation column snaps to the lead.
        let mut cm = CopyMode::new(lines(&["abcd", "가나"]), W, 2, (0, 3), true);
        cm.press("j");
        assert_eq!(pos(&cm), (1, 2));
        // A cursor at the end of a line stays at line ends (tmux lastsx).
        let mut cm = CopyMode::new(lines(&["abc", "가나다"]), W, 2, (0, 3), true);
        cm.press("j");
        assert_eq!(pos(&cm), (1, 4), "vi: on the last character");
        let mut cm = CopyMode::new(lines(&["abc", "가나다"]), W, 2, (0, 3), false);
        cm.press("Down");
        assert_eq!(pos(&cm), (1, 6));
    }

    #[test]
    fn repeat_counts() {
        let mut cm = numbered();
        keys(&mut cm, &["5", "k"]);
        assert_eq!(cm.cy, 9);
        keys(&mut cm, &["1", "0", "j"]);
        assert_eq!(cm.cy, 14, "clamped at last line");
        keys(&mut cm, &["3", "l"]);
        assert_eq!(cm.cx, 3);
        keys(&mut cm, &["0"]);
        assert_eq!(cm.cx, 0, "0 without count is start-of-line");
        let mut cm = CopyMode::new(lines(&["a b c d e"]), W, 1, (0, 0), true);
        keys(&mut cm, &["3", "w"]);
        assert_eq!(cm.cx, 6);
        let mut cm = CopyMode::new(lines(&["a b c d e"]), W, 1, (0, 0), false);
        keys(&mut cm, &["M-2", "Right"]);
        assert_eq!(cm.cx, 2);
    }

    #[test]
    fn page_movement_clamps() {
        // a page is two lines less than the view, half a page half of it
        let mut cm = numbered();
        cm.press("k"); // row 3
        cm.press("C-b");
        assert_eq!(cm.scroll_offset(), 3);
        assert_eq!(cm.cy, 10);
        cm.press("PageUp");
        assert_eq!(cm.scroll_offset(), 6);
        cm.press("C-b");
        assert_eq!(cm.scroll_offset(), 9);
        assert_eq!(cm.cy, 4);
        cm.press("C-b");
        assert_eq!(cm.scroll_offset(), 10, "clamped at top");
        assert_eq!(cm.cy, 0, "cursor row moves up when the view can't");
        cm.press("C-f");
        assert_eq!(cm.scroll_offset(), 7);
        cm.press("PageDown");
        cm.press("C-f");
        assert_eq!(cm.scroll_offset(), 1);
        cm.press("C-f");
        assert_eq!(cm.scroll_offset(), 0);
        assert_eq!(cm.cy, 13, "cursor pushed down a page at the bottom");
        cm.press("C-u");
        assert_eq!(cm.scroll_offset(), 2);
        cm.press("C-d");
        assert_eq!(cm.scroll_offset(), 0);
    }

    #[test]
    fn scroll_line_keeps_cursor_row() {
        // emacs scroll-up / scroll-down: the view moves, the cursor keeps its screen row
        let v: Vec<String> = (0..15).map(|i| format!("line{i}")).collect();
        let v: Vec<&str> = v.iter().map(String::as_str).collect();
        let mut cm = CopyMode::new(lines(&v), W, 5, (4, 0), false);
        cm.press("Up");
        let row = cm.cy - cm.top;
        cm.press("C-Up");
        assert_eq!(cm.scroll_offset(), 1);
        assert_eq!((cm.cy - cm.top, cm.cy), (row, 12));
        cm.press("C-Up");
        assert_eq!((cm.cy - cm.top, cm.cy), (row, 11));
        cm.press("C-Down");
        assert_eq!(cm.scroll_offset(), 1);
        assert_eq!(cm.cy, 12);
        // vi: the cursor stays on its text (tmux 3.8) unless it would leave the view
        let mut cm = numbered();
        cm.press("k");
        cm.press("C-y");
        assert_eq!(cm.scroll_offset(), 1);
        assert_eq!((cm.cy - cm.top, cm.cy), (4, 13));
        cm.press("C-y");
        assert_eq!((cm.cy - cm.top, cm.cy), (4, 12), "pushed along at the bottom row");
        cm.press("C-e");
        assert_eq!((cm.cy - cm.top, cm.cy), (3, 12));
        cm.press("C-e");
        assert_eq!((cm.scroll_offset(), cm.cy - cm.top, cm.cy), (0, 2, 12));
    }

    #[test]
    fn scroll_middle() {
        let mut cm = numbered();
        keys(&mut cm, &["g", "z"]);
        assert_eq!(cm.scroll_offset(), 10);
        keys(&mut cm, &["1", "2", "j", "z"]);
        assert_eq!(cm.top, 10);
    }

    #[test]
    fn search_wraps_around() {
        let mut cm = CopyMode::new(lines(&["foo one", "bar", "foo two", "baz"]), W, 4, (1, 0), true);
        assert_eq!(cm.press("/"), Action::Prompt { label: "(search down)".into(), command: "search-forward".into() });
        cm.run("search-forward", &["foo".into()]);
        assert_eq!(pos(&cm), (2, 0));
        cm.press("n");
        assert_eq!(pos(&cm), (0, 0), "wrapped to the top");
        cm.press("n");
        assert_eq!(pos(&cm), (2, 0));
        cm.press("N");
        assert_eq!(pos(&cm), (0, 0));
        assert_eq!(cm.press("?"), Action::Prompt { label: "(search up)".into(), command: "search-backward".into() });
        cm.run("search-backward", &["ba".into()]);
        assert_eq!(pos(&cm), (3, 0), "backward wraps to the bottom");
        cm.press("n");
        assert_eq!(pos(&cm), (1, 0));
        // Not found leaves the cursor alone.
        cm.run("search-forward", &["nope".into()]);
        assert_eq!(pos(&cm), (1, 0));
        // Case-sensitive.
        cm.run("search-forward", &["FOO".into()]);
        assert_eq!(pos(&cm), (1, 0));
    }

    #[test]
    fn search_scrolls_and_handles_wide_chars() {
        let mut v: Vec<String> = (0..30).map(|i| format!("row{i}")).collect();
        v[3] = "한글 target".into();
        let v: Vec<&str> = v.iter().map(String::as_str).collect();
        let mut cm = CopyMode::new(lines(&v), W, 5, (4, 0), true);
        cm.run("search-backward", &["target".into()]);
        assert_eq!(pos(&cm), (3, 5));
        assert!(cm.top <= 3 && 3 < cm.top + 5);
        cm.run("search-forward", &["글".into()]);
        assert_eq!(pos(&cm), (3, 2));
        let (view, _) = cm.render();
        let row = &view[3 - cm.top].cells;
        assert_eq!(row[2].style.bg, MAGENTA, "current match");
        assert_eq!(row[3].style.bg, MAGENTA, "covers wide continuation");
    }

    #[test]
    fn emacs_search_prompts() {
        let mut cm = CopyMode::new(lines(&["x"]), W, 1, (0, 0), false);
        assert!(matches!(cm.press("C-s"), Action::Prompt { ref label, .. } if label == "(search down)"));
        assert!(matches!(cm.press("C-r"), Action::Prompt { ref label, .. } if label == "(search up)"));
        assert!(matches!(cm.press("g"), Action::Prompt { ref command, .. } if command == "goto-line"));
    }

    #[test]
    fn search_text_under_cursor() {
        let mut cm = CopyMode::new(lines(&["alpha beta", "gamma alpha"]), W, 2, (0, 1), true);
        cm.press("*");
        assert_eq!(pos(&cm), (1, 6));
        cm.press("n");
        assert_eq!(pos(&cm), (0, 0));
    }

    #[test]
    fn selection_copy_vi_inclusive() {
        let mut cm = CopyMode::new(lines(&["hello world", "second line"]), W, 2, (0, 0), true);
        keys(&mut cm, &["Space", "e"]);
        assert!(cm.selection_active());
        let a = cm.press("Enter");
        assert_eq!(a, Action::Copy { text: "hello".into(), cancel: true, append: false });
        // Multi-line, trailing spaces trimmed, newline between lines.
        let mut cm = CopyMode::new(lines(&["hello world", "second line"]), W, 2, (0, 6), true);
        keys(&mut cm, &["Space", "j", "0", "e"]);
        assert_eq!(copied(cm.press("Enter")), "world\nsecond");
        // Selecting backwards works too.
        let mut cm = CopyMode::new(lines(&["hello world"]), W, 1, (0, 4), true);
        keys(&mut cm, &["Space", "0"]);
        assert_eq!(copied(cm.run("copy-selection", &[])), "hello");
        assert!(!cm.selection_active(), "copy-selection clears");
        // v$ stops on the last character (tmux 3.7), so no newline.
        let mut cm = CopyMode::new(lines(&["abc"]), W, 1, (0, 1), true);
        keys(&mut cm, &["Space", "$"]);
        assert_eq!(copied(cm.run("copy-selection-no-clear", &[])), "bc");
        assert!(cm.selection_active());
    }

    #[test]
    fn selection_copy_emacs_exclusive() {
        let mut cm = CopyMode::new(lines(&["hello world"]), W, 1, (0, 0), false);
        keys(&mut cm, &["C-Space", "M-f"]);
        assert_eq!(cm.press("M-w"), Action::Copy { text: "hello".into(), cancel: true, append: false });
        let mut cm = CopyMode::new(lines(&["abc", "def"]), W, 2, (0, 0), false);
        keys(&mut cm, &["C-Space", "C-e", "C-n", "C-e"]);
        assert_eq!(copied(cm.press("M-w")), "abc\ndef");
        // C-g clears, Escape cancels.
        let mut cm = CopyMode::new(lines(&["abc"]), W, 1, (0, 0), false);
        cm.press("C-Space");
        cm.press("C-g");
        assert!(!cm.selection_active());
        assert_eq!(cm.press("Escape"), Action::Cancel);
    }

    #[test]
    fn copy_joins_soft_wrapped_lines() {
        let v = vec![line("aaaaaaaaaaaaaaaaaaaa", true), line("bbb   ", false), line("ccc", false)];
        let mut cm = CopyMode::new(v, W, 3, (0, 0), true);
        keys(&mut cm, &["Space", "j", "j", "$"]);
        assert_eq!(copied(cm.press("Enter")), format!("{}bbb\nccc", "a".repeat(20)));
    }

    #[test]
    fn copy_wide_chars_once() {
        let mut cm = CopyMode::new(lines(&["x 한국어 y"]), W, 1, (0, 2), true);
        keys(&mut cm, &["Space", "e"]);
        assert_eq!(cm.cx, 6);
        assert_eq!(copied(cm.press("Enter")), "한국어");
    }

    #[test]
    fn line_selection() {
        let mut cm = CopyMode::new(lines(&["one", "two", "three"]), W, 3, (0, 1), true);
        keys(&mut cm, &["V", "j"]);
        assert_eq!(copied(cm.press("Enter")), "one\ntwo\n");
        let mut cm = CopyMode::new(lines(&["one", "two", "three"]), W, 3, (2, 2), true);
        keys(&mut cm, &["V", "k"]);
        assert_eq!(copied(cm.press("Enter")), "two\nthree\n");
        let (view, _) = {
            let mut cm = CopyMode::new(lines(&["one", "two"]), W, 2, (0, 1), true);
            cm.press("V");
            cm.render()
        };
        // screen_check_selection: vi selects up to the line's length (one past the text)
        assert!(view[0].cells[..4].iter().all(|c| c.style.bg == YELLOW && c.style.fg == BLACK));
        assert!(view[0].cells[4..15].iter().all(|c| c.style.bg != YELLOW));
        assert!(view[1].cells.iter().all(|c| c.style.bg != YELLOW));
    }

    #[test]
    fn highlight_follows_screen_check_selection() {
        let marked = |cm: &CopyMode, row: usize| -> String {
            let (view, _) = cm.render_with(&RenderStyle { position_text: Some(String::new()), ..RenderStyle::default() });
            view[row].cells.iter().map(|c| if c.style.bg == YELLOW { c.text.as_str() } else { "." }).collect::<String>().trim_end_matches('.').to_string()
        };
        let text = ["abcdefghij", "klmnopqrst", "uvwxyz"];
        // an emacs rectangle includes both columns, also when dragged leftwards
        let mut cm = CopyMode::new(lines(&text), 20, 3, (0, 2), false);
        cm.run("begin-selection", &[]);
        cm.run("rectangle-on", &[]);
        cm.run("cursor-down", &[]);
        (0..3).for_each(|_| { cm.run("cursor-right", &[]); });
        assert_eq!((marked(&cm, 0), marked(&cm, 1)), ("..cdef".to_string(), "..mnop".to_string()));
        let mut cm = CopyMode::new(lines(&text), 20, 3, (0, 8), false);
        cm.run("begin-selection", &[]);
        cm.run("rectangle-on", &[]);
        cm.run("cursor-down", &[]);
        (0..3).for_each(|_| { cm.run("cursor-left", &[]); });
        assert_eq!((marked(&cm, 0), marked(&cm, 1)), (".....fghi".to_string(), ".....pqrs".to_string()));
        assert_eq!(copied(cm.run("copy-selection", &[])), "fghi\npqrs");
        // emacs: an empty selection at column 0 shows that cell, a downward one ending at
        // column 0 shows it too
        let mut cm = CopyMode::new(lines(&text), 20, 3, (0, 0), false);
        cm.run("begin-selection", &[]);
        assert_eq!(marked(&cm, 0), "a");
        cm.run("cursor-down", &[]);
        assert_eq!((marked(&cm, 0), marked(&cm, 1)), ("abcdefghij          ".to_string(), "k".to_string()));
        // vi upward from column 0: the start row is not shown
        let mut cm = CopyMode::new(lines(&text), 20, 3, (1, 0), true);
        cm.run("begin-selection", &[]);
        cm.run("cursor-up", &[]);
        assert_eq!((marked(&cm, 0), marked(&cm, 1)), ("abcdefghij          ".to_string(), String::new()));
        // screen_select_cell: default colours and attributes come from the cell
        let mut l = line("ab", false);
        l.cells[0].style = Style { fg: Color::Idx(1), bg: Color::Default, attrs: crate::cells::BOLD, ..Style::default() };
        let mut cm = CopyMode::new(vec![l], 20, 1, (0, 0), true);
        cm.run("begin-selection", &[]);
        let st = RenderStyle { selection: Style { fg: Color::Default, bg: CYAN, attrs: 0, ..Style::default() }, selection_noattr: false, position_text: Some(String::new()), ..RenderStyle::default() };
        assert_eq!(cm.render_with(&st).0[0].cells[0].style, Style { fg: Color::Idx(1), bg: CYAN, attrs: crate::cells::BOLD, ..Style::default() });
        let st = RenderStyle { selection_noattr: true, ..st };
        assert_eq!(cm.render_with(&st).0[0].cells[0].style, Style { fg: Color::Idx(1), bg: CYAN, attrs: 0, ..Style::default() });
    }

    #[test]
    fn rectangle_selection() {
        let mut cm = CopyMode::new(lines(&["abcdef", "ghijkl", "mn", "stuvwx"]), W, 4, (0, 1), true);
        keys(&mut cm, &["Space", "C-v", "3", "j", "l", "l"]);
        assert_eq!(copied(cm.press("Enter")), "bcd\nhij\nn\ntuv");
        // v is rectangle-toggle in the default vi table.
        let mut cm = CopyMode::new(lines(&["abcdef", "ghijkl"]), W, 2, (0, 1), true);
        keys(&mut cm, &["Space", "v", "j", "l"]);
        assert_eq!(copied(cm.press("Enter")), "bc\nhi");
        let mut cm = CopyMode::new(lines(&["abcdef", "ghijkl"]), W, 2, (0, 1), false);
        keys(&mut cm, &["C-Space", "R", "Down", "Right", "Right"]);
        assert_eq!(copied(cm.press("M-w")), "bc\nhi");
    }

    #[test]
    fn select_word_other_end_and_clear() {
        let mut cm = CopyMode::new(lines(&["foo barbaz qux"]), W, 1, (0, 6), true);
        cm.run("select-word", &[]);
        assert_eq!(copied(cm.run("copy-selection-no-clear", &[])), "barbaz");
        cm.press("o");
        assert_eq!(cm.cx, 4);
        assert_eq!(copied(cm.run("copy-selection-no-clear", &[])), "barbaz");
        cm.press("Escape");
        assert!(!cm.selection_active());
        assert_eq!(cm.press("Enter"), Action::Cancel, "copy without selection just cancels");
        assert_eq!(cm.run("copy-selection", &[]), Action::None);
    }

    #[test]
    fn copy_line_and_end_of_line() {
        let mut cm = CopyMode::new(lines(&["first line", "second"]), W, 2, (0, 6), true);
        // vi copies to the last character, without the newline (tmux 3.8)
        assert_eq!(cm.run("copy-end-of-line", &[]), Action::Copy { text: "line".into(), cancel: false, append: false });
        assert_eq!(pos(&cm), (0, 6), "state restored");
        assert_eq!(copied(cm.run("copy-line", &[])), "first line");
        cm.count = Some(2);
        assert_eq!(copied(cm.run("copy-line", &[])), "first line\nsecond");
        assert_eq!(cm.press("D"), Action::Copy { text: "line".into(), cancel: true, append: false });
        let mut cm = CopyMode::new(lines(&["first line"]), W, 1, (0, 6), false);
        assert_eq!(cm.press("C-k"), Action::Copy { text: "line".into(), cancel: true, append: false });
    }

    #[test]
    fn edge_scroll_without_history_and_mouse_moves_keep_the_selection_end() {
        // window_copy_cursor_up/down(scroll_only) at the top or bottom move only the cursor
        // (vi), and window_copy_move_mouse never updates the selection
        let text = ["top line", "second", "third", "fourth"];
        let mut cm = CopyMode::new(lines(&text), 30, 8, (4, 0), true);
        cm.start_drag(3, 3);
        cm.drag_to(0, 2);
        cm.scroll_line(true);
        assert_eq!(cm.view_cursor(), (1, 2));
        assert_eq!(cm.selection_ends(), Some(((3, 3), (2, 0))));
        cm.end_drag();
        cm.move_mouse(0, 2);
        assert_eq!(copied(cm.run("copy-pipe-and-cancel", &[])), "p line\nsecond\nthird\nfour");
        // at the bottom of the history: the last row stays selected
        let mut cm = CopyMode::new(lines(&["row1", "row2", "lastr"]), 30, 3, (2, 0), true);
        cm.start_drag(0, 1);
        cm.drag_to(2, 4);
        cm.scroll_line(false);
        assert_eq!(cm.view_cursor(), (1, 4));
        cm.end_drag();
        cm.move_mouse(2, 4);
        assert_eq!(cm.view_cursor(), (2, 4));
        assert_eq!(copied(cm.run("copy-selection", &[])), "ow1\nrow2\nlastr");
        // a command from MouseDragEnd: the cursor goes to the release point first
        let mut cm = CopyMode::new(lines(&text), 30, 8, (4, 0), false);
        cm.start_drag(0, 0);
        cm.drag_to(1, 3);
        cm.end_drag();
        cm.move_mouse(2, 4);
        cm.run("stop-selection", &[]);
        assert_eq!(cm.view_cursor(), (2, 4));
        assert_eq!(cm.selection_ends(), Some(((0, 0), (3, 1))));
    }

    #[test]
    fn copy_end_of_line_keeps_selection_flags_and_clears_the_selection() {
        // window_copy_do_copy_end_of_line: start_selection keeps selflag, lineflag and
        // rectflag; the selection is cleared afterwards (checked against tmux 3.8)
        let text = ["one two three", "four five", "six seven", "eight"];
        let run = |vi: bool, pre: &str, n: usize, cmd: &str| {
            let mut cm = CopyMode::new(lines(&text), 30, 6, (0, 0), vi);
            cm.run("history-top", &[]);
            for _ in 0..5 { cm.run("cursor-right", &[]); }
            if !pre.is_empty() { cm.run(pre, &[]); }
            cm.count = Some(n);
            let t = copied(cm.run(cmd, &[]));
            assert!(!cm.selection_present() && !cm.selection_following() && cm.selection_mode() == SelMode::Char, "{pre} {cmd}");
            t
        };
        assert_eq!(run(false, "", 1, "copy-end-of-line"), "wo three");
        assert_eq!(run(false, "begin-selection", 2, "copy-end-of-line"), "wo three\nfour five");
        assert_eq!(run(false, "select-line", 1, "copy-end-of-line"), "one two three");
        assert_eq!(run(false, "select-word", 1, "copy-end-of-line"), "two three\nfour");
        assert_eq!(run(false, "select-word", 2, "copy-end-of-line"), "two three\nfour five\nsix");
        assert_eq!(run(false, "rectangle-on", 2, "copy-end-of-line"), "wo three\nfive");
        assert_eq!(run(true, "", 2, "copy-end-of-line"), "wo three\nfour five");
        assert_eq!(run(true, "select-line", 2, "copy-end-of-line"), "one two three\nfour five\n");
        assert_eq!(run(true, "select-word", 1, "copy-end-of-line"), "two three");
        assert_eq!(run(true, "rectangle-on", 1, "copy-end-of-line"), "wo three\n");
        assert_eq!(run(true, "rectangle-on", 2, "copy-end-of-line"), "wo three\nfive\n");
        assert_eq!(run(true, "begin-selection", 1, "copy-line"), "one two three");
        assert_eq!(run(false, "select-line", 1, "copy-line"), "one two three");
    }

    #[test]
    fn regex_search_and_literal_text_search() {
        let mut cm = CopyMode::new(lines(&["abc123", "a.b", "acb", "end"]), W, 4, (3, 0), true);
        cm.run("search-backward", &["^abc[[:digit:]]+ *$".into()]);
        assert_eq!(pos(&cm), (0, 0));
        cm.run("search-forward-text", &["a.b".into()]);
        assert_eq!(pos(&cm), (1, 0), "text search escapes the dot");
        cm.run("search-again", &[]);
        assert_eq!(pos(&cm), (1, 0), "literal text must not match acb");
        cm.run("search-forward", &["a.b".into()]);
        assert_eq!(pos(&cm), (2, 0), "ordinary search uses regex semantics");
    }

    #[test]
    fn incremental_search_preview_commit_and_cancel() {
        // as tmux 3.8: each edit sends "=text"; a changed string restarts from the first
        // search's row at its end; Enter and cancel send nothing
        let mut cm = CopyMode::new(lines(&["foo", "food", "finish"]), W, 3, (2, 3), false);
        cm.begin_incremental_search(false);
        cm.preview_search("f", false);
        assert_eq!(pos(&cm), (2, 0));
        cm.preview_search("foo", false);
        assert_eq!(pos(&cm), (1, 0));
        cm.preview_search("food", false);
        assert_eq!(pos(&cm), (1, 0), "typing extends the search from its original row");
        cm.preview_search("", false);
        assert_eq!(pos(&cm), (2, 6), "deleting the query goes back to the end of the first row");
        assert!(!cm.search_present());
        cm.preview_search("foo", false);
        cm.finish_incremental_search(true);
        assert_eq!(pos(&cm), (1, 0), "cancel keeps the cursor");
        // reopening sends "=": the string changed, so the cursor goes back and the marks go
        cm.begin_incremental_search(false);
        assert_eq!((pos(&cm), cm.search_present()), ((2, 6), false));
        cm.finish_incremental_search(false);
        cm.run("search-again", &[]);
        assert_eq!(pos(&cm), (1, 0));
        cm.run("search-again", &[]);
        assert_eq!(pos(&cm), (0, 0));
        // a command that clears the marks forgets the restart position
        assert!(cm.incremental_command_origin.is_some());
        cm.run("cursor-down", &[]);
        assert!(cm.incremental_command_origin.is_none());
    }

    #[test]
    fn refresh_keeps_search_marks() {
        let mut cm = CopyMode::new(lines(&["foo", "bar foo", "x"]), W, 3, (2, 0), true);
        cm.run("search-backward", &["foo".into()]);
        assert_eq!(cm.search_count(), Some(2));
        cm.refresh(lines(&["foo", "bar foo", "x", "foo"]));
        assert!(cm.search_present());
        assert_eq!(cm.search_count(), Some(3));
    }


    #[test]
    fn matching_brackets_and_default_keys() {
        let mut cm = CopyMode::new(lines(&["x (a [b] (c)) end", "plain", "{가", "}"]), W, 4, (0, 0), true);
        cm.press("%");
        assert_eq!(pos(&cm), (0, 12), "vi scans to the first bracket and handles nesting");
        cm.press("%");
        assert_eq!(pos(&cm), (0, 2), "vi closing bracket jumps back");
        cm.set_cursor(1, 0);
        cm.press("%");
        assert_eq!(pos(&cm), (1, 0), "vi does not scan beyond a hard line break");
        cm.set_cursor(2, 0);
        cm.press("%");
        assert_eq!(pos(&cm), (3, 0), "a matching pair may span lines");
        cm.run("previous-matching-bracket", &[]);
        assert_eq!(pos(&cm), (2, 0));
        cm.set_cursor(0, 0);
        assert_eq!(cm.press("y"), Action::None, "y is unbound in tmux's default vi table");
        let mut emacs = CopyMode::new(lines(&["(a (b))"]), W, 1, (0, 0), false);
        emacs.press("C-M-f");
        assert_eq!(pos(&emacs), (0, 6));
        emacs.press("C-M-b");
        assert_eq!(pos(&emacs), (0, 0));
        let mut missing = CopyMode::new(lines(&["(no close"]), W, 1, (0, 0), true);
        missing.press("%");
        assert_eq!(pos(&missing), (0, 0));
    }

    #[test]
    fn jumps() {
        let mut cm = CopyMode::new(lines(&["a.b.c.d"]), W, 1, (0, 0), true);
        keys(&mut cm, &["f", "."]);
        assert_eq!(cm.cx, 1);
        cm.press(";");
        assert_eq!(cm.cx, 3);
        cm.press(",");
        assert_eq!(cm.cx, 1);
        keys(&mut cm, &["0", "t", "."]);
        assert_eq!(cm.cx, 2, "t starts looking two cells right, like tmux");
        keys(&mut cm, &["t", "c"]);
        assert_eq!(cm.cx, 3);
        cm.press(";");
        assert_eq!(cm.cx, 3, "no further c");
        keys(&mut cm, &["$", "F", "b"]);
        assert_eq!(cm.cx, 2);
        keys(&mut cm, &["$", "T", "b"]);
        assert_eq!(cm.cx, 3);
        keys(&mut cm, &["0", "2", "f", "."]);
        assert_eq!(cm.cx, 3, "count applies to jump");
        keys(&mut cm, &["f", "z"]);
        assert_eq!(cm.cx, 3, "missing char doesn't move");
        keys(&mut cm, &["f", "Escape", "l"]);
        assert_eq!(cm.cx, 4, "non-char key aborts the pending jump");
        cm.run("jump-backward", &["a".into()]);
        assert_eq!(cm.cx, 0);
        let mut cm = CopyMode::new(lines(&["x 가 y"]), W, 1, (0, 0), true);
        keys(&mut cm, &["f", "Space"]);
        assert_eq!(cm.cx, 1);
        keys(&mut cm, &["f", "y"]);
        assert_eq!(cm.cx, 5);
    }

    #[test]
    fn goto_line_and_paragraphs() {
        let mut cm = numbered();
        assert!(matches!(cm.press(":"), Action::Prompt { ref command, .. } if command == "goto-line"));
        cm.run("goto-line", &["7".into()]);
        assert_eq!(cm.scroll_offset(), 7);
        assert_eq!(cm.cy, 3 + 4);
        cm.run("goto-line", &["999".into()]);
        assert_eq!(cm.scroll_offset(), 10);
        let mut cm = CopyMode::new(lines(&["a", "b", "", "c", "d", "", "e"]), W, 7, (6, 0), true);
        cm.press("{");
        assert_eq!(cm.cy, 5);
        cm.press("{");
        assert_eq!(cm.cy, 2);
        cm.press("}");
        assert_eq!(cm.cy, 5);
        cm.press("}");
        assert_eq!(pos(&cm), (6, 1));
    }

    #[test]
    fn marks() {
        let mut cm = numbered();
        cm.press("X");
        cm.press("g");
        cm.press("M-x");
        assert_eq!(pos(&cm), (14, 0));
        cm.press("M-x");
        assert_eq!(pos(&cm), (0, 0));
    }

    #[test]
    fn cancel_keys() {
        let mut cm = numbered();
        assert_eq!(cm.press("q"), Action::Cancel);
        assert_eq!(cm.press("C-c"), Action::Cancel);
        assert_eq!(cm.press("Escape"), Action::None, "vi Escape clears the selection");
        assert_eq!(cm.run("cancel", &[]), Action::Cancel);
        assert_eq!(cm.press("unbound-key"), Action::None);
    }

    #[test]
    fn render_selection_and_indicator() {
        let mut cm = numbered();
        keys(&mut cm, &["k", "k", "C-y", "l", "Space", "l", "l"]);
        let (view, cur) = cm.render();
        assert_eq!(cm.scroll_offset(), 1);
        let ind = "[1/10]";
        let top: String = view[0].cells.iter().map(|c| c.text.as_str()).collect();
        assert!(top.ends_with(ind), "{top:?}");
        let start = W as usize - ind.len();
        assert!(view[0].cells[start..].iter().all(|c| c.style.bg == YELLOW && c.style.fg == BLACK));
        // vi C-y kept the cursor on its line 12, now screen row 3 (top 9).
        assert_eq!(cur, (3, 3));
        let inv: Vec<bool> = view[3].cells[..6].iter().map(|c| c.style.bg == YELLOW).collect();
        assert_eq!(inv, vec![false, true, true, true, false, false]);
        assert!(view[1].cells.iter().all(|c| c.style.bg != YELLOW));
        cm.press("P");
        let (view, _) = cm.render();
        assert!(!view[0].cells.iter().any(|c| c.style.bg == YELLOW));
    }

    #[test]
    fn select_word_like_tmux() {
        let pick = |text: &str, x: u16, vi: bool| {
            let mut cm = CopyMode::new(lines(&[text]), W, 1, (0, x), vi);
            cm.set_word_separators("!\"#$%&'()*+,-./:;<=>?@[\\]^`{|}~");
            cm.run("select-word", &[]);
            match cm.run("copy-selection", &[]) {
                Action::Copy { text, .. } => text,
                _ => String::new(),
            }
        };
        for vi in [false, true] {
            assert_eq!(pick("alpha beta gamma", 8, vi), "beta");
            // on a space: the word before it
            assert_eq!(pick("history 48 filler", 10, vi), "48");
            // separators end words; a run of separators is a word
            assert_eq!(pick("gamma-delta", 18 - 10, vi), "delta");
            assert_eq!(pick("a -- b", 3, vi), "--");
        }
        // a single character word leaves the cursor on it: an empty selection in emacs
        assert_eq!(pick("x=1 y=22", 2, true), "1");
        assert_eq!(pick("x=1 y=22", 2, false), "");
    }

    #[test]
    fn render_highlights_matches() {
        let mut cm = CopyMode::new(lines(&["ab ab", "xab"]), W, 2, (0, 0), true);
        cm.run("search-forward", &["ab".into()]);
        assert_eq!(pos(&cm), (0, 3));
        cm.show_position = false; // Test matches independently of the result-count overlay.
        let (view, _) = cm.render();
        assert_eq!(view[0].cells[0].style.bg, CYAN);
        assert_eq!(view[0].cells[3].style.bg, MAGENTA);
        assert_eq!(view[0].cells[2].style.bg, Color::Default);
        assert_eq!(view[1].cells[1].style.bg, CYAN);
    }

    #[test]
    fn emacs_motion_clears_search_marks_but_keeps_query_and_selection_style() {
        let mut cm = CopyMode::new(lines(&["first first", "last"]), 40, 2, (1, 0), false);
        cm.run("search-backward", &["first".into()]);
        assert!(cm.search_marks);
        cm.run("start-of-line", &[]);
        assert!(!cm.search_marks);
        cm.run("begin-selection", &[]);
        cm.run("cursor-right", &[]);
        let selected = Style { fg: BLACK, bg: CYAN, attrs: 0, ..Style::default() };
        let (view,_) = cm.render_with_style(selected);
        assert_eq!(view[0].cells[0].style, selected);
        assert_eq!(view[0].cells[2].style.bg, Color::Default);
        // the count goes with the marks (tmux 3.7); searching again for the same text
        // marks the matches but does not count them
        assert!(!view[0].text().contains("results"));
        assert_eq!(cm.search_count(), None);
        cm.run("search-again", &[]);
        assert!(cm.search_marks);
        assert_eq!(cm.search_count(), None);
        cm.run("search-forward", &["las".into()]);
        assert!(cm.render().0[0].text().ends_with("(1 results)"));
    }

    #[test]
    fn resize_and_set_lines() {
        let mut cm = numbered();
        keys(&mut cm, &["k", "C-y"]);
        let (off, cy) = (cm.scroll_offset(), cm.cy);
        let mut more: Vec<Line> = (0..3).map(|i| line(&format!("new{i}"), false)).collect();
        let mut v = cm.lines.to_vec();
        v.append(&mut more);
        cm.set_lines(v);
        assert_eq!(cm.cy, cy + 3, "position kept relative to the bottom");
        assert_eq!(cm.scroll_offset(), off);
        cm.resize(W, 3);
        assert_eq!(cm.render().0.len(), 3);
        assert!(cm.cy >= cm.top && cm.cy < cm.top + 3);
        cm.resize(10, 30);
        assert_eq!(cm.scroll_offset(), 0);
        let (view, _) = cm.render();
        assert_eq!(view.len(), 30);
        assert!(view.iter().all(|l| l.cells.len() == 10));
    }

    #[test]
    fn reflow_keeps_line_marks_times_and_cell_metadata_on_their_rows() {
        let nz = |n| std::num::NonZeroU16::new(n).unwrap();
        let mut p = vt100::Parser::new(nz(4), nz(20), 50);
        p.screen_mut().set_time(1000);
        p.process(b"\x1b]133;A\x07$ abcdefghijklmnop\r\n\x1b]133;C\x07a\tb\x1b]8;;http://z\x07zz\x1b]8;;\x07\r\n\r\n\r\n\r\nend");
        let lines = crate::cells::snapshot(p.screen());
        assert_eq!((lines[0].flags, lines[0].time, lines[1].time), (vt100::ROW_START_PROMPT, 1000, 1000));
        let mut cm = CopyMode::new(lines, 20, 4, (3, 3), true);
        cm.resize(8, 4);
        // the prompt line is split: its mark and time stay on its first row
        let rows: Vec<(u8, u64, bool)> = cm.lines.iter().take(4).map(|l| (l.flags, l.time, l.wrapped)).collect();
        assert_eq!(rows, [(vt100::ROW_START_PROMPT, 1000, true), (0, 0, true), (0, 0, false), (vt100::ROW_START_OUTPUT | vt100::ROW_HYPERLINK | vt100::ROW_EXTENDED, 1000, true)]);
        // the tab is a cell of its width and each padding cell counts a column
        // (grid_reflow): the padding goes on to the next row, where it shows nothing, and
        // takes the link's cells with it (capture-pane -M -F: "OHWX a^I", "HWX bz", "HX z")
        assert_eq!(cm.lines[3].tab_at(4), Some((1, 7)));
        assert_eq!((cm.lines[3].cells.len(), cm.lines[4].flags), (2, vt100::ROW_HYPERLINK | vt100::ROW_EXTENDED));
        let text = |l: &Line| (0..l.cells.len()).map(|x| l.cell_text(x)).collect::<String>();
        assert_eq!((text(&cm.lines[4]), (0..6).all(|x| cm.lines[4].is_padding(x))), ("bz".into(), true));
        assert_eq!((cm.lines[4].hyperlink_at(7).map(|l| l.uri.as_str()), cm.lines[5].hyperlink_at(0).map(|l| l.uri.as_str())), (Some("http://z"), Some("http://z")));
        // drawn, the moved padding is blank so the text after it keeps its column
        let (top, shown) = (cm.top, cm.show_position);
        (cm.top, cm.show_position) = (4, false);
        let drawn: String = cm.render().0[0].cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(drawn, "      bz");
        (cm.top, cm.show_position) = (top, shown);
        cm.run("history-top", &[]);
        cm.run("next-prompt", &["-o".into()]);
        assert_eq!(cm.cursor_line(), "a\t");
        // wider again the line is whole, the tab still a tab
        cm.resize(20, 4);
        assert_eq!(text(&cm.lines[1]).trim_end(), "a\tbzz");
    }

    #[test]
    fn reflow_again_keeps_a_split_tab_and_links_on_their_cells() {
        // a tab split by one resize keeps its metadata on its own cell's row with its
        // padding on the next; a second resize to any width placed that padding past
        // where the next row starts and underflowed (debug builds panicked). In tmux the
        // tab flag and links are part of the cells, so whatever the widths each stays
        // on its cell: the unwrapped line reads the same as after one resize.
        let nz = |n| std::num::NonZeroU16::new(n).unwrap();
        let mut p = vt100::Parser::new(nz(4), nz(20), 50);
        p.process(b"a\tb\x1b]8;;http://z\x07zz\x1b]8;;\x07\r\nxy");
        let lines = crate::cells::snapshot(p.screen());
        // each unwrapped line: its text and (offset, tab width, link) of its metadata
        type Unwrapped = (String, Vec<(usize, u16, Option<String>)>);
        let unwrapped = |cm: &CopyMode| -> Vec<Unwrapped> {
            let mut out: Vec<Unwrapped> = Vec::new();
            let mut open = false;
            for l in cm.lines.to_vec() {
                if !open {
                    out.push(Default::default());
                }
                let (text, meta) = out.last_mut().unwrap();
                let at = text.chars().count();
                meta.extend(l.metadata.iter().map(|(&x, m)| (at + usize::from(x), m.tab_width, m.hyperlink.as_ref().map(|h| h.uri.clone()))));
                text.extend((0..cells_used(&l)).map(|x| if l.is_padding(x) { '_' } else { l.cell_text(x).chars().next().unwrap_or(' ') }));
                open = l.wrapped;
            }
            out.into_iter().filter(|(t, _)| !t.trim().is_empty()).collect()
        };
        let mut cm = CopyMode::new(lines.clone(), 20, 4, (0, 0), true);
        let whole = unwrapped(&cm);
        assert_eq!(whole[0].1, [(1, 7, None), (9, 0, Some("http://z".into())), (10, 0, Some("http://z".into()))]);
        cm.resize(8, 4);
        assert_eq!(cm.lines[0].tab_at(1), Some((1, 7)));
        for width in 1..30 {
            let mut twice = CopyMode::new(lines.clone(), 20, 4, (0, 0), true);
            twice.resize(8, 4);
            twice.resize(width, 4);
            let mut once = CopyMode::new(lines.clone(), 20, 4, (0, 0), true);
            once.resize(width, 4);
            assert_eq!(unwrapped(&twice), unwrapped(&once), "8 then {width} columns");
            assert_eq!(unwrapped(&twice)[0].1, whole[0].1, "8 then {width} columns");
        }
    }

    #[test]
    fn reflow_gives_each_row_the_cells_tmux_does() {
        // gl->cellsize after window_copy_resize's grid_reflow, as tmux 3.8's capture-pane
        // -R shows it for the pane itself: a line moved across keeps its size, the first
        // part of a split line has exactly its cells, the other parts and the cells joined
        // onto a line are written a cell at a time (so 10 cells at 40 columns is a quarter,
        // and 20 a half, not the next size), the rest of a line a join took part of has
        // what is left. capture-pane -M -N and -e read it (0 lost every line's text).
        let nz = |n| std::num::NonZeroU16::new(n).unwrap();
        let zeros = |n: usize| "0".repeat(n);
        let text = format!("ab\r\nabcdefghijklmnopqrstuvwxyz\r\n{}\r\n{}\r\n{}\r\n{}\r\nq", zeros(50), zeros(60), zeros(70), zeros(100));
        let sizes = |lines: &[Line]| lines.iter().map(|l| l.cellsize).take_while(|&n| n > 0).collect::<Vec<_>>();
        let pane = |cols: u16| {
            let mut p = vt100::Parser::new(nz(14), nz(80), 50);
            p.process(text.as_bytes());
            if cols != 80 {
                p.set_size(nz(14), nz(cols));
            }
            sizes(&crate::cells::snapshot(p.screen()))
        };
        let mut p = vt100::Parser::new(nz(14), nz(80), 50);
        p.process(text.as_bytes());
        let mut cm = CopyMode::new(crate::cells::snapshot(p.screen()), 80, 14, (0, 0), true);
        assert_eq!(sizes(&cm.lines.to_vec()), [20, 40, 80, 80, 80, 80, 40, 20]);
        cm.resize(40, 14);
        p.set_size(nz(14), nz(40));
        let at40 = sizes(&cm.lines.to_vec());
        assert_eq!(at40, [20, 40, 40, 10, 40, 20, 40, 40, 40, 40, 40, 20]);
        assert_eq!(at40, sizes(&crate::cells::snapshot(p.screen())));
        cm.resize(120, 14);
        p.set_size(nz(14), nz(120));
        let at120 = sizes(&cm.lines.to_vec());
        assert_eq!(at120, [20, 40, 60, 60, 120, 120, 20]);
        assert_eq!(at120, sizes(&crate::cells::snapshot(p.screen())));
        // a line joined at 200 columns: 80 cells and 20 more is a half (tmux 100, not 200)
        let mut q = vt100::Parser::new(nz(14), nz(80), 50);
        q.process(text.as_bytes());
        let mut cm = CopyMode::new(crate::cells::snapshot(q.screen()), 80, 14, (0, 0), true);
        cm.resize(200, 14);
        assert_eq!(sizes(&cm.lines.to_vec()), [20, 40, 80, 80, 80, 100, 20]);
        assert_eq!(pane(200), [20, 40, 80, 80, 80, 100, 20]);
        // copy-mode -s of the pane into one of another width rewraps it the same way
        let cm = CopyMode::clone_from(crate::cells::snapshot(q.screen()), (80, 14), (6, 1), (40, 14), true, true);
        assert_eq!(sizes(&cm.lines.to_vec()), at40);
        assert_eq!(pane(40), at40);
    }

    #[test]
    fn cursor_left_steps_onto_padding_and_cursor_hyperlink_reads_the_drawn_screen() {
        let nz = |n| std::num::NonZeroU16::new(n).unwrap();
        let mut p = vt100::Parser::new(nz(6), nz(30), 50);
        p.process("ab\tcd\tef\r\n\tX\r\na\u{4e2d}\u{6587}b\r\nab\x1b]8;;http://z\x07ZZZZ\x1b]8;;\x07cd".as_bytes());
        let mut cm = CopyMode::new(crate::cells::snapshot(p.screen()), 30, 6, (0, 0), false);
        // grid_reader_cursor_left lands on a tab's or wide character's padding; the next
        // step goes past its start (tmux 3.8: 8 7 18 17 16 15 and 6 5 4 2 0)
        let lefts = |cm: &mut CopyMode, y: usize, n: usize| -> Vec<(usize, usize)> {
            cm.set_cursor(y, 0);
            cm.run("end-of-line", &[]);
            (0..n).map(|_| { cm.run("cursor-left", &[]); (cm.cx, cm.cy) }).collect()
        };
        assert_eq!(lefts(&mut cm, 1, 6), [(8, 1), (7, 1), (18, 0), (17, 0), (16, 0), (15, 0)]);
        assert_eq!(lefts(&mut cm, 2, 4), [(5, 2), (4, 2), (2, 2), (0, 2)]);
        // copy_cursor_hyperlink: the copy-mode screen as drawn, at the backing column, so
        // the position indicator ("[0/0]" at the right) hides a link and line numbers
        // shift what is read
        let mut p = vt100::Parser::new(nz(6), nz(10), 50);
        p.process(b"\r\n\r\n\r\nab\x1b]8;;http://z\x07ZZZZ\x1b]8;;\x07cd");
        let mut cm = CopyMode::new(crate::cells::snapshot(p.screen()), 10, 3, (0, 0), false);
        let links = |cm: &mut CopyMode| -> Vec<bool> {
            (0..8).map(|x| { cm.set_cursor(3, x); cm.cursor_hyperlink(&RenderStyle::default()).is_some() }).collect()
        };
        assert_eq!((cm.top, links(&mut cm)), (3, vec![false, false, true, true, true, false, false, false]));
        cm.top = 2;
        assert_eq!(links(&mut cm), [false, false, true, true, true, true, false, false]);
        cm.set_line_number_option(LineNumbers::Absolute);
        cm.set_line_numbers(true);
        let lnw = (0..10).position(|x| { cm.set_cursor(3, x); cm.cursor_hyperlink(&RenderStyle::default()).is_some() }).unwrap() - 2;
        assert!(lnw > 0);
    }

    #[test]
    fn clone_trims_resizes_and_rewraps_like_tmux() {
        let row = |s: &str, w: usize, wrapped: bool| {
            let mut l = Line { cells: s.chars().map(|c| Cell { text: c.to_string(), style: Style::default() }).collect(), wrapped, ..Default::default() };
            l.cells.resize(w, Cell::default());
            l
        };
        let texts = |cm: &CopyMode| -> Vec<String> { cm.render_with(&RenderStyle { position_text: Some(String::new()), ..RenderStyle::default() }).0.iter().map(|l| l.cells.iter().map(|c| c.text.as_str()).collect::<String>().trim_end().to_string()).collect() };
        // copy-mode -s from a taller pane: the empty rows go, the view is the target's height
        // (live tmux 3.8: a2 a3 a4, cursor on a4, position 0/1)
        let src: Vec<Line> = ["a1", "a2", "a3", "a4", "", "", ""].iter().map(|s| row(s, 30, false)).collect();
        let cm = CopyMode::clone_from(src, (30, 7), (4, 0), (30, 3), true, true);
        assert_eq!(texts(&cm)[..3], ["a2", "a3", "a4"]);
        assert_eq!((cm.cursor(), cm.cursor_line(), cm.position()), ((0, 2), "a4".to_string(), (0, 1)));
        // from a shorter pane: history comes down into the taller view (14..20, cursor on 20)
        let src: Vec<Line> = (1..=20).map(|i| row(&i.to_string(), 30, false)).chain([row("", 30, false)]).collect();
        let cm = CopyMode::clone_from(src, (30, 3), (2, 0), (30, 7), true, true);
        assert_eq!(texts(&cm), ["14", "15", "16", "17", "18", "19", "20"]);
        assert_eq!((cm.cursor(), cm.position()), ((0, 6), (0, 13)));
        // a different width rewraps (live: DDEEEEEEEEEE and DONE in view, DDD found at 14,1)
        let long = "AAAAAAAAAABBBBBBBBBBCCCCCCCCCCDDDDDDDDDDEEEEEEEEEE";
        let src = vec![row(long, 60, false), row("DONE", 60, false), row("", 60, false)];
        let mut cm = CopyMode::clone_from(src, (60, 3), (2, 0), (19, 10), false, true);
        assert_eq!(texts(&cm)[..2], ["DDEEEEEEEEEE", "DONE"]);
        cm.run("history-top", &[]);
        cm.run("search-forward", &["DDD".into()]);
        assert_eq!((cm.cursor(), cm.search_count(), cm.cursor_line()), ((14, 1), Some(3), "BCCCCCCCCCCDDDDDDDD".to_string()));
        // a resize rewraps around the cursor and joins rows again when wider
        let src = vec![row(&long[..40], 40, true), row(&long[40..], 40, false), row("x", 40, false)];
        let mut cm = CopyMode::new(src, 40, 3, (0, 0), true);
        cm.run("cursor-right", &[]);
        cm.resize(13, 3);
        assert_eq!((cm.cursor(), cm.cursor_line()), ((1, 0), "AAAAAAAAAABBB".to_string()));
        cm.resize(60, 3);
        assert_eq!(texts(&cm)[0], long);
        assert_eq!(cm.cursor(), (1, 0));
        // grid_reflow counts a wide character's padding cell as a column: four CJK
        // characters to a 13-column row, as live tmux 3.8 shows
        let mut wide = Line { cells: Vec::new(), wrapped: false, ..Default::default() };
        for _ in 0..22 {
            wide.cells.push(Cell { text: "漢".into(), style: Style::default() });
            wide.cells.push(Cell { text: String::new(), style: Style::default() });
        }
        let first = Line { cells: wide.cells[..40].to_vec(), wrapped: true, ..Default::default() };
        let rest = Line { cells: wide.cells[40..].to_vec(), wrapped: false, ..Default::default() };
        let mut cm = CopyMode::new(vec![first, rest, line("DONE", false)], 40, 6, (2, 0), true);
        cm.resize(13, 6);
        let t: Vec<String> = cm.lines.iter().map(|l| l.cells.iter().map(|c| c.text.as_str()).collect::<String>().trim_end().to_string()).collect();
        assert_eq!(t[..7], ["漢漢漢漢", "漢漢漢漢", "漢漢漢漢", "漢漢漢漢", "漢漢漢漢", "漢漢", "DONE"]);
    }

    #[test]
    fn default_bindings_tables() {
        let vi = CopyMode::default_bindings(true);
        assert!(vi.contains(&("j", "send-keys -X cursor-down")));
        assert!(vi.contains(&("r", "send-keys -X refresh-now")));
        assert!(vi.contains(&("C-[", "send-keys -X clear-selection")));
        assert_eq!(vi.len(), 88);
        let emacs = CopyMode::default_bindings(false);
        assert!(emacs.contains(&("C-Space", "send-keys -X begin-selection")));
        assert!(emacs.contains(&("M-w", "send-keys -X copy-pipe-and-cancel")));
        assert!(emacs.contains(&("C-l", "send-keys -X recentre-top-bottom")));
        assert!(emacs.contains(&("L", "send-keys -X line-numbers-toggle")));
        assert_eq!(emacs.len(), 78);
        // every keyboard binding is understood by the engine and names a real command
        for (k, c) in vi.iter().chain(emacs) {
            match internal(c) {
                Some(Internal::Command(n)) => assert!(is_command(n), "{k}: {n}"),
                Some(Internal::Prompt(n)) => assert!(is_command(n), "{k}: {n}"),
                Some(_) => {}
                None => assert!(k.contains("Mouse") || k.contains("Wheel") || k.contains("Click"), "{k}: {c}"),
            }
        }
        assert_eq!(internal(vi[10].1), Some(Internal::Count(1)));
        assert_eq!(internal(vi[3].1), Some(Internal::CursorWord(false)));
    }

    fn text_of(l: &Line) -> String {
        l.cells.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn line_numbers_modes_and_offsets() {
        // 10 history lines + 5 on screen, cursor on the bottom row, scrolled up 2
        let mut cm = numbered();
        cm.set_line_number_option(LineNumbers::Absolute);
        keys(&mut cm, &["C-y", "C-y"]);
        assert_eq!((cm.scroll_offset(), cm.cy - cm.top), (2, 4));
        assert_eq!(cm.line_number_width(), 4, "3 digits at least, and a space");
        let (view, cur) = cm.render();
        assert!(text_of(&view[1]).starts_with(" 10 line9"), "{:?}", text_of(&view[1]));
        assert_eq!(cur, (4, 4), "the cursor is offset past the numbers");
        assert_eq!(cm.position(), (9, 15), "absolute: first line in view and line count");
        cm.set_line_number_option(LineNumbers::Relative);
        let (view, _) = cm.render();
        assert!(text_of(&view[0]).starts_with("  4 line8"));
        assert!(text_of(&view[4]).starts_with("  0 line12"));
        cm.set_line_number_option(LineNumbers::Hybrid);
        let (view, _) = cm.render();
        assert!(text_of(&view[4]).starts_with(" 13 line12"));
        assert!(text_of(&view[3]).starts_with("  1 line11"));
        // default numbering counts from the scroll position
        cm.set_line_number_option(LineNumbers::Default);
        let (view, _) = cm.render();
        assert!(text_of(&view[0]).starts_with("  2 line8"));
        assert!(text_of(&view[2]).starts_with("  0 line10"));
        assert_eq!(cm.position(), (2, 10));
        // screen columns map back to text columns
        assert_eq!((cm.cursor_offset(3), cm.cursor_unoffset(7), cm.cursor_unoffset(1)), (7, 3, 0));
        // the indicator is drawn after the numbers
        assert!(text_of(&cm.render().0[0]).ends_with("[2/10]"));
        // copy-mode entered with the mouse: none, though the option is on
        cm.set_line_numbers(false);
        assert!(!cm.line_numbers_active());
        assert_eq!(cm.render().1, (4, 0));
        // line-numbers-toggle shows them (in the default style) even with the option off
        cm.set_line_number_option(LineNumbers::Off);
        cm.run("line-numbers-toggle", &[]);
        assert_eq!(cm.line_number_mode(), LineNumbers::Default);
        cm.run("line-numbers-toggle", &[]);
        assert!(!cm.line_numbers_active());
        cm.run("line-numbers-on", &[]);
        assert!(cm.line_numbers_active());
        cm.run("line-numbers-off", &[]);
        assert!(!cm.line_numbers_active());
    }

    #[test]
    fn goto_line_with_absolute_numbers() {
        let mut cm = numbered();
        cm.run("goto-line", &["3".into()]);
        assert_eq!(cm.scroll_offset(), 3);
        cm.set_line_number_option(LineNumbers::Absolute);
        cm.run("goto-line", &["3".into()]);
        assert_eq!(cm.top, 2, "absolute line 3 is the third line");
        cm.run("goto-line", &["0".into()]);
        assert_eq!(cm.top, 0);
        cm.run("goto-line", &["999".into()]);
        assert_eq!(cm.scroll_offset(), 0);
    }

    #[test]
    fn styles_for_current_line_mark_and_matches() {
        let mut cm = CopyMode::new(lines(&["ab ab", "xab", "zz"]), W, 3, (2, 0), false);
        let red = Style { bg: Color::Idx(1), ..Style::default() };
        let mark = Style { fg: Color::Idx(2), bg: Color::Idx(3), attrs: 0, ..Style::default() };
        let st = RenderStyle { current_line: Some(red), mark, position_text: Some(String::new()), ..RenderStyle::default() };
        let (view, _) = cm.render_with(&st);
        assert!(view[2].cells.iter().all(|c| c.style.bg == red.bg), "current line");
        assert!(view[1].cells.iter().all(|c| c.style.bg == Color::Default));
        cm.run("cursor-up", &[]);
        cm.run("set-mark", &[]);
        cm.run("cursor-up", &[]);
        let (view, _) = cm.render_with(&st);
        // the mark line takes the mark style, its cell the reverse
        let mx = cm.mark.1;
        assert_eq!(mx, 0);
        assert_eq!((view[1].cells[2].style.fg, view[1].cells[2].style.bg), (mark.fg, mark.bg));
        assert_eq!((view[1].cells[mx].style.fg, view[1].cells[mx].style.bg), (mark.bg, mark.fg));
        cm.run("search-forward", &["ab".into()]);
        let (view, _) = cm.render_with(&st);
        // emacs leaves the cursor after the match, which is the current one
        assert_eq!(cm.cx, 2);
        assert_eq!(view[0].cells[0].style.bg, MAGENTA, "the match at the cursor");
        assert_eq!(view[0].cells[3].style.bg, CYAN);
        assert_eq!(view[1].cells[1].style.bg, CYAN, "matches are drawn over the mark line");
        assert_eq!(view[1].cells[0].style.bg, mark.fg, "the mark cell stays reversed");
        // jump-to-mark swaps the mark and the cursor
        let here = (cm.cy, cm.cx);
        cm.run("jump-to-mark", &[]);
        assert_eq!((cm.cy, cm.cx), (1, mx));
        assert_eq!(cm.mark, here);
    }

    #[test]
    fn position_format_overlays_only_its_cells() {
        let cm = CopyMode::new(lines(&["hello world", ""]), W, 2, (0, 0), false);
        let pos = Style { fg: Color::Idx(4), ..Style::default() };
        let st = RenderStyle { position: pos, position_text: Some("#[align=right]<#[fg=red]x>".into()), ..RenderStyle::default() };
        let (view, _) = cm.render_with(&st);
        assert_eq!(text_of(&view[0]), "hello world      <x>");
        assert_eq!(view[0].cells[17].style.fg, Color::Idx(4));
        assert_eq!(view[0].cells[18].style.fg, Color::Idx(1));
        assert_eq!(view[0].cells[16].style, Style::default());
        // a one line screen has no indicator (window_copy_write_line: rupper < rlower)
        let cm = CopyMode::new(lines(&["hello world"]), W, 1, (0, 0), false);
        assert_eq!(text_of(&cm.render_with(&st).0[0]).trim_end(), "hello world");
    }

    #[test]
    fn position_format_blanks_cut_wide_characters_and_narrow_gutters_keep_a_column() {
        let row0 = |cm: &CopyMode, fmt: &str| {
            let st = RenderStyle { position_text: Some(fmt.into()), ..RenderStyle::default() };
            cm.render_with(&st).0[0].cells.iter().map(|c| c.text.as_str()).collect::<String>()
        };
        let cm = CopyMode::new(lines(&["漢字漢字漢字漢字", "foo"]), 12, 4, (0, 0), true);
        // what is left of a wide character the format overwrites becomes a space
        assert_eq!(row0(&cm, "XYZ"), "XYZ 漢字漢字");
        assert_eq!(row0(&cm, "#[align=left]X"), "X 字漢字漢字");
        assert_eq!(row0(&cm, "#[align=left]AB#[align=centre]C#[align=right]Z"), "AB字漢C 漢 Z");
        // line numbers wider than the pane: the gutter loses its last column to the text
        let v: Vec<String> = (1..=1206).map(|i| format!("{}", (b'a' + (i % 26) as u8) as char)).collect();
        let v: Vec<&str> = v.iter().map(String::as_str).collect();
        let mut cm = CopyMode::new(lines(&v), 5, 5, (4, 0), true);
        cm.set_line_number_option(LineNumbers::Absolute);
        let (view, cursor) = cm.render_with(&RenderStyle { position_text: Some(String::new()), ..RenderStyle::default() });
        let rows: Vec<String> = view.iter().map(|l| l.cells.iter().map(|c| c.text.as_str()).collect()).collect();
        assert_eq!(rows[1], format!("1203{}", v[1202]));
        assert_eq!(cursor, (4, 4));
    }

    #[test]
    fn a_wide_cell_in_the_last_column_is_drawn_blank() {
        // its second half is off the screen (screen_write_combine): tmux draws it empty
        let nz = |n| std::num::NonZeroU16::new(n).unwrap();
        let mut p = vt100::Parser::new(nz(2), nz(25), 0);
        p.process(format!("{}\u{1F1F0}\u{1F1F7}", "x".repeat(24)).as_bytes());
        let cm = CopyMode::new(crate::cells::snapshot(p.screen()), 25, 2, (0, 0), true);
        let view = cm.render_with(&RenderStyle { position_text: Some(String::new()), ..RenderStyle::default() }).0;
        assert_eq!(view[0].cells.iter().map(|c| c.text.as_str()).collect::<String>(), "x".repeat(24) + " ");
    }

    #[test]
    fn vi_cursor_stays_on_text_and_rectangles_go_past_it() {
        let mut cm = CopyMode::new(lines(&["hello", "ab", "longer line"]), W, 3, (0, 0), true);
        keys(&mut cm, &["$"]);
        assert_eq!(cm.cx, 4);
        keys(&mut cm, &["l"]);
        assert_eq!(pos(&cm), (1, 0), "l at the last character wraps");
        // virtualedit=block: a rectangle selection may go past the end of the line
        let mut cm = CopyMode::new(lines(&["hello", "ab", "longer line"]), W, 3, (0, 0), true);
        keys(&mut cm, &["v", "Space", "$"]);
        assert_eq!(cm.cx, W as usize, "to the edge of the screen");
        keys(&mut cm, &["j"]);
        assert_eq!(pos(&cm), (1, W as usize), "kept past the end of a short line");
        assert_eq!(copied(cm.run("copy-selection", &[])), "hello\nab\n", "vi keeps the newline past the text");
        // leaving rectangle mode brings the cursor back within the line
        keys(&mut cm, &["v"]);
        assert_eq!(cm.cx, 1);
        // emacs may go one past the text
        let mut cm = CopyMode::new(lines(&["hello"]), W, 1, (0, 0), false);
        keys(&mut cm, &["C-e"]);
        assert_eq!(cm.cx, 5);
    }

    #[test]
    fn scrolling_with_a_selection_moves_only_the_view() {
        let mut cm = numbered();
        keys(&mut cm, &["k", "k", "Space", "k"]);
        let ends = cm.selection_ends();
        assert!(cm.selection_following());
        keys(&mut cm, &["C-y", "C-y"]);
        assert_eq!(cm.scroll_offset(), 2);
        assert_eq!(cm.selection_ends(), ends, "the selection stays where it was");
        assert!(!cm.selection_following(), "and no longer follows the cursor");
        let row = cm.cy - cm.top;
        keys(&mut cm, &["C-e"]);
        assert_eq!(cm.cy - cm.top, row, "the cursor keeps its row");
        // at the bottom with a selection, scrolling never leaves copy mode
        cm.set_exit_on_bottom(true);
        assert_eq!(keys(&mut cm, &["C-e", "C-e"]), Action::None);
        assert_eq!(cm.run("page-down", &[]), Action::None);
        assert_eq!(cm.run("scroll-down", &[]), Action::None);
        // other-end on a stopped selection makes its end follow the cursor again
        cm.run("other-end", &[]);
        assert!(cm.selection_following());
        assert_eq!(cm.selection_ends(), ends);
        cm.run("clear-selection", &[]);
        assert_eq!(cm.run("scroll-down", &[]), Action::Cancel, "nothing selected: -e exits");
        // stop-selection
        let mut cm = numbered();
        keys(&mut cm, &["Space", "k"]);
        cm.run("stop-selection", &[]);
        let ends = cm.selection_ends();
        keys(&mut cm, &["k", "k"]);
        assert_eq!(cm.selection_ends(), ends);
    }

    #[test]
    fn scroll_exit_commands() {
        let mut cm = numbered();
        cm.run("halfpage-up", &[]);
        assert_eq!(cm.run("scroll-exit-on", &[]), Action::None);
        assert_eq!(cm.run("halfpage-down", &[]), Action::Cancel);
        let mut cm = numbered();
        cm.set_exit_on_bottom(true);
        cm.run("scroll-exit-toggle", &[]);
        cm.run("page-up", &[]);
        assert_eq!(cm.run("page-down", &[]), Action::None);
        cm.run("scroll-exit-toggle", &[]);
        cm.run("page-up", &[]);
        assert_eq!(cm.run("page-down", &[]), Action::Cancel);
        let mut cm = numbered();
        cm.run("scroll-exit-on", &[]);
        cm.run("scroll-exit-off", &[]);
        cm.run("page-up", &[]);
        assert_eq!(cm.run("page-down", &[]), Action::None);
        assert_eq!(cm.run("page-down-and-cancel", &[]), Action::Cancel);
        // scroll-down at the bottom: nothing, or leave with -e
        let mut cm = numbered();
        assert_eq!(cm.run("scroll-down", &[]), Action::None);
        cm.set_exit_on_bottom(true);
        assert_eq!(cm.run("scroll-down", &[]), Action::Cancel);
        // copy-mode -d: a page down, leaving at the bottom with -e
        let mut cm = numbered();
        cm.run("page-up", &[]);
        cm.run("page-up", &[]);
        assert!(!cm.page_down_exit(true));
        assert!(cm.page_down_exit(true));
    }

    #[test]
    fn recentre_top_bottom_cycles() {
        let mut cm = numbered();
        keys(&mut cm, &["k", "k", "k", "k", "k", "k"]);
        assert_eq!((cm.cy, cm.cy - cm.top), (8, 0));
        cm.run("recentre-top-bottom", &[]);
        assert_eq!((cm.cy, cm.cy - cm.top), (8, 2), "middle");
        cm.run("recentre-top-bottom", &[]);
        assert_eq!((cm.cy, cm.cy - cm.top), (8, 0), "top");
        cm.run("recentre-top-bottom", &[]);
        assert_eq!((cm.cy, cm.cy - cm.top), (8, 4), "bottom");
        cm.run("recentre-top-bottom", &[]);
        assert_eq!((cm.cy, cm.cy - cm.top), (8, 2), "middle again");
        // a different line starts at the middle again
        keys(&mut cm, &["k"]);
        cm.run("recentre-top-bottom", &[]);
        assert_eq!(cm.cy - cm.top, 2);
        // emacs C-l
        let v: Vec<String> = (0..15).map(|i| format!("l{i}")).collect();
        let v: Vec<&str> = v.iter().map(String::as_str).collect();
        let mut cm = CopyMode::new(lines(&v), W, 5, (4, 0), false);
        cm.press("C-l");
        assert_eq!(cm.cy - cm.top, 4, "the view cannot scroll past the bottom");
        keys(&mut cm, &["Up", "Up", "Up", "Up", "Up", "Up"]);
        cm.press("C-l");
        assert_eq!(cm.cy - cm.top, 2);
    }

    #[test]
    fn unicode_whitespace_in_words() {
        // NBSP and EM SPACE separate words; a space in word-separators matches them
        let mut cm = CopyMode::new(lines(&["a\u{a0}b c\u{2003}d e"]), W, 1, (0, 0), true);
        cm.set_word_separators(" ");
        let mut stops = Vec::new();
        for _ in 0..4 {
            cm.press("w");
            stops.push(cm.cx);
        }
        assert_eq!(stops, vec![2, 4, 6, 8]);
        cm.press("b");
        assert_eq!(cm.cx, 6);
        let mut cm = CopyMode::new(lines(&["x\u{3000}yy"]), W, 1, (0, 3), false);
        cm.run("select-word", &[]);
        assert_eq!(copied(cm.run("copy-selection", &[])), "yy");
        assert!(cm.in_set((0, 1), " "));
        assert!(!cm.in_set((0, 1), "-"));
        // grid_in_set: the padding half of U+3000 is whitespace too (tmux 3.8 probe)
        assert!(cm.in_set((0, 2), " ") && !cm.in_set((0, 2), "-"));
        let text = "ab\u{3000}\u{3000}cd\u{a0}ef\u{2003}gh-ij  \u{3000}kl";
        for (vi, want) in [(true, [1, 7, 10, 13, 14, 16, 22]), (false, [2, 8, 11, 14, 15, 17, 23])] {
            // line() keeps W cells; this line is longer
            let mut l = Line { cells: Vec::new(), wrapped: false, ..Default::default() };
            for ch in text.chars() {
                l.cells.push(Cell { text: ch.to_string(), style: Style::default() });
                if ch.width() == Some(2) {
                    l.cells.push(Cell { text: String::new(), style: Style::default() });
                }
            }
            l.cells.resize(40, Cell::default());
            let mut cm = CopyMode::new(vec![l, line("DONE", false)], 40, 6, (0, 0), vi);
            cm.set_word_separators("!\"#$%&'()*+,-./:;<=>?@[\\]^`{|}~");
            cm.run("history-top", &[]);
            let stops: Vec<usize> = (0..7).map(|_| { cm.run("next-word-end", &[]); cm.cx }).collect();
            assert_eq!(stops, want, "vi={vi}");
        }
    }

    #[test]
    fn dragging_inside_a_selection_adjusts_it() {
        let mut cm = CopyMode::new(lines(&["0123456789"]), W, 1, (0, 0), false);
        cm.start_drag(0, 2);
        cm.drag_to(0, 7);
        cm.end_drag();
        assert_eq!(cm.selection_ends(), Some(((2, 0), (7, 0))));
        // near the end: the end follows, the start stays
        cm.start_drag(0, 6);
        cm.drag_to(0, 9);
        assert_eq!(cm.sel_bounds(), Some(((0, 2), (0, 9))));
        cm.end_drag();
        // near the start: the start follows
        cm.start_drag(0, 3);
        cm.drag_to(0, 1);
        assert_eq!(cm.sel_bounds(), Some(((0, 1), (0, 9))));
        cm.end_drag();
        // outside: a new selection
        cm.start_drag(0, 0);
        assert_eq!(cm.sel_bounds(), Some(((0, 0), (0, 0))));
        assert_eq!(cm.selection_mode(), SelMode::Char);
    }

    #[test]
    fn selection_modes() {
        let mut cm = CopyMode::new(lines(&["one two", "three"]), W, 2, (0, 5), true);
        assert_eq!(cm.selection_mode(), SelMode::Char);
        cm.run("select-word", &[]);
        assert_eq!(cm.selection_mode(), SelMode::Word);
        cm.run("cursor-down", &[]);
        cm.run("selection-mode", &["line".into()]);
        assert_eq!(cm.selection_mode(), SelMode::Line);
        assert_eq!(copied(cm.run("copy-selection", &[])), "one two\nthree\n");
        assert_eq!(cm.selection_mode(), SelMode::Char, "copying clears the selection mode");
        cm.run("selection-mode", &["w".into()]);
        assert_eq!(cm.selection_mode().name(), "word");
        cm.run("selection-mode", &[]);
        assert_eq!(cm.selection_mode().name(), "char");
    }

    #[test]
    fn selection_mode_line_keeps_vi_cursor_on_text() {
        for vi in [true, false] {
            let mut cm = CopyMode::new(lines(&["one two three", "ab"]), W, 2, (0, 1), vi);
            cm.run("begin-selection", &[]);
            cm.run("selection-mode", &["line".into()]);
            assert_eq!(cm.cx, if vi { 12 } else { 13 });
            assert_eq!(cm.selection_ends(), Some(((0, 0), (13, 0))));
            cm.run("stop-selection", &[]);
            assert_eq!(cm.selection_ends(), Some(((0, 0), (13, 0))));
            assert_eq!(copied(cm.run("copy-selection", &[])), if vi { "one two three\n" } else { "one two three" });
        }
    }

    #[test]
    fn other_end_preserves_the_fixed_line_endpoint() {
        let mut cm = CopyMode::new(lines(&["one two three", "ab"]), W, 2, (0, 1), true);
        cm.run("select-line", &[]);
        cm.run("selection-mode", &["char".into()]);
        cm.run("other-end", &[]);
        assert_eq!(pos(&cm), (0, 0));
        assert_eq!(cm.selection_ends(), Some(((0, 0), (13, 0))));
        assert_eq!(copied(cm.run("copy-selection-no-clear", &[])), "one two three\n");
        cm.run("stop-selection", &[]);
        cm.run("other-end", &[]);
        assert_eq!(pos(&cm), (0, 12));
        assert_eq!(cm.selection_ends(), Some(((0, 0), (12, 0))));
        assert_eq!(copied(cm.run("copy-selection", &[])), "one two three");
    }

    #[test]
    fn selection_mode_word_uses_reset_positions_on_motion() {
        for vi in [true, false] {
            let mut cm = CopyMode::new(lines(&["one two three", "ab", "longer fourth"]), W, 3, (0, 1), vi);
            cm.run("begin-selection", &[]);
            cm.run("selection-mode", &["word".into()]);
            assert_eq!(cm.selection_ends(), Some(((1, 0), (1, 0))), "changing mode leaves endpoints alone");
            cm.run("cursor-down", &[]);
            assert_eq!(cm.selection_ends(), Some(((0, 0), (1, 1))), "motion uses reset endpoints");
        }
    }

    #[test]
    fn copy_without_selection_copies_the_search_match() {
        for vi in [true, false] {
            let mut cm = CopyMode::new(lines(&["one two three"]), W, 1, (0, 0), vi);
            cm.run("search-forward", &["two".into()]);
            assert_eq!(copied(cm.run("copy-selection", &[])), "two");
            assert_eq!(cm.run("copy-selection", &[]), Action::None, "copy clears search marks");
        }
    }

    #[test]
    fn counts_from_digits_and_meta_digits() {
        let mut cm = numbered();
        keys(&mut cm, &["1", "0", "k"]);
        assert_eq!(cm.cy, 4);
        // emacs: M-digit starts a count, plain digits extend it, M-digit starts again
        let v: Vec<String> = (0..30).map(|i| format!("l{i}")).collect();
        let v: Vec<&str> = v.iter().map(String::as_str).collect();
        let mut cm = CopyMode::new(lines(&v), W, 5, (4, 0), false);
        keys(&mut cm, &["M-1", "2", "Up"]);
        assert_eq!(cm.cy, 17);
        keys(&mut cm, &["M-3", "M-2", "Up"]);
        assert_eq!(cm.cy, 15);
        // send-keys -N
        cm.set_count(4);
        cm.run("cursor-up", &[]);
        assert_eq!(cm.cy, 11);
        cm.run("cursor-up", &[]);
        assert_eq!(cm.cy, 10, "the count is used once");
        // a jump waits for its character with the count
        let mut cm = CopyMode::new(lines(&["a.b.c.d"]), W, 1, (0, 0), true);
        keys(&mut cm, &["2", "f"]);
        assert!(cm.waiting_for_key());
        keys(&mut cm, &["."]);
        assert_eq!(cm.cx, 3);
    }

    #[test]
    fn escape_bracket_and_cursor_word_bindings() {
        let mut cm = CopyMode::new(lines(&["foo.bar foo.bar"]), W, 1, (0, 0), true);
        keys(&mut cm, &["Space", "l"]);
        keys(&mut cm, &["C-["]);
        assert!(!cm.selection_active(), "vi C-[ clears the selection like Escape");
        let mut emacs = CopyMode::new(lines(&["x"]), W, 1, (0, 0), false);
        assert_eq!(emacs.press("C-["), Action::Cancel);
        // # and * search for the word under the cursor as a regex
        assert_eq!(cm.key("*"), Action::Run("search-forward".into(), vec!["#{copy_cursor_word}".into()], true));
        // refresh-now asks for the content again; r is bound to it
        assert_eq!(cm.press("r"), Action::Refresh);
        // tmux ignores unknown commands, refresh-from-pane is gone
        assert_eq!(cm.run("refresh-from-pane", &[]), Action::None);
        assert!(!is_command("refresh-from-pane"));
        assert!(command_allowed_readonly("cursor-down") && command_allowed_readonly("refresh-now"));
        assert!(!command_allowed_readonly("begin-selection") && !command_allowed_readonly("copy-selection"));
        assert!(!command_allowed_readonly("scroll-exit-on"));
    }

    #[test]
    fn refresh_follows_or_keeps_lines() {
        let mk = |n: usize| -> Vec<Line> { (0..n).map(|i| line(&format!("l{i}"), false)).collect() };
        let mut cm = CopyMode::new(mk(15), W, 5, (4, 0), true);
        cm.run("refresh-on", &[]);
        assert!(cm.refresh_active() && cm.refresh_ready());
        // the cursor on the bottom line follows new output
        cm.refresh(mk(18));
        assert_eq!((cm.scroll_offset(), cm.cy), (0, 17));
        // elsewhere the same lines stay in view
        keys(&mut cm, &["k", "C-y"]);
        let (top, row) = (cm.top, cm.cy - cm.top);
        cm.refresh(mk(25));
        assert_eq!((cm.top, cm.cy - cm.top), (top, row));
        // paused while selecting
        cm.run("begin-selection", &[]);
        assert!(!cm.refresh_ready());
        cm.run("refresh-toggle", &[]);
        assert!(!cm.refresh_active());
        // view mode and copies of other panes do not refresh
        cm.from_other = true;
        cm.run("refresh-on", &[]);
        assert!(!cm.refresh_active());
        assert_eq!(cm.run("refresh-now", &[]), Action::None);
    }

    #[test]
    fn scrollbar_slider_drag() {
        let mut cm = numbered();
        assert_eq!(cm.slider(), (4, 1));
        // dragging the slider to the top shows the top of the history
        assert!(!cm.scroll_to_slider(0, 0, false));
        assert_eq!(cm.scroll_offset(), 10);
        assert_eq!(cm.slider(), (0, 1));
        // and back to the bottom, leaving with -e
        assert!(cm.scroll_to_slider(0, 4, true));
        assert_eq!(cm.scroll_offset(), 0);
    }

    #[test]
    fn copy_variants() {
        let mut cm = CopyMode::new(lines(&["abc def"]), W, 1, (0, 0), false);
        keys(&mut cm, &["C-Space", "M-f"]);
        assert_eq!(cm.run("append-selection", &[]), Action::Copy { text: "abc".into(), cancel: false, append: true });
        keys(&mut cm, &["C-Space", "M-f"]);
        assert_eq!(cm.run("pipe-no-clear", &[]), Action::Pipe { text: " def".into(), cancel: false });
        assert!(cm.selection_active());
        assert_eq!(cm.run("pipe-and-cancel", &[]), Action::Pipe { text: " def".into(), cancel: true });
        assert!(!cm.selection_active());
        // cursor-down-and-cancel leaves at the bottom
        let mut cm = numbered();
        assert_eq!(cm.run("cursor-down-and-cancel", &[]), Action::Cancel);
        // cursor-centre-*
        cm.run("cursor-centre-vertical", &[]);
        assert_eq!(cm.cy - cm.top, 2);
        cm.run("cursor-centre-horizontal", &[]);
        assert_eq!(cm.cx, 5, "clamped to the line");
        // scroll-top / scroll-bottom
        cm.run("page-up", &[]);
        cm.run("scroll-top", &[]);
        assert_eq!(cm.cy - cm.top, 0);
        cm.run("scroll-bottom", &[]);
        assert_eq!(cm.cy - cm.top, 4);
    }

    #[test]
    fn search_spans_wrapped_rows_and_styles() {
        for vi in [true, false] {
            for command in ["search-forward", "search-backward", "search-forward-text", "search-backward-text",
                            "search-forward-incremental", "search-backward-incremental"] {
                let mut cm = CopyMode::new(vec![line("01234567890123456ABC", true),
                    line("DefghIJKL", false), line("hardAB", false), line("CDbreak", false)], W, 4, (0, 0), vi);
                cm.run(command, &[if command.ends_with("incremental") { "=ABCDefghIJKL".into() } else { "ABCDefghIJKL".into() }]);
                assert_eq!(cm.search_count(), Some(1), "{command}");
                assert_eq!(cm.search_match().as_deref(), Some("ABCDefghIJKL"), "{command}");
                assert_eq!((cm.cy, cm.cx), if !vi && command.contains("forward") { (1, 9) } else { (0, 17) });
                cm.show_position = false;
                let view = cm.render().0;
                assert_eq!(view[0].cells[17].style.bg, MAGENTA);
                assert_eq!(view[1].cells[8].style.bg, MAGENTA);
                cm.run("search-again", &[]);
                assert_eq!(cm.search_match().as_deref(), Some("ABCDefghIJKL"));
                cm.run("search-reverse", &[]);
                assert_eq!(cm.search_match().as_deref(), Some("ABCDefghIJKL"));
                cm.run("search-forward", &["ABCDbreak".into()]);
                assert!(!cm.search_present(), "hard breaks are not joined");
            }
        }
    }

    #[test]
    fn search_joined_regex_overlap_wide_cells_and_limit() {
        let mut cm = CopyMode::new(vec![line("01234567890123456界A", true),
            line("BCDE", false)], W, 2, (0, 0), true);
        cm.run("search-forward", &["界AB.*E".into()]);
        assert_eq!((cm.cy, cm.cx), (0, 17));
        assert_eq!(cm.search_match().as_deref(), Some("界ABCDE"));
        cm.run("search-backward", &[".*E".into()]);
        assert_eq!((cm.cy, cm.cx), (0, 0), "backward overlap returns the beginning of the mark");
        let mut cm = CopyMode::new((0..102).map(|_| line("xxxxxxxxxxxxxxxxxxxx", true)).collect::<Vec<_>>(), W, 2, (0, 0), true);
        assert!(cm.row_matches(0, "[x]{2001}").is_empty());
        assert_eq!(cm.row_matches(0, "[x]{2000}"), vec![(0, 2000)]);
        assert!(cm.row_matches(0, "^").is_empty());
        cm.search_regex = false;
        assert_eq!(cm.row_matches(0, &"x".repeat(2001)), vec![(0, 2001)], "literal searches have no regex join limit");
    }

    #[test]
    fn search_over_a_long_wrapped_line_is_linear() {
        // one 160000-cell line soft-wrapped over 2000 rows: each row is searched once
        let mut v: Vec<Line> = (0..2000).map(|_| line(&"x".repeat(80), true)).collect();
        v.push(line("DONE", false));
        let mut cm = CopyMode::new(v, 80, 24, (0, 0), true);
        cm.run("history-top", &[]);
        let started = std::time::Instant::now();
        cm.run("search-forward", &["Q.Q".into()]);
        cm.run("search-forward", &["QQQ".into()]);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
        assert!(!cm.search_present());
        cm.run("search-forward", &["DONE".into()]);
        assert_eq!(cm.cursor(), (0, 23));
        // a match on an earlier row of a wrapped line is found from its first row
        let mut cm = CopyMode::new(vec![line("0123ABCD", true), line("abcdEFGH", true), line("ijkl", false), line("DONE", false)], 8, 4, (3, 0), true);
        cm.run("history-top", &[]);
        cm.run("search-forward", &["EFGH".into()]);
        assert_eq!((cm.cursor(), cm.search_count()), ((4, 1), Some(1)));
        cm.run("search-backward", &["ABCD".into()]);
        assert_eq!(cm.cursor(), (4, 0));
    }

    #[test]
    fn search_marks_clip_to_the_viewport() {
        let mut rows = vec![line("Axxxxxxxxxxxxxxxxxxx", true)];
        rows.extend((0..8).map(|_| line("xxxxxxxxxxxxxxxxxxxx", true)));
        rows.push(line("xxxxxxxxxxxxxxxxxxxZ", false));
        rows.extend(lines(&["DONE", ""]));
        let mut cm = CopyMode::new(rows, W, 8, (0, 0), true);
        cm.run("history-top", &[]);
        cm.run("search-forward", &["A.*Z".into()]);
        assert_eq!(cm.search_match(), Some(format!("A{}", "x".repeat(159))));
        cm.run("scroll-down", &[]);
        assert_eq!(cm.search_match(), None, "marks whose start is above the view are absent");
        cm.show_position = false;
        assert_ne!(cm.render().0[0].cells[0].style.bg, MAGENTA);
        cm.run("history-top", &[]);
        cm.vi = false;
        cm.run("search-forward", &["A.*Z".into()]);
        assert_eq!(cm.cursor(), (0, 8), "emacs moves after the clipped visible mark without scrolling");
        assert_eq!(cm.search_match(), None);
    }

    #[test]
    fn search_budget_partial_rounding_and_timeout() {
        for (n, expected) in [(0, None), (10, Some(10)), (100, Some(100)), (1000, Some(1000))] {
            let mut cm = CopyMode::new(vec![line(&"x".repeat(n.min(20)), false)], W, 1, (0, 0), true);
            // Each row has 20 matches; retry counts are added to the interrupted pass.
            cm.lines = (0..(n / 20 + 1)).map(|_| line("xxxxxxxxxxxxxxxxxxxx", false)).collect::<Vec<_>>().into();
            cm.height = cm.lines.len();
            if n == 0 { cm.lines = lines(&["none"]).into(); cm.height = 1; }
            let mut calls = 0;
            cm.count_search_matches("x", false, || { calls += 1; if calls == 1 { 201 } else { 202 } });
            assert_eq!(cm.search_count(), expected);
            assert!(cm.search_count_partial());
        }
        let mut cm = CopyMode::new(lines(&["x", "x"]), W, 2, (0, 0), true);
        cm.count_search_matches("x", false, || 200);
        assert_eq!(cm.search_count(), Some(2));
        assert!(!cm.search_count_partial());
        let mut calls = 0;
        cm.count_search_matches("x", false, || { calls += 1; if calls == 1 { 201 } else { 10001 } });
        assert!(cm.search_timed_out(), "visible retry shares the original hard deadline");
        assert!(!cm.search_present());
        cm.search_timed_out = false;
        cm.count_search_matches("x", false, || 10001);
        assert!(cm.search_timed_out());
        assert_eq!(cm.search_count(), None);
        assert!(!cm.search_count_partial());
        cm.search = Some(("x".into(), true));
        cm.run("search-again", &[]);
        assert!(cm.search_timed_out(), "repeat is blocked until a new search");
        cm.run("search-forward", &["x".into()]);
        assert!(!cm.search_timed_out());
        assert!(cm.search_present());
    }

    #[test]
    fn search_counts_positions_and_wrapping() {
        // checked against tmux 3.8: "ab ab", "xab", "zz" from the top
        let mk = |vi: bool| CopyMode::new(lines(&["ab ab", "xab", "zz"]), W, 3, (0, 0), vi);
        let mut cm = mk(false);
        assert_eq!(cm.search_count(), Some(0), "0 before any search");
        cm.run("search-forward", &["ab".into()]);
        assert_eq!((pos(&cm), cm.search_count()), ((0, 2), Some(3)), "emacs: after the match");
        assert_eq!(cm.search_match().as_deref(), Some("ab"), "one position after the match");
        cm.run("search-again", &[]);
        assert_eq!(pos(&cm), (0, 5));
        cm.run("search-again", &[]);
        assert_eq!(pos(&cm), (1, 3));
        cm.run("search-again", &[]);
        assert_eq!(pos(&cm), (0, 2), "wrapped");
        cm.run("search-reverse", &[]);
        assert_eq!(pos(&cm), (0, 0));
        // the marks go with a motion, and searching for the same text again does not count
        cm.run("cursor-right", &[]);
        assert_eq!((cm.search_present(), cm.search_count()), (false, None));
        cm.run("search-again", &[]);
        assert_eq!((cm.search_present(), cm.search_count()), (true, None));
        // a failed search for new text leaves no marks
        cm.run("search-forward", &["zzz".into()]);
        assert_eq!((cm.search_present(), cm.search_count()), (false, None));
        // wrap-search off
        cm.set_wrap_search(false);
        cm.run("history-bottom", &[]);
        cm.run("end-of-line", &[]);
        let here = pos(&cm);
        cm.run("search-forward", &["ab".into()]);
        assert_eq!(pos(&cm), here);
        // vi: the cursor stays at the start of the match, the next search starts after it
        let mut cm = mk(true);
        cm.run("search-forward", &["ab".into()]);
        assert_eq!(pos(&cm), (0, 3));
        cm.run("search-again", &[]);
        assert_eq!(pos(&cm), (1, 1));
        // the pane's last search is repeated upwards
        let mut cm = mk(true);
        cm.set_pane_search("zz");
        cm.run("search-again", &[]);
        assert_eq!(pos(&cm), (2, 0));
        assert_eq!(cm.search_count(), Some(1), "the first search counts");
    }

    #[test]
    fn selection_ends_follow_the_dragged_end() {
        // checked against tmux 3.8 (selection_start_* / selection_end_*)
        let text = ["one two three", "four five six", "seven"];
        for vi in [false, true] {
            let mut cm = CopyMode::new(lines(&text), W, 3, (0, 2), vi);
            cm.run("begin-selection", &[]);
            cm.set_count(3);
            cm.run("cursor-right", &[]);
            cm.run("cursor-down", &[]);
            assert_eq!(cm.selection_ends(), Some(((2, 0), (5, 1))));
            // other-end: the cursor drags the start, the ends stay put
            cm.run("other-end", &[]);
            assert_eq!(pos(&cm), (0, 2));
            assert_eq!(cm.selection_ends(), Some(((2, 0), (5, 1))));
            cm.run("cursor-left", &[]);
            assert_eq!(cm.selection_ends(), Some(((1, 0), (5, 1))));
            cm.run("other-end", &[]);
            cm.run("cursor-up", &[]);
            assert_eq!(cm.selection_ends(), Some(((1, 0), (5, 0))));
            // a line selection runs from the start of its first line to the end of its last
            cm.run("select-line", &[]);
            assert_eq!(cm.selection_ends(), Some(((0, 0), (13, 0))));
            assert_eq!(cm.cx, if vi { 12 } else { 13 });
            cm.run("other-end", &[]);
            assert_eq!((pos(&cm), cm.selection_mode()), ((0, 0), SelMode::Char));
            assert_eq!(cm.selection_ends(), Some(((0, 0), (13, 0))));
            // moving down turns round at the anchor line
            cm.run("cursor-down", &[]);
            let x = if vi { 12 } else { 13 };
            assert_eq!(pos(&cm), (1, x));
            assert_eq!(cm.selection_ends(), Some(((0, 0), (x, 1))));
        }
        // dragging the start of a selection reports the mouse's end as the start
        let mut cm = CopyMode::new(lines(&text), W, 3, (0, 0), false);
        cm.start_drag(0, 2);
        cm.drag_to(0, 8);
        cm.end_drag();
        cm.start_drag(0, 3);
        cm.drag_to(0, 1);
        cm.end_drag();
        assert_eq!(cm.selection_ends(), Some(((1, 0), (8, 0))));
    }

    #[test]
    fn line_selection_cursor_sides() {
        // checked against tmux 3.8: the cursor is at the end of lines below the anchor
        // line and at the start above it
        let text = ["l0 aaaa", "l1 bb", "l2 cccccc", "l3 d", "l4 eeeee", "l5"];
        for vi in [false, true] {
            let e = |x: usize| if vi { x - 1 } else { x };
            let mut cm = CopyMode::new(lines(&text), W, 6, (2, 2), vi);
            cm.run("select-line", &[]);
            let mut seen = vec![(pos(&cm), cm.selection_ends())];
            for c in ["cursor-down", "cursor-up", "cursor-up", "cursor-up", "cursor-down", "cursor-down", "cursor-down"] {
                cm.run(c, &[]);
                seen.push((pos(&cm), cm.selection_ends()));
            }
            let want = vec![
                ((2, e(9)), Some(((0, 2), (9, 2)))),
                ((3, e(4)), Some(((0, 2), (4, 3)))),
                ((2, e(9)), Some(((0, 2), (9, 2)))),
                ((1, 0), Some(((0, 1), (9, 2)))),
                ((0, 0), Some(((0, 0), (9, 2)))),
                ((1, 0), Some(((0, 1), (9, 2)))),
                ((2, 0), Some(((0, 2), (9, 2)))),
                ((3, e(4)), Some(((0, 2), (4, 3)))),
            ];
            assert_eq!(seen, want, "vi {vi}");
            cm.run("other-end", &[]);
            cm.run("cursor-up", &[]);
            assert_eq!(copied(cm.run("copy-selection-no-clear", &[])), if vi { "l1 bb\nl2 cccccc\nl3 d\n" } else { "l1 bb\nl2 cccccc\nl3 d" });
        }
    }
}

#[cfg(test)]
mod display_search_tests {
    use super::*;
    #[test]
    fn emacs_search_ending_in_the_last_cell_keeps_the_view() {
        // emacs search-forward leaves the cursor after the match; after the last cell of
        // the grid that is the first cell (window_copy_move_after_search_mark wraps with
        // window_copy_move_right). tmux 3.8 leaves the view on the match, so with history
        // its cursor is above the view (tmux 3.4 live: copy_cursor_y -4). Here the cursor
        // stays on the last cell and searches go on from the first cell like tmux's.
        let row = |s: &str| Line { cells: s.chars().map(|c| crate::cells::Cell { text: c.to_string(), style: Default::default() }).collect(), wrapped: false, ..Default::default() };
        let lines: Vec<Line> = ["x1", "l2", "l3", "l4", "l5", "l6", "l7", "l8", "abx"].into_iter().map(row).collect();
        let mut cm = CopyMode::new(lines.clone(), 3, 5, (4, 2), false);
        assert_eq!((cm.cy, cm.cx, cm.top), (8, 2, 4));
        cm.run("search-forward", &["x".into()]);
        assert_eq!((cm.cy, cm.cx, cm.top, cm.cursor()), (8, 2, 4, (2, 4)));
        // the match stays in view; tmux's cursor is not on it, so it is not the current
        // match and search_match is empty (window_copy_search_mark_at fails above the view)
        let (view, _) = cm.render();
        assert_eq!(view[4].cells[2].style.bg, CYAN);
        assert_eq!(cm.search_match(), None);
        // search-again goes on from the first cell: the match there, the view follows
        cm.run("search-again", &[]);
        assert_eq!((cm.cy, cm.cx, cm.top), (0, 1, 0));
        // backward from the first cell wraps to the last match
        let mut cm = CopyMode::new(lines.clone(), 3, 5, (4, 2), false);
        cm.run("search-forward", &["x".into()]);
        cm.run("search-reverse", &[]);
        assert_eq!((cm.cy, cm.cx, cm.top), (8, 2, 4));
        // any other command moves on from the cursor as drawn
        let mut cm = CopyMode::new(lines.clone(), 3, 5, (4, 2), false);
        cm.run("search-forward", &["x".into()]);
        cm.run("cursor-left", &[]);
        cm.run("cursor-right", &[]);
        cm.run("search-again", &[]);
        assert_eq!((cm.cy, cm.cx, cm.top), (8, 2, 4));
        // without wrap-search the cursor stays on the match's last cell (tmux too) and
        // searches go on from there
        let mut cm = CopyMode::new(lines, 3, 5, (4, 2), false);
        cm.wrap_search = false;
        cm.run("search-forward", &["x".into()]);
        assert_eq!((cm.cy, cm.cx, cm.top), (8, 2, 4));
        assert!(!cm.search_wrapped);
        // with no history the first cell is in view and the cursor goes there as in tmux
        let lines: Vec<Line> = ["x1", "l2", "l3", "l4", "abx"].into_iter().map(row).collect();
        let mut cm = CopyMode::new(lines, 3, 5, (4, 2), false);
        assert_eq!((cm.cy, cm.cx, cm.top), (4, 2, 0));
        cm.run("search-forward", &["x".into()]);
        assert_eq!((cm.cy, cm.cx, cm.top, cm.cursor()), (0, 0, 0, (0, 0)));
        cm.run("search-again", &[]);
        assert_eq!((cm.cy, cm.cx, cm.top), (0, 1, 0));
    }

    #[test]
    fn wrap_search_can_be_disabled() {
        let lines: Vec<Line> = ["target", "middle", "target"].into_iter().map(|s| Line { cells: s.chars().map(|c| crate::cells::Cell { text: c.to_string(), style: Default::default() }).collect(), wrapped: false, ..Default::default() }).collect();
        let mut cm = CopyMode::new(lines, 10, 3, (2, 0), false);
        cm.wrap_search = false;
        cm.run("search-forward", &["target".into()]);
        assert_eq!(cm.cy, 2);
        cm.wrap_search = true;
        cm.run("search-forward", &["target".into()]);
        assert_eq!(cm.cy, 0);
        cm.wrap_search = false;
        cm.run("search-backward", &["target".into()]);
        assert_eq!(cm.cy, 0);
    }
}

#[cfg(test)]
mod frozen_backing_tests {
    use super::*;
    use crate::cells::{random_chunk, random_parser, snapshot, TestRng};

    /// Everything a copy-mode command can show or give the server. The match count is left
    /// out with `count` false: counting stops on a clock (200 ms), which a loaded machine
    /// can reach in one copy and not the other. That leaves it out of the drawn position
    /// indicator too ("(1000+ results)" in one copy, "(1234 results)" or nothing in the
    /// other): the screen is drawn with the indicator's position part only.
    fn state(cm: &CopyMode, action: Action, count: bool) -> String {
        let (pos, limit) = cm.position();
        let style = RenderStyle { position_text: (!count).then(|| format!("#[align=right][{pos}/{limit}]")), ..RenderStyle::default() };
        format!(
            "{action:?} {:?} {} {} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {}",
            cm.cursor(), cm.view_top(), cm.line_count(), cm.position(), cm.selection_ends(), cm.search_count().filter(|_| count),
            cm.search_match(), cm.cursor_word(), cm.cursor_line(), cm.render_with(&style), cm.top_line_time(),
        )
    }

    /// The states of two copies, which must be the same.
    fn states(a: &CopyMode, ra: Action, b: &CopyMode, rb: Action) -> (String, String) {
        let count = ![a, b].iter().any(|c| c.search_count_partial() || c.search_timed_out());
        (state(a, ra, count), state(b, rb, count))
    }

    const COMMANDS: &[&str] = &[
        "cursor-up", "cursor-down", "cursor-left", "cursor-right", "start-of-line", "end-of-line", "back-to-indentation",
        "next-word", "next-space", "next-word-end", "previous-word", "previous-space", "next-matching-bracket",
        "previous-matching-bracket", "top-line", "middle-line", "bottom-line", "history-top", "history-bottom",
        "page-up", "halfpage-up", "page-down", "halfpage-down", "scroll-up", "scroll-down", "scroll-top",
        "scroll-middle", "begin-selection", "stop-selection", "select-line", "select-word", "rectangle-toggle",
        "clear-selection", "other-end", "copy-selection-no-clear", "copy-line", "copy-end-of-line", "search-again",
        "search-reverse", "jump-again", "next-prompt", "previous-prompt", "next-prompt -o", "previous-paragraph",
        "next-paragraph", "set-mark", "jump-to-mark", "search-forward x", "search-backward ab", "search-forward-text a",
        "search-backward 글", "search-forward [a-z]+\\(", "jump-forward x", "jump-backward a", "goto-line 3",
        "line-numbers-toggle", "refresh-now",
    ];

    #[test]
    fn lazy_lines_from_a_frozen_screen_act_like_a_snapshot() {
        // the server used to give copy mode cells::snapshot of the pane; it now gives the
        // screen itself, read a row at a time: every command must see the same lines
        for seed in 0..300 {
            let mut p = random_parser(seed);
            let mut rng = TestRng(seed * 7919 + 13);
            let (rows, cols) = p.screen().size();
            let (rows, cols) = (rows.get(), cols.get());
            let (cy, cx) = p.screen().cursor_position();
            let other = rng.below(4) == 0;
            let size = if other { (cols + rng.below(5) as u16, rows + 1) } else { (cols, rows) };
            let vi = rng.below(2) == 0;
            let mut a = CopyMode::clone_from(snapshot(p.screen()), (cols, rows), (cy, cx), size, vi, other);
            let mut b = CopyMode::clone_from(Backing::frozen(p.screen().clone()), (cols, rows), (cy, cx), size, vi, other);
            let (sa, sb) = states(&a, Action::None, &b, Action::None);
            assert_eq!(sa, sb, "seed {seed} entry");
            for step in 0..150 {
                let (ra, rb) = match rng.below(20) {
                    0 => {
                        // more output, then window_copy_do_refresh
                        for _ in 0..rng.below(30) {
                            random_chunk(&mut rng, &mut p);
                        }
                        a.refresh(snapshot(p.screen()));
                        b.refresh(Backing::frozen(p.screen().clone()));
                        (Action::None, Action::None)
                    }
                    1 => {
                        let (w, h) = (2 + rng.below(30) as u16, 1 + rng.below(8) as u16);
                        let (w, h) = if rng.below(2) == 0 { (a.width as u16, h) } else { (w, h) };
                        a.resize(w, h);
                        b.resize(w, h);
                        (Action::None, Action::None)
                    }
                    2 => {
                        let (r, c) = (rng.below(10) as u16, rng.below(40) as u16);
                        a.move_to_view(r, c);
                        b.move_to_view(r, c);
                        (Action::None, Action::None)
                    }
                    3 => {
                        let up = rng.below(2) == 0;
                        a.scroll_line(up);
                        b.scroll_line(up);
                        (Action::None, Action::None)
                    }
                    _ => {
                        let mut words = COMMANDS[rng.below(COMMANDS.len())].splitn(2, ' ');
                        let command = words.next().unwrap();
                        let args: Vec<String> = words.next().map(String::from).into_iter().collect();
                        if rng.below(4) == 0 {
                            let n = 1 + rng.below(40);
                            a.set_count(n);
                            b.set_count(n);
                        }
                        (a.run(command, &args), b.run(command, &args))
                    }
                };
                let (sa, sb) = states(&a, ra, &b, rb);
                assert_eq!(sa, sb, "seed {seed} step {step}");
            }
            assert_eq!(a.backing_screen().0.to_vec(), b.backing_screen().0.to_vec(), "seed {seed}");
            assert_eq!(a.backing_screen().1, b.backing_screen().1, "seed {seed}");
        }
    }

    #[test]
    fn a_frozen_screen_keeps_few_lines() {
        // only the rows read are built, and the ones kept stay near the view
        let mut p = vt100::Parser::new(std::num::NonZeroU16::new(10).unwrap(), std::num::NonZeroU16::new(20).unwrap(), 5000);
        for i in 0..5000 {
            p.process(format!("line {i}\r\n").as_bytes());
        }
        let mut cm = CopyMode::new(Backing::frozen(p.screen().clone()), 20, 10, (9, 0), true);
        let built = |cm: &CopyMode| cm.lines.frozen.as_ref().map_or(0, |f| f.cache.iter().filter(|c| c.get().is_some()).count());
        let _ = cm.render();
        assert!(built(&cm) <= 10);
        for _ in 0..400 {
            cm.run("page-up", &[]);
            let _ = cm.render();
        }
        assert!(built(&cm) <= 3 * 10 + 64 + 10, "{} lines kept", built(&cm));
        cm.run("search-forward", &["line 7".into()]);
        cm.run("search-backward", &["nothing".into()]);
        assert!(built(&cm) <= 3 * 10 + 64 + 10, "{} lines kept", built(&cm));
        let frozen = cm.lines.frozen.as_ref().unwrap();
        assert_eq!(frozen.cached.borrow().len(), built(&cm), "the list of built rows is exact");
    }

    #[test]
    fn commands_over_the_whole_history_build_no_lines() {
        // motions and copies that cross the history read the frozen rows' cells and keep no
        // lines while they run (the cache is only trimmed after the command)
        let mut p = vt100::Parser::new(std::num::NonZeroU16::new(10).unwrap(), std::num::NonZeroU16::new(20).unwrap(), 9000);
        p.process(b"first a\tb \xed\x95\x9c\r\n");
        (0..6000).for_each(|_| p.process(b"\r\n"));
        (0..1000).for_each(|i| p.process(format!("w{i} [x] y\r\n").as_bytes()));
        p.process(b"last");
        let mut cm = CopyMode::new(Backing::frozen(p.screen().clone()), 20, 10, (9, 4), true);
        let built = |cm: &CopyMode| cm.lines.frozen.as_ref().map_or(0, |f| f.cache.iter().filter(|c| c.get().is_some()).count());
        let mut steps: Vec<(&str, usize, &str)> = vec![
            ("history-top", 1, ""), ("next-word", 6000, ""), ("history-top", 1, ""), ("next-space", 3000, ""),
            ("history-top", 1, ""), ("next-word-end", 6000, ""), ("history-bottom", 1, ""), ("previous-word", 6000, ""),
            ("history-bottom", 1, ""), ("previous-space", 3000, ""), ("history-top", 1, ""), ("next-paragraph", 3, ""),
            ("history-bottom", 1, ""), ("previous-paragraph", 3, ""), ("history-top", 1, ""), ("jump-forward", 1, "b"),
        ];
        steps.extend([("history-top", 1, ""), ("begin-selection", 1, ""), ("history-bottom", 1, ""), ("end-of-line", 1, "")]);
        for (command, n, arg) in steps {
            let before = built(&cm);
            cm.set_count(n);
            let args: Vec<String> = if arg.is_empty() { vec![] } else { vec![arg.into()] };
            let _ = cm.run_inner(command, &args);
            assert!(built(&cm) <= before + 64, "{command}: {} lines kept", built(&cm));
            cm.trim_cache();
        }
        let before = built(&cm);
        let Action::Copy { text, .. } = cm.run_inner("copy-selection-no-clear", &[]) else { panic!("no copy") };
        assert!(built(&cm) <= before + 64, "copy: {} lines kept", built(&cm));
        assert!(text.starts_with("first a\tb \u{d55c}\n\n") && text.ends_with("w999 [x] y\nlast"), "{text:?}");
        assert_eq!(text.lines().count(), 7002);
        cm.run_inner("rectangle-toggle", &[]);
        let Action::Copy { text, .. } = cm.run_inner("copy-selection-no-clear", &[]) else { panic!("no copy") };
        assert!(built(&cm) <= before + 64, "rectangle copy: {} lines kept", built(&cm));
        assert_eq!(text.lines().count(), 7002);
    }
}
