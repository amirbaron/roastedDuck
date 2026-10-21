//! Symbol histograms (`hist.c`).

/// `HIST_countFast_wksp`'s threshold: smaller inputs are counted simply.
const PARALLEL_MIN_SIZE: usize = 1500;

/// Counts the bytes of `src` into `count[0..=*max_symbol]` (`HIST_count_simple` and its
/// faster variants, which give the same result): `*max_symbol` becomes the largest symbol
/// present (0 for empty input); returns the largest count. Every byte is at most
/// `*max_symbol`.
pub(crate) fn count(count: &mut [u32], max_symbol: &mut u32, src: &[u8]) -> u32 {
    let size = *max_symbol as usize + 1;
    count[..size].fill(0);
    if src.is_empty() {
        *max_symbol = 0;
        return 0;
    }
    if src.len() >= PARALLEL_MIN_SIZE {
        // `HIST_count_parallel_wksp`: four tables avoid store-to-load stalls on repeated
        // symbols, and the input is read four bytes at a time.
        let mut tables = [[0u32; 256]; 4];
        let (chunks, remainder) = src.as_chunks::<4>();
        for chunk in chunks {
            let word = u32::from_le_bytes(*chunk);
            tables[0][(word & 0xFF) as usize] += 1;
            tables[1][((word >> 8) & 0xFF) as usize] += 1;
            tables[2][((word >> 16) & 0xFF) as usize] += 1;
            tables[3][(word >> 24) as usize] += 1;
        }
        for &byte in remainder {
            tables[0][byte as usize] += 1;
        }
        for (symbol, total) in count[..size].iter_mut().enumerate() {
            *total = tables[0][symbol] + tables[1][symbol] + tables[2][symbol] + tables[3][symbol];
        }
    } else {
        for &byte in src {
            count[byte as usize] += 1;
        }
    }
    let mut max = *max_symbol as usize;
    while count[max] == 0 {
        max -= 1;
    }
    *max_symbol = max as u32;
    count[..=max].iter().copied().max().unwrap_or(0)
}
