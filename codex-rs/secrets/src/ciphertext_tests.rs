use std::sync::Arc;

use codex_keyring_store::tests::MockKeyringStore;
use keyring::Error as KeyringError;
use pretty_assertions::assert_eq;

use super::*;

#[test]
fn provider_credentials_round_trip_with_separately_stored_key() -> Result<()> {
    let codex_home = tempfile::tempdir().expect("temporary Codex home should be created");
    let keyring = Arc::new(MockKeyringStore::default());
    let cipher = EnvironmentProviderCredentialCipher::new_with_keyring_store(
        codex_home.path().to_path_buf(),
        keyring.clone(),
    );
    let plaintext = b"ona-personal-access-token";

    let first = cipher.encrypt(plaintext)?;
    let second = cipher.encrypt(plaintext)?;

    assert_ne!(first.ciphertext, second.ciphertext);
    assert!(
        !first
            .ciphertext
            .windows(plaintext.len())
            .any(|window| window == plaintext)
    );
    assert_eq!(cipher.decrypt(&first)?, plaintext);
    assert_eq!(cipher.decrypt(&second)?, plaintext);
    let stored_key = keyring
        .saved_value(&cipher.keyring_account)
        .expect("encryption key should be stored in the keyring");
    assert!(!stored_key.contains("ona-personal-access-token"));
    assert!(
        cipher
            .keyring_account
            .starts_with("environment-provider-credentials|")
    );
    Ok(())
}

#[test]
fn provider_credential_decryption_requires_existing_key() {
    let first_home = tempfile::tempdir().expect("first Codex home should be created");
    let first_cipher = EnvironmentProviderCredentialCipher::new_with_keyring_store(
        first_home.path().to_path_buf(),
        Arc::new(MockKeyringStore::default()),
    );
    let encrypted = first_cipher
        .encrypt(b"token")
        .expect("credential should encrypt");
    let second_home = tempfile::tempdir().expect("second Codex home should be created");
    let second_cipher = EnvironmentProviderCredentialCipher::new_with_keyring_store(
        second_home.path().to_path_buf(),
        Arc::new(MockKeyringStore::default()),
    );

    let error = second_cipher
        .decrypt(&encrypted)
        .expect_err("decryption should fail without the original key");
    assert!(
        error.to_string().contains("is missing from the keyring"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn provider_credential_cipher_reports_keyring_failures() {
    let codex_home = tempfile::tempdir().expect("temporary Codex home should be created");
    let keyring = Arc::new(MockKeyringStore::default());
    let cipher = EnvironmentProviderCredentialCipher::new_with_keyring_store(
        codex_home.path().to_path_buf(),
        keyring.clone(),
    );
    keyring.set_error(
        &cipher.keyring_account,
        KeyringError::Invalid("provider key".into(), "unavailable".into()),
    );

    let error = cipher
        .encrypt(b"token")
        .expect_err("encryption should fail when the keyring is unavailable");
    assert!(
        error
            .to_string()
            .contains("failed to load environment provider credential key"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn provider_credential_cipher_rejects_unknown_versions() {
    let codex_home = tempfile::tempdir().expect("temporary Codex home should be created");
    let cipher = EnvironmentProviderCredentialCipher::new_with_keyring_store(
        codex_home.path().to_path_buf(),
        Arc::new(MockKeyringStore::default()),
    );

    let error = cipher
        .decrypt(&EnvironmentProviderCredentialCiphertext {
            version: ENVIRONMENT_PROVIDER_CREDENTIAL_VERSION + 1,
            ciphertext: vec![1, 2, 3],
        })
        .expect_err("unknown ciphertext version should fail");
    assert!(
        error.to_string().contains("version 2 is not supported"),
        "unexpected error: {error:#}"
    );
}
