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
