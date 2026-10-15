//! Normalized sort keys (DuckDB's `create_sort_key` / `decode_sort_key`).
//!
//! Every row's ORDER BY columns are encoded into one byte string whose byte-wise order is
//! the requested order:
//!
//! - each value starts with a validity byte (`1` or `2`, chosen by NULLS FIRST/LAST);
//! - fixed-width values are big-endian with the sign bit flipped (floats transformed so
//!   NaN sorts last), intervals as raw `(months, days, micros)`;
//! - VARCHAR bytes are shifted by one and terminated by `0`; other string types (BLOB, ...)
//!   escape `0`/`1` bytes with `1` and are terminated by `0`;
//! - structs write their fields in order, lists their elements followed by `0`, arrays
//!   their elements even when NULL;
//! - DESC inverts the value bytes (not the validity bytes). Inside nested values NULLs come
//!   last for ASC and first for DESC, like PostgreSQL.
//!
//! The format is byte-identical to DuckDB's, so keys can be checked against the reference
//! `create_sort_key()` function.

use rustdb_common::{Error, Result};
use rustdb_types::{ListEntry, LogicalType, LogicalTypeId, PhysicalType, StringT, TypeInfo, Vector};

use crate::format::{FormatKind, VectorFormat};
use crate::heap::ensure_capacity;

/// ASC or DESC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OrderType {
    Ascending,
    Descending,
}

/// NULLS FIRST or NULLS LAST.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NullOrder {
    NullsFirst,
    NullsLast,
}

/// The modifiers of one ORDER BY column (DuckDB's `OrderModifiers`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OrderModifiers {
    pub order: OrderType,
    pub nulls: NullOrder,
}

impl OrderModifiers {
    pub const fn new(order: OrderType, nulls: NullOrder) -> Self {
        Self { order, nulls }
    }

    /// Parses `ASC NULLS LAST` etc. (case-insensitive, `_` allowed for spaces).
    pub fn parse(text: &str) -> Result<Self> {
        let lower = text.to_lowercase().replace('_', " ");
        let order = if lower.starts_with("asc") {
            OrderType::Ascending
        } else if lower.starts_with("desc") {
            OrderType::Descending
        } else {
            return Err(Error::binder(
                "create_sort_key modifier must start with either ASC or DESC",
            ));
        };
        let nulls = if lower.ends_with("nulls first") {
            NullOrder::NullsFirst
        } else if lower.ends_with("nulls last") {
            NullOrder::NullsLast
        } else {
            return Err(Error::binder(
                "create_sort_key modifier must end with either NULLS FIRST or NULLS LAST",
            ));
        };
        Ok(Self { order, nulls })
    }

    /// The modifiers used inside nested values: NULLs last for ASC, first for DESC.
    fn child(self) -> Self {
        let nulls = match self.order {
            OrderType::Ascending => NullOrder::NullsLast,
            OrderType::Descending => NullOrder::NullsFirst,
        };
        Self {
            order: self.order,
            nulls,
        }
    }

    /// `(null byte, valid byte)`.
    pub(crate) fn validity_bytes(self) -> (u8, u8) {
        match self.nulls {
            NullOrder::NullsFirst => (NULL_FIRST_BYTE, NULL_LAST_BYTE),
            NullOrder::NullsLast => (NULL_LAST_BYTE, NULL_FIRST_BYTE),
        }
    }

    pub(crate) fn flip(self) -> bool {
        self.order == OrderType::Descending
    }
}

const NULL_FIRST_BYTE: u8 = 1;
const NULL_LAST_BYTE: u8 = 2;
const STRING_DELIMITER: u8 = 0;
const LIST_DELIMITER: u8 = 0;
const BLOB_ESCAPE: u8 = 1;

/// The type `create_sort_key` returns for keys over `types`: BIGINT if every column is
/// fixed-width and the key fits in 8 bytes, BLOB otherwise.
pub fn sort_key_type_for(types: &[LogicalType]) -> LogicalType {
    let mut size = 0;
    for ty in types {
        let physical = ty.physical_type();
        if !is_constant_size(physical) {
            return LogicalType::BLOB;
        }
        size += physical.size() + 1;
    }
    if size <= 8 {
        LogicalType::BIGINT
    } else {
        LogicalType::BLOB
    }
}

fn is_constant_size(physical: PhysicalType) -> bool {
    !matches!(
        physical,
        PhysicalType::Varchar | PhysicalType::List | PhysicalType::Array | PhysicalType::Struct
    ) && physical.size() > 0
}

/// Normalized keys of a batch of rows: key `i` is `bytes(i)`.
#[derive(Clone, Debug, Default)]
pub struct SortKeyBuffer {
    data: Vec<u8>,
    offsets: Vec<usize>,
}

impl SortKeyBuffer {
    pub fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The key of row `row`.
    pub fn key(&self, row: usize) -> &[u8] {
        &self.data[self.offsets[row]..self.offsets[row + 1]]
    }

    /// The key of row `row` as DuckDB's BIGINT sort key (keys of at most 8 bytes).
    pub fn key_as_bigint(&self, row: usize) -> i64 {
        let key = self.key(row);
        let mut bytes = [0u8; 8];
        bytes[..key.len()].copy_from_slice(key);
        // DuckDB byte-swaps the big-endian key into the integer on little-endian machines.
        u64::from_be_bytes(bytes) as i64
    }
}

/// One column to encode: its format plus its modifiers.
struct Encoder<'a> {
    format: VectorFormat<'a>,
    is_varchar: bool,
    modifiers: OrderModifiers,
    children: Vec<Encoder<'a>>,
}

impl<'a> Encoder<'a> {
    fn new(format: VectorFormat<'a>, modifiers: OrderModifiers) -> Self {
        let is_varchar = format.vector.logical_type().id() == LogicalTypeId::Varchar;
        let child_modifiers = modifiers.child();
        let mut encoder = Self {
            format,
            is_varchar,
            modifiers,
            children: Vec::new(),
        };
        let child_formats = std::mem::take(&mut encoder.format.children);
        encoder.children = child_formats
            .into_iter()
            .map(|child| Encoder::new(child, child_modifiers))
            .collect();
        encoder
    }

    /// Bytes of the encoding of logical row `row`.
    fn length(&self, row: usize) -> usize {
        let slot = self.format.slot(row);
        let valid = self.format.slot_is_valid(slot);
        1 + match self.format.kind {
            FormatKind::Fixed(width) => {
                if valid {
                    width
                } else {
                    0
                }
            }
            FormatKind::Varchar => {
                if !valid {
                    0
                } else if self.is_varchar {
                    self.format.string(slot).len() + 1
                } else {
                    let bytes = self.format.string_bytes(slot);
                    bytes.len() + bytes.iter().filter(|&&byte| byte <= 1).count() + 1
                }
            }
            FormatKind::Struct => self.children.iter().map(|child| child.length(row)).sum(),
            FormatKind::List | FormatKind::Array(_) => {
                if !valid && self.format.kind == FormatKind::List {
                    0
                } else {
                    let (offset, length) = self.format.list_entry(slot);
                    1 + (offset..offset + length)
                        .map(|element| self.children[0].length(element))
                        .sum::<usize>()
                }
            }
        }
    }

    /// Appends the encoding of logical row `row` to `out`.
    fn encode(&self, row: usize, flip: bool, out: &mut Vec<u8>) {
        let slot = self.format.slot(row);
        let (null_byte, valid_byte) = self.modifiers.validity_bytes();
        let valid = self.format.slot_is_valid(slot);
        out.push(if valid { valid_byte } else { null_byte });
        match self.format.kind {
            FormatKind::Fixed(width) => {
                if valid {
                    let start = out.len();
                    encode_fixed(
                        self.format.vector.logical_type().physical_type(),
                        self.format.slot_bytes(slot, width),
                        out,
                    );
                    flip_bytes(&mut out[start..], flip);
                }
            }
            FormatKind::Varchar => {
                if valid {
                    let start = out.len();
                    let bytes = self.format.string_bytes(slot);
                    if self.is_varchar {
                        out.extend(bytes.iter().map(|&byte| byte.wrapping_add(1)));
                    } else {
                        for &byte in bytes {
                            if byte <= 1 {
                                out.push(BLOB_ESCAPE);
                            }
                            out.push(byte);
                        }
                    }
                    out.push(STRING_DELIMITER);
                    flip_bytes(&mut out[start..], flip);
                }
            }
            FormatKind::Struct => {
                for child in &self.children {
                    child.encode(row, flip, out);
                }
            }
            FormatKind::List | FormatKind::Array(_) => {
                // Arrays write their elements even when the array itself is NULL.
                if valid || self.format.kind != FormatKind::List {
                    let (offset, length) = self.format.list_entry(slot);
                    for element in offset..offset + length {
                        self.children[0].encode(element, flip, out);
                    }
                    out.push(if flip { !LIST_DELIMITER } else { LIST_DELIMITER });
                }
            }
        }
    }
}

fn flip_bytes(bytes: &mut [u8], flip: bool) {
    if flip {
        for byte in bytes {
            *byte = !*byte;
        }
    }
}

/// `Radix::EncodeFloat`.
fn encode_f32(value: f32) -> u32 {
    if value == 0.0 {
        return 1 << 31;
    }
    if value.is_nan() {
        return u32::MAX;
    }
    if value > f32::MAX {
        return u32::MAX - 1;
    }
    if value < -f32::MAX {
        return 0;
    }
    let bits = value.to_bits();
    if bits & (1 << 31) == 0 {
        bits | (1 << 31)
    } else {
        !bits
    }
}

fn decode_f32(input: u32) -> f32 {
    match input {
        u32::MAX => f32::NAN,
        x if x == u32::MAX - 1 => f32::INFINITY,
        0 => f32::NEG_INFINITY,
        x if x & (1 << 31) != 0 => f32::from_bits(x ^ (1 << 31)),
        x => f32::from_bits(!x),
    }
}

/// `Radix::EncodeDouble`.
fn encode_f64(value: f64) -> u64 {
    if value == 0.0 {
        return 1 << 63;
    }
    if value.is_nan() {
        return u64::MAX;
    }
    if value > f64::MAX {
        return u64::MAX - 1;
    }
    if value < -f64::MAX {
        return 0;
    }
    let bits = value.to_bits();
    if bits < (1 << 63) { bits + (1 << 63) } else { !bits }
}

fn decode_f64(input: u64) -> f64 {
    match input {
        u64::MAX => f64::NAN,
        x if x == u64::MAX - 1 => f64::INFINITY,
        0 => f64::NEG_INFINITY,
        x if x & (1 << 63) != 0 => f64::from_bits(x ^ (1 << 63)),
        x => f64::from_bits(!x),
    }
}

macro_rules! read_ne {
    ($ty:ty, $bytes:expr) => {{
        let mut buffer = [0u8; size_of::<$ty>()];
        buffer.copy_from_slice(&$bytes[..size_of::<$ty>()]);
        <$ty>::from_ne_bytes(buffer)
    }};
}

/// Big-endian with the sign bit flipped for signed integers.
fn push_signed(out: &mut Vec<u8>, be: &[u8]) {
    let start = out.len();
    out.extend_from_slice(be);
    out[start] ^= 0x80;
}

/// `Radix::EncodeData` for a value of physical type `physical` stored natively in `bytes`.
fn encode_fixed(physical: PhysicalType, bytes: &[u8], out: &mut Vec<u8>) {
    match physical {
        PhysicalType::Bool => out.push(u8::from(bytes[0] != 0)),
        PhysicalType::UInt8 => out.push(bytes[0]),
        PhysicalType::Int8 => push_signed(out, &read_ne!(i8, bytes).to_be_bytes()),
        PhysicalType::Int16 => push_signed(out, &read_ne!(i16, bytes).to_be_bytes()),
        PhysicalType::UInt16 => out.extend_from_slice(&read_ne!(u16, bytes).to_be_bytes()),
        PhysicalType::Int32 => push_signed(out, &read_ne!(i32, bytes).to_be_bytes()),
        PhysicalType::UInt32 => out.extend_from_slice(&read_ne!(u32, bytes).to_be_bytes()),
        PhysicalType::Int64 => push_signed(out, &read_ne!(i64, bytes).to_be_bytes()),
        PhysicalType::UInt64 => out.extend_from_slice(&read_ne!(u64, bytes).to_be_bytes()),
        // hugeint_t is (upper: i64, lower: u64): signed upper half, unsigned lower half.
        PhysicalType::Int128 => push_signed(out, &read_ne!(i128, bytes).to_be_bytes()),
        PhysicalType::UInt128 => out.extend_from_slice(&read_ne!(u128, bytes).to_be_bytes()),
        PhysicalType::Float => out.extend_from_slice(&encode_f32(read_ne!(f32, bytes)).to_be_bytes()),
        PhysicalType::Double => out.extend_from_slice(&encode_f64(read_ne!(f64, bytes)).to_be_bytes()),
        PhysicalType::Interval => {
            push_signed(out, &read_ne!(i32, bytes[0..4]).to_be_bytes());
            push_signed(out, &read_ne!(i32, bytes[4..8]).to_be_bytes());
            push_signed(out, &read_ne!(i64, bytes[8..16]).to_be_bytes());
        }
        _ => out.extend_from_slice(bytes),
    }
}

/// Encodes the first `count` rows of `columns` with `modifiers` (`CreateSortKey`).
pub fn create_sort_keys(
    columns: &[&Vector],
    modifiers: &[OrderModifiers],
    count: usize,
) -> Result<SortKeyBuffer> {
    if columns.len() != modifiers.len() {
        return Err(Error::internal(
            "create_sort_keys: one modifier per column is required",
        ));
    }
    let encoders = columns
        .iter()
        .zip(modifiers)
        .map(|(column, &modifiers)| Ok(Encoder::new(VectorFormat::new(column, count)?, modifiers)))
        .collect::<Result<Vec<_>>>()?;
    let mut buffer = SortKeyBuffer {
        data: Vec::new(),
        offsets: Vec::with_capacity(count + 1),
    };
    let total: usize = (0..count)
        .map(|row| encoders.iter().map(|encoder| encoder.length(row)).sum::<usize>())
        .sum();
    buffer.data.reserve(total);
    buffer.offsets.push(0);
    for row in 0..count {
        for encoder in &encoders {
            encoder.encode(row, encoder.modifiers.flip(), &mut buffer.data);
        }
        buffer.offsets.push(buffer.data.len());
    }
    Ok(buffer)
}

/// Writes keys as DuckDB's `create_sort_key()` result: a BIGINT vector (keys of at most 8
/// bytes) or a BLOB vector.
pub fn sort_keys_to_vector(keys: &SortKeyBuffer, result: &mut Vector) -> Result<()> {
    let count = keys.len();
    match result.logical_type().id() {
        LogicalTypeId::BigInt => {
            *result = Vector::flat(LogicalType::BIGINT, count.max(1));
            let values = result.values_mut::<i64>();
            for (row, value) in values[..count].iter_mut().enumerate() {
                *value = keys.key_as_bigint(row);
            }
        }
        LogicalTypeId::Blob => {
            *result = Vector::flat(LogicalType::BLOB, count.max(1));
            for row in 0..count {
                let string = result.add_string(keys.key(row));
                result.values_mut::<StringT>()[row] = string;
            }
        }
        _ => return Err(Error::internal("Unsupported key type for CreateSortKey")),
    }
    Ok(())
}

/// Decodes one key into row `row` of `columns` (`DecodeSortKey`); returns the bytes read.
/// The vectors must be flat and have room for `row`.
pub fn decode_sort_key(
    key: &[u8],
    columns: &mut [Vector],
    row: usize,
    modifiers: &[OrderModifiers],
) -> Result<usize> {
    let mut position = 0;
    for (column, &modifiers) in columns.iter_mut().zip(modifiers) {
        decode_value(key, &mut position, column, row, modifiers)?;
    }
    Ok(position)
}

fn corrupt() -> Error {
    Error::invalid_input("Failed to decode sort key: unexpected end of key")
}

fn take<'a>(key: &'a [u8], position: &mut usize, len: usize) -> Result<&'a [u8]> {
    let bytes = key.get(*position..*position + len).ok_or_else(corrupt)?;
    *position += len;
    Ok(bytes)
}

fn decode_value(
    key: &[u8],
    position: &mut usize,
    target: &mut Vector,
    row: usize,
    modifiers: OrderModifiers,
) -> Result<()> {
    let (null_byte, _) = modifiers.validity_bytes();
    let flip = modifiers.flip();
    let validity = *take(key, position, 1)?.first().ok_or_else(corrupt)?;
    let is_null = validity == null_byte;
    let physical = target.logical_type().physical_type();
    ensure_capacity(target, row + 1);
    if is_null {
        let capacity = target.row_capacity().max(row + 1);
        target.validity_mut().set_invalid(row, capacity);
        // Structs and arrays still encode their children.
        match physical {
            PhysicalType::Struct | PhysicalType::Array => {}
            _ => return Ok(()),
        }
    }
    let child_modifiers = modifiers.child();
    match physical {
        PhysicalType::Varchar => {
            let (delimiter, escape) = if flip {
                (!STRING_DELIMITER, !BLOB_ESCAPE)
            } else {
                (STRING_DELIMITER, BLOB_ESCAPE)
            };
            let mut value = Vec::new();
            if target.logical_type().id() == LogicalTypeId::Varchar {
                // VARCHAR bytes are shifted by one and contain no delimiter.
                let rest = key.get(*position..).ok_or_else(corrupt)?;
                let end = rest
                    .iter()
                    .position(|&byte| byte == delimiter)
                    .ok_or_else(corrupt)?;
                *position += end + 1;
                let mut short = [0u8; 64];
                let decoded: &mut [u8] = if end <= short.len() {
                    &mut short[..end]
                } else {
                    value.resize(end, 0);
                    &mut value
                };
                for (out, &byte) in decoded.iter_mut().zip(&rest[..end]) {
                    *out = if flip { !byte } else { byte }.wrapping_sub(1);
                }
                let string = target.add_string(decoded);
                target.values_mut::<StringT>()[row] = string;
                return Ok(());
            }
            loop {
                let mut byte = *key.get(*position).ok_or_else(corrupt)?;
                *position += 1;
                if byte == delimiter {
                    break;
                }
                if byte == escape {
                    byte = *key.get(*position).ok_or_else(corrupt)?;
                    *position += 1;
                }
                value.push(if flip { !byte } else { byte });
            }
            let string = target.add_string(&value);
            target.values_mut::<StringT>()[row] = string;
        }
        PhysicalType::Struct => {
            let children = target
                .struct_children_mut()
                .ok_or_else(|| Error::internal("struct vector without children"))?;
            for child in children {
                decode_value(key, position, child, row, child_modifiers)?;
            }
        }
        PhysicalType::List => {
            let delimiter = if flip { !LIST_DELIMITER } else { LIST_DELIMITER };
            let start = target.list_child().map_or(0, |(_, size)| size);
            let mut size = start;
            let child = target
                .list_child_mut()
                .ok_or_else(|| Error::internal("list vector without child"))?;
            while *key.get(*position).ok_or_else(corrupt)? != delimiter {
                decode_value(key, position, child, size, child_modifiers)?;
                size += 1;
            }
            *position += 1;
            target.set_list_size(size);
            target.values_mut::<ListEntry>()[row] = ListEntry {
                offset: start as u64,
                length: (size - start) as u64,
            };
        }
        PhysicalType::Array => {
            let array_size = match target.logical_type().info() {
                Some(TypeInfo::Array { size, .. }) => *size as usize,
                _ => return Err(Error::internal("ARRAY type without size")),
            };
            let delimiter = if flip { !LIST_DELIMITER } else { LIST_DELIMITER };
            let child = target
                .array_child_mut()
                .ok_or_else(|| Error::internal("array vector without child"))?;
            let mut found = 0;
            while *key.get(*position).ok_or_else(corrupt)? != delimiter {
                found += 1;
                if found > array_size {
                    break;
                }
                decode_value(
                    key,
                    position,
                    child,
                    row * array_size + found - 1,
                    child_modifiers,
                )?;
            }
            *position += 1;
            if found != array_size {
                return Err(Error::invalid_input(format!(
                    "Failed to decode array - found {found} elements but expected {array_size}"
                )));
            }
        }
        PhysicalType::Bit | PhysicalType::Unknown | PhysicalType::Invalid => {
            return Err(Error::not_implemented(format!(
                "Unsupported type {} in DecodeSortKey",
                target.logical_type()
            )));
        }
        fixed => {
            let width = fixed.size();
            let encoded = take(key, position, width)?;
            decode_fixed_into(
                fixed,
                encoded,
                flip,
                &mut target.data_bytes_mut()[row * width..(row + 1) * width],
            );
        }
    }
    Ok(())
}

/// Inverse of [`encode_fixed`]: writes the native bytes of the value encoded in `encoded`
/// (bitwise inverted first when `flip`) to `out`, which has the value's width.
pub(crate) fn decode_fixed_into(physical: PhysicalType, encoded: &[u8], flip: bool, out: &mut [u8]) {
    let width = out.len();
    let mut bytes = [0u8; 16];
    bytes[..width].copy_from_slice(&encoded[..width]);
    if flip {
        for byte in &mut bytes[..width] {
            *byte = !*byte;
        }
    }
    macro_rules! be {
        ($ty:ty, $range:expr) => {{
            let mut buffer = [0u8; size_of::<$ty>()];
            buffer.copy_from_slice(&bytes[$range]);
            <$ty>::from_be_bytes(buffer)
        }};
    }
    match physical {
        PhysicalType::Int8
        | PhysicalType::Int16
        | PhysicalType::Int32
        | PhysicalType::Int64
        | PhysicalType::Int128 => {
            bytes[0] ^= 0x80;
            match physical {
                PhysicalType::Int8 => out.copy_from_slice(&be!(i8, 0..1).to_ne_bytes()),
                PhysicalType::Int16 => out.copy_from_slice(&be!(i16, 0..2).to_ne_bytes()),
                PhysicalType::Int32 => out.copy_from_slice(&be!(i32, 0..4).to_ne_bytes()),
                PhysicalType::Int64 => out.copy_from_slice(&be!(i64, 0..8).to_ne_bytes()),
                _ => out.copy_from_slice(&be!(i128, 0..16).to_ne_bytes()),
            }
        }
        PhysicalType::UInt16 => out.copy_from_slice(&be!(u16, 0..2).to_ne_bytes()),
        PhysicalType::UInt32 => out.copy_from_slice(&be!(u32, 0..4).to_ne_bytes()),
        PhysicalType::UInt64 => out.copy_from_slice(&be!(u64, 0..8).to_ne_bytes()),
        PhysicalType::UInt128 => out.copy_from_slice(&be!(u128, 0..16).to_ne_bytes()),
        PhysicalType::Float => out.copy_from_slice(&decode_f32(be!(u32, 0..4)).to_ne_bytes()),
        PhysicalType::Double => out.copy_from_slice(&decode_f64(be!(u64, 0..8)).to_ne_bytes()),
        PhysicalType::Interval => {
            bytes[0] ^= 0x80;
            bytes[4] ^= 0x80;
            bytes[8] ^= 0x80;
            out[0..4].copy_from_slice(&be!(i32, 0..4).to_ne_bytes());
            out[4..8].copy_from_slice(&be!(i32, 4..8).to_ne_bytes());
            out[8..16].copy_from_slice(&be!(i64, 8..16).to_ne_bytes());
        }
        // BOOLEAN, UTINYINT and anything stored as raw bytes.
        _ => out.copy_from_slice(&bytes[..width]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_types::Value;

    fn int_vector(values: &[Option<i32>]) -> Vector {
        let mut vector = Vector::flat(LogicalType::INTEGER, values.len());
        for (row, value) in values.iter().enumerate() {
            vector
                .set_value(
                    row,
                    &value.map_or(Value::null(LogicalType::INTEGER), Value::integer),
                )
                .unwrap();
        }
        vector
    }

    #[test]
    fn parses_modifiers() {
        assert_eq!(
            OrderModifiers::parse("desc_nulls_first").unwrap(),
            OrderModifiers::new(OrderType::Descending, NullOrder::NullsFirst)
        );
        assert_eq!(
            OrderModifiers::parse("sideways").unwrap_err().to_string(),
            "Binder Error: create_sort_key modifier must start with either ASC or DESC"
        );
        assert_eq!(
            OrderModifiers::parse("ASC").unwrap_err().to_string(),
            "Binder Error: create_sort_key modifier must end with either NULLS FIRST or NULLS LAST"
        );
    }

    #[test]
    fn integer_keys_match_duckdb() {
        // SELECT create_sort_key(x, 'ASC NULLS LAST') FROM (VALUES (1), (-1), (NULL)) t(x)
        let column = int_vector(&[Some(1), Some(-1), None]);
        let asc = OrderModifiers::new(OrderType::Ascending, NullOrder::NullsLast);
        let keys = create_sort_keys(&[&column], &[asc], 3).unwrap();
        assert_eq!(sort_key_type_for(&[LogicalType::INTEGER]), LogicalType::BIGINT);
        assert_eq!(keys.key(0), &[1, 0x80, 0, 0, 1]);
        assert_eq!(keys.key_as_bigint(0), 108_086_391_073_669_120);
        assert_eq!(keys.key_as_bigint(1), 108_086_391_040_114_688);
        assert_eq!(keys.key_as_bigint(2), 144_115_188_075_855_872);
        let mut decoded = vec![Vector::flat(LogicalType::INTEGER, 3)];
        for row in 0..3 {
            decode_sort_key(keys.key(row), &mut decoded, row, &[asc]).unwrap();
        }
        assert_eq!(decoded[0].get_value(1), Value::integer(-1));
        assert!(decoded[0].is_null(2));
    }

    #[test]
    fn strings_and_desc() {
        let mut column = Vector::flat(LogicalType::VARCHAR, 2);
        column.set_value(0, &Value::varchar("ab")).unwrap();
        column.set_value(1, &Value::varchar("")).unwrap();
        let desc = OrderModifiers::new(OrderType::Descending, NullOrder::NullsFirst);
        let keys = create_sort_keys(&[&column], &[desc], 2).unwrap();
        // valid byte 2 (NULLS FIRST), then !(b'a' + 1), !(b'b' + 1), !0
        assert_eq!(keys.key(0), &[2, !b'b', !b'c', 0xFF]);
        assert_eq!(keys.key(1), &[2, 0xFF]);
        assert!(keys.key(1) > keys.key(0), "DESC puts the empty string last");
        // A BLOB escapes 0 and 1: create_sort_key('a\\x00b'::BLOB, 'ASC NULLS LAST').
        let mut blob = Vector::flat(LogicalType::BLOB, 1);
        blob.set_value(0, &Value::blob(b"a\0b")).unwrap();
        let asc = OrderModifiers::new(OrderType::Ascending, NullOrder::NullsLast);
        let keys = create_sort_keys(&[&blob], &[asc], 1).unwrap();
        assert_eq!(keys.key(0), b"\x01a\x01\x00b\x00");
        let mut decoded = vec![Vector::flat(LogicalType::BLOB, 1)];
        decode_sort_key(keys.key(0), &mut decoded, 0, &[asc]).unwrap();
        assert_eq!(decoded[0].string_bytes(0), b"a\0b");
    }

    #[test]
    fn nested_keys_match_duckdb() {
        // create_sort_key(x, 'ASC NULLS FIRST') for [1, NULL], NULL and [].
        let mut list = Vector::flat(LogicalType::list(LogicalType::INTEGER), 3);
        {
            let child = list.list_child_mut().unwrap();
            child.set_value(0, &Value::integer(1)).unwrap();
            child.set_value(1, &Value::null(LogicalType::INTEGER)).unwrap();
        }
        list.set_list_size(2);
        list.values_mut::<ListEntry>()[0] = ListEntry { offset: 0, length: 2 };
        list.values_mut::<ListEntry>()[2] = ListEntry { offset: 2, length: 0 };
        list.validity_mut().set_invalid(1, 3);
        let asc_first = OrderModifiers::new(OrderType::Ascending, NullOrder::NullsFirst);
        let keys = create_sort_keys(&[&list], &[asc_first], 3).unwrap();
        assert_eq!(keys.key(0), &[0x02, 0x01, 0x80, 0x00, 0x00, 0x01, 0x02, 0x00]);
        assert_eq!(keys.key(1), &[0x01]);
        assert_eq!(keys.key(2), &[0x02, 0x00]);
        let mut decoded = vec![Vector::flat(LogicalType::list(LogicalType::INTEGER), 3)];
        for row in 0..3 {
            decode_sort_key(keys.key(row), &mut decoded, row, &[asc_first]).unwrap();
        }
        assert!(decoded[0].is_null(1));
        let (child, size) = decoded[0].list_child().unwrap();
        assert_eq!(size, 2);
        assert_eq!(child.get_value(0), Value::integer(1));
        assert!(child.is_null(1));

        // create_sort_key({'a': 1, 'b': NULL::VARCHAR}, 'DESC NULLS LAST')
        let ty = LogicalType::with_info(
            LogicalTypeId::Struct,
            TypeInfo::Struct {
                children: vec![
                    ("a".into(), LogicalType::INTEGER),
                    ("b".into(), LogicalType::VARCHAR),
                ],
            },
        );
        let mut row = Vector::flat(ty, 1);
        {
            let children = row.struct_children_mut().unwrap();
            children[0].set_value(0, &Value::integer(1)).unwrap();
            children[1]
                .set_value(0, &Value::null(LogicalType::VARCHAR))
                .unwrap();
        }
        let desc_last = OrderModifiers::new(OrderType::Descending, NullOrder::NullsLast);
        let keys = create_sort_keys(&[&row], &[desc_last], 1).unwrap();
        assert_eq!(keys.key(0), &[0x01, 0x02, 0x7F, 0xFF, 0xFF, 0xFE, 0x01]);
    }
}
