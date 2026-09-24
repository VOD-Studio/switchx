use std::fmt;

use keyring::{Entry, Error as KeyringError};
use zeroize::Zeroize;

pub const PROVIDER_KEY_SERVICE: &str = "dev.switchx.provider-key";

pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([redacted])")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialError {
    InvalidReference,
    Missing,
    Unavailable,
}

impl fmt::Display for CredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidReference => "invalid credential reference",
            Self::Missing => "credential is missing",
            Self::Unavailable => "system credential store is unavailable",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CredentialError {}

pub struct CredentialStore {
    service: String,
}

impl CredentialStore {
    pub fn new(service: &str) -> Result<Self, CredentialError> {
        if !service.starts_with("dev.switchx.") || service.len() > 128 {
            return Err(CredentialError::InvalidReference);
        }
        Ok(Self {
            service: service.to_owned(),
        })
    }

    fn entry(&self, reference: &str) -> Result<Entry, CredentialError> {
        if reference.is_empty()
            || reference.len() > 64
            || !reference
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(CredentialError::InvalidReference);
        }
        Entry::new(&self.service, reference).map_err(map_error)
    }

    pub fn put(&self, reference: &str, secret: &Secret) -> Result<(), CredentialError> {
        if secret.expose().is_empty() {
            return Err(CredentialError::InvalidReference);
        }
        self.entry(reference)?
            .set_password(secret.expose())
            .map_err(map_error)
    }

    pub fn get(&self, reference: &str) -> Result<Secret, CredentialError> {
        self.entry(reference)?
            .get_password()
            .map(Secret::new)
            .map_err(map_error)
    }

    pub fn delete(&self, reference: &str) -> Result<(), CredentialError> {
        self.entry(reference)?
            .delete_credential()
            .map_err(map_error)
    }
}

fn map_error(error: KeyringError) -> CredentialError {
    match error {
        KeyringError::NoEntry => CredentialError::Missing,
        _ => CredentialError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_references_and_redacts_debug_output() {
        let store = CredentialStore::new(PROVIDER_KEY_SERVICE).unwrap();
        assert_eq!(
            store.get("../outside").unwrap_err(),
            CredentialError::InvalidReference
        );
        assert!(CredentialStore::new("another.app").is_err());
        assert_eq!(
            format!("{:?}", Secret::new("private-value".into())),
            "Secret([redacted])"
        );
    }
}
