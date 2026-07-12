/// A configured environment provider kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentProviderKind {
    /// The single built-in provider for process-local, statically registered environments.
    Static,
    /// An Ona environment provider.
    Ona,
}

/// Provider-qualified identity of a dynamic environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentRef {
    pub provider_id: String,
    pub environment_id: String,
}

/// Structured source used to provision an environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentSource {
    pub repository_url: String,
    /// Opaque provider-interpreted Git ref.
    pub git_ref: String,
}

/// Common environment lifecycle phases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentPhase {
    Unknown,
    Creating,
    Starting,
    Running,
    Updating,
    Stopping,
    Stopped,
    Deleting,
    Deleted,
}

/// Current common environment lifecycle status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentStatus {
    pub phase: EnvironmentPhase,
    pub error: Option<String>,
}

/// Minimal authoritative environment record returned by provider adapters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Environment {
    pub environment_ref: EnvironmentRef,
    pub source: EnvironmentSource,
    /// Opaque provider-native size or machine class.
    pub resource_class: String,
    pub status: EnvironmentStatus,
}

/// Common environment provisioning input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateEnvironmentParams {
    pub source: EnvironmentSource,
    pub resource_class: String,
}

/// Common environment read input using a provider-native ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadEnvironmentParams {
    pub environment_id: String,
}

/// Common environment deletion input using a provider-native ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteEnvironmentParams {
    pub environment_id: String,
}

/// Provider-owned cursor pagination input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListEnvironmentsParams {
    pub cursor: Option<String>,
    pub limit: usize,
}

/// Provider-owned cursor page of authoritative environments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentListPage {
    pub data: Vec<Environment>,
    pub next_cursor: Option<String>,
}

/// Resource signal emitted by a provider watch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvironmentProviderEvent {
    /// The environment should be read to obtain its latest complete record.
    Changed { environment_id: String },
    /// The provider reports that the environment has been deleted.
    Deleted { environment_id: String },
}

/// A Personal Access Token whose debug representation never exposes its value.
#[derive(Clone, PartialEq, Eq)]
pub struct PersonalAccessToken(String);

impl PersonalAccessToken {
    /// Creates a non-empty PAT.
    pub fn new(token: String) -> Result<Self, &'static str> {
        if token.is_empty() {
            Err("personal access token must not be empty")
        } else {
            Ok(Self(token))
        }
    }

    /// Exposes the PAT to an authenticated provider operation.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PersonalAccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PersonalAccessToken([REDACTED])")
    }
}

/// Authentication supplied when creating or updating a provider definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvironmentProviderAuthenticationInput {
    /// Personal Access Token authentication.
    Pat(PersonalAccessToken),
}

/// Redacted authentication metadata returned with a provider definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentProviderAuthentication {
    /// Personal Access Token authentication is configured.
    Pat,
}

/// User-visible environment provider metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentProvider {
    /// Stable opaque provider ID.
    pub id: String,
    /// User-visible display name.
    pub name: String,
    /// Provider kind.
    pub kind: EnvironmentProviderKind,
    /// Resolved control-plane URL, or `None` for the built-in static provider.
    pub url: Option<String>,
    /// Redacted authentication metadata, or `None` for the built-in static provider.
    pub authentication: Option<EnvironmentProviderAuthentication>,
}

/// Provider creation input accepted by [`crate::EnvironmentProviderService`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentProviderServiceCreateParams {
    /// Unique display name.
    pub name: String,
    /// Dynamic provider kind. The built-in static kind is rejected.
    pub kind: EnvironmentProviderKind,
    /// Optional control-plane URL. Provider-specific defaults are resolved before persistence.
    pub url: Option<String>,
    /// Provider authentication.
    pub authentication: EnvironmentProviderAuthenticationInput,
}

/// Provider update input accepted by [`crate::EnvironmentProviderService`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentProviderServiceUpdateParams {
    /// Stable provider ID.
    pub id: String,
    /// Replacement display name, or `None` to preserve it.
    pub name: Option<String>,
    /// Replacement authentication, or `None` to preserve it.
    pub authentication: Option<EnvironmentProviderAuthenticationInput>,
}

/// Cursor-paginated user-visible environment providers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentProviderListPage {
    /// Providers with the built-in static provider first.
    pub data: Vec<EnvironmentProvider>,
    /// Service-owned cursor for the next page, if another page exists.
    pub next_cursor: Option<String>,
}

/// A dynamic provider definition with decrypted authentication for provider operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedEnvironmentProviderDefinition {
    /// Stable opaque provider ID.
    pub id: String,
    /// User-visible display name.
    pub name: String,
    /// Dynamic provider kind.
    pub kind: EnvironmentProviderKind,
    /// Resolved control-plane URL.
    pub url: String,
    /// Decrypted authentication for an authenticated provider operation.
    pub authentication: EnvironmentProviderAuthenticationInput,
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
