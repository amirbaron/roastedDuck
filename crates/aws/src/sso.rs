//! SSO cache, refresh and writeback semantics of the pinned C++ SDK.
use std::path::PathBuf;
use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_runtime::env_config::section::Profile;
use aws_smithy_types::{DateTime, date_time::Format};
use aws_types::region::Region;
use sha1::{Digest, Sha1};

fn cache_path(identifier: &str) -> Option<PathBuf> {
    let credentials_file = std::env::var("AWS_SHARED_CREDENTIALS_FILE")
        .ok()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            aws_runtime::fs_util::home_dir(
                &aws_types::os_shim_internal::Env::real(),
                aws_runtime::fs_util::Os::real(),
            )
            .map(|h| PathBuf::from(h).join(".aws/credentials"))
        })?;
    // The pinned SDK derives its SSO cache from the credentials file directory,
    // independently of AWS_CONFIG_FILE and the Rust SDK's home cache default.
    let parent = credentials_file.parent()?;
    let parent = if parent.as_os_str().is_empty() {
        std::path::Path::new(std::path::MAIN_SEPARATOR_STR)
    } else {
        parent
    };
    Some(
        parent
            .join("sso/cache")
            .join(format!("{:x}.json", Sha1::digest(identifier.as_bytes()))),
    )
}

fn field<'a>(token: &'a serde_json::Value, name: &str) -> &'a str {
    token
        .get(name)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}
fn date(token: &serde_json::Value, name: &str) -> DateTime {
    DateTime::from_str(field(token, name), Format::DateTimeWithOffset).unwrap_or(DateTime::from_secs(0))
}

pub(crate) async fn legacy(profile: &Profile, sdk: &aws_types::SdkConfig) -> Option<Credentials> {
    let path = cache_path(profile.get("sso_start_url").unwrap_or_default())?;
    let token: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let access = token.get("accessToken")?.as_str()?;
    if access.is_empty() {
        return None;
    }
    let expiry = SystemTime::try_from(
        DateTime::from_str(token.get("expiresAt")?.as_str()?, Format::DateTimeWithOffset).ok()?,
    )
    .ok()?;
    if expiry < SystemTime::now() {
        return None;
    }
    role(
        profile,
        profile.get("sso_region").unwrap_or_default(),
        access,
        sdk,
    )
    .await
}

async fn role(
    profile: &Profile,
    region: &str,
    access: &str,
    sdk: &aws_types::SdkConfig,
) -> Option<Credentials> {
    let configuration = aws_sdk_sso::config::Builder::from(sdk)
        .region(Region::new(region.to_owned()))
        .build();
    let response = aws_sdk_sso::Client::from_conf(configuration)
        .get_role_credentials()
        .account_id(profile.get("sso_account_id").unwrap_or_default())
        .role_name(profile.get("sso_role_name").unwrap_or_default())
        .access_token(access)
        .send()
        .await
        .ok()?;
    let credentials = response.role_credentials()?;
    Some(Credentials::new(
        credentials.access_key_id().unwrap_or_default(),
        credentials.secret_access_key().unwrap_or_default(),
        credentials.session_token().map(str::to_owned),
        Some(SystemTime::try_from(DateTime::from_millis(credentials.expiration())).ok()?),
        "PinnedSsoLegacy",
    ))
}

pub(crate) async fn session(
    profile: &Profile,
    name: &str,
    region: &str,
    sdk: &aws_types::SdkConfig,
) -> Option<Credentials> {
    let path = cache_path(name)?;
    let mut token: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    if field(&token, "accessToken").is_empty() {
        return None;
    }
    let expiry = date(&token, "expiresAt");
    let now = DateTime::from(SystemTime::now());
    if expiry.to_millis().ok()? <= now.to_millis().ok()?.saturating_add(600_000) {
        // The pinned bearer provider rereads the cache before refreshing. It
        // does not gate refresh on registration expiry or metadata completeness.
        let reread = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(serde_json::Value::Null);
        let mut updated = reread;
        let configuration = aws_sdk_ssooidc::config::Builder::from(sdk)
            .region(Region::new(field(&updated, "region").to_owned()))
            .build();
        let result = aws_sdk_ssooidc::Client::from_conf(configuration)
            .create_token()
            .client_id(field(&updated, "clientId"))
            .client_secret(field(&updated, "clientSecret"))
            .refresh_token(field(&updated, "refreshToken"))
            .grant_type("refresh_token")
            .customize()
            .mutate_request(|request| {
                // The pinned C++ SDK omits empty request fields.
                // SSO OIDC's generated Rust builder requires their presence, so
                // preserve its transport while matching the pinned wire object.
                if let Some(body) = request.body().bytes()
                    && let Ok(serde_json::Value::Object(mut fields)) = serde_json::from_slice(body)
                {
                    fields.retain(|_, value| value.as_str() != Some(""));
                    if let Ok(body) = serde_json::to_vec(&fields) {
                        request
                            .headers_mut()
                            .insert("content-length", body.len().to_string());
                        *request.body_mut() = aws_smithy_types::body::SdkBody::from(body);
                    }
                }
            })
            .send()
            .await;
        if let Ok(result) = result
            && let Some(access) = result.access_token().filter(|s| !s.is_empty())
        {
            updated["accessToken"] = access.into();
            let expiry = DateTime::from(SystemTime::now())
                .to_millis()
                .ok()?
                .saturating_add(i64::from(result.expires_in()) * 1000);
            updated["expiresAt"] = DateTime::from_millis(expiry).fmt(Format::DateTime).ok()?.into();
            if let Some(refresh) = result.refresh_token().filter(|s| !s.is_empty()) {
                updated["refreshToken"] = refresh.into();
            }
        }
        // Failed refreshes also rewrite known fields. A write failure leaves
        // the old in-memory token in use, even if the server issued a new one.
        if write_cache(&path, &updated) {
            token = updated;
        }
    }
    if field(&token, "accessToken").is_empty()
        || date(&token, "expiresAt") <= DateTime::from(SystemTime::now())
    {
        return None;
    }
    role(profile, region, field(&token, "accessToken"), sdk).await
}

fn write_cache(path: &std::path::Path, token: &serde_json::Value) -> bool {
    let mut output = serde_json::Map::new();
    for name in [
        "accessToken",
        "refreshToken",
        "clientId",
        "clientSecret",
        "region",
        "startUrl",
    ] {
        let value = field(token, name);
        if !value.is_empty() {
            output.insert(name.into(), value.into());
        }
    }
    for name in ["expiresAt", "registrationExpiresAt"] {
        let value = date(token, name);
        if value != DateTime::from_secs(0)
            && let Ok(value) = DateTime::from_secs(value.secs()).fmt(Format::DateTime)
        {
            output.insert(name.into(), value.into());
        }
    }
    serde_json::to_vec_pretty(&output).is_ok_and(|bytes| std::fs::write(path, bytes).is_ok())
}
