# sde-discovery report: `crates/discovery` (`pet-discovery`)

Status: **DONE**. `cargo test -p pet-discovery` passes: 31 tests. `cargo clippy -p pet-discovery --all-targets` is clean. No `todo!()` stubs are left.

## Files created (all inside `crates/discovery/`)

| File | Purpose |
|---|---|
| `Cargo.toml` | Adds reqwest 0.13 (rustls, json, http2), toml_edit 0.25, similar 3 and serde_json `preserve_order`. tempfile is a dev-dependency. |
| `src/lib.rs` | `CustomAgent`, `DiscoveryConfig`, `discover()`, built-in agents, registry-to-`Agent` resolution, `build_agents()` (pure) and `binary_install_dir()` |
| `src/registry.rs` | Tolerant model of the real registry, the 24h cache (`registry-cache.json` with ETag), `load()` and `platform_keys()` |
| `src/detect.rs` | `HostEnv`: PATH index with Windows `PATHEXT`, `which()`, npm global roots, npx cache scan and package `bin` lookup |
| `src/hooks/mod.rs` | Contract API (`NormalizedHook`, `HookTarget`, `normalize`, `forward`, `install`) plus helpers for the hub CLI (`run_hook_cli`, `read_input`, `stdout_reply`, `parse_hook_source`, `HookTarget::from_cli`) |
| `src/hooks/normalize.rs` | Normalizers for Claude Code, Codex (`notify` and lifecycle hooks) and Copilot (camelCase and VS Code-compatible payloads) |
| `src/hooks/install.rs` | JSON/TOML merging, timestamped `.bak` backups, atomic writes, unified-diff dry run and `install_with_home()` |
| `src/hooks/tests.rs` | Unit tests using sample payloads taken from the vendor docs. Install tests run only in temp dirs. |
| `examples/discover.rs` | `cargo run -p pet-discovery --example discover -- <cache_dir> [--offline]` prints the agents as JSON |
| `examples/install_hooks.rs` | Dry-runs or applies hook installation against a scratch home directory |

## Research (current docs, checked 2026-11-23)

- **ACP registry** (`registry.json`, `version: "1.0.0"`, 46 agents). It has the fields `id`, `name`, `version`, `description`, `icon`, `authors`, `license`, `repository` and `website`, plus `distribution`, which is one or more of:
  - `npx {package, args?, env?}`
  - `uvx {package, args?, env?}`
  - `binary {<os>-<arch>[-musl]: {archive, cmd, args?, env?, sha256?}}`
- **Registry ids differ from our stable ids.** They are folded in as follows:

  | Registry id | Our id | Command source |
  |---|---|---|
  | `claude-acp` | `claude-code` | `@agentclientprotocol/claude-agent-acp` |
  | `codex-acp` | `codex` | `@agentclientprotocol/codex-acp`. The old `@zed-industries/codex-acp` is deprecated on npm. |
  | `gemini` | `gemini` | `@google/gemini-cli --acp`. The flag is now `--acp`, not `--experimental-acp`. |
  | `github-copilot-cli` | `copilot` | `@github/copilot --acp` |

  Because of this, ACP sessions and hook sessions share one agent id, and so one pet.
- **Claude Code hooks** (code.claude.com/docs/en/hooks):
  - Common fields are `session_id`, `cwd`, `hook_event_name`, `transcript_path` and `permission_mode`.
  - Notable event fields: SessionStart has `source` and `session_title`; Stop and SubagentStop have `last_assistant_message`; Notification has `notification_type` (for example `permission_prompt` or `idle_prompt`); SessionEnd has `reason`.
  - Newer events are also handled: `PermissionRequest`, `PostToolUseFailure` and `StopFailure`.
  - SessionEnd hooks share a 1.5 s budget, so we set `timeout: 3`.
  - Origin comes from the `CLAUDE_CODE_ENTRYPOINT` env var (`claude-vscode`) and `TERM_PROGRAM`.
- **Codex:**
  - `notify = [argv…]` passes the JSON as the last argv. The payload has `type: agent-turn-complete`, `thread-id`, `turn-id`, `cwd`, `input-messages` and `last-assistant-message`.
  - Codex also has Claude-compatible lifecycle hooks, configured inline under `[hooks]` or in `$CODEX_HOME/hooks.json`.
  - These hooks need the user to trust them via `/hooks`.
  - `Stop` and `Interrupt` require JSON on stdout. SessionEnd allows at most 3 s.
- **Copilot** (CLI and VS Code share one harness and format):
  - Files are `{"version":1,"hooks":{event:[{type,bash,powershell,env,timeoutSec}]}}`.
  - camelCase event names give camelCase payloads *with no event name*. PascalCase names give snake_case payloads with `hook_event_name`. We install PascalCase names and also set the env var `AI_PET_HOOK_EVENT`.
  - User hooks live in `~/.copilot/hooks` (or `$COPILOT_HOME/hooks`). Repo hooks live in `.github/hooks`.

## Behaviour summary

**`discover()`**
- Registry loading order:
  1. If the cache is less than 24 h old, use it without any network call.
  2. Otherwise send a GET (10 s timeout, `If-None-Match` ETag). A 304 refreshes the cache timestamp.
  3. If that fails, use the stale cache.
  4. If there is no cache, use the built-in list.
  - `offline` skips the network entirely.
  - The env var `AI_PET_REGISTRY_URL` overrides the URL.
- Output order: the 4 built-ins, then the other registry agents in registry order, then custom agents. A custom agent with an existing id replaces that entry and gets `source: custom`.
- Command resolution, in order:
  1. A registry binary unpacked in `cache_dir/agents/<id>/<version>/`.
  2. That binary's name on PATH.
  3. A global npm install's bin.
  4. `npx -y <pkg@ver> args`.
  5. The uvx tool on PATH, else `uvx <pkg> args`.
  - `command[0]` is resolved to an absolute path, for example `C:\Program Files\nodejs\npx.cmd`. Rust's `Command` on Windows doesn't search for `.cmd` files itself.
- `installed` is true when the agent is actually runnable or set up. That means one of:
  - its binary is found (cached or on PATH)
  - it is installed globally
  - it is in the npx cache
  - for built-ins, the vendor CLI or home dir is present (`claude` or `~/.claude`, and likewise for codex, gemini and copilot)

  Having npx alone does **not** mark all 23 npx agents as installed; that would give 23 dogs.
- Live smoke run on this machine: 46 agents, 6 installed. Claude resolves to the global `claude-agent-acp.cmd`, Copilot to the winget `copilot.exe --acp`, and OpenCode to `~/.bun/bin/opencode.exe acp`.

**`hooks::normalize`**
- Status mapping follows CONTRACTS.md. Extra mappings:

  | Event | Status |
  |---|---|
  | PermissionRequest, or Notification `permission_prompt` / elicitation | `waiting_permission`, plus an alert notification |
  | StopFailure, or a Copilot non-recoverable ErrorOccurred | `error` |
  | Interrupt | `idle` |
  | SubagentStart / SubagentStop | `working` |

- The title is `session_title` if present, otherwise the latest prompt (first 80 characters).
- Large strings in `payload.raw` are cut at 4000 characters and arrays at 200 items, so file contents from `Write` don't bloat the event log.
- Returns `None` when the input isn't a JSON object or has no recognisable event.

**`hooks::forward`**
- POSTs to `{hub}/hooks/{claude|codex|copilot}` with `Bearer <token>`, a 2 s total timeout and no proxy.
- It adds `"_ai_pet": {term_program, claude_entrypoint, codex_originator, hook_event, vscode_process_type, vscode_ext_host}` to the body. These come from a fixed allowlist of env vars, so no secrets are sent. `normalize` reads this block to tell VS Code sessions from terminal ones.

**Session origin (`detect_source`, per t13's decision to follow PLAN.md)**
- Only the vendors' VS Code extensions count as `vscode_hook`. Every terminal CLI counts as `cli_hook`, including one running in VS Code's integrated terminal.
- Rules, checked in this order (first match wins):
  1. **`vscode_hook`** if any of these is true:
     - Claude `CLAUDE_CODE_ENTRYPOINT` contains `vscode` (that is, `claude-vscode`).
     - Codex `CODEX_INTERNAL_ORIGINATOR_OVERRIDE` contains `vscode`.
     - `VSCODE_CRASH_REPORTER_PROCESS_TYPE=extensionHost`, meaning an extension spawned the process. This covers Copilot agent hooks.
  2. **`cli_hook`** if `TERM_PROGRAM=vscode`, meaning the integrated terminal.
  3. **`vscode_hook`** if the extension-host markers are present: `VSCODE_PID`, `VSCODE_IPC_HOOK`, `VSCODE_NLS_CONFIG`, `VSCODE_HANDLES_UNCAUGHT_ERRORS` or `VSCODE_ESM`/`AMD_ENTRYPOINT`.
  4. **`cli_hook`** otherwise, including when the `_ai_pet` block is missing.
- Variables that only the terminal sets (`VSCODE_IPC_HOOK_CLI`, `VSCODE_GIT_IPC_HANDLE`, `VSCODE_INJECTION`) are never treated as signs of VS Code.

**`hooks::install`**
- A dry run returns a unified diff and notes. Apply writes `<file>.<YYYYMMDD-HHMMSS>.bak` and then writes the file atomically.
- Our entries are recognised by the hub exe name plus `hook <source>`, so re-running replaces them in place. Running it twice changes nothing and makes no new backup.
- Other hooks, settings, TOML comments and formatting are kept. Invalid JSON or TOML is refused, never overwritten.
- Per target:
  - **Claude:** 11 events in `settings.json` (honours `CLAUDE_CONFIG_DIR`). The command is a plain forward-slash path, or exec form (`args`) when the path has spaces.
  - **Codex:** sets `notify` only if it is free or already ours. Otherwise it keeps the user's notify and adds a `Stop` hook. Lifecycle hooks go into `hooks.json` if that file already exists, else inline into `config.toml`.
  - **Copilot:** writes `~/.copilot/hooks/ai-pet.json` or `<repo>/.github/hooks/ai-pet.json`, with both `bash` and `powershell` commands, quoted correctly for each.

## Commands run

- `curl` of the registry, Claude, Codex and GitHub doc pages into the scratchpad (read only).
- `cargo check -p pet-discovery --all-targets`: OK.
- `cargo clippy -p pet-discovery --all-targets`: clean.
- `cargo test -p pet-discovery`: **31 passed**.
- `cargo run -p pet-discovery --example discover -- <scratch dir>`: live registry, 46 agents.
- `cargo run -p pet-discovery --example install_hooks -- codex|claude <exe> <scratch home>`: dry run only.
- **No real user config files were read for writing or modified.**

## For sde-hub (integration notes)

- **`pet-hub hook <source>` must accept trailing positional args.** Codex `notify` runs `pet-hub hook codex '<json>'` and passes the JSON as the last argv. With clap, use something like `#[arg(trailing_var_arg = true)] rest: Vec<String>`. Then call `pet_discovery::hooks::run_hook_cli(source, &rest, &hub_url, &token).await`. It:
  - reads the argv JSON or stdin (never blocks on a TTY)
  - prints the stdout reply that is required: `{}` for Codex lifecycle hooks and for Copilot
  - forwards with the 2 s cap
  - never errors

  Use `hooks::parse_hook_source(&str)` for the argument.
- **`install-hooks <target> [--apply]`:** call `HookTarget::from_cli(name, std::env::current_dir()?)` and then `hooks::install(target, &std::env::current_exe()?, apply)`, and print the returned string.
- **`POST /hooks/:source`:** call `hooks::normalize(source, body)`. Ignore the `_ai_pet` key otherwise; it stays in `payload.raw`.
- **Alert dedupe (suggested):** Claude can send `PermissionRequest` immediately and then `Notification(permission_prompt)` about 6 s later for the same prompt. Suppress a second `waiting_permission` alert for the same session within about 10 s.

## Decisions for the orchestrator

1. **Resolved by t13: follow PLAN.md.** The VS Code integrated terminal now counts as `cli_hook`, and only the vendor extensions count as `vscode_hook`. Implemented in `normalize.rs::detect_source` and `env_meta`, with tests for both sides. See "Session origin" above.
2. **Approved by t13: the `serde_json` `preserve_order` feature.** It keeps users' `settings.json` key order when merging. It applies to the whole workspace through feature unification. Value equality is unaffected, but map serialization order now follows insertion order everywhere.
3. **`installed` semantics** are as described above (runnable or set up, not just "npx exists"). Confirm this is what the UI and pet creation should use.
4. **Copilot personal hooks:** per the VS Code docs, `~/.copilot/hooks` covers Copilot in VS Code too. So the `copilot-cli` target probably serves both, and `copilot-vscode` (a repo file) is optional. The repo file holds this machine's absolute hub path, and the dry-run notes tell the user to gitignore it.

## Known gaps

- **No downloader or unpacker for registry `binary` archives.** We only detect binaries already unpacked at `binary_install_dir(cache_dir, id, version)`, a helper we expose. `sha256` is parsed but not yet used. Agents that ship only as binaries (goose, cursor and others) are found if they are on PATH, otherwise they show as not installed.
- **Codex's shell for hook `command` on Windows is undocumented.** We use an unquoted forward-slash path, which works in cmd, PowerShell and bash. When the hub path needs quoting we skip the lifecycle hooks and only set `notify`, and the output says so.
- **Claude exec-form hooks** (used only for hub paths with spaces) need a recent Claude Code 2.1.x. This machine has 2.1.331.
- **The extension-host env markers are VS Code internals and have not been checked against a running VS Code.**
  - The vendor signals (Claude's entrypoint, Codex's originator) are checked first, so they are the main path for those two.
  - There is one misclassification edge case. Suppose VS Code itself is started with `code .` from inside a VS Code integrated terminal. The extension host then inherits `TERM_PROGRAM=vscode`, and Copilot hooks without the `extensionHost` process type are labelled `cli_hook`.
- **Not verified live:** that each hook fires in each editor build. That is E2E and out of scope. PLAN.md already notes that Claude's VS Code extension fires fewer hooks.
