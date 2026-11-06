//! The little-endian bit stream FSE and Huffman write (`BIT_CStream_t`, `HUF_CStream_t`):
//! values are appended from the lowest bit up, and flushed a byte at a time.

/// A bit writer appending to a byte vector, into room reserved up front: the write
/// position stays in a register and the vector's length is set when the stream closes.
#[derive(Debug)]
pub(crate) struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    start: usize,
    /// Where the stream starts (`startPtr`).
    base: *mut u8,
    /// The next byte to write.
    ptr: *mut u8,
    /// The last position an 8-byte write may start at (`endPtr`): writes past the room
    /// reserved stop there, and the stream reports the overflow when it closes.
    end: *mut u8,
    container: u64,
    bit_pos: u32,
}

impl<'a> BitWriter<'a> {
    /// `BIT_initCStream` with room for `capacity` bytes after `out`'s content.
    pub(crate) fn new(out: &'a mut Vec<u8>, capacity: usize) -> Self {
        let start = out.len();
        out.reserve(capacity + 8);
        // SAFETY: `capacity + 8` bytes are reserved after `start`.
        let ptr = unsafe { out.as_mut_ptr().add(start) };
        // SAFETY: as above.
        let end = unsafe { ptr.add(capacity) };
        Self {
            out,
            start,
            base: ptr,
            ptr,
            end,
            container: 0,
            bit_pos: 0,
        }
    }

    /// `BIT_addBits`: appends the low `nb_bits` bits of `value` (at most 63 bits may be
    /// pending).
    #[inline(always)]
    pub(crate) fn add_bits(&mut self, value: u64, nb_bits: u32) {
        debug_assert!(nb_bits + self.bit_pos < 64);
        let mask = (1u64 << nb_bits) - 1;
        self.container |= (value & mask) << self.bit_pos;
        self.bit_pos += nb_bits;
    }

    /// `BIT_addBitsFast`: `value` has no bits above `nb_bits`.
    #[inline(always)]
    pub(crate) fn add_bits_fast(&mut self, value: u64, nb_bits: u32) {
        debug_assert!(nb_bits + self.bit_pos < 64 && value >> nb_bits == 0);
        self.container |= value << self.bit_pos;
        self.bit_pos += nb_bits;
    }

    /// `BIT_flushBits`: writes the complete bytes.
    #[inline(always)]
    pub(crate) fn flush(&mut self) {
        let bytes = (self.bit_pos >> 3) as usize;
        // SAFETY: `ptr` is at most `end`, and 8 bytes are reserved from `end` on.
        unsafe {
            self.ptr.cast::<u64>().write_unaligned(self.container.to_le());
        }
        let next = self.ptr.wrapping_add(bytes);
        self.ptr = if next > self.end { self.end } else { next };
        self.container >>= bytes * 8;
        self.bit_pos &= 7;
    }

    /// `BIT_closeCStream`: appends the end mark and writes the last byte; returns the size of
    /// the stream in bytes, 0 if it did not fit the room reserved.
    pub(crate) fn close(mut self) -> usize {
        self.add_bits_fast(1, 1);
        self.flush();
        if self.ptr >= self.end {
            return 0;
        }
        let size = self.ptr as usize - self.base as usize + usize::from(self.bit_pos > 0);
        // SAFETY: the flushes initialized every byte up to `ptr`, and the last one the byte
        // holding the pending bits.
        unsafe { self.out.set_len(self.start + size) };
        size
    }
}
