//! RDS IAM tokens use a presigned GET for the `rds-db` service, not an RDS
//! control-plane API request. Token creation performs no database/network call.
use std::fmt::Write as _;
use std::time::{Duration, SystemTime};

use aws_sigv4::http_request::{SignableBody, SignableRequest, SignatureLocation, SigningSettings, sign};
use aws_sigv4::sign::v4;

use crate::{Credentials, ResolveError};

fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

pub fn rds_auth_token(
    credentials: &Credentials,
    user: &str,
    host: &str,
    port: &str,
    region: &str,
    time: SystemTime,
) -> Result<String, ResolveError> {
    let identity = aws_credential_types::Credentials::new(
        credentials.access_key.clone(),
        credentials.secret_key.clone(),
        credentials.session_token.clone(),
        credentials.expires_at,
        "roastedDuck-RDS",
    )
    .into();
    let mut settings = SigningSettings::default();
    settings.signature_location = SignatureLocation::QueryParams;
    settings.expires_in = Some(Duration::from_mins(15));
    let params = v4::SigningParams::builder()
        .identity(&identity)
        .region(region)
        .name("rds-db")
        .time(time)
        .settings(settings)
        .build()
        .map_err(|_| ResolveError::Runtime("Invalid RDS signing parameters".into()))?
        .into();
    let uri = format!("http://{host}:{port}/?Action=connect&DBUser={}", encode(user));
    let request = SignableRequest::new("GET", &uri, std::iter::empty(), SignableBody::Bytes(&[]))
        .map_err(|_| ResolveError::Runtime("Invalid RDS signing URI".into()))?;
    let (instructions, _) = sign(request, &params)
        .map_err(|_| ResolveError::Runtime("Unable to sign RDS authentication token".into()))?
        .into_parts();
    let mut query = vec![
        ("Action".to_owned(), "connect".to_owned()),
        ("DBUser".to_owned(), encode(user)),
    ];
    let mut signature = None;
    for (name, value) in instructions.params() {
        if *name == "X-Amz-Signature" {
            signature = Some(encode(value));
        } else {
            query.push((encode(name), encode(value)));
        }
    }
    query.sort();
    query.push((
        "X-Amz-Signature".to_owned(),
        signature.ok_or_else(|| ResolveError::Runtime("Missing RDS signature".into()))?,
    ));
    Ok(format!(
        "{host}:{port}/?{}",
        query
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_pinned_cpp_rds_token_at_its_recorded_timestamp() {
        // Captured from the pinned C++1.5.6 AWS/Postgres extensions with fake
        // credentials; independently checked using Python's hashlib/hmac.
        let credentials = Credentials {
            access_key: "fixture-key".into(),
            secret_key: "fixture-secret".into(),
            session_token: Some("fixture-token".into()),
            expires_at: None,
        };
        let token = rds_auth_token(
            &credentials,
            "db user",
            "fixture.rds.amazonaws.com",
            "5432",
            "us-east-1",
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_794_694_396),
        )
        .unwrap();
        assert_eq!(
            token,
            "fixture.rds.amazonaws.com:5432/?Action=connect&DBUser=db%20user&X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=fixture-key%2F20261114%2Fus-east-1%2Frds-db%2Faws4_request&X-Amz-Date=20261114T221316Z&X-Amz-Expires=900&X-Amz-Security-Token=fixture-token&X-Amz-SignedHeaders=host&X-Amz-Signature=da6bf1682f663cafecdcd4063b415b53cfe98cde0fe26bbb628ae40893409f87"
        );
    }
}
