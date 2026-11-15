//! Metadata protocol compatibility while retaining the existing Rust HTTP stack.
use aws_credential_types::Credentials;
use aws_smithy_http_client::{Connector, tls};
use aws_smithy_runtime_api::client::connector_metadata::ConnectorMetadata;
use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpClient,
    SharedHttpConnector,
};
use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_runtime_api::client::runtime_components::{RuntimeComponents, RuntimeComponentsBuilder};
use aws_smithy_runtime_api::http::Request;
use aws_smithy_types::{
    DateTime, body::SdkBody, byte_stream::ByteStream, config_bag::ConfigBag, date_time::Format,
};
use std::time::{Duration, SystemTime};
type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The C++ ECS parser accepts an absent session token. Keep SDK URI validation,
/// token-file handling and transport. Apply the pinned one-retry policy and
/// optional token/expiry parsing before handing credentials back to the SDK.
#[derive(Debug)]
pub(crate) struct EcsClient(pub SharedHttpClient);
impl HttpClient for EcsClient {
    fn http_connector(
        &self,
        settings: &HttpConnectorSettings,
        components: &RuntimeComponents,
    ) -> SharedHttpConnector {
        SharedHttpConnector::new(EcsConnector(self.0.http_connector(settings, components)))
    }
    fn validate_base_client_config(
        &self,
        components: &RuntimeComponentsBuilder,
        config: &ConfigBag,
    ) -> Result<(), BoxError> {
        self.0.validate_base_client_config(components, config)
    }
    fn validate_final_config(
        &self,
        components: &RuntimeComponents,
        config: &ConfigBag,
    ) -> Result<(), BoxError> {
        self.0.validate_final_config(components, config)
    }
    fn connector_metadata(&self) -> Option<ConnectorMetadata> {
        self.0.connector_metadata()
    }
}
#[derive(Debug)]
struct EcsConnector(SharedHttpConnector);
impl HttpConnector for EcsConnector {
    fn call(&self, request: Request) -> HttpConnectorFuture {
        let connector = self.0.clone();
        HttpConnectorFuture::new(async move {
            for attempt in 0..2 {
                let request = request.try_clone().ok_or_else(invalid_metadata)?;
                let response = tokio::time::timeout(Duration::from_secs(1), async {
                    let mut response = connector.call(request).await?;
                    if response.status().as_u16() == 200 {
                        let bytes = ByteStream::new(response.take_body())
                            .collect()
                            .await
                            .map_err(|e| ConnectorError::other(Box::new(e), None))?
                            .into_bytes();
                        let serde_json::Value::Object(mut object) =
                            serde_json::from_slice(&bytes).map_err(|_| invalid_metadata())?
                        else {
                            return Err(invalid_metadata());
                        };
                        if ["AccessKeyId", "SecretAccessKey"].iter().any(|name| {
                            object
                                .get(*name)
                                .and_then(serde_json::Value::as_str)
                                .is_none_or(str::is_empty)
                        }) {
                            return Err(invalid_metadata());
                        }
                        let mut changed = false;
                        if !object.contains_key("Token") {
                            object.insert("Token".into(), "".into());
                            changed = true;
                        }
                        let expiration = object
                            .get("Expiration")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        if DateTime::from_str(expiration, Format::DateTimeWithOffset).is_err() {
                            object.insert("Expiration".into(), "1970-01-01T00:00:00Z".into());
                            changed = true;
                        }
                        let body = if changed {
                            serde_json::to_vec(&object).map_err(|_| invalid_metadata())?
                        } else {
                            bytes.to_vec()
                        };
                        response
                            .headers_mut()
                            .insert("content-length", body.len().to_string());
                        *response.body_mut() = SdkBody::from(body);
                    }
                    Ok(response)
                })
                .await;
                match response {
                    Ok(Ok(response)) if response.status().as_u16() == 200 => return Ok(response),
                    Ok(Ok(response)) if !retryable(response.status().as_u16()) => {
                        return Err(invalid_metadata());
                    }
                    Ok(Err(error)) if error.is_user() => return Err(error),
                    _ if attempt == 0 => {}
                    _ => return Err(invalid_metadata()),
                }
            }
            Err(invalid_metadata())
        })
    }
}

fn invalid_metadata() -> ConnectorError {
    ConnectorError::user(std::io::Error::other("metadata credentials unavailable").into())
}
fn retryable(status: u16) -> bool {
    matches!(status, 408 | 429 | 500..=599)
}

struct Response {
    status: u16,
    body: String,
}
async fn fetch(connector: &Connector, method: &str, uri: &str, headers: &[(&str, &str)]) -> Response {
    for attempt in 0..2 {
        let call = async {
            let mut request = Request::get(uri).ok()?;
            request.set_method(method).ok()?;
            for (name, value) in headers {
                request
                    .headers_mut()
                    .try_insert((*name).to_owned(), (*value).to_owned())
                    .ok()?;
            }
            let response = connector.call(request).await.ok()?;
            let status = response.status().as_u16();
            let body = ByteStream::new(response.into_body())
                .collect()
                .await
                .ok()?
                .into_bytes();
            Some(Response {
                status,
                body: String::from_utf8(body.to_vec()).ok()?,
            })
        };
        let response = tokio::time::timeout(Duration::from_secs(1), call)
            .await
            .ok()
            .flatten();
        let retry = response.as_ref().is_none_or(|r| retryable(r.status));
        if !retry || attempt == 1 {
            return response.unwrap_or(Response {
                status: 0,
                body: String::new(),
            });
        }
    }
    Response {
        status: 0,
        body: String::new(),
    }
}
fn credentials(body: &str, instance: bool) -> Option<Credentials> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    if instance && json.get("Code").and_then(serde_json::Value::as_str) == Some("InternalServiceException") {
        return None;
    }
    let access = json.get("AccessKeyId")?.as_str()?;
    let secret = json.get("SecretAccessKey")?.as_str()?;
    if access.is_empty() || secret.is_empty() {
        return None;
    }
    let expiration = DateTime::from_str(
        json.get("Expiration")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
        Format::DateTimeWithOffset,
    )
    .unwrap_or(DateTime::from_secs(0));
    Some(Credentials::new(
        access,
        secret,
        json.get("Token")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        if instance {
            None
        } else {
            Some(SystemTime::try_from(expiration).ok()?)
        },
        "roastedDuck-instance",
    ))
}

/// This entry point is also used for explicit CHAIN 'instance', which the
/// pinned extension resolves even when its default-chain metadata flag is off.
pub(crate) async fn instance() -> Option<Credentials> {
    let endpoint = std::env::var("AWS_EC2_METADATA_SERVICE_ENDPOINT")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if std::env::var("AWS_EC2_METADATA_SERVICE_ENDPOINT_MODE")
                .is_ok_and(|s| s.eq_ignore_ascii_case("ipv6"))
            {
                "http://[fd00:ec2::254]"
            } else {
                "http://169.254.169.254"
            }
            .into()
        });
    let endpoint = endpoint.trim_end_matches('/');
    let connector = Connector::builder()
        .tls_provider(tls::Provider::Rustls(tls::rustls_provider::CryptoMode::Ring))
        .build();
    let token = fetch(
        &connector,
        "PUT",
        &format!("{endpoint}/latest/api/token"),
        &[("x-aws-ec2-metadata-token-ttl-seconds", "21600")],
    )
    .await;
    if token.status == 400 {
        return None;
    }
    let value = if token.status == 200 {
        token.body.trim_ascii()
    } else {
        ""
    };
    let disabled_v1 =
        std::env::var("AWS_EC2_METADATA_V1_DISABLED").is_ok_and(|s| s.eq_ignore_ascii_case("true"));
    let headers = if !value.is_empty() || disabled_v1 {
        vec![("x-aws-ec2-metadata-token", value)]
    } else {
        Vec::new()
    };
    let roles = fetch(
        &connector,
        "GET",
        &format!("{endpoint}/latest/meta-data/iam/security-credentials"),
        &headers,
    )
    .await;
    if roles.status != 200 {
        return None;
    }
    let role = roles
        .body
        .trim_ascii()
        .split('\n')
        .next()
        .filter(|s| !s.is_empty())?;
    let response = fetch(
        &connector,
        "GET",
        &format!("{endpoint}/latest/meta-data/iam/security-credentials/{role}"),
        &headers,
    )
    .await;
    if response.status != 200 {
        return None;
    }
    credentials(&response.body, true)
}

fn allowed_address(address: std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(ip) => {
            ip.octets()[..3] == [127, 0, 0] || matches!(ip.octets(), [169, 254, 170, 2 | 23])
        }
        std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.segments() == [0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x23],
    }
}
async fn allowed_uri(uri: &url::Url) -> bool {
    if uri.scheme() == "https" {
        return true;
    }
    if uri.scheme() != "http" {
        return false;
    }
    let Some(host) = uri.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(address) = host.parse() {
        return allowed_address(address);
    }
    let addresses = tokio::time::timeout(
        Duration::from_secs(1),
        tokio::net::lookup_host((host, uri.port_or_known_default().unwrap_or(80))),
    )
    .await;
    let Ok(Ok(addresses)) = addresses else {
        return false;
    };
    let addresses: Vec<_> = addresses.collect();
    !addresses.is_empty() && addresses.iter().all(|a| allowed_address(a.ip()))
}

pub(crate) async fn ecs(
    providers: &aws_config::provider_config::ProviderConfig,
    sdk: &aws_types::SdkConfig,
) -> Option<Credentials> {
    use aws_credential_types::provider::ProvideCredentials;
    let relative = std::env::var("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI").ok();
    if relative
        .as_deref()
        .is_some_and(|s| !s.is_empty() && !s.starts_with('/'))
    {
        return None;
    }
    // Unlike the pinned SDK, the Rust SDK treats an empty relative-URI variable
    // as configured and cannot be given an explicit URI override. Resolve this
    // case without modifying the caller's process environment.
    let empty_file = std::env::var("AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE").is_ok_and(|s| s.is_empty());
    if relative.as_deref() != Some("") && !empty_file {
        let providers = providers
            .clone()
            .with_http_client(EcsClient(sdk.http_client()?.clone()));
        return aws_config::ecs::EcsCredentialsProvider::builder()
            .configure(&providers)
            .build()
            .provide_credentials()
            .await
            .ok()
            .filter(|c| !c.access_key_id().is_empty() && !c.secret_access_key().is_empty());
    }
    let address = if let Some(relative) = relative.filter(|s| !s.is_empty()) {
        format!("http://169.254.170.2{relative}")
    } else {
        std::env::var("AWS_CONTAINER_CREDENTIALS_FULL_URI").ok()?
    };
    let uri = url::Url::parse(&address).ok()?;
    if !allowed_uri(&uri).await {
        return None;
    }
    let mut token = std::env::var("AWS_CONTAINER_AUTHORIZATION_TOKEN").unwrap_or_default();
    if token.contains("\r\n") {
        return None;
    }
    if let Ok(path) = std::env::var("AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE")
        && !path.is_empty()
    {
        token = std::fs::read_to_string(path).ok()?;
    }
    if token.contains(['\r', '\n']) {
        return None;
    }
    let headers = if token.is_empty() {
        Vec::new()
    } else {
        vec![("authorization", token.as_str())]
    };
    let connector = Connector::builder()
        .tls_provider(tls::Provider::Rustls(tls::rustls_provider::CryptoMode::Ring))
        .build();
    let response = fetch(&connector, "GET", uri.as_str(), &headers).await;
    if response.status != 200 {
        return None;
    }
    credentials(&response.body, false)
}
