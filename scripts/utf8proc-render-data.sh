#!/usr/bin/env bash
# REF=<frozen DuckDB 1.5.6 source> scripts/utf8proc-render-data.sh <external build dir>
# Offline generation only. rustdb links no C++ code.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${REF:?set REF to the frozen DuckDB 1.5.6 source directory}"
build="${1:?provide an external build directory}"
mkdir -p "$build" crates/common/data
"${CXX:-c++}" -std=c++17 -O2 -I"$REF/third_party/utf8proc/include" \
    scripts/utf8proc-render-data.cpp "$REF/third_party/utf8proc/utf8proc.cpp" \
    -o "$build/utf8proc-render-data"
scripts/sign.sh "$build/utf8proc-render-data"
"$build/utf8proc-render-data" "$build/utf8proc_render.bin"
cmp -s "$build/utf8proc_render.bin" crates/common/data/utf8proc_render.bin || \
    cp "$build/utf8proc_render.bin" crates/common/data/utf8proc_render.bin
