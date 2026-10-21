//! A zstd compressor: a port of zstd 1.5.6's `lib/compress` (the version DuckDB vendors) that
//! writes the frames zstd writes at the same level, for one-shot compression
//! (`ZSTD_compress`) and for buffered streaming (`ZSTD_compressStream2`). Decoding is left
//! to a decoder of the format.
//!
//! The `ZSTD_fast` and `ZSTD_dfast` match finders are ported, with and without an extDict
//! (the window of a stream that wrapped around its input buffer); the stronger strategies
//! (`ZSTD_greedy` and above, levels 4 or 5 and up) compress with the double-fast match
//! finder at their level's window, so their frames are valid but differ from zstd's.
//! Dictionaries, checksums, long-distance matching and multi-threading are not supported:
//! DuckDB uses none of them.

#![allow(
    clippy::inline_always,
    reason = "the hot loops of the match finders and entropy coders must inline these"
)]

mod bits;
mod block;
mod cctx;
mod dfast;
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
pub use params::{DEFAULT_LEVEL, MAX_LEVEL, MIN_LEVEL, compress_bound};
