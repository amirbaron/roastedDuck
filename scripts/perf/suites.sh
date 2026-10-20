#!/bin/bash
# Runs benchmark suites on both engines and keeps each table and its JSON.
#
# usage: scripts/perf/suites.sh TAG [THREADS] [SUITE...]   (default: 14 threads; tpch3 h2og h2oj hits)
# writes $RUSTDB_PERF_DIR/res/TAG-SUITE-tTHREADS.{txt,json}; REPS=N sets the timed runs.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DIR="${RUSTDB_PERF_DIR:-$ROOT/.context/perf}"
TAG=$1
T=${2:-14}
shift $(($# > 1 ? 2 : 1))
SUITES=${*:-tpch3 h2og h2oj hits}
mkdir -p "$DIR/res"
for s in $SUITES; do
  out="$DIR/res/$TAG-$s-t$T"
  python3 -I "$ROOT/scripts/perf/bench.py" "$s" --threads "$T" --reps "${REPS:-5}" --out "$out.json" > "$out.txt" 2>&1
  tail -1 "$out.txt"
done
