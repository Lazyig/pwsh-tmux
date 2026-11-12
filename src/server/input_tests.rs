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
fn encode(f: &Fixture, key: &str) -> Vec<u8> { let k = keys::parse_key_name(key).unwrap(); f.encode_pane_key(f.pane, &k, keys::encode_key(&k, false)) }

#[test]
fn input_option_backspace_user_keys_xterm_and_extended_keys() {
    let mut f = fixture();
    assert_eq!(encode(&f, "BSpace"), b"\x7f");
    f.opts_server.set("backspace", "C-h".into());
    assert_eq!(encode(&f, "BSpace"), b"\x08");
    assert_eq!(encode(&f, "M-BSpace"), b"\x1b\x08");
    f.opts_server.set("user-keys[4]", "\x1b[99~".into());
    assert_eq!(encode(&f, "User4"), b"\x1b[99~");
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
    server.opts_session.set("@width-text", "AB".into());
    assert_eq!(server.expand(Default::default(), Some(1), "#{w:@width-text}"), "4");
    let widths = features::codepoint_widths(&["U+41-U+5A=2".into(), "B=1".into(), "U+5A-U+41=1".into(), "U+41=9".into()]);
    assert_eq!(widths, [(65, 90, 2), (66, 66, 1)]);
    let mut p = vt100::Parser::new(nz(3), nz(20), 10);
    p.screen_mut().set_codepoint_widths(widths, true);
    p.process(b"AB"); assert_eq!(p.screen().cursor_position(), (0, 3));
    assert!(p.screen().cell(0, 0).unwrap().is_wide());
    p.process("x\u{fe0f}".as_bytes()); assert_eq!(p.screen().cursor_position(), (0, 5));
    let mut p = vt100::Parser::new(nz(3), nz(20), 10);
    p.screen_mut().set_codepoint_widths(vec![], false);
    p.process("x\u{fe0f}".as_bytes()); assert_eq!(p.screen().cursor_position(), (0, 1));
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
    p.process(b"\x1b]2;12345678901234567890\x07ok");
    assert!(p.callbacks().title.is_none()); assert!(p.screen().contents().contains("ok"));
    p.process(b"\x1b]2;yes\x07"); assert_eq!(p.callbacks().title.as_deref(), Some("yes"));
}

#[test]
fn input_option_cursor_styles_prompt_keypad_and_focus_following() {
    let mut f = fixture(); let pane = f.pane;
    f.panes.get_mut(&pane).unwrap().options.set("cursor-style", "bar".into());
    f.panes.get_mut(&pane).unwrap().options.set("cursor-colour", "#{?pane_active,#123456,red}".into());
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
    assert!(f.output.try_recv().unwrap().ends_with(b"\x1b]52;c;?\x07"));
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
    f.prompt_history = vec!["first".into(), "second".into()]; f.save_prompt_history(); f.prompt_history.clear(); f.load_prompt_history();
    assert_eq!(f.prompt_history, ["first", "second"]); std::fs::remove_file(path).unwrap();
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
