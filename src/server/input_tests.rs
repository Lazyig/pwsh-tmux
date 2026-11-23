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
    // always forces mode 1 only at a screen reset, not in a pane that exists already
    assert_eq!(encode(&f, "C-1"), b"1");
    let g = f.panes[&pane].generation;
    f.pane_data(pane, g, b"\x1bc");
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
            // tmux 3.8 (checked on the pod): any key closes it, a pasted one too
            // (server_client_handle_dead_key skips only the paste start and end markers)
            if setting == "key" { f.handle_input(1, b"x"); } else { f.dispatch_input(1, keys::Input::Paste(b"paste".to_vec())); }
            assert!(!f.panes.contains_key(&pane)); assert!(f.sessions.is_empty());
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
    let g = f.panes[&pane].generation;
    f.pane_data(pane, g, b"\x1bc");
    assert_eq!(encode(&f, "C-S-a"), b"\x01");
    assert_eq!(encode(&f, "C-Space"), b"\x00");
    assert_eq!(encode(&f, "C-1"), b"\x1b[49;5u");
}

#[test]
fn input_remain_on_exit_key_closes_before_bindings() {
    let mut f = fixture(); let pane = f.pane;
    f.opts_session.set("remain-on-exit", "key".into());
    run(&mut f, &["new-window", "-d"]);
    f.panes.get_mut(&pane).unwrap().options.set("remain-on-exit", "key".into());
    f.pane_exited(pane, Some(0));
    let windows = f.sessions[&f.sid].links.len();
    // server_client_handle_key: the prefix itself closes it (tmux 3.8, checked on the pod),
    // and the next key goes on to the window now current, not to the prefix table
    f.handle_input(1, b"\x02c");
    assert!(!f.panes.contains_key(&pane)); assert_eq!(f.sessions[&f.sid].links.len(), windows - 1);
    let mut f = fixture(); let pane = f.pane;
    run(&mut f, &["new-window", "-d"]);
    f.panes.get_mut(&pane).unwrap().options.set("remain-on-exit", "key".into());
    f.pane_exited(pane, Some(0));
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
    // RGB alone is not the 256 feature, and terminfo colors#256 is not either (tty_get_features)
    let f = features::compute("xterm-256color", "", &["xterm*:RGB:appesc:clipboard".into()], "");
    assert_eq!(f.list(), "appesc,bpaste,clipboard,mouse,RGB,strikethrough");
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
fn input_detach_resets_cursor_and_application_escape() {
    // detach_client clears `attached` before the exit message; the reset must still be sent
    let mut f = fixture(); let pane = f.pane;
    f.clients.get_mut(&1).unwrap().features.parse("appesc", &[',']);
    f.panes.get_mut(&pane).unwrap().options.set("cursor-style", "block".into());
    f.clients.get_mut(&1).unwrap().prev = None; f.render(1);
    assert!(frames(&f, S_OUTPUT).windows(8).any(|w| w == b"\x1b[?7727h"));
    f.detach_client(1, "[detached]");
    let out = frames(&f, S_OUTPUT);
    assert_eq!(out, b"\x1b[0 q\x1b[?7727l", "{:?}", String::from_utf8_lossy(&out));
    // nothing for a client whose terminal was never given anything
    let mut f = fixture();
    f.clients.get_mut(&1).unwrap().features.parse("appesc", &[',']);
    f.output.try_iter().for_each(drop);
    f.detach_client(1, "[detached]");
    assert!(frames(&f, S_OUTPUT).is_empty());
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
fn pane_prompts_wait_and_use_the_typed_history() {
    let mut f = fixture();
    // command-prompt -P without -b waits like a status line prompt (CMD_RETURN_WAIT)
    run(&mut f, &["command-prompt", "-P", "-T", "search", "-I", "pane", "set -g @pp '%%'"]);
    assert_eq!(f.prompt_waits.len(), 1);
    let pane = f.pane;
    assert!(f.panes[&pane].prompt.is_some() && f.clients[&1].prompt.is_none());
    f.timers(); assert_eq!(f.prompt_waits.len(), 1);
    f.handle_input(1, b"\r");
    assert_eq!(f.opts_session.get("@pp"), Some("pane"));
    assert_eq!(f.prompt_history[1], ["pane"]);
    f.timers(); assert!(f.prompt_waits.is_empty());
    // with -b nothing waits
    run(&mut f, &["command-prompt", "-b", "set -g @b '%%'"]);
    assert!(f.prompt_waits.is_empty() && f.clients[&1].prompt.is_some());
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

fn feed(f: &mut Fixture, data: &[u8]) { let pane = f.pane; let g = f.panes[&pane].generation; f.pane_data(pane, g, data); }
fn row0(f: &Fixture) -> String { f.panes[&f.pane].screen().contents().lines().next().unwrap_or("").to_string() }
fn title(f: &Fixture) -> String { f.panes[&f.pane].title.clone() }

#[test]
fn control_strings_end_and_dispatch_as_tmux_does() {
    let mut f = fixture();
    // an OSC ends on any ESC and on CAN/SUB, and is dispatched either way
    feed(&mut f, b"\x1bcA\x1b]2;abc\x1b[31mRED\x1b[0m|B");
    assert_eq!((title(&f), row0(&f)), ("abc".to_string(), "ARED|B".to_string()));
    feed(&mut f, b"\x1bcC\x1b]2;def\x18XY|");
    assert_eq!((title(&f), row0(&f)), ("def".to_string(), "CXY|".to_string()));
    // C0 controls inside are dropped, DEL makes the title invalid, '#(' and '\' are made safe
    feed(&mut f, b"\x1b]2;ab\ncd\x07"); assert_eq!(title(&f), "abcd");
    feed(&mut f, b"\x1b]2;ab\x7fcd\x07"); assert_eq!(title(&f), "abcd");
    feed(&mut f, b"\x1b]2;a\\b#(x)\x07"); assert_eq!(title(&f), if cfg!(windows) { "a\\b_(x)" } else { "a\\\\b_(x)" });
    feed(&mut f, b"\x1b]0;q;r\x1b\\"); assert_eq!(title(&f), "q;r");
    // APC sets the title too
    feed(&mut f, b"\x1b_apctitle\x1b\\"); assert_eq!(title(&f), "apctitle");
    let pane = f.pane;
    f.panes.get_mut(&pane).unwrap().options.set("allow-set-title", "off".into());
    feed(&mut f, b"\x1b_other\x1b\\\x1b]2;other\x07"); assert_eq!(title(&f), "apctitle");
    // a DCS keeps CAN once its data started; only ESC \ ends it, and ESC x keeps x
    feed(&mut f, b"\x1bcA\x1bPzz\x18B|"); assert_eq!(row0(&f), "A");
    feed(&mut f, b"\x1b\\C"); assert_eq!(row0(&f), "AC");
    f.panes.get_mut(&pane).unwrap().options.set("allow-passthrough", "on".into());
    let mut p = vt100::Parser::new_with_callbacks(nz(3), nz(20), 0, PaneCb::default());
    p.process(b"\x1bPtmux;\x1b\x1b]0;x\x07\x1b\\");
    assert_eq!(p.callbacks().passthrough, vec![b"\x1b]0;x\x07".to_vec()]);
    // before its final byte a DCS is cancelled by CAN
    p.process(b"\x1bP1;2\x18Z"); assert_eq!(p.screen().contents(), "Z");
    // bytes that are not UTF-8, and C1 bytes, show as U+FFFD
    feed(&mut f, b"\x1bcA\x9b1mB|"); assert_eq!(row0(&f), "A\u{fffd}1mB|");
    feed(&mut f, b"\x1bcA\xc3(B\xffC|"); assert_eq!(row0(&f), "A\u{fffd}(B\u{fffd}C|");
    // a C1 character sent as UTF-8 is kept with no width: it joins the cell before, and is
    // dropped at the start of a line (tmux 3.8 capture-pane: "a\302\205b")
    feed(&mut f, "\x1bca\u{85}b|\u{80}\u{9f}c\r\n\u{85}x".as_bytes());
    assert_eq!(f.panes[&f.pane].screen().contents(), "a\u{85}b|\u{80}\u{9f}c\nx");
    // DECALN
    feed(&mut f, b"\x1bc\x1b[3;3HA\x1b#8|");
    assert_eq!(row0(&f), format!("|{}", "E".repeat(f.panes[&pane].size.0 as usize - 1)));
    assert_eq!(f.panes[&pane].screen().cursor_position(), (0, 1));
}

#[test]
fn screen_k_renames_the_window_only_with_allow_rename() {
    let mut f = fixture(); let pane = f.pane;
    let wid = f.window_of(pane).unwrap();
    let name = |f: &Fixture| f.windows[&wid].name.clone();
    let before = name(&f);
    // OSC titles never rename the window; ESC k is ignored with allow-rename off
    f.panes.get_mut(&pane).unwrap().options.set("allow-rename", "on".into());
    feed(&mut f, b"\x1bcA\x1b]2;T\x07\x1bkK#(1)\x1b\\B|");
    assert_eq!((title(&f), name(&f), row0(&f)), ("T".to_string(), "K_(1)".to_string(), "AB|".to_string()));
    assert_eq!(f.windows[&wid].options.get("automatic-rename"), Some("off"));
    // an empty name goes back to automatic-rename's inherited value
    feed(&mut f, b"\x1bk\x1b\\");
    assert_eq!(f.windows[&wid].options.get("automatic-rename"), None);
    assert_eq!(name(&f), "K_(1)");
    f.opts_window.set("automatic-rename", "off".into());
    feed(&mut f, b"\x1bk\x1b\\"); assert_eq!(name(&f), "");
    f.panes.get_mut(&pane).unwrap().options.set("allow-rename", "off".into());
    feed(&mut f, b"\x1bkZ\x1b\\"); assert_eq!(name(&f), "");
    let _ = before;
}

#[test]
fn alternate_screen_modes_match_tmux() {
    let mut f = fixture(); let pane = f.pane;
    let pos = |f: &Fixture| f.panes[&f.pane].screen().cursor_position();
    let rows = |f: &Fixture| f.panes[&f.pane].screen().contents().lines().map(|l| l.trim_end().to_string()).filter(|l| !l.is_empty()).collect::<Vec<_>>();
    // 1047 is an alternate screen without a saved cursor
    feed(&mut f, b"\x1bcabc\r\n\x1b[?1047hALT\x1b[?1047lZ");
    assert_eq!((rows(&f), pos(&f)), (vec!["abc".to_string(), "   Z".into()], (1, 4)));
    // the 1049 cursor is not the DECSC one
    feed(&mut f, b"\x1bc\x1b[3;3H\x1b7\x1b[6;6Hq\x1b[?1049h\x1b[?1049lW\x1b8");
    assert_eq!(pos(&f), (2, 2));
    // alternate-screen off: none of them do anything, 1049l does not restore the cursor
    f.panes.get_mut(&pane).unwrap().options.set("alternate-screen", "off".into());
    feed(&mut f, b"\x1bcabc\r\n\r\n\x1b[5;10H\x1b[?1049hX\x1b[?1049lY");
    assert_eq!((rows(&f), pos(&f)), (vec!["abc".to_string(), "         XY".into()], (4, 11)));
    feed(&mut f, b"\x1bc\x1b[3;3H\x1b7\x1b[6;6Hq\x1b[?1049h\x1b[?1049lW");
    assert_eq!(pos(&f), (5, 7));
}

fn encode_raw(f: &Fixture, raw: &[u8]) -> Vec<u8> {
    let mut p = keys::Parser::new();
    let mut out = Vec::new();
    for i in p.feed(raw).into_iter().chain(p.flush_pending()) {
        match i {
            keys::Input::Key(k, raw) => out.extend(f.encode_pane_key(f.pane, &k, raw).unwrap_or_default()),
            keys::Input::Unknown(raw) => out.extend(raw),
            _ => {}
        }
    }
    out
}

#[test]
fn extended_keys_from_the_terminal_keep_the_key_tmux_parsed() {
    let mut f = fixture(); let pane = f.pane;
    f.opts_server.set("extended-keys", "on".into());
    feed(&mut f, b"\x1b[>4;2m");
    assert_eq!(f.panes[&pane].parser.callbacks().ext_keys, 2);
    for (input, want) in [
        (&b"\x1b[105;5u"[..], &b"\x1b[27;5;105~"[..]), (b"\x1b[27;5;105~", b"\x1b[27;5;105~"), (b"\x1b[109;5u", b"\x1b[27;5;109~"),
        (b"\x1b[64;5u", b"\x1b[27;5;64~"), (b"\x1b[65;5u", b"\x1b[27;5;65~"), (b"\x1b[27;6;65~", b"\x1b[27;6;65~"),
        (b"\x1b[9;6u", b"\x1b[27;6;9~"), (b"\x1b[Z", b"\x1b[27;2;9~"), (b"\x1b[9;2u", b"\x1b[27;2;9~"), (b"\x1b[91;5u", b"\x1b[27;5;91~"),
        // not keys for tmux: M-[ and text
        (b"\x1b[57399u", b"\x1b[27;3;91~57399u"), (b"\x1b[97;5:1u", b"\x1b[27;3;91~97;5:1u"),
        (b"\x1b[1;9A", b"\x1b[1;3A"),
    ] {
        assert_eq!(encode_raw(&f, input), want, "{input:?}");
    }
    // standard mode
    feed(&mut f, b"\x1b[>4n");
    for (input, want) in [
        (&b"\x1b[27;5u"[..], &b""[..]), (b"\x1b[105;5u", b"\t"), (b"\x1b[65;5u", b"\x01"), (b"\x1b[59;5u", b";"),
        (b"\x1b[57399u", b"\x1b[57399u"), (b"\x1b[97::65;6u", b"\x1b[97::65;6u"), (b"\x1b[1;9A", b"\x1b[1;3A"),
        (b"\x1b[9;6u", b"\x1b[Z"), (b"\x1bO5A", b"\x1bO5A"),
    ] {
        assert_eq!(encode_raw(&f, input), want, "{input:?}");
    }
    // a request made while extended-keys is off is ignored
    f.opts_server.set("extended-keys", "off".into());
    feed(&mut f, b"\x1b[>4;2m");
    assert_eq!(f.panes[&pane].parser.callbacks().ext_keys, 0);
}

#[test]
fn kp_enter_and_remapped_backspace_follow_input_key() {
    let mut f = fixture();
    assert_eq!(encode(&f, "KPEnter"), b"\n");
    assert_eq!(encode(&f, "M-KPEnter"), b"\x1b\n");
    f.opts_server.set("backspace", "F1".into());
    assert_eq!([encode(&f, "M-BSpace"), encode(&f, "C-BSpace"), encode(&f, "S-BSpace")], [b"\x1b\x1bOP".to_vec(), b"\x1b[1;5P".to_vec(), b"\x1b[1;2P".to_vec()]);
    f.opts_server.set("backspace", "Up".into());
    assert_eq!([encode(&f, "M-BSpace"), encode(&f, "C-BSpace")], [b"\x1b\x1b[A".to_vec(), b"\x1b[1;5A".to_vec()]);
    f.opts_server.set("backspace", "BSpace".into());
    assert_eq!([encode(&f, "BSpace"), encode(&f, "M-BSpace")], [b"\x07".to_vec(), b"".to_vec()]);
    assert_eq!(encode_opt(&f, "C-BSpace"), Some(vec![]));
    f.opts_server.set("backspace", "C-h".into());
    assert_eq!(encode(&f, "M-BSpace"), b"\x1b\x08");
}

#[test]
fn cursor_style_blink_colour_names_and_narrow_terminal_widths() {
    use super::render::{merged_cursor_style, non_utf8_text};
    // DECSET 12 makes the option's bar blink; DECSCUSR 4 then 12h is a blinking underline
    assert_eq!(merged_cursor_style(0, (true, true), 6), 5);
    assert_eq!(merged_cursor_style(0, (false, false), 6), 6);
    assert_eq!(merged_cursor_style(4, (true, true), 6), 3);
    assert_eq!(merged_cursor_style(3, (true, false), 0), 3);
    assert_eq!(merged_cursor_style(0, (true, true), 0), 0);
    // OSC 12 names are X11 colours
    assert_eq!(parse_x11_colour("blue").as_deref(), Some("#0000ff"));
    assert_eq!(parse_x11_colour(" red ").as_deref(), Some("#ff0000"));
    assert_eq!(parse_x11_colour("cmyk:0/1/1/0").as_deref(), Some("#ff0000"));
    let mut f = fixture();
    feed(&mut f, b"\x1b]12;blue\x07");
    assert_eq!(f.panes[&f.pane].parser.callbacks().osc.cursor_colour.as_deref(), Some("#0000ff"));
    // a character made wide by VS16 is two underscores on a terminal without UTF-8
    assert_eq!(non_utf8_text("\u{2764}\u{fe0f}", true), "__");
    assert_eq!(non_utf8_text("\u{e9}", false), "_");
}

#[test]
fn terminal_title_extended_keys_and_detach_use_capabilities() {
    let mut f = fixture(); let sid = f.sid;
    {
        let c = f.clients.get_mut(&1).unwrap();
        c.features = features::compute("xterm-256color", "", &[], "title,extkeys,bpaste,focus");
        for (k, v) in [("tsl", "\x1b]1;"), ("fsl", "\x07"), ("Eneks", "\x1b[>4;7m"), ("Dseks", "\x1b[>4;8m"), ("Dsbp", "\x1b[?9200l"), ("Dsfcs", "\x1b[?9004l")] {
            c.term_caps.insert(k.into(), v.into());
        }
    }
    f.opts_server.set("extended-keys", "on".into());
    f.sessions.get_mut(&sid).unwrap().options.set("set-titles", "on".into());
    f.sessions.get_mut(&sid).unwrap().options.set("set-titles-string", "TT".into());
    f.clients.get_mut(&1).unwrap().prev = None;
    f.output.try_iter().for_each(drop); f.render(1);
    let out = String::from_utf8_lossy(&frames(&f, S_OUTPUT)).into_owned();
    assert!(out.contains("\x1b]1;TT\x07") && !out.contains("\x1b]2;"), "{out:?}");
    assert!(out.contains("\x1b[>4;7m") && !out.contains("\x1b[>4;1m"), "{out:?}");
    // the default Eneks is mode 2 for extended-keys on
    f.clients.get_mut(&1).unwrap().term_caps.remove("Eneks");
    f.clients.get_mut(&1).unwrap().prev = None; f.render(1);
    assert!(String::from_utf8_lossy(&frames(&f, S_OUTPUT)).contains("\x1b[>4;2m"));
    f.clients[&1].send_exit("");
    let out = String::from_utf8_lossy(&frames(&f, S_OUTPUT)).into_owned();
    assert!(out.contains("\x1b[?9200l") && out.contains("\x1b[?9004l") && out.contains("\x1b[>4;8m"), "{out:?}");
}

#[test]
fn attach_asks_for_synchronized_output() {
    let mut f = fixture(); let sid = f.sid;
    f.clients.get_mut(&1).unwrap().ident = Some(Identity { cols: 80, rows: 24, tty: true, cwd: String::new(), term: "xterm-256color".into(), pid: 0, ctx_pane: None, utf8: true, env: Vec::new() });
    f.clients.get_mut(&1).unwrap().attached = false;
    f.clients.get_mut(&1).unwrap().terminal.vt100 = true;
    f.output.try_iter().for_each(drop); f.attach_client(1, sid);
    let out = frames(&f, S_OUTPUT);
    assert!(out.windows(9).any(|w| w == b"\x1b[?2026$p"), "{:?}", String::from_utf8_lossy(&out));
    assert!(!f.clients[&1].features.has("sync"));
    f.pane_input.try_iter().for_each(drop);
    f.handle_input(1, b"\x1b[?2026;2$y");
    assert!(f.clients[&1].features.has("sync"));
    assert!(f.pane_input.try_recv().is_err()); // the reply is not typed into the pane
    // tty_keys_sync: only the first reply is read (TTY_HAVESYNC)
    f.clients.get_mut(&1).unwrap().features.0.remove("sync");
    f.handle_input(1, b"\x1b[?2026;2$y");
    assert!(!f.clients[&1].features.has("sync"));
}

#[test]
fn attach_asks_for_primary_device_attributes() {
    // tty_send_requests asks for DA1; tty_keys_device_attributes adds margins, rectfill,
    // sixel and clipboard from a VT420 or later reply, once
    let mut f = fixture(); let sid = f.sid;
    f.clients.get_mut(&1).unwrap().ident = Some(Identity { cols: 80, rows: 24, tty: true, cwd: String::new(), term: "xterm-256color".into(), pid: 0, ctx_pane: None, utf8: true, env: Vec::new() });
    f.clients.get_mut(&1).unwrap().attached = false;
    f.clients.get_mut(&1).unwrap().terminal.vt100 = true;
    for feat in ["margins", "rectfill", "sixel"] {
        f.clients.get_mut(&1).unwrap().features.0.remove(feat);
    }
    f.output.try_iter().for_each(drop); f.attach_client(1, sid);
    let out = frames(&f, S_OUTPUT);
    assert!(out.windows(3).any(|w| w == b"\x1b[c"), "{:?}", String::from_utf8_lossy(&out));
    f.pane_input.try_iter().for_each(drop);
    f.handle_input(1, b"\x1b[?64;1;2;4;6;9;15;18;21;22;28c");
    let c = &f.clients[&1];
    assert!(c.features.has("margins") && c.features.has("rectfill") && c.features.has("sixel"), "{:?}", c.features);
    assert!(f.pane_input.try_recv().is_err());
}

#[test]
fn readonly_clients_read_terminal_replies() {
    // the DECRPM 2026 reply is a terminal reply, not input: read-only clients get sync too
    let mut f = fixture(); let sid = f.sid;
    f.clients.get_mut(&1).unwrap().ident = Some(Identity { cols: 80, rows: 24, tty: true, cwd: String::new(), term: "xterm-256color".into(), pid: 0, ctx_pane: None, utf8: true, env: Vec::new() });
    f.clients.get_mut(&1).unwrap().attached = false;
    f.clients.get_mut(&1).unwrap().terminal.vt100 = true;
    f.attach_client(1, sid);
    f.clients.get_mut(&1).unwrap().readonly = true;
    f.clients.get_mut(&1).unwrap().features.0.remove("sync");
    f.handle_input(1, b"\x1b[?2026;1$y");
    assert!(f.clients[&1].features.has("sync"));
}

#[test]
fn pasted_input_never_reaches_a_pane_in_a_mode() {
    let mut f = fixture(); let pane = f.pane;
    f.clients.get_mut(&1).unwrap().features.0.remove("bpaste");
    f.opts_session.set("assume-paste-time", "1000".into());
    run(&mut f, &["copy-mode"]);
    f.pane_input.try_iter().for_each(drop);
    f.handle_input(1, b"abcdefq");
    f.handle_input(1, b"\x1b[200~hello\x1b[201~");
    assert!(f.pane_input.try_recv().is_err());
    assert!(f.panes[&pane].mode.is_some());
    run(&mut f, &["send-keys", "-X", "cancel"]);
    // the paste-start key ends clock mode, then the text goes to the pane
    run(&mut f, &["clock-mode"]);
    f.handle_input(1, b"\x1b[200~hi\x1b[201~");
    assert!(f.panes[&pane].mode.is_none());
    assert_eq!(f.pane_input.try_iter().flatten().collect::<Vec<u8>>(), b"hi");
    // clock mode stacked on copy mode: ending it brings copy mode back, which drops
    // the text (tmux 3.8, pod)
    run(&mut f, &["copy-mode"]);
    run(&mut f, &["clock-mode"]);
    f.pane_input.try_iter().for_each(drop);
    f.handle_input(1, b"\x1b[200~b1\r\x1b[201~");
    assert!(matches!(f.panes[&pane].mode, Some(PaneMode::Copy(_))));
    assert!(f.pane_input.try_recv().is_err());
    run(&mut f, &["send-keys", "-X", "cancel"]);
    // a dead pane kept by remain-on-exit key closes on typed (or guessed pasted) input
    // too: server_client_handle_key checks it before the paste guess (tmux 3.8, pod)
    f.panes.get_mut(&pane).unwrap().options.set("remain-on-exit", "key".into());
    run(&mut f, &["new-window", "-d"]);
    f.pane_exited(pane, Some(0));
    f.handle_input(1, b"xyz");
    assert!(!f.panes.contains_key(&pane));
}

#[test]
fn repeat_ends_on_a_binding_that_does_not_repeat() {
    let mut f = fixture();
    f.opts_session.set("repeat-time", "2000".into());
    run(&mut f, &["bind-key", "-r", "a", "set-option -ga @r a"]);
    run(&mut f, &["bind-key", "z", "set-option -ga @r z"]);
    f.opts_session.set("@r", "".into());
    f.pane_input.try_iter().for_each(drop);
    // C-b a z: z is not a repeating binding, so it goes to the root table (and the pane)
    f.handle_input(1, b"\x02a"); f.handle_input(1, b"z");
    assert_eq!(f.opts_session.get("@r"), Some("a"));
    assert_eq!(f.pane_input.try_iter().flatten().collect::<Vec<u8>>(), b"z");
    // C-b a C-b z: the prefix ends the repeat and starts the prefix table again
    f.handle_input(1, b"\x02a"); f.handle_input(1, b"\x02"); f.handle_input(1, b"z");
    assert_eq!(f.opts_session.get("@r"), Some("aaz"));
    assert!(f.pane_input.try_recv().is_err());
    assert_eq!(f.clients[&1].key_table, "root");
}

#[test]
fn prefix_timeout_counts_from_when_the_table_was_set() {
    let mut f = fixture();
    f.opts_server.set("prefix-timeout", "1000".into());
    run(&mut f, &["bind-key", "z", "set-option -g @z yes"]);
    f.handle_input(1, b"\x02");
    // the prefix was pressed long ago, but switch-client -T sets the table now
    f.clients.get_mut(&1).unwrap().input_state.table_at = Some(Instant::now() - Duration::from_secs(10));
    run(&mut f, &["switch-client", "-T", "prefix"]);
    f.handle_input(1, b"z");
    assert_eq!(f.opts_session.get("@z"), Some("yes"));
    f.opts_session.unset("@z");
    f.handle_input(1, b"\x02");
    f.clients.get_mut(&1).unwrap().input_state.table_at = Some(Instant::now() - Duration::from_secs(10));
    f.handle_input(1, b"z");
    assert!(f.opts_session.get("@z").is_none());
}

#[test]
fn clipboard_request_goes_to_the_most_recent_usable_client() {
    let mut f = fixture(); let pane = f.pane; let sid = f.sid;
    f.opts_server.set("set-clipboard", "on".into());
    f.opts_server.set("get-clipboard", "request".into());
    // a second attached client, more recently active than client 1
    let (tx, out2) = mpsc::channel();
    let mut c2 = super::security_tests::fixture().0.clients.remove(&1).unwrap();
    c2.id = 2; c2.tx = tx; c2.readonly = false; c2.acl_readonly = false; c2.closed.store(false, std::sync::atomic::Ordering::SeqCst);
    c2.features = features::compute("xterm-256color", "", &[], "clipboard");
    f.clients.insert(2, c2);
    f.attach_client(2, sid);
    f.clients.get_mut(&1).unwrap().features.0.insert("clipboard".into());
    f.clients.get_mut(&1).unwrap().activity = 1;
    f.clients.get_mut(&2).unwrap().activity = now_secs();
    f.output.try_iter().for_each(drop); out2.try_iter().for_each(drop);
    let g = f.panes[&pane].generation;
    f.pane_data(pane, g, b"\x1b]52;c;?\x07");
    assert!(out2.try_iter().any(|frame| frame.ends_with(b"\x1b]52;;?\x07")));
    assert!(!f.output.try_iter().any(|frame| frame.ends_with(b"\x1b]52;;?\x07")));
    // a suspended (locked) client is skipped
    f.clients.get_mut(&2).unwrap().suspended = true;
    f.pane_data(pane, g, b"\x1b]52;c;?\x07");
    assert!(!out2.try_iter().any(|frame| frame.ends_with(b"\x1b]52;;?\x07")));
    assert!(f.output.try_iter().any(|frame| frame.ends_with(b"\x1b]52;;?\x07")));
}

#[test]
fn readonly_copy_bindings_expand_command_aliases() {
    // key_bindings_dispatch: cmd_list_all_have(CMD_READONLY) on the parsed list, where
    // aliases are already expanded
    let mut f = fixture();
    run(&mut f, &["set", "-s", "command-alias[90]", "up1=send-keys -X cursor-up"]);
    run(&mut f, &["set", "-s", "command-alias[91]", "sp=select-pane"]);
    assert!(f.binding_allowed_readonly("send-keys -X cursor-up"));
    assert!(f.binding_allowed_readonly("send-keys -X history-top ; copy-mode -q"));
    assert!(f.binding_allowed_readonly("up1"));
    // the search and count prompts are not for read-only clients
    assert!(!f.binding_allowed_readonly("command-prompt -T search -p \"(search down)\" { send-keys -X search-forward -- \"%%\" }"));
    assert!(!f.binding_allowed_readonly("send-keys -X cancel ; select-pane -L"));
    assert!(!f.binding_allowed_readonly("sp -L"));
}

#[test]
fn any_key_bindings_catch_keys_a_table_does_not_bind() {
    // server_client_key_callback: a key not found in a table tries KEYC_ANY in that table
    // before going back to the root table or to the pane
    let mut f = fixture();
    run(&mut f, &["bind-key", "-T", "prefix", "Any", "set-option -ga @h P"]);
    run(&mut f, &["bind-key", "-T", "prefix", "z", "set-option -ga @h Z"]);
    f.handle_input(1, b"\x02");
    f.handle_input(1, b"\x1b[23~"); // F11, unbound in the prefix table
    f.handle_input(1, b"\x02");
    f.handle_input(1, b"z");
    assert_eq!(f.opts_session.get("@h"), Some("PZ"));
    assert_eq!(f.clients[&1].key_table, "root");
    // a root Any binding takes keys before the pane
    run(&mut f, &["bind-key", "-n", "Any", "set-option -ga @h R"]);
    f.pane_input.try_iter().for_each(drop);
    f.cfg_finished = true;
    f.handle_input(1, b"a");
    f.handle_input(1, b"b");
    assert_eq!(f.opts_session.get("@h"), Some("PZRR"));
    assert!(f.pane_input.try_recv().is_err());
    // the message log names the key typed, not Any
    assert!(f.messages.iter().any(|(_, m)| m.ends_with("key b: set-option -ag @h R")));
    // a table switch-client -T chose
    run(&mut f, &["unbind-key", "-n", "Any"]);
    run(&mut f, &["bind-key", "-T", "mytab", "Any", "set-option -ga @h T"]);
    f.clients.get_mut(&1).unwrap().set_key_table("mytab");
    f.handle_input(1, b"k");
    assert_eq!(f.opts_session.get("@h"), Some("PZRRT"));
    assert_eq!(f.clients[&1].key_table, "root");
}

#[test]
fn window_names_follow_mode_changes_at_most_every_name_interval() {
    // check_window_name: a mode set or reset marks the pane changed and the window is
    // renamed when the loop next checks it, unless its name was checked less than
    // NAME_INTERVAL ago (then its timer does it)
    let mut f = fixture();
    let pane = f.pane;
    let wid = f.window_of(pane).unwrap();
    // (the pane's program is read as the name is checked on Unix, else known once a
    // lookup has found it)
    let g = f.panes[&pane].generation;
    f.name_known.insert((pane, g));
    // (the name is checked once the pane's shell has started: before its exec has finished
    // what it runs is not known yet, and the check would take its start command)
    #[cfg(unix)]
    {
        let start = Instant::now();
        let (pid, pgrp) = (f.panes[&pane].pid, f.panes[&pane].fg_pgrp());
        while procinfo::pane_programs(&[(pid, pgrp)]).pop().and_then(|r| r.0).is_none() && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    f.automatic_rename();
    let before = f.windows[&wid].name.clone();
    assert_ne!(before, "[tmux]");
    std::thread::sleep(NAME_INTERVAL + Duration::from_millis(20));
    run(&mut f, &["copy-mode", "-t", &format!("%{pane}")]);
    f.automatic_rename();
    assert_eq!(f.windows[&wid].name, "[tmux]");
    // straight after: the timer, not the loop, renames it back
    run(&mut f, &["send-keys", "-t", &format!("%{pane}"), "-X", "cancel"]);
    f.automatic_rename();
    assert_eq!(f.windows[&wid].name, "[tmux]");
    assert!(f.next_timeout() <= NAME_INTERVAL);
    std::thread::sleep(NAME_INTERVAL + Duration::from_millis(20));
    f.automatic_rename();
    assert_eq!(f.windows[&wid].name, before);
    // nothing changed: nothing to check, no timer
    f.automatic_rename();
    assert!(f.name_state[&wid].timer.is_none());
}

#[test]
fn automatic_names_are_compared_as_expanded_and_set_cleaned() {
    // automatic_rename_update: the expanded format is compared with the window's name and
    // set through window_set_name(w, name, 1) when they differ, so a name that clean_name
    // changes is set (and window-renamed fired) at each check, as in tmux 3.8
    let mut f = fixture();
    let pane = f.pane;
    let wid = f.window_of(pane).unwrap();
    let g = f.panes[&pane].generation;
    f.name_known.insert((pane, g));
    #[cfg(unix)]
    {
        let start = Instant::now();
        let (pid, pgrp) = (f.panes[&pane].pid, f.panes[&pane].fg_pgrp());
        while procinfo::pane_programs(&[(pid, pgrp)]).pop().and_then(|r| r.0).is_none() && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    run(&mut f, &["set-option", "-w", "automatic-rename-format", "a\\b ##(x) c"]);
    let want = if cfg!(windows) { "a\\b _(x) c" } else { "a\\\\b _(x) c" };
    f.ev.renamed.clear();
    f.name_changed.insert(pane);
    f.automatic_rename();
    assert_eq!(f.windows[&wid].name, want);
    assert_eq!(f.ev.renamed.get(&wid), Some(&1));
    std::thread::sleep(NAME_INTERVAL + Duration::from_millis(20));
    f.name_changed.insert(pane);
    f.automatic_rename();
    assert_eq!(f.windows[&wid].name, want);
    assert_eq!(f.ev.renamed.get(&wid), Some(&2));
    // a name that clean_name keeps is set once
    run(&mut f, &["set-option", "-w", "automatic-rename-format", "plain"]);
    f.ev.renamed.clear();
    for _ in 0..2 {
        std::thread::sleep(NAME_INTERVAL + Duration::from_millis(20));
        f.name_changed.insert(pane);
        f.automatic_rename();
    }
    assert_eq!(f.windows[&wid].name, "plain");
    assert_eq!(f.ev.renamed.get(&wid), Some(&1));
}

#[test]
fn keys_of_one_read_wait_behind_the_prompt_one_of_them_opens() {
    // server_client_handle_key0 takes every key of a read before the client's queue runs
    // any: C-b : opens a prompt that waits (CMD_RETURN_WAIT), and the keys read with it
    // are items behind it in the client's queue, not keys for the prompt
    let mut f = fixture();
    f.handle_input(1, b"\x02:abc\r");
    assert_eq!(f.clients[&1].prompt.as_ref().map(|p| p.input.len()), Some(0));
    assert_eq!(f.client_items.get(&1).map(|q| q.len()), Some(4));
    // a later read goes to the prompt (handled at once, the queue may be blocked)
    f.handle_input(1, b"x");
    assert_eq!(f.clients[&1].prompt.as_ref().map(|p| p.input.iter().collect::<String>()), Some("x".into()));
    // the prompt closes: its queue goes on, then the keys waiting behind it
    f.pane_input.try_iter().for_each(drop);
    f.handle_input(1, b"\x03");
    assert!(f.clients[&1].prompt.is_none());
    assert!(f.client_items.is_empty());
    let typed: Vec<u8> = f.pane_input.try_iter().flatten().collect();
    assert_eq!(typed, b"abc\r");
}

#[test]
fn commands_for_a_client_wait_behind_its_waiting_prompt() {
    // a menu item or a mode's choice is appended to the client's queue (cmdq_append):
    // behind a prompt a binding opened, which first runs its answer (cmdq_insert_after,
    // with the binding's key in the message log) and the rest of the binding
    let mut f = fixture();
    f.cfg_finished = true;
    run(&mut f, &["bind-key", "-n", "F5", "command-prompt -p y 'set -ga @h %%' ; set -ga @h T"]);
    f.handle_input(1, b"\x1b[15~");
    assert!(f.clients[&1].prompt.is_some());
    f.run_string(1, "set -ga @h M", None);
    assert_eq!(f.opts_session.get("@h"), None);
    f.handle_input(1, b"q");
    f.handle_input(1, b"\r");
    assert_eq!(f.opts_session.get("@h"), Some("qTM"));
    assert!(f.messages.iter().any(|(_, m)| m.ends_with("key F5: set-option -ag @h q")), "{:?}", f.messages);
    assert!(f.messages.iter().any(|(_, m)| m.ends_with(" command: set-option -ag @h M")), "{:?}", f.messages);
    // a prompt closed without the client's input being read (detach, a command)
    // resumes its queue in the next turn at once
    f.handle_input(1, b"\x1b[15~");
    f.clients.get_mut(&1).unwrap().prompt = None;
    assert!(f.next_timeout() <= Duration::from_millis(5));
    f.drain_clients();
    assert_eq!(f.opts_session.get("@h"), Some("qTMT"));
}

#[test]
fn pasted_text_keeps_its_place_in_the_clients_queue() {
    // server_client_key_callback's paste_key: pasted keys are items of the client's
    // queue, after the keys read before them and behind a command that waits
    let mut f = fixture();
    f.pane_input.try_iter().for_each(drop);
    f.handle_input(1, b"ab\x1b[200~XY\x1b[201~cd");
    let typed: Vec<u8> = f.pane_input.try_iter().flatten().collect();
    assert_eq!(String::from_utf8_lossy(&typed).replace("\x1b[200~", "").replace("\x1b[201~", ""), "abXYcd");
    // a typed key's run-shell -d waits: a later paste (and a parse error a menu or a
    // prompt gives, cmdq_get_error) waits behind it
    run(&mut f, &["bind-key", "-n", "F5", "run-shell -d 5 ; set -ga @h D"]);
    f.handle_input(1, b"\x1b[15~");
    f.handle_input(1, b"\x1b[200~PQ\x1b[201~");
    assert!(f.parse_string("set -g @x { a").is_err());
    f.run_string(1, "set -g @x { a", None);
    assert!(f.pane_input.try_recv().is_err());
    assert!(f.clients[&1].message.is_none());
    for d in &mut f.delayed {
        d.0 = Instant::now();
    }
    f.timers();
    assert_eq!(f.opts_session.get("@h"), Some("D"));
    let typed: Vec<u8> = f.pane_input.try_iter().flatten().collect();
    assert!(String::from_utf8_lossy(&typed).contains("PQ"));
    assert!(f.clients[&1].message.is_some());
    // run-shell -b -d does not wait (JOB_NOWAIT): the keys after it go on at once
    run(&mut f, &["bind-key", "-n", "F6", "run-shell -b -d 5 true ; set -ga @h B"]);
    f.handle_input(1, b"\x1b[17~z");
    assert_eq!(f.opts_session.get("@h"), Some("DB"));
    let typed: Vec<u8> = f.pane_input.try_iter().flatten().collect();
    assert_eq!(typed, b"z");
    assert!(!f.client_blocked(1));
}

#[test]
fn any_with_repeat_compares_the_bindings_key_and_bare_send_keys_sends_the_key() {
    // server_client_repeat_time compares bd->key (KEYC_ANY) with c->last_key: a second
    // key matched by Any is a repeat, with repeat-time, not initial-repeat-time
    let mut f = fixture();
    run(&mut f, &["bind-key", "-r", "-T", "prefix", "Any", "set-option -ga @h A"]);
    run(&mut f, &["set-option", "-g", "initial-repeat-time", "3000"]);
    run(&mut f, &["set-option", "-g", "repeat-time", "500"]);
    f.handle_input(1, b"\x02");
    f.handle_input(1, b"\x1b[23~");
    let first = f.clients[&1].repeat_until.unwrap().saturating_duration_since(Instant::now());
    assert!(first > Duration::from_millis(2000), "{first:?}");
    f.handle_input(1, b"\x1b[24~");
    let second = f.clients[&1].repeat_until.unwrap().saturating_duration_since(Instant::now());
    assert!(second <= Duration::from_millis(500), "{second:?}");
    assert_eq!(f.opts_session.get("@h"), Some("AA"));
    run(&mut f, &["unbind-key", "-T", "prefix", "Any"]);
    f.clients.get_mut(&1).unwrap().repeat_until = None;
    f.clients.get_mut(&1).unwrap().set_key_table("root");
    // cmd-send-keys.c with no keys: the event's key (for bind -n Any send-keys)
    run(&mut f, &["bind-key", "-n", "Any", "send-keys"]);
    run(&mut f, &["bind-key", "-n", "F5", "send-keys -N 2"]);
    f.pane_input.try_iter().for_each(drop);
    f.handle_input(1, b"q");
    f.handle_input(1, b"\x1b[15~");
    let typed: Vec<u8> = f.pane_input.try_iter().flatten().collect();
    assert_eq!(typed, b"q");
    // from the command line there is no key
    run(&mut f, &["send-keys"]);
    assert!(f.pane_input.try_recv().is_err());
}

#[test]
fn a_window_name_timer_that_has_fired_is_dropped() {
    // name_time_callback is a one-shot timer: with automatic-rename off (or nothing to
    // check) a timer that has fired does not keep the loop waking
    let mut f = fixture();
    let pane = f.pane;
    let wid = f.window_of(pane).unwrap();
    f.automatic_rename();
    run(&mut f, &["rename-window", "foo"]);
    f.name_changed.insert(pane);
    f.name_state.entry(wid).or_default().timer = Some(Instant::now() - Duration::from_millis(1));
    f.automatic_rename();
    assert!(f.name_state[&wid].timer.is_none());
    assert!(f.next_timeout() > Duration::from_millis(5));
}

#[test]
fn run_shell_c_waits_for_its_delay_and_with_b_goes_in_the_clients_queue() {
    // cmd_run_shell_exec: -d also delays -C; with -b the commands are appended to the
    // target client's queue (no state, so they log as the client's commands)
    let mut f = fixture();
    f.cfg_finished = true;
    run(&mut f, &["bind-key", "-n", "F7", "run-shell -C -d 5 'set -ga @h C' ; set -ga @h E"]);
    run(&mut f, &["bind-key", "-n", "F8", "run-shell -b -C 'set -ga @h X' ; set -ga @h Y"]);
    f.handle_input(1, b"\x1b[18~");
    assert_eq!(f.opts_session.get("@h"), None);
    for d in &mut f.delayed {
        d.0 = Instant::now();
    }
    f.timers();
    assert_eq!(f.opts_session.get("@h"), Some("CE"));
    f.handle_input(1, b"\x1b[19~");
    assert_eq!(f.opts_session.get("@h"), Some("CEYX"));
    assert!(f.messages.iter().any(|(_, m)| m.ends_with(" command: set-option -ag @h X") && !m.contains(" key ")), "{:?}", f.messages);
}

/// The pane of the client's popup, its input taken over by the test.
fn popup_input(f: &mut Fixture) -> (PaneId, mpsc::Receiver<Vec<u8>>) {
    let Some(Overlay::Popup(p)) = &f.clients[&1].overlay else { panic!("no popup") };
    let pane = p.pane;
    let (tx, rx) = mpsc::channel();
    f.panes.get_mut(&pane).unwrap().input = tx;
    (pane, rx)
}

#[test]
fn a_paste_goes_to_the_popup_a_key_opened() {
    // server_client_handle_key0 hands every pasted key (PasteStart, the text, PasteEnd)
    // to the overlay at once: a popup a key's binding opened gets the paste even though
    // the client's queue waits for the popup, and nothing reaches the pane under it
    let mut f = fixture();
    let pane = f.pane;
    run(&mut f, &["bind-key", "-n", "F5", "display-popup -E cat"]);
    f.handle_input(1, b"\x1b[15~");
    assert!(f.client_blocked(1));
    let (popup, input) = popup_input(&mut f);
    f.pane_input.try_iter().for_each(drop);
    f.handle_input(1, b"typed\r");
    f.handle_input(1, b"\x1b[200~pasted\r\x1b[201~");
    // bytes the key parser does not know go to it too
    f.handle_input(1, b"\x1b[16~");
    let got: Vec<u8> = input.try_iter().flatten().collect();
    assert_eq!(got, b"typed\rpasted\r\x1b[16~");
    // the paste markers only when the popup's program asked for bracketed paste
    let g = f.panes[&popup].generation;
    f.pane_data(popup, g, b"\x1b[?2004h");
    f.handle_input(1, b"\x1b[200~two\x1b[201~");
    let got: Vec<u8> = input.try_iter().flatten().collect();
    assert_eq!(got, b"\x1b[200~two\x1b[201~");
    assert!(f.client_items.get(&1).is_none_or(|q| q.is_empty()));
    // the popup closes: nothing was waiting for the pane
    f.handle_input(1, b"\x1b[200~\x1b[201~");
    run(&mut f, &["display-popup", "-C"]);
    f.drain_clients();
    assert!(f.clients[&1].overlay.is_none());
    assert!(f.pane_input.try_recv().is_err());
    assert!(f.panes.contains_key(&pane));
}

#[test]
fn a_paste_with_a_cancel_key_closes_a_popup_without_e_and_the_rest_is_typed() {
    // popup_key_cb: without -E the pasted C-c closes the popup; PasteStart was taken
    // by the popup, so the keys after it are typed keys for the pane (not a paste)
    let mut f = fixture();
    run(&mut f, &["display-popup", "cat"]);
    let (_, input) = popup_input(&mut f);
    f.pane_input.try_iter().for_each(drop);
    run(&mut f, &["bind-key", "-n", "z", "set-option -g @z yes"]);
    f.handle_input(1, b"\x1b[200~ab\x03yz\x1b[201~");
    assert_eq!(input.try_iter().flatten().collect::<Vec<u8>>(), b"ab");
    assert!(f.clients[&1].overlay.is_none());
    assert_eq!(f.pane_input.try_iter().flatten().collect::<Vec<u8>>(), b"y");
    assert_eq!(f.opts_session.get("@z"), Some("yes"));
}

#[test]
fn a_paste_goes_to_a_pane_prompt_and_a_status_prompt_key_by_key() {
    // window_pane_prompt_key and status_prompt_key get each pasted key: a copy-mode
    // search prompt (vi /) gets the text, a pasted Enter answers a status prompt and the
    // keys after it are typed keys (the prompt took PasteStart)
    let mut f = fixture();
    let pane = f.pane;
    run(&mut f, &["set-option", "-g", "mode-keys", "vi"]);
    run(&mut f, &["copy-mode"]);
    f.handle_input(1, b"/");
    assert!(f.panes[&pane].prompt.is_some());
    f.handle_input(1, b"\x1b[200~gamma\x1b[201~");
    assert_eq!(f.panes[&pane].prompt.as_ref().map(|p| p.input.iter().collect::<String>()).as_deref(), Some("gamma"));
    f.handle_input(1, b"\r");
    assert!(f.panes[&pane].prompt.is_none());
    run(&mut f, &["send-keys", "-X", "cancel"]);
    f.pane_input.try_iter().for_each(drop);
    f.handle_input(1, b"\x02:");
    assert!(f.clients[&1].prompt.is_some());
    f.handle_input(1, b"\x1b[200~set -g @pp yes\rabc\x1b[201~");
    assert!(f.clients[&1].prompt.is_none());
    assert_eq!(f.opts_session.get("@pp"), Some("yes"));
    assert_eq!(f.pane_input.try_iter().flatten().collect::<Vec<u8>>(), b"abc");
    // a numeric prompt ("(repeat) N") closes on PasteStart: the paste goes on to the
    // pane, in copy mode, which drops it
    run(&mut f, &["copy-mode"]);
    f.handle_input(1, b"3");
    assert!(f.panes[&pane].prompt.as_ref().is_some_and(|p| p.numeric));
    f.handle_input(1, b"\x1b[200~12\x1b[201~");
    assert!(f.panes[&pane].prompt.is_none());
    assert!(f.pane_input.try_recv().is_err());
}

#[test]
fn escape_waits_at_least_500ms_while_terminal_replies_are_awaited() {
    // tty_keys_next: until the terminal has answered the device attribute, DECRPM and
    // colour requests (or the start timer fires) a lone Escape waits 500ms, so an Escape
    // and the key typed straight after it are one Meta key, as in tmux (here a request a
    // later reply shows will not be answered is not waited for: the next test)
    let mut f = fixture(); let sid = f.sid;
    f.clients.get_mut(&1).unwrap().ident = Some(Identity { cols: 80, rows: 24, tty: true, cwd: String::new(), term: "xterm-256color".into(), pid: 0, ctx_pane: None, utf8: true, env: Vec::new() });
    f.clients.get_mut(&1).unwrap().attached = false;
    f.clients.get_mut(&1).unwrap().terminal.vt100 = true;
    f.attach_client(1, sid);
    f.handle_input(1, b"\x1b");
    assert!(f.clients[&1].esc_deadline.unwrap().duration_since(Instant::now()) > Duration::from_millis(400));
    f.clients.get_mut(&1).unwrap().keyp.flush_pending();
    f.clients.get_mut(&1).unwrap().esc_deadline = None;
    for reply in [&b"\x1b[?62;22c"[..], b"\x1b[>0;10;1c", b"\x1bP>|xterm(1)\x1b\\", b"\x1b[?2026;2$y", b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"] {
        f.handle_input(1, reply);
    }
    // (the background colour is still awaited)
    f.handle_input(1, b"\x1b");
    assert!(f.clients[&1].esc_deadline.unwrap().duration_since(Instant::now()) > Duration::from_millis(400));
    f.clients.get_mut(&1).unwrap().keyp.flush_pending();
    f.clients.get_mut(&1).unwrap().esc_deadline = None;
    f.handle_input(1, b"\x1b]11;rgb:0000/0000/0000\x1b\\");
    f.handle_input(1, b"\x1b");
    assert!(f.clients[&1].esc_deadline.unwrap().duration_since(Instant::now()) <= Duration::from_millis(100));
    // a terminal that is not VT100-like is asked nothing
    let mut f = fixture();
    f.handle_input(1, b"\x1b");
    assert!(f.clients[&1].esc_deadline.unwrap().duration_since(Instant::now()) <= Duration::from_millis(100));
}

/// A VT100-like client attaching: it is sent the requests (tty_send_requests).
fn attach_vt100() -> Fixture {
    let mut f = fixture(); let sid = f.sid;
    f.clients.get_mut(&1).unwrap().ident = Some(Identity { cols: 80, rows: 24, tty: true, cwd: String::new(), term: "xterm-256color".into(), pid: 0, ctx_pane: None, utf8: true, env: Vec::new() });
    f.clients.get_mut(&1).unwrap().attached = false;
    f.clients.get_mut(&1).unwrap().terminal.vt100 = true;
    f.attach_client(1, sid);
    f.pane_input.try_iter().for_each(drop);
    f
}
/// The Escape delay of a lone Escape typed now, which is then dropped.
fn escape_delay(f: &mut Fixture) -> Duration {
    f.handle_input(1, b"\x1b");
    let d = f.clients[&1].esc_deadline.unwrap().duration_since(Instant::now());
    f.clients.get_mut(&1).unwrap().keyp.flush_pending();
    f.clients.get_mut(&1).unwrap().esc_deadline = None;
    d
}

#[test]
fn escape_does_not_wait_for_a_request_a_later_reply_shows_will_not_be_answered() {
    // Windows Terminal answers DA1, DA2, DECRPM 2026 and the colours but not XTVERSION,
    // which tty_send_requests asks for before DECRPM: a terminal answers in order, so once
    // the replies after it came, no XTVERSION reply is waited for (tmux 3.8 waits until
    // its 5s start timer)
    let mut f = attach_vt100();
    for reply in [&b"\x1b[?61;4;6;7;14;21;22;23;24;28;32;42;52c"[..], b"\x1b[>0;10;1c", b"\x1b[?2026;2$y"] {
        f.handle_input(1, reply);
    }
    // (the colours, asked after DECRPM, are still awaited)
    assert!(escape_delay(&mut f) > Duration::from_millis(400));
    // a colour reply in two reads
    f.handle_input(1, b"\x1b]10;rgb:cc");
    assert!(f.clients[&1].esc_deadline.unwrap().duration_since(Instant::now()) > Duration::from_millis(400));
    f.handle_input(1, b"cc/cccc/cccc\x1b\\\x1b]11;rgb:0c0c/0c0c/0c0c\x1b\\");
    assert!(escape_delay(&mut f) <= Duration::from_millis(100));
    assert!(f.pane_input.try_recv().is_err());
    // an XTVERSION reply that still comes, in two reads, is read as one: the partial reply
    // waits 500ms, and nothing is typed
    f.handle_input(1, b"\x1bP>");
    assert!(f.clients[&1].esc_deadline.unwrap().duration_since(Instant::now()) > Duration::from_millis(400));
    f.handle_input(1, b"|XTerm(400)\x1b\\");
    assert_eq!(f.clients[&1].term_type, "XTerm(400)");
    assert!(f.pane_input.try_recv().is_err());
    assert!(escape_delay(&mut f) <= Duration::from_millis(100));
    // resizing asks for the colours again (tty_repeat_requests): they are waited for again
    f.clients.get_mut(&1).unwrap().terminal.requests(true);
    assert!(escape_delay(&mut f) > Duration::from_millis(400));
    f.handle_input(1, b"\x1b]10;rgb:cccc/cccc/cccc\x1b\\\x1b]11;rgb:0c0c/0c0c/0c0c\x1b\\");
    assert!(escape_delay(&mut f) <= Duration::from_millis(100));
}

#[test]
fn a_reply_after_one_that_settled_its_request_is_still_read() {
    // the DA1 reply coming after the others (out of order): once they settled DA1 an
    // Escape does not wait, but a DA1 reply in two reads still waits 500ms and is read
    let mut f = attach_vt100();
    f.clients.get_mut(&1).unwrap().features.0.remove("margins");
    f.handle_input(1, b"\x1b[>0;10;1c\x1b[?2026;2$y\x1b]10;rgb:cccc/cccc/cccc\x1b\\\x1b]11;rgb:0c0c/0c0c/0c0c\x1b\\");
    assert!(escape_delay(&mut f) <= Duration::from_millis(100));
    f.handle_input(1, b"\x1b[?64;2");
    assert!(f.clients[&1].esc_deadline.unwrap().duration_since(Instant::now()) > Duration::from_millis(400));
    f.handle_input(1, b"1c");
    assert!(f.clients[&1].features.has("margins"));
    assert!(f.pane_input.try_recv().is_err());
    // (once DA1 has come, only the usual escape-time)
    f.handle_input(1, b"\x1b[?6");
    assert!(f.clients[&1].esc_deadline.unwrap().duration_since(Instant::now()) <= Duration::from_millis(100));
}

#[test]
fn a_late_reply_split_after_two_bytes_or_after_an_escape_is_still_read() {
    let deadline = |f: &Fixture| f.clients[&1].esc_deadline.unwrap().duration_since(Instant::now());
    // Windows Terminal replies, then an XTVERSION reply split after ESC P
    let mut f = attach_vt100();
    f.handle_input(1, b"\x1b[?61;4;6;7;14;21;22;23;24;28;32;42;52c\x1b[>0;10;1c\x1b[?2026;2$y\x1b]10;rgb:cccc/cccc/cccc\x1b\\\x1b]11;rgb:0c0c/0c0c/0c0c\x1b\\");
    assert!(escape_delay(&mut f) <= Duration::from_millis(100));
    f.handle_input(1, b"\x1bP");
    assert!(deadline(&f) > Duration::from_millis(400));
    f.handle_input(1, b">|Late(1)\x1b\\");
    assert_eq!(f.clients[&1].term_type, "Late(1)");
    assert!(f.pane_input.try_recv().is_err());
    // (no CSI reply may still come: ESC [ is the usual escape-time)
    f.handle_input(1, b"\x1b[");
    assert!(deadline(&f) <= Duration::from_millis(100));
    // a DA1 reply after the others, split after ESC [, and one an Escape typed just before
    // it holds back as a possible Meta prefix (ESC ESC [): both are read as replies
    for (first, typed) in [(&b"\x1b["[..], false), (b"\x1b", true)] {
        let mut f = attach_vt100();
        f.clients.get_mut(&1).unwrap().features.0.remove("margins");
        f.handle_input(1, b"\x1b[>0;10;1c\x1bP>|XTerm(400)\x1b\\\x1b[?2026;2$y\x1b]10;rgb:cccc/cccc/cccc\x1b\\\x1b]11;rgb:0c0c/0c0c/0c0c\x1b\\");
        f.handle_input(1, first);
        if typed {
            assert!(deadline(&f) <= Duration::from_millis(100));
            f.handle_input(1, b"\x1b[");
        }
        assert!(deadline(&f) > Duration::from_millis(400));
        f.handle_input(1, b"?64;21c");
        assert!(f.clients[&1].features.has("margins"));
        if typed { f.clients.get_mut(&1).unwrap().keyp.flush_pending(); }
    }
}

#[test]
fn colours_asked_again_at_once_are_waited_for_after_the_first_replies() {
    // a theme report (DSR 997) asks for the colours again at once (tty_repeat_requests
    // with force): the first pair's replies settle every request, but the second pair is
    // still waited for, so a reply to it split after ESC is not read as keys
    let deadline = |f: &Fixture| f.clients[&1].esc_deadline.unwrap().duration_since(Instant::now());
    let mut f = attach_vt100();
    f.handle_input(1, b"\x1b[?997;1n\x1b[?61;22c\x1b[>0;10;1c\x1b[?2026;2$y\x1b]10;rgb:cccc/cccc/cccc\x1b\\\x1b]11;rgb:0c0c/0c0c/0c0c\x1b\\");
    f.handle_input(1, b"\x1b");
    assert!(deadline(&f) > Duration::from_millis(400));
    f.handle_input(1, b"]10;rgb:cccc/cccc/cccc\x1b\\\x1b]11;rgb:0c0c/0c0c/0c0c\x1b\\");
    assert!(f.pane_input.try_recv().is_err());
    assert!(escape_delay(&mut f) <= Duration::from_millis(100));
}

#[test]
fn escape_waits_for_requests_without_a_later_reply() {
    // a terminal that answers nothing, or only DA1 and DA2 (no reply after the others):
    // as in tmux 3.8 the Escape delay is 500ms until the start timer
    let mut f = attach_vt100();
    assert!(escape_delay(&mut f) > Duration::from_millis(400));
    f.handle_input(1, b"\x1b[?62;22c\x1b[>0;136;0c");
    assert!(escape_delay(&mut f) > Duration::from_millis(400));
    // a terminal answering everything: no wait once all came
    let mut f = attach_vt100();
    f.handle_input(1, b"\x1b[?62;22c\x1b[>0;10;1c\x1bP>|XTerm(400)\x1b\\\x1b[?2026;2$y\x1b]10;rgb:cccc/cccc/cccc\x1b\\");
    assert!(escape_delay(&mut f) > Duration::from_millis(400));
    f.handle_input(1, b"\x1b]11;rgb:0c0c/0c0c/0c0c\x1b\\");
    assert!(escape_delay(&mut f) <= Duration::from_millis(100));
    assert!(f.pane_input.try_recv().is_err());
}

#[test]
fn an_unknown_csi_sequence_is_read_as_meta_bracket_and_a_key_per_byte() {
    // tty_keys_next finds no key for it: ESC [ is M-[ and each byte after it is a key,
    // so a status prompt gets the bytes as text, typed or pasted (tmux 3.8, pod)
    let mut f = fixture();
    f.handle_input(1, b"\x02:");
    assert!(f.clients[&1].prompt.is_some());
    f.handle_input(1, b"p\x1b[99~q\x1b[200~x\x1b[44~y\x1b[201~");
    assert_eq!(f.clients[&1].prompt.as_ref().map(|p| p.input.iter().collect::<String>()).as_deref(), Some("p99~qx44~y"));
    f.handle_input(1, b"\x03");
    assert!(f.clients[&1].prompt.is_none());
    // a binding of M-[ runs, and a pane still gets the bytes as they were
    run(&mut f, &["bind", "-n", "M-[", "set", "-g", "@mb", "hit"]);
    f.pane_input.try_iter().for_each(drop);
    f.handle_input(1, b"a\x1b[99~b");
    assert_eq!(f.opts_session.get("@mb"), Some("hit"));
    assert_eq!(f.pane_input.try_iter().flatten().collect::<Vec<u8>>(), b"a99~b");
    run(&mut f, &["unbind", "-n", "M-["]);
    f.handle_input(1, b"a\x1b[99~b");
    assert_eq!(f.pane_input.try_iter().flatten().collect::<Vec<u8>>(), b"a\x1b[99~b");
    // replies, mouse reports and extended keys are not
    assert!(input::unknown_csi_keys(b"\x1b[?5;6y").is_none());
    assert!(input::unknown_csi_keys(b"\x1b[<66;1;1M").is_none());
    assert!(input::unknown_csi_keys(b"\x1b[57441;2u").is_none());
    assert!(input::unknown_csi_keys(b"\x1b[27;5;9999~").is_none());
}

#[test]
fn a_block_template_takes_the_answers_without_being_parsed_again() {
    // args_make_commands with a { } block: cmd_list_copy puts the answer into the
    // block's parsed values (args_copy_copy_value), nothing is parsed again; a string
    // template is filled in and parsed (cmd_parse_from_string), as by tmux 3.8
    let mut f = fixture();
    let wid = f.window_of(f.pane).unwrap();
    let rename = |f: &mut Fixture, typed: &str| -> String {
        f.handle_input(1, b"\x02,");
        f.handle_input(1, b"\x15");
        f.handle_input(1, typed.as_bytes());
        f.handle_input(1, b"\r");
        assert!(f.clients[&1].prompt.is_none(), "{typed}");
        f.windows[&wid].name.clone()
    };
    // the default binding: command-prompt -I'#W' { rename-window -- '%%' }
    for (typed, name) in [("a\"b", "a\"b"), ("say \"hi\"", "say \"hi\""), ("a\\b", "a\\\\b"), ("a$HOME", "a$HOME"), ("~", "~"), ("~u", "~u"),
        ("-foo", "-foo"), ("-t x", "-t x"), ("--", "--"), ("x;y", "x;y"), ("{b}", "{b}"), ("%%", "%%"), ("a  b", "a  b"),
        // tmux 3.8 gives "it'\\''s" here: a value that is %% alone keeps its single
        // quotes (an intentional difference)
        ("it's", "it's"), ("a'b\"c\\d", "a'b\"c\\\\d")] {
        // (on Windows clean_name keeps a backslash, COMPATIBILITY.md)
        let name = if cfg!(windows) { name.replace("\\\\", "\\") } else { name.to_string() };
        assert_eq!(rename(&mut f, typed), name, "{typed}");
    }
    // a flag's value takes the answer too, and %1 / %2 the answers of each prompt
    run(&mut f, &["bind-key", "F2", "command-prompt -p a,b { set -g @pair '%1|%2' ; set -g -- @first '%%' }"]);
    f.handle_input(1, b"\x02\x1bOQ");
    f.handle_input(1, b"-x $y\r'q\"\r");
    assert_eq!(f.opts_session.get("@pair"), Some("-x $y|'q\""));
    assert_eq!(f.opts_session.get("@first"), Some("-x $y"));
    // a block inside the block gets them as well
    run(&mut f, &["bind-key", "F3", "command-prompt { if-shell -F 1 { set -g @inner '%%' } }"]);
    f.handle_input(1, b"\x02\x1bOR");
    f.handle_input(1, b"$x \"y\" ~z\r");
    assert_eq!(f.opts_session.get("@inner"), Some("$x \"y\" ~z"));
    // a value with more than %% (a command parsed again later) escapes single quotes
    // as tmux 3.8 does, so an answer cannot end its quotes
    run(&mut f, &["bind-key", "F4", "command-prompt { if-shell -F 1 \"set -g @c '%%'\" }"]);
    for typed in ["it's", "x' ; set -g @pwned 1 ; set -g @e '"] {
        f.handle_input(1, b"\x02\x1bOS");
        f.handle_input(1, typed.as_bytes());
        f.handle_input(1, b"\r");
        assert_eq!(f.opts_session.get("@c"), Some(typed));
    }
    assert_eq!(f.opts_session.get("@pwned"), None);
    let fill = |v: &str, a: &str| input::fill_block_value(v, &[a.to_string()], input::Reader::Direct, input::Quoting::TMUX);
    assert_eq!(fill("test '%%' = x", "x' = x -o 'y"), "test 'x'\\'' = x -o '\\''y' = x");
    assert_eq!(fill("%%", "it's"), "it's");
    assert_eq!(fill("%1:%%", "a'"), "a':a'\\''");
    // a string template is parsed again
    run(&mut f, &["bind-key", ",", "command-prompt -I'#W' \"rename-window -- '%%'\""]);
    for (typed, name) in [("it's", "it's"), ("a\"b", "a\"b"), ("a\\b", "a\\\\b"), ("a$HOME", "a$HOME")] {
        let name = if cfg!(windows) { name.replace("\\\\", "\\") } else { name.to_string() };
        assert_eq!(rename(&mut f, typed), name, "{typed}");
    }
}

#[test]
fn a_status_prompt_freezes_the_panes_until_it_closes() {
    // status_prompt_set: TTY_FREEZE without -C or -i, so what panes write is not drawn
    // (tty_client_ready) until status_prompt_clear redraws everything
    let mut f = fixture();
    let pane = f.pane;
    let row0 = |f: &Fixture| f.compose(1).unwrap().frame.row(0).iter().map(|c| c.text.as_str()).collect::<String>();
    let write = |f: &mut Fixture, text: &str| {
        let g = f.panes[&pane].generation;
        f.pane_data(pane, g, format!("\x1b[H\x1b[2K{text}").as_bytes());
    };
    write(&mut f, "before");
    f.render(1);
    run(&mut f, &["command-prompt", "-I", "x", "set -g @x '%%'"]);
    f.render(1);
    write(&mut f, "during");
    assert!(row0(&f).starts_with("before"));
    f.render(1);
    assert!(row0(&f).starts_with("before"));
    // the window changing is drawn (CLIENT_REDRAWWINDOW)
    run(&mut f, &["split-window", "-d"]);
    assert!(row0(&f).starts_with("during"));
    f.render(1);
    write(&mut f, "later");
    assert!(row0(&f).starts_with("during"));
    f.dispatch_input(1, keys::Input::Key(Key::plain(KeyCode::Escape), vec![27]));
    assert!(f.clients[&1].prompt.is_none());
    assert!(row0(&f).starts_with("later"));
    // -C and -i keep drawing the panes
    for flag in ["-C", "-i"] {
        f.render(1);
        run(&mut f, &["command-prompt", flag, "set -g @x '%%'"]);
        f.render(1);
        write(&mut f, flag);
        assert!(row0(&f).starts_with(flag), "{flag}");
        f.dispatch_input(1, keys::Input::Key(Key::plain(KeyCode::Escape), vec![27]));
    }
    // confirm-before freezes too
    f.render(1);
    run(&mut f, &["confirm-before", "set -g @x y"]);
    f.render(1);
    write(&mut f, "confirm");
    assert!(row0(&f).starts_with("-i"));
}

#[test]
fn only_enter_confirms_a_prompt_not_c_j_or_c_m() {
    // tty_keys_next makes a typed LF C-j (CR stays Enter); prompt_key has a case for
    // '\r' only, so C-j and an extended C-m (CSI 109;5u) are dropped in emacs and vi keys
    let mut f = fixture();
    let input = |f: &Fixture| f.clients[&1].prompt.as_ref().map(|p| p.input.iter().collect::<String>());
    for (keys, vi_command) in [("emacs", false), ("vi", false), ("vi", true)] {
        f.opts_session.set("status-keys", keys.into());
        run(&mut f, &["command-prompt", "set -g @cj '%%'"]);
        f.handle_input(1, b"cj");
        if vi_command {
            f.dispatch_input(1, keys::Input::Key(Key::plain(KeyCode::Escape), vec![27]));
        }
        f.handle_input(1, b"\n");
        f.handle_input(1, b"\x1b[109;5u");
        f.handle_input(1, b"\x1b[106;5u");
        assert_eq!(input(&f).as_deref(), Some("cj"), "{keys} {vi_command}");
        f.handle_input(1, b"\r");
        assert_eq!(input(&f), None, "{keys} {vi_command}");
        assert_eq!(f.opts_session.get("@cj"), Some("cj"));
        f.opts_session.unset("@cj");
    }
}

#[test]
fn without_a_status_line_the_prompt_takes_the_row_status_position_names() {
    // screen_redraw_screen: a prompt or message with no status line is drawn on one line
    // at the top with status-position top (REDRAW_STATUS_TOP), else at the bottom
    let mut f = fixture();
    f.opts_session.set("status", "off".into());
    let h = f.clients[&1].size.1;
    let row = |f: &Fixture, y: u16| f.compose(1).unwrap().frame.row(y).iter().map(|c| c.text.as_str()).collect::<String>();
    for (pos, y) in [("top", 0), ("bottom", h - 1)] {
        f.opts_session.set("status-position", pos.into());
        run(&mut f, &["command-prompt", "-p", "PROMPT", "set -g @x '%%'"]);
        assert!(row(&f, y).starts_with("PROMPT "), "{pos}");
        assert_eq!(f.compose(1).unwrap().cursor.map(|c| c.1), Some(y), "{pos}");
        assert_eq!(f.message_row(1), Some(y), "{pos}");
        f.dispatch_input(1, keys::Input::Key(Key::plain(KeyCode::Escape), vec![27]));
        f.set_message(1, "MESSAGE");
        assert!(row(&f, y).starts_with("MESSAGE"), "{pos}");
        f.clients.get_mut(&1).unwrap().message = None;
    }
}

#[test]
fn fill_terminal_clears_the_message_area_and_fill_default_does_not() {
    // colour_fromstring: "terminal" is 9, not "default" (8), so prompt_draw and
    // format_draw clear the area with it (sy.fill != 8)
    let mut f = fixture();
    f.opts_session.set("status-right", "RIGHTMOD".into());
    let h = f.clients[&1].size.1;
    let last = |f: &Fixture| f.compose(1).unwrap().frame.row(h - 1).iter().map(|c| c.text.as_str()).collect::<String>();
    for (style, kept) in [("fg=red,bg=default,fill=terminal", false), ("fg=red,bg=default,fill=default", true), ("fg=red,fill=blue,fill=terminal", false), ("bg=red", true)] {
        f.opts_session.set("message-style", style.into());
        run(&mut f, &["command-prompt", "-p", "P", "set -g @x '%%'"]);
        assert_eq!(last(&f).contains("RIGHTMOD"), kept, "prompt {style}");
        f.dispatch_input(1, keys::Input::Key(Key::plain(KeyCode::Escape), vec![27]));
        f.set_message(1, "M");
        assert_eq!(last(&f).contains("RIGHTMOD"), kept, "message {style}");
        f.clients.get_mut(&1).unwrap().message = None;
    }
    let frame = |f: &Fixture| f.compose(1).unwrap().frame;
    f.opts_session.set("message-style", "fill=terminal".into());
    run(&mut f, &["command-prompt", "-p", "P", "set -g @x '%%'"]);
    let fr = frame(&f);
    assert_eq!(fr.get(70, h - 1).unwrap().style, crate::cells::Style::default());
    assert_eq!(crate::format::parse_fill("terminal", None), Some(vt100::Color::Default));
    assert_eq!(crate::format::parse_fill("Default", None), None);
}

#[test]
fn the_status_line_under_a_prompt_or_message_stays_as_it_opened() {
    // status_prompt_redraw / status_message_redraw draw over sl->screen, which is not
    // drawn again (status_redraw) while either is shown
    let mut f = fixture();
    f.opts_session.set("message-style", "bg=red".into());
    f.opts_session.set("status-right", "R=#{@clock}".into());
    let h = f.clients[&1].size.1;
    let last = |f: &Fixture| f.compose(1).unwrap().frame.row(h - 1).iter().map(|c| c.text.as_str()).collect::<String>();
    f.opts_session.set("@clock", "1".into());
    f.render(1);
    run(&mut f, &["command-prompt", "-p", "P", "set -g @x '%%'"]);
    f.render(1);
    assert!(last(&f).contains("R=1"));
    f.opts_session.set("@clock", "2".into());
    assert!(last(&f).contains("R=1"));
    f.render(1);
    // a message after it keeps the same status screen
    f.dispatch_input(1, keys::Input::Key(Key::plain(KeyCode::Escape), vec![27]));
    f.set_message(1, "M");
    assert!(last(&f).contains("R=1"));
    f.clients.get_mut(&1).unwrap().message = None;
    assert!(last(&f).contains("R=2"));
    f.render(1);
    f.set_message(1, "M");
    f.render(1);
    f.opts_session.set("@clock", "3".into());
    assert!(last(&f).contains("R=2"));
}

#[test]
fn c_y_after_a_c_w_that_deleted_nothing_pastes_nothing() {
    // prompt.c: C-w always sets pr->copied, even to nothing, and prompt_paste uses
    // pr->copied once it is set rather than the top buffer
    let mut f = fixture();
    let input = |f: &Fixture| f.clients[&1].prompt.as_ref().map(|p| p.input.iter().collect::<String>()).unwrap_or_default();
    f.add_buffer(None, b"ZZBUF".to_vec(), false);
    run(&mut f, &["command-prompt", "set -g @x '%%'"]);
    f.handle_input(1, b"\x19");
    assert_eq!(input(&f), "ZZBUF");
    f.handle_input(1, b"\x15\x17\x19");
    assert_eq!(input(&f), "");
    f.handle_input(1, b"one two\x17\x19\x19");
    assert_eq!(input(&f), "one twotwo");
}

#[test]
fn a_string_templates_prompt_is_its_text_up_to_a_space_or_comma() {
    // args_make_commands_get_command: strcspn(" ,") for a string, the first command's
    // name for a block
    let mut f = fixture();
    let label = |f: &mut Fixture, tpl: &str| -> String {
        run(f, &["bind-key", "F4", &format!("command-prompt {tpl}")]);
        f.handle_input(1, b"\x02\x1b[14~");
        let l = f.clients[&1].prompt.as_ref().map(|p| p.label.clone()).unwrap_or_default();
        f.dispatch_input(1, keys::Input::Key(Key::plain(KeyCode::Escape), vec![27]));
        l
    };
    assert_eq!(label(&mut f, "\"renamew,x '%%'\""), "(renamew) ");
    assert_eq!(label(&mut f, "\"renamew\tx '%%'\""), "(renamew\tx) ");
    assert_eq!(label(&mut f, "\"new-window -n '%%'\""), "(new-window) ");
    assert_eq!(label(&mut f, "{ renamew -- '%%' }"), "(rename-window) ");
}

#[test]
fn an_event_hooks_commands_log_without_a_client() {
    // hooks_insert_one puts an event hook's commands on the global queue (no client), so
    // cmdq_add_message logs them as "command: ..."; after-* hooks follow their command in
    // the client's queue with its event (hooks_insert), so they log as the bound command
    // does, with its key
    let mut f = fixture();
    f.cfg_finished = true;
    run(&mut f, &["set-hook", "-g", "window-renamed", "set -g @rn 1"]);
    run(&mut f, &["set-hook", "-g", "after-rename-window", "set -g @after 1"]);
    run(&mut f, &["bind-key", "F6", "rename-window hooked"]);
    f.handle_input(1, b"\x02\x1b[17~");
    assert_eq!(f.opts_session.get("@rn"), Some("1"));
    let name = f.clients[&1].name();
    let logged = |m: &str| f.messages.iter().any(|(_, l)| l == m);
    assert!(logged("command: set-option -g @rn 1"), "{:?}", f.messages);
    assert!(logged(&format!("{name} key F6: set-option -g @after 1")), "{:?}", f.messages);
    assert!(logged(&format!("{name} key F6: rename-window hooked")), "{:?}", f.messages);
}

#[test]
fn confirm_before_expands_and_parses_a_string_command_at_once() {
    // cmd_confirm_before_exec: args_make_commands_now(self, item, 0, 1), so a string is a
    // format expanded and parsed when the command runs; a { } block is used as it is
    let mut f = fixture();
    run(&mut f, &["confirm-before", "-y", "set -g @c '#{session_name}'"]);
    f.handle_input(1, b"\r");
    assert_eq!(f.opts_session.get("@c"), Some("input"));
    run(&mut f, &["bind-key", "F8", "confirm-before -y { set -g @cb '#{session_name}' }"]);
    f.handle_input(1, b"\x02\x1b[19~");
    assert!(f.clients[&1].prompt.is_some());
    f.handle_input(1, b"\r");
    assert_eq!(f.opts_session.get("@cb"), Some("#{session_name}"));
    let mut ctx = Ctx { client: Some(1), ..Ctx::default() };
    assert!(f.exec(vec!["confirm-before".into(), "set -g @x { a".into()], &mut ctx).is_err());
    assert!(f.clients[&1].prompt.is_none());
}

#[test]
fn confirm_before_names_the_command_an_alias_stands_for() {
    // cmd_confirm_before_exec: cmd_get_entry of the first command as it was built
    let mut f = fixture();
    run(&mut f, &["set", "-s", "command-alias[100]", "myren=rename-window"]);
    run(&mut f, &["confirm-before", "myren 'x'"]);
    assert_eq!(f.clients[&1].prompt.as_ref().map(|p| p.label.as_str()), Some("Confirm 'rename-window'? (y/n) "));
    f.handle_input(1, b"y");
    assert_eq!(f.windows[&f.window_of(f.pane).unwrap()].name, "x");
}

#[test]
fn a_frozen_client_redraws_for_a_new_prompt_a_message_and_what_is_over_the_panes() {
    let mut f = fixture();
    let pane = f.pane;
    let row0 = |f: &Fixture| f.compose(1).unwrap().frame.row(0).iter().map(|c| c.text.as_str()).collect::<String>();
    let write = |f: &mut Fixture, text: &str| {
        let g = f.panes[&pane].generation;
        f.pane_data(pane, g, format!("\x1b[H\x1b[2K{text}").as_bytes());
    };
    // a prompt opened by a prompt's answer: status_prompt_clear redraws everything
    // (CLIENT_ALLREDRAWFLAGS), then the new one freezes again
    run(&mut f, &["bind-key", "F5", "command-prompt -p one { command-prompt -p two { set -g @two '%%' } }"]);
    write(&mut f, "before");
    f.render(1);
    f.handle_input(1, b"\x02\x1b[15~");
    f.render(1);
    write(&mut f, "during");
    f.render(1);
    assert!(row0(&f).starts_with("before"));
    f.handle_input(1, b"a\r");
    assert!(f.clients[&1].prompt.is_some());
    assert!(row0(&f).starts_with("during"));
    f.render(1);
    write(&mut f, "later");
    f.render(1);
    assert!(row0(&f).starts_with("during"));
    f.handle_input(1, b"b\r");
    // (the first answer went into the nested block too: args_copy_copy_value)
    assert_eq!(f.opts_session.get("@two"), Some("a"));
    assert!(f.clients[&1].prompt.is_none());
    assert!(row0(&f).starts_with("later"));
    f.render(1);

    // a message freezes too (status_message_set), unless display-message -C
    run(&mut f, &["display-message", "-d", "0", "msg"]);
    f.render(1);
    write(&mut f, "m1");
    f.render(1);
    assert!(row0(&f).starts_with("later"));
    f.handle_input(1, b"x");
    assert!(f.clients[&1].message.is_none());
    assert!(row0(&f).starts_with("m1"));
    f.render(1);
    run(&mut f, &["display-message", "-C", "-d", "0", "msg"]);
    f.render(1);
    write(&mut f, "m2");
    assert!(row0(&f).starts_with("m2"));
    f.handle_input(1, b"x");
    f.render(1);

    // a message is drawn over a prompt, and clearing it redraws the panes
    run(&mut f, &["command-prompt", "-p", "(pp)", "set -g @x '%%'"]);
    f.render(1);
    let last = |f: &Fixture| f.compose(1).unwrap().frame.row(23).iter().map(|c| c.text.as_str()).collect::<String>();
    assert!(last(&f).starts_with("(pp)"), "{}", last(&f));
    write(&mut f, "m3");
    run(&mut f, &["display-message", "-d", "0", "over"]);
    f.render(1);
    assert!(last(&f).contains("over"), "{}", last(&f));
    assert!(row0(&f).starts_with("m2"));
    // the key clears the message and goes on to the prompt
    f.handle_input(1, b"z");
    assert!(last(&f).starts_with("(pp) z"), "{}", last(&f));
    assert!(row0(&f).starts_with("m3"));
    f.render(1);

    // a menu over the frozen panes is drawn as it changes (CLIENT_REDRAWMENU)
    run(&mut f, &["display-menu", "-x", "0", "-y", "10", "-T", "M", "one", "1", "set -g @m 1", "two", "2", "set -g @m 2"]);
    f.render(1);
    let before = f.compose(1).unwrap().frame;
    f.handle_input(1, b"\x1b[B");
    let after = f.compose(1).unwrap().frame;
    assert!(before.cells != after.cells, "the menu's selection is drawn");
    f.handle_input(1, b"\r");
    assert!(f.menus.is_empty());
    assert!(f.clients[&1].prompt.is_some());
    // (the menu's command waits in the client's queue behind the command-prompt)
    f.handle_input(1, b"\r");
    assert_eq!(f.opts_session.get("@x"), Some("z"));
    assert_eq!(f.opts_session.get("@m"), Some("2"));
}

#[test]
fn the_status_line_under_a_prompt_survives_a_full_redraw() {
    // status_prompt_redraw draws over sl->screen; a full redraw does not draw the status
    let mut f = fixture();
    run(&mut f, &["set", "-g", "status", "2"]);
    run(&mut f, &["set", "-g", "status-format[1]", "BEFORE"]);
    f.render(1);
    run(&mut f, &["command-prompt", "-p", "(x)", "set -g @x '%%'"]);
    f.render(1);
    let last = |f: &Fixture| f.compose(1).unwrap().frame.row(23).iter().map(|c| c.text.as_str()).collect::<String>();
    assert!(last(&f).starts_with("BEFORE"));
    run(&mut f, &["set", "-g", "status-format[1]", "AFTER"]);
    f.clients.get_mut(&1).unwrap().prev = None;
    f.render(1);
    assert!(last(&f).starts_with("BEFORE"), "{}", last(&f));
    f.handle_input(1, b"\x03");
    assert!(f.clients[&1].prompt.is_none());
    assert!(last(&f).starts_with("AFTER"), "{}", last(&f));
}

#[test]
fn a_menu_taller_than_the_client_opens_at_the_top() {
    // cmd_display_menu_get_menu_pos never refuses a size; menu_display puts a menu as
    // tall as the window or taller at py 0
    let mut f = fixture();
    let mut args: Vec<String> = ["display-menu", "-T", "M"].iter().map(|s| s.to_string()).collect();
    for i in 0..30 {
        args.extend([format!("item{i}"), String::new(), format!("set -g @tall {i}")]);
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run(&mut f, &args);
    let wid = f.window_of(f.pane).unwrap();
    let m = f.menus.get(&wid).expect("the menu is open");
    assert_eq!(m.y, 0);
    f.render(1);
    let row = |f: &Fixture, y: u16| f.compose(1).unwrap().frame.row(y).iter().map(|c| c.text.as_str()).collect::<String>();
    assert!(row(&f, 1).contains("item0"), "{}", row(&f, 1));
    f.handle_input(1, b"\x1b[B\r");
    assert_eq!(f.opts_session.get("@tall"), Some("1"));
}

#[test]
fn prompt_answers_are_escaped_for_the_shells_that_may_run_them() {
    // Unix keeps tmux 3.8's escapes whatever default-shell is (jobs run under /bin/sh);
    // Windows takes the jobs' shell and every default-shell into account
    let mut f = fixture();
    let job = procinfo::default_shell();
    let expect = |shells: &[&str]| if cfg!(windows) { input::Quoting::for_shells(shells.iter().copied()) } else { input::Quoting::TMUX };
    f.opts_session.set("default-shell", "/bin/sh".into());
    assert_eq!(f.answer_quoting(), expect(&[&job, "/bin/sh"]));
    f.opts_session.set("default-shell", "pwsh.exe".into());
    assert_eq!(f.answer_quoting(), expect(&[&job, "pwsh.exe"]));
    // a session's own default-shell counts too
    let sid = f.sid;
    f.sessions.get_mut(&sid).unwrap().options.set("default-shell", "C:\\Windows\\System32\\cmd.exe".into());
    let q = f.answer_quoting();
    assert_eq!(q, expect(&[&job, "pwsh.exe", "cmd.exe"]));
    // the block value of a binding's prompt gets those escapes (`set` keeps it as given)
    run(&mut f, &["bind-key", "F6", "command-prompt { set -g @v \"echo '%%' %%\" }"]);
    f.handle_input(1, b"\x02\x1b[17~");
    f.handle_input(1, b"it's & x\r");
    let want = input::fill_block_value("echo '%%' %%", &["it's & x".into()], input::Reader::Direct, q);
    assert_eq!(f.opts_session.get("@v"), Some(want.as_str()));
    if cfg!(windows) {
        assert_eq!(want, "echo 'it''s ^& x' %%");
    } else {
        assert_eq!(want, "echo 'it'\\''s & x' %%");
    }
}

/// On Windows the default shell (PowerShell) runs a prompt's answer in a block as the
/// answer, whatever quotes it has (Unix: the same through /bin/sh with tmux's escapes).
#[cfg(windows)]
#[test]
fn windows_job_shell_gets_powershell_escapes() {
    let mut f = fixture();
    f.opts_session.set("default-shell", procinfo::default_shell().into());
    let q = f.answer_quoting();
    if procinfo::shell_kind(&procinfo::default_shell()) == procinfo::ShellKind::PowerShell {
        assert!(q.powershell && !q.cmd, "{q:?}");
        assert_eq!(input::fill_block_value("echo '%%'", &["x'y\u{2019}z".into()], input::Reader::Direct, q), "echo 'x''y\u{2019}\u{2019}z'");
    }
}
