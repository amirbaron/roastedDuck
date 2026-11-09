//! Fast Static Symbol Table string compression (DuckDB's vendored `third_party/fsst`:
//! `libfsst.hpp`, `libfsst.cpp` and the `fsst.h` API).
//!
//! A symbol table maps up to 255 one-byte codes to symbols of 1 to 8 bytes; byte 255
//! escapes a literal byte. The encoder builds the table from a sample of the strings in
//! five rounds and serialises it with [`FsstEncoder::export`]; [`FsstDecoder::import`]
//! reads it back. The construction follows the reference implementation step by step
//! (including its 8/4-bit split counters and its sampling hash), so the same input gives
//! the same symbol table and the same compressed bytes as DuckDB.

use roastedduck_common::libcxx::UnorderedOrder;
use std::collections::HashMap;

/// `FSST_VERSION` (`FSST_VERSION_20190218`).
const FSST_VERSION: u64 = 20_190_218;
/// `FSST_ENDIAN_MARKER`.
const FSST_ENDIAN_MARKER: u64 = 1;
/// The escape code (`FSST_ESC`).
pub(crate) const FSST_ESC: u8 = 255;

const FSST_LEN_BITS: u32 = 12;
const FSST_CODE_BITS: u32 = 9;
/// The first real code; codes below are escaped bytes (`FSST_CODE_BASE`).
const FSST_CODE_BASE: u32 = 256;
/// `FSST_CODE_MAX`.
const FSST_CODE_MAX: u32 = 1 << FSST_CODE_BITS;
/// `FSST_CODE_MASK`.
const FSST_CODE_MASK: u32 = FSST_CODE_MAX - 1;
const FSST_HASH_LOG2SIZE: u32 = 10;
const FSST_HASH_PRIME: u64 = 2_971_215_073;
const FSST_SHIFT: u32 = 15;
/// High bits of `icl` marking a free hash table bucket (`FSST_ICL_FREE`).
const FSST_ICL_FREE: u64 = (15 << 28) | ((FSST_CODE_MASK as u64) << 16);
/// The sample size the symbol table is built from (`FSST_SAMPLETARGET`).
const FSST_SAMPLETARGET: usize = 1 << 14;
/// `FSST_SAMPLEMAXSZ`.
const FSST_SAMPLEMAXSZ: usize = 2 * FSST_SAMPLETARGET;
/// `FSST_SAMPLELINE`.
const FSST_SAMPLELINE: usize = 512;
const HASH_TAB_SIZE: usize = 1 << FSST_HASH_LOG2SIZE;

/// The size of DuckDB's `duckdb_fsst_decoder_t`: version, zero-terminated flag, 255
/// lengths and 255 symbols (with alignment padding).
pub(crate) const FSST_DECODER_SIZE: usize = 2304;

/// `FSST_HASH`.
#[inline]
const fn fsst_hash(w: u64) -> u64 {
    let product = w.wrapping_mul(FSST_HASH_PRIME);
    product ^ (product >> FSST_SHIFT)
}

/// `fsst_unaligned_load` of 8 bytes (zero padded past the end of `bytes`).
#[inline]
fn load_u64(bytes: &[u8], offset: usize) -> u64 {
    let mut raw = [0u8; 8];
    let end = (offset + 8).min(bytes.len());
    if offset < end {
        raw[..end - offset].copy_from_slice(&bytes[offset..end]);
    }
    u64::from_le_bytes(raw)
}

/// A symbol of up to 8 bytes with its code and length (`Symbol`): `icl` holds
/// `length:4 << 28 | code:12 << 16 | ignored bits`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Symbol {
    val: u64,
    icl: u64,
}

impl Symbol {
    /// A single-byte symbol with `code` (`Symbol(u8, u16)`).
    fn single(byte: u8, code: u32) -> Self {
        Self {
            val: u64::from(byte),
            icl: (1 << 28) | (u64::from(code) << 16) | 0x38,
        }
    }

    /// The first (up to) 8 bytes of `input` (`Symbol(const char *, u32)`).
    fn from_bytes(input: &[u8]) -> Self {
        let len = input.len().min(8);
        let mut raw = [0u8; 8];
        raw[..len].copy_from_slice(&input[..len]);
        let mut symbol = Self {
            val: u64::from_le_bytes(raw),
            icl: 0,
        };
        symbol.set_code_len(FSST_CODE_MAX, len as u32);
        symbol
    }

    fn set_code_len(&mut self, code: u32, len: u32) {
        self.icl = (u64::from(len) << 28) | (u64::from(code) << 16) | u64::from((8 - len) * 8);
    }

    fn length(self) -> u32 {
        (self.icl >> 28) as u32
    }

    fn code(self) -> u32 {
        ((self.icl >> 16) as u32) & FSST_CODE_MASK
    }

    /// The low byte of `icl`: the number of unused high bits of `val`.
    fn ignored_bits(self) -> u32 {
        u32::from(self.icl as u8)
    }

    fn first(self) -> usize {
        (self.val & 0xFF) as usize
    }

    fn first2(self) -> usize {
        (self.val & 0xFFFF) as usize
    }

    fn hash(self) -> u64 {
        fsst_hash(self.val & 0xFF_FFFF)
    }

    /// The symbol's bytes.
    fn bytes(self) -> Vec<u8> {
        self.val.to_le_bytes()[..self.length() as usize].to_vec()
    }
}

/// `concat`: `a` followed by `b`, cut to 8 bytes.
fn concat(a: Symbol, b: Symbol) -> Symbol {
    let length = (a.length() + b.length()).min(8);
    let mut symbol = Symbol {
        val: (b.val.checked_shl(8 * a.length()).unwrap_or(0)) | a.val,
        icl: 0,
    };
    symbol.set_code_len(FSST_CODE_MASK, length);
    symbol
}

/// `mask >> ignored_bits` without overflowing on 64.
#[inline]
fn keep_mask(ignored_bits: u32) -> u64 {
    u64::MAX.checked_shr(ignored_bits).unwrap_or(0)
}

/// The symbol table of an encoder (`SymbolTable`).
#[derive(Clone)]
struct SymbolTable {
    /// Code for the next two bytes (2-byte symbols), else for the first byte.
    short_codes: Vec<u16>,
    /// Code for every 1-byte symbol, else the escape pseudo code.
    byte_codes: [u16; 256],
    /// Symbols by code: `0..256` pseudo symbols (escaped bytes), then real symbols.
    symbols: Vec<Symbol>,
    /// Symbols of 3 and more bytes, by the hash of their first 3 bytes.
    hash_tab: Vec<Symbol>,
    n_symbols: u16,
    suffix_lim: u16,
    terminator: u16,
    zero_terminated: bool,
    /// `len_histo[x]`: the number of symbols of length `x + 1`.
    len_histo: [u16; FSST_CODE_BITS as usize],
}

impl std::fmt::Debug for SymbolTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SymbolTable")
            .field("n_symbols", &self.n_symbols)
            .field("suffix_lim", &self.suffix_lim)
            .field("terminator", &self.terminator)
            .finish_non_exhaustive()
    }
}

impl SymbolTable {
    fn new() -> Self {
        let mut symbols = vec![Symbol::default(); FSST_CODE_MAX as usize];
        for (index, symbol) in symbols.iter_mut().enumerate().take(256) {
            *symbol = Symbol::single(index as u8, index as u32 | (1 << FSST_LEN_BITS));
        }
        let unused = Symbol::single(0, FSST_CODE_MASK);
        for symbol in &mut symbols[256..] {
            *symbol = unused;
        }
        let free = Symbol {
            val: 0,
            icl: FSST_ICL_FREE,
        };
        Self {
            short_codes: (0..65_536u32)
                .map(|i| ((1 << FSST_LEN_BITS) | (i & 255)) as u16)
                .collect(),
            byte_codes: std::array::from_fn(|i| ((1 << FSST_LEN_BITS) | i) as u16),
            symbols,
            hash_tab: vec![free; HASH_TAB_SIZE],
            n_symbols: 0,
            suffix_lim: FSST_CODE_MAX as u16,
            terminator: 0,
            zero_terminated: false,
            len_histo: [0; FSST_CODE_BITS as usize],
        }
    }

    /// `clear`: forgets the symbols, resetting only the entries they used.
    fn clear(&mut self) {
        self.len_histo = [0; FSST_CODE_BITS as usize];
        for index in FSST_CODE_BASE as usize..FSST_CODE_BASE as usize + usize::from(self.n_symbols) {
            let symbol = self.symbols[index];
            if symbol.length() == 1 {
                let value = symbol.first();
                self.byte_codes[value] = ((1 << FSST_LEN_BITS) | value) as u16;
            } else if symbol.length() == 2 {
                let value = symbol.first2();
                self.short_codes[value] = ((1 << FSST_LEN_BITS) | (value & 255)) as u16;
            } else {
                let index = (symbol.hash() & (HASH_TAB_SIZE as u64 - 1)) as usize;
                self.hash_tab[index] = Symbol {
                    val: 0,
                    icl: FSST_ICL_FREE,
                };
            }
        }
        self.n_symbols = 0;
    }

    fn hash_insert(&mut self, symbol: Symbol) -> bool {
        let index = (symbol.hash() & (HASH_TAB_SIZE as u64 - 1)) as usize;
        if self.hash_tab[index].icl < FSST_ICL_FREE {
            return false;
        }
        self.hash_tab[index] = Symbol {
            val: symbol.val & keep_mask(symbol.ignored_bits()),
            icl: symbol.icl,
        };
        true
    }

    fn add(&mut self, mut symbol: Symbol) -> bool {
        let len = symbol.length();
        let code = FSST_CODE_BASE + u32::from(self.n_symbols);
        symbol.set_code_len(code, len);
        if len == 1 {
            self.byte_codes[symbol.first()] = (code + (1 << FSST_LEN_BITS)) as u16;
        } else if len == 2 {
            self.short_codes[symbol.first2()] = (code + (2 << FSST_LEN_BITS)) as u16;
        } else if !self.hash_insert(symbol) {
            return false;
        }
        self.symbols[code as usize] = symbol;
        self.n_symbols += 1;
        self.len_histo[len as usize - 1] += 1;
        true
    }

    /// `findLongestSymbol`: the code of the longest symbol `symbol` starts with.
    fn find_longest_symbol(&self, symbol: Symbol) -> u32 {
        let index = (symbol.hash() & (HASH_TAB_SIZE as u64 - 1)) as usize;
        let entry = self.hash_tab[index];
        if entry.icl <= symbol.icl && entry.val == (symbol.val & keep_mask(entry.ignored_bits())) {
            return ((entry.icl >> 16) as u32) & FSST_CODE_MASK;
        }
        if symbol.length() >= 2 {
            let code = u32::from(self.short_codes[symbol.first2()]) & FSST_CODE_MASK;
            if code >= FSST_CODE_BASE {
                return code;
            }
        }
        u32::from(self.byte_codes[symbol.first()]) & FSST_CODE_MASK
    }

    /// `finalize`: renumbers the codes grouped by length (2..8, then 1) with the 2-byte
    /// codes without a longer suffix first, and folds the 1-byte codes into `short_codes`.
    fn finalize(&mut self, zero_terminated: u8) {
        let mut new_code = [0u8; 256];
        let mut rsum = [0u8; 8];
        let byte_lim = (u32::from(self.n_symbols))
            .wrapping_sub(u32::from(self.len_histo[0]).wrapping_sub(u32::from(zero_terminated)))
            as u8;
        rsum[0] = byte_lim;
        rsum[1] = zero_terminated;
        for i in 1..7 {
            rsum[i + 1] = rsum[i].wrapping_add(self.len_histo[i] as u8);
        }
        self.suffix_lim = u16::from(rsum[1]);
        new_code[0] = 0;
        self.symbols[0] = self.symbols[256];
        let mut j = u32::from(rsum[2]);
        for i in u32::from(zero_terminated)..u32::from(self.n_symbols) {
            let mut s1 = self.symbols[(FSST_CODE_BASE + i) as usize];
            let len = s1.length();
            let mut opt = if len == 2 { u32::from(self.n_symbols) } else { 0 };
            if opt > 0 {
                let first2 = s1.first2();
                let mut k = 0;
                while k < opt {
                    let s2 = self.symbols[(FSST_CODE_BASE + k) as usize];
                    if k != i && s2.length() > 1 && first2 == s2.first2() {
                        opt = 0;
                    }
                    k += 1;
                }
                new_code[i as usize] = if opt > 0 {
                    let code = self.suffix_lim as u8;
                    self.suffix_lim = self.suffix_lim.wrapping_add(1);
                    code
                } else {
                    j = j.wrapping_sub(1);
                    j as u8
                };
            } else {
                let slot = &mut rsum[len as usize - 1];
                new_code[i as usize] = *slot;
                *slot = slot.wrapping_add(1);
            }
            s1.set_code_len(u32::from(new_code[i as usize]), len);
            self.symbols[usize::from(new_code[i as usize])] = s1;
        }
        for code in &mut self.byte_codes {
            *code = if u32::from(*code) & FSST_CODE_MASK >= FSST_CODE_BASE {
                u16::from(new_code[usize::from(*code as u8)]) + (1 << FSST_LEN_BITS)
            } else {
                511 + (1 << FSST_LEN_BITS)
            };
        }
        for i in 0..self.short_codes.len() {
            let code = self.short_codes[i];
            self.short_codes[i] = if u32::from(code) & FSST_CODE_MASK >= FSST_CODE_BASE {
                u16::from(new_code[usize::from(code as u8)]) + (code & (15 << FSST_LEN_BITS))
            } else {
                self.byte_codes[i & 0xFF]
            };
        }
        for index in 0..HASH_TAB_SIZE {
            if self.hash_tab[index].icl < FSST_ICL_FREE {
                let code = self.hash_tab[index].code() as u8;
                self.hash_tab[index] = self.symbols[usize::from(new_code[usize::from(code)])];
            }
        }
    }
}

/// Symbol and symbol pair counters (`Counters`, the optimized layout): 16-bit single
/// counters split in a high and a low byte, 12-bit pair counters split in a low byte and a
/// high nibble. The arrays are laid out back to back like the C++ struct, because the
/// skip-ahead reads of `count1_get_next` / `count2_get_next` read 8 bytes at a time and
/// may run into the next array.
struct Counters {
    bytes: Vec<u8>,
}

const COUNT1_HIGH: usize = 0;
const COUNT1_LOW: usize = COUNT1_HIGH + FSST_CODE_MAX as usize;
const COUNT2_HIGH: usize = COUNT1_LOW + FSST_CODE_MAX as usize;
const COUNT2_LOW: usize = COUNT2_HIGH + (FSST_CODE_MAX * FSST_CODE_MAX / 2) as usize;
const COUNTERS_SIZE: usize = COUNT2_LOW + (FSST_CODE_MAX * FSST_CODE_MAX) as usize;

impl Counters {
    fn new() -> Self {
        Self {
            bytes: vec![0; COUNTERS_SIZE + 8],
        }
    }

    fn reset(&mut self) {
        self.bytes.fill(0);
    }

    fn count1_set(&mut self, pos1: usize, value: u16) {
        self.bytes[COUNT1_LOW + pos1] = value as u8;
        self.bytes[COUNT1_HIGH + pos1] = (value >> 8) as u8;
    }

    fn count1_inc(&mut self, pos1: usize) {
        let low = &mut self.bytes[COUNT1_LOW + pos1];
        let was_zero = *low == 0;
        *low = low.wrapping_add(1);
        if was_zero {
            let high = &mut self.bytes[COUNT1_HIGH + pos1];
            *high = high.wrapping_add(1);
        }
    }

    fn count2_inc(&mut self, pos1: usize, pos2: usize) {
        let low = &mut self.bytes[COUNT2_LOW + pos1 * FSST_CODE_MAX as usize + pos2];
        let was_zero = *low == 0;
        *low = low.wrapping_add(1);
        if was_zero {
            let high = &mut self.bytes[COUNT2_HIGH + pos1 * (FSST_CODE_MAX as usize / 2) + (pos2 >> 1)];
            *high = high.wrapping_add(1 << ((pos2 & 1) << 2));
        }
    }

    /// `count1GetNext`: the count at `pos1`, first advancing `pos1` past zero counters.
    fn count1_get_next(&self, pos1: &mut usize) -> u32 {
        let mut high = load_u64(&self.bytes, COUNT1_HIGH + *pos1);
        let zero = if high == 0 {
            7
        } else {
            high.trailing_zeros() as usize >> 3
        };
        high = (high >> (zero << 3)) & 255;
        *pos1 += zero;
        if *pos1 >= FSST_CODE_MAX as usize || high == 0 {
            return 0;
        }
        let low = u64::from(self.bytes[COUNT1_LOW + *pos1]);
        if low != 0 {
            high -= 1;
        }
        ((high << 8) + low) as u32
    }

    /// `count2GetNext`: the count at `(pos1, pos2)`, first advancing `pos2` past zero
    /// counters.
    fn count2_get_next(&self, pos1: usize, pos2: &mut usize) -> u32 {
        let mut high = load_u64(
            &self.bytes,
            COUNT2_HIGH + pos1 * (FSST_CODE_MAX as usize / 2) + (*pos2 >> 1),
        );
        high >>= (*pos2 & 1) << 2;
        let zero = if high == 0 {
            15 - (*pos2 & 1)
        } else {
            high.trailing_zeros() as usize >> 2
        };
        high = (high >> (zero << 2)) & 15;
        *pos2 += zero;
        if *pos2 >= FSST_CODE_MAX as usize || high == 0 {
            return 0;
        }
        let low = u64::from(self.bytes[COUNT2_LOW + pos1 * FSST_CODE_MAX as usize + *pos2]);
        if low != 0 {
            high -= 1;
        }
        ((high << 8) + low) as u32
    }

    fn backup1(&self) -> Vec<u8> {
        self.bytes[COUNT1_HIGH..COUNT2_HIGH].to_vec()
    }

    fn restore1(&mut self, backup: &[u8]) {
        self.bytes[COUNT1_HIGH..COUNT2_HIGH].copy_from_slice(backup);
    }
}

/// A candidate symbol with its gain (`QSymbol`), keyed by its bytes and length.
type CandidateKey = (u64, u32);

/// `buildSymbolTable`.
fn build_symbol_table(counters: &mut Counters, lines: &[&[u8]]) -> SymbolTable {
    let mut st = SymbolTable::new();
    let mut best_table = SymbolTable::new();
    let mut best_gain = -(FSST_SAMPLEMAXSZ as i32);
    // The terminator is the (lowest) least frequent byte.
    let mut byte_histo = [0u16; 256];
    for line in lines {
        for &byte in *line {
            byte_histo[usize::from(byte)] = byte_histo[usize::from(byte)].wrapping_add(1);
        }
    }
    let mut min_size = FSST_SAMPLEMAXSZ as u32;
    st.terminator = 256;
    for i in (0..256).rev() {
        if u32::from(byte_histo[i]) > min_size {
            continue;
        }
        st.terminator = i as u16;
        min_size = u32::from(byte_histo[i]);
    }

    let mut best_counters = Vec::new();
    let mut sample_frac: usize = 8;
    loop {
        counters.reset();
        let gain = compress_count(&st, counters, lines, sample_frac);
        if gain >= best_gain {
            best_table = st.clone();
            best_gain = gain;
            best_counters = counters.backup1();
        }
        if sample_frac >= 128 {
            break;
        }
        make_table(&mut st, counters, sample_frac);
        sample_frac += 30;
    }
    counters.restore1(&best_counters);
    make_table(&mut best_table, counters, sample_frac);
    best_table.finalize(0);
    best_table
}

/// `rnd128`: a pseudo random number between 1 and 128 for line `i`.
fn rnd128(i: usize, sample_frac: usize) -> usize {
    1 + (fsst_hash(((i + 1) * sample_frac) as u64) & 127) as usize
}

/// `compressCount`: compresses the sample with `st`, counting symbols and symbol pairs;
/// returns the gain.
fn compress_count(st: &SymbolTable, counters: &mut Counters, lines: &[&[u8]], sample_frac: usize) -> i32 {
    let mut gain: i32 = 0;
    let escape = |code: u32| i32::from(code < FSST_CODE_BASE);
    for (i, line) in lines.iter().enumerate() {
        if sample_frac < 128 && rnd128(i, sample_frac) > sample_frac {
            continue;
        }
        let end = line.len();
        if end == 0 {
            continue;
        }
        let mut start = 0;
        let mut code1 = st.find_longest_symbol(Symbol::from_bytes(line));
        let mut cur = st.symbols[code1 as usize].length() as usize;
        gain += st.symbols[code1 as usize].length() as i32 - (1 + escape(code1));
        loop {
            counters.count1_inc(code1 as usize);
            if st.symbols[code1 as usize].length() != 1 {
                counters.count1_inc(usize::from(line[start]));
            }
            if cur == end {
                break;
            }
            start = cur;
            let code2;
            if cur + 7 < end {
                let mut word = load_u64(line, cur);
                let code = word & 0xFF_FFFF;
                let index = (fsst_hash(code) & (HASH_TAB_SIZE as u64 - 1)) as usize;
                let symbol = st.hash_tab[index];
                let short = u32::from(st.short_codes[(word & 0xFFFF) as usize]) & FSST_CODE_MASK;
                word &= keep_mask(symbol.ignored_bits());
                if symbol.icl < FSST_ICL_FREE && symbol.val == word {
                    code2 = symbol.code();
                    cur += symbol.length() as usize;
                } else if short >= FSST_CODE_BASE {
                    code2 = short;
                    cur += 2;
                } else {
                    code2 = u32::from(st.byte_codes[(word & 0xFF) as usize]) & FSST_CODE_MASK;
                    cur += 1;
                }
            } else {
                code2 = st.find_longest_symbol(Symbol::from_bytes(&line[cur..end]));
                cur += st.symbols[code2 as usize].length() as usize;
            }
            gain += (cur - start) as i32 - (1 + escape(code2));
            if sample_frac < 128 {
                counters.count2_inc(code1 as usize, code2 as usize);
                if cur - start > 1 {
                    counters.count2_inc(code1 as usize, usize::from(line[start]));
                }
            }
            code1 = code2;
        }
    }
    gain
}

// Frozen QSymbol hashing deliberately ignores length. Different lengths of a
// zero-padded symbol can therefore have identical hashes and queue priorities.
fn candidate_hash(mut value: u64) -> u64 {
    const M: u64 = 0xc6a4_a793_5bd1_e995;
    let mut hash = 0x8445_d61a_4e77_4912 ^ 8u64.wrapping_mul(M);
    value = value.wrapping_mul(M);
    value ^= value >> 47;
    value = value.wrapping_mul(M);
    hash ^= value;
    hash = hash.wrapping_mul(M);
    hash ^= hash >> 47;
    hash = hash.wrapping_mul(M);
    hash ^ (hash >> 47)
}

type Candidate = (Symbol, u32);

fn candidate_less(a: Candidate, b: Candidate) -> bool {
    a.1 < b.1 || (a.1 == b.1 && a.0.val > b.0.val)
}

// Match libc++ priority_queue, including equal-priority items: push_heap stops
// on ties; pop_heap uses Floyd's descent over the original (not shortened) heap
// and then sifts the last item up. Sorting with a length tie-break changes FSST.
fn candidate_sift_up(queue: &mut [Candidate], mut hole: usize) {
    let value = queue[hole];
    while hole > 0 {
        let parent = (hole - 1) / 2;
        if !candidate_less(queue[parent], value) {
            break;
        }
        queue[hole] = queue[parent];
        hole = parent;
    }
    queue[hole] = value;
}

fn candidate_pop(queue: &mut Vec<Candidate>) -> Option<Candidate> {
    let top = *queue.first()?;
    let len = queue.len();
    if len > 1 {
        let mut hole = 0;
        loop {
            let mut child = 2 * hole + 1;
            if child + 1 < len && candidate_less(queue[child], queue[child + 1]) {
                child += 1;
            }
            queue[hole] = queue[child];
            hole = child;
            if hole > (len - 2) / 2 {
                break;
            }
        }
        if hole != len - 1 {
            queue[hole] = queue[len - 1];
            candidate_sift_up(queue, hole);
        }
    }
    queue.pop();
    Some(top)
}

/// `makeTable`: rebuilds `st` from the 255 candidate symbols with the highest gain.
fn make_table(st: &mut SymbolTable, counters: &mut Counters, sample_frac: usize) {
    let mut candidates: HashMap<CandidateKey, Candidate> = HashMap::new();
    let mut order = UnorderedOrder::default();
    let terminator = if st.n_symbols > 0 {
        FSST_CODE_BASE as usize
    } else {
        usize::from(st.terminator)
    };
    counters.count1_set(terminator, 65535);
    let mut add_or_inc = |symbol: Symbol, count: u64| {
        if count < (5 * sample_frac as u64) / 128 {
            return;
        }
        let gain = count.wrapping_mul(u64::from(symbol.length())) as u32;
        let key = (symbol.val, symbol.length());
        let hash = candidate_hash(symbol.val);
        order.remove(hash, &key, |a, b| a == b);
        order.insert(hash, key, |a, b| a == b);
        let entry = candidates.entry(key).or_insert((symbol, 0));
        entry.0 = symbol;
        entry.1 = entry.1.wrapping_add(gain);
    };
    let limit = FSST_CODE_BASE as usize + usize::from(st.n_symbols);
    let mut pos1 = 0;
    while pos1 < limit {
        let cnt1 = counters.count1_get_next(&mut pos1);
        if cnt1 != 0 {
            let s1 = st.symbols[pos1];
            let weight = if s1.length() == 1 { 8 } else { 1 };
            add_or_inc(s1, weight * u64::from(cnt1));
            let extendable =
                sample_frac < 128 && s1.length() != 8 && u16::from(s1.val as u8) != st.terminator;
            if extendable {
                let mut pos2 = 0;
                while pos2 < limit {
                    let cnt2 = counters.count2_get_next(pos1, &mut pos2);
                    if cnt2 != 0 {
                        let s2 = st.symbols[pos2];
                        let s3 = concat(s1, s2);
                        if u16::from(s2.val as u8) != st.terminator {
                            add_or_inc(s3, u64::from(cnt2));
                        }
                    }
                    pos2 += 1;
                }
            }
        }
        pos1 += 1;
    }
    let mut queue = Vec::with_capacity(candidates.len());
    for index in order.order() {
        queue.push(candidates[&order.keys()[index]]);
        let last = queue.len() - 1;
        candidate_sift_up(&mut queue, last);
    }
    st.clear();
    while st.n_symbols < 255 {
        let Some((symbol, _)) = candidate_pop(&mut queue) else {
            break;
        };
        st.add(symbol);
    }
}

/// `makeSample`: the lines to build the table from: all of them when they are small,
/// otherwise a pseudo random selection of chunks of at most 512 bytes summing to at
/// least 16 KiB.
fn make_sample(strings: &[&[u8]]) -> Vec<Vec<u8>> {
    let total: usize = strings.iter().map(|s| s.len()).sum();
    if total < FSST_SAMPLETARGET {
        return strings.iter().map(|s| s.to_vec()).collect();
    }
    let lines = strings.len().max(1);
    let mut sample = Vec::new();
    let mut sample_size = 0;
    let mut rnd = fsst_hash(4_637_947);
    while sample_size < FSST_SAMPLETARGET {
        rnd = fsst_hash(rnd);
        let mut line = (rnd % lines as u64) as usize;
        while strings[line].is_empty() {
            line += 1;
            if line == lines {
                line = 0;
            }
        }
        let len = strings[line].len();
        let chunks = 1 + (len - 1) / FSST_SAMPLELINE;
        rnd = fsst_hash(rnd);
        let chunk = FSST_SAMPLELINE * (rnd % chunks as u64) as usize;
        let take = (len - chunk).min(FSST_SAMPLELINE);
        sample.push(strings[line][chunk..chunk + take].to_vec());
        sample_size += take;
    }
    sample
}

/// An FSST encoder: a finalized symbol table (`duckdb_fsst_encoder_t`).
#[derive(Clone, Debug)]
pub(crate) struct FsstEncoder {
    table: SymbolTable,
}

impl FsstEncoder {
    /// `duckdb_fsst_create` (not zero-terminated): builds a symbol table for `strings`.
    pub(crate) fn create(strings: &[&[u8]]) -> Self {
        let sample = make_sample(strings);
        let lines: Vec<&[u8]> = sample.iter().map(Vec::as_slice).collect();
        let mut counters = Counters::new();
        Self {
            table: build_symbol_table(&mut counters, &lines),
        }
    }

    /// `duckdb_fsst_export`: the serialized symbol table (at most [`FSST_DECODER_SIZE`]
    /// bytes).
    pub(crate) fn export(&self) -> Vec<u8> {
        let st = &self.table;
        let version = (FSST_VERSION << 32)
            | (u64::from(st.suffix_lim) << 24)
            | (u64::from(st.terminator) << 16)
            | (u64::from(st.n_symbols) << 8)
            | FSST_ENDIAN_MARKER;
        let mut buf = Vec::with_capacity(FSST_DECODER_SIZE);
        buf.extend_from_slice(&version.to_le_bytes());
        buf.push(u8::from(st.zero_terminated));
        for i in 0..8 {
            buf.push(st.len_histo[i] as u8);
        }
        for symbol in &st.symbols[usize::from(st.zero_terminated)..usize::from(st.n_symbols)] {
            buf.extend_from_slice(&symbol.bytes());
        }
        buf
    }

    /// `duckdb_fsst_compress`: compresses every string (`compressAuto` / `compressBulk`).
    pub(crate) fn compress(&self, strings: &[&[u8]]) -> Vec<Vec<u8>> {
        self.compress_with_limit(strings, usize::MAX)
    }

    /// `duckdb_fsst_compress` into an output buffer of `size` bytes: compression stops at the
    /// first string whose next 511-byte chunk may not fit (`2 * chunk + 7` bytes); only the
    /// strings compressed completely are returned.
    pub(crate) fn compress_with_limit(&self, strings: &[&[u8]], size: usize) -> Vec<Vec<u8>> {
        let st = &self.table;
        let mut no_suffix_opt = false;
        let mut avoid_branch = false;
        let histo = |i: usize| u32::from(st.len_histo[i]);
        let n_symbols = u32::from(st.n_symbols);
        if 100 * histo(1) > 65 * n_symbols && 100 * u32::from(st.suffix_lim) > 95 * histo(1) {
            no_suffix_opt = true;
        } else if (histo(0) > 24 && histo(0) < 92)
            && (histo(0) < 43 || histo(6) + histo(7) < 29)
            && (histo(0) < 72 || histo(2) < 72)
        {
            avoid_branch = true;
        }
        let mut buf = [0u8; 512 + 8];
        let mut used = 0usize;
        let mut result = Vec::with_capacity(strings.len());
        'lines: for string in strings {
            let mut out = Vec::with_capacity(string.len() * 2 + 7);
            let mut offset = 0;
            loop {
                let chunk = (string.len() - offset).min(511);
                if 2 * chunk + 7 > size.saturating_sub(used + out.len()) {
                    break 'lines;
                }
                buf[..chunk].copy_from_slice(&string[offset..offset + chunk]);
                buf[chunk] = st.terminator as u8;
                compress_chunk(st, &buf, chunk, no_suffix_opt, avoid_branch, &mut out);
                offset += chunk;
                if offset >= string.len() {
                    break;
                }
            }
            used += out.len();
            result.push(out);
        }
        result
    }
}

/// `compressVariant` of `compressBulk` over `buf[..end]` (followed by the terminator).
fn compress_chunk(
    st: &SymbolTable,
    buf: &[u8; 520],
    end: usize,
    no_suffix_opt: bool,
    avoid_branch: bool,
    out: &mut Vec<u8>,
) {
    let suffix_lim = st.suffix_lim;
    let byte_lim = (u32::from(st.n_symbols) + u32::from(st.zero_terminated))
        .wrapping_sub(u32::from(st.len_histo[0])) as u8;
    let mut cur = 0;
    while cur < end {
        let mut word = load_u64(buf, cur);
        let code = u32::from(st.short_codes[(word & 0xFFFF) as usize]);
        if no_suffix_opt && u16::from(code as u8) < suffix_lim {
            out.push(code as u8);
            cur += 2;
            continue;
        }
        let index = (fsst_hash(word & 0xFF_FFFF) & (HASH_TAB_SIZE as u64 - 1)) as usize;
        let symbol = st.hash_tab[index];
        let literal = word as u8;
        word &= keep_mask(symbol.ignored_bits());
        if symbol.icl < FSST_ICL_FREE && symbol.val == word {
            out.push(symbol.code() as u8);
            cur += symbol.length() as usize;
        } else if avoid_branch {
            out.push(code as u8);
            if code & FSST_CODE_BASE != 0 {
                out.push(literal);
            }
            cur += (code >> FSST_LEN_BITS) as usize;
        } else if (code as u8) < byte_lim {
            out.push(code as u8);
            cur += 2;
        } else {
            out.push(code as u8);
            if code & FSST_CODE_BASE != 0 {
                out.push(literal);
            }
            cur += 1;
        }
    }
}

/// An FSST decoder (`duckdb_fsst_decoder_t`).
#[derive(Clone, Debug)]
pub(crate) struct FsstDecoder {
    zero_terminated: bool,
    /// Indexed by code; 256 entries (code 255, the escape, is never looked up) so that a
    /// `u8` index needs no bounds check. Every length is 1..=8.
    len: [u8; 256],
    symbol: [u64; 256],
}

/// `FSST_CORRUPT`: "corrupt" in little endian, the symbol of unused codes.
const FSST_CORRUPT: u64 = 32_774_747_032_022_883;

impl FsstDecoder {
    /// `duckdb_fsst_import`: reads a serialized symbol table; `None` when `buf` holds no
    /// symbol table (a version mismatch, e.g. the zeroed table of an all-empty segment).
    /// Returns the decoder and the number of bytes read.
    pub(crate) fn import(buf: &[u8]) -> Option<(Self, usize)> {
        let version = u64::from_le_bytes(buf.get(..8)?.try_into().ok()?);
        if version >> 32 != FSST_VERSION {
            return None;
        }
        let zero_terminated = buf.get(8)? & 1 == 1;
        let mut len_histo = [0u8; 8];
        len_histo.copy_from_slice(buf.get(9..17)?);
        let mut decoder = Self {
            zero_terminated,
            len: [8; 256],
            symbol: [FSST_CORRUPT; 256],
        };
        decoder.len[0] = 1;
        decoder.symbol[0] = 0;
        let mut code = usize::from(zero_terminated);
        if zero_terminated {
            len_histo[0] = len_histo[0].wrapping_sub(1);
        }
        let mut pos = 17;
        for l in 1..=8u32 {
            let count = len_histo[(l & 7) as usize];
            for _ in 0..count {
                if code >= 255 {
                    return None;
                }
                let len = (l & 7) as usize + 1;
                let mut raw = [0u8; 8];
                raw[..len].copy_from_slice(buf.get(pos..pos + len)?);
                decoder.len[code] = len as u8;
                decoder.symbol[code] = u64::from_le_bytes(raw);
                pos += len;
                code += 1;
            }
        }
        for slot in code..255 {
            decoder.symbol[slot] = FSST_CORRUPT;
            decoder.len[slot] = 8;
        }
        Some((decoder, pos))
    }

    /// `duckdb_fsst_decompress` into `out` (appending the whole decoded string). Like
    /// DuckDB, every symbol is written as a whole 8-byte word and the end advanced by its
    /// length, so no per-symbol copy of a variable length is needed.
    pub(crate) fn decompress_into(&self, input: &[u8], out: &mut Vec<u8>) {
        let start = out.len();
        // A code decodes to at most 8 bytes; the last word written may extend 8 bytes past
        // the decoded end.
        out.reserve(input.len() * 8 + 8);
        // SAFETY: the reserve above leaves `input.len() * 8 + 8` spare bytes at `start`.
        let written = unsafe { self.decompress_to(input, out.as_mut_ptr().add(start)) };
        // SAFETY: `decompress_to` initialised `written` bytes after `start`.
        unsafe { out.set_len(start + written) };
    }

    /// [`FsstDecoder::decompress_into`] without the capacity check: decodes `input` to
    /// `out` and returns the decoded length.
    ///
    /// # Safety
    /// `out` must be valid for writes of `input.len() * 8 + 8` bytes.
    #[inline]
    pub(crate) unsafe fn decompress_to(&self, input: &[u8], out: *mut u8) -> usize {
        let mut written = 0;
        let mut pos = 0;
        let store = |at: usize, symbol: u64| {
            // SAFETY: every code advances `written` by at most 8 (lengths are 1..=8), so
            // `at + 8 <= input.len() * 8 + 8`, within `out` (the caller's contract).
            unsafe { out.add(at).cast::<u64>().write_unaligned(symbol.to_le()) };
        };
        loop {
            // Four codes at a time while none of them is an escape (`fsst_decompress`'s
            // unrolled loop with its `escapeMask` test, which flags 0xFF bytes).
            while let Some(block) = input.get(pos..pos + 4) {
                let codes = u32::from_le_bytes([block[0], block[1], block[2], block[3]]);
                let escapes =
                    (codes & 0x8080_8080) & ((!codes & 0x7F7F_7F7F).wrapping_add(0x7F7F_7F7F) ^ 0x8080_8080);
                if escapes != 0 {
                    break;
                }
                for &code in block {
                    let index = usize::from(code);
                    store(written, self.symbol[index]);
                    written += usize::from(self.len[index]);
                }
                pos += 4;
            }
            if pos >= input.len() {
                break;
            }
            // One code (an escape among the next four, or the last few codes).
            let code = input[pos];
            pos += 1;
            if code < FSST_ESC {
                let index = usize::from(code);
                store(written, self.symbol[index]);
                written += usize::from(self.len[index]);
            } else {
                // An escaped literal byte (a truncated escape decodes to nothing).
                if let Some(&byte) = input.get(pos) {
                    // SAFETY: as in `store`, `written < input.len() * 8 + 8`.
                    unsafe { out.add(written).write(byte) };
                    written += 1;
                }
                pos += 1;
            }
        }
        let _ = self.zero_terminated;
        written
    }

    /// Decompresses `input` into a fresh buffer.
    #[cfg(test)]
    pub(crate) fn decompress(&self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len() * 3);
        self.decompress_into(input, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(strings: &[&[u8]]) -> Vec<Vec<u8>> {
        let encoder = FsstEncoder::create(strings);
        let table = encoder.export();
        assert!(table.len() <= FSST_DECODER_SIZE);
        let (decoder, read) = FsstDecoder::import(&table).expect("symbol table");
        assert_eq!(read, table.len());
        let compressed = encoder.compress(strings);
        for (original, packed) in strings.iter().zip(&compressed) {
            assert_eq!(&decoder.decompress(packed), original);
        }
        compressed
    }

    #[test]
    fn candidate_heap_ties_match_native_libcxx() {
        let mut queue = Vec::new();
        for i in 0..512u32 {
            let mut symbol = Symbol::single((i % 11) as u8, 0);
            symbol.set_code_len(0, 1 + i % 8);
            queue.push((symbol, 1 + i % 7));
            let last = queue.len() - 1;
            candidate_sift_up(&mut queue, last);
        }
        for line in include_str!("../../tests/data/fsst_heap_1.5.6.tsv").lines() {
            let expected: Vec<u64> = line.split('\t').map(|v| v.parse().unwrap()).collect();
            let (symbol, gain) = candidate_pop(&mut queue).unwrap();
            assert_eq!(
                [symbol.val, u64::from(symbol.length()), u64::from(gain)],
                expected.as_slice()
            );
        }
        assert!(candidate_pop(&mut queue).is_none());
    }

    #[test]
    fn nul_length_candidates_match_frozen_fsst_table_and_records() {
        // DuckDB 1.5.6's independently compiled vendored encoder. This was a
        // real counterexample: sorting equal-priority lengths produced 8 symbols
        // instead of 7 and different compressed rows (cross-decodes still passed).
        let strings: Vec<Vec<u8>> = (1..1000)
            .map(|i| {
                let mut value = b"a\0".repeat(i);
                value.resize(value.len() + i % 9, 0);
                value
            })
            .collect();
        let refs: Vec<&[u8]> = strings.iter().map(Vec::as_slice).collect();
        let encoder = FsstEncoder::create(&refs);
        let header = encoder.export();
        assert_eq!(
            header,
            [
                0x01, 0x07, 0x01, 0x01, 0x0a, 0x14, 0x34, 0x01, 0x00, 0x03, 0x02, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x02, 0x00, 0x00, 0x61, 0x00, 0x61, 0x00, 0x61, 0x00, 0x61, 0x00, 0x61, 0x00, 0x61,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x61, 0x00
            ]
        );
        let compressed = encoder.compress(&refs);
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in (compressed.len() as u64)
            .to_le_bytes()
            .into_iter()
            .chain(compressed.iter().flat_map(|row| {
                (row.len() as u64)
                    .to_le_bytes()
                    .into_iter()
                    .chain(row.iter().copied())
            }))
        {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
        }
        assert_eq!(hash, 0xabf2_5866_aef9_48ffu64);
        let (decoder, _) = FsstDecoder::import(&header).unwrap();
        for (original, packed) in strings.iter().zip(compressed) {
            assert_eq!(decoder.decompress(&packed), *original);
        }
    }

    #[test]
    fn compresses_repetitive_strings() {
        let owned: Vec<String> = (0..2000)
            .map(|i| format!("http://www.example.com/page/{}", i % 37))
            .collect();
        let strings: Vec<&[u8]> = owned.iter().map(String::as_bytes).collect();
        let compressed = round_trip(&strings);
        let input: usize = strings.iter().map(|s| s.len()).sum();
        let output: usize = compressed.iter().map(Vec::len).sum();
        assert!(output * 3 < input, "{output} vs {input}");
    }

    #[test]
    fn handles_long_and_binary_strings() {
        let long: Vec<u8> = (0..5000u32).map(|i| (i * 31 % 251) as u8).collect();
        let binary: Vec<u8> = (0..=255u8).collect();
        round_trip(&[&long, &binary, b"", b"a", &[0, 0, 0, 255, 255]]);
    }
}
