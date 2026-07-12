use std::sync::Arc;

use codex_state::StateRuntime;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;

struct TestStore {
    store: LocalEnvironmentProviderStore,
    _codex_home: TempDir,
}

async fn test_store() -> TestStore {
    let codex_home = TempDir::new().expect("temporary Codex home should be created");
    let state_db = StateRuntime::init(codex_home.path().to_path_buf(), "test-provider".to_string())
        .await
        .expect("state runtime should initialize");
    TestStore {
        store: LocalEnvironmentProviderStore::new(Arc::clone(&state_db)),
        _codex_home: codex_home,
    }
}

fn pat(version: u32, ciphertext: &[u8]) -> StoredEnvironmentProviderAuthentication {
    StoredEnvironmentProviderAuthentication::Pat(EncryptedEnvironmentProviderCredential {
        version,
        ciphertext: ciphertext.to_vec(),
    })
}

async fn create_provider(
    store: &LocalEnvironmentProviderStore,
    name: &str,
    ciphertext: &[u8],
) -> EnvironmentProviderDefinition {
    store
        .create_provider(CreateEnvironmentProviderParams {
            name: name.to_string(),
            kind: EnvironmentProviderKind::Ona,
            url: "https://app.gitpod.io/api".to_string(),
            authentication: pat(/*version*/ 1, ciphertext),
        })
        .await
        .expect("provider should be created")
}

#[tokio::test]
async fn local_store_round_trips_updates_and_deletes_provider_definitions() {
    let fixture = test_store().await;
    let created = create_provider(&fixture.store, "Ona Production", b"ciphertext-one").await;

    assert_eq!(
        fixture
            .store
            .read_provider(created.id.clone())
            .await
            .expect("created provider should be readable"),
        created
    );

    let updated = fixture
        .store
        .update_provider(UpdateEnvironmentProviderParams {
            id: created.id.clone(),
            name: Some("Ona Primary".to_string()),
            authentication: Some(pat(/*version*/ 2, b"ciphertext-two")),
        })
        .await
        .expect("provider should update");
    assert_eq!(
        updated,
        EnvironmentProviderDefinition {
            id: created.id.clone(),
            name: "Ona Primary".to_string(),
            normalized_name: "ona primary".to_string(),
            kind: EnvironmentProviderKind::Ona,
            url: "https://app.gitpod.io/api".to_string(),
            authentication: pat(/*version*/ 2, b"ciphertext-two"),
        }
    );

    fixture
        .store
        .delete_provider(created.id.clone())
        .await
        .expect("provider should delete");
    assert!(matches!(
        fixture.store.read_provider(created.id.clone()).await,
        Err(EnvironmentProviderStoreError::ProviderNotFound { provider_id })
            if provider_id == created.id
    ));
}

#[tokio::test]
async fn local_store_enforces_case_insensitive_unique_names_transactionally() {
    let fixture = test_store().await;
    let first = create_provider(&fixture.store, "Ona Production", b"first").await;

    assert!(matches!(
        fixture
            .store
            .create_provider(CreateEnvironmentProviderParams {
                name: "ona production".to_string(),
                kind: EnvironmentProviderKind::Ona,
                url: "https://example.com/api".to_string(),
                authentication: pat(/*version*/ 1, b"duplicate"),
            })
            .await,
        Err(EnvironmentProviderStoreError::NameConflict { name })
            if name == "ona production"
    ));

    let second = create_provider(&fixture.store, "Ona Staging", b"second").await;
    assert!(matches!(
        fixture
            .store
            .update_provider(UpdateEnvironmentProviderParams {
                id: second.id.clone(),
                name: Some("ONA PRODUCTION".to_string()),
                authentication: Some(pat(/*version*/ 2, b"replacement")),
            })
            .await,
        Err(EnvironmentProviderStoreError::NameConflict { name })
            if name == "ONA PRODUCTION"
    ));
    assert_eq!(
        fixture
            .store
            .read_provider(second.id.clone())
            .await
            .expect("conflicting update should preserve the provider"),
        second
    );
    assert_eq!(
        fixture
            .store
            .read_provider(first.id.clone())
            .await
            .expect("conflicting update should preserve the existing name owner"),
        first
    );
}

#[tokio::test]
async fn local_store_lists_provider_definitions_with_keyset_pagination() {
    let fixture = test_store().await;
    let charlie = create_provider(&fixture.store, "Charlie", b"charlie").await;
    let alpha = create_provider(&fixture.store, "Alpha", b"alpha").await;
    let bravo = create_provider(&fixture.store, "Bravo", b"bravo").await;

    let first_page = fixture
        .store
        .list_providers(ListEnvironmentProvidersParams {
            cursor: None,
            limit: 2,
        })
        .await
        .expect("first provider page should load");
    assert_eq!(first_page.data, vec![alpha, bravo]);
    let cursor = first_page
        .next_cursor
        .expect("first provider page should have a cursor");

    assert_eq!(
        fixture
            .store
            .list_providers(ListEnvironmentProvidersParams {
                cursor: Some(cursor),
                limit: 2,
            })
            .await
            .expect("second provider page should load"),
        EnvironmentProviderPage {
            data: vec![charlie],
            next_cursor: None,
        }
    );
    assert!(matches!(
        fixture
            .store
            .list_providers(ListEnvironmentProvidersParams {
                cursor: Some("not a cursor".to_string()),
                limit: 2,
            })
            .await,
        Err(EnvironmentProviderStoreError::InvalidCursor)
    ));
}

#[tokio::test]
async fn local_store_rejects_the_synthesized_static_provider() {
    let fixture = test_store().await;

    assert!(matches!(
        fixture
            .store
            .create_provider(CreateEnvironmentProviderParams {
                name: "Static".to_string(),
                kind: EnvironmentProviderKind::Static,
                url: String::new(),
                authentication: pat(/*version*/ 1, b"unused"),
            })
            .await,
        Err(EnvironmentProviderStoreError::InvalidRequest { .. })
    ));
}
