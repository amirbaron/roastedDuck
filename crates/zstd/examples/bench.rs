//! Compression and decompression throughput of roastedduck-zstd, to compare with
//! `zstd_ref bench` (DuckDB's zstd): `compress` and `decompress` in a loop, a new
//! compression context per call as `ZSTD_compress` makes one (the decompression context
//! is reused per thread).
//!
//! Usage: `bench <level> <input> <iterations> [compress|decompress]`; the last argument
//! repeats only that half (for profiling).
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
    let only = args.get(4).map(String::as_str);
    let (compress_iterations, decompress_iterations) = match only {
        Some("compress") => (iterations, 1),
        Some("decompress") => (1, iterations),
        _ => (iterations, iterations),
    };
    let mut frame = Vec::new();
    let start = Instant::now();
    for _ in 0..compress_iterations {
        frame = roastedduck_zstd::compress(&src, level);
    }
    let middle = Instant::now();
    let mut back = vec![0u8; src.len()];
    for _ in 0..decompress_iterations {
        if let Err(error) = roastedduck_zstd::decompress(&mut back, &frame) {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
    let end = Instant::now();
    assert_eq!(back, src, "round trip");
    let mb = src.len() as f64 / f64::from(1 << 20);
    println!(
        "compressed {} -> {}: compress {:.1} MB/s, decompress {:.1} MB/s",
        src.len(),
        frame.len(),
        mb * compress_iterations as f64 / (middle - start).as_secs_f64(),
        mb * decompress_iterations as f64 / (end - middle).as_secs_f64()
    );
}
