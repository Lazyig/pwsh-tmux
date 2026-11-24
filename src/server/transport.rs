//! Cancellable nonblocking I/O. A silent or stalled peer must not keep helper
//! threads and queued output alive indefinitely.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

pub const MAX_CLIENTS: usize = 64;
pub const MAX_CLIENT_FRAME: usize = crate::protocol::MAX_CLIENT_FRAME;
pub const MAX_OUTPUT_BACKLOG: usize = 8 << 20;
/// What holding a queued frame takes besides its bytes: its allocation's header and
/// rounding and its slot in the writer's channel (about 60 bytes measured for a bell).
pub const FRAME_OVERHEAD: usize = 64;

/// What a queued frame of `payload` bytes counts against MAX_OUTPUT_BACKLOG: its bytes
/// (with the 5-byte header) and FRAME_OVERHEAD, so the limit bounds memory for small
/// frames too.
pub fn frame_cost(payload: usize) -> usize {
    payload.saturating_add(5 + FRAME_OVERHEAD)
}
pub const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// How often a wait that nothing can wake looks at the cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(5);
/// How often a write to a peer that has not read for a while looks again, where nothing
/// can wake it (a paused or stopped terminal client has no write deadline).
#[cfg(not(unix))]
const STALLED_POLL: Duration = Duration::from_millis(50);
/// What the kernel may hold for an attached terminal client (SO_SNDBUF; Linux doubles
/// it). Frames in the socket buffer are out of reach of the deferred redraw and of
/// blocking (block.rs): a slow terminal would draw the default ~200 KiB of old frames
/// after its panes had stopped, where tmux writes to the terminal itself.
#[cfg(unix)]
const TERMINAL_SEND_BUFFER: libc::c_int = 4096;

/// Lowers the send buffer of an attached terminal client's connection (see
/// TERMINAL_SEND_BUFFER). Best effort: the default buffer only makes the tail longer.
pub fn shrink_send_buffer(stream: &Stream) {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let Stream::UdSocket(socket) = stream;
        let size = TERMINAL_SEND_BUFFER;
        unsafe {
            libc::setsockopt(socket.inner().as_raw_fd(), libc::SOL_SOCKET, libc::SO_SNDBUF,
                (&size as *const libc::c_int).cast(), std::mem::size_of::<libc::c_int>() as libc::socklen_t);
        }
    }
    #[cfg(not(unix))]
    let _ = stream;
}

/// A connection's cancel flag. On Unix, `close` also wakes a reader or writer waiting
/// for the socket, which then sees the flag at once. Without a wake pipe (Windows, or
/// one could not be made) waits look at the flag every 5 ms.
#[derive(Default)]
pub struct Closer {
    closed: AtomicBool,
    /// Readable once the connection is closed. The byte is never read.
    #[cfg(unix)]
    wake: Option<(std::io::PipeReader, std::io::PipeWriter)>,
}

impl Closer {
    pub fn new() -> Self {
        Self {
            closed: AtomicBool::new(false),
            #[cfg(unix)]
            wake: std::io::pipe().ok(),
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn close(&self) {
        #[cfg(unix)]
        if !self.closed.swap(true, Ordering::SeqCst) {
            if let Some((_, w)) = &self.wake {
                // only the first close writes, so this never fills the pipe
                let _ = (&*w).write(&[1]);
            }
        }
        #[cfg(not(unix))]
        self.closed.store(true, Ordering::SeqCst);
    }
}

pub struct Io<'a> {
    pub stream: &'a Stream,
    pub closed: &'a Closer,
    pub deadline: Option<Instant>,
}

impl Io<'_> {
    /// Runs `operation` until it does not report WouldBlock, the connection is closed or
    /// the deadline passes. `events` is what to wait for between tries (POLLIN, POLLOUT).
    fn retry<T>(&self, events: i16, mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
        #[cfg(not(unix))]
        let since = Instant::now();
        loop {
            if self.closed.is_closed() {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            if self.deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match operation() {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    #[cfg(unix)]
                    self.wait(events);
                    // a write stalled for a second (a stopped client, a console selection)
                    // looks less often; a read stays quick for the next key
                    #[cfg(not(unix))]
                    thread::sleep(if events == WRITE && since.elapsed() >= Duration::from_secs(1) { STALLED_POLL } else { CANCEL_POLL });
                }
                result => return result,
            }
        }
    }

    /// Waits until the socket may be ready for `events`, the connection is closed or the
    /// deadline comes. A wake-up for any other reason only means one more try.
    #[cfg(unix)]
    fn wait(&self, events: i16) {
        use std::os::fd::AsRawFd;
        let Stream::UdSocket(socket) = self.stream;
        let mut limit = self.deadline.map(|d| d.saturating_duration_since(Instant::now()));
        if self.closed.wake.is_none() {
            limit = Some(limit.map_or(CANCEL_POLL, |l| l.min(CANCEL_POLL)));
        }
        // rounded up, so the deadline has passed when a timed-out poll returns
        let timeout = limit.map_or(-1, |l| l.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32);
        let mut fds = [libc::pollfd { fd: socket.inner().as_raw_fd(), events, revents: 0 }; 2];
        let mut count = 1;
        if let Some((r, _)) = &self.closed.wake {
            fds[1] = libc::pollfd { fd: r.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            count = 2;
        }
        // An error (EINTR) is a wake-up like any other.
        unsafe { libc::poll(fds.as_mut_ptr(), count, timeout) };
    }
}

#[cfg(unix)]
const READ: i16 = libc::POLLIN;
#[cfg(unix)]
const WRITE: i16 = libc::POLLOUT;
#[cfg(not(unix))]
const READ: i16 = 0;
#[cfg(not(unix))]
const WRITE: i16 = 1;

impl Read for Io<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            self.retry(READ, || (&*self.stream).read(buf))
        }
        #[cfg(windows)]
        self.retry(READ, || {
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
            self.retry(WRITE, || (&*self.stream).write(&buf[..buf.len().min(16384)]))
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            use std::os::fd::AsRawFd;
            let Stream::UdSocket(socket) = self.stream;
            self.retry(WRITE, || {
                // Some older kernels can block a large send despite O_NONBLOCK.
                // Bound each syscall and request nonblocking operation explicitly.
                let written = unsafe { libc::send(socket.inner().as_raw_fd(), buf.as_ptr().cast(),
                    buf.len().min(16384), libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL) };
                if written < 0 { Err(io::Error::last_os_error()) } else { Ok(written as usize) }
            })
        }
        #[cfg(windows)]
        self.retry(WRITE, || {
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
        // with a wake pipe, and without one (5 ms checks of the flag)
        for closed in [Closer::new(), Closer::default()] {
            let (server, _peer) = pair();
            let mut io = Io {
                stream: &server,
                closed: &closed,
                deadline: Some(Instant::now() + Duration::from_millis(30)),
            };
            assert_eq!(
                io.read(&mut [0]).unwrap_err().kind(),
                io::ErrorKind::TimedOut
            );
            closed.close();
            io.deadline = None;
            assert_eq!(
                io.read(&mut [0]).unwrap_err().kind(),
                io::ErrorKind::BrokenPipe
            );
        }
    }

    #[test]
    fn stalled_writer_times_out_instead_of_holding_connection() {
        for closed in [Closer::new(), Closer::default()] {
            let (server, _peer) = pair();
            let started = Instant::now();
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
    }

    /// Closing a connection that is waiting to read (no deadline) or to write (a peer that
    /// reads nothing) ends the wait at once.
    #[test]
    fn close_ends_waiting_reads_and_writes_promptly() {
        for write in [false, true] {
            for closed in [Closer::new(), Closer::default()] {
                let (server, _peer) = pair();
                let closed = Arc::new(closed);
                let (started_tx, started) = mpsc::channel();
                let waiter = {
                    let closed = closed.clone();
                    thread::spawn(move || {
                        let mut io = Io { stream: &server, closed: &closed, deadline: None };
                        started_tx.send(()).unwrap();
                        let result = if write {
                            io.write_all(&vec![0; MAX_OUTPUT_BACKLOG])
                        } else {
                            io.read(&mut [0]).map(drop)
                        };
                        (result.unwrap_err().kind(), Instant::now())
                    })
                };
                started.recv().unwrap();
                // let it fill the socket and start waiting
                thread::sleep(Duration::from_millis(30));
                assert!(!waiter.is_finished());
                let at = Instant::now();
                closed.close();
                let (kind, ended) = waiter.join().unwrap();
                assert_eq!(kind, io::ErrorKind::BrokenPipe);
                let took = ended.saturating_duration_since(at);
                // generous so a loaded machine does not fail it; a lost wake-up never ends
                assert!(took < Duration::from_millis(500), "write {write}: close took {took:?}");
            }
        }
    }

    /// A connection that sends nothing ends as soon as the server closes it.
    #[test]
    fn closing_an_idle_client_ends_its_reader_promptly() {
        let (server, mut peer) = pair();
        let (tx, events) = mpsc::sync_channel(64);
        let mut srv = Server::new(tx);
        srv.accept(server);
        write_frame(&mut peer, C_IDENTIFY, b"").unwrap();
        let cid = *srv.clients.keys().next().unwrap();
        // the identify frame arrives; after that the reader waits with no deadline
        loop {
            let event = events.recv_timeout(Duration::from_secs(2)).expect("identify");
            if matches!(event, Event::ClientMsg(..)) { break; }
            assert!(!matches!(event, Event::ClientGone(_)), "refused");
        }
        thread::sleep(Duration::from_millis(30));
        let at = Instant::now();
        srv.clients[&cid].closed.close();
        loop {
            let event = events.recv_timeout(Duration::from_secs(2)).expect("reader end");
            if matches!(event, Event::ClientGone(c) if c == cid) { break; }
        }
        let took = at.elapsed();
        assert!(took < Duration::from_millis(500), "reader ended after {took:?}");
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
