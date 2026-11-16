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
    // the default bindings open it in a new pane (Tab windows, BTab sessions); list-keys
    // prints them in canonical form once new-pane (floating panes) exists
    let keys = ctx.ok(&["list-keys", "-T", "prefix"]);
    let floating = !ctx.ok(&["list-commands", "new-pane"]).trim().is_empty();
    let (w, s) = if floating { ("switch-mode -kw", "switch-mode -ks") } else { ("switch-mode -wk", "switch-mode -sk") };
    assert!(keys.lines().any(|l| l.contains(" Tab ") && l.contains(w)), "{keys}");
    assert!(keys.lines().any(|l| l.contains(" BTab ") && l.contains(s)), "{keys}");
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
    // s on a global session option sets it for the mode's session (window_customize_set_option)
    t.send(b"s");
    t.wait_screen("set prompt", |s| s.contains("(@mine, for session c) value-1"));
    t.send(b"\x15value-2\r");
    wait_for("set", || ctx.ok(&["show", "-v", "-t", "c", "@mine"]).trim() == "value-2");
    assert_eq!(ctx.ok(&["show", "-gv", "@mine"]).trim(), "value-1");
    // e edits the value with the editor option's program, in a popup
    #[cfg(unix)]
    {
        ctx.ok(&["set", "-s", "editor", "sed -i s/value-2/value-3/"]);
        t.send(b"e");
        wait_for("edited", || ctx.ok(&["show", "-v", "-t", "c", "@mine"]).trim() == "value-3");
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

#[test]
fn switch_mode_builds_the_list_again_as_keys_are_typed() {
    let ctx = Ctx::new("switch-rebuild");
    ctx.ok(&["new-session", "-d", "-s", "beta"]);
    let t = start(&ctx, "alpha");
    ctx.ok(&["switch-mode", "-t", "alpha"]);
    t.wait_screen("list", |s| s.lines().any(|l| l.starts_with("beta ")));
    ctx.ok(&["new-session", "-d", "-s", "gamma"]);
    ctx.ok(&["rename-session", "-t", "beta", "delta"]);
    t.send(b"a");
    t.wait_screen("rebuilt", |s| s.lines().any(|l| l.starts_with("gamma ")) && s.lines().any(|l| l.starts_with("delta ")));
    t.send(b"\x7fde\r");
    wait_for("switched", || ctx.ok(&["list-clients", "-F", "#{client_session}"]).trim() == "delta");
}

#[test]
fn mode_choices_apply_to_the_client_that_pressed_the_key() {
    let ctx = Ctx::new("mode-presser");
    ctx.ok(&["new-session", "-d", "-s", "beta"]);
    let a = start(&ctx, "alpha");
    let ca = client(&ctx);
    let b = Term::attach(&ctx, &["attach", "-t", "alpha"], 100, 40);
    b.wait_screen("second client", |s| s.contains("[alpha]"));
    a.send(b"\x02s");
    b.wait_screen("tree", |s| s.contains("(1)") && s.contains("beta"));
    // the tree starts on alpha's window; Down goes to beta
    b.send(b"\x1b[B");
    std::thread::sleep(Duration::from_millis(200));
    b.send(b"\r");
    wait_for("b switched", || {
        let l = ctx.ok(&["list-clients", "-F", "#{client_name} #{client_session}"]);
        l.lines().any(|x| x == format!("{ca} alpha")) && l.lines().any(|x| !x.starts_with(&ca) && x.ends_with(" beta"))
    });
    let _ = a;
}

#[cfg(unix)]
#[test]
fn buffer_editor_keeps_the_final_newline_like_window_buffer_edit_close_cb() {
    let ctx = Ctx::new("buffer-editor");
    let t = start(&ctx, "be");
    // the editor changes nothing: "line\n" stays "line\n"
    ctx.ok(&["set", "-s", "editor", "true"]);
    ctx.ok(&["setb", "-b", "nl", "line\n"]);
    ctx.ok(&["choose-buffer", "-t", "be", "-F", "x"]);
    t.wait_screen("buffers", |s| s.contains("(0) nl"));
    t.send(b"e");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(ctx.ok(&["show-buffer", "-b", "nl"]), "line\n");
    // a buffer without one loses the newline the editor adds
    ctx.ok(&["set", "-s", "editor", "printf 'new\\n' >"]);
    ctx.ok(&["deleteb", "-b", "nl"]);
    ctx.ok(&["setb", "-b", "nonl", "old"]);
    wait_for("list", || t.contents().contains("(0) nonl"));
    t.send(b"e");
    wait_for("edited", || ctx.ok(&["show-buffer", "-b", "nonl"]) == "new");
    // an empty file changes nothing
    ctx.ok(&["set", "-s", "editor", "truncate -s 0"]);
    t.send(b"e");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(ctx.ok(&["show-buffer", "-b", "nonl"]), "new");
}

#[test]
fn customize_mode_adds_items_from_groups_and_keeps_command_lists() {
    let ctx = Ctx::new("customize-add");
    let t = start(&ctx, "ca");
    // s on a group row asks for a new item
    ctx.ok(&["customize-mode", "-t", "ca"]);
    t.wait_screen("groups", |s| s.contains("Server Options"));
    t.send(b"s");
    t.wait_screen("option prompt", |s| s.contains("New user option: @"));
    t.send(b"xyz hello\r");
    wait_for("option set", || ctx.ok(&["show", "-sv", "@xyz"]).trim() == "hello");
    t.send(b"jjjjjs");
    t.wait_screen("env prompt", |s| s.contains("New environment: "));
    t.send(b"NEWV=1\r");
    wait_for("env set", || ctx.ok(&["showenv", "-g", "NEWV"]).trim() == "NEWV=1");
    t.send(b"q");
    wait_for("closed", || ctx.fmt("ca", "#{pane_in_mode}") == "0");
    // keys are in key code order, as list-keys prints them
    for k in ["b", "A", "Enter", "1", "C-a"] {
        ctx.ok(&["bind", "-T", "zz", k, "display", k]);
    }
    ctx.ok(&["customize-mode", "-t", "ca", "-f", "#{||:#{==:#{key},Enter},#{||:#{==:#{key},1},#{||:#{==:#{key},A},#{||:#{==:#{key},b},#{==:#{key},C-a}}}}}"]);
    t.wait_screen("zz table", |s| s.contains("Key Table - zz"));
    t.send(b"Gl");
    t.wait_screen("zz keys", |s| {
        let pos = |k: &str| s.lines().position(|l| l.ends_with(&format!(" {k}")) || l.contains(&format!("+ {k} ")) || l.trim_end().ends_with(&format!("+ {k}")));
        let v: Vec<_> = ["Enter", "1", "A", "b", "C-a"].iter().map(|k| pos(k)).collect();
        v.iter().all(|p| p.is_some()) && v.windows(2).all(|w| w[0] < w[1])
    });
    t.send(b"q");
    wait_for("closed", || ctx.fmt("ca", "#{pane_in_mode}") == "0");
    // toggling Repeat keeps a binding of several commands whole
    ctx.ok(&["bind-key", "-T", "prefix", "F1", "set -g @a 1 ; set -g @b 2"]);
    ctx.ok(&["customize-mode", "-t", "ca", "-f", "#{==:#{key},F1}"]);
    t.wait_screen("prefix", |s| s.contains("Key Table - prefix"));
    for k in [b"G", b"k", b"k", b"l", b"j", b"l", b"j", b"j", b"j"] {
        t.send(k);
        std::thread::sleep(Duration::from_millis(50));
    }
    t.wait_screen("repeat line", |s| s.contains("Repeat: off"));
    t.send(b"\r");
    wait_for("repeat on", || ctx.ok(&["list-keys", "-T", "prefix", "F1"]).contains("bind-key -r"));
    assert!(ctx.fails(&["show", "-gv", "@b"]).contains("invalid option"));
    assert!(ctx.ok(&["list-keys", "-T", "prefix", "F1"]).contains("@b 2"));
}

#[test]
fn find_window_opens_a_filtered_tree() {
    let ctx = Ctx::new("find-window");
    let t = start(&ctx, "fw");
    ctx.ok(&["new-window", "-d", "-t", "fw", "-n", "editor"]);
    ctx.ok(&["new-window", "-d", "-t", "fw", "-n", "other"]);
    // one match does not switch to it, the tree shows it
    ctx.ok(&["find-window", "-N", "-t", "fw:0", "edi"]);
    assert_eq!(ctx.fmt("fw:0", "#{pane_mode}"), "tree-mode");
    t.wait_screen("filtered", |s| s.contains("1: editor") && !s.contains("2: other") && s.contains("(filter: active)"));
    assert_eq!(ctx.fmt("fw", "#{window_index}"), "0");
    t.send(b"q");
    wait_for("left", || ctx.fmt("fw:0", "#{pane_in_mode}") == "0");
    // no match: the whole tree (mode_tree_build drops a filter matching nothing)
    ctx.ok(&["find-window", "-N", "-t", "fw:0", "nosuch"]);
    t.wait_screen("unfiltered", |s| s.contains("2: other"));
}

#[test]
fn pane_prompt_belongs_to_the_pane_not_the_client() {
    let ctx = Ctx::new("pane-prompt");
    let t = start(&ctx, "pp");
    let c1 = client(&ctx);
    let t2 = Term::attach(&ctx, &["attach", "-t", "pp"], 100, 40);
    t2.wait_screen("second client", |s| s.contains("[pp]"));
    // a status prompt and a pane prompt are separate (window_pane_set_prompt)
    ctx.ok(&["command-prompt", "-b", "-t", &c1, "-p", "STATUSP", "set -g @s '%%'"]);
    ctx.ok(&["command-prompt", "-b", "-P", "-t", &c1, "-p", "PANEP", "set -g @p '%%'"]);
    t.wait_screen("both prompts", |_| t.row(38).starts_with("PANEP") && t.row(39).starts_with("STATUSP"));
    // the other client shows the pane prompt but not the first client's status prompt
    t2.wait_screen("pane prompt on the other client", |_| t2.row(38).starts_with("PANEP") && t2.row(39).starts_with("[pp]"));
    // one prompt per pane: another -P does nothing
    ctx.ok(&["command-prompt", "-b", "-P", "-t", &c1, "-p", "OTHER", "set -g @o 1"]);
    std::thread::sleep(Duration::from_millis(200));
    assert!(t2.row(38).starts_with("PANEP"), "{}", t2.row(38));
    // keys from a client without a status prompt go to the pane prompt
    t2.send(b"xyz\r");
    wait_for("pane prompt answered", || ctx.cmd(&["show", "-gv", "@p"]).1.trim() == "xyz");
    t2.wait_screen("pane prompt closed", |_| !t2.row(38).starts_with("PANEP"));
    assert!(ctx.fails(&["show", "-gv", "@s"]).contains("invalid option"));
    // the first client's status prompt is still open
    t.send(b"abc\r");
    wait_for("status prompt answered", || ctx.cmd(&["show", "-gv", "@s"]).1.trim() == "abc");
}

#[test]
fn pane_prompt_expands_for_its_own_pane() {
    let ctx = Ctx::new("pane-prompt-fmt");
    let t = start(&ctx, "pf");
    let c1 = client(&ctx);
    ctx.ok(&["split-window", "-h", "-t", "pf"]);
    ctx.ok(&["set", "-g", "message-format", "<#{pane_index}>#{message}"]);
    ctx.ok(&["command-prompt", "-b", "-P", "-t", &c1, "-p", "IDX#{pane_index}", "set -g @p '%%'"]);
    t.wait_screen("prompt on pane 1", |s| s.contains("<1>IDX1"));
    // prompt_create keeps the command's state: the label is still for pane 1 once
    // another pane is active
    ctx.ok(&["select-pane", "-t", "pf:0.0"]);
    ctx.ok(&["refresh-client", "-t", &c1]);
    std::thread::sleep(Duration::from_millis(300));
    t.wait_screen("still pane 1", |s| s.contains("<1>IDX1") && !s.contains("IDX0"));
}

#[test]
fn pane_in_mode_counts_the_mode_stack_and_only_copy_mode_takes_commands() {
    let ctx = Ctx::new("mode-count");
    ctx.ok(&["new-session", "-d", "-s", "n"]);
    ctx.ok(&["clock-mode", "-t", "n"]);
    ctx.ok(&["choose-tree", "-t", "n"]);
    // format_cb_pane_in_mode: one for each mode on the stack
    assert_eq!(ctx.fmt("n", "#{pane_in_mode} #{pane_mode}"), "2 tree-mode");
    // cmd_send_keys_exec: tree and clock mode have no command callback
    assert!(ctx.fails(&["send-keys", "-t", "n", "-X", "cancel"]).contains("not in a mode"));
    assert_eq!(ctx.fmt("n", "#{pane_in_mode}"), "2");
    ctx.ok(&["copy-mode", "-q", "-t", "n"]);
    assert!(ctx.fails(&["send-keys", "-t", "n", "-X", "cancel"]).contains("not in a mode"));
    ctx.ok(&["copy-mode", "-t", "n"]);
    ctx.ok(&["send-keys", "-t", "n", "-X", "cancel"]);
    assert_eq!(ctx.fmt("n", "#{pane_in_mode}"), "0");
}

#[test]
fn tree_swap_keeps_the_current_window_like_window_tree_swap() {
    let ctx = Ctx::new("tree-swap");
    let t = start(&ctx, "sw");
    ctx.ok(&["set", "-g", "automatic-rename", "off"]);
    ctx.ok(&["rename-window", "-t", "sw:0", "first"]);
    ctx.ok(&["new-window", "-d", "-t", "sw:1", "-n", "second"]);
    ctx.ok(&["choose-tree", "-w", "-t", "sw:0"]);
    t.wait_screen("tree", |s| s.contains("1: second"));
    // the selection is on window 0 (the current one): J swaps it with window 1, and the
    // session's current window follows it, so the client still sees the tree
    t.send(b"J");
    wait_for("swapped", || ctx.ok(&["list-windows", "-t", "sw", "-F", "#{window_index}:#{window_name}:#{window_active}:#{window_last_flag}"]) == "0:second:0:1\n1:first:1:0\n");
    assert_eq!(ctx.fmt("sw", "#{pane_mode}"), "tree-mode");
    t.send(b"K");
    wait_for("back", || ctx.ok(&["list-windows", "-t", "sw", "-F", "#{window_index}:#{window_name}:#{window_active}:#{window_last_flag}"]) == "0:first:1:0\n1:second:0:1\n");
}

#[test]
fn send_keys_strings_go_to_the_mode_one_character_at_a_time() {
    let ctx = Ctx::new("send-mode");
    let t = start(&ctx, "sk");
    let c = client(&ctx);
    ctx.ok(&["choose-tree", "-t", "sk"]);
    t.wait_screen("tree", |s| s.contains("(0) - sk"));
    // -l text goes to the mode's prompt rather than the program under the mode
    ctx.ok(&["send-keys", "-c", &c, "-t", "sk", "C-s"]);
    ctx.ok(&["send-keys", "-c", &c, "-t", "sk", "-l", "thi"]);
    t.wait_screen("search text", |s| s.contains("(search) thi"));
    ctx.ok(&["send-keys", "-c", &c, "-t", "sk", "Escape"]);
    // a string that is not a key name is typed a character at a time
    ctx.ok(&["send-keys", "-c", &c, "-t", "sk", "f", "abc"]);
    t.wait_screen("filter text", |s| s.contains("(filter) abc"));
    ctx.ok(&["send-keys", "-c", &c, "-t", "sk", "Escape"]);
    // -H bytes too: q leaves the mode
    ctx.ok(&["send-keys", "-c", &c, "-t", "sk", "-H", "71"]);
    wait_for("left", || ctx.fmt("sk", "#{pane_in_mode}") == "0");
    ctx.ok(&["choose-tree", "-t", "sk"]);
    ctx.ok(&["send-keys", "-c", &c, "-t", "sk", "-l", "q"]);
    wait_for("left with -l", || ctx.fmt("sk", "#{pane_in_mode}") == "0");
}

#[test]
fn numeric_prompts_take_the_input_at_the_first_other_key() {
    let ctx = Ctx::new("numeric");
    let t = start(&ctx, "nu");
    ctx.ok(&["bind", "-n", "F7", "command-prompt", "-N", "-p", "num", "set -g @n '%%'"]);
    ctx.ok(&["bind", "-n", "F8", "command-prompt", "-P", "-N", "-p", "num", "set -g @p '%%'"]);
    // PROMPT_NUMERIC: b runs the template with 5 and closes the prompt; b goes on to the pane
    t.send(b"\x1b[18~");
    t.wait_screen("prompt", |s| s.contains("num"));
    t.send(b"5b6");
    wait_for("status prompt", || ctx.ok(&["show", "-gv", "@n"]).trim() == "5");
    t.send(b"\x1b[19~");
    t.wait_screen("pane prompt", |s| s.lines().any(|l| l.starts_with("num")));
    t.send(b"7x");
    wait_for("pane prompt", || ctx.ok(&["show", "-gv", "@p"]).trim() == "7");
    t.wait_screen("keys went on to the shell", |s| s.contains("b6x"));
}

#[test]
fn mode_prompts_take_every_key_and_keep_a_history() {
    let ctx = Ctx::new("mode-prompt");
    let t = start(&ctx, "mp");
    ctx.ok(&["new-window", "-d", "-t", "mp", "-n", "alpha"]);
    ctx.ok(&["choose-tree", "-t", "mp"]);
    t.wait_screen("tree", |s| s.contains("alpha"));
    // keys the prompt does not use are not passed to the tree: no tags, no movement
    t.send(b"/");
    t.wait_screen("search", |s| s.contains("(search)"));
    t.send(b"\x1b[B\x1b[B\x14");
    t.send(b"\x1b");
    t.wait_screen("closed", |s| !s.contains("(search)"));
    t.send(b"X");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!t.contents().contains("tagged?"), "{}", t.contents());
    // accepted input goes to the search history and Up recalls it
    t.send(b"f\x15xyz\r");
    t.wait_screen("filtered", |s| s.contains("(filter: "));
    t.send(b"f\x15");
    t.wait_screen("empty filter", |s| s.lines().any(|l| l.trim_end().ends_with("(filter)") || l.starts_with("(filter) ─")));
    t.send(b"\x1b[A");
    t.wait_screen("recalled", |s| s.contains("(filter) xyz"));
    t.send(b"\x1b");
    let h = ctx.ok(&["show-prompt-history", "-T", "search"]);
    assert_eq!(h, "History for search:\n\n1: xyz\n\n");
    // the prompts are drawn with message-format, and prompt_flags says what they are
    ctx.ok(&["set", "-g", "message-format", "#{prompt_type}|#{prompt_flags}|#{message}"]);
    t.send(b"f");
    t.wait_screen("format", |s| s.contains("search|NOFORMAT,ISMODE|(filter) xyz"));
    t.send(b"\x1b");
    ctx.ok(&["bind", "-n", "F8", "command-prompt", "-P", "-N", "-p", "num", "set -g @p '%%'"]);
    t.send(b"q\x1b[19~");
    t.wait_screen("pane prompt format", |s| s.contains("command|NUMERIC,ISPANE|num"));
}

#[test]
fn switch_mode_cursor_keys_keep_the_selection() {
    let ctx = Ctx::new("switch-keys");
    ctx.ok(&["new-session", "-d", "-s", "beta"]);
    let t = start(&ctx, "alpha");
    ctx.ok(&["switch-mode", "-t", "alpha"]);
    t.wait_screen("list", |s| s.lines().nth(1).is_some_and(|r| r.starts_with("beta ")));
    // a, then Down to beta; Left, Home and C-a only move the prompt cursor
    t.send(b"a\x1b[B");
    std::thread::sleep(Duration::from_millis(200));
    t.send(b"\x1b[D\x1b[H\x01\x05");
    std::thread::sleep(Duration::from_millis(200));
    t.send(b"\r");
    wait_for("switched to beta", || ctx.ok(&["list-clients", "-F", "#{client_session}"]).trim() == "beta");
}

#[test]
fn customize_mode_toggles_cycles_appends_and_describes() {
    let ctx = Ctx::new("customize-set");
    let t = start(&ctx, "cu");
    ctx.ok(&["set", "-g", "mouse", "off"]);
    ctx.ok(&["set", "-g", "status-left-length", "44"]);
    ctx.ok(&["customize-mode", "-t", "cu", "-f", "#{||:#{==:#{option_name},mouse},#{||:#{==:#{option_name},status-position},#{||:#{==:#{option_name},status-left-length},#{m:command-alias*,#{option_name}}}}}"]);
    t.wait_screen("groups", |s| s.contains("Session Options"));
    t.send(b"\x1b+");
    t.wait_screen("expanded", |s| s.contains("mouse: off"));
    // the preview has the option table's text and the global value
    // the line of an item counted from the first item (the status line may be at the top)
    let line_of = |f: &dyn Fn(&str) -> bool| {
        let c = t.contents();
        let first = c.lines().position(|l| l.starts_with("(0)")).unwrap();
        c.lines().position(f).unwrap() - first
    };
    let find = |name: &str| line_of(&|l: &str| l.contains(&format!("{name}:")));
    t.send(b"\x1b[H");
    for _ in 0..find("mouse") {
        t.send(b"\x1b[B");
    }
    t.wait_screen("description", |s| s.contains("Whether the mouse is recognised"));
    // a flag toggles at once, for the session
    t.send(b"\r");
    wait_for("toggled", || ctx.ok(&["show", "-v", "-t", "cu", "mouse"]).trim() == "on");
    assert_eq!(ctx.ok(&["show", "-gv", "mouse"]).trim(), "off");
    t.wait_screen("global value", |s| s.contains("Global value: off"));
    // a choice moves to the next value
    t.send(b"\x1b[H");
    for _ in 0..find("status-position") {
        t.send(b"\x1b[B");
    }
    t.send(b"s");
    wait_for("cycled", || ctx.ok(&["show", "-v", "-t", "cu", "status-position"]).trim() == "top");
    // other options get a prompt naming where the value goes
    t.send(b"\x1b[H");
    for _ in 0..find("status-left-length") {
        t.send(b"\x1b[B");
    }
    t.send(b"S");
    t.wait_screen("global prompt", |s| s.contains("(status-left-length, global) 44"));
    t.send(b"\x1b");
    // a whole array gets the [+] prompt, which adds an element
    t.send(b"\x1b[H");
    for _ in 0..line_of(&|l: &str| l.contains("command-alias") && !l.contains('[')) {
        t.send(b"\x1b[B");
    }
    t.send(b"s");
    t.wait_screen("append prompt", |s| s.contains("(command-alias[+]) "));
    t.send(b"\x15zz=display hi\r");
    wait_for("appended", || ctx.ok(&["show", "-sv", "command-alias"]).contains("zz=display hi"));
    // C keeps only the elements that differ from the default
    t.send(b"C");
    t.wait_screen("changed only", |s| s.contains("+ command-alias"));
    t.send(b"\x1b[H\x1b[B\x1b[C");
    t.wait_screen("changed elements", |s| s.contains("zz=display hi") && !s.contains("command-alias[0]"));
}

#[test]
fn customize_mode_appends_to_a_sparse_hook_at_the_first_free_index() {
    let ctx = Ctx::new("customize-hookadd");
    let t = start(&ctx, "ch");
    ctx.ok(&["set-hook", "-g", "after-new-window[1]", "set -g @one 1"]);
    ctx.ok(&["set-hook", "-g", "after-new-window[2]", "set -g @two 1"]);
    ctx.ok(&["customize-mode", "-t", "ch", "-f", "#{==:#{option_name},after-new-window}"]);
    t.wait_screen("groups", |s| s.contains("Session Hooks"));
    t.send(b"\x1b+");
    t.wait_screen("expanded", |s| s.contains("+ after-new-window"));
    let c = t.contents();
    let first = c.lines().position(|l| l.starts_with("(0)")).unwrap();
    let at = c.lines().position(|l| l.contains("after-new-window") && !l.contains('[')).unwrap() - first;
    t.send(b"\x1b[H");
    for _ in 0..at {
        t.send(b"\x1b[B");
    }
    t.send(b"s");
    t.wait_screen("append prompt", |s| s.contains("(after-new-window[+]"));
    t.send(b"\x15set -g @zero 1\r");
    // window_customize_set_option_callback: the first index without a value, here 0
    wait_for("appended", || ctx.ok(&["show-hooks", "-g", "after-new-window"]).contains("@zero"));
    assert_eq!(
        ctx.ok(&["show-hooks", "-g", "after-new-window"]),
        "after-new-window[0] set-option -g @zero 1\nafter-new-window[1] set-option -g @one 1\nafter-new-window[2] set-option -g @two 1\n"
    );
}

#[test]
fn pane_mode_hooks_fire_like_window_fire_pane_mode_changed() {
    let ctx = Ctx::new("mode-hooks");
    let _t = start(&ctx, "mh");
    ctx.ok(&["set-hook", "-g", "pane-mode-entered", "set -ga @e e"]);
    ctx.ok(&["set-hook", "-g", "pane-mode-exited", "set -ga @x x"]);
    ctx.ok(&["set-hook", "-g", "pane-mode-changed", "set -ga @c +"]);
    let v = |n: &str| ctx.cmd(&["show", "-gv", n]).1.trim().to_string();
    ctx.ok(&["clock-mode", "-t", "mh"]);
    ctx.ok(&["choose-tree", "-t", "mh"]);
    wait_for("entered", || v("@e") == "ee" && v("@c") == "++");
    // copy-mode -q leaves every mode: exited and changed for each
    ctx.ok(&["copy-mode", "-q", "-t", "mh"]);
    wait_for("exited", || v("@x") == "xx" && v("@c") == "++++");
    // a mode further down the stack raised to the top counts as entered again
    ctx.ok(&["clock-mode", "-t", "mh"]);
    ctx.ok(&["choose-tree", "-t", "mh"]);
    ctx.ok(&["clock-mode", "-t", "mh"]);
    wait_for("raised", || v("@e") == "eeeee");
}

#[test]
fn choose_client_info_view_has_window_client_info_lines() {
    let ctx = Ctx::new("client-info");
    let t = start(&ctx, "ci");
    ctx.ok(&["choose-client", "-i", "-t", "ci"]);
    t.wait_screen("info", |s| s.contains("(view: info)"));
    let c = t.contents();
    let labels: Vec<String> = c.lines().filter_map(|l| l.strip_prefix("│ ")).map(|l| l.split('│').next().unwrap_or("").trim().to_string()).filter(|l| !l.is_empty()).collect();
    assert_eq!(labels[..9], ["Client Name", "Session", "Attach Time", "Activity Time", "Terminal Type", "TERM", "Size", "Bytes Written", "Features"], "{c}");
    assert!(c.contains("(cell ") && c.contains(" discarded)"), "{c}");
    t.send(b"q");
}

#[test]
fn mode_tree_right_click_menus_send_keys_to_the_mode() {
    let ctx = Ctx::new("mode-menu");
    let t = start(&ctx, "mm");
    ctx.ok(&["set", "-g", "mouse", "on"]);
    ctx.ok(&["set", "-g", "status-position", "bottom"]);
    ctx.ok(&["new-window", "-d", "-t", "mm", "-n", "second"]);
    ctx.ok(&["choose-tree", "-w", "-t", "mm"]);
    t.wait_screen("tree", |s| s.contains("1: second"));
    // right press on an item: its menu, titled with the item's name, with the keys
    let row = t.contents().lines().position(|l| l.contains("1: second")).unwrap() as u16;
    t.mouse(2, 10, row, false);
    t.wait_screen("menu", |s| s.contains("Tag All ([DC4])") && s.contains("Kill Tagged (X)") && s.contains("─1─"));
    // Tag sends t for that item
    let tag = t.contents().lines().position(|l| l.contains("│ Tag ") && l.contains("(t)")).unwrap() as u16;
    t.mouse(0, 6, tag, false);
    t.mouse(0, 6, tag, true);
    t.wait_screen("tagged", |s| s.contains("second*") || s.contains("1*:") || s.lines().any(|l| l.contains("1: second") && l.contains('*')));
    // outside the list: the menu that scrolls the previews; Cancel leaves the mode
    t.mouse(2, 50, 30, false);
    t.wait_screen("outside menu", |s| s.contains("Scroll Left") && s.contains("Scroll Right"));
    let cancel = t.contents().lines().position(|l| l.contains("Cancel")).unwrap() as u16;
    let r = t.row(cancel);
    let col = r[..r.find("Cancel").unwrap()].chars().count() as u16;
    t.mouse(0, col, cancel, false);
    t.mouse(0, col, cancel, true);
    wait_for("cancelled", || ctx.fmt("mm", "#{pane_in_mode}") == "0");
}

#[cfg(unix)]
#[test]
fn mode_editor_runs_in_a_modal_floating_pane() {
    // spawn_editor: a modal floating pane 9/10 of the window in its middle, gone when
    // the editor exits
    let ctx = Ctx::new("editor-modal");
    let t = start(&ctx, "em");
    ctx.ok(&["set", "-s", "editor", "sleep 1; printf 'edited\\n' >"]);
    ctx.ok(&["setb", "-b", "one", "old"]);
    ctx.ok(&["choose-buffer", "-t", "em", "-F", "x"]);
    t.wait_screen("buffers", |s| s.contains("(0) one"));
    let (sx, sy): (u32, u32) = {
        let s = ctx.fmt("em", "#{window_width} #{window_height}");
        let (a, b) = s.split_once(' ').unwrap();
        (a.parse().unwrap(), b.parse().unwrap())
    };
    t.send(b"e");
    let (w, h) = (sx * 9 / 10, sy * 9 / 10);
    let want = format!("{},{} {w}x{h} 1 1 1", sx / 2 - w / 2, sy / 2 - h / 2);
    wait_for("editor pane", || ctx.ok(&["list-panes", "-t", "em", "-F", "#{pane_left},#{pane_top} #{pane_width}x#{pane_height} #{pane_modal_flag} #{pane_floating_flag} #{pane_active}"]).lines().any(|l| l == want));
    wait_for("edited", || ctx.ok(&["show-buffer", "-b", "one"]) == "edited");
    wait_for("editor pane gone", || ctx.ok(&["list-panes", "-t", "em"]).lines().count() == 1);
}
