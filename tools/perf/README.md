# tools/perf

Benchmark tools that compare pwsh-tmux with tmux 3.8, or one pwsh-tmux build
with another, on the same Linux host. They are development tools only and are
not part of the build or `cargo test`.

| File | Purpose |
|---|---|
| `bench.py` | The benchmark harness. It runs on the benchmark host and starts every implementation the same way. |
| `render_latency.py` | How long pane output takes to reach an attached client's terminal (a pty it reads, optionally slowly with `READ_RATE`): the last line of a 50 MB `cat` and of `seq`, lines of a flood and of 100 lines/s, and key echo beside a flood. Runs on any Linux host. |
| `summarize.py` | Turns one or more `bench.py` result files into a markdown table and `summary.json`. |
| `gen_data.py` | Writes the input files `bench.py` reads (`text50`, `colour50`, `text60k`). The output is deterministic; the files are not committed. |
| `capcmp.sh` | Feeds a file to a fresh pane and prints hashes of what capture-pane returns. Use it to compare two binaries. |
| `pexec.sh` | Runs a shell script from stdin inside the benchmark pod. |
| `put.sh` | Copies a file into the pod in verified chunks. |
| `sync.sh` | Copies the harness files into the pod (default `/work/h`). |

## The benchmark pod

`pexec.sh`, `put.sh` and `sync.sh` reach a Kubernetes pod with
`kubectl exec`. Its location is not stored in the repository. Set
`PERF_KUBECTL` (the kubectl command, including any wrapper or context flags),
`PERF_NS` and `PERF_POD` in one of these places:

- a file named by `PERF_ENV`;
- `tools/perf/pod.env` (ignored by git; see `pod.env.example`);
- the environment.

The pod is expected to have tmux 3.8 and its userland unpacked under
`/work/root`, with wrappers `/work/py` (Python) and `/work/tmux`.
`pexec.sh` puts `/work/root` on the library, terminfo and Python paths and
starts in `/work`. Override that preamble with `PERF_PROLOGUE`.

```sh
export PERF_ENV=/path/to/pod.env
tools/perf/sync.sh                                   # harness -> /work/h
tools/perf/put.sh target/x86_64-unknown-linux-musl/release/pwsh-tmux /work/bin/wp5-musl
echo 'chmod +x /work/bin/wp5-musl' | tools/perf/pexec.sh
```

## bench.py

```sh
REPS=3 IDLE_SECS=30 /work/py /work/h/bench.py OUT.json GROUPS NAME=BIN [NAME=BIN ...]
```

- `GROUPS` is a comma list of `idle`, `memory`, `throughput`, `latency`,
  `commands` and `heavy`, or `all`.
- Each implementation runs with `-f /dev/null` on a private socket named
  `b-NAME-TEST`. Give every implementation a name that no one else running
  on the same host uses, for example a work-package prefix.
- Repetitions alternate the order of the implementations. `idle` runs all of
  them at once, in the same window.
- The raw samples are rewritten to `OUT.json` after every test.
- The inputs live in `$BENCH_DATA` (default `/work/data`). They are created
  with `gen_data.py` on first use, which takes about 6 s.

Environment variables: `REPS` (default 3), `IDLE_SECS` (default 30),
`BENCH_DATA`.

The host may be shared. Absolute numbers drift, so compare a base build and a
candidate in the same run, with `REPS` of at least 3, and compare medians.
The throughput, latency and 5-client flood tests record the cgroup CPU
throttling that happened during the test (`throttled_ms`).

## summarize.py

```sh
/work/py /work/h/summarize.py [--ref NAME] [--max-throttled-ms MS] OUT.json [MORE.json ...]
```

The script prints `median [min-max] (n)` for each implementation and the
ratio to the reference. The reference is `--ref`, else `tmux` if present,
else the first implementation. Samples from several files are merged.
`--max-throttled-ms MS` leaves out repetitions whose test recorded more
throttling than `MS`, and the table shows how many were left out. Use
`--max-throttled-ms 0` for latency verdicts. The script also writes
`summary.json` next to the first input.

## capcmp.sh

```sh
tools/perf/capcmp.sh BIN FILE [COLSxROWS]
tools/perf/capcmp.sh --cmp BASE CAND FILE... [-- COLSxROWS]
```

The script starts a private server (default size 120x39) and runs `cat FILE`
in a new window. It waits until the capture stops changing, then prints:

- the sha1 (first 12 hex digits) of `capture-pane -e -S - -E -`;
- the same for `-J -S -` and for `-N -S -`;
- `#{history_size} #{cursor_x},#{cursor_y} #{pane_title}`.

`--cmp` runs both binaries on each file and prints `SAME` or `DIFF`. It exits
with status 1 if any file differs. Sockets are named
`${CAPCMP_PREFIX:-cap}-PID-TAG`, `HOME` is a temporary directory, and every
server is killed on exit. It works with tmux too, but run tmux 3.8 only
inside the pod.
