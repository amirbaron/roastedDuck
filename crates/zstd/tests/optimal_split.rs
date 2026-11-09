//! Default C++ optimal blocks, recursive splitting and chronological entropy reuse.
#![allow(clippy::expect_used, reason = "Frozen independent C++ reference fixtures")]
mod common;

fn check(state: &str) {
    let mut inputs: std::collections::BTreeMap<_, _> = common::inputs().into_iter().collect();
    // The independent streaming oracle uses two copies of these 4.5 MB
    // fixtures, rather than the differently seeded ordinary text inputs.
    inputs.insert(
        "text",
        common::text(&mut common::Rng::new(42), 4_500_000).repeat(2),
    );
    let pattern = b"aaaaaaaabaaaaaaaacaaaaaaaab";
    let overlap: Vec<u8> = (0..4_500_000)
        .map(|i| {
            if i % 197 == 0 {
                (i / 197) as u8
            } else {
                pattern[i % pattern.len()]
            }
        })
        .collect();
    inputs.insert("overlap", overlap.repeat(2));
    let mut compressor = roastedduck_zstd::Compressor::new(3);
    let mut checked = 0;
    for line in include_str!("data/optimal_split_1.5.6.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
    {
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), 6);
        if fields[0] != state {
            continue;
        }
        let level: i32 = fields[2].parse().expect("level");
        let chunk: usize = fields[3].parse().expect("chunk");
        let expected: usize = fields[4].parse().expect("frame size");
        let hash = u64::from_str_radix(fields[5], 16).expect("frame hash");
        let data = &inputs[fields[1]];
        if state == "fresh" {
            compressor = roastedduck_zstd::Compressor::new(level);
        }
        compressor.set_level(level);
        let frame = if chunk == 0 {
            let mut out = Vec::new();
            compressor.compress_into(&mut out, data);
            out
        } else {
            common::stream_frame_with(&mut compressor, data, chunk)
        };
        assert_eq!((frame.len(), common::fnv64(&frame)), (expected, hash), "{line}");
        let mut decoded = vec![0; data.len()];
        assert_eq!(
            roastedduck_zstd::decompress(&mut decoded, &frame).expect("decode"),
            data.len()
        );
        assert_eq!(&decoded, data, "{line}");
        checked += 1;
    }
    assert_eq!(checked, if state == "fresh" { 41 } else { 32 });
}

#[test]
fn default_optimal_blocks_and_streams_match_cpp() {
    check("fresh");
}

#[test]
fn default_optimal_split_contexts_match_cpp() {
    check("reuse");
}
