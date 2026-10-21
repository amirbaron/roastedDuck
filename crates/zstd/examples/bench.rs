//! Compression (rustdb-zstd) and decompression (`ruzstd`) throughput, to compare with
//! `zstd_ref bench` (DuckDB's zstd).
//!
//! Usage: bench <level> <input> <iterations>
use std::io::Read;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: bench <level> <input> <iterations>");
        std::process::exit(2);
    }
    let level: i32 = args[1].parse().unwrap_or(3);
    let src = std::fs::read(&args[2]).unwrap_or_default();
    let iterations: usize = args[3].parse().unwrap_or(1);
    let mut compressor = rustdb_zstd::Compressor::new(level);
    let mut frame = Vec::new();
    let start = Instant::now();
    for _ in 0..iterations {
        frame.clear();
        compressor.compress_into(&mut frame, &src);
    }
    let middle = Instant::now();
    let mut back = Vec::with_capacity(src.len());
    for _ in 0..iterations {
        back.clear();
        let mut decoder = ruzstd::decoding::StreamingDecoder::new(frame.as_slice()).unwrap_or_else(|error| {
            eprintln!("{error}");
            std::process::exit(1)
        });
        decoder.read_to_end(&mut back).unwrap_or(0);
    }
    let end = Instant::now();
    assert_eq!(back, src, "round trip");
    let mb = src.len() as f64 * iterations as f64 / f64::from(1 << 20);
    println!(
        "compressed {} -> {}: compress {:.1} MB/s, decompress {:.1} MB/s",
        src.len(),
        frame.len(),
        mb / (middle - start).as_secs_f64(),
        mb / (end - middle).as_secs_f64()
    );
}
