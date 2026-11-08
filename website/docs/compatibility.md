# Compatibility and preview status

roastedDuck targets **DuckDB 1.5.6**, with the same release version and version-family branch naming.
It is an experimental public preview. Evaluate your own workload and retain
backups before using it with important data.

## Supported scope

| Surface | Recorded candidate evidence | Boundary |
| --- | --- | --- |
| macOS arm64 | Native SQL, shell, C API/ADBC, Python, Go, Swift | Other Mac architectures are outside the preview |
| Linux arm64 | SQL, shell, C API/ADBC, Python, Go, Swift | No Linux benchmark headline |
| Linux x86_64 | Same functional target | Original qualification used translation; native-run CI is a separate check |
| Python | Normal CPython 3.10–3.14, 15 ABI/platform cells | Matching skips/xfails retained; new packaging has separate checks |
| iOS arm64 simulator | 133 Swift cases, typed consumer, WAL journeys | Simulator evidence only |
| iOS arm64 device | Signed build | Physical execution/provisioning unverified |
| Windows, Intel Mac, other clients | Outside release scope | No support claim |

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
commit. Preview packaging preserves its native bytes. Additional installation
checks and CI have their own reports and must not be confused with rerunning
the entire historical qualification.

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
