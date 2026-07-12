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
