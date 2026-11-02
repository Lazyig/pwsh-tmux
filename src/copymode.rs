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
    /// Put text into a paste buffer; leave copy mode if `cancel`.
    Copy { text: String, cancel: bool },
    /// Caller asks the user for a string, then calls `run(command, &[input])`.
    Prompt { label: String, command: String },
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

type Pos = (usize, usize);
type SearchOrigin = (Pos, usize, Option<(String, bool)>);

/// tmux WHITESPACE.
const WHITESPACE: &str = "\t ";

/// tmux's default `word-separators` (tmux 3.0 - 3.2).
pub const DEFAULT_WORD_SEPARATORS: &str = " -_@";

const BLACK: Color = Color::Idx(0);
const MAGENTA: Color = Color::Idx(5);
const CYAN: Color = Color::Idx(6);
const YELLOW: Color = Color::Idx(3);

pub struct CopyMode {
    lines: Vec<Line>,
    width: usize,
    height: usize,
    /// Absolute index of the first displayed line.
    top: usize,
    cy: usize,
    cx: usize,
    /// Desired column for vertical motion.
    lastcx: usize,
    vi: bool,
    /// Selection anchor; the other end is the cursor.
    anchor: Option<Pos>,
    line_mode: bool,
    rect: bool,
    /// Last search: (text, forward).
    search: Option<(String, bool)>,
    incremental_origin: Option<SearchOrigin>,
    search_cache: std::cell::RefCell<(String, Option<fancy_regex::Regex>)>,
    /// Last jump: (kind, character).
    jump: Option<(JumpKind, String)>,
    /// A jump command waiting for its character argument.
    pending_jump: Option<JumpKind>,
    count: Option<usize>,
    separators: String,
    mark: Option<Pos>,
    show_position: bool,
    search_marks: bool,
    exit_on_bottom: bool,
    /// tmux's lastcx / lastsx: the column to return to and the length of the line it was on,
    /// for vertical motion and scrolling (both start at 0).
    tlastcx: usize,
    tlastsx: usize,
}

impl CopyMode {
    /// `lines`: snapshot (history + screen). The view initially shows the bottom `height`
    /// lines; `cursor` is (row within that bottom view, col).
    pub fn new(lines: Vec<Line>, width: u16, height: u16, cursor: (u16, u16), vi: bool) -> Self {
        let mut cm = CopyMode {
            lines,
            width: (width as usize).max(1),
            height: (height as usize).max(1),
            top: 0,
            cy: 0,
            cx: 0,
            lastcx: 0,
            vi,
            anchor: None,
            line_mode: false,
            rect: false,
            search: None,
            incremental_origin: None,
            search_cache: std::cell::RefCell::new((String::new(), None)),
            jump: None,
            pending_jump: None,
            count: None,
            separators: DEFAULT_WORD_SEPARATORS.to_string(),
            mark: None,
            show_position: true,
            search_marks: false,
            exit_on_bottom: false,
            tlastcx: 0,
            tlastsx: 0,
        };
        cm.pad();
        cm.top = cm.hsize();
        let row = (cursor.0 as usize).min(cm.height - 1);
        cm.cy = cm.top + row;
        cm.cx = cm.snap(cm.cy, cursor.1 as usize);
        cm.lastcx = cm.cx;
        cm
    }

    /// `copy-mode -e`: leave copy mode once scrolled back to the bottom.
    pub fn set_exit_on_bottom(&mut self, on: bool) {
        self.exit_on_bottom = on;
    }

    /// `copy-mode -H`: hide the position indicator.
    pub fn set_show_position(&mut self, on: bool) {
        self.show_position = on;
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

    /// Absolute index of the first line in view.
    pub fn view_top(&self) -> usize {
        self.top
    }

    /// window_copy_start_drag for a character selection: select from this cell.
    pub fn start_drag(&mut self, row: u16, col: u16) {
        self.move_to_view(row, col);
        self.line_mode = false;
        self.anchor = Some((self.cy, self.cx));
    }

    /// The other end of a mouse selection moves here.
    pub fn drag_to(&mut self, row: u16, col: u16) {
        self.move_to_view(row, col);
    }

    /// `copy-mode -d`: a page down; true when that should leave copy mode (with -e).
    pub fn page_down_exit(&mut self, exit: bool) -> bool {
        self.page_down(self.height);
        exit && self.scroll_offset() == 0
    }

    /// window_copy_cursor_up/down(scroll_only): scroll the view one line while the cursor
    /// keeps its row on the screen, sticking to the end of lines as tmux does.
    pub fn scroll_line(&mut self, up: bool) {
        let row = self.cy - self.top;
        let norect = self.anchor.is_none() || !self.rect;
        self.sticky_begin(norect);
        if up {
            self.top = self.top.saturating_sub(1);
        } else {
            self.top = (self.top + 1).min(self.hsize());
        }
        self.cy = (self.top + row).min(self.lines.len() - 1);
        self.sticky_end(norect);
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

    /// After a vertical move: a cursor that was at a line end stays at the line end.
    fn sticky_end(&mut self, norect: bool) {
        if norect {
            let px = self.len(self.cy);
            if (self.cx >= self.tlastsx && self.cx != px) || self.cx > px {
                self.cx = px;
            }
        }
        self.cx = self.snap(self.cy, self.cx);
    }

    /// Moves the cursor to a cell of the current view (mouse clicks and drags).
    pub fn move_to_view(&mut self, row: u16, col: u16) {
        let row = (row as usize).min(self.height - 1);
        self.cy = (self.top + row).min(self.lines.len().saturating_sub(1));
        self.cx = self.snap(self.cy, col as usize);
        self.lastcx = self.cx;
    }

    pub fn set_word_separators(&mut self, s: &str) {
        self.separators = s.to_string();
    }

    pub fn scroll_offset(&self) -> usize {
        self.hsize() - self.top
    }

    pub fn selection_active(&self) -> bool {
        self.anchor.is_some()
    }

    /// selection_start_* / selection_end_*: the anchor and the cursor as (x, y), y counted
    /// from the oldest history line.
    pub fn selection_ends(&self) -> Option<((usize, usize), (usize, usize))> {
        self.anchor.map(|(ay, ax)| ((ax, ay), (self.cx, self.cy)))
    }

    pub fn begin_incremental_search(&mut self) {
        self.incremental_origin = Some(((self.cy, self.cx), self.top, self.search.clone()));
    }

    pub fn preview_search(&mut self, text: &str, forward: bool) {
        let Some((pos, top, search)) = self.incremental_origin.clone() else { return };
        self.set_cursor(pos.0, pos.1);
        self.top = top;
        self.search = search;
        if !text.is_empty() {
            self.search_marks = true;
            self.search = Some((text.into(), forward));
            self.do_search(text, forward, 1);
        }
    }

    pub fn finish_incremental_search(&mut self, cancel: bool) {
        if cancel { self.preview_search("", true); }
        self.incremental_origin = None;
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = (width as usize).max(1);
        self.height = (height as usize).max(1);
        self.pad();
        self.top = self.top.min(self.hsize());
        self.cy = self.cy.min(self.lines.len() - 1);
        self.cx = self.snap(self.cy, self.cx);
        self.ensure_visible();
    }

    /// Replace the content keeping the position relative to the bottom.
    pub fn set_lines(&mut self, lines: Vec<Line>) {
        let old = self.lines.len();
        self.lines = lines;
        self.pad();
        let new = self.lines.len();
        let remap = |y: usize| (new + y).saturating_sub(old).min(new - 1);
        self.top = remap(self.top).min(self.hsize());
        self.cy = remap(self.cy);
        self.anchor = self.anchor.map(|(y, x)| (remap(y), x));
        self.mark = self.mark.map(|(y, x)| (remap(y), x));
        self.cx = self.snap(self.cy, self.cx);
        self.ensure_visible();
    }

    // ----------------------------------------------------------------- bindings

    /// Default bindings for the copy-mode-vi (`vi`) or copy-mode table.
    pub fn default_bindings(vi: bool) -> Vec<(&'static str, &'static str)> {
        if vi {
            vec![
                ("C-b", "page-up"),
                ("C-c", "cancel"),
                ("C-d", "halfpage-down"),
                ("C-e", "scroll-down"),
                ("C-f", "page-down"),
                ("C-h", "cursor-left"),
                ("C-j", "copy-selection-and-cancel"),
                ("Enter", "copy-selection-and-cancel"),
                ("C-u", "halfpage-up"),
                ("C-v", "rectangle-toggle"),
                ("C-y", "scroll-up"),
                ("Escape", "clear-selection"),
                ("Space", "begin-selection"),
                ("#", "search-backward-text"),
                ("%", "next-matching-bracket"),
                ("$", "end-of-line"),
                ("*", "search-forward-text"),
                (",", "jump-reverse"),
                ("/", "search-forward-prompt"),
                ("0", "start-of-line"),
                (":", "goto-line-prompt"),
                (";", "jump-again"),
                ("?", "search-backward-prompt"),
                ("A", "append-selection-and-cancel"),
                ("B", "previous-space"),
                ("D", "copy-end-of-line-and-cancel"),
                ("E", "next-space-end"),
                ("F", "jump-backward-prompt"),
                ("G", "history-bottom"),
                ("H", "top-line"),
                ("J", "scroll-down"),
                ("K", "scroll-up"),
                ("L", "bottom-line"),
                ("M", "middle-line"),
                ("N", "search-reverse"),
                ("P", "toggle-position"),
                ("T", "jump-to-backward-prompt"),
                ("V", "select-line"),
                ("W", "next-space"),
                ("X", "set-mark"),
                ("^", "back-to-indentation"),
                ("b", "previous-word"),
                ("e", "next-word-end"),
                ("f", "jump-forward-prompt"),
                ("g", "history-top"),
                ("h", "cursor-left"),
                ("j", "cursor-down"),
                ("k", "cursor-up"),
                ("l", "cursor-right"),
                ("n", "search-again"),
                ("o", "other-end"),
                ("q", "cancel"),
                ("r", "refresh-from-pane"),
                ("t", "jump-to-forward-prompt"),
                ("v", "rectangle-toggle"),
                ("w", "next-word"),
                ("z", "scroll-middle"),
                ("{", "previous-paragraph"),
                ("}", "next-paragraph"),
                ("M-x", "jump-to-mark"),
                ("BSpace", "cursor-left"),
                ("C-Down", "scroll-down"),
                ("C-Up", "scroll-up"),
                ("Down", "cursor-down"),
                ("End", "end-of-line"),
                ("Home", "start-of-line"),
                ("Left", "cursor-left"),
                ("NPage", "page-down"),
                ("PPage", "page-up"),
                ("Right", "cursor-right"),
                ("Up", "cursor-up"),
            ]
        } else {
            vec![
                ("C-Space", "begin-selection"),
                ("C-a", "start-of-line"),
                ("C-b", "cursor-left"),
                ("C-c", "cancel"),
                ("C-e", "end-of-line"),
                ("C-f", "cursor-right"),
                ("C-g", "clear-selection"),
                ("C-k", "copy-end-of-line-and-cancel"),
                ("C-n", "cursor-down"),
                ("C-p", "cursor-up"),
                ("C-r", "search-backward-incremental-prompt"),
                ("C-s", "search-forward-incremental-prompt"),
                ("C-v", "page-down"),
                ("C-w", "copy-selection-and-cancel"),
                ("Escape", "cancel"),
                ("Space", "page-down"),
                (",", "jump-reverse"),
                (";", "jump-again"),
                ("F", "jump-backward-prompt"),
                ("N", "search-reverse"),
                ("P", "toggle-position"),
                ("R", "rectangle-toggle"),
                ("T", "jump-to-backward-prompt"),
                ("X", "set-mark"),
                ("f", "jump-forward-prompt"),
                ("g", "goto-line-prompt"),
                ("n", "search-again"),
                ("q", "cancel"),
                ("r", "refresh-from-pane"),
                ("t", "jump-to-forward-prompt"),
                ("Home", "start-of-line"),
                ("End", "end-of-line"),
                ("M-<", "history-top"),
                ("M->", "history-bottom"),
                ("M-R", "top-line"),
                ("M-b", "previous-word"),
                ("C-M-b", "previous-matching-bracket"),
                ("M-f", "next-word-end"),
                ("C-M-f", "next-matching-bracket"),
                ("M-m", "back-to-indentation"),
                ("M-r", "middle-line"),
                ("M-v", "page-up"),
                ("M-w", "copy-selection-and-cancel"),
                ("M-x", "jump-to-mark"),
                ("M-{", "previous-paragraph"),
                ("M-}", "next-paragraph"),
                ("M-Up", "halfpage-up"),
                ("M-Down", "halfpage-down"),
                ("C-Up", "scroll-up"),
                ("C-Down", "scroll-down"),
                ("NPage", "page-down"),
                ("PPage", "page-up"),
                ("Up", "cursor-up"),
                ("Down", "cursor-down"),
                ("Left", "cursor-left"),
                ("Right", "cursor-right"),
            ]
        }
    }

    /// Handle one key (tmux key name) using the default table.
    pub fn key(&mut self, key: &str) -> Action {
        let key = normalize_key(key);
        if let Some(kind) = self.pending_jump.take() {
            return match key_char(key) {
                Some(c) => {
                    let n = self.take_count();
                    self.jump = Some((kind, c.clone()));
                    self.do_jump(kind, &c, n);
                    Action::None
                }
                None => {
                    self.count = None;
                    Action::None
                }
            };
        }
        let digit = if self.vi { Some(key) } else { key.strip_prefix("M-") }
            .filter(|k| k.len() == 1)
            .and_then(|k| k.parse::<usize>().ok());
        if let Some(d) = digit
            && (d != 0 || self.count.is_some())
        {
            self.count = Some((self.count.unwrap_or(0) * 10 + d).min(100_000));
            return Action::None;
        }
        let cmd = Self::default_bindings(self.vi).into_iter().find(|(k, _)| *k == key).map(|(_, c)| c);
        match cmd {
            Some(cmd) => self.run(cmd, &[]),
            None => {
                self.count = None;
                Action::None
            }
        }
    }

    /// Run a named copy-mode command (`send-keys -X name args`). A pending repeat count
    /// (from digit keys) is applied and consumed.
    pub fn run(&mut self, command: &str, args: &[String]) -> Action {
        if command.starts_with("search-") {
            self.search_marks = true;
        } else if matches!(command,
                "append-selection"
                | "append-selection-and-cancel"
                | "back-to-indentation"
                | "begin-selection"
                | "cancel"
                | "clear-selection"
                | "copy-end-of-line"
                | "copy-end-of-line-and-cancel"
                | "copy-line"
                | "copy-line-and-cancel"
                | "copy-pipe"
                | "copy-pipe-and-cancel"
                | "copy-pipe-end-of-line"
                | "copy-pipe-end-of-line-and-cancel"
                | "copy-pipe-line"
                | "copy-pipe-line-and-cancel"
                | "copy-selection"
                | "copy-selection-and-cancel"
                | "cursor-down-and-cancel"
                | "halfpage-down-and-cancel"
                | "jump-to-mark"
                | "next-matching-bracket"
                | "next-prompt"
                | "page-down-and-cancel"
                | "pipe"
                | "pipe-and-cancel"
                | "previous-matching-bracket"
                | "previous-prompt"
                | "rectangle-off"
                | "rectangle-on"
                | "rectangle-toggle"
                | "refresh-from-pane"
                | "scroll-bottom"
                | "scroll-down-and-cancel"
                | "scroll-middle"
                | "scroll-top"
                | "select-line"
                | "select-word"
                | "set-mark"
                | "stop-selection"
            ) || (!self.vi && matches!(command,
                "bottom-line"
                | "cursor-centre-horizontal"
                | "cursor-centre-vertical"
                | "cursor-down"
                | "cursor-left"
                | "cursor-right"
                | "cursor-up"
                | "end-of-line"
                | "goto-line"
                | "halfpage-down"
                | "halfpage-up"
                | "history-bottom"
                | "history-top"
                | "jump-again"
                | "jump-backward"
                | "jump-forward"
                | "jump-reverse"
                | "jump-to-backward"
                | "jump-to-forward"
                | "middle-line"
                | "next-paragraph"
                | "next-space"
                | "next-space-end"
                | "next-word"
                | "next-word-end"
                | "other-end"
                | "page-down"
                | "page-up"
                | "previous-paragraph"
                | "previous-space"
                | "previous-word"
                | "scroll-down"
                | "scroll-up"
                | "start-of-line"
                | "top-line"
            )) {
            self.search_marks = false;
        }
        let action = self.run_inner(command, args);
        // copy-mode -e: these leave copy mode at the bottom, selection or not
        let downward = matches!(command, "scroll-down" | "page-down" | "halfpage-down");
        if self.exit_on_bottom && downward && action == Action::None && self.scroll_offset() == 0 {
            return Action::Cancel;
        }
        action
    }

    fn run_inner(&mut self, command: &str, args: &[String]) -> Action {
        self.pending_jump = None;
        let n = self.take_count();
        let arg = args.first().map(String::as_str);
        let seps = self.separators.clone();
        match command {
            "cancel" => return Action::Cancel,
            "cursor-up" => self.vertical(-(n as isize)),
            "cursor-down" => self.vertical(n as isize),
            "cursor-left" => self.repeat_move(n, |s, p| s.step_left(p)),
            "cursor-right" => self.repeat_move(n, |s, p| s.step_right(p)),
            "start-of-line" => self.set_x(0),
            "end-of-line" => {
                let x = self.max_x(self.cy);
                self.set_x(x)
            }
            "back-to-indentation" => {
                let x = self.indent(self.cy);
                self.set_x(x)
            }
            "next-word" => self.repeat_move(n, |s, p| Some(s.next_word(p, &seps))),
            "next-space" => self.repeat_move(n, |s, p| Some(s.next_word(p, ""))),
            "next-word-end" => self.repeat_move(n, |s, p| Some(s.next_word_end(p, &seps))),
            "next-space-end" => self.repeat_move(n, |s, p| Some(s.next_word_end(p, ""))),
            "previous-word" => self.repeat_move(n, |s, p| Some(s.previous_word(p, &seps))),
            "previous-space" => self.repeat_move(n, |s, p| Some(s.previous_word(p, ""))),
            "next-matching-bracket" => (0..n).for_each(|_| self.matching_bracket(true)),
            "previous-matching-bracket" => (0..n).for_each(|_| self.matching_bracket(false)),
            "top-line" => self.set_cursor(self.top, 0),
            "middle-line" => {
                let rows = self.height.min(self.lines.len() - self.top);
                self.set_cursor(self.top + (rows - 1) / 2, 0)
            }
            "bottom-line" => {
                let y = (self.top + self.height - 1).min(self.lines.len() - 1);
                self.set_cursor(y, 0)
            }
            "history-top" => {
                self.top = 0;
                self.set_cursor(0, 0)
            }
            "history-bottom" => {
                self.top = self.hsize();
                let y = self.lines.len() - 1;
                let x = self.max_x(y);
                self.set_cursor(y, x)
            }
            "page-up" => (0..n).for_each(|_| self.page_up(self.height)),
            "page-down" => (0..n).for_each(|_| self.page_down(self.height)),
            "halfpage-up" => (0..n).for_each(|_| self.page_up((self.height / 2).max(1))),
            "halfpage-down" => (0..n).for_each(|_| self.page_down((self.height / 2).max(1))),
            "scroll-up" => (0..n).for_each(|_| self.scroll_line(true)),
            "scroll-down" => (0..n).for_each(|_| self.scroll_line(false)),
            "scroll-middle" => {
                self.top = self.cy.saturating_sub(self.height / 2).min(self.hsize());
            }
            "begin-selection" => {
                self.anchor = Some((self.cy, self.cx));
                self.line_mode = false;
            }
            "select-line" => {
                self.line_mode = true;
                self.anchor = Some((self.cy, 0));
                for _ in 1..n {
                    self.vertical(1);
                }
                let x = self.max_x(self.cy);
                self.set_x(x);
            }
            "select-word" => {
                // window_copy_cmd_select_word: back to the start of the word, then to its end
                let start = self.reader_previous_word((self.cy, self.cx), &seps, false, !self.vi);
                let (py, px) = start;
                let (mut ny, mut nx) = (py, px + 1);
                if self.wrapped(ny) && nx > self.width - 1 && ny + 1 < self.lines.len() {
                    nx = 0;
                    ny += 1;
                }
                let single = px < self.len(py) && self.in_set((ny, nx), WHITESPACE);
                self.line_mode = false;
                self.anchor = Some(start);
                let end = if single { self.step_right(start).unwrap_or(start) } else { self.reader_next_word_end(start, &seps) };
                let end = if self.vi { self.step_left(end).filter(|p| *p >= start).unwrap_or(start) } else { end };
                self.set_cursor(end.0, end.1);
            }
            "rectangle-toggle" => self.rect = !self.rect,
            "rectangle-on" => self.rect = true,
            "rectangle-off" => self.rect = false,
            "clear-selection" => {
                self.anchor = None;
                self.line_mode = false;
            }
            "other-end" => {
                if let Some(a) = self.anchor {
                    self.anchor = Some((self.cy, self.cx));
                    if self.line_mode {
                        // Keep whole lines selected: anchor at the start of its line.
                        let (ay, _) = a;
                        let (cy, _) = self.anchor.unwrap();
                        if ay < cy {
                            self.anchor = Some((cy, self.max_x(cy)));
                        }
                    }
                    self.set_cursor(a.0, a.1);
                }
            }
            "copy-selection" | "copy-pipe" => return self.copy_selection(true, false),
            "copy-selection-no-clear" | "copy-pipe-no-clear" => return self.copy_selection(false, false),
            "copy-selection-and-cancel" | "copy-pipe-and-cancel" | "append-selection-and-cancel" => {
                return self.copy_selection(true, true);
            }
            "append-selection" => return self.copy_selection(true, false),
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
            "search-forward" | "search-forward-incremental" | "search-backward" | "search-backward-incremental" => {
                let forward = command.starts_with("search-forward");
                match arg {
                    Some(t) if !t.is_empty() => self.search = Some((t.to_string(), forward)),
                    _ => {
                        if let Some((_, f)) = self.search.as_mut() {
                            *f = forward;
                        }
                    }
                }
                if let Some((t, _)) = self.search.clone() {
                    self.do_search(&t, forward, n);
                }
            }
            "search-forward-text" | "search-backward-text" => {
                let forward = command == "search-forward-text";
                let text = match arg {
                    Some(t) => t.to_string(),
                    None => self.word_under_cursor(),
                };
                if !text.is_empty() {
                    // The -text commands search the word literally, even if it
                    // contains regex metacharacters.
                    let mut pattern = String::new();
                    for c in text.chars() {
                        if ".^$*+?()[]{}|\\".contains(c) { pattern.push('\\'); }
                        pattern.push(c);
                    }
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
                if let Some(l) = arg.and_then(|a| a.trim().parse::<usize>().ok()) {
                    let row = self.cy - self.top;
                    self.top = self.hsize() - l.min(self.hsize());
                    let y = (self.top + row).min(self.lines.len() - 1);
                    self.cy = y;
                    self.cx = self.snap(y, self.lastcx.min(self.max_x(y)));
                }
            }
            "previous-paragraph" => (0..n).for_each(|_| self.previous_paragraph()),
            "next-paragraph" => (0..n).for_each(|_| self.next_paragraph()),
            "set-mark" => self.mark = Some((self.cy, self.cx)),
            "jump-to-mark" => {
                if let Some(m) = self.mark {
                    self.mark = Some((self.cy, self.cx));
                    let m = (m.0.min(self.lines.len() - 1), m.1);
                    self.scroll_to(m);
                }
            }
            "toggle-position" => self.show_position = !self.show_position,
            // Handled by the caller (it owns the pane); nothing to do here.
            "refresh-from-pane" => {}
            _ => {}
        }
        Action::None
    }

    // ----------------------------------------------------------------- render

    /// The `height` lines to display plus the cursor position within them.
    pub fn render(&self) -> (Vec<Line>, (u16, u16)) {
        self.render_with_style(Style { fg: BLACK, bg: YELLOW, attrs: 0 })
    }

    pub fn render_with_style(&self, mode_style: Style) -> (Vec<Line>, (u16, u16)) {
        let mut out = Vec::with_capacity(self.height);
        let needle = self.search.as_ref().map(|(t, _)| t.as_str()).filter(|t| !t.is_empty());
        for row in 0..self.height {
            let y = self.top + row;
            let mut line = Line { cells: Vec::with_capacity(self.width), wrapped: false };
            if let Some(src) = self.lines.get(y) {
                line.wrapped = src.wrapped;
                line.cells.extend(src.cells.iter().take(self.width).cloned());
                // A wide char cut at the right edge: blank its lead.
                if line.cells.len() == self.width
                    && src.cells.get(self.width).is_some_and(Cell::is_continuation)
                    && let Some(last) = line.cells.last_mut()
                {
                    last.text = " ".into();
                }
            }
            line.cells.resize(self.width, Cell::default());
            if let Some(needle) = needle.filter(|_| self.search_marks) {
                for (x0, x1) in self.row_matches(y, needle) {
                    let current = y == self.cy && x0 == self.cx;
                    let bg = if current { MAGENTA } else { CYAN };
                    for c in line.cells.iter_mut().take(x1.min(self.width)).skip(x0) {
                        c.style.fg = BLACK;
                        c.style.bg = bg;
                    }
                }
            }
            if let Some((x0, x1)) = self.sel_range(y) {
                for c in line.cells.iter_mut().take(x1.min(self.width)).skip(x0) {
                    c.style = mode_style;
                }
            }
            out.push(line);
        }
        if self.show_position {
            let mut ind = format!("[{}/{}]", self.scroll_offset(), self.hsize());
            if let Some(needle) = needle {
                let results: usize = (0..self.lines.len()).map(|y| self.row_matches(y, needle).len()).sum();
                ind.push_str(&format!(" ({results} results)"));
            }
            let len = ind.chars().count();
            if len <= self.width {
                let start = self.width - len;
                let row = &mut out[0].cells;
                if row[start].is_continuation() && start > 0 {
                    row[start - 1].text = " ".into();
                }
                let style = mode_style;
                for (i, ch) in ind.chars().enumerate() {
                    row[start + i] = Cell { text: ch.to_string(), style };
                }
            }
        }
        let cursor = ((self.cy - self.top) as u16, self.cx.min(self.width - 1) as u16);
        (out, cursor)
    }

    // ----------------------------------------------------------------- geometry helpers

    fn pad(&mut self) {
        while self.lines.len() < self.height {
            self.lines.push(Line::default());
        }
    }

    fn hsize(&self) -> usize {
        self.lines.len().saturating_sub(self.height)
    }

    fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1).max(1)
    }

    fn cell(&self, y: usize, x: usize) -> Option<&Cell> {
        self.lines.get(y).and_then(|l| l.cells.get(x))
    }

    fn is_cont(&self, y: usize, x: usize) -> bool {
        self.cell(y, x).is_some_and(Cell::is_continuation)
    }

    fn wrapped(&self, y: usize) -> bool {
        self.lines.get(y).is_some_and(|l| l.wrapped)
    }

    /// Length of the line's text (trailing blanks excluded).
    fn len(&self, y: usize) -> usize {
        let Some(l) = self.lines.get(y) else { return 0 };
        l.cells.iter().rposition(|c| c.text != " ").map_or(0, |i| i + 1)
    }

    /// Number of cells copied from the line: the whole row when it soft-wraps.
    fn copy_len(&self, y: usize) -> usize {
        if self.wrapped(y) { self.lines[y].cells.len().min(self.width) } else { self.len(y) }
    }

    /// Largest column the cursor may occupy on row `y` (tmux allows one past the text).
    fn max_x(&self, y: usize) -> usize {
        if self.wrapped(y) {
            self.lines[y].cells.len().min(self.width).saturating_sub(1)
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

    fn indent(&self, y: usize) -> usize {
        let len = self.len(y);
        (0..len).find(|&x| self.cell(y, x).is_some_and(|c| c.text != " " && c.text != "\t")).unwrap_or(0)
    }

    fn class(&self, (y, x): Pos, seps: &str) -> Class {
        let x = self.snap(y, x);
        match self.cell(y, x) {
            None => Class::Space,
            Some(c) if c.text == " " || c.text == "\t" || c.text.is_empty() => Class::Space,
            Some(c) => {
                let mut chars = c.text.chars();
                match (chars.next(), chars.next()) {
                    (Some(ch), None) if seps.contains(ch) => Class::Sep,
                    _ => Class::Word,
                }
            }
        }
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

    /// Move the cursor, scrolling minimally.
    fn set_cursor(&mut self, y: usize, x: usize) {
        self.cy = y.min(self.lines.len() - 1);
        self.cx = self.snap(self.cy, x);
        self.lastcx = self.cx;
        self.ensure_visible();
    }

    fn set_x(&mut self, x: usize) {
        self.set_cursor(self.cy, x);
    }

    /// Move the cursor to a far position; centre the view on it if it is off screen.
    fn scroll_to(&mut self, (y, x): Pos) {
        if y < self.top || y >= self.top + self.height {
            self.top = y.saturating_sub(self.height / 2).min(self.hsize());
        }
        self.set_cursor(y, x);
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
            self.sticky_begin(norect);
            if dy < 0 {
                self.cy = self.cy.saturating_sub(1);
            } else {
                self.cy = (self.cy + 1).min(last);
            }
            self.ensure_visible();
            self.sticky_end(norect);
        }
        self.lastcx = self.cx;
    }

    /// tmux window_copy_pageup1: scroll the view up `n` lines keeping the cursor row.
    fn page_up(&mut self, n: usize) {
        let row = self.cy - self.top;
        let row = if self.top < n {
            self.top = 0;
            row.saturating_sub(n)
        } else {
            self.top -= n;
            row
        };
        self.place_row(row);
    }

    /// tmux window_copy_pagedown1.
    fn page_down(&mut self, n: usize) {
        let row = self.cy - self.top;
        let hsize = self.hsize();
        let row = if self.top + n > hsize {
            let rest = n - (hsize - self.top);
            self.top = hsize;
            (row + rest).min(self.height - 1)
        } else {
            self.top += n;
            row
        };
        self.place_row(row);
    }

    fn place_row(&mut self, row: usize) {
        self.sticky_begin(true);
        let y = (self.top + row).min(self.lines.len() - 1);
        self.cy = y;
        self.sticky_end(self.anchor.is_none() || !self.rect);
    }

    // ----------------------------------------------------------------- tmux grid_reader

    /// grid_in_set: whether the character at a position is in `set` (blank past the end).
    fn in_set(&self, (y, x): Pos, set: &str) -> bool {
        match self.cell(y, x) {
            None => set.contains(' '),
            Some(c) if c.is_continuation() => false,
            Some(c) => {
                let mut chars = c.text.chars();
                matches!((chars.next(), chars.next()), (Some(ch), None) if set.contains(ch))
            }
        }
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

    /// vi `w`: skip the current word (or run of separators), then any whitespace.
    fn next_word(&self, mut p: Pos, seps: &str) -> Pos {
        let c = self.class(p, seps);
        if c != Class::Space {
            while let Some(q) = self.step_right(p) {
                p = q;
                if self.class(p, seps) != c {
                    break;
                }
            }
        }
        while self.class(p, seps) == Class::Space {
            match self.step_right(p) {
                Some(q) => p = q,
                None => break,
            }
        }
        p
    }

    /// vi `e` lands on the last character of the next word; emacs `M-f` just after it.
    fn next_word_end(&self, mut p: Pos, seps: &str) -> Pos {
        if self.vi {
            match self.step_right(p) {
                Some(q) => p = q,
                None => return p,
            }
        }
        while self.class(p, seps) == Class::Space {
            match self.step_right(p) {
                Some(q) => p = q,
                None => return p,
            }
        }
        let c = self.class(p, seps);
        while let Some(q) = self.step_right(p) {
            if self.class(q, seps) != c {
                if !self.vi {
                    p = q;
                }
                break;
            }
            p = q;
        }
        p
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
            if let Some(c) = self.cell(p.0, p.1) {
                out.push_str(&c.text);
            }
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

    fn matching_bracket(&mut self, next: bool) {
        let original = (self.cy, self.cx);
        let mut p = original;
        let bracket = |s: &Self, p: Pos| s.cell(p.0, p.1).map(|c| c.text.as_str()).unwrap_or("").chars().next();
        let opens = "{[(";
        let closes = "}])";
        let direction;
        let found;
        loop {
            let ch = bracket(self, p).unwrap_or(' ');
            if next && opens.contains(ch) { direction = true; found = ch; break; }
            if (!next || self.vi) && closes.contains(ch) { direction = false; found = ch; break; }
            if !self.vi {
                let adjacent = if next { self.step_right(p) } else { self.step_left(p) };
                if p == original && let Some(adjacent) = adjacent.filter(|a| a.0 == original.0) {
                    p = adjacent;
                    continue;
                }
                let p = if next { self.next_word_end(original, opens) } else { self.previous_word(original, closes) };
                self.set_cursor(p.0, p.1);
                return;
            }
            // vi scans for either bracket on the current logical (soft-wrapped) line.
            if !next { return; }
            let Some(q) = self.step_right(p) else { return };
            if q.0 != p.0 && !self.wrapped(p.0) { return; }
            p = q;
        }
        let other = if direction { closes.as_bytes()[opens.find(found).unwrap()] } else { opens.as_bytes()[closes.find(found).unwrap()] } as char;
        let mut depth = 1;
        while let Some(q) = if direction { self.step_right(p) } else { self.step_left(p) } {
            p = q;
            let ch = bracket(self, p).unwrap_or(' ');
            if ch == found { depth += 1; }
            else if ch == other {
                depth -= 1;
                if depth == 0 { self.set_cursor(p.0, p.1); return; }
            }
        }
    }

    fn do_jump(&mut self, kind: JumpKind, ch: &str, n: usize) {
        for _ in 0..n {
            let y = self.cy;
            let cx = self.cx;
            let len = self.lines[y].cells.len();
            let hit = |x: usize| self.cell(y, x).is_some_and(|c| c.text == ch);
            let found = match kind {
                JumpKind::Forward => (cx + 1..len).find(|&x| hit(x)),
                JumpKind::ToForward => (cx + 2..len).find(|&x| hit(x)).map(|x| x - 1),
                JumpKind::Backward => (0..cx).rev().find(|&x| hit(x)),
                JumpKind::ToBackward => (0..cx.saturating_sub(1)).rev().find(|&x| hit(x)).map(|x| x + 1),
            };
            match found {
                Some(x) => self.set_x(x),
                None => break,
            }
        }
    }

    /// All matches of `needle` on row `y` as cell ranges [x0, x1).
    fn row_matches(&self, y: usize, needle: &str) -> Vec<(usize, usize)> {
        let Some(line) = self.lines.get(y) else { return Vec::new() };
        let len = self.copy_len(y);
        let mut s = String::new();
        let mut starts: Vec<(usize, usize)> = Vec::new(); // (byte offset, column)
        for (x, c) in line.cells.iter().take(len).enumerate() {
            if c.text.is_empty() {
                continue;
            }
            starts.push((s.len(), x));
            s.push_str(&c.text);
        }
        let col_at = |b: usize| starts.iter().rev().find(|(o, _)| *o <= b).map_or(0, |(_, x)| *x);
        let col_after = |e: usize| starts.iter().find(|(o, _)| *o >= e).map_or(len, |(_, x)| *x);
        let mut out = Vec::new();
        let mut cache = self.search_cache.borrow_mut();
        if cache.0 != needle {
            *cache = (needle.to_string(), crate::format::compile_regex(needle, false));
        }
        let Some(re) = &cache.1 else { return out };
        let mut from = 0;
        while from <= s.len() {
            let Ok(Some(found)) = re.find_from_pos(&s, from) else { break };
            let b = found.start();
            out.push((col_at(b), col_after(found.end())));
            from = b + s[b..].chars().next().map_or(1, char::len_utf8);
        }
        out
    }

    fn find_match(&self, needle: &str, forward: bool) -> Option<Pos> {
        let total = self.lines.len();
        let (cy, cx) = (self.cy, self.cx);
        if forward {
            for i in 0..=total {
                let y = (cy + i) % total;
                let m = self.row_matches(y, needle);
                let found = if i == 0 {
                    m.iter().find(|r| r.0 > cx)
                } else if i == total {
                    m.iter().find(|r| r.0 <= cx)
                } else {
                    m.first()
                };
                if let Some(r) = found {
                    return Some((y, r.0));
                }
            }
        } else {
            for i in 0..=total {
                let y = (cy + total * 2 - i) % total;
                let m = self.row_matches(y, needle);
                let found = if i == 0 {
                    m.iter().rev().find(|r| r.0 < cx)
                } else if i == total {
                    m.iter().rev().find(|r| r.0 >= cx)
                } else {
                    m.last()
                };
                if let Some(r) = found {
                    return Some((y, r.0));
                }
            }
        }
        None
    }

    fn do_search(&mut self, needle: &str, forward: bool, n: usize) {
        for _ in 0..n {
            match self.find_match(needle, forward) {
                Some(p) => self.scroll_to(p),
                None => break,
            }
        }
    }

    // ----------------------------------------------------------------- selection

    /// Ordered (start, end) of the current selection.
    fn sel_bounds(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        let c = (self.cy, self.cx);
        Some(if a <= c { (a, c) } else { (c, a) })
    }

    /// Exclusive end column for an inclusive (vi) end at `x`, covering a wide char.
    fn incl_end(&self, y: usize, x: usize) -> usize {
        let mut e = x + 1;
        while self.is_cont(y, e) {
            e += 1;
        }
        e
    }

    /// Selected cell range [x0, x1) on row `y` (x1 may exceed the width).
    fn sel_range(&self, y: usize) -> Option<(usize, usize)> {
        let ((sy, sx), (ey, ex)) = self.sel_bounds()?;
        if y < sy || y > ey {
            return None;
        }
        if self.line_mode {
            return Some((0, usize::MAX));
        }
        if self.rect {
            let (ax, cx) = (self.anchor?.1, self.cx);
            let (l, r) = (ax.min(cx), ax.max(cx));
            let r = if self.vi { self.incl_end(y, r) } else { r };
            return Some((l, r));
        }
        let x0 = if y == sy { sx } else { 0 };
        let x1 = if y == ey {
            if self.vi { self.incl_end(y, ex) } else { ex }
        } else {
            usize::MAX
        };
        Some((x0, x1))
    }

    fn selection_text(&self) -> String {
        let Some(((sy, _), (ey, _))) = self.sel_bounds() else { return String::new() };
        let mut out = String::new();
        for y in sy..=ey {
            let Some((x0, x1)) = self.sel_range(y) else { continue };
            let len = self.copy_len(y);
            let end = x1.min(len);
            for x in x0..end {
                if let Some(c) = self.cell(y, x) {
                    out.push_str(&c.text);
                }
            }
            if self.rect {
                if y != ey {
                    out.push('\n');
                }
            } else if y != ey {
                if !self.wrapped(y) {
                    out.push('\n');
                }
            } else if self.vi && !self.wrapped(y) && x1 > len {
                // tmux keeps the final newline in vi mode when the selection runs past the text.
                out.push('\n');
            }
        }
        out
    }

    fn copy_selection(&mut self, clear: bool, cancel: bool) -> Action {
        if self.anchor.is_none() {
            return if cancel { Action::Cancel } else { Action::None };
        }
        let text = self.selection_text();
        if clear {
            self.anchor = None;
            self.line_mode = false;
        }
        if text.is_empty() {
            return if cancel { Action::Cancel } else { Action::None };
        }
        Action::Copy { text, cancel }
    }

    /// copy-line / copy-end-of-line: select from line start (or the cursor) to the end of
    /// the `n`th line and copy it, leaving the original state untouched.
    fn copy_lines(&mut self, n: usize, from_cursor: bool, cancel: bool) -> Action {
        let saved = (self.anchor, self.line_mode, self.rect, self.cy, self.cx, self.lastcx, self.top);
        self.anchor = Some((self.cy, if from_cursor { self.cx } else { 0 }));
        self.line_mode = false;
        self.rect = false;
        for _ in 1..n {
            self.vertical(1);
        }
        self.cx = self.max_x(self.cy);
        let text = self.selection_text();
        (self.anchor, self.line_mode, self.rect, self.cy, self.cx, self.lastcx, self.top) = saved;
        if text.is_empty() {
            return if cancel { Action::Cancel } else { Action::None };
        }
        Action::Copy { text, cancel }
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
        Line { cells, wrapped }
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

    fn keys(cm: &mut CopyMode, ks: &[&str]) -> Action {
        let mut last = Action::None;
        for k in ks {
            last = cm.key(k);
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
        assert_eq!(cm.cx, 5, "end-of-line is one past the text like tmux");
        keys(&mut cm, &["0"]);
        assert_eq!(cm.cx, 0);
        keys(&mut cm, &["g"]);
        assert_eq!(pos(&cm), (0, 0));
        assert_eq!(cm.scroll_offset(), 10);
        keys(&mut cm, &["G"]);
        assert_eq!(pos(&cm), (14, 6));
        assert_eq!(cm.scroll_offset(), 0);
        keys(&mut cm, &["Up", "Up", "Down"]);
        assert_eq!(cm.cy, 13);
    }

    #[test]
    fn horizontal_wraps_between_lines() {
        let mut cm = CopyMode::new(lines(&["ab", "cd"]), W, 2, (1, 0), true);
        cm.key("h");
        assert_eq!(pos(&cm), (0, 2));
        cm.key("l");
        assert_eq!(pos(&cm), (1, 0));
    }

    #[test]
    fn vertical_motion_keeps_column() {
        let mut cm = CopyMode::new(lines(&["abcdefgh", "ab", "abcdefgh"]), W, 3, (0, 6), true);
        cm.key("j");
        assert_eq!(pos(&cm), (1, 2));
        cm.key("j");
        assert_eq!(pos(&cm), (2, 6));
    }

    #[test]
    fn top_middle_bottom_and_indentation() {
        let mut cm = numbered();
        cm.key("H");
        assert_eq!(pos(&cm), (10, 0));
        cm.key("M");
        assert_eq!(cm.cy, 12);
        cm.key("L");
        assert_eq!(cm.cy, 14);
        let mut cm = CopyMode::new(lines(&["    indented"]), W, 1, (0, 9), true);
        cm.key("^");
        assert_eq!(cm.cx, 4);
    }

    #[test]
    fn word_motions_vi() {
        let mut cm = CopyMode::new(lines(&["foo bar-baz  qux", "next"]), W, 2, (0, 0), true);
        cm.key("w");
        assert_eq!(cm.cx, 4);
        cm.key("w");
        assert_eq!(cm.cx, 7, "separator run is its own word");
        cm.key("w");
        assert_eq!(cm.cx, 8);
        cm.key("w");
        assert_eq!(cm.cx, 13);
        cm.key("w");
        assert_eq!(pos(&cm), (1, 0), "crosses unwrapped line end");
        cm.key("b");
        assert_eq!(pos(&cm), (0, 13));
        cm.key("b");
        assert_eq!(cm.cx, 8);
        cm.key("b");
        assert_eq!(cm.cx, 7);
        cm.key("b");
        assert_eq!(cm.cx, 4);
        cm.key("0");
        cm.key("e");
        assert_eq!(cm.cx, 2);
        cm.key("e");
        assert_eq!(cm.cx, 6);
        cm.key("0");
        cm.key("W");
        assert_eq!(cm.cx, 4);
        cm.key("W");
        assert_eq!(cm.cx, 13, "space-word skips separators");
        cm.key("B");
        assert_eq!(cm.cx, 4);
        cm.key("E");
        assert_eq!(cm.cx, 10);
    }

    #[test]
    fn word_motion_emacs_end_is_after_word() {
        let mut cm = CopyMode::new(lines(&["foo bar"]), W, 1, (0, 0), false);
        cm.key("M-f");
        assert_eq!(cm.cx, 3);
        cm.key("M-f");
        assert_eq!(cm.cx, 7);
        cm.key("M-b");
        assert_eq!(cm.cx, 4);
    }

    #[test]
    fn custom_word_separators() {
        let mut cm = CopyMode::new(lines(&["a.b c"]), W, 1, (0, 0), true);
        cm.key("w");
        assert_eq!(cm.cx, 4);
        cm.key("0");
        cm.set_word_separators(" .");
        cm.key("w");
        assert_eq!(cm.cx, 1);
    }

    #[test]
    fn words_do_not_break_on_soft_wrap() {
        let mut v = vec![line("aaaaaaaaaaaaaaaaaaab", true), line("cd ef", false)];
        v[0].cells[19].text = "b".into();
        let mut cm = CopyMode::new(v, W, 2, (0, 0), true);
        cm.key("w");
        assert_eq!(pos(&cm), (1, 3), "wrapped word continues on next row");
        cm.key("b");
        cm.key("b");
        assert_eq!(pos(&cm), (0, 0));
        cm.key("e");
        assert_eq!(pos(&cm), (1, 1));
    }

    #[test]
    fn wide_chars_navigation() {
        let mut cm = CopyMode::new(lines(&["가나 다라 x"]), W, 1, (0, 0), true);
        cm.key("l");
        assert_eq!(cm.cx, 2, "skips continuation cell");
        cm.key("h");
        assert_eq!(cm.cx, 0);
        cm.key("w");
        assert_eq!(cm.cx, 5);
        cm.key("w");
        assert_eq!(cm.cx, 10);
        cm.key("b");
        assert_eq!(cm.cx, 5);
        // Moving onto a continuation column snaps to the lead.
        let mut cm = CopyMode::new(lines(&["abcd", "가나"]), W, 2, (0, 3), true);
        cm.key("j");
        assert_eq!(pos(&cm), (1, 2));
        // A cursor at the end of a line stays at line ends (tmux lastsx).
        let mut cm = CopyMode::new(lines(&["abc", "가나다"]), W, 2, (0, 3), true);
        cm.key("j");
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
        let mut cm = numbered();
        cm.key("k"); // row 3
        cm.key("C-b");
        assert_eq!(cm.scroll_offset(), 5);
        assert_eq!(cm.cy, 8);
        cm.key("PageUp");
        assert_eq!(cm.scroll_offset(), 10);
        assert_eq!(cm.cy, 3);
        cm.key("C-b");
        assert_eq!(cm.scroll_offset(), 10, "clamped at top");
        assert_eq!(cm.cy, 0, "cursor row moves up when the view can't");
        cm.key("C-f");
        assert_eq!(cm.scroll_offset(), 5);
        cm.key("PageDown");
        cm.key("C-f");
        assert_eq!(cm.scroll_offset(), 0);
        assert_eq!(cm.cy, 14, "cursor pushed to the bottom row");
        cm.key("C-u");
        assert_eq!(cm.scroll_offset(), 2);
        cm.key("C-d");
        assert_eq!(cm.scroll_offset(), 0);
    }

    #[test]
    fn scroll_line_keeps_cursor_row() {
        // tmux 3.6 scroll-up / scroll-down: the view moves, the cursor keeps its screen row
        let mut cm = numbered();
        cm.key("k");
        let row = cm.cy - cm.top;
        cm.key("C-y");
        assert_eq!(cm.scroll_offset(), 1);
        assert_eq!((cm.cy - cm.top, cm.cy), (row, 12));
        cm.key("C-y");
        assert_eq!((cm.cy - cm.top, cm.cy), (row, 11));
        cm.key("C-e");
        assert_eq!(cm.scroll_offset(), 1);
        assert_eq!(cm.cy, 12);
        keys(&mut cm, &["g", "z"]);
        assert_eq!(cm.scroll_offset(), 10);
        keys(&mut cm, &["1", "2", "j", "z"]);
        assert_eq!(cm.top, 10);
    }

    #[test]
    fn search_wraps_around() {
        let mut cm = CopyMode::new(lines(&["foo one", "bar", "foo two", "baz"]), W, 4, (1, 0), true);
        assert_eq!(cm.key("/"), Action::Prompt { label: "(search down)".into(), command: "search-forward".into() });
        cm.run("search-forward", &["foo".into()]);
        assert_eq!(pos(&cm), (2, 0));
        cm.key("n");
        assert_eq!(pos(&cm), (0, 0), "wrapped to the top");
        cm.key("n");
        assert_eq!(pos(&cm), (2, 0));
        cm.key("N");
        assert_eq!(pos(&cm), (0, 0));
        assert_eq!(cm.key("?"), Action::Prompt { label: "(search up)".into(), command: "search-backward".into() });
        cm.run("search-backward", &["ba".into()]);
        assert_eq!(pos(&cm), (3, 0), "backward wraps to the bottom");
        cm.key("n");
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
        assert!(matches!(cm.key("C-s"), Action::Prompt { ref label, .. } if label == "(search down)"));
        assert!(matches!(cm.key("C-r"), Action::Prompt { ref label, .. } if label == "(search up)"));
        assert!(matches!(cm.key("g"), Action::Prompt { ref command, .. } if command == "goto-line"));
    }

    #[test]
    fn search_text_under_cursor() {
        let mut cm = CopyMode::new(lines(&["alpha beta", "gamma alpha"]), W, 2, (0, 1), true);
        cm.key("*");
        assert_eq!(pos(&cm), (1, 6));
        cm.key("n");
        assert_eq!(pos(&cm), (0, 0));
    }

    #[test]
    fn selection_copy_vi_inclusive() {
        let mut cm = CopyMode::new(lines(&["hello world", "second line"]), W, 2, (0, 0), true);
        keys(&mut cm, &["Space", "e"]);
        assert!(cm.selection_active());
        let a = cm.key("Enter");
        assert_eq!(a, Action::Copy { text: "hello".into(), cancel: true });
        // Multi-line, trailing spaces trimmed, newline between lines.
        let mut cm = CopyMode::new(lines(&["hello world", "second line"]), W, 2, (0, 6), true);
        keys(&mut cm, &["Space", "j", "0", "e"]);
        assert_eq!(copied(cm.key("Enter")), "world\nsecond");
        // Selecting backwards works too.
        let mut cm = CopyMode::new(lines(&["hello world"]), W, 1, (0, 4), true);
        keys(&mut cm, &["Space", "0"]);
        assert_eq!(copied(cm.run("copy-selection", &[])), "hello");
        assert!(!cm.selection_active(), "copy-selection clears");
        // v$ includes the newline like tmux.
        let mut cm = CopyMode::new(lines(&["abc"]), W, 1, (0, 1), true);
        keys(&mut cm, &["Space", "$"]);
        assert_eq!(copied(cm.run("copy-selection-no-clear", &[])), "bc\n");
        assert!(cm.selection_active());
    }

    #[test]
    fn selection_copy_emacs_exclusive() {
        let mut cm = CopyMode::new(lines(&["hello world"]), W, 1, (0, 0), false);
        keys(&mut cm, &["C-Space", "M-f"]);
        assert_eq!(cm.key("M-w"), Action::Copy { text: "hello".into(), cancel: true });
        let mut cm = CopyMode::new(lines(&["abc", "def"]), W, 2, (0, 0), false);
        keys(&mut cm, &["C-Space", "C-e", "C-n", "C-e"]);
        assert_eq!(copied(cm.key("M-w")), "abc\ndef");
        // C-g clears, Escape cancels.
        let mut cm = CopyMode::new(lines(&["abc"]), W, 1, (0, 0), false);
        cm.key("C-Space");
        cm.key("C-g");
        assert!(!cm.selection_active());
        assert_eq!(cm.key("Escape"), Action::Cancel);
    }

    #[test]
    fn copy_joins_soft_wrapped_lines() {
        let v = vec![line("aaaaaaaaaaaaaaaaaaaa", true), line("bbb   ", false), line("ccc", false)];
        let mut cm = CopyMode::new(v, W, 3, (0, 0), true);
        keys(&mut cm, &["Space", "j", "j", "$", "h"]);
        assert_eq!(copied(cm.key("Enter")), format!("{}bbb\nccc", "a".repeat(20)));
    }

    #[test]
    fn copy_wide_chars_once() {
        let mut cm = CopyMode::new(lines(&["x 한국어 y"]), W, 1, (0, 2), true);
        keys(&mut cm, &["Space", "e"]);
        assert_eq!(cm.cx, 6);
        assert_eq!(copied(cm.key("Enter")), "한국어");
    }

    #[test]
    fn line_selection() {
        let mut cm = CopyMode::new(lines(&["one", "two", "three"]), W, 3, (0, 1), true);
        keys(&mut cm, &["V", "j"]);
        assert_eq!(copied(cm.key("Enter")), "one\ntwo\n");
        let mut cm = CopyMode::new(lines(&["one", "two", "three"]), W, 3, (2, 2), true);
        keys(&mut cm, &["V", "k"]);
        assert_eq!(copied(cm.key("Enter")), "two\nthree\n");
        let (view, _) = {
            let mut cm = CopyMode::new(lines(&["one", "two"]), W, 2, (0, 1), true);
            cm.key("V");
            cm.render()
        };
        assert!(view[0].cells[..10].iter().all(|c| c.style.bg == YELLOW && c.style.fg == BLACK));
        assert!(view[1].cells.iter().all(|c| c.style.bg != YELLOW));
    }

    #[test]
    fn rectangle_selection() {
        let mut cm = CopyMode::new(lines(&["abcdef", "ghijkl", "mn", "stuvwx"]), W, 4, (0, 1), true);
        keys(&mut cm, &["Space", "C-v", "3", "j", "l", "l"]);
        assert_eq!(copied(cm.key("Enter")), "bcd\nhij\nn\ntuv");
        // v is rectangle-toggle in the default vi table.
        let mut cm = CopyMode::new(lines(&["abcdef", "ghijkl"]), W, 2, (0, 1), true);
        keys(&mut cm, &["Space", "v", "j", "l"]);
        assert_eq!(copied(cm.key("Enter")), "bc\nhi");
        let mut cm = CopyMode::new(lines(&["abcdef", "ghijkl"]), W, 2, (0, 1), false);
        keys(&mut cm, &["C-Space", "R", "Down", "Right", "Right"]);
        assert_eq!(copied(cm.key("M-w")), "bc\nhi");
    }

    #[test]
    fn select_word_other_end_and_clear() {
        let mut cm = CopyMode::new(lines(&["foo barbaz qux"]), W, 1, (0, 6), true);
        cm.run("select-word", &[]);
        assert_eq!(copied(cm.run("copy-selection-no-clear", &[])), "barbaz");
        cm.key("o");
        assert_eq!(cm.cx, 4);
        assert_eq!(copied(cm.run("copy-selection-no-clear", &[])), "barbaz");
        cm.key("Escape");
        assert!(!cm.selection_active());
        assert_eq!(cm.key("Enter"), Action::Cancel, "copy without selection just cancels");
        assert_eq!(cm.run("copy-selection", &[]), Action::None);
    }

    #[test]
    fn copy_line_and_end_of_line() {
        let mut cm = CopyMode::new(lines(&["first line", "second"]), W, 2, (0, 6), true);
        assert_eq!(cm.run("copy-end-of-line", &[]), Action::Copy { text: "line\n".into(), cancel: false });
        assert_eq!(pos(&cm), (0, 6), "state restored");
        assert_eq!(copied(cm.run("copy-line", &[])), "first line\n");
        cm.count = Some(2);
        assert_eq!(copied(cm.run("copy-line", &[])), "first line\nsecond\n");
        assert_eq!(cm.key("D"), Action::Copy { text: "line\n".into(), cancel: true });
        let mut cm = CopyMode::new(lines(&["first line"]), W, 1, (0, 6), false);
        assert_eq!(cm.key("C-k"), Action::Copy { text: "line".into(), cancel: true });
    }

    #[test]
    fn regex_search_and_literal_text_search() {
        let mut cm = CopyMode::new(lines(&["abc123", "a.b", "acb", "end"]), W, 4, (3, 0), true);
        cm.run("search-backward", &["^abc[[:digit:]]+$".into()]);
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
        let mut cm = CopyMode::new(lines(&["foo", "food", "finish"]), W, 3, (2, 3), false);
        let origin = pos(&cm);
        cm.begin_incremental_search();
        cm.preview_search("f", false);
        assert_eq!(pos(&cm), (2, 0));
        cm.preview_search("foo", false);
        assert_eq!(pos(&cm), (1, 0));
        cm.preview_search("food", false);
        assert_eq!(pos(&cm), (1, 0), "typing extends the search from its original position");
        cm.preview_search("", false);
        assert_eq!(pos(&cm), origin, "deleting the query restores the cursor");
        cm.preview_search("foo", false);
        cm.finish_incremental_search(true);
        assert_eq!(pos(&cm), origin, "cancel restores the original cursor");
        cm.begin_incremental_search();
        cm.preview_search("foo", false);
        cm.finish_incremental_search(false);
        assert_eq!(pos(&cm), (1, 0), "accept must not advance to the next match");
        cm.run("search-again", &[]);
        assert_eq!(pos(&cm), (0, 0));
    }

    #[test]
    fn matching_brackets_and_default_keys() {
        let mut cm = CopyMode::new(lines(&["x (a [b] (c)) end", "plain", "{가", "}"]), W, 4, (0, 0), true);
        cm.key("%");
        assert_eq!(pos(&cm), (0, 12), "vi scans to the first bracket and handles nesting");
        cm.key("%");
        assert_eq!(pos(&cm), (0, 2), "vi closing bracket jumps back");
        cm.set_cursor(1, 0);
        cm.key("%");
        assert_eq!(pos(&cm), (1, 0), "vi does not scan beyond a hard line break");
        cm.set_cursor(2, 0);
        cm.key("%");
        assert_eq!(pos(&cm), (3, 0), "a matching pair may span lines");
        cm.run("previous-matching-bracket", &[]);
        assert_eq!(pos(&cm), (2, 0));
        cm.set_cursor(0, 0);
        assert_eq!(cm.key("y"), Action::None, "y is unbound in tmux's default vi table");
        let mut emacs = CopyMode::new(lines(&["(a (b))"]), W, 1, (0, 0), false);
        emacs.key("C-M-f");
        assert_eq!(pos(&emacs), (0, 6));
        emacs.key("C-M-b");
        assert_eq!(pos(&emacs), (0, 0));
        let mut missing = CopyMode::new(lines(&["(no close"]), W, 1, (0, 0), true);
        missing.key("%");
        assert_eq!(pos(&missing), (0, 0));
    }

    #[test]
    fn jumps() {
        let mut cm = CopyMode::new(lines(&["a.b.c.d"]), W, 1, (0, 0), true);
        keys(&mut cm, &["f", "."]);
        assert_eq!(cm.cx, 1);
        cm.key(";");
        assert_eq!(cm.cx, 3);
        cm.key(",");
        assert_eq!(cm.cx, 1);
        keys(&mut cm, &["0", "t", "."]);
        assert_eq!(cm.cx, 2, "t starts looking two cells right, like tmux");
        keys(&mut cm, &["t", "c"]);
        assert_eq!(cm.cx, 3);
        cm.key(";");
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
        assert!(matches!(cm.key(":"), Action::Prompt { ref command, .. } if command == "goto-line"));
        cm.run("goto-line", &["7".into()]);
        assert_eq!(cm.scroll_offset(), 7);
        assert_eq!(cm.cy, 3 + 4);
        cm.run("goto-line", &["999".into()]);
        assert_eq!(cm.scroll_offset(), 10);
        let mut cm = CopyMode::new(lines(&["a", "b", "", "c", "d", "", "e"]), W, 7, (6, 0), true);
        cm.key("{");
        assert_eq!(cm.cy, 5);
        cm.key("{");
        assert_eq!(cm.cy, 2);
        cm.key("}");
        assert_eq!(cm.cy, 5);
        cm.key("}");
        assert_eq!(pos(&cm), (6, 1));
    }

    #[test]
    fn marks() {
        let mut cm = numbered();
        cm.key("X");
        cm.key("g");
        cm.key("M-x");
        assert_eq!(pos(&cm), (14, 0));
        cm.key("M-x");
        assert_eq!(pos(&cm), (0, 0));
    }

    #[test]
    fn cancel_keys() {
        let mut cm = numbered();
        assert_eq!(cm.key("q"), Action::Cancel);
        assert_eq!(cm.key("C-c"), Action::Cancel);
        assert_eq!(cm.key("Escape"), Action::None, "vi Escape clears the selection");
        assert_eq!(cm.run("cancel", &[]), Action::Cancel);
        assert_eq!(cm.key("unbound-key"), Action::None);
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
        // C-y kept the cursor on screen row 2: line 11 (top 9).
        assert_eq!(cur, (2, 3));
        let inv: Vec<bool> = view[2].cells[..6].iter().map(|c| c.style.bg == YELLOW).collect();
        assert_eq!(inv, vec![false, true, true, true, false, false]);
        assert!(view[1].cells.iter().all(|c| c.style.bg != YELLOW));
        cm.key("P");
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
                other => panic!("{other:?}"),
            }
        };
        for vi in [false, true] {
            assert_eq!(pick("alpha beta gamma", 8, vi), "beta");
            // on a space: the word before it
            assert_eq!(pick("history 48 filler", 10, vi), "48");
            // separators end words; a run of separators is a word
            assert_eq!(pick("gamma-delta", 18 - 10, vi), "delta");
            assert_eq!(pick("x=1 y=22", 2, vi), "1");
            assert_eq!(pick("a -- b", 3, vi), "--");
        }
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
        let selected = Style { fg: BLACK, bg: CYAN, attrs: 0 };
        let (view,_) = cm.render_with_style(selected);
        assert_eq!(view[0].cells[0].style, selected);
        assert_eq!(view[0].cells[2].style.bg, Color::Default);
        assert!(view[0].text().ends_with("(2 results)"));
        cm.run("search-again", &[]);
        assert!(cm.search_marks);
    }

    #[test]
    fn resize_and_set_lines() {
        let mut cm = numbered();
        keys(&mut cm, &["k", "C-y"]);
        let (off, cy) = (cm.scroll_offset(), cm.cy);
        let mut more: Vec<Line> = (0..3).map(|i| line(&format!("new{i}"), false)).collect();
        let mut v = cm.lines.clone();
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
    fn default_bindings_tables() {
        let vi = CopyMode::default_bindings(true);
        assert!(vi.contains(&("j", "cursor-down")));
        assert!(vi.contains(&("/", "search-forward-prompt")));
        let emacs = CopyMode::default_bindings(false);
        assert!(emacs.contains(&("C-Space", "begin-selection")));
        assert!(emacs.contains(&("M-w", "copy-selection-and-cancel")));
    }
}
