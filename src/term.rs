//! Client-side terminal handling: raw VT input/output and size.

use std::io;

pub fn size() -> (u16, u16) {
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

/// Blocking reader of raw input bytes (UTF-8 / VT sequences).
#[derive(Default)]
pub struct Input {
    #[cfg(windows)]
    high_surrogate: Option<u16>,
}

impl Input {
    /// Waits up to `ms` for typed input; Ok(false) on timeout, Err when the terminal is gone. Lets the reader thread stay
    /// out of `read` while a lock command or suspended shell owns the terminal.
    pub fn wait(&self, ms: u32) -> io::Result<bool> {
        #[cfg(unix)]
        {
            let mut fds = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
            match unsafe { libc::poll(&mut fds, 1, ms as i32) } {
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
            use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
            use windows_sys::Win32::System::Threading::WaitForSingleObject;
            match WaitForSingleObject(win::stdin(), ms) {
                0 => {}
                WAIT_TIMEOUT => return Ok(false),
                // the console went away (terminal closed)
                _ => return Err(io::Error::last_os_error()),
            }
            // Drop non-key records (focus, resize, menu) so a later read can't block on them.
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
