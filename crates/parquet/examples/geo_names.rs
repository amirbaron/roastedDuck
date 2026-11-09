//! Native `GeoParquet` names against actual C++ JSON construction/error boundaries.
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_parquet::geo::{GeoParquetMetadata, GeoParquetVersion, GeoStats, geometry_type};
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
pub fn compare(line: &str) -> Result<()> {
    let f: Vec<_> = line.split('\t').collect();
    if f.len() != 3 {
        return Err(Error::invalid_input("invalid GeoParquet record"));
    }
    let name = ByteString::from(decode(f[0])?);
    let mut geo = GeoParquetMetadata::default();
    geo.add_stats(
        &name,
        &geometry_type(""),
        &GeoStats::default(),
        GeoParquetVersion::V1,
    )?;
    let result = geo.to_json(GeoParquetVersion::V1);
    match result {
        Ok(Some(json)) if f[1] == "ok" => {
            let actual: serde_json::Value =
                serde_json::from_str(&json).map_err(|_| Error::internal("Rust GeoParquet invalid JSON"))?;
            let expected: serde_json::Value = serde_json::from_slice(&decode(f[2])?)
                .map_err(|_| Error::internal("C++ GeoParquet invalid JSON"))?;
            if actual != expected {
                return Err(Error::internal(format!(
                    "GeoParquet JSON differs for {}: {actual:?} vs {expected:?}",
                    f[0]
                )));
            }
            Ok(())
        }
        Err(error) if f[1] == "error" && error.raw_message_bytes() == decode(f[2])? => Ok(()),
        result => Err(Error::internal(format!(
            "GeoParquet outcome differs for {}: {result:?}",
            f[0]
        ))),
    }
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ GeoParquet name JSON/error comparisons passed");
    Ok(())
}
