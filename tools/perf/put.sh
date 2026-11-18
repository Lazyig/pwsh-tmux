#!/bin/sh
# put.sh LOCAL_FILE REMOTE_PATH : copy a file into the benchmark pod in 512 KiB
# chunks, each checked with sha256 and retried (large kubectl exec stdin
# transfers can be cut short). Prints the remote and local sha256.
set -u
. "$(dirname "$0")/pod-env.sh"
L="$1"; R="$2"; T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
split -b 512k "$L" "$T/c."
$K sh -c "rm -rf '$R.d'; mkdir -p '$R.d'" || exit 1
for c in "$T"/c.*; do
  b=$(basename "$c"); h=$(sha256sum < "$c" | cut -d' ' -f1); n=0
  while :; do
    r=$($K sh -c "cat > '$R.d/$b'; sha256sum < '$R.d/$b' | cut -d' ' -f1" < "$c" 2>/dev/null)
    [ "$r" = "$h" ] && break
    n=$((n+1)); [ $n -lt 5 ] || { echo "chunk $b failed"; exit 1; }
  done
done
$K sh -c "cat '$R.d'/c.* > '$R' && rm -rf '$R.d' && sha256sum '$R'"
sha256sum < "$L"
