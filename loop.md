# Codex CLI Startup Performance Loop

## Objective

Measure and explain the startup-time difference between `codex resume --last --remote ws://127.0.0.1:4222` using the local thread store and the AWS Object Log thread store.

## Fixed Setup

- Working directory: `/workspaces/codex`
- Local `CODEX_HOME`: `/workspaces/codex/codex-smoke-local`
- AWS `CODEX_HOME`: `/workspaces/codex/codex-smoke-aws`
- Server endpoint: `ws://127.0.0.1:4222`
- Logging filter: `warn,codex_tui=info,codex_app_server=info,codex_thread_store::remote::aws_object_log=info`
- AWS Object Log config:

```toml
experimental_thread_store = { type = "aws_object_log", namespace = "smoke", table_name = "codex-threads", bucket_name = "codex-thread-payloads-furisto", key_prefix = "thread-payloads", aws_region = "eu-central-1" }
```

## Iteration 1

### Hypothesis

The AWS Object Log history load is not the main source of the roughly 4 second CLI startup delay. Prior logs showed AWS history resume around 150 ms for a 114-item thread, so the larger delay is likely in TUI bootstrap, app-server startup phases, skill/plugin loading, hooks review, or thread replay/rendering.

### Setup And Commands

Use the isolated homes above. Build and install the current instrumented binary once before running measurements because the worktree contains new timing logs.

### Local Backend Timings

Attempted with `CODEX_HOME=/workspaces/codex/codex-smoke-local` and no `experimental_thread_store`.

The first harness used plain `timeout`, which was invalid for TUI startup: `strace` showed the Codex child process was stopped by `SIGTTOU` while trying to change terminal mode. The correct harness must use `timeout --foreground` so the TUI remains in the foreground process group.

With `timeout --foreground`, the local CLI did start and connected to the app server, but it did not resume an existing conversation. It listed threads and then sent `thread/start`, producing a fresh thread with the placeholder prompt instead of a roughly 100-item resumed thread. This run is therefore not a valid local baseline.

Observed timings from the invalid fresh local run:

- terminal startup probes: 99 ms
- remote app-server connect: 81 ms
- app-server session initialized: 195 ms from TUI start
- `account/read`: 2 ms
- `configRequirements/read`: 605 ms
- `model/list`: 8 ms
- TUI bootstrap total: 617 ms
- bootstrap + hooks prefetch: 617 ms
- hooks review: 0 ms, 832 ms from TUI start
- initial frame scheduled: 634 ms after app construction started
- fresh `thread/start` RPC: 1632 ms

Evidence:

- `perf-logs/tui-local/codex-tui.log`
- `perf-logs/iter-1-local-fg/server.log`
- `perf-logs/iter-1-local-fg/time.txt`

### AWS Object Log Timings

Not run in this iteration. The local comparison run failed the required resumed-thread size check, so continuing to AWS would compare AWS resume against a fresh local thread and produce a misleading delta.

### Resumed Thread Size Check

Failed for the local backend. The isolated local home does not contain a local `sessions/` rollout tree or other local state that can produce a roughly 100-item `resume --last` target. The CLI fell back to `thread/start`, so this is a fresh conversation, not a resume.

### Interpretation

The current local isolated home is not a valid baseline fixture. It only proves fresh-start timing for the local backend. A valid local baseline needs a seeded local rollout/history for the same or comparable roughly 100-item conversation.

The measurement harness also needs `timeout --foreground`; otherwise the TUI is stopped by terminal job control before reaching app-server bootstrap.

### Next Instrumentation Or Code Change

No further code instrumentation should be added until the local baseline is seeded. The next measurement should reuse the same binary and run:

```bash
timeout --foreground 8s /workspaces/codex/codex-rs/target-devcontainer/debug/codex resume --last --remote ws://127.0.0.1:4222
```

with server logs redirected and TUI `log_dir` configured.

### Possible Fixes Or Optimizations

Not yet attributable. The first useful fix is to create a valid local baseline fixture. Once both backends resume a roughly 100-item thread, compare:

- local `thread/resume` and local rollout load time
- AWS `read_thread include_history=true`, snapshot S3 load/decode, commit pointer query, and `thread/resume`
- shared TUI costs such as `configRequirements/read`, hooks prefetch, MCP startup, initial replay/rendering, and thread conversion

## Iteration 2

### Hypothesis

With a real local conversation created by the user, the local baseline should resume a roughly 100-item thread and expose whether the startup delay is specific to AWS storage or a shared TUI/app-server resume path.

### Setup And Commands

User-created local thread:

- `019f4640-a813-7c41-a98b-b3751ab6e916`
- rollout file: `/workspaces/codex/codex-smoke-local/sessions/2026/07/09/rollout-2026-07-09T09-41-10-019f4640-a813-7c41-a98b-b3751ab6e916.jsonl`
- line count: 98

Local command shape:

```bash
CODEX_HOME=/workspaces/codex/codex-smoke-local \
RUST_LOG='warn,codex_tui=info,codex_app_server=info,codex_thread_store::remote::aws_object_log=info' \
/workspaces/codex/codex-rs/target-devcontainer/debug/codex app-server --listen ws://127.0.0.1:4222

CODEX_HOME=/workspaces/codex/codex-smoke-local \
RUST_LOG='warn,codex_tui=info,codex_app_server=info,codex_thread_store::remote::aws_object_log=info' \
script -q -c 'timeout --foreground 12s /workspaces/codex/codex-rs/target-devcontainer/debug/codex resume --last --remote ws://127.0.0.1:4222' perf-logs/iter-3-local-user/cli.typescript
```

AWS command shape was the same with `CODEX_HOME=/workspaces/codex/codex-smoke-aws` and AWS credentials exported in the process environment, not written to disk.

### Local Backend Timings

Valid local resume of the 98-line user-created thread.

- TUI app-server session initialized: 195 ms
- bootstrap: 568 ms
- hooks review: 0 ms, 783 ms total startup elapsed
- initial frame: 1901 ms
- `thread_and_widget_ms`: 1319 ms
- `initial_session_ms`: 12 ms

### AWS Object Log Timings

Valid AWS resume of thread `019f43ce-0a65-70f0-87fc-1eed5f181cb3`.

- Object-log `read_thread include_history=true`: about 150 ms in earlier logs, same thread and 114 items
- TUI app-server session initialized: 218 ms
- bootstrap: 577 ms
- hooks review: 0 ms, 932 ms total startup elapsed
- initial frame: 5178 ms
- `thread_and_widget_ms`: 1398 ms
- `initial_session_ms`: 3202 ms
- TUI warning immediately before first frame: `failed to read thread goal after resume: thread/goal/get failed in TUI`

### Resumed Thread Size Check

Passed for both backends:

- local: 98 local JSONL records
- AWS: `history_item_count=114`

### Observed Performance Difference

AWS first frame was about 3.3 seconds slower than local in this run: 5178 ms versus 1901 ms. The difference did not appear in object-log history loading; it appeared almost entirely in `initial_session_ms`.

### Interpretation

The measurement bucket was too coarse. `initial_session_ms` includes initial session enqueue and the optional startup paused-goal lookup. The AWS log showed a failed goal lookup on the startup path, but the existing logs did not split enqueue from `thread/goal/get`, so the next iteration needs more focused TUI-side timing.

### Next Instrumentation Or Code Change

Add focused TUI logs around:

- `thread/resume` RPC and conversion in `AppServerSession::resume_thread`
- `enqueue_primary_thread_session`
- `maybe_prompt_resume_paused_goal_after_resume`

### Possible Fixes Or Optimizations

Do not block the first TUI frame on optional goal lookup after resume. If goal lookup must happen, run it after the first frame or skip it for threads that cannot support goals.

## Iteration 3

### Hypothesis

AWS startup is slower because the TUI waits for a post-resume goal lookup that cannot succeed for the AWS Object Log thread, not because S3/DynamoDB history loading is slow.

### Setup And Commands

Added focused timing logs in:

- `codex-rs/tui/src/app_server_session.rs`
- `codex-rs/tui/src/app.rs`
- `codex-rs/tui/src/app/thread_goal_actions.rs`

Then ran:

```bash
cd /workspaces/codex/codex-rs
just fmt
cargo build -p codex-cli --bin codex
```

Measurement harness remained the same as Iteration 2. Logs:

- local: `perf-logs/iter-4-local-user/server.log`, `perf-logs/tui-local/codex-tui.log`
- AWS: `perf-logs/iter-4-aws/server.log`, `perf-logs/tui-aws/codex-tui.log`

### Local Backend Timings

Valid local resume of thread `019f4640-a813-7c41-a98b-b3751ab6e916`.

- TUI app-server session initialized: 196 ms
- bootstrap: 801 ms
- `thread/resume` RPC: 1286 ms
- response conversion: 0 ms
- returned turns: 2
- initial session enqueue: 7 ms
- paused-goal check: 4 ms, success
- first frame: 2117 ms
- `thread_and_widget_ms`: 1302 ms
- `initial_session_ms`: 12 ms

### AWS Object Log Timings

Valid AWS resume of thread `019f43ce-0a65-70f0-87fc-1eed5f181cb3`.

- TUI app-server session initialized: 218 ms
- bootstrap: 611 ms
- object-log `read_thread include_history=true`: 163 ms
- S3 snapshot get: 89 ms
- snapshot decode/materialize: 101 ms
- commit pointer query: 46 ms
- history item count: 114
- skipped commit count: 113
- loaded commit count: 0
- `thread/resume` RPC: 1485 ms
- response conversion: 0 ms
- returned turns: 3
- initial session enqueue: 8 ms
- paused-goal check: 3132 ms, failed
- first frame: 5254 ms
- `thread_and_widget_ms`: 1500 ms
- `initial_session_ms`: 3141 ms

### Resumed Thread Size Check

Passed:

- local: 98 records
- AWS: 114 history items

### Observed Performance Difference

AWS first frame was about 3.1 seconds slower than local: 5254 ms versus 2117 ms.

The measured storage-specific part is small:

- AWS object-log history read: 163 ms
- AWS `thread/resume` RPC vs local `thread/resume` RPC: 1485 ms vs 1286 ms, about 199 ms slower

The large delta is the startup paused-goal check:

- local: 4 ms
- AWS: 3132 ms

### Interpretation

The ~4 second perceived startup delay is not primarily DynamoDB/S3. For the measured 114-item AWS thread, object-log history materialization is about 163 ms. The object-log path adds roughly 200 ms to the resume RPC compared with the local baseline.

The main AWS-only cost is that the TUI blocks first-frame rendering on `maybe_prompt_resume_paused_goal_after_resume`. The AWS thread-store resume has no local rollout path and the app-server goal processor treats the thread as not goal-materialized. The server logs:

```text
ephemeral thread does not support goals: 019f43ce-0a65-70f0-87fc-1eed5f181cb3
```

The server-side `thread/goal/get` processor closes in under 2 ms for both local and AWS. In the AWS run, the TUI does not observe the failed result for 3132 ms. So the delay is response delivery/client wait on an optional startup goal check, not goal database work and not S3/DynamoDB history loading.

There is also a correctness mismatch in the API surface: the AWS resume response reports `ephemeral=false`, but the goal subsystem still treats the thread as unsupported for goals because it is not materialized in the local state DB. The TUI therefore cannot know cheaply that the startup paused-goal check is pointless.

### Next Instrumentation Or Code Change

No more measurement instrumentation is needed to explain the user-visible delta. The next code change should be a behavioral fix, not more logging.

### Possible Fixes Or Optimizations

Primary fix:

- Do not await `maybe_prompt_resume_paused_goal_after_resume` before the first frame. Start it after the initial frame/event loop is active, or spawn it as a non-blocking startup task. This lookup only decides whether to show an optional prompt for a paused/blocked/usage-limited goal; it should not gate rendering a resumed conversation.

Better correctness fix:

- Teach the API whether a resumed thread supports goals. For example, return goal capability/materialization metadata in the resume response, or make `thread/goal/get` return `goal: null` for non-goal-materialized threads when called by startup prompt logic. Avoid inferring this from `ephemeral`, because the AWS response currently says `ephemeral=false` while the goal processor rejects it as unsupported.

AWS Object Log optimization:

- In `load_items_for_head`, if `head.snapshot_seq == head.seq`, skip `query_commit_pointers`. The current run has a head snapshot at seq 114, then queries 113 commit pointers only to skip every commit. That saves about 46 ms in this measurement.

Secondary cleanup:

- Suppress or downgrade the startup warning from `emit_resume_goal_snapshot_and_continue` when a thread-store-backed resume has no local goal state DB. This warning is expected for the current AWS Object Log shape and is not useful as an error signal.

## Iteration 4

### Hypothesis

The 3 second AWS paused-goal delay is not in DynamoDB/S3 and not in the app-server `thread/goal/get` processor, but it may still be in response delivery: app-server outgoing enqueue, websocket writer FIFO backlog, remote-client receive, or typed request resolution.

### Setup And Commands

Added request-id keyed timing logs in:

- `codex-rs/app-server/src/outgoing_message.rs`
- `codex-rs/app-server/src/transport.rs`
- `codex-rs/app-server-transport/src/transport/websocket.rs`
- `codex-rs/app-server-client/src/remote.rs`
- `codex-rs/tui/src/app_server_session.rs`

Built only the CLI binary:

```bash
cd /workspaces/codex/codex-rs
just fmt
cargo build -p codex-cli --bin codex
```

Then reran the app-server-backed resume harness with:

```bash
RUST_LOG='warn,codex_tui=info,codex_app_server=info,codex_app_server_transport::transport::websocket=info,codex_app_server_client=info,codex_thread_store::remote::aws_object_log=info'
```

Logs:

- local: `perf-logs/iter-5-local/server.log`, `perf-logs/tui-local/codex-tui.log`
- AWS: `perf-logs/iter-5-aws/server.log`, `perf-logs/tui-aws/codex-tui.log`

### Local Baseline

Thread: `019f4640-a813-7c41-a98b-b3751ab6e916`.

The local `thread/goal/get` request id was `7`.

- TUI request started: `10:08:53.665385`
- remote client write completed: `10:08:53.666285`
- app-server processor closed: `10:08:53.669339` (`time.busy=1.66ms`, `time.idle=671us`)
- app-server routed response to websocket writer: `10:08:53.669183` to `10:08:53.669285`
- websocket write completed: `10:08:53.669837`, `pending_writer_messages=0`
- remote client received response frame: `10:08:53.670199`
- TUI request completed: `10:08:53.670407`
- TUI paused-goal check completed: `5 ms`
- first frame scheduled: `2000 ms`

This confirms the successful local path has no meaningful transport or client wait for `thread/goal/get`.

### AWS Object Log Delivery Measurement

Thread: `019f43ce-0a65-70f0-87fc-1eed5f181cb3`.

The AWS `thread/goal/get` request id was also `7`.

- TUI request started: `10:09:30.087861`
- remote client write completed: `10:09:30.088562`
- app-server processor closed: `10:09:30.090747` (`time.busy=954us`, `time.idle=549us`)
- app-server routed error to websocket writer: `10:09:30.090573` to `10:09:30.090719`
- websocket write completed: `10:09:30.091151`, `pending_writer_messages=0`
- remote client received error frame: `10:09:30.091433`
- TUI request completed: `10:09:33.196322`
- TUI paused-goal check completed: `3108 ms`
- first frame scheduled: `5146 ms`

This rules out:

- DynamoDB/S3 history loading: AWS history read was about `190 ms`, with 114 items.
- app-server goal processing: under `2 ms`.
- app-server outgoing enqueue: `0 ms`.
- websocket writer FIFO backlog: `pending_writer_messages=0`.
- remote client receive-loop backlog: the error frame was received within about `4 ms` of the TUI request start.

The unexplained gap moved to after the remote client received the JSON-RPC error and before `AppServerSession::thread_goal_get` logged completion.

### Next Instrumentation

Split `AppServerSession::thread_goal_get` into:

- raw typed result received
- context-wrapped result completed

Also added a remote `request_typed` raw-result log.

Reran AWS only:

- AWS: `perf-logs/iter-6-aws/server.log`, `perf-logs/tui-aws/codex-tui.log`

### AWS Error Wrapping Measurement

The raw typed error is fast:

- TUI `thread/goal/get` request started: `10:11:34.413406`
- remote client error frame received: `10:11:34.417190`
- remote typed request raw result received: `10:11:34.417333`, `elapsed_ms=3`
- TUI typed result received: `10:11:34.417411`, `elapsed_ms=4`
- TUI context-wrapped request completed: `10:11:37.583189`, `elapsed_ms=3169`

So the 3.1 second startup cost is specifically between:

```rust
let raw_result = self.client.request_typed(...).await;
let result = raw_result.wrap_err("thread/goal/get failed in TUI");
```

The AWS path hits this because the resumed AWS Object Log thread has `has_rollout_path=false`, and the app-server goal processor returns a JSON-RPC error for non-materialized goal state:

```text
ephemeral thread does not support goals: 019f43ce-0a65-70f0-87fc-1eed5f181cb3
```

The local path does not pay this cost because its resumed thread has a rollout path and `thread/goal/get` succeeds, so `wrap_err` does not construct an error report.

### Interpretation

The startup regression is not AWS Object Log storage performance and not websocket response delivery. It is an expected startup probe taking the expensive error-report path before first frame.

The expensive operation is `color_eyre::WrapErr` on the failed `thread/goal/get` result. In this debug build, constructing the contextual report for this expected JSON-RPC server error costs about `3165 ms`. Because AWS Object Log resumed threads currently do not have a local rollout/state DB, this expected failure happens on every AWS resume. Because the TUI awaits `maybe_prompt_resume_paused_goal_after_resume` before scheduling the first frame, that error-report cost is fully visible as startup latency.

### Code-Level Fix

Primary fix:

- Do not treat unsupported startup goal lookup as an exceptional `color_eyre` path before first frame.
- For the startup paused-goal probe, call an API that returns a typed non-exceptional outcome, for example:
  - `Ok(None)` / `GoalLookup::Unsupported` when the thread has no materialized local goal state, or
  - a resume response capability such as `supports_goals=false`, so the TUI skips the probe entirely for AWS Object Log resumed threads.

Pragmatic first patch:

- Add goal capability/materialization metadata to `ThreadResumeResponse` or the returned thread model.
- In `maybe_prompt_resume_paused_goal_after_resume`, skip `thread/goal/get` when the resumed thread cannot support goals.
- Do not infer this from `ephemeral`; AWS currently returns `ephemeral=false` but still has `has_rollout_path=false`.

Rendering fix:

- The paused-goal check is optional UI. It should not gate first-frame rendering even when it is supported. Schedule the first frame first, then run the goal check as a startup task/event.

Error handling cleanup:

- Avoid `wrap_err` for expected control-flow errors on startup probes. If a startup probe can legitimately fail because a backend does not support that capability, convert the result to a small enum or `Option` before returning to the TUI startup path.

Secondary AWS Object Log optimization:

- `load_items_for_head` still queries 113 commit pointers even when the snapshot is at `head.seq=114` and all commits are skipped. Skipping `query_commit_pointers` when `head.snapshot_seq == head.seq` would save about `45-50 ms`, but that is not the source of the multi-second startup delay.

## Iteration 5: Release Build

### Hypothesis

The 3.1 second `wrap_err` cost may be inflated by the debug build. A release build should show the production-sized impact while preserving the same request-delivery timeline.

### Setup And Commands

Built the release CLI binary:

```bash
cd /workspaces/codex/codex-rs
cargo build --release -p codex-cli --bin codex
```

The release binary was written to:

```bash
/workspaces/codex/codex-rs/target-devcontainer/release/codex
```

Then reran the same app-server-backed harness with the release binary:

- local logs: `perf-logs/iter-7-release-local/server.log`, TUI lines appended to `perf-logs/tui-local/codex-tui.log`
- AWS logs: `perf-logs/iter-7-release-aws/server.log`, TUI lines appended to `perf-logs/tui-aws/codex-tui.log`

Note: `CODEX_TUI_LOG_FILE` is ignored by the TUI; it writes to the configured log directory, so the release TUI lines appeared in the existing `tui-local` / `tui-aws` log files.

### Local Release Baseline

Thread: `019f4640-a813-7c41-a98b-b3751ab6e916`.

- bootstrap: `1432 ms`
- `thread/resume` RPC: `445 ms`
- resumed turns: `2`
- `has_rollout_path=true`
- initial session enqueue: `1 ms`
- `thread/goal/get` raw typed result: `1 ms`, success
- `thread/goal/get` completed: `1 ms`
- paused-goal check: `1 ms`
- first frame scheduled: `1883 ms`

### AWS Object Log Release Measurement

Thread: `019f43ce-0a65-70f0-87fc-1eed5f181cb3`.

The resumed AWS thread was valid:

- `history_item_count=114`
- `head_seq=114`
- `has_snapshot=true`
- resumed turns: `3`
- `has_rollout_path=false`

Storage and resume:

- AWS `load_items_for_head`: `140 ms`
- AWS `read_thread include_history=true`: `144 ms`
- `thread/resume` RPC: `522 ms`

Paused-goal check:

- TUI `thread/goal/get` started: `10:46:14.602338`
- remote client error frame received: `10:46:14.603312`
- remote typed request raw result: `10:46:14.603389`, `elapsed_ms=1`
- TUI typed result received: `10:46:14.603423`, `elapsed_ms=1`
- TUI context-wrapped request completed: `10:46:15.462935`, `elapsed_ms=860`
- paused-goal check completed: `860 ms`
- first frame scheduled: `2034 ms`

Server-side `thread/goal/get` remained fast:

- processor closed with `time.busy=203us`, `time.idle=186us`
- outgoing enqueue: `0 ms`
- websocket writer queue backlog: `pending_writer_messages=0`
- websocket write: `0 ms`

### Interpretation

The release build confirms the same root cause as the debug build, but the cost is smaller:

- debug AWS `wrap_err` / contextual error construction: about `3165 ms`
- release AWS `wrap_err` / contextual error construction: about `859 ms`

The production-visible AWS startup difference is therefore not the full 3 seconds from the debug build. In this release run:

- local first frame: `1883 ms`
- AWS first frame: `2034 ms`

The first-frame delta was only about `151 ms`, because the AWS release run had a faster bootstrap path than the local run. Looking specifically at the optional startup goal probe, however, AWS still pays an avoidable `~859 ms` before first frame when the goal lookup fails.

The raw response path is fast in release just as in debug: the JSON-RPC error reaches the TUI in `1 ms`. The delay is still after the raw typed result and before the wrapped result, i.e. expected-error construction on the startup path.

### Code-Level Fix

The fix remains the same, but release changes the priority framing:

- Do not call `wrap_err` for expected unsupported goal state during startup.
- Do not block first-frame rendering on the optional paused-goal prompt.
- Add explicit goal support/materialization metadata so AWS Object Log resumed threads can skip the startup `thread/goal/get` probe.

This removes an avoidable `~859 ms` release-build startup cost for AWS Object Log resumes and also removes the much larger debug-build latency that made the issue obvious.

## Iteration 8 - Release Build After Durable Goal Store

### Hypothesis

The backend-neutral durable goal store should remove the AWS-only `thread/goal/get` failure during TUI startup. If that was the remaining AWS delay, AWS should no longer pay the `~860 ms` release-mode error wrapping cost seen in iteration 7.

### Setup

- Built release binary with `cargo build --release -p codex-cli --bin codex`.
- Installed release binary to `/usr/local/bin/codex`.
- Local config: `/workspaces/codex/codex-smoke-local/config.toml`.
- AWS config: `/workspaces/codex/codex-smoke-aws/config.toml`.
- Local logs: `perf-logs/iter6-release-local/server.log`, TUI log `perf-logs/tui-local/codex-tui.log`.
- AWS logs: `perf-logs/iter6-release-aws/server.log`, TUI log `perf-logs/tui-aws/codex-tui.log`.

### Local Release Baseline

Thread: `019f4640-a813-7c41-a98b-b3751ab6e916`.

- app-server session initialized: `189 ms`
- bootstrap and hooks prefetch: `468 ms`
- `thread/resume` RPC: `437 ms`
- resumed turns: `2`
- `has_rollout_path=true`
- `thread/goal/get`: success, `1 ms`
- paused-goal check: success, `1 ms`
- first frame scheduled: `911 ms`

### AWS Object Log Release Measurement

Thread: `019f43ce-0a65-70f0-87fc-1eed5f181cb3`.

The resumed AWS thread was valid:

- `history_item_count=114`
- snapshot `item_count=114`
- `head_seq=114`
- `has_snapshot=true`
- resumed turns: `3`
- `has_rollout_path=false`

Storage and resume:

- AWS `read_head`: `6 ms`
- AWS snapshot S3 `get_s3_object`: `110 ms` for `209793` bytes
- AWS `get_snapshot_payload`: `111 ms`
- AWS `query_commit_pointers`: `16 ms` for `113` commit pointers
- AWS `load_items_for_head`: `128 ms`
- AWS `read_thread include_history=true`: `134 ms`
- `thread/resume` RPC: `559 ms`

Paused-goal check:

- `thread/goal/get`: success, `5 ms`
- server-side `thread/goal/get` close: `time.busy=1.58ms`, `time.idle=1.81ms`
- paused-goal check: success, `5 ms`
- first frame scheduled: `1035 ms`

### Interpretation

The durable goal-store fix removed the previous AWS-specific startup failure:

- before fix, AWS `thread/goal/get` failed and the TUI spent about `860 ms` in release mode constructing/context-wrapping the expected error before first frame
- after fix, AWS `thread/goal/get` succeeds in `5 ms`

The remaining release-mode first-frame delta in this run was:

- local first frame: `911 ms`
- AWS first frame: `1035 ms`
- AWS minus local: `124 ms`

That `124 ms` delta is consistent with AWS Object Log history loading, primarily the S3 snapshot fetch and decode path:

- AWS `read_thread include_history=true`: `134 ms`
- S3 snapshot fetch alone: `110 ms`
- local rollout/session resume has no comparable network fetch

### Conclusion

The specific delay we were investigating is fixed. AWS startup is no longer blocked by unsupported goal handling or expensive expected-error wrapping. The remaining AWS-vs-local difference is about `100-150 ms` in this release run and is explained by remote snapshot retrieval for the 114-item thread.

Potential follow-up optimization:

- avoid querying commit pointers when the snapshot sequence equals the head sequence, since this run loaded `0` commit payloads and skipped all `113` commits after reading the snapshot
- cache or prefetch the latest snapshot/head metadata if repeated resumes of the same thread are common
