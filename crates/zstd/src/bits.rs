//! The little-endian bit stream FSE and Huffman write (`BIT_CStream_t`, `HUF_CStream_t`):
//! values are appended from the lowest bit up, and flushed a byte at a time.

/// A bit writer appending to a byte vector.
#[derive(Debug)]
pub(crate) struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    start: usize,
    container: u64,
    bit_pos: u32,
}

impl<'a> BitWriter<'a> {
    pub(crate) fn new(out: &'a mut Vec<u8>) -> Self {
        let start = out.len();
        out.reserve(4096);
        Self {
            out,
            start,
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
        let len = self.out.len();
        if self.out.capacity() - len < 8 {
            self.out.reserve(1 << 16);
        }
        // SAFETY: at least 8 bytes of capacity follow the length (reserved above); the
        // length only grows over the `bytes` (at most 7) initialized by the write.
        unsafe {
            self.out
                .as_mut_ptr()
                .add(len)
                .cast::<u64>()
                .write_unaligned(self.container.to_le());
            self.out.set_len(len + bytes);
        }
        self.container >>= bytes * 8;
        self.bit_pos &= 7;
    }

    /// `BIT_closeCStream`: appends the end mark and writes the last byte; returns the size of
    /// the stream in bytes.
    pub(crate) fn close(mut self) -> usize {
        self.add_bits_fast(1, 1);
        self.flush();
        if self.bit_pos > 0 {
            self.out.push(self.container as u8);
        }
        self.out.len() - self.start
    }
}
