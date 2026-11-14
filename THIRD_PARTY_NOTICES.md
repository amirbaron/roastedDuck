# Third-party notices

roastedDuck's own code is available under the [MIT license](LICENSE).
Third-party components retain their original licenses; the top-level license
does not replace those terms or grant rights to third-party trademarks.

| Component | Origin / retained notice |
| --- | --- |
| DuckDB algorithms, interfaces, fixtures, Python/Swift helpers | Copyright 2018–2026 Stichting DuckDB Foundation; MIT. [Retained upstream text](python/LICENSE-UPSTREAM). Compatibility target: 1.5.6. |
| DuckDB Go client | Pinned in `go/upstream.json`; its MIT license is retained in each distributed source copy. |
| Snowball stemmers and byte runtime | Pinned Snowball algorithms translated to Rust; [BSD-3-Clause notice](crates/snowball/LICENSE). Provenance and regeneration are recorded in [the crate](crates/snowball/README.md). |
| DuckDB FTS indexing SQL and stopwords | Pinned `duckdb-fts` source; [retained MIT notice](crates/roastedduck/src/fts/LICENSE). |
| DuckDB VSS table macros | Pinned `duckdb-vss` SQL; [retained MIT notice](crates/roastedduck/src/vss/LICENSE). The HNSW graph is independently implemented in Rust. |
| ICU data and implementation | [ICU license](crates/icu/LICENSE-ICU) |
| utf8proc data | [utf8proc license](crates/common/data/utf8proc-LICENSE) |
| Zstandard | [Retained ZSTD license](crates/zstd/LICENSE-ZSTD) |
| TPC-H dbgen / TPC-DS dsdgen | [TPC-H notice](crates/tpch/LICENSE), [TPC-DS notice](crates/tpcds/LICENSE). Measurements here are engineering workloads, not audited TPC benchmark results. |
| Apache Arrow ADBC interface | [Apache 2.0](crates/capi/src/adbc/LICENSE-APACHE), [NOTICE](crates/capi/src/adbc/NOTICE) |
| Python experimental Spark compatibility helpers | [Retained Apache license](python/duckdb/experimental/spark/LICENSE) |
| Modified miniz_oxide | [Notices](third_party/miniz_oxide/LICENSE), MIT / Apache-2.0 / zlib texts in that directory |
| Modified ureq-proto | [MIT](third_party/ureq-proto/LICENSE-MIT.txt), [Apache-2.0](third_party/ureq-proto/LICENSE-APACHE.txt) |
| Native cryptography | OpenSSL, ring, and their bundled license texts are retained with the generated dependency notices. |
| Other Rust dependencies | Versions and source identifiers are pinned in `Cargo.lock`; distributions include generated dependency-license notices. |
| Documentation theme | Material for MkDocs, MIT; installed by `website/requirements.txt`. |

Keep the notices included with each release when redistributing it. Some
compatibility identifiers, including the `duckdb` Python import and `libduckdb`
ABI, intentionally retain upstream names. They do not indicate upstream endorsement.
