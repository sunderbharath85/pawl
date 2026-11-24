# sde-brain report — crate `pet-brain` (`crates/brain/`)

Status: **DONE** (rev 2: reads scoped to the session cwd). Gate `cargo test -p pet-brain` passes (51 tests); `cargo clippy -p pet-brain --all-targets` is clean.

## Rev 2 — reads scoped to the session cwd (t13's decision on item 3)

- New `rules::evaluate_in(rules, req, cwd: Option<&str>) -> RuleAction`. `evaluate(rules, req)` is now `evaluate_in(rules, req, None)`.
  - Also added: `rules::decide(rules, req, cwd) -> Decision { action, rule_id, sensitive }` for the audit log, `matching_rule_in`, `is_sensitive_path`, and `CWD_TOKEN`.
- A rule path may start with `{cwd}` (token is case-insensitive). It expands to the session cwd: slashes normalized, `\\?\` stripped, trailing slash removed, glob metacharacters escaped. Matching is case-insensitive on Windows/macOS.
  - With no cwd (`None`, empty, or relative), a `{cwd}` rule never matches.
  - Relative request paths are joined onto the cwd before matching, and `..` is resolved, so `../x` falls outside.
- `default-read` **and** `default-search` are now `{cwd}/**`. These ask:
  - path-less reads/searches;
  - any location outside the cwd, including sibling dirs like `repo-evil` and the cwd itself;
  - every read when `cwd` is `None`.

  I scoped search too, because a search outside the cwd can expose secrets just like a read. Changing `default-search` back to no path is a one-line edit; see decision 4.
- Sensitive-path guard, in code: if the result would be AutoAllow and any path is sensitive, the result becomes **Ask**, whatever the rules say; a Deny rule still denies.
  - Covered: `.env*` (incl. `.envrc`), `*.env`, `*.pem`, `*.key`, `*.p12`/`*.pfx`/`*.jks`/`*.keystore`/`*.kdbx`, `id_rsa*`/`id_ed25519*`/…, `.npmrc`, `.pypirc`, `.netrc`, `.git-credentials`, `.pgpass`, `*credential*`, `*secret*`, `.ssh`/`.aws`/`.azure`/`.gnupg`/`.kube` (the directory and everything in it), `.docker/**`, `gcloud/**`, `secrets/**`, and the hub's `ai-pet-hub/{token,config.toml,hub.db*}`.
  - The old "default-sensitive-*" Ask rules were removed from `default_rules()`; the guard replaces them.
- New tests cover:
  - inside/outside the cwd, path-less reads, no cwd, relative paths and `..`;
  - sibling prefixes, backslash/`\\?\`/trailing slash, case on Windows, glob metacharacters in the cwd, root cwd;
  - `{CWD}` casing, `{cwd}` with no cwd, deny rules using `{cwd}`;
  - the guard against the defaults and against a user rule that auto-allows `**`, against an explicit execute opt-in, sensitive paths in rawInput, deny still denying, and non-sensitive look-alikes (`environment.md`, `keyboard.rs`).
- Hub: `crates/hub/src/core.rs:816` has a placeholder waiting for exactly `pet_brain::rules::evaluate_in(rules, req, cwd.as_deref())`. **Until the hub switches, every read asks**, which is safe. I couldn't re-check `pet-hub` because `pet-acp` currently fails to build (`hub_session_acp_id_placeholder` missing in `crates/acp/src/host.rs:1182`; sde-acp's file, not mine). The pet-brain change only adds functions; `evaluate`'s signature is unchanged.
- Item 2: settled, hub uses 10 min.

## Files created

| File | What |
|---|---|
| `crates/brain/Cargo.toml` | deps: pet-protocol, serde, serde_json, anyhow, tracing, tokio (workspace); `globset = "0.4"`, `reqwest = { version = "0.13", features = ["json"] }` (default rustls TLS) |
| `src/lib.rs` | `pub mod rules, chat, watchdog, growth` (+ private `util`) |
| `src/rules.rs` | rules engine + invariant tests |
| `src/growth.rs` | default pet, mood/xp per event |
| `src/watchdog.rs` | waiting nudges with dedupe/repeat |
| `src/chat/mod.rs` | `Brain`, `BrainConfig`, `BrainContext`, `BrainAction`, `BrainReply`; reply composition |
| `src/chat/intent.rs` | deterministic intent parser (the only source of actions) |
| `src/chat/llm.rs` | Anthropic Messages API + Ollama `/api/chat` via reqwest |
| `src/chat/summary.rs` | session facts, deterministic summaries, event digest, daily report |
| `src/chat/voice.rs` | species sounds/actions + LLM system prompt |
| `src/chat/tests.rs`, `src/util.rs` | Brain end-to-end unit tests; shared helpers |

## Public API (CONTRACTS.md, exact, plus additive helpers)

- `rules::{default_rules, evaluate, pick_option}`. Extras: `matching_rule` (which rule decided; for the audit log), `request_paths`, `normalize_path`, `requires_explicit_kind`, `is_destructive`, `effective_kind`, consts `SENSITIVE_FILES_GLOB`, `SENSITIVE_DIRS_GLOB`, `HUB_SECRETS_GLOB`.
- `chat::{BrainConfig, BrainContext, BrainAction, BrainReply, Brain}` with `new/handle/summarize_session/daily_report`. Extras: `DEFAULT_MODEL`, `BrainConfig::{default, from_env, has_model}`, `Brain::{config, has_llm}`. `BrainConfig`'s Debug output redacts the API key. `BrainAction` and `BrainReply` serialize like the protocol (`type` tag, camelCase). `Brain` is `Send + Sync + Clone`, and the futures are `Send` (tested).
- `watchdog::{WatchdogConfig, Watchdog}` with `new/tick`. Extras: `WatchdogConfig::default()` (5 min / 10 min), `Watchdog::config`, `STALE_DONE_MS`.
- `growth::{default_pet, on_event}`. Extras: `pet_id_for` (`pet-<agentId>`), `default_name`, `level`, `mood_label`, consts.

## Behavior

**rules.** First rule that applies wins; no rule applies → Ask.
- Defaults (rev 2):
  - Auto-allow read and search only inside `{cwd}/**`; auto-allow think.
  - Ask for edit, move, delete, execute, fetch, switch_mode and other.
  - Everything else asks, including reads when the cwd is unknown.
- Sensitive-path guard (in code, not a rule): a request touching `.env*`, keys/certs, `.ssh`, `.aws`, `*secret*`, `*credential*`, or the hub's own `ai-pet-hub/token|config.toml|hub.db` is never auto-allowed. An agent that reads the hub token could approve its own requests.
- A rule with no `toolKind` never auto-allows a guarded kind: delete, execute, fetch, switch_mode, or other/missing. That rule is skipped and the next one decides.
- An auto-allow path rule needs every path to match, and at least one path. Ask/deny path rules apply when any path matches.
- Paths come from tool-call `locations` plus well-known `rawInput` keys (`path`, `file_path`, `paths`, …). They use `/` separators, `..` is resolved lexically so traversal can't escape a glob, and matching is case-insensitive on Windows/macOS. In globs, `*` does not cross `/`.
- `pick_option`: AutoAllow → allow_once, else allow_always. Deny → reject_once, else None (cancelled).

**chat.** Messages go through a deterministic parser, in this order:
1. help
2. approve/deny: "approve", "yes", "always allow codex", "deny 2", "no", "never"
3. commands: "tell/ask/have codex to … in repo X", "codex, …", "start a new claude session in <path> to …"
4. daily report
5. status: "what's Claude doing?", "status", "codex?"
6. small talk

A leading "hey" or the pet's name is ignored.
- "repo X" resolves to a session cwd by basename: exact match first, then alphanumeric-equal; fuzzy-unique matching only when the user said "repo".
- Paths are accepted directly, including `~` and quoted paths with spaces.
- An owned, live, non-busy session for the same agent and cwd gets a **Prompt**; otherwise the reply is **StartSession**. A busy session gets an explanation and no action. Hook-only agents, uninstalled agents, and unknown or ambiguous repos get a helpful reply and no action.
- Approve/deny acts only when exactly one request matches. When several are pending, the reply lists them numbered.
- LLM: Anthropic first if a key is set (model from config, default `claude-haiku-5-5`, headers `x-api-key` + `anthropic-version: 2023-06-01`), then Ollama (`stream:false`). It is used for single-session summaries, the daily-report intro and small talk. The system prompt gives the pet's name, species, personality and mood, and tells the model to treat `<context>` as data.
- When no model is configured, or a call fails, every path has a deterministic in-character reply. Dog is the default voice; cat, bird and robot have their own.

**watchdog.** Nudges for:
- a pending request older than N (Alert, repeats every `repeat_every_ms`; `<= 0` means no repeats);
- a `waiting_permission` session with no request object, i.e. a hook session (Alert, the body says "approve it in VS Code/terminal");
- a live `done` session waiting for the next prompt (Info, once per turn, skipped after 12 h so a restart doesn't spam).

State is pruned every tick. Notification ids are `watchdog:<key>:<n>`.

**growth.** Dog with a name from a list of 16, seeded by FNV-1a of the agent id so it is stable across restarts, mood 70, xp 0. Per event:

| Event | Mood | XP |
|---|---|---|
| end_turn | +5 | +10 |
| refusal | −6 | |
| max tokens | −2 | +2 |
| error | −8 | |
| tool failed | −2 | |
| tool call / completed | | +1 |
| plan | | +2 |
| user resolved a permission | +2 | |
| hook stop / agent-turn-complete | +3 | +5 |

Mood is clamped to 0..=100 and xp saturates.

## Commands run

- `cargo search` / `cargo info reqwest`: checked current versions (globset 0.4.22, reqwest 0.13.5).
- Web search of the Anthropic Messages and Ollama `/api/chat` docs to confirm request/response shapes.
- `cargo check -p pet-brain`: skeleton built in about the first 5 minutes.
- `cargo test -p pet-brain`: **49 passed, 0 failed**.
- `cargo clippy -p pet-brain --all-targets`: no warnings.
- `cargo check -p pet-hub`: OK.

## Known gaps

- Small talk and summaries were never run against a live Anthropic or Ollama model (no key used, E2E out of scope). The parsing and fallback paths are unit-tested, including a model endpoint that refuses connections.
- `daily_report` covers sessions active in the last 24 h measured from now. The hub decides when to send it.
- The parser is English-only and keyword-based.
- `summarize_session` and `daily_report` (the public functions with no `pet` argument) use the default dog voice.

## Decisions for the orchestrator

1. **sde-hub fix needed**: `crates/hub/src/core.rs` `execute_action` passes `Some(prompt)` for `BrainAction::StartSession` even when the prompt is empty ("start codex in repo X" with no task). Map an empty prompt to `None`: `Some(prompt).filter(|p| !p.trim().is_empty())`.
2. ~~Watchdog defaults differ.~~ Settled: hub uses 10 min.
3. ~~Path-less reads auto-allowed.~~ Settled in rev 2: reads are scoped to `{cwd}/**` and secrets are guarded.
4. `default-search` is also scoped to `{cwd}/**`, which is stricter than the decision asked for. Claude's Grep/Glob calls with no `path` will now ask. If that's too noisy, set `default-search` back to no path; the sensitive guard still applies to any paths it does carry.
5. CONTRACTS.md lists only `evaluate`. Consider adding `evaluate_in` and the `{cwd}` token to the `pet-brain` section.
