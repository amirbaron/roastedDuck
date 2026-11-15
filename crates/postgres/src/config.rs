//! libpq-style connection options, kept separate from the wire driver's subset.
use roastedduck_common::{Error, Result};
use std::collections::BTreeMap;

#[derive(Clone, Default)]
pub struct Parameters {
    values: BTreeMap<String, String>,
}
impl std::fmt::Debug for Parameters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresParameters")
            .field("host", &self.get("host"))
            .field("dbname", &self.get("dbname"))
            .field("user", &self.get("user"))
            .field("sslmode", &self.get("sslmode"))
            .finish_non_exhaustive()
    }
}

const ENVIRONMENT: &[(&str, &str)] = &[
    ("host", "PGHOST"),
    ("hostaddr", "PGHOSTADDR"),
    ("port", "PGPORT"),
    ("dbname", "PGDATABASE"),
    ("user", "PGUSER"),
    ("password", "PGPASSWORD"),
    ("passfile", "PGPASSFILE"),
    ("application_name", "PGAPPNAME"),
    ("options", "PGOPTIONS"),
    ("connect_timeout", "PGCONNECT_TIMEOUT"),
    ("sslmode", "PGSSLMODE"),
    ("sslrootcert", "PGSSLROOTCERT"),
    ("sslcert", "PGSSLCERT"),
    ("sslkey", "PGSSLKEY"),
    ("sslpassword", "PGSSLPASSWORD"),
    ("sslcrl", "PGSSLCRL"),
    ("sslnegotiation", "PGSSLNEGOTIATION"),
    ("channel_binding", "PGCHANNELBINDING"),
    ("target_session_attrs", "PGTARGETSESSIONATTRS"),
    ("service", "PGSERVICE"),
];

fn invalid(message: impl Into<String>) -> Error {
    Error::invalid_input(message.into())
}
fn decode(value: &str) -> Result<String> {
    percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .map(std::borrow::Cow::into_owned)
        .map_err(|_| invalid("invalid UTF-8 in PostgreSQL connection URI"))
}

impl Parameters {
    pub fn parse(input: &str) -> Result<Self> {
        let mut values = if let Some(uri) = input
            .strip_prefix("postgresql://")
            .or_else(|| input.strip_prefix("postgres://"))
        {
            uri_values(uri)?
        } else {
            keyword_values(input)?
        };
        let service = values
            .get("service")
            .cloned()
            .or_else(|| std::env::var("PGSERVICE").ok());
        if let Some(service) = service.filter(|s| !s.is_empty()) {
            let path = std::env::var("PGSERVICEFILE").unwrap_or_else(|_| {
                format!("{}/.pg_service.conf", std::env::var("HOME").unwrap_or_default())
            });
            let content = std::fs::read_to_string(&path)
                .map_err(|_| invalid(format!("service file '{path}' could not be read")))?;
            let mut active = false;
            let mut found = false;
            for line in content
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
            {
                if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                    active = section == service;
                    found |= active;
                } else if active && let Some((key, value)) = line.split_once('=') {
                    values
                        .entry(key.trim().into())
                        .or_insert_with(|| value.trim().into());
                }
            }
            if !found {
                return Err(invalid(format!("definition of service '{service}' not found")));
            }
        }
        for &(key, variable) in ENVIRONMENT {
            if !values.contains_key(key)
                && let Ok(value) = std::env::var(variable)
                && !value.is_empty()
            {
                values.insert(key.into(), value);
            }
        }
        if !values.contains_key("user") {
            values.insert(
                "user".into(),
                whoami::username().map_err(|e| invalid(e.to_string()))?,
            );
        }
        if !values.contains_key("dbname") {
            values.insert("dbname".into(), values.get("user").cloned().unwrap_or_default());
        }
        values.entry("host".into()).or_insert_with(|| {
            if cfg!(unix) {
                "/tmp".into()
            } else {
                "localhost".into()
            }
        });
        values.entry("port".into()).or_insert_with(|| "5432".into());
        values.entry("sslmode".into()).or_insert_with(|| "prefer".into());
        if !values.contains_key("password") {
            let file = values
                .get("passfile")
                .cloned()
                .unwrap_or_else(|| format!("{}/.pgpass", std::env::var("HOME").unwrap_or_default()));
            if let Ok(content) = std::fs::read_to_string(&file) {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    if std::fs::metadata(&file).is_ok_and(|m| m.permissions().mode() & 0o077 != 0) {
                        return Ok(Self { values });
                    }
                }
                let host = values.get("host").map_or("localhost", String::as_str);
                let host = if host.starts_with('/') {
                    "localhost"
                } else {
                    host.split(',').next().unwrap_or(host)
                };
                let wanted = [
                    host,
                    values.get("port").map_or("5432", String::as_str),
                    values.get("dbname").map_or("", String::as_str),
                    values.get("user").map_or("", String::as_str),
                ];
                for line in content.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
                    let mut fields = vec![String::new()];
                    let mut escaped = false;
                    for c in line.chars() {
                        if escaped {
                            if let Some(last) = fields.last_mut() {
                                last.push(c);
                            }
                            escaped = false;
                        } else if c == '\\' {
                            escaped = true;
                        } else if c == ':' && fields.len() < 5 {
                            fields.push(String::new());
                        } else if let Some(last) = fields.last_mut() {
                            last.push(c);
                        }
                    }
                    if fields.len() == 5
                        && fields[..4]
                            .iter()
                            .zip(wanted)
                            .all(|(field, w)| field == "*" || field == w)
                    {
                        values.insert("password".into(), fields[4].clone());
                        break;
                    }
                }
            }
        }
        Ok(Self { values })
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.values.insert(key.into(), value.into());
    }
    pub fn wire_config(&self) -> Result<tokio_postgres::Config> {
        let mut text = String::new();
        for (key, value) in &self.values {
            if matches!(
                key.as_str(),
                "sslrootcert"
                    | "sslcert"
                    | "sslkey"
                    | "sslpassword"
                    | "sslcrl"
                    | "passfile"
                    | "service"
                    | "oauth_token"
            ) {
                continue;
            }
            let value = if key == "sslmode" && matches!(value.as_str(), "verify-ca" | "verify-full") {
                "require"
            } else if key == "sslmode" && value == "allow" {
                "disable"
            } else {
                value
            };
            text.push_str(key);
            text.push_str("='");
            text.push_str(&value.replace('\\', "\\\\").replace('\'', "\\'"));
            text.push_str("' ");
        }
        text.parse()
            .map_err(|e: tokio_postgres::Error| invalid(e.to_string()))
    }
}

fn keyword_values(input: &str) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    let mut chars = input.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            return Ok(result);
        }
        let mut key = String::new();
        while chars.peek().is_some_and(|c| !c.is_whitespace() && *c != '=') {
            if let Some(c) = chars.next() {
                key.push(c);
            }
        }
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if key.is_empty() || chars.next() != Some('=') {
            return Err(invalid(format!(
                "missing \"=\" after \"{key}\" in connection info string"
            )));
        }
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let quoted = chars.peek() == Some(&'\'');
        if quoted {
            chars.next();
        }
        let mut value = String::new();
        let mut closed = !quoted;
        while let Some(c) = chars.next() {
            if c == '\\' {
                value.push(
                    chars
                        .next()
                        .ok_or_else(|| invalid("unterminated quoted string in connection info string"))?,
                );
            } else if quoted && c == '\'' {
                closed = true;
                break;
            } else if !quoted && c.is_whitespace() {
                break;
            } else {
                value.push(c);
            }
        }
        if !closed {
            return Err(invalid("unterminated quoted string in connection info string"));
        }
        result.insert(key, value);
    }
}

fn uri_values(uri: &str) -> Result<BTreeMap<String, String>> {
    let (address, query) = uri.split_once('?').unwrap_or((uri, ""));
    let (authority, database) = address.split_once('/').unwrap_or((address, ""));
    let mut result = BTreeMap::new();
    let hosts = if let Some((user, hosts)) = authority.rsplit_once('@') {
        let (user, password) = user.split_once(':').map_or((user, None), |(u, p)| (u, Some(p)));
        result.insert("user".into(), decode(user)?);
        if let Some(password) = password {
            result.insert("password".into(), decode(password)?);
        }
        hosts
    } else {
        authority
    };
    let mut names = Vec::new();
    let mut ports = Vec::new();
    for host in hosts.split(',').filter(|s| !s.is_empty()) {
        let (name, port) = if let Some(ip) = host.strip_prefix('[') {
            let (ip, tail) = ip
                .split_once(']')
                .ok_or_else(|| invalid("invalid IPv6 address in connection URI"))?;
            (ip, tail.strip_prefix(':').unwrap_or(""))
        } else {
            host.split_once(':').unwrap_or((host, ""))
        };
        names.push(decode(name)?);
        ports.push(decode(port)?);
    }
    if !names.is_empty() {
        result.insert("host".into(), names.join(","));
        if ports.iter().any(|p| !p.is_empty()) {
            result.insert("port".into(), ports.join(","));
        }
    }
    if !database.is_empty() {
        result.insert("dbname".into(), decode(database)?);
    }
    for field in query.split('&').filter(|s| !s.is_empty()) {
        let (key, value) = field
            .split_once('=')
            .ok_or_else(|| invalid("missing key/value separator in connection URI"))?;
        result.insert(decode(key)?, decode(value)?);
    }
    Ok(result)
}
