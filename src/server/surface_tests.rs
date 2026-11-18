//! tmux 3.8 surface details: alerts, pane query replies and the client and history formats.
//! Expected values are what tmux 3.8 does for the same input.
use super::vars::Target;
use super::*;

struct Fixture { srv: Server, output: mpsc::Receiver<Vec<u8>>, sid: SessId, _events: mpsc::Receiver<Event> }
impl std::ops::Deref for Fixture { type Target = Server; fn deref(&self) -> &Server { &self.srv } }
impl std::ops::DerefMut for Fixture { fn deref_mut(&mut self) -> &mut Server { &mut self.srv } }
impl Drop for Fixture { fn drop(&mut self) { for p in self.srv.panes.values_mut() { p.kill(); } } }

/// Session "al" with windows 0 (current) and 1, attached by client 1.
fn fixture() -> Fixture {
    let (mut srv, events) = super::security_tests::fixture();
    let (tx, output) = mpsc::channel();
    let c = srv.clients.get_mut(&1).unwrap();
    c.readonly = false; c.acl_readonly = false; c.tx = tx; c.closed = Default::default();
    run(&mut srv, &["new-session", "-d", "-s", "al"]);
    run(&mut srv, &["new-window", "-d"]);
    let sid = *srv.sessions.keys().next().unwrap();
    srv.attach_client(1, sid);
    Fixture { srv, output, sid, _events: events }
}

fn run(srv: &mut Server, args: &[&str]) {
    srv.exec(args.iter().map(|s| s.to_string()).collect(), &mut Ctx::default()).unwrap();
}

fn pane(f: &Fixture, idx: u32) -> PaneId {
    f.windows[&f.sessions[&f.sid].links[&idx]].active
}

fn output(f: &mut Fixture, idx: u32, data: &[u8]) {
    let p = pane(f, idx);
    let g = f.panes[&p].generation;
    f.pane_data(p, g, data);
}

/// Whether the client was sent a bell since the last call.
fn rang(f: &Fixture) -> bool {
    f.output.try_iter().filter(|fr| *fr == frame(S_OUTPUT, b"\x07")).count() > 0
}

fn flags(f: &Fixture, idx: u32) -> u8 {
    f.sessions[&f.sid].alerts.get(&idx).copied().unwrap_or(0)
}

fn message(f: &Fixture) -> String {
    f.clients[&1].message.as_ref().map(|m| m.0.clone()).unwrap_or_default()
}

#[test]
fn bell_action_any_current_other_and_visual_bell() {
    let mut f = fixture();
    rang(&f);
    // any (the default): the current window rings too, without a flag
    output(&mut f, 0, b"\x07");
    assert!(rang(&f));
    assert_eq!(flags(&f, 0), 0);
    output(&mut f, 1, b"\x07");
    assert!(rang(&f));
    assert_eq!(flags(&f, 1), ALERT_BELL);
    // bells repeat even with the flag set
    output(&mut f, 1, b"\x07");
    assert!(rang(&f));
    // current: only the current window rings; others are still flagged
    run(&mut f, &["set", "-g", "bell-action", "current"]);
    let sid = f.sid;
    f.sessions.get_mut(&sid).unwrap().alerts.clear();
    output(&mut f, 1, b"\x07");
    assert!(!rang(&f));
    assert_eq!(flags(&f, 1), ALERT_BELL);
    output(&mut f, 0, b"\x07");
    assert!(rang(&f));
    // other: never the current window
    run(&mut f, &["set", "-g", "bell-action", "other"]);
    output(&mut f, 0, b"\x07");
    assert!(!rang(&f));
    // visual-bell on shows a message instead; both does both
    run(&mut f, &["set", "-g", "bell-action", "any"]);
    run(&mut f, &["set", "-g", "visual-bell", "on"]);
    output(&mut f, 0, b"\x07");
    assert!(!rang(&f));
    assert_eq!(message(&f), "Bell in current window");
    run(&mut f, &["set", "-g", "visual-bell", "both"]);
    output(&mut f, 1, b"\x07");
    assert!(rang(&f));
    assert_eq!(message(&f), "Bell in window 1");
}

#[test]
fn activity_alerts_ring_once_per_flag_and_stale_flags_are_found_on_attach() {
    let mut f = fixture();
    // output before monitor-activity is on leaves the window flag (w->flags) behind
    output(&mut f, 1, b"x");
    assert_eq!(flags(&f, 1), 0);
    run(&mut f, &["set", "-g", "monitor-activity", "on"]);
    rang(&f);
    let sid = f.sid;
    f.attach_client(1, sid);
    assert_eq!(flags(&f, 1), ALERT_ACTIVITY);
    // visual-activity off rings (not only bells do)
    assert!(rang(&f));
    output(&mut f, 1, b"y");
    assert!(!rang(&f), "activity alerts once until the flag is cleared");
    // activity-action other (the default) skips the current window; any covers it, and
    // as it is never flagged it alerts each time
    output(&mut f, 0, b"z");
    assert!(!rang(&f));
    run(&mut f, &["set", "-g", "activity-action", "any"]);
    output(&mut f, 0, b"z");
    assert!(rang(&f));
    output(&mut f, 0, b"z");
    assert!(rang(&f));
    assert_eq!(flags(&f, 0), 0);
}

#[test]
fn window_linked_sessions_counts_a_group_once_in_link_order() {
    let mut f = fixture();
    run(&mut f, &["new-session", "-d", "-s", "grp", "-t", "al"]);
    let p = pane(&f, 0);
    assert_eq!(f.expand_for_pane(p, "#{window_linked_sessions} #{window_linked_sessions_list}"), "1 al,grp");
    run(&mut f, &["kill-session", "-t", "grp"]);
    run(&mut f, &["new-session", "-d", "-s", "a0"]);
    run(&mut f, &["link-window", "-s", "a0:0", "-t", "al:5"]);
    let p = pane(&f, 5);
    // w->winlinks order: a0 linked it first
    assert_eq!(f.expand_for_pane(p, "#{window_linked_sessions} [#{window_linked_sessions_list}]"), "2 [a0,al]");
}

#[test]
fn session_last_attached_is_empty_until_attached() {
    let mut f = fixture();
    run(&mut f, &["new-session", "-d", "-s", "never"]);
    let s = f.sessions.values().find(|s| s.name == "never").unwrap();
    let p = f.windows[&s.active_window()].active;
    assert_eq!(f.expand_for_pane(p, "[#{session_last_attached}] #{?session_last_attached,y,n}"), "[] n");
    let p = pane(&f, 0);
    assert_eq!(f.expand_for_pane(p, "#{?session_last_attached,y,n}"), "y");
}

#[test]
fn history_formats_report_the_normal_screen_on_the_alternate_screen() {
    let mut f = fixture();
    let lines: String = (1..=100).map(|n| format!("{n}\r\n")).collect();
    output(&mut f, 0, lines.as_bytes());
    output(&mut f, 0, b"\x1b[?1049h");
    let p = pane(&f, 0);
    let (hsize, limit) = f.panes[&p].screen().base_scrollback();
    assert_eq!(f.expand_for_pane(p, "#{alternate_on} #{history_size} #{history_limit}"), format!("1 {hsize} {limit}"));
    assert!(hsize >= 76, "{hsize}");
    // history rows count as before; the empty alternate screen rows use no cells
    let rows = usize::from(f.panes[&p].size.1);
    let normal = f.expand_for_pane(p, "#{history_all_bytes}");
    assert!(normal.starts_with(&format!("{},", hsize + rows)), "{normal}");
    assert_eq!(normal.split(',').nth(2), Some(&*(hsize * 20).to_string()));
}

#[test]
fn client_termtype_and_terminal_capabilities() {
    let mut f = fixture();
    f.clients.get_mut(&1).unwrap().ident = Some(Identity { cols: 80, rows: 24, tty: true, cwd: String::new(), term: "xterm-256color".into(), pid: 0, ctx_pane: None, utf8: true, env: Vec::new() });
    f.clients.get_mut(&1).unwrap().terminal.vt100 = true;
    let t = Target { pane: Some(pane(&f, 0)), ..f.target_for_pane(pane(&f, 0)) };
    assert_eq!(f.expand(t, Some(1), "[#{client_termtype}]"), "[]");
    f.start_terminal_queries(1);
    assert!(f.terminal_report(1, b"\x1bP>|XTerm(400)\x1b\\"));
    assert_eq!(f.expand(t, Some(1), "[#{client_termtype}] #{client_termname}"), "[XTerm(400)] xterm-256color");
    // tty_term_has_name: what show-messages -T lists for the client's terminal, with
    // terminal-overrides as they were when the features last changed (the XTVERSION
    // reply above). Nobr is in neither xterm-256color's terminfo nor a feature, so only
    // the override could add it; Ms comes from terminfo on Unix and from the clipboard
    // feature where there is no terminfo (Windows), so only the override removes it.
    f.opts_server.set("terminal-overrides[90]", "xterm*:Nobr=\\E[x:Ms@".into());
    assert_eq!(f.expand(t, Some(1), "#{I/c:Nobr}|#{I/c:nosuch}|#{I/c:Ms}"), "0|0|1");
    // tty_update_features (here a DA reply with new features) applies them again
    assert!(f.terminal_report(1, b"\x1b[?62;52c"));
    assert_eq!(f.expand(t, Some(1), "#{I/c:Nobr}|#{I/c:Ms}"), "1|0");
}

/// What a command prints.
fn out(f: &mut Fixture, args: &[&str]) -> String {
    let mut ctx = Ctx::default();
    f.exec(args.iter().map(|s| s.to_string()).collect(), &mut ctx).unwrap();
    ctx.out.to_string()
}

#[test]
fn silence_alerts_repeat_for_the_current_window() {
    let mut f = fixture();
    run(&mut f, &["set", "-g", "monitor-silence", "1"]);
    run(&mut f, &["set", "-g", "silence-action", "any"]);
    run(&mut f, &["set", "-g", "visual-silence", "on"]);
    let (w0, w1) = (f.sessions[&f.sid].links[&0], f.sessions[&f.sid].links[&1]);
    let back = |f: &mut Fixture, w: WinId| f.windows.get_mut(&w).unwrap().silence_reset_us = now_us() - 1_100_000;
    // alerts_timer starts again each time: the current window (never flagged) alerts
    // each period, another window once until its flag is cleared
    for _ in 0..2 {
        f.clients.get_mut(&1).unwrap().message = None;
        back(&mut f, w0);
        f.silence_check();
        assert_eq!(message(&f), "Silence in current window");
        assert_eq!(flags(&f, 0), 0);
    }
    back(&mut f, w1);
    f.silence_check();
    assert_eq!((message(&f), flags(&f, 1)), ("Silence in window 1".to_string(), ALERT_SILENCE));
    f.clients.get_mut(&1).unwrap().message = None;
    back(&mut f, w1);
    f.silence_check();
    assert_eq!(message(&f), "");
}

#[test]
fn a_bell_left_on_the_window_is_found_by_the_next_check() {
    let mut f = fixture();
    run(&mut f, &["set", "-g", "visual-bell", "on"]);
    run(&mut f, &["set", "-g", "monitor-bell", "off"]);
    output(&mut f, 1, b"\x07");
    assert_eq!(message(&f), "");
    // alerts_callback checks every flag the window has once activity is queued
    run(&mut f, &["set", "-g", "monitor-bell", "on"]);
    run(&mut f, &["set", "-g", "monitor-activity", "on"]);
    output(&mut f, 1, b"x");
    assert_eq!(message(&f), "Bell in window 1");
}

#[test]
fn alternate_screen_keeps_the_normal_history() {
    let mut f = fixture();
    let lines: String = (1..=40).map(|n| format!("{n}\r\n")).collect();
    output(&mut f, 0, lines.as_bytes());
    output(&mut f, 0, b"\x1b[?1049halt");
    // capture-pane and copy mode read the history above the alternate screen
    let p = pane(&f, 0);
    let t = format!("%{p}");
    assert_eq!(out(&mut f, &["capture-pane", "-p", "-t", &t, "-S", "-", "-E", "1"]).lines().take(2).collect::<Vec<_>>(), ["1", "2"]);
    run(&mut f, &["copy-mode", "-t", &t]);
    run(&mut f, &["send-keys", "-t", &t, "-X", "history-top"]);
    assert!(f.expand_for_pane(p, "#{scroll_position}").parse::<usize>().unwrap() > 0);
    run(&mut f, &["send-keys", "-t", &t, "-X", "cancel"]);
    // clear-history clears it (grid_clear_history of the base grid)
    run(&mut f, &["clear-history", "-t", &t]);
    assert_eq!(f.expand_for_pane(p, "#{history_size}"), "0");
}

#[test]
fn capture_escapes_keep_trailing_cells_with_n() {
    let mut f = fixture();
    output(&mut f, 0, b"\x1b[31mred\x1b[0m plain\r\n");
    let t = format!("%{}", pane(&f, 0));
    // -N: no GRID_STRING_TRIM_SPACES, the cells go on to what the line has room for
    let w = f.panes[&pane(&f, 0)].size.0 as usize;
    let kept = out(&mut f, &["capture-pane", "-p", "-e", "-N", "-t", &t, "-S", "0", "-E", "0"]);
    assert_eq!(kept, format!("\x1b[31mred\x1b[39m plain{}\n", " ".repeat(w / 4 - 9)));
    assert_eq!(out(&mut f, &["capture-pane", "-p", "-e", "-t", &t, "-S", "0", "-E", "0"]), "\x1b[31mred\x1b[39m plain\n");
}

/// What a pane answers for `input`, with cursor-style `style` (its DECSCUSR number).
fn replies(style: u16, input: &[u8]) -> String {
    let nz = |n| std::num::NonZeroU16::new(n).unwrap();
    let mut parser = vt100::Parser::new_with_callbacks(nz(24), nz(80), 10, PaneCb { cursor_style_opt: style, ..PaneCb::default() });
    parser.process(input);
    String::from_utf8_lossy(&parser.callbacks().replies).into_owned()
}

#[test]
fn pane_query_replies() {
    // XTVERSION
    assert_eq!(replies(0, b"\x1b[>q"), "\x1bP>|tmux 3.8\x1b\\");
    // DECRQM, private modes: set, reset, permanently reset (3) and unknown
    assert_eq!(replies(0, b"\x1b[?1h\x1b[?1$p\x1b[?3$p\x1b[?6$p\x1b[?7$p\x1b[?25$p\x1b[?9999$p"), "\x1b[?1;1$y\x1b[?3;4$y\x1b[?6;2$y\x1b[?7;1$y\x1b[?25;1$y\x1b[?9999;0$y");
    assert_eq!(replies(0, b"\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[?1049h\x1b[?1000$p\x1b[?1006$p\x1b[?2004$p\x1b[?1049$p\x1b[?1004$p\x1b[?2026$p"),
        "\x1b[?1000;1$y\x1b[?1006;1$y\x1b[?2004;1$y\x1b[?1049;1$y\x1b[?1004;2$y\x1b[?2026;2$y");
    // 12 follows the cursor-style option until the application sets a style
    assert_eq!(replies(0, b"\x1b[?12$p"), "\x1b[?12;2$y");
    assert_eq!(replies(5, b"\x1b[?12$p"), "\x1b[?12;1$y");
    assert_eq!(replies(5, b"\x1b[2 q\x1b[?12$p"), "\x1b[?12;2$y");
    // ANSI modes: only IRM
    assert_eq!(replies(0, b"\x1b[4$p\x1b[4h\x1b[4$p\x1b[20$p"), "\x1b[4;2$y\x1b[4;1$y\x1b[20;0$y");
    // DECRQSS: the cursor style, from the option without one; anything else is unknown
    assert_eq!(replies(0, b"\x1bP$q q\x1b\\"), "\x1bP1$r q0 q\x1b\\");
    assert_eq!(replies(4, b"\x1bP$q q\x1b\\"), "\x1bP1$r q4 q\x1b\\");
    assert_eq!(replies(0, b"\x1b[5 q\x1bP$q q\x1b\\\x1bP$qm\x1b\\"), "\x1bP1$r q5 q\x1b\\\x1bP0$r\x1b\\");
    // XTWINOPS sizes, with tmux's default 16x32 cells; 22/23 take their argument
    assert_eq!(replies(0, b"\x1b[18t\x1b[19t\x1b[14t\x1b[16t"), "\x1b[8;24;80t\x1b[9;24;80t\x1b[4;768;1280t\x1b[6;32;16t");
    assert_eq!(replies(0, b"\x1b[22;0;18t"), "\x1b[8;24;80t");
}
