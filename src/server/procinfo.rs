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

fn is_powershell(shell: &str) -> bool {
    let stem = std::path::Path::new(shell).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    stem == "pwsh" || stem == "powershell"
}

fn is_cmd(shell: &str) -> bool {
    std::path::Path::new(shell).file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("cmd"))
}

/// Wraps the user's prompt so PowerShell reports its location with OSC 7; that is how
/// #{pane_current_path} works on Windows, where another process's cwd can't be read.
const PWSH_HOOK: &str = "$global:__pwsh_tmux_prompt = $function:prompt; \
function global:prompt { $l = $executionContext.SessionState.Path.CurrentLocation; \
if ($l.Provider.Name -eq 'FileSystem') { [Console]::Write([char]27 + ']7;file://localhost/' + ($l.ProviderPath -replace '\\\\','/') + [char]7) }; \
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
        let mut c = std::process::Command::new("/bin/sh");
        c.args(["-c", command]);
        c
    }
}

pub fn program_name(argv0: &str) -> String {
    std::path::Path::new(argv0)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| argv0.to_string())
}

/// Name of the foreground program in a pane: on Unix the pty's foreground process group,
/// on Windows the deepest descendant of the pane's shell.
pub fn current_command(pid: Option<u32>, fg_pgrp: Option<i32>) -> Option<String> {
    #[cfg(unix)]
    {
        let p = fg_pgrp.map(|p| p as u32).or(pid)?;
        let comm = std::fs::read_to_string(format!("/proc/{p}/comm")).ok()?;
        Some(comm.trim().to_string())
    }
    #[cfg(windows)]
    {
        let _ = fg_pgrp;
        win::deepest_descendant(pid?).map(|n| program_name(&n))
    }
}

pub fn current_path(pid: Option<u32>, fg_pgrp: Option<i32>) -> Option<String> {
    #[cfg(unix)]
    {
        let p = fg_pgrp.map(|p| p as u32).or(pid)?;
        std::fs::read_link(format!("/proc/{p}/cwd")).ok().map(|p| p.to_string_lossy().into_owned())
    }
    #[cfg(windows)]
    {
        let _ = (pid, fg_pgrp);
        None
    }
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

    pub fn deepest_descendant(root: u32) -> Option<String> {
        let procs = processes();
        let name_of = |pid: u32| procs.iter().find(|p| p.0 == pid).map(|p| p.2.clone());
        let mut best = (0usize, root);
        let mut stack = vec![(root, 0usize)];
        while let Some((pid, depth)) = stack.pop() {
            for (child, _, name) in procs.iter().filter(|p| p.1 == pid && p.0 != pid) {
                let lname = name.to_lowercase();
                if lname == "conhost.exe" || lname == "openconsole.exe" {
                    continue;
                }
                if depth + 1 >= best.0 {
                    best = (depth + 1, *child);
                }
                stack.push((*child, depth + 1));
            }
        }
        name_of(best.1)
    }
}
