use std::collections::BTreeSet;
use std::sync::Arc;

use reqwest::StatusCode;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::CreateEnvironmentParams;
use crate::DeleteEnvironmentParams;
use crate::Environment;
use crate::EnvironmentListPage;
use crate::EnvironmentProviderAdapter;
use crate::EnvironmentProviderAdapterError;
use crate::EnvironmentProviderAdapterFactory;
use crate::EnvironmentProviderAdapterFuture;
use crate::EnvironmentProviderAuthenticationInput;
use crate::EnvironmentProviderKind;
use crate::EnvironmentProviderWatch;
use crate::ListEnvironmentsParams;
use crate::PersonalAccessToken;
use crate::ReadEnvironmentParams;
use crate::ResolvedEnvironmentProviderDefinition;
use crate::ona::OnaCreateEnvironmentResponse;
use crate::ona::OnaDeleteEnvironmentRequest;
use crate::ona::OnaGetEnvironmentRequest;
use crate::ona::OnaGetEnvironmentResponse;
use crate::ona::OnaListEnvironmentsRequest;
use crate::ona::OnaListEnvironmentsResponse;
use crate::ona::OnaPaginationRequest;
use crate::ona::create_environment_request;
use crate::ona::environment_from_ona;
use crate::ona::is_owned_environment;

const GET_ENVIRONMENT_PROCEDURE: &str = "gitpod.v1.EnvironmentService/GetEnvironment";
const LIST_ENVIRONMENTS_PROCEDURE: &str = "gitpod.v1.EnvironmentService/ListEnvironments";
const CREATE_ENVIRONMENT_PROCEDURE: &str = "gitpod.v1.EnvironmentService/CreateEnvironment";
const DELETE_ENVIRONMENT_PROCEDURE: &str = "gitpod.v1.EnvironmentService/DeleteEnvironment";
const MAX_ONA_PAGE_SIZE: usize = 100;
const MAX_ERROR_BODY_CHARS: usize = 1_000;

/// Constructs Ona adapters from persisted Ona provider definitions.
#[derive(Clone, Debug, Default)]
pub struct OnaEnvironmentProviderAdapterFactory {
    client: reqwest::Client,
}

impl EnvironmentProviderAdapterFactory for OnaEnvironmentProviderAdapterFactory {
    fn create_adapter(
        &self,
        definition: ResolvedEnvironmentProviderDefinition,
    ) -> EnvironmentProviderAdapterFuture<'_, Arc<dyn EnvironmentProviderAdapter>> {
        let client = self.client.clone();
        Box::pin(async move {
            if definition.kind != EnvironmentProviderKind::Ona {
                return Err(EnvironmentProviderAdapterError::InvalidRequest {
                    message: "the Ona adapter factory requires an Ona provider".to_string(),
                });
            }
            let EnvironmentProviderAuthenticationInput::Pat(token) = definition.authentication;
            Ok(Arc::new(OnaEnvironmentProviderAdapter {
                provider_id: definition.id,
                base_url: definition.url.trim_end_matches('/').to_string(),
                token,
                client,
            }) as Arc<dyn EnvironmentProviderAdapter>)
        })
    }
}

struct OnaEnvironmentProviderAdapter {
    provider_id: String,
    base_url: String,
    token: PersonalAccessToken,
    client: reqwest::Client,
}

impl std::fmt::Debug for OnaEnvironmentProviderAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnaEnvironmentProviderAdapter")
            .field("provider_id", &self.provider_id)
            .field("base_url", &self.base_url)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl EnvironmentProviderAdapter for OnaEnvironmentProviderAdapter {
    fn create_environment(
        &self,
        params: CreateEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, Environment> {
        Box::pin(async move {
            let response: OnaCreateEnvironmentResponse = self
                .post(
                    CREATE_ENVIRONMENT_PROCEDURE,
                    &create_environment_request(&self.provider_id, params),
                    RequestTarget::Provider,
                )
                .await?;
            environment_from_ona(&self.provider_id, response.environment)
        })
    }

    fn read_environment(
        &self,
        params: ReadEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, Environment> {
        Box::pin(async move {
            let environment_id = params.environment_id;
            let response: OnaGetEnvironmentResponse = self
                .post(
                    GET_ENVIRONMENT_PROCEDURE,
                    &OnaGetEnvironmentRequest {
                        environment_id: environment_id.clone(),
                    },
                    RequestTarget::Environment(environment_id),
                )
                .await?;
            environment_from_ona(&self.provider_id, response.environment)
        })
    }

    fn list_environments(
        &self,
        params: ListEnvironmentsParams,
    ) -> EnvironmentProviderAdapterFuture<'_, EnvironmentListPage> {
        Box::pin(async move {
            if params.limit == 0 {
                return Err(EnvironmentProviderAdapterError::InvalidRequest {
                    message: "limit must be greater than zero".to_string(),
                });
            }
            let mut token = params.cursor.unwrap_or_default();
            let mut seen_tokens = BTreeSet::new();
            let mut environments = Vec::new();
            loop {
                let page_size = (params.limit - environments.len()).min(MAX_ONA_PAGE_SIZE);
                let response: OnaListEnvironmentsResponse = self
                    .post(
                        LIST_ENVIRONMENTS_PROCEDURE,
                        &OnaListEnvironmentsRequest {
                            pagination: OnaPaginationRequest {
                                page_size,
                                token: token.clone(),
                            },
                        },
                        RequestTarget::Provider,
                    )
                    .await?;
                for environment in response.environments {
                    if is_owned_environment(&self.provider_id, &environment) {
                        environments.push(environment_from_ona(&self.provider_id, environment)?);
                    }
                }
                let next_token = response.pagination.next_token;
                if environments.len() == params.limit || next_token.is_empty() {
                    return Ok(EnvironmentListPage {
                        data: environments,
                        next_cursor: (!next_token.is_empty()).then_some(next_token),
                    });
                }
                if !seen_tokens.insert(next_token.clone()) {
                    return Err(EnvironmentProviderAdapterError::Internal {
                        message: "Ona returned a repeated environment pagination token".to_string(),
                    });
                }
                token = next_token;
            }
        })
    }

    fn delete_environment(
        &self,
        params: DeleteEnvironmentParams,
    ) -> EnvironmentProviderAdapterFuture<'_, ()> {
        Box::pin(async move {
            let environment_id = params.environment_id;
            let _: serde_json::Value = self
                .post(
                    DELETE_ENVIRONMENT_PROCEDURE,
                    &OnaDeleteEnvironmentRequest {
                        environment_id: environment_id.clone(),
                        force: false,
                    },
                    RequestTarget::Environment(environment_id),
                )
                .await?;
            Ok(())
        })
    }

    fn watch(&self) -> EnvironmentProviderAdapterFuture<'_, EnvironmentProviderWatch> {
        Box::pin(async {
            Err(EnvironmentProviderAdapterError::Unavailable {
                message: "the Ona event watch is not implemented yet".to_string(),
            })
        })
    }
}

impl OnaEnvironmentProviderAdapter {
    async fn post<Request, Response>(
        &self,
        procedure: &str,
        request: &Request,
        target: RequestTarget,
    ) -> Result<Response, EnvironmentProviderAdapterError>
    where
        Request: Serialize + ?Sized,
        Response: DeserializeOwned,
    {
        let response = self
            .client
            .post(format!("{}/{procedure}", self.base_url))
            .bearer_auth(self.token.expose())
            .header("Connect-Protocol-Version", "1")
            .json(request)
            .send()
            .await
            .map_err(|error| EnvironmentProviderAdapterError::Unavailable {
                message: format!("Ona request failed: {error}"),
            })?;
        let status = response.status();
        let body = response.text().await.map_err(|error| {
            EnvironmentProviderAdapterError::Unavailable {
                message: format!("failed to read Ona response: {error}"),
            }
        })?;
        if !status.is_success() {
            return Err(response_error(status, body, target));
        }
        serde_json::from_str(&body).map_err(|error| EnvironmentProviderAdapterError::Internal {
            message: format!("failed to decode Ona response: {error}"),
        })
    }
}

enum RequestTarget {
    Provider,
    Environment(String),
}

fn response_error(
    status: StatusCode,
    body: String,
    target: RequestTarget,
) -> EnvironmentProviderAdapterError {
    if status == StatusCode::NOT_FOUND
        && let RequestTarget::Environment(environment_id) = target
    {
        return EnvironmentProviderAdapterError::EnvironmentNotFound { environment_id };
    }
    let body = body.chars().take(MAX_ERROR_BODY_CHARS).collect::<String>();
    let message = format!("Ona returned HTTP {status}: {body}");
    if status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY {
        EnvironmentProviderAdapterError::InvalidRequest { message }
    } else if status == StatusCode::UNAUTHORIZED
        || status == StatusCode::FORBIDDEN
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
    {
        EnvironmentProviderAdapterError::Unavailable { message }
    } else {
        EnvironmentProviderAdapterError::Internal { message }
    }
}

#[cfg(test)]
#[path = "ona_adapter_tests.rs"]
mod tests;
