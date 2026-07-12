/// A configured environment provider kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentProviderKind {
    /// The single built-in provider for process-local, statically registered environments.
    Static,
    /// An Ona environment provider.
    Ona,
}

/// Versioned ciphertext for a provider credential.
///
/// The encryption key is deliberately not part of this value and must be stored separately in a
/// keyring or secret manager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncryptedEnvironmentProviderCredential {
    /// Ciphertext format version understood by the caller-owned encryption facility.
    pub version: u32,
    /// Opaque encrypted credential bytes.
    pub ciphertext: Vec<u8>,
}

/// Authentication persisted with a dynamic provider definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoredEnvironmentProviderAuthentication {
    /// A Personal Access Token stored only as versioned ciphertext.
    Pat(EncryptedEnvironmentProviderCredential),
}

/// A persisted dynamic environment provider definition.
///
/// The built-in static provider is synthesized by the service layer and must not be stored as a
/// definition. Provider IDs, kinds, and URLs are immutable after creation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentProviderDefinition {
    /// Stable, opaque identifier assigned by the store.
    pub id: String,
    /// User-visible, mutable display name.
    pub name: String,
    /// Lowercased name used for stable ordering and uniqueness checks.
    pub normalized_name: String,
    /// Immutable dynamic provider kind.
    pub kind: EnvironmentProviderKind,
    /// Immutable, resolved and normalized control-plane URL.
    pub url: String,
    /// Encrypted provider authentication.
    pub authentication: StoredEnvironmentProviderAuthentication,
}

/// Values used to create a dynamic provider definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateEnvironmentProviderParams {
    /// User-visible display name.
    pub name: String,
    /// Dynamic provider kind. [`EnvironmentProviderKind::Static`] is invalid for persistence.
    pub kind: EnvironmentProviderKind,
    /// Resolved and normalized control-plane URL.
    pub url: String,
    /// Encrypted provider authentication.
    pub authentication: StoredEnvironmentProviderAuthentication,
}

/// Mutable fields of an existing dynamic provider definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateEnvironmentProviderParams {
    /// Stable provider identifier.
    pub id: String,
    /// Replacement display name, or `None` to preserve the current name.
    pub name: Option<String>,
    /// Replacement encrypted authentication, or `None` to preserve the current credential.
    pub authentication: Option<StoredEnvironmentProviderAuthentication>,
}

/// Cursor-paginated provider list request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListEnvironmentProvidersParams {
    /// Store-owned opaque cursor returned by a previous page.
    pub cursor: Option<String>,
    /// Maximum number of definitions to return.
    pub limit: usize,
}

/// Cursor-paginated dynamic provider definitions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentProviderPage {
    /// Definitions in normalized-name and provider-ID order.
    pub data: Vec<EnvironmentProviderDefinition>,
    /// Store-owned cursor for the next page, if another page exists.
    pub next_cursor: Option<String>,
}

/// Returns the canonical representation used for case-insensitive provider-name comparisons.
pub fn normalize_environment_provider_name(name: &str) -> String {
    name.to_lowercase()
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod tests;
