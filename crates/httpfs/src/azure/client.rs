use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::SystemTime;

use ring::hmac;
use roastedduck_common::{Error, Result};
use roastedduck_types::blob::{parse_base64, write_base64};

use super::url::{AzureUrl, decode, encode};
use super::{SECRET_RESOLVER_KEY, SecretResolver, check_interrupt, io_error, setting};
use crate::{HttpFile, Parameters, body};

#[derive(Clone, Debug)]
pub(super) struct Client {
    pub parsed: AzureUrl,
    pub original: String,
    base: String,
    account: String,
    auth: Auth,
    parameters: Parameters,
    user_agent: String,
}

#[derive(Clone)]
enum Auth {
    Anonymous,
    SharedKey(Vec<u8>),
    Sas(String),
    Bearer(String),
}
impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AzureCredential")
    }
}

impl Client {
    pub fn new(path: &str) -> Result<Self> {
        let parsed = AzureUrl::parse(path)?;
        let resolver = roastedduck_function::file_system::opener()
            .and_then(|c| c.object_cache())
            .and_then(|cache| cache.get::<SecretResolver>(SECRET_RESOLVER_KEY));
        let user_agent = resolver.as_ref().map_or_else(String::new, |r| r.0.user_agent());
        let secret = resolver
            .map(|resolver| resolver.0.resolve(path))
            .transpose()?
            .flatten();
        let text = |key: &str| -> Result<Option<String>> {
            secret
                .as_ref()
                .and_then(|s| s.values.get(key))
                .filter(|v| !v.is_null())
                .map(roastedduck_types::Value::to_varchar)
                .transpose()
        };
        let required = |key: &str| -> Result<String> {
            text(key)?.ok_or_else(|| {
                Error::invalid_input(format!(
                    "Failed to fetch key '{key}' from secret '{}' of type 'azure'",
                    secret.as_ref().map_or("", |s| s.name.as_str())
                ))
            })
        };
        let transport = setting("azure_transport_option_type", "default");
        if !matches!(transport.as_str(), "default" | "curl") {
            return Err(Error::invalid_input(format!(
                "transport_option_type cannot take value '{transport}'"
            )));
        }
        let mut parameters = Parameters::current_with_http_secret(path, false, false)?;
        parameters.httplib = false;
        parameters.verify = true;
        parameters.redirects = 0;
        parameters.ca = std::env::var("CURL_CA_INFO").unwrap_or_default();
        parameters.proxy = if secret.is_some() {
            text("http_proxy")?.unwrap_or_else(|| std::env::var("HTTP_PROXY").unwrap_or_default())
        } else {
            setting("azure_http_proxy", "")
        };
        parameters.proxy_user = if secret.is_some() {
            text("proxy_user_name")?.unwrap_or_default()
        } else {
            setting("azure_proxy_user_name", "")
        };
        parameters.proxy_password = if secret.is_some() {
            text("proxy_password")?.unwrap_or_default()
        } else {
            setting("azure_proxy_password", "")
        };
        let provider = secret.as_ref().map_or("config", |s| s.provider.as_str());
        let mut connection = if secret.is_some() {
            text("connection_string")?
        } else {
            Some(setting("azure_storage_connection_string", "")).filter(|s| !s.is_empty())
        };
        if secret.is_none()
            && let Some(value) = &connection
        {
            let name = value
                .find("AccountName=")
                .map(|start| &value[start + 12..])
                .ok_or_else(|| Error::invalid_input("A invalid connection string has been provided."))?;
            if !name.starts_with(&parsed.account) {
                connection = None;
            }
        }
        let mut account = parsed.account.clone();
        let (base, auth) = if let Some(connection) = connection.filter(|_| provider == "config") {
            let fields: BTreeMap<_, _> = connection
                .split(';')
                .filter(|s| !s.is_empty())
                .map(|s| {
                    s.split_once('=')
                        .ok_or_else(|| Error::invalid_input("A invalid connection string has been provided."))
                })
                .collect::<Result<_>>()?;
            let name = fields.get("AccountName").copied().unwrap_or_default();
            if parsed.qualified() && !name.starts_with(&account) {
                return Err(Error::invalid_input(format!(
                    "The provided connection string does not match the storage account named {account}"
                )));
            }
            account = name.into();
            let endpoint_key = if parsed.dfs() {
                "DfsEndpoint"
            } else {
                "BlobEndpoint"
            };
            let default_endpoint = format!(
                "{}://{account}.{}.{}",
                fields.get("DefaultEndpointsProtocol").unwrap_or(&"https"),
                if parsed.dfs() { "dfs" } else { "blob" },
                fields.get("EndpointSuffix").unwrap_or(&"core.windows.net")
            );
            let base = fields.get(endpoint_key).map_or(default_endpoint, |s| (*s).into());
            let auth = if let Some(key) = fields.get("AccountKey") {
                let mut bytes = Vec::new();
                parse_base64(key.as_bytes(), &mut bytes).map_err(io_error)?;
                Auth::SharedKey(bytes)
            } else if let Some(sas) = fields.get("SharedAccessSignature") {
                Auth::Sas(sas.trim_start_matches('?').into())
            } else {
                Auth::Anonymous
            };
            (base, auth)
        } else {
            if !parsed.qualified() {
                account = if secret.is_some() {
                    required("account_name")?
                } else {
                    setting("azure_account_name", "")
                };
                if account.is_empty() {
                    return Err(Error::invalid_input("No valid Azure credentials found!"));
                }
            }
            let endpoint = if parsed.qualified() {
                parsed.endpoint.clone()
            } else if secret.is_some() {
                text("endpoint")?.unwrap_or_else(|| "blob.core.windows.net".into())
            } else {
                setting("azure_endpoint", "blob.core.windows.net")
            };
            let auth = match provider {
                "config" if secret.is_none() && !setting("azure_credential_chain", "").is_empty() => {
                    return Err(Error::not_implemented(
                        "Azure credential chain is not implemented yet",
                    ));
                }
                "config" => Auth::Anonymous,
                "access_token" => Auth::Bearer(required("access_token")?),
                // Identity providers resolve at file access, as in the pinned SDK.
                _ => {
                    return Err(Error::not_implemented(format!(
                        "Azure identity provider '{provider}' is not implemented yet"
                    )));
                }
            };
            (format!("https://{account}.{endpoint}"), auth)
        };
        Ok(Self {
            parsed,
            original: path.into(),
            base: base.trim_end_matches('/').into(),
            account,
            auth,
            parameters,
            user_agent,
        })
    }

    pub fn request(
        &self,
        method: &str,
        object: Option<&str>,
        query: &str,
        mut headers: Vec<(String, String)>,
        payload: &[u8],
    ) -> Result<ureq::http::Response<body::Body>> {
        check_interrupt()?;
        let mut url = format!("{}/{}", self.base, encode(&self.parsed.container, false));
        if let Some(object) = object {
            url.push('/');
            url.push_str(&encode(object, true));
        }
        let query = match &self.auth {
            Auth::Sas(sas) if !query.is_empty() => format!("{query}&{sas}"),
            Auth::Sas(sas) => sas.clone(),
            _ => query.into(),
        };
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query);
        }
        headers.push(("x-ms-date".into(), httpdate::fmt_http_date(SystemTime::now())));
        headers.push(("x-ms-version".into(), "2025-07-05".into()));
        headers.push(("User-Agent".into(), self.user_agent.clone()));
        if matches!(method, "PUT" | "POST" | "PATCH") {
            headers.push(("Content-Length".into(), payload.len().to_string()));
        }
        match &self.auth {
            Auth::SharedKey(key) => {
                let uri: ureq::http::Uri = url.parse().map_err(io_error)?;
                let signed: BTreeMap<_, _> = headers
                    .iter()
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let mut canonical = method.to_owned();
                for key in [
                    "content-encoding",
                    "content-language",
                    "content-length",
                    "content-md5",
                    "content-type",
                    "date",
                    "if-modified-since",
                    "if-match",
                    "if-none-match",
                    "if-unmodified-since",
                    "range",
                ] {
                    canonical.push('\n');
                    if let Some(value) = signed.get(key).filter(|v| key != "content-length" || *v != "0") {
                        canonical.push_str(value);
                    }
                }
                canonical.push('\n');
                for (key, value) in &signed {
                    if key.starts_with("x-ms-") {
                        let _ = writeln!(canonical, "{key}:{value}");
                    }
                }
                let _ = write!(canonical, "/{}{}", self.account, uri.path());
                let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
                for field in query.split('&').filter(|v| !v.is_empty()) {
                    let (key, value) = field.split_once('=').unwrap_or((field, ""));
                    fields
                        .entry(decode(key)?.to_ascii_lowercase())
                        .or_default()
                        .push(decode(value)?);
                }
                for (key, mut values) in fields {
                    values.sort();
                    let _ = write!(canonical, "\n{key}:{}", values.join(","));
                }
                let signature = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), canonical.as_bytes());
                let mut encoded = Vec::new();
                write_base64(signature.as_ref(), &mut encoded);
                headers.push((
                    "Authorization".into(),
                    format!("SharedKey {}:{}", self.account, String::from_utf8_lossy(&encoded)),
                ));
            }
            Auth::Bearer(token) => {
                headers.push(("Authorization".into(), format!("Bearer {token}")));
            }
            Auth::Anonymous | Auth::Sas(_) => {}
        }
        let mut parameters = self.parameters.clone();
        parameters.headers = headers;
        let mut request = HttpFile::new_request(url, parameters, None)?;
        let split = |value: String| {
            value
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        };
        request.logger = if setting("azure_http_logging", "true") == "true" {
            crate::logging::HttpLogger::azure(
                split(setting("azure_http_logging_redact_query_params", "sig")),
                split(setting("azure_http_logging_redact_headers", "Authorization").to_ascii_lowercase()),
            )
        } else {
            None
        };
        request.azure_stats = Some(setting("azure_http_stats", "false") == "true");
        request.request_payload(method, None, payload, true)
    }

    pub fn check(&self, response: &ureq::http::Response<body::Body>, action: &str) -> Result<()> {
        if response.status().is_success() {
            return Ok(());
        }
        let code = response
            .headers()
            .get("x-ms-error-code")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let reason = response
            .extensions()
            .get::<ureq_proto::OriginalResponseMetadata>()
            .map_or_else(
                || response.status().canonical_reason().unwrap_or_default(),
                |m| m.reason.as_str(),
            );
        if action == "open file" {
            return Err(io_error(format!(
                "AzureStorageFileSystem open file '{}' failed with code '{code}', Reason Phrase: '{reason}', Message: ''",
                self.original
            )));
        }
        Err(io_error(format!(
            "AzureBlobStorageFileSystem {action} '{}' failed with {code} Reason Phrase: {}",
            self.original, reason
        )))
    }
}
