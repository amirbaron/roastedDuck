//! The decoder reads zstd's frames: those the encoder writes (zstd's own at levels -5 to 3)
//! and those of zstd's CLI (`tests/data`: strong levels with checksums, two frames around
//! a skippable one), whole or streamed through any split of input and output; it agrees
//! with `ruzstd`, and corrupt frames fail without panicking.
#![allow(clippy::expect_used, clippy::panic)] // Test helpers fail like the tests themselves.

mod common;

use std::io::Read;

use rustdb_zstd::{Decompressor, Error, StreamDecoder};

/// The zstd CLI's frames of `tests/data` and their content's size and hash.
const FIXTURES: &[(&str, usize, u64)] = &[
    ("text_l19_checksum", 96_000, 0xefa6_516d_0db7_fa19),
    ("mixed_ultra22", 70_000, 0xb8b5_aed1_ace0_8207),
    ("text_fast5_no_checksum", 64_000, 0x1801_7984_52ca_893a),
    ("mixed_l12_checksum", 40_000, 0xf844_344f_3627_b612),
    ("two_frames_and_skippable", 50_000, 0xaffd_0123_be3e_23e5),
    ("zeros_random_text_l3", 180_000, 0x4f13_2966_f9be_332e),
];

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn one_shot(frame: &[u8], capacity: usize) -> Result<Vec<u8>, Error> {
    let mut out = vec![0; capacity];
    let size = rustdb_zstd::decompress(&mut out, frame)?;
    out.truncate(size);
    Ok(out)
}

/// Streams `frames` through `decompress_stream`, `in_chunk` more input bytes and an
/// `out_chunk`-byte output per call. A stream that wants more input after the last byte
/// is truncated (`SrcSizeWrong`).
fn streamed(frames: &[u8], in_chunk: usize, out_chunk: usize) -> Result<Vec<u8>, Error> {
    let mut decompressor = Decompressor::new();
    let mut result = Vec::new();
    let mut out = vec![0u8; out_chunk];
    let mut in_pos = 0;
    let mut available = 0;
    loop {
        available = (available + in_chunk).min(frames.len());
        let before = in_pos;
        let mut out_pos = 0;
        let hint =
            decompressor.decompress_stream(&mut out, &mut out_pos, &frames[..available], &mut in_pos)?;
        result.extend_from_slice(&out[..out_pos]);
        if in_pos == frames.len() && hint == 0 {
            return Ok(result);
        }
        if available == frames.len() && in_pos == before && out_pos == 0 {
            return Err(Error::SrcSizeWrong);
        }
    }
}

fn assert_content(name: &str, content: &[u8], size: usize, hash: u64) {
    assert_eq!((content.len(), common::fnv64(content)), (size, hash), "{name}");
}

#[test]
fn reads_the_zstd_clis_frames() {
    for &(name, size, hash) in FIXTURES {
        let frames = fixture(name);
        assert_content(name, &one_shot(&frames, size).expect(name), size, hash);
        for (in_chunk, out_chunk) in [
            (1 << 20, 1 << 20),
            (1, 1 << 17),
            (7, 1),
            (100, 333),
            (4096, 65536),
        ] {
            let content = streamed(&frames, in_chunk, out_chunk).expect(name);
            assert_content(name, &content, size, hash);
        }
        let mut reader = StreamDecoder::new(frames.as_slice());
        let mut content = Vec::new();
        reader.read_to_end(&mut content).expect(name);
        assert_content(name, &content, size, hash);
    }
}

#[test]
fn reads_the_encoders_frames() {
    for (name, data) in common::inputs() {
        for level in [-5, -1, 1, 3, 4, 9, 19] {
            let frame = rustdb_zstd::compress(&data, level);
            assert_eq!(
                one_shot(&frame, data.len()).expect(name),
                data,
                "{name} level {level}"
            );
        }
        for level in [0, 1] {
            let frame = common::stream_frame(&data, level, 4096);
            assert_eq!(
                one_shot(&frame, data.len()).expect(name),
                data,
                "{name} stream {level}"
            );
            for (in_chunk, out_chunk) in [(1 << 20, 1 << 20), (13, 4096), (65_536, 1000)] {
                assert_eq!(
                    streamed(&frame, in_chunk, out_chunk).expect(name),
                    data,
                    "{name} stream {level} by {in_chunk}/{out_chunk}"
                );
            }
        }
    }
}

#[test]
fn reads_duckdbs_string_vectors() {
    for (name, strings) in common::vectors() {
        let original: Vec<u8> = strings.concat();
        for (first, page) in common::PAGE_SPACES {
            let frame = common::strings_frame(&strings, first, page);
            assert_eq!(one_shot(&frame, original.len()).expect(name), original, "{name}");
            assert_eq!(streamed(&frame, 4096, 1 << 16).expect(name), original, "{name}");
        }
    }
}

#[test]
fn agrees_with_ruzstd() {
    for &(name, size, _) in FIXTURES {
        let frames = fixture(name);
        let mut theirs = Vec::new();
        let mut decoder = ruzstd::decoding::StreamingDecoder::new(frames.as_slice()).expect(name);
        decoder.read_to_end(&mut theirs).expect(name);
        let ours = one_shot(&frames, size).expect(name);
        assert_eq!(ours[..theirs.len()], theirs[..], "{name}");
    }
}

#[test]
fn checks_sizes_and_checksums() {
    let frames = fixture("text_l19_checksum");
    // Too little room for the content.
    assert_eq!(one_shot(&frames, 95_999), Err(Error::DstSizeTooSmall));
    // A wrong checksum.
    let mut wrong = frames.clone();
    let last = wrong.len() - 1;
    wrong[last] ^= 1;
    assert_eq!(one_shot(&wrong, 96_000), Err(Error::ChecksumWrong));
    assert_eq!(streamed(&wrong, 1000, 1000), Err(Error::ChecksumWrong));
    // Not a frame (too short to hold the header its fifth byte describes, then long enough).
    assert_eq!(one_shot(b"hello world", 100), Err(Error::SrcSizeWrong));
    assert_eq!(
        one_shot(b"hello world, not a frame", 100),
        Err(Error::PrefixUnknown)
    );
    assert_eq!(streamed(b"hello world", 3, 100), Err(Error::PrefixUnknown));
    // Garbage after a frame.
    let mut trailing = frames;
    trailing.extend_from_slice(b"garbage!");
    assert_eq!(one_shot(&trailing, 96_000), Err(Error::SrcSizeWrong));
    // A truncated frame.
    let frame = rustdb_zstd::compress(&common::text(&mut common::Rng::new(5), 10_000), 3);
    assert_eq!(
        one_shot(&frame[..frame.len() - 3], 10_000),
        Err(Error::SrcSizeWrong)
    );
}

#[test]
fn corrupt_frames_fail_without_panicking() {
    let mut rng = common::Rng::new(99);
    let mut frames: Vec<(Vec<u8>, usize)> = FIXTURES
        .iter()
        .map(|&(name, size, _)| (fixture(name), size))
        .collect();
    for (name, data) in common::inputs() {
        if name.starts_with("text_100k") || name == "binary" || name == "uuids_2400k" {
            frames.push((
                rustdb_zstd::compress(&data[..data.len().min(200_000)], -1),
                data.len(),
            ));
            frames.push((
                rustdb_zstd::compress(&data[..data.len().min(200_000)], 3),
                data.len(),
            ));
        }
    }
    // `ZSTD_FUZZ_ROUNDS` runs longer.
    let rounds = std::env::var("ZSTD_FUZZ_ROUNDS")
        .ok()
        .and_then(|rounds| rounds.parse().ok())
        .unwrap_or(3000usize);
    let mut failures = 0usize;
    for round in 0..rounds {
        let (frame, size) = &frames[round % frames.len()];
        let mut corrupt = frame.clone();
        for _ in 0..=rng.below(4) {
            let at = rng.below(corrupt.len() as u64) as usize;
            corrupt[at] ^= 1 << rng.below(8);
        }
        if rng.below(10) == 0 {
            corrupt.truncate(rng.below(corrupt.len() as u64) as usize);
        }
        failures += usize::from(one_shot(&corrupt, *size).is_err());
        failures += usize::from(streamed(&corrupt, 5000, 7000).is_err());
    }
    assert!(failures > 0);
}
