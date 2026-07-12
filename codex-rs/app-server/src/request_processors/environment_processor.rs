use super::*;
use codex_app_server_protocol::EnvironmentProvider as ApiEnvironmentProvider;
use codex_app_server_protocol::EnvironmentProviderAuthentication as ApiEnvironmentProviderAuthentication;
use codex_app_server_protocol::EnvironmentProviderAuthenticationParams;
use codex_app_server_protocol::EnvironmentProviderCreateParams;
use codex_app_server_protocol::EnvironmentProviderCreateResponse;
use codex_app_server_protocol::EnvironmentProviderDeleteParams;
use codex_app_server_protocol::EnvironmentProviderDeleteResponse;
use codex_app_server_protocol::EnvironmentProviderKind as ApiEnvironmentProviderKind;
use codex_app_server_protocol::EnvironmentProviderListParams;
use codex_app_server_protocol::EnvironmentProviderListResponse;
use codex_app_server_protocol::EnvironmentProviderUpdateParams;
use codex_app_server_protocol::EnvironmentProviderUpdateResponse;
use codex_environment_provider::DeleteEnvironmentProviderMode;
use codex_environment_provider::DeleteEnvironmentProviderParams as DomainDeleteEnvironmentProviderParams;
use codex_environment_provider::EnvironmentLifecycleService;
use codex_environment_provider::EnvironmentProvider as DomainEnvironmentProvider;
use codex_environment_provider::EnvironmentProviderAuthentication as DomainEnvironmentProviderAuthentication;
use codex_environment_provider::EnvironmentProviderAuthenticationInput;
use codex_environment_provider::EnvironmentProviderCleanupStatus as DomainEnvironmentProviderCleanupStatus;
use codex_environment_provider::EnvironmentProviderDeletionError;
use codex_environment_provider::EnvironmentProviderDeletionService;
use codex_environment_provider::EnvironmentProviderKind as DomainEnvironmentProviderKind;
use codex_environment_provider::EnvironmentProviderService;
use codex_environment_provider::EnvironmentProviderServiceCreateParams;
use codex_environment_provider::EnvironmentProviderServiceError;
use codex_environment_provider::EnvironmentProviderServiceUpdateParams;
use codex_environment_provider::ListEnvironmentProvidersParams;
use codex_environment_provider::PersonalAccessToken;
use std::time::Duration;

const DEFAULT_PROVIDER_LIST_LIMIT: usize = 50;
const MAX_PROVIDER_LIST_LIMIT: usize = 100;

#[derive(Clone)]
pub(crate) struct EnvironmentRequestProcessor {
    environment_manager: Arc<EnvironmentManager>,
    environment_provider_service: EnvironmentProviderService,
    environment_lifecycle_service: EnvironmentLifecycleService,
    environment_provider_deletion_service: EnvironmentProviderDeletionService,
}

impl EnvironmentRequestProcessor {
    pub(crate) fn new(
        environment_manager: Arc<EnvironmentManager>,
        environment_provider_service: EnvironmentProviderService,
        environment_lifecycle_service: EnvironmentLifecycleService,
    ) -> Self {
        let environment_provider_deletion_service =
            environment_lifecycle_service.deletion_service();
        Self {
            environment_manager,
            environment_provider_service,
            environment_lifecycle_service,
            environment_provider_deletion_service,
        }
    }

    pub(crate) async fn environment_add(
        &self,
        params: EnvironmentAddParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let environment_id = canonical_static_environment_id(&params.environment_id)
            .map_err(|err| invalid_request(err.to_string()))?;
        self.environment_manager
            .upsert_environment(
                environment_id,
                params.exec_server_url,
                params.connect_timeout_ms.map(Duration::from_millis),
            )
            .map_err(|err| invalid_request(err.to_string()))?;
        Ok(Some(EnvironmentAddResponse {}.into()))
    }

    pub(crate) async fn environment_info(
        &self,
        params: EnvironmentInfoParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let environment_id = params.environment_id;
        let environment = self
            .environment_manager
            .get_environment(&environment_id)
            .ok_or_else(|| invalid_request(format!("unknown environment id `{environment_id}`")))?;
        let info = environment.info().await.map_err(|err| {
            internal_error(format!(
                "failed to get info for environment `{environment_id}`: {err}"
            ))
        })?;
        Ok(Some(
            EnvironmentInfoResponse {
                shell: EnvironmentShellInfo {
                    name: info.shell.name,
                    path: info.shell.path,
                },
                cwd: info.cwd,
            }
            .into(),
        ))
    }

    pub(crate) async fn provider_create(
        &self,
        params: EnvironmentProviderCreateParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let provider = self
            .environment_provider_service
            .create_provider(EnvironmentProviderServiceCreateParams {
                name: params.name,
                kind: domain_provider_kind(params.kind),
                url: params.url,
                authentication: domain_provider_authentication(params.authentication)?,
            })
            .await
            .map_err(provider_service_error)?;
        Ok(Some(
            EnvironmentProviderCreateResponse {
                provider: api_provider(provider),
            }
            .into(),
        ))
    }

    pub(crate) async fn provider_update(
        &self,
        params: EnvironmentProviderUpdateParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let authentication_changed = params.authentication.is_some();
        let provider_id = params.provider_id.clone();
        let authentication = params
            .authentication
            .map(domain_provider_authentication)
            .transpose()?;
        let provider = self
            .environment_provider_service
            .update_provider(EnvironmentProviderServiceUpdateParams {
                id: params.provider_id,
                name: params.name,
                authentication,
            })
            .await
            .map_err(provider_service_error)?;
        if authentication_changed {
            self.environment_lifecycle_service
                .invalidate_provider(&provider_id);
        }
        Ok(Some(
            EnvironmentProviderUpdateResponse {
                provider: api_provider(provider),
            }
            .into(),
        ))
    }

    pub(crate) async fn provider_list(
        &self,
        params: EnvironmentProviderListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let limit = params
            .limit
            .map(|limit| limit as usize)
            .unwrap_or(DEFAULT_PROVIDER_LIST_LIMIT)
            .clamp(1, MAX_PROVIDER_LIST_LIMIT);
        let page = self
            .environment_provider_service
            .list_providers(ListEnvironmentProvidersParams {
                cursor: params.cursor,
                limit,
            })
            .await
            .map_err(provider_service_error)?;
        Ok(Some(
            EnvironmentProviderListResponse {
                data: page.data.into_iter().map(api_provider).collect(),
                next_cursor: page.next_cursor,
            }
            .into(),
        ))
    }

    pub(crate) async fn provider_delete(
        &self,
        params: EnvironmentProviderDeleteParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let cleanup = self
            .environment_provider_deletion_service
            .delete_provider(DomainDeleteEnvironmentProviderParams {
                provider_id: params.provider_id,
                mode: if params.force {
                    DeleteEnvironmentProviderMode::Force
                } else {
                    DeleteEnvironmentProviderMode::Normal
                },
            })
            .await
            .map_err(provider_deletion_error)?;
        Ok(Some(
            EnvironmentProviderDeleteResponse {
                cleanup: codex_app_server_protocol::EnvironmentProviderCleanup {
                    status: match cleanup.status {
                        DomainEnvironmentProviderCleanupStatus::Complete => {
                            codex_app_server_protocol::EnvironmentProviderCleanupStatus::Complete
                        }
                        DomainEnvironmentProviderCleanupStatus::Partial => {
                            codex_app_server_protocol::EnvironmentProviderCleanupStatus::Partial
                        }
                        DomainEnvironmentProviderCleanupStatus::Unknown => {
                            codex_app_server_protocol::EnvironmentProviderCleanupStatus::Unknown
                        }
                    },
                    failed_environment_ids: cleanup.failed_environment_ids,
                },
            }
            .into(),
        ))
    }
}

fn domain_provider_kind(kind: ApiEnvironmentProviderKind) -> DomainEnvironmentProviderKind {
    match kind {
        ApiEnvironmentProviderKind::Static => DomainEnvironmentProviderKind::Static,
        ApiEnvironmentProviderKind::Ona => DomainEnvironmentProviderKind::Ona,
    }
}

fn domain_provider_authentication(
    authentication: EnvironmentProviderAuthenticationParams,
) -> Result<EnvironmentProviderAuthenticationInput, JSONRPCErrorError> {
    match authentication {
        EnvironmentProviderAuthenticationParams::Pat { token } => PersonalAccessToken::new(token)
            .map(EnvironmentProviderAuthenticationInput::Pat)
            .map_err(invalid_request),
    }
}

fn api_provider(provider: DomainEnvironmentProvider) -> ApiEnvironmentProvider {
    ApiEnvironmentProvider {
        id: provider.id,
        name: provider.name,
        kind: match provider.kind {
            DomainEnvironmentProviderKind::Static => ApiEnvironmentProviderKind::Static,
            DomainEnvironmentProviderKind::Ona => ApiEnvironmentProviderKind::Ona,
        },
        url: provider.url,
        authentication: provider
            .authentication
            .map(|authentication| match authentication {
                DomainEnvironmentProviderAuthentication::Pat => {
                    ApiEnvironmentProviderAuthentication::Pat
                }
            }),
    }
}

pub(super) fn provider_service_error(error: EnvironmentProviderServiceError) -> JSONRPCErrorError {
    match error {
        EnvironmentProviderServiceError::ProviderNotFound { .. }
        | EnvironmentProviderServiceError::NameConflict { .. }
        | EnvironmentProviderServiceError::InvalidRequest { .. }
        | EnvironmentProviderServiceError::InvalidCursor => invalid_request(error.to_string()),
        EnvironmentProviderServiceError::StorageUnavailable { .. }
        | EnvironmentProviderServiceError::CredentialUnavailable { .. }
        | EnvironmentProviderServiceError::Internal { .. } => internal_error(error.to_string()),
    }
}

fn provider_deletion_error(error: EnvironmentProviderDeletionError) -> JSONRPCErrorError {
    match error {
        EnvironmentProviderDeletionError::Configuration(error) => provider_service_error(error),
        EnvironmentProviderDeletionError::ProviderNotEmpty { .. } => {
            invalid_request(error.to_string())
        }
        EnvironmentProviderDeletionError::CleanupUnavailable { .. } => {
            internal_error(error.to_string())
        }
    }
}
