use pretty_assertions::assert_eq;

use super::normalize_environment_provider_name;

#[test]
fn provider_names_are_normalized_case_insensitively() {
    assert_eq!(
        normalize_environment_provider_name("Ona Production"),
        normalize_environment_provider_name("ona production")
    );
}
