//! Deterministic inputs shared by the reference tests and the example that records the
//! reference encoder's frames for them.
#![allow(
    dead_code,
    reason = "shared by a test and an example, which use different parts"
)]

/// A xorshift generator, so that the inputs need not be stored.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const WORDS: [&str; 24] = [
    "the",
    "quick",
    "brown",
    "fox",
    "jumps",
    "over",
    "lazy",
    "dog",
    "lorem",
    "ipsum",
    "dolor",
    "sit",
    "amet",
    "consectetur",
    "adipiscing",
    "elit",
    "sed",
    "do",
    "eiusmod",
    "tempor",
    "incididunt",
    "labore",
    "magna",
    "aliqua",
];

/// Words separated by spaces, `size` bytes.
pub fn text(rng: &mut Rng, size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size + 16);
    while out.len() < size {
        out.extend_from_slice(WORDS[rng.below(WORDS.len() as u64) as usize].as_bytes());
        out.push(b' ');
    }
    out.truncate(size);
    out
}

/// 128 random UUIDs back to back, like `list_reduce([uuid()::varchar ...], concat)`.
pub fn uuids(rng: &mut Rng) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = Vec::with_capacity(36 * 128);
    for _ in 0..128 {
        for (i, group) in [8, 4, 4, 4, 12].iter().enumerate() {
            if i > 0 {
                out.push(b'-');
            }
            for position in 0..*group {
                let nibble = if i == 2 && position == 0 {
                    4
                } else {
                    rng.below(16) as usize
                };
                out.push(HEX[nibble]);
            }
        }
    }
    out
}

/// The named inputs of the one-shot and stream cases.
pub fn inputs() -> Vec<(&'static str, Vec<u8>)> {
    let mut rng = Rng::new(42);
    let mut binary = Vec::new();
    for i in 0..30_000u64 {
        binary.extend_from_slice(&((i * 7) % 1000).to_le_bytes()[..4]);
        binary.extend_from_slice(&rng.below(1 << 20).to_le_bytes());
        let length = rng.below(30) as usize;
        let words = text(&mut rng, length);
        binary.extend_from_slice(&words);
    }
    vec![
        ("empty", Vec::new()),
        ("tiny", b"hello".to_vec()),
        ("text_3k", text(&mut rng, 3_000)),
        ("text_100k", text(&mut rng, 100_000)),
        ("text_300k", text(&mut rng, 300_000)),
        ("text_2500k", text(&mut rng, 2_500_000)),
        ("random_200k", (0..200_000).map(|_| rng.next() as u8).collect()),
        ("zeros_300k", vec![0; 300_000]),
        ("uuids_2400k", (0..520).flat_map(|_| uuids(&mut rng)).collect()),
        ("binary", binary),
    ]
}

/// The named vectors of strings of the DuckDB string compression cases.
pub fn vectors() -> Vec<(&'static str, Vec<Vec<u8>>)> {
    let mut rng = Rng::new(7);
    vec![
        ("single", vec![text(&mut rng, 100)]),
        ("empty", vec![Vec::new(); 2048]),
        (
            "last_only",
            (0..2048)
                .map(|i| {
                    if i == 2047 {
                        text(&mut rng, 5000)
                    } else {
                        Vec::new()
                    }
                })
                .collect(),
        ),
        (
            "short_text",
            (0..2048)
                .map(|_| {
                    let length = rng.below(200) as usize;
                    text(&mut rng, length)
                })
                .collect(),
        ),
        ("uuids", (0..600).map(|_| uuids(&mut rng)).collect()),
        ("wrap_shortcut", (0..35).map(|_| text(&mut rng, 65_536)).collect()),
        (
            "wrap_empty_end",
            (0..35)
                .map(|i| {
                    if i == 34 {
                        Vec::new()
                    } else {
                        text(&mut rng, 65_536)
                    }
                })
                .collect(),
        ),
    ]
}

/// FNV-1a of the frame, which the tables record instead of the frame.
pub fn fnv64(data: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// The page spaces DuckDB's string compression writes into: the first page's, then a full
/// page's.
pub const PAGE_SPACES: [(usize, usize); 3] = [(200_000, 262_136), (100, 262_136), (50_000, 4_000)];

/// The levels of the one-shot cases.
pub const ONE_SHOT_LEVELS: [i32; 6] = [3, 1, 2, -1, -3, -5];

/// DuckDB's ZSTD string compression of one vector (`ZSTDCompressionState::CompressString`):
/// each string is one streaming call into the space left on the page, the last one ending
/// the frame, empty ones before it skipped; output left to flush starts a new page.
pub fn strings_frame(strings: &[Vec<u8>], first_space: usize, page_space: usize) -> Vec<u8> {
    strings_frame_with(
        &mut roastedduck_zstd::Compressor::new(0),
        strings,
        first_space,
        page_space,
    )
}

/// `strings_frame` with a context used before, as DuckDB keeps one per column: a new
/// session per vector (`ZSTD_CCtx_reset(ZSTD_reset_session_only)`).
pub fn strings_frame_with(
    compressor: &mut roastedduck_zstd::Compressor,
    strings: &[Vec<u8>],
    first_space: usize,
    page_space: usize,
) -> Vec<u8> {
    use roastedduck_zstd::EndDirective;
    compressor.reset_session();
    let mut result = Vec::new();
    let mut page = vec![0u8; first_space];
    let mut out_pos = 0;
    for (i, string) in strings.iter().enumerate() {
        let end = i + 1 == strings.len();
        if !end && string.is_empty() {
            continue;
        }
        let directive = if end {
            EndDirective::End
        } else {
            EndDirective::Continue
        };
        let mut in_pos = 0;
        loop {
            let remaining =
                compressor.compress_stream(&mut page, &mut out_pos, string, &mut in_pos, directive);
            if remaining == 0 {
                break;
            }
            result.extend_from_slice(&page[..out_pos]);
            page = vec![0u8; page_space];
            out_pos = 0;
        }
    }
    result.extend_from_slice(&page[..out_pos]);
    result
}

/// `ZstdStreamWrapper`: `chunk` bytes per `ZSTD_e_continue` call, then `ZSTD_e_end`.
pub fn stream_frame(data: &[u8], level: i32, chunk: usize) -> Vec<u8> {
    stream_frame_with(&mut roastedduck_zstd::Compressor::new(level), data, chunk)
}

/// Buffered stream with the same context retained across caller-selected levels.
pub fn stream_frame_with(
    compressor: &mut roastedduck_zstd::Compressor,
    data: &[u8],
    chunk: usize,
) -> Vec<u8> {
    use roastedduck_zstd::{EndDirective, compress_bound};
    compressor.reset_session();
    let mut result = Vec::new();
    let mut buffer = vec![0u8; compress_bound(1 << 17) + 3 + 4];
    for piece in data.chunks(chunk) {
        let mut in_pos = 0;
        while in_pos < piece.len() {
            let mut out_pos = 0;
            compressor.compress_stream(
                &mut buffer,
                &mut out_pos,
                piece,
                &mut in_pos,
                EndDirective::Continue,
            );
            result.extend_from_slice(&buffer[..out_pos]);
        }
    }
    loop {
        let (mut out_pos, mut in_pos) = (0, 0);
        let remaining =
            compressor.compress_stream(&mut buffer, &mut out_pos, &[], &mut in_pos, EndDirective::End);
        result.extend_from_slice(&buffer[..out_pos]);
        if remaining == 0 {
            return result;
        }
    }
}

/// Length-prefixed strings, the input format of `zstd_ref strings`.
pub fn length_prefixed(strings: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for string in strings {
        out.extend_from_slice(&(string.len() as u32).to_le_bytes());
        out.extend_from_slice(string);
    }
    out
}
