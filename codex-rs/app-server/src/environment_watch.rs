use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;

use codex_app_server_protocol::EnvironmentCreatedNotification;
use codex_app_server_protocol::EnvironmentDeletedNotification;
use codex_app_server_protocol::EnvironmentUpdatedNotification;
use codex_app_server_protocol::ServerNotification;
use codex_environment_provider::Environment;
use codex_environment_provider::EnvironmentLifecycleEvent;
use codex_environment_provider::EnvironmentLifecycleService;
use codex_environment_provider::EnvironmentProviderService;
use codex_environment_provider::EnvironmentWatchManager;
use codex_environment_provider::ListEnvironmentProvidersParams;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::outgoing_message::OutgoingMessageSender;
use crate::request_processors::api_environment;

const EVENT_CHANNEL_CAPACITY: usize = 100;
const PROVIDER_LIST_PAGE_SIZE: usize = 100;

pub(crate) struct EnvironmentWatchWorker {
    manager: EnvironmentWatchManager,
    shutdown: CancellationToken,
    _event_task: JoinHandle<()>,
    _startup_task: JoinHandle<()>,
}

impl EnvironmentWatchWorker {
    pub(crate) fn spawn(
        providers: EnvironmentProviderService,
        lifecycle: EnvironmentLifecycleService,
        outgoing: Arc<OutgoingMessageSender>,
    ) -> Self {
        let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let manager = EnvironmentWatchManager::new(lifecycle, events_tx);
        let shutdown = CancellationToken::new();
        let event_task = tokio::spawn(deliver_events(events_rx, outgoing, shutdown.child_token()));
        let startup_task = tokio::spawn(start_configured_providers(
            providers,
            manager.clone(),
            shutdown.child_token(),
        ));
        Self {
            manager,
            shutdown,
            _event_task: event_task,
            _startup_task: startup_task,
        }
    }

    pub(crate) fn manager(&self) -> EnvironmentWatchManager {
        self.manager.clone()
    }

    pub(crate) fn shutdown(&self) {
        self.manager.shutdown();
        self.shutdown.cancel();
    }
}

impl Drop for EnvironmentWatchWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn start_configured_providers(
    providers: EnvironmentProviderService,
    manager: EnvironmentWatchManager,
    shutdown: CancellationToken,
) {
    let mut cursor = None;
    let mut seen_cursors = BTreeSet::new();
    loop {
        let page = tokio::select! {
            () = shutdown.cancelled() => return,
            result = providers.list_providers(ListEnvironmentProvidersParams {
                cursor: cursor.clone(),
                limit: PROVIDER_LIST_PAGE_SIZE,
            }) => result,
        };
        let page = match page {
            Ok(page) => page,
            Err(error) => {
                warn!("failed to list configured environment providers for watches: {error}");
                return;
            }
        };
        for provider in page.data {
            manager.start_provider(provider.id);
        }
        let Some(next_cursor) = page.next_cursor else {
            return;
        };
        if !seen_cursors.insert(next_cursor.clone()) {
            warn!("configured environment provider list returned a repeated cursor");
            return;
        }
        cursor = Some(next_cursor);
    }
}

async fn deliver_events(
    mut events: mpsc::Receiver<EnvironmentLifecycleEvent>,
    outgoing: Arc<OutgoingMessageSender>,
    shutdown: CancellationToken,
) {
    let mut projection = BTreeMap::new();
    loop {
        let event = tokio::select! {
            () = shutdown.cancelled() => return,
            event = events.recv() => event,
        };
        let Some(event) = event else {
            return;
        };
        let Some(event) = apply_event(&mut projection, event) else {
            continue;
        };
        let notification = match event {
            EnvironmentLifecycleEvent::Created(environment) => {
                ServerNotification::EnvironmentCreated(EnvironmentCreatedNotification {
                    environment: api_environment(environment),
                })
            }
            EnvironmentLifecycleEvent::Updated(environment) => {
                ServerNotification::EnvironmentUpdated(EnvironmentUpdatedNotification {
                    environment: api_environment(environment),
                })
            }
            EnvironmentLifecycleEvent::Deleted(environment_ref) => {
                ServerNotification::EnvironmentDeleted(EnvironmentDeletedNotification {
                    environment_id: format!(
                        "{}/{}",
                        environment_ref.provider_id, environment_ref.environment_id
                    ),
                })
            }
        };
        outgoing.send_server_notification(notification).await;
    }
}

fn apply_event(
    projection: &mut BTreeMap<(String, String), Environment>,
    event: EnvironmentLifecycleEvent,
) -> Option<EnvironmentLifecycleEvent> {
    match event {
        EnvironmentLifecycleEvent::Created(environment)
        | EnvironmentLifecycleEvent::Updated(environment) => {
            let key = (
                environment.environment_ref.provider_id.clone(),
                environment.environment_ref.environment_id.clone(),
            );
            match projection.insert(key, environment.clone()) {
                None => Some(EnvironmentLifecycleEvent::Created(environment)),
                Some(previous) if previous != environment => {
                    Some(EnvironmentLifecycleEvent::Updated(environment))
                }
                Some(_) => None,
            }
        }
        EnvironmentLifecycleEvent::Deleted(environment_ref) => projection
            .remove(&(
                environment_ref.provider_id.clone(),
                environment_ref.environment_id.clone(),
            ))
            .map(|_| EnvironmentLifecycleEvent::Deleted(environment_ref)),
    }
}

#[cfg(test)]
#[path = "environment_watch_tests.rs"]
mod tests;
