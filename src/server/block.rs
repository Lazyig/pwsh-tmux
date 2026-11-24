//! A client that cannot keep up (tmux's TTY_BLOCK, tty_block_maybe and
//! tty_timer_callback, and server_client_check_redraw's deferred redraw).
//!
//! tmux stops drawing to a terminal whose output buffer holds 1 + sx*sy*8 bytes or more,
//! counts what it would have written as discarded, and every 100 ms looks at how much
//! that was: below 1 + sx*sy/8 the client is unblocked and redrawn in full. The client is
//! never disconnected for being slow. tmux also defers a redraw while the buffer is not
//! empty.
//!
//! Here the buffer is the client's queue of frames for its writer thread, with what was
//! written that the client has not yet passed to its terminal (it acknowledges what it
//! has, C_ACK), and every frame is a redraw (a diff of what the client shows): a dirty
//! client is not rendered while frames are still queued for it or on their way to its
//! terminal, and the writer (or the client's acknowledgement) wakes the main loop once
//! they are not, so the next frame covers everything that changed in between instead
//! of every state in turn. Without the acknowledgement, the next frame would wait only
//! for the connection to take the last one, behind what the connection and the client
//! still hold for a slow terminal: on a terminal reading 100 KB/s that kept every frame
//! about 0.3 s behind the pane. A small socket send buffer (see
//! [`transport::shrink_send_buffer`]) keeps what the kernel holds for a client that does
//! not acknowledge (an older one) short. What a deferred client is not shown is the
//! output of the panes it shows (tmux passes pane output to the terminal nearly as it
//! comes); once that and the queue reach the start threshold and the render has waited
//! for [`BLOCK_BEHIND`], the client is blocked and the pane output is counted as
//! discarded. Frames already queued are
//! complete sequences and are written, not dropped, so the client stays blocked until
//! they have been: tmux empties its buffer when it blocks a client and unblocks it into
//! that empty buffer. A full frame diff is not a good measure: a pane beside another
//! one is not scrolled on the terminal (render.rs, find_scroll), so one scrolled line
//! rewrites every row of it.
//!
//! Output outside frames (a bell, an OSC 52 query) goes through tty_add in tmux: while
//! the client is blocked it is counted as discarded like pane output, and otherwise it
//! is added to a buffer that tty_block_maybe empties once it holds the start threshold.
//! Here it is queued, and a run of it the writer has not started on is dropped beyond
//! that threshold, so neither a client that is never blocked (TTY_NOBLOCK) nor one
//! between two blocks queues it without bound.
use super::*;
use std::sync::atomic::Ordering;

pub const BLOCK_INTERVAL: Duration = Duration::from_millis(100);

/// How long a render may wait for the terminal to take the frames before it, with the
/// start threshold of pane output come meanwhile, before the client is blocked. tmux
/// blocks as soon as its buffer holds the threshold, which a flood fills in a few
/// milliseconds once the terminal stops taking output; this allows for a frame being
/// written to a terminal that keeps up (and for a busy machine). A terminal slower than
/// a frame in this time is blocked like tmux's, so frames do not reach it as late as
/// everything it was sent before them takes it to read.
pub const BLOCK_BEHIND: Duration = Duration::from_millis(20);

/// TTY_BLOCK_START, capped well below the queue limit so a huge client size still blocks
/// before the limit would close the connection.
pub fn block_start((sx, sy): (u16, u16)) -> usize {
    (1 + sx as usize * sy as usize * 8).min(transport::MAX_OUTPUT_BACKLOG / 4)
}

/// TTY_BLOCK_STOP.
pub fn block_stop((sx, sy): (u16, u16)) -> usize {
    1 + sx as usize * sy as usize / 8
}

#[derive(Default)]
pub struct Block {
    /// When the next check is due; None when not blocked.
    pub tick: Option<Instant>,
    /// Bytes not written since the last check (tty->discarded).
    pub discarded: u64,
    /// The next frame is the full redraw after unblocking.
    pub redraw: bool,
    /// io.queued_total once that redraw was queued: until the writer is past it, the
    /// redraw does not count toward blocking (tmux's c->redraw).
    pub redraw_until: u64,
    /// The last render was deferred because frames were still queued.
    pub deferred: bool,
    /// Since when renders have been deferred without the queue draining.
    pub deferred_since: Option<Instant>,
    /// Output of the panes the client shows since its render was deferred: what tmux
    /// would have added to its buffer meanwhile.
    pub pending: u64,
    /// io.queued_total once the last passthrough or Ms sequence was queued: until the
    /// writer is past it, the client is not blocked (TTY_NOBLOCK).
    pub noblock_until: std::cell::Cell<u64>,
    /// Output outside frames dropped while blocked since the last check (tty_add adds it
    /// to tty->discarded); added to `discarded` by the check.
    pub tty_discarded: std::cell::Cell<u64>,
    /// The run of output outside frames queued since the writer last got to one: the
    /// io.queued_total where it starts and what it costs against the queue limit.
    pub tty_run: std::cell::Cell<(u64, usize)>,
}

impl Block {
    pub fn active(&self) -> bool {
        self.tick.is_some()
    }
}

impl Client {
    /// Frames are queued, or output written has not been taken by the terminal yet
    /// (C_ACK): tmux's tty buffer is not empty.
    fn waiting(&self) -> bool {
        self.queued_bytes.load(Ordering::SeqCst) > 0 || self.io.unacked() > 0
    }

    /// What io.queued_total the writer has written up to.
    fn flushed(&self, queued: usize) -> u64 {
        self.io.queued_total.load(Ordering::SeqCst).saturating_sub(queued as u64)
    }

    /// A passthrough or Ms sequence was queued: tmux sets TTY_NOBLOCK, so a large image or
    /// clipboard write does not block a client that reads it.
    pub fn noblock(&self) {
        self.block.noblock_until.set(self.io.queued_total.load(Ordering::SeqCst));
    }

    /// tty_add while blocked: the output is dropped and counted toward the next check, as
    /// pane output drawn to the client is.
    fn tty_blocked(&self, data: &[u8]) -> bool {
        if self.block.active() {
            self.block.tty_discarded.set(self.block.tty_discarded.get() + data.len() as u64);
        }
        self.block.active()
    }

    /// tty_add for terminal output outside frames (a bell, an OSC 52 query): dropped and
    /// counted as discarded while the client is blocked; otherwise queued unless the run
    /// of such output the writer has not started on already costs the block start
    /// threshold, which is as much as tmux's buffer holds before tty_block_maybe empties
    /// it. A client that is reading gets to each run, so nothing it is sent is dropped;
    /// a stopped one that cannot be blocked (TTY_NOBLOCK), or is between two blocks,
    /// does not queue it without bound. Returns whether it was queued.
    pub fn send_tty(&self, data: &[u8]) -> bool {
        if self.tty_blocked(data) {
            return false;
        }
        let queued = self.queued_bytes.load(Ordering::SeqCst);
        let (mut from, mut run) = self.block.tty_run.get();
        if queued == 0 || self.flushed(queued) > from {
            (from, run) = (self.io.queued_total.load(Ordering::SeqCst), 0);
        }
        let cost = transport::frame_cost(data.len());
        if run + cost > block_start(self.size) {
            self.io.discarded.fetch_add(data.len() as u64, Ordering::Relaxed);
            self.block.tty_run.set((from, run));
            return false;
        }
        self.send(S_OUTPUT, data);
        self.block.tty_run.set((from, run + cost));
        true
    }

    /// Ms (tty_set_selection): dropped and counted while the client is blocked, otherwise
    /// queued however long it is, without blocking the client while it is written
    /// (TTY_NOBLOCK).
    pub fn send_noblock(&self, data: &[u8]) {
        if !self.tty_blocked(data) {
            self.send(S_OUTPUT, data);
            self.noblock();
        }
    }

    /// Before a frame is rendered: true when nothing is to be drawn now, because the
    /// client is blocked (tty_block_maybe) or frames are still queued for it (a deferred
    /// redraw; the writer wakes the main loop when they are written).
    pub fn block_maybe(&mut self) -> bool {
        if self.block.active() {
            return true;
        }
        let queued = self.queued_bytes.load(Ordering::SeqCst);
        if queued == 0 && self.io.unacked() == 0 {
            self.block.deferred = false;
            self.block.deferred_since = None;
            self.block.pending = 0;
            return false;
        }
        let now = Instant::now();
        let flushed = self.flushed(queued);
        // without the part of the last full redraw not yet written
        let backlog = queued.saturating_sub(self.block.redraw_until.saturating_sub(flushed) as usize) as u64 + self.block.pending;
        // A client that takes its frames is not blocked by a burst of output that came
        // while one was being written (pane output arrives far faster than frames are
        // sent to a terminal that keeps up): only one whose queue has not drained for an
        // interval.
        let behind = self.block.deferred_since.is_some_and(|t| now.duration_since(t) >= BLOCK_BEHIND);
        if behind && flushed >= self.block.noblock_until.get() && backlog >= block_start(self.size) as u64 {
            self.block.tick = Some(now + BLOCK_INTERVAL);
            // tmux drains its buffer: here what it holds beyond the queue is pane output
            self.io.discarded.fetch_add(self.block.pending, Ordering::Relaxed);
            self.block.pending = 0;
            self.block.deferred = false;
            self.block.deferred_since = None;
            self.block.discarded = 0;
            return true;
        }
        self.block.deferred = true;
        self.block.deferred_since.get_or_insert(now);
        // set before looking again, so either this sees the queue drained or the writer
        // sees the flag
        self.io.drain_wake.store(true, Ordering::SeqCst);
        if self.waiting() {
            return true;
        }
        self.io.drain_wake.store(false, Ordering::SeqCst);
        self.block.deferred = false;
        self.block.deferred_since = None;
        self.block.pending = 0;
        false
    }

    /// A frame was rendered and queued: if it is the redraw after unblocking, remember
    /// where it ends.
    pub fn block_sent(&mut self) {
        if std::mem::take(&mut self.block.redraw) {
            self.block.redraw_until = self.io.queued_total.load(Ordering::SeqCst);
        }
    }

    /// What was discarded since the last check goes to client_discarded (the client is
    /// going, or tty_timer_callback).
    pub fn block_flush(&mut self) -> u64 {
        let discarded = std::mem::take(&mut self.block.discarded) + self.block.tty_discarded.take();
        self.io.discarded.fetch_add(discarded, Ordering::Relaxed);
        discarded
    }

    /// tty_timer_callback. tmux unblocks a client into the buffer it emptied when it
    /// blocked it; here the frames queued before the block are written (and taken by the
    /// terminal) instead, so the client stays blocked, and what it is sent is dropped,
    /// until they have been. A stopped client therefore keeps its timer, as a tmux client
    /// that stays behind does.
    ///
    /// The first check marks the client for a full redraw, and from then on the panes'
    /// output is not drawn to it, so not counted (tmux's CLIENT_REDRAWWINDOW makes
    /// tty_client_ready skip it): a client is unblocked by the next check once the
    /// terminal has taken what it was sent, even while the panes are still writing. It
    /// is sent nothing meanwhile, so a slow terminal has had at least an interval to
    /// catch up when the redraw comes, and it shows what the panes show then.
    fn block_timer(&mut self, now: Instant) {
        if !self.block.tick.is_some_and(|t| t <= now) {
            return;
        }
        let discarded = self.block_flush();
        // CLIENT_ALLREDRAWFLAGS: from here the panes' output is not drawn to the client
        // (tty_client_ready), so not counted, until it has been redrawn in full
        self.block.redraw = true;
        self.prev = None;
        self.dirty = true;
        if self.waiting() || discarded >= block_stop(self.size) as u64 {
            self.block.tick = Some(now + BLOCK_INTERVAL);
        } else {
            // tty_invalidate
            self.block.tick = None;
        }
    }

    /// Whether panes are held as they were (a prompt or message that freezes them,
    /// TTY_FREEZE; see Server::freeze_panes).
    fn panes_frozen(&self) -> bool {
        self.prompt.as_ref().is_some_and(|p| p.freeze && !p.incremental && p.copy_incremental.is_none() && p.host.is_none())
            || (self.message.is_some() && !self.message_no_freeze)
    }
}

impl Server {
    /// Whether output of `pane` is drawn to client `c` as it comes (tty_client_ready and
    /// screen_write_set_client_cb): the pane is on screen, not hidden by zoom, not in a
    /// mode (whose screen is shown instead) and not held by a prompt or message.
    pub(super) fn pane_drawn(&self, c: &Client, pane: PaneId) -> bool {
        if !c.attached || c.suspended || c.control != 0 || c.panes_frozen() {
            return false;
        }
        if matches!(&c.overlay, Some(Overlay::Popup(p)) if p.pane == pane) {
            return true;
        }
        let Some(w) = c.session.and_then(|s| self.sessions.get(&s)).and_then(|s| self.windows.get(&s.active_window())) else { return false };
        w.panes().contains(&pane) && (!w.zoomed || w.active == pane) && self.panes.get(&pane).is_some_and(|p| p.mode.is_none())
    }

    /// Output of `pane`: what blocked clients drawing it do not get, and what deferred
    /// ones have not been shown yet.
    pub(super) fn block_count(&mut self, pane: PaneId, n: usize) {
        if n == 0 || !self.clients.values().any(|c| c.block.active() || c.block.deferred) {
            return;
        }
        let shown: Vec<ClientId> = self.clients.iter().filter(|(_, c)| (c.block.active() || c.block.deferred) && self.pane_drawn(c, pane)).map(|(cid, _)| *cid).collect();
        for cid in shown {
            if let Some(c) = self.clients.get_mut(&cid) {
                if c.block.active() {
                    // not drawn while a full redraw is due (CLIENT_REDRAWWINDOW)
                    if !c.block.redraw {
                        c.block.discarded += n as u64;
                    }
                } else {
                    c.block.pending += n as u64;
                }
            }
        }
    }

    pub(super) fn block_timers(&mut self, now: Instant) {
        for c in self.clients.values_mut() {
            c.block_timer(now);
        }
    }

    pub(super) fn block_deadline(&self) -> Option<Instant> {
        self.clients.values().filter_map(|c| c.block.tick).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Client 1 attached to a new 120x32 session, with a writer that never writes (a
    /// stopped peer) whose frames the receiver gets.
    fn stopped() -> (Server, mpsc::Receiver<Vec<u8>>, PaneId) {
        let (mut srv, _events) = super::super::security_tests::fixture();
        let (tx, output) = mpsc::channel();
        let c = srv.clients.get_mut(&1).unwrap();
        c.readonly = false;
        c.acl_readonly = false;
        c.tx = tx;
        c.closed = Default::default();
        c.size = (120, 32);
        let mut ctx = Ctx::default();
        let args = ["new-session", "-d", "-s", "b", "-x", "120", "-y", "32"];
        srv.exec(args.iter().map(|s| s.to_string()).collect(), &mut ctx).unwrap();
        let sid = *srv.sessions.keys().next().unwrap();
        srv.attach_client(1, sid);
        let pane = srv.client_active_pane(1).unwrap();
        (srv, output, pane)
    }

    fn run(srv: &mut Server, args: &[&str]) {
        let mut ctx = Ctx { client: Some(1), ..Default::default() };
        srv.exec(args.iter().map(|s| s.to_string()).collect(), &mut ctx).unwrap();
    }

    fn feed(srv: &mut Server, pane: PaneId, from: usize, lines: usize) -> usize {
        let text: String = (from..from + lines).map(|j| format!("\x1b[3{}m{}\x1b[m\r\n", j % 8, j)).collect();
        let generation = srv.panes[&pane].generation;
        srv.pane_data(pane, generation, text.as_bytes());
        text.len()
    }

    /// The queue has not drained for an interval.
    fn behind(srv: &mut Server) {
        let c = srv.clients.get_mut(&1).unwrap();
        c.block.deferred_since = Some(Instant::now() - BLOCK_INTERVAL);
    }

    fn queued(srv: &Server) -> usize {
        srv.clients[&1].queued_bytes.load(Ordering::SeqCst)
    }

    /// The writer wrote everything queued.
    fn drain(srv: &Server) {
        srv.clients[&1].queued_bytes.store(0, Ordering::SeqCst);
    }

    fn frames(output: &mpsc::Receiver<Vec<u8>>) -> String {
        let out: Vec<u8> = output.try_iter().filter(|f| f[0] == S_OUTPUT).flat_map(|f| f[5..].to_vec()).collect();
        String::from_utf8_lossy(&out).into_owned()
    }

    fn kill(srv: &mut Server) {
        for p in srv.panes.values_mut() {
            p.kill();
        }
    }

    /// While frames are queued nothing more is rendered; what the client is not shown
    /// meanwhile blocks it at the start threshold, it is not closed, what it does not get
    /// is counted, and once the pane is quiet and its queue has drained it is redrawn in
    /// full.
    #[test]
    fn a_client_that_does_not_read_is_blocked_then_redrawn() {
        let (mut srv, output, pane) = stopped();
        let start = block_start((120, 32));
        let mut most = 0;
        let mut fed = 0;
        for i in 0..400 {
            fed += feed(&mut srv, pane, i * 50, 50);
            if i <= 100 {
                // less than an interval since the render was deferred, however slowly
                // this runs
                let c = srv.clients.get_mut(&1).unwrap();
                c.block.deferred_since = c.block.deferred_since.map(|_| Instant::now());
            }
            srv.render_all();
            // a burst while a frame is written does not block
            assert!(!srv.clients[&1].block.active() || i > 100);
            if i == 100 {
                behind(&mut srv);
            }
            most = most.max(queued(&srv));
        }
        let c = &srv.clients[&1];
        assert!(c.block.active() && !c.closed.is_closed());
        // the first frame is all that was queued: renders waited for it to be written
        assert!(most > 0 && most < start, "queued {most} (threshold {start})");
        let written = c.io.written.load(Ordering::Relaxed);
        let sent: u64 = output.try_iter().filter(|f| f[0] == S_OUTPUT).map(|f| f.len() as u64 - 5).sum();
        assert_eq!(written, sent);
        // still blocked while output comes faster than 1 + sx*sy/8 bytes an interval
        let now = Instant::now();
        srv.block_timers(now + BLOCK_INTERVAL);
        let c = &srv.clients[&1];
        assert!(c.block.active());
        let discarded = c.io.discarded.load(Ordering::Relaxed);
        assert!(discarded > (fed / 2) as u64, "discarded {discarded} of {fed}");
        // quiet, but nothing has been read: still blocked, so nothing more is queued
        srv.block_timers(now + 2 * BLOCK_INTERVAL);
        assert!(srv.clients[&1].block.active() && srv.block_deadline().is_some());
        srv.render_all();
        assert!(frames(&output).is_empty());
        // the peer reads everything: unblocked at the next check and redrawn in full
        drain(&srv);
        srv.block_timers(now + 3 * BLOCK_INTERVAL);
        assert!(!srv.clients[&1].block.active() && srv.block_deadline().is_none());
        srv.render_all();
        let out = frames(&output);
        assert!(out.contains("\x1b[0m\x1b[H") && out.contains("19999"), "{out:?}");
        kill(&mut srv);
    }

    /// A blocked client that has read what was queued is unblocked and redrawn in full
    /// while the pane keeps writing: after the first check the pane's output is no longer
    /// drawn to it, so not counted (CLIENT_REDRAWWINDOW).
    #[test]
    fn a_client_that_catches_up_is_unblocked_while_output_goes_on() {
        let (mut srv, output, pane) = stopped();
        let mut n = 0;
        while !srv.clients[&1].block.active() {
            n = feed(&mut srv, pane, n, 50) + n;
            srv.render_all();
            behind(&mut srv);
        }
        let now = Instant::now();
        feed(&mut srv, pane, n, 400);
        srv.block_timers(now + BLOCK_INTERVAL);
        assert!(srv.clients[&1].block.active() && srv.clients[&1].block.redraw);
        let _ = frames(&output);
        drain(&srv);
        feed(&mut srv, pane, n + 400, 400);
        assert_eq!(srv.clients[&1].block.discarded, 0);
        srv.block_timers(now + 2 * BLOCK_INTERVAL);
        assert!(!srv.clients[&1].block.active() && srv.clients[&1].block.redraw);
        srv.render_all();
        let out = frames(&output);
        assert!(out.contains("\x1b[0m\x1b[H") && out.contains(&format!("{}", n + 799)), "{out:?}");
        kill(&mut srv);
    }

    /// tty_timer_callback while the panes keep writing to a client that reads: blocked,
    /// then (whatever was written in the first interval) unblocked by the second check
    /// with a full redraw, and sent nothing in between.
    #[test]
    fn a_flood_keeps_a_reading_client_blocked_for_two_checks_at_most() {
        let (mut srv, output, pane) = stopped();
        let mut n = 0;
        while !srv.clients[&1].block.active() {
            n = feed(&mut srv, pane, n, 50) + n;
            srv.render_all();
            behind(&mut srv);
        }
        drain(&srv);
        let _ = frames(&output);
        let now = Instant::now();
        feed(&mut srv, pane, n, 400);
        assert!(srv.clients[&1].block.discarded > block_stop((120, 32)) as u64);
        srv.block_timers(now + BLOCK_INTERVAL);
        assert!(srv.clients[&1].block.active());
        feed(&mut srv, pane, n + 400, 400);
        srv.render_all();
        assert!(frames(&output).is_empty());
        srv.block_timers(now + 2 * BLOCK_INTERVAL);
        assert!(!srv.clients[&1].block.active());
        srv.render_all();
        let out = frames(&output);
        assert!(out.contains("\x1b[0m\x1b[H") && out.contains(&format!("{}", n + 799)), "{out:?}");
        kill(&mut srv);
    }

    /// A client that acknowledges its output (C_ACK) is not rendered to while what was
    /// written to it has not reached its terminal, though the writer has emptied the
    /// queue; its acknowledgement wakes the main loop. One that never acknowledges is
    /// rendered to once the queue has drained.
    #[test]
    fn renders_wait_for_the_terminal_to_take_the_output_before() {
        let (mut srv, output, pane) = stopped();
        drain(&srv);
        let _ = frames(&output);
        srv.render_all();
        let sent: usize = output.try_iter().filter(|f| f[0] == S_OUTPUT).map(|f| f.len() - 5).sum();
        assert!(sent > 10);
        let io = srv.clients[&1].io.clone();
        // the writer wrote it all; an old client has not acknowledged anything
        io.output_sent.store(sent as u64, Ordering::SeqCst);
        drain(&srv);
        assert_eq!(io.unacked(), 0);
        // the client acknowledges part of it: the rest is on its way to the terminal
        assert!(!io.ack(10, &srv.clients[&1].queued_bytes));
        assert_eq!(io.unacked(), sent as u64 - 10);
        feed(&mut srv, pane, 0, 5);
        srv.render_all();
        assert!(frames(&output).is_empty() && srv.clients[&1].block.deferred);
        assert!(io.drain_wake.load(Ordering::SeqCst));
        // a stale acknowledgement changes nothing
        assert!(!io.ack(5, &srv.clients[&1].queued_bytes));
        // the terminal has it all: the main loop is woken (once) and renders
        assert!(io.ack(sent as u64, &srv.clients[&1].queued_bytes));
        assert!(!io.ack(sent as u64, &srv.clients[&1].queued_bytes));
        srv.render_all();
        assert!(frames(&output).contains("\x1b[0;34m4"));
        // nor while frames are still queued, however much was acknowledged
        feed(&mut srv, pane, 5, 1);
        srv.render_all();
        assert!(!io.ack(u64::MAX, &srv.clients[&1].queued_bytes) && queued(&srv) > 0);
        kill(&mut srv);
    }

    /// TTY_NOBLOCK: a large passthrough or Ms sequence the client is reading does not
    /// block it; renders only wait for it.
    #[test]
    fn a_large_passthrough_does_not_block() {
        let (mut srv, output, pane) = stopped();
        srv.render_all();
        drain(&srv);
        let _ = frames(&output);
        let image = vec![b'x'; 40 * 1024];
        srv.clients[&1].send(S_OUTPUT, &image);
        srv.clients[&1].noblock();
        feed(&mut srv, pane, 0, 10);
        srv.render_all();
        behind(&mut srv);
        srv.render_all();
        assert!(!srv.clients[&1].block.active() && srv.clients[&1].block.deferred);
        // the same without TTY_NOBLOCK blocks
        let (mut other, _o, p) = stopped();
        other.render_all();
        drain(&other);
        other.clients[&1].send(S_OUTPUT, &image);
        feed(&mut other, p, 0, 10);
        other.render_all();
        behind(&mut other);
        other.render_all();
        assert!(other.clients[&1].block.active());
        kill(&mut other);
        // read: drawn as usual, not redrawn in full
        assert!(!srv.clients[&1].block.redraw);
        drain(&srv);
        srv.render_all();
        let out = frames(&output);
        assert!(out.len() > image.len() && out[image.len()..].contains("\x1b[10;1H\x1b[0;31m9"), "{:?}", &out[image.len()..]);
        assert!(!srv.clients[&1].block.deferred);
        kill(&mut srv);
    }

    /// Output that is not drawn to the client (a pane hidden by zoom, a pane in copy
    /// mode) is not counted as discarded, so it does not keep the client blocked, and
    /// Ms is dropped while blocked.
    #[test]
    fn what_is_not_drawn_does_not_keep_a_client_blocked() {
        let (mut srv, output, first) = stopped();
        run(&mut srv, &["split-window", "-d", "-t", &format!("%{first}")]);
        let wid = srv.client_window(1).unwrap();
        let second = *srv.windows[&wid].panes().iter().find(|p| **p != first).unwrap();
        srv.render_all();
        let mut n = 0;
        while !srv.clients[&1].block.active() {
            n = feed(&mut srv, first, n, 50) + n;
            srv.render_all();
            behind(&mut srv);
        }
        let now = Instant::now();
        let discarded = |srv: &Server| srv.clients[&1].block.discarded;
        run(&mut srv, &["resize-pane", "-Z", "-t", &format!("%{first}")]);
        feed(&mut srv, second, 0, 300);
        assert_eq!(discarded(&srv), 0, "a pane hidden by zoom");
        run(&mut srv, &["resize-pane", "-Z", "-t", &format!("%{first}")]);
        run(&mut srv, &["copy-mode", "-t", &format!("%{second}")]);
        feed(&mut srv, second, 0, 300);
        assert_eq!(discarded(&srv), 0, "a pane in copy mode");
        feed(&mut srv, first, 0, 1);
        assert!(discarded(&srv) > 0, "a pane on screen");
        // Ms is dropped and counted at the next check
        srv.clients.get_mut(&1).unwrap().features.add("clipboard");
        let (before, total) = (queued(&srv), srv.clients[&1].io.discarded.load(Ordering::Relaxed));
        srv.set_selection(1, "", &[b'y'; 3000]);
        assert_eq!(queued(&srv), before);
        assert!(srv.clients[&1].block.tty_discarded.get() >= 4000);
        // once the queue has drained
        srv.block_timers(now + BLOCK_INTERVAL);
        assert!(srv.clients[&1].block.active());
        drain(&srv);
        srv.block_timers(now + 2 * BLOCK_INTERVAL);
        assert!(!srv.clients[&1].block.active());
        assert!(srv.clients[&1].io.discarded.load(Ordering::Relaxed) >= total + 4000);
        let _ = frames(&output);
        kill(&mut srv);
    }

    /// A bell rings through tty_add like any other terminal output: a stopped client is
    /// blocked by a pane that only rings, and bells then are dropped and counted, so its
    /// queue stays near the start threshold instead of growing to the limit.
    #[test]
    fn bells_are_dropped_while_blocked() {
        let (mut srv, output, pane) = stopped();
        srv.render_all();
        let start = block_start((120, 32));
        let mut most = 0;
        for i in 0..20_000 {
            let generation = srv.panes[&pane].generation;
            srv.pane_data(pane, generation, b"\x07");
            srv.render_all();
            if i % 100 == 0 {
                behind(&mut srv);
            }
            most = most.max(queued(&srv));
        }
        let c = &srv.clients[&1];
        assert!(c.block.active() && !c.closed.is_closed());
        assert!(most < 2 * start, "queued {most} (threshold {start})");
        // what was queued is the first frame and bells up to the threshold
        let queued_bells = output.try_iter().filter(|f| *f == frame(S_OUTPUT, b"\x07")).count();
        assert!(queued_bells > 0 && queued_bells < 20_000, "{queued_bells}");
        assert!(c.io.discarded.load(Ordering::Relaxed) + c.block.tty_discarded.get() >= (20_000 - queued_bells) as u64);
        // read: unblocked once an interval has gone by without as many bells dropped as
        // the stop threshold, and bells are queued again
        drain(&srv);
        let now = Instant::now();
        srv.block_timers(now + BLOCK_INTERVAL);
        srv.block_timers(now + 2 * BLOCK_INTERVAL);
        assert!(!srv.clients[&1].block.active());
        bell(&mut srv, pane);
        assert_eq!(bells(&output), 1);
        kill(&mut srv);
    }

    fn bell(srv: &mut Server, pane: PaneId) {
        let generation = srv.panes[&pane].generation;
        srv.pane_data(pane, generation, b"\x07");
    }

    fn bells(output: &mpsc::Receiver<Vec<u8>>) -> usize {
        output.try_iter().filter(|f| *f == frame(S_OUTPUT, b"\x07")).count()
    }

    /// Rings `n` bells in `pane`, rendering after each; a deferred render has waited for an
    /// interval at every `behind_every`th, and the timer runs (100 ms later each time) at every
    /// `tick`th. Returns the most that was queued.
    fn ring(srv: &mut Server, pane: PaneId, n: usize, behind_every: usize, tick: usize) -> usize {
        let t0 = Instant::now();
        let mut most = 0;
        for i in 0..n {
            bell(srv, pane);
            srv.render_all();
            // (a render deferred for an interval: a blocked client is not rendered)
            let c = &srv.clients[&1];
            if i % behind_every == behind_every / 2 && c.block.deferred && !c.block.active() {
                behind(srv);
            }
            if i % tick == 0 {
                srv.block_timers(t0 + BLOCK_INTERVAL * (i / tick + 1) as u32);
            }
            most = most.max(queued(srv));
        }
        most
    }

    /// Bells from a window the client does not show are not pane output drawn to it, so
    /// at each check while it is blocked only the bells dropped (tty_add) count: below
    /// the stop threshold. A stopped client is still not unblocked while its queue has
    /// not drained, so the bells do not pile up behind the frames it has not read.
    #[test]
    fn background_bells_to_a_stopped_client_stay_bounded() {
        let (mut srv, output, _) = stopped();
        run(&mut srv, &["new-window", "-d"]);
        let shown = srv.client_window(1).unwrap();
        let other = srv.windows.iter().find(|(w, _)| **w != shown).map(|(_, w)| w.active).unwrap();
        srv.render_all();
        let start = block_start((120, 32));
        let most = ring(&mut srv, other, 40_000, 100, 100);
        assert!(most < 2 * start, "queued {most} (threshold {start})");
        let c = &srv.clients[&1];
        assert!(c.block.active() && !c.closed.is_closed());
        assert!(bells(&output) < 1000);
        // the client reads: unblocked at the next check, and a bell is sent at once
        drain(&srv);
        srv.block_timers(Instant::now() + BLOCK_INTERVAL * 1000);
        assert!(!srv.clients[&1].block.active());
        bell(&mut srv, other);
        assert_eq!(bells(&output), 1);
        kill(&mut srv);
    }

    /// Fewer bells an interval than the stop threshold from the pane the client shows:
    /// each check sees too little discarded to keep it blocked, but its queue has not
    /// drained, so it stays blocked and the bells are not queued.
    #[test]
    fn slow_bells_to_a_stopped_client_stay_bounded() {
        let (mut srv, output, pane) = stopped();
        srv.render_all();
        let start = block_start((120, 32));
        let most = ring(&mut srv, pane, 20_000, 1, 1);
        assert!(most < 2 * start, "queued {most} (threshold {start})");
        let c = &srv.clients[&1];
        assert!(c.block.active() && !c.closed.is_closed());
        assert!(bells(&output) < 1000);
        kill(&mut srv);
    }

    /// A client that cannot be blocked because the Ms it was sent has not been written
    /// (TTY_NOBLOCK) does not queue bells without bound either: a run of output outside
    /// frames the writer has not started on stops at the start threshold.
    #[test]
    fn bells_behind_an_unwritten_ms_stay_bounded() {
        let (mut srv, output, pane) = stopped();
        srv.render_all();
        drain(&srv);
        let _ = frames(&output);
        srv.clients.get_mut(&1).unwrap().features.add("clipboard");
        srv.set_selection(1, "", &[b'y'; 30_000]);
        let ms = queued(&srv);
        let start = block_start((120, 32));
        let most = ring(&mut srv, pane, 20_000, 1, 1);
        let c = &srv.clients[&1];
        assert!(!c.block.active() && !c.closed.is_closed());
        assert!(most < ms + 2 * start, "queued {most} (Ms {ms}, threshold {start})");
        let discarded = c.io.discarded.load(Ordering::Relaxed);
        assert!(discarded > 15_000, "discarded {discarded}");
        kill(&mut srv);
    }

    /// A client that reads gets every bell as it rings, whether or not a frame is still
    /// being written to it.
    #[test]
    fn a_reading_client_gets_every_bell() {
        let (mut srv, output, pane) = stopped();
        srv.render_all();
        for _ in 0..2_000 {
            bell(&mut srv, pane);
            srv.render_all();
            drain(&srv);
        }
        assert_eq!(bells(&output), 2_000);
        // while a frame is being written: the writer takes a little more than a bell
        // between two bells
        feed(&mut srv, pane, 0, 40);
        srv.render_all();
        assert!(queued(&srv) > 0);
        for n in 1..=2_000 {
            bell(&mut srv, pane);
            assert_eq!(bells(&output), 1, "bell {n}");
            let c = &srv.clients[&1];
            let q = c.queued_bytes.load(Ordering::SeqCst);
            c.queued_bytes.fetch_sub(q.min(transport::frame_cost(1) + 30), Ordering::SeqCst);
        }
        assert!(!srv.clients[&1].block.active());
        kill(&mut srv);
    }
}
