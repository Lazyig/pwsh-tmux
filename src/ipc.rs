//! Per-user server endpoint: a named pipe on Windows, a Unix socket in a 0700 directory elsewhere.

use interprocess::local_socket::{prelude::*, Listener, ListenerOptions, Stream};
use std::io;
use std::path::PathBuf;

fn label() -> String {
    std::env::var("PWSH_TMUX_SOCKET")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".into())
}

#[cfg(windows)]
fn pipe_name() -> String {
    if let Some(p) = std::env::var("PWSH_TMUX_SOCKET_PATH").ok().filter(|s| !s.is_empty()) {
        return p.trim_start_matches("\\\\.\\pipe\\").to_string();
    }
    // The SID (not USERNAME) identifies the user unambiguously across domains.
    let user = win::own_sid_string().unwrap_or_else(|| std::env::var("USERNAME").unwrap_or_else(|_| "user".into()));
    format!("pwsh-tmux-{}-{}", user, label())
}

/// Directory for the socket (Unix) and the server log.
pub fn runtime_dir() -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let base = std::env::var_os("PWSH_TMUX_TMPDIR")
            .or_else(|| std::env::var_os("TMUX_TMPDIR"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let uid = unsafe { libc::getuid() };
        let dir = base.join(format!("pwsh-tmux-{uid}"));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let meta = std::fs::symlink_metadata(&dir)?;
        if !meta.is_dir() || meta.uid() != uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not a directory owned by this user", dir.display()),
            ));
        }
        if meta.mode() & 0o077 != 0 {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(dir)
    }
    #[cfg(windows)]
    {
        let dir = std::env::temp_dir().join("pwsh-tmux");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}

#[cfg(unix)]
fn socket_path() -> io::Result<PathBuf> {
    if let Some(p) = std::env::var_os("PWSH_TMUX_SOCKET_PATH").filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    Ok(runtime_dir()?.join(label()))
}

/// Human-readable endpoint description for messages.
pub fn describe() -> String {
    #[cfg(unix)]
    {
        socket_path().map(|p| p.display().to_string()).unwrap_or_default()
    }
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\{}", pipe_name())
    }
}

/// Connects to the server, refusing one that runs as a different user
/// (e.g. a squatted pipe name on a shared Windows host).
pub fn connect() -> io::Result<Stream> {
    use interprocess::local_socket::traits::StreamCommon;
    #[cfg(unix)]
    let s = {
        use interprocess::local_socket::GenericFilePath;
        let path = socket_path()?;
        Stream::connect(path.as_path().to_fs_name::<GenericFilePath>()?)?
    };
    #[cfg(windows)]
    let s = {
        use interprocess::local_socket::GenericNamespaced;
        Stream::connect(pipe_name().to_ns_name::<GenericNamespaced>()?)?
    };
    let creds = s.peer_creds()?;
    #[cfg(unix)]
    let same = creds.euid().is_none_or(|u| u == unsafe { libc::geteuid() });
    #[cfg(windows)]
    let same = creds.pid().is_some_and(|p| win::same_user(p as u32));
    if !same {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{} is owned by another user", describe())));
    }
    Ok(s)
}

/// Creates the endpoint unless a server already answers on it (then `None`).
pub fn listen_exclusive() -> io::Result<Option<Listener>> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // Serialise check + bind so two autostarting clients can't both bind.
        let mut lock_path = socket_path()?.into_os_string();
        lock_path.push(".lock");
        let lock = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(&lock_path)?;
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
        if connect().is_ok() {
            return Ok(None);
        }
        listen().map(Some)
    }
    #[cfg(windows)]
    {
        if connect().is_ok() {
            return Ok(None);
        }
        // FIRST_PIPE_INSTANCE makes a concurrent second server fail here.
        listen().map(Some)
    }
}

fn listen() -> io::Result<Listener> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::GenericFilePath;
        let path = socket_path()?;
        // A stale socket from a crashed server would make bind fail.
        let _ = std::fs::remove_file(&path);
        ListenerOptions::new()
            .name(path.as_path().to_fs_name::<GenericFilePath>()?)
            .create_sync()
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::GenericNamespaced;
        use interprocess::os::windows::local_socket::ListenerOptionsExt;
        use interprocess::os::windows::security_descriptor::SecurityDescriptor;
        let mut opts = ListenerOptions::new().name(pipe_name().to_ns_name::<GenericNamespaced>()?);
        // Only this user (and SYSTEM) may open the pipe.
        if let Some(sid) = win::own_sid_string() {
            let sddl = widestring::U16CString::from_str(format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)"))
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
            opts = opts.security_descriptor(SecurityDescriptor::deserialize(&sddl)?);
        }
        opts.create_sync()
    }
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{EqualSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};

    /// TOKEN_USER of a process, in a u64 buffer for alignment.
    fn token_user(process: HANDLE) -> Option<Vec<u64>> {
        unsafe {
            let mut tok: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(process, TOKEN_QUERY, &mut tok) == 0 {
                return None;
            }
            let mut len = 0u32;
            GetTokenInformation(tok, TokenUser, std::ptr::null_mut(), 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
            let ok = GetTokenInformation(tok, TokenUser, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut len);
            CloseHandle(tok);
            (ok != 0).then_some(buf)
        }
    }

    pub fn own_sid_string() -> Option<String> {
        let buf = token_user(unsafe { GetCurrentProcess() })?;
        unsafe {
            let tu = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut p: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(tu.User.Sid, &mut p) == 0 || p.is_null() {
                return None;
            }
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            LocalFree(p.cast());
            Some(s)
        }
    }

    pub fn same_user(pid: u32) -> bool {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let other = token_user(h);
            CloseHandle(h);
            let (Some(other), Some(mine)) = (other, token_user(GetCurrentProcess())) else { return false };
            let a = &*(other.as_ptr() as *const TOKEN_USER);
            let b = &*(mine.as_ptr() as *const TOKEN_USER);
            EqualSid(a.User.Sid, b.User.Sid) != 0
        }
    }
}

pub fn cleanup() {
    #[cfg(unix)]
    if let Ok(p) = socket_path() {
        let _ = std::fs::remove_file(p);
    }
}
