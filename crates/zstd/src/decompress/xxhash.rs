//! XXH64 (`xxhash.c`), the content checksum of zstd frames: the low 32 bits of the hash
//! of the decompressed content, seed 0.

const PRIME1: u64 = 0x9E37_79B1_85EB_CA87;
const PRIME2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const PRIME3: u64 = 0x1656_67B1_9E37_79F9;
const PRIME4: u64 = 0x85EB_CA77_C2B2_AE63;
const PRIME5: u64 = 0x27D4_EB2F_1656_67C5;

#[inline(always)]
fn read64(buf: &[u8], pos: usize) -> u64 {
    u64::from_le_bytes(buf[pos..pos + 8].try_into().unwrap_or_default())
}

#[inline(always)]
fn round(acc: u64, input: u64) -> u64 {
    acc.wrapping_add(input.wrapping_mul(PRIME2))
        .rotate_left(31)
        .wrapping_mul(PRIME1)
}

#[inline(always)]
fn merge_round(acc: u64, value: u64) -> u64 {
    (acc ^ round(0, value)).wrapping_mul(PRIME1).wrapping_add(PRIME4)
}

/// `XXH64_state_t`.
#[derive(Clone, Debug)]
pub(crate) struct Xxh64 {
    total_len: u64,
    v: [u64; 4],
    mem: [u8; 32],
    mem_size: usize,
}

impl Xxh64 {
    /// `XXH64_reset` with seed 0.
    pub(crate) fn new() -> Self {
        Self {
            total_len: 0,
            v: [PRIME1.wrapping_add(PRIME2), PRIME2, 0, 0u64.wrapping_sub(PRIME1)],
            mem: [0; 32],
            mem_size: 0,
        }
    }

    #[inline(always)]
    fn stripe(&mut self, block: &[u8]) {
        for (i, v) in self.v.iter_mut().enumerate() {
            *v = round(*v, read64(block, 8 * i));
        }
    }

    /// `XXH64_update`.
    pub(crate) fn update(&mut self, mut input: &[u8]) {
        self.total_len += input.len() as u64;
        if self.mem_size + input.len() < 32 {
            self.mem[self.mem_size..self.mem_size + input.len()].copy_from_slice(input);
            self.mem_size += input.len();
            return;
        }
        if self.mem_size > 0 {
            let fill = 32 - self.mem_size;
            self.mem[self.mem_size..].copy_from_slice(&input[..fill]);
            let block = self.mem;
            self.stripe(&block);
            input = &input[fill..];
            self.mem_size = 0;
        }
        let (blocks, rest) = input.as_chunks::<32>();
        for block in blocks {
            self.stripe(block);
        }
        self.mem[..rest.len()].copy_from_slice(rest);
        self.mem_size = rest.len();
    }

    /// `XXH64_digest`.
    pub(crate) fn digest(&self) -> u64 {
        let mut h = if self.total_len >= 32 {
            let [v1, v2, v3, v4] = self.v;
            let mut h = v1
                .rotate_left(1)
                .wrapping_add(v2.rotate_left(7))
                .wrapping_add(v3.rotate_left(12))
                .wrapping_add(v4.rotate_left(18));
            for v in self.v {
                h = merge_round(h, v);
            }
            h
        } else {
            self.v[2].wrapping_add(PRIME5)
        };
        h = h.wrapping_add(self.total_len);
        let rest = &self.mem[..self.mem_size];
        let mut pos = 0;
        while pos + 8 <= rest.len() {
            h ^= round(0, read64(rest, pos));
            h = h.rotate_left(27).wrapping_mul(PRIME1).wrapping_add(PRIME4);
            pos += 8;
        }
        if pos + 4 <= rest.len() {
            let word = u32::from_le_bytes(rest[pos..pos + 4].try_into().unwrap_or_default());
            h ^= u64::from(word).wrapping_mul(PRIME1);
            h = h.rotate_left(23).wrapping_mul(PRIME2).wrapping_add(PRIME3);
            pos += 4;
        }
        for &byte in &rest[pos..] {
            h ^= u64::from(byte).wrapping_mul(PRIME5);
            h = h.rotate_left(11).wrapping_mul(PRIME1);
        }
        h ^= h >> 33;
        h = h.wrapping_mul(PRIME2);
        h ^= h >> 29;
        h = h.wrapping_mul(PRIME3);
        h ^ (h >> 32)
    }
}

#[cfg(test)]
mod tests {
    use super::Xxh64;

    fn hash(data: &[u8]) -> u64 {
        let mut state = Xxh64::new();
        state.update(data);
        state.digest()
    }

    #[test]
    fn known_values() {
        assert_eq!(hash(b""), 0xEF46_DB37_51D8_E999);
        assert_eq!(hash(b"a"), 0xD24E_C4F1_A98C_6E5B);
        assert_eq!(hash(b"abc"), 0x44BC_2CF5_AD77_0999);
    }

    #[test]
    fn split_updates_agree() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 + i / 13) as u8).collect();
        let whole = hash(&data);
        for split in [0, 1, 5, 31, 32, 33, 100, 999] {
            let mut state = Xxh64::new();
            state.update(&data[..split]);
            state.update(&data[split..]);
            assert_eq!(state.digest(), whole, "split at {split}");
        }
    }
}
