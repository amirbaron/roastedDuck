#!/usr/bin/env bash
# Signs the release binaries with the local `rustdb-dev` code-signing identity and a stable
# identifier per binary, so macOS keeps the permissions granted to them (e.g. access to an
# external volume) across rebuilds. The linker's ad-hoc signature changes with every build.
#
# Usage: scripts/sign.sh [binary...]   (default: every rustdb binary in target/release)
# Without the identity (see ~/.config/rustdb-codesign) it warns and does nothing.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RELEASE="${CARGO_TARGET_DIR:-$ROOT/target}/release"
IDENTITY="${RUSTDB_CODESIGN_IDENTITY:-rustdb-dev}"

if [[ "$(uname)" != "Darwin" ]]; then
  exit 0
fi
if ! security find-identity -p codesigning 2>/dev/null | grep -q "\"$IDENTITY\""; then
  echo "sign.sh: no code-signing identity \"$IDENTITY\"; binaries stay ad-hoc signed" >&2
  exit 0
fi

if [[ $# -gt 0 ]]; then
  BINARIES=("$@")
else
  BINARIES=()
  for name in slt rustdb bench bench-driver libduckdb.dylib; do
    [[ -f "$RELEASE/$name" ]] && BINARIES+=("$RELEASE/$name")
  done
fi

for binary in "${BINARIES[@]}"; do
  name="$(basename "$binary")"
  codesign --force --sign "$IDENTITY" --identifier "dev.rustdb.${name%.dylib}" "$binary" 2>/dev/null
done
