//! Compares actual C++ JSON reader/COPY option outcomes, retaining native keys.
use rustdb_common::{Error, Result};
use rustdb_json::json_reader_options::JsonReaderOptions;
use rustdb_types::Value;
use std::io::{self, BufRead};
fn decode(hex: &str) -> Result<Vec<u8>> {
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| {
            u8::from_str_radix(
                std::str::from_utf8(p).map_err(|_| Error::invalid_input("bad hex"))?,
                16,
            )
            .map_err(|_| Error::invalid_input("bad hex"))
        })
        .collect()
}
pub fn compare(line: &str) -> Result<()> {
    let f: Vec<_> = line.split('\t').collect();
    if f.len() != 3 {
        return Err(Error::invalid_input("bad record"));
    }
    let key = decode(f[1])?;
    let mut options = JsonReaderOptions::default();
    let actual = match f[0] {
        "null" => options.parse_option(None, &key, &Value::null(rustdb_types::LogicalType::SQLNULL)),
        "bool" => options.parse_option(None, &key, &Value::boolean(true)),
        "copy0" => options.parse_copy_option(&key, &[]),
        "copy1" => options.parse_copy_option(&key, &[Value::boolean(true)]),
        "copy2" => options.parse_copy_option(&key, &[Value::boolean(true), Value::boolean(false)]),
        _ => return Err(Error::invalid_input("bad mode")),
    };
    let matches = match actual {
        Ok(accepted) => {
            f[2] == format!(
                "OK:{}:{}:{}:{}",
                u8::from(accepted),
                u8::from(options.ignore_errors),
                u8::from(options.auto_detect),
                options.format as u8
            )
        }
        Err(e) => f[2]
            .strip_prefix("ERR:")
            .is_some_and(|expected| decode(expected).is_ok_and(|v| v == e.into_json().to_bytes())),
    };
    if !matches {
        return Err(Error::internal(format!(
            "JSON option outcome differs: {} {}",
            f[0], f[1]
        )));
    }
    Ok(())
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        compare(&line?)?;
        count += 1;
    }
    println!("{count} C++ native JSON option comparisons passed");
    Ok(())
}
