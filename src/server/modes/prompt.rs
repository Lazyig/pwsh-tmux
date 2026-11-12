//! A prompt drawn inside a pane rather than on the status line (tmux 3.8 window_pane_set_prompt
//! and the prompts of the tree and switch modes): one line of input with a label.

use crate::cells::{Cell, Style};
use unicode_width::UnicodeWidthChar;

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
}

#[derive(Debug, PartialEq, Eq)]
pub enum PromptKey {
    /// The key edited the input (or moved the cursor).
    Handled,
    /// Enter, or the key of a single-key prompt: the input is done.
    Done(String),
    /// Escape, C-c or C-g.
    Cancel,
    /// Not a prompt key (the owner may use it, as PROMPT_KEY_NOT_HANDLED).
    NotHandled,
}

impl PanePrompt {
    pub fn new(label: &str, input: &str) -> PanePrompt {
        let input: Vec<char> = input.chars().collect();
        PanePrompt { label: label.to_string(), cursor: input.len(), input, ..PanePrompt::default() }
    }

    pub fn text(&self) -> String {
        self.input.iter().collect()
    }

    /// Edit with one key (emacs status-keys bindings).
    pub fn key(&mut self, name: &str) -> PromptKey {
        match name {
            "Enter" | "C-m" | "C-j" => return PromptKey::Done(self.text()),
            "Escape" | "C-c" | "C-g" | "C-[" => return PromptKey::Cancel,
            _ => {}
        }
        if self.single {
            let text = match name {
                "Space" => " ".to_string(),
                n if n.chars().count() == 1 => n.to_string(),
                _ => return PromptKey::NotHandled,
            };
            return PromptKey::Done(text);
        }
        match name {
            "BSpace" | "C-h" if self.bspace_exit && self.input.is_empty() => return PromptKey::Cancel,
            "BSpace" | "C-h" => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.input.remove(self.cursor);
                }
            }
            "Delete" | "DC" | "C-d" => {
                if self.cursor < self.input.len() {
                    self.input.remove(self.cursor);
                }
            }
            "Left" | "C-b" => self.cursor = self.cursor.saturating_sub(1),
            "Right" | "C-f" => self.cursor = (self.cursor + 1).min(self.input.len()),
            "Home" | "C-a" => self.cursor = 0,
            "End" | "C-e" => self.cursor = self.input.len(),
            "C-k" => self.input.truncate(self.cursor),
            "C-u" => {
                self.input.drain(..self.cursor);
                self.cursor = 0;
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
            }
            "Space" => self.insert(' '),
            n => {
                let mut chars = n.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if !c.is_control() => {
                        if self.numeric && !c.is_ascii_digit() {
                            return PromptKey::Handled;
                        }
                        self.insert(c)
                    }
                    _ => return PromptKey::NotHandled,
                }
            }
        }
        PromptKey::Handled
    }

    fn insert(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += 1;
    }

    /// One line of `width` cells in `style` (message-style) and the cursor column. When the
    /// input does not fit, it scrolls to keep the cursor visible.
    pub fn draw(&self, width: usize, style: Style) -> (Vec<Cell>, usize) {
        let blank = Cell { text: " ".into(), style };
        let mut cells = vec![blank; width];
        let mut x = 0;
        for c in self.label.chars() {
            let w = c.width().unwrap_or(0);
            if w == 0 || x + w > width {
                if w != 0 {
                    break;
                }
                continue;
            }
            cells[x] = Cell { text: c.to_string(), style };
            x += w;
        }
        let avail = width.saturating_sub(x);
        if avail == 0 {
            return (cells, width.saturating_sub(1));
        }
        let widths: Vec<usize> = self.input.iter().map(|c| c.width().unwrap_or(0)).collect();
        let before: usize = widths[..self.cursor].iter().sum();
        // first input character shown: keep the cursor inside the area
        let mut start = 0;
        let mut skipped = 0;
        while before - skipped >= avail && start < self.input.len() {
            skipped += widths[start];
            start += 1;
        }
        let mut cx = x;
        for (i, c) in self.input.iter().enumerate().skip(start) {
            let w = widths[i];
            if x + w > width {
                break;
            }
            if i == self.cursor {
                cx = x;
            }
            if w > 0 {
                cells[x] = Cell { text: c.to_string(), style };
                if w == 2 && x + 1 < width {
                    cells[x + 1] = Cell { text: String::new(), style };
                }
            }
            x += w;
        }
        if self.cursor >= self.input.len() {
            cx = x.min(width - 1);
        }
        (cells, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_keys() {
        let mut p = PanePrompt::new("(search) ", "ab");
        assert_eq!(p.key("c"), PromptKey::Handled);
        assert_eq!(p.text(), "abc");
        p.key("Left");
        p.key("BSpace");
        assert_eq!(p.text(), "ac");
        p.key("C-a");
        p.key("x");
        assert_eq!(p.text(), "xac");
        p.key("C-k");
        assert_eq!(p.text(), "x");
        assert_eq!(p.key("Up"), PromptKey::NotHandled);
        assert_eq!(p.key("Enter"), PromptKey::Done("x".into()));
        assert_eq!(p.key("Escape"), PromptKey::Cancel);
        let mut s = PanePrompt { single: true, ..PanePrompt::new("Kill? ", "") };
        assert_eq!(s.key("y"), PromptKey::Done("y".into()));
    }

    #[test]
    fn draw_scrolls_to_cursor() {
        let p = PanePrompt::new("> ", "abcdefghij");
        let (cells, cx) = p.draw(8, Style::default());
        let text: String = cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(text, "> fghij ");
        assert_eq!(cx, 7);
        let (cells, cx) = PanePrompt::new("(search) ", "").draw(20, Style::default());
        assert_eq!(cells[0].text, "(");
        assert_eq!(cx, 9);
    }
}
