# miniz_oxide (vendored for roastedDuck)

Unmodified miniz_oxide 0.9.1 (MIT / Zlib / Apache-2.0, see the LICENSE files) except
for one change in `src/deflate/core.rs` (`flush_block`): a block that does not shrink is
written as a stored block whatever its size, as C miniz's `tdefl_flush_block` does.
DuckDB compresses gzip files and Parquet GZIP pages with C miniz, so this keeps roastedDuck's
compressed output byte-identical to DuckDB's.

The workspace uses it through `[patch.crates-io]` in the root `Cargo.toml`.
