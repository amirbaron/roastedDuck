//! The sequences section of a block (`zstd_compress_sequences.c` and
//! `ZSTD_buildSequencesStatistics`).

use crate::bits::BitWriter;
use crate::fse::{self, CTable};
use crate::literals::{SET_BASIC, SET_COMPRESSED, SET_REPEAT, SET_RLE};
use crate::params::Strategy;
use crate::seq::{LL_BITS, ML_BITS, SeqStore};

pub(crate) const MAX_LL: u32 = 35;
pub(crate) const MAX_ML: u32 = 52;
pub(crate) const MAX_OFF: u32 = 31;
const DEFAULT_MAX_OFF: u32 = 28;
const LL_FSE_LOG: u32 = 9;
const ML_FSE_LOG: u32 = 9;
const OFF_FSE_LOG: u32 = 8;
const MAX_SEQ: usize = 52;

#[rustfmt::skip]
const LL_DEFAULT_NORM: [i16; 36] = [
    4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1,
    -1, -1, -1, -1,
];
const LL_DEFAULT_NORM_LOG: u32 = 6;
#[rustfmt::skip]
const ML_DEFAULT_NORM: [i16; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1,
    -1, -1, -1, -1, -1,
];
const ML_DEFAULT_NORM_LOG: u32 = 6;
#[rustfmt::skip]
const OF_DEFAULT_NORM: [i16; 29] = [
    1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
];
const OF_DEFAULT_NORM_LOG: u32 = 5;

/// `-log2(x / 256)` in 1/256 bits (`kInverseProbabilityLog256`).
#[rustfmt::skip]
const INVERSE_PROBABILITY_LOG256: [u32; 256] = [
    0, 2048, 1792, 1642, 1536, 1453, 1386, 1329, 1280, 1236, 1197, 1162,
    1130, 1100, 1073, 1047, 1024, 1001, 980, 960, 941, 923, 906, 889,
    874, 859, 844, 830, 817, 804, 791, 779, 768, 756, 745, 734,
    724, 714, 704, 694, 685, 676, 667, 658, 650, 642, 633, 626,
    618, 610, 603, 595, 588, 581, 574, 567, 561, 554, 548, 542,
    535, 529, 523, 517, 512, 506, 500, 495, 489, 484, 478, 473,
    468, 463, 458, 453, 448, 443, 438, 434, 429, 424, 420, 415,
    411, 407, 402, 398, 394, 390, 386, 382, 377, 373, 370, 366,
    362, 358, 354, 350, 347, 343, 339, 336, 332, 329, 325, 322,
    318, 315, 311, 308, 305, 302, 298, 295, 292, 289, 286, 282,
    279, 276, 273, 270, 267, 264, 261, 258, 256, 253, 250, 247,
    244, 241, 239, 236, 233, 230, 228, 225, 222, 220, 217, 215,
    212, 209, 207, 204, 202, 199, 197, 194, 192, 190, 187, 185,
    182, 180, 178, 175, 173, 171, 168, 166, 164, 162, 159, 157,
    155, 153, 151, 149, 146, 144, 142, 140, 138, 136, 134, 132,
    130, 128, 126, 123, 121, 119, 117, 115, 114, 112, 110, 108,
    106, 104, 102, 100, 98, 96, 94, 93, 91, 89, 87, 85,
    83, 82, 80, 78, 76, 74, 73, 71, 69, 67, 66, 64,
    62, 61, 59, 57, 55, 54, 52, 50, 49, 47, 46, 44,
    42, 41, 39, 37, 36, 34, 33, 31, 30, 28, 26, 25,
    23, 22, 20, 19, 17, 16, 14, 13, 11, 10, 8, 7,
    5, 4, 2, 1,
];

/// `FSE_repeat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FseRepeat {
    None,
    Check,
    Valid,
}

/// The FSE tables of a block and whether the next may reuse them (`ZSTD_fseCTables_t`).
#[derive(Clone, Debug)]
pub(crate) struct FseTables {
    pub offcode: CTable,
    pub matchlength: CTable,
    pub litlength: CTable,
    pub offcode_repeat: FseRepeat,
    pub matchlength_repeat: FseRepeat,
    pub litlength_repeat: FseRepeat,
}

impl Default for FseTables {
    fn default() -> Self {
        Self {
            offcode: CTable::default(),
            matchlength: CTable::default(),
            litlength: CTable::default(),
            offcode_repeat: FseRepeat::None,
            matchlength_repeat: FseRepeat::None,
            litlength_repeat: FseRepeat::None,
        }
    }
}

/// `ZSTD_useLowProbCount`.
fn use_low_prob_count(nb_seq: usize) -> bool {
    nb_seq >= 2048
}

/// `ZSTD_NCountCost`: the size of the table description in bytes, `None` on an error.
fn ncount_cost(count: &[u32], max: u32, nb_seq: usize, fse_log: u32) -> Option<usize> {
    let mut norm = [0i16; MAX_SEQ + 1];
    let table_log = fse::optimal_table_log(fse_log, nb_seq, max);
    fse::normalize_count(
        &mut norm,
        table_log,
        count,
        nb_seq,
        max,
        use_low_prob_count(nb_seq),
    )?;
    let mut scratch = Vec::with_capacity(512);
    fse::write_ncount(&mut scratch, &norm, max, table_log)
}

/// `ZSTD_entropyCost`: in bits.
fn entropy_cost(count: &[u32], max: u32, total: usize) -> usize {
    let mut cost: u32 = 0;
    for &c in &count[..=max as usize] {
        let mut norm = ((256 * c as usize) / total) as u32;
        if c != 0 && norm == 0 {
            norm = 1;
        }
        cost = cost.wrapping_add(c.wrapping_mul(INVERSE_PROBABILITY_LOG256[norm as usize]));
    }
    (cost >> 8) as usize
}

/// `ZSTD_fseBitCost`: in bits; `None` when the table cannot encode every symbol present.
fn fse_bit_cost(table: &CTable, count: &[u32], max: u32) -> Option<usize> {
    const ACCURACY_LOG: u32 = 8;
    if table.max_symbol < max {
        return None;
    }
    let mut cost = 0usize;
    let bad_cost = (table.table_log + 1) << ACCURACY_LOG;
    for (s, &c) in count[..=max as usize].iter().enumerate() {
        if c == 0 {
            continue;
        }
        let bit_cost = table.bit_cost(s as u32, ACCURACY_LOG);
        if bit_cost >= bad_cost {
            return None;
        }
        cost += c as usize * bit_cost as usize;
    }
    Some(cost >> ACCURACY_LOG)
}

/// `ZSTD_crossEntropyCost`: in bits.
fn cross_entropy_cost(norm: &[i16], accuracy_log: u32, count: &[u32], max: u32) -> usize {
    let shift = 8 - accuracy_log;
    let mut cost = 0usize;
    for s in 0..=max as usize {
        let norm_acc = if norm[s] == -1 { 1 } else { norm[s] as u32 };
        let norm256 = norm_acc << shift;
        cost += count[s] as usize * INVERSE_PROBABILITY_LOG256[norm256 as usize] as usize;
    }
    cost >> 8
}

/// `ZSTD_selectEncodingType`.
#[allow(clippy::too_many_arguments)]
fn select_encoding_type(
    repeat: &mut FseRepeat,
    count: &[u32],
    max: u32,
    most_frequent: usize,
    nb_seq: usize,
    fse_log: u32,
    prev: &CTable,
    default_norm: &[i16],
    default_norm_log: u32,
    default_allowed: bool,
    strategy: Strategy,
) -> u32 {
    if most_frequent == nb_seq {
        *repeat = FseRepeat::None;
        if default_allowed && nb_seq <= 2 {
            return SET_BASIC;
        }
        return SET_RLE;
    }
    if strategy < Strategy::Lazy {
        if default_allowed {
            const STATIC_FSE_NB_SEQ_MAX: usize = 1000;
            let mult = 10 - strategy as usize;
            let dynamic_fse_nb_seq_min = ((1usize << default_norm_log) * mult) >> 3;
            if *repeat == FseRepeat::Valid && nb_seq < STATIC_FSE_NB_SEQ_MAX {
                return SET_REPEAT;
            }
            if nb_seq < dynamic_fse_nb_seq_min || most_frequent < (nb_seq >> (default_norm_log - 1)) {
                *repeat = FseRepeat::None;
                return SET_BASIC;
            }
        }
    } else {
        let basic_cost =
            default_allowed.then(|| cross_entropy_cost(default_norm, default_norm_log, count, max));
        let repeat_cost = if *repeat == FseRepeat::None {
            None
        } else {
            fse_bit_cost(prev, count, max)
        };
        let ncount = ncount_cost(count, max, nb_seq, fse_log).unwrap_or(usize::MAX >> 4);
        let compressed_cost = (ncount << 3) + entropy_cost(count, max, nb_seq);
        let basic = basic_cost.unwrap_or(usize::MAX);
        let repeated = repeat_cost.unwrap_or(usize::MAX);
        if basic <= repeated && basic <= compressed_cost {
            *repeat = FseRepeat::None;
            return SET_BASIC;
        }
        if repeated <= compressed_cost {
            return SET_REPEAT;
        }
    }
    *repeat = FseRepeat::Check;
    SET_COMPRESSED
}

/// `ZSTD_buildCTable`: fills `next` for `kind` and appends the table description; returns
/// its size, `None` on an error.
#[allow(clippy::too_many_arguments)]
fn build_ctable(
    out: &mut Vec<u8>,
    next: &mut CTable,
    fse_log: u32,
    kind: u32,
    count: &mut [u32],
    max: u32,
    codes: &[u8],
    default_norm: &[i16],
    default_norm_log: u32,
    default_max: u32,
    prev: &CTable,
) -> Option<usize> {
    match kind {
        SET_RLE => {
            fse::build_ctable_rle(next, max as u8);
            out.push(codes[0]);
            Some(1)
        }
        SET_REPEAT => {
            next.clone_from(prev);
            Some(0)
        }
        SET_BASIC => {
            fse::build_ctable(next, default_norm, default_max, default_norm_log);
            Some(0)
        }
        _ => {
            let nb_seq = codes.len();
            let mut nb_seq_1 = nb_seq;
            let table_log = fse::optimal_table_log(fse_log, nb_seq, max);
            let last = codes[nb_seq - 1] as usize;
            if count[last] > 1 {
                count[last] -= 1;
                nb_seq_1 -= 1;
            }
            let mut norm = [0i16; MAX_SEQ + 1];
            fse::normalize_count(
                &mut norm,
                table_log,
                count,
                nb_seq_1,
                max,
                use_low_prob_count(nb_seq_1),
            )?;
            let size = fse::write_ncount(out, &norm, max, table_log)?;
            fse::build_ctable(next, &norm, max, table_log);
            Some(size)
        }
    }
}

/// The encoding of the three code streams and the size of their table descriptions
/// (`ZSTD_symbolEncodingTypeStats_t`).
pub(crate) struct EncodingTypes {
    pub ll: u32,
    pub of: u32,
    pub ml: u32,
    /// The size of the last `set_compressed` table description (zstd 1.3.4 workaround).
    pub last_count_size: usize,
}

/// `HIST_countFast_wksp` of a code table: the counts, the largest symbol and count.
fn count_codes(count: &mut [u32], max: &mut u32, codes: &[u8]) -> usize {
    crate::hist::count(count, max, codes) as usize
}

/// `ZSTD_buildSequencesStatistics`: chooses the encoding of each code stream, fills `next`
/// and appends the table descriptions. `None` on an error.
pub(crate) fn build_statistics(
    out: &mut Vec<u8>,
    seq: &SeqStore,
    prev: &FseTables,
    next: &mut FseTables,
    strategy: Strategy,
) -> Option<EncodingTypes> {
    let nb_seq = seq.sequences.len();
    let mut count = [0u32; MAX_SEQ + 1];
    let mut last_count_size = 0;
    // Literal lengths.
    let mut max = MAX_LL;
    let most_frequent = count_codes(&mut count, &mut max, &seq.ll_code);
    next.litlength_repeat = prev.litlength_repeat;
    let ll = select_encoding_type(
        &mut next.litlength_repeat,
        &count,
        max,
        most_frequent,
        nb_seq,
        LL_FSE_LOG,
        &prev.litlength,
        &LL_DEFAULT_NORM,
        LL_DEFAULT_NORM_LOG,
        true,
        strategy,
    );
    let size = build_ctable(
        out,
        &mut next.litlength,
        LL_FSE_LOG,
        ll,
        &mut count,
        max,
        &seq.ll_code,
        &LL_DEFAULT_NORM,
        LL_DEFAULT_NORM_LOG,
        MAX_LL,
        &prev.litlength,
    )?;
    if ll == SET_COMPRESSED {
        last_count_size = size;
    }
    // Offsets.
    let mut max = MAX_OFF;
    let most_frequent = count_codes(&mut count, &mut max, &seq.of_code);
    let default_allowed = max <= DEFAULT_MAX_OFF;
    next.offcode_repeat = prev.offcode_repeat;
    let of = select_encoding_type(
        &mut next.offcode_repeat,
        &count,
        max,
        most_frequent,
        nb_seq,
        OFF_FSE_LOG,
        &prev.offcode,
        &OF_DEFAULT_NORM,
        OF_DEFAULT_NORM_LOG,
        default_allowed,
        strategy,
    );
    let size = build_ctable(
        out,
        &mut next.offcode,
        OFF_FSE_LOG,
        of,
        &mut count,
        max,
        &seq.of_code,
        &OF_DEFAULT_NORM,
        OF_DEFAULT_NORM_LOG,
        DEFAULT_MAX_OFF,
        &prev.offcode,
    )?;
    if of == SET_COMPRESSED {
        last_count_size = size;
    }
    // Match lengths.
    let mut max = MAX_ML;
    let most_frequent = count_codes(&mut count, &mut max, &seq.ml_code);
    next.matchlength_repeat = prev.matchlength_repeat;
    let ml = select_encoding_type(
        &mut next.matchlength_repeat,
        &count,
        max,
        most_frequent,
        nb_seq,
        ML_FSE_LOG,
        &prev.matchlength,
        &ML_DEFAULT_NORM,
        ML_DEFAULT_NORM_LOG,
        true,
        strategy,
    );
    let size = build_ctable(
        out,
        &mut next.matchlength,
        ML_FSE_LOG,
        ml,
        &mut count,
        max,
        &seq.ml_code,
        &ML_DEFAULT_NORM,
        ML_DEFAULT_NORM_LOG,
        MAX_ML,
        &prev.matchlength,
    )?;
    if ml == SET_COMPRESSED {
        last_count_size = size;
    }
    Some(EncodingTypes {
        ll,
        of,
        ml,
        last_count_size,
    })
}

/// `ZSTD_encodeSequences`: appends the sequences bit stream; returns its size.
pub(crate) fn encode_sequences(out: &mut Vec<u8>, tables: &FseTables, seq: &SeqStore) -> usize {
    let n_seq = seq.sequences.len();
    let (ml_codes, of_codes, ll_codes) = (&seq.ml_code, &seq.of_code, &seq.ll_code);
    let mut bits = BitWriter::new(out);
    let last = n_seq - 1;
    let mut state_ml = tables.matchlength.init_state2(ml_codes[last]);
    let mut state_of = tables.offcode.init_state2(of_codes[last]);
    let mut state_ll = tables.litlength.init_state2(ll_codes[last]);
    let sequence = &seq.sequences[last];
    bits.add_bits(
        u64::from(sequence.lit_length),
        u32::from(LL_BITS[ll_codes[last] as usize]),
    );
    bits.add_bits(
        u64::from(sequence.ml_base),
        u32::from(ML_BITS[ml_codes[last] as usize]),
    );
    bits.add_bits(u64::from(sequence.off_base), u32::from(of_codes[last]));
    bits.flush();
    for n in (0..last).rev() {
        let (ll_code, of_code, ml_code) = (ll_codes[n], of_codes[n], ml_codes[n]);
        let ll_bits = u32::from(LL_BITS[ll_code as usize]);
        let of_bits = u32::from(of_code);
        let ml_bits = u32::from(ML_BITS[ml_code as usize]);
        tables.offcode.encode(&mut bits, &mut state_of, of_code);
        tables.matchlength.encode(&mut bits, &mut state_ml, ml_code);
        tables.litlength.encode(&mut bits, &mut state_ll, ll_code);
        if of_bits + ml_bits + ll_bits >= 64 - 7 - (LL_FSE_LOG + ML_FSE_LOG + OFF_FSE_LOG) {
            bits.flush();
        }
        let sequence = &seq.sequences[n];
        bits.add_bits(u64::from(sequence.lit_length), ll_bits);
        bits.add_bits(u64::from(sequence.ml_base), ml_bits);
        if of_bits + ml_bits + ll_bits > 56 {
            bits.flush();
        }
        bits.add_bits(u64::from(sequence.off_base), of_bits);
        bits.flush();
    }
    tables.matchlength.flush_state(&mut bits, state_ml);
    tables.offcode.flush_state(&mut bits, state_of);
    tables.litlength.flush_state(&mut bits, state_ll);
    bits.close()
}
