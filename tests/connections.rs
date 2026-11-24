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

/// The server serves at most 64 connections (SECURITY.md). One more is refused with a
/// message and exit status 1, not "lost server" and not "no server running" (which would
/// start a second server); the connections it has keep working.
#[cfg(target_os = "linux")]
#[test]
fn a_connection_over_the_limit_is_refused_with_a_message() {
    use common::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    const LIMIT: usize = 64;
    const MESSAGE: &str = "server has too many clients (limit is 64)\n";
    fn frame(kind: u8, args: &[&str]) -> Vec<u8> {
        let payload: Vec<u8> = args.iter().flat_map(|a| a.bytes().chain([0])).collect();
        let mut f = vec![kind];
        f.extend((payload.len() as u32).to_le_bytes());
        f.extend(payload);
        f
    }
    let identify = frame(5, &["80", "24", "0", "/", "xterm", "1", "", "0", "", "1"]);
    // runs a command on an identified connection: (status, stdout and reply text)
    let command = |s: &mut UnixStream, args: &[&str]| -> (u8, String) {
        s.set_nonblocking(false).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        s.write_all(&frame(1, args)).unwrap();
        let mut text = Vec::new();
        loop {
            let mut header = [0u8; 5];
            s.read_exact(&mut header).unwrap();
            let mut body = vec![0u8; u32::from_le_bytes(header[1..].try_into().unwrap()) as usize];
            s.read_exact(&mut body).unwrap();
            match header[0] {
                16 => text.extend(body),                              // S_STDOUT
                12 => {                                               // S_REPLY
                    text.extend(&body[1..]);
                    return (body[0], String::from_utf8_lossy(&text).into_owned());
                }
                _ => {}
            }
        }
    };

    let ctx = Ctx::new("limit");
    ctx.ok(&["-f", "/dev/null", "new-session", "-d", "-s", "s", "sleep 100"]);
    let pid = ctx.fmt("s", "#{pid}");
    let path = ctx.fmt("s", "#{socket_path}");
    // Fill the server with identified connections that send no command. One refused
    // (a client above had not yet gone) has a reply waiting: replace it.
    let mut held: Vec<UnixStream> = Vec::new();
    wait_for("64 connections to be accepted", || {
        while held.len() < LIMIT {
            let mut s = UnixStream::connect(&path).unwrap();
            s.write_all(&identify).unwrap();
            held.push(s);
        }
        std::thread::sleep(Duration::from_millis(200));
        held.retain(|s| {
            s.set_nonblocking(true).unwrap();
            matches!((&*s).read(&mut [0u8; 1]), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
        });
        held.len() == LIMIT
    });

    for args in [&["list-sessions"][..], &["new-session", "-d", "-s", "x"], &["attach-session"], &["-C", "attach-session"]] {
        let out = ctx.command().args(args).stdin(std::process::Stdio::null()).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), MESSAGE, "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }

    // the connections the server has still work, and no second server started
    assert_eq!(command(&mut held[0], &["display-message", "-p", "#{pid} #{session_name}"]), (0, format!("{pid} s\n")));
    assert_eq!(command(&mut held[1], &["has-session", "-t", "x"]).0, 1);
    // a command client that has finished leaves a free place
    held.remove(0);
    wait_for("a place to come free", || ctx.cmd(&["list-sessions", "-F", "#{session_name}"]) == (true, "s\n".into()));
    assert_eq!(ctx.fmt("s", "#{pid}"), pid);
}
