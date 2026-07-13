use std::collections::HashMap;

use serde::Deserialize;
use serde::Serialize;

use crate::CreateEnvironmentParams;
use crate::Environment;
use crate::EnvironmentConnection;
use crate::EnvironmentPhase;
use crate::EnvironmentProviderAdapterError;
use crate::EnvironmentProviderAdapterResult;
use crate::EnvironmentProviderEvent;
use crate::EnvironmentRef;
use crate::EnvironmentSource;
use crate::EnvironmentStatus;

pub(crate) const CODEX_PROVIDER_ANNOTATION: &str = "openai.com/codex-provider-id";
const CODEX_SOURCE_REF_ANNOTATION: &str = "openai.com/codex-source-ref";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OnaCreateEnvironmentRequest {
    spec: OnaEnvironmentSpec,
    annotations: HashMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OnaCreateEnvironmentResponse {
    pub(crate) environment: OnaEnvironment,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OnaGetEnvironmentRequest {
    pub(crate) environment_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OnaGetEnvironmentResponse {
    pub(crate) environment: OnaEnvironment,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OnaListEnvironmentsRequest {
    pub(crate) pagination: OnaPaginationRequest,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OnaListEnvironmentsResponse {
    #[serde(default)]
    pub(crate) environments: Vec<OnaEnvironment>,
    #[serde(default)]
    pub(crate) pagination: OnaPaginationResponse,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OnaPaginationRequest {
    pub(crate) page_size: usize,
    pub(crate) token: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OnaPaginationResponse {
    #[serde(default)]
    pub(crate) next_token: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OnaDeleteEnvironmentRequest {
    pub(crate) environment_id: String,
    pub(crate) force: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OnaWatchEventsRequest {
    organization: bool,
    resource_type_filters: Vec<OnaResourceTypeFilter>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OnaResourceTypeFilter {
    resource_type: OnaResourceType,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OnaWatchEventsResponse {
    operation: OnaResourceOperation,
    resource_type: OnaResourceType,
    resource_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum OnaResourceType {
    #[serde(rename = "RESOURCE_TYPE_ENVIRONMENT")]
    Environment,
}

#[derive(Clone, Copy, Debug, Deserialize)]
enum OnaResourceOperation {
    #[serde(rename = "RESOURCE_OPERATION_CREATE")]
    Create,
    #[serde(rename = "RESOURCE_OPERATION_UPDATE")]
    Update,
    #[serde(rename = "RESOURCE_OPERATION_DELETE")]
    Delete,
    #[serde(rename = "RESOURCE_OPERATION_UPDATE_STATUS")]
    UpdateStatus,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OnaEnvironment {
    pub(crate) id: String,
    #[serde(default)]
    metadata: Option<OnaEnvironmentMetadata>,
    #[serde(default)]
    spec: Option<OnaEnvironmentSpec>,
    #[serde(default)]
    status: Option<OnaEnvironmentStatus>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OnaEnvironmentMetadata {
    #[serde(default)]
    annotations: HashMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OnaEnvironmentSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    desired_phase: Option<OnaEnvironmentPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    machine: Option<OnaMachineSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content: Option<OnaContentSpec>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OnaMachineSpec {
    class: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OnaContentSpec {
    initializer: OnaEnvironmentInitializer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OnaEnvironmentInitializer {
    specs: Vec<OnaEnvironmentInitializerSpec>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OnaEnvironmentInitializerSpec {
    git: OnaGitInitializer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OnaGitInitializer {
    remote_uri: String,
    target_mode: OnaCloneTargetMode,
    clone_target: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
enum OnaCloneTargetMode {
    #[serde(rename = "CLONE_TARGET_MODE_REMOTE_COMMIT")]
    Commit,
    #[serde(rename = "CLONE_TARGET_MODE_REMOTE_BRANCH")]
    Branch,
    #[serde(rename = "CLONE_TARGET_MODE_REMOTE_TAG")]
    Tag,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OnaEnvironmentStatus {
    #[serde(default)]
    phase: Option<OnaEnvironmentPhase>,
    #[serde(default)]
    failure_message: Vec<String>,
    #[serde(default)]
    exec_server_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
enum OnaEnvironmentPhase {
    #[serde(rename = "ENVIRONMENT_PHASE_UNSPECIFIED")]
    Unknown,
    #[serde(rename = "ENVIRONMENT_PHASE_CREATING")]
    Creating,
    #[serde(rename = "ENVIRONMENT_PHASE_STARTING")]
    Starting,
    #[serde(rename = "ENVIRONMENT_PHASE_RUNNING")]
    Running,
    #[serde(rename = "ENVIRONMENT_PHASE_UPDATING")]
    Updating,
    #[serde(rename = "ENVIRONMENT_PHASE_STOPPING")]
    Stopping,
    #[serde(rename = "ENVIRONMENT_PHASE_STOPPED")]
    Stopped,
    #[serde(rename = "ENVIRONMENT_PHASE_DELETING")]
    Deleting,
    #[serde(rename = "ENVIRONMENT_PHASE_DELETED")]
    Deleted,
}

pub(crate) fn create_environment_request(
    provider_id: &str,
    params: CreateEnvironmentParams,
) -> OnaCreateEnvironmentRequest {
    let (target_mode, clone_target) = interpret_git_ref(&params.source.git_ref);
    OnaCreateEnvironmentRequest {
        spec: OnaEnvironmentSpec {
            desired_phase: Some(OnaEnvironmentPhase::Running),
            machine: Some(OnaMachineSpec {
                class: params.resource_class,
            }),
            content: Some(OnaContentSpec {
                initializer: OnaEnvironmentInitializer {
                    specs: vec![OnaEnvironmentInitializerSpec {
                        git: OnaGitInitializer {
                            remote_uri: params.source.repository_url,
                            target_mode,
                            clone_target,
                        },
                    }],
                },
            }),
        },
        annotations: HashMap::from([
            (
                CODEX_PROVIDER_ANNOTATION.to_string(),
                provider_id.to_string(),
            ),
            (
                CODEX_SOURCE_REF_ANNOTATION.to_string(),
                params.source.git_ref,
            ),
        ]),
    }
}

pub(crate) fn watch_events_request() -> OnaWatchEventsRequest {
    OnaWatchEventsRequest {
        organization: true,
        resource_type_filters: vec![OnaResourceTypeFilter {
            resource_type: OnaResourceType::Environment,
        }],
    }
}

pub(crate) fn event_from_ona(response: OnaWatchEventsResponse) -> EnvironmentProviderEvent {
    let OnaWatchEventsResponse {
        operation,
        resource_type: OnaResourceType::Environment,
        resource_id,
    } = response;
    match operation {
        OnaResourceOperation::Create
        | OnaResourceOperation::Update
        | OnaResourceOperation::UpdateStatus => EnvironmentProviderEvent::Changed {
            environment_id: resource_id,
        },
        OnaResourceOperation::Delete => EnvironmentProviderEvent::Deleted {
            environment_id: resource_id,
        },
    }
}

pub(crate) fn environment_from_ona(
    provider_id: &str,
    environment: OnaEnvironment,
) -> EnvironmentProviderAdapterResult<Environment> {
    let metadata = environment.metadata.unwrap_or_default();
    if metadata
        .annotations
        .get(CODEX_PROVIDER_ANNOTATION)
        .is_none_or(|owner| owner != provider_id)
    {
        return Err(EnvironmentProviderAdapterError::EnvironmentNotFound {
            environment_id: environment.id,
        });
    }
    let spec = environment
        .spec
        .ok_or_else(|| malformed_environment(&environment.id, "spec"))?;
    let resource_class = spec
        .machine
        .map(|machine| machine.class)
        .filter(|class| !class.is_empty())
        .ok_or_else(|| malformed_environment(&environment.id, "machine class"))?;
    let git = spec
        .content
        .and_then(|content| content.initializer.specs.into_iter().next())
        .map(|initializer| initializer.git)
        .ok_or_else(|| malformed_environment(&environment.id, "Git initializer"))?;
    let git_ref = metadata
        .annotations
        .get(CODEX_SOURCE_REF_ANNOTATION)
        .cloned()
        .unwrap_or(git.clone_target);
    let status = environment.status.unwrap_or_default();
    Ok(Environment {
        environment_ref: EnvironmentRef {
            provider_id: provider_id.to_string(),
            environment_id: environment.id,
        },
        source: EnvironmentSource {
            repository_url: git.remote_uri,
            git_ref,
        },
        resource_class,
        status: EnvironmentStatus {
            phase: status
                .phase
                .map(map_phase)
                .unwrap_or(EnvironmentPhase::Unknown),
            error: (!status.failure_message.is_empty()).then(|| status.failure_message.join("; ")),
        },
    })
}

pub(crate) fn connection_from_ona(
    environment: &OnaEnvironment,
) -> EnvironmentProviderAdapterResult<EnvironmentConnection> {
    let websocket_url = environment
        .status
        .as_ref()
        .and_then(|status| status.exec_server_url.clone())
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| EnvironmentProviderAdapterError::Unavailable {
            message: format!(
                "Ona environment {} does not expose an exec-server URL",
                environment.id
            ),
        })?;
    Ok(EnvironmentConnection { websocket_url })
}

pub(crate) fn is_owned_environment(provider_id: &str, environment: &OnaEnvironment) -> bool {
    environment
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.annotations.get(CODEX_PROVIDER_ANNOTATION))
        .is_some_and(|owner| owner == provider_id)
}

fn interpret_git_ref(git_ref: &str) -> (OnaCloneTargetMode, String) {
    if let Some(branch) = git_ref.strip_prefix("refs/heads/") {
        (OnaCloneTargetMode::Branch, branch.to_string())
    } else if let Some(tag) = git_ref.strip_prefix("refs/tags/") {
        (OnaCloneTargetMode::Tag, tag.to_string())
    } else if (7..=64).contains(&git_ref.len())
        && git_ref.chars().all(|char| char.is_ascii_hexdigit())
    {
        (OnaCloneTargetMode::Commit, git_ref.to_string())
    } else {
        (OnaCloneTargetMode::Branch, git_ref.to_string())
    }
}

fn map_phase(phase: OnaEnvironmentPhase) -> EnvironmentPhase {
    match phase {
        OnaEnvironmentPhase::Unknown => EnvironmentPhase::Unknown,
        OnaEnvironmentPhase::Creating => EnvironmentPhase::Creating,
        OnaEnvironmentPhase::Starting => EnvironmentPhase::Starting,
        OnaEnvironmentPhase::Running => EnvironmentPhase::Running,
        OnaEnvironmentPhase::Updating => EnvironmentPhase::Updating,
        OnaEnvironmentPhase::Stopping => EnvironmentPhase::Stopping,
        OnaEnvironmentPhase::Stopped => EnvironmentPhase::Stopped,
        OnaEnvironmentPhase::Deleting => EnvironmentPhase::Deleting,
        OnaEnvironmentPhase::Deleted => EnvironmentPhase::Deleted,
    }
}

fn malformed_environment(environment_id: &str, field: &str) -> EnvironmentProviderAdapterError {
    EnvironmentProviderAdapterError::Internal {
        message: format!("Ona environment {environment_id} is missing its {field}"),
    }
}

#[cfg(test)]
#[path = "ona_tests.rs"]
mod tests;
