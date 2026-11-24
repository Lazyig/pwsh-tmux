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
/// codepoint-widths applies to UTF-8 characters only: tmux handles 7-bit characters directly.
/// After it comes tmux's built-in table (utf8_default_width_cache), then the Unicode width.
pub fn char_width(ch: char) -> Option<usize> {
    if ch.is_ascii() { return ch.width(); }
    rule_width(ch).or_else(|| ch.width())
}
/// The width codepoint-widths or tmux's built-in table gives a UTF-8 character.
fn rule_width(ch: char) -> Option<usize> {
    RULES.with(|r| r.borrow().0.iter().rev().find(|(a, b, _)| (*a..=*b).contains(&(ch as u32))).map(|(_, _, w)| *w as usize))
        .or_else(|| vt100::default_width(ch).map(usize::from))
}
pub fn str_width(text: &str) -> usize {
    let mut width = unicode_width::UnicodeWidthStr::width(text) as isize;
    let mut previous = None;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        let unicode = ch.width().unwrap_or(0);
        if let Some(rule) = rule_width(ch).filter(|_| !ch.is_ascii()) {
            // the string width counted a character with VS16 after it as the pair
            let counted = if chars.peek() == Some(&'\u{fe0f}') {
                unicode_width::UnicodeWidthStr::width(format!("{ch}\u{fe0f}").as_str())
            } else {
                unicode
            };
            width += rule as isize - counted as isize;
        }
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

#[cfg(test)]
mod tests {
    #[test]
    fn tmux_default_widths_count_in_formats() {
        // utf8_default_width_cache: U+261D is two columns, with or without VS16 after it
        // (the format width is the sum of the characters' widths); codepoint-widths wins
        let _outer = super::enter(Vec::new(), false);
        assert_eq!((super::str_width("a\u{261d}b"), super::str_width("\u{261d}\u{fe0f}"), super::char_width('\u{261d}')), (4, 2, Some(2)));
        assert_eq!(super::str_width("\u{261e}"), 1);
        let _inner = super::enter(vec![(0x261d, 0x261d, 1)], false);
        assert_eq!((super::str_width("a\u{261d}b"), super::str_width("\u{261d}\u{fe0f}")), (3, 1));
    }
}
