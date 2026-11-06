# Persistent key/value secret interoperability

This checkpoint fixes a concrete C++ DuckDB 1.5.6 compatibility failure. The old
signed Rust image847 could not reopen C++ persistent secrets: deserialization
failed with field-id mismatch100/25600. Isolated baseline873 reproduces that
failure. All fixture credentials are synthetic; no normal user secret directory
was changed.

New persistent files now use C++'s representation: base fields100–103, a typed
`MAP(VARCHAR, ANY)` value in field201, and a `LIST(VARCHAR)` value in field202 for
redaction keys. The reader handles the default key/value serialization type and
its optional field104. It retains the port's previous list-format reader for
backward compatibility. New writes use the C++ representation exclusively.

Legacy reads do not rewrite files. Explicit replacement writes the compatible
format. C++ still cannot read an untouched legacy port file; those files need
replacement before C++ can consume them. This checkpoint does not add an implicit
migration of a user's directory.

## Independent cross-engine evidence

Evidence root: `.context/artifacts/platform-matrix-20261029/` on the dedicated
external SSD. The C++ reference is authenticated official1.5.6 Linux AMD64 under
Rosetta; the tested port image is Mac ARM64 development876. Strict workspace/
all-target Clippy874 and fresh build875 pass. Whole-file pinned-certificate CMS
checks authenticate engines/packages before and after; native Mac images also
pass normal strict codesigning. Shared library876 SHA-256:
`fef9d991a31c1a8f49a64c92af9539e4b14ff81c519b2c3e611919d71b48bc1a`.

Eight fixtures cover empty HTTP maps, strings, booleans, nested extra-header maps,
multiple/empty scopes, mixed-case names, HTTP proxy credentials, S3 session/KMS/
requester-pays parameters and AWS/R2/GCS/HuggingFace types. Each file retains0600
permissions and leaves no temporary output after successful writes.

Journeys878 pass **17 stages / 105 SQL records**:

- Reopen C++ files in Rust, replace them in Rust, then reopen them in C++.
- Create Rust files, reopen and replace them in C++, then reopen them in Rust.
- Read key/value files without loading HTTPFS on both engines.
- Read preserved old-Rust files without modifying their hashes; explicitly
  replace them and reopen the resulting files in C++.
- Drop files using either engine and confirm their absence from a fresh instance
  of the other engine. Drop controls operate on separate copies of the fixtures.

Review889 compares nine pairs of corresponding stages literally, with no
differences. Only exact package prefixes and each actual encoded loopback endpoint
are mapped; SQL, statuses, values and error text remain intact. Old/new Rust
creation records also match. Request counts remain separate.

A loopback S3 receiver verifies22 actual requests using restored access keys,
secrets, session tokens, region and requester-pays headers, including SigV4 over
the actual request. Queries return the expected data. This checks recovered
credentials beyond the redacted metadata table. No upstream service is contacted.

## Regressions on the same signed image

Pipeline888 guards1,485 code/test inputs before and after each step. Same-source
SDK877 is also authenticated for filesystem controls. Reviews890–895 preserve:

| Gate | Result | Evidence |
| --- | --- | --- |
| S3 region/version and remote COPY | 22 cases, 245 literal records match C++ | versions879, review890 |
| TLS/HuggingFace | 31 cases, 293 literal records match | TLS880, review891 |
| S3 read/list/limit | 30 cases, 276 literal records match | reads883, review892 |
| HTTP | 16 cases, 121 literal records match | HTTP884, review892 |
| Upload | 35 functional records match | writes882, review893 |
| Partition/delete | 30 SQL and11 SDK records match | partitions881, review894 |
| Extension controls | 59 cases, 246 records unchanged | extensions885, review892 |
| Full public C API | 30 files, 155 cases, 746432 assertions pass | CAPI886 |
| Public trust-root metadata | 12 literal records match | roots887, review895 |

Assertion counts include workload-dependent loops, not a progress metric. The
upload allocation sample remains16,515,072 EXTENSION bytes on both engines with
zero before/after; this is not peak RSS. All21 tracked buffer/cache/batch-memory/
Parquet COPY inputs remain unchanged from source869. No object/table/query cache
or memory-budget increase was introduced.

## Remaining scope

The common transaction visibility/rollback/conflict cases are now verified in
[secret transaction parity](secret-transaction-parity.md). Transaction lifecycle/
stress coverage, custom secret serializers,
malformed-file/error details, additional filesystem edge cases and required
Linux/iOS runtime coverage remain open. This is persistent key/value format
interoperability, not complete secret-manager parity. The existing S3 body-only
version/301 Location/proxy differences remain documented separately.

The whole goal remains active and incomplete. Required clients are Python, Go
and Swift, preserving C API/ADBC; final targets are macOS ARM64, iOS ARM64 devices/
simulators and Linux ARM64/x86_64. Mac Intel and additional clients remain outside
scope. Final common-source signed release artifacts still need the full SQL
corpus, shell, client/platform/ADBC matrices, cross-engine database/WAL journeys
and coupled native speed/RSS measurements with per-query outliers and documented
acceptance of the lower-memory tradeoff. Component results do not satisfy those
release gates.
