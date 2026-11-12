use super::vars::Target;
use super::modes::Popup;
use super::*;

fn fixture() -> (Server, mpsc::Receiver<Event>) {
    let (tx, events) = mpsc::sync_channel(64);
    let mut srv = Server::new(tx);
    let cid = 1;
    let now = now_secs();
    let (tx, _output) = mpsc::channel();
    let pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let backlog_since = Arc::new(std::sync::atomic::AtomicU64::new(0));
    srv.clients.insert(
        cid,
        Client {
            id: cid,
            tx,
            ident: None,
            session: None,
            attached: false,
            readonly: false,
            size: (80, 24),
            key_table: "root".into(),
            repeat_until: None,
            keyp: keys::Parser::new(),
            esc_deadline: None,
            prompt: None,
            overlay: None,
            overlay_epoch: 0,
            message: None,
            prev: None,
            dirty: false,
            last_session: None,
            created: now,
            activity: now,
            drag: None,
            title: String::new(),
            suspended: false,
            click: Default::default(),
            mouse_last: (0, 0, 0),
            mouse_drag_flag: 0,
            status_ranges: Vec::new(),
            control: 0,
            control_size: None,
            control_seq: 0,
            seen: Default::default(),
            features: Default::default(),
            pending,
            queued_bytes: Default::default(),
            closed: Default::default(),
            backlog_since,
            ctl: Default::default(),
            user: None,
            denied: false,
            acl_readonly: false,
            io: Default::default(),
        },
    );
    srv.acl.push(("publication-review-readonly".into(), false));
    srv.client_user(cid, Some("publication-review-readonly".into()));
    assert!(srv.clients[&cid].acl_readonly && srv.clients[&cid].readonly);
    (srv, events)
}

fn command(srv: &mut Server, args: &[&str]) -> Result<Flow, String> {
    let mut ctx = Ctx {
        client: Some(1),
        reply: Some(1),
        ..Ctx::default()
    };
    srv.exec(args.iter().map(|s| s.to_string()).collect(), &mut ctx)
}

#[test]
fn readonly_format_never_starts_shell_or_exposes_cached_output() {
    let (mut srv, events) = fixture();
    assert!(
        command(
            &mut srv,
            &["display-message", "-p", "#(echo SECURITY_PROOF)"]
        )
        .is_ok()
    );
    assert!(srv.shell_cache.borrow().is_empty());
    assert!(events.try_recv().is_err());
    // A format target is not the security principal. Even a target with no
    // client must inherit the originating command's restriction.
    srv.readonly_context.set(true);
    assert_eq!(
        srv.expand(Target::default(), None, "#(echo SECURITY_PROOF)"),
        ""
    );
    srv.readonly_context.set(false);
    // Writable clients retain shell formats. Wait for completion to distinguish
    // a blocked job from one that simply has not finished yet.
    assert_eq!(
        srv.expand(Target::default(), None, "#(echo SECURITY_PROOF)"),
        ""
    );
    let event = events
        .recv_timeout(Duration::from_secs(15))
        .expect("writable shell format");
    match &event {
        Event::FormatShell(_, output) => assert_eq!(output.trim(), "SECURITY_PROOF"),
        _ => panic!("unexpected event"),
    }
    srv.handle(event);
    assert_eq!(
        srv.expand(Target::default(), Some(1), "#(echo SECURITY_PROOF)"),
        ""
    );
}

#[test]
fn readonly_cannot_clear_flags_even_if_input_flag_was_lost() {
    let (mut srv, _) = fixture();
    srv.clients.get_mut(&1).unwrap().attached = true;
    assert!(command(&mut srv, &["refresh-client", "-f", "!read-only"]).is_err());
    assert!(command(&mut srv, &["switch-client", "-r"]).is_err());
    let c = srv.clients.get_mut(&1).unwrap();
    c.readonly = false;
    assert!(c.is_readonly());
    assert!(command(&mut srv, &["run-shell", "echo forbidden"]).is_err());
    assert_eq!(
        srv.expand(Target::default(), Some(1), "#{client_readonly}"),
        "1"
    );
}

#[test]
fn format_target_and_client_target_cannot_change_request_authority() {
    let (mut srv, events) = fixture();
    let (mut other, _) = fixture();
    let mut writable = other.clients.remove(&1).unwrap();
    writable.id = 2;
    writable.readonly = false;
    writable.acl_readonly = false;
    writable.attached = true;
    srv.clients.insert(2, writable);
    assert!(
        command(
            &mut srv,
            &[
                "display-message",
                "-p",
                "-c",
                "client-2",
                "#(echo forbidden)"
            ]
        )
        .is_ok()
    );
    assert!(srv.shell_cache.borrow().is_empty());
    assert!(events.try_recv().is_err());
    for cmd in ["refresh-client", "detach-client"] {
        assert!(command(&mut srv, &[cmd, "-t", "client-2"]).is_err());
    }
    assert!(command(&mut srv, &["switch-client", "-c", "client-2"]).is_err());
    assert!(srv.clients[&2].attached);
}

#[test]
fn readonly_rejects_mutating_variants_and_environment_assignment() {
    let (mut srv, _) = fixture();
    for args in [
        vec!["SECURITY_TEST=value"],
        vec!["capture-pane"],
        vec!["display-message", "-I"],
        vec!["detach-client", "-a"],
        vec!["attach-session", "-d"],
        vec!["attach-session", "-t", "%1"],
        vec!["switch-client", "-Z"],
        vec!["server-access", "-w", "anyone"],
        vec!["choose-tree"],
    ] {
        assert_eq!(
            command(&mut srv, &args).err().as_deref(),
            Some("client is read-only"),
            "{args:?}"
        );
    }
    assert!(!srv.env.contains_key("SECURITY_TEST"));
    let mut ctx = Ctx {
        client: Some(1),
        hook_depth: 1,
        ..Ctx::default()
    };
    assert!(
        srv.exec(vec!["run-shell".into(), "echo forbidden".into()], &mut ctx)
            .is_err()
    );
    assert!(srv.jobs.is_empty());
}

#[test]
fn unknown_identity_and_disconnected_origin_fail_closed() {
    let (mut srv, _) = fixture();
    srv.owner.clear();
    srv.client_user(1, None);
    assert!(srv.clients[&1].denied);
    srv.clients.remove(&1);
    assert!(command(&mut srv, &["run-shell", "echo forbidden"]).is_err());
}

#[test]
fn overlay_queue_waits_for_close_and_rechecks_access() {
    let (mut srv, _) = fixture();
    let client = srv.clients.get_mut(&1).unwrap();
    client.readonly = false;
    client.acl_readonly = false;
    client.overlay_epoch = 7;
    client.overlay = Some(Overlay::DisplayPanes {
        until: Instant::now() + Duration::from_secs(30),
        template: String::new(),
    });
    let queue = Queue {
        cmds: VecDeque::from([vec!["set-option".into(), "-g".into(), "@overlay-result".into(), "written".into()]]),
        ctx: Ctx { client: Some(1), ..Ctx::default() }, soft: 0,
    };
    srv.overlay_waits.push((1, 7, "display-menu".into(), queue));
    srv.timers();
    assert_eq!(srv.overlay_waits.len(), 1);
    assert!(!srv.opts_session.0.contains_key("@overlay-result"));
    // A parked writable request must lose write access after a downgrade.
    srv.clients.get_mut(&1).unwrap().acl_readonly = true;
    srv.clients.get_mut(&1).unwrap().overlay = None;
    srv.timers();
    assert!(srv.overlay_waits.is_empty());
    assert!(!srv.opts_session.0.contains_key("@overlay-result"));
}

#[test]
fn replacing_overlay_resumes_the_previous_queue() {
    let (mut srv, _) = fixture();
    let client = srv.clients.get_mut(&1).unwrap();
    client.readonly = false;
    client.acl_readonly = false;
    client.overlay_epoch = 8;
    client.overlay = Some(Overlay::DisplayPanes {
        until: Instant::now() + Duration::from_secs(30), template: String::new(),
    });
    srv.overlay_waits.push((1, 7, "display-menu".into(), Queue {
        cmds: VecDeque::from([vec!["set-option".into(), "-g".into(), "@overlay-result".into(), "written".into()]]),
        ctx: Ctx { client: Some(1), ..Ctx::default() }, soft: 0,
    }));
    srv.hooks.insert("after-display-menu".into(), vec!["set-option -g @overlay-hook called".into()]);
    srv.timers();
    assert!(srv.overlay_waits.is_empty());
    assert_eq!(srv.opts_session.0.get("@overlay-hook").map(String::as_str), Some("called"));
    assert_eq!(srv.opts_session.0.get("@overlay-result").map(String::as_str), Some("written"));
    assert!(srv.clients[&1].overlay.is_some(), "replacement stays open");
}

#[test]
fn flag_option_formats_are_numeric_and_false_when_off() {
    let (mut srv, _) = fixture();
    assert_eq!(srv.expand(Target::default(), None, "#{synchronize-panes}|#{?synchronize-panes,red,green}"), "0|green");
    srv.opts_window.0.insert("synchronize-panes".into(), "on".into());
    assert_eq!(srv.expand(Target::default(), None, "#{synchronize-panes}|#{?synchronize-panes,red,green}"), "1|red");
}

#[test]
fn readonly_hook_context_cannot_become_owner_context() {
    let (mut srv, events) = fixture();
    srv.hooks.insert(
        "security-hook".into(),
        vec!["run-shell echo forbidden".into()],
    );
    srv.run_hook(
        None,
        "security-hook",
        Ctx {
            client: Some(1),
            ..Ctx::default()
        },
    );
    srv.run_hook(
        None,
        "security-hook",
        Ctx {
            readonly: true,
            ..Ctx::default()
        },
    );
    assert!(srv.jobs.is_empty());
    assert!(events.try_recv().is_err());
}

#[test]
fn slow_client_output_backlog_is_bounded_and_cancelled() {
    use std::sync::atomic::Ordering;
    let (srv, _) = fixture();
    let mut client = srv.clients.into_values().next().unwrap();
    let (tx, output) = mpsc::channel();
    client.tx = tx;
    let payload = vec![b'x'; transport::MAX_OUTPUT_BACKLOG / 2];
    client.send(S_OUTPUT, &payload);
    client.send(S_OUTPUT, &payload);
    assert!(client.closed.load(Ordering::SeqCst));
    assert!(client.queued_bytes.load(Ordering::SeqCst) <= transport::MAX_OUTPUT_BACKLOG);
    assert_eq!(output.try_iter().count(), 1);
}

#[test]
fn readonly_switch_with_no_sessions_returns_error_without_panicking() {
    let (mut srv, _) = fixture();
    srv.clients.get_mut(&1).unwrap().attached = true;
    for flag in ["-n", "-p"] {
        assert_eq!(command(&mut srv, &["switch-client", flag]).err().as_deref(), Some("no sessions"));
    }
}

#[test]
fn repeated_readonly_formats_cannot_accumulate_unbounded_reply() {
    let (mut srv, _) = fixture();
    let mut ctx = Ctx { client: Some(1), reply: Some(1), ..Ctx::default() };
    let mut rejected = false;
    // About 1MB per reply; tmux limits each padding to 10000 columns.
    let fmt = "#{p10000:#{l:x}}".repeat(100);
    for _ in 0..16 {
        let args = ["display-message", "-p", fmt.as_str()].map(String::from).to_vec();
        if srv.exec(args, &mut ctx).is_err() { rejected = true; break; }
    }
    assert!(rejected, "combined query replies exceeded the request budget");
    assert!(ctx.out.len() <= 4 << 20);
}

#[test]
fn subscription_configuration_has_an_aggregate_memory_limit() {
    let (mut srv, _) = fixture();
    let fmt = "x".repeat(65536);
    let mut rejected = false;
    for n in 0..32 {
        if srv.control_subscribe(1, &format!("s{n}::{fmt}")).is_err() { rejected = true; break; }
    }
    assert!(rejected);
    let stored: usize = srv.clients[&1].ctl.subs.iter().map(|(name, (what, fmt, _))| name.len() + what.len() + fmt.len()).sum();
    assert!(stored <= 1 << 20);
}

#[test]
fn legacy_stdin_requests_are_rejected_before_command_splitting() {
    let (mut srv, _) = fixture();
    let (tx, output) = mpsc::channel();
    let c = srv.clients.get_mut(&1).unwrap();
    c.tx = tx;
    c.readonly = false;
    c.acl_readonly = false;
    let args = vec!["load-buffer".into(), "\x01stdin:buffer\0;\0set-option\0-g\0@injected\0yes".into()];
    srv.client_msg(1, C_COMMAND, encode_args(&args));
    assert!(srv.buffers.is_empty());
    assert!(!srv.opts_session.0.contains_key("@injected"));
    let reply = output.try_recv().unwrap();
    let (kind, payload) = read_frame(&mut &reply[..]).unwrap();
    assert_eq!(kind, S_REPLY);
    let (code, message) = decode_reply(&payload);
    assert_eq!(code, 1);
    assert!(message.contains("legacy stdin"));
}

#[test]
fn replacing_popup_with_other_overlays_releases_its_pane() {
    let popup = |srv: &mut Server| {
        let c = srv.clients.get_mut(&1).unwrap();
        c.readonly = false;
        c.acl_readonly = false;
        c.attached = true;
        let pane = srv.spawn_pane(None, 20, 6, &[], None, &[]).unwrap();
        srv.replace_overlay(1, Some(Overlay::Popup(Popup { pane, x: 0, y: 0, w: 22, h: 8, border: true, close_on_exit: false, title: String::new(), any_key: false, look: Default::default() }))).unwrap();
        pane
    };
    // like tmux, other overlays are not shown over a popup: it and its pane stay
    for args in [&["display-panes", "-b"][..], &["display-menu", "Item", "i", "display-message done"][..]] {
        let (mut srv, _events) = fixture();
        let pane = popup(&mut srv);
        let epoch = srv.clients[&1].overlay_epoch;
        let result = command(&mut srv, args);
        let kept = srv.panes.contains_key(&pane) && srv.clients[&1].overlay_epoch == epoch && matches!(srv.clients[&1].overlay, Some(Overlay::Popup(_)));
        srv.shutdown();
        assert!(result.is_ok());
        assert!(kept, "{args:?} replaced the popup");
    }
    // replacing the overlay directly releases the popup's pane
    let (mut srv, _events) = fixture();
    let pane = popup(&mut srv);
    srv.replace_overlay(1, Some(Overlay::DisplayPanes { until: Instant::now(), template: String::new() })).unwrap();
    let released = !srv.panes.contains_key(&pane);
    srv.shutdown();
    assert!(released, "replaced popup left an orphan pane");
}

#[test]
fn close_popup_does_not_remove_an_unrelated_menu() {
    let (mut srv, _) = fixture();
    let c = srv.clients.get_mut(&1).unwrap();
    c.readonly = false;
    c.acl_readonly = false;
    c.attached = true;
    command(&mut srv, &["display-menu", "Item", "i", "display-message done"]).unwrap();
    command(&mut srv, &["display-popup", "-C"]).unwrap();
    assert!(matches!(srv.clients[&1].overlay, Some(Overlay::Menu(_))));
}

#[test]
fn osc7_with_a_multibyte_character_after_percent_does_not_panic() {
    assert_eq!(super::decode_file_url("file://localhost/C:/%한"), Some("C:\\%한".into()));
    assert_eq!(super::decode_file_url("file://h/tmp/%e2%82%acx%zz%4"), Some("/tmp/€x%zz%4".into()));
    assert_eq!(super::decode_file_url("file://h/a%20b"), Some("/a b".into()));
}

#[test]
#[cfg(windows)]
fn osc7_unc_paths_keep_their_server() {
    // the PowerShell hook's forms, with the % and ; it encodes
    assert_eq!(super::decode_file_url("file://localhost///server/share/a%3Bb"), Some(r"\\server\share\a;b".into()));
    assert_eq!(super::decode_file_url("file://localhost/C:/a%2520b"), Some(r"C:\a%20b".into()));
    // a remote shell's directory is not turned into a share
    assert_eq!(super::decode_file_url("file://remote/home/me"), Some("/home/me".into()));
}
