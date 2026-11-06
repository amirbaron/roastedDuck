# Native ARM64 SQL reference and current corpus probe

The old Mac C++ comparator reported `v0.0.1` and was unsuitable for final DuckDB
1.5.6 comparisons. A clean native ARM64 reference now builds from the preserved
1.5.6 source with explicit release identity. Build932 completed all695 Ninja
steps, with all14,535 source files unchanged before/after. It includes the shell,
unittest executable and seven bundled extensions. This is a source-built C++
comparator with dummy commit ID `0123456789`, not an official binary distribution.

Evidence root: `.context/artifacts/platform-matrix-20261029/` on the external SSD.
Reference934 and current-source SQL tools937/948 are frozen and authenticated by
whole-file CMS using the pinned development certificate. Engine, shell, unittest,
shim and SQL runner pass normal strict macOS codesigning. DuckDB test extension
trailers are preserved byte-for-byte: each native Mach-O image is verified
separately and the complete file, including its trailer, is covered by CMS.
A format probe933 confirms why whole-file Mach-O validation rejects these required
trailers; they are not removed or rewritten for the corpus.

Identity940 verifies:

- The shared library and shell report `v1.5.6`; the SQL shim reports ABI1 and the
  frozen reference build directory.
- dyld's actual loaded-image list contains only the frozen reference library and
  the expected shim. Logging via `DYLD_PRINT_LIBRARIES` was absent in probe938;
  the replacement checks the process's actual images instead.
- The official authenticated Mac HTTPFS package590 loads with unsigned extensions
  disabled, and reports loaded=true. This executes the real C++ provider.
- A native shell query returns version1.5.6 and the expected arithmetic result.

## Upstream secret SQL

Both engines pass all nine original secret-related SQL files in944, with no
skips: secret expressions, filesystem logging, HuggingFace/autoload errors,
restart without client context, historical HTTP secret compatibility, disabled
local filesystem, external-access policy and autocomplete. Historical secret
fixtures are explicitly enabled and receive0600 permissions in the owned copy.
All5,692 guarded SQL/data input hashes remain unchanged. No SQL expectations or
ratchets were edited. Fixture copy942 followed an intentional dangling symlink;
fresh copy943 preserves symlinks and their targets, leaving the failed setup
receipt intact.

These nine files are a scoped upstream check, not a refreshed full-corpus count.
The persistent-format and transaction fixes remain documented in their own
component reports.

## Full original corpus in progress

A fresh signed SQL runner948 adds an optional `SLT_MEMORY_LIMIT` test-harness
setting applied equally to both backends before opening each database. Production
engine defaults and the lower-memory cache policy are unchanged. Smoke951 parses
and verifies both actual verdicts, confirming the requested5.6GiB profile. The
canonical displayed string is `5.5 GiB` on both engines. Probe950 used the wrong
expected display string and records two fail verdicts even though `slt exec`
returned transport exit0; it is retained, not counted as passing evidence.

Run952 executes the exact original4,870 IDs from the canonical manifest, C++ then
Rust, with separate pristine4870-test/1522-data roots, jobs1, normal engine threads,
300seconds/file, UTC and the same default5.6GiB memory limit. Tests may set their
own limits. The host has48GiB RAM and14 logical CPUs. A safety guard kills a test
worker exceeding7GiB sampled RSS or128MiB worker output; such files remain explicit
nonpasses with resource receipts. RSS sampling is every0.5seconds, not exact peak
RSS and not final performance acceptance. Every raw result/error/log remains on
SSD, and fixture/signature/source checks run before and after each engine.

The collection is not yet accepted. Review every verdict, resource termination,
skip reason and individual file outlier after both engines finish. Whole-file
corpus timings are not individual-query TPC-H/ClickBench speed/RSS acceptance.

## Remaining release requirements

The whole port goal remains active and incomplete. The reference and SQL runner
are development verification tools. Final common-source signed Rust release
artifacts still require the full SQL/shell, C API/ADBC, Python/Go/Swift, required
platform and cross-engine database/WAL gates, plus native coupled speed/memory
measurements and documented acceptance with individual query outliers. Required
platforms remain macOS ARM64, iOS ARM64 devices/simulators and Linux ARM64/x86_64;
Mac Intel and additional language clients remain outside scope.
