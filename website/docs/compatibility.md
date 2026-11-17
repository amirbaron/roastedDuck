# Compatibility and preview status

roastedDuck targets **DuckDB 1.5.6**, with matching version-family branch naming.
The current `1.5.6.post4` revision bundles roastedDuck's implementations of
DuckDB's httpfs, aws, azure, icu, postgres_scanner, FTS and VSS extensions while
retaining the 1.5.6 engine compatibility identifiers.
It is an experimental public preview. Evaluate your own workload and retain
backups before using it with important data.

## Supported scope

| Surface | Recorded candidate evidence | Boundary |
| --- | --- | --- |
| macOS arm64 | Native SQL, shell, C API/ADBC, Python, Go, Swift | Other Mac architectures are outside the preview |
| Linux arm64 | SQL, shell, C API/ADBC, Python, Go, Swift | No Linux benchmark headline |
| Linux x86_64 | Same functional target | Current package/client CI runs natively; historical full-corpus qualification used translation |
| Python | Normal CPython 3.10–3.14, 15 ABI/platform cells | Fresh source-built wheel qualification and ordinary PyPI installs; matching nonpasses retained |
| iOS arm64 simulator | 133 Swift cases and four encrypted checkpoint/reopen profiles | Simulator evidence only |
| iOS arm64 device | Signed build | Physical execution/provisioning unverified |
| Windows, Intel Mac, other clients | Outside release scope | No support claim |

## Current post2 qualification

The [post2 producer](https://github.com/amirbaron/roastedDuck/actions/runs/42760325168)
passed native-client and encryption gates on all three platforms and all 15 paired
Python ABI cells. Each Python cell recorded 3,455 regular passes, 28 skips,
8 expected failures and 1 unexpected pass, with matching C++/Rust verdicts.
The [official-index installation run](https://github.com/amirbaron/roastedDuck/actions/runs/42785009114)
verified all 15 published wheels in fresh environments.

## Recorded November 6 qualification

| Gate | Original result |
| --- | --- |
| macOS SQL corpus | 4,826 PASS / 30 SKIP / 14 raw FAIL across 4,870 files |
| Linux SQL corpus, each architecture | 4,828 PASS / 28 SKIP / 14 raw FAIL |
| Workspace harnesses | 1,089 PASS; zero failures; seven ignores, two separately exercised |
| C API | 30 harnesses / 155 public cases per engine/target |
| ADBC | 64 cases / 3,040 assertions per engine/target |
| Python macOS | 30,743 passes / 392 matching nonpasses per engine |
| Python Linux | 61,486 passes / 784 matching nonpasses per engine |
| Selected clients | Go 652 cases; Swift 133 cases per scoped platform/engine |
| Distribution | 300 original payload files and symlink targets preserved after extraction |

These results describe the frozen source2551 candidate, not every future
commit. The original 1.5.6 preview preserved those native bytes. The post1 security
revision and post2 branding/validation revision have their own client, encryption
and installation reports. Neither claims a rerun of the entire historical corpus
or benchmark matrix.

## Bundled extensions

Release 1.5.6.post4 bundles Rust implementations of DuckDB's httpfs, aws, azure,
icu and postgres_scanner extensions; see the [extension guide](extensions.md#what-was-tested)
for the per-extension test results. The differences that remain:

- **Autoloading needs no install.** With `autoload_known_extensions` on, a bundled
  extension loads even when `autoinstall_extension_repository` points at a missing
  repository. Four httpfs autoloading tests expect that install to fail.
- **PostgreSQL connection errors** are worded by roastedDuck's client, for example
  `error connecting to server: Connection refused`, rather than libpq's
  `Unable to connect to Postgres at "<dsn>": connection to server at ...`. A failed
  `ATTACH` makes one connection attempt where DuckDB makes two.
- **Debug verification pragmas:** `PRAGMA enable_verification` runs the regular
  SELECT verifiers; `verify_external`, `verify_fetch_row`, `verify_serializer` and
  `verify_parallelism` are accepted and do nothing.
- **Cloud services** were tested against local fixtures (MinIO, an Azure
  emulator and protocol fixtures, PostgreSQL 17, STS/SSO and metadata services),
  not live AWS, Azure, Google Cloud, Redshift or Aurora accounts.

## Known exceptions

- **14 raw SQL failures:** eleven require private C++ engine-internal extension
  fixtures; three expect an out-of-memory error where Rust succeeds. The latter
  does not prove peak-memory equivalence or exercise later skipped assertions.
- **Shell:** metadata, native frames, and some invalid UTF-8 behavior differ.
  Universal byte-for-byte parity is not claimed.
- **Reference incidents:** original C++ concurrency failures and x86_64 crashes
  remain recorded. Their causes are not established; they are not speedup evidence.
- **Shared skips and expected failures:** retained as nonpasses, including
  Python dependency/version conditions and five ignored diagnostics.
- **Swift:** upstream Sendable/pointer warnings remain. Strict Swift 6 mode is
  not a new compatibility claim.
- **Go:** use `duckdb_use_lib,duckdb_arrow` with the selected Rust library. The
  upstream client's default bundled C++ engine is outside this contract.
- **Performance:** aggregate memory savings do not apply to every query;
  [all measured outliers](benchmarks.md) remain visible.
- **Hardware:** physical iOS execution remains unverified.

The detailed dated engineering report is
[release-known-exceptions.md](https://github.com/amirbaron/roastedDuck/blob/main/docs/release-known-exceptions.md).
Its historical local evidence paths are archival references; the public
[evidence files](benchmarks.md#download-the-evidence) are available separately.
