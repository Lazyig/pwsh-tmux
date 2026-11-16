# tmux compatibility investigation

The compatibility reference is now **tmux 3.8**: configs, themes and scripts written for
3.8 should behave the same. Behaviour is compared against a stock tmux 3.8 build running
in a separate reference environment, with the same scripts run against a local build, and
the 3.8 C source is the authority for defaults and edge cases.

The sections below record the original investigation against **tmux 3.6a** (the Linux
x86_64 static binary from the [official tmux builds release](https://github.com/tmux/tmux-builds/releases/tag/v3.6a),
archive SHA256 `c0a772a5e6ca8f129b0111d10029a52e02bcbc8352d5a8c0d3de8466a1e59c2e`, run
under WSL against the candidate on Windows with ConPTY). Each comparison creates separate
private servers and shuts them down afterward. Where 3.8 changed behaviour, the code
follows 3.8.

## Differences corrected

| Area | Previous behaviour | Corrected behaviour |
|---|---|---|
| Detached session height | Subtracted a status row from `new-session -d -y` | Treats the requested height as the pane area, including for control clients |
| `select-layout -E` | Recursively evened every split in the window | Spreads the target pane's siblings, climbing to ancestors only if already even; honours pane targets and unzooms |
| `select-layout -o` | Accepted the flag but ignored it | Saves and restores the previous layout; repeated calls toggle the layouts |
| Layout cycle | First `next-layout` skipped even-horizontal | Starts at even-horizontal; previous starts at tiled; includes both mirrored main layouts |
| Layout names | Accepted a few hardcoded abbreviations | Accepts exact names and unique prefixes, rejecting ambiguous prefixes |
| Pane indices | Derived indices from the geometric layout tree | Maintains a pane list independently, preserving indices when layouts move panes |
| Format loops | `S`, `W`, `P` were missing | Expands nested loops in the appropriate session/window/pane context, including active alternatives and sorting/reversal |
| Pane content search | `C` always returned zero | Returns the first matching visible row, one-based, with glob/regex and ignore-case support |
| Format substitution | Minimal regex lacked groups, alternation and backreferences; applied only the first `s` modifier | UTF-8 regex with leftmost-longest matching, captures and backreferences; applies substitutions in sequence |
| UTF-8 length | `n` counted characters | Counts UTF-8 bytes, as tmux does; `w` remains display width |
| Invalid format syntax | Could emit trailing fragments after a malformed format | Stops expansion at syntax errors, matching tmux |
| Arithmetic | Missed the `m` modulus operator | Supports `m` as well as `%` |
| Copy-mode bracket motion | Missing commands and default bindings | Implements next/previous matching brackets, vi `%`, and emacs `C-M-f`/`C-M-b` |
| Copy-mode defaults | Added an unrequested vi `y` binding | Leaves `y` unbound, as in tmux; Enter copies |
| Copy cursor formats | Reported x and y in reverse | Reports column as x and row as y |
| Copy-mode search | Searched only literal strings | Uses regex for ordinary searches and literal patterns for the `-text` commands |
| Incremental search | Waited for Enter before searching | Updates while typing, restores the original position on cancel, and accepts without advancing again |

The original even-layout remainder distribution was already correct for tmux 3.6a.
tmux 3.5a uses a different distribution; it was not adopted as a compatibility fix.
Likewise, chained format conditions and two-argument conditions are valid in 3.6a
and were retained after checking the reference.

The reference implementations used for inspection are
[layout.c](https://github.com/tmux/tmux/blob/3.6a/layout.c),
[cmd-new-session.c](https://github.com/tmux/tmux/blob/3.6a/cmd-new-session.c),
[format.c](https://github.com/tmux/tmux/blob/3.6a/format.c), and
[window-copy.c](https://github.com/tmux/tmux/blob/3.6a/window-copy.c).

## Mouse (`set -g mouse on`)

[`tests/mouse_compat.py`](tests/mouse_compat.py) attaches a client to private servers of
both programs inside a pseudo terminal, writes the same SGR mouse reports and compares
the key events that fired (through logging bindings), the active pane and window, pane
geometry, copy-mode state and selection, paste buffers, menu results and the bytes an
application in the pane received. It runs on Linux or WSL:

```sh
python3 tests/mouse_compat.py --reference /path/to/tmux-3.6a --candidate target/debug/pwsh-tmux
```

The first run against tmux 3.6a matched 2 of 62 scenarios. After the changes below, all
**80 scenarios** match. They cover clicks, second/double/triple clicks and their timing,
drags and drag ends on panes, borders and status lines, modifiers, status-line ranges,
the default root and copy-mode bindings, menus, choose-tree and clock mode, mouse
applications, the alternate screen and `mouse off`.

| Area | Previous behaviour | Corrected behaviour |
|---|---|---|
| Click events | A second press fired `DoubleClick` at once, only at the same cell | `SecondClick` fires at once, `DoubleClick` only after 300 ms without a third press, `TripleClick` on the third (tmux click timer) |
| Drag events | Drags repeated `MouseDrag` and ended with `MouseUp` | The first drag is named after where the button went down, the release fires `MouseDragEnd` where it happened, and drags followed by `resize-pane -M` or a selection send no keys |
| Status line areas | Every status click was `Status` | `StatusLeft`, `StatusRight`, `Status` (window, pane, session or user range) and `StatusDefault`, from the ranges drawn, including `#[range=...]` in `status-format` with tmux's closing rules |
| Key tables | Copy-mode mouse handling was built in | Mouse keys use the pane's mode table, then root; unbound keys go to the pane (or are dropped after a mode table), and the prefix table returns to root |
| Default bindings | A subset of tmux 3.5 bindings | The tmux 3.6a root, copy-mode and copy-mode-vi mouse bindings and menus, including word/line items, middle-click paste, the session menu and `alternate_on` |
| Copy mode | A click moved the cursor and cleared the selection; scrolling kept the cursor on its text; `select-word` used its own rules | Only the bound commands act; `send-keys -X` from the mouse moves to the mouse first and `begin-selection` starts a drag that scrolls at the edges; scrolling keeps the cursor row with tmux's end-of-line rule; `select-word` follows tmux's word reader |
| Menus | Positions and mouse handling differed; empty items were shown | tmux placement (`-x/-y` C, R, P, M, W, S and formats), releasing the button chooses or closes, `-O`, keyboard menus ignore the mouse, empty and doubled lines are dropped, items are expanded when the menu opens |
| Formats | No `mouse_*` positions or words | `mouse_x`, `mouse_y`, `mouse_word`, `mouse_line`, `mouse_pane`, `mouse_status_line`, `mouse_status_range`, `selection_start_*`/`selection_end_*`; `#{||:...}` and `#{&&:...}` take any number of arguments |
| Applications | Drags were reported at the press position; nothing reached the pane with `mouse off` | Events are reported where they happen, the delayed `DoubleClick` is not sent, and with `mouse off` the terminal reports what the active pane asked for |
| `run-shell -d` | Blocked the whole server while waiting | The command queue resumes from a timer |

Fixed on the way: `split-window -b` without a size gave the new pane the smaller half,
`switch-client -t =` ignored the window, a tree opened from the command line chose
through a client that was no longer connected, `display-menu`/`display-panes`/`display-popup`
replaced an overlay already shown (tmux leaves it), and `pane_left`/`pane_top` ignored
`pane-border-status` and zoom.

Since then `pane-scrollbars` (with the `Scrollbar*` mouse areas, slider drags and
`copy-mode -S`) is implemented, and the terminal emulator keeps tmux 3.8's line metadata
(OSC 133 prompt marks, OSC 8 hyperlinks, tab cells and history line times) for copy mode,
`capture-pane` and the mouse formats.

## Differences retained for a reason

- PowerShell is the default shell: this is the project's stated purpose.
- Windows uses ConPTY and named pipes, and identifies users with SIDs. Unix uses
  ptys, Unix sockets and uids. Windows pipe access changes require recreating the
  pipe because its ACL is established when the pipe is created.
- Windows cannot perform Unix job-control suspension. `suspend-client` has no
  equivalent there. Shell-dependent locking requires a usable `lock-command`.
- Window dimensions follow tmux's WINDOW_MAXIMUM of 10000 by 10000. Screen rows are
  allocated as they are written, so a large window that shows little costs little
  (an empty 10000 by 10000 pane uses about 11 MB).
- Configuration, executable and environment variable names use `pwsh-tmux` to
  coexist with stock tmux. The deployed executable may separately be named `tmux.exe`.

## Validation and limits

The initial CLI investigation passed all **62 differential cases**, **156 unit tests**, and
**28 local integration tests** (3 attach, 17 commands, 8 interactive).
`cargo build --release --locked --offline` also succeeded. The existing executable
on the user's PATH is not replaced by these source changes or build commands.

Later release/security validation is recorded in [`VALIDATION.md`](VALIDATION.md).
The subsequent real-terminal UI comparison found visual and interaction differences;
see [`UI_COMPARISON.md`](UI_COMPARISON.md). The 62 CLI cases do not establish UI equality.

Run the native regression suite with `cargo test --locked`. The remote SSH test
requires `PT_SSH` and `PT_SSH_BIN`; its default early return is not a remote test pass.
An attached-client regression checks incremental updates before Enter, cancellation,
and accepting the preview without jumping to another match.

Run the independent comparison harness against an installed reference binary:

```powershell
cargo build --locked
python tests/tmux_compat.py --candidate target/debug/pwsh-tmux.exe --reference C:/Windows/System32/wsl.exe -d Ubuntu --exec /path/to/tmux
```

The harness compares observable CLI output for layouts, layout restoration, pane
indices, format loops, regex substitution, Unicode, pane search, copy cursor
coordinates and bracket/search movement. Its JSON output reports the number of
cases and all differences, and it exits nonzero on differences. It does not
require changing the user's tmux installation, configuration or active servers.

This is a bounded investigation, not certification of complete tmux compatibility.
Every command flag, copy-mode command, locale-specific regex rule and newer tmux
feature has not been exhaustively tested. In particular, the complete default key
tables and generic `command-prompt -i` callbacks require
separate compatibility coverage. Unsupported details are implementation gaps,
not justified platform differences.

Security restrictions are deliberate differences: read-only control clients cannot
execute commands, remove read-only flags, run shell formats or change shared pane
state. Read-only sizes do not influence shared window sizes, and hooks caused by
restricted requests are suppressed. Resource bounds also reject oversized or
excessively costly input. See [`SECURITY.md`](SECURITY.md).

The existing repository does not follow default rustfmt formatting, so
`cargo fmt --all -- --check` reports differences throughout unchanged files as well.
The compatibility fixes preserve the existing style; `git diff --check` is used
to check whitespace in the patch.
