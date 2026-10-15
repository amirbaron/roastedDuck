//! Column statistics (DuckDB's `storage/statistics`): NULL-ness, numeric min/max, string
//! prefixes and nested children. Segments keep them as zone maps for filter pruning; the
//! optimizer reads them through `DataTable::column_statistics`.

mod numeric;
mod serialize;
mod string;

pub use numeric::{NumericStats, NumericValue, StatsNative, maximum_value, minimum_value};
pub use serialize::{deserialize_table_statistics, serialize_table_statistics};
pub use string::{MAX_STRING_MINMAX_SIZE, StringStats};

use std::fmt;

use rustdb_types::{LogicalType, LogicalTypeId, PhysicalType, Value};

/// What a filter evaluates to on a set of values, judged from statistics
/// (DuckDB's `FilterPropagateResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FilterPropagateResult {
    NoPruningPossible,
    FilterAlwaysTrue,
    FilterAlwaysFalse,
    FilterTrueOrNull,
    FilterFalseOrNull,
}

/// The kind of type-specific statistics (DuckDB's `StatisticsType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatisticsType {
    Numeric,
    String,
    List,
    Struct,
    Base,
    Array,
    Geometry,
    Variant,
}

impl StatisticsType {
    /// The statistics kind used for values of `ty` (`BaseStatistics::GetStatsType`).
    pub fn of(ty: &LogicalType) -> Self {
        match ty.id() {
            LogicalTypeId::SqlNull => return Self::Base,
            LogicalTypeId::Geometry => return Self::Geometry,
            LogicalTypeId::Variant => return Self::Variant,
            _ => {}
        }
        match ty.physical_type() {
            PhysicalType::Bool
            | PhysicalType::Int8
            | PhysicalType::Int16
            | PhysicalType::Int32
            | PhysicalType::Int64
            | PhysicalType::UInt8
            | PhysicalType::UInt16
            | PhysicalType::UInt32
            | PhysicalType::UInt64
            | PhysicalType::Int128
            | PhysicalType::UInt128
            | PhysicalType::Float
            | PhysicalType::Double => Self::Numeric,
            PhysicalType::Varchar => Self::String,
            PhysicalType::Struct => Self::Struct,
            PhysicalType::List => Self::List,
            PhysicalType::Array => Self::Array,
            _ => Self::Base,
        }
    }
}

/// Type-specific part of [`BaseStatistics`].
#[derive(Clone, Debug, PartialEq)]
pub enum StatsData {
    /// No type-specific statistics (validity, interval, bit, ...).
    Base,
    Numeric(NumericStats),
    String(StringStats),
    /// The statistics of all list elements.
    List(Box<BaseStatistics>),
    /// One entry per struct field.
    Struct(Vec<BaseStatistics>),
    /// The statistics of all array elements.
    Array(Box<BaseStatistics>),
}

/// Statistics of a set of values of one type (DuckDB's `BaseStatistics`).
#[derive(Clone, Debug, PartialEq)]
pub struct BaseStatistics {
    ty: LogicalType,
    has_null: bool,
    has_no_null: bool,
    distinct_count: u64,
    data: StatsData,
}

/// Initial state of new statistics.
#[derive(Clone, Copy)]
enum Init {
    Empty,
    Unknown,
}

fn construct(ty: &LogicalType, init: Init) -> StatsData {
    match StatisticsType::of(ty) {
        StatisticsType::Numeric => StatsData::Numeric(match init {
            Init::Empty => NumericStats::empty(ty),
            Init::Unknown => NumericStats::unknown(),
        }),
        StatisticsType::String | StatisticsType::Geometry => StatsData::String(match init {
            Init::Empty => StringStats::empty(),
            Init::Unknown => StringStats::unknown(),
        }),
        StatisticsType::List => {
            let child = ty.child_type().cloned().unwrap_or_default();
            StatsData::List(Box::new(BaseStatistics::create(child, init)))
        }
        StatisticsType::Array => {
            let child = ty
                .array_child()
                .map(|(child, _)| child.clone())
                .unwrap_or_default();
            StatsData::Array(Box::new(BaseStatistics::create(child, init)))
        }
        StatisticsType::Struct => StatsData::Struct(
            ty.struct_children()
                .unwrap_or_default()
                .iter()
                .map(|(_, child)| BaseStatistics::create(child.clone(), init))
                .collect(),
        ),
        StatisticsType::Base | StatisticsType::Variant => StatsData::Base,
    }
}

/// A value of the statistics as DuckDB prints it (`Value::ToString`).
fn value_text(value: Option<Value>) -> String {
    match value {
        Some(value) if !value.is_null() => value.to_varchar().unwrap_or_default(),
        _ => "NULL".to_owned(),
    }
}

/// The bytes of a string statistics bound up to the first zero (`Blob::ToString` of
/// `GetValidMinMaxSubstring`).
fn bound_text(bytes: &[u8]) -> String {
    let len = bytes.iter().position(|&byte| byte == 0).unwrap_or(bytes.len());
    let mut result = String::new();
    for &byte in &bytes[..len] {
        if (32..=126).contains(&byte) && !matches!(byte, b'\\' | b'\'' | b'"') {
            result.push(char::from(byte));
        } else {
            let _ = fmt::Write::write_fmt(&mut result, format_args!("\\x{byte:02X}"));
        }
    }
    result
}

impl fmt::Display for BaseStatistics {
    /// DuckDB's `BaseStatistics::ToString`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.data {
            StatsData::Base => {}
            StatsData::Numeric(numeric) => write!(
                f,
                "[Min: {}, Max: {}]",
                value_text(numeric.min.map(|min| min.to_value(&self.ty))),
                value_text(numeric.max.map(|max| max.to_value(&self.ty)))
            )?,
            StatsData::String(string) => write!(
                f,
                "[Min: {}, Max: {}, Has Unicode: {}, Max String Length: {}]",
                bound_text(&string.min),
                bound_text(&string.max),
                string.has_unicode,
                if string.has_max_string_length {
                    string.max_string_length.to_string()
                } else {
                    "?".to_owned()
                }
            )?,
            StatsData::List(child) | StatsData::Array(child) => write!(f, "[{child}]")?,
            StatsData::Struct(children) => {
                f.write_str(" {")?;
                let names = self.ty.struct_children().unwrap_or_default();
                for (index, child) in children.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    let name = names.get(index).map_or("", |(name, _)| name.as_str());
                    write!(f, "{name}: {child}")?;
                }
                f.write_str("}")?;
            }
        }
        write!(
            f,
            "[Has Null: {}, Has No Null: {}]",
            self.has_null, self.has_no_null
        )?;
        if self.distinct_count > 0 {
            write!(f, "[Approx Unique: {}]", self.distinct_count)?;
        }
        Ok(())
    }
}

impl BaseStatistics {
    fn create(ty: LogicalType, init: Init) -> Self {
        let data = construct(&ty, init);
        let unknown = matches!(init, Init::Unknown);
        Self {
            ty,
            has_null: unknown,
            has_no_null: unknown,
            distinct_count: 0,
            data,
        }
    }

    /// Statistics of an empty set: no NULLs, no values, empty ranges (`CreateEmpty`).
    pub fn create_empty(ty: LogicalType) -> Self {
        Self::create(ty, Init::Empty)
    }

    /// Statistics that admit any value (`CreateUnknown`).
    pub fn create_unknown(ty: LogicalType) -> Self {
        Self::create(ty, Init::Unknown)
    }

    /// Merges only the NULL flags of `other` (the statistics of a validity column).
    pub fn merge_validity(&mut self, other: &Self) {
        self.has_null |= other.has_null;
        self.has_no_null |= other.has_no_null;
    }

    /// Statistics of a single constant value (`BaseStatistics::FromConstant`).
    pub fn from_constant(value: &Value) -> Self {
        let mut stats = Self::create_empty(value.logical_type().clone());
        stats.update_from_value(value);
        stats.distinct_count = 1;
        if value.is_null() {
            stats.has_null = true;
            stats.has_no_null = false;
        } else {
            stats.has_null = false;
            stats.has_no_null = true;
        }
        stats
    }

    fn update_from_value(&mut self, value: &Value) {
        match &mut self.data {
            StatsData::Numeric(numeric) => {
                let bound = NumericValue::from_value(value);
                numeric.min = bound;
                numeric.max = bound;
            }
            StatsData::String(string) => {
                if let Some(bytes) = value.as_bytes() {
                    string.update(bytes, value.logical_type().id() == LogicalTypeId::Varchar);
                }
            }
            StatsData::List(child) | StatsData::Array(child) => {
                for element in value.children().unwrap_or_default() {
                    child.merge(&Self::from_constant(element));
                }
            }
            StatsData::Struct(children) => {
                let fields = value.children().unwrap_or_default();
                for (index, child) in children.iter_mut().enumerate() {
                    *child = match fields.get(index) {
                        Some(field) if !value.is_null() => Self::from_constant(field),
                        _ => Self::from_constant(&Value::null(child.ty.clone())),
                    };
                }
            }
            StatsData::Base => {}
        }
    }

    pub fn logical_type(&self) -> &LogicalType {
        &self.ty
    }

    pub fn stats_type(&self) -> StatisticsType {
        StatisticsType::of(&self.ty)
    }

    pub fn data(&self) -> &StatsData {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut StatsData {
        &mut self.data
    }

    /// Whether the values may contain NULLs.
    pub fn can_have_null(&self) -> bool {
        self.has_null
    }

    /// Whether the values may contain non-NULL values.
    pub fn can_have_no_null(&self) -> bool {
        self.has_no_null
    }

    /// Marks that this level contains NULLs (`SetHasNullFast`).
    #[inline]
    pub fn set_has_null_fast(&mut self) {
        self.has_null = true;
    }

    /// Marks that this level contains non-NULL values (`SetHasNoNullFast`).
    #[inline]
    pub fn set_has_no_null_fast(&mut self) {
        self.has_no_null = true;
    }

    /// Marks that the values may contain NULLs, including struct fields (`SetHasNull`).
    pub fn set_has_null(&mut self) {
        self.has_null = true;
        if let StatsData::Struct(children) = &mut self.data {
            children.iter_mut().for_each(Self::set_has_null);
        }
    }

    /// Marks that the values may contain non-NULLs, including struct fields.
    pub fn set_has_no_null(&mut self) {
        self.has_no_null = true;
        if let StatsData::Struct(children) = &mut self.data {
            children.iter_mut().for_each(Self::set_has_no_null);
        }
    }

    /// Overrides both NULL flags.
    pub fn set_validity(&mut self, has_null: bool, has_no_null: bool) {
        self.has_null = has_null;
        self.has_no_null = has_no_null;
    }

    pub fn distinct_count(&self) -> u64 {
        self.distinct_count
    }

    pub fn set_distinct_count(&mut self, count: u64) {
        self.distinct_count = count;
    }

    /// Whether every value is the same (`IsConstant`).
    pub fn is_constant(&self) -> bool {
        if self.ty.id() == LogicalTypeId::Validity {
            return self.has_null != self.has_no_null;
        }
        match &self.data {
            StatsData::Numeric(numeric) => numeric.is_constant(),
            _ => false,
        }
    }

    /// Merges `other` (statistics of more values of the same type) into this.
    pub fn merge(&mut self, other: &Self) {
        self.has_null |= other.has_null;
        self.has_no_null |= other.has_no_null;
        if other.ty.id() == LogicalTypeId::Validity || other.ty.id() == LogicalTypeId::SqlNull {
            return;
        }
        match (&mut self.data, &other.data) {
            (StatsData::Numeric(own), StatsData::Numeric(theirs)) => own.merge(theirs),
            (StatsData::String(own), StatsData::String(theirs)) => own.merge(theirs),
            (StatsData::List(own), StatsData::List(theirs))
            | (StatsData::Array(own), StatsData::Array(theirs)) => own.merge(theirs),
            (StatsData::Struct(own), StatsData::Struct(theirs)) => {
                for (own, theirs) in own.iter_mut().zip(theirs) {
                    own.merge(theirs);
                }
            }
            _ => {}
        }
    }

    /// Numeric min/max, for numeric statistics.
    pub fn numeric(&self) -> Option<&NumericStats> {
        match &self.data {
            StatsData::Numeric(numeric) => Some(numeric),
            _ => None,
        }
    }

    pub fn numeric_mut(&mut self) -> Option<&mut NumericStats> {
        match &mut self.data {
            StatsData::Numeric(numeric) => Some(numeric),
            _ => None,
        }
    }

    /// String prefixes and lengths, for string statistics.
    pub fn string(&self) -> Option<&StringStats> {
        match &self.data {
            StatsData::String(string) => Some(string),
            _ => None,
        }
    }

    pub fn string_mut(&mut self) -> Option<&mut StringStats> {
        match &mut self.data {
            StatsData::String(string) => Some(string),
            _ => None,
        }
    }

    /// The element statistics of a LIST or ARRAY.
    pub fn child(&self) -> Option<&Self> {
        match &self.data {
            StatsData::List(child) | StatsData::Array(child) => Some(child),
            _ => None,
        }
    }

    pub fn child_mut(&mut self) -> Option<&mut Self> {
        match &mut self.data {
            StatsData::List(child) | StatsData::Array(child) => Some(child),
            _ => None,
        }
    }

    /// The field statistics of a STRUCT.
    pub fn struct_children(&self) -> Option<&[Self]> {
        match &self.data {
            StatsData::Struct(children) => Some(children),
            _ => None,
        }
    }

    pub fn struct_children_mut(&mut self) -> Option<&mut [Self]> {
        match &mut self.data {
            StatsData::Struct(children) => Some(children),
            _ => None,
        }
    }

    /// The minimum as a value of the column type, if known (`NumericStats::Min`).
    pub fn numeric_min(&self) -> Option<Value> {
        self.numeric()?.min.map(|min| min.to_value(&self.ty))
    }

    /// The maximum as a value of the column type, if known (`NumericStats::Max`).
    pub fn numeric_max(&self) -> Option<Value> {
        self.numeric()?.max.map(|max| max.to_value(&self.ty))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_statistics_merge_children() {
        let ty = LogicalType::struct_type(vec![
            ("a".into(), LogicalType::INTEGER),
            ("b".into(), LogicalType::VARCHAR),
        ]);
        let mut stats = BaseStatistics::create_empty(ty);
        let value = Value::struct_value(vec![
            ("a".into(), Value::integer(4)),
            ("b".into(), Value::varchar("x")),
        ]);
        stats.merge(&BaseStatistics::from_constant(&value));
        let children = stats.struct_children().unwrap();
        assert_eq!(children[0].numeric_min(), Some(Value::integer(4)));
        assert_eq!(children[1].string().unwrap().min_bytes(), b"x");
        assert!(stats.can_have_no_null());
        assert!(!stats.can_have_null());
    }

    #[test]
    fn validity_statistics_are_constant_when_uniform() {
        let mut stats = BaseStatistics::create_empty(LogicalType::simple(LogicalTypeId::Validity));
        stats.set_has_no_null_fast();
        assert!(stats.is_constant());
        stats.set_has_null_fast();
        assert!(!stats.is_constant());
    }
}
