# Local Azure compatibility tests

These scripts use Azurite and disposable fake credentials. They never use a real
Azure account or change the user's cloud profiles. Install the tools with
[`../local-services/setup.py`](../local-services/setup.py) first. Keep tools,
service state and output on the workspace's external artifact volume.

`upstream_blob.py` copies original tests and data from the source pinned in
`extensions/azure/upstream.json`, checks the test hashes, uploads the data to
fresh Azurite storage for each engine, and runs the unchanged SQL on both engines.
Supply `--file test/sql/example.test` repeatedly to choose a cohort. Without
`--file`, it runs secret, scope/full-path, glob and write originals.

```sh
python scripts/azure-tests/upstream_blob.py \
  --slt /path/to/signed/slt \
  --shim /path/to/libslt_oracle.dylib \
  --source /path/to/pinned/duckdb-azure \
  --extension /path/to/oracle/azure.duckdb_extension \
  --tools "$PWD/.context/artifacts/local-services" \
  --out "$PWD/.context/artifacts/azure-run-unique"
```

`blob_control.py` compares normal loading, escaped object names, SharedKey/SAS
reads, invalid-key rejection, secret metadata/redaction and SAS log redaction.
It takes `--cli`, `--oracle`, `--extension`, `--tools` and `--out`; use `--help`.
The Python interpreter must be the local-services virtual environment's Python.

Both scripts stop their services on completion and save `verification.json` with
raw outcomes and executable hashes. A nonzero exit preserves failures. Azurite
does not emulate ADLS Gen2 or Azure identity providers; passing these checks does
not establish those capabilities or live-cloud acceptance. The C++ extension is
used only by the separate oracle process.

`dfs_control.py` runs 12 paired ADLS SQL controls against `dfs_fixture.py`, a separate
loopback REST fixture. It verifies SharedKey signatures, hierarchical listings,
continuation pages, sequential append positions and visibility on flush. Tests
cover `abfs://` and `abfss://`, reads, the upstream glob restrictions, intermediate
commits, close=true and partitioned Parquet. It takes `--cli`, `--oracle`,
`--extension` and `--out`; `--engine cpp` can qualify the fixture independently.
This is protocol-fixture acceptance, not an Azurite ADLS capability or live Azure.

`identity_control.py` runs paired identity controls using an owned TLS certificate,
local token/storage services, and a fake `az` executable. It verifies OAuth fields,
RSA assertion signatures/thumbprints/claims, workload assertions, managed-identity
selectors, fallback ordering, CLI environment/arguments, cache behavior for expired
legacy timestamps, and relevant failures. No system trust store, cloud profile or
real Azure CLI installation is changed. It takes the same executable arguments as
`dfs_control.py`. Use the local-services virtual environment (cryptography is required).

The Unix CLI helper's restricted environment and acceptance of valid JSON on
nonzero exit match the pinned SDK. Direct managed_identity deliberately ignores
the custom Azure CA setting, as the pinned source does; its positive metadata
fixtures use ordinary loopback HTTP and storage remains TLS. The test also proves
that the custom CA does not silently become trusted by that provider.

`scripts/aws-tests/interrupt_control.py --extension azure` verifies cancellation of
the real SQL lookup, including helper descendants. Pass `--api-probe` pointing at
the signed `aws_interrupt_probe` example to verify the actual Rust ErrorKind as
well as the native CLI behavior.

`upstream_cloud.py --all` runs all 27 original Azure files, including the four
fixed-port proxy tests, both large/concurrent write suites and data integrity.
It uses owned TLS/token/storage services and a CONNECT proxy restricted to the
fixture hostnames. Ports 3128 and 3129 must be free; the script never stops another
service to obtain them. Original SQL and data remain unchanged and are hashed.
The integrity case uses real persistent secret files in an owned directory and
sets both of its upstream prerequisite environment variables.

The test runner's `SLT_AZURE_TRANSPORT=curl` selects the ordinary transport option
on both engines so the fixture CA is trusted through `CURL_CA_INFO`. The optional
`SLT_SECRET_DIRECTORY` selects an owned persistent-secret directory. Neither changes
the engine's product defaults. Initial object timestamps come from the original
data files; successful writes update timestamps and version tags. Cloud listings
use the service's normal page size; the smaller pagination fixture remains separate.

`transfer_control.py` compares both URL schemes for read-buffer bypass, chunk sizes,
concurrency (including the pinned zero/negative behavior), exact bytes and a file
overwritten between chunks. It checks the `If-Match` guards and requires failure
on a changed ETag. All fixture exceptions are bounded and included in receipts.
