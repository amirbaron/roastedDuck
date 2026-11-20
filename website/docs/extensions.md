# Extensions

!!! note "Available in 1.5.6.post4"
    roastedDuck's implementations of httpfs, aws, azure, icu and postgres_scanner
    are bundled in the post4 Homebrew and PyPI distributions, next to the FTS and
    VSS implementations added in post3. See [installation](install.md) for the
    package commands.

## Bundled extensions

| Extension | Load with | Provides |
| --- | --- | --- |
| httpfs | `LOAD httpfs;` | HTTP(S), S3, R2, GCS and HuggingFace (`hf://`) reads; S3 writes and globbing; HTTP and S3 secrets; metadata and connection caching |
| aws | `LOAD aws;` | `PROVIDER credential_chain` for S3 secrets: environment, profiles, processes, SSO, STS assume-role, web identity, container and instance metadata; `load_aws_credentials` |
| azure | `LOAD azure;` | `az://`, `azure://`, `abfs://` and `abfss://` reads, writes and globbing; connection strings, SAS, SharedKey and the Azure identity providers |
| icu | `LOAD icu;` | Collations such as `COLLATE de`, time zones, calendars and ICU date functions |
| postgres_scanner | `LOAD postgres;` | `ATTACH ... (TYPE postgres)`, `postgres_scan`, `postgres_query`, `postgres_execute`, writes and DDL, pooling, RDS IAM and OAuth tokens |
| fts | `LOAD fts;` | Full-text indexes and BM25 ranking |
| vss | `LOAD vss;` | In-memory HNSW vector indexes |

All of them are compiled into normal builds. No extra package, downloaded
extension binary, extension repository or writable extension cache is needed, and
official C++ extension binaries are not used. A fresh database starts with them
installed but not loaded; repeated `LOAD` is safe. Bare `INSTALL` and
`FORCE INSTALL` succeed offline for these names, and `FROM` or `VERSION`
overrides are rejected: upgrade roastedDuck to update them.

With `SET autoload_known_extensions=true`, a query that uses a function, file
scheme, secret type or setting of one of these extensions loads it, as in DuckDB.
That includes `SET`, `RESET` and `current_setting()` on an extension setting.

## Avro

The 1.5.6.post5 source includes a bundled Avro extension. Install and load it
without an extension download.

```sql
INSTALL avro;
LOAD avro;
COPY (SELECT 1 AS id, 'open' AS status) TO 'events.avro' (FORMAT AVRO);
SELECT * FROM read_avro('events.avro', filename=true);
```

`read_avro` accepts file paths, globs and lists of paths. Use `LOAD httpfs`
for HTTP and S3 paths. The reader uses the embedded schema, flattens top-level
records into SQL columns and supports null, Deflate, Snappy, LZMA and Zstandard
blocks. Files in one call must have matching output schemas. Hive partitions,
`filename` and `file_index` are available.

Write with `COPY ... TO`, using optional `ROOT_NAME`, `FIELD_IDS` and
`METADATA`. Writes use uncompressed Avro blocks. Read into an existing table
with `INSERT INTO target SELECT * FROM read_avro(...)`. Recursive schemas,
`union_by_name`, separate reader schemas and `COPY ... FROM` are outside the
DuckDB 1.5.6 Avro contract. [DuckDB 1.5 Avro reference](https://duckdb.org/docs/1.5/core_extensions/avro).

## Remote files

```sql
LOAD httpfs;
SELECT count(*) FROM 'https://example.com/events.parquet';

LOAD aws;
CREATE SECRET s3 (TYPE s3, PROVIDER credential_chain);
SELECT count(*) FROM read_parquet('s3://my-bucket/events/*.parquet');
COPY (SELECT * FROM events) TO 's3://my-bucket/export/events.parquet';
```

Reads use range requests with an adaptive read buffer, and the external file cache
serves repeated reads of unchanged files. `SET force_download=true` downloads whole
files instead. HTTP and S3 errors carry the same text as DuckDB's, including the
region and credential hints for S3.

```sql
LOAD azure;
CREATE SECRET az (TYPE azure, CONNECTION_STRING 'DefaultEndpointsProtocol=https;AccountName=...;AccountKey=...');
SELECT count(*) FROM 'az://container/events/*.parquet';
```

Azure secrets also accept `PROVIDER credential_chain` (environment, workload
identity, managed identity and Azure CLI) and `PROVIDER service_principal`.

## PostgreSQL

```sql
LOAD postgres;
ATTACH 'host=localhost dbname=app user=reader' AS pg (TYPE postgres, READ_ONLY);
SELECT * FROM pg.public.orders WHERE created_at > now() - INTERVAL 1 DAY;
SELECT * FROM postgres_query('pg', 'SELECT version()');
```

The client speaks the PostgreSQL wire protocol in Rust, with TLS (including
`verify-full` and CRLs), SCRAM and channel binding; libpq is not used. Connection
strings, `PG*` environment variables, service and password files, `postgres`
secrets and the `pg_*` settings follow the pinned extension.

## Time zones and collations

```sql
LOAD icu;
SET TimeZone = 'Europe/Amsterdam';
SELECT now(), name FROM people ORDER BY name COLLATE de;
```

The ICU data is embedded in the binary.

## What was tested

Each extension's original SQL tests from its pinned upstream source were run
unchanged on DuckDB 1.5.6 and on roastedDuck, against local MinIO, Azure,
PostgreSQL 17, STS/SSO, metadata and proxy fixtures:

| Extension | Original test files | Pass on both engines | Other verdicts |
| --- | ---: | ---: | --- |
| icu | 63 | 63 | none |
| azure | 27 | 27 | none |
| aws | 21 | 20 | one credential-refresh test fails the same way on both |
| postgres_scanner | 152 | 145 | seven skip on both: missing PostGIS, RDS, OAuth-service and concurrency prerequisites |
| httpfs | 132 | 116 | 8 disabled upstream or setup-only; 3 with the same non-pass on both; 1 out of memory only on DuckDB; 4 expect an install from a missing repository to fail |

These are local fixtures, not live AWS, Azure, Google Cloud, Redshift or Aurora
accounts. The remaining differences are listed in the
[compatibility guide](compatibility.md#bundled-extensions).

## FTS and VSS

### Load FTS and VSS

```sql
LOAD fts;
LOAD vss;
```

Both are compiled into normal builds. No extra package, downloaded extension
binary, extension repository, or writable extension cache is needed. They are
installed but initially unloaded in a fresh database instance. Repeated `LOAD`
is safe; applications can execute it during connection initialization. Execute
`LOAD` as a separate startup statement before submitting a batch containing
extension pragmas (or call Python's `load_extension` first).

For scripts that already install first, `INSTALL fts; INSTALL vss;` is supported.
Plain `INSTALL` and `FORCE INSTALL` acknowledge the bundled implementations without
network access and do not load them. Run `LOAD` afterward. Update the engine to
update these implementations: `FROM` and `VERSION` overrides for their bundled
names are rejected with guidance. This bundled policy is intentional; it does not
change installation of other extensions or explicit file paths.

```sql
SELECT extension_name, installed, loaded, install_mode
FROM duckdb_extensions()
WHERE extension_name IN ('fts', 'vss');
```

`install_mode` is `STATICALLY_LINKED`. A fresh application process must load the
implementations again, even when opening an existing FTS database. Extension load
state is not saved into the database file. The same SQL works through the CLI,
Python, Go, Swift, C API, and ADBC when those clients are built with this engine.

Explicit loading is the recommended application startup path. FTS functions such
as `stem` can also autoload after `SET autoload_known_extensions=true`; the
existing global default is unchanged. VSS is outside the pinned autoload allowlist:
use `LOAD vss` explicitly before its functions, settings or HNSW index creation.

### Full-text search

Run this against a new database, either in memory or in a file:

```sql
-- Run against a new database. FTS data can be saved in a database file.
LOAD fts;
CREATE TABLE documents(id INTEGER PRIMARY KEY, body VARCHAR);
INSERT INTO documents VALUES
    (1, 'a red fox running through the forest'),
    (2, 'a blue bird above the ocean');
PRAGMA create_fts_index('documents', 'id', 'body');
SELECT id, body, fts_main_documents.match_bm25(id, 'fox') AS score
FROM documents
WHERE score IS NOT NULL
ORDER BY score DESC, id;
-- After changing documents, refresh the snapshot with:
-- PRAGMA create_fts_index('documents', 'id', 'body', overwrite=true);
-- PRAGMA drop_fts_index('documents');
```

FTS indexing creates ordinary tables and macros in `fts_main_documents`.
`match_bm25` returns NULL for no match; filter those rows and order scores descending.
The index is a snapshot: rebuild with `overwrite=true` after document changes.
An FTS database can be reopened by roastedDuck or the pinned C++ DuckDB 1.5.6
with its matching FTS implementation loaded.

### Vector search

Start with an in-memory database (`roastedDuck :memory:`):

```sql
-- Run against :memory:. Persisted HNSW creation is not implemented.
LOAD vss;
CREATE TABLE items(id INTEGER PRIMARY KEY, embedding FLOAT[3]);
INSERT INTO items VALUES (1, [1, 0, 0]), (2, [0, 1, 0]), (3, [0, 0, 1]);
CREATE INDEX items_hnsw ON items USING HNSW(embedding) WITH (metric='cosine');
SET hnsw_ef_search=200;
SELECT id, array_cosine_distance(embedding, [1, 0, 0]::FLOAT[3]) AS distance
FROM items
ORDER BY distance
LIMIT 1;
-- Inspect acceleration with EXPLAIN before the SELECT above.
-- SELECT * FROM pragma_hnsw_index_info();
-- PRAGMA hnsw_compact_index('items_hnsw');
-- DROP INDEX items_hnsw;
```

Use fixed-size `FLOAT[N]`, not an unconstrained list or `DOUBLE[N]`. Index metrics
are `l2sq` (default), `cosine`, and `ip`; use the matching SQL distance expression
in `ORDER BY ... LIMIT`. HNSW is approximate. Increasing `hnsw_ef_search` can
improve recall at greater query cost; 200 is the measured acceptance profile,
not the default and not a per-query accuracy guarantee. Set it on each connection.

`EXPLAIN` exposes an eligible HNSW scan. `EXPLAIN ANALYZE` uses a `TABLE_SCAN`
profiler label with the actual `HNSW Index` attribute. Transaction-local writes
may require an explicitly reported exact fallback. `pragma_hnsw_index_info()`
reports actual graph metadata; `hnsw_compact_index` reclaims deleted graph nodes.
The `vss_join` and `vss_match` macros are also available.

HNSW creation currently requires an in-memory database. Setting
`hnsw_enable_experimental_persistence=true` does not implement persistence.
Persist source vectors elsewhere and rebuild the in-memory graph on startup.
Databases containing C++ HNSW indexes can be read with those indexes left unbound;
this does not provide accelerated access to their persisted graphs. Unsupported
mutation/checkpoint binding is rejected.

### Python application startup

For `roastedduck` 1.5.6.post3 or later (FTS and VSS were added in post3):

```python
import duckdb

con = duckdb.connect(":memory:")
con.load_extension("fts")
con.load_extension("vss")
con.execute("SET hnsw_ef_search=200")
# Execute the SQL examples above through con.execute(...).
con.close()
```

No separate `pip install` for FTS or VSS is needed. Go, Swift, C API and ADBC
applications can execute the same `LOAD` statements as part of startup. These
examples share the engine's SQL contract; this page does not claim a new complete
qualification of every client package or platform.

### Troubleshooting

| Symptom | Action |
| --- | --- |
| Function or HNSW index type is missing | Run `LOAD fts` or `LOAD vss`; check the engine came from a source build containing these implementations. |
| `INSTALL ... FROM ...` or `VERSION ...` is rejected | Use the bundled name with `LOAD`; upgrade roastedDuck to update it. |
| Persistent HNSW creation is rejected | Open `:memory:` and rebuild the graph there. FTS can use a persistent file. |
| FTS results omit new documents | Rebuild the FTS snapshot with `overwrite=true`. |
| HNSW query does not use the index | Check fixed FLOAT dimension, matching distance metric, ORDER BY/LIMIT shape and EXPLAIN. |
| A `.duckdb_extension` C++ file is rejected | These are roastedDuck's compatible implementations; C++ extension binaries are not used. The stable C bridge is separate. |

## Extension development

These implementations stay in the engine repository, with standalone Rust
algorithm crates and internal engine integration. A separate Git repository would
not create a compatible binary/plugin interface. Maintainers can change index
lifecycle, optimizer hooks and extension code in one reviewed change.

See the [extension inventory and contributor workflow](https://github.com/amirbaron/roastedDuck/tree/main/extensions),
[FTS contract](https://github.com/amirbaron/roastedDuck/blob/main/docs/fts-extension-spec.md),
[VSS contract](https://github.com/amirbaron/roastedDuck/blob/main/docs/vss-extension-spec.md),
and [runnable examples](https://github.com/amirbaron/roastedDuck/tree/main/examples/extensions).
