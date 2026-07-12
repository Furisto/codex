use std::collections::BTreeSet;
use std::sync::Arc;

use futures::StreamExt;
use futures::stream;

use crate::DeleteEnvironmentParams;
use crate::DeleteEnvironmentProviderMode;
use crate::DeleteEnvironmentProviderParams;
use crate::EnvironmentProviderAdapter;
use crate::EnvironmentProviderAdapterError;
use crate::EnvironmentProviderAdapterFactory;
use crate::EnvironmentProviderAdapterPool;
use crate::EnvironmentProviderCleanup;
use crate::EnvironmentProviderCleanupStatus;
use crate::EnvironmentProviderService;
use crate::EnvironmentProviderServiceError;
use crate::ListEnvironmentsParams;
use crate::ProviderOperationLocks;
use crate::ResolveEnvironmentProviderAdapterError;
use crate::STATIC_ENVIRONMENT_PROVIDER_ID;
use crate::UnavailableEnvironmentProviderAdapterFactory;

const CLEANUP_LIST_PAGE_SIZE: usize = 100;
const CLEANUP_DELETE_CONCURRENCY: usize = 8;

/// Result returned by provider deletion orchestration.
pub type EnvironmentProviderDeletionServiceResult<T> = Result<T, EnvironmentProviderDeletionError>;

/// Error returned by provider deletion orchestration.
#[derive(Debug, thiserror::Error)]
pub enum EnvironmentProviderDeletionError {
    /// Provider configuration storage, validation, or credential resolution failed.
    #[error(transparent)]
    Configuration(#[from] EnvironmentProviderServiceError),

    /// Normal deletion found at least one provider-owned environment.
    #[error("environment provider {provider_id} still has environments")]
    ProviderNotEmpty { provider_id: String },

    /// Normal deletion could not construct or query the authoritative provider adapter.
    #[error("environment provider cleanup unavailable: {message}")]
    CleanupUnavailable { message: String },
}

/// Enforces normal and forced cleanup policy before deleting provider definitions.
#[derive(Clone)]
pub struct EnvironmentProviderDeletionService {
    configuration: EnvironmentProviderService,
    adapters: EnvironmentProviderAdapterPool,
    operation_locks: ProviderOperationLocks,
}

impl std::fmt::Debug for EnvironmentProviderDeletionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvironmentProviderDeletionService")
            .field("configuration", &self.configuration)
            .finish_non_exhaustive()
    }
}

impl EnvironmentProviderDeletionService {
    /// Creates deletion orchestration backed by the configured adapter factory.
    pub fn new(
        configuration: EnvironmentProviderService,
        adapter_factory: Arc<dyn EnvironmentProviderAdapterFactory>,
    ) -> Self {
        let adapters = EnvironmentProviderAdapterPool::new(
            configuration.clone(),
            Arc::clone(&adapter_factory),
        );
        Self::from_runtime(configuration, adapters, ProviderOperationLocks::default())
    }

    pub(crate) fn from_runtime(
        configuration: EnvironmentProviderService,
        adapters: EnvironmentProviderAdapterPool,
        operation_locks: ProviderOperationLocks,
    ) -> Self {
        Self {
            configuration,
            adapters,
            operation_locks,
        }
    }

    /// Creates deletion orchestration that has no concrete dynamic adapters yet.
    ///
    /// Normal deletion fails closed. Forced deletion reports unknown cleanup and still attempts to
    /// remove the provider definition.
    pub fn without_adapters(configuration: EnvironmentProviderService) -> Self {
        Self::new(
            configuration,
            Arc::new(UnavailableEnvironmentProviderAdapterFactory),
        )
    }

    /// Deletes a provider definition according to normal or forced cleanup policy.
    pub async fn delete_provider(
        &self,
        params: DeleteEnvironmentProviderParams,
    ) -> EnvironmentProviderDeletionServiceResult<EnvironmentProviderCleanup> {
        if params.provider_id == STATIC_ENVIRONMENT_PROVIDER_ID {
            return Err(EnvironmentProviderServiceError::InvalidRequest {
                message: "the built-in static environment provider cannot be deleted".to_string(),
            }
            .into());
        }
        let _guard = self.operation_locks.lock(&params.provider_id).await;
        match params.mode {
            DeleteEnvironmentProviderMode::Normal => {
                self.delete_provider_normally(params.provider_id).await
            }
            DeleteEnvironmentProviderMode::Force => {
                self.force_delete_provider(params.provider_id).await
            }
        }
    }

    async fn delete_provider_normally(
        &self,
        provider_id: String,
    ) -> EnvironmentProviderDeletionServiceResult<EnvironmentProviderCleanup> {
        let adapter = self
            .adapters
            .adapter(&provider_id)
            .await
            .map_err(deletion_adapter_unavailable)?;
        let page = adapter
            .list_environments(ListEnvironmentsParams {
                cursor: None,
                limit: 1,
            })
            .await
            .map_err(cleanup_unavailable)?;
        if !page.data.is_empty() || page.next_cursor.is_some() {
            return Err(EnvironmentProviderDeletionError::ProviderNotEmpty { provider_id });
        }
        self.configuration
            .delete_provider_definition_after_cleanup(provider_id.clone())
            .await?;
        self.adapters.invalidate(&provider_id);
        Ok(EnvironmentProviderCleanup {
            status: EnvironmentProviderCleanupStatus::Complete,
            failed_environment_ids: Vec::new(),
        })
    }

    async fn force_delete_provider(
        &self,
        provider_id: String,
    ) -> EnvironmentProviderDeletionServiceResult<EnvironmentProviderCleanup> {
        let adapter = match self.adapters.adapter(&provider_id).await {
            Ok(adapter) => Some(adapter),
            Err(ResolveEnvironmentProviderAdapterError::Configuration(
                EnvironmentProviderServiceError::ProviderNotFound { provider_id },
            )) => {
                return Err(
                    EnvironmentProviderServiceError::ProviderNotFound { provider_id }.into(),
                );
            }
            Err(_) => None,
        };
        let (enumeration_complete, environment_ids) = match adapter.as_ref() {
            Some(adapter) => enumerate_environment_ids(adapter).await,
            None => (false, Vec::new()),
        };
        let failed_environment_ids = match adapter {
            Some(adapter) => delete_environments(adapter, environment_ids).await,
            None => Vec::new(),
        };
        let status = if !enumeration_complete {
            EnvironmentProviderCleanupStatus::Unknown
        } else if failed_environment_ids.is_empty() {
            EnvironmentProviderCleanupStatus::Complete
        } else {
            EnvironmentProviderCleanupStatus::Partial
        };

        self.configuration
            .delete_provider_definition_after_cleanup(provider_id.clone())
            .await?;
        self.adapters.invalidate(&provider_id);
        Ok(EnvironmentProviderCleanup {
            status,
            failed_environment_ids,
        })
    }
}

async fn enumerate_environment_ids(
    adapter: &Arc<dyn EnvironmentProviderAdapter>,
) -> (bool, Vec<String>) {
    let mut cursor = None;
    let mut seen_cursors = BTreeSet::new();
    let mut environment_ids = BTreeSet::new();
    loop {
        let page = match adapter
            .list_environments(ListEnvironmentsParams {
                cursor: cursor.clone(),
                limit: CLEANUP_LIST_PAGE_SIZE,
            })
            .await
        {
            Ok(page) => page,
            Err(_) => return (false, environment_ids.into_iter().collect()),
        };
        environment_ids.extend(
            page.data
                .into_iter()
                .map(|environment| environment.environment_ref.environment_id),
        );
        let Some(next_cursor) = page.next_cursor else {
            return (true, environment_ids.into_iter().collect());
        };
        if !seen_cursors.insert(next_cursor.clone()) {
            return (false, environment_ids.into_iter().collect());
        }
        cursor = Some(next_cursor);
    }
}

async fn delete_environments(
    adapter: Arc<dyn EnvironmentProviderAdapter>,
    environment_ids: Vec<String>,
) -> Vec<String> {
    let mut failed_environment_ids = stream::iter(environment_ids)
        .map(|environment_id| {
            let adapter = Arc::clone(&adapter);
            async move {
                adapter
                    .delete_environment(DeleteEnvironmentParams {
                        environment_id: environment_id.clone(),
                    })
                    .await
                    .err()
                    .map(|_| environment_id)
            }
        })
        .buffer_unordered(CLEANUP_DELETE_CONCURRENCY)
        .filter_map(futures::future::ready)
        .collect::<Vec<_>>()
        .await;
    failed_environment_ids.sort();
    failed_environment_ids
}

fn cleanup_unavailable(error: EnvironmentProviderAdapterError) -> EnvironmentProviderDeletionError {
    EnvironmentProviderDeletionError::CleanupUnavailable {
        message: error.to_string(),
    }
}

fn deletion_adapter_unavailable(
    error: ResolveEnvironmentProviderAdapterError,
) -> EnvironmentProviderDeletionError {
    match error {
        ResolveEnvironmentProviderAdapterError::Configuration(error) => {
            EnvironmentProviderDeletionError::Configuration(error)
        }
        ResolveEnvironmentProviderAdapterError::Adapter(error) => cleanup_unavailable(error),
    }
}

#[cfg(test)]
#[path = "deletion_tests.rs"]
mod tests;
