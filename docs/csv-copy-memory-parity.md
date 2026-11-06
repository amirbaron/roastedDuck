# CSV ownership and the large-gzip memory outlier

The original development corpus exposed a serious individual RSS outlier in
`test/sql/copy/csv/test_copy_gzip_large_block.test_slow`. It writes2048 strings of
2100000 bytes each, then checks2048 rows and4300800000 total string bytes after
readback. Preserve that original fixture and its original resource-kill receipt;
a later diagnostic does not replace the canonical verdict.

## Cause and correction

Two independent copies doubled live payload memory:

- The regular writer encoded a chunk, copied it into `pending`, and immediately
  flushed `pending`. When no earlier rows are pending and the block already reaches
  FLUSH_SIZE, write the existing encoded block directly. Flush/newline boundaries
  are identical, including file-size accounting. This retains the single write
  larger than UINT32_MAX that the original gzip test exercises.
- The reader retained immutable input buffers while copying their strings into
  vector heaps. Unescaped strings wholly contained in the current pinned buffer
  now reference it. Each vector retains that buffer's owner once per buffer, across
  cloning, slicing, scanner reset and buffer-manager release. Temporary decoded,
  escaped, cross-buffer and otherwise noncontained strings still copy. The
  `allocate` flag alone is insufficient: containment is checked before referencing.

No query/object/table cache, release_on_unpin, buffer-manager default, COPY batch
throttling or Parquet memory-policy input changed.

## Signed development verification

Strict CSV all-target Clippy993 passes. The first invocation992 failed only because
of a missing collection type annotation in the new test; its receipt is retained.
Fresh build994 succeeded before signing shared995 and SQL tools996. Fresh test
build997 produced six signed executables998. Runtime999 passes all seven existing
and new CSV cases. The new lifetime case retains5000 rows across scanner resets,
multiple input buffers, quoted/escaped fields, cross-buffer values and Unicode,
then destroys the scan and removes its file before checking reversed slices and
clones. It checks result lifetime, not a mirror of the allocation implementation.

Current signed development shared library SHA256:
`8bb628f0d441a84ee001c00cf7ac0cef3df123709d0eed10ed1101a1b6993c01`.
Whole-file CMS manifests are pinned to the existing rustdb-dev certificate; native
images also pass strict normal codesign verification before execution. These are
Mac ARM64 development components, not final release artifacts.

## Separate COPY and readback measurements

Independent public-CAPI processes,14 threads,16GiB engine memory limit,20GiB
sampled RSS guard. The C++ comparator is the source-built native ARM64 DuckDB1.5.6
Release bundle934, not an official binary distribution. All readback answers pass.
The original and corrected Rust gzip files are byte-identical (991 comparison;
1000 byte-equivalence.json records the final comparison).

| Phase | C++ seconds | Rust before seconds | Rust after seconds | C++ sampled RSS KiB | Rust before KiB | Rust after KiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| COPY |16.151|4.067|3.716|4250992|8456624|4256816|
| Readback |141.413|91.246|88.215|4477376|8684384|4606224|

Writer-only intermediate990/991 measures3.770s and4256384KiB. It isolates the
writer correction before the separate reader correction. Corrected995/1000 reduces
COPY sampled RSS49.7% and readback46.96% against976/988; times also improve in these
single diagnostic trials. Corrected readback remains about2.88% above C++ sampled
RSS. Report these individual results; averages must not conceal their original gap.

RSS is sampled every0.5 seconds, so these values are observations, not exact peaks.
Each process also records macOS ru_maxrss in bytes. Timing is a single diagnostic
trial and compares development Rust against release C++; it does not establish
optimized-release speed ratios or accept the full TPC-H/ClickBench baseline.

## COPY subtree and remaining acceptance

The complete539-file original COPY subtree comparison1001 is complete: BOTH
engines518pass/21skip, zero failures, crashes, timeouts or resource kills. Every
verdict and exact skip reason matches. Original fixture, source and signature
checks pass before/after. The21 skips are19 HTTPFS requirements and2 Windows
cases; they are not encrypted database/WAL coverage. Signed SQL996 and C++934
used equal16GiB engine/20GiB sampled-RSS/900-second bounds, serial jobs.

Current public C API1005 passes30files/155cases/723862assertions with strict native
signatures and actual loaded-library checks. There are547 duckdb_ exports and no
private ADBC test hooks in the shared image. This does not replace ADBC/client
release verification.

Individual whole-file timing outliers remain visible:

| Original file | C++ seconds | Development Rust seconds |
| --- | ---: | ---: |
| csv/tpch_csv_sf01.test_slow |3.495|18.292|
| partitioned/partitioned_write_tpch.test_slow |3.609|14.354|
| file_size_bytes_large.test_slow |2.333|8.414|
| partitioned/hive_partitioned_write.test_slow |3.113|9.634|
| csv/test_multiple_big_compressed_csvs.test_slow |63.694|100.741|

All file ratios above1.25, without an absolute-time filter, are retained in
`csv-component-audit-1004/all-file-time-outliers.json`, including available
sampled RSS and explicit nulls for workers missed by sampling. These are whole
files, including setup, not query timings or optimized-release regressions.
Historical source953 development times for TPC-H CSV and multiple compressed
CSVs were19.489s and107.018s respectively; those are separate5.6GiB-profile runs,
not controlled before/after optimization trials. Preserve all original results.

The same-original-budget regression1006 PASSES the exact original gzip file on
signed996 Rust:96187ms and4580560KiB sampled peak RSS, requested5.6GiB engine
budget/7GiB RSS guard/300-second bound. No resource kill occurred; all final
source/fixture/signature guards pass. The original952 kill is retained separately.
Audit1004 verifies all1467 guarded project inputs, signatures,539 exact verdicts
and skip reasons, seven CSV tests and the full155-case C API result. It records
all27 file-time ratios above1.25. Source1003 archives the current build inputs;
final candidate/manifest identities and finished owned jobs are linked in the audit.

All evidence is under the dedicated external SSD workspace directory:
`.context/artifacts/platform-matrix-20261029/`, specifically988 through1006.
The full goal remains active: final same-source signed SQL/shell/C API/ADBC,
Python/Go/Swift and platform matrices, cross-engine checkpoint/nonempty/torn/
encrypted DB/WAL journeys and native coupled TPC-H/ClickBench measurements remain.
Retain the accepted lower-memory policy and the separately accepted throttled COPY
exception; these CSV diagnostics do not substitute for final performance acceptance.
