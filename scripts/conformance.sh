#!/usr/bin/env bash
# Runs DuckDB's sqllogictest corpus against roastedduck, compares with the C++ reference
# results, enforces the ratchet and refreshes the dashboard.
#
# Usage: scripts/conformance.sh [--update] [test paths...]   (default: the whole corpus)
#   --update   add newly passing files to conformance/passing.txt
# Env: REF (DuckDB 1.5.6 source root), JOBS (parallel workers, default 1).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REF="${REF:-$ROOT/.context/duckdb-download/duckdb-1.5.6}"
JOBS="${JOBS:-1}"
UPDATE=0
if [[ "${1:-}" == "--update" ]]; then UPDATE=1; shift; fi
PATHS=("$@")
[[ ${#PATHS[@]} -eq 0 ]] && PATHS=(test)

cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release -p slt -j "$JOBS" >/dev/null
"$ROOT/scripts/sign.sh" "$ROOT/target/release/slt"
SLT="$ROOT/target/release/slt"
OUT="$ROOT/target/slt"
mkdir -p "$OUT"

"$SLT" run --engine rust --root "$REF" --jobs "$JOBS" --timeout-secs 300 \
  --out "$OUT/rust-run.jsonl" "${PATHS[@]}" | sed -n '/^group/,$p'

# A partial run updates the latest full results (target/slt/rust.jsonl) for the files it
# ran, so the ratchet compares every file against its most recent verdict.
python3 - "$OUT/rust.jsonl" "$OUT/rust-run.jsonl" <<'PY'
import json, sys, os
full, run = sys.argv[1], sys.argv[2]
rows = {}
if os.path.exists(full):
    for line in open(full):
        if line.strip():
            row = json.loads(line)
            rows[row["file"]] = row
for line in open(run):
    if line.strip():
        row = json.loads(line)
        rows[row["file"]] = row
with open(full, "w") as out:
    for name in sorted(rows):
        out.write(json.dumps(rows[name]) + "\n")
PY

"$SLT" compare conformance/reference-cpp.jsonl "$OUT/rust.jsonl" --out conformance/dashboard.md
"$SLT" ratchet check "$OUT/rust.jsonl"
if [[ $UPDATE -eq 1 ]]; then
  "$SLT" ratchet update "$OUT/rust.jsonl"
fi
echo "passing files: $(grep -cv '^\s*$' conformance/passing.txt 2>/dev/null || echo 0)"
