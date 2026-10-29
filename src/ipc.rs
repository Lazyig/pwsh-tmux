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
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
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

pub fn connect() -> io::Result<Stream> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::GenericFilePath;
        let path = socket_path()?;
        Stream::connect(path.as_path().to_fs_name::<GenericFilePath>()?)
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::GenericNamespaced;
        Stream::connect(pipe_name().to_ns_name::<GenericNamespaced>()?)
    }
}

/// Caller must have verified that no server answers on the endpoint.
pub fn listen() -> io::Result<Listener> {
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
        ListenerOptions::new()
            .name(pipe_name().to_ns_name::<GenericNamespaced>()?)
            .create_sync()
    }
}

pub fn cleanup() {
    #[cfg(unix)]
    if let Ok(p) = socket_path() {
        let _ = std::fs::remove_file(p);
    }
}
