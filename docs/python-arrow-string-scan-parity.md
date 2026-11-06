# Arrow-backed Pandas string scan

The Linux arm64 candidate replaces full-column NumPy/Python-string materialization
for Pandas Arrow-backed `StringDtype` columns with bounded Arrow-buffer conversion.
It captures the immutable Arrow array at each Pandas bind and imports at most one
engine chunk through the existing Arrow C-data converter. Mixed columns keep their
existing Pandas conversion paths, names/index handling and query plan. No retained
table-block cache or full-column contiguous Arrow copy is added.

Capturing an Arrow table when the relation is created would change observable
behavior. Frozen C++ controls on both Arrow19/21 observe in-place element/NULL
changes before execution and on later executions of the same relation; the early
table snapshot misses those updates. That approach was rejected. The candidate
keeps conversion inside the Pandas binder so each execution sees the correct
buffer snapshot, while each running query owns its immutable data.

## Current Linux arm64 verification

The release build and `cargo clippy -p rustdb-python --all-targets -- -D warnings`
pass. The signed candidate wheel is
`3b7db7f70ab702695344a3d7d56358c11269b549647b235d38000e0b201c87cf`;
its native extension is
`818d5153d7807f08f235deb3d49607a44f12d4e08a86ff4a4cb1042a1dfb1361`.
The build parent exited zero before signing. The baseline wheel/environments remain
preserved. Candidate environments install the new wheel and reuse explicitly
recorded unchanged third-party dependencies; the old engine package is excluded.

All23 focused controls match frozen C++ on each Arrow19/21 profile, with no errors.
They cover in-place mutations before and after execution, replacing columns,
ownership, column normalization, named/multi-indexes, empty/NULL data, embedded NUL
and Unicode, mixed columns, vector/chunk boundaries and sliced Arrow offsets.

Both complete fast matrices also match fresh C++ controls exactly in case IDs,
verdicts and skip reasons:

| Profile | Outcomes per engine |
|---|---|
| Arrow19 |3,241:3,205 pass,27 skip,eight xfail,one shared xpass|
| Arrow21 |3,492:3,455 pass,28 skip,eight xfail,one shared xpass|

All300 pinned upstream test/data files remain unchanged. Installed native identities
and signed candidate payloads verify around the runs. JUnit includes the shared
TIME→TIME_NS xpass among successful records; it is not an extra ordinary pass.

The installed-client refresh also passes all17 object-buffer outcomes per engine
and profile, the seven-file non-fast cohort (71 pass/eight exact shared skips),
seven pinned stubs,385 runtime exports, two strict positive typing clients and two
negative clients with four expected errors each, installed ADBC, eight bidirectional
15-column checkpoint/nonempty-WAL journeys and48 encrypted-Parquet cases. Native
identities, signed payloads and frozen upstream inputs remain unchanged.

## Coupled speed and memory

Three interleaved fresh-process samples per engine/storage in the pinned Arrow19
environment use one engine thread and a two-CPU/7 GiB/no-swap Linux arm64 container.
The timed region includes `from_df` binding/conversion and the query over one million
nullable strings. All12 results and in-process native hashes/sole engine mappings
match, with wheel signatures verified around the run. Whole-process peak RSS is
reported in decimal MB; it is not an incremental-allocation measurement.

| Pandas string storage | C++ time / peak RSS | Rust time / peak RSS | Rust/C++ time | Rust/C++ RSS |
|---|---|---|---|---|
| Python |42.31 ms /139.12 MB|51.49 ms /139.66 MB|1.217|1.004|
| PyArrow |79.56 ms /195.65 MB|10.90 ms /149.23 MB|0.137|0.763|

The Arrow-backed input outlier improves both speed and memory. The Python-backed
string path remains an explicit21.7% timing outlier with similar RSS. These are
scoped client measurements; they neither replace the accepted TPC-H/ClickBench
baseline nor establish final integrated performance or other-platform acceptance.
The implementation adds no cache to close the remaining speed gap.

The repository measurement helper now normalizes Linux `ru_maxrss` KiB to bytes
and reports the actual platform instead of stale macOS/development-build labels.
The paired candidate run also records native identities in each measured process.

## Evidence

The dedicated SSD root is `.context/artifacts/platform-matrix-20261029/`:

- `work/linux-arm64/python-arrow-string-contract-1/`: the rejected early-snapshot
  route and exact mutation differences on both engines/profiles.
- `source-22/`: compiled candidate source; `source-23/`: expanded boundary controls.
- `linux-python-build-3/terminal.json`: successful actual build-parent exit.
- `linux-python-wheel-freeze-2.json` and `work/linux-arm64/python-wheel-signed-2/`:
  candidate bytes, native identity, signed manifest and trusted certificate pin.
- `work/linux-arm64/python-env-install-2/`: isolated candidate environments and
  recorded dependency reuse, versions and native identities.
- `work/linux-arm64/python-arrow-string-controls-2/verification.json`:23 exact
  paired controls per Arrow profile.
- `work/linux-arm64/python-fast-arrow{19,21}-2/comparison.json`: complete matrices,
  exact case/verdict/skip comparison and raw logs.
- `work/linux-arm64/python-supplement-2/verification.json`: boundary/non-fast gates.
- `work/linux-arm64/python-package-controls-3/verification.json`: stubs, exports,
  typing and installed ADBC.
- `work/linux-arm64/python-persistence-2/verification.json`: eight typed journeys
  and original DB/WAL snapshots.
- `work/linux-arm64/python-encryption-2/verification.json`:48 installed cases.
- `work/linux-arm64/python-client-measurements-2/verification.json`: all12 samples,
  results, native identities and coupled time/RSS summaries.

The first build-driver attempt after the source change failed on an incorrectly
named checksum-list path before compilation. The successful fresh attempt uses
the correct source22 list; both attempts remain preserved. The pre-existing whole
file rustfmt differences in `pandas.rs` were left intact; the new blocks follow
rustfmt. Neither issue is reported as a passing check or an engine failure.
The first package refresh stopped after its functional checks because it expected
an absent mypy directory. A fresh run verifies/reuses the existing exact pinned
tool versions and passes all typing assertions. The next orchestration attempt
still referenced that failed prerequisite; it stopped before launching persistence.
The corrected run uses the actual successful package terminal. Failed attempts and
all successful actual process exits remain preserved separately.
