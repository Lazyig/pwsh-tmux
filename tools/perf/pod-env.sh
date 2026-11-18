# Sourced by pexec.sh and put.sh: where the benchmark pod is.
# Settings come from the file named by $PERF_ENV, else tools/perf/pod.env
# (not committed; see pod.env.example), else the environment:
#   PERF_KUBECTL  command that runs kubectl with the right credentials, e.g. "kubectl --context lab"
#   PERF_NS       namespace of the pod
#   PERF_POD      pod name
if [ -n "${PERF_ENV:-}" ]; then
    . "$PERF_ENV"
elif [ -f "$(dirname "$0")/pod.env" ]; then
    . "$(dirname "$0")/pod.env"
fi
: "${PERF_KUBECTL:?set PERF_KUBECTL (or PERF_ENV), see tools/perf/README.md}"
: "${PERF_NS:?set PERF_NS (or PERF_ENV)}"
: "${PERF_POD:?set PERF_POD (or PERF_ENV)}"
# word splitting of PERF_KUBECTL is intended
K="$PERF_KUBECTL exec -i -n $PERF_NS $PERF_POD --"
