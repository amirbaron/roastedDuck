//! Plain decoding of the values of a leaf column into DuckDB vectors (the `Plain` /
//! `PlainSkip` methods of DuckDB's `TemplatedColumnReader`, `CallbackColumnReader`,
//! `StringColumnReader`, `BooleanColumnReader`, `DecimalColumnReader`,
//! `IntervalColumnReader`, `UUIDColumnReader` and `NullColumnReader`, with the value
//! conversions of `parquet_timestamp.cpp` and `parquet_float16.cpp`).
//!
//! Fixed-width values go through [`FixedConversion`], monomorphised per conversion so the
//! inner loops have no dispatch; values that are stored as-is take a `memcpy` path.

use std::marker::PhantomData;
use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_types::{LogicalType, LogicalTypeId, NativeType, StringT, Vector};

use crate::decode::{ByteBuffer, PageBlock, out_of_buffer};
use crate::schema::{ColumnSchema, ExtraTypeInfo};
use crate::thrift::Type;

/// Where plain-encoded values come from: a cursor into a page (or a buffer a decoder
/// produced), plus the block that owns the bytes so strings can point into it.
#[derive(Debug)]
pub struct PlainSource<'a> {
    pub buffer: ByteBuffer<'a>,
    /// The owner of the bytes; vectors referencing them keep it alive.
    pub owner: Option<&'a PageBlock>,
}

impl<'a> PlainSource<'a> {
    pub fn new(data: &'a [u8], owner: Option<&'a PageBlock>) -> Self {
        Self {
            buffer: ByteBuffer::new(data),
            owner,
        }
    }
}

/// Decodes plain-encoded values of one column type.
pub trait PlainDecoder: Send {
    /// Decodes `num_values` rows into `result[offset..offset + num_values]`; rows whose
    /// definition level is below `max_define` are NULL and consume no value.
    fn plain(
        &mut self,
        source: &mut PlainSource<'_>,
        defines: Option<&[u8]>,
        max_define: u8,
        num_values: usize,
        offset: usize,
        result: &mut Vector,
    ) -> Result<()>;

    /// Skips the values of `num_values` rows (`PlainSkip`); `defines` starts at row 0.
    fn skip(
        &mut self,
        source: &mut PlainSource<'_>,
        defines: Option<&[u8]>,
        max_define: u8,
        num_values: usize,
    ) -> Result<()>;

    /// Called when a new page starts (`ResetPage`).
    fn reset_page(&mut self) {}

    /// The byte width of one parquet value for fixed-width types (dictionary sizing).
    fn fixed_width(&self) -> Option<usize> {
        None
    }
}

/// Counts the rows of `defines` that hold a value.
#[inline]
pub fn valid_count(defines: Option<&[u8]>, max_define: u8, count: usize) -> usize {
    defines.map_or(count, |defines| {
        defines[..count]
            .iter()
            .map(|&level| usize::from(level == max_define))
            .sum()
    })
}

/// Marks `rows` of `result` NULL where the definition level is below the maximum.
fn set_nulls(result: &mut Vector, defines: &[u8], max_define: u8, offset: usize, num_values: usize) {
    let capacity = result.capacity().max(offset + num_values);
    for (row, &level) in defines.iter().enumerate().skip(offset).take(num_values) {
        if level != max_define {
            result.validity_mut().set_invalid(row, capacity);
        }
    }
}

/// A fixed-width Parquet value converted into a DuckDB value.
pub trait FixedConversion: Send + 'static {
    type Out: NativeType;
    /// The width of one Parquet value.
    const WIDTH: usize;
    /// Whether the Parquet bytes are the DuckDB value (a `memcpy` suffices).
    const COPY: bool;
    fn convert(bytes: &[u8]) -> Result<Self::Out>;
}

macro_rules! le_value {
    ($ty:ty, $bytes:expr) => {{
        let mut array = [0u8; size_of::<$ty>()];
        array.copy_from_slice(&$bytes[..size_of::<$ty>()]);
        <$ty>::from_le_bytes(array)
    }};
}

/// A value stored as-is (`TemplatedParquetValueConversion<T>` into a vector of `T`).
#[derive(Debug)]
pub struct Copy<T>(PhantomData<T>);

macro_rules! copy_conversion {
    ($($ty:ty),*) => {$(
        impl FixedConversion for Copy<$ty> {
            type Out = $ty;
            const WIDTH: usize = size_of::<$ty>();
            const COPY: bool = true;
            #[inline]
            fn convert(bytes: &[u8]) -> Result<$ty> {
                Ok(le_value!($ty, bytes))
            }
        }
    )*};
}
copy_conversion!(i32, i64, u32, u64, f32, f64);

/// A 32/64-bit integer stored into a narrower or wider integer, wrapping like the C++
/// implicit conversion (`TemplatedParquetValueConversion<int32_t>` into an `int8_t`
/// vector, or decimals stored as INT32/INT64).
#[derive(Debug)]
pub struct Convert<From, To>(PhantomData<(From, To)>);

macro_rules! convert_conversion {
    ($($from:ty => $($to:ty),*;)*) => {$($(
        impl FixedConversion for Convert<$from, $to> {
            type Out = $to;
            const WIDTH: usize = size_of::<$from>();
            const COPY: bool = false;
            #[inline]
            fn convert(bytes: &[u8]) -> Result<$to> {
                Ok(le_value!($from, bytes) as $to)
            }
        }
    )*)*};
}
convert_conversion! {
    i32 => i8, i16, i64, i128;
    u32 => u8, u16;
    i64 => i16, i32, i128;
}

macro_rules! callback_conversion {
    ($(#[$meta:meta])* $name:ident, $from:ty => $to:ty, |$value:ident| $body:expr) => {
        $(#[$meta])*
        #[derive(Debug)]
        pub struct $name;

        impl FixedConversion for $name {
            type Out = $to;
            const WIDTH: usize = size_of::<$from>();
            const COPY: bool = false;
            #[inline]
            fn convert(bytes: &[u8]) -> Result<$to> {
                let $value = le_value!($from, bytes);
                $body
            }
        }
    };
}

const JULIAN_TO_UNIX_EPOCH_DAYS: i64 = 2_440_588;
const MICROS_PER_DAY: i64 = 86_400_000_000;
const NANOS_PER_DAY: i64 = MICROS_PER_DAY * 1000;
const MILLIS_PER_DAY: i32 = 86_400_000;

/// `ParquetWrapTime`: a time of day modulo one day (the end of the day stays).
fn wrap_time<T>(raw: T, day: T) -> T
where
    T: std::ops::Rem<Output = T> + std::ops::Add<Output = T> + PartialOrd + Default + std::marker::Copy,
{
    if raw == day {
        return raw;
    }
    let modulus = raw % day;
    if modulus < T::default() {
        modulus + day
    } else {
        modulus
    }
}

fn conversion_error(message: &'static str) -> Error {
    Error::conversion(message)
}

callback_conversion!(
    /// `ParquetTimestampMsToTimestamp`.
    TimestampMsToUs, i64 => i64, |raw| {
        if roastedduck_types::Timestamp::is_finite_raw(raw) {
            raw.checked_mul(1000)
                .ok_or_else(|| conversion_error("Could not convert Timestamp(MS) to Timestamp(US)"))
        } else {
            Ok(raw)
        }
    }
);
callback_conversion!(
    /// `ParquetTimestampNsToTimestamp`.
    TimestampNsToUs, i64 => i64, |raw| {
        Ok(if roastedduck_types::Timestamp::is_finite_raw(raw) { raw / 1000 } else { raw })
    }
);
callback_conversion!(
    /// `ParquetTimestampMsToTimestampNs` (DuckDB scales milliseconds by 1000 here).
    TimestampMsToNs, i64 => i64, |raw| {
        if roastedduck_types::Timestamp::is_finite_raw(raw) {
            raw.checked_mul(1000)
                .ok_or_else(|| conversion_error("Could not convert Timestamp(US) to Timestamp(NS)"))
        } else {
            Ok(raw)
        }
    }
);
callback_conversion!(
    /// `ParquetTimestampUsToTimestampNs` (DuckDB scales microseconds by 10^6 here).
    TimestampUsToNs, i64 => i64, |raw| {
        if roastedduck_types::Timestamp::is_finite_raw(raw) {
            raw.checked_mul(1_000_000)
                .ok_or_else(|| conversion_error("Could not convert Timestamp(MS) to Timestamp(NS)"))
        } else {
            Ok(raw)
        }
    }
);
callback_conversion!(
    /// `ParquetMsIntToTime`.
    TimeMsToUs, i32 => i64, |raw| Ok(i64::from(raw) * 1000)
);
callback_conversion!(
    /// `ParquetNsIntToTime`.
    TimeNsToUs, i64 => i64, |raw| Ok(raw / 1000)
);
callback_conversion!(
    /// `ParquetMsIntToTimeNs`.
    TimeMsToNs, i32 => i64, |raw| Ok(i64::from(raw).wrapping_mul(1_000_000))
);
callback_conversion!(
    /// `ParquetUsIntToTimeNs`.
    TimeUsToNs, i64 => i64, |raw| Ok(raw.wrapping_mul(1000))
);
callback_conversion!(
    /// `ParquetIntToTimeMsTZ`.
    TimeTzFromMs, i32 => i64, |raw| {
        let millis = wrap_time(raw, MILLIS_PER_DAY);
        Ok(roastedduck_types::TimeTz::new(roastedduck_types::Time(i64::from(millis) * 1000), 0).0 as i64)
    }
);
callback_conversion!(
    /// `ParquetIntToTimeTZ`.
    TimeTzFromUs, i64 => i64, |raw| {
        let micros = wrap_time(raw, MICROS_PER_DAY);
        Ok(roastedduck_types::TimeTz::new(roastedduck_types::Time(micros), 0).0 as i64)
    }
);
callback_conversion!(
    /// `ParquetIntToTimeNsTZ`.
    TimeTzFromNs, i64 => i64, |raw| {
        let nanos = wrap_time(raw, NANOS_PER_DAY);
        Ok(roastedduck_types::TimeTz::new(roastedduck_types::Time(nanos / 1000), 0).0 as i64)
    }
);
callback_conversion!(
    /// `Float16ToFloat32`.
    Float16ToFloat, u16 => f32, |raw| Ok(float16_to_float32(raw))
);

/// An Impala INT96 timestamp: nanoseconds of the day, then the Julian day.
fn impala_parts(bytes: &[u8]) -> (i64, i64) {
    let nanos = le_value!(i64, bytes);
    let day = i64::from(le_value!(u32, &bytes[8..]));
    (day - JULIAN_TO_UNIX_EPOCH_DAYS, nanos)
}

/// `ImpalaTimestampToTimestamp`.
#[derive(Debug)]
pub struct ImpalaToUs;

impl FixedConversion for ImpalaToUs {
    type Out = i64;
    const WIDTH: usize = 12;
    const COPY: bool = false;
    #[inline]
    fn convert(bytes: &[u8]) -> Result<i64> {
        let (days, nanos) = impala_parts(bytes);
        Ok(days.wrapping_mul(MICROS_PER_DAY).wrapping_add(nanos / 1000))
    }
}

/// `ImpalaTimestampToTimestampNS`.
#[derive(Debug)]
pub struct ImpalaToNs;

impl FixedConversion for ImpalaToNs {
    type Out = i64;
    const WIDTH: usize = 12;
    const COPY: bool = false;
    #[inline]
    fn convert(bytes: &[u8]) -> Result<i64> {
        let (days, nanos) = impala_parts(bytes);
        Ok(days.wrapping_mul(NANOS_PER_DAY).wrapping_add(nanos))
    }
}

/// `UUIDValueConversion` (`BaseUUID::FromBlob`): big-endian bytes, sign bit flipped.
#[derive(Debug)]
pub struct UuidConversion;

impl FixedConversion for UuidConversion {
    type Out = i128;
    const WIDTH: usize = 16;
    const COPY: bool = false;
    #[inline]
    fn convert(bytes: &[u8]) -> Result<i128> {
        let mut array = [0; 16];
        array.copy_from_slice(&bytes[..16]);
        Ok((u128::from_be_bytes(array) ^ (1u128 << 127)) as i128)
    }
}

/// `Float16ToFloat32`.
pub fn float16_to_float32(value: u16) -> f32 {
    let sign = u32::from(value >> 15);
    let mut exponent = u32::from((value >> 10) & 0x1f);
    let mut fraction = u32::from(value & 0x3ff);
    let bits = if exponent == 0 {
        if fraction == 0 {
            sign << 31
        } else {
            exponent = 127 - 14;
            while fraction & (1 << 10) == 0 {
                exponent -= 1;
                fraction <<= 1;
            }
            fraction &= 0x3ff;
            (sign << 31) | (exponent << 23) | (fraction << 13)
        }
    } else if exponent == 0x1f {
        (sign << 31) | (0xff << 23) | (fraction << 13)
    } else {
        (sign << 31) | ((exponent + (127 - 15)) << 23) | (fraction << 13)
    };
    f32::from_bits(bits)
}

/// Plain decoding of a [`FixedConversion`] (`PlainTemplated`).
#[derive(Debug)]
pub struct FixedDecoder<C>(PhantomData<C>);

impl<C> FixedDecoder<C> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<C> Default for FixedDecoder<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: FixedConversion> PlainDecoder for FixedDecoder<C> {
    fn plain(
        &mut self,
        source: &mut PlainSource<'_>,
        defines: Option<&[u8]>,
        max_define: u8,
        num_values: usize,
        offset: usize,
        result: &mut Vector,
    ) -> Result<()> {
        let buffer = &mut source.buffer;
        match defines {
            None => {
                let bytes = buffer.take(num_values * C::WIDTH)?;
                if C::COPY {
                    // The Parquet bytes are the vector's slots (little-endian hosts).
                    result.data_bytes_mut()[offset * C::WIDTH..(offset + num_values) * C::WIDTH]
                        .copy_from_slice(bytes);
                } else {
                    let values = &mut result.values_mut::<C::Out>()[offset..offset + num_values];
                    for (value, chunk) in values.iter_mut().zip(bytes.chunks_exact(C::WIDTH)) {
                        *value = C::convert(chunk)?;
                    }
                }
            }
            Some(defines) => {
                set_nulls(result, defines, max_define, offset, num_values);
                let values = result.values_mut::<C::Out>();
                for row in offset..offset + num_values {
                    if defines[row] != max_define {
                        continue;
                    }
                    values[row] = C::convert(buffer.take(C::WIDTH)?)?;
                }
            }
        }
        Ok(())
    }

    fn skip(
        &mut self,
        source: &mut PlainSource<'_>,
        defines: Option<&[u8]>,
        max_define: u8,
        num_values: usize,
    ) -> Result<()> {
        let count = valid_count(defines, max_define, num_values);
        source.buffer.inc(count * C::WIDTH)
    }

    fn fixed_width(&self) -> Option<usize> {
        Some(C::WIDTH)
    }
}

/// `BooleanColumnReader`: bit-packed booleans, least significant bit first.
#[derive(Debug, Default)]
pub struct BooleanDecoder {
    byte_position: u8,
}

impl BooleanDecoder {
    #[inline]
    fn read(&mut self, buffer: &mut ByteBuffer<'_>) -> Result<bool> {
        let byte = *buffer.remaining().first().ok_or_else(out_of_buffer)?;
        let value = (byte >> self.byte_position) & 1 == 1;
        self.byte_position += 1;
        if self.byte_position == 8 {
            self.byte_position = 0;
            buffer.inc(1)?;
        }
        Ok(value)
    }
}

impl PlainDecoder for BooleanDecoder {
    fn plain(
        &mut self,
        source: &mut PlainSource<'_>,
        defines: Option<&[u8]>,
        max_define: u8,
        num_values: usize,
        offset: usize,
        result: &mut Vector,
    ) -> Result<()> {
        if let Some(defines) = defines {
            set_nulls(result, defines, max_define, offset, num_values);
        }
        let values = result.values_mut::<bool>();
        for row in offset..offset + num_values {
            if defines.is_some_and(|defines| defines[row] != max_define) {
                continue;
            }
            values[row] = self.read(&mut source.buffer)?;
        }
        Ok(())
    }

    fn skip(
        &mut self,
        source: &mut PlainSource<'_>,
        defines: Option<&[u8]>,
        max_define: u8,
        num_values: usize,
    ) -> Result<()> {
        for _ in 0..valid_count(defines, max_define, num_values) {
            self.read(&mut source.buffer)?;
        }
        Ok(())
    }

    fn reset_page(&mut self) {
        self.byte_position = 0;
    }
}

/// Booleans already unpacked into one byte each (RLE-encoded pages).
#[derive(Debug, Default)]
pub struct ByteBooleanDecoder;

impl PlainDecoder for ByteBooleanDecoder {
    fn plain(
        &mut self,
        source: &mut PlainSource<'_>,
        defines: Option<&[u8]>,
        max_define: u8,
        num_values: usize,
        offset: usize,
        result: &mut Vector,
    ) -> Result<()> {
        if let Some(defines) = defines {
            set_nulls(result, defines, max_define, offset, num_values);
        }
        let values = result.values_mut::<bool>();
        for row in offset..offset + num_values {
            if defines.is_some_and(|defines| defines[row] != max_define) {
                continue;
            }
            values[row] = source.buffer.read_u8()? != 0;
        }
        Ok(())
    }

    fn skip(
        &mut self,
        source: &mut PlainSource<'_>,
        defines: Option<&[u8]>,
        max_define: u8,
        num_values: usize,
    ) -> Result<()> {
        source.buffer.inc(valid_count(defines, max_define, num_values))
    }
}

/// How the strings of a column are validated (`StringColumnType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringKind {
    Varchar,
    Json,
    Other,
}

/// `StringColumnReader`: length-prefixed (or fixed-length) byte arrays, referenced in
/// place in the page.
#[derive(Debug)]
pub struct StringDecoder {
    /// The length of `FIXED_LEN_BYTE_ARRAY` values; 0 for length-prefixed ones.
    pub fixed_length: usize,
    pub kind: StringKind,
    pub file_name: Arc<[u8]>,
}

impl StringDecoder {
    pub fn new(schema: &ColumnSchema, file_name: Arc<[u8]>) -> Self {
        let kind = if schema
            .logical_type
            .alias()
            .is_some_and(|alias| alias.as_bytes() == b"JSON")
        {
            StringKind::Json
        } else if schema.logical_type.id() == LogicalTypeId::Varchar {
            StringKind::Varchar
        } else {
            StringKind::Other
        };
        Self {
            fixed_length: if schema.parquet_type == Type::FIXED_LEN_BYTE_ARRAY {
                schema.type_length as usize
            } else {
                0
            },
            kind,
            file_name,
        }
    }

    /// `StringColumnReader::VerifyString`.
    pub fn verify(&self, bytes: &[u8]) -> Result<()> {
        match self.kind {
            StringKind::Varchar => verify_utf8(bytes, &self.file_name),
            // Gap: JSON values are not validated (DuckDB's `StringUtil::ValidateJSON`).
            StringKind::Json | StringKind::Other => Ok(()),
        }
    }

    #[inline]
    fn next<'a>(&self, buffer: &mut ByteBuffer<'a>) -> Result<&'a [u8]> {
        let len = if self.fixed_length == 0 {
            buffer.read_u32()? as usize
        } else {
            self.fixed_length
        };
        buffer.take(len)
    }
}

/// Fails unless `bytes` is valid UTF-8 (`Utf8Proc::Analyze`).
pub fn verify_utf8(bytes: &[u8], file_name: &[u8]) -> Result<()> {
    if std::str::from_utf8(bytes).is_ok() {
        return Ok(());
    }
    Err(Error::invalid_input(roastedduck_common::byte_format!(
        "Invalid string encoding found in Parquet file \"{file_name}\": value \"{}\" is not valid UTF8!",
        blob_text(bytes)
    )))
}

/// `Blob::ToString`: printable ASCII as-is, other bytes as `\xNN`.
pub fn blob_text(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    roastedduck_types::blob::write_text(bytes, &mut out);
    String::from_utf8_lossy(&out).into_owned()
}

/// Stores `bytes` (which live in the source's block) in `values[row]`.
#[inline]
fn store_string(values: &mut [StringT], row: usize, bytes: &[u8]) {
    // SAFETY: `bytes` lies in the page block the caller registers with the result
    // vector (`add_keep_alive`), so it outlives every read of the string.
    values[row] = unsafe { StringT::from_raw(bytes) };
}

impl PlainDecoder for StringDecoder {
    fn plain(
        &mut self,
        source: &mut PlainSource<'_>,
        defines: Option<&[u8]>,
        max_define: u8,
        num_values: usize,
        offset: usize,
        result: &mut Vector,
    ) -> Result<()> {
        let owner = source.owner.ok_or_else(|| {
            Error::not_implemented("StringColumnReader can only read plain data from a shared buffer")
        })?;
        result.add_keep_alive(Arc::clone(owner) as Arc<dyn std::any::Any + Send + Sync>);
        if let Some(defines) = defines {
            set_nulls(result, defines, max_define, offset, num_values);
        }
        let values = result.values_mut::<StringT>();
        let start = source.buffer.remaining();
        let mut read = Ok(());
        for row in offset..offset + num_values {
            if defines.is_some_and(|defines| defines[row] != max_define) {
                continue;
            }
            match self.next(&mut source.buffer) {
                Ok(bytes) => store_string(values, row, bytes),
                Err(error) => {
                    read = Err(error);
                    break;
                }
            }
        }
        if self.kind == StringKind::Varchar {
            // The bytes just read (values and their lengths) are mostly ASCII, which is
            // valid UTF-8: check them at once, and the strings one by one otherwise.
            let consumed = &start[..start.len() - source.buffer.remaining().len()];
            if !consumed.is_ascii() {
                let mut again = ByteBuffer::new(consumed);
                while !again.is_empty() {
                    let bytes = self.next(&mut again)?;
                    self.verify(bytes)?;
                }
            }
        }
        // A truncated page fails after the strings before the truncation are checked.
        read
    }

    fn skip(
        &mut self,
        source: &mut PlainSource<'_>,
        defines: Option<&[u8]>,
        max_define: u8,
        num_values: usize,
    ) -> Result<()> {
        for _ in 0..valid_count(defines, max_define, num_values) {
            self.next(&mut source.buffer)?;
        }
        Ok(())
    }
}

/// A DuckDB decimal storage type read from big-endian two's complement bytes
/// (`ParquetDecimalUtils::ReadDecimalValue`).
pub trait DecimalTarget: NativeType {
    fn read(bytes: &[u8], scale: u32) -> Result<Self>;
}

macro_rules! decimal_target {
    ($($ty:ty),*) => {$(
        impl DecimalTarget for $ty {
            fn read(bytes: &[u8], _scale: u32) -> Result<Self> {
                const SIZE: usize = size_of::<$ty>();
                if bytes.is_empty() {
                    return Ok(0);
                }
                let positive = bytes[0] & 0x80 == 0;
                let mut le = [0u8; SIZE];
                let size = bytes.len();
                for i in 0..size.min(SIZE) {
                    let byte = bytes[size - i - 1];
                    le[i] = if positive { byte } else { byte ^ 0xff };
                }
                for i in SIZE..size {
                    let mut byte = bytes[size - i - 1];
                    if !positive {
                        byte ^= 0xff;
                    }
                    if byte != 0 {
                        return Err(Error::invalid_input("Invalid decimal encoding in Parquet file"));
                    }
                }
                let value = <$ty>::from_le_bytes(le);
                Ok(if positive { value } else { (value.wrapping_add(1)).wrapping_neg() })
            }
        }
    )*};
}
decimal_target!(i16, i32, i64, i128);

impl DecimalTarget for f64 {
    fn read(bytes: &[u8], scale: u32) -> Result<Self> {
        if bytes.is_empty() {
            return Ok(0.0);
        }
        let positive = bytes[0] & 0x80 == 0;
        let mut result = 0.0f64;
        let mut i = 0;
        while i < bytes.len() {
            let byte_size = 8.min(bytes.len() - i);
            let mut input = [0u8; 8];
            for k in 0..byte_size {
                let byte = bytes[i + k];
                input[8 - k - 1] = if positive { byte } else { byte ^ 0xff };
            }
            result *= u64::MAX as f64 + 1.0;
            result += u64::from_le_bytes(input) as f64;
            i += 8;
        }
        let divisor = 10f64.powi(scale as i32);
        if positive {
            Ok(result / divisor)
        } else {
            Ok(-((result + 1.0) / divisor))
        }
    }
}

/// `DecimalColumnReader`: decimals stored as (fixed-length) byte arrays.
#[derive(Debug)]
pub struct DecimalBytesDecoder<T> {
    /// The `FIXED_LEN_BYTE_ARRAY` length; 0 for length-prefixed values.
    fixed_length: usize,
    scale: u32,
    target: PhantomData<T>,
}

impl<T> DecimalBytesDecoder<T> {
    pub fn new(schema: &ColumnSchema) -> Self {
        Self {
            fixed_length: if schema.parquet_type == Type::FIXED_LEN_BYTE_ARRAY {
                schema.type_length as usize
            } else {
                0
            },
            scale: schema.type_scale,
            target: PhantomData,
        }
    }

    #[inline]
    fn next<'a>(&self, buffer: &mut ByteBuffer<'a>) -> Result<&'a [u8]> {
        let len = if self.fixed_length == 0 {
            buffer.read_u32()? as usize
        } else {
            self.fixed_length
        };
        buffer.take(len)
    }
}

impl<T: DecimalTarget> PlainDecoder for DecimalBytesDecoder<T> {
    fn plain(
        &mut self,
        source: &mut PlainSource<'_>,
        defines: Option<&[u8]>,
        max_define: u8,
        num_values: usize,
        offset: usize,
        result: &mut Vector,
    ) -> Result<()> {
        if let Some(defines) = defines {
            set_nulls(result, defines, max_define, offset, num_values);
        }
        let values = result.values_mut::<T>();
        for row in offset..offset + num_values {
            if defines.is_some_and(|defines| defines[row] != max_define) {
                continue;
            }
            let bytes = self.next(&mut source.buffer)?;
            values[row] = T::read(bytes, self.scale)?;
        }
        Ok(())
    }

    fn skip(
        &mut self,
        source: &mut PlainSource<'_>,
        defines: Option<&[u8]>,
        max_define: u8,
        num_values: usize,
    ) -> Result<()> {
        for _ in 0..valid_count(defines, max_define, num_values) {
            self.next(&mut source.buffer)?;
        }
        Ok(())
    }
}

/// `IntervalColumnReader`: months, days and milliseconds as little-endian `u32`s.
#[derive(Debug, Default)]
pub struct IntervalDecoder;

/// The size of a Parquet INTERVAL (`PARQUET_INTERVAL_SIZE`).
const PARQUET_INTERVAL_SIZE: usize = 12;

impl PlainDecoder for IntervalDecoder {
    fn plain(
        &mut self,
        source: &mut PlainSource<'_>,
        defines: Option<&[u8]>,
        max_define: u8,
        num_values: usize,
        offset: usize,
        result: &mut Vector,
    ) -> Result<()> {
        if let Some(defines) = defines {
            set_nulls(result, defines, max_define, offset, num_values);
        }
        let data = result.data_bytes_mut();
        for row in offset..offset + num_values {
            if defines.is_some_and(|defines| defines[row] != max_define) {
                continue;
            }
            let bytes = source.buffer.take(PARQUET_INTERVAL_SIZE)?;
            let months = le_value!(i32, bytes);
            let days = le_value!(i32, &bytes[4..]);
            let micros = i64::from(le_value!(u32, &bytes[8..])) * 1000;
            let slot = &mut data[row * 16..row * 16 + 16];
            slot[..4].copy_from_slice(&months.to_le_bytes());
            slot[4..8].copy_from_slice(&days.to_le_bytes());
            slot[8..].copy_from_slice(&micros.to_le_bytes());
        }
        Ok(())
    }

    fn skip(
        &mut self,
        source: &mut PlainSource<'_>,
        defines: Option<&[u8]>,
        max_define: u8,
        num_values: usize,
    ) -> Result<()> {
        source
            .buffer
            .inc(valid_count(defines, max_define, num_values) * PARQUET_INTERVAL_SIZE)
    }

    fn fixed_width(&self) -> Option<usize> {
        Some(PARQUET_INTERVAL_SIZE)
    }
}

/// `NullColumnReader`: every row is NULL.
#[derive(Debug, Default)]
pub struct NullDecoder;

impl PlainDecoder for NullDecoder {
    fn plain(
        &mut self,
        _: &mut PlainSource<'_>,
        _: Option<&[u8]>,
        _: u8,
        num_values: usize,
        offset: usize,
        result: &mut Vector,
    ) -> Result<()> {
        let capacity = result.capacity().max(offset + num_values);
        for row in offset..offset + num_values {
            result.validity_mut().set_invalid(row, capacity);
        }
        Ok(())
    }

    fn skip(&mut self, _: &mut PlainSource<'_>, _: Option<&[u8]>, _: u8, _: usize) -> Result<()> {
        Ok(())
    }
}

fn boxed<D: PlainDecoder + 'static>(decoder: D) -> Box<dyn PlainDecoder> {
    Box::new(decoder)
}

fn fixed<C: FixedConversion>() -> Box<dyn PlainDecoder> {
    boxed(FixedDecoder::<C>::new())
}

fn decimal_bytes(schema: &ColumnSchema) -> Result<Box<dyn PlainDecoder>> {
    use roastedduck_types::PhysicalType as P;
    Ok(match schema.logical_type.physical_type() {
        P::Int16 => boxed(DecimalBytesDecoder::<i16>::new(schema)),
        P::Int32 => boxed(DecimalBytesDecoder::<i32>::new(schema)),
        P::Int64 => boxed(DecimalBytesDecoder::<i64>::new(schema)),
        P::Int128 => boxed(DecimalBytesDecoder::<i128>::new(schema)),
        P::Double => boxed(DecimalBytesDecoder::<f64>::new(schema)),
        _ => return Err(Error::internal("Unrecognized type for Decimal")),
    })
}

fn type_info_error(what: &str) -> Error {
    Error::internal(roastedduck_common::byte_format!("{what} requires type info"))
}

/// The plain decoder of a leaf (`ColumnReader::CreateReader`).
pub fn create_decoder(schema: &ColumnSchema, file_name: &Arc<[u8]>) -> Result<Box<dyn PlainDecoder>> {
    use LogicalTypeId as L;
    let info = schema.type_info;
    let ty: &LogicalType = &schema.logical_type;
    Ok(match ty.id() {
        L::Boolean => boxed(BooleanDecoder::default()),
        L::UTinyInt => fixed::<Convert<u32, u8>>(),
        L::USmallInt => fixed::<Convert<u32, u16>>(),
        L::UInteger => fixed::<Copy<u32>>(),
        L::UBigInt => fixed::<Copy<u64>>(),
        L::TinyInt => fixed::<Convert<i32, i8>>(),
        L::SmallInt => fixed::<Convert<i32, i16>>(),
        L::Integer | L::Date => fixed::<Copy<i32>>(),
        L::BigInt => fixed::<Copy<i64>>(),
        L::Float if info == ExtraTypeInfo::Float16 => fixed::<Float16ToFloat>(),
        L::Float => fixed::<Copy<f32>>(),
        L::Double if info == ExtraTypeInfo::DecimalByteArray => decimal_bytes(schema)?,
        L::Double => fixed::<Copy<f64>>(),
        L::Timestamp | L::TimestampTz => match info {
            ExtraTypeInfo::ImpalaTimestamp => fixed::<ImpalaToUs>(),
            ExtraTypeInfo::UnitMs => fixed::<TimestampMsToUs>(),
            ExtraTypeInfo::UnitMicros => fixed::<Copy<i64>>(),
            ExtraTypeInfo::UnitNs => fixed::<TimestampNsToUs>(),
            _ => return Err(type_info_error("TIMESTAMP")),
        },
        L::TimestampNs => match info {
            ExtraTypeInfo::ImpalaTimestamp => fixed::<ImpalaToNs>(),
            ExtraTypeInfo::UnitMs => fixed::<TimestampMsToNs>(),
            ExtraTypeInfo::UnitMicros => fixed::<TimestampUsToNs>(),
            ExtraTypeInfo::UnitNs => fixed::<Copy<i64>>(),
            _ => return Err(type_info_error("TIMESTAMP_NS")),
        },
        L::Time => match info {
            ExtraTypeInfo::UnitMs => fixed::<TimeMsToUs>(),
            ExtraTypeInfo::UnitMicros => fixed::<Copy<i64>>(),
            ExtraTypeInfo::UnitNs => fixed::<TimeNsToUs>(),
            _ => return Err(type_info_error("TIME")),
        },
        L::TimeNs => match info {
            ExtraTypeInfo::UnitMs => fixed::<TimeMsToNs>(),
            ExtraTypeInfo::UnitMicros => fixed::<TimeUsToNs>(),
            ExtraTypeInfo::UnitNs => fixed::<Copy<i64>>(),
            _ => return Err(type_info_error("TIME")),
        },
        L::TimeTz => match info {
            ExtraTypeInfo::UnitMs => fixed::<TimeTzFromMs>(),
            ExtraTypeInfo::UnitMicros => fixed::<TimeTzFromUs>(),
            ExtraTypeInfo::UnitNs => fixed::<TimeTzFromNs>(),
            _ => return Err(type_info_error("TIME_TZ")),
        },
        L::Blob | L::Varchar => boxed(StringDecoder::new(schema, Arc::clone(file_name))),
        L::Decimal => {
            use roastedduck_types::PhysicalType as P;
            let physical = ty.physical_type();
            match info {
                ExtraTypeInfo::DecimalInt32 => match physical {
                    P::Int16 => fixed::<Convert<i32, i16>>(),
                    P::Int32 => fixed::<Copy<i32>>(),
                    P::Int64 => fixed::<Convert<i32, i64>>(),
                    P::Int128 => fixed::<Convert<i32, i128>>(),
                    _ => {
                        return Err(Error::not_implemented(
                            "Unimplemented internal type for CreateDecimalReader",
                        ));
                    }
                },
                ExtraTypeInfo::DecimalInt64 => match physical {
                    P::Int16 => fixed::<Convert<i64, i16>>(),
                    P::Int32 => fixed::<Convert<i64, i32>>(),
                    P::Int64 => fixed::<Copy<i64>>(),
                    P::Int128 => fixed::<Convert<i64, i128>>(),
                    _ => {
                        return Err(Error::not_implemented(
                            "Unimplemented internal type for CreateDecimalReader",
                        ));
                    }
                },
                ExtraTypeInfo::DecimalByteArray => decimal_bytes(schema)?,
                _ => return Err(Error::not_implemented("Unrecognized Parquet type for Decimal")),
            }
        }
        L::Uuid => fixed::<UuidConversion>(),
        L::Interval => boxed(IntervalDecoder),
        L::SqlNull => boxed(NullDecoder),
        _ => return Err(Error::not_implemented(ty.to_string())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_from_big_endian_bytes() {
        assert_eq!(i32::read(&[0x01, 0x00], 0).unwrap(), 256);
        assert_eq!(i32::read(&[0xff, 0xfe], 0).unwrap(), -2);
        assert_eq!(
            i64::read(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05], 0).unwrap(),
            5
        );
        assert!(i16::read(&[0x01, 0x00, 0x00], 0).is_err());
        // Like DuckDB, every 8-byte group is read as a big-endian integer.
        assert!((f64::read(&[0, 0, 0, 0, 0, 0, 0x04, 0xd2], 2).unwrap() - 12.34).abs() < 1e-9);
        assert!(
            (f64::read(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfb, 0x2e], 2).unwrap() + 12.34).abs() < 1e-9
        );
    }

    #[test]
    fn float16() {
        assert_eq!(float16_to_float32(0x3c00), 1.0);
        assert_eq!(float16_to_float32(0xc000), -2.0);
        assert!(float16_to_float32(0x7e00).is_nan());
        assert_eq!(float16_to_float32(0x0001), 5.960_464_5e-8);
    }

    #[test]
    fn uuid_flips_the_sign_bit() {
        let bytes = [0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        assert_eq!(UuidConversion::convert(&bytes).unwrap(), 1);
    }
}
