//! Opaque API-key input validation. The value never has a public formatter or
//! accessor and is zeroized when dropped.

use reqwest::RequestBuilder;
use tokio_tungstenite::tungstenite::http::{header::AUTHORIZATION, HeaderValue, Request};
use zeroize::{Zeroize, Zeroizing};

use crate::error::{Error, ErrorKind};

pub type SecretError = Error;

/// A validated API key owned by the REST transport.
///
/// There is intentionally no `Clone`, `Debug`, `Display`, `Serialize`,
/// `Deserialize`, `as_str`, or `to_string` implementation. The REST transport
/// applies it directly to an Authorization header without returning the raw
/// value to callers.
pub struct SecretApiKey {
    value: String,
}

impl SecretApiKey {
    /// Validate a bearer-header-safe, non-empty ASCII value.
    pub fn new(value: String) -> Result<Self, Error> {
        if value.is_empty() || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
            return Err(Error::new(ErrorKind::InvalidSecret));
        }
        Ok(Self { value })
    }

    pub(crate) fn apply_authorization(&self, request: RequestBuilder) -> RequestBuilder {
        request.bearer_auth(&self.value)
    }

    pub(crate) fn apply_stream_authorization(
        &self,
        mut request: Request<()>,
    ) -> Result<Request<()>, Error> {
        let mut bearer = Zeroizing::new(String::with_capacity(7 + self.value.len()));
        bearer.push_str("Bearer ");
        bearer.push_str(&self.value);
        let value =
            HeaderValue::from_str(&bearer).map_err(|_| Error::new(ErrorKind::InvalidSecret))?;
        let mut value = value;
        value.set_sensitive(true);
        request.headers_mut().insert(AUTHORIZATION, value);
        Ok(request)
    }
}

impl Drop for SecretApiKey {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_secret() -> String {
        ["part", "-", "one", ".", "two"].concat()
    }

    #[test]
    fn accepts_visible_header_value() {
        assert!(SecretApiKey::new(test_secret()).is_ok());
    }

    #[test]
    fn rejects_empty_control_space_and_non_ascii_values() {
        for value in [
            String::new(),
            "a\nvalue".into(),
            "a\tvalue".into(),
            "a value".into(),
            "é".into(),
        ] {
            let error = match SecretApiKey::new(value) {
                Ok(_) => panic!("invalid secret was accepted"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), ErrorKind::InvalidSecret);
            assert!(!error.to_string().contains("part"));
        }
    }

    #[test]
    fn secret_has_no_sensitive_error_surface() {
        let invalid = ["bad", "\r\n", "value"].concat();
        let error = match SecretApiKey::new(invalid) {
            Ok(_) => panic!("invalid secret was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), "invalid_secret");
    }

    #[test]
    fn stream_authorization_header_is_sensitive() {
        let request = Request::builder()
            .uri("ws://example.test/api/v1/stream")
            .body(())
            .unwrap();
        let request = SecretApiKey::new(test_secret())
            .unwrap()
            .apply_stream_authorization(request)
            .unwrap();
        assert!(request.headers().get(AUTHORIZATION).unwrap().is_sensitive());
        assert!(!format!("{:?}", request.headers()).contains(&test_secret()));
        assert!(
            !format!("{:?}", request.headers().get(AUTHORIZATION).unwrap())
                .contains(&test_secret())
        );
    }
}
