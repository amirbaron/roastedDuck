#!/usr/bin/env bash
# Generates crates/icu/data/icu66_collation.bin: the collation data of DuckDB's ICU 66 (the
# root collation data, the tailoring every bundle of the coll tree loads, and the NFC data
# the collation code reads), exported by scripts/icu-collation-data.cpp from DuckDB's own
# ICU sources and embedded data. The C++ is an offline tool; rustdb ships only the data.
#
#   REF=<duckdb-1.5.6 source> scripts/icu-collation-data.sh [build dir]
#   JOBS=1 (default) limits compilation to one process.
#
# The build dir (default: a temporary directory) keeps the compiled ICU objects; the tool
# is left there as `icu-collation-data` (`keys <collation>` prints DuckDB's sort keys of
# the lines on stdin).
set -euo pipefail
cd "$(dirname "$0")/.."
: "${REF:?set REF to the DuckDB 1.5.6 source directory}"
icu="$REF/extension/icu/third_party/icu"
build="${1:-$(mktemp -d)}"
mkdir -p "$build/obj"
cxx="${CXX:-c++}"
jobs="${JOBS:-1}"
if [[ ! "$jobs" =~ ^[1-9][0-9]*$ ]]; then
    echo "JOBS must be a positive integer" >&2
    exit 1
fi
# (uloc.cpp includes DuckDB's string_util.hpp)
flags=(-std=c++11 -O2 -w -I"$icu/common" -I"$icu/i18n" -I"$REF/src/include")

# DuckDB's ICU: every source of common/ and i18n/ (their CMakeLists list them all) and
# stubdata.cpp, which embeds DuckDB's icudt66l.dat
sources=("$icu"/common/*.cpp "$icu"/i18n/*.cpp "$icu/stubdata/stubdata.cpp")
printf '%s\n' "${sources[@]}" | xargs -P "$jobs" -I{} sh -c '
    build="$1" source="$2"
    shift 2
    obj="$build/obj/$(basename "$(dirname "$source")")_$(basename "$source" .cpp).o"
    [ -f "$obj" ] && [ "$obj" -nt "$source" ] || exec "$@" -c "$source" -o "$obj"
' sh "$build" {} "$cxx" "${flags[@]}"
"$cxx" "${flags[@]}" scripts/icu-collation-data.cpp "$build"/obj/*.o -o "$build/icu-collation-data"
scripts/sign.sh "$build/icu-collation-data"
# (the types come from a process of their own, see `Types`)
# shellcheck disable=SC2046
"$build/icu-collation-data" export crates/icu/data/icu66_collation.bin $("$build/icu-collation-data" types)
