# Environment Providers Implementation Status

Last updated: 2026-07-12

## Overall status

Implementation is in progress on branch `ts/env-provider`. Delivery stages 1 and 2 are complete,
including provider configuration CRUD and deletion cleanup. Delivery stage 3 environment
lifecycle APIs and runtime orchestration are next.

## Completed milestones

### 1. Exec-server environment module refactor

Commit: `2161bc15b2 Refactor exec-server environment modules`

- Moved exec-server environment code into `exec-server/src/environment/`.
- Split the module into `manager`, `provider`, and `static_provider` components.
- Renamed `TomlEnvironmentProvider` to `StaticEnvironmentProvider` while preserving TOML as its
  current configuration source.
- Preserved the existing public exec-server exports.

Verification:

- `just fmt` passed.
- All 49 environment-focused exec-server tests passed at the time of the commit.
- Scoped `just fix -p codex-exec-server` passed.
- The full exec-server test run compiled and ran 302 tests; 270 passed and 32 pre-existing
  `file_system_unix` sandbox-helper tests failed because the helper aborted with `SIGABRT` in this
  container. The failures were unrelated to the module refactor.

### 2. Exec-server protocol-version negotiation

Commit: `59b045c972 Negotiate exec-server protocol versions`

- Added `protocolVersion` to the exec-server initialize response.
- Older exec servers that omit the field are interpreted as protocol version 1.
- Current servers always advertise their protocol version.
- Clients store the active connection's negotiated version, including after reconnects.
- Added a centralized operation-to-minimum-version mapping before client RPC calls.
- Added protocol serialization, legacy compatibility, client, and server initialization coverage.

Verification:

- `just test -p codex-exec-server-protocol`: 7/7 passed.
- `just test -p codex-exec-server -E 'not(binary(file_system_unix))'`: 225/225 passed, with two
  skipped tests.
- Scoped fixes passed for both `codex-exec-server-protocol` and `codex-exec-server`.

### 3. Qualified environment identity

Commit: `8a1053caa8 Qualify environment IDs by provider`

- Added canonical `providerId/environmentId` identity helpers.
- Bare legacy IDs resolve as aliases for `static/<id>`.
- Qualified IDs split only on the first slash, preserving slashes in provider-native IDs.
- Changed the built-in IDs to `static/local` and `static/remote`.
- Static TOML and environment-variable registrations now produce canonical IDs.
- `EnvironmentManager` stores and returns canonical IDs while accepting bare static aliases for
  lookup and legacy snapshots.
- Reserved `static/local` cannot be replaced through environment upsert.
- `environment/add` is restricted to the static provider and rejects qualified IDs for other
  providers.
- Thread and turn environment selections are canonicalized at the app-server boundary and again
  when core resolves legacy persisted selections.
- Duplicate selection validation treats bare and canonical static IDs as the same environment.
- Updated the app-server README and affected tests for canonical identity behavior.

Verification:

- Exec-server environment tests: 55/55 passed.
- Core environment-selection tests: 14/14 passed.
- `cargo check -p codex-app-server --lib` passed.
- Scoped fixes passed for `codex-exec-server` and `codex-core`.
- App-server library-only Clippy passed.

Known test blockers unrelated to this milestone:

- Package-wide `codex-app-server` tests currently fail to compile in existing
  `thread_processor_tests.rs` because `codex_protocol::protocol::SandboxMode` is missing and
  `JSONRPCErrorError` does not implement `StdError`.
- The app-server integration-test binary also has an existing compile error in
  `remote_thread_store.rs`: a `CreateThreadParams` initializer is missing `config_snapshot`.
  These prevent running the new `environment/add` integration test without changing unrelated
  code.

### 4. Environment provider domain and store contract

Commit: `7e107ed264 Add environment provider store contract`

- Added the dedicated `codex-environment-provider` crate instead of expanding `codex-core`.
- Added the storage-neutral `EnvironmentProviderStore` trait following the `ThreadStore` pattern,
  with explicit `Send` futures and create, read, cursor-paginated list, update, and delete methods.
- Kept the built-in static provider and provider-owned environment state explicitly outside the
  persistence boundary.
- Added provider-definition domain types for opaque IDs, immutable kinds and resolved URLs,
  display and normalized names, and versioned encrypted PAT ciphertext.
- Limited update parameters structurally to the provider name and encrypted authentication.
- Documented that store implementations assign IDs, reject persisted static providers, enforce
  case-insensitive name uniqueness transactionally, and order pages by normalized name and ID.
- Added the crate to Cargo and Bazel workspace metadata and refreshed both lockfiles.

Verification:

- `just test -p codex-environment-provider`: 1/1 passed.
- `just fix -p codex-environment-provider` passed.
- `just bazel-lock-update` passed after installing the repository-pinned Bazelisk launcher.

### 5. State persistence for provider definitions

Commit: `a9048da3c3 Persist environment provider definitions in state`

- Added state migration `0041_environment_providers.sql`.
- Persisted opaque provider ID, display and normalized names, kind, resolved URL, authentication
  kind, credential format version, and ciphertext.
- Added `StateRuntime` primitives for create, read, keyset-ordered list, atomic mutable-field
  update, and delete.
- Provider IDs are generated as UUIDs by the local storage backend.
- The normalized-name unique index is the transactional authority for case-insensitive name
  conflicts, including concurrent updates.
- Credential versions are constrained to the `u32` range in SQLite.

Verification:

- `just test -p codex-state`: 152/152 passed.
- `just fix -p codex-state` passed.

### 6. Local environment provider store

Commit: `fa889ba931 Implement local environment provider store`

- Implemented `LocalEnvironmentProviderStore` over an initialized `StateRuntime`.
- Mapped provider kinds and tagged authentication between the storage-neutral domain and state
  rows without exposing plaintext credential fields.
- Added opaque JSON keyset cursors and limit validation for provider listing.
- Mapped missing providers, duplicate normalized names, invalid cursors, storage unavailability,
  and corrupt persisted discriminants into typed store errors.
- Preserved immutable IDs, kinds, and URLs by exposing only name and authentication through the
  update path.
- Added coverage for complete-record CRUD round trips, credential replacement, case-insensitive
  create/update conflicts, cursor pagination and ordering, invalid cursors, and rejection of the
  synthesized static provider.

Verification:

- `just test -p codex-environment-provider`: 5/5 passed.
- `just fix -p codex-environment-provider` passed.
- `just bazel-lock-update` passed after the crate dependency changes.

### 7. Provider credential encryption

Commit: `a370d74fc7 Encrypt environment provider credentials`

- Added `EnvironmentProviderCredentialCipher` to `codex-secrets`.
- Reused the existing age/scrypt encryption path and random 256-bit key generation.
- Stored the encryption key under a dedicated `environment-provider-credentials|<home-hash>` OS
  keyring account, separate from the managed-secrets key.
- Returned caller-owned ciphertext with an explicit format version; the cipher itself does not
  persist provider configuration or ciphertext.
- Decryption requires the existing key and never silently creates a replacement key.
- Keyring load/save errors, missing keys, unsupported versions, and invalid ciphertext fail closed.
- Added coverage proving randomized ciphertext round trips, plaintext is absent from ciphertext
  and keyring values, missing-key behavior, keyring failures, and version rejection.

Verification:

- `just test -p codex-secrets`: 11/11 passed.
- `just fix -p codex-secrets` passed.

### 8. Provider configuration service domain

Commit: `729987c352 Define environment provider service domain`

- Added the user-visible provider model shared by static and dynamic definitions.
- Added redacted authentication metadata and a PAT input wrapper whose debug representation never
  exposes its value.
- Added create, update, cursor-page, and resolved authenticated provider types for the
  configuration service boundary.
- Added typed service errors for not-found, case-insensitive name conflicts, invalid input and
  cursors, unavailable storage, unavailable credentials, and corrupt internal state.
- Mapped storage-neutral store errors into the service error model without losing their category.

Verification:

- `just test -p codex-environment-provider`: 5/5 passed for the domain/store-only commit.

### 9. Provider configuration service orchestration

Commit: `1098b48543 Orchestrate environment provider configuration`

- Added `EnvironmentProviderService` over an optional storage-neutral provider store and the
  provider credential cipher.
- Synthesized the fixed `static` provider with ID `static`, display name `Static`, and null URL and
  authentication.
- Kept static provider listing available when dynamic storage is absent or unavailable; dynamic
  CRUD fails with a typed storage-unavailable error.
- Resolved the Ona default URL to `https://app.gitpod.io/api`, validated absolute HTTP(S) URLs, and
  normalized trailing path slashes without contacting the provider.
- Encrypted PATs before passing definitions to persistence and returned only redacted PAT metadata.
- Decrypted PATs only through `resolve_provider`, the boundary intended for authenticated provider
  operations.
- Implemented provider-list pagination across the synthesized static row and store-owned dynamic
  keyset cursors.
- Allowed only name and PAT replacement, and made keyring failures block PAT changes while leaving
  rename-only updates available.
- Exposed definition deletion only as a post-cleanup operation; provider adapter orchestration must
  enforce normal and forced environment cleanup before calling it.
- Added coverage for static-only operation, URL/default validation, encrypted persistence with no
  plaintext PAT in SQLite files, credential resolution, immutable URL/kind behavior, pagination,
  static mutation rejection, and keyring failure behavior.

Verification:

- `just test -p codex-environment-provider`: 11/11 passed.
- `just fix -p codex-environment-provider` passed.
- `just bazel-lock-update` passed after the crate dependency changes.

### 10. App-server v2 provider payloads

Commit: `1739dc1e0b Define environment provider API payloads`

- Added v2 provider kinds `static` and `ona`.
- Added explicitly tagged PAT input `{type: "pat", token}` and redacted output `{type: "pat"}`.
- Added the common provider response with nullable URL and authentication for the built-in static
  provider.
- Added create and update params/responses, with URL optional only on create and only name and
  authentication present on update.
- Added cursor-paginated list params/response.
- Added delete params with `force` defaulting to false and cleanup results containing
  `complete`/`partial`/`unknown` plus failed environment IDs.
- Kept these as payload definitions only; methods are not registered until handlers enforce the
  configuration and cleanup semantics.
- Added wire-format coverage for tagged PAT input, redacted/static output, camelCase fields, and
  the force default.

Verification:

- `just test -p codex-app-server-protocol`: 254/254 passed.
- `just fix -p codex-app-server-protocol` passed.

### 11. App-server provider create, update, and list APIs

Commit: `cb14d874a0 Expose environment provider configuration APIs`

- Registered experimental v2 methods `environmentProvider/create`, `update`, and `list` with
  global write/read serialization scopes.
- Initialized one process-scoped provider configuration service from the local state database and
  platform keyring; when state is unavailable the service remains static-only.
- Wired create and update through URL/default validation, PAT encryption, immutable-field policy,
  and redacted responses.
- Wired list through the service-owned cursor that merges the fixed static provider with dynamic
  store pagination, with API limits clamped to 1–100 and a default of 50.
- Mapped validation, conflicts, missing providers, storage failures, credential failures, and
  internal corruption into app-server request/internal error categories.
- Updated the app-server README and regenerated schemas in experimental and stable modes.
- Added end-to-end JSON-RPC coverage for static/dynamic listing, persisted definition mapping,
  rename-only update, and static create rejection before keyring access.

Verification:

- `just test -p codex-app-server-protocol`: 254/254 passed after method registration.
- `cargo check -p codex-app-server --lib` passed.
- The two targeted `environment_provider` app-server integration tests passed after temporarily
  adding the unrelated missing `config_snapshot: None` initializer in `remote_thread_store.rs`;
  that temporary edit was removed afterward.
- Scoped fixes passed for `codex-app-server-protocol` and `codex-environment-provider`.
- App-server `just fix` remains blocked by the previously documented unrelated
  `thread_processor_tests.rs` and `remote_thread_store.rs` compilation errors.
- `just bazel-lock-update` passed.

### 12. Generic environment provider adapter contract

Commit: `a6df07b5cd Define environment provider adapter contract`

- Added a documented `EnvironmentProviderAdapter` trait with explicit `Send` futures for dynamic
  environment create, read, provider-owned cursor list, delete, and the provider's single watch.
- Added a documented adapter factory from a persisted definition with decrypted authentication.
- Added common dynamic-environment domain records for qualified references, structured repository
  source and opaque Git ref, provider-native resource class, status, and Ona-aligned phases.
- Added provider event signals that carry resource IDs so orchestration can perform authoritative
  reads and reconciliation instead of leaking vendor event payloads.
- Kept provider-native pagination cursors behind the adapter boundary.

Verification:

- `just test -p codex-environment-provider`: 11/11 passed.
- `just fix -p codex-environment-provider` passed.
- `just bazel-lock-update` passed after adding the stream dependency.

### 13. Provider deletion cleanup orchestration

Commit: `b0dd256287 Enforce environment provider deletion cleanup`

- Added a separate deletion service over provider configuration and the adapter factory.
- Normal deletion resolves authenticated provider configuration, constructs the adapter, queries
  the authoritative provider, and removes the definition only when the first page proves it empty.
- Normal deletion rejects non-empty providers and fails closed on credential, adapter-construction,
  provider-list, or storage failures.
- Forced deletion consumes all reachable provider pages, detects repeated cursors, deduplicates
  environment IDs, and issues deletion requests with concurrency capped at eight.
- Forced cleanup reports `complete` when enumeration and all deletes succeed, `partial` with sorted
  known failed IDs when complete enumeration has delete failures, and `unknown` when enumeration or
  adapter construction is incomplete.
- Forced deletion still attempts definition removal when no adapter is available or listing fails,
  and deletes IDs discovered before a later page failure.
- Static provider deletion is rejected before any cleanup work.
- Added fake-adapter coverage for empty/non-empty normal deletion, fail-closed list errors, forced
  complete/partial/unknown results, bounded cleanup behavior, and no-adapter definition removal.

Verification:

- `just test -p codex-environment-provider`: 18/18 passed.
- `just fix -p codex-environment-provider` passed.

### 14. App-server provider delete API

Commit: `de5517ccfe Expose environment provider delete API`

- Registered the experimental v2 `environmentProvider/delete` method and dispatched it through
  the environment request processor.
- Wired normal deletion to fail closed unless authenticated provider cleanup can prove the
  provider has no environments.
- Wired forced deletion to best-effort cleanup and return `complete`, `partial`, or `unknown` with
  known failed environment IDs before removing the provider definition.
- Kept the static provider undeletable through the domain deletion service.
- Initialized app-server deletion without dynamic adapters for now: normal deletion therefore
  fails closed, while forced deletion removes the definition and reports `unknown`. The Ona
  adapter milestone will provide live cleanup.
- Updated the app-server README and regenerated stable and experimental schemas.
- Added end-to-end JSON-RPC coverage proving forced deletion reports unknown without an adapter,
  removes the dynamic definition, and preserves the static provider.

Verification:

- `just test -p codex-app-server-protocol`: 254/254 passed.
- `cargo check -p codex-app-server --lib` passed.
- All three targeted `environment_provider` app-server integration tests passed after temporarily
  adding the unrelated missing `config_snapshot: None` initializer in `remote_thread_store.rs`;
  that temporary edit was removed afterward.
- Scoped fixes passed for `codex-app-server-protocol` and `codex-environment-provider`.
- App-server `just fix` remains blocked by the previously documented unrelated
  `thread_processor_tests.rs` and `remote_thread_store.rs` compilation errors.

### 15. Environment lifecycle API payloads

Commit: `b4f15db1d1 Define environment lifecycle API payloads`

- Added the common provider-qualified environment reference, structured repository source with a
  required opaque Git ref, provider-native resource class, lifecycle phase, and status payloads.
- Added one environment response shape shared by dynamic and static environments; static records
  represent source and resource class as `null` and use the common status model.
- Added experimental v2 payload definitions for environment create, read, cursor-paginated list,
  and asynchronous delete.
- Kept dynamic create inputs provider-independent and required: provider ID, source, and resource
  class, with no provider-specific options.
- Made create return only the provider-qualified environment reference and delete return an empty
  success response.
- Added created/updated notification payloads carrying complete records and a deleted payload
  carrying the canonical qualified environment ID.
- Kept the payloads unregistered until executable lifecycle handlers and notification delivery are
  present, so schema fixtures correctly remain unchanged in this milestone.

Verification:

- `just test -p codex-app-server-protocol`: 256/256 passed.
- `just fix -p codex-app-server-protocol` passed.
- Experimental and stable schema generation both completed and confirmed no unregistered surface
  was exported.

### 16. Shared provider runtime adapters and mutation locks

Commit: `8c23a59013 Share environment provider runtime adapters`

- Added a runtime adapter pool that lazily resolves provider credentials, constructs exactly one
  adapter per provider definition, and shares concurrent initialization attempts.
- Added explicit adapter invalidation so PAT replacement and provider removal can retire cached
  authenticated adapters.
- Added per-provider asynchronous mutation locks with independent concurrency across provider IDs
  and no permanently retained lock entries.
- Moved provider deletion onto the shared adapter pool and held its provider mutation lock across
  authoritative enumeration, cleanup, and definition removal.
- Invalidated the cached adapter only after normal or forced provider-definition deletion
  succeeds, preserving retry behavior when storage removal fails.
- Kept the existing public deletion-service constructors and cleanup semantics intact while adding
  a crate-private constructor for lifecycle orchestration to share the same pool and locks.

Verification:

- `just test -p codex-environment-provider`: 18/18 passed.
- `just fix -p codex-environment-provider` passed.
- `just bazel-lock-update` passed after promoting Tokio synchronization support to a runtime
  dependency; no lockfile content changed.

### 17. Dynamic environment lifecycle orchestration

Commit: `7e93d2aee2 Orchestrate environment lifecycle operations`

- Added `EnvironmentLifecycleService` for authoritative dynamic create, read, cursor list, and
  asynchronous delete operations.
- Routed all operations through the shared one-adapter-per-provider pool, while reads and lists
  always call the provider instead of consulting a local projection.
- Serialized environment create and delete with provider deletion through the same per-provider
  mutation lock domain.
- Returned the provider's complete initial record from create so app-server orchestration can
  project it, emit a created notification, and return only its reference to the client.
- Rejected static create/delete before adapter resolution; static reads/lists remain an app-server
  routing concern over the existing environment manager.
- Validated that every create/read/list record is owned by the requested provider definition and
  treated mismatches as invalid provider responses.
- Exposed adapter invalidation for PAT replacement and provider removal, and made lifecycle-created
  provider deletion share the same cached adapter and locks.
- Added fake-adapter coverage for parameter routing, authoritative records and cursors, adapter
  reuse, explicit invalidation, and shared provider deletion.

Verification:

- `just test -p codex-environment-provider`: 20/20 passed.
- `just fix -p codex-environment-provider` passed.

### 18. App-server environment lifecycle APIs

Commit: `4aec29a398 Expose environment lifecycle APIs` (pushed to `ts/env-provider`)

- Registered experimental v2 `environment/create`, `read`, `list`, and `delete` methods and
  dispatched them through a dedicated lifecycle request processor.
- Registered experimental `environment/created`, `updated`, and `deleted` notification shapes;
  successful dynamic creation immediately emits the complete created record and returns only its
  provider-qualified reference.
- Mapped dynamic API requests to the shared lifecycle service, preserving provider-native cursors
  and all common status phases and errors.
- Added a process-scoped unavailable adapter factory as the explicit placeholder until the Ona
  implementation is installed; dynamic lifecycle operations fail clearly while static behavior
  and provider configuration remain available.
- Made PAT updates invalidate the cached authenticated adapter, while rename-only updates leave it
  running; provider deletion now shares the lifecycle adapter and mutation lock domain.
- Added stable, lexically ordered environment ID snapshots to `EnvironmentManager`.
- Implemented static `environment/read` and cursor-paginated `environment/list` over the live
  manager, returning nullable source/resource class and a running status. Static create/delete are
  rejected before adapter access.
- Updated the app-server README and regenerated stable and experimental schema fixtures.
- Added end-to-end JSON-RPC coverage for static read, multi-page opaque cursor listing, lifecycle
  response shapes, and static create/delete rejection.

Verification:

- `just test -p codex-app-server-protocol`: 256/256 passed.
- `just test -p codex-environment-provider`: 20/20 passed.
- `just test -p codex-exec-server -E 'test(environment)'`: 64/64 passed.
- `cargo check -p codex-app-server --lib` passed.
- All four targeted `environment_provider` app-server integration tests passed after temporarily
  adding the unrelated missing `config_snapshot: None` initializer in `remote_thread_store.rs`;
  that temporary edit was removed afterward.
- Scoped fixes passed for `codex-app-server-protocol`, `codex-environment-provider`, and
  `codex-exec-server`.
- App-server `just fix` remains blocked by the previously documented unrelated
  `thread_processor_tests.rs` compilation errors.

### 19. Provider lifecycle reconciliation state machine

Commit: `651c7e186a Reconcile environment provider lifecycle state` (pushed to
`ts/env-provider`)

- Added a provider-local, in-memory `EnvironmentReconciler`; its projection is explicitly
  non-durable and never replaces the external provider as authority.
- Implemented complete multi-page reconciliation with provider-owned cursors, repeated-cursor
  detection, duplicate-ID rejection, and provider-ownership validation.
- Diffed successful snapshots deterministically into normalized created, updated, and deleted
  lifecycle events.
- Replaced the projection only after every page succeeds, preserving the last known-good state
  across provider failures and malformed pagination for reconnect recovery.
- Applied provider watch signals by reading complete records after change events, suppressing
  unchanged updates, translating read-after-change not-found races into deletion, and ignoring
  duplicate/unknown delete events.
- Added fake-adapter coverage for initial and subsequent reconciliation, multi-page ordering,
  repeated-cursor recovery without projection loss, complete-record watch reads, and delete
  deduplication.

Verification:

- `just test -p codex-environment-provider`: 23/23 passed before the final repeated-cursor test
  refinement.
- `just test -p codex-environment-provider -E 'test(reconciliation)'`: 3/3 passed after the final
  refinement.
- `just fix -p codex-environment-provider` passed.

### 20. Reconnectable provider watch runner

Commit: `0de8bdd3c0 Reconnect environment provider watches` (pushed to `ts/env-provider`)

- Added a watch runner for one configured provider adapter and its in-memory reconciler.
- Required every watch connection attempt to complete authoritative full reconciliation before
  opening the provider event stream, recovering changes missed while disconnected.
- Routed normalized lifecycle changes through a bounded Tokio channel so a slow consumer applies
  backpressure rather than allowing unbounded buffering.
- Read complete records after changed signals through the reconciler and emitted only deduplicated
  created, updated, or deleted events.
- Treated provider errors and cleanly ended streams as reconnectable failures with capped
  exponential retry delays.
- Stopped the runner permanently when its event receiver closes, giving the future task owner a
  clean shutdown mechanism in addition to task cancellation.
- Added fake-watch coverage proving initial reconciliation precedes signals, a second connection
  reconciles missed resources before resuming events, ended streams reconnect, and closed event
  consumers stop delivery.

Verification:

- `just test -p codex-environment-provider`: 25/25 passed.
- `just fix -p codex-environment-provider` passed.
- `just bazel-lock-update` passed after enabling Tokio time support; no lockfile content changed.

### 21. Ona lifecycle wire and domain mapping

Commit: `fe15306333 Map Ona environment lifecycle records` (pushed to `ts/env-provider`)

- Added typed serde models for the Ona environment fields needed by the common lifecycle adapter,
  following the local `environment.proto` protobuf-JSON shapes.
- Mapped common create source/resource class into Ona's running desired phase, machine class, and
  Git initializer.
- Interpreted explicit head/tag refs and commit-like hashes inside the Ona adapter while treating
  other opaque refs as branches; no provider-specific public API options were added.
- Annotated create requests with `openai.com/codex-provider-id=<providerId>` for ownership filtering
  and a separate Codex source-ref annotation so the client's original opaque ref survives Ona's
  target-mode representation.
- Mapped owned Ona records back into complete common environment records, including provider-native
  ID, repository source, original ref, resource class, lifecycle phase, and combined failure text.
- Failed closed for foreign resources and malformed owned records instead of leaking unrelated Ona
  environments or synthesizing incomplete state.
- Added coverage for exact create JSON, annotation mapping, source-ref round trips, phase/error
  mapping, local ownership filtering, and malformed-resource rejection.

Verification:

- `just test -p codex-environment-provider`: 28/28 passed before the final lint-only enum rename.
- `just test -p codex-environment-provider -E 'test(ona::tests)'`: 3/3 passed after the rename.
- `just fix -p codex-environment-provider` passed without warnings.

### 22. Ona unary lifecycle adapter

Commit: `953c2958dd Implement Ona environment lifecycle adapter` (pushed to
`ts/env-provider`)

- Added the concrete Ona adapter factory over resolved provider definitions and decrypted PAT
  authentication.
- Implemented authenticated protobuf-JSON Connect calls for Ona environment create, get, list, and
  delete using the provider's resolved control-plane URL.
- Sent bearer authentication and Connect protocol version headers on provider requests without
  exposing the PAT through debug output or responses.
- Classified invalid requests, missing environments, authentication/rate-limit/server
  unavailability, and malformed successful responses into the common adapter error model.
- Bounded provider error-body text before including it in errors.
- Implemented Ona's required local annotation filtering: each upstream list request is sized to the
  remaining requested Codex page, foreign resources are discarded locally, and successive
  upstream tokens are fetched until the Codex page is full or Ona is exhausted.
- Preserved Ona's opaque continuation token as the Codex cursor and rejected repeated tokens.
- Kept Ona event watch construction explicitly unavailable for the next streaming milestone.
- Added mock-server coverage for exact routes, PAT and Connect headers, create/read/delete mapping,
  request bodies, two-page post-filter filling, and cursor propagation.

Verification:

- `just test -p codex-environment-provider`: 30/30 passed.
- `just fix -p codex-environment-provider` passed.
- `just bazel-lock-update` passed after adding Reqwest and Wiremock dependencies; Cargo lock
  membership was refreshed and the Bazel lockfile required no content change.

### 23. App-server Ona lifecycle activation

Commit: `6b4a67cdb9 Enable Ona lifecycle APIs in app server` (pushed to
`ts/env-provider`)

- Replaced the app-server's placeholder no-adapter lifecycle service with the concrete Ona adapter
  factory.
- Dynamic `environment/create`, `environment/read`, `environment/list`, and `environment/delete`
  requests now resolve persisted Ona providers and execute through their authenticated control
  plane adapter.
- Provider deletion cleanup now uses the same authoritative Ona adapter pool instead of failing
  because no dynamic adapter factory was registered.
- Kept static environment handling unchanged and isolated from the dynamic provider adapter path.

Verification:

- `cargo check -p codex-app-server --lib` passed.
- The attempted spawned-process API test reached provider creation but cannot run in this test
  environment because its OS keyring is unavailable; no test-only credential injection hook was
  added. Ona HTTP authentication and lifecycle mapping remain covered by the 30 passing
  `codex-environment-provider` tests from milestone 22.

### 24. Process-scoped provider watch ownership

Commit: `fff23a1380 Own environment provider watch tasks` (pushed to
`ts/env-provider`)

- Added a crate-owned watch manager that retains exactly one abortable task for each configured
  dynamic provider.
- Starting an existing provider replaces its task, providing the lifecycle needed for PAT-driven
  adapter replacement and fresh reconciliation.
- Excluded the built-in static provider from dynamic watch construction.
- Retried provider configuration and adapter resolution failures with capped exponential backoff;
  provider-level watch failures continue to use the runner's reconcile-before-reconnect loop.
- Routed every provider task through one caller-owned bounded event channel so downstream
  processing applies shared backpressure.
- Added explicit per-provider stop and process-wide shutdown operations.
- Added coverage for initial task startup, replacement, cached adapter reuse, reconciliation event
  delivery, task stop, and static-provider exclusion.

Verification:

- `just test -p codex-environment-provider`: 32/32 passed.
- `just fix -p codex-environment-provider` passed.
- `just bazel-lock-update` passed after enabling Tokio runtime support; no lockfile content changed.

### 25. App-server watch startup and lifecycle notification delivery

Commit: `61f59a8025 Deliver environment provider lifecycle events` (pushed to
`ts/env-provider`)

- Added a process-scoped app-server worker that cursor-lists all configured provider definitions at
  startup and starts their dynamic watch tasks.
- Hooked provider create, PAT update, and successful provider deletion into watch start,
  stop/invalidate/restart, and stop behavior respectively; name-only updates leave the watch
  untouched.
- Routed explicit environment creation through the same bounded lifecycle stream as provider
  reconciliation and watch events.
- Maintained one non-durable app-server projection for notification normalization: first sightings
  emit `environment/created`, changed complete records emit `environment/updated`, identical
  records are suppressed, and known removals emit `environment/deleted` with the canonical
  qualified ID.
- This shared projection suppresses the duplicate created notification that would otherwise occur
  when reconciliation observes the record returned by `environment/create`.
- Added process shutdown handling for watch tasks and event delivery.
- Added normalization coverage for explicit create, duplicate reconciliation, phase update,
  deletion, and duplicate deletion.

Verification:

- `just test -p codex-environment-provider`: 32/32 passed after adding explicit event publishing.
- `just fix -p codex-environment-provider` passed.
- `cargo check -p codex-app-server --lib` passed.
- `cargo clippy -p codex-app-server --lib --no-deps -- -D warnings` passed.
- The focused app-server unit test and `just fix -p codex-app-server` remain blocked before running
  this code by unrelated existing `thread_processor_tests.rs` errors: the stale `SandboxMode` path
  and three `JSONRPCErrorError` conversions to `anyhow::Error`.

### 26. Ona Connect event stream

Commit: `9a59d1d0cd Stream Ona environment lifecycle events` (pushed to
`ts/env-provider`)

- Implemented Ona's organization-scoped `EventService/WatchEvents` server stream with an
  environment resource-type filter.
- Sent PAT authentication, Connect protocol version, streaming content type, and a correctly
  framed protobuf-JSON request.
- Added a separate bounded Connect JSON framing decoder for arbitrary HTTP chunk boundaries,
  standard messages, final EndStream envelopes, and provider-reported stream errors.
- Rejected compressed/reserved flags, oversized frames/buffers, malformed messages, missing final
  EndStream envelopes, and buffered data after EndStream.
- Mapped Ona create, update, and update-status operations to generic changed signals and delete
  operations to generic deleted signals; reconciliation still reads authoritative complete records
  and filters ownership locally.
- The existing watch runner now performs initial reconciliation, consumes real Ona events, and
  fully reconciles before every reconnect.
- Added mock-server coverage for the exact authenticated/framed request and streamed operation
  mapping, plus framing/parser and wire-model coverage.

Verification:

- `just test -p codex-environment-provider`: 36/36 passed.
- `just fix -p codex-environment-provider` passed.
- `cargo check -p codex-environment-provider` passed after the final bounded-buffer refinement.
- `just bazel-lock-update` passed after enabling Reqwest streaming; no lockfile content changed.

### 27. Reconnectable provider WebSocket transport

Commit: `4a8adcec3e Reconnect provider WebSocket environments` (pushed to
`ts/env-provider`)

- Added an exec-server connection-provider boundary that resolves a fresh authenticated WebSocket
  URL immediately before every physical connection attempt.
- Added a dynamic WebSocket transport and reconnect strategy alongside the existing static URL and
  Noise rendezvous strategies.
- Preserved logical exec-server session resume while replacing short-lived physical connection
  material on reconnect.
- Applied the existing provider/registry reconnect backoff to dynamic WebSocket material and
  connection failures.
- Added `EnvironmentManager` operations to install/replace connector-backed environments and to
  remove environments while cancelling unfinished startup work.
- Kept connector-backed URLs out of debug/environment metadata because the resolved URL may carry
  short-lived authentication.
- Added coverage proving separate connection attempts resolve separate URLs and proving replacement
  and removal cancel superseded startup tasks.

Verification:

- Focused exec-server tests: 2/2 passed for dynamic URL resolution and replacement/removal startup
  cancellation.
- `just fix -p codex-exec-server` passed.
- The full `just test -p codex-exec-server` run executed the new connector test successfully, but
  finished with 40 unrelated `file_system_unix` failures after the filesystem sandbox helper
  aborted with `SIGABRT`; 269 tests passed.

### 28. Provider execution projection and Ona connector

Commit: `5244afc2d0 Project provider environments for execution` (pushed to
`ts/env-provider`)

- Added a provider-domain connector boundary that returns fresh connection material without
  exposing it through app-server lifecycle payloads.
- Implemented pooled connectors that resolve the current adapter on every connection attempt, so
  PAT invalidation automatically switches existing execution entries to the rebuilt adapter.
- Added Ona connection resolution that re-reads the authoritative environment immediately before
  each connection and extracts its short-lived `status.execServerUrl`.
- Kept running environments in lifecycle APIs even when connection material is absent; selecting
  one for execution fails explicitly when the connector resolves, as designed.
- Projected created/updated running provider environments into `EnvironmentManager` using canonical
  `providerId/environmentId` identities and connector-backed WebSocket transport.
- Removed non-running, deleted, and provider-deleted environments from the execution projection,
  cancelling superseded startup work through `EnvironmentManager`.
- Added adapter and lifecycle coverage for Ona URL extraction and connector behavior across adapter
  invalidation.

Verification:

- `just test -p codex-environment-provider`: 36/36 passed.
- `just fix -p codex-environment-provider` passed.
- `cargo check -p codex-app-server --lib` passed.
- `cargo clippy -p codex-app-server --lib --no-deps -- -D warnings` passed.

## Next work

1. Run final targeted/schema verification, review the accumulated diff, and resolve any remaining
   integration gaps before requesting the complete workspace test run.

This file will be updated after each subsequent milestone is committed.
