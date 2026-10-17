//! How DuckDB values become Parquet values (the `SRC`/`TGT` operator pairs of DuckDB's
//! `StandardColumnWriter`: `ParquetCastOperator`, `FloatingPointOperator`,
//! `ParquetStringOperator`, `ParquetUUIDOperator`, `ParquetIntervalOperator`,
//! `ParquetTimeTZOperator`, `ParquetHugeintOperator`, ...).

use std::hash::Hash;
use std::marker::PhantomData;

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_types::{NativeType, Vector};

use super::stats::{
    BytesStatistics, NoStatistics, NumericStatistics, StatValue, StringStatistics, WriterStatistics,
};
use crate::thrift::Type;

/// The encodings a column type can use when it is not dictionary encoded (V2 files).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackEncoding {
    Plain,
    DeltaBinaryPacked,
    DeltaLengthByteArray,
    ByteStreamSplit,
}

/// A DuckDB column type as written to Parquet.
pub trait WriteType: Send + std::fmt::Debug + 'static {
    /// The value in the DuckDB vector (`SRC`).
    type Src<'a>: Copy;
    /// The dictionary identity of a source value (`PrimitiveDictionary` compares `SRC`s).
    type Key: Eq + Hash + Send + std::fmt::Debug;
    /// The statistics collected over the Parquet values.
    type Stats: WriterStatistics + std::fmt::Debug + 'static;

    const PARQUET_TYPE: Type;
    /// The encoding of V2 files when the dictionary is not used.
    const FALLBACK: FallbackEncoding;

    fn new_stats() -> Self::Stats;
    /// What `get` reads the values of a vector from (its typed slice, or the vector).
    type Batch<'a>: Copy;
    fn batch(vector: &Vector) -> Self::Batch<'_>;
    fn get<'a>(batch: Self::Batch<'a>, row: usize) -> Self::Src<'a>;
    fn src(vector: &Vector, row: usize) -> Self::Src<'_> {
        Self::get(Self::batch(vector), row)
    }
    fn key(src: Self::Src<'_>) -> Self::Key;
    /// The dictionary index of a value (without building its key when it can be
    /// looked up by reference).
    fn lookup(indexes: &FastMap<Self::Key, u32>, src: Self::Src<'_>) -> Option<u32> {
        indexes.get(&Self::key(src)).copied()
    }
    /// Appends the plain encoding of the Parquet value (`WriteToStream`).
    fn write_plain(src: Self::Src<'_>, out: &mut Vec<u8>) -> Result<()>;
    /// Updates the statistics with the value (`HandleStats`).
    fn update_stats(stats: &mut Self::Stats, src: Self::Src<'_>) -> Result<()>;
    /// The xxHash64 of the Parquet value for bloom filters (`XXHash64`).
    fn hash(src: Self::Src<'_>) -> Result<u64>;
    /// The estimated plain size of a value (`GetRowSize`).
    fn row_size(src: Self::Src<'_>) -> usize;
    /// The bytes of a string value (`DlbaEncoder::GetStringSize` uses their length).
    fn string_bytes(src: Self::Src<'_>) -> Option<&[u8]> {
        let _ = src;
        None
    }
    /// The Parquet value as an integer for `DELTA_BINARY_PACKED`.
    fn delta_value(src: Self::Src<'_>) -> Result<i64> {
        let _ = src;
        Err(Error::internal("DbpEncoder should only be used with integers"))
    }
    /// Whether the plain encoding has a constant size (`TypeIsConstantSize`).
    fn is_constant_size() -> bool {
        true
    }
    /// Whether the column chunk gets statistics at all (`InitializeStats` is not null).
    const HAS_STATISTICS: bool = true;
}

/// A fast hasher for the dictionaries (multiply-rotate with a final fold).
#[derive(Default, Clone, Copy, Debug)]
pub struct FastHasher(u64);

const FAST_HASH_K: u64 = 0xf135_7aea_2e62_a9c5;

impl FastHasher {
    #[inline]
    fn add(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(26) ^ value).wrapping_mul(FAST_HASH_K);
    }
}

impl std::hash::Hasher for FastHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for chunk in chunks {
            self.add(u64::from_le_bytes(*chunk));
        }
        if !rest.is_empty() {
            let mut last = [0u8; 8];
            last[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(last));
        }
    }
    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.add(u64::from(value));
    }
    #[inline]
    fn write_u16(&mut self, value: u16) {
        self.add(u64::from(value));
    }
    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.add(u64::from(value));
    }
    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }
    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        let folded = u128::from(self.0) * u128::from(FAST_HASH_K);
        (folded as u64) ^ ((folded >> 64) as u64)
    }
}

/// The hash map of the dictionaries.
pub type FastMap<K, V> = std::collections::HashMap<K, V, std::hash::BuildHasherDefault<FastHasher>>;

/// `XXH64(data, len, 0)`.
pub fn xxhash64(bytes: &[u8]) -> u64 {
    twox_hash::XxHash64::oneshot(0, bytes)
}

/// An integer or floating-point Parquet value.
pub trait PlainNative: StatValue + Copy + Send + std::fmt::Debug + 'static {
    fn to_le(self, out: &mut Vec<u8>);
    fn as_delta(self) -> i64;
}

macro_rules! plain_native {
    ($($ty:ty),*) => {$(
        impl PlainNative for $ty {
            #[inline]
            fn to_le(self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }
            #[inline]
            fn as_delta(self) -> i64 {
                self as i64
            }
        }
    )*};
}
plain_native!(i32, i64, u32, u64, f32, f64);

/// How a fixed-width DuckDB value converts to a Parquet value.
pub trait NumericCast: Send + std::fmt::Debug + 'static {
    type Src: NativeType + Copy + Eq + Hash + std::fmt::Debug;
    type Tgt: PlainNative;
    const PARQUET_TYPE: Type;
    const FALLBACK: FallbackEncoding;
    /// Whether the source type is signed (only then the deprecated min/max are written).
    const SIGNED: bool;
    /// Whether the column gets statistics (`InitializeStats` of the operator).
    const HAS_STATISTICS: bool = true;
    fn cast(src: Self::Src) -> Result<Self::Tgt>;
}

/// A `StandardColumnWriter<SRC, TGT, ParquetCastOperator>`.
#[derive(Debug)]
pub struct Numeric<C>(PhantomData<C>);

impl<C: NumericCast> WriteType for Numeric<C> {
    type Src<'a> = C::Src;
    type Key = C::Src;
    type Stats = NumericStatistics<C::Tgt>;
    const PARQUET_TYPE: Type = C::PARQUET_TYPE;
    const FALLBACK: FallbackEncoding = C::FALLBACK;
    const HAS_STATISTICS: bool = C::HAS_STATISTICS;

    fn new_stats() -> Self::Stats {
        NumericStatistics::new(C::SIGNED, false)
    }
    type Batch<'a> = &'a [C::Src];
    #[inline]
    fn batch(vector: &Vector) -> Self::Batch<'_> {
        vector.values::<C::Src>()
    }
    #[inline]
    fn get<'a>(batch: Self::Batch<'a>, row: usize) -> Self::Src<'a> {
        batch[row]
    }
    #[inline]
    fn key(src: C::Src) -> C::Src {
        src
    }
    #[inline]
    fn write_plain(src: C::Src, out: &mut Vec<u8>) -> Result<()> {
        C::cast(src)?.to_le(out);
        Ok(())
    }
    #[inline]
    fn update_stats(stats: &mut Self::Stats, src: C::Src) -> Result<()> {
        stats.update(C::cast(src)?);
        Ok(())
    }
    fn hash(src: C::Src) -> Result<u64> {
        let mut bytes = Vec::with_capacity(8);
        C::cast(src)?.to_le(&mut bytes);
        Ok(xxhash64(&bytes))
    }
    fn row_size(_: C::Src) -> usize {
        size_of::<C::Tgt>()
    }
    fn delta_value(src: C::Src) -> Result<i64> {
        Ok(C::cast(src)?.as_delta())
    }
}

macro_rules! numeric_cast {
    ($(#[$meta:meta])* $name:ident, $src:ty => $tgt:ty, $parquet:ident, $fallback:ident, $signed:expr, |$value:ident| $body:expr) => {
        numeric_cast!($(#[$meta])* $name, $src => $tgt, $parquet, $fallback, $signed, true, |$value| $body);
    };
    ($(#[$meta:meta])* $name:ident, $src:ty => $tgt:ty, $parquet:ident, $fallback:ident, $signed:expr, $stats:expr, |$value:ident| $body:expr) => {
        $(#[$meta])*
        #[derive(Debug)]
        pub struct $name;

        impl NumericCast for $name {
            type Src = $src;
            type Tgt = $tgt;
            const PARQUET_TYPE: Type = Type::$parquet;
            const FALLBACK: FallbackEncoding = FallbackEncoding::$fallback;
            const SIGNED: bool = $signed;
            const HAS_STATISTICS: bool = $stats;
            #[inline]
            fn cast($value: $src) -> Result<$tgt> {
                $body
            }
        }
    };
}

numeric_cast!(
    /// TINYINT as INT32.
    TinyInt, i8 => i32, INT32, DeltaBinaryPacked, true, |value| Ok(i32::from(value))
);
numeric_cast!(
    /// SMALLINT (and DECIMAL(4)) as INT32.
    SmallInt, i16 => i32, INT32, DeltaBinaryPacked, true, |value| Ok(i32::from(value))
);
numeric_cast!(
    /// INTEGER, DATE and DECIMAL(9) as INT32.
    Integer, i32 => i32, INT32, DeltaBinaryPacked, true, |value| Ok(value)
);
numeric_cast!(
    /// BIGINT, TIME, TIMESTAMP(_TZ/_MS/_NS) and DECIMAL(18) as INT64.
    BigInt, i64 => i64, INT64, DeltaBinaryPacked, true, |value| Ok(value)
);
numeric_cast!(
    /// UTINYINT as INT32.
    UTinyInt, u8 => i32, INT32, DeltaBinaryPacked, false, |value| Ok(i32::from(value))
);
numeric_cast!(
    /// USMALLINT as INT32.
    USmallInt, u16 => i32, INT32, DeltaBinaryPacked, false, |value| Ok(i32::from(value))
);
numeric_cast!(
    /// UINTEGER as INT32 (bit pattern).
    UInteger, u32 => u32, INT32, DeltaBinaryPacked, false, |value| Ok(value)
);
numeric_cast!(
    /// UBIGINT as INT64 (bit pattern).
    UBigInt, u64 => u64, INT64, DeltaBinaryPacked, false, |value| Ok(value)
);
numeric_cast!(
    /// `TIMESTAMP_S` as INT64 microseconds (`ParquetTimestampSOperator`).
    TimestampSec, i64 => i64, INT64, DeltaBinaryPacked, true, |value| {
        // `FromEpochSecondsPossiblyInfinite` scales infinities too (and so rejects them).
        value
            .checked_mul(1_000_000)
            .ok_or_else(|| Error::conversion("Could not convert Timestamp(S) to Timestamp(US)"))
    }
);
numeric_cast!(
    /// TIME WITH TIME ZONE as INT64 UTC microseconds of the day (`ParquetTimeTZOperator`,
    /// `Time::NormalizeTimeTZ`), without statistics (its operator has none).
    TimeTz, i64 => i64, INT64, DeltaBinaryPacked, true, false, |value| {
        const MICROS_PER_DAY: i64 = 86_400_000_000;
        let time = rustdb_types::TimeTz(value as u64);
        let shift = -i64::from(time.offset()) * 1_000_000;
        let mut micros = time.time().0 + (shift - (shift / MICROS_PER_DAY) * MICROS_PER_DAY);
        if micros >= MICROS_PER_DAY {
            micros -= MICROS_PER_DAY;
        } else if micros < 0 {
            micros += MICROS_PER_DAY;
        }
        Ok(micros)
    }
);

/// A floating-point value (`FloatingPointOperator` over `float_na_equal`): NaNs compare
/// equal in the dictionary and do not enter min/max.
#[derive(Debug)]
pub struct Float<T>(PhantomData<T>);

/// The dictionary key of a float: NaNs are one key, and `0.0 == -0.0`.
#[derive(Clone, Copy, Debug)]
pub struct FloatKey(u64, bool);

impl PartialEq for FloatKey {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0 && self.1 == other.1
    }
}
impl Eq for FloatKey {}
impl Hash for FloatKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
        self.1.hash(state);
    }
}

macro_rules! float_type {
    ($ty:ty, $parquet:ident) => {
        impl WriteType for Float<$ty> {
            type Src<'a> = $ty;
            type Key = FloatKey;
            type Stats = NumericStatistics<$ty>;
            const PARQUET_TYPE: Type = Type::$parquet;
            const FALLBACK: FallbackEncoding = FallbackEncoding::ByteStreamSplit;

            fn new_stats() -> Self::Stats {
                NumericStatistics::new(true, true)
            }
            type Batch<'a> = &'a [$ty];
            #[inline]
            fn batch(vector: &Vector) -> Self::Batch<'_> {
                vector.values::<$ty>()
            }
            #[inline]
            fn get<'a>(batch: Self::Batch<'a>, row: usize) -> Self::Src<'a> {
                batch[row]
            }
            #[inline]
            fn key(src: $ty) -> FloatKey {
                if src.is_nan() {
                    FloatKey(0, true)
                } else if src == 0.0 {
                    FloatKey(0, false)
                } else {
                    FloatKey(src.to_bits() as u64, false)
                }
            }
            #[inline]
            fn write_plain(src: $ty, out: &mut Vec<u8>) -> Result<()> {
                out.extend_from_slice(&src.to_le_bytes());
                Ok(())
            }
            #[inline]
            fn update_stats(stats: &mut Self::Stats, src: $ty) -> Result<()> {
                stats.update(src);
                Ok(())
            }
            fn hash(src: $ty) -> Result<u64> {
                Ok(xxhash64(&src.to_le_bytes()))
            }
            fn row_size(_: $ty) -> usize {
                size_of::<$ty>()
            }
        }
    };
}
float_type!(f32, FLOAT);
float_type!(f64, DOUBLE);

/// A 128-bit integer written as DOUBLE without statistics (`ParquetHugeintOperator`).
#[derive(Debug)]
pub struct HugeIntAsDouble<T>(PhantomData<T>);

macro_rules! huge_type {
    ($ty:ty) => {
        impl WriteType for HugeIntAsDouble<$ty> {
            type Src<'a> = $ty;
            type Key = $ty;
            type Stats = NoStatistics;
            const PARQUET_TYPE: Type = Type::DOUBLE;
            const FALLBACK: FallbackEncoding = FallbackEncoding::ByteStreamSplit;

            fn new_stats() -> NoStatistics {
                NoStatistics
            }
            type Batch<'a> = &'a [$ty];
            fn batch(vector: &Vector) -> Self::Batch<'_> {
                vector.values::<$ty>()
            }
            fn get<'a>(batch: Self::Batch<'a>, row: usize) -> Self::Src<'a> {
                batch[row]
            }
            fn key(src: $ty) -> $ty {
                src
            }
            fn write_plain(src: $ty, out: &mut Vec<u8>) -> Result<()> {
                out.extend_from_slice(&(src as f64).to_le_bytes());
                Ok(())
            }
            fn update_stats(_: &mut NoStatistics, _: $ty) -> Result<()> {
                Ok(())
            }
            fn hash(src: $ty) -> Result<u64> {
                Ok(xxhash64(&(src as f64).to_le_bytes()))
            }
            fn row_size(_: $ty) -> usize {
                8
            }
        }
    };
}
huge_type!(i128);
huge_type!(u128);

/// VARCHAR / BLOB values (`ParquetStringOperator` / `ParquetBlobOperator`).
#[derive(Debug)]
pub struct Bytes<const BLOB: bool>;

impl<const BLOB: bool> WriteType for Bytes<BLOB> {
    type Src<'a> = &'a [u8];
    type Key = Box<[u8]>;
    type Stats = StringStatistics;
    const PARQUET_TYPE: Type = Type::BYTE_ARRAY;
    const FALLBACK: FallbackEncoding = FallbackEncoding::DeltaLengthByteArray;

    fn new_stats() -> StringStatistics {
        StringStatistics::new(BLOB)
    }
    type Batch<'a> = &'a Vector;
    #[inline]
    fn batch(vector: &Vector) -> Self::Batch<'_> {
        vector
    }
    #[inline]
    fn get<'a>(batch: Self::Batch<'a>, row: usize) -> Self::Src<'a> {
        batch.string_bytes(row)
    }
    fn key(src: &[u8]) -> Box<[u8]> {
        src.into()
    }
    #[inline]
    fn lookup(indexes: &FastMap<Box<[u8]>, u32>, src: &[u8]) -> Option<u32> {
        indexes.get(src).copied()
    }
    #[inline]
    fn write_plain(src: &[u8], out: &mut Vec<u8>) -> Result<()> {
        out.extend_from_slice(&(src.len() as u32).to_le_bytes());
        out.extend_from_slice(src);
        Ok(())
    }
    #[inline]
    fn update_stats(stats: &mut StringStatistics, src: &[u8]) -> Result<()> {
        stats.update(src);
        Ok(())
    }
    fn hash(src: &[u8]) -> Result<u64> {
        Ok(xxhash64(src))
    }
    fn row_size(src: &[u8]) -> usize {
        4 + src.len()
    }
    fn string_bytes(src: Self::Src<'_>) -> Option<&[u8]> {
        Some(src)
    }
    fn is_constant_size() -> bool {
        false
    }
}

/// GEOMETRY values: their WKB bytes, with geometry statistics (`ParquetGeometryOperator`).
#[derive(Debug)]
pub struct Geometry;

impl WriteType for Geometry {
    type Src<'a> = &'a [u8];
    type Key = Box<[u8]>;
    type Stats = super::stats::GeoStatistics;
    const PARQUET_TYPE: Type = Type::BYTE_ARRAY;
    const FALLBACK: FallbackEncoding = FallbackEncoding::DeltaLengthByteArray;

    fn new_stats() -> super::stats::GeoStatistics {
        super::stats::GeoStatistics::default()
    }
    type Batch<'a> = &'a Vector;
    #[inline]
    fn batch(vector: &Vector) -> Self::Batch<'_> {
        vector
    }
    #[inline]
    fn get<'a>(batch: Self::Batch<'a>, row: usize) -> Self::Src<'a> {
        batch.string_bytes(row)
    }
    fn key(src: &[u8]) -> Box<[u8]> {
        src.into()
    }
    #[inline]
    fn lookup(indexes: &FastMap<Box<[u8]>, u32>, src: &[u8]) -> Option<u32> {
        indexes.get(src).copied()
    }
    #[inline]
    fn write_plain(src: &[u8], out: &mut Vec<u8>) -> Result<()> {
        out.extend_from_slice(&(src.len() as u32).to_le_bytes());
        out.extend_from_slice(src);
        Ok(())
    }
    fn update_stats(stats: &mut super::stats::GeoStatistics, src: &[u8]) -> Result<()> {
        stats.stats.update(src);
        stats.has_stats = true;
        Ok(())
    }
    fn hash(src: &[u8]) -> Result<u64> {
        Ok(xxhash64(src))
    }
    fn row_size(src: &[u8]) -> usize {
        4 + src.len()
    }
    fn string_bytes(src: Self::Src<'_>) -> Option<&[u8]> {
        Some(src)
    }
    fn is_constant_size() -> bool {
        false
    }
}

/// UUIDs as 16 big-endian bytes (`ParquetUUIDOperator`, `BaseUUID::ToBlob`).
#[derive(Debug)]
pub struct Uuid;

fn uuid_bytes(value: i128) -> [u8; 16] {
    ((value as u128) ^ (1u128 << 127)).to_be_bytes()
}

impl WriteType for Uuid {
    type Src<'a> = i128;
    type Key = i128;
    type Stats = BytesStatistics;
    const PARQUET_TYPE: Type = Type::FIXED_LEN_BYTE_ARRAY;
    const FALLBACK: FallbackEncoding = FallbackEncoding::Plain;

    fn new_stats() -> BytesStatistics {
        BytesStatistics::default()
    }
    type Batch<'a> = &'a [i128];
    fn batch(vector: &Vector) -> Self::Batch<'_> {
        vector.values::<i128>()
    }
    fn get<'a>(batch: Self::Batch<'a>, row: usize) -> Self::Src<'a> {
        batch[row]
    }
    fn key(src: i128) -> i128 {
        src
    }
    fn write_plain(src: i128, out: &mut Vec<u8>) -> Result<()> {
        out.extend_from_slice(&uuid_bytes(src));
        Ok(())
    }
    fn update_stats(stats: &mut BytesStatistics, src: i128) -> Result<()> {
        stats.update(&uuid_bytes(src));
        Ok(())
    }
    fn hash(src: i128) -> Result<u64> {
        Ok(xxhash64(&uuid_bytes(src)))
    }
    fn row_size(_: i128) -> usize {
        16
    }
}

/// INTERVALs as 12 bytes of months, days and milliseconds (`ParquetIntervalOperator`).
#[derive(Debug)]
pub struct IntervalType;

/// An interval read from a vector slot: months, days, micros.
pub type IntervalParts = (i32, i32, i64);

fn interval_bytes((months, days, micros): IntervalParts) -> Result<[u8; 12]> {
    if days < 0 || months < 0 || micros < 0 {
        return Err(Error::new(
            ErrorKind::Io,
            "Parquet files do not support negative intervals",
        ));
    }
    let mut bytes = [0; 12];
    bytes[..4].copy_from_slice(&(months as u32).to_le_bytes());
    bytes[4..8].copy_from_slice(&(days as u32).to_le_bytes());
    bytes[8..].copy_from_slice(&((micros / 1000) as u32).to_le_bytes());
    Ok(bytes)
}

impl WriteType for IntervalType {
    type Src<'a> = IntervalParts;
    type Key = IntervalParts;
    type Stats = NoStatistics;
    const PARQUET_TYPE: Type = Type::FIXED_LEN_BYTE_ARRAY;
    const FALLBACK: FallbackEncoding = FallbackEncoding::Plain;
    const HAS_STATISTICS: bool = false;

    fn new_stats() -> NoStatistics {
        NoStatistics
    }
    type Batch<'a> = &'a [u8];
    fn batch(vector: &Vector) -> Self::Batch<'_> {
        vector.data_bytes()
    }
    fn get<'a>(batch: Self::Batch<'a>, row: usize) -> Self::Src<'a> {
        let bytes = &batch[row * 16..row * 16 + 16];
        let months = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let days = i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let mut micros = [0u8; 8];
        micros.copy_from_slice(&bytes[8..16]);
        (months, days, i64::from_le_bytes(micros))
    }
    fn key(src: IntervalParts) -> IntervalParts {
        src
    }
    fn write_plain(src: IntervalParts, out: &mut Vec<u8>) -> Result<()> {
        out.extend_from_slice(&interval_bytes(src)?);
        Ok(())
    }
    fn update_stats(_: &mut NoStatistics, src: IntervalParts) -> Result<()> {
        // The conversion still rejects negative intervals.
        interval_bytes(src).map(|_| ())
    }
    fn hash(src: IntervalParts) -> Result<u64> {
        Ok(xxhash64(&interval_bytes(src)?))
    }
    fn row_size(_: IntervalParts) -> usize {
        12
    }
}
