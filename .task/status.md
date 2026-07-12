# Environment Providers Implementation Status

Last updated: 2026-07-12

## Overall status

Implementation is in progress on branch `ts/env-provider`. The first two foundational milestones
from `.task/plan.md` have been completed and committed. The qualified environment identity
milestone is implemented and verified locally, but has not yet been committed.

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

## Current milestone: qualified environment identity

Implementation is present in the worktree and awaiting its milestone commit.

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

Verification completed so far:

- Exec-server environment tests: 55/55 passed after the latest refactor.
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

## Next work

1. Review and commit the qualified environment identity milestone.
2. Begin delivery stage 2: introduce the environment-provider domain/store crate and local
   provider-definition persistence.
3. Add encrypted PAT storage support, static provider synthesis, and provider CRUD APIs in
   separately committed logical milestones.

This file will be updated after each subsequent milestone is committed.
