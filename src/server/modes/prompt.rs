//! A prompt drawn inside a pane rather than on the status line (tmux 3.8 prompt.c as used by
//! window_pane_set_prompt and the prompts of the tree and switch modes): one line of input
//! with a label.

use crate::cells::{Cell, Color, Style};
use crate::server::vars::{Target, VarCtx};
use crate::server::{ClientId, Server};
use std::collections::HashMap;
use unicode_width::UnicodeWidthChar;

/// PROMPT_TYPE_COMMAND and PROMPT_TYPE_SEARCH: the history a prompt uses.
pub const TYPE_COMMAND: usize = 0;
pub const TYPE_SEARCH: usize = 1;

#[derive(Clone, Debug, Default)]
pub struct PanePrompt {
    pub label: String,
    pub input: Vec<char>,
    pub cursor: usize,
    /// PROMPT_SINGLE: the first key typed is the answer.
    pub single: bool,
    /// PROMPT_NUMERIC: only digits are accepted.
    pub numeric: bool,
    /// PROMPT_BSPACE_EXIT: backspace on empty input closes the prompt.
    pub bspace_exit: bool,
    /// PROMPT_INCREMENTAL: Up, Down, PPage and NPage (and Left and Right without
    /// PROMPT_EDITARROWS) are movement keys for the owner.
    pub incremental: bool,
    pub editarrows: bool,
    /// PROMPT_NOFORMAT (only shown in prompt_flags here).
    pub noformat: bool,
    /// PROMPT_ISMODE: a mode's prompt (prompt_flags).
    pub ismode: bool,
    /// The prompt type: TYPE_COMMAND or TYPE_SEARCH.
    pub ptype: usize,
    /// The position in the history (prompt_up_history's index: 0 is the input).
    pub hindex: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PromptKey {
    /// The key moved the cursor or was ignored.
    Handled,
    /// The key changed the input (prompt_key's "changed:").
    Changed,
    /// A movement key of an incremental prompt (PROMPT_KEY_MOVE).
    Move,
    /// Enter, or the key of a single-key prompt: the input is done.
    Done(String),
    /// Escape, C-c or C-g.
    Cancel,
    /// A non-digit in a numeric prompt: the input is done and the key goes on
    /// (PROMPT_KEY_NOT_HANDLED after the callback).
    Numeric(String),
}

/// The text for a prompt_flags format (prompt_flags_to_string).
#[allow(clippy::too_many_arguments)]
pub fn flags_string(single: bool, numeric: bool, incremental: bool, noformat: bool, key: bool, bspace_exit: bool, ispane: bool, ismode: bool, editarrows: bool) -> String {
    let flags = [
        (single, "SINGLE"),
        (numeric, "NUMERIC"),
        (incremental, "INCREMENTAL"),
        (noformat, "NOFORMAT"),
        (key, "KEY"),
        (bspace_exit, "BSPACE_EXIT"),
        (ispane, "ISPANE"),
        (ismode, "ISMODE"),
        (editarrows, "EDITARROWS"),
    ];
    flags.iter().filter(|(on, _)| *on).map(|(_, f)| *f).collect::<Vec<_>>().join(",")
}

/// The character a key types into a prompt (append_key), if any.
fn key_char(name: &str) -> Option<char> {
    if name == "Space" {
        return Some(' ');
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() => Some(c),
        _ => None,
    }
}

impl PanePrompt {
    pub fn new(label: &str, input: &str) -> PanePrompt {
        let input: Vec<char> = input.chars().collect();
        PanePrompt { label: label.to_string(), cursor: input.len(), input, ..PanePrompt::default() }
    }

    pub fn text(&self) -> String {
        self.input.iter().collect()
    }

    fn set_text(&mut self, s: &str) {
        self.input = s.chars().collect();
        self.cursor = self.input.len();
    }

    /// prompt_key with emacs status-keys: `history` is the history for the prompt's type,
    /// oldest first.
    pub fn key(&mut self, name: &str, history: &[String]) -> PromptKey {
        if self.numeric {
            if let Some(c) = key_char(name).filter(|c| c.is_ascii_digit()) {
                self.insert(c);
                return PromptKey::Changed;
            }
            return PromptKey::Numeric(self.text());
        }
        if self.single {
            // the key itself is the answer; keys that are not characters (arrows, function
            // keys) are ignored, control keys answer with something that is not a character
            return match name {
                "Enter" | "C-m" | "C-j" => PromptKey::Done("\r".into()),
                "Escape" | "C-[" | "C-c" | "C-g" | "BSpace" | "Tab" => PromptKey::Cancel,
                n => match key_char(n).or_else(|| n.strip_prefix("M-").and_then(key_char)) {
                    Some(c) => PromptKey::Done(c.to_string()),
                    None if n.starts_with("C-") && n.chars().count() == 3 => PromptKey::Cancel,
                    None => PromptKey::Handled,
                },
            };
        }
        // prompt_check_move
        if self.incremental {
            match name {
                "Up" | "Down" | "PPage" | "NPage" => return PromptKey::Move,
                "Left" | "Right" if !self.editarrows => return PromptKey::Move,
                _ => {}
            }
        }
        let size = self.input.len();
        match name {
            "Left" | "C-b" => self.cursor = self.cursor.saturating_sub(1),
            "Right" | "C-f" => self.cursor = (self.cursor + 1).min(size),
            "Home" | "C-a" => self.cursor = 0,
            "End" | "C-e" => self.cursor = size,
            "BSpace" | "C-h" => {
                if self.bspace_exit && size == 0 {
                    return PromptKey::Cancel;
                }
                if self.cursor == 0 {
                    return PromptKey::Handled;
                }
                self.cursor -= 1;
                self.input.remove(self.cursor);
                return PromptKey::Changed;
            }
            "DC" | "Delete" | "C-d" => {
                if self.cursor == size {
                    return PromptKey::Handled;
                }
                self.input.remove(self.cursor);
                return PromptKey::Changed;
            }
            "C-u" => {
                self.input.clear();
                self.cursor = 0;
                return PromptKey::Changed;
            }
            "C-k" => {
                if self.cursor == size {
                    return PromptKey::Handled;
                }
                self.input.truncate(self.cursor);
                return PromptKey::Changed;
            }
            "C-w" => {
                let mut i = self.cursor;
                while i > 0 && self.input[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && self.input[i - 1] != ' ' {
                    i -= 1;
                }
                self.input.drain(i..self.cursor);
                self.cursor = i;
                return PromptKey::Changed;
            }
            "C-Right" | "M-f" => {
                while self.cursor < size && self.input[self.cursor] == ' ' {
                    self.cursor += 1;
                }
                while self.cursor < size && self.input[self.cursor] != ' ' {
                    self.cursor += 1;
                }
                return PromptKey::Changed;
            }
            "C-Left" | "M-b" => {
                while self.cursor > 0 && self.input[self.cursor - 1] == ' ' {
                    self.cursor -= 1;
                }
                while self.cursor > 0 && self.input[self.cursor - 1] != ' ' {
                    self.cursor -= 1;
                }
                return PromptKey::Changed;
            }
            // prompt_up_history and prompt_down_history: the index runs from 0 (the input)
            // to the size of the history; down past the newest entry empties the input
            "Up" | "C-p" => {
                if history.is_empty() || self.hindex == history.len() {
                    return PromptKey::Handled;
                }
                self.hindex += 1;
                let s = history[history.len() - self.hindex].clone();
                self.set_text(&s);
                return PromptKey::Changed;
            }
            "Down" | "C-n" => {
                let s = if history.is_empty() || self.hindex == 0 {
                    String::new()
                } else {
                    self.hindex -= 1;
                    if self.hindex == 0 { String::new() } else { history[history.len() - self.hindex].clone() }
                };
                self.set_text(&s);
                return PromptKey::Changed;
            }
            "C-t" => {
                let idx = if self.cursor < size { self.cursor + 1 } else { self.cursor };
                if idx < 2 {
                    return PromptKey::Handled;
                }
                self.input.swap(idx - 2, idx - 1);
                self.cursor = idx;
                return PromptKey::Changed;
            }
            "Enter" | "C-m" | "C-j" => return PromptKey::Done(self.text()),
            "Escape" | "C-[" | "C-c" | "C-g" => return PromptKey::Cancel,
            n => {
                // append_key: anything else that is a character is typed, the rest (function
                // keys, other control and meta keys) is ignored
                if let Some(c) = key_char(n) {
                    self.insert(c);
                    return PromptKey::Changed;
                }
            }
        }
        PromptKey::Handled
    }

    /// prompt_mouse: a click at column `x` of the line puts the cursor there.
    pub fn mouse(&mut self, x: usize) {
        let mut col = crate::format::width(&self.label);
        let mut i = 0;
        while i < self.input.len() && col < x {
            col += self.input[i].width().unwrap_or(0);
            i += 1;
        }
        self.cursor = i;
    }

    fn insert(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += 1;
    }

    /// The prompt line in a pane `width` cells wide (prompt_draw): None where the line is
    /// left as it is.
    pub fn draw(&self, srv: &Server, t: Target, client: Option<ClientId>, width: usize, ispane: bool) -> (Vec<Option<Cell>>, usize) {
        let flags = flags_string(self.single, self.numeric, self.incremental, self.noformat, false, self.bspace_exit, ispane, self.ismode, self.editarrows);
        prompt_line(srv, t, client, &self.label, &self.input, self.cursor, &flags, self.ptype, width)
    }
}

/// fill= and align= of a style (the rest of it is parsed by parse_style_with).
fn fill_align(s: &str, vars: &VarCtx) -> (Option<Color>, String) {
    let (mut fill, mut align) = (None, String::from("left"));
    for word in s.split([',', ' ', '\n']).filter(|w| !w.is_empty()) {
        let lower = word.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("fill=") {
            fill = crate::format::parse_fill(v, Some(vars));
        } else if let Some(v) = lower.strip_prefix("align=") {
            align = v.to_string();
        } else if lower == "default" {
            fill = None;
        }
    }
    (fill, align)
}

/// A prompt line `width` cells wide drawn like prompt_layout and prompt_draw: the label is
/// message-format with #{message} set to the expanded prompt string, in message-style
/// (expanded with prompt_flags and prompt_type, so the default style only fills the line for
/// prompts outside panes and modes), followed by the input. Cells without a fill are None.
/// Returns the cells and the cursor column.
#[allow(clippy::too_many_arguments)]
pub fn prompt_line(srv: &Server, t: Target, client: Option<ClientId>, label: &str, input: &[char], cursor: usize, flags: &str, ptype: usize, width: usize) -> (Vec<Option<Cell>>, usize) {
    let mut out: Vec<Option<Cell>> = vec![None; width];
    if width == 0 {
        return (out, 0);
    }
    let mut extra = HashMap::new();
    extra.insert("prompt_input".to_string(), input.iter().collect::<String>());
    extra.insert("prompt_flags".to_string(), flags.to_string());
    extra.insert("prompt_type".to_string(), if ptype == TYPE_SEARCH { "search" } else { "command" }.to_string());
    extra.insert("command_prompt".to_string(), "0".to_string());
    let mut vars = VarCtx { srv, t, client, extra };
    let style_str = crate::format::expand_time(&srv.opt_session(t.sid, "message-style"), &vars);
    let gc = crate::format::parse_style_with(&style_str, Style::default(), Some(&vars)).map(|p| p.style).unwrap_or_default();
    let (fill, align) = fill_align(&style_str, &vars);
    let message = crate::format::expand_time(label, &vars);
    vars.extra.insert("message".to_string(), message);
    let expanded = crate::format::expand_time(&srv.opt_session(t.sid, "message-format"), &vars);
    let spans = crate::format::spans(&expanded, gc);
    let label_width = spans.iter().map(|s| crate::format::width(&s.text)).sum::<usize>().min(width);

    let shown = |c: char| -> (String, usize) {
        if (c as u32) < 0x20 || c as u32 == 0x7f {
            (format!("^{}", ((c as u8) ^ 0x40) as char), 2)
        } else {
            (c.to_string(), c.width().unwrap_or(0))
        }
    };
    let widths: Vec<usize> = input.iter().map(|c| shown(*c).1).collect();
    let pcursor: usize = widths[..cursor.min(input.len())].iter().sum();
    let pwidth: usize = widths.iter().sum();
    let avail = width - label_width;
    let (offset, input_width, mut cursor_x) = if avail == 0 {
        (0, 0, label_width)
    } else if pcursor >= avail {
        (pcursor - avail + 1, avail, label_width + avail - 1)
    } else {
        (0, pwidth.min(avail), label_width + pcursor)
    };
    let content_width = (label_width + input_width).min(width);
    let content_x = match align.as_str() {
        "centre" | "absolute-centre" => (width - content_width) / 2,
        "right" => width - content_width,
        _ => 0,
    };
    cursor_x += content_x;
    if let Some(bg) = fill {
        for c in out.iter_mut() {
            *c = Some(Cell { text: " ".into(), style: Style { bg, ..Style::default() } });
        }
    }
    if content_width != 0 {
        if label_width != 0 {
            let cells = crate::format::layout_line(&spans, label_width, gc);
            for (i, c) in cells.into_iter().enumerate() {
                out[content_x + i] = Some(c);
            }
        }
        let (mut col, mut x) = (0, content_x + label_width);
        for c in input {
            let (text, w) = shown(*c);
            if col < offset {
                col += w;
                continue;
            }
            if col + w > offset + input_width {
                break;
            }
            col += w;
            if w == 2 && text.starts_with('^') && text.len() == 2 {
                out[x] = Some(Cell { text: "^".into(), style: gc });
                out[x + 1] = Some(Cell { text: text[1..].into(), style: gc });
            } else if w > 0 {
                out[x] = Some(Cell { text, style: gc });
                if w == 2 {
                    out[x + 1] = Some(Cell { text: String::new(), style: gc });
                }
            }
            x += w;
        }
    }
    (out, cursor_x.min(width - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_keys() {
        let mut p = PanePrompt::new("(search) ", "ab");
        assert_eq!(p.key("c", &[]), PromptKey::Changed);
        assert_eq!(p.text(), "abc");
        assert_eq!(p.key("Left", &[]), PromptKey::Handled);
        p.key("BSpace", &[]);
        assert_eq!(p.text(), "ac");
        p.key("C-a", &[]);
        p.key("x", &[]);
        assert_eq!(p.text(), "xac");
        p.key("C-k", &[]);
        assert_eq!(p.text(), "x");
        // prompt_key takes every key: the rest is ignored rather than passed on
        for k in ["F1", "M-+", "PPage", "Tab", "C-o"] {
            assert_eq!(p.key(k, &[]), PromptKey::Handled, "{k}");
        }
        assert_eq!(p.text(), "x");
        // C-u empties the whole input
        p.key("a", &[]);
        p.key("Left", &[]);
        assert_eq!(p.key("C-u", &[]), PromptKey::Changed);
        assert_eq!(p.text(), "");
        assert_eq!(p.key("Enter", &[]), PromptKey::Done("".into()));
        assert_eq!(p.key("Escape", &[]), PromptKey::Cancel);
        let mut s = PanePrompt { single: true, ..PanePrompt::new("Kill? ", "") };
        assert_eq!(s.key("y", &[]), PromptKey::Done("y".into()));
        assert_eq!(s.key("F1", &[]), PromptKey::Handled);
        assert_eq!(s.key("BSpace", &[]), PromptKey::Cancel);
    }

    #[test]
    fn history_up_and_down() {
        let h = vec!["old".to_string(), "new".to_string()];
        let mut p = PanePrompt::new("(filter) ", "");
        assert_eq!(p.key("Up", &h), PromptKey::Changed);
        assert_eq!(p.text(), "new");
        p.key("Up", &h);
        assert_eq!(p.text(), "old");
        assert_eq!(p.key("Up", &h), PromptKey::Handled);
        p.key("Down", &h);
        assert_eq!(p.text(), "new");
        p.key("Down", &h);
        assert_eq!(p.text(), "");
        // with no history Down still empties the input (prompt_down_history returns "")
        let mut p = PanePrompt::new("(filter) ", "abc");
        assert_eq!(p.key("Down", &[]), PromptKey::Changed);
        assert_eq!(p.text(), "");
    }

    #[test]
    fn incremental_and_numeric() {
        let mut p = PanePrompt { incremental: true, editarrows: true, ..PanePrompt::new("(search) ", "ab") };
        assert_eq!(p.key("Up", &[]), PromptKey::Move);
        assert_eq!(p.key("NPage", &[]), PromptKey::Move);
        // with EDITARROWS the arrows move the cursor without changing the input
        assert_eq!(p.key("Left", &[]), PromptKey::Handled);
        assert_eq!(p.key("Home", &[]), PromptKey::Handled);
        assert_eq!(p.key("End", &[]), PromptKey::Handled);
        assert_eq!(p.key("C-a", &[]), PromptKey::Handled);
        assert_eq!(p.cursor, 0);
        assert_eq!(p.key("x", &[]), PromptKey::Changed);
        let mut n = PanePrompt { numeric: true, ..PanePrompt::new("num ", "") };
        assert_eq!(n.key("5", &[]), PromptKey::Changed);
        assert_eq!(n.key("b", &[]), PromptKey::Numeric("5".into()));
        assert_eq!(n.key("Enter", &[]), PromptKey::Numeric("5".into()));
        assert_eq!(flags_string(false, true, false, false, false, false, true, false, false), "NUMERIC,ISPANE");
        assert_eq!(flags_string(false, false, true, true, false, false, false, true, true), "INCREMENTAL,NOFORMAT,ISMODE,EDITARROWS");
    }
}
