//! Actual C++ extension-install hint output with native base messages.
use roastedduck_common::{Error, Result};
use std::io::{self, BufRead};
fn decode(h: &str) -> Result<Vec<u8>> {
    h.as_bytes()
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
    if f.len() != 5 {
        return Err(Error::invalid_input("bad record"));
    }
    let base = decode(f[0])?;
    let actual = roastedduck_function::extension_entries::add_install_hint_bytes(
        &base,
        f[1],
        f[2] == "1",
        f[3] == "1",
    );
    if actual.as_bytes() != decode(f[4])? {
        return Err(Error::internal("native install hint differs"));
    }
    Ok(())
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut count = 0;
    for l in io::stdin().lock().lines() {
        compare(&l?)?;
        count += 1;
    }
    println!("{count} C++ native install hint comparisons passed");
    Ok(())
}
