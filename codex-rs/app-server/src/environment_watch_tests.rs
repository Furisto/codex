use pretty_assertions::assert_eq;

use super::*;
use codex_environment_provider::EnvironmentPhase;
use codex_environment_provider::EnvironmentRef;
use codex_environment_provider::EnvironmentSource;
use codex_environment_provider::EnvironmentStatus;

#[test]
fn explicit_and_provider_events_are_normalized_against_one_projection() {
    let creating = environment(EnvironmentPhase::Creating);
    let running = environment(EnvironmentPhase::Running);
    let environment_ref = running.environment_ref.clone();
    let mut projection = BTreeMap::new();

    assert_eq!(
        apply_event(
            &mut projection,
            EnvironmentLifecycleEvent::Created(creating.clone())
        ),
        Some(EnvironmentLifecycleEvent::Created(creating.clone()))
    );
    assert_eq!(
        apply_event(
            &mut projection,
            EnvironmentLifecycleEvent::Created(creating)
        ),
        None
    );
    assert_eq!(
        apply_event(
            &mut projection,
            EnvironmentLifecycleEvent::Created(running.clone())
        ),
        Some(EnvironmentLifecycleEvent::Updated(running))
    );
    assert_eq!(
        apply_event(
            &mut projection,
            EnvironmentLifecycleEvent::Deleted(environment_ref.clone())
        ),
        Some(EnvironmentLifecycleEvent::Deleted(environment_ref.clone()))
    );
    assert_eq!(
        apply_event(
            &mut projection,
            EnvironmentLifecycleEvent::Deleted(environment_ref)
        ),
        None
    );
}

fn environment(phase: EnvironmentPhase) -> Environment {
    Environment {
        environment_ref: EnvironmentRef {
            provider_id: "provider".to_string(),
            environment_id: "environment".to_string(),
        },
        source: EnvironmentSource {
            repository_url: "https://example.com/repository.git".to_string(),
            git_ref: "main".to_string(),
        },
        resource_class: "large".to_string(),
        status: EnvironmentStatus { phase, error: None },
    }
}
