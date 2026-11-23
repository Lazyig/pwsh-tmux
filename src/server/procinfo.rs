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
/// the server reads or whose input it writes. On Windows the server has no console, so a
/// job gets a console of its own (without a window), with the system's console code page
/// (an OEM one such as 437 or 949, unless UTF-8 is set for the system): PowerShell and
/// cmd.exe write to a pipe in that code page, which loses what it has no character for
/// (`é` is `e` in 949). So the job's shell is first told to use UTF-8 ([`job_script`]),
/// and what is not UTF-8 is read in the console code page ([`job_text`]).
pub fn job_command(command: &str) -> std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let shell = default_shell();
        let mut c = via_shell(&shell, &job_script(&shell, command)).command();
        c.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        c
    }
    #[cfg(unix)]
    {
        shell_command(command)
    }
}

/// `command` for the shell of jobs on Windows, with its console set to UTF-8 first, so
/// what it prints reaches the server as printed. Nothing else changes for the command:
/// - PowerShell (5.1 and 7): [Console]::OutputEncoding (what PowerShell writes to a pipe
///   in, and the console code page the programs it runs get), [Console]::InputEncoding
///   (how it reads copy-pipe's and pipe-pane's text) and $OutputEncoding (what it gives
///   programs it pipes to) are UTF-8 without a BOM; an error doing so is ignored. Not for
///   a command that starts with `using`, which must come first in a script.
/// - cmd.exe: `chcp 65001`, its output and errors thrown away, then the command after
///   `&` (which binds least, so the command is all of what follows).
/// - Other shells are given the command as it is (Git Bash's programs write UTF-8).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn job_script(shell: &str, command: &str) -> String {
    match shell_kind(shell) {
        ShellKind::PowerShell => {
            let first = command.trim_start().split(|c: char| !c.is_ascii_alphabetic()).next().unwrap_or("");
            if first.eq_ignore_ascii_case("using") {
                return command.to_string();
            }
            format!("{PWSH_UTF8}; {command}")
        }
        ShellKind::Cmd => format!("chcp 65001 >nul 2>nul & {command}"),
        _ => command.to_string(),
    }
}

/// Sets a PowerShell job's console and encodings to UTF-8 ([`job_script`]).
#[cfg_attr(not(windows), allow(dead_code))]
const PWSH_UTF8: &str = "try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false; \
$OutputEncoding = [Console]::OutputEncoding; [Console]::InputEncoding = New-Object System.Text.UTF8Encoding $false } catch { }";

/// What a job printed, as text: on Unix the bytes as UTF-8, as tmux shows them. On
/// Windows each line that is UTF-8 is read as it is, and any other in the console code
/// page jobs get ([`job_command`]): a program that writes in that code page (as programs
/// run by Git Bash do, or a shell that was not told to use UTF-8) is shown as it would be
/// in a pane, whose console reads it in that code page too.
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

    fn processes() -> Vec<(u32, u32, String)> {
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

    #[cfg(windows)]
    #[test]
    fn windows_cmd_runs_its_command_as_written() {
        let out = via_shell("cmd.exe", "echo \"%%%\" & echo \"a b\"").command().stdin(std::process::Stdio::null()).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).replace(' ', ""), "\"%%%\"\r\n\"ab\"\r\n");
        let out = via_shell("cmd.exe", "\"cmd.exe\" /c echo \"x\"").command().stdin(std::process::Stdio::null()).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "\"x\"\r\n");
    }

    #[test]
    fn jobs_shells_print_utf8() {
        // PowerShell: the encodings first, unless the command must come first
        for ps in ["powershell.exe", "pwsh", "C:\\Program Files\\PowerShell\\7\\pwsh.exe"] {
            assert_eq!(job_script(ps, "echo '\u{d55c}\u{ae00}'"), format!("{PWSH_UTF8}; echo '\u{d55c}\u{ae00}'"));
            assert_eq!(job_script(ps, "  Using namespace System.Text; [UTF8Encoding]"), "  Using namespace System.Text; [UTF8Encoding]");
            assert_eq!(job_script(ps, "usingx"), format!("{PWSH_UTF8}; usingx"));
        }
        assert!(PWSH_UTF8.starts_with("try {") && PWSH_UTF8.ends_with("} catch { }"));
        assert!(PWSH_UTF8.contains("[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false"));
        // cmd.exe: chcp, its output and errors thrown away
        assert_eq!(job_script("cmd.exe", "echo \"%%%\" | more"), "chcp 65001 >nul 2>nul & echo \"%%%\" | more");
        // others as they are
        assert_eq!(job_script("bash", "echo 'x'; exit 3"), "echo 'x'; exit 3");
        assert_eq!(job_script("/bin/sh", "a"), "a");
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

    /// Runs `command` as a job's `shell` (a console of its own without a window, as
    /// the server's jobs have), with what it prints read as the server reads it.
    #[cfg(windows)]
    fn job_output(shell: &str, command: &str) -> String {
        use std::os::windows::process::CommandExt;
        let mut c = via_shell(shell, &job_script(shell, command)).command();
        c.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        c.stdin(std::process::Stdio::null());
        let o = c.output().expect("run the job");
        job_text(&o.stdout) + &job_text(&o.stderr)
    }

    #[cfg(windows)]
    #[test]
    fn windows_code_pages_are_read() {
        assert_eq!(win::code_page_text(b"\xc7\xd1\xb1\xdb", 949).as_deref(), Some("\u{d55c}\u{ae00}"));
        assert_eq!(win::code_page_text(b"\x82", 437).as_deref(), Some("\u{e9}"));
        assert_eq!(win::code_page_text(b"", 949).as_deref(), Some(""));
    }

    #[cfg(windows)]
    #[test]
    fn windows_jobs_print_what_their_shell_prints() {
        // Hangul, é and a curly quote: none of them are in every console code page (é and
        // ‘ are not in 949, Hangul is not in 437); PowerShell's ‘ ’ are its quotes
        let text = "\u{d55c}\u{ae00} \u{e9} \u{201c}q\u{201d}";
        let mut shells = vec!["powershell.exe"];
        if which("pwsh.exe").is_some() {
            shells.push("pwsh.exe");
        }
        // (the user's profile runs first and may print something)
        for ps in shells {
            let out = job_output(ps, &format!("echo '{text}'"));
            assert!(out.ends_with(&format!("\n{text}\r\n")) || out == format!("{text}\r\n"), "{ps}: {out:?}");
            let out = job_output(ps, &format!("echo '{text}' | findstr q"));
            assert!(out.trim_end().ends_with(text), "{ps}, a program it pipes to: {out:?}");
            let out = job_output(ps, "using namespace System.Text; [UTF8Encoding]::new($false).GetType().Name");
            assert!(out.trim_end().ends_with("UTF8Encoding"), "{ps}: {out:?}");
        }
        // cmd.exe: quotes as written, no backslashes added; its own output in UTF-8
        assert_eq!(job_output("cmd.exe", "echo \"%%%\" & echo \"a b\"").replace(' ', ""), "\"%%%\"\r\n\"ab\"\r\n");
        assert_eq!(job_output("cmd.exe", &format!("echo {text}")), format!("{text}\r\n"));
        assert_eq!(job_output("cmd.exe", "\"cmd.exe\" /c echo \"x\""), "\"x\"\r\n");
        // Git Bash, when it is installed
        let bash = std::path::Path::new("C:\\Program Files\\Git\\bin\\bash.exe");
        if bash.is_file() {
            let bash = bash.to_string_lossy();
            assert_eq!(job_output(&bash, &format!("echo '{text}' \"%%%\" \\\"")), format!("{text} %%% \"\n"));
        }
    }
}
