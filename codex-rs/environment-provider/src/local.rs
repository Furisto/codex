use std::sync::Arc;

use codex_state::CreateEnvironmentProviderRecord;
use codex_state::EnvironmentProviderAuthenticationRecord;
use codex_state::EnvironmentProviderListCursor;
use codex_state::EnvironmentProviderNameRecord;
use codex_state::EnvironmentProviderRecord;
use codex_state::EnvironmentProviderUpdate;
use codex_state::StateRuntime;
use codex_state::UpdateEnvironmentProviderRecordOutcome;
use serde::Deserialize;
use serde::Serialize;

use crate::CreateEnvironmentProviderParams;
use crate::EncryptedEnvironmentProviderCredential;
use crate::EnvironmentProviderDefinition;
use crate::EnvironmentProviderKind;
use crate::EnvironmentProviderPage;
use crate::EnvironmentProviderStore;
use crate::EnvironmentProviderStoreError;
use crate::EnvironmentProviderStoreFuture;
use crate::ListEnvironmentProvidersParams;
use crate::StoredEnvironmentProviderAuthentication;
use crate::UpdateEnvironmentProviderParams;
use crate::normalize_environment_provider_name;

const ONA_PROVIDER_KIND: &str = "ona";
const PAT_AUTHENTICATION_KIND: &str = "pat";

/// SQLite-backed implementation of [`EnvironmentProviderStore`] using an existing state runtime.
#[derive(Clone)]
pub struct LocalEnvironmentProviderStore {
    state_db: Arc<StateRuntime>,
}

impl std::fmt::Debug for LocalEnvironmentProviderStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalEnvironmentProviderStore")
            .field("codex_home", &self.state_db.codex_home())
            .finish_non_exhaustive()
    }
}

impl LocalEnvironmentProviderStore {
    /// Creates a local provider store from an already-initialized state runtime.
    pub fn new(state_db: Arc<StateRuntime>) -> Self {
        Self { state_db }
    }
}

impl EnvironmentProviderStore for LocalEnvironmentProviderStore {
    fn create_provider(
        &self,
        params: CreateEnvironmentProviderParams,
    ) -> EnvironmentProviderStoreFuture<'_, EnvironmentProviderDefinition> {
        Box::pin(async move {
            let kind = persisted_kind(params.kind)?;
            let name = params.name;
            let record = CreateEnvironmentProviderRecord {
                normalized_name: normalize_environment_provider_name(&name),
                name: name.clone(),
                kind: kind.to_string(),
                url: params.url,
                authentication: authentication_to_record(params.authentication),
            };
            let Some(created) = self
                .state_db
                .create_environment_provider(record)
                .await
                .map_err(storage_unavailable)?
            else {
                return Err(EnvironmentProviderStoreError::NameConflict { name });
            };
            definition_from_record(created)
        })
    }

    fn read_provider(
        &self,
        provider_id: String,
    ) -> EnvironmentProviderStoreFuture<'_, EnvironmentProviderDefinition> {
        Box::pin(async move {
            let record = self
                .state_db
                .read_environment_provider(&provider_id)
                .await
                .map_err(storage_unavailable)?
                .ok_or_else(|| EnvironmentProviderStoreError::ProviderNotFound {
                    provider_id: provider_id.clone(),
                })?;
            definition_from_record(record)
        })
    }

    fn list_providers(
        &self,
        params: ListEnvironmentProvidersParams,
    ) -> EnvironmentProviderStoreFuture<'_, EnvironmentProviderPage> {
        Box::pin(async move {
            if params.limit == 0 {
                return Err(EnvironmentProviderStoreError::InvalidRequest {
                    message: "limit must be greater than zero".to_string(),
                });
            }
            let cursor = params.cursor.as_deref().map(decode_cursor).transpose()?;
            let fetch_limit = params.limit.checked_add(1).ok_or_else(|| {
                EnvironmentProviderStoreError::InvalidRequest {
                    message: "limit is too large".to_string(),
                }
            })?;
            let fetch_limit = i64::try_from(fetch_limit).map_err(|_| {
                EnvironmentProviderStoreError::InvalidRequest {
                    message: "limit is too large".to_string(),
                }
            })?;
            let mut records = self
                .state_db
                .list_environment_providers(cursor.as_ref(), fetch_limit)
                .await
                .map_err(storage_unavailable)?;
            let has_more = records.len() > params.limit;
            records.truncate(params.limit);
            let data = records
                .into_iter()
                .map(definition_from_record)
                .collect::<Result<Vec<_>, _>>()?;
            let next_cursor = if has_more {
                data.last().map(encode_cursor).transpose()?
            } else {
                None
            };
            Ok(EnvironmentProviderPage { data, next_cursor })
        })
    }

    fn update_provider(
        &self,
        params: UpdateEnvironmentProviderParams,
    ) -> EnvironmentProviderStoreFuture<'_, EnvironmentProviderDefinition> {
        Box::pin(async move {
            let requested_name = params.name.clone();
            let update = EnvironmentProviderUpdate {
                name: params.name.map(|name| {
                    let normalized_name = normalize_environment_provider_name(&name);
                    EnvironmentProviderNameRecord {
                        name,
                        normalized_name,
                    }
                }),
                authentication: params.authentication.map(authentication_to_record),
            };
            match self
                .state_db
                .update_environment_provider(&params.id, update)
                .await
                .map_err(storage_unavailable)?
            {
                UpdateEnvironmentProviderRecordOutcome::Updated(record) => {
                    definition_from_record(record)
                }
                UpdateEnvironmentProviderRecordOutcome::NotFound => {
                    Err(EnvironmentProviderStoreError::ProviderNotFound {
                        provider_id: params.id,
                    })
                }
                UpdateEnvironmentProviderRecordOutcome::NameConflict => {
                    let name =
                        requested_name.ok_or_else(|| EnvironmentProviderStoreError::Internal {
                            message: "state store reported a name conflict without a name update"
                                .to_string(),
                        })?;
                    Err(EnvironmentProviderStoreError::NameConflict { name })
                }
            }
        })
    }

    fn delete_provider(&self, provider_id: String) -> EnvironmentProviderStoreFuture<'_, ()> {
        Box::pin(async move {
            let deleted = self
                .state_db
                .delete_environment_provider(&provider_id)
                .await
                .map_err(storage_unavailable)?;
            if deleted {
                Ok(())
            } else {
                Err(EnvironmentProviderStoreError::ProviderNotFound { provider_id })
            }
        })
    }
}

#[derive(Deserialize, Serialize)]
struct LocalEnvironmentProviderCursor {
    normalized_name: String,
    id: String,
}

fn encode_cursor(
    definition: &EnvironmentProviderDefinition,
) -> Result<String, EnvironmentProviderStoreError> {
    serde_json::to_string(&LocalEnvironmentProviderCursor {
        normalized_name: definition.normalized_name.clone(),
        id: definition.id.clone(),
    })
    .map_err(internal_error)
}

fn decode_cursor(
    cursor: &str,
) -> Result<EnvironmentProviderListCursor, EnvironmentProviderStoreError> {
    let cursor = serde_json::from_str::<LocalEnvironmentProviderCursor>(cursor)
        .map_err(|_| EnvironmentProviderStoreError::InvalidCursor)?;
    Ok(EnvironmentProviderListCursor {
        normalized_name: cursor.normalized_name,
        id: cursor.id,
    })
}

fn persisted_kind(
    kind: EnvironmentProviderKind,
) -> Result<&'static str, EnvironmentProviderStoreError> {
    match kind {
        EnvironmentProviderKind::Static => Err(EnvironmentProviderStoreError::InvalidRequest {
            message: "the static environment provider cannot be persisted".to_string(),
        }),
        EnvironmentProviderKind::Ona => Ok(ONA_PROVIDER_KIND),
    }
}

fn authentication_to_record(
    authentication: StoredEnvironmentProviderAuthentication,
) -> EnvironmentProviderAuthenticationRecord {
    match authentication {
        StoredEnvironmentProviderAuthentication::Pat(credential) => {
            EnvironmentProviderAuthenticationRecord {
                kind: PAT_AUTHENTICATION_KIND.to_string(),
                ciphertext_version: i64::from(credential.version),
                ciphertext: credential.ciphertext,
            }
        }
    }
}

fn definition_from_record(
    record: EnvironmentProviderRecord,
) -> Result<EnvironmentProviderDefinition, EnvironmentProviderStoreError> {
    let kind = match record.kind.as_str() {
        ONA_PROVIDER_KIND => EnvironmentProviderKind::Ona,
        kind => {
            return Err(EnvironmentProviderStoreError::Internal {
                message: format!("unknown persisted environment provider kind {kind:?}"),
            });
        }
    };
    let authentication = match record.authentication.kind.as_str() {
        PAT_AUTHENTICATION_KIND => {
            StoredEnvironmentProviderAuthentication::Pat(EncryptedEnvironmentProviderCredential {
                version: u32::try_from(record.authentication.ciphertext_version).map_err(|_| {
                    EnvironmentProviderStoreError::Internal {
                        message: format!(
                            "invalid persisted credential version {}",
                            record.authentication.ciphertext_version
                        ),
                    }
                })?,
                ciphertext: record.authentication.ciphertext,
            })
        }
        kind => {
            return Err(EnvironmentProviderStoreError::Internal {
                message: format!("unknown persisted provider authentication kind {kind:?}"),
            });
        }
    };
    Ok(EnvironmentProviderDefinition {
        id: record.id,
        name: record.name,
        normalized_name: record.normalized_name,
        kind,
        url: record.url,
        authentication,
    })
}

fn storage_unavailable(err: impl std::fmt::Display) -> EnvironmentProviderStoreError {
    EnvironmentProviderStoreError::Unavailable {
        message: err.to_string(),
    }
}

fn internal_error(err: impl std::fmt::Display) -> EnvironmentProviderStoreError {
    EnvironmentProviderStoreError::Internal {
        message: err.to_string(),
    }
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
