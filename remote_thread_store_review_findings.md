# Remote ThreadStore Reviewer Findings

## 1. Pathless remote `thread/resume` still fails after loading history

Status: **Valid**.

What I found:
- `thread_resume_inner` can load a stored thread by `thread_id` when no rollout path is provided via `read_stored_thread_for_resume(... path: None ...)`.
- `read_stored_thread_for_resume` correctly calls `ThreadStore.read_thread(... include_history=true)` for the pathless case.
- After `ThreadManager.resume_thread_with_history(...)` returns, the app-server destructures `SessionConfiguredEvent { rollout_path, .. }` and returns `internal_error("rollout path missing ...")` when it is `None`.
- Remote stores intentionally return `StoredThread.rollout_path = None`.
- The existing app-server test comment in `remote_thread_store.rs` already documents this gap: pathless resume currently fails later while assembling the response.

Relevant code:
- `codex-rs/app-server/src/request_processors/thread_processor.rs`
  - `thread_resume_inner`
  - `read_stored_thread_for_resume`
  - `stored_thread_to_initial_history`
  - `load_thread_from_resume_source_or_send_internal`
- `codex-rs/app-server/tests/suite/v2/remote_thread_store.rs`
- `codex-rs/thread-store/src/live_thread.rs`

Suggested fix:
- Stop requiring `SessionConfiguredEvent.rollout_path` for the cold resume response path.
- Change `load_thread_from_resume_source_or_send_internal` to accept `Option<&Path>` instead of `&Path`.
- For `InitialHistory::Resumed`, build the response thread from `resume_source_thread` or `ThreadStore.read_thread(thread_id)` by id, which the helper already mostly does.
- Only set `thread.path = Some(...)` when a rollout path exists; keep it `None` for remote stores.
- Preserve the existing rollout-path behavior for local stores.
- Add/adjust the remote app-server test so pathless resume succeeds instead of only asserting the history-bearing probe was reused.

## 2. Config snapshot is stored, but cold-resume replay is incomplete

Status: **Valid**.

What I found:
- `StoredThreadConfigSnapshot` contains more fields than the cold-resume overlay applies.
- `apply_thread_store_config_snapshot_to_resume_overrides` applies:
  - model
  - model provider
  - service tier
  - cwd
  - workspace roots
  - approval policy
  - approvals reviewer
  - permission profile
  - personality
  - ephemeral
  - reasoning effort
  - reasoning summary
- The stored snapshot also contains fields that the helper does not apply:
  - `active_permission_profile`
  - `profile_workspace_roots`
  - `collaboration_mode`
  - `session_source`
  - `history_mode`
  - `forked_from_thread_id`
  - `parent_thread_id`
  - `thread_source`
  - `originator`
- Some of these cannot currently be expressed through `ConfigOverrides`, so `ConfigManager.load_for_cwd(...)` falls back to current machine config/defaults for them.

Relevant code:
- `codex-rs/thread-store/src/types.rs`
  - `StoredThreadConfigSnapshot`
- `codex-rs/app-server/src/request_processors/thread_processor.rs`
  - `apply_thread_store_config_snapshot_to_resume_overrides`
  - `thread_resume_inner`
- `codex-rs/core/src/config/mod.rs`
  - `ConfigOverrides`
- `codex-rs/core/src/session/session.rs`
  - `SessionConfiguration::thread_config_snapshot`
  - `stored_thread_config_snapshot_from_session`

Suggested fix:
- Do not keep extending cold resume as a loose set of config override fields.
- Add a typed conversion from `StoredThreadConfigSnapshot` to the core/app-server resume inputs needed to reconstruct the effective session configuration.
- Either:
  - extend `ConfigOverrides` with typed fields for `active_permission_profile`, `profile_workspace_roots`, and `collaboration_mode`, then make `ConfigManager` honor them; or
  - add a dedicated core helper that builds a resumed `SessionConfiguration` from a persisted snapshot plus explicit resume overrides, bypassing local defaults for snapshot-owned fields.
- Keep explicit `thread/resume` request overrides higher precedence than the snapshot where the API currently allows overrides.
- Add tests where current local config differs from the stored snapshot for each of the missing fields and assert cold resume uses the stored value.

## 3. Developer/collaboration mode state is especially suspect

Status: **Valid**.

What I found:
- `SessionConfiguration` has a standalone `developer_instructions: Option<String>` field.
- `SessionConfiguration::thread_config_snapshot()` does **not** include standalone `developer_instructions`.
- `StoredThreadConfigSnapshot` therefore cannot restore standalone developer instructions during cold resume.
- `StoredThreadConfigSnapshot` does store `collaboration_mode`, but `apply_thread_store_config_snapshot_to_resume_overrides` does not apply it.
- Turn/context construction uses both:
  - `session_configuration.developer_instructions`
  - `session_configuration.collaboration_mode`
- `CodexThreadSettingsOverrides` can carry a full `collaboration_mode`, and live settings updates preserve it. The cold resume path does not currently have an equivalent persisted-snapshot application path.

Relevant code:
- `codex-rs/core/src/session/session.rs`
  - `SessionConfiguration`
  - `SessionConfiguration::thread_config_snapshot`
  - `stored_thread_config_snapshot_from_session`
- `codex-rs/core/src/session/turn_context.rs`
- `codex-rs/core/src/context_manager/updates.rs`
- `codex-rs/core/src/codex_thread.rs`
  - `CodexThreadSettingsOverrides`
  - `thread_settings_update`
- `codex-rs/app-server/src/request_processors/thread_processor.rs`
  - `apply_thread_store_config_snapshot_to_resume_overrides`

Suggested fix:
- Add `developer_instructions: Option<String>` to `ThreadConfigSnapshot` and `StoredThreadConfigSnapshot::V1` or introduce `V2` if compatibility needs an explicit schema bump.
- Persist the standalone developer instructions captured in `SessionConfiguration`.
- Apply the persisted `collaboration_mode` during cold resume, not just model/reasoning fields derived from it.
- Preserve explicit resume overrides:
  - explicit `model` / `reasoning_effort` should update the stored collaboration mode using the existing `CollaborationMode::with_updates(...)` path
  - explicit `developer_instructions` should override the persisted standalone developer instructions
  - if the API grows an explicit collaboration mode override, that should take precedence over the snapshot
- Add tests proving:
  - stored collaboration mode survives cold resume when current defaults differ
  - standalone developer instructions survive cold resume when current defaults differ
  - explicit resume overrides still win
  - model context after cold resume contains the expected developer and collaboration-mode instructions

## 4. Some local `state_db`-backed features remain local-only

Status: **Valid as a scope caveat**.

What I found:
- Session init only gets a SQLite `state_db` handle when the session is non-ephemeral and the configured `ThreadStore` downcasts to `LocalThreadStore`.
- For `AwsObjectLogThreadStore`, `state_db_fut` returns `None`, so the resumed session is created with `persistent_thread_state_available = false` and `SessionServices.state_db = None`.
- `ThreadManager` only constructs the default `LocalAgentGraphStore` from a local `state_db` handle.
- Several runtime features still read or write through `SessionServices.state_db`, including agent job reporting/spawning, memory-mode pollution markers, stage-1 memory citation tracking, and shell snapshot cleanup/projection.
- This is not necessarily a blocker for the minimal durable ThreadStore claim if the claim is limited to "a root thread can be resumed by id from committed remote history and continue." It is a blocker for any stronger claim that every adjacent local SQLite-backed thread feature is remotely reconstructed.

Relevant code:
- `codex-rs/core/src/session/session.rs`
  - `state_db_fut`
  - `ThreadStartInput { persistent_thread_state_available: ... }`
  - `SessionServices { state_db: ... }`
  - `ShellSnapshot::new(...)`
- `codex-rs/core/src/thread_manager.rs`
  - `thread_store_from_config`
  - `local_agent_graph_store_from_state_db`
- `codex-rs/core/src/state/service.rs`
  - `SessionServices::state_db`
- `codex-rs/core/src/tools/handlers/agent_jobs.rs`
  - `required_state_db`
- `codex-rs/core/src/tools/handlers/agent_jobs/report_agent_job_result.rs`
- `codex-rs/core/src/tools/handlers/agent_jobs/spawn_agents_on_csv.rs`
- `codex-rs/core/src/stream_events_utils.rs`
- `codex-rs/core/src/mcp_tool_call.rs`
- `codex-rs/core/src/tools/registry.rs`
- `codex-rs/core/src/shell_snapshot.rs`

Suggested fix:
- For v1, document and enforce the support boundary explicitly: remote ThreadStore supports durable root-thread history/config replay, but SQLite-only adjunct state is unavailable unless separately migrated.
- Make features that require `state_db` fail with clear capability errors for remote-store sessions instead of failing later with generic messages.
- Move durable projections that must survive remote resume into one of:
  - the ThreadStore metadata/config snapshot when they are thread-level state;
  - committed rollout/history items when they are part of replayable session behavior;
  - a separate remote projection service/store when they are query/update-heavy auxiliary state.
- Add remote-store integration tests for the intended v1 boundary:
  - idle root thread resumes and can continue without `state_db`
  - agent-job tools return clear unsupported/capability errors when `state_db` is unavailable
  - thread tree/sub-agent listing behavior is either backed by remote projections or explicitly marked unsupported for remote stores

## Recommended fix order

1. **Fix pathless resume response assembly first.** Remote resume by `thread_id` can load history but cannot complete the app-server response while `rollout_path` is required.
2. **Make config snapshot replay complete.** Cold resume should use the persisted ThreadStore snapshot as the base for all snapshot-owned fields, with explicit resume overrides applied on top.
3. **Persist and replay developer/collaboration state.** Add standalone `developer_instructions` to the durable snapshot and apply persisted `collaboration_mode`.
4. **Define the `state_db` boundary.** Either scope remote v1 to durable root-thread replay or migrate required SQLite projections to remote-backed storage before claiming broader remote session reconstruction.
