#!/bin/sh
# sync.sh [REMOTE_DIR] : copy the harness (bench.py, gen_data.py, summarize.py,
# capcmp.sh) into the benchmark pod, default /work/h.
set -eu
D=$(cd "$(dirname "$0")" && pwd); R=${1:-/work/h}
printf 'mkdir -p %s\n' "$R" | "$D/pexec.sh"
for f in bench.py gen_data.py summarize.py capcmp.sh; do
    "$D/put.sh" "$D/$f" "$R/$f" > /dev/null
done
printf 'chmod +x %s/capcmp.sh; sha256sum %s/bench.py %s/gen_data.py %s/summarize.py %s/capcmp.sh\n' "$R" "$R" "$R" "$R" "$R" | "$D/pexec.sh"
