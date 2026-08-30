//! Validated HTTP endpoint and safe REST route construction.

use std::fmt;

use reqwest::Url;

use crate::error::{Error, ErrorKind, Result};

/// An HTTP origin plus an optional path prefix. Query strings, fragments, and
/// userinfo are rejected so routes and credentials cannot be confused.
pub struct Endpoint {
    base: Url,
}

impl Endpoint {
    pub fn new<S: AsRef<str>>(input: S) -> Result<Self> {
        let raw = input.as_ref();
        if raw
            .strip_prefix("http://")
            .is_some_and(|rest| rest.split(['/', '?', '#']).next().is_none_or(str::is_empty))
        {
            return Err(Error::new(ErrorKind::InvalidEndpoint));
        }
        let mut base = Url::parse(raw).map_err(|_| Error::new(ErrorKind::InvalidEndpoint))?;
        if base.scheme() != "http"
            || base.host_str().is_none()
            || base.username() != ""
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.port() == Some(0)
        {
            return Err(Error::new(ErrorKind::InvalidEndpoint));
        }

        let trailing_slashes = base.path().chars().rev().take_while(|&c| c == '/').count();
        {
            let mut segments = base
                .path_segments_mut()
                .map_err(|_| Error::new(ErrorKind::InvalidEndpoint))?;
            for _ in 0..trailing_slashes {
                segments.pop_if_empty();
            }
            segments.push("");
        }
        Ok(Self { base })
    }

    /// Build the two REST route URLs without performing I/O.
    pub fn rest_urls(&self) -> RestUrls {
        RestUrls {
            tags: append_path(&self.base, "tags"),
            values: append_path(&self.base, "values"),
        }
    }

    pub fn tags_url(&self) -> Url {
        self.rest_urls().tags
    }

    pub fn values_url(&self) -> Url {
        self.rest_urls().values
    }

    pub fn scheme(&self) -> &'static str {
        "http"
    }

    pub fn host(&self) -> Option<&str> {
        self.base.host_str()
    }

    pub fn port(&self) -> Option<u16> {
        self.base.port()
    }
}

fn append_path(base: &Url, leaf: &str) -> Url {
    let mut url = base.clone();
    let mut segments = url
        .path_segments_mut()
        .expect("HTTP URLs always support path segment mutation");
    segments.pop_if_empty().push("api").push("v1").push(leaf);
    drop(segments);
    url
}

/// The fixed REST routes derived from an [`Endpoint`].
pub struct RestUrls {
    tags: Url,
    values: Url,
}

impl RestUrls {
    pub fn tags(&self) -> &Url {
        &self.tags
    }

    pub fn values(&self) -> &Url {
        &self.values
    }
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Endpoint")
            .field("scheme", &self.scheme())
            .field("host", &self.host())
            .field("port", &self.port())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_base_prefix_and_builds_safe_routes() {
        let endpoint = Endpoint::new("http://example.test/base///").unwrap();
        assert_eq!(
            endpoint.tags_url().as_str(),
            "http://example.test/base/api/v1/tags"
        );
        assert_eq!(endpoint.values_url().path(), "/base/api/v1/values");
        let debug = format!("{endpoint:?}");
        assert!(!debug.contains("base"));
        assert!(debug.contains("example.test"));
    }

    #[test]
    fn preserves_percent_encoded_prefix_when_building_routes() {
        let endpoint = Endpoint::new("http://example.test/space%20name").unwrap();
        assert_eq!(
            endpoint.tags_url().as_str(),
            "http://example.test/space%20name/api/v1/tags"
        );
    }

    #[test]
    fn rejects_non_http_and_ambiguous_url_parts() {
        for input in [
            "https://example.test",
            "ws://example.test",
            "http://name@example.test",
            "http://name:part@example.test",
            "http://example.test?query=1",
            "http://example.test#fragment",
            "http:///missing-host",
            "http://example.test:0",
        ] {
            let error = match Endpoint::new(input) {
                Ok(_) => panic!("invalid endpoint was accepted"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), ErrorKind::InvalidEndpoint);
        }
    }
}
