//! Client-side terminal handling: raw VT input/output and size.

use std::io;

pub fn size() -> (u16, u16) {
    #[cfg(unix)]
    {
        // ioctl only: crossterm falls back to running `tput cols` and `tput lines`
        // when there is no terminal, which cost two process spawns per command.
        for fd in [1, 0, 2] {
            let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
            if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
                return (ws.ws_col, ws.ws_row);
            }
        }
        (80, 24)
    }
    #[cfg(windows)]
    match crossterm::terminal::size() {
        Ok((c, r)) if c > 0 && r > 0 => (c, r),
        _ => (80, 24),
    }
}

/// Puts the controlling terminal into raw mode with VT input; restores on drop.
pub struct RawMode {
    #[cfg(windows)]
    saved: (u32, u32, u32, u32),
}

#[cfg(windows)]
mod win {
    pub use windows_sys::Win32::Foundation::HANDLE;
    pub use windows_sys::Win32::System::Console::*;
    pub fn stdin() -> HANDLE {
        unsafe { GetStdHandle(STD_INPUT_HANDLE) }
    }
    pub fn stdout() -> HANDLE {
        unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }
    }
}

impl RawMode {
    pub fn enter() -> io::Result<RawMode> {
        #[cfg(unix)]
        {
            crossterm::terminal::enable_raw_mode()?;
            Ok(RawMode {})
        }
        #[cfg(windows)]
        unsafe {
            use win::*;
            let (hin, hout) = (stdin(), stdout());
            let (mut in_mode, mut out_mode) = (0u32, 0u32);
            if GetConsoleMode(hin, &mut in_mode) == 0 || GetConsoleMode(hout, &mut out_mode) == 0 {
                return Err(io::Error::new(io::ErrorKind::Unsupported, "not a console (stdin/stdout redirected?)"));
            }
            let saved = (in_mode, out_mode, GetConsoleCP(), GetConsoleOutputCP());
            let raw_in = (in_mode & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT | ENABLE_MOUSE_INPUT))
                | ENABLE_VIRTUAL_TERMINAL_INPUT;
            if SetConsoleMode(hin, raw_in) == 0 {
                return Err(io::Error::last_os_error());
            }
            let raw_out = out_mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING | ENABLE_PROCESSED_OUTPUT | DISABLE_NEWLINE_AUTO_RETURN;
            if SetConsoleMode(hout, raw_out) == 0 {
                SetConsoleMode(hin, in_mode);
                return Err(io::Error::last_os_error());
            }
            SetConsoleCP(65001);
            SetConsoleOutputCP(65001);
            Ok(RawMode { saved })
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = crossterm::terminal::disable_raw_mode();
        }
        #[cfg(windows)]
        unsafe {
            use win::*;
            let (i, o, icp, ocp) = self.saved;
            SetConsoleMode(stdin(), i);
            SetConsoleMode(stdout(), o);
            SetConsoleCP(icp);
            SetConsoleOutputCP(ocp);
        }
    }
}

/// Set while the terminal was last asked for focus reports (Enfcs): a console makes focus
/// records on every focus change, but tmux's terminal is silent after Dsfcs.
pub static FOCUS_REPORTS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Notes the last focus reporting mode set in output written to the terminal.
pub fn note_focus_mode(out: &[u8]) {
    let last = |pat: &[u8]| out.windows(pat.len()).rposition(|w| w == pat);
    let (on, off) = (last(b"\x1b[?1004h"), last(b"\x1b[?1004l"));
    if on.is_some() || off.is_some() {
        FOCUS_REPORTS.store(on > off, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A self-pipe: a nonblocking, close-on-exec pipe whose reading end a thread waits on.
#[cfg(unix)]
struct Pipe {
    r: libc::c_int,
    w: libc::c_int,
}

#[cfg(unix)]
impl Pipe {
    fn new() -> Option<Pipe> {
        let mut fds = [0; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return None;
        }
        for fd in fds {
            unsafe {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
            }
        }
        Some(Pipe { r: fds[0], w: fds[1] })
    }

    /// Async-signal-safe; a full pipe already has a wakeup pending.
    fn notify(&self) {
        unsafe { libc::write(self.w, b"x".as_ptr().cast(), 1) };
    }

    fn drain(&self) {
        let mut buf = [0u8; 64];
        while unsafe { libc::read(self.r, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
    }

    /// Blocks until the pipe has been written to (or a signal interrupts the wait, or
    /// `timeout_ms` passes unless it is -1), then empties it.
    fn wait(&self, timeout_ms: libc::c_int) {
        let mut fds = libc::pollfd { fd: self.r, events: libc::POLLIN, revents: 0 };
        unsafe { libc::poll(&mut fds, 1, timeout_ms) };
        self.drain();
    }
}

#[cfg(unix)]
impl Drop for Pipe {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.r);
            libc::close(self.w);
        }
    }
}

/// Set while a lock command or a suspended shell owns the terminal, so the input thread
/// leaves stdin alone. On Unix changing it wakes the thread at once through a pipe the
/// thread waits on beside stdin; on Windows (or without the pipe) the thread looks at it
/// between waits of at most 100 ms.
pub struct Pause {
    paused: std::sync::atomic::AtomicBool,
    #[cfg(unix)]
    wake: Option<Pipe>,
}

impl Pause {
    pub fn new() -> Pause {
        Pause {
            paused: std::sync::atomic::AtomicBool::new(false),
            #[cfg(unix)]
            wake: Pipe::new(),
        }
    }

    pub fn set(&self, paused: bool) {
        self.paused.store(paused, std::sync::atomic::Ordering::SeqCst);
        #[cfg(unix)]
        if let Some(wake) = &self.wake {
            wake.notify();
        }
    }

    pub fn is_set(&self) -> bool {
        self.paused.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Waits for the next `set` while paused; the caller looks at the flag again.
    pub fn wait(&self) {
        #[cfg(unix)]
        if let Some(wake) = &self.wake {
            wake.wait(-1);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(unix)]
static WINCH: std::sync::OnceLock<Option<Pipe>> = std::sync::OnceLock::new();
#[cfg(unix)]
static WINCH_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// This thread's errno, which the signal handler keeps for the code it interrupted.
#[cfg(unix)]
fn errno_location() -> *mut libc::c_int {
    #[cfg(any(target_os = "linux", target_os = "redox"))]
    return unsafe { libc::__errno_location() };
    #[cfg(any(target_os = "android", target_os = "netbsd", target_os = "openbsd"))]
    return unsafe { libc::__errno() };
    #[cfg(any(target_vendor = "apple", target_os = "freebsd", target_os = "dragonfly"))]
    return unsafe { libc::__error() };
    #[cfg(any(target_os = "solaris", target_os = "illumos"))]
    return unsafe { libc::___errno() };
    #[allow(unreachable_code)]
    std::ptr::null_mut()
}

#[cfg(unix)]
extern "C" fn on_winch(_: libc::c_int) {
    let fd = WINCH_FD.load(std::sync::atomic::Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    let errno = errno_location();
    let saved = if errno.is_null() { 0 } else { unsafe { *errno } };
    unsafe { libc::write(fd, b"x".as_ptr().cast(), 1) };
    if !errno.is_null() {
        unsafe { *errno = saved };
    }
}

/// Wakes the resize thread when the terminal may have changed size. On Unix that is
/// SIGWINCH, which a handler (SA_RESTART, so other threads' blocking calls carry on)
/// passes on through a pipe; the handler is reset by exec and the signal mask is left
/// alone, so commands the client runs get SIGWINCH as before. On Windows (or when the
/// handler can't be set up), and while a command owns the terminal (one that takes its
/// foreground keeps SIGWINCH from us), the thread looks every 100 ms.
#[derive(Clone, Copy)]
pub struct SizeWatch {
    #[cfg(unix)]
    pipe: Option<&'static Pipe>,
}

impl SizeWatch {
    /// Sets up the watch; the first `wait` returns at once, so the size is compared
    /// straight away like the polling's first look.
    pub fn install() -> SizeWatch {
        #[cfg(unix)]
        {
            let pipe = WINCH
                .get_or_init(|| {
                    let pipe = Pipe::new()?;
                    WINCH_FD.store(pipe.w, std::sync::atomic::Ordering::Relaxed);
                    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
                    sa.sa_sigaction = on_winch as extern "C" fn(libc::c_int) as libc::sighandler_t;
                    sa.sa_flags = libc::SA_RESTART;
                    unsafe { libc::sigemptyset(&mut sa.sa_mask) };
                    if unsafe { libc::sigaction(libc::SIGWINCH, &sa, std::ptr::null_mut()) } != 0 {
                        WINCH_FD.store(-1, std::sync::atomic::Ordering::Relaxed);
                        return None;
                    }
                    Some(pipe)
                })
                .as_ref();
            let watch = SizeWatch { pipe };
            watch.nudge();
            watch
        }
        #[cfg(windows)]
        SizeWatch {}
    }

    /// Makes the next `wait` return.
    pub fn nudge(&self) {
        #[cfg(unix)]
        if let Some(pipe) = self.pipe {
            pipe.notify();
        }
    }

    /// Waits for a possible size change; `poll` (a command owns the terminal) limits the
    /// wait to 100 ms.
    #[cfg_attr(windows, allow(unused_variables))]
    pub fn wait(&self, poll: bool) {
        #[cfg(unix)]
        if let Some(pipe) = self.pipe {
            pipe.wait(if poll { 100 } else { -1 });
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Blocking reader of raw input bytes (UTF-8 / VT sequences).
#[derive(Default)]
pub struct Input {
    #[cfg(windows)]
    high_surrogate: Option<u16>,
    /// Focus reports made from console focus records, returned by the next read.
    #[cfg(windows)]
    pending: Vec<u8>,
}

impl Input {
    /// Waits for typed input; Ok(false) when `pause` changed (or on a timeout), Err when the
    /// terminal is gone. Lets the reader thread stay out of `read` while a lock command or
    /// suspended shell owns the terminal. On Unix with the pause pipe there is no timeout;
    /// otherwise the wait is at most 100 ms.
    #[cfg_attr(windows, allow(unused_variables))]
    pub fn wait(&mut self, pause: &Pause) -> io::Result<bool> {
        #[cfg(unix)]
        {
            let wake = pause.wake.as_ref();
            let mut fds = [
                libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: wake.map_or(-1, |w| w.r), events: libc::POLLIN, revents: 0 },
            ];
            let timeout = if wake.is_some() { -1 } else { 100 };
            match unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) } {
                // the caller looks at the pause again before reading
                n if n > 0 && fds[1].revents != 0 => {
                    if let Some(w) = wake {
                        w.drain();
                    }
                    Ok(false)
                }
                // POLLHUP/POLLERR also count as ready: the following read reports them
                n if n > 0 => Ok(true),
                0 => Ok(false),
                _ => {
                    let e = io::Error::last_os_error();
                    if e.kind() == io::ErrorKind::Interrupted { Ok(false) } else { Err(e) }
                }
            }
        }
        #[cfg(windows)]
        unsafe {
            if !self.pending.is_empty() {
                return Ok(true);
            }
            use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
            use windows_sys::Win32::System::Threading::WaitForSingleObject;
            match WaitForSingleObject(win::stdin(), 100) {
                0 => {}
                WAIT_TIMEOUT => return Ok(false),
                // the console went away (terminal closed)
                _ => return Err(io::Error::last_os_error()),
            }
            // Drop non-key records (resize, menu) so a later read can't block on them. A
            // console that does not pass the terminal's focus reports on as VT input (conhost
            // makes FOCUS_EVENT records of them) gets them back as CSI I / CSI O, but only
            // while focus reports are asked for: a console makes the records regardless.
            loop {
                let mut rec: win::INPUT_RECORD = std::mem::zeroed();
                let mut n = 0u32;
                if win::PeekConsoleInputW(win::stdin(), &mut rec, 1, &mut n) == 0 {
                    return Err(io::Error::last_os_error());
                }
                if n == 0 {
                    return Ok(false);
                }
                if rec.EventType as u32 == win::KEY_EVENT as u32 && rec.Event.KeyEvent.bKeyDown != 0 {
                    return Ok(true);
                }
                if win::ReadConsoleInputW(win::stdin(), &mut rec, 1, &mut n) == 0 {
                    return Err(io::Error::last_os_error());
                }
                if n == 1 && rec.EventType as u32 == win::FOCUS_EVENT as u32 && FOCUS_REPORTS.load(std::sync::atomic::Ordering::SeqCst) {
                    self.pending.extend_from_slice(if rec.Event.FocusEvent.bSetFocus != 0 { b"\x1b[I" } else { b"\x1b[O" });
                    return Ok(true);
                }
            }
        }
    }

    /// Returns an empty vec on EOF.
    pub fn read(&mut self) -> io::Result<Vec<u8>> {
        #[cfg(unix)]
        {
            let mut buf = vec![0u8; 8192];
            loop {
                let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
                if n >= 0 {
                    buf.truncate(n as usize);
                    return Ok(buf);
                }
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::Interrupted {
                    return Err(e);
                }
            }
        }
        #[cfg(windows)]
        {
            if !self.pending.is_empty() {
                return Ok(std::mem::take(&mut self.pending));
            }
            // Raw ReadConsoleW rather than std's stdin, which treats ^Z as EOF and
            // ReadFile, which mangles non-ASCII input on older consoles.
            let mut wbuf = [0u16; 4096];
            loop {
                let mut n = 0u32;
                let ok = unsafe {
                    win::ReadConsoleW(win::stdin(), wbuf.as_mut_ptr().cast(), wbuf.len() as u32, &mut n, std::ptr::null())
                };
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut units: Vec<u16> = self.high_surrogate.take().into_iter().collect();
                units.extend_from_slice(&wbuf[..n as usize]);
                if let Some(&last) = units.last() {
                    if (0xD800..0xDC00).contains(&last) {
                        self.high_surrogate = units.pop();
                    }
                }
                if units.is_empty() {
                    continue;
                }
                return Ok(String::from_utf16_lossy(&units).into_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn focus_mode_follows_the_last_enfcs_or_dsfcs() {
        note_focus_mode(b"\x1b[?1004l\x1b[2J\x1b[?1004h");
        assert!(FOCUS_REPORTS.load(Ordering::SeqCst));
        // output without either keeps the mode
        note_focus_mode(b"\x1b[Hhello");
        assert!(FOCUS_REPORTS.load(Ordering::SeqCst));
        note_focus_mode(b"\x1b[?1004h\x1b[?1004l");
        assert!(!FOCUS_REPORTS.load(Ordering::SeqCst));
    }

    #[test]
    fn pause_wakes_a_waiting_reader() {
        let pause = std::sync::Arc::new(Pause::new());
        pause.set(true);
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = pause.clone();
        std::thread::spawn(move || {
            while waiter.is_set() {
                waiter.wait();
            }
            tx.send(()).unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        pause.set(false);
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn size_watch_wakes_on_sigwinch() {
        let watch = SizeWatch::install();
        assert!(watch.pipe.is_some());
        // the first wait returns at once
        watch.wait(false);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            watch.wait(false);
            tx.send(()).unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(rx.try_recv().is_err());
        // only this thread takes the signal
        unsafe { *errno_location() = libc::EAGAIN };
        unsafe { libc::pthread_kill(libc::pthread_self(), libc::SIGWINCH) };
        assert_eq!(unsafe { *errno_location() }, libc::EAGAIN);
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    }
}
