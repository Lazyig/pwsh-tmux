#!/bin/sh
# Feed this script to tools/ref-tmux.sh and tools/loc-tmux.sh WORKTREE, then diff stdout.
# It emits default tables and 432 selection transitions, including copied bytes.
set -eu
socket=copy-compat-$$
t() { tmux -L "$socket" "$@"; }
x() { t send-keys -t c -X "$@"; }
p() { t display -p -t c '#{copy_cursor_x},#{copy_cursor_y} #{scroll_position} #{selection_start_x},#{selection_start_y}-#{selection_end_x},#{selection_end_y} #{selection_mode}/#{selection_active}/#{selection_present}'; }
trap 't kill-server 2>/dev/null || :' 0
t -f /dev/null new-session -d -s c -x 24 -y 8 "printf 'one two three\nab\nlonger fourth line\nlast\nfive\nsix\nseven\neight\nnine\nten\n'; sleep 100"
# Poll for process output before cloning the backing screen.
while :; do
    case "$(t capture-pane -p -t c)" in *ten*) break ;; esac
    sleep 0.1
done
for table in copy-mode copy-mode-vi; do t list-keys -T "$table"; done
for mode in vi emacs; do
    t set -w -t c mode-keys "$mode"
    for initial in select-word select-line begin-selection; do
        for motion in cursor-left cursor-right cursor-down end-of-line scroll-down other-end stop-selection rectangle-on 'selection-mode char' 'selection-mode line' 'selection-mode word' next-word; do
            for end in other-end cursor-down stop-selection rectangle-toggle 'selection-mode char' next-word-end; do
                echo "$mode $initial / $motion / $end"
                t copy-mode -q -t c
                t copy-mode -t c
                x history-top
                x cursor-right
                x "$initial"
                # Deliberate splitting: command plus its optional selection-mode argument.
                x $motion
                x $end
                p
                x copy-selection-no-clear
                t show-buffer 2>/dev/null | od -An -tx1
                t delete-buffer 2>/dev/null || :
            done
        done
    done
done
