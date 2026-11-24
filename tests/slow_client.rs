//! A client that stops reading (tmux's TTY_BLOCK): it is not disconnected, the server
//! does not queue the output it cannot take, and once it reads again it is redrawn.

mod common;

/// Like the tmux 3.8 run of the same scenario: the attached client is stopped with
/// SIGSTOP while two panes print 300000 lines each, stays stopped for longer than the
/// write timeout of command clients, then continues and shows the final screen.
#[cfg(unix)]
#[test]
fn a_stopped_client_is_kept_and_redrawn_when_it_continues() {
    use common::*;
    use std::time::Duration;

    let ctx = Ctx::new("slow");
    let go = std::env::temp_dir().join(format!("pwsh-tmux-slow-{}", ctx.socket));
    let _ = std::fs::remove_file(&go);
    let pane = format!("while [ ! -e '{}' ]; do sleep 0.05; done; seq 1 300000; echo END; exec sleep 1000", go.display());
    ctx.ok(&["-f", "/dev/null", "new-session", "-d", "-s", "s", "-x", "120", "-y", "32", "sh", "-c", &pane]);
    ctx.ok(&["set", "-g", "status", "off"]);
    ctx.ok(&["split-window", "-h", "-t", "s", "sh", "-c", &pane]);
    let t = Term::attach(&ctx, &["attach", "-t", "s"], 120, 32);
    wait_for("the client", || ctx.ok(&["list-clients", "-F", "#{client_width}x#{client_height}"]).trim() == "120x32");
    std::thread::sleep(Duration::from_millis(500));
    let pid = t.child.process_id().unwrap().to_string();
    let counters = || {
        let v = ctx.ok(&["list-clients", "-F", "#{client_written} #{client_discarded}"]);
        let v: Vec<u64> = v.split_whitespace().map(|n| n.parse().unwrap()).collect();
        assert_eq!(v.len(), 2, "one client");
        (v[0], v[1])
    };
    let (written0, _) = counters();

    assert!(std::process::Command::new("kill").args(["-STOP", &pid]).status().unwrap().success());
    std::fs::write(&go, b"").unwrap();
    let done = |p: &str| ctx.capture(p).lines().any(|l| l == "END");
    wait_for("both panes to finish", || done("s:0.0") && done("s:0.1"));
    // longer than IO_TIMEOUT: an attached client is not dropped for not reading
    std::thread::sleep(Duration::from_secs(11));
    let (written, discarded) = counters();
    // 2 x 1.9 MB of pane output; what was queued is the socket's buffer and about one
    // block threshold (1 + 120 * 32 * 8 bytes), not the frames of every render
    assert!(written - written0 < 1 << 20, "written {}", written - written0);
    assert!(discarded > 100_000, "discarded {discarded}");

    assert!(std::process::Command::new("kill").args(["-CONT", &pid]).status().unwrap().success());
    t.wait_screen("the final screen", |s| {
        let rows: Vec<&str> = s.lines().collect();
        rows.len() >= 31 && rows[29].starts_with("300000 ") && rows[29].ends_with("│300000")
            && rows[30].starts_with("END ") && rows[30].ends_with("│END")
    });
    // the whole screen, not only the last rows: the redraw after unblocking
    let rows: Vec<String> = t.contents().lines().map(str::to_string).collect();
    let width: usize = ctx.fmt("s:0.0", "#{pane_width}").parse().unwrap();
    let left: Vec<String> = ctx.capture("s:0.0").lines().map(str::to_string).collect();
    for (y, l) in left.iter().enumerate().take(31) {
        assert!(rows[y].starts_with(&format!("{l:<width$}│")), "row {y}: {:?} vs {l:?}", rows[y]);
    }
    assert_eq!(ctx.ok(&["list-clients"]).lines().count(), 1);
    let _ = std::fs::remove_file(&go);
}
