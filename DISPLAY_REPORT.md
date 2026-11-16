# Phase 3 display work package

Implemented:

- Messages: `display-time 0` and `display -d 0` wait for a key; any key clears a message and is then handled; `display -N` with a timer ignores keys.
- Command prompt editing follows tmux 3.8 prompt.c: vi translation (prompt_translate_key), word motions with `word-separators` (w/e/b/W/E/B, M-f/M-b, C-Left/C-Right, C-w), C-v quoting, history Down to an empty line, C-u/C-k not filling the C-y text.
- `display-menu` returns without waiting for the menu (3.8); `-b`/`-s`/`-S`/`-H` for menus and `-b`/`-s`/`-S` for popups; menu item names are drawn as formats.
- Format `buffer_*` variables describe the top automatic buffer outside buffer lists (default pane menu Paste item); `list=left-marker`/`right-marker` text is not drawn.
- Messages and prompts use `message-format`, `message-line`, `message-style` fill/width/alignment and vi command-mode `message-command-style`. Unfilled formats preserve the underlying status cells. Prompt input scrolls horizontally; `status-keys` selects emacs or vi editing, including Escape's cursor-left transition. Session status style options remain available through the existing format-option lookup.
- `fill-character` expands with `is_inside` for both window and extra client areas, including the default ACS outside character.
- Menu/popup border line choices; pane border colour/arrows/both indicators.
- Scrollbars: off/on/modal/auto-hide, left/right, pane style colours/width/padding, reserved width only for on, hover and scrolling visibility, millisecond timeout, alternate-screen suppression, page-up/down and slider mouse bindings.
- Layout maximum tiled columns and other-pane width/height, including percentage main/other dimensions and mirrored layouts.
- `wrap-search` lookup on copy-mode entry, search commands, keyboard actions and incremental search. Small CopyMode additions provide the wrap guard and scrollbar history/position helpers.
- Dead-pane format text is formatted and written at the bottom with the cursor hidden; an empty format writes no text.
- Clock seconds styles and visible glyphs on terminals that ignore colour escapes; the clock clears its pane, uses strftime's ` 1:02 PM` 12-hour text and centres the small fallback.
- 109 prefix/root defaults extracted from tmux 3.8 C, with all three default menus, session Detach, pane Paste and prefix T. The regression test checks fixture regeneration against the local C source when available, then compares complete list-keys command and note dumps against source-file loading of that fixture.

Delegated, as requested:

- Copy-mode and copy-mode-vi key tables remain with the copy package.
- Prefix `*`, `@`, `g` and the move table; floating-aware `M-Up`, `M-Left`, `C-Up`, `C-Left`; root `C-MouseDrag1Pane`, `C-MouseDrag1Empty`, `M-MouseDrag1Pane`, `M-MouseDrag1Border`, `MouseDown1Control7`: floating package.
- Prefix Tab and BTab: modes package.
- Floating actions in the source-derived menus are retained; their implementations belong to the floating package. Integrators should add delegated bindings after the display-defaults loader (or extend its fixture/extractor exclusions).

Shared files touched:

- `src/cmdparse.rs`: recognize compact `{command}` blocks required by the reference default menus.
- `src/format.rs`: list marker text dropped in spans (format package file, minimal change).
- `src/server/vars.rs`: top-buffer fallback for `buffer_*` (format package file, minimal change).
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
- Linux: `cargo test --release --no-fail-fast`: 249 unit tests passed; commands 36 passed with only known `format_loops_and_utf8_length` failing; interactive 19 passed with only known `readonly_terminal_blocks_keys_paste_mouse_and_shared_resize` failing; attach 3, SSH 1 passed; UI recording 1 ignored.
- Compared live against tmux 3.8 in the reference pod (nested outer/inner client scenarios in the scratchpad `vd/` directory): layouts, borders/indicators, fill-character, scrollbars, messages, prompt editing, menus/popups, clock, dead panes, wrap-search and list-keys. Remaining differences belong to other packages (see below) or are terminal capture noise.

Known differences left to other packages:

- Copy mode: `page-up` scrolls by height-2 in tmux (scrollbar clicks too), search leaves the cursor at the match end, `copy-mode-position-format` shows the line time, `send -X` outside a mode is not an error here.
- Input/terminal: `clear` does not push the screen into history (`scroll-on-clear`).
- Modes: choose-tree drawing; help list lacks the delegated Tab/BTab/*/@/g bindings.
- 3.8 "menus belong to the window" (shown on all clients) is not implemented; menus stay per client.

## Second verification round

Fixed after the second live comparison (scenarios in the scratchpad `vb2/`, `v2dc/` and `vd3/` directories):

- Prompts follow tmux 3.8 prompt.c: `-N` runs on a non-digit key and passes the key on; `-1` and `confirm-before` take control keys (`C-x` as 0x18), BSpace (0x7f) and Escape as one character before vi translation, and with `-I` text close without running; `-i` runs the template on every change with `=`, `+` (C-s) or `-` (C-r) in front, starting empty with the `-I` text kept for C-r/C-s, and cursor keys end it; empty input runs the template too.
- Tab completion inserts the longest common prefix of command (and `command-alias`) names and otherwise shows the matches underlined after the input; a click picks one, and a click in the prompt moves the cursor.
- Templates use cmd_template_replace: `%%` escapes single quotes, `%%%` and `%N%` double-quote specials, each answer takes the next `%%`.
- Prompt strings get the space command-prompt and confirm-before add (none for `-l` or the default `:`) and are formats expanded when drawn.
- `command-prompt -P` (and the copy-mode prompts, which tmux 3.8 binds with `-P`) draw on the pane's last line.
- `prompt-cursor-style`/`-colour` and the `prompt-command-*` forms set DECSCUSR and OSC 12 while a prompt has the cursor.
- Command errors start with a capital letter; only display-message interprets `#[...]` in messages; mouse events clear a message like keys.
- Menus are positioned in window coordinates (below a top status line, above a bottom one); popup `-x`/`-y` expand letters and formats with y the bottom row, and popups are drawn over the status line. Escape/C-c close a popup without `-E`, after the command exits only they (or `-k` any key).
- `pane-border-lines spaces`/`none` are blank, `number` uses the pane the cell is drawn for. Pane status lines start after a left scrollbar and honour format alignment; arrows sit one in from the pane content corners, including on the status line.
- Scrollbars: hidden auto-hide bars and pane status lines take no clicks, slider drags keep the grabbed row, clock/tree modes keep the bar, history-top/bottom show it only when scrolling, all-motion mouse mode stays on with an overlay.
- `pane_dead_signal`, and `pane_left` of panes hidden by zoom beside a left scrollbar.

Not changed:

- `message-style` with `width=N%` and `align=`: tmux 3.8 built with musl leaves the text at the area's left only because strftime fails on the `%` of `#{E:message-style}`; with `width=40` it centres like here.
- Default menus in list-keys and the delegated default bindings wait for the floating and modes packages (`new-pane`, `move-pane -P`, `break-pane -W`).
- `Can't find session` instead of `Can't find pane` for a bad `-t` is target resolution, not display.
