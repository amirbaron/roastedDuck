# HTTPFS compatibility checks

These scripts qualify roastedDuck's compatible implementation against the pinned
DuckDB 1.5.6 oracle. They do not require an AWS account. The source inventory and
original SQL hashes are in `extensions/httpfs/upstream.json`.

`upstream_metadata.py` runs original metadata and secret tests, including the
historical secret files shipped with the pinned source.

`upstream_s3.py` runs explicitly selected, unchanged original SQL against an owned
MinIO instance. It validates original SQL hashes and the source archive hash,
resets only its own buckets between files, and saves per-engine SLT results.
Every service uses loopback and separate data/configuration directories; no user
MinIO configuration, system DNS, routes, Docker state, or cloud resources change.
The original fake credentials are disposable and intentionally recorded.

Required artifacts:

- Signed `slt` and the signed C++ SLT shim.
- Extracted pinned HTTPFS source with its original `httpfs.tar.gz` two levels up.
- Pinned DuckDB core source/data and authenticated oracle extension packages.
- Signed native `minio` and `mc` tools matching the tags in upstream's service recipe.
- On macOS, the signed test-only `scripts/aws-tests/oracle_transport.c` adapter,
  compiled as a dynamic library with Security, Network and CoreFoundation frameworks.
  `ROASTEDDUCK_FIXTURE_S3_PORT` maps only the original MinIO hostnames/port to the
  owned service inside the test child. It does not change the signed Host header.

Example (set paths to the prepared artifacts, and keep output on the external SSD
in local Conductor workspaces):

```sh
python3 scripts/httpfs-tests/upstream_s3.py \
  --slt "$slt" --shim "$shim" --source "$httpfs_source" --core "$core_source" \
  --extensions "$oracle_packages" --tools "$minio_tools" \
  --transport "$transport_dylib" --out "$new_output_directory" \
  --file test/sql/copy/s3/upload_small_file.test \
  --file test/sql/copy/s3/url_encode.test
```

The runner fails for any failed or skipped original. Inspect `verification.json`,
not just SLT's exit code. Failed runs are retained. Originals that need presigned
URLs, pre-generated databases, proxies or public datasets need those additional
fixtures; this runner does not silently replace them or count missing prerequisites
as passes.

`generate_s3_seeds.py` creates the database and Parquet prerequisites with the
pinned C++ CLI and authenticated TPCH package. It checks the core SQL against
`--core-manifest` and records generated file hashes. Pass its output with
`upstream_s3.py --seeds`. Small presigned fixtures come from the pinned core data
and are checked against its adjacent manifest. Presigned URLs use the real MinIO
share command. Python-server originals get an owned standard HTTP server without
Range support, matching their upstream prerequisite.

For originals that intentionally query public services, `--public-network`
disables child-local transport mapping and labels the report accordingly. Such
results qualify the observed public endpoint, not an owned emulator. Keep shared
service failures, skips and upstream memory-limit failures explicit.

`audit_originals.py` combines ordered receipts and separately lists files whose
SQL is entirely disabled by upstream `mode skip`. Their raw SLT pass verdicts do
not count as exercised coverage. The audit is a retained per-file inventory, not
a fresh sweep of one frozen candidate.

`metadata_cache_control.py` supplements the originals with two-connection cache
boundaries and actual versioned MinIO objects. Run it separately for each signed
C API library, supplying `--library`, `--package`, `--tools` and a fresh `--out`.
Its assertions first run against the C++ oracle. Existing `s3_*_control.py` scripts
also check signatures, redirects, refresh, partitions and version behavior using
bounded loopback protocol fixtures.

`download_control.py` compares forced downloads, strict threshold boundaries,
range retries/fallback, invalid HEAD sizes and empty files with independent
request receipts. Compare its C++ and Rust records with only owned ports and
the engine build-source ID normalized. Native download tests additionally verify
spill-backed reads across block boundaries and buffer release after disconnects.

`logging_control.py` checks mixed-case and duplicate response headers, redirect
responses, and the deprecated logging setting's manager/client interaction. It
records the server's requests independently. `diagnostics_control.py` covers CA
loading, glob errors, cold/warm secret storage with disabled filesystems, and
prepared secret enumeration. Preserve its verification-enabled cold-bind
difference until engine query verification is implemented; successful error-status
assertions alone do not prove matching error text or state transitions.

`connection_cache_control.py` uses an owned HTTP/1.1 keep-alive server to identify
accepted TCP connections independently of engine logs. Its comparisons cover
shared-database connections, credentials, timeout changes, proxy requests,
disable/re-enable and backend resets. Cache-event matches alone are insufficient:
the server must also observe the expected connection reuse. Only owned endpoint
ports are normalized in the paired record comparison.

`read_buffer_control.py` serves owned blob, text, CSV, JSON and Parquet files
and records every request. It compares the handle's read buffer, cached
`read_blob` reads and the ranges of `duckdb_external_file_cache()` between the
two libraries. Pass `--parquet` a pinned multi-megabyte Parquet file and
`--extensions` the directory of authenticated oracle packages; compare the
`records` of the two `verification.json` files with only the loopback port
normalized.

