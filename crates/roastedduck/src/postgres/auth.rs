//! Connection factories and the upstream RDS template/token lifecycle.
use crate::secrets::KeyValueSecret;
use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_postgres::{Driver, Session, config::Parameters};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Instant;

type Factory = Arc<dyn Fn() -> Result<Session> + Send + Sync>;
// StringUtil::Trim uses ASCII whitespace on the left and positive C char
// values on the right. Preserve the pinned platform ABI's signed-char behavior.
fn trimmed(value: &str) -> &str {
    let space = |byte: u8| matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
    let bytes = value.as_bytes();
    let start = bytes.iter().position(|b| !space(*b)).unwrap_or(bytes.len());
    let end = bytes[start..]
        .iter()
        .rposition(|b| (*b as std::ffi::c_char) > 0 && !space(*b))
        .map_or(start, |p| start + p + 1);
    &value[start..end]
}
fn configuration(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidConfiguration, message.into())
}
fn run(connection: &mut crate::Connection, sql: &str) -> Result<()> {
    connection
        .query(sql)
        .map(|_| ())
        .map_err(|error| configuration(format!("Error generating RDS IAM token: {error}")))
}
fn generate(
    instance: &Weak<crate::database::DatabaseInstance>,
    template: &str,
    attached: &str,
) -> Result<String> {
    let instance = instance
        .upgrade()
        .ok_or_else(|| Error::internal("Postgres token generation after shutdown"))?;
    let database = crate::Database::from_instance(&Arc::downgrade(&instance))
        .ok_or_else(|| Error::internal("Postgres token generation after shutdown"))?;
    let mut connection = database.connect()?;
    run(&mut connection, "LOAD aws")?;
    run(&mut connection, "BEGIN TRANSACTION")?;
    let temporary = format!("{template}_{attached}_postgres_temp");
    let quoted = super::identifier(&temporary);
    let mut created = false;
    let result = (|| {
        let entry = connection
            .in_query_scope(|connection| {
                instance
                    .secrets
                    .get_secret_by_name(template, None, &connection.current_transaction())
            })?
            .ok_or_else(|| {
                configuration(format!(
                    "Specified RDS secret with name: \"{template}\" not found"
                ))
            })?;
        let mut query =
            format!("CREATE OR REPLACE TEMPORARY SECRET {quoted} (TYPE rds, PROVIDER credential_chain,");
        for (name, value) in entry.secret.map {
            let value = value
                .to_sql_string()
                .into_string()
                .map_err(|_| Error::invalid_input("Invalid UTF-8 in RDS template"))?;
            let _ = write!(query, "{} {value},", super::identifier(&name));
        }
        let _ = write!(query, "RDS_TEMPLATE_SECRET_NAME {})", super::literal(template));
        connection.query(&query).map_err(|error|configuration(format!("Error generating RDS IAM token, type: {}, error creating RDS secret from template: {template}",error.kind().display_name().to_ascii_uppercase().replace(' ',"_"))))?;
        created = true;
        let entry = connection
            .in_query_scope(|connection| {
                instance
                    .secrets
                    .get_secret_by_name(&temporary, None, &connection.current_transaction())
            })?
            .ok_or_else(|| {
                configuration(format!(
                    "Specified RDS secret with name: \"{temporary}\" not found"
                ))
            })?;
        let token = entry
            .secret
            .map
            .get("session_token")
            .map(roastedduck_types::Value::to_varchar)
            .transpose()?
            .unwrap_or_else(|| "NULL".into());
        if !token.contains("X-Amz-Algorithm") {
            return Err(configuration(format!(
                "Unable to generate RDS IAM token for secret with name: \"{template}\""
            )));
        }
        Ok(token)
    })();
    let cleanup = if created {
        run(&mut connection, &format!("DROP SECRET {quoted}"))
    } else {
        Ok(())
    };
    let _ = connection.query("ROLLBACK");
    result.and_then(|token| cleanup.map(|()| token))
}

pub(crate) fn factory(
    instance: Weak<crate::database::DatabaseInstance>,
    attached: &str,
    path: &str,
    secret: Option<&KeyValueSecret>,
) -> Result<Factory> {
    let driver = Driver::new()?;
    let template = secret
        .and_then(|s| s.map.get("aws_rds_secret"))
        .filter(|v| !v.is_null())
        .map(roastedduck_types::Value::to_varchar)
        .transpose()?
        .unwrap_or_default();
    let template = trimmed(&template).to_owned();
    if template.is_empty() {
        let dsn = crate::secrets::postgres::connection_string(secret, path)?;
        return Ok(Arc::new(move || driver.connect(&Parameters::parse(&dsn)?)));
    }
    let secret = secret.ok_or_else(|| Error::internal("RDS template without secret"))?;
    if secret
        .map
        .get("password")
        .filter(|v| !v.is_null())
        .map(roastedduck_types::Value::to_varchar)
        .transpose()?
        .is_some_and(|s| !trimmed(&s).is_empty())
    {
        return Err(Error::binder(
            "Invalid AWS RDS IAM auth secret configuration: 'PASSWORD' parameters must not be specified - generated IAM token will be used instead of the password",
        ));
    }
    let mut base = String::new();
    for key in crate::secrets::postgres::CONNECTION_OPTIONS
        .iter()
        .filter(|k| !matches!(**k, "password" | "passfile"))
    {
        if let Some(value) = secret.map.get(*key).filter(|v| !v.is_null()) {
            let _ = write!(
                base,
                "{key}={} ",
                crate::secrets::postgres::escape(&value.to_varchar()?)
            );
        }
    }
    let attached = attached.to_owned();
    let path = path.to_owned();
    let cache = Mutex::new(None::<(Instant, String)>);
    Ok(Arc::new(move || {
        let dsn = {
            let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
            if cache
                .as_ref()
                .is_none_or(|(when, _)| when.elapsed().as_secs() > 840)
            {
                let now = Instant::now();
                let token = generate(&instance, &template, &attached)?;
                *cache = Some((now, token));
            }
            let token = &cache
                .as_ref()
                .ok_or_else(|| Error::internal("Missing RDS token"))?
                .1;
            format!(
                "{base}password={} {path}",
                crate::secrets::postgres::escape(token)
            )
        };
        driver.connect(&Parameters::parse(&dsn)?)
    }))
}

pub(crate) fn with_oauth<T>(
    client: &roastedduck_function::FunctionClient,
    call: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let token = client
        .settings()
        .and_then(|s| s.current_setting("pg_oauth_token").ok())
        .filter(|v| !v.is_null())
        .map(|v| v.to_varchar())
        .transpose()?;
    roastedduck_postgres::oauth::with_token(token.as_deref(), call)
}
