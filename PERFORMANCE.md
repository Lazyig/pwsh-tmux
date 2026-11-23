# Performance comparison with tmux 3.8 (2026-11-23)

This file compares pwsh-tmux with tmux 3.8 on Linux before and after the
performance work merged on this branch. The work was meant to change resource
use and response time only. Every change was checked against the previous
build for identical output (see "Behaviour checks").

## Method

- **Host.** A Kubernetes pod on one node (Intel i3-12100, Linux 6.8). It had a
  cgroup limit of 2 CPUs (`cpu.max 200000 100000`) and 3 GiB of memory. The
  node is shared, so only results from the same run are compared.
- **Implementations.** Five, all run by the same harness in one run:
  - tmux 3.8 (Alpine package, dynamic musl with libevent and ncurses).
  - before: pwsh-tmux 849b998. Its `src/` and `vendor/` match release 0.2.0
    (72e7957).
  - after: this branch with all eight work packages merged.
  - Both pwsh-tmux builds were built twice: static musl, which uses the same
    libc and malloc as tmux, and static glibc, like the released Linux build.
- **Harness.** `tools/perf/bench.py` ran with `REPS=3 IDLE_SECS=30` and all
  groups. Each implementation used a private `-L` socket, `-f /dev/null`,
  busybox `sh` as the shell, `TERM=xterm-256color` and a 120x40 client pty.
  - The order of the implementations alternated between repetitions.
  - The idle group measured all five implementations in the same 30-second
    window.
  - Values are medians of three runs.
  - CPU is utime+stime from `/proc`, with a 10 ms clock tick.
  - Wakeups are voluntary plus involuntary context switches of all threads.
- **Throttling.** CPU throttling of the cgroup was recorded.
  - It was zero for the throughput, 5-client and quiet latency tests.
  - The flood latency test saturates the CPU and was throttled for every
    implementation, 18-142 ms per 20-40 s run. Treat those rows as indicative.
- **Raw data.** The raw samples and the full table are outside the
  repository. `tools/perf/summarize.py` rebuilds the table from a `bench.py`
  result file.

## Results

Lower is better. The last column is the after musl build divided by tmux.

| Measurement | tmux 3.8 | musl before → after | glibc before → after | after musl / tmux |
|---|---:|---:|---:|---:|
| Key echo, quiet: p50 / p99 (ms) | 0.28 / 2.06 | 4.78 / 11.6 → 1.43 / 6.72 | 4.18 / 11.1 → 0.96 / 5.27 | 5.1x / 3.3x |
| Key echo, other pane flooding: p50 / p99 (ms) | 1.16 / 9.07 | 62.4 / 219 → 4.68 / 15.9 | 15.8 / 74.3 → 3.18 / 17.4 | 4.0x / 1.75x |
| Server CPU during that flood (s) | 4.62 | 36.6 → 6.85 | 14.4 → 5.88 | 1.48x |
| 50 MB text, no client: server CPU (s) | 2.49 | 6.47 → 1.90 | 4.33 → 1.86 | 0.76x |
| 50 MB text, one client: server CPU (s) | 2.86 | 9.02 → 2.57 | 5.93 → 2.15 | 0.90x |
| 50 MB colour, no client: server CPU (s) | 4.40 | 4.61 → 2.33 | 3.99 → 1.86 | 0.53x |
| 50 MB colour, one client: server CPU (s) | 6.57 | 6.36 → 2.82 | 4.85 → 2.16 | 0.43x |
| 5 clients + 50 MB: server CPU (s) | 4.11 | 19.8 → 4.42 | 8.80 → 3.30 | 1.08x |
| copy-mode on 50,000 lines of history, until drawn (ms) | 14.7 | 324 → 8.4 | 166 → 4.5 | 0.57x |
| resize-window 200x60 ↔ 80x24 with 50,000 lines (ms per cycle) | 18.8 | 298 → 9.0 | 193 → 7.9 | 0.48x |
| Regex search, 50,000 lines: hit / miss (ms) | 417 / 212 | 491 / 283 → 433 / 230 | 164 / 91 → 75 / 38 | 1.04x / 1.08x |
| Server start, `new -d` p50 (ms) | 23.8 | 54.0 → 12.3 | 54.1 → 11.4 | 0.52x |
| Attached, clock + copy-mode + status-interval 1: server wakeups/s | 2.1 | 211 → 3.7 | 209 → 1.5 | 1.75x |
| Same: server CPU per 30 s (ms) | 20 | 200 → 50 | 160 → 40 | 2.5x |
| Same: client wakeups/s | 0 | 20 → 0.03 | 20 → 0.03 | - |
| Same: server RSS (MiB) | 3.3 | 13.7 → 7.0 | 16.8 → 8.9 | 2.1x |
| 80 idle panes: server CPU per 30 s (ms) | 0 | 150 → 90 | 100 → 70 | - |
| Server CPU for 1000 `display -p` (s) | 0.34 | 0.83 → 0.59 | 0.59 → 0.50 | 1.74x |
| Per-pane memory (KiB) | 7.4 | 30.4 → 31.4 | 98 → 96 | 4.2x |

- **Bytes written to the terminal.** pwsh-tmux merges frames, so it writes
  far fewer bytes than tmux during bulk output: 1.5 MiB against tmux's 35.8
  MiB for the 50 MB text test. The after build writes fewer bytes than the
  before build (4.2 MiB) because it finishes the same input sooner and draws
  fewer intermediate frames.
- **Small increases.** No server CPU measurement rose by more than 5%.
  - Idle RSS of the musl build rose by 0.14-0.23 MiB because the binary is
    larger. Anonymous memory stayed within 1%.
  - Two glibc wall-time command timings, `send-keys` and
    new-window/kill-window, read 8-13% higher, with wide ranges. These timings
    are mostly fork/exec, and their server CPU went down.

## What changed

| Package | Change | Main effect |
|---|---|---|
| WP1 | A client connection waits on `poll` and a wake pipe instead of sleeping 5 ms between reads (Unix). | Quiet key latency, idle wakeups |
| WP2 | Theme colours are worked out only for panes that enabled mode 2031, as tmux does. | Idle CPU with many panes |
| WP3 | The client backs off from 1 ms when connecting to a starting server. It waits for SIGWINCH instead of polling every 100 ms, and falls back to polling when the signal cannot arrive (Unix). | Server start, client wakeups, resize response |
| WP4 | Pane output in flight is limited by credits. A pane's exit is reported only after its last output. | Key latency during floods |
| WP5 | vt100 reuses row storage, compacts rows once and moves fitting lines whole on reflow. | Resize, colour throughput |
| WP6 | The vt100 parser passes runs of plain bytes to `vte` at once and has an ASCII fast path. | Text throughput |
| WP7 | Each client reuses its previous frame, and cells are written in place. | Render CPU with clients attached |
| WP8 | copy-mode reads lines from a frozen screen on demand instead of copying the whole history. | copy-mode enter and exit, memory, search |

## Behaviour checks

- **After each merge:** `cargo test --release --locked`, `tools/test_vt100.py`,
  the clippy set from VALIDATION.md (Linux and `x86_64-pc-windows-gnu`), and
  `cargo check --target x86_64-pc-windows-gnu --all-targets` with no warnings.
- **The vt100 changes** were checked with `tools/vt100-diff` against the
  previous commit. These all matched:
  - 100,000 random cases;
  - 400 resize sequences;
  - 50 MB text, 50 MB colour and wide-character files, with resizes.
- **Copy mode.** `tests/copy_compat.sh`, `tests/copy_search_compat.sh` and
  `tests/grid_metadata.sh` give byte-identical output before and after.
- **capture-pane.** `tools/perf/capcmp.sh` hashes of the history, joined and
  `-N` captures, the cursor and the history size match before and after.
- **Each package's own checks.** These are recorded in the work package
  reports:
  - fault injection for clients;
  - ordering of a pane's last output and its exit;
  - signal-mask inheritance;
  - comparison of render output.

## Remaining gaps against tmux 3.8

- **Key latency.** p50 is still about 1.2 ms higher when quiet and about 3.5 ms
  higher during a flood. Pane output is parsed on the main loop, and the client
  relays output through a socket instead of tmux's direct tty writes.
- **Threads and per-pane memory.** Each pane has three threads (242 for 80
  panes against tmux's 1) and uses about four times tmux's memory. Merging
  the threads would change how a pane's exit is ordered, so it was left for
  later.
- **Command cost.** Each command connection starts a thread. Server CPU per
  `display` is 1.7x tmux's, and per new/kill-window it is 2x.
- **Idle work with many panes.** Periodic name and state checks scale with the
  number of panes: 90 ms of CPU per 30 s for 80 panes, where tmux uses none.
- **Base RSS.** The static binary maps about 5 MiB of text. The musl build's
  anonymous memory is lower than tmux's: 0.75 MiB against 0.86 MiB for one
  pane.
- **musl allocator.** The musl build is slower than the glibc build on
  allocation-heavy paths, such as copy-mode search and floods. The glibc
  build, which is what the Linux release uses, is faster than tmux there.

## Windows

None of these numbers were measured on Windows. Only `cargo check` and clippy
for `x86_64-pc-windows-gnu` ran on this branch. The changes behave differently
on Windows in these ways:

- **Connection waits.** WP1 does not change how Windows waits on a named
  pipe. Connection pipes are non-blocking (`PIPE_NOWAIT`) and are still
  checked every 5 ms. That adds about 2.5 ms of key latency and about 200
  wakeups per second per client. Removing it needs overlapped pipe I/O, which
  is deferred until it can be tested on Windows.
- **Client loops.** WP3's signal-driven size and pause waits are Unix only.
  Windows keeps its 100 ms console size poll.
- **Pane output credits.** WP4's credits apply to ConPTY too. Unix decides
  that a pane is finished by polling the terminal. Windows has no such signal
  and uses a 50 ms quiet period with an upper bound.
- **Shared code.** WP2, WP5, WP6, WP7 and WP8 are platform-independent.

### Checks still needed on a Windows machine

1. **Connections (WP1):**
   - attach and detach;
   - kill-server;
   - the server survives a client that is killed;
   - the 64-client limit;
   - a user allowed by the pipe ACL can connect;
   - a control-mode client exits cleanly;
   - the full `cargo test` passes on Windows;
   - after a client is removed, its writer thread ends at once.
2. **Server start (WP3):**
   - cold-start time;
   - two clients starting a server at once leave one server;
   - a slow start, for example during a Defender scan, still connects within
     5 s.
3. **Client loops (WP3):** smoke-test console resize, lock-client and
   run-shell (keys are not taken), suspend and detach.
4. **Pane output (WP4):**
   - with remain-on-exit, `1..20000; exit` in pwsh keeps its last line;
   - kill-pane and kill-server during a flood do not hang;
   - ClosePseudoConsole does not block on older conhost (Windows 10, Server
     2022);
   - client bytes, CPU and key latency during a flood compared with the
     previous build.
5. **Screen output (WP6, WP7):**
   - pwsh output, progress bars and output heavy in EL/CUP sequences look the
     same as with the previous build;
   - text with U+00A0, °, ± and cp949 console output looks the same;
   - the Windows ASCII and Unicode sets of UI_COMPARISON show no new
     differences (`tools/ui_report.py`);
   - console client output bytes match the previous build
     (`PWSH_TMUX_OUTPUT_COPY`);
   - the ConPTY accent-composition path renders the same.
6. **copy-mode (WP8):**
   - enter and exit with a large history;
   - page and scroll moves;
   - long `-N` word and space moves;
   - rectangle and wide-character/tab selections;
   - search;
   - a width-changing resize in copy-mode;
   - `capture-pane -M`.
