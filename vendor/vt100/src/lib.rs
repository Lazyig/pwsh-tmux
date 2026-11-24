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
mod tmux_width;

pub use attrs::Color;
pub use callbacks::Callbacks;
pub use cell::Cell;
pub use row::grown_cellsize;
pub use tmux_width::default_width;
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
    fn cellsize_is_what_tmux_allocates_for_the_line() {
        // gl->cellsize as tmux 3.8's capture-pane -R shows it for each line, written at 80
        // columns: a quarter, half or all of the width once a cell is written (printed
        // spaces too), not less after EL from the cursor or a backspace; ICH and DCH move
        // cells up to the end, ECH and EL with the default background clear only what
        // the line has, with another colour up to the cells cleared; EL 2 empties it
        let lines: [(&str, u16); 14] = [
            ("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\r50%\x1b[K", 80),
            ("zzzzzzzzzzzzzzzzzzzzzzzzz\x08\x08\x08\x08\x08\x08\x08\x08\x08\x08\x1b[K", 40),
            ("ab                              ", 40),
            ("xxxxxxxxxxxxxxxxxx\u{4e2d}", 20),
            ("q", 20),
            ("wide\u{4e2d}\u{4e2d}\x1b[3D\x1b[K", 20),
            ("ich\x1b[5@", 80),
            ("dch:abcdef\x1b[3D\x1b[2P", 80),
            ("ech:abcdef\x1b[3D\x1b[2X", 20),
            ("\x1b[44mbg\x1b[K\x1b[0m", 80),
            ("el2:abc\x1b[2K", 0),
            ("tab\tx", 20),
            ("0000000000000000000000000000000000000000000000000000000000000000000000\x1b[1K", 80),
            ("", 0),
        ];
        let mut p = parser(16, 80);
        for (text, _) in &lines { p.process(text.as_bytes()); p.process(b"\r\n"); }
        let size = |p: &Parser, y: usize| p.screen().history_row(y).unwrap().cellsize();
        for (y, (text, want)) in lines.iter().enumerate() {
            assert_eq!(size(&p, y), *want, "line {y} {text:?}");
        }
        // a run of printable characters (screen_write_collect_add) is written with one
        // grid_set_cells, so one ending at a quarter of the width takes the line to a half;
        // a run ends at anything but a printable character and at the end of a read
        let mut p = parser(4, 20);
        p.process(b"round\r\nl1\r\n");
        for c in b"abcde" { p.process(&[*c]); }
        p.process(b"\r\nabc\x1b[1mde");
        assert_eq!([size(&p, 0), size(&p, 1), size(&p, 2), size(&p, 3)], [10, 5, 5, 5]);
        // printed past the line's cells and then cleared, the line keeps them until EL 2
        let mut p = parser(4, 80);
        p.process(b"\x1b[60Gx\x1b[1K\r\n\x1b[2Kx");
        assert_eq!((size(&p, 0), size(&p, 1)), (80, 20));
        // DECALN fills every line
        p.process(b"\x1b#8");
        assert_eq!(size(&p, 3), 80);
        // reflow (grid_reflow): a line moved across keeps its size, the first part of a
        // split line has exactly its cells and the other parts what they are written
        // with, a line joined onto grows (tmux 3.8, resized from 80 columns)
        let mut p = parser(8, 80);
        p.process(b"q\r\nab                              \r\n0000000000000000000000000000000000000000000000000000000000000000000000\r\nwide\xe4\xb8\xad");
        let all = |p: &Parser| (0..p.screen().history_rows_len()).map(|y| size(p, y)).collect::<alloc::vec::Vec<_>>();
        let resize = |p: &mut Parser, cols: u16| p.set_size(NonZeroU16::new(8).unwrap(), NonZeroU16::new(cols).unwrap());
        assert_eq!(all(&p)[..4], [20, 40, 80, 20]);
        resize(&mut p, 120);
        assert_eq!(all(&p)[..4], [20, 40, 80, 20]);
        resize(&mut p, 60);
        assert_eq!(all(&p)[..5], [20, 40, 60, 15, 20]);
        resize(&mut p, 200);
        assert_eq!(all(&p)[..4], [20, 40, 100, 20]);
        resize(&mut p, 25);
        assert_eq!(all(&p)[..7], [20, 25, 12, 25, 25, 25, 20]);
    }
    #[test]
    fn cellsize_grows_a_cell_at_a_time_where_tmux_does() {
        // tmux 3.8's capture-pane -R: a tab's padding is set a cell at a time
        // (grid_set_padding), ECH and EL 1 with a background colour expand the line once
        // to the cells cleared (grid_clear), and grid_reflow writes the parts of a split
        // line and the cells joined onto one a cell at a time (grid_set_cell), so a line
        // written up to a quarter or half of the width stays there
        let size = |p: &Parser, y: usize| p.screen().history_row(y).unwrap().cellsize();
        let first = |cols: u16, text: &str| { let mut p = parser(4, cols); p.process(text.as_bytes()); size(&p, 0) };
        assert_eq!([first(32, "\t"), first(64, "\x1b[9G\t"), first(80, "a\x1b[33G\t")], [8, 16, 40]);
        assert_eq!([first(80, "\x1b[44m\x1b[20X"), first(80, "\x1b[44m\x1b[40X"), first(80, "\x1b[44m\x1b[19X")], [40, 80, 20]);
        assert_eq!([first(80, "\x1b[20G\x1b[44m\x1b[1K"), first(80, "\x1b[40G\x1b[44m\x1b[1K")], [40, 80]);
        let all = |p: &Parser| (0..p.screen().history_rows_len()).map(|y| size(p, y)).take_while(|&n| n > 0).collect::<alloc::vec::Vec<_>>();
        let resize = |p: &mut Parser, cols: u16| p.set_size(NonZeroU16::new(8).unwrap(), NonZeroU16::new(cols).unwrap());
        let zeros = |n: usize| "0".repeat(n);
        let mut p = parser(8, 80);
        p.process(alloc::format!("{}\r\n{}\r\n{}\r\n", zeros(50), zeros(60), zeros(70)).as_bytes());
        resize(&mut p, 40);
        assert_eq!(all(&p), [40, 10, 40, 20, 40, 40]);
        resize(&mut p, 120);
        assert_eq!(all(&p), [60, 60, 120]);
        let mut p = parser(8, 80);
        p.process(alloc::format!("{}\r\n{}\r\n", zeros(100), zeros(120)).as_bytes());
        assert_eq!(all(&p), [80, 40, 80, 80]);
        resize(&mut p, 200);
        assert_eq!(all(&p), [100, 200]);
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
        assert_eq!(core::mem::size_of::<Cell>(), 38);
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
            // utf8_should_combine reads the cell's first character: a second modifier joins
            ("👋🏽🏽", 2, "👋🏽🏽"),
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

    #[test]
    fn a_cell_widened_in_the_last_column_stays_there_like_tmux() {
        // screen_write_combine widens the cell where it is: in the last column its second
        // half is off the screen and the cursor stays; what follows wraps, writes over it
        // or moves it like any cell (each expected screen is tmux 3.8's in a 25x5 pane)
        let flag = "\u{1F1F0}\u{1F1F7}";
        let x = |n: usize| "x".repeat(n);
        let screen = |p: &Parser| -> alloc::vec::Vec<alloc::string::String> {
            let s = p.screen();
            (0..5).map(|y| (0..25).filter_map(|c| s.cell(y, c)).filter(|c| !c.is_wide_continuation())
                .map(|c| if c.has_contents() { c.contents() } else { " " }).collect::<alloc::string::String>().trim_end().into()).collect()
        };
        let nl = "\r\n".repeat(4);
        for (input, rows, cursor) in [
            (x(24) + flag + "c", [&*(x(24) + flag), "c", "", "", ""], (1, 1)),
            (x(24) + flag + "\x1bDc", [&*(x(24) + flag), "", "c", "", ""], (2, 1)),
            (nl.clone() + &x(24) + flag + "\x1bD\x1b[rabc", ["abc", "", "", &*(x(24) + flag), ""], (0, 3)),
            (x(24) + flag + "\r\x1b[24Cc", [&*(x(24) + "c"), "", "", "", ""], (0, 25)),
            (nl + &x(24) + flag + "\r\x1b[24Cc", ["", "", "", "", &*(x(24) + "c")], (4, 25)),
            (x(24) + flag + "\x1b[2Dc", [&*(x(23) + "c" + flag), "", "", "", ""], (0, 24)),
            (x(24) + flag + "\r\x1b[P", [&*(x(23) + flag), "", "", "", ""], (0, 0)),
            (x(24) + flag + "\r\x1b[@", [&*(" ".to_owned() + &x(24)), "", "", "", ""], (0, 0)),
            (x(24) + flag + "\r\x1b[24C\x1b[X", [&*x(24), "", "", "", ""], (0, 24)),
            (x(24) + flag + "\r\x1b[23C\u{4e2d}", [&*(x(23) + "\u{4e2d}"), "", "", "", ""], (0, 25)),
            ("\x1b[2;4r\x1b[4;1H".to_owned() + &x(24) + flag + "c", ["", "", &*(x(24) + flag), "c", ""], (3, 1)),
            ("\x1b[?7l".to_owned() + &x(24) + flag + "c", [&*(x(24) + "c"), "", "", "", ""], (0, 24)),
            // nothing joins it: a modifier is a character of its own, a zero width
            // character is dropped
            (x(24) + flag + "\u{1F3FD}", [&*(x(24) + flag), "\u{1F3FD}", "", "", ""], (1, 2)),
            (x(24) + flag + "\u{200D}c", [&*(x(24) + flag), "c", "", "", ""], (1, 1)),
            (x(24) + flag + "\u{301}Q", [&*(x(24) + flag), "Q", "", "", ""], (1, 1)),
        ] {
            let mut p = Parser::new(NonZeroU16::new(5).unwrap(), NonZeroU16::new(25).unwrap(), 0);
            p.process(input.as_bytes());
            assert_eq!(screen(&p), rows, "{input:?}");
            assert_eq!(p.screen().cursor_position(), cursor, "{input:?}");
        }
        let mut p = Parser::new(NonZeroU16::new(5).unwrap(), NonZeroU16::new(25).unwrap(), 0);
        p.process((x(24) + flag).as_bytes());
        let cell = p.screen().cell(0, 24).unwrap();
        assert!(cell.is_wide() && cell.contents() == flag);
        // nor does anything join a wide cell from its second half (a combining mark after
        // CUB, or after a wide character in the last two columns without DECAWM)
        for (input, at, cursor) in [("\u{4e2d}\x1b[D\u{301}".to_owned(), 0, (0, 1)), ("\x1b[?7l".to_owned() + &x(23) + "\u{4e2d}\u{301}", 23, (0, 24))] {
            let mut p = Parser::new(NonZeroU16::new(5).unwrap(), NonZeroU16::new(25).unwrap(), 0);
            p.process(input.as_bytes());
            assert_eq!((p.screen().cell(0, at).unwrap().contents(), p.screen().cursor_position()), ("\u{4e2d}", cursor), "{input:?}");
        }
        // a wider or narrower pane shows all of it, the cursor after it on its second
        // half (grid_reflow keeps tmux's padding cell; the alternate screen is not reflowed)
        for (alternate, cols, row, at, cursor) in [(false, 30, 0, 24, (0, 25)), (false, 20, 0, 4, (0, 5)), (true, 30, 0, 24, (0, 25))] {
            let mut p = Parser::new(NonZeroU16::new(3).unwrap(), NonZeroU16::new(25).unwrap(), 10);
            p.process(format!("{}{}{flag}", if alternate { "\x1b[?1049h" } else { "" }, x(24)).as_bytes());
            p.set_size(NonZeroU16::new(3).unwrap(), NonZeroU16::new(cols).unwrap());
            let s = p.screen();
            let y = s.history_rows_len() - 3;
            let line = s.history_row(y + row).unwrap();
            assert!(line.cell(at).unwrap().is_wide() && line.cell(at + 1).unwrap().is_wide_continuation(), "{cols}");
            assert_eq!((line.cell(at).unwrap().contents(), s.cursor_position()), (flag, cursor), "{cols}");
        }
        // one column: the pair is a wide cell in it
        let mut p = Parser::new(NonZeroU16::new(3).unwrap(), NonZeroU16::new(1).unwrap(), 0);
        p.process(flag.as_bytes());
        assert_eq!((p.screen().cell(0, 0).unwrap().contents(), p.screen().cursor_position()), (flag, (0, 1)));
        p.process(format!("a\r{flag}\x1b[P\x1bDb\u{301}{flag}\x1b[@\u{1F1EF}\x1b[X").as_bytes());
    }

    fn p30(input: &str) -> Parser {
        let mut p = Parser::new(NonZeroU16::new(8).unwrap(), NonZeroU16::new(30).unwrap(), 0);
        p.process(input.as_bytes());
        p
    }

    #[test]
    fn sgr_attributes_underline_styles_and_colours() {
        // input_csi_dispatch_sgr: 58 takes its colour from the parameters after it
        let p = p30("\x1b[4;58;2;10;20;30mA\x1b[0;1;2;5;8;9;53mB\x1b[0;4:3;38:2::1:2:3mC\x1b[22;21mD\x1b[58:5:9;24mE");
        let c = |x| p.screen().cell(0, x).unwrap().clone();
        assert_eq!((c(0).underline_style(), c(0).underline_color(), c(0).fgcolor()), (1, Color::Rgb(10, 20, 30), Color::Default));
        assert!(c(1).bold() && c(1).dim() && c(1).blink() && c(1).hidden() && c(1).strikethrough() && c(1).overline());
        assert_eq!((c(2).underline_style(), c(2).fgcolor()), (3, Color::Rgb(1, 2, 3)));
        assert_eq!((c(3).bold(), c(3).underline_style()), (false, 2));
        assert_eq!((c(4).underline_style(), c(4).underline_color()), (0, Color::Palette(9)));
    }

    #[test]
    fn dec_graphics_wrap_off_and_cursor_controls() {
        // SCS and SO/SI: the characters are kept, with GRID_ATTR_CHARSET
        let p = p30("\x1b(0lq\x1b(Bk\x1b)0\x0ex\x0fy");
        let cs: alloc::vec::Vec<bool> = (0..5).map(|x| p.screen().cell(0, x).unwrap().charset()).collect();
        assert_eq!((p.screen().contents(), cs), ("lqkxy".into(), alloc::vec![true, true, false, true, false]));
        // DECAWM off: the last column is written over
        let p = p30("\x1b[?7labcdefghijklmnopqrstuvwxyz0123456789");
        assert_eq!((p.screen().contents(), p.screen().cursor_position()), ("abcdefghijklmnopqrstuvwxyz0129".into(), (0, 29)));
        // a cursor movement up or down leaves the wrap pending at the last column
        let p = p30(&("a".repeat(29) + "x\x1b[Bz"));
        assert_eq!((p.screen().cell(1, 29).unwrap().contents(), p.screen().cursor_position()), ("z", (1, 30)));
        // IND, NEL, CBT, SCOSC and SCORC
        let p = p30("ab\x1bDcd\x1bEe\x1b[4;1Habcdefghijkl\x1b[Zx\x1b[5;1Ha\x1b[sbc\x1b[u|");
        assert_eq!(p.screen().contents(), "ab\n  cd\ne\nabcdefghxjkl\na|c");
        // DECRC keeps the cursor on the screen: a saved pending wrap is dropped
        let p = p30(&("a".repeat(29) + "x\x1b7\x1b[1;1H\x1b8z"));
        assert_eq!((p.screen().contents(), p.screen().cursor_position()), ("a".repeat(29) + "z", (0, 30)));
    }
}


/// tmux 3.8's erase style, background colour erase, scroll region and width
/// table (input.c, screen-write.c, grid.c, grid-view.c, screen.c, utf8.c).
#[cfg(test)]
mod tmux_screen_tests {
    use super::*;
    use alloc::{string::String, vec::Vec};
    use core::num::NonZeroU16;

    fn run(rows: u16, cols: u16, input: &str) -> Parser {
        let mut p = Parser::new(NonZeroU16::new(rows).unwrap(), NonZeroU16::new(cols).unwrap(), 100);
        p.process(input.as_bytes());
        p
    }
    type Style = (String, bool, bool, bool, Color, Color);
    /// A cell's text, bold, underline, inverse, foreground and background.
    fn style(p: &Parser, row: u16, col: u16) -> Style {
        let c = p.screen().cell(row, col).unwrap();
        (c.contents().into(), c.bold(), c.underline(), c.inverse(), c.fgcolor(), c.bgcolor())
    }
    /// What grid_clear_cell and grid_empty_line leave: nothing but the background.
    fn blank(bg: Color) -> Style {
        (String::new(), false, false, false, Color::Default, bg)
    }
    fn rows(p: &Parser) -> Vec<String> {
        p.screen().rows(0, p.screen().size().1.get()).collect()
    }
    fn row_is(p: &Parser, row: u16, want: &Style) -> bool {
        (0..p.screen().size().1.get()).all(|x| style(p, row, x) == *want)
    }

    #[test]
    fn erases_leave_only_the_background_colour() {
        // ECH (screen_write_clearcharacter), EL and ED pass grid_clear only the
        // current background: bold, underline, inverse and the foreground are not kept
        let p = run(4, 10, "\x1b[1;4;31mab\x1b[5X\r\n\x1b[1;7;31;44mcd\x1b[3X\x1b[0m");
        assert_eq!(style(&p, 0, 1), ("b".into(), true, true, false, Color::Idx(1), Color::Default));
        assert!((2..10).all(|x| style(&p, 0, x) == blank(Color::Default)));
        assert!((2..5).all(|x| style(&p, 1, x) == blank(Color::Idx(4))));
        let p = run(4, 10, "\x1b[1;4;31;42mxy\x1b[K\r\n\x1b[1;7;43m\x1b[J");
        assert!((2..10).all(|x| style(&p, 0, x) == blank(Color::Idx(2))));
        assert!(row_is(&p, 1, &blank(Color::Idx(3))) && row_is(&p, 3, &blank(Color::Idx(3))));
        // ECH over the whole line too
        let p = run(2, 6, "abc\r\x1b[1;45m\x1b[9X");
        assert!(row_is(&p, 0, &blank(Color::Idx(5))));
    }

    #[test]
    fn inserts_deletes_and_scrolls_bring_in_the_background_colour() {
        let bg = blank(Color::Idx(4));
        let plain = |s: &str| -> Style { (s.into(), false, false, false, Color::Default, Color::Default) };
        // ICH and DCH (grid_view_insert_cells, grid_view_delete_cells)
        let p = run(6, 8, "abcdef\r\x1b[44;1m\x1b[2@");
        assert_eq!((style(&p, 0, 0), style(&p, 0, 1), style(&p, 0, 2)), (bg.clone(), bg.clone(), plain("a")));
        let p = run(6, 8, "abcdef\r\x1b[44;1m\x1b[2P");
        assert_eq!((style(&p, 0, 0), style(&p, 0, 5), style(&p, 0, 6), style(&p, 0, 7)), (plain("c"), blank(Color::Default), bg.clone(), bg.clone()));
        // in the last column ICH clears that cell; with a wrap pending it does nothing
        let p = run(6, 8, "abcdefgh\x1b[8G\x1b[44m\x1b[3@");
        assert_eq!((style(&p, 0, 6), style(&p, 0, 7)), (plain("g"), bg.clone()));
        let p = run(6, 8, "abcdefgh\x1b[44m\x1b[3@\x1b[3P");
        assert_eq!(style(&p, 0, 7), plain("h"));
        // IL, DL, SU, SD, LF at the bottom, RI at the top
        for (input, row, other) in [
            ("a\r\nb\x1b[H\x1b[44m\x1b[L", 0, (1, "a")),
            ("a\r\nb\x1b[H\x1b[44m\x1b[M", 5, (0, "b")),
            ("a\r\nb\x1b[44m\x1b[S", 5, (0, "b")),
            ("a\r\nb\x1b[44m\x1b[T", 0, (1, "a")),
            ("a\r\nb\x1b[6;1H\x1b[44m\n", 5, (0, "b")),
            ("a\r\nb\x1b[6;1H\x1b[44m\x1bD", 5, (0, "b")),
            ("a\r\nb\x1b[6;1H\x1b[44m\x1bE", 5, (0, "b")),
            ("a\r\nb\x1b[H\x1b[44m\x1bM", 0, (1, "a")),
        ] {
            let p = run(6, 8, input);
            assert!(row_is(&p, row, &bg), "{input:?}");
            assert_eq!(style(&p, other.0, 0), plain(other.1), "{input:?}");
        }
        // a wrap scrolls in a default line (screen_write_linefeed(ctx, 1, 8))
        let p = run(6, 8, "\x1b[6;1H\x1b[44mabcdefghZ");
        assert_eq!(style(&p, 5, 0).5, Color::Idx(4));
        assert_eq!(style(&p, 5, 1), blank(Color::Default));
        // with the default background nothing changes
        let p = run(6, 8, "a\r\nb\x1b[1;31m\x1b[H\x1b[L\x1b[2@");
        assert!(row_is(&p, 0, &blank(Color::Default)));
    }

    #[test]
    fn insert_and_delete_lines_outside_the_region_reach_the_bottom() {
        // screen_write_insertline and _deleteline: with the cursor outside the scroll
        // region the lines from it to the bottom of the screen move
        let p = run(6, 8, "1\r\n2\r\n3\r\n4\r\n5\x1b[2;3r\x1b[5;1H\x1b[L");
        assert_eq!(rows(&p), ["1", "2", "3", "4", "", "5"]);
        let p = run(6, 8, "1\r\n2\r\n3\r\n4\r\n5\r\n6\x1b[2;3r\x1b[4;1H\x1b[M");
        assert_eq!(rows(&p), ["1", "2", "3", "5", "6", ""]);
        // inside it, only the region
        let p = run(6, 8, "1\r\n2\r\n3\r\n4\r\n5\x1b[2;3r\x1b[2;1H\x1b[L");
        assert_eq!(rows(&p), ["1", "", "2", "4", "5", ""]);
    }

    #[test]
    fn scroll_region_homes_the_cursor_and_ignores_one_line() {
        // screen_write_scrollregion: the cursor goes to 0,0 (not the region's top, origin
        // mode or not); a region of one line, or one that is so once kept on the screen,
        // is ignored and the cursor stays
        let p = run(6, 8, "1\r\n2\r\n3\r\n4\r\n5\x1b[3;5rZ");
        assert_eq!((p.screen().scroll_region(), p.screen().cursor_position(), rows(&p)[0].as_str()), ((2, 4), (0, 1), "Z"));
        let p = run(6, 8, "1\r\n2\r\n3\r\n4\r\n5\x1b[?6h\x1b[3;5rZ");
        assert_eq!((p.screen().cursor_position(), rows(&p)[0].as_str()), ((0, 1), "Z"));
        let mut p = run(6, 8, "\x1b[?6h\x1b[3;5r");
        p.process(b"\x1b[H");
        assert_eq!(p.screen().cursor_position(), (2, 0));
        let p = run(6, 8, "1\r\n2\r\n3\r\n4\r\n5\x1b[3;9rZ");
        assert_eq!((p.screen().scroll_region(), p.screen().cursor_position()), ((2, 5), (0, 1)));
        for input in ["\x1b[2;4r\x1b[5;3H\x1b[7;9r", "\x1b[2;4r\x1b[5;3H\x1b[3;3r", "\x1b[2;4r\x1b[5;3H\x1b[5;2r"] {
            let p = run(6, 8, input);
            assert_eq!((p.screen().scroll_region(), p.screen().cursor_position()), ((1, 3), (4, 2)), "{input:?}");
        }
        // CSI r is the whole screen
        let p = run(6, 8, "\x1b[2;4r\x1b[5;3H\x1b[r");
        assert_eq!((p.screen().scroll_region(), p.screen().cursor_position()), ((0, 5), (0, 0)));
    }

    #[test]
    fn region_origin_mode_and_saved_cursor_belong_to_the_screen() {
        // tmux keeps the region and origin mode in the screen and DECSC's cursor in the
        // input context: the alternate screen shares them
        let p = run(6, 8, "\x1b[3;5r\x1b[?1049h");
        assert_eq!((p.screen().alternate_screen(), p.screen().scroll_region()), (true, (2, 4)));
        let p = run(6, 8, "\x1b[3;5r\x1b[?1049h\x1b[?1049l");
        assert_eq!((p.screen().alternate_screen(), p.screen().scroll_region(), p.screen().cursor_position()), (false, (2, 4), (0, 0)));
        let p = run(6, 8, "\x1b[?47h\x1b[3;5r\x1b[?47l");
        assert_eq!(p.screen().scroll_region(), (2, 4));
        let p = run(6, 8, "\x1b[3;5r\x1b[?6h\x1b[?1049h\x1b[2;1HZ");
        assert_eq!((p.screen().origin_mode(), p.screen().cursor_position()), (true, (3, 1)));
        let p = run(6, 8, "\x1b[3;4H\x1b7\x1b[?1049h\x1b[H\x1b8Z");
        assert_eq!((p.screen().cursor_position(), style(&p, 2, 3).0.as_str()), ((2, 4), "Z"));
        // screen_resize_y resets the region; a change of width alone keeps it
        let mut p = run(6, 8, "\x1b[3;5r");
        p.set_size(NonZeroU16::new(6).unwrap(), NonZeroU16::new(12).unwrap());
        assert_eq!(p.screen().scroll_region(), (2, 4));
        p.set_size(NonZeroU16::new(7).unwrap(), NonZeroU16::new(12).unwrap());
        assert_eq!(p.screen().scroll_region(), (0, 6));
    }

    #[test]
    fn tmux_width_table_comes_before_the_unicode_width() {
        // utf8_default_width_cache: U+261D, U+26F9, U+270C, U+270D and others are two
        // columns in tmux 3.8, one in the Unicode tables; codepoint-widths overrides them.
        // U+261D is not one utf8_should_combine joins a skin tone to: 2 + 2 + 1
        for (input, cursor) in [("a\u{261d}b", 4), ("\u{270c}x", 3), ("\u{1f590}x", 3), ("\u{261e}x", 2), ("\u{261d}\u{1f3fb}x", 5), ("\u{1f590}\u{1f3fb}x", 3), ("\u{270c}\u{fe0f}y", 3)] {
            let p = run(2, 10, input);
            assert_eq!(p.screen().cursor_position().1, cursor, "{input:?}");
        }
        let p = run(2, 10, "a\u{261d}b");
        assert!(p.screen().cell(0, 1).unwrap().is_wide() && p.screen().cell(0, 2).unwrap().is_wide_continuation());
        let mut p = run(2, 10, "");
        p.screen_mut().set_codepoint_widths(alloc::vec![(0x261d, 0x261d, 1)], false);
        p.process("a\u{261d}b".as_bytes());
        assert_eq!(p.screen().cursor_position().1, 3);
    }
}
