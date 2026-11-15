//! Legacy SSO cache location and token semantics of the pinned C++ SDK.
use std::path::PathBuf;
use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_runtime::env_config::section::Profile;
use aws_smithy_types::{DateTime, date_time::Format};
use aws_types::region::Region;
use sha1::{Digest, Sha1};

pub(crate) async fn legacy(profile: &Profile, sdk: &aws_types::SdkConfig) -> Option<Credentials> {
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
    let start = profile.get("sso_start_url").unwrap_or_default();
    let path = parent
        .join("sso/cache")
        .join(format!("{:x}.json", Sha1::digest(start.as_bytes())));
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
    let configuration = aws_sdk_sso::config::Builder::from(sdk)
        .region(Region::new(
            profile.get("sso_region").unwrap_or_default().to_owned(),
        ))
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
