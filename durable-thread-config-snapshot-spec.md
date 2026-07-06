# Durable Remote Thread Config Snapshot Spec

## Objective

Extend `AwsObjectLogThreadStore` so a replacement Codex instance can recreate a remote thread session from durable state alone. The store must persist a versioned, non-secret effective thread/session configuration snapshot on the DynamoDB thread `META` item at thread creation time, then read that snapshot during resume before constructing the replacement `SessionConfiguration`.

Committed rollout history remains in S3 objects referenced by DynamoDB commit records. The configuration snapshot is metadata-sized and should live directly in DynamoDB unless it exceeds DynamoDB item limits, in which case DynamoDB stores a small verified S3 pointer.

## Current Code Context

- `ThreadStore` is the storage-neutral boundary in `codex-rs/thread-store/src/store.rs`. It supports `create_thread`, `resume_thread`, `append_items`, `flush_thread`, `load_history`, `read_thread`, list/search, metadata update, archive, unarchive, and delete.
- `CreateThreadParams` and `ResumeThreadParams` are defined in `codex-rs/thread-store/src/types.rs`. `CreateThreadParams` already carries thread identity, source, base instructions, dynamic tools, selected capability roots, history mode, initial window id, and lightweight persistence metadata. `ResumeThreadParams` currently carries thread id, optional rollout path, optional already-loaded history, archive policy, and metadata for future writes.
- `AwsObjectLogThreadStore` is in `codex-rs/thread-store/src/remote/aws_object_log.rs`. The prototype models the DynamoDB `META` item as `AwsObjectLogThreadHead`, commit pointer items as `AwsObjectLogCommitPointer`, and S3 payloads as immutable commit payload objects.
- `SessionConfiguration` is built in `codex-rs/core/src/session/mod.rs` from the current process `Config`, conversation history, environment selections, auth/model managers, and runtime services before persistence is opened.
- `SessionConfiguration::thread_config_snapshot()` already produces a `ThreadConfigSnapshot` in `codex-rs/core/src/session/session.rs`, and `ThreadConfigSnapshot` is defined in `codex-rs/core/src/codex_thread.rs`. That snapshot is useful app-server state, but it is not currently a durable ThreadStore payload and is not versioned for storage.
- `thread_manager.rs` converts a `StoredThread` with `include_history=true` into `InitialHistory::Resumed`, so resume currently replays committed history but still resolves effective config from the replacement process defaults.

## Requirements

1. Add a versioned durable snapshot type for the non-secret effective session/thread config needed to recreate `SessionConfiguration`.
2. Persist the snapshot during remote thread creation atomically with the DynamoDB `META` item.
3. Read the snapshot from `META` during remote resume and use it to reconstruct or override the replacement session configuration before `SessionConfiguration` is finalized.
4. Preserve legacy behavior for threads that do not have a snapshot: resume must continue using existing history-derived values plus the current process config defaults.
5. Do not persist auth tokens, API keys, refresh tokens, live handles, process IDs, channels, thread-local task state, model manager caches, runtime service objects, or machine-local `codex_home`.
6. Be explicit about machine-local fields:
   - Persist effective `cwd`, environment selections, workspace roots, and profile workspace roots because they affect runtime behavior.
   - On snapshot-backed resume, validate those paths/selections on the replacement machine before starting the session.
   - If validation fails, fail closed with a clear `InvalidRequest`/startup error. Do not silently replace them with current machine defaults.
   - Legacy threads without snapshots keep the existing fallback behavior.
7. If the serialized snapshot cannot fit safely in the DynamoDB `META` item, store the snapshot body in S3 and store only a DynamoDB pointer containing bucket/key, snapshot version, byte length, sha256, and created_at.
8. Snapshot-backed resume must use persisted config rather than current machine defaults for model, provider id, service tier, reasoning settings, approval policy, permission profile, collaboration mode, personality, session/thread source metadata, dynamic tools, selected capability roots, and environment/path selections.

## Non-Requirements

- Do not build a general configuration sync service.
- Do not make local `LocalThreadStore` depend on remote snapshot semantics.
- Do not migrate all existing local rollout files immediately.
- Do not persist secret-bearing provider definitions or credentials.
- Do not implement silent path remapping in the first version.
- Do not use S3 as the primary snapshot location for normal-sized snapshots.

## Architecture

### Storage Model

The DynamoDB thread `META` item is the source of truth for the durable config snapshot.

Embedded snapshot case:

```json
{
  "pk": "THREAD#<thread_id>",
  "sk": "META",
  "thread_id": "<thread_id>",
  "session_id": "<session_id>",
  "history_mode": "legacy",
  "head_seq": 1,
  "metadata": { "...": "existing thread metadata projection" },
  "config_snapshot": {
    "schema": "codex.thread_config_snapshot.v1",
    "created_at": "2026-07-06T12:00:00Z",
    "snapshot": {
      "model": "gpt-5",
      "model_provider_id": "openai",
      "service_tier": "auto",
      "approval_policy": "on-request",
      "approvals_reviewer": "user",
      "permission_profile": { "...": "materialized non-secret profile" },
      "active_permission_profile": null,
      "windows_sandbox_level": "default",
      "environments": { "...": "TurnEnvironmentSelections" },
      "workspace_roots": ["/workspaces/codex"],
      "profile_workspace_roots": ["/workspaces/codex"],
      "reasoning_effort": "medium",
      "reasoning_summary": null,
      "personality": null,
      "collaboration_mode": { "...": "effective collaboration mode" },
      "base_instructions": "...",
      "developer_instructions": null,
      "compact_prompt": null,
      "dynamic_tools": [],
      "selected_capability_roots": [],
      "session_source": "cli",
      "history_mode": "legacy",
      "forked_from_thread_id": null,
      "parent_thread_id": null,
      "thread_source": null,
      "originator": "codex_cli"
    }
  }
}
```

Overflow pointer case:

```json
{
  "pk": "THREAD#<thread_id>",
  "sk": "META",
  "thread_id": "<thread_id>",
  "config_snapshot_ref": {
    "bucket": "codex-thread-payloads",
    "key": "threads/<thread_id>/config-snapshots/v1/<snapshot_id>.json.zst",
    "snapshot_version": 1,
    "byte_length": 524288,
    "sha256": "b8e2...",
    "created_at": "2026-07-06T12:00:00Z"
  }
}
```

For the current in-memory prototype, add equivalent fields to `AwsObjectLogThreadHead`:

- `config_snapshot: Option<StoredThreadConfigSnapshot>`
- `config_snapshot_ref: Option<StoredThreadConfigSnapshotRef>` only if S3 overflow is implemented in the prototype

### Snapshot Type Shape

Add storage DTOs in `codex-rs/thread-store/src/types.rs` or a new sibling module so the thread-store crate remains independent of `codex-core`.

Proposed public types:

```rust
pub struct StoredThreadConfigSnapshot {
    pub version: ThreadConfigSnapshotVersion,
    pub created_at: DateTime<Utc>,
    pub payload: StoredThreadConfigSnapshotPayload,
}

pub enum ThreadConfigSnapshotVersion {
    V1,
}

pub enum StoredThreadConfigSnapshotPayload {
    V1(StoredThreadConfigSnapshotV1),
}

pub struct StoredThreadConfigSnapshotV1 {
    pub model: String,
    pub model_provider_id: String,
    pub service_tier: Option<String>,
    pub approval_policy: AskForApproval,
    pub approvals_reviewer: ApprovalsReviewer,
    pub permission_profile: PermissionProfile,
    pub active_permission_profile: Option<ActivePermissionProfile>,
    pub windows_sandbox_level: WindowsSandboxLevel,
    pub environments: TurnEnvironmentSelections,
    pub workspace_roots: Vec<AbsolutePathBuf>,
    pub profile_workspace_roots: Vec<AbsolutePathBuf>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub reasoning_summary: Option<ReasoningSummary>,
    pub personality: Option<Personality>,
    pub collaboration_mode: CollaborationMode,
    pub base_instructions: String,
    pub developer_instructions: Option<String>,
    pub compact_prompt: Option<String>,
    pub dynamic_tools: Vec<DynamicToolSpec>,
    pub selected_capability_roots: Vec<SelectedCapabilityRoot>,
    pub session_source: SessionSource,
    pub history_mode: ThreadHistoryMode,
    pub forked_from_thread_id: Option<ThreadId>,
    pub parent_thread_id: Option<ThreadId>,
    pub thread_source: Option<ThreadSource>,
    pub originator: String,
}
```

Do not include:

- `ModelProviderInfo` as a whole, because provider definitions can carry secret lookup behavior and process-local state.
- auth manager state, auth tokens, refresh tokens, account ids, API keys, or credential paths.
- `original_config_do_not_use`.
- `codex_home`.
- model manager, MCP manager, plugins manager, skills service, extension runtime, event channels, task handles, process ids, or shell handles.
- `user_shell_override`, unless a future version stores a portable declarative shell preference. The first version should resolve shell locally on resume.
- metrics client names and app-server client version. Those are client/process metadata, not durable thread configuration.

### API Changes

Add the snapshot to thread creation:

```rust
pub struct CreateThreadParams {
    ...
    pub config_snapshot: Option<StoredThreadConfigSnapshot>,
}
```

Add a read path that does not require listing or exposing snapshots in general thread list responses:

```rust
pub struct ReadThreadConfigSnapshotParams {
    pub thread_id: ThreadId,
    pub include_archived: bool,
}

pub struct ReadThreadConfigSnapshotResponse {
    pub thread_id: ThreadId,
    pub config_snapshot: Option<StoredThreadConfigSnapshot>,
}

pub trait ThreadStore {
    ...
    fn read_thread_config_snapshot(
        &self,
        params: ReadThreadConfigSnapshotParams,
    ) -> ThreadStoreFuture<'_, ReadThreadConfigSnapshotResponse> {
        Box::pin(async move {
            Ok(ReadThreadConfigSnapshotResponse {
                thread_id: params.thread_id,
                config_snapshot: None,
            })
        })
    }
}
```

Rationale:

- `ThreadStore` cannot depend on `codex-core::ThreadConfigSnapshot`.
- `StoredThread` is used by list/search/read surfaces; adding config snapshots there risks accidentally returning durable config in APIs that do not need it.
- A dedicated method keeps the snapshot opt-in and makes the resume call site explicit.
- Local and legacy stores can use the default `None` implementation.

### Core Conversion

Add conversion in `codex-core` from `SessionConfiguration` to `StoredThreadConfigSnapshot`:

- Extend or replace `SessionConfiguration::thread_config_snapshot()` so it can produce the storage DTO for creation.
- Keep existing `ThreadConfigSnapshot` for app-server previews if needed.
- Add a conversion from `StoredThreadConfigSnapshotV1` back into the values used to build `SessionConfiguration`.

The resume construction should work as follows:

1. `thread_manager` reads the stored thread with `include_history=true` as it does today.
2. For `InitialHistory::Resumed`, before `Codex::spawn` finalizes `SessionConfiguration`, call `thread_store.read_thread_config_snapshot(thread_id, include_archived=true)`.
3. If the snapshot is present:
   - Validate snapshot version.
   - Validate machine-local paths and environment selections.
   - Resolve `model_provider_id` against the replacement machine config/model provider registry.
   - Require the replacement process to have usable auth for that provider through the normal auth manager path.
   - Apply persisted effective fields instead of current process defaults.
4. If the snapshot is absent:
   - Preserve current resume behavior.
   - Continue deriving what is available from rollout history, `SessionMeta`, and current config.

### DynamoDB Create Algorithm

Normal embedded case:

1. Core builds `SessionConfiguration`.
2. Core converts it into `StoredThreadConfigSnapshot`.
3. Core calls `LiveThread::create` with `CreateThreadParams { config_snapshot: Some(snapshot), ... }`.
4. `AwsObjectLogThreadStore::create_thread` serializes the snapshot.
5. If the serialized snapshot plus the rest of `META` is under the configured DynamoDB safety threshold, include it directly on `META`.
6. In the real DynamoDB implementation, create uses one `TransactWriteItems` operation for:
   - conditional put `META` with `attribute_not_exists(pk)`
   - put initial session-meta commit pointer/idempotency record as currently modeled
   - optional list projection item
7. Return success only after the transaction commits.

Overflow case:

1. Serialize and compress snapshot.
2. Compute `sha256` and byte length.
3. Put the S3 snapshot object under an immutable key using conditional create semantics.
4. Store `config_snapshot_ref` on `META` in the same DynamoDB transaction as thread creation.
5. If DynamoDB transaction fails after S3 PUT, the S3 object is orphaned and ignored because no committed `META` points to it. Lifecycle/reconciliation can remove it later.
6. On read, fetch the S3 object, validate byte length and sha256, then deserialize by version.

### Resume Algorithm

1. Resolve the thread id and load committed history as today.
2. Read `META.config_snapshot` or `META.config_snapshot_ref`.
3. If snapshot is missing, use legacy resume behavior.
4. If snapshot is present:
   - Validate supported version.
   - Validate checksum for overflow payloads.
   - Validate `history_mode` matches the stored thread `history_mode`; mismatch is corruption and must fail.
   - Validate `parent_thread_id`, `forked_from_thread_id`, `thread_source`, and `session_source` do not conflict with caller-supplied resume request fields.
   - Resolve model provider by `model_provider_id`; if unavailable, fail with a clear error.
   - Resolve model by stored `model`; do not silently fall back to the current default model. If the model is unavailable and existing model-manager behavior requires fallback, log/return an explicit "stored model unavailable" error unless a future explicit resume override allows fallback.
   - Validate stored environment selections and paths against the replacement environment manager.
   - Build `SessionConfiguration` from persisted effective values plus process-local services/auth managers.
5. Open persistence with `ResumeThreadParams` as today, but derive its metadata from the snapshot-backed configuration rather than current defaults.

## Machine-Local Path and Environment Policy

Snapshot-backed threads should be deterministic by default.

- Stored `cwd`, `workspace_roots`, `profile_workspace_roots`, and environment selection paths must be validated on resume.
- If a selected environment id no longer exists, if a required cwd cannot be resolved, or if a workspace root is unavailable, resume fails before model turn startup.
- The first version should not auto-map paths between machines.
- The error should name the invalid class of state, for example `stored cwd is unavailable`, without leaking sensitive path details beyond what the local user already configured.
- A later version may add explicit resume overrides for environment/path remapping. That should be an explicit API parameter and should update or supersede the stored snapshot only if product semantics require it.

## Legacy Compatibility

- Existing remote threads without `config_snapshot` continue to resume using current behavior.
- Existing local rollouts imported into the remote store may have `config_snapshot=None` unless the importer is explicitly given an effective config snapshot at import time.
- `SessionMeta` remains in committed rollout history for history compatibility, list/read metadata, and older reconstruction paths.
- The versioned snapshot is additive. Unknown future versions fail clearly rather than being partially interpreted.

## Security and Privacy Constraints

- The stored type should make secret persistence difficult by construction. Do not use `Config` or `ModelProviderInfo` as the serialized shape.
- Add tests that serialize a snapshot built from a config containing sentinel secret strings and assert the serialized snapshot does not contain those strings.
- Avoid generic `serde_json::Value` for the primary payload because it makes accidental secret inclusion harder to review.
- If logging snapshot read/write failures, log version, byte length, checksum prefix, and thread id only. Do not log the snapshot body.
- If using S3 overflow, require SSE/KMS configuration consistent with the rest of the object-log store.

## Implementation Steps

1. Add storage DTOs to `codex-thread-store`.
   - Add `StoredThreadConfigSnapshot`, `StoredThreadConfigSnapshotV1`, overflow reference type, read params/response, and optional `CreateThreadParams.config_snapshot`.
   - Use explicit versioning and serde rename policy.
   - Keep fields limited to protocol/config utility types already safe for thread-store dependencies.

2. Extend `AwsObjectLogThreadStore`.
   - Add snapshot fields to `AwsObjectLogThreadHead`.
   - Persist the snapshot in `create_thread` before inserting the head/META equivalent.
   - Implement `read_thread_config_snapshot`.
   - Implement overflow only if needed now; otherwise add a configurable hard error when the serialized snapshot exceeds the DynamoDB safety threshold and leave S3 overflow as a follow-up. If overflow is implemented, validate byte length and sha256 on read.

3. Add core snapshot conversion.
   - Convert `SessionConfiguration` into `StoredThreadConfigSnapshotV1`.
   - Include effective base instructions, developer instructions, compact prompt, dynamic tools, selected capability roots, model/provider settings, approvals, permissions, collaboration settings, environment selections, history mode, source metadata, and originator.
   - Exclude secrets and process-local state.

4. Wire creation.
   - In `Session::new`, when building `CreateThreadParams`, include `config_snapshot: Some(...)` for non-ephemeral new/forked/cleared histories.
   - Keep local and in-memory stores compatible with the optional field.

5. Wire resume.
   - In `thread_manager` or `Codex::spawn`, read the stored snapshot before finalizing `SessionConfiguration` for resumed threads.
   - Apply snapshot-backed values instead of current process defaults.
   - Validate paths, environment selections, model provider id, model, history mode, and thread relation fields.
   - Preserve current behavior when the snapshot is absent.

6. Add tests.
   - Unit tests in `codex-rs/thread-store/src/remote/aws_object_log_tests.rs`.
   - Core integration or focused session/thread-manager tests proving snapshot-backed resume uses persisted values.
   - Serialization security tests for secret exclusion.
   - Overflow tests if S3 overflow is implemented.

7. Run required validation.
   - `just fmt` from `codex-rs`.
   - `just test -p codex-thread-store`.
   - Targeted `codex-core` tests covering creation/resume conversion.
   - `just fix -p codex-thread-store` and `just fix -p codex-core` if those crates changed.
   - Ask before running the full workspace `just test`.

## Test Plan

Required tests:

1. Snapshot is written on create.
   - Create an `AwsObjectLogThreadStore` thread with a snapshot.
   - Read `read_thread_config_snapshot`.
   - Assert full snapshot equality.

2. Snapshot is read on resume.
   - Create a thread, shut down/discard live context, resume it.
   - Assert the read snapshot is still available and unchanged.

3. Resume uses persisted config rather than current machine defaults.
   - Start a thread with model/provider/approval/profile values A.
   - Change replacement config defaults to values B.
   - Resume the thread.
   - Assert outbound session/model configuration uses A.

4. Legacy threads without snapshots still resume.
   - Create/import a thread with no snapshot.
   - Resume with current config.
   - Assert behavior matches existing resume path.

5. Secrets/auth material are not persisted.
   - Build config/auth/provider inputs with sentinel secret strings.
   - Serialize the stored snapshot.
   - Assert the sentinel strings are absent.

6. Path/environment validation.
   - Snapshot-backed resume with valid paths succeeds.
   - Snapshot-backed resume with unavailable cwd/workspace/environment selection fails before the session starts.
   - Legacy no-snapshot resume keeps current fallback behavior.

7. History mode consistency.
   - Stored snapshot history mode must match thread META/history mode.
   - Mismatch fails as corruption/invalid state.

8. Oversized snapshot behavior.
   - If overflow is implemented, force a small threshold, assert snapshot body goes to S3, DynamoDB/META stores only pointer metadata, read validates sha256 and byte length, and corrupted object read fails.
   - If overflow is not implemented in this stage, assert oversized snapshots fail at create with a clear error and no committed thread META.

## Success Criteria

- A remote thread created with `AwsObjectLogThreadStore` has a versioned config snapshot persisted on the DynamoDB `META` item or a verified S3 overflow pointer on `META`.
- A replacement Codex instance can resume the thread using committed history plus the persisted snapshot without relying on current process defaults for effective session configuration.
- Threads without snapshots still resume using the existing legacy behavior.
- Snapshot-backed resume fails clearly when persisted model/provider/path/environment state cannot be satisfied on the replacement machine.
- The serialized snapshot contains no auth tokens, API keys, live handles, process ids, channels, or machine-local runtime state.
- Tests prove create, resume, persisted-config precedence, legacy compatibility, secret exclusion, and oversized behavior.

## Open Questions

1. Should the first implementation include S3 overflow immediately, or should it fail clearly above a conservative DynamoDB item-size threshold and add overflow in a later patch?
2. Should snapshot-backed resume allow explicit user-provided path/environment overrides in v1, or fail closed until an override API is designed?
3. Should stored unavailable model ids fail hard, or should there be an explicit opt-in fallback to the provider default?
4. Should imported local rollout history be allowed to attach a snapshot supplied by the importing process?
5. Should the existing app-server `ThreadConfigSnapshot` be unified with the durable storage DTO, or should it remain a separate presentation/runtime snapshot with explicit conversions?
