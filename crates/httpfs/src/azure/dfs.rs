//! ADLS Gen2 path operations and hierarchical glob traversal.
use std::io::Read;
use std::sync::Arc;

use roastedduck_common::{ByteString, Error, Result};
use roastedduck_function::logging::FileSystemLogger;
use roastedduck_function::registered_files::{FileHandle, FileSystem};

use super::client::Client;
use super::url::encode;
use super::{AzureBlobFileSystem, io_error};

#[derive(Debug)]
pub struct AzureDfsFileSystem;

#[derive(Debug)]
struct PathEntry {
    name: String,
    directory: bool,
}

fn client(path: &[u8]) -> Result<Client> {
    Client::new(std::str::from_utf8(path).map_err(io_error)?)
}

fn paths(client: &Client, directory: &str, recursive: bool) -> Result<Vec<PathEntry>> {
    let mut result = Vec::new();
    let mut marker = String::new();
    loop {
        let mut query = format!(
            "resource=filesystem&recursive={recursive}&continuation={}",
            encode(&marker, false)
        );
        if !directory.is_empty() {
            query.push_str("&directory=");
            query.push_str(&encode(directory, false));
        }
        let mut response = client.request("GET", None, &query, Vec::new(), &[])?;
        client.check(&response, "ListPaths")?;
        let next = response
            .headers()
            .get("x-ms-continuation")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .read_to_end(&mut body)
            .map_err(io_error)?;
        let value: serde_json::Value = serde_json::from_slice(&body).map_err(io_error)?;
        if let Some(entries) = value.get("paths").and_then(serde_json::Value::as_array) {
            for entry in entries {
                let name = entry
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| io_error("ADLS path has no name"))?;
                let directory = entry
                    .get("isDirectory")
                    .is_some_and(|v| v.as_bool() == Some(true) || v.as_str() == Some("true"));
                result.push(PathEntry {
                    name: name.into(),
                    directory,
                });
            }
        }
        if next.is_empty() {
            return Ok(result);
        }
        if next == marker {
            return Err(io_error("ADLS listing repeated its continuation marker"));
        }
        marker = next;
    }
}

fn walk(
    client: &Client,
    directory: &str,
    pattern: &str,
    end: usize,
    result: &mut Vec<ByteString>,
) -> Result<()> {
    let recursive = pattern[..end].contains("**");
    if recursive && end < pattern.len() {
        return Err(Error::not_implemented(format!(
            "abfss do not manage recursive lookup patterns, {pattern} is therefor illegal, only pattern ending by ** are allowed."
        )));
    }
    for entry in paths(client, directory, recursive)? {
        if entry.directory {
            if !recursive
                && end < pattern.len()
                && roastedduck_common::glob::glob(entry.name.as_bytes(), &pattern.as_bytes()[..end])
            {
                let next = pattern[end + 1..]
                    .find('/')
                    .map_or(pattern.len(), |offset| end + 1 + offset);
                walk(client, &entry.name, pattern, next, result)?;
            }
        } else if roastedduck_common::glob::glob(entry.name.as_bytes(), pattern.as_bytes()) {
            result.push(format!("{}{}", client.parsed.result_prefix(), entry.name).into());
        }
    }
    Ok(())
}

fn directory_header(response: &ureq::http::Response<crate::body::Body>) -> bool {
    response
        .headers()
        .get("x-ms-resource-type")
        .is_some_and(|v| v == "directory")
        || response
            .headers()
            .get("x-ms-meta-hdi_isfolder")
            .is_some_and(|v| v == "true")
}

impl FileSystem for AzureDfsFileSystem {
    fn name(&self) -> &'static str {
        "AzureDfsStorageFileSystem"
    }
    fn is_remote(&self) -> bool {
        true
    }
    fn handles(&self, path: &[u8]) -> bool {
        path.starts_with(b"abfs://") || path.starts_with(b"abfss://")
    }
    fn open(&self, path: &[u8], write: bool) -> Result<Arc<dyn FileHandle>> {
        AzureBlobFileSystem.open(path, write)
    }
    fn open_read_direct(&self, path: &[u8]) -> Result<Arc<dyn FileHandle>> {
        AzureBlobFileSystem.open_read_direct(path)
    }
    fn glob(&self, path: &[u8]) -> Result<Vec<ByteString>> {
        let client = client(path)?;
        let Some(wildcard) = client.parsed.path.find(['*', '[', '\\']) else {
            return if self.is_file(path)? {
                Ok(vec![path.into()])
            } else {
                Ok(Vec::new())
            };
        };
        let root = client.parsed.path[..wildcard].rfind('/').unwrap_or(0);
        let end = client.parsed.path[root + 1..]
            .find('/')
            .map_or(client.parsed.path.len(), |offset| root + 1 + offset);
        let mut result = Vec::new();
        walk(
            &client,
            &client.parsed.path[..root],
            &client.parsed.path,
            end,
            &mut result,
        )?;
        Ok(result)
    }
    fn is_file(&self, path: &[u8]) -> Result<bool> {
        let client = client(path)?;
        let response = client.request(
            "HEAD",
            Some(client.parsed.path.trim_end_matches('/')),
            "",
            Vec::new(),
            &[],
        )?;
        if response.status().as_u16() == 404 {
            return Ok(false);
        }
        client.check(&response, "open file")?;
        let _ = FileSystemLogger::open(path);
        Ok(!path.ends_with(b"/") && !directory_header(&response))
    }
    fn is_dir(&self, path: &[u8]) -> Result<bool> {
        let client = client(path)?;
        let response = client.request(
            "HEAD",
            Some(client.parsed.path.trim_end_matches('/')),
            "",
            Vec::new(),
            &[],
        )?;
        if response.status().as_u16() == 404 {
            return Ok(false);
        }
        client.check(&response, "open file")?;
        let _ = FileSystemLogger::open(path);
        Ok(path.ends_with(b"/") || directory_header(&response))
    }
    fn mkdir(&self, path: &[u8]) -> Result<()> {
        let client = client(path)?;
        let response = client.request(
            "PUT",
            Some(&client.parsed.path),
            "resource=directory",
            Vec::new(),
            &[],
        )?;
        client.check(&response, "CreateDirectory")
    }
    fn remove(&self, path: &[u8]) -> Result<()> {
        let client = client(path)?;
        let response = client.request("DELETE", Some(&client.parsed.path), "", Vec::new(), &[])?;
        client.check(&response, "Delete of")
    }
    fn rename(&self, _source: &[u8], _target: &[u8]) -> Result<()> {
        // The pinned extension inherits FileSystem's unsupported MoveFile.
        Err(Error::not_implemented(
            "AzureDfsStorageFileSystem does not support moving files",
        ))
    }
}
