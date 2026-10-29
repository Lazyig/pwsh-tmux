#[allow(dead_code)]
mod cells;
mod client;
mod ipc;
mod layout;
mod opts;
mod protocol;
mod server;
mod term;

const USAGE: &str = "\
usage: pwsh-tmux [-L socket] [command [flags]]

Sessions
  new-session [-d] [-s name] [-n window] [-c dir] [-x cols] [-y rows] [cmd...]
  attach-session [-t target]        list-sessions (ls)
  kill-session [-t target]          rename-session [-t target] name
  has-session [-t target]           detach-client [-s target]
  kill-server

Windows and panes
  new-window [-d] [-t target] [-n name] [-c dir] [cmd...]
  split-window [-h|-v] [-d] [-t target] [-c dir] [cmd...]
  select-window -t target | -n | -p | -l     next-window, previous-window, last-window
  select-pane [-t target] [-L|-R|-U|-D|-l]   resize-pane [-t target] -L|-R|-U|-D [n] | -Z
  kill-window [-t target]  kill-pane [-t target]  rename-window [-t target] name
  list-windows (lsw)  list-panes (lsp)

Scripting
  send-keys [-l] [-t target] key...   (Enter, Tab, Escape, BSpace, Up, C-c, M-x, ...)
  capture-pane -p [-t target]         display-message -p [-t target] '#{session_name}'

Targets: session, session:window, session:window.pane, %pane-id

Keys (prefix Ctrl+b)
  \"  split top/bottom     %  split left/right    arrows  move between panes
  c  new window           n/p  next/prev window  0-9  select window   l  last window
  o  next pane            ;  last pane           z  zoom pane        x  kill pane
  &  kill window          d  detach              Ctrl/Alt+arrows  resize pane
  Ctrl+b  send a literal Ctrl+b

Environment: PWSH_TMUX_SHELL overrides the default shell (pwsh, then powershell
on Windows; pwsh, then $SHELL on Unix).
";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("-L") && args.len() >= 2 {
        // Safe: no other threads exist yet.
        unsafe { std::env::set_var("PWSH_TMUX_SOCKET", &args[1]) };
        args.drain(..2);
    }
    let code = match args.first().map(String::as_str) {
        Some("__server") => match server::run() {
            Ok(()) => 0,
            Err(e) => {
                server::log(&format!("server failed: {e}"));
                1
            }
        },
        Some("-V" | "--version" | "version") => {
            println!("pwsh-tmux {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            0
        }
        _ => client::main(args),
    };
    std::process::exit(code);
}
