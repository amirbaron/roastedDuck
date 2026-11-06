//! Reading entropy table descriptions (`entropy_common.c`) and FSE decoding of bytes
//! (`fse_decompress.c`), which the Huffman weights are compressed with.

use super::bits::{BitReader, Status, highbit32};
use crate::error::{Error, Result};

/// `FSE_MIN_TABLELOG`.
const FSE_MIN_TABLELOG: u32 = 5;
/// `FSE_TABLELOG_ABSOLUTE_MAX`.
const FSE_TABLELOG_ABSOLUTE_MAX: u32 = 15;
/// `HUF_TABLELOG_MAX`.
pub(crate) const HUF_TABLELOG_MAX: u32 = 12;
/// `HUF_SYMBOLVALUE_MAX`.
pub(crate) const HUF_SYMBOLVALUE_MAX: usize = 255;
/// The table log the Huffman weights are FSE-compressed with at most.
const HUF_WEIGHTS_MAX_LOG: u32 = 6;

#[inline(always)]
fn read32(buf: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]])
}

/// `FSE_readNCount`: reads the normalized counts of symbols `0..=*max_symbol` from the
/// start of `src` into `norm`; `*max_symbol` becomes the last symbol read. Returns the
/// header size and the table log.
pub(crate) fn read_ncount(norm: &mut [i16], max_symbol: &mut u32, src: &[u8]) -> Result<(usize, u32)> {
    if src.len() < 8 {
        // The body reads 4 bytes at a time: pad with zeros.
        let mut buffer = [0u8; 8];
        buffer[..src.len()].copy_from_slice(src);
        let (size, table_log) = read_ncount(norm, max_symbol, &buffer)?;
        if size > src.len() {
            return Err(Error::CorruptionDetected);
        }
        return Ok((size, table_log));
    }
    let iend = src.len();
    let max_sv1 = *max_symbol + 1;
    norm[..max_sv1 as usize].fill(0);
    let mut ip = 0usize;
    let mut bit_stream = read32(src, ip);
    let mut nb_bits = (bit_stream & 0xF) as i32 + FSE_MIN_TABLELOG as i32;
    if nb_bits > FSE_TABLELOG_ABSOLUTE_MAX as i32 {
        return Err(Error::TableLogTooLarge);
    }
    bit_stream >>= 4;
    let mut bit_count: i32 = 4;
    let table_log = nb_bits as u32;
    let mut remaining: i32 = (1 << nb_bits) + 1;
    let mut threshold: i32 = 1 << nb_bits;
    nb_bits += 1;
    let mut charnum: u32 = 0;
    let mut previous0 = false;
    loop {
        if previous0 {
            // Each 2-bit repeat code 0b11 adds 3 zero counts.
            let mut repeats = ((!bit_stream | 0x8000_0000).trailing_zeros() >> 1) as i32;
            while repeats >= 12 {
                charnum += 3 * 12;
                if ip + 7 <= iend {
                    ip += 3;
                } else {
                    bit_count -= 8 * (iend as i32 - 7 - ip as i32);
                    bit_count &= 31;
                    ip = iend - 4;
                }
                bit_stream = read32(src, ip) >> bit_count;
                repeats = ((!bit_stream | 0x8000_0000).trailing_zeros() >> 1) as i32;
            }
            charnum += 3 * repeats as u32;
            bit_stream >>= 2 * repeats;
            bit_count += 2 * repeats;
            debug_assert!(bit_stream & 3 < 3);
            charnum += bit_stream & 3;
            bit_count += 2;
            if charnum >= max_sv1 {
                break;
            }
            if ip + 7 <= iend || ip + (bit_count >> 3) as usize + 4 <= iend {
                ip += (bit_count >> 3) as usize;
                bit_count &= 7;
            } else {
                bit_count -= 8 * (iend as i32 - 4 - ip as i32);
                bit_count &= 31;
                ip = iend - 4;
            }
            bit_stream = read32(src, ip) >> bit_count;
        }
        {
            let max = (2 * threshold - 1) - remaining;
            let mut count: i32;
            if (bit_stream & (threshold as u32 - 1)) < max as u32 {
                count = (bit_stream & (threshold as u32 - 1)) as i32;
                bit_count += nb_bits - 1;
            } else {
                count = (bit_stream & (2 * threshold as u32 - 1)) as i32;
                if count >= threshold {
                    count -= max;
                }
                bit_count += nb_bits;
            }
            count -= 1;
            if count >= 0 {
                remaining -= count;
            } else {
                remaining += count;
            }
            norm[charnum as usize] = count as i16;
            charnum += 1;
            previous0 = count == 0;
            if remaining < threshold {
                if remaining <= 1 {
                    break;
                }
                nb_bits = highbit32(remaining as u32) as i32 + 1;
                threshold = 1 << (nb_bits - 1);
            }
            if charnum >= max_sv1 {
                break;
            }
            if ip + 7 <= iend || ip + (bit_count >> 3) as usize + 4 <= iend {
                ip += (bit_count >> 3) as usize;
                bit_count &= 7;
            } else {
                bit_count -= 8 * (iend as i32 - 4 - ip as i32);
                bit_count &= 31;
                ip = iend - 4;
            }
            bit_stream = read32(src, ip) >> bit_count;
        }
    }
    if remaining != 1 {
        return Err(Error::CorruptionDetected);
    }
    if charnum > max_sv1 {
        return Err(Error::MaxSymbolValueTooSmall);
    }
    if bit_count > 32 {
        return Err(Error::CorruptionDetected);
    }
    *max_symbol = charnum - 1;
    ip += ((bit_count + 7) >> 3) as usize;
    Ok((ip, table_log))
}

/// The Huffman weights of a literals section (`HUF_readStats`).
#[derive(Clone, Debug)]
pub(crate) struct HufStats {
    /// The weight of each symbol (0: absent).
    pub weights: [u8; HUF_SYMBOLVALUE_MAX + 1],
    /// The number of symbols of each weight.
    pub rank_stats: [u32; HUF_TABLELOG_MAX as usize + 1],
    pub nb_symbols: u32,
    pub table_log: u32,
}

impl Default for HufStats {
    fn default() -> Self {
        Self {
            weights: [0; HUF_SYMBOLVALUE_MAX + 1],
            rank_stats: [0; HUF_TABLELOG_MAX as usize + 1],
            nb_symbols: 0,
            table_log: 0,
        }
    }
}

/// `HUF_readStats`: reads the weights at the start of `src`; returns the header size.
pub(crate) fn read_stats(stats: &mut HufStats, src: &[u8]) -> Result<usize> {
    let Some(&first) = src.first() else {
        return Err(Error::SrcSizeWrong);
    };
    let mut i_size = first as usize;
    let o_size;
    let weights = &mut stats.weights;
    if i_size >= 128 {
        // Raw 4-bit weights.
        o_size = i_size - 127;
        i_size = o_size.div_ceil(2);
        if i_size + 1 > src.len() {
            return Err(Error::SrcSizeWrong);
        }
        if o_size > HUF_SYMBOLVALUE_MAX {
            return Err(Error::CorruptionDetected);
        }
        for n in (0..o_size).step_by(2) {
            weights[n] = src[1 + n / 2] >> 4;
            weights[n + 1] = src[1 + n / 2] & 15;
        }
    } else {
        if i_size + 1 > src.len() {
            return Err(Error::SrcSizeWrong);
        }
        // The last weight is implied.
        o_size = fse_decompress(
            &mut weights[..HUF_SYMBOLVALUE_MAX],
            &src[1..=i_size],
            HUF_WEIGHTS_MAX_LOG,
        )?;
    }
    stats.rank_stats.fill(0);
    let mut weight_total = 0u32;
    for &weight in &weights[..o_size] {
        if u32::from(weight) > HUF_TABLELOG_MAX {
            return Err(Error::CorruptionDetected);
        }
        stats.rank_stats[weight as usize] += 1;
        weight_total += (1 << weight) >> 1;
    }
    if weight_total == 0 {
        return Err(Error::CorruptionDetected);
    }
    let table_log = highbit32(weight_total) + 1;
    if table_log > HUF_TABLELOG_MAX {
        return Err(Error::CorruptionDetected);
    }
    stats.table_log = table_log;
    let rest = (1u32 << table_log) - weight_total;
    let verif = 1u32 << highbit32(rest);
    let last_weight = highbit32(rest) + 1;
    if verif != rest {
        return Err(Error::CorruptionDetected);
    }
    weights[o_size] = last_weight as u8;
    stats.rank_stats[last_weight as usize] += 1;
    if stats.rank_stats[1] < 2 || stats.rank_stats[1] & 1 != 0 {
        return Err(Error::CorruptionDetected);
    }
    stats.nb_symbols = o_size as u32 + 1;
    Ok(i_size + 1)
}

/// `FSE_decode_t`.
#[derive(Clone, Copy, Debug, Default)]
struct FseDecode {
    new_state: u16,
    symbol: u8,
    nb_bits: u8,
}

/// `FSE_TABLESTEP`.
pub(crate) const fn table_step(table_size: u32) -> u32 {
    (table_size >> 1) + (table_size >> 3) + 3
}

/// `FSE_decompress_wksp`: decodes the FSE stream of byte symbols in `src` (its table
/// description first, of table log at most `max_log`) into `dst`; returns the size.
fn fse_decompress(dst: &mut [u8], src: &[u8], max_log: u32) -> Result<usize> {
    const MAX_SYMBOL: u32 = 255;
    let mut norm = [0i16; MAX_SYMBOL as usize + 1];
    let mut max_symbol = MAX_SYMBOL;
    let (header_size, table_log) = read_ncount(&mut norm, &mut max_symbol, src)?;
    if table_log > max_log {
        return Err(Error::TableLogTooLarge);
    }
    let mut table = [FseDecode::default(); 1 << HUF_WEIGHTS_MAX_LOG];
    let fast = build_fse_table(&mut table, &norm, max_symbol, table_log);
    let src = &src[header_size..];
    if fast {
        decompress_using_table::<true>(dst, src, &table, table_log)
    } else {
        decompress_using_table::<false>(dst, src, &table, table_log)
    }
}

/// `FSE_buildDTable_internal`; returns whether no symbol takes half the table or more
/// (`fastMode`).
fn build_fse_table(table: &mut [FseDecode], norm: &[i16], max_symbol: u32, table_log: u32) -> bool {
    let max_sv1 = max_symbol as usize + 1;
    let table_size = 1u32 << table_log;
    let mut high_threshold = table_size - 1;
    let mut symbol_next = [0u16; 256];
    let mut fast = true;
    let large_limit = 1i16 << (table_log - 1);
    for s in 0..max_sv1 {
        if norm[s] == -1 {
            table[high_threshold as usize].symbol = s as u8;
            high_threshold = high_threshold.wrapping_sub(1);
            symbol_next[s] = 1;
        } else {
            if norm[s] >= large_limit {
                fast = false;
            }
            symbol_next[s] = norm[s] as u16;
        }
    }
    let mask = table_size - 1;
    let step = table_step(table_size);
    let mut position = 0u32;
    for (s, &count) in norm.iter().enumerate().take(max_sv1) {
        for _ in 0..count.max(0) {
            table[position as usize].symbol = s as u8;
            position = (position + step) & mask;
            while position > high_threshold {
                position = (position + step) & mask;
            }
        }
    }
    for entry in &mut table[..table_size as usize] {
        let symbol = entry.symbol as usize;
        let next_state = u32::from(symbol_next[symbol]);
        symbol_next[symbol] += 1;
        entry.nb_bits = (table_log - highbit32(next_state)) as u8;
        entry.new_state = ((next_state << entry.nb_bits) - table_size) as u16;
    }
    fast
}

/// `FSE_decompress_usingDTable_generic`: two interleaved states.
fn decompress_using_table<const FAST: bool>(
    dst: &mut [u8],
    src: &[u8],
    table: &[FseDecode],
    table_log: u32,
) -> Result<usize> {
    let mut bits = BitReader::new(src)?;
    let mut state1 = bits.read(table_log) as usize;
    bits.reload();
    let mut state2 = bits.read(table_log) as usize;
    bits.reload();
    let decode = |state: &mut usize, bits: &mut BitReader<'_>| -> u8 {
        let entry = table[*state];
        let low = if FAST {
            bits.read_fast(u32::from(entry.nb_bits))
        } else {
            bits.read(u32::from(entry.nb_bits))
        };
        *state = usize::from(entry.new_state) + low as usize;
        entry.symbol
    };
    let omax = dst.len();
    let olimit = omax.saturating_sub(3);
    let mut op = 0usize;
    // 4 symbols per loop (a 64-bit container holds 4 reads of up to 12 bits).
    while bits.reload() == Status::Unfinished && op < olimit {
        dst[op] = decode(&mut state1, &mut bits);
        dst[op + 1] = decode(&mut state2, &mut bits);
        dst[op + 2] = decode(&mut state1, &mut bits);
        dst[op + 3] = decode(&mut state2, &mut bits);
        op += 4;
    }
    // The tail ends at exactly `Completed`.
    loop {
        if op + 2 > omax {
            return Err(Error::DstSizeTooSmall);
        }
        dst[op] = decode(&mut state1, &mut bits);
        op += 1;
        if bits.reload() == Status::Overflow {
            dst[op] = decode(&mut state2, &mut bits);
            op += 1;
            break;
        }
        if op + 2 > omax {
            return Err(Error::DstSizeTooSmall);
        }
        dst[op] = decode(&mut state2, &mut bits);
        op += 1;
        if bits.reload() == Status::Overflow {
            dst[op] = decode(&mut state1, &mut bits);
            op += 1;
            break;
        }
    }
    Ok(op)
}
