use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;

use codex_keyring_store::tests::MockKeyringStore;
use codex_secrets::EnvironmentProviderCredentialCipher;
use codex_state::StateRuntime;
use futures::stream;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;
use crate::CreateEnvironmentParams;
use crate::Environment;
use crate::EnvironmentListPage;
use crate::EnvironmentPhase;
use crate::EnvironmentProviderAdapterFuture;
use crate::EnvironmentProviderAuthenticationInput;
use crate::EnvironmentProviderKind;
use crate::EnvironmentProviderServiceCreateParams;
use crate::EnvironmentProviderStore;
use crate::EnvironmentProviderWatch;
use crate::EnvironmentRef;
use crate::EnvironmentSource;
use crate::EnvironmentStatus;
use crate::LocalEnvironmentProviderStore;
use crate::PersonalAccessToken;
use crate::ReadEnvironmentParams;
use crate::ResolvedEnvironmentProviderDefinition;

struct TestFixture {
    configuration: EnvironmentProviderService,
    deletion: EnvironmentProviderDeletionService,
    store: Arc<LocalEnvironmentProviderStore>,
    adapter: Arc<FakeAdapter>,
    provider_id: String,
    _codex_home: TempDir,
}

async fn test_fixture(
    pages: Vec<Result<EnvironmentListPage, EnvironmentProviderAdapterError>>,
    failed_deletions: &[&str],
) -> TestFixture {
    let codex_home = TempDir::new().expect("temporary Codex home should be created");
    let state_db = StateRuntime::init(codex_home.path().to_path_buf(), "test-provider".to_string())
        .await
        .expect("state runtime should initialize");
    let store = Arc::new(LocalEnvironmentProviderStore::new(state_db));
    let configuration = EnvironmentProviderService::new(
        store.clone(),
        EnvironmentProviderCredentialCipher::new_with_keyring_store(
            codex_home.path().to_path_buf(),
            Arc::new(MockKeyringStore::default()),
        ),
    );
    let provider = configuration
        .create_provider(EnvironmentProviderServiceCreateParams {
            name: "Production".to_string(),
            kind: EnvironmentProviderKind::Ona,
            url: None,
            authentication: EnvironmentProviderAuthenticationInput::Pat(
                PersonalAccessToken::new("token".to_string()).expect("PAT should be valid"),
            ),
        })
        .await
        .expect("provider should be created");
    let adapter = Arc::new(FakeAdapter {
        pages: Mutex::new(pages.into()),
        failed_deletions: failed_deletions
            .iter()
            .map(|id| (*id).to_string())
            .collect(),
        deleted: Mutex::new(Vec::new()),
    });
    let deletion = EnvironmentProviderDeletionService::new(
        configuration.clone(),
        Arc::new(FakeAdapterFactory {
            adapter: adapter.clone(),
        }),
    );
    TestFixture {
        configuration,
        deletion,
        store,
        adapter,
        provider_id: provider.id,
        _codex_home: codex_home,
    }
}

fn environment(environment_id: &str) -> Environment {
    Environment {
        environment_ref: EnvironmentRef {
            provider_id: "provider".to_string(),
            environment_id: environment_id.to_string(),
        },
        source: EnvironmentSource {
            repository_url: "https://example.com/repository.git".to_string(),
            git_ref: "main".to_string(),
        },
        resource_class: "large".to_string(),
        status: EnvironmentStatus {
            phase: EnvironmentPhase::Running,
            error: None,
        },
    }
}

fn page(ids: &[&str], next_cursor: Option<&str>) -> EnvironmentListPage {
    EnvironmentListPage {
        data: ids.iter().map(|id| environment(id)).collect(),
        next_cursor: next_cursor.map(ToString::to_string),
    }
}

async fn assert_provider_deleted(fixture: &TestFixture) {
    assert!(matches!(
        fixture
            .store
            .read_provider(fixture.provider_id.clone())
            .await,
        Err(crate::EnvironmentProviderStoreError::ProviderNotFound { .. })
    ));
}

#[tokio::test]
async fn normal_deletion_requires_an_authoritative_empty_list() {
    let fixture = test_fixture(vec![Ok(page(&[], None))], &[]).await;

    assert_eq!(
        fixture
            .deletion
            .delete_provider(DeleteEnvironmentProviderParams {
                provider_id: fixture.provider_id.clone(),
                mode: DeleteEnvironmentProviderMode::Normal,
            })
            .await
            .expect("empty provider should delete"),
        EnvironmentProviderCleanup {
            status: EnvironmentProviderCleanupStatus::Complete,
            failed_environment_ids: Vec::new(),
        }
    );
    assert_provider_deleted(&fixture).await;
}

#[tokio::test]
async fn normal_deletion_rejects_non_empty_provider_and_preserves_definition() {
    let fixture = test_fixture(vec![Ok(page(&["environment-a"], None))], &[]).await;

    assert!(matches!(
        fixture
            .deletion
            .delete_provider(DeleteEnvironmentProviderParams {
                provider_id: fixture.provider_id.clone(),
                mode: DeleteEnvironmentProviderMode::Normal,
            })
            .await,
        Err(EnvironmentProviderDeletionError::ProviderNotEmpty { provider_id })
            if provider_id == fixture.provider_id
    ));
    assert!(
        fixture
            .store
            .read_provider(fixture.provider_id.clone())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn normal_deletion_fails_closed_when_provider_list_fails() {
    let fixture = test_fixture(
        vec![Err(EnvironmentProviderAdapterError::Unavailable {
            message: "offline".to_string(),
        })],
        &[],
    )
    .await;

    assert!(matches!(
        fixture
            .deletion
            .delete_provider(DeleteEnvironmentProviderParams {
                provider_id: fixture.provider_id.clone(),
                mode: DeleteEnvironmentProviderMode::Normal,
            })
            .await,
        Err(EnvironmentProviderDeletionError::CleanupUnavailable { .. })
    ));
    assert!(
        fixture
            .store
            .read_provider(fixture.provider_id.clone())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn forced_deletion_reports_complete_and_removes_all_discovered_environments() {
    let fixture = test_fixture(
        vec![
            Ok(page(&["environment-a"], Some("next"))),
            Ok(page(&["environment-b"], None)),
        ],
        &[],
    )
    .await;

    assert_eq!(
        fixture
            .deletion
            .delete_provider(DeleteEnvironmentProviderParams {
                provider_id: fixture.provider_id.clone(),
                mode: DeleteEnvironmentProviderMode::Force,
            })
            .await
            .expect("forced deletion should complete"),
        EnvironmentProviderCleanup {
            status: EnvironmentProviderCleanupStatus::Complete,
            failed_environment_ids: Vec::new(),
        }
    );
    assert_eq!(
        fixture.adapter.deleted(),
        vec!["environment-a", "environment-b"]
    );
    assert_provider_deleted(&fixture).await;
}

#[tokio::test]
async fn forced_deletion_reports_partial_with_known_failed_ids() {
    let fixture = test_fixture(
        vec![Ok(page(&["environment-b", "environment-a"], None))],
        &["environment-b"],
    )
    .await;

    assert_eq!(
        fixture
            .deletion
            .delete_provider(DeleteEnvironmentProviderParams {
                provider_id: fixture.provider_id.clone(),
                mode: DeleteEnvironmentProviderMode::Force,
            })
            .await
            .expect("forced deletion should remove the definition"),
        EnvironmentProviderCleanup {
            status: EnvironmentProviderCleanupStatus::Partial,
            failed_environment_ids: vec!["environment-b".to_string()],
        }
    );
    assert_provider_deleted(&fixture).await;
}

#[tokio::test]
async fn forced_deletion_reports_unknown_but_deletes_ids_found_before_list_failure() {
    let fixture = test_fixture(
        vec![
            Ok(page(&["environment-a"], Some("next"))),
            Err(EnvironmentProviderAdapterError::Unavailable {
                message: "watching page failed".to_string(),
            }),
        ],
        &[],
    )
    .await;

    assert_eq!(
        fixture
            .deletion
            .delete_provider(DeleteEnvironmentProviderParams {
                provider_id: fixture.provider_id.clone(),
                mode: DeleteEnvironmentProviderMode::Force,
            })
            .await
            .expect("forced deletion should remove the definition"),
        EnvironmentProviderCleanup {
            status: EnvironmentProviderCleanupStatus::Unknown,
            failed_environment_ids: Vec::new(),
        }
    );
    assert_eq!(fixture.adapter.deleted(), vec!["environment-a"]);
    assert_provider_deleted(&fixture).await;
}

#[tokio::test]
async fn forced_deletion_removes_definition_when_no_adapter_is_available() {
    let fixture = test_fixture(Vec::new(), &[]).await;
    let deletion =
        EnvironmentProviderDeletionService::without_adapters(fixture.configuration.clone());

    assert_eq!(
        deletion
            .delete_provider(DeleteEnvironmentProviderParams {
                provider_id: fixture.provider_id.clone(),
                mode: DeleteEnvironmentProviderMode::Force,
            })
            .await
            .expect("forced deletion should remove the definition"),
        EnvironmentProviderCleanup {
            status: EnvironmentProviderCleanupStatus::Unknown,
            failed_environment_ids: Vec::new(),
        }
    );
    assert_provider_deleted(&fixture).await;
}

#[derive(Debug)]
struct FakeAdapter {
    pages: Mutex<VecDeque<Result<EnvironmentListPage, EnvironmentProviderAdapterError>>>,
    failed_deletions: BTreeSet<String>,
    deleted: Mutex<Vec<String>>,
}

impl FakeAdapter {
    fn deleted(&self) -> Vec<String> {
        let mut deleted = self
            .deleted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        deleted.sort();
        deleted
    }
}

impl EnvironmentProviderAdapter for FakeAdapter {
    fn create_environment(
        &self,
        _params: CreateEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, Environment> {
        Box::pin(async { Err(unused_operation()) })
    }

    fn read_environment(
        &self,
        _params: ReadEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, Environment> {
        Box::pin(async { Err(unused_operation()) })
    }

    fn list_environments(
        &self,
        _params: ListEnvironmentsParams,
    ) -> EnvironmentProviderAdapterFuture<'_, EnvironmentListPage> {
        Box::pin(async move {
            self.pages
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front()
                .unwrap_or_else(|| Ok(page(&[], None)))
        })
    }

    fn delete_environment(
        &self,
        params: DeleteEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, ()> {
        Box::pin(async move {
            if self.failed_deletions.contains(&params.environment_id) {
                return Err(EnvironmentProviderAdapterError::Unavailable {
                    message: format!("failed to delete {}", params.environment_id),
                });
            }
            self.deleted
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(params.environment_id);
            Ok(())
        })
    }

    fn watch(&self) -> EnvironmentProviderAdapterFuture<'_, EnvironmentProviderWatch> {
        Box::pin(async {
            let watch: EnvironmentProviderWatch = Box::pin(stream::empty());
            Ok(watch)
        })
    }
}

fn unused_operation() -> EnvironmentProviderAdapterError {
    EnvironmentProviderAdapterError::Internal {
        message: "operation is unused by provider deletion tests".to_string(),
    }
}

#[derive(Debug)]
struct FakeAdapterFactory {
    adapter: Arc<FakeAdapter>,
}

impl EnvironmentProviderAdapterFactory for FakeAdapterFactory {
    fn create_adapter(
        &self,
        _definition: ResolvedEnvironmentProviderDefinition,
    ) -> EnvironmentProviderAdapterFuture<'_, Arc<dyn EnvironmentProviderAdapter>> {
        let adapter: Arc<dyn EnvironmentProviderAdapter> = self.adapter.clone();
        Box::pin(async move { Ok(adapter) })
    }
}
