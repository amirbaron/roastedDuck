# Linux x86_64 CPython 3.13 client verification

The signed source-56 Rust wheel completes the recorded CPython 3.13 component
matrix against the original C++ 1.5.6 wheel under Linux x86_64/Rosetta. Both Arrow
profiles use identical pinned dependencies and CPython 3.13.18. The full source
snapshot covers 2,310 files. Strict Clippy, explicit Cargo release compilation,
Maturin packaging and source/signature checks pass. This is component evidence,
not final common-source release or other Python ABI acceptance.

Wheel SHA256: `0bdbd9e96d177aaf65be2fdf02504b0921563af4306297109d29bba70780725d`.
Rust native SHA256: `0e92fccccc970634822ddbf2ea61da11abe7742bc8b1550588a10ba80689021d`.
C++ native SHA256: `917eddb07d089cf38c298f0fe126897768bed23fdf787354cd8a8b3894bc5c77`.
All installed native hashes/ELF architecture are checked before first engine import.
Each process reports exactly its expected loaded engine. Runtime containers use
7 GiB RAM without swap; builds/tests are serial and low priority.

| Gate | Result |
|---|---|
| Full Arrow 19 fast matrix | Both 3,205 passes, one XPASS, 27 skips and eight XFAILs; all verdicts and skip reasons match |
| Full Arrow 21 fast matrix | Both 3,455 passes, one XPASS, 28 skips and eight XFAILs; all verdicts and skip reasons match |
| Object-buffer boundaries | 17 independent outcomes match per Arrow profile |
| Seven-file non-fast cohort | Both 71 passes/eight literal matching skips; frozen H2O transport only |
| Installed package | Exact pinned stubs/385 runtime exports, strict positive and negative typing controls, installed ADBC/UDF/transaction/reopen checks |
| Typed persistence/WAL | Eight 15-column checkpoint/nonempty-WAL journeys, both directions/profiles, receiver mutation/checkpoint and origin readback |
| Encrypted Parquet | 48 local/registered-memory cross-engine cases with literal error comparison |
| Arrow producers | 62 independent controls per engine/profile, including dynamic filters, prepared reset and profiles |
| Named-memory behavior | 11 exact fresh C++ controls per profile |
| ArrowString contract | 23 exact controls per profile, including mutation, ownership, chunks/offsets and NULLs |

Three interleaved fresh-process samples per engine/storage mode also verify values,
identities, execution time and whole-process RSS. These are translated client
measurements, not native TPC-H/ClickBench acceptance:

| Nullable million-string input | C++ time / RSS | Rust time / RSS | Rust/C++ time / RSS |
|---|---|---|---|
| Python-backed | 75.85 ms / 192.29 MB | 82.48 ms / 196.98 MB | 1.087 / 1.024 |
| Arrow-backed | 119.51 ms / 248.78 MB | 20.14 ms / 207.54 MB | 0.169 / 0.834 |

The Python-backed RSS is 2.4% higher, explicitly separate from the macro lower-memory
policy. No table cache or higher memory budget was added to obtain these results.

Evidence under external `.context/artifacts/platform-matrix-20261029/`:

- `linux-amd64-python-build-56/` and `linux-amd64-python-wheel-freeze-56.json`.
- `work/linux-amd64/python-wheel-signed-56/`: immutable wheel/native/signatures.
- `work/linux-amd64/python-env-install-58/`: four offline pinned environments and pre-import identities.
- `work/linux-amd64/python-fast-arrow{19,21}-59/comparison.json`: complete paired case verdicts.
- `work/linux-amd64/python-{supplement-63,package-controls-64,persistence-65,encryption-66}/verification.json`.
- `work/linux-amd64/python-{arrow-producers-67,named-memory-controls-68,client-measurements-69,arrow-string-controls-73}/verification.json`.

Other ABI/target cells remain open. CP314 has no published wheels for the older
NumPy/Pandas/Arrow versions in these profiles, so it requires an applicable pinned
profile rather than forcing unsupported combinations. Remaining native Swift/ADBC,
core/shell/storage and final coupled macro/outlier gates also remain open.

A further explicit negative-input probe121 finds a remaining contract gap outside
the completed component cohorts: C++ rejects generic `__dataframe__` replacement
objects with InvalidInputException, while source56 reports NotImplementedException.
The current unverified fix removes that special pending branch and uses the
existing C++-compatible unsupported-object error. A new paired control also
checks that unsupported protocol descriptors are not invoked. The enum constructor
NotImplementedException already matches C++. Rebuild and runtime checks remain
pending; this new finding does not rewrite prior component receipts.
