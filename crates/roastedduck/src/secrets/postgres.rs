//! PostgreSQL config-secret binding and ordered connection string construction.
use super::{
    BTreeMap, BTreeSet, CreateSecretInput, Error, KeyValueSecret, LogicalType, Result, Value, ValueCast,
    provider_not_found,
};

pub(crate) const CONNECTION_OPTIONS: &[&str] = &[
    "host",
    "hostaddr",
    "port",
    "dbname",
    "user",
    "password",
    "passfile",
    "require_auth",
    "channel_binding",
    "connect_timeout",
    "client_encoding",
    "options",
    "application_name",
    "fallback_application_name",
    "keepalives",
    "keepalives_idle",
    "keepalives_interval",
    "keepalives_count",
    "tcp_user_timeout",
    "replication",
    "gssencmode",
    "sslmode",
    "requiressl",
    "sslnegotiation",
    "sslcompression",
    "sslcert",
    "sslkey",
    "sslkeylogfile",
    "sslpassword",
    "sslcertmode",
    "sslrootcert",
    "sslcrl",
    "sslcrldir",
    "sslsni",
    "requirepeer",
    "ssl_min_protocol_version",
    "ssl_max_protocol_version",
    "min_protocol_version",
    "max_protocol_version",
    "krbsrvname",
    "gsslib",
    "gssdelegation",
    "scram_client_key",
    "scram_server_key",
    "service",
    "target_session_attrs",
    "load_balance_hosts",
    "oauth_issuer",
    "oauth_client_id",
    "oauth_client_secret",
    "oauth_scope",
];
pub(super) fn create(input: &CreateSecretInput) -> Result<KeyValueSecret> {
    let provider = if input.provider.is_empty() {
        "config"
    } else {
        &input.provider
    };
    if provider != "config" {
        return Err(provider_not_found(
            "postgres",
            provider,
            input.provider.is_empty(),
        ));
    }
    let mut map = BTreeMap::new();
    // CREATE statement copying and value binding rebuild the parameter map
    // before provider binding. Preserve the pinned platform's resulting order.
    let bound = roastedduck_function::named_parameters_in_map_order(&input.options)
        .cloned()
        .collect::<Vec<_>>();
    for (name, value) in roastedduck_function::named_parameters_in_map_order(&bound) {
        let lower = String::from_utf8_lossy(name.to_ascii_lowercase().as_bytes()).into_owned();
        let key = match lower.as_str() {
            "database" => "dbname",
            "hostname" => "host",
            "username" => "user",
            other => other,
        };
        if !CONNECTION_OPTIONS.contains(&key) && !["uri", "aws_rds_secret"].contains(&key) {
            let default = if input.provider.is_empty() { "default " } else { "" };
            return Err(Error::binder(roastedduck_common::byte_format!(
                "Unknown parameter '{name}' for secret type 'postgres' with {default}provider '{provider}'"
            )));
        }
        let value = value.default_cast_as(&LogicalType::VARCHAR).map_err(|error| {
            Error::binder(format!(
                "Failed to cast option '{lower}' to type 'VARCHAR': '{}'",
                error.raw_message()
            ))
        })?;
        map.insert(key.to_owned(), Value::varchar(value.to_varchar()?));
    }
    Ok(KeyValueSecret {
        name: input.name.clone(),
        secret_type: "postgres".into(),
        provider: "config".into(),
        scope: Vec::new(),
        map,
        redact_keys: BTreeSet::from([
            "password".into(),
            "sslpassword".into(),
            "oauth_client_secret".into(),
            "uri".into(),
        ]),
    })
}

pub(crate) fn connection_string(secret: Option<&KeyValueSecret>, attach_path: &str) -> Result<String> {
    let Some(secret) = secret else {
        return Ok(attach_path.to_owned());
    };
    if let Some(uri) = secret.map.get("uri").filter(|v| !v.is_null()) {
        for name in CONNECTION_OPTIONS {
            if secret.map.get(*name).is_some_and(|v| !v.is_null()) {
                return Err(Error::binder(format!(
                    "Options with name \"{name}\" cannot be specified when 'URI' option is specified"
                )));
            }
        }
        if !attach_path.is_empty() {
            return Err(Error::binder(format!(
                "ATTACH path must be empty when 'URI' option is specified, was: \"{attach_path}\""
            )));
        }
        return uri.to_varchar();
    }
    let mut result = String::new();
    for key in CONNECTION_OPTIONS {
        if let Some(value) = secret.map.get(*key).filter(|v| !v.is_null()) {
            result.push_str(key);
            result.push('=');
            result.push_str(&escape(&value.to_varchar()?));
            result.push(' ');
        }
    }
    result.push_str(attach_path);
    Ok(result)
}
pub(crate) fn escape(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}
