# Copy-mode compatibility audit

Branch: `wp-copy`. Reference: the supplied tmux 3.8 `window-copy.c`,
`cmd-copy-mode.c`, `key-bindings.c`, `tmux.1`, and live reference pod.
The WIP commit was retained and reviewed; subsequent commits fix its selection
transitions. Earlier non-WIP commits were not rewritten.

## Implemented in this package

- Both default copy-mode tables match live tmux 3.8 `list-keys` output exactly
  (78 emacs and 88 vi bindings), including C-[, emacs C-l, and refresh-now on r.
- Line-number modes, number styles, current-line style, position format/style,
  selection/match/current-match/mark styles; mouse entry suppresses line numbers.
  Option changes repaint copy mode and update mode-keys, wrap-search and line numbers.
- refresh-on/off/toggle/now, with a 50 ms automatic refresh timer, selection/drag
  suspension, bottom following, and prohibition on refreshing another pane's copy.
- recentre-top-bottom, scroll-exit commands, vi scrolling relative to text,
  cursor limits, selection-safe scrolling, rectangular selection past line ends,
  and dragging to adjust an existing selection.
- Unicode whitespace in word motions and word-separators, including changes to
  session word-separators while copy mode is open. copy_cursor_word uses the
  pane's session options.
- copy-mode -k/-d/-S/-e/-u/-M/-H/-q/-s handling; -k kills on leaving mode.
- Read-only navigation and command restrictions matching the C command table;
  selection_mode and the other copy-mode position, selection and search formats.
  Clearing marks also clears search counts. capture-pane -M reads the backing copy.
- Selection endpoint tracking, line/word/character transitions, vi copied final
  newlines, wrapped-line extraction, wide characters, and copying a search match
  when no selection exists.

## Round 2 search compatibility

- Searches join physical rows only through soft wraps, preserving cell positions
  for wide characters. Regex subjects use tmux's 2000-cell joining limit; literal
  text can extend further. Empty regex matches are discarded, and lowercase
  queries use tmux's case-insensitive matching.
- Forward/backward regex, text and incremental commands, search-again/reverse,
  search-match copying, match/current-match styles and mark inversion use spans
  across rows. Incremental commands accept tmux's `=`, `+`, `-` prefix protocol
  and search the subsequent text literally.
- Counting uses a 200 ms all-history budget followed by a visible-context retry,
  with a shared 10 s deadline. Partial lower bounds use tmux's strict >10, >100,
  >1000 thresholds; timeout clears marks and blocks repeats until a new search.
  `search_timed_out`, `search_count_partial` and the position indicator reflect
  this state. Clock-injected unit tests cover budget boundaries and hard timeout
  without requiring timing-sensitive sleeps.
- `tests/copy_search_compat.sh` is a separate live reference/local fixture for
  both key modes and all search command forms, hard breaks, wrapped and multi-row
  regex/text matches, overlapping starts, wide cells, empty matches, smart case,
  repeat/reverse, viewport-clipped marks, match extraction and a large-history
  partial-count fallback (150000 rows of 79 matches).
  Compare it using the same helpers as the selection fixture below. The budget
  case compares the partial flag and a lower-bound predicate, since the number
  of rows visited in 200 ms varies by machine. The forced 10 s branch is tested
  deterministically in unit tests; the live fixture checks its normal zero state.
  Final reference/local comparison: **376 identical lines**, covering 280
  cursor/match states plus the partial-count fallback.

## Live comparisons

Run the committed fixture with the helpers from the shared brief:

```sh
tools/ref-tmux.sh < WORKTREE/tests/copy_compat.sh > reference.out
tools/loc-tmux.sh WORKTREE < WORKTREE/tests/copy_compat.sh > local.out
diff -u reference.out local.out
```

The final fixture has **identical output**: both complete default key tables and
432 selection-transition cases across vi and emacs, checking cursor, scroll,
selection endpoints/mode/activity/presence and exact copied bytes.

Additional identical live probes covered all five line-number option modes and
goto-line positions, refresh toggles, selection-safe scroll exit, live session
word-separators/copy_cursor_word, -u/-d/-e/-H/-q and kill-on-exit. A separate
synchronized-output probe matched automatic refresh and suspension during a
selection. Rendering styles are covered by the engine unit tests and C-source
comparison; this CLI fixture does not capture an attached terminal's rendered UI.

## Remaining differences

- **Scrollbar mouse integration is incomplete.** scroll-to-mouse and -S use the
  tmux slider arithmetic, and the root bindings exist, but this repository has no
  scrollbar drawing or mouse location classification. The server currently uses
  slider grab offset zero. Real slider dragging therefore needs shared rendering,
  mouse hit testing and per-client slider grab state before it works end to end.
  The emacs search-mark bookkeeping on this special server path also remains.
- next-prompt/previous-prompt have bindings and metadata but no movement: snapshots
  do not carry shell prompt markers. They need terminal-parser/snapshot metadata.
- Snapshots lack hyperlink, original tab and history timestamp metadata, so
  copy_cursor_hyperlink, original-tab copying and top_line_time lack full parity.

These metadata and scrollbar limitations remain outside the round 2 search scope.
The package should not be described as full window-copy.c parity. A no-current-client scroll-to-mouse probe crashed the live reference;
that invalid-context probe is excluded from the committed fixture.

## Validation

All Cargo commands used `CARGO_BUILD_JOBS=2`.

- `cargo test --release --no-fail-fast`: 318 passed, one known Linux failure
  (`format_loops_and_utf8_length`), one ignored UI recording test. The documented
  read-only terminal test passed on this run. All 67 copy-mode unit tests passed,
  including the four round 2 tests.
- All three copy-mode command integration tests passed in the full suite.
- `cargo check --target x86_64-pc-windows-gnu --tests`: passed.

Round 2 touches only `src/copymode.rs`, `src/server/copy.rs`,
`tests/copy_search_compat.sh`, and this audit. No files outside copy scope changed.

Earlier continuation changes: `src/copymode.rs`, `src/server/copy.rs`,
`tests/commands.rs`, and this audit/fixture. Shared files touched across the
whole package additionally include `src/keys.rs`, `src/server/cmds.rs`,
`src/server/input.rs`, `src/server/mod.rs`, `src/server/mouse.rs`,
`src/server/render.rs`, and `src/server/vars.rs`.
