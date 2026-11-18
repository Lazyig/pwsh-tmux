#!/bin/sh
# run.sh [--base REV] MODE [ARGS...]
#   Compares vendor/vt100 at REV with the working tree's vendor/vt100.
#   MODE is fuzz, resize-fuzz, cmp or bench (see vt100-diff --help), or
#   selfcheck [FUZZ_N] [RESIZE_N]: plants each inject.py mutation into a copy of
#   the base and checks that the modes catch it.
# REV defaults to $VT100_DIFF_BASE_REV, else the merge base of HEAD and main,
# else HEAD. Everything is written under tools/vt100-diff/target/.
set -eu
TOOL=$(cd "$(dirname "$0")" && pwd)
ROOT=$(git -C "$TOOL" rev-parse --show-toplevel)
T=$TOOL/target
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"

REV=${VT100_DIFF_BASE_REV:-}
if [ "${1:-}" = "--base" ]; then REV=$2; shift 2; fi
if [ -z "$REV" ]; then
    REV=$(git -C "$ROOT" merge-base HEAD main 2>/dev/null || git -C "$ROOT" rev-parse HEAD)
fi
REV=$(git -C "$ROOT" rev-parse --verify "$REV^{commit}")
SHORT=$(git -C "$ROOT" rev-parse --short "$REV")

# extract REV's vendor/vt100 to DIR (mtimes set to now so cargo rebuilds)
extract() {
    rm -rf "$1.tmp"; mkdir -p "$1.tmp"
    git -C "$ROOT" archive --format=tar "$REV" vendor/vt100 | tar -x -m -C "$1.tmp"
    rm -rf "$1"; mv "$1.tmp/vendor/vt100" "$1"; rm -rf "$1.tmp"
}

# the base copy, renamed so both copies can be linked into one binary
mkdir -p "$T"
if [ "$(cat "$T/vt100-base.rev" 2>/dev/null || true)" != "$REV" ]; then
    extract "$T/vt100-base"
    python3 - "$T/vt100-base/Cargo.toml" <<'EOF'
import re, sys
p = sys.argv[1]; s = open(p, encoding='utf-8').read()
s, n1 = re.subn(r'(?m)^(\[package\][^\[]*?)^name = "vt100"$', r'\1name = "vt100-base"', s)
s, n2 = re.subn(r'(?m)^(\[lib\][^\[]*?)^name = "vt100"$', r'\1name = "vt100_base"', s)
if not n1: sys.exit('run.sh: no [package] name = "vt100" in ' + p)
open(p, 'w', encoding='utf-8').write(s)
EOF
    echo "$REV" > "$T/vt100-base.rev"
fi
# same dependency versions as the application
[ -f "$TOOL/Cargo.lock" ] || cp "$ROOT/Cargo.lock" "$TOOL/Cargo.lock"

build() { # build MANIFEST TARGET_DIR
    CARGO_TARGET_DIR=$2 cargo build --quiet --release --manifest-path "$1" >&2
}

export VT100_DIFF_BASE="$SHORT"
if [ "${1:-}" != "selfcheck" ]; then
    build "$TOOL/Cargo.toml" "$T/build"
    export VT100_DIFF_OUT="${VT100_DIFF_OUT:-$T/vt100-diff-out}"
    exec "$T/build/release/vt100-diff" "$@"
fi

# selfcheck: every mutation must be caught by each mode it lists
FN=${2:-20000}; RN=${3:-400}
SC=$T/selfcheck; mkdir -p "$SC"
fail=0
for line in $(python3 "$TOOL/inject.py" --list | tr ' ' ':'); do
    m=${line%%:*}; modes=$(echo "${line#*:}" | tr ',' ' ')
    d=$SC/$m
    extract "$d/vt100"
    python3 "$TOOL/inject.py" "$d/vt100" "$m"
    sed -e "s#\"../../vendor/vt100\"#\"$d/vt100\"#" -e "s#\"target/vt100-base\"#\"$T/vt100-base\"#" \
        -e "s#\"src/main.rs\"#\"$TOOL/src/main.rs\"#" "$TOOL/Cargo.toml" > "$d/Cargo.toml"
    cp "$TOOL/Cargo.lock" "$d/Cargo.lock"
    build "$d/Cargo.toml" "$SC/build"
    cp "$SC/build/release/vt100-diff" "$d/vt100-diff"
    for mode in $modes; do
        n=$FN; [ "$mode" = resize-fuzz ] && n=$RN
        set +e
        VT100_DIFF_OUT=$d/out-$mode "$d/vt100-diff" "$mode" "$n" --quiet > "$d/$mode.log" 2>&1
        rc=$?
        set -e
        if [ $rc -eq 1 ]; then
            echo "selfcheck $m: $mode caught it ($(grep -m1 MISMATCH "$d/$mode.log"))"
        else
            echo "selfcheck $m: $mode MISSED it (exit $rc, see $d/$mode.log)"; fail=1
        fi
    done
done
[ $fail -eq 0 ] && echo "selfcheck ok (base $SHORT)" || { echo "selfcheck FAILED"; exit 1; }
