//! The duckbox renderer (DuckDB's `src/common/box_renderer.cpp`): a query result as a
//! table of unicode boxes, shortened to the terminal width and to `.maxrows` rows, with a
//! footer that counts the rows and columns.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use rustdb::{LogicalType, LogicalTypeId, MaterializedResult, Result};

use crate::printer;
use crate::render_context::RenderContext;
use crate::utf8::{char_render_width, next_grapheme_cluster, render_width};

/// How a value is aligned in its column (`ValueRenderAlignment`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueRenderAlignment {
    Left,
    Middle,
    Right,
}

/// Whether results render one row per line or one column per line
/// (DuckDB's `RenderMode` of the box renderer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxRenderMode {
    Rows,
    Columns,
}

/// What a piece of rendered text is, for highlighting (`ResultRenderType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultRenderType {
    Layout,
    ColumnName,
    ColumnType,
    Value,
    NullValue,
    Footer,
    StringLiteral,
}

/// How large numbers are summarized (`LargeNumberRendering`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LargeNumberRendering {
    /// Numbers render as they are.
    None,
    /// A single-row result gets a second row with readable summaries of large numbers.
    Footer,
    /// Every large number renders as its summary.
    All,
}

/// Where the rendered pieces go (`BaseResultRenderer`).
pub trait BaseResultRenderer {
    fn render_layout(&mut self, text: &str);
    fn render_column_name(&mut self, text: &str);
    fn render_type(&mut self, text: &str);
    fn render_value(&mut self, text: &str, ty: &LogicalType);
    fn render_null(&mut self, text: &str, ty: &LogicalType);
    fn render_string_literal(&mut self, text: &str, ty: &LogicalType) {
        self.render_value(text, ty);
    }
    fn render_footer(&mut self, text: &str);
}

/// A [`BaseResultRenderer`] with the result's column types, so that values render with
/// the type of the column being rendered (`BaseResultRenderer::Render`,
/// `SetResultTypes`, `SetValueColumn`).
struct ResultRenderer<'a> {
    renderer: &'a mut dyn BaseResultRenderer,
    column_types: Vec<LogicalType>,
    column_idx: Option<usize>,
}

impl ResultRenderer<'_> {
    fn layout(&mut self, text: &str) {
        self.renderer.render_layout(text);
    }

    /// The type of the column being rendered, `INVALID` outside a column
    /// (`invalid_type`).
    fn value_type(&self) -> LogicalType {
        self.column_idx
            .map_or(LogicalType::INVALID, |column| self.column_types[column].clone())
    }

    fn render(&mut self, render_mode: ResultRenderType, value: &str) {
        let ty = self.value_type();
        match render_mode {
            ResultRenderType::Layout => self.renderer.render_layout(value),
            ResultRenderType::ColumnName => self.renderer.render_column_name(value),
            ResultRenderType::ColumnType => self.renderer.render_type(value),
            ResultRenderType::Value => self.renderer.render_value(value, &ty),
            ResultRenderType::NullValue => self.renderer.render_null(value, &ty),
            ResultRenderType::StringLiteral => self.renderer.render_string_literal(value, &ty),
            ResultRenderType::Footer => self.renderer.render_footer(value),
        }
    }
}

/// The box renderer's options (`BoxRendererConfig`).
#[derive(Clone, Debug)]
pub struct BoxRendererConfig {
    /// The maximum width; 0 is the terminal width.
    pub max_width: usize,
    /// The maximum number of rows to render.
    pub max_rows: usize,
    /// The maximum number of rows analyzed to size the columns.
    pub max_analyze_rows: usize,
    /// The limit applied before rendering: rendering exactly this many rows shows `?`.
    pub limit: usize,
    /// The width a column shrinks to when the result does not fit.
    pub max_col_width: usize,
    /// How NULL renders.
    pub null_value: String,
    pub decimal_separator: Option<u8>,
    pub thousand_separator: Option<u8>,
    pub render_mode: BoxRenderMode,
    pub large_number_rendering: LargeNumberRendering,
    /// The hint shown in the footer when rows or columns are hidden.
    pub hidden_rows_hint: String,
}

impl Default for BoxRendererConfig {
    fn default() -> Self {
        Self {
            max_width: 0,
            max_rows: 20,
            max_analyze_rows: usize::MAX,
            limit: 0,
            max_col_width: 20,
            null_value: "NULL".to_owned(),
            decimal_separator: None,
            thousand_separator: None,
            render_mode: BoxRenderMode::Rows,
            large_number_rendering: LargeNumberRendering::None,
            hidden_rows_hint: String::new(),
        }
    }
}

pub const LTCORNER: &str = "┌";
pub const RTCORNER: &str = "┐";
pub const LDCORNER: &str = "└";
pub const RDCORNER: &str = "┘";
const MIDDLE: &str = "┼";
const TMIDDLE: &str = "┬";
const LMIDDLE: &str = "├";
const RMIDDLE: &str = "┤";
const DMIDDLE: &str = "┴";
pub const VERTICAL: &str = "│";
pub const HORIZONTAL: &str = "─";
pub const DOTDOTDOT: &str = "…";
const DOT: &str = "·";
pub const DOTDOTDOT_LENGTH: usize = 1;

/// Where highlighting switches within a value (`HighlightingAnnotation`).
#[derive(Clone, Copy, Debug)]
struct HighlightingAnnotation {
    render_mode: ResultRenderType,
    start: usize,
}

/// A value placed in a column (`BoxRenderValue`).
#[derive(Clone, Debug)]
struct BoxRenderValue {
    text: String,
    render_mode: ResultRenderType,
    annotations: Vec<HighlightingAnnotation>,
    alignment: ValueRenderAlignment,
    column_idx: Option<usize>,
    render_width: Option<usize>,
    decomposed: bool,
}

impl BoxRenderValue {
    fn new(
        text: String,
        render_mode: ResultRenderType,
        alignment: ValueRenderAlignment,
        column_idx: Option<usize>,
        render_width: Option<usize>,
    ) -> Self {
        Self {
            text,
            render_mode,
            annotations: Vec::new(),
            alignment,
            column_idx,
            render_width,
            decomposed: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RenderRowType {
    RowValues,
    Separator,
}

/// The texts of the footer (`BoxRendererFooter`).
#[derive(Debug, Default)]
struct BoxRendererFooter {
    row_count_str: String,
    readable_rows_str: String,
    shown_str: String,
    column_count_str: String,
    render_length: usize,
    must_show_footer: bool,
    has_hidden_rows: bool,
    has_hidden_columns: bool,
}

#[derive(Clone, Debug)]
struct BoxRenderRow {
    row_type: RenderRowType,
    values: Vec<BoxRenderValue>,
}

impl BoxRenderRow {
    fn new(row_type: RenderRowType) -> Self {
        Self {
            row_type,
            values: Vec::new(),
        }
    }
}

/// A rendered value (`NULL` as `None`) and its render width: a row of DuckDB's
/// `RenderDataCollection`, which keeps both per column.
#[derive(Clone, Debug)]
struct RenderCell {
    text: Option<String>,
    render_length: usize,
}

/// Rows of rendered values (`RenderDataCollection`).
type RenderDataCollection = Vec<Vec<RenderCell>>;

/// A result prepared for rendering (`BoxRendererImplementation`): the rows to show are
/// fetched and converted, the column widths fixed.
#[derive(Debug)]
pub struct BoxRendererState {
    config: BoxRendererConfig,
    column_names: Vec<String>,
    result_types: Vec<LogicalType>,
    result: Arc<MaterializedResult>,
    column_widths: Vec<usize>,
    column_boundary_positions: Vec<usize>,
    total_render_length: usize,
    header_rows: Vec<BoxRenderRow>,
    footer: BoxRendererFooter,
    pruned_columns: HashSet<usize>,
    column_map: Vec<Option<usize>>,
    expand_rows: bool,
    is_first_row: bool,
    max_rows_per_row: usize,
    render_collections: Vec<RenderDataCollection>,
    top_rows: usize,
    bottom_rows: usize,
    /// Where scanning resumes if the result was not exhausted: the chunk and the row.
    resume_position: Option<(usize, usize)>,
}

/// The column count of a result's types.
fn original_types(result: &MaterializedResult) -> &[LogicalType] {
    result.types()
}

impl BoxRendererState {
    /// `BoxRenderer::Prepare`: fetches the rows to render and computes the layout.
    pub fn prepare(
        config: BoxRendererConfig,
        context: &dyn RenderContext,
        names: &[String],
        result: Arc<MaterializedResult>,
    ) -> Result<Self> {
        let mut state = Self {
            config,
            column_names: names.to_vec(),
            result_types: result.types().to_vec(),
            result,
            column_widths: Vec::new(),
            column_boundary_positions: Vec::new(),
            total_render_length: 0,
            header_rows: Vec::new(),
            footer: BoxRendererFooter::default(),
            pruned_columns: HashSet::new(),
            column_map: Vec::new(),
            expand_rows: false,
            is_first_row: true,
            max_rows_per_row: 1,
            render_collections: Vec::new(),
            top_rows: 0,
            bottom_rows: 0,
            resume_position: None,
        };
        state.initialize(context)?;
        Ok(state)
    }

    /// The width of the rendered table (`TotalRenderWidth`).
    pub fn total_render_width(&self) -> usize {
        self.total_render_length
    }

    fn column_count(&self) -> usize {
        self.result.column_count()
    }

    /// `ComputeRowFooter`.
    fn compute_row_footer(&mut self, row_count: usize, rendered_rows: usize) {
        let column_count = self.column_count();
        self.footer.column_count_str = format!("{column_count} column");
        if column_count > 1 {
            self.footer.column_count_str.push('s');
        }
        self.footer.row_count_str = format!("{} rows", self.format_number(&row_count.to_string()));
        let has_limited_rows = self.config.limit > 0 && row_count == self.config.limit;
        if has_limited_rows {
            self.footer.row_count_str = "? rows".to_owned();
        }
        if self.config.large_number_rendering == LargeNumberRendering::Footer && !has_limited_rows {
            let readable_str = self.try_format_large_number(&row_count.to_string());
            if !readable_str.is_empty() {
                self.footer.readable_rows_str = format!("{row_count} total");
                self.footer.row_count_str = format!("{readable_str} rows");
            }
        }
        self.footer.has_hidden_rows = rendered_rows < row_count;
        if self.footer.has_hidden_rows {
            if has_limited_rows {
                let limit = self.format_number(&(self.config.limit - 1).to_string());
                self.footer.shown_str += &format!(">{limit} rows, ");
            }
            let shown = self.format_number(&rendered_rows.to_string());
            self.footer.shown_str += &format!("{shown} shown");
        }
        self.footer.must_show_footer = has_limited_rows || self.footer.has_hidden_rows || row_count == 0;
        self.footer.render_length = self
            .footer
            .row_count_str
            .len()
            .max(self.footer.shown_str.len() + 2)
            .max(self.footer.readable_rows_str.len() + 2)
            + 4;
    }

    /// `UpdateColumnCountFooter`.
    fn update_column_count_footer(&mut self, column_count: usize) {
        if self.pruned_columns.is_empty() {
            return;
        }
        if self.config.render_mode == BoxRenderMode::Columns {
            // In columns mode the pruned columns are pruned rows.
            self.footer.has_hidden_rows = true;
            let shown_row_count = column_count - self.pruned_columns.len();
            self.footer.shown_str = format!("{} shown", shown_row_count - 2);
        } else {
            self.footer.has_hidden_columns = true;
            let shown_column_count = column_count - self.pruned_columns.len();
            self.footer.column_count_str += &format!(" ({shown_column_count} shown)");
        }
    }

    /// `Initialize`.
    fn initialize(&mut self, context: &dyn RenderContext) -> Result<()> {
        let mut max_width = self.config.max_width;
        if max_width == 0 {
            max_width = if printer::stdout_is_terminal() {
                printer::terminal_width()
            } else {
                120
            };
        }
        // Widths under 80 are not supported.
        max_width = max_width.max(80);

        // Which rows to render.
        let row_count = self.result.row_count();
        let mut rows_to_render = row_count.min(self.config.max_rows);
        if row_count <= self.config.max_rows.saturating_add(3) {
            // Hiding rows adds three lines: hiding just a few rows makes no sense.
            rows_to_render = row_count;
        }
        if rows_to_render == row_count {
            self.top_rows = row_count;
            self.bottom_rows = 0;
        } else {
            self.top_rows = rows_to_render / 2 + usize::from(rows_to_render % 2 != 0);
            self.bottom_rows = rows_to_render - self.top_rows;
        }
        self.compute_row_footer(row_count, self.top_rows + self.bottom_rows);

        let collections = self.fetch_render_collections(context)?;
        self.render_collections = collections;
        if self.config.render_mode == BoxRenderMode::Columns && rows_to_render > 0 {
            let collections = std::mem::take(&mut self.render_collections);
            self.render_collections = self.pivot_collections(collections, row_count, context);
        }

        // The width of every column: its name, its type and its values.
        let min_width = if self.footer.must_show_footer {
            self.footer.render_length
        } else {
            0
        };
        self.compute_render_widths(min_width, max_width);

        // The boundaries of the columns.
        for c in 0..self.column_widths.len() {
            let render_boundary = if c == 0 {
                self.column_widths[c] + 2
            } else {
                self.column_boundary_positions[c - 1] + self.column_widths[c] + 3
            };
            self.column_boundary_positions.push(render_boundary);
        }
        Ok(())
    }

    /// `Render`.
    pub fn render(
        &mut self,
        renderer: &mut dyn BaseResultRenderer,
        context: &dyn RenderContext,
    ) -> Result<()> {
        let mut ss = ResultRenderer {
            renderer,
            column_types: original_types(&self.result).to_vec(),
            column_idx: None,
        };
        self.render_header(&mut ss, context);
        loop {
            let mut collections = std::mem::take(&mut self.render_collections);
            self.render_values(&mut ss, &mut collections, context);
            let Some((chunk_idx, row_idx)) = self.resume_position else {
                break;
            };
            // More rows to fetch.
            let mut top = RenderDataCollection::new();
            self.fetch_top_collection(&mut top, chunk_idx, row_idx, context)?;
            self.render_collections = vec![top, RenderDataCollection::new()];
        }
        let column_count = self.result_types.len();
        self.render_footer(&mut ss, self.result.row_count(), column_count);
        Ok(())
    }

    /// `RenderValue`.
    fn render_value(
        &self,
        ss: &mut ResultRenderer<'_>,
        value: &str,
        column_width: usize,
        render_mode: ResultRenderType,
        annotations: &[HighlightingAnnotation],
        alignment: ValueRenderAlignment,
        render_width_input: Option<usize>,
        vertical: Option<&str>,
    ) {
        let mut render_width_value = render_width_input.unwrap_or_else(|| render_width(value));
        let mut render_value = value;
        let small_value;
        let mut max_render_pos = value.len();
        if render_width_value > column_width {
            // The value does not fit: the column was shortened; render what fits.
            let mut pos = 0;
            let mut current_render_width = DOTDOTDOT_LENGTH;
            let mut truncated = truncate_value(value, column_width, &mut pos, &mut current_render_width);
            max_render_pos = truncated.len();
            truncated.push_str(DOTDOTDOT);
            render_width_value = current_render_width;
            small_value = truncated;
            render_value = &small_value;
        }
        let padding_count = (column_width - render_width_value) + 2;
        let (lpadding, rpadding) = match alignment {
            ValueRenderAlignment::Left => (1, padding_count - 1),
            ValueRenderAlignment::Middle => (padding_count / 2, padding_count - padding_count / 2),
            ValueRenderAlignment::Right => (padding_count - 1, 1),
        };
        ss.layout(vertical.unwrap_or(VERTICAL));
        ss.layout(&" ".repeat(lpadding));
        if annotations.is_empty() {
            ss.render(render_mode, render_value);
        } else {
            // Render the pieces between the annotations.
            let mut pos = 0;
            let mut active_render_mode = render_mode;
            for annotation in annotations {
                if annotation.start >= max_render_pos {
                    break;
                }
                let render_end = max_render_pos.min(annotation.start);
                ss.render(active_render_mode, &render_value[pos..render_end]);
                active_render_mode = annotation.render_mode;
                pos = render_end;
            }
            if pos < render_value.len() {
                ss.render(active_render_mode, &render_value[pos..]);
            }
        }
        ss.layout(&" ".repeat(rpadding));
    }

    /// `TryFormatLargeNumber` with the configured decimal separator.
    fn try_format_large_number(&self, numeric: &str) -> String {
        try_format_large_number(numeric, self.config.decimal_separator)
    }

    /// `FetchRenderCollections`: the top rows and the bottom rows.
    fn fetch_render_collections(&mut self, context: &dyn RenderContext) -> Result<Vec<RenderDataCollection>> {
        if self.config.large_number_rendering == LargeNumberRendering::Footer
            && (self.config.render_mode != BoxRenderMode::Rows || self.result.row_count() != 1)
        {
            // The large number footer needs a single row in ROWS mode.
            self.config.large_number_rendering = LargeNumberRendering::None;
        }
        let mut top = RenderDataCollection::new();
        let mut bottom = RenderDataCollection::new();
        self.fetch_top_collection(&mut top, 0, 0, context)?;
        self.fetch_bottom_collection(&mut bottom, context)?;
        Ok(vec![top, bottom])
    }

    /// The rendered values of the first `count` rows of `chunk` from row `offset`
    /// (`VectorOperations::Cast` to VARCHAR followed by `ConvertRenderVector`).
    fn convert_chunk(
        &self,
        context: &dyn RenderContext,
        chunk: &rustdb::DataChunk,
        offset: usize,
        count: usize,
    ) -> Result<Vec<Vec<RenderCell>>> {
        let null_render_length = render_width(&self.config.null_value);
        let mut rows: Vec<Vec<RenderCell>> = (0..count)
            .map(|_| Vec::with_capacity(chunk.column_count()))
            .collect();
        for c in 0..chunk.column_count() {
            let source = chunk.column(c);
            let strings = context.cast_to_varchar(source, offset + count)?;
            for (row, value) in rows.iter_mut().zip(strings.into_iter().skip(offset)) {
                row.push(match value {
                    None => RenderCell {
                        text: None,
                        render_length: null_render_length,
                    },
                    Some(text) => {
                        let text = self.convert_render_value_typed(&text, source.logical_type());
                        RenderCell {
                            render_length: render_width(&text),
                            text: Some(text),
                        }
                    }
                });
            }
        }
        Ok(rows)
    }

    /// `FetchTopCollection`.
    fn fetch_top_collection(
        &mut self,
        top_collection: &mut RenderDataCollection,
        mut chunk_idx: usize,
        mut row_idx: usize,
        context: &dyn RenderContext,
    ) -> Result<()> {
        let column_count = self.column_count();
        self.resume_position = None;
        let result = Arc::clone(&self.result);
        let chunks = result.chunks();
        while row_idx < self.top_rows {
            if context.is_interrupted() || chunk_idx >= chunks.len() {
                break;
            }
            let chunk = &chunks[chunk_idx];
            let insert_count = chunk.len().min(self.top_rows - row_idx);
            let rows = self.convert_chunk(context, chunk, 0, insert_count)?;
            top_collection.extend(rows);

            if self.config.large_number_rendering == LargeNumberRendering::Footer {
                // A second row with readable renderings of the values ("1.23 million").
                let mut readable_numbers = Vec::with_capacity(column_count);
                let mut all_readable = true;
                for c in 0..column_count {
                    if !result.types()[c].is_numeric() {
                        all_readable = false;
                        break;
                    }
                    let numeric_val = top_collection
                        .last()
                        .and_then(|row| row[c].text.clone())
                        .unwrap_or_else(|| "NULL".to_owned());
                    let readable = self.try_format_large_number(&numeric_val);
                    if readable.is_empty() {
                        all_readable = false;
                        break;
                    }
                    readable_numbers.push(format!("({readable})"));
                }
                if all_readable {
                    top_collection.push(
                        readable_numbers
                            .into_iter()
                            .map(|text| RenderCell {
                                render_length: render_width(&text),
                                text: Some(text),
                            })
                            .collect(),
                    );
                } else {
                    self.config.large_number_rendering = LargeNumberRendering::None;
                }
            }

            chunk_idx += 1;
            row_idx += chunk.len();
            if self.bottom_rows == 0
                && row_idx >= self.config.max_analyze_rows
                && self.config.render_mode == BoxRenderMode::Rows
            {
                // Stop fetching for now: render these rows and resume from here.
                self.resume_position = Some((chunk_idx, row_idx));
                break;
            }
        }
        Ok(())
    }

    /// `FetchBottomCollection`.
    fn fetch_bottom_collection(
        &self,
        bottom_collection: &mut RenderDataCollection,
        context: &dyn RenderContext,
    ) -> Result<()> {
        if self.bottom_rows == 0 {
            return Ok(());
        }
        let chunks = self.result.chunks();
        // The chunks holding the bottom rows, from the last one backwards.
        let mut fetched_row_count = 0;
        let mut first_chunk = chunks.len();
        while fetched_row_count < self.bottom_rows && first_chunk > 0 {
            first_chunk -= 1;
            fetched_row_count += chunks[first_chunk].len();
        }
        if fetched_row_count < self.bottom_rows {
            return Err(rustdb::Error::internal("Failed to fetch enough rows"));
        }
        for (i, chunk) in chunks[first_chunk..].iter().enumerate() {
            // Skip the rows of the first chunk above the bottom rows.
            let offset = if i == 0 {
                fetched_row_count - self.bottom_rows
            } else {
                0
            };
            let rows = self.convert_chunk(context, chunk, offset, chunk.len() - offset)?;
            bottom_collection.extend(rows);
        }
        Ok(())
    }

    /// `PivotCollections`: in columns mode every result column becomes a row of its
    /// name, its type and its values.
    fn pivot_collections(
        &mut self,
        input: Vec<RenderDataCollection>,
        row_count: usize,
        context: &dyn RenderContext,
    ) -> Vec<RenderDataCollection> {
        let top_count = input[0].len();
        let bottom_count = input[1].len();
        let mut new_names = vec!["Column".to_owned(), "Type".to_owned()];
        let mut new_types = vec![LogicalType::VARCHAR, LogicalType::VARCHAR];
        for r in 0..top_count {
            new_names.push(format!("Row {}", r + 1));
            new_types.push(LogicalType::VARCHAR);
        }
        for r in 0..bottom_count {
            new_names.push(format!("Row {}", row_count - bottom_count + r + 1));
            new_types.push(LogicalType::VARCHAR);
        }
        let mut result = RenderDataCollection::new();
        for (c, column_name) in self.column_names.iter().enumerate() {
            if context.is_interrupted() {
                break;
            }
            let type_name = render_type(&self.result_types[c]);
            let mut row = vec![
                RenderCell {
                    render_length: render_width(column_name),
                    text: Some(column_name.clone()),
                },
                RenderCell {
                    render_length: render_width(&type_name),
                    text: Some(type_name),
                },
            ];
            for collection in &input {
                row.extend(collection.iter().map(|values| values[c].clone()));
            }
            result.push(row);
        }
        self.column_names = new_names;
        self.result_types = new_types;
        vec![result]
    }

    /// `ConvertRenderValue(const string &)`: control characters as escapes.
    fn convert_render_value(input: &str) -> String {
        let mut result = String::with_capacity(input.len());
        for c in input.chars() {
            let byte_value = c as u32;
            if byte_value >= 32 {
                result.push(c);
                continue;
            }
            result.push('\\');
            match byte_value {
                7 => result.push('a'),
                8 => result.push('b'),
                9 => result.push('t'),
                10 => result.push('n'),
                11 => result.push('v'),
                12 => result.push('f'),
                13 => result.push('r'),
                27 => result.push('e'),
                _ => result.push_str(&byte_value.to_string()),
            }
        }
        result
    }

    /// `FormatNumber`: the separators of the configuration, or the readable rendering of
    /// all large numbers.
    fn format_number(&self, input: &str) -> String {
        if self.config.large_number_rendering == LargeNumberRendering::All {
            let number = self.try_format_large_number(input);
            if !number.is_empty() {
                return number;
            }
        }
        if self.config.decimal_separator.is_none() && self.config.thousand_separator.is_none() {
            return input.to_owned();
        }
        let bytes = input.as_bytes();
        // The digits before any decimal point.
        let character_count = bytes.iter().take_while(|c| c.is_ascii_digit()).count();
        // The position of the first thousand separator.
        let mut separator_position = if character_count % 3 == 0 {
            3
        } else {
            character_count % 3
        };
        let mut result = Vec::with_capacity(bytes.len() + character_count / 3);
        for (c, &byte) in bytes.iter().enumerate().take(character_count) {
            if c == separator_position
                && let Some(separator) = self.config.thousand_separator
            {
                result.push(separator);
                separator_position += 3;
            }
            result.push(byte);
        }
        for &byte in &bytes[character_count..] {
            match self.config.decimal_separator {
                Some(separator) if byte == b'.' => result.push(separator),
                _ => result.push(byte),
            }
        }
        String::from_utf8_lossy(&result).into_owned()
    }

    /// `ConvertRenderValue(const string &, const LogicalType &)`.
    fn convert_render_value_typed(&self, input: &str, ty: &LogicalType) -> String {
        if is_numeric_render_type(ty) {
            self.format_number(input)
        } else {
            Self::convert_render_value(input)
        }
    }

    fn can_pretty_print(&self, render_value: &BoxRenderValue) -> bool {
        render_value
            .column_idx
            .is_some_and(|column| is_json_or_nested(&original_types(&self.result)[column]))
    }

    /// `PrettyPrintValue`.
    fn pretty_print_value(&self, render_value: &mut BoxRenderValue, max_rows: usize, max_width: usize) {
        if !self.can_pretty_print(render_value) {
            return;
        }
        JsonFormatter::format_value(render_value, max_rows, max_width);
    }

    /// `HighlightValue`.
    fn highlight_value(&self, render_value: &mut BoxRenderValue) {
        if !self.can_pretty_print(render_value) {
            return;
        }
        let text = render_value.text.clone();
        JsonHighlighter::highlight(render_value, &text);
    }

    /// `PotentiallyExpandRow`: a value too wide for its column continues on extra rows.
    fn potentially_expand_row(
        &self,
        mut row: BoxRenderRow,
        rows: &mut Vec<BoxRenderRow>,
        is_first_row: bool,
    ) {
        let max_rows_per_row = self.max_rows_per_row;
        let mut extra_rows: Vec<BoxRenderRow> = Vec::new();
        for c in 0..row.values.len() {
            if self.can_pretty_print(&row.values[c]) {
                self.pretty_print_value(&mut row.values[c], max_rows_per_row, self.column_widths[c]);
                self.highlight_value(&mut row.values[c]);
                row.values[c].render_width = Some(self.column_widths[c] + 1);
            }
            let render_width_value = row.values[c].render_width.unwrap_or(0);
            if render_width_value <= self.column_widths[c] {
                // Not shortened.
                continue;
            }
            // The value was shortened: stretch it over extra rows, starting with what fits
            // on the first row.
            let mut current_row = 0;
            let mut current_pos = 0;
            let mut current_render_width = 0;
            let full_value = std::mem::take(&mut row.values[c].text);
            let mut annotations = std::mem::take(&mut row.values[c].annotations);
            let mut annotation_idx = 0;
            let mut active_render_mode = ResultRenderType::Value;
            row.values[c].text = truncate_value(
                &full_value,
                self.column_widths[c],
                &mut current_pos,
                &mut current_render_width,
            );
            row.values[c].render_width = Some(current_render_width);
            row.values[c].decomposed = true;
            while annotation_idx < annotations.len() && annotations[annotation_idx].start < current_pos {
                row.values[c].annotations.push(annotations[annotation_idx]);
                annotation_idx += 1;
            }
            while current_pos < full_value.len() {
                if current_row >= extra_rows.len() {
                    if extra_rows.len() > max_rows_per_row {
                        // No space for another row.
                        break;
                    }
                    let mut extra_row = BoxRenderRow::new(RenderRowType::RowValues);
                    for current_val in &row.values {
                        extra_row.values.push(BoxRenderValue::new(
                            String::new(),
                            current_val.render_mode,
                            current_val.alignment,
                            current_val.column_idx,
                            None,
                        ));
                    }
                    extra_rows.push(extra_row);
                }
                let can_add_extra_row =
                    current_row + 1 < extra_rows.len() || extra_rows.len() < max_rows_per_row;
                let extra_value = &mut extra_rows[current_row].values[c];
                current_row += 1;
                let start_pos = current_pos;
                current_render_width = 0;
                if can_add_extra_row {
                    extra_value.text = truncate_value(
                        &full_value,
                        self.column_widths[c],
                        &mut current_pos,
                        &mut current_render_width,
                    );
                } else {
                    // The last row takes all the remaining text.
                    extra_value.text = full_value[current_pos..].to_owned();
                    current_render_width = render_width(&extra_value.text);
                    current_pos = full_value.len();
                }
                extra_value.render_width = Some(current_render_width);
                extra_value.decomposed = true;
                if active_render_mode != ResultRenderType::Value {
                    extra_value.annotations.push(HighlightingAnnotation {
                        render_mode: active_render_mode,
                        start: 0,
                    });
                }
                while annotation_idx < annotations.len() && annotations[annotation_idx].start < current_pos {
                    annotations[annotation_idx].start -= start_pos;
                    extra_value.annotations.push(annotations[annotation_idx]);
                    active_render_mode = annotations[annotation_idx].render_mode;
                    annotation_idx += 1;
                }
            }
        }
        // A separator before every row but the first.
        if !is_first_row {
            rows.push(BoxRenderRow::new(RenderRowType::Separator));
        }
        rows.push(row);
        rows.extend(extra_rows);
    }

    /// `ComputeRenderWidths`.
    fn compute_render_widths(&mut self, min_width: usize, max_width: usize) {
        let mut column_count = self.result_types.len();

        // The header: names and types.
        let mut header_row = BoxRenderRow::new(RenderRowType::RowValues);
        let mut type_row = BoxRenderRow::new(RenderRowType::RowValues);
        for c in 0..column_count {
            let column_name = Self::convert_render_value(&self.column_names[c]);
            let column_name_width = render_width(&column_name);
            let mut column_type_width = 0;
            header_row.values.push(BoxRenderValue::new(
                column_name,
                ResultRenderType::ColumnName,
                ValueRenderAlignment::Middle,
                None,
                Some(column_name_width),
            ));
            if self.config.render_mode == BoxRenderMode::Rows {
                let column_type = render_type(&self.result_types[c]);
                column_type_width = render_width(&column_type);
                type_row.values.push(BoxRenderValue::new(
                    column_type,
                    ResultRenderType::ColumnType,
                    ValueRenderAlignment::Middle,
                    None,
                    Some(column_type_width),
                ));
            }
            self.column_widths.push(column_name_width.max(column_type_width));
        }
        self.header_rows.push(header_row);
        if self.config.render_mode == BoxRenderMode::Rows {
            self.header_rows.push(type_row);
        }

        // The widths of the values.
        for collection in &self.render_collections {
            for row in collection {
                for (c, cell) in row.iter().enumerate() {
                    if cell.render_length > self.column_widths[c] {
                        self.column_widths[c] = cell.render_length;
                    }
                }
            }
        }

        let mut shortened_columns = false;
        // The total width: a pipe, then per column a space, the value, a space and a pipe.
        self.total_render_length = 1;
        for width in &self.column_widths {
            self.total_render_length += width + 3;
        }
        if self.total_render_length < min_width {
            // Stretch the first column so that the footer fits.
            self.column_widths[0] += min_width - self.total_render_length;
            self.total_render_length = min_width;
        }
        // Constrain the width.
        if self.total_render_length > max_width {
            let original_widths = self.column_widths.clone();
            // Before removing columns, try shortening them.
            let mut max_shorten_amount = Vec::with_capacity(self.column_widths.len());
            let mut total_max_shorten_amount = 0;
            for &width in &self.column_widths {
                if width <= self.config.max_col_width {
                    max_shorten_amount.push(0);
                    continue;
                }
                let max_diff = width - self.config.max_col_width;
                max_shorten_amount.push(max_diff);
                total_max_shorten_amount += max_diff;
            }
            let mut shorten_amount_required = self.total_render_length - max_width;
            if total_max_shorten_amount >= shorten_amount_required {
                // Shortening suffices: shorten the widest columns towards the same width.
                let mut shorten_amount_required_map: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
                for (col_idx, &amount) in max_shorten_amount.iter().enumerate() {
                    shorten_amount_required_map
                        .entry(amount)
                        .or_default()
                        .push(col_idx);
                }
                let mut actual_shorten_amounts = vec![0; max_shorten_amount.len()];
                while shorten_amount_required > 0 {
                    // The columns that can shrink the most.
                    let Some((&largest_width, column_list)) = shorten_amount_required_map.iter().next_back()
                    else {
                        break;
                    };
                    let column_list = column_list.clone();
                    // Shrink them to the next-largest width.
                    let second_largest = shorten_amount_required_map.range(..largest_width).next_back();
                    let second_largest_width = second_largest.map_or(0, |(&width, _)| width);
                    let max_shorten_width = largest_width - second_largest_width;
                    let total_potential_shorten_width = max_shorten_width * column_list.len();
                    if total_potential_shorten_width >= shorten_amount_required {
                        // This set of columns suffices: shorten them equally.
                        let shorten_amount_per_column = shorten_amount_required / column_list.len();
                        for &column_idx in &column_list {
                            actual_shorten_amounts[column_idx] += shorten_amount_per_column;
                            shorten_amount_required -= shorten_amount_per_column;
                        }
                        // Truncation may leave single units to shorten.
                        for &column_idx in column_list.iter().rev() {
                            if shorten_amount_required == 0 {
                                break;
                            }
                            actual_shorten_amounts[column_idx] += 1;
                            shorten_amount_required -= 1;
                        }
                        break;
                    }
                    if second_largest.is_none() {
                        break;
                    }
                    // Shorten these columns to the next width and merge them into its list.
                    for &column_idx in &column_list {
                        actual_shorten_amounts[column_idx] += max_shorten_width;
                    }
                    shorten_amount_required_map
                        .entry(second_largest_width)
                        .or_default()
                        .extend(column_list);
                    shorten_amount_required_map.remove(&largest_width);
                    shorten_amount_required -= total_potential_shorten_width;
                }
                for (c, &amount) in actual_shorten_amounts.iter().enumerate() {
                    if amount == 0 {
                        continue;
                    }
                    self.column_widths[c] -= amount;
                    self.total_render_length -= amount;
                    shortened_columns = true;
                }
            } else {
                // Shortening does not suffice: shorten every column to the maximum column
                // width, then prune columns.
                for width in &mut self.column_widths {
                    if *width <= self.config.max_col_width {
                        continue;
                    }
                    self.total_render_length -= *width - self.config.max_col_width;
                    *width = self.config.max_col_width;
                    shortened_columns = true;
                }
            }

            if self.total_render_length > max_width {
                // Still too wide: remove columns, making room for the "…" column first.
                self.total_render_length += 3 + DOTDOTDOT_LENGTH;
                // Columns go in zig-zag order from the middle: #5, #4, #6, #3, #7, ...
                let mut offset: i64 = 0;
                while self.total_render_length > max_width {
                    let c = (column_count as i64 / 2 + offset) as usize;
                    self.total_render_length -= self.column_widths[c] + 3;
                    self.pruned_columns.insert(c);
                    offset = if offset >= 0 { -offset - 1 } else { -offset };
                }
                // Space left after pruning widens the shortened columns again.
                let mut space_left = max_width - self.total_render_length;
                for c in 0..self.column_widths.len() {
                    if space_left == 0 {
                        break;
                    }
                    if self.pruned_columns.contains(&c) || self.column_widths[c] >= original_widths[c] {
                        continue;
                    }
                    let increase_amount = space_left.min(original_widths[c] - self.column_widths[c]);
                    self.column_widths[c] += increase_amount;
                    space_left -= increase_amount;
                    self.total_render_length += increase_amount;
                }
            }
        }

        self.update_column_count_footer(column_count);

        let mut added_split_column = false;
        let mut new_widths = Vec::new();
        for c in 0..column_count {
            if !self.pruned_columns.contains(&c) {
                self.column_map.push(Some(c));
                new_widths.push(self.column_widths[c]);
            } else if !added_split_column {
                // The "…" column.
                self.column_map.push(None);
                new_widths.push(DOTDOTDOT_LENGTH);
                added_split_column = true;
            }
        }
        self.column_widths = new_widths;
        column_count = self.column_widths.len();
        debug_assert_eq!(column_count, self.column_map.len());

        // Prune the columns from the header rows.
        for header_row in &mut self.header_rows {
            let mut values: Vec<Option<BoxRenderValue>> = std::mem::take(&mut header_row.values)
                .into_iter()
                .map(Some)
                .collect();
            header_row.values = self
                .column_map
                .iter()
                .flatten()
                .filter_map(|&c| values[c].take())
                .collect();
        }

        let row_count: usize = self.render_collections.iter().map(Vec::len).sum();
        // Shortened columns may expand over several lines, in `.mode rows` without hidden
        // columns.
        if shortened_columns
            && self.config.render_mode == BoxRenderMode::Rows
            && row_count > 0
            && row_count + 5 < self.config.max_rows
            && self.pruned_columns.is_empty()
        {
            let rows_per_row = if self.config.max_rows <= 5 {
                0
            } else {
                (self.config.max_rows - 5) / row_count
            };
            self.max_rows_per_row = rows_per_row.max(1);
            if self.max_rows_per_row > 1 {
                self.expand_rows = true;
            }
        }
    }

    /// `RenderLayoutLine`.
    fn render_layout_line(
        &self,
        ss: &mut ResultRenderer<'_>,
        layout: &str,
        boundary: &str,
        left_corner: &str,
        right_corner: &str,
    ) {
        let mut line = String::with_capacity(self.total_render_length * 3 + 1);
        line.push_str(left_corner);
        let mut column_index = 0;
        for k in 0..self.total_render_length - 2 {
            if column_index < self.column_boundary_positions.len()
                && k == self.column_boundary_positions[column_index]
            {
                line.push_str(boundary);
                column_index += 1;
            } else {
                line.push_str(layout);
            }
        }
        line.push_str(right_corner);
        line.push('\n');
        ss.layout(&line);
    }

    /// `RenderDivider`: three rows of dots between the top and the bottom rows.
    fn render_divider(&self, ss: &mut ResultRenderer<'_>, prev_row: &BoxRenderRow, next_row: &BoxRenderRow) {
        const DIVIDER_ROW_COUNT: usize = 3;
        let mut divider_rows: Vec<BoxRenderRow> = (0..DIVIDER_ROW_COUNT)
            .map(|_| BoxRenderRow::new(RenderRowType::RowValues))
            .collect();
        for c in 0..prev_row.values.len() {
            let prev_value = &prev_row.values[c];
            let next_value = &next_row.values[c];
            let mut alignment = prev_value.alignment;
            let text = if alignment == ValueRenderAlignment::Middle {
                DOT.to_owned()
            } else {
                // Left or right aligned: the dot goes in the middle of the values.
                let top_length = self.column_widths[c].min(render_width(&prev_value.text));
                let bottom_length = self.column_widths[c].min(render_width(&next_value.text));
                let mut dot_length = top_length.min(bottom_length);
                if top_length == 0 {
                    dot_length = bottom_length;
                } else if bottom_length == 0 {
                    dot_length = top_length;
                }
                if dot_length > 1 {
                    let padding = dot_length - 1;
                    let (left_padding, right_padding) = if alignment == ValueRenderAlignment::Left {
                        (padding / 2, padding - padding / 2)
                    } else {
                        (padding - padding / 2, padding / 2)
                    };
                    format!("{}{DOT}{}", " ".repeat(left_padding), " ".repeat(right_padding))
                } else {
                    if dot_length == 0 {
                        // Everything is empty.
                        alignment = ValueRenderAlignment::Middle;
                    }
                    DOT.to_owned()
                }
            };
            for divider in &mut divider_rows {
                divider.values.push(BoxRenderValue::new(
                    text.clone(),
                    ResultRenderType::Layout,
                    alignment,
                    None,
                    None,
                ));
            }
        }
        for divider in &mut divider_rows {
            self.render_row(ss, divider);
        }
    }

    /// `RenderRow`.
    fn render_row(&self, ss: &mut ResultRenderer<'_>, row: &mut BoxRenderRow) {
        if row.row_type == RenderRowType::Separator {
            self.render_layout_line(ss, HORIZONTAL, MIDDLE, LMIDDLE, RMIDDLE);
            return;
        }
        let mut split_value = BoxRenderValue::new(
            DOTDOTDOT.to_owned(),
            ResultRenderType::Layout,
            ValueRenderAlignment::Middle,
            None,
            Some(1),
        );
        let mut value_idx = 0;
        for column_idx in 0..self.column_widths.len() {
            let render_value = if self.column_map[column_idx].is_some() {
                value_idx += 1;
                &mut row.values[value_idx - 1]
            } else {
                &mut split_value
            };
            let render_mode = render_value.render_mode;
            if matches!(render_mode, ResultRenderType::NullValue | ResultRenderType::Value) {
                ss.column_idx = render_value.column_idx;
                if !render_value.decomposed && self.can_pretty_print(render_value) {
                    self.highlight_value(render_value);
                }
            }
            self.render_value(
                ss,
                &render_value.text,
                self.column_widths[column_idx],
                render_mode,
                &render_value.annotations,
                render_value.alignment,
                render_value.render_width,
                None,
            );
        }
        ss.layout(VERTICAL);
        ss.layout("\n");
    }

    /// `RenderHeader`.
    fn render_header(&mut self, ss: &mut ResultRenderer<'_>, context: &dyn RenderContext) {
        self.render_layout_line(ss, HORIZONTAL, TMIDDLE, LTCORNER, RTCORNER);
        let mut rows_to_render = Vec::new();
        for row in std::mem::take(&mut self.header_rows) {
            if self.expand_rows {
                let is_first_row = self.is_first_row;
                self.potentially_expand_row(row, &mut rows_to_render, is_first_row);
            } else {
                rows_to_render.push(row);
            }
        }
        for row in &mut rows_to_render {
            if context.is_interrupted() {
                return;
            }
            self.render_row(ss, row);
        }
        if self.result.row_count() > 0 {
            self.render_row(ss, &mut BoxRenderRow::new(RenderRowType::Separator));
        }
    }

    /// `RenderValues`.
    fn render_values(
        &mut self,
        ss: &mut ResultRenderer<'_>,
        collections: &mut [RenderDataCollection],
        context: &dyn RenderContext,
    ) {
        let columns_to_scan: Vec<usize> = self.column_map.iter().flatten().copied().collect();
        let mut render_divider = false;
        let mut last_rendered_rows: Vec<BoxRenderRow> = Vec::new();
        let mut row_idx = 0;
        let mut is_first_collection = true;
        for collection in collections.iter_mut() {
            if collection.is_empty() {
                continue;
            }
            if !is_first_collection {
                render_divider = true;
            }
            is_first_collection = false;
            let mut chunk_rows: Vec<BoxRenderRow> = Vec::with_capacity(collection.len());
            for (r, cells) in collection.iter_mut().enumerate() {
                let mut row = BoxRenderRow::new(RenderRowType::RowValues);
                for (c, &scan_column) in columns_to_scan.iter().enumerate() {
                    let cell = &mut cells[scan_column];
                    let (render_value, mut render_type) = match cell.text.take() {
                        None => (self.config.null_value.clone(), ResultRenderType::NullValue),
                        Some(text) => (text, ResultRenderType::Value),
                    };
                    let mut column_idx = None;
                    let mut alignment;
                    if self.config.render_mode == BoxRenderMode::Rows {
                        // Rows mode aligns every column by its type.
                        column_idx = Some(scan_column);
                        alignment = type_alignment(&self.result_types[scan_column]);
                    } else {
                        // Columns mode left-aligns the header columns and right-aligns the
                        // values, whose type is that of the row's column.
                        match c {
                            0 => {
                                render_type = ResultRenderType::ColumnName;
                                alignment = ValueRenderAlignment::Left;
                            }
                            1 => {
                                render_type = ResultRenderType::ColumnType;
                                alignment = ValueRenderAlignment::Left;
                            }
                            _ => {
                                render_type = ResultRenderType::Value;
                                alignment = ValueRenderAlignment::Right;
                                column_idx = Some(row_idx + r);
                            }
                        }
                    }
                    if self.config.large_number_rendering == LargeNumberRendering::Footer {
                        // The large number footer aligns to the middle and renders as NULL.
                        alignment = ValueRenderAlignment::Middle;
                        if row_idx + r == 1 {
                            render_type = ResultRenderType::NullValue;
                        }
                    }
                    row.values.push(BoxRenderValue::new(
                        render_value,
                        render_type,
                        alignment,
                        column_idx,
                        Some(cell.render_length),
                    ));
                }
                chunk_rows.push(row);
            }
            row_idx += collection.len();
            let mut rows_to_render = Vec::new();
            for row in chunk_rows {
                if context.is_interrupted() {
                    return;
                }
                if render_divider {
                    if let Some(last) = last_rendered_rows.last() {
                        self.render_divider(ss, last, &row);
                    }
                    render_divider = false;
                }
                if self.expand_rows {
                    let is_first_row = self.is_first_row;
                    self.potentially_expand_row(row, &mut rows_to_render, is_first_row);
                } else {
                    rows_to_render.push(row);
                }
                self.is_first_row = false;
            }
            for row in &mut rows_to_render {
                if context.is_interrupted() {
                    return;
                }
                self.render_row(ss, row);
            }
            last_rendered_rows = rows_to_render;
        }
    }

    /// `RenderFooter`.
    fn render_footer(&mut self, ss: &mut ResultRenderer<'_>, row_count: usize, column_count: usize) {
        let footer = std::mem::take(&mut self.footer);
        let BoxRendererFooter {
            row_count_str,
            mut readable_rows_str,
            mut shown_str,
            column_count_str,
            render_length,
            has_hidden_columns,
            has_hidden_rows,
            ..
        } = footer;
        let total = self.total_render_length;
        // Can the row count, the readable count and the shown count share a line?
        let minimum_length = row_count_str.len() + column_count_str.len() + 6;
        let render_rows_and_columns = total >= minimum_length
            && ((has_hidden_columns && row_count > 0) || (row_count >= 10 && column_count > 1));
        let render_rows = total >= render_length && (row_count == 0 || row_count >= 10);
        // The bottom of the table.
        self.render_layout_line(ss, HORIZONTAL, DMIDDLE, LDCORNER, RDCORNER);
        if !render_rows && !render_rows_and_columns {
            return;
        }
        let mut padding = total - row_count_str.len() - 4;
        if render_rows_and_columns {
            padding -= column_count_str.len();
        }
        let mut extra_render_str = String::new();
        // Is there space for the shown count on the same line?
        let shown_size =
            readable_rows_str.len() + shown_str.len() + if readable_rows_str.is_empty() { 3 } else { 5 };
        if has_hidden_rows && padding >= shown_size {
            extra_render_str = format!(" ({shown_str}");
            if !readable_rows_str.is_empty() {
                extra_render_str += &format!(", {readable_rows_str}");
            }
            extra_render_str.push(')');
            padding -= shown_size;
            readable_rows_str.clear();
            shown_str.clear();
        }

        ss.layout("  ");
        if render_rows_and_columns {
            ss.render(ResultRenderType::Footer, &row_count_str);
            if !extra_render_str.is_empty() {
                ss.render(ResultRenderType::Footer, &extra_render_str);
            }
            // Is there space for the hidden rows hint on this line?
            let hint = &self.config.hidden_rows_hint;
            if (has_hidden_columns || has_hidden_rows) && !hint.is_empty() && padding >= hint.len() + 10 {
                padding -= hint.len();
                let lpadding = padding / 2;
                let rpadding = padding - lpadding;
                ss.layout(&" ".repeat(lpadding));
                ss.render(ResultRenderType::Footer, hint);
                ss.layout(&" ".repeat(rpadding));
            } else {
                ss.layout(&" ".repeat(padding));
            }
            ss.render(ResultRenderType::Footer, &column_count_str);
        } else if render_rows {
            let lpadding = padding / 2;
            let rpadding = padding - lpadding;
            ss.layout(&" ".repeat(lpadding));
            ss.render(ResultRenderType::Footer, &row_count_str);
            if !extra_render_str.is_empty() {
                ss.render(ResultRenderType::Footer, &extra_render_str);
            }
            ss.layout(&" ".repeat(rpadding));
        }
        ss.layout("\n");
        if readable_rows_str.is_empty() && shown_str.is_empty() {
            return;
        }
        // The readable and shown counts still need rendering: on one line if they fit.
        let combined_shown_length = readable_rows_str.len() + shown_str.len() + 4;
        if !readable_rows_str.is_empty() && !shown_str.is_empty() && combined_shown_length <= total {
            ss.layout("  ");
            ss.render(ResultRenderType::Footer, &shown_str);
            ss.layout(&" ".repeat(total - combined_shown_length));
            ss.render(ResultRenderType::Footer, &readable_rows_str);
            ss.layout("\n");
            readable_rows_str.clear();
            shown_str.clear();
        }
        let alignment = if render_rows_and_columns {
            ValueRenderAlignment::Left
        } else {
            ValueRenderAlignment::Middle
        };
        for text in [shown_str, readable_rows_str] {
            if text.is_empty() {
                continue;
            }
            self.render_value(
                ss,
                &format!("({text})"),
                total - 4,
                ResultRenderType::Footer,
                &[],
                alignment,
                None,
                Some(" "),
            );
            ss.layout("\n");
        }
    }
}

/// `BoxRenderer::TruncateValue`: the part of `value` from `pos` that fits in
/// `column_width` columns given `current_render_width` used already, up to a newline
/// (which is skipped). Advances `pos` and `current_render_width`.
pub fn truncate_value(
    value: &str,
    column_width: usize,
    pos: &mut usize,
    current_render_width: &mut usize,
) -> String {
    let start_pos = *pos;
    let bytes = value.as_bytes();
    while *pos < value.len() {
        if bytes[*pos] == b'\n' {
            // A newline ends the line; skip it.
            let render_pos = *pos;
            *pos += 1;
            return value[start_pos..render_pos].to_owned();
        }
        // Does this character fit?
        let char_size = char_render_width(value, *pos);
        if *current_render_width + char_size > column_width {
            break;
        }
        *current_render_width += char_size;
        *pos = next_grapheme_cluster(value, *pos);
    }
    value[start_pos..*pos].to_owned()
}

/// `BoxRenderer::TryFormatLargeNumber`: a readable rendering of a number of a million or
/// more ("1.23 million"), empty for other input.
pub fn try_format_large_number(numeric: &str, decimal_sep: Option<u8>) -> String {
    if numeric.len() <= 5 {
        // Too small for sure.
        return String::new();
    }
    let bytes = numeric.as_bytes();
    let mut number: u64 = 0;
    let negative = bytes[0] == b'-';
    for &c in &bytes[usize::from(negative)..] {
        if c == b'.' {
            break;
        }
        if !c.is_ascii_digit() {
            // Not a number, or something like 1.23e7.
            return String::new();
        }
        if number >= 1_000_000_000_000_000_000 {
            // Too big.
            return String::new();
        }
        number = number * 10 + u64::from(c - b'0');
    }
    const BASES: [(u64, &str); 5] = [
        (1_000_000, "million"),
        (1_000_000_000, "billion"),
        (1_000_000_000_000, "trillion"),
        (1_000_000_000_000_000, "quadrillion"),
        (1_000_000_000_000_000_000, "quintillion"),
    ];
    let mut base = 0;
    let mut unit = "";
    for (candidate, name) in BASES {
        // Round the number according to this base.
        let rounded_number = number + ((candidate / 100) / 2);
        if rounded_number >= candidate {
            base = candidate;
            unit = name;
        }
    }
    if unit.is_empty() {
        return String::new();
    }
    number += (base / 100) / 2;
    let decimal_str = (number / (base / 100)).to_string();
    let split = decimal_str.len() - 2;
    let mut result = String::new();
    if negative {
        result.push('-');
    }
    result.push_str(&decimal_str[..split]);
    result.push(char::from(decimal_sep.unwrap_or(b'.')));
    result.push_str(&decimal_str[split..]);
    result.push(' ');
    result.push_str(unit);
    result
}

/// `RenderType`: the short name of a type in the header.
fn render_type(ty: &LogicalType) -> String {
    if ty.alias().is_some() {
        return ty.to_string().to_lowercase();
    }
    match ty.id() {
        LogicalTypeId::TinyInt => "int8".to_owned(),
        LogicalTypeId::SmallInt => "int16".to_owned(),
        LogicalTypeId::Integer => "int32".to_owned(),
        LogicalTypeId::BigInt => "int64".to_owned(),
        LogicalTypeId::HugeInt => "int128".to_owned(),
        LogicalTypeId::UTinyInt => "uint8".to_owned(),
        LogicalTypeId::USmallInt => "uint16".to_owned(),
        LogicalTypeId::UInteger => "uint32".to_owned(),
        LogicalTypeId::UBigInt => "uint64".to_owned(),
        LogicalTypeId::UHugeInt => "uint128".to_owned(),
        LogicalTypeId::List => match ty.child_type() {
            Some(child) => format!("{}[]", render_type(child)),
            None => ty.to_string().to_lowercase(),
        },
        _ => ty.to_string().to_lowercase(),
    }
}

/// The types whose values render as numbers (`TypeAlignment`, `ConvertRenderValue`).
fn is_numeric_render_type(ty: &LogicalType) -> bool {
    matches!(
        ty.id(),
        LogicalTypeId::TinyInt
            | LogicalTypeId::SmallInt
            | LogicalTypeId::Integer
            | LogicalTypeId::BigInt
            | LogicalTypeId::HugeInt
            | LogicalTypeId::UTinyInt
            | LogicalTypeId::USmallInt
            | LogicalTypeId::UInteger
            | LogicalTypeId::UBigInt
            | LogicalTypeId::UHugeInt
            | LogicalTypeId::Decimal
            | LogicalTypeId::Float
            | LogicalTypeId::Double
    )
}

/// `TypeAlignment`: numbers are right-aligned, everything else left-aligned.
fn type_alignment(ty: &LogicalType) -> ValueRenderAlignment {
    if is_numeric_render_type(ty) {
        ValueRenderAlignment::Right
    } else {
        ValueRenderAlignment::Left
    }
}

/// `LogicalType::IsJSONType() || LogicalType::IsNested()`.
pub fn is_json_or_nested(ty: &LogicalType) -> bool {
    is_json_type(ty) || ty.is_nested()
}

/// `LogicalType::IsJSONType`: VARCHAR with the JSON alias.
pub fn is_json_type(ty: &LogicalType) -> bool {
    ty.id() == LogicalTypeId::Varchar && ty.alias() == Some("JSON")
}

/// The state of [`JsonParser`] (`JSONState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JsonState {
    Regular,
    InQuote,
    Escape,
}

/// What a [`JsonParser`] reports (the virtual `Handle*` methods of `JSONParser`).
trait JsonHandler {
    fn handle_null(&mut self, _pos: usize) {}
    fn handle_bracket_open(&mut self, _bracket: u8) {}
    fn handle_bracket_close(&mut self, _bracket: u8) {}
    fn handle_quote_start(&mut self, _quote: u8, _pos: usize) {}
    fn handle_quote_end(&mut self, _quote: u8, _pos: usize) {}
    fn handle_comma(&mut self) {}
    fn handle_colon(&mut self) {}
    fn handle_character(&mut self, _c: u8) {}
    fn handle_escape_start(&mut self, _c: u8) {}
}

/// `JSONParser::Process`: walks a rendered nested value (JSON-like text) and reports
/// its structure; `false` if the brackets do not match.
fn process_json(value: &str, handler: &mut dyn JsonHandler) -> bool {
    let bytes = value.as_bytes();
    let mut separators: Vec<u8> = Vec::new();
    let mut state = JsonState::Regular;
    let mut quote_char = b'"';
    let mut can_parse_value = false;
    let mut in_unquoted_value = false;
    let mut pos = 0;
    while pos < bytes.len() {
        let c = bytes[pos];
        match state {
            JsonState::Regular => {
                if can_parse_value
                    && pos + 4 < bytes.len()
                    && bytes[pos].eq_ignore_ascii_case(&b'n')
                    && bytes[pos + 1].eq_ignore_ascii_case(&b'u')
                    && bytes[pos + 2].eq_ignore_ascii_case(&b'l')
                    && bytes[pos + 3].eq_ignore_ascii_case(&b'l')
                {
                    handler.handle_null(pos);
                    pos += 4;
                    continue;
                }
                match c {
                    b'[' | b'{' => {
                        separators.push(c);
                        handler.handle_bracket_open(c);
                        can_parse_value = c == b'[';
                    }
                    b'}' | b']' => {
                        let matching =
                            matches!((separators.last(), c), (Some(b'{'), b'}') | (Some(b'['), b']'));
                        if !matching {
                            return false;
                        }
                        separators.pop();
                        handler.handle_bracket_close(c);
                        in_unquoted_value = false;
                    }
                    b'"' | b'\'' => {
                        handler.handle_quote_start(c, pos);
                        quote_char = c;
                        state = JsonState::InQuote;
                    }
                    b',' => {
                        handler.handle_comma();
                        in_unquoted_value = false;
                    }
                    b':' => {
                        handler.handle_colon();
                        can_parse_value = true;
                    }
                    b'\\' => {
                        // Skip the literal "\n" and "\t" escapes of the rendering.
                        if pos + 1 < bytes.len() && matches!(bytes[pos + 1], b'n' | b't') {
                            pos += 2;
                            continue;
                        }
                        handler.handle_character(c);
                    }
                    b' ' => {
                        if in_unquoted_value {
                            handler.handle_character(c);
                        }
                    }
                    b'\t' | b'\n' => {}
                    _ => {
                        in_unquoted_value = true;
                        handler.handle_character(c);
                    }
                }
            }
            JsonState::InQuote => {
                if c == quote_char {
                    state = JsonState::Regular;
                    handler.handle_quote_end(c, pos);
                } else if c == b'\\' {
                    state = JsonState::Escape;
                    handler.handle_escape_start(c);
                } else {
                    handler.handle_character(c);
                }
            }
            JsonState::Escape => {
                state = JsonState::InQuote;
                handler.handle_character(c);
            }
        }
        pos += 1;
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JsonFormattingMode {
    Standard,
    CompactVertical,
    CompactHorizontal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JsonComponentType {
    BracketOpen,
    BracketClose,
    Literal,
    Colon,
    Comma,
    NullValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JsonFormattingResult {
    Success,
    TooManyRows,
    TooWide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InlineMode {
    Standard,
    InlinedSingleLine,
    InlinedMultiLine,
}

#[derive(Debug)]
struct JsonComponent {
    kind: JsonComponentType,
    /// The component's bytes (a literal may hold part of a multi-byte character).
    text: Vec<u8>,
}

/// The state of one formatting attempt (`JSONFormatter::FormatState`).
#[derive(Debug)]
struct FormatState {
    mode: JsonFormattingMode,
    result: Vec<u8>,
    component_idx: usize,
    row_count: usize,
    line_length: usize,
    depth: usize,
    max_rows: usize,
    max_width: usize,
    indentation_size: usize,
    format_result: JsonFormattingResult,
}

/// Lays out a nested value over several lines (`JSONFormatter`).
#[derive(Debug, Default)]
struct JsonFormatter {
    components: Vec<JsonComponent>,
}

/// The render width of bytes that may hold partial characters.
fn bytes_render_width(text: &[u8]) -> usize {
    render_width(&String::from_utf8_lossy(text))
}

impl JsonHandler for JsonFormatter {
    fn handle_null(&mut self, _pos: usize) {
        self.push(JsonComponentType::NullValue, b"null");
    }
    fn handle_bracket_open(&mut self, bracket: u8) {
        self.push(JsonComponentType::BracketOpen, &[bracket]);
    }
    fn handle_bracket_close(&mut self, bracket: u8) {
        self.push(JsonComponentType::BracketClose, &[bracket]);
    }
    fn handle_quote_start(&mut self, quote: u8, _pos: usize) {
        self.add_literal_character(quote);
    }
    fn handle_quote_end(&mut self, quote: u8, _pos: usize) {
        self.add_literal_character(quote);
    }
    fn handle_comma(&mut self) {
        self.push(JsonComponentType::Comma, b",");
    }
    fn handle_colon(&mut self) {
        self.push(JsonComponentType::Colon, b":");
    }
    fn handle_character(&mut self, c: u8) {
        self.add_literal_character(c);
    }
    fn handle_escape_start(&mut self, c: u8) {
        self.add_literal_character(c);
    }
}

impl JsonFormatter {
    /// `FormatValue`: the standard layout, or a compact one if that does not fit.
    fn format_value(render_value: &mut BoxRenderValue, max_rows: usize, max_width: usize) {
        let mut formatter = Self::default();
        process_json(&render_value.text, &mut formatter);
        let mut indentation_size = 2;
        let result = formatter.try_format(JsonFormattingMode::Standard, render_value, max_rows, max_width, 2);
        if result == JsonFormattingResult::Success {
            return;
        }
        // Too wide: smaller indentation; too many rows: compact vertically.
        let mode = if result == JsonFormattingResult::TooWide {
            indentation_size = 1;
            JsonFormattingMode::CompactHorizontal
        } else {
            JsonFormattingMode::CompactVertical
        };
        formatter.try_format(mode, render_value, max_rows, max_width, indentation_size);
    }

    fn push(&mut self, kind: JsonComponentType, text: &[u8]) {
        self.components.push(JsonComponent {
            kind,
            text: text.to_vec(),
        });
    }

    fn add_literal_character(&mut self, c: u8) {
        if self
            .components
            .last()
            .is_none_or(|last| last.kind != JsonComponentType::Literal)
        {
            self.push(JsonComponentType::Literal, b"");
        }
        if let Some(last) = self.components.last_mut() {
            last.text.push(c);
        }
    }

    fn literal_fits(format_state: &FormatState, render_width_value: usize) -> bool {
        format_state.line_length + render_width_value <= format_state.max_width
    }

    fn add_literal(format_state: &mut FormatState, text: &[u8], skip_adding_if_does_not_fit: bool) {
        let render_width_value = bytes_render_width(text);
        if !Self::literal_fits(format_state, render_width_value) {
            if skip_adding_if_does_not_fit {
                return;
            }
            Self::add_newline(format_state);
            if format_state.format_result != JsonFormattingResult::Success {
                return;
            }
        }
        format_state.result.extend_from_slice(text);
        format_state.line_length += render_width_value;
        if format_state.line_length > format_state.max_width {
            format_state.format_result = JsonFormattingResult::TooWide;
        }
    }

    fn add_space(format_state: &mut FormatState) {
        Self::add_literal(format_state, b" ", true);
    }

    fn add_newline(format_state: &mut FormatState) {
        format_state.result.push(b'\n');
        format_state
            .result
            .extend(std::iter::repeat_n(b' ', format_state.depth));
        format_state.row_count += 1;
        if format_state.row_count > format_state.max_rows {
            format_state.format_result = JsonFormattingResult::TooManyRows;
            return;
        }
        format_state.line_length = format_state.depth;
        if format_state.line_length > format_state.max_width {
            format_state.format_result = JsonFormattingResult::TooWide;
        }
    }

    /// `FormatComponent`.
    fn format_component(
        &self,
        format_state: &mut FormatState,
        component: &JsonComponent,
        inline_mode: InlineMode,
    ) {
        let components = &self.components;
        let c = format_state.component_idx;
        let max_width = format_state.max_width;
        match component.kind {
            JsonComponentType::BracketOpen => {
                format_state.depth += if component.text == b"{" {
                    format_state.indentation_size
                } else {
                    1
                };
                Self::add_literal(format_state, &component.text, false);
                if inline_mode != InlineMode::Standard {
                    return;
                }
                // Not inlined: can the value up to the matching bracket go on this line?
                let mut peek_depth = 0;
                let mut render_size = format_state.line_length;
                let mut peek_idx = c + 1;
                let mut inline_child_mode = InlineMode::Standard;
                while peek_idx < components.len() && render_size <= max_width {
                    let peek_component = &components[peek_idx];
                    if peek_component.kind == JsonComponentType::BracketOpen {
                        peek_depth += 1;
                    } else if peek_component.kind == JsonComponentType::BracketClose {
                        if peek_depth == 0 {
                            if render_size + 1 < max_width {
                                // Fits on a single line.
                                inline_child_mode = InlineMode::InlinedSingleLine;
                            }
                            break;
                        }
                        peek_depth -= 1;
                    }
                    render_size += bytes_render_width(&peek_component.text);
                    if matches!(
                        peek_component.kind,
                        JsonComponentType::Comma | JsonComponentType::Colon
                    ) {
                        render_size += 1;
                    }
                    peek_idx += 1;
                }
                if component.text == b"[" {
                    // Arrays always inline unless they hold objects.
                    // DuckDB resets the depth for every component, so the first closing
                    // bracket ends the scan.
                    peek_idx = c + 1;
                    while peek_idx < components.len() {
                        let peek_component = &components[peek_idx];
                        if peek_component.kind == JsonComponentType::BracketOpen
                            && peek_component.text == b"{"
                        {
                            // An object within the array.
                            break;
                        }
                        if peek_component.kind == JsonComponentType::BracketClose {
                            inline_child_mode = InlineMode::InlinedMultiLine;
                            break;
                        }
                        peek_idx += 1;
                    }
                }
                if inline_child_mode != InlineMode::Standard {
                    for inline_idx in c + 1..=peek_idx.min(components.len().saturating_sub(1)) {
                        let inline_component = &components[inline_idx];
                        if inline_child_mode == InlineMode::InlinedMultiLine && inline_idx < peek_idx {
                            let next_component = &components[inline_idx + 1];
                            if matches!(
                                next_component.kind,
                                JsonComponentType::Comma | JsonComponentType::BracketClose
                            ) {
                                let mut joined = inline_component.text.clone();
                                joined.extend_from_slice(&next_component.text);
                                if !Self::literal_fits(format_state, bytes_render_width(&joined)) {
                                    Self::add_newline(format_state);
                                }
                            }
                        }
                        format_state.component_idx = inline_idx;
                        self.format_component(format_state, inline_component, inline_child_mode);
                    }
                    format_state.component_idx = peek_idx;
                    return;
                }
                if format_state.mode == JsonFormattingMode::CompactVertical
                    && c + 1 < components.len()
                    && components[c + 1].kind == JsonComponentType::BracketOpen
                {
                    // The next bracket adds the newline.
                    return;
                }
                Self::add_newline(format_state);
            }
            JsonComponentType::BracketClose => {
                let depth_diff = if component.text == b"}" {
                    format_state.indentation_size
                } else {
                    1
                };
                format_state.depth = format_state.depth.saturating_sub(depth_diff);
                if inline_mode == InlineMode::Standard {
                    Self::add_newline(format_state);
                }
                Self::add_literal(format_state, &component.text, false);
            }
            JsonComponentType::Comma | JsonComponentType::Colon => {
                Self::add_literal(format_state, &component.text, false);
                // Colons always inline, except when compacting horizontally.
                let always_inline = format_state.mode != JsonFormattingMode::CompactHorizontal
                    && component.kind == JsonComponentType::Colon;
                if inline_mode != InlineMode::Standard || always_inline {
                    Self::add_space(format_state);
                    return;
                }
                if format_state.mode != JsonFormattingMode::Standard {
                    // Compact modes try to inline up to the next comma.
                    let mut peek_depth = 0;
                    let mut render_size = format_state.line_length + 1;
                    let mut peek_idx = c + 1;
                    let mut inline_comma = false;
                    while peek_idx < components.len() && render_size <= max_width {
                        let peek_component = &components[peek_idx];
                        if peek_component.kind == JsonComponentType::BracketOpen {
                            peek_depth += 1;
                        } else if peek_component.kind == JsonComponentType::BracketClose {
                            if peek_depth == 0 {
                                inline_comma = render_size + 1 < max_width;
                                break;
                            }
                            peek_depth -= 1;
                        }
                        if peek_depth == 0 && peek_component.kind == JsonComponentType::Comma {
                            inline_comma = render_size + 2 <= max_width;
                            break;
                        }
                        render_size += bytes_render_width(&peek_component.text);
                        if matches!(
                            peek_component.kind,
                            JsonComponentType::Comma | JsonComponentType::Colon
                        ) {
                            render_size += 1;
                        }
                        peek_idx += 1;
                    }
                    if inline_comma {
                        Self::add_space(format_state);
                        for inline_idx in c + 1..peek_idx {
                            format_state.component_idx = inline_idx;
                            self.format_component(
                                format_state,
                                &components[inline_idx],
                                InlineMode::InlinedSingleLine,
                            );
                        }
                        format_state.component_idx = peek_idx - 1;
                        return;
                    }
                }
                Self::add_newline(format_state);
            }
            JsonComponentType::NullValue | JsonComponentType::Literal => {
                Self::add_literal(format_state, &component.text, false);
            }
        }
    }

    /// `TryFormat`.
    fn try_format(
        &self,
        mode: JsonFormattingMode,
        render_value: &mut BoxRenderValue,
        max_rows: usize,
        max_width: usize,
        indentation_size: usize,
    ) -> JsonFormattingResult {
        let mut format_state = FormatState {
            mode,
            result: Vec::new(),
            component_idx: 0,
            row_count: 0,
            line_length: 0,
            depth: 0,
            max_rows,
            max_width,
            indentation_size,
            format_result: JsonFormattingResult::Success,
        };
        while format_state.component_idx < self.components.len()
            && format_state.format_result == JsonFormattingResult::Success
        {
            let component = &self.components[format_state.component_idx];
            self.format_component(&mut format_state, component, InlineMode::Standard);
            format_state.component_idx += 1;
        }
        if format_state.format_result != JsonFormattingResult::Success {
            return format_state.format_result;
        }
        render_value.text = String::from_utf8_lossy(&format_state.result).into_owned();
        JsonFormattingResult::Success
    }
}

/// Highlights the NULLs and string literals of a nested value (`JSONHighlighter`).
#[derive(Debug)]
struct JsonHighlighter {
    annotations: Vec<HighlightingAnnotation>,
    render_mode: ResultRenderType,
}

impl JsonHandler for JsonHighlighter {
    fn handle_null(&mut self, pos: usize) {
        self.annotations.push(HighlightingAnnotation {
            render_mode: ResultRenderType::NullValue,
            start: pos,
        });
        self.annotations.push(HighlightingAnnotation {
            render_mode: self.render_mode,
            start: pos + 4,
        });
    }
    fn handle_quote_start(&mut self, _quote: u8, pos: usize) {
        self.annotations.push(HighlightingAnnotation {
            render_mode: ResultRenderType::StringLiteral,
            start: pos,
        });
    }
    fn handle_quote_end(&mut self, _quote: u8, pos: usize) {
        self.annotations.push(HighlightingAnnotation {
            render_mode: self.render_mode,
            start: pos + 1,
        });
    }
}

impl JsonHighlighter {
    /// `Highlight`: adds the annotations of `value`, none if it does not parse.
    fn highlight(render_value: &mut BoxRenderValue, value: &str) {
        let mut highlighter = Self {
            annotations: Vec::new(),
            render_mode: render_value.render_mode,
        };
        if process_json(value, &mut highlighter) {
            render_value.annotations.extend(highlighter.annotations);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_numbers() {
        assert_eq!(try_format_large_number("1234567", None), "1.23 million");
        assert_eq!(try_format_large_number("-999999999", None), "-1000.00 million");
        assert_eq!(try_format_large_number("12345", None), "");
        assert_eq!(try_format_large_number("1.5e7", None), "");
    }

    #[test]
    fn truncation() {
        let mut pos = 0;
        let mut width = 0;
        assert_eq!(truncate_value("hello world", 5, &mut pos, &mut width), "hello");
        assert_eq!((pos, width), (5, 5));
    }
}
