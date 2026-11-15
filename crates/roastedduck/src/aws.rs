//! SQL integration for roastedDuck's compatible AWS implementation.
use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::{Arc, Weak};
use std::time::SystemTime;

use roastedduck_common::{ByteString, Error, ErrorKind, Result, byte_format};
use roastedduck_function::cast::ValueCast;
use roastedduck_function::logging::{LogLevel, LogScope};
use roastedduck_function::{
    FunctionClient, FunctionData, TableFunction, TableFunctionBindOutput, TableFunctionSet,
};
use roastedduck_planner::operator::CreateSecretInput;
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};

use crate::secrets::KeyValueSecret;

fn provider_error(error: roastedduck_aws::ResolveError) -> Error {
    match error {
        roastedduck_aws::ResolveError::Interrupted => Error::new(ErrorKind::Interrupt, "Interrupted!"),
        roastedduck_aws::ResolveError::InvalidInput(message) => Error::new(ErrorKind::InvalidInput, message),
        roastedduck_aws::ResolveError::Configuration(message) => {
            Error::new(ErrorKind::InvalidConfiguration, message)
        }
        roastedduck_aws::ResolveError::Runtime(message) => Error::new(ErrorKind::Io, message),
    }
}

fn current_setting(name: &str) -> Option<Value> {
    roastedduck_function::file_system::opener()?
        .settings()?
        .current_setting(name)
        .ok()
}

fn validation_error(request: &roastedduck_aws::Request, chain: &str) -> Error {
    let verb = if matches!(chain, "sts" | "sso" | "instance" | "process" | "web_identity")
        || !request.assume_role.is_empty()
    {
        "generate"
    } else {
        "create"
    };
    let mut message = format!("Secret Validation Failure: during `{verb}` using the following:\n");
    for (label, value) in [
        ("Profile", request.profile.as_deref().unwrap_or_default()),
        ("Credential Chain", chain),
        ("Role-arn", request.assume_role.as_str()),
        ("External-id", request.external_id.as_str()),
        (
            "Web Identity Token File",
            request.web_identity_token_file.as_str(),
        ),
        ("Session Name", request.session_name.as_str()),
    ] {
        if !value.is_empty() {
            let _ = writeln!(message, "{label}: '{value}'");
        }
    }
    Error::new(ErrorKind::InvalidConfiguration, message)
}

pub(crate) fn create_secret(input: &CreateSecretInput) -> Result<KeyValueSecret> {
    let mut options = BTreeMap::new();
    let mut refresh_fields = Vec::new();
    for (name, value) in &input.options {
        let lower = name.to_ascii_lowercase();
        let key = std::str::from_utf8(lower.as_bytes()).unwrap_or("").to_owned();
        let ty = match key.as_str() {
            "key_id"
            | "secret"
            | "region"
            | "session_token"
            | "endpoint"
            | "url_style"
            | "assume_role_arn"
            | "external_id"
            | "web_identity_token_file"
            | "session_name"
            | "http_proxy"
            | "http_proxy_username"
            | "http_proxy_password"
            | "refresh"
            | "chain"
            | "profile"
            | "validation" => LogicalType::VARCHAR,
            "use_ssl" | "url_compatibility_mode" => LogicalType::BOOLEAN,
            "account_id" if input.secret_type == "r2" => LogicalType::VARCHAR,
            "rds_user" | "rds_host" | "rds_port" | "rds_template_secret_name"
                if input.secret_type == "rds" =>
            {
                LogicalType::VARCHAR
            }
            _ => {
                let default = if input.provider.is_empty() { "default " } else { "" };
                return Err(Error::binder(byte_format!(
                    "Unknown parameter '{name}' for secret type '{}' with {default}provider 'credential_chain'",
                    input.secret_type
                )));
            }
        };
        let value = value.default_cast_as(&ty).map_err(|error| {
            Error::binder(format!(
                "Failed to cast option '{key}' to type '{ty}': '{}'",
                error.raw_message()
            ))
        })?;
        refresh_fields.push((lower, value.clone()));
        options.insert(key, value);
    }
    let text = |key: &str| -> Result<String> {
        Ok(options
            .get(key)
            .map(Value::to_varchar)
            .transpose()?
            .unwrap_or_default())
    };
    let validation = text("validation")?.to_ascii_lowercase();
    if !matches!(validation.as_str(), "" | "exists" | "none") {
        return Err(Error::invalid_input(format!(
            "Unknown AWS validation mode: `{validation}`"
        )));
    }
    let mut region = text("region")?;
    if region.is_empty() {
        region = current_setting("s3_region")
            .filter(|v| !v.is_null())
            .map(|v| v.to_varchar())
            .transpose()?
            .unwrap_or_default();
    }
    let request = roastedduck_aws::Request {
        profile: options.get("profile").map(Value::to_varchar).transpose()?,
        chain: text("chain")?,
        assume_role: text("assume_role_arn")?,
        external_id: text("external_id")?,
        web_identity_token_file: text("web_identity_token_file")?,
        session_name: text("session_name")?,
        region,
        require_credentials: validation != "none",
        legacy: false,
        interrupted: roastedduck_function::file_system::opener().and_then(|client| client.interrupt_flag()),
    };
    if input.secret_type == "rds" {
        if !request.assume_role.is_empty() && request.chain.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "Must pass CHAIN value when passing ASSUME_ROLE_ARN",
            ));
        }
        if request.chain.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "Invalid RDS secret parameters, 'CHAIN' option must be specified",
            ));
        }
        let configuration = roastedduck_aws::resolve_configuration(&request).map_err(provider_error)?;
        let user = text("rds_user")?;
        let host = text("rds_host")?;
        let port = text("rds_port")?;
        if user.is_empty() || host.is_empty() || port.is_empty() || configuration.region.is_empty() {
            return Err(Error::invalid_input(
                "Invalid RDS secret parameters, 'RDS_USER', 'RDS_HOST', 'RDS_PORT' and 'REGION' options must be specified",
            ));
        }
        let map = if text("rds_template_secret_name")?.is_empty() {
            options.clone()
        } else {
            let resolution = roastedduck_aws::resolve(&request).map_err(provider_error)?;
            if resolution.credentials.is_none() && request.require_credentials {
                return Err(validation_error(&request, &resolution.effective_chain));
            }
            let token = resolution
                .credentials
                .as_ref()
                .map(|credentials| {
                    roastedduck_aws::rds_auth_token(
                        credentials,
                        &user,
                        &host,
                        &port,
                        &configuration.region,
                        SystemTime::now(),
                    )
                    .map_err(provider_error)
                })
                .transpose()?
                .unwrap_or_else(|| roastedduck_aws::rds_unsigned_token(&user, &host, &port));
            BTreeMap::from([("session_token".into(), Value::varchar(token))])
        };
        return Ok(KeyValueSecret {
            name: input.name.clone(),
            secret_type: "rds".into(),
            provider: "credential_chain".into(),
            scope: Vec::new(),
            map,
            redact_keys: BTreeSet::from(["secret".into(), "session_token".into()]),
        });
    }
    let resolution = roastedduck_aws::resolve(&request).map_err(provider_error)?;
    if resolution.region.is_empty()
        && let Some(logger) = roastedduck_function::file_system::opener().and_then(|client| client.logger())
        && logger.should_log(LogScope::Connection, "", LogLevel::Warning)
    {
        logger.write_log(LogScope::Connection, "", LogLevel::Warning,
            "Set region explicitly using REGION 'us-east-1' in your CREATE SECRET statement, adding a region to your profile in ~/.aws/config or configure the AWS_REGION or AWS_DEFAULT_REGION environment variables.")?;
    }
    if resolution.credentials.is_none() && request.require_credentials {
        return Err(validation_error(&request, &resolution.effective_chain));
    }
    let mut map = BTreeMap::new();
    if !resolution.region.is_empty() {
        map.insert("region".into(), Value::varchar(resolution.region));
    }
    let refresh = text("refresh")?;
    if refresh == "auto"
        || (refresh.is_empty() && matches!(resolution.effective_chain.as_str(), "sts" | "web_identity"))
    {
        map.insert(
            "refresh_info".into(),
            Value::struct_value(
                roastedduck_function::named_parameters_in_map_order(&refresh_fields)
                    .cloned()
                    .collect(),
            ),
        );
    }
    if let Some(credentials) = resolution.credentials
        && credentials
            .expires_at
            .is_none_or(|expires| expires > SystemTime::now())
    {
        map.insert("key_id".into(), Value::varchar(credentials.access_key));
        map.insert("secret".into(), Value::varchar(credentials.secret_key));
        map.insert(
            "session_token".into(),
            Value::varchar(credentials.session_token.unwrap_or_default()),
        );
    }
    // Preserve the pinned override list, including its URL-compatibility spelling.
    for key in [
        "key_id",
        "secret",
        "region",
        "endpoint",
        "session_token",
        "url_style",
        "use_ssl",
        "s3_url_compatibility_mode",
        "http_proxy",
        "http_proxy_username",
        "http_proxy_password",
    ] {
        if let Some(value) = options.get(key) {
            map.insert(key.to_owned(), value.clone());
        }
    }
    let empty = |map: &BTreeMap<String, Value>, key: &str| -> Result<bool> {
        Ok(map
            .get(key)
            .map(Value::to_varchar)
            .transpose()?
            .is_none_or(|v| v.is_empty()))
    };
    if empty(&map, "endpoint")? {
        match input.secret_type.as_str() {
            "s3" => {
                map.insert("endpoint".into(), Value::varchar("s3.amazonaws.com"));
            }
            "gcs" => {
                map.insert("endpoint".into(), Value::varchar("storage.googleapis.com"));
            }
            "r2" if options.contains_key("account_id") => {
                map.insert(
                    "endpoint".into(),
                    Value::varchar(format!("{}.r2.cloudflarestorage.com", text("account_id")?)),
                );
            }
            _ => {}
        }
    }
    if empty(&map, "url_style")? && matches!(input.secret_type.as_str(), "r2" | "gcs") {
        map.insert("url_style".into(), Value::varchar("path"));
    }
    let scope = if input.scope.is_empty() {
        match input.secret_type.as_str() {
            "s3" => vec!["s3://".into(), "s3n://".into(), "s3a://".into()],
            "gcs" => vec!["gcs://".into(), "gs://".into()],
            "r2" => vec!["r2://".into()],
            _ => vec![String::new()],
        }
    } else {
        input.scope.clone()
    };
    let redact_keys: BTreeSet<String> = ["secret", "session_token"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    Ok(KeyValueSecret {
        name: input.name.clone(),
        secret_type: input.secret_type.clone(),
        provider: "credential_chain".into(),
        scope,
        map,
        redact_keys,
    })
}

#[derive(Debug)]
struct LegacyData {
    instance: Weak<crate::database::DatabaseInstance>,
    client: Arc<FunctionClient>,
    profile: Option<String>,
    redact: bool,
    set_region: bool,
}
impl FunctionData for LegacyData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub(crate) fn legacy_function() -> TableFunctionSet {
    let mut set = TableFunctionSet::new("load_aws_credentials");
    for arguments in [Vec::new(), vec![LogicalType::VARCHAR]] {
        let mut function = TableFunction::new("load_aws_credentials", arguments)
            .with_bind(|input| {
                let context = input.binder.and_then(|c| c.downcast_ref::<roastedduck_planner::ClientContext>())
                    .ok_or_else(|| Error::internal("AWS credentials bound without client context"))?;
                let boolean = |key: &[u8]| input.named_parameters.iter().find(|(name, _)| name.as_bytes().eq_ignore_ascii_case(key))
                    .is_none_or(|(_, value)| value.as_bool().unwrap_or(false));
                let redact = boolean(b"redact_secret");
                if !redact && !context.config.allow_unredacted_secrets {
                    return Err(Error::invalid_input("Displaying unredacted secrets is disabled: set allow_unredacted_secrets to true at startup to enable this"));
                }
                Ok(TableFunctionBindOutput {
                    return_types: vec![LogicalType::VARCHAR; 4],
                    names: ["loaded_access_key_id", "loaded_secret_access_key", "loaded_session_token", "loaded_region"].into_iter().map(ByteString::from).collect(),
                    bind_data: Some(Arc::new(LegacyData {
                        instance: crate::database::instance_state(context)?.instance(),
                        client: Arc::clone(&context.function_client),
                        profile: input.inputs.first().map(Value::to_varchar).transpose()?,
                        redact, set_region: boolean(b"set_region"),
                    })),
                })
            })
            .with_scan(|bind_data, _, state, chunk| {
                if state.position != 0 {
                    *chunk = DataChunk::new((0..4).map(|_| Vector::flat(LogicalType::VARCHAR, 1)).collect(), 0);
                    return Ok(false);
                }
                let data = bind_data.and_then(|d| d.as_any().downcast_ref::<LegacyData>())
                    .ok_or_else(|| Error::internal("AWS credentials scan without bind data"))?;
                let db = crate::Database::from_instance(&data.instance)
                    .ok_or_else(|| Error::internal("AWS credentials scan after database shutdown"))?;
                if !db.httpfs_loaded() {
                    let autoload = data.client.settings().and_then(|s| s.current_setting("autoload_known_extensions").ok())
                        .and_then(|v| v.as_bool()).unwrap_or(false);
                    if !autoload { return Err(Error::new(ErrorKind::MissingExtension, "httpfs extension is required for load_aws_credentials")); }
                    db.load_extension("httpfs")?;
                }
                let resolution = roastedduck_aws::resolve(&roastedduck_aws::Request {
                    profile: data.profile.clone(), legacy: true, require_credentials: false, interrupted: data.client.interrupt_flag(), ..Default::default()
                }).map_err(provider_error)?;
                let mut values = vec![Value::null(LogicalType::VARCHAR); 4];
                if let Some(credentials) = resolution.credentials
                    && credentials.expires_at.is_none_or(|expires| expires > SystemTime::now())
                {
                    let token = credentials.session_token.unwrap_or_default();
                    for (name, value) in [("s3_access_key_id", credentials.access_key.as_str()),
                        ("s3_secret_access_key", credentials.secret_key.as_str()), ("s3_session_token", token.as_str())] {
                        db.set_extension_global_value(name, &Value::varchar(value))?;
                    }
                    for (i, value) in [credentials.access_key.as_str(), credentials.secret_key.as_str(), token.as_str()].into_iter().enumerate() {
                        if !value.is_empty() { values[i] = Value::varchar(if i == 1 && data.redact { "<redacted>" } else { value }); }
                    }
                }
                if data.set_region && !resolution.region.is_empty() {
                    db.set_extension_global_value("s3_region", &Value::varchar(&resolution.region))?;
                    values[3] = Value::varchar(resolution.region);
                }
                *chunk = DataChunk::new(values.iter().map(Vector::constant).collect::<Result<Vec<_>>>()?, 1);
                state.position = 1;
                Ok(false)
            });
        function.named_parameters = vec![
            ("set_region".into(), LogicalType::BOOLEAN),
            ("redact_secret".into(), LogicalType::BOOLEAN),
        ];
        set.add(function);
    }
    set
}
