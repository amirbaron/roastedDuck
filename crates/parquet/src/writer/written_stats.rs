//! The statistics of a written file that `COPY ... (RETURN_STATS)` returns
//! (`ParquetWriter::GatherWrittenStatistics`, `FlushColumnStats` and the
//! `ColumnStatsUnifier`s): per leaf column, the min/max over all row groups, the NULL
//! count, the number of values and the compressed size.

use std::cmp::Ordering;
use std::fmt::Write as _;

use rustdb_function::copy::CopyFunctionFileStatistics;
use rustdb_types::{Date, LogicalType, LogicalTypeId, PhysicalType, Time, TimeNs, TimeTz, Timestamp, Value};

use super::column::{ColumnWriter, FinishedColumn};
use crate::column_reader::values::DecimalTarget;
use crate::thrift::{FieldRepetitionType, FileMetaData};

/// How the min/max bytes of a column are compared and printed (`GetBaseStatsUnifier`).
#[derive(Clone, Copy, Debug)]
enum StatsKind {
    /// `NumericStatsUnifier<T>` over the little-endian bytes of `T`.
    Numeric(NumericKind),
    /// `DecimalStatsUnifier<T>`.
    Decimal {
        physical: NumericKind,
        width: u8,
        scale: u8,
    },
    Varchar,
    Blob,
    Uuid,
    /// `NullStatsUnifier`: no min/max.
    Null,
}

/// The C++ type a numeric unifier loads.
#[derive(Clone, Copy, Debug)]
enum NumericKind {
    I8,
    I32,
    Date,
    I64,
    Time,
    TimeNs,
    Timestamp,
    TimestampTz,
    TimestampMs,
    TimestampNs,
    TimeTz,
    U32,
    U64,
    F32,
    F64,
    /// Big-endian two's complement bytes (`FIXED_LEN_BYTE_ARRAY` decimals).
    BigEndian128,
}

impl NumericKind {
    fn size(self) -> Option<usize> {
        Some(match self {
            Self::I8 => 1,
            Self::I32 | Self::Date | Self::U32 | Self::F32 => 4,
            Self::BigEndian128 => return None,
            _ => 8,
        })
    }

    /// The bytes as a number for comparing.
    fn decode(self, bytes: &[u8]) -> Number {
        let le = |n: usize| {
            let mut buffer = [0u8; 8];
            let len = n.min(bytes.len());
            buffer[..len].copy_from_slice(&bytes[..len]);
            buffer
        };
        match self {
            Self::I8 => Number::Int(i128::from(bytes.first().copied().unwrap_or(0) as i8)),
            Self::I32 | Self::Date => {
                let b = le(4);
                Number::Int(i128::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])))
            }
            Self::U32 => {
                let b = le(4);
                Number::Int(i128::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])))
            }
            Self::U64 => Number::Int(i128::from(u64::from_le_bytes(le(8)))),
            Self::F32 => {
                let b = le(4);
                Number::Float(f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])))
            }
            Self::F64 => Number::Float(f64::from_le_bytes(le(8))),
            Self::BigEndian128 => Number::Int(i128::read(bytes, 0).unwrap_or(0)),
            _ => Number::Int(i128::from(i64::from_le_bytes(le(8)))),
        }
    }

    /// `Value::CreateValue<T>(Load<T>(bytes)).ToString()`.
    fn to_value(self, bytes: &[u8]) -> Value {
        let int = || match self.decode(bytes) {
            Number::Int(value) => value,
            Number::Float(value) => value as i128,
        };
        let float = || match self.decode(bytes) {
            Number::Float(value) => value,
            Number::Int(value) => value as f64,
        };
        match self {
            Self::I8 => Value::tinyint(int() as i8),
            Self::I32 => Value::integer(int() as i32),
            Self::Date => Value::date(Date(int() as i32)),
            Self::I64 => Value::bigint(int() as i64),
            Self::Time => Value::time(Time(int() as i64)),
            Self::TimeNs => Value::time_ns(TimeNs(int() as i64)),
            Self::Timestamp => Value::timestamp(Timestamp(int() as i64)),
            Self::TimestampTz => Value::timestamp_tz(Timestamp(int() as i64)),
            Self::TimestampMs => Value::timestamp_ms(int() as i64),
            Self::TimestampNs => Value::timestamp_ns(int() as i64),
            Self::TimeTz => Value::time_tz(TimeTz(int() as i64 as u64)),
            Self::U32 => Value::uinteger(int() as u32),
            Self::U64 => Value::ubigint(int() as u64),
            Self::F32 => Value::float(float() as f32),
            Self::F64 => Value::double(float()),
            Self::BigEndian128 => Value::hugeint(int()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
enum Number {
    Int(i128),
    Float(f64),
}

/// `GetBaseStatsUnifier`.
fn stats_kind(ty: &LogicalType) -> StatsKind {
    use LogicalTypeId as L;
    use NumericKind as N;
    StatsKind::Numeric(match ty.id() {
        L::Boolean => N::I8,
        L::TinyInt | L::SmallInt | L::Integer | L::UTinyInt | L::USmallInt => N::I32,
        L::Date => N::Date,
        L::BigInt => N::I64,
        L::Time => N::Time,
        L::TimeNs => N::TimeNs,
        L::TimestampSec | L::Timestamp => N::Timestamp,
        L::TimestampTz => N::TimestampTz,
        L::TimestampMs => N::TimestampMs,
        L::TimestampNs => N::TimestampNs,
        L::TimeTz => N::TimeTz,
        L::UInteger => N::U32,
        L::UBigInt => N::U64,
        L::Float => N::F32,
        L::HugeInt | L::UHugeInt | L::Double => N::F64,
        L::Decimal => {
            let (width, scale) = ty.decimal_properties().unwrap_or((18, 3));
            let physical = match ty.physical_type() {
                PhysicalType::Int16 | PhysicalType::Int32 => N::I32,
                PhysicalType::Int64 => N::I64,
                PhysicalType::Int128 => N::BigEndian128,
                _ => return StatsKind::Null,
            };
            return StatsKind::Decimal {
                physical,
                width,
                scale,
            };
        }
        L::Blob => return StatsKind::Blob,
        L::Varchar => return StatsKind::Varchar,
        L::Uuid => return StatsKind::Uuid,
        _ => return StatsKind::Null,
    })
}

/// The statistics of one leaf column accumulated over the row groups
/// (`ColumnStatsUnifier`).
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools, reason = "DuckDB's ColumnStatsUnifier flags")]
struct Unifier {
    column_name: String,
    kind: StatsKind,
    global_min: Option<Vec<u8>>,
    global_max: Option<Vec<u8>>,
    all_min_max_set: bool,
    null_count: u64,
    all_nulls_set: bool,
    can_have_nan: bool,
    has_nan: bool,
    column_size_bytes: u64,
    num_values: u64,
    /// The geometry statistics of a GEOMETRY column (`UnifyGeoStats`).
    geo_stats: Option<crate::geo::GeoStats>,
    /// The layout of a VARIANT, on its `metadata` field.
    variant_type: Option<String>,
}

impl Unifier {
    fn new(column_name: String, kind: StatsKind) -> Self {
        Self {
            column_name,
            kind,
            global_min: None,
            global_max: None,
            all_min_max_set: true,
            null_count: 0,
            all_nulls_set: true,
            can_have_nan: false,
            has_nan: false,
            column_size_bytes: 0,
            num_values: 0,
            geo_stats: None,
            variant_type: None,
        }
    }

    /// Orders two min/max values of the column.
    fn compare(&self, left: &[u8], right: &[u8]) -> Ordering {
        match self.kind {
            StatsKind::Numeric(kind) | StatsKind::Decimal { physical: kind, .. } => kind
                .decode(left)
                .partial_cmp(&kind.decode(right))
                .unwrap_or(Ordering::Equal),
            _ => left.cmp(right),
        }
    }

    /// `UnifyMinMax`.
    fn unify_min_max(&mut self, new_min: &[u8], new_max: &[u8]) {
        if matches!(self.kind, StatsKind::Null) {
            return;
        }
        if let StatsKind::Numeric(kind) | StatsKind::Decimal { physical: kind, .. } = self.kind
            && kind
                .size()
                .is_some_and(|size| new_min.len() != size || new_max.len() != size)
        {
            // "Incorrect size for stats in UnifyMinMax": keep what we have.
            return;
        }
        let replace_min = self
            .global_min
            .as_deref()
            .is_none_or(|min| self.compare(new_min, min) == Ordering::Less);
        if replace_min {
            self.global_min = Some(new_min.to_vec());
        }
        let replace_max = self
            .global_max
            .as_deref()
            .is_none_or(|max| self.compare(new_max, max) == Ordering::Greater);
        if replace_max {
            self.global_max = Some(new_max.to_vec());
        }
    }

    /// `StatsToString`.
    fn stats_to_string(&self, stats: &[u8]) -> String {
        match self.kind {
            StatsKind::Numeric(kind) => {
                if stats.is_empty() {
                    return String::new();
                }
                kind.to_value(stats).to_varchar().unwrap_or_default()
            }
            StatsKind::Decimal {
                physical,
                width,
                scale,
            } => {
                if stats.is_empty() {
                    return String::new();
                }
                let unscaled = match physical.decode(stats) {
                    Number::Int(value) => value,
                    Number::Float(value) => value as i128,
                };
                Value::decimal(unscaled, width, scale)
                    .to_varchar()
                    .unwrap_or_default()
            }
            StatsKind::Varchar => String::from_utf8_lossy(stats).into_owned(),
            StatsKind::Blob => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                stats
                    .iter()
                    .flat_map(|byte| [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0xf)]])
                    .map(char::from)
                    .collect()
            }
            StatsKind::Uuid => {
                if stats.len() != 16 {
                    return String::new();
                }
                let mut result = String::with_capacity(36);
                for (index, byte) in stats.iter().enumerate() {
                    if matches!(index, 4 | 6 | 8 | 10) {
                        result.push('-');
                    }
                    let _ = write!(result, "{byte:02x}");
                }
                result
            }
            StatsKind::Null => String::new(),
        }
    }
}

/// `KeywordHelper::WriteQuoted(name, '"')`.
fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `GetStatsUnifier`: one unifier per leaf, named by the quoted path without the
/// repeated groups.
fn collect_unifiers(writer: &ColumnWriter, mut base_name: String, unifiers: &mut Vec<Unifier>) {
    let schema = writer.schema();
    if schema.repetition != FieldRepetitionType::REPEATED {
        if !base_name.is_empty() {
            base_name.push('.');
        }
        base_name.push_str(&quoted(&schema.name));
    }
    match writer {
        ColumnWriter::Primitive(primitive) => {
            unifiers.push(Unifier::new(
                base_name,
                stats_kind(&primitive.schema.logical_type),
            ));
        }
        ColumnWriter::List(list) => collect_unifiers(&list.child, base_name, unifiers),
        ColumnWriter::Struct(node) => {
            for child in &node.children {
                collect_unifiers(child, base_name.clone(), unifiers);
            }
        }
        ColumnWriter::Variant(node) => {
            if let ColumnWriter::Struct(inner) = &node.inner {
                for child in &inner.children {
                    let start = unifiers.len();
                    collect_unifiers(child, base_name.clone(), unifiers);
                    // The 'metadata' field carries the layout of the VARIANT.
                    if child.schema().name == "metadata"
                        && let Some(unifier) = unifiers.get_mut(start)
                    {
                        unifier.variant_type = Some(node.transformed_type().to_string());
                    }
                }
            }
        }
    }
}

/// `GatherWrittenStatistics` over the finished file.
pub fn gather(
    columns: &[ColumnWriter],
    metadata: &FileMetaData,
    finished: &[Vec<FinishedColumn>],
    file_size_bytes: u64,
    footer_size: u64,
) -> CopyFunctionFileStatistics {
    let mut unifiers = Vec::new();
    for column in columns {
        collect_unifiers(column, String::new(), &mut unifiers);
    }
    // `FlushColumnStats` for every column chunk.
    for (group_index, group) in metadata.row_groups.iter().enumerate() {
        for (leaf, chunk) in group.columns.iter().enumerate() {
            let Some(unifier) = unifiers.get_mut(leaf) else {
                continue;
            };
            let writer_stats = finished
                .get(group_index)
                .and_then(|columns| columns.iter().find(|column| column.column_index == leaf));
            if let Some(stats) = writer_stats {
                unifier.can_have_nan = stats.can_have_nan;
                unifier.has_nan = stats.has_nan;
            }
            let Some(meta) = &chunk.meta_data else {
                continue;
            };
            let Some(statistics) = &meta.statistics else {
                continue;
            };
            if let Some((min, max)) = writer_stats.and_then(|stats| stats.nan_min_max.as_ref()) {
                // NaNs kept the min/max out of the file, but they are known.
                unifier.unify_min_max(min, max);
            } else if let (Some(min), Some(max)) = (&statistics.min_value, &statistics.max_value) {
                unifier.unify_min_max(min, max);
            } else {
                unifier.all_min_max_set = false;
            }
            if let Some((_, _, geo)) = writer_stats.and_then(|stats| stats.geo.as_ref()) {
                match &mut unifier.geo_stats {
                    Some(stats) => stats.merge(geo),
                    None => unifier.geo_stats = Some(*geo),
                }
            }
            match statistics.null_count {
                Some(count) => unifier.null_count += count.max(0) as u64,
                None => unifier.all_nulls_set = false,
            }
            unifier.column_size_bytes += meta.total_compressed_size.max(0) as u64;
            unifier.num_values += meta.num_values.max(0) as u64;
        }
    }
    let column_statistics = unifiers
        .iter()
        .map(|unifier| {
            let mut stats = vec![
                (
                    "column_size_bytes".to_owned(),
                    Value::ubigint(unifier.column_size_bytes),
                ),
                ("num_values".to_owned(), Value::ubigint(unifier.num_values)),
            ];
            if unifier.all_min_max_set {
                if let Some(min) = &unifier.global_min {
                    stats.push(("min".to_owned(), Value::varchar(unifier.stats_to_string(min))));
                }
                if let Some(max) = &unifier.global_max {
                    stats.push(("max".to_owned(), Value::varchar(unifier.stats_to_string(max))));
                }
            }
            if let Some(variant_type) = &unifier.variant_type {
                stats.push(("variant_type".to_owned(), Value::varchar(variant_type)));
            }
            if unifier.all_nulls_set {
                stats.push(("null_count".to_owned(), Value::ubigint(unifier.null_count)));
            }
            if unifier.can_have_nan {
                stats.push(("has_nan".to_owned(), Value::boolean(unifier.has_nan)));
            }
            if let Some(geo) = &unifier.geo_stats {
                let extent = &geo.extent;
                if crate::geo::has_xy(extent) {
                    stats.push(("bbox_xmin".to_owned(), Value::double(extent.x_min)));
                    stats.push(("bbox_xmax".to_owned(), Value::double(extent.x_max)));
                    stats.push(("bbox_ymin".to_owned(), Value::double(extent.y_min)));
                    stats.push(("bbox_ymax".to_owned(), Value::double(extent.y_max)));
                    if crate::geo::has_z(extent) {
                        stats.push(("bbox_zmin".to_owned(), Value::double(extent.z_min)));
                        stats.push(("bbox_zmax".to_owned(), Value::double(extent.z_max)));
                    }
                    if crate::geo::has_m(extent) {
                        stats.push(("bbox_mmin".to_owned(), Value::double(extent.m_min)));
                        stats.push(("bbox_mmax".to_owned(), Value::double(extent.m_max)));
                    }
                }
                if !geo.types.is_empty() {
                    let types = geo
                        .types
                        .to_strings(true)
                        .into_iter()
                        .map(Value::varchar)
                        .collect();
                    stats.push(("geo_types".to_owned(), Value::list(LogicalType::VARCHAR, types)));
                }
            }
            (unifier.column_name.clone(), stats)
        })
        .collect();
    CopyFunctionFileStatistics {
        row_count: metadata.num_rows.max(0) as u64,
        file_size_bytes,
        footer_size_bytes: Value::ubigint(footer_size),
        column_statistics,
    }
}
