//! Single-object removal and credential-separated `ListObjectsV2` deletion batches.
use md5::{Digest, Md5};
use roastedduck_common::{ByteString, Error, ErrorKind, Result};
use std::io::Read as _;

use super::{S3FileSystem, S3Request, SECRET_RESOLVER_KEY, SecretResolver, encode, io_error, tag};
use crate::HttpFile;

pub(super) fn remove(path: &[u8]) -> Result<()> {
    roastedduck_function::file_system::verify_can_write(path)?;
    let file = match S3FileSystem::open_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::Http && error.raw_message().contains("HTTP 404") => {
            return Err(Error::new_bytes(
                ErrorKind::Io,
                roastedduck_common::byte_format!(
                    "Could not remove file \"{path}\": No such file or directory"
                ),
            )
            .with_extra_info("errno", "404"));
        }
        Err(error) => return Err(error),
    };
    let response = file.request_payload("DELETE", None, &[], true)?;
    let status = response.status();
    if status.as_u16() == 200 || status.as_u16() == 204 {
        return Ok(());
    }
    Err(Error::new_bytes(
        ErrorKind::Io,
        roastedduck_common::byte_format!(
            "Could not remove file \"{path}\": {}",
            status.canonical_reason().unwrap_or_default()
        ),
    )
    .with_extra_info("errno", status.as_u16().to_string()))
}

/// No Debug implementation: a batching key contains credential material.
struct Batch {
    identity: Vec<Vec<u8>>,
    request: S3Request,
    keys: Vec<String>,
}

fn identity(request: &S3Request, path: &str) -> Result<Vec<Vec<u8>>> {
    let s = &request.signing;
    let p = &request.parameters;
    let root_path = &s.path[..s.path.len() - request.key.len()];
    let protocol = request.url.split_once("://").map_or("", |(protocol, _)| protocol);
    let mut key = [
        request.bucket_url.as_str(),
        protocol,
        &s.host,
        root_path,
        &s.region,
        &s.key_id,
        &s.secret,
        &s.token,
        &s.kms_key_id,
        &s.bearer,
        &p.proxy,
        &p.proxy_user,
        &p.proxy_password,
        &p.ca,
    ]
    .into_iter()
    .map(|s| s.as_bytes().to_vec())
    .collect::<Vec<_>>();
    key.push(s.requester_pays.to_string().into_bytes());
    key.push(p.verify.to_string().into_bytes());
    let mut headers = p.headers.clone();
    headers.sort();
    key.push(headers.len().to_string().into_bytes());
    for (name, value) in headers {
        key.push(name.into_bytes());
        key.push(value.into_bytes());
    }
    if let Some(resolver) = roastedduck_function::file_system::opener()
        .and_then(|c| c.object_cache())
        .and_then(|c| c.get::<SecretResolver>(SECRET_RESOLVER_KEY))
    {
        key.extend(resolver.0.batch_identity(path)?);
    }
    Ok(key)
}

pub(super) fn remove_files(paths: &[ByteString]) -> Result<()> {
    let mut batches: Vec<Batch> = Vec::new();
    for path in paths {
        roastedduck_function::file_system::verify_can_write(path)?;
        let request = S3FileSystem::prepare(path, false)?;
        let key = identity(&request, std::str::from_utf8(path).map_err(io_error)?)?;
        if let Some(batch) = batches.iter_mut().find(|batch| batch.identity == key) {
            batch.keys.push(request.key);
        } else {
            let first = request.key.clone();
            batches.push(Batch {
                identity: key,
                request,
                keys: vec![first],
            });
        }
    }
    for batch in batches {
        for keys in batch.keys.chunks(1000) {
            let mut body = String::from(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Delete xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
            );
            // The pinned implementation writes the original key bytes verbatim.
            for key in keys {
                body.push_str("<Object><Key>");
                body.push_str(key);
                body.push_str("</Key></Object>");
            }
            body.push_str("<Quiet>true</Quiet></Delete>");
            let mut signing = batch.request.signing.clone();
            // C++ bulk deletion does not use the per-object region redirect wrapper.
            signing.allow_region_redirect = false;
            signing
                .path
                .truncate(signing.path.len() - batch.request.key.len());
            signing.bucket_request = true;
            signing.query = "delete=".into();
            signing.content_type = "application/xml".into();
            let digest = Md5::digest(body.as_bytes());
            let mut encoded = Vec::new();
            roastedduck_types::blob::write_base64(digest.as_slice(), &mut encoded);
            signing.content_md5 = String::from_utf8(encoded).map_err(io_error)?;
            let protocol = batch
                .request
                .url
                .split_once("://")
                .map_or("https", |(protocol, _)| protocol);
            let url = format!(
                "{protocol}://{}{}?delete",
                signing.host,
                encode(&signing.path, false)
            );
            let file = HttpFile::new_request(url, batch.request.parameters.clone(), Some(signing))?;
            let mut response = file.request_payload("POST", None, body.as_bytes(), true)?;
            let status = response.status();
            let mut result = String::new();
            response
                .body_mut()
                .as_reader()
                .read_to_string(&mut result)
                .map_err(io_error)?;
            if status.as_u16() != 200 {
                return Err(Error::new(
                    ErrorKind::Io,
                    format!(
                        "Failed to remove files: HTTP {} ({})\n{result}",
                        status.as_u16(),
                        status.canonical_reason().unwrap_or_default()
                    ),
                ));
            }
            if let Some((error, _)) = tag(&result, "Error", 0)? {
                return Err(Error::new(
                    ErrorKind::Io,
                    format!("Failed to remove files: {error}"),
                ));
            }
        }
    }
    Ok(())
}
