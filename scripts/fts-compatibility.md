# FTS database interoperability verification

`fts-compatibility.py` verifies persisted FTS indexes using the official C++ DuckDB
1.5.6 CLI and a built/signed roastedDuck CLI. It supplements the unchanged upstream
SQL suite and the separate all-algorithm Snowball differential test; it does not
claim those gates passed.

Run serially, with a **new** output directory on the workspace's external artifact
storage:

```sh
python3 scripts/fts-compatibility.py \
  --oracle-cli .context/artifacts/vss-fts/oracle/official/duckdb \
  --rust-cli target/release/roastedDuck \
  --fts-extension .context/artifacts/vss-fts/oracle/fts.duckdb_extension \
  --upstream-source .context/artifacts/vss-fts/fts-source \
  --out .context/artifacts/vss-fts/fts-verification/interop-001
```

Use the actual CLI build path for `--rust-cli`; the example is not a build command.
`--oracle-only` omits the Rust CLI and runs both roles using C++, explicitly labeled
`ORACLE_FIXTURE_SELF_TEST`. Identical oracle/Rust executables are rejected in normal
mode. The harness enforces the oracle version, extension footer pin, and pinned
FTS source hashes. It records executable hashes, original test-file hashes, every
SQL payload, exit code, raw stdout/stderr, and structured output in
`verification.json`. Mismatches or failed invocations return nonzero; no numeric,
Unicode, or macro-text normalization is applied. CLI JSON formatting is parsed,
but SQL strings and numerical token spellings are preserved.

Each engine creates a database with default FTS and a second schema using custom
stopwords, no stemming, and disabled lowercasing/accent removal. The creator closes
and reopens the checkpointed file. The other engine then reads it with explicit
load, explicitly enabled autoload with network installation disabled, and `ATTACH` into a fresh
connection. Comparisons include all six FTS tables, column types, persisted macro
definitions, tokenization, and BM25 ranks/scores with field filtering, parameter
overrides, conjunction, repeated terms, accents, empty/NULL input, and no match.
Read-only checks must leave the file hash unchanged. On a copy, the reader inserts
data, rebuilds the index, checkpoints it, and passes it back to the original engine.
It then drops both indexes and verifies their absence after another reopen.

The isolated oracle extension directory contains only the pinned FTS artifact;
the Rust path executes `LOAD fts`, never the C++ artifact. Temporary files are
contained in the output directory. Original fixtures are retained after rebuild/drop.

## Unchanged upstream SQL suite

The pin has ten files under `test/sql/fts`, including `test_indexing.test_slow` and
`test_stemmer.test_slow`. They need no external CSV/Parquet fixtures; the attach test
uses the harness's `__TEST_DIR__`. Preserve `skip_reload`, `no_alternative_verify`
(including upstream spelling variants), and all original statements/results.
The existing SLT runner's `require fts` delegates to `Database::load_extension` in
the Rust backend, so native FTS registration is sufficient for discovery. Pass
`--require fts` to turn a missing implementation into failure rather than a skip:

```sh
target/release/slt run --engine rust \
  --root .context/artifacts/vss-fts/fts-source \
  --require fts --jobs 1 --timeout-secs 300 \
  --out .context/artifacts/vss-fts/fts-verification/upstream-rust.jsonl \
  test/sql/fts
```

Build/sign the runner before executing it. The C++ SLT backend uses its native
oracle shim, not the CLI. Its standard helper checks only statically linked
extensions. For FTS/VSS, the oracle shim can instead use an explicit
`SLT_ORACLE_EXTENSION_DIRECTORY` containing the pinned `fts.duckdb_extension`
and/or `vss.duckdb_extension`. It requires an existing absolute directory and
executes ordinary `LOAD`, preserving signature/version validation and reporting
load failures as errors. Other extensions and calls without the environment
variable retain their previous behavior. This is oracle tooling only.

Authenticate the shim's actual linked DuckDB 1.5.6 image before using results.
The official macOS universal library is staged at
`.context/artifacts/vss-fts/fts-verification/official-library/src/libduckdb.dylib`;
pass its parent build directory to the shim's CMake configuration. The historical
source build labeled `v0.0.1` is not the official-extension oracle. A reference run
uses `--engine cpp --shim /absolute/path/to/libslt_oracle.dylib --require fts`,
the same upstream test root/path, and the explicit extension directory environment.
`scripts/conformance.sh` remains required after every merge; this supplemental
harness does not replace its core ratchet or full-corpus obligations.
