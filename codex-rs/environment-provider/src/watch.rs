use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::mpsc;

use crate::EnvironmentLifecycleEvent;
use crate::EnvironmentProviderAdapter;
use crate::EnvironmentProviderAdapterError;
use crate::EnvironmentReconciler;

const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Error that permanently stops an environment provider watch runner.
#[derive(Debug, thiserror::Error)]
pub enum EnvironmentWatchError {
    #[error("environment lifecycle event receiver closed")]
    EventReceiverClosed,
}

/// Reconciles and consumes the single event watch for one configured provider definition.
///
/// Every connection attempt performs a complete reconciliation before opening the watch. Provider
/// failures and ended streams retry with capped exponential backoff; dropping the bounded event
/// receiver permanently stops the runner.
pub struct EnvironmentWatchRunner {
    adapter: Arc<dyn EnvironmentProviderAdapter>,
    reconciler: EnvironmentReconciler,
}

impl std::fmt::Debug for EnvironmentWatchRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvironmentWatchRunner")
            .field("reconciler", &self.reconciler)
            .finish_non_exhaustive()
    }
}

impl EnvironmentWatchRunner {
    pub fn new(provider_id: String, adapter: Arc<dyn EnvironmentProviderAdapter>) -> Self {
        Self {
            adapter,
            reconciler: EnvironmentReconciler::new(provider_id),
        }
    }

    /// Runs until the lifecycle event receiver closes.
    pub async fn run(
        mut self,
        events: mpsc::Sender<EnvironmentLifecycleEvent>,
    ) -> EnvironmentWatchError {
        let mut retry_delay = INITIAL_RETRY_DELAY;
        loop {
            match self.run_connection(&events).await {
                Ok(()) => {
                    retry_delay = INITIAL_RETRY_DELAY;
                }
                Err(WatchConnectionError::Provider(_)) => {
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = retry_delay.saturating_mul(2).min(MAX_RETRY_DELAY);
                }
                Err(WatchConnectionError::EventReceiverClosed) => {
                    return EnvironmentWatchError::EventReceiverClosed;
                }
            }
        }
    }

    async fn run_connection(
        &mut self,
        events: &mpsc::Sender<EnvironmentLifecycleEvent>,
    ) -> Result<(), WatchConnectionError> {
        for event in self.reconciler.reconcile(&self.adapter).await? {
            send_event(events, event).await?;
        }
        let mut watch = self.adapter.watch().await?;
        while let Some(event) = watch.next().await {
            if let Some(event) = self.reconciler.apply_event(&self.adapter, event?).await? {
                send_event(events, event).await?;
            }
        }
        Err(EnvironmentProviderAdapterError::Unavailable {
            message: "environment provider watch ended".to_string(),
        }
        .into())
    }
}

#[derive(Debug, thiserror::Error)]
enum WatchConnectionError {
    #[error(transparent)]
    Provider(#[from] EnvironmentProviderAdapterError),
    #[error("environment lifecycle event receiver closed")]
    EventReceiverClosed,
}

async fn send_event(
    events: &mpsc::Sender<EnvironmentLifecycleEvent>,
    event: EnvironmentLifecycleEvent,
) -> Result<(), WatchConnectionError> {
    events
        .send(event)
        .await
        .map_err(|_| WatchConnectionError::EventReceiverClosed)
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
