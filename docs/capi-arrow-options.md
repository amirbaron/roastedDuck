# C API Arrow result settings

The 2026-10-25 signed release checkpoint fixes conversion settings changing after a
result has already been produced. `duckdb_result_get_arrow_options` and
`duckdb_result_arrow_array` now use the result's owned settings. Query, prepared and
pending execution capture them under the same connection lock as execution. Legacy
Arrow execution now also captures settings before releasing that lock. Failed initial
results get the frozen C++ constructor's default properties.

`scripts/capi-tests/arrow_snapshot.py` uses only the public C ABI and Arrow C structs;
it imports no DuckDB Python engine. Three profiles cover timezone, offset width,
string/list views, output versions, decimal widths and lossless UUID metadata across
query/prepared/streaming/pending and both legacy Arrow execution paths. It changes
connection settings, checks schemas and string-array layouts, and converts schemas
again after destroying the owning result. Failed-result option ownership is checked
separately within each profile. The saved previous Rust library mismatches 12 cases;
the rebuilt library matches C++ in **21/21 scenarios**.

The rebuilt shipping library also passes all 30 files in the existing upstream C API
harness: **155 cases / 778,832 assertions** on this run. The harness's documented
C++-internal exclusions remain. The three affected C++ control files pass 12 cases /
11,356 assertions. C API all-target Clippy with warnings denied passes.

An additional exploratory scenario remains an **unaccepted reference discrepancy**:
using a failed result's nonnull Arrow options to convert a VARCHAR schema returns a
C++ INTERNAL error (`Attempting to dereference an optional pointer that is not set`);
Rust returns a valid string schema. Both outcomes are preserved separately. This is
outside the 21 matching scenarios and is not silently accepted as full parity.

Fresh-process, one-thread C ABI microchecks retain timing and peak RSS together:
1000 checked trivial queries take 14.0–14.3 ms before versus 15.8–16.2 ms after.
Warm-repeat whole-process peaks are 36.5–36.6 MB before versus 36.8–37.2 MB after;
the first older-library sample peaks at 45.3 MB and is also retained. Both libraries
are release builds, but the previous shipping library predates other SDK integration
changes, so this comparison does not attribute all overhead to the snapshot fix.
There is no table-cache change or final benchmark acceptance claim.

Evidence is under `.context/artifacts/capi-arrow-snapshot-20261025/`: signed
`before-libduckdb.dylib` and `after-libduckdb.dylib`, build/Clippy logs,
`{cpp,rust-before,rust-after}-3.json`, separate failed-option receipts,
`upstream-summary.json`, full upstream logs and `performance-comparison.json`.
Complete API semantics, ADBC/client package matrices and final core/shell/corpus/
storage/WAL/performance acceptance remain open.
