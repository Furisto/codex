# Environment Providers Decision Log

This document records the environment-provider design discussion to date. It distinguishes
confirmed decisions from open questions and from observations about prior implementations. It is
not an implementation plan.

## Feature intent

Codex currently supports statically registered execution environments through an implementation
named `TomlEnvironmentProvider`. The feature will generalize environment support to multiple
providers, including dynamic providers such as Ona, Modal, and Daytona.

Users must be able to:

- Create, update, list, and delete environment provider definitions.
- Provision an environment through a selected provider.
- Read, list, and delete provisioned environments.
- Continue using the existing `environment/add` API.

A dynamic provider is responsible for creating, querying, watching, and deleting its own
environments.

## Confirmed architecture

The following architectural interpretation was explicitly confirmed:

- A storage-neutral, store-like interface persists environment provider definitions. It follows
  the same pattern as `ThreadStore`, allowing local and future remote storage backends.
- One configured vendor adapter exists for each provider definition and implements environment
  create, read, list, delete, and watch behavior.
- The external provider is authoritative for its environments. Codex does not durably persist
  provider-owned environment state.
- `EnvironmentManager` maintains only the live, in-memory projection needed for Codex to connect
  tools to available execution environments.
- Static environments participate through a renamed `StaticEnvironmentProvider`.
- Environment-related exec-server code moves into a dedicated module as part of this work,
  alongside renaming `TomlEnvironmentProvider` to `StaticEnvironmentProvider`.

## Confirmed decisions

### Provider-definition storage and ownership

**Question:** What scope owns provider registrations, and where should they be persisted?

**Answer:** Follow the repository's existing storage patterns. In particular, use an interface
similar to `ThreadStore`, which abstracts over storage backends. `ThreadStore` historically used a
local backend and now also supports a remote backend such as AWS Object Log.

**Result:** Provider definitions are persisted behind a store-like abstraction rather than being
tied directly to TOML, SQLite, or a particular remote backend.

The exact tenancy scope of the stored records (installation, user, workspace, or organization) has
not yet been explicitly specified.

### Static provider terminology

**Question:** Should `TomlEnvironmentProvider` remain a special implementation, and how does it
relate to the new provider model?

**Answer:** Rename it to `StaticEnvironmentProvider`. TOML is only a serialization format; the
meaningful distinction is that its environments are statically registered rather than dynamically
provisioned.

**Question:** Should the static provider be exposed through provider CRUD?

**Answer:** Yes.

The built-in static provider has special CRUD semantics:

- The static provider ID is the literal string `static`.
- There is exactly one static provider.
- Users cannot create additional static providers.
- The static provider cannot be renamed.
- The static provider cannot be deleted.

### Existing `environment/add` compatibility

**Question:** What happens to the existing `environment/add` API?

**Answer:** Continue supporting it. It always adds environments to the single built-in `static`
provider.

Environments added through `environment/add` remain process-local and ephemeral. They are not
persisted in the provider-definition store or another durable store.

### Provider identity

**Question:** Is a provider name its stable identifier?

**Answer:** No. A provider has an immutable opaque ID and a unique, mutable name.

Provider names are unique case-insensitively. For example, `Ona Production` and
`ona production` conflict.

### Provider definition

The dynamic provider definition consists of:

- Immutable opaque provider ID.
- Unique, mutable name.
- Provider kind, such as Ona, Modal, or Daytona.
- URL.
- Authentication.

Control-plane URLs may have provider-specific defaults. Dynamic provider creation therefore does
not universally require the client to supply a URL; omission selects the default for that provider
kind when one exists. The exact persistence and response semantics for a resolved default URL are
still to be confirmed.

### Provider updates

**Question:** What can a user update?

**Answer:** Only the provider name and authentication token. The URL cannot be updated. The
provider kind and ID are also treated as immutable.

Omitted authentication on update preserves the current PAT. There is no operation for clearing the
credential in the first iteration.

### Authentication

**Question:** Should authentication be modeled as a tagged union now even though only PAT is
supported initially?

**Answer:** Yes. This preserves an extension point for OAuth.

**Question:** What should provider responses reveal for PAT authentication?

**Answer:** Only:

```json
{ "type": "pat" }
```

The PAT itself must not be returned in API responses.

### Provider creation validation

**Question:** Should provider creation validate credentials or connectivity?

**Answer:** No. Creating a provider definition should not contact the provider to validate its URL,
credentials, or availability.

Codex does perform syntactic validation for dynamic provider URLs: the URL must be an absolute
HTTP(S) URL, and trailing slashes are normalized. This validation makes no network request.

### Provider deletion

**Question:** What happens when deleting a provider that still has environments?

**Answer:** Reject the deletion. All environments must be deleted before the provider can be
deleted.

Because Codex does not persist provider-owned environment state, this check is performed by
querying the provider. If that query cannot be completed, for example because the token is invalid
or the provider is unreachable, deletion fails closed by default.

Provider deletion also supports an explicit force option with these semantics:

- Force deletion first attempts to delete the provider's environments.
- Failure to list or delete some or all environments does not prevent deletion of the provider
  definition.
- Force deletion may therefore orphan provider-owned environments.
- The delete response reports cleanup status as `complete`, `partial`, or `unknown` and includes
  environment IDs whose deletion failed when those IDs are known.

### Environment API scope

**Question:** Which environment operations are in scope?

**Answer:** The following operations are in scope:

- Create.
- Read.
- List.
- Delete.

Environment update is not currently in scope.

### Environment listing scope

**Question:** Must `environment/list` identify a provider, or can it aggregate environments across
all providers?

**Answer:** `environment/list` requires a provider ID.

**Result:** Cross-provider pagination, ordering, and partial-failure aggregation are out of scope.

Both provider-definition listing and provider-specific environment listing use cursor pagination.
Environment pagination is compatible with provider-owned state: each provider adapter owns and
translates its opaque cursor. Ona can map the Codex cursor and limit directly to its pagination
token and page size. Static environments can use a stateless keyset cursor over a stable ordering.

### Provider-owned environment discovery

**Question:** How does a provider distinguish environments created through Codex from unrelated
provider resources?

**Answer:** For now, assume each provider can tag resources created through Codex and will only
retrieve Codex-related resources when queried.

For Ona specifically, the initial implementation cannot filter annotations in the upstream list
request. It must list all visible environments and filter the results locally to retain only
Codex-annotated environments. Pagination and reconciliation must account for filtering after
fetching upstream pages.

### Environment persistence

**Question:** Should Codex store provider-owned environment state or use local state to enforce
provider deletion constraints?

**Answer:** No. Codex does not want to durably store state about a provider's environments. Reads,
lists, and the provider-deletion check query the authoritative provider.

### Asynchronous creation and identity

**Question:** Is environment creation synchronous?

**Answer:** No. Environment creation is asynchronous.

**Question:** What does `environment/create` return?

**Answer:** It returns only an environment reference consisting of:

```text
EnvironmentRef {
    providerId,
    environmentId,
}
```

`environmentId` is the identifier assigned by the underlying provider. Status is subsequently
available through `environment/read` and notifications.

**Discussion outcome:** A separate operation ID is not considered necessary. The environment is
itself the asynchronous resource.

Environment deletion is also asynchronous. `environment/delete` returns an empty success response
after the provider accepts the deletion. The eventual delete notification signals completion.

After deletion completes, `environment/read` returns not-found and `environment/list` omits the
resource. The `deleted` phase may be observable only while the provider retains a transitional or
tombstone representation; Codex does not retain deleted environment records.

### Environment watches

**Question:** How is asynchronous state advanced?

**Answer:** The provider has a single watch that receives provider notifications and updates the
environment state.

There is one watch per configured provider definition. Each provider implementation must:

- Perform an initial list before watching.
- Reconnect its watch automatically.
- Perform a full reconciliation list after reconnecting so missed events are recovered.
- Hide vendor-specific watch and reconciliation mechanics behind the provider abstraction.

### Resource classes

**Question:** How does a client specify environment size?

**Answer:** The create request includes a resource class that names whatever size or class the
underlying provider supports.

**Question:** Must clients be able to discover valid resource classes in this work?

**Answer:** No. Assume the user knows the provider's valid resource classes. Resource-class
discovery is out of scope for now.

The current interpretation is that `resourceClass` is an opaque provider-native string.

### Provider-specific creation options

**Question:** Should creation include provider-specific options now?

**Answer:** No demonstrated need exists in the first iteration. A typed common request plus tagged
provider-specific options may be appropriate later, but provider-specific options should not be
introduced preemptively.

### Source definition

**Question:** How is the source repository represented?

**Answer:** As a structured source object containing a repository URL and a ref.

The source ref is required.

Codex treats the ref as an opaque, provider-interpreted Git ref. It does not classify or resolve it
as a branch, tag, or commit.

For private repositories, Codex does not supply separate source credentials. Ona authenticates
repository access through its own account and SCM integrations.

### Idempotency

**Question:** Does Codex require a provider-independent idempotency key for environment creation?

**Answer:** No. A provider implementation should use an idempotency key if its provider supports
one. Otherwise, duplicate environments caused by retries are accepted.

### Provider-specific lifecycle options

**Question:** Should Codex expose provider-specific provisioning fields in the first iteration?

**Answer:** No, unless implementation experience demonstrates a concrete need. The common source
and resource-class fields are sufficient for now.

### Minimal environment record

**Question:** What common environment record should providers return?

**Answer:** The minimal proposal is sufficient:

```text
Environment {
    ref
    source {
        repositoryUrl
        ref
    }
    resourceClass
    status {
        phase
        error
    }
}
```

No name, creation timestamp, or provider-specific fields are currently required.

### Environment notifications

**Question:** What should environment lifecycle notifications contain?

**Answer:** Use three typed notifications:

- `environment/created` contains the complete minimal environment record.
- `environment/updated` contains the complete minimal environment record.
- `environment/deleted` contains `{ environmentId }` with the qualified environment ID.

### Environment connection

A ready environment carries a private, optional `EnvironmentConnector` in the internal domain
model. The connector:

- Is never serialized through the app-server API.
- Produces fresh provider-specific connection material on demand.
- Encapsulates short-lived credentials, access-token refresh, and mechanisms such as Noise
  rendezvous.

`EnvironmentManager` owns connection startup and reconnection. Providers do not return an already
open stream and do not construct the concrete runtime environment directly.

### Qualified environment identity

**Question:** How should environments be identified in thread, turn, and other environment-aware
APIs?

**Answer:** Use the canonical qualified string form:

```text
providerId/environmentId
```

Use this representation consistently across APIs. Existing unqualified IDs are accepted as legacy
aliases for `static/<id>`.

The built-in local environment belongs to the static provider and is identified as `static/local`.
TOML environments, environment-variable environments, and environments added through
`environment/add` also belong to `static`.

### Provider API representation

**Question:** Does the provider-definition response require a kind-tagged union to account for the
static provider?

**Answer:** No. Use one provider response shape. `url` and `authentication` are nullable for the
built-in provider with `kind: "static"` and required for dynamic providers. Provider creation
rejects `kind: "static"`.

The built-in provider has the fixed display name `Static`. It is always included in
`environmentProvider/list`, even when it currently has no environments.

Provider create and update operations return the complete redacted provider record, not only its
ID.

### App-server v2 methods

The accepted v2 method names are:

- `environmentProvider/create`
- `environmentProvider/update`
- `environmentProvider/list`
- `environmentProvider/delete`
- `environment/create`
- `environment/read`
- `environment/list`
- `environment/delete`

### Force-delete response

Forced provider deletion returns:

```text
{
    cleanup: {
        status: "complete" | "partial" | "unknown",
        failedEnvironmentIds: string[]
    }
}
```

- `complete` means all discovered environment deletion requests were accepted.
- `partial` means enumeration succeeded but at least one environment deletion request failed.
- `unknown` means environments could not be enumerated completely.
- The provider definition has been deleted for all three statuses.

### Common environment phases

The common lifecycle phases follow Ona:

- `unknown`
- `creating`
- `starting`
- `running`
- `updating`
- `stopping`
- `stopped`
- `deleting`
- `deleted`

`status.error` is separate from the phase and nullable.

### First dynamic provider

**Question:** Which concrete dynamic provider is implemented first?

**Answer:** Ona. Modal and Daytona are future implementations rather than part of the first
concrete provider implementation.

### Initial provider-definition store

The first provider-definition store is a local implementation backed by Codex's state database.
The storage-neutral boundary supports future remote implementations, but no remote implementation
is required initially.

PAT ciphertext is stored with the provider configuration. The encryption key is stored separately
in the configured keyring or secret manager; plaintext PATs are never stored with provider
configuration.

### Provider health

Provider health is not exposed in the first API. Provider watches retry in the background, while
explicit environment operations return provider errors. Provider creation still does not validate
connectivity or credentials.

Updating a provider PAT restarts that provider's watch and refreshes its environment connectors.
Renaming a provider does not restart its watch.

## Terminology clarification

During discussion, "registry" was used as shorthand for the persisted collection of provider
definitions. No separate registry service was proposed. The preferred model is the confirmed
store-like provider-definition persistence boundary.

## Repository observations

### Existing Codex environment implementation

- The current `EnvironmentProvider` trait only returns a startup snapshot of environments.
- `TomlEnvironmentProvider` reads `environments.toml` and produces that snapshot.
- `EnvironmentManager` owns concrete local and remote execution environments in memory.
- The experimental `environment/add` API directly upserts an environment into
  `EnvironmentManager`; it is not durably persisted.
- Existing thread and turn selection surfaces identify environments by a single string ID.

Environment identity has since been resolved as the canonical qualified string
`providerId/environmentId`, with legacy bare IDs treated as aliases for `static/<id>`.

### Ona API inspiration

The Ona implementation in `/workspaces/gitpod-next` was examined, especially:

- `api/def/gitpod/v1/environment.proto`
- `api/def/gitpod/v1/event.proto`

Relevant observations:

- Environment creation returns an environment resource immediately while provisioning continues
  asynchronously.
- Ona exposes a high-level lifecycle phase including creating, starting, running, updating,
  stopping, stopped, deleting, and deleted.
- Failure messages are orthogonal to the lifecycle phase rather than represented solely as a
  `failed` phase.
- Ona exposes a monotonic `status_version` for ordering status updates.
- Organization-scoped event watches emit a resource operation, resource type, and resource ID, not
  the full updated resource.
- A consumer therefore reads the environment after create/update/status events and removes it from
  its projection after delete events.
- Ona supports metadata annotations that can be used to identify Codex-created resources.

The generic lifecycle phases now follow Ona, with failures exposed separately through nullable
`status.error`.

### Previous cloud-tasks attempt

The following crates were examined:

- `codex-rs/cloud-tasks`
- `codex-rs/cloud-tasks-client`
- `codex-rs/cloud-tasks-mock-client`

Useful precedents:

- `CloudBackend` exposes normalized domain operations behind a trait.
- `HttpClient` and `MockClient` implement the same abstraction.
- Asynchronous task creation returns only `CreatedTask { id }`, with later reads exposing status.
- HTTP response mapping remains inside the backend implementation.

Problems to avoid in the new environment design:

- Environment discovery bypasses `CloudBackend` and performs backend-specific HTTP requests in the
  application/UI crate.
- Base URLs, authentication, route conventions, and response decoding leak into application code.
- Environment identity is a bare string and assumes a single backend.
- The application uses an ad hoc, lossy `EnvironmentRow` instead of a stable domain model.
- There is no environment watch; state depends on explicit refreshes.
- Several backend methods use opaque positional arguments rather than structured parameter types.

## Open questions

Initial provider definitions otherwise follow the scope of the local Codex state database under
`CODEX_HOME`. A future remote store defines its corresponding ownership and tenancy scope.

No capability-query API is introduced for environments or providers. Codex determines supported
exec-server operations through an integer exec-server protocol version:

- The version is returned during exec-server initialization.
- Missing version information from older exec servers is interpreted as legacy protocol version 1.
- Codex owns the mapping from each operation to its minimum supported protocol version.
- The protocol version is independent of provider kind and Codex release version.
- Unsupported operations still return an explicit unsupported-operation error.
- If support becomes non-linear in the future, explicit negotiated capabilities may be added
  alongside the protocol version then.

PAT encryption key lifecycle details, such as rotation and recovery, may still require follow-up
when the encrypted local provider store is designed.
