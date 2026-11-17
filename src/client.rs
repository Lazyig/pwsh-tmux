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

/// Names a file the client appends its terminal output to (used by the tests).
pub const OUTPUT_COPY: &str = "PWSH_TMUX_OUTPUT_COPY";

pub struct Global {
    pub config: Option<String>,
    pub no_start: bool,
    /// -C (1) / -CC (2): control mode
    pub control: u8,
    /// -T: extra terminal features ("RGB,sync,...")
    pub features: String,
    /// -u: the terminal is UTF-8 whatever the locale says
    pub utf8: bool,
}

fn spawn_server(config: Option<&str>) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    // Preserve source-file's ~/ form. A relative path stays as given (errors and
    // #{config_files} show it so); the server reads it relative to our cwd.
    let config = config.map(|path| {
        if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
            crate::server::procinfo::home_dir().map(|home| std::path::PathBuf::from(home).join(rest).to_string_lossy().into_owned()).unwrap_or_else(|| path.into())
        } else { path.to_string() }
    });
    let config = config.as_deref();
    let exe = std::env::current_exe()?;
    // the configuration's spawns start in the client's directory (cfg_client's cwd)
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let make = || {
        let mut c = Command::new(&exe);
        c.arg("__server").arg("-c").arg(&cwd);
        if let Some(f) = config {
            c.arg(f);
        }
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).env_remove("PWSH_TMUX").env_remove("PWSH_TMUX_PANE").env_remove(OUTPUT_COPY);
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
        win_spawn_detached(&exe, config, &cwd)
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

/// One argument quoted for CommandLineToArgvW: backslashes are doubled before a quote
/// and at the end (a cwd of `C:\\` must not escape the closing quote).
#[cfg_attr(not(windows), allow(dead_code))]
fn win_quote(s: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for ch in s.chars() {
        match ch {
            '\\' => {
                slashes += 1;
                continue;
            }
            '"' => out.push_str(&"\\".repeat(slashes * 2 + 1)),
            _ => out.push_str(&"\\".repeat(slashes)),
        }
        slashes = 0;
        out.push(ch);
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

/// Starts the server with CreateProcessW directly: std's Command always enables handle
/// inheritance, which leaks whatever inheritable handles the client got from its parent
/// (e.g. sshd's session pipes, keeping `ssh host pwsh-tmux new -d` from ever returning).
#[cfg(windows)]
fn win_spawn_detached(exe: &std::path::Path, config: Option<&str>, cwd: &str) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS, PROCESS_INFORMATION, STARTUPINFOW,
    };
    let quote = win_quote;
    let mut cmdline = format!("{} __server -c {}", quote(&exe.to_string_lossy()), quote(cwd));
    if let Some(f) = config {
        cmdline.push(' ');
        cmdline.push_str(&quote(f));
    }
    let mut cmdline: Vec<u16> = cmdline.encode_utf16().chain(std::iter::once(0)).collect();
    // environment block without our nesting markers
    let mut env: Vec<u16> = Vec::new();
    for (k, v) in std::env::vars_os() {
        if k == "PWSH_TMUX" || k == "PWSH_TMUX_PANE" || k == OUTPUT_COPY {
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
        // client_main: a refused connection is "no server running"; tmux leaves the socket
        // when its server exits but this server removes it, so a missing one is the same
        Err(e) if !autostart && matches!(e.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound) => {
            return Err(format!("no server running on {}", ipc::describe()))
        }
        Err(e) if !autostart => return Err(format!("error connecting to {} ({})", ipc::describe(), crate::server::strerror(&e))),
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

/// A native Windows console supports VT even when a launcher inherited TERM=dumb.
/// Preserve explicit terminal names and non-console clients (including Unix).
/// CLIENT_UTF8: -u, running inside tmux, or a UTF-8 locale (LC_ALL, then LC_CTYPE, then LANG).
/// Windows consoles are driven in UTF-8.
fn client_utf8(flag: bool) -> bool {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let inside = std::env::var_os("TMUX").is_some() || var("PWSH_TMUX").is_some();
    let locale = var("LC_ALL").or_else(|| var("LC_CTYPE")).or_else(|| var("LANG")).unwrap_or_default().to_ascii_uppercase();
    flag || cfg!(windows) || inside || locale.contains("UTF-8") || locale.contains("UTF8")
}

fn terminal_name(term: String, windows_console: bool) -> String {
    if windows_console && term == "dumb" { String::new() } else { term }
}

fn prepare_stdin_buffers(args: &mut [String], input: &mut impl Read) -> std::io::Result<()> {
    let mut start = 0;
    for index in 0..args.len() {
        // command boundaries as split_commands finds them: ";" alone, or "x;" (not "x\;")
        let separate = args[index] == ";";
        let suffixed = !separate && args[index].ends_with(';') && !args[index].ends_with("\\;");
        if !separate && !suffixed && index + 1 != args.len() { continue; }
        let end = if separate { index } else { index + 1 };
        if end > start + 1 && matches!(args[start].as_str(), "load-buffer" | "loadb") {
            let last = end - 1;
            let path = if suffixed { &args[last][..args[last].len() - 1] } else { &args[last] };
            if path == "-" {
                let mut data = Vec::new();
                input.take(MAX_STDIN_BUFFER as u64 + 1).read_to_end(&mut data)?;
                if data.len() > MAX_STDIN_BUFFER { return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "stdin buffer exceeds command frame limit")); }
                let separator = if suffixed { ";" } else { "" };
                args[last] = encode_stdin_buffer(&data) + separator;
            }
        }
        start = index + 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_quote_survives_command_line_parsing() {
        assert_eq!(win_quote(r"C:\Users\a b"), r#""C:\Users\a b""#);
        assert_eq!(win_quote(r"C:\"), r#""C:\\""#);
        assert_eq!(win_quote(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(win_quote(""), r#""""#);
    }

    #[test]
    fn native_windows_console_does_not_inherit_dumb_color_capabilities() {
        assert_eq!(terminal_name("dumb".into(), true), "");
        assert_eq!(terminal_name("xterm-256color".into(), true), "xterm-256color");
        assert_eq!(terminal_name("dumb".into(), false), "dumb");
        assert_eq!(terminal_name(String::new(), true), "");
    }

    #[test]
    fn stdin_data_stays_in_one_argument_in_compound_commands() {
        let data = b"buffer\0;\0set-option\0-g\0@injected\0yes\xff\\;";
        let mut argv = ["loadb", "-", ";", "display-message", "-p", "done"].map(String::from);
        prepare_stdin_buffers(&mut argv, &mut &data[..]).unwrap();
        let commands = crate::cmdparse::split_commands(&decode_args(&encode_args(&argv)));
        assert_eq!(commands.len(), 2);
        assert_eq!(decode_stdin_buffer(&commands[0][1]).unwrap(), data);
        assert_eq!(commands[1], ["display-message", "-p", "done"]);
    }

    #[test]
    fn stdin_loading_recognizes_command_boundaries_and_limits() {
        let mut argv = ["run-shell", "load-buffer", "-"].map(String::from);
        let mut input = std::io::Cursor::new(b"not consumed");
        prepare_stdin_buffers(&mut argv, &mut input).unwrap();
        assert_eq!(input.position(), 0);
        let mut argv = ["load-buffer", "-;", "load-buffer", "-"].map(String::from);
        prepare_stdin_buffers(&mut argv, &mut &b"data"[..]).unwrap();
        let commands = crate::cmdparse::split_commands(&argv);
        assert_eq!(decode_stdin_buffer(&commands[0][1]).unwrap(), b"data");
        assert!(decode_stdin_buffer(&commands[1][1]).unwrap().is_empty());
        let mut argv = ["load-buffer", "-"].map(String::from);
        assert!(prepare_stdin_buffers(&mut argv, &mut std::io::repeat(b'x')).is_err());
    }
}

pub fn main(mut args: Vec<String>, g: Global) -> i32 {
    let default_command = args.is_empty();
    let cmds = crate::cmdparse::split_commands(&args);
    let starts = cmds.iter().any(|c| matches!(c.first().map(String::as_str), Some("new-session" | "new" | "start-server" | "start")));
    if let Err(error) = prepare_stdin_buffers(&mut args, &mut std::io::stdin().lock()) {
        eprintln!("cannot load buffer from stdin: {error}");
        return 1;
    }
    let command_payload = encode_args(&args);
    if command_payload.len() > MAX_CLIENT_FRAME {
        eprintln!("command frame too large (stdin data must fit after base64 encoding)");
        return 1;
    }
    let stream = match connect((starts || default_command) && !g.no_start, g.config.as_deref()) {
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
        terminal_name(std::env::var("TERM").unwrap_or_default(), cfg!(windows) && is_tty()),
        std::process::id().to_string(),
        std::env::var("PWSH_TMUX_PANE").ok().filter(|_| std::env::var_os("PWSH_TMUX").is_some()).unwrap_or_default(),
        g.control.to_string(),
        g.features.clone(),
        if client_utf8(g.utf8) { "1".into() } else { "0".into() },
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
    if !post(C_IDENTIFY, &encode_args(&ident)) || !post(C_COMMAND, &command_payload) {
        eprintln!("lost server");
        return 1;
    }

    let mut raw: Option<term::RawMode> = None;
    let mut control_started = false;
    let mut alternate = true;
    // Set while a lock command or a suspended shell owns the terminal.
    let paused = Arc::new(AtomicBool::new(false));
    let mut out = std::io::stdout();
    // a copy of the terminal output for the tests: on Windows ConPTY re-renders what the
    // client writes and keeps some sequences (OSC 52 queries) from the terminal
    let mut output_copy = std::env::var_os(OUTPUT_COPY).and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
    let restore = |out: &mut std::io::Stdout, alternate: bool| {
        let _ = out.write_all(b"\x1b[?2031l\x1b[0m\x1b[?25h\x1b[?1l\x1b[?2004l\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1004l\x1b[>4m");
        let _ = out.write_all(if alternate { b"\x1b[?1049l" } else { b"\x1b[H\x1b[2J" });
        let _ = out.flush();
    };
    loop {
        let frame = read_frame(&mut &*stream);
        match frame {
            Ok((S_OUTPUT, p)) => {
                term::note_focus_mode(&p);
                let _ = out.write_all(&p);
                let _ = out.flush();
                if let Some(f) = output_copy.as_mut() {
                    let _ = f.write_all(&p);
                }
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
                        restore(&mut out, alternate);
                        eprintln!("{text}");
                        return 1;
                    }
                    continue;
                }
                // bytes of arguments that were not UTF-8 are printed as they were
                if code == 0 {
                    let _ = std::io::stdout().write_all(&crate::rawarg::to_bytes(&text));
                } else if !text.is_empty() {
                    // a bare exit status (new-pane -W) prints nothing
                    let nl = if text.ends_with('\n') { "" } else { "\n" };
                    let _ = std::io::stderr().write_all(&crate::rawarg::to_bytes(&format!("{text}{nl}")));
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
            Ok((S_ATTACHED, payload)) => {
                alternate = payload != b"no-clear";
                if raw.is_none() {
                    match term::RawMode::enter() {
                        Ok(r) => raw = Some(r),
                        Err(e) => {
                            eprintln!("can't use terminal: {e}");
                            return 1;
                        }
                    }
                    // the server sends the theme and colour queries (VT100-like terminals only)
                    if alternate { let _ = out.write_all(b"\x1b[?1049h\x1b[H\x1b[2J"); }
                    else {
                        // Scroll the previous terminal contents into scrollback (tmux scrolls
                        // sy + 1 lines with indn, which the terminal caps at its height).
                        let _ = out.write_all(format!("\x1b[{};1H{}\x1b[H", rows, "\r\n".repeat(rows as usize)).as_bytes());
                    }
                    let _ = out.flush();
                    // tmux's client answers MSG_READY with MSG_RESIZE (client-resized)
                    let (c, r) = term::size();
                    post(C_RESIZE, &encode_size(c, r));
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
            Ok((S_EXEC, p)) => {
                // detach-client -E: the shell command replaces the client
                drop(raw.take());
                restore(&mut out, alternate);
                let p = String::from_utf8_lossy(&p).into_owned();
                let (cmd, shell) = p.split_once('\0').unwrap_or((p.as_str(), ""));
                let shell = if shell.is_empty() { crate::server::procinfo::default_shell() } else { shell.to_string() };
                let argv = crate::server::procinfo::via_shell(&shell, cmd);
                let mut c = std::process::Command::new(&argv[0]);
                c.args(&argv[1..]);
                #[cfg(unix)]
                {
                    use std::os::unix::process::CommandExt;
                    let e = c.exec();
                    eprintln!("{}: {e}", argv[0]);
                    std::process::exit(1);
                }
                #[cfg(not(unix))]
                std::process::exit(c.status().ok().and_then(|s| s.code()).unwrap_or(1));
            }
            Ok((S_EXIT, p)) => {
                drop(raw.take());
                restore(&mut out, alternate);
                println!("{}", String::from_utf8_lossy(&p));
                // The input thread is blocked in a terminal read; exit without joining it.
                std::process::exit(0);
            }
            Ok((S_SUSPEND, _)) => {
                paused.store(true, Ordering::SeqCst);
                drop(raw.take());
                restore(&mut out, alternate);
                #[cfg(unix)]
                unsafe {
                    libc::raise(libc::SIGTSTP);
                }
                raw = term::RawMode::enter().ok();
                if alternate { let _ = out.write_all(b"\x1b[?1049h"); }
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
                restore(&mut out, alternate);
                let cmd = String::from_utf8_lossy(&p).into_owned();
                let _ = crate::server::procinfo::job_command(&cmd).status();
                raw = term::RawMode::enter().ok();
                if alternate { let _ = out.write_all(b"\x1b[?1049h"); }
                let _ = out.flush();
                paused.store(false, Ordering::SeqCst);
                post(C_RESUMED, b"");
            }
            Ok(_) => {}
            Err(_) => {
                let was_attached = raw.is_some();
                drop(raw.take());
                if was_attached {
                    restore(&mut out, alternate);
                    println!("[lost server]");
                } else {
                    eprintln!("lost server");
                }
                std::process::exit(1);
            }
        }
    }
}
