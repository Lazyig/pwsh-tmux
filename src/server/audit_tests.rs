//! tmux 3.8 behaviour found by the second audit: options, buffers, names, keys and
//! targets. Expected values are what tmux 3.8 does for the same input.
use super::*;

struct Fixture { srv: Server, sid: SessId, _events: mpsc::Receiver<Event> }
impl std::ops::Deref for Fixture { type Target = Server; fn deref(&self) -> &Server { &self.srv } }
impl std::ops::DerefMut for Fixture { fn deref_mut(&mut self) -> &mut Server { &mut self.srv } }
impl Drop for Fixture { fn drop(&mut self) { for p in self.srv.panes.values_mut() { p.kill(); } } }

/// Session "au" with window 0.
fn fixture() -> Fixture {
    let (mut srv, events) = super::security_tests::fixture();
    run(&mut srv, &["new-session", "-d", "-s", "au"]).unwrap();
    let sid = *srv.sessions.keys().next().unwrap();
    Fixture { srv, sid, _events: events }
}

fn run(srv: &mut Server, args: &[&str]) -> Result<String, String> {
    let mut ctx = Ctx::default();
    srv.exec(args.iter().map(|s| s.to_string()).collect(), &mut ctx).map(|_| ctx.out.to_string())
}

fn pane(f: &Fixture, idx: u32) -> PaneId {
    f.windows[&f.sessions[&f.sid].links[&idx]].active
}

fn output(f: &mut Fixture, idx: u32, data: &[u8]) {
    let p = pane(f, idx);
    let g = f.panes[&p].generation;
    f.pane_data(p, g, data);
}

fn fmt(f: &Fixture, s: &str) -> String {
    f.expand_for_pane(pane(f, 0), s)
}

#[test]
fn history_limit_applies_to_existing_panes() {
    let mut f = fixture();
    run(&mut f, &["set", "-g", "history-limit", "50"]).unwrap();
    let lines: String = (1..=300).map(|n| format!("{n}\r\n")).collect();
    output(&mut f, 0, lines.as_bytes());
    // grid_collect_history: a full history loses a tenth of its limit at once
    let rows = f.panes[&pane(&f, 0)].size.1 as usize;
    let pushed = 300 + 1 - rows;
    let expect = if pushed <= 50 { pushed } else { 45 + (pushed - 50) % 5 };
    assert_eq!(fmt(&f, "#{history_size} #{history_limit}"), format!("{expect} 50"));
    run(&mut f, &["set", "-g", "history-limit", "10"]).unwrap();
    assert_eq!(fmt(&f, "#{history_size} #{history_limit}"), "10 10");
    run(&mut f, &["set", "-g", "history-limit", "1000"]).unwrap();
    assert_eq!(fmt(&f, "#{history_size} #{history_limit}"), "10 1000");
    // a full history loses one line even to the same limit
    run(&mut f, &["set", "-g", "history-limit", "10"]).unwrap();
    run(&mut f, &["set", "-g", "history-limit", "10"]).unwrap();
    assert_eq!(fmt(&f, "#{history_size} #{history_limit}"), "9 10");
}

#[test]
fn buffer_names_are_checked_and_unnamed_commands_use_the_newest_automatic_buffer() {
    let mut f = fixture();
    // paste_set and paste_rename: clean_name refuses control characters
    assert_eq!(run(&mut f, &["set-buffer", "-b", "n\u{1}m", "x"]), Err("invalid buffer name: n\u{1}m".into()));
    assert_eq!(run(&mut f, &["set-buffer", "-b", "", "x"]), Err("empty buffer name".into()));
    run(&mut f, &["set-buffer", "-b", "a\\b", "x"]).unwrap();
    assert!(f.buffers.iter().any(|b| b.name == "a\\\\b"));
    run(&mut f, &["set-buffer", "-b", "ok", "x"]).unwrap();
    assert_eq!(run(&mut f, &["set-buffer", "-b", "ok", "-n", "r\tq"]), Err("invalid buffer name: r\tq".into()));
    // paste_get_top skips named buffers
    run(&mut f, &["set-buffer", "one"]).unwrap();
    run(&mut f, &["set-buffer", "two"]).unwrap();
    run(&mut f, &["set-buffer", "-b", "named", "three"]).unwrap();
    assert_eq!(run(&mut f, &["show-buffer"]).unwrap(), "two");
    run(&mut f, &["delete-buffer"]).unwrap();
    assert_eq!(run(&mut f, &["show-buffer"]).unwrap(), "one");
    // file errors as file_read writes them
    let e = run(&mut f, &["load-buffer", "/nonexistent/fx"]).unwrap_err();
    assert_eq!(e, "No such file or directory: /nonexistent/fx");
}

#[test]
fn pane_titles_and_paths_pass_clean_name() {
    let mut f = fixture();
    let before = fmt(&f, "#{pane_title}");
    run(&mut f, &["select-pane", "-T", "t\u{1b}[31mx"]).unwrap();
    assert_eq!(fmt(&f, "#{pane_title}"), before);
    run(&mut f, &["select-pane", "-T", "ok"]).unwrap();
    assert_eq!(fmt(&f, "#{pane_title}"), "ok");
    // screen_set_path(untrusted): "#(" is not kept
    output(&mut f, 0, b"\x1b]7;file:///tmp/#(x)\x07");
    assert_eq!(fmt(&f, "#{pane_path}"), "file:///tmp/_(x)");
    // a path that is not valid UTF-8 is not taken
    output(&mut f, 0, b"\x1b]7;file:///tmp/a\xffb\x07");
    assert_eq!(fmt(&f, "#{pane_path}"), "file:///tmp/_(x)");
}

fn windows(f: &Fixture) -> String {
    f.sessions[&f.sid].links.iter().map(|(i, w)| format!("{i}:{}", f.windows[w].name)).collect::<Vec<_>>().join(" ")
}

#[test]
fn window_index_targets_shuffle_and_link_errors() {
    let mut f = fixture();
    run(&mut f, &["rename-window", "w0"]).unwrap();
    for (i, n) in [("1", "w1"), ("5", "w5"), ("6", "w6")] {
        run(&mut f, &["new-window", "-d", "-t", &format!("au:{i}"), "-n", n]).unwrap();
    }
    // winlink_shuffle_up moves only the windows up to the next free index
    run(&mut f, &["new-window", "-d", "-a", "-t", "au:0", "-n", "new"]).unwrap();
    assert_eq!(windows(&f), "0:w0 1:new 2:w1 5:w5 6:w6");
    // an unused index has no winlink: -a puts the window there
    run(&mut f, &["new-window", "-d", "-a", "-t", "au:9", "-n", "a9"]).unwrap();
    assert_eq!(windows(&f), "0:w0 1:new 2:w1 5:w5 6:w6 9:a9");
    assert_eq!(run(&mut f, &["new-window", "-d", "-t", "au:0.1"]), Err("can't specify pane here".into()));
    // a name is a window of the current session before it is a session
    assert_eq!(run(&mut f, &["new-window", "-d", "-t", "w5"]), Err("create window failed: index 5 in use".into()));
    assert_eq!(run(&mut f, &["link-window", "-d", "-s", "au:1", "-t", "au:1"]), Err("same index: 1".into()));
    assert_eq!(run(&mut f, &["break-pane", "-d", "-t", "au:0.1"]), Err("can't specify pane here".into()));
    // break-pane of a window's only pane moves the window
    run(&mut f, &["break-pane", "-d", "-s", "au:9", "-t", "au:7", "-n", "moved"]).unwrap();
    assert_eq!(windows(&f), "0:w0 1:new 2:w1 5:w5 6:w6 7:moved");
}

#[test]
fn new_session_env_unbind_errors_and_respawn_empty() {
    let mut f = fixture();
    run(&mut f, &["new-session", "-d", "-s", "e2", "-x", "80", "-e", "FOO=bar"]).unwrap();
    let s = f.sessions.values().find(|s| s.name == "e2").unwrap();
    assert_eq!(s.env.get("FOO"), Some(&Some("bar".to_string())));
    assert_eq!(run(&mut f, &["unbind-key", "nosuchkey"]), Err("unknown key: nosuchkey".into()));
    assert!(run(&mut f, &["unbind-key", "-q", "nosuchkey"]).is_ok());
    run(&mut f, &["bind-key", "-T", "mytab", "x", "display", "hi"]).unwrap();
    run(&mut f, &["unbind-key", "-T", "mytab", "-a"]).unwrap();
    assert_eq!(run(&mut f, &["list-keys", "-T", "mytab"]), Err("table mytab doesn't exist".into()));
    assert_eq!(run(&mut f, &["unbind-key", "-T", "mytab", "-a"]), Err("table mytab doesn't exist".into()));
    // keys tmux keeps apart and hexadecimal names
    run(&mut f, &["bind-key", "-n", "C-m", "display", "cm"]).unwrap();
    run(&mut f, &["bind-key", "-n", "0x263a", "display", "smile"]).unwrap();
    let keys = run(&mut f, &["list-keys", "-T", "root"]).unwrap();
    assert!(keys.contains("C-m ") && keys.contains("☺") && !keys.contains("Enter  "), "{keys}");
    // SPAWN_EMPTY: respawn -E ignores a command
    assert!(run(&mut f, &["respawn-pane", "-E", "-k", "ls"]).is_ok());
}

#[test]
fn formats_loops_features_prompts_and_renames() {
    let mut f = fixture();
    run(&mut f, &["split-window", "-d"]).unwrap();
    run(&mut f, &["split-window", "-d", "-t", "au:0.0"]).unwrap();
    // P/i: by pane index, not by creation
    let ids = fmt(&f, "#{P:#{pane_id} }");
    let by_index = fmt(&f, "#{P/i:#{pane_id} }");
    assert_ne!(ids, by_index);
    assert_eq!(by_index, fmt(&f, "#{P/ir:#{pane_id} }").split_whitespace().rev().map(|s| format!("{s} ")).collect::<String>());
    // window_set_name fires window-renamed even for the same name
    run(&mut f, &["set-hook", "-g", "window-renamed", "set -ga @r x"]).unwrap();
    for _ in 0..2 {
        run(&mut f, &["rename-window", "q"]).unwrap();
        f.detect_changes(None);
    }
    f.drain_hooks();
    assert_eq!(f.opts_session.get("@r"), Some("xx"));
}
