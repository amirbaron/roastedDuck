#!/usr/bin/env bash
# Full frozen upstream ADBC suite using public C API helpers and an opt-in native test bridge.
set -euo pipefail
BUILD_ONLY=0
if [[ "${1:-}" == --build-only ]]; then
 BUILD_ONLY=1
 shift
fi
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REF="${REF:-$ROOT/.context/duckdb-download/duckdb-1.5.6}"
OUT="${ADBC_TEST_OUTPUT_DIR:?Set ADBC_TEST_OUTPUT_DIR to a dedicated external artifact directory}"
LIBDIR="${ADBC_TEST_LIB_DIR:?Set ADBC_TEST_LIB_DIR to the library directory}"
CXX="${CXX:-clang++}"
LIBRARY_NAME=libduckdb.so
PLATFORM_LINK_FLAGS=(-ldl -pthread)
NATIVE_VISIBILITY_FLAGS=(-fvisibility=hidden -fvisibility-inlines-hidden)
if [[ "$(uname)" == Darwin ]]; then
 LIBRARY_NAME=libduckdb.dylib
 PLATFORM_LINK_FLAGS=()
 NATIVE_VISIBILITY_FLAGS=()
fi
[[ -f "$LIBDIR/$LIBRARY_NAME" ]] || { echo "No $LIBRARY_NAME in $LIBDIR" >&2; exit 2; }
mkdir -p "$OUT/obj" "$OUT/src" "$OUT/data"
python3 "$ROOT/scripts/adbc-tests/prepare.py" "$REF/test/api/adbc/test_adbc.cpp" "$OUT/src/test_adbc.cpp" > "$OUT/source-manifest.txt"
FLAGS=(-std=c++17 -O1 -g -w -I "$ROOT/scripts/adbc-tests/shim" -I "$ROOT/scripts/capi-tests/shim" -I "$REF/src/include" -I "$REF/test/api/adbc" -I "$REF/third_party/catch")
if [[ "$(uname)" != Darwin ]]; then
 # Stand-ins for private C++ types must not preempt the selected engine's internals.
 FLAGS+=("${NATIVE_VISIBILITY_FLAGS[@]}")
fi
if [[ "${ADBC_TEST_BACKEND:-rust}" == rust ]]; then FLAGS+=(-DADBC_RUST_TARGET); fi
sources=("$ROOT/scripts/adbc-tests/main.cpp" "$REF/test/api/adbc/driver_manager.cpp" "$REF/src/common/adbc/nanoarrow/schema.cpp" "$REF/src/common/adbc/nanoarrow/metadata.cpp" "$REF/src/common/adbc/nanoarrow/allocator.cpp" "$OUT/src/test_adbc.cpp")
objects=()
config="$OUT/build-config.txt"
new_config="${ADBC_TEST_BACKEND:-rust}|$LIBDIR|$REF|$CXX"
if [[ ! -f "$config" || "$(cat "$config")" != "$new_config" ]]; then echo "$new_config" > "$config"; fi
for i in "${!sources[@]}"; do
 object="$OUT/obj/$i.o"
 if [[ ! -f "$object" || "${sources[$i]}" -nt "$object" || "$config" -nt "$object" ]] || [[ -n "$(find "$ROOT/scripts/adbc-tests/shim" "$ROOT/scripts/capi-tests/shim" -type f -newer "$object" -print -quit)" ]]; then
  "$CXX" "${FLAGS[@]}" -c "${sources[$i]}" -o "$object" >> "$OUT/build.log" 2>&1
 fi
 objects+=("$object")
done
if [[ "${ADBC_TEST_BACKEND:-rust}" == cpp ]]; then
 "$CXX" -std=c++17 -O1 -g ${NATIVE_VISIBILITY_FLAGS[@]+"${NATIVE_VISIBILITY_FLAGS[@]}"} -I "$REF/src/include" -I "$REF/third_party/fmt/include" -c "$ROOT/scripts/adbc-tests/cpp_reference_bridge.cpp" -o "$OUT/obj/cpp_reference.o" >> "$OUT/build.log" 2>&1
 objects+=("$OUT/obj/cpp_reference.o")
fi
"$CXX" "${objects[@]}" -L "$LIBDIR" -lduckdb -Wl,-rpath,"$LIBDIR" ${PLATFORM_LINK_FLAGS[@]+"${PLATFORM_LINK_FLAGS[@]}"} -o "$OUT/adbc-tests" >> "$OUT/build.log" 2>&1
"$ROOT/scripts/sign.sh" "$OUT/adbc-tests" > "$OUT/sign.log" 2>&1
if [[ "$(uname)" == Darwin ]]; then
 codesign --verify --strict "$OUT/adbc-tests"
fi
if [[ "$BUILD_ONLY" == 1 ]]; then
 exit 0
fi
(cd "${ADBC_TEST_WORKING_DIR:-$REF}" && CAPI_TEST_DIR="$OUT/data" DUCKDB_INSTALL_LIB="$LIBDIR/$LIBRARY_NAME" "$OUT/adbc-tests" "$@") > "$OUT/test.log" 2>&1
