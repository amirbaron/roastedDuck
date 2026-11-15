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

#[derive(Debug, Default)]
struct ReadState {
    start: u64,
    bytes: Vec<u8>,
}

#[derive(Debug)]
struct BlobFile {
    client: Client,
    size: u64,
    modified: i64,
    write: Option<Mutex<WriteState>>,
    block_size: usize,
    commit_interval: u64,
    logger: Option<FileSystemLogger>,
    read: Mutex<ReadState>,
    read_buffer_size: usize,
    transfer_chunk_size: usize,
    transfer_concurrency: i32,
}

fn block_id(index: u32) -> String {
    let mut result = Vec::new();
    write_base64(&index.to_be_bytes(), &mut result);
    String::from_utf8_lossy(&result).into_owned()
}

impl BlobFile {
    fn download_chunk(&self, offset: u64, buffer: &mut [u8], etag: Option<&str>) -> Result<String> {
        let mut headers = vec![(
            "Range".into(),
            format!("bytes={offset}-{}", offset + buffer.len() as u64 - 1),
        )];
        if let Some(etag) = etag {
            headers.push(("If-Match".into(), etag.into()));
        }
        let mut response = self
            .client
            .request("GET", Some(&self.client.parsed.path), "", headers, &[])?;
        self.client.check(&response, "Read to")?;
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        response
            .body_mut()
            .as_reader()
            .read_exact(buffer)
            .map_err(io_error)?;
        Ok(etag)
    }

    fn download_range(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        if buffer.is_empty() {
            return Ok(());
        }
        if self.transfer_chunk_size == 0 {
            return Err(io_error(
                "azure_read_transfer_chunk_size must be greater than zero",
            ));
        }
        let first = buffer.len().min(self.transfer_chunk_size);
        // DownloadTo discovers the ETag in the initial request and pins every
        // subsequent chunk to it, so an overwrite cannot produce mixed bytes.
        let etag = self.download_chunk(offset, &mut buffer[..first], None)?;
        let remaining = &mut buffer[first..];
        if remaining.is_empty() {
            return Ok(());
        }
        // ConcurrentTransfer always runs one worker on the caller, including
        // when concurrency is zero or negative in the pinned SDK.
        let concurrency = usize::try_from(self.transfer_concurrency.max(1)).map_err(io_error)?;
        let chunks = remaining.len().div_ceil(self.transfer_chunk_size);
        let per_worker = chunks
            .div_ceil(concurrency)
            .saturating_mul(self.transfer_chunk_size);
        let opener = roastedduck_function::file_system::opener();
        let failed = std::sync::atomic::AtomicBool::new(false);
        let download = |start: u64, part: &mut [u8]| -> Result<()> {
            for (index, chunk) in part.chunks_mut(self.transfer_chunk_size).enumerate() {
                if failed.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                if let Err(error) = self.download_chunk(
                    start + (index * self.transfer_chunk_size) as u64,
                    chunk,
                    Some(&etag),
                ) {
                    failed.store(true, std::sync::atomic::Ordering::Relaxed);
                    return Err(error);
                }
            }
            Ok(())
        };
        std::thread::scope(|scope| -> Result<()> {
            let mut parts = remaining.chunks_mut(per_worker).enumerate();
            let Some((_, caller_part)) = parts.next() else {
                return Ok(());
            };
            let mut workers = Vec::new();
            for (index, part) in parts {
                let start = offset + first as u64 + (index * per_worker) as u64;
                let opener = opener.clone();
                let download = &download;
                workers.push(
                    std::thread::Builder::new()
                        .spawn_scoped(scope, move || -> Result<()> {
                            let _guard = opener.map(roastedduck_function::file_system::activate_opener);
                            download(start, part)
                        })
                        .map_err(io_error)?,
                );
            }
            download(offset + first as u64, caller_part)?;
            for worker in workers {
                worker
                    .join()
                    .map_err(|_| Error::internal("Azure read worker panicked"))??;
            }
            Ok(())
        })
    }

    fn stage(&self, state: &mut WriteState) -> Result<()> {
        if state.buffer.is_empty() {
            return Ok(());
        }
        let dfs = self.client.parsed.dfs();
        let query = if dfs {
            format!(
                "action=append&position={}",
                state.position - state.buffer.len() as u64
            )
        } else {
            format!("comp=block&blockid={}", encode(&block_id(state.blocks), false))
        };
        let response = self.client.request(
            if dfs { "PATCH" } else { "PUT" },
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
        if self.client.parsed.dfs() {
            let query = format!("action=flush&position={}&close={force}", state.position);
            let response =
                self.client
                    .request("PATCH", Some(&self.client.parsed.path), &query, Vec::new(), &[])?;
            self.client.check(&response, "FileSync of")?;
            state.committed = state.blocks;
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
        if count == 0 {
            if let Some(logger) = &self.logger {
                logger.read(0, offset);
            }
            return Ok(0);
        }
        let mut cached = self.read.lock().unwrap_or_else(PoisonError::into_inner);
        let mut copied = 0;
        while copied < count {
            let position = offset + copied as u64;
            let relative = position
                .checked_sub(cached.start)
                .and_then(|v| usize::try_from(v).ok());
            if let Some(relative) = relative.filter(|r| *r < cached.bytes.len()) {
                let length = (count - copied).min(cached.bytes.len() - relative);
                buffer[copied..copied + length].copy_from_slice(&cached.bytes[relative..relative + length]);
                copied += length;
                continue;
            }
            let length = self
                .read_buffer_size
                .min(self.size.saturating_sub(position) as usize);
            if count - copied > length {
                self.download_range(position, &mut buffer[copied..count])?;
                cached.bytes.clear();
                break;
            }
            cached.bytes.resize(length, 0);
            if let Err(error) = self.download_range(position, &mut cached.bytes) {
                cached.bytes.clear();
                return Err(error);
            }
            cached.start = position;
        }
        if let Some(logger) = &self.logger {
            logger.read(count as u64, offset);
        }
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
    // The pinned filesystem does not expose a version tag to the engine cache.
    // Its cache validation uses Last-Modified's ten-second freshness window;
    // ETags still protect the chunks within each DownloadTo operation above.
    fn flush(&self) -> Result<()> {
        if let Some(write) = &self.write {
            let mut state = write.lock().unwrap_or_else(PoisonError::into_inner);
            self.stage(&mut state)?;
            self.commit(&mut state, false)?;
        }
        Ok(())
    }
    fn close(&self) -> Result<()> {
        if self.client.parsed.dfs()
            && let Some(write) = &self.write
        {
            let mut state = write.lock().unwrap_or_else(PoisonError::into_inner);
            self.stage(&mut state)?;
            self.commit(&mut state, true)?;
        } else {
            self.flush()?;
        }
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

fn open_file(path: &[u8], write: bool, direct_io: bool) -> Result<Arc<dyn FileHandle>> {
    let mut client = Client::new(path_text(path)?)?;
    if client.parsed.dfs() && client.parsed.path.len() > 1 && client.parsed.path.ends_with('/') {
        client.parsed.path.pop();
    }
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
        write: write.then(|| Mutex::new(WriteState::default())),
        block_size,
        commit_interval: setting("azure_write_staged_blocks_per_commit", "0")
            .parse()
            .map_err(io_error)?,
        logger: FileSystemLogger::open(path),
        read: Mutex::default(),
        read_buffer_size: if direct_io {
            0
        } else {
            setting("azure_read_buffer_size", "8388608")
                .parse()
                .map_err(io_error)?
        },
        transfer_chunk_size: setting("azure_read_transfer_chunk_size", "8388608")
            .parse()
            .map_err(io_error)?,
        transfer_concurrency: setting("azure_read_transfer_concurrency", "5")
            .parse()
            .map_err(io_error)?,
    };
    if write {
        if file.client.parsed.dfs() {
            let response = file.client.request(
                "PUT",
                Some(&file.client.parsed.path),
                "resource=file",
                Vec::new(),
                &[],
            )?;
            file.client.check(&response, "Create")?;
        } else {
            file.commit(&mut WriteState::default(), true)?;
        }
    }
    Ok(Arc::new(file))
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
        open_file(path, write, false)
    }
    fn open_read_direct(&self, path: &[u8]) -> Result<Arc<dyn FileHandle>> {
        open_file(path, false, true)
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
