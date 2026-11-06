# macOS Intel CPython 3.12 checkpoint

The signed Rust CPython 3.12 wheel matches fresh C++ DuckDB 1.5.6 controls on both
Arrow 19 and Arrow 21 profiles. These tests run under Rosetta on an arm64 Mac;
they do not establish other Python ABIs, native Intel performance or whole-release
acceptance. The previously verified Mac arm64 CPython 3.13 candidate remains a
separate checkpoint.

The current source snapshot is `source-37/manifest.json`. Its changes from source
36 concern Swift tooling/documentation; Rust product sources remain unchanged.
The compiler/interpreter is the existing x86_64 CPython 3.12.9, with PyO3 0.27.2,
Maturin 1.9.6, Rust 1.99.0, full release LTO and one build job. Every generated
wheel, environment, dependency cache, temporary file and log is on the dedicated SSD.

Signed candidate identities:

- Wheel: `cdb1e2497b8a85e84570c38381f4ecdce81c44f3db25b51a382f36896b3af05b`
- Native: `49382d57c81a8680afd4713f6f8d8a6d9f65f1210e682421f1546fb6f0ca08e6`

The native extension is x86_64/macOS, minimum 11.0 and SDK 27.0. Successful build
parent termination and source guards precede signing. The reference's original
wheel SHA256 is `79de3dfa8705b1ba0d59e7e3252e40ff399e0afd12f485502a6c7bf7c2fd809a`,
verified against [PyPI release metadata](https://pypi.org/pypi/duckdb/1.5.6/json).
Only separate copies are re-signed and their wheel RECORD regenerated; the
published original remains intact. Fresh engine environments share engine-neutral
dependencies, with exact installed native hashes and one-engine identity checked.

| Gate | Actual paired result |
|---|---|
| Focused controls, each profile | 11 named-memory, 23 Arrow-string and 17 object-buffer cases match |
| Arrow 19 full fast suite | 3,205 pass, 27 skip, eight xfail, one shared xpass; all 3,241 records match |
| Arrow 21 full fast suite | 3,455 pass, 28 skip, eight xfail, one shared xpass; all 3,492 records match |
| Non-fast selected cohort | 71 pass/eight identical skips; all 79 records match |
| Installed package | Seven exact stubs, 385 expected runtime exports, two positive typing clients and four expected errors in each negative client |
| Installed ADBC | Both Rust profiles pass Python/UDF/Arrow rollback, commit, checkpoint and reopen |
| Typed persistence | Eight bidirectional, 15-column checkpoint/nonempty-WAL append, receiver-mutation and original-readback journeys pass |
| Arrow producers | 62 literal C++ and independent arithmetic controls per profile pass |
| Encrypted Parquet | 48 local/registered-memory cases across both profiles, including exact error controls, pass |

Case IDs, verdicts and skip reasons match in each full matrix. JUnit's 3,206/3,456
successful records include the shared TIME-to-TIME_NS xpass, not an extra ordinary
pass. All 300 frozen upstream test/data inputs remain unchanged. The non-fast HTTP
transport replays five pinned official responses; configured HTTPFS/cloud access
is not inferred. Typing explicitly targets Python 3.12. Native signatures, detached
content manifests and installed native hashes verify before/after the stages.

## Individual client timing diagnostics

Three interleaved fresh-process samples per engine/storage, one million nullable
strings and one engine thread. Values are separate medians; RSS is whole-process
decimal MB, including interpreter and input. These are Rosetta measurements,
not native Intel hardware or TPC-H/ClickBench acceptance.

| Storage | C++ ms | Rust ms | Rust/C++ time | C++ MB | Rust MB |
|---|---:|---:|---:|---:|---:|
| Python strings | 55.83 | 71.93 | 1.289 | 197.10 | 189.42 |
| Arrow strings | 105.49 | 15.79 | 0.150 | 264.54 | 208.10 |

The Python-backed string scan remains an explicit 28.9% timing outlier with
similar memory. It is not accepted by averaging it with the faster Arrow path.
No table cache or memory-budget increase was introduced. Final native coupled
speed/memory acceptance and individual outlier dispositions remain open.

Evidence under `.context/artifacts/platform-matrix-20261029/`:

- `macos-intel312-python-client-verification-1.json`: ten reconciled gate receipts.
- `macos-intel312-{python,cpp}-wheel-freeze-1.json`: original/signed artifact identities.
- `work/apple/python-intel312-{controls,fast-arrow19,fast-arrow21,package-controls,nonfast,persistence,arrow-producers,encryption-arrow19,encryption-arrow21,client-measurements}-1/`:
  exact stage records, source/image checks, before-replay snapshots and raw samples.
- `work/apple/python-intel312-inputs-1/`: interpreter identity, published wheel
  metadata, exact dependency versions and downloaded wheel hashes.

Other supported Python ABIs, Linux x86_64, iOS runtime and final common-source
signed release verification remain separate goal requirements.
