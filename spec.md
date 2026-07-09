# Backend-Neutral Durable Goal Store Spec

## Objective

Make thread goals durable through a backend-neutral storage contract instead of requiring a local rollout path and local SQLite state DB.

The immediate bug this fixes is that AWS Object Log threads are persistent and resumable through `ThreadStore`, but `thread/goal/get`, `thread/goal/set`, `thread/goal/clear`, and resume goal snapshot emission still assume every persisted thread has a local `rollout_path()` and local `StateDbHandle`.

The finished design must let a non-local ThreadStore backend, specifically AWS Object Log, support durable goals without fabricating a local rollout JSONL path.

## Current Problem

Current app-server goal handling is local-storage coupled:

- `ThreadGoalRequestProcessor::state_db_for_materialized_thread` rejects running threads with `thread.rollout_path().is_none()`.
- `thread_goal_set_inner` and `thread_goal_clear_inner` call `reconcile_thread_goal_rollout`, which finds and scans a local rollout JSONL file before writing goal state.
- `GoalService` takes `&codex_state::StateRuntime` and writes through `state_db.thread_goals()`.
- `ThreadListenerCommand::EmitThreadGoalSnapshot` carries a `StateDbHandle`, so ordered resume snapshots are also SQLite-specific.
- AWS Object Log stores rollout items remotely, but no local rollout JSONL file exists for resumed AWS threads.

This means AWS-backed threads can be durable from the ThreadStore perspective while goal APIs still fail as if the thread were ephemeral or pathless.

## Requirements

### Functional Requirements

1. Keep the existing app-server goal RPC surface:
   - `thread/goal/get`
   - `thread/goal/set`
   - `thread/goal/clear`
   - `threadGoalUpdated` notifications
   - `threadGoalCleared` notifications
2. Introduce a backend-neutral durable goal storage contract.
3. Preserve the current local behavior by implementing that contract for the existing SQLite-backed `codex_state::GoalStore`.
4. Implement the same contract for AWS Object Log backed threads.
5. Stop requiring `rollout_path()` for goal get/set/clear/snapshot on stores that support durable goals.
6. Preserve current goal semantics:
   - one active goal row per thread
   - goal replacement creates a new `goal_id`
   - expected-goal-id compare-and-update behavior
   - status transitions for active, paused, blocked, usage-limited, budget-limited, complete
   - token and time accounting
   - budget-limit promotion when usage reaches budget
7. Preserve live runtime ordering:
   - external goal mutations must not race idle goal continuation logic
   - goal update and clear notifications must stay ordered with running-thread resume responses
8. Preserve local rollout metadata compatibility:
   - local goal updates should still append `EventMsg::ThreadGoalUpdated` where needed so local thread list/search previews continue to work
   - remote implementations should not need a local rollout append for goal correctness
9. Avoid slow failing startup probes:
   - if a backend does not support durable goals, return an explicit unsupported/capability result quickly
   - TUI resume should not block first-frame rendering on an expected unsupported goal probe
10. Do not create a remote `rollout_path` abstraction.

### Correctness Requirements

1. For a given thread, goal writes must be durable before the RPC response is sent.
2. A read after a successful set/clear must observe the new state when reading by thread id from the same backend.
3. Concurrent updates with stale `expected_goal_id` must fail or return no update, matching existing SQLite semantics.
4. Accounting updates must be atomic with respect to goal status and usage counters.
5. A failed goal write must not emit goal update/clear notifications.
6. A live thread whose goal is modified externally must update its in-memory runtime effects after durable write success.
7. Local goal reconciliation from historical rollout JSONL remains available for local threads, but remote goal reads must not scan or materialize local rollout files.
8. AWS goal state must survive app-server process loss and machine loss.

### Non-Requirements

1. Do not migrate every historical local rollout's goal state eagerly.
2. Do not make AWS Object Log produce local rollout JSONL files.
3. Do not change the public app-server v2 goal API unless a later compatibility review explicitly approves it.
4. Do not require a new AWS table in the first implementation if the existing `codex-threads` table can store goal items safely.
5. Do not make goals available on stores that cannot provide durable compare-and-update semantics.

## Constraints

1. Keep app-server logic storage-neutral. It may depend on a goal-store trait, not on `StateDbHandle` or local rollout paths for the common path.
2. Keep local SQLite as the compatibility implementation.
3. Do not add a trait with `async fn` unless it uses an explicit object-safe boxed future shape. Follow the repository guidance against `async_trait`.
4. Keep new public trait API documented.
5. Prefer storing the trait and shared goal model in `codex-state`, because:
   - `codex-state` already owns `ThreadGoal`, `GoalUpdate`, and accounting semantics
   - `codex-goal-extension` already depends on `codex-state`
   - `codex-thread-store` already depends on `codex-state`
6. Do not make `codex-state` depend on `codex-thread-store` or app-server.
7. AWS goal writes must use conditional writes or transactions, not read-modify-write without compare conditions.
8. AWS goal records must use explicit schema versions.
9. Tests must not require real AWS credentials for routine local runs.

## Architecture

### High-Level Shape

```text
app-server goal RPCs
  -> GoalService
      -> dyn ThreadGoalStore
          local: SQLite GoalStore
          aws: DynamoDB-backed goal store

ThreadStore
  remains responsible for thread history, metadata, list/read/resume/fork

rollout JSONL
  remains local ThreadStore durable history format
  is not the universal goal storage mechanism
```

The key architectural change is that goal operations receive a `ThreadGoalStore` handle selected for the target thread, not a `StateDbHandle`.

### New Storage Trait

Add an object-safe trait in `codex-state`, likely next to `runtime/goals.rs`.

Suggested shape:

```rust
pub type ThreadGoalStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send + 'a>>;

/// Durable storage for one thread's goal state.
///
/// Implementations must provide atomic per-thread updates. `expected_goal_id`
/// guards must compare against the currently stored goal id and return `None`
/// when the guard does not match.
pub trait ThreadGoalStore: Send + Sync {
    fn get_thread_goal(&self, thread_id: ThreadId)
        -> ThreadGoalStoreFuture<'_, Option<ThreadGoal>>;

    fn replace_thread_goal(
        &self,
        thread_id: ThreadId,
        objective: String,
        status: ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> ThreadGoalStoreFuture<'_, ThreadGoal>;

    fn insert_thread_goal(
        &self,
        thread_id: ThreadId,
        objective: String,
        status: ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> ThreadGoalStoreFuture<'_, Option<ThreadGoal>>;

    fn update_thread_goal(
        &self,
        thread_id: ThreadId,
        update: GoalUpdate,
    ) -> ThreadGoalStoreFuture<'_, Option<ThreadGoal>>;

    fn delete_thread_goal(&self, thread_id: ThreadId)
        -> ThreadGoalStoreFuture<'_, Option<ThreadGoal>>;

    fn account_thread_goal_usage(
        &self,
        thread_id: ThreadId,
        time_delta_seconds: i64,
        token_delta: i64,
        mode: GoalAccountingMode,
        expected_goal_id: Option<String>,
    ) -> ThreadGoalStoreFuture<'_, GoalAccountingOutcome>;

    fn pause_active_thread_goal(&self, thread_id: ThreadId)
        -> ThreadGoalStoreFuture<'_, Option<ThreadGoal>>;

    fn usage_limit_active_thread_goal(&self, thread_id: ThreadId)
        -> ThreadGoalStoreFuture<'_, Option<ThreadGoal>>;
}
```

Implementation notes:

- The exact method signatures may use owned `String` values to avoid borrowed data crossing boxed futures.
- `GoalUpdate.expected_goal_id` should become owned if needed for object-safe async boundaries.
- Existing `codex_state::GoalStore` should implement this trait and keep its SQL behavior unchanged.
- Existing direct methods on `GoalStore` can remain as convenience methods if they delegate through shared private helpers.

### GoalService Changes

Change `codex-goal-extension::GoalService` from:

```rust
fn get_thread_goal(&self, state_db: &StateRuntime, thread_id: ThreadId)
fn set_thread_goal(&self, state_db: &StateRuntime, request: GoalSetRequest)
fn clear_thread_goal(&self, state_db: &StateRuntime, thread_id: ThreadId)
```

to:

```rust
fn get_thread_goal(&self, goal_store: &dyn ThreadGoalStore, thread_id: ThreadId)
fn set_thread_goal(&self, goal_store: &dyn ThreadGoalStore, request: GoalSetRequest)
fn clear_thread_goal(&self, goal_store: &dyn ThreadGoalStore, thread_id: ThreadId)
```

The service should retain business rules:

- objective validation
- token budget validation
- runtime mutation permit handling
- previous-goal snapshots
- runtime side effects after successful durable writes

The service should stop directly calling `state_db.thread_goals()`.

### Selecting The Goal Store For A Thread

Introduce a small resolver in app-server, not scattered call-site logic.

Suggested type:

```rust
struct ThreadGoalStoreResolver {
    local_goal_store: Option<Arc<dyn ThreadGoalStore>>,
    thread_store: Arc<dyn ThreadStore>,
}
```

Responsibilities:

1. Given a `ThreadId`, decide whether goals are supported.
2. Return `Arc<dyn ThreadGoalStore>` for supported threads.
3. Return a typed unsupported result quickly for unsupported threads.
4. Avoid local rollout path lookup for stores that can answer via backend capabilities.

Resolution policy:

- For local ThreadStore:
  - return the SQLite-backed local `GoalStore`
  - only perform legacy rollout reconciliation when the target thread is local and a set/clear operation needs compatibility repair
- For AWS Object Log ThreadStore:
  - return the AWS-backed goal store
  - do not call `find_thread_path_by_id_str`
  - do not call `rollout_path()`
- For in-memory or other stores:
  - if they implement goal storage, return it
  - otherwise return `Unsupported { operation: "thread/goal/*" }`

### Where The Backend Capability Lives

Prefer a separate trait over adding goal methods directly to the main `ThreadStore` trait.

Recommended:

```rust
pub trait ThreadStoreGoalExt: Send + Sync {
    fn thread_goal_store(&self) -> Option<Arc<dyn ThreadGoalStore>>;
}
```

Possible implementation options:

1. Add an optional method to `ThreadStore`:

```rust
fn goal_store(&self) -> Option<Arc<dyn ThreadGoalStore>> {
    None
}
```

2. Use `as_any()` downcast in app-server resolver for known store types.

The better long-term option is method 1 because it is explicit and avoids downcast sprawl. This does expand the `ThreadStore` trait, but only with a default method and no mandatory implementation change for unsupported stores.

### App-Server Goal Processor Changes

Replace:

- `state_db_for_materialized_thread`
- `reconcile_thread_goal_rollout` as a universal precondition
- `ThreadListenerCommand::EmitThreadGoalSnapshot { state_db }`

with:

- `goal_store_for_thread(thread_id) -> Result<Arc<dyn ThreadGoalStore>, JSONRPCErrorError>`
- `reconcile_local_goal_rollout_if_needed(thread_id, goal_store)` only for local SQLite-backed threads
- `ThreadListenerCommand::EmitThreadGoalSnapshot { goal_store: Arc<dyn ThreadGoalStore> }`

Set/clear flow becomes:

```text
parse thread id
resolve goal store
if local compatibility requires it, reconcile local rollout
call GoalService
if local live thread exists, append ThreadGoalUpdated rollout item for preview compatibility
send RPC response
emit ordered notification
apply runtime effects
```

Get flow becomes:

```text
parse thread id
resolve goal store
call GoalService::get_thread_goal
return goal or null
```

Resume snapshot flow becomes:

```text
if goals disabled, skip
resolve goal store
if unsupported, log debug or trace and skip
if supported, emit latest goal or cleared notification in listener order
do not block startup on an expected unsupported backend
```

### AWS Object Log Goal Storage

Implement `ThreadGoalStore` for AWS Object Log using DynamoDB.

Preferred data model in the existing table:

```text
pk = NS#{namespace}#THREAD#{thread_id}
sk = GOAL
```

Attributes:

```text
schema_version = 1
namespace
thread_id
goal_id
objective
status
token_budget
tokens_used
time_used_seconds
created_at_ms
updated_at_ms
```

Operations:

- `get_thread_goal`: strongly consistent `GetItem`.
- `replace_thread_goal`: `PutItem` or transactional write with a new `goal_id`, resetting usage counters to zero.
- `insert_thread_goal`: conditional write/update that only replaces when no goal exists or current status is `complete`, matching SQLite behavior.
- `update_thread_goal`: conditional `UpdateItem` with `expected_goal_id` guard when present.
- `delete_thread_goal`: `DeleteItem` returning old attributes if supported by SDK operation shape, otherwise transactionally read then delete with condition.
- `account_thread_goal_usage`: conditional `UpdateItem` that increments counters and updates status in one write.
- `pause_active_thread_goal`: conditional status update from `active`.
- `usage_limit_active_thread_goal`: conditional status update from `active` or `budget_limited`.

AWS should use strongly consistent reads for direct goal operations because goal RPCs are thread-scoped and user-visible.

### Local Rollout Reconciliation

Keep reconciliation only for local compatibility.

Current `reconcile_rollout(...)` scans the local rollout file and repairs SQLite goal state. That remains useful for old local rollouts that have `EventMsg::ThreadGoalUpdated` but no goals DB row.

New rule:

```text
local SQLite-backed goal store:
  may reconcile local rollout before set/clear or first read if needed

remote AWS goal store:
  must not reconcile local rollout
```

For local get, consider avoiding unconditional reconciliation on every read. If existing behavior does not reconcile on get today except via `state_db_for_materialized_thread`, do not add new scan cost.

### Thread Metadata And Preview

`ThreadGoalUpdated` rollout items currently feed thread metadata sync and can set preview text for goal-first threads.

Local:

- Continue appending `ThreadGoalUpdated` rollout items for live local threads after successful goal set.
- Continue using existing metadata sync paths.

AWS:

- Goal state is durable in the goal item.
- Thread preview metadata should be updated via `ThreadStore::update_thread_metadata` or store-owned metadata projection, not by requiring a local rollout append.
- If AWS append of `ThreadGoalUpdated` rollout items is still desired for history replay, append to AWS Object Log through the live thread. This is optional for goal correctness but useful for parity with local history.

### Unsupported Backend Behavior

Unsupported goals should be explicit and cheap.

Expected behavior:

- Direct `thread/goal/get` on unsupported backend returns JSON-RPC invalid request or unsupported operation with a clear message, for example `thread goals are not supported by this thread store`.
- Resume snapshot emission skips unsupported stores without warning-level noise.
- TUI startup should not pay hundreds of milliseconds formatting expected unsupported errors.

This should be treated separately from fatal storage errors:

- Unsupported: expected capability absence.
- Backend error: DynamoDB/SQLite/read failure and should be surfaced/logged.

## Implementation Steps

### Step 1: Introduce The Trait

1. Add `ThreadGoalStoreFuture` and `ThreadGoalStore` to `codex-state`.
2. Document atomicity expectations and `expected_goal_id` semantics.
3. Implement `ThreadGoalStore` for existing `codex_state::GoalStore`.
4. Keep existing tests passing for `codex-state`.

### Step 2: Decouple GoalService From StateRuntime

1. Update `GoalService` methods to accept `&dyn ThreadGoalStore`.
2. Move direct `state_db.thread_goals()` calls behind the trait.
3. Keep validation and runtime-effect behavior in `GoalService`.
4. Update goal extension tests to use the SQLite implementation through the trait.

### Step 3: Add Store Capability Plumbing

1. Add a default `goal_store()` method to `ThreadStore`, returning `None`.
2. Implement it for `LocalThreadStore` by returning the SQLite `GoalStore` when available.
3. Implement it for AWS Object Log by returning an AWS-backed goal store.
4. Implement it for in-memory only if needed by tests; otherwise leave unsupported.

### Step 4: Refactor App-Server Goal Processor

1. Replace `state_db_for_materialized_thread` with `goal_store_for_thread`.
2. Restrict `reconcile_thread_goal_rollout` to local SQLite-backed stores.
3. Update get/set/clear to call `GoalService` with a `ThreadGoalStore`.
4. Update resume snapshot ordering to carry a goal-store handle instead of `StateDbHandle`.
5. Change unsupported resume snapshot logging from `warn` to `debug` or skip silently.

### Step 5: Implement AWS Goal Store

1. Add AWS goal item serialization records under the AWS object-log module.
2. Add DynamoDB read/write helpers for the `GOAL` item.
3. Implement all `ThreadGoalStore` operations with conditional writes.
4. Add unit tests for record serialization and conditional update request construction where possible.
5. Add LocalStack integration coverage for get, set, update, clear, accounting, and process restart behavior.

### Step 6: Preserve Metadata And History Parity

1. Confirm local goal-first thread list preview still updates.
2. Decide whether AWS goal updates should append `EventMsg::ThreadGoalUpdated` to AWS history.
3. If yes, append through the existing live thread persistence path after durable goal update succeeds.
4. If no, ensure AWS thread metadata preview is updated through metadata projection instead.

### Step 7: Clean Up Startup Probe Cost

1. Update TUI resume code to avoid expensive `wrap_err` formatting for expected unsupported goal reads.
2. Prefer capability-aware skip before issuing `thread/goal/get` when app-server exposes enough information.
3. If protocol cannot expose capability yet, make app-server unsupported responses cheap and non-warning.

## Testing Plan

### Unit Tests

- `codex-state`: trait implementation preserves current SQLite goal semantics.
- `codex-goal-extension`: `GoalService` works against a trait-backed fake and SQLite store.
- AWS record serialization round trips.
- AWS conditional update builders cover stale `expected_goal_id` and status filters.

### Integration Tests

- Local app-server:
  - set/get/clear still works
  - goal-first thread preview still appears
  - resume emits goal snapshot in the same order as today
- AWS Object Log with LocalStack:
  - create thread, set goal, kill/recreate app-server/store, resume, get same goal
  - update with correct `expected_goal_id` succeeds
  - update with stale `expected_goal_id` fails or returns no update
  - accounting increments usage atomically
  - clear survives restart
  - no local rollout path is required
- Unsupported store:
  - direct goal RPC returns explicit unsupported error
  - resume does not emit warning-level log spam
  - startup first-frame rendering is not blocked on expected unsupported goal state

## Success Criteria

1. AWS-backed resumed threads can use `thread/goal/get`, `thread/goal/set`, and `thread/goal/clear` without a local rollout file.
2. AWS goal state persists across app-server process restart and machine-local state loss.
3. Local SQLite-backed goals behave the same as before.
4. No app-server goal path requires `thread.rollout_path()` except the local-only reconciliation path.
5. Resume of an AWS thread no longer logs `ephemeral thread does not support goals`.
6. Resume startup avoids the previously observed slow failing `thread/goal/get` path.
7. Tests cover local, AWS, and unsupported-store behavior.

## Risks

1. Goal runtime code may still hold direct `StateRuntime` or `GoalStore` references outside the app-server RPC path.
2. AWS conditional expressions for accounting can become complex and should be reviewed carefully against SQLite behavior.
3. Updating thread preview metadata for AWS may require choosing between appending goal events to history and writing a metadata projection.
4. Adding `goal_store()` to `ThreadStore` expands the trait surface, though the default method keeps existing stores source-compatible.
5. If app-server protocol needs an explicit `supportsGoals` capability later, that should be a separate API compatibility change.

## Implementation Notes

- Do not implement a remote `rollout_path`.
- Do not materialize AWS history into local JSONL as part of goal support.
- Keep goal storage durability separate from rollout history durability, while optionally recording goal events in history for replay parity.
- Prefer small, staged PRs:
  1. trait plus SQLite implementation
  2. GoalService/app-server decoupling
  3. AWS implementation
  4. startup probe optimization
