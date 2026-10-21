//! Compares rustdb-zstd's frames with the reference encoder's (`tools/zstd-ref`, DuckDB's
//! zstd 1.5.6), driven the same way.
//!
//! Usage: `compare <zstd_ref> <scratch dir> <mode> <level> <input> [args]` with the modes
//! - `oneshot <level> <input>` (`ZSTD_compress`),
//! - `strings 0 <length-prefixed strings> <first_space> <page_space>` (DuckDB's ZSTD strings),
//! - `stream <level> <input> <chunk>` (`ZstdStreamWrapper`).
use std::process::Command;

use rustdb_zstd::{Compressor, EndDirective, compress, compress_bound};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 6 {
        eprintln!("usage: compare <zstd_ref> <scratch> <oneshot|strings|stream> <level> <input> [args]");
        std::process::exit(2);
    }
    let (reference, scratch, mode, level, input) = (&args[1], &args[2], &args[3], &args[4], &args[5]);
    let level: i32 = level.parse().unwrap_or(3);
    let data = std::fs::read(input).unwrap_or_default();
    let reference_out = format!("{scratch}/reference.zst");
    let started = std::time::Instant::now();
    let ours = match mode.as_str() {
        "oneshot" => {
            run(reference, &["oneshot", &level.to_string(), input, &reference_out]);
            compress(&data, level)
        }
        "strings" => {
            let (first, page) = (&args[6], &args[7]);
            run(reference, &["strings", input, &reference_out, first, page]);
            strings(&data, first.parse().unwrap_or(0), page.parse().unwrap_or(0))
        }
        _ => {
            let chunk = &args[6];
            run(
                reference,
                &["stream", &level.to_string(), input, &reference_out, chunk],
            );
            stream(&data, level, chunk.parse().unwrap_or(1))
        }
    };
    let elapsed = started.elapsed();
    std::fs::write(format!("{scratch}/ours.zst"), &ours).unwrap_or(());
    let expected = std::fs::read(&reference_out).unwrap_or_default();
    let first_difference = ours.iter().zip(&expected).position(|(a, b)| a != b);
    match first_difference {
        None if ours.len() == expected.len() => println!("MATCH {} bytes ({elapsed:?})", ours.len()),
        _ => println!(
            "DIFFER ours {} reference {} first difference at {:?}",
            ours.len(),
            expected.len(),
            first_difference
        ),
    }
}

fn run(reference: &str, args: &[&str]) {
    let status = Command::new(reference).args(args).status().map(|s| s.success());
    if !matches!(status, Ok(true)) {
        eprintln!("reference failed: {status:?}");
        std::process::exit(2);
    }
}

fn strings(data: &[u8], first_space: usize, page_space: usize) -> Vec<u8> {
    let mut strings = Vec::new();
    let mut pos = 0;
    while pos + 4 <= data.len() {
        let length = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        strings.push(&data[pos + 4..pos + 4 + length]);
        pos += 4 + length;
    }
    let mut compressor = Compressor::new(0);
    compressor.reset_session();
    let mut result = Vec::new();
    let mut page = vec![0u8; first_space];
    let mut out_pos = 0;
    for (i, string) in strings.iter().enumerate() {
        let end = i + 1 == strings.len();
        if !end && string.is_empty() {
            continue;
        }
        let directive = if end {
            EndDirective::End
        } else {
            EndDirective::Continue
        };
        let mut in_pos = 0;
        loop {
            let remaining =
                compressor.compress_stream(&mut page, &mut out_pos, string, &mut in_pos, directive);
            if remaining == 0 {
                break;
            }
            result.extend_from_slice(&page[..out_pos]);
            page = vec![0u8; page_space];
            out_pos = 0;
        }
    }
    result.extend_from_slice(&page[..out_pos]);
    result
}

fn stream(data: &[u8], level: i32, chunk: usize) -> Vec<u8> {
    let mut compressor = Compressor::new(level);
    let mut result = Vec::new();
    let mut buffer = vec![0u8; compress_bound(1 << 17) + 3 + 4];
    for piece in data.chunks(chunk.max(1)) {
        let mut in_pos = 0;
        while in_pos < piece.len() {
            let mut out_pos = 0;
            compressor.compress_stream(
                &mut buffer,
                &mut out_pos,
                piece,
                &mut in_pos,
                EndDirective::Continue,
            );
            result.extend_from_slice(&buffer[..out_pos]);
        }
    }
    loop {
        let (mut out_pos, mut in_pos) = (0, 0);
        let remaining =
            compressor.compress_stream(&mut buffer, &mut out_pos, &[], &mut in_pos, EndDirective::End);
        result.extend_from_slice(&buffer[..out_pos]);
        if remaining == 0 {
            break;
        }
    }
    result
}
