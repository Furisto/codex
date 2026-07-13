use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;

use crate::CreateEnvironmentParams;
use crate::DeleteEnvironmentParams;
use crate::Environment;
use crate::EnvironmentConnection;
use crate::EnvironmentListPage;
use crate::EnvironmentProviderEvent;
use crate::ListEnvironmentsParams;
use crate::ReadEnvironmentParams;
use crate::ResolvedEnvironmentProviderDefinition;

/// Result returned by environment provider adapter operations.
pub type EnvironmentProviderAdapterResult<T> = Result<T, EnvironmentProviderAdapterError>;

/// Future returned by [`EnvironmentProviderAdapter`] and factory operations.
pub type EnvironmentProviderAdapterFuture<'a, T> =
    Pin<Box<dyn Future<Output = EnvironmentProviderAdapterResult<T>> + Send + 'a>>;

/// Event stream returned by [`EnvironmentProviderAdapter::watch`].
pub type EnvironmentProviderWatch =
    Pin<Box<dyn Stream<Item = EnvironmentProviderAdapterResult<EnvironmentProviderEvent>> + Send>>;

/// Error shared by environment provider adapters.
#[derive(Debug, thiserror::Error)]
pub enum EnvironmentProviderAdapterError {
    /// The requested provider-owned environment does not exist.
    #[error("environment {environment_id} not found")]
    EnvironmentNotFound { environment_id: String },

    /// The caller supplied invalid provider-native input.
    #[error("invalid environment provider operation: {message}")]
    InvalidRequest { message: String },

    /// The external provider or its authentication is unavailable.
    #[error("environment provider unavailable: {message}")]
    Unavailable { message: String },

    /// Catch-all for invalid provider responses and implementation failures.
    #[error("environment provider internal error: {message}")]
    Internal { message: String },
}

/// Provider-specific lifecycle boundary for authoritative external environments.
///
/// Implementations map the common source and resource-class request into provider-native APIs,
/// return only environments tagged as owned by their configured Codex provider definition, and
/// preserve provider-native cursors. A watch reports resource IDs; orchestration performs reads
/// and reconciliation so provider event payload details do not leak through this boundary.
pub trait EnvironmentProviderAdapter: Send + Sync {
    /// Starts asynchronous environment provisioning and returns its initial provider record.
    fn create_environment(
        &self,
        params: CreateEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, Environment>;

    /// Reads the current authoritative provider record.
    fn read_environment(
        &self,
        params: ReadEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, Environment>;

    /// Lists provider-owned environments using an opaque provider-native cursor.
    fn list_environments(
        &self,
        params: ListEnvironmentsParams,
    ) -> EnvironmentProviderAdapterFuture<'_, EnvironmentListPage>;

    /// Requests asynchronous deletion of a provider-owned environment.
    fn delete_environment(
        &self,
        params: DeleteEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, ()>;

    /// Resolves fresh exec-server connection material for a ready environment.
    fn connection(
        &self,
        params: ReadEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, EnvironmentConnection>;

    /// Opens the provider's single event watch for this configured definition.
    fn watch(&self) -> EnvironmentProviderAdapterFuture<'_, EnvironmentProviderWatch>;
}

/// Constructs a provider adapter from a persisted definition and decrypted authentication.
///
/// Implementations select the concrete adapter by provider kind. Construction must not cause
/// provider definitions to become authoritative for environment state; all lifecycle reads and
/// lists still go through the returned adapter.
pub trait EnvironmentProviderAdapterFactory: Send + Sync {
    /// Creates the configured adapter used for lifecycle operations and its single watch.
    fn create_adapter(
        &self,
        definition: ResolvedEnvironmentProviderDefinition,
    ) -> EnvironmentProviderAdapterFuture<'_, Arc<dyn EnvironmentProviderAdapter>>;
}

#[derive(Debug)]
pub(crate) struct UnavailableEnvironmentProviderAdapterFactory;

impl EnvironmentProviderAdapterFactory for UnavailableEnvironmentProviderAdapterFactory {
    fn create_adapter(
        &self,
        _definition: ResolvedEnvironmentProviderDefinition,
    ) -> EnvironmentProviderAdapterFuture<'_, Arc<dyn EnvironmentProviderAdapter>> {
        Box::pin(async {
            Err(EnvironmentProviderAdapterError::Unavailable {
                message: "no adapter is implemented for this provider kind".to_string(),
            })
        })
    }
}
