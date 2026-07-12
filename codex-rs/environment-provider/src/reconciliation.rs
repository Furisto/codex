use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;

use crate::Environment;
use crate::EnvironmentListPage;
use crate::EnvironmentProviderAdapter;
use crate::EnvironmentProviderAdapterError;
use crate::EnvironmentProviderAdapterResult;
use crate::EnvironmentProviderEvent;
use crate::EnvironmentRef;
use crate::ListEnvironmentsParams;
use crate::ReadEnvironmentParams;

const RECONCILIATION_PAGE_SIZE: usize = 100;

/// A normalized lifecycle change produced by provider reconciliation or watch handling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvironmentLifecycleEvent {
    Created(Environment),
    Updated(Environment),
    Deleted(EnvironmentRef),
}

/// Maintains one provider's in-memory last-seen projection for lifecycle event diffing.
///
/// The projection is never durable or authoritative. Full reconciliation replaces it only after
/// every provider page succeeds, allowing watch reconnects to recover missed events without
/// discarding the last known-good state on transient failures.
#[derive(Debug)]
pub struct EnvironmentReconciler {
    provider_id: String,
    environments: BTreeMap<String, Environment>,
}

impl EnvironmentReconciler {
    pub fn new(provider_id: String) -> Self {
        Self {
            provider_id,
            environments: BTreeMap::new(),
        }
    }

    /// Performs a complete authoritative list and returns deterministic projection changes.
    pub async fn reconcile(
        &mut self,
        adapter: &Arc<dyn EnvironmentProviderAdapter>,
    ) -> EnvironmentProviderAdapterResult<Vec<EnvironmentLifecycleEvent>> {
        let next = self.list_all(adapter).await?;
        let mut events = Vec::new();
        for (environment_id, environment) in &next {
            match self.environments.get(environment_id) {
                None => events.push(EnvironmentLifecycleEvent::Created(environment.clone())),
                Some(previous) if previous != environment => {
                    events.push(EnvironmentLifecycleEvent::Updated(environment.clone()));
                }
                Some(_) => {}
            }
        }
        for environment_id in self.environments.keys() {
            if !next.contains_key(environment_id) {
                events.push(EnvironmentLifecycleEvent::Deleted(EnvironmentRef {
                    provider_id: self.provider_id.clone(),
                    environment_id: environment_id.clone(),
                }));
            }
        }
        self.environments = next;
        Ok(events)
    }

    /// Applies one normalized provider watch signal and returns any resulting lifecycle change.
    pub async fn apply_event(
        &mut self,
        adapter: &Arc<dyn EnvironmentProviderAdapter>,
        event: EnvironmentProviderEvent,
    ) -> EnvironmentProviderAdapterResult<Option<EnvironmentLifecycleEvent>> {
        match event {
            EnvironmentProviderEvent::Changed { environment_id } => {
                let environment = match adapter
                    .read_environment(ReadEnvironmentParams {
                        environment_id: environment_id.clone(),
                    })
                    .await
                {
                    Ok(environment) => environment,
                    Err(EnvironmentProviderAdapterError::EnvironmentNotFound { .. }) => {
                        return Ok(self.remove_environment(environment_id));
                    }
                    Err(error) => return Err(error),
                };
                self.validate_environment(&environment)?;
                let event = match self.environments.get(&environment_id) {
                    None => Some(EnvironmentLifecycleEvent::Created(environment.clone())),
                    Some(previous) if previous != &environment => {
                        Some(EnvironmentLifecycleEvent::Updated(environment.clone()))
                    }
                    Some(_) => None,
                };
                self.environments.insert(environment_id, environment);
                Ok(event)
            }
            EnvironmentProviderEvent::Deleted { environment_id } => {
                Ok(self.remove_environment(environment_id))
            }
        }
    }

    async fn list_all(
        &self,
        adapter: &Arc<dyn EnvironmentProviderAdapter>,
    ) -> EnvironmentProviderAdapterResult<BTreeMap<String, Environment>> {
        let mut cursor = None;
        let mut seen_cursors = BTreeSet::new();
        let mut environments = BTreeMap::new();
        loop {
            let EnvironmentListPage { data, next_cursor } = adapter
                .list_environments(ListEnvironmentsParams {
                    cursor: cursor.clone(),
                    limit: RECONCILIATION_PAGE_SIZE,
                })
                .await?;
            for environment in data {
                self.validate_environment(&environment)?;
                let environment_id = environment.environment_ref.environment_id.clone();
                if environments
                    .insert(environment_id.clone(), environment)
                    .is_some()
                {
                    return Err(EnvironmentProviderAdapterError::Internal {
                        message: format!(
                            "provider {} returned duplicate environment {environment_id}",
                            self.provider_id
                        ),
                    });
                }
            }
            let Some(next_cursor) = next_cursor else {
                return Ok(environments);
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(EnvironmentProviderAdapterError::Internal {
                    message: format!(
                        "provider {} returned a repeated environment cursor",
                        self.provider_id
                    ),
                });
            }
            cursor = Some(next_cursor);
        }
    }

    fn validate_environment(
        &self,
        environment: &Environment,
    ) -> EnvironmentProviderAdapterResult<()> {
        if environment.environment_ref.provider_id != self.provider_id {
            return Err(EnvironmentProviderAdapterError::Internal {
                message: format!(
                    "provider {} returned environment {} owned by provider {}",
                    self.provider_id,
                    environment.environment_ref.environment_id,
                    environment.environment_ref.provider_id
                ),
            });
        }
        Ok(())
    }

    fn remove_environment(&mut self, environment_id: String) -> Option<EnvironmentLifecycleEvent> {
        self.environments.remove(&environment_id).map(|_| {
            EnvironmentLifecycleEvent::Deleted(EnvironmentRef {
                provider_id: self.provider_id.clone(),
                environment_id,
            })
        })
    }
}

#[cfg(test)]
#[path = "reconciliation_tests.rs"]
mod tests;
