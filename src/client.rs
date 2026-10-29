//! Command-line client: forwards commands to the server and runs the attach loop.

use crate::ipc;
use crate::opts::getopt;
use crate::protocol::*;
use crate::term;
use interprocess::local_socket::Stream;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub fn main(mut args: Vec<String>) -> i32 {
    if args.is_empty() {
        args.push("new-session".into());
    }
    let result = match args[0].as_str() {
        "new-session" | "new" => new_session(&args[1..]),
        "attach-session" | "attach" | "a" | "at" => {
            getopt(&args[1..], "dr", "t").and_then(|o| attach(o.get('t').unwrap_or("")))
        }
        _ => request(&args, false).and_then(|(code, text)| {
            if code == 0 {
                print!("{text}");
                Ok(0)
            } else {
                Err(text)
            }
        }),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("pwsh-tmux: {e}");
            1
        }
    }
}

fn spawn_server() -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe()?;
    let make = || {
        let mut c = Command::new(&exe);
        c.arg("__server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env_remove("PWSH_TMUX")
            .env_remove("PWSH_TMUX_PANE");
        // Don't pin the client's cwd for the server's lifetime.
        let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        if let Some(h) = std::env::var_os(home) {
            c.current_dir(h);
        }
        c
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
        // std spawns with handle inheritance on, which would leak our stdio
        // (e.g. a caller's capture pipe) into the long-lived server.
        for h in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            unsafe { SetHandleInformation(GetStdHandle(h), HANDLE_FLAG_INHERIT, 0) };
        }
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        // Breaking away from the job keeps the server alive when e.g. an SSH
        // session's job object is closed; not every job allows it.
        if make().creation_flags(base | CREATE_BREAKAWAY_FROM_JOB).spawn().is_ok() {
            return Ok(());
        }
        make().creation_flags(base).spawn().map(drop)
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut c = make();
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        c.spawn().map(drop)
    }
}

fn connect(autostart: bool) -> Result<Stream, String> {
    match ipc::connect() {
        Ok(s) => return Ok(s),
        Err(e) if !autostart => return Err(format!("no server running on {} ({e})", ipc::describe())),
        Err(_) => {}
    }
    spawn_server().map_err(|e| format!("failed to start server: {e}"))?;
    for _ in 0..100 {
        thread::sleep(Duration::from_millis(50));
        if let Ok(s) = ipc::connect() {
            return Ok(s);
        }
    }
    Err("server did not start".into())
}

/// Sends a command, returns (exit code, text).
fn request(args: &[String], autostart: bool) -> Result<(u8, String), String> {
    let stream = connect(autostart)?;
    let ctx = std::env::var("PWSH_TMUX_PANE").unwrap_or_default();
    let mut payload = vec![ctx];
    payload.extend(args.iter().cloned());
    write_frame(&mut &stream, C_COMMAND, &encode_args(&payload)).map_err(|e| e.to_string())?;
    loop {
        let (kind, p) = read_frame(&mut &stream).map_err(|e| format!("lost server: {e}"))?;
        if kind == S_REPLY {
            return Ok(decode_reply(&p));
        }
    }
}

fn check_nesting() -> Result<(), String> {
    if std::env::var_os("PWSH_TMUX").is_some_and(|v| !v.is_empty()) {
        return Err("sessions should be nested with care, unset PWSH_TMUX to force".into());
    }
    Ok(())
}

fn new_session(args: &[String]) -> Result<i32, String> {
    let o = getopt(args, "dAD", "sxycnt")?;
    let detached = o.has('d');
    if !detached {
        check_nesting()?;
    }
    let (cols, rows) = if detached { (80, 24) } else { term::size() };
    let mut full = vec!["new-session".to_string(), "-x".into(), cols.to_string(), "-y".into(), rows.to_string()];
    if let Ok(cwd) = std::env::current_dir() {
        full.extend(["-c".to_string(), cwd.to_string_lossy().into_owned()]);
    }
    // User flags come later and override the defaults above.
    full.extend(args.iter().cloned());
    let (code, name) = request(&full, true)?;
    if code != 0 {
        return Err(name);
    }
    if detached {
        return Ok(0);
    }
    attach(&name)
}

fn attach(target: &str) -> Result<i32, String> {
    check_nesting()?;
    let stream = connect(false)?;
    let (cols, rows) = term::size();
    let mut hello = encode_size(cols, rows);
    hello.extend_from_slice(target.as_bytes());

    let raw = term::RawMode::enter().map_err(|e| format!("can't use terminal: {e}"))?;
    let stream = Arc::new(stream);
    // Input and resize threads both write; keep their frames whole.
    let write_lock = Arc::new(Mutex::new(()));
    let post = {
        let stream = stream.clone();
        move |kind: u8, payload: &[u8]| -> bool {
            let _guard = write_lock.lock().unwrap();
            write_frame(&mut &*stream, kind, payload).is_ok()
        }
    };
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b[?1049h\x1b[H\x1b[2J");
    let _ = out.flush();

    if !post(C_ATTACH, &hello) {
        drop(raw);
        return Err("lost server".into());
    }
    let input_post = post.clone();
    thread::spawn(move || {
        let mut input = term::Input::default();
        while let Ok(data) = input.read() {
            if data.is_empty() || !input_post(C_INPUT, &data) {
                break;
            }
        }
    });
    let resize_post = post;
    thread::spawn(move || {
        let mut last = (cols, rows);
        loop {
            thread::sleep(Duration::from_millis(100));
            let now = term::size();
            if now != last {
                last = now;
                if !resize_post(C_RESIZE, &encode_size(now.0, now.1)) {
                    break;
                }
            }
        }
    });

    let (code, msg) = loop {
        match read_frame(&mut &*stream) {
            Ok((S_OUTPUT, p)) => {
                let _ = out.write_all(&p);
                let _ = out.flush();
            }
            Ok((S_EXIT, p)) => break (0, String::from_utf8_lossy(&p).into_owned()),
            Ok((S_REPLY, p)) => {
                let (code, text) = decode_reply(&p);
                if code != 0 {
                    break (1, format!("pwsh-tmux: {text}"));
                }
            }
            Ok(_) => {}
            Err(_) => break (1, "[lost server]".into()),
        }
    };
    let _ = out.write_all(b"\x1b[0m\x1b[?25h\x1b[?1l\x1b[?2004l\x1b[?1049l");
    let _ = out.flush();
    drop(raw);
    println!("{msg}");
    // The input thread is blocked in a terminal read; exit without joining it.
    std::process::exit(code);
}
