use std::fs;
use std::sync::Arc;

use codex_keyring_store::CredentialStoreError;
use codex_keyring_store::KeyringStore;
use codex_keyring_store::tests::MockKeyringStore;
use codex_secrets::EnvironmentProviderCredentialCipher;
use codex_state::StateRuntime;
use keyring::Error as KeyringError;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;
use crate::LocalEnvironmentProviderStore;

struct TestService {
    service: EnvironmentProviderService,
    store: Arc<LocalEnvironmentProviderStore>,
    codex_home: TempDir,
}

async fn test_service() -> TestService {
    let codex_home = TempDir::new().expect("temporary Codex home should be created");
    let state_db = StateRuntime::init(codex_home.path().to_path_buf(), "test-provider".to_string())
        .await
        .expect("state runtime should initialize");
    let store = Arc::new(LocalEnvironmentProviderStore::new(state_db));
    let cipher = EnvironmentProviderCredentialCipher::new_with_keyring_store(
        codex_home.path().to_path_buf(),
        Arc::new(MockKeyringStore::default()),
    );
    let service = EnvironmentProviderService::new(store.clone(), cipher);
    TestService {
        service,
        store,
        codex_home,
    }
}

fn pat(token: &str) -> EnvironmentProviderAuthenticationInput {
    EnvironmentProviderAuthenticationInput::Pat(
        PersonalAccessToken::new(token.to_string()).expect("PAT should be non-empty"),
    )
}

async fn create_provider(
    service: &EnvironmentProviderService,
    name: &str,
    token: &str,
) -> EnvironmentProvider {
    service
        .create_provider(EnvironmentProviderServiceCreateParams {
            name: name.to_string(),
            kind: EnvironmentProviderKind::Ona,
            url: None,
            authentication: pat(token),
        })
        .await
        .expect("provider should be created")
}

#[tokio::test]
async fn service_synthesizes_static_provider_without_dynamic_storage() {
    let codex_home = TempDir::new().expect("temporary Codex home should be created");
    let service = EnvironmentProviderService::static_only(
        EnvironmentProviderCredentialCipher::new_with_keyring_store(
            codex_home.path().to_path_buf(),
            Arc::new(MockKeyringStore::default()),
        ),
    );

    assert_eq!(
        service
            .list_providers(ListEnvironmentProvidersParams {
                cursor: None,
                limit: 10,
            })
            .await
            .expect("static provider should list without storage"),
        EnvironmentProviderListPage {
            data: vec![static_provider()],
            next_cursor: None,
        }
    );
    assert!(matches!(
        service
            .create_provider(EnvironmentProviderServiceCreateParams {
                name: "Ona".to_string(),
                kind: EnvironmentProviderKind::Ona,
                url: None,
                authentication: pat("token"),
            })
            .await,
        Err(EnvironmentProviderServiceError::StorageUnavailable { .. })
    ));
}

#[tokio::test]
async fn service_encrypts_credentials_and_resolves_provider_defaults() {
    let fixture = test_service().await;
    let token = "plaintext-token-that-must-not-reach-sqlite";
    let created = create_provider(&fixture.service, "Ona Production", token).await;

    assert_eq!(
        created,
        EnvironmentProvider {
            id: created.id.clone(),
            name: "Ona Production".to_string(),
            kind: EnvironmentProviderKind::Ona,
            url: Some(ONA_DEFAULT_URL.to_string()),
            authentication: Some(EnvironmentProviderAuthentication::Pat),
        }
    );
    assert_eq!(
        fixture
            .service
            .resolve_provider(created.id.clone())
            .await
            .expect("provider should resolve for an authenticated operation"),
        ResolvedEnvironmentProviderDefinition {
            id: created.id,
            name: "Ona Production".to_string(),
            kind: EnvironmentProviderKind::Ona,
            url: ONA_DEFAULT_URL.to_string(),
            authentication: pat(token),
        }
    );

    for entry in fs::read_dir(fixture.codex_home.path()).expect("Codex home should be readable") {
        let path = entry.expect("directory entry should be readable").path();
        if path.is_file() {
            let bytes = fs::read(&path).expect("state file should be readable");
            assert!(
                !bytes
                    .windows(token.len())
                    .any(|window| window == token.as_bytes()),
                "plaintext PAT leaked into {}",
                path.display()
            );
        }
    }
}

#[tokio::test]
async fn service_updates_only_name_and_authentication() {
    let fixture = test_service().await;
    let created = fixture
        .service
        .create_provider(EnvironmentProviderServiceCreateParams {
            name: "Original".to_string(),
            kind: EnvironmentProviderKind::Ona,
            url: Some("https://example.com/control///".to_string()),
            authentication: pat("old-token"),
        })
        .await
        .expect("provider should be created");

    let updated = fixture
        .service
        .update_provider(EnvironmentProviderServiceUpdateParams {
            id: created.id.clone(),
            name: Some("Renamed".to_string()),
            authentication: Some(pat("new-token")),
        })
        .await
        .expect("provider should update");
    assert_eq!(
        updated,
        EnvironmentProvider {
            id: created.id.clone(),
            name: "Renamed".to_string(),
            kind: EnvironmentProviderKind::Ona,
            url: Some("https://example.com/control".to_string()),
            authentication: Some(EnvironmentProviderAuthentication::Pat),
        }
    );
    assert_eq!(
        fixture
            .service
            .resolve_provider(created.id)
            .await
            .expect("updated provider should resolve"),
        ResolvedEnvironmentProviderDefinition {
            id: updated.id,
            name: "Renamed".to_string(),
            kind: EnvironmentProviderKind::Ona,
            url: "https://example.com/control".to_string(),
            authentication: pat("new-token"),
        }
    );
}

#[tokio::test]
async fn service_lists_static_first_with_cursor_pagination() {
    let fixture = test_service().await;
    for name in ["Charlie", "Alpha", "Bravo"] {
        create_provider(&fixture.service, name, &format!("{name}-token")).await;
    }

    let mut cursor = None;
    let mut names = Vec::new();
    loop {
        let page = fixture
            .service
            .list_providers(ListEnvironmentProvidersParams { cursor, limit: 1 })
            .await
            .expect("provider page should load");
        names.extend(page.data.into_iter().map(|provider| provider.name));
        let Some(next_cursor) = page.next_cursor else {
            break;
        };
        cursor = Some(next_cursor);
    }

    assert_eq!(names, vec!["Static", "Alpha", "Bravo", "Charlie"]);
}

#[tokio::test]
async fn service_validates_provider_kind_url_name_and_static_mutation() {
    let fixture = test_service().await;

    for params in [
        EnvironmentProviderServiceCreateParams {
            name: "Static".to_string(),
            kind: EnvironmentProviderKind::Static,
            url: None,
            authentication: pat("token"),
        },
        EnvironmentProviderServiceCreateParams {
            name: "Invalid URL".to_string(),
            kind: EnvironmentProviderKind::Ona,
            url: Some("file:///tmp/provider".to_string()),
            authentication: pat("token"),
        },
        EnvironmentProviderServiceCreateParams {
            name: "  ".to_string(),
            kind: EnvironmentProviderKind::Ona,
            url: None,
            authentication: pat("token"),
        },
    ] {
        assert!(matches!(
            fixture.service.create_provider(params).await,
            Err(EnvironmentProviderServiceError::InvalidRequest { .. })
        ));
    }
    assert!(matches!(
        fixture
            .service
            .update_provider(EnvironmentProviderServiceUpdateParams {
                id: STATIC_ENVIRONMENT_PROVIDER_ID.to_string(),
                name: Some("Other".to_string()),
                authentication: None,
            })
            .await,
        Err(EnvironmentProviderServiceError::InvalidRequest { .. })
    ));
    assert!(matches!(
        fixture
            .service
            .delete_provider_definition_after_cleanup(STATIC_ENVIRONMENT_PROVIDER_ID.to_string())
            .await,
        Err(EnvironmentProviderServiceError::InvalidRequest { .. })
    ));
}

#[derive(Debug)]
struct FailingKeyringStore;

impl KeyringStore for FailingKeyringStore {
    fn load(&self, _service: &str, _account: &str) -> Result<Option<String>, CredentialStoreError> {
        Err(CredentialStoreError::new(KeyringError::Invalid(
            "provider key".into(),
            "unavailable".into(),
        )))
    }

    fn save(
        &self,
        _service: &str,
        _account: &str,
        _value: &str,
    ) -> Result<(), CredentialStoreError> {
        unreachable!("load fails before save")
    }

    fn delete(&self, _service: &str, _account: &str) -> Result<bool, CredentialStoreError> {
        unreachable!("provider service never deletes the encryption key")
    }
}

#[tokio::test]
async fn keyring_failure_blocks_pat_updates_but_not_renames() {
    let fixture = test_service().await;
    let created = create_provider(&fixture.service, "Original", "old-token").await;
    let failing_service = EnvironmentProviderService::new(
        fixture.store,
        EnvironmentProviderCredentialCipher::new_with_keyring_store(
            fixture.codex_home.path().to_path_buf(),
            Arc::new(FailingKeyringStore),
        ),
    );

    let renamed = failing_service
        .update_provider(EnvironmentProviderServiceUpdateParams {
            id: created.id.clone(),
            name: Some("Renamed".to_string()),
            authentication: None,
        })
        .await
        .expect("rename should not access the credential key");
    assert_eq!(
        renamed,
        EnvironmentProvider {
            id: created.id.clone(),
            name: "Renamed".to_string(),
            kind: EnvironmentProviderKind::Ona,
            url: Some(ONA_DEFAULT_URL.to_string()),
            authentication: Some(EnvironmentProviderAuthentication::Pat),
        }
    );
    assert!(matches!(
        failing_service
            .update_provider(EnvironmentProviderServiceUpdateParams {
                id: created.id,
                name: None,
                authentication: Some(pat("new-token")),
            })
            .await,
        Err(EnvironmentProviderServiceError::CredentialUnavailable { .. })
    ));
}
