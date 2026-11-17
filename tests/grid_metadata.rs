//! tmux 3.8 grid metadata: OSC 133 line marks, OSC 8 hyperlinks, tab cells and line
//! times, and what reads them (capture-pane -F/-H/-I/-e, copy mode, attached clients).
//! The expected outputs come from tmux 3.8 (tests/grid_metadata.sh on the reference pod);
//! the panes run printf, so the tests are Unix-only.
#![cfg(unix)]
mod common;
use common::*;

/// Lines of `capture-pane -p` with the given flags.
fn cap(ctx: &Ctx, flags: &[&str]) -> Vec<String> {
    let mut args = vec!["capture-pane", "-p", "-t", "m"];
    args.extend_from_slice(flags);
    ctx.ok(&args).lines().map(String::from).collect()
}

fn x(ctx: &Ctx, cmd: &[&str]) {
    let mut args = vec!["send", "-X", "-t", "m"];
    args.extend_from_slice(cmd);
    ctx.ok(&args);
}

#[test]
fn prompt_marks_and_prompt_motions() {
    let ctx = Ctx::new("grid-prompt");
    ctx.ok(&["new", "-d", "-s", "m", "-x", "30", "-y", "6",
        "printf '\\033]133;A\\007$ \\033]133;B\\007one\\r\\n\\033]133;C\\007out1\\r\\nout1b\\r\\n\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007two\\r\\n\\033]133;C\\007out2\\r\\n\\033]133;D;1\\007\\033]133;P;k=s\\007> cont\\r\\n\\033]133;A\\007$ \\033]133;B\\007three\\r\\n\\033]133;C\\007o3\\r\\no3\\r\\no3\\r\\no3\\r\\n\\033]133;A\\007$ '; sleep 100"]);
    wait_for("prompt output", || cap(&ctx, &["-S", "-"]).len() >= 12);
    let marks = ["P $ one", "O out1", "- out1b", "P $ two", "O out2", "- > cont", "P $ three", "O o3", "- o3", "- o3", "- o3", "P $"];
    assert_eq!(cap(&ctx, &["-F", "-S", "-", "-E", "-"]), marks);
    ctx.ok(&["copy-mode", "-t", "m"]);
    x(&ctx, &["history-top"]);
    let pos = "#{copy_cursor_y}:#{copy_cursor_x} pos=#{scroll_position} line=#{copy_cursor_line}";
    // the line found goes to the top of the view (or the screen when it is on it)
    for (cmd, want) in [
        (&["next-prompt"][..], "0:0 pos=3 line=$ two"),
        (&["next-prompt"], "0:0 pos=0 line=$ three"),
        (&["next-prompt", "-o"], "1:0 pos=0 line=o3"),
        (&["next-prompt"], "5:0 pos=0 line=$"),
        (&["next-prompt"], "5:0 pos=0 line=$"),
        (&["previous-prompt", "-o"], "1:0 pos=0 line=o3"),
        (&["previous-prompt"], "0:0 pos=0 line=$ three"),
        (&["previous-prompt"], "0:0 pos=3 line=$ two"),
        (&["previous-prompt"], "0:0 pos=6 line=$ one"),
        (&["previous-prompt"], "0:0 pos=6 line=$ one"),
    ] {
        x(&ctx, cmd);
        assert_eq!(ctx.fmt("m", pos), want, "{cmd:?}");
    }
    // the repeat count is not used
    x(&ctx, &["history-bottom"]);
    ctx.ok(&["send", "-X", "-N", "3", "-t", "m", "previous-prompt"]);
    assert_eq!(ctx.fmt("m", "#{copy_cursor_y}:#{copy_cursor_x} pos=#{scroll_position}"), "0:0 pos=0");
    x(&ctx, &["cancel"]);
    // marks stay on their lines through a resize that rewraps them
    ctx.ok(&["resize-window", "-t", "m", "-x", "5"]);
    wait_for("rewrap", || cap(&ctx, &["-S", "-"]).len() > 12);
    assert_eq!(cap(&ctx, &["-F", "-S", "-", "-E", "-"]), ["P $ one", "O out1", "- out1b", "P $ two", "O out2", "W > con", "- t", "PW $ thr", "- ee", "O o3", "- o3", "- o3", "- o3", "P $"]);
    ctx.ok(&["resize-window", "-t", "m", "-x", "30"]);
    wait_for("unwrap", || cap(&ctx, &["-F", "-S", "-", "-E", "-"]) == marks);
}

#[test]
fn hyperlinks_in_capture_and_copy_mode() {
    let ctx = Ctx::new("grid-link");
    ctx.ok(&["new", "-d", "-s", "m", "-x", "30", "-y", "8",
        "printf '\\033]8;id=a;http://a\\007AA\\033]8;;\\007 \\033]8;;http://b\\007BB\\033]8;;\\007 \\033]8;;http://b\\007bb\\033]8;;\\007\\r\\n\\033]8;id=a;http://a\\007again\\033]8;;\\007 \\033]8;id=c;http://c\\007\\033[31mC\\033[0mc\\033]8;;\\007\\r\\nplain\\r\\n\\033]8;;http://w\\007\\344\\270\\255\\346\\226\\207\\033]8;;\\007x\\r\\n\\033]8;id=x:y=z;http://q?a=1;b=2\\007semi\\033]8;;\\007\\r\\n\\033]8;id=1;id=2;http://bad\\007bad\\033]8;;\\007\\r\\nAB\\033]8;;http://o\\007\\033[1;1HX\\033]8;;\\007'; sleep 100"]);
    wait_for("link output", || ctx.capture("m").contains("AB"));
    // each link once: anonymous links are distinct, a named one is shared, a link
    // listed on an earlier line is not repeated; the first ';' ends the parameters
    assert_eq!(cap(&ctx, &["-H"]), ["http://o http://a http://b http://b", "http://c", "http://w", "http://q?a=1;b=2", "id=2;http://bad"]);
    assert_eq!(cap(&ctx, &["-H", "-S", "1", "-E", "1"]), ["http://a http://c"]);
    assert_eq!(cap(&ctx, &["-H", "-F", "-L"])[1], "1 HX http://c");
    assert_eq!(cap(&ctx, &["-F"])[..3], ["HX XA BB bb", "HX again Cc", "- plain"]);
    let grid = ctx.ok(&["capture-pane", "-p", "-R", "-t", "m"]);
    assert!(grid.contains("\tL 0 (0) flags=EXTENDED,HYPERLINK[102] ") && grid.contains("\t\tC 0,1 data=(1,1,A) flags=NONE[0] attr=NONE[0] fg=default[8] bg=default[8] us=default[8] link=http://a linkid=a\n"), "{grid}");
    // -e: the link (with the application's id) after the attributes, once for a wide character
    let e = cap(&ctx, &["-e"]);
    assert_eq!(e[0], "\x1b]8;;http://o\x1b\\X\x1b]8;id=a;http://a\x1b\\A\x1b]8;;\x1b\\ \x1b]8;;http://b\x1b\\BB\x1b]8;;\x1b\\ \x1b]8;;http://b\x1b\\bb\x1b]8;;\x1b\\");
    assert!(e[1].contains("m\x1b]8;id=c;http://c\x1b\\C"), "{:?}", e[1]);
    assert_eq!(e[3], "\x1b]8;;http://w\x1b\\中文\x1b]8;;\x1b\\x");
    assert_eq!(cap(&ctx, &["-e", "-C", "-S", "3", "-E", "3"]), ["\\033]8;;http://w\\033\\\\中文\\033]8;;\\033\\\\x"]);
    ctx.ok(&["copy-mode", "-t", "m"]);
    x(&ctx, &["top-line"]);
    let mut links = Vec::new();
    for _ in 0..10 {
        links.push(ctx.fmt("m", "#{copy_cursor_hyperlink}"));
        x(&ctx, &["cursor-right"]);
    }
    // (the last cursor-right went on to the next line)
    assert_eq!(links, ["http://o", "http://a", "", "http://b", "http://b", "", "http://b", "http://b", "", "http://a"]);
    // the right half of a wide character has the link of its left half
    x(&ctx, &["top-line"]);
    ctx.ok(&["send", "-X", "-N", "3", "-t", "m", "cursor-down"]);
    assert_eq!(ctx.fmt("m", "#{copy_cursor_x}:#{copy_cursor_hyperlink}"), "0:http://w");
    x(&ctx, &["cursor-right"]);
    assert_eq!(ctx.fmt("m", "#{copy_cursor_x}:#{copy_cursor_hyperlink}"), "2:http://w");
    // the mode's screen is a clone whose links do not resolve
    assert_eq!(ctx.ok(&["capture-pane", "-p", "-M", "-H", "-t", "m"]), "\n");
    assert_eq!(ctx.ok(&["capture-pane", "-p", "-M", "-e", "-E", "1", "-t", "m"]).lines().next(), Some("XA BB bb"));
    assert_eq!(ctx.ok(&["capture-pane", "-p", "-M", "-F", "-E", "0", "-t", "m"]), "HX XA BB bb\n");
}

#[test]
fn clear_history_h_forgets_links() {
    let ctx = Ctx::new("grid-link-reset");
    ctx.ok(&["new", "-d", "-s", "m", "-x", "30", "-y", "4",
        "printf '\\033]8;;http://a\\007one\\033]8;;\\007\\r\\n\\033]8;id=k;http://k\\007two\\r\\n'; read x; printf 'three\\033]8;id=k;http://k\\007four\\033]8;;\\007\\r\\n'; sleep 100"]);
    wait_for("link output", || ctx.capture("m").contains("two"));
    assert_eq!(cap(&ctx, &["-H"]), ["http://a", "http://k"]);
    ctx.ok(&["clear-history", "-t", "m"]);
    assert_eq!(cap(&ctx, &["-H"]), ["http://a", "http://k"]);
    ctx.ok(&["clear-history", "-H", "-t", "m"]);
    assert_eq!(ctx.ok(&["capture-pane", "-p", "-H", "-t", "m"]), "\n");
    // the lines keep their H flag; the link still open is gone, a new one works
    assert_eq!(cap(&ctx, &["-F", "-E", "1"]), ["HX one", "HX two"]);
    ctx.ok(&["send", "-t", "m", "Enter"]);
    wait_for("more output", || ctx.capture("m").contains("four"));
    assert_eq!(cap(&ctx, &["-H"]), ["http://k"]);
    assert_eq!(cap(&ctx, &["-e"])[2], "three\x1b]8;id=k;http://k\x1b\\four\x1b]8;;\x1b\\");
}

#[test]
fn tab_cells_are_captured_and_copied_as_tabs() {
    let ctx = Ctx::new("grid-tab");
    ctx.ok(&["new", "-d", "-s", "m", "-x", "30", "-y", "10",
        "printf 'a\\tb\\tc\\r\\n\\tlead\\r\\ntrail\\t\\r\\nxxxxxxxxxxxxxxxxxxxx\\r\\ty\\r\\n\\033[41ma\\tb\\033[0m\\r\\n0123456789012345678901234567\\tZ\\r\\n\\033[3g\\033[1;20H\\033H\\033[6;1Hq\\tw\\r\\nab\\033[1Dcd\\tX\\033[2D\\033[@Y\\r\\n'; sleep 100"]);
    wait_for("tab output", || ctx.capture("m").contains("acd"));
    // a tab over text only moves the cursor; one over blank cells is kept, also with a
    // new tab stop and when a character is inserted inside it
    let plain = ["a\tb\tc", "\tlead", "trail\t", "xxxxxxxxyxxxxxxxxxxx", "a\tb", "q123456789012345678w01234567\tZ", "acd\tY X"];
    assert_eq!(cap(&ctx, &[])[..7], plain);
    assert_eq!(cap(&ctx, &["-C"])[..7], plain);
    assert_eq!(cap(&ctx, &["-T", "-N"])[..3], ["a\tb\tc", "\tlead", "trail\t"]);
    assert_eq!(cap(&ctx, &["-F"])[..4], ["X a\tb\tc", "X \tlead", "X trail\t", "- xxxxxxxxyxxxxxxxxxxx"]);
    // -R shows a tab as one cell of its width followed by padding
    let grid = ctx.ok(&["capture-pane", "-p", "-R", "-t", "m"]);
    assert!(grid.contains("\t\tC 0,1 data=(7,7,       ) flags=TAB[80] ") && grid.contains("\t\tC 0,2 data=(1,1,!) flags=PADDING[4] "), "{grid}");
    assert!(grid.contains("\tL 0 (0) flags=EXTENDED[2] "), "{grid}");
    // the tab keeps the style of the cells it is written over
    // (capture -e writes only what changes, as grid_string_cells_code)
    assert!(cap(&ctx, &["-e"])[4].contains("\x1b[41ma\x1b[49m\t\x1b[41mb"), "{:?}", cap(&ctx, &["-e"])[4]);
    ctx.ok(&["copy-mode", "-t", "m"]);
    x(&ctx, &["top-line"]);
    x(&ctx, &["begin-selection"]);
    x(&ctx, &["end-of-line"]);
    x(&ctx, &["copy-selection"]);
    assert_eq!(ctx.ok(&["show-buffer"]), "a\tb\tc");
    // a selection starting inside a tab leaves it out
    x(&ctx, &["top-line"]);
    ctx.ok(&["send", "-X", "-N", "3", "-t", "m", "cursor-right"]);
    x(&ctx, &["begin-selection"]);
    ctx.ok(&["send", "-X", "-N", "2", "-t", "m", "cursor-down"]);
    x(&ctx, &["copy-selection"]);
    assert_eq!(ctx.ok(&["show-buffer"]), "\tc\n\tlead\ntrail\t");
    x(&ctx, &["top-line"]);
    x(&ctx, &["cursor-right"]);
    assert_eq!(ctx.fmt("m", "x=#{copy_cursor_x} w=#{copy_cursor_word} l=#{copy_cursor_line}|"), "x=1 w=b l=a\tb\tc|");
    let mut at = Vec::new();
    for cmd in ["next-word", "next-word", "previous-word", "end-of-line"] {
        x(&ctx, &[cmd]);
        at.push(ctx.fmt("m", "#{copy_cursor_x}"));
    }
    assert_eq!(at, ["8", "16", "8", "17"]);
    for want in ["12", "8"] {
        x(&ctx, &["cursor-down"]);
        x(&ctx, &["end-of-line"]);
        assert_eq!(ctx.fmt("m", "#{copy_cursor_x}"), want);
    }
    x(&ctx, &["select-line"]);
    x(&ctx, &["copy-selection"]);
    assert_eq!(ctx.ok(&["show-buffer"]), "trail\t");
    // a tab in a search matches a tab (the cursor goes where tmux 3.8 puts it)
    x(&ctx, &["top-line"]);
    x(&ctx, &["search-forward", "\tc"]);
    assert_eq!(ctx.fmt("m", "#{copy_cursor_x},#{copy_cursor_y}"), "17,0");
    x(&ctx, &["top-line"]);
    x(&ctx, &["search-forward", " c"]);
    assert_eq!(ctx.fmt("m", "#{copy_cursor_x},#{copy_cursor_y}"), "0,0");
}

#[test]
fn line_times_for_capture_and_copy_mode() {
    let ctx = Ctx::new("grid-time");
    ctx.ok(&["new", "-d", "-s", "m", "-x", "20", "-y", "4", "seq 1 8; printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'; sleep 100"]);
    wait_for("history output", || ctx.capture("m").contains("aaaa"));
    // history lines have the time they scrolled off the screen, screen lines 0
    let times = |flags: &[&str]| -> Vec<String> { cap(&ctx, flags).iter().map(|l| { let t = l.split(' ').next().unwrap(); if t.len() >= 9 { "T".into() } else { t.into() } }).collect() };
    assert_eq!(times(&["-I", "-S", "-"]), ["T", "T", "T", "T", "T", "T", "0", "0", "0", "0"]);
    let first = cap(&ctx, &["-I", "-S", "-"])[0].split(' ').next().unwrap().to_string();
    ctx.ok(&["copy-mode", "-t", "m"]);
    assert_eq!(ctx.fmt("m", "#{top_line_time}"), "0");
    x(&ctx, &["history-top"]);
    assert_eq!(ctx.fmt("m", "#{top_line_time}"), first);
    // the mode's screen is a clone of the pane's lines with their times and flags
    assert_eq!(times(&["-M", "-I", "-S", "-"]), ["T", "T", "T", "T", "T", "T", "0", "0", "0", "0"]);
    assert_eq!(cap(&ctx, &["-M", "-F", "-S", "2", "-E", "3"]), ["W aaaaaaaaaaaaaaaaaaaa", "- aaaaaaaaaa"]);
    x(&ctx, &["cancel"]);
    // a line split by a resize keeps its time on the first row only
    ctx.ok(&["resize-window", "-t", "m", "-x", "9"]);
    wait_for("rewrap", || cap(&ctx, &["-S", "-"]).len() == 12);
    assert_eq!(cap(&ctx, &["-I", "-F", "-S", "-"]).iter().map(|l| { let mut w = l.splitn(3, ' '); let t = w.next().unwrap(); format!("{} {}", if t.len() >= 9 { "T" } else { t }, w.next().unwrap()) }).collect::<Vec<_>>(),
        ["T -", "T -", "T -", "T -", "T -", "T -", "0 -", "0 -", "0 W", "0 W", "0 W", "0 -"]);
    ctx.ok(&["copy-mode", "-t", "m"]);
    x(&ctx, &["history-top"]);
    assert_eq!(ctx.fmt("m", "#{top_line_time}"), first);
}

#[test]
fn client_hyperlinks_are_feature_gated_and_survive_copy_mode() {
    for enabled in [false, true] {
        let ctx = Ctx::new("grid-link-tty");
        ctx.ok(&["new", "-d", "-s", "m", "-x", "40", "-y", "8", "printf '\\033]8;id=x;https://example.org\\007LINK\\033]8;;\\007 \\344\\270\\255\\033]8;;http://w\\007\\346\\226\\207\\033]8;;\\007'; sleep 100"]);
        wait_for("link output", || ctx.capture("m").contains("LINK"));
        ctx.ok(&["set", "-as", "terminal-features", if enabled { "xterm*:hyperlinks" } else { "xterm*:hyperlinks@" }]);
        let term = Term::attach(&ctx, &["attach", "-t", "m"], 40, 8);
        term.wait_screen("link drawing", |s| s.contains("LINK"));
        wait_for("wide link drawing", || !enabled || String::from_utf8_lossy(&term.raw.lock().unwrap()).contains("http://w"));
        let raw = String::from_utf8_lossy(&term.raw.lock().unwrap()).into_owned();
        assert_eq!(raw.contains("https://example.org"), enabled, "{raw:?}");
        if enabled {
            // tty_hyperlink: Hls with tmux's own id, closed with an empty one
            assert!(raw.contains("\x1b]8;id=tmux"), "{raw:?}");
            assert!(raw.contains(";https://example.org\x1b\\LINK\x1b]8;;\x1b\\"), "{raw:?}");
            assert!(raw.contains(";http://w\x1b\\文\x1b]8;;\x1b\\"), "{raw:?}");
            term.raw.lock().unwrap().clear();
            ctx.ok(&["copy-mode", "-H", "-t", "m"]);
            ctx.ok(&["refresh-client"]);
            wait_for("copy link drawing", || String::from_utf8_lossy(&term.raw.lock().unwrap()).contains("https://example.org"));
        }
    }
}

#[test]
fn tabs_rewrap_with_their_padding_and_stay_tabs() {
    // grid_reflow: a tab is a cell of its width and each padding cell counts a column,
    // so what follows a tab moves on early; widened again the tab is still a tab
    let ctx = Ctx::new("grid-tabflow");
    ctx.ok(&["new", "-d", "-s", "m", "-x", "20", "-y", "6", "printf 'abcde\\tf\\r\\nab\\tcd\\r\\n\\tx\\033[31m\\tq\\033[m\\r\\nzz\\t\\r\\n\\tA\\r\\033[3C\\tB'; sleep 100"]);
    wait_for("tab output", || ctx.capture("m").contains("B"));
    ctx.ok(&["resize-window", "-t", "m", "-x", "9"]);
    let narrow = ["WX abcde\t", "- f", "WX ab\t", "- cd", "WX \t", "W x", "WX \t", "- q", "WX zz\t", "- "];
    wait_for("rewrap 9", || cap(&ctx, &["-F", "-S", "-"]).starts_with(&narrow.map(String::from)));
    ctx.ok(&["resize-window", "-t", "m", "-x", "5"]);
    wait_for("rewrap 5", || cap(&ctx, &["-F", "-S", "-"]).len() > 16);
    assert_eq!(cap(&ctx, &["-F", "-S", "-"])[..8], ["WX abcde", "WX \t", "- f", "WX ab", "W ", "WX \t", "W ", "- cd"]);
    ctx.ok(&["resize-window", "-t", "m", "-x", "20"]);
    // an HT over a tab's cells only moves the cursor (B replaces A after the tab)
    wait_for("unwrap", || cap(&ctx, &["-F", "-S", "-"])[..6] == ["X abcde\tf", "X ab\tcd", "WX \tx", "X \tq", "X zz\t", "X \tB"]);
    ctx.ok(&["resize-window", "-t", "m", "-x", "30"]);
    wait_for("unwrap 30", || cap(&ctx, &["-S", "-"])[..4] == ["abcde\tf", "ab\tcd", "\tx\tq", "zz\t"]);
}

#[test]
fn links_carry_across_captured_lines_and_reset_like_tmux() {
    let ctx = Ctx::new("grid-linkcarry");
    ctx.ok(&["new", "-d", "-s", "m", "-x", "10", "-y", "8", "printf '\\033]8;;http://x\\007x1\\r\\nx2\\033]8;;\\007\\r\\n\\033]8;;http://y\\007yyyyy\\r\\nyy\\033]8;;\\007\\r\\n\\033]8;id=k;http://long\\007abcdefghijklmn\\033]8;;\\007\\r\\n\\033]8;;http://m\\007a\\033[0mb\\033[mc\\r\\n\\033]8;;http://c\\\\x\\007v\\033]8;;\\007'; sleep 100"]);
    wait_for("link output", || ctx.capture("m").contains('v'));
    // grid_string_cells keeps the last cell from line to line: a line going on with the
    // link the last cell read had does not open it again; without -J or -T the cells
    // read go to the end of what the line has room for (a quarter, half or all of it)
    let open = |id: &str, uri: &str| format!("\x1b]8;{id};{uri}\x1b\\");
    let close = "\x1b]8;;\x1b\\";
    let e = cap(&ctx, &["-e"]);
    assert_eq!(e[..5], [format!("{}x1{close}", open("", "http://x")), format!("{}x2{close}", open("", "http://x")), format!("{}yyyyy{close}", open("", "http://y")), format!("{}yy{close}", open("", "http://y")), format!("{}abcdefghij{close}", open("id=k", "http://long"))]);
    assert_eq!(e[5], "klmn");
    let j = cap(&ctx, &["-e", "-J"]);
    assert_eq!(j[..3], [format!("{}x1{close}", open("", "http://x")), "x2".into(), format!("{}yyyyy{close}", open("", "http://y"))]);
    assert_eq!(j[4], format!("{}abcdefghij{close}klmn", open("id=k", "http://long")));
    // CSI m (no parameter) ends the link, CSI 0 m does not; URIs are vis-encoded
    assert_eq!(cap(&ctx, &["-H"])[3..], ["http://m", "http://c\\\\x"]);
    assert!(e[6].starts_with(&format!("{}a", open("", "http://m"))) && e[6].contains(&format!("b{close}c")), "{:?}", e[6]);
}

#[test]
fn erases_resizes_and_irm_change_lines_like_tmux() {
    let ctx = Ctx::new("grid-erase");
    ctx.ok(&["new", "-d", "-s", "m", "-x", "10", "-y", "8", "printf '0123456789wrap\\033[1G\\033[2K\\r\\n0123456789wrap\\033[1G\\033[K\\r\\n0123456789wrap\\033[3G\\033[K\\r\\n0123456789wrap\\033[1G\\033[J'; sleep 100"]);
    wait_for("erase output", || cap(&ctx, &["-F"]).iter().filter(|l| l.contains("0123")).count() == 4);
    // a whole-line clear ends the wrap of the line above (grid_clear_lines)
    assert_eq!(cap(&ctx, &["-F"]), ["- 0123456789", "- ", "- 0123456789", "- ", "W 0123456789", "- wr", "- 0123456789", "- "]);
    // shrinking moves lines into the history without a time (screen_resize_y)
    ctx.ok(&["resize-window", "-t", "m", "-y", "3"]);
    wait_for("shrink", || ctx.fmt("m", "#{history_size}") == "5");
    assert!(cap(&ctx, &["-I", "-S", "-", "-E", "-1"]).iter().all(|l| l.starts_with("0 ")));
    ctx.ok(&["kill-server"]);
    // IRM: a character or a tab makes room for itself
    let ctx = Ctx::new("grid-irm");
    ctx.ok(&["new", "-d", "-s", "m", "-x", "30", "-y", "4", "printf 'abc\\r\\033[4hZ\\033[4l\\r\\n        X\\r\\033[4h\\tY\\033[4l'; sleep 100"]);
    wait_for("irm output", || ctx.capture("m").contains('Y'));
    assert_eq!(cap(&ctx, &[])[..2], ["Zabc", "\tY        X"]);
}

#[test]
fn a_linked_tab_is_drawn_inside_its_link() {
    let ctx = Ctx::new("grid-tablink");
    ctx.ok(&["new", "-d", "-s", "m", "-x", "30", "-y", "4", "printf '\\033]8;;http://t\\007a        \\r\\033[C\\tb\\033]8;;\\007'; sleep 100"]);
    wait_for("tab link output", || ctx.capture("m").contains('b'));
    ctx.ok(&["set", "-as", "terminal-features", "xterm*:hyperlinks"]);
    let term = Term::attach(&ctx, &["attach", "-t", "m"], 30, 4);
    wait_for("tab link drawing", || String::from_utf8_lossy(&term.raw.lock().unwrap()).contains(";http://t\x1b\\a"));
    // the tab takes the link of the cells it is written over (input_c0_dispatch) and is
    // drawn as one cell with it: no link change inside it
    let raw = String::from_utf8_lossy(&term.raw.lock().unwrap()).into_owned();
    let at = raw.find(";http://t\x1b\\a").unwrap();
    let rest = &raw[at..];
    let b = rest.find('b').unwrap();
    assert!(!rest[..b].contains("\x1b]8"), "{rest:?}");
}
