//! Avro object container framing. Apache Avro handles schemas and the datum encoding.
use std::io::{BufRead, BufReader, BufWriter, Read, Write};

use apache_avro::Schema;
use apache_avro::reader::datum::GenericDatumReader;
use apache_avro::types::Value;
use apache_avro::writer::datum::GenericDatumWriter;
use roastedduck_common::{Error, Result};
use roastedduck_function::caching_file_system::{CachedRead, CachingFileHandle};
use roastedduck_function::external_file_cache::CacheValidationMode;
use roastedduck_function::registered_files::Output;
use serde_json::Value as Json;

const MAGIC: &[u8; 4] = b"Obj\x01";
const MAX_BLOCK: usize = 128 * 1024 * 1024;

pub(super) fn avro_error(error: apache_avro::Error) -> Error {
    Error::invalid_input(format!("Avro: {error}"))
}

pub(super) fn io_error(error: std::io::Error) -> Error {
    roastedduck_function::registered_files::callback_error(&error)
        .unwrap_or_else(|| Error::new(roastedduck_common::ErrorKind::Io, error.to_string()))
}

pub(super) fn parse_schema(json: &Json) -> Result<Schema> {
    let mut json = json.clone();
    normalize_timestamps(&mut json)?;
    Schema::parse(json).map_err(avro_error)
}

// DuckDB's Avro extension uses the Iceberg adjust-to-utc property to distinguish
// timestamps with and without time zone. The binary representation is identical.
fn normalize_timestamps(json: &mut Json) -> Result<()> {
    if let Some(array) = json.as_array_mut() {
        for child in array {
            normalize_timestamps(child)?;
        }
    } else if let Some(object) = json.as_object_mut() {
        let utc = object.get("adjust-to-utc").and_then(Json::as_bool) == Some(true);
        match (object.get("logicalType").and_then(Json::as_str), utc) {
            (Some("timestamp-nanos"), true) => {
                return Err(Error::not_implemented(
                    "Avro timestamp-nanos with adjust_to_utc not supported",
                ));
            }
            (Some("timestamp-micros"), false) => {
                object.insert("logicalType".into(), "local-timestamp-micros".into());
            }
            (Some("timestamp-millis"), false) => {
                object.insert("logicalType".into(), "local-timestamp-millis".into());
            }
            _ => {}
        }
        for key in ["type", "items", "values"] {
            if let Some(child) = object.get_mut(key) {
                normalize_timestamps(child)?;
            }
        }
        if let Some(fields) = object.get_mut("fields").and_then(Json::as_array_mut) {
            for field in fields {
                if let Some(child) = field.get_mut("type") {
                    normalize_timestamps(child)?;
                }
            }
        }
    }
    Ok(())
}

const WHOLE_FILE_CACHE_LIMIT: u64 = 32 * 1024 * 1024;

struct CachedBytes(CachedRead);
impl AsRef<[u8]> for CachedBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.bytes()
    }
}

struct FileStream {
    handle: CachingFileHandle,
    size: u64,
    position: u64,
}

impl Read for FileStream {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let count = (output.len() as u64).min(self.size - self.position) as usize;
        if count != 0 {
            self.handle
                .read_direct(&mut output[..count], self.position)
                .map_err(std::io::Error::other)?;
            self.position += count as u64;
        }
        Ok(count)
    }
}

fn open_input(path: &[u8]) -> Result<Box<dyn Read + Send>> {
    use roastedduck_function::file_system;

    file_system::verify_can_access_file(path)?;
    let client = file_system::opener();
    let cache = client.as_ref().and_then(|client| client.external_file_cache());
    let cache_limit = cache
        .as_ref()
        .filter(|cache| cache.is_enabled())
        .map_or(0, |cache| {
            WHOLE_FILE_CACHE_LIMIT.min(cache.buffer_manager().max_memory() as u64 / 8)
        });
    let validation = CacheValidationMode::of_client(client.as_deref());
    let handle = CachingFileHandle::open(cache, path, validation)?;
    let size = handle.file_size()?;
    if size != 0 && size <= cache_limit {
        return Ok(Box::new(std::io::Cursor::new(CachedBytes(handle.read(size, 0)?))));
    }
    Ok(Box::new(FileStream {
        handle,
        size,
        position: 0,
    }))
}

type Input = BufReader<Box<dyn Read + Send>>;

#[derive(Debug)]
enum Codec {
    Null,
    Deflate,
    Snappy,
    Lzma,
    Zstandard,
}

impl Codec {
    fn parse(value: &[u8]) -> Result<Self> {
        match value {
            b"null" => Ok(Self::Null),
            b"deflate" => Ok(Self::Deflate),
            b"snappy" => Ok(Self::Snappy),
            b"lzma" => Ok(Self::Lzma),
            b"zstandard" => Ok(Self::Zstandard),
            _ => Err(Error::not_implemented(format!(
                "Unsupported Avro codec: {}",
                String::from_utf8_lossy(value)
            ))),
        }
    }

    fn decompress(&self, bytes: Vec<u8>) -> Result<Vec<u8>> {
        match self {
            Self::Null => Ok(bytes),
            Self::Deflate => read_bounded(flate2::read::DeflateDecoder::new(bytes.as_slice())),
            // Avro C uses raw LZMA2 with liblzma's default 8 MiB dictionary.
            Self::Lzma => read_bounded(lzma_rust2::Lzma2Reader::new(
                bytes.as_slice(),
                8 * 1024 * 1024,
                None,
            )),
            Self::Zstandard => read_bounded(roastedduck_zstd::StreamDecoder::new(bytes.as_slice())),
            Self::Snappy => {
                let split = bytes.len().checked_sub(4).ok_or_else(invalid_block)?;
                let (compressed, checksum) = bytes.split_at(split);
                let size = snap::raw::decompress_len(compressed).map_err(|_| invalid_block())?;
                if size > MAX_BLOCK {
                    return Err(invalid_block());
                }
                let decoded = snap::raw::Decoder::new()
                    .decompress_vec(compressed)
                    .map_err(|_| invalid_block())?;
                if crc32fast::hash(&decoded).to_be_bytes() != checksum {
                    return Err(Error::invalid_input("Avro Snappy block checksum mismatch"));
                }
                Ok(decoded)
            }
        }
    }
}

fn invalid_block() -> Error {
    Error::invalid_input("Invalid or oversized Avro data block")
}

fn read_bounded(reader: impl Read) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    reader
        .take(MAX_BLOCK as u64 + 1)
        .read_to_end(&mut data)
        .map_err(io_error)?;
    if data.len() > MAX_BLOCK {
        return Err(invalid_block());
    }
    Ok(data)
}

fn read_block_number(input: &mut Input) -> Result<usize> {
    let mut bits = 0u64;
    for shift in (0..64).step_by(7) {
        let mut byte = [0];
        input.read_exact(&mut byte).map_err(io_error)?;
        if shift == 63 && byte[0] > 1 {
            return Err(invalid_block());
        }
        bits |= u64::from(byte[0] & 0x7f) << shift;
        if byte[0] < 0x80 {
            return if bits & 1 == 0 {
                usize::try_from(bits >> 1).map_err(|_| invalid_block())
            } else {
                Err(invalid_block())
            };
        }
    }
    Err(invalid_block())
}

pub(super) struct Reader {
    input: Input,
    pub(super) schema: Schema,
    marker: [u8; 16],
    codec: Codec,
    block: Vec<u8>,
    position: usize,
    remaining: usize,
}

impl Reader {
    pub(super) fn open(path: &[u8]) -> Result<Self> {
        let mut input = BufReader::with_capacity(64 * 1024, open_input(path)?);
        let mut magic = [0; 4];
        input.read_exact(&mut magic).map_err(io_error)?;
        if &magic != MAGIC {
            return Err(Error::invalid_input("Incorrect Avro container file magic number"));
        }
        let metadata_schema = Schema::map(Schema::Bytes).build();
        let metadata_reader = GenericDatumReader::builder(&metadata_schema)
            .build()
            .map_err(avro_error)?;
        let Value::Map(mut metadata) = metadata_reader.read_value(&mut input).map_err(avro_error)? else {
            return Err(Error::invalid_input("Invalid Avro container metadata"));
        };
        let Some(Value::Bytes(schema)) = metadata.remove("avro.schema") else {
            return Err(Error::invalid_input(
                "Avro container is missing its writer schema",
            ));
        };
        let json = serde_json::from_slice(&schema)
            .map_err(|error| Error::invalid_input(format!("Invalid Avro schema: {error}")))?;
        let schema = parse_schema(&json)?;
        let codec = match metadata.remove("avro.codec") {
            Some(Value::Bytes(codec)) => Codec::parse(&codec)?,
            _ => Codec::Null,
        };
        let mut marker = [0; 16];
        input.read_exact(&mut marker).map_err(io_error)?;
        Ok(Self {
            input,
            schema,
            marker,
            codec,
            block: Vec::new(),
            position: 0,
            remaining: 0,
        })
    }

    fn next_block(&mut self) -> Result<bool> {
        if self.input.fill_buf().map_err(io_error)?.is_empty() {
            return Ok(false);
        }
        let count = read_block_number(&mut self.input)?;
        let length = read_block_number(&mut self.input)?;
        if count == 0 || length > MAX_BLOCK {
            return Err(invalid_block());
        }
        let mut bytes = vec![0; length];
        self.input.read_exact(&mut bytes).map_err(io_error)?;
        let mut marker = [0; 16];
        self.input.read_exact(&mut marker).map_err(io_error)?;
        if marker != self.marker {
            return Err(Error::invalid_input("Avro data block sync marker mismatch"));
        }
        self.block = self.codec.decompress(bytes)?;
        if count > self.block.len() && count > MAX_BLOCK / std::mem::size_of::<Value>() {
            return Err(invalid_block());
        }
        self.position = 0;
        self.remaining = count;
        Ok(true)
    }

    pub(super) fn read(
        &mut self,
        limit: usize,
        mut consume: impl FnMut(Value) -> Result<()>,
    ) -> Result<usize> {
        let mut read = 0;
        while read < limit {
            if self.remaining == 0 && !self.next_block()? {
                break;
            }
            let decoder = GenericDatumReader::builder(&self.schema)
                .build()
                .map_err(avro_error)?;
            for _ in 0..self.remaining.min(limit - read) {
                let mut bytes = &self.block[self.position..];
                let value = decoder.read_value(&mut bytes).map_err(avro_error)?;
                self.position = self.block.len() - bytes.len();
                self.remaining -= 1;
                if self.remaining == 0 && !bytes.is_empty() {
                    return Err(invalid_block());
                }
                consume(value)?;
                read += 1;
            }
        }
        Ok(read)
    }
}

pub(super) struct Writer {
    output: BufWriter<Output>,
    marker: [u8; 16],
}

impl Writer {
    pub(super) fn new(path: &[u8], json: &Json, metadata: &[(String, String)]) -> Result<Self> {
        let mut marker = [0; 16];
        roastedduck_crypto::generate_random_data(&mut marker)?;
        let mut output = BufWriter::new(Output::create(path)?);
        let schema = serde_json::to_vec(json).map_err(|error| Error::internal(error.to_string()))?;
        let mut metadata = metadata
            .iter()
            .map(|(key, value)| (key.clone(), Value::Bytes(value.as_bytes().to_vec())))
            .collect::<std::collections::HashMap<_, _>>();
        metadata.insert("avro.schema".into(), Value::Bytes(schema));
        metadata.insert("avro.codec".into(), Value::Bytes(b"null".to_vec()));
        let metadata_schema = Schema::map(Schema::Bytes).build();
        let encoder = GenericDatumWriter::builder(&metadata_schema)
            .build()
            .map_err(avro_error)?;

        output.write_all(MAGIC).map_err(io_error)?;
        encoder
            .write_value(&mut output, Value::Map(metadata))
            .map_err(avro_error)?;
        output.write_all(&marker).map_err(io_error)?;
        Ok(Self { output, marker })
    }

    pub(super) fn write_block(&mut self, data: &[u8], count: usize) -> Result<()> {
        if data.len() > MAX_BLOCK {
            return Err(invalid_block());
        }
        let encoder = GenericDatumWriter::builder(&Schema::Long)
            .build()
            .map_err(avro_error)?;
        encoder
            .write_value(&mut self.output, Value::Long(count as i64))
            .map_err(avro_error)?;
        encoder
            .write_value(&mut self.output, Value::Long(data.len() as i64))
            .map_err(avro_error)?;
        self.output.write_all(data).map_err(io_error)?;
        self.output.write_all(&self.marker).map_err(io_error)
    }

    pub(super) fn close(mut self) -> Result<()> {
        self.output.flush().map_err(io_error)?;
        self.output.get_mut().close().map_err(io_error)
    }
}
