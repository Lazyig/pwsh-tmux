//! This crate parses a terminal byte stream and provides an in-memory
//! representation of the rendered contents.
//!
//! # Overview
//!
//! This is essentially the terminal parser component of a graphical terminal
//! emulator pulled out into a separate crate. Although you can use this crate
//! to build a graphical terminal emulator, it also contains functionality
//! necessary for implementing terminal applications that want to run other
//! terminal applications - programs like `screen` or `tmux` for example.
//!
//! # Synopsis
//!
//! ```
//! let mut parser = vt100::Parser::default();
//!
//! let screen = parser.screen().clone();
//! parser.process(b"this text is \x1b[31mRED\x1b[m");
//! assert_eq!(
//!     parser.screen().cell(0, 13).unwrap().fgcolor(),
//!     vt100::Color::Idx(1),
//! );
//!
//! let screen = parser.screen().clone();
//! parser.process(b"\x1b[3D\x1b[32mGREEN");
//! assert_eq!(
//!     parser.screen().contents_formatted(),
//!     "\x1b[?25h\x1b[m\x1b[H\x1b[Jthis text is \x1b[32mGREEN",
//! );
//! assert_eq!(
//!     parser.screen().contents_diff(&screen),
//!     "\x1b[1;14H\x1b[32mGREEN",
//! );
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]
#![warn(clippy::cargo)]
#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::as_conversions)]
#![warn(clippy::get_unwrap)]
#![allow(clippy::cognitive_complexity)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::similar_names)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::type_complexity)]

extern crate alloc;

mod attrs;
mod callbacks;
pub mod capture;
mod cell;
mod grid;
mod parser;
mod perform;
mod row;
mod row_cells;
mod screen;
mod term;

pub use attrs::Color;
pub use callbacks::Callbacks;
pub use cell::Cell;
pub use row::{ROW_START_OUTPUT, ROW_START_PROMPT, ROW_HYPERLINK, ROW_EXTENDED, ROW_START_COMMAND, ROW_END_OUTPUT, ROW_SECOND_PROMPT};
pub use parser::Parser;
pub use screen::{
    CursorStyle, MouseProtocolEncoding, MouseProtocolMode, Screen,
};

/// One OSC 8 link, shared by its cells without increasing the cell size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hyperlink {
    /// Unique internal identity (anonymous links are distinct).
    pub inner: u64,
    /// Application supplied OSC 8 identifier.
    pub id: alloc::string::String,
    /// Identifier emitted to a client terminal.
    pub external_id: alloc::string::String,
    /// Link destination.
    pub uri: alloc::string::String,
}

/// Sparse metadata for a cell. Ordinary cells have no entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellMetadata {
    /// OSC 8 link attached to this cell.
    pub hyperlink: Option<alloc::sync::Arc<Hyperlink>>,
    /// Number of columns occupied by an original horizontal tab, or zero.
    pub tab_width: u16,
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    use core::num::NonZeroU16;
    fn parser(rows: u16, cols: u16) -> Parser {
        Parser::new(NonZeroU16::new(rows).unwrap(), NonZeroU16::new(cols).unwrap(), 100)
    }
    #[test]
    fn osc_marks_links_tabs_and_history() {
        let mut p = parser(3, 20);
        p.screen_mut().set_time(123);
        p.process(b"\x1b]133;A\x07prompt\r\n\x1b]133;C\x07a\tb\r\n\x1b]8;id=x;https://example.org\x07link\x1b]8;;\x07\r\nend");
        let s = p.screen();
        assert_eq!(s.history_row_flags(0).0 & ROW_START_PROMPT, ROW_START_PROMPT);
        assert_eq!(s.history_row_time(0), 123);
        assert_eq!(s.history_row_flags(1).0 & ROW_START_OUTPUT, ROW_START_OUTPUT);
        let row = s.history_row(1).unwrap();
        assert_eq!(row.metadata().find(|(x,_)| *x == 1).unwrap().1.tab_width, 7);
        let row = s.history_row(2).unwrap();
        let meta: alloc::vec::Vec<_> = row.metadata().collect();
        assert_eq!(meta.len(), 4);
        assert_eq!(meta[0].1.hyperlink.as_ref().unwrap().uri, "https://example.org");
        assert_eq!(meta[0].1.hyperlink.as_ref().unwrap().inner, meta[3].1.hyperlink.as_ref().unwrap().inner);
        assert_eq!(core::mem::size_of::<Cell>(), 32);
    }
    #[test]
    fn metadata_edits_reflow_and_anonymous_identity() {
        let mut p = parser(4, 16);
        p.process(b"\x1b]8;;https://a\x07ab\x1b]8;;\x07\x1b]8;;https://a\x07cd\x1b]8;;\x07\r\na\tb");
        let ids: alloc::vec::Vec<_> = p.screen().history_row(0).unwrap().metadata().map(|(_,m)| m.hyperlink.as_ref().unwrap().inner).collect();
        assert_eq!(ids[0], ids[1]); assert_ne!(ids[1], ids[2]);
        p.process(b"\x1b[1;2H\x1b[@");
        assert_eq!(p.screen().history_row(0).unwrap().metadata().map(|(x,_)| x).collect::<alloc::vec::Vec<_>>(), [0,2,3,4]);
        p.process(b"\x1b[1;3H\x1b[P");
        assert_eq!(p.screen().history_row(0).unwrap().metadata().map(|(x,_)| x).collect::<alloc::vec::Vec<_>>(), [0,2,3]);
        p.process(b"\x1b[2;4HX");
        assert!(p.screen().history_row(1).unwrap().metadata().all(|(_,m)| m.tab_width == 0));
        p.set_size(NonZeroU16::new(4).unwrap(), NonZeroU16::new(3).unwrap());
        assert_eq!(p.screen().history_row(0).unwrap().metadata().count(), 2);
        assert_eq!(p.screen().history_row(1).unwrap().metadata().count(), 1);
        p.process(b"\x1b[2J");
        for y in p.screen().scrollback_len()..p.screen().history_rows_len() { assert_eq!(p.screen().history_row(y).unwrap().metadata().count(), 0); }
    }
    #[test]
    fn tabs_keep_the_cell_link_move_whole_on_reflow_and_links_reset() {
        let tabs = |p: &Parser, y: usize| -> alloc::vec::Vec<(u16, u16)> { p.screen().history_row(y).unwrap().metadata().filter(|(_, m)| m.tab_width > 0).map(|(x, m)| (x, m.tab_width)).collect() };
        // HT over text or differently styled cells only moves the cursor
        let mut p = parser(4, 20);
        p.process(b"abcdefghij\r\tX\r\n\x1b[41m \x1b[0m\r\tY");
        assert_eq!(tabs(&p, 0), []);
        assert_eq!(tabs(&p, 1), []);
        // the tab takes the link of the cells it is written over, not the current one
        let mut p = parser(4, 20);
        p.process(b"\x1b]8;;http://a\x07 \r\tX\r\n\x1b]8;;http://a\x07        \x1b]8;;http://b\x07\r\tZ");
        assert_eq!(tabs(&p, 0), []);
        p.process(b"\x1b[H\x1b[M");
        let row = p.screen().history_row(0).unwrap();
        let m = row.cell_metadata(0).unwrap();
        assert_eq!((m.tab_width, m.hyperlink.as_ref().unwrap().uri.as_str()), (8, "http://a"));
        assert_eq!(row.cell_metadata(8).unwrap().hyperlink.as_ref().unwrap().uri, "http://b");
        // writing inside a tab turns it into spaces
        p.process(b"\x1b[1;4H-");
        assert_eq!(p.screen().history_row(0).unwrap().cell_metadata(0).map(|m| m.tab_width), Some(0));
        // an insert inside a tab keeps the part before it (capture-pane shows the tab)
        let mut p = parser(4, 30);
        p.process(b"\x1b[3gab\x1b[1Dcd\x1b[1;20H\x1bH\x1b[1;4H\tX\x1b[2D\x1b[@Y");
        assert_eq!(tabs(&p, 0), [(3, 15)]);
        // reflow (grid_reflow): a tab is one cell of its width and each of its padding
        // cells counts a column, so what follows moves on early; the tab stays a tab, a
        // split whose first cell is wider than the row leaves an empty row (tmux 3.8)
        let mut p = parser(3, 20);
        p.process(b"abcde\tf\r\nab\tcd");
        assert_eq!(tabs(&p, 0), [(5, 3)]);
        assert_eq!(tabs(&p, 1), [(2, 6)]);
        let rows = |p: &Parser, w: u16| -> alloc::vec::Vec<(alloc::string::String, bool)> {
            let s = p.screen();
            (0..s.history_rows_len()).map(|y| { let r = s.history_row(y).unwrap(); let mut t = alloc::string::String::new(); r.write_plain(&mut t, w); (t.trim_end().into(), r.wrapped()) }).collect()
        };
        p.set_size(NonZeroU16::new(3).unwrap(), NonZeroU16::new(9).unwrap());
        assert_eq!(rows(&p, 9)[..4], [("abcde".into(), true), ("f".into(), false), ("ab".into(), true), ("cd".into(), false)]);
        assert_eq!(tabs(&p, 0), [(5, 3)]);
        assert_eq!(tabs(&p, 2), [(2, 6)]);
        p.set_size(NonZeroU16::new(3).unwrap(), NonZeroU16::new(5).unwrap());
        let r = rows(&p, 5);
        assert_eq!(r.iter().map(|(t, w)| (t.as_str(), *w)).take(8).collect::<alloc::vec::Vec<_>>(), [("abcde", true), ("", true), ("f", false), ("ab", true), ("", true), ("", true), ("", true), ("cd", false)]);
        assert_eq!((tabs(&p, 1), tabs(&p, 5)), (alloc::vec![(0, 3)], alloc::vec![(0, 6)]));
        p.set_size(NonZeroU16::new(3).unwrap(), NonZeroU16::new(20).unwrap());
        assert_eq!(rows(&p, 20)[..2], [("abcde   f".into(), false), ("ab      cd".into(), false)]);
        assert_eq!((tabs(&p, 0), tabs(&p, 1)), (alloc::vec![(5, 3)], alloc::vec![(2, 6)]));
        // DECRC restores the saved link, SGR 0 keeps it
        let mut p = parser(3, 20);
        p.process(b"\x1b]8;;http://s\x07\x1b7\x1b]8;;\x07A\x1b8B\x1b[0mC\x1b]8;;\x07D");
        let row = p.screen().history_row(0).unwrap();
        let links: alloc::vec::Vec<_> = (0..4).map(|x| row.cell_metadata(x).and_then(|m| m.hyperlink.as_ref()).map(|l| l.uri.as_str())).collect();
        assert_eq!(links, [Some("http://s"), Some("http://s"), None, None]);
        // clear-history -H: every link goes, tabs stay
        let mut p = parser(3, 20);
        p.process(b"\x1b]8;;http://a\x07ab\r\n\x1b]8;;\x07\tc\r\n\r\n\r\n\x1b]8;;http://b\x07d");
        p.screen_mut().reset_hyperlinks();
        let s = p.screen();
        assert!((0..s.history_rows_len()).all(|y| s.history_row(y).unwrap().metadata().all(|(_, m)| m.hyperlink.is_none())));
        assert_eq!(tabs(&p, 1), [(0, 8)]);
        assert_eq!(s.history_row_flags(0).0 & ROW_HYPERLINK, ROW_HYPERLINK);
        p.process(b"e");
        let y = p.screen().history_rows_len() - 3;
        assert!(p.screen().history_row(y).unwrap().cell_metadata(1).is_none());
    }

    fn text(p: &Parser, y: usize) -> alloc::string::String {
        let s = p.screen();
        let mut t = alloc::string::String::new();
        s.history_row(y).unwrap().write_plain(&mut t, s.size().1.get());
        t.trim_end().into()
    }
    fn tabs(p: &Parser, y: usize) -> alloc::vec::Vec<(u16, u16)> {
        p.screen().history_row(y).unwrap().metadata().filter(|(_, m)| m.tab_width > 0).map(|(x, m)| (x, m.tab_width)).collect()
    }
    #[test]
    fn tab_over_a_tab_moves_on_and_irm_inserts() {
        // a tab's own cell and its padding are content to a later HT (has_content in
        // input_c0_dispatch): it only moves the cursor, writing inside the tab ends it
        let mut p = parser(4, 20);
        p.process(b"\tA\r\x1b[3C\tB\r\n\tX\r\x1b[5G\x1bH\r\tY");
        assert_eq!((tabs(&p, 0), text(&p, 0)), (alloc::vec![(0, 8)], "        B".into()));
        assert_eq!((tabs(&p, 1), text(&p, 1)), (alloc::vec![], "    Y   X".into()));
        // IRM: a character or tab makes room for itself (screen_write_cell)
        let mut p = parser(4, 30);
        p.process(b"abc\r\x1b[4hZ\x1b[4l\r\n        X\r\x1b[4h\tY\x1b[4l");
        assert_eq!(text(&p, 0), "Zabc");
        assert_eq!((tabs(&p, 1), text(&p, 1)), (alloc::vec![(0, 8)], "        Y        X".into()));
    }
    #[test]
    fn link_uris_are_vis_encoded_and_reset_by_an_empty_sgr() {
        let links = |p: &Parser, y: usize, n: u16| -> alloc::vec::Vec<Option<alloc::string::String>> {
            let s = p.screen();
            let r = s.history_row(y).unwrap();
            (0..n).map(|x| r.cell_metadata(x).and_then(|m| m.hyperlink.as_ref()).map(|l| l.uri.clone())).collect()
        };
        // hyperlinks_put: utf8_stravis(VIS_OCTAL|VIS_CSTYLE) keeps UTF-8 and doubles '\'
        let mut p = parser(4, 20);
        p.process(b"\x1b]8;;http://c\\x\x07a\x1b]8;;http://\xff\xe4\xb8\xad\x07b\x1b]8;;\x07");
        assert_eq!(links(&p, 0, 2), [Some("http://c\\\\x".into()), Some("http://\\377\u{4e2d}".into())]);
        // CSI m is the default cell, link included; CSI 0 m keeps the link
        p.process(b"\r\n\x1b]8;;http://m\x07a\x1b[0mb\x1b[mc");
        assert_eq!(links(&p, 1, 3), [Some("http://m".into()), Some("http://m".into()), None]);
        // a long run of live named links sweeps the lookup table only when it doubles
        let mut p = Parser::new(NonZeroU16::new(4).unwrap(), NonZeroU16::new(80).unwrap(), 10000);
        for i in 0..6000 {
            p.process(alloc::format!("\x1b]8;id=n{i};http://n\x07x").as_bytes());
        }
        assert_eq!(p.screen().hyperlinks_sweep, 10000);
    }
    #[test]
    fn full_line_erases_clear_line_flags() {
        let flags = |p: &Parser, y: usize| p.screen().history_row_flags(y);
        let mut p = parser(6, 10);
        // EL from the line start is screen_write_clearline: OSC 133 marks stay, H/X go
        p.process(b"\x1b]133;A\x07\x1b]8;;http://x\x07L\x1b]8;;\x07\x1b[1G\x1b[K");
        assert_eq!(flags(&p, 0), (ROW_START_PROMPT, false));
        // EL past the start leaves the flags, EL 1 to the last column is the whole line
        p.process(b"\r\n\x1b]133;A\x07\x1b]8;;http://x\x07L\x1b]8;;\x07M\x1b[2G\x1b[K");
        assert_eq!(flags(&p, 1), (ROW_START_PROMPT | ROW_HYPERLINK | ROW_EXTENDED, true));
        p.process(b"\x1b[10G\x1b[1K");
        assert_eq!(flags(&p, 1), (ROW_START_PROMPT, false));
        // ED from the line start (grid_clear_lines) takes the marks too and the line above
        // no longer wraps onto it
        p.process(b"\r\n0123456789\x1b]133;A\x07wrap\x1b[1G\x1b[J");
        assert_eq!((flags(&p, 3), p.screen().history_row(2).unwrap().wrapped()), ((0, false), false));
        // zsh's CR EL after the space it wraps with ends the wrap (tmux 3.8 too)
        let mut q = parser(6, 10);
        q.process(b"0123456789 \r\x1b[KE\rER");
        assert_eq!((q.screen().history_row(0).unwrap().wrapped(), text(&q, 1)), (false, "ER".into()));
        // ECH over the whole line too
        p.process(b"\x1b]133;C\x07\xe4\xb8\xad\x1b[1G\x1b[10X");
        assert_eq!(flags(&p, 3), (0, false));
    }
    #[test]
    fn shrinking_pushes_lines_without_a_time_and_x_stays() {
        let mut p = parser(4, 10);
        p.screen_mut().set_time(77);
        p.process(b"1\r\n2\r\n3\r\n4\r\n5");
        // screen_resize_y only grows hsize: the lines it moves have no time
        p.set_size(NonZeroU16::new(2).unwrap(), NonZeroU16::new(10).unwrap());
        let s = p.screen();
        assert_eq!((0..3).map(|y| (text(&p, y), s.history_row_time(y))).collect::<alloc::vec::Vec<_>>(), [("1".into(), 77), ("2".into(), 0), ("3".into(), 0)]);
        // a line keeps GRID_LINE_EXTENDED after its wide character is overwritten
        p.process(b"\r\n\xe4\xb8\xad\rab");
        let y = p.screen().history_rows_len() - 1;
        assert_eq!((text(&p, y), p.screen().history_row_flags(y).1), ("ab".into(), true));
    }
    #[test]
    fn region_scrolls_and_ris_keep_the_history() {
        // grid_scroll_history_region: the top line of a scroll region not at the top goes
        // into the history too, with its time
        let mut p = parser(4, 10);
        p.screen_mut().set_time(5);
        p.process(b"a1\r\na2\r\na3\r\na4\x1b[2;3r\x1b[3;1H\n\n\x1b[r");
        let s = p.screen();
        assert_eq!((s.scrollback_len(), text(&p, 0), text(&p, 1), s.history_row_time(0)), (2, "a2".into(), "a3".into(), 5));
        // RIS is screen_write_reset: a clear (into the history with scroll-on-clear)
        let mut p = parser(4, 10);
        p.screen_mut().set_scroll_on_clear(true);
        p.process(b"h1\r\nh2\r\nh3\r\nh4\r\nh5\x1bc");
        assert_eq!((p.screen().scrollback_len(), text(&p, 0), text(&p, 4)), (5, "h1".into(), "h5".into()));
    }

    #[test]
    fn emoji_modifiers_joiners_and_regional_indicators_combine() {
        // screen_write_combine: one wide cell for each of these; 7-bit characters never join
        for (input, cursor, cell) in [
            ("👋🏽", 2, "👋🏽"), ("🏽👋", 2, "🏽👋"), ("👨\u{200D}👩", 2, "👨\u{200D}👩"), ("🇰🇷🇯", 3, "🇰🇷"),
            ("a🏽", 3, "a"), ("x\u{200D}y", 2, "x\u{200D}"),
        ] {
            let mut p = Parser::new(NonZeroU16::new(5).unwrap(), NonZeroU16::new(20).unwrap(), 0);
            p.process(input.as_bytes());
            assert_eq!(p.screen().cursor_position().1, cursor, "{input}");
            assert_eq!(p.screen().cell(0, 0).unwrap().contents(), cell, "{input}");
        }
        // as a pair of narrow regional indicators would not: the second makes the cell wide
        let mut p = Parser::new(NonZeroU16::new(5).unwrap(), NonZeroU16::new(20).unwrap(), 0);
        p.process("🇰🇷".as_bytes());
        assert!(p.screen().cell(0, 0).unwrap().is_wide());
    }
}
