#!/usr/bin/env bash
# Builds DuckDB's own C API tests (test/api/capi, Catch) against a libduckdb and runs them,
# one executable per test file, printing the Catch totals of each.
#
# Usage: scripts/capi-tests/run.sh [--lib rust|cpp] [--no-build] [test_file.cpp...]
#   --lib rust   link rustdb's target/release/libduckdb.dylib (default; built first)
#   --lib cpp    link the reference C++ library, to check the harness itself
#   --no-build   skip `cargo build` (rust only)
# Extra Catch arguments go after `--` (e.g. `-- "Test pending statements in C API"`).
# Env: REF (DuckDB 1.5.6 source root), CXX.
# RUSTDB_CAPI_LIBRARY_DIR selects an already-signed immutable library directory
# (requires --no-build); RUSTDB_CAPI_TEST_BIN_DIR preserves a separate run's binaries/logs.
# RUSTDB_CAPI_TEST_OUTPUT_DIR relocates all harness objects/sources to external storage.
#
# The tests may only reach the engine through duckdb.h. The C++ helpers they include
# (test_helpers.hpp, exception.hpp, arrow.hpp) are replaced by the header-only stand-ins in
# shim/; capi_tester.hpp and capi_tester.cpp are DuckDB's, unmodified. Test cases that need
# the C++ API (duckdb::DuckDB, duckdb::Connection internals) are excluded below by name.
# Test-only tooling: nothing here is part of the shipped library.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REF="${REF:-$ROOT/.context/duckdb-download/duckdb-1.5.6}"
CXX="${CXX:-clang++}"
SYSTEM="$(uname)"
LIBRARY_NAME=libduckdb.so
LINK_STRIP_FLAGS=(-Wl,--gc-sections)
# Stand-in C++ helpers share names with DuckDB internals but have different layouts.
# Keep them out of ELF's dynamic symbol table so they cannot preempt the reference DLL.
PLATFORM_FLAGS=(-ffunction-sections -fdata-sections -pthread -fvisibility=hidden -fvisibility-inlines-hidden)
if [[ "$SYSTEM" == Darwin ]]; then
  LIBRARY_NAME=libduckdb.dylib
  LINK_STRIP_FLAGS=(-Wl,-dead_strip)
  PLATFORM_FLAGS=()
fi
LIB=rust
BUILD=1
FILES=()
CATCH_ARGS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --lib) LIB="$2"; shift 2 ;;
    --no-build) BUILD=0; shift ;;
    --) shift; CATCH_ARGS=("$@"); break ;;
    *) FILES+=("$1"); shift ;;
  esac
done

# DuckDB's C API test files. Skipped files and why:
#   test_starting_database.cpp, test_without_disabled_functions.cpp
#       use the C++ API (DuckDB, Connection, DBConfig) beside the C API.
TESTS=(
  test_capi.cpp test_capi_prepared.cpp test_capi_pending.cpp test_capi_streaming.cpp
  test_capi_values.cpp test_capi_complex_types.cpp test_capi_data_chunk.cpp test_capi_vector.cpp
  test_capi_appender.cpp test_capi_append_data_chunk.cpp test_capi_table_description.cpp
  test_capi_to_decimal.cpp test_capi_extract.cpp test_capi_any_invalid_type.cpp test_capi_website.cpp
  capi_scalar_functions.cpp capi_aggregate_functions.cpp capi_table_functions.cpp
  capi_copy_function.cpp capi_custom_type.cpp capi_file_system.cpp capi_catalog.cpp
  capi_config_options.cpp test_capi_replacement_scan.cpp test_capi_logging.cpp
  test_capi_profiling.cpp test_capi_arrow.cpp test_capi_geometry.cpp test_capi_variant.cpp
  test_capi_instance_cache.cpp
)
[[ ${#FILES[@]} -eq 0 ]] && FILES=("${TESTS[@]}")

# Test cases (and `SECTION:` sections) that use the C++ API, removed from the copies of
# their files that are built (strip_cases.py); linking with -dead_strip drops the helpers
# only they call:
EXCLUDED=(
  # creates the database through duckdb::DuckDB and reads DBConfig internals
  "Test C API config"
  # casts duckdb_connection to duckdb::Connection to create a type through the C++ API
  "Logical types with aliases"
  # cast duckdb_vector to duckdb::Vector to check the values
  "Test DataChunk populate ListVector in C API"
  "Test DataChunk populate ArrayVector in C API"
  # reads the statements of duckdb_extracted_statements through ExtractStatementsWrapper
  "Test extract statements for \`ALTER TABLE ... ADD COLUMN ... DEFAULT\` in C API"
  # builds its table from the C++ test_all_types() type list (TestAllTypesFun)
  "Test the appender with parallel appends and multiple data types in the C API"
  # lists the test directory through duckdb::FileSystem
  "Test the database instance cache with case-insensitive FS"
  # builds its Arrow input with the C++ ArrowConverter and ArrowAppender
  "SECTION:test scan"
)

SELECTED_LIBRARY_DIR="${RUSTDB_CAPI_LIBRARY_DIR:-}"
if [[ -n "$SELECTED_LIBRARY_DIR" ]]; then
  if [[ $BUILD -ne 0 ]]; then
    echo "RUSTDB_CAPI_LIBRARY_DIR requires --no-build" >&2
    exit 2
  fi
  LIBDIR="$(cd "$SELECTED_LIBRARY_DIR" && pwd -P)"
  [[ -f "$LIBDIR/$LIBRARY_NAME" ]] || { echo "No $LIBRARY_NAME in $LIBDIR" >&2; exit 2; }
  if [[ "$(uname)" == Darwin ]]; then
    codesign --verify --strict "$LIBDIR/libduckdb.dylib"
  fi
elif [[ "$LIB" == rust ]]; then
  LIBDIR="$ROOT/target/release"
  if [[ $BUILD -eq 1 ]]; then
    cargo build --release -p rustdb-capi --manifest-path "$ROOT/Cargo.toml"
  fi
  "$ROOT/scripts/sign.sh" "$LIBDIR/$LIBRARY_NAME"
else
  LIBDIR="$REF/build/release/src"
fi

OUT="${RUSTDB_CAPI_TEST_OUTPUT_DIR:-$ROOT/target/capi-tests}"
OBJ="$OUT/obj"
BIN="${RUSTDB_CAPI_TEST_BIN_DIR:-$OUT/$LIB}"
INC="$OUT/include"
mkdir -p "$OBJ" "$BIN" "$INC"
# capi_tester.hpp includes "test_helpers.hpp"; a copy outside test/include makes that
# resolve to the shim instead of the C++ helpers next to it.
cp "$REF/test/include/capi_tester.hpp" "$INC/capi_tester.hpp"
FLAGS=(-std=c++17 -O1 -g -w -I "$ROOT/scripts/capi-tests/shim" -I "$INC" -I "$REF/src/include"
  -I "$REF/third_party/catch" ${PLATFORM_FLAGS[@]+"${PLATFORM_FLAGS[@]}"})
LINK_HEADER_FLAGS=()
if [[ "$(uname)" == Darwin ]]; then
  LINK_HEADER_FLAGS=(-Wl,-headerpad_max_install_names)
fi

compile() { # source object
  if [[ ! -f "$2" || "$1" -nt "$2" || "$ROOT/scripts/capi-tests/shim/test_helpers.hpp" -nt "$2" ]]; then
    "$CXX" "${FLAGS[@]}" -c "$1" -o "$2"
  fi
}
compile "$ROOT/scripts/capi-tests/shim/main.cpp" "$OBJ/main.o"
compile "$REF/test/helpers/capi_tester.cpp" "$OBJ/capi_tester.o"

SRC="$OUT/src"
mkdir -p "$SRC"

status=0
for file in "${FILES[@]}"; do
  name="${file%.cpp}"
  python3 -I "$ROOT/scripts/capi-tests/strip_cases.py" "$REF/test/api/capi/$file" "$SRC/$file.new" \
    "${EXCLUDED[@]}"
  # Keep the copy's timestamp unless it changed, so the object is not rebuilt.
  if cmp -s "$SRC/$file.new" "$SRC/$file"; then rm "$SRC/$file.new"; else mv "$SRC/$file.new" "$SRC/$file"; fi
  if ! compile "$SRC/$file" "$OBJ/$name.o" 2>"$BIN/$name.build.log"; then
    printf '%-36s does not compile (see %s)\n' "$name" "$BIN/$name.build.log"
    status=1
    continue
  fi
  if ! "$CXX" "$OBJ/main.o" "$OBJ/capi_tester.o" "$OBJ/$name.o" -L "$LIBDIR" -lduckdb \
    -Wl,-rpath,"$LIBDIR" "${LINK_STRIP_FLAGS[@]}" ${PLATFORM_FLAGS[@]+"${PLATFORM_FLAGS[@]}"} ${LINK_HEADER_FLAGS[@]+"${LINK_HEADER_FLAGS[@]}"} \
    -o "$BIN/$name" 2>"$BIN/$name.link.log"; then
    missing="$(grep -c '^  "_' "$BIN/$name.link.log" || true)"
    printf '%-36s does not link: %s missing symbols (see %s)\n' "$name" "$missing" "$BIN/$name.link.log"
    status=1
    continue
  fi
  if [[ -n "$SELECTED_LIBRARY_DIR" && "$(uname)" == Darwin ]]; then
    # Rust's default dylib install name can be an absolute mutable build path.
    # Pin this test executable to the selected image without modifying that image.
    install_name="$(otool -D "$LIBDIR/libduckdb.dylib" | tail -n 1)"
    install_name_tool -change "$install_name" "$LIBDIR/libduckdb.dylib" "$BIN/$name"
  fi
  "$ROOT/scripts/sign.sh" "$BIN/$name"
  printf '%-36s ' "$name"
  log="$BIN/$name.log"
  RUN_COMMAND=("$BIN/$name")
  if [[ -n "$SELECTED_LIBRARY_DIR" && "$(uname)" == Darwin ]]; then
    # Preserve the loader's actual selected path, not just the executable's load command.
    RUN_COMMAND=(env DYLD_PRINT_LIBRARIES=1 "$BIN/$name")
  fi
  if (cd "$REF" && "${RUN_COMMAND[@]}" ${CATCH_ARGS[@]+"${CATCH_ARGS[@]}"}) >"$log" 2>&1; then
    tail -n 2 "$log" | grep -E "passed|test cases" | tr '\n' ' '
    echo
  else
    status=1
    summary="$(grep -E "^test cases" "$log" | tail -n 1 || true)"
    echo "${summary:-crashed (see $log)}"
  fi
done
exit $status
