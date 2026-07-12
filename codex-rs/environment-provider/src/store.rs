use std::future::Future;
use std::pin::Pin;

use crate::CreateEnvironmentProviderParams;
use crate::EnvironmentProviderDefinition;
use crate::EnvironmentProviderPage;
use crate::EnvironmentProviderStoreResult;
use crate::ListEnvironmentProvidersParams;
use crate::UpdateEnvironmentProviderParams;

/// Future returned by [`EnvironmentProviderStore`] operations.
pub type EnvironmentProviderStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = EnvironmentProviderStoreResult<T>> + Send + 'a>>;

/// Storage-neutral persistence boundary for dynamic environment provider definitions.
///
/// Implementations assign immutable opaque IDs, persist resolved URLs and encrypted credentials,
/// and enforce case-insensitive name uniqueness transactionally. The built-in static provider and
/// provider-owned environments are intentionally outside this store: callers synthesize the
/// static provider, while environment state remains authoritative in each external provider.
pub trait EnvironmentProviderStore: Send + Sync {
    /// Creates and returns a dynamic provider definition.
    ///
    /// Implementations must reject [`crate::EnvironmentProviderKind::Static`] and use
    /// [`crate::normalize_environment_provider_name`] for the persisted normalized name and the
    /// transactional uniqueness check.
    fn create_provider(
        &self,
        params: CreateEnvironmentProviderParams,
    ) -> EnvironmentProviderStoreFuture<'_, EnvironmentProviderDefinition>;

    /// Reads a dynamic provider definition by its opaque ID.
    fn read_provider(
        &self,
        provider_id: String,
    ) -> EnvironmentProviderStoreFuture<'_, EnvironmentProviderDefinition>;

    /// Lists dynamic definitions in normalized-name and provider-ID order.
    ///
    /// Cursors are opaque outside the implementation. The synthesized static provider is not
    /// included in this page.
    fn list_providers(
        &self,
        params: ListEnvironmentProvidersParams,
    ) -> EnvironmentProviderStoreFuture<'_, EnvironmentProviderPage>;

    /// Updates only the mutable name and encrypted authentication fields.
    ///
    /// If a replacement name is supplied, implementations must update its normalized form and
    /// enforce uniqueness in the same transaction as the display name update.
    fn update_provider(
        &self,
        params: UpdateEnvironmentProviderParams,
    ) -> EnvironmentProviderStoreFuture<'_, EnvironmentProviderDefinition>;

    /// Deletes a dynamic provider definition by its opaque ID.
    ///
    /// The caller is responsible for enforcing environment cleanup semantics before invoking this
    /// storage operation.
    fn delete_provider(&self, provider_id: String) -> EnvironmentProviderStoreFuture<'_, ()>;
}
