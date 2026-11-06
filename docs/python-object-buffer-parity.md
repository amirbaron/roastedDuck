# Python object-buffer scan checkpoint — 2026-10-29

The scanner now reads ordinary NumPy object buffers through PEP3118 instead of
calling Python indexing for every string. It retains the exporter and takes an
owning reference to each object before conversion. Custom conversions release the
buffer view first. NumPy subclasses retain the indexing path, so custom buffer
protocols cannot replace their values. No table cache or additional row snapshot is
introduced.

## Signed component and checks

The accepted scanner component is Python source3, plus the permanent regression
script in source4 (all compiled inputs unchanged). Signed wheel SHA256:
`8fadbda7c6d97630baddc2ac1e9f35e4793dcbb870854fc87035d9053e94691c`.
Native extension SHA256:
`2201dc1b1f296183dcf83eb8e7024829f7b7a2a2092d057cea190713bc8304e2`.
Evidence: `.context/artifacts/python-object-buffer-20261029/` on the dedicated SSD.

- Clippy for all Python targets passes with warnings denied.
- All17 boundary outcomes match C++ in both Arrow19/21 environments, including
  zero/non-unit strides, existing negative-stride errors, Unicode/NUL/long strings,
  NULLs, reentrant conversions and a subclass overriding `__buffer__`.
- Both full fast matrices match every C++ ID, verdict and skip reason:3241/3492
  records (3205/3455 ordinary passes,27/28 skips,eight xfails,one shared xpass).
- All300 frozen upstream test/data hashes, signatures and actual sole Rust image
  identities verify. Both environments retain385 runtime exports,547 C API exports,
  zero ADBC test hooks and all seven pinned installed stubs.
- The permanent script `scripts/python-tests/object-buffer-boundaries.py` reproduces
  the same17 outcomes against both engines/environments after promotion.
- Full supplemental refresh passes:62 Arrow producer controls per environment,
  seven non-fast files71pass/eight exact shared skips, strict positive/negative typing
  clients, installed ADBC handoffs, eight15-column checkpoint/nonempty-WAL mutation/
  readback journeys, and24 encrypted-Parquet cases per environment with exact error
  comparisons. All child/driver exits are preserved and successful.

`core-input-equivalence/verification.json` verifies that this Python-only change
and regression script do not enter the dependency closure of the native C API,
shell or SQL runner. Their sealed images and prior native/Go/Swift/performance
proofs remain exact artifact controls. Subsequent Parquet core work is separate
and requires a fresh integrated release; this checkpoint does not transfer these
results to a later rebuilt engine.

## Coupled speed and memory

Three paired/interleaved fresh processes per storage kind, one million rows, one
engine thread. All count/length assertions pass. Values are separate medians; RSS
is whole-process decimal MB. The before wheel is the preserved final-native-era
Python release, not a development/release comparison.

| Input | Rust before ms / MB | Rust after ms / MB | C++ current ms / MB |
|---|---:|---:|---:|
| Pandas Python strings |60.71 /139.46|43.39 /140.10|35.76 /142.28|
| Pandas Arrow strings |112.97 /212.43|93.20 /212.57|57.76 /197.97|

Rust is roughly29%/17% faster with no material RSS change. Remaining1.21x/1.61x
execution ratios and the Arrow string RSS excess remain individual outliers;
this component improvement does not silently accept them or require exact speed
parity. Final TPC-H/ClickBench policy is separate and remains unchanged.
Raw samples: `python-contract-3/verification.json` and per-sample JSON/logs.

## Rejected candidates and harness corrections

Candidate1 built successfully but was never executed; review added the callback
buffer-release boundary before testing. Candidate2 passed all6733 fast-suite
records but a separate subclass control showed a real regression: C++ and the
preserved Rust wheel read original values without invoking `__buffer__`, whereas
candidate2 called the override and read replacement values. Candidate2 is explicitly
rejected. Candidate3 restricts the fast path to exact NumPy arrays and passes that
permanent regression. The original failed/counterexample receipts are retained.

The initial boundary test wrongly expected negative-stride arrays to succeed.
Diagnostics confirmed the frozen C++ client and existing Rust client reject them
with the same unsigned-stride cast error. Corrected controls assert that exact
existing rejection; no product behavior or upstream expectation was weakened.
The first candidate2 build driver also stopped at its cwd assertion before building;
its actual successful build ran from the Python package directory.

The full goal remains active: the later Parquet schema-evolution/remapping candidate,
remaining core/accounting audits, release-platform scope and final integrated
artifact verification are not closed by this scanner component.
