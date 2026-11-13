//! tmux 3.7/3.8 interactive modes with an attached client: switch-mode, display-panes as a
//! mode, tree mode (help, -h, -k, filters), customize mode, command-prompt -P and menus.

mod common;
use common::*;
use std::time::Duration;

fn start(ctx: &Ctx, name: &str) -> Term {
    let t = Term::attach(ctx, &["new-session", "-s", name], 100, 40);
    t.wait_screen("status line", |s| s.contains(&format!("[{name}]")));
    ctx.wait_prompt(name);
    t
}

fn client(ctx: &Ctx) -> String {
    ctx.ok(&["list-clients", "-F", "#{client_name}"]).lines().next().unwrap_or("").to_string()
}

#[test]
fn switch_mode_filters_and_switches() {
    let ctx = Ctx::new("switch-mode");
    ctx.ok(&["new-session", "-d", "-s", "beta"]);
    ctx.ok(&["new-session", "-d", "-s", "gamma"]);
    let t = start(&ctx, "alpha");
    ctx.ok(&["switch-mode", "-t", "alpha"]);
    assert_eq!(ctx.fmt("alpha", "#{pane_mode}"), "switch-mode");
    // sessions by name, the prompt on the last line of the pane
    t.wait_screen("session list", |s| {
        let rows: Vec<&str> = s.lines().collect();
        rows.first().is_some_and(|r| r.starts_with("alpha ")) && rows.get(1).is_some_and(|r| r.starts_with("beta ")) && s.contains("(search)")
    });
    // typing narrows the list (fuzzy, best match first), Enter switches
    t.send(b"gm");
    t.wait_screen("filtered", |s| s.lines().next().is_some_and(|r| r.starts_with("gamma ")) && !s.contains("beta "));
    t.send(b"\r");
    wait_for("switched", || ctx.ok(&["list-clients", "-F", "#{client_session}"]).trim() == "gamma");
    assert_eq!(ctx.fmt("alpha", "#{pane_in_mode}"), "0");
    // windows with -w; Escape leaves the mode
    ctx.ok(&["rename-window", "-t", "gamma", "target-win"]);
    ctx.ok(&["switch-mode", "-w", "-t", "gamma"]);
    t.wait_screen("window list", |s| s.contains("target-win gamma:0"));
    t.send(b"\x1b");
    wait_for("escape", || ctx.fmt("gamma", "#{pane_in_mode}") == "0");
    // the default bindings open it in a new pane (Tab windows, BTab sessions)
    let keys = ctx.ok(&["list-keys", "-T", "prefix"]);
    assert!(keys.lines().any(|l| l.contains(" Tab ") && l.contains("switch-mode -kw")), "{keys}");
    assert!(keys.lines().any(|l| l.contains(" BTab ") && l.contains("switch-mode -ks")), "{keys}");
}

#[test]
fn display_panes_is_a_mode() {
    let ctx = Ctx::new("panes-mode");
    let t = start(&ctx, "p");
    ctx.ok(&["split-window", "-h", "-t", "p"]);
    ctx.ok(&["split-window", "-v", "-t", "p"]);
    // -b is gone
    assert!(ctx.fails(&["display-panes", "-b"]).contains("unknown flag -b"));
    // the mode zooms the pane and draws the window inside it with the sizes
    ctx.ok(&["display-panes", "-d", "0", "-t", "p.0"]);
    assert_eq!(ctx.fmt("p.0", "#{pane_mode}"), "panes-mode");
    assert_eq!(ctx.fmt("p", "#{window_zoomed_flag}"), "1");
    let w0 = ctx.fmt("p.0", "#{pane_unzoomed_width}");
    t.wait_screen("pane sizes", |s| s.contains(&format!("{w0}x")) && s.contains('├'));
    // a digit runs the template for that pane and unzooms
    t.send(b"2");
    wait_for("selected", || ctx.fmt("p", "#P") == "2");
    assert_eq!(ctx.fmt("p", "#{window_zoomed_flag}"), "0");
    assert_eq!(ctx.fmt("p.0", "#{pane_in_mode}"), "0");
    // -Z does not zoom, -N ignores keys except q, -d closes it after the delay
    // (keys go to the active pane, so the mode is put there)
    ctx.ok(&["display-panes", "-Z", "-N", "-d", "0", "-t", "p.2"]);
    assert_eq!(ctx.fmt("p", "#{window_zoomed_flag}"), "0");
    t.send(b"0");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(ctx.fmt("p.2", "#{pane_mode}"), "panes-mode");
    assert_eq!(ctx.fmt("p", "#P"), "2");
    t.send(b"q");
    wait_for("q leaves", || ctx.fmt("p.2", "#{pane_in_mode}") == "0");
    ctx.ok(&["display-panes", "-d", "300", "-t", "p.1"]);
    wait_for("timed out", || ctx.fmt("p.1", "#{pane_in_mode}") == "0");
    // a template; -k kills the pane when the mode exits
    ctx.ok(&["display-panes", "-d", "0", "-k", "-t", "p.2", "set -g @chosen '%%'"]);
    t.send(b"0");
    wait_for("template", || ctx.ok(&["show", "-gv", "@chosen"]).trim().starts_with('%'));
    wait_for("-k kills", || ctx.ok(&["list-panes", "-t", "p"]).lines().count() == 2);
}

#[test]
fn tree_mode_help_filter_hide_and_kill() {
    let ctx = Ctx::new("tree-38");
    let t = start(&ctx, "one");
    ctx.ok(&["rename-window", "-t", "one", "first"]);
    ctx.ok(&["split-window", "-h", "-t", "one"]);
    ctx.ok(&["new-window", "-d", "-t", "one", "-n", "second"]);
    // prefix and names like tmux 3.8: key, tree branches, expanded marker, name and text
    ctx.ok(&["choose-tree", "-Z", "-t", "one.1"]);
    assert_eq!(ctx.fmt("one.1", "#{pane_mode}"), "tree-mode");
    t.wait_screen("tree", |s| s.contains("(0) - one: 2 windows (attached)") && s.contains("(1) ├─→ - 0: ") && s.contains("(2) │   ├─→ 0: ") && s.contains("(4) └─→ - 1: second"));
    t.wait_screen("preview box", |s| s.contains("(sort: index) (view: preview)"));
    // C-h shows the help box, any key closes it
    t.send(b"\x08");
    t.wait_screen("help", |s| s.contains("Toggle preview") && s.contains("Exit mode"));
    t.send(b"x");
    t.wait_screen("help closed", |s| !s.contains("Toggle preview"));
    // a filter that matches nothing is ignored and says so
    t.send(b"f");
    t.wait_screen("filter prompt", |s| s.contains("(filter)"));
    t.send(b"#{==:#{window_name},none}\r");
    t.wait_screen("no matches", |s| s.contains("(filter: no matches)") && s.contains("second"));
    t.send(b"c");
    t.wait_screen("cleared", |s| !s.contains("(filter:"));
    // O changes the sort order
    t.send(b"O");
    t.wait_screen("sort", |s| s.contains("(sort: name)"));
    t.send(b"q");
    wait_for("closed", || ctx.fmt("one.1", "#{pane_in_mode}") == "0");
    // -f with more than one pane, -h hides the mode's pane, -k kills it on exit
    ctx.ok(&["choose-tree", "-h", "-k", "-f", "#{==:#{window_name},first}", "-t", "one.1"]);
    t.wait_screen("hidden pane", |s| s.contains("(1) └─→ - 0: first") && s.contains("(2)     └─→ 0: ") && !s.contains("(3)"));
    t.send(b"q");
    wait_for("-k kills", || ctx.ok(&["list-panes", "-t", "one:0"]).lines().count() == 1);
    // -y kills without asking; x asks first
    ctx.ok(&["choose-tree", "-w", "-t", "one:0"]);
    t.wait_screen("windows collapsed", |s| s.contains("+ 1: second"));
    t.send(b"\x1b[B");
    t.send(b"x");
    t.wait_screen("kill prompt", |s| s.contains("Kill window 1?"));
    t.send(b"y");
    wait_for("killed", || ctx.ok(&["list-windows", "-t", "one"]).lines().count() == 1);
    t.send(b"q");
}

#[test]
fn customize_mode_groups_changed_only_and_keys() {
    let ctx = Ctx::new("customize-38");
    let t = start(&ctx, "c");
    ctx.ok(&["set", "-g", "@mine", "value-1"]);
    ctx.ok(&["set-hook", "-g", "session-renamed", "set -g @renamed 1"]);
    ctx.ok(&["customize-mode", "-t", "c"]);
    assert_eq!(ctx.fmt("c", "#{pane_mode}"), "options-mode");
    t.wait_screen("groups", |s| {
        ["Server Options", "Session Options", "Window & Pane Options", "Session Hooks", "Window & Pane Hooks", "Global Environment", "Session Environment", "Key Table - prefix"]
            .iter()
            .all(|g| s.contains(g))
    });
    // C shows only changed items: the user option and the hook, not the environment
    t.send(b"C");
    t.wait_screen("changed only", |s| !s.contains("Global Environment") && s.contains("Session Options") && s.contains("Session Hooks"));
    // search finds the option and expands its group; s sets the value with a prompt in the pane
    t.send(b"\x13");
    t.wait_screen("search prompt", |s| s.contains("(search)"));
    t.send(b"@mine\r");
    t.wait_screen("user option", |s| s.contains("@mine") && s.contains("value-1"));
    t.send(b"s");
    t.wait_screen("set prompt", |s| s.contains("(@mine) value-1"));
    t.send(b"\x15value-2\r");
    wait_for("set", || ctx.ok(&["show", "-gv", "@mine"]).trim() == "value-2");
    // e edits the value with the editor option's program, in a popup
    #[cfg(unix)]
    {
        ctx.ok(&["set", "-s", "editor", "sed -i s/value-2/value-3/"]);
        t.send(b"e");
        wait_for("edited", || ctx.ok(&["show", "-gv", "@mine"]).trim() == "value-3");
    }
    t.send(b"C");
    t.wait_screen("all again", |s| s.contains("Server Options"));
    t.send(b"q");
    wait_for("closed", || ctx.fmt("c", "#{pane_in_mode}") == "0");
    // -k kills the pane when the mode exits
    ctx.ok(&["split-window", "-t", "c"]);
    ctx.ok(&["customize-mode", "-k", "-t", "c.1"]);
    t.wait_screen("customize", |s| s.contains("Server Options"));
    t.send(b"q");
    wait_for("-k", || ctx.ok(&["list-panes", "-t", "c"]).lines().count() == 1);
}

#[test]
fn pane_prompt_and_menus_on_all_clients() {
    let ctx = Ctx::new("prompt-menu");
    let t = start(&ctx, "m");
    ctx.ok(&["split-window", "-h", "-t", "m"]);
    let left: usize = ctx.fmt("m.1", "#{pane_left}").parse().unwrap();
    let pane_part = |row: String| row.chars().skip(left).collect::<String>();
    // command-prompt -P draws the prompt over the last line of the pane
    ctx.ok(&["bind-key", "-n", "F5", "command-prompt", "-P", "-p", "name", "rename-window '%%'"]);
    t.send(b"\x1b[15~");
    t.wait_screen("pane prompt", |_| pane_part(t.row(38)).starts_with("name"));
    assert!(!t.row(38).chars().take(left).collect::<String>().contains("name"));
    assert!(!t.row(39).contains("name"), "{}", t.row(39));
    t.send(b"pp\r");
    wait_for("renamed", || ctx.fmt("m", "#W") == "pp");
    // a menu belongs to the window: a second client sees it and can choose from it
    let c1 = client(&ctx);
    let t2 = Term::attach(&ctx, &["attach", "-t", "m"], 100, 40);
    t2.wait_screen("second client", |s| s.contains("[m]"));
    let mut menu = ctx.command().args(["display-menu", "-c", &c1, "-x", "2", "-y", "2", "-T", "Title", "One", "1", "set -g @menu one", "Two", "2", "set -g @menu two"]).spawn().unwrap();
    t.wait_screen("menu", |s| s.contains("One (1)"));
    t2.wait_screen("menu on the other client", |s| s.contains("One (1)") && s.contains("Two (2)"));
    t2.send(b"2");
    wait_for("chosen on the other client", || ctx.ok(&["show", "-gv", "@menu"]).trim() == "two");
    t.wait_screen("closed everywhere", |s| !s.contains("One (1)"));
    assert!(menu.wait().unwrap().success());
}

#[test]
fn customize_mode_empty_list_keeps_screen_and_any_key_exits() {
    let ctx = Ctx::new("customize-empty");
    let t = start(&ctx, "e");
    // nothing changed from the defaults (default-shell is set from $SHELL at startup)
    ctx.ok(&["set", "-gu", "default-shell"]);
    // a filter that matches nothing leaves the groups without items
    ctx.ok(&["customize-mode", "-f", "#{==:#{option_name},nosuch}", "-t", "e"]);
    t.wait_screen("groups", |s| s.contains("(0)   Server Options") && s.contains("(1)   Session Options"));
    // C with nothing changed empties the list: tmux does not redraw, the old items stay
    t.send(b"C");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(ctx.fmt("e", "#{pane_mode}"), "options-mode");
    assert!(t.contents().contains("(0)   Server Options"), "{}", t.contents());
    // with an empty list any key leaves the mode
    t.send(b"j");
    wait_for("exited", || ctx.fmt("e", "#{pane_in_mode}") == "0");
}

#[test]
fn modes_stack_and_leaving_one_restores_the_one_below() {
    let ctx = Ctx::new("mode-stack");
    let t = start(&ctx, "st");
    ctx.ok(&["choose-tree", "-t", "st"]);
    ctx.ok(&["setb", "hello"]);
    ctx.ok(&["choose-buffer", "-t", "st"]);
    assert_eq!(ctx.fmt("st", "#{pane_mode}"), "buffer-mode");
    t.send(b"q");
    wait_for("tree mode again", || ctx.fmt("st", "#{pane_mode}") == "tree-mode");
    t.wait_screen("tree list", |s| s.contains("(0)") && s.contains("st:"));
    // choose-tree with tree mode on top does nothing; panes mode stacks too
    ctx.ok(&["choose-tree", "-t", "st"]);
    ctx.ok(&["display-panes", "-d", "0", "-t", "st"]);
    assert_eq!(ctx.fmt("st", "#{pane_mode}"), "panes-mode");
    t.send(b"q");
    wait_for("tree after panes", || ctx.fmt("st", "#{pane_mode}") == "tree-mode");
    // panes mode does not stack under another mode (WINDOW_MODE_NO_STACK)
    ctx.ok(&["display-panes", "-d", "0", "-t", "st"]);
    ctx.ok(&["switch-mode", "-t", "st"]);
    t.send(b"\x1b");
    wait_for("tree after switch", || ctx.fmt("st", "#{pane_mode}") == "tree-mode");
    // a mode already in the stack is raised instead of opened again
    ctx.ok(&["clock-mode", "-t", "st"]);
    ctx.ok(&["choose-tree", "-t", "st"]);
    assert_eq!(ctx.fmt("st", "#{pane_mode}"), "tree-mode");
    t.send(b"q");
    wait_for("clock below", || ctx.fmt("st", "#{pane_mode}") == "clock-mode");
    // copy-mode -q leaves all of them
    ctx.ok(&["choose-tree", "-t", "st"]);
    ctx.ok(&["copy-mode", "-q", "-t", "st"]);
    assert_eq!(ctx.fmt("st", "#{pane_in_mode}"), "0");
}

#[test]
fn send_keys_reach_tree_like_modes_only_with_a_client() {
    let ctx = Ctx::new("mode-noclient");
    ctx.ok(&["new-session", "-d", "-s", "b"]);
    ctx.ok(&["switch-mode", "-t", "b"]);
    ctx.ok(&["send-keys", "-t", "b", "Escape"]);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(ctx.fmt("b", "#{pane_mode}"), "switch-mode");
    // copy mode has a key table, so its keys work without a client
    ctx.ok(&["copy-mode", "-t", "b"]);
    ctx.ok(&["send-keys", "-t", "b", "q"]);
    wait_for("copy mode left", || ctx.fmt("b", "#{pane_mode}") == "switch-mode");
}

#[test]
fn menus_belong_to_the_window_and_do_not_block() {
    let ctx = Ctx::new("window-menu");
    let t = start(&ctx, "w");
    ctx.ok(&["new-window", "-d", "-t", "w"]);
    let c1 = client(&ctx);
    // display-menu returns at once (3.8) and the rest of the list runs
    ctx.ok(&["display-menu", "-c", &c1, "-x", "2", "-y", "10", "-T", "T1", "One", "1", "set -g @m one", ";", "set", "-g", "@after", "yes"]);
    assert_eq!(ctx.ok(&["show", "-gv", "@after"]).trim(), "yes");
    t.wait_screen("menu", |s| s.contains("One (1)"));
    // a second menu on the window replaces the first
    ctx.ok(&["display-menu", "-c", &c1, "-x", "2", "-y", "10", "-T", "T2", "Two", "2", "set -g @m two"]);
    t.wait_screen("second menu", |s| s.contains("Two (2)") && !s.contains("One (1)"));
    // it stays with its window
    ctx.ok(&["select-window", "-t", "w:1"]);
    t.wait_screen("no menu on window 1", |s| !s.contains("Two (2)"));
    ctx.ok(&["select-window", "-t", "w:0"]);
    t.wait_screen("menu again", |s| s.contains("Two (2)"));
    t.send(b"2");
    wait_for("chosen", || ctx.ok(&["show", "-gv", "@m"]).trim() == "two");
    // bad -b and -C values
    assert!(ctx.fails(&["display-menu", "-c", &c1, "-b", "bogus", "a", "a", ""]).contains("menu-border-lines unknown value: bogus"));
    assert!(ctx.fails(&["display-menu", "-c", &c1, "-C", "9x", "a", "a", ""]).contains("starting choice invalid"));
}

#[test]
fn choose_buffer_and_tree_sort_like_sort_c() {
    let ctx = Ctx::new("mode-sort");
    let t = start(&ctx, "so");
    ctx.ok(&["setb", "-b", "zz", "short"]);
    ctx.ok(&["setb", "-b", "aa", "a much longer buffer"]);
    ctx.ok(&["setb", "-b", "mm", "mid size"]);
    let order = |s: &str| -> Vec<String> { s.lines().filter_map(|l| l.split(") ").nth(1)).filter_map(|r| r.split(':').next()).map(String::from).take(3).collect() };
    for (o, want) in [("size", ["zz", "mm", "aa"]), ("index", ["aa", "mm", "zz"]), ("order", ["mm", "aa", "zz"]), ("creation", ["mm", "aa", "zz"])] {
        ctx.ok(&["choose-buffer", "-O", o, "-N", "-t", "so"]);
        t.wait_screen(o, |s| order(s) == want);
        t.send(b"q");
        wait_for("left", || ctx.fmt("so", "#{pane_in_mode}") == "0");
    }
    // tree mode keeps -O creation, size, order and modifier
    for o in ["creation", "size", "order", "modifier"] {
        ctx.ok(&["choose-tree", "-O", o, "-t", "so"]);
        t.wait_screen(o, |s| s.contains(&format!("(sort: {o})")));
        t.send(b"q");
        wait_for("left", || ctx.fmt("so", "#{pane_in_mode}") == "0");
    }
}
