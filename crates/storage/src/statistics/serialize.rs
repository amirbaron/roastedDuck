//! Serialization of statistics (DuckDB's `BaseStatistics::Serialize/Deserialize`,
//! `NumericStats`, `StringStats`, `ListStats`, `StructStats`, `ArrayStats`, and the
//! `ColumnStatistics` / `TableStatistics` wrappers). Deserialization needs the type,
//! which DuckDB passes through the deserializer context.

use roastedduck_common::{Error, Result};
use roastedduck_serde::{Deserializer, Serializer};
use roastedduck_types::{LogicalType, PhysicalType};

use super::numeric::NumericValue;
use super::string::MAX_STRING_MINMAX_SIZE;
use super::{BaseStatistics, DistinctStatistics, GeometryStats, HyperLogLog, StatsData, StringStats};

/// `SerializeNumericStatsValue`.
fn write_numeric_value(serializer: &mut dyn Serializer, value: Option<NumericValue>) -> Result<()> {
    serializer.write_property(100, "has_value", &value.is_some())?;
    let Some(value) = value else {
        return Ok(());
    };
    serializer.on_property_begin(101, "value");
    match value {
        NumericValue::Bool(v) => serializer.write_bool(v),
        NumericValue::I8(v) => serializer.write_i8(v),
        NumericValue::I16(v) => serializer.write_i16(v),
        NumericValue::I32(v) => serializer.write_i32(v),
        NumericValue::I64(v) => serializer.write_i64(v),
        NumericValue::I128(v) => serializer.write_i128(v),
        NumericValue::U8(v) => serializer.write_u8(v),
        NumericValue::U16(v) => serializer.write_u16(v),
        NumericValue::U32(v) => serializer.write_u32(v),
        NumericValue::U64(v) => serializer.write_u64(v),
        NumericValue::U128(v) => serializer.write_u128(v),
        NumericValue::F32(v) => serializer.write_f32(v),
        NumericValue::F64(v) => serializer.write_f64(v),
    }
    serializer.on_property_end();
    Ok(())
}

/// `DeserializeNumericStatsValue`.
fn read_numeric_value(deserializer: &mut dyn Deserializer, ty: &LogicalType) -> Result<Option<NumericValue>> {
    let has_value: bool = deserializer.read_property(100, "has_value")?;
    if !has_value {
        return Ok(None);
    }
    deserializer.on_property_begin(101, "value")?;
    let value = match ty.physical_type() {
        PhysicalType::Bool => NumericValue::Bool(deserializer.read_bool()?),
        PhysicalType::Int8 => NumericValue::I8(deserializer.read_i8()?),
        PhysicalType::Int16 => NumericValue::I16(deserializer.read_i16()?),
        PhysicalType::Int32 => NumericValue::I32(deserializer.read_i32()?),
        PhysicalType::Int64 => NumericValue::I64(deserializer.read_i64()?),
        PhysicalType::Int128 => NumericValue::I128(deserializer.read_i128()?),
        PhysicalType::UInt8 => NumericValue::U8(deserializer.read_u8()?),
        PhysicalType::UInt16 => NumericValue::U16(deserializer.read_u16()?),
        PhysicalType::UInt32 => NumericValue::U32(deserializer.read_u32()?),
        PhysicalType::UInt64 => NumericValue::U64(deserializer.read_u64()?),
        PhysicalType::UInt128 => NumericValue::U128(deserializer.read_u128()?),
        PhysicalType::Float => NumericValue::F32(deserializer.read_f32()?),
        PhysicalType::Double => NumericValue::F64(deserializer.read_f64()?),
        other => {
            return Err(Error::internal(format!(
                "Unsupported type for deserializing numeric statistics: {other:?}"
            )));
        }
    };
    deserializer.on_property_end()?;
    Ok(Some(value))
}

/// `StringStats::Serialize`.
fn write_string_stats(serializer: &mut dyn Serializer, string: &StringStats) -> Result<()> {
    serializer.on_property_begin(200, "min");
    serializer.write_data(&string.min);
    serializer.on_property_end();
    serializer.on_property_begin(201, "max");
    serializer.write_data(&string.max);
    serializer.on_property_end();
    serializer.write_property(202, "has_unicode", &string.has_unicode)?;
    serializer.write_property(203, "has_max_string_length", &string.has_max_string_length)?;
    serializer.write_property(204, "max_string_length", &string.max_string_length)
}

/// `StringStats::Deserialize`.
fn read_string_stats(deserializer: &mut dyn Deserializer, string: &mut StringStats) -> Result<()> {
    deserializer.on_property_begin(200, "min")?;
    string
        .min
        .copy_from_slice(&deserializer.read_data(MAX_STRING_MINMAX_SIZE)?);
    deserializer.on_property_end()?;
    deserializer.on_property_begin(201, "max")?;
    string
        .max
        .copy_from_slice(&deserializer.read_data(MAX_STRING_MINMAX_SIZE)?);
    deserializer.on_property_end()?;
    string.has_unicode = deserializer.read_property(202, "has_unicode")?;
    string.has_max_string_length = deserializer.read_property(203, "has_max_string_length")?;
    string.max_string_length = deserializer.read_property(204, "max_string_length")?;
    Ok(())
}

/// The version that added GEOMETRY to the core (`Geometry::VERSION_ADDED`).
const GEOMETRY_VERSION_ADDED: u64 = 7;

/// `GeometryStats::Serialize`: older storage versions get unknown string statistics, as
/// the spatial extension's GEOMETRY wrote.
fn write_geometry_stats(serializer: &mut dyn Serializer, geometry: &GeometryStats) -> Result<()> {
    if !serializer.should_serialize(GEOMETRY_VERSION_ADDED) {
        return write_string_stats(serializer, &StringStats::unknown());
    }
    let extent = &geometry.extent;
    serializer.write_object_property(300, "extent", |s| {
        s.write_property(101, "x_min", &extent.x_min)?;
        s.write_property(102, "x_max", &extent.x_max)?;
        s.write_property(103, "y_min", &extent.y_min)?;
        s.write_property(104, "y_max", &extent.y_max)?;
        s.write_property(105, "z_min", &extent.z_min)?;
        s.write_property(106, "z_max", &extent.z_max)?;
        s.write_property(107, "m_min", &extent.m_min)?;
        s.write_property(108, "m_max", &extent.m_max)
    })?;
    let sets = &geometry.types.sets;
    serializer.write_object_property(301, "types", |s| {
        s.write_property(101, "types_xy", &sets[0])?;
        s.write_property(102, "types_xyz", &sets[1])?;
        s.write_property(103, "types_xym", &sets[2])?;
        s.write_property(104, "types_xyzm", &sets[3])
    })?;
    serializer.write_property_with_default(302, "flags", &geometry.flags.flags)
}

/// `GeometryStats::Deserialize`: the string statistics of the spatial extension's
/// GEOMETRY are read and ignored, leaving unknown geometry statistics.
fn read_geometry_stats(deserializer: &mut dyn Deserializer, geometry: &mut GeometryStats) -> Result<()> {
    if deserializer.can_deserialize_property(200, "min")? {
        let mut ignored = StringStats::empty();
        read_string_stats(deserializer, &mut ignored)?;
        *geometry = GeometryStats::unknown();
        return Ok(());
    }
    let extent = &mut geometry.extent;
    deserializer.read_object_property(300, "extent", |d| {
        extent.x_min = d.read_property(101, "x_min")?;
        extent.x_max = d.read_property(102, "x_max")?;
        extent.y_min = d.read_property(103, "y_min")?;
        extent.y_max = d.read_property(104, "y_max")?;
        extent.z_min = d.read_property(105, "z_min")?;
        extent.z_max = d.read_property(106, "z_max")?;
        extent.m_min = d.read_property(107, "m_min")?;
        extent.m_max = d.read_property(108, "m_max")?;
        Ok(())
    })?;
    let sets = &mut geometry.types.sets;
    deserializer.read_object_property(301, "types", |d| {
        sets[0] = d.read_property(101, "types_xy")?;
        sets[1] = d.read_property(102, "types_xyz")?;
        sets[2] = d.read_property(103, "types_xym")?;
        sets[3] = d.read_property(104, "types_xyzm")?;
        Ok(())
    })?;
    geometry.flags.flags = deserializer.read_property_with_default(302, "flags")?;
    Ok(())
}

impl BaseStatistics {
    /// Writes the properties of these statistics (`BaseStatistics::Serialize`).
    pub fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_property(100, "has_null", &self.has_null)?;
        serializer.write_property(101, "has_no_null", &self.has_no_null)?;
        serializer.write_property(102, "distinct_count", &self.distinct_count)?;
        serializer.write_object_property(103, "type_stats", |serializer| match &self.data {
            StatsData::Numeric(numeric) => {
                // DuckDB writes the minimum under the tag "max" and vice versa.
                serializer.write_object_property(200, "max", |s| write_numeric_value(s, numeric.min))?;
                serializer.write_object_property(201, "min", |s| write_numeric_value(s, numeric.max))
            }
            StatsData::String(string) => write_string_stats(serializer, string),
            StatsData::Geometry(geometry) => write_geometry_stats(serializer, geometry),
            StatsData::List(child) | StatsData::Array(child) => {
                serializer.write_object_property(200, "child_stats", |s| child.serialize_fields(s))
            }
            StatsData::Struct(children) => {
                serializer.write_list_property(200, "child_stats", children.len(), |s, i| {
                    s.on_object_begin();
                    children[i].serialize_fields(s)?;
                    s.on_object_end();
                    Ok(())
                })
            }
            StatsData::Variant(variant) => super::variant::write_variant_stats(serializer, variant),
            StatsData::Base => Ok(()),
        })
    }

    /// Reads statistics of type `ty` (`BaseStatistics::Deserialize`).
    pub fn deserialize_fields(deserializer: &mut dyn Deserializer, ty: &LogicalType) -> Result<Self> {
        let has_null: bool = deserializer.read_property(100, "has_null")?;
        let has_no_null: bool = deserializer.read_property(101, "has_no_null")?;
        let distinct_count: u64 = deserializer.read_property(102, "distinct_count")?;
        let mut stats = Self::create_empty(ty.clone());
        stats.has_null = has_null;
        stats.has_no_null = has_no_null;
        stats.distinct_count = distinct_count;
        deserializer.read_object_property(103, "type_stats", |deserializer| {
            match &mut stats.data {
                StatsData::Numeric(numeric) => {
                    numeric.min =
                        deserializer.read_object_property(200, "max", |d| read_numeric_value(d, ty))?;
                    numeric.max =
                        deserializer.read_object_property(201, "min", |d| read_numeric_value(d, ty))?;
                }
                StatsData::String(string) => read_string_stats(deserializer, string)?,
                StatsData::Geometry(geometry) => read_geometry_stats(deserializer, geometry)?,
                StatsData::List(child) | StatsData::Array(child) => {
                    let child_type = child.logical_type().clone();
                    **child = deserializer.read_object_property(200, "child_stats", |d| {
                        Self::deserialize_fields(d, &child_type)
                    })?;
                }
                StatsData::Struct(children) => {
                    let types: Vec<LogicalType> = children.iter().map(|c| c.logical_type().clone()).collect();
                    deserializer.read_list_property(200, "child_stats", |d, i| {
                        let child_type = types
                            .get(i)
                            .ok_or_else(|| Error::serialization("too many struct statistics"))?;
                        d.on_object_begin()?;
                        let child = Self::deserialize_fields(d, child_type)?;
                        d.on_object_end()?;
                        children[i] = child;
                        Ok(())
                    })?;
                }
                StatsData::Variant(variant) => super::variant::read_variant_stats(deserializer, variant)?,
                StatsData::Base => {}
            }
            Ok(())
        })?;
        Ok(stats)
    }
}

/// `DistinctStatistics::Serialize` (with `HyperLogLog::Serialize`), as the optional
/// "distinct" property of a `ColumnStatistics`.
fn write_distinct(s: &mut dyn Serializer, distinct: Option<&DistinctStatistics>) -> Result<()> {
    let Some(distinct) = distinct else {
        s.on_optional_property_begin(101, "distinct", false);
        s.on_optional_property_end(false);
        return Ok(());
    };
    s.on_optional_property_begin(101, "distinct", true);
    s.on_nullable_begin(true);
    s.on_object_begin();
    s.write_property_with_default(100, "sample_count", &distinct.sample_count)?;
    s.write_property_with_default(101, "total_count", &distinct.total_count)?;
    s.on_optional_property_begin(102, "log", true);
    s.on_nullable_begin(true);
    s.on_object_begin();
    let (storage_type, data) = distinct.log.to_storage(s.should_serialize(3));
    s.write_property(100, "type", &storage_type)?;
    s.on_property_begin(101, "data");
    s.write_data(&data);
    s.on_property_end();
    s.on_object_end();
    s.on_nullable_end();
    s.on_optional_property_end(true);
    s.on_object_end();
    s.on_nullable_end();
    s.on_optional_property_end(true);
    Ok(())
}

/// `DistinctStatistics::Deserialize`, from the "distinct" property; `None` if absent or
/// unreadable.
fn read_distinct(d: &mut dyn Deserializer) -> Result<Option<DistinctStatistics>> {
    if !d.on_optional_property_begin(101, "distinct")? {
        return Ok(None);
    }
    let mut result = None;
    if d.on_nullable_begin()? {
        d.on_object_begin()?;
        let sample_count: Option<u64> = d.read_optional(100, "sample_count")?;
        let total_count: Option<u64> = d.read_optional(101, "total_count")?;
        let mut log = None;
        if d.on_optional_property_begin(102, "log")? {
            if d.on_nullable_begin()? {
                d.on_object_begin()?;
                let storage_type: u8 = d.read_property(100, "type")?;
                let len = HyperLogLog::storage_size(storage_type)
                    .ok_or_else(|| Error::serialization("Unknown HyperLogLog storage type!"))?;
                d.on_property_begin(101, "data")?;
                let data = d.read_data(len)?;
                d.on_property_end()?;
                d.on_object_end()?;
                log = HyperLogLog::from_storage(storage_type, &data);
            }
            d.on_nullable_end()?;
            d.on_optional_property_end(true)?;
        }
        d.on_object_end()?;
        result = log.map(|log| DistinctStatistics {
            log,
            sample_count: sample_count.unwrap_or(0),
            total_count: total_count.unwrap_or(0),
        });
    }
    d.on_nullable_end()?;
    d.on_optional_property_end(true)?;
    Ok(result)
}

/// `TableStatistics::Serialize`: per column a nullable `ColumnStatistics` object holding
/// the statistics and the distinct statistics, and no sample.
pub fn serialize_table_statistics(
    serializer: &mut dyn Serializer,
    columns: &[BaseStatistics],
    distinct: &[Option<DistinctStatistics>],
) -> Result<()> {
    serializer.write_list_property(100, "column_stats", columns.len(), |s, i| {
        s.on_nullable_begin(true);
        s.on_object_begin();
        s.write_object_property(100, "statistics", |s| columns[i].serialize_fields(s))?;
        write_distinct(s, distinct.get(i).and_then(Option::as_ref))?;
        s.on_object_end();
        s.on_nullable_end();
        Ok(())
    })?;
    serializer.on_optional_property_begin(101, "table_sample", false);
    serializer.on_optional_property_end(false);
    Ok(())
}

/// `TableStatistics::Deserialize` for columns of `types`: the statistics and distinct
/// statistics of each column.
pub fn deserialize_table_statistics(
    d: &mut dyn Deserializer,
    types: &[LogicalType],
) -> Result<(Vec<BaseStatistics>, Vec<Option<DistinctStatistics>>)> {
    let mut result = Vec::with_capacity(types.len());
    let mut distinct = Vec::with_capacity(types.len());
    d.on_property_begin(100, "column_stats")?;
    let count = d.on_list_begin()?;
    for index in 0..count {
        let ty = types
            .get(index)
            .ok_or_else(|| Error::serialization("more column statistics than columns"))?;
        if !d.on_nullable_begin()? {
            result.push(BaseStatistics::create_empty(ty.clone()));
            distinct.push(None);
            d.on_nullable_end()?;
            continue;
        }
        d.on_object_begin()?;
        let stats =
            d.read_object_property(100, "statistics", |d| BaseStatistics::deserialize_fields(d, ty))?;
        distinct.push(read_distinct(d)?);
        d.on_object_end()?;
        d.on_nullable_end()?;
        result.push(stats);
    }
    d.on_list_end()?;
    d.on_property_end()?;
    if d.on_optional_property_begin(101, "table_sample")? {
        skip_table_sample(d)?;
        d.on_optional_property_end(true)?;
    }
    Ok((result, distinct))
}

/// Reads and drops a `unique_ptr<BlockingSample>` (`BlockingSample::Deserialize`): table
/// samples are not kept.
fn skip_table_sample(d: &mut dyn Deserializer) -> Result<()> {
    if !d.on_nullable_begin()? {
        return d.on_nullable_end();
    }
    d.on_object_begin()?;
    if d.on_optional_property_begin(100, "base_reservoir_sample")? {
        if d.on_nullable_begin()? {
            d.on_object_begin()?;
            let _: Option<u64> = d.read_optional(100, "next_index_to_sample")?;
            let _: f64 = d.read_property(101, "min_weight_threshold")?;
            let _: Option<u64> = d.read_optional(102, "min_weighted_entry_index")?;
            let _: Option<u64> = d.read_optional(103, "num_entries_to_skip_b4_next_sample")?;
            let _: Option<u64> = d.read_optional(104, "num_entries_seen_total")?;
            if d.on_optional_property_begin(105, "reservoir_weights")? {
                d.read_list(|d, _| {
                    d.on_object_begin()?;
                    let _: f64 = d.read_property(0, "first")?;
                    let _: u64 = d.read_property(1, "second")?;
                    d.on_object_end()
                })?;
                d.on_optional_property_end(true)?;
            }
            d.on_object_end()?;
        }
        d.on_nullable_end()?;
        d.on_optional_property_end(true)?;
    }
    // `SampleType`: RESERVOIR_SAMPLE = 1, RESERVOIR_PERCENTAGE_SAMPLE = 2.
    let sample_type: u8 = d.read_property(101, "type")?;
    let _: Option<bool> = d.read_optional(102, "destroyed")?;
    match sample_type {
        2 => {
            let _: f64 = d.read_property(200, "sample_percentage")?;
            let _: Option<u64> = d.read_optional(201, "reservoir_sample_size")?;
        }
        1 => {
            let _: Option<u64> = d.read_optional(200, "sample_count")?;
            if d.on_optional_property_begin(201, "reservoir_chunk")? {
                if d.on_nullable_begin()? {
                    d.on_object_begin()?;
                    crate::checkpoint::vector_serde::deserialize_chunk(d)?;
                    d.on_object_end()?;
                }
                d.on_nullable_end()?;
                d.on_optional_property_end(true)?;
            }
        }
        _ => {
            return Err(Error::serialization(
                "Unsupported type for deserialization of BlockingSample!",
            ));
        }
    }
    d.on_object_end()?;
    d.on_nullable_end()
}
