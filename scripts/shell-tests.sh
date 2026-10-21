#!/usr/bin/env bash
# Runs DuckDB's shell tests (tools/shell/tests, pytest) against a shell binary.
#
# Usage: scripts/shell-tests.sh [--binary rust|cpp|PATH] [--no-build] [pytest arguments...]
#   --binary rust  rustdb's target/release/rustdb (default; built and signed first)
#   --binary cpp   the reference C++ CLI, to get the baseline
#   --binary PATH  any shell binary
#   --no-build     skip `cargo build` (rust only)
# Extra arguments go to pytest (e.g. `-k test_dump` or a test file of tools/shell/tests).
# Env: REF (DuckDB 1.5.6 source root), PYTEST (default: a venv in $TMP_ROOT/shell-venv,
# created on first use), TMP_ROOT (where the venv and the run directory live).
#
# The tests run from a scratch directory with `data` and `test` linking to the DuckDB
# tree, as they read files such as data/csv/... and test/storage/bc/... by relative path,
# and write a few files of their own there.
#
# Tests that pass on the C++ CLI but cannot pass on rustdb, and why:
#   (none; tests needing extensions the reference build lacks skip on both, e.g.
#    test_warning.py needs httpfs, test_http_logging.py is skipped upstream)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REF="${REF:-$ROOT/.context/duckdb-download/duckdb-1.5.6}"
TMP_ROOT="${TMP_ROOT:-/Volumes/amirs ssd/ai context/conductor/duckdb-rust/tmp}"
BINARY=rust
BUILD=1
PYTEST_ARGS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary) BINARY="$2"; shift 2 ;;
    --no-build) BUILD=0; shift ;;
    *) PYTEST_ARGS+=("$1"); shift ;;
  esac
done

case "$BINARY" in
  rust)
    TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
    if [[ $BUILD -eq 1 ]]; then
      cargo build --release -p rustdb-shell --manifest-path "$ROOT/Cargo.toml"
    fi
    SHELL_BINARY="$TARGET/release/rustdb"
    "$ROOT/scripts/sign.sh" "$SHELL_BINARY"
    ;;
  cpp) SHELL_BINARY="$REF/build/release/duckdb" ;;
  *) SHELL_BINARY="$(cd "$(dirname "$BINARY")" && pwd)/$(basename "$BINARY")" ;;
esac

if [[ -z "${PYTEST:-}" ]]; then
  VENV="$TMP_ROOT/shell-venv"
  if [[ ! -x "$VENV/bin/pytest" ]]; then
    python3 -m venv "$VENV"
    "$VENV/bin/pip" install --quiet pytest
  fi
  PYTEST="$VENV/bin/pytest"
fi

RUN_DIR="$TMP_ROOT/shell-tests/run-$(basename "$SHELL_BINARY")-$$"
mkdir -p "$RUN_DIR"
trap 'rm -rf "$RUN_DIR"' EXIT
ln -sfn "$REF/data" "$RUN_DIR/data"
ln -sfn "$REF/test" "$RUN_DIR/test"
# Test files or ids given replace the whole directory.
TESTS=("$REF/tools/shell/tests")
for arg in "${PYTEST_ARGS[@]+"${PYTEST_ARGS[@]}"}"; do
  [[ "$arg" == *.py* ]] && TESTS=()
done
cd "$RUN_DIR"
# The tests import their conftest as `tools.shell.tests.conftest`.
PYTHONPATH="$REF" "$PYTEST" -p no:cacheprovider -q -rfEs \
  --shell-binary "$SHELL_BINARY" "${PYTEST_ARGS[@]+"${PYTEST_ARGS[@]}"}" "${TESTS[@]+"${TESTS[@]}"}"
