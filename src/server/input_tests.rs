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
