use pretty_assertions::assert_eq;

use super::*;

#[test]
fn bare_environment_id_is_a_static_alias() {
    assert_eq!(
        canonical_environment_id("local").expect("canonical id"),
        LOCAL_ENVIRONMENT_ID
    );
    assert_eq!(
        split_environment_id("local").expect("split id"),
        (STATIC_ENVIRONMENT_PROVIDER_ID, "local")
    );
}

#[test]
fn qualified_environment_id_splits_only_on_first_slash() {
    assert_eq!(
        split_environment_id("provider/environment/with/slashes").expect("split id"),
        ("provider", "environment/with/slashes")
    );
    assert_eq!(
        canonical_environment_id("provider/environment/with/slashes").expect("canonical id"),
        "provider/environment/with/slashes"
    );
}

#[test]
fn static_environment_id_rejects_another_provider() {
    let error = canonical_static_environment_id("ona/environment").expect_err("provider mismatch");

    assert_eq!(
        error.to_string(),
        "exec-server protocol error: environment `ona/environment` does not belong to the static provider"
    );
}

#[test]
fn environment_id_rejects_empty_components() {
    for environment_id in ["", "/environment", "provider/"] {
        canonical_environment_id(environment_id).expect_err("invalid environment id");
    }
}
