//! Reads real C++ Parquet files, verifies native metadata/data, rewrites with Rust.
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_csv::multi_file::{FileList, MultiFileOptions};
use roastedduck_function::{TableScanState, cast::CastFunctionSet};
use roastedduck_parquet::{
    reader::ParquetReader,
    scan::{bind_files, read_parquet_functions},
    schema::ParquetOptions,
    writer::{ParquetWriter, WriterOptions},
};
use roastedduck_types::DataChunk;
use std::sync::Arc;
fn run(directory: &str) -> Result<()> {
    for (label, name) in [
        ("raw", ByteString::from(b"caf\xe9")),
        ("utf8", ByteString::from("café")),
    ] {
        let input = format!("{directory}/{label}-cpp.parquet");
        let output = format!("{directory}/{label}-rust.parquet");
        let reader = ParquetReader::open(input.as_bytes(), ParquetOptions::default())?;
        if reader.columns[0].name != name
            || reader.columns[1]
                .logical_type
                .struct_children()
                .ok_or_else(|| Error::internal("not a STRUCT"))?[0]
                .0
                != name
        {
            return Err(Error::internal("C++ file names changed in Rust reader"));
        }
        let casts = Arc::new(CastFunctionSet::new());
        let data = bind_files(
            &casts,
            FileList {
                files: vec![input.into()],
            },
            MultiFileOptions::default(),
            &ParquetOptions::default(),
        )?;
        let functions = read_parquet_functions();
        let scan = functions[0].functions[0]
            .scan
            .as_ref()
            .ok_or_else(|| Error::internal("missing scan"))?;
        let mut state = TableScanState::default();
        let mut chunk = DataChunk::default();
        let mut rows = 0;
        let options = WriterOptions {
            codec: roastedduck_parquet::thrift::CompressionCodec::UNCOMPRESSED,
            ..WriterOptions::default()
        };
        let mut writer = ParquetWriter::new(output.as_bytes(), &data.names, &data.types, options)?;
        loop {
            let more = scan(Some(&data), &[0, 1], &mut state, &mut chunk)?;
            for row in 0..chunk.len() {
                if chunk.column(0).get_value(row).as_i32() != Some(rows)
                    || chunk
                        .column(1)
                        .get_value(row)
                        .children()
                        .and_then(|v| v.first())
                        .and_then(roastedduck_types::Value::as_i32)
                        != Some(rows + 10)
                {
                    return Err(Error::internal("Parquet row data mismatch"));
                }
                rows += 1;
            }
            if !chunk.is_empty() {
                writer.flush(&[chunk.clone()])?;
            }
            if !more {
                break;
            }
        }
        if rows != 2 {
            return Err(Error::internal("wrong Parquet row count"));
        }
        writer.finalize()?;
        let rewritten = ParquetReader::open(output.as_bytes(), ParquetOptions::default())?;
        if rewritten.columns[0].name != name
            || rewritten.columns[1]
                .logical_type
                .struct_children()
                .ok_or_else(|| Error::internal("rewritten not STRUCT"))?[0]
                .0
                != name
        {
            return Err(Error::internal("Rust file name metadata mismatch"));
        }
        println!("{label}: native names, 2 rows, Rust rewrite/reopen passed");
    }
    Ok(())
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args().nth(1).ok_or("directory required")?;
    run(&directory)?;
    Ok(())
}
