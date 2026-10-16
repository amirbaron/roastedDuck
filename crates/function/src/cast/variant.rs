//! Casts to and from VARIANT (DuckDB's `to_variant.cpp` and `from_variant.cpp`).
//!
//! Any type converts to VARIANT ([`rustdb_types::variant::encode`]). VARIANT converts to
//! a type by reading values of the matching variant type directly, and otherwise by
//! converting the stored value back to a SQL value and casting that (strictly). Nested
//! targets walk OBJECTs and ARRAYs, falling back to the value cast when the shape does
//! not match.

use rustdb_common::Result;
use rustdb_types::variant::{self, VariantData, VariantLogicalType, VariantNested, VariantPathComponent};
use rustdb_types::{LogicalType, LogicalTypeId, Value, Vector};

use super::{BoundCastInfo, CastFunctionSet, CastParameters, ValueCast};

/// `DefaultCasts::ImplicitToVariantCast` (`CastToVARIANT`).
pub(super) fn to_variant() -> BoundCastInfo {
    BoundCastInfo::new(|source, result, count, _| {
        if count == 0 {
            return Ok(true);
        }
        let ty = result.logical_type().clone();
        if source.is_constant() {
            let encoded = variant::encode(&[source.get_value(0)], source.logical_type())?;
            *result = match encoded.into_iter().next().flatten() {
                Some(value) => Vector::constant(&value)?,
                None => Vector::constant_null(ty),
            };
            return Ok(true);
        }
        let values: Vec<Value> = (0..count).map(|row| source.get_value(row)).collect();
        let encoded = variant::encode(&values, source.logical_type())?;
        let mut out = Vector::flat(ty, count);
        for (row, value) in encoded.iter().enumerate() {
            match value {
                Some(value) => out.set_value(row, value)?,
                None => out.set_null(row),
            }
        }
        *result = out;
        Ok(true)
    })
}

/// `DefaultCasts::VariantCastSwitch`: the targets VARIANT converts to.
pub(super) fn cast_switch(target: &LogicalType) -> Option<BoundCastInfo> {
    use LogicalTypeId as L;
    match target.id() {
        L::Boolean
        | L::TinyInt
        | L::SmallInt
        | L::Integer
        | L::BigInt
        | L::Date
        | L::Timestamp
        | L::TimestampSec
        | L::TimestampMs
        | L::TimestampNs
        | L::Double
        | L::Float
        | L::Blob
        | L::Bit
        | L::Bignum
        | L::Interval
        | L::HugeInt
        | L::Decimal
        | L::UTinyInt
        | L::USmallInt
        | L::UInteger
        | L::UBigInt
        | L::UHugeInt
        | L::Time
        | L::List
        | L::Struct
        | L::TimeTz
        | L::TimeNs
        | L::TimestampTz
        | L::Map
        | L::Union
        | L::Uuid
        | L::Array
        | L::Geometry
        | L::Varchar => Some(BoundCastInfo::new(cast_from_variant)),
        _ => None,
    }
}

/// One value to convert: the VARIANT it lives in and its index in `values`; `None` for
/// rows that are NULL already.
type Item<'a> = Option<(&'a VariantData, u32)>;

/// `CastFromVARIANT`.
fn cast_from_variant(
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let target = result.logical_type().clone();
    let rows = if source.is_constant() { count.min(1) } else { count };
    let data = (0..rows)
        .map(|row| VariantData::from_value(&source.get_value(row)))
        .collect::<Result<Vec<_>>>()?;
    let items: Vec<Item<'_>> = data
        .iter()
        .map(|data| data.as_ref().map(|data| (data, 0)))
        .collect();
    let mut converter = Converter::default();
    let (values, success) = converter.cast(&items, &target)?;
    let mut out = if source.is_constant() {
        match values.first() {
            Some(value) if !value.is_null() => Vector::constant(value)?,
            _ => Vector::constant_null(target.clone()),
        }
    } else {
        let mut out = Vector::flat(target.clone(), count);
        for (row, value) in values.iter().enumerate() {
            if value.is_null() {
                out.set_null(row);
            } else {
                out.set_value(row, value)?;
            }
        }
        out
    };
    std::mem::swap(result, &mut out);
    if success {
        return Ok(true);
    }
    // `FinalizeErrorMessage`: the message is used as is (no source column suffix).
    let message = format!("{} to '{target}'", converter.error);
    if !params.is_try_cast() {
        return Err(rustdb_common::Error::conversion(message));
    }
    let column = params.source_column.take();
    let assigned = params.assign_error(message);
    params.source_column = column;
    assigned?;
    Ok(false)
}

/// The conversion state shared by all rows (`FromVariantConversionData`).
#[derive(Default)]
struct Converter {
    /// The error of the conversion, if unsuccessful.
    error: String,
}

/// The variant type a target type reads directly, if any.
fn direct_type(target: &LogicalType) -> Option<VariantLogicalType> {
    use LogicalTypeId as L;
    use VariantLogicalType as V;
    Some(match target.id() {
        L::TinyInt => V::Int8,
        L::SmallInt => V::Int16,
        L::Integer => V::Int32,
        L::BigInt => V::Int64,
        L::HugeInt => V::Int128,
        L::UTinyInt => V::Uint8,
        L::USmallInt => V::Uint16,
        L::UInteger => V::Uint32,
        L::UBigInt => V::Uint64,
        L::UHugeInt => V::Uint128,
        L::Float => V::Float,
        L::Double => V::Double,
        L::Date => V::Date,
        L::Timestamp => V::TimestampMicros,
        L::TimestampMs => V::TimestampMilis,
        L::TimestampSec => V::TimestampSec,
        L::TimestampNs => V::TimestampNanos,
        L::Blob => V::Blob,
        L::Geometry => V::Geometry,
        L::Varchar => V::Varchar,
        L::Interval => V::Interval,
        L::Decimal => V::Decimal,
        L::Time => V::TimeMicros,
        L::TimeNs => V::TimeNanos,
        L::TimeTz => V::TimeMicrosTz,
        L::TimestampTz => V::TimestampMicrosTz,
        L::Uuid => V::Uuid,
        L::Bit => V::Bitstring,
        L::Bignum => V::Bignum,
        _ => return None,
    })
}

/// The non-NULL OBJECT/ARRAY data of the items (`VariantUtils::CollectNestedData`):
/// VARIANT NULLs become invalid; the first item of another type fails the collection.
fn collect_nested(
    items: &[Item<'_>],
    expected: VariantLogicalType,
) -> Result<std::result::Result<Vec<Option<VariantNested>>, VariantLogicalType>> {
    let mut nested = Vec::with_capacity(items.len());
    let mut wrong_type = None;
    for item in items {
        let Some((data, index)) = *item else {
            nested.push(None);
            continue;
        };
        let type_id = data.type_id(index)?;
        if type_id == VariantLogicalType::VariantNull {
            nested.push(None);
            continue;
        }
        if type_id != expected {
            wrong_type.get_or_insert(type_id);
            nested.push(None);
            continue;
        }
        nested.push(Some(data.nested(index)?));
    }
    Ok(match wrong_type {
        Some(wrong) => Err(wrong),
        None => Ok(nested),
    })
}

impl Converter {
    /// `CastVariant`: converts the items to `target`, returning one value per item and
    /// whether every item converted.
    fn cast(&mut self, items: &[Item<'_>], target: &LogicalType) -> Result<(Vec<Value>, bool)> {
        use LogicalTypeId as L;
        if !target.is_nested() {
            return self.cast_primitive(items, target);
        }
        let converted = match target.id() {
            L::Struct => self.convert_struct(items, target)?,
            L::List | L::Map => self.convert_list(items, target)?,
            L::Array => self.convert_array(items, target)?,
            L::Union => {
                "Can't convert VARIANT".clone_into(&mut self.error);
                return Ok((vec![Value::null(target.clone()); items.len()], false));
            }
            _ => {
                self.error = format!("Nested type: '{target}' not handled, can't convert VARIANT");
                return Ok((vec![Value::null(target.clone()); items.len()], false));
            }
        };
        if let Some(values) = converted {
            return Ok((values, true));
        }
        // Fall back to converting each value and casting it.
        let mut all_valid = true;
        let mut values = Vec::with_capacity(items.len());
        for item in items {
            let value = match item {
                Some((data, index)) => data.to_sql_value(*index)?,
                None => Value::null(LogicalType::SQLNULL),
            };
            if let Ok(value) = value.try_cast_as(&CastFunctionSet::new(), target, true) {
                values.push(value);
            } else {
                values.push(Value::null(target.clone()));
                all_valid = false;
            }
        }
        Ok((values, all_valid))
    }

    /// `CastVariantToPrimitive`.
    fn cast_primitive(&mut self, items: &[Item<'_>], target: &LogicalType) -> Result<(Vec<Value>, bool)> {
        let Some(direct) = direct_type(target)
            .or_else(|| (target.id() == LogicalTypeId::Boolean).then_some(VariantLogicalType::BoolTrue))
        else {
            "Can't convert VARIANT".clone_into(&mut self.error);
            return Ok((vec![Value::null(target.clone()); items.len()], false));
        };
        let mut all_valid = true;
        let mut values = Vec::with_capacity(items.len());
        for item in items {
            let Some((data, index)) = *item else {
                values.push(Value::null(target.clone()));
                continue;
            };
            let type_id = data.type_id(index)?;
            if !type_id.is_nested() {
                let matches = match direct {
                    VariantLogicalType::BoolTrue => {
                        matches!(
                            type_id,
                            VariantLogicalType::BoolTrue | VariantLogicalType::BoolFalse
                        )
                    }
                    VariantLogicalType::Decimal if type_id == VariantLogicalType::Decimal => {
                        let (width, scale, _) = data.decimal(index)?;
                        let expected = target.decimal_properties().unwrap_or_default();
                        if (width, scale) == (u32::from(expected.0), u32::from(expected.1)) {
                            true
                        } else {
                            self.error = format!("Can't convert from VARIANT(DECIMAL({width}, {scale}))");
                            false
                        }
                    }
                    direct => type_id == direct,
                };
                if matches {
                    values.push(data.to_sql_value(index)?.reinterpret(target.clone()));
                    continue;
                }
                if type_id != VariantLogicalType::Decimal || direct != VariantLogicalType::Decimal {
                    self.set_error_if_empty(format!("Can't convert from VARIANT({})", type_id.name()));
                }
            }
            let value = data.to_sql_value(index)?;
            if let Ok(converted) = value.try_cast_as(&CastFunctionSet::new(), target, true) {
                values.push(converted);
            } else {
                let text = value.to_varchar()?;
                self.error = format!("Can't convert VARIANT({}) value '{text}'", type_id.name());
                values.push(Value::null(target.clone()));
                all_valid = false;
            }
        }
        Ok((values, all_valid))
    }

    fn set_error_if_empty(&mut self, message: String) {
        if self.error.is_empty() {
            self.error = message;
        }
    }

    /// `ConvertVariantToStruct`; `None` if the items do not have the shape of `target`.
    fn convert_struct(&mut self, items: &[Item<'_>], target: &LogicalType) -> Result<Option<Vec<Value>>> {
        let nested = match collect_nested(items, VariantLogicalType::Object)? {
            Ok(nested) => nested,
            Err(wrong) => {
                self.error = format!(
                    "Expected to find VARIANT(OBJECT), found VARIANT({}) instead, can't convert",
                    wrong.name()
                );
                return Ok(None);
            }
        };
        let fields = target.struct_children().unwrap_or_default();
        let mut columns = Vec::with_capacity(fields.len());
        for (name, field_type) in fields {
            let component = VariantPathComponent::Key(name.clone());
            let mut child_items = Vec::with_capacity(items.len());
            for (item, nested) in items.iter().zip(&nested) {
                let (Some((data, _)), Some(nested)) = (item, nested) else {
                    child_items.push(None);
                    continue;
                };
                let Some(child) = data.find_child(*nested, &component)? else {
                    let keys = data.object_keys(*nested)?;
                    self.error = format!("VARIANT(OBJECT({})) is missing key '{name}'", keys.join(","));
                    return Ok(None);
                };
                child_items.push(Some((*data, child)));
            }
            let (values, success) = self.cast(&child_items, field_type)?;
            if !success {
                return Ok(None);
            }
            columns.push(values);
        }
        let values = nested
            .iter()
            .enumerate()
            .map(|(row, nested)| match nested {
                Some(_) => Value::from_children(
                    target.clone(),
                    columns.iter().map(|column| column[row].clone()).collect(),
                ),
                None => Value::null(target.clone()),
            })
            .collect();
        Ok(Some(values))
    }

    /// The element items of each ARRAY item.
    fn array_children<'a>(&mut self, items: &[Item<'a>]) -> Result<Option<Vec<Option<Vec<Item<'a>>>>>> {
        let nested = match collect_nested(items, VariantLogicalType::Array)? {
            Ok(nested) => nested,
            Err(wrong) => {
                self.error = format!(
                    "Expected to find VARIANT(ARRAY), found VARIANT({}) instead, can't convert",
                    wrong.name()
                );
                return Ok(None);
            }
        };
        items
            .iter()
            .zip(nested)
            .map(|(item, nested)| {
                let (Some((data, _)), Some(nested)) = (item, nested) else {
                    return Ok(None);
                };
                (0..nested.child_count)
                    .map(|index| Ok(Some((*data, data.child(nested, index)?.values_index))))
                    .collect::<Result<Vec<_>>>()
                    .map(Some)
            })
            .collect::<Result<Vec<_>>>()
            .map(Some)
    }

    /// `ConvertVariantToList` (also for MAP, whose entries are OBJECTs with `key` and
    /// `value` fields).
    fn convert_list(&mut self, items: &[Item<'_>], target: &LogicalType) -> Result<Option<Vec<Value>>> {
        let Some(children) = self.array_children(items)? else {
            return Ok(None);
        };
        let child_type = target.child_type().cloned().unwrap_or(LogicalType::SQLNULL);
        let mut values = Vec::with_capacity(items.len());
        for elements in children {
            let Some(elements) = elements else {
                values.push(Value::null(target.clone()));
                continue;
            };
            let (converted, success) = self.cast(&elements, &child_type)?;
            if !success {
                return Ok(None);
            }
            values.push(Value::from_children(target.clone(), converted));
        }
        Ok(Some(values))
    }

    /// `ConvertVariantToArray`.
    fn convert_array(&mut self, items: &[Item<'_>], target: &LogicalType) -> Result<Option<Vec<Value>>> {
        let Some(children) = self.array_children(items)? else {
            return Ok(None);
        };
        let (child_type, size) = target
            .array_child()
            .map_or((LogicalType::SQLNULL, 0), |(child, size)| (child.clone(), size));
        if let Some(found) = children
            .iter()
            .flatten()
            .map(Vec::len)
            .find(|&length| length != size as usize)
        {
            self.error = format!("Array size '{size}' was expected, found '{found}', can't convert VARIANT");
            return Ok(None);
        }
        let mut values = Vec::with_capacity(items.len());
        for elements in children {
            let Some(elements) = elements else {
                values.push(Value::null(target.clone()));
                continue;
            };
            let (converted, success) = self.cast(&elements, &child_type)?;
            if !success {
                return Ok(None);
            }
            values.push(Value::from_children(target.clone(), converted));
        }
        Ok(Some(values))
    }
}
