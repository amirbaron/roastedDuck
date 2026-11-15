//! Pinned Azure secret-provider parameter binding. Authentication is deferred to I/O.
use super::{
    Arc, BTreeMap, BTreeSet, CreateSecretInput, Error, KeyValueSecret, LogicalType, Result, SecretManager,
    ValueCast, provider_not_found,
};

pub(super) fn create(input: &CreateSecretInput) -> Result<KeyValueSecret> {
    let provider = if input.provider.is_empty() {
        "config"
    } else {
        &input.provider
    };
    let specific: &[&str] = match provider {
        "config" => &["connection_string"],
        "credential_chain" => &["chain"],
        "managed_identity" => &["client_id", "object_id", "resource_id"],
        "service_principal" => &[
            "tenant_id",
            "client_id",
            "client_secret",
            "client_certificate_path",
        ],
        "access_token" => &["access_token"],
        _ => return Err(provider_not_found("azure", provider, input.provider.is_empty())),
    };
    let mut map = BTreeMap::new();
    for (key, value) in &input.options {
        let key = String::from_utf8_lossy(key.to_ascii_lowercase().as_bytes()).into_owned();
        if ![
            "account_name",
            "endpoint",
            "http_proxy",
            "proxy_user_name",
            "proxy_password",
        ]
        .contains(&key.as_str())
            && !specific.contains(&key.as_str())
        {
            let default = if input.provider.is_empty() { "default " } else { "" };
            return Err(Error::binder(format!(
                "Unknown parameter '{key}' for secret type 'azure' with {default}provider '{provider}'"
            )));
        }
        let value = value.default_cast_as(&LogicalType::VARCHAR).map_err(|error| {
            Error::binder(format!(
                "Failed to cast option '{key}' to type 'VARCHAR': '{}'",
                error.raw_message()
            ))
        })?;
        map.insert(key, value);
    }
    let mut redact_keys = BTreeSet::from(["proxy_password".into()]);
    for key in [
        "connection_string",
        "client_secret",
        "client_certificate_path",
        "access_token",
    ] {
        if specific.contains(&key) {
            redact_keys.insert(key.into());
        }
    }
    Ok(KeyValueSecret {
        name: input.name.clone(),
        secret_type: "azure".into(),
        provider: provider.into(),
        map,
        redact_keys,
        scope: if input.scope.is_empty() {
            ["azure://", "az://", "abfss://", "abfs://"]
                .map(str::to_owned)
                .to_vec()
        } else {
            input.scope.clone()
        },
    })
}

pub(crate) struct AzureSecretLookup(pub Arc<SecretManager>);
impl std::fmt::Debug for AzureSecretLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AzureSecretLookup")
    }
}
impl roastedduck_httpfs::azure::ResolveAzureSecret for AzureSecretLookup {
    fn user_agent(&self) -> String {
        super::http_user_agent()
    }
    fn resolve(&self, path: &str) -> Result<Option<roastedduck_httpfs::azure::AzureSecret>> {
        self.0
            .lookup_secret(path, "azure")?
            .map(|entry| {
                Ok(roastedduck_httpfs::azure::AzureSecret {
                    name: String::from_utf8_lossy(entry.secret.name.as_bytes()).into_owned(),
                    provider: entry.secret.provider,
                    values: entry.secret.map,
                })
            })
            .transpose()
    }
}
