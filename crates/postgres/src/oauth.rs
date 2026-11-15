//! libpq 18.3 supplied-token OAuth discovery and issuer validation.
use crate::config::Parameters;
use roastedduck_common::{Error, ErrorKind, Result};
use serde::de::{MapAccess, Visitor};
use std::cell::RefCell;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use zeroize::Zeroizing;

thread_local! {static TOKEN:RefCell<Zeroizing<String>>=RefCell::new(Zeroizing::new(String::new()));}
struct Clear;
impl Drop for Clear {
    fn drop(&mut self) {
        TOKEN.with(|t| *t.borrow_mut() = Zeroizing::new(String::new()));
    }
}
pub fn with_token<T>(token: Option<&str>, call: impl FnOnce() -> T) -> T {
    if let Some(token) = token {
        TOKEN.with(|t| *t.borrow_mut() = Zeroizing::new(token.to_owned()));
    }
    let _clear = Clear;
    call()
}
fn error(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Io, message.into())
}
fn issuer(uri: &str) -> Result<String> {
    let Some((scheme, rest)) = uri.split_once("://") else {
        return Err(error(format!("OAuth discovery URI \"{uri}\" must use HTTPS")));
    };
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(error(format!("OAuth discovery URI \"{uri}\" must use HTTPS")));
    }
    if rest.contains(['?', '#']) {
        return Err(error(format!(
            "OAuth discovery URI \"{uri}\" must not contain query or fragment components"
        )));
    }
    let prefix = "/.well-known/";
    let start = uri
        .find(prefix)
        .ok_or_else(|| error(format!("OAuth discovery URI \"{uri}\" is not a .well-known URI")))?;
    let suffix = &uri[start + prefix.len()..];
    let name = ["openid-configuration", "oauth-authorization-server"]
        .into_iter()
        .find(|n| {
            suffix
                .strip_prefix(n)
                .is_some_and(|s| s.is_empty() || s.starts_with('/'))
        })
        .ok_or_else(|| {
            error(format!(
                "OAuth discovery URI \"{uri}\" uses an unsupported .well-known suffix"
            ))
        })?;
    let end = start + prefix.len() + name.len();
    if end < uri.len() && rest.find('/').map(|p| p + scheme.len() + 3) != Some(start) {
        return Err(error(format!(
            "OAuth discovery URI \"{uri}\" uses an invalid format"
        )));
    }
    Ok(format!("{}{}", &uri[..start], &uri[end..]))
}
struct Pairs(Vec<(String, serde_json::Value)>);
impl<'de> serde::Deserialize<'de> for Pairs {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct Fields;
        impl<'de> Visitor<'de> for Fields {
            type Value = Pairs;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an OAuth error object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> std::result::Result<Self::Value, M::Error> {
                let mut fields = Vec::new();
                while let Some(pair) = map.next_entry()? {
                    fields.push(pair);
                }
                Ok(Pairs(fields))
            }
        }
        d.deserialize_map(Fields)
    }
}
fn challenge(data: &[u8]) -> Result<Vec<(String, String)>> {
    if data.contains(&0) {
        return Err(error(
            "server's error message contained an embedded NULL, and was discarded",
        ));
    }
    let text = std::str::from_utf8(data).map_err(|_| error("server's error response is not valid UTF-8"))?;
    let parse_error = |s: String| error(format!("failed to parse server's error response: {s}"));
    if !text.trim_start().starts_with('{') {
        return Err(parse_error("top-level element must be an object".into()));
    }
    let mut depth = 0_u32;
    let mut quoted = false;
    let mut escaped = false;
    for b in data {
        if quoted {
            if escaped {
                escaped = false;
            } else if *b == b'\\' {
                escaped = true;
            } else if *b == b'"' {
                quoted = false;
            }
        } else {
            match b {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > 8 {
                        return Err(parse_error("JSON is too deeply nested".into()));
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    let Pairs(pairs) = serde_json::from_slice(data).map_err(|e| parse_error(e.to_string()))?;
    let mut result: Vec<(String, String)> = Vec::new();
    for (key, value) in pairs {
        if !["status", "scope", "openid-configuration"].contains(&key.as_str()) {
            continue;
        }
        if result.iter().any(|(k, _)| k == &key) {
            return Err(parse_error(format!("field \"{key}\" is duplicated")));
        }
        let value = value
            .as_str()
            .ok_or_else(|| parse_error(format!("field \"{key}\" must be a string")))?;
        if value.contains('\0') {
            return Err(parse_error(format!("field \"{key}\" contains a NULL")));
        }
        result.push((key, value.to_owned()));
    }
    Ok(result)
}
#[derive(Default)]
struct State {
    discovery: Option<String>,
    sent_token: bool,
}
struct Handler {
    expected: Option<String>,
    client_id: Option<String>,
    token: Zeroizing<String>,
    state: Mutex<State>,
}
impl tokio_postgres::oauth::OAuthHandler for Handler {
    fn authenticated(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .sent_token
    }
    fn initial_response(&self) -> std::result::Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        let (Some(expected), Some(_)) = (&self.expected, &self.client_id) else {
            return Err(Box::new(error(
                "server requires OAuth authentication, but oauth_issuer and oauth_client_id are not both set",
            )));
        };
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.discovery.is_none() && expected.contains("/.well-known/") {
            issuer(expected)?;
            state.discovery = Some(expected.clone());
        }
        if state.discovery.is_none() {
            return Ok(b"n,,\x01auth=\x01\x01".to_vec());
        }
        if self.token.is_empty() {
            return Err(Box::new(error(
                "no OAuth flows are available (try installing the libpq-oauth package)",
            )));
        }
        state.sent_token = true;
        Ok(format!(
            "n,,\x01auth=Bearer {}\x01\x01",
            self.token.split('\0').next().unwrap_or_default()
        )
        .into_bytes())
    }
    fn challenge(&self, data: &[u8]) -> std::result::Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let fields = challenge(data)?;
        let get = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
        let expected = self
            .expected
            .as_deref()
            .ok_or_else(|| error("OAuth issuer missing"))?;
        let expected_id = if expected.contains("/.well-known/") {
            issuer(expected)?
        } else {
            expected.to_owned()
        };
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(discovery) = get("openid-configuration") {
            let actual = issuer(discovery)?;
            if actual != expected_id {
                return Err(Box::new(error(format!(
                    "server's discovery document at {discovery} (issuer \"{actual}\") is incompatible with oauth_issuer ({expected_id})"
                ))));
            }
            if let Some(previous) = &state.discovery {
                if previous != discovery {
                    return Err(Box::new(error(format!(
                        "server's discovery document has moved to {discovery} (previous location was {previous})"
                    ))));
                }
            } else {
                state.discovery = Some(discovery.to_owned());
            }
        }
        let status = get("status").ok_or_else(|| error("server sent error response without a status"))?;
        if status != "invalid_token" {
            return Err(Box::new(error(format!(
                "server rejected OAuth bearer token: {status}"
            ))));
        }
        if state.sent_token {
            return Ok(false);
        }
        if state.discovery.is_none() {
            return Err(Box::new(error(
                "server requires OAuth authentication, but no discovery metadata was provided",
            )));
        }
        if self.token.is_empty() {
            return Err(Box::new(error(
                "no OAuth flows are available (try installing the libpq-oauth package)",
            )));
        }
        Ok(true)
    }
}
pub(crate) fn handler(parameters: &Parameters) -> Arc<dyn tokio_postgres::oauth::OAuthHandler> {
    let token = TOKEN.with(|t| t.borrow().to_string());
    let token = if token.is_empty() {
        std::env::var("PGOAUTHTOKEN").unwrap_or_default()
    } else {
        token
    };
    Arc::new(Handler {
        expected: parameters
            .get("oauth_issuer")
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        client_id: parameters
            .get("oauth_client_id")
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        token: Zeroizing::new(token),
        state: Mutex::default(),
    })
}
