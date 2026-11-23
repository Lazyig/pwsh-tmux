//! Server connection handling seen from outside the server process.

mod common;

/// Every connection has a reader and a writer thread. Both must end when the connection
/// does, whether the client sent nothing, ran a command or was refused for its first frame.
#[cfg(target_os = "linux")]
#[test]
fn connections_that_come_and_go_leave_no_threads_behind() {
    use common::*;
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let ctx = Ctx::new("conns");
    ctx.ok(&["-f", "/dev/null", "new-session", "-d", "-s", "s", "sleep 100"]);
    let pid = ctx.fmt("s", "#{pid}");
    let path = ctx.fmt("s", "#{socket_path}");
    let threads = || std::fs::read_dir(format!("/proc/{pid}/task")).unwrap().count();
    // the clients above are gone once nothing changes for a while
    let mut base = threads();
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        let now = threads();
        if now == base { break; }
        base = now;
    }
    for i in 0..500 {
        let mut s = UnixStream::connect(&path).unwrap();
        // a frame header with a kind no client sends first, half of the time
        if i % 2 == 1 { let _ = s.write_all(&[0xff, 0, 0, 0, 0]); }
        drop(s);
        if i % 25 == 0 { ctx.ok(&["display-message", "-p", "x"]); }
    }
    wait_for("the connection threads to end", || threads() <= base);
}
