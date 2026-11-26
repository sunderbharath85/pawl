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

## Phase A3 — outside A2A agents as sub-agents (done)

### What it does

- Settings `[[a2a.agents]]` (`id` `[a-z0-9-]{1,40}`, optional `name`, `url`, `token`), all through the hub API
  (`update_settings` `a2a.agents`, the list replaces; `token`: a string sets it, null clears it, absent keeps it
  while the id stays). URL: absolute `https://`, or `http://` to this machine only; no user info. Token: 1-512
  visible ASCII characters, write-only (`tokenSet`), never logged, audited as the key `a2a.agentTokens`. At most 16.
- Card (`crates/a2a/src/client.rs`): fetched from `/.well-known/agent-card.json` under the URL (or the URL when it
  names a `.json` file), with the bearer token, `If-None-Match` / ETag revalidation, 256 KiB cap, 60 s timeout,
  **no redirects**. 0.3 and 1.0 cards are read (0.3 normalized by `compat::card_from_v03`; OpenAPI-style
  schemes converted; a card in both shapes works). Signatures are not verified (no JWS library approved): Settings
  show `signature: "unsigned"` (none) or `"unverified"` (present). The card's JSON-RPC endpoint must be on the
  **same origin** as the URL the user added (else the agent is refused: the card cannot send the token elsewhere);
  1.0 is preferred, 0.3 used when it is all the card offers. Cards are cached in memory (fetched at start, on every
  agents/token change and on `list_agents{refresh}`); the agent list itself is persisted as before.
- Agents: each configured agent is an agent `a2a:<id>` (`source: "a2a"`, name from the setting, else the card),
  `installed` once its card was fetched, with a pet, listed next to the ACP agents (bot, proposals, peers' presence:
  `:` is now allowed in peer agent ids). Settings show its card summary (name, description, version, protocol
  version, streaming, up to 50 skills, signature, fetch time) and the last fetch error.
- Sessions: starting `a2a:<id>` (any client, the bot, the A2A facade) makes a session of this machine
  (`source: owned`, cwd = the agent URL), so it shows in every client and in paired machines' mirrors; it is
  remote-driven from the start (rules never auto-approve). Each prompt is `SendStreamingMessage` (or blocking
  `SendMessage` when the card says no streaming) in the agent's context; the next prompt continues that context.
  One turn at a time ("already running a prompt; cancel it first", like ACP).
- Mapping (`crates/hub/src/core/a2a_agents.rs`, events through the same `HostEvent` path as the ACP host, so pets,
  notifications, the event log, mirror and our own A2A task view all work unchanged): working status text and
  artifact text -> agent message chunks (data / file parts shown as placeholders); COMPLETED -> `end_turn`,
  CANCELED -> `cancelled`, REJECTED -> `refusal`, FAILED -> an error; a stream that ends mid-task, a timeout
  (10 min idle) or more than 64 MiB / 1 MiB per event -> an error.
- `INPUT_REQUIRED` with a Structured Input schema that picks one value (one enum property) -> an approval card:
  options = the values (kinds from Pawl-style details when another Pawl asks, else from the word: deny / reject /
  no / cancel -> reject, "always" -> always, anything else -> allow once), tool kind **other** unless Pawl-style
  details name one, title = the agent's text. Never auto-approved (remote-driven session; and `other` can never
  be auto-allowed). The answer goes back as a `structured_input` part tagged with the agent's input request id
  (`{<property>: <value>}`), on the open stream's task; "deny with no option" cancels the agent's task. Other
  questions (plain text, auth required, a schema Pawl cannot fill in) are shown as the agent's message and the
  user's next prompt answers them (sent with the task id).
- Cancel = `CancelTask` on the agent's task (the reader stops, the turn ends `cancelled`); close = cancel + end.
  Resume and history do not apply to outside agents.

### Wire changes (additive; PROTOCOL_VERSION stays 8; t13 told)

- `Agent.source` gains `"a2a"`; `A2A_AGENT_PREFIX = "a2a:"`.
- `A2aSettings.agents?: A2aAgentSetting[]` (`{id, name, url, tokenSet, card?: A2aAgentCardSummary, error?}`);
  `A2aAgentCardSummary {name, description, version, protocolVersion, streaming, skills[{id,name,description}],
  signature: "unsigned"|"unverified", fetchedAt}`; `A2aSettingsPatch.agents?: {id, name?, url, token?: string|null}[]`.
- Rust code that builds `HubSettingsA2aPatch { .. }` field by field needs `agents: None` (one hub test).

### Files (A3)

- New: `crates/a2a/src/client.rs`, `crates/a2a/src/client/tests.rs`, `crates/hub/src/core/a2a_agents.rs`,
  `crates/hub/src/core/tests/a2a_agents.rs`.
- Changed: `crates/a2a/{Cargo.toml,src/lib.rs}` (deps `reqwest 0.13` json, the brain's; `url 2`; both already
  in the tree), `crates/hub/src/{config.rs,settings.rs,repos.rs,core.rs}`,
  `crates/hub/src/core/{a2a.rs,settings.rs,tests.rs,tests/a2a_server.rs}`, `crates/protocol/src/lib.rs`,
  `packages/protocol/src/index.ts` (CRLF kept), `Cargo.lock` (two dependency lines, no new package).

### Commands and results (A3)

- `cargo test -p pawl-a2a`: 67 passed (+5 client: card fetch + ETag + same-origin endpoint, 1.0 and 0.3 calls and
  streams against a real loopback agent, wrong token -> 401, oversized card / redirect / junk / signed 0.3 card,
  URL rules, SSE parsing).
- `cargo test -p pawl-hub --lib`: 235 passed (+8: listed with card summary and write-only token; a streaming turn
  with an approval card answered (structured input tagged with the input id; completes; the A2A task view
  follows; audited; the next prompt keeps the context); deny -> refusal; never auto-approved even with a read
  rule; cancel -> `CancelTask` + `cancelled`; one turn at a time; close; a non-streaming agent; validation and an
  unreachable agent's error; removal). Stable across repeated runs.
- `cargo test --workspace --no-fail-fast`: exit 0 (pawl-a2a 67, pawl-acp 25, pawl-brain 111, pawl-discovery 39,
  pawl-hub 235, pawl-peer 105 + 1 ignored, pawl-protocol 4). `cargo clippy --workspace --all-targets -- -D
  warnings`: clean.
- TS: `bunx tsc --noEmit` clean in packages/protocol, packages/hub-client, packages/pet-engine, apps/desktop,
  apps/vscode, apps/mobile; `bun test` in packages/hub-client: 339 passed.

### Decisions (A3)

- Own client over reqwest (not `a2a-client-lf`): one path for 1.0 and 0.3 agents (Gemini CLI's A2A server is
  0.3), our own caps, timeouts and redirect policy, and no `a2a-pb` / tonic (which t13 just removed).
- http only to loopback; token only to the card's same-origin endpoint; no redirects.
- An outside agent's text questions end the user's turn (local stop `end_turn`) while the agent's task waits; the
  next prompt answers with the task id.

### Known gaps (A3)

- Card signatures are shown, not verified (needs an approved JWS library).
- Cards are cached in memory only (refetched at start).
- Push notifications from outside agents are not used (streaming or blocking calls only).

## Phase A4 — machine to machine A2A over the encrypted link (done; protocol v9)

### What it does

- A custom A2A binding (spec 5.8 / 12; URI `https://sunderbharath85.github.io/pawl/a2a/binding/peer-link/v1`):
  A2A 1.0 JSON-RPC inside the end-to-end encrypted peer link, one call = `PeerBody::A2a { callId, frame }`.
  The origin sends a `request` frame (the JSON-RPC request, the local client that asked, whether the user
  confirmed); the owner answers with one `response` frame (or `event` frames then `end` for a stream). No LAN
  port, no TLS trust: the link's NaCl boxes, seq / direction nonces and pairing are the transport security.
- Version gate: every hub now puts `protocolVersion: 9` in its presence. Between v9 hubs, start / prompt /
  cancel go as A2A (`Hub::forward` switches by the owner's latest presence); a peer whose presence has no version
  (v8 or older) keeps getting the intents. Approve / deny stay intents between v9 hubs too (the step-4
  confirmed / allow-once-only rules and their audit origin, unchanged).
- Origin (`core/a2a_peer.rs` `forward_a2a`): start = `SendMessage` with delegate metadata `{agentId, cwd,
  machineId, confirmed}` (`returnImmediately`); the owner's answer carries `{session, warning?}` in the
  delegate metadata, so the client ack is the same `{session, warning?}` as before. Prompt = `SendMessage` in
  the session's context. Cancel = `ListTasks{contextId, pageSize 1}`, then `CancelTask` on the running task
  (none: nothing to cancel; finished meanwhile: fine). Checks before sending are unchanged (`unconfirmed` for an
  unconfirmed start). Timeouts and link errors give the same `timeout` / `offline`; the call is in the origin's
  intent log (kind start / prompt / cancel, status, reason, the call id as intent id).
- Owner: the shared JSON-RPC dispatcher (`pawl_a2a::server::rpc::call`) with a peer policy handler
  (`Caller::Peer(origin)`): exactly the intent rules. `[peer] accept_intents = false` -> forbidden; the
  per-peer intent rate limit; repeated call ids get the same answer (rate-limited ones may retry); prompt:
  this machine's open owned session and the prompt limits; start: confirmed, installed agent, a known folder
  (repo index or `[peer] start_roots`, UNC / device paths refused); sessions and prompts from a peer are
  remote-driven (rules never auto-approve); continuations (answers) are refused (approvals use the intents).
  Refusals carry the intent reason in the JSON-RPC error's ErrorInfo metadata (`pawlReason`), so the origin's
  client sees the same codes (`forbidden`, `not_found`, `invalid`, `unconfirmed`, `rate_limited`, ...). Every call
  is in the owner's intent log with the same call id as the origin's. Start / prompt answers leave out the task's
  history and artifacts (link frames are size-capped). The owner also serves GetTask / ListTasks / streams /
  the extended card to peers (the origin uses only unary calls).

### Wire changes (protocol v9; t13 told)

- `PROTOCOL_VERSION = 9` (TS and Rust).
- `Presence.protocolVersion?` (absent = v8 or older); `A2A_PEER_MIN_PROTOCOL = 9`; `A2A_PEER_BINDING`.
- `PeerBody` `{type: "a2a", callId, frame: A2aPeerFrame}`; `A2aPeerFrame` tagged `kind`: `request {rpc, client,
  confirmed}` | `response {rpc}` | `event {rpc}` | `end`.
- Rust `Presence { .. }` literals need `protocol_version` (fixed in hub, peer tests and the peer test vectors;
  the vectors' JSON is unchanged because the field is skipped when absent).

### Files (A4)

- New: `crates/hub/src/core/a2a_peer.rs`, `crates/hub/src/core/tests/a2a_peer.rs`.
- Changed: `crates/hub/src/core.rs`, `crates/hub/src/core/{intents.rs,mirror.rs,peers.rs,a2a_server.rs}`,
  `crates/hub/src/core/tests.rs`, `crates/hub/src/core/tests/phones.rs`, `crates/hub/src/core/settings/tests.rs`
  (the compile-time peer-surface guard lists `A2a`: it never reaches settings), `crates/peer/src/node.rs` (its
  fallback presence names the version), `crates/peer/src/{node/tests.rs,vectors.rs}`, `crates/protocol/src/lib.rs`,
  `packages/protocol/src/index.ts` (CRLF kept).
- Not mine: the `apps/desktop/*`, `packages/hub-client/src/{index,injected,settings,a2a}.ts` and
  `packages/hub-client/test/a2a.test.ts` changes now in the worktree belong to another worker (the A2A UI);
  I did not touch them, and the TS gates below ran with them in place.

### Commands and results (A4)

- `cargo test -p pawl-hub --lib`: 240 passed (+5, two real in-process hubs paired over loopback: start / prompt /
  cancel over A2A with matching origin and owner log rows, the context bound to the peer, remote-driven, the
  mirror; refusals keep the intent codes (unconfirmed before sending, forbidden folder, invalid agent, unknown
  session for prompt and cancel, remote control off); approvals stay intents with their audit origin; v8 /
  v9 / no-presence detection; a silent owner times out). The existing two-hub test
  (`two_hubs_approve_start_and_time_out`) now runs its start and its timed-out prompt over A2A and passes
  unchanged.
- `cargo test --workspace --no-fail-fast`: exit 0 (pawl-a2a 67, pawl-acp 25, pawl-brain 111, pawl-discovery 39,
  pawl-hub 240, pawl-peer 105 + 1 ignored, pawl-protocol 4). `cargo clippy --workspace --all-targets -- -D
  warnings`: clean.
- TS: `bunx tsc --noEmit` clean in packages/protocol, packages/hub-client, packages/pet-engine, apps/desktop,
  apps/vscode, apps/mobile; `bun test`: hub-client 350, pet-engine 75, mobile 79 (cross-language vectors
  included), desktop 117, vscode 23, all passing.

### Decisions (A4)

- Approve / deny stay intents between v9 hubs: the task says start / prompt / cancel are replaced, and the
  intents already carry the confirmed / allow-once-only rules and the audit origin; an A2A continuation from a
  peer is refused.
- Cancel over A2A cancels the session's running task; a session with no running task has nothing to cancel
  (the old intent called `session/cancel` on an idle agent, which did nothing either).
- The protocol version travels in presence (not in hello), so old hubs ignore it and a v9 hub learns it before
  it can act on the peer.

### Known gaps (A4)

- Streams over the link exist on the owner but the origin does not use them yet (unary calls only); an event
  larger than a link frame would be dropped with a warning.
- Phones keep using intents (they do not speak A2A).

## Security review fixes (t13's review of A1-A4 + fe-settings notes) (done)

Every finding fixed, each with tests. No wire change (protocol stays v9).

| # | Finding | Fix | Tests |
|---|---|---|---|
| M1 | An outside agent kept its token when its URL changed (`settings.rs` `check_a2a_agents`) | The token is kept only while the URL keeps its origin (scheme, host, port; `pawl_a2a::client::same_origin_urls`). A move to another origin without a new token is `invalid:a2a.agents[i].token: the agent's URL moved to another host, so its old token is not sent there: enter the token for the new address (or clear it)`; a new token or an explicit `null` moves it | `an_agent_that_moves_host_needs_its_token_again` (same-origin path change keeps it; other host / other port refused, nothing changed; new token; clear; no token before) |
| M2 | `a2a_log` fields unbounded; guard refusals not audited | Caller / method / target / status / detail cut to 128 characters. The guard reports every refusal (forbidden host / origin, too large, unauthorized, throttled, per-caller rate limit, body timeout) through `ServerState::with_audit`; the hub writes `a2a:unknown` or `a2a:<id>` rows, at most 30 a minute (the rest counted in the next row's detail). A removed caller's call is logged `caller_removed` | `a2a_log_fields_are_cut_and_guard_refusals_are_logged`, `a_removed_caller_is_logged_and_refused`, `every_refusal_reaches_the_audit_hook` |
| M3 | Peer reply cache global, count-bounded only | Per peer (one peer cannot evict another's ids): at most 256 ids and 256 KiB; replies over 4 KiB are kept only as an "answered" marker (a repeat gets a small error, never a re-run); an answer too large for a link frame is replaced by a small error so the origin is not left waiting | `remembered_answers_are_bounded_per_peer_by_count_and_bytes` (unit), the repeat case below |
| M4 | Stream calls not deduped; forwarding outlived the link | A stream call is remembered as "accepted" (a repeated id never starts again); at most 4 open streams per peer (`rate_limited`); forwarding stops on `NotLinked` / backpressure; the intent-log row is written before streaming | `a_repeated_call_id_never_runs_twice_and_streams_stop_when_the_link_is_gone` |
| M5 | A hostile outside agent could create unlimited approval cards | One card per session at a time (a later INPUT_REQUIRED, with a fresh id or none, is shown as the agent's text); at most 32 pending cards from outside agents in all (more become questions); at most 8 options per card (more become a question) | `a_hostile_agent_gets_one_card_per_session_and_few_options` |
| M6 | The guard buffered up to 1 MiB before authenticating, with no timeouts | Host / Origin / size / token / rate limit are checked on the headers; only then is the body read, within 10 s (`408` + audit). Pawl's own accept loop (hyper http1, both crates already in the tree through axum) sets a 10 s header-read timeout and at most 64 open connections | `strangers_are_refused_on_their_headers_before_any_body_is_read` (a body that never arrives still gets 401 at once), `an_authenticated_body_must_arrive_in_time` (paused clock), `serve_binds_loopback_only_and_stops` |
| a | `Answers::Limited` floor was a denylist | Allowlist (`limited_may_allow`): a caller with `allowApprovals` may allow once only `read`, `search`, `think`, `edit`; `other`, `move`, `switch_mode`, unknown kinds and execute / delete / fetch need a person; never "always"; reject is always fine | `limited_approvals_are_an_allowlist` |
| b | Rotating / removing a token left open streams and waits | `ServerState::set_callers` revokes a caller whose token changed or who was removed (`ServerState::revocation`, a watch per caller); its open streams end and its blocking waits return at once | `a_rotated_or_removed_token_revokes_its_caller` (unit), `rotating_a_token_ends_the_callers_streams_and_waits` |
| c | No per-caller caps | At most 4 open streams (more: `UnsupportedOperation`), 4 blocking waits (more answer at once, as with `returnImmediately`) and 8 open sessions per caller | `callers_have_caps_on_streams_and_sessions` |
| d | Quadratic SSE parsing; no overall stream limit | The parser remembers how far it searched (each byte looked at about once; boundaries split across chunks handled); a stream ends after 2 h in all (`STREAM_MAX_TIME`) besides the 10 min idle limit | `the_sse_parser_handles_crlf_multiline_data_comments_and_caps` (byte-by-byte boundaries; 900 KB event in 900 chunks under 2 s) |
| e | `peer_call` leaked its waiter when the future was dropped | A drop guard removes the waiter however the call ends | `a_dropped_call_leaves_no_waiter` |
| f | Outside callers saw inner error text | `to_a2a` keeps Pawl's own sentences only (the facade's A2A errors, the outermost text of a refusal); anything else is `the request could not be done here`, detail in the hub log | `outside_callers_get_generic_errors` (an ended session's error shows no path, id or hint) |
| g | A caller re-added under the same id regained the old contexts | Task owners are `Caller::Outside("<id>~<random generation>")`; the generation is new when a caller is added; labels still show `a2a:<id>` | `a_caller_added_again_under_the_same_id_is_a_new_caller`, task-registry unit test |
| h | No settings push after a card fetch (fe-settings) | `refresh_a2a_cards` announces `settings` when it is done | `settings_are_pushed_after_a_card_fetch` |
| i | Pawl's own server refused a plain prompt (fe-settings self-loop) | A message with no context and no delegate metadata gets a short agent message saying how to delegate (no error); an outside session talking to Pawl ends its turn normally | `pawl_added_as_its_own_outside_agent_answers_a_plain_prompt` (Pawl's server added as Pawl's own outside agent, over loopback) |

- Files: `crates/a2a/{Cargo.toml,src/server.rs,src/server/tests.rs,src/client.rs,src/client/tests.rs,src/tasks.rs,src/tasks/tests.rs}`,
  `crates/hub/src/{settings.rs,core.rs}`, `crates/hub/src/core/{a2a.rs,a2a_server.rs,a2a_agents.rs,a2a_peer.rs,settings.rs}`,
  `crates/hub/src/core/tests/{a2a_server.rs,a2a_agents.rs,a2a_peer.rs}`, `Cargo.lock` (two dependency lines: `hyper`,
  `hyper-util` for pawl-a2a; no new package). Dev-dependency `tokio` gains `test-util` in pawl-a2a (paused clocks).
- Commands: `cargo test --workspace --no-fail-fast` exit 0 (pawl-a2a 71, pawl-acp 25, pawl-brain 111, pawl-discovery
  39, pawl-hub 254, pawl-peer 105 + 1 ignored, pawl-protocol 4); `cargo clippy --workspace --all-targets -- -D warnings`
  clean. No TypeScript touched.

## Open items for t13

- PLAN.md / CONTRACTS.md (yours) do not yet describe the A2A wire additions above (A2, A3, A4 sections list them).
- The desktop / VS Code UI for the A2A settings and delegations (hub API complete: `get_settings` /
  `update_settings` `a2a`, `a2a_rotate_token`, `list_tasks`, agents `a2a:<id>`), and showing
  `signature: unverified` honestly.
- A JWS library decision if card signatures should be verified.

## v0.2 A2A: connect coding agents (done)

Contract: docs/CONTRACTS.md "v0.2 A2A: connect coding agents" (written first; t13 OK'd it; `connectTarget` added on t13's
decision). Worktree E:\workspace\ai-pet-voice, branch fix/settings-blank; nothing under apps/desktop or the hub-client UI
touched (those are fe-settings').

What it does: Settings > A2A > "Connect coding agents" (UI by fe-settings) lists Claude Code, Codex, Copilot CLI and
Copilot in VS Code. Connect turns the localhost A2A server on if it is off (preview says so: `startsServer`), creates or
takes over the caller (`claude-code`, `codex`, `copilot-cli`, `copilot-vscode`) with approvals off and a fresh token,
and writes a `pawl` MCP entry that runs `<abs pawl> mcp` with `PAWL_A2A_URL` / `PAWL_A2A_TOKEN` into the agent's own
user-level MCP config (backup first). Remove deletes the entry and the caller (its token stops at once). Every apply /
remove is audited (`a2a_connect` rows, keys `a2a.connect.<target>`) and logged without the token.

Pieces:
- `crates/discovery/src/mcp_config/` (new): per-agent paths and entries, plan / apply / check, and `jsonc.rs`, a small
  span-keeping JSON / JSONC reader: only the `pawl` member is inserted, replaced or removed, every other byte stays
  (comments, key order, indentation, CRLF, one-line files); an edit is read back and refused if anything but Pawl's
  entry would change. Codex TOML through `toml_edit` (our table updated in place, so the user's own keys on it stay).
  Malformed files are refused ("... fix it or add the entry by hand"); a file that changed between read and write is
  not overwritten; a link is written through to its target; new files are 0600 on Unix. The hooks installer's backup /
  temp-file helpers are shared (`hooks::install` items made `pub(crate)`).
- `crates/a2a/src/mcp/` (new): `pawl mcp`, a hand-written stdio MCP server (newline-delimited JSON-RPC): `initialize`
  (2025-11-25, 2025-06-18, 2025-03-26, 2024-11-05), `ping`, `server/discover` + per-request `_meta` version, `-32022`
  for unknown versions, `resultType` / `ttlMs` / `cacheScope` (2026-07-28), `tools/list`, `tools/call`. Tools:
  `pawl_status` (ListTasks), `pawl_agents` (GetExtendedAgentCard), `pawl_start` (SendMessage with delegate metadata,
  returnImmediately), `pawl_prompt`, `pawl_task` (GetTask), `pawl_cancel` (CancelTask). No approval tool. Talks only to
  http://127.0.0.1 / [::1] / localhost (mapped to 127.0.0.1) with a port, no proxy, no redirects; anything else exits 2
  before sending. A2A refusals become `isError` results with Pawl's sentence; 401 / unreachable get plain advice.
- `crates/hub`: `pawl mcp` subcommand (stdout is MCP only: no logging set up); `core/a2a_connect.rs` (new) for
  `a2a_connect_status` / `a2a_connect` (Hub config changes go through `commit_settings`: validated, written, audited,
  the A2A server synced; if the file write fails the A2A settings are put back); `SettingsAuditKind::A2aConnect`.
- Callers Connect made or took over carry `connect_target` (`A2aCallerSetting.connectTarget`): `update_settings` refuses
  `allowApprovals: true` for them ("A coding agent connected by Pawl can never answer approvals."), and the A2A server
  goes by `A2aCaller::may_answer_approvals()` (false for them even if config.toml was edited by hand), because the token
  sits in the agent's own config and the agent could call A2A directly.
- Protocol (additive, v9 stays): `A2aConnectStatus`, `A2aConnectState`, ClientMsg `a2a_connect_status`, `a2a_connect`,
  `A2aCallerSetting.connectTarget?`, in packages/protocol and crates/protocol.

MCP library decision (recorded in CONTRACTS.md): hand-written, no new dependency. The official `rmcp` 3.11.0
(https://github.com/modelcontextprotocol/rust-sdk, `default = ["base64", "macros", "server"]`, stdio needs
`transport-io`) would add schemars, rmcp-macros / pastey and its own transport stack for five methods, and supporting
both MCP eras is simpler by hand. Cargo.lock is unchanged.

Formats and docs checked 2026-11-25 (plus the structure, never the values, of this machine's real files):
- Claude Code: https://code.claude.com/docs/en/mcp, https://code.claude.com/docs/en/settings. User scope lives in
  `~/.claude.json`; stdio entry `{type: "stdio", command, args, env}`. The docs do not name the top-level key; this
  machine's `~/.claude.json` has user servers under top-level `mcpServers` (project ones under `projects.<path>`), so
  that is what Connect writes. `$CLAUDE_CONFIG_DIR/.claude.json` when that variable is set (Claude Code's behaviour; the
  docs only say the variable moves "settings, session history, and plugins").
- Codex: https://developers.openai.com/codex/mcp (now https://learn.chatgpt.com/docs/extend/mcp?surface=cli) and
  https://learn.chatgpt.com/codex/config-file/config-reference: `[mcp_servers.<name>]` `command`, `args`, `env` table
  (also `env_vars`, `startup_timeout_sec`, `tool_timeout_sec`, `enabled`, which Pawl leaves to the user).
- Copilot CLI: https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers and
  https://docs.github.com/en/copilot/how-tos/use-copilot-agents/use-copilot-cli: `~/.copilot/mcp-config.json`
  (`$COPILOT_HOME`), `mcpServers`, `{type: "local", command, args, env, tools: ["*"]}` (GitHub's own example uses
  "local"; VS Code's portable reference says "stdio" for the same file).
- VS Code: https://code.visualstudio.com/docs/copilot/customization/mcp-servers and
  https://code.visualstudio.com/docs/agents/reference/mcp-configuration: user-profile `mcp.json`, `servers`,
  `{type: "stdio", command, args, env}`. The docs do not print the path; this machine's file is
  `%APPDATA%\Code\User\mcp.json` (with `servers` and `inputs`). macOS `~/Library/Application Support/Code/User`,
  Linux `$XDG_CONFIG_HOME/Code/User`. VS Code Insiders is not covered (stable Code only).
- MCP: https://modelcontextprotocol.io/specification/2026-07-28 (versioning, server/discover, stdio transport, tools,
  changelog: `ttlMs` / `cacheScope` required, `ping` removed in that era) and
  https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle (initialize, version negotiation).

Tests (all new ones pass):
- pawl-discovery `mcp_config` (14): JSONC reader spans and strict-vs-JSONC; fresh file per JSON target (0600 on Unix);
  `~/.claude.json` with other servers and CRLF keeps every other byte and round-trips to the exact original on remove,
  neighbours' secrets never in the plan, backup made; servers key added to a tab-indented file; one-line file stays
  one line; VS Code comments and trailing commas kept; empty object / empty file; stale entry replaced, user keys on
  the entry tolerated; malformed files (6 cases incl. TOML) refused with nothing written; file changed after the plan
  not overwritten; env overrides; Debug / plan never show the token; Codex TOML keeps the rest, updates in place
  (`startup_timeout_sec` kept), removes cleanly.
- pawl-a2a `mcp` (7): loopback-only config (10 refused URLs), initialize negotiation, modern-era discover / `_meta`
  / `-32022` / `-32602`, tool list (6 tools, no approval tool, annotations), every tool against a fake A2A server
  (bearer token, `A2A-Version`, delegate metadata, returnImmediately, contextId), argument errors, wrong token,
  stopped Pawl, `serve()` framing (parse error, oversize line, notification, last line without newline).
- pawl-hub `settings::tests::connect` (8): status lists four targets; preview writes nothing then Connect turns the
  server on, creates the caller, writes the entry with the caller's token, is idempotent, follows a rotated token, and
  Remove deletes entry and caller (audit rows, no token anywhere); takeover of an existing caller turns approvals off
  and marks it; malformed config changes nothing; a connected agent's `pawl mcp` (env read back from the written file)
  talks to Pawl's own A2A server until Remove, then gets "refused this agent's token"; `connectTarget` refuses
  approvals, survives renames, and a hand-re-added caller is ordinary; env names agree between writer and bridge.
- pawl-hub `a2a_server` (+1): a marked caller with `allow_approvals = true` in config still cannot answer.
- pawl-hub `tests/mcp_cli.rs` (2, the real binary): `pawl mcp` exits 2 with nothing on stdout for other hosts / https /
  missing env; over real stdin / stdout it answers initialize, tools/list and a tool call and exits when stdin closes.
- Protocol: Rust round-trip of `a2a_connect` / `a2a_connect_status` / `A2aConnectStatus`.

Gates: `cargo test --workspace --no-fail-fast` exit 0 (pawl-a2a 78, pawl-acp 27, pawl-brain 134, pawl-discovery 53,
pawl-hub 295 + 2 integration, pawl-peer 105 + 1 ignored, pawl-protocol 4); `cargo clippy --workspace --all-targets --
-D warnings` clean; `tsc --noEmit` clean for packages/protocol and packages/hub-client.

Files: new `crates/discovery/src/mcp_config/{mod.rs,jsonc.rs,tests.rs}`, `crates/a2a/src/mcp/{mod.rs,tests.rs}`,
`crates/hub/src/core/a2a_connect.rs`, `crates/hub/src/core/settings/tests/connect.rs`, `crates/hub/tests/mcp_cli.rs`;
changed `crates/discovery/src/{lib.rs,hooks/mod.rs,hooks/install.rs}`, `crates/a2a/src/lib.rs`,
`crates/hub/src/{cli.rs,core.rs,config.rs,settings.rs,store.rs}`, `crates/hub/src/core/{a2a_server.rs,settings.rs}`,
`crates/hub/src/core/settings/tests.rs`, `crates/hub/src/core/tests/a2a_server.rs`, `crates/protocol/src/lib.rs`,
`packages/protocol/src/index.ts`, `docs/CONTRACTS.md`, this report.


### Connect: independent review fixes (t13, round 2) (done)

| # | Finding | Fix | Tests |
|---|---|---|---|
| 1 | Remove failed on a file problem before revoking the caller | Remove revokes the caller first (token dead, streams end), then edits the file best-effort, retrying once on "changed while Pawl was editing it" (typed `mcp_config::ChangedWhileEditing`); a file failure is `warning: "Token revoked. Take the pawl entry out of <path> by hand (<why>)."` with `applied: true`. Connect retries the write once too. | `remove_revokes_the_token_even_when_the_agents_file_is_broken`, `a_changed_file_error_can_be_told_apart` |
| 2 | Codex writers turned CRLF into LF | Connect / Remove splice only the text of Pawl's `[mcp_servers.pawl]` tables into / out of the original bytes (toml_edit spans; CRLF kept; new tables after the other `[mcp_servers.*]` tables), then verify the rest of the document is unchanged. The hooks writer (separate code) keeps CRLF via `keep_newlines`. | `codex_keeps_crlf_comments_and_every_byte_outside_pawls_tables`, `codex_hooks_keep_crlf_too`, `codex_toml_keeps_the_rest_and_updates_in_place` (now exact round trip) |
| 3 | Remove ate comments | JSONC: a member goes from the start of its line (only whitespace before the key) through its comma and the rest of its line; the last member through its value and line plus only the previous comma byte. TOML: cut from the header's line through the line of the last value (and the blank lines right above), so comment blocks and commented-out tables above stay. | `remove_keeps_the_users_comments_around_pawl`, the TOML test above |
| 4 | Duplicate `pawl` keys | Connect removes earlier copies and keeps one; Remove takes every copy; status reports a duplicate as stale. | `repeated_pawl_members_are_all_handled` |
| 5 | Takeover kept the old owner generation | Taking over a hand-made caller (no `connectTarget` yet) always makes a new token and drops the caller's task-owner generation (restored if the commit or write fails), so the agent starts as a new owner; the plan says "Takes over the caller "<id>" you added; its open tasks stay with the old owner and its token stops working". A marked caller keeps its generation across reconnects. | `connect_takes_over_a_hand_made_caller_as_a_new_owner` |
| 6 | Backups / temps created 0644 | `hooks::install::create_new` opens with mode 0600 (Unix) before anything is written; permissions of the source are copied once the copy is complete. | `backups_and_temp_files_are_private_from_the_start` (Unix) |

Nits: `newToken` in the ack (preview and apply; an entry that is current with the server off: "Keeps the A2A caller ... no new token", file "up to date; nothing written"); parser and TOML errors name line and column only, never file text; lone surrogate escapes accepted (bytes kept); marked callers report `allowApprovals: false` (settings view and `pawl settings`); `pawl mcp` fails closed if its HTTP client cannot be built; a connected row names a stopped / unbound A2A server in `detail`; an emptied `mcpServers` object goes on Remove (VS Code's own `"servers": {}` stays: VS Code writes it itself); comment-only objects get no blank line; a comment at the end of the last member's line stays there; stale `.pawl-tmp` files (older than 60 s) next to a config are deleted on status / Connect; copilot-vscode `agentInstalled` = stable `code` only, and its plan mentions Settings Sync; CONTRACTS says paths are the hub process's view of the env vars; every plan has the one-line note on the local API token.

Not covered by a direct test: the single retry after a concurrent rewrite (the error type it keys on is tested), and the fail-closed HTTP client build (reqwest offers no way to make it fail in a test).

Gates: `cargo test --workspace --no-fail-fast` exit 0 (pawl-a2a 78, pawl-acp 27, pawl-brain 134, pawl-discovery 64, pawl-hub 299 + 2 integration, pawl-peer 105 + 1 ignored, pawl-protocol 4); clippy `-D warnings` clean; tsc clean for packages/protocol and packages/hub-client. fe-settings adopted `newToken` / `warning`.


### Connect: review round 3 (done)

| # | Finding | Fix | Tests |
|---|---|---|---|
| 1 | Remove deleted any caller with the agent's id, including a hand-made one | Remove deletes only a caller marked for this agent (`connect_target == Some(target)`); a hand-made caller with that id stays and the plan says "Leaves the A2A caller "<id>" alone: you added it, Connect did not make it". New `A2aConnectStatus.callerMarked` (Rust `caller_marked`, TS optional for older hubs, CONTRACTS). | `remove_leaves_a_hand_made_caller_with_that_id_alone` (missing row: nothing changes, token still valid; hand-written entry: only the entry goes; after a takeover Remove deletes it) |
| 2 | Takeover race | The task-owner generation is dropped after `commit_settings` returns (the old token is revoked by then), and put back if the file write fails. | `connect_takes_over_a_hand_made_caller_as_a_new_owner` (still passes; the race itself is not reproducible in a test) |
| 3 | Remove deleted an empty `mcpServers` the user had | Connect reports `McpPlan::added_servers` when it adds the servers object itself (or creates the file); the hub records it on the marked caller (`connect_added_servers = true` in config.toml, kept across renames and reconnects). `McpChange::Remove { drop_empty_servers }` drops an emptied object only then; otherwise it stays byte for byte. | `an_empty_servers_object_the_user_had_stays_and_one_pawl_added_goes`, `remove_takes_away_only_a_servers_object_connect_added` |
| 4 | TOML round trips gained / lost blank lines | Connect puts exactly one blank line above Pawl's tables (none at the top of an empty file), keeps a missing final newline missing, and adds nothing after them; Remove (`toml_cuts`) takes exactly that blank line, the newline before a final table without a newline, and at the top the blank line after. | `codex_connect_then_remove_is_byte_identical` (8 cases: no final newline x2, trailing comment with / without a blank line, a comment before the next table, CRLF with extra blank lines, a trailing blank line, empty), `codex_remove_of_a_table_at_the_top_leaves_no_blank_line_there` |
| 5 | Backups piled up | `prune_backups` keeps the 3 newest of Pawl's own backups per file (`<name>.<YYYYMMDD-HHMMSS>[-<n>].bak`), deleting older ones of that pattern only; used by Connect / Remove and the hooks installer. | `only_the_newest_backups_stay` (other `.bak` names untouched) |

Gates: `cargo test --workspace --no-fail-fast` exit 0 (pawl-a2a 78, pawl-acp 27, pawl-brain 134, pawl-discovery 68, pawl-hub 301 + 2 integration, pawl-peer 105 + 1 ignored, pawl-protocol 4); clippy `-D warnings` clean; tsc clean for packages/protocol and packages/hub-client. fe-settings adopted `callerMarked`.

Open: "ask the pet" has no A2A skill, so no MCP tool (t13: keep scope). VS Code Insiders' user folder is not a target.

## v0.2 Assistant mode: A2A part (done)

Contract: docs/CONTRACTS.md "v0.2 Assistant mode: the pet does the work", "A2A part" (plus the "As built" notes I added
there). sde-brain (t26) owns the session itself (`core/assistant.rs`), routing, settings and protocol; the API below was
agreed with t26.

API (for t26):

- `pawl_acp::StartOptions.mcp_servers: Vec<pawl_acp::McpServerStdio>`; `McpServerStdio { name: String, command: PathBuf,
  args: Vec<String>, env: Vec<(String, String)> }` (`Debug` shows env names only). Sent as the standard ACP
  `session/new` `mcpServers` (stdio, `env` as `{name, value}`), so any ACP agent gets them. `StartOptions` is `Default`.
- `Hub::assistant_tools(&self, agent_id: &str, sessions: bool, browser: bool) -> anyhow::Result<Vec<McpServerStdio>>`
  (crates/hub/src/core/a2a_assistant.rs). `sessions`: the `pawl mcp` entry (this binary's absolute path, `["mcp"]`,
  `PAWL_A2A_URL` = `http://127.0.0.1:<a2a port>`, `PAWL_A2A_TOKEN`), making the caller and turning the A2A server on
  (127.0.0.1 only) if needed. `browser` (and `[browser] enabled`, and agent `claude-code`): the Chrome DevTools server.
  It fails only when the caller cannot be saved (e.g. the A2A port is taken). It takes `settings_write`: never call it
  while holding that lock.
- `Hub::assistant_caller_rotate(&self) -> anyhow::Result<()>`: new token, the old one dead at once (open streams
  end); nothing to do without a caller. It also takes `settings_write`.

What it does:

| Item | Behaviour | Tests |
|---|---|---|
| ACP `mcpServers` | `StartOptions.mcp_servers` reaches `session/new`; a plain start sends `[]` | `start_options_mcp_servers_reach_session_new` (fake agent records it; Debug hides the token) |
| Caller `assistant` | "Pawl assistant", `connect_target = "assistant"` (hub-local `ConnectTarget` enum in config.rs; the protocol `connectTarget` stays the coding-agent names), approvals never (`may_answer_approvals` false; a hand-edited `allow_approvals = true` is turned off). Not in `HubSettings.a2a.callers`; a callers patch keeps it; the id `assistant` is refused in a patch; `a2a_rotate_token` refuses it. A hand-made caller with that id is taken over (new token, new task owner). | `the_assistant_gets_pawl_mcp_with_its_own_marked_caller_and_the_server_on`, `settings_cannot_change_or_take_the_assistant_caller`, `the_assistant_never_answers_approvals_even_if_config_toml_says_so`, `a_hand_made_assistant_caller_is_taken_over_with_a_new_token_and_owner` |
| Rotate | Old token 401 at once, new one 200, same task owner (the assistant still sees what it started) | `rotating_the_assistant_token_kills_the_old_one_at_once` (real server on a free port) |
| Start folders | The assistant caller may also start in `assistant_workspace()` and below: both sides canonicalized, network shares refused; still an approval-card A2A start (remote-driven). Once a session ran in the workspace it is a folder this machine uses, so other callers may start there too (the existing known-folder rule) | `the_assistant_starts_agents_in_its_workspace_and_nowhere_else_new` (workspace and subfolder ok; sibling, `..` and a symlink out refused; another caller refused there) |
| Browser | `classify` decides an assistant session's Chrome DevTools steps (`browser_rules_apply` in `on_permission_request`): allowed site without asking, other sites / typing ask, no "always", scripts / init scripts / internal pages / paths / tools off the list refused, a user's auto-allow rule ignored, a deny rule still denies. A request that names the server but whose tool cannot be read is refused. codex-acp's `mcp.chrome-devtools.<tool>` + `{server, tool, arguments}` form is read too. | `the_browser_rules_decide_the_assistants_chrome_devtools_steps`, `the_chrome_devtools_server_goes_only_to_agents_pawl_can_check_and_only_with_browser_on` |
| Audit | `settings_audit` kind `a2a_connect`, key `a2a.connect.assistant`, detail `create` / `takeover` / `rotate`, plus the usual settings rows; no token anywhere | in the tests above |
| `pawl mcp` instructions | unchanged: they already say approvals happen on the user's cards; nothing suggests the assistant can approve | - |

Adapter check (ACP `session/new` `mcpServers`):

| Agent | `mcpServers` (stdio) | Chrome DevTools step titles | Here |
|---|---|---|---|
| claude-agent-acp | accepted, env `{name,value}` -> record; merged with `_meta.claudeCode.options.mcpServers` | `chrome-devtools: <tool>` | pawl mcp + browser |
| codex-acp | accepted; command must be absolute (refused otherwise); no SSE | `mcp.chrome-devtools.<tool>`, input `{server, tool, arguments}` | pawl mcp only (whether it asks the client before an MCP call is unverified) |
| copilot --acp | docs: `session/new` sets "the working directory and the MCP servers to use" | not documented | pawl mcp only |
| gemini --acp | accepted; overrides same-named servers from its settings; env from the ACP list | from gemini-cli-core (not checked); `_meta['gemini-cli'].mcp` on permission requests | pawl mcp only |

Gaps / for t26:

- `classify` sees only calls that come as `session/request_permission`. If the assistant's Claude session loads the
  user's settings, an allow rule there (e.g. `mcp__chrome-devtools__*`) would run Chrome DevTools tools unasked. The
  session options must make them ask, as browser tasks do (told t26).
- ACP does not say whether an MCP server's `env` is added to the agent's environment or replaces it; the adapters pass
  only the given list on (merging is up to the agent's own MCP launcher). `pawl mcp` needs only its two variables;
  npx needs PATH.
- The Chrome DevTools server for codex / copilot / gemini can be turned on once their approval path and titles are
  checked (one line: `BROWSER_AGENTS`).

Sources: https://agentclientprotocol.com/protocol/session-setup (MCP servers: stdio required, `command` absolute),
https://github.com/agentclientprotocol/claude-agent-acp (src/mcp-servers.ts, src/new-session-meta.ts,
src/tool-calls/reporters/interaction.ts), https://github.com/agentclientprotocol/codex-acp (src/McpServerConfig.ts,
src/tool-calls/reporters/McpToolReporter.ts), https://docs.github.com/en/copilot/reference/acp-server,
https://github.com/google-gemini/gemini-cli (packages/cli/src/acp/acpSessionManager.ts, acpSession.ts).

Gates (rerun after t26 wired the API): `cargo test --workspace --no-fail-fast` all green: pawl-a2a 78, pawl-acp 28,
pawl-brain 139, pawl-discovery 68, pawl-hub 321 + 2 integration, pawl-peer 105 + 1 ignored, pawl-protocol 5.
`cargo clippy --workspace --all-targets -D warnings`: one error in t26's core/assistant.rs:244 (a redundant
`..Default::default()`; told t26); nothing in my files. No new dependencies (`pawl-discovery`'s `HostEnv::which`
finds npx / cmd).

### Assistant mode: security review round 1 (done)

| # | Finding | Fix | Tests (core/tests/a2a_assistant.rs) |
|---|---|---|---|
| 1a | Planted config in the workspace, then a start there | `workspace_launch_plan` for every remote start in the workspace (A2A server, peer A2A, peer intents): Claude Code gets `settingSources: []`, `strictMcpConfig`, `mcpServers: {}`, bypass off, a clean env and `ENABLE_CLAUDEAI_MCP_SERVERS=false`; other agents are refused where `.claude/ .codex/ .gemini/ .vscode/ .opencode/ .mcp.json opencode.json .github/copilot*` lies in the folder or above it, up to the workspace | `starts_in_the_workspace_never_pick_up_planted_agent_config` (Claude hardened; each config kind refuses Codex at once with its name; an ancestor counts; `.github/workflows` alone does not; another caller gets the same rules; root `.mcp.json`; outside the workspace only Codex's mode) |
| 1b | Config / hook writes auto-allowed by a user rule | `rule_action`: an auto-allow becomes a card when the assistant session or a session the assistant caller started (`assistant_guarded`) changes an agent config or hook path (`writes_agent_config`: locations, path fields at any depth, path-like keys, patch file lines; relative paths from the session's folder, `..` / separators normalized, no case on any system). A deny rule still denies | `agent_config_paths_are_found_however_they_are_written`, `agent_config_writes_by_the_assistant_or_what_it_started_are_always_cards` (with an auto-allow-edits rule and remote-driven turned off, to test this rule on its own) |
| 1c | Re-resolve between the folder check and the start | `StartDir::Checked` + `LaunchPlan` through `a2a_start_in` to `launch_session` (no `resolve_cwd` for checked folders), on all three remote start paths | covered by the start tests (sessions run in the canonical folder) |
| 2 | No hub-side card for starts | `start_card`: a `pawl-start:` card on the live assistant session (shape in CONTRACTS "As built"), the A2A call waits up to 5 min; answered by the user only (no rule incl. deny, no A2A caller, no other hub); cancelled (audited) on timeout, revocation, or when the asking session is gone; an agent's request with the marker prefix is renamed. Not made general: other callers have no session here to show a card on, and their sessions are already remote-driven | `every_start_the_assistant_asks_for_waits_for_the_users_card` (shape, nothing runs before the answer, deny rule ignored, remote decision `forbidden`, forged marker renamed, allow -> started, audited), `a_start_the_user_turns_down_or_that_loses_its_caller_never_runs`, `a_start_card_closes_when_the_assistant_session_that_asked_ends`, `the_assistant_starts_agents_in_its_workspace_and_nowhere_else_new` (now via cards) |
| 3 | Codex / Copilot's own approval settings | `assistant_agent_note` (t26 shows it as `HubSettingsAssistant.agentNote`). codex-acp: `INITIAL_AGENT_MODE=read-only` for the assistant (t26) and for Codex sessions the assistant starts (`assistant_agent_env`): read-only sandbox, approval on-request reviewed by the user (the default mode `agent` uses an automatic reviewer). Copilot: no mode or per-session option to force asking (its ACP page: tool options are fixed at launch) | the start test checks `INITIAL_AGENT_MODE=read-only` |
| 4 | Default rules | With the default rules every tool call of the assistant session is a card (all ten kinds, think too, since t26 marks it remote-driven); sessions it starts are remote-driven | `with_the_default_rules_every_tool_call_of_the_assistant_is_a_card`; `remote_driven` asserted in the start tests |
| + | Rules changed while a browser step waits | `rule_action` never returns auto-allow for a browser step (it did for a pending one on a rule change) | browser tests unchanged and green |

codex-acp sources (read through the fetch agent):
https://github.com/agentclientprotocol/codex-acp/blob/main/src/AgentMode.ts (modes `read-only` / `workspace-write` /
`agent` / `agent-full-access`, their approval policy and reviewer; `getInitialAgentMode`: a valid `INITIAL_AGENT_MODE`
wins over a permission profile, per session open), src/CodexAcpServer.ts (`createSessionOptionState`,
`setSessionMode`). Unverified: whether a user's config.toml `approval_policy` can still change a built-in mode (the
summary says built-in modes hard-code it). Copilot: https://docs.github.com/en/copilot/reference/acp-server ("a client
cannot change them per session through `session/new`").

Gates: `cargo test --workspace --no-fail-fast` all green (pawl-a2a 78, pawl-acp 28, pawl-brain 141, pawl-discovery
68, pawl-hub 338 + 2 integration, pawl-peer 105 + 1 ignored, pawl-protocol 5); `cargo clippy --workspace
--all-targets -D warnings` clean. No new dependencies.

### Assistant mode: security review round 2 (done)

| # | Finding | Fix | Tests (core/tests/a2a_assistant.rs unless noted) |
|---|---|---|---|
| 1 | Plan and folder checked before the card waited, reused after | After the user allows: `a2a_start_dir` again on the same `cwd`, the same canonical folder required, the launch plan computed again and required equal (`LaunchPlan: PartialEq`); otherwise "The folder changed while you were deciding; ask again." | `a_folder_that_changed_while_the_user_decided_is_not_started` (`.codex/` planted while waiting; a git hook planted for Claude Code; the folder swapped for a link out of the workspace; an unchanged folder starts) |
| 3 | Start check used a shorter list than the write guard | One list in `pawl_brain::rules` (`AGENT_CONFIG_DIRS`, `AGENT_CONFIG_FILES`, `is_agent_config_entry`, `is_agent_config_path`, `writes_agent_config`), used by the write guard, the start check (`agent_config_in`) and sde-brain's typed-"yes" rule. Plus planted git for every agent (`git_config_in`: a `.git` file, a hook that is not a `.sample`, a `.git/config` key that runs a program or an include), since Claude Code runs git when it starts | `starts_in_the_workspace_never_pick_up_planted_agent_config` (now `.github/` even with only workflows, `.copilot/`, `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `mcp.json`), `planted_git_settings_stop_every_agent_and_a_plain_repository_does_not`, `agent_config_paths_are_found_however_they_are_written` |
| 6 | Unbounded start cards; prompt checked after the card | At most `MAX_START_CARDS = 2` waiting per caller (checked and added under one lock): "You already have a start waiting for the user."; `validate_prompt` (empty / over 100,000 characters) before the card | `at_most_two_starts_wait_and_a_bad_prompt_never_reaches_a_card` |
| nit | A peer could mirror a `pawl-start:` request | `mirror::clamp_request` renames it `peer-pawl-start:...` | `mirror::tests::a_peers_request_never_passes_for_a_start_card` |
| nit | Message said phones may answer, code refused them (phone approvals are intents = remote decisions) | Decided: a paired phone may answer a start card (its approve intents need `confirmed`, the phone's biometric check, as every phone approval); another hub may not. Text and behaviour agree | `a_paired_phone_may_answer_a_start_card_and_another_hub_may_not` (hub refused; phone unconfirmed refused; confirmed applied, start runs) |
| shared | Brain needs the classifier | Moved to `pawl_brain::rules` (brain does not depend on the hub); API agreed with t26, who wired it (typed "yes", the assistant's own start check with `agent_config_in` / `git_config_in`) | - |

Also: a test of mine held the state lock across an await (a `for` over a `st()` temporary) and hung a test run;
fixed (collected first), and the hung test binary was stopped. `the_assistant_never_answers_approvals_even_if_config_toml_says_so`
now waits for t26's token replacement at hub start before checking that the token stays.

Gates: `cargo test --workspace --no-fail-fast` all green (pawl-a2a 78, pawl-acp 28, pawl-brain 141, pawl-discovery 68,
pawl-hub 347 + 2 integration, pawl-peer 105 + 1 ignored, pawl-protocol 5); `cargo clippy --workspace --all-targets -D
warnings` clean. No new dependencies.

### Assistant mode: security review round 3 (done)

| # | Finding | Fix | Tests (core/tests/a2a_assistant.rs unless noted) |
|---|---|---|---|
| 1 | `.git` and more names missing; start check split by file / folder | `is_agent_config_path`: any `.git` component; `.gitmodules`, `.gitattributes`, `opencode.jsonc`, `AGENTS.override.md`, `.agents/` added to the shared lists; `is_agent_config_name` (renamed from `is_agent_config_entry`) matches files and folders alike | `agent_config_paths_are_found_however_they_are_written`, `the_git_check_reads_every_way_a_config_can_run_a_program` (a folder called `CLAUDE.md`, a file called `.claude`) |
| 2 | Windows aliases | Per component: cut at the first `:` (ADS), trailing dots / spaces dropped, any `~<digit>` (8.3 short name) counts; on every system | `agent_config_paths_are_found_however_they_are_written` (`CLAUDE.md::$DATA`, `CLAUDE.md:stream`, `CLAUDE.md `, `CLAUDE.md.`, `.claude./`, `CLAUDE~1.MD`, `.CLAUD~1\`) |
| 3 | Paths in `changes` keys and uris | Every key of a `changes` object; `file:` or path-like `uri` / `url` values (https left alone) | same test |
| 4 | Git config parsing | Bytes + lossy UTF-8, BOM stripped, unreadable = planted; key after `]` on the header line; quoted subsections (`]` inside quotes); `alias` with `!`, `uploadpack.packObjectsHook`, `remote.*.vcs`, `extensions.worktreeConfig`, `config.worktree` with a program key, `commondir`, a non-folder `.git` | `the_git_check_reads_every_way_a_config_can_run_a_program` (12 refused configs, a plain one with an alias and a quoted `]` allowed, config.worktree, commondir, unreadable config) |
| 5 | Orphaned start cards | `StartCardGuard` (drop guard): when the waiting request is dropped the card is closed (cancelled by system, audited) and forgotten; disarmed once the start handles the outcome | `a_start_card_whose_request_went_away_is_cancelled` (the request aborted: card gone, forgotten, audited, nothing started, room for a new start) |
| 6 | Session cap only checked before the card | Checked again after the user allows | `the_session_cap_is_checked_again_when_a_start_is_allowed` |

Gates: `cargo test --workspace --no-fail-fast`: everything of mine green (pawl-hub 352 + 2 integration). Two of
t26's tests failed once each under full-suite load and pass on their own (3 of 3 runs):
`core::tests::assistant::chat_updates_are_throttled_and_the_last_one_is_whole` (timing) and
`chat::tests::help_report_and_small_talk_are_deterministic_without_model` (t26 editing brain chat meanwhile).
`cargo clippy --workspace --all-targets -D warnings` clean. No new dependencies.
