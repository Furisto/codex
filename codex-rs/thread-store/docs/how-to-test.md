# Remote Thread Store Testing

This README describes how to test the AWS object-log `ThreadStore` and the real
app-server/TUI flow that uses it. It is intended for follow-up investigation on
a machine with enough CPU to build the Rust workspace quickly.

The feature under test is remote thread persistence and cold resume using:

- DynamoDB as the thin ordered/indexed metadata store.
- S3 as the commit payload and snapshot object store.
- persisted thread config snapshots for cold resume.
- app-server `thread/start`, `turn/start`, thread history load, and resume paths.

Do not put AWS credentials in `config.toml`, README files, shell history that is
shared, or committed files. Export them only into the process environment that
runs the commands.

## 1. Check Out And Build

From the repo root:

```bash
cd /workspaces/codex/codex-rs
git status --short
```

If another Rust build is running, wait for it to finish before starting these
commands. The repo uses shared Cargo locks and parallel builds will slow each
other down.

Format and focused tests should be run with `just`, not raw `cargo test`:

```bash
just fmt
just test -p codex-thread-store
just test -p codex-app-server
```

If only the remote store changed and app-server was not touched, start with:

```bash
just test -p codex-thread-store aws_object_log_persists_history_and_config_in_localstack
```

## 2. LocalStack Integration Test

The LocalStack test is the fastest way to verify the AWS object-log store
without touching real AWS resources. It creates a temporary DynamoDB table and
S3 bucket inside LocalStack.

Prerequisites:

- Docker is running.
- LocalStack image pulls are allowed.
- `just` is available.

Run:

```bash
cd /workspaces/codex/codex-rs
just test -p codex-thread-store aws_object_log_persists_history_and_config_in_localstack
```

This test proves:

- a thread can be created with a config snapshot.
- append commits are written and read back.
- idempotency keys replay safely.
- conflicting idempotency payloads are rejected.
- stale expected sequence numbers are rejected.
- list/archive paths use the DynamoDB indexes.

It does not prove:

- real AWS IAM permissions.
- app-server wiring.
- TUI behavior.
- cold resume through the public app-server API.

## 3. Real AWS Store Setup

The AWS object-log store expects one DynamoDB table and one S3 bucket.

DynamoDB table shape:

- table name: choose a test-specific name, for example `codex-threads-smoke`.
- billing mode: on-demand/pay-per-request.
- partition key: `pk` string.
- sort key: `sk` string.
- GSI `gsi1`: partition key `gsi1pk` string, sort key `gsi1sk` string.
- GSI `gsi2`: partition key `gsi2pk` string, sort key `gsi2sk` string.

S3 bucket:

- use a dedicated test bucket.
- same region as the DynamoDB table.
- optional KMS key if testing encrypted payload objects.

Example AWS CLI setup:

```bash
export AWS_REGION=eu-central-1
export TABLE_NAME=codex-threads-smoke
export BUCKET_NAME=codex-thread-payloads-smoke

aws dynamodb create-table \
  --region "$AWS_REGION" \
  --table-name "$TABLE_NAME" \
  --billing-mode PAY_PER_REQUEST \
  --attribute-definitions \
    AttributeName=pk,AttributeType=S \
    AttributeName=sk,AttributeType=S \
    AttributeName=gsi1pk,AttributeType=S \
    AttributeName=gsi1sk,AttributeType=S \
    AttributeName=gsi2pk,AttributeType=S \
    AttributeName=gsi2sk,AttributeType=S \
  --key-schema \
    AttributeName=pk,KeyType=HASH \
    AttributeName=sk,KeyType=RANGE \
  --global-secondary-indexes '[
    {
      "IndexName": "gsi1",
      "KeySchema": [
        {"AttributeName": "gsi1pk", "KeyType": "HASH"},
        {"AttributeName": "gsi1sk", "KeyType": "RANGE"}
      ],
      "Projection": {"ProjectionType": "ALL"}
    },
    {
      "IndexName": "gsi2",
      "KeySchema": [
        {"AttributeName": "gsi2pk", "KeyType": "HASH"},
        {"AttributeName": "gsi2sk", "KeyType": "RANGE"}
      ],
      "Projection": {"ProjectionType": "ALL"}
    }
  ]'

aws dynamodb wait table-exists --region "$AWS_REGION" --table-name "$TABLE_NAME"

aws s3api create-bucket \
  --region "$AWS_REGION" \
  --bucket "$BUCKET_NAME" \
  --create-bucket-configuration LocationConstraint="$AWS_REGION"
```

## 4. Configure A Smoke `CODEX_HOME`

Use a dedicated `CODEX_HOME` so local state from normal development does not
hide remote-store issues.

```bash
export CODEX_HOME=/workspaces/codex/.codex-remote-smoke
mkdir -p "$CODEX_HOME"
```

Create `$CODEX_HOME/config.toml`:

```toml
experimental_thread_store = {
  type = "aws_object_log",
  namespace = "smoke",
  table_name = "codex-threads-smoke",
  bucket_name = "codex-thread-payloads-smoke",
  key_prefix = "thread-payloads",
  aws_region = "eu-central-1"
}
```

For a one-line config:

```toml
experimental_thread_store = { type = "aws_object_log", namespace = "smoke", table_name = "codex-threads-smoke", bucket_name = "codex-thread-payloads-smoke", key_prefix = "thread-payloads", aws_region = "eu-central-1" }
```

Authenticate this `CODEX_HOME` if the app-server path requires ChatGPT auth:

```bash
CODEX_HOME="$CODEX_HOME" cargo run -p codex-cli --bin codex -- login
```

If the app-server logs repeat `remote control requires ChatGPT authentication`,
either authenticate this `CODEX_HOME` or disable internal remote control for the
smoke run:

```bash
export CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED=1
```

## 5. Start The Real App Server

There are two useful ways to start the server.

Option A: use `codex-app-server-test-client serve`. This is the most convenient
for repeatable smoke tests because the same test client can create, list, and
resume threads.

```bash
cd /workspaces/codex/codex-rs

export CODEX_HOME=/workspaces/codex/.codex-remote-smoke
export AWS_REGION=eu-central-1
export AWS_ACCESS_KEY_ID=...
export AWS_SECRET_ACCESS_KEY=...
export AWS_SESSION_TOKEN=...
export CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED=1

cargo build -p codex-cli --bin codex

RUST_LOG='warn,codex_app_server_transport::transport::websocket=info,codex_app_server=info,codex_app_server::app_server_tracing=warn,codex_core=warn,codex_core_skills=warn' \
  cargo run -p codex-app-server-test-client -- \
    --codex-bin ./target/debug/codex \
    serve --listen ws://127.0.0.1:4567 --kill
```

The `serve` command starts the app-server in the background and writes the
app-server log to `/tmp/codex-app-server-test-client/app-server.log`.

Option B: start `codex app-server` directly.

```bash
cd /workspaces/codex/codex-rs

export CODEX_HOME=/workspaces/codex/.codex-remote-smoke
export AWS_REGION=eu-central-1
export AWS_ACCESS_KEY_ID=...
export AWS_SECRET_ACCESS_KEY=...
export AWS_SESSION_TOKEN=...
export CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED=1

RUST_LOG='warn,codex_app_server_transport::transport::websocket=info,codex_app_server=info,codex_app_server::app_server_tracing=warn,codex_core=warn,codex_core_skills=warn' \
  cargo run -p codex-cli --bin codex -- app-server --listen ws://127.0.0.1:4567 \
  2>&1 | tee /tmp/codex_app_server_remote_store.log
```

The log should show the websocket listener and then request activity when a
client connects. If `tee` appears to miss logs, confirm stderr is redirected
with `2>&1`; tracing output is usually written to stderr.

## 6. Connect A Client And Create A Conversation

Use the test client first, because it exercises the public app-server JSON-RPC
API without manual UI steps.

In a second terminal:

```bash
cd /workspaces/codex/codex-rs

export CODEX_HOME=/workspaces/codex/.codex-remote-smoke
export AWS_REGION=eu-central-1
export AWS_ACCESS_KEY_ID=...
export AWS_SECRET_ACCESS_KEY=...
export AWS_SESSION_TOKEN=...

cargo run -p codex-app-server-test-client -- \
  --url ws://127.0.0.1:4567 \
  send-message-v2 "Reply with exactly: remote thread smoke ok"
```

Then list threads and save the new thread id:

```bash
cargo run -p codex-app-server-test-client -- \
  --url ws://127.0.0.1:4567 \
  thread-list --limit 5

export THREAD_ID=<copy-thread-id>
```

If you specifically need to validate a UI client, connect that UI to
`ws://127.0.0.1:4567`, create a conversation, and send the same message:

```text
Reply with exactly: remote thread smoke ok
```

Expected user-visible result:

- the conversation starts.
- the turn runs.
- the assistant replies.
- no persistence errors appear in the app-server log.

Expected remote durable state:

- DynamoDB has one `HEAD#...` record for the thread.
- DynamoDB has one or more `COMMIT#...` records.
- S3 has commit payload objects under
  `thread-payloads/namespaces/<namespace>/threads/<thread_id>/commits/`.
- the thread head includes a config snapshot.

## 7. Verify Remote Objects

Set the thread id from the app/server response or logs:

```bash
export THREAD_ID=...
export NAMESPACE=smoke
export TABLE_NAME=codex-threads-smoke
export BUCKET_NAME=codex-thread-payloads-smoke
export KEY_PREFIX=thread-payloads
export AWS_REGION=eu-central-1
```

Read DynamoDB records:

```bash
aws dynamodb query \
  --region "$AWS_REGION" \
  --table-name "$TABLE_NAME" \
  --key-condition-expression 'pk = :pk' \
  --expression-attribute-values "{\":pk\":{\"S\":\"THREAD#$NAMESPACE#$THREAD_ID\"}}" \
  | jq .
```

List S3 payload objects:

```bash
aws s3 ls \
  "s3://$BUCKET_NAME/$KEY_PREFIX/namespaces/$NAMESPACE/threads/$THREAD_ID/" \
  --region "$AWS_REGION" \
  --recursive
```

Download and inspect a commit payload:

```bash
export COMMIT_KEY='thread-payloads/namespaces/smoke/threads/<thread_id>/commits/<commit>.json'

aws s3 cp "s3://$BUCKET_NAME/$COMMIT_KEY" /tmp/codex_commit_payload.json \
  --region "$AWS_REGION"

jq . /tmp/codex_commit_payload.json
```

Important shape checks:

- top-level `schema` is `codex.thread.commit-payload.v1`.
- `items[].seq` is ordered and contiguous across commits.
- token-count rollout items use:

```json
{
  "type": "event_msg",
  "payload": {
    "type": "token_count",
    "rate_limits": {
      "primary": {
        "used_percent": 1.0,
        "window_minutes": 300,
        "resets_at": 1783471121
      }
    }
  }
}
```

If a reader expects `primary` or `secondary` to be a bare number, it is running
against an older schema than the payload writer.

## 8. Cold Resume Test

Stop the app-server only after the turn is idle. For this milestone, do not
kill it while a model turn or tool call is active.

Restart the app-server with the same `CODEX_HOME`, AWS credentials, and
`experimental_thread_store` config.

Resume the known thread id from the test client:

```bash
cd /workspaces/codex/codex-rs

cargo run -p codex-app-server-test-client -- \
  --url ws://127.0.0.1:4567 \
  thread-resume "$THREAD_ID"
```

Then send a message into the resumed thread:

```bash
cargo run -p codex-app-server-test-client -- \
  --url ws://127.0.0.1:4567 \
  resume-message-v2 "$THREAD_ID" "What was my previous exact smoke-test phrase?"
```

Expected result:

- the app-server can find the thread by remote `thread_id`.
- the session is rebuilt from remote thread metadata, config snapshot, and
  committed history.
- the assistant can answer based on the previous committed history.
- new rollout items append after the previous head sequence.
- no local rollout path is required.

## 9. Decode Error Investigation

Symptom:

```text
failed to record rollout items: thread-store internal error:
failed to decode commit payload ... invalid type: map, expected f64
```

This usually means append is trying to read existing commits before writing a
new commit, and at least one existing commit payload cannot be decoded by the
currently running binary.

Steps:

1. Copy the exact S3 key from the log.
2. Download the payload.
3. Inspect the field around the reported column.
4. Compare the JSON shape with the current Rust types.
5. Decide whether this is stale test data, version skew, or a missing backward
   compatible deserializer.

Commands:

```bash
export COMMIT_KEY='thread-payloads/namespaces/smoke/threads/<thread_id>/commits/<commit>.json'

aws s3 cp "s3://$BUCKET_NAME/$COMMIT_KEY" /tmp/failing_commit.json \
  --region "$AWS_REGION"

wc -c /tmp/failing_commit.json
jq . /tmp/failing_commit.json | sed -n '1,220p'
perl -0777 -ne 'print substr($_, 840, 140), "\n"' /tmp/failing_commit.json
```

For better serde diagnostics, add a temporary test or small local reproducer
using `serde_path_to_error` against the downloaded JSON. Do not commit temporary
credentials or downloaded payloads.

If the payload has object-shaped rate-limit windows like:

```json
"primary": {
  "used_percent": 1.0,
  "window_minutes": 300,
  "resets_at": 1783471121
}
```

but the running process reports `expected f64`, verify that the app-server was
rebuilt and restarted from the current checkout. That error is consistent with
a reader that still expects an older bare-float rate-limit window shape.

## 10. Useful Focused Tests To Add Or Run

If changing decode compatibility, add a fixture-style test that deserializes the
exact persisted rollout item shape. Good candidates:

- `codex-rs/protocol/src/protocol.rs` tests for `RolloutItem` and
  `EventMsg::TokenCount` JSON compatibility.
- `codex-rs/thread-store/tests/aws_object_log_localstack.rs` for full
  commit-payload round trips.
- `codex-rs/app-server/tests/suite/v2/remote_thread_store.rs` for app-server
  public API resume behavior.

Run focused tests first:

```bash
cd /workspaces/codex/codex-rs
just test -p codex-protocol <test_name>
just test -p codex-thread-store aws_object_log_persists_history_and_config_in_localstack
just test -p codex-app-server <remote_thread_store_or_resume_test_name>
```

After Rust code changes:

```bash
just fmt
just fix -p codex-thread-store
just test -p codex-thread-store
```

If common protocol or app-server behavior changed, also run the relevant crate
tests. Ask before running the full workspace suite.

## 11. Cleanup

For a disposable smoke namespace, delete the AWS resources when done:

```bash
aws s3 rm "s3://$BUCKET_NAME/$KEY_PREFIX/namespaces/$NAMESPACE/" \
  --region "$AWS_REGION" \
  --recursive

aws dynamodb delete-table \
  --region "$AWS_REGION" \
  --table-name "$TABLE_NAME"
```

If you are sharing a bucket/table with other smoke tests, delete only the
namespace/thread prefix you created.

## 12. Success Criteria

The feature is passing the milestone when all of these are true:

- LocalStack object-log integration test passes.
- real app-server can create a thread with `experimental_thread_store`.
- a real turn appends commit payloads without persistence errors.
- the app-server can be stopped while idle and restarted.
- a known remote `thread_id` resumes without local rollout files.
- resumed session uses the stored config snapshot, not current defaults.
- subsequent messages append after the existing remote head.
- S3 commit payloads and DynamoDB head/commit records are inspectable and
  consistent.
