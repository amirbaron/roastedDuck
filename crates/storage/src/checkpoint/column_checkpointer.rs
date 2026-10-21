//! Checkpointing the data of one column (DuckDB's `ColumnDataCheckpointer`,
//! `ColumnCheckpointState::FlushSegmentInternal` and the `Checkpoint` methods of the column
//! data classes).
//!
//! Every codec that supports the column's type analyzes all of its rows; the one with the
//! smallest estimated size (or the one `force_compression` names) then compresses them
//! into segments, which are written to blocks. A standard column is checkpointed together
//! with its validity, so codecs that store NULLs themselves (`DICT_FSST`) can leave an
//! empty validity segment. LIST columns checkpoint their offsets, validity and child;
//! STRUCT and ARRAY columns only their validity and children.

use std::sync::Arc;

use rustdb_buffer::BlockId;
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{GeometryStorageType, LogicalType, LogicalTypeId, PhysicalType, Vector, geometry};

use super::pointers::{BlockPointer, DataPointer, PersistentColumnData};
use super::table_writer::BlockWriter;
use crate::compression::{
    Analyzer, CompressionInfo, CompressionType, Compressor, candidate_functions, covers_validity,
    init_analyze,
};
use crate::data_table::TableIoManager;
use crate::statistics::BaseStatistics;
use crate::table::column_data::{ColumnData, ColumnDataType, ColumnKind, ColumnScanState};
use crate::table::geo_column_data::interpret_layout_statistics;

/// A segment a codec finished: its bytes (the first `segment_size` bytes of its block),
/// row count, statistics, encoding and extra blocks.
#[derive(Debug)]
pub(crate) struct FinishedSegment {
    pub data: Vec<u8>,
    pub count: u64,
    pub stats: BaseStatistics,
    pub compression: CompressionType,
    /// Blocks the segment refers to besides its own (overflow strings, ZSTD pages).
    pub overflow_blocks: Vec<BlockId>,
}

/// Where a codec's finished segments go (DuckDB's `ColumnCheckpointState` with the
/// `PartialBlockManager`).
#[derive(Debug)]
pub(crate) struct SegmentSink<'a> {
    pub(crate) writer: &'a mut BlockWriter,
    /// The type of the segments (`Validity` for validity columns).
    pub(crate) ty: &'a LogicalType,
    pub(crate) info: CompressionInfo,
    pointers: &'a mut Vec<DataPointer>,
    /// The first row of the next segment, within the row group.
    next_row: &'a mut u64,
    row_group_start: u64,
}

impl SegmentSink<'_> {
    /// `ColumnCheckpointState::FlushSegmentInternal`: constant segments need no block,
    /// empty validity segments neither; the others are written to (partial) blocks.
    pub(crate) fn flush(&mut self, segment: FinishedSegment) -> Result<()> {
        if segment.data.len() > self.info.block_size {
            return Err(Error::internal(
                "segment size exceeds block size in ColumnCheckpointState::FlushSegmentInternal",
            ));
        }
        if segment.count == 0 {
            return Ok(());
        }
        let (block_pointer, compression) = if segment.stats.is_constant() {
            (BlockPointer::invalid(), CompressionType::Constant)
        } else if segment.data.is_empty() {
            (BlockPointer::invalid(), segment.compression)
        } else {
            (self.writer.write_segment(&segment.data)?, segment.compression)
        };
        self.pointers.push(DataPointer {
            row_start: *self.next_row + self.row_group_start,
            tuple_count: segment.count,
            block_pointer,
            compression,
            statistics: segment.stats,
            overflow_blocks: segment.overflow_blocks,
        });
        *self.next_row += segment.count;
        Ok(())
    }
}

/// The state shared by the columns of a checkpoint.
#[derive(Debug)]
pub(crate) struct CheckpointContext<'a> {
    pub writer: &'a mut BlockWriter,
    pub io: &'a Arc<TableIoManager>,
    pub info: CompressionInfo,
    /// The compression the column was declared with (`USING COMPRESSION`,
    /// `ColumnCheckpointInfo::GetCompressionType`); `Auto` lets the codecs compete.
    pub column_compression: CompressionType,
    /// `schema.table` of the column, for the log.
    pub table_name: &'a str,
    /// The storage version written: GEOMETRY uses the legacy format before v1.5.0.
    pub storage_version: u64,
    /// `geometry_minimum_shredding_size`.
    pub geometry_minimum_shredding_size: i64,
}

/// `DUCKDB_LOG_TRACE` of the codec `DetectBestCompressionMethod` chose for a column.
fn log_final_analyze(
    table_name: &str,
    column: &ColumnData,
    ty: &LogicalType,
    function: CompressionType,
    score: u64,
) {
    use rustdb_function::logging::{LogLevel, LogScope};
    let Some(logger) = rustdb_function::file_system::opener().and_then(|client| client.logger()) else {
        return;
    };
    if !logger.should_log(LogScope::Database, "", LogLevel::Trace) {
        return;
    }
    let message = format!(
        "ColumnDataCheckpointer FinalAnalyze({}) result for {table_name}.{}({ty}): {score}",
        super::pointers::compression_enum_name(function),
        column.column_index()
    );
    let _ = logger.write_log(LogScope::Database, "", LogLevel::Trace, &message);
}

/// The physical type segments of `ty` store.
fn segment_physical_type(ty: &LogicalType) -> PhysicalType {
    if ty.id() == LogicalTypeId::Validity {
        PhysicalType::Bit
    } else {
        ty.physical_type()
    }
}

fn validity_type() -> LogicalType {
    LogicalType::simple(LogicalTypeId::Validity)
}

/// Checkpoints rows `0..count` of `column` and its children (`ColumnData::Checkpoint` and
/// the overrides of the nested column classes).
pub(crate) fn checkpoint_column(
    column: &ColumnData,
    count: u64,
    row_group_start: u64,
    ctx: &mut CheckpointContext<'_>,
) -> Result<PersistentColumnData> {
    if column.logical_type().id() == LogicalTypeId::Geometry {
        return checkpoint_geometry(column, count, row_group_start, ctx);
    }
    checkpoint_column_data(column, count, row_group_start, ctx)
}

/// The storage version that added GEOMETRY to the core (`Geometry::VERSION_ADDED`).
const GEOMETRY_VERSION_ADDED: u64 = 7;

/// The geometry statistics of a WKB GEOMETRY column: its segments' (the `old_stats`
/// `GeoColumnData::Checkpoint` decides the shredding by).
fn geometry_column_statistics(column: &ColumnData) -> BaseStatistics {
    let mut stats = BaseStatistics::create_empty(column.logical_type().clone());
    for node in column.segments().nodes() {
        stats.merge(&node.node.statistics());
    }
    stats
}

/// Writes rows `0..count` of a GEOMETRY column converted to `storage` into a new column of
/// that layout and checkpoints it (the specialize loop of `GeoColumnData::Checkpoint`).
fn checkpoint_converted_geometry(
    column: &ColumnData,
    count: u64,
    storage: GeometryStorageType,
    row_group_start: u64,
    ctx: &mut CheckpointContext<'_>,
) -> Result<PersistentColumnData> {
    let layout_type = geometry::vectorized_type(storage)?;
    let target = ColumnData::new(
        Arc::clone(ctx.io),
        column.column_index(),
        layout_type,
        ColumnDataType::CheckpointTarget,
    );
    let mut append_state = target.initialize_append()?;
    let mut state = ColumnScanState::for_type(column.logical_type());
    column.initialize_scan(&mut state);
    let mut vector = Vector::flat(column.logical_type().clone(), STANDARD_VECTOR_SIZE);
    let mut row = 0u64;
    while row < count {
        let rows = (count - row).min(STANDARD_VECTOR_SIZE as u64) as usize;
        column.checkpoint_scan(&mut state, row, rows, &mut vector)?;
        let layout = geometry::to_vectorized(&vector, rows, storage)?;
        target.append(&mut append_state, &layout, rows)?;
        row += rows as u64;
    }
    let mut result = checkpoint_column_data(&target, count, row_group_start, ctx)?;
    result.geometry_storage = (storage != GeometryStorageType::Spatial).then_some(storage);
    Ok(result)
}

/// `GeoColumnData::Checkpoint`: an unchanged column keeps its layout; a changed one is
/// written in the spatial extension's legacy format for storage versions before v1.5.0,
/// shredded when the row group is large enough and all its geometries are non-empty and of
/// one shreddable type, and as WKB otherwise.
fn checkpoint_geometry(
    column: &ColumnData,
    count: u64,
    row_group_start: u64,
    ctx: &mut CheckpointContext<'_>,
) -> Result<PersistentColumnData> {
    // A shredded or legacy column is never changed (updates are rewritten as deletes and
    // appends go to new row groups): it keeps its layout.
    if let ColumnKind::Geometry { base, storage } = column.kind() {
        let mut result = checkpoint_column_data(base, count, row_group_start, ctx)?;
        result.geometry_storage = (*storage != GeometryStorageType::Spatial).then_some(*storage);
        return Ok(result);
    }
    if !column.has_any_changes() {
        let mut result = checkpoint_column_data(column, count, row_group_start, ctx)?;
        result.geometry_storage = Some(GeometryStorageType::Wkb);
        return Ok(result);
    }
    if ctx.storage_version < GEOMETRY_VERSION_ADDED {
        return checkpoint_converted_geometry(
            column,
            count,
            GeometryStorageType::Spatial,
            row_group_start,
            ctx,
        );
    }
    let threshold = ctx.geometry_minimum_shredding_size;
    let should_shred = threshold >= 0 && column.count() >= threshold as u64;
    let shred_type = should_shred
        .then(|| geometry_column_statistics(column))
        .and_then(|stats| {
            let data = *stats.geometry()?;
            let (geometry_type, vertex_type) = data.types.try_get_single_type()?;
            // Empty geometries have no vertices to put in the layouts.
            if data.flags.has_empty_geometry() || data.flags.has_empty_part() {
                return None;
            }
            let storage = geometry::storage_type(geometry_type, vertex_type);
            (storage != GeometryStorageType::Wkb).then_some(storage)
        });
    if let Some(storage) = shred_type {
        return checkpoint_converted_geometry(column, count, storage, row_group_start, ctx);
    }
    let mut result = checkpoint_column_data(column, count, row_group_start, ctx)?;
    result.geometry_storage = Some(GeometryStorageType::Wkb);
    Ok(result)
}

/// [`checkpoint_column`] for the column classes other than GEOMETRY.
fn checkpoint_column_data(
    column: &ColumnData,
    count: u64,
    row_group_start: u64,
    ctx: &mut CheckpointContext<'_>,
) -> Result<PersistentColumnData> {
    let ty = column.logical_type().clone();
    let mut result = PersistentColumnData::new(ty.clone());
    match column.kind() {
        ColumnKind::Geometry { .. } => return checkpoint_geometry(column, count, row_group_start, ctx),
        ColumnKind::Validity => {
            let [pointers] =
                checkpoint_segments(column, count, &[ty], &LogicalType::BOOLEAN, row_group_start, ctx)?;
            result.pointers = pointers;
        }
        ColumnKind::Standard { .. } => {
            // `StandardColumnData::Checkpoint`: data and validity in one checkpointer.
            let [pointers, validity] = checkpoint_segments(
                column,
                count,
                &[ty.clone(), validity_type()],
                &ty,
                row_group_start,
                ctx,
            )?;
            result.pointers = pointers;
            let mut validity_data = PersistentColumnData::new(validity_type());
            validity_data.pointers = validity;
            result.child_columns.push(validity_data);
        }
        ColumnKind::List { validity, child } => {
            let [pointers] =
                checkpoint_segments(column, count, &[ty], &LogicalType::UBIGINT, row_group_start, ctx)?;
            result.pointers = pointers;
            result
                .child_columns
                .push(checkpoint_column(validity, count, row_group_start, ctx)?);
            result
                .child_columns
                .push(checkpoint_column(child, child.count(), row_group_start, ctx)?);
        }
        ColumnKind::Array { validity, child } => {
            result
                .child_columns
                .push(checkpoint_column(validity, count, row_group_start, ctx)?);
            result
                .child_columns
                .push(checkpoint_column(child, child.count(), row_group_start, ctx)?);
        }
        ColumnKind::Struct { validity, fields } => {
            result
                .child_columns
                .push(checkpoint_column(validity, count, row_group_start, ctx)?);
            for field in fields {
                result
                    .child_columns
                    .push(checkpoint_column(field, count, row_group_start, ctx)?);
            }
        }
    }
    Ok(result)
}

/// Calls `callback` with every vector of rows `0..count` of `column`, scanned with
/// committed updates as a vector of `vector_type` (`ColumnDataCheckpointer::ScanSegments`:
/// vectors of up to 2048 rows that do not cross the column's segment boundaries).
fn scan_segments(
    column: &ColumnData,
    count: u64,
    vector_type: &LogicalType,
    mut callback: impl FnMut(&Vector, usize) -> Result<()>,
) -> Result<()> {
    let mut state = ColumnScanState::for_type(column.logical_type());
    column.initialize_scan(&mut state);
    let mut vector = Vector::flat(vector_type.clone(), STANDARD_VECTOR_SIZE);
    let mut row = 0u64;
    for node in column.segments().nodes() {
        let segment_end = row + node.node.count();
        while row < segment_end.min(count) {
            let rows = (segment_end.min(count) - row).min(STANDARD_VECTOR_SIZE as u64) as usize;
            column.checkpoint_scan(&mut state, row, rows, &mut vector)?;
            callback(&vector, rows)?;
            row += rows as u64;
        }
    }
    Ok(())
}

/// `ForceCompression`: when `forced` applies to the column, every other codec but
/// `Uncompressed` is ruled out; returns the forced codec, or `Auto` when it does not apply.
fn force_compression(functions: &mut [Option<CompressionType>], forced: CompressionType) -> CompressionType {
    if forced == CompressionType::Auto || !functions.contains(&Some(forced)) {
        return CompressionType::Auto;
    }
    for function in functions.iter_mut() {
        if !matches!(*function, Some(CompressionType::Uncompressed)) && *function != Some(forced) {
            *function = None;
        }
    }
    forced
}

/// Runs the checkpointer for `N` targets that share their scan (`WriteToDisk`): detects the
/// best codec of each and compresses the rows with it; returns the data pointers of each.
fn checkpoint_segments<const N: usize>(
    column: &ColumnData,
    count: u64,
    targets: &[LogicalType; N],
    vector_type: &LogicalType,
    row_group_start: u64,
    ctx: &mut CheckpointContext<'_>,
) -> Result<[Vec<DataPointer>; N]> {
    let info = ctx.info;
    let io = ctx.io;
    // `ColumnDataCheckpointer::Checkpoint`: when none of the targets has changes, their
    // persistent segments are kept as they are (`WritePersistentSegments`).
    let validity = match column.kind() {
        ColumnKind::Standard { validity } if N == 2 => Some(validity.as_ref()),
        _ => None,
    };
    if !column.has_changes() && validity.is_none_or(|validity| !validity.has_changes()) {
        let mut result: [Vec<DataPointer>; N] = std::array::from_fn(|_| Vec::new());
        result[0] = column.persistent_data_pointers(row_group_start)?;
        if let Some(validity) = validity {
            result[1] = validity.persistent_data_pointers(row_group_start)?;
        }
        for pointers in &result {
            ctx.writer.add_reused_pointers(pointers);
        }
        return Ok(result);
    }
    // `DetectBestCompressionMethod`.
    let mut functions: Vec<Vec<Option<CompressionType>>> = Vec::with_capacity(N);
    let mut forced = Vec::with_capacity(N);
    let mut states: Vec<Vec<Option<Box<dyn Analyzer>>>> = Vec::with_capacity(N);
    for ty in targets {
        let physical = segment_physical_type(ty);
        // `DBConfig::GetCompressionFunctions`: the codecs of the type that are not disabled.
        let mut target_functions: Vec<Option<CompressionType>> = candidate_functions(physical)
            .into_iter()
            .filter(|&compression| !info.config.is_disabled(compression))
            .map(Some)
            .collect();
        // The column's `USING COMPRESSION` comes first, then `force_compression`.
        let forced_type = if ctx.column_compression == CompressionType::Auto {
            info.config.force_compression
        } else {
            ctx.column_compression
        };
        forced.push(force_compression(&mut target_functions, forced_type));
        let mut target_states = Vec::with_capacity(target_functions.len());
        for function in &target_functions {
            target_states.push(match function {
                Some(compression) => init_analyze(*compression, ty, physical, &info, io)?,
                None => None,
            });
        }
        functions.push(target_functions);
        states.push(target_states);
    }
    scan_segments(column, count, vector_type, |vector, rows| {
        for (target_functions, target_states) in functions.iter_mut().zip(states.iter_mut()) {
            for (function, state) in target_functions.iter_mut().zip(target_states.iter_mut()) {
                if let Some(analyzer) = state
                    && !analyzer.analyze(vector, rows)?
                {
                    *state = None;
                    *function = None;
                }
            }
        }
        Ok(())
    })?;
    let mut chosen: Vec<(CompressionType, Box<dyn Analyzer>)> = Vec::with_capacity(N);
    for ((ty, target_functions), (target_states, forced)) in targets
        .iter()
        .zip(&functions)
        .zip(states.into_iter().zip(&forced))
    {
        let mut best: Option<(CompressionType, Box<dyn Analyzer>)> = None;
        let mut best_score = u64::MAX;
        for (function, state) in target_functions.iter().zip(target_states) {
            let (Some(function), Some(mut state)) = (*function, state) else {
                continue;
            };
            let forced_found = function == *forced;
            let Some(score) = state.final_analyze()? else {
                continue;
            };
            if score < best_score || forced_found {
                best_score = score;
                best = Some((function, state));
            }
            if forced_found {
                break;
            }
        }
        let best = best.ok_or_else(|| {
            Error::new(
                rustdb_common::ErrorKind::Fatal,
                format!("No suitable compression/storage method found to store column of type {ty}"),
            )
        })?;
        log_final_analyze(ctx.table_name, column, ty, best.0, best_score);
        chosen.push(best);
    }
    // `ValidityCoveredByBasedata`: the validity becomes a single empty segment.
    let validity_covered = N == 2 && covers_validity(chosen[0].0);
    let mut compressors: Vec<Box<dyn Compressor>> = Vec::with_capacity(N);
    for (index, (_, analyzer)) in chosen.into_iter().enumerate() {
        if validity_covered && index == 1 {
            compressors.push(Box::new(
                crate::compression::constant::EmptyValidityCompressor::default(),
            ));
        } else {
            compressors.push(analyzer.into_compressor()?);
        }
    }
    let mut pointers: [Vec<DataPointer>; N] = std::array::from_fn(|_| Vec::new());
    let mut next_rows = [0u64; N];
    scan_segments(column, count, vector_type, |vector, rows| {
        for (index, compressor) in compressors.iter_mut().enumerate() {
            let mut sink = SegmentSink {
                writer: &mut *ctx.writer,
                ty: &targets[index],
                info,
                pointers: &mut pointers[index],
                next_row: &mut next_rows[index],
                row_group_start,
            };
            compressor.compress(&mut sink, vector, rows)?;
        }
        Ok(())
    })?;
    for (index, compressor) in compressors.iter_mut().enumerate() {
        let mut sink = SegmentSink {
            writer: &mut *ctx.writer,
            ty: &targets[index],
            info,
            pointers: &mut pointers[index],
            next_row: &mut next_rows[index],
            row_group_start,
        };
        compressor.finalize(&mut sink)?;
    }
    Ok(pointers)
}

/// Merges the statistics of the segments of `data` into `stats` the way loading the column
/// does (`InitializeColumn`): validity segments only contribute NULL information, LIST /
/// ARRAY children and STRUCT fields their own statistics.
pub(crate) fn merge_persistent_statistics(data: &PersistentColumnData, stats: &mut BaseStatistics) {
    // `GeoColumnCheckpointState::GetStatistics`: the statistics of a shredded layout, or of
    // the legacy format just written as a BLOB, are interpreted.
    if stats.logical_type().id() == LogicalTypeId::Geometry
        && data.logical_type.id() != LogicalTypeId::Validity
        && data.logical_type.id() != LogicalTypeId::Geometry
    {
        let storage = data.geometry_storage.unwrap_or(GeometryStorageType::Spatial);
        let mut layout = BaseStatistics::create_empty(data.logical_type.clone());
        merge_persistent_statistics(data, &mut layout);
        let mut row_group = BaseStatistics::create_empty(stats.logical_type().clone());
        interpret_layout_statistics(&layout, storage, &mut row_group);
        stats.merge(&row_group);
        return;
    }
    for pointer in &data.pointers {
        if data.logical_type.id() == LogicalTypeId::Validity {
            stats.merge_validity(&pointer.statistics);
        } else {
            stats.merge(&pointer.statistics);
        }
    }
    let Some((validity, children)) = data.child_columns.split_first() else {
        return;
    };
    merge_persistent_statistics(validity, stats);
    match data.logical_type.physical_type() {
        PhysicalType::List | PhysicalType::Array => {
            if let (Some(child), Some(target)) = (children.first(), stats.child_mut()) {
                merge_persistent_statistics(child, target);
            }
        }
        PhysicalType::Struct => {
            if let Some(targets) = stats.struct_children_mut() {
                for (child, target) in children.iter().zip(targets.iter_mut()) {
                    merge_persistent_statistics(child, target);
                }
            }
        }
        _ => {}
    }
}
