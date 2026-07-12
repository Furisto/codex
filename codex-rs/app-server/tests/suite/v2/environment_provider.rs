use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use codex_app_server_protocol::Environment;
use codex_app_server_protocol::EnvironmentListResponse;
use codex_app_server_protocol::EnvironmentPhase;
use codex_app_server_protocol::EnvironmentProvider;
use codex_app_server_protocol::EnvironmentProviderAuthentication;
use codex_app_server_protocol::EnvironmentProviderCleanup;
use codex_app_server_protocol::EnvironmentProviderCleanupStatus;
use codex_app_server_protocol::EnvironmentProviderDeleteResponse;
use codex_app_server_protocol::EnvironmentProviderKind as ApiEnvironmentProviderKind;
use codex_app_server_protocol::EnvironmentProviderListResponse;
use codex_app_server_protocol::EnvironmentProviderUpdateResponse;
use codex_app_server_protocol::EnvironmentReadResponse;
use codex_app_server_protocol::EnvironmentRef;
use codex_app_server_protocol::EnvironmentStatus;
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

#[tokio::test]
async fn static_environment_read_and_cursor_list_use_lifecycle_shape() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("environments.toml"),
        r#"
default = "none"
include_local = false

[[environments]]
id = "zeta"
url = "ws://127.0.0.1:1"

[[environments]]
id = "alpha"
url = "ws://127.0.0.1:2"
"#,
    )?;
    let mut app_server = TestAppServer::new(codex_home.path()).await?;
    timeout(RPC_TIMEOUT, app_server.initialize()).await??;

    let first_request_id = app_server
        .send_raw_request(
            "environment/list",
            Some(json!({"providerId": "static", "cursor": null, "limit": 1})),
        )
        .await?;
    let first_response: JSONRPCResponse = timeout(
        RPC_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(first_request_id)),
    )
    .await??;
    let first_page = to_response::<EnvironmentListResponse>(first_response)?;
    let next_cursor = first_page
        .next_cursor
        .clone()
        .expect("first static page should have a cursor");
    assert_eq!(
        first_page,
        EnvironmentListResponse {
            data: vec![static_environment("alpha")],
            next_cursor: Some(next_cursor.clone()),
        }
    );

    let second_request_id = app_server
        .send_raw_request(
            "environment/list",
            Some(json!({
                "providerId": "static",
                "cursor": next_cursor,
                "limit": 1,
            })),
        )
        .await?;
    let second_response: JSONRPCResponse = timeout(
        RPC_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(second_request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<EnvironmentListResponse>(second_response)?,
        EnvironmentListResponse {
            data: vec![static_environment("zeta")],
            next_cursor: None,
        }
    );

    let read_request_id = app_server
        .send_raw_request(
            "environment/read",
            Some(json!({"providerId": "static", "environmentId": "zeta"})),
        )
        .await?;
    let read_response: JSONRPCResponse = timeout(
        RPC_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(read_request_id)),
    )
    .await??;
    assert_eq!(
        to_response::<EnvironmentReadResponse>(read_response)?,
        EnvironmentReadResponse {
            environment: static_environment("zeta"),
        }
    );

    for (method, params, expected_message) in [
        (
            "environment/create",
            json!({
                "providerId": "static",
                "source": {"repositoryUrl": "https://example.com/repo", "ref": "main"},
                "resourceClass": "large",
            }),
            "invalid environment provider request: static environments cannot be created through this API",
        ),
        (
            "environment/delete",
            json!({"providerId": "static", "environmentId": "zeta"}),
            "invalid environment provider request: static environments cannot be deleted through this API",
        ),
    ] {
        let request_id = app_server.send_raw_request(method, Some(params)).await?;
        let error: JSONRPCError = timeout(
            RPC_TIMEOUT,
            app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
        )
        .await??;
        assert_eq!(error.error.message, expected_message);
    }
    Ok(())
}

fn static_environment(environment_id: &str) -> Environment {
    Environment {
        environment_ref: EnvironmentRef {
            provider_id: "static".to_string(),
            environment_id: environment_id.to_string(),
        },
        source: None,
        resource_class: None,
        status: EnvironmentStatus {
            phase: EnvironmentPhase::Running,
            error: None,
        },
    }
}
