//! Huffman coding of literals (`huf_compress.c`).

use crate::bits::BitWriter;
use crate::fse;
use crate::hist;
use crate::params::highbit32;

pub(crate) const HUF_TABLELOG_MAX: u32 = 12;
pub(crate) const HUF_TABLELOG_DEFAULT: u32 = 11;
pub(crate) const HUF_SYMBOLVALUE_MAX: u32 = 255;
const MAX_FSE_TABLELOG_FOR_HUFF_HEADER: u32 = 6;
const STARTNODE: usize = HUF_SYMBOLVALUE_MAX as usize + 1;

/// `HUF_repeat`: whether the previous block's table may be reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Repeat {
    /// Cannot use the previous table.
    None,
    /// Can use the previous table, but it must be checked.
    Check,
    /// Can use the previous table, and it is assumed to be valid.
    Valid,
}

/// `HUF_flags_e`.
pub(crate) const FLAG_OPTIMAL_DEPTH: u32 = 1 << 1;
pub(crate) const FLAG_PREFER_REPEAT: u32 = 1 << 2;
pub(crate) const FLAG_SUSPECT_UNCOMPRESSIBLE: u32 = 1 << 3;

/// A Huffman compression table (`HUF_CElt[]` with its header): the code length and value
/// of every symbol.
#[derive(Clone, Debug)]
pub(crate) struct CTable {
    pub table_log: u32,
    pub max_symbol: u32,
    pub nb_bits: [u8; 256],
    pub value: [u16; 256],
}

impl Default for CTable {
    fn default() -> Self {
        Self {
            table_log: 0,
            max_symbol: 0,
            nb_bits: [0; 256],
            value: [0; 256],
        }
    }
}

/// A node of the Huffman tree (`nodeElt`).
#[derive(Clone, Copy, Debug, Default)]
struct Node {
    count: u32,
    parent: u16,
    byte: u8,
    nb_bits: u8,
}

/// `HUF_compressWeights`: the FSE-compressed weights, or 0 (not compressible) or 1 (one
/// weight). `None` is an error.
fn compress_weights(out: &mut Vec<u8>, weights: &[u8]) -> Option<usize> {
    if weights.len() <= 1 {
        return Some(0);
    }
    let start = out.len();
    let mut count = [0u32; HUF_TABLELOG_MAX as usize + 1];
    let mut max_symbol = HUF_TABLELOG_MAX;
    let max_count = hist::count(&mut count, &mut max_symbol, weights);
    if max_count as usize == weights.len() {
        return Some(1);
    }
    if max_count == 1 {
        return Some(0);
    }
    let table_log = fse::optimal_table_log(MAX_FSE_TABLELOG_FOR_HUFF_HEADER, weights.len(), max_symbol);
    let mut norm = [0i16; HUF_TABLELOG_MAX as usize + 1];
    fse::normalize_count(&mut norm, table_log, &count, weights.len(), max_symbol, false)?;
    let header = fse::write_ncount(out, &norm, max_symbol, table_log)?;
    let mut table = fse::CTable::default();
    fse::build_ctable(&mut table, &norm, max_symbol, table_log);
    let size = fse::compress_using_ctable(out, weights, &table);
    if size == 0 {
        out.truncate(start);
        return Some(0);
    }
    Some(header + size)
}

/// `HUF_writeCTable_wksp`: appends the table description. `None` is an error.
pub(crate) fn write_ctable(
    out: &mut Vec<u8>,
    table: &CTable,
    max_symbol: u32,
    huff_log: u32,
) -> Option<usize> {
    let mut bits_to_weight = [0u8; HUF_TABLELOG_MAX as usize + 1];
    for (weight, n) in bits_to_weight[1..=huff_log as usize].iter_mut().zip(1u32..) {
        *weight = (huff_log + 1 - n) as u8;
    }
    let mut weights = [0u8; HUF_SYMBOLVALUE_MAX as usize + 1];
    for n in 0..max_symbol as usize {
        weights[n] = bits_to_weight[table.nb_bits[n] as usize];
    }
    let start = out.len();
    out.push(0);
    let size = compress_weights(out, &weights[..max_symbol as usize])?;
    if size > 1 && size < max_symbol as usize / 2 {
        out[start] = size as u8;
        return Some(size + 1);
    }
    out.truncate(start);
    if max_symbol > 256 - 128 {
        return None;
    }
    out.push((128 + (max_symbol - 1)) as u8);
    weights[max_symbol as usize] = 0;
    for n in (0..max_symbol as usize).step_by(2) {
        out.push((weights[n] << 4) + weights[n + 1]);
    }
    Some((max_symbol as usize).div_ceil(2) + 1)
}

/// `HUF_setMaxHeight`: limits the code lengths to `target_nb_bits`.
fn set_max_height(nodes: &mut [Node], last_non_null: usize, target_nb_bits: u32) -> u32 {
    const NO_SYMBOL: u32 = 0xF0F0_F0F0;
    let largest_bits = u32::from(nodes[last_non_null].nb_bits);
    if largest_bits <= target_nb_bits {
        return largest_bits;
    }
    let mut total_cost: i32 = 0;
    let base_cost: i32 = 1 << (largest_bits - target_nb_bits);
    let mut n = last_non_null as i32;
    while u32::from(nodes[n as usize].nb_bits) > target_nb_bits {
        total_cost += base_cost - (1 << (largest_bits - u32::from(nodes[n as usize].nb_bits)));
        nodes[n as usize].nb_bits = target_nb_bits as u8;
        n -= 1;
    }
    while u32::from(nodes[n as usize].nb_bits) == target_nb_bits {
        n -= 1;
    }
    total_cost >>= largest_bits - target_nb_bits;
    let mut rank_last = [NO_SYMBOL; HUF_TABLELOG_MAX as usize + 2];
    {
        let mut current_nb_bits = target_nb_bits;
        let mut pos = n;
        while pos >= 0 {
            let bits = u32::from(nodes[pos as usize].nb_bits);
            if bits < current_nb_bits {
                current_nb_bits = bits;
                rank_last[(target_nb_bits - current_nb_bits) as usize] = pos as u32;
            }
            pos -= 1;
        }
    }
    while total_cost > 0 {
        let mut nb_bits_to_decrease = highbit32(total_cost as u32) + 1;
        while nb_bits_to_decrease > 1 {
            let high_pos = rank_last[nb_bits_to_decrease as usize];
            let low_pos = rank_last[nb_bits_to_decrease as usize - 1];
            if high_pos == NO_SYMBOL {
                nb_bits_to_decrease -= 1;
                continue;
            }
            if low_pos == NO_SYMBOL {
                break;
            }
            let high_total = nodes[high_pos as usize].count;
            let low_total = 2 * nodes[low_pos as usize].count;
            if high_total <= low_total {
                break;
            }
            nb_bits_to_decrease -= 1;
        }
        while nb_bits_to_decrease <= HUF_TABLELOG_MAX && rank_last[nb_bits_to_decrease as usize] == NO_SYMBOL
        {
            nb_bits_to_decrease += 1;
        }
        let index = nb_bits_to_decrease as usize;
        total_cost -= 1 << (nb_bits_to_decrease - 1);
        nodes[rank_last[index] as usize].nb_bits += 1;
        if rank_last[index - 1] == NO_SYMBOL {
            rank_last[index - 1] = rank_last[index];
        }
        if rank_last[index] == 0 {
            rank_last[index] = NO_SYMBOL;
        } else {
            rank_last[index] -= 1;
            if u32::from(nodes[rank_last[index] as usize].nb_bits) != target_nb_bits - nb_bits_to_decrease {
                rank_last[index] = NO_SYMBOL;
            }
        }
    }
    while total_cost < 0 {
        if rank_last[1] == NO_SYMBOL {
            while u32::from(nodes[n as usize].nb_bits) == target_nb_bits {
                n -= 1;
            }
            nodes[(n + 1) as usize].nb_bits -= 1;
            rank_last[1] = (n + 1) as u32;
            total_cost += 1;
            continue;
        }
        nodes[rank_last[1] as usize + 1].nb_bits -= 1;
        rank_last[1] += 1;
        total_cost += 1;
    }
    target_nb_bits
}

const RANK_POSITION_TABLE_SIZE: usize = 192;
const RANK_POSITION_MAX_COUNT_LOG: u32 = 32;
const RANK_POSITION_LOG_BUCKETS_BEGIN: u32 =
    (RANK_POSITION_TABLE_SIZE as u32 - 1) - RANK_POSITION_MAX_COUNT_LOG - 1;
const RANK_POSITION_DISTINCT_COUNT_CUTOFF: u32 = RANK_POSITION_LOG_BUCKETS_BEGIN + 7; // highbit32(158) == 7

/// `HUF_getIndex`.
fn rank_index(count: u32) -> u32 {
    if count < RANK_POSITION_DISTINCT_COUNT_CUTOFF {
        count
    } else {
        highbit32(count) + RANK_POSITION_LOG_BUCKETS_BEGIN
    }
}

/// `HUF_insertionSort`: descending by count.
fn insertion_sort(nodes: &mut [Node]) {
    for i in 1..nodes.len() {
        let key = nodes[i];
        let mut j = i as isize - 1;
        while j >= 0 && nodes[j as usize].count < key.count {
            nodes[(j + 1) as usize] = nodes[j as usize];
            j -= 1;
        }
        nodes[(j + 1) as usize] = key;
    }
}

/// `HUF_quickSortPartition`.
fn quick_sort_partition(nodes: &mut [Node], low: isize, high: isize) -> isize {
    let pivot = nodes[high as usize].count;
    let mut i = low - 1;
    for j in low..high {
        if nodes[j as usize].count > pivot {
            i += 1;
            nodes.swap(i as usize, j as usize);
        }
    }
    nodes.swap((i + 1) as usize, high as usize);
    i + 1
}

/// `HUF_simpleQuickSort`: descending by count.
fn simple_quick_sort(nodes: &mut [Node], mut low: isize, mut high: isize) {
    const INSERTION_SORT_THRESHOLD: isize = 8;
    if high - low < INSERTION_SORT_THRESHOLD {
        if high >= low {
            insertion_sort(&mut nodes[low as usize..=high as usize]);
        }
        return;
    }
    while low < high {
        let idx = quick_sort_partition(nodes, low, high);
        if idx - low < high - idx {
            simple_quick_sort(nodes, low, idx - 1);
            low = idx + 1;
        } else {
            simple_quick_sort(nodes, idx + 1, high);
            high = idx - 1;
        }
    }
}

/// `HUF_sort`: the symbols by decreasing count, into `nodes[1..]`.
fn sort(nodes: &mut [Node], count: &[u32], max_symbol: u32) {
    let max_sv1 = max_symbol as usize + 1;
    let mut rank_base = [0u16; RANK_POSITION_TABLE_SIZE];
    let mut rank_curr = [0u16; RANK_POSITION_TABLE_SIZE];
    for &c in &count[..max_sv1] {
        rank_base[rank_index(c) as usize] += 1;
    }
    for n in (1..RANK_POSITION_TABLE_SIZE).rev() {
        rank_base[n - 1] += rank_base[n];
        rank_curr[n - 1] = rank_base[n - 1];
    }
    for (n, &c) in count[..max_sv1].iter().enumerate() {
        let r = rank_index(c) as usize + 1;
        let pos = rank_curr[r] as usize;
        rank_curr[r] += 1;
        nodes[pos + 1].count = c;
        nodes[pos + 1].byte = n as u8;
    }
    for n in RANK_POSITION_DISTINCT_COUNT_CUTOFF as usize..RANK_POSITION_TABLE_SIZE - 1 {
        let bucket_size = rank_curr[n] as isize - rank_base[n] as isize;
        let start = rank_base[n] as usize;
        if bucket_size > 1 {
            simple_quick_sort(&mut nodes[start + 1..], 0, bucket_size - 1);
        }
    }
}

/// `HUF_buildTree`: the unlimited-depth tree over the sorted symbols; returns the index of
/// the last symbol with a non-zero count. `nodes[0]` is the barrier `huffNode[-1]`.
fn build_tree(nodes: &mut [Node], max_symbol: u32) -> usize {
    // Index `i` of zstd's `huffNode` is `nodes[i + 1]`.
    let at = |i: isize| (i + 1) as usize;
    let mut non_null_rank = max_symbol as isize;
    while nodes[at(non_null_rank)].count == 0 {
        non_null_rank -= 1;
    }
    let mut low_s = non_null_rank;
    let node_root = STARTNODE as isize + low_s - 1;
    let mut low_n = STARTNODE as isize;
    let mut node_nb = STARTNODE as isize;
    nodes[at(node_nb)].count = nodes[at(low_s)].count + nodes[at(low_s - 1)].count;
    nodes[at(low_s)].parent = node_nb as u16;
    nodes[at(low_s - 1)].parent = node_nb as u16;
    node_nb += 1;
    low_s -= 2;
    for n in node_nb..=node_root {
        nodes[at(n)].count = 1 << 30;
    }
    nodes[0].count = 1 << 31;
    while node_nb <= node_root {
        let n1 = if nodes[at(low_s)].count < nodes[at(low_n)].count {
            low_s -= 1;
            low_s + 1
        } else {
            low_n += 1;
            low_n - 1
        };
        let n2 = if nodes[at(low_s)].count < nodes[at(low_n)].count {
            low_s -= 1;
            low_s + 1
        } else {
            low_n += 1;
            low_n - 1
        };
        nodes[at(node_nb)].count = nodes[at(n1)].count.wrapping_add(nodes[at(n2)].count);
        nodes[at(n1)].parent = node_nb as u16;
        nodes[at(n2)].parent = node_nb as u16;
        node_nb += 1;
    }
    nodes[at(node_root)].nb_bits = 0;
    let mut n = node_root - 1;
    while n >= STARTNODE as isize {
        let parent = nodes[at(n)].parent as isize;
        nodes[at(n)].nb_bits = nodes[at(parent)].nb_bits + 1;
        n -= 1;
    }
    for n in 0..=non_null_rank {
        let parent = nodes[at(n)].parent as isize;
        nodes[at(n)].nb_bits = nodes[at(parent)].nb_bits + 1;
    }
    non_null_rank as usize
}

/// `HUF_buildCTable_wksp`: a table with codes of at most `max_nb_bits` bits; returns the
/// longest code.
pub(crate) fn build_ctable(table: &mut CTable, count: &[u32], max_symbol: u32, max_nb_bits: u32) -> u32 {
    let max_nb_bits = if max_nb_bits == 0 {
        HUF_TABLELOG_DEFAULT
    } else {
        max_nb_bits
    };
    let mut nodes = [Node::default(); 2 * (HUF_SYMBOLVALUE_MAX as usize + 1)];
    sort(&mut nodes, count, max_symbol);
    let non_null_rank = build_tree(&mut nodes, max_symbol);
    let max_nb_bits = set_max_height(&mut nodes[1..], non_null_rank, max_nb_bits);
    // `HUF_buildCTableFromTree`.
    let huff_nodes = &nodes[1..];
    let mut nb_per_rank = [0u16; HUF_TABLELOG_MAX as usize + 1];
    let mut val_per_rank = [0u16; HUF_TABLELOG_MAX as usize + 1];
    for node in &huff_nodes[..=non_null_rank] {
        nb_per_rank[node.nb_bits as usize] += 1;
    }
    let mut min: u16 = 0;
    for n in (1..=max_nb_bits as usize).rev() {
        val_per_rank[n] = min;
        min += nb_per_rank[n];
        min >>= 1;
    }
    let alphabet_size = max_symbol as usize + 1;
    for node in &huff_nodes[..alphabet_size] {
        table.nb_bits[node.byte as usize] = node.nb_bits;
    }
    for n in 0..alphabet_size {
        let bits = table.nb_bits[n] as usize;
        table.value[n] = val_per_rank[bits];
        val_per_rank[bits] = val_per_rank[bits].wrapping_add(1);
    }
    table.table_log = max_nb_bits;
    table.max_symbol = max_symbol;
    max_nb_bits
}

/// `HUF_estimateCompressedSize`.
pub(crate) fn estimate_compressed_size(table: &CTable, count: &[u32], max_symbol: u32) -> usize {
    let bits: usize = (0..=max_symbol as usize)
        .map(|s| table.nb_bits[s] as usize * count[s] as usize)
        .sum();
    bits >> 3
}

/// `HUF_validateCTable`: whether `table` encodes every symbol present.
pub(crate) fn validate_ctable(table: &CTable, count: &[u32], max_symbol: u32) -> bool {
    if table.max_symbol < max_symbol {
        return false;
    }
    (0..=max_symbol as usize).all(|s| count[s] == 0 || table.nb_bits[s] != 0)
}

/// `HUF_compress1X_usingCTable_internal`: appends the stream; returns its size.
fn compress_1x(out: &mut Vec<u8>, src: &[u8], table: &CTable) -> usize {
    // Codes are 12 bits at most.
    let mut bits = BitWriter::new(out, src.len() + src.len() / 2 + 16);
    let mut n = src.len();
    // The symbols are written last to first; flushing every four keeps at most 55 bits
    // pending.
    while n >= 4 {
        for _ in 0..4 {
            n -= 1;
            let symbol = src[n] as usize;
            bits.add_bits_fast(u64::from(table.value[symbol]), u32::from(table.nb_bits[symbol]));
        }
        bits.flush();
    }
    while n > 0 {
        n -= 1;
        let symbol = src[n] as usize;
        bits.add_bits_fast(u64::from(table.value[symbol]), u32::from(table.nb_bits[symbol]));
    }
    bits.flush();
    bits.close()
}

/// `HUF_compress4X_usingCTable_internal`: four streams after a jump table; 0 if they do not
/// fit the format.
fn compress_4x(out: &mut Vec<u8>, src: &[u8], table: &CTable) -> usize {
    if src.len() < 12 {
        return 0;
    }
    let start = out.len();
    let segment_size = src.len().div_ceil(4);
    out.extend_from_slice(&[0; 6]);
    let mut ip = 0;
    for stream in 0..4 {
        let end = if stream == 3 { src.len() } else { ip + segment_size };
        let size = compress_1x(out, &src[ip..end], table);
        if size == 0 || size > 65535 {
            out.truncate(start);
            return 0;
        }
        if stream < 3 {
            out[start + stream * 2..start + stream * 2 + 2].copy_from_slice(&(size as u16).to_le_bytes());
        }
        ip = end;
    }
    out.len() - start
}

/// `HUF_compressCTable_internal`: appends the streams after the `header_size` bytes already
/// written at `start`; 0 when the result is not smaller than the input.
fn compress_ctable(out: &mut Vec<u8>, start: usize, src: &[u8], four_streams: bool, table: &CTable) -> usize {
    let size = if four_streams {
        compress_4x(out, src, table)
    } else {
        compress_1x(out, src, table)
    };
    if size == 0 || out.len() - start >= src.len() - 1 {
        out.truncate(start);
        return 0;
    }
    out.len() - start
}

/// `HUF_optimalTableLog`.
pub(crate) fn optimal_table_log(
    max_table_log: u32,
    src_size: usize,
    max_symbol: u32,
    count: &[u32],
    flags: u32,
) -> u32 {
    if flags & FLAG_OPTIMAL_DEPTH == 0 {
        return fse::optimal_table_log_internal(max_table_log, src_size, max_symbol, 1);
    }
    let cardinality = count[..=max_symbol as usize].iter().filter(|&&c| c != 0).count() as u32;
    let min_table_log = highbit32(cardinality) + 1;
    let mut opt_size = usize::MAX - 1;
    let mut opt_log = max_table_log;
    let mut table = CTable::default();
    let mut scratch = Vec::new();
    for guess in min_table_log..=max_table_log {
        let max_bits = build_ctable(&mut table, count, max_symbol, guess);
        if max_bits < guess && guess > min_table_log {
            break;
        }
        scratch.clear();
        let Some(header_size) = write_ctable(&mut scratch, &table, max_symbol, max_bits) else {
            continue;
        };
        let new_size = estimate_compressed_size(&table, count, max_symbol) + header_size;
        if new_size > opt_size + 1 {
            break;
        }
        if new_size < opt_size {
            opt_size = new_size;
            opt_log = guess;
        }
    }
    opt_log
}

/// `HUF_compress_internal` (`HUF_compress1X_repeat`, `HUF_compress4X_repeat`): appends the
/// table description (unless the previous one is reused) and the streams. Returns the size
/// written, 0 if not compressible, 1 for a single symbol (whose byte is written), `None`
/// on an error.
pub(crate) fn compress(
    out: &mut Vec<u8>,
    src: &[u8],
    four_streams: bool,
    old_table: &mut CTable,
    repeat: &mut Repeat,
    flags: u32,
) -> Option<usize> {
    const SAMPLE_SIZE: usize = 4096;
    const SAMPLE_RATIO: usize = 10;
    let max_symbol_value = HUF_SYMBOLVALUE_MAX;
    let huff_log = HUF_TABLELOG_DEFAULT;
    if src.is_empty() {
        return Some(0);
    }
    let start = out.len();
    if flags & FLAG_PREFER_REPEAT != 0 && *repeat == Repeat::Valid {
        return Some(compress_ctable(out, start, src, four_streams, old_table));
    }
    let mut count = [0u32; 256];
    if flags & FLAG_SUSPECT_UNCOMPRESSIBLE != 0 && src.len() >= SAMPLE_SIZE * SAMPLE_RATIO {
        let mut max_begin = max_symbol_value;
        let largest_begin = hist::count(&mut count, &mut max_begin, &src[..SAMPLE_SIZE]);
        let mut max_end = max_symbol_value;
        let largest_end = hist::count(&mut count, &mut max_end, &src[src.len() - SAMPLE_SIZE..]);
        if largest_begin + largest_end <= ((2 * SAMPLE_SIZE) >> 7) as u32 + 4 {
            return Some(0);
        }
    }
    let mut max_symbol = max_symbol_value;
    let largest = hist::count(&mut count, &mut max_symbol, src);
    if largest as usize == src.len() {
        out.push(src[0]);
        return Some(1);
    }
    if largest as usize <= (src.len() >> 7) + 4 {
        return Some(0);
    }
    if *repeat == Repeat::Check && !validate_ctable(old_table, &count, max_symbol) {
        *repeat = Repeat::None;
    }
    if flags & FLAG_PREFER_REPEAT != 0 && *repeat != Repeat::None {
        return Some(compress_ctable(out, start, src, four_streams, old_table));
    }
    let huff_log = optimal_table_log(huff_log, src.len(), max_symbol, &count, flags);
    let mut table = CTable::default();
    let huff_log = build_ctable(&mut table, &count, max_symbol, huff_log);
    let header_size = write_ctable(out, &table, max_symbol, huff_log)?;
    if *repeat != Repeat::None {
        let old_size = estimate_compressed_size(old_table, &count, max_symbol);
        let new_size = estimate_compressed_size(&table, &count, max_symbol);
        if old_size <= header_size + new_size || header_size + 12 >= src.len() {
            out.truncate(start);
            return Some(compress_ctable(out, start, src, four_streams, old_table));
        }
    }
    if header_size + 12 >= src.len() {
        out.truncate(start);
        return Some(0);
    }
    *repeat = Repeat::None;
    *old_table = table;
    Some(compress_ctable(out, start, src, four_streams, old_table))
}
