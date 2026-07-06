# Cold Thread Resume Config Snapshot Wiring Spec

## Objective

Wire cold `thread/resume` by `thread_id` so app-server uses the persisted ThreadStore config snapshot as the base session configuration when resuming a thread after process or EC2 restart.

The remote ThreadStore already persists committed history and a durable config snapshot on the thread META item. The remaining gap is in app-server resume: the cold path reads stored history, but still builds `Config` from local/current defaults plus local `state_db` resume metadata. For remote threads with a persisted config snapshot, the ThreadStore snapshot must become the source of truth for resume-critical config. Local `state_db` remains only a legacy fallback for stored threads that do not have a ThreadStore config snapshot.

## Current Code Context

- `codex-rs/app-server/src/request_processors/thread_processor.rs`
  - `thread_resume_inner` handles all `thread/resume` requests.
  - `resume_running_thread` handles loaded/running thread rejoin and already uses `existing_thread.config_snapshot().await` for response data and override mismatch warnings.
  - The cold path calls `read_stored_thread_for_resume(... include_history=true)`, converts the `StoredThread` into `InitialHistory::Resumed`, builds request `ConfigOverrides`, calls `load_and_apply_persisted_resume_metadata`, and then calls `config_manager.load_for_cwd(...)`.
  - `load_and_apply_persisted_resume_metadata` currently reads local `state_db` metadata and merges model/provider/reasoning fallback into request overrides.
  - `build_thread_config_overrides` maps explicit `ThreadResumeParams` fields into `ConfigOverrides`.
  - `resume_thread_from_rollout`, `read_stored_thread_for_resume`, and `stored_thread_to_initial_history` form the cold stored-thread loading path.
- `codex-rs/app-server-protocol/src/protocol/v2/thread.rs`
  - `ThreadResumeParams` allows explicit overrides for model, model provider, service tier, cwd, runtime workspace roots, approval policy, approvals reviewer, sandbox, permissions, config map, base instructions, developer instructions, and personality.
  - `ThreadResumeResponse` reports the effective resumed model/provider/cwd/permissions fields.
- `codex-rs/thread-store/src/types.rs`
  - `StoredThread` is the thread-store read result and, after the prior snapshot work, should expose the optional persisted config snapshot.
  - Thread-store snapshot types must remain storage-level DTOs; `codex-thread-store` must not depend on `codex-core`.
- `codex-rs/core/src/codex_thread.rs`
  - `ThreadConfigSnapshot` is the core/app-server effective thread config view used for running-thread responses.
- `codex-rs/core/src/session/session.rs`
  - `SessionConfiguration::thread_config_snapshot` produces the live thread snapshot used by existing loaded-thread paths.

## Requirements

1. Cold `thread/resume` by `thread_id` must continue to load the stored thread through `ThreadStore.read_thread(... include_history=true)`.
2. If the returned `StoredThread` includes a persisted config snapshot:
   - Build the resumed `Config` from that snapshot as the base.
   - Apply explicit `thread/resume` request overrides on top where the API currently allows overrides.
   - Do not consult local `state_db` for resume-critical config.
   - Do not allow current machine defaults to silently replace persisted snapshot values.
   - Use current runtime only for machine-local or process-local resources that cannot be durable, such as auth managers, executable paths, handles, channels, and service instances.
3. If the returned `StoredThread` does not include a config snapshot:
   - Preserve existing legacy behavior.
   - Continue calling `load_and_apply_persisted_resume_metadata`.
   - Continue using current `config_manager.load_for_cwd(...)` fallback semantics.
4. Running-thread rejoin behavior must remain unchanged.
   - If the thread is already loaded/running, keep using `existing_thread.config_snapshot().await`.
   - Preserve existing mismatch logging and rejoin semantics for ignored overrides on loaded threads.
5. Explicit resume override semantics must remain intentional.
   - Allowed explicit overrides must apply over the persisted snapshot base.
   - Overrides that conflict with fields that must not change must fail clearly rather than being ignored.
   - Do not silently ignore explicit overrides in the cold snapshot-backed path.
6. Machine-local/runtime-only values must be explicit.
   - Persisted snapshot values define thread behavior.
   - Local runtime supplies auth, executable paths, process handles, live services, and environment-manager handles.
   - Persisted cwd/workspace roots/environment selections must be validated on the restarted EC2 instance.
   - If unavailable, return a clear error using existing JSON-RPC/config-load conventions.
7. Do not introduce a remote `state_db`.
   - ThreadStore is the source of truth for remote resume.
   - `state_db` stays local/legacy/projection fallback only.

## Non-Requirements

- Do not change the `thread/resume` wire API unless an existing response/schema shape is demonstrably insufficient.
- Do not change warm/running thread resume behavior except for shared helper extraction.
- Do not implement a remote `state_db`.
- Do not use local `state_db` as a secondary source of truth when `StoredThread.config_snapshot` exists.
- Do not change Thin DynamoDB/S3 persistence semantics beyond consuming the snapshot already returned by ThreadStore.
- Do not add silent path remapping across machines.

## Architecture

### Resume Decision Point

Make the source-of-truth decision explicit in `thread_resume_inner` after the stored thread is loaded and before `Config` is built.

Current cold flow:

```text
read stored thread + history
stored_thread_to_initial_history
build request ConfigOverrides
load_and_apply_persisted_resume_metadata from local state_db
config_manager.load_for_cwd(request_overrides, typesafe_overrides, history_cwd)
thread_manager.resume_thread_with_history(...)
```

Target cold flow:

```text
read stored thread + history
extract optional StoredThread.config_snapshot before consuming StoredThread
stored_thread_to_initial_history
build explicit request ConfigOverrides

if config_snapshot exists:
    convert ThreadStore snapshot -> core/app-server snapshot/base config inputs
    build Config from snapshot base + explicit resume overrides
    skip local state_db resume metadata
else:
    legacy path:
        load_and_apply_persisted_resume_metadata(...)
        config_manager.load_for_cwd(...)

thread_manager.resume_thread_with_history(...)
```

### Snapshot Conversion Boundary

Keep dependency direction clean:

- `codex-thread-store` owns storage DTOs.
- `codex-core` or `codex-app-server` converts storage DTOs into core/app-server config inputs.
- `codex-thread-store` must not depend on `codex-core`.

Add a helper in app-server or core, depending on which location avoids dependency churn:

```rust
fn build_config_from_thread_store_snapshot_for_resume(
    base_config_manager: &ConfigManager,
    request_config_overrides: Option<HashMap<String, serde_json::Value>>,
    request_typesafe_overrides: ConfigOverrides,
    stored_snapshot: StoredThreadConfigSnapshot,
    runtime_paths: RuntimeResumePaths,
) -> Result<Config, ResumeConfigError>
```

The helper should:

1. Convert the storage snapshot to a core/app-server `ThreadConfigSnapshot`-like effective view.
2. Build base config inputs from persisted values.
3. Overlay explicit `thread/resume` request overrides using existing `ConfigOverrides` behavior where possible.
4. Inject process-local executable paths from `arg0_paths` into the final overrides.
5. Validate cwd/workspace roots/environment selections before returning `Config`.

If `ConfigManager` cannot load from an in-memory base config today, add the narrowest helper needed rather than duplicating all config merge logic. The important invariant is that persisted snapshot values are the base values, not current config defaults.

### Fields Sourced From Snapshot

Snapshot-backed resume should source these from `StoredThread.config_snapshot` unless explicitly overridden by `ThreadResumeParams`:

- model
- model provider id
- service tier
- reasoning effort
- reasoning summary
- personality
- collaboration mode
- base instructions
- developer instructions
- compact prompt if present in the stored snapshot type
- approval policy
- approvals reviewer
- permission profile / active permission profile
- sandbox-equivalent permission behavior
- cwd
- environment selections
- runtime workspace roots
- profile workspace roots
- dynamic tools
- selected capability roots
- session source
- history mode
- forked-from thread id
- parent thread id
- thread source
- originator

Runtime/local fields still come from the current process:

- auth manager and current credentials
- model manager/service handles
- MCP/plugin/skills/extension managers
- event channels and listener state
- executable paths such as sandbox binaries and exec wrappers
- telemetry clients and request trace context
- local app-server client name/version
- current process shell discovery unless a future portable shell preference is persisted

### Explicit Override Semantics

The cold snapshot path should keep explicit override behavior aligned with current cold resume semantics:

- `model`, `model_provider`, `service_tier`, `cwd`, `runtime_workspace_roots`, `approval_policy`, `approvals_reviewer`, `sandbox`, `permissions`, `config`, `base_instructions`, `developer_instructions`, and `personality` should apply when they are already valid resume overrides.
- `sandbox` and `permissions` remain mutually exclusive.
- `runtime_workspace_roots` must remain absolute.
- `config` map overrides should apply after snapshot-derived base values, but must not be allowed to mutate fields that are intentionally immutable for a resumed thread.
- If a request override changes a field that must not change for correctness, return `invalid_request` with a clear message.

Fields that should be treated as immutable in the cold snapshot path unless an explicit product decision says otherwise:

- thread id
- session id
- history mode
- parent/fork relationship
- source identity for subagent lineage
- storage backend selection

Model/provider fallback policy:

- Do not silently replace the persisted model/provider with current defaults.
- If an explicit request override supplies a new model/provider, run normal validation for that override.
- If the persisted model/provider is unavailable in the current runtime, return a clear config/model resolution error unless an existing explicit `allow_provider_model_fallback`-style resume option is added later.

### Machine-Local Validation

Snapshot-backed cold resume should validate persisted local paths before session startup:

- `cwd` must resolve and be usable according to existing `Config` path validation.
- `workspace_roots` and `profile_workspace_roots` must be valid absolute paths.
- Environment selections must still be available/resolvable in the restarted environment.
- If validation fails, return an existing-style config load or invalid request error that explains the unavailable persisted field.

Legacy no-snapshot resume keeps existing fallback behavior, including `history_cwd` and `state_db` metadata.

### Response Construction

After `resume_thread_with_history` succeeds, continue using the live `codex_thread.config_snapshot().await` for the response. This confirms the actual resumed session state and avoids duplicating response derivation.

`load_thread_from_resume_source_or_send_internal` should continue to use the stored-thread source when available. If snapshot-backed config changes the fallback provider used for API thread projection, prefer the live resumed snapshot for response-visible config fields.

## Implementation Steps

1. Identify the persisted snapshot field on `StoredThread`.
   - Confirm `read_thread(... include_history=true)` returns `StoredThread.config_snapshot` for `AwsObjectLogThreadStore`.
   - Ensure local/legacy stores return `None`.

2. Add conversion helpers.
   - Add a boundary conversion from the ThreadStore persisted snapshot DTO to app-server/core `ThreadConfigSnapshot` or to a snapshot-backed config base type.
   - Keep conversions outside `codex-thread-store`.
   - Add clear errors for unknown snapshot versions or unsupported fields.

3. Add snapshot-backed config building.
   - Add a helper that builds resumed `Config` from persisted snapshot base plus explicit `ThreadResumeParams` overrides.
   - Reuse `ConfigManager` and `ConfigOverrides` where possible.
   - Ensure `arg0_paths` executable overrides still come from the current process.
   - Ensure current defaults do not fill resume-critical fields when a snapshot value exists.

4. Wire `thread_resume_inner`.
   - Preserve running-thread branch unchanged.
   - In the cold stored-thread branch, extract `config_snapshot` from `resume_source_thread`.
   - If present, call the snapshot-backed config builder and skip `load_and_apply_persisted_resume_metadata`.
   - If absent, run the existing legacy path.
   - Keep history/path resume behavior unchanged for explicit `history` and `path` cases unless they return a stored thread with a snapshot.

5. Validate override handling.
   - Preserve existing `sandbox` and `permissions` conflict validation.
   - Validate absolute workspace roots.
   - Decide and enforce immutable-field conflict behavior.
   - Ensure explicit overrides are reflected in the live `SessionConfiguredEvent` and `ThreadResumeResponse`.

6. Add tests.
   - Add focused app-server tests for cold resume snapshot behavior.
   - Add ThreadStore tests if `StoredThread.config_snapshot` read behavior needs coverage in the current branch.
   - Add regression coverage for legacy no-snapshot resume.

7. Validate.
   - Run `just fmt` in `codex-rs`.
   - Run `just test -p codex-app-server`.
   - Run `just test -p codex-thread-store`.
   - If app-server protocol/schema changes are made, run `just write-app-server-schema` and `just test -p codex-app-server-protocol`.
   - If shared core config code changes, ask before running the full workspace `just test`.

## Tests To Add

1. Cold resume uses ThreadStore config snapshot.
   - Mock or create a stored thread with committed history and a config snapshot.
   - Set current config defaults to different model/provider/cwd/approval/profile values.
   - Make local `state_db` missing or intentionally different.
   - Call `thread/resume`.
   - Assert the resumed live thread/session uses snapshot values.

2. Local `state_db` is not required when snapshot exists.
   - Use a remote-like stored thread with snapshot and no local `state_db` entry.
   - Resume succeeds.
   - Assert no fallback model/provider/reasoning metadata is required.

3. Legacy resume still works without snapshot.
   - Stored thread has history but `config_snapshot=None`.
   - Existing `state_db` fallback path still applies.
   - Current behavior and response fields remain unchanged.

4. Explicit overrides apply over snapshot.
   - Stored snapshot contains model/provider/cwd/approval/sandbox/personality values A.
   - Request supplies allowed overrides B.
   - Resume succeeds and live snapshot/response reflects B where allowed and A elsewhere.

5. Explicit invalid overrides fail clearly.
   - Request supplies an override that conflicts with immutable resume state.
   - Assert `thread/resume` returns `invalid_request` or the existing appropriate error type.

6. Running-thread rejoin remains unchanged.
   - Start/load a thread.
   - Call `thread/resume` for the already loaded thread.
   - Assert branch still uses `existing_thread.config_snapshot().await`.
   - Assert mismatch behavior for loaded threads remains warning/rejoin semantics, not cold snapshot rebuild.

7. Machine-local path validation.
   - Snapshot has unavailable cwd or workspace root.
   - Cold resume returns a clear error before session startup.
   - Legacy no-snapshot path remains unchanged.

8. Remote store integration if feasible.
   - Create a remote `AwsObjectLogThreadStore` thread with snapshot.
   - `read_thread(... include_history=true)` returns history and snapshot.
   - App-server cold resume consumes the snapshot.

## Success Criteria

- Cold `thread/resume` by `thread_id` uses `StoredThread.config_snapshot` as the base config whenever the snapshot exists.
- Local/current config defaults do not silently replace persisted snapshot values on snapshot-backed resume.
- Local `state_db` metadata is not consulted for resume-critical config when the snapshot exists.
- Threads without snapshots keep the existing legacy fallback path.
- Running-thread rejoin behavior is unchanged.
- Explicit resume overrides apply predictably over snapshot values or fail clearly when invalid.
- Persisted cwd/workspace/environment state is validated on the restarted machine.
- Tests cover snapshot-backed cold resume, no-state-db remote resume, legacy fallback, explicit overrides, running rejoin, and path validation.

## Open Questions

1. Does the persisted ThreadStore snapshot already include every field needed to build `Config`, or does it need a small expansion for base instructions, developer instructions, compact prompt, dynamic tools, selected capability roots, or profile workspace roots?
2. Should cold snapshot-backed resume allow changing model/provider via explicit override, or should those be immutable for remote continuity?
3. What exact error convention should unavailable persisted cwd/workspace roots use in app-server: `invalid_request`, config load error, or a more specific resume error?
4. Should explicit `path` resume use a stored config snapshot when the path maps to a remote-backed stored thread, or should path resume remain local-rollout/legacy only?
5. Should `ThreadResumeParams` grow an explicit `allowProviderModelFallback` later, or should persisted model/provider resolution fail hard when unavailable?
