# macOS arm64 Python checkpoint

The fresh signed CPython3.13 candidate incorporates the bounded ArrowString scan
and named-memory lifetime/raw-name corrections. It matches fresh official C++1.5.6
controls on macOS arm64. Linux results are recorded separately; neither checkpoint
establishes other architectures, Python ABIs or final release acceptance.

The signed wheel SHA256 is
`cfe1b17651d86233a8d7727166ad936f392fcf36ae3305dba61af30355f83eaa`;
the installed native extension is
`e5d8b108bbfefda43e5a680aff4aa48e6e25473bd20ca63afd6aec774f6108c3`.
Mach-O metadata identifies arm64, macOS11.0 minimum and SDK27.0. The build parent
exits zero before signing; native code signatures and the detached content manifest
verify. Both isolated Arrow environments install the exact signed native bytes and
reuse recorded, unchanged pinned dependencies without the old engine package.

## Functional and installed-client gates

| Gate | Current result |
|---|---|
| Arrow19 complete fast matrix |3,241 outcomes per engine:3,205 pass,27 skip,eight xfail,one shared xpass|
| Arrow21 complete fast matrix |3,492 outcomes per engine:3,455 pass,28 skip,eight xfail,one shared xpass|
| Exact comparison |Case IDs, verdicts and skip reasons match; all300 frozen test/data files unchanged|
| Named-memory controls |11 matching outcomes per profile, including Apple case folding, raw spelling, final-close lifetime, no files and `chdir` independence|
| ArrowString controls |23 matching outcomes per profile, including repeated mutation, indexes/names, slices/chunks and mixed columns|
| Object-buffer boundaries |17 matching outcomes per profile, including strides, ownership and subclass overrides|
| Arrow producers |62 literal/arithmetic/projection/filter/prepared/profile controls per profile|
| Non-fast |71 pass/eight shared skips across all seven frozen files|
| Package/typing |Seven pinned stubs,385 runtime exports, two strict positive clients and two negative clients with four expected errors each|
| Installed ADBC |Both environments pass UDF/Arrow, file, rollback/commit and reopen checks|
| Persistence |Eight15-column bidirectional checkpoint/nonempty-WAL journeys, receiver mutation/checkpoint and origin readback|
| Encrypted Parquet |48 installed cases across profiles; local/registered-memory, prepared writes, read/COPY FROM/nested equality and exact invalid-input errors|

The shared TIME→TIME_NS xpass is included among JUnit successful records; it is not
an extra ordinary pass. The non-fast H2O transport replays five pinned official
responses. These results do not prove configured HTTPFS/arbitrary extension loading.

## Coupled client measurements

Three interleaved fresh processes per engine/storage use one million nullable
Pandas strings and one engine thread on the Mac host. The timed region includes
`from_df` binding/conversion and query execution. All12 results and in-process
native hashes/sole engine mappings match. Peak RSS is the whole process, in decimal
MB. This host run is not presented with the Linux container's resource limits.

| String storage | C++ time / RSS | Rust time / RSS | Rust/C++ time | Rust/C++ RSS |
|---|---|---|---|---|
| Python |35.09 ms /160.76 MB|41.39 ms /159.45 MB|1.180|0.992|
| PyArrow |62.02 ms /214.76 MB|10.23 ms /177.34 MB|0.165|0.826|

Python-backed strings remain an explicit18.0% timing gap with similar RSS. The
ArrowString gain is preserved. The corresponding Linux gap remains21.7%; the
platform results are not averaged together. No cache is added to close these gaps.
Final coupled TPC-H/ClickBench acceptance remains separate and open.

## Evidence

The dedicated SSD root is `.context/artifacts/platform-matrix-20261029/`:

- `source-30/`: compiled Mac source; `source-31/`: portable ArrowString image guard.
- `macos-python-build-1/terminal.json`: actual successful build.
- `macos-python-wheel-freeze-1.json`, `work/apple/python-wheel-signed-1/`: signatures
  and immutable wheel/native identities.
- `work/apple/python-env-install-1/`: dependency reuse, versions and native identity.
- `work/apple/python-controls-1/verification.json`: focused paired controls.
- `work/apple/python-fast-arrow{19,21}-1/comparison.json`: exact full matrices.
- `work/apple/python-nonfast-1/verification.json`: non-fast outcomes and transport pins.
- `work/apple/python-package-controls-1/verification.json`: package/typing/ADBC.
- `work/apple/python-persistence-1/verification.json`: eight journeys and DB/WAL snapshots.
- `work/apple/python-arrow-producers-1/verification.json`:62 controls per profile.
- `work/apple/python-encryption-arrow{19,21}-1/verification.json`:48 encrypted cases.
- `work/apple/python-client-measurements-1/verification.json`: all12 coupled samples.

Other Python versions admitted by the package (`>=3.10`) have not yet received this
full ABI/runtime matrix. macOS Intel, Linux Intel, iOS package/runtime and final
integrated core/shell/C API/ADBC/client/storage/WAL/performance gates remain open.
