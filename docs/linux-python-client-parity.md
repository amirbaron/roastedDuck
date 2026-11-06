# Linux arm64 Python checkpoint — 2026-10-30

The newer signed ArrowString candidate has completed the same full fast,
supplement/package/typing/ADBC/persistence/encryption gates. Its native hash is
`818d5153d7807f08f235deb3d49607a44f12d4e08a86ff4a4cb1042a1dfb1361`.
Arrow-backed strings now measure10.90ms/149.23MB versus C++79.56ms/195.65MB;
Python-backed strings remain1.217x time/1.004x RSS. See
[current ArrowString evidence](python-arrow-string-scan-parity.md).
The following sections preserve the preceding wheel's identities and measurements;
they are not the newer candidate's artifact manifest. Final integrated macro and
other-platform acceptance remain open.

The signed CPython3.13 Linux arm64 wheel matches fresh official C++ controls across
both complete Arrow fast matrices,the seven-file non-fast cohort and object-buffer
boundaries. Installed stubs/typing/exports/ADBC and eight typed persistence journeys
also pass. This is not full release,other-platform or performance acceptance.

## Functional and package evidence

| Gate | Current Linux result |
|---|---|
| Arrow19 full fast |3,241 records each:3,205 pass,27 skip,eight xfail,one shared xpass; exact IDs/verdicts/skip reasons |
| Arrow21 full fast |3,492 records each:3,455 pass,28 skip,eight xfail,one shared xpass; exact IDs/verdicts/skip reasons |
| Non-fast |71 pass/eight exact shared skips on both engines,all seven files invoked |
| Object-buffer boundaries |17 matching outcomes on each engine in each Arrow environment; stride/ownership/reentry/subclass controls unchanged |
| Native identity |Exact installed extension hash and exactly one engine image in `/proc/self/maps` |
| Stubs |All seven installed files match pinned hashes in both environments |
| Runtime exports |All385 frozen exports present; typing-only declarations classified separately |
| Strict typing |Two positive clients pass;each of two negative clients emits its four expected errors |
| Installed Python ADBC |Both environments pass Python UDF→ADBC sequential file,rollback,commit and reopen with one native image |
| Persistence |Eight bidirectional15-column checkpoint/nonempty-WAL,append/defaults,receiver mutation/checkpoint and origin readback journeys pass |

The fast-suite shared xpass is TIME→TIME_NS insertion. JUnit counts it among
3,206/3,456 successful records; it is not an extra ordinary passing case. The
non-fast skips include Windows-only cases and unset configured-extension/HTTPFS
fixtures. The H2O transport replays five pinned official responses. These results
do not establish configured HTTPFS or arbitrary extension loading.

Both Python versions are1.5.6 with the same pinned dependencies. The full300-file
upstream test/data input set matches the release archive. The Rust wheel is tagged
`cp313-cp313-manylinux_2_28_aarch64`; wheel SHA256:
`9d649e08f8275b6b09bdc500a1d9a7e35eb8fa6b1d62c1822c65315a4ec1f94a`;
native SHA256:
`b1162d10d1838751dedd0483fc94881983802307b11bf8228af9bb7b3166dcb2`.
The official C++ native hash is
`8cf879cf819bc239a6fbdfcf010b97ed53ad098bcfb8822e2477a3197f182ed4`.

The wheel and extracted native payload are content-signed with the existing local
identity. Installation retains the exact signed native bytes. Source hashes,
installed image identities and signed payloads verify around the runs. This is
content authentication,not a published or vendor-signed production release.

## Individual string-scan outliers

Three interleaved fresh-process samples per engine/storage,one million nullable
strings,one engine thread,warmed imports/input and checked count/length answers.
Values are separate medians; RSS is whole-process decimal MB including interpreter
and input. Linux `getrusage` KiB is explicitly converted to bytes. This is the
2CPU/7GiB Linux arm64 VM environment,not native Linux hardware or macro acceptance.

| Storage | C++ ms | Rust ms | Rust/C++ time | C++ MB | Rust MB | Rust/C++ RSS |
|---|---:|---:|---:|---:|---:|---:|
| Pandas Python strings |42.45|53.41|1.258|158.05|139.08|0.880|
| Pandas Arrow strings |73.40|118.82|1.619|203.92|223.77|1.097|

The Arrow-backed case is worse on both time and RSS and remains an open individual
outlier. No aggregate average or lower-memory macro result is used to accept it.
The Python-backed case saves12% whole-process RSS but takes25.8% longer; its
disposition is also separate from the accepted TPC-H/ClickBench policy.

## Evidence and remaining gates

Root: `.context/artifacts/platform-matrix-20261029/`,on the dedicated SSD.

- `linux-python-wheel-freeze-1.json`:wheel/native identities and signing record.
- `work/linux-arm64/python-env-install-1/`:four pinned environments,identities/freezes.
- `linux-python-fast-both-1.json` and
  `work/linux-arm64/python-fast-arrow{19,21}-1/`:fresh C++ and Rust full matrices.
- `work/linux-arm64/python-supplement-1/verification.json`:object/non-fast controls.
- `work/linux-arm64/python-package-controls-1/verification.json`:stubs/typing/exports/ADBC.
- `work/linux-arm64/python-persistence-1/verification.json`:eight typed journeys and
  original pre-replay DB/WAL snapshots.
- `work/linux-arm64/python-client-measurements-1/verification.json`:all12 paired
  string samples,conditions,result checks and time/RSS summaries.

Encrypted Parquet installed-client checks subsequently pass all48 cases across both
Arrow profiles: local and registered-memory files, three key sizes, prepared COPY,
COPY FROM, nested values, plan serialization and exact wrong-key/corruption errors.
Evidence: `work/linux-arm64/python-encryption-1/verification.json`. Broader installed
producer/control coverage,other OS/architectures,updated Mac gates and the remaining
core/shell/storage/WAL/performance release requirements remain open. Preserve the
accepted uncached lower-memory macro policy; do not add RAM merely to chase C++ speed.

## Arrow-backed string route diagnosis

A subsequent unchanged-wheel diagnostic compares the public `from_df(frame)` path
with `from_arrow(pyarrow.Table.from_pandas(frame))` for the same one-million-row
nullable `string[pyarrow]` frame. The timed region includes the Arrow export. Three
interleaved fresh-process samples per engine/route, one engine thread and the pinned
Arrow19 environment produce these medians:

| Engine | Input route | Total time | Whole-process peak RSS |
|---|---|---|---|
| C++ | Pandas |74.65 ms|195.35 MB|
| Rust | Pandas |127.03 ms|222.61 MB|
| C++ | Arrow export + scan |4.96 ms|156.77 MB|
| Rust | Arrow export + scan |8.03 ms|157.12 MB|

All12 samples return the exact750,000 non-NULL/2,500,000-character result. Each
sample verifies its actual native image/hash and sole engine mapping in-process
before and after; the signed wheel manifest also verifies around the complete run.
Evidence: `work/linux-arm64/python-arrow-string-routes-1/verification.json`.

This identifies a promising lower-memory input route, not an implemented automatic
optimization or performance acceptance. Current `pandas.rs` recognizes Pandas
`ArrowDtype` for Arrow import, while Arrow-backed `StringDtype` takes the object-array
`to_numpy` scan. Before changing that route, verify column normalization, named
indexes, buffer lifetime and mutations against the frozen C++ behavior. Mixed
non-Arrow columns must retain their existing conversion/error contract. The
original installed-client outlier and final macro acceptance remain open.
