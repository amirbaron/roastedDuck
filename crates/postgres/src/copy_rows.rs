//! Incremental COPY OUT framing, retaining only the current protocol payload/row.
use crate::CopyReader;
use roastedduck_common::{Error, ErrorKind, Result};
use std::sync::atomic::AtomicBool;

#[derive(Debug)]
pub struct CopyRows {
    source: CopyReader,
    bytes: Vec<u8>,
    offset: usize,
    header: bool,
    finished: bool,
}
impl CopyRows {
    pub fn new(source: CopyReader) -> Self {
        Self {
            source,
            bytes: Vec::new(),
            offset: 0,
            header: false,
            finished: false,
        }
    }
    fn ensure(&mut self, length: usize, interrupted: Option<&AtomicBool>) -> Result<bool> {
        while self.bytes.len() - self.offset < length {
            if self.offset > 0 {
                self.bytes.drain(..self.offset);
                self.offset = 0;
            }
            let Some(next) = self.source.next_chunk_interruptible(interrupted)? else {
                return Ok(false);
            };
            self.bytes.extend_from_slice(&next);
        }
        Ok(true)
    }
    fn read(&mut self, length: usize, interrupted: Option<&AtomicBool>) -> Result<&[u8]> {
        if !self.ensure(length, interrupted)? {
            return Err(Error::new(
                ErrorKind::Io,
                "Unable to read binary COPY data from Postgres: truncated value",
            ));
        }
        let start = self.offset;
        self.offset += length;
        Ok(&self.bytes[start..self.offset])
    }
    pub fn next_batch(
        &mut self,
        max: usize,
        interrupted: Option<&AtomicBool>,
    ) -> Result<Vec<Vec<Option<Vec<u8>>>>> {
        if self.finished {
            return Ok(Vec::new());
        }
        if !self.header {
            let header = self.read(19, interrupted)?;
            if header[..11] != crate::copy::BINARY_HEADER[..11] {
                return Err(Error::new(
                    ErrorKind::Io,
                    "Expected Postgres binary COPY header, got something else",
                ));
            }
            self.header = true;
        }
        let mut rows = Vec::new();
        while rows.len() < max {
            if !self.ensure(2, interrupted)? {
                return Err(Error::new(
                    ErrorKind::Io,
                    "Postgres binary COPY ended without a trailer",
                ));
            }
            let count = self.read(2, interrupted)?;
            let count = i16::from_be_bytes([count[0], count[1]]);
            if count == -1 {
                while self.source.next_chunk_interruptible(interrupted)?.is_some() {}
                self.finished = true;
                break;
            }
            if count < 0 {
                return Err(Error::new(ErrorKind::Io, "Invalid PostgreSQL COPY field count"));
            }
            let mut row = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let raw = self.read(4, interrupted)?;
                let length = i32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
                if length == -1 {
                    row.push(None);
                } else {
                    let length = usize::try_from(length)
                        .map_err(|_| Error::new(ErrorKind::Io, "Invalid PostgreSQL COPY field length"))?;
                    row.push(Some(self.read(length, interrupted)?.to_vec()));
                }
            }
            rows.push(row);
        }
        Ok(rows)
    }
}
