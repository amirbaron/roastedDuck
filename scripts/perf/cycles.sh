#!/bin/bash
# Cycles and instructions one run of a query costs, from the process totals that
# `/usr/bin/time -l` reports (macOS): a process running the query N times minus one running
# it once, divided by N - 1, so opening the database does not count. Unlike wall time, the
# counts barely move when other work shares the machine; alternate the CLIs to compare them.
#
# usage: scripts/perf/cycles.sh CLI DB "SQL"|@FILE.sql [THREADS] [N]
#        (CLI: target/release/rustdb, DuckDB's build/release/duckdb, or another build's CLI)
set -euo pipefail
CLI=$1
DB=$2
SQL=$3
if [[ "$SQL" == @* ]]; then SQL=$(grep -v "^\s*--" "${SQL#@}" | tr "\n" " "); fi
SQL="${SQL%"${SQL##*[![:space:]]}"}"
THREADS=${4:-14}
N=${5:-4}
SCRIPT=$(mktemp)
counts() {
  {
    echo "SET threads=$THREADS;"
    for _ in $(seq "$1"); do echo "${SQL%;};"; done
  } > "$SCRIPT"
  /usr/bin/time -l "$CLI" "$DB" < "$SCRIPT" 2>&1 > /dev/null |
    awk '/cycles elapsed/ { c = $1 } /instructions retired/ { i = $1 } END { print c, i }'
}
read -r c1 i1 < <(counts 1)
read -r cn in < <(counts "$N")
rm -f "$SCRIPT"
awk -v c1="$c1" -v i1="$i1" -v cn="$cn" -v in_="$in" -v n="$N" \
  'BEGIN { printf "Gcycles/run %.3f  Ginstructions/run %.3f\n", (cn - c1) / (n - 1) / 1e9, (in_ - i1) / (n - 1) / 1e9 }'
