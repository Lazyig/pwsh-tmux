use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

// chosen to fill the cell struct to 38 bytes (the attributes hold tmux's underline
// colour and styles, overline, blink, hidden, strikethrough and the DEC charset)
const CONTENT_BYTES: usize = 23;

const IS_WIDE: u8 = 0b1000_0000;
const IS_WIDE_CONTINUATION: u8 = 0b0100_0000;
const LEN_BITS: u8 = 0b0001_1111;

/// Represents a single terminal cell.
#[derive(Clone, Debug, Eq)]
pub struct Cell {
    contents: [u8; CONTENT_BYTES],
    len: u8,
    attrs: crate::attrs::Attrs,
}
const _: () = assert!(core::mem::size_of::<Cell>() == 38);

// Immutable, default-style ASCII cells can be shared by all history rows.
// Index zero denotes an empty cell, distinct from a cell containing a space.
pub(crate) static ASCII_CELLS: [Cell; 128] = {
    let mut cells = [const { Cell::new() }; 128];
    let mut i = 1;
    while i < cells.len() {
        cells[i].contents[0] = i as u8;
        cells[i].len = 1;
        i += 1;
    }
    cells
};

impl PartialEq<Self> for Cell {
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len {
            return false;
        }
        if self.attrs != other.attrs {
            return false;
        }
        let len = self.len();
        self.contents[..len] == other.contents[..len]
    }
}

impl Cell {
    pub(crate) const DEFAULT: Self = Self::new();

    pub(crate) const fn new() -> Self {
        Self {
            contents: [0; CONTENT_BYTES],
            len: 0,
            attrs: crate::attrs::Attrs::new(),
        }
    }

    pub(crate) fn ascii_index(&self) -> Option<u8> {
        if self.attrs != crate::attrs::Attrs::new() {
            return None;
        }
        match self.len {
            0 => Some(0),
            1 if self.contents[0] > 0 && self.contents[0] < 128 => {
                Some(self.contents[0])
            }
            _ => None,
        }
    }

    fn len(&self) -> usize {
        usize::from(self.len & LEN_BITS)
    }

    /// A hash of everything `eq` compares, for looking cells up in a row's palette.
    pub(crate) fn palette_hash(&self) -> u32 {
        const PRIME: u32 = 0x0100_0193;
        let color = |c: crate::Color| match c {
            crate::Color::Default => 0,
            crate::Color::Idx(i) => 0x100 | u32::from(i),
            crate::Color::Palette(i) => 0x200 | u32::from(i),
            crate::Color::Rgb(r, g, b) => 0x0400_0000 | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
        };
        let mut h = 0x811c_9dc5 ^ u32::from(self.len);
        for &b in &self.contents[..self.len()] {
            h = (h ^ u32::from(b)).wrapping_mul(PRIME);
        }
        let a = self.attrs;
        for v in [color(a.fgcolor), color(a.bgcolor), color(a.ulcolor), u32::from(a.mode)] {
            h = (h ^ v).wrapping_mul(PRIME);
        }
        h ^ (h >> 16) ^ (h >> 8)
    }

    pub(crate) fn set(&mut self, c: char, a: crate::attrs::Attrs) {
        self.len = 0;
        self.append_char(0, c);
        // if we're setting a single character, we can just look at that one
        // character to determine the cell width
        self.set_wide(c.width().unwrap_or(1) > 1);
        self.attrs = a;
    }

    pub(crate) fn append(&mut self, c: char) {
        let len = self.len();
        if len >= CONTENT_BYTES - 4 {
            return;
        }
        if len == 0 {
            self.contents[0] = b' ';
            self.len += 1;
        }

        // we already checked that we have space for another codepoint
        self.append_char(self.len(), c);
    }

    #[cfg(test)]
    pub(crate) fn set_contents(&mut self, s: &str, a: crate::attrs::Attrs) {
        let mut chars = s.chars();
        let Some(c) = chars.next() else {
            return;
        };
        self.set(c, a);
        for c in chars {
            self.append(c);
        }
    }

    // Writes bytes representing c at start
    // Requires caller to verify start <= CODEPOINTS_IN_CELL * 4
    fn append_char(&mut self, start: usize, c: char) {
        c.encode_utf8(&mut self.contents[start..]);
        self.len += u8::try_from(c.len_utf8()).unwrap();
        // if we're appending characters, we have to account for emoji
        // presentation being requested, which can turn a base character that
        // is otherwise considered to be narrow into a wide character
        if c == '\u{FE0F}' {
            self.set_wide(self.contents().width() > 1);
        }
    }

    pub(crate) fn clear(&mut self, attrs: crate::attrs::Attrs) {
        self.len = 0;
        self.attrs = attrs;
    }

    /// Returns the text contents of the cell.
    ///
    /// Can include multiple unicode characters if combining characters are
    /// used, but will contain at most one character with a non-zero character
    /// width.
    // Since contents has been constructed by appending chars encoded as UTF-8 it will be valid UTF-8
    #[allow(clippy::missing_panics_doc)]
    #[must_use]
    pub fn contents(&self) -> &str {
        core::str::from_utf8(&self.contents[..self.len()]).unwrap()
    }

    /// Returns whether the cell contains any text data.
    #[must_use]
    pub fn has_contents(&self) -> bool {
        self.len() > 0
    }

    /// Returns whether the text data in the cell represents a wide character.
    #[must_use]
    pub fn is_wide(&self) -> bool {
        self.len & IS_WIDE != 0
    }

    /// Returns whether the cell contains the second half of a wide character
    /// (in other words, whether the previous cell in the row contains a wide
    /// character)
    #[must_use]
    pub fn is_wide_continuation(&self) -> bool {
        self.len & IS_WIDE_CONTINUATION != 0
    }

    pub(crate) fn set_wide(&mut self, wide: bool) {
        if wide {
            self.len |= IS_WIDE;
        } else {
            self.len &= !IS_WIDE;
        }
    }

    pub(crate) fn set_wide_continuation(&mut self, wide: bool) {
        if wide {
            self.len |= IS_WIDE_CONTINUATION;
        } else {
            self.len &= !IS_WIDE_CONTINUATION;
        }
    }

    pub(crate) fn attrs(&self) -> &crate::attrs::Attrs {
        &self.attrs
    }

    /// Returns the foreground color of the cell.
    #[must_use]
    pub fn fgcolor(&self) -> crate::Color {
        self.attrs.fgcolor
    }

    /// Returns the background color of the cell.
    #[must_use]
    pub fn bgcolor(&self) -> crate::Color {
        self.attrs.bgcolor
    }

    /// Returns whether the cell should be rendered with the bold text
    /// attribute.
    #[must_use]
    pub fn bold(&self) -> bool {
        self.attrs.bold()
    }

    /// Returns whether the cell should be rendered with the dim text
    /// attribute.
    #[must_use]
    pub fn dim(&self) -> bool {
        self.attrs.dim()
    }

    /// Returns whether the cell should be rendered with the italic text
    /// attribute.
    #[must_use]
    pub fn italic(&self) -> bool {
        self.attrs.italic()
    }

    /// Returns whether the cell should be rendered with the underlined text
    /// attribute.
    #[must_use]
    pub fn underline(&self) -> bool {
        self.attrs.underline()
    }

    /// Returns whether the cell should be rendered with the inverse text
    /// attribute.
    #[must_use]
    pub fn inverse(&self) -> bool {
        self.attrs.inverse()
    }

    /// The underline style: 0 none, 1 single, 2 double, 3 curly, 4 dotted, 5 dashed.
    #[must_use]
    pub fn underline_style(&self) -> u8 {
        self.attrs.underline_style()
    }

    /// The underline colour (SGR 58).
    #[must_use]
    pub fn underline_color(&self) -> crate::Color {
        self.attrs.ulcolor
    }

    /// Returns whether the cell blinks (SGR 5).
    #[must_use]
    pub fn blink(&self) -> bool {
        self.attrs.blink()
    }

    /// Returns whether the cell is hidden (SGR 8).
    #[must_use]
    pub fn hidden(&self) -> bool {
        self.attrs.hidden()
    }

    /// Returns whether the cell is struck through (SGR 9).
    #[must_use]
    pub fn strikethrough(&self) -> bool {
        self.attrs.strikethrough()
    }

    /// Returns whether the cell has an overline (SGR 53).
    #[must_use]
    pub fn overline(&self) -> bool {
        self.attrs.overline()
    }

    /// Whether the cell is drawn from the DEC special graphics set (GRID_ATTR_CHARSET).
    #[must_use]
    pub fn charset(&self) -> bool {
        self.attrs.charset()
    }
}
