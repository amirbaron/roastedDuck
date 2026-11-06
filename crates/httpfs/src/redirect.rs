//! Redirect policy for signed requests, handled before S3 region retries.

use rustdb_common::Result;
use ureq::http::Uri;

use crate::io_error;

pub(crate) fn resolve(base: &str, location: &str) -> Result<String> {
    let base = base.parse::<Uri>().map_err(io_error)?;
    let location = location.split('#').next().unwrap_or_default();
    let scheme = base
        .scheme_str()
        .ok_or_else(|| io_error("Redirect base has no scheme"))?;
    let authority = base
        .authority()
        .ok_or_else(|| io_error("Redirect base has no host"))?;
    let target = if location.contains("://") {
        location.to_owned()
    } else if location.starts_with("//") {
        format!("{scheme}:{location}")
    } else {
        let path = if location.is_empty() {
            base.path_and_query().map_or("/", |part| part.as_str()).to_owned()
        } else if location.starts_with('?') {
            format!("{}{location}", base.path())
        } else {
            let combined = if location.starts_with('/') {
                location.to_owned()
            } else {
                let parent = base.path().rsplit_once('/').map_or("", |(parent, _)| parent);
                format!("{parent}/{location}")
            };
            let (path, query) = combined
                .split_once('?')
                .map_or((combined.as_str(), ""), |(path, query)| (path, query));
            let mut segments = Vec::new();
            for segment in path.split('/') {
                match segment {
                    "." => {}
                    ".." => {
                        if segments.len() > 1 {
                            segments.pop();
                        }
                    }
                    _ => segments.push(segment),
                }
            }
            let trailing_directory = path.ends_with("/.") || path.ends_with("/..");
            let mut path = segments.join("/");
            if path.is_empty() || trailing_directory {
                path.push('/');
            }
            if !query.is_empty() || combined.contains('?') {
                path.push('?');
                path.push_str(query);
            }
            path
        };
        format!("{scheme}://{authority}{path}")
    };
    let target = target.parse::<Uri>().map_err(io_error)?;
    if !matches!(target.scheme_str(), Some("http" | "https")) || target.host().is_none() {
        return Err(io_error("Unsupported redirect target"));
    }
    Ok(target.to_string())
}

pub(crate) fn same_origin(left: &str, right: &str) -> bool {
    let (Ok(left), Ok(right)) = (left.parse::<Uri>(), right.parse::<Uri>()) else {
        return false;
    };
    let port = |uri: &Uri| {
        uri.port_u16().unwrap_or(if uri.scheme_str() == Some("https") {
            443
        } else {
            80
        })
    };
    left.scheme() == right.scheme()
        && left
            .host()
            .zip(right.host())
            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
        && port(&left) == port(&right)
}

pub(crate) fn changes_to_get(status: u16, method: &str, httplib: bool) -> bool {
    if httplib {
        status == 303 && !matches!(method, "GET" | "HEAD")
    } else {
        method == "POST" && matches!(status, 301..=303)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_locations_keep_queries_fragments_and_directory_boundaries() {
        for (location, expected) in [
            ("../next?x=1#ignored", "http://example.test/next?x=1"),
            ("?new=2", "http://example.test/a/file?new=2"),
            ("#ignored", "http://example.test/a/file?old=1"),
            ("//other.test/path", "http://other.test/path"),
            ("/x/../y", "http://example.test/y"),
        ] {
            assert_eq!(
                resolve("http://example.test/a/file?old=1", location).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn credential_origin_boundary_includes_scheme_and_port() {
        assert!(same_origin("http://EXAMPLE.test/a", "http://example.test:80/b"));
        assert!(!same_origin("http://example.test/a", "https://example.test/a"));
        assert!(!same_origin("http://example.test/a", "http://example.test:81/a"));
        assert!(!same_origin("http://example.test/a", "http://other.test/a"));
    }
}
