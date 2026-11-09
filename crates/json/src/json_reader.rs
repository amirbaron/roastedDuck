//! Reading JSON files (DuckDB's `json_reader.cpp`): files are read in buffers, the format
//! (newline-delimited, array, unstructured) and record type are detected from the first
//! buffer, and values are split off and parsed in place, `STANDARD_VECTOR_SIZE` at a time.
//!
//! A reader either scans a whole file, or — for newline-delimited files read by several
//! threads — the lines that start in a byte range of it.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::sync::Arc;

use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_csv::options::FileCompressionType;

use crate::json_enums::{JsonFormat, JsonRecordType, JsonScanType};
use crate::json_reader_options::{JsonReaderOptions, STANDARD_VECTOR_SIZE};
use crate::yyjson::{JsonArena, PADDING_SIZE, ReadError, ReadErrorCode, Val, ValId, read_insitu};

/// `StringUtil::CharacterIsSpace`.
#[inline]
pub(crate) fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// An open JSON file, decompressed (DuckDB's `JSONFileHandle`).
pub(crate) struct JsonFileHandle {
    reader: Box<dyn Read + Send>,
    file_size: u64,
}

/// Positional, bounded reads through the database's existing external file cache.
/// The cache's buffer manager owns accounting and eviction; readers retain no copy.
struct CachedJsonReader {
    handle: roastedduck_function::caching_file_system::CachingFileHandle,
    position: u64,
    size: u64,
}

impl Read for CachedJsonReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let count = (output.len() as u64).min(self.size.saturating_sub(self.position)) as usize;
        if count == 0 {
            return Ok(0);
        }
        let data = self
            .handle
            .read(count as u64, self.position)
            .map_err(std::io::Error::other)?;
        output[..count].copy_from_slice(data.bytes());
        self.position += count as u64;
        Ok(count)
    }
}

impl std::fmt::Debug for JsonFileHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonFileHandle")
            .field("file_size", &self.file_size)
            .finish_non_exhaustive()
    }
}

/// Whether `path` is a pipe (`LocalFileSystem::IsPipe`: a FIFO).
pub(crate) fn is_pipe(path: &[u8]) -> bool {
    use std::os::unix::fs::FileTypeExt;
    roastedduck_function::file_system::native_path(path)
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .is_some_and(|metadata| metadata.file_type().is_fifo())
}

/// Size of a local or database-owned immutable file, without materializing its bytes.
pub(crate) fn file_size(path: &[u8]) -> u64 {
    if let Some(fs) = roastedduck_function::registered_files::lookup(path) {
        return fs.open(path, false).and_then(|h| h.size()).unwrap_or(0);
    }
    if let Some(file) = roastedduck_function::object_store::lookup(path) {
        return file.bytes.len() as u64;
    }
    roastedduck_function::file_system::native_path(path)
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .map_or(0, |m| m.len())
}

/// Reads the whole pipe at `path`, still compressed.
pub(crate) fn read_pipe(path: &[u8]) -> Result<Vec<u8>> {
    roastedduck_function::file_system::verify_can_access_file(path)?;
    let mut file = File::open(roastedduck_function::file_system::native_path(path)?)
        .map_err(|error| roastedduck_csv::buffer::open_error(path, &error))?;
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)
        .map_err(|error| Error::new(ErrorKind::Io, error.to_string()))?;
    Ok(contents)
}

impl JsonFileHandle {
    /// Opens `path` (`FileSystem::OpenFile` with the compression of the options); a pipe
    /// read at bind time is read from its `contents`.
    pub(crate) fn open(
        path: &[u8],
        compression: FileCompressionType,
        start: u64,
        contents: Option<Arc<[u8]>>,
    ) -> Result<Self> {
        if let Some(contents) = contents {
            let compression = compression.resolve(path);
            let raw: Box<dyn Read + Send> = Box::new(std::io::Cursor::new(contents));
            let reader: Box<dyn Read + Send> = match compression {
                FileCompressionType::Gzip => Box::new(flate2::read::MultiGzDecoder::new(raw)),
                FileCompressionType::Zstd => Box::new(roastedduck_zstd::StreamDecoder::new(raw)),
                _ => raw,
            };
            // `GetFileSize` of a pipe.
            return Ok(Self { reader, file_size: 0 });
        }
        roastedduck_function::file_system::verify_can_access_file(path)?;
        if let Some(fs) = roastedduck_function::registered_files::lookup(path) {
            let (cursor, file_size): (Box<dyn Read + Send>, u64) = if fs.is_remote() {
                use roastedduck_function::caching_file_system::CachingFileHandle;
                use roastedduck_function::external_file_cache::CacheValidationMode;
                let client = roastedduck_function::file_system::opener();
                let handle = CachingFileHandle::open(
                    client.as_ref().and_then(|client| client.external_file_cache()),
                    path,
                    CacheValidationMode::of_client(client.as_deref()),
                )?;
                let size = handle.file_size()?;
                (
                    Box::new(CachedJsonReader {
                        handle,
                        position: start,
                        size,
                    }),
                    size,
                )
            } else {
                let handle = fs.open(path, false)?;
                let size = handle.size()?;
                let mut cursor = roastedduck_function::registered_files::Cursor::new(handle);
                cursor
                    .seek(SeekFrom::Start(start))
                    .map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?;
                (Box::new(cursor), size)
            };
            let mut buffered = BufReader::with_capacity(1 << 16, cursor);
            let compression = compression.resolve(path);
            if compression == FileCompressionType::Zstd {
                roastedduck_function::file_system::verify_zstd_file_system()?;
            }
            if compression == FileCompressionType::Gzip && start == 0 {
                roastedduck_csv::buffer::verify_gzip_header(&mut buffered, path)?;
            }
            let raw: Box<dyn Read + Send> = Box::new(buffered);
            let reader: Box<dyn Read + Send> = match compression {
                FileCompressionType::Gzip => Box::new(flate2::read::MultiGzDecoder::new(raw)),
                FileCompressionType::Zstd => Box::new(roastedduck_zstd::StreamDecoder::new(raw)),
                _ => raw,
            };
            return Ok(Self { reader, file_size });
        }
        if let Some(file) = roastedduck_function::object_store::lookup(path) {
            let file_size = file.bytes.len() as u64;
            let mut cursor = std::io::Cursor::new(Arc::clone(&file.bytes));
            cursor
                .seek(SeekFrom::Start(start))
                .map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?;
            let compression = compression.resolve(path);
            if compression == FileCompressionType::Zstd {
                roastedduck_function::file_system::verify_zstd_file_system()?;
            }
            let mut buffered = BufReader::with_capacity(1 << 16, cursor);
            if compression == FileCompressionType::Gzip && start == 0 {
                roastedduck_csv::buffer::verify_gzip_header(&mut buffered, path)?;
            }
            let raw: Box<dyn Read + Send> = Box::new(buffered);
            let reader: Box<dyn Read + Send> = match compression {
                FileCompressionType::Gzip => Box::new(flate2::read::MultiGzDecoder::new(raw)),
                FileCompressionType::Zstd => Box::new(roastedduck_zstd::StreamDecoder::new(raw)),
                _ => raw,
            };
            return Ok(Self { reader, file_size });
        }
        let mut file = File::open(roastedduck_function::file_system::native_path(path)?)
            .map_err(|error| roastedduck_csv::buffer::open_error(path, &error))?;
        let metadata = file
            .metadata()
            .map_err(|error| roastedduck_csv::buffer::open_error(path, &error))?;
        if metadata.is_dir() {
            return Err(Error::new_bytes(
                ErrorKind::Io,
                roastedduck_common::byte_format!("Could not read from file \"{path}\": Is a directory"),
            ));
        }
        let compression = compression.resolve(path);
        if compression == FileCompressionType::Zstd {
            roastedduck_function::file_system::verify_zstd_file_system()?;
        }
        if start > 0 {
            file.seek(SeekFrom::Start(start))
                .map_err(|error| Error::new(ErrorKind::Io, error.to_string()))?;
        }
        let mut buffered = BufReader::with_capacity(1 << 16, file);
        if compression == FileCompressionType::Gzip && start == 0 {
            roastedduck_csv::buffer::verify_gzip_header(&mut buffered, path)?;
        }
        let raw: Box<dyn Read + Send> = Box::new(buffered);
        let reader: Box<dyn Read + Send> = match compression {
            FileCompressionType::Gzip => Box::new(flate2::read::MultiGzDecoder::new(raw)),
            FileCompressionType::Zstd => Box::new(roastedduck_zstd::StreamDecoder::new(raw)),
            _ => raw,
        };
        Ok(Self {
            reader,
            file_size: metadata.len(),
        })
    }

    pub(crate) fn file_size(&self) -> u64 {
        self.file_size
    }

    /// Reads until `buffer` is full or the file ends (`ReadInternal`).
    pub(crate) fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        let mut total = 0;
        while total < buffer.len() {
            match self.reader.read(&mut buffer[total..]) {
                Ok(0) => break,
                Ok(read) => total += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(Error::new(ErrorKind::Io, error.to_string())),
            }
        }
        Ok(total)
    }
}

/// `SkipWhitespace`.
fn skip_whitespace(buffer: &[u8], mut offset: usize, size: usize) -> usize {
    while offset != size && is_space(buffer[offset]) {
        offset += 1;
    }
    offset
}

/// `NextJSONDefault`: the end of an object, array or string starting at `ptr`.
fn next_json_default(buffer: &[u8], mut ptr: usize, end: usize) -> usize {
    let mut parents: usize = 0;
    while ptr != end {
        let c = buffer[ptr];
        ptr += 1;
        match c {
            b'{' | b'[' => {
                parents += 1;
                continue;
            }
            b'}' | b']' => parents = parents.wrapping_sub(1),
            b'"' => {
                while ptr != end {
                    let string_char = buffer[ptr];
                    ptr += 1;
                    if string_char == b'"' {
                        break;
                    } else if string_char == b'\\' && ptr != end {
                        ptr += 1;
                    }
                }
            }
            _ => continue,
        }
        if parents == 0 {
            break;
        }
    }
    ptr
}

/// `NextJSON`: the end of the value starting at `start` (not whitespace), if it ends
/// before `start + size`.
fn next_json(buffer: &[u8], start: usize, size: usize) -> Option<usize> {
    let end = start + size;
    let ptr = match buffer[start] {
        b'{' | b'[' | b'"' => next_json_default(buffer, start, end),
        _ => {
            // A value without clear "parents" in an array: up to the next ',' or ']'.
            let mut ptr = start;
            while ptr != end {
                let c = buffer[ptr];
                ptr += 1;
                if c == b',' || c == b']' {
                    ptr -= 1;
                    break;
                }
            }
            ptr
        }
    };
    (ptr != end).then_some(ptr)
}

/// `DetectFormatAndRecordType`: from the first buffer of a file.
pub(crate) fn detect_format_and_record_type(buffer: &[u8]) -> (JsonFormat, JsonRecordType) {
    let size = buffer.len();
    let mut arena = JsonArena::default();
    if let Some(line_end) = memchr(b'\n', buffer) {
        let line_size = skip_whitespace(buffer, line_end, size);
        if let Ok(doc) = arena.read(&buffer[..line_size], false) {
            let view = arena.doc();
            if view.is_arr(doc.root) && line_size == size {
                let first_is_obj = view.arr_get(doc.root, 0).is_some_and(|first| view.is_obj(first));
                if view.arr_size(doc.root) == 0 || first_is_obj {
                    return (JsonFormat::Array, JsonRecordType::Records);
                }
                return (JsonFormat::Array, JsonRecordType::Values);
            }
            if view.is_obj(doc.root) {
                return (JsonFormat::NewlineDelimited, JsonRecordType::Records);
            }
            return (JsonFormat::NewlineDelimited, JsonRecordType::Values);
        }
    }
    let offset = skip_whitespace(buffer, 0, size);
    let remaining = size - offset;
    if remaining == 0 || buffer[offset] == b'{' {
        return (JsonFormat::Unstructured, JsonRecordType::Records);
    }
    if buffer[offset] != b'[' {
        return (JsonFormat::Unstructured, JsonRecordType::Values);
    }
    arena.reset();
    if let Ok(doc) = arena.read(&buffer[offset..], true) {
        let after = skip_whitespace(buffer, offset + doc.read_size, size);
        if after != size {
            return (JsonFormat::Unstructured, JsonRecordType::Values);
        }
        let view = arena.doc();
        let first_is_obj = view.arr_get(doc.root, 0).is_some_and(|first| view.is_obj(first));
        if view.arr_size(doc.root) == 0 || first_is_obj {
            return (JsonFormat::Array, JsonRecordType::Records);
        }
        return (JsonFormat::Array, JsonRecordType::Values);
    }
    // Not a complete array in this buffer: look at the first element.
    let after_bracket = skip_whitespace(buffer, offset + 1, size);
    if after_bracket == size || buffer[after_bracket] == b'{' {
        return (JsonFormat::Array, JsonRecordType::Records);
    }
    (JsonFormat::Array, JsonRecordType::Values)
}

/// The position of `byte` in `haystack`.
#[inline]
pub(crate) fn memchr(byte: u8, haystack: &[u8]) -> Option<usize> {
    memchr::memchr(byte, haystack)
}

/// The values of one scan step: value slots parsed in place in the reader's buffer, and
/// for each value its root (`None` for errors ignored with `ignore_errors`) and its
/// trimmed text (`JSONReaderScanState::values` / `units`).
#[derive(Debug, Default)]
pub(crate) struct ScanBatch {
    pub(crate) vals: Vec<Val>,
    pub(crate) values: Vec<Option<ValId>>,
    pub(crate) units: Vec<(usize, usize)>,
    /// Values parsed from copies (`read_json_objects`).
    pub(crate) arena: JsonArena,
    /// The number of values (lines or objects) of the file before this batch.
    pub(crate) first_value: u64,
}

impl ScanBatch {
    fn clear(&mut self) {
        self.vals.clear();
        self.values.clear();
        self.units.clear();
        self.arena.reset();
    }
}

/// The byte range of a file a partial reader scans: the lines starting in `start..end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ByteRange {
    pub(crate) start: u64,
    pub(crate) end: u64,
}

/// Reads one file (or a byte range of it) (`JSONReader` with its `JSONReaderScanState`).
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors DuckDB's reader state flags"
)]
pub(crate) struct JsonReader {
    pub(crate) path: roastedduck_common::ByteString,
    scan_type: JsonScanType,
    ignore_errors: bool,
    maximum_object_size: u64,
    compression: FileCompressionType,
    pub(crate) format: JsonFormat,
    pub(crate) record_type: JsonRecordType,
    handle: Option<JsonFileHandle>,
    /// The data (`buffer_size` bytes) followed by zero padding.
    buffer: Vec<u8>,
    buffer_capacity: usize,
    buffer_size: usize,
    buffer_offset: usize,
    /// Unparsed bytes at the end of the buffer, moved to the next one.
    remainder: Option<usize>,
    /// The first buffer, read while detecting the format.
    detected: Option<Vec<u8>>,
    buffer_index: u64,
    is_last: bool,
    initialized: bool,
    /// Values parsed so far.
    values_seen: u64,
    range: Option<ByteRange>,
    /// The file offset of the first byte of the buffer (partial reads).
    buffer_file_offset: u64,
    /// A partial reader skips to the first line that starts in its range.
    skip_to_line_start: bool,
    /// The contents of the file if it is a pipe, read when binding.
    pipe_contents: Option<Arc<[u8]>>,
}

impl std::fmt::Debug for JsonReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonReader")
            .field("path", &self.path)
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

impl JsonReader {
    /// A reader of `path`; `buffer_capacity` is twice the maximum object size.
    pub(crate) fn new(path: &[u8], options: &JsonReaderOptions, range: Option<ByteRange>) -> Self {
        Self {
            path: path.into(),
            scan_type: options.scan_type,
            ignore_errors: options.ignore_errors,
            maximum_object_size: options.maximum_object_size,
            compression: options.compression,
            format: options.format,
            record_type: options.record_type,
            handle: None,
            buffer: Vec::new(),
            buffer_capacity: usize::try_from(options.maximum_object_size.saturating_mul(2))
                .unwrap_or(usize::MAX),
            buffer_size: 0,
            buffer_offset: 0,
            remainder: None,
            detected: None,
            buffer_index: 0,
            is_last: false,
            initialized: false,
            values_seen: 0,
            range,
            buffer_file_offset: 0,
            skip_to_line_start: range.is_some_and(|range| range.start > 0),
            pipe_contents: options.pipe_contents.get(path),
        }
    }

    /// The size of the file, once opened.
    pub(crate) fn file_size(&self) -> u64 {
        self.handle.as_ref().map_or(0, JsonFileHandle::file_size)
    }

    /// `Initialize`: opens the file and detects its format if needed.
    pub(crate) fn initialize(&mut self) -> Result<()> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        let start = self.range.map_or(0, |range| range.start.saturating_sub(1));
        self.handle = Some(JsonFileHandle::open(
            &self.path,
            self.compression,
            start,
            self.pipe_contents.clone(),
        )?);
        self.buffer_file_offset = start;
        if self.range.is_none()
            && (self.record_type == JsonRecordType::AutoDetect || self.format == JsonFormat::AutoDetect)
        {
            self.auto_detect()?;
        }
        Ok(())
    }

    /// `AutoDetect`: reads the first buffer and detects the format from it.
    fn auto_detect(&mut self) -> Result<()> {
        let Some(handle) = self.handle.as_mut() else {
            return Ok(());
        };
        let request = self.buffer_capacity.saturating_sub(PADDING_SIZE);
        let data = read_up_to(handle, request)?;
        if data.is_empty() {
            return Ok(());
        }
        let (format, record_type) = detect_format_and_record_type(&data);
        if self.format == JsonFormat::AutoDetect {
            self.format = format;
        }
        if self.record_type == JsonRecordType::AutoDetect {
            self.record_type = record_type;
        }
        self.detected = Some(data);
        Ok(())
    }

    /// The error prefix and line of a value (`AddParseError` / `AddTransformError`): the
    /// number of values before it in the file.
    fn values_before(&self, values_in_batch: u64) -> Result<u64> {
        let before_range = match self.range {
            Some(range) if range.start > 0 => count_lines_before(&self.path, range.start)?,
            _ => 0,
        };
        Ok(before_range + self.values_seen + values_in_batch)
    }

    /// The unit of error messages.
    fn unit(&self) -> &'static str {
        if self.format == JsonFormat::NewlineDelimited {
            "line"
        } else {
            "record/value"
        }
    }

    /// `AddParseError`.
    fn parse_error(&self, values_in_batch: u64, error: &ReadError, extra: &str) -> Result<Error> {
        // DuckDB reports the 0-based value index plus two.
        let line = self.values_before(values_in_batch)? + 2;
        Ok(Error::invalid_input(roastedduck_common::byte_format!(
            "Malformed JSON in file \"{}\", at byte {} in {} {line}: {}. {extra}",
            self.path,
            error.pos + 1,
            self.unit(),
            error.msg
        )))
    }

    /// `AddTransformError`: an error converting value `object_index` of the last batch.
    pub(crate) fn transform_error(
        &self,
        batch: &ScanBatch,
        object_index: usize,
        message: &[u8],
    ) -> Result<Error> {
        let line = batch.first_value + object_index as u64 + 1;
        let line = match self.range {
            Some(range) if range.start > 0 => line + count_lines_before(&self.path, range.start)?,
            _ => line,
        };
        let mut text = roastedduck_common::byte_format!(
            "JSON transform error in file \"{}\", in {} {line}: ",
            self.path,
            self.unit()
        )
        .into_bytes();
        text.extend_from_slice(message);
        Ok(Error::new_bytes(
            roastedduck_common::ErrorKind::InvalidInput,
            text,
        ))
    }

    /// `ThrowObjectSizeError`.
    fn object_size_error(&self, object_size: usize) -> Error {
        Error::invalid_input(roastedduck_common::byte_format!(
            "\"maximum_object_size\" of {} bytes exceeded while reading file \"{}\" (>{object_size} bytes).\n Try increasing \"maximum_object_size\".",
            self.maximum_object_size,
            self.path
        ))
    }

    /// Reads the next buffer, keeping the unparsed remainder of the previous one
    /// (`ReadNextBuffer`); `false` at the end of the file.
    fn read_next_buffer(&mut self) -> Result<bool> {
        if self.is_last {
            return Ok(false);
        }
        let remainder = if let Some(offset) = self.remainder.take() {
            let len = self.buffer_size - offset;
            self.buffer.copy_within(offset..self.buffer_size, 0);
            self.buffer_file_offset += offset as u64;
            len
        } else {
            self.buffer_file_offset += self.buffer_size as u64;
            0
        };
        let first = self.buffer_index == 0;
        let read = if first && let Some(data) = self.detected.take() {
            self.buffer = data;
            self.buffer.len()
        } else {
            let Some(handle) = self.handle.as_mut() else {
                return Ok(false);
            };
            let request = if self.range.is_some() {
                // Partial reads use smaller buffers; lines may extend them.
                (8 << 20usize)
                    .max(remainder + 1)
                    .min(self.buffer_capacity.saturating_sub(PADDING_SIZE))
            } else {
                self.buffer_capacity.saturating_sub(remainder + PADDING_SIZE)
            };
            self.buffer.truncate(remainder);
            let read = append_up_to(handle, &mut self.buffer, request.saturating_sub(remainder).max(1))?;
            if read == 0 {
                self.handle = None;
            }
            read
        };
        self.buffer_index += 1;
        self.is_last = read == 0;
        self.buffer_size = remainder + read;
        self.buffer.truncate(self.buffer_size);
        self.buffer.extend_from_slice(&[0; PADDING_SIZE]);
        self.buffer_offset = 0;
        if self.skip_to_line_start && !self.is_last {
            // Start after the first newline at or after `start - 1`.
            match memchr(b'\n', &self.buffer[..self.buffer_size]) {
                Some(newline) => {
                    self.buffer_offset = newline + 1;
                    self.skip_to_line_start = false;
                }
                None => self.buffer_offset = self.buffer_size,
            }
        }
        if first && !self.is_last && self.range.is_none() {
            if self.buffer_size >= 3 && self.buffer[..3] == [0xEF, 0xBB, 0xBF] {
                self.buffer_offset = 3;
            }
            if self.format == JsonFormat::Array {
                self.skip_over_array_start()?;
            }
        }
        Ok(true)
    }

    /// `SkipOverArrayStart`.
    fn skip_over_array_start(&mut self) -> Result<()> {
        let size = self.buffer_size;
        self.buffer_offset = skip_whitespace(&self.buffer, self.buffer_offset, size);
        if self.buffer_offset == size {
            return Ok(());
        }
        if self.buffer[self.buffer_offset] != b'[' {
            return Err(Error::invalid_input(roastedduck_common::byte_format!(
                "Expected top-level JSON array with format='array', but first character is '{}' in file \"{}\".\n Try setting format='auto' or format='newline_delimited'.",
                char::from(self.buffer[self.buffer_offset]),
                self.path
            )));
        }
        self.buffer_offset = skip_whitespace(&self.buffer, self.buffer_offset + 1, size);
        if self.buffer_offset >= size {
            return Err(Error::invalid_input(roastedduck_common::byte_format!(
                "Missing closing brace ']' in JSON array with format='array' in file \"{}\"",
                self.path
            )));
        }
        if self.buffer[self.buffer_offset] == b']' {
            self.buffer_offset = skip_whitespace(&self.buffer, self.buffer_offset + 1, size);
            if self.buffer_offset != size {
                return Err(Error::invalid_input(roastedduck_common::byte_format!(
                    "Empty array with trailing data when parsing JSON array with format='array' in file \"{}\"",
                    self.path
                )));
            }
        }
        Ok(())
    }

    /// Whether the line of the value starting at buffer offset `offset` starts past the
    /// reader's range.
    fn past_range(&self, offset: usize) -> bool {
        let Some(range) = self.range else {
            return false;
        };
        let line_start = self.buffer[..offset]
            .iter()
            .rposition(|&c| c == b'\n')
            .map_or(0, |newline| newline + 1);
        self.buffer_file_offset + line_start as u64 >= range.end
    }

    /// `Scan`: the next values (up to `STANDARD_VECTOR_SIZE`); 0 at the end.
    pub(crate) fn scan(&mut self, batch: &mut ScanBatch) -> Result<usize> {
        self.initialize()?;
        batch.clear();
        batch.first_value = self.values_seen;
        loop {
            while self.buffer_offset >= self.buffer_size {
                if !self.read_next_buffer()? {
                    return Ok(0);
                }
            }
            let done = self.parse_next_chunk(batch)?;
            if !batch.values.is_empty() || done {
                self.values_seen += batch.values.len() as u64;
                return Ok(batch.values.len());
            }
        }
    }

    /// `ParseNextChunk`: splits off and parses values from the buffer. Returns `true` if
    /// the reader is done (end of its range).
    fn parse_next_chunk(&mut self, batch: &mut ScanBatch) -> Result<bool> {
        let size = self.buffer_size;
        while batch.values.len() < STANDARD_VECTOR_SIZE {
            self.buffer_offset = skip_whitespace(&self.buffer, self.buffer_offset, size);
            let start = self.buffer_offset;
            let remaining = size - start;
            if remaining == 0 {
                break;
            }
            if self.past_range(start) {
                self.is_last = true;
                self.buffer_offset = size;
                self.handle = None;
                return Ok(true);
            }
            let end = if self.format == JsonFormat::NewlineDelimited {
                memchr(b'\n', &self.buffer[start..size]).map(|offset| start + offset)
            } else {
                next_json(&self.buffer, start, remaining)
            };
            let end = if let Some(end) = end {
                end
            } else {
                if remaining as u64 > self.maximum_object_size {
                    return Err(self.object_size_error(remaining));
                }
                if !self.is_last {
                    self.remainder = Some(start);
                    self.buffer_offset = size;
                    break;
                }
                start + remaining
            };
            let json_size = end - start;
            self.parse_json(batch, start, json_size, remaining)?;
            self.buffer_offset += json_size;
            if self.format == JsonFormat::Array {
                self.buffer_offset = skip_whitespace(&self.buffer, self.buffer_offset, size);
                let c = self.buffer[self.buffer_offset];
                if c == b',' || c == b']' {
                    self.buffer_offset += 1;
                } else {
                    let error = ReadError {
                        code: ReadErrorCode::UnexpectedCharacter,
                        msg: "unexpected character",
                        pos: json_size,
                    };
                    let values_in_batch = batch.values.len() as u64;
                    return Err(self.parse_error(values_in_batch, &error, "")?);
                }
            }
            self.buffer_offset = skip_whitespace(&self.buffer, self.buffer_offset, size);
        }
        Ok(false)
    }

    /// `ParseJSON`: parses the value at `start` (`json_size` bytes, `remaining` in the
    /// buffer) and appends it to the batch.
    fn parse_json(
        &mut self,
        batch: &mut ScanBatch,
        start: usize,
        json_size: usize,
        remaining: usize,
    ) -> Result<()> {
        let values_in_batch = batch.values.len() as u64;
        let parsed = if self.scan_type == JsonScanType::ReadJsonObjects {
            batch.arena.read(&self.buffer[start..start + json_size], true)
        } else {
            read_insitu(&mut self.buffer, start, start + remaining, true, &mut batch.vals)
        };
        let mut root = match parsed {
            Ok(result) => Some(result),
            Err(error) => {
                let mut extra = "";
                let mut can_ignore = self.ignore_errors;
                if self.format != JsonFormat::NewlineDelimited {
                    can_ignore = false;
                    if self.ignore_errors {
                        extra =
                            "Parse errors cannot be ignored for JSON formats other than 'newline_delimited'";
                    }
                }
                if !can_ignore {
                    return Err(self.parse_error(values_in_batch, &error, extra)?);
                }
                None
            }
        };
        let read_size = root.map_or(0, |result| result.read_size);
        if read_size > json_size {
            let error = ReadError {
                code: ReadErrorCode::UnexpectedEnd,
                msg: "unexpected end of data",
                pos: json_size,
            };
            return Err(self.parse_error(values_in_batch, &error, "Try auto-detecting the JSON format")?);
        }
        if read_size < json_size {
            let off = skip_whitespace(&self.buffer, start + read_size, start + json_size);
            if off != start + json_size {
                if !self.ignore_errors {
                    let error = ReadError {
                        code: ReadErrorCode::UnexpectedContent,
                        msg: "unexpected content after document",
                        pos: read_size,
                    };
                    return Err(self.parse_error(
                        values_in_batch,
                        &error,
                        "Try auto-detecting the JSON format",
                    )?);
                }
                root = None;
            }
        }
        batch.values.push(root.map(|result| result.root));
        // The trimmed text of the value.
        let mut unit_start = start;
        let mut unit_end = start + json_size;
        while unit_start < unit_end && is_space(self.buffer[unit_start]) {
            unit_start += 1;
        }
        while unit_end > unit_start && is_space(self.buffer[unit_end - 1]) {
            unit_end -= 1;
        }
        batch.units.push((unit_start, unit_end - unit_start));
        Ok(())
    }

    /// The position of the scan in the current buffer.
    pub(crate) fn buffer_offset(&self) -> usize {
        self.buffer_offset
    }

    /// The bytes the batch's values and units refer to.
    pub(crate) fn buffer(&self) -> &[u8] {
        &self.buffer
    }
}

/// Reads up to `size` bytes from the start of `handle`.
fn read_up_to(handle: &mut JsonFileHandle, size: usize) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    append_up_to(handle, &mut data, size)?;
    Ok(data)
}

/// Appends up to `size` bytes of `handle` to `data`, growing it as data arrives (small
/// files only take what they need); returns the number of bytes read.
fn append_up_to(handle: &mut JsonFileHandle, data: &mut Vec<u8>, size: usize) -> Result<usize> {
    let start = data.len();
    let expected = size.min(usize::try_from(handle.file_size()).unwrap_or(size));
    if expected != 0 {
        data.reserve_exact(expected.saturating_add(PADDING_SIZE));
    }
    let mut total = 0;
    let mut scratch = vec![0u8; 1 << 16];
    while total < size {
        let chunk = scratch.len().min(size - total);
        let read = handle.read(&mut scratch[..chunk])?;
        if read == 0 {
            break;
        }
        // Reserve padding together with actual input. Reading into spare tail
        // space first would grow/zero a large allocation even on the EOF probe.
        data.reserve(read + PADDING_SIZE);
        data.extend_from_slice(&scratch[..read]);
        total += read;
        if read < chunk {
            break;
        }
    }
    debug_assert_eq!(data.len(), start + total);
    Ok(total)
}

/// The number of values (non-blank lines) of a newline-delimited file before `offset`,
/// for line numbers of errors found by partial readers.
fn count_lines_before(path: &[u8], offset: u64) -> Result<u64> {
    let mut handle = JsonFileHandle::open(path, FileCompressionType::Uncompressed, 0, None)?;
    let mut remaining = offset;
    let mut buffer = vec![0u8; 1 << 16];
    let mut count = 0;
    let mut line_has_content = false;
    while remaining > 0 {
        let want = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = handle.read(&mut buffer[..want])?;
        if read == 0 {
            break;
        }
        for &c in &buffer[..read] {
            if c == b'\n' {
                if line_has_content {
                    count += 1;
                }
                line_has_content = false;
            } else if !is_space(c) {
                line_has_content = true;
            }
        }
        remaining -= read as u64;
    }
    // A line that started before `offset` belongs to the previous ranges.
    if line_has_content {
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod memory_file_tests {
    use super::*;
    use roastedduck_function::{FunctionClient, ObjectCache, object_store::ObjectStore};
    use std::io::Write;

    #[test]
    fn snapshot_reader_retains_bytes_after_name_release() -> Result<()> {
        let cache = Arc::new(ObjectCache::default());
        let client = Arc::new(FunctionClient::default());
        client.set_object_cache(Arc::clone(&cache));
        let _scope = roastedduck_function::file_system::activate_opener(client);
        let path = b"DUCKDB_INTERNAL_OBJECTSTORE://json-offset";
        let owner = ObjectStore::register(
            &cache,
            path.as_slice().into(),
            Arc::from(b"skip{\"i\":42}".as_slice()),
        );
        assert_eq!(file_size(path), 12);
        let mut reader = JsonFileHandle::open(path, FileCompressionType::Uncompressed, 4, None)?;
        assert_eq!(reader.file_size(), 12);
        drop(owner);
        assert!(roastedduck_function::object_store::lookup(path).is_none());
        let mut buffer = [0; 32];
        let size = reader.read(&mut buffer)?;
        assert_eq!(&buffer[..size], b"{\"i\":42}");
        assert_eq!(reader.read(&mut buffer)?, 0);
        Ok(())
    }

    #[test]
    fn compressed_snapshot_uses_native_gzip_reader() -> Result<()> {
        let cache = Arc::new(ObjectCache::default());
        let client = Arc::new(FunctionClient::default());
        client.set_object_cache(Arc::clone(&cache));
        let _scope = roastedduck_function::file_system::activate_opener(client);
        let text = b"{\"i\":42}\n";
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(text)
            .map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?;
        let compressed = encoder
            .finish()
            .map_err(|e| Error::new(ErrorKind::Io, e.to_string()))?;
        let path = b"DUCKDB_INTERNAL_OBJECTSTORE://json-gzip";
        let owner = ObjectStore::register(&cache, path.as_slice().into(), Arc::from(compressed));
        let mut reader = JsonFileHandle::open(path, FileCompressionType::Gzip, 0, None)?;
        drop(owner);
        let mut buffer = [0; 32];
        let size = reader.read(&mut buffer)?;
        assert_eq!(&buffer[..size], text);
        assert_eq!(reader.read(&mut buffer)?, 0);
        Ok(())
    }
}
