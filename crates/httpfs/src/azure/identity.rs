//! Azure identity ordering and wire contracts from azure-identity-cpp 1.13.2.
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::url::encode;
use super::{AzureSecret, check_interrupt, io_error};
use crate::{HttpFile, Parameters};
use roastedduck_common::{Error, Result};
use roastedduck_types::{Timestamp, Value};

const SCOPE: &str = "https://storage.azure.com/.default";
const RESOURCE: &str = "https://storage.azure.com";

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn text(values: &BTreeMap<String, Value>, key: &str) -> Result<Option<String>> {
    values
        .get(key)
        .filter(|v| !v.is_null())
        .map(Value::to_varchar)
        .transpose()
}

pub(super) struct Identity {
    sources: Vec<Credential>,
    reuse: bool,
    chosen: Mutex<Option<usize>>,
    name: &'static str,
}
impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AzureIdentity")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}
struct Credential {
    kind: Kind,
    cached: Mutex<Option<Token>>,
}
struct Token {
    value: String,
    expires: u64,
}
enum Kind {
    Unavailable(String),
    Principal {
        tenant: String,
        client: String,
        authority: String,
        proof: Proof,
    },
    Managed {
        url: String,
        headers: Vec<(String, String)>,
        post: bool,
        arc: bool,
        probe: AtomicBool,
        inherit_transport: bool,
    },
    Cli,
    Chain(Identity),
}
enum Proof {
    Secret(String),
    Workload(String),
    Certificate(Box<super::certificate::Certificate>),
}

impl Credential {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            cached: Mutex::new(None),
        }
    }
    fn token(&self, parameters: &Parameters) -> Result<String> {
        if let Kind::Chain(identity) = &self.kind {
            return identity.token(parameters);
        }
        let mut cache = self.cached.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(token) = cache.as_ref().filter(|t| t.expires > now().saturating_add(120)) {
            return Ok(token.value.clone());
        }
        check_interrupt()?;
        let token = match &self.kind {
            Kind::Unavailable(message) => return Err(io_error(message)),
            Kind::Cli => parse_token(&super::process::token()?, true, false)?,
            Kind::Principal {
                tenant,
                client,
                authority,
                proof,
            } => {
                if !authority.starts_with("https://") {
                    return Err(io_error(
                        "Authority host must be a TLS protected (https) endpoint.",
                    ));
                }
                let adfs = tenant.eq_ignore_ascii_case("adfs");
                let endpoint = format!(
                    "{}/{}/{}",
                    authority.trim_end_matches('/'),
                    encode(tenant, false),
                    if adfs { "oauth2/token" } else { "oauth2/v2.0/token" }
                );
                let mut fields = vec![
                    ("grant_type", "client_credentials".into()),
                    ("client_id", client.clone()),
                ];
                match proof {
                    Proof::Secret(secret) => fields.push(("client_secret", secret.clone())),
                    Proof::Workload(path) => {
                        fields.push((
                            "client_assertion_type",
                            "urn:ietf:params:oauth:client-assertion-type:jwt-bearer".into(),
                        ));
                        // The SDK reads the file on refresh; Kubernetes may rotate it.
                        fields.push((
                            "client_assertion",
                            std::fs::read_to_string(path).unwrap_or_default(),
                        ));
                    }
                    Proof::Certificate(certificate) => {
                        fields.push((
                            "client_assertion_type",
                            "urn:ietf:params:oauth:client-assertion-type:jwt-bearer".into(),
                        ));
                        fields.push(("client_assertion", certificate.assertion(&endpoint, client)?));
                    }
                }
                fields.push(("scope", if adfs { RESOURCE } else { SCOPE }.into()));
                let payload = fields
                    .into_iter()
                    .map(|(k, v)| format!("{k}={}", encode(&v, false)))
                    .collect::<Vec<_>>()
                    .join("&");
                let response = request(parameters, "POST", &endpoint, Vec::new(), payload.as_bytes())?;
                parse_token(&response, false, false)?
            }
            Kind::Managed {
                url,
                headers,
                post,
                arc,
                probe,
                inherit_transport,
            } => {
                let mut parameters = parameters.clone();
                if !inherit_transport {
                    parameters.ca.clear();
                    parameters.proxy.clear();
                    parameters.proxy_user.clear();
                    parameters.proxy_password.clear();
                }
                if probe.load(Ordering::Relaxed) {
                    parameters.timeout = 1;
                    parameters.retries = 0;
                }
                let payload = if *post {
                    format!("resource={}", encode(RESOURCE, false))
                } else {
                    String::new()
                };
                let mut endpoint = url.clone();
                if !post {
                    add_query(&mut endpoint, "resource", RESOURCE);
                }
                let bytes = if *arc {
                    arc_request(&parameters, &endpoint, headers.clone())?
                } else {
                    request(
                        &parameters,
                        if *post { "POST" } else { "GET" },
                        &endpoint,
                        headers.clone(),
                        payload.as_bytes(),
                    )?
                };
                probe.store(false, Ordering::Relaxed);
                parse_token(&bytes, false, true)?
            }
            Kind::Chain(_) => unreachable!(),
        };
        let value = token.value.clone();
        *cache = Some(token);
        Ok(value)
    }
}

impl Identity {
    pub fn new(secret: &AzureSecret) -> Result<Self> {
        let required = |key: &str| {
            text(&secret.values, key)?.ok_or_else(|| {
                Error::invalid_input(format!(
                    "Failed to fetch key '{key}' from secret '{}' of type 'azure'",
                    secret.name
                ))
            })
        };
        let kinds = match secret.provider.as_str() {
            "credential_chain" => {
                let chain = text(&secret.values, "chain")?.unwrap_or_else(|| "default".into());
                chain
                    .split(';')
                    .map(|name| source(name, false))
                    .collect::<Result<Vec<_>>>()?
            }
            "service_principal" => {
                let tenant = required("tenant_id")?;
                let client = required("client_id")?;
                let secret_value = text(&secret.values, "client_secret")?.unwrap_or_default();
                let certificate = text(&secret.values, "client_certificate_path")?.unwrap_or_default();
                let proof = if !secret_value.is_empty() {
                    Proof::Secret(secret_value)
                } else if !certificate.is_empty() {
                    Proof::Certificate(Box::new(super::certificate::Certificate::load(&certificate)?))
                } else {
                    return Err(Error::invalid_input(
                        "Failed to fetch key 'client_secret' or 'client_certificate_path' from secret 'service_principal' of type 'azure'",
                    ));
                };
                vec![principal(tenant, client, proof)]
            }
            "managed_identity" => {
                let mut ids = Vec::new();
                for key in ["client_id", "object_id", "resource_id"] {
                    if let Some(value) = text(&secret.values, key)? {
                        ids.push((key, value));
                    }
                }
                if ids.len() > 1 {
                    return Err(Error::invalid_input(
                        "Only 1 of client_id, object_id, or resource_id, may be specified with provider managed_identity",
                    ));
                }
                vec![managed(ids.first().map(|(k, v)| (*k, v.as_str())), false, false)?]
            }
            provider => {
                return Err(Error::invalid_input(format!(
                    "Unsupported provider type {provider} for azure"
                )));
            }
        };
        Ok(Self {
            sources: kinds.into_iter().map(Credential::new).collect(),
            reuse: false,
            chosen: Mutex::new(None),
            name: if secret.provider == "credential_chain" {
                "ChainedTokenCredential"
            } else {
                "DirectCredential"
            },
        })
    }
    pub fn token(&self, parameters: &Parameters) -> Result<String> {
        let mut chosen = self.chosen.lock().unwrap_or_else(PoisonError::into_inner);
        for (index, source) in self.sources.iter().enumerate() {
            if chosen.is_some_and(|selected| selected != index) {
                continue;
            }
            match source.token(parameters) {
                Ok(token) => {
                    if self.reuse {
                        *chosen = Some(index);
                    }
                    return Ok(token);
                }
                Err(error) if error.kind() == roastedduck_common::ErrorKind::Interrupt => return Err(error),
                Err(error) if self.name == "DirectCredential" => {
                    return Err(error);
                }
                Err(_) => {}
            }
        }
        Err(io_error(format!("Failed to get token from {}.", self.name)))
    }
}

fn principal(tenant: String, client: String, proof: Proof) -> Kind {
    let authority = env("AZURE_AUTHORITY_HOST");
    Kind::Principal {
        tenant,
        client,
        proof,
        authority: if authority.is_empty() {
            "https://login.microsoftonline.com".into()
        } else {
            authority
        },
    }
}
fn source(name: &str, probe: bool) -> Result<Kind> {
    Ok(match name {
        "env" => {
            let tenant = env("AZURE_TENANT_ID");
            let client = env("AZURE_CLIENT_ID");
            let secret = env("AZURE_CLIENT_SECRET");
            let certificate = env("AZURE_CLIENT_CERTIFICATE_PATH");
            if tenant.is_empty() || client.is_empty() || secret.is_empty() && certificate.is_empty() {
                Kind::Unavailable("EnvironmentCredential authentication unavailable. Environment variables are not fully configured.".into())
            } else if !secret.is_empty() {
                principal(tenant, client, Proof::Secret(secret))
            } else {
                principal(
                    tenant,
                    client,
                    Proof::Certificate(Box::new(super::certificate::Certificate::load(&certificate)?)),
                )
            }
        }
        "workload_identity" => {
            let tenant = env("AZURE_TENANT_ID");
            let client = env("AZURE_CLIENT_ID");
            let path = env("AZURE_FEDERATED_TOKEN_FILE");
            if tenant.is_empty() || client.is_empty() || path.is_empty() {
                Kind::Unavailable("WorkloadIdentityCredential authentication unavailable. Azure Kubernetes environment is not set up correctly.".into())
            } else {
                principal(tenant, client, Proof::Workload(path))
            }
        }
        "managed_identity" => managed(None, probe, true)?,
        "cli" => Kind::Cli,
        "default" => {
            let choice = env("AZURE_TOKEN_CREDENTIALS");
            let choice = choice.trim().to_ascii_lowercase();
            let names: &[&str] = match choice.as_str() {
                "" | "dev" => &["env", "workload_identity", "managed_identity", "cli"],
                "prod" => &["env", "workload_identity", "managed_identity"],
                "environmentcredential" => &["env"],
                "workloadidentitycredential" => &["workload_identity"],
                "managedidentitycredential" => &["managed_identity"],
                "azureclicredential" => &["cli"],
                _ => {
                    return Err(io_error(format!(
                        "DefaultAzureCredential: Invalid value '{}' for the 'AZURE_TOKEN_CREDENTIALS' environment variable.",
                        env("AZURE_TOKEN_CREDENTIALS")
                    )));
                }
            };
            let kinds = names
                .iter()
                .map(|name| source(name, names.len() > 1))
                .collect::<Result<Vec<_>>>()?;
            Kind::Chain(Identity {
                sources: kinds.into_iter().map(Credential::new).collect(),
                reuse: true,
                chosen: Mutex::new(None),
                name: "DefaultAzureCredential",
            })
        }
        _ => {
            return Err(Error::invalid_input(format!(
                "Unknown credential provider found: {name}"
            )));
        }
    })
}

fn add_query(url: &mut String, key: &str, value: &str) {
    url.push(if url.contains('?') { '&' } else { '?' });
    url.push_str(key);
    url.push('=');
    url.push_str(&encode(value, false));
}

fn managed(id: Option<(&str, &str)>, probe: bool, inherit_transport: bool) -> Result<Kind> {
    let identity_endpoint = env("IDENTITY_ENDPOINT");
    let identity_header = env("IDENTITY_HEADER");
    let msi_endpoint = env("MSI_ENDPOINT");
    let msi_secret = env("MSI_SECRET");
    let (mut url, mut headers, version, post, arc, imds) =
        if !identity_endpoint.is_empty() && !identity_header.is_empty() {
            (
                identity_endpoint,
                vec![("X-IDENTITY-HEADER".into(), identity_header)],
                "2019-08-01",
                false,
                false,
                false,
            )
        } else if !msi_endpoint.is_empty() && !msi_secret.is_empty() {
            (
                msi_endpoint,
                vec![("secret".into(), msi_secret)],
                "2017-09-01",
                false,
                false,
                false,
            )
        } else if !msi_endpoint.is_empty() {
            (msi_endpoint, Vec::new(), "", true, false, false)
        } else if !identity_endpoint.is_empty() && !env("IMDS_ENDPOINT").is_empty() {
            (identity_endpoint, Vec::new(), "2019-11-01", false, true, false)
        } else {
            let host = env("AZURE_POD_IDENTITY_AUTHORITY_HOST");
            (
                format!(
                    "{}/metadata/identity/oauth2/token",
                    if host.is_empty() {
                        "http://169.254.169.254"
                    } else {
                        host.trim_end_matches('/')
                    }
                ),
                Vec::new(),
                "2018-02-01",
                false,
                false,
                true,
            )
        };
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(io_error("Managed identity endpoint must be an HTTP(S) URL"));
    }
    if !version.is_empty() {
        add_query(&mut url, "api-version", version);
    }
    if post || arc || imds {
        headers.push(("Metadata".into(), "true".into()));
    }
    if let Some((key, value)) = id.filter(|(_, v)| !v.is_empty()) {
        if post || arc {
            return Err(io_error(
                "User assigned identity is not supported by this managed identity endpoint",
            ));
        }
        let key = match (key, imds, version) {
            ("client_id", false, "2017-09-01") => "clientid",
            ("client_id", _, _) => "client_id",
            ("object_id", true, _) => "object_id",
            ("object_id", false, _) => "principal_id",
            (_, true, _) => "msi_res_id",
            _ => "mi_res_id",
        };
        add_query(&mut url, key, value);
    }
    Ok(Kind::Managed {
        url,
        headers,
        post,
        arc,
        probe: AtomicBool::new(probe && imds),
        inherit_transport,
    })
}

fn request(
    parameters: &Parameters,
    method: &str,
    url: &str,
    headers: Vec<(String, String)>,
    payload: &[u8],
) -> Result<Vec<u8>> {
    response_bytes(send(parameters, method, url, headers, payload)?)
}

fn send(
    parameters: &Parameters,
    method: &str,
    url: &str,
    headers: Vec<(String, String)>,
    payload: &[u8],
) -> Result<ureq::http::Response<crate::body::Body>> {
    check_interrupt()?;
    let mut parameters = parameters.clone();
    parameters.headers = headers;
    if method == "POST" {
        parameters
            .headers
            .push(("Content-Type".into(), "application/x-www-form-urlencoded".into()));
    }
    let mut request = HttpFile::new_request(url.into(), parameters, None)?;
    request.logger = None;
    request.azure_stats = Some(false);
    request.request_payload(method, None, payload, true)
}

fn response_bytes(mut response: ureq::http::Response<crate::body::Body>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    check_interrupt()?;
    if !response.status().is_success() {
        return Err(io_error(format!(
            "Azure token request failed: HTTP {}",
            response.status().as_u16()
        )));
    }
    Ok(bytes)
}

fn arc_request(
    parameters: &Parameters,
    endpoint: &str,
    mut headers: Vec<(String, String)>,
) -> Result<Vec<u8>> {
    let response = send(parameters, "GET", endpoint, headers.clone(), &[])?;
    if response.status().as_u16() != 401 {
        return response_bytes(response);
    }
    let challenge = response.headers().get("www-authenticate").and_then(|v| v.to_str().ok())
        .ok_or_else(|| io_error("Did not receive expected 'WWW-Authenticate' header in the response from Azure Arc Managed Identity Endpoint."))?;
    let (_, path) = challenge.split_once('=').filter(|(_,p)| !p.contains('='))
        .ok_or_else(|| io_error("The 'WWW-Authenticate' header in the response from Azure Arc Managed Identity Endpoint did not match the expected format."))?;
    let expected: String = if cfg!(target_os = "linux") {
        "/var/opt/azcmagent/tokens".into()
    } else if cfg!(windows) {
        let data = env("ProgramData");
        if data.is_empty() {
            return Err(io_error("Unable to get ProgramData folder path."));
        }
        format!("{data}\\AzureConnectedMachineAgent\\Tokens")
    } else {
        return Err(io_error("Unsupported OS. Arc supports only Linux and Windows."));
    };
    let file = std::path::Path::new(path);
    if file.parent() != Some(std::path::Path::new(&expected)) || file.extension().is_none_or(|e| e != "key") {
        return Err(io_error(
            "The file specified in the 'WWW-Authenticate' header in the response from Azure Arc Managed Identity Endpoint has an unexpected file path.",
        ));
    }
    if std::fs::metadata(file).map_err(io_error)?.len() > 4096 {
        return Err(io_error(
            "The file specified in the 'WWW-Authenticate' header in the response from Azure Arc Managed Identity Endpoint is larger than 4096 bytes.",
        ));
    }
    headers.push((
        "Authorization".into(),
        format!("Basic {}", std::fs::read_to_string(file).map_err(io_error)?),
    ));
    request(parameters, "GET", endpoint, headers, &[])
}

fn parse_token(bytes: &[u8], cli: bool, proactive: bool) -> Result<Token> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(io_error)?;
    let key = if cli { "accessToken" } else { "access_token" };
    let token = value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            io_error(format!(
                "Token JSON object: can't find or parse '{key}' property."
            ))
        })?;
    let number = |v: &serde_json::Value| v.as_u64().or_else(|| v.as_str()?.parse().ok());
    let mut explicit_refresh = false;
    let expires = if let Some(seconds) = value
        .get("refresh_in")
        .and_then(serde_json::Value::as_u64)
        .filter(|s| i32::try_from(*s).is_ok())
    {
        explicit_refresh = true;
        now().saturating_add(seconds)
    } else if let Some(seconds) = value
        .get(if cli { "expiresIn" } else { "expires_in" })
        .and_then(number)
        .filter(|s| i32::try_from(*s).is_ok())
    {
        now().saturating_add(seconds)
    } else {
        let mut found = None;
        for key in if cli {
            ["expires_on", "expiresOn"]
        } else {
            ["expires_on", "expires_on"]
        } {
            if let Some(value) = value.get(key) {
                found = number(value).or_else(|| {
                    let text = value.as_str()?;
                    httpdate::parse_http_date(text)
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .or_else(|| {
                            let offset = if cli { cli_time_offset() } else { 0 };
                            let text = if offset == 0 {
                                text.to_owned()
                            } else {
                                format!(
                                    "{text}{}{:02}:{:02}",
                                    if offset < 0 { '-' } else { '+' },
                                    offset.abs() / 3600,
                                    (offset.abs() % 3600) / 60
                                )
                            };
                            Timestamp::parse(text.as_bytes(), true)
                                .ok()
                                .and_then(|(t, _)| u64::try_from(t.0 / 1_000_000).ok())
                        })
                });
                if found.is_some() {
                    break;
                }
            }
        }
        found.ok_or_else(|| io_error("Token JSON object: can't find or parse expiration property."))?
    };
    let expires = if proactive
        && !explicit_refresh
        && expires.saturating_sub(now()) >= Duration::from_secs(7200).as_secs()
    {
        now() + (expires - now()) / 2
    } else {
        expires
    };
    Ok(Token {
        value: token.into(),
        expires,
    })
}

fn cli_time_offset() -> i64 {
    #[cfg(any(unix, windows))]
    {
        let time = now() as libc::time_t;
        let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
        let mut utc = std::mem::MaybeUninit::<libc::tm>::uninit();
        // SAFETY: the OS writes complete tm values into valid allocated outputs;
        // mktime reads them only after both conversion calls succeed.
        unsafe {
            #[cfg(unix)]
            if libc::localtime_r(&raw const time, local.as_mut_ptr()).is_null()
                || libc::gmtime_r(&raw const time, utc.as_mut_ptr()).is_null()
            {
                return 0;
            }
            #[cfg(windows)]
            if libc::localtime_s(local.as_mut_ptr(), &raw const time) != 0
                || libc::gmtime_s(utc.as_mut_ptr(), &raw const time) != 0
            {
                return 0;
            }
            libc::difftime(libc::mktime(local.as_mut_ptr()), libc::mktime(utc.as_mut_ptr())) as i64
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        0
    }
}
