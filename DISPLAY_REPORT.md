# Phase 3 display work package

Implemented:

- Messages and prompts use `message-format`, `message-line`, `message-style` fill/width/alignment and vi command-mode `message-command-style`. Unfilled formats preserve the underlying status cells. Prompt input scrolls horizontally; `status-keys` selects emacs or vi editing, including Escape's cursor-left transition. Session status style options remain available through the existing format-option lookup.
- `fill-character` expands with `is_inside` for both window and extra client areas, including the default ACS outside character.
- Menu/popup border line choices; pane border colour/arrows/both indicators.
- Scrollbars: off/on/modal/auto-hide, left/right, pane style colours/width/padding, reserved width only for on, hover and scrolling visibility, millisecond timeout, alternate-screen suppression, page-up/down and slider mouse bindings.
- Layout maximum tiled columns and other-pane width/height, including percentage main/other dimensions and mirrored layouts.
- `wrap-search` lookup on copy-mode entry, search commands, keyboard actions and incremental search. Small CopyMode additions provide the wrap guard and scrollbar history/position helpers.
- Dead-pane format text is formatted and written at the bottom with the cursor hidden; an empty format writes no text.
- Clock seconds styles and visible glyphs on terminals that ignore colour escapes.
- 109 prefix/root defaults extracted from tmux 3.8 C, with all three default menus, session Detach, pane Paste and prefix T. The regression test checks fixture regeneration against the local C source when available, then compares complete list-keys command and note dumps against source-file loading of that fixture.

Delegated, as requested:

- Copy-mode and copy-mode-vi key tables remain with the copy package.
- Prefix `*`, `@`, `g` and the move table; floating-aware `M-Up`, `M-Left`, `C-Up`, `C-Left`; root `C-MouseDrag1Pane`, `C-MouseDrag1Empty`, `M-MouseDrag1Pane`, `M-MouseDrag1Border`, `MouseDown1Control7`: floating package.
- Prefix Tab and BTab: modes package.
- Floating actions in the source-derived menus are retained; their implementations belong to the floating package. Integrators should add delegated bindings after the display-defaults loader (or extend its fixture/extractor exclusions).

Shared files touched:

- `src/cmdparse.rs`: recognize compact `{command}` blocks required by the reference default menus.
- `src/copymode.rs`: search wrap guard and two scrollbar position helpers only; no key table changes.
- `src/layout.rs`: option-aware preset builder and layout tests.
- `src/server/cmds.rs`: layout/scrollbar/popup option wiring and prompt initialization.
- `src/server/input.rs`: default binding initialization, prompt editing, copy-mode option lookup/scrollbar visibility.
- `src/server/mod.rs`: scrollbar state/timers/content geometry, alternate-screen resize and dead-pane text.
- `src/server/modes.rs`: menu drawing argument and clock rendering only.
- `src/server/mouse.rs`: scrollbar classification/hover/drag and movement reporting.
- `src/server/render.rs`: display composition.
- `tests/commands.rs`, `tests/interactive.rs`: regression tests.

New files: `src/server/display.rs`, `src/server/display-defaults.conf`, `tools/extract-display-defaults.py`, this report.

Validation (all cargo commands use `CARGO_BUILD_JOBS=2`):

- Windows: `cargo check --target x86_64-pc-windows-gnu --tests` passed.
- Linux: `cargo test --release --no-fail-fast` completed: 244 unit tests passed; commands 35 passed with only known `format_loops_and_utf8_length` failing; interactive 15 passed with only known `readonly_terminal_blocks_keys_paste_mouse_and_shared_resize` failing; attach 3 passed; SSH 1 passed; UI recording 1 ignored. All new display regressions passed.
- Reference behaviour was derived from the supplied tmux 3.8 C/man sources; the reference pod was unavailable as stated in the assignment.
