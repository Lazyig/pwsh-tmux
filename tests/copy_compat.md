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
- Search does not implement tmux's time-budget/partial-count machinery or matches
  spanning physical rows. search_timed_out remains zero.

These limitations predate the continuation and are not fixed by its selection
and live-option changes. The package should not be described as full window-copy.c
parity. A no-current-client scroll-to-mouse probe crashed the live reference;
that invalid-context probe is excluded from the committed fixture.

## Validation

All Cargo commands used `CARGO_BUILD_JOBS=2`.

- `cargo test --release --no-fail-fast`: 314 passed, one known Linux failure
  (`format_loops_and_utf8_length`), one ignored UI recording test. The documented
  read-only terminal test passed on this run. All 63 copy-mode unit tests passed.
- `cargo test --release --test commands copy_mode -- --test-threads=2`:
  all three copy-mode command integration tests passed.
- `cargo check --target x86_64-pc-windows-gnu --tests`: passed.

Continuation changes: `src/copymode.rs`, `src/server/copy.rs`,
`tests/commands.rs`, and this audit/fixture. Shared files touched across the
whole package additionally include `src/keys.rs`, `src/server/cmds.rs`,
`src/server/input.rs`, `src/server/mod.rs`, `src/server/mouse.rs`,
`src/server/render.rs`, and `src/server/vars.rs`.
