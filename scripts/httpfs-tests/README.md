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

`metadata_cache_control.py` supplements the originals with two-connection cache
boundaries and actual versioned MinIO objects. Run it separately for each signed
C API library, supplying `--library`, `--package`, `--tools` and a fresh `--out`.
Its assertions first run against the C++ oracle. Existing `s3_*_control.py` scripts
also check signatures, redirects, refresh, partitions and version behavior using
bounded loopback protocol fixtures.
