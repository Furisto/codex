use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use codex_keyring_store::tests::MockKeyringStore;
use codex_secrets::EnvironmentProviderCredentialCipher;
use codex_state::StateRuntime;
use futures::stream;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;
use crate::DeleteEnvironmentProviderMode;
use crate::DeleteEnvironmentProviderParams;
use crate::EnvironmentPhase;
use crate::EnvironmentProviderAdapter;
use crate::EnvironmentProviderAdapterFuture;
use crate::EnvironmentProviderAuthenticationInput;
use crate::EnvironmentProviderCleanup;
use crate::EnvironmentProviderCleanupStatus;
use crate::EnvironmentProviderEvent;
use crate::EnvironmentProviderKind;
use crate::EnvironmentProviderServiceCreateParams;
use crate::EnvironmentProviderWatch;
use crate::EnvironmentRef;
use crate::EnvironmentSource;
use crate::EnvironmentStatus;
use crate::LocalEnvironmentProviderStore;
use crate::PersonalAccessToken;
use crate::ResolvedEnvironmentProviderDefinition;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RecordedOperations {
    creates: Vec<CreateEnvironmentParams>,
    reads: Vec<ReadEnvironmentParams>,
    lists: Vec<ListEnvironmentsParams>,
    deletes: Vec<DeleteEnvironmentParams>,
}

struct FakeAdapter {
    returned_provider_id: String,
    list_data: Mutex<Vec<Environment>>,
    operations: Mutex<RecordedOperations>,
}

impl FakeAdapter {
    fn environment(&self, environment_id: String, phase: EnvironmentPhase) -> Environment {
        Environment {
            environment_ref: EnvironmentRef {
                provider_id: self.returned_provider_id.clone(),
                environment_id,
            },
            source: EnvironmentSource {
                repository_url: "https://example.com/repository.git".to_string(),
                git_ref: "main".to_string(),
            },
            resource_class: "large".to_string(),
            status: EnvironmentStatus { phase, error: None },
        }
    }
}

impl EnvironmentProviderAdapter for FakeAdapter {
    fn create_environment(
        &self,
        params: CreateEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, Environment> {
        self.operations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .creates
            .push(params);
        Box::pin(async { Ok(self.environment("created".to_string(), EnvironmentPhase::Creating)) })
    }

    fn read_environment(
        &self,
        params: ReadEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, Environment> {
        let environment_id = params.environment_id.clone();
        self.operations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reads
            .push(params);
        Box::pin(async move { Ok(self.environment(environment_id, EnvironmentPhase::Running)) })
    }

    fn list_environments(
        &self,
        params: ListEnvironmentsParams,
    ) -> EnvironmentProviderAdapterFuture<'_, EnvironmentListPage> {
        self.operations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .lists
            .push(params);
        let data = self
            .list_data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Box::pin(async move {
            Ok(EnvironmentListPage {
                next_cursor: (!data.is_empty()).then(|| "next".to_string()),
                data,
            })
        })
    }

    fn delete_environment(
        &self,
        params: DeleteEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, ()> {
        self.operations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .deletes
            .push(params);
        Box::pin(async { Ok(()) })
    }

    fn connection(
        &self,
        _params: ReadEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, crate::EnvironmentConnection> {
        Box::pin(async {
            Ok(crate::EnvironmentConnection {
                websocket_url: "wss://example.com/exec".to_string(),
            })
        })
    }

    fn watch(&self) -> EnvironmentProviderAdapterFuture<'_, EnvironmentProviderWatch> {
        Box::pin(async {
            Ok(Box::pin(stream::empty::<
                Result<EnvironmentProviderEvent, EnvironmentProviderAdapterError>,
            >()) as EnvironmentProviderWatch)
        })
    }
}

struct FakeAdapterFactory {
    adapter: Arc<FakeAdapter>,
    calls: Arc<AtomicUsize>,
}

impl EnvironmentProviderAdapterFactory for FakeAdapterFactory {
    fn create_adapter(
        &self,
        _definition: ResolvedEnvironmentProviderDefinition,
    ) -> EnvironmentProviderAdapterFuture<'_, Arc<dyn EnvironmentProviderAdapter>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let adapter: Arc<dyn EnvironmentProviderAdapter> = self.adapter.clone();
        Box::pin(async move { Ok(adapter) })
    }
}

struct TestFixture {
    lifecycle: EnvironmentLifecycleService,
    adapter: Arc<FakeAdapter>,
    factory_calls: Arc<AtomicUsize>,
    provider_id: String,
    _codex_home: TempDir,
}

async fn test_fixture() -> TestFixture {
    let codex_home = TempDir::new().expect("temporary Codex home should be created");
    let state_db = StateRuntime::init(codex_home.path().to_path_buf(), "test-provider".to_string())
        .await
        .expect("state runtime should initialize");
    let configuration = EnvironmentProviderService::new(
        Arc::new(LocalEnvironmentProviderStore::new(state_db)),
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
        returned_provider_id: provider.id.clone(),
        list_data: Mutex::new(Vec::new()),
        operations: Mutex::new(RecordedOperations::default()),
    });
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let lifecycle = EnvironmentLifecycleService::new(
        configuration,
        Arc::new(FakeAdapterFactory {
            adapter: adapter.clone(),
            calls: factory_calls.clone(),
        }),
    );
    TestFixture {
        lifecycle,
        adapter,
        factory_calls,
        provider_id: provider.id,
        _codex_home: codex_home,
    }
}

#[tokio::test]
async fn lifecycle_routes_authoritative_operations_through_one_adapter() {
    let fixture = test_fixture().await;
    let create = CreateEnvironmentParams {
        source: EnvironmentSource {
            repository_url: "https://example.com/repository.git".to_string(),
            git_ref: "main".to_string(),
        },
        resource_class: "large".to_string(),
    };
    fixture
        .lifecycle
        .create_environment(fixture.provider_id.clone(), create.clone())
        .await
        .expect("create should succeed");

    let read = ReadEnvironmentParams {
        environment_id: "read".to_string(),
    };
    let listed_environment = fixture
        .adapter
        .environment("read".to_string(), EnvironmentPhase::Running);
    fixture
        .lifecycle
        .read_environment(fixture.provider_id.clone(), read.clone())
        .await
        .expect("read should succeed");
    fixture
        .adapter
        .list_data
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(listed_environment);
    let list = ListEnvironmentsParams {
        cursor: Some("cursor".to_string()),
        limit: 25,
    };
    fixture
        .lifecycle
        .list_environments(fixture.provider_id.clone(), list.clone())
        .await
        .expect("list should succeed");
    let delete = DeleteEnvironmentParams {
        environment_id: "delete".to_string(),
    };
    fixture
        .lifecycle
        .delete_environment(fixture.provider_id.clone(), delete.clone())
        .await
        .expect("delete should succeed");

    assert_eq!(fixture.factory_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .adapter
            .operations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone(),
        RecordedOperations {
            creates: vec![create],
            reads: vec![read],
            lists: vec![list],
            deletes: vec![delete],
        }
    );
}

#[tokio::test]
async fn lifecycle_invalidates_adapters_and_shares_them_with_provider_deletion() {
    let fixture = test_fixture().await;
    let connector = fixture
        .lifecycle
        .environment_connector(fixture.provider_id.clone(), "read".to_string());
    connector
        .connection()
        .await
        .expect("initial connection should succeed");
    fixture.lifecycle.invalidate_provider(&fixture.provider_id);
    connector
        .connection()
        .await
        .expect("connection after invalidation should recreate the adapter");
    assert_eq!(fixture.factory_calls.load(Ordering::SeqCst), 2);

    assert_eq!(
        fixture
            .lifecycle
            .deletion_service()
            .delete_provider(DeleteEnvironmentProviderParams {
                provider_id: fixture.provider_id,
                mode: DeleteEnvironmentProviderMode::Normal,
            })
            .await
            .expect("empty provider should be deleted"),
        EnvironmentProviderCleanup {
            status: EnvironmentProviderCleanupStatus::Complete,
            failed_environment_ids: Vec::new(),
        }
    );
    assert_eq!(fixture.factory_calls.load(Ordering::SeqCst), 2);
}
