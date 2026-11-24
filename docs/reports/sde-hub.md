Renamed to Krypto on 2026-11-23; names below are pre-rename.

# sde-hub report: `crates/hub` (`pet-hub`, binary and lib)

Status: **DONE**.
- `cargo check -p pet-hub --all-targets` passes.
- `cargo clippy -p pet-hub --all-targets` is clean.
- `cargo test -p pet-hub` passes all 33 tests.
- No `todo!()` or placeholder bodies are left.

## Files (all inside `crates/hub/`)

| File | Purpose |
|---|---|
| `Cargo.toml` | Lib `pet_hub` and bin `pet-hub`. Pinned dependencies: axum 0.8.9 (ws), rusqlite 0.43 (bundled), clap 4.6 (derive, env), toml 1.2, chrono 0.4, subtle 2.6, getrandom 0.4, hex, tokio-tungstenite 0.29 (same version axum uses), tracing-appender 0.2. Dev dependency: tempfile. |
| `src/lib.rs` | Module list and `HUB_VERSION` |
| `src/main.rs` | `#[tokio::main]` calling `cli::run()` |
| `src/config.rs` | `Paths` (data dir layout), `HubConfig` and `config.toml` (serde defaults, commented template written on first `serve`), token (32 random bytes as hex, `create_new`, mode 0600 on Unix), `HubEndpoint` (parses `AI_PET_HUB_URL` as http or ws) |
| `src/store.rs` | `Store` over rusqlite: `PRAGMA user_version` migrations, WAL, blocking methods, and `Store::run` (spawn_blocking) for async callers. Tables: `agents`, `sessions`, `events` (`id INTEGER PRIMARY KEY AUTOINCREMENT` = `HubEvent.id`), `pets`, `rules`, `chat`, `audit_log` and `settings`. |
| `src/bus.rs` | `Bus`: a tokio broadcast of `ServerMsg` (capacity 4096) |
| `src/notify.rs` | `HubNotification` builders (stop, error, permission) and session labels. The hub only emits `notification` messages; clients decide how to show them. |
| `src/core.rs` | `Hub`: in-memory cache plus store, ACP and hook feed merge, rules, permissions and audit, pets and growth, chat and brain actions, watchdog, daily report, discovery refresh |
| `src/core/host.rs` | A thin enum seam over `AcpHost`, with a `FakeHost` for unit tests (`#[cfg(test)]`) |
| `src/core/tests.rs` | Message-handling tests against the fake host |
| `src/server.rs` | axum router: `/health`, `/ws` (hello first, Origin allowlist, constant-time token check), `/hooks/{source}` (Bearer token). Binds `127.0.0.1` only. |
| `src/logging.rs` | tracing to stderr plus `data_dir/hub.log` (non-blocking; rotated to `hub.log.1` above 10 MB) |
| `src/cli.rs` | clap CLI, `RemoteHub` WebSocket client, `Conn` (remote or in-process), event printer, permission prompts |

## Behaviour

**Startup**
- Loads agents, sessions (open ones, plus any active in the last 7 days), pets and rules.
- Rules come from hub.db if a client ever called `set_rules`, else from `config.toml` `rules`, else `pet_brain::rules::default_rules()`.
- Prunes events older than `event_retention_days` (default 30).
- Owned sessions left open by a previous hub process are marked ended (`error` if they were mid-turn), because their ACP processes died with that process.

**Client messages:** every `ClientMsg` gets exactly one `ack`. The result shapes match `AckResults`, and the tests check them.
- `start_session`
  - Calls `session/new` (`AcpHost::start_session` with `prompt=None`).
  - Logs a `session_start` event (the host does not emit one).
  - If a prompt was given, calls `AcpHost::prompt`. The host emits the `user_message` event itself; the hub does not log a second copy.
  - `cwd` must be an existing directory. It is made absolute (not canonicalized, so no `\\?\` prefix) and `~` is expanded.
- `prompt` and `cancel` work only on open owned sessions. Hook sessions get a clear error.
- `cancel` cancels any permission requests still pending for that session.
- `resume_session` reuses an open owned session with the same `acpSessionId`.
- `list_sessions` returns open sessions. With `includeEnded` it also returns up to 500 recent sessions from the database.
- `get_events` defaults to the latest 500 events, `sinceId` pages forward, and the limit is capped at 5000.
- `hello` returns a `Welcome`. It includes open sessions, sessions ended within the last 24 h, pets, pending permissions and rules.

**Permissions**
- Every ACP `PermissionRequest` is checked with `pet_brain::rules::evaluate_in(rules, req, Some(session cwd))` (through one helper, `Hub::rule_action`).
- **auto_allow / deny:** answered through `AcpHost::resolve_permission`, then `permission_resolved{by:"rule"}` is broadcast. No `permission_request` and no `request_permission` event are emitted, so the dog doesn't bark at auto-approved reads.
  - If auto_allow finds no allow option, the hub falls back to asking.
  - If deny finds no reject option, the hub answers `cancelled`.
- **ask:** the request goes into pending. The hub logs a `request_permission` event and broadcasts `permission_request` plus an alert notification.
- The pending entry is removed before resolving, so two clients answering at once can't both resolve it. An unknown `optionId` is rejected and the request stays pending.
- **Audit log:** every decision is written to `audit_log`: when, request, session, agent, tool kind, title and paths, `decided_by`, actor (`desktop#3`, `chat:vscode#2`, `rule:auto_allow`, `acp-host`, `hub:session_finished`), option, outcome (`allowed`, `rejected`, `cancelled` or `failed`) and detail.
- **Host-initiated cancels:** when the host emits `Event{PermissionResolved}`, the hub removes the request from pending, audits it, logs the event and broadcasts `permission_resolved` with the event's `by`. If the hub already closed that request, the echo is dropped, so it is never broadcast twice.
- When a session is done, errored or ended, any requests still pending for it are closed with `by: system`.
- The `permission_resolved` event payload is `PermissionResolvedPayload` plus a `toolCall` field.

**Hooks:** `POST /hooks/{source}` runs `pet_discovery::hooks::normalize`, which yields a session keyed by `(agentId, externalId)`.
- The session is upserted: status, title, cwd, `vscode_hook` promotion, ended, and reopened if the same external id comes back.
- The event is logged and the notification is broadcast.
- Alert notifications for the same hook session are deduped within 10 s. This is the Claude PermissionRequest-then-Notification double fire that sde-discovery flagged.
- Hook sessions with no hooks for `hook_session_stale_hours` (default 12) are marked ended by the watchdog tick.

**Pets**
- Each installed agent, and each agent that gets a session, gets `growth::default_pet` (a dog).
- `growth::on_event` runs on every logged event. Changed pets are persisted and broadcast on a 2 s flush, which avoids one `pet` message per streaming chunk.
- `update_pet` validates its input. Names are 1–40 characters.

**Chat**
- The user's line is stored, broadcast (hub-client upserts chat by id, so the requester sees no duplicate) and acked.
- `Brain::handle` then runs in a spawned task with a `BrainContext`: agents, open and recent sessions, about 400 recent events (milestones plus streaming text), pending requests and the pet.
- The resulting `BrainAction` is executed. StartSession ignores an empty prompt, as t13 asked.
- The reply is sent as `ServerMsg::Chat`. If the action fails, the error is appended to the reply text.

**Watchdog and daily report**
- `watchdog.tick` runs every `watchdog.tick_secs` (30 s). Its defaults are 5 min to the first nudge and 10 min between repeats, matching pet-brain.
- The daily report fires once per local date, in the hour after `daily_report_time` (default `18:00`, `"off"` disables it).
  - It is generated by `Brain::daily_report`, with a deterministic summary as the fallback.
  - It is sent as an info notification and stored as a pet chat line.
  - The last report date is stored in `settings`, so a restart doesn't repeat it.

**Server**
- `/ws` rejects any non-empty Origin that is not on the allowlist (`vscode-webview://…`, `tauri://localhost`, `http(s)://tauri.localhost`, `http://localhost:1420`). `null` is rejected.
- The first frame must be `hello` within 5 s.
- A bad token gets `ack{ok:false,error:"unauthorized"}` and then close code 1008.
- After the hello, the connection subscribes to the bus before taking the Welcome snapshot.
- Requests are handled concurrently, so a slow `start_session` doesn't block other requests.
- The server pings every 30 s.
- A lagging client is closed with code 1013 so it reconnects and resyncs.
- `/hooks` also checks Origin, and the token comparison is constant-time.

**CLI**
- `serve` runs the daemon (Ctrl-C or SIGTERM triggers a graceful stop and shuts down the agents).
- `agents [--refresh] [--json]`
  - Goes through the running hub when one is up, otherwise runs `discover` locally.
  - `--refresh` moves the 24 h registry cache aside so the registry is fetched again, and restores the cache if the fetch fails.
- `sessions [--json]`
- `start <agent> --cwd <dir> [--prompt <text>] [--local]`
  - Goes through the running hub when one is up, otherwise drives the agent in-process. In-process runs still log to hub.db.
  - Streams readable events: message and thought text, tools, plans and permission outcomes.
  - Asks on stdin for `ask` permissions: an option number, `y` or `n`. End of input means deny.
  - Ctrl-C cancels the turn; a second Ctrl-C quits.
  - Without `--prompt` it runs an interactive prompt loop.
- `hook <source> [json…]` calls `pet_discovery::hooks::run_hook_cli`, which:
  - accepts argv JSON or stdin
  - prints the stdout reply Codex and Copilot require, even when there is no token or the hub is down
  - forwards with a 2 s cap
  - always exits 0
- `install-hooks <claude|codex|copilot-cli|copilot-vscode> [--apply] [--repo]` runs `hooks::install` with `current_exe()`.
- `token`
- Global options:
  - `--data-dir` (env `AI_PET_HUB_DATA_DIR`)
  - `--port`, which takes precedence over `AI_PET_HUB_URL`, then `config.toml`, then 47800

## Commands run and results

- `cargo check -p pet-hub --all-targets`: OK, once all dependencies existed. Earlier runs failed only while pet-brain's `chat.rs` was mid-rewrite.
- `cargo clippy -p pet-hub --all-targets`: clean. The remaining warnings are in pet-brain.
- `cargo test -p pet-hub`: **33 passed**. They cover store, config, token, endpoint, origin, bearer, CLI parsing, and core message handling with a fake host: ack shapes, ask then user-resolve plus audit, auto allow and deny, `set_rules` re-evaluation, cancel and session-end cleanup, host-initiated cancel dedupe, stop notifications, pet updates, chat ack, resume reuse,, close_session cleanup, and restart closing owned sessions.
- **Binary smoke test:** `serve` with a scratch `--data-dir`, `offline = true` and port 48123.
  - `GET /health` returned `{"ok":true,"version":"0.1.0"}`.
  - The token file is 64 hex characters.
  - `/hooks/claude` with no auth returned 401, a bad Origin returned 403, and a valid request returned 200 `{"ok":true}`.
  - `/ws` with an evil Origin returned 403.
  - `pet-hub hook codex '<notify json>'` and a Claude `Stop` hook on stdin both exited 0 and created sessions.
  - `pet-hub sessions` and `pet-hub agents` went through the hub over WebSocket and printed tables. Discovery found Claude, Codex, Gemini and Copilot.
  - A bad `AI_PET_HUB_TOKEN` was refused with a clear message.
  - `start` with an unknown agent or a missing cwd failed with clear errors.
- **Not run:** a real Claude or Codex ACP turn. That is E2E and paid usage, so it is out of scope. The command for it is `pet-hub start claude-code --cwd <repo> --prompt "..."`.

## Known gaps

- **`AcpHost::close_session` has no client trigger:** the protocol has no "end session" message. The hub uses it (via `Hub::close_session`) only to close owned sessions idle in idle/done/error for `owned_session_idle_hours` (default 12, 0 = never), freeing their agent process. `shutdown()` closes everything when the hub exits.
- **Windows token file perms:** the token relies on the inherited ACL of the user-only `%APPDATA%` directory; no explicit DACL is set. Unix uses 0600.
- **`set_rules` has no broadcast:** the protocol has no `rules` `ServerMsg`, so other clients see new rules only on reconnect (`Welcome.rules`).
- **Notifications are not persisted:** they are broadcast-only, so a client that was offline misses them. The daily report is also stored as a chat line.
- **Daily report timing:** it only fires if the hub runs during the hour after `daily_report_time`.

## Decisions for the orchestrator

1. **Rules precedence:** rules in hub.db, once a client has called `set_rules`, win over `config.toml` `rules`, which win over `default_rules()`. Should `config.toml` rules win instead?
2. **`cancel` by a user:** pending requests get `permission_resolved{by:"user"}`. Automatic cleanups (session done or ended) use `by:"system"`. Host echoes keep the `by` the host sent.
3. **Should a `rules` `ServerMsg` be added** so `set_rules` reaches every client live?
4. **Should an "end session" `ClientMsg` be added** that calls `AcpHost::close_session`?
5. **`AI_PET_HUB_DATA_DIR` / `--data-dir`:** this env var is extra (meant for tests and portable installs). Clients don't know about it, so they would need `AI_PET_HUB_TOKEN` with it.

## t13 follow-ups applied

- `pet-hub hook <source> [args…]` delegates to `pet_discovery::hooks::run_hook_cli`. The source is parsed with `parse_hook_source`, and trailing argv JSON is accepted.
- `BrainAction::StartSession` treats an empty prompt as no prompt.
- The default for `watchdog.repeat_every_ms` is now 10 min.
- `ResolvedBy::System` is used for automatic cancels, and the CLI printer handles it.
- The host's `Event{PermissionResolved}` clears pending, is audited and is broadcast once.
- Rule checks use `pet_brain::rules::evaluate_in` with the session cwd.
- `AcpHost::close_session` is wired in through `Hub::close_session`, which closes idle owned sessions.

## Step-0 security floor (t13, unfrozen task)

- **`set_rules` validates first.** It runs `pet_brain::rules::validate_rules(&rules)` before anything else.
  - On failure the ack is `invalid_rule: <reason>` (for example `invalid_rule: rule "x": toolKind "execute" can never be auto-allowed (...)`) and **nothing changes**: neither the live rules nor hub.db are touched.
  - Empty or duplicate ids also fail with an `invalid_rule:` error.
  - Pending requests are re-checked only after a valid set has been stored.
- **Startup sanitizing.** Rules loaded from hub.db, config.toml or the built-in defaults go through `core::sanitize_rules`. That function keeps a rule only if `validate_rules` passes on it alone and its id is unique and non-empty.
  - Dropped rules are logged with `warn!`, naming their source, ids and reasons.
  - They also raise a `HubNotification` (level `warning`, "Unsafe permission rules ignored"). It is broadcast on the bus and **replayed to every new WebSocket client right after its Welcome**, because no client is connected at startup. It stops being replayed after the next successful `set_rules`.
  - The stored rules are not rewritten, so the user can still fix the source.
- **The rules engine is the only automatic approval path.** I audited every `resolve_permission` call site:
  - Automatic answers happen only in `on_permission_request` and in the post-`set_rules` re-evaluation. Both go through `Hub::rule_action`, which calls `evaluate_in`, and `auto_option`.
  - `close_permission` only ever answers `cancelled`.
  - User decisions come from `resolve_permission` client messages, chat-parsed "approve"/"deny" commands (audited as `chat:<client>`), and CLI stdin. When CLI input closes, the CLI denies.
- **Tests:**
  - `set_rules_rejects_unsafe_rules_and_changes_nothing`: auto_allow for execute, delete, fetch, other and a missing toolKind, plus duplicate ids. Ask and deny rules for any kind are accepted.
  - `startup_drops_invalid_rules_from_the_store`: invalid, missing-kind and duplicate rules are dropped and the notice names them. An execute request still asks the user. A later fix clears the notice.
  - `startup_drops_invalid_rules_from_config`: valid config and the defaults raise no notice.
  - The existing auto-allow tests now use `edit`, a kind that may be auto-allowed.

## Step-0 fix: prompt printed twice

- **Cause:** `AcpHost::prompt` already emits `Event{UserMessage}` for every prompt (pet-acp `host.rs`), and `Hub::prompt` recorded a second copy. So `pet-hub start --prompt X` printed `you: X` twice, and the event log stored the prompt twice.
- **Fix:** the hub no longer records its own `user_message`; the host's event is the only one. As a belt-and-braces guard, the CLI printer also drops a `you:` line identical to the one it just printed. That covers agents that replay the prompt as `user_message_chunk`. A later turn with the same text still prints.
- **Tests:** `prompt_echo_printed_once`, and `start_prompt_events_and_ack_shapes` now feeds the host's user_message events.
