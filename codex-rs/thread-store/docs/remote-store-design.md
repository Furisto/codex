# Remote ThreadStore Design

## Summary

Codex needs to resume a thread after the original process or machine is gone. The remote ThreadStore feature provides durable thread history and durable non-secret session configuration using an AWS object-log backend:

- DynamoDB stores the thread metadata row, sequence state, idempotency records, and compact item index.
- S3 stores large committed rollout/history payloads.
- App-server cold `thread/resume` loads committed history and configuration through `ThreadStore.read_thread(... include_history=true)`.
- Core recreates a `SessionConfiguration` from the persisted snapshot plus explicit resume overrides.

The target outcome is that an idle root thread can be resumed by `thread_id` on a replacement instance without a local rollout file or local `state_db`.

## Goals

- Provide durable per-thread committed history for remote resume.
- Persist enough non-secret session configuration to recreate the effective session behavior.
- Keep local JSONL/SQLite behavior working for legacy/local stores.
- Support pathless remote stores where no local rollout path exists.
- Preserve explicit `thread/resume` overrides where the API already supports them.
- Keep the customer-account deployment footprint small: one DynamoDB table, one S3 bucket/prefix, IAM/KMS, and optional LocalStack tests.

## Why DynamoDB + S3

The short version: **DynamoDB is the commit authority; S3 is the payload store.**

We are not choosing DynamoDB + S3 because it is the fewest moving parts. We are choosing it because it is the smallest customer-account AWS design that gives us both:

- a reliable commit point for thread history
- cheap durable storage for large rollout payloads

The ThreadStore has two very different storage needs:

- small, correctness-critical coordination state
- potentially large history and snapshot payloads

DynamoDB is a good fit for the first need. The store needs to make ordered append decisions, enforce idempotency keys, advance committed sequence state, update metadata projections, and read/list threads by stable keys. Those are small records where conditional writes and transactions are the important property.

S3 is a good fit for the second need. Rollout batches, snapshots, and compacted history can grow beyond what should be stored directly in DynamoDB items. S3 gives durable low-cost blob storage, lifecycle policies, and archive behavior without increasing the size or write cost of the commit/index records.

The split is intentional and maps directly to the append protocol:

1. Write the append batch payload to S3 under a deterministic key.
2. Commit the append in DynamoDB with a conditional or transactional write:
   - record the idempotency key
   - reserve/advance the per-thread sequence range
   - write item index records that point at the S3 object
   - update the thread `META` projection and committed sequence
3. Treat the append as visible only after the DynamoDB commit succeeds.
4. On retry, look up the idempotency key in DynamoDB and return the original committed sequence range.

That means S3 objects can exist before commit, but they are not part of thread history until DynamoDB says they are. Readers reconstruct history from DynamoDB committed index state and fetch only the referenced S3 payloads.

This gives us a clean correctness boundary:

- DynamoDB is the source of truth for what is committed.
- S3 stores the bytes referenced by committed DynamoDB records.

The important point is that S3 is never asked to decide ordering, idempotency, visibility, or metadata projection. DynamoDB owns those decisions.

This avoids the worst tradeoffs of the alternatives:

- **DynamoDB only** would make every rollout item batch and snapshot a DynamoDB item. That runs into item-size limits, makes large histories expensive to write/read, and makes compaction/archive awkward.
- **S3 only** would make S3 both payload store and commit authority. Codex would need to build its own commit protocol: lock objects, manifests, idempotency records, compare-and-swap semantics, partial-write recovery, list/index projection, and reconciliation. That is possible, but it moves correctness into custom code where bugs are more likely.
- **Postgres/Aurora** would give a natural transactional model, but it adds a heavier customer-account operational footprint: database provisioning, sizing, patching, backups, failover, connection management, and idle cost. That is too much weight for the first customer-account remote ThreadStore.

The design does add the complexity of coordinating two AWS services, but that coordination is deliberately one-way:

- S3 write first, then DynamoDB commit.
- DynamoDB commit succeeds: the append is durable and visible.
- DynamoDB commit fails: the S3 object is ignored and later cleaned up.
- DynamoDB says an item exists: the S3 object must be fetched and verified.

For customer-account deployment, DynamoDB + S3 is the smallest AWS-native design that still gives us strong commit/index semantics and cheap durable payload storage.

## Non-Goals

- Reconstruct every local SQLite-backed adjunct feature in v1.
- Store secrets, auth tokens, live handles, process IDs, or machine-local runtime handles.
- Make remote ThreadStore a general remote `state_db`.
- Require a separate remote control plane for the first durable AWS backend.

## Storage Model

### DynamoDB

DynamoDB is the source of truth for thread metadata and commit/index state.

Expected logical records:

- `META`: one row per thread with metadata, sequence counters, archive state, history mode, config snapshot, and projection fields used by list/read.
- `ITEM`: committed item index entries, ordered by per-thread sequence.
- `IDEMPOTENCY`: retry records keyed by thread and idempotency key, returning the committed sequence range for duplicate appends.
- Snapshot/compaction markers when the thread has a compacted prefix.

The current durable AWS implementation is in `codex-rs/thread-store/src/remote/aws_object_log/`.

### S3

S3 stores payload bytes that should not be duplicated directly into DynamoDB rows:

- append batch payloads
- snapshot payloads
- compacted history payloads
- overflow config snapshots if a future snapshot exceeds DynamoDB item limits

Object keys are deterministic under the configured namespace/prefix so retry and reconstruction can verify object identity.

## Durable Config Snapshot

Remote resume cannot depend on the replacement machine's local config defaults. Each thread creation persists a versioned `StoredThreadConfigSnapshot` on the ThreadStore metadata row.

The snapshot is non-secret and includes:

- model and provider
- service tier
- approval and reviewer policy
- permission profile, active permission profile, and profile workspace roots
- cwd and runtime workspace roots
- ephemeral flag
- reasoning effort and summary
- personality
- standalone developer instructions
- collaboration mode
- session source
- history mode
- fork/parent/thread source
- originator

The core in-memory snapshot type is `ThreadConfigSnapshot` in `codex-rs/core/src/codex_thread.rs`. The storage-facing type is `StoredThreadConfigSnapshot` in `codex-rs/thread-store/src/types.rs`. The conversion from session state to stored state happens in `codex-rs/core/src/session/session.rs`.

## Resume Flow

### Running Thread Rejoin

If the thread is already loaded, app-server continues to use the existing running-thread branch. It uses the live thread's current `config_snapshot().await`, validates explicit override mismatches, attaches the caller to the running thread, and returns the existing session.

This path is intentionally separate from cold remote resume.

### Cold Thread Resume

For a non-running thread:

1. `thread_resume_inner` reads the thread by `thread_id` or rollout path.
2. For remote/pathless stores, `read_thread(... include_history=true)` returns committed history and no local rollout path.
3. `stored_thread_to_initial_history` builds `InitialHistory::Resumed`.
4. If `StoredThread.config_snapshot` exists:
   - app-server applies snapshot values to `ConfigOverrides` before config load
   - app-server reloads `Config` using those snapshot defaults plus explicit request overrides
   - app-server restores trusted permission profile projection fields onto the loaded config
   - app-server calls the snapshot-aware `ThreadManager::resume_thread_with_history_from_snapshot`
5. If no snapshot exists, app-server keeps the legacy fallback path using local persisted resume metadata where available.
6. The resumed session emits `SessionConfigured`.
7. App-server assembles `ThreadResumeResponse` from store metadata and live session state.

Remote stores do not have local rollout paths. The response builder therefore treats `rollout_path` as optional and leaves `thread.path = None` when absent.

## Override Semantics

The persisted snapshot is the base for cold remote resume. Explicit request overrides remain higher priority where supported.

Examples:

- explicit `model` replaces the snapshot model
- explicit `model_reasoning_effort` replaces the snapshot effort
- explicit `developerInstructions` replaces stored standalone developer instructions
- explicit `sandbox` or `permissions` prevents restoring the stored permission profile projection

Collaboration mode is restored from the snapshot, then updated with the final model/reasoning values from the loaded config. This preserves the stored mode and collaboration-mode developer instructions while still honoring explicit resume model/reasoning overrides.

## Correctness Properties

### Ordered Append

The store maintains per-thread monotonically increasing item sequence. Reads reconstruct history by sequence order.

### Retry-Safe Append

Append calls use idempotency records. A retry with the same key returns the originally committed sequence range instead of duplicating logical items.

### Read After Commit

`flush_thread` is the commit barrier. After it completes, `read_thread(... include_history=true)` must return all committed items through the flushed sequence.

### Partial Writes

Payloads can land in S3 before DynamoDB commit state advances. Readers only use DynamoDB-committed index/META state, so uncommitted payload objects are ignored and can be cleaned up later.

### Pathless Resume

Remote stores intentionally return no local rollout path. Cold resume must not require one after it has loaded history from ThreadStore.

## Local State Boundary

Remote ThreadStore v1 is not a remote `state_db`.

When the configured store is not `LocalThreadStore`, session init does not receive a SQLite `state_db` handle. Features that currently require local state should either:

- fail with a clear unsupported/capability error for remote-store sessions,
- move durable thread-level state into ThreadStore metadata/history, or
- move query-heavy auxiliary state into a separate remote projection store.

This is acceptable for the minimal v1 claim: durable root-thread history/config replay and continuation by `thread_id`.

## Failure Behavior

### Writer Dies Before Commit

S3 objects may remain orphaned. DynamoDB committed sequence state does not advance, so readers do not observe partial appends. A cleanup process can remove orphaned objects by age/prefix.

### Network Timeout After Commit

The caller retries with the same idempotency key. DynamoDB idempotency state returns the existing committed append result.

### Replacement Instance Starts

External orchestration is assumed to ensure only one Codex instance is active for a thread. The store still preserves committed history and idempotency, but v1 does not rely on an internal lease/fencing protocol for split-brain prevention.

### Missing Config Snapshot

Legacy threads without a stored snapshot continue through the existing fallback path.

### Missing Local Paths

Persisted cwd/workspace roots are restored as path values. If the replacement machine cannot use them, failures should surface through existing config/environment validation. The store does not rewrite these paths to current-machine defaults.

## Tests

Existing and added coverage includes:

- pathless non-local cold resume succeeds and returns `thread.path = None`
- snapshot values become cold resume base values
- explicit resume overrides remain higher priority
- developer instructions are persisted and applied
- collaboration mode is restored while explicit model/reasoning overrides are applied
- active permission profile and profile workspace roots are restored to loaded config
- AWS object-log LocalStack tests cover durable store behavior and config snapshot round trip

Important test locations:

- `codex-rs/app-server/tests/suite/v2/remote_thread_store.rs`
- `codex-rs/app-server/src/request_processors/thread_processor_tests.rs`
- `codex-rs/thread-store/tests/aws_object_log_localstack.rs`

## Implementation Map

- ThreadStore storage API and types:
  - `codex-rs/thread-store/src/types.rs`
- AWS object-log backend:
  - `codex-rs/thread-store/src/remote/aws_object_log/`
- Live thread persistence:
  - `codex-rs/thread-store/src/live_thread.rs`
- Core session snapshot production:
  - `codex-rs/core/src/session/session.rs`
  - `codex-rs/core/src/codex_thread.rs`
- Core resume spawn plumbing:
  - `codex-rs/core/src/thread_manager.rs`
  - `codex-rs/core/src/session/mod.rs`
- App-server cold resume:
  - `codex-rs/app-server/src/request_processors/thread_processor.rs`

## Open Follow-Ups

- Add cleanup/reconciliation for orphaned S3 payloads.
- Decide whether Codex should implement internal lease/fencing or continue relying on external single-active-instance control.
- Define which `state_db`-backed features are unsupported in remote sessions and make their errors explicit.
- Add snapshot overflow support if config snapshots approach DynamoDB item limits.
- Consider remote projection storage for subagent graph/listing state.
