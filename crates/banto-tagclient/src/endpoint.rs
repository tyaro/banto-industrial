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

    pub(crate) fn stream_url(&self) -> Result<Url> {
        let mut url = append_path(&self.base, "stream");
        url.set_scheme("ws")
            .map_err(|_| Error::new(ErrorKind::InvalidEndpoint))?;
        Ok(url)
    }

    /// `POST /api/v1/values/{tag}` (design §5.1/§6). `external_name` becomes
    /// exactly one additional path segment past [`values_url`](Self::values_url);
    /// `Url::path_segments_mut` percent-encodes it, so a name is never
    /// mistaken for an extra path level.
    pub(crate) fn value_url(&self, external_name: &str) -> Url {
        let mut url = append_path(&self.base, "values");
        url.path_segments_mut()
            .expect("HTTP URLs always support path segment mutation")
            .push(external_name);
        url
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

    #[test]
    fn builds_prefixed_websocket_route_without_query_or_fragment() {
        let endpoint = Endpoint::new("http://example.test/private-prefix///").unwrap();
        let url = endpoint.stream_url().unwrap();
        assert_eq!(
            url.as_str(),
            "ws://example.test/private-prefix/api/v1/stream"
        );
        assert!(url.query().is_none());
        assert!(url.fragment().is_none());
    }

    #[test]
    fn builds_prefixed_write_route_as_one_extra_segment() {
        let endpoint = Endpoint::new("http://example.test/private-prefix///").unwrap();
        let url = endpoint.value_url("line1.fast.temp01");
        assert_eq!(
            url.as_str(),
            "http://example.test/private-prefix/api/v1/values/line1.fast.temp01"
        );
    }

    #[test]
    fn write_route_percent_encodes_a_name_instead_of_adding_a_path_level() {
        let endpoint = Endpoint::new("http://example.test").unwrap();
        let url = endpoint.value_url("line1/evil");
        assert_eq!(
            url.as_str(),
            "http://example.test/api/v1/values/line1%2Fevil"
        );
        assert_eq!(url.path_segments().unwrap().count(), 4);
    }
}
