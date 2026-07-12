use crate::EnvironmentProviderStoreError;

/// Result returned by environment provider configuration service operations.
pub type EnvironmentProviderServiceResult<T> = Result<T, EnvironmentProviderServiceError>;

/// Error returned by environment provider configuration service operations.
#[derive(Debug, thiserror::Error)]
pub enum EnvironmentProviderServiceError {
    /// The requested dynamic provider does not exist.
    #[error("environment provider {provider_id} not found")]
    ProviderNotFound { provider_id: String },

    /// The requested provider name is already used case-insensitively.
    #[error("environment provider name {name:?} is already in use")]
    NameConflict { name: String },

    /// The caller supplied invalid provider configuration.
    #[error("invalid environment provider request: {message}")]
    InvalidRequest { message: String },

    /// The supplied provider list cursor is invalid.
    #[error("invalid environment provider list cursor")]
    InvalidCursor,

    /// Provider-definition storage is unavailable.
    #[error("environment provider storage unavailable: {message}")]
    StorageUnavailable { message: String },

    /// Provider authentication could not be encrypted or decrypted securely.
    #[error("environment provider credential unavailable: {message}")]
    CredentialUnavailable { message: String },

    /// Catch-all for corrupt persisted data and unexpected implementation failures.
    #[error("environment provider service internal error: {message}")]
    Internal { message: String },
}

impl From<EnvironmentProviderStoreError> for EnvironmentProviderServiceError {
    fn from(error: EnvironmentProviderStoreError) -> Self {
        match error {
            EnvironmentProviderStoreError::ProviderNotFound { provider_id } => {
                Self::ProviderNotFound { provider_id }
            }
            EnvironmentProviderStoreError::NameConflict { name } => Self::NameConflict { name },
            EnvironmentProviderStoreError::InvalidRequest { message } => {
                Self::InvalidRequest { message }
            }
            EnvironmentProviderStoreError::InvalidCursor => Self::InvalidCursor,
            EnvironmentProviderStoreError::Unavailable { message } => {
                Self::StorageUnavailable { message }
            }
            EnvironmentProviderStoreError::Internal { message } => Self::Internal { message },
        }
    }
}
