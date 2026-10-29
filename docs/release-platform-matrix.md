# Required release platforms

Owner-confirmed on2026-10-29: **Apple platforms plus Linux**, then clarified as
**macOS and iOS devices/simulators**. tvOS,watchOS,visionOS and Windows are outside
this release goal. Language clients remain Python,Go,Swift; preserve C API/ADBC.

| Platform | Planned architectures | Applicable artifacts and clients | Current evidence |
|---|---|---|---|
| macOS |arm64,x86_64|CLI,C API/ADBC,Python,Go,Swift package/framework|Earlier signed arm64 component gates only; x86_64 execution available through Rosetta, artifacts not yet verified|
| iOS device |arm64|Swift package/XCFramework and embedded C API engine|Not yet built or verified|
| iOS simulator |arm64,x86_64|Swift package/XCFramework, consumer and engine tests|SDK27.1 installed; iOS26.5 runtime available; no target artifacts verified|
| Linux |arm64,x86_64|CLI,C API/ADBC,Python,Go,Swift native loading and packages|Not yet built or verified|

Client combinations follow their upstream desktop/server or Apple-client surface;
this does not introduce new iOS Python/Go clients beyond C++ DuckDB's client support.
Device-slice compilation/signing, simulator execution and physical-device execution
must be reported distinctly. Emulated/Rosetta performance must be labeled with its
actual conditions, and cannot be presented as native-hardware timing.

Every applicable target still needs artifact identities, loading/install checks,
correctness/client/storage/WAL evidence and recorded signing verification. The Mac
arm64 tests do not prove other targets. Full SQL/shell/client and paired performance
acceptance remains tied to the eventual signed artifacts, with individual outliers.

## Isolated Linux runtime preparation

Docker/Colima/Lima tools exist. The default Docker daemon is unavailable. An unrelated
Colima profile is running; it is not changed or reused for this task. A dedicated
profile plan is prepared under external `platform-matrix-20261029/` but has not been
started. It caps the VM at2vCPUs/8GiB for initial correctness work and does not alter
the host's active Docker context or SSH configuration.

All VM disks, downloaded images, container caches, compiler output, temporary files
and logs must stay under this workspace's dedicated SSD directory. The short `/tmp`
path is only a symlink to that external directory for Unix socket-length limits;
there is no internal-disk artifact fallback. Runtime keys/disks are not guest-mounted.
Only the dedicated work directory is writable; source/reference/benchmark mounts
are read-only.

The [upstream Colima FAQ](https://github.com/abiosoft/colima/blob/main/docs/FAQ.md)
and [configuration source](https://github.com/abiosoft/colima/blob/main/config/files.go)
document separate config/cache/Docker directories and respect for `LIMA_HOME`.
The prepared driver creates these directories before setting their environment
variables, because a missing explicit config directory can fall back to user defaults.

Evidence and the exact start command: `.context/artifacts/platform-matrix-20261029/`.
Do not overlap VM startup or guest builds/tests with another heavy workspace workload.
The current Parquet correctness/performance repair takes priority before platform
execution. No cross-platform completion is claimed by this preparation.
