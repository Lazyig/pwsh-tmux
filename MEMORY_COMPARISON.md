# Memory comparison (2026-11-02)

Lossless history storage reduces the measured eight-window ASCII history case
from **110.05 MiB to 15.93 MiB (85.5%)**. History limits and Unicode cell capacity
are unchanged. Empty-screen memory is essentially unchanged.

| Condition | Original tmux 3.6a | Project before | Project after |
|---|---:|---:|---:|
| One empty window | 2.36 MiB | 6.37 MiB | 6.39 MiB |
| Eight empty windows | 2.41 MiB | 9.40 MiB | 9.43 MiB |
| One window with 1,900 history rows | 4.66 MiB | 19.45 MiB | 7.93 MiB |
| Eight windows, each with 1,900 history rows | 20.61 MiB | 110.05 MiB | 15.93 MiB |

These are Linux process RSS measurements on Ubuntu 18.04 under WSL1, not a
native Windows-to-Linux memory comparison. Both project builds are release
builds based on 0.1.1; "after" includes the compact-history changes. Each window
has one 209-column by 50-row pane and the server has one attached client.
Values sum server and client RSS, excluding child workloads, AI programs and
the terminal emulator. The reference column is from the post-change run;
before-change reference medians differed by at most 0.02 MiB.

Each condition ran five times with alternating implementation order. A sample
is the median of seven RSS readings; the table gives the median across the five
runs. Every history row contains 204 ASCII characters; history-limit is 2,000,
and all panes are checked for exactly 1,900 retained rows. Idle is measured first
and history afterwards on each fresh server. RSS can double-count shared pages;
WSL1 did not provide smaps_rollup, so these are not PSS or USS measurements.
These results do not predict the saving for arbitrary Unicode/color workloads.

The storage change shares immutable default-style ASCII cells and uses at most
128 additional distinct cells per row. Other cells retain their complete text,
style and width flags. Rows that cannot benefit remain dense. Reads use the
compact representation directly; edits expand only the affected row.

For 20,000 ASCII output rows of 204 characters with an attached client, median
wall time over five runs was 0.268 s before and 0.232 s after. This includes
command/polling overhead and establishes no slowdown in this workload; it is
not a general CPU benchmark or a guaranteed speedup.

Validation: Windows 192 / Linux 190 application unit tests; 34 configured local
integration tests on each platform; 4 bundled vt100 unit tests and 2 doc tests;
Clippy correctness/suspicious checks. Linux Unicode and Windows ASCII UI
comparisons each passed 52 cases: 51 exact visible matches and one differing
only in runtime pane IDs; state, copy, terminal modes and CLI waits matched.
The default SSH test did not perform a configured SSH connection in this run.

Reproduce on Linux/WSL with release Linux binaries:

```sh
python3 tools/benchmark_memory.py \
  --reference /path/to/original-tmux-3.6a \
  --candidate /path/to/project-release \
  --output target/memory-run
python3 tools/test_vt100.py
cargo test --locked
```

The benchmark creates unique private sockets and cleans up only its own
servers. It writes raw samples, binary hashes and medians to the output directory.
