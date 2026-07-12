use super::environment_processor::provider_service_error;
use super::*;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use codex_app_server_protocol::Environment as ApiEnvironment;
use codex_app_server_protocol::EnvironmentCreateParams;
use codex_app_server_protocol::EnvironmentCreateResponse;
use codex_app_server_protocol::EnvironmentCreatedNotification;
use codex_app_server_protocol::EnvironmentDeleteParams;
use codex_app_server_protocol::EnvironmentDeleteResponse;
use codex_app_server_protocol::EnvironmentListParams;
use codex_app_server_protocol::EnvironmentListResponse;
use codex_app_server_protocol::EnvironmentPhase as ApiEnvironmentPhase;
use codex_app_server_protocol::EnvironmentReadParams;
use codex_app_server_protocol::EnvironmentReadResponse;
use codex_app_server_protocol::EnvironmentRef as ApiEnvironmentRef;
use codex_app_server_protocol::EnvironmentSource as ApiEnvironmentSource;
use codex_app_server_protocol::EnvironmentStatus as ApiEnvironmentStatus;
use codex_environment_provider::CreateEnvironmentParams as DomainCreateEnvironmentParams;
use codex_environment_provider::DeleteEnvironmentParams as DomainDeleteEnvironmentParams;
use codex_environment_provider::Environment as DomainEnvironment;
use codex_environment_provider::EnvironmentLifecycleService;
use codex_environment_provider::EnvironmentLifecycleServiceError;
use codex_environment_provider::EnvironmentPhase as DomainEnvironmentPhase;
use codex_environment_provider::EnvironmentProviderAdapterError;
use codex_environment_provider::EnvironmentSource as DomainEnvironmentSource;
use codex_environment_provider::ListEnvironmentsParams as DomainListEnvironmentsParams;
use codex_environment_provider::ReadEnvironmentParams as DomainReadEnvironmentParams;
use codex_exec_server::STATIC_ENVIRONMENT_PROVIDER_ID;
use codex_exec_server::split_environment_id;
use serde::Deserialize;
use serde::Serialize;

const DEFAULT_ENVIRONMENT_LIST_LIMIT: usize = 50;
const MAX_ENVIRONMENT_LIST_LIMIT: usize = 100;

#[derive(Clone)]
pub(crate) struct EnvironmentLifecycleRequestProcessor {
    environment_manager: Arc<EnvironmentManager>,
    lifecycle: EnvironmentLifecycleService,
    outgoing: Arc<OutgoingMessageSender>,
}

impl EnvironmentLifecycleRequestProcessor {
    pub(crate) fn new(
        environment_manager: Arc<EnvironmentManager>,
        lifecycle: EnvironmentLifecycleService,
        outgoing: Arc<OutgoingMessageSender>,
    ) -> Self {
        Self {
            environment_manager,
            lifecycle,
            outgoing,
        }
    }

    pub(crate) async fn create(
        &self,
        params: EnvironmentCreateParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let environment = self
            .lifecycle
            .create_environment(
                params.provider_id,
                DomainCreateEnvironmentParams {
                    source: DomainEnvironmentSource {
                        repository_url: params.source.repository_url,
                        git_ref: params.source.git_ref,
                    },
                    resource_class: params.resource_class,
                },
            )
            .await
            .map_err(lifecycle_error)?;
        let environment_ref = api_environment_ref(&environment);
        self.outgoing
            .send_server_notification(ServerNotification::EnvironmentCreated(
                EnvironmentCreatedNotification {
                    environment: api_environment(environment),
                },
            ))
            .await;
        Ok(Some(
            EnvironmentCreateResponse {
                environment: environment_ref,
            }
            .into(),
        ))
    }

    pub(crate) async fn read(
        &self,
        params: EnvironmentReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let environment = if params.provider_id == STATIC_ENVIRONMENT_PROVIDER_ID {
            self.read_static(&params.environment_id)?
        } else {
            api_environment(
                self.lifecycle
                    .read_environment(
                        params.provider_id,
                        DomainReadEnvironmentParams {
                            environment_id: params.environment_id,
                        },
                    )
                    .await
                    .map_err(lifecycle_error)?,
            )
        };
        Ok(Some(EnvironmentReadResponse { environment }.into()))
    }

    pub(crate) async fn list(
        &self,
        params: EnvironmentListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let limit = params
            .limit
            .map(|limit| limit as usize)
            .unwrap_or(DEFAULT_ENVIRONMENT_LIST_LIMIT)
            .clamp(1, MAX_ENVIRONMENT_LIST_LIMIT);
        let page = if params.provider_id == STATIC_ENVIRONMENT_PROVIDER_ID {
            self.list_static(params.cursor, limit)?
        } else {
            let page = self
                .lifecycle
                .list_environments(
                    params.provider_id,
                    DomainListEnvironmentsParams {
                        cursor: params.cursor,
                        limit,
                    },
                )
                .await
                .map_err(lifecycle_error)?;
            EnvironmentListResponse {
                data: page.data.into_iter().map(api_environment).collect(),
                next_cursor: page.next_cursor,
            }
        };
        Ok(Some(page.into()))
    }

    pub(crate) async fn delete(
        &self,
        params: EnvironmentDeleteParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.lifecycle
            .delete_environment(
                params.provider_id,
                DomainDeleteEnvironmentParams {
                    environment_id: params.environment_id,
                },
            )
            .await
            .map_err(lifecycle_error)?;
        Ok(Some(EnvironmentDeleteResponse {}.into()))
    }

    fn read_static(&self, environment_id: &str) -> Result<ApiEnvironment, JSONRPCErrorError> {
        if environment_id.is_empty() {
            return Err(invalid_request("environment id cannot be empty"));
        }
        let canonical_id = format!("{STATIC_ENVIRONMENT_PROVIDER_ID}/{environment_id}");
        self.environment_manager
            .get_environment(&canonical_id)
            .ok_or_else(|| invalid_request(format!("unknown environment id `{canonical_id}`")))?;
        Ok(api_static_environment(environment_id.to_string()))
    }

    fn list_static(
        &self,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<EnvironmentListResponse, JSONRPCErrorError> {
        let cursor = cursor.as_deref().map(decode_static_cursor).transpose()?;
        let environment_ids = self
            .environment_manager
            .environment_ids()
            .into_iter()
            .filter_map(|environment_id| {
                let (provider_id, provider_environment_id) =
                    split_environment_id(&environment_id).ok()?;
                (provider_id == STATIC_ENVIRONMENT_PROVIDER_ID)
                    .then(|| provider_environment_id.to_string())
            })
            .filter(|environment_id| {
                cursor
                    .as_ref()
                    .is_none_or(|cursor| environment_id > &cursor.last_environment_id)
            })
            .collect::<Vec<_>>();
        let has_more = environment_ids.len() > limit;
        let data = environment_ids
            .into_iter()
            .take(limit)
            .map(api_static_environment)
            .collect::<Vec<_>>();
        let next_cursor = if has_more {
            data.last()
                .map(|environment| {
                    encode_static_cursor(&StaticEnvironmentCursor {
                        last_environment_id: environment.environment_ref.environment_id.clone(),
                    })
                })
                .transpose()?
        } else {
            None
        };
        Ok(EnvironmentListResponse { data, next_cursor })
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StaticEnvironmentCursor {
    last_environment_id: String,
}

fn encode_static_cursor(cursor: &StaticEnvironmentCursor) -> Result<String, JSONRPCErrorError> {
    serde_json::to_vec(cursor)
        .map(|cursor| URL_SAFE_NO_PAD.encode(cursor))
        .map_err(|error| internal_error(format!("failed to encode environment cursor: {error}")))
}

fn decode_static_cursor(cursor: &str) -> Result<StaticEnvironmentCursor, JSONRPCErrorError> {
    let cursor = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| invalid_request("invalid environment cursor"))?;
    let cursor: StaticEnvironmentCursor = serde_json::from_slice(&cursor)
        .map_err(|_| invalid_request("invalid environment cursor"))?;
    if cursor.last_environment_id.is_empty() {
        return Err(invalid_request("invalid environment cursor"));
    }
    Ok(cursor)
}

fn api_environment(environment: DomainEnvironment) -> ApiEnvironment {
    ApiEnvironment {
        environment_ref: api_environment_ref(&environment),
        source: Some(ApiEnvironmentSource {
            repository_url: environment.source.repository_url,
            git_ref: environment.source.git_ref,
        }),
        resource_class: Some(environment.resource_class),
        status: ApiEnvironmentStatus {
            phase: match environment.status.phase {
                DomainEnvironmentPhase::Unknown => ApiEnvironmentPhase::Unknown,
                DomainEnvironmentPhase::Creating => ApiEnvironmentPhase::Creating,
                DomainEnvironmentPhase::Starting => ApiEnvironmentPhase::Starting,
                DomainEnvironmentPhase::Running => ApiEnvironmentPhase::Running,
                DomainEnvironmentPhase::Updating => ApiEnvironmentPhase::Updating,
                DomainEnvironmentPhase::Stopping => ApiEnvironmentPhase::Stopping,
                DomainEnvironmentPhase::Stopped => ApiEnvironmentPhase::Stopped,
                DomainEnvironmentPhase::Deleting => ApiEnvironmentPhase::Deleting,
                DomainEnvironmentPhase::Deleted => ApiEnvironmentPhase::Deleted,
            },
            error: environment.status.error,
        },
    }
}

fn api_environment_ref(environment: &DomainEnvironment) -> ApiEnvironmentRef {
    ApiEnvironmentRef {
        provider_id: environment.environment_ref.provider_id.clone(),
        environment_id: environment.environment_ref.environment_id.clone(),
    }
}

fn api_static_environment(environment_id: String) -> ApiEnvironment {
    ApiEnvironment {
        environment_ref: ApiEnvironmentRef {
            provider_id: STATIC_ENVIRONMENT_PROVIDER_ID.to_string(),
            environment_id,
        },
        source: None,
        resource_class: None,
        status: ApiEnvironmentStatus {
            phase: ApiEnvironmentPhase::Running,
            error: None,
        },
    }
}

fn lifecycle_error(error: EnvironmentLifecycleServiceError) -> JSONRPCErrorError {
    match error {
        EnvironmentLifecycleServiceError::Configuration(error) => provider_service_error(error),
        EnvironmentLifecycleServiceError::Provider(
            error @ (EnvironmentProviderAdapterError::EnvironmentNotFound { .. }
            | EnvironmentProviderAdapterError::InvalidRequest { .. }),
        ) => invalid_request(error.to_string()),
        EnvironmentLifecycleServiceError::Provider(
            error @ (EnvironmentProviderAdapterError::Unavailable { .. }
            | EnvironmentProviderAdapterError::Internal { .. }),
        ) => internal_error(error.to_string()),
    }
}
