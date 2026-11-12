//! Scoped width rules: format expansion and rendering use the owning server's options.
use std::cell::RefCell;
use unicode_width::UnicodeWidthChar;

type Rules = (Vec<(u32, u32, u16)>, bool);
thread_local! { static RULES: RefCell<Rules> = const { RefCell::new((Vec::new(), false)) }; }

pub struct Guard(Rules);
impl Drop for Guard { fn drop(&mut self) { RULES.with(|r| *r.borrow_mut() = std::mem::take(&mut self.0)); } }
pub fn enter(ranges: Vec<(u32, u32, u16)>, variation: bool) -> Guard {
    Guard(RULES.with(|r| r.replace((ranges, variation))))
}
pub fn char_width(ch: char) -> Option<usize> {
    RULES.with(|r| r.borrow().0.iter().rev().find(|(a, b, _)| (*a..=*b).contains(&(ch as u32))).map(|(_, _, w)| *w as usize))
        .or_else(|| ch.width())
}
pub fn str_width(text: &str) -> usize {
    let mut width = unicode_width::UnicodeWidthStr::width(text) as isize;
    let mut previous = None;
    for ch in text.chars() {
        width += char_width(ch).unwrap_or(0) as isize - ch.width().unwrap_or(0) as isize;
        if ch == '\u{fe0f}' && RULES.with(|r| r.borrow().1) {
            if let Some(base) = previous {
                let cluster = format!("{base}{ch}");
                let old = unicode_width::UnicodeWidthStr::width(cluster.as_str());
                width += 2isize.saturating_sub(old as isize).max(0);
            }
        } else if char_width(ch).unwrap_or(0) > 0 { previous = Some(ch); }
    }
    width.max(0) as usize
}
