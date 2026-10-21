//! The backward bit stream FSE and Huffman decoding read (`BIT_DStream_t`,
//! `bitstream.h`): the stream is read from its last byte down, the highest bit first.

use std::marker::PhantomData;

use crate::error::{Error, Result};

/// `BIT_DStream_status`, in the order zstd compares them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Status {
    /// The container is filled with at least 57 bits.
    Unfinished,
    /// The start of the stream is loaded; the container may not be full.
    EndOfBuffer,
    /// Every bit was read.
    Completed,
    /// More bits were read than the stream holds (corruption or the end of a stream).
    Overflow,
}

/// Reads 8 little-endian bytes at `pos`.
#[inline(always)]
pub(crate) fn read64(buf: &[u8], pos: usize) -> u64 {
    debug_assert!(pos + 8 <= buf.len(), "read of 8 bytes at {pos} of {}", buf.len());
    // SAFETY: callers keep `pos + 8 <= buf.len()` (zstd reads the same bytes; the tests
    // run with the debug assertion).
    u64::from_le(unsafe { buf.as_ptr().add(pos).cast::<u64>().read_unaligned() })
}

/// The highest set bit of `value` (`ZSTD_highbit32`); `value` is not 0.
#[inline(always)]
pub(crate) fn highbit32(value: u32) -> u32 {
    debug_assert!(value != 0);
    value.ilog2()
}

/// `BIT_DStream_t` over a buffer whose first byte is the lowest the stream may read
/// (`start`). Pointers, as zstd keeps them, so that refilling needs no base register.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BitReader<'a> {
    start: *const u8,
    /// `start + 8`: below it, a refill must not read a whole word below `ptr`.
    limit: *const u8,
    /// Where the 8 bytes loaded into `container` are.
    ptr: *const u8,
    pub container: u64,
    /// The bits of `container` read so far, from its top.
    pub consumed: u32,
    buf: PhantomData<&'a [u8]>,
}

#[inline(always)]
unsafe fn load(ptr: *const u8) -> u64 {
    // SAFETY: the caller guarantees 8 readable bytes.
    u64::from_le(unsafe { ptr.cast::<u64>().read_unaligned() })
}

impl<'a> BitReader<'a> {
    /// `BIT_initDStream`: the stream of all of `buf`, whose last byte holds the end mark.
    #[inline]
    pub(crate) fn new(buf: &'a [u8]) -> Result<Self> {
        let len = buf.len();
        if len == 0 {
            return Err(Error::SrcSizeWrong);
        }
        let last = buf[len - 1];
        let start = buf.as_ptr();
        // SAFETY: `start + 8` is only compared with (buffers of 8 bytes or more have it).
        let limit = start.wrapping_add(8);
        if len >= 8 {
            if last == 0 {
                return Err(Error::Generic);
            }
            return Ok(Self {
                start,
                limit,
                ptr: start.wrapping_add(len - 8),
                container: read64(buf, len - 8),
                consumed: 8 - highbit32(u32::from(last)),
                buf: PhantomData,
            });
        }
        if last == 0 {
            return Err(Error::CorruptionDetected);
        }
        let mut container = 0u64;
        for (i, &byte) in buf.iter().enumerate() {
            container += u64::from(byte) << (8 * i);
        }
        Ok(Self {
            start,
            limit,
            ptr: start,
            container,
            consumed: 8 - highbit32(u32::from(last)) + (8 - len as u32) * 8,
            buf: PhantomData,
        })
    }

    /// A stream of `buf` positioned by the fast Huffman loops: `container` holds the 8 bytes
    /// at `pos`, of which `consumed` bits are read (`HUF_initRemainingDStream`).
    #[inline]
    pub(crate) fn at(buf: &'a [u8], pos: usize, consumed: u32) -> Self {
        let start = buf.as_ptr();
        Self {
            start,
            limit: start.wrapping_add(8),
            ptr: start.wrapping_add(pos),
            container: read64(buf, pos),
            consumed,
            buf: PhantomData,
        }
    }

    /// `BIT_lookBits`: the next `nb_bits` bits (0 to 56), not consumed.
    #[inline(always)]
    pub(crate) fn look(&self, nb_bits: u32) -> u64 {
        let start = 64u32.wrapping_sub(self.consumed).wrapping_sub(nb_bits) & 63;
        (self.container >> start) & ((1u64 << nb_bits) - 1)
    }

    /// `BIT_lookBitsFast`: as `look`, for 1 bit or more.
    #[inline(always)]
    pub(crate) fn look_fast(&self, nb_bits: u32) -> u64 {
        debug_assert!(nb_bits >= 1);
        (self.container << (self.consumed & 63)) >> ((64 - nb_bits) & 63)
    }

    /// `BIT_skipBits`.
    #[inline(always)]
    pub(crate) fn skip(&mut self, nb_bits: u32) {
        self.consumed += nb_bits;
    }

    /// `BIT_readBits`.
    #[inline(always)]
    pub(crate) fn read(&mut self, nb_bits: u32) -> u64 {
        let value = self.look(nb_bits);
        self.skip(nb_bits);
        value
    }

    /// `BIT_readBitsFast`: as `read`, for 1 bit or more.
    #[inline(always)]
    pub(crate) fn read_fast(&mut self, nb_bits: u32) -> u64 {
        let value = self.look_fast(nb_bits);
        self.skip(nb_bits);
        value
    }

    #[inline(always)]
    fn reload_internal(&mut self) -> Status {
        self.ptr = self.ptr.wrapping_sub((self.consumed >> 3) as usize);
        self.consumed &= 7;
        // SAFETY: `ptr` was at `limit` or above and `consumed` was 64 or less, so it is at
        // `start` or above, and never above the stream's last word.
        self.container = unsafe { load(self.ptr) };
        Status::Unfinished
    }

    /// `BIT_reloadDStreamFast`: refills while 8 bytes remain below the container.
    #[inline(always)]
    pub(crate) fn reload_fast(&mut self) -> Status {
        if self.ptr < self.limit {
            return Status::Overflow;
        }
        self.reload_internal()
    }

    /// `BIT_reloadDStream`: refills the container without reading below the stream.
    #[inline(always)]
    pub(crate) fn reload(&mut self) -> Status {
        if self.consumed > 64 {
            return Status::Overflow;
        }
        if self.ptr >= self.limit {
            return self.reload_internal();
        }
        if self.ptr == self.start {
            return if self.consumed < 64 {
                Status::EndOfBuffer
            } else {
                Status::Completed
            };
        }
        let mut nb_bytes = (self.consumed >> 3) as usize;
        let mut result = Status::Unfinished;
        let available = self.ptr as usize - self.start as usize;
        if available < nb_bytes {
            nb_bytes = available;
            result = Status::EndOfBuffer;
        }
        self.ptr = self.ptr.wrapping_sub(nb_bytes);
        self.consumed -= nb_bytes as u32 * 8;
        // SAFETY: `ptr` stays at `start` or above, and the stream holds 8 bytes or more
        // (shorter ones start with `ptr` at `start`).
        self.container = unsafe { load(self.ptr) };
        result
    }

    /// `BIT_endOfDStream`: every bit was read, exactly.
    #[inline(always)]
    pub(crate) fn finished(&self) -> bool {
        self.ptr == self.start && self.consumed == 64
    }
}
