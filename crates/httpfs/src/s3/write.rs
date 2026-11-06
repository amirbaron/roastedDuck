//! Accounted S3 upload parts and explicit successful writer finalization.
use std::collections::BTreeMap;
use std::io::Read as _;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use rustdb_buffer::{BufferHandle, BufferManager, MemoryTag};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::registered_files::FileHandle;

use super::{S3Request, encode, io_error, optional_tag, setting};
use crate::HttpFile;

struct PartBuffer {
    pin: BufferHandle,
    len: usize,
}

impl PartBuffer {
    fn append(&mut self, bytes: &[u8], capacity: usize) -> usize {
        let count = bytes.len().min(capacity - self.len);
        // SAFETY: this buffer owns its only pin, the writer mutex excludes all
        // other access, and both ranges contain count bytes without overlapping.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.pin.as_ptr().add(self.len), count);
        }
        self.len += count;
        count
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: exactly len bytes were initialized by append; the unique pin
        // keeps them loaded and no writes occur while this slice is borrowed.
        unsafe { std::slice::from_raw_parts(self.pin.as_ptr(), self.len) }
    }
}

struct WriteContext {
    transport: HttpFile,
    source: String,
    opener: Option<Arc<rustdb_function::FunctionClient>>,
}

struct Response {
    status: u16,
    etag: Option<String>,
    body: String,
    raw_body: Option<Vec<u8>>,
    metadata: crate::ResponseMetadata,
}

impl Response {
    fn error(&self, error: Error) -> Error {
        error.with_http_response(
            self.status,
            self.raw_body.as_deref().unwrap_or(self.body.as_bytes()),
            self.metadata
                .headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_slice())),
            self.metadata.reason.as_str(),
        )
    }
}

impl WriteContext {
    fn request(&self, method: &str, query: &str, body: &[u8]) -> Result<Response> {
        // Upload workers must retain the writer's connection settings and secret
        // resolver when an authentication response triggers credential refresh.
        let _opener = self
            .opener
            .clone()
            .map(rustdb_function::file_system::activate_opener);
        let mut request = self.transport.clone();
        if !query.is_empty() {
            request.url = format!("{}?{query}", self.transport.url);
        }
        let signing = request
            .signing
            .as_mut()
            .ok_or_else(|| Error::internal("S3 upload without signing parameters"))?;
        query.clone_into(&mut signing.query);
        signing.content_type = if matches!(method, "PUT" | "POST") {
            "application/octet-stream".into()
        } else {
            String::new()
        };
        for attempt in 0..=request.parameters.retries {
            let mut response = request.request_payload(method, None, body, true)?;
            let status = response.status().as_u16();
            let etag = response
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let metadata = crate::take_response_metadata(&mut response, request.parameters.httplib);
            let (text, raw_body) = if method == "PUT" && status == 200 {
                // A successful PUT uses status/ETag, not a textual response body.
                // Drain it for connection reuse without retaining a binary body.
                std::io::copy(&mut response.body_mut().as_reader(), &mut std::io::sink())
                    .map_err(io_error)?;
                (String::new(), None)
            } else {
                let mut bytes = Vec::new();
                response
                    .body_mut()
                    .as_reader()
                    .read_to_end(&mut bytes)
                    .map_err(io_error)?;
                match String::from_utf8(bytes) {
                    Ok(text) => (text, None),
                    Err(error) => {
                        let bytes = error.into_bytes();
                        (String::from_utf8_lossy(&bytes).into_owned(), Some(bytes))
                    }
                }
            };
            if status == 400
                && optional_tag(&text, "Error").and_then(|e| optional_tag(e, "Code"))
                    == Some("RequestTimeout")
                && attempt < request.parameters.retries
            {
                self.retry_wait(attempt);
                continue;
            }
            return Ok(Response {
                status,
                etag,
                body: text,
                raw_body,
                metadata,
            });
        }
        Err(Error::internal("S3 request retry loop ended unexpectedly"))
    }

    fn retry_wait(&self, attempt: u64) {
        if attempt != 0 {
            let delay = self.transport.parameters.wait_ms as f64
                * self.transport.parameters.backoff.powf((attempt - 1) as f64);
            if delay.is_finite() && delay > 0.0 {
                std::thread::sleep(Duration::from_millis(delay as u64));
            }
        }
    }

    fn initialize(&self) -> Result<String> {
        let response = self.request("POST", "uploads=", &[])?;
        if response.status != 200 {
            return Err(self.status_error(&response, false));
        }
        optional_tag(&response.body, "UploadId")
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Http,
                    "Unexpected response while initializing S3 multipart upload",
                )
            })
    }

    fn status_error(&self, response: &Response, details: bool) -> Error {
        let extra = if details {
            super::error_details(&response.body)
        } else {
            String::new()
        };
        let reason = response.metadata.reason.as_str();
        response.error(Error::new(
            ErrorKind::Http,
            format!(
                "Unable to connect to URL {}: {reason} (HTTP code {}){extra}",
                self.source, response.status
            ),
        ))
    }

    fn put(&self, query: &str, bytes: &[u8]) -> Result<String> {
        let response = self.request("PUT", query, bytes)?;
        if response.status != 200 {
            return Err(self.status_error(&response, true));
        }
        response
            .etag
            .ok_or_else(|| Error::new(ErrorKind::Io, "Unexpected response when uploading part to S3"))
    }

    fn complete(&self, upload_id: &str, etags: &BTreeMap<usize, String>) -> Result<()> {
        use std::fmt::Write as _;
        let mut body =
            String::from("<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
        for (part, etag) in etags {
            let _ = write!(
                body,
                "<Part><ETag>{etag}</ETag><PartNumber>{}</PartNumber></Part>",
                part + 1
            );
        }
        body.push_str("</CompleteMultipartUpload>");
        let query = format!("uploadId={}", encode(upload_id, true));
        for attempt in 0..=self.transport.parameters.retries {
            let response = self.request("POST", &query, body.as_bytes())?;
            if response.body.contains("<CompleteMultipartUploadResult") {
                return Ok(());
            }
            let code = optional_tag(&response.body, "Error").and_then(|e| optional_tag(e, "Code"));
            if response.status == 200
                && code == Some("InternalError")
                && attempt < self.transport.parameters.retries
            {
                self.retry_wait(attempt);
                continue;
            }
            let extra = super::error_details(&response.body);
            return Err(response.error(Error::new(
                ErrorKind::Http,
                format!(
                    "Unexpected response during S3 multipart upload finalization: {}{extra}\n\n{}",
                    response.status, response.body
                ),
            )));
        }
        Err(Error::internal("S3 completion retry loop ended unexpectedly"))
    }
}

#[derive(Default)]
struct UploadStatus {
    active: usize,
    generation: u64,
    etags: BTreeMap<usize, String>,
    error: Option<Error>,
}

#[derive(Default)]
struct Uploads {
    status: Mutex<UploadStatus>,
    changed: Condvar,
}

#[derive(Default)]
struct WriteState {
    buffer: Option<PartBuffer>,
    length: u64,
    upload_id: Option<String>,
    next_part: usize,
    closed: bool,
    error: Option<Error>,
}

pub(super) struct S3WriteFile {
    context: Arc<WriteContext>,
    manager: BufferManager,
    part_size: usize,
    max_parts: usize,
    max_threads: usize,
    uploads: Arc<Uploads>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    state: Mutex<WriteState>,
}

impl std::fmt::Debug for S3WriteFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3WriteFile").finish_non_exhaustive()
    }
}

impl S3WriteFile {
    pub(super) fn open(request: S3Request, manager: BufferManager) -> Result<Self> {
        let text =
            |name, default| setting(name).map_or_else(|| Ok(String::from(default)), |v| v.to_varchar());
        let max_size = rustdb_buffer::parse_memory_limit(&text("s3_uploader_max_filesize", "800GB")?)?;
        let max_parts: usize = text("s3_uploader_max_parts_per_file", "10000")?
            .parse()
            .map_err(io_error)?;
        let max_threads: usize = text("s3_uploader_thread_limit", "50")?
            .parse()
            .map_err(io_error)?;
        if max_parts == 0 || max_threads == 0 {
            return Err(Error::invalid_input(
                "S3 uploader part and thread limits must be greater than zero",
            ));
        }
        let part_size = usize::try_from((max_size / max_parts as u64).max(5_242_880)).map_err(io_error)?;
        let block_size =
            rustdb_buffer::DEFAULT_BLOCK_ALLOC_SIZE - rustdb_buffer::DEFAULT_BLOCK_HEADER_STORAGE_SIZE;
        let part_size = part_size
            .checked_add(block_size - 1)
            .ok_or_else(|| Error::invalid_input("S3 uploader part size is too large"))?
            / block_size
            * block_size;
        let source = format!("{}{}{}", request.bucket_url, request.key, request.query_suffix);
        let transport = HttpFile::new_request(request.url, request.parameters, Some(request.signing))?;
        Ok(Self {
            context: Arc::new(WriteContext {
                transport,
                source,
                opener: rustdb_function::file_system::opener(),
            }),
            manager,
            part_size,
            max_parts,
            max_threads,
            uploads: Arc::new(Uploads::default()),
            workers: Mutex::default(),
            state: Mutex::default(),
        })
    }

    fn allocate(&self) -> Result<PartBuffer> {
        loop {
            let generation = self
                .uploads
                .status
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .generation;
            match self.manager.allocate(MemoryTag::Extension, self.part_size, true) {
                Ok(pin) => return Ok(PartBuffer { pin, len: 0 }),
                Err(error) if error.kind() == ErrorKind::OutOfMemory => {
                    let mut status = self.uploads.status.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Some(error) = &status.error {
                        return Err(error.clone());
                    }
                    if status.generation != generation {
                        continue;
                    }
                    if status.active == 0 {
                        return Err(error);
                    }
                    while status.generation == generation && status.error.is_none() {
                        status = self
                            .uploads
                            .changed
                            .wait(status)
                            .unwrap_or_else(PoisonError::into_inner);
                    }
                    if let Some(error) = &status.error {
                        return Err(error.clone());
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn start_part(&self, state: &mut WriteState) -> Result<()> {
        if state.next_part >= self.max_parts {
            return Err(io_error("S3 uploader exceeded the maximum parts per file"));
        }
        let mut status = self.uploads.status.lock().unwrap_or_else(PoisonError::into_inner);
        while status.active >= self.max_threads && status.error.is_none() {
            status = self
                .uploads
                .changed
                .wait(status)
                .unwrap_or_else(PoisonError::into_inner);
        }
        if let Some(error) = &status.error {
            return Err(error.clone());
        }
        drop(status);
        if state.upload_id.is_none() {
            state.upload_id = Some(self.context.initialize()?);
        }
        let upload_id = state
            .upload_id
            .as_ref()
            .ok_or_else(|| Error::internal("Missing S3 upload id"))?;
        let query = format!(
            "partNumber={}&uploadId={}",
            state.next_part + 1,
            encode(upload_id, true)
        );
        let buffer = state
            .buffer
            .take()
            .ok_or_else(|| Error::internal("Missing S3 upload part"))?;
        let part = state.next_part;
        state.next_part += 1;
        let context = Arc::clone(&self.context);
        let uploads = Arc::clone(&self.uploads);
        self.uploads
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .active += 1;
        let spawned = std::thread::Builder::new()
            .name("rustdb-s3-upload".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    context.put(&query, buffer.bytes())
                }))
                .unwrap_or_else(|_| Err(Error::internal("S3 upload worker panicked")));
                drop(buffer); // Release the accounted allocation before waking a writer.
                let mut status = uploads.status.lock().unwrap_or_else(PoisonError::into_inner);
                match result {
                    Ok(etag) => {
                        status.etags.insert(part, etag);
                    }
                    Err(error) => {
                        if status.error.is_none() {
                            status.error = Some(error);
                        }
                    }
                }
                status.active -= 1;
                status.generation = status.generation.wrapping_add(1);
                uploads.changed.notify_all();
            });
        if let Ok(worker) = spawned {
            self.workers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(worker);
        } else if let Err(error) = spawned {
            let mut status = self.uploads.status.lock().unwrap_or_else(PoisonError::into_inner);
            status.active -= 1;
            status.error = Some(io_error(error));
            self.uploads.changed.notify_all();
            return Err(status
                .error
                .clone()
                .ok_or_else(|| Error::internal("Missing upload worker error"))?);
        }
        Ok(())
    }

    fn wait(&self) -> Result<()> {
        let mut status = self.uploads.status.lock().unwrap_or_else(PoisonError::into_inner);
        while status.active != 0 {
            status = self
                .uploads
                .changed
                .wait(status)
                .unwrap_or_else(PoisonError::into_inner);
        }
        let error = status.error.clone();
        drop(status);
        for worker in self
            .workers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
        {
            worker
                .join()
                .map_err(|_| Error::internal("S3 upload worker panicked"))?;
        }
        error.map_or(Ok(()), Err)
    }

    fn finish(&self, state: &mut WriteState) -> Result<()> {
        if state.closed {
            return Ok(());
        }
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        self.wait()?;
        let no_kms = self
            .context
            .transport
            .signing
            .as_ref()
            .is_none_or(|s| s.kms_key_id.is_empty());
        if state.upload_id.is_none()
            && no_kms
            && let Some(buffer) = &state.buffer
        {
            self.context.put("", buffer.bytes())?;
            state.buffer = None;
            state.closed = true;
            return Ok(());
        }
        if state.upload_id.is_none() {
            state.upload_id = Some(self.context.initialize()?);
        }
        if state.buffer.as_ref().is_some_and(|b| b.len != 0) {
            self.start_part(state)?;
        }
        self.wait()?;
        let status = self.uploads.status.lock().unwrap_or_else(PoisonError::into_inner);
        if !status.etags.is_empty() {
            self.context.complete(
                state
                    .upload_id
                    .as_deref()
                    .ok_or_else(|| Error::internal("Missing S3 upload id"))?,
                &status.etags,
            )?;
        }
        state.closed = true;
        Ok(())
    }
}

impl FileHandle for S3WriteFile {
    fn read_at(&self, _: u64, _: &mut [u8]) -> Result<usize> {
        Err(Error::not_implemented("Cannot read an S3 write handle"))
    }
    fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<usize> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let result = (|| {
            if let Some(error) = &state.error {
                return Err(error.clone());
            }
            if state.closed {
                return Err(io_error("Cannot write after S3 upload finalization"));
            }
            if offset != state.length {
                return Err(Error::internal("Non-sequential write not supported!"));
            }
            let mut remaining = bytes;
            while !remaining.is_empty() {
                if state.buffer.is_none() {
                    state.buffer = Some(self.allocate()?);
                }
                let count = state
                    .buffer
                    .as_mut()
                    .ok_or_else(|| Error::internal("Missing S3 buffer"))?
                    .append(remaining, self.part_size);
                state.length += count as u64;
                remaining = &remaining[count..];
                if state.buffer.as_ref().is_some_and(|b| b.len == self.part_size) {
                    self.start_part(&mut state)?;
                }
            }
            Ok(bytes.len())
        })();
        if let Err(error) = &result {
            state.error = Some(error.clone());
        }
        result
    }
    fn size(&self) -> Result<u64> {
        Ok(self.state.lock().unwrap_or_else(PoisonError::into_inner).length)
    }
    fn modified(&self) -> Result<i64> {
        Ok(0)
    }
    fn flush(&self) -> Result<()> {
        self.uploads
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .error
            .clone()
            .map_or(Ok(()), Err)
    }
    fn sync(&self) -> Result<()> {
        self.close()
    }
    fn close(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let result = self.finish(&mut state);
        if let Err(error) = &result {
            state.error = Some(error.clone());
        }
        result
    }
}

impl Drop for S3WriteFile {
    fn drop(&mut self) {
        let _ = self.wait();
    }
}
