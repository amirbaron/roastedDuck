//! Independent frozen C++ frames cover small-window hash-chain parsing and reuse.
#![allow(clippy::expect_used, reason = "Frozen fixture parsing and assertions")]
mod common;

pub fn input(kind: &str, size: usize) -> Vec<u8> {
    match kind {
        "zero" => vec![0; size],
        "text" => common::text(&mut common::Rng::new(42), size),
        "random" => {
            let mut rng = common::Rng::new(42);
            (0..size).map(|_| rng.next() as u8).collect()
        }
        "overlap" => {
            let pattern = b"aaaaaaaabaaaaaaaacaaaaaaaab";
            (0..size)
                .map(|i| {
                    if i % 197 == 0 {
                        (i / 197) as u8
                    } else {
                        pattern[i % pattern.len()]
                    }
                })
                .collect()
        }
        _ => unreachable!("Unknown hash-chain fixture"),
    }
}

#[test]
fn frozen_hash_chain_frames_match_and_reused_contexts_reset() {
    let fixtures = include_str!("data/lazy_hash_chain_1.5.6.tsv");
    let mut compressor = rustdb_zstd::Compressor::new(3);
    for _ in 0..2 {
        for row in fixtures.lines().filter(|row| !row.starts_with('#')) {
            let fields: Vec<_> = row.split('\t').collect();
            assert_eq!(fields.len(), 5);
            let size: usize = fields[1].parse().expect("fixture size");
            let level: i32 = fields[2].parse().expect("fixture level");
            let bytes: usize = fields[3].parse().expect("frame size");
            let hash = u64::from_str_radix(fields[4], 16).expect("frame hash");
            let data = input(fields[0], size);
            compressor.set_level(level);
            let mut frame = Vec::new();
            compressor.compress_into(&mut frame, &data);
            assert_eq!((frame.len(), common::fnv64(&frame)), (bytes, hash), "{row}");
            let mut decoded = vec![0; size];
            assert_eq!(
                rustdb_zstd::decompress(&mut decoded, &frame).expect("frame decodes"),
                size
            );
            assert_eq!(decoded, data, "{row}");
        }
    }
}
