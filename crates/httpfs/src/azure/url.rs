use roastedduck_common::{Error, Result};

#[derive(Clone, Debug)]
pub(super) struct AzureUrl {
    pub prefix: String,
    pub account: String,
    pub endpoint: String,
    pub container: String,
    pub path: String,
}

impl AzureUrl {
    pub fn parse(url: &str) -> Result<Self> {
        let invalid = || {
            super::io_error(format!(
                "The URL {url} does not match the expected formats: (azure|az)://<container>/[<path>] or the fully qualified one: (abfs[s]|azure|az)://<storage account>.<endpoint>/<container>/[<path>] or abfs[s]://<container>@<storage account>.<endpoint>/[<path>]"
            ))
        };
        let (scheme, rest) = url
            .split_once("://")
            .filter(|(s, _)| matches!(*s, "azure" | "az" | "abfs" | "abfss"))
            .ok_or_else(|| {
                super::io_error("URL needs to start with azure:// or az:// or abfss:// or abfs://")
            })?;
        let (authority, tail) = rest.split_once('/').ok_or_else(invalid)?;
        let mut result = Self {
            prefix: format!("{scheme}://"),
            account: String::new(),
            endpoint: String::new(),
            container: String::new(),
            path: String::new(),
        };
        if let Some((head, endpoint)) = authority.split_once('.') {
            result.endpoint = endpoint.into();
            if matches!(scheme, "abfs" | "abfss")
                && let Some((container, account)) = head.split_once('@')
            {
                result.container = container.into();
                result.account = account.into();
                result.path = tail.into();
            } else {
                let (container, path) = tail.split_once('/').ok_or_else(invalid)?;
                result.account = head.into();
                result.container = container.into();
                result.path = path.into();
            }
        } else {
            if authority.is_empty() {
                return Err(invalid());
            }
            result.container = authority.into();
            result.path = tail.into();
        }
        Ok(result)
    }

    pub fn qualified(&self) -> bool {
        !self.endpoint.is_empty()
    }
    pub fn dfs(&self) -> bool {
        self.prefix.starts_with("abfs")
    }
    pub fn result_prefix(&self) -> String {
        if self.qualified() {
            format!(
                "{}{}.{}/{}/",
                self.prefix, self.account, self.endpoint, self.container
            )
        } else {
            format!("{}{}/", self.prefix, self.container)
        }
    }
}

pub(super) fn encode(text: &str, slash: bool) -> String {
    use std::fmt::Write;
    let mut result = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) || slash && b == b'/' {
            result.push(char::from(b));
        } else {
            let _ = write!(result, "%{b:02X}");
        }
    }
    result
}

pub(super) fn decode(text: &str) -> Result<String> {
    let mut output = Vec::new();
    let mut bytes = text.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hex = |b: Option<u8>| b.and_then(|v| char::from(v).to_digit(16));
            let high = hex(bytes.next());
            let low = hex(bytes.next());
            let (Some(high), Some(low)) = (high, low) else {
                return Err(Error::invalid_input("Invalid URL encoding"));
            };
            output.push((high * 16 + low) as u8);
        } else {
            output.push(b);
        }
    }
    String::from_utf8(output).map_err(super::io_error)
}
