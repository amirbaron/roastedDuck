//! Development differential helper: `algorithm<TAB>input hex` on stdin, output hex on stdout.
use std::io::{self, BufRead, Write};

fn decode(hex: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if !hex.len().is_multiple_of(2) {
        return Err("odd hex length".into());
    }
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = char::from(pair[0]).to_digit(16).ok_or("bad hex")?;
            let low = char::from(pair[1]).to_digit(16).ok_or("bad hex")?;
            Ok((high * 16 + low) as u8)
        })
        .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut output = io::BufWriter::new(io::stdout().lock());
    for line in io::stdin().lock().lines() {
        let line = line?;
        let (algorithm, word) = line.split_once('\t').ok_or("expected algorithm and hex input")?;
        let word = decode(word)?;
        match roastedduck_snowball::stem(&word, algorithm.as_bytes()) {
            Ok(stemmed) => {
                for byte in stemmed {
                    write!(output, "{byte:02X}")?;
                }
            }
            Err(error) => {
                write!(output, "ERROR ")?;
                for byte in error.to_string().as_bytes() {
                    write!(output, "{byte:02X}")?;
                }
            }
        }
        writeln!(output)?;
    }
    Ok(())
}
