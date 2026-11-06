//! The renderers of the output modes (DuckDB's `tools/shell/shell_renderer.cpp`): column
//! renderers that size columns over the whole result (`column`, `table`, `markdown`,
//! `box`, `latex`), row renderers that print rows as they stream (`line`, `list`, `csv`,
//! `json`, `insert`, ...), the duckbox renderer, the `DESCRIBE` renderer and `trash`.

use rustdb_common::ByteString;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rustdb::{
    Connection, DataChunk, LogicalType, LogicalTypeId, MaterializedResult, Result, StreamQueryResult,
};
use rustdb_types::PhysicalType;

use crate::box_renderer::{
    self, BaseResultRenderer, BoxRenderMode, BoxRendererConfig, BoxRendererState, is_json_type,
};
use crate::highlight::{HighlightElementType, ShellHighlight};
use crate::interrupt;
use crate::output::{Output, PrintOutput};
use crate::render_context::RenderContext;
use crate::state::{
    LargeNumberRendering, PagerMode, RenderMode, ShellColumnInfo, ShellFlag, ShellState, ShellTableInfo,
    SuccessState,
};

/// A materialized result with the position of its scan (`MaterializedQueryResult` and
/// its `scan_state`): fetching chunks consumes them, also for later renderings of the
/// same result (`.last`).
#[derive(Clone, Debug)]
pub struct ScannedResult {
    pub result: Arc<MaterializedResult>,
    scan_position: Arc<AtomicUsize>,
}

impl ScannedResult {
    pub fn new(result: MaterializedResult) -> Self {
        Self {
            result: Arc::new(result),
            scan_position: Arc::default(),
        }
    }

    /// `MaterializedQueryResult::Fetch`: the next non-empty chunk.
    pub fn fetch(&self) -> Result<Option<DataChunk>> {
        let chunk_count = self.result.chunk_count();
        loop {
            let position = self.scan_position.fetch_add(1, Ordering::AcqRel);
            if position >= chunk_count {
                self.scan_position.store(chunk_count, Ordering::Release);
                return Ok(None);
            }
            if self.result.chunk_len(position) > 0 {
                return self.result.chunk(position).map(|chunk| Some(chunk.into_owned()));
            }
        }
    }
}

/// The result a renderer reads (`duckdb::QueryResult`).
#[derive(Debug)]
pub enum ResultSource {
    Materialized(ScannedResult),
    Stream(StreamQueryResult),
}

impl ResultSource {
    fn names(&self) -> &[ByteString] {
        match self {
            Self::Materialized(scanned) => scanned.result.names(),
            Self::Stream(stream) => stream.names(),
        }
    }

    fn types(&self) -> &[LogicalType] {
        match self {
            Self::Materialized(scanned) => scanned.result.types(),
            Self::Stream(stream) => stream.types(),
        }
    }
}

/// `GetTypeName`: the type names of `.mode latex` (and `ColumnTypeIsInteger`).
fn type_name(ty: &LogicalType) -> &'static str {
    match ty.id() {
        LogicalTypeId::Boolean => "BOOLEAN",
        LogicalTypeId::TinyInt => "TINYINT",
        LogicalTypeId::SmallInt => "SMALLINT",
        LogicalTypeId::Integer => "INTEGER",
        LogicalTypeId::BigInt => "BIGINT",
        LogicalTypeId::Float => "FLOAT",
        LogicalTypeId::Double => "DOUBLE",
        LogicalTypeId::Decimal => "DECIMAL",
        LogicalTypeId::Date => "DATE",
        LogicalTypeId::Time | LogicalTypeId::TimeNs => "TIME",
        LogicalTypeId::Timestamp
        | LogicalTypeId::TimestampNs
        | LogicalTypeId::TimestampMs
        | LogicalTypeId::TimestampSec => "TIMESTAMP",
        LogicalTypeId::Varchar => "VARCHAR",
        LogicalTypeId::List => "LIST",
        LogicalTypeId::Map => "MAP",
        LogicalTypeId::Struct => "STRUCT",
        LogicalTypeId::Blob => "BLOB",
        _ => "NULL",
    }
}

/// `LogicalType::IsNested`: a type stored as a struct, list or array.
fn is_nested(ty: &LogicalType) -> bool {
    matches!(
        ty.physical_type(),
        PhysicalType::Struct | PhysicalType::List | PhysicalType::Array
    )
}

/// `LogicalType::IsTemporal`.
fn is_temporal(ty: &LogicalType) -> bool {
    matches!(
        ty.id(),
        LogicalTypeId::Date
            | LogicalTypeId::Time
            | LogicalTypeId::TimeNs
            | LogicalTypeId::Timestamp
            | LogicalTypeId::TimeTz
            | LogicalTypeId::TimestampTz
            | LogicalTypeId::TimestampSec
            | LogicalTypeId::TimestampMs
            | LogicalTypeId::TimestampNs
    )
}

/// The names and types of a result's columns (`ResultMetadata`).
#[derive(Clone, Debug)]
pub struct ResultMetadata {
    pub column_names: Vec<ByteString>,
    pub types: Vec<LogicalType>,
    pub type_names: Vec<&'static str>,
}

impl ResultMetadata {
    fn new(source: &ResultSource) -> Self {
        Self {
            column_names: source.names().to_vec(),
            types: source.types().to_vec(),
            type_names: source.types().iter().map(type_name).collect(),
        }
    }

    pub fn column_count(&self) -> usize {
        self.column_names.len()
    }
}

/// A row as text (`RowData`): NULLs are the renderer's NULL text.
#[derive(Clone, Debug, Default)]
pub struct RowData {
    pub data: Vec<ByteString>,
    pub is_null: Vec<bool>,
    pub row_index: usize,
}

/// A chunk converted to text, by column (`None` for NULL).
#[derive(Clone, Debug)]
pub struct StringChunk {
    columns: Vec<Vec<Option<String>>>,
    len: usize,
}

/// A result as the renderers read it (`RenderingQueryResult`): the chunks fetched so far,
/// converted to text.
#[derive(Debug)]
pub struct RenderingQueryResult {
    source: ResultSource,
    pub metadata: ResultMetadata,
    chunks: Vec<StringChunk>,
    exhausted_result: bool,
    loaded_row_count: usize,
}

impl RenderingQueryResult {
    pub fn new(source: ResultSource) -> Self {
        Self {
            metadata: ResultMetadata::new(&source),
            source,
            chunks: Vec::new(),
            exhausted_result: false,
            loaded_row_count: 0,
        }
    }

    pub fn column_count(&self) -> usize {
        self.metadata.column_count()
    }

    /// `TryConvertChunk`: fetches and converts the next chunk; `false` at the end.
    fn try_convert_chunk<R: ShellRenderer + ?Sized>(
        &mut self,
        renderer: &R,
        conn: &mut Connection,
    ) -> Result<bool> {
        if self.exhausted_result {
            return Ok(false);
        }
        let fetched = match &self.source {
            ResultSource::Materialized(scanned) => match scanned.fetch()? {
                Some(chunk) => Some(renderer.convert_chunk(conn, &chunk)?),
                None => None,
            },
            // A fetch that fails ends a streaming result: DuckDB's `QueryResult::Fetch` then
            // returns nothing and keeps the error on the result, which the shell does not
            // look at.
            ResultSource::Stream(stream) => match conn.fetch(stream) {
                Ok(Some(chunk)) => Some(renderer.convert_chunk(conn, &chunk)?),
                Ok(None) | Err(_) => None,
            },
        };
        let Some(mut chunk) = fetched else {
            self.exhausted_result = true;
            return Ok(false);
        };
        if renderer.has_convert_value() {
            for column in &mut chunk.columns {
                for value in column.iter_mut().flatten() {
                    if renderer.should_convert_value(value) {
                        *value = renderer.convert_value(value);
                    }
                }
            }
        }
        self.loaded_row_count += chunk.len;
        self.chunks.push(chunk);
        Ok(true)
    }

    /// The row at `cursor` (`RenderingResultIterator`), loading chunks as needed; `None`
    /// past the end.
    fn row<R: ShellRenderer + ?Sized>(
        &mut self,
        cursor: &mut RowCursor,
        renderer: &R,
        conn: &mut Connection,
    ) -> Result<Option<RowData>> {
        while cursor.row_index >= self.loaded_row_count {
            if self.exhausted_result || !self.try_convert_chunk(renderer, conn)? {
                return Ok(None);
            }
        }
        while cursor.row_in_chunk >= self.chunks[cursor.chunk_idx].len {
            cursor.row_in_chunk = 0;
            cursor.chunk_idx += 1;
        }
        let chunk = &self.chunks[cursor.chunk_idx];
        let mut row = RowData {
            data: Vec::with_capacity(chunk.columns.len()),
            is_null: Vec::with_capacity(chunk.columns.len()),
            row_index: cursor.row_index,
        };
        for column in &chunk.columns {
            if let Some(value) = &column[cursor.row_in_chunk] {
                row.data.push(value.clone().into());
                row.is_null.push(false);
            } else {
                row.data.push(renderer.null_value().into());
                row.is_null.push(true);
            }
        }
        cursor.row_index += 1;
        cursor.row_in_chunk += 1;
        Ok(Some(row))
    }
}

/// The position of a [`RenderingQueryResult`] iteration.
#[derive(Clone, Copy, Debug, Default)]
struct RowCursor {
    row_index: usize,
    chunk_idx: usize,
    row_in_chunk: usize,
}

/// How a value is aligned in its column (`TextAlignment`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlignment {
    Center,
    Left,
    Right,
}

/// Where rendered text goes (`PrintStream`).
pub trait PrintStream {
    fn print(&mut self, text: &str);
    fn print_bytes(&mut self, text: &[u8]);
    /// Whether the stream can take terminal color codes.
    fn supports_highlight(&self) -> bool {
        true
    }
}

/// `RenderAlignedValue`: `text` padded to `width` columns.
fn render_aligned_bytes(out: &mut dyn PrintStream, text: &[u8], width: usize, alignment: TextAlignment) {
    let n = ShellState::render_length(text);
    let space_count = width.saturating_sub(n);
    let (lspace, rspace) = match alignment {
        TextAlignment::Left => (0, space_count),
        TextAlignment::Right => (space_count, 0),
        TextAlignment::Center => (space_count / 2, space_count.div_ceil(2)),
    };
    if lspace > 0 {
        out.print(&" ".repeat(lspace));
    }
    out.print_bytes(text);
    if rspace > 0 {
        out.print(&" ".repeat(rspace));
    }
}

fn replace_name_byte(name: &[u8], target: u8, replacement: &[u8]) -> ByteString {
    let mut out = ByteString::new();
    for &byte in name {
        if byte == target {
            out.push_bytes(replacement);
        } else {
            out.push(byte);
        }
    }
    out
}

/// `PrintDashes`.
fn print_dashes(out: &mut dyn PrintStream, count: usize) {
    out.print(&"-".repeat(count));
}

/// `OutputQuotedString`: a SQL string literal.
fn output_quoted_string(out: &mut dyn PrintStream, text: impl AsRef<[u8]>) {
    let text = text.as_ref();
    let mut quoted = Vec::with_capacity(text.len() + 2);
    quoted.push(b'\'');
    for &byte in text {
        quoted.push(byte);
        if byte == b'\'' {
            quoted.push(byte);
        }
    }
    quoted.push(b'\'');
    out.print_bytes(&quoted);
}

/// The shell's output as a [`PrintStream`] (`PrintStream`).
#[derive(Debug)]
pub struct StatePrintStream {
    out: Output,
}

impl StatePrintStream {
    pub fn new(out: Output) -> Self {
        Self { out }
    }
}

impl PrintStream for StatePrintStream {
    fn print_bytes(&mut self, text: &[u8]) {
        self.out.print(PrintOutput::Stdout, text);
    }
    fn print(&mut self, text: &str) {
        self.out.print(PrintOutput::Stdout, text);
    }
}

/// Measures the widest line of the output instead of printing it
/// (`WidthMeasuringStream`).
#[derive(Debug, Default)]
struct WidthMeasuringStream {
    output: ByteString,
    max_width: usize,
}

impl PrintStream for WidthMeasuringStream {
    fn print(&mut self, text: &str) {
        self.print_bytes(text.as_bytes());
    }
    fn print_bytes(&mut self, text: &[u8]) {
        for &byte in text {
            match byte {
                b'\r' => {}
                b'\n' => {
                    self.max_width = self.max_width.max(ShellState::render_length(&self.output));
                    self.output.clear();
                }
                _ => self.output.push(byte),
            }
        }
    }

    fn supports_highlight(&self) -> bool {
        false
    }
}

/// The settings a renderer copies from the shell when it is created (the members of
/// `ShellRenderer` and the state it reads while rendering).
#[derive(Clone, Debug)]
pub struct RendererSettings {
    pub show_header: bool,
    pub col_sep: ByteString,
    pub row_sep: ByteString,
    pub null_value: ByteString,
    pub dest_table: ByteString,
    pub newlines: bool,
    pub col_width: Vec<i32>,
    pub pager_min_rows: usize,
    pub max_render_width: usize,
}

impl RendererSettings {
    fn new(state: &ShellState) -> Self {
        Self {
            show_header: state.show_header,
            col_sep: state.col_separator.clone(),
            row_sep: state.row_separator.clone(),
            null_value: state.null_value.clone(),
            dest_table: state.dest_table.clone(),
            newlines: state.has_flag(ShellFlag::Newlines),
            col_width: state.col_width.clone(),
            pager_min_rows: state.pager_min_rows,
            max_render_width: state.max_render_width(),
        }
    }
}

/// A renderer of query results (`ShellRenderer`).
pub trait ShellRenderer {
    fn settings(&self) -> &RendererSettings;

    fn settings_mut(&mut self) -> &mut RendererSettings;

    /// `RenderQueryResult`: the header, the rows and the footer.
    fn render_query_result(
        &mut self,
        out: &mut dyn PrintStream,
        _state: &mut ShellState,
        result: &mut RenderingQueryResult,
        conn: &mut Connection,
    ) -> Result<SuccessState> {
        render_rows(self, out, result, conn)
    }

    /// `RemoveRenderLimits`: `.last` shows everything.
    fn remove_render_limits(&mut self) {}

    /// `Analyze`: work done before rendering, such as sizing columns.
    fn analyze(
        &mut self,
        state: &mut ShellState,
        result: &mut RenderingQueryResult,
        conn: &mut Connection,
    ) -> Result<()> {
        let _ = (state, result, conn);
        Ok(())
    }

    fn render_header(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata) {
        let _ = (out, metadata);
    }

    fn render_row(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata, row: &RowData) {
        let _ = (out, metadata, row);
    }

    fn render_footer(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata) {
        let _ = (out, metadata);
    }

    /// `NullValue`: how NULL renders.
    fn null_value(&self) -> &[u8] {
        &self.settings().null_value
    }

    /// Whether the result must be materialized before rendering.
    fn require_materialized_result(&self) -> bool;

    /// `ShouldUsePager`: whether the result is long or wide enough for the pager.
    fn should_use_pager(
        &mut self,
        state: &mut ShellState,
        result: &mut RenderingQueryResult,
        conn: &mut Connection,
        global_mode: PagerMode,
    ) -> Result<bool>;

    /// `ConvertChunk`: the chunk's values as text.
    fn convert_chunk(&self, conn: &Connection, chunk: &DataChunk) -> Result<StringChunk> {
        convert_chunk_to_varchar(conn, chunk)
    }

    fn has_convert_value(&self) -> bool {
        false
    }

    fn should_convert_value(&self, _value: &str) -> bool {
        false
    }

    fn convert_value(&self, value: &str) -> String {
        value.to_owned()
    }
    fn convert_name(&self, name: &[u8]) -> ByteString {
        name.into()
    }
}

/// `ShellRenderer::ConvertChunk`: every column cast to VARCHAR.
fn convert_chunk_to_varchar(conn: &Connection, chunk: &DataChunk) -> Result<StringChunk> {
    let len = chunk.len();
    let columns = chunk
        .columns()
        .iter()
        .map(|column| conn.cast_to_varchar(column, len))
        .collect::<Result<Vec<_>>>()?;
    Ok(StringChunk { columns, len })
}

/// `ShellRenderer::RenderQueryResult`: the header, every row, the footer.
fn render_rows<R: ShellRenderer + ?Sized>(
    renderer: &mut R,
    out: &mut dyn PrintStream,
    result: &mut RenderingQueryResult,
    conn: &mut Connection,
) -> Result<SuccessState> {
    let metadata = result.metadata.clone();
    renderer.render_header(out, &metadata);
    let mut cursor = RowCursor::default();
    while let Some(row) = result.row(&mut cursor, &*renderer, conn)? {
        if interrupt::seen_interrupt() {
            // Printing stops after an interrupt, so this is not shown (as in DuckDB).
            out.print("Interrupt\n");
            return Ok(SuccessState::Failure);
        }
        renderer.render_row(out, &metadata, &row);
    }
    renderer.render_footer(out, &metadata);
    Ok(SuccessState::Success)
}

/// The column widths of the column renderers (`ColumnRenderer`).
#[derive(Debug)]
struct ColumnRendererState {
    settings: RendererSettings,
    column_width: Vec<usize>,
    right_align: Vec<bool>,
}

impl ColumnRendererState {
    fn new(state: &ShellState) -> Self {
        Self {
            settings: RendererSettings::new(state),
            column_width: Vec::new(),
            right_align: Vec::new(),
        }
    }

    fn alignment(&self, column: usize) -> TextAlignment {
        if self.right_align[column] {
            TextAlignment::Right
        } else {
            TextAlignment::Left
        }
    }
}

/// The parts that differ between the column renderers.
trait ColumnLayout {
    fn column_state(&self) -> &ColumnRendererState;
    fn column_state_mut(&mut self) -> &mut ColumnRendererState;
    fn column_separator(&self) -> &'static str;
    fn row_separator(&self) -> &'static str;
    fn row_start(&self) -> Option<&'static str> {
        None
    }
}

/// `ColumnRenderer::Analyze`: materializes the result and sizes the columns by their
/// names, `.width` and every value.
fn analyze_columns<R: ShellRenderer + ColumnLayout>(
    renderer: &mut R,
    state: &ShellState,
    result: &mut RenderingQueryResult,
    conn: &mut Connection,
) -> Result<()> {
    let names: Vec<ByteString> = result
        .metadata
        .column_names
        .iter()
        .map(|name| renderer.convert_name(name))
        .collect();
    result.metadata.column_names = names;
    while result.try_convert_chunk(&*renderer, conn)? {
        if interrupt::seen_interrupt() {
            state.print("Interrupt\n");
            return Ok(());
        }
    }
    let null_width = crate::utf8::native_terminal_render_width(&renderer.column_state().settings.null_value)?;
    let column_state = renderer.column_state_mut();
    for c in 0..result.column_count() {
        let mut width = column_state.settings.col_width.get(c).copied().unwrap_or(0);
        column_state.right_align.push(width < 0);
        width = width.saturating_abs();
        let name_width = crate::utf8::native_terminal_render_width(&result.metadata.column_names[c])?;
        column_state.column_width.push((width as usize).max(name_width));
    }
    for c in 0..result.column_count() {
        for chunk in &result.chunks {
            for value in &chunk.columns[c] {
                let width = value.as_ref().map_or(null_width, ShellState::render_length);
                if width > column_state.column_width[c] {
                    column_state.column_width[c] = width;
                }
            }
        }
    }
    Ok(())
}

/// `ColumnRenderer::ShouldUsePager`: many rows, or wider than the terminal.
fn columns_should_use_pager(
    column_state: &ColumnRendererState,
    result: &RenderingQueryResult,
    global_mode: PagerMode,
) -> bool {
    if global_mode == PagerMode::On || result.loaded_row_count >= column_state.settings.pager_min_rows {
        return true;
    }
    let total_width: usize = column_state.column_width.iter().map(|width| width + 2).sum();
    total_width > column_state.settings.max_render_width
}

/// `ColumnRenderer::RenderRow`.
fn render_column_row<R: ColumnLayout>(renderer: &R, out: &mut dyn PrintStream, row: &RowData) {
    if let Some(row_start) = renderer.row_start() {
        out.print(row_start);
    }
    let column_state = renderer.column_state();
    for (c, value) in row.data.iter().enumerate() {
        if c > 0 {
            out.print(renderer.column_separator());
        }
        render_aligned_bytes(
            out,
            value,
            column_state.column_width[c],
            column_state.alignment(c),
        );
    }
    out.print(renderer.row_separator());
}

/// Implements [`ShellRenderer`] for a column renderer: the shared analysis, rows and
/// pager decision, with the renderer's own header, footer and value conversion.
macro_rules! column_renderer {
    ($renderer:ty, { $($methods:tt)* }) => {
        impl ShellRenderer for $renderer {
            fn settings(&self) -> &RendererSettings {
                &self.0.settings
            }

            fn settings_mut(&mut self) -> &mut RendererSettings {
                &mut self.0.settings
            }

            fn analyze(
                &mut self,
                state: &mut ShellState,
                result: &mut RenderingQueryResult,
                conn: &mut Connection,
            ) -> Result<()> {
                analyze_columns(self, state, result, conn)
            }

            fn render_row(&mut self, out: &mut dyn PrintStream, _metadata: &ResultMetadata, row: &RowData) {
                render_column_row(self, out, row);
            }

            fn require_materialized_result(&self) -> bool {
                true
            }

            $($methods)*
        }

        impl ColumnLayout for $renderer {
            fn column_state(&self) -> &ColumnRendererState {
                &self.0
            }

            fn column_state_mut(&mut self) -> &mut ColumnRendererState {
                &mut self.0
            }

            fn column_separator(&self) -> &'static str {
                Self::COLUMN_SEPARATOR
            }

            fn row_separator(&self) -> &'static str {
                Self::ROW_SEPARATOR
            }

            fn row_start(&self) -> Option<&'static str> {
                Self::ROW_START
            }
        }
    };
}

/// The pager decision of the column renderers that use it.
macro_rules! column_pager {
    () => {
        fn should_use_pager(
            &mut self,
            _state: &mut ShellState,
            result: &mut RenderingQueryResult,
            _conn: &mut Connection,
            global_mode: PagerMode,
        ) -> Result<bool> {
            Ok(columns_should_use_pager(&self.0, result, global_mode))
        }
    };
}

/// The pager decision of the modes that only page when the pager is on.
macro_rules! pager_only_when_on {
    () => {
        fn should_use_pager(
            &mut self,
            _state: &mut ShellState,
            _result: &mut RenderingQueryResult,
            _conn: &mut Connection,
            global_mode: PagerMode,
        ) -> Result<bool> {
            Ok(global_mode == PagerMode::On)
        }
    };
}

/// `.mode column` (`ModeColumnRenderer`).
#[derive(Debug)]
struct ModeColumnRenderer(ColumnRendererState);

impl ModeColumnRenderer {
    const COLUMN_SEPARATOR: &str = "  ";
    const ROW_SEPARATOR: &str = "\n";
    const ROW_START: Option<&str> = None;
}

column_renderer!(ModeColumnRenderer, {
    column_pager!();

    fn render_header(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata) {
        if !self.0.settings.show_header {
            return;
        }
        let column_count = metadata.column_count();
        for (c, name) in metadata.column_names.iter().enumerate() {
            render_aligned_bytes(out, name, self.0.column_width[c], self.0.alignment(c));
            out.print(if c == column_count - 1 { "\n" } else { "  " });
        }
        for c in 0..column_count {
            print_dashes(out, self.0.column_width[c]);
            out.print(if c == column_count - 1 { "\n" } else { "  " });
        }
    }
});

/// A separator line of `.mode table` (`PrintRowSeparator`).
fn print_table_separator(out: &mut dyn PrintStream, widths: &[usize]) {
    if !widths.is_empty() {
        for width in widths {
            out.print("+");
            print_dashes(out, width + 2);
        }
        out.print("+");
    }
    out.print("\n");
}

/// `.mode table` (`ModeTableRenderer`).
#[derive(Debug)]
struct ModeTableRenderer(ColumnRendererState);

impl ModeTableRenderer {
    const COLUMN_SEPARATOR: &str = " | ";
    const ROW_SEPARATOR: &str = " |\n";
    const ROW_START: Option<&str> = Some("| ");
}

column_renderer!(ModeTableRenderer, {
    column_pager!();

    fn render_header(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata) {
        let column_count = metadata.column_count();
        print_table_separator(out, &self.0.column_width);
        out.print("| ");
        for (c, name) in metadata.column_names.iter().enumerate() {
            render_aligned_bytes(out, name, self.0.column_width[c], TextAlignment::Center);
            out.print(if c == column_count - 1 { " |\n" } else { " | " });
        }
        print_table_separator(out, &self.0.column_width);
    }

    fn render_footer(&mut self, out: &mut dyn PrintStream, _metadata: &ResultMetadata) {
        print_table_separator(out, &self.0.column_width);
    }
});

/// `.mode markdown` (`ModeMarkdownRenderer`): pipes in values are escaped.
#[derive(Debug)]
struct ModeMarkdownRenderer(ColumnRendererState);

impl ModeMarkdownRenderer {
    const COLUMN_SEPARATOR: &str = " | ";
    const ROW_SEPARATOR: &str = " |\n";
    const ROW_START: Option<&str> = Some("| ");
}

column_renderer!(ModeMarkdownRenderer, {
    pager_only_when_on!();

    fn has_convert_value(&self) -> bool {
        true
    }

    fn should_convert_value(&self, value: &str) -> bool {
        value.contains('|')
    }

    fn convert_value(&self, value: &str) -> String {
        value.replace('|', "\\|")
    }
    fn convert_name(&self, name: &[u8]) -> ByteString {
        replace_name_byte(name, b'|', b"\\|")
    }

    fn render_header(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata) {
        out.print("| ");
        for (c, name) in metadata.column_names.iter().enumerate() {
            if c > 0 {
                out.print(" | ");
            }
            render_aligned_bytes(out, name, self.0.column_width[c], TextAlignment::Center);
        }
        out.print(" |\n");
        // The separator, right-aligning numbers.
        if !metadata.types.is_empty() {
            for (ty, width) in metadata.types.iter().zip(&self.0.column_width) {
                out.print("|");
                if ty.is_numeric() {
                    print_dashes(out, width + 1);
                    out.print(":");
                } else {
                    print_dashes(out, width + 2);
                }
            }
            out.print("|");
        }
        out.print("\n");
    }
});

const BOX_24: &str = "─";
const BOX_13: &str = "│";
const BOX_23: &str = "┌";
const BOX_34: &str = "┐";
const BOX_12: &str = "└";
const BOX_14: &str = "┘";
const BOX_123: &str = "├";
const BOX_134: &str = "┤";
const BOX_234: &str = "┬";
const BOX_124: &str = "┴";
const BOX_1234: &str = "┼";

/// A horizontal separator of `.mode box` (`PrintBoxRowSeparator`).
fn print_box_row_separator(out: &mut dyn PrintStream, widths: &[usize], sep1: &str, sep2: &str, sep3: &str) {
    if let Some((first, rest)) = widths.split_first() {
        out.print(sep1);
        out.print(&BOX_24.repeat(first + 2));
        for width in rest {
            out.print(sep2);
            out.print(&BOX_24.repeat(width + 2));
        }
        out.print(sep3);
    }
    out.print("\n");
}

/// `.mode box` (`ModeBoxRenderer`): newlines in values render as `\n`.
#[derive(Debug)]
struct ModeBoxRenderer(ColumnRendererState);

impl ModeBoxRenderer {
    const COLUMN_SEPARATOR: &str = " │ ";
    const ROW_SEPARATOR: &str = " │\n";
    const ROW_START: Option<&str> = Some("│ ");
}

column_renderer!(ModeBoxRenderer, {
    column_pager!();

    fn has_convert_value(&self) -> bool {
        true
    }

    fn should_convert_value(&self, value: &str) -> bool {
        value.contains('\n')
    }

    fn convert_value(&self, value: &str) -> String {
        value.replace('\n', "\\n")
    }
    fn convert_name(&self, name: &[u8]) -> ByteString {
        replace_name_byte(name, b'\n', b"\\n")
    }

    fn render_header(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata) {
        let column_count = metadata.column_count();
        print_box_row_separator(out, &self.0.column_width, BOX_23, BOX_234, BOX_34);
        out.print(&format!("{BOX_13} "));
        for (c, name) in metadata.column_names.iter().enumerate() {
            render_aligned_bytes(out, name, self.0.column_width[c], TextAlignment::Center);
            out.print(if c == column_count - 1 { " │\n" } else { " │ " });
        }
        print_box_row_separator(out, &self.0.column_width, BOX_123, BOX_1234, BOX_134);
    }

    fn render_footer(&mut self, out: &mut dyn PrintStream, _metadata: &ResultMetadata) {
        print_box_row_separator(out, &self.0.column_width, BOX_12, BOX_124, BOX_14);
    }
});

/// `.mode latex` (`ModeLatexRenderer`).
#[derive(Debug)]
struct ModeLatexRenderer(ColumnRendererState);

impl ModeLatexRenderer {
    const COLUMN_SEPARATOR: &str = " & ";
    const ROW_SEPARATOR: &str = " \\\\\n";
    const ROW_START: Option<&str> = None;
}

column_renderer!(ModeLatexRenderer, {
    pager_only_when_on!();

    fn render_header(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata) {
        let column_count = metadata.column_count();
        out.print("\\begin{tabular}{|");
        for type_name in &metadata.type_names {
            out.print(if ShellState::column_type_is_integer(type_name) {
                "r"
            } else {
                "l"
            });
        }
        out.print("|}\n");
        out.print("\\hline\n");
        for (c, name) in metadata.column_names.iter().enumerate() {
            render_aligned_bytes(out, name, self.0.column_width[c], TextAlignment::Center);
            out.print(if c == column_count - 1 {
                Self::ROW_SEPARATOR
            } else {
                Self::COLUMN_SEPARATOR
            });
        }
        out.print("\\hline\n");
    }

    fn render_footer(&mut self, out: &mut dyn PrintStream, _metadata: &ResultMetadata) {
        out.print("\\hline\n");
        out.print("\\end{tabular}\n");
    }
});

/// `RowRenderer::ShouldUsePager`: loads up to the pager's row count, and measures the
/// width of the rendering if the result is shorter.
fn rows_should_use_pager<R: ShellRenderer + ?Sized>(
    renderer: &mut R,
    result: &mut RenderingQueryResult,
    conn: &mut Connection,
    global_mode: PagerMode,
) -> Result<bool> {
    if global_mode == PagerMode::On {
        return Ok(true);
    }
    let min_rows = renderer.settings().pager_min_rows;
    while result.loaded_row_count < min_rows && result.try_convert_chunk(&*renderer, conn)? {}
    if result.loaded_row_count >= min_rows {
        return Ok(true);
    }
    let mut stream = WidthMeasuringStream::default();
    render_rows(renderer, &mut stream, result, conn)?;
    Ok(stream.max_width > renderer.settings().max_render_width)
}

/// Implements the members shared by the row renderers.
macro_rules! row_renderer {
    () => {
        fn settings(&self) -> &RendererSettings {
            &self.settings
        }

        fn settings_mut(&mut self) -> &mut RendererSettings {
            &mut self.settings
        }

        fn require_materialized_result(&self) -> bool {
            false
        }
    };
}

/// The pager decision of the row renderers that use it.
macro_rules! row_pager {
    () => {
        fn should_use_pager(
            &mut self,
            _state: &mut ShellState,
            result: &mut RenderingQueryResult,
            conn: &mut Connection,
            global_mode: PagerMode,
        ) -> Result<bool> {
            rows_should_use_pager(self, result, conn, global_mode)
        }
    };
}

/// `.mode line` (`ModeLineRenderer`): `name = value` per line.
#[derive(Debug)]
struct ModeLineRenderer {
    settings: RendererSettings,
    header_width: usize,
}

impl ShellRenderer for ModeLineRenderer {
    row_renderer!();
    row_pager!();

    fn render_header(&mut self, _out: &mut dyn PrintStream, metadata: &ResultMetadata) {
        self.header_width = metadata
            .column_names
            .iter()
            .map(|name| name.len())
            .fold(5, usize::max);
    }

    fn render_row(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata, row: &RowData) {
        if row.row_index > 0 {
            out.print_bytes(&self.settings.row_sep);
        }
        for (name, value) in metadata.column_names.iter().zip(&row.data) {
            let space_count = self.header_width - name.len();
            if space_count > 0 {
                out.print(&" ".repeat(space_count));
            }
            out.print_bytes(name);
            out.print(" = ");
            out.print_bytes(value);
            out.print_bytes(&self.settings.row_sep);
        }
    }
}

/// The output of `EXPLAIN` (`ModeExplainRenderer`).
#[derive(Debug)]
struct ModeExplainRenderer {
    settings: RendererSettings,
}

impl ShellRenderer for ModeExplainRenderer {
    fn settings(&self) -> &RendererSettings {
        &self.settings
    }

    fn settings_mut(&mut self) -> &mut RendererSettings {
        &mut self.settings
    }

    fn render_row(&mut self, out: &mut dyn PrintStream, _metadata: &ResultMetadata, row: &RowData) {
        let [kind, plan] = row.data.as_slice() else {
            return;
        };
        let title = match kind.as_bytes() {
            b"logical_plan" => Some("││ Unoptimized Logical Plan  ││\n"),
            b"logical_opt" => Some("││  Optimized Logical Plan   ││\n"),
            b"physical_plan" => Some("││       Physical Plan       ││\n"),
            _ => None,
        };
        if let Some(title) = title {
            out.print("\n┌─────────────────────────────┐\n");
            out.print("│┌───────────────────────────┐│\n");
            out.print(title);
            out.print("│└───────────────────────────┘│\n");
            out.print("└─────────────────────────────┘\n");
        }
        out.print_bytes(plan);
    }

    fn require_materialized_result(&self) -> bool {
        true
    }

    fn should_use_pager(
        &mut self,
        _state: &mut ShellState,
        result: &mut RenderingQueryResult,
        _conn: &mut Connection,
        global_mode: PagerMode,
    ) -> Result<bool> {
        if global_mode == PagerMode::On {
            return Ok(true);
        }
        let row_count: usize = result
            .chunks
            .iter()
            .flat_map(|chunk| chunk.columns.get(1).into_iter().flatten().flatten())
            .map(|plan| plan.matches('\n').count())
            .sum();
        Ok(row_count >= self.settings.pager_min_rows)
    }
}

/// `.mode list` (`ModeListRenderer`).
#[derive(Debug)]
struct ModeListRenderer {
    settings: RendererSettings,
}

impl ShellRenderer for ModeListRenderer {
    row_renderer!();
    row_pager!();

    fn render_header(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata) {
        if !self.settings.show_header {
            return;
        }
        out.print_bytes(&ByteString::join(
            &metadata.column_names,
            self.settings.col_sep.as_bytes(),
        ));
        out.print_bytes(&self.settings.row_sep);
    }

    fn render_row(&mut self, out: &mut dyn PrintStream, _metadata: &ResultMetadata, row: &RowData) {
        for (i, value) in row.data.iter().enumerate() {
            if i > 0 {
                out.print_bytes(&self.settings.col_sep);
            }
            out.print_bytes(value);
        }
        out.print_bytes(&self.settings.row_sep);
    }
}

fn html_escape_bytes(text: &[u8]) -> ByteString {
    let mut out = ByteString::new();
    for &byte in text {
        match byte {
            b'<' => out.push_bytes(b"&lt;"),
            b'&' => out.push_bytes(b"&amp;"),
            b'>' => out.push_bytes(b"&gt;"),
            b'"' => out.push_bytes(b"&quot;"),
            b'\'' => out.push_bytes(b"&#39;"),
            _ => out.push(byte),
        }
    }
    out
}
fn escape_c_bytes(text: &[u8]) -> ByteString {
    let mut out = ByteString::from("\"");
    for &byte in text {
        match byte {
            b'\\' => out.push_bytes(b"\\\\"),
            b'"' => out.push_bytes(b"\\\""),
            b'\t' => out.push_bytes(b"\\t"),
            b'\n' => out.push_bytes(b"\\n"),
            b'\r' => out.push_bytes(b"\\r"),
            0x20..=0x7e => out.push(byte),
            _ => out.push_bytes(format!("\\{byte:03o}")),
        }
    }
    out.push(b'"');
    out
}

/// `OutputHTMLString`: text with the characters special to HTML escaped.
/// `.mode html` (`ModeHtmlRenderer`).
#[derive(Debug)]
struct ModeHtmlRenderer {
    settings: RendererSettings,
}

impl ShellRenderer for ModeHtmlRenderer {
    row_renderer!();
    pager_only_when_on!();

    fn render_header(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata) {
        if !self.settings.show_header {
            return;
        }
        out.print("<tr>");
        for name in &metadata.column_names {
            out.print("<th>");
            out.print_bytes(&html_escape_bytes(name));
            out.print("</th>\n");
        }
        out.print("</tr>\n");
    }

    fn render_row(&mut self, out: &mut dyn PrintStream, _metadata: &ResultMetadata, row: &RowData) {
        out.print("<tr>");
        for value in &row.data {
            out.print("<td>");
            out.print_bytes(&html_escape_bytes(value));
            out.print("</td>\n");
        }
        out.print("</tr>\n");
    }
}

/// `.mode tcl` (`ModeTclRenderer`).
#[derive(Debug)]
struct ModeTclRenderer {
    settings: RendererSettings,
}

impl ShellRenderer for ModeTclRenderer {
    row_renderer!();
    row_pager!();

    fn render_header(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata) {
        if !self.settings.show_header {
            return;
        }
        for (i, name) in metadata.column_names.iter().enumerate() {
            if i > 0 {
                out.print_bytes(&self.settings.col_sep);
            }
            out.print_bytes(&escape_c_bytes(name));
        }
        out.print_bytes(&self.settings.row_sep);
    }

    fn render_row(&mut self, out: &mut dyn PrintStream, _metadata: &ResultMetadata, row: &RowData) {
        for (i, value) in row.data.iter().enumerate() {
            if i > 0 {
                out.print_bytes(&self.settings.col_sep);
            }
            out.print(&ShellState::escape_c_string(value));
        }
        out.print_bytes(&self.settings.row_sep);
    }
}

/// `.mode csv` (`ModeCsvRenderer`).
#[derive(Debug)]
struct ModeCsvRenderer {
    settings: RendererSettings,
}

impl ModeCsvRenderer {
    /// `PrintEscapedCSV`: a value holding a control or quote character, a byte above 127
    /// or the separator is written as an identifier.
    fn print_escaped_csv_bytes(&self, out: &mut dyn PrintStream, bytes: &[u8]) {
        let separator = self.settings.col_sep.as_bytes();
        let needs_quote = (0..bytes.len()).any(|idx| {
            let c = bytes[idx];
            c <= 31 || c == b'"' || c == b'\'' || c >= 127 || bytes[idx..].starts_with(separator)
        });
        if needs_quote {
            // `SQLIdentifier`: quoted only if it is not a plain identifier.
            out.print_bytes(&rustdb_ast::keyword::write_optionally_quoted_bytes(bytes));
        } else {
            out.print_bytes(bytes);
        }
    }
}

impl ShellRenderer for ModeCsvRenderer {
    row_renderer!();
    pager_only_when_on!();

    fn render_header(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata) {
        if !self.settings.show_header {
            return;
        }
        for (i, name) in metadata.column_names.iter().enumerate() {
            if i > 0 {
                out.print_bytes(&self.settings.col_sep);
            }
            self.print_escaped_csv_bytes(out, name);
        }
        out.print_bytes(&self.settings.row_sep);
    }

    fn render_row(&mut self, out: &mut dyn PrintStream, _metadata: &ResultMetadata, row: &RowData) {
        for (i, value) in row.data.iter().enumerate() {
            if i > 0 {
                out.print_bytes(&self.settings.col_sep);
            }
            self.print_escaped_csv_bytes(out, value);
        }
        out.print_bytes(&self.settings.row_sep);
    }
}

/// `.mode ascii` (`ModeAsciiRenderer`): as DuckDB's, it separates with newlines.
#[derive(Debug)]
struct ModeAsciiRenderer {
    settings: RendererSettings,
}

impl ShellRenderer for ModeAsciiRenderer {
    row_renderer!();
    pager_only_when_on!();

    fn render_header(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata) {
        if !self.settings.show_header {
            return;
        }
        out.print_bytes(&ByteString::join(
            &metadata.column_names,
            self.settings.col_sep.as_bytes(),
        ));
        out.print_bytes(&self.settings.row_sep);
    }

    fn render_row(&mut self, out: &mut dyn PrintStream, _metadata: &ResultMetadata, row: &RowData) {
        out.print_bytes(&ByteString::join(&row.data, &self.settings.col_sep));
        out.print_bytes(&self.settings.row_sep);
    }
}

/// `.mode quote` (`ModeQuoteRenderer`).
#[derive(Debug)]
struct ModeQuoteRenderer {
    settings: RendererSettings,
}

impl ShellRenderer for ModeQuoteRenderer {
    row_renderer!();
    row_pager!();

    fn render_header(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata) {
        if !self.settings.show_header {
            return;
        }
        for (i, name) in metadata.column_names.iter().enumerate() {
            if i > 0 {
                out.print_bytes(&self.settings.col_sep);
            }
            out.print_bytes(&rustdb_ast::keyword::sql_string_bytes(name));
        }
        out.print_bytes(&self.settings.row_sep);
    }

    fn render_row(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata, row: &RowData) {
        for (i, value) in row.data.iter().enumerate() {
            if i > 0 {
                out.print_bytes(&self.settings.col_sep);
            }
            let ty = &metadata.types[i];
            if ty.is_numeric() || row.is_null[i] || ty.id() == LogicalTypeId::Boolean {
                out.print_bytes(value);
            } else {
                output_quoted_string(out, value);
            }
        }
        out.print_bytes(&self.settings.row_sep);
    }

    fn null_value(&self) -> &'static [u8] {
        b"NULL"
    }
}

/// `.mode json` and `.mode jsonlines` (`ModeJsonRenderer`).
#[derive(Debug)]
struct ModeJsonRenderer {
    settings: RendererSettings,
    json_array: bool,
}

/// `EscapeJSONString`.
fn escape_json_string(text: &[u8]) -> ByteString {
    let mut result = ByteString::from("\"");
    for &byte in text {
        match byte {
            b'\\' | b'"' => {
                result.push(b'\\');
                result.push(byte);
            }
            8 => result.push_bytes("\\b"),
            12 => result.push_bytes("\\f"),
            b'\n' => result.push_bytes("\\n"),
            b'\r' => result.push_bytes("\\r"),
            b'\t' => result.push_bytes("\\t"),
            0..=31 => result.push_bytes(format!("\\u{byte:04x}")),
            _ => result.push(byte),
        }
    }
    result.push(b'"');
    result
}

impl ModeJsonRenderer {
    /// `RequiresQuotes`: everything but booleans and numbers that fit JSON numbers.
    fn requires_quotes(ty: &LogicalType) -> bool {
        if ty.id() == LogicalTypeId::Boolean {
            return false;
        }
        if !ty.is_numeric() {
            return true;
        }
        matches!(
            ty.id(),
            LogicalTypeId::UBigInt
                | LogicalTypeId::HugeInt
                | LogicalTypeId::UHugeInt
                | LogicalTypeId::Decimal
        )
    }

    /// `RequiresJSONCast`: nested values keep their structure, floats their `inf`/`nan`.
    fn requires_json_cast(ty: &LogicalType) -> bool {
        is_nested(ty) || matches!(ty.id(), LogicalTypeId::Float | LogicalTypeId::Double)
    }
}

impl ShellRenderer for ModeJsonRenderer {
    row_renderer!();
    pager_only_when_on!();

    fn render_header(&mut self, out: &mut dyn PrintStream, _metadata: &ResultMetadata) {
        if self.json_array {
            out.print("[");
        }
    }

    fn render_row(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata, row: &RowData) {
        if row.row_index > 0 {
            if self.json_array {
                out.print(",");
            }
            out.print("\n");
        }
        out.print("{");
        for (i, name) in metadata.column_names.iter().enumerate() {
            if i > 0 {
                out.print(",");
            }
            out.print_bytes(&rustdb_common::string_util::json_quote_bytes(name));
            out.print(":");
            let ty = &metadata.types[i];
            if row.is_null[i] {
                out.print("null");
            } else if is_nested(ty) || is_json_type(ty) || !Self::requires_quotes(ty) {
                // Nested values are JSON already; numbers print as they are.
                out.print_bytes(&row.data[i]);
            } else {
                out.print_bytes(&escape_json_string(&row.data[i]));
            }
        }
        out.print("}");
    }

    /// `ConvertChunk`: nested and floating-point columns become JSON first.
    fn convert_chunk(&self, conn: &Connection, chunk: &DataChunk) -> Result<StringChunk> {
        let len = chunk.len();
        let json = LogicalType::VARCHAR.with_alias("JSON");
        let columns = chunk
            .columns()
            .iter()
            .map(|column| {
                if Self::requires_json_cast(column.logical_type()) {
                    let json_column = conn.cast_vector(column, &json, len)?;
                    conn.cast_to_varchar(&json_column, len)
                } else {
                    conn.cast_to_varchar(column, len)
                }
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(StringChunk { columns, len })
    }

    fn render_footer(&mut self, out: &mut dyn PrintStream, _metadata: &ResultMetadata) {
        out.print(if self.json_array { "]\n" } else { "\n" });
    }

    fn null_value(&self) -> &'static [u8] {
        b"null"
    }
}

/// `.mode insert` (`ModeInsertRenderer`).
#[derive(Debug)]
struct ModeInsertRenderer {
    settings: RendererSettings,
}

/// `EscapeNewlines`: a string literal whose newlines, carriage returns and NULs are
/// `chr()` calls in a `concat`.
fn escape_newlines(text: &[u8]) -> ByteString {
    let needs_concat = text.iter().any(|c| matches!(c, b'\n' | b'\r' | 0));
    let mut result = ByteString::from(if needs_concat { "concat('" } else { "'" });
    for &byte in text {
        match byte {
            b'\n' => result.push_bytes("', chr(10), '"),
            b'\r' => result.push_bytes("', chr(13), '"),
            0 => result.push_bytes("', chr(0), '"),
            b'\'' => result.push_bytes("''"),
            _ => result.push(byte),
        }
    }
    result.push(b'\'');
    if needs_concat {
        result.push(b')');
    }
    result
}

impl ShellRenderer for ModeInsertRenderer {
    row_renderer!();
    pager_only_when_on!();

    fn render_row(&mut self, out: &mut dyn PrintStream, metadata: &ResultMetadata, row: &RowData) {
        out.print("INSERT INTO ");
        out.print_bytes(&self.settings.dest_table);
        if self.settings.show_header {
            out.print("(");
            for (i, name) in metadata.column_names.iter().enumerate() {
                if i > 0 {
                    out.print(",");
                }
                out.print_bytes(&rustdb_ast::keyword::write_optionally_quoted_bytes(name));
            }
            out.print(")");
        }
        for (i, value) in row.data.iter().enumerate() {
            out.print(if i > 0 { "," } else { " VALUES(" });
            if row.is_null[i] {
                out.print("NULL");
            } else if metadata.types[i].is_numeric() {
                out.print_bytes(value);
            } else if self.settings.newlines {
                output_quoted_string(out, value);
            } else {
                out.print_bytes(&escape_newlines(value));
            }
        }
        out.print(");\n");
    }
}

/// The `.schema` renderer (`ModeSemiRenderer`).
#[derive(Debug)]
struct ModeSemiRenderer {
    settings: RendererSettings,
}

impl ShellRenderer for ModeSemiRenderer {
    row_renderer!();
    row_pager!();

    fn render_row(&mut self, out: &mut dyn PrintStream, _metadata: &ResultMetadata, row: &RowData) {
        out.print_bytes(&row.data[0]);
        out.print("\n");
    }
}

/// The `.schema --indent` renderer (`ModePrettyRenderer`).
#[derive(Debug)]
struct ModePrettyRenderer {
    settings: RendererSettings,
}

/// `wsToEol`: whether only whitespace and comments remain on the line.
fn ws_to_eol(z: &[u8]) -> bool {
    for (i, &c) in z.iter().enumerate() {
        if c == b'\n' {
            return true;
        }
        if is_space(c) {
            continue;
        }
        return c == b'-' && z.get(i + 1) == Some(&b'-');
    }
    true
}

/// `StringUtil::CharacterIsSpace`.
pub fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

impl ShellRenderer for ModePrettyRenderer {
    row_renderer!();
    row_pager!();

    fn render_row(&mut self, out: &mut dyn PrintStream, _metadata: &ResultMetadata, row: &RowData) {
        let [statement] = row.data.as_slice() else {
            return;
        };
        if statement.starts_with(b"CREATE VIEW") || statement.starts_with(b"CREATE TRIG") {
            out.print_bytes(statement);
            out.print(";\n");
            return;
        }
        // Collapse whitespace, dropping it after `(` and before `(` or `)`.
        let source = statement.as_bytes();
        let mut z: Vec<u8> = Vec::with_capacity(source.len());
        let start = source.iter().take_while(|&&c| is_space(c)).count();
        for &c in &source[start..] {
            if is_space(c) {
                if z.last() == Some(&b'\r') {
                    *z.last_mut().unwrap_or(&mut 0) = b'\n';
                }
                if z.last().is_some_and(|&last| is_space(last) || last == b'(') {
                    continue;
                }
            } else if (c == b'(' || c == b')') && z.last().is_some_and(|&last| is_space(last)) {
                z.pop();
            }
            z.push(c);
        }
        while z.last().is_some_and(|&last| is_space(last)) {
            z.pop();
        }
        if z.len() >= 79 {
            // A long statement: one line per element of the outer parentheses.
            let mut n_paren = 0;
            let mut c_end = 0u8;
            let mut n_line = 0;
            let mut line: Vec<u8> = Vec::new();
            let mut i = 0;
            while i < z.len() {
                let c = z[i];
                if c == c_end {
                    c_end = 0;
                } else if c == b'"' || c == b'\'' || c == b'`' {
                    c_end = c;
                } else if c == b'[' {
                    c_end = b']';
                } else if c == b'-' && z.get(i + 1) == Some(&b'-') {
                    c_end = b'\n';
                } else if c == b'(' {
                    n_paren += 1;
                } else if c == b')' {
                    n_paren -= 1;
                    if n_line > 0 && n_paren == 0 && !line.is_empty() {
                        out.print(&ShellState::schema_line_n(&line, line.len(), "\n"));
                        line.clear();
                    }
                }
                line.push(c);
                if n_paren == 1
                    && c_end == 0
                    && (c == b'(' || c == b'\n' || (c == b',' && !ws_to_eol(&z[i + 1..])))
                {
                    if c == b'\n' {
                        line.pop();
                    }
                    out.print(&ShellState::schema_line_n(&line, line.len(), "\n  "));
                    line.clear();
                    n_line += 1;
                    while z.get(i + 1).is_some_and(|&next| is_space(next)) {
                        i += 1;
                    }
                }
                i += 1;
            }
            z = line;
        }
        out.print(&ShellState::schema_line(&String::from_utf8_lossy(&z), ";\n"));
    }
}

/// Prints the duckbox renderer's pieces, highlighted if results are
/// (`DuckBoxRenderer`).
struct DuckBoxRenderer<'a> {
    out: &'a mut dyn PrintStream,
    highlight: Option<ShellHighlight<'a>>,
}

impl DuckBoxRenderer<'_> {
    fn print_bytes(&mut self, text: &[u8], element_type: HighlightElementType) {
        match &self.highlight {
            Some(highlight) => highlight.print_bytes(text, PrintOutput::Stdout, element_type),
            None => self.out.print_bytes(text),
        }
    }
}

impl BaseResultRenderer for DuckBoxRenderer<'_> {
    fn render_layout(&mut self, text: &[u8]) {
        self.print_bytes(text, HighlightElementType::Layout);
    }

    fn render_column_name(&mut self, text: &[u8]) {
        self.print_bytes(text, HighlightElementType::ColumnName);
    }

    fn render_type(&mut self, text: &[u8]) {
        self.print_bytes(text, HighlightElementType::ColumnType);
    }

    fn render_value(&mut self, text: &[u8], ty: &LogicalType) {
        let element = if ty.is_numeric() {
            HighlightElementType::NumericValue
        } else if is_temporal(ty) {
            HighlightElementType::TemporalValue
        } else {
            HighlightElementType::StringValue
        };
        self.print_bytes(text, element);
    }

    fn render_string_literal(&mut self, text: &[u8], _ty: &LogicalType) {
        self.print_bytes(text, HighlightElementType::StringConstant);
    }

    fn render_null(&mut self, text: &[u8], _ty: &LogicalType) {
        self.print_bytes(text, HighlightElementType::NullValue);
    }

    fn render_footer(&mut self, text: &[u8]) {
        self.print_bytes(text, HighlightElementType::Footer);
    }
}

/// `.mode duckbox` (`ModeDuckBoxRenderer`).
#[derive(Debug)]
struct ModeDuckBoxRenderer {
    settings: RendererSettings,
    config: BoxRendererConfig,
    render_state: Option<BoxRendererState>,
    error_str: String,
}

impl ModeDuckBoxRenderer {
    fn new(state: &ShellState) -> Self {
        let mut config = BoxRendererConfig {
            max_rows: state.max_rows,
            max_width: state.max_width,
            ..BoxRendererConfig::default()
        };
        if state.max_analyze_rows == 0 {
            // Automatic: 100K rows on a terminal.
            if state.stdout_is_console {
                config.max_analyze_rows = 100_000;
            }
        } else {
            config.max_analyze_rows = state.max_analyze_rows;
        }
        if config.max_width == 0 {
            // Automatic: unlimited in files.
            if !state.outfile.is_empty() && !state.outfile.starts_with(b"|") {
                config.max_rows = usize::MAX;
                config.max_width = usize::MAX;
            }
            if !state.stdout_is_console {
                config.max_width = usize::MAX;
            }
        }
        let large_rendering = match state.large_number_rendering {
            LargeNumberRendering::Default if state.stdout_is_console => LargeNumberRendering::Footer,
            LargeNumberRendering::Default => LargeNumberRendering::None,
            rendering => rendering,
        };
        config.null_value.clone_from(&state.null_value);
        if state.columns {
            config.render_mode = BoxRenderMode::Columns;
        }
        config.decimal_separator = state.decimal_separator;
        config.thousand_separator = state.thousand_separator;
        config.large_number_rendering = match large_rendering {
            LargeNumberRendering::Footer => box_renderer::LargeNumberRendering::Footer,
            LargeNumberRendering::All => box_renderer::LargeNumberRendering::All,
            LargeNumberRendering::None | LargeNumberRendering::Default => {
                box_renderer::LargeNumberRendering::None
            }
        };
        "use .last to show entire result".clone_into(&mut config.hidden_rows_hint);
        Self {
            settings: RendererSettings::new(state),
            config,
            render_state: None,
            error_str: String::new(),
        }
    }
}

impl ShellRenderer for ModeDuckBoxRenderer {
    fn settings(&self) -> &RendererSettings {
        &self.settings
    }

    fn settings_mut(&mut self) -> &mut RendererSettings {
        &mut self.settings
    }

    fn remove_render_limits(&mut self) {
        self.config.max_rows = usize::MAX;
        self.config.max_width = usize::MAX;
    }

    fn analyze(
        &mut self,
        _state: &mut ShellState,
        result: &mut RenderingQueryResult,
        conn: &mut Connection,
    ) -> Result<()> {
        let ResultSource::Materialized(scanned) = &result.source else {
            return Err(rustdb::Error::internal(
                "duckbox rendering needs a materialized result",
            ));
        };
        match BoxRendererState::prepare(
            self.config.clone(),
            conn,
            &result.metadata.column_names,
            Arc::clone(&scanned.result),
        ) {
            Ok(render_state) => self.render_state = Some(render_state),
            // The error shows when rendering.
            Err(error) => self.error_str = error.to_string(),
        }
        Ok(())
    }

    fn should_use_pager(
        &mut self,
        _state: &mut ShellState,
        result: &mut RenderingQueryResult,
        _conn: &mut Connection,
        global_mode: PagerMode,
    ) -> Result<bool> {
        if global_mode == PagerMode::On {
            return Ok(true);
        }
        // A set `.maxwidth` pages results wider than the terminal.
        if self.config.max_width != 0
            && self.render_state.as_ref().is_some_and(|render_state| {
                render_state.total_render_width() > self.settings.max_render_width
            })
        {
            return Ok(true);
        }
        // Results are cut to `.maxrows`; page those longer than the pager's row count.
        let row_count = match &result.source {
            ResultSource::Materialized(scanned) => scanned.result.row_count(),
            ResultSource::Stream(_) => 0,
        };
        Ok(self.config.max_rows >= self.settings.pager_min_rows && row_count >= self.settings.pager_min_rows)
    }

    fn render_query_result(
        &mut self,
        out: &mut dyn PrintStream,
        state: &mut ShellState,
        _result: &mut RenderingQueryResult,
        conn: &mut Connection,
    ) -> Result<SuccessState> {
        let highlight = state.highlight_results() && out.supports_highlight();
        let shell_out = state.out.clone();
        let mut renderer = DuckBoxRenderer {
            out,
            highlight: highlight.then(|| ShellHighlight::new(&shell_out)),
        };
        let rendered = if self.error_str.is_empty() {
            match &mut self.render_state {
                Some(render_state) => render_state
                    .render(&mut renderer, conn)
                    .map_err(|error| error.to_string()),
                None => Ok(()),
            }
        } else {
            Err(self.error_str.clone())
        };
        match rendered {
            Ok(()) => Ok(SuccessState::Success),
            Err(error) => {
                renderer.render_layout(&rustdb_common::byte_format!("{}\n", error.as_bytes()));
                Ok(SuccessState::Failure)
            }
        }
    }

    fn require_materialized_result(&self) -> bool {
        true
    }
}

/// The table of a `DESCRIBE` (`ModeDescribeRenderer`).
#[derive(Debug)]
struct ModeDescribeRenderer {
    settings: RendererSettings,
}

impl ShellRenderer for ModeDescribeRenderer {
    fn settings(&self) -> &RendererSettings {
        &self.settings
    }

    fn settings_mut(&mut self) -> &mut RendererSettings {
        &mut self.settings
    }

    fn render_query_result(
        &mut self,
        _out: &mut dyn PrintStream,
        state: &mut ShellState,
        result: &mut RenderingQueryResult,
        _conn: &mut Connection,
    ) -> Result<SuccessState> {
        let ResultSource::Materialized(scanned) = &result.source else {
            return Err(rustdb::Error::internal(
                "DESCRIBE rendering needs a materialized result",
            ));
        };
        let mut table = ShellTableInfo {
            table_name: state.describe_table_name.clone(),
            ..ShellTableInfo::default()
        };
        // The rows of the collection, as `for (auto &row : res.result)` scans them.
        while let Some(chunk) = scanned.fetch()? {
            let value = |column: usize, row: usize| -> Option<String> {
                let value = chunk.column(column).get_value(row);
                (!value.is_null()).then(|| value.to_varchar().unwrap_or_default())
            };
            for row in 0..chunk.len() {
                let mut column = ShellColumnInfo {
                    column_name: value(0, row).unwrap_or_default().into(),
                    column_type: value(1, row).unwrap_or_default(),
                    ..ShellColumnInfo::default()
                };
                if let Some(null) = value(2, row) {
                    column.is_not_null = null == "NO";
                }
                if let Some(key) = value(3, row) {
                    column.is_primary_key = key == "PRI";
                    column.is_unique = key == "UNI";
                }
                if let Some(default_value) = value(4, row) {
                    column.default_value = default_value;
                }
                table.columns.push(column);
            }
        }
        state.render_table_metadata(vec![table]);
        Ok(SuccessState::Success)
    }

    fn require_materialized_result(&self) -> bool {
        true
    }

    fn should_use_pager(
        &mut self,
        _state: &mut ShellState,
        _result: &mut RenderingQueryResult,
        _conn: &mut Connection,
        _global_mode: PagerMode,
    ) -> Result<bool> {
        // `RenderTableMetadata` decides.
        Ok(false)
    }
}

/// `.mode trash` (`ModeTrashRenderer`).
#[derive(Debug)]
struct ModeTrashRenderer {
    settings: RendererSettings,
}

impl ShellRenderer for ModeTrashRenderer {
    fn settings(&self) -> &RendererSettings {
        &self.settings
    }

    fn settings_mut(&mut self) -> &mut RendererSettings {
        &mut self.settings
    }

    fn render_query_result(
        &mut self,
        _out: &mut dyn PrintStream,
        _state: &mut ShellState,
        _result: &mut RenderingQueryResult,
        _conn: &mut Connection,
    ) -> Result<SuccessState> {
        Ok(SuccessState::Success)
    }

    fn require_materialized_result(&self) -> bool {
        true
    }

    fn should_use_pager(
        &mut self,
        _state: &mut ShellState,
        _result: &mut RenderingQueryResult,
        _conn: &mut Connection,
        _global_mode: PagerMode,
    ) -> Result<bool> {
        Ok(false)
    }
}

/// `ShellState::GetRenderer(mode)`.
pub fn renderer_for(state: &ShellState, mode: RenderMode) -> Box<dyn ShellRenderer> {
    let settings = || RendererSettings::new(state);
    match mode {
        RenderMode::Line => Box::new(ModeLineRenderer {
            settings: settings(),
            header_width: 0,
        }),
        RenderMode::Explain => Box::new(ModeExplainRenderer { settings: settings() }),
        RenderMode::List => Box::new(ModeListRenderer { settings: settings() }),
        RenderMode::Html => Box::new(ModeHtmlRenderer { settings: settings() }),
        RenderMode::Tcl => Box::new(ModeTclRenderer { settings: settings() }),
        RenderMode::Csv => Box::new(ModeCsvRenderer { settings: settings() }),
        RenderMode::Ascii => {
            let mut settings = settings();
            settings.col_sep = "\n".into();
            settings.row_sep = "\n".into();
            Box::new(ModeAsciiRenderer { settings })
        }
        RenderMode::Quote => Box::new(ModeQuoteRenderer { settings: settings() }),
        RenderMode::Json => Box::new(ModeJsonRenderer {
            settings: settings(),
            json_array: true,
        }),
        RenderMode::JsonLines => Box::new(ModeJsonRenderer {
            settings: settings(),
            json_array: false,
        }),
        RenderMode::Insert => Box::new(ModeInsertRenderer { settings: settings() }),
        RenderMode::Semi => Box::new(ModeSemiRenderer { settings: settings() }),
        RenderMode::Pretty => Box::new(ModePrettyRenderer { settings: settings() }),
        RenderMode::Column => Box::new(ModeColumnRenderer(ColumnRendererState::new(state))),
        RenderMode::Table => Box::new(ModeTableRenderer(ColumnRendererState::new(state))),
        RenderMode::Markdown => Box::new(ModeMarkdownRenderer(ColumnRendererState::new(state))),
        RenderMode::Box => Box::new(ModeBoxRenderer(ColumnRendererState::new(state))),
        RenderMode::Latex => Box::new(ModeLatexRenderer(ColumnRendererState::new(state))),
        RenderMode::Duckbox => Box::new(ModeDuckBoxRenderer::new(state)),
        RenderMode::Describe => Box::new(ModeDescribeRenderer { settings: settings() }),
        RenderMode::Trash => Box::new(ModeTrashRenderer { settings: settings() }),
    }
}
