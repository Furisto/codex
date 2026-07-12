use std::path::PathBuf;
use std::sync::Arc;

use codex_secrets::EnvironmentProviderCredentialCipher;
use codex_secrets::EnvironmentProviderCredentialCiphertext;
use codex_state::StateRuntime;
use serde::Deserialize;
use serde::Serialize;
use url::Url;

use crate::CreateEnvironmentProviderParams;
use crate::EncryptedEnvironmentProviderCredential;
use crate::EnvironmentProvider;
use crate::EnvironmentProviderAuthentication;
use crate::EnvironmentProviderAuthenticationInput;
use crate::EnvironmentProviderDefinition;
use crate::EnvironmentProviderKind;
use crate::EnvironmentProviderListPage;
use crate::EnvironmentProviderServiceCreateParams;
use crate::EnvironmentProviderServiceError;
use crate::EnvironmentProviderServiceResult;
use crate::EnvironmentProviderServiceUpdateParams;
use crate::EnvironmentProviderStore;
use crate::EnvironmentProviderStoreError;
use crate::ListEnvironmentProvidersParams;
use crate::LocalEnvironmentProviderStore;
use crate::PersonalAccessToken;
use crate::ResolvedEnvironmentProviderDefinition;
use crate::StoredEnvironmentProviderAuthentication;
use crate::UpdateEnvironmentProviderParams;

pub const STATIC_ENVIRONMENT_PROVIDER_ID: &str = "static";
pub const STATIC_ENVIRONMENT_PROVIDER_NAME: &str = "Static";
pub const ONA_DEFAULT_URL: &str = "https://app.gitpod.io/api";

/// Owns environment provider configuration policy above a storage-neutral provider store.
#[derive(Clone)]
pub struct EnvironmentProviderService {
    store: Option<Arc<dyn EnvironmentProviderStore>>,
    credential_cipher: EnvironmentProviderCredentialCipher,
}

impl std::fmt::Debug for EnvironmentProviderService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvironmentProviderService")
            .field("storage_available", &self.store.is_some())
            .field("credential_cipher", &self.credential_cipher)
            .finish_non_exhaustive()
    }
}

impl EnvironmentProviderService {
    /// Creates a provider service using the local state database and platform keyring.
    pub fn new_local(codex_home: PathBuf, state_db: Arc<StateRuntime>) -> Self {
        Self::new(
            Arc::new(LocalEnvironmentProviderStore::new(state_db)),
            EnvironmentProviderCredentialCipher::new(codex_home),
        )
    }

    /// Creates a static-only local service when the state database is unavailable.
    pub fn new_local_static_only(codex_home: PathBuf) -> Self {
        Self::static_only(EnvironmentProviderCredentialCipher::new(codex_home))
    }

    /// Creates a provider service with dynamic provider persistence.
    pub fn new(
        store: Arc<dyn EnvironmentProviderStore>,
        credential_cipher: EnvironmentProviderCredentialCipher,
    ) -> Self {
        Self {
            store: Some(store),
            credential_cipher,
        }
    }

    /// Creates a provider service that exposes only the built-in static provider.
    pub fn static_only(credential_cipher: EnvironmentProviderCredentialCipher) -> Self {
        Self {
            store: None,
            credential_cipher,
        }
    }

    /// Creates and returns a redacted dynamic provider definition without contacting it.
    pub async fn create_provider(
        &self,
        params: EnvironmentProviderServiceCreateParams,
    ) -> EnvironmentProviderServiceResult<EnvironmentProvider> {
        validate_provider_name(&params.name)?;
        let url = resolve_provider_url(params.kind, params.url.as_deref())?;
        let store = self.required_store()?;
        let authentication = self.encrypt_authentication(params.authentication)?;
        let definition = store
            .create_provider(CreateEnvironmentProviderParams {
                name: params.name,
                kind: params.kind,
                url,
                authentication,
            })
            .await?;
        Ok(redacted_provider(definition))
    }

    /// Updates and returns the mutable, redacted fields of a dynamic provider definition.
    pub async fn update_provider(
        &self,
        params: EnvironmentProviderServiceUpdateParams,
    ) -> EnvironmentProviderServiceResult<EnvironmentProvider> {
        reject_static_provider(&params.id)?;
        if let Some(name) = params.name.as_deref() {
            validate_provider_name(name)?;
        }
        let store = self.required_store()?;
        let authentication = params
            .authentication
            .map(|authentication| self.encrypt_authentication(authentication))
            .transpose()?;
        let definition = store
            .update_provider(UpdateEnvironmentProviderParams {
                id: params.id,
                name: params.name,
                authentication,
            })
            .await?;
        Ok(redacted_provider(definition))
    }

    /// Lists the fixed static provider followed by dynamic providers in stable keyset order.
    pub async fn list_providers(
        &self,
        params: ListEnvironmentProvidersParams,
    ) -> EnvironmentProviderServiceResult<EnvironmentProviderListPage> {
        if params.limit == 0 {
            return Err(EnvironmentProviderServiceError::InvalidRequest {
                message: "limit must be greater than zero".to_string(),
            });
        }
        let first_page = params.cursor.is_none();
        let dynamic_cursor = params
            .cursor
            .as_deref()
            .map(decode_service_cursor)
            .transpose()?
            .flatten();
        let mut data = Vec::new();
        if first_page {
            data.push(static_provider());
        }
        let Some(store) = self.store.as_ref() else {
            return Ok(EnvironmentProviderListPage {
                data,
                next_cursor: None,
            });
        };
        let dynamic_limit = if first_page {
            params.limit.saturating_sub(1).max(1)
        } else {
            params.limit
        };
        let dynamic_page = match store
            .list_providers(ListEnvironmentProvidersParams {
                cursor: dynamic_cursor,
                limit: dynamic_limit,
            })
            .await
        {
            Ok(page) => page,
            Err(EnvironmentProviderStoreError::Unavailable { .. }) => {
                return Ok(EnvironmentProviderListPage {
                    data,
                    next_cursor: None,
                });
            }
            Err(error) => return Err(error.into()),
        };
        let static_filled_page = first_page && params.limit == 1;
        let has_deferred_dynamic = static_filled_page && !dynamic_page.data.is_empty();
        if !static_filled_page {
            data.extend(dynamic_page.data.into_iter().map(redacted_provider));
        }
        let next_cursor = if has_deferred_dynamic {
            Some(encode_service_cursor(/*dynamic_cursor*/ None)?)
        } else {
            dynamic_page
                .next_cursor
                .map(|cursor| encode_service_cursor(Some(cursor)))
                .transpose()?
        };
        Ok(EnvironmentProviderListPage { data, next_cursor })
    }

    /// Loads and decrypts a dynamic provider definition for an authenticated provider operation.
    pub async fn resolve_provider(
        &self,
        provider_id: String,
    ) -> EnvironmentProviderServiceResult<ResolvedEnvironmentProviderDefinition> {
        reject_static_provider(&provider_id)?;
        let definition = self.required_store()?.read_provider(provider_id).await?;
        let authentication = self.decrypt_authentication(definition.authentication)?;
        Ok(ResolvedEnvironmentProviderDefinition {
            id: definition.id,
            name: definition.name,
            kind: definition.kind,
            url: definition.url,
            authentication,
        })
    }

    /// Removes a provider definition after an adapter has enforced cleanup policy.
    ///
    /// This method does not query or delete provider-owned environments. Callers must invoke it
    /// only after normal or forced cleanup orchestration has completed.
    pub async fn delete_provider_definition_after_cleanup(
        &self,
        provider_id: String,
    ) -> EnvironmentProviderServiceResult<()> {
        reject_static_provider(&provider_id)?;
        self.required_store()?
            .delete_provider(provider_id)
            .await
            .map_err(Into::into)
    }

    fn required_store(
        &self,
    ) -> EnvironmentProviderServiceResult<&Arc<dyn EnvironmentProviderStore>> {
        self.store
            .as_ref()
            .ok_or_else(|| EnvironmentProviderServiceError::StorageUnavailable {
                message: "the provider-definition store is not initialized".to_string(),
            })
    }

    fn encrypt_authentication(
        &self,
        authentication: EnvironmentProviderAuthenticationInput,
    ) -> EnvironmentProviderServiceResult<StoredEnvironmentProviderAuthentication> {
        match authentication {
            EnvironmentProviderAuthenticationInput::Pat(token) => {
                let encrypted = self
                    .credential_cipher
                    .encrypt(token.expose().as_bytes())
                    .map_err(credential_unavailable)?;
                Ok(StoredEnvironmentProviderAuthentication::Pat(
                    EncryptedEnvironmentProviderCredential {
                        version: encrypted.version,
                        ciphertext: encrypted.ciphertext,
                    },
                ))
            }
        }
    }

    fn decrypt_authentication(
        &self,
        authentication: StoredEnvironmentProviderAuthentication,
    ) -> EnvironmentProviderServiceResult<EnvironmentProviderAuthenticationInput> {
        match authentication {
            StoredEnvironmentProviderAuthentication::Pat(encrypted) => {
                let plaintext = self
                    .credential_cipher
                    .decrypt(&EnvironmentProviderCredentialCiphertext {
                        version: encrypted.version,
                        ciphertext: encrypted.ciphertext,
                    })
                    .map_err(credential_unavailable)?;
                let token = String::from_utf8(plaintext).map_err(|error| {
                    EnvironmentProviderServiceError::CredentialUnavailable {
                        message: format!("decrypted provider PAT is not valid UTF-8: {error}"),
                    }
                })?;
                let token = PersonalAccessToken::new(token).map_err(|message| {
                    EnvironmentProviderServiceError::CredentialUnavailable {
                        message: message.to_string(),
                    }
                })?;
                Ok(EnvironmentProviderAuthenticationInput::Pat(token))
            }
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentProviderServiceCursor {
    dynamic_cursor: Option<String>,
}

fn encode_service_cursor(
    dynamic_cursor: Option<String>,
) -> EnvironmentProviderServiceResult<String> {
    serde_json::to_string(&EnvironmentProviderServiceCursor { dynamic_cursor }).map_err(|error| {
        EnvironmentProviderServiceError::Internal {
            message: format!("failed to encode provider list cursor: {error}"),
        }
    })
}

fn decode_service_cursor(cursor: &str) -> EnvironmentProviderServiceResult<Option<String>> {
    serde_json::from_str::<EnvironmentProviderServiceCursor>(cursor)
        .map(|cursor| cursor.dynamic_cursor)
        .map_err(|_| EnvironmentProviderServiceError::InvalidCursor)
}

fn static_provider() -> EnvironmentProvider {
    EnvironmentProvider {
        id: STATIC_ENVIRONMENT_PROVIDER_ID.to_string(),
        name: STATIC_ENVIRONMENT_PROVIDER_NAME.to_string(),
        kind: EnvironmentProviderKind::Static,
        url: None,
        authentication: None,
    }
}

fn redacted_provider(definition: EnvironmentProviderDefinition) -> EnvironmentProvider {
    let authentication = match definition.authentication {
        StoredEnvironmentProviderAuthentication::Pat(_) => EnvironmentProviderAuthentication::Pat,
    };
    EnvironmentProvider {
        id: definition.id,
        name: definition.name,
        kind: definition.kind,
        url: Some(definition.url),
        authentication: Some(authentication),
    }
}

fn validate_provider_name(name: &str) -> EnvironmentProviderServiceResult<()> {
    if name.trim().is_empty() {
        Err(EnvironmentProviderServiceError::InvalidRequest {
            message: "provider name must not be empty".to_string(),
        })
    } else {
        Ok(())
    }
}

fn reject_static_provider(provider_id: &str) -> EnvironmentProviderServiceResult<()> {
    if provider_id == STATIC_ENVIRONMENT_PROVIDER_ID {
        Err(EnvironmentProviderServiceError::InvalidRequest {
            message: "the built-in static environment provider cannot be modified".to_string(),
        })
    } else {
        Ok(())
    }
}

fn resolve_provider_url(
    kind: EnvironmentProviderKind,
    requested_url: Option<&str>,
) -> EnvironmentProviderServiceResult<String> {
    let raw_url = match (kind, requested_url) {
        (EnvironmentProviderKind::Static, _) => {
            return Err(EnvironmentProviderServiceError::InvalidRequest {
                message: "the built-in static environment provider cannot be created".to_string(),
            });
        }
        (EnvironmentProviderKind::Ona, Some(url)) => url,
        (EnvironmentProviderKind::Ona, None) => ONA_DEFAULT_URL,
    };
    let mut url =
        Url::parse(raw_url).map_err(|error| EnvironmentProviderServiceError::InvalidRequest {
            message: format!("provider URL must be an absolute HTTP(S) URL: {error}"),
        })?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(EnvironmentProviderServiceError::InvalidRequest {
            message: "provider URL must be an absolute HTTP(S) URL".to_string(),
        });
    }
    let normalized_path = url.path().trim_end_matches('/').to_string();
    url.set_path(if normalized_path.is_empty() {
        "/"
    } else {
        &normalized_path
    });
    Ok(url.to_string())
}

fn credential_unavailable(error: impl std::fmt::Display) -> EnvironmentProviderServiceError {
    EnvironmentProviderServiceError::CredentialUnavailable {
        message: error.to_string(),
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
