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

## Phase A2 — localhost A2A server (done)

### What it does

- `[a2a]` in config.toml, all of it in Settings through the hub API: `server` (default **false**), `port`
  (default 47802), `[[a2a.callers]]` (`id` `[a-z0-9-]{1,40}`, `name`, `token`, `allow_approvals`, default
  false). The server binds **127.0.0.1 only** and starts, stops, moves or swaps its callers at runtime when
  the settings change (a port in use is refused before anything is written: `invalid:a2a.port: is in use`).
  The settings view shows its live state (`running`, `url`, `error`).
- Tokens: 32 random bytes hex, generated by the hub (`a2a_rotate_token{callerId}`), returned once in that ack,
  stored in config.toml like the other write-only secrets, never in `get_settings` (only `tokenSet`), never
  logged, audited as the key `a2a.callerTokens` without a value. A rotated or removed token stops working at
  once. A new caller has no token until the user makes one.
- Transport and security (`crates/a2a/src/server.rs`, generic over the SDK's `RequestHandler`):
  - JSON-RPC at `POST /`, HTTP+JSON (REST) under `/`, SSE streaming, no push notifications (every push
    method answers `PushNotificationNotSupported`). Pawl's own dispatcher over the `a2a-lf` types since the
    server swap below (`server/rpc.rs`, `server/rest.rs`).
  - Version: `A2A-Version` header or `?A2A-Version=` (spec 3.6.1). JSON-RPC: 1.x -> 1.0; none or 0.3 -> 0.3
    (`compat.rs` converts methods, `kind` shapes, enums, `blocking`, stream events with `final`, the card) to
    the same handler; anything else -> `VersionNotSupported` (-32009). REST is 1.0 only (none or 1.x).
  - Every request: `Host` must be `127.0.0.1`, `localhost` or `[::1]` **with the bound port** (DNS rebinding
    defence); an `Origin` must be this server's own origin; `OPTIONS` is refused; no CORS headers ever;
    bodies over 1 MiB are refused (the guard buffers every body with that limit).
  - Everything but the public card needs `Authorization: Bearer <token>`: constant-time compare against
    every caller's token; `WWW-Authenticate: Bearer` on 401. 30 failed attempts a minute (all clients) ->
    every attempt gets 429 for the rest of the minute. 20 calls/s per caller (burst 40). The public card:
    10/s all together.
  - The guard strips any client-sent `x-pawl-a2a-caller` and the `Authorization` header and passes the
    authenticated caller id to the handler in `x-pawl-a2a-caller`.
- Agent Card (`card.rs`) at `/.well-known/agent-card.json`, public: orchestrator skills only (`status`,
  `delegate`, `prompt`), interfaces JSON-RPC 1.0, HTTP+JSON 1.0 and JSON-RPC 0.3 (same URL), capabilities
  streaming / no push / extended card, the Structured Input extension (`jsonschema_draft: 2020-12`) and Pawl's
  delegate extension, bearer security. Readers sending `A2A-Version: 1.x` get the strict 1.0 card; others get a
  dual card (1.0 fields + 0.3 `url` / `preferredTransport` / `protocolVersion` / `additionalInterfaces`,
  without the scheme table, whose 0.3 and 1.0 shapes conflict) that both the SDK and 0.3 readers parse (the
  SDK client sends no version header when it fetches a card). ETag + `If-None-Match`, `Cache-Control:
  max-age=300`. The extended card (`GetExtendedAgentCard` / `agent/getAuthenticatedExtendedCard`, token
  required) adds a `delegate:<agent>` skill per installed agent with the repo names; the public card never
  names agents, repos or folders.
- Policy (`crates/hub/src/core/a2a_server.rs`): a caller is `Caller::Outside(id)`, bound to the contexts it
  created (others' tasks are `TaskNotFound`, others' contexts `invalid params: unknown context`). A start
  needs an installed agent and a folder the machine already knows (its repo index or `[peer] start_roots`,
  the step-4 rule for paired machines; UNC/device paths refused); the session is remote-driven (rules never
  auto-approve its requests); a caller's own `confirmed` is ignored. Answers to `INPUT_REQUIRED`: never,
  unless the caller has `allow_approvals`; then allow once or deny only, never "always", never allow on
  execute / delete / fetch. Decisions go through `Hub::decide` (first valid decision wins) and land in the
  permission audit log as `a2a:<id>`.
- `SendMessage` blocks until the task finishes or asks for input (spec 3.2.2), at most 60 s, unless
  `returnImmediately`; streams send the task, then its status / artifact updates until a terminal state
  (a lagging reader gets a fresh task snapshot). `SubscribeToTask` on a finished task is
  `UnsupportedOperation`.
- Audit: every call is one `a2a_log` row (new table, store schema v9): `a2a:<id>`, method, target task /
  context, `ok` or the error reason. Never message text or tokens. Kept: newest 100 000.
- Hub API for the UI (A2A section of Settings and a delegations view): `get_settings` / `update_settings`
  (`a2a` section), `a2a_rotate_token{callerId}` -> `{token}`, `list_tasks{sessionId?}` -> `{tasks}`;
  `@pawl/hub-client` gains `rotateA2aToken(callerId)` and `listTasks(sessionId?)`. `pawl settings` prints the
  section (tokens as set / not set).

### Wire changes (additive; PROTOCOL_VERSION stays 8; t13 told)

- `HubSettings.a2a` (`A2aSettings`: server, port, running, url, error, callers `A2aCallerSetting[]`): optional
  in TS so existing full `HubSettings` fixtures compile; `Box<HubSettingsA2a>` with `serde(default)` in Rust
  (boxed to keep `ServerMsg` under clippy's variant-size limit).
- `HubSettingsPatch.a2a` (`A2aSettingsPatch`: server?, port?, callers? `{id, name, allowApprovals?}[]`).
- `A2A_DEFAULT_PORT = 47802`; `A2aTaskSummary {taskId, sessionId, state, text?, caller, updatedAt?}`.
- ClientMsg `a2a_rotate_token{callerId}` -> `{token}`; ClientMsg `list_tasks{sessionId?}` -> `{tasks}`.
- Rust code that builds `HubSettingsPatch { .. }` field by field needs `a2a: None` (one hub test).

### Files (A2)

- New: `crates/a2a/src/{compat,card,server}.rs`, `crates/a2a/src/compat/tests.rs`,
  `crates/a2a/src/server/tests.rs`, `crates/hub/src/core/a2a_server.rs`, `crates/hub/src/core/tests/a2a_server.rs`.
- Changed: `crates/a2a/{Cargo.toml,src/lib.rs}`, `crates/hub/Cargo.toml` (dev-dep `tower`, already in the tree),
  `crates/hub/src/{config.rs,settings.rs,store.rs,core.rs}`, `crates/hub/src/core/{a2a.rs,settings.rs}`,
  `crates/hub/src/core/tests.rs`, `crates/hub/src/settings/tests.rs`, `crates/protocol/src/lib.rs`,
  `packages/protocol/src/index.ts` (both CRLF kept), `packages/hub-client/src/client.ts`,
  `packages/hub-client/test/settings.test.ts`, `Cargo.lock`.

### Dependencies (A2)

- First version: `a2a-server-lf = "=0.5.3"`. Its `a2a-pb` dependency has a build script that runs a vendored
  `protoc` and compiles tonic / prost / pbjson (~40 crates) although Pawl uses no gRPC; flagged to t13, who
  decided to drop it (see "A2 server swap").
- Now: `axum 0.8.9` (the hub's), `subtle 2.6.1` (the hub's), `async-trait 0.1` (re-exported as
  `pawl_a2a::async_trait` for `A2aHandler`), dev-dependency `tower 0.5` (util; in the tree through axum).

### Commands and results (A2)

- `cargo test -p pawl-a2a`: 53 passed (27 A1 + compat 8, card 3, server 15).
- `cargo test -p pawl-hub --lib`: 227 passed (+10: outside delegation bound and audited, known folders and
  installed agents only, approvals per caller with the hard floor, 0.3 client end to end, blocking SendMessage,
  streaming until the end, extended card behind the token, settings on/off + token rotation over a real
  loopback socket, validation, list_tasks).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- TS: `bunx tsc --noEmit` clean in packages/protocol, packages/hub-client, packages/pet-engine, apps/desktop,
  apps/vscode, apps/mobile (read-only check of the apps); `bun test` in packages/hub-client: 339 passed (+2).
- `cargo test --workspace --no-fail-fast` (A2 gate): exit 0. pawl-a2a 53, pawl-acp 25, pawl-brain 111,
  pawl-discovery 39, pawl-hub 227, pawl-peer 105 (+1 ignored, as before), pawl-protocol 4.

### Decisions (A2)

- Approvals by outside callers are off per caller by default; even when on, they never cover execute /
  delete / fetch or "always" (the hard floor needs a person on a Pawl client).
- Outside callers start agents only in known folders (the same rule as paired machines).
- 0.3 is accepted over JSON-RPC (the 0.3 default transport); REST is 1.0 only.
- The public card omits the security scheme table for readers that send no version (see above); auth is
  enforced regardless and declared on the strict and extended cards.

### Known gaps (A2)

- No per-caller cap on open SSE streams (the rate limit bounds how fast they open).
- The desktop Settings UI for the A2A section is another worker's (everything is in the hub API).

## A2 server swap (t13's decision: drop `a2a-server-lf`) (done)

- `crates/a2a/src/server.rs` defines Pawl's `A2aHandler` trait (the seven A2A operations; push configuration
  is not part of it: the dispatcher answers those methods `PushNotificationNotSupported` itself, spec 3.3.4),
  `ServiceParams`, `EventStream`, and its own SSE writer (numbered events, 15 s keep-alive). Guard, auth, rate
  limits, card negotiation and the 1 MiB cap are unchanged.
- `server/rpc.rs`: one JSON-RPC dispatcher for 1.0 and 0.3 (`Wire::V1 | V03`) over typed `a2a-lf` requests.
  `rpc::call(handler, params, body, wire)` returns a JSON-RPC response or a stream of them, so the HTTP route
  (JSON or SSE) and the encrypted peer link (phase A4) share it. Checks: JSON (-32700), object with `"jsonrpc":
  "2.0"`, an `id` and a method (-32600), known method of that version (-32601), object params and typed
  parsing (-32602); errors carry the `ErrorInfo` detail (`a2a-lf`'s `to_jsonrpc_error`).
- `server/rest.rs`: HTTP+JSON per spec 11.3 (`/message:send`, `/message:stream`, `GET /tasks/{id}`,
  `GET /tasks` with query parameters per 11.5, `POST /tasks/{id}:cancel`, `POST /tasks/{id}:subscribe`, push
  config routes -> not supported, `GET /extendedAgentCard`); answers `application/a2a+json`; errors are the
  11.6 `google.rpc.Status` JSON with the 5.4 HTTP status and gRPC status name and the `ErrorInfo` detail.
  `:cancel` / `:subscribe` come off the end of the id segment (Pawl task ids contain `:`).
- ListTasks now refuses `pageSize` over 100 and negative `historyLength` (spec 6.5 validation example)
  instead of clamping.
- Conformance tests (`server/conformance.rs`, 9 tests) send the spec's own requests through the router and
  check the replies: 6.1 basic task (exact JSON), 6.2 streaming (task, artifactUpdate, statusUpdate), 6.3
  multi-turn (INPUT_REQUIRED, follow-up with `taskId`), 6.4 version error, 6.5 listing (exact JSON, query
  mapping, pagination token, validation example refused before the handler), 6.9 extended card, 9.4.1-9.4.8
  every JSON-RPC method (SSE responses carry the request id), 9.5 errors (-32700, -32600 x3, -32601, -32602 x2,
  TaskNotFound with ErrorInfo), 11.6 HTTP error shape. Spec inconsistency noted: the 6.4 and 6.5 examples show
  `application/problem+json` bodies while the normative 11.6 defines `google.rpc.Status`; Pawl follows 11.6.
- Cargo.lock lost: a2a-pb, a2a-server-lf, tonic, tonic-build, tonic-prost, tonic-prost-build, prost,
  prost-build, prost-derive, prost-types, pbjson, pbjson-build, pbjson-types, protoc-bin-vendored (+8 platform
  binaries), tower-http 0.7, petgraph, regex, pulldown-cmark(-to-cmark), prettyplease, multimap, itertools,
  hyper-timeout, axum-macros, tokio-stream, wasm-streams, unicase, either, foldhash, hashbrown (457 lines).
  Against the A2 commit only `a2a-lf`, `async-trait` and the A1 entries stay added.
- Results: `cargo test -p pawl-a2a` 62 passed (53 + 9 conformance); `cargo test --workspace --no-fail-fast` exit 0
  (pawl-a2a 62, pawl-acp 25, pawl-brain 111, pawl-discovery 39, pawl-hub 227, pawl-peer 105 + 1 ignored,
  pawl-protocol 4); `cargo clippy --workspace --all-targets -- -D warnings` clean. No wire change.
