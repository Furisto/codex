use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::PoisonError;

use futures::stream;
use pretty_assertions::assert_eq;

use super::*;
use crate::CreateEnvironmentParams;
use crate::DeleteEnvironmentParams;
use crate::Environment;
use crate::EnvironmentListPage;
use crate::EnvironmentPhase;
use crate::EnvironmentProviderAdapterFuture;
use crate::EnvironmentProviderAdapterResult;
use crate::EnvironmentProviderEvent;
use crate::EnvironmentProviderWatch;
use crate::EnvironmentRef;
use crate::EnvironmentSource;
use crate::EnvironmentStatus;
use crate::ListEnvironmentsParams;
use crate::ReadEnvironmentParams;

struct FakeAdapter {
    pages: Mutex<VecDeque<EnvironmentProviderAdapterResult<EnvironmentListPage>>>,
    reads: Mutex<VecDeque<EnvironmentProviderAdapterResult<Environment>>>,
    watches: Mutex<VecDeque<EnvironmentProviderAdapterResult<EnvironmentProviderWatch>>>,
}

impl EnvironmentProviderAdapter for FakeAdapter {
    fn create_environment(
        &self,
        _params: CreateEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, Environment> {
        Box::pin(async { unreachable!("create is not used by watch tests") })
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
        Box::pin(async { unreachable!("delete is not used by watch tests") })
    }

    fn connection(
        &self,
        _params: ReadEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, crate::EnvironmentConnection> {
        Box::pin(async { unreachable!("connection is not used by watch tests") })
    }

    fn watch(&self) -> EnvironmentProviderAdapterFuture<'_, EnvironmentProviderWatch> {
        let result = self
            .watches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .expect("a watch result should be configured");
        Box::pin(async move { result })
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

fn watch(
    events: Vec<EnvironmentProviderAdapterResult<EnvironmentProviderEvent>>,
) -> EnvironmentProviderWatch {
    Box::pin(stream::iter(events))
}

#[tokio::test]
async fn every_watch_connection_reconciles_before_consuming_signals() {
    let creating = environment("a", EnvironmentPhase::Creating);
    let running = environment("a", EnvironmentPhase::Running);
    let recovered = environment("b", EnvironmentPhase::Stopped);
    let adapter: Arc<dyn EnvironmentProviderAdapter> = Arc::new(FakeAdapter {
        pages: Mutex::new(
            vec![
                Ok(EnvironmentListPage {
                    data: vec![creating.clone()],
                    next_cursor: None,
                }),
                Ok(EnvironmentListPage {
                    data: vec![running.clone(), recovered.clone()],
                    next_cursor: None,
                }),
            ]
            .into(),
        ),
        reads: Mutex::new(vec![Ok(running.clone())].into()),
        watches: Mutex::new(
            vec![
                Ok(watch(vec![Ok(EnvironmentProviderEvent::Changed {
                    environment_id: "a".to_string(),
                })])),
                Ok(watch(vec![Ok(EnvironmentProviderEvent::Deleted {
                    environment_id: "a".to_string(),
                })])),
            ]
            .into(),
        ),
    });
    let mut runner = EnvironmentWatchRunner::new("provider".to_string(), adapter);
    let (events_tx, mut events_rx) = mpsc::channel(10);

    assert!(matches!(
        runner.run_connection(&events_tx).await,
        Err(WatchConnectionError::Provider(
            EnvironmentProviderAdapterError::Unavailable { .. }
        ))
    ));
    assert_eq!(
        drain_events(&mut events_rx),
        vec![
            EnvironmentLifecycleEvent::Created(creating),
            EnvironmentLifecycleEvent::Updated(running.clone()),
        ]
    );

    assert!(matches!(
        runner.run_connection(&events_tx).await,
        Err(WatchConnectionError::Provider(
            EnvironmentProviderAdapterError::Unavailable { .. }
        ))
    ));
    assert_eq!(
        drain_events(&mut events_rx),
        vec![
            EnvironmentLifecycleEvent::Created(recovered),
            EnvironmentLifecycleEvent::Deleted(running.environment_ref),
        ]
    );
}

#[tokio::test]
async fn closed_event_receiver_stops_connection_delivery() {
    let adapter: Arc<dyn EnvironmentProviderAdapter> = Arc::new(FakeAdapter {
        pages: Mutex::new(
            vec![Ok(EnvironmentListPage {
                data: vec![environment("a", EnvironmentPhase::Running)],
                next_cursor: None,
            })]
            .into(),
        ),
        reads: Mutex::new(VecDeque::new()),
        watches: Mutex::new(VecDeque::new()),
    });
    let mut runner = EnvironmentWatchRunner::new("provider".to_string(), adapter);
    let (events_tx, events_rx) = mpsc::channel(1);
    drop(events_rx);

    assert!(matches!(
        runner.run_connection(&events_tx).await,
        Err(WatchConnectionError::EventReceiverClosed)
    ));
}

fn drain_events(
    events: &mut mpsc::Receiver<EnvironmentLifecycleEvent>,
) -> Vec<EnvironmentLifecycleEvent> {
    let mut drained = Vec::new();
    while let Ok(event) = events.try_recv() {
        drained.push(event);
    }
    drained
}
