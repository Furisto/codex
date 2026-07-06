# Thin DynamoDB + S3 ThreadStore Spec

## Goal

Implement a Thin DynamoDB + S3 `ThreadStore` backend for durable, shared Codex thread persistence. The backend must allow one Codex process or machine to disappear and another process or machine to resume a thread from committed durable state.

Thin means DynamoDB owns control-plane state, ordering, idempotency, commit metadata, and queryable projections. S3 owns large immutable replay payloads, snapshots, archive payloads, and compaction artifacts.

## Requirements

- Preserve ordered per-thread append semantics.
- Support retry-safe appends with caller-provided idempotency keys.
- Provide read-after-flush/read-after-commit behavior.
- Persist metadata updates and expose metadata projections for read/list/search APIs.
- Keep storage scoped to one customer deployment. Deployment/account isolation comes from AWS account ownership, IAM, table/bucket names, and an optional configured deployment prefix.
- Rely on an external singleton/active-writer mechanism for v1 so only one Codex instance writes a live thread at a time.
- Preserve compatibility with existing rollout history through import/migration and replay reconstruction.
- Reconstruct a thread after process or machine loss using only committed DynamoDB rows and referenced S3 objects.
- Handle large payloads without relying on DynamoDB item storage.
- Support snapshots, archive, and compaction without changing the logical thread history contract.
- Be operationally acceptable inside a customer AWS account with minimal always-on infrastructure.

## Constraints

- Do not require Kubernetes, Kafka, a dedicated control plane, or an always-on database for the customer-account default.
- Do not make S3 listing a normal correctness path for live reads, appends, or metadata listing.
- Do not depend on local rollout files or local SQLite for remote persistence once this backend is selected.
- Keep DynamoDB items small and predictable; large serialized rollout data must go to S3.
- Treat S3 objects as immutable after commit.
- Commit rows in DynamoDB are the source of truth for visible history. S3 objects without commit rows are ignored.
- Do not pass customer or deployment scope on every operation. This backend is for a single-customer server, so deployment scope belongs in store configuration.
- If the single-customer server exposes multiple end users, user identity is thread metadata and an authorization/listing filter, not a required append parameter.
- The first implementation may support bounded metadata search only. Full content search can be deferred behind a separate index.
- The existing local JSONL + SQLite store must keep working.

## Existing ThreadStore Gaps To Close

The current `ThreadStore::append_items` takes only `thread_id` and `items`. Thin v1 requires explicit append identity for idempotent retries. It does not require Codex-owned writer leases in v1 because active-writer exclusivity is handled externally.

Required backend state and API additions:

- store configuration: `deployment_prefix`, table name, bucket name, region, endpoint override, and optional KMS key
- optional create/read/list context: `user_id`, only if this single-customer server still needs per-user authorization or listing
- append input: `idempotency_key`
- optional append input for import/testing/strict CAS paths: `expected_next_seq`
- append result: `first_seq`, `last_seq`, and committed item count

The current lifecycle methods also need clearer remote semantics:

- `create_thread`: create metadata and initial session metadata payload.
- `resume_thread`: reopen backend writer state after the external active-writer mechanism has selected this Codex instance.
- `append_items`: write S3 segment, then conditionally commit in DynamoDB.
- `flush_thread`: return only after all accepted appends are committed and readable.
- `shutdown_thread`: flush, then release local writer state.
- `discard_thread`: release local writer state without deleting committed history.

## Architecture

### AWS Resources

- One S3 bucket for thread payloads.
- One DynamoDB table for thread metadata, idempotency, commits, and snapshot pointers.
- Optional DynamoDB streams later for asynchronous indexing, compaction, or analytics.
- Optional KMS customer-managed key depending on customer security requirements.

### DynamoDB Table

Single-table design:

```text
Table: codex-thread-store
PK: string
SK: string
```

Primary thread partition:

```text
PK = DEPLOYMENT#{deployment_prefix}#THREAD#{thread_id}
SK = META
SK = IDEMP#{idempotency_key}
SK = COMMIT#{seq_padded}
SK = SNAPSHOT#{seq_padded}
SK = TOMBSTONE
```

List indexes:

```text
GSI1PK = DEPLOYMENT#{deployment_prefix}#ARCHIVE#{active|archived}
GSI1SK = RECENCY#{recency_at_ms_padded}#THREAD#{thread_id}

GSI2PK = DEPLOYMENT#{deployment_prefix}#PARENT#{parent_thread_id}
GSI2SK = RECENCY#{recency_at_ms_padded}#THREAD#{thread_id}

GSI3PK = DEPLOYMENT#{deployment_prefix}#CWD#{cwd_hash}
GSI3SK = RECENCY#{recency_at_ms_padded}#THREAD#{thread_id}
```

Optional per-user listing index, only if the server has multiple authenticated users:

```text
GSI_USER_PK = DEPLOYMENT#{deployment_prefix}#USER#{user_id}#ARCHIVE#{active|archived}
GSI_USER_SK = RECENCY#{recency_at_ms_padded}#THREAD#{thread_id}
```

The exact GSI set should start minimal:

- deployment/archive recency listing
- direct parent listing if needed by current app-server APIs
- optional per-user listing if the server has user-scoped history
- add cwd/model/source indexes only if query volume justifies them

### S3 Layout

```text
s3://{bucket}/{deployment_prefix}/thread/{thread_id}/segments/{first_seq}-{last_seq}.jsonl.zst
s3://{bucket}/{deployment_prefix}/thread/{thread_id}/snapshots/{through_seq}.json.zst
s3://{bucket}/{deployment_prefix}/thread/{thread_id}/archive/{archive_id}.json.zst
s3://{bucket}/{deployment_prefix}/thread/{thread_id}/blobs/{sha256}
```

Objects are content-addressed or sequence-addressed and immutable. Lifecycle rules may delete uncommitted temporary objects and transition old archive objects.

## Stored Data Examples

### META Item

```json
{
  "PK": "DEPLOYMENT#customer-prod#THREAD#018f...",
  "SK": "META",
  "deploymentPrefix": "customer-prod",
  "userId": null,
  "threadId": "018f...",
  "sessionId": "018f...",
  "historyMode": "paginated",
  "createdAtMs": 1762300000000,
  "updatedAtMs": 1762300060000,
  "recencyAtMs": 1762300060000,
  "archivedAtMs": null,
  "preview": "Investigate remote storage options",
  "name": null,
  "modelProvider": "openai",
  "model": "gpt-5",
  "cwd": "/workspace/codex",
  "cwdHash": "sha256:...",
  "source": "cli",
  "parentThreadId": null,
  "forkedFromId": null,
  "nextSeq": 43,
  "lastCommittedSeq": 42,
  "deleted": false,
  "GSI1PK": "DEPLOYMENT#customer-prod#ARCHIVE#active",
  "GSI1SK": "RECENCY#1762300060000#THREAD#018f..."
}
```

### IDEMP Item

```json
{
  "PK": "DEPLOYMENT#customer-prod#THREAD#018f...",
  "SK": "IDEMP#turn-9@appends-1",
  "idempotencyKey": "turn-9@appends-1",
  "payloadSha256": "b83c...",
  "firstSeq": 40,
  "lastSeq": 42,
  "itemCount": 3,
  "status": "committed",
  "createdAtMs": 1762300060000
}
```

### COMMIT Item

```json
{
  "PK": "DEPLOYMENT#customer-prod#THREAD#018f...",
  "SK": "COMMIT#000000000040",
  "firstSeq": 40,
  "lastSeq": 42,
  "itemCount": 3,
  "segment": {
    "bucket": "codex-thread-store",
    "key": "customer-prod/thread/018f.../segments/000000000040-000000000042.jsonl.zst",
    "bytes": 58120,
    "sha256": "b83c...",
    "encoding": "jsonl+zstd"
  },
  "idempotencyKey": "turn-9@appends-1",
  "committedAtMs": 1762300060000
}
```

### S3 Segment Contents

Logical contents before compression:

```jsonl
{"seq":40,"type":"user_message","payload":{"text":"Investigate remote storage options"}}
{"seq":41,"type":"response_item","payload":{"item":{"type":"message","content":[]}}}
{"seq":42,"type":"turn_completed","payload":{"usage":{"total_tokens":18422}}}
```

## Append Algorithm

1. Validate that the thread belongs to this configured deployment, and apply optional user authorization if enabled.
2. Require a non-empty idempotency key.
3. Check existing `IDEMP#{idempotency_key}`:
   - If committed with the same payload hash, return the stored append result.
   - If committed with a different payload hash, return conflict.
   - If pending and expired, allow recovery according to stale-pending rules.
4. Read `META.nextSeq` or use caller-provided `expected_next_seq`.
5. Assign `first_seq..last_seq`.
6. Serialize canonical rollout items with assigned sequence numbers.
7. Compress payload and write immutable S3 segment.
8. Use a DynamoDB transaction to:
   - condition `META.nextSeq == first_seq`
   - insert `COMMIT#{first_seq}`
   - insert `IDEMP#{idempotency_key}`
   - update `META.nextSeq`, `lastCommittedSeq`, `updatedAtMs`, `recencyAtMs`, and projection fields if supplied
9. Return committed sequence range.

If the S3 PUT succeeds and the DynamoDB transaction fails, leave the S3 object orphaned. Readers ignore it because no commit row points to it. Lifecycle cleanup can remove it later.

## Read Algorithm

### Load Full Replay History

1. Validate deployment scope, and apply optional user authorization if enabled.
2. Read `META`.
3. Query `COMMIT#` rows in ascending sequence order.
4. Fetch referenced S3 segments in bounded parallelism.
5. Verify segment hash and sequence range.
6. Decode items and return replay history in sequence order.

### Read Thread Metadata

1. Read `META`.
2. Return projected `StoredThread` fields.
3. Include history only by running the full replay path.

### List Threads

1. Query deployment/archive GSI by recency, or optional user/archive GSI when user-scoped listing is enabled.
2. Apply filters supported by available GSIs.
3. For unsupported filters, either reject, bounded-filter after query, or add a dedicated GSI in a later phase.

### Search

V1 supports metadata/title/preview search only if it can be implemented from projected metadata within bounded query cost.

Full content search is deferred. Later options:

- DynamoDB stream to OpenSearch.
- Batch-built search index.
- Hosted-only Postgres/Aurora full-text search.

## Active Writer Model

Thin v1 assumes an external mechanism ensures only one Codex instance writes a live thread at a time. Codex does not acquire or renew storage leases in v1.

The backend still conditions append commits on `META.nextSeq == first_seq`. This is not a distributed-writer lease, but it is a cheap correctness tripwire. If the external singleton guarantee fails, out-of-order or duplicate writers should produce a DynamoDB conditional conflict rather than silently corrupting the committed sequence.

## Later Extension: Leases And Fencing

If Codex must later enforce active-writer exclusivity itself, add a lease/fencing extension without changing the S3 segment format:

- Add `SK=LEASE` item with `holderId`, `epoch`, `expiresAtMs`, and `updatedAtMs`.
- Add backend-owned live writer state after create/resume: `writer_id` and `lease_epoch`.
- Acquire lease by conditionally creating/updating `LEASE` and incrementing `META.leaseEpoch`.
- Renew lease by extending `expiresAtMs` when held by the same writer.
- Allow takeover only after expiry.
- Add every append transaction condition: `LEASE.epoch == lease_epoch` and lease is not expired.
- Release lease best-effort on shutdown; rely on expiry and epoch fencing for correctness.

## Idempotency

Idempotency keys are scoped to deployment/thread.

Rules:

- Same idempotency key plus same payload hash returns the original result.
- Same idempotency key plus different payload hash returns conflict.
- Idempotency records must outlive client retry windows.
- A TTL may remove old idempotency records only after the append can no longer be retried by clients.

## Metadata Projection

Metadata projection should remain store-owned but policy-derived fields should still be computed above the store when possible.

Persist at least:

- preview
- name/title
- model provider
- model
- reasoning effort
- created/updated/recency timestamps
- archived timestamp
- cwd and cwd hash
- source and thread source
- parent/fork relationships
- git info
- approval mode
- permission profile
- token usage
- first user message

Projection updates should be part of the append transaction when derived from appended items. Explicit metadata updates should use conditional updates and preserve field-presence semantics.

## Migration From Local Rollout History

Migration is an import path, not a live fallback path.

1. Enumerate local rollout files using existing local mechanisms.
2. Load rollout items through existing rollout readers.
3. Create remote `META`.
4. Write imported history as S3 segments.
5. Insert DynamoDB commit rows with deterministic sequence ranges.
6. Reconstruct metadata projection from rollout items and existing SQLite metadata if available.
7. Mark imported threads with an import source/version.
8. Validate by reading remote history and comparing item count, first session metadata, and thread id.

## Implementation Steps

### Phase 1: Contract And Types

- Add configured deployment scope to the Thin store.
- If needed, add optional user context to create/read/list operations, not to append.
- Add append idempotency key.
- Add append result type with committed sequence range.
- Define remote error mapping for conflict, unauthorized, missing payload, and retryable storage errors.
- Keep local backend behavior unchanged where possible.

### Phase 2: Storage Crate Skeleton

- Add a new remote Thin backend crate or module outside `codex-core`.
- Define DynamoDB key builders.
- Define S3 key builders.
- Define serialized record structs for `META`, `IDEMP`, `COMMIT`, and `SNAPSHOT`.
- Add config for deployment prefix, table name, bucket name, region, endpoint override, and optional KMS key.

### Phase 3: Create/Resume

- Implement `create_thread` with initial metadata and initial session metadata segment.
- Implement `resume_thread` assuming external active-writer selection has already happened.
- Add tests for create/resume without local rollout or SQLite artifacts.

### Phase 4: Append/Flush/Read

- Implement canonical item serialization and compression.
- Implement S3 segment write.
- Implement DynamoDB conditional transaction commit.
- Implement idempotency replay.
- Implement `flush_thread` as a committed-visibility barrier.
- Implement full replay read from commit rows and S3 segments.
- Add crash/retry tests around S3 success plus DynamoDB failure.

### Phase 5: Metadata/List/Archive/Delete

- Implement metadata projection update.
- Implement explicit metadata patch updates.
- Implement `read_thread`.
- Implement `list_threads` through GSI.
- Implement archive/unarchive by updating `META` and list-index keys.
- Implement delete as tombstone first, then asynchronous/best-effort S3 cleanup.

### Phase 6: Migration And Compatibility

- Implement local rollout import tool or internal migration flow.
- Preserve compatibility with existing rollout item formats.
- Add validation mode that compares local and remote reconstructed history.

### Phase 7: Performance And Operations

- Add bounded parallel S3 GETs for replay.
- Add segment-size tuning.
- Add metrics for append latency, DDB transaction latency, S3 PUT/GET latency, retry counts, conditional sequence conflicts, orphaned segment count, and read reconstruction time.
- Add alarms/runbook guidance for throttling and high conflict rates.

## Test Plan

- Unit tests for key construction and serialization.
- Integration tests with mocked AWS clients or local AWS-compatible test services.
- End-to-end ThreadStore tests for create, resume, append, flush, read, list, archive, delete.
- Idempotency tests for same key/same payload and same key/different payload.
- Conditional sequence conflict tests that simulate external active-writer failure.
- Recovery tests for process loss after S3 PUT before DynamoDB commit.
- Recovery tests for process loss after DynamoDB commit.
- Migration tests from representative rollout JSONL files.
- Authorization tests for optional user-scoped access if enabled.

## Success Criteria

- A thread can be created, appended to, flushed, destroyed with the process, and resumed by a different process using only DynamoDB and S3.
- Append retries are idempotent and never duplicate committed items.
- External active-writer enforcement is documented as a deployment prerequisite for v1.
- If two append attempts race on the same expected sequence, the DynamoDB conditional update rejects one instead of silently corrupting order.
- Reads after successful flush return all committed items in order.
- Metadata listing works without S3 listing or local SQLite.
- Large payloads are stored in S3 and do not exceed DynamoDB item limits.
- S3 orphan objects do not affect reads and can be cleaned up safely.
- Existing local rollout history can be imported and replayed from the Thin backend.
- The local ThreadStore remains compatible and unchanged for existing local deployments.
- Customer-account deployment requires only DynamoDB, S3, IAM, and optional KMS configuration.

## Avoid Initially

- S3-only live append semantics.
- Full content search in v1.
- Cross-region active-active writes.
- Global DynamoDB tables.
- OpenSearch dependency.
- Storing large rollout payloads inline in DynamoDB.
- A heavyweight control plane for compaction or lease ownership.
