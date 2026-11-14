# Architecture

roastedDuck is an in-process analytical database. SQL is parsed and bound into
a logical plan, optimized, and executed through vectorized operators. Storage,
buffer management, transactions, and codecs sit beneath that execution layer.

```text
SQL / Python / Go / Swift / C API / ADBC
                    │
             parser and binder
                    │
          logical plan and optimizer
                    │
         vectorized execution pipelines
                    │
      catalog · transactions · storage
                    │
        buffers · codecs · file access
```

| Area | Source |
| --- | --- |
| Values, vectors, errors | `crates/types`, `crates/common` |
| SQL syntax and planning | `crates/ast`, `crates/parser`, `crates/planner` |
| Optimization and execution | `crates/optimizer`, `crates/execution`, `crates/row` |
| Database API and catalog | `crates/roastedduck`, `crates/catalog` |
| Persistence and memory | `crates/storage`, `crates/buffer` |
| File formats and extensions | `crates/parquet`, `crates/csv`, `crates/json`, `crates/httpfs` |
| Clients and shell | `crates/capi`, `crates/python`, `crates/shell` |
| Differential testing | `tools/slt`, `scripts/`, `conformance/` |

The engine is implemented in Rust. Native cryptography dependencies are allowed;
this is not a claim that the complete dependency graph is Rust or free of unsafe
code. The upstream C++ engine is used as a reference for testing, not as the
runtime engine in the release payload.

The public CLI package is `roastedduck-cli`, producing `roastedDuck`. Rust engine
crates and modules use `roastedduck-*` and `roastedduck_*`; release versions match the targeted DuckDB
compatibility version.

Read the [detailed engineering conventions](https://github.com/amirbaron/roastedDuck/blob/main/docs/architecture.md)
before changing operator, memory-accounting, or persistence behavior.


FTS and VSS are bundled implementations maintained in this repository. Their
algorithm crates (`crates/snowball` and `crates/hnsw`) are separate from their SQL
and storage integration. Internal Rust hooks evolve atomically with the engine;
there is no independently versioned Rust plugin ABI. See the [extension guide](extensions.md).
