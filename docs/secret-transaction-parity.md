# Secret transaction visibility and rollback

The previous secret manager exposed uncommitted changes to other connections and
kept catalog changes after rollback. Signed-image876 baseline899/900 differed
from C++ DuckDB1.5.6 in60 of256 records across24 two-connection cases. Baseline910
preserves each difference. The complete port goal remains active and incomplete.

Secret entries now carry catalog versions through the existing transaction
manager's undo lifecycle. Reads select the caller's snapshot, writes detect
conflicts with other active or newer committed versions, commit publishes version
timestamps, rollback restores the previous catalog state, and safe cleanup prunes
old versions. Temporary and persistent catalogs use the same mechanism. Metadata
binding passes its transaction explicitly; filesystem secret lookup uses the
active query's transaction context. Secret changes do not produce database WAL
records, matching the distinction between secrets and database objects.

## Independent behavioral comparison

Evidence is under `.context/artifacts/platform-matrix-20261029/` on this workspace's
external SSD. The official authenticated C++1.5.6 Linux AMD64 reference runs under
Rosetta; the tested port is a Mac ARM64 development image. No native AMD64
performance claim is made.

C++909 and Rust906 pass40 cases and all644 SQL/status/row/error records match
literally. Only known package prefixes and the actual unique loopback endpoint
are mapped. Review912 also verifies64 identical file-presence records and280
identical HTTP requests per engine, including paths, methods and received secret
headers. The receiver opens no upstream connections. A query's returned number
comes from the header actually received, so this verifies filesystem use of the
secret as well as its metadata display.

Cases cover both memory and local-file storage:

- Create, replace and drop with commit or rollback, observed from a second
  connection with and without an existing reader snapshot.
- Writer and reader HTTP reads before/after transaction end and reader refresh.
- Competing creates/replacements/drops while the first writer is active, and
  writes from a snapshot predating another connection's commit.
- A drop that cannot see another transaction's new secret, independent writes to
  different names, and reads in aborted transactions.
- Fresh database instances reading the resulting persistent files.

Persistent files deliberately retain the reference behavior: writes/deletions
happen before catalog commit and are not undone by catalog rollback. For example,
a rolled-back create disappears from that instance's catalog but its file is
visible to a fresh instance. A rolled-back replacement restores the old catalog
entry while the file contains the replacement. A rolled-back drop restores the
catalog entry while its file remains deleted. A competing create can write its
file before failing the catalog conflict check. Both engines exhibit these
outcomes in the controls; the port preserves the operation order.

## Same-image verification

Strict workspace/all-target Clippy907 and fresh build903 pass. Shared904 and
SDK905 pass normal strict macOS codesigning and whole-file CMS authentication
with the caller-pinned certificate. Native exports remain547 duckdb-prefixed
symbols without private ADBC test hooks. Shared SHA-256:
`f1c9172080478a6326f76b27dd6110949cc8493f619a05324df9df19a21c0ccd`.

The prior format-interoperability journeys pass again on this image:17 stages,
105 SQL records, nine literal paired comparisons, C++/Rust reopen/replace/drop,
extension-free reads and unchanged legacy files until explicit rewrite. Evidence:
journeys911 and review928. Strict901/902 failures (missing provider argument and
collapsible-if style) are retained and fixed before successful907/903.

Pipeline923 guards1,487 code/test inputs before/after each step. Reviews924–928
preserve these regressions on the same signed image:

| Gate | Result | Evidence |
| --- | --- | --- |
| S3 region/version and remote COPY | 22 cases,245 literal records match | versions914,review928 |
| TLS/HuggingFace | 31 cases,293 literal records match | TLS915,review924 |
| S3 read/list/limit | 30 cases,276 literal records match | reads918,review925 |
| HTTP | 16 cases,121 literal records match | HTTP919,review925 |
| Upload | 35 functional records match | writes917,review926 |
| Partition/delete | 30 SQL+11 SDK records match | partitions916,review927 |
| Extension controls | 59 cases,246 records unchanged | extensions920,review925 |
| Full public C API | 30 files,155 cases,745130 assertions pass | CAPI921 |
| Public trust-root metadata | 12 literal records match | roots922,review928 |

Assertion counts include workload-dependent loops and are not a progress metric.
The upload allocation sample remains16,515,072 EXTENSION bytes on both engines,
zero before/after; this is not peak RSS. The21 tracked buffer/cache/batch-memory/
Parquet COPY inputs remain unchanged from source897. Version histories provide
transaction correctness and use the transaction manager's safe cleanup; no
object/table/query cache or memory-budget increase was added.

## Remaining gates

These controls establish the listed transaction semantics, not complete secret-
manager parity. Disconnect/cancellation and failed-commit journeys, extended
concurrency/cleanup stress, custom serializers, malformed-file/error behavior,
other filesystem/configuration cases and required-platform runtime coverage
remain to verify. Existing body-only S3 version capture,301 Location and proxy
wire differences remain documented separately.

Final common-source signed release artifacts still require full SQL-corpus and
shell tests, C API/ADBC and Python/Go/Swift matrices on required platforms,
cross-engine database/WAL journeys and coupled native TPC-H/ClickBench speed/RSS
measurements with individual query outliers and documented lower-memory acceptance.
Required platforms remain macOS ARM64, iOS ARM64 devices/simulators and Linux
ARM64/x86_64. Mac Intel and additional clients are outside scope. Component
verification does not complete the full goal.
