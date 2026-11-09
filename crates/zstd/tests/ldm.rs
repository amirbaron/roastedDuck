//! Default C++ LDM frames include threshold inputs, window wraps and level resets.
#![allow(clippy::expect_used, reason = "Independent frozen C++ frame fixtures")]
mod common;

fn append(out: &mut Vec<u8>, bytes: &[u8], limit: usize) {
    let len = (limit - out.len()).min(bytes.len());
    out.extend_from_slice(&bytes[..len]);
}

fn mixed(text: &[u8], random: &[u8], size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    let block = 1 << 17;
    let zeros = vec![0; block];
    let mut cycle = 0;
    while out.len() < size {
        let fresh: Vec<u8> = random[..block].iter().map(|b| b ^ (cycle & 255) as u8).collect();
        for bytes in [
            &fresh[..],
            &text[..block],
            &text[..65536],
            &fresh[..32768],
            &zeros[..16384],
            &text[..16384],
            &zeros[..],
            &fresh[..65536],
            &text[..65536],
        ] {
            append(&mut out, bytes, size);
        }
        cycle += 1;
    }
    out
}

fn diverse(kind: &str, size: usize) -> Vec<u8> {
    let first = match kind {
        "text" => common::text(&mut common::Rng::new(42), 4_500_000),
        "zero" => vec![0; 4_500_000],
        "random" => {
            let mut rng = common::Rng::new(42);
            (0..4_500_000).map(|_| rng.next() as u8).collect()
        }
        "overlap" => {
            let pattern = b"aaaaaaaabaaaaaaaacaaaaaaaab";
            (0..4_500_000)
                .map(|i| {
                    if i % 197 == 0 {
                        (i / 197) as u8
                    } else {
                        pattern[i % pattern.len()]
                    }
                })
                .collect()
        }
        _ => unreachable!("unknown diverse LDM fixture"),
    };
    let mut result = Vec::with_capacity(size);
    while result.len() < size {
        append(&mut result, &first, size);
    }
    result
}

fn check(state: &str) {
    let originals: std::collections::BTreeMap<_, _> = common::inputs().into_iter().collect();
    let mut compressor = roastedduck_zstd::Compressor::new(3);
    let mut checked = 0;
    for row in include_str!("data/ldm_1.5.6.tsv")
        .lines()
        .filter(|row| !row.starts_with('#'))
    {
        let fields: Vec<_> = row.split('\t').collect();
        assert_eq!(fields.len(), 7);
        if fields[0] != state {
            continue;
        }
        let size: usize = fields[2].parse().expect("source size");
        let level: i32 = fields[3].parse().expect("level");
        let chunk: usize = fields[4].parse().expect("chunk");
        let expected: usize = fields[5].parse().expect("frame size");
        let hash = u64::from_str_radix(fields[6], 16).expect("frame hash");
        let generated;
        let data = if let Some(kind) = fields[1].strip_prefix("diverse_") {
            generated = diverse(kind, size);
            &generated[..]
        } else if matches!(fields[1], "mixed" | "mixed9" | "wide70") {
            generated = mixed(&originals["text_300k"], &originals["random_200k"], size);
            &generated[..]
        } else {
            &originals[fields[1]][..size]
        };
        if state == "fresh" {
            compressor = roastedduck_zstd::Compressor::new(level);
        }
        compressor.set_level(level);
        let frame = if chunk == 0 {
            let mut frame = Vec::new();
            compressor.compress_into(&mut frame, data);
            frame
        } else {
            common::stream_frame_with(&mut compressor, data, chunk)
        };
        assert_eq!((frame.len(), common::fnv64(&frame)), (expected, hash), "{row}");
        let mut decoded = vec![0; size];
        assert_eq!(
            roastedduck_zstd::decompress(&mut decoded, &frame).expect("decode"),
            size
        );
        assert_eq!(decoded, data, "{row}");
        checked += 1;
    }
    assert_eq!(checked, if state == "fresh" { 30 } else { 16 });
}

#[test]
fn default_ldm_frames_and_window_wrap_match_cpp() {
    check("fresh");
}

#[test]
fn default_ldm_context_and_level_resets_match_cpp() {
    check("reuse");
}
