use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek};
use std::sync::Arc;

use roastedduck_common::{ByteString, Error, ErrorKind, Result};
use roastedduck_function::cast::ValueCast;
use roastedduck_function::file_system;
use roastedduck_function::registered_files::{self, Cursor};
use roastedduck_function::{FunctionData, TableFunction, TableFunctionBindOutput, TableFunctionSet};
use roastedduck_types::{
    DataChunk, Date, LogicalType, LogicalTypeId, STANDARD_VECTOR_SIZE, Time, Timestamp, Value,
};
use roxmltree::{Document, Node, ParsingOptions};
use zip::ZipArchive;

use super::{EXCEL_EPOCH, MAX_COLUMNS, MAX_ROWS, MICROS_PER_DAY, column_name, decode_excel_string, io_error};

pub(super) fn options() -> [(&'static str, LogicalType); 8] {
    [
        ("sheet", LogicalType::VARCHAR),
        ("range", LogicalType::VARCHAR),
        ("header", LogicalType::BOOLEAN),
        ("all_varchar", LogicalType::BOOLEAN),
        ("stop_at_empty", LogicalType::BOOLEAN),
        ("ignore_errors", LogicalType::BOOLEAN),
        ("empty_as_varchar", LogicalType::BOOLEAN),
        ("normalize_names", LogicalType::BOOLEAN),
    ]
}

struct CellRange {
    first_row: usize,
    last_row: Option<usize>,
    first_col: usize,
    last_col: usize,
}

fn address(value: &str) -> Option<(Option<usize>, Option<usize>)> {
    let delimiter = value
        .bytes()
        .position(|byte| !byte.is_ascii_alphabetic())
        .unwrap_or(value.len());
    let (letters, digits) = value.split_at(delimiter);
    let col = if letters.is_empty() {
        None
    } else {
        let number = letters.bytes().try_fold(0_usize, |number, letter| {
            number
                .checked_mul(26)?
                .checked_add(usize::from(letter.to_ascii_uppercase() - b'A' + 1))
        })?;
        Some(number.checked_sub(1).filter(|col| *col < MAX_COLUMNS)?)
    };
    let row = if digits.is_empty() {
        None
    } else {
        if !digits.bytes().all(|digit| digit.is_ascii_digit()) {
            return None;
        }
        Some(
            digits
                .parse::<usize>()
                .ok()
                .filter(|row| (1..=MAX_ROWS).contains(row))?,
        )
    };
    Some((col, row))
}

impl CellRange {
    fn parse(value: &str) -> Result<Self> {
        let parse = || {
            let (first, last) = value.split_once(':')?;
            let (first_col, first_row) = address(first)?;
            let (last_col, last_row) = address(last)?;
            let range = Self {
                first_col: first_col.unwrap_or(0),
                first_row: first_row.unwrap_or(1),
                last_col: last_col?,
                last_row,
            };
            (range.first_col <= range.last_col && range.last_row.is_none_or(|last| last >= range.first_row))
                .then_some(range)
        };
        parse().ok_or_else(|| Error::binder(format!("Invalid range '{value}' specified")))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CellKind {
    Numeric,
    Text,
    FormulaText,
    Boolean,
    IsoDate,
}

#[derive(Debug)]
struct Cell {
    value: Arc<str>,
    kind: CellKind,
    style: usize,
    blank: bool,
}

type Row = BTreeMap<usize, Cell>;
type Rows = BTreeMap<usize, Row>;

struct Workbook {
    rows: Rows,
    formats: Vec<LogicalType>,
    epoch: f64,
}

fn is_tag(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

fn part(source: &mut ZipArchive<impl Read + Seek>, path: &str) -> Result<Option<String>> {
    // Bound XML expansion and tree allocations even if the ZIP directory lies about the size.
    const MAX_PART_BYTES: u64 = 128 * 1024 * 1024;
    let entry = match source.by_name(path) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    let mut xml = String::new();
    entry
        .take(MAX_PART_BYTES + 1)
        .read_to_string(&mut xml)
        .map_err(io_error)?;
    if xml.len() as u64 > MAX_PART_BYTES {
        return Err(io_error(format!("XML part '{path}' exceeds 128 MiB")));
    }
    Ok(Some(xml))
}

fn required_part(source: &mut ZipArchive<impl Read + Seek>, path: &str) -> Result<String> {
    part(source, path)?.ok_or_else(|| Error::binder(format!("No {path} found in xlsx file")))
}

fn parse_xml(xml: &str) -> Result<Document<'_>> {
    Document::parse_with_options(
        xml,
        ParsingOptions {
            nodes_limit: 5_000_000,
            ..Default::default()
        },
    )
    .map_err(io_error)
}

fn rich_text(node: Node<'_, '_>) -> String {
    let text: String = node
        .children()
        .filter(|child| is_tag(*child, "t") || is_tag(*child, "r"))
        .flat_map(|child| {
            let text = if is_tag(child, "r") {
                child.children().find(|text| is_tag(*text, "t"))
            } else {
                Some(child)
            };
            text.and_then(|text| text.text())
        })
        .collect();
    if text.contains("_x") {
        decode_excel_string(&text)
    } else {
        text
    }
}

fn relationship_path(target: &str) -> String {
    if let Some(absolute) = target.strip_prefix('/') {
        absolute.to_owned()
    } else {
        format!("xl/{target}")
    }
}

fn open_workbook(path: &[u8], sheet: Option<&str>) -> Result<Workbook> {
    file_system::verify_can_access_file(path)?;
    if let Some(fs) = registered_files::lookup(path) {
        return read_workbook(Cursor::new(fs.open(path, false)?), path, sheet);
    }
    let file = std::fs::File::open(file_system::native_path(path)?)
        .map_err(|error| roastedduck_csv::buffer::open_error(path, &error))?;
    read_workbook(file, path, sheet)
}

fn read_workbook(input: impl Read + Seek, path: &[u8], sheet: Option<&str>) -> Result<Workbook> {
    let mut archive = ZipArchive::new(input).map_err(io_error)?;
    required_part(&mut archive, "[Content_Types].xml")?;
    let workbook_xml = required_part(&mut archive, "xl/workbook.xml")?;
    let rels_xml = required_part(&mut archive, "xl/_rels/workbook.xml.rels")?;
    let workbook = parse_xml(&workbook_xml)?;
    let rels = parse_xml(&rels_xml)?;
    let relationships: HashMap<_, _> = rels
        .root_element()
        .children()
        .filter(|rel| is_tag(*rel, "Relationship") && rel.attribute("TargetMode") != Some("External"))
        .filter_map(|rel| {
            Some((
                rel.attribute("Id")?,
                (rel.attribute("Type")?, rel.attribute("Target")?),
            ))
        })
        .collect();
    let sheets: Vec<_> = workbook
        .descendants()
        .filter(|node| is_tag(*node, "sheet"))
        .filter_map(|node| {
            let id = node.attributes().find(|attr| attr.name() == "id")?.value();
            let (kind, target) = relationships.get(id)?;
            kind.ends_with("/worksheet")
                .then_some((node.attribute("name")?, *target))
        })
        .collect();
    if sheets.is_empty() {
        return Err(Error::binder(
            "No sheets found in xlsx file (is the file corrupt?)",
        ));
    }
    let target = if let Some(sheet) = sheet.filter(|name| !name.is_empty()) {
        sheets
            .iter()
            .find(|(name, _)| *name == sheet)
            .map(|(_, target)| *target)
            .ok_or_else(|| {
                let names: Vec<String> = sheets.iter().map(|(name, _)| (*name).to_owned()).collect();
                let suggestions = roastedduck_common::string_util::candidates_message_bytes(
                    &roastedduck_common::string_util::top_n_jaro_winkler(&names, sheet),
                    b"Did you mean",
                );
                Error::binder(roastedduck_common::byte_format!(
                    "Sheet \"{sheet}\" not found in xlsx file \"{path}\"{suggestions}"
                ))
            })?
    } else {
        sheets[0].1
    };
    let epoch = if workbook
        .descendants()
        .any(|node| is_tag(node, "workbookPr") && matches!(node.attribute("date1904"), Some("1" | "true")))
    {
        EXCEL_EPOCH - 1462.0
    } else {
        EXCEL_EPOCH
    };
    let relation = |suffix: &str, fallback: &str| {
        relationships
            .values()
            .find(|(kind, _)| kind.ends_with(suffix))
            .map_or_else(|| fallback.to_owned(), |(_, target)| relationship_path(target))
    };
    let strings: Vec<Arc<str>> =
        if let Some(xml) = part(&mut archive, &relation("/sharedStrings", "xl/sharedStrings.xml"))? {
            parse_xml(&xml)?
                .root_element()
                .children()
                .filter(|node| is_tag(*node, "si"))
                .map(|node| Arc::from(rich_text(node)))
                .collect()
        } else {
            Vec::new()
        };
    let formats = part(&mut archive, &relation("/styles", "xl/styles.xml"))?
        .map_or_else(|| Ok(Vec::new()), |xml| styles(&xml))?;
    let sheet_path = relationship_path(target);
    let worksheet = required_part(&mut archive, &sheet_path)?;
    Ok(Workbook {
        rows: sheet_rows(&worksheet, &strings)?,
        formats,
        epoch,
    })
}

fn styles(xml: &str) -> Result<Vec<LogicalType>> {
    let doc = parse_xml(xml)?;
    let custom: HashMap<_, _> = doc
        .descendants()
        .filter(|node| is_tag(*node, "numFmt"))
        .filter_map(|node| {
            Some((
                node.attribute("numFmtId")?.parse::<u32>().ok()?,
                node.attribute("formatCode")?,
            ))
        })
        .collect();
    let result = doc
        .root_element()
        .children()
        .find(|node| is_tag(*node, "cellXfs"))
        .into_iter()
        .flat_map(|node| node.children())
        .filter(|node| is_tag(*node, "xf"))
        .map(|node| {
            let id = node
                .attribute("numFmtId")
                .and_then(|id| id.parse::<u32>().ok())
                .unwrap_or(0);
            custom.get(&id).map_or_else(
                || match id {
                    14..=17 | 27..=31 | 34..=36 | 50..=58 => LogicalType::DATE,
                    18..=21 | 32..=33 | 45..=47 => LogicalType::TIME,
                    22 => LogicalType::TIMESTAMP,
                    _ => LogicalType::DOUBLE,
                },
                |code| format_type(code),
            )
        })
        .collect();
    Ok(result)
}

fn format_type(code: &str) -> LogicalType {
    if code.eq_ignore_ascii_case("\"TRUE\";\"TRUE\";\"FALSE\"") {
        return LogicalType::BOOLEAN;
    }
    // Date/time fields occur outside literals and decorations.
    let mut chars = code.chars();
    let mut date = false;
    let mut time = false;
    let mut month = false;
    while let Some(ch) = chars.next() {
        match ch.to_ascii_lowercase() {
            '"' => {
                chars.by_ref().find(|ch| *ch == '"');
            }
            '\\' | '_' | '*' => {
                chars.next();
            }
            '[' => {
                let token: String = chars.by_ref().take_while(|ch| *ch != ']').collect();
                time |= matches!(
                    token.to_ascii_lowercase().as_str(),
                    "h" | "hh" | "m" | "mm" | "s" | "ss"
                );
            }
            ';' => break,
            'd' | 'y' => date = true,
            'h' | 's' => time = true,
            'm' => month = true,
            _ => {}
        }
    }
    match (date || month && !time, time) {
        (true, true) => LogicalType::TIMESTAMP,
        (true, false) => LogicalType::DATE,
        (false, true) => LogicalType::TIME,
        (false, false) => LogicalType::DOUBLE,
    }
}

fn sheet_rows(xml: &str, strings: &[Arc<str>]) -> Result<Rows> {
    let doc = parse_xml(xml)?;
    let data = doc
        .root_element()
        .children()
        .find(|node| is_tag(*node, "sheetData"))
        .ok_or_else(|| io_error("Worksheet has no sheetData"))?;
    let mut rows = Rows::new();
    let mut row_index = 0;
    for row in data.children().filter(|node| is_tag(*node, "row")) {
        row_index = row.attribute("r").map_or_else(
            || Ok(row_index + 1),
            |index| index.parse::<usize>().map_err(io_error),
        )?;
        if row_index == 0 || row_index > MAX_ROWS {
            return Err(io_error("Invalid worksheet row index"));
        }
        let mut cells = Row::new();
        let mut next_column = 0;
        for node in row.children().filter(|node| is_tag(*node, "c")) {
            let col = if let Some(reference) = node.attribute("r") {
                address(reference)
                    .filter(|(_, row)| *row == Some(row_index))
                    .and_then(|(col, _)| col)
                    .ok_or_else(|| io_error(format!("Invalid cell reference '{reference}'")))?
            } else {
                next_column
            };
            if col >= MAX_COLUMNS {
                return Err(io_error("Invalid worksheet column index"));
            }
            next_column = col + 1;
            let cell_type = node.attribute("t").unwrap_or("n");
            let raw = node
                .children()
                .find(|child| is_tag(*child, "v"))
                .and_then(|value| value.text())
                .unwrap_or("");
            let (kind, value, blank) = match cell_type {
                "s" => {
                    let value = raw
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| strings.get(index))
                        .ok_or_else(|| io_error(format!("Invalid shared string index '{raw}'")))?;
                    (CellKind::Text, Arc::clone(value), false)
                }
                "inlineStr" => {
                    let value = node
                        .children()
                        .find(|child| is_tag(*child, "is"))
                        .map_or_else(String::new, rich_text);
                    (CellKind::Text, value.into(), false)
                }
                "str" | "e" => (CellKind::FormulaText, Arc::from(raw), raw.is_empty()),
                "b" => (CellKind::Boolean, Arc::from(raw), raw.is_empty()),
                "d" => (CellKind::IsoDate, Arc::from(raw), raw.is_empty()),
                _ => (CellKind::Numeric, Arc::from(raw), raw.is_empty()),
            };
            let style = node
                .attribute("s")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            cells.insert(
                col,
                Cell {
                    value,
                    kind,
                    style,
                    blank,
                },
            );
        }
        rows.insert(row_index, cells);
    }
    Ok(rows)
}

fn infer_range(rows: &Rows) -> Result<CellRange> {
    for (&row, cells) in rows {
        let mut values = cells
            .iter()
            .skip_while(|(_, cell)| cell.blank || cell.value.is_empty());
        if let Some((&first, _)) = values.next() {
            let mut last = first;
            for (&col, cell) in values {
                if cell.blank || cell.value.is_empty() {
                    break;
                }
                last = col;
            }
            return Ok(CellRange {
                first_row: row,
                last_row: None,
                first_col: first,
                last_col: last,
            });
        }
    }
    Err(Error::binder("No rows found in xlsx file"))
}

#[derive(Debug)]
struct ReadData {
    rows: Rows,
    first_row: usize,
    last_row: usize,
    first_col: usize,
    epoch: f64,
    types: Vec<LogicalType>,
    ignore_errors: bool,
}

impl FunctionData for ReadData {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn infer_type(cell: Option<&Cell>, formats: &[LogicalType], empty_as_varchar: bool) -> LogicalType {
    let Some(cell) = cell else {
        return if empty_as_varchar {
            LogicalType::VARCHAR
        } else {
            LogicalType::DOUBLE
        };
    };
    match cell.kind {
        CellKind::Text | CellKind::FormulaText
            if cell.value.eq_ignore_ascii_case("true") || cell.value.eq_ignore_ascii_case("false") =>
        {
            LogicalType::BOOLEAN
        }
        CellKind::Text | CellKind::FormulaText => LogicalType::VARCHAR,
        CellKind::Boolean => LogicalType::BOOLEAN,
        CellKind::IsoDate => LogicalType::DATE,
        CellKind::Numeric => formats.get(cell.style).cloned().unwrap_or(LogicalType::DOUBLE),
    }
}

pub(super) fn bind(
    path: &[u8],
    option: impl Fn(&str) -> Option<Value>,
    expected: Option<&[LogicalType]>,
) -> Result<(Vec<ByteString>, Vec<LogicalType>, Arc<dyn FunctionData>)> {
    let opt_bool = |key| option(key).and_then(|v| v.as_bool());
    let opt_str = |key| {
        option(key)
            .filter(|v| !v.is_null())
            .map(|v| v.to_varchar())
            .transpose()
    };
    let explicit = opt_str("range")?
        .map(|value| CellRange::parse(&value))
        .transpose()?;
    let stop_at_empty = opt_bool("stop_at_empty").unwrap_or(explicit.is_none());
    let files = roastedduck_csv::multi_file::local_glob(path)?;
    let path = files
        .first()
        .ok_or_else(|| {
            Error::new_bytes(
                ErrorKind::Io,
                roastedduck_common::byte_format!("Cannot open file \"{path}\": No such file or directory"),
            )
        })?
        .as_bytes();
    let Workbook { rows, formats, epoch } = open_workbook(path, opt_str("sheet")?.as_deref())?;
    let is_explicit = explicit.is_some();
    let range = explicit.map_or_else(|| infer_range(&rows), Ok)?;
    let columns = range.first_col..=range.last_col;
    let sniffed = rows
        .range(range.first_row..=range.last_row.unwrap_or(MAX_ROWS))
        .next();
    let first = sniffed.map(|(_, cells)| cells);
    fn cell_at(row: Option<&Row>, col: usize) -> Option<&Cell> {
        row.and_then(|row| row.get(&col))
    }
    let header = opt_bool("header").unwrap_or_else(|| {
        columns.clone().all(|col| {
            cell_at(first, col).is_some_and(|cell| cell.kind == CellKind::Text && !cell.value.is_empty())
        })
    });
    let first_row = if header {
        sniffed.map_or(range.first_row, |(&row, _)| row) + 1
    } else {
        range.first_row
    };
    let sample = rows
        .range(first_row..=range.last_row.unwrap_or(MAX_ROWS).max(first_row))
        .next()
        .map(|(_, row)| row);
    let mut last_row = range
        .last_row
        .unwrap_or_else(|| rows.last_key_value().map_or(0, |(&row, _)| row));
    if stop_at_empty {
        for row in first_row..=last_row {
            let has_data = rows.get(&row).is_some_and(|cells| {
                cells
                    .range(columns.clone())
                    .any(|(_, cell)| !cell.blank && !cell.value.is_empty())
            });
            if !has_data {
                last_row = row - 1;
                break;
            }
        }
    }
    let mut names: Vec<ByteString> = columns
        .clone()
        .map(|col| {
            let name = cell_at(first, col)
                .filter(|cell| header && !cell.blank)
                .map_or("", |cell| cell.value.trim_matches(|ch: char| ch.is_whitespace()));
            if name.is_empty() {
                let column = column_name(col);
                if is_explicit {
                    column.into()
                } else {
                    format!("{column}{}", range.first_row).into()
                }
            } else if opt_bool("normalize_names") == Some(true) {
                roastedduck_csv::sniffer::normalize_column_name(name).into()
            } else {
                name.into()
            }
        })
        .collect();
    roastedduck_planner::deduplicate_columns(&mut names);
    let types: Vec<_> = if let Some(expected) = expected {
        if expected.len() != names.len() {
            return Err(Error::new(
                ErrorKind::Conversion,
                format!(
                    "Failed to read file(s) \"{}\" - column count mismatch: expected {} columns but found {}",
                    String::from_utf8_lossy(path),
                    expected.len(),
                    names.len()
                ),
            ));
        }
        expected.to_vec()
    } else {
        columns
            .map(|col| {
                if opt_bool("all_varchar") == Some(true) {
                    LogicalType::VARCHAR
                } else {
                    infer_type(
                        cell_at(sample, col),
                        &formats,
                        opt_bool("empty_as_varchar") == Some(true),
                    )
                }
            })
            .collect()
    };
    let data = ReadData {
        rows,
        first_row,
        last_row,
        first_col: range.first_col,
        epoch,
        types: types.clone(),
        ignore_errors: opt_bool("ignore_errors") == Some(true),
    };
    Ok((names, types, Arc::new(data)))
}

fn convert(cell: &Cell, ty: &LogicalType, epoch: f64) -> Result<Value> {
    if cell.blank {
        return Ok(Value::null(ty.clone()));
    }
    if cell.kind == CellKind::Numeric
        && matches!(
            ty.id(),
            LogicalTypeId::Date | LogicalTypeId::Time | LogicalTypeId::Timestamp
        )
    {
        let number = cell
            .value
            .parse::<f64>()
            .map_err(|error| Error::invalid_input(error.to_string()))?;
        let days = if ty.id() == LogicalTypeId::Time {
            number.rem_euclid(1.0)
        } else {
            number - epoch
        };
        if !days.is_finite() || days.abs() >= (i64::MAX as f64 / MICROS_PER_DAY as f64) {
            return Err(Error::invalid_input("Excel serial date is out of range"));
        }
        let millis = (days * (MICROS_PER_DAY / 1000) as f64).round() as i64;
        let micros = millis
            .checked_mul(1000)
            .ok_or_else(|| Error::invalid_input("Excel serial date is out of range"))?;
        return Ok(match ty.id() {
            LogicalTypeId::Date => Value::date(Date(micros.div_euclid(MICROS_PER_DAY) as i32)),
            LogicalTypeId::Time => Value::time(Time(micros.rem_euclid(MICROS_PER_DAY))),
            _ => Value::timestamp(Timestamp(micros)),
        });
    }
    Value::varchar(&cell.value).try_cast(ty)
}

pub(super) fn reader() -> TableFunction {
    let mut function = TableFunction::new("read_xlsx", vec![LogicalType::VARCHAR])
        .with_bind(|input| {
            let path = input.inputs[0].to_varchar_bytes()?;
            let (names, return_types, data) = bind(
                &path,
                |key| {
                    input
                        .named_parameters
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case(key.as_bytes()))
                        .map(|(_, value)| value.clone())
                },
                None,
            )?;
            Ok(TableFunctionBindOutput {
                names,
                return_types,
                bind_data: Some(data),
            })
        })
        .with_scan(|bind, _, state, output| {
            let data = bind
                .and_then(|data| data.as_any().downcast_ref::<ReadData>())
                .ok_or_else(|| Error::internal("Missing XLSX read data"))?;
            let start = data.first_row + state.position as usize;
            let count = (data.last_row + 1)
                .saturating_sub(start)
                .min(STANDARD_VECTOR_SIZE);
            if count == 0 {
                output.set_len(0);
                return Ok(false);
            }
            *output = DataChunk::with_types(&data.types, count);
            for (index, row) in (start..start + count).enumerate() {
                let cells = data.rows.get(&row);
                for (column, ty) in data.types.iter().enumerate() {
                    let sheet_column = data.first_col + column;
                    let converted = cells.and_then(|cells| cells.get(&sheet_column)).map_or_else(
                        || Ok(Value::null(ty.clone())),
                        |cell| convert(cell, ty, data.epoch),
                    );
                    let value = match converted {
                        Ok(value) => value,
                        Err(_) if data.ignore_errors => Value::null(ty.clone()),
                        Err(error) => {
                            return Err(Error::invalid_input(format!(
                                "read_xlsx: Failed to parse cell '{}{row}': {}",
                                column_name(sheet_column),
                                error.raw_message()
                            )));
                        }
                    };
                    output.column_mut(column).set_value(index, &value)?;
                }
            }
            output.set_len(count);
            state.position += count as u64;
            Ok(true)
        });
    function.named_parameters = options()
        .into_iter()
        .map(|(name, ty)| (name.into(), ty))
        .collect();
    function
}

pub(super) fn table_function() -> TableFunctionSet {
    let mut functions = TableFunctionSet::new("read_xlsx");
    functions.add(reader());
    functions
}
