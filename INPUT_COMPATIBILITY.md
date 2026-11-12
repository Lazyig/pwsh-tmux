# Input and terminal options (tmux 3.8)

Key input reads `backspace`, `user-keys`, `assume-paste-time`, `prefix-timeout`,
`initial-repeat-time`, `repeat-time`, `extended-keys` and `extended-keys-format`.
User keys are named `User0` through `User999`. Bracketed-paste terminals do not
use paste guessing. Unterminated pastes recover after five seconds, and partial
paste end markers receive at least 500ms before their escape timer expires.
`xterm-keys` is retained as an intentional no-op: tmux 3.8's options-table.c
marks this option "no longer used"; modified function keys remain enabled.

Terminal features accept both the native `feature@` removal syntax and `@feature`.
`utf8` controls Unicode terminal output and `appesc` enables application Escape
mode. WezTerm and Ghostty advertise left/right margins. The renderer redraws
using absolute coordinates and does not require margin scrolling optimizations.
Terminal overrides support pattern matching, capability removal, VT string
escapes and terminfo parameter expressions, including arithmetic, variables,
conditionals and string parameters for cursor and clipboard capabilities.
On Unix, `infocmp` supplies existing capabilities, including Ss/Se;
explicit overrides take precedence. Missing `infocmp` falls back to VT defaults.

Cursor style and colour defaults apply to panes; explicit application cursor
settings take precedence. Prompt cursor options apply in insert mode, and
`prompt-command-cursor-*` applies in vi command mode (`status-keys vi`).

`get-clipboard buffer` answers OSC 52 queries from the newest paste buffer;
`request` forwards queries to an attached outer terminal and replies to the
requesting pane; `both` also stores the response. `off` suppresses replies.
Application OSC 52 queries require `set-clipboard on`, as in tmux. Requests expire
after five seconds. Clipboard reads depend on the outer terminal permitting OSC
52 queries. `allow-passthrough on` permits tmux DCS wrappers from visible panes;
`all` also permits hidden panes in the client's session. `allow-set-title` gates
application pane titles; `alternate-screen` gates application screen switching.
`clear-on-attach off` keeps the client in its normal screen and scrolls its
previous contents into terminal scrollback.

`codepoint-widths` accepts individual characters, `U+XXXX` and
`U+XXXX-U+YYYY` ranges with widths 0, 1 or 2; malformed entries are ignored and
later entries win. These widths and `variation-selector-always-wide` control
pane terminal cells, status text, prompt text and format width calculations.
`input-buffer-size` bounds OSC/DCS strings, discarding oversized
strings through their terminator without turning their contents into text.

Idle locking uses session activity and the session's `lock-command`. Explicit
`lock-client`, `lock-session` and `lock-server` use each affected session's
command. An empty command leaves the client unlocked. Prompt history loads
from `history-file` after configuration and is saved when it changes. Empty
client invocations use `default-client-command`. Command aliases expand once,
with caller arguments appended to the last command. `remain-on-exit key` and
`failed-key` keep qualifying panes until a keyboard key dismisses them; paste
and mouse reports do not dismiss them.

## Windows

ConPTY and Windows console VT input use the same option paths. Windows has no
system terminfo database: host database lookup is explicitly a no-op under
`cfg(windows)`; native VT defaults and explicit overrides remain available.
Locking runs the configured Windows command on the client's console; it does
not invoke a Unix screen locker or lock the Windows desktop. Unix process
suspension is unavailable on Windows (the existing SIGTSTP path is Unix-only).

## Validation

Built with `CARGO_BUILD_JOBS=2`. `cargo test --release --no-fail-fast` passes
253 unit tests and the new CLI tests. The only failures are the existing Linux
baselines `format_loops_and_utf8_length` and
`readonly_terminal_blocks_keys_paste_mouse_and_shared_resize`; the UI recording
test remains intentionally ignored. `cargo check --target
x86_64-pc-windows-gnu --tests` passes. Reference semantics were checked against
the local tmux 3.8 C sources and manual; the reference pod is inaccessible from
this sandbox.
