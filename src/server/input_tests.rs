//! Input option behavior, exercised through the server and terminal parsers.
use super::*;
use crate::keys::{Key, KeyCode, Mods};

struct Fixture { srv: Server, output: mpsc::Receiver<Vec<u8>>, pane_input: mpsc::Receiver<Vec<u8>>, pane: PaneId, sid: SessId, _events: mpsc::Receiver<Event> }
impl std::ops::Deref for Fixture { type Target = Server; fn deref(&self) -> &Server { &self.srv } }
impl std::ops::DerefMut for Fixture { fn deref_mut(&mut self) -> &mut Server { &mut self.srv } }
impl Drop for Fixture { fn drop(&mut self) { for p in self.srv.panes.values_mut() { p.kill(); } } }
fn fixture() -> Fixture {
    let (mut srv, events) = super::security_tests::fixture();
    let (tx, output) = mpsc::channel();
    let c = srv.clients.get_mut(&1).unwrap();
    c.readonly = false; c.acl_readonly = false; c.tx = tx; c.closed.store(false, std::sync::atomic::Ordering::SeqCst);
    let mut ctx = Ctx::default();
    srv.exec(vec!["new-session".into(), "-d".into(), "-s".into(), "input".into()], &mut ctx).unwrap();
    let sid = *srv.sessions.keys().next().unwrap();
    srv.attach_client(1, sid);
    let pane = srv.client_active_pane(1).unwrap();
    let (tx, pane_input) = mpsc::channel();
    srv.panes.get_mut(&pane).unwrap().input = tx;
    srv.clients.get_mut(&1).unwrap().features = features::compute("xterm-256color", "", &[], "bpaste,cstyle,ccolour");
    Fixture { srv, output, pane_input, pane, sid, _events: events }
}
fn run(srv: &mut Server, args: &[&str]) {
    let ctx = Ctx { client: Some(1), ..Ctx::default() };
    srv.run_queue(Queue { cmds: vec![args.iter().map(|s| s.to_string()).collect()].into(), ctx, soft: 0 });
}
fn encode(f: &Fixture, key: &str) -> Vec<u8> { let k = keys::parse_key_name(key).unwrap(); f.encode_pane_key(f.pane, &k, keys::encode_key(&k, false)).unwrap_or_default() }
fn encode_opt(f: &Fixture, key: &str) -> Option<Vec<u8>> { let k = keys::parse_key_name(key).unwrap(); f.encode_pane_key(f.pane, &k, keys::encode_key(&k, false)) }

#[test]
fn input_option_backspace_user_keys_xterm_and_extended_keys() {
    let mut f = fixture();
    assert_eq!(encode(&f, "BSpace"), b"\x7f");
    f.opts_server.set("backspace", "C-h".into());
    assert_eq!(encode(&f, "BSpace"), b"\x08");
    assert_eq!(encode(&f, "M-BSpace"), b"\x1b\x08");
    f.opts_server.set("user-keys[4]", "\x1b[99~".into());
    assert_eq!(encode_opt(&f, "User4"), Some(vec![])); // user keys are never written to a pane
    run(&mut f, &["bind-key", "-n", "User4", "set-option -g @user-key yes"]);
    f.handle_input(1, b"\x1b[9"); assert!(f.opts_session.get("@user-key").is_none());
    f.handle_input(1, b"9~"); assert_eq!(f.opts_session.get("@user-key"), Some("yes"));
    let pane = f.pane;
    f.panes.get_mut(&pane).unwrap().options.set("xterm-keys", "off".into());
    assert_eq!(encode(&f, "C-Up"), b"\x1b[1;5A"); // xterm-keys is obsolete in tmux 3.8
    f.panes.get_mut(&pane).unwrap().options.set("xterm-keys", "on".into());
    assert_eq!(encode(&f, "C-Up"), b"\x1b[1;5A");
    f.opts_server.set("extended-keys", "on".into());
    assert_eq!(encode(&f, "C-1"), b"1");
    f.panes.get_mut(&pane).unwrap().parser.callbacks_mut().ext_keys = 1;
    assert_eq!(encode(&f, "C-1"), b"\x1b[27;5;49~");
    f.opts_server.set("extended-keys-format", "csi-u".into());
    assert_eq!(encode(&f, "C-1"), b"\x1b[49;5u");
    f.panes.get_mut(&pane).unwrap().parser.callbacks_mut().ext_keys = 0;
    f.opts_server.set("extended-keys", "always".into());
    assert_eq!(encode(&f, "C-1"), b"\x1b[49;5u");
    let mut p = keys::Parser::new();
    assert!(matches!(&p.feed(b"\x1b[91;5u")[0], keys::Input::Key(Key { code: KeyCode::Escape, .. }, _)));
    assert!(matches!(&p.feed(b"\x1b\x7f")[0], keys::Input::Key(Key { code: KeyCode::BSpace, mods: Mods { meta: true, .. } }, _)));
}

#[test]
fn input_option_prefix_and_repeat_timing() {
    let mut f = fixture();
    f.opts_server.set("prefix-timeout", "20".into());
    f.opts_session.set("initial-repeat-time", "500".into());
    f.opts_session.set("repeat-time", "100".into());
    run(&mut f, &["bind-key", "-r", "x", "set-option -g @repeated yes"]);
    f.handle_input(1, b"\x02x");
    let until = f.clients[&1].repeat_until.unwrap();
    assert!(until.duration_since(Instant::now()) > Duration::from_millis(400));
    f.handle_input(1, b"x");
    assert!(f.clients[&1].repeat_until.unwrap().duration_since(Instant::now()) <= Duration::from_millis(100));
    f.opts_session.set("repeat-time", "0".into());
    f.handle_input(1, b"x"); assert!(f.clients[&1].repeat_until.is_none());
    f.handle_input(1, b"\x02");
    std::thread::sleep(Duration::from_millis(30));
    f.handle_input(1, b"x");
    assert_eq!(f.clients[&1].key_table, "root");
    assert_eq!(f.pane_input.try_recv().unwrap(), b"x");
}

#[test]
fn input_option_paste_guessing_and_partial_end_delay() {
    let mut f = fixture();
    run(&mut f, &["bind-key", "-n", "x", "set-option -g @bound yes"]);
    f.opts_session.set("assume-paste-time", "1000".into());
    f.handle_input(1, b"abx");
    assert_eq!(f.opts_session.get("@bound"), Some("yes")); // bpaste disables guessing
    f.opts_session.unset("@bound");
    f.clients.get_mut(&1).unwrap().features.0.remove("bpaste");
    f.handle_input(1, b"abx");
    assert!(f.opts_session.get("@bound").is_none());
    f.opts_session.set("assume-paste-time", "0".into());
    f.handle_input(1, b"x"); assert_eq!(f.opts_session.get("@bound"), Some("yes"));
    f.handle_input(1, b"\x1b[200~abc\x1b[20");
    assert!(f.clients[&1].esc_deadline.unwrap().duration_since(Instant::now()) > Duration::from_millis(400));
    f.handle_input(1, b"1~");
    assert!(!f.clients[&1].keyp.in_paste());
    f.handle_input(1, b"\x1b[200~unfinished");
    f.pane_input.try_iter().for_each(drop);
    std::thread::sleep(Duration::from_millis(5050));
    f.input_timers();
    assert!(!f.clients[&1].keyp.in_paste());
    assert!(f.pane_input.try_recv().unwrap().ends_with(b"unfinished"));
    let mut parser = keys::Parser::new();
    parser.feed(b"\x1b[200~unfinished");
    assert_eq!(parser.expire_paste(), vec![keys::Input::Paste(b"unfinished".to_vec())]);
    assert!(matches!(&parser.feed(b"x")[0], keys::Input::Key(_, _)));
}

#[test]
fn input_option_terminal_features_overrides_and_width_ranges() {
    let table = ["xterm*:RGB:mouse@:utf8@:appesc".into()];
    let mut f = features::compute("xterm-256color", "", &table, "");
    assert!(f.has("RGB") && f.has("appesc") && !f.has("mouse") && !f.has("utf8"));
    let caps = features::overrides("xterm-256color", &["xterm*:Ss=\\E[%p1%d q:Se=\\E[2 q:Ms@:Enbp@".into()], &mut f);
    assert_eq!(features::cursor_sequence(&caps, 5), "\x1b[5 q");
    assert_eq!(features::cursor_sequence(&caps, 0), "\x1b[2 q");
    assert!(!f.has("clipboard") && !f.has("bpaste"));
    assert!(features::compute("xterm-ghostty", "", &[], "").has("margins"));
    assert!(features::compute("WezTerm", "", &[], "").has("margins"));
    assert!(features::compute_named("xterm-256color", "WezTerm", "", &[], "").has("margins"));
    assert!(!features::compute_named("xterm-256color", "ghostty", "", &["*:margins@".into()], "").has("margins"));
    let mut server = fixture();
    server.opts_server.set("codepoint-widths[0]", "U+41-U+5A=2".into());
    server.opts_server.set("codepoint-widths[1]", "U+263A=2".into());
    server.opts_session.set("@width-text", "AB\u{263a}".into());
    // 7-bit characters never take codepoint-widths
    assert_eq!(server.expand(Default::default(), Some(1), "#{w:@width-text}"), "4");
    let widths = features::codepoint_widths(&["U+41-U+5A=2".into(), "B=1".into(), "U+5A-U+41=1".into(), "U+41=9".into()]);
    assert_eq!(widths, [(65, 90, 2), (66, 66, 1)]);
    let mut p = vt100::Parser::new(nz(3), nz(20), 10);
    p.screen_mut().set_codepoint_widths(widths, true);
    p.process(b"AB"); assert_eq!(p.screen().cursor_position(), (0, 2));
    assert!(!p.screen().cell(0, 0).unwrap().is_wide());
    p.screen_mut().set_codepoint_widths(vec![(0xe9, 0xe9, 2)], true);
    p.process("\u{e9}".as_bytes()); assert_eq!(p.screen().cursor_position(), (0, 4));
    p.process("x\u{fe0f}".as_bytes()); assert_eq!(p.screen().cursor_position(), (0, 6));
    let mut p = vt100::Parser::new(nz(3), nz(20), 10);
    p.screen_mut().set_codepoint_widths(vec![], false);
    p.process("x\u{fe0f}".as_bytes()); assert_eq!(p.screen().cursor_position(), (0, 1));
    // variation-selector-always-wide off: VS16 leaves a narrow emoji base narrow
    p.process("\u{263a}\u{fe0f}b\u{2764}\u{fe0f}".as_bytes()); assert_eq!(p.screen().cursor_position(), (0, 4));
    p.screen_mut().set_codepoint_widths(vec![], true);
    p.process("\u{263a}\u{fe0f}".as_bytes()); assert_eq!(p.screen().cursor_position(), (0, 6));
}

#[test]
fn input_option_alternate_screen_titles_passthrough_and_buffer_limit() {
    let mut f = fixture(); let pane = f.pane; let generation = f.panes[&pane].generation;
    f.panes.get_mut(&pane).unwrap().options.set("alternate-screen", "off".into());
    f.panes.get_mut(&pane).unwrap().options.set("allow-set-title", "off".into());
    let title = f.panes[&pane].title.clone();
    f.pane_data(pane, generation, b"\x1bc\x1b[?1049h\x1b]2;blocked\x07");
    assert!(!f.panes[&pane].screen().alternate_screen()); assert_eq!(f.panes[&pane].title, title);
    f.panes.get_mut(&pane).unwrap().options.set("alternate-screen", "on".into());
    f.panes.get_mut(&pane).unwrap().options.set("allow-set-title", "on".into());
    f.pane_data(pane, generation, b"\x1b[?1049h\x1b]2;allowed\x07");
    assert!(f.panes[&pane].screen().alternate_screen()); assert_eq!(f.panes[&pane].title, "allowed");
    f.output.try_iter().for_each(drop);
    f.pane_data(pane, generation, b"\x1bPtmux;\x1b\x1b]2;outer\x07\x1b\\");
    assert!(f.output.try_recv().is_err());
    f.panes.get_mut(&pane).unwrap().options.set("allow-passthrough", "on".into());
    f.pane_data(pane, generation, b"\x1bPtmux;\x1b\x1b]2;outer\x07\x1b\\");
    assert!(f.output.try_recv().unwrap().ends_with(b"\x1b]2;outer\x07"));
    let mut p = vt100::Parser::new_with_callbacks(nz(3), nz(20), 0, PaneCb::default());
    p.set_input_buffer_size(10);
    p.process(b"\x1b]2;1234567890123456789012345678901234567890\x07ok");
    assert!(p.callbacks().title.is_none()); assert!(p.screen().contents().contains("ok"));
    p.process(b"\x1b]2;yes\x07"); assert_eq!(p.callbacks().title.as_deref(), Some("yes"));
}

#[test]
fn input_option_cursor_styles_prompt_keypad_and_focus_following() {
    let mut f = fixture(); let pane = f.pane;
    f.panes.get_mut(&pane).unwrap().options.set("cursor-style", "bar".into());
    f.panes.get_mut(&pane).unwrap().options.set("cursor-colour", "#123456".into());
    let state = f.compose(1).unwrap(); assert_eq!(state.modes.cursor_style, 6); assert_eq!(state.modes.cursor_colour, Some(vt100::Color::Rgb(18,52,86)));
    f.opts_session.set("prompt-command-cursor-style", "underline".into());
    f.opts_session.set("prompt-command-cursor-colour", "red".into());
    f.open_prompt(1, PromptKind::Command, vec![(":".into(), "".into())], "%1".into(), Some(pane), false, false);
    f.opts_session.set("status-keys", "vi".into());
    f.dispatch_input(1, keys::Input::Key(Key::plain(KeyCode::Escape), vec![27]));
    let state = f.compose(1).unwrap(); assert_eq!(state.modes.cursor_style, 4); assert_eq!(state.modes.cursor_colour, Some(vt100::Color::Idx(1)));
    f.handle_input(1, b"i\x1bOp"); assert_eq!(f.clients[&1].prompt.as_ref().unwrap().input, ['0']);
    f.opts_session.set("prompt-cursor-style", "block".into());
    f.opts_session.set("prompt-cursor-colour", "blue".into());
    f.clients.get_mut(&1).unwrap().prompt.as_mut().unwrap().template = "rename-window '%%'".into();
    let state = f.compose(1).unwrap(); assert_eq!(state.modes.cursor_style, 2); assert_eq!(state.modes.cursor_colour, Some(vt100::Color::Idx(4)));
    f.clients.get_mut(&1).unwrap().prompt = None;
    run(&mut f, &["split-window", "-h", "-d"]);
    f.opts_session.set("mouse", "on".into()); f.opts_session.set("focus-follows-mouse", "on".into());
    let wid = f.window_of(pane).unwrap();
    let (other, rect) = f.rects(wid).0.into_iter().find(|(p, _)| *p != pane).unwrap();
    assert_eq!(f.compose(1).unwrap().modes.mouse, 1003);
    f.dispatch_input(1, keys::Input::Mouse(keys::Mouse { kind: keys::MouseKind::Move, button: 0, x: rect.x+1, y: rect.y+1, mods: Mods::NONE }, vec![]));
    assert_eq!(f.windows[&wid].active, other);
}

#[test]
fn input_option_clipboard_request_buffer_and_off() {
    let mut f = fixture(); let pane = f.pane; let generation = f.panes[&pane].generation;
    f.clients.get_mut(&1).unwrap().features.0.insert("clipboard".into());
    f.opts_server.set("set-clipboard", "on".into());
    f.add_buffer(None, b"stored".to_vec(), false);
    f.pane_data(pane, generation, b"\x1b]52;c;?\x07");
    assert_eq!(f.pane_input.try_recv().unwrap(), b"\x1b]52;c;c3RvcmVk\x07");
    f.pane_data(pane, generation, b"\x1b]52;c;?\x1b\\");
    assert_eq!(f.pane_input.try_recv().unwrap(), b"\x1b]52;c;c3RvcmVk\x1b\\");
    f.opts_server.set("get-clipboard", "off".into());
    f.pane_data(pane, generation, b"\x1b]52;c;?\x07"); assert!(f.pane_input.try_recv().is_err());
    f.output.try_iter().for_each(drop);
    f.opts_server.set("get-clipboard", "request".into());
    f.pane_data(pane, generation, b"\x1b]52;c;?\x07");
    assert!(f.output.try_recv().unwrap().ends_with(b"\x1b]52;;?\x07")); // tty_clipboard_query
    f.handle_input(1, b"\x1b]52;c;bmV3\x07");
    assert_eq!(f.pane_input.try_recv().unwrap(), b"\x1b]52;c;bmV3\x07"); assert_eq!(f.buffers[0].data, b"stored");
    f.opts_server.set("get-clipboard", "both".into());
    f.pane_data(pane, generation, b"\x1b]52;c;?\x07");
    f.handle_input(1, b"\x1b]52;c;bmV3\x1b\\"); assert_eq!(f.buffers[0].data, b"new");
}

#[test]
fn input_option_lock_history_default_command_alias_and_clear_attach() {
    let mut f = fixture(); let sid = f.sid;
    f.opts_session.set("lock-after-time", "1".into());
    f.sessions.get_mut(&sid).unwrap().activity = now_secs().saturating_sub(2);
    f.sessions.get_mut(&sid).unwrap().options.set("lock-command", "session-lock".into());
    f.input_timers(); assert!(f.clients[&1].suspended);
    assert!(f.output.try_iter().any(|frame| frame[0] == S_SHELL && frame.ends_with(b"session-lock")));
    f.client_msg(1, C_RESUMED, vec![]); f.input_timers(); assert!(!f.clients[&1].suspended);
    run(&mut f, &["lock-session"]); assert!(f.clients[&1].suspended);
    f.client_msg(1, C_RESUMED, vec![]);
    let path = std::env::temp_dir().join(format!("pwsh-input-history-{}-{}", std::process::id(), now_ms()));
    f.opts_server.set("history-file", path.to_string_lossy().into_owned());
    f.prompt_history = [vec!["first".into(), "second".into()], vec!["needle".into()]]; f.save_prompt_history();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "command:first\ncommand:second\nsearch:needle\n");
    f.prompt_history = Default::default(); f.load_prompt_history();
    assert_eq!(f.prompt_history, [vec!["first".to_string(), "second".into()], vec!["needle".into()]]); std::fs::remove_file(path).unwrap();
    run(&mut f, &["set", "-s", "command-alias[100]", "alias=set -g @first yes ; set -g"]);
    run(&mut f, &["alias", "@last", "with spaces"]);
    assert_eq!(f.opts_session.get("@first"), Some("yes")); assert_eq!(f.opts_session.get("@last"), Some("with spaces"));
    f.opts_server.set("default-client-command", "set -g @default-client yes".into());
    f.client_msg(1, C_COMMAND, vec![]); assert_eq!(f.opts_session.get("@default-client"), Some("yes"));
    f.clients.get_mut(&1).unwrap().attached = false;
    f.opts_server.set("clear-on-attach", "off".into()); f.output.try_iter().for_each(drop); f.attach_client(1, sid);
    assert!(f.output.try_iter().any(|frame| frame[0] == S_ATTACHED && frame.ends_with(b"no-clear")));
}

#[test]
fn input_option_remain_on_exit_key_and_failed_key() {
    for (setting, status, keep) in [("key", 0, true), ("failed-key", 1, true), ("failed-key", 0, false)] {
        let mut f = fixture(); let pane = f.pane;
        f.panes.get_mut(&pane).unwrap().options.set("remain-on-exit", setting.into());
        f.pane_exited(pane, Some(status));
        assert_eq!(f.panes.contains_key(&pane), keep);
        if keep {
            f.dispatch_input(1, keys::Input::Paste(b"paste".to_vec())); assert!(f.panes.contains_key(&pane));
            f.handle_input(1, b"x"); assert!(!f.panes.contains_key(&pane)); assert!(f.sessions.is_empty());
        }
    }
}

fn frames(f: &Fixture, kind: u8) -> Vec<u8> {
    f.output.try_iter().filter(|frame| frame[0] == kind).flat_map(|frame| frame[5..].to_vec()).collect()
}

#[test]
fn input_user_keys_match_tmux_names_ranges_and_tree() {
    let mut f = fixture();
    assert_eq!(keys::parse_key_name("User1000"), Some(Key::plain(KeyCode::User(1000))));
    assert!(keys::parse_key_name("User1001").is_none() && keys::parse_key_name("user5").is_none());
    f.opts_server.set("user-keys[1000]", "kk".into());
    run(&mut f, &["bind-key", "-n", "User1000", "set-option -g @u1000 hit"]);
    f.handle_input(1, b"kk"); assert_eq!(f.opts_session.get("@u1000"), Some("hit"));
    // an unbound user key writes nothing to the pane, typed or sent
    f.opts_server.set("user-keys[5]", "zq".into());
    f.pane_input.try_iter().for_each(drop);
    f.handle_input(1, b"zq"); assert!(f.pane_input.try_recv().is_err());
    run(&mut f, &["send-keys", "User5", "x"]); assert_eq!(f.pane_input.try_recv().unwrap(), b"x");
    // the longest match wins: a user key ESC does not swallow built-in sequences
    let mut p = keys::Parser::new();
    p.set_user_keys(vec![(1, b"\x1b".to_vec())]);
    assert!(matches!(&p.feed(b"\x1b[A")[..], [keys::Input::Key(Key { code: KeyCode::Up, .. }, _)]));
    assert!(p.feed(b"\x1b").is_empty());
    assert_eq!(p.flush_pending(), vec![keys::Input::Key(Key::plain(KeyCode::User(1)), b"\x1b".to_vec())]);
    p.set_user_keys(vec![(2, b"\x1b[9".to_vec())]);
    assert!(matches!(&p.feed(b"\x1b[9x")[..], [keys::Input::Key(Key { code: KeyCode::User(2), .. }, _), keys::Input::Key(Key { code: KeyCode::Char('x'), .. }, _)]));
}

#[test]
fn input_meta_bracket_is_a_key_and_only_osc_52_is_held() {
    let mut p = keys::Parser::new();
    assert!(p.feed(b"\x1b]").is_empty());
    assert!(matches!(&p.flush_pending()[..], [keys::Input::Key(Key { code: KeyCode::Char(']'), mods: Mods { meta: true, .. } }, _)]));
    assert!(matches!(&p.feed(b"\x1b]x")[..], [keys::Input::Key(Key { code: KeyCode::Char(']'), mods: Mods { meta: true, .. } }, _), keys::Input::Key(Key { code: KeyCode::Char('x'), .. }, _)]));
    assert!(p.feed(b"\x1b]5").is_empty());
    assert_eq!(p.feed(b"2;c;YQ==\x07"), vec![keys::Input::Unknown(b"\x1b]52;c;YQ==\x07".to_vec())]);
    let mut f = fixture();
    run(&mut f, &["bind-key", "-n", "M-]", "set-option -g @hit yes"]);
    f.handle_input(1, b"\x1b]x"); assert_eq!(f.opts_session.get("@hit"), Some("yes"));
}

#[test]
fn input_backspace_and_extended_keys_follow_input_key() {
    let mut f = fixture();
    f.opts_server.set("backspace", "M-x".into());
    assert_eq!(encode(&f, "BSpace"), b"");
    f.opts_server.set("backspace", "F1".into());
    assert_eq!(encode(&f, "BSpace"), b"\x08"); // KEYC_F1 truncated to a byte
    f.opts_server.set("backspace", "C-?".into());
    assert_eq!(encode(&f, "BSpace"), b"\x7f");
    // C-BSpace has no standard form: send-keys falls back to the key name
    assert_eq!(encode_opt(&f, "C-BSpace"), None);
    f.pane_input.try_iter().for_each(drop);
    run(&mut f, &["send-keys", "C-BSpace"]); assert_eq!(f.pane_input.try_recv().unwrap(), b"C-BSpace");
    let pane = f.pane;
    f.opts_server.set("extended-keys", "on".into());
    f.panes.get_mut(&pane).unwrap().parser.callbacks_mut().ext_keys = 1;
    assert_eq!(encode(&f, "C-S-a"), b"\x01"); // mode 1 keeps Ctrl letters standard
    assert_eq!(encode(&f, "C-BSpace"), b"\x1b[27;5;127~");
    f.panes.get_mut(&pane).unwrap().parser.callbacks_mut().ext_keys = 2;
    assert_eq!(encode(&f, "C-S-a"), b"\x1b[27;6;97~");
    f.panes.get_mut(&pane).unwrap().parser.callbacks_mut().ext_keys = 0;
    f.opts_server.set("extended-keys", "always".into());
    f.opts_server.set("extended-keys-format", "csi-u".into());
    assert_eq!(encode(&f, "C-S-a"), b"\x01");
    assert_eq!(encode(&f, "C-Space"), b"\x00");
    assert_eq!(encode(&f, "C-1"), b"\x1b[49;5u");
}

#[test]
fn input_remain_on_exit_key_runs_bindings_first() {
    let mut f = fixture(); let pane = f.pane;
    f.opts_session.set("remain-on-exit", "key".into());
    run(&mut f, &["new-window", "-d"]);
    f.panes.get_mut(&pane).unwrap().options.set("remain-on-exit", "key".into());
    f.pane_exited(pane, Some(0));
    let windows = f.sessions[&f.sid].links.len();
    f.handle_input(1, b"\x02c"); // the prefix and a bound key reach the key tables
    assert!(f.panes.contains_key(&pane)); assert_eq!(f.sessions[&f.sid].links.len(), windows + 1);
    run(&mut f, &["select-window", "-t", ":0"]);
    f.clients.get_mut(&1).unwrap().readonly = true;
    f.handle_input(1, b"x"); assert!(!f.panes.contains_key(&pane)); // read-only clients may dismiss it
}

#[test]
fn input_terminal_features_parse_like_tty_parse_features() {
    let f = features::compute("xterm-256color", "", &["xterm*:@mouse:bogus:sync".into()], "");
    assert!(f.has("mouse") && !f.has("bogus") && !f.has("sync"));
    let f = features::compute("xterm-256color", "", &["xterm*:title@".into(), "xterm*:title".into()], "");
    assert!(!f.has("title"));
    let f = features::compute("xterm-256color", "", &["xterm*:RGB:appesc:clipboard".into()], "");
    assert_eq!(f.list(), "256,appesc,bpaste,clipboard,mouse,RGB,strikethrough");
    assert!(!features::compute("xterm-256color", "", &[], "").has("utf8"));
    assert!(features::compute_named("xterm-256color", "tmux", "", &[], "").has("extkeys"));
    assert!(features::compute_named("xterm-256color", "iTerm.app", "", &[], "").has("osc7"));
    // `::` in overrides is a literal colon
    assert_eq!(features::override_tokens("xterm*:Smulx=\\E[4::%p1%dm:Ms@"), ["xterm*", "Smulx=\\E[4:%p1%dm", "Ms@"]);
    let mut feats = features::Features::default();
    let caps = features::overrides("xterm-256color", &["xterm*:Cs=\\E]12;a::b%p1%s\\007".into()], &mut feats);
    assert_eq!(caps.get("Cs").map(String::as_str), Some("\x1b]12;a:b%p1%s\x07"));
    assert!(!caps.contains_key("b%p1%s\\007"));
}

#[test]
fn input_client_utf8_comes_from_the_client_and_draws_underscores() {
    let mut f = fixture(); let pane = f.pane; let generation = f.panes[&pane].generation;
    f.clients.get_mut(&1).unwrap().ident = Some(Identity { cols: 80, rows: 24, tty: true, cwd: String::new(), term: "xterm-256color".into(), pid: 0, ctx_pane: None, utf8: false, env: vec![] });
    f.opts_session.set("status", "off".into());
    f.pane_data(pane, generation, "a\u{e9}b\u{4e2d}c".as_bytes());
    assert_eq!(f.expand(Default::default(), Some(1), "#{client_utf8}"), "0");
    f.output.try_iter().for_each(drop); f.render(1);
    let out = String::from_utf8_lossy(&frames(&f, S_OUTPUT)).into_owned();
    assert!(out.contains("a_b__c"), "{out:?}");
    // the utf8 feature turns UTF-8 on; utf8@ never turns it off
    f.clients.get_mut(&1).unwrap().features.0.insert("utf8".into());
    assert_eq!(f.expand(Default::default(), Some(1), "#{client_utf8}"), "1");
    f.clients.get_mut(&1).unwrap().features.0.remove("utf8");
    f.clients.get_mut(&1).unwrap().ident.as_mut().unwrap().utf8 = true;
    f.clients.get_mut(&1).unwrap().prev = None; f.output.try_iter().for_each(drop); f.render(1);
    assert!(String::from_utf8_lossy(&frames(&f, S_OUTPUT)).contains("a\u{e9}b\u{4e2d}c"));
}

#[test]
fn input_osc_52_buffer_replies_sets_and_selection_writes() {
    let mut f = fixture(); let pane = f.pane; let generation = f.panes[&pane].generation;
    f.clients.get_mut(&1).unwrap().features.0.insert("clipboard".into());
    f.opts_server.set("set-clipboard", "on".into());
    f.opts_server.set("get-clipboard", "buffer".into());
    run(&mut f, &["set-buffer", "-b", "named", "namedval"]);
    f.pane_data(pane, generation, b"\x1b]52;c;?\x07"); assert!(f.pane_input.try_recv().is_err()); // named buffers are not used
    f.add_buffer(None, b"autoval".to_vec(), false);
    f.pane_data(pane, generation, b"\x1b]52;pc;?\x1b\\"); assert_eq!(f.pane_input.try_recv().unwrap(), b"\x1b]52;p;YXV0b3ZhbA==\x1b\\");
    f.pane_data(pane, generation, b"\x1b]52;x;?\x07"); assert_eq!(f.pane_input.try_recv().unwrap(), b"\x1b]52;;YXV0b3ZhbA==\x07");
    // an application's selection becomes a buffer and goes to the terminal with Ms
    f.clients.get_mut(&1).unwrap().term_caps.insert("Ms".into(), "\x1b]52;%p1%s;X%p2%s\x1b\\".into());
    f.output.try_iter().for_each(drop);
    f.pane_data(pane, generation, b"\x1b]52;cx;ZGVm\x07");
    assert_eq!(f.buffers[0].data, b"def"); assert!(f.buffers[0].automatic);
    assert!(frames(&f, S_OUTPUT).ends_with(b"\x1b]52;c;XZGVm\x1b\\"));
    f.opts_server.set("set-clipboard", "external".into());
    f.pane_data(pane, generation, b"\x1b]52;c;Z2hp\x07"); assert_eq!(f.buffers[0].data, b"def");
    // set-buffer -w goes to the target client through Ms with an empty selection
    run(&mut f, &["set-buffer", "-w", "hello"]);
    assert!(frames(&f, S_OUTPUT).ends_with(b"\x1b]52;;XaGVsbG8=\x1b\\"));
}

#[test]
fn input_cursor_colour_and_terminal_reset_on_exit() {
    let mut f = fixture(); let pane = f.pane;
    f.clients.get_mut(&1).unwrap().term_caps.insert("Se".into(), "\x1b[99 q".into());
    f.panes.get_mut(&pane).unwrap().options.set("cursor-colour", "#FF0000".into());
    f.panes.get_mut(&pane).unwrap().options.set("cursor-style", "bar".into());
    f.output.try_iter().for_each(drop); f.render(1);
    let out = frames(&f, S_OUTPUT);
    assert!(out.windows(18).any(|w| w == b"\x1b]12;rgb:ff/00/00\x07"), "{:?}", String::from_utf8_lossy(&out));
    f.panes.get_mut(&pane).unwrap().options.set("cursor-colour", "blue".into());
    f.clients.get_mut(&1).unwrap().prev = None; f.render(1);
    assert!(frames(&f, S_OUTPUT).windows(18).any(|w| w == b"\x1b]12;rgb:00/00/80\x07"));
    f.panes.get_mut(&pane).unwrap().options.set("cursor-colour", "default".into());
    f.clients.get_mut(&1).unwrap().prev = None; f.render(1);
    assert!(frames(&f, S_OUTPUT).windows(6).any(|w| w == b"\x1b]112\x07"));
    f.panes.get_mut(&pane).unwrap().options.set("cursor-colour", "green".into());
    f.clients.get_mut(&1).unwrap().prev = None; f.render(1); f.output.try_iter().for_each(drop);
    f.clients[&1].send_exit("");
    let out = frames(&f, S_OUTPUT);
    assert_eq!(out, b"\x1b[99 q\x1b]112\x07"); // Se and Cr, no appesc reset without the feature
}

#[test]
fn input_prompt_history_types_paths_and_save_at_exit() {
    let mut f = fixture();
    let path = std::env::temp_dir().join(format!("pwsh-input-typed-{}-{}", std::process::id(), now_ms()));
    std::fs::write(&path, "command:display -p hi\nsearch:needle\nplain line\nbogus:x\n").unwrap();
    f.opts_server.set("history-file", path.to_string_lossy().into_owned());
    f.load_prompt_history();
    assert_eq!(f.prompt_history[0], ["display -p hi", "plain line", "bogus:x"]); assert_eq!(f.prompt_history[1], ["needle"]);
    run(&mut f, &["command-prompt", "-T", "search", "-I", "found", "set -g @s '%%'"]);
    f.handle_input(1, b"\r"); assert_eq!(f.prompt_history[1], ["needle", "found"]);
    assert!(std::fs::read_to_string(&path).unwrap().starts_with("command:display")); // untouched until exit
    let mut ctx = Ctx::default();
    assert_eq!(f.exec(vec!["command-prompt".into(), "-T".into(), "target".into()], &mut ctx).err().as_deref(), Some("unknown type: target"));
    run(&mut f, &["clear-prompt-history", "-T", "search"]); assert!(f.prompt_history[1].is_empty());
    std::fs::remove_file(&path).unwrap();
    f.opts_server.set("history-file", "relative-history".into());
    f.save_prompt_history(); assert!(!std::path::Path::new("relative-history").exists());
}

#[test]
fn input_vi_prompt_command_mode_keys() {
    let mut f = fixture();
    f.opts_session.set("status-keys", "vi".into());
    let esc = |f: &mut Fixture| f.dispatch_input(1, keys::Input::Key(Key::plain(KeyCode::Escape), vec![27]));
    let edit = |f: &mut Fixture, init: &str, keys: &[u8]| -> String {
        run(f, &["command-prompt", "-I", init, "set -g @out '%%'"]);
        esc(f); f.handle_input(1, keys);
        f.clients[&1].prompt.as_ref().map(|p| p.input.iter().collect()).unwrap_or_default()
    };
    assert_eq!(edit(&mut f, "hello", b"X"), "helo");
    f.clients.get_mut(&1).unwrap().prompt = None;
    assert_eq!(edit(&mut f, "set -g @w hello", b"0wDAz"), "set z");
    f.clients.get_mut(&1).unwrap().prompt = None;
    assert_eq!(edit(&mut f, "one two three", b"0wwD"), "one two ");
    f.clients.get_mut(&1).unwrap().prompt = None;
    assert_eq!(edit(&mut f, "one two", b"bC"), "one ");
    f.clients.get_mut(&1).unwrap().prompt = None;
    assert_eq!(edit(&mut f, "abc", b"d"), "");
    f.clients.get_mut(&1).unwrap().prompt = None;
    edit(&mut f, "abc", b"q"); assert!(f.clients[&1].prompt.is_none());
    f.add_buffer(None, b"pasted".to_vec(), false);
    assert_eq!(edit(&mut f, "", b"p"), "pasted");
    f.clients.get_mut(&1).unwrap().prompt = None;
    assert_eq!(edit(&mut f, "abc def", b"0eiX"), "abXc def");
}

#[test]
fn input_bindings_parse_aliases_and_locked_clients_are_not_listed() {
    let mut f = fixture();
    run(&mut f, &["set", "-s", "command-alias[100]", "foo=set -g @a"]);
    run(&mut f, &["bind-key", "x", "foo", "bar"]);
    assert_eq!(f.bindings["prefix"]["x"].cmd, "set-option -g @a bar");
    let mut ctx = Ctx::default();
    assert_eq!(f.exec(vec!["bind-key".into(), "y".into(), "bogusalias".into()], &mut ctx).err().as_deref(), Some("unknown command: bogusalias"));
    let sid = f.sid; f.sessions.get_mut(&sid).unwrap().options.set("lock-command", "true".into());
    run(&mut f, &["lock-client"]);
    let mut ctx = Ctx::default();
    f.exec(vec!["list-clients".into(), "-F".into(), "x".into()], &mut ctx).unwrap();
    assert_eq!(&*ctx.out, "");
}

#[test]
fn input_buffer_size_is_a_power_of_two() {
    let mut p = vt100::Parser::new_with_callbacks(nz(3), nz(20), 0, PaneCb::default());
    p.set_input_buffer_size(2_000_000);
    let title = "t".repeat(1_100_000);
    p.process(format!("\x1b]2;{title}\x07").as_bytes()); assert!(p.callbacks().title.is_none());
    let title = "t".repeat(1_000_000);
    p.process(format!("\x1b]2;{title}\x07").as_bytes()); assert_eq!(p.callbacks().title.as_ref().map(String::len), Some(1_000_000));
}
