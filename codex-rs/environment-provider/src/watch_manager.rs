use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use crate::EnvironmentLifecycleEvent;
use crate::EnvironmentLifecycleService;
use crate::EnvironmentWatchError;
use crate::STATIC_ENVIRONMENT_PROVIDER_ID;

const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Owns the process-scoped watch task for every configured dynamic provider.
///
/// Starting an already watched provider replaces its task. This is used after PAT updates so the
/// replacement resolves a fresh adapter and performs a complete reconciliation before watching.
/// Provider events share a caller-owned bounded channel, which applies backpressure consistently
/// across all providers.
#[derive(Clone)]
pub struct EnvironmentWatchManager {
    lifecycle: EnvironmentLifecycleService,
    events: mpsc::Sender<EnvironmentLifecycleEvent>,
    tasks: Arc<Mutex<HashMap<String, AbortHandle>>>,
}

impl std::fmt::Debug for EnvironmentWatchManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvironmentWatchManager")
            .field("lifecycle", &self.lifecycle)
            .finish_non_exhaustive()
    }
}

impl EnvironmentWatchManager {
    pub fn new(
        lifecycle: EnvironmentLifecycleService,
        events: mpsc::Sender<EnvironmentLifecycleEvent>,
    ) -> Self {
        Self {
            lifecycle,
            events,
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Starts or replaces the single watch task for a dynamic provider definition.
    pub fn start_provider(&self, provider_id: String) {
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(previous) = tasks.remove(&provider_id) {
            previous.abort();
        }
        if provider_id == STATIC_ENVIRONMENT_PROVIDER_ID {
            return;
        }

        let lifecycle = self.lifecycle.clone();
        let events = self.events.clone();
        let task_provider_id = provider_id.clone();
        let task = tokio::spawn(async move {
            run_provider_watch(lifecycle, task_provider_id, events).await;
        });
        tasks.insert(provider_id, task.abort_handle());
    }

    /// Enqueues an explicit lifecycle change into the same bounded stream as provider watches.
    pub async fn publish_event(
        &self,
        event: EnvironmentLifecycleEvent,
    ) -> Result<(), EnvironmentWatchError> {
        self.events
            .send(event)
            .await
            .map_err(|_| EnvironmentWatchError::EventReceiverClosed)
    }

    /// Stops a provider watch without affecting other configured providers.
    pub fn stop_provider(&self, provider_id: &str) {
        if let Some(task) = self
            .tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(provider_id)
        {
            task.abort();
        }
    }

    /// Stops every provider watch owned by this process.
    pub fn shutdown(&self) {
        let tasks = self
            .tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .map(|(_, task)| task)
            .collect::<Vec<_>>();
        for task in tasks {
            task.abort();
        }
    }
}

async fn run_provider_watch(
    lifecycle: EnvironmentLifecycleService,
    provider_id: String,
    events: mpsc::Sender<EnvironmentLifecycleEvent>,
) {
    let mut retry_delay = INITIAL_RETRY_DELAY;
    loop {
        let runner = tokio::select! {
            () = events.closed() => return,
            result = lifecycle.watch_runner(provider_id.clone()) => result,
        };
        match runner {
            Ok(runner) => match runner.run(events.clone()).await {
                EnvironmentWatchError::EventReceiverClosed => return,
            },
            Err(_) => {
                tokio::select! {
                    () = events.closed() => return,
                    () = tokio::time::sleep(retry_delay) => {}
                }
                retry_delay = retry_delay.saturating_mul(2).min(MAX_RETRY_DELAY);
            }
        }
    }
}

#[cfg(test)]
#[path = "watch_manager_tests.rs"]
mod tests;
