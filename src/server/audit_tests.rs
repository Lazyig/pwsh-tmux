//! tmux 3.8 behaviour found by the second audit: options, buffers, names, keys and
//! targets. Expected values are what tmux 3.8 does for the same input.
use super::*;

struct Fixture { srv: Server, sid: SessId, _events: mpsc::Receiver<Event> }
impl std::ops::Deref for Fixture { type Target = Server; fn deref(&self) -> &Server { &self.srv } }
impl std::ops::DerefMut for Fixture { fn deref_mut(&mut self) -> &mut Server { &mut self.srv } }
impl Drop for Fixture { fn drop(&mut self) { for p in self.srv.panes.values_mut() { p.kill(); } } }

/// Session "au" with window 0.
fn fixture() -> Fixture {
    let (mut srv, events) = super::security_tests::fixture();
    run(&mut srv, &["new-session", "-d", "-s", "au"]).unwrap();
    let sid = *srv.sessions.keys().next().unwrap();
    Fixture { srv, sid, _events: events }
}

fn run(srv: &mut Server, args: &[&str]) -> Result<String, String> {
    let mut ctx = Ctx::default();
    srv.exec(args.iter().map(|s| s.to_string()).collect(), &mut ctx).map(|_| ctx.out.to_string())
}

fn pane(f: &Fixture, idx: u32) -> PaneId {
    f.windows[&f.sessions[&f.sid].links[&idx]].active
}

fn output(f: &mut Fixture, idx: u32, data: &[u8]) {
    let p = pane(f, idx);
    let g = f.panes[&p].generation;
    f.pane_data(p, g, data);
}

fn fmt(f: &Fixture, s: &str) -> String {
    f.expand_for_pane(pane(f, 0), s)
}

#[test]
fn history_limit_applies_to_existing_panes() {
    let mut f = fixture();
    run(&mut f, &["set", "-g", "history-limit", "50"]).unwrap();
    let lines: String = (1..=300).map(|n| format!("{n}\r\n")).collect();
    output(&mut f, 0, lines.as_bytes());
    // grid_collect_history: a full history loses a tenth of its limit at once
    let rows = f.panes[&pane(&f, 0)].size.1 as usize;
    let pushed = 300 + 1 - rows;
    let expect = if pushed <= 50 { pushed } else { 45 + (pushed - 50) % 5 };
    assert_eq!(fmt(&f, "#{history_size} #{history_limit}"), format!("{expect} 50"));
    run(&mut f, &["set", "-g", "history-limit", "10"]).unwrap();
    assert_eq!(fmt(&f, "#{history_size} #{history_limit}"), "10 10");
    run(&mut f, &["set", "-g", "history-limit", "1000"]).unwrap();
    assert_eq!(fmt(&f, "#{history_size} #{history_limit}"), "10 1000");
    // a full history loses one line even to the same limit
    run(&mut f, &["set", "-g", "history-limit", "10"]).unwrap();
    run(&mut f, &["set", "-g", "history-limit", "10"]).unwrap();
    assert_eq!(fmt(&f, "#{history_size} #{history_limit}"), "9 10");
}
