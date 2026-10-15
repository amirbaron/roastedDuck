//! Shared helpers for the row-format tests: a deterministic RNG, a type-independent value
//! model for any (nested) type, and conversions between it and vectors.
#![allow(dead_code, clippy::unwrap_used, clippy::panic)]

use rustdb_buffer::{BufferManager, BufferManagerConfig};
use rustdb_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, StringT, TypeInfo, Vector};

/// `SplitMix64`.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }

    /// True with probability `percent`%.
    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

/// A value of any type: NULL, raw fixed-width bytes, string bytes, list/array elements or
/// struct fields.
#[derive(Clone, Debug, PartialEq)]
pub enum TestValue {
    Null,
    Fixed(Vec<u8>),
    Str(Vec<u8>),
    List(Vec<TestValue>),
    Struct(Vec<TestValue>),
}

pub fn struct_type(fields: &[(&str, LogicalType)]) -> LogicalType {
    LogicalType::with_info(
        LogicalTypeId::Struct,
        TypeInfo::Struct {
            children: fields
                .iter()
                .map(|(name, ty)| ((*name).to_owned(), ty.clone()))
                .collect(),
        },
    )
}

pub fn array_type(child: LogicalType, size: u32) -> LogicalType {
    LogicalType::with_info(LogicalTypeId::Array, TypeInfo::Array { child, size })
}

pub fn map_type(key: LogicalType, value: LogicalType) -> LogicalType {
    LogicalType::with_info(
        LogicalTypeId::Map,
        TypeInfo::List {
            child: struct_type(&[("key", key), ("value", value)]),
        },
    )
}

fn struct_fields(ty: &LogicalType) -> Vec<LogicalType> {
    match ty.info() {
        Some(TypeInfo::Struct { children }) => children.iter().map(|(_, ty)| ty.clone()).collect(),
        _ => panic!("not a struct: {ty}"),
    }
}

fn element_type(ty: &LogicalType) -> (LogicalType, Option<usize>) {
    match ty.info() {
        Some(TypeInfo::List { child }) => (child.clone(), None),
        Some(TypeInfo::Array { child, size }) => (child.clone(), Some(*size as usize)),
        _ => panic!("not a list/array: {ty}"),
    }
}

/// A random value of `ty`; NULL with probability `null_percent`%.
pub fn random_value(ty: &LogicalType, rng: &mut Rng, null_percent: u64) -> TestValue {
    use rustdb_types::PhysicalType as P;
    if rng.chance(null_percent) {
        return TestValue::Null;
    }
    match ty.physical_type() {
        P::Bool => TestValue::Fixed(vec![(rng.next_u64() & 1) as u8]),
        P::Varchar => {
            // A mix of empty, inlined (<= 12 bytes) and heap strings.
            let len = match rng.below(4) {
                0 => rng.below(4),
                1 => rng.below(13),
                _ => 13 + rng.below(40),
            } as usize;
            TestValue::Str((0..len).map(|_| b'a' + (rng.below(26) as u8)).collect())
        }
        P::Struct => TestValue::Struct(
            struct_fields(ty)
                .iter()
                .map(|field| random_value(field, rng, null_percent))
                .collect(),
        ),
        P::List | P::Array => {
            let (child, size) = element_type(ty);
            let len = size.unwrap_or_else(|| rng.below(5) as usize);
            TestValue::List(
                (0..len)
                    .map(|_| random_value(&child, rng, null_percent))
                    .collect(),
            )
        }
        fixed => TestValue::Fixed((0..fixed.size()).map(|_| rng.next_u64() as u8).collect()),
    }
}

pub fn random_values(ty: &LogicalType, count: usize, rng: &mut Rng, null_percent: u64) -> Vec<TestValue> {
    (0..count).map(|_| random_value(ty, rng, null_percent)).collect()
}

fn write_value(vector: &mut Vector, row: usize, value: &TestValue) {
    use rustdb_types::PhysicalType as P;
    let ty = vector.logical_type().clone();
    if matches!(value, TestValue::Null) {
        let capacity = vector.row_capacity().max(row + 1);
        vector.validity_mut().set_invalid(row, capacity);
        if ty.physical_type() == P::Struct {
            let fields = vector.struct_children_mut().unwrap();
            for field in fields {
                write_value(field, row, &TestValue::Null);
            }
        }
        return;
    }
    match (ty.physical_type(), value) {
        (P::Varchar, TestValue::Str(bytes)) => {
            let string = vector.add_string(bytes);
            vector.values_mut::<StringT>()[row] = string;
        }
        (P::Struct, TestValue::Struct(fields)) => {
            let children = vector.struct_children_mut().unwrap();
            for (child, field) in children.iter_mut().zip(fields) {
                write_value(child, row, field);
            }
        }
        (P::List, TestValue::List(elements)) => {
            let offset = vector.list_child().unwrap().1;
            let child = vector.list_child_mut().unwrap();
            child.reserve(offset + elements.len());
            for (i, element) in elements.iter().enumerate() {
                write_value(child, offset + i, element);
            }
            vector.set_list_size(offset + elements.len());
            vector.values_mut::<ListEntry>()[row] = ListEntry {
                offset: offset as u64,
                length: elements.len() as u64,
            };
        }
        (P::Array, TestValue::List(elements)) => {
            let child = vector.array_child_mut().unwrap();
            let size = elements.len();
            for (i, element) in elements.iter().enumerate() {
                write_value(child, row * size + i, element);
            }
        }
        (_, TestValue::Fixed(bytes)) => {
            let width = bytes.len();
            vector.data_bytes_mut()[row * width..(row + 1) * width].copy_from_slice(bytes);
        }
        (physical, value) => panic!("cannot write {value:?} into a {physical:?} vector"),
    }
}

/// A flat vector holding `values`.
pub fn build_vector(ty: &LogicalType, values: &[TestValue]) -> Vector {
    let mut vector = Vector::flat(ty.clone(), values.len().max(1));
    for (row, value) in values.iter().enumerate() {
        write_value(&mut vector, row, value);
    }
    vector
}

/// The value of logical row `row` of `vector` (any representation).
pub fn read_value(vector: &Vector, row: usize) -> TestValue {
    use rustdb_types::PhysicalType as P;
    let ty = vector.logical_type();
    let slot = vector.sel().slot(row);
    if !vector.validity().is_valid(slot) {
        return TestValue::Null;
    }
    match ty.physical_type() {
        P::Varchar => {
            let string = vector.values::<StringT>()[slot];
            TestValue::Str(vector.string_value(&string).to_vec())
        }
        P::Struct => TestValue::Struct(
            vector
                .struct_children()
                .unwrap()
                .iter()
                .map(|child| read_value(child, slot))
                .collect(),
        ),
        P::List => {
            let entry = vector.values::<ListEntry>()[slot];
            let (child, _) = vector.list_child().unwrap();
            TestValue::List(
                (0..entry.length as usize)
                    .map(|i| read_value(child, entry.offset as usize + i))
                    .collect(),
            )
        }
        P::Array => {
            let (_, size) = element_type(ty);
            let size = size.unwrap();
            let child = vector.array_child().unwrap();
            TestValue::List((0..size).map(|i| read_value(child, slot * size + i)).collect())
        }
        fixed => {
            let width = fixed.size();
            TestValue::Fixed(vector.data_bytes()[slot * width..(slot + 1) * width].to_vec())
        }
    }
}

pub fn read_vector(vector: &Vector, count: usize) -> Vec<TestValue> {
    (0..count).map(|row| read_value(vector, row)).collect()
}

/// Rows of `chunks`, column-major per chunk, as one row-major table.
pub fn read_rows(chunks: &[DataChunk]) -> Vec<Vec<TestValue>> {
    let mut rows = Vec::new();
    for chunk in chunks {
        for row in 0..chunk.len() {
            rows.push(
                chunk
                    .columns()
                    .iter()
                    .map(|column| read_value(column, row))
                    .collect(),
            );
        }
    }
    rows
}

/// Builds chunks of at most `chunk_size` rows from a row-major table.
pub fn build_chunks(types: &[LogicalType], rows: &[Vec<TestValue>], chunk_size: usize) -> Vec<DataChunk> {
    rows.chunks(chunk_size)
        .map(|rows| {
            let columns = types
                .iter()
                .enumerate()
                .map(|(col, ty)| {
                    let values: Vec<_> = rows.iter().map(|row| row[col].clone()).collect();
                    build_vector(ty, &values)
                })
                .collect();
            DataChunk::new(columns, rows.len())
        })
        .collect()
}

/// A buffer manager with plenty of memory and no spilling.
pub fn buffer_manager() -> BufferManager {
    BufferManager::new(BufferManagerConfig {
        memory_limit: Some(4 << 30),
        ..BufferManagerConfig::default()
    })
    .unwrap()
}

/// A fresh temporary directory path (not created).
pub fn temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("rustdb-row-{name}-{}-{nanos}", std::process::id()))
}

/// A buffer manager limited to `limit` bytes that spills to a fresh temp directory.
pub fn spilling_buffer_manager(name: &str, limit: u64) -> BufferManager {
    BufferManager::new(BufferManagerConfig {
        memory_limit: Some(limit),
        temp_directory: Some(temp_dir(name)),
        ..BufferManagerConfig::default()
    })
    .unwrap()
}

/// Every type the row format supports, including nested combinations.
pub fn all_types() -> Vec<LogicalType> {
    let point = struct_type(&[("x", LogicalType::INTEGER), ("label", LogicalType::VARCHAR)]);
    vec![
        LogicalType::BOOLEAN,
        LogicalType::TINYINT,
        LogicalType::SMALLINT,
        LogicalType::INTEGER,
        LogicalType::BIGINT,
        LogicalType::HUGEINT,
        LogicalType::UTINYINT,
        LogicalType::USMALLINT,
        LogicalType::UINTEGER,
        LogicalType::UBIGINT,
        LogicalType::UHUGEINT,
        LogicalType::FLOAT,
        LogicalType::DOUBLE,
        LogicalType::DATE,
        LogicalType::TIMESTAMP,
        LogicalType::INTERVAL,
        LogicalType::decimal(4, 1),
        LogicalType::decimal(18, 3),
        LogicalType::decimal(38, 10),
        LogicalType::VARCHAR,
        LogicalType::BLOB,
        LogicalType::list(LogicalType::INTEGER),
        LogicalType::list(LogicalType::VARCHAR),
        LogicalType::list(LogicalType::list(LogicalType::BIGINT)),
        LogicalType::list(LogicalType::list(LogicalType::VARCHAR)),
        LogicalType::list(point.clone()),
        point.clone(),
        struct_type(&[
            ("inner", point.clone()),
            ("tags", LogicalType::list(LogicalType::VARCHAR)),
        ]),
        array_type(LogicalType::INTEGER, 3),
        array_type(LogicalType::VARCHAR, 2),
        LogicalType::list(array_type(LogicalType::SMALLINT, 2)),
        array_type(LogicalType::list(LogicalType::INTEGER), 2),
        map_type(LogicalType::VARCHAR, LogicalType::DOUBLE),
    ]
}
