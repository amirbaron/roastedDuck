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
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, PhysicalType};

use self::column::{ColumnWriter, FinishedColumn, ListWriter, PrimitiveWriter, StructWriter, WriterSchema};
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
        LogicalTypeId::Variant => Err(Error::not_implemented("Writing VARIANT columns to Parquet")),
        _ => {
            let values = primitive_values(ty)?;
            Ok(ColumnWriter::Primitive(Box::new(PrimitiveWriter::new(
                schema, values,
            ))))
        }
    }
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
        L::Blob | L::Geometry => standard::<Bytes<true>>(true),
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
            types: types.to_vec(),
            columns,
            metadata,
            bloom_filters: Vec::new(),
            finished: Vec::new(),
            footer_size: None,
            final_size: 0,
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

    /// Writes `chunks` as one row group (`Flush` = `PrepareRowGroup` + `FlushRowGroup`).
    pub fn flush(&mut self, chunks: &[DataChunk]) -> Result<()> {
        let num_rows: usize = chunks.iter().map(DataChunk::len).sum();
        if num_rows == 0 {
            return Ok(());
        }
        // The writers index vector slots directly, so nested values are made flat.
        let mut flat_chunks = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            let columns = chunk
                .columns()
                .iter()
                .map(|column| deep_flatten(column, chunk.len()))
                .collect::<Result<Vec<_>>>()?;
            flat_chunks.push((columns, chunk.len()));
        }
        let mut row_group = RowGroup {
            num_rows: num_rows as i64,
            file_offset: Some(0),
            ..RowGroup::default()
        };
        self.initialize_schema()?;
        let options = self.options.clone();
        for (index, column) in self.columns.iter_mut().enumerate() {
            column.initialize_write(&mut row_group.columns, num_rows, &options);
            if column.has_analyze() {
                for (vectors, count) in &flat_chunks {
                    column.analyze(None, &vectors[index], *count)?;
                }
                column.finalize_analyze(&options);
            }
            for (vectors, count) in &flat_chunks {
                column.prepare(None, &vectors[index], *count, true)?;
            }
            column.begin_write(&options)?;
            for (vectors, count) in &flat_chunks {
                column.write(&vectors[index], *count, &options)?;
            }
        }
        // `FlushRowGroup`.
        let sink = self
            .sink
            .as_mut()
            .ok_or_else(|| Error::internal("Parquet writer already finalized"))?;
        let file_offset = sink.written();
        row_group.file_offset = Some(file_offset as i64);
        let mut finished = Vec::new();
        let mut total_byte_size = 0i64;
        for column in &mut self.columns {
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
