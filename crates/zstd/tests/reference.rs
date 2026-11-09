//! roastedduck-zstd writes the frames DuckDB's zstd 1.5.6 writes: the size and hash of each
//! frame of the shared inputs, recorded from the reference encoder with
//! `cargo run --release --example record_reference -- <zstd_ref> <scratch dir>`
//! (`tools/zstd-ref/build.sh` builds `zstd_ref`). Every frame also decodes with `ruzstd`.
#![allow(clippy::expect_used, clippy::panic)] // Test helpers fail like the tests themselves.

mod common;

use std::io::Read;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// `ZSTD_compress` at the level.
    OneShot,
    /// `ZstdStreamWrapper` at the level (0: the default), 4096 bytes per call.
    Stream,
    /// DuckDB's ZSTD string compression into the page spaces of `PAGE_SPACES[level]`.
    Strings,
}

#[rustfmt::skip]
#[allow(clippy::unreadable_literal, reason = "hashes as the recorder prints them")]
const EXPECTED: &[(&str, Mode, i32, usize, u64)] = &[
    ("empty", Mode::OneShot, 3, 9, 0x608d6184a88329b1),
    ("empty", Mode::OneShot, 1, 9, 0x608d6184a88329b1),
    ("empty", Mode::OneShot, 2, 9, 0x608d6184a88329b1),
    ("empty", Mode::OneShot, -1, 9, 0x608d6184a88329b1),
    ("empty", Mode::OneShot, -3, 9, 0x608d6184a88329b1),
    ("empty", Mode::OneShot, -5, 9, 0x608d6184a88329b1),
    ("empty", Mode::Stream, 0, 9, 0x608d6184a88329b1),
    ("empty", Mode::Stream, 1, 9, 0x608d6184a88329b1),
    ("tiny", Mode::OneShot, 3, 14, 0x2c194be04c3f7e68),
    ("tiny", Mode::OneShot, 1, 14, 0x2c194be04c3f7e68),
    ("tiny", Mode::OneShot, 2, 14, 0x2c194be04c3f7e68),
    ("tiny", Mode::OneShot, -1, 14, 0x2c194be04c3f7e68),
    ("tiny", Mode::OneShot, -3, 14, 0x2c194be04c3f7e68),
    ("tiny", Mode::OneShot, -5, 14, 0x2c194be04c3f7e68),
    ("tiny", Mode::Stream, 0, 14, 0x0ccb33ec76219b9f),
    ("tiny", Mode::Stream, 1, 14, 0x3d074901b1ad922f),
    ("text_3k", Mode::OneShot, 3, 783, 0xba4b14b11775b8fd),
    ("text_3k", Mode::OneShot, 1, 851, 0xc521af11d754bf20),
    ("text_3k", Mode::OneShot, 2, 820, 0x1ebfb5aa422fdc8f),
    ("text_3k", Mode::OneShot, -1, 948, 0x70431f9e7d43b0c2),
    ("text_3k", Mode::OneShot, -3, 1006, 0x02347fc8565ee8c6),
    ("text_3k", Mode::OneShot, -5, 1209, 0x5d18e50e8ce2dcfb),
    ("text_3k", Mode::Stream, 0, 804, 0x2c2e6a5549125603),
    ("text_3k", Mode::Stream, 1, 914, 0x63e4ca7c0e994fa7),
    ("text_100k", Mode::OneShot, 3, 19692, 0x7a030fbe2e06c6cf),
    ("text_100k", Mode::OneShot, 1, 21724, 0xe58024ab5a12673a),
    ("text_100k", Mode::OneShot, 2, 22774, 0xd8c107d7961bfacf),
    ("text_100k", Mode::OneShot, -1, 22938, 0x48a027aa0ebc6c97),
    ("text_100k", Mode::OneShot, -3, 23114, 0xfb185809ef3f4301),
    ("text_100k", Mode::OneShot, -5, 23696, 0x60df1dff06577bc8),
    ("text_100k", Mode::Stream, 0, 19679, 0xcc551157d45ba40f),
    ("text_100k", Mode::Stream, 1, 20722, 0xf2b6bce247d0f457),
    ("text_300k", Mode::OneShot, 3, 57917, 0x5bb0fbb425f7572e),
    ("text_300k", Mode::OneShot, 1, 61170, 0x68dcfafe5a3be9da),
    ("text_300k", Mode::OneShot, 2, 64585, 0xefab69fe428500f6),
    ("text_300k", Mode::OneShot, -1, 64922, 0xedee81f202f5faa1),
    ("text_300k", Mode::OneShot, -3, 65289, 0x7ef07cec2fb34b01),
    ("text_300k", Mode::OneShot, -5, 66408, 0x0d98803423ab732c),
    ("text_300k", Mode::Stream, 0, 57914, 0xeca7bc5df292d4e3),
    ("text_300k", Mode::Stream, 1, 61167, 0x525efde405c12c11),
    ("text_2500k", Mode::OneShot, 3, 477678, 0x3d7c46eac1ff450d),
    ("text_2500k", Mode::OneShot, 1, 504420, 0x8f1b1962c78f4af8),
    ("text_2500k", Mode::OneShot, 2, 534299, 0xe437bf49cbf6ec19),
    ("text_2500k", Mode::OneShot, -1, 535448, 0x43758d0e6de0da25),
    ("text_2500k", Mode::OneShot, -3, 537650, 0xb815e6de64cc8a19),
    ("text_2500k", Mode::OneShot, -5, 544093, 0xf478748301fcedae),
    ("text_2500k", Mode::Stream, 0, 478464, 0x759a808cfe3562c8),
    ("text_2500k", Mode::Stream, 1, 504416, 0x148653706762e119),
    ("random_200k", Mode::OneShot, 3, 200015, 0x47c0f11121ae557b),
    ("random_200k", Mode::OneShot, 1, 200015, 0x47c0f11121ae557b),
    ("random_200k", Mode::OneShot, 2, 200015, 0x47c0f11121ae557b),
    ("random_200k", Mode::OneShot, -1, 200015, 0x47c0f11121ae557b),
    ("random_200k", Mode::OneShot, -3, 200015, 0x47c0f11121ae557b),
    ("random_200k", Mode::OneShot, -5, 200015, 0x47c0f11121ae557b),
    ("random_200k", Mode::Stream, 0, 200012, 0xda71a5007dab6a9f),
    ("random_200k", Mode::Stream, 1, 200012, 0xb91c633fdbfab9cf),
    ("zeros_300k", Mode::OneShot, 3, 30, 0x9dcd46f94b4d514d),
    ("zeros_300k", Mode::OneShot, 1, 30, 0x9dcd46f94b4d514d),
    ("zeros_300k", Mode::OneShot, 2, 30, 0x9dcd46f94b4d514d),
    ("zeros_300k", Mode::OneShot, -1, 30, 0x9dcd46f94b4d514d),
    ("zeros_300k", Mode::OneShot, -3, 32, 0x6b59e6bbad696de7),
    ("zeros_300k", Mode::OneShot, -5, 34, 0x0842bbc722be3e21),
    ("zeros_300k", Mode::Stream, 0, 27, 0x6ede79157eccb052),
    ("zeros_300k", Mode::Stream, 1, 27, 0x3b51b692faef20e2),
    ("uuids_2400k", Mode::OneShot, 3, 1264722, 0xb4e155f4bf621b58),
    ("uuids_2400k", Mode::OneShot, 1, 1229020, 0x1a0ee7f0dfe530c7),
    ("uuids_2400k", Mode::OneShot, 2, 1230658, 0x73e6138a45eb943f),
    ("uuids_2400k", Mode::OneShot, -1, 2396227, 0x142139c1b8e207e5),
    ("uuids_2400k", Mode::OneShot, -3, 2396227, 0x142139c1b8e207e5),
    ("uuids_2400k", Mode::OneShot, -5, 2396227, 0x142139c1b8e207e5),
    ("uuids_2400k", Mode::Stream, 0, 1264718, 0xfa386e982616cddc),
    ("uuids_2400k", Mode::Stream, 1, 1229026, 0x4ffa23098f7eb77e),
    ("binary", Mode::OneShot, 3, 314337, 0xea442ec7c69fba6f),
    ("binary", Mode::OneShot, 1, 339869, 0xb2ae2fe4c6175fd2),
    ("binary", Mode::OneShot, 2, 339744, 0x582802116bef23b0),
    ("binary", Mode::OneShot, -1, 408244, 0xa38ceab5f4002a10),
    ("binary", Mode::OneShot, -3, 416439, 0x0b9e41fb8008af35),
    ("binary", Mode::OneShot, -5, 413575, 0xe7d02faccdecb252),
    ("binary", Mode::Stream, 0, 314334, 0x6066f78be15033e2),
    ("binary", Mode::Stream, 1, 339865, 0xc0b9061a6ebf9f25),
    ("single", Mode::Strings, 0, 80, 0x2548078224701d99),
    ("single", Mode::Strings, 1, 80, 0x2548078224701d99),
    ("single", Mode::Strings, 2, 80, 0x2548078224701d99),
    ("empty", Mode::Strings, 0, 9, 0x608d6184a88329b1),
    ("empty", Mode::Strings, 1, 9, 0x608d6184a88329b1),
    ("empty", Mode::Strings, 2, 9, 0x608d6184a88329b1),
    ("last_only", Mode::Strings, 0, 1203, 0xa919ecc5fc5ad497),
    ("last_only", Mode::Strings, 1, 1203, 0xa919ecc5fc5ad497),
    ("last_only", Mode::Strings, 2, 1203, 0xa919ecc5fc5ad497),
    ("short_text", Mode::Strings, 0, 42660, 0x232cd2b78f6d3979),
    ("short_text", Mode::Strings, 1, 42660, 0x232cd2b78f6d3979),
    ("short_text", Mode::Strings, 2, 42660, 0x232cd2b78f6d3979),
    ("uuids", Mode::Strings, 0, 1461382, 0xadc49b7ad93f15b6),
    ("uuids", Mode::Strings, 1, 1461382, 0xadc49b7ad93f15b6),
    ("uuids", Mode::Strings, 2, 1461382, 0xadc49b7ad93f15b6),
    ("wrap_shortcut", Mode::Strings, 0, 438572, 0x32d288b1c39a1472),
    ("wrap_shortcut", Mode::Strings, 1, 438572, 0x32d288b1c39a1472),
    ("wrap_shortcut", Mode::Strings, 2, 438572, 0x32d288b1c39a1472),
    ("wrap_empty_end", Mode::Strings, 0, 425433, 0xcaf13822fcb53c40),
    ("wrap_empty_end", Mode::Strings, 1, 425433, 0xcaf13822fcb53c40),
    ("wrap_empty_end", Mode::Strings, 2, 425433, 0xcaf13822fcb53c40),
];

fn decode(frame: &[u8]) -> Vec<u8> {
    let mut decoder = ruzstd::decoding::StreamingDecoder::new(frame).expect("frame header");
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).expect("frame decodes");
    out
}

fn check(name: &str, mode: Mode, level: i32, frame: &[u8], original: &[u8]) {
    let &(_, _, _, size, hash) = EXPECTED
        .iter()
        .find(|&&(n, m, l, _, _)| n == name && m == mode && l == level)
        .unwrap_or_else(|| panic!("no reference for {name} {mode:?} {level}"));
    assert_eq!(
        (frame.len(), common::fnv64(frame)),
        (size, hash),
        "{name} {mode:?} {level}: frame differs from zstd's"
    );
    assert_eq!(decode(frame), original, "{name} {mode:?} {level}: round trip");
}

#[test]
fn one_shot_and_streamed_frames_match_zstd() {
    for (name, data) in common::inputs() {
        for level in common::ONE_SHOT_LEVELS {
            check(
                name,
                Mode::OneShot,
                level,
                &roastedduck_zstd::compress(&data, level),
                &data,
            );
        }
        for level in [0, 1] {
            check(
                name,
                Mode::Stream,
                level,
                &common::stream_frame(&data, level, 4096),
                &data,
            );
        }
        // How the input is split into calls does not change a stream's frame.
        let whole = roastedduck_zstd::compress_streamed(&data, 0);
        check(name, Mode::Stream, 0, &whole, &data);
    }
}

#[test]
fn string_vectors_match_duckdbs_zstd() {
    for (name, strings) in common::vectors() {
        let original: Vec<u8> = strings.concat();
        for (index, (first, page)) in common::PAGE_SPACES.iter().enumerate() {
            let frame = common::strings_frame(&strings, *first, *page);
            check(name, Mode::Strings, index as i32, &frame, &original);
        }
    }
}

#[test]
fn reused_contexts_write_the_same_frames() {
    // A context used before continues its indexes instead of clearing its tables
    // (`ZSTDirp_continue`); the frames do not change.
    let mut compressor = roastedduck_zstd::Compressor::new(0);
    for _ in 0..3 {
        for (name, strings) in common::vectors() {
            let original: Vec<u8> = strings.concat();
            for (index, (first, page)) in common::PAGE_SPACES.iter().enumerate() {
                let frame = common::strings_frame_with(&mut compressor, &strings, *first, *page);
                check(name, Mode::Strings, index as i32, &frame, &original);
            }
        }
        let mut one_shot = roastedduck_zstd::Compressor::new(3);
        for (name, data) in common::inputs() {
            for level in common::ONE_SHOT_LEVELS {
                one_shot.set_level(level);
                let mut frame = Vec::new();
                one_shot.compress_into(&mut frame, &data);
                check(name, Mode::OneShot, level, &frame, &data);
            }
        }
    }
}

#[test]
fn stronger_levels_round_trip() {
    // Levels above dfast run the double-fast match finder: valid frames.
    let mut rng = common::Rng::new(3);
    let data = common::text(&mut rng, 400_000);
    for level in [4, 5, 9, 19, 22] {
        assert_eq!(
            decode(&roastedduck_zstd::compress(&data, level)),
            data,
            "level {level}"
        );
    }
}
