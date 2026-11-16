//! The part of a window a client shows when the window is bigger than the client
//! (tmux tty_window_offset1, refresh-client -L/-R/-U/-D/-c).
use super::*;

/// Where a client's view of its current window is (tmux tty->oox, ooy, osx, osy and oflag).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewOffset {
    /// The window is bigger than the client: only part of it is shown (window_bigger).
    pub bigger: bool,
    pub ox: u16,
    pub oy: u16,
    /// Columns and rows of the window area on the client (the window size when it fits).
    pub sx: u16,
    pub sy: u16,
}

impl Server {
    /// Rows of the client taken by status lines (status_line_size), as composed.
    pub(super) fn client_status_rows(&self, cid: ClientId) -> u16 {
        let Some(c) = self.clients.get(&cid) else { return 0 };
        self.status_lines(c.session).min(c.size.1.max(1).saturating_sub(1))
    }

    /// The cursor of a pane's screen in its window, if the screen shows one (MODE_CURSOR):
    /// a mode's cursor, the tree modes and the clock have none.
    fn view_cursor(&self, wid: WinId, pane: PaneId) -> Option<(i64, i64)> {
        let p = self.panes.get(&pane)?;
        let (cx, cy) = match &p.mode {
            Some(PaneMode::Copy(cm)) => {
                let (x, y) = cm.cursor();
                (cm.cursor_offset(x), y)
            }
            Some(_) => return None,
            None if p.screen().hide_cursor() => return None,
            None => {
                let (y, x) = p.screen().cursor_position();
                (x as usize, y as usize)
            }
        };
        let (xoff, yoff) = match self.pane_geom(wid, pane).filter(|_| self.shown_floating(wid, pane)) {
            Some(g) => {
                let g = self.float_content(wid, pane, g);
                (g.x as i64, g.y as i64)
            }
            None => {
                let r = self.rects(wid).0.into_iter().find(|(id, _)| *id == pane).map(|(_, r)| self.content_rect(wid, r))?;
                (r.x as i64, r.y as i64)
            }
        };
        Some((xoff + cx as i64, yoff + cy as i64))
    }

    /// tty_window_offset1: a window that fits is drawn whole; a bigger one from where
    /// refresh-client panned it, or else so the active pane's cursor is visible.
    pub fn view_offset(&self, cid: ClientId) -> ViewOffset {
        let Some(c) = self.clients.get(&cid) else { return ViewOffset::default() };
        let Some(wid) = self.client_window(cid) else { return ViewOffset::default() };
        let Some(w) = self.windows.get(&wid) else { return ViewOffset::default() };
        let (tsx, tsy) = (c.size.0.max(1), c.size.1.max(1).saturating_sub(self.client_status_rows(cid)));
        // control clients have no terminal to show part of a window on
        if c.control > 0 || (tsx >= w.sx && tsy >= w.sy) {
            return ViewOffset { bigger: false, ox: 0, oy: 0, sx: w.sx, sy: w.sy };
        }
        let (sx, sy) = (tsx as i64, tsy as i64);
        let (wsx, wsy) = (w.sx as i64, w.sy as i64);
        if let Some((_, px, py)) = c.pan.filter(|p| p.0 == wid) {
            let ox = if sx >= wsx { 0 } else { (px as i64).min(wsx - sx) };
            let oy = if sy >= wsy { 0 } else { (py as i64).min(wsy - sy) };
            return ViewOffset { bigger: true, ox: ox as u16, oy: oy as u16, sx: tsx, sy: tsy };
        }
        let (ox, oy) = match self.view_cursor(wid, w.active) {
            None => (0, 0),
            Some((cx, cy)) => {
                let ox = if cx < sx { 0 } else if cx > wsx - sx { wsx - sx } else { cx - sx / 2 };
                let oy = if cy < sy { 0 } else if cy > wsy - sy { wsy - sy } else { cy - sy + 1 };
                (ox.max(0), oy.max(0))
            }
        };
        ViewOffset { bigger: true, ox: ox as u16, oy: oy as u16, sx: tsx, sy: tsy }
    }

    /// refresh-client -c (back to following the cursor) or -L/-R/-U/-D by `adjust`: the
    /// pan starts where the view is and stops at the window's edges.
    pub fn pan_client(&mut self, cid: ClientId, dir: char, adjust: u16) {
        let view = self.view_offset(cid);
        let wid = self.client_window(cid);
        let Some(w) = wid.and_then(|w| self.windows.get(&w)).map(|w| (w.id, w.sx, w.sy)) else { return };
        let Some(c) = self.clients.get_mut(&cid) else { return };
        if dir == 'c' {
            c.pan = None;
        } else {
            // a new pan starts where the view is (tty->oox, ooy); an old one is clamped
            let (mut px, mut py) = (view.ox, view.oy);
            match dir {
                'L' => px = px.saturating_sub(adjust),
                'R' => px = px.saturating_add(adjust).min(w.1.saturating_sub(view.sx)),
                'U' => py = py.saturating_sub(adjust),
                _ => py = py.saturating_add(adjust).min(w.2.saturating_sub(view.sy)),
            }
            c.pan = Some((w.0, px, py));
        }
        c.dirty = true;
        self.update_view(cid);
    }

    /// tty_update_client_offset: the pan is forgotten once the window fits or another
    /// window is shown, and kept within the window's edges.
    pub(super) fn update_view(&mut self, cid: ClientId) {
        let view = self.view_offset(cid);
        let wid = self.client_window(cid);
        if let Some(c) = self.clients.get_mut(&cid) {
            c.pan = match c.pan {
                Some((w, _, _)) if view.bigger && Some(w) == wid => Some((w, view.ox, view.oy)),
                _ => None,
            };
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::keys::{Mods, Mouse, MouseKind};

    /// A server with client 1 (cols x rows, writable) and, with `second`, client 2.
    pub(crate) fn clients(size: (u16, u16), second: Option<(u16, u16)>) -> (Server, Vec<mpsc::Receiver<Event>>) {
        let (mut srv, ev) = super::super::security_tests::fixture();
        let mut evs = vec![ev];
        if let Some(size) = second {
            // another fixture's client, renumbered
            let (mut other, ev) = super::super::security_tests::fixture();
            let mut c = other.clients.remove(&1).unwrap();
            c.id = 2;
            c.size = size;
            srv.clients.insert(2, c);
            evs.push(ev);
        }
        for c in srv.clients.values_mut() {
            c.readonly = false;
            c.acl_readonly = false;
        }
        srv.clients.get_mut(&1).unwrap().size = size;
        (srv, evs)
    }

    pub(crate) fn run(srv: &mut Server, cid: ClientId, args: &[&str]) -> Result<String, String> {
        let mut ctx = Ctx { client: Some(cid), ..Ctx::default() };
        match srv.exec(args.iter().map(|s| s.to_string()).collect(), &mut ctx) {
            Ok(_) => Ok(ctx.out.to_string()),
            Err(e) if e.starts_with('\0') => Ok(String::new()),
            Err(e) => Err(e),
        }
    }

    fn ok(srv: &mut Server, args: &[&str]) {
        run(srv, 1, args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
    }

    pub(crate) fn kill(srv: &mut Server) {
        for p in srv.panes.values_mut() {
            p.kill();
        }
    }

    fn row(srv: &Server, y: u16) -> String {
        let f = srv.compose(1).unwrap().frame;
        f.row(y).iter().map(|c| c.text.as_str()).collect()
    }

    fn vars(srv: &Server) -> String {
        let t = srv.target_for_session(srv.clients[&1].session.unwrap());
        srv.expand(t, Some(1), "#{window_bigger} #{window_offset_x} #{window_offset_y}")
    }

    /// A 70x15 client on a 100x20 window whose pane has written 30 numbered lines.
    fn bigger() -> (Server, Vec<mpsc::Receiver<Event>>, PaneId) {
        let (mut srv, evs) = clients((70, 15), None);
        ok(&mut srv, &["new-session", "-d", "-s", "s", "-x", "70", "-y", "14"]);
        let sid = *srv.sessions.keys().next().unwrap();
        srv.attach_client(1, sid);
        ok(&mut srv, &["set", "-g", "window-size", "manual"]);
        ok(&mut srv, &["resize-window", "-x", "100", "-y", "20"]);
        let pane = srv.client_active_pane(1).unwrap();
        let g = srv.panes[&pane].generation;
        let text: String = (1..=30).map(|i| format!("row{i:02} {}\r\n", "abcdefghij".repeat(9))).collect();
        srv.pane_data(pane, g, format!("\x1b[2J\x1b[H{text}").as_bytes());
        (srv, evs, pane)
    }

    #[test]
    fn a_bigger_window_follows_the_cursor_and_refresh_client_pans_it() {
        let (mut srv, _evs, _) = bigger();
        // tty_window_offset1: the cursor (row 19) is kept on the 14 rows shown
        assert_eq!(srv.view_offset(1), ViewOffset { bigger: true, ox: 0, oy: 6, sx: 70, sy: 14 });
        assert_eq!(vars(&srv), "1 0 6");
        assert!(row(&srv, 0).starts_with("row18 abc"), "{}", row(&srv, 0));
        ok(&mut srv, &["refresh-client", "-R", "15"]);
        assert_eq!(vars(&srv), "1 15 6");
        assert!(row(&srv, 0).starts_with("jabcdefghij"), "{}", row(&srv, 0));
        // the pan stops at the window's edges
        ok(&mut srv, &["refresh-client", "-R", "100"]);
        ok(&mut srv, &["refresh-client", "-U", "3"]);
        assert_eq!(vars(&srv), "1 30 3");
        ok(&mut srv, &["refresh-client", "-L", "7"]);
        ok(&mut srv, &["refresh-client", "-D"]);
        assert_eq!(vars(&srv), "1 23 4");
        ok(&mut srv, &["refresh-client", "-U", "100"]);
        ok(&mut srv, &["refresh-client", "-L", "100"]);
        assert_eq!(vars(&srv), "1 0 0");
        assert!(row(&srv, 0).starts_with("row12 abc"), "{}", row(&srv, 0));
        // -c follows the cursor again
        ok(&mut srv, &["refresh-client", "-c"]);
        assert_eq!(vars(&srv), "1 0 6");
        assert_eq!(run(&mut srv, 1, &["refresh-client", "-R", "0"]), Err("adjustment too small".into()));
        assert_eq!(run(&mut srv, 1, &["refresh-client", "-R", "x"]), Err("adjustment invalid".into()));
        // a pan is forgotten once the window fits
        ok(&mut srv, &["refresh-client", "-U", "2"]);
        ok(&mut srv, &["resize-window", "-x", "60", "-y", "10"]);
        srv.render(1);
        assert_eq!(vars(&srv), "0  ");
        ok(&mut srv, &["resize-window", "-x", "100", "-y", "20"]);
        assert_eq!(srv.view_offset(1).bigger, true);
        assert_ne!(srv.clients[&1].pan.map(|p| p.2), Some(4));
        kill(&mut srv);
    }

    #[test]
    fn a_panned_window_draws_its_borders_and_takes_mouse_events_where_it_is_shown() {
        let (mut srv, _evs, left) = bigger();
        ok(&mut srv, &["split-window", "-h", "-d"]);
        ok(&mut srv, &["set", "-g", "mouse", "on"]);
        let wid = srv.client_window(1).unwrap();
        let right = *srv.windows[&wid].panes().iter().find(|p| **p != left).unwrap();
        ok(&mut srv, &["refresh-client", "-R", "30"]);
        let ox = srv.view_offset(1).ox;
        assert_eq!(ox, 30);
        // the separator at window column 50 is client column 20
        let sep = srv.rects(wid).1[0].x;
        assert_eq!(srv.compose(1).unwrap().frame.get(sep - ox, 0).unwrap().text, "│");
        // a click at client column 25 is on window column 55: the right pane
        let click = |srv: &mut Server, kind| srv.dispatch_input(1, crate::keys::Input::Mouse(Mouse { kind, button: 1, x: 25, y: 2, mods: Mods::NONE }, vec![]));
        click(&mut srv, MouseKind::Down);
        click(&mut srv, MouseKind::Up);
        assert_eq!(srv.windows[&wid].active, right);
        kill(&mut srv);
    }

    #[test]
    fn windows_are_sized_for_the_clients_that_show_them() {
        let (mut srv, _evs) = clients((80, 24), Some((50, 12)));
        ok(&mut srv, &["new-session", "-d", "-s", "a", "-x", "80", "-y", "24", "-n", "w0"]);
        ok(&mut srv, &["new-window", "-d", "-t", "a:1", "-n", "w1"]);
        ok(&mut srv, &["new-window", "-d", "-t", "a:2", "-n", "w2"]);
        ok(&mut srv, &["new-session", "-d", "-s", "c", "-x", "80", "-y", "24", "-n", "c0"]);
        ok(&mut srv, &["link-window", "-d", "-s", "a:2", "-t", "c:5"]);
        let sid = |srv: &Server, n: &str| srv.sessions.values().find(|s| s.name == n).unwrap().id;
        let (a, c) = (sid(&srv, "a"), sid(&srv, "c"));
        srv.attach_client(1, a);
        srv.attach_client(2, c);
        // the client running select-window becomes the window's latest
        run(&mut srv, 2, &["select-window", "-t", "c:5"]).unwrap();
        let sizes = |srv: &Server| -> String {
            let w = |n: &str| srv.windows.values().find(|w| w.name == n).map(|w| format!("{}x{}", w.sx, w.sy)).unwrap();
            ["w0", "w1", "w2", "c0"].map(w).join(" ")
        };
        // w1 is no one's current window and keeps its size until it is
        // (server_client_check_window_resize); w2 follows its latest client
        assert_eq!(sizes(&srv), "80x23 80x24 50x11 50x11");
        ok(&mut srv, &["set", "-g", "window-size", "largest"]);
        assert_eq!(sizes(&srv), "80x23 80x24 80x23 50x11");
        ok(&mut srv, &["set", "-g", "window-size", "smallest"]);
        assert_eq!(sizes(&srv), "80x23 80x24 50x11 50x11");
        // aggressive-resize: only clients whose current window it is count
        ok(&mut srv, &["select-window", "-t", "a:2"]);
        run(&mut srv, 2, &["select-window", "-t", "c:0"]).unwrap();
        ok(&mut srv, &["set", "-g", "aggressive-resize", "on"]);
        assert_eq!(sizes(&srv), "80x23 80x24 80x23 50x11");
        ok(&mut srv, &["set", "-g", "aggressive-resize", "off"]);
        assert_eq!(sizes(&srv), "80x23 80x24 50x11 50x11");
        ok(&mut srv, &["select-window", "-t", "a:1"]);
        assert_eq!(sizes(&srv), "80x23 80x23 50x11 50x11");
        // latest with both clients on w2: the latest client's size
        ok(&mut srv, &["select-window", "-t", "a:2"]);
        run(&mut srv, 2, &["select-window", "-t", "c:5"]).unwrap();
        ok(&mut srv, &["set", "-g", "window-size", "latest"]);
        assert_eq!(sizes(&srv), "80x23 80x23 50x11 50x11");
        srv.update_latest(1);
        assert_eq!(sizes(&srv), "80x23 80x23 80x23 50x11");
        srv.update_latest(2);
        assert_eq!(sizes(&srv), "80x23 80x23 50x11 50x11");
        kill(&mut srv);
    }
}
