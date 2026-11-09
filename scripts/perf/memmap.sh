#!/bin/bash
# Shows where an engine's resident memory goes while it runs one query repeatedly: polls
# `vmmap -summary` and prints the snapshot with the largest physical footprint (malloc
# zones vs. mapped buffer blocks vs. the binary).
#
# usage: scripts/perf/memmap.sh rust|duck DB.duckdb "SQL"|@FILE.sql [THREADS] [SECONDS]
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REF="${DUCKDB_SRC:-$ROOT/.context/duckdb-download/duckdb-1.5.6}"
ENGINE=$1
DB=$2
SQL=$3
if [[ "$SQL" == @* ]]; then SQL=$(grep -v "^\s*--" "${SQL#@}" | tr "\n" " "); fi
SQL="${SQL%"${SQL##*[![:space:]]}"}"
THREADS=${4:-14}
SECONDS_=${5:-10}
case "$ENGINE" in
rust) CLI="$ROOT/target/release/roastedDuck" ;;
duck) CLI="$REF/build/release/duckdb" ;;
*) echo "engine must be rust or duck" >&2; exit 1 ;;
esac
SCRIPT=$(mktemp)
{
  echo "SET threads=$THREADS;"
  for _ in $(seq 500); do
    echo "${SQL%;};"
    # h2oai answers are temporary tables
    if [[ "$SQL" == CREATE* ]]; then echo "DROP TABLE ans;"; fi
  done
} > "$SCRIPT"
"$CLI" "$DB" < "$SCRIPT" > /dev/null 2>&1 &
PID=$!
BEST=0
BEST_OUT=""
END=$((SECONDS + SECONDS_))
while [[ $SECONDS -lt $END ]] && kill -0 "$PID" 2>/dev/null; do
  OUT=$(vmmap -summary "$PID" 2>/dev/null || true)
  FOOT=$(echo "$OUT" | awk '/^Physical footprint:/ {print $3}')
  VALUE=$(echo "$FOOT" | sed -E 's/([0-9.]+)([KMG]).*/\1 \2/' | awk '{m = ($2 == "G") ? 1024 : ($2 == "K" ? 1/1024 : 1); print int($1 * m)}')
  if [[ -n "$VALUE" && "$VALUE" -gt "$BEST" ]]; then
    BEST=$VALUE
    BEST_OUT=$OUT
  fi
done
kill -9 "$PID" 2>/dev/null || true
wait "$PID" 2>/dev/null || true
rm -f "$SCRIPT"
echo "peak physical footprint: ${BEST} MB"
echo "$BEST_OUT" | sed -n '/^REGION TYPE/,/^TOTAL/p' | awk '$0 ~ /REGION|TOTAL/ || $3 ~ /[0-9]M|G/ || $4 ~ /[0-9]M|G/'
echo "$BEST_OUT" | sed -n '/MALLOC ZONE/,/^TOTAL/p'
