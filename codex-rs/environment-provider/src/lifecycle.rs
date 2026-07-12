use std::sync::Arc;

use crate::CreateEnvironmentParams;
use crate::DeleteEnvironmentParams;
use crate::Environment;
use crate::EnvironmentListPage;
use crate::EnvironmentProviderAdapterError;
use crate::EnvironmentProviderAdapterFactory;
use crate::EnvironmentProviderAdapterPool;
use crate::EnvironmentProviderDeletionService;
use crate::EnvironmentProviderService;
use crate::EnvironmentProviderServiceError;
use crate::ListEnvironmentsParams;
use crate::ProviderOperationLocks;
use crate::ReadEnvironmentParams;
use crate::ResolveEnvironmentProviderAdapterError;
use crate::STATIC_ENVIRONMENT_PROVIDER_ID;

/// Result returned by dynamic environment lifecycle orchestration.
pub type EnvironmentLifecycleServiceResult<T> = Result<T, EnvironmentLifecycleServiceError>;

/// Error returned by dynamic environment lifecycle orchestration.
#[derive(Debug, thiserror::Error)]
pub enum EnvironmentLifecycleServiceError {
    /// Provider configuration storage, validation, or credential resolution failed.
    #[error(transparent)]
    Configuration(#[from] EnvironmentProviderServiceError),
    /// The configured provider rejected or failed the lifecycle operation.
    #[error(transparent)]
    Provider(#[from] EnvironmentProviderAdapterError),
}

/// Runs authoritative dynamic environment operations through one cached adapter per provider.
///
/// Create and delete operations are serialized per provider definition. Reads and lists remain
/// concurrent and always query the provider adapter rather than a local environment projection.
#[derive(Clone, Debug)]
pub struct EnvironmentLifecycleService {
    configuration: EnvironmentProviderService,
    adapters: EnvironmentProviderAdapterPool,
    operation_locks: ProviderOperationLocks,
}

impl EnvironmentLifecycleService {
    /// Creates lifecycle orchestration backed by the configured provider adapter factory.
    pub fn new(
        configuration: EnvironmentProviderService,
        adapter_factory: Arc<dyn EnvironmentProviderAdapterFactory>,
    ) -> Self {
        let adapters = EnvironmentProviderAdapterPool::new(configuration.clone(), adapter_factory);
        Self {
            configuration,
            adapters,
            operation_locks: ProviderOperationLocks::default(),
        }
    }

    /// Creates provider-deletion orchestration sharing adapters and mutation serialization.
    pub fn deletion_service(&self) -> EnvironmentProviderDeletionService {
        EnvironmentProviderDeletionService::from_runtime(
            self.configuration.clone(),
            self.adapters.clone(),
            self.operation_locks.clone(),
        )
    }

    /// Invalidates a provider adapter after its PAT changes or its definition is removed.
    pub fn invalidate_provider(&self, provider_id: &str) {
        self.adapters.invalidate(provider_id);
    }

    /// Starts asynchronous provisioning and returns the provider's initial complete record.
    pub async fn create_environment(
        &self,
        provider_id: String,
        params: CreateEnvironmentParams,
    ) -> EnvironmentLifecycleServiceResult<Environment> {
        reject_static_mutation(&provider_id, "created")?;
        let _guard = self.operation_locks.lock(&provider_id).await;
        let adapter = self.adapters.adapter(&provider_id).await?;
        let environment = adapter.create_environment(params).await?;
        validate_environment_provider(&environment, &provider_id)?;
        Ok(environment)
    }

    /// Reads an authoritative dynamic environment record.
    pub async fn read_environment(
        &self,
        provider_id: String,
        params: ReadEnvironmentParams,
    ) -> EnvironmentLifecycleServiceResult<Environment> {
        let adapter = self.adapters.adapter(&provider_id).await?;
        let environment = adapter.read_environment(params).await?;
        validate_environment_provider(&environment, &provider_id)?;
        Ok(environment)
    }

    /// Lists authoritative provider-owned environments using the provider's opaque cursor.
    pub async fn list_environments(
        &self,
        provider_id: String,
        params: ListEnvironmentsParams,
    ) -> EnvironmentLifecycleServiceResult<EnvironmentListPage> {
        if params.limit == 0 {
            return Err(EnvironmentProviderAdapterError::InvalidRequest {
                message: "limit must be greater than zero".to_string(),
            }
            .into());
        }
        let adapter = self.adapters.adapter(&provider_id).await?;
        let page = adapter.list_environments(params).await?;
        for environment in &page.data {
            validate_environment_provider(environment, &provider_id)?;
        }
        Ok(page)
    }

    /// Requests asynchronous deletion after serializing mutations for the provider.
    pub async fn delete_environment(
        &self,
        provider_id: String,
        params: DeleteEnvironmentParams,
    ) -> EnvironmentLifecycleServiceResult<()> {
        reject_static_mutation(&provider_id, "deleted")?;
        let _guard = self.operation_locks.lock(&provider_id).await;
        let adapter = self.adapters.adapter(&provider_id).await?;
        adapter.delete_environment(params).await?;
        Ok(())
    }
}

impl From<ResolveEnvironmentProviderAdapterError> for EnvironmentLifecycleServiceError {
    fn from(error: ResolveEnvironmentProviderAdapterError) -> Self {
        match error {
            ResolveEnvironmentProviderAdapterError::Configuration(error) => {
                EnvironmentLifecycleServiceError::Configuration(error)
            }
            ResolveEnvironmentProviderAdapterError::Adapter(error) => {
                EnvironmentLifecycleServiceError::Provider(error)
            }
        }
    }
}

fn reject_static_mutation(
    provider_id: &str,
    operation: &str,
) -> EnvironmentLifecycleServiceResult<()> {
    if provider_id == STATIC_ENVIRONMENT_PROVIDER_ID {
        return Err(EnvironmentProviderServiceError::InvalidRequest {
            message: format!("static environments cannot be {operation} through this API"),
        }
        .into());
    }
    Ok(())
}

fn validate_environment_provider(
    environment: &Environment,
    provider_id: &str,
) -> EnvironmentLifecycleServiceResult<()> {
    if environment.environment_ref.provider_id != provider_id {
        return Err(EnvironmentProviderAdapterError::Internal {
            message: format!(
                "provider {provider_id} returned environment {} owned by provider {}",
                environment.environment_ref.environment_id, environment.environment_ref.provider_id
            ),
        }
        .into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
