//! The Parquet writer (DuckDB's `ParquetWriter` and `ColumnWriter::CreateWriterRecursive`):
//! buffers rows into row groups, encodes each row group column by column and writes the
//! footer.

pub mod bloom;
pub mod column;
pub mod encoders;
pub mod stats;
pub mod types;
pub mod values;
pub mod written_stats;

use std::fs::File;
use std::io::{BufWriter, Write as _};

use rustdb_common::{CaseInsensitiveMap, Error, ErrorKind, Result};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, PhysicalType, Vector};

use self::column::{
    ColumnWriter, FinishedColumn, ListWriter, PrimitiveWriter, StructWriter, VariantWriter, WriterSchema,
};
use self::types::{Bytes, Float, HugeIntAsDouble, IntervalType, Numeric, Uuid};
use self::values::{
    BooleanWriter, EnumWriter, FixedDecimalWriter, StandardWriter, ValueWriter, deep_flatten,
};
use crate::thrift::{
    BloomFilterAlgorithm, BloomFilterCompression, BloomFilterHash, BloomFilterHeader, ColumnOrder,
    CompressionCodec, FieldRepetitionType, FileMetaData, KeyValue, RowGroup, SchemaElement,
    SplitBlockAlgorithm, ThriftValue, TypeDefinedOrder, Uncompressed, XxHash,
};

/// The format version written (`ParquetVersion`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ParquetVersion {
    #[default]
    V1,
    V2,
}

/// The default number of rows per row group (`DEFAULT_ROW_GROUP_SIZE`).
pub const DEFAULT_ROW_GROUP_SIZE: usize = 122_880;
/// The default limit of a string dictionary page (`MAX_UNCOMPRESSED_DICT_PAGE_SIZE`).
pub const MAX_UNCOMPRESSED_DICT_PAGE_SIZE: usize = 1_073_741_824;

/// The field id of a column and of its children (`FieldID` / `ChildFieldIDs`).
#[derive(Clone, Debug, Default)]
pub struct FieldId {
    pub field_id: Option<i32>,
    pub children: CaseInsensitiveMap<FieldId>,
}

/// The options of a Parquet write (`ParquetWriteBindData`).
#[derive(Clone, Debug)]
pub struct WriterOptions {
    pub codec: CompressionCodec,
    pub compression_level: i64,
    pub row_group_size: usize,
    pub row_group_size_bytes: usize,
    pub row_groups_per_file: Option<u64>,
    pub kv_metadata: Vec<(String, Vec<u8>)>,
    pub field_ids: CaseInsensitiveMap<FieldId>,
    pub dictionary_size_limit: Option<usize>,
    pub string_dictionary_page_size_limit: usize,
    pub enable_bloom_filters: bool,
    pub bloom_filter_false_positive_ratio: f64,
    pub version: ParquetVersion,
    /// The shredding types of VARIANT columns (`SHREDDING`), by column name.
    pub shredding: Vec<(String, LogicalType)>,
    /// The `GeoParquet` metadata and geometry types written (`GEOPARQUET_VERSION`).
    pub geoparquet_version: crate::geo::GeoParquetVersion,
    /// Whether `GeoParquet` metadata is written at all (`enable_geoparquet_conversion`).
    pub geoparquet_conversion: bool,
}

impl Default for WriterOptions {
    fn default() -> Self {
        Self {
            codec: CompressionCodec::SNAPPY,
            compression_level: 3,
            row_group_size: DEFAULT_ROW_GROUP_SIZE,
            row_group_size_bytes: usize::MAX,
            row_groups_per_file: None,
            kv_metadata: Vec::new(),
            field_ids: CaseInsensitiveMap::default(),
            dictionary_size_limit: None,
            string_dictionary_page_size_limit: MAX_UNCOMPRESSED_DICT_PAGE_SIZE,
            enable_bloom_filters: true,
            bloom_filter_false_positive_ratio: 0.01,
            version: ParquetVersion::V1,
            geoparquet_version: crate::geo::GeoParquetVersion::V1,
            geoparquet_conversion: true,
            shredding: Vec::new(),
        }
    }
}

/// The output file with the number of bytes written (`BufferedFileWriter`).
#[derive(Debug)]
pub struct FileSink {
    out: BufWriter<File>,
    written: u64,
    path: String,
}

impl FileSink {
    fn create(path: &str) -> Result<Self> {
        rustdb_function::file_system::verify_can_write(path)?;
        let file = File::create(path).map_err(|error| {
            Error::new(
                ErrorKind::Io,
                format!("Cannot open file \"{path}\": {}", io_reason(&error)),
            )
        })?;
        Ok(Self {
            out: BufWriter::with_capacity(1 << 20, file),
            written: 0,
            path: path.to_owned(),
        })
    }

    /// `GetTotalWritten`.
    pub fn written(&self) -> u64 {
        self.written
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.out.write_all(bytes).map_err(|error| {
            Error::new(
                ErrorKind::Io,
                format!("Could not write file \"{}\": {}", self.path, io_reason(&error)),
            )
        })?;
        self.written += bytes.len() as u64;
        Ok(())
    }

    /// Writes a Thrift structure; returns its size.
    pub fn write_thrift<T: ThriftValue>(&mut self, value: &T) -> Result<u64> {
        let bytes = crate::thrift::write_to_vec(value);
        self.write_all(&bytes)?;
        Ok(bytes.len() as u64)
    }

    fn close(mut self) -> Result<()> {
        self.out.flush().map_err(|error| {
            Error::new(
                ErrorKind::Io,
                format!("Could not write file \"{}\": {}", self.path, io_reason(&error)),
            )
        })
    }
}

fn io_reason(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory".to_owned(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".to_owned(),
        _ => error.to_string(),
    }
}

/// The `created_by` of written files.
pub fn created_by() -> String {
    format!(
        "DuckDB version v1.5.6 (build {})",
        rustdb_storage::storage_info::SOURCE_ID
    )
}

/// A bloom filter waiting to be written before the footer.
#[derive(Debug)]
struct PendingBloomFilter {
    row_group: usize,
    column: usize,
    filter: bloom::BloomFilter,
}

/// Writes one Parquet file (`ParquetWriter`).
#[derive(Debug)]
pub struct ParquetWriter {
    sink: Option<FileSink>,
    options: WriterOptions,
    names: Vec<String>,
    types: Vec<LogicalType>,
    columns: Vec<ColumnWriter>,
    metadata: FileMetaData,
    bloom_filters: Vec<PendingBloomFilter>,
    /// The finished column chunks of every row group, for `RETURN_STATS`.
    pub finished: Vec<Vec<FinishedColumn>>,
    /// The footer size, once written.
    pub footer_size: Option<u64>,
    /// The size of the finished file.
    final_size: u64,
    /// The `GeoParquet` metadata gathered from the geometry columns.
    geo: crate::geo::GeoParquetMetadata,
}

/// Prepares row groups of a file without its writer, so several threads can encode
/// batches at once (`ParquetWritePrepareBatch`).
#[derive(Debug, Clone)]
pub struct RowGroupPreparer {
    names: Vec<String>,
    types: Vec<LogicalType>,
    options: WriterOptions,
}

impl RowGroupPreparer {
    /// Column writers like the file writer's own.
    fn fresh_columns(&self) -> Result<Vec<ColumnWriter>> {
        let mut columns = Vec::with_capacity(self.types.len());
        for (name, ty) in self.names.iter().zip(&self.types) {
            columns.push(create_writer(
                &[],
                ty,
                name,
                Some(&self.options.field_ids),
                0,
                1,
                true,
            )?);
        }
        let mut schema = Vec::new();
        for column in &mut columns {
            column.finalize_schema(&mut schema, &self.options)?;
        }
        Ok(columns)
    }

    /// `PrepareRowGroup` on column writers of its own (only for
    /// [`ParquetWriter::supports_batches`] types).
    pub fn prepare(&self, chunks: &[DataChunk]) -> Result<PreparedRowGroup> {
        let num_rows: usize = chunks.iter().map(DataChunk::len).sum();
        let mut columns = self.fresh_columns()?;
        let row_group = if num_rows == 0 {
            RowGroup::default()
        } else {
            let flat_chunks = ParquetWriter::flat_chunks(chunks)?;
            ParquetWriter::encode_row_group(&mut columns, &flat_chunks, num_rows, &self.options)?
        };
        Ok(PreparedRowGroup { row_group, columns })
    }
}

/// A row group encoded off the writer (`PreparedRowGroup`), written later in order.
#[derive(Debug)]
pub struct PreparedRowGroup {
    row_group: RowGroup,
    columns: Vec<ColumnWriter>,
}

/// `ColumnWriter::CreateWriterRecursive`.
fn create_writer(
    path: &[String],
    ty: &LogicalType,
    name: &str,
    field_ids: Option<&CaseInsensitiveMap<FieldId>>,
    max_repeat: u16,
    mut max_define: u16,
    can_have_nulls: bool,
) -> Result<ColumnWriter> {
    let mut path = path.to_vec();
    path.push(name.to_owned());
    if !can_have_nulls {
        max_define -= 1;
    }
    let repetition = if can_have_nulls {
        FieldRepetitionType::OPTIONAL
    } else {
        FieldRepetitionType::REQUIRED
    };
    let field_id = field_ids.and_then(|ids| ids.get(name));
    let child_ids = field_id.map(|id| &id.children);
    let schema = WriterSchema {
        name: name.to_owned(),
        logical_type: ty.clone(),
        max_define,
        max_repeat,
        repetition,
        field_id: field_id.and_then(|id| id.field_id),
        path: path.clone(),
    };
    match ty.id() {
        LogicalTypeId::Struct | LogicalTypeId::Union => {
            let children = ty
                .struct_children()
                .or_else(|| ty.union_members())
                .ok_or_else(|| Error::internal("STRUCT type without children"))?;
            let mut writers = Vec::with_capacity(children.len());
            for (child_name, child_type) in children {
                writers.push(create_writer(
                    &path,
                    child_type,
                    child_name,
                    child_ids,
                    max_repeat,
                    max_define + 1,
                    true,
                )?);
            }
            Ok(ColumnWriter::Struct(StructWriter::new(schema, writers)))
        }
        LogicalTypeId::List | LogicalTypeId::Array => {
            let is_list = ty.id() == LogicalTypeId::List;
            let child_type = if is_list {
                ty.child_type().cloned()
            } else {
                ty.array_child().map(|(child, _)| child.clone())
            }
            .ok_or_else(|| Error::internal("LIST type without a child"))?;
            path.push("list".to_owned());
            let child = create_writer(
                &path,
                &child_type,
                "element",
                child_ids,
                max_repeat + 1,
                max_define + 2,
                true,
            )?;
            Ok(ColumnWriter::List(Box::new(ListWriter::new(
                schema, child, !is_list,
            ))))
        }
        LogicalTypeId::Map => {
            path.push("key_value".to_owned());
            let (key, value) = ty
                .map_key_value()
                .ok_or_else(|| Error::internal("MAP type without key/value"))?;
            let key_writer = create_writer(
                &path,
                key,
                "key",
                child_ids,
                max_repeat + 1,
                max_define + 2,
                false,
            )?;
            let value_writer = create_writer(
                &path,
                value,
                "value",
                child_ids,
                max_repeat + 1,
                max_define + 2,
                true,
            )?;
            let key_value_type = LogicalType::struct_type(vec![
                ("key".to_owned(), key.clone()),
                ("value".to_owned(), value.clone()),
            ]);
            let key_value_schema = WriterSchema {
                name: "key_value".to_owned(),
                logical_type: key_value_type,
                max_define: max_define + 1,
                max_repeat: max_repeat + 1,
                repetition: FieldRepetitionType::REPEATED,
                field_id: None,
                path: path.clone(),
            };
            let struct_writer = ColumnWriter::Struct(StructWriter::new(
                key_value_schema,
                vec![key_writer, value_writer],
            ));
            Ok(ColumnWriter::List(Box::new(ListWriter::new(
                schema,
                struct_writer,
                false,
            ))))
        }
        LogicalTypeId::Variant => {
            let mut writer = VariantWriter {
                schema,
                inner: ColumnWriter::Struct(StructWriter::new(
                    WriterSchema {
                        name: String::new(),
                        logical_type: LogicalType::INVALID,
                        max_define: 0,
                        max_repeat: 0,
                        repetition: FieldRepetitionType::REQUIRED,
                        field_id: None,
                        path: Vec::new(),
                    },
                    Vec::new(),
                )),
                shredding: None,
                analyzed: false,
            };
            build_variant_children(&mut writer, None)?;
            Ok(ColumnWriter::Variant(Box::new(writer)))
        }
        _ => {
            let values = primitive_values(ty)?;
            Ok(ColumnWriter::Primitive(Box::new(PrimitiveWriter::new(
                schema, values,
            ))))
        }
    }
}

/// `CreateWriterRecursive` for VARIANT (and `AnalyzeSchemaFinalize`): the `metadata`,
/// `value` and, when shredded on `shredding`, `typed_value` writers.
fn build_variant_children(writer: &mut VariantWriter, shredding: Option<&LogicalType>) -> Result<()> {
    let schema = &writer.schema;
    let path = schema.path.clone();
    let parent_path = &path[..path.len().saturating_sub(1)];
    let column_path: Vec<String> = path.clone();
    let _ = parent_path;
    let typed = shredding
        .filter(|ty| !matches!(ty.id(), LogicalTypeId::Any | LogicalTypeId::Variant))
        .map(crate::variant::write::transform_typed_value)
        .transpose()?;
    let is_shredded = shredding.is_some();
    let child_define = schema.max_define + 1;
    let mut children = vec![
        create_writer(
            &column_path,
            &LogicalType::BLOB,
            "metadata",
            None,
            schema.max_repeat,
            child_define,
            false,
        )?,
        create_writer(
            &column_path,
            &LogicalType::BLOB,
            "value",
            None,
            schema.max_repeat,
            child_define,
            is_shredded,
        )?,
    ];
    let mut fields = vec![
        ("metadata".to_owned(), LogicalType::BLOB),
        ("value".to_owned(), LogicalType::BLOB),
    ];
    if let Some(typed) = &typed {
        children.push(create_writer(
            &column_path,
            typed,
            "typed_value",
            None,
            schema.max_repeat,
            child_define,
            true,
        )?);
        fields.push(("typed_value".to_owned(), typed.clone()));
    }
    let inner_schema = WriterSchema {
        logical_type: LogicalType::struct_type(fields),
        ..schema.clone()
    };
    writer.inner = ColumnWriter::Struct(StructWriter::new(inner_schema, children));
    writer.shredding = shredding.cloned();
    Ok(())
}

/// Transforms the VARIANT columns of a row group into their Parquet structs
/// (`AnalyzeSchema`, then the `variant_to_parquet_variant` transform), shredding on the
/// type found in the first row group when none was given.
fn transform_variant_columns(
    columns: &mut [ColumnWriter],
    chunks: &mut [(Vec<Vector>, usize)],
) -> Result<()> {
    use rustdb_types::variant::VariantData;
    for (index, column) in columns.iter_mut().enumerate() {
        let ColumnWriter::Variant(writer) = column else {
            continue;
        };
        let mut rows: Vec<Vec<Option<VariantData>>> = Vec::with_capacity(chunks.len());
        for (vectors, count) in chunks.iter() {
            let vector = &vectors[index];
            let mut chunk_rows = Vec::with_capacity(*count);
            for row in 0..*count {
                chunk_rows.push(VariantData::from_value(&vector.get_value(row))?);
            }
            rows.push(chunk_rows);
        }
        if writer.child_count() == 2 && !writer.analyzed {
            // `AnalyzeSchema` over the first row group.
            let mut analysis = crate::variant::write::AnalyzeData::default();
            for row in rows.iter().flatten() {
                analysis.analyze(row.as_ref(), 0)?;
            }
            writer.analyzed = true;
            if let Some(shredded) = analysis.shredded_type() {
                build_variant_children(writer, Some(&shredded))?;
            }
        }
        let struct_type = writer.transformed_type().clone();
        for ((vectors, count), chunk_rows) in chunks.iter_mut().zip(&rows) {
            let values = crate::variant::write::to_parquet_variant(
                chunk_rows,
                writer.shredding.as_ref(),
                &struct_type,
            )?;
            let mut vector = Vector::flat(struct_type.clone(), (*count).max(1));
            for (row, value) in values.iter().enumerate() {
                vector.set_value(row, value)?;
            }
            vectors[index] = vector;
        }
    }
    Ok(())
}

/// Applies the `SHREDDING` types of the top-level VARIANT columns.
fn apply_shredding(columns: &mut [ColumnWriter], names: &[String], options: &WriterOptions) -> Result<()> {
    for (column, name) in columns.iter_mut().zip(names) {
        if let ColumnWriter::Variant(writer) = column
            && let Some((_, ty)) = options
                .shredding
                .iter()
                .find(|(column, _)| column.eq_ignore_ascii_case(name))
        {
            let shredding = if ty.id() == LogicalTypeId::Variant {
                LogicalType::ANY
            } else {
                ty.clone()
            };
            build_variant_children(writer, Some(&shredding))?;
        }
    }
    Ok(())
}

fn standard<T: types::WriteType>(is_string: bool) -> Box<dyn ValueWriter>
where
    T::Key: std::fmt::Debug,
    T::Stats: std::fmt::Debug,
{
    Box::new(StandardWriter::<T>::new(is_string))
}

/// The value writer of a leaf type (the leaf cases of `CreateWriterRecursive`).
fn primitive_values(ty: &LogicalType) -> Result<Box<dyn ValueWriter>> {
    use LogicalTypeId as L;
    use types::{
        BigInt, Integer, SmallInt, TimeTz, TimestampSec, TinyInt, UBigInt, UInteger, USmallInt, UTinyInt,
    };
    Ok(match ty.id() {
        L::Boolean => Box::new(BooleanWriter::default()),
        L::TinyInt => standard::<Numeric<TinyInt>>(false),
        L::SmallInt => standard::<Numeric<SmallInt>>(false),
        L::Integer | L::Date => standard::<Numeric<Integer>>(false),
        L::BigInt | L::Time | L::TimeNs | L::Timestamp | L::TimestampTz | L::TimestampMs | L::TimestampNs => {
            standard::<Numeric<BigInt>>(false)
        }
        L::TimeTz => standard::<Numeric<TimeTz>>(false),
        L::HugeInt => standard::<HugeIntAsDouble<i128>>(false),
        L::UHugeInt => standard::<HugeIntAsDouble<u128>>(false),
        L::TimestampSec => standard::<Numeric<TimestampSec>>(false),
        L::UTinyInt => standard::<Numeric<UTinyInt>>(false),
        L::USmallInt => standard::<Numeric<USmallInt>>(false),
        L::UInteger => standard::<Numeric<UInteger>>(false),
        L::UBigInt => standard::<Numeric<UBigInt>>(false),
        L::Float => standard::<Float<f32>>(false),
        L::Double => standard::<Float<f64>>(false),
        L::Decimal => match ty.physical_type() {
            PhysicalType::Int16 => standard::<Numeric<SmallInt>>(false),
            PhysicalType::Int32 => standard::<Numeric<Integer>>(false),
            PhysicalType::Int64 => standard::<Numeric<BigInt>>(false),
            _ => Box::new(FixedDecimalWriter::default()),
        },
        L::Blob => standard::<Bytes<true>>(true),
        L::Geometry => standard::<types::Geometry>(true),
        L::Varchar => standard::<Bytes<false>>(true),
        L::Uuid => standard::<Uuid>(false),
        L::Interval => standard::<IntervalType>(false),
        L::Enum => Box::new(EnumWriter::new(ty.enum_values().unwrap_or_default().to_vec())),
        _ => {
            return Err(Error::internal(format!(
                "Unsupported type \"{ty}\" in Parquet writer"
            )));
        }
    })
}

impl ParquetWriter {
    /// Creates `path` and prepares the column writers (`ParquetWriter::ParquetWriter`).
    pub fn new(path: &str, names: &[String], types: &[LogicalType], options: WriterOptions) -> Result<Self> {
        let mut sink = FileSink::create(path)?;
        sink.write_all(b"PAR1")?;
        let mut columns = Vec::with_capacity(types.len());
        for (name, ty) in names.iter().zip(types) {
            columns.push(create_writer(
                &[],
                ty,
                name,
                Some(&options.field_ids),
                0,
                1,
                true,
            )?);
        }
        apply_shredding(&mut columns, names, &options)?;
        let key_value_metadata = (!options.kv_metadata.is_empty()).then(|| {
            options
                .kv_metadata
                .iter()
                .map(|(key, value)| KeyValue {
                    key: crate::thrift::Binary(key.clone().into_bytes()),
                    value: Some(crate::thrift::Binary(value.clone())),
                })
                .collect()
        });
        let metadata = FileMetaData {
            version: match options.version {
                ParquetVersion::V1 => 1,
                ParquetVersion::V2 => 2,
            },
            num_rows: 0,
            created_by: Some(created_by()),
            key_value_metadata,
            ..FileMetaData::default()
        };
        Ok(Self {
            sink: Some(sink),
            options,
            names: names.to_vec(),
            types: types.to_vec(),
            columns,
            metadata,
            bloom_filters: Vec::new(),
            finished: Vec::new(),
            footer_size: None,
            final_size: 0,
            geo: crate::geo::GeoParquetMetadata::default(),
        })
    }

    pub fn options(&self) -> &WriterOptions {
        &self.options
    }

    /// The bytes written so far (`FileSize`).
    pub fn file_size(&self) -> u64 {
        self.sink.as_ref().map_or(self.final_size, FileSink::written)
    }

    /// What `RETURN_STATS` reports about the finalized file (`GatherWrittenStatistics`).
    pub fn written_statistics(&self) -> rustdb_function::copy::CopyFunctionFileStatistics {
        written_stats::gather(
            &self.columns,
            &self.metadata,
            &self.finished,
            self.final_size,
            self.footer_size.unwrap_or(0),
        )
    }

    /// The number of row groups written (`NumberOfRowGroups`).
    pub fn row_group_count(&self) -> usize {
        self.metadata.row_groups.len()
    }

    pub fn metadata(&self) -> &FileMetaData {
        &self.metadata
    }

    pub fn types(&self) -> &[LogicalType] {
        &self.types
    }

    /// `InitializeSchemaElements`.
    fn initialize_schema(&mut self) -> Result<()> {
        if !self.metadata.schema.is_empty() {
            return Ok(());
        }
        let mut schema = vec![SchemaElement {
            name: "duckdb_schema".to_owned(),
            num_children: Some(self.types.len() as i32),
            repetition_type: Some(FieldRepetitionType::REQUIRED),
            ..SchemaElement::default()
        }];
        let mut leaves = 0;
        for column in &mut self.columns {
            leaves += column.finalize_schema(&mut schema, &self.options)?;
        }
        self.metadata.schema = schema;
        self.metadata.column_orders = Some(vec![
            ColumnOrder {
                type_order: Some(TypeDefinedOrder {}),
            };
            leaves
        ]);
        Ok(())
    }

    /// Whether row groups can be prepared on other threads (`prepare_batch`): VARIANT
    /// columns decide their shredding from the first row group the writer sees.
    pub fn supports_batches(types: &[LogicalType]) -> bool {
        !types
            .iter()
            .any(|ty| crate::copy::type_contains(ty, &|ty: &LogicalType| ty.id() == LogicalTypeId::Variant))
    }

    /// What prepares row groups of this file on other threads.
    pub fn preparer(&self) -> RowGroupPreparer {
        RowGroupPreparer {
            names: self.names.clone(),
            types: self.types.clone(),
            options: self.options.clone(),
        }
    }

    /// The vectors of `chunks` made flat (the writers index vector slots directly).
    fn flat_chunks(chunks: &[DataChunk]) -> Result<Vec<(Vec<Vector>, usize)>> {
        let mut flat_chunks = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            let columns = chunk
                .columns()
                .iter()
                .map(|column| deep_flatten(column, chunk.len()))
                .collect::<Result<Vec<_>>>()?;
            flat_chunks.push((columns, chunk.len()));
        }
        Ok(flat_chunks)
    }

    /// Encodes the rows of `flat_chunks` with `columns` (the column part of
    /// `PrepareRowGroup`): the pages are complete, the row group not yet written.
    fn encode_row_group(
        columns: &mut [ColumnWriter],
        flat_chunks: &[(Vec<Vector>, usize)],
        num_rows: usize,
        options: &WriterOptions,
    ) -> Result<RowGroup> {
        let mut row_group = RowGroup {
            num_rows: num_rows as i64,
            file_offset: Some(0),
            ..RowGroup::default()
        };
        for (index, column) in columns.iter_mut().enumerate() {
            column.initialize_write(&mut row_group.columns, num_rows, options);
            if column.has_analyze() {
                for (vectors, count) in flat_chunks {
                    column.analyze(None, &vectors[index], *count)?;
                }
                column.finalize_analyze(options);
            }
            for (vectors, count) in flat_chunks {
                column.prepare(None, &vectors[index], *count, true)?;
            }
            column.begin_write(options)?;
            for (vectors, count) in flat_chunks {
                column.write(&vectors[index], *count, options)?;
            }
        }
        Ok(row_group)
    }

    /// `FlushRowGroup` of a prepared row group.
    pub fn flush_prepared(&mut self, prepared: PreparedRowGroup) -> Result<()> {
        let PreparedRowGroup {
            row_group,
            mut columns,
        } = prepared;
        if row_group.num_rows == 0 {
            return Ok(());
        }
        self.initialize_schema()?;
        self.write_row_group(row_group, &mut columns)
    }

    /// Writes `chunks` as one row group (`Flush` = `PrepareRowGroup` + `FlushRowGroup`).
    pub fn flush(&mut self, chunks: &[DataChunk]) -> Result<()> {
        let num_rows: usize = chunks.iter().map(DataChunk::len).sum();
        if num_rows == 0 {
            return Ok(());
        }
        let mut flat_chunks = Self::flat_chunks(chunks)?;
        transform_variant_columns(&mut self.columns, &mut flat_chunks)?;
        self.initialize_schema()?;
        let mut columns = std::mem::take(&mut self.columns);
        let result = Self::encode_row_group(&mut columns, &flat_chunks, num_rows, &self.options)
            .and_then(|row_group| self.write_row_group(row_group, &mut columns));
        self.columns = columns;
        result
    }

    /// `FlushRowGroup`: writes the pages of `columns` and records the row group.
    fn write_row_group(&mut self, mut row_group: RowGroup, columns: &mut [ColumnWriter]) -> Result<()> {
        let options = self.options.clone();
        let sink = self
            .sink
            .as_mut()
            .ok_or_else(|| Error::internal("Parquet writer already finalized"))?;
        let file_offset = sink.written();
        row_group.file_offset = Some(file_offset as i64);
        let mut finished = Vec::new();
        let mut total_byte_size = 0i64;
        for column in columns.iter_mut() {
            column.finalize_write(
                &mut row_group.columns,
                &mut total_byte_size,
                sink,
                &options,
                0,
                &mut finished,
            )?;
        }
        row_group.total_byte_size = total_byte_size;
        row_group.total_compressed_size = Some((sink.written() - file_offset) as i64);
        let row_group_index = self.metadata.row_groups.len();
        if options.geoparquet_version.json() {
            for column in &finished {
                if let Some((name, ty, stats)) = &column.geo {
                    self.geo.add_stats(name, ty, stats, options.geoparquet_version)?;
                }
            }
        }
        for column in &mut finished {
            if let Some(filter) = column.bloom_filter.take() {
                self.bloom_filters.push(PendingBloomFilter {
                    row_group: row_group_index,
                    column: column.column_index,
                    filter,
                });
            }
        }
        self.finished.push(finished);
        self.metadata.num_rows += row_group.num_rows;
        self.metadata.row_groups.push(row_group);
        Ok(())
    }

    /// Writes the bloom filters and the footer (`ParquetWriter::Finalize`).
    pub fn finalize(&mut self) -> Result<()> {
        self.initialize_schema()?;
        let mut sink = self
            .sink
            .take()
            .ok_or_else(|| Error::internal("Parquet writer already finalized"))?;
        for pending in std::mem::take(&mut self.bloom_filters) {
            let bytes = pending.filter.to_bytes();
            let header = BloomFilterHeader {
                num_bytes: bytes.len() as i32,
                algorithm: BloomFilterAlgorithm {
                    block: Some(SplitBlockAlgorithm {}),
                },
                hash: BloomFilterHash {
                    xxhash: Some(XxHash {}),
                },
                compression: BloomFilterCompression {
                    uncompressed: Some(Uncompressed {}),
                },
            };
            let offset = sink.written();
            let header_size = sink.write_thrift(&header)?;
            sink.write_all(&bytes)?;
            if let Some(meta) = self
                .metadata
                .row_groups
                .get_mut(pending.row_group)
                .and_then(|group| group.columns.get_mut(pending.column))
                .and_then(|column| column.meta_data.as_mut())
            {
                meta.bloom_filter_offset = Some(offset as i64);
                meta.bloom_filter_length = Some((header_size + bytes.len() as u64) as i32);
            }
        }
        // The GeoParquet metadata follows the user's key/value metadata.
        if self.options.geoparquet_conversion
            && self.options.geoparquet_version.json()
            && let Some(json) = self.geo.to_json(self.options.geoparquet_version)?
        {
            self.metadata
                .key_value_metadata
                .get_or_insert_with(Vec::new)
                .push(crate::thrift::KeyValue {
                    key: crate::thrift::Binary(b"geo".to_vec()),
                    value: Some(crate::thrift::Binary(json.into_bytes())),
                });
        }
        let metadata_start = sink.written();
        sink.write_thrift(&self.metadata)?;
        let footer_size = sink.written() - metadata_start;
        sink.write_all(&(footer_size as u32).to_le_bytes())?;
        sink.write_all(b"PAR1")?;
        self.footer_size = Some(footer_size);
        self.final_size = sink.written();
        sink.close()
    }
}
