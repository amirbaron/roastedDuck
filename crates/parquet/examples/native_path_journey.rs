//! Reads a C++ Unicode-path file, rewrites it with Rust, and reopens the result.
use roastedduck_common::{Error, Result};
use roastedduck_parquet::{
    reader::{ParquetReader, ScanColumn},
    schema::ParquetOptions,
    writer::{ParquetWriter, WriterOptions},
};
use roastedduck_types::{DataChunk, Value};
use std::path::Path;
fn read(path: &Path) -> Result<(ParquetReader, Vec<DataChunk>)> {
    let reader = ParquetReader::open(path.as_os_str().as_encoded_bytes(), ParquetOptions::default())?;
    if reader.columns[0].name != "値" {
        return Err(Error::internal("native column name changed"));
    }
    let mut scan = reader.initialize_scan(vec![0], &[ScanColumn::column(0)], Vec::new())?;
    let mut chunks = Vec::new();
    let mut values = Vec::new();
    loop {
        let mut vectors = Vec::new();
        let count = scan.scan(&mut vectors)?;
        if count == 0 {
            break;
        }
        for row in 0..count {
            values.push(vectors[0].get_value(row).as_i32());
        }
        chunks.push(DataChunk::new(vectors, count));
    }
    if values != [Some(42), Some(7)] {
        return Err(Error::internal("native path Parquet data changed"));
    }
    Ok((reader, chunks))
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let input = args.next().ok_or("input required")?;
    let output = args.next().ok_or("output required")?;
    let (reader, chunks) = read(Path::new(&input))?;
    let names = reader.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
    let types = reader
        .columns
        .iter()
        .map(|c| c.logical_type.clone())
        .collect::<Vec<_>>();
    let mut writer = ParquetWriter::new(
        output.as_encoded_bytes(),
        &names,
        &types,
        WriterOptions::default(),
    )?;
    writer.flush(&chunks)?;
    writer.finalize()?;
    drop(writer);
    read(Path::new(&output))?;
    println!(
        "C++ read, Rust rewrite/reopen: Unicode path/name and values {:?} passed",
        [Value::integer(42), Value::integer(7)]
    );
    Ok(())
}
