use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use codex_keyring_store::tests::MockKeyringStore;
use codex_secrets::EnvironmentProviderCredentialCipher;
use codex_state::StateRuntime;
use futures::stream;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;
use crate::CreateEnvironmentParams;
use crate::DeleteEnvironmentParams;
use crate::Environment;
use crate::EnvironmentListPage;
use crate::EnvironmentPhase;
use crate::EnvironmentProviderAdapter;
use crate::EnvironmentProviderAdapterFactory;
use crate::EnvironmentProviderAdapterFuture;
use crate::EnvironmentProviderAdapterResult;
use crate::EnvironmentProviderAuthenticationInput;
use crate::EnvironmentProviderEvent;
use crate::EnvironmentProviderKind;
use crate::EnvironmentProviderService;
use crate::EnvironmentProviderServiceCreateParams;
use crate::EnvironmentProviderWatch;
use crate::EnvironmentRef;
use crate::EnvironmentSource;
use crate::EnvironmentStatus;
use crate::ListEnvironmentsParams;
use crate::LocalEnvironmentProviderStore;
use crate::PersonalAccessToken;
use crate::ReadEnvironmentParams;
use crate::ResolvedEnvironmentProviderDefinition;

struct FakeAdapter {
    provider_id: String,
    list_calls: Arc<AtomicUsize>,
}

impl EnvironmentProviderAdapter for FakeAdapter {
    fn create_environment(
        &self,
        _params: CreateEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, Environment> {
        Box::pin(async { unreachable!("create is not used by watch manager tests") })
    }

    fn read_environment(
        &self,
        _params: ReadEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, Environment> {
        Box::pin(async { unreachable!("read is not used by watch manager tests") })
    }

    fn list_environments(
        &self,
        _params: ListEnvironmentsParams,
    ) -> EnvironmentProviderAdapterFuture<'_, EnvironmentListPage> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        let environment = environment(&self.provider_id);
        Box::pin(async move {
            Ok(EnvironmentListPage {
                data: vec![environment],
                next_cursor: None,
            })
        })
    }

    fn delete_environment(
        &self,
        _params: DeleteEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, ()> {
        Box::pin(async { unreachable!("delete is not used by watch manager tests") })
    }

    fn watch(&self) -> EnvironmentProviderAdapterFuture<'_, EnvironmentProviderWatch> {
        Box::pin(async {
            Ok(Box::pin(stream::pending::<
                EnvironmentProviderAdapterResult<EnvironmentProviderEvent>,
            >()) as EnvironmentProviderWatch)
        })
    }
}

struct FakeAdapterFactory {
    adapter: Arc<FakeAdapter>,
    factory_calls: Arc<AtomicUsize>,
}

impl EnvironmentProviderAdapterFactory for FakeAdapterFactory {
    fn create_adapter(
        &self,
        _definition: ResolvedEnvironmentProviderDefinition,
    ) -> EnvironmentProviderAdapterFuture<'_, Arc<dyn EnvironmentProviderAdapter>> {
        self.factory_calls.fetch_add(1, Ordering::SeqCst);
        let adapter: Arc<dyn EnvironmentProviderAdapter> = self.adapter.clone();
        Box::pin(async move { Ok(adapter) })
    }
}

#[tokio::test]
async fn starting_and_replacing_provider_watch_owns_one_task() {
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
    let list_calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let lifecycle = EnvironmentLifecycleService::new(
        configuration,
        Arc::new(FakeAdapterFactory {
            adapter: Arc::new(FakeAdapter {
                provider_id: provider.id.clone(),
                list_calls: Arc::clone(&list_calls),
            }),
            factory_calls: Arc::clone(&factory_calls),
        }),
    );
    let (events_tx, mut events_rx) = mpsc::channel(4);
    let manager = EnvironmentWatchManager::new(lifecycle, events_tx);

    manager.start_provider(provider.id.clone());
    assert_eq!(
        events_rx.recv().await,
        Some(EnvironmentLifecycleEvent::Created(environment(
            &provider.id
        )))
    );
    manager.start_provider(provider.id.clone());
    assert_eq!(
        events_rx.recv().await,
        Some(EnvironmentLifecycleEvent::Created(environment(
            &provider.id
        )))
    );
    manager.stop_provider(&provider.id);

    assert_eq!(factory_calls.load(Ordering::SeqCst), 1);
    assert_eq!(list_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn static_provider_does_not_start_a_watch_task() {
    let codex_home = TempDir::new().expect("temporary Codex home should be created");
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let adapter = Arc::new(FakeAdapter {
        provider_id: STATIC_ENVIRONMENT_PROVIDER_ID.to_string(),
        list_calls: Arc::new(AtomicUsize::new(0)),
    });
    let lifecycle = EnvironmentLifecycleService::new(
        EnvironmentProviderService::static_only(
            EnvironmentProviderCredentialCipher::new_with_keyring_store(
                codex_home.path().to_path_buf(),
                Arc::new(MockKeyringStore::default()),
            ),
        ),
        Arc::new(FakeAdapterFactory {
            adapter,
            factory_calls: Arc::clone(&factory_calls),
        }),
    );
    let (events_tx, _events_rx) = mpsc::channel(1);
    let manager = EnvironmentWatchManager::new(lifecycle, events_tx);

    manager.start_provider(STATIC_ENVIRONMENT_PROVIDER_ID.to_string());

    assert_eq!(factory_calls.load(Ordering::SeqCst), 0);
}

fn environment(provider_id: &str) -> Environment {
    Environment {
        environment_ref: EnvironmentRef {
            provider_id: provider_id.to_string(),
            environment_id: "environment".to_string(),
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
