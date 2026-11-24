//! A client that cannot keep up (tmux's TTY_BLOCK, tty_block_maybe and
//! tty_timer_callback).
//!
//! tmux stops drawing to a terminal whose output buffer holds 1 + sx*sy*8 bytes or more,
//! counts what it would have written as discarded, and every 100 ms looks at how much
//! that was: below 1 + sx*sy/8 the client is unblocked and redrawn in full. The client is
//! never disconnected for being slow.
//!
//! Here the buffer is the client's queue of frames for its writer thread. Frames already
//! queued are complete sequences and are written, not dropped; while blocked no new frame
//! is rendered. What tmux would have written while blocked is the output of the panes
//! the client shows (tmux passes pane output to the terminal nearly as it comes), so that
//! is what is counted. A full frame diff is not a good measure: this renderer has no
//! scroll optimisation, so one scrolled line rewrites every row, and a client looking at a
//! slowly scrolling pane would never be unblocked. A client is unblocked only once its
//! queue has drained as well, so a client that stays stopped is not sent a full redraw
//! every interval.
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
}

impl Block {
    pub fn active(&self) -> bool {
        self.tick.is_some()
    }
}

impl Client {
    /// Bytes queued for the writer, without the part of the last full redraw not yet
    /// written.
    fn block_backlog(&self) -> usize {
        let queued = self.queued_bytes.load(Ordering::SeqCst);
        let flushed = self.io.queued_total.load(Ordering::SeqCst).saturating_sub(queued as u64);
        queued.saturating_sub(self.block.redraw_until.saturating_sub(flushed) as usize)
    }

    /// tty_block_maybe, before a frame is rendered: true when nothing is to be drawn.
    pub fn block_maybe(&mut self) -> bool {
        if self.block.active() {
            return true;
        }
        let size = self.block_backlog();
        if size < block_start(self.size) {
            return false;
        }
        self.block.tick = Some(Instant::now() + BLOCK_INTERVAL);
        self.block.discarded = 0;
        true
    }

    /// A frame was rendered and queued: if it is the redraw after unblocking, remember
    /// where it ends.
    pub fn block_sent(&mut self) {
        if std::mem::take(&mut self.block.redraw) {
            self.block.redraw_until = self.io.queued_total.load(Ordering::SeqCst);
        }
    }

    /// tty_timer_callback.
    fn block_timer(&mut self, now: Instant) {
        if !self.block.tick.is_some_and(|t| t <= now) {
            return;
        }
        let discarded = std::mem::take(&mut self.block.discarded);
        self.io.discarded.fetch_add(discarded, Ordering::Relaxed);
        let stop = block_stop(self.size);
        if discarded < stop as u64 && self.queued_bytes.load(Ordering::SeqCst) < stop {
            // tty_invalidate and CLIENT_ALLREDRAWFLAGS
            self.block.tick = None;
            self.block.redraw = true;
            self.prev = None;
            self.dirty = true;
            return;
        }
        self.block.tick = Some(now + BLOCK_INTERVAL);
    }
}

impl Server {
    /// Output of `pane` that blocked clients showing it do not get.
    pub(super) fn block_count(&mut self, pane: PaneId, n: usize) {
        if n == 0 || !self.clients.values().any(|c| c.block.active()) {
            return;
        }
        let wid = self.window_of(pane);
        let shown: Vec<ClientId> = self.clients.iter().filter(|(_, c)| c.block.active()).filter(|(cid, c)| match wid {
            Some(w) => self.client_session(**cid).and_then(|s| self.sessions.get(&s)).is_some_and(|s| s.active_window() == w),
            None => matches!(&c.overlay, Some(Overlay::Popup(p)) if p.pane == pane),
        }).map(|(cid, _)| *cid).collect();
        for cid in shown {
            if let Some(c) = self.clients.get_mut(&cid) {
                c.block.discarded += n as u64;
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

    /// A client whose writer never writes (a stopped peer): its queue stays near the
    /// threshold, it is not closed, what it does not get is counted, and once its queue
    /// has drained and the pane is quiet it is unblocked and redrawn in full.
    #[test]
    fn a_client_that_does_not_read_is_blocked_then_redrawn() {
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
        let generation = srv.panes[&pane].generation;
        let start = block_start((120, 32));
        let queued = |srv: &Server| srv.clients[&1].queued_bytes.load(Ordering::SeqCst);
        let mut most = 0;
        let mut fed = 0;
        for i in 0..400 {
            let text: String = (0..50).map(|j| format!("\x1b[3{}m{}\x1b[m\r\n", j % 8, i * 50 + j)).collect();
            fed += text.len();
            srv.pane_data(pane, generation, text.as_bytes());
            srv.render_all();
            most = most.max(queued(&srv));
        }
        let c = &srv.clients[&1];
        assert!(c.block.active() && !c.closed.is_closed());
        // the frame that crossed the threshold is the most that is queued beyond it
        assert!(most >= start && most < start + 32 * 1024, "queued {most} (threshold {start})");
        let written = c.io.written.load(Ordering::Relaxed);
        let frames: u64 = output.try_iter().filter(|f| f[0] == S_OUTPUT).map(|f| f.len() as u64 - 5).sum();
        assert_eq!(written, frames);
        // still blocked while output comes faster than 1 + sx*sy/8 bytes an interval
        let now = Instant::now();
        srv.block_timers(now + BLOCK_INTERVAL);
        let c = &srv.clients[&1];
        assert!(c.block.active());
        let discarded = c.io.discarded.load(Ordering::Relaxed);
        assert!(discarded > (fed / 2) as u64, "discarded {discarded} of {fed}");
        // quiet, but nothing has been read: no redraw is queued behind the old frames
        srv.block_timers(now + 2 * BLOCK_INTERVAL);
        assert!(srv.clients[&1].block.active());
        // the peer reads everything: unblocked at the next check, and redrawn in full
        srv.clients[&1].queued_bytes.store(0, Ordering::SeqCst);
        srv.block_timers(now + 3 * BLOCK_INTERVAL);
        assert!(!srv.clients[&1].block.active());
        srv.render_all();
        let out: Vec<u8> = output.try_iter().filter(|f| f[0] == S_OUTPUT).flat_map(|f| f[5..].to_vec()).collect();
        let out = String::from_utf8_lossy(&out);
        assert!(out.contains("\x1b[0m\x1b[H") && out.contains("19999"), "{out:?}");
        for p in srv.panes.values_mut() {
            p.kill();
        }
    }
}
