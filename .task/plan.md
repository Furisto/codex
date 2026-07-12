# Environment Providers

## Summary

Generalize environment support around persisted provider definitions and provider-specific adapters, with Ona as the first dynamic implementation. Preserve the built-in static provider and existing `environment/add` behavior.

Implementation should be delivered as reviewable stages: foundational exec-server changes; provider storage and CRUD; generic lifecycle APIs/runtime; then the Ona adapter and watch integration.

## Public APIs and compatibility

- Add experimental app-server v2 methods:

  - `environmentProvider/create|update|list|delete`
  - `environment/create|read|list|delete`

- Model providers as `{id, name, kind, url, authentication}`. Supported kinds are initially `static` and `ona`; dynamic creation rejects `static`. PAT input is `{type:"pat", token}`, while responses expose only `{type:"pat"}`.
- Provider creation accepts an optional URL. Ona defaults to `https://app.gitpod.io/api`; resolve, normalize, persist, and return the concrete URL. Provider update accepts only optional `name` and `authentication`.
- Provider lists use `{cursor, limit}` and return `{data, nextCursor}`. Order the fixed `static` provider first and dynamic providers by normalized name and ID.
- Provider deletion accepts `force`, defaulting to false, and always returns cleanup status. Normal deletion fails closed unless the provider can be queried and has no environments. Forced deletion best-effort deletes discovered environments before removing the definition and reports `complete`, `partial`, or `unknown`.
- Model an environment reference as `{providerId, environmentId}` and an environment as `{ref, source, resourceClass, status}`. Dynamic environments require source and resource class; static records serialize both as `null` and use `running` to mean statically registered.
- `environment/create` requires provider ID, `{repositoryUrl, ref}`, and an opaque `resourceClass`, and returns only the environment reference. Read/delete take provider ID plus provider-native environment ID. List requires provider ID and supports opaque cursor pagination.
- `environment/create` and `environment/delete` reject `providerId: "static"`. `environment/add` remains the only static mutation, always targets `static`, and remains process-local.
- Emit `environment/created` and `environment/updated` with the complete environment record, and `environment/deleted` with the canonical qualified ID.
- Canonicalize environment selections everywhere as `providerId/environmentId`, splitting only on the first slash. Continue accepting bare legacy IDs as aliases for `static/<id>`, including persisted rollouts and existing thread/turn requests. Return canonical IDs from new and existing APIs.
- Keep existing `environment/add` and `environment/info` methods compatible.
- Extend exec-server initialization with integer `protocolVersion`. Servers always send it; clients default a missing field to version 1. Store the negotiated version per connection and centrally map exec-server operations to minimum versions. Existing operations remain version 1 and unsupported calls still return explicit errors.

## Architecture and implementation

- Move exec-server environment code into a private `environment/` module tree and rename `TomlEnvironmentProvider` to `StaticEnvironmentProvider`. Keep `EnvironmentManager` focused on the live execution projection.
- Introduce a dedicated environment-provider crate rather than adding this feature to `codex-core`. It owns:

  - Provider and environment domain models.
  - A documented, storage-neutral `EnvironmentProviderStore` trait using explicit `Send` futures.
  - A documented provider-adapter trait for create/read/list/delete/watch.
  - Provider factories, lifecycle orchestration, pagination, and watch task ownership.
  - The Ona implementation.

- Add a local store implementation over `codex-state`. Persist opaque provider ID, display and normalized names, kind, resolved URL, auth kind, and versioned PAT ciphertext. Enforce immutable IDs/kinds/URLs and case-insensitive unique names transactionally.
- Synthesize the static provider rather than storing it. If the state database is unavailable, static environments continue working and provider listing returns static; dynamic CRUD returns a storage-unavailable error.
- Extend `codex-secrets` with a small caller-owned ciphertext facility. Store the encryption key under a dedicated keyring identity and the ciphertext in the provider row. Never fall back to plaintext. Keyring failures prevent create, PAT update, and authenticated provider operations; redacted provider listing remains available. Key rotation and recovery are outside v1.
- On startup, load provider definitions, construct one adapter per definition, perform an initial reconciliation, populate `EnvironmentManager`, and start one watch task per provider. Keep only an in-memory last-seen projection for notification diffing.
- Updating a PAT replaces the adapter, terminates its old watch and connectors, and performs a fresh reconciliation. Renaming only updates stored metadata. Deleting a provider stops its watch and removes its environments from `EnvironmentManager`.
- Serialize create/delete/provider-delete operations per provider to prevent local races. Reads and lists always query the provider rather than the in-memory projection.
- Generalize the existing reconnectable exec-server transport around a private `EnvironmentConnector`. A ready provider environment carries an optional connector; `EnvironmentManager` asks it for fresh connection material on every reconnect.
- Ona connectors use a direct authenticated exec-server WebSocket URL supplied in Ona environment metadata. Re-read the environment before each reconnect to obtain fresh metadata. If a running environment lacks the URL, retain it in lifecycle APIs but make execution selection fail explicitly as unavailable.
- The Ona adapter uses typed Connect-compatible HTTP payloads for environment create/get/list/delete and event watch:

  - Map `repositoryUrl` and opaque `ref` into Ona’s Git initializer and `resourceClass` into its machine class.
  - Annotate environments with `openai.com/codex-provider-id=<providerId>`.
  - List all Ona environments and filter that annotation locally.
  - Fill a requested page by fetching successive upstream pages until enough matching environments are collected or upstream is exhausted; return the upstream continuation token as the opaque Codex cursor.
  - Reconciliation consumes every upstream page.
  - Watch organization-scoped environment events. Read and filter resources after create/update events; process delete events only for IDs already tracked by the adapter.
  - Use Ona `statusVersion` to discard stale updates and map phases/errors into the common status model.
  - After watch failure, retry with backoff and run a complete reconciliation before resuming event processing.

- After explicit creation, immediately project and notify the returned creating environment; watch reconciliation deduplicates it. Register its connector only when it becomes executable. On deleting/deleted or reconciliation removal, remove it from `EnvironmentManager`.
- Force provider deletion enumerates only environments bearing that provider definition’s annotation, issues bounded-concurrency delete requests, then attempts definition deletion regardless of enumeration or provider failures.

## Delivery stages

1. Refactor exec-server environment modules, add protocol-version negotiation, and migrate internal/API identity handling to qualified IDs with legacy aliases.
2. Add the provider-domain/store crate, state migration, PAT encryption, static provider synthesis, and provider CRUD APIs.
3. Add generic environment lifecycle APIs, adapter orchestration, notifications, reconciliation, connector integration, and fake-provider integration coverage.
4. Add the Ona Connect client, local annotation filtering and pagination, event watch, status mapping, and direct WebSocket connector.

Regenerate app-server schemas and update the app-server API README whenever each public surface lands. Refresh Bazel locks if dependencies change.

## Test plan

- Store tests: migration, CRUD, case-insensitive uniqueness, immutable fields, cursor pagination, encrypted PAT round trips, absence of plaintext in SQLite, and keyring failure behavior.
- Protocol tests: initialization with and without `protocolVersion`, minimum-version rejection, explicit unsupported-operation errors, qualified IDs, slash-containing provider-native IDs, and bare static aliases.
- App-server integration tests: all provider/environment methods, redacted auth, default URL persistence, static-provider restrictions, async create/delete responses, notifications, pagination, and error mapping.
- Deletion tests: non-empty rejection, fail-closed provider failure, forced complete/partial/unknown cleanup, known failed IDs, and guaranteed definition-removal attempts.
- Ona mock-server tests: request mapping, PAT headers, annotation creation, multi-page post-filtering, read/list/delete, phase/error mapping, stale-event rejection, initial reconciliation, watch reconnect, missed-event recovery, and delete-event handling.
- Execution tests: running Ona environment registration, fresh WebSocket metadata on reconnect, missing connection URL behavior, PAT replacement invalidating old connectors, and provider deletion removing execution entries.
- Compatibility tests: built-in local becomes `static/local`; TOML, environment-variable, and `environment/add` entries remain available; old thread/turn/rollout IDs continue resolving.
- Run scoped `just test` commands for every affected crate, `just fmt`, scoped `just fix`, schema generation, and Bazel lock refresh where applicable. Because common/protocol code changes, request approval before the final complete `just test` workspace run.

## Assumptions

- Provider definitions have the same local ownership scope as the current state database under `CODEX_HOME`.
- Modal, Daytona, OAuth, resource-class discovery, provider health, provider-specific create options, PAT clearing, and encryption-key rotation are outside this iteration.
- Ona supplies an authenticated exec-server WebSocket URL in environment metadata; adding or changing the corresponding Ona upstream field is outside this repository.
- Static registration lifecycle is not provisioner lifecycle: static entries are listed as registered/running, while connection failures surface through execution errors.
