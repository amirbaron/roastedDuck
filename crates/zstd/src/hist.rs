//! Symbol histograms (`hist.c`).

/// Counts the bytes of `src` into `count[0..=*max_symbol]` (`HIST_count_simple` and its
/// faster variants, which give the same result): `*max_symbol` becomes the largest symbol
/// present (0 for empty input); returns the largest count.
pub(crate) fn count(count: &mut [u32], max_symbol: &mut u32, src: &[u8]) -> u32 {
    let size = *max_symbol as usize + 1;
    count[..size].fill(0);
    if src.is_empty() {
        *max_symbol = 0;
        return 0;
    }
    if size == 256 {
        // Four tables avoid store-to-load stalls on repeated symbols.
        let mut tables = [[0u32; 256]; 4];
        let (chunks, remainder) = src.as_chunks::<4>();
        for chunk in chunks {
            tables[0][chunk[0] as usize] += 1;
            tables[1][chunk[1] as usize] += 1;
            tables[2][chunk[2] as usize] += 1;
            tables[3][chunk[3] as usize] += 1;
        }
        for &byte in remainder {
            tables[0][byte as usize] += 1;
        }
        for symbol in 0..256 {
            count[symbol] = tables[0][symbol] + tables[1][symbol] + tables[2][symbol] + tables[3][symbol];
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
