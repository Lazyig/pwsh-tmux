//! Pane output flow control.
//!
//! A pane's reader keeps at most [`CREDITS`] chunks queued but not yet handled by the main
//! loop. Reading ahead without a limit fills the shared event queue, and client input
//! then waits behind a queue of output. A reader waiting for a credit holds the chunk it
//! has read, as a reader blocked on a full queue would, and the main loop sees that (and
//! output waiting in a Unix terminal for the reader) in the [`Backlog`]: a burst of
//! output is still rendered once per burst.
//!
//! A pane's exit is reported after the output it wrote before exiting has been handled
//! (tmux's window_pane_destroy_ready), so its last output is always on the screen.

use super::{Event, PaneId};
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Chunks (up to 64 KiB each) a pane may have queued to the main loop.
const CREDITS: usize = 4;

/// How long [`Flow::settle`] waits for a pane's output once its process has exited.
#[derive(Clone, Copy)]
struct Timing {
    /// The reader must have waited in a read, with nothing pending, for this long.
    quiet: Duration,
    /// Give up when nothing has been read or handled for this long...
    stall: Duration,
    /// ...or after this long in all (a background process may keep writing).
    cap: Duration,
}

const TIMING: Timing = Timing {
    // Unix sees the bytes the kernel holds (FIONREAD): this only has to cover a reader
    // that has just returned from its read. ConPTY's pipe cannot be peeked (see
    // Pending), and conhost renders the last output a little after the program exits.
    quiet: Duration::from_millis(if cfg!(windows) { 50 } else { 2 }),
    stall: Duration::from_millis(200),
    cap: Duration::from_secs(1),
};

#[derive(Default)]
struct State {
    /// Chunks read and not yet handled by the main loop, the one the reader holds included.
    in_flight: usize,
    /// The reader waits for output: in a poll with nothing to read (Unix), or in a read
    /// or about to start one.
    reading: bool,
    /// Counts the reader's reads that returned.
    reads: u64,
    /// A read returned end of file or an error.
    eof: bool,
    /// The pane is gone: the reader discards what it reads.
    closed: bool,
    /// Threads waiting for a change: a change wakes them (only then, as a wake-up is a
    /// system call and there is one per chunk otherwise).
    waiting: usize,
}

/// What a pane's reader, its child waiter and the main loop share.
#[derive(Default)]
pub struct Flow {
    state: Mutex<State>,
    changed: Condvar,
}

impl Flow {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn update(&self, f: impl FnOnce(&mut State)) {
        let mut s = self.lock();
        f(&mut s);
        if s.waiting > 0 {
            self.changed.notify_all();
        }
    }

    fn wait<'a>(&self, mut s: MutexGuard<'a, State>, timeout: Option<Duration>) -> MutexGuard<'a, State> {
        s.waiting += 1;
        let mut s = match timeout {
            None => self.changed.wait(s).unwrap_or_else(|e| e.into_inner()),
            Some(t) => self.changed.wait_timeout(s, t).unwrap_or_else(|e| e.into_inner()).0,
        };
        s.waiting -= 1;
        s
    }

    /// Marks the reader as reading. False once the pane is gone.
    fn begin_read(&self) -> bool {
        let mut s = self.lock();
        s.reading = !s.closed;
        !s.closed
    }

    #[cfg(unix)]
    fn set_reading(&self, reading: bool) {
        self.lock().reading = reading;
    }

    /// Waits, holding a chunk just read, until [`CREDITS`] or fewer chunks are queued
    /// before it. False once the pane is gone.
    fn wait_credit(&self) -> bool {
        let mut s = self.lock();
        while s.in_flight > CREDITS && !s.closed {
            s = self.wait(s, None);
        }
        !s.closed
    }

    /// Records the end of a read (`got`: it returned data). The chunk's credit, which
    /// counts it in flight, unless the read ended the output or the pane has gone.
    fn end_read(self: &Arc<Self>, got: bool) -> Option<Credit> {
        let mut credit = None;
        self.update(|s| {
            s.reading = false;
            s.reads += 1;
            s.eof |= !got;
            if got && !s.closed {
                s.in_flight += 1;
                credit = Some(Credit(self.clone()));
            }
        });
        credit
    }

    fn close(&self) {
        self.update(|s| s.closed = true);
    }

    /// Called once the pane's process has exited: waits until the output read so far has
    /// been handled and nothing more is waiting to be read. It does not wait for end of
    /// file, which never comes from ConPTY, nor on Unix while a background process holds
    /// the terminal.
    pub fn settle(&self, pending: &Pending) {
        self.settle_with(|| pending.any(), TIMING);
    }

    fn settle_with(&self, mut pending: impl FnMut() -> bool, t: Timing) {
        let start = Instant::now();
        let mut s = self.lock();
        let mut seen = (s.reads, s.in_flight);
        let mut progress = start;
        // since when (and as of which read) the reader has looked idle
        let mut idle: Option<(Instant, u64)> = None;
        loop {
            if s.closed || (s.eof && s.in_flight == 0) {
                return;
            }
            let now = Instant::now();
            if (s.reads, s.in_flight) != seen {
                seen = (s.reads, s.in_flight);
                progress = now;
            }
            // A read that has just returned is not counted yet: the reader must stay in
            // the same read, with nothing pending, for a while.
            if s.in_flight == 0 && s.reading && !pending() {
                match idle {
                    Some((since, reads)) if reads == s.reads => {
                        if now.duration_since(since) >= t.quiet {
                            return;
                        }
                    }
                    _ => idle = Some((now, s.reads)),
                }
            } else {
                idle = None;
            }
            if now.duration_since(progress) >= t.stall || now.duration_since(start) >= t.cap {
                return;
            }
            // woken by the reader and the main loop; the kernel's bytes are polled
            let wait = idle.map_or(t.quiet, |(since, _)| t.quiet.saturating_sub(now.duration_since(since)));
            s = self.wait(s, Some(wait.max(Duration::from_millis(1))));
        }
    }
}

/// A chunk in flight: dropping it, once the main loop has handled the chunk or thrown
/// it away, gives the credit back.
pub struct Credit(Arc<Flow>);

impl Drop for Credit {
    fn drop(&mut self) {
        self.0.update(|s| s.in_flight -= 1);
    }
}

/// The pane's side of its flow: when it goes (with the pane), the reader stops waiting
/// for credits and drains its terminal.
pub struct Output(Arc<Flow>);

impl Output {
    pub fn new() -> (Self, Arc<Flow>) {
        let flow = Arc::new(Flow::default());
        (Output(flow.clone()), flow)
    }

    pub fn close(&self) {
        self.0.close();
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Counts the panes' readers that hold output read but not yet queued, or (Unix) whose
/// terminal has more for the next read. Output follows soon then: the main loop has
/// handled (or is about to take) the chunks before it, which would otherwise be queued.
#[derive(Clone, Default)]
pub struct Backlog(Arc<AtomicUsize>);

impl Backlog {
    pub fn any(&self) -> bool {
        self.0.load(Ordering::SeqCst) > 0
    }

    fn hold(&self) -> Held<'_> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Held(self)
    }
}

struct Held<'a>(&'a Backlog);

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.0 .0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The pane reader thread: reads the terminal and sends its output to the main loop.
pub fn read_pane(mut reader: Box<dyn Read + Send>, more: Pending, flow: Arc<Flow>, backlog: Backlog, tx: SyncSender<Event>, id: PaneId, generation: u32) {
    let mut buf = vec![0u8; 64 * 1024];
    let mut held = None;
    while flow.begin_read() {
        more.wait_output(&flow, &mut held);
        let n = match reader.read(&mut buf) {
            Ok(n) if n > 0 => n,
            _ => {
                flow.end_read(false);
                return;
            }
        };
        let Some(credit) = flow.end_read(true) else { break };
        // until the chunk is queued (a full queue included)
        let mut h = Some(held.take().unwrap_or_else(|| backlog.hold()));
        if !flow.wait_credit() {
            break;
        }
        // Held on after it is queued, until the next chunk is read, only while the
        // terminal has more (wait_output). Let go before it is queued otherwise: the main
        // loop, woken by the chunk, would find it held and wait for the next chunk that
        // is not coming (next_in_burst) until it looked again.
        let more_now = more.any();
        if !more_now {
            h = None;
        }
        match tx.try_send(Event::PaneData(id, generation, buf[..n].to_vec(), credit)) {
            Ok(()) => {}
            Err(TrySendError::Full(ev)) => {
                // waiting on a full queue holds the chunk
                h.get_or_insert_with(|| backlog.hold());
                if tx.send(ev).is_err() {
                    return;
                }
            }
            Err(TrySendError::Disconnected(_)) => return,
        }
        held = h.filter(|_| more_now);
    }
    drop(held);
    // The pane has gone. Keep reading to the end: a pseudoconsole being closed waits for
    // its output to be read (older Windows), and a Unix process is not left blocked.
    while matches!(reader.read(&mut buf), Ok(n) if n > 0) {}
}

/// Opens a pane's terminal for reading: the reader thread's end and the child waiter's
/// [`Pending`]. On Unix both use one duplicate of the master, as many descriptors as a
/// plain reader.
pub fn open(master: &dyn portable_pty::MasterPty) -> Result<(Box<dyn Read + Send>, Pending), String> {
    #[cfg(unix)]
    if let Some(fd) = master.as_raw_fd() {
        use std::os::fd::BorrowedFd;
        // SAFETY: the master is open for the duration of this call; the message is
        // portable-pty's (try_clone_reader)
        let dup = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned().map_err(|_| format!("dup of fd {fd} failed"))?;
        let file = Arc::new(std::fs::File::from(dup));
        return Ok((Box::new(PtyReader(file.clone())), Pending { fd: Some(file) }));
    }
    let reader = master.try_clone_reader().map_err(|e| e.to_string())?;
    #[cfg(unix)]
    return Ok((reader, Pending { fd: None }));
    // PeekNamedPipe on ConPTY's output pipe would wait for the reader's pending
    // synchronous read (they share a file object), and portable-pty does not expose the
    // handle: Flow::settle goes by the reader alone.
    #[cfg(windows)]
    Ok((reader, Pending {}))
}

#[cfg(unix)]
struct PtyReader(Arc<std::fs::File>);

#[cfg(unix)]
impl Read for PtyReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        (&*self.0).read(buf)
    }
}

/// Whether bytes are waiting in a pane's terminal, for [`Flow::settle`] and the reader.
#[derive(Clone)]
pub struct Pending {
    /// The reader's pty master, for poll and FIONREAD.
    #[cfg(unix)]
    fd: Option<Arc<std::fs::File>>,
}

impl Pending {
    /// Before a read: returns once the terminal has output (or has closed). The reader
    /// counts as waiting for output, for [`Flow::settle`], only while it has none to
    /// read: so a read that took the last bytes is never mistaken for an idle reader. A
    /// `held` chunk is let go when the reader has to wait.
    fn wait_output<'a>(&self, flow: &Flow, held: &mut Option<Held<'a>>) {
        #[cfg(unix)]
        if let Some(fd) = &self.fd {
            let ready = match poll_in(fd, 0) {
                Some(false) => {
                    *held = None;
                    poll_in(fd, -1)
                }
                ready => ready,
            };
            if ready.is_some() {
                flow.set_reading(false);
                return;
            }
        }
        // the read waits, and counts as waiting for output
        let _ = flow;
        *held = None;
    }

    fn any(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let Some(fd) = &self.fd else { return false };
            let mut n: libc::c_int = 0;
            // SAFETY: FIONREAD writes one int
            unsafe { libc::ioctl(fd.as_raw_fd(), libc::FIONREAD, &mut n) == 0 && n > 0 }
        }
        #[cfg(windows)]
        {
            false
        }
    }
}

/// Whether the terminal has output to read (or has closed), waiting up to `timeout`
/// milliseconds (-1: no limit). None if it cannot be polled: the read is left to wait
/// (and to report any error).
#[cfg(unix)]
fn poll_in(fd: &std::fs::File, timeout: libc::c_int) -> Option<bool> {
    use std::os::fd::AsRawFd;
    let mut p = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    loop {
        // SAFETY: one pollfd
        match unsafe { libc::poll(&mut p, 1, timeout) } {
            0 => return Some(false),
            n if n > 0 => return (p.revents & libc::POLLNVAL == 0).then_some(true),
            _ if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted => return None,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    // the quiet time is long against the tests' own sleeps, for a loaded machine
    const SLOW: Timing = Timing { quiet: Duration::from_millis(600), stall: Duration::from_secs(5), cap: Duration::from_secs(10) };

    /// Reads `CREDITS` chunks the main loop has not handled yet, then a fifth.
    fn queue_four_and_read_a_fifth(flow: &Arc<Flow>) -> (Vec<Credit>, Credit) {
        let credits: Vec<_> = (0..CREDITS).map(|_| {
            assert!(flow.begin_read());
            let c = flow.end_read(true).unwrap();
            assert!(flow.wait_credit());
            c
        }).collect();
        assert!(flow.begin_read(), "the reader reads on with four chunks queued");
        (credits, flow.end_read(true).unwrap())
    }

    #[test]
    fn a_reader_holds_a_fifth_chunk_until_one_is_handled() {
        let (out, flow) = Output::new();
        let (mut credits, fifth) = queue_four_and_read_a_fifth(&flow);
        let (done_tx, done) = std::sync::mpsc::channel();
        let f = flow.clone();
        thread::spawn(move || done_tx.send(f.wait_credit()).unwrap());
        assert!(done.recv_timeout(Duration::from_millis(100)).is_err(), "a fifth chunk must wait");
        credits.pop(); // the main loop handled one
        assert!(done.recv_timeout(Duration::from_secs(5)).unwrap());
        drop((credits, fifth));
        assert_eq!(flow.lock().in_flight, 0);
        drop(out);
    }

    #[test]
    fn a_reader_stops_waiting_and_drains_once_the_pane_goes() {
        let (out, flow) = Output::new();
        let (credits, fifth) = queue_four_and_read_a_fifth(&flow);
        let (done_tx, done) = std::sync::mpsc::channel();
        let f = flow.clone();
        thread::spawn(move || done_tx.send(f.wait_credit()).unwrap());
        assert!(done.recv_timeout(Duration::from_millis(50)).is_err());
        drop(out);
        assert!(!done.recv_timeout(Duration::from_secs(5)).unwrap(), "no credit once the pane is gone");
        assert!(!flow.begin_read());
        // a read in progress when the pane goes is not sent
        assert!(flow.end_read(true).is_none());
        drop((credits, fifth));
        assert_eq!(flow.lock().in_flight, 0);
    }

    #[test]
    fn the_reader_drains_its_terminal_after_the_pane_goes() {
        let (out, flow) = Output::new();
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let data = vec![b'x'; 64 * 1024 * 10];
        let read = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        struct Src(Vec<u8>, usize, Arc<std::sync::atomic::AtomicUsize>);
        impl Read for Src {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = buf.len().min(self.0.len() - self.1).min(64 * 1024);
                buf[..n].copy_from_slice(&self.0[self.1..self.1 + n]);
                self.1 += n;
                self.2.fetch_add(n, Ordering::SeqCst);
                Ok(n)
            }
        }
        let src = Src(data, 0, read.clone());
        let backlog = Backlog::default();
        let b = backlog.clone();
        let none = Pending {
            #[cfg(unix)]
            fd: None,
        };
        let reader = thread::spawn(move || read_pane(Box::new(src), none, flow, b, tx, 1, 0));
        // four chunks reach the queue, then the reader waits holding a fifth
        let mut got = Vec::new();
        for _ in 0..CREDITS {
            got.push(rx.recv_timeout(Duration::from_secs(5)).unwrap());
        }
        thread::sleep(Duration::from_millis(50));
        assert_eq!(read.load(Ordering::SeqCst), (CREDITS + 1) * 64 * 1024);
        assert!(rx.try_recv().is_err());
        assert!(backlog.any(), "the main loop sees the chunk held");
        // handling one lets the held chunk through
        got.pop();
        got.push(rx.recv_timeout(Duration::from_secs(5)).unwrap());
        thread::sleep(Duration::from_millis(50));
        assert_eq!(read.load(Ordering::SeqCst), (CREDITS + 2) * 64 * 1024);
        assert!(backlog.any());
        drop(out);
        reader.join().unwrap();
        assert_eq!(read.load(Ordering::SeqCst), 10 * 64 * 1024, "read to the end");
        assert!(rx.try_recv().is_err(), "nothing sent once the pane is gone");
        assert!(!backlog.any());
        drop(got);
    }

    #[test]
    fn a_burst_takes_the_output_a_reader_holds_up_to_its_deadline() {
        use super::super::next_in_burst;
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let backlog = Backlog::default();
        // nothing queued or held: the burst ends at once
        let start = Instant::now();
        assert!(next_in_burst(&rx, &backlog, start + Duration::from_secs(10)).is_none());
        assert!(start.elapsed() < Duration::from_secs(2));

        // a reader holds a chunk: it is part of the burst
        let (held_tx, held) = std::sync::mpsc::channel();
        let (b, t) = (backlog.clone(), tx.clone());
        let sender = thread::spawn(move || {
            let _held = b.hold();
            held_tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(50));
            t.send(Event::ClientGone(7)).unwrap();
        });
        held.recv().unwrap();
        let got = next_in_burst(&rx, &backlog, Instant::now() + Duration::from_secs(10));
        assert!(matches!(got, Some(Event::ClientGone(7))));
        sender.join().unwrap();
        assert!(!backlog.any());

        // ...but not past the deadline
        let _held = backlog.hold();
        let start = Instant::now();
        assert!(next_in_burst(&rx, &backlog, start + Duration::from_millis(100)).is_none());
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(100) && took < Duration::from_secs(5), "{took:?}");
        drop(tx);
    }

    // A Unix reader polls first: it counts as waiting for output only while it has none.
    #[cfg(unix)]
    #[test]
    fn a_reader_waits_for_output_in_a_poll_and_not_while_it_reads() {
        use std::io::Write;
        use std::os::fd::FromRawFd;
        let mut fds = [0; 2];
        // SAFETY: two descriptors
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: just opened, owned here
        let (r, mut w) = unsafe { (std::fs::File::from_raw_fd(fds[0]), std::fs::File::from_raw_fd(fds[1])) };
        let r = Arc::new(r);
        let (out, flow) = Output::new();
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let backlog = Backlog::default();
        let (f, b, more) = (flow.clone(), backlog.clone(), Pending { fd: Some(r.clone()) });
        let reader = thread::spawn(move || read_pane(Box::new(PtyReader(r)), more, f, b, tx, 1, 0));
        let start = Instant::now();
        while !flow.lock().reading {
            assert!(start.elapsed() < Duration::from_secs(5));
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!backlog.any(), "nothing held while the terminal has nothing");
        w.write_all(b"abc").unwrap();
        let Ok(Event::PaneData(_, _, data, credit)) = rx.recv_timeout(Duration::from_secs(5)) else { panic!("no output") };
        assert_eq!(data, b"abc");
        assert_eq!(flow.lock().in_flight, 1);
        drop(credit);
        drop(w);
        reader.join().unwrap();
        assert!(flow.lock().eof && !backlog.any());
        drop(out);
    }

    #[test]
    fn settle_waits_for_the_main_loop_to_handle_what_was_read() {
        let (_out, flow) = Output::new();
        assert!(flow.begin_read());
        let credit = flow.end_read(true).unwrap();
        assert!(flow.begin_read()); // the reader waits in its next read
        let f = flow.clone();
        let start = Instant::now();
        let waiter = thread::spawn(move || {
            f.settle_with(|| false, Timing { quiet: Duration::from_millis(5), ..SLOW });
            Instant::now()
        });
        thread::sleep(Duration::from_millis(100));
        let handled = Instant::now();
        drop(credit);
        let settled = waiter.join().unwrap();
        assert!(settled >= handled, "settled {:?} before the chunk was handled", handled - start);
        assert!(settled - handled < Duration::from_secs(2));
    }

    #[test]
    fn settle_waits_while_the_kernel_holds_output() {
        let (_out, flow) = Output::new();
        assert!(flow.begin_read());
        let pending = Arc::new(AtomicBool::new(true));
        let (f, p) = (flow.clone(), pending.clone());
        let waiter = thread::spawn(move || {
            f.settle_with(|| p.load(Ordering::SeqCst), Timing { quiet: Duration::from_millis(5), ..SLOW });
            Instant::now()
        });
        thread::sleep(Duration::from_millis(100));
        let drained = Instant::now();
        pending.store(false, Ordering::SeqCst);
        assert!(waiter.join().unwrap() >= drained);
    }

    // The race the quiet time closes: the reader's read has taken the last bytes from the
    // kernel (nothing pending) but the reader has not counted the chunk yet.
    #[test]
    fn settle_does_not_miss_a_read_that_has_returned_but_is_not_counted_yet() {
        let (_out, flow) = Output::new();
        assert!(flow.begin_read());
        let pending = Arc::new(AtomicBool::new(true));
        let (f, p) = (flow.clone(), pending.clone());
        let waiter = thread::spawn(move || {
            f.settle_with(|| p.load(Ordering::SeqCst), SLOW);
            Instant::now()
        });
        thread::sleep(Duration::from_millis(30));
        pending.store(false, Ordering::SeqCst); // the read returned with the last bytes
        thread::sleep(Duration::from_millis(20));
        let credit = flow.end_read(true).unwrap(); // ...and is counted, within the quiet time
        assert!(flow.begin_read());
        thread::sleep(Duration::from_millis(100));
        let handled = Instant::now();
        drop(credit);
        assert!(waiter.join().unwrap() >= handled);
    }

    #[test]
    fn settle_returns_at_end_of_file_or_when_the_pane_goes() {
        // end of file: done once the last chunk is handled, whatever is pending
        let (_out, flow) = Output::new();
        assert!(flow.begin_read());
        let credit = flow.end_read(true).unwrap();
        assert!(flow.begin_read());
        assert!(flow.end_read(false).is_none());
        let f = flow.clone();
        let waiter = thread::spawn(move || {
            f.settle_with(|| true, SLOW);
            Instant::now()
        });
        thread::sleep(Duration::from_millis(50));
        let handled = Instant::now();
        drop(credit);
        let settled = waiter.join().unwrap();
        assert!(settled >= handled && settled - handled < Duration::from_secs(2));

        // the pane has gone (kill-pane, respawn-pane): nothing to wait for
        let (out, flow) = Output::new();
        assert!(flow.begin_read());
        let _never = flow.end_read(true).unwrap();
        let f = flow.clone();
        let waiter = thread::spawn(move || {
            f.settle_with(|| true, SLOW);
            Instant::now()
        });
        thread::sleep(Duration::from_millis(50));
        let gone = Instant::now();
        drop(out);
        let settled = waiter.join().unwrap();
        assert!(settled >= gone && settled - gone < Duration::from_secs(2));
    }

    #[test]
    fn settle_gives_up_when_nothing_moves_or_output_never_ends() {
        let (_out, flow) = Output::new();
        assert!(flow.begin_read());
        let _stuck = flow.end_read(true).unwrap(); // the main loop never handles it
        let start = Instant::now();
        flow.settle_with(|| true, Timing { quiet: Duration::from_millis(2), stall: Duration::from_millis(100), cap: Duration::from_secs(10) });
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(100) && took < Duration::from_secs(5), "{took:?}");

        // a background process that keeps writing: the cap ends the wait
        let (_out, flow) = Output::new();
        let f = flow.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let writer = thread::spawn(move || {
            while !s.load(Ordering::SeqCst) && f.begin_read() {
                let c = f.end_read(true);
                thread::sleep(Duration::from_millis(1));
                drop(c);
            }
        });
        let start = Instant::now();
        // (no stall: the writer may not get the cpu for a while)
        flow.settle_with(|| true, Timing { quiet: Duration::from_millis(2), stall: Duration::from_secs(3), cap: Duration::from_millis(300) });
        let took = start.elapsed();
        stop.store(true, Ordering::SeqCst);
        writer.join().unwrap();
        assert!(took >= Duration::from_millis(300) && took < Duration::from_secs(5), "{took:?}");
    }
}
