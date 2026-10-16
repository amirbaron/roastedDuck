//! The metadata table functions (DuckDB's `parquet_metadata.cpp`): `parquet_metadata`,
//! `parquet_schema`, `parquet_kv_metadata`, `parquet_file_metadata`,
//! `parquet_bloom_probe` and `parquet_full_metadata`.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_csv::multi_file::{FileList, glob_file_list, parse_paths};
use rustdb_function::cast::{CastFunctionSet, ValueCast as _};
use rustdb_function::{
    FunctionData, TableFunction, TableFunctionBindOutput, TableFunctionSet, TableScanState,
};
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

use crate::reader::ParquetReader;
use crate::schema::{ColumnSchema, ParquetOptions, derive_logical_type};
use crate::statistics::convert_value;
use crate::thrift::{
    ColumnOrder, EncryptionAlgorithm, FileMetaData, LogicalType as ParquetLogicalType, TimeUnit,
};
use crate::writer::bloom::BloomFilter;

/// Which metadata a function returns (`ParquetMetadataOperatorType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetadataKind {
    RowGroups,
    Schema,
    KeyValue,
    File,
    BloomProbe,
    Full,
}

/// The bind data of a metadata function (`ParquetMetaDataBindData`).
#[derive(Debug)]
struct MetadataBindData {
    kind: MetadataKind,
    files: FileList,
    options: ParquetOptions,
    casts: Arc<CastFunctionSet>,
    /// `parquet_bloom_probe`'s column and value.
    probe: Option<(String, Value)>,
}

impl FunctionData for MetadataBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn names_types(columns: &[(&str, LogicalType)]) -> (Vec<String>, Vec<LogicalType>) {
    columns
        .iter()
        .map(|(name, ty)| ((*name).to_owned(), ty.clone()))
        .unzip()
}

fn geo_bbox_type() -> LogicalType {
    LogicalType::struct_type(
        ["xmin", "xmax", "ymin", "ymax", "zmin", "zmax", "mmin", "mmax"]
            .into_iter()
            .map(|name| (name.to_owned(), LogicalType::DOUBLE))
            .collect(),
    )
}

/// The columns of each function (`BindSchema`).
fn schema_of(kind: MetadataKind) -> Vec<(&'static str, LogicalType)> {
    use LogicalType as T;
    match kind {
        MetadataKind::RowGroups => vec![
            ("file_name", T::VARCHAR),
            ("row_group_id", T::BIGINT),
            ("row_group_num_rows", T::BIGINT),
            ("row_group_num_columns", T::BIGINT),
            ("row_group_bytes", T::BIGINT),
            ("column_id", T::BIGINT),
            ("file_offset", T::BIGINT),
            ("num_values", T::BIGINT),
            ("path_in_schema", T::VARCHAR),
            ("type", T::VARCHAR),
            ("stats_min", T::VARCHAR),
            ("stats_max", T::VARCHAR),
            ("stats_null_count", T::BIGINT),
            ("stats_distinct_count", T::BIGINT),
            ("stats_min_value", T::VARCHAR),
            ("stats_max_value", T::VARCHAR),
            ("compression", T::VARCHAR),
            ("encodings", T::VARCHAR),
            ("index_page_offset", T::BIGINT),
            ("dictionary_page_offset", T::BIGINT),
            ("data_page_offset", T::BIGINT),
            ("total_compressed_size", T::BIGINT),
            ("total_uncompressed_size", T::BIGINT),
            ("key_value_metadata", T::map(T::BLOB, T::BLOB)),
            ("bloom_filter_offset", T::BIGINT),
            ("bloom_filter_length", T::BIGINT),
            ("min_is_exact", T::BOOLEAN),
            ("max_is_exact", T::BOOLEAN),
            ("row_group_compressed_bytes", T::BIGINT),
            ("geo_bbox", geo_bbox_type()),
            ("geo_types", T::list(T::VARCHAR)),
        ],
        MetadataKind::Schema => vec![
            ("file_name", T::VARCHAR),
            ("name", T::VARCHAR),
            ("type", T::VARCHAR),
            ("type_length", T::VARCHAR),
            ("repetition_type", T::VARCHAR),
            ("num_children", T::BIGINT),
            ("converted_type", T::VARCHAR),
            ("scale", T::BIGINT),
            ("precision", T::BIGINT),
            ("field_id", T::BIGINT),
            ("logical_type", T::VARCHAR),
            ("duckdb_type", T::VARCHAR),
            ("column_id", T::BIGINT),
        ],
        MetadataKind::KeyValue => vec![("file_name", T::VARCHAR), ("key", T::BLOB), ("value", T::BLOB)],
        MetadataKind::File => vec![
            ("file_name", T::VARCHAR),
            ("created_by", T::VARCHAR),
            ("num_rows", T::BIGINT),
            ("num_row_groups", T::BIGINT),
            ("format_version", T::BIGINT),
            ("encryption_algorithm", T::VARCHAR),
            ("footer_signing_key_metadata", T::VARCHAR),
            ("file_size_bytes", T::UBIGINT),
            ("footer_size", T::UBIGINT),
            ("column_orders", T::list(T::VARCHAR)),
        ],
        MetadataKind::BloomProbe => vec![
            ("file_name", T::VARCHAR),
            ("row_group_id", T::BIGINT),
            ("bloom_filter_excludes", T::BOOLEAN),
        ],
        MetadataKind::Full => {
            let nested = |kind| {
                T::list(T::struct_type(
                    schema_of(kind)
                        .into_iter()
                        .map(|(name, ty)| (name.to_owned(), ty))
                        .collect(),
                ))
            };
            vec![
                ("parquet_file_metadata", nested(MetadataKind::File)),
                ("parquet_metadata", nested(MetadataKind::RowGroups)),
                ("parquet_schema", nested(MetadataKind::Schema)),
                ("parquet_kv_metadata", nested(MetadataKind::KeyValue)),
            ]
        }
    }
}

/// Thrift's `operator<<` of an enum: its name, or its number.
fn enum_text(name: Option<&'static str>, value: i32) -> String {
    name.map_or_else(|| value.to_string(), str::to_owned)
}

fn optional<T>(value: Option<T>, convert: impl FnOnce(T) -> Value, ty: &LogicalType) -> Value {
    value.map_or_else(|| Value::null(ty.clone()), convert)
}

/// Thrift's `printTo` of a `TimeUnit`.
fn time_unit_text(unit: &TimeUnit) -> String {
    let part = |set: bool, name: &str| {
        if set {
            format!("{name}()")
        } else {
            "<null>".to_owned()
        }
    };
    format!(
        "TimeUnit(MILLIS={}, MICROS={}, NANOS={})",
        part(unit.millis.is_some(), "MilliSeconds"),
        part(unit.micros.is_some(), "MicroSeconds"),
        part(unit.nanos.is_some(), "NanoSeconds")
    )
}

/// Thrift's `to_string(bool)` (an `ostream` without `boolalpha`).
fn bool_text(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

/// `ParquetLogicalTypeToString`: Thrift's `printTo` of the set member.
fn logical_type_text(logical: &ParquetLogicalType) -> Option<String> {
    Some(if logical.string.is_some() {
        "StringType()".to_owned()
    } else if logical.map.is_some() {
        "MapType()".to_owned()
    } else if logical.list.is_some() {
        "ListType()".to_owned()
    } else if logical.enum_.is_some() {
        "EnumType()".to_owned()
    } else if let Some(decimal) = &logical.decimal {
        format!(
            "DecimalType(scale={}, precision={})",
            decimal.scale, decimal.precision
        )
    } else if logical.date.is_some() {
        "DateType()".to_owned()
    } else if let Some(time) = &logical.time {
        format!(
            "TimeType(isAdjustedToUTC={}, unit={})",
            bool_text(time.is_adjusted_to_utc),
            time_unit_text(&time.unit)
        )
    } else if let Some(timestamp) = &logical.timestamp {
        format!(
            "TimestampType(isAdjustedToUTC={}, unit={})",
            bool_text(timestamp.is_adjusted_to_utc),
            time_unit_text(&timestamp.unit)
        )
    } else if let Some(integer) = &logical.integer {
        // `to_string(int8_t)` streams the byte as a character.
        format!(
            "IntType(bitWidth={}, isSigned={})",
            char::from(integer.bit_width as u8),
            bool_text(integer.is_signed)
        )
    } else if logical.unknown.is_some() {
        "NullType()".to_owned()
    } else if logical.json.is_some() {
        "JsonType()".to_owned()
    } else if logical.bson.is_some() {
        "BsonType()".to_owned()
    } else if logical.uuid.is_some() {
        "UUIDType()".to_owned()
    } else if logical.float16.is_some() {
        "Float16Type()".to_owned()
    } else if let Some(geometry) = &logical.geometry {
        format!(
            "GeometryType(crs={})",
            geometry.crs.as_deref().unwrap_or("<null>")
        )
    } else if let Some(geography) = &logical.geography {
        format!(
            "GeographyType(crs={}, algorithm={})",
            geography.crs.as_deref().unwrap_or("<null>"),
            geography.algorithm.map_or_else(
                || "<null>".to_owned(),
                |algorithm| enum_text(algorithm.name(), algorithm.0)
            )
        )
    } else {
        return None;
    })
}

/// Thrift's `printTo` of a `ColumnOrder`.
fn column_order_text(order: &ColumnOrder) -> String {
    format!(
        "ColumnOrder(TYPE_ORDER={})",
        if order.type_order.is_some() {
            "TypeDefinedOrder()"
        } else {
            "<null>"
        }
    )
}

/// Thrift's `printTo` of an `EncryptionAlgorithm`.
fn encryption_text(algorithm: &EncryptionAlgorithm) -> String {
    let aes = |prefix: &Option<crate::thrift::Binary>,
               unique: &Option<crate::thrift::Binary>,
               supply: Option<bool>| {
        let binary = |value: &Option<crate::thrift::Binary>| {
            value.as_ref().map_or_else(
                || "<null>".to_owned(),
                |value| String::from_utf8_lossy(value).into_owned(),
            )
        };
        format!(
            "(aad_prefix={}, aad_file_unique={}, supply_aad_prefix={})",
            binary(prefix),
            binary(unique),
            supply.map_or("<null>", bool_text)
        )
    };
    format!(
        "EncryptionAlgorithm(AES_GCM_V1={}, AES_GCM_CTR_V1={})",
        algorithm.aes_gcm_v1.as_ref().map_or_else(
            || "<null>".to_owned(),
            |aes_gcm| format!(
                "AesGcmV1{}",
                aes(
                    &aes_gcm.aad_prefix,
                    &aes_gcm.aad_file_unique,
                    aes_gcm.supply_aad_prefix
                )
            )
        ),
        algorithm.aes_gcm_ctr_v1.as_ref().map_or_else(
            || "<null>".to_owned(),
            |aes_gcm| format!(
                "AesGcmCtrV1{}",
                aes(
                    &aes_gcm.aad_prefix,
                    &aes_gcm.aad_file_unique,
                    aes_gcm.supply_aad_prefix
                )
            )
        )
    )
}

/// The leaves of a file's schema with their DuckDB types
/// (`ParquetRowGroupMetadataProcessor::InitializeInternal`).
fn leaf_schemas(metadata: &FileMetaData, options: &ParquetOptions) -> Result<Vec<ColumnSchema>> {
    let mut result = Vec::new();
    for element in &metadata.schema {
        if element.num_children.unwrap_or(0) > 0 {
            continue;
        }
        let mut schema = ColumnSchema::file_row_number();
        schema.logical_type = derive_logical_type(element, options, &mut schema)?;
        result.push(schema);
    }
    Ok(result)
}

/// `ConvertParquetStats`: a statistics value as text.
fn stats_text(
    schema: &ColumnSchema,
    bytes: Option<&crate::thrift::Binary>,
    casts: &CastFunctionSet,
) -> Result<Value> {
    let Some(bytes) = bytes else {
        return Ok(Value::null(LogicalType::VARCHAR));
    };
    let value = convert_value(&schema.logical_type, schema, bytes, casts)?;
    value.try_cast_as(casts, &LogicalType::VARCHAR, false)
}

/// The rows of `parquet_metadata` for one file.
fn row_group_rows(reader: &ParquetReader, data: &MetadataBindData) -> Result<Vec<Vec<Value>>> {
    let metadata = reader.metadata();
    let schemas = leaf_schemas(metadata, &data.options)?;
    let mut rows = Vec::new();
    let bigint = |value: Option<i64>| optional(value, Value::bigint, &LogicalType::BIGINT);
    for (group_index, group) in metadata.row_groups.iter().enumerate() {
        for (column_index, schema) in schemas.iter().enumerate() {
            let column = group
                .columns
                .get(column_index)
                .ok_or_else(|| Error::internal("row group has fewer columns than the schema"))?;
            let meta = column.meta_data.clone().unwrap_or_default();
            let stats = meta.statistics.clone().unwrap_or_default();
            let encodings: Vec<String> = meta
                .encodings
                .iter()
                .map(|encoding| enum_text(encoding.name(), encoding.0))
                .collect();
            let kv: Vec<(Value, Value)> = meta
                .key_value_metadata
                .iter()
                .flatten()
                .map(|entry| {
                    (
                        Value::blob(&entry.key),
                        entry
                            .value
                            .as_ref()
                            .map_or_else(|| Value::blob(b""), |value| Value::blob(value)),
                    )
                })
                .collect();
            let geo = meta.geospatial_statistics.clone();
            let bbox = match geo.as_ref().and_then(|geo| geo.bbox.as_ref()) {
                None => Value::null(geo_bbox_type()),
                Some(bbox) => {
                    let double = |value: Option<f64>| optional(value, Value::double, &LogicalType::DOUBLE);
                    Value::struct_value(vec![
                        ("xmin".to_owned(), Value::double(bbox.xmin)),
                        ("xmax".to_owned(), Value::double(bbox.xmax)),
                        ("ymin".to_owned(), Value::double(bbox.ymin)),
                        ("ymax".to_owned(), Value::double(bbox.ymax)),
                        ("zmin".to_owned(), double(bbox.zmin)),
                        ("zmax".to_owned(), double(bbox.zmax)),
                        ("mmin".to_owned(), double(bbox.mmin)),
                        ("mmax".to_owned(), double(bbox.mmax)),
                    ])
                }
            };
            let geo_types = match geo.as_ref().and_then(|geo| geo.geospatial_types.as_ref()) {
                None => Value::null(LogicalType::list(LogicalType::VARCHAR)),
                Some(types) => {
                    let mut set = crate::geo::GeometryTypeSet::default();
                    for &code in types {
                        let (geometry_type, vertex_type) = (code % 1000, code / 1000);
                        if !(1..=7).contains(&geometry_type) {
                            return Err(Error::invalid_input(
                                "Unsupported geometry type in Parquet geo metadata",
                            ));
                        }
                        if !(0..=3).contains(&vertex_type) {
                            return Err(Error::invalid_input(
                                "Unsupported geometry vertex type in Parquet geo metadata",
                            ));
                        }
                        set.add(geometry_type as u32, vertex_type as u32);
                    }
                    Value::list(
                        LogicalType::VARCHAR,
                        set.to_strings(true).into_iter().map(Value::varchar).collect(),
                    )
                }
            };
            rows.push(vec![
                Value::varchar(reader.file_name()),
                Value::bigint(group_index as i64),
                Value::bigint(group.num_rows),
                Value::bigint(group.columns.len() as i64),
                Value::bigint(group.total_byte_size),
                Value::bigint(column_index as i64),
                bigint(group.file_offset.map(|_| column.file_offset)),
                Value::bigint(meta.num_values),
                Value::varchar(meta.path_in_schema.join(", ")),
                Value::varchar(enum_text(meta.type_.name(), meta.type_.0)),
                stats_text(schema, stats.min.as_ref(), &data.casts)?,
                stats_text(schema, stats.max.as_ref(), &data.casts)?,
                bigint(stats.null_count),
                bigint(stats.distinct_count),
                stats_text(schema, stats.min_value.as_ref(), &data.casts)?,
                stats_text(schema, stats.max_value.as_ref(), &data.casts)?,
                Value::varchar(enum_text(meta.codec.name(), meta.codec.0)),
                Value::varchar(encodings.join(", ")),
                bigint(meta.index_page_offset),
                bigint(meta.dictionary_page_offset),
                Value::bigint(meta.data_page_offset),
                Value::bigint(meta.total_compressed_size),
                Value::bigint(meta.total_uncompressed_size),
                Value::map(LogicalType::BLOB, LogicalType::BLOB, kv),
                bigint(meta.bloom_filter_offset),
                bigint(meta.bloom_filter_length.map(i64::from)),
                optional(stats.is_min_value_exact, Value::boolean, &LogicalType::BOOLEAN),
                optional(stats.is_max_value_exact, Value::boolean, &LogicalType::BOOLEAN),
                bigint(group.total_compressed_size),
                bbox,
                geo_types,
            ]);
        }
    }
    Ok(rows)
}

/// The rows of `parquet_schema` for one file.
fn schema_rows(reader: &ParquetReader, data: &MetadataBindData) -> Result<Vec<Vec<Value>>> {
    let metadata = reader.metadata();
    let mut rows = Vec::new();
    let varchar = |value: Option<String>| optional(value, Value::varchar, &LogicalType::VARCHAR);
    let bigint = |value: Option<i32>| {
        optional(
            value,
            |value| Value::bigint(i64::from(value)),
            &LogicalType::BIGINT,
        )
    };
    for (index, element) in metadata.schema.iter().enumerate() {
        let duckdb_type = match element.type_ {
            Some(_) => {
                let mut schema = ColumnSchema::file_row_number();
                Value::varchar(derive_logical_type(element, &data.options, &mut schema)?.to_string())
            }
            None => Value::null(LogicalType::VARCHAR),
        };
        rows.push(vec![
            Value::varchar(reader.file_name()),
            Value::varchar(&element.name),
            varchar(element.type_.map(|ty| enum_text(ty.name(), ty.0))),
            varchar(element.type_length.map(|length| length.to_string())),
            varchar(element.repetition_type.map(|ty| enum_text(ty.name(), ty.0))),
            bigint(element.num_children),
            varchar(element.converted_type.map(|ty| enum_text(ty.name(), ty.0))),
            bigint(element.scale),
            bigint(element.precision),
            bigint(element.field_id),
            varchar(element.logical_type.as_ref().and_then(logical_type_text)),
            duckdb_type,
            Value::bigint(index as i64),
        ]);
    }
    Ok(rows)
}

/// The rows of `parquet_kv_metadata` for one file.
fn key_value_rows(reader: &ParquetReader) -> Vec<Vec<Value>> {
    reader
        .metadata()
        .key_value_metadata
        .iter()
        .flatten()
        .map(|entry| {
            vec![
                Value::varchar(reader.file_name()),
                Value::blob(&entry.key),
                entry
                    .value
                    .as_ref()
                    .map_or_else(|| Value::blob(b""), |value| Value::blob(value)),
            ]
        })
        .collect()
}

/// The row of `parquet_file_metadata` for one file.
fn file_rows(reader: &ParquetReader) -> Vec<Vec<Value>> {
    let metadata = reader.metadata();
    let column_orders = match &metadata.column_orders {
        None => Value::null(LogicalType::list(LogicalType::VARCHAR)),
        Some(orders) => Value::list(
            LogicalType::VARCHAR,
            orders
                .iter()
                .map(|order| Value::varchar(column_order_text(order)))
                .collect(),
        ),
    };
    vec![vec![
        Value::varchar(reader.file_name()),
        optional(metadata.created_by.clone(), Value::varchar, &LogicalType::VARCHAR),
        Value::bigint(metadata.num_rows),
        Value::bigint(metadata.row_groups.len() as i64),
        Value::bigint(i64::from(metadata.version)),
        optional(
            metadata.encryption_algorithm.as_ref().map(encryption_text),
            Value::varchar,
            &LogicalType::VARCHAR,
        ),
        optional(
            metadata
                .footer_signing_key_metadata
                .as_ref()
                .map(|key| String::from_utf8_lossy(key).into_owned()),
            Value::varchar,
            &LogicalType::VARCHAR,
        ),
        Value::ubigint(reader.context.file.size()),
        Value::ubigint(u64::from(reader.footer_len)),
        column_orders,
    ]]
}

/// The rows of `parquet_bloom_probe` for one file.
fn bloom_probe_rows(reader: &ParquetReader, data: &MetadataBindData) -> Result<Vec<Vec<Value>>> {
    let (column, constant) = data
        .probe
        .as_ref()
        .ok_or_else(|| Error::internal("parquet_bloom_probe without a probe"))?;
    let Some(column_index) = reader
        .columns
        .iter()
        .position(|definition| &definition.name == column)
    else {
        return Err(Error::invalid_input(format!(
            "Column {column} not found in {}",
            reader.file_name()
        )));
    };
    let column_type = reader.columns[column_index].logical_type.clone();
    // `Value::DefaultCastAs`.
    let constant = constant
        .try_cast_as(&data.casts, &column_type, false)
        .map_err(|error| Error::invalid_input(format!("Failed to cast value: {}", error.raw_message())))?;
    let mut rows = Vec::new();
    for (group_index, group) in reader.metadata().row_groups.iter().enumerate() {
        let meta = group
            .columns
            .get(column_index)
            .and_then(|chunk| chunk.meta_data.as_ref());
        let excludes = match meta {
            Some(meta) => bloom_filter_excludes(reader, meta, &constant)?,
            None => false,
        };
        rows.push(vec![
            Value::varchar(reader.file_name()),
            Value::bigint(group_index as i64),
            Value::boolean(excludes),
        ]);
    }
    Ok(rows)
}

/// `ValueXXH64`: the hash of a constant as its Parquet plain encoding (0 when unsupported).
pub fn value_hash(constant: &Value) -> u64 {
    use crate::writer::types::xxhash64;
    use rustdb_types::PhysicalType as P;
    let ty = constant.logical_type();
    let integer = || constant.as_i128().unwrap_or(0);
    match ty.physical_type() {
        P::UInt8 | P::Int8 | P::UInt16 | P::Int16 | P::Int32 => xxhash64(&(integer() as i32).to_le_bytes()),
        P::UInt32 => xxhash64(&(integer() as u32).to_le_bytes()),
        P::UInt64 => xxhash64(&(integer() as u64).to_le_bytes()),
        P::Int64 => xxhash64(&(integer() as i64).to_le_bytes()),
        P::Float => xxhash64(&(constant.as_f64().unwrap_or(0.0) as f32).to_le_bytes()),
        P::Double => xxhash64(&constant.as_f64().unwrap_or(0.0).to_le_bytes()),
        P::Varchar => {
            let bytes = constant
                .as_bytes()
                .map(<[u8]>::to_vec)
                .or_else(|| constant.to_varchar().ok().map(String::into_bytes))
                .unwrap_or_default();
            xxhash64(&bytes)
        }
        _ => 0,
    }
}

/// Reads the bloom filter of a column chunk, if it has a usable one.
pub fn read_bloom_filter(
    reader: &ParquetReader,
    meta: &crate::thrift::ColumnMetaData,
) -> Result<Option<BloomFilter>> {
    let Some(offset) = meta.bloom_filter_offset.filter(|&offset| offset > 0) else {
        return Ok(None);
    };
    let file = &reader.context.file;
    let available = file.size().saturating_sub(offset as u64);
    let len = meta
        .bloom_filter_length
        .filter(|&length| length > 0)
        .map_or(available, |length| {
            u64::from(length.unsigned_abs()).min(available)
        });
    let bytes = file.read_range(offset as u64, len as usize)?;
    let mut thrift = crate::thrift::CompactReader::new(&bytes);
    let header = <crate::thrift::BloomFilterHeader as crate::thrift::ThriftValue>::read(&mut thrift)?;
    if header.algorithm.block.is_none()
        || header.compression.uncompressed.is_none()
        || header.hash.xxhash.is_none()
    {
        return Ok(None);
    }
    let start = thrift.position();
    let size = usize::try_from(header.num_bytes).unwrap_or(0);
    let filter_bytes = if start + size <= bytes.len() {
        bytes[start..start + size].to_vec()
    } else {
        file.read_range(offset as u64 + start as u64, size)?
    };
    Ok(Some(BloomFilter::from_bytes(&filter_bytes)))
}

/// `BloomFilterExcludes` for an equality with `constant`.
fn bloom_filter_excludes(
    reader: &ParquetReader,
    meta: &crate::thrift::ColumnMetaData,
    constant: &Value,
) -> Result<bool> {
    if constant.is_null() {
        return Ok(false);
    }
    let Some(filter) = read_bloom_filter(reader, meta)? else {
        return Ok(false);
    };
    let hash = value_hash(constant);
    Ok(hash > 0 && !filter.check(hash))
}

/// Packs `rows` as a LIST of STRUCTs (`PopulateMetadata`).
fn nested_rows(kind: MetadataKind, rows: Vec<Vec<Value>>) -> Value {
    let fields = schema_of(kind);
    let struct_type = LogicalType::struct_type(
        fields
            .iter()
            .map(|(name, ty)| ((*name).to_owned(), ty.clone()))
            .collect(),
    );
    Value::list(
        struct_type,
        rows.into_iter()
            .map(|row| {
                Value::struct_value(
                    fields
                        .iter()
                        .map(|(name, _)| (*name).to_owned())
                        .zip(row)
                        .collect(),
                )
            })
            .collect(),
    )
}

/// The rows a function returns for one file.
fn file_metadata_rows(data: &MetadataBindData, path: &str) -> Result<Vec<Vec<Value>>> {
    let reader = ParquetReader::open(path, data.options.clone())?;
    match data.kind {
        MetadataKind::RowGroups => row_group_rows(&reader, data),
        MetadataKind::Schema => schema_rows(&reader, data),
        MetadataKind::KeyValue => Ok(key_value_rows(&reader)),
        MetadataKind::File => Ok(file_rows(&reader)),
        MetadataKind::BloomProbe => bloom_probe_rows(&reader, data),
        MetadataKind::Full => Ok(vec![vec![
            nested_rows(MetadataKind::File, file_rows(&reader)),
            nested_rows(MetadataKind::RowGroups, row_group_rows(&reader, data)?),
            nested_rows(MetadataKind::Schema, schema_rows(&reader, data)?),
            nested_rows(MetadataKind::KeyValue, key_value_rows(&reader)),
        ]]),
    }
}

/// The scan state: the rows of the current file and the next file.
#[derive(Debug, Default)]
struct MetadataScan {
    next_file: usize,
    rows: Vec<Vec<Value>>,
    next_row: usize,
}

fn metadata_scan(
    bind_data: Option<&dyn FunctionData>,
    _: &[u64],
    state: &mut TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<MetadataBindData>())
        .ok_or_else(|| Error::internal("parquet metadata scan without bind data"))?;
    if state.local.is_none() {
        state.local = Some(Box::new(MetadataScan::default()));
    }
    let scan = state
        .local
        .as_mut()
        .and_then(|local| local.downcast_mut::<MetadataScan>())
        .ok_or_else(|| Error::internal("parquet metadata scan state"))?;
    let types: Vec<LogicalType> = schema_of(data.kind).into_iter().map(|(_, ty)| ty).collect();
    let mut output: Vec<Vec<Value>> = Vec::new();
    while output.len() < STANDARD_VECTOR_SIZE {
        if scan.next_row >= scan.rows.len() {
            if !output.is_empty() {
                break;
            }
            let Some(path) = data.files.files.get(scan.next_file) else {
                break;
            };
            scan.rows = file_metadata_rows(data, path)?;
            scan.next_row = 0;
            scan.next_file += 1;
            continue;
        }
        let take = (STANDARD_VECTOR_SIZE - output.len()).min(scan.rows.len() - scan.next_row);
        output.extend(scan.rows[scan.next_row..scan.next_row + take].iter().cloned());
        scan.next_row += take;
        if data.kind == MetadataKind::Full {
            break;
        }
    }
    let count = output.len();
    let mut columns: Vec<Vector> = types
        .iter()
        .map(|ty| Vector::flat(ty.clone(), count.max(1)))
        .collect();
    for (row, values) in output.iter().enumerate() {
        for (column, value) in columns.iter_mut().zip(values) {
            column.set_value(row, value)?;
        }
    }
    *chunk = DataChunk::new(columns, count);
    Ok(count > 0)
}

/// The options of the metadata functions (`ParquetOptions(context)`).
fn context_options(
    input: &rustdb_function::TableFunctionBindInput<'_>,
) -> Result<(ParquetOptions, Arc<CastFunctionSet>)> {
    let context = crate::scan::client_context(input)?;
    Ok((
        ParquetOptions {
            binary_as_string: crate::scan::setting_bool(context, "binary_as_string"),
            ..ParquetOptions::default()
        },
        Arc::clone(&context.casts),
    ))
}

fn metadata_function(name: &'static str, kind: MetadataKind, argument: LogicalType) -> TableFunction {
    let mut arguments = vec![argument];
    if kind == MetadataKind::BloomProbe {
        arguments.extend([LogicalType::VARCHAR, LogicalType::ANY]);
    }
    TableFunction::new(name, arguments)
        .with_bind(move |input| {
            let (options, casts) = context_options(input)?;
            let first = input
                .inputs
                .first()
                .ok_or_else(|| Error::internal("parquet metadata function without a file"))?;
            let mut probe = None;
            if kind == MetadataKind::BloomProbe {
                let (Some(column), Some(constant)) = (input.inputs.get(1), input.inputs.get(2)) else {
                    return Err(Error::internal("parquet_bloom_probe expects three arguments"));
                };
                if column.is_null() || constant.is_null() {
                    return Err(Error::invalid_input(
                        "Can't have NULL parameters for parquet_bloom_probe",
                    ));
                }
                probe = Some((column.to_varchar()?, constant.clone()));
            }
            let mut files = Vec::new();
            for path in parse_paths("ParquetMetadata", first)? {
                let mut expanded = glob_file_list(&path, Some("parquet"))?;
                expanded.sort();
                files.extend(expanded);
            }
            let (names, return_types) = names_types(&schema_of(kind));
            Ok(TableFunctionBindOutput {
                return_types,
                names,
                bind_data: Some(Arc::new(MetadataBindData {
                    kind,
                    files: FileList { files },
                    options,
                    casts,
                    probe,
                })),
            })
        })
        .with_scan(metadata_scan)
}

/// The metadata functions (each over a VARCHAR or a LIST(VARCHAR) of files).
pub fn metadata_functions() -> Vec<TableFunctionSet> {
    [
        ("parquet_metadata", MetadataKind::RowGroups),
        ("parquet_schema", MetadataKind::Schema),
        ("parquet_kv_metadata", MetadataKind::KeyValue),
        ("parquet_file_metadata", MetadataKind::File),
        ("parquet_bloom_probe", MetadataKind::BloomProbe),
        ("parquet_full_metadata", MetadataKind::Full),
    ]
    .into_iter()
    .map(|(name, kind)| {
        let mut set = TableFunctionSet::new(name);
        set.add(metadata_function(name, kind, LogicalType::VARCHAR));
        set.add(metadata_function(
            name,
            kind,
            LogicalType::list(LogicalType::VARCHAR),
        ));
        set
    })
    .collect()
}
