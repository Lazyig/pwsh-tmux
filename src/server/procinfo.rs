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

fn is_cmd(shell: &str) -> bool {
    std::path::Path::new(shell).file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("cmd"))
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

/// argv to run a command string through the shell (tmux's `default-shell -c command`).
pub fn via_shell(shell: &str, command: &str) -> Vec<String> {
    if is_cmd(shell) {
        return vec![shell.into(), "/c".into(), command.into()];
    }
    if is_powershell(shell) {
        return vec![shell.into(), "-NoLogo".into(), "-Command".into(), command.into()];
    }
    vec![shell.into(), "-c".into(), command.into()]
}

/// Shell used for run-shell / if-shell / pipe-pane jobs.
pub fn job_command(command: &str) -> std::process::Command {
    #[cfg(windows)]
    {
        let shell = default_shell();
        let argv = via_shell(&shell, command);
        let mut c = std::process::Command::new(&argv[0]);
        c.args(&argv[1..]);
        c
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

pub fn program_name(argv0: &str) -> String {
    std::path::Path::new(argv0)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| argv0.to_string())
}

/// What each pane runs, for (shell pid, pty foreground process group) pairs: the
/// foreground program's name and, where it can be read, its working directory.
///
/// On Unix that is the pty's foreground process group. On Windows it is the deepest
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
                let comm = std::fs::read_to_string(format!("/proc/{p}/comm")).ok().map(|c| c.trim().to_string());
                let cwd = std::fs::read_link(format!("/proc/{p}/cwd")).ok().map(|p| p.to_string_lossy().into_owned());
                (comm, cwd)
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
