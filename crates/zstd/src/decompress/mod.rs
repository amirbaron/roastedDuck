//! The decoder: a port of zstd 1.5.6's `lib/decompress` (`zstd_decompress.c`,
//! `zstd_decompress_block.c`, `huf_decompress.c`) with its entropy and checksum helpers.
//! Legacy formats and dictionaries are not supported; DuckDB writes neither, and frames
//! that need a dictionary fail with `Error::DictionaryWrong` as they do in zstd.

mod bits;
mod block;
mod entropy;
mod frame;
mod huf;
mod xxhash;

use std::cell::RefCell;
use std::io::{self, Read};

pub use frame::{Decompressor, find_frame_compressed_size, frame_content_size};

use crate::error::Result;
use crate::params::BLOCK_SIZE_MAX;

thread_local! {
    /// The context `decompress` reuses on this thread (`ZSTD_decompress` makes one per
    /// call; its tables and literal buffer would be allocated and cleared each time).
    static CONTEXT: RefCell<Option<Box<Decompressor>>> = const { RefCell::new(None) };
}

/// Decompresses every frame of `src` into `dst` (`ZSTD_decompress`); returns the
/// decompressed size.
pub fn decompress(dst: &mut [u8], src: &[u8]) -> Result<usize> {
    CONTEXT.with(|context| match context.try_borrow_mut() {
        Ok(mut context) => context.get_or_insert_with(Box::default).decompress(dst, src),
        Err(_) => Decompressor::new().decompress(dst, src),
    })
}

/// `ZSTD_DStreamInSize`: the input a stream decodes a block from.
const STREAM_IN_SIZE: usize = BLOCK_SIZE_MAX + 3;

/// A reader of the decompressed content of a zstd stream read from `R` (DuckDB's
/// `ZstdStreamWrapper` reading through `ZSTD_decompressStream`): frames follow each other
/// until the input ends.
pub struct StreamDecoder<R> {
    inner: R,
    decompressor: Box<Decompressor>,
    input: Vec<u8>,
    in_start: usize,
    in_end: usize,
    eof: bool,
}

impl<R: Read> StreamDecoder<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            decompressor: Box::default(),
            input: vec![0; STREAM_IN_SIZE],
            in_start: 0,
            in_end: 0,
            eof: false,
        }
    }

    /// The reader the compressed stream comes from.
    pub fn get_ref(&self) -> &R {
        &self.inner
    }
}

impl<R> std::fmt::Debug for StreamDecoder<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamDecoder")
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

impl<R: Read> Read for StreamDecoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.in_start == self.in_end && !self.eof {
                let read = self.inner.read(&mut self.input)?;
                self.in_start = 0;
                self.in_end = read;
                self.eof = read == 0;
            }
            let mut out_pos = 0;
            let mut in_pos = self.in_start;
            self.decompressor.decompress_stream(
                buf,
                &mut out_pos,
                &self.input[..self.in_end],
                &mut in_pos,
            )?;
            self.in_start = in_pos;
            if out_pos > 0 {
                return Ok(out_pos);
            }
            if self.eof && self.in_start == self.in_end {
                return Ok(0);
            }
        }
    }
}
