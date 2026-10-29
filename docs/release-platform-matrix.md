# Required release platforms

Owner-confirmed on2026-10-29: **Apple platforms plus Linux**, then clarified as
**macOS and iOS devices/simulators**. tvOS,watchOS,visionOS and Windows are outside
this release goal. Language clients remain Python,Go,Swift; preserve C API/ADBC.

| Platform | Planned architectures | Applicable artifacts and clients | Current evidence |
|---|---|---|---|
| macOS |arm64,x86_64|CLI,C API/ADBC,Python,Go,Swift package/framework|Earlier signed arm64 component gates only; x86_64 execution available through Rosetta, artifacts not yet verified|
| iOS device |arm64|Swift package/XCFramework and embedded C API engine|Not yet built or verified|
| iOS simulator |arm64,x86_64|Swift package/XCFramework, consumer and engine tests|SDK27.1 installed; iOS26.5 runtime available; no target artifacts verified|
| Linux |arm64,x86_64|CLI,C API/ADBC,Python,Go,Swift native loading and packages|Isolated arm64 VM/Docker ready; pinned manylinux builder and Rust portability check in progress. No release/runtime parity pass yet|

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
profile is now running under external `platform-matrix-20261029/`, with its physical
VM/config state at `/Volumes/amirs ssd/dk-50198a764d3b1fdc/`. It caps the VM at2vCPUs/8GiB for initial correctness work and does not alter
the host's active Docker context or SSH configuration.

All VM disks, downloaded images, container caches, compiler output, temporary files
and logs must stay under this workspace's dedicated SSD directory. The first `/tmp`
symlink did not solve Lima's length check because Lima resolves it. The task now
indexes the physically shorter SSD profile directory through `runtime/short-profile`;
there is no internal-disk artifact fallback. Runtime keys/disks are not guest-mounted.
Only the dedicated work directory is writable; source/reference/benchmark mounts
are read-only.

The [upstream Colima FAQ](https://github.com/abiosoft/colima/blob/main/docs/FAQ.md)
and [configuration source](https://github.com/abiosoft/colima/blob/main/config/files.go)
document separate config/cache/Docker directories and respect for `LIMA_HOME`.
The driver creates these directories before setting their environment variables,
because a missing explicit config directory can fall back to user defaults. Lima's
separate fixed macOS cache path was absent; a new link directs it to this task's SSD
cache without moving or overwriting existing cache data.

Evidence and the exact start command: `.context/artifacts/platform-matrix-20261029/`.
Do not overlap VM startup or guest builds/tests with another heavy workspace workload.
The Parquet component passed its479-file affected SQL cohort before runtime startup:
473 passes/six exact C++ skips/zero regressions, all fixture hashes stable. Linux
bootstrap now verifies the Rust installer checksum, pins Rust1.99 and the manylinux
image digest, checks969 guest source hashes and runs Cargo portability checks. This
is infrastructure/compiler preparation, not cross-platform release completion.


## Preserved setup failures and runtime proof

Attempt1 failed on overlapping mounts; the reference source was already inside the
read-only workspace mount. Attempt2 completed the SSD image download but failed
Lima's resolved153-character Unix socket path check. Attempt3 uses a physically
short SSD directory (80-character socket path), and starts successfully. The failed
profiles/files/logs are preserved; other projects' profiles are untouched.

The first Docker probe inherited host TLS settings and reached the plain local
socket with HTTPS. Its output is preserved. The corrected task environment removes
only those inherited Docker TLS/context overrides and targets the task socket.
Runtime proof reports Linux/aarch64,2CPUs,7.74GiB guest memory and Docker29.5.2;
Docker and containerd data both reside on the VM data disk, whose backing file is
on the SSD. Workspace/reference/benchmark mounts remain read-only; only `work/` is
writable. `runtime-verification.json` is explicitly an infrastructure gate.

The arm64 builder is pinned as
`quay.io/pypa/manylinux_2_28_aarch64@sha256:a3e02308c822fb6ce0d12248b0c941817ba5e1a1bc9e7bb5929f29c50eaa40a9`.
Its actual digest/architecture and source/bootstrap identities are recorded under
`manylinux-arm64-image-1/` and `linux-arm64-check-1/`. All new toolchains, caches,
targets and logs go to the SSD work directory. Checks have not yet established
release artifacts, Linux correctness or x86_64 execution.
