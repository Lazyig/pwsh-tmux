//! Cancellable nonblocking I/O. A silent or stalled peer must not keep helper
//! threads and queued output alive indefinitely.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

pub const MAX_CLIENTS: usize = 64;
pub const MAX_CLIENT_FRAME: usize = 1 << 20;
pub const MAX_OUTPUT_BACKLOG: usize = 8 << 20;
pub const IO_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Io<'a> {
    pub stream: &'a Stream,
    pub closed: &'a AtomicBool,
    pub deadline: Option<Instant>,
}

impl Io<'_> {
    fn retry<T>(&self, mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
        loop {
            if self.closed.load(Ordering::SeqCst) {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            if self.deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match operation() {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                result => return result,
            }
        }
    }
}

impl Read for Io<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            self.retry(|| (&*self.stream).read(buf))
        }
        #[cfg(windows)]
        self.retry(|| {
            use std::os::windows::io::{AsHandle, AsRawHandle};
            use windows_sys::Win32::Storage::FileSystem::ReadFile;
            let Stream::NamedPipe(pipe) = self.stream;
            let mut read = 0;
            let ok = unsafe {
                ReadFile(
                    pipe.inner().as_handle().as_raw_handle(),
                    buf.as_mut_ptr(),
                    buf.len().min(u32::MAX as usize) as u32,
                    &mut read,
                    std::ptr::null_mut(),
                )
            };
            if ok != 0 {
                return Ok(read as usize);
            }
            let error = io::Error::last_os_error();
            // The library maps ERROR_NO_DATA to EOF. For PIPE_NOWAIT it means
            // the pipe is still connected but currently has no bytes to read.
            if error.raw_os_error() == Some(232) {
                Err(io::ErrorKind::WouldBlock.into())
            } else {
                Err(error)
            }
        })
    }
}

impl Write for Io<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
        {
            self.retry(|| (&*self.stream).write(&buf[..buf.len().min(16384)]))
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            use std::os::fd::AsRawFd;
            let Stream::UdSocket(socket) = self.stream;
            self.retry(|| {
                // Some older kernels can block a large send despite O_NONBLOCK.
                // Bound each syscall and request nonblocking operation explicitly.
                let written = unsafe { libc::send(socket.inner().as_raw_fd(), buf.as_ptr().cast(),
                    buf.len().min(16384), libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL) };
                if written < 0 { Err(io::Error::last_os_error()) } else { Ok(written as usize) }
            })
        }
        #[cfg(windows)]
        self.retry(|| {
            use std::os::windows::io::{AsHandle, AsRawHandle};
            use windows_sys::Win32::Storage::FileSystem::WriteFile;
            let Stream::NamedPipe(pipe) = self.stream;
            let mut written = 0;
            let ok = unsafe {
                WriteFile(
                    pipe.inner().as_handle().as_raw_handle(),
                    buf.as_ptr(),
                    buf.len().min(u32::MAX as usize) as u32,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };
            if ok != 0 {
                return if written == 0 && !buf.is_empty() {
                    Err(io::ErrorKind::WouldBlock.into())
                } else {
                    Ok(written as usize)
                };
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(232) {
                Err(io::ErrorKind::WouldBlock.into())
            } else {
                Err(error)
            }
        })
    }
    fn flush(&mut self) -> io::Result<()> {
        // This is a byte stream, with no userspace output buffer. In particular,
        // do not wait for a Windows peer to consume the pipe via FlushFileBuffers.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interprocess::local_socket::ListenerOptions;

    fn pair() -> (Stream, Stream) {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let label = format!(
            "pwsh-tmux-io-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        );
        #[cfg(windows)]
        let name = label
            .to_ns_name::<interprocess::local_socket::GenericNamespaced>()
            .unwrap();
        #[cfg(unix)]
        let path = std::env::temp_dir().join(label);
        #[cfg(unix)]
        let name = path
            .as_path()
            .to_fs_name::<interprocess::local_socket::GenericFilePath>()
            .unwrap().into_owned();
        let listener = ListenerOptions::new()
            .name(name.clone())
            .create_sync()
            .unwrap();
        let peer = thread::spawn(move || Stream::connect(name).unwrap());
        let server = listener.accept().unwrap();
        let peer = peer.join().unwrap();
        server.set_nonblocking(true).unwrap();
        (server, peer)
    }

    #[test]
    fn silent_peer_times_out_and_cancelled_reads_exit() {
        let (server, _peer) = pair();
        let closed = AtomicBool::new(false);
        let mut io = Io {
            stream: &server,
            closed: &closed,
            deadline: Some(Instant::now() + Duration::from_millis(30)),
        };
        assert_eq!(
            io.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        closed.store(true, Ordering::SeqCst);
        io.deadline = None;
        assert_eq!(
            io.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn stalled_writer_times_out_instead_of_holding_connection() {
        let (server, _peer) = pair();
        let started = Instant::now();
        let closed = AtomicBool::new(false);
        let mut io = Io {
            stream: &server,
            closed: &closed,
            deadline: Some(Instant::now() + Duration::from_millis(30)),
        };
        assert_eq!(
            io.write_all(&vec![0; MAX_OUTPUT_BACKLOG])
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(1), "write syscall bypassed the deadline");
    }

    #[test]
    fn invalid_first_frames_are_disconnected_before_reading_body() {
        for (kind, len) in [(C_IDENTIFY, MAX_CLIENT_FRAME + 1), (C_INPUT, 32)] {
            let (server, mut peer) = pair();
            let (tx, events) = mpsc::sync_channel(64);
            let mut srv = Server::new(tx);
            srv.accept(server);
            let mut header = vec![kind];
            header.extend_from_slice(&(len as u32).to_le_bytes());
            peer.write_all(&header).unwrap();
            // Deliberately send no payload. Waiting for a body would time out
            // here; an invalid header must close the connection immediately.
            loop {
                let event = events.recv_timeout(Duration::from_secs(2)).expect("invalid frame disconnect");
                assert!(!matches!(event, Event::ClientMsg(_, _, _)));
                let gone = matches!(event, Event::ClientGone(_));
                srv.handle(event);
                if gone { break; }
            }
            assert!(srv.clients.is_empty());
        }
    }

    #[test]
    fn unauthorized_os_peer_gets_refusal_before_disconnect() {
        // root/SYSTEM is deliberately trusted by the server policy.
        if ipc::is_superuser(&ipc::own_user_id()) { return; }
        let (server, mut peer) = pair();
        let (tx, events) = mpsc::sync_channel(64);
        let mut srv = Server::new(tx);
        srv.owner = "a-different-server-owner".into();
        srv.accept(server);
        write_frame(&mut peer, C_IDENTIFY, b"").unwrap();
        let (kind, payload) = read_frame(&mut peer).expect("access refusal");
        assert_eq!(kind, S_REPLY);
        assert!(String::from_utf8_lossy(&payload).contains("access not allowed"));
        let event = events.recv_timeout(Duration::from_secs(2)).expect("refused peer disconnect");
        assert!(matches!(event, Event::ClientGone(_)));
        srv.handle(event);
        assert!(srv.clients.is_empty());
    }
}
