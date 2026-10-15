//! Radix partitioning on hash values (DuckDB's `RadixPartitioning`).
//!
//! The partition index is taken from the hash bits just below the top 16 bits, which hash
//! tables use as salt.

use rustdb_types::Vector;

/// Radix partitioning helpers.
#[derive(Debug)]
pub struct RadixPartitioning;

impl RadixPartitioning {
    /// 4096 partitions ought to be enough to go out of core properly.
    pub const MAX_RADIX_BITS: usize = 12;

    /// Number of partitions for `radix_bits` bits.
    pub const fn number_of_partitions(radix_bits: usize) -> usize {
        1 << radix_bits
    }

    /// Bits needed to represent `n` (DuckDB's `RadixBits`).
    pub const fn radix_bits(n: usize) -> usize {
        n.bit_width() as usize
    }

    /// The radix bits of a power-of-two partition count.
    pub const fn radix_bits_of_power_of_two(partitions: usize) -> usize {
        Self::radix_bits(partitions) - 1
    }

    /// Right shift that brings the radix bits down.
    pub const fn shift(radix_bits: usize) -> u32 {
        (48 - radix_bits) as u32
    }

    /// Mask of the radix bits within a hash.
    pub const fn mask(radix_bits: usize) -> u64 {
        ((1u64 << radix_bits) - 1) << Self::shift(radix_bits)
    }

    /// The partition of `hash`.
    #[inline]
    pub const fn partition_index(hash: u64, radix_bits: usize) -> usize {
        ((hash & Self::mask(radix_bits)) >> Self::shift(radix_bits)) as usize
    }

    /// Splits rows by whether their partition is selected in `partitions`
    /// (`RadixPartitioning::Select`). Rows are taken from `sel` (or `0..count`); returns
    /// the number of selected rows, which go to `true_sel`, the others to `false_sel`.
    pub fn select(
        hashes: &Vector,
        sel: Option<&[u32]>,
        count: usize,
        radix_bits: usize,
        partitions: &[bool],
        mut true_sel: Option<&mut Vec<u32>>,
        mut false_sel: Option<&mut Vec<u32>>,
    ) -> usize {
        let hashes = hashes.unified::<u64>();
        let mut selected = 0;
        for i in 0..count {
            let row = sel.map_or(i, |sel| sel[i] as usize);
            let hash = hashes.values[hashes.slot(row)];
            if partitions[Self::partition_index(hash, radix_bits)] {
                selected += 1;
                if let Some(true_sel) = true_sel.as_deref_mut() {
                    true_sel.push(row as u32);
                }
            } else if let Some(false_sel) = false_sel.as_deref_mut() {
                false_sel.push(row as u32);
            }
        }
        selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_bits_sit_below_the_salt() {
        assert_eq!(RadixPartitioning::shift(4), 44);
        assert_eq!(RadixPartitioning::mask(4), 0x0000_F000_0000_0000);
        assert_eq!(RadixPartitioning::partition_index(0xFFFF_A000_0000_0000, 4), 0xA);
        assert_eq!(RadixPartitioning::radix_bits(0), 0);
        assert_eq!(RadixPartitioning::radix_bits(1), 1);
        assert_eq!(RadixPartitioning::radix_bits(16), 5);
        assert_eq!(RadixPartitioning::radix_bits_of_power_of_two(16), 4);
    }
}
