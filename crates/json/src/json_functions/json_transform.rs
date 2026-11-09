//! Converting JSON to SQL values (DuckDB's `json_transform.cpp`): `json_transform` /
//! `from_json` (+ `_strict`), the casts from JSON to any type, and the column conversion
//! of `read_json`.
//!
//! A transform works on a whole column of JSON values at once: objects are split into
//! their fields, arrays into their elements, and scalar values are converted with DuckDB's
//! cast semantics (grouped by JSON type and run through the vectorised casts).

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_function::cast::{BoundCastInfo, CastFunctionSet, CastParameters, cast_vector};
use roastedduck_function::{BindInput, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet};
use roastedduck_functions::strtime::StrpTimeFormat;
use roastedduck_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, Value, Vector, kernels::scatter};

use crate::json_common::{read_document, val_to_string, val_type_to_string};
use crate::json_executors::{ColumnBuilder, StrBuilder, StrInput, addressable, local_arena};
use crate::json_functions::{input_types, is_json_type};
use crate::yyjson::{Doc, JsonArena, ValId, key_eq, tag, write_val};

/// Candidate date and timestamp formats, by type (`DateFormatMap`): the forced
/// `dateformat` / `timestampformat`, or those detected while sampling.
#[derive(Clone, Debug, Default)]
pub(crate) struct DateFormatMap {
    pub(crate) date: Vec<StrpTimeFormat>,
    pub(crate) timestamp: Vec<StrpTimeFormat>,
}

impl DateFormatMap {
    /// The formats of `ty`, if it has any (`HasFormats` / `GetFormats`).
    pub(crate) fn formats(&self, ty: LogicalTypeId) -> Option<&[StrpTimeFormat]> {
        let formats = match ty {
            LogicalTypeId::Date => &self.date,
            LogicalTypeId::Timestamp => &self.timestamp,
            _ => return None,
        };
        (!formats.is_empty()).then_some(formats.as_slice())
    }
}

/// How a transform treats mismatches (`JSONTransformOptions`).
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools, reason = "DuckDB's JSONTransformOptions")]
pub(crate) struct TransformOptions {
    /// Values that cannot be converted are errors instead of NULL.
    pub(crate) strict_cast: bool,
    pub(crate) error_duplicate_key: bool,
    pub(crate) error_missing_key: bool,
    pub(crate) error_unknown_key: bool,
    /// Report errors through the return value instead of raising them.
    pub(crate) delay_error: bool,
    pub(crate) error_message: Vec<u8>,
    /// The row of the first error.
    pub(crate) object_index: usize,
    pub(crate) date_format_map: Option<Arc<DateFormatMap>>,
    pub(crate) casts: Arc<CastFunctionSet>,
}

impl TransformOptions {
    #[allow(
        clippy::fn_params_excessive_bools,
        reason = "DuckDB's JSONTransformOptions constructor"
    )]
    pub(crate) fn new(
        strict_cast: bool,
        error_duplicate_key: bool,
        error_missing_key: bool,
        error_unknown_key: bool,
        casts: Arc<CastFunctionSet>,
    ) -> Self {
        Self {
            strict_cast,
            error_duplicate_key,
            error_missing_key,
            error_unknown_key,
            delay_error: false,
            error_message: Vec::new(),
            object_index: 0,
            date_format_map: None,
            casts,
        }
    }

    /// Records the first error (when strict).
    fn fail(&mut self, success: &mut bool, row: usize, message: impl FnOnce() -> String) {
        if *success && self.strict_cast {
            self.error_message = message().into_bytes();
            self.object_index = row;
            *success = false;
        }
    }
}

/// The values to convert: `None` for missing values (treated like JSON `null`).
pub(crate) type Vals = [Option<ValId>];

/// Whether a value is absent or JSON `null`.
fn is_null(doc: &Doc<'_>, val: Option<ValId>) -> bool {
    val.is_none_or(|val| doc.is_null(val))
}

/// A column of `count` NULLs of `ty`.
fn null_vector(ty: &LogicalType, count: usize) -> Vector {
    let mut vector = Vector::flat(ty.clone(), count.max(1));
    for row in 0..count {
        vector.set_null(row);
    }
    vector
}

/// The JSON source type groups of a scalar conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Bool,
    UInt,
    SInt,
    Real,
    Str,
}

impl Source {
    fn logical_type(self) -> LogicalType {
        match self {
            Self::Bool => LogicalType::BOOLEAN,
            Self::UInt => LogicalType::UBIGINT,
            Self::SInt => LogicalType::BIGINT,
            Self::Real => LogicalType::DOUBLE,
            Self::Str => LogicalType::VARCHAR,
        }
    }
}

/// The source group of a scalar value; `None` for arrays and objects.
fn source_of(doc: &Doc<'_>, val: ValId) -> Option<Source> {
    match doc.tag(val) {
        tag::TRUE | tag::FALSE => Some(Source::Bool),
        tag::UINT => Some(Source::UInt),
        tag::SINT => Some(Source::SInt),
        tag::REAL => Some(Source::Real),
        tag::STR | tag::STR_NOESC | tag::RAW => Some(Source::Str),
        _ => None,
    }
}

/// The values of one source group as a vector of the group's type.
fn group_vector(doc: &Doc<'_>, vals: &Vals, rows: &[u32], source: Source) -> Result<Vector> {
    let count = rows.len();
    let ty = source.logical_type();
    let val_at = |i: usize| vals[rows[i] as usize].unwrap_or(0);
    Ok(match source {
        Source::Bool => {
            let mut vector = Vector::flat(ty, count.max(1));
            for (i, out) in vector.values_mut::<bool>()[..count].iter_mut().enumerate() {
                *out = doc.get_bool(val_at(i));
            }
            vector
        }
        Source::UInt => {
            let mut vector = Vector::flat(ty, count.max(1));
            for (i, out) in vector.values_mut::<u64>()[..count].iter_mut().enumerate() {
                *out = doc.get_uint(val_at(i));
            }
            vector
        }
        Source::SInt => {
            let mut vector = Vector::flat(ty, count.max(1));
            for (i, out) in vector.values_mut::<i64>()[..count].iter_mut().enumerate() {
                *out = doc.get_sint(val_at(i));
            }
            vector
        }
        Source::Real => {
            let mut vector = Vector::flat(ty, count.max(1));
            for (i, out) in vector.values_mut::<f64>()[..count].iter_mut().enumerate() {
                *out = doc.get_real(val_at(i));
            }
            vector
        }
        Source::Str => {
            let mut builder = StrBuilder::default();
            for i in 0..count {
                builder.push(doc.str(val_at(i)));
            }
            builder.finish(&ty)?
        }
    })
}

/// Converts scalar values with DuckDB's `TryCast` semantics (`TransformNumerical` /
/// `TransformDecimal`): grouped by JSON type, each group cast as a vector.
fn transform_scalar(
    doc: &Doc<'_>,
    vals: &Vals,
    target: &LogicalType,
    options: &mut TransformOptions,
    error_prefix: &str,
) -> Result<(Vector, bool)> {
    let count = vals.len();
    let mut groups: [Vec<u32>; 5] = Default::default();
    let mut failed_rows: Vec<usize> = Vec::new();
    let mut nulls: Vec<usize> = Vec::new();
    for (row, &val) in vals.iter().enumerate() {
        let Some(val) = val.filter(|&val| !doc.is_null(val)) else {
            nulls.push(row);
            continue;
        };
        match source_of(doc, val) {
            Some(source) => groups[source as usize].push(row as u32),
            None => failed_rows.push(row),
        }
    }
    let mut result = Vector::flat(target.clone(), count.max(1));
    for &row in &nulls {
        result.set_null(row);
    }
    for &row in &failed_rows {
        result.set_null(row);
    }
    for source in [
        Source::Bool,
        Source::UInt,
        Source::SInt,
        Source::Real,
        Source::Str,
    ] {
        let rows = &groups[source as usize];
        if rows.is_empty() {
            continue;
        }
        let input = group_vector(doc, vals, rows, source)?;
        let mut params = CastParameters::try_cast();
        params.strict = options.strict_cast;
        let converted = cast_vector(&options.casts, &input, target, rows.len(), &mut params)?;
        for (i, &row) in rows.iter().enumerate() {
            if converted.is_null(i) {
                failed_rows.push(row as usize);
            }
        }
        scatter(&converted, &mut result, rows)?;
    }
    let mut success = true;
    if let Some(&first) = failed_rows.iter().min() {
        for &row in &failed_rows {
            result.set_null(row);
        }
        let val = vals[first].unwrap_or(0);
        options.fail(&mut success, first, || {
            format!("{error_prefix}: {}", val_to_string(doc, val, Some(50)))
        });
    }
    Ok((result, success))
}

/// `TransformToString`: strings as their text, other values as JSON or cast text.
fn transform_to_string(
    doc: &Doc<'_>,
    vals: &Vals,
    target: &LogicalType,
    options: &TransformOptions,
) -> Result<Vector> {
    let mut builder = StrBuilder::default();
    for &val in vals {
        let Some(val) = val.filter(|&val| !doc.is_null(val)) else {
            builder.push_null();
            continue;
        };
        match doc.tag(val) {
            tag::STR | tag::STR_NOESC | tag::RAW => builder.push(doc.str(val)),
            tag::ARR | tag::OBJ => builder.push_with(|buf| write_val(buf, doc, val)),
            tag::TRUE => builder.push(b"true"),
            tag::FALSE => builder.push(b"false"),
            tag::UINT => builder.push_with(|buf| crate::yyjson::write_u64(buf, doc.get_uint(val))),
            tag::SINT => builder.push_with(|buf| crate::yyjson::write_i64(buf, doc.get_sint(val))),
            _ => {
                let text = Value::double(doc.get_real(val)).to_varchar().unwrap_or_default();
                builder.push(text.as_bytes());
            }
        }
    }
    let _ = options;
    builder.finish(target)
}

/// `TransformToJSON`: the values as JSON text.
fn transform_to_json(doc: &Doc<'_>, vals: &Vals, target: &LogicalType) -> Result<Vector> {
    let mut builder = StrBuilder::default();
    for &val in vals {
        match val.filter(|&val| !doc.is_null(val)) {
            Some(val) => builder.push_with(|buf| write_val(buf, doc, val)),
            None => builder.push_null(),
        }
    }
    builder.finish(target)
}

/// `EnumUtil::ToString(LogicalTypeId)`.
pub(crate) fn type_id_string(ty: LogicalTypeId) -> String {
    use LogicalTypeId as L;
    let name = match ty {
        L::AggregateState => "AGGREGATE_STATE",
        L::Enum => "ENUM",
        L::Date => "DATE",
        L::Interval => "INTERVAL",
        L::Time => "TIME",
        L::TimeNs => "TIME_NS",
        L::TimeTz => "TIME WITH TIME ZONE",
        L::Timestamp => "TIMESTAMP",
        L::TimestampTz => "TIMESTAMP WITH TIME ZONE",
        L::TimestampNs => "TIMESTAMP_NS",
        L::TimestampMs => "TIMESTAMP_MS",
        L::TimestampSec => "TIMESTAMP_S",
        L::Uuid => "UUID",
        L::Geometry => "GEOMETRY",
        L::SqlNull => "NULL",
        _ => "INVALID",
    };
    name.to_owned()
}

/// `JSONTransform::GetStringVector`: the string values (others NULL, an error if strict).
pub(crate) fn get_string_vector(
    doc: &Doc<'_>,
    vals: &Vals,
    target: LogicalTypeId,
    options: &mut TransformOptions,
) -> Result<(Vector, bool)> {
    let mut builder = StrBuilder::default();
    let mut success = true;
    for (row, &val) in vals.iter().enumerate() {
        let Some(val) = val.filter(|&val| !doc.is_null(val)) else {
            builder.push_null();
            continue;
        };
        if !doc.is_str(val) {
            builder.push_null();
            options.fail(&mut success, row, || {
                format!(
                    "Unable to cast '{}' to {}",
                    val_to_string(doc, val, Some(50)),
                    type_id_string(target)
                )
            });
            continue;
        }
        builder.push(doc.str(val));
    }
    Ok((builder.finish(&LogicalType::VARCHAR)?, success))
}

/// `TransformFromString`: strings cast to the target type.
fn transform_from_string(
    doc: &Doc<'_>,
    vals: &Vals,
    target: &LogicalType,
    options: &mut TransformOptions,
) -> Result<(Vector, bool)> {
    let (strings, mut success) = get_string_vector(doc, vals, target.id(), options)?;
    let mut params = CastParameters::try_cast();
    let result = cast_vector(&options.casts, &strings, target, vals.len(), &mut params)?;
    if let Some(message) = params.take_error_message()
        && options.strict_cast
    {
        options.error_message = message;
        options.error_message.extend_from_slice(
            b"\n If this error occurred during read_json, line/object number information is approximate",
        );
        options.object_index = 0;
        success = false;
    }
    Ok((result, success))
}

/// `TransformFromStringWithFormat`: dates/timestamps with the detected formats.
fn transform_from_string_with_format(
    doc: &Doc<'_>,
    vals: &Vals,
    target: &LogicalType,
    formats: &[StrpTimeFormat],
    options: &mut TransformOptions,
) -> Result<(Vector, bool)> {
    let (strings, mut success) = get_string_vector(doc, vals, target.id(), options)?;
    let count = vals.len();
    let input = StrInput::new(&strings);
    let is_date = target.id() == LogicalTypeId::Date;
    let mut result = Vector::flat(target.clone(), count.max(1));
    let try_format = |format: &StrpTimeFormat, text: &[u8], result: &mut Vector, row: usize| -> bool {
        if is_date {
            match format.try_parse_date(text) {
                Some(date) => {
                    result.values_mut::<roastedduck_types::Date>()[row] = date;
                    true
                }
                None => false,
            }
        } else {
            match format.try_parse_timestamp(text) {
                Some(timestamp) => {
                    result.values_mut::<roastedduck_types::Timestamp>()[row] = timestamp;
                    true
                }
                None => false,
            }
        }
    };
    // The most specific format that matches the first non-NULL value.
    let mut matched = None;
    for row in 0..count {
        let Some(text) = input.get(row) else {
            continue;
        };
        matched = (0..formats.len())
            .rev()
            .find(|&j| try_format(&formats[j], text, &mut result, row));
        break;
    }
    let limit = matched.map_or(formats.len(), |index| index + 1);
    for row in 0..count {
        let Some(text) = input.get(row) else {
            result.set_null(row);
            continue;
        };
        let parsed = (0..limit)
            .rev()
            .any(|j| try_format(&formats[j], text, &mut result, row));
        if !parsed {
            result.set_null(row);
            if success && options.strict_cast {
                options.object_index = row;
                success = false;
            }
        }
    }
    Ok((result, success))
}

/// Names of a struct type.
fn struct_names(ty: &LogicalType) -> Vec<roastedduck_common::ByteString> {
    ty.struct_children()
        .unwrap_or_default()
        .iter()
        .map(|(name, _)| name.clone())
        .collect()
}

/// Looks up keys in a set of column names (a hash map for many columns).
pub(crate) struct KeyMap<'n> {
    names: &'n [roastedduck_common::ByteString],
    map: Option<HashMap<&'n [u8], usize>>,
    /// Whether the names are distinct, so that a guessed match is the first match.
    distinct: bool,
}

impl<'n> KeyMap<'n> {
    pub(crate) fn new(names: &'n [roastedduck_common::ByteString]) -> Self {
        let mut map = HashMap::with_capacity(names.len());
        for (index, name) in names.iter().enumerate() {
            map.entry(name.as_bytes()).or_insert(index);
        }
        let distinct = map.len() == names.len();
        let map = (names.len() > 8).then_some(map);
        Self { names, map, distinct }
    }

    /// [`Self::find`], trying column `hint` first: objects usually list their keys in the
    /// order of the columns, so the column after the previous key's is a good guess.
    #[inline]
    pub(crate) fn find_from(&self, key: &[u8], hint: usize) -> Option<usize> {
        if self.distinct
            && self
                .names
                .get(hint)
                .is_some_and(|name| key_eq(name.as_bytes(), key))
        {
            return Some(hint);
        }
        self.find(key)
    }

    #[inline]
    pub(crate) fn find(&self, key: &[u8]) -> Option<usize> {
        match &self.map {
            Some(map) => map.get(key).copied(),
            None => self.names.iter().position(|name| key_eq(name.as_bytes(), key)),
        }
    }
}

/// `JSONTransform::TransformObject`: splits objects into the given fields and converts
/// each field. Returns the field vectors.
pub(crate) fn transform_object(
    doc: &Doc<'_>,
    objects: &Vals,
    names: &[roastedduck_common::ByteString],
    types: &[LogicalType],
    options: &mut TransformOptions,
    error_unknown_key: bool,
) -> Result<(Vec<Vector>, bool)> {
    let count = objects.len();
    let column_count = names.len();
    let key_map = KeyMap::new(names);
    let mut nested: Vec<Vec<Option<ValId>>> = vec![vec![None; count]; column_count];
    let mut found = vec![false; column_count];
    let mut success = true;
    for (i, &obj) in objects.iter().enumerate() {
        let Some(obj) = obj.filter(|&obj| !doc.is_null(obj)) else {
            continue;
        };
        if !doc.is_obj(obj) {
            options.fail(&mut success, i, || {
                format!(
                    "Expected OBJECT, but got {}: {}",
                    val_type_to_string(doc, obj),
                    val_to_string(doc, obj, Some(50))
                )
            });
            continue;
        }
        found.fill(false);
        let mut found_count = 0;
        let mut hint = 0;
        for (key, val) in doc.obj_iter(obj) {
            let key_bytes = doc.str(key);
            match key_map.find_from(key_bytes, hint) {
                Some(col) => {
                    hint = col + 1;
                    if found[col] {
                        if success && options.error_duplicate_key {
                            options.error_message = format!(
                                "Object {} has duplicate key \"{}\"",
                                val_to_string(doc, obj, Some(50)),
                                String::from_utf8_lossy(key_bytes)
                            )
                            .into_bytes();
                            options.object_index = i;
                            success = false;
                        }
                    } else {
                        nested[col][i] = Some(val);
                        found[col] = true;
                        found_count += 1;
                    }
                }
                None => {
                    if success && error_unknown_key && options.error_unknown_key {
                        options.error_message = format!(
                            "Object {} has unknown key \"{}\"",
                            val_to_string(doc, obj, Some(50)),
                            String::from_utf8_lossy(key_bytes)
                        )
                        .into_bytes();
                        options.object_index = i;
                        success = false;
                    }
                }
            }
        }
        if found_count != column_count {
            for col in 0..column_count {
                if found[col] {
                    continue;
                }
                if success && options.error_missing_key {
                    options.error_message = roastedduck_common::byte_format!(
                        "Object {} does not have key \"{}\"",
                        val_to_string(doc, obj, Some(50)),
                        names[col]
                    )
                    .into_bytes();
                    options.object_index = i;
                    success = false;
                }
            }
        }
    }
    let mut vectors = Vec::with_capacity(column_count);
    for (col, ty) in types.iter().enumerate() {
        let (vector, ok) = transform(doc, &nested[col], ty, options)?;
        success &= ok;
        vectors.push(vector);
    }
    if !options.delay_error && !success {
        return Err(Error::new_bytes(
            roastedduck_common::ErrorKind::InvalidInput,
            options.error_message.clone(),
        ));
    }
    Ok((vectors, success))
}

/// `TransformObjectInternal`: a STRUCT column.
fn transform_struct(
    doc: &Doc<'_>,
    objects: &Vals,
    target: &LogicalType,
    options: &mut TransformOptions,
) -> Result<(Vector, bool)> {
    let names = struct_names(target);
    let types: Vec<LogicalType> = target
        .struct_children()
        .unwrap_or_default()
        .iter()
        .map(|(_, ty)| ty.clone())
        .collect();
    let (children, success) = transform_object(doc, objects, &names, &types, options, true)?;
    let count = objects.len();
    let mut result = Vector::flat(target.clone(), count.max(1));
    if let Some(slots) = result.struct_children_mut() {
        for (slot, child) in slots.iter_mut().zip(children) {
            *slot = child;
        }
    }
    for (row, &obj) in objects.iter().enumerate() {
        if is_null(doc, obj) {
            result.set_null(row);
        }
    }
    Ok((result, success))
}

/// `TransformArrayToList`.
fn transform_list(
    doc: &Doc<'_>,
    arrays: &Vals,
    target: &LogicalType,
    options: &mut TransformOptions,
) -> Result<(Vector, bool)> {
    let count = arrays.len();
    let mut success = true;
    let mut entries = vec![ListEntry::default(); count];
    let mut nulls = Vec::new();
    let mut elements = Vec::new();
    for (i, &arr) in arrays.iter().enumerate() {
        let Some(arr) = arr.filter(|&arr| !doc.is_null(arr)) else {
            nulls.push(i);
            continue;
        };
        if !doc.is_arr(arr) {
            nulls.push(i);
            options.fail(&mut success, i, || {
                format!(
                    "Expected ARRAY, but got {}: {}",
                    val_type_to_string(doc, arr),
                    val_to_string(doc, arr, Some(50))
                )
            });
            continue;
        }
        let offset = elements.len();
        elements.extend(doc.arr_iter(arr).map(Some));
        entries[i] = ListEntry::new(offset, elements.len() - offset);
    }
    let child_type = target.child_type().cloned().unwrap_or(LogicalType::SQLNULL);
    let child_object_index_before = options.object_index;
    let (child, child_ok) = transform(doc, &elements, &child_type, options)?;
    if !child_ok && success {
        // Map the failing element back to its list for line numbers.
        let element = options.object_index;
        if let Some(row) = entries
            .iter()
            .enumerate()
            .find(|(row, entry)| !nulls.contains(row) && entry.range().contains(&element))
            .map(|(row, _)| row)
        {
            options.object_index = row;
        } else {
            options.object_index = child_object_index_before;
        }
    }
    success &= child_ok;
    let mut result = Vector::flat(target.clone(), count.max(1));
    result.values_mut::<ListEntry>()[..count].copy_from_slice(&entries);
    result.set_list_child(child, elements.len());
    for row in nulls {
        result.set_null(row);
    }
    if !options.delay_error && !success {
        return Err(Error::new_bytes(
            roastedduck_common::ErrorKind::InvalidInput,
            options.error_message.clone(),
        ));
    }
    Ok((result, success))
}

/// `TransformArrayToArray`.
fn transform_array(
    doc: &Doc<'_>,
    arrays: &Vals,
    target: &LogicalType,
    options: &mut TransformOptions,
) -> Result<(Vector, bool)> {
    let count = arrays.len();
    let (child_type, size) = target
        .array_child()
        .map_or((LogicalType::SQLNULL, 0), |(child, size)| {
            (child.clone(), size as usize)
        });
    let mut success = true;
    let mut nulls = Vec::new();
    let mut elements: Vec<Option<ValId>> = Vec::with_capacity(count * size);
    for (i, &arr) in arrays.iter().enumerate() {
        let valid = match arr.filter(|&arr| !doc.is_null(arr)) {
            None => false,
            Some(arr) if !doc.is_arr(arr) => {
                options.fail(&mut success, i, || {
                    format!(
                        "Expected ARRAY, but got {}: {}",
                        val_type_to_string(doc, arr),
                        val_to_string(doc, arr, Some(50))
                    )
                });
                false
            }
            Some(arr) if doc.len(arr) != size => {
                let len = doc.len(arr);
                options.fail(&mut success, i, || {
                    format!(
                        "Expected array of size {size}, but got '{}' with size {len}",
                        val_to_string(doc, arr, Some(50))
                    )
                });
                false
            }
            Some(arr) => {
                elements.extend(doc.arr_iter(arr).map(Some));
                true
            }
        };
        if !valid {
            nulls.push(i);
            elements.extend(std::iter::repeat_n(None, size));
        }
    }
    if !success {
        let element = options.object_index;
        if let Some(row) =
            (0..count).find(|row| !nulls.contains(row) && (row * size..(row + 1) * size).contains(&element))
        {
            options.object_index = row;
        }
    }
    let (child, child_ok) = transform(doc, &elements, &child_type, options)?;
    success &= child_ok;
    let mut result = Vector::flat(target.clone(), count.max(1));
    result.set_array_child(child);
    for row in nulls {
        result.set_null(row);
    }
    if !options.delay_error && !success {
        return Err(Error::new_bytes(
            roastedduck_common::ErrorKind::InvalidInput,
            options.error_message.clone(),
        ));
    }
    Ok((result, success))
}

/// `TransformObjectToMap`.
fn transform_map(
    doc: &Doc<'_>,
    objects: &Vals,
    target: &LogicalType,
    options: &mut TransformOptions,
) -> Result<(Vector, bool)> {
    let count = objects.len();
    let (key_type, value_type) = target
        .map_key_value()
        .map_or((LogicalType::VARCHAR, LogicalType::SQLNULL), |(k, v)| {
            (k.clone(), v.clone())
        });
    let mut success = true;
    let mut entries = vec![ListEntry::default(); count];
    let mut nulls = Vec::new();
    let mut keys = Vec::new();
    let mut values = Vec::new();
    for (i, &obj) in objects.iter().enumerate() {
        let Some(obj) = obj.filter(|&obj| !doc.is_null(obj)) else {
            nulls.push(i);
            continue;
        };
        if !doc.is_obj(obj) {
            nulls.push(i);
            options.fail(&mut success, i, || {
                format!(
                    "Expected OBJECT, but got {}: {}",
                    val_type_to_string(doc, obj),
                    val_to_string(doc, obj, Some(50))
                )
            });
            continue;
        }
        let offset = keys.len();
        for (key, value) in doc.obj_iter(obj) {
            keys.push(Some(key));
            values.push(Some(value));
        }
        entries[i] = ListEntry::new(offset, keys.len() - offset);
    }
    let (key_vector, keys_ok) = transform(doc, &keys, &key_type, options)?;
    if !keys_ok {
        let mut message = options.error_message.clone();
        message.extend_from_slice(b". Cannot default to NULL, because map keys cannot be NULL");
        return Err(Error::new_bytes(
            roastedduck_common::ErrorKind::Conversion,
            message,
        ));
    }
    let (value_vector, values_ok) = transform(doc, &values, &value_type, options)?;
    success &= values_ok;
    let entry_type = LogicalType::struct_type(vec![
        ("key".to_owned().into(), key_type),
        ("value".to_owned().into(), value_type),
    ]);
    let mut entry_vector = Vector::flat(entry_type, keys.len().max(1));
    if let Some(slots) = entry_vector.struct_children_mut() {
        let mut parts = [key_vector, value_vector].into_iter();
        for slot in slots.iter_mut() {
            if let Some(part) = parts.next() {
                *slot = part;
            }
        }
    }
    let mut result = Vector::flat(target.clone(), count.max(1));
    result.values_mut::<ListEntry>()[..count].copy_from_slice(&entries);
    result.set_list_child(entry_vector, keys.len());
    for row in nulls {
        result.set_null(row);
    }
    if !options.delay_error && !success {
        return Err(Error::new_bytes(
            roastedduck_common::ErrorKind::InvalidInput,
            options.error_message.clone(),
        ));
    }
    Ok((result, success))
}

/// `TransformValueIntoUnion`.
fn transform_union(
    doc: &Doc<'_>,
    vals: &Vals,
    target: &LogicalType,
    options: &mut TransformOptions,
) -> Result<(Vector, bool)> {
    let members: Vec<(roastedduck_common::ByteString, LogicalType)> =
        target.union_members().unwrap_or_default().to_vec();
    let count = vals.len();
    let mut success = true;
    let mut result = Vector::flat(target.clone(), count.max(1));
    for (i, &val) in vals.iter().enumerate() {
        let Some(obj) = val.filter(|&val| !doc.is_null(val)) else {
            result.set_null(i);
            continue;
        };
        let mut set_error = |options: &mut TransformOptions, result: &mut Vector, message: String| {
            result.set_null(i);
            options.fail(&mut success, i, || message);
        };
        if !doc.is_obj(obj) {
            let desc = match doc.get_type(obj) {
                crate::yyjson::ValType::Raw => "raw",
                crate::yyjson::ValType::Null => "null",
                crate::yyjson::ValType::Bool => "boolean",
                crate::yyjson::ValType::Num => "number",
                crate::yyjson::ValType::Str => "string",
                crate::yyjson::ValType::Arr => "array",
                _ => "unknown",
            };
            let desc = match doc.tag(obj) {
                tag::UINT => "uint",
                tag::SINT => "sint",
                tag::REAL => "real",
                tag::TRUE => "true",
                tag::FALSE => "false",
                _ => desc,
            };
            set_error(
                options,
                &mut result,
                format!("Expected an object representing a union, got {desc}"),
            );
            continue;
        }
        let len = doc.len(obj);
        if len > 1 {
            set_error(
                options,
                &mut result,
                "Found object containing more than one key, instead of union".to_owned(),
            );
            continue;
        }
        if len == 0 {
            set_error(
                options,
                &mut result,
                "Found empty object, instead of union".to_owned(),
            );
            continue;
        }
        let Some((key, value)) = doc.obj_iter(obj).next() else {
            continue;
        };
        let key_text = String::from_utf8_lossy(doc.str(key)).into_owned();
        let Some(tag_index) = members.iter().position(|(name, _)| *name == key_text) else {
            set_error(
                options,
                &mut result,
                format!("Found object containing unknown key, instead of union: {key_text}"),
            );
            continue;
        };
        let (single, ok) = transform(doc, &[Some(value)], &members[tag_index].1, options)?;
        success &= ok;
        let value = Value::union_value(target.clone(), tag_index as u8, single.get_value(0));
        result.set_value(i, &value)?;
    }
    Ok((result, success))
}

/// `JSONTransform::Transform`: converts a column of JSON values to `target`.
pub(crate) fn transform(
    doc: &Doc<'_>,
    vals: &Vals,
    target: &LogicalType,
    options: &mut TransformOptions,
) -> Result<(Vector, bool)> {
    use LogicalTypeId as L;
    let count = vals.len();
    if matches!(target.id(), L::Timestamp | L::Date)
        && let Some(map) = options.date_format_map.clone()
        && let Some(formats) = map.formats(target.id())
    {
        return transform_from_string_with_format(doc, vals, target, formats, options);
    }
    if is_json_type(target) {
        return Ok((transform_to_json(doc, vals, target)?, true));
    }
    match target.id() {
        L::SqlNull => Ok((null_vector(target, count), true)),
        L::Boolean
        | L::TinyInt
        | L::SmallInt
        | L::Integer
        | L::BigInt
        | L::UTinyInt
        | L::USmallInt
        | L::UInteger
        | L::UBigInt
        | L::HugeInt
        | L::UHugeInt
        | L::Float
        | L::Double => {
            if let Some(result) = transform_fast_numeric(doc, vals, target) {
                return Ok((result, true));
            }
            transform_scalar(doc, vals, target, options, "Failed to cast value to numerical")
        }
        L::Decimal => transform_scalar(doc, vals, target, options, "Failed to cast value to decimal"),
        L::AggregateState
        | L::Enum
        | L::Date
        | L::Interval
        | L::Time
        | L::TimeNs
        | L::TimeTz
        | L::Timestamp
        | L::TimestampTz
        | L::TimestampNs
        | L::TimestampMs
        | L::TimestampSec
        | L::Uuid
        | L::Geometry => transform_from_string(doc, vals, target, options),
        L::Varchar | L::Blob => Ok((transform_to_string(doc, vals, target, options)?, true)),
        L::Struct => transform_struct(doc, vals, target, options),
        L::List => transform_list(doc, vals, target, options),
        L::Map => transform_map(doc, vals, target, options),
        L::Union => transform_union(doc, vals, target, options),
        L::Array => transform_array(doc, vals, target, options),
        _ => Err(Error::not_implemented(format!(
            "Cannot read a value of type {target} from a json file"
        ))),
    }
}

/// Converts without casts when every value already has the target's natural JSON type
/// (BIGINT from integers, DOUBLE from numbers, BOOLEAN from booleans).
fn transform_fast_numeric(doc: &Doc<'_>, vals: &Vals, target: &LogicalType) -> Option<Vector> {
    let count = vals.len();
    let mut result = Vector::flat(target.clone(), count.max(1));
    let mut nulls = Vec::new();
    match target.id() {
        LogicalTypeId::BigInt => {
            let out = result.values_mut::<i64>();
            for (row, &val) in vals.iter().enumerate() {
                match val {
                    None => nulls.push(row),
                    Some(val) => match doc.tag(val) {
                        tag::NULL => nulls.push(row),
                        tag::SINT => out[row] = doc.get_sint(val),
                        tag::UINT => out[row] = i64::try_from(doc.get_uint(val)).ok()?,
                        _ => return None,
                    },
                }
            }
        }
        LogicalTypeId::Double => {
            let out = result.values_mut::<f64>();
            for (row, &val) in vals.iter().enumerate() {
                match val {
                    None => nulls.push(row),
                    Some(val) => match doc.tag(val) {
                        tag::NULL => nulls.push(row),
                        tag::REAL => out[row] = doc.get_real(val),
                        tag::SINT => out[row] = doc.get_sint(val) as f64,
                        tag::UINT => out[row] = doc.get_uint(val) as f64,
                        _ => return None,
                    },
                }
            }
        }
        LogicalTypeId::Boolean => {
            let out = result.values_mut::<bool>();
            for (row, &val) in vals.iter().enumerate() {
                match val {
                    None => nulls.push(row),
                    Some(val) => match doc.tag(val) {
                        tag::NULL => nulls.push(row),
                        tag::TRUE => out[row] = true,
                        tag::FALSE => out[row] = false,
                        _ => return None,
                    },
                }
            }
        }
        _ => return None,
    }
    for row in nulls {
        result.set_null(row);
    }
    Some(result)
}

/// The bind data of `json_transform`: the target type.
#[derive(Debug)]
struct TransformBindData {
    casts: Arc<CastFunctionSet>,
}

impl FunctionData for TransformBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn equals(&self, other: &dyn FunctionData) -> bool {
        other.as_any().is::<Self>()
    }
}

/// The client context of a bind.
fn bind_context<'a>(input: &BindInput<'a>) -> Option<&'a roastedduck_planner::ClientContext> {
    input
        .binder
        .and_then(|binder| binder.downcast_ref::<roastedduck_planner::ClientContext>())
}

/// The types suggested when a type name does not parse (`TransformStringToUnboundType`).
const SUGGESTED_TYPES: [&str; 47] = [
    "BIGINT",
    "INT8",
    "LONG",
    "BIT",
    "BITSTRING",
    "BLOB",
    "BYTEA",
    "BINARY,",
    "VARBINARY",
    "BOOLEAN",
    "BOOL",
    "LOGICAL",
    "DATE",
    "DECIMAL(prec, scale)",
    "DOUBLE",
    "FLOAT8",
    "FLOAT",
    "FLOAT4",
    "REAL",
    "HUGEINT",
    "INTEGER",
    "INT4",
    "INT",
    "SIGNED",
    "INTERVAL",
    "SMALLINT",
    "INT2",
    "SHORT",
    "TIME",
    "TIMESTAMPTZ",
    "TIMESTAMP",
    "DATETIME",
    "TINYINT",
    "INT1",
    "UBIGINT",
    "UHUGEINT",
    "UINTEGER",
    "USMALLINT",
    "UTINYINT",
    "UUID",
    "VARCHAR",
    "CHAR",
    "BPCHAR",
    "TEXT",
    "STRING",
    "MAP(INTEGER, VARCHAR)",
    "UNION(num INTEGER, text VARCHAR)",
];

/// `TransformStringToLogicalType` (`TypeManager::ParseLogicalType`): a type name as
/// written in SQL.
pub(crate) fn string_to_logical_type(
    context: Option<&roastedduck_planner::ClientContext>,
    text: &str,
) -> Result<LogicalType> {
    if text.eq_ignore_ascii_case("null") {
        return Ok(LogicalType::SQLNULL);
    }
    let column = match roastedduck_parser::parse_column_definition(
        &format!("dummy {text}"),
        &roastedduck_parser::ParserOptions::default(),
    ) {
        Ok(column) => column,
        Err(error) if error.kind() == roastedduck_common::ErrorKind::Parser => {
            let candidates: Vec<String> = SUGGESTED_TYPES.iter().map(|ty| (*ty).to_owned()).collect();
            let mut message = format!(
                "Value \"{text}\" can not be converted to a DuckDB Type.\nPossible examples as suggestions: \n"
            );
            for suggestion in roastedduck_common::string_util::top_n_jaro_winkler(&candidates, text) {
                message.push_str("* ");
                message.push_str(&suggestion);
                message.push('\n');
            }
            return Err(Error::invalid_input(message));
        }
        Err(error) => return Err(error),
    };
    match context {
        Some(context) => roastedduck_planner::bind_logical_type(context, &column.logical_type),
        None => Err(Error::internal("type binding without a client context")),
    }
}

/// `StructureStringToType`: the type described by a JSON structure.
fn structure_string_to_type(
    doc: &Doc<'_>,
    val: ValId,
    context: Option<&roastedduck_planner::ClientContext>,
) -> Result<LogicalType> {
    match doc.get_type(val) {
        crate::yyjson::ValType::Arr => {
            if doc.len(val) != 1 {
                return Err(Error::binder("Too many values in array of JSON structure"));
            }
            let first = val + 1;
            Ok(LogicalType::list(structure_string_to_type(doc, first, context)?))
        }
        crate::yyjson::ValType::Obj => {
            let mut names = std::collections::HashSet::new();
            let mut children = Vec::new();
            for (key, value) in doc.obj_iter(val) {
                let name = String::from_utf8_lossy(doc.str(key)).into_owned();
                if !names.insert(name.clone()) {
                    return Err(Error::invalid_input(roastedduck_common::byte_format!(
                        "Duplicate keys in object in JSON structure: {}",
                        val_to_string(doc, value, None)
                    )));
                }
                children.push((name.into(), structure_string_to_type(doc, value, context)?));
            }
            if children.is_empty() {
                return Err(Error::binder("Empty object in JSON structure"));
            }
            Ok(LogicalType::struct_type(children))
        }
        crate::yyjson::ValType::Str => {
            let text = String::from_utf8_lossy(doc.str(val)).into_owned();
            string_to_logical_type(context, &text)
        }
        _ => Err(Error::binder("invalid JSON structure")),
    }
}

/// `JSONTransformBind`.
fn transform_bind(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    if input
        .argument_types
        .get(1)
        .is_some_and(|ty| ty.id() == LogicalTypeId::Unknown)
    {
        return Err(roastedduck_planner::expression_binder::parameter_not_resolved());
    }
    let Some(Some(structure)) = input.constants.get(1) else {
        return Err(Error::binder("JSON structure must be a constant!"));
    };
    let context = bind_context(input);
    if structure.is_null()
        || input
            .argument_types
            .get(1)
            .is_some_and(|ty| ty.id() == LogicalTypeId::SqlNull)
    {
        function.return_type = LogicalType::SQLNULL;
    } else {
        let Ok(text) = structure.to_varchar() else {
            return Err(Error::binder("Cannot cast JSON structure to string"));
        };
        let mut arena = JsonArena::default();
        let doc = read_document(&mut arena, text.as_bytes())?;
        function.return_type = structure_string_to_type(&arena.doc(), doc.root, context)?;
    }
    let casts = context.map_or_else(
        || Arc::new(CastFunctionSet::new()),
        |context| Arc::clone(&context.casts),
    );
    Ok(Some(Arc::new(TransformBindData { casts })))
}

/// `TransformFunctionInternal`: parses every row and converts the column.
fn transform_function_internal(
    input: &Vector,
    count: usize,
    target: &LogicalType,
    arena: &mut JsonArena,
    options: &mut TransformOptions,
) -> Result<(Vector, bool)> {
    let rows = if input.is_constant() { count.min(1) } else { count };
    let column = addressable(input, rows);
    let strings = StrInput::new(&column);
    arena.reset();
    let mut vals = Vec::with_capacity(rows);
    for row in 0..rows {
        match strings.get(row) {
            Some(json) => vals.push(Some(read_document(arena, json)?.root)),
            None => vals.push(None),
        }
    }
    let doc = arena.doc();
    let (mut result, success) = transform(&doc, &vals, target, options)?;
    for (row, val) in vals.iter().enumerate() {
        if val.is_none() {
            result.set_null(row);
        }
    }
    if input.is_constant() {
        result.make_constant();
    }
    Ok((result, success))
}

fn transform_rows(
    args: &DataChunk,
    state: &mut FunctionState,
    result: &mut Vector,
    strict: bool,
) -> Result<()> {
    let casts = state
        .bind_data
        .as_deref()
        .and_then(|data| data.as_any().downcast_ref::<TransformBindData>())
        .map_or_else(
            || Arc::new(CastFunctionSet::new()),
            |data| Arc::clone(&data.casts),
        );
    let target = result.logical_type().clone();
    let mut options = TransformOptions::new(strict, strict, strict, false, casts);
    let arena = local_arena(state);
    let (vector, success) =
        transform_function_internal(args.column(0), args.len(), &target, arena, &mut options)?;
    if !success {
        return Err(Error::new_bytes(
            roastedduck_common::ErrorKind::InvalidInput,
            options.error_message,
        ));
    }
    *result = vector;
    Ok(())
}

/// The `json_transform` overloads of `name`.
fn transform_set(name: &str, strict: bool) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    for input in input_types() {
        set.add(
            ScalarFunction::new(
                name,
                vec![input, LogicalType::VARCHAR],
                LogicalType::ANY,
                move |args, state, result| transform_rows(args, state, result, strict),
            )
            .with_bind(transform_bind),
        );
    }
    set
}

/// `JSONFunctions::GetTransformFunction`.
pub(crate) fn transform_function() -> ScalarFunctionSet {
    transform_set("json_transform", false)
}

/// `JSONFunctions::GetTransformStrictFunction`.
pub(crate) fn transform_strict_function() -> ScalarFunctionSet {
    transform_set("json_transform_strict", true)
}

/// `JSONToAnyCast`: JSON to any type, strict, with errors through the cast parameters.
pub(crate) fn json_to_any_cast(casts: &CastFunctionSet) -> BoundCastInfo {
    let casts = Arc::new(casts.clone());
    BoundCastInfo::new(move |source, result, count, params| {
        let mut options = TransformOptions::new(true, true, true, true, Arc::clone(&casts));
        options.delay_error = true;
        let target = result.logical_type().clone();
        let mut arena = JsonArena::default();
        let (vector, success) =
            transform_function_internal(source, count, &target, &mut arena, &mut options)?;
        *result = vector;
        if !success {
            params.assign_error_bytes(options.error_message)?;
        }
        Ok(success)
    })
}
