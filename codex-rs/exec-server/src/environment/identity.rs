use crate::ExecServerError;

/// Stable ID of the built-in static environment provider.
pub const STATIC_ENVIRONMENT_PROVIDER_ID: &str = "static";
/// Canonical ID of the built-in local environment.
pub const LOCAL_ENVIRONMENT_ID: &str = "static/local";
/// Canonical ID used by the legacy remote-environment configuration.
pub const REMOTE_ENVIRONMENT_ID: &str = "static/remote";

pub(super) const STATIC_LOCAL_ENVIRONMENT_ID: &str = "local";

/// Returns the canonical `providerId/environmentId` representation.
///
/// Bare legacy IDs are interpreted as environments owned by the built-in
/// static provider. Provider-native IDs may contain additional slashes.
pub fn canonical_environment_id(environment_id: &str) -> Result<String, ExecServerError> {
    let (provider_id, provider_environment_id) = split_environment_id(environment_id)?;
    Ok(format!("{provider_id}/{provider_environment_id}"))
}

/// Returns the provider ID and provider-native environment ID.
///
/// The canonical separator is the first slash. Bare legacy IDs are returned
/// with the built-in static provider ID.
pub fn split_environment_id(environment_id: &str) -> Result<(&str, &str), ExecServerError> {
    if environment_id.is_empty() {
        return Err(ExecServerError::Protocol(
            "environment id cannot be empty".to_string(),
        ));
    }
    match environment_id.split_once('/') {
        Some(("", _)) => Err(ExecServerError::Protocol(format!(
            "environment id `{environment_id}` has an empty provider id"
        ))),
        Some((_, "")) => Err(ExecServerError::Protocol(format!(
            "environment id `{environment_id}` has an empty provider environment id"
        ))),
        Some((provider_id, provider_environment_id)) => Ok((provider_id, provider_environment_id)),
        None => Ok((STATIC_ENVIRONMENT_PROVIDER_ID, environment_id)),
    }
}

/// Canonicalizes an environment ID and requires it to belong to the static provider.
pub fn canonical_static_environment_id(environment_id: &str) -> Result<String, ExecServerError> {
    let (provider_id, provider_environment_id) = split_environment_id(environment_id)?;
    if provider_id != STATIC_ENVIRONMENT_PROVIDER_ID {
        return Err(ExecServerError::Protocol(format!(
            "environment `{environment_id}` does not belong to the static provider"
        )));
    }
    Ok(format!(
        "{STATIC_ENVIRONMENT_PROVIDER_ID}/{provider_environment_id}"
    ))
}

pub(super) fn qualify_static_environment_id(environment_id: &str) -> String {
    format!("{STATIC_ENVIRONMENT_PROVIDER_ID}/{environment_id}")
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
