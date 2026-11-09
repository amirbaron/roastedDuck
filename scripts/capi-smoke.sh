#!/usr/bin/env bash
# Builds roastedduck's libduckdb and runs a C program compiled against DuckDB's own duckdb.h.
# Dev-only: needs a C compiler and the reference DuckDB source tree (REF).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REF="${REF:-$ROOT/.context/duckdb-download/duckdb-1.5.6}"
cargo build --release -p roastedduck-capi --manifest-path "$ROOT/Cargo.toml"
"$ROOT/scripts/sign.sh"
OUT="$ROOT/target/capi-smoke"
mkdir -p "$OUT"
cc -std=c11 -Wall -Werror -I "$REF/src/include" "$ROOT/crates/capi/tests/smoke.c" \
  -L "$ROOT/target/release" -lduckdb -Wl,-rpath,"$ROOT/target/release" -o "$OUT/smoke"
"$OUT/smoke"
