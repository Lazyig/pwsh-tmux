//! Opt-in UI recorder. Completion means captures were written, not that UIs match.
//! cargo test --test ui_compat -- --ignored --nocapture
//! Compare/render its JSON files with tools/ui_report.py.
mod common;
use common::Term;
use std::io::{BufRead, BufReader, Write};
use portable_pty::{CommandBuilder, PtySize};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""), '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"), '\r' => out.push_str("\\r"), '\t' => out.push_str("\\t"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"'); out
}

fn color(c: vt100::Color) -> String {
    match c {
        vt100::Color::Default => "default".into(),
        vt100::Color::Idx(i) | vt100::Color::Palette(i) => format!("idx:{i}"),
        vt100::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

struct App {
    role: &'static str,
    prefix: Vec<String>,
    socket: String,
    config: String,
    fixture: String,
    term: Option<Term>,
    pending: Vec<Child>,
    errors: Vec<String>,
    captures: Vec<String>,
    directory: PathBuf,
    broker: Option<Broker>,
}

/// The WSL (/mnt/<drive>/...) form of a Windows path, for the reference side.
fn wsl_path(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    match s.as_bytes() {
        [d, b':', rest @ ..] => format!("/mnt/{}{}", (*d as char).to_ascii_lowercase(), String::from_utf8_lossy(rest)),
        _ => s,
    }
}

struct Broker { child: Child, input: std::process::ChildStdin, output: BufReader<std::process::ChildStdout> }
impl Broker {
    fn new() -> Self {
        let mut child = Command::new("C:/Windows/System32/wsl.exe").args(["-d", "Ubuntu", "--exec", "python3", &wsl_path(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ui_broker.py"))])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
        Self { input: child.stdin.take().unwrap(), output: BufReader::new(child.stdout.take().unwrap()), child }
    }
    fn request(&mut self, operation: &str, args: &[String]) -> (i32, String, String) {
        let encoded: String = args.join("\0").bytes().map(|b| format!("{b:02x}")).collect();
        writeln!(self.input, "{operation} {encoded}").unwrap(); self.input.flush().unwrap();
        let mut line = String::new(); assert!(self.output.read_line(&mut line).unwrap() > 0, "reference broker closed");
        let mut parts = line.trim_end_matches(['\r', '\n']).split(' ');
        let code = parts.next().unwrap().parse().unwrap();
        let decode = |s: &str| String::from_utf8_lossy(&(0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i+2],16).unwrap()).collect::<Vec<_>>()).into_owned();
        (code, decode(parts.next().unwrap_or("")), decode(parts.next().unwrap_or("")))
    }
}
impl Drop for Broker { fn drop(&mut self) { let _ = writeln!(self.input, "CLOSE "); let _ = self.input.flush(); let _ = self.child.wait(); } }

impl App {
    fn new(role: &'static str, directory: &Path) -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let is_candidate = role == "candidate";
        let prefix = if is_candidate {
            vec![std::env::var("PT_UI_CANDIDATE").unwrap_or_else(|_| common::binary().to_string_lossy().into_owned())]
        } else if cfg!(windows) {
            vec!["C:/Windows/System32/wsl.exe".into(), "-d".into(), "Ubuntu".into(), "--exec".into(),
                "env".into(), "TERM=xterm-256color".into(), "LC_ALL=C.UTF-8".into(),
                std::env::var("PT_UI_REFERENCE").unwrap_or_else(|_| wsl_path(&root.join("target/tmux-reference/tmux")))]
        } else {
            vec![std::env::var("PT_UI_REFERENCE").unwrap_or_else(|_| root.join("target/tmux-reference/tmux").to_string_lossy().into_owned())]
        };
        let mut fixture = if cfg!(windows) && is_candidate {
            format!("python {}", root.join("tests/ui_fixture.py").display())
        } else if cfg!(windows) {
            format!("python3 {}", wsl_path(&root.join("tests/ui_fixture.py")))
        } else { format!("python3 {}", root.join("tests/ui_fixture.py").display()) };
        if let Ok(suffix) = std::env::var("PT_UI_FIXTURE_SUFFIX") { fixture.push(' '); fixture.push_str(&suffix); }
        Self {
            role, prefix, fixture, directory: directory.into(),
            socket: format!("ui-{role}-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()),
            config: if cfg!(windows) && is_candidate { "NUL" } else { "/dev/null" }.into(),
            term: None, pending: Vec::new(), errors: Vec::new(), captures: Vec::new(),
            broker: if cfg!(windows) && !is_candidate { Some(Broker::new()) } else { None },
        }
    }

    fn arguments(&self, args: &[&str]) -> Vec<String> {
        self.prefix.iter().skip(1).cloned().chain(["-L".into(), self.socket.clone(), "-f".into(), self.config.clone()])
            .chain(args.iter().map(|s| s.to_string())).collect()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.prefix[0]); c.args(self.arguments(args));
        for key in ["TMUX", "TMUX_PANE", "PWSH_TMUX", "PWSH_TMUX_PANE", "PWSH_TMUX_SOCKET", "PWSH_TMUX_SOCKET_PATH", "COLORTERM"] { c.env_remove(key); }
        c.env("TERM", "xterm-256color").env("LC_ALL", "C.UTF-8").env("PYTHONIOENCODING", "utf-8"); c
    }

    fn run(&mut self, args: &[&str]) -> String {
        if self.broker.is_some() {
            let command: Vec<String> = [self.prefix.last().unwrap().clone(), "-L".into(), self.socket.clone(), "-f".into(), self.config.clone()]
                .into_iter().chain(args.iter().map(|s| s.to_string())).collect();
            let (code, stdout, stderr) = self.broker.as_mut().unwrap().request("RUN", &command);
            if code != 0 { self.errors.push(format!("{args:?}: {stderr}")); }
            return stdout.trim_end_matches(['\r','\n']).into();
        }
        let output = self.command(args).output().expect("comparison CLI");
        if !output.status.success() {
            self.errors.push(format!("{args:?}: {}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)));
        }
        String::from_utf8_lossy(&output.stdout).trim_end_matches(['\r', '\n']).into()
    }

    fn start(&mut self) {
        let fixture = self.fixture.clone();
        assert!(self.run(&["new-session", "-d", "-s", "ui", "-n", "main", "-x", "80", "-y", "24", &fixture]).is_empty());
        self.run(&["set-option", "-g", "default-command", &fixture]);
        self.run(&["set-option", "-gw", "automatic-rename", "off"]);
        // Only wall-clock output is fixed; pane title and status layout remain tested.
        self.run(&["set-option", "-g", "status-right", "\"#{=21:pane_title}\" 12:34 31-Oct-26"]);
        self.run(&["set-option", "-g", "display-time", "1000"]);
        self.run(&["set-option", "-g", "display-panes-time", "5000"]);
        let mut cb = CommandBuilder::new(&self.prefix[0]); cb.args(self.arguments(&["attach-session", "-t", "ui"]));
        for key in ["TMUX", "TMUX_PANE", "PWSH_TMUX", "PWSH_TMUX_PANE", "PWSH_TMUX_SOCKET", "PWSH_TMUX_SOCKET_PATH", "COLORTERM"] { cb.env_remove(key); }
        cb.env("TERM", "xterm-256color"); cb.env("LC_ALL", "C.UTF-8");
        let term = Term::spawn(cb, 80, 24);
        term.wait_screen("fixture and status", |s| s.contains("PANE FIXTURE") && s.contains("[ui]"));
        self.term = Some(term);
    }

    fn asynchronous(&mut self, args: &[&str]) {
        if let Some(broker) = self.broker.as_mut() {
            let command: Vec<String> = [self.prefix.last().unwrap().clone(), "-L".into(), self.socket.clone(), "-f".into(), self.config.clone()]
                .into_iter().chain(args.iter().map(|s| s.to_string())).collect();
            assert_eq!(broker.request("START", &command).0, 0); return;
        }
        let child = self.command(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        self.pending.push(child);
    }

    fn snapshot(&mut self, name: &str) {
        let state = self.run(&["display-message", "-p", "-t", "ui", "#{session_name}|#{window_index}|#{window_name}|#{pane_index}|#{window_panes}|#{window_zoomed_flag}|#{pane_in_mode}|#{window_width}x#{window_height}|#{copy_cursor_x},#{copy_cursor_y}"]);
        let menu = self.run(&["show-options", "-gqv", "@ui-menu"]);
        let copied = self.run(&["show-options", "-gqv", "@ui-copy"]);
        let pending = if let Some(broker) = self.broker.as_mut() { broker.request("PENDING", &[]).1.parse().unwrap() }
            else { self.pending.iter_mut().map(|c| c.try_wait().unwrap().is_none()).filter(|waiting| *waiting).count() };
        let term = self.term.as_ref().unwrap();
        let guard = term.screen.lock().unwrap(); let screen = guard.screen();
        let (rows, cols) = screen.size(); let (rows, cols) = (rows.get(), cols.get());
        let mut cells = Vec::new();
        for y in 0..rows {
            let mut row = Vec::new();
            for x in 0..cols {
                let c = screen.cell(y, x).unwrap();
                let text = if c.is_wide_continuation() { "" } else if c.has_contents() { c.contents() } else { " " };
                let attrs = (c.bold() as u8) | ((c.dim() as u8) << 1) | ((c.italic() as u8) << 2) | ((c.underline() as u8) << 3) | ((c.inverse() as u8) << 4);
                row.push(format!("[{},{},{},{}]", quote(text), quote(&color(c.fgcolor())), quote(&color(c.bgcolor())), attrs));
            }
            cells.push(format!("[{}]", row.join(",")));
        }
        let (cy, cx) = screen.cursor_position();
        let value = format!("{{\"name\":{},\"width\":{cols},\"height\":{rows},\"cells\":[{}],\"cursor\":{{\"x\":{cx},\"y\":{cy},\"visible\":{},\"style\":{}}},\"modes\":{{\"mouse\":{},\"encoding\":{},\"application_cursor\":{},\"bracketed_paste\":{}}},\"state\":{},\"menu_result\":{},\"copy_result\":{},\"pending_cli\":{pending},\"errors\":[{}]}}",
            quote(name), cells.join(","), !screen.hide_cursor(), quote(&format!("{:?}", screen.cursor_style())),
            quote(&format!("{:?}", screen.mouse_protocol_mode())), quote(&format!("{:?}", screen.mouse_protocol_encoding())), screen.application_cursor(), screen.bracketed_paste(), quote(&state), quote(&menu), quote(&copied), self.errors.iter().map(|e| quote(e)).collect::<Vec<_>>().join(","));
        drop(guard);
        self.captures.push(value); self.errors.clear();
        std::fs::write(self.directory.join(format!("{}.json", self.role)), format!("{{\"role\":{},\"platform\":{},\"terminal\":\"xterm-256color\",\"cases\":[{}]}}", quote(self.role), quote(std::env::consts::OS), self.captures.join(","))).unwrap();
        std::fs::write(self.directory.join(format!("{}-latest.ansi", self.role)), term.raw.lock().unwrap().as_slice()).unwrap();
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.run(&["kill-server"]);
        for child in &mut self.pending { let _ = child.kill(); let _ = child.wait(); }
    }
}

struct Pair { reference: App, candidate: App }
impl Pair {
    fn both(&mut self, args: &[&str]) { self.reference.run(args); self.candidate.run(args); }
    fn key(&self, bytes: &[u8]) { self.reference.term.as_ref().unwrap().send(bytes); self.candidate.term.as_ref().unwrap().send(bytes); }
    fn prefix(&self, bytes: &[u8]) { self.key(b"\x02"); self.key(bytes); }
    fn mouse(&self, button: u8, x: u16, y: u16, release: bool) {
        self.reference.term.as_ref().unwrap().mouse(button,x,y,release);
        self.candidate.term.as_ref().unwrap().mouse(button,x,y,release);
    }
    fn capture(&mut self, name: &str) {
        std::thread::sleep(Duration::from_millis(250));
        self.reference.snapshot(name); self.candidate.snapshot(name); println!("captured {name}");
    }
    fn resize(&mut self, cols: u16, rows: u16) {
        for app in [&mut self.reference, &mut self.candidate] {
            let t = app.term.as_mut().unwrap();
            t.screen.lock().unwrap().set_size(rows.try_into().unwrap(), cols.try_into().unwrap());
            t.master.as_ref().unwrap().resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).unwrap();
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    fn overlay(&mut self, args: &[&str]) { self.reference.asynchronous(args); self.candidate.asynchronous(args); }
}

#[test]
#[ignore = "records real tmux and candidate UIs; inspect tools/ui_report.py results"]
fn record_terminal_ui_comparison() {
    let out = std::env::var_os("PT_UI_OUT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/ui-comparison").join(std::env::consts::OS));
    std::fs::create_dir_all(&out).unwrap();
    let mut pair = Pair { reference: App::new("reference", &out), candidate: App::new("candidate", &out) };
    pair.reference.start(); pair.candidate.start();
    pair.capture("01-default-80x24");
    if std::env::var_os("PT_UI_BASELINE_ONLY").is_some() { return; }
    pair.prefix(b":"); pair.capture("02-command-prompt-empty");
    pair.key(b"display-message hello"); pair.capture("03-command-prompt-typed");
    pair.key(b"\x15"); pair.key(&vec![b'x'; 120]); pair.capture("04-command-prompt-overflow");
    pair.key(b"\x01"); pair.capture("05-command-prompt-home");
    pair.key(b"\x1b"); std::thread::sleep(Duration::from_millis(400));
    pair.prefix(b","); pair.capture("06-rename-prompt-prefill");
    pair.key(b"\x15renamed\r"); pair.capture("07-rename-result"); pair.both(&["rename-window", "-t", "ui:0", "main"]);
    pair.both(&["display-message", "-d", "1000", "hello UI message"]); pair.capture("08-status-message");
    std::thread::sleep(Duration::from_millis(1200));
    pair.both(&["set-option", "-g", "mouse", "on"]); pair.capture("09-mouse-mode");
    pair.prefix(b"%"); std::thread::sleep(Duration::from_millis(400)); pair.capture("10-split-horizontal");
    pair.prefix(b"z"); pair.capture("11-zoom"); pair.prefix(b"z");
    pair.prefix(b"\x1b[D"); std::thread::sleep(Duration::from_millis(600)); pair.capture("12-select-left-key");
    pair.prefix(b"\""); std::thread::sleep(Duration::from_millis(400)); pair.capture("13-split-junction");
    pair.mouse(0,60,5,false); pair.mouse(0,60,5,true); pair.capture("13a-mouse-select-right");
    let reference_layout = pair.reference.run(&["display-message", "-p", "-t", "ui", "#{window_layout}"]);
    let candidate_layout = pair.candidate.run(&["display-message", "-p", "-t", "ui", "#{window_layout}"]);
    pair.mouse(0,40,5,false); pair.mouse(32,44,5,false); pair.mouse(0,44,5,true); pair.capture("13b-mouse-drag-border");
    pair.reference.run(&["select-layout", "-t", "ui:0", &reference_layout]);
    pair.candidate.run(&["select-layout", "-t", "ui:0", &candidate_layout]);
    std::thread::sleep(Duration::from_millis(500));
    // Resize redraws are asynchronous and can add a different number of old
    // fixture frames to history. Test copy UI with a known history independently
    // of application scheduling; cells.rs covers reflow and clear preservation.
    pair.both(&["clear-history", "-t", "ui:0.2"]);
    pair.both(&["select-pane", "-t", "ui:0.2"]); pair.mouse(64,60,5,false); pair.capture("13c-mouse-wheel-copy"); pair.key(b"q");
    pair.both(&["select-pane", "-t", "ui:0.1"]);
    for preset in ["single", "double", "heavy", "simple", "number"] {
        pair.both(&["set-window-option", "-t", "ui:0", "pane-border-lines", preset]); pair.capture(&format!("14-border-{preset}"));
    }
    pair.both(&["set-window-option", "-t", "ui:0", "pane-border-lines", "single"]);
    pair.both(&["set-window-option", "-t", "ui:0", "pane-border-status", "top"]); pair.capture("15-pane-border-title-top");
    pair.both(&["set-window-option", "-t", "ui:0", "pane-border-status", "bottom"]); pair.capture("16-pane-border-title-bottom");
    pair.both(&["set-window-option", "-t", "ui:0", "pane-border-status", "off"]);
    pair.overlay(&["display-panes", "-d", "5000"]); pair.capture("17-display-pane-numbers"); pair.key(b"0"); pair.capture("18-pane-number-selection");
    pair.overlay(&["display-menu", "-T", "Actions", "-x", "C", "-y", "C", "--", "First", "a", "set-option -g @ui-menu first", "", "-Disabled", "d", "", "Last", "l", "set-option -g @ui-menu last"]);
    pair.capture("19-custom-menu"); pair.key(b"\x1b[B"); pair.capture("20-menu-arrow-selection"); pair.key(b"l"); pair.capture("21-menu-key-action");
    pair.prefix(b">"); pair.capture("22-default-pane-menu"); pair.key(b"\x1b"); std::thread::sleep(Duration::from_millis(300));
    for app in [&mut pair.reference, &mut pair.candidate] { let fixture=app.fixture.clone(); app.asynchronous(&["display-popup", "-w", "40", "-h", "10", "-T", "Popup", &fixture]); }
    std::thread::sleep(Duration::from_millis(500)); pair.capture("23-popup-centred"); pair.both(&["display-popup", "-C"]);
    pair.prefix(b"z"); pair.capture("24-copy-fixture-zoom");
    pair.both(&["clear-history", "-t", "ui:0.0"]);
    pair.prefix(b"["); pair.capture("25-copy-mode-emacs");
    pair.key(b"\x12"); pair.key(b"needle"); pair.capture("26-copy-incremental-search"); pair.key(b"\x07"); pair.capture("26a-copy-search-cancel");
    // Establish the same selection anchor, then use actual emacs selection/copy keys.
    pair.both(&["send-keys", "-X", "-t", "ui:0.0", "history-bottom"]);
    pair.both(&["send-keys", "-X", "-t", "ui:0.0", "search-backward", "first"]);
    pair.both(&["send-keys", "-X", "-t", "ui:0.0", "start-of-line"]);
    pair.key(b"\0"); for _ in 0..3 { pair.key(b"\x1b[C"); } pair.capture("27-copy-selection"); pair.key(b"\x1bw");
    for app in [&mut pair.reference,&mut pair.candidate] { let copied=app.run(&["show-buffer"]); app.run(&["set-option", "-g", "@ui-copy", &copied]); }
    pair.capture("27a-copy-result");
    pair.both(&["set-window-option", "-t", "ui:0", "mode-keys", "vi"]); pair.prefix(b"["); pair.key(b"?"); pair.key(b"needle"); pair.capture("28-copy-vi-search-prompt");
    pair.key(b"\r"); pair.capture("29-copy-vi-search-result"); pair.key(b"q");
    pair.prefix(b"t"); pair.capture("30-clock-mode"); pair.key(b"q");
    pair.prefix(b"c"); std::thread::sleep(Duration::from_millis(400)); pair.both(&["rename-window", "-t", "ui:1", "second"]); pair.capture("31-new-window");
    pair.mouse(0,6,23,false); pair.mouse(0,6,23,true); pair.capture("31a-mouse-status-window"); pair.both(&["select-window", "-t", "ui:1"]);
    pair.prefix(b"p"); pair.capture("32-previous-window");
    pair.prefix(b"w"); pair.capture("33-choose-tree-default"); pair.key(b"\x1b[B"); pair.capture("34-choose-tree-move"); pair.key(b"q");
    pair.both(&["set-buffer", "-b", "sample", "first line\nsecond line\n"]); pair.prefix(b"="); pair.capture("35-choose-buffer"); pair.key(b"q");
    for i in 2..7 { pair.both(&["new-window", "-d", "-t", "ui", "-n", &format!("window-{i}-long")]); }
    pair.both(&["select-window", "-t", "ui:6"]); std::thread::sleep(Duration::from_millis(400)); pair.capture("36-status-window-overflow");
    pair.both(&["set-option", "-g", "status-justify", "centre"]); pair.capture("37-status-centre");
    pair.resize(40, 12); pair.capture("38-small-40x12");
    pair.resize(20, 6); pair.capture("39-tiny-20x6");
    pair.resize(100, 30); pair.capture("40-large-100x30");
    pair.both(&["set-option", "-g", "status-position", "top"]); pair.capture("41-status-at-top");
    pair.both(&["set-option", "-g", "status", "2"]);
    pair.both(&["set-option", "-g", "status-format[0]", "#[align=left]LEFT#[align=centre]CENTRE#[align=right]RIGHT"]);
    pair.both(&["set-option", "-g", "status-format[1]", "#[fg=yellow,bg=blue]SECOND LINE"]); pair.capture("42-status-two-lines");
    println!("UI captures written to {}. This recorder does not assert UI equality.", out.display());
}
