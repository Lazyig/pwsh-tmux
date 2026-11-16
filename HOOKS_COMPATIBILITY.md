# Hooks work package: tmux 3.8 audit

The implementation uses `events::Payload` for hooks, event waiters and control
notifications. Built-in hook names and scopes match the 89 entries in the 3.8
options table. The reference no longer has notify.c: events.c, events-payload.c,
hooks.c and control-notify.c provide its behavior.

Implemented and covered by command/module tests:

- `set-hook -E`, `-R`, `-B`, `-T`, `show-hooks -B`, hook arrays and user hooks;
  one-second monitor checks, change/true filters, target payloads and fire statistics.
- `wait-for -E`, `-F`, `-v`, `-l`, `-w`, separate signal waiters and lock waiters,
  remembered signals and disconnect cleanup. Empty `-F` remains a false filter.
- Client lifecycle/focus/resize/session events, pane activity/bell/title/mode/
  move/resize/create/exit events, session lifecycle/group/current-window events,
  window lifetime/link/layout/zoom/resize/name/active-pane events, alerts and marks.
- OSC 133 prompt/start/finish payloads reuse `OscState`. Multiple titles, OSC 133,
  bells and clipboard events in one read retain parsing order and individual data.
  Repeated OSC titles, title-stack restores and `split-window -T` fire title events.
- Hook target/format restoration and error status survive suspended command queues.
  Each hook array member is a separate error group.
- Control replies use one block per command. Event notifications and pause/continue
  notifications are deferred until the block ends. Later input lines wait behind
  suspended control commands. Command replies, notifications, input lines and
  transport output have memory bounds. Exiting clients receive no new event notices.
  Pane output is delivered eagerly and generation checked; there are no per-pane
  control buffer offsets to reset on respawn.

Review fixes (second round), each with a regression test:

- Hook items inserted into a sourced file keep none of its soft (per-line) error
  allowance; after hooks need a valid target (none while no session exists);
  config line errors from a command client go to stderr with exit status 1.
- Commands that change layouts in steps fire tmux's sequence instead of one
  snapshot diff: unzoom events first (window_push_zoom/server_unzoom_window),
  closed cells before the new active pane, break-pane/join-pane/move-window and
  the transient first window of `new-session -t`; select-pane -Z re-zooms.
- Focus is updated only for the panes and windows tmux updates; a stale focused
  pane elsewhere is not reported.
- A control client's waiting command (wait-for, run-shell, if-shell) ends its
  block at once; later output and -v payloads are plain lines after it.
- Change detection is skipped on loop turns with only pane output (at least every
  50ms), keeping server CPU at the baseline's level with many panes.

## Cross-package integration still required

The modes and theme worktrees are being implemented independently. This worktree
has no pane-owned prompt storage or terminal theme report handling; duplicating
those implementations here would conflict with their owners.

- After modes is merged, call `fire_pane_prompt(name, pane, prompt_type)` from its
  `set_pane_prompt` owner: close the previous prompt before replacing it and fire
  the open event after creating the new one. Fire close on accept/cancel, but not
  while destroying the pane. Names are `pane-prompt-opened` and
  `pane-prompt-closed`; the payload includes pane, window and prompt_type. Do not
  fire these for client status prompts. The helper is implemented in events.rs.
- The theme package calls the retained `run_hook` shim for `client-light-theme`
  and `client-dark-theme`; the shim turns these into client payload events.
- Floating panes are not represented on this branch. General pane/window change
  detection uses `Window::panes()`; the float package must expose floating panes
  there and supply explicit movement events for changes within the same window.
- Pane mode stacks are owned by modes. Snapshot detection covers transitions
  observable after a command/key; intermediate stack transitions inside one
  operation need event calls at the mode owner after integration.

## Live reference verification

Reference comparisons used the user's reference pod, through ref-tmux.sh, never a
locally downloaded tmux. The same scripts ran through loc-tmux.sh. Cases covered
hook error groups and suspended context restoration; empty filters and event-name
validation; channel locks and explicit wake; destroyed-window names; monitor
payloads and retained groups; control command blocks/messages/pause/continue/wait
ordering; interleaved OSC/title/bell events; split titles and title-stack restores.

The upstream 3.8 regress scripts hooks.sh, hooks-after.sh, hooks-lifecycle.sh,
hooks-notify.sh, set-hook-B.sh, set-hook-E.sh, set-hook-R.sh and
control-notify-events.sh were also executed against the reference and candidate.
Final results are recorded in the completion report.

## Final build checks

`CARGO_BUILD_JOBS=2 cargo test --release --no-fail-fast`: 304 passed, with only the
brief's two known Linux failures (`format_loops_and_utf8_length` and
`readonly_terminal_blocks_keys_paste_mouse_and_shared_resize`); one UI recording
test is ignored. All 46 other command tests, all 243 module tests, all three
attach tests, eleven other interactive tests and the SSH test passed.

`CARGO_BUILD_JOBS=2 cargo check --target x86_64-pc-windows-gnu --tests`: passed.

The final candidate matches the live reference scenarios above. The original
hooks-notify.sh reaches its last nested-terminal resize scenario before failing
to attach the inner client (the two implementations use different nesting
variables). wait-for-client-lost.sh expects tmux's C allocation debug logs,
which pwsh-tmux does not emit. Portable runs retain the behavioral assertions
and omit these harness-only assumptions; these are reported separately from
unmodified upstream runs.

Shared files touched across this branch, besides the new server/events.rs:
server/mod.rs, server/cmds.rs, server/control.rs, server/input.rs,
server/security_tests.rs, server/vars.rs, cmdparse.rs and tests/commands.rs.
The parser change is required so session IDs survive monitor hook expansion;
the variables change preserves group names after the founding session exits.

Final upstream comparison: all eleven original scripts passed on the reference.
Nine passed unmodified on the candidate; hooks-notify.sh and
wait-for-client-lost.sh had only the harness differences described above.
Both hooks-notify-portable.sh and hooks-wait-lost-portable.sh passed their
remaining behavioral assertions on the final candidate. The edge-case comparison
also produced identical stdout once the diagnostic's `%999999` token was separated
from the following semicolon, as required by tmux's lexer.
