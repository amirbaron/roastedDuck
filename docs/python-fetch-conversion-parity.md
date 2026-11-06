# Python fetch conversion and cursor parity

Current verification (2026-11-03): source1959/core1961 has completed all five
macOS CPython3.10–3.14 functional cells and independent reviews1980. Matrix2024
exits0; summary2062 verifies31,135matching XML records per engine (30,743passes/
392retained nonpasses),36typed WAL journeys,216encrypted Parquet cases,18installed
ADBC environments,40typing checks and108coupled samples across18workloads.
Every current workload is listed separately below. Reference-only warning logs
remain explicit. Linux Python and final macro/release acceptance remain open;
older source1742/core1637 receipts below are historical.

The frozen DuckDB 1.5.6 client distinguishes streaming and materialized results
when conversion fails. A successful query alone does not establish fetch parity:
the error, empty output and subsequent fetch position must also match.

| Operation | C++ behavior reproduced by the binding |
|---|---|
| Empty streaming `TIME WITH TIME ZONE` NumPy/DataFrame result | Empty object-typed output |
| Materialized `TIME WITH TIME ZONE` NumPy/DataFrame conversion, including empty results | Exact unsupported-type exception; retry repeats that exception |
| Nonempty streaming `TIME WITH TIME ZONE` bulk conversion | Unsupported-type exception after consuming the fetched chunk |
| Unsupported aggregate-state row conversion | Full logical type in the exception; failed row remains available for the next fetch |
| Materialized bulk conversion failure | Independent collection iteration preserves the existing row-fetch position |

The binding restores a failed row conversion only if the same cursor identity and
epoch remain current. Python conversion runs without holding the state mutex;
replacing or advancing the result therefore prevents a stale rewind. Materialized
bulk scans use a separate chunk index. Empty materialized type validation runs
before collection consumption, so retries retain the original error.

These are Python-binding changes. They add no data cache or query-memory budget.
For the original binding-only source1742, common engine, C API, storage and
dependency inputs were identical to core1637/source1634 (equivalence1749). Current
source1959 includes those five binding files unchanged plus the separately
verified two-file S3 worker-context correction.

The permanent worker is
[`fetch-conversion-boundaries.py`](../scripts/python-tests/fetch-conversion-boundaries.py).
Its JSON configuration names `engine` (`cpp` or `rust`), `native_path`,
`native_sha256` and `report`. The caller authenticates the artifact first. The
worker checks the hash before importing it and verifies the sole loaded engine
before and after execution. It runs all420 records:14 queries, connection/relation/
explicit-materialized surfaces, five fetch methods and mixed/repeated sequences.
Reports retain values, dtypes, shapes, null masks, complete errors and subsequent
cursor behavior. Compare the complete `records` arrays between engines.

The unchanged420-record comparison1746 passes on the corrected signed macOS arm64
CPython3.10 wheel1744 with Arrow19 and21:zero differences on both profiles.
Failed before/intermediate runs1708/1719/1720/1727/1736 remain preserved. The
permanent worker has the same semantic body as the tested worker; only its
docstring and portable identity-helper import name differ (receipt1757).

Full corrected-binding chain1751 now passes: both complete fast suites (6,733
records per engine), unchanged HTTP profiles, all six buffer/Arrow/persistence/
encryption supplements, installed package/typing/ADBC, and both79-record non-fast
profiles. Resolution1762 closes binding marker1718 for this verified macOS CP310
scope. Independent review1816 rechecks the signed wheel, actual installed native hashes,
all12functional receipts and raw XML outcomes/messages/bodies. All6,891records per
engine match:6,804pass and87matching skips/xfails retained as nonpasses across fast
and non-fast profiles. It also independently recomputes the24client samples below.
This closes the scoped CPython3.10 functional review; the final release is not accepted.
Other Python ABI/platform and final release acceptance remain required.

## Coupled client observations

Measurement1758 uses three interleaved fresh processes per engine/workload,
one engine thread and one million nullable strings. Exact answers, native hashes
and sole loaded images pass. Values below are independent three-sample medians;
RSS is whole-process peak in decimal MB. All24 coupled samples, best/median times,
whole/incremental RSS and maximum RSS remain in the external receipt.

| Arrow | String storage | C++ ms | Rust ms | Rust/C++ time | C++ MB | Rust MB |
|---|---|---:|---:|---:|---:|---:|
| 19 | python | 36.127 | 44.820 | 1.241 | 147.177 | 145.801 |
| 19 | pyarrow | 52.630 | 10.692 | 0.203 | 216.728 | 163.299 |
| 21 | python | 35.919 | 43.803 | 1.219 | 145.801 | 143.704 |
| 21 | pyarrow | 52.634 | 10.250 | 0.195 | 215.007 | 161.890 |

Python-backed scans are individual client outliers:22–24% longer execution for
roughly1% lower whole-process RSS (about8ms extra here). Arrow-backed scans use
about25% less RSS and roughly one fifth of the C++ time. These four workload rows
are kept separate; an average must not hide the Python-backed overhead. No caching
or memory-budget increase was made to close that small absolute gap. This is scoped
client evidence, not TPC-H/ClickBench acceptance, a claimed regression relative to
another Python ABI, or whole-port performance completion.

## CPython 3.11

The corrected-source signed CP311 wheel1752 now completes all functional stages
in1818. Independent review1822 checks the exact ABI/source/core provenance, actual
fresh optimized compiler artifact, wheel CMS, installed native hashes/signatures,
all case IDs and complete raw outcome/message/body records. Both fast and both
non-fast profiles total6,891records per engine:6,804pass and87matching skips/xfails,
all retained. Both420-record boundary profiles, HTTP profiles, six supplements,
eight typed checkpoint/WAL handoffs,48encrypted cases,385exports, four installed
ADBC environments and eight positive/negative typing checks pass.

Signed native SHA-256:
`0e2ee6289662f41dd488c040dd82da8830c0a36f9a51590d4029e70a310df691`.
The original1804helper preflight failure expected the earlier development label;
recovery1817 preserves it, requires exact current provenance and adds native/source
linkage. No behavioral assertion, native artifact or engine memory policy changed.

Measurement1770 retains24interleaved fresh-process samples under the same scoped
conditions described above. Independent review1822 recomputes every workload:

| Arrow | String storage | C++ ms | Rust ms | Rust/C++ time | C++ MB | Rust MB |
|---|---|---:|---:|---:|---:|---:|
| 19 | python | 35.748 | 43.156 | 1.207 | 151.912 | 151.011 |
| 19 | pyarrow | 55.736 | 11.225 | 0.201 | 221.348 | 167.346 |
| 21 | python | 36.289 | 43.829 | 1.208 | 151.142 | 149.144 |
| 21 | pyarrow | 60.037 | 11.151 | 0.186 | 219.988 | 166.265 |

Python-backed scans are separate client outliers:about21%longer execution,
0.6–1.3%less whole-process RSS and about7.4–7.5ms extra here. Arrow-backed scans
use about24%less RSS and roughly one fifth of the time. Whole/max/incremental RSS
and best/median times remain in raw receipts; no average hides a workload. These
are current CP311 observations, not cross-ABI regression claims or final TPC-H/
ClickBench acceptance. The remaining ABI/platform/release gates are still open.

## CPython 3.12

All15functional stages in1818and independent review1822pass on signed wheel1752.
The exact-ABI/source/core/fresh-opt3/native-signature and sole-image checks pass.
Raw XML matches all6891records per engine: 6804passes and87matching skips/xfails
retained as nonpasses. Both420-record boundary and24-record HTTP profiles, six
supplements, eight typed WAL journeys,48encryption cases,385exports, four installed
ADBC environments and eight typing checks pass.

Native SHA-256: `0896cdcca2d78b85a773d3557c0450bdd78ce3aae8d4250b937383a89fcdf8e9`.

The24coupled fresh-process samples use the same declared client workload conditions.
Each row below retains its own median; full best/median/max/incremental values and
all raw samples remain in the external1770measurement and1822review.

| Arrow | String storage | C++ ms | Rust ms | Rust/C++ time | C++ MB | Rust MB |
|---|---|---:|---:|---:|---:|---:|
| 19 | python | 36.079 | 45.095 | 1.250 | 151.339 | 150.929 |
| 19 | pyarrow | 57.749 | 11.003 | 0.191 | 209.240 | 168.755 |
| 21 | python | 36.368 | 44.161 | 1.214 | 150.569 | 149.160 |
| 21 | pyarrow | 58.252 | 11.088 | 0.190 | 207.569 | 167.543 |

Python-backed scans remain individual outliers:21–25%longer execution with about
0.3–0.9%less whole-process peak RSS. Arrow-backed scans use about19%less RSS and
take roughly one fifth of the C++time. No cross-ABI regression or final macro
acceptance is inferred, and no cache/memory-budget increase was made. CP313/314,
other platforms and all remaining release gates still require verification.

## CPython 3.13

All15stages in1818and independent1822review pass on signed wheel1752. Exact
ABI/source/core/fresh-opt3, signatures and sole native images verify. The complete
raw XML comparison matches6891records per engine: 6804passes and87matching
skips/xfails retained. Boundary/HTTP profiles, six supplements, eight typed WAL
journeys,48encryption cases,385exports, four installed ADBC environments and eight
positive/negative typing checks pass.

Native SHA-256: `5ae2ac636dd6bb06367c1f1cdc056667625892f2311cb6fcdbd7ed4188637afa`.

Independent review recomputes all24coupled client samples. Medians are shown
separately; all best/median and whole/max/incremental RSS values remain in receipts.

| Arrow | String storage | C++ ms | Rust ms | Rust/C++ time | C++ MB | Rust MB |
|---|---|---:|---:|---:|---:|---:|
| 19 | python | 36.938 | 43.755 | 1.185 | 141.607 | 140.902 |
| 19 | pyarrow | 60.613 | 11.228 | 0.185 | 198.148 | 157.319 |
| 21 | python | 35.527 | 43.533 | 1.225 | 140.165 | 139.788 |
| 21 | pyarrow | 65.937 | 11.349 | 0.172 | 196.755 | 155.763 |

Python-backed scans take18–23%longer for about0.3–0.5%less whole-process RSS.
Arrow-backed scans use about21%less RSS and take17–19%of C++time. Each workload
remains explicit; no cross-ABI regression claim, cache/memory increase or final
TPC-H/ClickBench acceptance is inferred.

Raw C++fast logs retain two `PytestUnraisableExceptionWarning` sites per Arrow
profile, in Pandas varchar/union tests. The underlying ignored exception comes
from `PyObject_HasAttrString()` probing the deprecated private Pandas `_data`
attribute, which raises a `FutureWarning` recommending `_pa_array`. The same
diagnostic is absent from Rust logs; values and case verdicts match. External
review1822retains both raw-log hashes and case IDs. Warning logs are not claimed
byte-identical, and no warning suppression or private-attribute emulation was added.

## CPython 3.14

All14declared stages in1818and independent review1822pass on signed wheel1752,
using the declared soleArrow22profile. Exact ABI/source/core/fresh-opt3, signatures
and sole native images verify. Complete fast plus non-fast XML matches3,571records
per engine:3,527passes and44matching skips/xfails retained. The420-record boundary
and24-record HTTP profiles, six supplements, four typed WAL journeys,24encryption
cases,385exports, two installed ADBC environments and eight typing checks pass.

Native SHA-256: `fa3758bba0bd328744549425aa85baee5e21691e86f2ee842a745bae9467e2d9`.

All12coupled samples retain the same fresh-process/one-thread workload conditions.
The two individual median rows are:

| Arrow | String storage | C++ ms | Rust ms | Rust/C++ time | C++ MB | Rust MB |
|---|---|---:|---:|---:|---:|---:|
| 22 | python | 35.620 | 45.941 | 1.290 | 169.607 | 168.296 |
| 22 | pyarrow | 51.459 | 11.240 | 0.218 | 227.377 | 185.614 |

The Python-backed row is an explicit outlier:about29%longer execution for0.8%less
whole-process peak RSS. The Arrow-backed row takes about22%of the C++time with
18%less RSS. All best/median/max/incremental values remain in receipts; no averaging
hides the slower row, no memory/cache increase was made, and final TPC-H/ClickBench
acceptance is still required. The same two reference-only Pandas/pytest warning
sites described for3.13occur in the C++log and are absent from Rust; their raw-log
hashes are retained in1822. Values, errors, cases and skip reasons match.

## Historical macOS Python matrix (source1742/core1637)

Independent matrix index1831verifies all five declared normal macOS arm64 ABI cells
(CPython3.10–3.14), source1742and unchanged common core1637. Across all profiles,
31,135XML records per engine match:30,743passes and392retained matching nonpasses.
The matrix covers36typed checkpoint/WAL journeys,216encryption cases,18installed
ADBC environments,40typing runs and108coupled client samples over18separate
workloads. This closes the declared macOS Python functional matrix. Linux Python,
current Apple SDK/iOS/physical-device, other runtime/client/storage gates and final
native macro performance acceptance remain open.


Current source1959/core1961 CPython 3.10 review (2026-11-03): independent review1980
verifies the fresh signed wheel across both Arrow profiles. The complete cell has
6,891 matching XML records per engine: 6,804 passes and 87 retained matching
nonpasses. It also verifies eight typed checkpoint/WAL journeys, 48 encrypted
Parquet cases, four installed ADBC environments and eight strict typing runs.
Python 3.11 follows serially. No whole release or macro performance acceptance
is inferred.

The current coupled client observations retain each workload separately:

| Arrow | Input storage | Rust best/C++ | Rust median/C++ | C++ median time (ms) | Rust median time (ms) | C++ max RSS (MiB) | Rust max RSS (MiB) |
|---|---|---:|---:|---:|---:|---:|---:|
| 19 | python | 1.239 | 1.250 | 36.333 | 45.407 | 140.95 | 139.97 |
| 19 | pyarrow | 0.198 | 0.202 | 53.548 | 10.796 | 207.14 | 156.12 |
| 21 | python | 1.236 | 1.261 | 35.710 | 45.018 | 139.73 | 137.25 |
| 21 | pyarrow | 0.197 | 0.198 | 52.559 | 10.425 | 205.89 | 155.06 |

The Python-backed rows remain individual timing outliers: about 24% longer best
time and 25–26% longer median time, with roughly 0.7–1.8% less maximum whole
process RSS. The absolute median difference is about 9 ms. Arrow-backed rows
use about one fifth of C++ time and three quarters of its maximum RSS. All 24
fresh-process samples, whole/incremental RSS and exact native identity checks
remain in the measurement receipt. These client observations are not the final
TPC-H/ClickBench baseline. Cache, buffer and query memory policy is unchanged.
Evidence: `.context/artifacts/platform-matrix-20261029/macos-cp310-functional-review-1980/`
and `work/apple/python-cp310-client-measurements-1979/` under the same artifact root.


Current source1959/core1961 CPython 3.11 review (2026-11-03): independent1980
verifies the full fresh cell: 6,891 matching XML records per engine, 6,804 passes
and 87 matching nonpasses retained; eight typed WAL journeys, 48 encrypted
Parquet cases, four installed ADBC environments and eight typing runs. Python
3.12 follows serially. These scoped results do not accept the whole release.

| Arrow | Input storage | Rust best/C++ | Rust median/C++ | C++ median time (ms) | Rust median time (ms) | C++ max RSS (MiB) | Rust max RSS (MiB) |
|---|---|---:|---:|---:|---:|---:|---:|
| 19 | python | 1.243 | 1.219 | 35.222 | 42.926 | 145.61 | 144.25 |
| 19 | pyarrow | 0.203 | 0.203 | 53.984 | 10.943 | 211.16 | 160.06 |
| 21 | python | 1.225 | 1.230 | 35.878 | 44.135 | 143.45 | 143.03 |
| 21 | pyarrow | 0.201 | 0.194 | 55.149 | 10.713 | 210.03 | 158.89 |

Python-backed timing remains an individual client outlier: about 23–24% longer
best time and 22–23% longer median time, with less than 1% lower maximum whole
RSS. Arrow-backed rows use about one fifth of C++ time and 76% of its maximum
RSS. All 24 raw fresh-process samples and whole/incremental RSS are retained.
These are client observations, not TPC-H/ClickBench acceptance; no cache or memory
policy changed. Evidence: `.context/artifacts/platform-matrix-20261029/macos-cp311-functional-review-1980/`.


Current source1959/core1961 CPython 3.12 review (2026-11-03): independent1980
verifies the fresh signed wheel and complete functional cell: 6,891 matching XML
records per engine, 6,804 passes and 87 matching nonpasses retained. It verifies
eight typed WAL journeys, 48 encrypted Parquet cases, four installed ADBC
environments and eight strict typing runs. Python 3.13 follows serially.

| Arrow | Input storage | Rust best/C++ | Rust median/C++ | C++ median time (ms) | Rust median time (ms) | C++ max RSS (MiB) | Rust max RSS (MiB) |
|---|---|---:|---:|---:|---:|---:|---:|
| 19 | python | 1.221 | 1.236 | 35.162 | 43.459 | 145.42 | 144.28 |
| 19 | pyarrow | 0.200 | 0.199 | 56.742 | 11.319 | 200.20 | 161.27 |
| 21 | python | 1.246 | 1.223 | 35.611 | 43.556 | 143.62 | 142.61 |
| 21 | pyarrow | 0.182 | 0.184 | 56.901 | 10.488 | 198.42 | 159.77 |

Python-backed rows remain individual timing outliers: 22–25% longer best time and
22–24% longer median time (about 8 ms), with under 1% lower maximum whole-process
RSS. Arrow-backed rows use 18–20% of C++ time and about 81% of its maximum RSS.
All 24 fresh-process samples, best/median times and whole/incremental RSS remain
in the receipt. These measurements do not establish TPC-H/ClickBench acceptance;
cache and query memory policy are unchanged. Evidence:
`.context/artifacts/platform-matrix-20261029/macos-cp312-functional-review-1980/`.


Current source1959/core1961 CPython 3.13 review (2026-11-03): independent1980
verifies the complete fresh signed functional cell: 6,891 matching XML records per
engine, 6,804 passes and87 matching nonpasses retained. Eight typed WAL journeys,
48 encrypted Parquet cases, four installed ADBC environments, eight strict typing
runs and24coupled client samples verify. Python3.14 follows serially.

| Arrow | Input storage | Rust best/C++ | Rust median/C++ | C++ median time (ms) | Rust median time (ms) | C++ max RSS (MiB) | Rust max RSS (MiB) |
|---|---|---:|---:|---:|---:|---:|---:|
| 19 | python | 1.247 | 1.245 | 35.661 | 44.413 | 135.70 | 134.84 |
| 19 | pyarrow | 0.189 | 0.196 | 57.610 | 11.294 | 189.17 | 150.12 |
| 21 | python | 1.230 | 1.230 | 34.887 | 42.906 | 133.56 | 133.14 |
| 21 | pyarrow | 0.184 | 0.183 | 58.637 | 10.736 | 187.64 | 148.61 |

The two Python-backed workloads are individual outliers: 23–25% longer best and
median execution (about8–9ms), with under1% lower maximum whole-process RSS.
Arrow-backed rows use18–20% of C++ time and about79% of its maximum RSS. All raw
samples, best/median time and whole/incremental RSS remain in the receipt; no
cache or query memory policy changed. These are client observations, not final
TPC-H/ClickBench acceptance.

Each current C++ fast log records two PytestUnraisableExceptionWarning sites in
test_all_types Pandas varchar/union cases, from PyObject_HasAttrString accessing
Pandas' deprecated _data property. These warnings are absent from Rust logs;
raw SHA/log summaries are retained in2059/2060. Warning logs are not byte-identical,
and no warning suppression or private-attribute emulation was added. Evidence:
`.context/artifacts/platform-matrix-20261029/macos-cp313-functional-review-1980/`.


Current source1959/core1961 CPython 3.14 review (2026-11-03): independent1980
verifies the fresh signed normal ABI cell on its declared Arrow22 profile:
3,571matching XML records per engine,3,527passes and44retained matching nonpasses.
Four typed WAL journeys,24encrypted Parquet cases, two installed ADBC environments,
eight typing checks and12coupled client samples verify.

| Arrow | Input storage | Rust best/C++ | Rust median/C++ | C++ median time (ms) | Rust median time (ms) | C++ max RSS (MiB) | Rust max RSS (MiB) |
|---|---|---:|---:|---:|---:|---:|---:|
| 22 | python | 1.312 | 1.293 | 35.325 | 45.669 | 162.80 | 161.81 |
| 22 | pyarrow | 0.216 | 0.215 | 48.101 | 10.358 | 217.39 | 177.73 |

The Python-backed row is an individual outlier:31.2%longer best time and29.3%longer
median time (about10ms), with0.6%lower maximum whole-process RSS. Arrow-backed
conversion takes21.5–21.6%of C++time with18.2%lower maximum whole-process RSS.
All raw samples and whole/incremental RSS remain in the receipt. These are client
observations, not final TPC-H/ClickBench acceptance; memory/cache policy is unchanged.
The same two C++-only Pandas warning sites occur in the current Arrow22fast log;
2061retains raw log hashes and summaries. Warning logs are not byte-identical.
Evidence: `.context/artifacts/platform-matrix-20261029/macos-cp314-functional-review-1980/`.


## Corrected candidate: Python 3.10 (2026-11-03)

Source2078/signed normal CP310 wheel2098 now passes its complete paired Arrow19/21 cell and independent2103 review:6,891matching XML records per engine (6,804passes,87retained skips/xfails),420typed boundaries and24HTTP records per profile,8typed checkpoint/WAL journeys,48encrypted Parquet cases,4installed ADBC environments and8typing runs. No case selection or raw nonpass changes. Wheel nativeSHA256: `1d003b096afeaab53ce60d4cbb849b60f9392b65577c10ea4de27255171dde4f`. Other required ABIs and final macro acceptance remain pending.

Each client workload remains separate. Times below are milliseconds; RSS is maximum whole-process decimal MB across three samples. These client observations are separate from the130 TPC-H/ClickBench configurations.

| Arrow / input storage | C++ best / median ms | Rust best / median ms | C++ / Rust max RSS MB | Rust/C++ median time |
|---|---:|---:|---:|---:|
| 19 / python | 35.86 / 36.33 | 42.06 / 43.23 | 148.09 / 146.11 | 1.190 |
| 19 / pyarrow | 50.41 / 52.64 | 10.31 / 10.34 | 216.65 / 164.59 | 0.196 |
| 21 / python | 35.41 / 36.13 | 43.24 / 43.43 | 145.85 / 144.54 | 1.202 |
| 21 / pyarrow | 49.91 / 52.36 | 10.20 / 10.40 | 215.71 / 162.37 | 0.199 |

Both Python-backed workloads remain flagged individually:17.3%/22.1%longer best time and19.0%/20.2%longer median time, with1.3%/0.9%lower maximum whole-process RSS. Their absolute median differences are about6.9/7.3ms. Arrow-backed workloads use about20%of C++median time and75–76%of its maximum whole-process RSS. All24raw samples and original native identities remain in `platform-matrix-20261029/work/apple/python-cp310-client-measurements-2102/`; the independent current index is `corrected-Mac-Python-cp310-current-index-2193/verification.json`. No cache/memory budget was increased, and no aggregate hides the Python-backed outliers.


## Corrected candidate: Python 3.11 (2026-11-03)

Source2078/signed CP311 wheel2098 passes both complete Arrow19/21 profiles and independent2103 review:6,891matching XML records per engine,6,804passes and87retained skips/xfails;420boundaries/24HTTP records per profile;8typed WAL journeys,48encrypted Parquet cases,4installed ADBC environments and8typing runs. NativeSHA256: `094d514ccf694517e3312f359d074721939fcdeed7d1d8dcb9d8b524b8c3feb7`. CP310andCP311together are13,782records per engine/13,608passes/174retained nonpasses; three Mac ABI cells remain pending.

All four measured CP311 client workloads remain separate (time in ms, maximum whole-process RSS in decimal MB across three samples):

| Arrow / input storage | C++ best / median ms | Rust best / median ms | C++ / Rust max RSS MB | Rust/C++ median time |
|---|---:|---:|---:|---:|
| 19 / python | 34.51 / 35.29 | 42.50 / 44.44 | 152.29 / 151.04 | 1.259 |
| 19 / pyarrow | 52.35 / 53.19 | 10.37 / 10.55 | 221.66 / 168.36 | 0.198 |
| 21 / python | 35.23 / 35.39 | 41.57 / 41.70 | 150.55 / 149.44 | 1.179 |
| 21 / pyarrow | 53.05 / 54.17 | 10.39 / 10.41 | 220.20 / 166.51 | 0.192 |

The Python-backed Arrow19/21rows remain individual outliers:23.2%/18.0%longer best time,25.9%/17.9%longer median time, with0.8%/0.7%lower maximum whole-process RSS. Arrow-backed rows take about19–20%of C++median time with24%lower maximum RSS. Original samples/native identities/source pins are retained under `work/apple/python-cp311-client-measurements-2102/`; current two-ABI index: `corrected-Mac-Python-two-ABI-index-2194/verification.json`. These observations do not establish native TPC-H/ClickBench or whole-client/release performance acceptance.


## Corrected candidate: Python 3.12 (2026-11-03)

Source2078/signed CP312 wheel2098 passes both complete Arrow19/21 profiles and independent2103 review:6,891matching XML records per engine,6,804passes and87retained skips/xfails;420boundaries/24HTTP records per profile;8typed WAL journeys,48encrypted Parquet cases,4installed ADBC environments and8typing runs. NativeSHA256: `478ba255b49b607a44f82ed57d700320b796bada1279e9808aaa85a8001eca83`. Current CP310–312 totals:20,673records per engine/20,412passes/261retained nonpasses; two Mac ABI cells remain pending.

All four CP312 client workloads remain separate (ms; maximum whole-process RSS in decimal MB across three samples):

| Arrow / input storage | C++ best / median ms | Rust best / median ms | C++ / Rust max RSS MB | Rust/C++ median time |
|---|---:|---:|---:|---:|
| 19 / python | 34.42 / 35.84 | 42.30 / 43.42 | 152.40 / 150.81 | 1.211 |
| 19 / pyarrow | 55.99 / 56.70 | 10.61 / 10.96 | 209.52 / 168.94 | 0.193 |
| 21 / python | 35.71 / 36.66 | 42.33 / 44.57 | 150.52 / 149.00 | 1.216 |
| 21 / pyarrow | 57.28 / 58.39 | 10.27 / 10.80 | 207.72 / 167.48 | 0.185 |

Python-backed Arrow19/21rows remain individual outliers:22.9%/18.5%longer best time,21.1%/21.6%longer median time, with about1.0%lower maximum whole-process RSS. Arrow-backed rows take about18–19%of C++median time with19.4%lower maximum RSS. All24raw samples/native identities/source pins remain under `work/apple/python-cp312-client-measurements-2102/`; current three-ABI index: `corrected-Mac-Python-three-ABI-index-2195/verification.json`. These observations are separate from the final native TPC-H/ClickBench policy and do not grant performance/release acceptance.


## Corrected candidate: Python 3.13 (2026-11-03)

Source2078/signed CP313 wheel2098 passes both complete Arrow19/21 profiles and independent2103 review:6,891matching XML records per engine,6,804passes and87retained skips/xfails;420boundaries/24HTTP records per profile;8typed WAL journeys,48encrypted Parquet cases,4installed ADBC environments and8typing runs. NativeSHA256: `86654fbe50c42fb24c5e21d7e41f9c2991f81b3e9313a19be28969f3e2455b74`. Current CP310–313 totals:27,564records per engine/27,216passes/348retained nonpasses; CP314remains pending.

All four CP313 client workloads remain separate (ms; maximum whole-process RSS in decimal MB across three samples):

| Arrow / input storage | C++ best / median ms | Rust best / median ms | C++ / Rust max RSS MB | Rust/C++ median time |
|---|---:|---:|---:|---:|
| 19 / python | 34.15 / 34.67 | 42.59 / 44.08 | 142.36 / 140.30 | 1.272 |
| 19 / pyarrow | 56.82 / 57.95 | 10.41 / 10.60 | 198.02 / 157.45 | 0.183 |
| 21 / python | 34.58 / 35.60 | 42.26 / 43.38 | 141.02 / 138.89 | 1.219 |
| 21 / pyarrow | 57.27 / 57.51 | 10.33 / 10.35 | 196.71 / 156.02 | 0.180 |

Python-backed Arrow19/21rows remain individual outliers:24.7%/22.2%longer best time,27.2%/21.9%longer median time, with about1.5%lower maximum whole-process RSS. Arrow-backed rows take about18%of C++median time with20.5–20.7%lower maximum RSS. All24raw samples/native identities/source pins remain under `work/apple/python-cp313-client-measurements-2102/`; current four-ABI index: `corrected-Mac-Python-four-ABI-index-2196/verification.json`. These observations are separate from final native TPC-H/ClickBench policy and do not grant performance/release acceptance.


## Corrected candidate: Python 3.14 and complete macOS matrix (2026-11-03)

Source2078/signed CP314 wheel2098 passes its complete Arrow22 profile and independent2103 review:3,571matching XML records per engine,3,527passes and44retained skips/xfails;420boundaries,24HTTP records,4typed WAL journeys,24encrypted Parquet cases,2installed ADBC environments and8typing runs. NativeSHA256: `c112c57e510f89a1e6e799f8a7a5d6832ab04ef05590fb241ab2c91174c12cde`.

All five fresh normal macOS CP310–314ABI cells now pass their complete original functional profiles and independent raw reviews:31,135XMLrecords per engine/30,743passes/392retained nonpasses;36typed checkpoint/WAL journeys,216encrypted Parquet cases,18installed ADBC environments,40typing runs,18separate client workloads and108raw coupled samples. Native/source/RECORD/signature/sole-image checks remain explicit. This closes the corrected macOS Python functional matrix; Linux Python and final native TPC-H/ClickBench/performance/release acceptance remain required.

CP314client rows remain separate (ms; maximum whole-process RSS in decimal MB across three samples):

| Arrow / input storage | C++ best / median ms | Rust best / median ms | C++ / Rust max RSS MB | Rust/C++ median time |
|---|---:|---:|---:|---:|
| 22 / python | 34.45 / 35.42 | 43.78 / 44.07 | 170.54 / 169.36 | 1.244 |
| 22 / pyarrow | 48.59 / 48.73 | 10.26 / 10.97 | 227.33 / 186.04 | 0.225 |

The Python-backed row remains an individual outlier:27.1%longer best time,24.4%longer median time and0.7%lower maximum whole-process RSS. The Arrow-backed row takes22.5%of C++median time with18.2%lower maximum RSS. All12CP314samples and every other individual profile remain in `work/apple/python-cp314-client-measurements-2102/` and `corrected-Mac-Python-all-five-ABI-index-2197/verification.json`. No overall client average conceals the Python-backed rows, and no cache or memory budget increase was used. These client observations do not establish macro or whole-release performance acceptance.
