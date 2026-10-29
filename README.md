# pwsh-tmux

A terminal multiplexer that follows tmux's commands, key bindings, options, formats and
config syntax, written in Rust for **Windows** (ConPTY + named pipes) and **Linux/Unix**
(pty + Unix sockets). The default shell is PowerShell (`pwsh`, falling back to
`powershell.exe` on Windows; `pwsh`, then `$SHELL` on Unix).

## Build

```
cargo build --release
# target/release/pwsh-tmux(.exe) — put it on PATH (optionally alias it to `tmux`)
```

## Usage

Everything works like tmux:

```
pwsh-tmux                         # new session, attached
pwsh-tmux new -s work -d          # detached session
pwsh-tmux attach -t work          # (alias: a / at)
pwsh-tmux ls
pwsh-tmux split-window -h -t work
pwsh-tmux send-keys -t work 'Get-Date' Enter
pwsh-tmux capture-pane -p -t work
pwsh-tmux list-keys               # all bindings, prefix C-b
pwsh-tmux list-commands
```

Global flags: `-L socket-name`, `-S socket-path`, `-f config`, `-N` (don't start a
server), `-c shell-command`, `-V`.

Config: `~/.pwsh-tmux.conf` or `~/.config/pwsh-tmux/pwsh-tmux.conf` (tmux syntax:
`set -g`, `bind`, `{ }` blocks, `\;`, `%if/%else/%endif`, `source-file`, ...). To reuse a
tmux config, put `source-file ~/.tmux.conf` in it.

## What is implemented

- **Sessions / windows / panes**: ~90 tmux commands, including new/attach/detach/switch,
  split/kill/select/resize/swap/rotate/break/join/move panes, new/kill/move/swap/link/unlink/
  rename/find windows, respawn, `remain-on-exit`, `synchronize-panes`, `pipe-pane`, marked
  pane, zoom.
- **Targets**: `session:window.pane`, `$id @id %id`, `{last} {next} {previous} {top} {left}
  {marked} {mouse}` ..., `+N/-N`, name prefixes and globs.
- **Layouts**: even-horizontal/vertical, main-horizontal/vertical, tiled, `next-layout`,
  `select-layout -E`, tmux layout strings (`#{window_layout}` round-trips).
- **Key tables**: tmux's default `prefix`, `root`, `copy-mode`, `copy-mode-vi` tables,
  `bind -n/-r/-T/-N`, `unbind`, `switch-client -T`, `prefix2`, repeat-time.
- **Copy mode** (emacs and vi keys): motions, word/line/rectangle selection, search,
  jumps, counts, `copy-pipe`, mouse selection; copied text goes to a paste buffer and to the
  outer terminal clipboard (OSC 52).
- **Buffers**: set/show/list/delete/save/load/paste/choose-buffer.
- **Formats**: `#{...}` variables, conditionals, comparisons, modifiers (`=N`, `s///`, `b:`,
  `t:`, `e|+|`, `m:` ...), strftime, `#[style]`. Status line built from `status-left`,
  `window-status(-current)-format`, `status-right`, `status-format[]`, justify, styles.
- **Options**: server/session/window/pane scopes, `set -g/-w/-s/-p/-u/-a/-o/-q/-F`,
  `show -A/-v/-g`, user `@options`.
- **Interactive**: command prompt (history, completion), confirm-before, choose-tree /
  choose-client / customize-mode, display-menu, display-popup, display-panes, clock-mode,
  messages, `pane-border-status`.
- **Mouse** (`set -g mouse on`): click to select panes/windows, drag borders, wheel scrolling
  into copy mode, drag selection, double/triple click, right-click menus.
- **Scripting**: hooks (`set-hook`, `after-*`, `client-attached`, `pane-exited`, ...),
  `run-shell`, `if-shell` (async and `-F`), `wait-for` (-S/-L/-U), `set-environment`,
  `update-environment`, alerts (bell/activity/silence).
- **Platform details**: on Windows `#{pane_current_path}` comes from an OSC 7 prompt hook
  injected into PowerShell (disable with `PWSH_TMUX_NO_HOOK=1`), and
  `#{pane_current_command}` / automatic-rename from the shell's descendant processes.
  On Unix they come from `/proc` and the pty's foreground process group.

## Also supported

- **Control mode** (`-C`, `-CC`): `%begin/%end/%error` replies, `%output`, and
  `%session-changed`, `%sessions-changed`, `%session-renamed`, `%session-window-changed`,
  `%window-add/close/renamed`, `%window-pane-changed`, `%layout-change`,
  `%pane-mode-changed`, `%exit`; `refresh-client -C WxH` sizes the session.
- **Session groups** (`new-session -t`): shared window list, per-session current window,
  `#{session_group*}` variables.
- **`#(command)` in formats**: run in the background, last output line cached and refreshed
  every `status-interval`.

## Not supported (yet)

- `server-access`, terminal features (`-T`) negotiation, extended keys output, control-mode
  flow control (`%pause`/`%continue`) and subscriptions.
- Window sizes are capped at 1000x1000 (the terminal emulator allocates every cell).
- On Windows `suspend-client` does nothing and `lock-*` needs `lock-command`.

## Tests

```
cargo test        # unit + integration tests (drive real attached clients through a pty)
```

The integration tests run on Windows locally and on Linux (both `pwsh` and `bash` as the
default shell).
