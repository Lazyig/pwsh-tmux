//! Over SSH against a remote Windows/Linux host with pwsh-tmux installed.
//! Run with PT_SSH="ssh args..." (e.g. "-p 2222 -i key tester@127.0.0.1") and
//! PT_SSH_BIN=remote path of pwsh-tmux; skipped otherwise.

mod common;
use common::*;
use portable_pty::CommandBuilder;
use std::process::Command;

fn cfg() -> Option<(Vec<String>, String)> {
    let args = std::env::var("PT_SSH").ok()?;
    let bin = std::env::var("PT_SSH_BIN").ok()?;
    Some((args.split_whitespace().map(String::from).collect(), bin))
}

fn remote(args: &[String], cmdline: &str) -> (bool, String) {
    let out = Command::new("ssh").args(["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR"]).args(args).arg(cmdline).output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr))
}

#[test]
fn over_ssh() {
    let Some((args, bin)) = cfg() else {
        eprintln!("PT_SSH not set; skipping");
        return;
    };
    let _ = remote(&args, &format!("{bin} kill-server"));
    // a detached session survives the SSH connection that created it
    let (ok, out) = remote(&args, &format!("{bin} new-session -d -s keep"));
    assert!(ok, "new-session over ssh: {out}");
    std::thread::sleep(std::time::Duration::from_secs(2));
    let (ok, out) = remote(&args, &format!("{bin} ls -F '#S'"));
    assert!(ok && out.contains("keep"), "session lost after ssh disconnect: {out}");

    // interactive attach through ssh -tt
    let mut cb = CommandBuilder::new("ssh");
    cb.args(["-tt", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR"]);
    cb.args(&args);
    cb.arg(format!("{bin} attach -t keep"));
    let mut t = Term::spawn(cb, 100, 30);
    t.wait_screen("status over ssh", |s| s.contains("[keep]"));
    t.prefix(b"%");
    wait_for("split over ssh", || remote(&args, &format!("{bin} display -p -t keep '#{{window_panes}}'")).1.trim() == "2");
    t.send("echo 한글-over-ssh\r".as_bytes());
    t.wait_screen("korean echo", |s| s.matches("한글-over-ssh").count() >= 2);
    t.prefix(b"d");
    t.wait_exit();
    assert!(t.raw_text().contains("[detached (from session keep)]"));
    // reattach after the detach: the split and the shell are still there
    let (_, panes) = remote(&args, &format!("{bin} display -p -t keep '#{{window_panes}}'"));
    assert_eq!(panes.trim(), "2");
    let _ = remote(&args, &format!("{bin} kill-server"));
}
