use std::io::{BufWriter, Write};
use std::sync::{Arc, Mutex};

use roastedduck_common::{ByteString, Error, Result};
use roastedduck_function::cast::ValueCast;
use roastedduck_function::copy::{CopyFunctionExecutionMode, CopyGlobalState, CopyOption, CopyOptionMode};
use roastedduck_function::file_system;
use roastedduck_function::registered_files::Output;
use roastedduck_function::{CopyFunction, FunctionClient, FunctionData};
use roastedduck_planner::ClientContext;
use roastedduck_types::{LogicalType, LogicalTypeId, Value};
use zip::write::{SimpleFileOptions, StreamWriter};
use zip::{CompressionMethod, ZipWriter};

use super::{EXCEL_EPOCH, MAX_COLUMNS, MAX_ROWS, MICROS_PER_DAY, column_name, escaped_code_unit, io_error};

const NS_SHEET: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const NS_DOC_RELS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const NS_PKG_RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const NS_TYPES: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const NS_CONTENT: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml";

#[derive(Debug)]
struct WriteData {
    client: Arc<FunctionClient>,
    names: Vec<ByteString>,
    sheet: String,
    limit: usize,
    header: bool,
}

impl FunctionData for WriteData {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

type Archive = ZipWriter<StreamWriter<BufWriter<Output>>>;

struct Writer {
    archive: Archive,
    names: Vec<String>,
    rows: usize,
    limit: usize,
}

fn xml(text: &str) -> String {
    let mut escaped = String::new();
    for (offset, ch) in text.char_indices() {
        match ch {
            '_' if escaped_code_unit(&text[offset..]).is_some() => escaped.push_str("_x005F_"),
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            '\r' => escaped.push_str("&#13;"),
            '\0'..='\u{8}' | '\u{b}'..='\u{c}' | '\u{e}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}' => {
                use std::fmt::Write as _;
                let _ = write!(escaped, "_x{:04X}_", u32::from(ch));
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(true)
}

fn part(archive: &mut Archive, path: &str, content: &str) -> Result<()> {
    archive.start_file(path, options()).map_err(io_error)?;
    archive.write_all(content.as_bytes()).map_err(io_error)
}

impl Writer {
    fn new(data: &WriteData, path: &[u8]) -> Result<Self> {
        let _opener = file_system::activate_opener(data.client.clone());
        let mut archive = ZipWriter::new_stream(BufWriter::new(Output::create(path)?));
        part(
            &mut archive,
            "[Content_Types].xml",
            &format!(
                r#"<Types xmlns="{NS_TYPES}">
                <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
                <Default Extension="xml" ContentType="application/xml"/>
                <Override PartName="/xl/workbook.xml" ContentType="{NS_CONTENT}.sheet.main+xml"/>
                <Override PartName="/xl/styles.xml" ContentType="{NS_CONTENT}.styles+xml"/>
                <Override PartName="/xl/worksheets/sheet1.xml" ContentType="{NS_CONTENT}.worksheet+xml"/>
            </Types>"#
            ),
        )?;
        part(
            &mut archive,
            "_rels/.rels",
            &format!(
                r#"<Relationships xmlns="{NS_PKG_RELS}">
                <Relationship Id="rId1" Type="{NS_DOC_RELS}/officeDocument" Target="xl/workbook.xml"/>
            </Relationships>"#
            ),
        )?;
        part(
            &mut archive,
            "xl/workbook.xml",
            &format!(
                r#"<workbook xmlns="{NS_SHEET}" xmlns:r="{NS_DOC_RELS}">
                <sheets><sheet name="{}" sheetId="1" r:id="rId1"/></sheets>
            </workbook>"#,
                xml(&data.sheet)
            ),
        )?;
        part(
            &mut archive,
            "xl/_rels/workbook.xml.rels",
            &format!(
                r#"<Relationships xmlns="{NS_PKG_RELS}">
                <Relationship Id="rId1" Type="{NS_DOC_RELS}/worksheet" Target="worksheets/sheet1.xml"/>
                <Relationship Id="rId2" Type="{NS_DOC_RELS}/styles" Target="styles.xml"/>
            </Relationships>"#
            ),
        )?;
        part(
            &mut archive,
            "xl/styles.xml",
            &format!(
                r#"<styleSheet xmlns="{NS_SHEET}">
                <numFmts count="2"><numFmt numFmtId="164" formatCode="yyyy-mm-dd hh:mm:ss.000"/>
                <numFmt numFmtId="165" formatCode="hh:mm:ss.000"/></numFmts>
                <fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts>
                <fills count="2"><fill><patternFill patternType="none"/></fill>
                <fill><patternFill patternType="gray125"/></fill></fills>
                <borders count="1"><border/></borders>
                <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
                <cellXfs count="4">
                    <xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
                    <xf numFmtId="14" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
                    <xf numFmtId="165" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
                    <xf numFmtId="164" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
                </cellXfs>
                <cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles>
            </styleSheet>"#
            ),
        )?;
        archive
            .start_file("xl/worksheets/sheet1.xml", options())
            .map_err(io_error)?;
        write!(archive, r#"<worksheet xmlns="{NS_SHEET}"><sheetData>"#).map_err(io_error)?;
        let mut writer = Self {
            archive,
            names: (0..data.names.len()).map(column_name).collect(),
            rows: 0,
            limit: data.limit,
        };
        if data.header {
            let names = data
                .names
                .iter()
                .map(|name| Value::try_varchar(name.as_bytes()))
                .collect::<Result<Vec<_>>>()?;
            writer.row(names)?;
        }
        Ok(writer)
    }

    fn row(&mut self, values: impl IntoIterator<Item = Value>) -> Result<()> {
        self.rows += 1;
        let row = self.rows;
        if row > self.limit {
            let limit = self.limit;
            let note = if limit == MAX_ROWS {
                "\n * XLSX files and compatible applications generally have a limit of '1048576' rows\n * You can export larger sheets at your own risk by setting the 'sheet_row_limit' parameter to a higher value"
            } else {
                ""
            };
            return Err(Error::invalid_input(format!(
                "XLSX: Sheet row limit of '{limit}' rows exceeded!{note}"
            )));
        }
        write!(self.archive, r#"<row r="{row}">"#).map_err(io_error)?;
        for (col, value) in values.into_iter().enumerate() {
            let reference = format!("{}{row}", self.names[col]);
            write_cell(&mut self.archive, &reference, &value)?;
        }
        self.archive.write_all(b"</row>").map_err(io_error)
    }

    fn finish(mut self) -> Result<()> {
        self.archive
            .write_all(b"</sheetData></worksheet>")
            .map_err(io_error)?;
        let mut output = self.archive.finish().map_err(io_error)?.into_inner();
        output.flush().map_err(io_error)?;
        output.get_mut().close().map_err(io_error)
    }
}

fn write_cell(writer: &mut impl Write, reference: &str, value: &Value) -> Result<()> {
    use LogicalTypeId as Id;
    let ty = value.logical_type();
    if value.is_null() {
        return Ok(());
    }
    let numeric = match ty.id() {
        Id::Boolean => {
            let number = u8::from(value.as_bool().unwrap_or_default());
            return write!(writer, r#"<c r="{reference}" t="b"><v>{number}</v></c>"#).map_err(io_error);
        }
        Id::Date => value
            .as_date()
            .filter(|date| date.is_finite())
            .map(|date| (1, f64::from(date.0) + EXCEL_EPOCH)),
        Id::Time | Id::TimeNs | Id::TimeTz => value
            .try_cast(&LogicalType::TIME)?
            .as_time()
            .map(|time| (2, time.0 as f64 / MICROS_PER_DAY as f64)),
        Id::Timestamp | Id::TimestampTz | Id::TimestampSec | Id::TimestampMs | Id::TimestampNs => value
            .try_cast(&LogicalType::TIMESTAMP)?
            .as_timestamp()
            .filter(|timestamp| timestamp.is_finite())
            .map(|timestamp| (3, timestamp.0 as f64 / MICROS_PER_DAY as f64 + EXCEL_EPOCH)),
        _ if ty.is_numeric() => {
            let text = value.to_varchar()?;
            if text.parse::<f64>().is_ok_and(f64::is_finite) {
                return write!(writer, r#"<c r="{reference}"><v>{text}</v></c>"#).map_err(io_error);
            }
            None
        }
        _ => None,
    };
    if let Some((style, number)) = numeric {
        write!(writer, r#"<c r="{reference}" s="{style}"><v>{number}</v></c>"#)
    } else {
        let text = xml(&value.to_varchar()?);
        write!(
            writer,
            r#"<c r="{reference}" t="inlineStr"><is><t xml:space="preserve">{text}</t></is></c>"#
        )
    }
    .map_err(io_error)
}

fn writer(state: &CopyGlobalState) -> Result<std::sync::MutexGuard<'_, Option<Writer>>> {
    state
        .downcast_ref::<Mutex<Option<Writer>>>()
        .ok_or_else(|| Error::internal("Missing XLSX writer"))?
        .lock()
        .map_err(|_| Error::internal("XLSX writer lock poisoned"))
}

pub(super) fn copy_function() -> CopyFunction {
    let mut function = CopyFunction::new("xlsx");
    function.extension = "xlsx".to_owned();
    for (name, ty) in super::read::options() {
        let mode = match name {
            "sheet" | "header" => CopyOptionMode::ReadWrite,
            _ => CopyOptionMode::ReadOnly,
        };
        function
            .options
            .push((name.to_owned(), CopyOption::new(ty, mode)));
    }
    function.options.push((
        "sheet_row_limit".to_owned(),
        CopyOption::new(LogicalType::INTEGER, CopyOptionMode::WriteOnly),
    ));
    function.copy_to_bind = Some(Arc::new(|input, names, _| {
        if names.len() > MAX_COLUMNS {
            return Err(Error::invalid_input(format!(
                "XLSX: cannot write more than {MAX_COLUMNS} columns"
            )));
        }
        let first = |key: &str| input.options.get(key).and_then(|values| values.first());
        let sheet = first("sheet").map_or_else(|| Ok("Sheet1".to_owned()), Value::to_varchar)?;
        if sheet.is_empty()
            || sheet.encode_utf16().count() > 31
            || sheet.contains(['[', ']', ':', '*', '?', '/', '\\'])
            || sheet.starts_with('\'')
            || sheet.ends_with('\'')
            || sheet.chars().any(char::is_control)
        {
            return Err(Error::invalid_input(format!(
                "XLSX: invalid sheet name '{sheet}'"
            )));
        }
        let limit = first("sheet_row_limit").map_or(Ok(MAX_ROWS), |value| {
            value
                .as_i128()
                .and_then(|value| usize::try_from(value).ok())
                .filter(|limit| *limit > 0)
                .ok_or_else(|| Error::invalid_input("XLSX: sheet_row_limit must be a positive integer"))
        })?;
        let header = input
            .options
            .get("header")
            .is_some_and(|values| values.first().is_none_or(|value| value.as_bool() == Some(true)));
        let client = input
            .binder
            .and_then(|binder| binder.downcast_ref::<ClientContext>())
            .map(|context| context.function_client.clone())
            .ok_or_else(|| Error::internal("XLSX COPY requires a client context"))?;
        Ok(Arc::new(WriteData {
            client,
            names: names.to_vec(),
            sheet,
            limit,
            header,
        }))
    }));
    function.copy_to_initialize_global = Some(Arc::new(|bind, path| {
        let data = bind
            .as_any()
            .downcast_ref::<WriteData>()
            .ok_or_else(|| Error::internal("Missing XLSX write data"))?;
        Ok(Box::new(Mutex::new(Some(Writer::new(data, path)?))))
    }));
    function.copy_to_sink = Some(Arc::new(|_, state, chunk| {
        let mut guard = writer(state)?;
        let output = guard
            .as_mut()
            .ok_or_else(|| Error::internal("XLSX output is closed"))?;
        for row in 0..chunk.len() {
            output.row(chunk.columns().iter().map(|column| column.get_value(row)))?;
        }
        Ok(())
    }));
    function.copy_to_finalize = Some(Arc::new(|_, state| {
        if let Some(output) = writer(state)?.take() {
            output.finish()?;
        }
        Ok(())
    }));
    function.execution_mode = Some(Arc::new(|_, _, _| CopyFunctionExecutionMode::RegularCopyToFile));
    function.copy_from_bind = Some(Arc::new(|input, _, types| {
        let (_, _, data) = super::read::bind(
            input.file_path,
            |key| {
                input
                    .options
                    .get(key)
                    .map(|values| values.first().cloned().unwrap_or_else(|| Value::boolean(true)))
            },
            Some(types),
        )?;
        Ok(data)
    }));
    function.copy_from_function = Some(super::read::reader());
    function
}
