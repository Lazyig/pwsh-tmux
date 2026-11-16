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
        // reflow moves a tab as one cell while it fits on the row, else it becomes spaces
        let mut p = parser(3, 20);
        p.process(b"abcde\tf\r\nab\tcd");
        assert_eq!(tabs(&p, 0), [(5, 3)]);
        assert_eq!(tabs(&p, 1), [(2, 6)]);
        p.set_size(NonZeroU16::new(3).unwrap(), NonZeroU16::new(9).unwrap());
        let s = p.screen();
        let text: alloc::vec::Vec<_> = (0..s.history_rows_len()).map(|y| { let mut t = alloc::string::String::new(); s.history_row(y).unwrap().write_plain(&mut t, 9); t }).collect();
        assert_eq!(text[..2], ["abcde   f", "ab      c"], "{text:?}");
        assert_eq!(tabs(&p, 0), [(5, 3)]);
        assert_eq!(tabs(&p, 1), [(2, 6)]);
        p.set_size(NonZeroU16::new(3).unwrap(), NonZeroU16::new(5).unwrap());
        assert_eq!(tabs(&p, 0), []);
        p.set_size(NonZeroU16::new(3).unwrap(), NonZeroU16::new(20).unwrap());
        let mut t = alloc::string::String::new();
        p.screen().history_row(0).unwrap().write_plain(&mut t, 20);
        assert_eq!(t, "abcde   f");
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
}
