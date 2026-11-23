/// A parser for terminal output which produces an in-memory representation of
/// the terminal contents.
pub struct Parser<CB: crate::Callbacks = ()> {
    parser: vte::Parser,
    string: alloc::vec::Vec<u8>,
    string_kind: Option<u8>,
    string_overflow: bool,
    escape: bool,
    /// The last byte given to vte was the `[` of ESC [.
    csi_start: bool,
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
            escape: false, csi_start: false, dcs_handler: false, dcs_ignore: false, input_limit: 1048575,
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
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            i += 1;
            if self.string_kind.is_none() && !self.escape && !self.csi_start && starts_run(b) {
                // Outside a string and not right after ESC or ESC [, this loop only hands
                // each byte to vte, so a run up to the next ESC goes to vte in one call:
                // vte makes the same calls in the same order. Two bytes keep going one at a
                // time because vte's buffer for a character split over calls differs from
                // its whole-string path:
                // - 0xC2, the lead byte of U+0080..U+009F: vte prints such a C1 character
                //   when it completes it in that buffer (as one byte at a time always does)
                //   but executes it when it decodes it whole, so a run ends before 0xC2.
                // - UTF-8 continuation bytes (starts_run): a character completed in that
                //   buffer skips every byte copied into it after the character, so a run
                //   never starts where an earlier call may have left part of a character.
                let end = bytes[i..].iter().position(|&c| c == 0x1b || c == 0xc2).map_or(bytes.len(), |p| i + p);
                self.parser.advance(&mut self.screen, &bytes[i - 1..end]);
                i = end;
                continue;
            }
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
            let mut after_escape = false;
            if self.escape {
                self.escape = false;
                if matches!(b, b']' | b'P' | b'_' | b'k' | b'X' | b'^') {
                    // ESC cancels whatever vte was in the middle of
                    self.parser.advance(&mut self.screen, b"\x1b\\");
                    self.string_kind = Some(if b == b'^' { b'X' } else { b });
                    self.csi_start = false;
                    continue;
                }
                self.parser.advance(&mut self.screen, &[0x1b]);
                after_escape = true;
            }
            if b == 0x1b { self.escape = true; self.csi_start = false; }
            else {
                // vte gives `CSI m` the parameter 0 it gives `CSI 0 m`; tell them apart here
                self.screen.screen.empty_sgr = self.csi_start && b == b'm';
                self.csi_start = after_escape && b == b'[';
                self.parser.advance(&mut self.screen, &[b]);
                self.screen.screen.empty_sgr = false;
            }
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

/// Whether `Parser::process` may hand vte a run of bytes starting with `b` in one call
/// (see there): not ESC, not 0xC2 and not a UTF-8 continuation byte.
fn starts_run(b: u8) -> bool {
    b != 0x1b && b != 0xc2 && !(0x80..=0xbf).contains(&b)
}

/// tmux's utf8_isvalid: printable ASCII and complete UTF-8 characters only.
fn utf8_isvalid(s: &[u8]) -> bool {
    core::str::from_utf8(s).is_ok_and(|t| t.chars().all(|c| !c.is_ascii() || (' '..='~').contains(&c)))
}

/// tmux's clean_name for untrusted names: None unless valid, "#(" becomes "_(" and a
/// backslash is escaped as vis(3) does. Not on Windows, where a backslash separates the
/// parts of a path and every console title (ConPTY sends the program's path) has some.
fn clean_name(s: &[u8]) -> Option<alloc::vec::Vec<u8>> {
    if !utf8_isvalid(s) { return None; }
    let mut out = alloc::vec::Vec::with_capacity(s.len());
    for (i, &c) in s.iter().enumerate() {
        match c {
            b'#' if s.get(i + 1) == Some(&b'(') => out.push(b'_'),
            b'\\' if cfg!(not(windows)) => out.extend_from_slice(b"\\\\"),
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

#[cfg(test)]
mod tests {
    use super::Parser;
    use core::num::NonZeroU16;

    fn p30(input: &str) -> Parser {
        let mut p = Parser::new(NonZeroU16::new(8).unwrap(), NonZeroU16::new(30).unwrap(), 0);
        p.process(input.as_bytes());
        p
    }

    /// Callbacks with the cursor at the time of each call.
    #[derive(Default)]
    struct Log(alloc::vec::Vec<alloc::string::String>);
    impl crate::Callbacks for Log {
        fn audible_bell(&mut self, s: &mut crate::Screen) { self.0.push(alloc::format!("bell {:?}", s.cursor_position())); }
        fn set_window_title(&mut self, s: &mut crate::Screen, t: &[u8]) { self.0.push(alloc::format!("title {t:?} {:?}", s.cursor_position())); }
        fn unhandled_control(&mut self, s: &mut crate::Screen, b: u8) { self.0.push(alloc::format!("control {b} {:?}", s.cursor_position())); }
        fn unhandled_csi(&mut self, s: &mut crate::Screen, i1: Option<u8>, i2: Option<u8>, params: &[&[u16]], c: char) {
            self.0.push(alloc::format!("csi {i1:?} {i2:?} {params:?} {c} {:?}", s.cursor_position()));
        }
        fn on_scroll(&mut self, row: crate::capture::RowContents<'_>, alt: bool) {
            self.0.push(alloc::format!("scroll {:?} {alt}", row.0));
        }
    }

    /// The whole state of a parser fed `chunks` one after the other.
    fn fed(chunks: &[&[u8]]) -> alloc::string::String {
        let mut p = Parser::new_with_callbacks(NonZeroU16::new(4).unwrap(), NonZeroU16::new(10).unwrap(), 20, Log::default());
        for chunk in chunks { p.process(chunk); }
        alloc::format!("{:?}\n{:?}\n{:?}", p.screen(), p.screen().contents_formatted(), p.callbacks().0)
    }

    #[test]
    fn runs_given_to_vte_at_once_match_one_byte_at_a_time() {
        // Parser::process passes runs of bytes to vte in one call; one byte per call is
        // how it worked before, so every split of the input must give the same state
        let inputs: [&[u8]; 14] = [
            b"plain text that wraps over the edge of the row and scrolls\r\nmore\x07",
            "wide \u{3042}\u{3044} and e\u{301} \u{1F1F0}\u{1F1F7} \u{1F44D}\u{1F3FD} done".as_bytes(),
            // C1 controls sent as UTF-8 are printed; lone C1 bytes are executed
            b"a\xc2\x9bb\xc2\x85c\x9bd\x85e\xc2\x80\xc2\x9f",
            b"\xc2Z\xc2\xc2\x9b\xc2",
            // invalid and cut off UTF-8
            b"x\xe2\x82y\xf0\x9f\x98z\xff\xfe\x80\xbf\xe0\x80\xed\xa0\x80\xf4\x90\x80\x80w",
            b"\xe2\x82\xac\xe2\x82\x1b[1m\xe2\x82\xac\xf0\x9f\x98\x80\xe2",
            // an empty SGR is not SGR 0; CSI after a run
            b"\x1b[1;4mbold\x1b[mplain\x1b[;mx\x1b[0my\x1b[38;2;1;2;3mrgb\x1b[48;5;9mbg",
            b"\x1b[2;3r\x1b[3;1Hab\x1bDcd\x1bMef\x1b[?6h\x1b[Hgh\x1b[r",
            b"\x1b(0lqk\x0exy\x0f\x1b(Bab\x1b[4hins\x1b[4l\x1b[?7lnowrap0123456789\x1b[?7h",
            // strings: OSC with BEL and ST, APC, DCS, PM, ESC k, CAN and SUB inside
            b"t\x1b]2;title\x07u\x1b]0;x\x1b\\v\x1b_apc\x1b\\w\x1bPq#0\x1b\\x\x1b^pm\x18y\x1bkname\x1b\\z\x1b]2;a\x1ab",
            b"\x1b[?1049halt screen\r\n\r\n\r\n\r\nscrolls\x1b[?1049lback",
            b"\x1b[1;5Hc\x1b[2Ja\x1b[3Jtab\tx\x08\x08y\x1b[5Xz\x1b[2@\x1b[P\x1b[b",
            b"\x1b[5n\x1b[c\x1b[>c\x1b[=3h\x1b[?2004h\x1b[12;34;56t\x1b[999;1H\x1b[J\x1b#8",
            "\u{3042}\u{3042}\u{3042}\u{3042}\u{3042}x\u{3042}\x1b[1;10Hab\x1b[1;9H\u{3042}y\x1b[1;2Hz".as_bytes(),
        ];
        for input in inputs {
            let bytes: alloc::vec::Vec<&[u8]> = input.chunks(1).collect();
            let want = fed(&bytes);
            assert_eq!(fed(&[input]), want, "{input:?}");
            for at in 0..=input.len() {
                assert_eq!(fed(&[&input[..at], &input[at..]]), want, "{input:?} at {at}");
            }
        }
    }

    #[test]
    fn c1_controls_sent_as_utf8_survive_runs_and_splits() {
        // vte executes U+0080..U+009F it decodes whole but prints one completed from bytes
        // split over calls; tmux keeps them as characters, as one byte at a time did
        let p = p30("a\u{9b}b");
        assert_eq!((p.screen().cell(0, 0).unwrap().contents(), p.screen().cell(0, 1).unwrap().contents()), ("a\u{9b}", "b"));
        // a lone C1 byte is an invalid UTF-8 byte
        let mut p = p30("");
        p.process(b"a\x9bb");
        assert_eq!(p.screen().contents(), "a\u{fffd}b");
        // vte drops the bytes after a character completed from a split, so the byte after
        // the split goes on its own
        let mut p = p30("");
        p.process(b"a\xc2");
        p.process(b"\x9bZ\xe2");
        p.process(b"\x82\xac!");
        assert_eq!(p.screen().contents(), "a\u{9b}Z\u{20ac}!");
    }
}
