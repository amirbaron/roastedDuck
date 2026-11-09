//! Column chunk statistics as DuckDB values and statistics (DuckDB's
//! `ParquetStatisticsUtils`): `parquet_metadata`'s min/max columns and the zone maps that
//! prune row groups.

use roastedduck_common::{Error, Result};
use roastedduck_function::cast::{CastFunctionSet, ValueCast as _};
use roastedduck_storage::statistics::{BaseStatistics, NumericValue, StatsData, wrap_shredded_levels};
use roastedduck_types::{
    Date, LogicalType, LogicalTypeId, PhysicalType, Time, TimeNs, TimeTz, Timestamp, Value,
};

use crate::column_reader::values::{DecimalTarget, FixedConversion, float16_to_float32};
use crate::column_reader::values::{
    ImpalaToNs, ImpalaToUs, TimeTzFromMs, TimeTzFromNs, TimeTzFromUs, TimestampMsToNs, TimestampMsToUs,
    TimestampNsToUs, TimestampUsToNs, blob_text,
};
use crate::schema::{ColumnSchema, ExtraTypeInfo, SchemaKind};
use crate::thrift::{ColumnChunk, Statistics};

fn size_error(what: &str) -> Error {
    Error::invalid_input(format!("Incorrect stats size for type {what}"))
}

fn fixed<const N: usize>(bytes: &[u8], what: &str) -> Result<[u8; N]> {
    bytes.try_into().map_err(|_| size_error(what))
}

/// A 4- or 8-byte integer (the TIME statistics accept both).
fn int_of_either(bytes: &[u8], what: &str) -> Result<i64> {
    match bytes.len() {
        4 => Ok(i64::from(i32::from_le_bytes(fixed(bytes, what)?))),
        8 => Ok(i64::from_le_bytes(fixed(bytes, what)?)),
        _ => Err(size_error(what)),
    }
}

/// `ParquetStatisticsUtils::ConvertValueInternal`.
pub fn convert_value_internal(ty: &LogicalType, schema: &ColumnSchema, bytes: &[u8]) -> Result<Value> {
    use LogicalTypeId as L;
    Ok(match ty.id() {
        L::Boolean => {
            let [byte] = fixed::<1>(bytes, "BOOLEAN")?;
            Value::boolean(byte != 0)
        }
        L::UTinyInt | L::USmallInt | L::UInteger => {
            Value::uinteger(u32::from_le_bytes(fixed(bytes, "UINTEGER")?))
        }
        L::UBigInt => Value::ubigint(u64::from_le_bytes(fixed(bytes, "UBIGINT")?)),
        L::TinyInt | L::SmallInt | L::Integer => Value::integer(i32::from_le_bytes(fixed(bytes, "INTEGER")?)),
        L::BigInt => Value::bigint(i64::from_le_bytes(fixed(bytes, "BIGINT")?)),
        L::Float => {
            let value = if schema.type_info == ExtraTypeInfo::Float16 {
                float16_to_float32(u16::from_le_bytes(fixed(bytes, "FLOAT16")?))
            } else {
                f32::from_le_bytes(fixed(bytes, "FLOAT")?)
            };
            if !value.is_finite() {
                return Ok(Value::default());
            }
            Value::float(value)
        }
        L::Double => {
            if schema.type_info == ExtraTypeInfo::DecimalByteArray {
                return Ok(Value::double(f64::read(bytes, schema.type_scale)?));
            }
            let value = f64::from_le_bytes(fixed(bytes, "DOUBLE")?);
            if !value.is_finite() {
                return Ok(Value::default());
            }
            Value::double(value)
        }
        L::Decimal => {
            let (width, scale) = ty.decimal_properties().unwrap_or((18, 3));
            let what = ty.to_string();
            let unscaled = match schema.type_info {
                ExtraTypeInfo::DecimalInt32 => i128::from(i32::from_le_bytes(fixed(bytes, &what)?)),
                ExtraTypeInfo::DecimalInt64 => i128::from(i64::from_le_bytes(fixed(bytes, &what)?)),
                ExtraTypeInfo::DecimalByteArray => match ty.physical_type() {
                    PhysicalType::Int16 => i128::from(i16::read(bytes, 0)?),
                    PhysicalType::Int32 => i128::from(i32::read(bytes, 0)?),
                    PhysicalType::Int64 => i128::from(i64::read(bytes, 0)?),
                    PhysicalType::Int128 => i128::read(bytes, 0)?,
                    _ => return Err(Error::invalid_input("Unsupported internal type for decimal")),
                },
                _ => return Err(Error::not_implemented("Unrecognized Parquet type for Decimal")),
            };
            Value::decimal(unscaled, width, scale)
        }
        L::Varchar | L::Blob => {
            if ty.id() == L::Blob || std::str::from_utf8(bytes).is_err() {
                Value::varchar(blob_text(bytes))
            } else {
                Value::varchar(String::from_utf8_lossy(bytes))
            }
        }
        L::Date => Value::date(Date(i32::from_le_bytes(fixed(bytes, "DATE")?))),
        L::Time => {
            let value = int_of_either(bytes, "TIME")?;
            Value::time(Time(match schema.type_info {
                ExtraTypeInfo::UnitMs => value
                    .checked_mul(1000)
                    .ok_or_else(|| Error::conversion("Could not convert Time(MS) to Time(US)"))?,
                ExtraTypeInfo::UnitNs => value / 1000,
                _ => value,
            }))
        }
        L::TimeNs => {
            let value = int_of_either(bytes, "TIME")?;
            Value::time_ns(TimeNs(match schema.type_info {
                ExtraTypeInfo::UnitMs => value.wrapping_mul(1_000_000),
                _ => value,
            }))
        }
        L::TimeTz => {
            let value = int_of_either(bytes, "TIMETZ")?;
            let bits = match schema.type_info {
                ExtraTypeInfo::UnitMs => TimeTzFromMs::convert(&(value as i32).to_le_bytes())?,
                ExtraTypeInfo::UnitNs => TimeTzFromNs::convert(&value.to_le_bytes())?,
                _ => TimeTzFromUs::convert(&value.to_le_bytes())?,
            };
            Value::time_tz(TimeTz(bits as u64))
        }
        L::Timestamp | L::TimestampTz => {
            let micros = if schema.type_info == ExtraTypeInfo::ImpalaTimestamp {
                ImpalaToUs::convert(&fixed::<12>(bytes, "TIMESTAMP")?)?
            } else {
                let raw = fixed::<8>(bytes, "TIMESTAMP")?;
                match schema.type_info {
                    ExtraTypeInfo::UnitMs => TimestampMsToUs::convert(&raw)?,
                    ExtraTypeInfo::UnitNs => TimestampNsToUs::convert(&raw)?,
                    _ => i64::from_le_bytes(raw),
                }
            };
            if ty.id() == L::TimestampTz {
                Value::timestamp_tz(Timestamp(micros))
            } else {
                Value::timestamp(Timestamp(micros))
            }
        }
        L::TimestampNs => {
            let nanos = if schema.type_info == ExtraTypeInfo::ImpalaTimestamp {
                ImpalaToNs::convert(&fixed::<12>(bytes, "TIMESTAMP_NS")?)?
            } else {
                let raw = fixed::<8>(bytes, "TIMESTAMP_NS")?;
                match schema.type_info {
                    ExtraTypeInfo::UnitMs => TimestampMsToNs::convert(&raw)?,
                    ExtraTypeInfo::UnitNs => i64::from_le_bytes(raw),
                    _ => TimestampUsToNs::convert(&raw)?,
                }
            };
            Value::timestamp_ns(nanos)
        }
        L::Uuid => {
            let raw = fixed::<16>(bytes, "UUID")?;
            let value = crate::column_reader::values::UuidConversion::convert(&raw)?;
            Value::uuid(roastedduck_types::Uuid(value))
        }
        _ => {
            return Err(Error::internal(format!("Unsupported type for stats {ty}")));
        }
    })
}

/// `ParquetStatisticsUtils::ConvertValue`: the value cast to the column type, NULL if
/// that fails.
pub fn convert_value(
    ty: &LogicalType,
    schema: &ColumnSchema,
    bytes: &[u8],
    casts: &CastFunctionSet,
) -> Result<Value> {
    let value = convert_value_internal(ty, schema, bytes)?;
    Ok(value
        .try_cast_as(casts, ty, false)
        .unwrap_or_else(|_| Value::null(ty.clone())))
}

/// `CreateNumericStats` (and `CreateFloatingPointStats` when NaNs are possible).
fn numeric_stats(
    ty: &LogicalType,
    schema: &ColumnSchema,
    stats: &Statistics,
    nan_max: bool,
    casts: &CastFunctionSet,
) -> Result<BaseStatistics> {
    let mut result = BaseStatistics::create_unknown(ty.clone());
    let min = match (&stats.min_value, &stats.min) {
        (Some(value), _) | (None, Some(value)) => convert_value(ty, schema, value, casts)?,
        _ => Value::null(ty.clone()),
    };
    let max = if nan_max {
        Value::varchar("nan").try_cast_as(casts, ty, false)?
    } else {
        match (&stats.max_value, &stats.max) {
            (Some(value), _) | (None, Some(value)) => convert_value(ty, schema, value, casts)?,
            _ => Value::null(ty.clone()),
        }
    };
    if let StatsData::Numeric(numeric) = result.data_mut() {
        numeric.min = if min.is_null() {
            None
        } else {
            NumericValue::from_value(&min)
        };
        numeric.max = if max.is_null() {
            None
        } else {
            NumericValue::from_value(&max)
        };
    }
    Ok(result)
}

/// Whether a min/max is valid for the column (`StringColumnReader::IsValid`).
fn string_is_valid(bytes: &[u8], is_varchar: bool) -> bool {
    !is_varchar || std::str::from_utf8(bytes).is_ok()
}

/// `ParquetStatisticsUtils::TransformColumnStatistics`: the statistics of a column in a
/// row group, `None` when the file has none.
pub fn transform_column_statistics(
    schema: &ColumnSchema,
    columns: &[ColumnChunk],
    can_have_nan: bool,
    casts: &CastFunctionSet,
) -> Result<Option<BaseStatistics>> {
    use LogicalTypeId as L;
    let ty = &schema.logical_type;
    match ty.id() {
        LogicalTypeId::Array | LogicalTypeId::Map => return Ok(None),
        LogicalTypeId::List => {
            let mut stats = BaseStatistics::create_unknown(ty.clone());
            if let Some(child) = schema.children.first()
                && let Some(child_stats) = transform_column_statistics(child, columns, can_have_nan, casts)?
                && let Some(slot) = stats.child_mut()
            {
                *slot = child_stats;
            }
            return Ok(Some(stats));
        }
        LogicalTypeId::Struct => {
            let mut stats = BaseStatistics::create_unknown(ty.clone());
            for (index, child) in schema.children.iter().enumerate() {
                let child_stats = transform_column_statistics(child, columns, can_have_nan, casts)?;
                if let (Some(child_stats), Some(slots)) = (child_stats, stats.struct_children_mut()) {
                    slots[index] = child_stats;
                }
            }
            return Ok(Some(stats));
        }
        _ => {}
    }
    if schema.kind == SchemaKind::Variant {
        return variant_statistics(schema, columns, can_have_nan, casts);
    }
    if !matches!(schema.kind, SchemaKind::Column | SchemaKind::Geometry) {
        return Ok(None);
    }
    let Some(chunk) = columns.get(schema.column_index) else {
        return Ok(None);
    };
    let Some(meta) = &chunk.meta_data else {
        return Ok(None);
    };
    let Some(parquet_stats) = &meta.statistics else {
        return Ok(None);
    };
    let mut stats = match ty.id() {
        L::UTinyInt
        | L::USmallInt
        | L::UInteger
        | L::UBigInt
        | L::TinyInt
        | L::SmallInt
        | L::Integer
        | L::BigInt
        | L::Date
        | L::Time
        | L::TimeTz
        | L::Timestamp
        | L::TimestampTz
        | L::TimestampSec
        | L::TimestampMs
        | L::TimestampNs
        | L::Decimal => numeric_stats(ty, schema, parquet_stats, false, casts)?,
        L::Float | L::Double => numeric_stats(ty, schema, parquet_stats, can_have_nan, casts)?,
        L::Blob | L::Varchar => {
            let mut stats = BaseStatistics::create_unknown(ty.clone());
            let is_varchar = ty.id() == L::Varchar;
            let min = [&parquet_stats.min_value, &parquet_stats.min]
                .into_iter()
                .flatten()
                .find(|value| string_is_valid(value, is_varchar));
            let max = [&parquet_stats.max_value, &parquet_stats.max]
                .into_iter()
                .flatten()
                .find(|value| string_is_valid(value, is_varchar));
            if let Some(string) = stats.string_mut() {
                if let Some(min) = min {
                    string.min = prefix(min);
                }
                if let Some(max) = max {
                    string.max = prefix(max);
                }
            }
            stats
        }
        L::Geometry => {
            let mut stats = BaseStatistics::create_unknown(ty.clone());
            if let (Some(geo), Some(data)) = (&meta.geospatial_statistics, stats.geometry_mut()) {
                if let Some(bbox) = &geo.bbox {
                    let extent = &mut data.extent;
                    // xmin > xmax is allowed for geometries crossing the antimeridian, which
                    // are not handled.
                    if bbox.xmin <= bbox.xmax {
                        extent.x_min = bbox.xmin;
                        extent.x_max = bbox.xmax;
                    }
                    if bbox.ymin <= bbox.ymax {
                        extent.y_min = bbox.ymin;
                        extent.y_max = bbox.ymax;
                    }
                    if let (Some(zmin), Some(zmax)) = (bbox.zmin, bbox.zmax)
                        && zmin <= zmax
                    {
                        extent.z_min = zmin;
                        extent.z_max = zmax;
                    }
                    if let (Some(mmin), Some(mmax)) = (bbox.mmin, bbox.mmax)
                        && mmin <= mmax
                    {
                        extent.m_min = mmin;
                        extent.m_max = mmax;
                    }
                }
                // Types that are set but empty still mean "any type".
                if let Some(types) = &geo.geospatial_types
                    && !types.is_empty()
                {
                    data.types.clear();
                    for &wkb_type in types {
                        data.types.add_wkb_type(wkb_type);
                    }
                }
            }
            stats
        }
        _ => BaseStatistics::create_unknown(ty.clone()),
    };
    // Validity: `CAN_HAVE_NULL_AND_VALID_VALUES`, narrowed by the null count.
    stats.set_validity(true, true);
    if parquet_stats.null_count == Some(0) {
        stats.set_validity(false, true);
    }
    if parquet_stats.null_count.is_some() && parquet_stats.null_count == Some(meta.num_values) {
        stats.set_validity(stats.can_have_null(), false);
    }
    Ok(Some(stats))
}

/// The VARIANT statistics of a shredded VARIANT column (the VARIANT case of
/// `TransformColumnStatistics`): the statistics of its `value` and `typed_value`
/// columns as those of `untyped_value_index` and `typed_value`.
fn variant_statistics(
    schema: &ColumnSchema,
    columns: &[ColumnChunk],
    can_have_nan: bool,
    casts: &CastFunctionSet,
) -> Result<Option<BaseStatistics>> {
    let [_, value, typed_value] = schema.children.as_slice() else {
        return Ok(None);
    };
    let Some(structured) = typed_value_layout_to_type(&typed_value.logical_type)? else {
        // A nested group without `typed_value` has no structured type.
        return Ok(None);
    };
    let mut stats = BaseStatistics::create_shredded_variant(wrap_shredded_levels(&structured));
    let value_stats = transform_column_statistics(value, columns, can_have_nan, casts)?;
    let typed_value_stats = transform_column_statistics(typed_value, columns, can_have_nan, casts)?;
    let Some(shredded) = stats.variant_mut().and_then(|variant| variant.shredded.as_mut()) else {
        return Ok(None);
    };
    let Some([typed, untyped]) = shredded.struct_children_mut() else {
        return Ok(None);
    };
    if !convert_unshredded_stats(untyped, value_stats.as_ref())
        || !convert_shredded_stats(typed, typed_value_stats.as_ref())
    {
        return Ok(None);
    }
    // The validity is unknown.
    stats.set_has_no_null();
    stats.set_has_null();
    Ok(Some(stats))
}

/// `VariantColumnReader::TypedValueLayoutToType`: the structured type of a `typed_value`
/// layout; `None` when a field or element group has no `typed_value`.
fn typed_value_layout_to_type(typed_value: &LogicalType) -> Result<Option<LogicalType>> {
    let group_typed_value = |group: &LogicalType| -> Result<Option<LogicalType>> {
        let typed_value = group
            .struct_children()
            .unwrap_or_default()
            .iter()
            .find(|(name, _)| name == "typed_value");
        match typed_value {
            Some((_, ty)) => typed_value_layout_to_type(ty),
            None => Ok(None),
        }
    };
    match typed_value.id() {
        _ if !typed_value.is_nested() => Ok(Some(typed_value.clone())),
        LogicalTypeId::Struct => {
            let mut fields = Vec::new();
            for (name, group) in typed_value.struct_children().unwrap_or_default() {
                let Some(field) = group_typed_value(group)? else {
                    return Ok(None);
                };
                fields.push((name.clone(), field));
            }
            Ok(Some(LogicalType::struct_type(fields)))
        }
        LogicalTypeId::List => {
            let element = typed_value.child_type().cloned().unwrap_or_default();
            Ok(group_typed_value(&element)?.map(LogicalType::list))
        }
        _ => Err(Error::invalid_input(format!(
            "VARIANT typed value has to be a primitive/struct/list, not {typed_value}"
        ))),
    }
}

/// `ConvertUnshreddedStats`: `untyped_value_index` statistics from those of a `value`
/// column; values that are all NULL or VARIANT NULL (an empty prefix) make it 0.
fn convert_unshredded_stats(result: &mut BaseStatistics, input: Option<&BaseStatistics>) -> bool {
    let Some(input) = input else {
        return false;
    };
    result.set_validity(input.can_have_null(), input.can_have_no_null());
    if !result.can_have_no_null() {
        return true;
    }
    let all_null = input
        .string()
        .is_some_and(|string| string.min_bytes().is_empty() && string.max_bytes().is_empty());
    if all_null && let Some(numeric) = result.numeric_mut() {
        numeric.min = Some(NumericValue::U32(0));
        numeric.max = Some(NumericValue::U32(0));
        result.set_has_no_null();
    }
    true
}

/// `ConvertShreddedStats`: the statistics of a `typed_value` level from those of its
/// Parquet columns.
fn convert_shredded_stats(result: &mut BaseStatistics, input: Option<&BaseStatistics>) -> bool {
    let Some(input) = input else {
        return false;
    };
    result.set_validity(input.can_have_null(), input.can_have_no_null());
    match result.logical_type().id() {
        LogicalTypeId::List => match (result.child_mut(), input.child()) {
            (Some(result), Some(input)) => convert_shredded_stats_item(result, input),
            _ => false,
        },
        LogicalTypeId::Struct => {
            let inputs = input.struct_children().unwrap_or_default();
            let Some(results) = result.struct_children_mut() else {
                return false;
            };
            results
                .iter_mut()
                .zip(inputs)
                .all(|(result, input)| convert_shredded_stats_item(result, input))
        }
        _ => {
            *result = input.clone();
            true
        }
    }
}

/// `ConvertShreddedStatsItem`: a `STRUCT(typed_value, untyped_value_index)` level from the
/// statistics of a Parquet `{value, typed_value}` group.
fn convert_shredded_stats_item(result: &mut BaseStatistics, input: &BaseStatistics) -> bool {
    let (Some([typed, untyped]), Some([value, typed_value, ..])) =
        (result.struct_children_mut(), input.struct_children())
    else {
        return false;
    };
    convert_unshredded_stats(untyped, Some(value)) && convert_shredded_stats(typed, Some(typed_value))
}

/// The zero-padded 8-byte prefix kept by string statistics.
fn prefix(bytes: &[u8]) -> [u8; 8] {
    let mut target = [0; 8];
    let len = bytes.len().min(8);
    target[..len].copy_from_slice(&bytes[..len]);
    target
}
