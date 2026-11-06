//! `COPY ... TO/FROM ... (FORMAT parquet)` (the copy function of DuckDB's
//! `parquet_extension.cpp`: `ParquetWriteBind`, `ParquetWriteSink`, ... and
//! `MultiFileBindCopy`).

use std::any::Any;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_function::copy::{
    CopyFromBindInput, CopyFunctionBindInput, CopyGlobalState, CopyOption, CopyOptionMode, CopyToSelectInput,
    CopyToType,
};
use rustdb_function::{CopyFunction, FunctionData};
use rustdb_planner::ClientContext;
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Value};

use crate::scan::{ParquetBindData, bind_files_with_client};
use crate::schema::ParquetOptions;
use crate::thrift::CompressionCodec;
use crate::writer::{FieldId, ParquetVersion, ParquetWriter, WriterOptions};

/// The name of the field id entry of a nested `FIELD_IDS` struct (`FieldID::DUCKDB_FIELD_ID`).
const DUCKDB_FIELD_ID: &str = "__duckdb_field_id";

/// The bind data of a Parquet write (`ParquetWriteBindData`).
#[derive(Debug)]
pub struct ParquetWriteBindData {
    pub names: Vec<rustdb_common::ByteString>,
    pub types: Vec<LogicalType>,
    pub options: WriterOptions,
}

impl FunctionData for ParquetWriteBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The rows buffered for the next row group (`ParquetWriteLocalState`).
#[derive(Debug, Default)]
struct RowBuffer {
    chunks: Vec<DataChunk>,
    rows: usize,
    bytes: usize,
}

/// The state of one written file (`ParquetWriteGlobalState`).
#[derive(Debug)]
pub struct ParquetWriteGlobal {
    inner: Mutex<(ParquetWriter, RowBuffer)>,
    /// The rows threads left behind in parallel writes (`combine_buffer`).
    combine_buffer: Mutex<Option<RowBuffer>>,
    /// Prepares batches without the writer's lock (`prepare_batch`).
    preparer: crate::writer::RowGroupPreparer,
    /// The path of the file.
    file_name: rustdb_common::ByteString,
    /// The COPY operator's logger, if the query logs it (`op`).
    logger: std::sync::OnceLock<rustdb_function::logging::OperatorLogger>,
}

impl ParquetWriteGlobal {
    /// `LogFlushingRowGroup`: the row group about to be written and why.
    fn log_flushing_row_group(&self, buffer: &RowBuffer, reason: &str) -> Result<()> {
        let Some(logger) = self.logger.get() else {
            return Ok(());
        };
        logger.log_bytes(
            "ParquetWriter",
            "FlushRowGroup",
            &[
                ("file", self.file_name.clone()),
                ("rows", buffer.rows.to_string().into()),
                ("size", buffer.collection_size_in_bytes().to_string().into()),
                ("reason", reason.into()),
            ],
        )
    }
}

impl RowBuffer {
    /// The size of the rows as DuckDB's buffer (a `ColumnDataCollection`) holds them
    /// (`SizeInBytes`): every column takes a vector of `STANDARD_VECTOR_SIZE` values and
    /// its validity mask per 2048 rows, strings longer than 12 bytes their bytes besides.
    fn collection_size_in_bytes(&self) -> usize {
        const VALIDITY_MASK_SIZE: usize = rustdb_common::STANDARD_VECTOR_SIZE / 8;
        let Some(first) = self.chunks.first() else {
            return 0;
        };
        let vectors = self.rows.div_ceil(rustdb_common::STANDARD_VECTOR_SIZE);
        let mut size = 0;
        for (index, column) in first.columns().iter().enumerate() {
            let physical = column.logical_type().physical_type();
            let width = physical.size().max(1);
            size += vectors * (width * rustdb_common::STANDARD_VECTOR_SIZE + VALIDITY_MASK_SIZE);
            if physical == rustdb_types::PhysicalType::Varchar {
                for chunk in &self.chunks {
                    let column = chunk.column(index);
                    if column.kind() != rustdb_types::VectorKind::Flat {
                        continue;
                    }
                    size += (0..chunk.len())
                        .filter(|&row| !column.is_null(row))
                        .map(|row| column.string_bytes(row).len())
                        .filter(|&len| len > 12)
                        .sum::<usize>();
                }
            }
        }
        size
    }
}

fn client_context(binder: Option<&dyn Any>) -> Result<&ClientContext> {
    binder
        .and_then(|binder| binder.downcast_ref::<ClientContext>())
        .ok_or_else(|| Error::internal("parquet copy bound without a client context"))
}

/// `ParquetListCopyOptions`.
fn parquet_copy_options() -> Vec<(String, CopyOption)> {
    use CopyOptionMode::{ReadOnly, ReadWrite, WriteOnly};
    let option =
        |name: &str, ty: LogicalType, mode: CopyOptionMode| (name.to_owned(), CopyOption::new(ty, mode));
    vec![
        option("row_group_size", LogicalType::UBIGINT, ReadWrite),
        option("chunk_size", LogicalType::UBIGINT, WriteOnly),
        option("row_group_size_bytes", LogicalType::ANY, WriteOnly),
        option("row_groups_per_file", LogicalType::UBIGINT, WriteOnly),
        option("compression", LogicalType::VARCHAR, ReadWrite),
        option("codec", LogicalType::VARCHAR, ReadWrite),
        option("field_ids", LogicalType::ANY, WriteOnly),
        option("kv_metadata", LogicalType::ANY, WriteOnly),
        option("encryption_config", LogicalType::ANY, ReadWrite),
        option(
            "dictionary_compression_ratio_threshold",
            LogicalType::ANY,
            WriteOnly,
        ),
        option("dictionary_size_limit", LogicalType::BIGINT, WriteOnly),
        option(
            "string_dictionary_page_size_limit",
            LogicalType::UBIGINT,
            WriteOnly,
        ),
        option(
            "bloom_filter_false_positive_ratio",
            LogicalType::DOUBLE,
            WriteOnly,
        ),
        option("debug_use_openssl", LogicalType::BOOLEAN, ReadWrite),
        option("write_bloom_filter", LogicalType::BOOLEAN, WriteOnly),
        option("compression_level", LogicalType::BIGINT, WriteOnly),
        option("parquet_version", LogicalType::VARCHAR, WriteOnly),
        option("binary_as_string", LogicalType::BOOLEAN, ReadOnly),
        option("file_row_number", LogicalType::BOOLEAN, ReadOnly),
        option("can_have_nan", LogicalType::BOOLEAN, ReadOnly),
        option("geoparquet_version", LogicalType::VARCHAR, WriteOnly),
        option("shredding", LogicalType::ANY, WriteOnly),
    ]
}

/// The value of an option as a `u64` (`Value::GetValue<uint64_t>`).
fn u64_value(context: &ClientContext, value: &Value) -> Result<u64> {
    use rustdb_function::cast::ValueCast as _;
    let value = value.try_cast_as(&context.casts, &LogicalType::UBIGINT, false)?;
    Ok(value.as_u64().unwrap_or(0))
}

fn i64_value(context: &ClientContext, value: &Value) -> Result<i64> {
    use rustdb_function::cast::ValueCast as _;
    let value = value.try_cast_as(&context.casts, &LogicalType::BIGINT, false)?;
    Ok(value.as_i64().unwrap_or(0))
}

fn f64_value(context: &ClientContext, value: &Value) -> Result<f64> {
    use rustdb_function::cast::ValueCast as _;
    let value = value.try_cast_as(&context.casts, &LogicalType::DOUBLE, false)?;
    Ok(value.as_f64().unwrap_or(0.0))
}

/// `LogicalTypeIdToString` of a type.
pub(crate) fn type_id_name(ty: &LogicalType) -> String {
    match ty.id() {
        LogicalTypeId::List => "LIST".to_owned(),
        LogicalTypeId::Map => "MAP".to_owned(),
        LogicalTypeId::Struct => "STRUCT".to_owned(),
        LogicalTypeId::Array => "ARRAY".to_owned(),
        LogicalTypeId::Decimal => "DECIMAL".to_owned(),
        LogicalTypeId::Enum => "ENUM".to_owned(),
        _ => ty.to_string(),
    }
}

/// The children of a nested type as `FIELD_IDS` sees them (`GetChildNameToTypeMap`).
fn child_name_to_type(
    ty: &LogicalType,
) -> Result<CaseInsensitiveMap<LogicalType, rustdb_common::ByteString>> {
    let mut map = CaseInsensitiveMap::default();
    match ty.id() {
        LogicalTypeId::List => {
            map.insert(
                "element",
                ty.child_type().cloned().unwrap_or(LogicalType::SQLNULL),
            );
        }
        LogicalTypeId::Map => {
            if let Some((key, value)) = ty.map_key_value() {
                map.insert("key", key.clone());
                map.insert("value", value.clone());
            }
        }
        LogicalTypeId::Struct => {
            for (name, child) in ty.struct_children().unwrap_or_default() {
                if name == DUCKDB_FIELD_ID {
                    return Err(Error::binder(rustdb_common::byte_format!(
                        "Cannot have column named \"{DUCKDB_FIELD_ID}\" with FIELD_IDS"
                    )));
                }
                map.insert(name.clone(), child.clone());
            }
        }
        _ => return Err(Error::internal("Unexpected type in GetChildNameToTypeMap")),
    }
    Ok(map)
}

/// The names and types below a nested type (`GetChildNamesAndTypes`).
fn child_names_and_types(ty: &LogicalType) -> Vec<(rustdb_common::ByteString, LogicalType)> {
    match ty.id() {
        LogicalTypeId::List => vec![(
            "element".into(),
            ty.child_type().cloned().unwrap_or(LogicalType::SQLNULL),
        )],
        LogicalTypeId::Map => ty
            .map_key_value()
            .map(|(key, value)| vec![("key".into(), key.clone()), ("value".into(), value.clone())])
            .unwrap_or_default(),
        LogicalTypeId::Struct => ty.struct_children().map(<[_]>::to_vec).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `FieldID::GenerateFieldIDs`: sequential ids in depth-first order.
fn generate_field_ids(
    ids: &mut CaseInsensitiveMap<FieldId, rustdb_common::ByteString>,
    next: &mut i32,
    columns: &[(rustdb_common::ByteString, LogicalType)],
) {
    for (name, ty) in columns {
        let mut field = FieldId {
            field_id: Some(*next),
            children: CaseInsensitiveMap::default(),
        };
        *next += 1;
        if matches!(
            ty.id(),
            LogicalTypeId::List | LogicalTypeId::Map | LogicalTypeId::Struct
        ) {
            generate_field_ids(&mut field.children, next, &child_names_and_types(ty));
        }
        ids.insert(name.clone(), field);
    }
}

/// `FieldID::GetFieldIDs`.
fn parse_field_ids(
    context: &ClientContext,
    value: &Value,
    ids: &mut CaseInsensitiveMap<FieldId, rustdb_common::ByteString>,
    unique: &mut std::collections::HashSet<i32>,
    name_to_type: &CaseInsensitiveMap<LogicalType, rustdb_common::ByteString>,
) -> Result<()> {
    let ty = value.logical_type();
    if ty.id() != LogicalTypeId::Struct {
        return Err(Error::binder(rustdb_common::byte_format!(
            "Expected FIELD_IDS to be a STRUCT, e.g., {{col1: 42, col2: {{{DUCKDB_FIELD_ID}: 43, nested_col: 44}}, col3: 44}}"
        )));
    }
    let names: Vec<rustdb_common::ByteString> = ty
        .struct_children()
        .unwrap_or_default()
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    let children = value.children().unwrap_or_default();
    for (name, child) in names.iter().zip(children) {
        let column = name.to_ascii_lowercase();
        if column == DUCKDB_FIELD_ID {
            continue;
        }
        let Some(column_type) = name_to_type.get(&column) else {
            let available: Vec<&rustdb_common::ByteString> =
                name_to_type.iter().map(|(name, _)| name).collect();
            return Err(Error::binder(rustdb_common::byte_format!(
                "Column name \"{column}\" specified in FIELD_IDS not found. Consider using WRITE_PARTITION_COLUMNS if this column is a partition column. Available column names: [{}]",
                rustdb_common::ByteString::join(&available, b", ")
            )));
        };
        let mut field_id_value = None;
        let mut nested = None;
        if child.logical_type().id() == LogicalTypeId::Struct {
            let nested_names = child.logical_type().struct_children().unwrap_or_default();
            for ((nested_name, _), nested_value) in
                nested_names.iter().zip(child.children().unwrap_or_default())
            {
                if nested_name == DUCKDB_FIELD_ID {
                    field_id_value = Some(nested_value.clone());
                } else {
                    nested = Some(child.clone());
                }
            }
        } else {
            field_id_value = Some(child.clone());
        }
        let mut field = FieldId::default();
        if let Some(id_value) = field_id_value {
            use rustdb_function::cast::ValueCast as _;
            // `Value::DefaultCastAs`.
            let id_value = id_value
                .try_cast_as(&context.casts, &LogicalType::INTEGER, false)
                .map_err(|error| {
                    Error::invalid_input(rustdb_common::byte_format!(
                        "Failed to cast value: {}",
                        error.raw_message()
                    ))
                })?;
            let id = id_value.as_i32().unwrap_or(0);
            if !unique.insert(id) {
                return Err(Error::binder(rustdb_common::byte_format!(
                    "Duplicate field_id {id} found in FIELD_IDS"
                )));
            }
            field.field_id = Some(id);
        }
        if let Some(nested) = nested {
            if !matches!(
                column_type.id(),
                LogicalTypeId::List | LogicalTypeId::Map | LogicalTypeId::Struct
            ) {
                return Err(Error::binder(rustdb_common::byte_format!(
                    "Column \"{column}\" with type \"{}\" cannot have a nested FIELD_IDS specification",
                    type_id_name(column_type)
                )));
            }
            let child_types = child_name_to_type(column_type)?;
            parse_field_ids(context, &nested, &mut field.children, unique, &child_types)?;
        }
        ids.insert(column, field);
    }
    Ok(())
}

/// `ParquetWriteBind`.
fn parquet_write_bind(
    input: &mut CopyFunctionBindInput<'_>,
    names: &[rustdb_common::ByteString],
    types: &[LogicalType],
) -> Result<Arc<dyn FunctionData>> {
    let context = client_context(input.binder)?;
    let mut options = WriterOptions {
        geoparquet_conversion: crate::scan::setting_bool(context, "enable_geoparquet_conversion"),
        ..WriterOptions::default()
    };
    let mut row_group_size_bytes_set = false;
    let mut compression_level_set = false;
    for (name, values) in input.options.iter() {
        let option = name.to_ascii_lowercase();
        let [value] = values.as_slice() else {
            return Err(Error::binder(rustdb_common::byte_format!(
                "{} requires exactly one argument",
                option.to_ascii_uppercase()
            )));
        };
        match option.as_bytes() {
            b"row_group_size" | b"chunk_size" => {
                options.row_group_size = usize::try_from(u64_value(context, value)?).unwrap_or(usize::MAX);
            }
            b"row_group_size_bytes" => {
                let bytes = if value.logical_type().id() == LogicalTypeId::Varchar {
                    rustdb_buffer::parse_memory_limit(&value.to_varchar()?)?
                } else {
                    u64_value(context, value)?
                };
                options.row_group_size_bytes = usize::try_from(bytes).unwrap_or(usize::MAX);
                row_group_size_bytes_set = true;
            }
            b"row_groups_per_file" => options.row_groups_per_file = Some(u64_value(context, value)?),
            b"compression" | b"codec" => {
                options.codec = match value.to_varchar()?.to_lowercase().as_str() {
                    "uncompressed" => CompressionCodec::UNCOMPRESSED,
                    "snappy" => CompressionCodec::SNAPPY,
                    "gzip" => CompressionCodec::GZIP,
                    "zstd" => CompressionCodec::ZSTD,
                    "brotli" => CompressionCodec::BROTLI,
                    "lz4" | "lz4_raw" => CompressionCodec::LZ4_RAW,
                    _ => {
                        return Err(Error::binder(rustdb_common::byte_format!(
                            "Expected {option} argument to be any of [uncompressed, brotli, gzip, snappy, lz4, lz4_raw or zstd]"
                        )));
                    }
                };
            }
            b"field_ids" => {
                if value.logical_type().id() == LogicalTypeId::Varchar
                    && value.to_varchar()?.eq_ignore_ascii_case("auto")
                {
                    let columns: Vec<(rustdb_common::ByteString, LogicalType)> =
                        names.iter().cloned().zip(types.iter().cloned()).collect();
                    let mut next = 0;
                    generate_field_ids(&mut options.field_ids, &mut next, &columns);
                } else {
                    let mut name_to_type = CaseInsensitiveMap::default();
                    for (column, ty) in names.iter().zip(types) {
                        if column == DUCKDB_FIELD_ID {
                            return Err(Error::binder(rustdb_common::byte_format!(
                                "Cannot have a column named \"{DUCKDB_FIELD_ID}\" when writing FIELD_IDS"
                            )));
                        }
                        name_to_type.insert(column.clone(), ty.clone());
                    }
                    let mut unique = std::collections::HashSet::new();
                    parse_field_ids(context, value, &mut options.field_ids, &mut unique, &name_to_type)?;
                }
            }
            b"shredding" => {
                if value.logical_type().id() == LogicalTypeId::Varchar
                    && value.to_varchar()?.eq_ignore_ascii_case("auto")
                {
                    return Err(Error::not_implemented(
                        "The 'auto' option is not yet implemented for 'shredding'",
                    ));
                }
                let variant_names: Vec<rustdb_common::ByteString> = names
                    .iter()
                    .zip(types)
                    .filter(|(_, ty)| ty.id() == LogicalTypeId::Variant)
                    .map(|(name, _)| name.clone())
                    .collect();
                let fields = value
                    .logical_type()
                    .struct_children()
                    .unwrap_or_default()
                    .to_vec();
                let children = value.children().unwrap_or_default().to_vec();
                for ((column, _), child) in fields.iter().zip(children) {
                    let column = column.to_ascii_lowercase();
                    if !variant_names
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(&column))
                    {
                        if variant_names.is_empty() {
                            return Err(Error::binder(rustdb_common::byte_format!(
                                "VARIANT by name \"{column}\" specified in SHREDDING not found. There are no VARIANT columns present."
                            )));
                        }
                        return Err(Error::binder(rustdb_common::byte_format!(
                            "VARIANT by name \"{column}\" specified in SHREDDING not found. Consider using WRITE_PARTITION_COLUMNS if this column is a partition column. Available names of VARIANT columns: [{}]",
                            rustdb_common::ByteString::join(&variant_names, b", ")
                        )));
                    }
                    if child.logical_type().id() != LogicalTypeId::Varchar {
                        return Err(Error::binder(
                            "SHREDDING value should be of type VARCHAR, a stringified type to use for the column",
                        ));
                    }
                    let ty = rustdb_csv::read_csv::transform_string_to_logical_type(
                        context,
                        &child.to_varchar()?,
                    )?;
                    crate::variant::write::check_shredding_type(&ty)?;
                    if ty.id() != LogicalTypeId::Variant {
                        crate::variant::write::transform_typed_value(&ty)?;
                    }
                    options.shredding.push((column, ty));
                }
            }
            b"kv_metadata" => {
                let ty = value.logical_type();
                if ty.id() != LogicalTypeId::Struct {
                    return Err(Error::binder("Expected kv_metadata argument to be a STRUCT"));
                }
                let keys = ty.struct_children().unwrap_or_default();
                for ((key, _), child) in keys.iter().zip(value.children().unwrap_or_default()) {
                    let bytes = if child.logical_type().id() == LogicalTypeId::Blob {
                        child.as_bytes().map(<[u8]>::to_vec).unwrap_or_default()
                    } else {
                        child.to_varchar()?.into_bytes()
                    };
                    options.kv_metadata.push((key.clone(), bytes));
                }
            }
            b"encryption_config" => {
                let cache = context.function_client.object_cache();
                options.encryption = Some(crate::crypto::parse_encryption_config(value, cache.as_deref())?);
            }
            b"dictionary_compression_ratio_threshold" | b"debug_use_openssl" => {}
            b"dictionary_size_limit" => {
                let limit = i64_value(context, value)?;
                if limit < 0 {
                    return Err(Error::binder(
                        "dictionary_size_limit must be greater than 0 or 0 to disable",
                    ));
                }
                options.dictionary_size_limit = Some(limit as usize);
            }
            b"string_dictionary_page_size_limit" => {
                let limit = u64_value(context, value)?;
                if limit > crate::writer::MAX_UNCOMPRESSED_DICT_PAGE_SIZE as u64 || limit == 0 {
                    return Err(Error::binder(rustdb_common::byte_format!(
                        "string_dictionary_page_size_limit cannot be 0 and must be less than or equal to {}",
                        crate::writer::MAX_UNCOMPRESSED_DICT_PAGE_SIZE
                    )));
                }
                options.string_dictionary_page_size_limit = limit as usize;
            }
            b"write_bloom_filter" => {
                use rustdb_function::cast::ValueCast as _;
                options.enable_bloom_filters = value
                    .try_cast_as(&context.casts, &LogicalType::BOOLEAN, false)?
                    .as_bool()
                    .unwrap_or(false);
            }
            b"bloom_filter_false_positive_ratio" => {
                let ratio = f64_value(context, value)?;
                if ratio <= 0.0 {
                    return Err(Error::binder(
                        "bloom_filter_false_positive_ratio must be greater than 0",
                    ));
                }
                options.bloom_filter_false_positive_ratio = ratio;
            }
            b"compression_level" => {
                let level = i64_value(context, value)?;
                if !(-131_072..=22).contains(&level) {
                    return Err(Error::binder("Compression level must be between -131072 and 22"));
                }
                options.compression_level = level;
                compression_level_set = true;
            }
            b"parquet_version" => {
                options.version = match value.to_varchar()?.to_uppercase().as_str() {
                    "V1" => ParquetVersion::V1,
                    "V2" => ParquetVersion::V2,
                    _ => return Err(Error::binder("Expected parquet_version 'V1' or 'V2'")),
                };
            }
            b"geoparquet_version" => {
                use crate::geo::GeoParquetVersion;
                options.geoparquet_version = match value.to_varchar()?.to_uppercase().as_str() {
                    "NONE" => GeoParquetVersion::None,
                    "V1" => GeoParquetVersion::V1,
                    "V2" => GeoParquetVersion::V2,
                    "BOTH" => GeoParquetVersion::Both,
                    _ => {
                        return Err(Error::binder(
                            "Expected geoparquet_version 'NONE', 'V1' or 'BOTH'",
                        ));
                    }
                };
            }
            _ => {
                return Err(Error::internal(rustdb_common::byte_format!(
                    "Unrecognized option for PARQUET: {name}"
                )));
            }
        }
    }
    if row_group_size_bytes_set && preserve_insertion_order(context) {
        return Err(Error::binder(
            "ROW_GROUP_SIZE_BYTES does not work while preserving insertion order. Use \"SET preserve_insertion_order=false;\" to disable preserving insertion order.",
        ));
    }
    if compression_level_set && options.codec != CompressionCodec::ZSTD {
        return Err(Error::binder(
            "Compression level is only supported for the ZSTD compression codec",
        ));
    }
    Ok(Arc::new(ParquetWriteBindData {
        names: names.to_vec(),
        types: types.to_vec(),
        options,
    }))
}

/// The `preserve_insertion_order` setting.
fn preserve_insertion_order(context: &ClientContext) -> bool {
    context
        .function_client
        .settings()
        .and_then(|settings| settings.current_setting("preserve_insertion_order").ok())
        .and_then(|value| value.as_bool())
        .unwrap_or(true)
}

fn bind_data(data: &dyn FunctionData) -> Result<&ParquetWriteBindData> {
    data.as_any()
        .downcast_ref::<ParquetWriteBindData>()
        .ok_or_else(|| Error::internal("parquet write without its bind data"))
}

fn global_state(state: &CopyGlobalState) -> Result<&ParquetWriteGlobal> {
    state
        .downcast_ref::<ParquetWriteGlobal>()
        .ok_or_else(|| Error::internal("parquet write without its global state"))
}

/// `ParquetWriteInitializeGlobal`.
fn parquet_write_initialize_global(data: &dyn FunctionData, path: &[u8]) -> Result<CopyGlobalState> {
    let data = bind_data(data)?;
    let writer = ParquetWriter::new(path, &data.names, &data.types, data.options.clone())?;
    let preparer = writer.preparer();
    Ok(Box::new(ParquetWriteGlobal {
        inner: Mutex::new((writer, RowBuffer::default())),
        combine_buffer: Mutex::new(None),
        preparer,
        file_name: path.into(),
        logger: std::sync::OnceLock::new(),
    }))
}

/// `ParquetWriteInitializeOperator`: the writer logs for the COPY operator.
fn parquet_write_initialize_operator(
    state: &CopyGlobalState,
    logger: &rustdb_function::logging::OperatorLogger,
) {
    if let Ok(global) = global_state(state) {
        let _ = global.logger.set(logger.clone());
    }
}

/// Why a buffer that reached the row group size is written (`ParquetWriteSink`).
fn row_group_size_reason(data: &ParquetWriteBindData, buffer: &RowBuffer) -> &'static str {
    if buffer.rows >= data.options.row_group_size {
        "ROW_GROUP_SIZE"
    } else {
        "ROW_GROUP_SIZE_BYTES"
    }
}

/// `ParquetWriter::Flush` from any thread: prepares the row group without the file's
/// lock, then writes it under the lock.
fn flush_rows(global: &ParquetWriteGlobal, buffer: &RowBuffer) -> Result<()> {
    if buffer.rows == 0 {
        return Ok(());
    }
    let prepared = global.preparer.prepare(&buffer.chunks)?;
    let mut inner = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
    inner.0.flush_prepared(prepared)
}

/// `ParquetWriteSink` with a thread's buffer (parallel writes).
fn parquet_write_sink_local(
    data: &dyn FunctionData,
    state: &CopyGlobalState,
    local: &mut rustdb_function::copy::CopyLocalState,
    chunk: &DataChunk,
) -> Result<()> {
    let data = bind_data(data)?;
    let global = global_state(state)?;
    let buffer = local
        .downcast_mut::<RowBuffer>()
        .ok_or_else(|| Error::internal("parquet write with a foreign local state"))?;
    if chunk.is_empty() {
        return Ok(());
    }
    buffer.rows += chunk.len();
    buffer.bytes += chunk_size_in_bytes(chunk);
    buffer.chunks.push(chunk.clone());
    if buffer.rows >= data.options.row_group_size || buffer.bytes >= data.options.row_group_size_bytes {
        global.log_flushing_row_group(buffer, row_group_size_reason(data, buffer))?;
        flush_rows(global, buffer)?;
        *buffer = RowBuffer::default();
    }
    Ok(())
}

/// `ParquetWriteCombine`: a thread's buffer is written if it is at least half a row
/// group, else combined with the other threads' leftovers.
fn parquet_write_combine(
    data: &dyn FunctionData,
    state: &CopyGlobalState,
    local: rustdb_function::copy::CopyLocalState,
) -> Result<()> {
    let data = bind_data(data)?;
    let global = global_state(state)?;
    let buffer = *local
        .downcast::<RowBuffer>()
        .map_err(|_| Error::internal("parquet write with a foreign local state"))?;
    let half_rows = data.options.row_group_size / 2;
    let half_bytes = data.options.row_group_size_bytes / 2;
    if buffer.rows >= half_rows || buffer.bytes >= half_bytes {
        global.log_flushing_row_group(&buffer, "Combine")?;
        return flush_rows(global, &buffer);
    }
    let mut combine = global
        .combine_buffer
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    match combine.as_mut() {
        Some(combined) => {
            combined.rows += buffer.rows;
            combined.bytes += buffer.bytes;
            combined.chunks.extend(buffer.chunks);
            if combined.rows >= half_rows || combined.bytes >= half_bytes {
                let owned = combine.take().unwrap_or_default();
                drop(combine);
                global.log_flushing_row_group(&owned, "Combine")?;
                return flush_rows(global, &owned);
            }
        }
        None => *combine = Some(buffer),
    }
    Ok(())
}

/// `ParquetWriteExecutionMode` (regular for VARIANT columns, whose shredding the first
/// row group decides).
fn parquet_write_execution_mode(
    data: &dyn FunctionData,
    preserve_insertion_order: bool,
    supports_batch_index: bool,
) -> rustdb_function::copy::CopyFunctionExecutionMode {
    use rustdb_function::copy::CopyFunctionExecutionMode as Mode;
    let batches = bind_data(data).is_ok_and(|data| ParquetWriter::supports_batches(&data.types));
    if !preserve_insertion_order {
        return if batches {
            Mode::ParallelCopyToFile
        } else {
            Mode::RegularCopyToFile
        };
    }
    if supports_batch_index && batches {
        return Mode::BatchCopyToFile;
    }
    Mode::RegularCopyToFile
}

/// `ParquetWritePrepareBatch`: encodes a row group.
fn parquet_write_prepare_batch(
    _: &dyn FunctionData,
    state: &CopyGlobalState,
    batch: &dyn rustdb_function::copy::CopyBatch,
) -> Result<rustdb_function::copy::PreparedBatch> {
    let global = global_state(state)?;
    Ok(Box::new(global.preparer.prepare_batch(batch)?))
}

/// `ParquetWriteFlushBatch`: writes a prepared row group.
fn parquet_write_flush_batch(
    _: &dyn FunctionData,
    state: &CopyGlobalState,
    batch: rustdb_function::copy::PreparedBatch,
) -> Result<()> {
    let global = global_state(state)?;
    let prepared = batch
        .downcast::<crate::writer::PreparedRowGroup>()
        .map_err(|_| Error::internal("parquet flush of a batch it did not prepare"))?;
    let mut inner = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
    inner.0.flush_prepared(*prepared)
}

/// The approximate in-memory size of a chunk (`ColumnDataCollection::SizeInBytes`).
fn chunk_size_in_bytes(chunk: &DataChunk) -> usize {
    chunk
        .columns()
        .iter()
        .map(|column| {
            let width = column.logical_type().physical_type().size().max(1);
            let mut size = width * chunk.len();
            if column.logical_type().physical_type() == rustdb_types::PhysicalType::Varchar
                && column.kind() == rustdb_types::VectorKind::Flat
            {
                size += (0..chunk.len())
                    .filter(|&row| !column.is_null(row))
                    .map(|row| column.string_bytes(row).len())
                    .filter(|&len| len > 12)
                    .sum::<usize>();
            }
            size
        })
        .sum()
}

/// `ParquetWriteSink`: buffers rows, writing a row group once it is full.
fn parquet_write_sink(data: &dyn FunctionData, state: &CopyGlobalState, chunk: &DataChunk) -> Result<()> {
    let data = bind_data(data)?;
    let global = global_state(state)?;
    let mut inner = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
    let (writer, buffer) = &mut *inner;
    if chunk.is_empty() {
        return Ok(());
    }
    buffer.rows += chunk.len();
    buffer.bytes += chunk_size_in_bytes(chunk);
    buffer.chunks.push(chunk.clone());
    if buffer.rows >= data.options.row_group_size || buffer.bytes >= data.options.row_group_size_bytes {
        global.log_flushing_row_group(buffer, row_group_size_reason(data, buffer))?;
        writer.flush(&buffer.chunks)?;
        *buffer = RowBuffer::default();
    }
    Ok(())
}

/// `ParquetWriteCombine` + `ParquetWriteFinalize`.
fn parquet_write_finalize(data: &dyn FunctionData, state: &CopyGlobalState) -> Result<()> {
    let data = bind_data(data)?;
    let global = global_state(state)?;
    // The rows threads combined in parallel writes.
    let combined = global
        .combine_buffer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    if let Some(combined) = combined {
        global.log_flushing_row_group(&combined, "Finalize")?;
        flush_rows(global, &combined)?;
    }
    let mut inner = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
    let (writer, buffer) = &mut *inner;
    if buffer.rows > 0 {
        // DuckDB's regular write combines this buffer first: written then if it holds at
        // least half a row group, else at finalize.
        let reason = if buffer.rows >= data.options.row_group_size / 2
            || buffer.bytes >= data.options.row_group_size_bytes / 2
        {
            "Combine"
        } else {
            "Finalize"
        };
        global.log_flushing_row_group(buffer, reason)?;
        writer.flush(&buffer.chunks)?;
        *buffer = RowBuffer::default();
    }
    writer.finalize()
}

/// `ParquetWriteGetWrittenStatistics`.
fn parquet_write_get_written_statistics(
    _: &dyn FunctionData,
    state: &CopyGlobalState,
) -> Result<rustdb_function::copy::CopyFunctionFileStatistics> {
    let global = global_state(state)?;
    let inner = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
    Ok(inner.0.written_statistics())
}

/// The bytes written to the file so far (for `FILE_SIZE_BYTES`).
fn parquet_write_file_size(state: &CopyGlobalState) -> u64 {
    global_state(state).map_or(0, |global| {
        global
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .file_size()
    })
}

/// `ParquetWriteRotateFiles`.
fn parquet_write_rotate_files(bind_data: &dyn FunctionData, file_size_bytes: Option<u64>) -> bool {
    file_size_bytes.is_some()
        || bind_data
            .as_any()
            .downcast_ref::<ParquetWriteBindData>()
            .is_some_and(|data| data.options.row_groups_per_file.is_some())
}

/// `ParquetWriteRotateNextFile`.
fn parquet_write_rotate_next_file(
    state: &CopyGlobalState,
    bind_data: &dyn FunctionData,
    file_size_bytes: Option<u64>,
) -> bool {
    let Ok(global) = global_state(state) else {
        return false;
    };
    let inner = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
    let writer = &inner.0;
    if file_size_bytes.is_some_and(|limit| writer.file_size() > limit) {
        return true;
    }
    bind_data
        .as_any()
        .downcast_ref::<ParquetWriteBindData>()
        .and_then(|data| data.options.row_groups_per_file)
        .is_some_and(|limit| writer.row_group_count() as u64 >= limit)
}

/// `MultiFileBindCopy` for Parquet.
fn parquet_copy_from_bind(
    input: &CopyFromBindInput<'_>,
    expected_names: &[rustdb_common::ByteString],
    expected_types: &[LogicalType],
) -> Result<Arc<dyn FunctionData>> {
    let context = client_context(input.binder)?;
    let files = rustdb_csv::multi_file::create_file_list(
        "COPY",
        &[rustdb_common::ByteString::from(input.file_path)],
        "parquet",
    )?;
    let mut options = ParquetOptions {
        geoparquet_conversion: crate::scan::setting_bool_checked(context, "enable_geoparquet_conversion")?,
        ..ParquetOptions::default()
    };
    crate::scan::set_file_cache_options(context, &mut options)?;
    for (name, values) in input.options.iter() {
        let option = name.to_ascii_lowercase();
        let boolean = || -> Result<bool> {
            use rustdb_function::cast::ValueCast as _;
            let Some(value) = values.first() else {
                return Ok(true);
            };
            value
                .try_cast_as(&context.casts, &LogicalType::BOOLEAN, false)
                .map(|value| value.as_bool().unwrap_or(false))
                .map_err(|_| {
                    Error::invalid_input(rustdb_common::byte_format!(
                        "Unable to cast \"{}\" to BOOLEAN for Parquet option \"{option}\"",
                        value.to_varchar().unwrap_or_default()
                    ))
                })
        };
        match option.as_bytes() {
            b"compression" | b"codec" | b"row_group_size" | b"debug_use_openssl" => {}
            b"binary_as_string" => options.binary_as_string = boolean()?,
            b"file_row_number" => options.file_row_number = boolean()?,
            b"can_have_nan" => {
                if values.len() != 1 {
                    return Err(Error::binder("Parquet can_have_nan cannot be empty!"));
                }
                options.can_have_nan = boolean()?;
            }
            b"encryption_config" => {
                let value = values
                    .first()
                    .ok_or_else(|| Error::binder("Parquet encryption_config cannot be empty"))?;
                let cache = context.function_client.object_cache();
                options.encryption = Some(crate::crypto::parse_encryption_config(value, cache.as_deref())?);
            }
            _ => {
                return Err(Error::not_implemented(rustdb_common::byte_format!(
                    "Unsupported option for COPY FROM: {name}"
                )));
            }
        }
    }
    let file_options = rustdb_csv::multi_file::MultiFileOptions {
        auto_detect_hive_partitioning: false,
        ..rustdb_csv::multi_file::MultiFileOptions::default()
    };
    let mut data: ParquetBindData =
        bind_files_with_client(&context.casts, &context.function_client, files, file_options, &options)?;
    if expected_types.len() != data.types.len() {
        let table: Vec<rustdb_common::ByteString> = expected_names
            .iter()
            .zip(expected_types)
            .map(|(name, ty)| rustdb_common::byte_format!("{name} {ty}"))
            .collect();
        let parquet: Vec<rustdb_common::ByteString> = data
            .names
            .iter()
            .zip(&data.types)
            .map(|(name, ty)| rustdb_common::byte_format!("{name} {ty}"))
            .collect();
        return Err(Error::conversion(rustdb_common::byte_format!(
            "Failed to read file(s) \"{}\" - column count mismatch: expected {} columns but found {}\nTable schema: {}\nParquet schema: {}\n\nPossible solutions:\n* Manually specify which columns to insert using \"INSERT INTO tbl SELECT ... FROM read_parquet(...)\"",
            data.files.first_file().unwrap_or_default(),
            expected_types.len(),
            data.types.len(),
            rustdb_common::ByteString::join(&table, b", "),
            rustdb_common::ByteString::join(&parquet, b", ")
        )));
    }
    data.types = expected_types.to_vec();
    data.table_columns = expected_names.to_vec();
    Ok(Arc::new(data))
}

/// `TypeVisitor::Contains`: whether `ty` or a type nested in it satisfies `predicate`.
pub(crate) fn type_contains(ty: &LogicalType, predicate: &impl Fn(&LogicalType) -> bool) -> bool {
    if predicate(ty) {
        return true;
    }
    match ty.id() {
        LogicalTypeId::Struct => ty
            .struct_children()
            .is_some_and(|children| children.iter().any(|(_, child)| type_contains(child, predicate))),
        LogicalTypeId::Union => ty
            .union_members()
            .is_some_and(|members| members.iter().any(|(_, member)| type_contains(member, predicate))),
        LogicalTypeId::List | LogicalTypeId::Array => ty
            .child_type()
            .is_some_and(|child| type_contains(child, predicate)),
        LogicalTypeId::Map => ty
            .map_key_value()
            .is_some_and(|(key, value)| type_contains(key, predicate) || type_contains(value, predicate)),
        _ => false,
    }
}

/// `TypeVisitor::VisitReplace`: `ty` with `replace` applied bottom-up to every type in it
/// (rebuilt nested types lose their alias, like DuckDB's).
fn type_replace(ty: &LogicalType, replace: &impl Fn(&LogicalType) -> LogicalType) -> LogicalType {
    let rebuilt = match ty.id() {
        LogicalTypeId::Struct => ty.struct_children().map(|children| {
            LogicalType::struct_type(
                children
                    .iter()
                    .map(|(name, child)| (name.clone(), type_replace(child, replace)))
                    .collect(),
            )
        }),
        LogicalTypeId::Union => ty.union_members().map(|members| {
            LogicalType::union_type(
                members
                    .iter()
                    .map(|(name, member)| (name.clone(), type_replace(member, replace)))
                    .collect(),
            )
        }),
        LogicalTypeId::List => ty
            .child_type()
            .map(|child| LogicalType::list(type_replace(child, replace))),
        LogicalTypeId::Array => ty
            .array_child()
            .map(|(child, size)| LogicalType::array(type_replace(child, replace), size)),
        LogicalTypeId::Map => ty
            .map_key_value()
            .map(|(key, value)| LogicalType::map(type_replace(key, replace), type_replace(value, replace))),
        _ => None,
    };
    replace(rebuilt.as_ref().unwrap_or(ty))
}

/// `ParquetWriteSelect`: EXPORT DATABASE writes HUGEINT/UHUGEINT (which Parquet stores as
/// lossy doubles) as VARCHAR, and types Parquet has no representation for are written
/// as VARCHAR.
fn parquet_write_select(input: &CopyToSelectInput<'_>) -> Option<Vec<LogicalType>> {
    let is_lossy = |ty: &LogicalType| matches!(ty.id(), LogicalTypeId::HugeInt | LogicalTypeId::UHugeInt);
    let is_not_supported =
        |ty: &LogicalType| !ty.is_nested() && crate::writer::column::try_parquet_type(ty).is_none();
    let mut any_change = false;
    let mut result = Vec::with_capacity(input.types.len());
    for ty in input.types {
        let replace_with_varchar = |predicate: &dyn Fn(&LogicalType) -> bool| {
            type_replace(ty, &|ty: &LogicalType| {
                if predicate(ty) {
                    LogicalType::VARCHAR
                } else {
                    ty.clone()
                }
            })
        };
        if input.copy_to_type == CopyToType::ExportDatabase && type_contains(ty, &is_lossy) {
            result.push(replace_with_varchar(&is_lossy));
            any_change = true;
        } else if type_contains(ty, &is_not_supported) {
            result.push(replace_with_varchar(&is_not_supported));
            any_change = true;
        } else {
            result.push(ty.clone());
        }
    }
    any_change.then_some(result)
}

/// The `parquet` copy function.
/// The entries of a `case_insensitive_map_t` filled in `entries`' order, in libc++'s
/// iteration order.
fn case_insensitive_map_order<'a, V>(
    entries: Vec<(&'a rustdb_common::ByteString, V)>,
) -> Vec<(&'a rustdb_common::ByteString, V)> {
    let mut map = rustdb_common::libcxx::UnorderedOrder::default();
    for (index, (name, _)) in entries.iter().enumerate() {
        map.insert(
            rustdb_common::libcxx::ci_hash(name),
            index,
            |a: &usize, b: &usize| entries[*a].0.eq_ignore_ascii_case(entries[*b].0),
        );
    }
    let order: Vec<usize> = map.order().into_iter().map(|node| map.keys()[node]).collect();
    let mut entries: Vec<Option<(&'a rustdb_common::ByteString, V)>> =
        entries.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|index| entries[index].take())
        .collect()
}

/// `ChildFieldIDs::Serialize`.
fn serialize_child_field_ids(
    serializer: &mut dyn rustdb_serde::Serializer,
    field_ids: &CaseInsensitiveMap<FieldId, rustdb_common::ByteString>,
) -> Result<()> {
    let entries = case_insensitive_map_order(field_ids.iter().collect());
    serializer.write_optional_with(100, "ids", entries.is_empty(), |serializer| {
        serializer.on_list_begin(entries.len());
        for (name, field_id) in &entries {
            serializer.on_object_begin();
            serializer.write_property(0, "key", *name)?;
            serializer.write_object_property(1, "value", |serializer| {
                // `FieldID::Serialize`; an unset id is left uninitialized by DuckDB.
                serializer.write_property_with_default(100, "set", &field_id.field_id.is_some())?;
                serializer.write_property_with_default(101, "field_id", &field_id.field_id.unwrap_or(0))?;
                serializer.write_object_property(102, "child_field_ids", |serializer| {
                    serialize_child_field_ids(serializer, &field_id.children)
                })
            })?;
            serializer.on_object_end();
        }
        serializer.on_list_end();
        Ok(())
    })
}

/// `ShreddingType::Serialize` of the shredding of `ty` (`ConvertShreddingTypeRecursive`),
/// or of the root (`None`) over the shredded columns `children`.
fn serialize_shredding_type(
    serializer: &mut dyn rustdb_serde::Serializer,
    ty: Option<&LogicalType>,
    children: &[(rustdb_common::ByteString, LogicalType)],
) -> Result<()> {
    let ty = match ty {
        None => LogicalType::INVALID,
        Some(ty) if ty.id() == LogicalTypeId::Variant => LogicalType::ANY,
        Some(ty) => ty.clone(),
    };
    serializer.write_property_with_default(100, "set", &(ty.id() != LogicalTypeId::Invalid))?;
    serializer.write_property(101, "type", &ty)?;
    let children = case_insensitive_map_order(children.iter().map(|(name, ty)| (name, ty.clone())).collect());
    serializer.write_object_property(102, "children", |serializer| {
        // `ChildShreddingTypes::Serialize`.
        serializer.write_optional_with(100, "types", children.is_empty(), |serializer| {
            serializer.on_list_begin(children.len());
            for (name, child) in &children {
                serializer.on_object_begin();
                serializer.write_property(0, "key", *name)?;
                let grandchildren: Vec<(rustdb_common::ByteString, LogicalType)> = match child.id() {
                    LogicalTypeId::Struct => child
                        .struct_children()
                        .unwrap_or_default()
                        .iter()
                        .map(|(name, ty)| (name.clone(), ty.clone()))
                        .collect(),
                    LogicalTypeId::List => child
                        .child_type()
                        .map(|element| vec![("element".into(), element.clone())])
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                serializer.write_object_property(1, "value", |serializer| {
                    serialize_shredding_type(serializer, Some(child), &grandchildren)
                })?;
                serializer.on_object_end();
            }
            serializer.on_list_end();
            Ok(())
        })
    })
}

/// `ParquetCopySerialize`: the columns and options of the write.
fn serialize_parquet_copy(
    serializer: &mut dyn rustdb_serde::Serializer,
    bind_data: &dyn FunctionData,
    _: &CopyFunction,
) -> Result<()> {
    use rustdb_serde::OptionalIdx;
    let data = bind_data
        .as_any()
        .downcast_ref::<ParquetWriteBindData>()
        .ok_or_else(|| Error::internal("parquet COPY without its bind data"))?;
    let options = &data.options;
    let defaults = WriterOptions::default();
    serializer.write_property(100, "sql_types", &data.types)?;
    serializer.write_property(101, "column_names", &data.names)?;
    serializer.write_property(102, "codec", options.codec.name().unwrap_or_default())?;
    serializer.write_property(103, "row_group_size", &(options.row_group_size as u64))?;
    serializer.write_property(
        104,
        "row_group_size_bytes",
        &(options.row_group_size_bytes as u64),
    )?;
    serializer.on_property_begin(105, "kv_metadata");
    serializer.on_list_begin(options.kv_metadata.len());
    for (key, value) in &options.kv_metadata {
        serializer.on_object_begin();
        serializer.write_property(0, "first", key)?;
        serializer.on_property_begin(1, "second");
        serializer.write_string_bytes(value);
        serializer.on_property_end();
        serializer.on_object_end();
    }
    serializer.on_list_end();
    serializer.on_property_end();
    serializer.write_object_property(106, "field_ids", |serializer| {
        serialize_child_field_ids(serializer, &options.field_ids)
    })?;
    serializer.write_optional_with(
        107,
        "encryption_config",
        options.encryption.is_none(),
        |serializer| crate::crypto::serialize_encryption_config(serializer, options.encryption.as_ref()),
    )?;
    // `SerializeCompressionLevel`: a negative level counts down from the largest index.
    let compression_level = if options.compression_level < 0 {
        u64::MAX - options.compression_level.unsigned_abs()
    } else {
        options.compression_level.unsigned_abs()
    };
    serializer.write_property_with_explicit_default(
        109,
        "compression_level",
        &OptionalIdx(Some(compression_level)),
        &OptionalIdx(None),
    )?;
    serializer.write_property_with_explicit_default(
        110,
        "row_groups_per_file",
        &OptionalIdx(options.row_groups_per_file),
        &OptionalIdx(None),
    )?;
    serializer.write_property_with_explicit_default(
        112,
        "dictionary_size_limit",
        &OptionalIdx(options.dictionary_size_limit.map(|limit| limit as u64)),
        &OptionalIdx(None),
    )?;
    serializer.write_optional(
        113,
        "bloom_filter_false_positive_ratio",
        options.bloom_filter_false_positive_ratio.to_bits()
            == defaults.bloom_filter_false_positive_ratio.to_bits(),
        &options.bloom_filter_false_positive_ratio,
    )?;
    let parquet_version = match options.version {
        ParquetVersion::V1 => "V1",
        ParquetVersion::V2 => "V2",
    };
    serializer.write_optional(
        114,
        "parquet_version",
        options.version == defaults.version,
        parquet_version,
    )?;
    serializer.write_optional(
        115,
        "string_dictionary_page_size_limit",
        options.string_dictionary_page_size_limit == defaults.string_dictionary_page_size_limit,
        &(options.string_dictionary_page_size_limit as u64),
    )?;
    let geoparquet_version = match options.geoparquet_version {
        crate::geo::GeoParquetVersion::V1 => "V1",
        crate::geo::GeoParquetVersion::V2 => "V2",
        crate::geo::GeoParquetVersion::Both => "BOTH",
        crate::geo::GeoParquetVersion::None => "NONE",
    };
    serializer.write_optional(
        116,
        "geoparquet_version",
        options.geoparquet_version == defaults.geoparquet_version,
        geoparquet_version,
    )?;
    let shredded: Vec<(rustdb_common::ByteString, LogicalType)> = options
        .shredding
        .iter()
        .map(|(column, ty)| (column.clone(), ty.clone()))
        .collect();
    serializer.write_optional_with(117, "shredding_types", shredded.is_empty(), |serializer| {
        serializer.write_object_with(|serializer| serialize_shredding_type(serializer, None, &shredded))
    })
}

pub fn parquet_copy_function() -> CopyFunction {
    let mut function = CopyFunction::new("parquet");
    "parquet".clone_into(&mut function.extension);
    function.serialize = Some(Arc::new(serialize_parquet_copy));
    function.options = parquet_copy_options();
    function.copy_to_select = Some(Arc::new(|input| Ok(parquet_write_select(input))));
    function.copy_to_bind = Some(Arc::new(parquet_write_bind));
    function.copy_to_initialize_global = Some(Arc::new(parquet_write_initialize_global));
    function.copy_to_initialize_operator = Some(Arc::new(parquet_write_initialize_operator));
    function.copy_to_sink = Some(Arc::new(parquet_write_sink));
    function.execution_mode = Some(Arc::new(parquet_write_execution_mode));
    function.prepare_batch = Some(Arc::new(parquet_write_prepare_batch));
    function.flush_batch = Some(Arc::new(parquet_write_flush_batch));
    // `ParquetWriteDesiredBatchSize`.
    function.copy_to_initialize_local = Some(Arc::new(|_| {
        Ok(Box::new(RowBuffer::default()) as rustdb_function::copy::CopyLocalState)
    }));
    function.copy_to_sink_local = Some(Arc::new(parquet_write_sink_local));
    function.copy_to_combine = Some(Arc::new(parquet_write_combine));
    function.desired_batch_size = Some(Arc::new(|data| {
        bind_data(data).map_or(0, |data| data.options.row_group_size as u64)
    }));
    function.copy_to_finalize = Some(Arc::new(parquet_write_finalize));
    function.copy_to_get_written_statistics = Some(Arc::new(parquet_write_get_written_statistics));
    function.copy_to_file_size = Some(Arc::new(parquet_write_file_size));
    function.rotate_files = Some(Arc::new(parquet_write_rotate_files));
    function.rotate_next_file = Some(Arc::new(parquet_write_rotate_next_file));
    function.copy_from_bind = Some(Arc::new(parquet_copy_from_bind));
    function.copy_from_function = crate::scan::read_parquet_functions()
        .into_iter()
        .next()
        .and_then(|set| set.functions.into_iter().next());
    function
}
