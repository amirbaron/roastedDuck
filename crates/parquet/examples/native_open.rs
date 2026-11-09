//! Compare actual native cache-handle open errors with C++ `LocalFileSystem`.
use roastedduck_function::{
    caching_file_system::CachingFileHandle, external_file_cache::CacheValidationMode,
};
use std::io::{self, BufRead};
fn decode(s: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if !s.len().is_multiple_of(2) {
        return Err("odd hex".into());
    }
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| Ok(u8::from_str_radix(std::str::from_utf8(p)?, 16)?))
        .collect()
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut passed, mut failed) = (0, 0);
    for (index, line) in io::stdin().lock().lines().enumerate() {
        let line = line?;
        let f: Vec<_> = line.split('\t').collect();
        let path = decode(f[0])?;
        let actual = CachingFileHandle::open(None, &path, CacheValidationMode::ValidateAll);
        let matches = match actual {
            Ok(_) => f[1] == "OK",
            Err(e) => f[1].strip_prefix("ERR:").is_some_and(|expected| {
                decode(expected).is_ok_and(|bytes| e.into_json().to_bytes() == bytes)
            }),
        };
        if matches {
            passed += 1;
        } else {
            failed += 1;
            eprintln!("case {index}: native open outcome differs");
        }
    }
    println!("passed={passed} failed={failed}");
    if failed > 0 {
        return Err("native open comparison failed".into());
    }
    Ok(())
}
