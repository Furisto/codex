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

## Next work

1. Define delivery stage 3 environment lifecycle API payloads.
2. Add runtime orchestration and a fake adapter for create/read/list/delete behavior.
3. Implement the Ona adapter and replace the no-adapter app-server deletion factory.

This file will be updated after each subsequent milestone is committed.
