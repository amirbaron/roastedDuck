#!/usr/bin/env bash
# Runs DuckDB's sqllogictest corpus against rustdb, compares with the C++ reference
# results, enforces the ratchet and refreshes the dashboard.
#
# Usage: scripts/conformance.sh [--update] [test paths...]   (default: the whole corpus)
#   --update   add newly passing files to conformance/passing.txt
# Env: REF (DuckDB 1.5.6 source root), JOBS (parallel workers, default 6).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REF="${REF:-$ROOT/.context/duckdb-download/duckdb-1.5.6}"
JOBS="${JOBS:-6}"
UPDATE=0
if [[ "${1:-}" == "--update" ]]; then UPDATE=1; shift; fi
PATHS=("$@")
[[ ${#PATHS[@]} -eq 0 ]] && PATHS=(test)

cd "$ROOT"
cargo build --release -p slt -j "$JOBS" >/dev/null
SLT="$ROOT/target/release/slt"
OUT="$ROOT/target/slt"
mkdir -p "$OUT"

"$SLT" run --engine rust --root "$REF" --jobs "$JOBS" --timeout-secs 300 \
  --out "$OUT/rust.jsonl" "${PATHS[@]}" | sed -n '/^group/,$p'

"$SLT" compare conformance/reference-cpp.jsonl "$OUT/rust.jsonl" --out conformance/dashboard.md
"$SLT" ratchet check "$OUT/rust.jsonl"
if [[ $UPDATE -eq 1 ]]; then
  "$SLT" ratchet update "$OUT/rust.jsonl"
fi
echo "passing files: $(grep -cv '^\s*$' conformance/passing.txt 2>/dev/null || echo 0)"
