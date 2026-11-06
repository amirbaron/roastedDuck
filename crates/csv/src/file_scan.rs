//! The scan of one CSV file (DuckDB's `csv_file_scanner.hpp/.cpp`) and the validation
//! of its boundaries (`csv_validator.hpp/.cpp`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_function::cast::CastFunctionSet;
use rustdb_types::LogicalType;

use crate::buffer::CsvBufferManager;
use crate::error::CsvErrorHandler;
use crate::options::{CsvReaderOptions, MultiFileOptionsView, NewLineIdentifier};
use crate::scanner::boundary::CsvIterator;
use crate::scanner::skip::SkipScanner;
use crate::schema::CsvSchema;
use crate::sniffer::CsvSniffer;
use crate::sniffer::dialect::detect_new_line_delimiter;
use crate::state_machine::{CsvStateMachine, cached_state_machine};

/// The first and last byte a scanner covered (DuckDB's `ValidatorLine`).
#[derive(Clone, Copy, Debug)]
pub struct ValidatorLine {
    pub start_pos: u64,
    pub end_pos: u64,
}

/// Checks that the scanners of a file covered it without gaps (DuckDB's `CSVValidator`).
#[derive(Debug, Default)]
pub struct CsvValidator {
    thread_lines: Mutex<BTreeMap<u64, ValidatorLine>>,
}

/// `ThreadLines::error_margin`.
const ERROR_MARGIN: u64 = 2;

impl CsvValidator {
    pub fn insert(&self, thread_idx: u64, line: ValidatorLine) {
        self.thread_lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(thread_idx, line);
    }

    /// `Verify`.
    pub fn verify(&self) -> Result<()> {
        let lines = self.thread_lines.lock().unwrap_or_else(PoisonError::into_inner);
        let mut initialized = false;
        let mut last_end_pos: u64 = 0;
        for line in lines.values() {
            if initialized {
                if line.start_pos == line.end_pos {
                    last_end_pos = line.end_pos;
                    continue;
                }
                if last_end_pos + ERROR_MARGIN < line.start_pos
                    || line.start_pos < last_end_pos.wrapping_sub(ERROR_MARGIN)
                {
                    return Err(Error::not_implemented(
                        "The Parallel CSV Reader currently does not support a full read on this file.\nTo correctly parse this file, please run with the single threaded error (i.e., parallel = false)\n",
                    ));
                }
            } else {
                initialized = true;
            }
            last_end_pos = line.end_pos;
        }
        Ok(())
    }
}

/// One file being scanned: its sniffed schema, buffers, state machine and errors
/// (DuckDB's `CSVFileScan`).
#[derive(Debug)]
pub struct CsvFileScan {
    pub file_path: rustdb_common::ByteString,
    pub file_idx: u64,
    pub options: CsvReaderOptions,
    pub buffer_manager: Arc<CsvBufferManager>,
    pub state_machine: Arc<CsvStateMachine>,
    pub error_handler: Arc<CsvErrorHandler>,
    /// The names and types of the file's columns.
    pub names: Vec<rustdb_common::ByteString>,
    pub types: Vec<LogicalType>,
    /// The types the projected columns are read as, in file column order.
    pub file_types: Vec<LogicalType>,
    /// The file columns that are read.
    pub projected_columns: BTreeSet<u64>,
    /// (file column, result index) of the projected columns, in file column order.
    pub projection_ids: Vec<(u64, u64)>,
    /// The file columns read, in the order of the output.
    pub column_ids: Vec<u64>,
    /// Types the reader converts file columns to (`cast_map`).
    pub cast_map: BTreeMap<u64, LogicalType>,
    /// Rows skipped beyond the requested ones (comments, empty lines).
    pub skipped_rows: u64,
    /// Where scanning starts (after skipped rows and the header).
    pub start_iterator: CsvIterator,
    pub bytes_read: AtomicU64,
    pub validator: CsvValidator,
    pub casts: Arc<CastFunctionSet>,
}

impl CsvFileScan {
    /// `CSVFileScan(context, file, options, file_options, names, types, file_schema,
    /// per_file_single_threaded, buffer_manager, fixed_schema)`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        casts: Arc<CastFunctionSet>,
        file_path: &[u8],
        file_idx: u64,
        mut options: CsvReaderOptions,
        file_options: &MultiFileOptionsView,
        names: &[rustdb_common::ByteString],
        types: &[LogicalType],
        file_schema: &CsvSchema,
        buffer_manager: Option<Arc<CsvBufferManager>>,
        fixed_schema: bool,
    ) -> Result<Self> {
        let error_handler = Arc::new(CsvErrorHandler::new(options.ignore_errors.value()));
        let buffer_manager = match buffer_manager {
            Some(manager) => manager,
            None => Arc::new(CsvBufferManager::new(&options, file_path)?),
        };
        let mut names = names.to_vec();
        let mut types = types.to_vec();
        if options.auto_detect {
            if fixed_schema {
                let mut sniffer = CsvSniffer::new(
                    &mut options,
                    file_options,
                    Arc::clone(&buffer_manager),
                    Arc::clone(&casts),
                    true,
                );
                sniffer.sniff_csv(false)?;
            } else if file_schema.is_empty() {
                return Err(Error::internal(
                    "CSV File Scanner cannot be created without a schema",
                ));
            } else if buffer_manager.file_size() > 0 {
                options.file_path = file_path.into();
                let mut sniffer = CsvSniffer::new(
                    &mut options,
                    file_options,
                    Arc::clone(&buffer_manager),
                    Arc::clone(&casts),
                    false,
                );
                let result = sniffer.adaptive_sniff(file_schema)?;
                names = result.names;
                types = result.return_types;
            }
        }
        Self::finish(
            casts,
            file_path,
            file_idx,
            options,
            names,
            types,
            buffer_manager,
            error_handler,
        )
    }

    /// `CSVFileScan(context, file, options, file_options)`: a scan that sniffs its file
    /// (for `union_by_name` and `sniff_csv`).
    pub fn sniffed(
        casts: Arc<CastFunctionSet>,
        file_path: &[u8],
        file_idx: u64,
        mut options: CsvReaderOptions,
        file_options: &MultiFileOptionsView,
    ) -> Result<Self> {
        let error_handler = Arc::new(CsvErrorHandler::new(options.ignore_errors.value()));
        let buffer_manager = Arc::new(CsvBufferManager::new(&options, file_path)?);
        let mut names = Vec::new();
        let mut types = Vec::new();
        if (options.auto_detect || file_options.union_by_name) && options.dialect_options.num_cols == 0 {
            let mut sniffer = CsvSniffer::new(
                &mut options,
                file_options,
                Arc::clone(&buffer_manager),
                Arc::clone(&casts),
                true,
            );
            let result = sniffer.sniff_csv(false)?;
            names = result.names;
            types = result.return_types;
        }
        let mut scan = Self::finish(
            casts,
            file_path,
            file_idx,
            options,
            names,
            types,
            buffer_manager,
            error_handler,
        )?;
        scan.set_start()?;
        Ok(scan)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        casts: Arc<CastFunctionSet>,
        file_path: &[u8],
        file_idx: u64,
        mut options: CsvReaderOptions,
        names: Vec<rustdb_common::ByteString>,
        types: Vec<LogicalType>,
        buffer_manager: Arc<CsvBufferManager>,
        error_handler: Arc<CsvErrorHandler>,
    ) -> Result<Self> {
        if options.dialect_options.num_cols == 0 {
            options.dialect_options.num_cols = options.sql_type_list.len() as u64;
        }
        if options.dialect_options.state_machine_options.new_line.value() == NewLineIdentifier::NotSet {
            let new_line = detect_new_line_delimiter(&buffer_manager)?;
            options.dialect_options.state_machine_options.new_line = crate::options::CsvOption::new(new_line);
        }
        let state_machine = Arc::new(CsvStateMachine::with_transitions(
            cached_state_machine(&options.dialect_options.state_machine_options),
            &options,
        ));
        Ok(Self {
            file_path: file_path.into(),
            file_idx,
            options,
            buffer_manager,
            state_machine,
            error_handler,
            names,
            types,
            file_types: Vec::new(),
            projected_columns: BTreeSet::new(),
            projection_ids: Vec::new(),
            column_ids: Vec::new(),
            cast_map: BTreeMap::new(),
            skipped_rows: 0,
            start_iterator: CsvIterator::default(),
            bytes_read: AtomicU64::new(0),
            validator: CsvValidator::default(),
            casts,
        })
    }

    /// `SetStart`: skips the rows to skip and the header.
    pub fn set_start(&mut self) -> Result<()> {
        let rows_to_skip = self.options.dialect_options.skip_rows.value()
            + u64::from(self.state_machine.dialect_options.header.value());
        if rows_to_skip == 0 {
            self.start_iterator.first_one = true;
            return Ok(());
        }
        let mut skip_scanner = SkipScanner::new(
            Arc::clone(&self.buffer_manager),
            Arc::clone(&self.state_machine),
            Arc::clone(&self.error_handler),
            rows_to_skip,
        )?;
        skip_scanner.parse_chunk()?;
        self.skipped_rows = skip_scanner.lines_read().wrapping_sub(rows_to_skip);
        self.start_iterator = skip_scanner.iterator();
        Ok(())
    }

    /// `InitializeFileNamesTypes`: the types of the projected columns, in file order.
    pub fn initialize_file_names_types(&mut self) {
        if self.column_ids.is_empty() {
            self.file_types.push(LogicalType::VARCHAR);
            self.projected_columns.insert(0);
            self.projection_ids.push((0, 0));
            return;
        }
        for (i, &column_id) in self.column_ids.iter().enumerate() {
            let ty = self
                .cast_map
                .get(&column_id)
                .cloned()
                .or_else(|| self.types.get(column_id as usize).cloned())
                .unwrap_or(LogicalType::VARCHAR);
            self.file_types.push(ty);
            self.projected_columns.insert(column_id);
            self.projection_ids.push((column_id, i as u64));
        }
        self.projection_ids.sort_unstable();
        self.file_types = self
            .projection_ids
            .iter()
            .map(|&(_, result_idx)| self.file_types[result_idx as usize].clone())
            .collect();
    }

    /// `InitializeProjection`: reads every column.
    pub fn initialize_projection(&mut self) {
        self.column_ids = (0..self.options.dialect_options.num_cols).collect();
    }
}
