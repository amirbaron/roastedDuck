//! Finite State Entropy tables (`fse_compress.c` and the encoder of `fse.h`).

use crate::bits::BitWriter;
use crate::params::highbit32;

pub(crate) const FSE_MIN_TABLELOG: u32 = 5;
pub(crate) const FSE_MAX_TABLELOG: u32 = 12;
const FSE_DEFAULT_TABLELOG: u32 = 11;

/// How a symbol is encoded (`FSE_symbolCompressionTransform`).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SymbolTransform {
    pub delta_find_state: i32,
    pub delta_nb_bits: u32,
}

/// An FSE compression table (`FSE_CTable`).
#[derive(Clone, Debug, Default)]
pub(crate) struct CTable {
    pub table_log: u32,
    pub max_symbol: u32,
    pub state_table: Vec<u16>,
    pub symbol_tt: Vec<SymbolTransform>,
}

/// `FSE_minTableLog`.
fn min_table_log(src_size: usize, max_symbol: u32) -> u32 {
    let min_bits_src = highbit32(src_size as u32) + 1;
    let min_bits_symbols = highbit32(max_symbol) + 2;
    min_bits_src.min(min_bits_symbols)
}

/// `FSE_optimalTableLog_internal`.
pub(crate) fn optimal_table_log_internal(
    max_table_log: u32,
    src_size: usize,
    max_symbol: u32,
    minus: u32,
) -> u32 {
    let max_bits_src = highbit32((src_size - 1) as u32).wrapping_sub(minus);
    let mut table_log = if max_table_log == 0 {
        FSE_DEFAULT_TABLELOG
    } else {
        max_table_log
    };
    let min_bits = min_table_log(src_size, max_symbol);
    if max_bits_src < table_log {
        table_log = max_bits_src;
    }
    if min_bits > table_log {
        table_log = min_bits;
    }
    table_log.clamp(FSE_MIN_TABLELOG, FSE_MAX_TABLELOG)
}

/// `FSE_optimalTableLog`.
pub(crate) fn optimal_table_log(max_table_log: u32, src_size: usize, max_symbol: u32) -> u32 {
    optimal_table_log_internal(max_table_log, src_size, max_symbol, 2)
}

/// `FSE_normalizeM2`: the secondary normalization, when the primary one fails.
fn normalize_m2(
    norm: &mut [i16],
    table_log: u32,
    count: &[u32],
    mut total: usize,
    max_symbol: u32,
    low_prob_count: i16,
) -> Option<()> {
    const NOT_YET_ASSIGNED: i16 = -2;
    let mut distributed: u32 = 0;
    let low_threshold = (total >> table_log) as u32;
    let mut low_one = ((total * 3) >> (table_log + 1)) as u32;
    for (n, &c) in norm.iter_mut().zip(count).take(max_symbol as usize + 1) {
        if c == 0 {
            *n = 0;
        } else if c <= low_threshold {
            *n = low_prob_count;
            distributed += 1;
            total -= c as usize;
        } else if c <= low_one {
            *n = 1;
            distributed += 1;
            total -= c as usize;
        } else {
            *n = NOT_YET_ASSIGNED;
        }
    }
    let mut to_distribute = (1u32 << table_log) - distributed;
    if to_distribute == 0 {
        return Some(());
    }
    if (total / to_distribute as usize) > low_one as usize {
        low_one = ((total * 3) / (to_distribute as usize * 2)) as u32;
        for s in 0..=max_symbol as usize {
            if norm[s] == NOT_YET_ASSIGNED && count[s] <= low_one {
                norm[s] = 1;
                distributed += 1;
                total -= count[s] as usize;
            }
        }
        to_distribute = (1u32 << table_log) - distributed;
    }
    if distributed == max_symbol + 1 {
        let (mut max_v, mut max_c) = (0usize, 0u32);
        for (s, &c) in count.iter().enumerate().take(max_symbol as usize + 1) {
            if c > max_c {
                max_v = s;
                max_c = c;
            }
        }
        norm[max_v] += to_distribute as i16;
        return Some(());
    }
    if total == 0 {
        let mut s = 0usize;
        while to_distribute > 0 {
            if norm[s] > 0 {
                to_distribute -= 1;
                norm[s] += 1;
            }
            s = (s + 1) % (max_symbol as usize + 1);
        }
        return Some(());
    }
    let v_step_log = 62 - u64::from(table_log);
    let mid = (1u64 << (v_step_log - 1)) - 1;
    let r_step = (((1u64 << v_step_log) * u64::from(to_distribute)) + mid) / total as u64;
    let mut tmp_total = mid;
    for s in 0..=max_symbol as usize {
        if norm[s] == NOT_YET_ASSIGNED {
            let end = tmp_total + u64::from(count[s]) * r_step;
            let s_start = (tmp_total >> v_step_log) as u32;
            let s_end = (end >> v_step_log) as u32;
            let weight = s_end - s_start;
            if weight < 1 {
                return None;
            }
            norm[s] = weight as i16;
            tmp_total = end;
        }
    }
    Some(())
}

/// `FSE_normalizeCount`: scales `count` (summing to `total`) to `1 << table_log`. Returns
/// `None` on failure (an error in zstd); `Some(false)` means a single symbol (RLE).
pub(crate) fn normalize_count(
    norm: &mut [i16],
    table_log: u32,
    count: &[u32],
    total: usize,
    max_symbol: u32,
    use_low_prob_count: bool,
) -> Option<bool> {
    const RTB_TABLE: [u64; 8] = [0, 473_195, 504_333, 520_860, 550_000, 700_000, 750_000, 830_000];
    let table_log = if table_log == 0 {
        FSE_DEFAULT_TABLELOG
    } else {
        table_log
    };
    if !(FSE_MIN_TABLELOG..=FSE_MAX_TABLELOG).contains(&table_log) {
        return None;
    }
    if table_log < min_table_log(total, max_symbol) {
        return None;
    }
    let low_prob_count: i16 = if use_low_prob_count { -1 } else { 1 };
    let scale = 62 - u64::from(table_log);
    let step = (1u64 << 62) / total as u64;
    let v_step = 1u64 << (scale - 20);
    let mut still_to_distribute: i32 = 1 << table_log;
    let mut largest = 0usize;
    let mut largest_p: i16 = 0;
    let low_threshold = (total >> table_log) as u32;
    for s in 0..=max_symbol as usize {
        if count[s] as usize == total {
            return Some(false);
        }
        if count[s] == 0 {
            norm[s] = 0;
            continue;
        }
        if count[s] <= low_threshold {
            norm[s] = low_prob_count;
            still_to_distribute -= 1;
        } else {
            let scaled = u64::from(count[s]) * step;
            let mut proba = (scaled >> scale) as i16;
            if proba < 8 {
                let rest_to_beat = v_step * RTB_TABLE[proba as usize];
                proba += i16::from(scaled - ((proba as u64) << scale) > rest_to_beat);
            }
            if proba > largest_p {
                largest_p = proba;
                largest = s;
            }
            norm[s] = proba;
            still_to_distribute -= i32::from(proba);
        }
    }
    if -still_to_distribute >= i32::from(norm[largest] >> 1) {
        normalize_m2(norm, table_log, count, total, max_symbol, low_prob_count)?;
    } else {
        norm[largest] += still_to_distribute as i16;
    }
    Some(true)
}

/// `FSE_writeNCount`: appends the table description; returns its size.
pub(crate) fn write_ncount(
    out: &mut Vec<u8>,
    norm: &[i16],
    max_symbol: u32,
    table_log: u32,
) -> Option<usize> {
    if !(FSE_MIN_TABLELOG..=FSE_MAX_TABLELOG).contains(&table_log) {
        return None;
    }
    let start = out.len();
    let table_size: i32 = 1 << table_log;
    let mut bit_stream: u32 = table_log - FSE_MIN_TABLELOG;
    let mut bit_count: i32 = 4;
    let mut remaining: i32 = table_size + 1;
    let mut threshold: i32 = table_size;
    let mut nb_bits: i32 = table_log as i32 + 1;
    let alphabet_size = max_symbol + 1;
    let mut symbol: u32 = 0;
    let mut previous_is0 = false;
    let flush16 = |out: &mut Vec<u8>, bit_stream: &mut u32| {
        out.push(*bit_stream as u8);
        out.push((*bit_stream >> 8) as u8);
        *bit_stream >>= 16;
    };
    while symbol < alphabet_size && remaining > 1 {
        if previous_is0 {
            let mut start_symbol = symbol;
            while symbol < alphabet_size && norm[symbol as usize] == 0 {
                symbol += 1;
            }
            if symbol == alphabet_size {
                break;
            }
            while symbol >= start_symbol + 24 {
                start_symbol += 24;
                bit_stream = bit_stream.wrapping_add(0xFFFF << bit_count);
                flush16(out, &mut bit_stream);
            }
            while symbol >= start_symbol + 3 {
                start_symbol += 3;
                bit_stream = bit_stream.wrapping_add(3 << bit_count);
                bit_count += 2;
            }
            bit_stream = bit_stream.wrapping_add((symbol - start_symbol) << bit_count);
            bit_count += 2;
            if bit_count > 16 {
                flush16(out, &mut bit_stream);
                bit_count -= 16;
            }
        }
        let mut count = i32::from(norm[symbol as usize]);
        symbol += 1;
        let max = (2 * threshold - 1) - remaining;
        remaining -= count.abs();
        count += 1;
        if count >= threshold {
            count += max;
        }
        bit_stream = bit_stream.wrapping_add((count as u32) << bit_count);
        bit_count += nb_bits;
        bit_count -= i32::from(count < max);
        previous_is0 = count == 1;
        if remaining < 1 {
            return None;
        }
        while remaining < threshold {
            nb_bits -= 1;
            threshold >>= 1;
        }
        if bit_count > 16 {
            flush16(out, &mut bit_stream);
            bit_count -= 16;
        }
    }
    if remaining != 1 {
        return None;
    }
    out.push(bit_stream as u8);
    out.push((bit_stream >> 8) as u8);
    out.truncate(out.len() - 2 + ((bit_count + 7) / 8) as usize);
    Some(out.len() - start)
}

/// `FSE_TABLESTEP`.
fn table_step(table_size: u32) -> u32 {
    (table_size >> 1) + (table_size >> 3) + 3
}

/// `FSE_buildCTable_wksp`.
pub(crate) fn build_ctable(table: &mut CTable, norm: &[i16], max_symbol: u32, table_log: u32) {
    let table_size = 1u32 << table_log;
    let table_mask = table_size - 1;
    let step = table_step(table_size);
    let max_sv1 = max_symbol as usize + 1;
    let mut cumul = [0u16; 256 + 1];
    let mut table_symbol = vec![0u8; table_size as usize];
    let mut high_threshold = table_size - 1;
    table.table_log = table_log;
    table.max_symbol = max_symbol;
    for u in 1..=max_sv1 {
        if norm[u - 1] == -1 {
            cumul[u] = cumul[u - 1] + 1;
            table_symbol[high_threshold as usize] = (u - 1) as u8;
            high_threshold = high_threshold.wrapping_sub(1);
        } else {
            cumul[u] = cumul[u - 1] + norm[u - 1] as u16;
        }
    }
    cumul[max_sv1] = (table_size + 1) as u16;
    // Spread the symbols (the unrolled spreading DuckDB's zstd uses without low
    // probability symbols lays them out identically).
    let mut position = 0u32;
    for (symbol, &freq) in norm.iter().enumerate().take(max_sv1) {
        for _ in 0..freq.max(0) {
            table_symbol[position as usize] = symbol as u8;
            position = (position + step) & table_mask;
            while position > high_threshold {
                position = (position + step) & table_mask;
            }
        }
    }
    debug_assert_eq!(position, 0);
    table.state_table.clear();
    table.state_table.resize(table_size as usize, 0);
    for u in 0..table_size {
        let s = table_symbol[u as usize] as usize;
        table.state_table[cumul[s] as usize] = (table_size + u) as u16;
        cumul[s] += 1;
    }
    table.symbol_tt.clear();
    table.symbol_tt.resize(max_sv1, SymbolTransform::default());
    let mut total: u32 = 0;
    for (transform, &freq) in table.symbol_tt.iter_mut().zip(norm) {
        match freq {
            0 => transform.delta_nb_bits = ((table_log + 1) << 16).wrapping_sub(1 << table_log),
            -1 | 1 => {
                transform.delta_nb_bits = (table_log << 16).wrapping_sub(1 << table_log);
                transform.delta_find_state = total as i32 - 1;
                total += 1;
            }
            freq => {
                let freq = freq as u32;
                let max_bits_out = table_log - highbit32(freq - 1);
                let min_state_plus = freq << max_bits_out;
                transform.delta_nb_bits = (max_bits_out << 16).wrapping_sub(min_state_plus);
                transform.delta_find_state = total as i32 - freq as i32;
                total += freq;
            }
        }
    }
}

/// `FSE_buildCTable_rle`: a table for a single symbol.
pub(crate) fn build_ctable_rle(table: &mut CTable, symbol: u8) {
    table.table_log = 0;
    table.max_symbol = u32::from(symbol);
    table.state_table.clear();
    table.state_table.extend_from_slice(&[0, 0]);
    table.symbol_tt.clear();
    table
        .symbol_tt
        .resize(symbol as usize + 1, SymbolTransform::default());
}

/// An encoder state (`FSE_CState_t`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CState {
    value: u64,
}

impl CTable {
    /// `FSE_initCState2`: the state after encoding `symbol` first, with the cheapest state.
    #[inline]
    pub(crate) fn init_state2(&self, symbol: u8) -> CState {
        let transform = self.symbol_tt[symbol as usize];
        let nb_bits_out = (transform.delta_nb_bits + (1 << 15)) >> 16;
        let value = (nb_bits_out << 16).wrapping_sub(transform.delta_nb_bits);
        let index = ((value >> nb_bits_out) as i64 + i64::from(transform.delta_find_state)) as usize;
        CState {
            value: u64::from(self.state_table[index]),
        }
    }

    /// `FSE_encodeSymbol`.
    #[inline(always)]
    pub(crate) fn encode(&self, bits: &mut BitWriter<'_>, state: &mut CState, symbol: u8) {
        debug_assert!((symbol as usize) < self.symbol_tt.len());
        // SAFETY: the symbols encoded are those the table was built for (`max_symbol` or
        // less), and a state stays within the table (`FSE_buildCTable`).
        let transform = unsafe { *self.symbol_tt.get_unchecked(symbol as usize) };
        let nb_bits_out = ((state.value + u64::from(transform.delta_nb_bits)) >> 16) as u32;
        bits.add_bits(state.value, nb_bits_out);
        let index = ((state.value >> nb_bits_out) as i64 + i64::from(transform.delta_find_state)) as usize;
        debug_assert!(index < self.state_table.len());
        // SAFETY: as above.
        state.value = u64::from(unsafe { *self.state_table.get_unchecked(index) });
    }

    /// `FSE_flushCState`.
    #[inline]
    pub(crate) fn flush_state(&self, bits: &mut BitWriter<'_>, state: CState) {
        bits.add_bits(state.value, self.table_log);
        bits.flush();
    }

    /// `FSE_bitCost` at `accuracy_log` fractional bits.
    pub(crate) fn bit_cost(&self, symbol: u32, accuracy_log: u32) -> u32 {
        let transform = self.symbol_tt[symbol as usize];
        let min_nb_bits = transform.delta_nb_bits >> 16;
        let threshold = (min_nb_bits + 1) << 16;
        let table_size = 1u32 << self.table_log;
        let delta_from_threshold = threshold.wrapping_sub(transform.delta_nb_bits.wrapping_add(table_size));
        let normalized = (delta_from_threshold << accuracy_log) >> self.table_log;
        let multiplier = 1u32 << accuracy_log;
        ((min_nb_bits + 1) * multiplier).wrapping_sub(normalized)
    }
}

/// `FSE_compress_usingCTable`: appends the FSE stream of `src`; returns its size, or 0 when
/// the input is too small.
pub(crate) fn compress_using_ctable(out: &mut Vec<u8>, src: &[u8], table: &CTable) -> usize {
    if src.len() <= 2 {
        return 0;
    }
    // At most 12 bits per symbol.
    let mut bits = BitWriter::new(out, src.len() * 2 + 16);
    let mut ip = src.len();
    let (mut state1, mut state2);
    if src.len() & 1 == 1 {
        ip -= 1;
        state1 = table.init_state2(src[ip]);
        ip -= 1;
        state2 = table.init_state2(src[ip]);
        ip -= 1;
        table.encode(&mut bits, &mut state1, src[ip]);
        bits.flush();
    } else {
        ip -= 1;
        state2 = table.init_state2(src[ip]);
        ip -= 1;
        state1 = table.init_state2(src[ip]);
    }
    let size = src.len() - 2;
    if size & 2 != 0 {
        ip -= 1;
        table.encode(&mut bits, &mut state2, src[ip]);
        ip -= 1;
        table.encode(&mut bits, &mut state1, src[ip]);
        bits.flush();
    }
    while ip > 0 {
        ip -= 1;
        table.encode(&mut bits, &mut state2, src[ip]);
        ip -= 1;
        table.encode(&mut bits, &mut state1, src[ip]);
        ip -= 1;
        table.encode(&mut bits, &mut state2, src[ip]);
        ip -= 1;
        table.encode(&mut bits, &mut state1, src[ip]);
        bits.flush();
    }
    table.flush_state(&mut bits, state2);
    table.flush_state(&mut bits, state1);
    bits.close()
}
