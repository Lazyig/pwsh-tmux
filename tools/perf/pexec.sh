#!/bin/sh
# pexec.sh : run stdin as a sh script in the benchmark pod, with the pod's
# userland (/work/root) on the library, terminfo and Python paths and cwd /work.
#   printf 'python3 /work/h/bench.py ...\n' | tools/perf/pexec.sh
. "$(dirname "$0")/pod-env.sh"
PROLOGUE=${PERF_PROLOGUE:-'export LD_LIBRARY_PATH=/work/root/usr/lib:/work/root/lib TERMINFO_DIRS=/work/root/etc/terminfo:/work/root/usr/share/terminfo LANG=C.UTF-8 PYTHONHOME=/work/root/usr TERM=xterm-256color; unset TMUX; cd /work'}
{ echo "$PROLOGUE"; cat; } | $K sh
