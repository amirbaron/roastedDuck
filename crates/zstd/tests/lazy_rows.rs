//! Frozen C++ row-hash frames cover streaming wraps and chronological context reuse.
#![allow(clippy::expect_used, reason = "Frozen reference fixtures and assertions")]
mod common;

fn check(state: &str) {
    let inputs: std::collections::BTreeMap<_, _> = common::inputs().into_iter().collect();
    let mut compressor = rustdb_zstd::Compressor::new(3);
    let mut checked = 0;
    for row in include_str!("data/lazy_rows_1.5.6.tsv")
        .lines()
        .filter(|row| !row.starts_with('#'))
    {
        let fields: Vec<_> = row.split('\t').collect();
        assert_eq!(fields.len(), 7);
        if fields[0] != state {
            continue;
        }
        let size: usize = fields[2].parse().expect("input size");
        let level: i32 = fields[3].parse().expect("level");
        let chunk: usize = fields[4].parse().expect("stream chunk");
        let expected_size: usize = fields[5].parse().expect("frame size");
        let expected_hash = u64::from_str_radix(fields[6], 16).expect("frame hash");
        let data = &inputs[fields[1]][..size];
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
            (expected_size, expected_hash),
            "{row}"
        );
        let mut decoded = vec![0; size];
        assert_eq!(
            rustdb_zstd::decompress(&mut decoded, &frame).expect("frame decodes"),
            size
        );
        assert_eq!(decoded, data, "{row}");
        checked += 1;
    }
    assert_eq!(checked, if state == "fresh" { 192 } else { 32 });
}

#[test]
fn fresh_one_shot_stream_and_tail_frames_match_cpp() {
    check("fresh");
}

#[test]
fn chronological_context_salts_and_level_changes_match_cpp() {
    check("reuse");
}
