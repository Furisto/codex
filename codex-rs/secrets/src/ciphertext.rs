use std::path::PathBuf;
use std::sync::Arc;

use age::secrecy::ExposeSecret;
use age::secrecy::SecretString;
use anyhow::Context;
use anyhow::Result;
use codex_keyring_store::DefaultKeyringStore;
use codex_keyring_store::KeyringStore;

use crate::compute_keyring_account_for_namespace;
use crate::keyring_service;
use crate::local::decrypt_with_passphrase;
use crate::local::encrypt_with_passphrase;
use crate::local::generate_passphrase;

const ENVIRONMENT_PROVIDER_CREDENTIAL_VERSION: u32 = 1;
const ENVIRONMENT_PROVIDER_KEY_NAMESPACE: &str = "environment-provider-credentials";

/// Versioned provider credential ciphertext owned and persisted by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentProviderCredentialCiphertext {
    /// Ciphertext format version required for decryption.
    pub version: u32,
    /// Opaque encrypted credential bytes.
    pub ciphertext: Vec<u8>,
}

/// Encrypts provider credentials with a key stored separately in the OS keyring.
///
/// This type never persists ciphertext. Callers own the returned bytes and can store them beside
/// provider configuration without storing either the plaintext credential or encryption key.
#[derive(Clone)]
pub struct EnvironmentProviderCredentialCipher {
    keyring_account: String,
    keyring_store: Arc<dyn KeyringStore>,
}

impl std::fmt::Debug for EnvironmentProviderCredentialCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvironmentProviderCredentialCipher")
            .field("keyring_account", &self.keyring_account)
            .finish_non_exhaustive()
    }
}

impl EnvironmentProviderCredentialCipher {
    /// Creates a provider credential cipher backed by the platform's default keyring.
    pub fn new(codex_home: PathBuf) -> Self {
        Self::new_with_keyring_store(codex_home, Arc::new(DefaultKeyringStore))
    }

    /// Creates a provider credential cipher backed by the supplied keyring implementation.
    pub fn new_with_keyring_store(
        codex_home: PathBuf,
        keyring_store: Arc<dyn KeyringStore>,
    ) -> Self {
        Self {
            keyring_account: compute_keyring_account_for_namespace(
                &codex_home,
                ENVIRONMENT_PROVIDER_KEY_NAMESPACE,
            ),
            keyring_store,
        }
    }

    /// Encrypts credential bytes and returns caller-owned versioned ciphertext.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<EnvironmentProviderCredentialCiphertext> {
        let key = self.load_or_create_key()?;
        let ciphertext = encrypt_with_passphrase(plaintext, &key)
            .context("failed to encrypt environment provider credential")?;
        Ok(EnvironmentProviderCredentialCiphertext {
            version: ENVIRONMENT_PROVIDER_CREDENTIAL_VERSION,
            ciphertext,
        })
    }

    /// Decrypts caller-owned ciphertext using the separately stored key.
    pub fn decrypt(&self, encrypted: &EnvironmentProviderCredentialCiphertext) -> Result<Vec<u8>> {
        anyhow::ensure!(
            encrypted.version == ENVIRONMENT_PROVIDER_CREDENTIAL_VERSION,
            "environment provider credential version {} is not supported",
            encrypted.version
        );
        let key = self.load_key()?.with_context(|| {
            format!(
                "environment provider credential key {} is missing from the keyring",
                self.keyring_account
            )
        })?;
        decrypt_with_passphrase(&encrypted.ciphertext, &key)
            .context("failed to decrypt environment provider credential")
    }

    fn load_or_create_key(&self) -> Result<SecretString> {
        if let Some(key) = self.load_key()? {
            return Ok(key);
        }
        let key = generate_passphrase().context("failed to generate provider credential key")?;
        self.keyring_store
            .save(
                keyring_service(),
                &self.keyring_account,
                key.expose_secret(),
            )
            .map_err(|err| anyhow::anyhow!(err.message()))
            .with_context(|| {
                format!(
                    "failed to save environment provider credential key {} to the keyring",
                    self.keyring_account
                )
            })?;
        Ok(key)
    }

    fn load_key(&self) -> Result<Option<SecretString>> {
        self.keyring_store
            .load(keyring_service(), &self.keyring_account)
            .map(|key| key.map(SecretString::from))
            .map_err(|err| anyhow::anyhow!(err.message()))
            .with_context(|| {
                format!(
                    "failed to load environment provider credential key {} from the keyring",
                    self.keyring_account
                )
            })
    }
}

#[cfg(test)]
#[path = "ciphertext_tests.rs"]
mod tests;
