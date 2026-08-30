//! Opaque API-key input validation. The value never has a public formatter or
//! accessor and is zeroized when dropped.

use zeroize::Zeroize;

use crate::error::{Error, ErrorKind};

pub type SecretError = Error;

/// A validated API key owned by the future transport layer.
///
/// There is intentionally no `Clone`, `Debug`, `Display`, `Serialize`,
/// `Deserialize`, `as_str`, or `to_string` implementation. S1a does not send
/// headers; the eventual transport adapter must keep the value private too.
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
}
