/// A parser for terminal output which produces an in-memory representation of
/// the terminal contents.
pub struct Parser<CB: crate::Callbacks = ()> {
    parser: vte::Parser,
    string: alloc::vec::Vec<u8>,
    string_kind: Option<u8>,
    string_overflow: bool,
    escape: bool,
    dcs_handler: bool,
    dcs_ignore: bool,
    input_limit: usize,
    screen: crate::screen::WrappedScreen<CB>,
}

impl Parser {
    /// The default number of rows used when creating a parser with
    /// [`Parser::default`].
    ///
    /// The value of this constant is 24.
    // `unwrap` is not `const` in this crate's MSRV.
    pub const DEFAULT_ROWS: core::num::NonZeroU16 =
        match core::num::NonZeroU16::new(24) {
            Some(n) => n,
            None => unreachable!(),
        };

    /// The default number of columns used when creating a parser with
    /// [`Parser::default`].
    ///
    /// The value of this constant is 80.
    pub const DEFAULT_COLS: core::num::NonZeroU16 =
        match core::num::NonZeroU16::new(80) {
            Some(n) => n,
            None => unreachable!(),
        };

    /// Creates a new terminal parser of the given size and with the given
    /// amount of scrollback.
    #[must_use]
    pub fn new(
        rows: core::num::NonZeroU16,
        cols: core::num::NonZeroU16,
        scrollback_capacity: usize,
    ) -> Self {
        Self::new_with_callbacks(rows, cols, scrollback_capacity, ())
    }
}

impl<CB: crate::Callbacks> Parser<CB> {
    /// Creates a new terminal parser of the given size and with the given
    /// amount of scrollback. Terminal events will be reported via method
    /// calls on the provided [`Callbacks`](crate::Callbacks) implementation.
    pub fn new_with_callbacks(
        rows: core::num::NonZeroU16,
        cols: core::num::NonZeroU16,
        scrollback_capacity: usize,
        callbacks: CB,
    ) -> Self {
        let screen = crate::screen::Screen::new(
            crate::grid::Size { rows, cols },
            scrollback_capacity,
        );
        Self {
            parser: vte::Parser::new(),
            string: alloc::vec::Vec::new(), string_kind: None, string_overflow: false,
            escape: false, dcs_handler: false, dcs_ignore: false, input_limit: 1048575,
            screen: crate::screen::WrappedScreen { screen, callbacks },
        }
    }

    /// Processes the contents of the given byte string, and updates the
    /// in-memory terminal state.
    pub fn process(&mut self, bytes: &[u8]) {
        // Control strings are collected here, as tmux's input.c does (vte's std OSC buffer has
        // no limit and its string rules differ). OSC, APC, ESC k and SOS/PM end on BEL (OSC
        // only), CAN, SUB or any ESC, and are dispatched however they end; C0 bytes inside are
        // dropped. A DCS can be cancelled until its final byte; after that it ends only on
        // ESC \ and ESC x keeps just x, so DCS tmux; payloads double their ESC bytes.
        for &b in bytes {
            if let Some(kind) = self.string_kind {
                if kind == b'P' && self.dcs_handler {
                    if self.escape {
                        self.escape = false;
                        if b == b'\\' {
                            if !self.string_overflow {
                                self.screen.callbacks.dcs(&mut self.screen.screen, &self.string);
                            }
                            self.end_string();
                            continue;
                        }
                        self.string_push(b);
                    } else if b == 0x1b {
                        self.escape = true;
                    } else {
                        self.string_push(b);
                    }
                    continue;
                }
                match b {
                    0x18 | 0x1a | 0x1b => {
                        self.finish_string(kind, false);
                        if b == 0x1b { self.escape = true; }
                    }
                    0x07 if kind == b']' => self.finish_string(kind, true),
                    0x00..=0x1f => {}
                    _ if kind == b'P' => {
                        // parameters and intermediates, then the final byte starts the data
                        if b >= 0x7f || self.dcs_ignore { continue; }
                        let interm = self.string.iter().any(|c| (0x20..=0x2f).contains(c));
                        if b == b':' || (b'<'..=b'?').contains(&b) && !self.string.is_empty()
                            || (0x30..=0x3f).contains(&b) && interm {
                            self.dcs_ignore = true;
                        }
                        self.string_push(b);
                        if (0x40..=0x7e).contains(&b) && !self.dcs_ignore { self.dcs_handler = true; }
                    }
                    _ if kind == b'X' => {}
                    _ => self.string_push(b),
                }
                continue;
            }
            if self.escape {
                self.escape = false;
                if matches!(b, b']' | b'P' | b'_' | b'k' | b'X' | b'^') {
                    // ESC cancels whatever vte was in the middle of
                    self.parser.advance(&mut self.screen, b"\x1b\\");
                    self.string_kind = Some(if b == b'^' { b'X' } else { b });
                    continue;
                }
                self.parser.advance(&mut self.screen, &[0x1b]);
            }
            if b == 0x1b { self.escape = true; }
            else { self.parser.advance(&mut self.screen, &[b]); }
        }
    }

    fn string_push(&mut self, b: u8) {
        if self.string_overflow { return; }
        if self.string.len() >= self.input_limit {
            self.string.clear();
            self.string_overflow = true;
        } else {
            self.string.push(b);
        }
    }

    fn end_string(&mut self) {
        self.string.clear();
        self.string_kind = None;
        self.string_overflow = false;
        self.dcs_handler = false;
        self.dcs_ignore = false;
    }

    /// The end of an OSC, APC, ESC k or SOS/PM string (input_exit_osc and friends). A DCS
    /// cancelled before its data is dropped.
    fn finish_string(&mut self, kind: u8, bel: bool) {
        if !self.string_overflow {
            let s = core::mem::take(&mut self.string);
            match kind {
                b']' => self.osc(&s, bel),
                b'_' => {
                    if let Some(t) = clean_name(&s) {
                        self.screen.callbacks.set_window_title(&mut self.screen.screen, &t);
                    }
                }
                b'k' => {
                    if utf8_isvalid(&s) {
                        let n = clean_name(&s).unwrap_or_default();
                        self.screen.callbacks.rename_window(&mut self.screen.screen, &n);
                    }
                }
                _ => {}
            }
        }
        self.end_string();
    }

    fn osc(&mut self, s: &[u8], bel: bool) {
        // input_exit_osc: a number, then ';' or the end
        let digits = s.iter().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 || !matches!(s.get(digits), None | Some(b';')) { return; }
        if matches!(&s[..digits], b"0" | b"2") {
            if let Some(t) = clean_name(s.get(digits + 1..).unwrap_or_default()) {
                self.screen.callbacks.set_window_title(&mut self.screen.screen, &t);
            }
            return;
        }
        self.parser.advance(&mut self.screen, b"\x1b]");
        self.parser.advance(&mut self.screen, s);
        self.parser.advance(&mut self.screen, if bel { b"\x07" } else { b"\x1b\\" });
    }

    /// input-buffer-size: tmux grows its string buffer from 32 bytes by doubling and discards
    /// the string once the doubled size passes the limit, so the longest string kept is a power
    /// of two less one.
    pub fn set_input_buffer_size(&mut self, limit: usize) {
        let mut space = 32usize;
        while space.saturating_mul(2) <= limit { space *= 2; }
        self.input_limit = space - 1;
    }

    /// Resizes the terminal.
    ///
    /// When the screen height is decreased, this function does the following:
    ///
    /// * If the cursor is not at the bottom of the screen, rows from the
    ///   bottom are discarded until the cursor is at the bottom (or until the
    ///   desired height is reached, whichever is earlier).
    /// * If the screen is still too tall after that, rows from the top of the
    ///   screen are pushed into scrollback. If this would cause scrollback to
    ///   exceed the maximum size, rows from the top of scrollback are
    ///   discarded, and the [`on_scroll`] callback is called for each of them.
    ///
    /// When the screen height is increased, this function does the following:
    ///
    /// * Rows from the bottom of scrollback are pushed to the top of the
    ///   screen and removed from scrollback, until the desired height is
    ///   reached.[^1]
    /// * If, after pushing all scrollback to the top, the screen is still too
    ///   short, blank rows are added to the bottom of the screen until the
    ///   desired height is reached.
    ///
    /// When the screen width is changed, each row is naively truncated or
    /// extended with blank cells as necessary. This does not match the
    /// behavior of most terminals and may be changed in a future
    /// SemVer-incompatible version.
    ///
    /// [^1]: The parser tracks how many rows of scrollback there *would* be if
    /// the scrollback capacity were unbounded. For the purposes of determining
    /// how many rows should be pushed to the top vs. the bottom, this
    /// *theoretical scrollback length* is used rather than the actual length
    /// of the scrollback buffer, which is capped at `scrollback_capacity`.
    /// Assuming a sufficiently large change in height, once the real
    /// scrollback buffer has been emptied, blank rows will continue to be
    /// pushed to the top of the screen until the theoretical scrollback length
    /// is reached, and only then will blank rows be added to the bottom until
    /// the desired height is reached. This behavior is intended to more
    /// closely match the behavior of a real terminal emulator that this parser
    /// might be mirroring.
    ///
    /// [`on_scroll`]: crate::Callbacks::on_scroll
    pub fn set_size(
        &mut self,
        rows: core::num::NonZeroU16,
        cols: core::num::NonZeroU16,
    ) {
        self.screen.set_size_with_cb(rows, cols);
    }

    /// Returns a reference to a [`Screen`](crate::Screen) object containing
    /// the terminal state.
    #[must_use]
    pub fn screen(&self) -> &crate::Screen {
        &self.screen.screen
    }

    /// Returns a mutable reference to a [`Screen`](crate::Screen) object
    /// containing the terminal state.
    #[must_use]
    pub fn screen_mut(&mut self) -> &mut crate::Screen {
        &mut self.screen.screen
    }

    /// Returns a reference to the [`Callbacks`](crate::Callbacks) state object
    /// passed into the constructor.
    pub fn callbacks(&self) -> &CB {
        &self.screen.callbacks
    }

    /// Returns a mutable reference to the [`Callbacks`](crate::Callbacks)
    /// state object passed into the constructor.
    pub fn callbacks_mut(&mut self) -> &mut CB {
        &mut self.screen.callbacks
    }

    /// Returns mutable references to both the [screen](Self::screen_mut) and
    /// [callbacks](Self::callbacks_mut).
    pub fn screen_and_callbacks_mut(
        &mut self,
    ) -> (&mut crate::Screen, &mut CB) {
        (&mut self.screen.screen, &mut self.screen.callbacks)
    }
}

/// tmux's utf8_isvalid: printable ASCII and complete UTF-8 characters only.
fn utf8_isvalid(s: &[u8]) -> bool {
    core::str::from_utf8(s).is_ok_and(|t| t.chars().all(|c| !c.is_ascii() || (' '..='~').contains(&c)))
}

/// tmux's clean_name for untrusted names: None unless valid, "#(" becomes "_(" and a
/// backslash is escaped as vis(3) does.
fn clean_name(s: &[u8]) -> Option<alloc::vec::Vec<u8>> {
    if !utf8_isvalid(s) { return None; }
    let mut out = alloc::vec::Vec::with_capacity(s.len());
    for (i, &c) in s.iter().enumerate() {
        match c {
            b'#' if s.get(i + 1) == Some(&b'(') => out.push(b'_'),
            b'\\' => out.extend_from_slice(b"\\\\"),
            _ => out.push(c),
        }
    }
    Some(out)
}

impl Default for Parser {
    /// Returns a parser with dimensions 80x24 and no scrollback.
    fn default() -> Self {
        Self::new(Self::DEFAULT_ROWS, Self::DEFAULT_COLS, 0)
    }
}

#[cfg(feature = "std")]
impl std::io::Write for Parser {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.process(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(feature = "embedded-io")]
impl embedded_io::ErrorType for Parser {
    type Error = core::convert::Infallible;
}

#[cfg(feature = "embedded-io")]
impl embedded_io::Write for Parser {
    fn write(
        &mut self,
        buf: &[u8],
    ) -> core::result::Result<usize, Self::Error> {
        self.process(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> core::result::Result<(), Self::Error> {
        Ok(())
    }
}

#[cfg(feature = "embedded-io")]
impl embedded_io::WriteReady for Parser {
    fn write_ready(&mut self) -> core::result::Result<bool, Self::Error> {
        Ok(true)
    }
}
