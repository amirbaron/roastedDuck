//! PostgreSQL binary COPY file framing. No PostgreSQL server is involved.
mod writer;
use crate::codec::Codec;
use roastedduck_common::{Error, ErrorKind, Result, STANDARD_VECTOR_SIZE};
use roastedduck_types::Value;
pub use writer::encode_row;

pub const DEFAULT_BUFFER_SIZE: usize = 32 * 1024 * 1024;
const HEADER_SIZE: usize = 19;
type ReadCallback = Box<dyn FnMut(&mut [u8]) -> Result<usize> + Send>;

pub struct Reader {
    read: ReadCallback,
    codecs: Vec<Codec>,
    buffer: Vec<u8>,
    file_size: u64,
    offset: u64,
    leftover: usize,
    leftover_offset: usize,
    position: usize,
    valid: usize,
    finished: bool,
    header_scanned: bool,
}
impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresBinaryReader")
            .field("file_size", &self.file_size)
            .field("offset", &self.offset)
            .field("buffer_size", &self.buffer.len())
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}
impl Reader {
    pub fn new(
        path: &[u8],
        file_size: u64,
        buffer_size: usize,
        codecs: Vec<Codec>,
        read: ReadCallback,
    ) -> Result<Self> {
        if file_size == 0 {
            return Err(Error::new_bytes(
                ErrorKind::Io,
                roastedduck_common::byte_format!("Postgres binary file '{path}' is empty"),
            ));
        }
        let mut buffer = Vec::new();
        buffer.try_reserve_exact(buffer_size).map_err(|_| {
            Error::new(
                ErrorKind::OutOfMemory,
                "Could not allocate Postgres binary read buffer",
            )
        })?;
        buffer.resize(buffer_size, 0);
        let mut result = Self {
            read,
            codecs,
            buffer,
            file_size,
            offset: 0,
            leftover: 0,
            leftover_offset: 0,
            position: 0,
            valid: 0,
            finished: false,
            header_scanned: false,
        };
        if !result.fill()? {
            return Err(Error::new_bytes(
                ErrorKind::Io,
                roastedduck_common::byte_format!("Failed to read postgres binary file '{path}'"),
            ));
        }
        if result.valid < HEADER_SIZE {
            return Err(io("Unable to read binary COPY data, invalid header"));
        }
        if result.buffer[..11] != crate::copy::BINARY_HEADER[..11] {
            return Err(io("Expected Postgres binary COPY header, got something else"));
        }
        // The pinned parser ignores flags and extension length.
        result.position = HEADER_SIZE;
        Ok(result)
    }

    fn fill(&mut self) -> Result<bool> {
        if self.offset >= self.file_size && self.leftover == 0 {
            self.finished = true;
            return Ok(false);
        }
        self.buffer
            .copy_within(self.leftover_offset..self.leftover_offset + self.leftover, 0);
        let amount = (self.buffer.len() - self.leftover)
            .min(usize::try_from(self.file_size - self.offset).unwrap_or(usize::MAX));
        let mut consumed = 0;
        while consumed < amount {
            let count = (self.read)(&mut self.buffer[self.leftover + consumed..self.leftover + amount])?;
            if count == 0 {
                return Err(io("Could not read requested bytes from Postgres binary file"));
            }
            if count > amount - consumed {
                return Err(io("Invalid byte count from Postgres binary file reader"));
            }
            consumed += count;
        }
        self.offset += consumed as u64;
        let total = self.leftover + consumed;
        if total == 0 {
            self.finished = true;
            return Ok(false);
        }
        let scan_offset = if self.header_scanned {
            0
        } else {
            HEADER_SIZE.min(total)
        };
        self.header_scanned = true;
        let complete = last_complete_row(&self.buffer[scan_offset..total]);
        let valid = if complete > 0 {
            complete + scan_offset
        } else if self.offset >= self.file_size {
            total
        } else if scan_offset > 0 {
            scan_offset
        } else {
            return Err(io(format!(
                "Postgres binary file contains a row larger than the read buffer ({} bytes). Increase buffer_size.",
                self.buffer.len()
            )));
        };
        self.position = 0;
        self.valid = valid;
        self.leftover = total - valid;
        self.leftover_offset = valid;
        Ok(true)
    }

    fn take(&mut self, size: usize, string: bool) -> Result<&[u8]> {
        if size > self.valid.saturating_sub(self.position) {
            return Err(io(if string {
                "Postgres binary reader - out of buffer in ReadString"
            } else {
                "Postgres binary reader - out of buffer in ReadInteger"
            }));
        }
        let begin = self.position;
        self.position += size;
        Ok(&self.buffer[begin..self.position])
    }

    pub fn next_batch(&mut self) -> Result<Vec<Vec<Value>>> {
        let mut rows = Vec::new();
        while rows.len() < STANDARD_VECTOR_SIZE {
            if self.position >= self.valid && (self.finished || !self.fill()?) {
                break;
            }
            let count = i16::from_be_bytes(
                self.take(2, false)?
                    .try_into()
                    .map_err(|_| Error::internal("Invalid binary integer width"))?,
            );
            if count <= 0 {
                self.position = self.valid;
                continue;
            }
            if count as usize != self.codecs.len() {
                return Err(io(format!(
                    "Postgres binary row has {count} columns, expected {}",
                    self.codecs.len()
                )));
            }
            let mut row = Vec::with_capacity(self.codecs.len());
            for index in 0..self.codecs.len() {
                let length = i32::from_be_bytes(
                    self.take(4, false)?
                        .try_into()
                        .map_err(|_| Error::internal("Invalid binary integer width"))?,
                );
                if length == -1 {
                    row.push(self.codecs[index].decode(None)?);
                    continue;
                }
                let length =
                    usize::try_from(length).map_err(|_| io("Invalid Postgres binary field length"))?;
                let string = matches!(
                    self.codecs[index].logical_type.id(),
                    roastedduck_types::LogicalTypeId::Varchar
                        | roastedduck_types::LogicalTypeId::Blob
                        | roastedduck_types::LogicalTypeId::Enum
                        | roastedduck_types::LogicalTypeId::Geometry
                );
                let start = self.position;
                self.take(length, string)?;
                row.push(self.codecs[index].decode(Some(&self.buffer[start..self.position]))?);
            }
            rows.push(row);
        }
        Ok(rows)
    }
}

fn io(message: impl Into<roastedduck_common::ByteString>) -> Error {
    Error::new_bytes(ErrorKind::Io, message.into())
}
fn last_complete_row(bytes: &[u8]) -> usize {
    let mut position = 0;
    let mut last = 0;
    while position + 2 <= bytes.len() {
        let count = i16::from_be_bytes([bytes[position], bytes[position + 1]]);
        position += 2;
        if count <= 0 {
            return position;
        }
        for _ in 0..count {
            let Some(raw) = bytes.get(position..position + 4) else {
                return last;
            };
            let length = i32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
            position += 4;
            if length > 0 {
                if length as usize > bytes.len() - position {
                    return last;
                }
                position += length as usize;
            }
        }
        last = position;
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Options;
    use roastedduck_types::LogicalType;
    use std::io::Read;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn reader(bytes: Vec<u8>, size: usize, ty: &LogicalType, reads: Arc<AtomicUsize>) -> Result<Reader> {
        let length = bytes.len() as u64;
        let mut source = std::io::Cursor::new(bytes);
        Reader::new(
            b"owned fixture",
            length,
            size,
            vec![Codec::for_file(ty, Options::default())?],
            Box::new(move |buffer| {
                reads.fetch_add(1, Ordering::Relaxed);
                let count = buffer.len().min(3);
                source.read(&mut buffer[..count]).map_err(|e| io(e.to_string()))
            }),
        )
    }
    #[test]
    fn fragmented_reads_preserve_rows_and_fuse_eof() -> Result<()> {
        let mut bytes = crate::copy::BINARY_HEADER.to_vec();
        for i in 0i32..17 {
            bytes.extend_from_slice(&1i16.to_be_bytes());
            bytes.extend_from_slice(&4i32.to_be_bytes());
            bytes.extend_from_slice(&i.to_be_bytes());
        }
        bytes.extend_from_slice(crate::copy::BINARY_FOOTER);
        let reads = Arc::new(AtomicUsize::new(0));
        let mut reader = reader(bytes, 19, &LogicalType::INTEGER, reads.clone())?;
        let rows = reader.next_batch()?;
        assert_eq!(rows, (0..17).map(|i| vec![Value::integer(i)]).collect::<Vec<_>>());
        let count = reads.load(Ordering::Relaxed);
        assert_eq!(reader.next_batch()?, Vec::<Vec<Value>>::new());
        assert_eq!(reader.next_batch()?, Vec::<Vec<Value>>::new());
        assert_eq!(reads.load(Ordering::Relaxed), count);
        Ok(())
    }
    #[test]
    fn row_limit_excludes_the_header_but_rejects_a_larger_row() -> Result<()> {
        for length in [50, 51] {
            let mut bytes = crate::copy::BINARY_HEADER.to_vec();
            bytes.extend_from_slice(&1i16.to_be_bytes());
            bytes.extend_from_slice(&(length as i32).to_be_bytes());
            bytes.extend(std::iter::repeat_n(b'x', length));
            bytes.extend_from_slice(crate::copy::BINARY_FOOTER);
            let mut reader = reader(bytes, 56, &LogicalType::VARCHAR, Arc::new(AtomicUsize::new(0)))?;
            let result = reader.next_batch();
            if length == 50 {
                assert_eq!(result?, vec![vec![Value::varchar("x".repeat(50))]]);
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("row larger than the read buffer")
                );
            }
        }
        Ok(())
    }
    #[test]
    fn malformed_field_and_column_counts_fail_without_reading_past_the_buffer() -> Result<()> {
        for columns in [1i16, 2] {
            let mut bytes = crate::copy::BINARY_HEADER.to_vec();
            bytes.extend_from_slice(&columns.to_be_bytes());
            bytes.extend_from_slice(&100i32.to_be_bytes());
            bytes.push(1);
            let mut reader = reader(bytes, 64, &LogicalType::INTEGER, Arc::new(AtomicUsize::new(0)))?;
            assert_eq!(reader.next_batch().unwrap_err().kind(), ErrorKind::Io);
        }
        Ok(())
    }
}
