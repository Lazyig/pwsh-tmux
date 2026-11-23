//! Platform helpers: default shell, hostname, and what a pane is running.

pub fn which(prog: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(prog);
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(prog)).find(|f| f.is_file())
}

pub fn home_dir() -> Option<String> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(var).ok().filter(|s| !s.is_empty())
}

pub fn hostname() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            return String::from_utf8_lossy(&buf[..end]).into_owned();
        }
        String::new()
    }
}

/// The shell used when neither a command nor default-command is given.
pub fn default_shell() -> String {
    if let Ok(sh) = std::env::var("PWSH_TMUX_SHELL") {
        if !sh.is_empty() {
            return sh;
        }
    }
    #[cfg(windows)]
    {
        if which("pwsh.exe").is_some() { "pwsh.exe".into() } else { "powershell.exe".into() }
    }
    #[cfg(unix)]
    {
        if which("pwsh").is_some() {
            return "pwsh".into();
        }
        std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())
    }
}

/// Whether `path` is a file that may be run (checkshell's access X_OK).
#[cfg(unix)]
pub fn is_executable(path: &str) -> bool {
    let Ok(c) = std::ffi::CString::new(path) else { return false };
    std::path::Path::new(path).is_file() && unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0
}

pub fn is_powershell(shell: &str) -> bool {
    let stem = std::path::Path::new(shell).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    stem == "pwsh" || stem == "powershell"
}

/// (Either separator on any platform, so the Windows choices are tested everywhere.)
fn is_cmd(shell: &str) -> bool {
    shell_kind(shell) == ShellKind::Cmd
}

/// The quoting a shell reads a command string with, for escaping a prompt's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind {
    /// sh and its kin: '\'' puts a single quote in a single-quoted string.
    Posix,
    /// PowerShell: '' (or a doubled ‘ ’ ‚ ‛) puts a single quote in one.
    PowerShell,
    /// cmd.exe: no single quotes; ^ escapes a special character outside double quotes.
    Cmd,
    /// Any other shell, whose quoting is not known.
    Other,
}

/// The kind of a shell from its name or path (either separator, any case, with or
/// without .exe), on any platform, so the Windows choices are tested everywhere.
pub fn shell_kind(shell: &str) -> ShellKind {
    let name = shell.rsplit(['/', '\\']).next().unwrap_or(shell).to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    match name {
        "pwsh" | "pwsh-preview" | "powershell" => ShellKind::PowerShell,
        "cmd" => ShellKind::Cmd,
        "sh" | "bash" | "dash" | "ash" | "zsh" | "ksh" | "ksh93" | "mksh" | "lksh" | "oksh" | "pdksh" | "yash" | "posh" | "bosh" => ShellKind::Posix,
        _ => ShellKind::Other,
    }
}

/// Wraps the user's prompt so PowerShell reports its location with OSC 7; that is how
/// #{pane_current_path} works on Windows, where another process's cwd can't be read.
/// `%` and `;` are percent-encoded: the URL is decoded, and `;` would end the OSC field.
const PWSH_HOOK: &str = "$global:__pwsh_tmux_prompt = $function:prompt; \
function global:prompt { $l = $executionContext.SessionState.Path.CurrentLocation; \
if ($l.Provider.Name -eq 'FileSystem') { [Console]::Write([char]27 + ']7;file://localhost/' + ($l.ProviderPath -replace '%','%25' -replace ';','%3B' -replace '\\\\','/') + [char]7) }; \
& $global:__pwsh_tmux_prompt }";

/// argv for an interactive shell.
pub fn interactive(shell: &str) -> Vec<String> {
    if is_powershell(shell) {
        let mut v = vec![shell.to_string(), "-NoLogo".into()];
        if cfg!(windows) && std::env::var_os("PWSH_TMUX_NO_HOOK").is_none() {
            v.extend(["-NoExit".into(), "-Command".into(), PWSH_HOOK.into()]);
        }
        return v;
    }
    vec![shell.to_string()]
}

/// What a shell is given to run a command string (tmux's `default-shell -c command`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellCommand {
    /// The program and its words, which Windows puts on the command line quoted for
    /// CommandLineToArgvW (a `"` in a word becomes `\"`).
    pub argv: Vec<String>,
    /// The rest of the command line, put there as it is: for cmd.exe on Windows, which
    /// reads its command line itself and takes `\"` as a backslash and a quote.
    pub raw: Option<String>,
}

impl ShellCommand {
    /// A program and its words, none of them raw.
    pub fn words(argv: Vec<String>) -> ShellCommand {
        ShellCommand { argv, raw: None }
    }

    /// A process for it (the command line as said above).
    pub fn command(&self) -> std::process::Command {
        let mut c = std::process::Command::new(&self.argv[0]);
        c.args(&self.argv[1..]);
        if let Some(raw) = &self.raw {
            #[cfg(windows)]
            std::os::windows::process::CommandExt::raw_arg(&mut c, raw);
            #[cfg(not(windows))]
            c.arg(raw);
        }
        c
    }
}

/// cmd.exe's command line after its name for running `command` as it is: `/s` makes
/// `/c` take off just the first and the last quote of the line, which are these, so
/// none of `command`'s own quotes are removed (without `/s`, a command that starts with
/// a quote would lose that one and its last).
pub fn cmd_line(command: &str) -> String {
    format!("/s /c \"{command}\"")
}

/// How `shell` runs a command string (tmux's `default-shell -c command`).
pub fn via_shell(shell: &str, command: &str) -> ShellCommand {
    via_shell_on(shell, command, cfg!(windows))
}

/// [`via_shell`] on Windows or elsewhere.
fn via_shell_on(shell: &str, command: &str, windows: bool) -> ShellCommand {
    if is_cmd(shell) {
        if windows {
            return ShellCommand { argv: vec![shell.into()], raw: Some(cmd_line(command)) };
        }
        return ShellCommand::words(vec![shell.into(), "/c".into(), command.into()]);
    }
    if is_powershell(shell) {
        return ShellCommand::words(vec![shell.into(), "-NoLogo".into(), "-Command".into(), command.into()]);
    }
    ShellCommand::words(vec![shell.into(), "-c".into(), command.into()])
}

/// A command string the shell of jobs runs in this process's console (`-c`, and
/// commands the server gives the client to run, such as lock-command).
pub fn shell_command(command: &str) -> std::process::Command {
    #[cfg(windows)]
    {
        via_shell(&default_shell(), command).command()
    }
    #[cfg(unix)]
    {
        // job_run: argv[0] is the shell's name (shell_argv0)
        use std::os::unix::process::CommandExt;
        let mut c = std::process::Command::new("/bin/sh");
        c.arg0("sh").args(["-c", command]);
        c
    }
}

/// Shell used for run-shell / if-shell / #() / pipe-pane / copy-pipe jobs, whose output
/// the server reads or whose input it writes. On Windows the server has no console, and
/// a console program it starts gets one in the system's console code page (an OEM one
/// such as 437 or 949, unless UTF-8 is set for the system), in which PowerShell and
/// cmd.exe write to a pipe and read one, losing what it has no character for (`é` is `e`
/// in 949); PowerShell reads its standard input in the code page it starts with, so
/// nothing its command does can change that. So the server makes one console without a
/// window for its jobs, in UTF-8 ([`win::jobs_console`]), and each job's shell is started
/// in it as the server's own child, with the standard input, output and error the caller
/// gives it and its command as it is. Without that console a job gets one of its own,
/// without a window, in the system's code page. What is not UTF-8 is read in the system's
/// console code page ([`job_text`]).
pub fn job_command(command: &str) -> std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let shell = default_shell();
        let in_jobs_console = win::jobs_console();
        let mut c = via_shell(&shell, &job_script(&shell, command, in_jobs_console)).command();
        c.creation_flags(job_creation_flags(in_jobs_console));
        c
    }
    #[cfg(unix)]
    {
        shell_command(command)
    }
}

/// What Windows PowerShell 5.1 runs first in the jobs' UTF-8 console. Its
/// `[Console]::InputEncoding` there is .NET Framework's code page 65001 encoding, which
/// has a byte order mark, and the writer it gives a program it pipes to writes that mark
/// before the text: `'x' | findstr x` finds nothing, as findstr reads `\u{feff}x`. The same
/// encoding without the mark keeps what is piped as `$OutputEncoding` makes it (ASCII,
/// unless the profile sets it): the console's code page stays 65001, and the job's input
/// is read in UTF-8 as before. It is a line of its own, so the job's command is as typed
/// from the start of the next one, its line numbers one more; a command that starts with
/// `using` or `param(...)`, which must come first, does not run as one. PowerShell 7
/// gives its encoding no mark.
#[cfg_attr(not(windows), allow(dead_code))]
pub const PS51_JOB_PRELUDE: &str = "try { [Console]::InputEncoding = New-Object System.Text.UTF8Encoding $false } catch {}";

/// Whether `shell` is Windows PowerShell (powershell.exe, 5.1 and before), not
/// PowerShell 7 (pwsh). (Either separator on any platform.)
#[cfg_attr(not(windows), allow(dead_code))]
fn is_windows_powershell(shell: &str) -> bool {
    let name = shell.rsplit(['\\', '/']).next().unwrap_or(shell).to_lowercase();
    name == "powershell" || name == "powershell.exe"
}

/// The command string a job's `shell` runs for `command`: [`PS51_JOB_PRELUDE`] and then
/// `command` for Windows PowerShell in the jobs' console, else `command` as it is.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn job_script(shell: &str, command: &str, in_jobs_console: bool) -> String {
    if in_jobs_console && is_windows_powershell(shell) {
        format!("{PS51_JOB_PRELUDE}\n{command}")
    } else {
        command.to_string()
    }
}

/// Windows' CREATE_NO_WINDOW: a console program gets a console of its own, without a
/// window.
#[cfg_attr(not(windows), allow(dead_code))]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The process creation flags of a job on Windows: none in the jobs' console, which the
/// job then shares with the server, else CREATE_NO_WINDOW, so that no console window
/// shows either way. (Not DETACHED_PROCESS, which would start the shell without a console
/// and give each console program it runs one with a window; not CREATE_NEW_CONSOLE,
/// whose console has a window.)
#[cfg_attr(not(windows), allow(dead_code))]
pub fn job_creation_flags(in_jobs_console: bool) -> u32 {
    if in_jobs_console { 0 } else { CREATE_NO_WINDOW }
}

/// Windows' console control events (wincon.h): Ctrl+C, Ctrl+Break, the console closing,
/// the user logging off, the system shutting down.
#[cfg_attr(not(windows), allow(dead_code))]
pub const CTRL_EVENTS: [u32; 5] = [0, 1, 2, 5, 6];

/// Whether the server goes on after console control event `kind`, which reaches it once
/// it is in the jobs' console (it had none and got none before). Ctrl+C and Ctrl+Break a
/// job sends to all of the console: yes. Logoff: yes, as Windows sends it to a console
/// process that outlives a user's logoff (a server started over SSH, in session 0, when
/// any user logs off), and one in the user's session is ended at logoff anyway. Shutdown:
/// yes, the system ends it. The console closing: no, as Windows ends the process when
/// its handler returns whatever it returns.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn survives_ctrl_event(kind: u32) -> bool {
    let [c, brk, _close, logoff, shutdown] = CTRL_EVENTS;
    [c, brk, logoff, shutdown].contains(&kind)
}

/// Makes up to `tries` attempts, `pause` apart, until one succeeds; stops early when
/// `gone` says there is nothing left to wait for. Whether an attempt succeeded.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn retry(tries: u32, pause: std::time::Duration, mut attempt: impl FnMut() -> bool, mut gone: impl FnMut() -> bool) -> bool {
    for n in 0..tries {
        if attempt() {
            return true;
        }
        if gone() {
            // one more try: what was waited for may have come just before it went
            return attempt();
        }
        if n + 1 < tries {
            std::thread::sleep(pause);
        }
    }
    false
}

/// What a job printed, as text: on Unix the bytes as UTF-8, as tmux shows them. On
/// Windows each line that is UTF-8 is read as it is (jobs' consoles are UTF-8, see
/// [`job_command`]), and any other in the system's console code page (`GetOEMCP`): a
/// program that writes in that code page whatever its console's is, is shown as a pane
/// whose console has that code page shows it.
pub fn job_text(bytes: &[u8]) -> String {
    #[cfg(windows)]
    {
        let cp = unsafe { windows_sys::Win32::Globalization::GetOEMCP() };
        decode_lines(bytes, |line| win::code_page_text(line, cp))
    }
    #[cfg(not(windows))]
    {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// `bytes` line by line: a line that is UTF-8 as it is, any other by `other` (else with
/// U+FFFD for what is not UTF-8). A line ends after `\n`, which no byte of a multibyte
/// character in an OEM or ANSI code page is.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn decode_lines(bytes: &[u8], other: impl Fn(&[u8]) -> Option<String>) -> String {
    let mut out = String::with_capacity(bytes.len());
    for line in bytes.split_inclusive(|&b| b == b'\n') {
        match std::str::from_utf8(line) {
            Ok(s) => out.push_str(s),
            Err(_) => match other(line) {
                Some(s) => out.push_str(&s),
                None => out.push_str(&String::from_utf8_lossy(line)),
            },
        }
    }
    out
}

pub fn program_name(argv0: &str) -> String {
    std::path::Path::new(argv0)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| argv0.to_string())
}

/// What each pane runs, for (shell pid, pty foreground process group) pairs: the
/// foreground program's name and, where it can be read, its working directory.
///
/// On Unix that is the pty's foreground process group, and the name is the first word
/// of its command line (osdep_get_name), such as `-bash` or `/usr/bin/vim`. On Windows it is the deepest
/// descendant of the pane's shell, found in one process snapshot shared by all panes.
/// This can be slow on a busy system, so the server calls it off its main thread.
pub fn pane_programs(panes: &[(Option<u32>, Option<i32>)]) -> Vec<(Option<String>, Option<String>)> {
    #[cfg(unix)]
    {
        panes
            .iter()
            .map(|&(pid, fg_pgrp)| {
                let Some(p) = fg_pgrp.map(|p| p as u32).or(pid) else { return (None, None) };
                if exec_pending(p) {
                    return (None, None);
                }
                // osdep_get_name: the first word of its command line as it is (the
                // name of the program a pane runs is parse_window_name of it), else its
                // name from the kernel
                let argv0 = std::fs::read(format!("/proc/{p}/cmdline")).ok().and_then(|c| {
                    let first = c.split(|&b| b == 0).next().unwrap_or_default();
                    (!first.is_empty()).then(|| String::from_utf8_lossy(first).into_owned())
                });
                let name = argv0.or_else(|| std::fs::read_to_string(format!("/proc/{p}/comm")).ok().map(|c| c.trim().to_string()));
                let cwd = std::fs::read_link(format!("/proc/{p}/cwd")).ok().map(|p| p.to_string_lossy().into_owned());
                (name, cwd)
            })
            .collect()
    }
    #[cfg(windows)]
    {
        if panes.iter().all(|(pid, _)| pid.is_none()) {
            return vec![(None, None); panes.len()];
        }
        let table = win::ProcessTable::snapshot();
        // the working directory comes from the shell's OSC 7 reports instead
        panes.iter().map(|&(pid, _)| (pid.and_then(|p| table.deepest_descendant(p)).map(|n| program_name(&n)), None)).collect()
    }
}

/// A pane's process that has not finished exec yet: what it runs is not known (the
/// caller uses the pane's start command). Between fork and exec it still runs the
/// server's image (its program and command line are the server's own) with the name of
/// the server thread that forked it, and during exec it has the new program but no
/// arguments yet and still the old name (the kernel sets the name before the arguments).
/// Checked before the name is read.
#[cfg(unix)]
pub fn exec_pending(pid: u32) -> bool {
    use std::sync::OnceLock;
    static SERVER: OnceLock<Option<(std::path::PathBuf, Vec<u8>)>> = OnceLock::new();
    let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else { return false };
    if cmdline.is_empty() {
        return true;
    }
    let Some((exe, own)) = SERVER.get_or_init(|| Some((std::fs::read_link("/proc/self/exe").ok()?, std::fs::read("/proc/self/cmdline").ok()?))) else {
        return false;
    };
    cmdline == *own && std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|e| e == *exe)
}

#[cfg(windows)]
pub mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };

    /// (pid, parent pid, executable name) of every process.
    pub(crate) fn processes() -> Vec<(u32, u32, String)> {
        let mut out = Vec::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return out;
            }
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut e);
            while ok != 0 {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                out.push((e.th32ProcessID, e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..len])));
                ok = Process32NextW(snap, &mut e);
            }
            CloseHandle(snap);
        }
        out
    }

    /// `bytes` in code page `cp` (MultiByteToWideChar), or None when it cannot be read.
    pub fn code_page_text(bytes: &[u8], cp: u32) -> Option<String> {
        use windows_sys::Win32::Globalization::MultiByteToWideChar;
        if bytes.is_empty() {
            return Some(String::new());
        }
        let len = i32::try_from(bytes.len()).ok()?;
        let n = unsafe { MultiByteToWideChar(cp, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0) };
        if n <= 0 {
            return None;
        }
        let mut wide = vec![0u16; n as usize];
        let n = unsafe { MultiByteToWideChar(cp, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), n) };
        (n > 0).then(|| String::from_utf16_lossy(&wide[..n as usize]))
    }

    /// Whether this process is in the console it made for jobs ([`super::job_command`]),
    /// making it the first time: a console without a window, whose input and output
    /// code pages are set to UTF-8 (65001) each time this is asked, as a job may have
    /// changed them (they are the console's, which the jobs share). Only a process that
    /// has no console makes one: the server, which the client starts with
    /// DETACHED_PROCESS. One that has a console (a test run in a terminal, a server
    /// started by hand) leaves it as it is, and its jobs get consoles of their own.
    /// False too when the console can no longer be used, so that jobs still get a
    /// console without a window.
    pub fn jobs_console() -> bool {
        use windows_sys::Win32::Globalization::CP_UTF8;
        use windows_sys::Win32::System::Console::{SetConsoleCP, SetConsoleOutputCP};
        static MADE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *MADE.get_or_init(make_jobs_console) && unsafe { SetConsoleCP(CP_UTF8) != 0 && SetConsoleOutputCP(CP_UTF8) != 0 }
    }

    /// A console without a window for this process, which has none. AllocConsole would
    /// show a window (or open a terminal, where one is the default), so a console program
    /// is started with CREATE_NO_WINDOW, which gives it a console without one, and this
    /// process attaches to that console, which stays while a process is in it. The program
    /// is cmd.exe's `pause`, which waits on its standard input (a pipe) and is ended once
    /// this process is attached; its console is made as it starts, after it is created,
    /// so attaching is retried (for up to five seconds).
    fn make_jobs_console() -> bool {
        use std::os::windows::process::CommandExt;
        use std::process::Stdio;
        use windows_sys::Win32::System::Console::{AttachConsole, GetConsoleCP, SetConsoleCtrlHandler};
        if unsafe { GetConsoleCP() } != 0 {
            return false;
        }
        // the system's cmd.exe (ComSpec may name another shell)
        let cmd = std::env::var_os("SystemRoot").map(|r| std::path::Path::new(&r).join("System32").join("cmd.exe")).filter(|p| p.is_file());
        let mut c = std::process::Command::new(cmd.unwrap_or_else(|| "cmd.exe".into()));
        c.args(["/d", "/c", "pause"]).creation_flags(super::CREATE_NO_WINDOW);
        c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
        let Ok(mut helper) = c.spawn() else { return false };
        let pid = helper.id();
        let stdin = helper.stdin.take();
        let attached = super::retry(
            500,
            std::time::Duration::from_millis(10),
            || unsafe { AttachConsole(pid) } != 0,
            || helper.try_wait().map_or(true, |s| s.is_some()),
        );
        drop(stdin);
        let _ = helper.kill();
        let _ = helper.wait();
        if attached {
            unsafe { SetConsoleCtrlHandler(Some(goes_on), 1) };
        }
        attached
    }

    /// The jobs' console handler: control events in that console, such as Ctrl+C and
    /// Ctrl+Break a job sends to all of it (GenerateConsoleCtrlEvent with group 0), or a
    /// logoff, reach this process too, which goes on ([`super::survives_ctrl_event`]).
    /// (Not SetConsoleCtrlHandler(NULL, TRUE), which the panes would inherit, and which
    /// covers only Ctrl+C.)
    unsafe extern "system" fn goes_on(kind: u32) -> windows_sys::core::BOOL {
        super::survives_ctrl_event(kind) as windows_sys::core::BOOL
    }

    /// Pids of direct children of `parent` with the given executable name.
    pub fn children_named(parent: u32, name: &str) -> Vec<u32> {
        processes().into_iter().filter(|p| p.1 == parent && p.2.eq_ignore_ascii_case(name)).map(|p| p.0).collect()
    }

    pub fn terminate(pid: u32) {
        use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !h.is_null() {
                TerminateProcess(h, 1);
                CloseHandle(h);
            }
        }
    }

    /// One process snapshot, indexed by parent, for looking up many panes at once.
    pub struct ProcessTable {
        names: std::collections::HashMap<u32, String>,
        /// Children of each pid in snapshot order (a process naming itself as its
        /// parent is left out).
        children: std::collections::HashMap<u32, Vec<u32>>,
    }

    impl ProcessTable {
        pub fn snapshot() -> ProcessTable {
            ProcessTable::from_list(processes())
        }

        pub fn from_list(procs: Vec<(u32, u32, String)>) -> ProcessTable {
            let mut names = std::collections::HashMap::with_capacity(procs.len());
            let mut children: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
            for (pid, parent, name) in procs {
                if pid != parent {
                    children.entry(parent).or_default().push(pid);
                }
                // a pid listed twice keeps its first name, as a linear search would
                names.entry(pid).or_insert(name);
            }
            ProcessTable { names, children }
        }

        /// Executable name of the deepest descendant of `root` (the latest found among
        /// equally deep ones), ignoring console hosts; `root` itself when it has none.
        pub fn deepest_descendant(&self, root: u32) -> Option<String> {
            let mut best = (0usize, root);
            let mut stack = vec![(root, 0usize)];
            // Parent IDs outlive their processes and may refer to a reused PID.
            // A snapshot can therefore contain cycles, even though a live process
            // tree cannot. Visit each PID once to bound both memory and traversal.
            let mut visited = std::collections::HashSet::from([root]);
            while let Some((pid, depth)) = stack.pop() {
                for &child in self.children.get(&pid).map(Vec::as_slice).unwrap_or_default() {
                    let name = self.names.get(&child).map(String::as_str).unwrap_or_default();
                    if name.eq_ignore_ascii_case("conhost.exe") || name.eq_ignore_ascii_case("openconsole.exe") {
                        continue;
                    }
                    if !visited.insert(child) {
                        continue;
                    }
                    if depth + 1 >= best.0 {
                        best = (depth + 1, child);
                    }
                    stack.push((child, depth + 1));
                }
            }
            self.names.get(&best.1).cloned()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::ProcessTable;

        fn deepest(procs: Vec<(u32, u32, String)>, root: u32) -> Option<String> {
            ProcessTable::from_list(procs).deepest_descendant(root)
        }

        #[test]
        fn reused_parent_ids_cannot_cycle_descendant_traversal() {
            let procs = vec![(1, 3, "shell.exe".into()), (2, 1, "child.exe".into()), (3, 2, "leaf.exe".into())];
            assert_eq!(deepest(procs, 1).as_deref(), Some("leaf.exe"));
        }

        #[test]
        fn ignores_console_hosts_and_keeps_leaf_commands() {
            let procs = vec![(1, 1, "shell.exe".into()), (2, 1, "conhost.exe".into()), (3, 1, "child.exe".into()), (4, 3, "tool.exe".into())];
            assert_eq!(deepest(procs.clone(), 1).as_deref(), Some("tool.exe"));
            assert_eq!(deepest(procs, 99), None);
        }

        #[test]
        fn one_table_answers_many_panes() {
            let procs = vec![
                (10, 1, "pwsh.exe".into()),
                (11, 10, "OpenConsole.exe".into()),
                (12, 10, "vim.exe".into()),
                (20, 1, "pwsh.exe".into()),
                (21, 20, "node.exe".into()),
                (22, 21, "esbuild.exe".into()),
                (23, 20, "git.exe".into()),
                (30, 1, "pwsh.exe".into()),
            ];
            let t = ProcessTable::from_list(procs);
            assert_eq!(t.deepest_descendant(10).as_deref(), Some("vim.exe"));
            assert_eq!(t.deepest_descendant(20).as_deref(), Some("esbuild.exe"));
            assert_eq!(t.deepest_descendant(30).as_deref(), Some("pwsh.exe"));
        }
    }
}

// (Linux only: the process's program, arguments and name are read from /proc)
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn a_process_that_has_not_exec_yet_has_no_program() {
        // this process stands for the server: its own image is what a pane's process
        // runs between fork and exec
        assert!(exec_pending(std::process::id()));
        // a child forked but not exec'd has the forking thread's name, not a program
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            loop {
                unsafe { libc::pause() };
            }
        }
        let forked = pane_programs(&[(Some(child as u32), None)]);
        unsafe {
            libc::kill(child, libc::SIGKILL);
            libc::waitpid(child, std::ptr::null_mut(), 0);
        }
        assert_eq!(forked, vec![(None, None)]);
        // a spawned program: nothing (spawn may return while exec is still going on), then
        // the program it runs, never the name it had before exec
        let mut sleep = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let start = std::time::Instant::now();
        let mut seen = Vec::new();
        while start.elapsed() < std::time::Duration::from_secs(5) {
            let name = pane_programs(&[(Some(sleep.id()), None)]).remove(0).0;
            seen.push(name.clone());
            if name.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let _ = sleep.kill();
        let _ = sleep.wait();
        assert!(seen.iter().all(|n| n.is_none() || n.as_deref() == Some("sleep")), "{seen:?}");
        assert_eq!(seen.last(), Some(&Some("sleep".to_string())));
    }
}

#[cfg(test)]
mod shell_tests {
    use super::*;

    /// What cmd.exe runs for its command line `line` after `/s /c `: it takes off the
    /// first character when it is a quote, and the last quote.
    fn cmd_runs(line: &str) -> String {
        let rest = line.strip_prefix("/s /c ").expect("/s /c");
        let rest = rest.strip_prefix('"').expect("a quote first");
        let last = rest.rfind('"').expect("a last quote");
        format!("{}{}", &rest[..last], &rest[last + 1..])
    }

    #[test]
    fn cmd_gets_its_command_line_as_it_is() {
        for command in ["echo \"%%%\" & pause", "\"C:\\Program Files\\x.exe\" \"a b\"", "echo \\\"x\\\" \"", "echo a", "", "echo \u{201c}q\u{201d} \u{2018}r\u{2019}"] {
            let run = via_shell_on("cmd.exe", command, true);
            assert_eq!(run.argv, vec!["cmd.exe".to_string()]);
            let raw = run.raw.expect("cmd.exe gets a raw command line");
            assert_eq!(raw, format!("/s /c \"{command}\""));
            assert_eq!(cmd_runs(&raw), command, "{raw}");
        }
        // any spelling of cmd.exe; not on Unix, where there is no cmd.exe to read it
        assert!(via_shell_on("C:\\Windows\\System32\\CMD.EXE", "x", true).raw.is_some());
        assert_eq!(via_shell_on("cmd", "echo \"a\"", false), ShellCommand::words(vec!["cmd".into(), "/c".into(), "echo \"a\"".into()]));
        // PowerShell and the others take words, which CommandLineToArgvW gives back
        assert_eq!(via_shell_on("powershell.exe", "echo \"a\"", true), ShellCommand::words(vec!["powershell.exe".into(), "-NoLogo".into(), "-Command".into(), "echo \"a\"".into()]));
        assert_eq!(via_shell_on("C:\\Program Files\\Git\\bin\\bash.exe", "echo \"a\"", true).raw, None);
    }

    #[test]
    fn windows_powershell_jobs_pipe_without_a_byte_order_mark() {
        for ps in ["powershell.exe", "powershell", "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", "POWERSHELL.EXE"] {
            assert_eq!(job_script(ps, "echo x | findstr x", true), format!("{PS51_JOB_PRELUDE}\necho x | findstr x"), "{ps}");
            assert_eq!(job_script(ps, "echo x", false), "echo x", "{ps}: not in the jobs' console");
        }
        for other in ["pwsh.exe", "C:\\Program Files\\PowerShell\\7\\pwsh.exe", "cmd.exe", "/bin/sh", "powershell-preview.exe"] {
            assert_eq!(job_script(other, "echo x", true), "echo x", "{other}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_cmd_runs_its_command_as_written() {
        let out = via_shell("cmd.exe", "echo \"%%%\" & echo \"a b\"").command().stdin(std::process::Stdio::null()).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).replace(' ', ""), "\"%%%\"\r\n\"ab\"\r\n");
        let out = via_shell("cmd.exe", "\"cmd.exe\" /c echo \"x\"").command().stdin(std::process::Stdio::null()).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "\"x\"\r\n");
    }

    #[test]
    fn jobs_never_get_a_console_window() {
        // in the jobs' console a job shares it; else it gets one of its own without a window
        assert_eq!(job_creation_flags(true), 0);
        assert_eq!(job_creation_flags(false), CREATE_NO_WINDOW);
        assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
        #[cfg(windows)]
        assert_eq!(CREATE_NO_WINDOW, windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }

    #[test]
    fn the_server_survives_its_jobs_consoles_events_but_its_closing() {
        let [c, brk, close, logoff, shutdown] = CTRL_EVENTS;
        assert!(survives_ctrl_event(c) && survives_ctrl_event(brk));
        assert!(survives_ctrl_event(logoff) && survives_ctrl_event(shutdown));
        assert!(!survives_ctrl_event(close));
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Console as w;
            assert_eq!(CTRL_EVENTS, [w::CTRL_C_EVENT, w::CTRL_BREAK_EVENT, w::CTRL_CLOSE_EVENT, w::CTRL_LOGOFF_EVENT, w::CTRL_SHUTDOWN_EVENT]);
        }
    }

    #[test]
    fn retry_stops_at_success_or_when_nothing_is_left_to_wait_for() {
        let pause = std::time::Duration::from_millis(1);
        // succeeds on the third attempt
        let mut n = 0;
        assert!(retry(5, pause, || { n += 1; n == 3 }, || false));
        assert_eq!(n, 3);
        // never: every attempt is made
        let mut n = 0;
        assert!(!retry(4, pause, || { n += 1; false }, || false));
        assert_eq!(n, 4);
        // gone after the second attempt: one more try, then no more
        let (mut n, mut checks) = (0, 0);
        assert!(!retry(100, pause, || { n += 1; false }, || { checks += 1; checks == 2 }));
        assert_eq!(n, 3);
        // what was waited for came just before it went
        let mut n = 0;
        assert!(retry(100, pause, || { n += 1; n == 2 }, || true));
        assert_eq!(n, 2);
        assert!(!retry(0, pause, || true, || false));
    }

    /// Runs `command` as the server runs a job: standard input NUL (or a pipe given
    /// `input`), output a pipe, errors NUL (a pipe with `stderr`, as for run-shell -E). Its
    /// exit status and output, read as the server reads it; None when it has not ended
    /// within 30 seconds (it is then ended).
    #[cfg(windows)]
    fn job(command: &str, input: Option<&str>, stderr: bool) -> Option<(Option<i32>, String)> {
        use std::io::{Read, Write};
        use std::process::Stdio;
        let mut c = job_command(command);
        c.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
        c.stdout(Stdio::piped()).stderr(if stderr { Stdio::piped() } else { Stdio::null() });
        let mut child = c.spawn().expect("start the job");
        if let (Some(text), Some(mut si)) = (input, child.stdin.take()) {
            si.write_all(text.as_bytes()).unwrap();
        }
        let read = |r: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut v = Vec::new();
                if let Some(mut r) = r {
                    let _ = r.read_to_end(&mut v);
                }
                v
            })
        };
        let out = read(child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>));
        let err = read(child.stderr.take().map(|e| Box::new(e) as Box<dyn Read + Send>));
        let start = std::time::Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            if start.elapsed() > std::time::Duration::from_secs(30) {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let mut bytes = out.join().unwrap();
        bytes.extend(err.join().unwrap());
        Some((status.code(), job_text(&bytes)))
    }

    /// The executable name of a job's process while it runs: the shell's, not pwsh-tmux's.
    #[cfg(windows)]
    fn job_process_name() -> Option<String> {
        use std::process::Stdio;
        let mut c = job_command("Start-Sleep 10");
        let mut child = c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("start the job");
        let name = win::processes().into_iter().find(|p| p.0 == child.id()).map(|p| p.2);
        let _ = child.kill();
        let _ = child.wait();
        name
    }

    /// Sets [`detached_job_checks`]'s case.
    #[cfg(windows)]
    const DETACHED_CASE: &str = "PWSH_TMUX_TEST_DETACHED_CASE";

    /// Runs `case` of [`detached_job_checks`] with `shell` as the jobs' shell in a copy of
    /// this test binary started as the client starts the server, with DETACHED_PROCESS,
    /// so without a console: it makes the jobs' console, as the server does.
    #[cfg(windows)]
    fn detached(case: &str, shell: &str) {
        use std::os::windows::process::CommandExt;
        let path = module_path!().split_once("::").map_or(module_path!(), |(_, rest)| rest);
        let name = format!("{path}::detached_job_checks");
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
            .env(DETACHED_CASE, case)
            .env("PWSH_TMUX_SHELL", shell)
            .creation_flags(windows_sys::Win32::System::Threading::DETACHED_PROCESS)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run this test binary without a console");
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && text.contains(" 1 passed"), "{case} with {shell}:\n{text}");
    }

    /// The PowerShells installed: Windows PowerShell, and PowerShell 7 when it is there.
    #[cfg(windows)]
    fn powershells() -> Vec<&'static str> {
        let mut v = vec!["powershell.exe"];
        if which("pwsh.exe").is_some() {
            v.push("pwsh.exe");
        }
        v
    }

    /// The checks [`detached`] runs, each in a process without a console (run by itself,
    /// without a case, it checks nothing).
    #[cfg(windows)]
    #[test]
    fn detached_job_checks() {
        use windows_sys::Win32::System::Console::{GetConsoleCP, GetConsoleOutputCP};
        let Ok(case) = std::env::var(DETACHED_CASE) else { return };
        assert_eq!(unsafe { GetConsoleCP() }, 0, "started without a console");
        assert!(win::jobs_console(), "the jobs' console is made");
        assert_eq!(unsafe { (GetConsoleCP(), GetConsoleOutputCP()) }, (65001, 65001));
        let shell = default_shell();
        let pwsh7 = program_name(&shell).eq_ignore_ascii_case("pwsh");
        // Hangul, é and curly double quotes: none of them are in every console code page
        // (é and the quotes are not in 949, Hangul is not in 437)
        let text = "\u{d55c}\u{ae00} \u{e9} \u{201c}q\u{201d}";
        let run = |command: &str| job(command, None, false).unwrap_or_else(|| panic!("{shell}: {command:?} did not end"));
        match case.as_str() {
            "utf8" => {
                // (the user's profile runs first and may print something)
                let (code, out) = run(&format!("echo '{text}'"));
                assert_eq!(code, Some(0));
                assert!(out.ends_with(&format!("{text}\r\n")), "{shell}: {out:?}");
                // the console code page it and its programs start with
                assert!(run("chcp").1.contains("65001"), "{shell}");
                let out = run("[Console]::InputEncoding.CodePage; [Console]::OutputEncoding.CodePage").1;
                assert!(out.ends_with("65001\r\n65001\r\n"), "{shell}: {out:?}");
                // (PowerShell's “ ” are quotes: a program it runs is given Hangul and é)
                let words = "\u{d55c}\u{ae00}\u{e9}";
                assert!(run(&format!("cmd.exe /d /c echo {words}")).1.ends_with(&format!("{words}\r\n")), "{shell}: a program it runs");
                // what it reads (copy-pipe, pipe-pane)
                let (_, out) = job("$input | ForEach-Object { 'got ' + $_ }", Some(&format!("{text}\n")), false).expect("ends");
                assert!(out.contains(&format!("got {text}")), "{shell}, its input: {out:?}");
                // the command as written: a command that must come first still can
                let out = run("using namespace System.Text; [UTF8Encoding]::new($false).GetType().Name").1;
                assert!(out.trim_end().ends_with("UTF8Encoding"), "{shell}: {out:?}");
                assert_eq!(run("exit 7").0, Some(7));
            }
            "cmd" => {
                assert_eq!(run(&format!("echo {text}")).1, format!("{text}\r\n"));
                assert_eq!(run("echo \"%%%\" & echo \"a b\"").1.replace(' ', ""), "\"%%%\"\r\n\"ab\"\r\n");
                assert_eq!(run("\"cmd.exe\" /c echo \"x\"").1, "\"x\"\r\n");
                assert!(run("chcp").1.contains("65001"));
                assert_eq!(run("exit /b 3").0, Some(3));
            }
            // Git Bash: its output and input as they are, its command as written
            "bash" => {
                let (code, out) = run(&format!("echo '{text}' \"%%%\" \\\""));
                assert_eq!((code, out.as_str()), (Some(0), format!("{text} %%% \"\n").as_str()));
                let (_, out) = job("cat", Some(&format!("{text}\n")), false).expect("ends");
                assert_eq!(out, format!("{text}\n"));
                assert_eq!(run("exit 5").0, Some(5));
            }
            // a): the job's process is the shell, started by the server itself
            "pid" => {
                let name = job_process_name();
                let want = std::path::Path::new(&shell).file_name().unwrap().to_string_lossy().into_owned();
                assert!(name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&want)), "{name:?}, not {want}");
                assert_eq!(run("echo SECURITY_PROOF").1.trim_end().lines().last(), Some("SECURITY_PROOF"));
            }
            // b): what a job starts neither keeps its shell from ending nor ends with it
            "start" => {
                let file = std::env::temp_dir().join(format!("pwsh-tmux-started-{}-{}", std::process::id(), program_name(&shell)));
                let _ = std::fs::remove_file(&file);
                let command = format!(
                    "Start-Process -WindowStyle Hidden -FilePath $env:ComSpec -ArgumentList '/d /c ping -n 3 127.0.0.1 >nul & echo done>\"{}\"'; 'started'",
                    file.display()
                );
                let (code, out) = job(&command, None, false).unwrap_or_else(|| panic!("{shell}: the shell did not end after Start-Process"));
                assert_eq!(code, Some(0), "{shell}: {out:?}");
                assert!(out.contains("started"), "{shell}: {out:?}");
                let start = std::time::Instant::now();
                while !file.exists() && start.elapsed() < std::time::Duration::from_secs(20) {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                assert!(file.exists(), "{shell}: what Start-Process started went on after the shell ended");
                let _ = std::fs::remove_file(&file);
            }
            // c): a failed command's error shows with run-shell -E; without it the error
            // output is NUL, as tmux discards a job's, and whether the error shows then is
            // up to the shell (not checked), but the status is the same
            "error" => {
                let (code, shown) = job("Get-PwshTmuxNoSuchCommand", None, true).expect("ends");
                assert_eq!(code, Some(1), "{shell}: {shown:?}");
                assert!(shown.contains("Get-PwshTmuxNoSuchCommand"), "{shell}, -E: {shown:?}");
                let (code, out) = run("Get-PwshTmuxNoSuchCommand");
                assert_eq!(code, Some(1), "{shell}: {out:?}");
            }
            // d): a program the shell pipes to gets $OutputEncoding (ASCII in Windows
            // PowerShell 5.1, as in a pane; UTF-8 in 7), and reads NUL as the job's input
            "findstr" => {
                let (code, out) = run(&format!("echo 'x{text}' | findstr x"));
                assert_eq!(code, Some(0), "{shell}: {out:?}");
                let want = if pwsh7 { format!("x{text}") } else { "x??".to_string() };
                assert!(out.contains(&want), "{shell}: {out:?}");
                let (code, out) = run("findstr x");
                assert_eq!(code, Some(1), "{shell}: findstr on the job's input (NUL): {out:?}");
            }
            other => panic!("no case {other}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_jobs_print_and_read_utf8_in_the_jobs_console() {
        for ps in powershells() {
            detached("utf8", ps);
        }
        detached("cmd", "cmd.exe");
        let bash = std::path::Path::new("C:\\Program Files\\Git\\bin\\bash.exe");
        if bash.is_file() {
            detached("bash", &bash.to_string_lossy());
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_a_job_is_its_shell() {
        // here, in a test run's console (left as it is), and as the server runs jobs
        let shell = default_shell();
        let want = std::path::Path::new(&shell).file_name().unwrap().to_string_lossy().into_owned();
        let name = job_process_name();
        assert!(name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&want)), "{name:?}, not {want}");
        for ps in powershells() {
            detached("pid", ps);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_what_a_job_starts_goes_on_without_holding_it() {
        for ps in powershells() {
            detached("start", ps);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_a_failed_powershell_command_shows_its_error() {
        for ps in powershells() {
            detached("error", ps);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_programs_a_job_pipes_to_get_its_output_encoding() {
        for ps in powershells() {
            detached("findstr", ps);
        }
    }

    #[test]
    fn job_output_lines_that_are_not_utf8_are_read_in_the_code_page() {
        // a stand-in for the console code page: Latin-1
        let latin1 = |b: &[u8]| Some(b.iter().map(|&c| c as char).collect::<String>());
        let bytes = [b"utf-8 \xc3\xa9\r\n".as_slice(), b"latin-1 \xe9\r\n", b"\xed\x95\x9c\n", b"last \xe9"].concat();
        assert_eq!(decode_lines(&bytes, latin1), "utf-8 \u{e9}\r\nlatin-1 \u{e9}\r\n\u{d55c}\nlast \u{e9}");
        // what the code page cannot read either is U+FFFD
        assert_eq!(decode_lines(b"a\xff\nb", |_| None), "a\u{fffd}\nb");
        assert_eq!(decode_lines(b"", latin1), "");
        #[cfg(not(windows))]
        assert_eq!(job_text(b"x\xe9\n"), "x\u{fffd}\n");
    }

    #[cfg(windows)]
    #[test]
    fn windows_code_pages_are_read() {
        assert_eq!(win::code_page_text(b"\xc7\xd1\xb1\xdb", 949).as_deref(), Some("\u{d55c}\u{ae00}"));
        assert_eq!(win::code_page_text(b"\x82", 437).as_deref(), Some("\u{e9}"));
        assert_eq!(win::code_page_text(b"", 949).as_deref(), Some(""));
    }
}
