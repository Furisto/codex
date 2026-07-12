use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;

#[test]
fn create_request_maps_common_source_resource_class_and_annotations() {
    let request = create_environment_request(
        "provider-id",
        CreateEnvironmentParams {
            source: EnvironmentSource {
                repository_url: "https://github.com/openai/codex".to_string(),
                git_ref: "refs/heads/feature".to_string(),
            },
            resource_class: "large".to_string(),
        },
    );

    assert_eq!(
        serde_json::to_value(request).expect("Ona create request should serialize"),
        json!({
            "spec": {
                "desiredPhase": "ENVIRONMENT_PHASE_RUNNING",
                "machine": {"class": "large"},
                "content": {
                    "initializer": {
                        "specs": [{
                            "git": {
                                "remoteUri": "https://github.com/openai/codex",
                                "targetMode": "CLONE_TARGET_MODE_REMOTE_BRANCH",
                                "cloneTarget": "feature"
                            }
                        }]
                    }
                }
            },
            "annotations": {
                "openai.com/codex-provider-id": "provider-id",
                "openai.com/codex-source-ref": "refs/heads/feature"
            }
        })
    );
}

#[test]
fn owned_ona_environment_maps_to_complete_common_record() {
    let environment = ona_environment(json!({
        "id": "environment-id",
        "metadata": {
            "annotations": {
                "openai.com/codex-provider-id": "provider-id",
                "openai.com/codex-source-ref": "refs/tags/v1.0.0"
            }
        },
        "spec": {
            "machine": {"class": "large"},
            "content": {
                "initializer": {
                    "specs": [{
                        "git": {
                            "remoteUri": "https://github.com/openai/codex",
                            "targetMode": "CLONE_TARGET_MODE_REMOTE_TAG",
                            "cloneTarget": "v1.0.0"
                        }
                    }]
                }
            }
        },
        "status": {
            "phase": "ENVIRONMENT_PHASE_STOPPED",
            "failureMessage": ["machine failed", "content failed"]
        }
    }));

    assert!(is_owned_environment("provider-id", &environment));
    assert_eq!(
        environment_from_ona("provider-id", environment).expect("owned Ona environment should map"),
        Environment {
            environment_ref: EnvironmentRef {
                provider_id: "provider-id".to_string(),
                environment_id: "environment-id".to_string(),
            },
            source: EnvironmentSource {
                repository_url: "https://github.com/openai/codex".to_string(),
                git_ref: "refs/tags/v1.0.0".to_string(),
            },
            resource_class: "large".to_string(),
            status: EnvironmentStatus {
                phase: EnvironmentPhase::Stopped,
                error: Some("machine failed; content failed".to_string()),
            },
        }
    );
}

#[test]
fn foreign_and_malformed_ona_environments_fail_closed() {
    let foreign = ona_environment(json!({
        "id": "foreign",
        "metadata": {
            "annotations": {"openai.com/codex-provider-id": "another-provider"}
        }
    }));
    assert!(!is_owned_environment("provider-id", &foreign));
    assert!(matches!(
        environment_from_ona("provider-id", foreign),
        Err(EnvironmentProviderAdapterError::EnvironmentNotFound { .. })
    ));

    let malformed = ona_environment(json!({
        "id": "malformed",
        "metadata": {
            "annotations": {"openai.com/codex-provider-id": "provider-id"}
        }
    }));
    assert!(matches!(
        environment_from_ona("provider-id", malformed),
        Err(EnvironmentProviderAdapterError::Internal { .. })
    ));
}

fn ona_environment(value: serde_json::Value) -> OnaEnvironment {
    serde_json::from_value(value).expect("Ona environment fixture should deserialize")
}
