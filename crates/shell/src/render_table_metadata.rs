//! Tables drawn as boxes of their columns (DuckDB's
//! `tools/shell/shell_render_table_metadata.cpp`): the output of `.tables`,
//! `.databases` and `DESCRIBE`, grouped by database and schema, with small tables placed
//! side by side.

use std::collections::{BTreeMap, HashSet};

use crate::box_renderer::{
    DOTDOTDOT, DOTDOTDOT_LENGTH, HORIZONTAL, LDCORNER, LTCORNER, RDCORNER, RTCORNER, VERTICAL,
    truncate_value, try_format_large_number,
};
use crate::highlight::{HighlightElementType, ShellHighlight};
use crate::output::PrintOutput;
use crate::state::{ShellState, ShellTableInfo};

/// A piece of a column line: its name, type or constraints (`RenderComponent`).
#[derive(Clone, Debug)]
struct RenderComponent {
    text: String,
    render_width: usize,
    element_type: HighlightElementType,
}

impl RenderComponent {
    fn new(text: String, element_type: HighlightElementType) -> Self {
        Self {
            render_width: ShellState::render_length(&text),
            text,
            element_type,
        }
    }
}

/// The components of a column (`ShellColumnRenderInfo`).
type ShellColumnRenderInfo = Vec<RenderComponent>;

/// A column of column lines (`ColumnRenderRow`): a table with many columns is split into
/// several next to each other.
type ColumnRenderRow = Vec<ShellColumnRenderInfo>;

/// A table laid out for rendering (`ShellTableRenderInfo`).
#[derive(Clone, Debug)]
struct ShellTableRenderInfo {
    table: ShellTableInfo,
    table_name_length: usize,
    render_width: usize,
    max_component_widths: Vec<usize>,
    estimated_size_text: String,
    estimated_size_length: Option<usize>,
    column_renders: Vec<ColumnRenderRow>,
}

/// `PerColumnWidth` of a set of component widths.
fn per_column_width(max_component_widths: &[usize]) -> usize {
    4 + max_component_widths.iter().sum::<usize>() + max_component_widths.len() - 1
}

/// `FormatTableMetadataType`: types in lower case, decimals without their width.
fn format_table_metadata_type(type_name: &str) -> String {
    let result = type_name.to_lowercase();
    if result.starts_with("decimal") {
        return "decimal".to_owned();
    }
    result
}

impl ShellTableRenderInfo {
    fn new(mut table: ShellTableInfo, decimal_sep: Option<u8>) -> Self {
        let table_name_length = ShellState::render_length(&table.table_name);
        // Constraints take a third component.
        let has_constraint_component = table
            .columns
            .iter()
            .any(|column| column.is_not_null || column.is_unique || !column.default_value.is_empty());
        let component_count = 2 + usize::from(has_constraint_component);
        let mut render_row = ColumnRenderRow::new();
        for column in std::mem::take(&mut table.columns) {
            let name_type = if column.is_primary_key {
                HighlightElementType::PrimaryKeyColumn
            } else {
                HighlightElementType::ColumnName
            };
            let mut components = vec![
                RenderComponent::new(column.column_name, name_type),
                RenderComponent::new(
                    format_table_metadata_type(&column.column_type),
                    HighlightElementType::ColumnType,
                ),
            ];
            if has_constraint_component {
                let mut constraints = Vec::new();
                if column.is_not_null {
                    constraints.push("not null".to_owned());
                }
                if column.is_unique {
                    constraints.push("unique".to_owned());
                }
                if !column.default_value.is_empty() {
                    constraints.push(format!("default {}", column.default_value));
                }
                components.push(RenderComponent::new(
                    constraints.join(" "),
                    HighlightElementType::ColumnType,
                ));
            }
            render_row.push(components);
        }
        let mut max_component_widths = vec![0; component_count];
        for column in &render_row {
            for (width, component) in max_component_widths.iter_mut().zip(column) {
                *width = (*width).max(component.render_width);
            }
        }
        let mut info = Self {
            table,
            table_name_length,
            render_width: 0,
            max_component_widths,
            estimated_size_text: String::new(),
            estimated_size_length: None,
            column_renders: vec![render_row],
        };
        info.render_width = (table_name_length + 4).max(info.per_column_width());
        if let Some(estimated_size) = info.table.estimated_size {
            let mut text = estimated_size.to_string();
            let formatted = try_format_large_number(&text, decimal_sep);
            if !formatted.is_empty() {
                text = formatted;
            }
            text.push_str(" rows");
            let length = ShellState::render_length(&text);
            info.estimated_size_text = text;
            info.estimated_size_length = Some(length);
            info.render_width = info.render_width.max(length + 4);
        }
        info
    }

    /// `LineCount`.
    fn line_count(&self) -> usize {
        let constant_count = if self.estimated_size_length.is_some() {
            6
        } else {
            4
        };
        self.column_lines() + constant_count
    }

    fn per_column_width(&self) -> usize {
        per_column_width(&self.max_component_widths)
    }

    /// `ColumnLines`: the lines of the first (longest) column of column lines.
    fn column_lines(&self) -> usize {
        self.column_renders[0].len()
    }

    /// `TruncateValueIfRequired`.
    fn truncate_value_if_required(value: &mut String, render_length: &mut usize, max_render_width: usize) {
        if *render_length <= max_render_width {
            return;
        }
        let mut pos = 0;
        let mut value_render_width = 0;
        *value = truncate_value(
            value,
            max_render_width - DOTDOTDOT_LENGTH,
            &mut pos,
            &mut value_render_width,
        ) + DOTDOTDOT;
        *render_length = value_render_width + DOTDOTDOT_LENGTH;
    }

    /// `Truncate`: shortens the table name and the column components to fit.
    fn truncate(&mut self, max_render_width: usize) {
        const MIN_COMPONENT_SIZE: usize = 5;
        if self.render_width <= max_render_width {
            return;
        }
        Self::truncate_value_if_required(
            &mut self.table.table_name,
            &mut self.table_name_length,
            max_render_width - 4,
        );
        let mut total_column_length = self.per_column_width();
        if total_column_length > max_render_width {
            // Keep the name as long as possible: truncate it only if it almost fills the
            // width by itself.
            let component_count = self.max_component_widths.len();
            let min_leftover_size = component_count * MIN_COMPONENT_SIZE;
            if self.max_component_widths[0] + min_leftover_size > max_render_width {
                self.max_component_widths[0] = max_render_width - min_leftover_size;
            }
            total_column_length = self.per_column_width();
            if total_column_length > max_render_width {
                for i in 1..self.max_component_widths.len() {
                    if self.max_component_widths[i] <= MIN_COMPONENT_SIZE {
                        continue;
                    }
                    let truncate_amount = (total_column_length - max_render_width)
                        .min(self.max_component_widths[i] - MIN_COMPONENT_SIZE);
                    self.max_component_widths[i] -= truncate_amount;
                    total_column_length -= truncate_amount;
                    if total_column_length <= self.render_width {
                        break;
                    }
                }
            }
            for column_render in &mut self.column_renders {
                for column in column_render {
                    for (component, &max_width) in column.iter_mut().zip(&self.max_component_widths) {
                        Self::truncate_value_if_required(
                            &mut component.text,
                            &mut component.render_width,
                            max_width,
                        );
                    }
                }
            }
        }
        self.render_width = max_render_width;
    }
}

/// Tables rendered next to each other (`TableMetadataLine`).
#[derive(Clone, Debug, Default)]
struct TableMetadataLine {
    render_height: usize,
    render_width: usize,
    max_component_widths: Vec<usize>,
    tables: Vec<usize>,
}

impl TableMetadataLine {
    /// `RenderLine`: line `line_idx` of the tables of this line.
    fn render_line(
        &self,
        highlight: &ShellHighlight<'_>,
        table_list: &[ShellTableRenderInfo],
        mut line_idx: usize,
        last_line: bool,
    ) {
        let print = |text: &str, element_type| highlight.print_text(text, PrintOutput::Stdout, element_type);
        // The table this line belongs to.
        let Some(&table_index) = self.tables.iter().find(|&&table| {
            let line_count = table_list[table].line_count();
            if line_idx < line_count {
                return true;
            }
            line_idx -= line_count;
            false
        }) else {
            // No table: spaces, unless this is the last line.
            if !last_line {
                print(&" ".repeat(self.render_width), HighlightElementType::Layout);
            }
            return;
        };
        let render_table = &table_list[table_index];
        let table = &render_table.table;
        let layout_type = if table.is_view {
            HighlightElementType::ViewLayout
        } else {
            HighlightElementType::TableLayout
        };
        let render_width = self.render_width;
        if line_idx == 0 {
            print(
                &format!("{LTCORNER}{}{RTCORNER}", HORIZONTAL.repeat(render_width - 2)),
                layout_type,
            );
            return;
        }
        if line_idx == 1 {
            // The table name.
            let space_count = render_width - render_table.table_name_length - 2;
            let lspace = space_count / 2;
            let rspace = space_count - lspace;
            print(&format!("{VERTICAL}{}", " ".repeat(lspace)), layout_type);
            print(&table.table_name, HighlightElementType::TableName);
            print(&format!("{}{VERTICAL}", " ".repeat(rspace)), layout_type);
            return;
        }
        if line_idx > 2 && line_idx < render_table.column_lines() + 3 {
            let column_idx = line_idx - 3;
            // A long table name may have widened the box: pad the columns.
            let per_column_render_width = per_column_width(&self.max_component_widths);
            let column_render_width = render_table.column_renders.len() * per_column_render_width;
            let mut extra_render_width = render_width - column_render_width;
            let render_width_per_column = extra_render_width / render_table.column_renders.len();
            for (render_idx, column_render) in render_table.column_renders.iter().enumerate() {
                let is_last = render_idx + 1 == render_table.column_renders.len();
                let mut column_line = if render_idx == 0 {
                    VERTICAL.to_owned()
                } else {
                    " ".to_owned()
                };
                if let Some(column) = column_render.get(column_idx) {
                    column_line.push(' ');
                    print(&column_line, layout_type);
                    for (component_idx, component) in column.iter().enumerate() {
                        print(&component.text, component.element_type);
                        // Padding between the components.
                        let mut padding =
                            " ".repeat(self.max_component_widths[component_idx] - component.render_width + 1);
                        if extra_render_width > 0 {
                            let render_count = if is_last {
                                extra_render_width
                            } else {
                                render_width_per_column
                            };
                            padding += &" ".repeat(render_count);
                            extra_render_width -= render_count;
                        }
                        print(&padding, layout_type);
                    }
                } else {
                    // This column of column lines has no more columns.
                    column_line = " ".repeat(per_column_render_width - 1);
                    print(&column_line, layout_type);
                }
                print(if is_last { VERTICAL } else { " " }, layout_type);
            }
            return;
        }
        if line_idx == 2 || (table.estimated_size.is_some() && line_idx == render_table.column_lines() + 3) {
            // A blank line after the table name, and before the row count.
            print(
                &format!("{VERTICAL}{}{VERTICAL}", " ".repeat(render_width - 2)),
                layout_type,
            );
            return;
        }
        if let Some(estimated_size_length) = render_table.estimated_size_length
            && line_idx == render_table.column_lines() + 4
        {
            let space_count = render_width - estimated_size_length - 2;
            let lspace = space_count / 2;
            let rspace = space_count - lspace;
            print(&format!("{VERTICAL}{}", " ".repeat(lspace)), layout_type);
            print(&render_table.estimated_size_text, HighlightElementType::Comment);
            print(&format!("{}{VERTICAL}", " ".repeat(rspace)), layout_type);
            return;
        }
        // The bottom line.
        print(
            &format!("{LDCORNER}{}{RDCORNER}", HORIZONTAL.repeat(render_width - 2)),
            layout_type,
        );
    }
}

/// The tables of one database and schema rendered together (`TableMetadataDisplayInfo`).
#[derive(Clone, Debug, Default)]
struct TableMetadataDisplayInfo {
    database_name: String,
    schema_name: String,
    render_height: usize,
    render_width: usize,
    display_lines: Vec<TableMetadataLine>,
}

/// `RenderLineDisplay`: a database or schema name between horizontal lines.
fn render_line_display(
    highlight: &ShellHighlight<'_>,
    text: &str,
    total_render_width: usize,
    element_type: HighlightElementType,
) {
    let mut text = text.to_owned();
    let mut render_size = ShellState::render_length(&text);
    ShellTableRenderInfo::truncate_value_if_required(&mut text, &mut render_size, total_render_width - 4);
    let total_lines = total_render_width - render_size - 4;
    let lline = total_lines / 2;
    let rline = total_lines - lline;
    let line = format!(
        " {} {text} {} \n",
        HORIZONTAL.repeat(lline),
        HORIZONTAL.repeat(rline)
    );
    highlight.print_text(&line, PrintOutput::Stdout, element_type);
}

impl ShellState {
    /// `RenderTableMetadata`.
    pub fn render_table_metadata(&mut self, tables: Vec<ShellTableInfo>) {
        const SPLIT_THRESHOLD: usize = 20;
        let max_render_width = self.max_render_width();
        let mut table_list: Vec<ShellTableRenderInfo> = tables
            .into_iter()
            .map(|table| ShellTableRenderInfo::new(table, None))
            .collect();
        for table in &mut table_list {
            table.truncate(max_render_width);
        }
        // Split tables of many columns into several columns of column lines.
        for table in &mut table_list {
            let column_count = table.column_renders[0].len();
            if column_count <= SPLIT_THRESHOLD {
                continue;
            }
            let max_split_count = column_count.div_ceil(SPLIT_THRESHOLD);
            let width_per_split = table.per_column_width();
            let max_splits = max_render_width / width_per_split;
            if max_splits <= 1 {
                // Too wide to split.
                continue;
            }
            let split_count = max_split_count.min(max_splits);
            let mut new_renders: Vec<ColumnRenderRow> = vec![ColumnRenderRow::new(); split_count];
            for (split_idx, column) in std::mem::take(&mut table.column_renders[0])
                .into_iter()
                .enumerate()
            {
                new_renders[split_idx % split_count].push(column);
            }
            table.column_renders = new_renders;
            table.render_width = table.render_width.max(split_count * width_per_split);
        }

        // Group the tables by database and schema.
        let mut grouped_tables: BTreeMap<String, BTreeMap<String, Vec<ShellTableRenderInfo>>> =
            BTreeMap::new();
        for entry in table_list {
            grouped_tables
                .entry(entry.table.database_name.clone())
                .or_default()
                .entry(entry.table.schema_name.clone())
                .or_default()
                .push(entry);
        }

        let mut metadata_displays: Vec<TableMetadataDisplayInfo> = Vec::new();
        for schemas in grouped_tables.values_mut() {
            for result in schemas.values_mut() {
                // From the tallest table to the smallest.
                result.sort_by_key(|table| std::cmp::Reverse(table.line_count()));
                // Place tables next to each other, greedily.
                let mut displayed_tables: HashSet<usize> = HashSet::new();
                for table_idx in 0..result.len() {
                    if !displayed_tables.insert(table_idx) {
                        continue;
                    }
                    let initial_table = &result[table_idx];
                    let initial_line = TableMetadataLine {
                        render_height: initial_table.line_count(),
                        render_width: initial_table.render_width,
                        max_component_widths: initial_table.max_component_widths.clone(),
                        tables: vec![table_idx],
                    };
                    let mut metadata_display = TableMetadataDisplayInfo {
                        database_name: initial_table.table.database_name.clone(),
                        schema_name: initial_table.table.schema_name.clone(),
                        render_height: initial_line.render_height,
                        render_width: initial_line.render_width,
                        display_lines: vec![initial_line],
                    };
                    for (next_idx, current_table) in result.iter().enumerate().skip(table_idx + 1) {
                        let render_width = current_table.render_width;
                        let render_height = current_table.line_count();
                        if render_height > metadata_display.render_height {
                            // Taller than the display: it never fits.
                            continue;
                        }
                        // Prefer adding the table below others on an existing line.
                        let mut added = false;
                        for existing_line in &mut metadata_display.display_lines {
                            if existing_line.render_height + render_height > metadata_display.render_height {
                                continue;
                            }
                            // The line stretches to fit the table. (DuckDB reads past the
                            // end of the shorter list when only one of the tables has
                            // constraints; missing components count as empty here.)
                            let component_count = existing_line
                                .max_component_widths
                                .len()
                                .max(current_table.max_component_widths.len());
                            let new_max_component_widths: Vec<usize> = (0..component_count)
                                .map(|i| {
                                    let existing =
                                        existing_line.max_component_widths.get(i).copied().unwrap_or(0);
                                    let current =
                                        current_table.max_component_widths.get(i).copied().unwrap_or(0);
                                    existing.max(current)
                                })
                                .collect();
                            let new_column_render_width = 3 + new_max_component_widths
                                .iter()
                                .map(|width| width + 1)
                                .sum::<usize>();
                            let new_rendering_width = render_width
                                .max(existing_line.render_width)
                                .max(new_column_render_width);
                            let extra_width = new_rendering_width - existing_line.render_width;
                            if metadata_display.render_width + extra_width > max_render_width {
                                continue;
                            }
                            existing_line.max_component_widths = new_max_component_widths;
                            existing_line.render_width += extra_width;
                            existing_line.render_height += render_height;
                            existing_line.tables.push(next_idx);
                            added = true;
                            break;
                        }
                        if !added && metadata_display.render_width + render_width <= max_render_width {
                            // A new line, if it fits.
                            metadata_display.render_width += render_width;
                            metadata_display.display_lines.push(TableMetadataLine {
                                render_height,
                                render_width,
                                max_component_widths: current_table.max_component_widths.clone(),
                                tables: vec![next_idx],
                            });
                            added = true;
                        }
                        if added {
                            displayed_tables.insert(next_idx);
                        }
                    }
                    metadata_display
                        .display_lines
                        .sort_by_key(|line| std::cmp::Reverse(line.render_height));
                    metadata_displays.push(metadata_display);
                }
            }
        }

        let line_count: usize = metadata_displays
            .iter()
            .map(|display| display.render_height)
            .sum();
        let pager = self.should_use_pager_for_lines(line_count) && self.setup_pager();
        // The width of the database and schema lines.
        let mut metadata_render_width = 0;
        for metadata_display in &metadata_displays {
            let mut size = ShellState::render_length(&metadata_display.database_name)
                .max(ShellState::render_length(&metadata_display.schema_name));
            size = max_render_width.min(size + 6);
            metadata_render_width = metadata_render_width.max(size).max(metadata_display.render_width);
        }
        let highlight = ShellHighlight::new(&self.out);
        let mut last_displayed_database = String::new();
        let mut last_displayed_schema = String::new();
        for metadata_display in &metadata_displays {
            // The database and schema names, when they change.
            if !metadata_display.database_name.is_empty()
                && last_displayed_database != metadata_display.database_name
            {
                render_line_display(
                    &highlight,
                    &metadata_display.database_name,
                    metadata_render_width,
                    HighlightElementType::DatabaseName,
                );
                last_displayed_database.clone_from(&metadata_display.database_name);
                last_displayed_schema.clear();
            }
            if !metadata_display.schema_name.is_empty()
                && last_displayed_schema != metadata_display.schema_name
            {
                render_line_display(
                    &highlight,
                    &metadata_display.schema_name,
                    metadata_render_width,
                    HighlightElementType::SchemaName,
                );
                last_displayed_schema.clone_from(&metadata_display.schema_name);
            }
            let group_table_list =
                &grouped_tables[&metadata_display.database_name][&metadata_display.schema_name];
            for line_idx in 0..metadata_display.render_height {
                for (table_line_idx, display_line) in metadata_display.display_lines.iter().enumerate() {
                    let is_last = table_line_idx + 1 == metadata_display.display_lines.len();
                    display_line.render_line(&highlight, group_table_list, line_idx, is_last);
                }
                highlight.print_text("\n", PrintOutput::Stdout, HighlightElementType::Layout);
            }
        }
        if pager {
            self.finish_pager();
        }
    }
}
