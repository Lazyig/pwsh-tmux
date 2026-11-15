#!/bin/sh
# Run through ref-tmux.sh and loc-tmux.sh, then diff stdout.
set -eu
socket=copy-search-$$
t() { tmux -L "$socket" "$@"; }
x() { t send-keys -t c -X "$@"; }
p() { t display -p -t c '#{copy_cursor_x},#{copy_cursor_y} #{scroll_position} #{search_present}/#{search_timed_out}/#{search_count}/#{search_count_partial} <#{search_match}>'; }
trap 't kill-server 2>/dev/null || :' 0
t -f /dev/null new -d -s c -x 20 -y 8 "printf '012345678901234567ABCDabcdEFGHefghIJKLijkl\nhardAB\nCDbreak\n012345678901234567ABCDabcdEFGHefghIJKLijkl\nDONE\n'; sleep 600"
while :; do case "$(t capture-pane -p -t c)" in *DONE*) break;; esac; sleep 0.1; done
for mode in vi emacs; do
    t set -w -t c mode-keys "$mode"
    for command in search-forward search-backward search-forward-text search-backward-text search-forward-incremental search-backward-incremental; do
        for pattern in ABCD abcd '.*ijkl' '^012.*ijkl' 'AB.*ijkl' 'ABCDabcdEFGHefghIJKLijkl' 'ABCDbreak'; do
            echo "$mode $command $pattern"
            t copy-mode -q -t c
            t copy-mode -t c
            x history-top
            case "$command" in *incremental) x "$command" "=$pattern";; *) x "$command" "$pattern";; esac
            p
            x search-again
            p
            x search-reverse
            p
        done
    done
done
# Wide cells at a wrap boundary and overlapping regex starts on continuation rows.
t copy-mode -q -t c
t new-window -t c:1 -n wide "printf '01234567890123456界ABCDE\\nDONE\\n'; sleep 600"
while :; do case "$(t capture-pane -p -t c)" in *DONE*) break;; esac; sleep 0.1; done
for mode in vi emacs; do
    t set -w -t c mode-keys "$mode"
    for pattern in '界AB.*E' '.*E' '^' 'E *$'; do
        echo "wide $mode $pattern"
        t copy-mode -q -t c
        t copy-mode -t c
        x history-top
        x search-backward "$pattern"
        p
        x search-again
        p
        x search-reverse
        p
    done
done
# A mark wider than the viewport is clipped; scrolling its start out removes it.
t copy-mode -q -t c
t new-window -t c:2 -n clipped "awk 'BEGIN {printf \"A\"; for (i=0;i<198;i++) printf \"x\"; print \"Z\"; print \"DONE\"}'; sleep 600"
while :; do case "$(t capture-pane -p -t c)" in *DONE*) break;; esac; sleep 0.1; done
for mode in vi emacs; do
    echo "clipped $mode"
    t set -w -t c mode-keys "$mode"
    t copy-mode -q -t c
    t copy-mode -t c
    x history-top
    x search-forward 'A.*Z'
    p
    x scroll-down
    p
done
# Exercise the 200 ms all-history fallback on both implementations. Exact time and
# rows visited vary: the stable result is a rounded lower bound plus visible marks.
t copy-mode -q -t c
t set -g history-limit 200000
t new-session -d -s budget -x 80 -y 8 "awk 'BEGIN { for (i=0;i<150000;i++) print \"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"; print \"READY\" }'; sleep 600"
while :; do case "$(t capture-pane -p -t budget)" in *READY*) break;; esac; sleep 0.1; done
t set -w -t budget mode-keys vi
t copy-mode -t budget
t send-keys -t budget -X search-backward '[x]'
echo budget
# Verify the lower bound rather than comparing a timing-dependent exact count.
t display -p -t budget '#{search_present}/#{search_timed_out}/#{search_count_partial} #{?#{e|>=:#{search_count},1000},many,few}'
