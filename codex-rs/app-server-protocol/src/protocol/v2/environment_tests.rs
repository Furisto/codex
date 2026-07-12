use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;

#[test]
fn provider_create_params_use_tagged_pat_authentication() {
    let params = EnvironmentProviderCreateParams {
        name: "Production".to_string(),
        kind: EnvironmentProviderKind::Ona,
        url: None,
        authentication: EnvironmentProviderAuthenticationParams::Pat {
            token: "secret-token".to_string(),
        },
    };

    assert_eq!(
        serde_json::to_value(&params).expect("provider create params should serialize"),
        json!({
            "name": "Production",
            "kind": "ona",
            "url": null,
            "authentication": {
                "type": "pat",
                "token": "secret-token"
            }
        })
    );
}

#[test]
fn provider_response_redacts_pat_and_supports_static_nulls() {
    let response = EnvironmentProviderListResponse {
        data: vec![
            EnvironmentProvider {
                id: "static".to_string(),
                name: "Static".to_string(),
                kind: EnvironmentProviderKind::Static,
                url: None,
                authentication: None,
            },
            EnvironmentProvider {
                id: "provider-id".to_string(),
                name: "Production".to_string(),
                kind: EnvironmentProviderKind::Ona,
                url: Some("https://app.gitpod.io/api".to_string()),
                authentication: Some(EnvironmentProviderAuthentication::Pat),
            },
        ],
        next_cursor: None,
    };

    assert_eq!(
        serde_json::to_value(&response).expect("provider list response should serialize"),
        json!({
            "data": [
                {
                    "id": "static",
                    "name": "Static",
                    "kind": "static",
                    "url": null,
                    "authentication": null
                },
                {
                    "id": "provider-id",
                    "name": "Production",
                    "kind": "ona",
                    "url": "https://app.gitpod.io/api",
                    "authentication": { "type": "pat" }
                }
            ],
            "nextCursor": null
        })
    );
}

#[test]
fn provider_delete_force_defaults_to_false() {
    assert_eq!(
        serde_json::from_value::<EnvironmentProviderDeleteParams>(json!({
            "providerId": "provider-id"
        }))
        .expect("provider delete params should deserialize"),
        EnvironmentProviderDeleteParams {
            provider_id: "provider-id".to_string(),
            force: false,
        }
    );
}

#[test]
fn environment_create_uses_structured_source_and_returns_only_a_reference() {
    let params = EnvironmentCreateParams {
        provider_id: "provider-id".to_string(),
        source: EnvironmentSource {
            repository_url: "https://github.com/openai/codex".to_string(),
            git_ref: "refs/heads/main".to_string(),
        },
        resource_class: "large".to_string(),
    };
    assert_eq!(
        serde_json::to_value(params).expect("environment create params should serialize"),
        json!({
            "providerId": "provider-id",
            "source": {
                "repositoryUrl": "https://github.com/openai/codex",
                "ref": "refs/heads/main"
            },
            "resourceClass": "large"
        })
    );

    let response = EnvironmentCreateResponse {
        environment: EnvironmentRef {
            provider_id: "provider-id".to_string(),
            environment_id: "environment-id".to_string(),
        },
    };
    assert_eq!(
        serde_json::to_value(response).expect("environment create response should serialize"),
        json!({
            "environment": {
                "providerId": "provider-id",
                "environmentId": "environment-id"
            }
        })
    );
}

#[test]
fn environment_records_support_dynamic_source_and_static_nulls() {
    let dynamic = Environment {
        environment_ref: EnvironmentRef {
            provider_id: "provider-id".to_string(),
            environment_id: "environment-id".to_string(),
        },
        source: Some(EnvironmentSource {
            repository_url: "https://github.com/openai/codex".to_string(),
            git_ref: "main".to_string(),
        }),
        resource_class: Some("large".to_string()),
        status: EnvironmentStatus {
            phase: EnvironmentPhase::Creating,
            error: None,
        },
    };
    let static_environment = Environment {
        environment_ref: EnvironmentRef {
            provider_id: "static".to_string(),
            environment_id: "local".to_string(),
        },
        source: None,
        resource_class: None,
        status: EnvironmentStatus {
            phase: EnvironmentPhase::Running,
            error: None,
        },
    };

    assert_eq!(
        serde_json::to_value(EnvironmentListResponse {
            data: vec![dynamic, static_environment],
            next_cursor: None,
        })
        .expect("environment list response should serialize"),
        json!({
            "data": [
                {
                    "ref": {
                        "providerId": "provider-id",
                        "environmentId": "environment-id"
                    },
                    "source": {
                        "repositoryUrl": "https://github.com/openai/codex",
                        "ref": "main"
                    },
                    "resourceClass": "large",
                    "status": {
                        "phase": "creating",
                        "error": null
                    }
                },
                {
                    "ref": {
                        "providerId": "static",
                        "environmentId": "local"
                    },
                    "source": null,
                    "resourceClass": null,
                    "status": {
                        "phase": "running",
                        "error": null
                    }
                }
            ],
            "nextCursor": null
        })
    );
}
