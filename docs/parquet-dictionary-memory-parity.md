# Reusable Parquet dictionaries under accounted COPY memory

Verified component checkpoint2026-10-29. The whole port goal remains active.
The original constant_dictionary_vector_parquet.test and slow variant now pass
at their unchanged10MB/100MB limits, with temp_directory=NULL and normal engine
threads. Raw-batch accounting and the accepted COPY speed–memory policy remain.

The693-file scan/join audit exposed these regressions in the earlier buffer-managed
COPY change. The retained post-COPY/pre-codec signed runner also fails both, while
the earlier import-source5 full corpus passed. Rust's Parquet decoder returned an
ordinary sliced vector; ColumnDataCollection could not identify its dictionary
and expanded long strings into a heap per row. C++ DictionaryDecoder instead
creates reusable dictionary metadata for all entries plus the optional NULL slot.

The decoder now shares an Arc<Vector> and Arc<DictionaryInfo> for each dictionary,
resets both at row-group boundaries, and returns a storage_dictionary vector.
Its size includes the NULL slot like C++. Partial reads appended at nonzero
offsets still copy into flat vectors like C++. Existing hash caching retains its
per-dictionary identity; this change adds no table cache or raised COPY budgets.

| Current-source check | Result |
|---|---|
| Source4 snapshot |965 overlay entries/964 guarded inputs |
| Workspace/all-target Clippy | Pass, warnings denied |
| Signed SDK debug/release | Four tests each pass: original budgets, identity/NULL slot/slices/reader lifetime, changing nullable row-group values/hashes |
| Signed broader Parquet/row/types |202pass, two existing optional Snappy comparison/benchmark tools ignored |
| Signed native build | Actual parent exit0; frozen CLI/library/SQL runner;547 C API exports, zero ADBC test hooks |
| Affected693-file SQL rerun |684pass/nine exact C++ skips; zero failures/crashes/timeouts/verdict regressions |
| Full C API |30files/155cases/775418assertions pass; actual selected library checked |
| Independent Parquet journeys |12 files written across C++/Rust and12 opposite-engine reads; original constant budgets/no spill, complete payload/key coverage and sums/hashes |

Additional nullable/repartition fixtures use the reference normal budget. C++ also
rejects the exploratory20MB constraint; it is not an original regression limit.
The first lifetime fixture inspected a result collector's flat vector; the corrected
fixture reads directly at the decoder boundary and verifies shared dictionary
identity through slicing and reader destruction. An initial2GB EXCEPT ALL checker
also exhausted C++ memory on expanded long-string relations. The replacement checks
every payload against its deterministic expression plus exact unique-key count/range
coverage, proving full dataset contents without expanding that checker. All failed
attempts and original expectations remain retained separately.

Current library SHA256:
`abd6b36c486f2bf144830c88f85ae25b445832375d6d941d519c3ed1e3fdbce7`.
CLI: `40653e5af71d44bfdf72e2c64c32b63fa8a51b495b74f4b24e1f631f55f91898`.
SQL runner: `65df208cbffd6da63393838df94bb5c7a3408fb1107a7105aeabaa243a09bb9c`.

External evidence: `.context/artifacts/parquet-dictionary-memory-20261029/`:
`source-4/manifest.json`, `clippy-4-terminal.json`, `sdk-debug-4/verification.json`,
`sdk-release-1/verification.json`, `affected-debug-1/verification.json`,
`native-images-1.json`, `affected-sql-1/verification.json`,
`capi-suite-1/verification.json`, `engine-2/verification.json`, `cpp-controls-1/`.
Before regressions remain under `arrow-producer-parity-20261029/{affected-sql-1/,
copy-dictionary-attribution-1/}`. No ratchet or original test limit changed.

The installed Python wheel and its6733 matching fast-suite records still belong to
the preceding Arrow source7 image; final client/artifact refresh remains required.
Next audit expression-index persistence/WAL, remaining storage/optimizer contracts,
then complete full SQL/shell/C API/ADBC/Python/Go/Swift/persistence/WAL and coupled
TPC-H/ClickBench speed/RSS measurements with individual outliers. No exact performance
parity or additional language clients are required.
