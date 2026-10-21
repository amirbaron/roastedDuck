//! Page compression codecs (`ColumnReader::DecompressInternal` and the writer's
//! `ColumnWriter::CompressPage`), all in pure Rust: snappy (our own decoder, `snap` to compress), gzip (`miniz_oxide`
//! through `flate2`), zstd (`rustdb-zstd`), LZ4 raw blocks (`lz4_flex`) and brotli (`brotli`).

use std::io::{Read as _, Write as _};

use rustdb_common::{Error, ErrorKind, Result};

use crate::thrift::CompressionCodec;

/// The fixed part of a gzip member header (`GZIP_HEADER_MINSIZE`).
const GZIP_HEADER_MINSIZE: usize = 10;
/// The CRC32 and size trailer of a gzip member (`GZIP_FOOTER_SIZE`).
const GZIP_FOOTER_SIZE: usize = 8;
/// Header flags miniz does not handle (`GZIP_FLAG_UNSUPPORTED`): FHCRC, FEXTRA, FNAME,
/// FCOMMENT and the reserved bits.
const GZIP_FLAG_UNSUPPORTED: u8 = 0x1 | 0x2 | 0x4 | 0x10 | 0xe0;

/// A `std::runtime_error` of the C++ code, reported as an `Invalid Error`.
fn runtime_error(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Invalid, message.into())
}

fn read_failure(file_name: &str, what: &str) -> Error {
    Error::invalid_input(format!("Failed to read file \"{file_name}\": {what}"))
}

/// Decompresses `src` into `dst`, which has exactly the page's uncompressed size.
pub fn decompress(codec: CompressionCodec, src: &[u8], dst: &mut [u8], file_name: &str) -> Result<()> {
    match codec {
        CompressionCodec::UNCOMPRESSED => Err(Error::internal("Parquet data unexpectedly uncompressed")),
        CompressionCodec::GZIP => gzip_decompress(src, dst),
        CompressionCodec::LZ4_RAW => {
            let written = lz4_flex::block::decompress_into(src, dst)
                .map_err(|_| read_failure(file_name, "LZ4 decompression failure"))?;
            if written != dst.len() {
                return Err(read_failure(file_name, "LZ4 decompression failure"));
            }
            Ok(())
        }
        CompressionCodec::SNAPPY => crate::snappy::decompress(src, dst).map_err(|error| match error {
            crate::snappy::SnappyError::LengthMismatch => read_failure(
                file_name,
                "Snappy decompression failure: Uncompressed data size mismatch",
            ),
            crate::snappy::SnappyError::Corrupt => read_failure(file_name, "Snappy decompression failure"),
        }),
        CompressionCodec::ZSTD => {
            let failure = || read_failure(file_name, "ZSTD Decompression failure");
            let written = rustdb_zstd::decompress(dst, src).map_err(|_| failure())?;
            if written != dst.len() {
                return Err(failure());
            }
            Ok(())
        }
        CompressionCodec::BROTLI => {
            let failure = || read_failure(file_name, "Brotli Decompression failure");
            let mut decoder = brotli::Decompressor::new(src, 4096);
            decoder.read_exact(dst).map_err(|_| failure())?;
            Ok(())
        }
        _ => {
            let name = codec.name().map_or_else(|| codec.0.to_string(), str::to_owned);
            Err(read_failure(
                file_name,
                &format!(
                    "Unsupported compression codec \"{name}\". Supported options are uncompressed, brotli, gzip, lz4_raw, snappy or zstd"
                ),
            ))
        }
    }
}

/// `MiniZStream::Decompress`: a sequence of gzip members, each inflated into the output.
fn gzip_decompress(mut src: &[u8], dst: &mut [u8]) -> Result<()> {
    let mut out_position = 0;
    while !src.is_empty() {
        if src.len() < GZIP_HEADER_MINSIZE {
            return Err(runtime_error(
                "Failed to decompress GZIP block: compressed size is less than gzip header size",
            ));
        }
        if src[0] != 0x1f || src[1] != 0x8b || src[2] != 8 || src[3] & GZIP_FLAG_UNSUPPORTED != 0 {
            return Err(runtime_error("Input is invalid/unsupported GZIP stream"));
        }
        src = &src[GZIP_HEADER_MINSIZE..];
        let mut inflater = flate2::Decompress::new(false);
        let status = inflater
            .decompress(src, &mut dst[out_position..], flate2::FlushDecompress::Finish)
            .map_err(|_| runtime_error("Failed to decompress GZIP block"))?;
        if !matches!(status, flate2::Status::Ok | flate2::Status::StreamEnd) {
            return Err(runtime_error("Failed to decompress GZIP block"));
        }
        let consumed = usize::try_from(inflater.total_in()).unwrap_or(usize::MAX);
        let produced = usize::try_from(inflater.total_out()).unwrap_or(usize::MAX);
        let skip = (consumed + GZIP_FOOTER_SIZE).min(src.len());
        src = &src[skip..];
        out_position = (out_position + produced).min(dst.len());
    }
    // Bytes no member produced read as zero.
    dst[out_position..].fill(0);
    Ok(())
}

/// Compresses a page with `codec` (`level` applies to zstd), appending to `out`.
pub fn compress(codec: CompressionCodec, src: &[u8], level: i64, out: &mut Vec<u8>) -> Result<()> {
    match codec {
        CompressionCodec::UNCOMPRESSED => out.extend_from_slice(src),
        CompressionCodec::SNAPPY => crate::snappy::compress(src, out),
        CompressionCodec::GZIP => {
            // `MiniZStream::Compress`: a minimal gzip member at miniz's default level.
            let mut encoder = flate2::write::GzEncoder::new(std::mem::take(out), flate2::Compression::new(6));
            encoder
                .write_all(src)
                .map_err(|error| Error::internal(format!("gzip compression failed: {error}")))?;
            *out = encoder
                .finish()
                .map_err(|error| Error::internal(format!("gzip compression failed: {error}")))?;
        }
        CompressionCodec::ZSTD => {
            // `ZSTD_compress` at the writer's level.
            let level = level.clamp(
                i64::from(rustdb_zstd::MIN_LEVEL),
                i64::from(rustdb_zstd::MAX_LEVEL),
            ) as i32;
            rustdb_zstd::Compressor::new(level).compress_into(out, src);
        }
        CompressionCodec::LZ4_RAW => crate::lz4::compress(src, out),
        CompressionCodec::BROTLI => brotli_compress(src, out)?,
        _ => {
            return Err(Error::internal(format!(
                "Unsupported codec for Parquet Writer: {}",
                codec.0
            )));
        }
    }
    Ok(())
}

/// `BrotliEncoderCompress(BROTLI_DEFAULT_QUALITY, BROTLI_DEFAULT_WINDOW,
/// BROTLI_DEFAULT_MODE, ...)`: one-shot compression with the input size as hint, which
/// shapes the output like DuckDB's.
fn brotli_compress(src: &[u8], out: &mut Vec<u8>) -> Result<()> {
    use brotli::enc::encode::{
        BrotliEncoderDestroyInstance, BrotliEncoderMaxCompressedSize, BrotliEncoderOperation,
        BrotliEncoderParameter, BrotliEncoderStateStruct,
    };
    let failure = || Error::internal("brotli compression failed");
    let mut state = BrotliEncoderStateStruct::new(brotli::enc::StandardAlloc::default());
    state.set_parameter(BrotliEncoderParameter::BROTLI_PARAM_QUALITY, 11);
    state.set_parameter(BrotliEncoderParameter::BROTLI_PARAM_LGWIN, 22);
    state.set_parameter(BrotliEncoderParameter::BROTLI_PARAM_MODE, 0);
    state.set_parameter(
        BrotliEncoderParameter::BROTLI_PARAM_SIZE_HINT,
        u32::try_from(src.len()).unwrap_or(u32::MAX),
    );
    let start = out.len();
    let capacity = BrotliEncoderMaxCompressedSize(src.len()).max(src.len() + 1024);
    out.resize(start + capacity, 0);
    let mut available_in = src.len();
    let mut in_offset = 0;
    let mut available_out = capacity;
    let mut out_offset = 0;
    let mut total_out = Some(0);
    let ok = state.compress_stream(
        BrotliEncoderOperation::BROTLI_OPERATION_FINISH,
        &mut available_in,
        src,
        &mut in_offset,
        &mut available_out,
        &mut out[start..],
        &mut out_offset,
        &mut total_out,
        &mut |_, _, _, _| (),
    );
    let finished = state.is_finished();
    BrotliEncoderDestroyInstance(&mut state);
    if !ok || !finished {
        return Err(failure());
    }
    out.truncate(start + out_offset);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codecs_round_trip() {
        let data: Vec<u8> = (0..10_000u32).flat_map(|i| (i % 97).to_le_bytes()).collect();
        for codec in [
            CompressionCodec::SNAPPY,
            CompressionCodec::GZIP,
            CompressionCodec::ZSTD,
            CompressionCodec::LZ4_RAW,
            CompressionCodec::BROTLI,
        ] {
            let mut compressed = Vec::new();
            compress(codec, &data, 3, &mut compressed).unwrap();
            let mut output = vec![0; data.len()];
            decompress(codec, &compressed, &mut output, "f").unwrap();
            assert_eq!(output, data, "{codec:?}");
        }
    }

    #[test]
    fn concatenated_gzip_members() {
        let mut compressed = Vec::new();
        compress(CompressionCodec::GZIP, b"hello ", 0, &mut compressed).unwrap();
        let mut second = Vec::new();
        compress(CompressionCodec::GZIP, b"world", 0, &mut second).unwrap();
        compressed.extend(second);
        let mut output = vec![0; 11];
        decompress(CompressionCodec::GZIP, &compressed, &mut output, "f").unwrap();
        assert_eq!(&output, b"hello world");
    }
}
