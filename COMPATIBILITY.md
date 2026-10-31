# tmux compatibility investigation

The reference for this investigation is **tmux 3.6a**, using the Linux x86_64
static binary from the [official tmux builds release](https://github.com/tmux/tmux-builds/releases/tag/v3.6a).
The archive SHA256 is `c0a772a5e6ca8f129b0111d10029a52e02bcbc8352d5a8c0d3de8466a1e59c2e`.
The reference runs under WSL; the candidate runs natively on Windows with ConPTY.
Each comparison creates separate private servers and shuts them down afterward.

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

## Differences retained for a reason

- PowerShell is the default shell: this is the project's stated purpose.
- Windows uses ConPTY and named pipes, and identifies users with SIDs. Unix uses
  ptys, Unix sockets and uids. Windows pipe access changes require recreating the
  pipe because its ACL is established when the pipe is created.
- Windows cannot perform Unix job-control suspension. `suspend-client` has no
  equivalent there. Shell-dependent locking requires a usable `lock-command`.
- Window dimensions remain capped at 1000 by 1000. The current terminal emulator
  allocates the screen eagerly; allowing 10000 by 10000 would allocate 100 million
  cells per pane. Raising the limit requires a different allocation strategy.
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
