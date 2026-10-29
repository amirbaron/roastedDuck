# Native Arrow producer projection and runtime filters

Verified component checkpoint2026-10-29. The whole port goal remains active.
Source7 preserves964 overlay entries/963 build inputs. No table cache was added;
per-execution bind data retains the final static/runtime filter set through scanning.

The controlled baseline matched eight C++ factory cases but omitted runtime join
filters in the ninth. Query answers alone concealed this gap because the hash join
still checked its keys. Generic PhysicalTableScan now retains DynamicTableFilterSet,
merges it with static filters when the source opens, forwards the result through the
function's pushdown hook, retains that bind data for scan callbacks, and renders the
reference Dynamic Filters field in runtime EXPLAIN output.

Signed debug and optimized SDK runs now match18 independent frozen C++ controls:
projection/reordering, filter-only columns, constants, empty scans, aggregates,
combined static/runtime filters, prepared keep/drop/empty/keep-again refresh,
runtime EXPLAIN, sparse128/2048-key builds and a single-key mandatory comparison.
They compare actual producer columns, scan-to-base maps, filter text, produced rows
and independent values. Optional pruning hints are deliberately ignored by both
controlled factories; their delivery is still checked literally. C++ and Rust emit
the same range hints in the sparse controls. Native factory ABIs use each engine's
SDK types; the stable public Arrow C data interface is tested separately.

Workspace/all-target Clippy passes with warnings denied. All46 signed broader
Arrow/execution/optimizer tests pass,0ignored. The shipping build exited0; frozen
CLI/library/SQL runner signatures pass, with547 C API exports and zero ADBC test
hooks. The full30-file/155-case C API suite passes705688assertions, with actual
selected-library loader paths checked.

The Python wheel was built from the same source, signed before installation, and
installed into isolated Arrow19/21 environments. Each environment matches62 C++
controls with independent arithmetic across Table/Dataset/Scanner/Reader inputs,
projections, static/dynamic/sparse/single joins, prepared refresh and runtime profiles.
Both complete pinned fast suites match C++ IDs/verdicts/skip reasons:3241 and3492
records. All300 frozen upstream test/data files and installed native image hashes
are verified before/after. The installed Rust module is the sole engine image.

## Shared legacy public C API limitation

Both frozen C++ and Rust public duckdb_arrow_scan factories ignore projection and
filter parameters while registering the projection-aware scanner. With a raw
three-column C stream, selecting the second/reordered column reads the wrong array
positions, and delegated predicates can be omitted. Eight paired stream controls
match literally, but only three match independent expected SQL values; five shared
incorrect cases are explicitly retained. The current-image public stream/array
matrix has16 literal comparisons, caller-owned stream releases and array-proxy
lifetime checks. This shared legacy behavior is separate from the correct native
projection-aware factories and installed Python adapter results above.

## Broader audit exposed earlier COPY regressions

The693-file scan/join/CSV/Parquet run yields682pass, nine exact C++ skips and two
failures: constant_dictionary_vector_parquet.test and its slow variant run out of
memory during the second COPY at10MB/100MB. Both passed the retained import-source5
full corpus. The retained post-COPY/pre-codec signed image also fails both, so these
regressions predate this Arrow correction. The failed scoped gate remains failed;
no test limit, expectation or ratchet was relaxed. They are the next repair priority.

Frozen C++ Parquet DictionaryDecoder creates reusable dictionary metadata including
the NULL slot. Rust returns an ordinary sliced dictionary, losing the size/identity
information ColumnDataCollection needs to retain compression. The buffer-managed
COPY path exposes this omission. Repair and final verification remain required;
the accepted memory-bound COPY performance tradeoff does not waive these tests.

Current native library SHA256:
`ad0541c58f39e22a49666dd85c2f52a8dcd031e743cd8001ebd2c35f67e5c4d7`.
CLI: `cd5cb0f9277933fbcafd62f4cb02eb2ef2b5e8264cc15b96996c25c499a52522`.
SQL runner: `17c90de00a9fb2a174bd2ffe626a4356d77889caf735e566468bce35134b44f0`.
Python native: `edea9955ae09b78e132bbad931aa1223d0d441f75da5c6d8d8f77440bafeb430`.

External evidence: `.context/artifacts/arrow-producer-parity-20261029/`, including
`source-7/manifest.json`, `reference-6.jsonl`, the normalized profile-field fixture,
`producer-debug-8/verification.json`, `producer-release-1/verification.json`,
`clippy-3-terminal.json`, `affected-debug-1/verification.json`, `native-images-1.json`,
`capi-suite-1/verification.json`, `raw-current-1/verification.json`,
`python-arrow-1/verification.json`, `fast-both-verification.json`, the failed
`affected-sql-1/verification.json` and `copy-dictionary-attribution-1/verification.json`.
Initial C++ tool namespace errors, Cargo PATH/lock-guard failures, an incremental
rustc1.99 fingerprint panic, an import error and a test-only Clippy semicolon remain
separate. Fresh nonincremental checks pass; caches were not deleted.

Full final corpus/shell/selected-client/ADBC/persistence/WAL and coupled speed/RSS
measurements/outliers remain after the core repairs. Python, Go and Swift are the
selected clients; preserve C API/ADBC and the accepted lower-memory baseline/COPY
tradeoff. Current broad SQL acceptance is blocked by the two concrete regressions.
