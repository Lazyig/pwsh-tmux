mod cells;
mod client;
mod cmdparse;
#[allow(dead_code)]
mod copymode;
mod format;
mod ipc;
#[allow(dead_code)]
mod keys;
#[allow(dead_code)]
mod layout;
mod options;
mod opts;
mod protocol;
mod server;
mod term;
mod width;

const USAGE: &str = "\
usage: pwsh-tmux [-2CDlNuVv] [-c shell-command] [-f file] [-L socket-name]
                 [-S socket-path] [-T features] [command [flags]]

pwsh-tmux is a terminal multiplexer compatible with tmux's commands, key bindings,
options and formats, for Windows (ConPTY) and Unix.

Run `pwsh-tmux list-commands` for every command and `pwsh-tmux list-keys` for the
key bindings (default prefix: C-b). Configuration is read from ~/.pwsh-tmux.conf
or ~/.config/pwsh-tmux/pwsh-tmux.conf (tmux syntax; `source-file ~/.tmux.conf` works).

Environment: PWSH_TMUX_SHELL overrides the default shell (pwsh, then powershell on
Windows; pwsh, then $SHELL on Unix).
";

/// Command-line arguments that are not valid UTF-8 (tmux takes any bytes and check_name
/// rejects such names): each byte that is not part of a UTF-8 character becomes a
/// private-use character, and back to the byte when the client prints a reply.
pub mod rawarg {
    const BASE: u32 = 0x10FF00;

    pub fn is_raw(c: char) -> bool {
        (BASE..=BASE + 0xFF).contains(&(c as u32))
    }

    pub fn from_os(a: std::ffi::OsString) -> String {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let mut out = String::new();
            for chunk in a.as_bytes().utf8_chunks() {
                out.push_str(chunk.valid());
                out.extend(chunk.invalid().iter().map(|b| char::from_u32(BASE + *b as u32).unwrap()));
            }
            out
        }
        #[cfg(not(unix))]
        {
            a.to_string_lossy().into_owned()
        }
    }

    pub fn to_bytes(s: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(s.len());
        for c in s.chars() {
            if is_raw(c) {
                out.push((c as u32 - BASE) as u8);
            } else {
                out.extend(c.encode_utf8(&mut [0; 4]).as_bytes());
            }
        }
        out
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args_os().skip(1).map(rawarg::from_os).collect();
    if args.first().map(String::as_str) == Some("__server") {
        let config = args.get(1).cloned();
        let code = match server::run(config) {
            Ok(()) => 0,
            Err(e) => {
                server::log(&format!("server failed: {e}"));
                1
            }
        };
        std::process::exit(code);
    }
    let mut g = client::Global { config: None, no_start: false, control: 0, features: String::new(), utf8: false };
    let mut shell_cmd = None;
    // global flags before the command
    while let Some(a) = args.first().cloned() {
        if !a.starts_with('-') || a == "-" {
            break;
        }
        args.remove(0);
        let mut chars = a[1..].chars().peekable();
        while let Some(c) = chars.next() {
            let mut value = || -> Option<String> {
                let rest: String = chars.by_ref().collect();
                if !rest.is_empty() { Some(rest) } else if !args.is_empty() { Some(args.remove(0)) } else { None }
            };
            match c {
                // Safe: no other threads exist yet.
                'L' => unsafe { std::env::set_var("PWSH_TMUX_SOCKET", value().unwrap_or_default()) },
                'S' => unsafe { std::env::set_var("PWSH_TMUX_SOCKET_PATH", value().unwrap_or_default()) },
                'f' => g.config = value(),
                'c' => shell_cmd = value(),
                // -T and -2 add to the client's features (tty_parse_features)
                'T' | '2' => {
                    let f = if c == '2' { "256".to_string() } else { value().unwrap_or_default() };
                    if !g.features.is_empty() { g.features.push(','); }
                    g.features.push_str(&f);
                }
                'N' => g.no_start = true,
                'V' => {
                    println!("pwsh-tmux {}", env!("CARGO_PKG_VERSION"));
                    return;
                }
                'h' => {
                    print!("{USAGE}");
                    return;
                }
                'C' => g.control = (g.control + 1).min(2),
                'u' => g.utf8 = true,
                'D' | 'l' | 'v' => {}
                '-' => {
                    if a == "--help" {
                        print!("{USAGE}");
                        return;
                    }
                    if a == "--version" {
                        println!("pwsh-tmux {}", env!("CARGO_PKG_VERSION"));
                        return;
                    }
                }
                _ => {
                    eprintln!("pwsh-tmux: unknown option -- {c}\n{USAGE}");
                    std::process::exit(1);
                }
            }
        }
    }
    if let Some(cmd) = shell_cmd {
        let status = server::procinfo::job_command(&cmd).status().map(|s| s.code().unwrap_or(1)).unwrap_or(127);
        std::process::exit(status);
    }
    std::process::exit(client::main(args, g));
}
