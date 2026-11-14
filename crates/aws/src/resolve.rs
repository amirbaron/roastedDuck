use std::borrow::Cow;
use std::fmt::{self, Write as _};
use std::time::Duration;

use aws_config::provider_config::ProviderConfig;
use aws_credential_types::provider::{ProvideCredentials, SharedCredentialsProvider};
use aws_runtime::env_config::file::{EnvConfigFileKind, EnvConfigFiles};
use aws_runtime::env_config::section::{EnvConfigSections, Profile};
use aws_types::os_shim_internal::{Env, Fs};
use aws_types::region::Region;

use crate::{Credentials, Provider};

/// SQL's explicit credential selection. Region includes the engine setting
/// override; environment/profile fallbacks are resolved below.
#[derive(Clone, Debug)]
pub struct Request {
    pub profile: Option<String>,
    pub chain: String,
    pub assume_role: String,
    pub external_id: String,
    pub web_identity_token_file: String,
    pub session_name: String,
    pub region: String,
    pub require_credentials: bool,
    /// The legacy table function uses the SDK lookup without secret validation/fallback.
    pub legacy: bool,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            profile: None,
            chain: String::new(),
            assume_role: String::new(),
            external_id: String::new(),
            web_identity_token_file: String::new(),
            session_name: String::new(),
            region: String::new(),
            require_credentials: true,
            legacy: false,
        }
    }
}

#[derive(Debug)]
pub struct Resolution {
    pub credentials: Option<Credentials>,
    pub region: String,
    pub effective_chain: String,
}

#[derive(Debug)]
pub enum ResolveError {
    InvalidInput(String),
    Configuration(String),
    Runtime(String),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) | Self::Configuration(message) | Self::Runtime(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for ResolveError {}

struct Context {
    providers: ProviderConfig,
    sdk: aws_types::SdkConfig,
    profiles: EnvConfigSections,
    config_profiles: EnvConfigSections,
    profile: String,
    default_profile: String,
}

type Material = Option<aws_credential_types::Credentials>;

/// Fetch credentials without exposing SDK errors that may contain secret material.
/// The SQL adapter supplies the pinned extension's validation diagnostic on None.
pub fn resolve(request: &Request) -> Result<Resolution, ResolveError> {
    if !request.assume_role.is_empty() && request.chain.is_empty() {
        return Err(ResolveError::Configuration(
            "Must pass CHAIN value when passing ASSUME_ROLE_ARN".into(),
        ));
    }
    let chain = if request.chain.is_empty() {
        Vec::new()
    } else {
        request.chain.split(';').collect::<Vec<_>>()
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| ResolveError::Runtime("Unable to start AWS credential runtime".into()))?;
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(60), resolve_async(request, &chain))
            .await
            .map_err(|_| ResolveError::Runtime("AWS credential resolution timed out".into()))?
    })
}

async fn read_profiles(files: &EnvConfigFiles, profile: Option<&str>) -> EnvConfigSections {
    aws_config::profile::load(
        &Fs::real(),
        &Env::real(),
        files,
        profile
            .filter(|name| !name.is_empty())
            .map(|name| Cow::Owned(name.to_owned())),
    )
    .await
    .unwrap_or_default()
}

async fn resolve_async(request: &Request, chain: &[&str]) -> Result<Resolution, ResolveError> {
    let profiles = read_profiles(&EnvConfigFiles::default(), request.profile.as_deref()).await;
    let config_profiles = read_profiles(
        &EnvConfigFiles::builder()
            .include_default_config_file(true)
            .build(),
        request.profile.as_deref(),
    )
    .await;
    let profile = profiles.selected_profile().to_owned();
    let mut region = request.region.clone();
    if region.is_empty() {
        region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_default();
    }
    if region.is_empty() {
        let name = request
            .profile
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or("default");
        region = config_profiles
            .get_profile(name)
            .and_then(|p| p.get("region"))
            .unwrap_or_default()
            .into();
    }
    if request.legacy && region.is_empty() {
        region = "us-east-1".into();
    }
    let http = aws_smithy_http_client::Builder::new()
        .tls_provider(aws_smithy_http_client::tls::Provider::Rustls(
            aws_smithy_http_client::tls::rustls_provider::CryptoMode::Ring,
        ))
        .build_https();
    let sdk_region = Region::new(if region.is_empty() {
        "us-east-1".to_owned()
    } else {
        region.clone()
    });
    let providers = ProviderConfig::without_region()
        .with_http_client(http.clone())
        .with_region(Some(sdk_region.clone()));
    let sdk = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(sdk_region)
        .http_client(http)
        .no_credentials()
        .load()
        .await;
    let context = Context {
        providers,
        sdk,
        profiles,
        config_profiles,
        profile,
        default_profile: std::env::var("AWS_PROFILE")
            .ok()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "default".into()),
    };
    // Construct/validate each component in order before fetching credentials.
    // For example, a missing config profile before an unknown provider reports
    // the profile error, even if an earlier environment provider would succeed.
    let mut providers = Vec::new();
    for name in chain {
        let provider = Provider::parse(name).map_err(ResolveError::InvalidInput)?;
        if provider == Provider::Config {
            context.config_role(request)?;
        } else if provider == Provider::Sts && request.assume_role.is_empty() {
            return Err(ResolveError::Configuration(
                "Chain value 'STS' is only supported with an ASSUME_ROLE_ARN value. If the selected profile uses STS, add \"CHAIN 'config'\"".into(),
            ));
        }
        providers.push(provider);
    }
    let mut effective_chain = request.chain.clone();
    let credentials = if chain.is_empty() {
        let result = if request
            .profile
            .as_ref()
            .is_some_and(|p| !request.legacy || !p.is_empty())
        {
            context.static_profile(&context.profile)
        } else {
            context.default_chain().await
        };
        // The pinned extension retries the config chain whenever the default
        // or explicitly named static profile returned no credentials. This also
        // changes the chain name in validation errors, even without a role.
        if result.is_none() && !request.legacy {
            effective_chain = "config".into();
            context.config(request).await?
        } else {
            result
        }
    } else {
        let mut result = None;
        for provider in &providers {
            result = match provider {
                Provider::Config => context.config(request).await?,
                Provider::Sts => {
                    context
                        .assume_role(&request.assume_role, &request.external_id)
                        .await
                }
                _ => context.leaf(*provider, request, &context.profile).await,
            };
            if result.is_some() {
                break;
            }
        }
        result
    };
    Ok(Resolution {
        credentials: credentials.map(Into::into),
        region,
        effective_chain,
    })
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

impl Context {
    fn static_profile(&self, name: &str) -> Material {
        let profile = self.profiles.get_profile(name)?;
        Some(aws_credential_types::Credentials::new(
            nonempty(profile.get("aws_access_key_id"))?,
            nonempty(profile.get("aws_secret_access_key"))?,
            nonempty(profile.get("aws_session_token")).map(str::to_owned),
            None,
            "roastedDuck-profile",
        ))
    }

    async fn default_chain(&self) -> Material {
        let request = Request::default();
        for provider in [
            Provider::Environment,
            Provider::Config,
            Provider::Process,
            Provider::WebIdentity,
            Provider::Sso,
        ] {
            let result = if provider == Provider::Config {
                self.static_profile(&self.default_profile)
            } else {
                self.leaf(provider, &request, &self.default_profile).await
            };
            if result.is_some() {
                return result;
            }
        }
        if std::env::var("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI").is_ok()
            || std::env::var("AWS_CONTAINER_CREDENTIALS_FULL_URI").is_ok()
        {
            aws_config::ecs::EcsCredentialsProvider::builder()
                .configure(&self.providers)
                .build()
                .provide_credentials()
                .await
                .ok()
        } else if !std::env::var("AWS_EC2_METADATA_DISABLED")
            .is_ok_and(|value| value.eq_ignore_ascii_case("true"))
        {
            self.leaf(Provider::Instance, &request, &self.default_profile)
                .await
        } else {
            None
        }
    }

    async fn leaf(&self, provider: Provider, request: &Request, profile: &str) -> Material {
        match provider {
            Provider::Environment => {
                aws_config::environment::credentials::EnvironmentVariableCredentialsProvider::new()
                    .provide_credentials()
                    .await
                    .ok()
            }
            Provider::Process => {
                let command = nonempty(self.profiles.get_profile(profile)?.get("credential_process"))?;
                aws_config::credential_process::CredentialProcessProvider::new(command.to_owned())
                    .provide_credentials()
                    .await
                    .ok()
            }
            Provider::Instance => aws_config::imds::credentials::ImdsCredentialsProvider::builder()
                .configure(&self.providers)
                .build()
                .provide_credentials()
                .await
                .ok(),
            Provider::WebIdentity => {
                let mut builder =
                    aws_config::web_identity_token::WebIdentityTokenCredentialsProvider::builder()
                        .configure(&self.providers);
                if !request.assume_role.is_empty()
                    || !request.web_identity_token_file.is_empty()
                    || !request.session_name.is_empty()
                {
                    builder =
                        builder.static_configuration(aws_config::web_identity_token::StaticConfiguration {
                            role_arn: if request.assume_role.is_empty() {
                                std::env::var("AWS_ROLE_ARN").ok()?
                            } else {
                                request.assume_role.clone()
                            },
                            web_identity_token_file: if request.web_identity_token_file.is_empty() {
                                std::env::var("AWS_WEB_IDENTITY_TOKEN_FILE").ok()?.into()
                            } else {
                                request.web_identity_token_file.clone().into()
                            },
                            session_name: if request.session_name.is_empty() {
                                std::env::var("AWS_ROLE_SESSION_NAME")
                                    .unwrap_or_else(|_| "roastedduck-web-identity".into())
                            } else {
                                request.session_name.clone()
                            },
                        });
                }
                builder.build().provide_credentials().await.ok()
            }
            Provider::Sso => self.sso(profile).await,
            // Role-aware config and STS are handled by the outer dispatcher.
            Provider::Config | Provider::Sts => None,
        }
    }

    fn config_role(&self, request: &Request) -> Result<Option<(String, String)>, ResolveError> {
        let Some(name) = request.profile.as_deref().filter(|name| !name.is_empty()) else {
            return Ok(None);
        };
        let profile = self.config_profiles.get_profile(name);
        if profile.is_none() && request.require_credentials {
            let path = std::env::var("AWS_CONFIG_FILE")
                .unwrap_or_else(|_| format!("{}/.aws/config", std::env::var("HOME").unwrap_or_default()));
            return Err(ResolveError::Configuration(format!(
                "Secret Validation Failure: no profile '{name}' found in config file {path}"
            )));
        }
        let field = |key| profile.and_then(|profile| profile.get(key)).unwrap_or_default();
        let profile_role = field("role_arn");
        let profile_external = field("external_id");
        if !profile_role.is_empty() && !request.assume_role.is_empty() {
            return Err(ResolveError::InvalidInput(format!(
                "Ambiguous role arn. Role_arn '{profile_role}' defined in profile '{name}'. Role_arn '{}' defined in secret statement",
                request.assume_role
            )));
        }
        if !profile_external.is_empty() && !request.external_id.is_empty() {
            return Err(ResolveError::InvalidInput(format!(
                "Ambiguous external id. external_id '{profile_external}' defined in profile '{name}'. external_id '{}' defined in secret statement",
                request.external_id
            )));
        }
        let role = if profile_role.is_empty() {
            &request.assume_role
        } else {
            profile_role
        };
        let external = if profile_external.is_empty() {
            &request.external_id
        } else {
            profile_external
        };
        Ok((!role.is_empty()).then(|| (role.to_owned(), external.to_owned())))
    }

    async fn config(&self, request: &Request) -> Result<Material, ResolveError> {
        Ok(match self.config_role(request)? {
            Some((role, external)) => self.assume_role(&role, &external).await,
            None => self.static_profile(
                request
                    .profile
                    .as_deref()
                    .filter(|p| !p.is_empty())
                    .unwrap_or(&self.profile),
            ),
        })
    }

    async fn assume_role(&self, role: &str, external: &str) -> Material {
        let credentials = self.default_chain().await?;
        let sdk = self
            .sdk
            .clone()
            .into_builder()
            .credentials_provider(SharedCredentialsProvider::new(credentials))
            .build();
        let mut builder = aws_config::sts::AssumeRoleProvider::builder(role).configure(&sdk);
        if !external.is_empty() {
            builder = builder.external_id(external);
        }
        builder.build().await.provide_credentials().await.ok()
    }

    async fn sso(&self, profile: &str) -> Material {
        let profile = self.profiles.get_profile(profile)?;
        // Feed only SSO fields to the SDK. A static key or role in this profile
        // must not replace the explicitly selected SSO provider.
        if profile.get("sso_session").is_none() && profile.get("sso_start_url").is_none() {
            return None;
        }
        let mut contents = "[profile roastedduck-internal-sso]\n".to_owned();
        append_profile_fields(
            &mut contents,
            profile,
            &[
                "sso_account_id",
                "sso_role_name",
                "sso_region",
                "sso_start_url",
                "sso_session",
            ],
        )?;
        if let Some(name) = profile.get("sso_session") {
            if name.contains(['\r', '\n', '[', ']']) {
                return None;
            }
            let session = self.profiles.sso_session(name)?;
            let _ = writeln!(contents, "\n[sso-session {name}]");
            for key in ["sso_region", "sso_start_url", "sso_registration_scopes"] {
                if let Some(value) = session.get(key) {
                    if value.contains(['\r', '\n']) {
                        return None;
                    }
                    let _ = writeln!(contents, "{key}={value}");
                }
            }
        }
        let files = EnvConfigFiles::builder()
            .with_contents(EnvConfigFileKind::Config, contents)
            .build();
        aws_config::profile::ProfileFileCredentialsProvider::builder()
            .configure(&self.providers)
            .profile_name("roastedduck-internal-sso")
            .profile_files(files)
            .build()
            .provide_credentials()
            .await
            .ok()
    }
}

fn append_profile_fields(output: &mut String, profile: &Profile, keys: &[&str]) -> Option<()> {
    use std::fmt::Write as _;
    for key in keys {
        if let Some(value) = profile.get(key) {
            if value.contains(['\r', '\n']) {
                return None;
            }
            let _ = writeln!(output, "{key}={value}");
        }
    }
    Some(())
}
