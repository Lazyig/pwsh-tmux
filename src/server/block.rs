//! A client that cannot keep up (tmux's TTY_BLOCK, tty_block_maybe and
//! tty_timer_callback, and server_client_check_redraw's deferred redraw).
//!
//! tmux stops drawing to a terminal whose output buffer holds 1 + sx*sy*8 bytes or more,
//! counts what it would have written as discarded, and every 100 ms looks at how much
//! that was: below 1 + sx*sy/8 the client is unblocked and redrawn in full. The client is
//! never disconnected for being slow. tmux also defers a redraw while the buffer is not
//! empty.
//!
//! Here the buffer is the client's queue of frames for its writer thread, and every
//! frame is a redraw (a diff of what the client shows): a dirty client is not rendered
//! while frames are still queued for it, and the writer wakes the main loop once its
//! queue has drained, so the next frame covers everything that changed in between
//! instead of every state in turn. A small socket send buffer (see
//! [`transport::shrink_send_buffer`]) keeps what the kernel holds for a slow peer short.
//! What a deferred client is not shown is the output of the panes it shows (tmux
//! passes pane output to the terminal nearly as it comes); once that and the queue
//! reach the start threshold, the client is blocked and the pane output is counted as
//! discarded. Frames already queued are complete sequences and are written, not
//! dropped. A full frame diff is not a good measure: this renderer has no scroll
//! optimisation, so one scrolled line rewrites every row.
use super::*;
use std::sync::atomic::Ordering;

pub const BLOCK_INTERVAL: Duration = Duration::from_millis(100);

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
    /// Output of the panes the client shows since its render was deferred: what tmux
    /// would have added to its buffer meanwhile.
    pub pending: u64,
    /// io.queued_total once the last passthrough or Ms sequence was queued: until the
    /// writer is past it, the client is not blocked (TTY_NOBLOCK).
    pub noblock_until: std::cell::Cell<u64>,
}

impl Block {
    pub fn active(&self) -> bool {
        self.tick.is_some()
    }
}

impl Client {
    /// What io.queued_total the writer has written up to.
    fn flushed(&self, queued: usize) -> u64 {
        self.io.queued_total.load(Ordering::SeqCst).saturating_sub(queued as u64)
    }

    /// A passthrough or Ms sequence was queued: tmux sets TTY_NOBLOCK, so a large image or
    /// clipboard write does not block a client that reads it.
    pub fn noblock(&self) {
        self.block.noblock_until.set(self.io.queued_total.load(Ordering::SeqCst));
    }

    /// tty_add for output outside frames that tmux drops while blocked (Ms): counted as
    /// discarded then, otherwise queued without blocking the client.
    pub fn send_noblock(&self, data: &[u8]) {
        if self.block.active() {
            self.io.discarded.fetch_add(data.len() as u64, Ordering::Relaxed);
            return;
        }
        self.send(S_OUTPUT, data);
        self.noblock();
    }

    /// Before a frame is rendered: true when nothing is to be drawn now, because the
    /// client is blocked (tty_block_maybe) or frames are still queued for it (a deferred
    /// redraw; the writer wakes the main loop when they are written).
    pub fn block_maybe(&mut self) -> bool {
        if self.block.active() {
            return true;
        }
        let queued = self.queued_bytes.load(Ordering::SeqCst);
        if queued == 0 {
            self.block.deferred = false;
            self.block.pending = 0;
            return false;
        }
        let flushed = self.flushed(queued);
        // without the part of the last full redraw not yet written
        let backlog = queued.saturating_sub(self.block.redraw_until.saturating_sub(flushed) as usize) as u64 + self.block.pending;
        if flushed >= self.block.noblock_until.get() && backlog >= block_start(self.size) as u64 {
            self.block.tick = Some(Instant::now() + BLOCK_INTERVAL);
            // tmux drains its buffer: here what it holds beyond the queue is pane output
            self.io.discarded.fetch_add(self.block.pending, Ordering::Relaxed);
            self.block.pending = 0;
            self.block.deferred = false;
            self.block.discarded = 0;
            return true;
        }
        self.block.deferred = true;
        // set before looking again, so either this sees the queue drained or the writer
        // sees the flag
        self.io.drain_wake.store(true, Ordering::SeqCst);
        if self.queued_bytes.load(Ordering::SeqCst) > 0 {
            return true;
        }
        self.io.drain_wake.store(false, Ordering::SeqCst);
        self.block.deferred = false;
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
        let discarded = std::mem::take(&mut self.block.discarded);
        self.io.discarded.fetch_add(discarded, Ordering::Relaxed);
        discarded
    }

    /// tty_timer_callback. Once unblocked, the redraw waits for the queue to drain like
    /// any other frame, so a client that stays stopped needs no timer.
    fn block_timer(&mut self, now: Instant) {
        if !self.block.tick.is_some_and(|t| t <= now) {
            return;
        }
        if self.block_flush() < block_stop(self.size) as u64 {
            // tty_invalidate and CLIENT_ALLREDRAWFLAGS
            self.block.tick = None;
            self.block.redraw = true;
            self.prev = None;
            self.dirty = true;
            return;
        }
        self.block.tick = Some(now + BLOCK_INTERVAL);
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
                    c.block.discarded += n as u64;
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
            srv.render_all();
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
        // quiet: unblocked, but nothing has been read, so the redraw waits (no timer)
        srv.block_timers(now + 2 * BLOCK_INTERVAL);
        assert!(!srv.clients[&1].block.active() && srv.block_deadline().is_none());
        srv.render_all();
        assert!(frames(&output).is_empty());
        assert!(srv.clients[&1].io.drain_wake.load(Ordering::SeqCst));
        // the peer reads everything: redrawn in full
        drain(&srv);
        srv.render_all();
        let out = frames(&output);
        assert!(out.contains("\x1b[0m\x1b[H") && out.contains("19999"), "{out:?}");
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
        assert!(!srv.clients[&1].block.active() && srv.clients[&1].block.deferred);
        // the same without TTY_NOBLOCK blocks
        let (mut other, _o, p) = stopped();
        other.render_all();
        drain(&other);
        other.clients[&1].send(S_OUTPUT, &image);
        feed(&mut other, p, 0, 10);
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
        // Ms is dropped and counted
        srv.clients.get_mut(&1).unwrap().features.add("clipboard");
        let (before, total) = (queued(&srv), srv.clients[&1].io.discarded.load(Ordering::Relaxed));
        srv.set_selection(1, "", &[b'y'; 3000]);
        assert_eq!(queued(&srv), before);
        assert!(srv.clients[&1].io.discarded.load(Ordering::Relaxed) >= total + 4000);
        srv.block_timers(now + BLOCK_INTERVAL);
        assert!(!srv.clients[&1].block.active());
        let _ = frames(&output);
        kill(&mut srv);
    }
}
