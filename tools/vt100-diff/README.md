# vt100-diff

A differential checker for the bundled `vendor/vt100`. It feeds the same input
to the vt100 of a base commit and to the working tree's `vendor/vt100` and
compares everything pwsh-tmux can read back. Use it to show that a change to
vt100 (a performance change, a refactor) leaves the behaviour unchanged.

It is a development tool only. The crate is its own Cargo workspace, so it is
not part of the pwsh-tmux build, `cargo test` or the release archives. It
writes only under `tools/vt100-diff/target/`.

## Running

```sh
tools/vt100-diff/run.sh [--base REV] MODE [ARGS...]
```

`run.sh` extracts `vendor/vt100` at the base revision with `git archive` into
`target/vt100-base` and renames that copy's package to `vt100-base`. It then
builds the checker in release mode (`CARGO_BUILD_JOBS` defaults to 2) and runs
it. The base revision is `--base REV`, else `$VT100_DIFF_BASE_REV`, else
`git merge-base HEAD main`, else `HEAD`. On a branch made from another
branch than `main`, pass that branch (`--base BRANCH`) so the base is the
code the change started from. The working copy is always the
`vendor/vt100` of the checkout that contains this `run.sh`.

| Command | What it does |
|---|---|
| `run.sh fuzz 100000` | Random cases: escapes, invalid UTF-8, wide and combining characters, OSC/DCS/APC/PM strings, random chunk boundaries, sizes (1-8 x 1-30), history (0/7/14). Also mixes in the pane settings the server applies (alternate screen allowed, scroll-on-clear, codepoint-widths, input-buffer-size, history time) and occasional resizes and clear-history. About 80 s for 100k cases with `-j 2`. |
| `run.sh resize-fuzz` | 400 seeds x 60 steps on a 10x30 pane with 300 history lines. Each step writes random text (alternate screen, scroll regions, wide characters, links, OSC 133 marks), then resizes; a third of the resizes change only the height. The full state is compared after every write and every resize. About 15 s. |
| `run.sh cmp FILE [--size RxC] [--hist N] [--chunk N] [--onlcr] [--time] [--resize RxC,...]` | One file fed in chunks (default 39x120, 2000 history, 4095-byte chunks). Compares the final state and the state after each listed resize. `--onlcr` turns `\n` into `\r\n` as a pty would. |
| `run.sh bench FILE [same options] [--reps N]` | CPU time of base and work for feeding the file and for each resize, median [min] over N runs (default 5) in alternating order. No comparison is made. |
| `run.sh selfcheck [FUZZ_N] [RESIZE_N]` | Checks the checker. It plants each mutation from `inject.py` into a copy of the base and requires the listed modes to report a mismatch. Each mutation changes one value in every 499th event: a wrapped flag, a row flag, a history time or one character. |

Common options: `-j N` (worker processes, default 2), `--start S` (first seed),
`--allow-panic`, `--strict-panic`, `--log-cap BYTES`, `--out DIR`, `--quiet`.
Exit status: 0 for no difference, 1 for a difference, 2 for a usage error, and
3 when a case panicked identically on both sides (unless `--allow-panic`).

Typical gate for a change under `vendor/vt100`:

```sh
tools/vt100-diff/run.sh fuzz 100000
tools/vt100-diff/run.sh resize-fuzz
tools/vt100-diff/run.sh cmp /work/data/text60k.txt --onlcr --hist 50000 \
    --resize 60x200,24x80,60x200,24x80,39x120,24x40,39x120
tools/vt100-diff/run.sh cmp /work/data/colour50.txt --onlcr
```

`gen_data.py` in `tools/perf` writes those data files (see its README).

## What is compared

Each side writes a log, and the two logs must be identical. The log holds:

- every callback with its arguments and the cursor position at the time of
  the call, including `on_scroll` rows;
- after each operation in `fuzz`: the cursor, the size and the history length;
- at each dump: size, cursor, scrollback view, history lengths and counters,
  all modes, the current pen, `contents_formatted`, `contents`,
  `attributes_formatted`, `input_mode_formatted` and `cursor_state_formatted`;
- every history row (`history_row`, which includes the visible rows), the
  saved normal-screen rows while the alternate screen is active, and the
  visible cells.

For each row the log holds the wrapped flag, the flags, the time, the used
length, the metadata (tab widths and links) and each cell. A cell is read
through the public getters: text, wide and continuation flags, every attribute
bit, the underline style and the three colours.

Three things are normalised, because pwsh-tmux cannot observe them:

- **Trailing blank cells.** An empty cell with default attributes at the end
  of a row is dropped. A history row may be stored narrower than the screen,
  and the application reads a missing cell as a blank.
- **Internal representation.** Cells are compared through getters, not with
  `Debug`, so stale bytes in a reused cell do not count.
- **Hyperlink numbers in metadata.** These are renumbered in order of first
  appearance. The `tmuxN` external ids in the formatted output are compared
  as they are.

vt100 numbers hyperlinks with a process-wide counter. To keep the two copies
in step, every case runs on both sides, in order, in one thread. `-j` splits
the seeds over child processes, never threads. If a mismatch does not repeat
with `run.sh MODE 1 --start SEED`, the difference came from state carried over
from an earlier case in the same process.

## Panics

Any panic is a failure. A case where only one side panics is a difference
(exit 1); when both sides panic the same way, the run still ends with exit 3
after the summary, which lists the panic sites and the first panicking seeds.

vt100 up to 72e7957 panics on about 1% of fuzz cases (`Option::unwrap()` in
`screen.rs`, `text_cleanup` and `text`); ebc077c fixed that. A base from
before the fix also behaves differently from the fixed code on those inputs,
so compare against a base that contains the fix. To compare two copies that
both lack it, pass `--allow-panic`: a case where both sides panic then counts
as a match when the panics come from the same operation, with the same
message, in the same source file. Line numbers are ignored because edits move
them; `--strict-panic` compares them too. `--deny-panic`, the default, is
still accepted.

## When it reports a difference

The checker stops at the first mismatching seed. It prints the first differing
log line of each side and the operation before it, then writes `base.txt`,
`work.txt`, `input.bin` and `case.txt` (sizes, settings and operations) to
`target/vt100-diff-out/` (`shard<k>/` when `-j` is greater than 1). To rerun
that seed alone, use `run.sh fuzz 1 --start SEED` or
`run.sh resize-fuzz 1 --start SEED`.

Seeds are tied to this version of the generator. Changing the generators
changes the cases behind each seed.
