# Input and terminal options (tmux 3.8)

Key input reads `backspace`, `user-keys`, `assume-paste-time`, `prefix-timeout`,
`initial-repeat-time`, `repeat-time`, `extended-keys` and `extended-keys-format`.
User keys are named `User0` through `User1000` (case-sensitive, as tmux's
`User%u`). They share the key tree with the built-in sequences, so the longest
match wins, and an unbound user key is never written to a pane (`send-keys UserN`
sends nothing). An unmodified BSpace writes one byte for a `backspace` key with no
modifiers or only Ctrl, and nothing otherwise. Extended keys follow tmux's modes:
mode 1 (`CSI > 4;1 m`, or `extended-keys always`) keeps Meta keys and Ctrl with
Space, `/`, `2`-`8` or `@`-`~` in their standard form. The mode belongs to the pane:
an application's request is ignored while `extended-keys` is off, and `always`
puts a pane in mode 1 when its screen is created or reset (RIS), not when the option
changes. A key the terminal sends in extended form (`CSI n;m u`, `CSI 27;m;n ~`) is
written to the pane as the key tmux parses, so C-i, C-m, C-@, C-[ and C-S-a stay
distinct from Tab, Enter, C-Space, Escape and C-a in mode 2. Forms tmux does not
parse (kitty's `CSI n u` without modifiers, `:` fields, `CSI 1;2 Z`, `ESC O 5 A`)
are M-[ or M-O followed by the rest as typed text, as in tmux. KPEnter is LF outside
application keypad mode. A key with no standard form
(C-BSpace with extended keys off) is sent by `send-keys` as its name. Only
`ESC ] 5 2 ;` is held back as an OSC 52 reply; any other `ESC ]` is M-].
For key bindings C-[ is the same key as Escape here (an extended-keys `CSI 91;5u`
decodes to Escape and `bind C-[` binds Escape), which gives tmux 3.8's "C-[ has the
bindings of Escape" everywhere; unlike tmux, `list-keys` shows such a binding as Escape.
Bracketed-paste terminals do not
use paste guessing. Unterminated pastes recover after five seconds, and partial
paste end markers receive at least 500ms before their escape timer expires.
`xterm-keys` is retained as an intentional no-op: tmux 3.8's options-table.c
marks this option "no longer used"; modified function keys remain enabled.

Terminal features are parsed like tty_parse_features: a trailing `@` removes a
feature and keeps later entries from adding it back, and an unknown name ends the
list. `client_termfeatures` lists features in tmux's table order. As in tmux 3.8, a
VT100-like terminal is asked on attach for its primary, secondary and extended device
attributes (`CSI c`, `CSI > c`, `CSI > q`) and for synchronized output (`CSI ? 2026 $ p`).
The replies add features as tty_keys_device_attributes and tty_default_features do (a
DA1 reply of level 61 to 65 lists features such as margins and clipboard; DA2 and
XTVERSION name the terminal), and a DECRPM reply of 1 to 3 adds the `sync` feature.
`TERM_PROGRAM` (or a WezTerm/Ghostty TERM) also selects the terminal's default
features. Clients are UTF-8 with `-u`, inside tmux (`$TMUX`), with a UTF-8
locale (LC_ALL, LC_CTYPE, LANG) or on Windows; the `utf8` feature also turns it on,
and `utf8@` never turns it off. A non-UTF-8 client gets line drawing through the
ACS charset and underscores for other characters. `appesc` enables application
Escape mode. The renderer redraws
using absolute coordinates and does not require margin scrolling optimizations.
Terminal overrides support pattern matching, `::` for a literal colon,
capability removal, VT string
escapes and terminfo parameter expressions, including arithmetic, variables,
conditionals and string parameters for cursor and clipboard capabilities.
On Unix, `infocmp` supplies existing capabilities, including Ss/Se;
explicit overrides take precedence. Missing `infocmp` falls back to VT defaults.
As in tmux, `terminal-features` and `terminal-overrides` are read when a client
attaches; changing them does not affect clients already attached.

Cursor style and colour defaults apply to panes; explicit application cursor
settings take precedence. Cursor colours are colour values (not formats) and
are sent as `Cs` with `rgb:RR/GG/BB` from tmux's 256-colour table, `Cr` when the
colour returns to the default; both, and the cursor style, are sent only when
they change. On exit, detach, suspend or lock the server sends `Se` (or `Ss` 0)
after a cursor style, `Cr` after a cursor colour and `Dsesc` for `appesc`. Prompt cursor options apply in insert mode, and
`prompt-command-cursor-*` applies in vi command mode (`status-keys vi`), which has
tmux's command-mode keys (i a A I s S C D d x X h l 0 ^ $ w W b B e E j k p q).

`get-clipboard buffer` answers OSC 52 queries from the newest automatic paste
buffer, with the first allowed selection character of the query; `request`
forwards queries to an attached outer terminal (`Ms` with an empty selection) and
replies to the requesting pane; `both` also stores the response. `off` suppresses
replies. Application OSC 52 queries and selections require `set-clipboard on`, as
in tmux: a selection creates a paste buffer and is written with `Ms` to the clients
showing the pane. `set-buffer -w` and `load-buffer -w` write `Ms` to the target client. Requests expire
after five seconds. Clipboard reads depend on the outer terminal permitting OSC
52 queries. `allow-passthrough on` permits tmux DCS wrappers from visible panes;
`all` also permits hidden panes in the client's session. `allow-set-title` gates
application pane titles; `alternate-screen` gates application screen switching.
`clear-on-attach off` keeps the client in its normal screen and scrolls its
previous contents into terminal scrollback.

`codepoint-widths` accepts individual characters, `U+XXXX` and
`U+XXXX-U+YYYY` ranges with widths 0, 1 or 2; malformed entries are ignored and
later entries win. They apply to UTF-8 characters only: 7-bit characters keep
width 1. These widths control pane terminal cells, status text, prompt text and
format width calculations; `variation-selector-always-wide` decides whether VS16
widens a narrow base character in pane cells. `input-buffer-size` bounds OSC/DCS
strings as tmux's doubling buffer does (the longest string kept is the largest
power of two not above the limit, less one), discarding oversized strings through
their terminator without turning their contents into text.

Idle locking uses session activity and the session's `lock-command`. Explicit
`lock-client`, `lock-session` and `lock-server` use each affected session's
command. An empty command leaves the client unlocked; locked clients are not listed by
`list-clients`. Prompt history is kept per type (`command`, `search`); it loads
from `history-file` after configuration and is saved as `type:line` when the
server exits. Lines without a known type load as command history, and a
`history-file` that is neither absolute nor under `~/` is ignored. Empty client
invocations use `default-client-command`. Command aliases expand once, with caller
arguments appended to the last command; key bindings are parsed when bound, so
their aliases expand then and unknown commands or flags are rejected (argument counts
are checked when the command runs, not when it is bound). `remain-on-exit key`
and `failed-key` keep qualifying panes until a key that no binding takes dismisses
them (read-only clients included); paste and mouse reports do not dismiss them.

## Windows

ConPTY and Windows console VT input use the same option paths. Windows has no
system terminfo database: host database lookup is explicitly a no-op under
`cfg(windows)`; native VT defaults and explicit overrides remain available.
Locking runs the configured Windows command on the client's console; it does
not invoke a Unix screen locker or lock the Windows desktop. Unix process
suspension is unavailable on Windows (the existing SIGTSTP path is Unix-only).

## Validation

Built with `CARGO_BUILD_JOBS=2`. `cargo test --release --no-fail-fast` and
`cargo check --target x86_64-pc-windows-gnu --tests` are run for every change; the
only Linux failures are the existing baselines `format_loops_and_utf8_length` and
`readonly_terminal_blocks_keys_paste_mouse_and_shared_resize`. Behaviour was
compared against tmux 3.8 in the reference pod.
