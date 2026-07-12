use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;

use futures::stream;
use pretty_assertions::assert_eq;

use super::*;
use crate::CreateEnvironmentParams;
use crate::DeleteEnvironmentParams;
use crate::EnvironmentPhase;
use crate::EnvironmentProviderAdapterFuture;
use crate::EnvironmentProviderWatch;
use crate::EnvironmentSource;
use crate::EnvironmentStatus;

struct FakeAdapter {
    pages: Mutex<VecDeque<EnvironmentProviderAdapterResult<EnvironmentListPage>>>,
    reads: Mutex<VecDeque<EnvironmentProviderAdapterResult<Environment>>>,
}

impl EnvironmentProviderAdapter for FakeAdapter {
    fn create_environment(
        &self,
        _params: CreateEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, Environment> {
        Box::pin(async {
            Err(EnvironmentProviderAdapterError::Internal {
                message: "create is not used by reconciliation tests".to_string(),
            })
        })
    }

    fn read_environment(
        &self,
        _params: ReadEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, Environment> {
        let result = self
            .reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .expect("a read result should be configured");
        Box::pin(async move { result })
    }

    fn list_environments(
        &self,
        _params: ListEnvironmentsParams,
    ) -> EnvironmentProviderAdapterFuture<'_, EnvironmentListPage> {
        let result = self
            .pages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .expect("a list page should be configured");
        Box::pin(async move { result })
    }

    fn delete_environment(
        &self,
        _params: DeleteEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, ()> {
        Box::pin(async {
            Err(EnvironmentProviderAdapterError::Internal {
                message: "delete is not used by reconciliation tests".to_string(),
            })
        })
    }

    fn watch(&self) -> EnvironmentProviderAdapterFuture<'_, EnvironmentProviderWatch> {
        Box::pin(async {
            Ok(Box::pin(stream::empty::<
                EnvironmentProviderAdapterResult<EnvironmentProviderEvent>,
            >()) as EnvironmentProviderWatch)
        })
    }
}

fn environment(environment_id: &str, phase: EnvironmentPhase) -> Environment {
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
        status: EnvironmentStatus { phase, error: None },
    }
}

fn page(data: Vec<Environment>, next_cursor: Option<&str>) -> EnvironmentListPage {
    EnvironmentListPage {
        data,
        next_cursor: next_cursor.map(ToString::to_string),
    }
}

fn fake_adapter(
    pages: Vec<EnvironmentProviderAdapterResult<EnvironmentListPage>>,
    reads: Vec<EnvironmentProviderAdapterResult<Environment>>,
) -> Arc<dyn EnvironmentProviderAdapter> {
    Arc::new(FakeAdapter {
        pages: Mutex::new(pages.into()),
        reads: Mutex::new(reads.into()),
    })
}

#[tokio::test]
async fn full_reconciliation_diffs_complete_provider_snapshots() {
    let initial_a = environment("a", EnvironmentPhase::Running);
    let initial_b = environment("b", EnvironmentPhase::Creating);
    let initial_c = environment("c", EnvironmentPhase::Stopped);
    let updated_b = environment("b", EnvironmentPhase::Running);
    let created_d = environment("d", EnvironmentPhase::Creating);
    let adapter = fake_adapter(
        vec![
            Ok(page(
                vec![initial_b.clone(), initial_a.clone()],
                Some("page-2"),
            )),
            Ok(page(vec![initial_c.clone()], None)),
            Ok(page(
                vec![initial_a.clone(), updated_b.clone(), created_d.clone()],
                None,
            )),
        ],
        Vec::new(),
    );
    let mut reconciler = EnvironmentReconciler::new("provider".to_string());

    assert_eq!(
        reconciler
            .reconcile(&adapter)
            .await
            .expect("initial reconciliation should succeed"),
        vec![
            EnvironmentLifecycleEvent::Created(initial_a),
            EnvironmentLifecycleEvent::Created(initial_b),
            EnvironmentLifecycleEvent::Created(initial_c.clone()),
        ]
    );
    assert_eq!(
        reconciler
            .reconcile(&adapter)
            .await
            .expect("second reconciliation should succeed"),
        vec![
            EnvironmentLifecycleEvent::Updated(updated_b),
            EnvironmentLifecycleEvent::Created(created_d),
            EnvironmentLifecycleEvent::Deleted(initial_c.environment_ref),
        ]
    );
}

#[tokio::test]
async fn failed_reconciliation_preserves_the_last_good_projection() {
    let existing = environment("existing", EnvironmentPhase::Running);
    let adapter = fake_adapter(
        vec![
            Ok(page(vec![existing.clone()], None)),
            Ok(page(Vec::new(), Some("repeated"))),
            Ok(page(Vec::new(), Some("repeated"))),
            Ok(page(vec![existing.clone()], None)),
        ],
        Vec::new(),
    );
    let mut reconciler = EnvironmentReconciler::new("provider".to_string());
    assert_eq!(
        reconciler
            .reconcile(&adapter)
            .await
            .expect("initial reconciliation should succeed"),
        vec![EnvironmentLifecycleEvent::Created(existing)]
    );
    assert!(matches!(
        reconciler.reconcile(&adapter).await,
        Err(EnvironmentProviderAdapterError::Internal { .. })
    ));
    assert_eq!(
        reconciler
            .reconcile(&adapter)
            .await
            .expect("recovery reconciliation should succeed"),
        Vec::new()
    );
}

#[tokio::test]
async fn watch_signals_read_complete_records_and_deduplicate_deletes() {
    let initial = environment("environment", EnvironmentPhase::Creating);
    let updated = environment("environment", EnvironmentPhase::Running);
    let adapter = fake_adapter(
        vec![Ok(page(vec![initial.clone()], None))],
        vec![
            Ok(initial.clone()),
            Ok(updated.clone()),
            Err(EnvironmentProviderAdapterError::EnvironmentNotFound {
                environment_id: "environment".to_string(),
            }),
        ],
    );
    let mut reconciler = EnvironmentReconciler::new("provider".to_string());
    reconciler
        .reconcile(&adapter)
        .await
        .expect("initial reconciliation should succeed");

    assert_eq!(
        reconciler
            .apply_event(
                &adapter,
                EnvironmentProviderEvent::Changed {
                    environment_id: "environment".to_string(),
                },
            )
            .await
            .expect("unchanged event should succeed"),
        None
    );
    assert_eq!(
        reconciler
            .apply_event(
                &adapter,
                EnvironmentProviderEvent::Changed {
                    environment_id: "environment".to_string(),
                },
            )
            .await
            .expect("updated event should succeed"),
        Some(EnvironmentLifecycleEvent::Updated(updated))
    );
    assert_eq!(
        reconciler
            .apply_event(
                &adapter,
                EnvironmentProviderEvent::Changed {
                    environment_id: "environment".to_string(),
                },
            )
            .await
            .expect("not-found read should become a delete"),
        Some(EnvironmentLifecycleEvent::Deleted(EnvironmentRef {
            provider_id: "provider".to_string(),
            environment_id: "environment".to_string(),
        }))
    );
    assert_eq!(
        reconciler
            .apply_event(
                &adapter,
                EnvironmentProviderEvent::Deleted {
                    environment_id: "environment".to_string(),
                },
            )
            .await
            .expect("duplicate delete should succeed"),
        None
    );
}
