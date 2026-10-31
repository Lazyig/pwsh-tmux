//! Bound accumulated replies before transport framing or asynchronous delivery.
use std::ops::Deref;

pub const MAX_REPLY: usize = 4 << 20;

#[derive(Clone, Default)]
pub struct Output {
    text: String,
    overflowed: bool,
}

impl Output {
    pub fn push_str(&mut self, text: &str) {
        if self.overflowed { return; }
        if self.text.len().saturating_add(text.len()) > MAX_REPLY {
            self.text.clear();
            self.overflowed = true;
            return;
        }
        self.text.push_str(text);
    }

    pub fn push(&mut self, c: char) {
        self.push_str(c.encode_utf8(&mut [0; 4]));
    }

    pub fn overflowed(&self) -> bool { self.overflowed }
}

impl Deref for Output {
    type Target = str;
    fn deref(&self) -> &str { &self.text }
}
