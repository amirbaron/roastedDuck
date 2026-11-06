//! VARIANT column storage (DuckDB's `VariantColumnData` with `variant_shredding.cpp` and
//! `variant_unshredding.cpp` of `storage/table/variant`, and the core `VariantShredding`
//! of `function/variant/variant_shredding.cpp`).
//!
//! Appends store a VARIANT column unshredded: a validity column and a STRUCT column with
//! the VARIANT layout ([`unshredded_type`]). A checkpoint of a large enough row group
//! shreds the column: the most common type of its values (and of the fields of OBJECTs
//! and the elements of ARRAYs, recursively) becomes a typed column, the "shredded" one,
//! which holds the values of that type; the other values stay in the unshredded column,
//! and the `untyped_value_index` of the shredded column points at them. Scans of a
//! shredded column put the values back together ([`unshred_rows`]).

use std::collections::{BTreeMap, HashMap};

use rustdb_common::libcxx::{UnorderedOrder, string_hash};
use rustdb_common::{ByteString, Error, Result};
use rustdb_types::variant::{
    VariantData, VariantLogicalType as V, VariantNormalizer, VariantPathComponent, unshredded_type,
};
use rustdb_types::variant_value::{VariantValue, to_variant_data};
use rustdb_types::variant_vector::VariantVectorBuilder;
use rustdb_types::{ListEntry, LogicalType, LogicalTypeId, PhysicalType, Value, Vector};

use crate::statistics::{TYPED_VALUE_INDEX, UNTYPED_VALUE_INDEX};

/// The settings that decide the shredding of VARIANT columns at checkpoints.
#[derive(Clone, Debug, PartialEq)]
pub struct VariantShreddingConfig {
    /// `variant_minimum_shredding_size`: the rows a row group needs before its VARIANT
    /// columns are shredded; -1 disables shredding.
    pub minimum_size: i64,
    /// `force_variant_shredding`: the shredded type to use instead of the one the values
    /// suggest (`DBConfigOptions::force_variant_shredding` without its unshredded part).
    pub forced_type: Option<LogicalType>,
}

impl Default for VariantShreddingConfig {
    fn default() -> Self {
        Self {
            minimum_size: 30_000,
            forced_type: None,
        }
    }
}

impl VariantShreddingConfig {
    /// `EnableShredding`: whether a row group of `count` rows is shredded.
    pub(crate) fn enables_shredding(&self, count: u64) -> bool {
        self.minimum_size >= 0 && count >= self.minimum_size as u64
    }
}

/// The number of [`V`] tags (`VariantLogicalType::ENUM_SIZE`).
const TYPE_COUNT: usize = V::Geometry as usize + 1;

/// The type statistics of one position of the values (`VariantColumnStatsData`): the root,
/// a field of OBJECTs or the elements of ARRAYs.
#[derive(Debug)]
struct ColumnStats {
    /// How often each type occurs.
    type_counts: [u64; TYPE_COUNT],
    decimal_width: u32,
    decimal_scale: u32,
    /// Whether every DECIMAL has the same width and scale.
    decimal_consistent: bool,
    total_count: u64,
    /// The fields (name and column index) in the iteration order of the
    /// `unordered_map<string, idx_t>` DuckDB keeps them in, which orders shredded fields.
    fields: UnorderedOrder<(ByteString, usize)>,
    /// The column of each field by name.
    field_columns: HashMap<ByteString, usize>,
    element: Option<usize>,
}

impl Default for ColumnStats {
    fn default() -> Self {
        Self {
            type_counts: [0; TYPE_COUNT],
            decimal_width: 0,
            decimal_scale: 0,
            decimal_consistent: false,
            total_count: 0,
            fields: UnorderedOrder::default(),
            field_columns: HashMap::new(),
            element: None,
        }
    }
}

/// The type analysis of the values of a VARIANT column (`VariantShreddingStats`).
#[derive(Debug)]
pub(crate) struct ShreddingAnalysis {
    /// The root first, then the fields and elements as they are found.
    columns: Vec<ColumnStats>,
}

impl ShreddingAnalysis {
    pub(crate) fn new() -> Self {
        Self {
            columns: vec![ColumnStats::default()],
        }
    }

    /// `VariantShreddingStats::Update` for one non-NULL value (NULL rows are not counted).
    pub(crate) fn update(&mut self, data: &VariantData) -> Result<()> {
        self.visit(data, 0, 0)
    }

    /// `VariantStatsVisitor`: counts the type of value `value_index` at `column`.
    fn visit(&mut self, data: &VariantData, value_index: u32, column: usize) -> Result<()> {
        let type_id = data.type_id(value_index)?;
        let stats = &mut self.columns[column];
        stats.type_counts[type_id as usize] += 1;
        stats.total_count += 1;
        match type_id {
            V::Decimal => {
                let (width, scale, _) = data.decimal(value_index)?;
                if stats.type_counts[V::Decimal as usize] == 1 {
                    stats.decimal_width = width;
                    stats.decimal_scale = scale;
                    stats.decimal_consistent = true;
                } else if stats.decimal_consistent
                    && (width != stats.decimal_width || scale != stats.decimal_scale)
                {
                    stats.decimal_consistent = false;
                }
            }
            V::Array => {
                let element = self.element(column);
                let nested = data.nested(value_index)?;
                for index in 0..nested.child_count {
                    self.visit(data, data.child(nested, index)?.values_index, element)?;
                }
            }
            V::Object => {
                let nested = data.nested(value_index)?;
                for index in 0..nested.child_count {
                    let child = data.child(nested, index)?;
                    let field = self.field(column, data.child_key(child)?);
                    self.visit(data, child.values_index, field)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// `GetOrCreateElement`.
    fn element(&mut self, parent: usize) -> usize {
        if let Some(element) = self.columns[parent].element {
            return element;
        }
        let element = self.columns.len();
        self.columns[parent].element = Some(element);
        self.columns.push(ColumnStats::default());
        element
    }

    /// `GetOrCreateField`.
    fn field(&mut self, parent: usize, name: &ByteString) -> usize {
        if let Some(&column) = self.columns[parent].field_columns.get(name) {
            return column;
        }
        let column = self.columns.len();
        let parent_stats = &mut self.columns[parent];
        parent_stats.field_columns.insert(name.to_owned(), column);
        parent_stats
            .fields
            .insert(string_hash(name.as_bytes()), (name.to_owned(), column), |a, b| {
                a.0 == b.0
            });
        self.columns.push(ColumnStats::default());
        column
    }

    /// `VariantShreddingStats::GetShreddedType`: the type to shred the values on, if the
    /// values suggest any.
    pub(crate) fn shredded_type(&self) -> Result<Option<LogicalType>> {
        self.shredded_type_of(0, None)
    }

    /// `GetShreddedTypeInternal`: the shredded type of `column`, whose parent OBJECTs number
    /// `parent_count`.
    fn shredded_type_of(&self, column: usize, parent_count: Option<u64>) -> Result<Option<LogicalType>> {
        let stats = &self.columns[column];
        if parent_count.is_some_and(|parent_count| stats.total_count > parent_count) {
            return Err(Error::internal(
                "Column count is larger than parent count - this should not be possible",
            ));
        }
        let total_value_count = parent_count.unwrap_or(stats.total_count);
        let null_count = stats.type_counts[V::VariantNull as usize];
        if null_count == stats.total_count {
            // All NULL: shred on INTEGER.
            return Ok(Some(wrap_shredded(
                LogicalType::INTEGER,
                null_count == total_value_count,
            )));
        }
        let mut max_count = 0;
        let mut type_index = 0;
        // VARIANT_NULL cannot be shredded on.
        for index in 1..TYPE_COUNT {
            if index == V::Decimal as usize && !stats.decimal_consistent {
                continue;
            }
            let count = stats.type_counts[index] + null_count;
            if max_count == 0 || count > max_count {
                max_count = count;
                type_index = index;
            }
        }
        if max_count == 0 {
            return Ok(None);
        }
        let fully_consistent = max_count == total_value_count;
        let type_id = V::from_u8(type_index as u8)?;
        let typed = match type_id {
            V::Object => {
                let mut fields = Vec::new();
                for position in stats.fields.order() {
                    let (name, field_column) = &stats.fields.keys()[position];
                    // Empty field names are not shredded.
                    if name.is_empty() {
                        continue;
                    }
                    if let Some(field_type) = self.shredded_type_of(*field_column, Some(total_value_count))? {
                        fields.push((name.clone(), field_type));
                    }
                }
                if fields.is_empty() {
                    return Ok(None);
                }
                // OBJECTs are never fully consistent.
                return Ok(Some(wrap_shredded(LogicalType::struct_type(fields), false)));
            }
            V::Array => {
                let element = stats
                    .element
                    .ok_or_else(|| Error::internal("ARRAY values without element statistics"))?;
                let Some(element_type) = self.shredded_type_of(element, None)? else {
                    return Ok(None);
                };
                LogicalType::list(element_type)
            }
            V::Decimal => LogicalType::decimal(stats.decimal_width as u8, stats.decimal_scale as u8),
            other => produce_shredded_type(other)?,
        };
        Ok(Some(wrap_shredded(typed, fully_consistent)))
    }
}

/// `ProduceShreddedType`: the DuckDB type of a primitive VARIANT type.
fn produce_shredded_type(type_id: V) -> Result<LogicalType> {
    Ok(match type_id {
        V::BoolTrue | V::BoolFalse => LogicalType::BOOLEAN,
        V::Int8 => LogicalType::TINYINT,
        V::Int16 => LogicalType::SMALLINT,
        V::Int32 => LogicalType::INTEGER,
        V::Int64 => LogicalType::BIGINT,
        V::Int128 => LogicalType::HUGEINT,
        V::Uint8 => LogicalType::UTINYINT,
        V::Uint16 => LogicalType::USMALLINT,
        V::Uint32 => LogicalType::UINTEGER,
        V::Uint64 => LogicalType::UBIGINT,
        V::Uint128 => LogicalType::UHUGEINT,
        V::Float => LogicalType::FLOAT,
        V::Double => LogicalType::DOUBLE,
        V::Varchar => LogicalType::VARCHAR,
        V::Blob => LogicalType::BLOB,
        V::Uuid => LogicalType::UUID,
        V::Date => LogicalType::DATE,
        V::TimeMicros => LogicalType::TIME,
        V::TimeNanos => LogicalType::simple(LogicalTypeId::TimeNs),
        V::TimestampSec => LogicalType::simple(LogicalTypeId::TimestampSec),
        V::TimestampMilis => LogicalType::simple(LogicalTypeId::TimestampMs),
        V::TimestampMicros => LogicalType::TIMESTAMP,
        V::TimestampNanos => LogicalType::TIMESTAMP_NS,
        V::TimeMicrosTz => LogicalType::simple(LogicalTypeId::TimeTz),
        V::TimestampMicrosTz => LogicalType::TIMESTAMP_TZ,
        V::Interval => LogicalType::INTERVAL,
        V::Bignum => LogicalType::BIGNUM,
        V::Bitstring => LogicalType::BIT,
        V::Geometry => LogicalType::GEOMETRY,
        V::Decimal => return Err(Error::internal("Can't shred on DECIMAL")),
        V::Object | V::Array => return Err(Error::internal("Already handled above")),
        V::VariantNull => {
            return Err(Error::not_implemented(format!(
                "Shredding on VariantLogicalType::{} not supported yet",
                type_id.name()
            )));
        }
    })
}

/// `SetShreddedType`: a fully consistent primitive level is the typed type itself; other
/// levels are `STRUCT(typed_value, [untyped_value_index])`.
fn wrap_shredded(typed: LogicalType, fully_consistent: bool) -> LogicalType {
    if fully_consistent && !typed.is_nested() {
        return typed;
    }
    let mut fields = vec![("typed_value".into(), typed)];
    if !fully_consistent {
        fields.push(("untyped_value_index".into(), LogicalType::UINTEGER));
    }
    LogicalType::struct_type(fields)
}

/// `GetVariantType`: the VARIANT types stored in a typed column of type `ty`.
fn variant_types(ty: &LogicalType) -> Result<Vec<V>> {
    use LogicalTypeId as L;
    Ok(match ty.id() {
        L::Any => Vec::new(),
        L::Struct => vec![V::Object],
        L::List => vec![V::Array],
        L::Boolean => vec![V::BoolTrue, V::BoolFalse],
        L::TinyInt => vec![V::Int8],
        L::SmallInt => vec![V::Int16],
        L::Integer => vec![V::Int32],
        L::BigInt => vec![V::Int64],
        L::HugeInt => vec![V::Int128],
        L::UTinyInt => vec![V::Uint8],
        L::USmallInt => vec![V::Uint16],
        L::UInteger => vec![V::Uint32],
        L::UBigInt => vec![V::Uint64],
        L::UHugeInt => vec![V::Uint128],
        L::Float => vec![V::Float],
        L::Double => vec![V::Double],
        L::Decimal => vec![V::Decimal],
        L::Date => vec![V::Date],
        L::Time => vec![V::TimeMicros],
        L::TimeTz => vec![V::TimeMicrosTz],
        L::TimestampTz => vec![V::TimestampMicrosTz],
        L::Timestamp => vec![V::TimestampMicros],
        L::TimestampSec => vec![V::TimestampSec],
        L::TimestampMs => vec![V::TimestampMilis],
        L::TimestampNs => vec![V::TimestampNanos],
        L::Blob => vec![V::Blob],
        L::Varchar => vec![V::Varchar],
        L::Uuid => vec![V::Uuid],
        L::Bignum => vec![V::Bignum],
        L::TimeNs => vec![V::TimeNanos],
        L::Interval => vec![V::Interval],
        L::Bit => vec![V::Bitstring],
        L::Geometry => vec![V::Geometry],
        _ => {
            return Err(Error::binder(format!(
                "Type '{ty}' can't be translated to a VARIANT type"
            )));
        }
    })
}

/// A value to write: its row, its index in the row's `values` and the row of the result
/// vector it goes to (the `sel`, `value_index_sel` and `result_sel` of DuckDB).
#[derive(Clone, Copy, Debug)]
struct Item {
    row: usize,
    value_index: u32,
    result: usize,
}

/// A vector of the shredded result under construction.
#[derive(Debug)]
struct OutVector {
    ty: LogicalType,
    valid: Vec<bool>,
    data: OutData,
}

#[derive(Debug)]
enum OutData {
    Values(Vec<Value>),
    /// The vectors of the fields.
    Struct(Vec<usize>),
    /// Offset and length per row, and the vector of the elements.
    List {
        entries: Vec<(usize, usize)>,
        child: usize,
    },
}

/// A value that stays unshredded: where it is in its row, where its index in the
/// unshredded VARIANT goes, and for a partially shredded OBJECT the positions of the
/// fields that are not shredded (`UnshreddedValue`).
#[derive(Debug)]
struct UnshreddedValue {
    source_value_index: u32,
    /// The vector and row of its `untyped_value_index`.
    target: (usize, usize),
    unshredded_children: Vec<u32>,
}

/// `DuckDBVariantShredding`: writes VARIANT values into a shredded vector, column by
/// column like DuckDB, which decides the order of the unshredded values of a row.
struct Shredder<'a> {
    rows: &'a [Option<VariantData>],
    /// The vectors of the result, the root first.
    vectors: Vec<OutVector>,
    /// Per row, the values that stay unshredded.
    unshredded_values: Vec<Vec<UnshreddedValue>>,
}

impl<'a> Shredder<'a> {
    fn new(rows: &'a [Option<VariantData>]) -> Self {
        Self {
            rows,
            vectors: Vec::new(),
            unshredded_values: (0..rows.len()).map(|_| Vec::new()).collect(),
        }
    }

    /// Allocates a (valid) vector of `ty` with `count` rows, and its children.
    fn allocate(&mut self, ty: &LogicalType, count: usize) -> usize {
        let data = match ty.id() {
            LogicalTypeId::Struct => OutData::Struct(
                ty.struct_children()
                    .unwrap_or_default()
                    .iter()
                    .map(|(_, field)| self.allocate(field, count))
                    .collect(),
            ),
            LogicalTypeId::List => OutData::List {
                entries: vec![(0, 0); count],
                child: self.allocate(ty.child_type().unwrap_or(&LogicalType::INVALID), 0),
            },
            _ => OutData::Values(vec![Value::null(ty.clone()); count]),
        };
        self.vectors.push(OutVector {
            ty: ty.clone(),
            valid: vec![true; count],
            data,
        });
        self.vectors.len() - 1
    }

    /// Makes vector `vector` (and its fields) `count` rows long (`ListVector::Reserve`).
    fn resize(&mut self, vector: usize, count: usize) {
        let target = &mut self.vectors[vector];
        target.valid.resize(count, true);
        let fields = match &mut target.data {
            OutData::Values(values) => {
                values.resize(count, Value::null(target.ty.clone()));
                return;
            }
            OutData::List { entries, .. } => {
                entries.resize(count, (0, 0));
                return;
            }
            OutData::Struct(fields) => fields.clone(),
        };
        for field in fields {
            self.resize(field, count);
        }
    }

    fn fields(&self, vector: usize) -> &[usize] {
        match &self.vectors[vector].data {
            OutData::Struct(fields) => fields,
            _ => &[],
        }
    }

    /// `FlatVector::SetNull`: a NULL STRUCT has NULL fields.
    fn set_null(&mut self, vector: usize, row: usize) {
        self.vectors[vector].valid[row] = false;
        for index in 0..self.fields(vector).len() {
            let field = self.fields(vector)[index];
            self.set_null(field, row);
        }
    }

    fn set_value(&mut self, vector: usize, row: usize, value: Value) -> Result<()> {
        let target = &mut self.vectors[vector];
        let OutData::Values(values) = &mut target.data else {
            return Err(Error::internal("writing a primitive value to a nested vector"));
        };
        values[row] = value;
        target.valid[row] = true;
        Ok(())
    }

    /// The typed and untyped vectors of a shredded level: `STRUCT(typed_value,
    /// [untyped_value_index])`, or a primitive vector of typed values.
    fn split_level(&self, vector: usize) -> (usize, Option<usize>) {
        if self.vectors[vector].ty.id() != LogicalTypeId::Struct {
            return (vector, None);
        }
        let fields = self.fields(vector);
        (
            fields[TYPED_VALUE_INDEX],
            fields.get(UNTYPED_VALUE_INDEX).copied(),
        )
    }

    fn row(&self, row: usize) -> Result<&'a VariantData> {
        self.rows[row]
            .as_ref()
            .ok_or_else(|| Error::internal("shredding a NULL VARIANT value"))
    }

    /// `DuckDBVariantShredding::WriteVariantValues`: writes `items` into the shredded
    /// level `result`, noting the values that stay unshredded.
    fn write_variant_values(&mut self, result: usize, items: &[Item]) -> Result<()> {
        let (typed, untyped) = self.split_level(result);
        let typed_type = self.vectors[typed].ty.clone();
        let types = variant_types(&typed_type)?;
        // `AnalyzeVariantValues`.
        let mut shredded = Vec::with_capacity(items.len());
        for &item in items {
            let row = self.rows[item.row].as_ref();
            if let Some(data) = row
                && value_is_shredded(data, item.value_index, &typed_type, &types)?
            {
                shredded.push(item);
                if typed_type.id() != LogicalTypeId::Struct {
                    if let Some(untyped) = untyped {
                        self.vectors[untyped].valid[item.result] = false;
                    }
                    continue;
                }
                // The fields an OBJECT is not shredded on stay unshredded.
                let unshredded_children = unshredded_object_children(data, item.value_index, &typed_type)?;
                if unshredded_children.is_empty() {
                    if let Some(untyped) = untyped {
                        self.vectors[untyped].valid[item.result] = false;
                    }
                } else {
                    let untyped = untyped.ok_or_else(inconsistent_values)?;
                    self.unshredded_values[item.row].push(UnshreddedValue {
                        source_value_index: item.value_index,
                        target: (untyped, item.result),
                        unshredded_children,
                    });
                }
                continue;
            }
            // NULL rows and VARIANT NULLs have a NULL `untyped_value_index`.
            let is_null = match row {
                None => true,
                Some(data) => data.type_id(item.value_index)? == V::VariantNull,
            };
            if is_null {
                if let Some(untyped) = untyped {
                    self.vectors[untyped].valid[item.result] = false;
                }
            } else {
                let untyped = untyped.ok_or_else(inconsistent_values)?;
                self.unshredded_values[item.row].push(UnshreddedValue {
                    source_value_index: item.value_index,
                    target: (untyped, item.result),
                    unshredded_children: Vec::new(),
                });
            }
        }
        if !shredded.is_empty() {
            self.write_typed_values(typed, &shredded)?;
        }
        // The rows that are not shredded have a NULL typed value.
        let mut next = 0;
        for item in items {
            if shredded
                .get(next)
                .is_some_and(|shredded| shredded.result == item.result)
            {
                next += 1;
                continue;
            }
            self.set_null(typed, item.result);
        }
        Ok(())
    }

    /// `VariantShredding::WriteTypedValues`.
    fn write_typed_values(&mut self, typed: usize, items: &[Item]) -> Result<()> {
        match self.vectors[typed].ty.id() {
            LogicalTypeId::Struct => self.write_typed_object_values(typed, items),
            LogicalTypeId::List => self.write_typed_array_values(typed, items),
            _ => self.write_typed_primitive_values(typed, items),
        }
    }

    /// `WriteTypedPrimitiveValues`: the payload of each value as the typed type (decimals
    /// keep their unscaled value).
    fn write_typed_primitive_values(&mut self, typed: usize, items: &[Item]) -> Result<()> {
        let ty = self.vectors[typed].ty.clone();
        for item in items {
            let data = self.row(item.row)?;
            let value = match data.type_id(item.value_index)? {
                V::BoolTrue => Value::boolean(true),
                V::BoolFalse => Value::boolean(false),
                _ => data.to_sql_value(item.value_index)?.reinterpret(ty.clone()),
            };
            self.set_value(typed, item.result, value)?;
        }
        Ok(())
    }

    /// `WriteTypedObjectValues`: each shredded field of the OBJECTs, by exact name; a
    /// missing field has a NULL typed value and an `untyped_value_index` of 0.
    fn write_typed_object_values(&mut self, typed: usize, items: &[Item]) -> Result<()> {
        let field_names: Vec<ByteString> = self.vectors[typed]
            .ty
            .struct_children()
            .unwrap_or_default()
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        let field_vectors = self.fields(typed).to_vec();
        let mut nested = Vec::with_capacity(items.len());
        for item in items {
            nested.push(self.row(item.row)?.nested(item.value_index)?);
        }
        for (name, field) in field_names.iter().zip(field_vectors) {
            // `FindChildValues` by key.
            let mut found = Vec::with_capacity(items.len());
            let mut any_missing = false;
            for (item, nested) in items.iter().zip(&nested) {
                let data = self.row(item.row)?;
                let mut child_value = None;
                for index in 0..nested.child_count {
                    let child = data.child(*nested, index)?;
                    if data.child_key(child)? == name {
                        child_value = Some(child.values_index);
                        break;
                    }
                }
                any_missing |= child_value.is_none();
                found.push(child_value);
            }
            if !any_missing {
                let children: Vec<Item> = items
                    .iter()
                    .zip(&found)
                    .map(|(item, child)| Item {
                        value_index: child.unwrap_or_default(),
                        ..*item
                    })
                    .collect();
                self.write_variant_values(field, &children)?;
                continue;
            }
            let (field_typed, field_untyped) = self.split_level(field);
            let mut present = Vec::with_capacity(items.len());
            for (item, child) in items.iter().zip(&found) {
                let Some(value_index) = child else {
                    // The field is missing.
                    self.set_null(field_typed, item.result);
                    let untyped = field_untyped.ok_or_else(|| {
                        Error::internal("Field is missing but untyped_value_index is not set")
                    })?;
                    self.set_value(untyped, item.result, Value::uinteger(0))?;
                    continue;
                };
                present.push(Item {
                    value_index: *value_index,
                    ..*item
                });
            }
            if !present.is_empty() {
                self.write_variant_values(field, &present)?;
            }
        }
        Ok(())
    }

    /// `WriteTypedArrayValues`: the elements of all ARRAYs go to the list's child vector.
    fn write_typed_array_values(&mut self, typed: usize, items: &[Item]) -> Result<()> {
        let mut total = 0;
        let mut entries = Vec::with_capacity(items.len());
        for item in items {
            let nested = self.row(item.row)?.nested(item.value_index)?;
            entries.push((item.result, total, nested));
            total += nested.child_count as usize;
        }
        let child = match &mut self.vectors[typed].data {
            OutData::List {
                entries: list_entries,
                child,
            } => {
                for (result, offset, nested) in &entries {
                    list_entries[*result] = (*offset, nested.child_count as usize);
                }
                *child
            }
            _ => return Err(Error::internal("writing ARRAY values to a non-list vector")),
        };
        self.resize(child, total);
        let mut children = Vec::with_capacity(total);
        for (item, (_, offset, nested)) in items.iter().zip(&entries) {
            let data = self.row(item.row)?;
            for index in 0..nested.child_count {
                children.push(Item {
                    row: item.row,
                    value_index: data.child(*nested, index)?.values_index,
                    result: offset + index as usize,
                });
            }
        }
        self.write_variant_values(child, &children)
    }

    /// Vector `vector` as a flat vector.
    fn build_vector(&self, vector: usize) -> Result<Vector> {
        let target = &self.vectors[vector];
        let rows = target.valid.len();
        let mut result = Vector::flat(target.ty.clone(), rows.max(1));
        match &target.data {
            OutData::Values(values) => {
                for (row, value) in values.iter().enumerate() {
                    if target.valid[row] {
                        result.set_value(row, value)?;
                    }
                }
            }
            OutData::Struct(fields) => {
                let built = fields
                    .iter()
                    .map(|&field| self.build_vector(field))
                    .collect::<Result<Vec<_>>>()?;
                if let Some(children) = result.struct_children_mut() {
                    for (child, field) in children.iter_mut().zip(built) {
                        *child = field;
                    }
                }
            }
            OutData::List { entries, child } => {
                for (entry, &(offset, length)) in result.values_mut::<ListEntry>().iter_mut().zip(entries) {
                    *entry = ListEntry::new(offset, length);
                }
                result.set_list_child(self.build_vector(*child)?, self.vectors[*child].valid.len());
            }
        }
        for (row, valid) in target.valid.iter().enumerate() {
            if !valid {
                result.validity_mut().set_invalid(row, rows.max(1));
            }
        }
        Ok(result)
    }
}

fn inconsistent_values() -> Error {
    Error::invalid_input(
        "Failed to shred variant value - untyped_value was not set but inconsistent values were found",
    )
}

/// `VariantShreddingState::ValueIsShredded`: whether value `value_index` has a type of the
/// typed column (a DECIMAL also needs its physical type).
fn value_is_shredded(
    data: &VariantData,
    value_index: u32,
    typed_type: &LogicalType,
    types: &[V],
) -> Result<bool> {
    let type_id = data.type_id(value_index)?;
    if !types.contains(&type_id) {
        return Ok(false);
    }
    if type_id == V::Decimal {
        let (width, _, _) = data.decimal(value_index)?;
        let physical = match width {
            0..=4 => PhysicalType::Int16,
            5..=9 => PhysicalType::Int32,
            10..=18 => PhysicalType::Int64,
            _ => PhysicalType::Int128,
        };
        return Ok(physical == typed_type.physical_type());
    }
    Ok(true)
}

/// `UnshreddedObjectChildren`: the positions of the fields of an OBJECT that the shredded
/// STRUCT `typed_type` has no field for (case-insensitively).
fn unshredded_object_children(
    data: &VariantData,
    value_index: u32,
    typed_type: &LogicalType,
) -> Result<Vec<u32>> {
    let shredded_fields = typed_type.struct_children().unwrap_or_default();
    let nested = data.nested(value_index)?;
    let mut result = Vec::new();
    for index in 0..nested.child_count {
        let key = data.child_key(data.child(nested, index)?)?;
        if !shredded_fields
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(key))
        {
            result.push(index);
        }
    }
    Ok(result)
}

/// `VariantColumnData::ShredVariantData`: the unshredded and the shredded (of
/// `shredded_type`) vector of VARIANT rows (`None` for NULL rows).
pub(crate) fn shred_rows(
    rows: &[Option<VariantData>],
    shredded_type: &LogicalType,
) -> Result<(Vector, Vector)> {
    let mut shredder = Shredder::new(rows);
    let shredded = shredder.allocate(shredded_type, rows.len());
    let items: Vec<Item> = (0..rows.len())
        .map(|row| Item {
            row,
            value_index: 0,
            result: row,
        })
        .collect();
    shredder.write_variant_values(shredded, &items)?;
    // Now the unshredded values: each row's in one VARIANT, in the order they were found.
    let mut unshredded = Vec::with_capacity(rows.len());
    for row in 0..rows.len() {
        let values = std::mem::take(&mut shredder.unshredded_values[row]);
        if values.is_empty() {
            unshredded.push(None);
            continue;
        }
        let data = shredder.row(row)?;
        let mut normalizer = VariantNormalizer::default();
        for value in values {
            let target = u32::try_from(normalizer.value_count() + 1)
                .map_err(|_| Error::internal("VARIANT exceeds 4 GiB"))?;
            let (vector, result) = value.target;
            shredder.set_value(vector, result, Value::uinteger(target))?;
            if value.unshredded_children.is_empty() {
                normalizer.visit(data, value.source_value_index)?;
            } else {
                normalizer.visit_object_fields(data, value.source_value_index, &value.unshredded_children)?;
            }
        }
        unshredded.push(Some(normalizer.finish()));
    }
    Ok((
        VariantData::write_vector(&unshredded, &unshredded_type())?,
        shredder.build_vector(shredded)?,
    ))
}

/// `VariantColumnData::UnshredVariantData`: a vector of type `ty` (VARIANT) with the
/// values of `count` rows of an unshredded and a shredded vector.
///
/// Rows of an OBJECT shredded on primitive fields with no unshredded values are written
/// straight into the VARIANT layout ([`ShreddedObject`]); the others go through
/// [`VariantValue`]s as in DuckDB (`Unshred` + `VariantValue::ToVARIANT`).
pub(crate) fn unshred_rows(
    unshredded: &Vector,
    shredded: &Vector,
    count: usize,
    ty: &LogicalType,
) -> Result<Vector> {
    let Some(object) = ShreddedObject::new(shredded) else {
        let mut values = Vec::with_capacity(count);
        for row in 0..count {
            let variant = VariantData::from_vector(unshredded, row)?;
            values.push(unshred(variant.as_ref(), shredded, row)?);
        }
        return VariantData::write_vector(&to_variant_data(&values)?, ty);
    };
    let mut builder = VariantVectorBuilder::with_capacity(count);
    for row in 0..count {
        if object.write_row(&mut builder, row)? {
            continue;
        }
        let variant = VariantData::from_vector(unshredded, row)?;
        let value = unshred(variant.as_ref(), shredded, row)?;
        let data = to_variant_data(std::slice::from_ref(&value))?;
        builder.push_data(data.first().and_then(Option::as_ref));
    }
    Ok(builder.finish(ty))
}

/// A shredded OBJECT level whose fields are shredded on primitive types, fully
/// consistently (their typed vectors are the field levels themselves): `Unshred` of a
/// row without unshredded values is the OBJECT of the fields' values, which
/// [`ShreddedObject::write_row`] writes the way `VariantValue::ToVARIANT` lays it out.
struct ShreddedObject<'a> {
    level: &'a Vector,
    typed: &'a Vector,
    untyped: Option<&'a Vector>,
    /// The fields in the order of their names (an OBJECT's `BTreeMap`), with the variant
    /// type of their values.
    fields: Vec<(&'a ByteString, &'a Vector, V)>,
}

impl<'a> ShreddedObject<'a> {
    fn new(level: &'a Vector) -> Option<Self> {
        if level.logical_type().id() != LogicalTypeId::Struct {
            return None;
        }
        let level_fields = level.struct_children()?;
        let typed = level_fields.get(TYPED_VALUE_INDEX)?;
        let names = typed.logical_type().struct_children()?;
        if typed.logical_type().id() != LogicalTypeId::Struct || names.is_empty() {
            return None;
        }
        let vectors = typed.struct_children()?;
        let mut fields = Vec::with_capacity(names.len());
        for ((name, field_type), vector) in names.iter().zip(vectors) {
            fields.push((name, vector, primitive_variant_type(field_type)?));
        }
        fields.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        if fields.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return None;
        }
        Some(Self {
            level,
            typed,
            untyped: level_fields.get(UNTYPED_VALUE_INDEX),
            fields,
        })
    }

    /// Writes row `row` if it has no unshredded value; `false` otherwise.
    fn write_row(&self, builder: &mut VariantVectorBuilder, row: usize) -> Result<bool> {
        let slot = self.level.sel().slot(row);
        if self.untyped.is_some_and(|untyped| !untyped.is_null(slot)) {
            return Ok(false);
        }
        if self.typed.is_null(slot) {
            // A NULL object and no unshredded value: NULL.
            builder.push_null();
            return Ok(true);
        }
        let typed_slot = self.typed.sel().slot(slot);
        builder.start_row();
        builder.push_value(V::Object)?;
        let count = self.fields.len();
        let data = builder.data_mut();
        rustdb_types::variant::varint_encode(count as u64, data);
        rustdb_types::variant::varint_encode(0, data);
        for (index, (name, _, _)) in self.fields.iter().enumerate() {
            builder.push_key(name.as_bytes());
            builder.push_child(Some(index as u32), index as u32 + 1);
        }
        for &(_, vector, type_id) in &self.fields {
            if vector.is_null(typed_slot) {
                builder.push_value(V::VariantNull)?;
                continue;
            }
            write_primitive(builder, vector, typed_slot, type_id)?;
        }
        builder.finish_row();
        Ok(true)
    }
}

/// The variant type of the values of a primitive column of type `ty` that
/// [`write_primitive`] writes (`ConvertPrimitive`'s type tags); `None` for the others.
fn primitive_variant_type(ty: &LogicalType) -> Option<V> {
    use LogicalTypeId as L;
    Some(match ty.id() {
        L::Boolean => V::BoolTrue,
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
        L::Time => V::TimeMicros,
        L::TimeNs => V::TimeNanos,
        L::Timestamp => V::TimestampMicros,
        L::TimestampSec => V::TimestampSec,
        L::TimestampMs => V::TimestampMilis,
        L::TimestampNs => V::TimestampNanos,
        L::TimestampTz => V::TimestampMicrosTz,
        L::Decimal => V::Decimal,
        L::Varchar if ty.alias().is_none() => V::Varchar,
        L::Blob => V::Blob,
        _ => return None,
    })
}

/// Appends the non-NULL value at `slot` of a primitive column to the current row, like
/// `ConvertPrimitive` does with its `Value`.
fn write_primitive(
    builder: &mut VariantVectorBuilder,
    vector: &Vector,
    slot: usize,
    type_id: V,
) -> Result<()> {
    let slot = vector.sel().slot(slot);
    match type_id {
        V::BoolTrue => {
            let flag = vector.values::<bool>()[slot];
            builder.push_value(if flag { V::BoolTrue } else { V::BoolFalse })?;
        }
        V::Int8 => push_fixed(builder, type_id, &vector.values::<i8>()[slot].to_le_bytes())?,
        V::Int16 => push_fixed(builder, type_id, &vector.values::<i16>()[slot].to_le_bytes())?,
        V::Int32 | V::Date => push_fixed(builder, type_id, &vector.values::<i32>()[slot].to_le_bytes())?,
        V::Int64
        | V::TimeMicros
        | V::TimeNanos
        | V::TimestampMicros
        | V::TimestampSec
        | V::TimestampMilis
        | V::TimestampNanos
        | V::TimestampMicrosTz => push_fixed(builder, type_id, &vector.values::<i64>()[slot].to_le_bytes())?,
        V::Int128 => push_fixed(builder, type_id, &vector.values::<i128>()[slot].to_le_bytes())?,
        V::Uint8 => push_fixed(builder, type_id, &vector.values::<u8>()[slot].to_le_bytes())?,
        V::Uint16 => push_fixed(builder, type_id, &vector.values::<u16>()[slot].to_le_bytes())?,
        V::Uint32 => push_fixed(builder, type_id, &vector.values::<u32>()[slot].to_le_bytes())?,
        V::Uint64 => push_fixed(builder, type_id, &vector.values::<u64>()[slot].to_le_bytes())?,
        V::Uint128 => push_fixed(builder, type_id, &vector.values::<u128>()[slot].to_le_bytes())?,
        V::Float => push_fixed(builder, type_id, &vector.values::<f32>()[slot].to_le_bytes())?,
        V::Double => push_fixed(builder, type_id, &vector.values::<f64>()[slot].to_le_bytes())?,
        V::Decimal => {
            let ty = vector.logical_type();
            let (width, scale) = ty
                .decimal_properties()
                .ok_or_else(|| Error::internal("DECIMAL without width and scale"))?;
            builder.push_value(V::Decimal)?;
            let data = builder.data_mut();
            rustdb_types::variant::varint_encode(u64::from(width), data);
            rustdb_types::variant::varint_encode(u64::from(scale), data);
            match ty.physical_type() {
                PhysicalType::Int16 => data.extend_from_slice(&vector.values::<i16>()[slot].to_le_bytes()),
                PhysicalType::Int32 => data.extend_from_slice(&vector.values::<i32>()[slot].to_le_bytes()),
                PhysicalType::Int64 => data.extend_from_slice(&vector.values::<i64>()[slot].to_le_bytes()),
                _ => data.extend_from_slice(&vector.values::<i128>()[slot].to_le_bytes()),
            }
        }
        V::Varchar | V::Blob => {
            let value = &vector.values::<rustdb_types::StringT>()[slot];
            let bytes = vector.string_value(value);
            builder.push_value(type_id)?;
            let data = builder.data_mut();
            rustdb_types::variant::varint_encode(bytes.len() as u64, data);
            data.extend_from_slice(bytes);
        }
        other => {
            return Err(Error::internal(format!(
                "VARIANT({}) written from a primitive column",
                other.name()
            )));
        }
    }
    Ok(())
}

/// Appends a value with a fixed-size payload to the current row.
fn push_fixed(builder: &mut VariantVectorBuilder, type_id: V, payload: &[u8]) -> Result<()> {
    builder.push_value(type_id)?;
    builder.data_mut().extend_from_slice(payload);
    Ok(())
}

/// `VariantUtils::VariantExtract` along a path of OBJECT fields: each row's value rooted
/// at the field; NULL where a level is not an OBJECT, lacks the field, or the field is a
/// VARIANT NULL.
pub(crate) fn extract_path<S: AsRef<[u8]>>(input: &Vector, path: &[S], count: usize) -> Result<Vector> {
    let components: Vec<VariantPathComponent> = path
        .iter()
        .map(|key| VariantPathComponent::Key(ByteString::from(key.as_ref())))
        .collect();
    rustdb_types::variant_vector::extract(input, &components, count)
}

/// `Unshred` for row `row` of a shredded level.
fn unshred(variant: Option<&VariantData>, level: &Vector, row: usize) -> Result<VariantValue> {
    let (typed, typed_row, untyped) = if level.logical_type().id() == LogicalTypeId::Struct {
        // "typed_value", "untyped_value_index": the fields of the level's row.
        let slot = level.sel().slot(row);
        let fields = level
            .struct_children()
            .ok_or_else(|| Error::internal("shredded level without its fields"))?;
        (
            &fields[TYPED_VALUE_INDEX],
            slot,
            fields.get(UNTYPED_VALUE_INDEX).map(|untyped| (untyped, slot)),
        )
    } else {
        (level, row, None)
    };
    let mut result = unshred_typed_value(variant, typed, typed_row)?;
    let Some((untyped, untyped_row)) =
        untyped.filter(|(untyped, untyped_row)| !untyped.is_null(*untyped_row))
    else {
        // A NULL `untyped_value_index`: fully shredded.
        return Ok(result);
    };
    let index = untyped.values::<u32>()[untyped.sel().slot(untyped_row)];
    if index == 0 {
        return Ok(VariantValue::Missing);
    }
    let unshredded = unshredded_value(variant, index)?;
    if result.is_null() {
        // Not shredded.
        return Ok(unshredded);
    }
    // A partially shredded OBJECT: its unshredded fields join the shredded ones (an OBJECT
    // all of whose shredded fields are missing stays missing, like DuckDB's).
    if let (VariantValue::Object(fields), VariantValue::Object(children)) = (&mut result, unshredded) {
        for (key, value) in children {
            VariantValue::add_child(fields, &key, value);
        }
    }
    Ok(result)
}

/// `UnshredTypedValue` for row `row` of a typed vector.
fn unshred_typed_value(variant: Option<&VariantData>, typed: &Vector, row: usize) -> Result<VariantValue> {
    if typed.is_null(row) {
        return Ok(VariantValue::null());
    }
    let slot = typed.sel().slot(row);
    match typed.logical_type().id() {
        LogicalTypeId::Struct => {
            // `UnshredTypedObject`: an OBJECT of the fields that are not missing.
            let names = typed.logical_type().struct_children().unwrap_or_default();
            let fields = typed.struct_children().unwrap_or_default();
            let mut result = VariantValue::Missing;
            for ((name, _), field) in names.iter().zip(fields) {
                let value = unshred(variant, field, slot)?;
                if matches!(value, VariantValue::Missing) {
                    continue;
                }
                if matches!(result, VariantValue::Missing) {
                    result = VariantValue::Object(BTreeMap::new());
                }
                if let VariantValue::Object(object) = &mut result {
                    VariantValue::add_child(object, name, value);
                }
            }
            Ok(result)
        }
        LogicalTypeId::List => {
            // `UnshredTypedArray`.
            let (elements, _) = typed
                .list_child()
                .ok_or_else(|| Error::internal("shredded ARRAY without its elements"))?;
            let items = typed.values::<ListEntry>()[slot]
                .range()
                .map(|element| unshred(variant, elements, element))
                .collect::<Result<Vec<_>>>()?;
            Ok(VariantValue::array(items))
        }
        _ => Ok(VariantValue::Primitive(typed.get_value(row))),
    }
}

/// `UnshreddedVariantValue`: value `values_index - 1` of the unshredded VARIANT of a row
/// (0 is a missing value).
fn unshredded_value(variant: Option<&VariantData>, values_index: u32) -> Result<VariantValue> {
    let Some(data) = variant else {
        return Ok(VariantValue::null());
    };
    if values_index == 0 {
        return Ok(VariantValue::Missing);
    }
    let index = values_index - 1;
    match data.type_id(index)? {
        V::Object => {
            let nested = data.nested(index)?;
            let mut fields = BTreeMap::new();
            for position in 0..nested.child_count {
                let child = data.child(nested, position)?;
                let value = unshredded_value(variant, child.values_index + 1)?;
                VariantValue::add_child(&mut fields, data.child_key(child)?, value);
            }
            Ok(VariantValue::Object(fields))
        }
        V::Array => {
            let nested = data.nested(index)?;
            let items = (0..nested.child_count)
                .map(|position| unshredded_value(variant, data.child(nested, position)?.values_index + 1))
                .collect::<Result<Vec<_>>>()?;
            Ok(VariantValue::array(items))
        }
        _ => Ok(VariantValue::Primitive(data.to_sql_value(index)?)),
    }
}

#[cfg(test)]
mod tests {
    use rustdb_types::variant::encode_value;

    use super::*;

    fn variant(value: &Value) -> Option<VariantData> {
        encode_value(value)
            .unwrap()
            .map(|encoded| VariantData::from_value(&encoded).unwrap().unwrap())
    }

    /// Shreds `rows` on the type their analysis suggests and unshreds them again.
    fn roundtrip(rows: &[Option<VariantData>]) -> (LogicalType, Vec<Option<VariantData>>) {
        let mut analysis = ShreddingAnalysis::new();
        for data in rows.iter().flatten() {
            analysis.update(data).unwrap();
        }
        let shredded_type = analysis.shredded_type().unwrap().unwrap();
        let values = roundtrip_on(rows, &shredded_type);
        (shredded_type, values)
    }

    /// Shreds `rows` on `shredded_type` and unshreds them again.
    fn roundtrip_on(rows: &[Option<VariantData>], shredded_type: &LogicalType) -> Vec<Option<VariantData>> {
        let (unshredded, shredded) = shred_rows(rows, shredded_type).unwrap();
        let result = unshred_rows(&unshredded, &shredded, rows.len(), &LogicalType::variant()).unwrap();
        (0..rows.len())
            .map(|row| VariantData::from_vector(&result, row).unwrap())
            .collect()
    }

    fn object(fields: Vec<(&str, Value)>) -> Value {
        Value::struct_value(
            fields
                .into_iter()
                .map(|(name, value)| (ByteString::from(name), value))
                .collect(),
        )
    }

    #[test]
    fn shredding_roundtrips_mixed_values() {
        let rows = vec![
            variant(&object(vec![
                ("a", Value::integer(1)),
                ("b", Value::varchar("x")),
            ])),
            variant(&object(vec![
                ("a", Value::varchar("not an int")),
                ("c", Value::boolean(true)),
            ])),
            None,
            variant(&Value::integer(42)),
            variant(&object(vec![
                ("a", Value::integer(2)),
                ("b", Value::varchar("y")),
            ])),
            variant(&Value::list(
                LogicalType::INTEGER,
                vec![Value::integer(1), Value::null(LogicalType::INTEGER)],
            )),
        ];
        let (shredded_type, values) = roundtrip(&rows);
        // The fields come in the order of DuckDB's `unordered_map` (checked against DuckDB).
        assert_eq!(
            shredded_type.to_string(),
            "STRUCT(typed_value STRUCT(b STRUCT(typed_value VARCHAR, untyped_value_index UINTEGER), \
             c STRUCT(typed_value BOOLEAN, untyped_value_index UINTEGER), \
             a STRUCT(typed_value INTEGER, untyped_value_index UINTEGER)), untyped_value_index UINTEGER)"
        );
        for (row, value) in rows.iter().zip(&values) {
            let normalized = row.as_ref().map(|data| data.normalize().unwrap());
            assert_eq!(value, &normalized);
        }
    }

    #[test]
    fn objects_without_shredded_fields_are_lost_like_in_duckdb() {
        // {'b': 1} shredded on STRUCT(a INTEGER) has unshredded fields only; DuckDB's
        // unshredding returns NULL for it.
        let rows = vec![
            variant(&object(vec![("a", Value::integer(2))])),
            variant(&object(vec![("b", Value::integer(1))])),
        ];
        let shredding = crate::statistics::wrap_shredded_levels(&LogicalType::struct_type(vec![(
            "a".into(),
            LogicalType::INTEGER,
        )]));
        let values = roundtrip_on(&rows, &shredding);
        assert_eq!(values[0], rows[0]);
        assert_eq!(values[1], None);
    }
}
