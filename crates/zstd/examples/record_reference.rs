//! Prints the table of `tests/reference.rs`: the size and FNV-1a hash of the frames the
//! reference encoder (`tools/zstd-ref`, DuckDB's zstd 1.5.6) writes for the shared inputs.
//!
//! Usage: `record_reference <zstd_ref> <scratch dir>`
use std::process::Command;

#[path = "../tests/common/mod.rs"]
mod common;

fn reference(zstd_ref: &str, args: &[&str], out: &str) -> Vec<u8> {
    let status = Command::new(zstd_ref).args(args).status();
    if !matches!(status, Ok(status) if status.success()) {
        eprintln!("zstd_ref {args:?} failed");
        std::process::exit(2);
    }
    std::fs::read(out).unwrap_or_default()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: record_reference <zstd_ref> <scratch dir>");
        std::process::exit(2);
    }
    let (zstd_ref, scratch) = (&args[1], &args[2]);
    let input = format!("{scratch}/input.bin");
    let output = format!("{scratch}/output.zst");
    for (name, data) in common::inputs() {
        std::fs::write(&input, &data).unwrap_or(());
        for level in common::ONE_SHOT_LEVELS {
            let frame = reference(
                zstd_ref,
                &["oneshot", &level.to_string(), &input, &output],
                &output,
            );
            println!(
                "    (\"{name}\", Mode::OneShot, {level}, {}, {:#018x}),",
                frame.len(),
                common::fnv64(&frame)
            );
        }
        for level in [0, 1] {
            let frame = reference(
                zstd_ref,
                &["stream", &level.to_string(), &input, &output, "4096"],
                &output,
            );
            println!(
                "    (\"{name}\", Mode::Stream, {level}, {}, {:#018x}),",
                frame.len(),
                common::fnv64(&frame)
            );
        }
    }
    for (name, strings) in common::vectors() {
        std::fs::write(&input, common::length_prefixed(&strings)).unwrap_or(());
        for (index, (first, page)) in common::PAGE_SPACES.iter().enumerate() {
            let frame = reference(
                zstd_ref,
                &["strings", &input, &output, &first.to_string(), &page.to_string()],
                &output,
            );
            println!(
                "    (\"{name}\", Mode::Strings, {index}, {}, {:#018x}),",
                frame.len(),
                common::fnv64(&frame)
            );
        }
    }
}
