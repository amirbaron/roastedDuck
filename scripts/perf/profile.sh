#!/bin/bash
# Profiles one query on one engine with macOS `sample`: the engine's CLI runs the query
# REPEAT times on DB while `sample` records SECONDS seconds, then the heaviest functions
# (self time) are printed. The full call tree stays in OUT (default /tmp/profile-ENGINE.txt).
#
# usage: scripts/perf/profile.sh rust|duck DB.duckdb "SQL"|@FILE.sql [SECONDS] [THREADS] [REPEAT]
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REF="${DUCKDB_SRC:-$ROOT/.context/duckdb-download/duckdb-1.5.6}"
ENGINE=$1
DB=$2
SQL=$3
if [[ "$SQL" == @* ]]; then SQL=$(grep -v "^\s*--" "${SQL#@}" | tr "\n" " "); fi
SQL="${SQL%"${SQL##*[![:space:]]}"}"
SECONDS_=${4:-10}
THREADS=${5:-14}
REPEAT=${6:-1000}
OUT=${OUT:-/tmp/profile-$ENGINE.txt}
case "$ENGINE" in
rust) CLI="$ROOT/target/release/roastedDuck" ;;
duck) CLI="$REF/build/release/duckdb" ;;
*) echo "engine must be rust or duck" >&2; exit 1 ;;
esac
SCRIPT=$(mktemp)
{
  echo "SET threads=$THREADS;"
  for _ in $(seq "$REPEAT"); do echo "${SQL%;};"; done
} > "$SCRIPT"
"$CLI" "$DB" < "$SCRIPT" > /dev/null &
PID=$!
sleep 1
/usr/bin/sample "$PID" "$SECONDS_" -file "$OUT" > /dev/null 2>&1 || true
kill -9 "$PID" 2>/dev/null || true
wait "$PID" 2>/dev/null || true
rm -f "$SCRIPT"
# Demangle Rust symbols when rustfilt is installed (`cargo install rustfilt`).
if command -v rustfilt > /dev/null; then
  rustfilt -i "$OUT" -o "$OUT.demangled" && mv "$OUT.demangled" "$OUT"
fi
# The "Sort by top of stack" section: functions by self samples.
sed -n '/Sort by top of stack/,/Binary Images/p' "$OUT" | head -${TOP:-40}
