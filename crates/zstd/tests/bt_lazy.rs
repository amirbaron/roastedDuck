//! Frozen delayed-tree frames include dictionary wraps and chronological reuse.
#![allow(clippy::expect_used, reason = "Independent fixture parsing and assertions")]
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
        _ => unreachable!("unknown delayed-tree fixture"),
    }
}

fn check(state: &str) {
    // Goldens are filled only from the independent frozen C++ oracle.
    let fixtures = include_str!("data/bt_lazy_1.5.6.tsv");
    let originals: std::collections::BTreeMap<_, _> = common::inputs().into_iter().collect();
    let mut compressor = rustdb_zstd::Compressor::new(3);
    let mut checked = 0;
    for line in fixtures.lines().filter(|line| !line.starts_with('#')) {
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), 7);
        if fields[0] != state {
            continue;
        }
        let size: usize = fields[2].parse().expect("size");
        let level: i32 = fields[3].parse().expect("level");
        let chunk: usize = fields[4].parse().expect("chunk");
        let expected: usize = fields[5].parse().expect("frame size");
        let expected_hash = u64::from_str_radix(fields[6], 16).expect("frame hash");
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
        let frame = if chunk == 0 {
            let mut frame = Vec::new();
            compressor.compress_into(&mut frame, data);
            frame
        } else {
            common::stream_frame_with(&mut compressor, data, chunk)
        };
        assert_eq!(
            (frame.len(), common::fnv64(&frame)),
            (expected, expected_hash),
            "{line}"
        );
        let mut decoded = vec![0; size];
        assert_eq!(
            rustdb_zstd::decompress(&mut decoded, &frame).expect("decode"),
            size
        );
        assert_eq!(decoded, data, "{line}");
        checked += 1;
    }
    assert_eq!(checked, if state == "fresh" { 257 } else { 32 });
}

#[test]
fn fresh_binary_tree_frames_match_cpp() {
    check("fresh");
}

#[test]
fn chronological_binary_tree_contexts_match_cpp() {
    check("reuse");
}
