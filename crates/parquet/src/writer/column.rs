//! Column writers (DuckDB's `ColumnWriter`, `PrimitiveColumnWriter`, `ListColumnWriter`,
//! `ArrayColumnWriter` and `StructColumnWriter`): definition and repetition levels, page
//! splitting, page encoding and compression, and the column chunk metadata.
//!
//! A row group is written in DuckDB's phases: `analyze` (dictionary candidates),
//! `prepare` (levels and page boundaries), `begin_write`, `write` (encode pages) and
//! `finalize_write` (write pages to the file).

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_types::{ListEntry, LogicalType, LogicalTypeId, SelectionVector, Vector};

use super::encoders::RleBpEncoder;
use super::stats::WriterStatistics;
use super::values::ValueWriter;
use super::{FileSink, WriterOptions};
use crate::compression::compress;
use crate::decode::compute_bit_width;
use crate::thrift::{
    ColumnChunk, ColumnMetaData, ConvertedType, DataPageHeader, DictionaryPageHeader, Encoding,
    FieldRepetitionType, PageHeader, PageType, SchemaElement, Type,
};

/// A definition level of a value that is defined at every level (`PARQUET_DEFINE_VALID`).
pub const DEFINE_VALID: u16 = u16::MAX;

/// The largest uncompressed data page (`MAX_UNCOMPRESSED_PAGE_SIZE`).
const MAX_UNCOMPRESSED_PAGE_SIZE: usize = 104_857_600;

/// The schema of a column being written (the writer's `ParquetColumnSchema`).
#[derive(Clone, Debug)]
pub struct WriterSchema {
    pub name: String,
    pub logical_type: LogicalType,
    pub max_define: u16,
    pub max_repeat: u16,
    pub repetition: FieldRepetitionType,
    pub field_id: Option<i32>,
    /// `path_in_schema` of the column chunks below this node.
    pub path: Vec<String>,
}

/// The levels computed for a node while preparing a row group (`ColumnWriterState`).
#[derive(Debug, Default)]
pub struct Levels {
    pub definition: Vec<u16>,
    pub repetition: Vec<u16>,
    /// Entries that hold no value of this node (NULL or empty parents, empty lists).
    pub is_empty: Vec<u8>,
    pub parent_null_count: u64,
    pub null_count: u64,
}

/// A node of the writer tree.
#[derive(Debug)]
pub enum ColumnWriter {
    Primitive(Box<PrimitiveWriter>),
    /// LIST, MAP (over a REPEATED key/value struct) and ARRAY.
    List(Box<ListWriter>),
    Struct(StructWriter),
    /// VARIANT (`VariantColumnWriter`): a struct of `metadata`, `value` and an optional
    /// shredded `typed_value`, written from the transformed values.
    Variant(Box<VariantWriter>),
}

/// The writer of a VARIANT column (`VariantColumnWriter`).
#[derive(Debug)]
pub struct VariantWriter {
    /// The VARIANT group.
    pub schema: WriterSchema,
    /// The struct of `metadata`, `value` and `typed_value` (`TransformedType`).
    pub inner: ColumnWriter,
    /// The type the values are shredded on, if any.
    pub shredding: Option<LogicalType>,
    /// Whether the column was analyzed for shredding already (`is_analyzed`).
    pub analyzed: bool,
}

impl VariantWriter {
    /// The type of the transformed values: the inner struct's.
    pub fn transformed_type(&self) -> &LogicalType {
        &self.inner.schema().logical_type
    }

    /// The number of children of the inner struct.
    pub fn child_count(&self) -> usize {
        match &self.inner {
            ColumnWriter::Struct(node) => node.children.len(),
            _ => 0,
        }
    }
}

/// `ColumnWriter::DecrementMaxDefineRecursive`.
fn decrement_max_define(writer: &mut ColumnWriter) {
    match writer {
        ColumnWriter::Primitive(node) => node.schema.max_define -= 1,
        ColumnWriter::List(node) => {
            node.schema.max_define -= 1;
            decrement_max_define(&mut node.child);
        }
        ColumnWriter::Struct(node) => {
            node.schema.max_define -= 1;
            node.children.iter_mut().for_each(decrement_max_define);
        }
        ColumnWriter::Variant(node) => {
            node.schema.max_define -= 1;
            decrement_max_define(&mut node.inner);
        }
    }
}

/// `MarkRepetitionRequired`.
fn mark_required(writer: &mut ColumnWriter) {
    let schema = match writer {
        ColumnWriter::Primitive(node) => &mut node.schema,
        ColumnWriter::List(node) => &mut node.schema,
        ColumnWriter::Struct(node) => &mut node.schema,
        ColumnWriter::Variant(node) => &mut node.schema,
    };
    if schema.repetition == FieldRepetitionType::REQUIRED {
        return;
    }
    schema.repetition = FieldRepetitionType::REQUIRED;
    decrement_max_define(writer);
}

/// `MarkShreddedGroupRequired`: field and element groups below `typed_value` are
/// REQUIRED, recursively.
fn mark_shredded_group_required(group: &mut ColumnWriter) {
    mark_required(group);
    if let ColumnWriter::Struct(node) = group
        && let Some(typed_value) = node
            .children
            .iter_mut()
            .find(|child| child.schema().name == "typed_value")
    {
        mark_typed_value_groups_required(typed_value);
    }
}

/// `MarkTypedValueShreddingGroupsRequired`.
fn mark_typed_value_groups_required(typed_value: &mut ColumnWriter) {
    match typed_value {
        ColumnWriter::Struct(node) => node.children.iter_mut().for_each(mark_shredded_group_required),
        ColumnWriter::List(node) => mark_shredded_group_required(&mut node.child),
        _ => {}
    }
}

fn map_key_null_error() -> Error {
    Error::new(
        ErrorKind::Io,
        "Parquet writer: map key column is not allowed to contain NULL values",
    )
}

/// `ColumnWriter::HandleRepeatLevels`.
fn handle_repeat_levels(levels: &mut Levels, parent: Option<&Levels>) {
    let Some(parent) = parent else {
        return;
    };
    if levels.repetition.len() >= parent.repetition.len() {
        return;
    }
    let start = levels.repetition.len();
    levels.repetition.extend_from_slice(&parent.repetition[start..]);
}

/// `ColumnWriter::HandleDefineLevels`.
fn handle_define_levels(
    levels: &mut Levels,
    parent: Option<&Levels>,
    vector: &Vector,
    count: usize,
    define_value: u16,
    null_value: u16,
    can_have_nulls: bool,
) -> Result<()> {
    let validity = vector.validity();
    if let Some(parent) = parent {
        let mut vector_index = 0;
        while levels.definition.len() < parent.definition.len() {
            let current = levels.definition.len();
            if parent.definition[current] != DEFINE_VALID {
                levels.definition.push(parent.definition[current]);
                levels.parent_null_count += 1;
            } else if validity.is_valid(vector_index) {
                levels.definition.push(define_value);
            } else {
                if !can_have_nulls {
                    return Err(map_key_null_error());
                }
                levels.null_count += 1;
                levels.definition.push(null_value);
            }
            if parent.is_empty.is_empty() || parent.is_empty[current] == 0 {
                vector_index += 1;
            }
        }
        return Ok(());
    }
    if validity.is_all_valid() {
        levels.definition.extend(std::iter::repeat_n(define_value, count));
    } else {
        for row in 0..count {
            let is_null = !validity.is_valid(row);
            levels
                .definition
                .push(if is_null { null_value } else { define_value });
            levels.null_count += u64::from(is_null);
        }
    }
    if !can_have_nulls && levels.null_count != 0 {
        return Err(map_key_null_error());
    }
    Ok(())
}

impl ColumnWriter {
    pub fn schema(&self) -> &WriterSchema {
        match self {
            Self::Primitive(writer) => &writer.schema,
            Self::List(writer) => &writer.schema,
            Self::Struct(writer) => &writer.schema,
            Self::Variant(writer) => &writer.schema,
        }
    }

    /// `FinalizeSchema`: appends the schema elements; returns the number of leaves.
    pub fn finalize_schema(
        &mut self,
        schemas: &mut Vec<SchemaElement>,
        options: &WriterOptions,
    ) -> Result<usize> {
        match self {
            Self::Primitive(writer) => {
                let schema = &writer.schema;
                let mut element = SchemaElement {
                    type_: Some(writer.values.parquet_type()),
                    repetition_type: Some(schema.repetition),
                    name: schema.name.clone(),
                    field_id: schema.field_id,
                    ..SchemaElement::default()
                };
                set_schema_properties(&schema.logical_type, &mut element, options);
                schemas.push(element);
                Ok(1)
            }
            Self::List(writer) => {
                let schema = &writer.schema;
                let is_map = schema.logical_type.id() == LogicalTypeId::Map;
                schemas.push(SchemaElement {
                    repetition_type: Some(schema.repetition),
                    num_children: Some(1),
                    converted_type: Some(if is_map {
                        ConvertedType::MAP
                    } else {
                        ConvertedType::LIST
                    }),
                    name: schema.name.clone(),
                    field_id: schema.field_id,
                    ..SchemaElement::default()
                });
                if !is_map {
                    schemas.push(SchemaElement {
                        repetition_type: Some(FieldRepetitionType::REPEATED),
                        num_children: Some(1),
                        name: "list".to_owned(),
                        ..SchemaElement::default()
                    });
                }
                writer.child.finalize_schema(schemas, options)
            }
            Self::Struct(writer) => {
                let schema = &writer.schema;
                schemas.push(SchemaElement {
                    repetition_type: Some(schema.repetition),
                    num_children: Some(writer.children.len() as i32),
                    name: schema.name.clone(),
                    field_id: schema.field_id,
                    ..SchemaElement::default()
                });
                let mut leaves = 0;
                for child in &mut writer.children {
                    leaves += child.finalize_schema(schemas, options)?;
                }
                Ok(leaves)
            }
            Self::Variant(writer) => {
                // `VariantColumnWriter::FinalizeSchema`.
                let ColumnWriter::Struct(inner) = &mut writer.inner else {
                    return Err(Error::internal("VARIANT writer without its struct"));
                };
                if let Some(typed_value) = inner
                    .children
                    .iter_mut()
                    .find(|child| child.schema().name == "typed_value")
                {
                    mark_typed_value_groups_required(typed_value);
                }
                let schema = &writer.schema;
                schemas.push(SchemaElement {
                    repetition_type: Some(schema.repetition),
                    num_children: Some(inner.children.len() as i32),
                    name: schema.name.clone(),
                    field_id: schema.field_id,
                    logical_type: Some(crate::thrift::LogicalType {
                        variant: Some(crate::thrift::VariantType {
                            specification_version: Some(1),
                        }),
                        ..crate::thrift::LogicalType::default()
                    }),
                    ..SchemaElement::default()
                });
                let mut leaves = 0;
                for child in &mut inner.children {
                    leaves += child.finalize_schema(schemas, options)?;
                }
                Ok(leaves)
            }
        }
    }

    /// `InitializeWriteState`: starts a row group; registers the column chunks.
    pub fn initialize_write(
        &mut self,
        columns: &mut Vec<ColumnChunk>,
        num_rows: usize,
        options: &WriterOptions,
    ) {
        match self {
            Self::Primitive(writer) => writer.initialize_write(columns, num_rows, options),
            Self::List(writer) => {
                writer.levels = Levels::default();
                writer.parent_index = 0;
                writer.child.initialize_write(columns, num_rows, options);
            }
            Self::Struct(writer) => {
                writer.levels = Levels::default();
                for child in &mut writer.children {
                    child.initialize_write(columns, num_rows, options);
                }
            }
            Self::Variant(writer) => writer.inner.initialize_write(columns, num_rows, options),
        }
    }

    /// `HasAnalyze`.
    pub fn has_analyze(&self) -> bool {
        match self {
            Self::Primitive(writer) => writer.values.has_analyze(),
            Self::List(writer) => writer.child.has_analyze(),
            Self::Struct(writer) => writer.children.iter().any(ColumnWriter::has_analyze),
            Self::Variant(writer) => writer.inner.has_analyze(),
        }
    }

    /// `Analyze`.
    pub fn analyze(&mut self, parent: Option<&Levels>, vector: &Vector, count: usize) -> Result<()> {
        match self {
            Self::Primitive(writer) => {
                let levels_len = writer.levels.definition.len();
                writer.values.analyze(vector, parent, levels_len, count)
            }
            Self::List(writer) => {
                if writer.is_array {
                    let size = array_size(vector);
                    let child = vector
                        .array_child()
                        .ok_or_else(|| Error::internal("ARRAY vector without a child"))?;
                    writer.child.analyze(Some(&writer.levels), child, size * count)
                } else {
                    let (child, size) = vector
                        .list_child()
                        .ok_or_else(|| Error::internal("LIST vector without a child"))?;
                    writer.child.analyze(Some(&writer.levels), child, size)
                }
            }
            Self::Struct(writer) => {
                let children = vector
                    .struct_children()
                    .ok_or_else(|| Error::internal("STRUCT vector without children"))?;
                for (child_writer, child) in writer.children.iter_mut().zip(children) {
                    if child_writer.has_analyze() {
                        child_writer.analyze(Some(&writer.levels), child, count)?;
                    }
                }
                Ok(())
            }
            Self::Variant(writer) => writer.inner.analyze(parent, vector, count),
        }
    }

    /// `FinalizeAnalyze`.
    pub fn finalize_analyze(&mut self, options: &WriterOptions) {
        match self {
            Self::Primitive(writer) => writer.values.finalize_analyze(options),
            Self::List(writer) => writer.child.finalize_analyze(options),
            Self::Struct(writer) => {
                for child in &mut writer.children {
                    if child.has_analyze() {
                        child.finalize_analyze(options);
                    }
                }
            }
            Self::Variant(writer) => writer.inner.finalize_analyze(options),
        }
    }

    /// `Prepare`: computes the levels and page boundaries of a chunk.
    pub fn prepare(
        &mut self,
        parent: Option<&Levels>,
        vector: &Vector,
        count: usize,
        can_span_pages: bool,
    ) -> Result<()> {
        match self {
            Self::Primitive(writer) => writer.prepare(parent, vector, count, can_span_pages),
            Self::List(writer) => writer.prepare(parent, vector, count),
            Self::Struct(writer) => {
                let levels = &mut writer.levels;
                if let Some(parent) = parent
                    && levels.is_empty.len() < parent.is_empty.len()
                {
                    let start = levels.is_empty.len();
                    levels.is_empty.extend_from_slice(&parent.is_empty[start..]);
                }
                handle_repeat_levels(levels, parent);
                handle_define_levels(
                    levels,
                    parent,
                    vector,
                    count,
                    DEFINE_VALID,
                    writer.schema.max_define - 1,
                    writer.schema.repetition == FieldRepetitionType::OPTIONAL,
                )?;
                let children = vector
                    .struct_children()
                    .ok_or_else(|| Error::internal("STRUCT vector without children"))?;
                for (child_writer, child) in writer.children.iter_mut().zip(children) {
                    child_writer.prepare(Some(&writer.levels), child, count, can_span_pages)?;
                }
                Ok(())
            }
            Self::Variant(writer) => writer.inner.prepare(parent, vector, count, can_span_pages),
        }
    }

    /// `BeginWrite`.
    pub fn begin_write(&mut self, options: &WriterOptions) -> Result<()> {
        match self {
            Self::Primitive(writer) => writer.begin_write(options),
            Self::List(writer) => writer.child.begin_write(options),
            Self::Struct(writer) => {
                for child in &mut writer.children {
                    child.begin_write(options)?;
                }
                Ok(())
            }
            Self::Variant(writer) => writer.inner.begin_write(options),
        }
    }

    /// `Write`: encodes the values of a chunk into pages.
    pub fn write(&mut self, vector: &Vector, count: usize, options: &WriterOptions) -> Result<()> {
        match self {
            Self::Primitive(writer) => writer.write(vector, count, options),
            Self::List(writer) => {
                if writer.is_array {
                    let size = array_size(vector);
                    let child = vector
                        .array_child()
                        .ok_or_else(|| Error::internal("ARRAY vector without a child"))?;
                    writer.child.write(child, count * size, options)
                } else {
                    let (child, length) = consecutive_child_list(vector, count)?;
                    writer.child.write(&child, length, options)
                }
            }
            Self::Struct(writer) => {
                let children = vector
                    .struct_children()
                    .ok_or_else(|| Error::internal("STRUCT vector without children"))?;
                for (child_writer, child) in writer.children.iter_mut().zip(children) {
                    child_writer.write(child, count, options)?;
                }
                Ok(())
            }
            Self::Variant(writer) => writer.inner.write(vector, count, options),
        }
    }

    /// `FinalizeWrite`: writes the pages and completes the column chunks.
    pub fn finalize_write(
        &mut self,
        columns: &mut [ColumnChunk],
        total_byte_size: &mut i64,
        sink: &mut FileSink,
        options: &WriterOptions,
        extra_null_count: u64,
        output: &mut Vec<FinishedColumn>,
    ) -> Result<()> {
        match self {
            Self::Primitive(writer) => {
                writer.levels.null_count += extra_null_count;
                writer.finalize_write(columns, total_byte_size, sink, options, output)
            }
            Self::List(writer) => {
                writer
                    .child
                    .finalize_write(columns, total_byte_size, sink, options, 0, output)
            }
            Self::Struct(writer) => {
                let null_count = writer.levels.null_count + extra_null_count;
                for child in &mut writer.children {
                    child.finalize_write(columns, total_byte_size, sink, options, null_count, output)?;
                }
                Ok(())
            }
            Self::Variant(writer) => {
                writer
                    .inner
                    .finalize_write(columns, total_byte_size, sink, options, extra_null_count, output)
            }
        }
    }
}

fn array_size(vector: &Vector) -> usize {
    vector
        .logical_type()
        .array_child()
        .map_or(0, |(_, size)| size as usize)
}

/// `GetConsecutiveChildList`: the child rows of the valid lists, in order.
fn consecutive_child_list(vector: &Vector, count: usize) -> Result<(Vector, usize)> {
    let (child, _) = vector
        .list_child()
        .ok_or_else(|| Error::internal("LIST vector without a child"))?;
    let entries = vector.values::<ListEntry>();
    let validity = vector.validity();
    let mut consecutive = true;
    let mut total = 0u64;
    for (row, entry) in entries.iter().enumerate().take(count) {
        if !validity.is_valid(row) {
            continue;
        }
        if entry.offset != total {
            consecutive = false;
        }
        total += entry.length;
    }
    if consecutive {
        return Ok((child.clone(), total as usize));
    }
    let mut sel = Vec::with_capacity(total as usize);
    for (row, entry) in entries.iter().enumerate().take(count) {
        if !validity.is_valid(row) {
            continue;
        }
        sel.extend(entry.range().map(|index| index as u32));
    }
    let sliced = child.slice(&SelectionVector::new(sel));
    let mut flat = Vector::flat(child.logical_type().clone(), total as usize);
    rustdb_types::vector_ops::copy_rows(&sliced, 0, total as usize, &mut flat, 0)?;
    Ok((flat, total as usize))
}

/// The writer of a LIST/MAP/ARRAY column (`ListColumnWriter` / `ArrayColumnWriter`).
#[derive(Debug)]
pub struct ListWriter {
    pub schema: WriterSchema,
    pub child: ColumnWriter,
    pub is_array: bool,
    levels: Levels,
    parent_index: usize,
}

impl ListWriter {
    pub fn new(schema: WriterSchema, child: ColumnWriter, is_array: bool) -> Self {
        Self {
            schema,
            child,
            is_array,
            levels: Levels::default(),
            parent_index: 0,
        }
    }

    fn can_have_nulls(&self) -> bool {
        self.schema.repetition == FieldRepetitionType::OPTIONAL
    }

    /// `ArrayColumnWriter::WriteArrayState`.
    fn push_array(&mut self, size: usize, first_repeat: u16, define: u16, is_empty: bool) {
        let levels = &mut self.levels;
        levels.definition.push(define);
        levels.repetition.push(first_repeat);
        levels.is_empty.push(u8::from(is_empty));
        if is_empty {
            return;
        }
        for _ in 1..size {
            levels.repetition.push(self.schema.max_repeat + 1);
            levels.definition.push(define);
            levels.is_empty.push(0);
        }
    }

    fn prepare(&mut self, parent: Option<&Levels>, vector: &Vector, count: usize) -> Result<()> {
        let vcount = parent.map_or(count, |parent| parent.definition.len() - self.parent_index);
        let validity = vector.validity().clone();
        let max_repeat = self.schema.max_repeat;
        let max_define = self.schema.max_define;
        let mut vector_index = 0;
        let size = if self.is_array { array_size(vector) } else { 0 };
        let entries: Vec<ListEntry> = if self.is_array {
            Vec::new()
        } else {
            vector.values::<ListEntry>().to_vec()
        };
        for i in 0..vcount {
            let parent_index = self.parent_index + i;
            if let Some(parent) = parent
                && !parent.is_empty.is_empty()
                && parent.is_empty[parent_index] != 0
            {
                if self.is_array {
                    self.push_array(
                        size,
                        parent.repetition[parent_index],
                        parent.definition[parent_index],
                        true,
                    );
                } else {
                    self.levels.definition.push(parent.definition[parent_index]);
                    self.levels.repetition.push(parent.repetition[parent_index]);
                    self.levels.is_empty.push(1);
                }
                continue;
            }
            let first_repeat = match parent {
                Some(parent) if !parent.repetition.is_empty() => parent.repetition[parent_index],
                _ => max_repeat,
            };
            if let Some(parent) = parent
                && parent.definition[parent_index] != DEFINE_VALID
            {
                if self.is_array {
                    self.push_array(size, first_repeat, parent.definition[parent_index], false);
                } else {
                    self.levels.definition.push(parent.definition[parent_index]);
                    self.levels.repetition.push(first_repeat);
                    self.levels.is_empty.push(1);
                }
            } else if validity.is_valid(vector_index) {
                if self.is_array {
                    self.push_array(size, first_repeat, DEFINE_VALID, false);
                } else {
                    let length = entries[vector_index].length;
                    if length == 0 {
                        self.levels.definition.push(max_define);
                        self.levels.is_empty.push(1);
                    } else {
                        self.levels.definition.push(DEFINE_VALID);
                        self.levels.is_empty.push(0);
                    }
                    self.levels.repetition.push(first_repeat);
                    for _ in 1..length {
                        self.levels.repetition.push(max_repeat + 1);
                        self.levels.definition.push(DEFINE_VALID);
                        self.levels.is_empty.push(0);
                    }
                }
            } else if self.is_array {
                self.push_array(size, first_repeat, max_define - 1, false);
            } else {
                if !self.can_have_nulls() {
                    return Err(map_key_null_error());
                }
                self.levels.definition.push(max_define - 1);
                self.levels.repetition.push(first_repeat);
                self.levels.is_empty.push(1);
            }
            vector_index += 1;
        }
        self.parent_index += vcount;
        if self.is_array {
            let child = vector
                .array_child()
                .ok_or_else(|| Error::internal("ARRAY vector without a child"))?;
            self.child.prepare(Some(&self.levels), child, count * size, false)
        } else {
            let (child, length) = consecutive_child_list(vector, count)?;
            self.child.prepare(Some(&self.levels), &child, length, false)
        }
    }
}

/// The writer of a STRUCT column (`StructColumnWriter`).
#[derive(Debug)]
pub struct StructWriter {
    pub schema: WriterSchema,
    pub children: Vec<ColumnWriter>,
    levels: Levels,
}

impl StructWriter {
    pub fn new(schema: WriterSchema, children: Vec<ColumnWriter>) -> Self {
        Self {
            schema,
            children,
            levels: Levels::default(),
        }
    }
}

/// `PageInformation`.
#[derive(Clone, Debug, Default)]
struct PageInformation {
    offset: usize,
    row_count: usize,
    empty_count: usize,
    estimated_page_size: usize,
    null_count: usize,
}

/// `PageWriteInformation`.
#[derive(Debug)]
struct PageWrite {
    header: PageHeader,
    /// The uncompressed page while it is written.
    buffer: Vec<u8>,
    /// The compressed page.
    compressed: Vec<u8>,
    write_count: usize,
    max_write_count: usize,
}

/// A written column chunk with what `RETURN_STATS` reports about it.
#[derive(Debug, Clone)]
pub struct FinishedColumn {
    pub column_index: usize,
    pub has_nan: bool,
    pub can_have_nan: bool,
    /// The min/max of the values when NaNs kept them out of the file.
    pub nan_min_max: Option<(Vec<u8>, Vec<u8>)>,
    /// The geometry statistics of a GEOMETRY column: its name, type and statistics.
    pub geo: Option<(String, LogicalType, crate::geo::GeoStats)>,
    pub bloom_filter: Option<super::bloom::BloomFilter>,
}

/// The writer of a leaf column (`PrimitiveColumnWriter`).
#[derive(Debug)]
pub struct PrimitiveWriter {
    pub schema: WriterSchema,
    pub values: Box<dyn ValueWriter>,
    levels: Levels,
    column_index: usize,
    num_values: i64,
    page_info: Vec<PageInformation>,
    pages: Vec<PageWrite>,
    current_page: usize,
    /// Page buffers of earlier row groups, reused so each row group does not allocate
    /// (and fragment) its large buffers anew.
    spare_buffers: Vec<Vec<u8>>,
}

impl PrimitiveWriter {
    /// An empty buffer with room for `capacity` bytes, reusing a spare one.
    fn take_buffer(&mut self, capacity: usize) -> Vec<u8> {
        let mut buffer = self.spare_buffers.pop().unwrap_or_default();
        buffer.clear();
        buffer.reserve(capacity);
        buffer
    }

    /// Keeps `buffer` for later pages.
    fn recycle_buffer(&mut self, buffer: Vec<u8>) {
        if buffer.capacity() > 0 {
            self.spare_buffers.push(buffer);
        }
    }

    pub fn new(schema: WriterSchema, values: Box<dyn ValueWriter>) -> Self {
        Self {
            schema,
            values,
            levels: Levels::default(),
            column_index: 0,
            num_values: 0,
            page_info: Vec::new(),
            pages: Vec::new(),
            current_page: 0,
            spare_buffers: Vec::new(),
        }
    }

    fn can_have_nulls(&self) -> bool {
        self.schema.repetition == FieldRepetitionType::OPTIONAL
    }

    /// `InitializeWriteState` + `RegisterToRowGroup`.
    fn initialize_write(&mut self, columns: &mut Vec<ColumnChunk>, num_rows: usize, options: &WriterOptions) {
        self.levels = Levels::default();
        self.column_index = columns.len();
        self.num_values = 0;
        self.page_info = vec![PageInformation::default()];
        self.pages.clear();
        self.current_page = 0;
        self.values.begin_row_group(num_rows, options);
        columns.push(ColumnChunk {
            meta_data: Some(ColumnMetaData {
                codec: options.codec,
                path_in_schema: self.schema.path.clone(),
                type_: self.values.parquet_type(),
                ..ColumnMetaData::default()
            }),
            ..ColumnChunk::default()
        });
    }

    /// `PrimitiveColumnWriter::Prepare`.
    fn prepare(
        &mut self,
        parent: Option<&Levels>,
        vector: &Vector,
        count: usize,
        can_span_pages: bool,
    ) -> Result<()> {
        let vcount = parent.map_or(count, |parent| {
            parent.definition.len() - self.levels.definition.len()
        });
        let parent_index = self.levels.definition.len();
        let can_have_nulls = self.can_have_nulls();
        let max_define = self.schema.max_define;
        handle_repeat_levels(&mut self.levels, parent);
        handle_define_levels(
            &mut self.levels,
            parent,
            vector,
            count,
            max_define,
            max_define.wrapping_sub(1),
            can_have_nulls,
        )?;
        self.num_values += vcount as i64;
        let check_parent_empty = parent.is_some_and(|parent| !parent.is_empty.is_empty());
        let validity = vector.validity();
        let mut page = self.page_info.len() - 1;
        if !check_parent_empty
            && validity.is_all_valid()
            && self.values.is_constant_size()
            && self.page_info[page].estimated_page_size + self.values.row_size(vector, 0) * vcount
                < MAX_UNCOMPRESSED_PAGE_SIZE
        {
            let row_size = self.values.row_size(vector, 0);
            let info = &mut self.page_info[page];
            info.row_count += vcount;
            info.estimated_page_size += row_size * vcount;
            return Ok(());
        }
        let mut vector_index = 0;
        for i in 0..vcount {
            self.page_info[page].row_count += 1;
            if check_parent_empty && parent.is_some_and(|parent| parent.is_empty[parent_index + i] != 0) {
                self.page_info[page].empty_count += 1;
                continue;
            }
            if validity.is_valid(vector_index) {
                let row_size = self.values.row_size(vector, vector_index);
                let info = &mut self.page_info[page];
                info.estimated_page_size += row_size;
                if info.estimated_page_size >= MAX_UNCOMPRESSED_PAGE_SIZE && (can_span_pages || i == 0) {
                    let offset = info.offset + info.row_count;
                    self.page_info.push(PageInformation {
                        offset,
                        ..PageInformation::default()
                    });
                    page += 1;
                }
            } else {
                self.page_info[page].null_count += 1;
            }
            vector_index += 1;
        }
        Ok(())
    }

    /// `PrimitiveColumnWriter::BeginWrite`.
    fn begin_write(&mut self, options: &WriterOptions) -> Result<()> {
        self.values.init_stats();
        if let Some(last) = self.page_info.last()
            && last.row_count == 0
        {
            self.page_info.pop();
        }
        let encoding = self.values.encoding();
        for index in 0..self.page_info.len() {
            let info = &self.page_info[index];
            let (row_count, empty_count, capacity) = (
                info.row_count,
                info.empty_count,
                info.estimated_page_size.next_power_of_two().max(512),
            );
            let buffer = self.take_buffer(capacity);
            self.pages.push(PageWrite {
                header: PageHeader {
                    type_: PageType::DATA_PAGE,
                    data_page_header: Some(DataPageHeader {
                        num_values: row_count as i32,
                        encoding,
                        definition_level_encoding: Encoding::RLE,
                        repetition_level_encoding: Encoding::RLE,
                        statistics: None,
                    }),
                    ..PageHeader::default()
                },
                buffer,
                compressed: Vec::new(),
                write_count: empty_count,
                max_write_count: row_count,
            });
        }
        self.next_page(options)
    }

    /// `WriteLevels`: RLE/bit-packed levels with their byte length in front.
    fn write_levels(
        out: &mut Vec<u8>,
        levels: &[u16],
        max_value: u16,
        offset: usize,
        count: usize,
        no_nulls: bool,
    ) {
        if levels.is_empty() || count == 0 {
            return;
        }
        let mut encoder = RleBpEncoder::new(compute_bit_width(u64::from(max_value)));
        let mut intermediate = Vec::new();
        encoder.begin_write();
        if no_nulls {
            encoder.write_many(&mut intermediate, u32::from(levels[0]), count as u64);
        } else {
            for &level in &levels[offset..offset + count] {
                encoder.write_value(&mut intermediate, u32::from(level));
            }
        }
        encoder.finish_write(&mut intermediate);
        out.extend_from_slice(&(intermediate.len() as u32).to_le_bytes());
        out.extend_from_slice(&intermediate);
    }

    /// `NextPage`: finishes the current page and starts the next one with its levels.
    fn next_page(&mut self, options: &WriterOptions) -> Result<()> {
        if self.current_page > 0 {
            self.flush_page(options)?;
        }
        if self.current_page >= self.pages.len() {
            self.current_page = self.pages.len() + 1;
            return Ok(());
        }
        let info = self.page_info[self.current_page].clone();
        let non_null = info.row_count - (info.empty_count + info.null_count);
        let page = &mut self.pages[self.current_page];
        self.current_page += 1;
        Self::write_levels(
            &mut page.buffer,
            &self.levels.repetition,
            self.schema.max_repeat,
            info.offset,
            info.row_count,
            false,
        );
        let no_nulls = self.levels.null_count + self.levels.parent_null_count == 0;
        Self::write_levels(
            &mut page.buffer,
            &self.levels.definition,
            self.schema.max_define,
            info.offset,
            info.row_count,
            no_nulls,
        );
        self.values.begin_page(non_null);
        Ok(())
    }

    /// `FlushPage`: completes and compresses the current page.
    fn flush_page(&mut self, options: &WriterOptions) -> Result<()> {
        if self.current_page > self.pages.len() || self.current_page == 0 {
            return Ok(());
        }
        let mut compressed = self.take_buffer(0);
        let page = &mut self.pages[self.current_page - 1];
        self.values.flush_page(&mut page.buffer)?;
        let uncompressed = page.buffer.len();
        if uncompressed > i32::MAX as usize {
            return Err(Error::internal(format!(
                "Parquet writer: {uncompressed} uncompressed page size out of range for type integer"
            )));
        }
        page.header.uncompressed_page_size = uncompressed as i32;
        compress(
            options.codec,
            &page.buffer,
            options.compression_level,
            &mut compressed,
        )?;
        page.header.compressed_page_size = compressed.len() as i32;
        page.compressed = compressed;
        let buffer = std::mem::take(&mut page.buffer);
        self.recycle_buffer(buffer);
        Ok(())
    }

    /// `PrimitiveColumnWriter::Write`.
    fn write(&mut self, vector: &Vector, count: usize, options: &WriterOptions) -> Result<()> {
        let mut remaining = count;
        let mut offset = 0;
        while remaining > 0 {
            let index = self
                .current_page
                .checked_sub(1)
                .filter(|&index| index < self.pages.len())
                .ok_or_else(|| Error::internal("Writes are not correctly aligned!?"))?;
            let page = &mut self.pages[index];
            let write_count = remaining.min(page.max_write_count - page.write_count);
            if write_count == 0 {
                return Err(Error::internal("Writes are not correctly aligned!?"));
            }
            self.values
                .write_vector(&mut page.buffer, vector, offset, offset + write_count)?;
            page.write_count += write_count;
            if page.write_count == page.max_write_count {
                self.next_page(options)?;
            }
            offset += write_count;
            remaining -= write_count;
        }
        Ok(())
    }

    /// `SetParquetStatistics`.
    fn statistics(&self, meta: &mut ColumnMetaData) {
        let Some(stats) = self.values.stats() else {
            return;
        };
        let null_count = if self.schema.max_repeat == 0 {
            self.levels.null_count
        } else {
            self.levels.null_count + self.levels.parent_null_count
        };
        let mut statistics = meta.statistics.take().unwrap_or_default();
        statistics.null_count = Some(null_count as i64);
        if !stats.has_nan() {
            let min = stats.min();
            if !min.is_empty() {
                statistics.min = Some(crate::thrift::Binary(min));
            }
            let max = stats.max();
            if !max.is_empty() {
                statistics.max = Some(crate::thrift::Binary(max));
            }
            if stats.has_stats() {
                statistics.min_value = Some(crate::thrift::Binary(stats.min_value()));
                statistics.is_min_value_exact = Some(stats.min_is_exact());
                statistics.max_value = Some(crate::thrift::Binary(stats.max_value()));
                statistics.is_max_value_exact = Some(stats.max_is_exact());
            }
        }
        if self.values.has_dictionary() {
            statistics.distinct_count = Some(self.values.dictionary_size() as i64);
        }
        meta.statistics = Some(statistics);
        for page in &self.pages {
            if page.header.type_ != PageType::DATA_PAGE && page.header.type_ != PageType::DATA_PAGE_V2 {
                continue;
            }
            if let Some(header) = &page.header.data_page_header {
                meta.encodings.push(header.encoding);
            }
        }
    }

    /// `PrimitiveColumnWriter::FinalizeWrite`.
    fn finalize_write(
        &mut self,
        columns: &mut [ColumnChunk],
        total_byte_size: &mut i64,
        sink: &mut FileSink,
        options: &WriterOptions,
        output: &mut Vec<FinishedColumn>,
    ) -> Result<()> {
        self.flush_page(options)?;
        let start_offset = sink.written();
        let column = columns
            .get_mut(self.column_index)
            .ok_or_else(|| Error::internal("column chunk not registered"))?;
        let mut meta = column.meta_data.take().unwrap_or_default();
        meta.num_values = self.num_values;
        let mut bloom_filter = None;
        if self.values.has_dictionary() {
            // The distinct count is part of the statistics written below.
            meta.dictionary_page_offset = Some(start_offset as i64);
            let dictionary = self.values.flush_dictionary(options)?;
            bloom_filter = dictionary.bloom_filter;
            let mut compressed = Vec::new();
            compress(
                options.codec,
                &dictionary.page,
                options.compression_level,
                &mut compressed,
            )?;
            self.pages.insert(
                0,
                PageWrite {
                    header: PageHeader {
                        type_: PageType::DICTIONARY_PAGE,
                        uncompressed_page_size: dictionary.page.len() as i32,
                        compressed_page_size: compressed.len() as i32,
                        dictionary_page_header: Some(DictionaryPageHeader {
                            num_values: dictionary.count as i32,
                            encoding: Encoding::PLAIN,
                            // DuckDB sets `is_sorted = false` without marking it set.
                            is_sorted: None,
                        }),
                        ..PageHeader::default()
                    },
                    buffer: Vec::new(),
                    compressed,
                    write_count: 0,
                    max_write_count: 0,
                },
            );
        }
        meta.data_page_offset = 0;
        self.statistics(&mut meta);
        let mut total_uncompressed = 0u64;
        for page in &self.pages {
            if meta.data_page_offset == 0
                && (page.header.type_ == PageType::DATA_PAGE || page.header.type_ == PageType::DATA_PAGE_V2)
            {
                meta.data_page_offset = sink.written() as i64;
            }
            let header_start = sink.written();
            sink.write_thrift(&page.header)?;
            total_uncompressed += sink.written() - header_start;
            total_uncompressed += page.header.uncompressed_page_size as u64;
            sink.write_all(&page.compressed)?;
        }
        for page in std::mem::take(&mut self.pages) {
            self.recycle_buffer(page.compressed);
        }
        meta.total_compressed_size = (sink.written() - start_offset) as i64;
        meta.total_uncompressed_size = total_uncompressed as i64;
        *total_byte_size += meta.total_uncompressed_size;
        let stats = self.values.stats();
        let has_nan = stats.is_some_and(WriterStatistics::has_nan);
        let geo = stats.and_then(WriterStatistics::geo_stats).copied();
        if let Some(geo) = &geo
            && options.geoparquet_version.native()
        {
            meta.geospatial_statistics = Some(geo.to_thrift());
        }
        output.push(FinishedColumn {
            geo: geo.map(|geo| (self.schema.name.clone(), self.schema.logical_type.clone(), geo)),
            column_index: self.column_index,
            has_nan,
            can_have_nan: stats.is_some_and(WriterStatistics::can_have_nan),
            nan_min_max: stats
                .filter(|stats| has_nan && stats.has_stats())
                .map(|stats| (stats.min(), stats.max())),
            bloom_filter,
        });
        self.pages.clear();
        column.meta_data = Some(meta);
        Ok(())
    }
}

/// `ParquetWriter::SetSchemaProperties`: the converted/logical type annotations.
pub fn set_schema_properties(ty: &LogicalType, element: &mut SchemaElement, options: &WriterOptions) {
    use crate::thrift::{
        DecimalType, JsonType, LogicalType as ParquetLogicalType, MicroSeconds, MilliSeconds, NanoSeconds,
        TimeType, TimeUnit, TimestampType, UuidType,
    };
    if ty.id() == LogicalTypeId::Geometry {
        // Native geometry columns, except for GeoParquet 1 files.
        if options.geoparquet_version.native() {
            element.logical_type = Some(ParquetLogicalType {
                geometry: Some(crate::thrift::GeometryType {
                    crs: crate::geo::type_crs(ty).map(str::to_owned),
                }),
                ..ParquetLogicalType::default()
            });
        }
        return;
    }
    if ty.alias() == Some("JSON") && ty.id() == LogicalTypeId::Varchar {
        element.converted_type = Some(ConvertedType::JSON);
        element.logical_type = Some(ParquetLogicalType {
            json: Some(JsonType {}),
            ..ParquetLogicalType::default()
        });
        return;
    }
    let micros = || TimeUnit {
        micros: Some(MicroSeconds {}),
        ..TimeUnit::default()
    };
    match ty.id() {
        LogicalTypeId::TinyInt => element.converted_type = Some(ConvertedType::INT_8),
        LogicalTypeId::SmallInt => element.converted_type = Some(ConvertedType::INT_16),
        LogicalTypeId::Integer => element.converted_type = Some(ConvertedType::INT_32),
        LogicalTypeId::BigInt => element.converted_type = Some(ConvertedType::INT_64),
        LogicalTypeId::UTinyInt => element.converted_type = Some(ConvertedType::UINT_8),
        LogicalTypeId::USmallInt => element.converted_type = Some(ConvertedType::UINT_16),
        LogicalTypeId::UInteger => element.converted_type = Some(ConvertedType::UINT_32),
        LogicalTypeId::UBigInt => element.converted_type = Some(ConvertedType::UINT_64),
        LogicalTypeId::Date => element.converted_type = Some(ConvertedType::DATE),
        LogicalTypeId::TimeTz | LogicalTypeId::Time => {
            element.converted_type = Some(ConvertedType::TIME_MICROS);
            element.logical_type = Some(ParquetLogicalType {
                time: Some(TimeType {
                    is_adjusted_to_utc: ty.id() == LogicalTypeId::TimeTz,
                    unit: micros(),
                }),
                ..ParquetLogicalType::default()
            });
        }
        LogicalTypeId::TimeNs => {
            element.logical_type = Some(ParquetLogicalType {
                time: Some(TimeType {
                    is_adjusted_to_utc: false,
                    unit: TimeUnit {
                        nanos: Some(NanoSeconds {}),
                        ..TimeUnit::default()
                    },
                }),
                ..ParquetLogicalType::default()
            });
        }
        LogicalTypeId::TimestampTz | LogicalTypeId::Timestamp | LogicalTypeId::TimestampSec => {
            element.converted_type = Some(ConvertedType::TIMESTAMP_MICROS);
            element.logical_type = Some(ParquetLogicalType {
                timestamp: Some(TimestampType {
                    is_adjusted_to_utc: ty.id() == LogicalTypeId::TimestampTz,
                    unit: micros(),
                }),
                ..ParquetLogicalType::default()
            });
        }
        LogicalTypeId::TimestampNs => {
            element.logical_type = Some(ParquetLogicalType {
                timestamp: Some(TimestampType {
                    is_adjusted_to_utc: false,
                    unit: TimeUnit {
                        nanos: Some(NanoSeconds {}),
                        ..TimeUnit::default()
                    },
                }),
                ..ParquetLogicalType::default()
            });
        }
        LogicalTypeId::TimestampMs => {
            element.converted_type = Some(ConvertedType::TIMESTAMP_MILLIS);
            element.logical_type = Some(ParquetLogicalType {
                timestamp: Some(TimestampType {
                    is_adjusted_to_utc: false,
                    unit: TimeUnit {
                        millis: Some(MilliSeconds {}),
                        ..TimeUnit::default()
                    },
                }),
                ..ParquetLogicalType::default()
            });
        }
        LogicalTypeId::Enum | LogicalTypeId::Varchar => element.converted_type = Some(ConvertedType::UTF8),
        LogicalTypeId::Interval => {
            element.type_length = Some(12);
            element.converted_type = Some(ConvertedType::INTERVAL);
        }
        LogicalTypeId::Uuid => {
            element.type_length = Some(16);
            element.logical_type = Some(ParquetLogicalType {
                uuid: Some(UuidType {}),
                ..ParquetLogicalType::default()
            });
        }
        LogicalTypeId::Decimal => {
            let (width, scale) = ty.decimal_properties().unwrap_or((18, 3));
            element.converted_type = Some(ConvertedType::DECIMAL);
            element.precision = Some(i32::from(width));
            element.scale = Some(i32::from(scale));
            if ty.physical_type() == rustdb_types::PhysicalType::Int128 {
                element.type_length = Some(16);
            }
            element.logical_type = Some(ParquetLogicalType {
                decimal: Some(DecimalType {
                    scale: i32::from(scale),
                    precision: i32::from(width),
                }),
                ..ParquetLogicalType::default()
            });
        }
        _ => {}
    }
}

/// `ParquetWriter::TryGetParquetType`.
pub fn try_parquet_type(ty: &LogicalType) -> Option<Type> {
    use LogicalTypeId as L;
    Some(match ty.id() {
        L::Boolean => Type::BOOLEAN,
        L::TinyInt | L::SmallInt | L::Integer | L::Date | L::UTinyInt | L::USmallInt | L::UInteger => {
            Type::INT32
        }
        L::Float => Type::FLOAT,
        L::Double | L::UHugeInt | L::HugeInt => Type::DOUBLE,
        L::Enum | L::Blob | L::Varchar | L::Geometry => Type::BYTE_ARRAY,
        L::BigInt
        | L::UBigInt
        | L::Time
        | L::TimeNs
        | L::TimeTz
        | L::Timestamp
        | L::TimestampTz
        | L::TimestampMs
        | L::TimestampNs
        | L::TimestampSec => Type::INT64,
        L::Interval | L::Uuid => Type::FIXED_LEN_BYTE_ARRAY,
        L::Decimal => match ty.physical_type() {
            rustdb_types::PhysicalType::Int16 | rustdb_types::PhysicalType::Int32 => Type::INT32,
            rustdb_types::PhysicalType::Int64 => Type::INT64,
            _ => Type::FIXED_LEN_BYTE_ARRAY,
        },
        _ => return None,
    })
}
