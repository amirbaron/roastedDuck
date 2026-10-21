#!/usr/bin/env bash
# Builds zstd_ref, the reference zstd encoder (DuckDB's vendored zstd 1.5.6, linked from the
# DuckDB 1.5.6 release build) that rustdb-zstd's frames are compared with.
#
# Usage: tools/zstd-ref/build.sh [duckdb-source-dir] [duckdb-build-dir]
#   duckdb-source-dir defaults to $DUCKDB_REF, then to the shared checkout under .context.
#   duckdb-build-dir  defaults to <duckdb-source-dir>/build/release.
# Output: target/zstd-ref/zstd_ref
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
source_dir="${1:-${DUCKDB_REF:-$repo/.context/duckdb-download/duckdb-1.5.6}}"
build_dir="${2:-$source_dir/build/release}"
out_dir="$repo/target/zstd-ref"

if [[ ! -f "$source_dir/third_party/zstd/include/zstd.h" ]]; then
  echo "error: DuckDB source not found at $source_dir (pass it as the first argument or set DUCKDB_REF)" >&2
  exit 1
fi

mkdir -p "$out_dir"
c++ -std=c++11 -O2 -DNDEBUG -DZSTD_STATIC_LINKING_ONLY \
  -I "$source_dir/third_party/zstd/include" \
  "$here/zstd_ref.cpp" \
  -L "$build_dir/src" -lduckdb -Wl,-rpath,"$build_dir/src" \
  -o "$out_dir/zstd_ref"
"$repo/scripts/sign.sh" "$out_dir/zstd_ref"
echo "built: $out_dir/zstd_ref"
