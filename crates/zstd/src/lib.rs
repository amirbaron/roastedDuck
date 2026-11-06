//! zstd, ported from zstd 1.5.6 (the version DuckDB vendors). The compressor writes the
//! frames zstd writes at the same level, for one-shot compression (`ZSTD_compress`) and for
//! buffered streaming (`ZSTD_compressStream2`); the decoder (`lib/decompress`) reads any
//! frame without a dictionary, whole (`ZSTD_decompress`) or streamed
//! (`ZSTD_decompressStream`).
//!
//! The `ZSTD_fast` and `ZSTD_dfast` match finders are ported, with and without an extDict
//! (the window of a stream that wrapped around its input buffer). Hash-chain
//! Greedy/Lazy/Lazy2 parsing uses hash chains or salted row hashes.
//! Delayed binary-tree lazy parsing is also implemented. Optimal parsing and
//! entropy-cost block splitting are under parity verification.
//! The compressor supports no attached dictionaries, checksums or multi-threading.
//! Automatic long-distance matching at the largest windows remains under parity
//! work; the frozen encoder enables it for optimal strategies with window log 27.

#![allow(
    clippy::inline_always,
    reason = "the hot loops of the match finders and entropy coders must inline these"
)]

mod bits;
mod block;
mod bt_lazy;
mod cctx;
mod decompress;
mod dfast;
mod error;
mod fast;
mod fse;
mod hist;
mod huf;
mod lazy;
mod ldm;
mod ldm_gear;
mod literals;
mod opt;
mod opt_tree;
mod params;
mod row;
mod seq;
mod sequences;
mod split;
mod window;

pub use cctx::{Compressor, EndDirective, compress, compress_streamed};
pub use decompress::{
    Decompressor, StreamDecoder, decompress, find_frame_compressed_size, frame_content_size,
};
pub use error::Error;
pub use params::{DEFAULT_LEVEL, MAX_LEVEL, MIN_LEVEL, compress_bound};
