//! zstd, ported from zstd 1.5.6 (the version DuckDB vendors). The compressor writes the
//! frames zstd writes at the same level, for one-shot compression (`ZSTD_compress`) and for
//! buffered streaming (`ZSTD_compressStream2`); the decoder (`lib/decompress`) reads any
//! frame without a dictionary, whole (`ZSTD_decompress`) or streamed
//! (`ZSTD_decompressStream`).
//!
//! The `ZSTD_fast` and `ZSTD_dfast` match finders are ported, with and without an extDict
//! (the window of a stream that wrapped around its input buffer); the stronger strategies
//! (`ZSTD_greedy` and above, levels 4 or 5 and up) compress with the double-fast match
//! finder at their level's window, so their frames are valid but differ from zstd's.
//! The compressor supports no dictionaries, checksums, long-distance matching or
//! multi-threading: DuckDB uses none of them.

#![allow(
    clippy::inline_always,
    reason = "the hot loops of the match finders and entropy coders must inline these"
)]

mod bits;
mod block;
mod cctx;
mod decompress;
mod dfast;
mod error;
mod fast;
mod fse;
mod hist;
mod huf;
mod literals;
mod params;
mod seq;
mod sequences;
mod window;

pub use cctx::{Compressor, EndDirective, compress, compress_streamed};
pub use decompress::{
    Decompressor, StreamDecoder, decompress, find_frame_compressed_size, frame_content_size,
};
pub use error::Error;
pub use params::{DEFAULT_LEVEL, MAX_LEVEL, MIN_LEVEL, compress_bound};
