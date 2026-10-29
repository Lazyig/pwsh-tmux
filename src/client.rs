//! Command-line client: sends the command line to the server (starting it if needed),
//! prints replies, and becomes a terminal proxy when the server attaches it.

use crate::ipc;
use crate::protocol::*;
use crate::term;
use interprocess::local_socket::Stream;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct Global {
    pub config: Option<String>,
    pub no_start: bool,
}

fn spawn_server(config: Option<&str>) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe()?;
    let make = || {
        let mut c = Command::new(&exe);
        c.arg("__server");
        if let Some(f) = config {
            c.arg(f);
        }
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).env_remove("PWSH_TMUX").env_remove("PWSH_TMUX_PANE");
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

fn connect(autostart: bool, config: Option<&str>) -> Result<Stream, String> {
    match ipc::connect() {
        Ok(s) => return Ok(s),
        Err(e) if !autostart => return Err(format!("no server running on {} ({e})", ipc::describe())),
        Err(_) => {}
    }
    spawn_server(config).map_err(|e| format!("failed to start server: {e}"))?;
    for _ in 0..100 {
        thread::sleep(Duration::from_millis(50));
        if let Ok(s) = ipc::connect() {
            return Ok(s);
        }
    }
    Err("server did not start".into())
}

fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

pub fn main(mut args: Vec<String>, g: Global) -> i32 {
    if args.is_empty() {
        args.push("new-session".into());
    }
    let cmds = crate::cmdparse::split_commands(&args);
    let starts = cmds.iter().any(|c| matches!(c.first().map(String::as_str), Some("new-session" | "new" | "start-server" | "start")));
    // load-buffer - reads the client's stdin
    if let Some(pos) = args.iter().position(|a| a == "load-buffer" || a == "loadb") {
        if args.get(pos + 1..).is_some_and(|r| r.last().map(String::as_str) == Some("-")) {
            let mut data = String::new();
            let _ = std::io::stdin().read_to_string(&mut data);
            let last = args.len() - 1;
            // NUL separates protocol arguments, so mark inline data with \x01
            args[last] = format!("\x01stdin:{data}");
        }
    }
    let stream = match connect(starts && !g.no_start, g.config.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            // tmux prints nothing and fails for e.g. has-session with no server
            eprintln!("{e}");
            return 1;
        }
    };
    let (cols, rows) = term::size();
    let mut ident = vec![
        cols.to_string(),
        rows.to_string(),
        if is_tty() { "1".into() } else { "0".into() },
        std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        std::env::var("TERM").unwrap_or_default(),
        std::process::id().to_string(),
        std::env::var("PWSH_TMUX_PANE").ok().filter(|_| std::env::var_os("PWSH_TMUX").is_some()).unwrap_or_default(),
    ];
    ident.extend(std::env::vars().map(|(k, v)| format!("{k}={v}")));
    let stream = Arc::new(stream);
    let write_lock = Arc::new(Mutex::new(()));
    let post = {
        let stream = stream.clone();
        let lock = write_lock.clone();
        move |kind: u8, payload: &[u8]| -> bool {
            let _g = lock.lock().unwrap();
            write_frame(&mut &*stream, kind, payload).is_ok()
        }
    };
    if !post(C_IDENTIFY, &encode_args(&ident)) || !post(C_COMMAND, &encode_args(&args)) {
        eprintln!("lost server");
        return 1;
    }

    let mut raw: Option<term::RawMode> = None;
    // Set while a lock command or a suspended shell owns the terminal.
    let paused = Arc::new(AtomicBool::new(false));
    let mut out = std::io::stdout();
    let restore = |out: &mut std::io::Stdout| {
        let _ = out.write_all(b"\x1b[0m\x1b[?25h\x1b[?1l\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1004l\x1b[0 q\x1b[?1049l");
        let _ = out.flush();
    };
    loop {
        let frame = read_frame(&mut &*stream);
        match frame {
            Ok((S_OUTPUT, p)) => {
                let _ = out.write_all(&p);
                let _ = out.flush();
            }
            Ok((S_STDOUT, p)) => {
                let _ = out.write_all(&p);
                let _ = out.flush();
            }
            Ok((S_REPLY, p)) => {
                let (code, text) = decode_reply(&p);
                if raw.is_some() {
                    if code != 0 {
                        // attach failed after we went raw
                        drop(raw.take());
                        restore(&mut out);
                        eprintln!("{text}");
                        return 1;
                    }
                    continue;
                }
                if code == 0 {
                    print!("{text}");
                } else {
                    eprint!("{text}{}", if text.ends_with('\n') { "" } else { "\n" });
                }
                let _ = std::io::stdout().flush();
                return code as i32;
            }
            Ok((S_ATTACHED, _)) => {
                if raw.is_none() {
                    match term::RawMode::enter() {
                        Ok(r) => raw = Some(r),
                        Err(e) => {
                            eprintln!("can't use terminal: {e}");
                            return 1;
                        }
                    }
                    let _ = out.write_all(b"\x1b[?1049h\x1b[H\x1b[2J");
                    let _ = out.flush();
                    let input_post = post.clone();
                    let paused = paused.clone();
                    thread::spawn(move || {
                        let mut input = term::Input::default();
                        loop {
                            if paused.load(Ordering::SeqCst) || !input.wait(100) {
                                if paused.load(Ordering::SeqCst) {
                                    thread::sleep(Duration::from_millis(50));
                                }
                                continue;
                            }
                            match input.read() {
                                Ok(data) if !data.is_empty() && input_post(C_INPUT, &data) => {}
                                _ => break,
                            }
                        }
                    });
                    let resize_post = post.clone();
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
                }
            }
            Ok((S_EXIT, p)) => {
                drop(raw.take());
                restore(&mut out);
                println!("{}", String::from_utf8_lossy(&p));
                // The input thread is blocked in a terminal read; exit without joining it.
                std::process::exit(0);
            }
            Ok((S_SUSPEND, _)) => {
                paused.store(true, Ordering::SeqCst);
                drop(raw.take());
                restore(&mut out);
                #[cfg(unix)]
                unsafe {
                    libc::raise(libc::SIGTSTP);
                }
                raw = term::RawMode::enter().ok();
                let _ = out.write_all(b"\x1b[?1049h");
                let _ = out.flush();
                let (c, r) = term::size();
                post(C_RESIZE, &encode_size(c, r));
                paused.store(false, Ordering::SeqCst);
                post(C_RESUMED, b"");
            }
            Ok((S_SHELL, p)) => {
                paused.store(true, Ordering::SeqCst);
                // give the reader a moment to leave its wait
                thread::sleep(Duration::from_millis(150));
                drop(raw.take());
                restore(&mut out);
                let cmd = String::from_utf8_lossy(&p).into_owned();
                let _ = crate::server::procinfo::job_command(&cmd).status();
                raw = term::RawMode::enter().ok();
                let _ = out.write_all(b"\x1b[?1049h");
                let _ = out.flush();
                paused.store(false, Ordering::SeqCst);
                post(C_RESUMED, b"");
            }
            Ok(_) => {}
            Err(_) => {
                let was_attached = raw.is_some();
                drop(raw.take());
                if was_attached {
                    restore(&mut out);
                    println!("[lost server]");
                } else {
                    eprintln!("lost server");
                }
                std::process::exit(1);
            }
        }
    }
}
