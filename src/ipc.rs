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
        // 0711 (traversable, set by server-access) is fine; readable/writable by others is not
        if meta.mode() & 0o066 != 0 {
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
        use interprocess::local_socket::{ConnectOptions, GenericNamespaced};
        // A busy pipe (every instance connected) is a server between two accepts, or one
        // on its way out (see cleanup): wait for an instance, or for the pipe to go away
        // (then there is no server), but not forever.
        ConnectOptions::new()
            .name(pipe_name().to_ns_name::<GenericNamespaced>()?)
            .wait_mode(interprocess::ConnectWaitMode::Timeout(std::time::Duration::from_secs(10)))
            .connect_sync()?
    };
    let creds = s.peer_creds()?;
    #[cfg(unix)]
    let same = creds.euid().is_some_and(|u| u == unsafe { libc::geteuid() });
    #[cfg(windows)]
    let same = creds.pid().is_some_and(|p| win::same_user(p as u32));
    // -S names another user's shared server explicitly (server-access); otherwise refuse
    let explicit = std::env::var_os("PWSH_TMUX_SOCKET_PATH").is_some_and(|p| !p.is_empty());
    if !same && !explicit {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{} is owned by another user", describe())));
    }
    Ok(s)
}

/// Creates the endpoint unless a server already answers on it (then `None`).
/// `extra_users`: Windows SIDs besides ours allowed to open the pipe (server-access).
pub fn listen_exclusive(extra_users: &[String]) -> io::Result<Option<Listener>> {
    #[cfg(unix)]
    let _ = extra_users;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // Serialise check + bind so two autostarting clients can't both bind.
        let mut lock_path = socket_path()?.into_os_string();
        lock_path.push(".lock");
        // The server that made the socket removes the lock file (below): a lock taken on
        // a file that is no longer at the path is taken again on the one there now, so
        // a client that opened the old file and one that made a new one cannot both bind.
        let lock = loop {
            use std::os::unix::fs::MetadataExt;
            let lock = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(&lock_path)?;
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
            let held = lock.metadata()?;
            if std::fs::metadata(&lock_path).is_ok_and(|m| m.dev() == held.dev() && m.ino() == held.ino()) {
                break lock;
            }
        };
        if connect().is_ok() {
            return Ok(None);
        }
        let listener = listen(extra_users)?;
        // server_start: the lock file goes once the socket is there (a client waiting on
        // it then connects to this server; a later one makes a new one and connects)
        let _ = std::fs::remove_file(&lock_path);
        drop(lock);
        Ok(Some(listener))
    }
    #[cfg(windows)]
    {
        if connect().is_ok() {
            return Ok(None);
        }
        // FIRST_PIPE_INSTANCE makes a concurrent second server fail here.
        listen(extra_users).map(Some)
    }
}

fn listen(#[cfg_attr(unix, allow(unused_variables))] extra_users: &[String]) -> io::Result<Listener> {
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
        // Only this user, SYSTEM and server-access users may open the pipe.
        {
            let sid = win::own_sid_string().ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "cannot determine pipe owner"))?;
            let others: String = extra_users.iter().filter(|s| s.starts_with("S-1-")).map(|s| format!("(A;;GA;;;{s})")).collect();
            let sddl = widestring::U16CString::from_str(format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY){others}"))
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

    unsafe fn sid_string(sid: windows_sys::Win32::Security::PSID) -> Option<String> {
        let mut p: *mut u16 = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &mut p) } == 0 || p.is_null() {
            return None;
        }
        let mut n = 0;
        while unsafe { *p.add(n) } != 0 {
            n += 1;
        }
        let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) });
        unsafe { LocalFree(p.cast()) };
        Some(s)
    }

    /// SID of the process at the other end of a server pipe (after data has been read from it).
    pub fn pipe_client_sid(pipe: HANDLE) -> Option<String> {
        use windows_sys::Win32::Security::RevertToSelf;
        use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
        use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};
        unsafe {
            if ImpersonateNamedPipeClient(pipe) == 0 {
                return None;
            }
            let mut tok: HANDLE = std::ptr::null_mut();
            let ok = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut tok);
            RevertToSelf();
            if ok == 0 {
                return None;
            }
            let mut len = 0u32;
            GetTokenInformation(tok, TokenUser, std::ptr::null_mut(), 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
            let ok = GetTokenInformation(tok, TokenUser, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut len);
            CloseHandle(tok);
            if ok == 0 {
                return None;
            }
            sid_string((*(buf.as_ptr() as *const TOKEN_USER)).User.Sid)
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn lookup_account(name: &str) -> Option<String> {
        use windows_sys::Win32::Security::{LookupAccountNameW, SID_NAME_USE};
        let mut sid = vec![0u64; 16];
        let mut sid_len = (sid.len() * 8) as u32;
        let mut dom = vec![0u16; 256];
        let mut dom_len = dom.len() as u32;
        let mut kind: SID_NAME_USE = 0;
        let ok = unsafe { LookupAccountNameW(std::ptr::null(), wide(name).as_ptr(), sid.as_mut_ptr().cast(), &mut sid_len, dom.as_mut_ptr(), &mut dom_len, &mut kind) };
        if ok == 0 {
            return None;
        }
        unsafe { sid_string(sid.as_mut_ptr().cast()) }
    }

    pub fn account_name(sid: &str) -> Option<String> {
        use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
        use windows_sys::Win32::Security::{LookupAccountSidW, SID_NAME_USE};
        unsafe {
            let mut psid: windows_sys::Win32::Security::PSID = std::ptr::null_mut();
            if ConvertStringSidToSidW(wide(sid).as_ptr(), &mut psid) == 0 {
                return None;
            }
            let (mut name, mut dom) = (vec![0u16; 256], vec![0u16; 256]);
            let (mut nlen, mut dlen) = (name.len() as u32, dom.len() as u32);
            let mut kind: SID_NAME_USE = 0;
            let ok = LookupAccountSidW(std::ptr::null(), psid, name.as_mut_ptr(), &mut nlen, dom.as_mut_ptr(), &mut dlen, &mut kind);
            LocalFree(psid.cast());
            if ok == 0 {
                return None;
            }
            let n = String::from_utf16_lossy(&name[..nlen as usize]);
            let d = String::from_utf16_lossy(&dom[..dlen as usize]);
            Some(if d.is_empty() { n } else { format!("{d}\\{n}") })
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

// ---- users (server-access) -------------------------------------------------------------

/// This process's user: the uid on Unix, the SID on Windows.
pub fn own_user_id() -> String {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() }.to_string()
    }
    #[cfg(windows)]
    {
        win::own_sid_string().unwrap_or_default()
    }
}

/// root / SYSTEM always get in, like tmux.
pub fn is_superuser(id: &str) -> bool {
    id == "0" || id == "S-1-5-18"
}

/// The user at the other end of a server-side connection. On Windows this needs a frame
/// to have been read from the pipe first.
pub fn peer_user_id(s: &Stream) -> Option<String> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::traits::StreamCommon;
        s.peer_creds().ok()?.euid().map(|u| u.to_string())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{AsHandle, AsRawHandle};
        match s {
            Stream::NamedPipe(p) => win::pipe_client_sid(p.inner().as_handle().as_raw_handle()),
        }
    }
}

pub fn user_id_for_name(name: &str) -> Option<String> {
    #[cfg(unix)]
    {
        let c = std::ffi::CString::new(name).ok()?;
        let pw = unsafe { libc::getpwnam(c.as_ptr()) };
        if pw.is_null() {
            return name.parse::<u32>().ok().map(|u| u.to_string());
        }
        Some(unsafe { (*pw).pw_uid }.to_string())
    }
    #[cfg(windows)]
    {
        if name.starts_with("S-1-") {
            return Some(name.to_string());
        }
        win::lookup_account(name)
    }
}

/// server-access -g: a group's ACL id ("g:" and the gid). Groups are Unix only.
pub fn group_id_for_name(name: &str) -> Option<String> {
    #[cfg(unix)]
    {
        let c = std::ffi::CString::new(name).ok()?;
        let gr = unsafe { libc::getgrnam(c.as_ptr()) };
        if gr.is_null() {
            return None;
        }
        Some(format!("g:{}", unsafe { (*gr).gr_gid }))
    }
    #[cfg(windows)]
    {
        let _ = name;
        None
    }
}

/// The group ACL id a user is matched by: their primary group (tmux uses the client's
/// effective gid, which is that group unless the client changed it).
pub fn group_id_of_user(id: &str) -> Option<String> {
    #[cfg(unix)]
    {
        let uid = id.parse::<u32>().ok()?;
        let pw = unsafe { libc::getpwuid(uid) };
        if pw.is_null() {
            return None;
        }
        Some(format!("g:{}", unsafe { (*pw).pw_gid }))
    }
    #[cfg(windows)]
    {
        let _ = id;
        None
    }
}

pub fn user_name_for_id(id: &str) -> String {
    #[cfg(unix)]
    if let Some(gid) = id.strip_prefix("g:").and_then(|g| g.parse::<u32>().ok()) {
        let gr = unsafe { libc::getgrgid(gid) };
        if gr.is_null() {
            return gid.to_string();
        }
        return unsafe { std::ffi::CStr::from_ptr((*gr).gr_name) }.to_string_lossy().into_owned();
    }
    #[cfg(unix)]
    {
        let Ok(uid) = id.parse::<u32>() else { return id.to_string() };
        let pw = unsafe { libc::getpwuid(uid) };
        if pw.is_null() {
            return id.to_string();
        }
        unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) }.to_string_lossy().into_owned()
    }
    #[cfg(windows)]
    {
        win::account_name(id).unwrap_or_else(|| id.to_string())
    }
}

/// Opens (or closes again) the endpoint to other users once server-access allows someone:
/// the socket directory becomes traversable and the socket writable. The server checks each
/// connecting user against its access list either way. On Windows the pipe's ACL is set
/// when the server starts (see listen_exclusive), so this does nothing there.
pub fn set_shared(shared: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let (Ok(dir), Ok(sock)) = (runtime_dir(), socket_path()) {
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(if shared { 0o711 } else { 0o700 }));
            let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(if shared { 0o777 } else { 0o700 }));
        }
    }
    #[cfg(windows)]
    let _ = shared;
}

#[cfg(windows)]
static STOPPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(windows)]
static STOPPED: (std::sync::Mutex<Option<bool>>, std::sync::Condvar) = (std::sync::Mutex::new(None), std::sync::Condvar::new());

/// Accepts connections on a thread until `conn` returns false or `cleanup` stops listening.
pub fn serve(listener: Listener, mut conn: impl FnMut(io::Result<Stream>) -> bool + Send + 'static) {
    #[cfg(windows)]
    {
        *STOPPED.0.lock().unwrap() = Some(false);
    }
    std::thread::spawn(move || {
        for c in listener.incoming() {
            // cleanup woke us with a connection of its own (or a client came just before
            // it): neither is served by a server on its way out
            #[cfg(windows)]
            if STOPPING.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            if !conn(c) {
                break;
            }
        }
        // the waiting pipe instance goes with the listener
        drop(listener);
        #[cfg(windows)]
        {
            *STOPPED.0.lock().unwrap() = Some(true);
            STOPPED.1.notify_all();
        }
    });
}

/// Stops listening, once: kill-server does it before it returns, so a client started next
/// starts a new server instead of reaching this one (tmux's server has exited by then).
/// Unix removes the socket (after that a new server may already own the path). A Windows
/// pipe name lasts while any instance of it is open, so the accept thread closes the
/// instance it waits on; until the connections still open are closed (this process
/// exits), a client finds the pipe busy and waits, then finds no pipe and starts a server.
pub fn cleanup() {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if DONE.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    #[cfg(unix)]
    if let Ok(p) = socket_path() {
        let _ = std::fs::remove_file(p);
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::{ConnectOptions, GenericNamespaced};
        use std::time::{Duration, Instant};
        if STOPPED.0.lock().unwrap().is_none() {
            return; // not listening
        }
        STOPPING.store(true, std::sync::atomic::Ordering::SeqCst);
        // The accept thread is blocked in ConnectNamedPipe: connect to wake it, and stay
        // connected until it has gone (a client that hangs up before the accept is
        // dropped and waited for again).
        let wake = pipe_name().to_ns_name::<GenericNamespaced>().ok().and_then(|name| {
            ConnectOptions::new().name(name).wait_mode(interprocess::ConnectWaitMode::Timeout(Duration::from_millis(500))).connect_sync().ok()
        });
        // bounded: the accept thread may be stuck handing over a connection
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut stopped = STOPPED.0.lock().unwrap();
        while *stopped != Some(true) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            stopped = STOPPED.1.wait_timeout(stopped, left).unwrap().0;
        }
        drop(stopped);
        drop(wake);
    }
}
