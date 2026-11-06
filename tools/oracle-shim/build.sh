#!/usr/bin/env bash
# Builds the C++ oracle shim (libslt_oracle) against a DuckDB 1.5.6 release build.
#
# Usage: tools/oracle-shim/build.sh [duckdb-source-dir] [duckdb-build-dir]
#   duckdb-source-dir defaults to $DUCKDB_REF, then to the shared checkout under .context.
#   duckdb-build-dir  defaults to <duckdb-source-dir>/build/release.
# Output: target/oracle-shim/libslt_oracle.{dylib,so}
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
source_dir="${1:-${DUCKDB_REF:-$repo/.context/duckdb-download/duckdb-1.5.6}}"
build_dir="${2:-$source_dir/build/release}"
out_dir="$repo/target/oracle-shim"

if [[ ! -d "$source_dir/src/include" ]]; then
  echo "error: DuckDB source not found at $source_dir (pass it as the first argument or set DUCKDB_REF)" >&2
  exit 1
fi

cmake -S "$here" -B "$out_dir/cmake" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DDUCKDB_SOURCE_DIR="$source_dir" \
  -DDUCKDB_BUILD_DIR="$build_dir" \
  -DCMAKE_LIBRARY_OUTPUT_DIRECTORY="$out_dir"
cmake --build "$out_dir/cmake"
echo "built: $(ls "$out_dir"/libslt_oracle.*)"
