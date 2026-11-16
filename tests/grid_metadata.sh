#!/bin/sh
# Grid metadata probe (OSC 133 marks, OSC 8 links, tab cells, line times): feed it to
# tools/ref-tmux.sh and tools/loc-tmux.sh WORKTREE and diff stdout. Line times are
# printed as T. Known differences outside this area: capture-pane -e writes SGR as
# \033[0;31m...\033[0m where tmux writes \033[31m...\033[39m, and -N/-J/-T pad to the
# full width where tmux stops at the line's allocated or used cells.

# part 1
s=vtq1
T() { tmux -L $s "$@"; }
# prompts: several commands, scrolled into history
T -f /dev/null new -d -x 30 -y 6 "printf '\033]133;A\007\$ \033]133;B\007one\r\n\033]133;C\007out1\r\nout1b\r\n\033]133;D;0\007\033]133;A\007\$ \033]133;B\007two\r\n\033]133;C\007out2\r\n\033]133;D;1\007\033]133;P;k=s\007> cont\r\n\033]133;A\007\$ \033]133;B\007three\r\n\033]133;C\007o3\r\no3\r\no3\r\no3\r\n\033]133;A\007\$ '; sleep 100"
sleep 1
echo '--F'; T capture-pane -p -F -S - -E -
echo '--copy'
T copy-mode
T send -X history-top
for c in 'next-prompt' 'next-prompt' 'next-prompt -o' 'next-prompt' 'next-prompt' 'next-prompt' 'previous-prompt -o' 'previous-prompt' 'previous-prompt' 'previous-prompt' 'previous-prompt'; do
  T send -X $c
  T display -p "$c => #{copy_cursor_y}:#{copy_cursor_x} pos=#{scroll_position} line=#{copy_cursor_line}"
done
T send -X history-bottom
T send -X -N 3 previous-prompt
T display -p "N3 => #{copy_cursor_y}:#{copy_cursor_x} pos=#{scroll_position}"
T send -X cancel
# clear line keeps marks; resize reflow
T resize-window -x 12
sleep 0.3
echo '--F after resize 12'; T capture-pane -p -F -S - -E -
T resize-window -x 40
sleep 0.3
echo '--F after resize 40'; T capture-pane -p -F -S - -E -
T kill-server

# part 2
s=vtq2
T() { tmux -L $s "$@"; }
T -f /dev/null new -d -x 30 -y 8 "printf '\033]8;id=a;http://a\007AA\033]8;;\007 \033]8;;http://b\007BB\033]8;;\007 \033]8;;http://b\007bb\033]8;;\007\r\n\033]8;id=a;http://a\007again\033]8;;\007 \033]8;id=c;http://c\007\033[31mC\033[0mc\033]8;;\007\r\nplain\r\n\033]8;;http://w\007\344\270\255\346\226\207\033]8;;\007x\r\n\033]8;id=x:y=z;http://q?a=1;b=2\007semi\033]8;;\007\r\n\033]8;id=1;id=2;http://bad\007bad\033]8;;\007 \033]8;;http://e\033\\esc\033]8;;\033\\\r\nAB\033]8;;http://o\007\033[1;1HX\033]8;;\007'; sleep 100"
sleep 1
echo '--H'; T capture-pane -p -H
echo '--H S1'; T capture-pane -p -H -S 1 -E 1
echo '--HF'; T capture-pane -p -H -F -L
echo '--e'; T capture-pane -p -e
echo '--eC'; T capture-pane -p -e -C
echo '--C'; T capture-pane -p -C
echo '--F'; T capture-pane -p -F
echo '--copy'
T copy-mode
T send -X top-line
for x in 0 1 2 3 4 5 6 7 8 9; do T display -p "$x:#{copy_cursor_hyperlink}|"; T send -X cursor-right; done
T send -X start-of-line
T send -X cursor-down
T send -X cursor-down
T send -X cursor-down
for x in 0 1 2 3 4 5; do T display -p "w$x:#{copy_cursor_x}:#{copy_cursor_hyperlink}|"; T send -X cursor-right; done
echo '--MH'; T capture-pane -p -M -H
echo '--Me'; T capture-pane -p -M -e -E 1
T kill-server

# part 3
s=vtq3
T() { tmux -L $s "$@"; }
T -f /dev/null new -d -x 30 -y 10 "printf 'a\tb\tc\r\n\tlead\r\ntrail\t\r\nxxxxxxxxxxxxxxxxxxxx\r\x09y\r\n\033[41ma\tb\033[0m\r\n0123456789012345678901234567\tZ\r\n\033[3g\033[1;20H\033H\033[6;1Hq\tw\r\nab\033[1Dcd\tX\033[2D\033[@Y\r\n'; sleep 100"
sleep 1
echo '--p'; T capture-pane -p
echo '--T'; T capture-pane -p -T -N
echo '--e'; T capture-pane -p -e
echo '--C'; T capture-pane -p -C
echo '--F'; T capture-pane -p -F
echo '--copy'
T copy-mode
T send -X top-line
T send -X begin-selection
T send -X end-of-line
T send -X copy-selection
T show-buffer; echo '|'
T send -X top-line
T send -X cursor-right
T send -X cursor-right
T send -X cursor-right
T send -X begin-selection
T send -X cursor-down
T send -X cursor-down
T send -X copy-selection
T show-buffer; echo '|'
T send -X top-line
T send -X cursor-right
T display -p 'x=#{copy_cursor_x} w=#{copy_cursor_word} l=#{copy_cursor_line}|'
T send -X next-word
T display -p 'nw x=#{copy_cursor_x}'
T send -X next-word
T display -p 'nw x=#{copy_cursor_x}'
T send -X previous-word
T display -p 'pw x=#{copy_cursor_x}'
T send -X end-of-line
T display -p 'eol x=#{copy_cursor_x}'
T send -X cursor-down
T send -X end-of-line
T display -p 'eol2 x=#{copy_cursor_x}'
T send -X cursor-down
T send -X end-of-line
T display -p 'eol3 x=#{copy_cursor_x}'
T send -X select-line
T send -X copy-selection
T show-buffer; echo '|'
T send -X cancel
T copy-mode
T send -X top-line
T send -X search-forward 'b'
T display -p 'sb x=#{copy_cursor_x} y=#{copy_cursor_y}'
T kill-server

# part 4
s=vtq4
T() { tmux -L $s "$@"; }
norm() { sed -E 's/^[0-9]{9,} /T /'; }
T -f /dev/null new -d -x 20 -y 4 "seq 1 8; printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'; sleep 100"
sleep 1
echo '--I'; T capture-pane -p -I -S - | norm
T copy-mode
T display -p 'bottom tlt=#{top_line_time}'
T send -X history-top
T display -p 'top tlt=#{top_line_time}' | sed -E 's/[0-9]{9,}/T/'
T send -X cursor-down
T send -X cursor-down
T send -X cursor-down
T send -X cursor-down
T send -X cursor-down
T display -p 'down5 tlt=#{top_line_time} pos=#{scroll_position}' | sed -E 's/[0-9]{9,}/T/'
echo '--MI'; T capture-pane -p -M -I -S - | norm
echo '--MF'; T capture-pane -p -M -F -S - 
T send -X cancel
echo '--resize'
T resize-window -x 9
sleep 0.3
T capture-pane -p -I -F -S - | norm
T copy-mode
T send -X history-top
T display -p 'top9 tlt=#{top_line_time}' | sed -E 's/[0-9]{9,}/T/'
T send -X cancel
T resize-window -x 40
sleep 0.3
T capture-pane -p -I -F -S - | norm
T kill-server
