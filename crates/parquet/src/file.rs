//! Reading a Parquet file: positional reads and the footer (DuckDB's `LoadMetadata` and
//! `ParseParquetFooter`).

use std::fs::File;

use rustdb_common::{Error, ErrorKind, Result};

use crate::thrift::{FileMetaData, ThriftValue};

/// An open Parquet file. Reads are positional, so one handle serves every thread.
#[derive(Debug)]
pub struct ParquetFile {
    path: String,
    file: File,
    size: u64,
    /// The last modification time, if known.
    modified: Option<std::time::SystemTime>,
}

fn io_error(message: String) -> Error {
    Error::new(ErrorKind::Io, message)
}

impl ParquetFile {
    /// Opens `path` for reading (`FileSystem::OpenFile`).
    pub fn open(path: &str) -> Result<Self> {
        let file = File::open(path).map_err(|error| {
            let reason = match error.kind() {
                std::io::ErrorKind::NotFound => "No such file or directory".to_owned(),
                std::io::ErrorKind::PermissionDenied => "Permission denied".to_owned(),
                _ => error.to_string(),
            };
            io_error(format!("Cannot open file \"{path}\": {reason}"))
        })?;
        let metadata = file
            .metadata()
            .map_err(|error| io_error(format!("Could not get file size of \"{path}\": {error}")))?;
        if metadata.is_dir() {
            return Err(io_error(format!("Cannot open file \"{path}\": Is a directory")));
        }
        Ok(Self {
            path: path.to_owned(),
            file,
            size: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn modified(&self) -> Option<std::time::SystemTime> {
        self.modified
    }

    /// Fills `buffer` from offset `offset`.
    pub fn read_exact_at(&self, buffer: &mut [u8], offset: u64) -> Result<()> {
        let end = offset.checked_add(buffer.len() as u64);
        if end.is_none_or(|end| end > self.size) {
            return Err(io_error(format!(
                "Could not read enough bytes from file \"{}\": attempted to read {} bytes from location {offset}",
                self.path,
                buffer.len()
            )));
        }
        read_exact_at(&self.file, buffer, offset)
            .map_err(|error| io_error(format!("Could not read from file \"{}\": {error}", self.path)))
    }

    /// The bytes `offset..offset + len`.
    pub fn read_range(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let mut buffer = vec![0; len];
        self.read_exact_at(&mut buffer, offset)?;
        Ok(buffer)
    }
}

#[cfg(unix)]
fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buffer, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    while !buffer.is_empty() {
        let read = std::os::windows::fs::FileExt::seek_read(file, buffer, offset)?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buffer = &mut buffer[read..];
        offset += read as u64;
    }
    Ok(())
}

/// The parsed footer of a file (DuckDB's `ParquetFileMetadataCache`).
#[derive(Debug)]
pub struct FileMetadata {
    pub metadata: FileMetaData,
    /// The length of the Thrift-encoded footer.
    pub footer_len: u32,
}

/// `LoadMetadata`: checks the magic bytes and parses the footer.
pub fn load_metadata(file: &ParquetFile) -> Result<FileMetadata> {
    let path = file.path();
    let size = file.size();
    if size < 12 {
        return Err(Error::invalid_input(format!(
            "File '{path}' too small to be a Parquet file"
        )));
    }
    let mut tail = [0u8; 8];
    file.read_exact_at(&mut tail, size - 8)?;
    match &tail[4..] {
        b"PAR1" => {}
        b"PARE" => {
            return Err(Error::invalid_input(format!(
                "File '{path}' is encrypted, but 'encryption_config' was not set"
            )));
        }
        _ => {
            return Err(Error::invalid_input(format!(
                "No magic bytes found at end of file '{path}'"
            )));
        }
    }
    let footer_len = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
    if footer_len == 0 || size < 12 + u64::from(footer_len) {
        return Err(Error::invalid_input(format!(
            "Footer length error in file '{path}'"
        )));
    }
    let footer = file.read_range(size - 8 - u64::from(footer_len), footer_len as usize)?;
    let mut reader = crate::thrift::CompactReader::new(&footer);
    let metadata = FileMetaData::read(&mut reader)?;
    if metadata.num_rows < 0 {
        return Err(Error::invalid_input(format!(
            "Failed to read Parquet file \"{path}\": has negative number of rows"
        )));
    }
    Ok(FileMetadata { metadata, footer_len })
}
