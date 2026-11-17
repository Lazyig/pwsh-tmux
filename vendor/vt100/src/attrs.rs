use crate::term::BufWrite as _;
use alloc::string::String;

/// Represents a foreground or background color for cells.
#[derive(Eq, PartialEq, Debug, Copy, Clone, Default)]
pub enum Color {
    /// The default terminal color.
    #[default]
    Default,

    /// An indexed terminal color.
    Idx(u8),

    /// Explicit palette colour (COLOUR_FLAG_256), including indices below 16.
    Palette(u8),

    /// An RGB terminal color. The parameters are (red, green, blue).
    Rgb(u8, u8, u8),
}

impl Color {
    /// Keep explicit palette identity where it differs from the ANSI colour index.
    pub fn palette(index: u8) -> Self {
        if index < 16 { Self::Palette(index) } else { Self::Idx(index) }
    }
}

const TEXT_MODE_INTENSITY: u16 = 0b0000_0011;
const TEXT_MODE_BOLD: u16 = 0b0000_0001;
const TEXT_MODE_DIM: u16 = 0b0000_0010;
const TEXT_MODE_ITALIC: u16 = 0b0000_0100;
const TEXT_MODE_UNDERLINE: u16 = 0b0000_1000;
const TEXT_MODE_INVERSE: u16 = 0b0001_0000;
const TEXT_MODE_BLINK: u16 = 0b0010_0000;
const TEXT_MODE_HIDDEN: u16 = 0b0100_0000;
const TEXT_MODE_STRIKE: u16 = 0b1000_0000;
const TEXT_MODE_OVERLINE: u16 = 0x100;
/// GRID_ATTR_CHARSET: drawn from the DEC special graphics set.
const TEXT_MODE_CHARSET: u16 = 0x200;
/// GRID_ATTR_UNDERSCORE_2 to _5: double, curly, dotted and dashed underlines.
const TEXT_MODE_UNDERLINE_STYLES: [u16; 4] = [0x400, 0x800, 0x1000, 0x2000];
const TEXT_MODE_ALL_UNDERLINE: u16 = TEXT_MODE_UNDERLINE | 0x400 | 0x800 | 0x1000 | 0x2000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attrs {
    pub fgcolor: Color,
    pub bgcolor: Color,
    /// The underline colour (SGR 58, `us` in tmux).
    pub ulcolor: Color,
    pub mode: u16,
}

impl Default for Attrs {
    fn default() -> Self {
        Self::new()
    }
}

impl Attrs {
    pub const fn new() -> Self {
        Self {
            fgcolor: Color::Default,
            bgcolor: Color::Default,
            ulcolor: Color::Default,
            mode: 0,
        }
    }

    pub fn bold(&self) -> bool {
        self.mode & TEXT_MODE_BOLD != 0
    }

    pub fn dim(&self) -> bool {
        self.mode & TEXT_MODE_DIM != 0
    }

    fn intensity(&self) -> u16 {
        self.mode & TEXT_MODE_INTENSITY
    }

    /// SGR 1: tmux keeps bold and dim apart (both can be on).
    pub fn set_bold(&mut self) {
        self.mode |= TEXT_MODE_BOLD;
    }

    pub fn set_dim(&mut self) {
        self.mode |= TEXT_MODE_DIM;
    }

    pub fn set_normal_intensity(&mut self) {
        self.mode &= !TEXT_MODE_INTENSITY;
    }

    pub fn italic(&self) -> bool {
        self.mode & TEXT_MODE_ITALIC != 0
    }

    pub fn set_italic(&mut self, italic: bool) {
        self.set_flag(TEXT_MODE_ITALIC, italic);
    }

    /// Any underline (single or one of the styles).
    pub fn underline(&self) -> bool {
        self.mode & TEXT_MODE_ALL_UNDERLINE != 0
    }

    /// 0 none, 1 single, 2 double, 3 curly, 4 dotted, 5 dashed.
    pub fn underline_style(&self) -> u8 {
        if self.mode & TEXT_MODE_UNDERLINE != 0 {
            return 1;
        }
        TEXT_MODE_UNDERLINE_STYLES.iter().position(|b| self.mode & b != 0).map_or(0, |i| i as u8 + 2)
    }

    pub fn set_underline_style(&mut self, style: u8) {
        self.mode &= !TEXT_MODE_ALL_UNDERLINE;
        match style {
            0 => {}
            1 => self.mode |= TEXT_MODE_UNDERLINE,
            n => self.mode |= TEXT_MODE_UNDERLINE_STYLES[usize::from(n.min(5) - 2)],
        }
    }

    pub fn inverse(&self) -> bool {
        self.mode & TEXT_MODE_INVERSE != 0
    }

    pub fn set_inverse(&mut self, inverse: bool) {
        self.set_flag(TEXT_MODE_INVERSE, inverse);
    }

    pub fn blink(&self) -> bool {
        self.mode & TEXT_MODE_BLINK != 0
    }

    pub fn set_blink(&mut self, on: bool) {
        self.set_flag(TEXT_MODE_BLINK, on);
    }

    pub fn hidden(&self) -> bool {
        self.mode & TEXT_MODE_HIDDEN != 0
    }

    pub fn set_hidden(&mut self, on: bool) {
        self.set_flag(TEXT_MODE_HIDDEN, on);
    }

    pub fn strikethrough(&self) -> bool {
        self.mode & TEXT_MODE_STRIKE != 0
    }

    pub fn set_strikethrough(&mut self, on: bool) {
        self.set_flag(TEXT_MODE_STRIKE, on);
    }

    pub fn overline(&self) -> bool {
        self.mode & TEXT_MODE_OVERLINE != 0
    }

    pub fn set_overline(&mut self, on: bool) {
        self.set_flag(TEXT_MODE_OVERLINE, on);
    }

    pub fn charset(&self) -> bool {
        self.mode & TEXT_MODE_CHARSET != 0
    }

    pub fn set_charset(&mut self, on: bool) {
        self.set_flag(TEXT_MODE_CHARSET, on);
    }

    fn set_flag(&mut self, bit: u16, on: bool) {
        if on {
            self.mode |= bit;
        } else {
            self.mode &= !bit;
        }
    }

    pub fn write_escape_code_diff(
        &self,
        contents: &mut String,
        other: &Self,
    ) {
        self.write_escape_code_diff_fmt(contents, other)
            .expect("writing to a String cannot fail");
    }

    pub fn write_escape_code_diff_fmt(
        &self,
        writer: &mut impl alloc::fmt::Write,
        other: &Self,
    ) -> alloc::fmt::Result {
        if self != other && self == &Self::default() {
            return crate::term::ClearAttrs.write_fmt(writer);
        }

        let attrs = crate::term::Attrs::default();

        let attrs = if self.fgcolor == other.fgcolor {
            attrs
        } else {
            attrs.fgcolor(self.fgcolor)
        };
        let attrs = if self.bgcolor == other.bgcolor {
            attrs
        } else {
            attrs.bgcolor(self.bgcolor)
        };
        let attrs = if self.intensity() == other.intensity() {
            attrs
        } else {
            attrs.intensity(match self.intensity() {
                0 => crate::term::Intensity::Normal,
                TEXT_MODE_DIM => crate::term::Intensity::Dim,
                _ => crate::term::Intensity::Bold,
            })
        };
        let attrs = if self.italic() == other.italic() {
            attrs
        } else {
            attrs.italic(self.italic())
        };
        let attrs = if self.underline() == other.underline() {
            attrs
        } else {
            attrs.underline(self.underline())
        };
        let attrs = if self.inverse() == other.inverse() {
            attrs
        } else {
            attrs.inverse(self.inverse())
        };

        attrs.write_fmt(writer)
    }
}
