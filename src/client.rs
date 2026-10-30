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
    /// -C (1) / -CC (2): control mode
    pub control: u8,
    /// -T: extra terminal features ("RGB,sync,...")
    pub features: String,
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
        let _ = make;
        win_spawn_detached(&exe, config)
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

/// Starts the server with CreateProcessW directly: std's Command always enables handle
/// inheritance, which leaks whatever inheritable handles the client got from its parent
/// (e.g. sshd's session pipes, keeping `ssh host pwsh-tmux new -d` from ever returning).
#[cfg(windows)]
fn win_spawn_detached(exe: &std::path::Path, config: Option<&str>) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS, PROCESS_INFORMATION, STARTUPINFOW,
    };
    let quote = |s: &str| format!("\"{}\"", s.replace('"', "\\\""));
    let mut cmdline = format!("{} __server", quote(&exe.to_string_lossy()));
    if let Some(f) = config {
        cmdline.push(' ');
        cmdline.push_str(&quote(f));
    }
    let mut cmdline: Vec<u16> = cmdline.encode_utf16().chain(std::iter::once(0)).collect();
    // environment block without our nesting markers
    let mut env: Vec<u16> = Vec::new();
    for (k, v) in std::env::vars_os() {
        if k == "PWSH_TMUX" || k == "PWSH_TMUX_PANE" {
            continue;
        }
        env.extend(k.encode_wide());
        env.push('=' as u16);
        env.extend(v.encode_wide());
        env.push(0);
    }
    env.push(0);
    let home: Option<Vec<u16>> = std::env::var_os("USERPROFILE").map(|h| h.encode_wide().chain(std::iter::once(0)).collect());
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT;
    // Breaking away from the job keeps the server alive when e.g. an SSH
    // session's job object is closed; not every job allows it.
    for flags in [base | CREATE_BREAKAWAY_FROM_JOB, base] {
        let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(),
                cmdline.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0, // bInheritHandles = FALSE
                flags,
                env.as_ptr().cast(),
                home.as_ref().map_or(std::ptr::null(), |h| h.as_ptr()),
                &si,
                &mut pi,
            )
        };
        if ok != 0 {
            unsafe {
                CloseHandle(pi.hProcess);
                CloseHandle(pi.hThread);
            }
            return Ok(());
        }
    }
    Err(std::io::Error::last_os_error())
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
        g.control.to_string(),
        g.features.clone(),
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
    let mut control_started = false;
    // Set while a lock command or a suspended shell owns the terminal.
    let paused = Arc::new(AtomicBool::new(false));
    let mut out = std::io::stdout();
    let restore = |out: &mut std::io::Stdout| {
        let _ = out.write_all(b"\x1b[0m\x1b[?25h\x1b[?1l\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1004l\x1b[>4m\x1b[0 q\x1b[?1049l");
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
            Ok((S_ATTACHED, _)) if g.control > 0 => {
                if !control_started {
                    control_started = true;
                    if g.control == 2 {
                        // -CC: the whole session is wrapped in a DCS for the terminal (iTerm2)
                        let _ = out.write_all(b"\x1bP1000p");
                        let _ = out.flush();
                    }
                    let line_post = post.clone();
                    thread::spawn(move || {
                        use std::io::BufRead;
                        for line in std::io::stdin().lock().lines() {
                            let Ok(line) = line else { break };
                            if !line_post(C_CONTROL_LINE, line.as_bytes()) {
                                return;
                            }
                        }
                        // EOF on stdin detaches, like an empty line
                        line_post(C_CONTROL_LINE, b"");
                    });
                }
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
                            if paused.load(Ordering::SeqCst) {
                                thread::sleep(Duration::from_millis(50));
                                continue;
                            }
                            match input.wait(100) {
                                Ok(false) => continue,
                                Ok(true) => {}
                                // terminal gone: exit like tmux on SIGHUP (the server detaches us)
                                Err(_) => std::process::exit(1),
                            }
                            match input.read() {
                                Ok(data) if !data.is_empty() && input_post(C_INPUT, &data) => {}
                                Ok(data) if data.is_empty() => std::process::exit(1),
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
            Ok((S_EXIT, _)) if g.control > 0 => {
                if g.control == 2 {
                    let _ = out.write_all(b"\x1b\\");
                }
                let _ = out.flush();
                std::process::exit(0);
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
