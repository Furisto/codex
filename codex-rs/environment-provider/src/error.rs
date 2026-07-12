/// Result type returned by environment provider store operations.
pub type EnvironmentProviderStoreResult<T> = Result<T, EnvironmentProviderStoreError>;

/// Error type shared by environment provider store implementations.
#[derive(Debug, thiserror::Error)]
pub enum EnvironmentProviderStoreError {
    /// The requested provider definition does not exist in this store.
    #[error("environment provider {provider_id} not found")]
    ProviderNotFound {
        /// Provider ID requested by the caller.
        provider_id: String,
    },

    /// The requested display name conflicts with another provider case-insensitively.
    #[error("environment provider name {name:?} is already in use")]
    NameConflict {
        /// Conflicting display name supplied by the caller.
        name: String,
    },

    /// The caller supplied invalid request data.
    #[error("invalid environment provider store request: {message}")]
    InvalidRequest {
        /// User-facing explanation of the invalid request.
        message: String,
    },

    /// The supplied list cursor cannot be decoded by this store.
    #[error("invalid environment provider list cursor")]
    InvalidCursor,

    /// The backing store is temporarily or permanently unavailable.
    #[error("environment provider store unavailable: {message}")]
    Unavailable {
        /// User-facing explanation of the storage failure.
        message: String,
    },

    /// Catch-all for implementation failures that do not fit a more specific category.
    #[error("environment provider store internal error: {message}")]
    Internal {
        /// User-facing explanation of the implementation failure.
        message: String,
    },
}
