# FTS and VSS

!!! note "Current source builds"
    This page describes roastedDuck's compatible implementations in current source.
    It does not claim the previously published `1.5.6.post2` packages include them.
    Build the engine from this source branch to use these additions.

## Load the bundled implementations

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

## Full-text search

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

## Vector search

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

## Python application startup

For a Python package built from this source:

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

## Troubleshooting

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
