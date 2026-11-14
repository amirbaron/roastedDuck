use std::fmt::Write as _;
use std::io::Read;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::UNIX_EPOCH;

use roastedduck_common::{ByteString, Error, Result};
use roastedduck_function::logging::FileSystemLogger;
use roastedduck_function::registered_files::{FileHandle, FileSystem};
use roastedduck_types::blob::write_base64;

use super::client::Client;
use super::url::encode;
use super::{io_error, setting};

#[derive(Debug, Default)]
struct WriteState {
    buffer: Vec<u8>,
    position: u64,
    blocks: u32,
    committed: u32,
}

#[derive(Debug)]
struct BlobFile {
    client: Client,
    size: u64,
    modified: i64,
    etag: String,
    write: Option<Mutex<WriteState>>,
    block_size: usize,
    commit_interval: u64,
    logger: Option<FileSystemLogger>,
}

fn block_id(index: u32) -> String {
    let mut result = Vec::new();
    write_base64(&index.to_be_bytes(), &mut result);
    String::from_utf8_lossy(&result).into_owned()
}

impl BlobFile {
    fn stage(&self, state: &mut WriteState) -> Result<()> {
        if state.buffer.is_empty() {
            return Ok(());
        }
        let query = format!("comp=block&blockid={}", encode(&block_id(state.blocks), false));
        let response = self.client.request(
            "PUT",
            Some(&self.client.parsed.path),
            &query,
            Vec::new(),
            &state.buffer,
        )?;
        self.client.check(&response, "Write to")?;
        state.blocks += 1;
        state.buffer.clear();
        Ok(())
    }

    fn commit(&self, state: &mut WriteState, force: bool) -> Result<()> {
        if !force && state.committed == state.blocks {
            return Ok(());
        }
        let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?><BlockList>");
        for index in 0..state.blocks {
            let _ = write!(xml, "<Latest>{}</Latest>", block_id(index));
        }
        xml.push_str("</BlockList>");
        let response = self.client.request(
            "PUT",
            Some(&self.client.parsed.path),
            "comp=blocklist",
            Vec::new(),
            xml.as_bytes(),
        )?;
        self.client.check(&response, "FileSync of")?;
        state.committed = state.blocks;
        Ok(())
    }
}

impl FileHandle for BlobFile {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize> {
        if self.write.is_some() {
            return Err(io_error("Azure blob is opened for writing"));
        }
        let count = buffer.len().min(self.size.saturating_sub(offset) as usize);
        if let Some(logger) = &self.logger {
            logger.read(count as u64, offset);
        }
        if count == 0 {
            return Ok(0);
        }
        let mut response = self.client.request(
            "GET",
            Some(&self.client.parsed.path),
            "",
            vec![(
                "Range".into(),
                format!("bytes={offset}-{}", offset + count as u64 - 1),
            )],
            &[],
        )?;
        self.client.check(&response, "Read to")?;
        response
            .body_mut()
            .as_reader()
            .read_exact(&mut buffer[..count])
            .map_err(io_error)?;
        Ok(count)
    }
    fn write_at(&self, offset: u64, mut buffer: &[u8]) -> Result<usize> {
        let write = self
            .write
            .as_ref()
            .ok_or_else(|| io_error("Write called on file opened in read mode"))?;
        let mut state = write.lock().unwrap_or_else(PoisonError::into_inner);
        if offset != state.position {
            return Err(io_error("Azure blob writes must be sequential"));
        }
        let original_len = buffer.len();
        while !buffer.is_empty() {
            let count = buffer.len().min(self.block_size - state.buffer.len());
            state.buffer.extend_from_slice(&buffer[..count]);
            state.position += count as u64;
            buffer = &buffer[count..];
            if state.buffer.len() == self.block_size {
                self.stage(&mut state)?;
                if self.commit_interval > 0
                    && u64::from(state.blocks - state.committed) >= self.commit_interval
                {
                    self.commit(&mut state, false)?;
                }
            }
        }
        if let Some(logger) = &self.logger {
            logger.write(original_len as u64, offset);
        }
        Ok(original_len)
    }
    fn size(&self) -> Result<u64> {
        Ok(self.write.as_ref().map_or(self.size, |s| {
            s.lock().unwrap_or_else(PoisonError::into_inner).position
        }))
    }
    fn modified(&self) -> Result<i64> {
        Ok(self.modified)
    }
    fn version_tag(&self) -> Result<String> {
        Ok(self.etag.clone())
    }
    fn flush(&self) -> Result<()> {
        if let Some(write) = &self.write {
            let mut state = write.lock().unwrap_or_else(PoisonError::into_inner);
            self.stage(&mut state)?;
            self.commit(&mut state, false)?;
        }
        Ok(())
    }
    fn close(&self) -> Result<()> {
        self.flush()?;
        if let Some(logger) = &self.logger {
            logger.close();
        }
        Ok(())
    }
}

/// Blob transport; file bodies are streamed into caller buffers or bounded blocks.
#[derive(Debug)]
pub struct AzureBlobFileSystem;

fn path_text(path: &[u8]) -> Result<&str> {
    std::str::from_utf8(path).map_err(io_error)
}

impl FileSystem for AzureBlobFileSystem {
    fn name(&self) -> &'static str {
        "AzureBlobStorageFileSystem"
    }
    fn is_remote(&self) -> bool {
        true
    }
    fn handles(&self, path: &[u8]) -> bool {
        path.starts_with(b"az://") || path.starts_with(b"azure://")
    }
    fn open(&self, path: &[u8], write: bool) -> Result<Arc<dyn FileHandle>> {
        let client = Client::new(path_text(path)?)?;
        let configured: u64 = setting("azure_write_block_size", "8388608")
            .parse()
            .map_err(io_error)?;
        if configured > 4000 * 1024 * 1024 {
            return Err(Error::invalid_input(
                "azure_write_block_size must be at most 4000 MiB",
            ));
        }
        let block_size = if configured == 0 {
            8 * 1024 * 1024
        } else {
            configured as usize
        };
        let response = client.request("HEAD", Some(&client.parsed.path), "", Vec::new(), &[])?;
        if !write || response.status().as_u16() != 404 {
            client.check(&response, "open file")?;
        }
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
        };
        let size = if write {
            0
        } else {
            header("content-length").parse().map_err(io_error)?
        };
        let modified = httpdate::parse_http_date(header("last-modified"))
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_micros() as i64);
        let file = BlobFile {
            client,
            size,
            modified,
            etag: header("etag").into(),
            write: write.then(|| Mutex::new(WriteState::default())),
            block_size,
            commit_interval: setting("azure_write_staged_blocks_per_commit", "0")
                .parse()
                .map_err(io_error)?,
            logger: FileSystemLogger::open(path),
        };
        if write {
            file.commit(&mut WriteState::default(), true)?;
        }
        Ok(Arc::new(file))
    }
    fn glob(&self, path: &[u8]) -> Result<Vec<ByteString>> {
        let client = Client::new(path_text(path)?)?;
        let Some(wildcard) = client.parsed.path.find(['*', '[', '\\']) else {
            return if self.is_file(path)? {
                Ok(vec![path.into()])
            } else {
                Ok(Vec::new())
            };
        };
        let prefix = &client.parsed.path[..wildcard];
        let pattern: Vec<_> = client.parsed.path.split('/').collect();
        Ok(list(&client, prefix)?
            .into_iter()
            .filter(|key| crate::s3::path_matches(&key.split('/').collect::<Vec<_>>(), &pattern, true))
            .map(|key| format!("{}{key}", client.parsed.result_prefix()).into())
            .collect())
    }
    fn is_file(&self, path: &[u8]) -> Result<bool> {
        let client = Client::new(path_text(path)?)?;
        let response = client.request("HEAD", Some(&client.parsed.path), "", Vec::new(), &[])?;
        if response.status().as_u16() == 404 {
            return Ok(false);
        }
        client.check(&response, "open file")?;
        let _ = FileSystemLogger::open(path);
        Ok(true)
    }
    fn is_dir(&self, path: &[u8]) -> Result<bool> {
        let path = path_text(path)?;
        let path = format!("{}/", path.trim_end_matches('/'));
        let client = Client::new(&path)?;
        Ok(!list(&client, &client.parsed.path)?.is_empty())
    }
    fn mkdir(&self, _path: &[u8]) -> Result<()> {
        Ok(())
    }
    fn remove(&self, path: &[u8]) -> Result<()> {
        let client = Client::new(path_text(path)?)?;
        let response = client.request("DELETE", Some(&client.parsed.path), "", Vec::new(), &[])?;
        client.check(&response, "Delete of")
    }
    fn rename(&self, _source: &[u8], _target: &[u8]) -> Result<()> {
        Err(Error::not_implemented(
            "AzureBlobStorageFileSystem does not support moving files",
        ))
    }
}

fn list(client: &Client, prefix: &str) -> Result<Vec<String>> {
    let mut marker = String::new();
    let mut result = Vec::new();
    loop {
        let query = format!(
            "restype=container&comp=list&prefix={}&marker={}",
            encode(prefix, false),
            encode(&marker, false)
        );
        let mut response = client.request("GET", None, &query, Vec::new(), &[])?;
        client.check(&response, "Read to")?;
        let mut xml = String::new();
        response
            .body_mut()
            .as_reader()
            .read_to_string(&mut xml)
            .map_err(io_error)?;
        let mut rest = xml.as_str();
        while let Some(start) = rest.find("<Name>") {
            rest = &rest[start + 6..];
            let end = rest
                .find("</Name>")
                .ok_or_else(|| io_error("Invalid Azure listing XML"))?;
            result.push(unescape(&rest[..end])?);
            rest = &rest[end + 7..];
        }
        let next = crate::s3::optional_tag(&xml, "NextMarker").unwrap_or_default();
        if next.is_empty() {
            break;
        }
        let next = unescape(next)?;
        if next == marker {
            return Err(io_error("Azure listing repeated its continuation marker"));
        }
        marker = next;
    }
    Ok(result)
}

fn unescape(text: &str) -> Result<String> {
    let mut result = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        result.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        let end = rest.find(';').ok_or_else(|| io_error("Invalid XML entity"))?;
        let entity = &rest[..end];
        let character = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let value = if let Some(hex) = entity.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16).ok()
                } else {
                    entity.strip_prefix('#').and_then(|s| s.parse().ok())
                };
                value
                    .and_then(char::from_u32)
                    .ok_or_else(|| io_error("Invalid XML entity"))?
            }
        };
        result.push(character);
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}
