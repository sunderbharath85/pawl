# sde-a2a — A2A orchestration (PLAN.md "A2A orchestration")

Worktree `E:\workspace\ai-pet-a2a`, branch `feat/a2a` (nothing committed; t13 reviews and commits).
The prompt first named `E:\workspace\ai-pet-voice`; t13 corrected that to this worktree, which is the
only one touched.

Docs read before coding (2026-11-24):

- A2A spec v1.0.1 (tag): https://github.com/a2aproject/A2A/blob/v1.0.1/docs/specification.md
  (sections 3.1 operations, 3.2.2 blocking vs `returnImmediately`, 3.2.4 history length, 3.4 contexts and
  tasks, 3.6 versioning: empty `A2A-Version` = 0.3, 5.8 custom bindings, 8 Agent Card, 9 JSON-RPC,
  11 HTTP+JSON, 12 custom binding guidelines, 13 security, Appendix A 0.3 -> 1.0 migration)
- A2A spec v0.3.0 (tag, for 0.3 clients): https://github.com/a2aproject/A2A/blob/v0.3.0/docs/specification.md
  (3.5.6 method table: `message/send`, `message/stream`, `tasks/get`, `tasks/cancel`, `tasks/resubscribe`,
  `agent/getAuthenticatedExtendedCard`; `kind` discriminators; lower-case states)
- Structured Input extension v1: https://github.com/a2aproject/experimental-ext-structured-input/blob/main/v1/spec.md
- Official Rust SDK https://github.com/a2aproject/a2a-rs, read from the published crate sources:
  `a2a-lf` 0.4.2 (https://docs.rs/a2a-lf/0.4.2), `a2a-server-lf` 0.5.3 (https://docs.rs/a2a-server-lf/0.5.3),
  `a2a-client-lf` 0.2.9 (https://docs.rs/a2a-client-lf/0.2.9), `a2a-pb` 0.3.2 (pulled in by both).
  Findings that shaped the design:
  - `a2a-server-lf`'s JSON-RPC router treats a missing `A2A-Version` as "0.3" and rejects it
    (`VERSION_NOT_SUPPORTED`), and `a2a-client-lf` only speaks 1.0. Accepting 0.3 clients therefore needs a
    compat layer of ours (phase A2).
  - The SDK's own Agent Card route reflects any `Origin` with `Access-Control-Allow-Credentials: true`;
    Pawl serves its card itself, behind the loopback Host/Origin checks.
  - `a2a-pb` depends on `tonic`/`prost` unconditionally, so the server and client crates pull those in even
    though Pawl uses no gRPC binding.

## Phase A1 — internal facade (done)

### What it does

- New crate `crates/a2a` (`pawl-a2a`, dependency `a2a-lf = "=0.4.2"` imported as `a2a`, plus `chrono`, which
  `a2a-lf` already pulls in):
  - `tasks.rs` `TaskRegistry`: the A2A view of this machine's owned ACP sessions. Context id = the session's
    global id; one Task per prompt turn with a global id `${machineId}:${uuidv7}` (so the owner can be read
    off it, like every other Pawl id). In memory only, bounded (50 tasks per context, 2000 in all, 256 KiB of
    reply text per task), live updates through a broadcast channel (`subscribe`), `ListTasks` paging
    (newest first, opaque cursor, page size default 50 / max 100), history-length trimming (spec 3.2.4).
    Contexts are bound to the caller that created them (`Caller::Local | Peer(machine) | Outside(caller)`):
    outside callers see and act on their own contexts only; this machine's clients and paired hubs see all.
  - `mapping.rs`: ACP -> A2A. Thought/tool/plan -> `WORKING` status updates (tool calls carry "Running:
    cargo test"-style status text); agent message chunks -> the `response` artifact (appended, last chunk
    flagged) and the final status message; edit/delete/move tool calls -> a `diff` artifact per call (the
    tool's locations and raw input; pawl-acp does not forward rendered diffs); plans -> a `plan` artifact.
    Stop reasons: end_turn -> COMPLETED, cancelled -> CANCELED, refusal -> REJECTED, max_tokens /
    max_turn_requests -> COMPLETED with metadata `pawl/stopReason`; `session/prompt` errors, process exit and
    session end -> FAILED.
  - `structured_input.rs`: `request_permission` -> `INPUT_REQUIRED` whose status message has a text line,
    a data part with the tool call and the options (names and kinds), and exactly one `input_schema` part:
    `{optionId: enum = the ACP option ids}`, `additionalProperties: false`, tagged with the extension URI and
    the Pawl request id as the input request id. Answers: a `structured_input` part (id must match, spec 6.3)
    or one untagged valid `DataPart` (spec 6.4). Invalid answers re-ask with `validation_error` parts in the
    JSON Schema basic output format (spec 7). Several pending requests in one turn are asked one at a time.
  - `ext.rs`: extension URIs (Structured Input; Pawl's delegate extension
    `https://sunderbharath85.github.io/pawl/a2a/delegate/v1`, message metadata `{agentId, cwd, machineId?,
    confirmed?}` for "start a session").
- Hub (`crates/hub/src/core/a2a.rs`), the in-process A2A agent the orchestrator delegates to:
  - `a2a_send(SendMessageRequest, caller, actor, answers)`: no `contextId` + delegate metadata = start a
    session (bound to the caller; then the first turn); `contextId` = a new turn (task) in that session;
    `taskId` of an `INPUT_REQUIRED` task = the answer to its pending confirmation, decided through the same
    `Hub::decide` path as a click (first valid decision wins, audit log). `a2a_cancel` = `CancelTask`
    (`session/cancel`, the task ends CANCELED; finished tasks are `TaskNotCancelable`). `a2a_get` /
    `a2a_list` = `GetTask` / `ListTasks` for the caller.
  - The orchestrator's code paths now go through it: `Hub::start_session`, `Hub::prompt` and `Hub::cancel`
    (used by ClientMsgs `start_session` / `prompt` / `cancel`, the bot's actions and proposals) build A2A
    requests and call the facade; the owner side of peer intents (remote prompt, start, cancel) calls it as
    `Caller::Peer(origin)`. The ACP host is only called by the facade's executor
    (`launch_session`, host `prompt`, `acp_cancel`).
  - Status: the bot's context (`BrainContext.tasks`, new `pawl_brain::chat::TaskBrief`) carries the latest
    task of every session; an agent status names a canceled / refused / failed last task.
  - Every event of an owned session is fed to the registry from `record_event` (same order as recorded);
    a session that ends fails its running task. Hook and mirrored sessions get no tasks.
  - Approval rights per caller (`Answers`): this machine's clients any option; outside callers (phase A2)
    only when the user allows it per caller, and then never "always" and never allow on `execute` /
    `delete` / `fetch` (a person approves those on a Pawl client); paired hubs keep the approve/deny intents.
  - Sessions started or prompted by a non-local caller are marked remote-driven before their first
    prompt: rules never auto-approve their requests (same as step 4).
- Behaviour unchanged: the same checks run in the same order (owned/open session, prompt limits, the agent's
  refusal leaves no task), the same ack codes and warnings, approvals and the audit log untouched, intents
  keep their validation and logging. All 207 existing hub tests pass unchanged.

### Files

- New: `crates/a2a/Cargo.toml`, `crates/a2a/src/{lib,ext,mapping,structured_input,tasks}.rs`,
  `crates/a2a/src/tasks/tests.rs`, `crates/hub/src/core/a2a.rs`, `crates/hub/src/core/tests/a2a.rs`,
  this report.
- Changed: `crates/hub/Cargo.toml` (`pawl-a2a` by path: the root `Cargo.toml` is t13's), `crates/hub/src/core.rs`
  (facade field, start/prompt/cancel through A2A, event feed, bot context tasks),
  `crates/hub/src/core/intents.rs` (owner side through the facade), `crates/hub/src/core/tests.rs`
  (`mod a2a;`), `crates/brain/src/chat/mod.rs` (`TaskBrief`, `BrainContext.tasks`, status note),
  `crates/brain/src/chat/tests.rs` (one test), `Cargo.lock` (adds `pawl-a2a` and `a2a-lf` 0.4.2 only; its other deps were already in the tree).
- No wire change in A1 (`packages/protocol` and `crates/protocol` untouched).

### Commands and results

- `cargo test -p pawl-a2a`: 27 passed.
- `cargo test -p pawl-hub --lib`: 217 passed (207 before + 10 new A2A tests).
- `cargo test -p pawl-brain`: 111 passed (110 + 1).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace --no-fail-fast` (A1 gate): exit 0. pawl-a2a 27, pawl-acp 25, pawl-brain 111,
  pawl-discovery 39, pawl-hub 217, pawl-peer 105 (+1 ignored, as before), pawl-protocol 4.
- No TypeScript package touched in A1.

### Decisions

- Tasks are in memory (not in hub.db): owned sessions end with the hub process, and the events tasks are
  built from are in the event log already. Nothing new is written to disk.
- max_tokens / max_turn_requests end COMPLETED with `pawl/stopReason` (the agent stopped normally, just
  early) rather than FAILED.
- Task ids are global ids so phase A4 can route `GetTask` / `CancelTask` by prefix.
- A prompt's task is opened before the prompt is sent (so the turn's first events attach to it) and
  dropped again if the agent refuses the prompt.
- Three A2 entry points (`A2aError::find`, `Delegated::response`, `a2a_list`) are only used by tests until the
  server lands; they carry `#[cfg_attr(not(test), expect(dead_code, ...))]`, which fails the build once A2 uses
  them and the marker must go.

### Known gaps (A1)

- No rendered diffs in `diff` artifacts: pawl-acp's tool-call payload has locations and raw input only.
- Prompts typed into a client of another machine reach this machine as intents and get tasks; prompts the
  ACP agent receives some other way (none today) would get an implicit task from their `user_message`.
