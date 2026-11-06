//! Native CSV metadata and error boundaries against DuckDB 1.5.6.
use rustdb_common::{ByteString, Error, Result};
use rustdb_csv::{
    error::{CsvError, LinesPerBoundary},
    options::CsvReaderOptions,
    schema::CsvSchema,
    sniffer::{SetColumns, SnifferResult},
};
use rustdb_types::{LogicalType, LogicalTypeId, Value};
use std::io::{self, BufRead};

fn decode(hex: &str) -> Result<Vec<u8>> {
    let (pairs, rest) = hex.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return Err(Error::invalid_input("odd hex length"));
    }
    pairs
        .iter()
        .map(
            |p| match (char::from(p[0]).to_digit(16), char::from(p[1]).to_digit(16)) {
                (Some(a), Some(b)) => Ok((a * 16 + b) as u8),
                _ => Err(Error::invalid_input("invalid hex")),
            },
        )
        .collect()
}
fn outcome(op: &str, name: ByteString, input: Option<&str>) -> Result<ByteString> {
    let mut options = CsvReaderOptions::default();
    match op {
        "columns" => {
            options.parse_option(
                "columns",
                &Value::struct_value(vec![(name, Value::varchar("INTEGER"))]),
                &|text| {
                    if text == "INTEGER" {
                        Ok(LogicalType::INTEGER)
                    } else {
                        Err(Error::invalid_input("unexpected type"))
                    }
                },
            )?;
            let mut output = options.name_list[0].clone();
            output.push(0);
            output.push_bytes(options.user_defined_parameters_string());
            Ok(output)
        }
        "named" => {
            options.name_list.push(name);
            let params = options.to_named_parameters()?;
            let names = &params
                .iter()
                .find(|(key, _)| key == "column_names")
                .ok_or_else(|| Error::internal("missing names"))?
                .1;
            Ok(names.children().ok_or_else(|| Error::internal("not a list"))?[0]
                .as_bytes()
                .ok_or_else(|| Error::internal("not a varchar"))?
                .into())
        }
        "types-error" => Ok(CsvError::column_types_error(vec![name], &[]).error_message),
        "cast-error" => Ok(CsvError::cast_error(
            &options,
            &name,
            "bad integer",
            0,
            "x".to_owned(),
            LinesPerBoundary::default(),
            0,
            None,
            LogicalTypeId::Integer,
            b"file.csv",
        )
        .full_error_message),
        "schema-error" => {
            let schema = CsvSchema::new(
                std::slice::from_ref(&name),
                &[LogicalType::INTEGER],
                b"main.csv",
                2,
                false,
            );
            let mut other = name;
            other.push_bytes("other");
            let mut result = SnifferResult {
                names: vec![other],
                return_types: vec![LogicalType::INTEGER],
            };
            let mut error = ByteString::new();
            let matches = schema.schemas_match(&mut error, &mut result, b"next.csv", None);
            let mut output = ByteString::from(if matches { "1" } else { "0" });
            output.push_bytes(error);
            Ok(output)
        }
        "read" => {
            use rustdb_csv::{
                multi_file::{FileList, MultiFileOptions},
                read_csv::{bind_csv_files, read_csv_functions},
            };
            use rustdb_function::{TableScanState, cast::CastFunctionSet};
            use rustdb_types::DataChunk;
            use std::sync::Arc;
            let path = input.ok_or_else(|| Error::invalid_input("CSV fixture path required"))?;
            options.auto_detect = false;
            options.columns_set = true;
            options.parallel = false;
            options.name_list = vec![name];
            options.sql_type_list = vec![LogicalType::INTEGER];
            options.dialect_options.header.set(false, true);
            let data = bind_csv_files(
                &Arc::new(CastFunctionSet::new()),
                FileList {
                    files: vec![path.into()],
                },
                MultiFileOptions::default(),
                options,
                None,
            )?;
            let mut output = data.names[0].clone();
            output.push(0);
            let functions = read_csv_functions();
            let scan = functions[0].functions[0]
                .scan
                .as_ref()
                .ok_or_else(|| Error::internal("missing CSV scanner"))?;
            let mut state = TableScanState::default();
            let mut chunk = DataChunk::default();
            loop {
                let more = scan(Some(&data), &[0], &mut state, &mut chunk)?;
                for row in 0..chunk.len() {
                    let value = chunk
                        .column(0)
                        .get_value(row)
                        .as_i32()
                        .ok_or_else(|| Error::internal("not an integer"))?;
                    output.push_bytes(format!("{value},"));
                }
                if !more {
                    break;
                }
            }
            Ok(output)
        }
        "set-columns" => Ok(SetColumns {
            names: vec![name],
            types: vec![LogicalType::INTEGER],
        }
        .to_display_string()),
        _ => Err(Error::invalid_input("unknown CSV oracle operation")),
    }
}
pub fn compare(line: &str, input: Option<&str>) -> Result<()> {
    let fields: Vec<_> = line.split('\t').collect();
    if fields.len() != 4 {
        return Err(Error::invalid_input("invalid CSV oracle record"));
    }
    let result = outcome(fields[0], decode(fields[1])?.into(), input);
    let (status, output): (&str, ByteString) = match result {
        Ok(bytes) => ("ok", bytes),
        Err(error) => ("error", error.raw_message_bytes().into()),
    };
    if status != fields[2] || output.as_bytes() != decode(fields[3])? {
        return Err(Error::internal(format!(
            "CSV outcome differs from C++: {} name {} ({status}, expected {}), actual {:?}, expected {:?}",
            fields[0], fields[1], fields[2], output.as_bytes(), decode(fields[3])?
        )));
    }
    Ok(())
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let input = std::env::var("CSV_NATIVE_INPUT").ok();
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        compare(&line?, input.as_deref())?;
        count += 1;
    }
    println!("{count} C++ CSV metadata/error comparisons passed");
    Ok(())
}
