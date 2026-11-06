# Python named-memory connections

A public client audit found that Rust's Python `connect(':memory:name')` shared
live connections through its pool but opened the name as a disk file. Closing the
last connection left the file and its table behind. The official frozen C++ wheel
creates no file and reopens the same name empty after the last close.

The signed ArrowString baseline independently reproduces this behavior. Both
engines share a live marker row42, but C++ reports zero marker tables after final
close and no files; Rust reports one marker table and creates`:memory:CaseProbe`.
The native hashes are checked in each process before the control runs.

## Correction and rejected intermediate

The Python pool now retains the process-local name as its key while opening the
engine at`:memory:`. Named keys do not depend on the current directory or resolve
through the filesystem. The existing platform case-folding policy still applies.

The first lifetime correction used `PathBuf` keys. Focused C++ controls exposed
two remaining mismatches: filesystem path equality merges names containing dot
components or repeated separators, whereas C++ treats those bytes as name data.
That signed intermediate candidate was rejected before the full matrix. The
current candidate uses `ByteString` keys; filesystem database paths retain their
existing canonicalization, while named-memory identities preserve their spelling.

The permanent public control script covers11 outcomes: live same-name sharing,
database/path introspection, final-close lifetime, current-directory independence,
platform case behavior, different-name isolation, unnamed isolation, configuration
mismatch, dot/separator identity and absence of persistent files. The C++ control
uses the actual1.5.6 API; an initial fixture incorrectly requested an unavailable
`in_memory` metadata column and was corrected to check its public `path` field.
That failed fixture run remains preserved.

The source28 release candidate passes Clippy and the complete Linux arm64 client
refresh. The signed wheel SHA256 is
`786b5f1fb4a5f865df06ae71b737c19fdf742aa677deeee3b62f8dd7b2827c37`;
its native extension is
`c5fd016d37ae6c98d189b4b6a9f8a59644835d8b4baa7e7e39a61227b9d4928c`.
Source29 adds only portable test-image enumeration and documentation; compiled
product code remains source28. The fresh macOS arm64 CPython3.13 wheel subsequently
passes the corresponding full client refresh; see
[macOS Python evidence](macos-python-client-parity.md). Other ABIs/architectures
remain unverified.

All11 public outcomes match fresh C++ controls in each Arrow19/21 environment.
Both complete fast matrices also match exactly in case IDs, verdicts and skip
reasons: Arrow19 has3,241 outcomes (3,205 pass,27 skip,eight xfail,one shared xpass);
Arrow21 has3,492 (3,455 pass,28 skip,eight xfail,one shared xpass). All300 frozen
upstream inputs remain unchanged. The shared xpass is not an additional ordinary
passing case.

The refresh passes17 object-buffer outcomes per engine/profile, the seven-file
non-fast cohort (71 pass/eight shared skips), seven stubs,385 runtime exports,
positive/negative strict typing and installed ADBC, eight15-column checkpoint/WAL
journeys and48 encrypted-Parquet cases. Signatures and native identities verify
around the runs; the original baseline and rejected intermediate remain preserved.

## Coupled client measurements

Three interleaved fresh processes per engine/storage, one million nullable rows,
one engine thread and the same Linux2CPU/7 GiB/no-swap profile produce these medians.
Peak RSS is the whole process in decimal MB, not incremental allocations.

| Pandas string storage | C++ time / RSS | Rust time / RSS | Rust/C++ time | Rust/C++ RSS |
|---|---|---|---|---|
| Python |42.13 ms /138.75 MB|51.27 ms /139.43 MB|1.217|1.005|
| PyArrow |76.68 ms /195.38 MB|11.94 ms /148.81 MB|0.156|0.762|

The Python-backed21.7% timing outlier remains explicit. The ArrowString gain is
preserved. These scoped client measurements do not establish final macro or
other-platform acceptance; no storage cache is added to close a timing gap.

## Evidence

The dedicated SSD root is `.context/artifacts/platform-matrix-20261029/`:

- `python-named-memory-control-1/` and
  `work/linux-arm64/python-named-memory-{cpp,rust}-1/report.json`: original
  lifetime/file counterexample on the unchanged signed baseline.
- `work/linux-arm64/python-named-contract-cpp-2.json`: eleven frozen C++ outcomes.
- `source-27/`, `linux-python-wheel-freeze-3.json`,
  `work/linux-arm64/python-wheel-signed-3/`: rejected intermediate bytes.
- `python-named-candidate3-namespace-differences.json`: two exact aliasing mismatches.
- `source-28/`, `linux-python-build-5/`: current byte-string-key candidate build.
- `linux-python-wheel-freeze-4.json`, `work/linux-arm64/python-wheel-signed-4/`:
  accepted component's exact signed payloads and trusted certificate pin.
- `work/linux-arm64/python-named-memory-controls-{4,4b}/verification.json`:
  eleven exact paired controls per profile, including the portable-helper rerun.
- `work/linux-arm64/python-fast-arrow{19,21}-4/comparison.json`: full matrices.
- `work/linux-arm64/python-{supplement,package-controls,persistence,encryption,client-measurements}-4/`:
  supplemental/package/recovery/encryption gates and all12 coupled samples.
- `python-candidate4-refresh-1/terminal.json`: actual successful sequential refresh.

The strict freeze guard initially caught a report edited after the successful
build. The updated report was preserved, the exact archived report restored for
the guard, and then the latest report reinstated after freezing. Product code and
the guard were unchanged. This bookkeeping failure is separate from compiler and
runtime outcomes.

This is a correctness repair, not a new persistent cache or an increase in memory
to close a speed gap. Other platforms and final integrated release verification
remain open.
