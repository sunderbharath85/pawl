Renamed to Krypto on 2026-11-23; names below are pre-rename.

# sde-acp report: `crates/acp` (`pet-acp`)

Status: **FROZEN** (per t13). Revision 2 (below) is complete and compiles. No code changes
are pending, and none will be made until the new plan is approved.

The `AcpHost` API in CONTRACTS.md is fully implemented on the official
`agent-client-protocol` 5.5.1 SDK (ACP protocol v1), plus `close_session`, which t13 requested in revision 2.

## Revision 2 (t13 decisions 1–3): exactly what changed
**Dependencies.** Platform-specific deps added to `crates/acp/Cargo.toml`, both already in `Cargo.lock`:
- `[target.'cfg(windows)'.dependencies] windows-sys = { version = "0.61.2", features = ["Win32_Foundation", "Win32_Security", "Win32_System_JobObjects", "Win32_System_Threading"] }`
- `[target.'cfg(unix)'.dependencies] libc = "0.2"`

No other dependency changes. Root files, the protocol crates and CONTRACTS.md were not touched.

**Toolchain (local, additive).** I ran `rustup target add x86_64-unknown-linux-gnu` so the `cfg(unix)` code could be type-checked. The Linux check output went to `target/linux-check/`. Undo with `rustup target remove x86_64-unknown-linux-gnu`.

**Decision 1: `ResolvedBy::System`.** Automatic cancels now emit `Event{PermissionResolved, by:"system"}`. This covers:
- turn end;
- the agent withdrawing a request (`$/cancel_request`);
- `close_session`;
- process exit;
- shutdown (shutdown now emits these events too; before, it emitted nothing).

`cancel()` still uses `by:"user"`. No `by:"rule"` is emitted by the host anymore.

**Decision 2: process trees** (`src/spawn.rs`, new `ProcessTree`).
- **Windows:** each agent is put into a new Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` right after spawn. `kill()` calls `TerminateJobObject`; drop calls `CloseHandle`, which kill-on-close turns into a kill as well.
- **Unix:** `Command::process_group(0)` gives the agent its own group; `kill()` calls `killpg(pgid, SIGKILL)`.
- **In the connection task (`host.rs`):**
  - Once the agent has exited (cleanly, or force-killed after the 3 s grace period), the whole tree is killed. Leftover children such as `npx` → `node` or tool subprocesses never outlive the connection.
  - On a forced kill, the tree is killed before the direct child.
- If Job assignment fails, a warning is logged and only the direct child is killed. A grandchild started in the microseconds before Job assignment could escape, because Rust's `Command` cannot spawn suspended; a shell shim never does this in practice.

**Decision 3: `pub async fn close_session(&self, hub_session_id: &str) -> anyhow::Result<()>`** (`lib.rs` and `host.rs`). The steps, in order:
1. Cancel the session's pending permissions (`by: system`).
2. Unregister the session.
3. Upsert it as `done` with `endedAt`.
4. Tell the agent: `session/close` if the agent advertises `sessionCapabilities.close` (10 s timeout), otherwise `session/cancel` if a turn is running.
5. Stop the agent process once it serves no other session (stdin closed, grace period, then tree kill).

Unknown or already-ended ids are a no-op (`Ok`).

To make that safe, a new `OpenGuard` / `ConnShared.opening` counter marks callers that are between `get_or_spawn` and registering their session (`start_session`, `resume_session`, `list_history`):
- `close_session` never stops a connection another caller is about to use. It checks under the connection's slot lock.
- The 5-minute idle reaper respects the counter too.

**Docs.** The crate docs and the `HostEvent::Event` docs now describe `close_session`, the process-tree kill and the `by` values.

**New tests** (22 total, all pass):
- `close_session_cancels_and_stops_process_with_last_session` (in-process fake agent, which now advertises `session/close`):
  - pending permission → `PermissionResolved by:"system"`;
  - the agent receives `session/close`;
  - closing twice is a no-op;
  - the process stays up while another session uses it;
  - closing the last session stops the connection.
- `closing_last_session_kills_process_tree` (real `node` subprocess; skipped if `node` isn't installed): the agent spawns an orphan grandchild, and closing the last session kills it. **Verified non-vacuous:** with the Job Object temporarily disabled, the test failed ("survived"). With it restored, it passed, and the stray process was cleaned up.

**Commands run this revision:**
- `cargo check -p pet-acp --all-targets`: OK, 0 warnings.
- `cargo clippy -p pet-acp --all-targets`: 0 warnings.
- `cargo test -p pet-acp`: 22 passed, three consecutive runs.
- `cargo check -p pet-acp --all-targets --target x86_64-unknown-linux-gnu`: OK. The Unix code type-checks; it was compiled but not run.

## Files
| File | Purpose |
|---|---|
| `crates/acp/Cargo.toml` | Crate deps:<br>• `agent-client-protocol` 5.5.1 (no features)<br>• `tokio-util` 0.7.20 (compat)<br>• `which` 8.0.6<br>• `chrono` 0.4 (no default features)<br>• workspace deps<br>Platform-specific: `windows-sys` 0.61.2 on Windows, `libc` 0.2 on Unix. Dev-dependency: `tracing-subscriber`. |
| `src/lib.rs` | Public API (`HostEvent`, `AcpHost` with the contract's methods plus `close_session`). A compile-time check asserts `Send + Sync + Clone` and that every future is `Send`. |
| `src/host.rs` | Connections, ACP handlers, session state, turns, permissions, history/resume, close and shutdown. |
| `src/convert.rs` | Mapping from ACP schema types to pet-protocol payloads, with unit tests. |
| `src/fs.rs` | The `fs/read_text_file` and `fs/write_text_file` sandbox, with unit tests. |
| `src/spawn.rs` | Program resolution (`which`, PATHEXT, agent `PATH` override), spawning, `ProcessTree` (Job Object / process group), with unit tests. |
| `src/tests.rs` | Host end-to-end tests against an in-process fake ACP agent, plus real Node.js subprocess tests. |
| `examples/smoke.rs` | Runs a given command in a cwd, sends one prompt, prints HostEvents and auto-cancels permissions. |

## What the tests cover (revision 1, still passing)
The flow:
- `initialize` sends the fs/terminal capabilities, and the host emits AgentCapabilities.
- `session/new` → `idle` → prompt → `user_message` → `thinking`.
- Thought chunk, then `tool_call` in progress → `working`.
- A permission request carrying only `toolCallId` gets `kind`/`title` filled from the cached tool call.
- Bad option ids and a second prompt during a turn are rejected.
- After resolve: `fs` write, then read with line and limit, then a read outside the cwd is **denied**.
- The merged `tool_call_update`, then `plan`, then a message chunk.
- `Stop{end_turn}` → `done`, with Stop emitted before the upsert.
- Refusal → `error`.

Other scenarios:
- `cancel()` → `by:user` → `Stop{cancelled}` → `idle`.
- `session/list` mapping.
- `session/load`: the Session upsert comes before the buffered replay.
- Opening the same ACP session twice is rejected.
- The agent dying → Error event plus `error`/`endedAt` for every session on it.
- A real subprocess crash reports the exit code and stderr.
- A non-ACP program fails `initialize`, and a missing binary gives a clear error.
- `npx.cmd` shims run on Windows.

## Design decisions (documented in the crate docs)
- **Process granularity:** one process per (agent id, cwd, command+env), spawned with `current_dir = cwd` and shared by sessions in that cwd. It is stopped when:
  - its last session is closed;
  - it has had no sessions for 5 minutes;
  - the host shuts down;
  - the last `AcpHost` clone is dropped.
- **No LocalSet needed:** the 5.x SDK makes connection futures `Send`, and the agent's stdio goes through `tokio_util::compat` + `ByteStreams`.
- **Spawning:**
  - `tokio::process` with stdio piped, `kill_on_drop`, `CREATE_NO_WINDOW`, and a Job Object / process group.
  - On Windows, `npx` resolves to `npx.cmd` via `which`.
  - `AI_PET_HUB_TOKEN` is removed from the child's environment.
  - The last 20 stderr lines are kept for error messages.
- **Permissions:** waits run off the dispatch loop. `waiting_permission` is upserted before `PermissionRequest` is emitted, so a rule's immediate resolve is not overwritten.
- **Status mapping:** follows CONTRACTS.md, plus:
  - message chunk → `thinking`;
  - all running tools finished → `thinking`;
  - after a permission is resolved: allow → `working`, otherwise `thinking`;
  - Stop `max_*` → `done`;
  - Stop `cancelled` → `idle`;
  - status only changes during a turn.
- **fs sandbox:** paths are canonicalized and must stay inside the canonical cwd. Reads are capped at 32 MiB.
- **Timeouts:** `initialize` 180 s, `session/new`/`load`/`resume` 120 s, `session/list` 30 s per page, `session/close` 10 s.

## Open items for orchestrator / sde-hub
1. The hub should handle `Event{PermissionResolved}` from the host (`by` is `user` or `system`): remove the request from `pending` and broadcast `ServerMsg::PermissionResolved`. At my last look, it only recorded the event.
2. Add `close_session` to CONTRACTS.md (the orchestrator owns that file) so sde-hub can wire it up.
3. Requests the hub resolves itself are not echoed as events, and the host emits no `session_start`/`session_end` events; the Session upsert carries them.

## Known gaps
- **Terminals:** not implemented (advertised `false`).
- **Agent-initiated updates** between the `session/new` response and session registration are dropped. None of them map to events.
- **Not run against real Claude/Codex adapters:** deliberate, to avoid paid usage. Try it with `cargo run -p pet-acp --example smoke -- --cwd <repo> -- npx -y @agentclientprotocol/claude-agent-acp`.
- **Unix process-group code:** type-checked for Linux but not executed (this machine runs Windows).
