use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use codex_app_server_protocol::EnvironmentProvider;
use codex_app_server_protocol::EnvironmentProviderAuthentication;
use codex_app_server_protocol::EnvironmentProviderCleanup;
use codex_app_server_protocol::EnvironmentProviderCleanupStatus;
use codex_app_server_protocol::EnvironmentProviderDeleteResponse;
use codex_app_server_protocol::EnvironmentProviderKind as ApiEnvironmentProviderKind;
use codex_app_server_protocol::EnvironmentProviderListResponse;
use codex_app_server_protocol::EnvironmentProviderUpdateResponse;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_environment_provider::CreateEnvironmentProviderParams;
use codex_environment_provider::EncryptedEnvironmentProviderCredential;
use codex_environment_provider::EnvironmentProviderKind;
use codex_environment_provider::EnvironmentProviderStore;
use codex_environment_provider::LocalEnvironmentProviderStore;
use codex_environment_provider::StoredEnvironmentProviderAuthentication;
use codex_state::StateRuntime;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const RPC_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn provider_list_and_rename_update_use_persisted_definitions() -> Result<()> {
    let codex_home = TempDir::new()?;
    let state_db =
        StateRuntime::init(codex_home.path().to_path_buf(), "test-provider".to_string()).await?;
    let stored = LocalEnvironmentProviderStore::new(state_db)
        .create_provider(CreateEnvironmentProviderParams {
            name: "Production".to_string(),
            kind: EnvironmentProviderKind::Ona,
            url: "https://app.gitpod.io/api".to_string(),
            authentication: StoredEnvironmentProviderAuthentication::Pat(
                EncryptedEnvironmentProviderCredential {
                    version: 1,
                    ciphertext: vec![1, 2, 3],
                },
            ),
        })
        .await?;

    let mut app_server = TestAppServer::new(codex_home.path()).await?;
    timeout(RPC_TIMEOUT, app_server.initialize()).await??;

    let list_request_id = app_server
        .send_raw_request(
            "environmentProvider/list",
            Some(json!({"cursor": null, "limit": 10})),
        )
        .await?;
    let list_response: JSONRPCResponse = timeout(
        RPC_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(list_request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<EnvironmentProviderListResponse>(list_response)?,
        EnvironmentProviderListResponse {
            data: vec![
                EnvironmentProvider {
                    id: "static".to_string(),
                    name: "Static".to_string(),
                    kind: ApiEnvironmentProviderKind::Static,
                    url: None,
                    authentication: None,
                },
                EnvironmentProvider {
                    id: stored.id.clone(),
                    name: "Production".to_string(),
                    kind: ApiEnvironmentProviderKind::Ona,
                    url: Some("https://app.gitpod.io/api".to_string()),
                    authentication: Some(EnvironmentProviderAuthentication::Pat),
                },
            ],
            next_cursor: None,
        }
    );

    let update_request_id = app_server
        .send_raw_request(
            "environmentProvider/update",
            Some(json!({
                "providerId": stored.id.clone(),
                "name": "Renamed",
            })),
        )
        .await?;
    let update_response: JSONRPCResponse = timeout(
        RPC_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(update_request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<EnvironmentProviderUpdateResponse>(update_response)?,
        EnvironmentProviderUpdateResponse {
            provider: EnvironmentProvider {
                id: stored.id,
                name: "Renamed".to_string(),
                kind: ApiEnvironmentProviderKind::Ona,
                url: Some("https://app.gitpod.io/api".to_string()),
                authentication: Some(EnvironmentProviderAuthentication::Pat),
            },
        }
    );
    Ok(())
}

#[tokio::test]
async fn provider_create_rejects_static_before_accessing_credentials() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut app_server = TestAppServer::new(codex_home.path()).await?;
    timeout(RPC_TIMEOUT, app_server.initialize()).await??;

    let request_id = app_server
        .send_raw_request(
            "environmentProvider/create",
            Some(json!({
                "name": "Other Static",
                "kind": "static",
                "url": null,
                "authentication": {"type": "pat", "token": "unused"},
            })),
        )
        .await?;
    let error: JSONRPCError = timeout(
        RPC_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(
        error.error.message,
        "invalid environment provider request: the built-in static environment provider cannot be created"
    );
    Ok(())
}

#[tokio::test]
async fn provider_force_delete_reports_unknown_without_adapter_and_removes_definition() -> Result<()>
{
    let codex_home = TempDir::new()?;
    let state_db =
        StateRuntime::init(codex_home.path().to_path_buf(), "test-provider".to_string()).await?;
    let stored = LocalEnvironmentProviderStore::new(state_db)
        .create_provider(CreateEnvironmentProviderParams {
            name: "Delete Me".to_string(),
            kind: EnvironmentProviderKind::Ona,
            url: "https://app.gitpod.io/api".to_string(),
            authentication: StoredEnvironmentProviderAuthentication::Pat(
                EncryptedEnvironmentProviderCredential {
                    version: 1,
                    ciphertext: vec![1, 2, 3],
                },
            ),
        })
        .await?;
    let mut app_server = TestAppServer::new(codex_home.path()).await?;
    timeout(RPC_TIMEOUT, app_server.initialize()).await??;

    let delete_request_id = app_server
        .send_raw_request(
            "environmentProvider/delete",
            Some(json!({"providerId": stored.id, "force": true})),
        )
        .await?;
    let delete_response: JSONRPCResponse = timeout(
        RPC_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(delete_request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<EnvironmentProviderDeleteResponse>(delete_response)?,
        EnvironmentProviderDeleteResponse {
            cleanup: EnvironmentProviderCleanup {
                status: EnvironmentProviderCleanupStatus::Unknown,
                failed_environment_ids: Vec::new(),
            },
        }
    );

    let list_request_id = app_server
        .send_raw_request(
            "environmentProvider/list",
            Some(json!({"cursor": null, "limit": 10})),
        )
        .await?;
    let list_response: JSONRPCResponse = timeout(
        RPC_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(list_request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<EnvironmentProviderListResponse>(list_response)?,
        EnvironmentProviderListResponse {
            data: vec![EnvironmentProvider {
                id: "static".to_string(),
                name: "Static".to_string(),
                kind: ApiEnvironmentProviderKind::Static,
                url: None,
                authentication: None,
            }],
            next_cursor: None,
        }
    );
    Ok(())
}
