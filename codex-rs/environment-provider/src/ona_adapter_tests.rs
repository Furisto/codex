use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_json;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::*;
use crate::EnvironmentPhase;
use crate::EnvironmentSource;

#[tokio::test]
async fn ona_adapter_routes_authenticated_create_read_and_delete_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/{CREATE_ENVIRONMENT_PROCEDURE}")))
        .and(header("authorization", "Bearer token"))
        .and(header("connect-protocol-version", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "environment": owned_environment("created")
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/{GET_ENVIRONMENT_PROCEDURE}")))
        .and(body_json(json!({"environmentId": "read"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "environment": owned_environment("read")
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/{DELETE_ENVIRONMENT_PROCEDURE}")))
        .and(body_json(
            json!({"environmentId": "delete", "force": false}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let adapter = adapter(&server).await;

    assert_eq!(
        adapter
            .create_environment(CreateEnvironmentParams {
                source: EnvironmentSource {
                    repository_url: "https://github.com/openai/codex".to_string(),
                    git_ref: "main".to_string(),
                },
                resource_class: "large".to_string(),
            })
            .await
            .expect("create should succeed")
            .environment_ref
            .environment_id,
        "created"
    );
    assert_eq!(
        adapter
            .read_environment(ReadEnvironmentParams {
                environment_id: "read".to_string(),
            })
            .await
            .expect("read should succeed")
            .status
            .phase,
        EnvironmentPhase::Running
    );
    adapter
        .delete_environment(DeleteEnvironmentParams {
            environment_id: "delete".to_string(),
        })
        .await
        .expect("delete should succeed");
}

#[tokio::test]
async fn ona_list_fills_pages_after_filtering_foreign_environments_locally() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/{LIST_ENVIRONMENTS_PROCEDURE}")))
        .and(body_json(
            json!({"pagination": {"pageSize": 2, "token": ""}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "environments": [foreign_environment("foreign"), owned_environment("first")],
            "pagination": {"nextToken": "page-2"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/{LIST_ENVIRONMENTS_PROCEDURE}")))
        .and(body_json(
            json!({"pagination": {"pageSize": 1, "token": "page-2"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "environments": [owned_environment("second")],
            "pagination": {"nextToken": "page-3"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let page = adapter(&server)
        .await
        .list_environments(ListEnvironmentsParams {
            cursor: None,
            limit: 2,
        })
        .await
        .expect("list should succeed");
    assert_eq!(
        page.data
            .into_iter()
            .map(|environment| environment.environment_ref.environment_id)
            .collect::<Vec<_>>(),
        vec!["first".to_string(), "second".to_string()]
    );
    assert_eq!(page.next_cursor, Some("page-3".to_string()));
}

async fn adapter(server: &MockServer) -> Arc<dyn EnvironmentProviderAdapter> {
    OnaEnvironmentProviderAdapterFactory::default()
        .create_adapter(ResolvedEnvironmentProviderDefinition {
            id: "provider-id".to_string(),
            name: "Ona".to_string(),
            kind: EnvironmentProviderKind::Ona,
            url: server.uri(),
            authentication: EnvironmentProviderAuthenticationInput::Pat(
                PersonalAccessToken::new("token".to_string()).expect("PAT should be valid"),
            ),
        })
        .await
        .expect("adapter should be created")
}

fn owned_environment(environment_id: &str) -> serde_json::Value {
    json!({
        "id": environment_id,
        "metadata": {"annotations": {
            "openai.com/codex-provider-id": "provider-id",
            "openai.com/codex-source-ref": "main"
        }},
        "spec": {
            "machine": {"class": "large"},
            "content": {"initializer": {"specs": [{"git": {
                "remoteUri": "https://github.com/openai/codex",
                "targetMode": "CLONE_TARGET_MODE_REMOTE_BRANCH",
                "cloneTarget": "main"
            }}]}}
        },
        "status": {"phase": "ENVIRONMENT_PHASE_RUNNING"}
    })
}

fn foreign_environment(environment_id: &str) -> serde_json::Value {
    json!({
        "id": environment_id,
        "metadata": {"annotations": {
            "openai.com/codex-provider-id": "another-provider"
        }}
    })
}
