# Durable AWS Object-Log ThreadStore Spec

## Objective

Replace the current in-memory `AwsObjectLogThreadStore` prototype with a real durable DynamoDB + S3 implementation.

The finished backend must allow a Codex process to die, lose all local process state, and be replaced by another Codex process on another machine that can resume the thread using only AWS-backed ThreadStore state.

The existing in-memory object-log behavior is not an acceptable production implementation. It may be retained only as a test fake or removed entirely.

## Current Problem

`codex-rs/thread-store/src/remote/aws_object_log.rs` currently models the intended object-log data structures in memory:

- Thread heads are stored in `HashMap<ThreadId, AwsObjectLogThreadHead>`.
- Commit pointers are stored in `HashMap<ThreadId, BTreeMap<u64, AwsObjectLogCommitPointer>>`.
- Idempotency records are stored in `HashMap<AwsObjectLogIdempotencyScope, AwsObjectLogIdempotencyRecord>`.
- Payload objects are stored in `HashMap<String, AwsObjectLogCommitPayload>`.
- All state is protected by one `tokio::sync::Mutex`.

That implementation validates some protocol semantics, but it is not durable and cannot satisfy remote resume after process or machine loss. The implementation work must replace those maps with real AWS calls.

## Requirements

### Functional Requirements

1. Persist thread creation durably in DynamoDB.
2. Persist each append batch durably as:
   - an immutable S3 payload object containing serialized rollout items
   - a DynamoDB commit pointer that makes the S3 object part of committed history
   - a DynamoDB idempotency record for retry-safe replay
   - a DynamoDB head update advancing the committed sequence
3. Persist `StoredThreadConfigSnapshot` on the DynamoDB `HEAD` item during thread creation.
4. Read `StoredThread.config_snapshot` from DynamoDB `HEAD` during `read_thread`.
5. Load committed history by reading DynamoDB commit pointers and then fetching referenced S3 payloads.
6. Support `read_thread(include_history=true)` and `load_history` without using local rollout files.
7. Support `list_threads` and `search_threads` from DynamoDB metadata projections.
8. Support archive, unarchive, delete, and metadata updates through DynamoDB.
9. Preserve local ThreadStore compatibility for existing local JSONL + SQLite history.
10. Support import from existing rollout history into DynamoDB + S3.
11. Keep customer-account deployment lightweight: one DynamoDB table, one S3 bucket, IAM policy, optional KMS keys, optional lifecycle policy.

### Correctness Requirements

1. DynamoDB is the source of truth for committed sequence order.
2. S3 objects are never considered committed until referenced by a committed DynamoDB item.
3. Readers must never discover history by listing S3.
4. Append retry with the same idempotency key and same request hash returns the original commit result.
5. Append retry with the same idempotency key and different request hash fails with an idempotency conflict.
6. If the network times out after DynamoDB commit but before response, retry must recover by reading the idempotency record.
7. If S3 PUT succeeds but DynamoDB commit fails, the orphan S3 object must be ignored and later cleaned up.
8. If the writer dies mid-append before DynamoDB commit, replacement resume must observe only previously committed history.
9. If another writer concurrently appends despite the external single-active guarantee, DynamoDB sequence conditions must fail closed.
10. Metadata projection must not advance beyond committed history for append-derived updates.

### Non-Requirements For The First Durable Version

1. Codex-managed leases or fencing. Single-active ownership is handled externally.
2. Multi-region active/active writes.
3. Full-text search over all rollout payload content.
4. A hosted ThreadStore control plane.
5. Per-method `tenant_id`, `user_id`, `writer_id`, or lease-token parameters. Namespace and authorization scope are configured when constructing the store.

## Constraints

1. Do not keep the production `AwsObjectLogThreadStore` backed by `HashMap` state.
2. Do not call this backend durable until it uses real DynamoDB and S3 clients.
3. Keep `codex-thread-store` independent from `codex-core`.
4. Store config snapshot DTOs in `codex-thread-store` using storage-neutral types.
5. Use explicit serialization formats and schema versions for every DynamoDB and S3 record.
6. Do not store secrets, auth tokens, live handles, process ids, or machine-local runtime handles.
7. Large rollout payloads must go to S3, not DynamoDB.
8. DynamoDB item size must stay below 400 KB.
9. DynamoDB transaction size must stay within AWS transaction limits.
10. S3 object keys must be deterministic enough to support safe retry cleanup and debugging.
11. The implementation must compile and test without requiring real AWS credentials for unit tests.
12. Integration tests must be able to run against AWS-compatible local endpoints or a real test AWS account when configured.

## Architecture

### Module Layout

Create a real remote implementation under `codex-rs/thread-store/src/remote/aws_object_log/`:

```text
remote/aws_object_log/
  mod.rs
  config.rs
  dynamodb.rs
  s3.rs
  records.rs
  serialization.rs
  append.rs
  read.rs
  metadata.rs
  import.rs
  tests.rs
```

The public exports remain:

- `AwsObjectLogThreadStore`
- `AwsObjectLogThreadStoreConfig`
- `AwsObjectLogAppendOptions`
- `AwsObjectLogAppendResult`

The current single-file in-memory implementation should be replaced. If a fake remains useful, move it under tests and name it explicitly as a fake, for example `FakeAwsObjectLogBackend`.

### Dependencies

Add AWS SDK dependencies to `codex-thread-store`:

- `aws-config`
- `aws-sdk-dynamodb`
- `aws-sdk-s3`
- `aws-smithy-types` if needed for byte streams and retries
- compression crate only if existing workspace dependencies do not already provide one
- hashing crate if existing workspace dependencies do not already provide SHA-256

After dependency changes:

1. Update `codex-rs/Cargo.lock`.
2. Run `just bazel-lock-update` from the repo root if Bazel is available.
3. If Bazel is unavailable locally, document that `MODULE.bazel.lock` could not be refreshed and must be updated by CI or a Bazel-capable environment.

### Configuration

`AwsObjectLogThreadStoreConfig` should contain:

```rust
pub struct AwsObjectLogThreadStoreConfig {
    pub table_name: String,
    pub bucket_name: String,
    pub namespace: String,
    pub key_prefix: String,
    pub aws_region: Option<String>,
    pub endpoint_url: Option<String>,
    pub kms_key_id: Option<String>,
    pub consistent_reads: bool,
    pub append_payload_compression: AwsObjectLogCompression,
}
```

Configuration is supplied at store construction, not on every ThreadStore method call.

Assumption for the first customer-account deployment: `namespace` is a deployment-local logical namespace, not a multi-tenant SaaS tenant id. It protects key layout and IAM scoping without making app-server multi-tenant.

### AWS Client Construction

`AwsObjectLogThreadStore::new` should remain cheap and deterministic where possible.

Recommended constructors:

```rust
impl AwsObjectLogThreadStore {
    pub async fn from_config(config: AwsObjectLogThreadStoreConfig) -> ThreadStoreResult<Self>;

    pub fn from_clients(
        config: AwsObjectLogThreadStoreConfig,
        dynamodb: aws_sdk_dynamodb::Client,
        s3: aws_sdk_s3::Client,
    ) -> Self;
}
```

Use `from_clients` in tests so clients can point at LocalStack. Real AWS account validation can be added later as an optional manual test path.

## Data Model

### DynamoDB Table

Use a single-table design.

Required key attributes:

```text
pk: string
sk: string
```

Thread partition:

```text
pk = NS#{namespace}#THREAD#{thread_id}
sk = HEAD
sk = COMMIT#{start_seq_020}
sk = IDEMP#APPEND#{idempotency_key}
sk = SNAPSHOT#{snapshot_seq_020}
sk = IMPORT#{import_id}
```

Use GSI projection attributes on the `HEAD` item for v1. This keeps authoritative thread metadata and list/search projection fields in one DynamoDB item, avoiding duplicate projection rows and projection drift.

```text
gsi1pk = NS#{namespace}#STATE#{active|archived}
gsi1sk = UPDATED#{updated_at_epoch_ms}#THREAD#{thread_id}

gsi2pk = NS#{namespace}#STATE#{active|archived}
gsi2sk = CREATED#{created_at_epoch_ms}#THREAD#{thread_id}
```

List/search consistency:

- `read_thread`, `load_history`, and append correctness use strongly consistent base-table reads.
- `list_threads` and metadata-only `search_threads` query GSIs and are eventually consistent.
- Tests should not require read-after-write list consistency unless they poll or read by thread id first.

Do not create separate projection items in v1. Revisit separate projection items only if GSI query patterns cannot satisfy pagination/filtering needs.

Base thread partition reads must use strongly consistent `GetItem`/`Query` where correctness depends on committed history.

### DynamoDB HEAD Item

The `HEAD` item stores:

- schema version
- namespace
- thread id
- session id
- `head_seq`
- `history_mode`
- `extra_config`
- `config_snapshot`
- fork/parent/source fields
- metadata projection fields currently represented by `ThreadMetadataSnapshot`
- archive/delete markers
- created/updated/recency timestamps
- optional latest snapshot pointer

### DynamoDB COMMIT Item

Each commit item stores:

- schema version
- namespace
- thread id
- commit id
- start sequence
- end sequence
- item count
- S3 bucket
- S3 key
- payload byte length
- payload SHA-256
- created timestamp
- optional compression

### DynamoDB IDEMP Item

Each append idempotency item stores:

- schema version
- namespace
- thread id
- operation name
- idempotency key
- request SHA-256
- payload SHA-256
- commit id
- start sequence
- end sequence
- item count
- created timestamp
- optional expiration timestamp

### S3 Payload Object

Commit payload object:

```json
{
  "schema": "codex.thread.commit.v1",
  "namespace": "customer-prod",
  "thread_id": "00000000-0000-4000-8000-000000000001",
  "commit_id": "01JZ...",
  "start_seq": 42,
  "end_seq": 45,
  "items": [
    {
      "seq": 42,
      "rollout_item": {}
    }
  ]
}
```

S3 object metadata:

- schema
- namespace
- thread id
- commit id
- start sequence
- end sequence
- SHA-256

S3 key:

```text
{key_prefix}/namespaces/{namespace}/threads/{thread_id}/commits/{start_seq_020}-{end_seq_020}-{commit_id}.json.zst
```

## Append Algorithm

Inputs:

- `AppendThreadItemsParams`
- generated or supplied idempotency key
- optional expected next sequence

Steps:

1. Canonicalize appended rollout items using existing `persisted_rollout_items`.
2. If canonical item list is empty, return an empty append result without AWS writes.
3. Serialize the commit payload with provisional sequence numbers.
4. Compute request hash over canonical items and append options.
5. Strongly read idempotency item by `IDEMP#APPEND#{idempotency_key}`.
6. If idempotency item exists:
   - compare request hash
   - return recorded result if it matches
   - fail with idempotency conflict if it differs
7. Strongly read `HEAD` item to get `head_seq`.
8. Compute `start_seq = head_seq + 1` and `end_seq = head_seq + item_count`.
9. Build deterministic commit id and S3 key from thread id, idempotency key, start seq, end seq, and request hash.
10. Put the S3 payload object.
11. Execute DynamoDB `TransactWriteItems`:
   - condition `HEAD.head_seq == previous_head_seq`
   - put `COMMIT#{start_seq}` with `attribute_not_exists`
   - put `IDEMP#APPEND#{idempotency_key}` with `attribute_not_exists`
   - update `HEAD.head_seq = end_seq`
   - update `HEAD.updated_at` and append-related metadata fields if included in the same operation
12. If transaction succeeds, return append result.
13. If transaction fails because idempotency now exists, strongly read idempotency item and replay/conflict.
14. If transaction fails because `HEAD.head_seq` changed unexpectedly, return a split-brain/concurrent-write error.
15. If response times out, retry from step 5. The idempotency item determines whether the commit completed.

Important: S3 PUT happens before DynamoDB commit. Orphan S3 objects are safe because readers only follow committed DynamoDB pointers.

## Idempotency Key Strategy

Add an idempotency field to append inputs. The field may be optional at the Rust type level to avoid forcing local stores to invent keys, but the AWS object-log store must reject non-empty appends that do not include one.

```rust
pub struct AppendThreadItemsParams {
    pub thread_id: ThreadId,
    pub items: Vec<RolloutItem>,
    pub idempotency_key: Option<String>,
    pub expected_next_seq: Option<u64>,
}
```

Local stores can ignore `idempotency_key` and `expected_next_seq` or use them for debug checks. The AWS object-log store must require `idempotency_key` and must use `expected_next_seq` when present.

`LiveThread` should generate stable append idempotency keys for each persisted append attempt. Suggested shape:

```text
thread:{thread_id}:session:{session_id}:window:{window_id}:append:{ordinal}
```

The key must be reused for retries of the same append call. It does not need to be reused after process death unless the same unacknowledged append can be reconstructed.

Do not ship the durable AWS implementation with only single-call retry idempotency. The append idempotency key is part of the durability contract.

## Read And Resume Algorithms

### `read_thread(include_history=false)`

1. Strongly read `HEAD`.
2. Return metadata, config snapshot, source/fork fields, archive/delete status.
3. Do not read S3 payloads.

### `read_thread(include_history=true)`

1. Strongly read `HEAD`.
2. Load latest snapshot pointer if present.
3. Query `COMMIT#` items from snapshot sequence + 1 through `HEAD.head_seq`.
4. Fetch referenced S3 payloads.
5. Verify SHA-256 and sequence ranges.
6. Concatenate rollout items in sequence order.
7. Return `StoredThread` with `StoredThreadHistory`.

### `load_history`

Use the same history loading path as `read_thread(include_history=true)`, but return only replayable history.

### Cold Resume

Cold app-server resume should:

1. Call `ThreadStore.read_thread(... include_history=true)`.
2. Use `StoredThread.config_snapshot` as the base config if present.
3. Replay committed history from DynamoDB/S3.
4. Use local state DB only for legacy threads without config snapshots.

This was already partially wired in app-server; verify it against the real AWS-backed store.

## Metadata Projection

`update_thread_metadata` writes to DynamoDB, not S3.

Rules:

1. Strongly read or conditionally update the `HEAD` item.
2. Apply `ThreadMetadataPatch` to `HEAD` metadata fields.
3. Update list/search GSI projection attributes on the `HEAD` item in the same DynamoDB update.
4. Since v1 uses GSI attributes on `HEAD`, there are no separate projection rows to drift from canonical metadata. GSI query results remain eventually consistent.
5. Metadata fields derived from append observation should include or respect a committed sequence watermark.

Search v1:

- title/name
- preview
- first user message
- model/provider filters
- cwd filter
- archived filter
- relation filters that can be expressed from projected fields

Search v1 does not search S3 payload content.

## Archive, Unarchive, Delete

### Archive

1. Transact update `HEAD.archived_at`.
2. Update active/archived GSI projection attributes on `HEAD`.
3. Do not move S3 objects.

### Unarchive

1. Transact clear `HEAD.archived_at`.
2. Update active/archived GSI projection attributes on `HEAD`.
3. Do not move S3 objects.

### Delete

First version should use soft delete:

1. Set `HEAD.deleted_at`.
2. Remove or hide list/search projections.
3. Leave S3 payloads for retention/lifecycle cleanup.

Hard delete can be added later with explicit S3 batch delete and DynamoDB item deletion.

## Snapshots And Compaction

Snapshots and compaction are part of the first durable implementation. They do not need to be optimized in the first patch, but the backend should not be considered complete until long-thread replay can use a snapshot plus tail commits instead of unbounded commit payload reads.

Snapshot item:

- `SNAPSHOT#{seq}`
- S3 bucket/key
- sequence covered
- byte length
- SHA-256
- created timestamp
- schema version

Snapshot S3 object:

- compact replay state up to sequence N
- enough rollout/history state for `load_history` to continue from sequence N + 1

Compaction strategy:

1. Commit append batches normally.
2. Background or explicit compaction builds a snapshot at safe sequence N.
3. Update `HEAD.latest_snapshot_seq` and snapshot pointer conditionally.
4. Keep old commits until retention window expires.
5. Readers use latest snapshot plus tail commits.

No reader may require listing S3.

## AWS Resource Plan

### DynamoDB

Provision:

- table name from config
- partition key `pk`
- sort key `sk`
- on-demand billing for first customer-account deployment
- optional PITR
- optional TTL for idempotency records and orphan markers
- required GSIs for list projections

### S3

Provision:

- bucket name from config
- bucket versioning optional
- SSE-S3 or SSE-KMS based on config
- lifecycle rule for orphan staging objects and deleted thread payloads
- block public access

### IAM

Minimum permissions:

- `dynamodb:GetItem`
- `dynamodb:PutItem`
- `dynamodb:UpdateItem`
- `dynamodb:DeleteItem` if hard delete is implemented
- `dynamodb:Query`
- `dynamodb:TransactWriteItems`
- `s3:GetObject`
- `s3:PutObject`
- `s3:DeleteObject` if hard delete or cleanup is implemented
- `kms:Encrypt`, `kms:Decrypt`, `kms:GenerateDataKey` when SSE-KMS is used

IAM should restrict resources to the configured table and bucket/prefix.

## Implementation Steps

### Phase 1: Replace Prototype With Real Backend Boundary

1. Split `aws_object_log.rs` into a module directory.
2. Move record structs into `records.rs`.
3. Introduce `AwsObjectLogBackend` internal trait only if needed for tests.
4. Ensure production `AwsObjectLogThreadStore` owns AWS DynamoDB and S3 clients.
5. Rename any remaining in-memory object-log implementation to a test fake and keep it out of production constructors.

Success criteria:

- No production `AwsObjectLogThreadStore` state is backed by `HashMap` commit/head/payload maps.
- Production constructor requires AWS clients or AWS config.
- Unit tests can still exercise serialization and algorithmic helpers.

### Phase 2: Add AWS Record Serialization

1. Define DynamoDB item encoders/decoders for `HEAD`, `COMMIT`, `IDEMP`, `SNAPSHOT`, and projection records.
2. Define S3 payload envelope structs.
3. Add schema version fields.
4. Add SHA-256 hashing for payload verification.
5. Add optional compression.

Success criteria:

- Round-trip tests cover each DynamoDB record type.
- S3 payload round-trip tests verify item sequence order and hash validation.
- Decoder rejects unsupported schema versions.

### Phase 3: Implement Durable Create/Read

1. Implement `create_thread` as conditional DynamoDB `PutItem` for `HEAD`.
2. Store `StoredThreadConfigSnapshot` on `HEAD`.
3. Implement `read_thread(include_history=false)` from `HEAD`.
4. Implement `list_threads` from `HEAD` GSI attributes.
5. Preserve local legacy behavior in `LocalThreadStore`.

Success criteria:

- Creating a thread persists a `HEAD` item in DynamoDB.
- Reading after create returns metadata and config snapshot from DynamoDB.
- Duplicate create fails closed.
- Tests prove process-local memory is not required after create.

### Phase 4: Implement Durable Append

1. Extend append parameters for idempotency and expected sequence.
2. Implement S3 payload PUT.
3. Implement DynamoDB transaction for commit pointer, idempotency record, and head sequence update.
4. Implement timeout/retry recovery through strongly consistent idempotency reads.
5. Implement split-brain detection through `HEAD.head_seq` condition checks.

Success criteria:

- Append writes S3 payload and DynamoDB commit pointer.
- Retry with same idempotency key returns same sequence range.
- Retry with changed payload fails.
- Network-timeout simulation after transaction recovers by reading idempotency record.
- S3 orphan objects are ignored by reads.

### Phase 5: Implement Durable History Replay

1. Query DynamoDB commit pointers by thread partition.
2. Fetch S3 payloads concurrently with bounded concurrency.
3. Verify hashes and sequence ranges.
4. Return ordered `StoredThreadHistory`.
5. Wire `read_thread(include_history=true)` and `load_history` to this path.

Success criteria:

- A new store instance with empty memory can reconstruct a committed thread from DynamoDB/S3.
- Missing S3 object returns a clear storage corruption error.
- Hash mismatch returns a clear storage corruption error.
- Commit gaps return a clear storage corruption error.

### Phase 6: Implement Metadata, Search, Archive, Delete

1. Implement `update_thread_metadata` as DynamoDB updates.
2. Keep list/search projection fields in sync.
3. Implement archive/unarchive with projection updates.
4. Implement soft delete.
5. Do not implement hard delete in v1.

Success criteria:

- List/read/search reflect committed metadata updates.
- Archive hides threads from active list and shows them in archived list.
- Unarchive reverses archive.
- Delete hides thread without requiring immediate S3 deletion.

### Phase 7: Implement Import

1. Read existing local rollout history.
2. Chunk into commit payloads.
3. Write chunks to S3.
4. Commit pointers to DynamoDB with deterministic import idempotency.
5. Create or update `HEAD` with imported metadata and config snapshot when available.

Success criteria:

- Imported local history can be replayed from AWS only.
- Re-running the same import is idempotent.
- Partial import can resume or fail cleanly without corrupting committed history.

### Phase 8: Add Snapshot/Compaction

1. Add snapshot writer for long histories.
2. Add latest snapshot pointer on `HEAD`.
3. Teach history replay to use snapshot plus tail commits.
4. Add cleanup policy for old commit payloads after retention window.

Success criteria:

- Long thread replay uses bounded S3 GETs after snapshot.
- Snapshot hash and sequence validation are enforced.
- Tail commits after snapshot replay correctly.

### Phase 9: Wire Configuration

1. Add app-server/core config fields for remote AWS ThreadStore table, bucket, namespace, region, endpoint, and KMS settings.
2. Ensure config schema updates are generated if `ConfigToml` changes.
3. Preserve existing local default.
4. Make remote store opt-in.

Success criteria:

- A user can select AWS object-log ThreadStore through config.
- Missing required table/bucket config fails at startup with a clear error.
- Local ThreadStore behavior is unchanged by default.

## Test Plan

### Unit Tests

- Record serialization/deserialization.
- Unsupported schema versions.
- Payload hash computation and verification.
- Idempotency conflict detection.
- Commit sequence gap detection.
- Config snapshot encode/decode.

### Fake Backend Tests

Use a test-only fake backend if it helps exercise algorithm branches. The fake must not be exported or wired as the production store.

Test cases:

- S3 PUT succeeds, DynamoDB transaction fails.
- DynamoDB transaction succeeds, response timeout occurs.
- Idempotency record exists before retry.
- Concurrent append changes `HEAD.head_seq`.
- Missing S3 payload.
- Hash mismatch.

### AWS-Compatible Integration Tests

Run against LocalStack by default. Real AWS account tests can be added later as optional/manual validation.

Required local setup:

- LocalStack for DynamoDB + S3.

Tests:

1. `create_thread` persists `HEAD`.
2. `create_thread` persists config snapshot.
3. `read_thread` can run from a fresh store instance.
4. `append_items` persists S3 payload and DynamoDB commit pointer.
5. `load_history` reconstructs ordered rollout items from a fresh store instance.
6. Same idempotency key replays original commit.
7. Same idempotency key with different payload conflicts.
8. Unexpected `HEAD.head_seq` fails closed.
9. Orphan S3 object is ignored.
10. Metadata update changes list/search projection.
11. Archive/unarchive/delete projection behavior.
12. Import from local rollout history.

### App-Server Tests

- Cold `thread/resume` reads from ThreadStore with `include_history=true`.
- Remote stored config snapshot is used as resumed config base.
- Local state DB fallback is used only for legacy threads without snapshot.
- Running-thread rejoin behavior remains unchanged.

## Success Criteria

The feature is complete when:

1. Production `AwsObjectLogThreadStore` uses AWS DynamoDB and S3 clients.
2. A process can create a thread, append history, exit, and a new process can read and resume from AWS only.
3. The current in-memory object-log maps are not part of production remote storage.
4. Idempotent append retry is proven by tests.
5. Split-brain accidental concurrent appends fail closed by DynamoDB condition checks.
6. Config snapshot persists on `HEAD` and is consumed by cold resume.
7. List/read/search/archive/delete work from DynamoDB projections.
8. Local ThreadStore behavior remains unchanged.
9. Customer-account deployment requires only DynamoDB, S3, IAM, optional KMS, and optional lifecycle rules.
10. Scoped tests pass:
    - `just fmt`
    - `just test -p codex-thread-store`
    - `just test -p codex-app-server`
    - `just test -p codex-core` if core config/session code changes
11. Dependency changes include updated Cargo and Bazel lockfiles where possible.

## Decisions

1. The first durable version requires an append idempotency key for every non-empty AWS append.
2. CI/local integration tests use LocalStack for DynamoDB + S3.
3. v1 uses soft delete plus S3 lifecycle policy, not hard delete.
4. Snapshots/compaction are included in the first durable implementation, not deferred indefinitely after append/read.
5. v1 uses GSI attributes on the `HEAD` item for list projections. Separate projection items are deferred unless GSI-based pagination/filtering proves insufficient.

## Implementation Guidance

Prioritize the smallest real durable sequence:

1. Real AWS create/read.
2. Real AWS append/idempotency.
3. Real AWS load history from a fresh store instance.
4. Metadata/list/archive.
5. Import.
6. Snapshot/compaction before marking the durable backend complete.

Do not spend more time extending the in-memory prototype except as a test fake. Every production path should move toward actual DynamoDB and S3 persistence.
