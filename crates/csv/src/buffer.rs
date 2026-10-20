//! Reading CSV files into buffers (DuckDB's `csv_file_handle`, `csv_encoder`, `csv_buffer`
//! and `csv_buffer_manager`): the file is decompressed and transcoded to UTF-8 and read in
//! buffers of `buffer_size` bytes, which scanners share.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::logging::FileSystemLogger;

use crate::options::{CsvReaderOptions, FileCompressionType};

/// A UTF-16 / Latin-1 to UTF-8 transcoder over a byte stream (DuckDB's `CSVEncoder` with
/// `DecodeUTF16ToUTF8` / `DecodeLatin1ToUTF8`).
struct Transcoder<R> {
    inner: R,
    utf16: bool,
    pending: Vec<u8>,
    pending_pos: usize,
    /// A leftover odd byte of UTF-16 input.
    carry: Option<u8>,
}

impl<R: Read> Transcoder<R> {
    fn fill(&mut self) -> std::io::Result<bool> {
        let mut raw = vec![0u8; 64 * 1024];
        let mut start = 0;
        if let Some(byte) = self.carry.take() {
            raw[0] = byte;
            start = 1;
        }
        let read = self.inner.read(&mut raw[start..])?;
        let total = start + read;
        if total == 0 {
            return Ok(false);
        }
        self.pending.clear();
        self.pending_pos = 0;
        if self.utf16 {
            let even = total - total % 2;
            if even < total {
                if read == 0 {
                    // A trailing odd byte is dropped like DuckDB's two-byte lookups do.
                } else {
                    self.carry = Some(raw[even]);
                }
            }
            for pair in raw[..even].as_chunks::<2>().0 {
                let ch = u16::from(pair[0]) | (u16::from(pair[1]) << 8);
                if (0xD800..=0xDFFF).contains(&ch) {
                    return Err(std::io::Error::other("File is not utf-16 encoded"));
                }
                if ch <= 0x7F {
                    self.pending.push(ch as u8);
                } else if ch <= 0x07FF {
                    self.pending.push(0xC0 | (ch >> 6) as u8);
                    self.pending.push(0x80 | (ch & 0x3F) as u8);
                } else {
                    self.pending.push(0xE0 | (ch >> 12) as u8);
                    self.pending.push(0x80 | ((ch >> 6) & 0x3F) as u8);
                    self.pending.push(0x80 | (ch & 0x3F) as u8);
                }
            }
            if self.pending.is_empty() && read != 0 {
                return self.fill();
            }
        } else {
            for &ch in &raw[..total] {
                if ch > 0x7F && ch <= 0x9F {
                    return Err(std::io::Error::other("File is not latin-1 encoded"));
                }
                if ch <= 0x7F {
                    self.pending.push(ch);
                } else {
                    self.pending.push(0xC2 + u8::from(ch > 0xBF));
                    self.pending.push((ch & 0x3F) + 0x80);
                }
            }
        }
        Ok(!self.pending.is_empty())
    }
}

impl<R: Read> Read for Transcoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pending_pos >= self.pending.len() && !self.fill()? {
            return Ok(0);
        }
        let available = &self.pending[self.pending_pos..];
        let count = available.len().min(buf.len());
        buf[..count].copy_from_slice(&available[..count]);
        self.pending_pos += count;
        Ok(count)
    }
}

/// An open CSV file (DuckDB's `CSVFileHandle`).
pub struct CsvFileHandle {
    path: String,
    compression: FileCompressionType,
    encoding: String,
    reader: Box<dyn Read + Send>,
    file_size: u64,
    finished: bool,
    can_seek: bool,
}

impl std::fmt::Debug for CsvFileHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CsvFileHandle")
            .field("path", &self.path)
            .field("compression", &self.compression)
            .finish_non_exhaustive()
    }
}

/// A local file whose reads go to the connection's `FileSystem` log (DuckDB's
/// `FileHandle` of the `LocalFileSystem`, which logs `OPEN`, every `READ` and `CLOSE`).
struct LoggedFile {
    file: File,
    logger: Option<FileSystemLogger>,
    /// Where the next read starts.
    position: u64,
}

impl Read for LoggedFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.file.read(buffer)?;
        if let Some(logger) = &self.logger {
            logger.read(read as u64, self.position);
        }
        self.position += read as u64;
        Ok(read)
    }
}

impl Drop for LoggedFile {
    fn drop(&mut self) {
        if let Some(logger) = &self.logger {
            logger.close();
        }
    }
}

/// The error of a file that cannot be opened (DuckDB's `LocalFileSystem::OpenFile`).
pub fn open_error(path: &str, error: &std::io::Error) -> Error {
    let reason = match error.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory".to_owned(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".to_owned(),
        _ if error.raw_os_error() == Some(21) => "Is a directory".to_owned(),
        _ => error.to_string(),
    };
    Error::new(ErrorKind::Io, format!("Cannot open file \"{path}\": {reason}"))
}

fn io_error(error: &std::io::Error) -> Error {
    let message = error.to_string();
    if message.starts_with("File is not") {
        return Error::invalid_input(message);
    }
    Error::new(ErrorKind::Io, message)
}

/// The encodings the reader supports (`EncodingFunctionSet::Initialize`).
const ENCODINGS: [&str; 3] = ["utf-8", "latin-1", "utf-16"];

impl CsvFileHandle {
    /// `CSVFileHandle::OpenFile`.
    pub fn open(path: &str, options: &CsvReaderOptions) -> Result<Self> {
        let encoding = options.encoding.to_lowercase();
        if !ENCODINGS.contains(&encoding.as_str()) {
            let mut error = format!(
                "The CSV Reader does not support the encoding: \"{}\"\nIt is possible that the encoding exists in the encodings extension. You can try \"INSTALL encodings; LOAD encodings\"\nThe currently supported encodings are: \n",
                options.encoding
            );
            for name in ENCODINGS {
                error += "*  ";
                error += name;
                error += "\n";
            }
            return Err(Error::invalid_input(error));
        }
        rustdb_function::file_system::verify_can_access_file(path)?;
        let file = File::open(path).map_err(|error| open_error(path, &error))?;
        let metadata = file.metadata().map_err(|error| open_error(path, &error))?;
        if metadata.is_dir() {
            return Err(Error::new(
                ErrorKind::Io,
                format!("Could not read from file \"{path}\": Is a directory"),
            ));
        }
        let compression = options.compression.resolve(path);
        let file = LoggedFile {
            file,
            logger: FileSystemLogger::open(path),
            position: 0,
        };
        let raw: Box<dyn Read + Send> = Box::new(BufReader::with_capacity(1 << 16, file));
        let decompressed: Box<dyn Read + Send> = match compression {
            FileCompressionType::Gzip => Box::new(flate2::read::MultiGzDecoder::new(raw)),
            FileCompressionType::Zstd => Box::new(
                ruzstd::decoding::StreamingDecoder::new(raw)
                    .map_err(|error| Error::new(ErrorKind::Io, format!("{error}")))?,
            ),
            _ => raw,
        };
        let reader: Box<dyn Read + Send> = match encoding.as_str() {
            "utf-16" => Box::new(Transcoder {
                inner: decompressed,
                utf16: true,
                pending: Vec::new(),
                pending_pos: 0,
                carry: None,
            }),
            "latin-1" => Box::new(Transcoder {
                inner: decompressed,
                utf16: false,
                pending: Vec::new(),
                pending_pos: 0,
                carry: None,
            }),
            _ => decompressed,
        };
        Ok(Self {
            path: path.to_owned(),
            compression,
            encoding,
            reader,
            file_size: metadata.len(),
            finished: false,
            can_seek: compression == FileCompressionType::Uncompressed,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    pub fn can_seek(&self) -> bool {
        self.can_seek
    }

    pub fn compression(&self) -> FileCompressionType {
        self.compression
    }

    pub fn encoding(&self) -> &str {
        &self.encoding
    }

    pub fn finished_reading(&self) -> bool {
        self.finished
    }

    /// `Read`: up to `buffer.len()` bytes; 0 at the end of the file.
    pub fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        let read = loop {
            match self.reader.read(buffer) {
                Ok(read) => break read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(io_error(&error)),
            }
        };
        if !self.finished {
            self.finished = read == 0;
        }
        Ok(read)
    }

    /// Reopens the file from the start (`Reset`).
    pub fn reset(&mut self, options: &CsvReaderOptions) -> Result<()> {
        let mut reopened = Self::open(&self.path, options)?;
        reopened.compression = self.compression;
        *self = reopened;
        Ok(())
    }
}

/// One buffer of a CSV file (DuckDB's `CSVBuffer`).
#[derive(Debug)]
pub struct CsvBuffer {
    data: Vec<u8>,
    requested_size: u64,
    last_buffer: std::sync::atomic::AtomicBool,
    buffer_idx: u64,
}

impl CsvBuffer {
    /// Reads the next `buffer_size` bytes of the file.
    fn read(file_handle: &mut CsvFileHandle, buffer_size: u64, buffer_idx: u64) -> Result<Self> {
        let mut data = vec![0u8; buffer_size as usize];
        let mut actual = file_handle.read(&mut data)?;
        while actual < data.len() && !file_handle.finished_reading() {
            actual += file_handle.read(&mut data[actual..])?;
        }
        data.truncate(actual);
        data.shrink_to_fit();
        Ok(Self {
            data,
            requested_size: buffer_size,
            last_buffer: std::sync::atomic::AtomicBool::new(file_handle.finished_reading()),
            buffer_idx,
        })
    }

    fn is_last(&self) -> bool {
        self.last_buffer.load(std::sync::atomic::Ordering::Acquire)
    }

    /// `Pin`: a handle that remembers whether this was the last buffer when pinned.
    fn pin(self: &Arc<Self>) -> CsvBufferHandle {
        Arc::new(PinnedBuffer {
            is_last_buffer: self.is_last(),
            buffer: Arc::clone(self),
        })
    }
}

/// A pinned buffer (DuckDB's `CSVBufferHandle`).
#[derive(Debug)]
pub struct PinnedBuffer {
    buffer: Arc<CsvBuffer>,
    pub is_last_buffer: bool,
}

impl PinnedBuffer {
    /// The bytes read.
    pub fn data(&self) -> &[u8] {
        &self.buffer.data
    }

    /// `actual_size`.
    pub fn actual_size(&self) -> u64 {
        self.buffer.data.len() as u64
    }

    /// `requested_size`.
    pub fn requested_size(&self) -> u64 {
        self.buffer.requested_size
    }

    pub fn buffer_idx(&self) -> u64 {
        self.buffer.buffer_idx
    }
}

/// A pinned buffer shared by a scanner and its results.
pub type CsvBufferHandle = Arc<PinnedBuffer>;

#[derive(Debug)]
struct ManagerState {
    file_handle: CsvFileHandle,
    cached_buffers: Vec<Option<Arc<CsvBuffer>>>,
    /// Whether the last cached buffer is the last of the file.
    last_buffer_is_last: bool,
    done: bool,
    bytes_read: u64,
    reset_when_possible: HashSet<u64>,
}

/// The buffers of one file (DuckDB's `CSVBufferManager`).
#[derive(Debug)]
pub struct CsvBufferManager {
    state: Mutex<ManagerState>,
    path: String,
    options: CsvReaderOptions,
    buffer_size: u64,
    pub skip_rows: u64,
    pub sniffing: Mutex<bool>,
}

impl CsvBufferManager {
    /// Opens `path` and reads its first buffer.
    pub fn new(options: &CsvReaderOptions, path: &str) -> Result<Self> {
        let file_handle = CsvFileHandle::open(path, options)?;
        Self::with_handle(options, path, file_handle)
    }

    pub fn with_handle(options: &CsvReaderOptions, path: &str, file_handle: CsvFileHandle) -> Result<Self> {
        let manager = Self {
            state: Mutex::new(ManagerState {
                file_handle,
                cached_buffers: Vec::new(),
                last_buffer_is_last: false,
                done: false,
                bytes_read: 0,
                reset_when_possible: HashSet::new(),
            }),
            path: path.to_owned(),
            options: options.clone(),
            buffer_size: options.buffer_size_option.value(),
            skip_rows: options.dialect_options.skip_rows.value(),
            sniffing: Mutex::new(false),
        };
        {
            let mut state = manager.lock();
            manager.initialize(&mut state)?;
        }
        Ok(manager)
    }

    fn lock(&self) -> MutexGuard<'_, ManagerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `Initialize`: reads the first buffer.
    fn initialize(&self, state: &mut ManagerState) -> Result<()> {
        if state.cached_buffers.is_empty() {
            let buffer = CsvBuffer::read(&mut state.file_handle, self.buffer_size, 0)?;
            state.last_buffer_is_last = buffer.is_last();
            state.cached_buffers.push(Some(Arc::new(buffer)));
        }
        Ok(())
    }

    /// `ReadNextAndCacheIt`.
    fn read_next_and_cache_it(&self, state: &mut ManagerState) -> Result<bool> {
        if state.last_buffer_is_last {
            return Ok(false);
        }
        let next_idx = state.cached_buffers.len() as u64;
        let buffer = CsvBuffer::read(&mut state.file_handle, self.buffer_size, next_idx)?;
        if buffer.data.is_empty() {
            state.last_buffer_is_last = true;
            // The previous buffer becomes the last one.
            if let Some(Some(previous)) = state.cached_buffers.last() {
                previous
                    .last_buffer
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            return Ok(false);
        }
        state.last_buffer_is_last = buffer.is_last();
        state.bytes_read += buffer.data.len() as u64;
        state.cached_buffers.push(Some(Arc::new(buffer)));
        Ok(true)
    }

    /// `GetBuffer`: the buffer at `pos`, reading buffers up to it; `None` past the end.
    pub fn get_buffer(&self, pos: u64) -> Result<Option<CsvBufferHandle>> {
        let mut state = self.lock();
        if pos == 0 && state.done && state.cached_buffers.is_empty() {
            state.done = false;
            state.file_handle.reset(&self.options)?;
            state.last_buffer_is_last = false;
            self.initialize(&mut state)?;
        }
        while pos as usize >= state.cached_buffers.len() {
            if state.done {
                return Ok(None);
            }
            if !self.read_next_and_cache_it(&mut state)? {
                state.done = true;
            }
        }
        match &state.cached_buffers[pos as usize] {
            Some(buffer) => Ok(Some(buffer.pin())),
            None => Err(Error::internal("CSV buffer was released before it was scanned")),
        }
    }

    /// `ResetBuffer`: releases a buffer once every scanner is past it.
    pub fn reset_buffer(&self, buffer_idx: u64) {
        let mut state = self.lock();
        let index = buffer_idx as usize;
        if index >= state.cached_buffers.len() {
            return;
        }
        if buffer_idx == 0 && state.cached_buffers.len() > 1 {
            state.cached_buffers[0] = None;
            let mut current = buffer_idx + 1;
            while state.reset_when_possible.remove(&current) {
                state.cached_buffers[current as usize] = None;
                current += 1;
            }
            return;
        }
        if buffer_idx > 0 && state.cached_buffers[index - 1].is_none() {
            if state.cached_buffers[index]
                .as_ref()
                .is_some_and(|buffer| buffer.is_last())
            {
                state.cached_buffers.clear();
                state.reset_when_possible.clear();
                return;
            }
            state.cached_buffers[index] = None;
            let mut current = buffer_idx + 1;
            while state.reset_when_possible.remove(&current) {
                if let Some(slot) = state.cached_buffers.get_mut(current as usize) {
                    *slot = None;
                }
                current += 1;
            }
        } else {
            state.reset_when_possible.insert(buffer_idx);
        }
    }

    pub fn buffer_size(&self) -> u64 {
        self.buffer_size
    }

    /// `BufferCount`.
    pub fn buffer_count(&self) -> u64 {
        self.lock().cached_buffers.len() as u64
    }

    /// `Done`: every buffer was read at least once.
    pub fn done(&self) -> bool {
        self.lock().done
    }

    /// `ResetBufferManager`: restarts reading the file (after sniffing).
    pub fn reset_buffer_manager(&self) -> Result<()> {
        let mut state = self.lock();
        state.cached_buffers.clear();
        state.reset_when_possible.clear();
        state.file_handle.reset(&self.options)?;
        state.last_buffer_is_last = false;
        state.done = false;
        self.initialize(&mut state)
    }

    pub fn file_path(&self) -> &str {
        &self.path
    }

    pub fn bytes_read(&self) -> u64 {
        self.lock().bytes_read
    }

    pub fn file_size(&self) -> u64 {
        self.lock().file_handle.file_size()
    }

    pub fn compression(&self) -> FileCompressionType {
        self.lock().file_handle.compression()
    }

    pub fn set_sniffing(&self, sniffing: bool) {
        *self.sniffing.lock().unwrap_or_else(PoisonError::into_inner) = sniffing;
    }
}
