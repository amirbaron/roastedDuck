//! The reader of one Parquet file (DuckDB's `ParquetReader`): its metadata and schema,
//! and scans of row groups into DuckDB vectors.

use std::sync::Arc;

use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_storage::TableFilter;
use roastedduck_types::{LogicalType, Sel, SelectionVector, Value, Vector, VectorKind};

use crate::column_reader::{ChildProjection, ColumnReader, ReaderContext, RowNumberReader, StructReader};
use crate::file::{ParquetFile, load_metadata};
use crate::schema::{ColumnSchema, ParquetOptions, SchemaKind, SchemaParser};
use crate::thrift::FileMetaData;

/// A column of a file as the multi-file reader sees it (`MultiFileColumnDefinition`).
#[derive(Clone, Debug)]
pub struct ColumnDefinition {
    pub name: roastedduck_common::ByteString,
    pub logical_type: LogicalType,
    /// The field id, if the file has one for the column.
    pub identifier: Option<Value>,
    pub children: Vec<ColumnDefinition>,
}

/// An open Parquet file with its parsed schema.
#[derive(Debug)]
pub struct ParquetReader {
    pub context: ReaderContext,
    pub options: ParquetOptions,
    pub root_schema: ColumnSchema,
    pub columns: Vec<ColumnDefinition>,
    /// The length of the encoded footer.
    pub footer_len: u32,
}

impl ParquetReader {
    /// Opens `path` and parses its footer and schema (`ParquetReader::ParquetReader`);
    /// with a metadata cache, a cached footer of the unchanged file is reused.
    pub fn open(path: &[u8], options: ParquetOptions) -> Result<Self> {
        // Remote footer reads use the same bounded 16–256 KiB estimate as C++.
        // Row-group buffering remains controlled independently by `prefetch`.
        let prefetch_footer = options.prefetch
            || (!options.disable_prefetch && roastedduck_function::file_system::is_remote_file(path));
        let file = Arc::new(ParquetFile::open(
            path,
            options.file_cache.clone(),
            options.cache_validation,
        )?);
        if let Some(key) = &options.encryption {
            let loaded = load_metadata(&file, Some(key), prefetch_footer)?;
            let decryption = loaded.decryption.map(Arc::new);
            let mut reader =
                Self::from_metadata(file, Arc::new(loaded.metadata), loaded.footer_len, options)?;
            reader.context.decryption = decryption;
            return Ok(reader);
        }
        if let Some(cache) = &options.metadata_cache {
            let key = CachedMetadata::key(path);
            if let Some(entry) = cache.get::<CachedMetadata>(&key)
                && entry.is_valid_for(file.handle())?
            {
                let metadata = Arc::clone(&entry.metadata);
                return Self::from_metadata(file, metadata, entry.footer_len, options);
            }
            let loaded = load_metadata(&file, None, prefetch_footer)?;
            let metadata = Arc::new(loaded.metadata);
            let handle = file.handle();
            cache.put(
                key,
                Arc::new(CachedMetadata {
                    metadata: Arc::clone(&metadata),
                    footer_len: loaded.footer_len,
                    validate: handle.validate(),
                    last_modified: handle.last_modified()?,
                    version_tag: handle.version_tag()?,
                }),
            );
            return Self::from_metadata(file, metadata, loaded.footer_len, options);
        }
        let loaded = load_metadata(&file, None, prefetch_footer)?;
        Self::from_metadata(file, Arc::new(loaded.metadata), loaded.footer_len, options)
    }

    /// A reader of `file` whose footer is `metadata` (`InitializeSchema`).
    pub fn from_metadata(
        file: Arc<ParquetFile>,
        metadata: Arc<FileMetaData>,
        footer_len: u32,
        options: ParquetOptions,
    ) -> Result<Self> {
        let path = roastedduck_common::ByteString::from(file.path());
        if let Some(algorithm) = &metadata.encryption_algorithm
            && algorithm.aes_gcm_ctr_v1.is_some()
        {
            return Err(Error::invalid_input(roastedduck_common::byte_format!(
                "File '{path}' is encrypted with AES_GCM_CTR_V1, but only AES_GCM_V1 is supported"
            )));
        }
        if metadata.schema.len() < 2 {
            return Err(Error::invalid_input(roastedduck_common::byte_format!(
                "Failed to read Parquet file '{path}': Need at least one non-root column in the file"
            )));
        }
        // `GeoParquetFileMetadata::TryRead`.
        let geo = if options.geoparquet_conversion {
            crate::geo::GeoParquetMetadata::read(&metadata)?
        } else {
            None
        };
        let root_schema = SchemaParser::new(&metadata, &options, &path)
            .with_geo(geo.as_ref())
            .parse()?;
        let columns = root_schema
            .children
            .iter()
            .map(|column| column_definition(&metadata, column))
            .collect();
        Ok(Self {
            context: ReaderContext {
                file_name: Arc::from(path.as_bytes()),
                file,
                metadata,
                decryption: None,
                prefetch: options.prefetch,
            },
            options,
            root_schema,
            columns,
            footer_len,
        })
    }

    pub fn file_name(&self) -> &[u8] {
        &self.context.file_name
    }

    pub fn metadata(&self) -> &FileMetaData {
        &self.context.metadata
    }

    pub fn num_rows(&self) -> u64 {
        self.context.metadata.num_rows.max(0) as u64
    }

    /// The statistics of column `name` over all row groups (`ParquetReader::ReadStatistics`);
    /// `None` when a row group has none for it.
    pub fn read_statistics(
        &self,
        name: &[u8],
        casts: &roastedduck_function::cast::CastFunctionSet,
    ) -> Option<roastedduck_storage::statistics::BaseStatistics> {
        let index = self
            .columns
            .iter()
            .position(|column| column.name.as_bytes() == name)?;
        let schema = self.root_schema.children.get(index)?;
        let mut result: Option<roastedduck_storage::statistics::BaseStatistics> = None;
        for group in &self.context.metadata.row_groups {
            let stats = crate::statistics::transform_column_statistics(
                schema,
                &group.columns,
                self.options.can_have_nan,
                casts,
            )
            .ok()??;
            match &mut result {
                None => result = Some(stats),
                Some(merged) => merged.merge(&stats),
            }
        }
        result
    }

    /// The `file_row_number` statistics of row group `row_group` alone.
    pub fn row_number_statistics_of(
        &self,
        row_group: usize,
    ) -> Option<roastedduck_storage::statistics::BaseStatistics> {
        use roastedduck_storage::statistics::BaseStatistics;
        let groups = &self.context.metadata.row_groups;
        let group = groups.get(row_group)?;
        if group.num_rows == 0 {
            return Some(BaseStatistics::create_empty(LogicalType::BIGINT));
        }
        let offset: i64 = groups[..row_group].iter().map(|group| group.num_rows).sum();
        let mut stats = BaseStatistics::create_unknown(LogicalType::BIGINT);
        if let roastedduck_storage::statistics::StatsData::Numeric(numeric) = stats.data_mut() {
            numeric.min = Some(roastedduck_storage::statistics::NumericValue::I64(offset));
            numeric.max = Some(roastedduck_storage::statistics::NumericValue::I64(
                offset + group.num_rows - 1,
            ));
        }
        stats.set_validity(false, true);
        Some(stats)
    }

    /// The statistics of the `file_row_number` column (`ParquetColumnSchema::Stats` of a
    /// `FILE_ROW_NUMBER` column): each row group's range of row numbers.
    pub fn row_number_statistics(&self) -> Option<roastedduck_storage::statistics::BaseStatistics> {
        use roastedduck_storage::statistics::BaseStatistics;
        let mut result: Option<BaseStatistics> = None;
        let mut offset = 0i64;
        for group in &self.context.metadata.row_groups {
            let stats = if group.num_rows == 0 {
                BaseStatistics::create_empty(LogicalType::BIGINT)
            } else {
                let mut stats = BaseStatistics::create_unknown(LogicalType::BIGINT);
                if let roastedduck_storage::statistics::StatsData::Numeric(numeric) = stats.data_mut() {
                    numeric.min = Some(roastedduck_storage::statistics::NumericValue::I64(offset));
                    numeric.max = Some(roastedduck_storage::statistics::NumericValue::I64(
                        offset + group.num_rows - 1,
                    ));
                }
                // `CANNOT_HAVE_NULL_VALUES`.
                stats.set_validity(false, true);
                stats
            };
            offset += group.num_rows;
            match &mut result {
                None => result = Some(stats),
                Some(merged) => merged.merge(&stats),
            }
        }
        result
    }

    pub fn num_row_groups(&self) -> usize {
        self.context.metadata.row_groups.len()
    }

    /// Prepares a scan of `row_groups` reading the file columns `columns`
    /// (`InitializeScan`).
    ///
    /// `filters` are applied while reading, keyed by position in `columns`: the rows a
    /// scan returns pass all of them.
    pub fn initialize_scan(
        &self,
        row_groups: Vec<usize>,
        columns: &[ScanColumn],
        filters: Vec<(usize, TableFilter)>,
    ) -> Result<ReaderScanState> {
        let file_columns = self.root_schema.children.len();
        let mut children: Vec<Option<ColumnReader>> = Vec::new();
        // One more slot for the `file_row_number` reader.
        children.resize_with(file_columns + 1, || None);
        let mut column_ids = Vec::with_capacity(columns.len());
        for column in columns {
            let Some(index) = column.file_index else {
                children[file_columns] = Some(ColumnReader::RowNumber(RowNumberReader::default()));
                column_ids.push(file_columns);
                continue;
            };
            let schema = self
                .root_schema
                .children
                .get(index)
                .ok_or_else(|| Error::internal("scanned column index out of range"))?;
            if children[index].is_none() {
                children[index] = Some(ColumnReader::create(&self.context, schema, &column.children)?);
            }
            column_ids.push(index);
        }
        Ok(ReaderScanState {
            context: self.context.clone(),
            group_indexes: row_groups,
            current_group: None,
            offset_in_group: 0,
            group_rows: 0,
            finished: false,
            root: StructReader::new(&self.root_schema, children),
            column_ids,
            prefetch: self.options.prefetch,
            read_ahead: crate::file::ReadAheadBuffer::default(),
            filters,
            columns: Vec::new(),
            defines: vec![0; STANDARD_VECTOR_SIZE],
            repeats: vec![0; STANDARD_VECTOR_SIZE],
        })
    }
}

/// A footer in the metadata cache (`ParquetFileMetadataCache`), with what identifies the
/// file version it was read from.
#[derive(Debug)]
pub struct CachedMetadata {
    pub metadata: Arc<FileMetaData>,
    footer_len: u32,
    /// Whether the file's handle validated cached data (`validate`).
    validate: bool,
    /// The file's modification time (microseconds) and version tag when it was read.
    last_modified: i64,
    version_tag: String,
}

impl CachedMetadata {
    /// The object cache key of the footer of `path` (`GetWithTypePrefix`).
    pub fn key(path: &[u8]) -> roastedduck_common::ByteString {
        roastedduck_common::byte_format!("parquet_metadata{path}")
    }

    /// Whether the cached footer is that of the file `handle` reads
    /// (`ParquetFileMetadataCache::IsValid`).
    pub fn is_valid_for(
        &self,
        handle: &roastedduck_function::caching_file_system::CachingFileHandle,
    ) -> Result<bool> {
        Ok(
            roastedduck_function::external_file_cache::ExternalFileCache::is_valid(
                self.validate,
                &self.version_tag,
                self.last_modified,
                &handle.version_tag()?,
                handle.last_modified()?,
            ),
        )
    }

    /// The cached footer of `path`, if the file has not changed since (`TryLoadCaches`).
    pub fn lookup(cache: &roastedduck_function::ObjectCache, path: &[u8]) -> Option<Arc<Self>> {
        let entry = cache.get::<Self>(&Self::key(path))?;
        let metadata = std::fs::metadata(roastedduck_function::file_system::native_path(path).ok()?).ok()?;
        let (last_modified, version_tag) = roastedduck_function::caching_file_system::file_version(&metadata);
        roastedduck_function::external_file_cache::ExternalFileCache::is_valid(
            entry.validate,
            &entry.version_tag,
            entry.last_modified,
            &version_tag,
            last_modified,
        )
        .then_some(entry)
    }
}

/// `ParquetReader::ParseColumnDefinition`.
fn column_definition(metadata: &FileMetaData, column: &ColumnSchema) -> ColumnDefinition {
    let mut result = ColumnDefinition {
        name: column.name.clone(),
        logical_type: column.logical_type.clone(),
        identifier: None,
        children: Vec::new(),
    };
    if column.kind == SchemaKind::FileRowNumber {
        return result;
    }
    let field_id = column
        .schema_index
        .and_then(|index| metadata.schema.get(index))
        .and_then(|element| element.field_id)
        .or_else(|| {
            column
                .parent_schema_index
                .and_then(|index| metadata.schema.get(index))
                .and_then(|element| element.field_id)
        });
    result.identifier = field_id.map(Value::integer);
    if column.kind != SchemaKind::Geometry {
        result.children = column
            .children
            .iter()
            .map(|child| column_definition(metadata, child))
            .collect();
    }
    result
}

/// A file column a scan reads, with the struct fields to read below it.
#[derive(Clone, Debug)]
pub struct ScanColumn {
    /// The index of the column in the file, `None` for `file_row_number`.
    pub file_index: Option<usize>,
    pub children: Vec<ChildProjection>,
}

impl ScanColumn {
    /// All of file column `index`.
    pub fn column(index: usize) -> Self {
        Self {
            file_index: Some(index),
            children: Vec::new(),
        }
    }

    /// The `file_row_number` virtual column.
    pub fn file_row_number() -> Self {
        Self {
            file_index: None,
            children: Vec::new(),
        }
    }
}

/// The state of a scan of some row groups of a file (`ParquetReaderScanState`).
#[derive(Debug)]
pub struct ReaderScanState {
    context: ReaderContext,
    group_indexes: Vec<usize>,
    /// The position in `group_indexes` of the group being read.
    current_group: Option<usize>,
    offset_in_group: u64,
    group_rows: u64,
    finished: bool,
    root: StructReader,
    /// The file column of each output column.
    column_ids: Vec<usize>,
    /// Whether the row groups are prefetched (DuckDB's `prefetch_mode`).
    prefetch: bool,
    /// The prefetched ranges of the current row group.
    read_ahead: crate::file::ReadAheadBuffer,
    /// The filters to apply, by output column (`ScanFilter`s); evaluated first, so the
    /// other columns are skipped when no row passes.
    filters: Vec<(usize, TableFilter)>,
    /// The columns of the chunk being read.
    columns: Vec<Option<Vector>>,
    defines: Vec<u8>,
    repeats: Vec<u8>,
}

impl ReaderScanState {
    /// Restarts the scan on `row_groups`, reusing the column readers.
    pub fn restart(&mut self, row_groups: Vec<usize>) {
        self.group_indexes = row_groups;
        self.current_group = None;
        self.offset_in_group = 0;
        self.group_rows = 0;
        self.finished = false;
    }

    /// Reads the next chunk of the scanned columns into `output` (one vector per scan
    /// column); returns its row count, 0 once the scan is done (`ParquetReader::Scan`).
    pub fn scan(&mut self, output: &mut Vec<Vector>) -> Result<usize> {
        loop {
            if self.finished {
                return Ok(0);
            }
            if self.current_group.is_none() || self.offset_in_group >= self.group_rows {
                let next = self.current_group.map_or(0, |group| group + 1);
                self.current_group = Some(next);
                self.offset_in_group = 0;
                let Some(&group_index) = self.group_indexes.get(next) else {
                    self.finished = true;
                    return Ok(0);
                };
                let metadata = Arc::clone(&self.context.metadata);
                let group = metadata
                    .row_groups
                    .get(group_index)
                    .ok_or_else(|| Error::internal("row group index out of range"))?;
                self.read_ahead.clear();
                self.group_rows = group.num_rows.max(0) as u64;
                let mut to_scan = 0;
                for &column in &self.column_ids {
                    let reader = self.root.child_mut(column)?;
                    reader.initialize_read(group_index, group, &self.context)?;
                    to_scan += reader.total_compressed_size();
                }
                if self.prefetch && self.group_rows > 0 {
                    self.prefetch_row_group(group, to_scan)?;
                }
                continue;
            }
            let count = (STANDARD_VECTOR_SIZE as u64).min(self.group_rows - self.offset_in_group) as usize;
            self.offset_in_group += count as u64;
            self.columns.clear();
            self.columns.resize(self.column_ids.len(), None);
            // `ParquetReader::Scan`: the filtered columns first.
            let mut selection: Option<Vec<u32>> = None;
            for filter_index in 0..self.filters.len() {
                let position = self.filters[filter_index].0;
                let vector = self.read_column(position, count)?;
                let rows = selection.get_or_insert_with(|| (0..count as u32).collect());
                let reader = self.root.child_mut(self.column_ids[position])?;
                filter_rows(
                    reader,
                    filter_index,
                    &self.filters[filter_index].1,
                    &vector,
                    count,
                    rows,
                )?;
                self.columns[position] = Some(vector);
                if rows.is_empty() {
                    break;
                }
            }
            if selection.as_ref().is_some_and(Vec::is_empty) {
                // No row passes: skip the columns not read yet.
                for (position, column) in self.columns.iter().enumerate() {
                    if column.is_none() {
                        self.root.child_mut(self.column_ids[position])?.skip(count);
                    }
                }
                continue;
            }
            for position in 0..self.column_ids.len() {
                if self.columns[position].is_none() {
                    let vector = self.read_column(position, count)?;
                    self.columns[position] = Some(vector);
                }
            }
            let selection = selection
                .filter(|rows| rows.len() < count)
                .map(SelectionVector::new);
            output.clear();
            for column in &mut self.columns {
                let vector = column
                    .take()
                    .ok_or_else(|| Error::internal("unread scan column"))?;
                output.push(match &selection {
                    Some(selection) => vector.slice(selection),
                    None => vector,
                });
            }
            return Ok(selection.map_or(count, |selection| selection.len()));
        }
    }

    /// The prefetch of a row group about to be read, of which the scanned columns span
    /// `to_scan` compressed bytes (`ParquetReader::Scan` in the prefetch mode): the whole
    /// group when nearly all of it is read and nothing is filtered, else each column
    /// (consecutive unfiltered ones merged), fetched on first use when a filter may make
    /// some unnecessary.
    fn prefetch_row_group(&mut self, group: &crate::thrift::RowGroup, to_scan: u64) -> Result<()> {
        /// `ParquetReaderPrefetchConfig::WHOLE_GROUP_PREFETCH_MINIMUM_SCAN`.
        const WHOLE_GROUP_PREFETCH_MINIMUM_SCAN: f64 = 0.95;
        let span = group_span(group);
        if to_scan > span {
            return Err(Error::new_bytes(
                roastedduck_common::ErrorKind::Io,
                roastedduck_common::byte_format!(
                    "The parquet file '{}' seems to have incorrectly set page offsets. This interferes with DuckDB's prefetching optimization. DuckDB may still be able to scan this file by manually disabling the prefetching mechanism using: 'SET disable_parquet_prefetching=true'.",
                    self.context.file_name
                ),
            ));
        }
        let file = Arc::clone(&self.context.file);
        let scan_percentage = to_scan as f64 / span as f64;
        if self.filters.is_empty() && scan_percentage > WHOLE_GROUP_PREFETCH_MINIMUM_SCAN {
            if group_compressed_size(group) > 0 {
                self.read_ahead.prefetch_range(&file, group_offset(group), span)?;
            }
        } else {
            let is_filter = |filter: &TableFilter| {
                !matches!(
                    filter,
                    TableFilter::Optional(_) | TableFilter::SelectivityOptional(_)
                )
            };
            // Lazy fetching: a range is read on first use, so columns all of whose rows
            // a filter removes are never read.
            let lazy_fetch = self.filters.iter().any(|(_, filter)| is_filter(filter));
            for (position, &column) in self.column_ids.iter().enumerate() {
                let has_filter = self
                    .filters
                    .iter()
                    .any(|(filtered, filter)| *filtered == position && is_filter(filter));
                self.root
                    .child_mut(column)?
                    .register_prefetch(&mut self.read_ahead, &file, !has_filter)?;
            }
            self.read_ahead.finalize_registration();
            if !lazy_fetch {
                self.read_ahead.prefetch(&file)?;
            }
        }
        for &column in &self.column_ids {
            self.root.child_mut(column)?.attach_read_heads(&self.read_ahead);
        }
        Ok(())
    }

    /// Reads the next `count` rows of output column `position`.
    fn read_column(&mut self, position: usize, count: usize) -> Result<Vector> {
        let column = self.column_ids[position];
        self.defines[..count].fill(0);
        self.repeats[..count].fill(0);
        let reader = self.root.child_mut(column)?;
        let mut vector = Vector::flat(reader.logical_type(), STANDARD_VECTOR_SIZE);
        let read = reader.read(count, &mut self.defines, &mut self.repeats, &mut vector)?;
        if read != count {
            return Err(Error::invalid_input(roastedduck_common::byte_format!(
                "Mismatch in parquet read for column {column}, expected {count} rows, got {read}"
            )));
        }
        Ok(vector)
    }
}

/// `GetGroupOffset`: where the group's first column chunk starts.
fn group_offset(group: &crate::thrift::RowGroup) -> u64 {
    let mut min_offset = u64::MAX;
    for column in &group.columns {
        let Some(meta) = &column.meta_data else {
            continue;
        };
        if let Some(offset) = meta.dictionary_page_offset {
            min_offset = min_offset.min(offset as u64);
        }
        if let Some(offset) = meta.index_page_offset {
            min_offset = min_offset.min(offset as u64);
        }
        min_offset = min_offset.min(meta.data_page_offset as u64);
    }
    min_offset
}

/// `GetGroupCompressedSize`: the group's `total_compressed_size`, or the sum of its
/// columns' when unset.
fn group_compressed_size(group: &crate::thrift::RowGroup) -> u64 {
    match group.total_compressed_size {
        Some(size) if size != 0 => size as u64,
        _ => group
            .columns
            .iter()
            .filter_map(|column| column.meta_data.as_ref())
            .map(|meta| meta.total_compressed_size as u64)
            .sum(),
    }
}

/// `GetGroupSpan`: the bytes from the first to the end of the last column chunk.
fn group_span(group: &crate::thrift::RowGroup) -> u64 {
    let mut min_offset = u64::MAX;
    let mut max_offset = 0u64;
    for column in &group.columns {
        let Some(meta) = &column.meta_data else {
            continue;
        };
        let mut current = u64::MAX;
        if let Some(offset) = meta.dictionary_page_offset {
            current = current.min(offset as u64);
        }
        if let Some(offset) = meta.index_page_offset {
            current = current.min(offset as u64);
        }
        current = current.min(meta.data_page_offset as u64);
        min_offset = min_offset.min(current);
        max_offset = max_offset.max((meta.total_compressed_size as u64).wrapping_add(current));
    }
    max_offset.wrapping_sub(min_offset)
}

/// Keeps the rows of `rows` whose value in `vector` passes `filter` (`ColumnReader::Filter`):
/// through the dictionary's filter result when the read was dictionary-encoded.
fn filter_rows(
    reader: &mut ColumnReader,
    filter_index: usize,
    filter: &TableFilter,
    vector: &Vector,
    count: usize,
    rows: &mut Vec<u32>,
) -> Result<()> {
    if let (ColumnReader::Leaf(leaf), VectorKind::Dictionary, Sel::Indices(indexes)) =
        (reader, vector.kind(), vector.sel())
        && let Some(mask) = leaf.dictionary_filter(filter_index, filter)?
    {
        let mut kept = 0;
        for index in 0..rows.len() {
            let row = rows[index];
            rows[kept] = row;
            kept += usize::from(mask.get(indexes[row as usize] as usize).copied().unwrap_or(false));
        }
        rows.truncate(kept);
        return Ok(());
    }
    filter.select(vector, count, rows)
}
