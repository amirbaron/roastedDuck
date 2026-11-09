//! The built-in, plain HTTP extension transport (C++ `HTTPUtil` / httplib).
//! Bodies stream into the installer; no extension or query data is cached.
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use roastedduck_common::{ByteString, Error, ErrorKind, Result, byte_format};

const READ_ERROR: &str = "Failed to read connection";

pub(crate) fn setting(db: &crate::Database, name: &str) -> ByteString {
    roastedduck_function::file_system::opener()
        .and_then(|client| client.settings())
        .and_then(|settings| settings.current_setting(name).ok())
        .or_else(|| db.global_setting(name))
        .or_else(|| {
            db.config()
                .options
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone())
        })
        .and_then(|value| value.to_varchar().ok())
        .unwrap_or_default()
        .into()
}

pub(crate) struct Response {
    pub status: u16,
    pub etag: ByteString,
    pub reason: ByteString,
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Box<dyn Read + Send>,
    pub failed: Arc<AtomicBool>,
}

enum Framing {
    Length(u64),
    Chunked {
        remaining: u64,
        separator: bool,
        done: bool,
    },
    Close,
}

pub(crate) struct Body {
    input: BufReader<TcpStream>,
    framing: Framing,
    failed: Arc<AtomicBool>,
}

fn read_error() -> io::Error {
    io::Error::other(READ_ERROR)
}

fn line(input: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    input.read_until(b'\n', &mut line)?;
    if !line.ends_with(b"\r\n") {
        return Err(read_error());
    }
    line.truncate(line.len() - 2);
    Ok(line)
}

impl Body {
    fn read_body(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        match &mut self.framing {
            Framing::Length(remaining) => {
                if *remaining == 0 {
                    return Ok(0);
                }
                let amount = output
                    .len()
                    .min(usize::try_from(*remaining).unwrap_or(usize::MAX));
                let read = self.input.read(&mut output[..amount])?;
                if read == 0 {
                    return Err(read_error());
                }
                *remaining -= read as u64;
                Ok(read)
            }
            Framing::Close => self.input.read(output),
            Framing::Chunked {
                remaining,
                separator,
                done,
            } => {
                if *done {
                    return Ok(0);
                }
                if *remaining == 0 {
                    if *separator && !line(&mut self.input)?.is_empty() {
                        return Err(read_error());
                    }
                    let header = line(&mut self.input)?;
                    let number = header.split(|&b| b == b';').next().unwrap_or_default();
                    *remaining = std::str::from_utf8(number)
                        .ok()
                        .and_then(|n| u64::from_str_radix(n.trim(), 16).ok())
                        .ok_or_else(read_error)?;
                    if *remaining == 0 {
                        while !line(&mut self.input)?.is_empty() {}
                        *done = true;
                        return Ok(0);
                    }
                }
                let amount = output
                    .len()
                    .min(usize::try_from(*remaining).unwrap_or(usize::MAX));
                let read = self.input.read(&mut output[..amount])?;
                if read == 0 {
                    return Err(read_error());
                }
                *remaining -= read as u64;
                *separator = true;
                Ok(read)
            }
        }
    }
}

impl Read for Body {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.read_body(output).map_err(|_| {
            self.failed.store(true, Ordering::Relaxed);
            read_error()
        })
    }
}

struct TrackedBody {
    input: Box<dyn Read + Send>,
    failed: Arc<AtomicBool>,
}

impl Read for TrackedBody {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.input
            .read(output)
            .inspect_err(|_| self.failed.store(true, Ordering::Relaxed))
    }
}

fn host_port(authority: &str, default_port: u16) -> Option<(String, u16)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, rest) = rest.split_once(']')?;
        let port = if rest.is_empty() {
            default_port
        } else {
            rest.strip_prefix(':')?.parse().ok()?
        };
        return Some((host.to_owned(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_owned(), port.parse().ok()?)),
        None => Some((authority.to_owned(), default_port)),
    }
}

/// httplib encodes these path bytes while retaining existing percent escapes.
fn encode_path(path: &str) -> String {
    let (path, query) = path.split_once('?').map_or((path, None), |(p, q)| (p, Some(q)));
    let mut encoded = String::new();
    for &byte in path.as_bytes() {
        if byte >= 0x80 || b" \r\n',;".contains(&byte) {
            encoded.push('%');
            encoded.push(char::from(b"0123456789ABCDEF"[usize::from(byte >> 4)]));
            encoded.push(char::from(b"0123456789ABCDEF"[usize::from(byte & 15)]));
        } else {
            encoded.push(char::from(byte));
        }
    }
    if let Some(query) = query {
        encoded.push('?');
        encoded.push_str(query);
    }
    encoded
}

pub(crate) fn get(
    url: &[u8],
    db: &crate::Database,
    etag: Option<&[u8]>,
) -> std::result::Result<Response, String> {
    if db.external_extensions().contains(b"httpfs") {
        let url = std::str::from_utf8(url).map_err(|_| "Invalid extension URL".to_owned())?;
        let response = roastedduck_httpfs::extension_get(url, etag)?;
        let failed = Arc::new(AtomicBool::new(false));
        return Ok(Response {
            status: response.status,
            etag: response.etag,
            reason: response.reason,
            headers: response.headers,
            body: Box::new(TrackedBody {
                input: Box::new(response.body),
                failed: Arc::clone(&failed),
            }),
            failed,
        });
    }
    let connection_error = || "Could not establish connection".to_owned();
    let url_text = std::str::from_utf8(url).map_err(|_| connection_error())?;
    let rest = url_text.strip_prefix("http://").ok_or_else(connection_error)?;
    let (authority, path) = rest.split_once('/').ok_or_else(connection_error)?;
    let proxy = setting(db, "http_proxy");
    let proxy_text = std::str::from_utf8(&proxy).map_err(|_| connection_error())?;
    let target = if proxy.is_empty() {
        authority
    } else {
        proxy_text.strip_prefix("http://").unwrap_or(proxy_text)
    };
    let (host, port) = host_port(target, 80).ok_or_else(connection_error)?;
    let timeout = Duration::from_secs(30);
    let stream = (host.as_str(), port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addresses| {
            addresses.find_map(|address| TcpStream::connect_timeout(&address, timeout).ok())
        })
        .ok_or_else(connection_error)?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|_| READ_ERROR.to_owned())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|_| "Failed to write connection".to_owned())?;
    let request_path = encode_path(&if proxy.is_empty() {
        format!("/{path}")
    } else {
        url_text.to_owned()
    });
    let mut request = byte_format!(
        "GET {request_path} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: duckdb/v1.5.6({})\r\nConnection: close\r\n",
        crate::external_extensions::platform()
    );
    let username = setting(db, "http_proxy_username");
    if let Some(etag) = etag {
        request.push_bytes(byte_format!("If-None-Match: {etag}\r\n"));
    }
    if !proxy.is_empty() && !username.is_empty() {
        let password = setting(db, "http_proxy_password");
        let mut encoded = Vec::new();
        roastedduck_types::blob::write_base64(&byte_format!("{username}:{password}"), &mut encoded);
        request.push_bytes(byte_format!("Proxy-Authorization: Basic {encoded}\r\n"));
    }
    request.push_bytes("\r\n");
    let mut writer = &stream;
    writer
        .write_all(&request)
        .map_err(|_| "Failed to write connection".to_owned())?;
    let parse = || -> io::Result<Response> {
        let mut input = BufReader::new(stream);
        loop {
            let status_line = line(&mut input)?;
            let status = std::str::from_utf8(&status_line)
                .ok()
                .and_then(|s| s.split_whitespace().nth(1)?.parse::<u16>().ok())
                .ok_or_else(read_error)?;
            let reason = status_line
                .splitn(3, |&byte| byte == b' ')
                .nth(2)
                .unwrap_or_default()
                .into();
            let mut length = None;
            let mut chunked = false;
            let mut headers = Vec::new();
            loop {
                let header = line(&mut input)?;
                if header.is_empty() {
                    break;
                }
                let colon = header.iter().position(|&b| b == b':').ok_or_else(read_error)?;
                let (name, value) = header.split_at(colon);
                let value = value[1..].trim_ascii();
                let header_name = std::str::from_utf8(name).map_err(|_| read_error())?.to_owned();
                headers.push((header_name, value.to_vec()));
                if name.eq_ignore_ascii_case(b"content-length") {
                    length = Some(
                        std::str::from_utf8(value)
                            .ok()
                            .and_then(|s| s.parse::<u64>().ok())
                            .ok_or_else(read_error)?,
                    );
                } else if name.eq_ignore_ascii_case(b"transfer-encoding") {
                    chunked = value.eq_ignore_ascii_case(b"chunked");
                }
            }
            if (100..200).contains(&status) && status != 101 {
                continue;
            }
            let framing = if matches!(status, 204 | 304) {
                Framing::Length(0)
            } else if chunked {
                Framing::Chunked {
                    remaining: 0,
                    separator: false,
                    done: false,
                }
            } else if let Some(size) = length {
                Framing::Length(size)
            } else {
                Framing::Close
            };
            let headers = roastedduck_httpfs::extension_headers(headers, true);
            let etag = headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("etag"))
                .map_or_else(ByteString::new, |(_, value)| value.as_slice().into());
            let failed = Arc::new(AtomicBool::new(false));
            return Ok(Response {
                status,
                etag,
                reason,
                headers,
                body: Box::new(Body {
                    input,
                    framing,
                    failed: Arc::clone(&failed),
                }),
                failed,
            });
        }
    };
    parse().map_err(|_| READ_ERROR.to_owned())
}

/// C++ `UncompressGZIPString` checks this header then decodes the first raw DEFLATE
/// stream. It does not validate the gzip checksum or concatenate members.
pub(crate) fn decompress(input: &mut impl Read, output: &mut impl Write) -> Result<()> {
    let gzip_error = |message| Error::new(ErrorKind::Io, message);
    let mut header = [0; 10];
    if input.read_exact(&mut header).is_err() || header[..2] != [0x1f, 0x8b] {
        return Err(gzip_error("Input is not a GZIP stream"));
    }
    if header[2] != 8 {
        return Err(gzip_error("Unsupported GZIP compression method"));
    }
    if header[3] & 0x33 != 0 {
        return Err(gzip_error("Unsupported GZIP archive"));
    }
    if header[3] & 4 != 0 {
        return Err(gzip_error("Extra field in a GZIP stream unsupported"));
    }
    if header[3] & 8 != 0 {
        loop {
            let mut byte = [0];
            input
                .read_exact(&mut byte)
                .map_err(|_| gzip_error("Failed to uncompress"))?;
            if byte[0] == 0 {
                break;
            }
        }
    }
    let copied = io::copy(&mut flate2::read::DeflateDecoder::new(&mut *input), output)
        .map_err(|_| gzip_error("Failed to uncompress"))?;
    if copied == 0 {
        return Err(gzip_error("Failed to uncompress"));
    }
    // Consume the complete HTTP framing before validating/publishing the file.
    io::copy(input, &mut io::sink()).map_err(|_| gzip_error(READ_ERROR))?;
    Ok(())
}
