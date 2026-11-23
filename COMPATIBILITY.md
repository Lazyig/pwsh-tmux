# tmux compatibility

## Baseline: tmux 3.8

The compatibility baseline is **tmux 3.8**, the latest release: configs, themes and scripts
written for 3.8 should behave the same. The baseline was previously tmux 3.6a; that
investigation is kept below under [Earlier baseline: tmux 3.6a](#earlier-baseline-tmux-36a).
The [3.8 C source](https://github.com/tmux/tmux/tree/3.8) is the authority for defaults
and edge cases.

How it was checked:

- **Reference build.** tmux 3.8, built from the release for Linux (musl), runs in a
  separate Linux container. The candidate is a local Linux build of this branch.
- **Scripted ref/loc diffs.** The same POSIX `sh` script runs against the reference and
  against the candidate, each on private `-L` sockets that it kills at the end, and the
  two outputs are diffed. Cases that need an attached client attach it inside an outer
  tmux and read what it was sent.
- **Surface byte comparison.** `list-commands` (also with `-F`), `list-keys` (every table,
  `-N`, `-F`, `-O`, `-r`), `show-options -s`/`-g`/`-wg`/`-A`/`-wA`/`-pA`/`-gH` and
  `show-hooks -g`/`-B`/`-F` are byte-identical to 3.8 except for three platform defaults:
  `default-terminal`, `default-shell` and `lock-command` (see
  [Intentional differences](#intentional-differences)). No 3.8 command, flag or option is
  missing, and every option in the 183-option table except `xterm-keys` is read by code
  (`xterm-keys` is accepted and ignored, as in 3.8).
  `set-option` gives the same error messages and exit status in 190 validation cases, and
  the 214 format variables were compared with a detached session, a nested attached
  client, and dead, piped, zoomed, marked, linked and alternate-screen panes.

The last full Linux run of `cargo test --release` passed 667 tests (unit 439, attach 3,
commands 130, grid_metadata 10, input_options 2, interactive 53, modes 29, ssh 1; the
ui_compat test is ignored). The fixes from the last audit round were checked for Windows
with `cargo check --target x86_64-pc-windows-gnu --tests` only, not run there.

## What tmux 3.7 and 3.8 added, now supported

From the [3.8 CHANGES](https://github.com/tmux/tmux/blob/3.8/CHANGES) (3.6b to 3.8):

- **Floating panes:** `new-pane` (bound to `*`), with `-T` titles and `-B` border lines,
  `-W` to wait for the command, `-A` to stay above a zoomed pane and modal panes (`-O`,
  with `-C`, `-K`, `-D`). `move-pane` and `resize-pane` move and resize them, also with
  the mouse. `break-pane` floats a tiled pane and `join-pane` tiles a floating one.
  `pane-border-status` has `top-floating` and `bottom-floating`, and `C-b g` enters the
  `move` key table.
- **JSON layouts:** `#{window_layout}` is the 3.8 JSON form (`{"V":2,...}`), which includes
  floating panes. The old layout strings are still accepted, and control clients get
  them unless they set the `new-layouts` flag.
- **Modes:** `switch-mode` (bound to `Tab` for windows and `BTab` for sessions),
  `display-panes` as a mode, `command-prompt -P` prompts inside a pane, `choose-tree -h`
  and `-k`, customize mode showing hooks and the environment with `e` (editor) and `C`
  (changed items only), and menus that belong to the window.
- **Themes:** the `theme` option, light and dark detection, `theme*` colour names in
  styles, `tree-mode-selection-style`.
- **Hooks and events:** hooks run as events with payloads; monitors (`set-hook -B`, `-T`,
  `show-hooks -B`); `set-hook -E`; `wait-for -E`, `-F`, `-l`, `-v`, `-w`; the new hooks,
  including `pane-shell-prompt`, `pane-command-started` and `pane-command-finished` from
  OSC 133.
- **Copy mode:** `refresh-on`, `refresh-off`, `refresh-toggle` and `refresh-now` (bound to
  `r`), `copy-mode-current-line-style`, line numbers (`copy-mode-line-numbers`),
  `recentre-top-bottom` and `scroll-exit-on`/`-off`/`-toggle`.
- **Line metadata** for `capture-pane -F`/`-H`/`-I`, copy mode and the mouse formats: the
  terminal emulator keeps OSC 133 prompt marks, OSC 8 hyperlinks, tab cells and history
  line times.
- **Display and options:** `pane-scrollbars auto-hide` and `pane-scrollbars-timeout`,
  `clear-on-attach`, the two-part `fill-character`, `message-format`,
  `remain-on-exit key` and `failed-key`, `mouse` on by default, and terminal features
  disabled with `@` plus the `utf8` and `appesc` features.
- **Commands and formats:** `new-window`/`respawn-pane`/`respawn-window -E`,
  `capture-pane -F`/`-H`/`-I`/`-L`/`-R`, `show-options -F` and `show-hooks -F`, `-O`
  sort orders and `-r` on the list commands, `-f` filters for `kill-pane`/`kill-window`/
  `kill-session -a`, `kill-session -g`, `server-access -g`, `paste-buffer -S`,
  `run-shell` arguments as `#{1}`, `#{2}`, ..., the `O:`, `V:`, `I/`, `t/d`, `c/f`, `c/b`
  and `A/` modifiers, and the new format variables.

## Intentional differences

- **Shell and platform defaults.** `default-shell` is PowerShell, the project's purpose:
  `pwsh`, falling back to `powershell.exe` on Windows and to `$SHELL` (then `/bin/sh`)
  on Unix. On Unix a pane without a command runs its shell as a login shell (argv0 `-` and
  the shell's name), as tmux does, except PowerShell, which is started with `-NoLogo`
  instead (Windows panes start the shell as a plain process). `editor` is `notepad.exe`
  on Windows (`/usr/bin/vi` elsewhere). `lock-command` is empty by default on every
  platform (tmux uses a default chosen when it is built: `vlock` if vlock was found,
  otherwise `lock -np`), since Windows has no lock program, so `lock-*` needs one to be set.
  Windows has no job control, so `suspend-client` does nothing there. On Unix the client
  turns on application keypad mode as tmux does (tty_start_tty's smkx, so keypad Enter
  arrives as `\eOM`); on Windows it does not, so keypad Enter arrives as CR and a pane's
  PowerShell gets Enter, not the LF (Ctrl+Enter there) that tmux sends a pane for keypad
  Enter outside keypad mode.
- **How a shell is given a command on Windows.** Where tmux runs `default-shell -c
  command` (a pane's or popup's command, `default-command`, `detach-client -E`, jobs and
  `-c`), a POSIX shell gets `-c command` and PowerShell `-NoLogo -Command command`, as
  words of the command line quoted for CommandLineToArgvW (which both read). cmd.exe
  reads its own command line and has no `\"` escape, so it gets `/s /c "command"` there
  as it is: `/s` makes it take off just those two quotes, and the command runs exactly as
  written (`split-window 'echo "%%%" & pause'` shows `"%%%"`, where the quoting for
  CommandLineToArgvW gave cmd.exe `\"%%%\"`, and a command that starts with a quote
  keeps it). A pane given several words runs them as a program with its
  arguments, quoted for CommandLineToArgvW, as before.
- **Job output on Windows is what the job printed.** The server has no console, and a
  console program it starts would get one in the system's console code page (an OEM one
  such as 437 or 949, or 65001 when Windows is set to use UTF-8), in which PowerShell and
  cmd.exe write to a pipe and read one, losing what it has no character for (`é` is `e`
  in 949, Hangul is `?` in 437), where a pane's console shows the same text whole; and
  PowerShell reads its standard input in the code page it starts with, so its command
  cannot change how that is read. So the server makes one console for its jobs
  (`run-shell`, `if-shell`, `#()`, `pipe-pane`, `copy-pipe`), without a window, and sets
  its input and output code pages to UTF-8 (65001), again before each job starts. Each
  job's shell is started in it by the server, as tmux starts a job: it is the job's
  process (its pid is the one in `show-messages -J` and `#{pane_pipe_pid}`), its command
  line is the job's as it is (an error points into the command as typed; for Windows
  PowerShell 5.1 the command follows a line of its own that sets `[Console]::InputEncoding`
  to UTF-8 without a byte order mark, because .NET Framework's 65001 encoding has one and
  writes it before what PowerShell pipes to a program (without the line `'x' | findstr x`
  finds nothing); an error in the command then says `At line:2`), and its
  standard input, output and error are the job's (input NUL unless the job is given
  text, errors NUL unless `run-shell -E`). PowerShell (5.1 and 7) therefore starts with
  `[Console]::InputEncoding` and `[Console]::OutputEncoding` UTF-8, so what it prints and
  what it reads from `copy-pipe` and `pipe-pane` are UTF-8, and cmd.exe and the programs
  a job runs get 65001 as their console code page (`chcp` says 65001). What PowerShell
  gives a program it pipes to is in `$OutputEncoding`, as in a pane: UTF-8 in PowerShell
  7, ASCII in Windows PowerShell 5.1 unless the profile sets it (`$OutputEncoding =
  [Text.UTF8Encoding]::new($false)`); so in 5.1 `run-shell "echo 'x한글é' | findstr x"`
  prints `x???`, and `findstr 한` finds nothing in it where the system's code page has
  Hangul (949: `'...' returned 1`, as in a pane). A failed command's error goes to the
  shell's standard error, which `run-shell -E` shows; without `-E` it may not show, as
  tmux discards a job's standard error (`'Get-Foo' returned 1` alone). What a job starts goes on by itself and does not
  keep the job from ending (`run-shell -b "Start-Process notepad"`). The jobs share that
  console: a job that changes its code page (`chcp 949`) changes it for the jobs running
  with it until the next one starts, and a Ctrl+C or Ctrl+Break a job sends to the whole
  console reaches the others (the server goes on, as it does at a user's logoff). A server started with a console of its
  own (by hand, in a terminal) leaves that console as it is and gives each job a console
  of its own without a window, in the system's code page. Each line of the output that
  is not UTF-8 is read in the system's console code page (`GetOEMCP`), as a pane's
  console with that code page would read a program that writes in it whatever its
  console's code page is (a program that writes in the ANSI code page is read wrongly
  where that differs from the OEM one). `pipe-pane -I` gives the pane the job's bytes as
  they are. PowerShell's `‘ ’ ‚ ‛` are quotes, as in a pane: `run-shell "echo '한글 ‘q’
  é'"` prints `한글 ` and `q é` on two lines (`‘` ends the first quoted text and `’`
  starts another).
- **`default-terminal` is `xterm-256color`**, not `tmux-256color`: Windows hosts usually
  have no tmux terminfo and ConPTY programs expect xterm. Panes therefore get
  `TERM=xterm-256color`, and Home and End are sent as xterm's `\e[H`/`\e[F` (`\eOH`/`\eOF`
  in application cursor mode) rather than tmux's `\e[1~`/`\e[4~`.
- **`send-keys -K` special keys reach the pane.** tmux 3.8 drops function and cursor keys
  (`F12`, `Up`, `Home`, `C-Up` and the like) that `send-keys -K` gives a pane, because
  `input_key` does not strip the sent-key flag; here they are sent as typed keys are.
- **Pane titles on Windows.** ConPTY sets the console title when the pane starts, so
  `pane_title` begins as the shell's executable path and changes with the console title.
  On Windows clean_name keeps backslashes (they separate the parts of a path) in every
  title, name and path: those set by the program in the pane (OSC 0/2, APC, `ESC k`), OSC 7
  paths, those given by commands (`select-pane -T`, the title of a floating pane from
  `new-pane -T`, `rename-window`, `new-window -n`, `break-pane -n`, `new-session -s`/`-n`,
  `rename-session`, and buffer names from `set-buffer -b`/`-n` and `load-buffer -b`), and
  window names that are not given (a new window's, from its command, and
  `automatic-rename`'s, `pane_current_command`), so a name typed as `C:\Users\x` is kept
  as typed and found by that name, and renaming from a prompt that shows the name
  (`prefix ,`) does not double them each time. On Unix they are escaped as in 3.8
  (`C:\\Users\\x`). There, as in 3.8, a new window is named by parse_window_name of its
  command line as `pane_start_command` shows it (each argument escaped), and
  `pane_current_command` is parse_window_name of the first word of the foreground
  program's command line (`/proc/PID/cmdline`, so `/p/-l` is `-l` and a long name is not
  cut to 15 bytes); `automatic-rename` compares the expanded
  `automatic-rename-format` with the window's name and, when they differ, sets it
  through clean_name again (window_set_name), so a program named `a\b` gives a window
  named `a\\\\b`, and fires `window-renamed` even when that is the name it had. On both,
  window_set_name makes a `#(` in an automatic name `_(`.
  Popup titles (`display-popup -T`) and floating-pane titles (`new-pane -T`, as
  `select-pane -T`) go through clean_name only where 3.8 has it: a popup title never, so it
  keeps backslashes on both; a floating pane's title as above. The other escapes (tab,
  newline and control characters as `\t`, `\n` and octal) are the same on both, and the
  names given by commands are refused for them anyway (check_name).
- **Names that coexist with tmux.** The server listens on `/tmp/pwsh-tmux-UID/NAME`
  (under `PWSH_TMUX_TMPDIR` or `TMUX_TMPDIR` if set) on Unix and on a named pipe on
  Windows. Panes get `PWSH_TMUX` and `PWSH_TMUX_PANE` in place of `TMUX` and `TMUX_PANE`,
  and `TERM_PROGRAM=pwsh-tmux` with the project version as `TERM_PROGRAM_VERSION`
  (`#{version}` reports 3.8). `TMUX` and `TMUX_PANE` are neither set nor cleared, so a
  pane inherits whatever values the server was started with (for example an outer tmux's);
  tmux always sets `TMUX` itself. Configuration, executable and environment variable names
  use `pwsh-tmux`; the deployed executable may separately be named `tmux.exe`. Scripts
  that test `$TMUX` or `TERM_PROGRAM` see these names.
- **Client names** are `client-N` rather than the client's tty path, on every platform;
  `client_tty` gives the same name.
- **Windows transport.** Windows uses ConPTY and named pipes and identifies users by SID;
  Unix uses ptys, Unix sockets and uids. A Windows pipe's ACL is set when the pipe is
  created, so `server-access` changes on Windows belong in the config file.
- **Escape timing after attach (a tmux 3.8 bug).** tmux 3.8 asks a VT100-like terminal, in
  this order, for DA1 (`CSI c`), DA2 (`CSI > c`), XTVERSION (`CSI > q`), DECRQM 2026 and
  the OSC 10 and 11 colours (tty_send_requests, tty.c:406-425), and while any of those
  replies is missing an Escape or a partial key waits at least 500 ms (tty_keys_next,
  tty-keys.c:1002-1008). Only the 5 s start timer (tty_start_timer_callback, tty.c:311)
  gives up on a reply that never comes, so with a terminal that does not answer one of
  them, an Escape takes half a second and an Escape followed within 500 ms by a key is
  that key with Meta for the first five seconds after every attach (and again for five
  seconds when a resize or theme change asks for the colours again, tty_repeat_requests,
  tty.c:428-450). Windows Terminal never answers XTVERSION (microsoft/terminal#18382,
  closed as not planned), nor do Alacritty, Terminal.app or conhost; Konsole does not
  answer DECRQM. This is treated as a bug: the 500 ms wait protects a reply on its way,
  and a terminal (or ssh, or ConPTY, which passes replies on, or as the older inbox ConPTY
  answers what it knows itself and drops the rest) returns replies in the order it was
  asked, so once a reply to a later request has come, an earlier request without a reply
  will not get one. The maintainer agrees that the replies' order can end the wait
  (tmux/tmux#5886: put DA last "and use it as a marker for having received everything")
  but tmux master still waits for the timer (see also tmux/tmux#4822). Here a complete
  DA2, XTVERSION, DECRPM 2026 or OSC 10/11 reply settles the requests sent before it that
  have no reply, and those are no longer waited for; an OSC 11 reply settles OSC 10 too,
  as OSC 10 is asked first, and colours asked again before the reply to an earlier OSC 11
  came (a theme report asks at once) are waited for until each OSC 11 has its reply. With
  Windows Terminal the wait ends as the OSC 11 reply comes. What is settled is not taken
  as received: a reply to it that still comes (out of order) is read as a reply until the
  start timer, as in tmux, and a partial one waits 500 ms for the rest from its second
  byte (`ESC P` or `ESC [`), also when an Escape typed just before it is held as a
  possible Meta prefix (`ESC ESC [`); a reply split after its first byte cannot be told
  from an Escape and is read as keys, as tmux reads any late reply after the start timer.
  Everything else is as in tmux 3.8: the same requests in the same order, the start timer
  and what happens after it (a late reply is then read as keys), the wait for a terminal
  that answers nothing, or nothing after its last missing reply (PuTTY, the Linux console:
  no OSC 10/11 reply), the wait for a colour asked again, and the waits for clipboard
  requests, a partial paste end and partial theme or colour replies. (Sending DA1 last as
  a marker, as #5886 suggests, would end the wait for those terminals too, but it changes
  the bytes sent, and multiplexers such as GNU screen and Zellij answer DA1 themselves
  before the colours they ask the outer terminal for.) A partial theme or colour reply
  also waits 500 ms after the start timer, where tmux decides by its request flags alone.
- **Single quotes in a `{ }` block value that is `%%` alone (a tmux 3.8 bug).** A
  prompt's answer goes into the already parsed values of a block template without being
  parsed again (args_make_commands and cmd_list_copy, arguments.c:784-790 and 349-372), as
  in tmux 3.8, so `"`, `\`, `$`, `~` and a leading `-` stay as typed. tmux 3.8 changed
  `%%` to escape single quotes for a single-quoted string (cmd_template_replace,
  cmd.c:849-906, CHANGES "%% is escaped for single quotes"), which keeps an answer from
  ending the quotes of a value that is parsed again later (an `if-shell` or `run-shell`
  command, a shell command such as `split-window "man '%%'"`), and pwsh-tmux keeps that
  escape there. But tmux 3.8 also applies it to a value that is the whole answer, such as
  `rename-window -- '%%'` of the default `prefix ,` binding, where nothing can remove it,
  so a window renamed to `it's` is named `it'\''s`. Here a value that is exactly `%%`
  keeps the answer's single quotes as typed; every other value, string templates (which
  are parsed again) and `%%%` / `%1%` escape as in 3.8 (on Windows, and on Unix with a
  PowerShell or cmd.exe `default-shell`, as in the next item).
- **Prompt answers are escaped for the shell on Windows** (and on Unix when a
  `default-shell` is PowerShell or cmd.exe). tmux 3.8's escapes (`'\''` for `%%`, a
  backslash before `" \ $ ; ~` for `%%%` and `%N%`) are made for the command parser and
  POSIX shells, and Unix keeps them exactly, as 3.8 has them (jobs there run under
  `/bin/sh`; in sh's double quotes tmux's `%%%` leaves a `` ` `` that runs a command and
  gives `\;` and `\~` with their backslash). On Windows jobs (`run-shell`, `if-shell`,
  `#()`, `pipe-pane`, `copy-pipe`) run under `PWSH_TMUX_SHELL` if it is set, else `pwsh`,
  else `powershell` (`default-shell` does not change this), and new panes,
  `display-popup` and `detach-client -E` run under `default-shell`. There `'\''` does not
  quote: `{ run-shell "echo '%%'" }` answered `x'; echo pwned; '` would run `echo pwned`.
  So on Windows the escape is chosen when the answer is put in, for the shells that may
  read the value: a command the jobs' shell runs (`run-shell`'s and the words after it,
  `if-shell`'s test without `-F`, `pipe-pane`'s) is for that shell alone; any other value
  (a pane's or popup's command, a format, whose `#()` the jobs' shell runs) is for the
  jobs' shell and every `default-shell` (global and of each session). On Unix the same
  is done for those other values, with `/bin/sh` as the jobs' shell, when a
  `default-shell` (global or of a session) is PowerShell or cmd.exe: `default-shell` is
  `pwsh` when it is installed (see Shell and platform defaults), and there `{
  split-window "Get-Content '%%'" }` would give PowerShell tmux's `'\''`. That is the
  only case where Unix differs from 3.8 here (3.8 gives a PowerShell `default-shell` the
  same `'\''`); jobs keep tmux's escapes, and with POSIX shells (or any other, such as
  fish) everything is as in 3.8. Each shell of the set then reads the answer as typed:
  - POSIX shells (sh, bash, zsh, dash, ksh and the like, such as Git Bash): `'\''` in
    single quotes, and a backslash before `\ " $` and `` ` `` in double quotes.
  - PowerShell (5.1 and 7): its single quotes (`'` and `‘ ’ ‚ ‛`) doubled in single quotes;
    `"` and `“ ” „` doubled and a backtick before `$` and `` ` `` in double quotes. `\` is
    kept as it is, so a Windows path stays one.
  - PowerShell and a POSIX shell (say, Git Bash as `default-shell`, for a pane's command,
    or PowerShell as `default-shell` on Unix): no form of PowerShell's quotes, nor in
    double quotes of `$`, `` ` `` or a `\` before another `\`, a line end or the end of
    the answer, is read as typed by both (a backtick is PowerShell's escape and starts a
    command in sh, and quoting the pieces apart, `"a"'$'"b"`, makes PowerShell take them
    as separate arguments). An answer with one of them is refused, with the error `an
    answer with ' cannot be quoted for both PowerShell and sh` (a character that is not
    ASCII is followed by its code point: `an answer with ‘ (U+2018) cannot be quoted for
    both PowerShell and sh`), and none of the template's commands run. The rest of an answer is put in as typed. (Setting
    `PWSH_TMUX_SHELL` and `default-shell` to the same kind of shell avoids this.)
  - cmd.exe has no single quotes, nothing escapes inside its double quotes, and where its
    double quotes are is not known when the answer is put in, so each of
    `^ & | < > ( ) % !` gets a caret, `"` is doubled and a newline becomes a space:
    cmd.exe acts on nothing in the answer (`cmd /c` has delayed expansion off unless the
    `DelayedExpansion` value under `Software\Microsoft\Command Processor` turns it on,
    which is not guaranteed: `!` is then special inside its double quotes too), and reads
    it as typed outside its double quotes. With a POSIX shell as well, `"` in double
    quotes is refused (`\"` ends cmd.exe's quotes, `""` sh's).
  - What the shells of a set with cmd.exe read is safe but not always as typed: each
    one's escapes are there for the others too. Inside cmd.exe's double quotes the
    carets stay in the text, and so do the other shells' escapes, which cmd.exe does not
    remove: with PowerShell (jobs under `pwsh`, `default-shell` cmd.exe) the backtick
    before `$` and `` ` `` in `"%%%"`, so `\\srv\c$\x` reaches cmd.exe as
    ``\\srv\c`$\x``, and PowerShell's doubled quotes: in `"%%%"` each of `“ ” „` is
    doubled, so an answer `“q”` reaches cmd.exe, and is shown by its `echo`, as `““q””`,
    and in `'%%'` each of `' ‘ ’ ‚ ‛` (`it’s` is `it’’s`); with a POSIX shell the backslash before `\ $` and `` ` ``. PowerShell
    and sh read cmd.exe's escapes as text the same way: the carets, and in `'%%'` the
    doubled `"` (`'a""b'` is `a""b` for them).
  - Not guaranteed: a shell whose quoting is not known (fish, nu, xonsh, `wsl`, ...) gets
    the escapes for PowerShell and a POSIX shell on Windows, but fish reads `\'` and `\\`
    in single quotes and nu has no escape in them, so an answer may still leave a `'%%'`.
    A native program that PowerShell runs gets the answer as PowerShell passes arguments
    (Windows PowerShell 5.1 and pwsh before 7.3 pass an embedded `"` unescaped).

  Which escape applies also depends on who reads the value. A `{ }` block's values are
  read by whatever its command gives them to: the arguments that are commands parsed
  again (`if-shell`'s commands, `run-shell -C`, `confirm-before`, `bind-key` and
  `set-hook` given one command string, templates of `command-prompt`, `choose-*` and
  `display-panes`, `display-menu` items) by the command parser, `run-shell`'s,
  `if-shell`'s test and `pipe-pane`'s by the jobs' shell, the rest as said above. A
  string template, and a value the parser reads, is read by the parser, and the quotes
  of each reader the answer goes through decide: where the answer is outside the
  parser's quotes, they get tmux's escape (the parser reads the answer's syntax); inside
  them, the command the parser gives the answer to says who reads it next, as in a
  block, and where the answer is inside that reader's quotes it gets that reader's
  escape (another parser's, or the shells', or the refusal) encoded for the parser's
  quotes. So `command-prompt "run-shell \"echo '%%'\""`, `command-prompt 'run-shell
  "echo \"%%%\""'`, `run-shell "echo \"%%%\""` (the shell's double quotes written `\"`
  in the parser's) and a command string in a command string, `if-shell true "run-shell
  \"echo '%%'\""`, each give the shell the answer as typed (or refuse it), and an
  answer that is outside the next reader's quotes, `rename-window "%%"`, reaches it as
  typed. (tmux 3.8, and Unix here, escape for the first parser's quotes only: it gives
  sh the answer's quotes unescaped in such cases, since the parser removes the backslash
  of `'\''`.) A `bind-key` given several words binds those words as the command's own
  arguments, so each is escaped for what that command does with it (`{ bind -n F3
  run-shell "echo '%%'" }` gives the shell's escape); as in 3.8, a filled word that ends
  with `;` (not `\;`) ends the bound command there, and who reads the words after it is
  decided where the filled words end the commands (an answer `x;` makes the next word a
  command's; an error if the commands would not stay put). A value that is `%%` alone
  and `%N` are put in as typed, as before. On both platforms, as in 3.8, the parser
  removes spaces and a comment after a newline inside its quotes, and a backslash before
  a newline, so such an answer loses them; and a template that puts `%%` outside any
  quotes, that is written for another shell's quoting, or that a command expands as a
  format (`run-shell`, `if-shell`: an answer's `#()` runs), is not made safe.
- **The panes stay frozen for a whole prompt.** While a status prompt is open (not
  `command-prompt -C`/`-i`) or a message is shown (not `display-message -C`), pane output
  is not drawn, as in tmux 3.8 (TTY_FREEZE, status.c:386 and 605). tmux 3.8 lifts the
  freeze in two places where it means to keep it or never set it: closing a popup over an
  open prompt unfreezes the panes for the rest of that prompt (server_client_clear_overlay,
  server-client.c:135, clears TTY_FREEZE without looking at the prompt), and a message
  shown over a `-C` prompt leaves the panes frozen after it is cleared until the prompt
  closes (status_message_clear, status.c:402, keeps the freeze whenever a prompt is open).
  Here the panes are frozen exactly while a freezing prompt or message is shown.
- **Configuration files with CRLF line endings** (as Windows editors and `core.autocrlf`
  checkouts write them) are read with each `\r\n` as a newline. tmux 3.8 keeps the `\r`,
  so a directive line such as `%endif\r` or `%else\r` is a syntax error there
  (cmd-parse.y's lexer ends a line at `\n` only).
- **No signal information on Windows:** `pane_dead_signal` and other signal values are
  never set there. A popup whose command was ended (TerminateProcess) gives its waiting
  command-line client that process's exit code, where on Unix it is the signal number
  (WTERMSIG), as in tmux.
- **History line times** record the real time a line entered the history; tmux uses
  a time cached at the start of its event loop, which can be older.
- **Terminal descriptions.** pwsh-tmux draws without terminfo, so a client terminal the
  terminfo database does not know is accepted on every platform, where tmux 3.8 refuses it
  with "missing or unsuitable terminal". A terminal terminfo does know is refused without
  `clear` or `cup`; Windows console clients have no terminfo, so this happens on Unix only.
- **Tests.** The `grid_metadata` tests run on Unix only, because their panes run `printf`.
- **Server shutdown on Windows.** On Unix the server unlinks its socket before the last
  reply, so a new client starts a fresh server. On Windows a client connecting to a named
  pipe during server shutdown can still race. To narrow this, the server sends the last
  command client a shutdown message (`S_SHUTDOWN`) and that client waits for the pipe to
  close.
- **Security restrictions** (read-only clients, resource bounds) are described in
  [`SECURITY.md`](SECURITY.md).

## Known differences

These do not match tmux 3.8 yet:

- **`capture-pane -R` cell sizes** only partly match. tmux's allocated size depends on how
  text was written (collected runs, ICH/DCH/EL expansion) and on CLEARED cell flags; the
  terminal emulator does not track per-row allocation.
- **Long UTF-8 sequences:** a cell holds 23 bytes, where tmux's `UTF8_SIZE` is 32, so tag
  sequences longer than 23 bytes (such as the England flag) are cut. Raising it would grow
  every cell (see [`MEMORY_COMPARISON.md`](MEMORY_COMPARISON.md)).
- **Hex key names `0x21` to `0x7e`** are the same key as the typed ASCII character here;
  in tmux they are separate keys that list under the same name. Binding one therefore
  replaces the binding of the other (after `bind 0x31 ...` and `bind 1 ...` only the
  second is left, where tmux keeps both).
- **Sixel** is not implemented: `#{sixel_support}` is 0 and the DA1 reply has no `;4`.
- **`history_bytes` and `history_all_bytes`** on the alternate screen with a scroll region
  count one row of cells fewer than tmux.
- **Silence alerts with equal deadlines** fire in window id order; tmux's order comes from
  its timer heap.
- **Error output:** merged stdout and stderr can interleave in a different order, and
  `load-buffer` of a directory says "Is a directory" where tmux says "I/O error". For a
  socket that never existed the client says "no server running on SOCK", where tmux says
  "error connecting to SOCK (No such file or directory)".
- **`select-layout -E`** with a parent that also holds nested cells: tmux 3.8 shares the
  whole size among the panes, which overflows the window; here the panes share what the
  nested cells leave.
- **Session groups:** `new-session -t` with an unknown name fails with "can't find
  session"; tmux 3.8 creates a new group of that name.
- **Reflow:** widening a pane with tabs from 3 columns to 4 gives fewer blank rows than
  tmux (shrinking to 3, 2 or 1 columns, and widening from there to the full width, match),
  and the pane grid counts a wide character as 2 columns where tmux's reflow also counts
  its padding (copy mode's reflow follows tmux).
- **Resizing a zoomed window:** after `resize-window` on a zoomed window, unzooming gives
  pane sizes that differ from tmux's (the zoomed layout keeps the older proportional
  resize). The same resize without zoom matches.
- **Blank cells in the status line and after prompts** are written with a foreground reset
  (an extra `\e[39m` in a nested `capture-pane -e`), where tmux clears them with a
  background-only erase. Nothing visible changes.
- **Copy mode on huge panes:** copy mode stores every line at full width, so entering it
  on a pane of about 5000 by 5000 takes seconds.
- **`backspace C-?`:** the option value `C-?` is read as the BSpace key; tmux's default is
  a raw `\177` that also prints as `C-?`.
- **Pastes, small cases:** the client turns on bracketed paste whenever the terminal has
  the `bpaste` feature, where tmux follows the mode of the active pane or popup; with
  `synchronize-panes`, the other panes of a `-K` pane get pasted text without the
  `\e[200~`/`\e[201~` markers tmux also copies to them.
- **Window names on Linux** come from `/proc/PID/comm` (cut at 15 characters), where tmux
  reads the program from the command line.
- **The pane's program on Windows.** tmux reads `pane_current_command` when it is used
  (a format, a window's name check), as Unix does here. On Windows a process snapshot is
  too slow for that, so what a pane runs is looked up in the background about once a
  second: `pane_current_command` and automatic rename follow a new program up to a second
  late, and a new pane's window name is first checked once that lookup has found its
  program. Window names are otherwise checked as tmux does: when the active pane changes
  (output, a mode, another active pane), at most every 500 ms.

## Earlier baseline: tmux 3.6a

This section records the original investigation against **tmux 3.6a** (the Linux x86_64
static binary from the [official tmux builds release](https://github.com/tmux/tmux-builds/releases/tag/v3.6a),
archive SHA256 `c0a772a5e6ca8f129b0111d10029a52e02bcbc8352d5a8c0d3de8466a1e59c2e`, run
under WSL against the candidate on Windows with ConPTY). Each comparison creates separate
private servers and shuts them down afterward. Where 3.8 changed behaviour, the code now
follows 3.8.

### Differences corrected

| Area | Previous behaviour | Corrected behaviour |
|---|---|---|
| Detached session height | Subtracted a status row from `new-session -d -y` | Treats the requested height as the pane area, including for control clients |
| `select-layout -E` | Recursively evened every split in the window | Spreads the target pane's siblings, climbing to ancestors only if already even; honours pane targets and unzooms |
| `select-layout -o` | Accepted the flag but ignored it | Saves and restores the previous layout; repeated calls toggle the layouts |
| Layout cycle | First `next-layout` skipped even-horizontal | Starts at even-horizontal; previous starts at tiled; includes both mirrored main layouts |
| Layout names | Accepted a few hardcoded abbreviations | Accepts exact names and unique prefixes, rejecting ambiguous prefixes |
| Pane indices | Derived indices from the geometric layout tree | Maintains a pane list independently, preserving indices when layouts move panes |
| Format loops | `S`, `W`, `P` were missing | Expands nested loops in the appropriate session/window/pane context, including active alternatives and sorting/reversal |
| Pane content search | `C` always returned zero | Returns the first matching visible row, one-based, with glob/regex and ignore-case support |
| Format substitution | Minimal regex lacked groups, alternation and backreferences; applied only the first `s` modifier | UTF-8 regex with leftmost-longest matching, captures and backreferences; applies substitutions in sequence |
| UTF-8 length | `n` counted characters | Counts UTF-8 bytes, as tmux does; `w` remains display width |
| Invalid format syntax | Could emit trailing fragments after a malformed format | Stops expansion at syntax errors, matching tmux |
| Arithmetic | Missed the `m` modulus operator | Supports `m` as well as `%` |
| Copy-mode bracket motion | Missing commands and default bindings | Implements next/previous matching brackets, vi `%`, and emacs `C-M-f`/`C-M-b` |
| Copy-mode defaults | Added an unrequested vi `y` binding | Leaves `y` unbound, as in tmux; Enter copies |
| Copy cursor formats | Reported x and y in reverse | Reports column as x and row as y |
| Copy-mode search | Searched only literal strings | Uses regex for ordinary searches and literal patterns for the `-text` commands |
| Incremental search | Waited for Enter before searching | Updates while typing, restores the original position on cancel, and accepts without advancing again |

The original even-layout remainder distribution was already correct for tmux 3.6a.
tmux 3.5a uses a different distribution; it was not adopted as a compatibility fix.
Likewise, chained format conditions and two-argument conditions are valid in 3.6a
and were retained after checking the reference.

The investigation read the 3.6a sources; the code now follows their 3.8 versions:
[layout.c](https://github.com/tmux/tmux/blob/3.8/layout.c),
[cmd-new-session.c](https://github.com/tmux/tmux/blob/3.8/cmd-new-session.c),
[format.c](https://github.com/tmux/tmux/blob/3.8/format.c), and
[window-copy.c](https://github.com/tmux/tmux/blob/3.8/window-copy.c).

### Mouse (`set -g mouse on`)

[`tests/mouse_compat.py`](tests/mouse_compat.py) attaches a client to private servers of
both programs inside a pseudo terminal, writes the same SGR mouse reports and compares
the key events that fired (through logging bindings), the active pane and window, pane
geometry, copy-mode state and selection, paste buffers, menu results and the bytes an
application in the pane received. It runs on Linux or WSL:

```sh
python3 tests/mouse_compat.py --reference /path/to/tmux-3.6a --candidate target/debug/pwsh-tmux
```

The first run against tmux 3.6a matched 2 of 62 scenarios. After the changes below, all
**80 scenarios** match. They cover clicks, second/double/triple clicks and their timing,
drags and drag ends on panes, borders and status lines, modifiers, status-line ranges,
the default root and copy-mode bindings, menus, choose-tree and clock mode, mouse
applications, the alternate screen and `mouse off`.

| Area | Previous behaviour | Corrected behaviour |
|---|---|---|
| Click events | A second press fired `DoubleClick` at once, only at the same cell | `SecondClick` fires at once, `DoubleClick` only after 300 ms without a third press, `TripleClick` on the third (tmux click timer) |
| Drag events | Drags repeated `MouseDrag` and ended with `MouseUp` | The first drag is named after where the button went down, the release fires `MouseDragEnd` where it happened, and drags followed by `resize-pane -M` or a selection send no keys |
| Status line areas | Every status click was `Status` | `StatusLeft`, `StatusRight`, `Status` (window, pane, session or user range) and `StatusDefault`, from the ranges drawn, including `#[range=...]` in `status-format` with tmux's closing rules |
| Key tables | Copy-mode mouse handling was built in | Mouse keys use the pane's mode table, then root; unbound keys go to the pane (or are dropped after a mode table), and the prefix table returns to root |
| Default bindings | A subset of tmux 3.5 bindings | The tmux 3.6a root, copy-mode and copy-mode-vi mouse bindings and menus, including word/line items, middle-click paste, the session menu and `alternate_on` |
| Copy mode | A click moved the cursor and cleared the selection; scrolling kept the cursor on its text; `select-word` used its own rules | Only the bound commands act; `send-keys -X` from the mouse moves to the mouse first and `begin-selection` starts a drag that scrolls at the edges; scrolling keeps the cursor row with tmux's end-of-line rule; `select-word` follows tmux's word reader |
| Menus | Positions and mouse handling differed; empty items were shown | tmux placement (`-x/-y` C, R, P, M, W, S and formats), releasing the button chooses or closes, `-O`, keyboard menus ignore the mouse, empty and doubled lines are dropped, items are expanded when the menu opens |
| Formats | No `mouse_*` positions or words | `mouse_x`, `mouse_y`, `mouse_word`, `mouse_line`, `mouse_pane`, `mouse_status_line`, `mouse_status_range`, `selection_start_*`/`selection_end_*`; `#{||:...}` and `#{&&:...}` take any number of arguments |
| Applications | Drags were reported at the press position; nothing reached the pane with `mouse off` | Events are reported where they happen, the delayed `DoubleClick` is not sent, and with `mouse off` the terminal reports what the active pane asked for |
| `run-shell -d` | Blocked the whole server while waiting | The command queue resumes from a timer |

Fixed on the way: `split-window -b` without a size gave the new pane the smaller half,
`switch-client -t =` ignored the window, a tree opened from the command line chose
through a client that was no longer connected, `display-menu`/`display-panes`/`display-popup`
replaced an overlay already shown (tmux leaves it), and `pane_left`/`pane_top` ignored
`pane-border-status` and zoom.

Since then the default key tables, mouse bindings included, have moved to tmux 3.8's
(`list-keys` is byte-identical), `pane-scrollbars` (with the `Scrollbar*` mouse areas,
slider drags and `copy-mode -S`) is implemented, and the terminal emulator keeps tmux
3.8's line metadata (OSC 133 prompt marks, OSC 8 hyperlinks, tab cells and history line
times) for copy mode, `capture-pane` and the mouse formats.

### Validation and limits

The initial CLI investigation passed all **62 differential cases**, **156 unit tests**, and
**28 local integration tests** (3 attach, 17 commands, 8 interactive).
`cargo build --release --locked --offline` also succeeded. The existing executable
on the user's PATH is not replaced by these source changes or build commands.

Later release/security validation is recorded in [`VALIDATION.md`](VALIDATION.md).
The subsequent real-terminal UI comparison found visual and interaction differences;
see [`UI_COMPARISON.md`](UI_COMPARISON.md). The 62 CLI cases do not establish UI equality.

Run the native regression suite with `cargo test --locked`. The remote SSH test
requires `PT_SSH` and `PT_SSH_BIN`; its default early return is not a remote test pass.
An attached-client regression checks incremental updates before Enter, cancellation,
and accepting the preview without jumping to another match.

Run the independent comparison harness against an installed reference binary:

```powershell
cargo build --locked
python tests/tmux_compat.py --candidate target/debug/pwsh-tmux.exe --reference C:/Windows/System32/wsl.exe -d Ubuntu --exec /path/to/tmux
```

The harness compares observable CLI output for layouts, layout restoration, pane
indices, format loops, regex substitution, Unicode, pane search, copy cursor
coordinates and bracket/search movement. Its JSON output reports the number of
cases and all differences, and it exits nonzero on differences. It does not
require changing the user's tmux installation, configuration or active servers.

This is a bounded investigation, not certification of complete tmux compatibility.
Every command flag, copy-mode command, locale-specific regex rule and newer tmux
feature has not been exhaustively tested. Unsupported details are implementation gaps,
not justified platform differences.

Security restrictions are deliberate differences: read-only control clients cannot
execute commands, remove read-only flags, run shell formats or change shared pane
state, and hooks caused by restricted requests are suppressed. As in tmux, a read-only
client still sizes windows unless it is also ignore-size (`attach -r` and
`switch-client -r` set both; `server-access -r` sets only read-only). Resource bounds
also reject oversized or excessively costly input. See [`SECURITY.md`](SECURITY.md).

The existing repository does not follow default rustfmt formatting, so
`cargo fmt --all -- --check` reports differences throughout unchanged files as well.
The compatibility fixes preserve the existing style; `git diff --check` is used
to check whitespace in the patch.
