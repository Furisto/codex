use codex_utils_path_uri::PathUri;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentAddParams {
    pub environment_id: String,
    pub exec_server_url: String,
    /// Optional WebSocket connection timeout. The server default applies when omitted.
    #[ts(type = "number | null")]
    #[ts(optional = nullable)]
    pub connect_timeout_ms: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentAddResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentInfoParams {
    pub environment_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentInfoResponse {
    pub shell: EnvironmentShellInfo,
    /// Default working directory reported by the environment, as a canonical file URI.
    pub cwd: Option<PathUri>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentShellInfo {
    /// Stable shell name, for example `zsh`, `bash`, `powershell`, `sh`, or `cmd`.
    pub name: String,
    /// Target-native shell executable path or command name.
    pub path: String,
}

/// Provider-qualified identity of an environment.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentRef {
    pub provider_id: String,
    pub environment_id: String,
}

/// Structured source used to provision a dynamic environment.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentSource {
    pub repository_url: String,
    /// Opaque provider-interpreted Git ref.
    #[serde(rename = "ref")]
    #[ts(rename = "ref")]
    pub git_ref: String,
}

/// Common environment lifecycle phases.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
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

/// Current environment lifecycle status.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentStatus {
    pub phase: EnvironmentPhase,
    pub error: Option<String>,
}

/// An environment visible through the app-server API.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct Environment {
    #[serde(rename = "ref")]
    #[ts(rename = "ref")]
    pub environment_ref: EnvironmentRef,
    /// Provisioning source, or `null` for a statically registered environment.
    pub source: Option<EnvironmentSource>,
    /// Provider-native size or machine class, or `null` for a static environment.
    pub resource_class: Option<String>,
    pub status: EnvironmentStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentCreateParams {
    pub provider_id: String,
    pub source: EnvironmentSource,
    pub resource_class: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentCreateResponse {
    pub environment: EnvironmentRef,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentReadParams {
    pub provider_id: String,
    pub environment_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentReadResponse {
    pub environment: Environment,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentListParams {
    pub provider_id: String,
    #[ts(optional = nullable)]
    pub cursor: Option<String>,
    #[ts(optional = nullable)]
    pub limit: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentListResponse {
    pub data: Vec<Environment>,
    pub next_cursor: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentDeleteParams {
    pub provider_id: String,
    pub environment_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentDeleteResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentCreatedNotification {
    pub environment: Environment,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentUpdatedNotification {
    pub environment: Environment,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentDeletedNotification {
    /// Canonical `providerId/environmentId` identity.
    pub environment_id: String,
}

/// Supported environment provider kinds.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum EnvironmentProviderKind {
    /// The single built-in provider for statically registered environments.
    Static,
    /// An Ona environment provider.
    Ona,
}

/// Authentication supplied when creating or updating an environment provider.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum EnvironmentProviderAuthenticationParams {
    /// Personal Access Token authentication.
    Pat { token: String },
}

/// Redacted environment provider authentication metadata.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum EnvironmentProviderAuthentication {
    /// Personal Access Token authentication is configured.
    Pat,
}

/// An environment provider visible through the app-server API.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentProvider {
    pub id: String,
    pub name: String,
    pub kind: EnvironmentProviderKind,
    /// Resolved control-plane URL, or `null` for the built-in static provider.
    pub url: Option<String>,
    /// Redacted authentication metadata, or `null` for the built-in static provider.
    pub authentication: Option<EnvironmentProviderAuthentication>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentProviderCreateParams {
    pub name: String,
    pub kind: EnvironmentProviderKind,
    /// Optional provider URL. Omission selects the provider-specific default when available.
    #[ts(optional = nullable)]
    pub url: Option<String>,
    pub authentication: EnvironmentProviderAuthenticationParams,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentProviderCreateResponse {
    pub provider: EnvironmentProvider,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentProviderUpdateParams {
    pub provider_id: String,
    #[ts(optional = nullable)]
    pub name: Option<String>,
    #[ts(optional = nullable)]
    pub authentication: Option<EnvironmentProviderAuthenticationParams>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentProviderUpdateResponse {
    pub provider: EnvironmentProvider,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentProviderListParams {
    #[ts(optional = nullable)]
    pub cursor: Option<String>,
    #[ts(optional = nullable)]
    pub limit: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentProviderListResponse {
    pub data: Vec<EnvironmentProvider>,
    pub next_cursor: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentProviderDeleteParams {
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentProviderDeleteResponse {
    pub cleanup: EnvironmentProviderCleanup,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentProviderCleanup {
    pub status: EnvironmentProviderCleanupStatus,
    pub failed_environment_ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum EnvironmentProviderCleanupStatus {
    Complete,
    Partial,
    Unknown,
}

#[cfg(test)]
#[path = "environment_tests.rs"]
mod tests;
