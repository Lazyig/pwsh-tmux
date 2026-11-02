# Release validation - 2026-11-01

The 0.1.1 release candidates were built from the current working tree using
Rust 1.99.1. After the final review, the validated Windows executable replaced
the existing user PATH installation; the previous file was backed up in the
workspace. The source repository is published at
[Lazyig/pwsh-tmux](https://github.com/Lazyig/pwsh-tmux).
These results describe the tested platforms and behaviours, not a guarantee
that every vulnerability or compatibility difference has been eliminated.

The terminal UI repair and final review are documented in
[`UI_COMPARISON.md`](UI_COMPARISON.md). Linux Unicode and Windows ASCII runs each
match 51 of 52 screens; the remaining screen differs only in a runtime pane ID.
Windows Unicode still has documented ConPTY-path differences; complete UI
identity across platforms is not claimed.

## Review and repair rounds

1. Rechecked the previous read-only fixes and added a regression for switching
   clients with an empty session list. The command now returns an error instead
   of dividing by zero if that state is encountered.
2. Ran the complete suite natively on Linux. An older WSL kernel could block an
   8 MiB write for more than a minute despite the nonblocking flag. Linux sends
   now use bounded chunks and explicit `MSG_DONTWAIT`; a regression asserts
   that a stalled write respects its deadline and returns within one second.
3. Reproduced cumulative reply amplification: individually limited formats
   could accumulate beyond the transport budget before framing. Replies now
   have a 4 MiB aggregate cap, and subscriptions have aggregate configuration
   and cache limits. Unit, real control-client and separate-UID regressions pass.
4. Built release archives, extracted their executables and reran the full
   integration, security, SSH and compatibility checks against those files.
   Both executables have matching hashes for their source/Cargo manifests.
5. Repaired the measured UI differences, including menus, copy-mode cursor and
   selection, borders, chooser previews, prompt/status layout and modal command
   completion. Rechecked parked-queue permissions and after hooks. Added bounded
   normal-screen reflow and opt-in scroll-on-clear to a bundled vt100 0.18.1;
   the release provenance manifest includes its source and license.
6. Running the optimized Windows package exposed PID reuse forming a cycle in
   a process snapshot. Descendant traversal now visits each PID once. Cycle,
   self-parent and console-host regression cases prevent unbounded traversal.
7. The subsequent code review reproduced stdin-buffer command injection, silent
   fallback from an unknown window destination, orphan popup processes after
   overlay replacement, and startup config paths resolved from the server home.
   Version 0.1.1 encodes binary stdin and rejects legacy requests before parsing,
   fails unknown explicit destinations without changing session links, destroys
   replaced popup panes, and resolves config paths before changing server cwd.
   Regression tests and the original isolated reproduction probes cover all four.

## Results

| Check | Result |
|---|---|
| Windows 11 native unit tests | 191 passed |
| Windows 11 ConPTY/CLI integration against packaged `tmux.exe` | 34 passed |
| Ubuntu 18.04, x86_64, WSL 1: native Linux unit tests | 189 passed |
| Linux PTY/CLI integration against packaged `tmux` | 34 passed |
| Real UID boundaries: server owner, reader and unlisted user | 30 passed |
| Windows SSH client to Linux package: host-key verification, detach, attach, split, Unicode input, reconnect | Passed |
| Windows package versus tmux 3.6a | 62 matched, zero differences |
| Linux package versus tmux 3.6a | 62 matched, zero differences |
| RustSec / yanked / maintenance audit | 97 lockfile entries; zero vulnerabilities or warnings |
| Clippy correctness and suspicious groups | Passed |
| Source and Git-history secret scan | 64 public candidate files; zero secret findings |
| tmux 3.6a terminal UI, Linux Unicode and Windows ASCII | 52 cases each; 51 exact visible matches and one runtime-ID-only difference; state, copy, input modes and CLI wait all match |
| Archive contents, checksums, source provenance and embedded build paths | Passed |
| Windows PE imports | OS DLLs only; no external Visual C++ runtime |

The default SSH test returns early unless configured. The SSH result above came
from a separate configured run against a temporary loopback sshd, using a pinned
host key and strict host verification. The temporary server was stopped afterward.
The UID test changed only child-process credentials and temporary directories;
it did not add accounts, install executables or modify an existing server.

Default strict Clippy style checks still report existing style and complexity
warnings; they are not represented as a clean all-warning lint result. No
correctness/suspicious-group failure remains in the current validation.
Windows Server 2022, macOS, BSD, ARM and real distinct Windows logon accounts
were not validated in this run. Platform support must not be inferred from
Linux UID tests alone. No independent human security certification is claimed.

## Repeating the checks

The subsequent compact-history optimization was checked separately against the
source-tree release builds. Its measurements, workload limits and updated test
counts are in [MEMORY_COMPARISON.md](MEMORY_COMPARISON.md). This does not change
the provenance or results of the previously packaged 0.1.1 release above.
`python tools/test_vt100.py` runs the bundled library's unit and doc tests;
application `cargo test` does not run dependency unit tests.

The color follow-up checks a truecolor usage bar whose unused area consists of
colored spaces. A native Windows console no longer inherits `TERM=dumb` color
capabilities from a launcher, and Windows panes now advertise the configured
virtual terminal and truecolor support just like Unix panes. Explicit terminal
names, Unix `TERM=dumb`, and application `NO_COLOR` preferences are preserved.

The later latency follow-up (ioctl terminal size on Unix, row-range
`capture-pane`, `wait-for -S` before a waiter) brought Windows 194 / Linux 192
unit tests and 36 local integration tests per platform, all passing, including
the real-terminal dark-background progress-bar regression.

The mouse follow-up compared `set -g mouse on` behaviour with tmux 3.6a using
`tests/mouse_compat.py` (80 of 80 scenarios match; see COMPATIBILITY.md). Afterwards
Windows 198 / Linux 196 unit tests and 36 local integration tests per platform pass,
and the Windows ASCII UI comparison still has 51 exact matches of 52, the remaining
case differing only in runtime pane IDs.

```text
cargo test --locked
cargo clippy --locked --all-targets -- -A clippy::style -A clippy::complexity -A clippy::perf -D clippy::correctness -D clippy::suspicious
python tools/build_release.py
```

Set `PT_TEST_BIN` to an extracted release executable before running `cargo test`
to test the actual package. On Unix, run the UID suite as root:

```text
python3 tests/security_multiuser.py /absolute/path/to/extracted/tmux
python3 tests/tmux_compat.py --candidate /absolute/path/to/extracted/tmux --reference /path/to/tmux-3.6a
```

For Windows-to-WSL reference comparison and configured SSH tests, see
`COMPATIBILITY.md` and `tests/ssh.rs`. Verify archive `.sha256` files and the
included `SHA256SUMS` / `BUILDINFO.json` before distributing the executable.
