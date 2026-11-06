//! Default C++ optimal-strategy frames below the block-split window threshold.
#![allow(clippy::expect_used, reason = "Independent frozen fixture parsing")]
mod common;

pub fn generated(kind: &str, size: usize) -> Vec<u8> {
    match kind {
        "text" => common::text(&mut common::Rng::new(42), size),
        "zero" => vec![0; size],
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
        _ => unreachable!("unknown optimal fixture"),
    }
}

fn check(state: &str) {
    let originals: std::collections::BTreeMap<_, _> = common::inputs().into_iter().collect();
    let mut compressor = rustdb_zstd::Compressor::new(3);
    let mut checked = 0;
    for line in include_str!("data/optimal_small_1.5.6.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
    {
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), 6);
        if fields[0] != state {
            continue;
        }
        let size: usize = fields[2].parse().expect("size");
        let level: i32 = fields[3].parse().expect("level");
        let expected: usize = fields[4].parse().expect("frame size");
        let hash = u64::from_str_radix(fields[5], 16).expect("frame hash");
        let generated;
        let data = if let Some(kind) = fields[1].strip_prefix("generated_") {
            generated = self::generated(kind, size);
            &generated[..]
        } else {
            &originals[fields[1]][..size]
        };
        if state == "fresh" {
            compressor = rustdb_zstd::Compressor::new(level);
        }
        compressor.set_level(level);
        let mut frame = Vec::new();
        compressor.compress_into(&mut frame, data);
        assert_eq!((frame.len(), common::fnv64(&frame)), (expected, hash), "{line}");
        let mut decoded = vec![0; size];
        assert_eq!(
            rustdb_zstd::decompress(&mut decoded, &frame).expect("decode"),
            size
        );
        assert_eq!(decoded, data, "{line}");
        checked += 1;
    }
    assert_eq!(checked, if state == "fresh" { 768 } else { 32 });
}

#[test]
fn fresh_optimal_frames_match_default_cpp() {
    check("fresh");
}

#[test]
fn chronological_optimal_resets_match_default_cpp() {
    check("reuse");
}
