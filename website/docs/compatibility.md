# Compatibility and preview status

roastedDuck targets **DuckDB 1.5.6**, with matching version-family branch naming.
The current `1.5.6.post2` revision completes the roastedDuck naming and updates
validation tooling while retaining the 1.5.6 engine compatibility identifiers.
It is an experimental public preview. Evaluate your own workload and retain
backups before using it with important data.

## Supported scope

| Surface | Recorded candidate evidence | Boundary |
| --- | --- | --- |
| macOS arm64 | Native SQL, shell, C API/ADBC, Python, Go, Swift | Other Mac architectures are outside the preview |
| Linux arm64 | SQL, shell, C API/ADBC, Python, Go, Swift | No Linux benchmark headline |
| Linux x86_64 | Same functional target | Current package/client CI runs natively; historical full-corpus qualification used translation |
| Python | Normal CPython 3.10–3.14, 15 ABI/platform cells | Fresh source-built wheel qualification and ordinary PyPI installs; matching nonpasses retained |
| iOS arm64 simulator | 133 Swift cases, typed consumer, WAL journeys | Simulator evidence only |
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
