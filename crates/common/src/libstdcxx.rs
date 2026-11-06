//! Iteration/string hashing of the pinned Linux libstdc++ comparator.
//! Offset-only implementation; entries are never exposed in Rust hash-table order.
#[derive(Debug)]
pub struct UnorderedOrder<K> {
    keys: Vec<K>,
    hashes: Vec<u64>,
    order: Vec<usize>,
    buckets: usize,
}
impl<K> Default for UnorderedOrder<K> {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            hashes: Vec::new(),
            order: Vec::new(),
            buckets: 0,
        }
    }
}
const PRIMES: &[u64] = &[
    3,
    5,
    7,
    11,
    13,
    17,
    19,
    23,
    29,
    31,
    37,
    41,
    43,
    47,
    53,
    59,
    61,
    67,
    71,
    73,
    79,
    83,
    89,
    97,
    103,
    109,
    113,
    127,
    137,
    139,
    149,
    157,
    167,
    179,
    193,
    199,
    211,
    227,
    241,
    257,
    277,
    293,
    313,
    337,
    359,
    383,
    409,
    439,
    467,
    503,
    541,
    577,
    619,
    661,
    709,
    761,
    823,
    887,
    953,
    1031,
    1109,
    1193,
    1289,
    1381,
    1493,
    1613,
    1741,
    1879,
    2029,
    2179,
    2357,
    2549,
    2753,
    2971,
    3209,
    3469,
    3739,
    4027,
    4349,
    4703,
    5087,
    5503,
    5953,
    6427,
    6949,
    7517,
    8123,
    8783,
    9497,
    10273,
    11113,
    12011,
    12983,
    14033,
    15173,
    16411,
    17749,
    19183,
    20753,
    22447,
    24281,
    26267,
    28411,
    30727,
    33223,
    35933,
    38873,
    42043,
    45481,
    49201,
    53201,
    57557,
    62233,
    67307,
    72817,
    78779,
    85229,
    92203,
    99733,
    107_897,
    116_731,
    126_271,
    136_607,
    147_793,
    159_871,
    172_933,
    187_091,
    202_409,
    218_971,
    236_897,
    256_279,
    277_261,
    299_951,
    324_503,
    351_061,
    379_787,
    410_857,
    444_487,
    480_881,
    520_241,
    562_841,
    608_903,
    658_753,
    712_697,
    771_049,
    834_181,
    902_483,
    976_369,
    1_056_323,
    1_142_821,
    1_236_397,
    1_337_629,
    1_447_153,
    1_565_659,
    1_693_859,
    1_832_561,
    1_982_627,
    2_144_977,
    2_320_627,
    2_510_653,
    2_716_249,
    2_938_679,
    3_179_303,
    3_439_651,
    3_721_303,
    4_026_031,
    4_355_707,
    4_712_381,
    5_098_259,
    5_515_729,
    5_967_347,
    6_456_007,
    6_984_629,
    7_556_579,
    8_175_383,
    8_844_859,
    9_569_143,
    10_352_717,
    11_200_489,
    12_117_689,
    13_109_983,
    14_183_539,
    15_345_007,
    16_601_593,
    17_961_079,
    19_431_899,
    21_023_161,
    22_744_717,
    24_607_243,
    26_622_317,
    28_802_401,
    31_160_981,
    33_712_729,
    36_473_443,
    39_460_231,
    42_691_603,
    46_187_573,
    49_969_847,
    54_061_849,
    58_488_943,
    63_278_561,
    68_460_391,
    74_066_549,
    80_131_819,
    86_693_767,
    93_793_069,
    101_473_717,
    109_783_337,
    118_773_397,
    128_499_677,
    139_022_417,
    150_406_843,
    162_723_577,
    176_048_909,
    190_465_427,
    206_062_531,
    222_936_881,
    241_193_053,
    260_944_219,
    282_312_799,
    305_431_229,
    330_442_829,
    357_502_601,
    386_778_277,
    418_451_333,
    452_718_089,
    489_790_921,
    529_899_637,
    573_292_817,
    620_239_453,
    671_030_513,
    725_980_837,
    785_430_967,
    849_749_479,
    919_334_987,
    994_618_837,
    1_076_067_617,
    1_164_186_217,
    1_259_520_799,
    1_362_662_261,
    1_474_249_943,
    1_594_975_441,
    1_725_587_117,
    1_866_894_511,
    2_019_773_507,
    2_185_171_673,
    2_364_114_217,
    2_557_710_269,
    2_767_159_799,
    2_993_761_039,
    3_238_918_481,
    3_504_151_727,
    3_791_104_843,
    4_101_556_399,
    4_294_967_291,
    6_442_450_933,
    8_589_934_583,
    12_884_901_857,
    17_179_869_143,
    25_769_803_693,
    34_359_738_337,
    51_539_607_367,
    68_719_476_731,
    103_079_215_087,
    137_438_953_447,
    206_158_430_123,
    274_877_906_899,
    412_316_860_387,
    549_755_813_881,
    824_633_720_731,
    1_099_511_627_689,
    1_649_267_441_579,
    2_199_023_255_531,
    3_298_534_883_309,
    4_398_046_511_093,
    6_597_069_766_607,
    8_796_093_022_151,
    13_194_139_533_241,
    17_592_186_044_399,
    26_388_279_066_581,
    35_184_372_088_777,
    52_776_558_133_177,
    70_368_744_177_643,
    105_553_116_266_399,
    140_737_488_355_213,
    211_106_232_532_861,
    281_474_976_710_597,
    562_949_953_421_231,
    1_125_899_906_842_597,
    2_251_799_813_685_119,
    4_503_599_627_370_449,
    9_007_199_254_740_881,
    18_014_398_509_481_951,
    36_028_797_018_963_913,
    72_057_594_037_927_931,
    144_115_188_075_855_859,
    288_230_376_151_711_717,
    576_460_752_303_423_433,
    1_152_921_504_606_846_883,
    2_305_843_009_213_693_951,
    4_611_686_018_427_387_847,
    9_223_372_036_854_775_783,
    18_446_744_073_709_551_557,
];
impl<K> UnorderedOrder<K> {
    fn prepend_bucket(&mut self, index: usize) {
        let bucket = self.hashes[index] % self.buckets as u64;
        let at = self
            .order
            .iter()
            .position(|i| self.hashes[*i] % self.buckets as u64 == bucket)
            .unwrap_or(0);
        self.order.insert(at, index);
    }
    pub fn insert(&mut self, hash: u64, key: K, equal: impl Fn(&K, &K) -> bool) -> (bool, usize) {
        if let Some(index) = self
            .keys
            .iter()
            .enumerate()
            .position(|(i, old)| self.hashes[i] == hash && equal(old, &key))
        {
            return (false, index);
        }
        let len = self.keys.len();
        if len + 1 >= self.buckets {
            let requested = if self.buckets == 0 {
                2
            } else {
                (self.buckets * 2).max(len + 2)
            };
            self.buckets = PRIMES
                .iter()
                .copied()
                .find(|n| *n >= requested as u64)
                .unwrap_or(u64::MAX) as usize;
            let previous = std::mem::take(&mut self.order);
            for index in previous {
                self.prepend_bucket(index);
            }
        }
        self.keys.push(key);
        self.hashes.push(hash);
        self.prepend_bucket(len);
        (true, len)
    }
    pub fn order(&self) -> Vec<usize> {
        self.order.clone()
    }
    pub fn keys(&self) -> &[K] {
        &self.keys
    }
}
pub fn string_hash(bytes: &[u8]) -> u64 {
    const M: u64 = 0xc6a4_a793_5bd1_e995;
    let mut hash = 0xc70f_6907_u64 ^ (bytes.len() as u64).wrapping_mul(M);
    let (words, tail) = bytes.as_chunks::<8>();
    for word in words {
        let mut key = u64::from_le_bytes(*word);
        key = key.wrapping_mul(M);
        key ^= key >> 47;
        key = key.wrapping_mul(M);
        hash ^= key;
        hash = hash.wrapping_mul(M);
    }
    if !tail.is_empty() {
        for (i, byte) in tail.iter().enumerate() {
            hash ^= u64::from(*byte) << (i * 8);
        }
        hash = hash.wrapping_mul(M);
    }
    hash ^= hash >> 47;
    hash = hash.wrapping_mul(M);
    hash ^ (hash >> 47)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_hash_and_iteration_match_signed_gnu_reference() {
        let mut map = UnorderedOrder::default();
        let fixture = include_str!("../tests/data/libstdcxx_string_order.tsv");
        for line in fixture.lines().filter(|line| line.starts_with("H ")) {
            let fields: Vec<_> = line.split_whitespace().collect();
            let expected: u64 = fields[2].parse().expect("frozen numeric hash");
            assert_eq!(string_hash(fields[1].as_bytes()), expected);
            map.insert(expected, fields[1], |a, b| a == b);
        }
        let expected: Vec<usize> = fixture
            .lines()
            .find(|line| line.starts_with("M "))
            .expect("frozen order")
            .split_whitespace()
            .skip(1)
            .map(|value| value.parse().expect("frozen index"))
            .collect();
        assert_eq!(map.order(), expected);
        let before = map.order();
        let key = map.keys()[0];
        assert_eq!(
            map.insert(string_hash(key.as_bytes()), key, |a, b| a == b),
            (false, 0)
        );
        assert_eq!(map.order(), before);
    }
}
