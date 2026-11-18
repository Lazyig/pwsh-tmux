#!/bin/sh
# capcmp.sh BIN FILE [COLSxROWS]
#   cat FILE in a fresh pane (default 120x39) of a private server and print
#   sha1 (12 hex) of capture-pane -e -S - -E -, of -J -S -, of -N -S -,
#   then #{history_size} #{cursor_x},#{cursor_y} #{pane_title}.
# capcmp.sh --cmp BASE CAND FILE... [-- COLSxROWS]
#   the same for two binaries; prints SAME or DIFF per file, exit 1 on any DIFF.
# Works with pwsh-tmux and tmux alike. Sockets are ${CAPCMP_PREFIX:-cap}-PID-TAG;
# every server started here is killed on exit.
set -u
P=${CAPCMP_PREFIX:-cap}
H=$(mktemp -d)
trap 'rm -rf "$H"' EXIT
trap 'exit 130' INT TERM

one() { # one BIN FILE SIZE TAG (runs in a subshell, which kills its server on exit)
    B=$1; F=$2; C=${3%x*}; R=${3#*x}; S=$P-$$-$4
    t() { env -u TMUX -u PWSH_TMUX -u PWSH_TMUX_PANE HOME="$H" SHELL=/bin/sh TERM=xterm-256color LANG=C.UTF-8 "$B" -L "$S" "$@"; }
    trap 't kill-server 2>/dev/null' EXIT
    t -f /dev/null new -d -s h -x "$C" -y "$R" 'exec cat' || return 1
    t new-window -t h: -n w "cat '$F'; '$B' -L '$S' wait-for -S d; exec cat" &
    t wait-for d
    wait
    # the server may still be reading the pane's last output: wait until the capture settles
    prev=; i=0
    while [ $i -lt 50 ]; do
        sleep 0.3
        cur=$(t capture-pane -t h:w -p -e -S - -E - | sha1sum)
        [ "$cur" = "$prev" ] && break
        prev=$cur; i=$((i+1))
    done
    a=$(echo "$cur" | cut -c1-12)
    b=$(t capture-pane -t h:w -p -J -S - | sha1sum | cut -c1-12)
    n=$(t capture-pane -t h:w -p -N -S - | sha1sum | cut -c1-12)
    c=$(t display -p -t h:w '#{history_size} #{cursor_x},#{cursor_y} #{pane_title}')
    echo "$a $b $n $c"
}

abs() { case $1 in /*) echo "$1" ;; *) echo "$(pwd)/$1" ;; esac; }

if [ "${1:-}" = "--cmp" ]; then
    [ $# -ge 4 ] || { sed -n '2,10p' "$0"; exit 2; }
    BASE=$(abs "$2"); CAND=$(abs "$3"); shift 3
    SIZE=120x39; FILES=
    while [ $# -gt 0 ]; do
        if [ "$1" = "--" ]; then SIZE=$2; break; fi
        FILES="$FILES $(abs "$1")"; shift
    done
    rc=0; k=0
    for f in $FILES; do
        k=$((k+1))
        x=$(one "$BASE" "$f" "$SIZE" b$k); y=$(one "$CAND" "$f" "$SIZE" c$k)
        if [ "$x" = "$y" ]; then echo "SAME $f: $x"; else echo "DIFF $f: base $x / cand $y"; rc=1; fi
    done
    exit $rc
fi
[ $# -ge 2 ] || { sed -n '2,10p' "$0"; exit 2; }
(one "$(abs "$1")" "$(abs "$2")" "${3:-120x39}" 1)
