//! Actual C++ query context output through the native Error boundary.
use rustdb_common::{Error, ErrorKind, Result};
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
    let fields: Vec<_> = line.split('\t').collect();
    if fields.len() != 3 {
        return Err(Error::invalid_input("bad record"));
    }
    let query = decode(fields[0])?;
    let pos = fields[1]
        .parse::<u64>()
        .map_err(|_| Error::invalid_input("bad position"))?;
    let actual = Error::new(ErrorKind::Binder, "message")
        .with_position(Some(pos))
        .with_query_context_bytes(&query);
    if actual.raw_message_bytes() != decode(fields[2])? {
        return Err(Error::internal(format!(
            "query context mismatch position {pos} query {}",
            fields[0]
        )));
    }
    Ok(())
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (mut passed, mut failed) = (0, 0);
    for line in io::stdin().lock().lines() {
        match compare(&line?) {
            Ok(()) => passed += 1,
            Err(e) => {
                failed += 1;
                eprintln!("{e}");
            }
        }
    }
    println!("passed={passed} failed={failed}");
    if failed > 0 {
        return Err("query context differs".into());
    }
    Ok(())
}
