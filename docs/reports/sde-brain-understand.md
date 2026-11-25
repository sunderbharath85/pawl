# sde-brain: the pet understands any wording (understand + providers + acp)

Branch `feat/one-pet`, worktree `E:\workspace\ai-pet-onepet`. Owned paths only (`crates/brain`, `crates/hub`); nothing
committed. `apps/` and `packages/` untouched (Settings copy is t29's, see "For Settings" below).

## Ask by default + the default pet's name (rev 5)

**(A) Rules: ask by default.** `pawl_brain::rules::default_rules()`: read and search now ask (were automatic inside
`{cwd}/**`); edit, move, delete, execute, fetch, switch_mode and other ask as before; **think stays automatic**: it
has no side effects (no file, process or network access; it is the agent's own reasoning step). Allowing reads is
the user's choice ("Always allow" on a request, or a rule). `legacy_default_rules()` keeps the old list only to
recognise a stored copy: at start, stored rules that are exactly the old defaults (saved, never customised) are
replaced by the new defaults once, written to hub.db, audited (settings audit row, actor `hub:default-rules`, key
`rules`) and announced with one startup notice "Default rules now ask before reads" (shown to clients that connect
during that run; the next start finds nothing to migrate). Customised sets and config.toml seed rules are left
alone; hubs without stored rules simply get the new defaults. Tests: `defaults_ask_for_everything_but_think`
(path-scoping tests now use the old cwd-scoped rules, still a valid user rule), hub
`never_customised_default_rules_move_to_ask_once`. Docs: `docs/CONTRACTS.md:149` still says "auto_allow
read/search/think" (read-only for me: t13); the desktop Rules view is t29's.

**(B) The default pet's name** (`bot.petName`, `[brain] pet_name`): at start, when unset or empty, the hub picks a
name from the pet-name pool (`growth::default_name`, seeded by machine id) that is not "Pawl" and not any pet's name
(this machine's and paired machines'), saves it to config.toml and announces the settings (not audited: no user
action). Renames: the pet-name rules (trimmed, 1-40 characters, no control / bidi / zero-width characters:
`settings::check_pet_name`), and not another pet's name, case-insensitive (`invalid:bot.petName: "X" is another
pet's name`); a change is broadcast like every setting. The other way round, a pet rename to the default pet's
name is `name_taken`, and new pets never get it. The brain gets it as `BrainContext.pet_name`: with no pet
attached the voice is that name, so the system prompt says "You are <name>, the user's pet", greetings say "Hi!
<name> here." and the help "I'm <name>. I look after all your agents.". Tests: settings
`the_pet_name_follows_the_pet_name_rules`, hub `the_default_pet_is_named_once_and_renamed_by_the_pet_name_rules`
(picked, unique, persisted, same after a restart, renames validated and announced, pets can't take it), brain
`the_default_pet_answers_as_its_name`.

Gates: `cargo test -p pawl-brain -p pawl-hub -p pawl-acp`: brain 110, hub 207, acp 25 passed; workspace clippy clean.

## Plain voice (rev 4)

The user disliked "Woof! Big question! *wags tail* For me, life is belly rubs…". New `bot.style` (`[brain] style`,
`plain` default | `playful`; closed list, validated by the enum; part of the brain rebuild keys; `pawl settings`
line):
- **plain** (`crates/brain/src/chat/style.rs`): the voice's decorations (sounds, `*actions*`) are empty; every
  reply, deterministic or model-written, goes through `style::plain_text`: `*…*` stage directions (1-60 chars,
  starting with a letter, one line) out, leading pet sounds per line out ("Woof!", "Beep boop."; "Bark at the moon"
  stays), every emoji out, spaces tidied, then ONE emoji first from the fixed set, chosen from what the reply is:
  🐾 greeting / general / help / a model's answer, 👀 status overview, ⚠️ something needs the user (pending
  approvals, a refusal, the approval-card hint), ✅ done (an approve/deny that resolved, the daily report), 🤔 not
  sure / asking which, 💤 all quiet (nothing running), 🛠️ a start / prompt (run or proposed). Model replies: the
  plain system prompt asks for 1-3 short, friendly, direct sentences, no role-play, sounds, puns, asterisks or emoji,
  and for off-topic questions one short sentence pointing back to what the pet does; the icon comes from the intent
  the brain handled, never from the model. Plain small talk: "Hi! …", "Thanks!", "You're welcome.", and an
  off-topic question without a model: "That's outside what I can help with. I keep track of your coding agents:
  try “status”, …". "Fetching Codex!" became "Starting Codex in api: …". The daily report and session summaries
  the hub posts are cleaned the same way.
- **playful**: today's voice, no emoji.
- Status overview (both styles): the ones that need the user come first (header counts them); finished sessions
  are grouped instead of listed: "4 finished today: ai-pet ×3, tiny-whisper." (open sessions done with their turn
  plus sessions ended in the last 24 h).

Tests: `style::tests` (actions, sounds, emoji, tidy, one icon), brain
`plain_replies_start_with_one_fitting_emoji_and_no_role_play` (11 deterministic cases with their icon, no `*`, no
sounds, one emoji; off-topic nudge; playful keeps `*…*` and no emoji), `model_replies_are_cleaned_in_plain_style`
(a role-playing model answer and status summary cleaned, the plain system prompt, a model proposal gets 🛠️,
playful untouched), updated status/small-talk tests; hub `the_reply_style_defaults_to_plain_and_is_persisted`,
the acp end-to-end test now expects "🐾 Right here.". Gates: brain 109, hub 204, acp 25 passed; workspace clippy
clean.

## Startup window (rev 3)

The live smoke showed "no language model configured" twice at start and the acp provider only ~0.4 s later, after
agent discovery; a chat in between got "Connect a model in Settings > Bot". Now:
- the hub tracks `agents_discovered` (set when the startup discovery finishes, success or not; true from the start
  for hubs without background discovery). While it is false, the provider allows acp (`auto` / `acp`), the acp
  agent is Claude Code and no other model answers, a chat reply waits for it, at most 5 s (`MODEL_WAIT`), then uses
  the rebuilt brain;
- the brain says "Connect a model in Settings > Bot" only when nothing is configured at all (no key, no Ollama
  model, provider `auto`, no agent session). With a chosen provider that is unavailable it gives the plain reply;
  if the model is still pending after the wait (`BrainContext.model_pending`) it says "My model is still waking up;
  ask me again in a few seconds.";
- logging: the brain logs at debug; the hub logs one "pet model" line once discovery is done (and after each
  change), e.g. `pet model model=acp:claude-code:sonnet`.

Tests: brain `a_configured_model_is_never_reported_missing`; hub
`a_chat_during_startup_discovery_waits_for_the_agent_model` (no reply while discovery runs, then the chat goes to
the agent session and gets its answer). Gates: brain 106, hub 203, acp 25 passed; workspace clippy clean.

## Review fixes (rev 2)

1. **acp is Claude Code only.** `bot.acpAgent` accepts only `claude-code` ("only claude-code can be the pet's
   model for now (its tools can be locked down; other agents' cannot yet)"); `Hub::acp_model()` returns a session
   only for it (an old config naming another agent gets no acp provider). The field stays for later agents.
2. **No tools, the supported way** (t13 approved the `crates/acp` addition): `AcpHost::start_session_with(agent,
   cwd, id, prompt, StartOptions { meta, clean_env })` (additive; `start_session` unchanged for every caller):
   - `meta` becomes `session/new` `_meta`; the brain session sends `{"claudeCode": {"options": {"tools": [],
     "strictMcpConfig": true, "mcpServers": {}, "disallowedTools": [every tool name]}}}`: Claude Agent SDK
     options claude-agent-acp passes through (`src/new-session-meta.ts`: `tools` passed through, `mcpServers` /
     `disallowedTools` merged); SDK docs (https://code.claude.com/docs/en/agent-sdk/typescript): `tools` = the
     tool list (`[]` = none), `strictMcpConfig` = only the given `mcpServers`, ignoring project `.mcp.json`, user
     settings, plugins and claude.ai connectors.
   - `clean_env` spawns the agent (its own connection, keyed apart) with `env_clear()` + only
     `pawl_acp::CLEAN_ENV_KEEP` (PATH, PATHEXT, HOME/USERPROFILE/HOMEDRIVE/HOMEPATH, APPDATA/LOCALAPPDATA/PROGRAMDATA,
     TEMP/TMP/TMPDIR, SYSTEMROOT/SYSTEMDRIVE/WINDIR/COMSPEC, PROGRAMFILES*, USER/USERNAME/LOGNAME, LANG/LC_*/TZ,
     TERM/SHELL, XDG_*, CLAUDE_CONFIG_DIR, HTTP(S)_PROXY/NO_PROXY, NODE_EXTRA_CA_CERTS/SSL_CERT_*) + `agent.env`
     (ANTHROPIC_MODEL, MAX_THINKING_TOKENS=0, ENABLE_CLAUDEAI_MCP_SERVERS=false). OPENAI/GEMINI/GOOGLE keys, hub
     and relay tokens, GitHub/AWS/npm/HF tokens never reach it (also blanked in `agent.env` as defence in depth;
     the Anthropic login is left alone so the user's plan is used).
   - Defence in depth kept and extended: the settings deny list adds PowerShell, KillBash, TaskOutput, TaskStop,
     TaskCreate/Update/List/Get, ExitPlanMode, EnterPlanMode, AskUserQuestion, ToolSearch, ListMcpResourcesTool,
     ReadMcpResourceTool, Monitor, worktree/cron/LSP/SendMessage tools (`"*"` and `"mcp__*"` stay first);
     `disableClaudeAiConnectors: true`, `deniedMcpServers: [{serverUrl: "*"}]`. A permission request is rejected
     AND ends the turn (its answer is discarded); a tool call ends the turn, discards the text so far and cancels;
     either error restarts the session.
3. **Secrets never reach a model.** `util::scrub_secrets` runs inside `context_block`, so it covers every model
   prompt (understanding pack, summaries, daily report, small talk): known key prefixes (`sk-`, `sk_`, `ghp_`,
   `gho_`, `ghu_`, `ghs_`, `ghr_`, `github_pat_`, `glpat-`, `xox[bpars]-`, `xapp-`, `AKIA`, `ASIA`, `AIza`, `ya29.`,
   `npm_`, `hf_`, `pk_live_`, `rk_live_`), the credential after `Bearer` / `Basic` / `Token` / `Authorization:` /
   `X-Api-Key:`-style headers, values of `*KEY*` / `*TOKEN*` / `*SECRET*` / `PASSWORD` / `PASSWD` / `PWD` /
   `AUTH` / `CREDENTIAL` / `COOKIE` / `SESSION` names (`export X=…`, `X: …`), `--password` / `--token` /
   `--api-key` arguments, user info of URLs (`https://user:pass@host`), and base64/hex runs of 24+ chars with
   letters and digits. Command requests reach the model only as kind + program ("R1: Claude Code wants to: run curl
   (kind execute)", also in the session line): `understand::program_of` drops `VAR=value` prefixes, `sudo`/`env`
   wrappers, labels, quotes and folders.
4. **ACP output cap**: a reply over 64 KiB ends the turn (error) and cancels it.
5. **HTTP body cap**: provider responses over 1 MiB are refused: a declared `Content-Length` before reading,
   otherwise while streaming (`read_capped`).
6. **Fast defaults**: OpenAI `reasoning_effort` = the lowest documented value per family (`none` for gpt-6*, as
   the GPT-6 Luna page lists `none`; `minimal` for gpt-5*; `low` for o-series; others unchanged), per the Chat
   Completions reference ("none, minimal, low, …; not all reasoning models support every value"). Gemini
   `generationConfig.thinkingConfig` = `{thinkingLevel: "MINIMAL"}` for Gemini 3 and later (the reference:
   "Recommended for Gemini 3 or later; use with earlier models results in an error"), `{thinkingBudget: 0}` for 2.5
   Flash, nothing for others.

Tests: `util::tests::secrets_are_scrubbed` (each pattern, and ordinary text kept), `understand::tests::
commands_and_secrets_never_reach_the_model` (Bearer header, URL password, `export KEY=`, `ghp_`, `xoxb-`, `AKIA`,
`TOKEN=` absent; only "run curl"), `programs_of_commands`; llm `request_bodies_follow_each_api` (reasoning effort,
thinking config) and the fake-server test (an oversized body is refused); acp `start_options_meta_reaches_session_new`
and `spawn::tests::clean_env_drops_inherited_variables` (a secret var of the parent is gone, PATH and the agent's
own env stay, nothing else inherited); hub `the_acp_brain_session_is_locked_down_and_never_a_session` (start
options: no tools + clean env; blanked keys; a permission request is rejected, ends the turn, restarts the session;
a tool started without asking discards the text and cancels; the 64 KiB cap), `only_claude_code_can_be_the_acp_provider`,
settings (`acpAgent: codex` refused with the message).

Gates after the fixes: `cargo test -p pawl-brain -p pawl-hub -p pawl-acp`: brain 105, hub 202, acp 25 passed;
`cargo clippy --workspace --all-targets -- -D warnings` clean. `crates/acp/src/spawn.rs` keeps its CRLF endings.

## What it does

1. **Settings** (`crates/hub/src/config.rs`, `settings.rs`): the new `HubSettings.bot` fields are stored under
   `[brain]` like the Anthropic key/model: `provider` (auto | anthropic | openai | gemini | ollama | acp),
   `openai_api_key` / `gemini_api_key` (write-only: never in `HubSettings`, errors, logs, `pawl settings` or Debug;
   `null` clears), `openai_model` / `gemini_model` / `acp_model` (`""` = the default, validated like the other
   model ids), `acp_agent` (must be a known agent, like `defaultAgent`; a removed custom agent resets it),
   `understand` (default `true`). Env keys work like `ANTHROPIC_API_KEY`: `OPENAI_API_KEY`, `GEMINI_API_KEY` count as
   set while config.toml has none and show up in `overriddenBy`. Unknown provider strings fail the patch's
   deserialization (closed enum). Any bot key change rebuilds the brain; a provider / acp agent / acp model change
   also closes the agent session. config.toml template documents them.
2. **Providers** (`crates/brain/src/chat/llm.rs`, plain reqwest): Anthropic, OpenAI (new), Gemini (new), Ollama,
   and `acp` (new, see 6). One provider answers: the chosen one if configured (chosen but not configured = no
   model; never a silent switch to another provider), or with `auto` the first configured in the order anthropic,
   openai, gemini, ollama, then acp when its agent is installed. So with no keys and Claude Code installed, the pet
   uses Claude Code + Sonnet. `Llm::complete` (text) and `Llm::complete_json` (structured output) per provider.
3. **Understanding** (`crates/brain/src/chat/understand.rs`, wired in `Brain::handle`): when the parser's intent is
   small talk / unknown, `understand` is on and a model is configured, ONE call turns the message into one intent
   with a strict JSON schema (flat object, every field required, `null` when unused: `kind` ∈ status, question,
   command, prompt, resolve, help, smalltalk; `agent`, `machine`, `repo`, `pet`, `session` (S#), `task`,
   `decision`, `which` (R#), `answer`). `validate` accepts only names the hub knows: agents by id/name/handle,
   machines by name, repos only as index names (anything with `/ \ : ~ ..` is refused), pets by name, sessions and
   requests only as S#/R# references of the pack; tasks bounded (1000 chars, control chars out). Then:
   - status: answered deterministically from hub data (no second model call);
   - question / smalltalk: the model's `answer` as reply text (cleaned, ≤ 700 chars);
   - command / prompt: ALWAYS a `ChatProposal`, also on this machine and also when the model named the agent or pet
     (local prompt proposals carry the session id; remote ones as before); `Brain::handle` drops any action on this
     path and keeps local proposals;
   - resolve: never resolves; "Tap Approve on the card above if you want Codex to run bun test." Deterministic
     approve words keep today's rules;
   - help: the help text.
   Errors, timeouts, non-JSON or refused answers give the deterministic reply (no second call). The typing state is
   the hub's, unchanged (one call per message, bounded by the provider timeout: 30 s cloud, 60 s Ollama, 45 s acp).
   With `understand` off, small talk is worded as before.
4. **Better replies without a model** (`summary.rs`, `mod.rs`): no raw hook names. A hook "Notification idle_prompt"
   makes the line "Claude Code in tiny-whisper is waiting for you in the terminal." (permission prompts: "waiting
   for your OK in the terminal"); other hooks read "Most recently it finished its turn / used Bash: ls / said: …".
   An agent's other sessions are grouped: " Also: tiny-whisper (working). 3 finished earlier: ai-pet ×2,
   tiny-whisper. 1 idle: web." Titles in the pack and card hints are cleaned (`understand::clean_title`; the hub
   cleans stored titles too). The "not sure" reply is now "I didn't catch that. Try “status”, “fix the failing
   test in repo X”, or “what's Claude doing?”." and, while no model is configured, says ONCE per brain (hub run or
   settings change) "Connect a model in Settings > Bot and I'll understand any wording."
5. **Privacy**: see the section below.
6. **acp provider** (`crates/hub/src/core/brain_session.rs`): the brain gets `pawl_brain::chat::ModelSession`
   (`complete(system, user, timeout) -> text`), implemented by the hub's `AcpModel`. The hub runs a hidden,
   long-lived ACP session with the installed `acp_agent` (default claude-code) on the user's own login (their plan's
   usage; no API key):
   - model through the adapter's documented order: env `ANTHROPIC_MODEL` (wins over settings) = `acp_model`
     (default `sonnet`), plus `settings.model`; `MAX_THINKING_TOKENS=0` turns extended thinking off for speed;
   - cwd = `<data dir>/brain-session/` (empty), with `.claude/settings.json` rewritten at every start:
     `permissions.deny = ["*", "mcp__*", "Bash", "BashOutput", "KillShell", "Edit", "MultiEdit", "Write", "Read",
     "Glob", "Grep", "LS", "WebFetch", "WebSearch", "Task", "Agent", "NotebookEdit", "NotebookRead", "TodoWrite",
     "SlashCommand", "Skill"]`, `disableAllHooks: true`, `enableAllProjectMcpServers: false`, `model`; no MCP servers
     are passed;
   - every permission request of that session is rejected (reject option, else cancel); a tool call it starts
     anyway ends the turn with an error, cancels it and the session is restarted;
   - it is never a hub session: started straight on the host (ids `<machine>:brain-<uuid>`), every host event of it
     is routed to the turn collector before the normal handling, so it is never stored, broadcast, mirrored, given a
     pet, listed, or counted in status; hook reports whose cwd is in its folder are dropped;
   - one turn at a time, per-turn timeout (45 s; timeout or error = close + restart next time), warm between turns,
     a fresh session every 20 turns (small history), closed after 10 min idle; the brain is rebuilt after agent
     discovery so the provider exists only while the agent is installed.
   Instructions go into the user turn (the host has no system-prompt override; Claude Code keeps its own system
   prompt); for JSON the schema is put in the instructions and parsed leniently (fences / `<think>` stripped).

## Defaults and docs read

- OpenAI default **`gpt-6-luna`**: https://developers.openai.com/api/docs/models ("GPT-6 Luna for cost-sensitive,
  high-volume workloads", $0.10 / $0.50 per MTok), https://developers.openai.com/api/docs/models/gpt-6-luna (Chat
  Completions endpoint, Structured outputs supported). Request shape: https://developers.openai.com/api/docs/guides/structured-outputs
  (`response_format: {type: "json_schema", json_schema: {name, strict: true, schema}}`), reference
  https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create
  (`max_completion_tokens`, `max_tokens` deprecated; `message.refusal`).
- Gemini default **`gemini-3.5-flash-lite`**: https://ai.google.dev/gemini-api/docs/models ("our fastest, most
  cost-effective 3.5 model", stable), https://ai.google.dev/gemini-api/docs/models/gemini-3.5-flash-lite (Structured
  outputs supported). API: https://ai.google.dev/api/generate-content (`POST /v1beta/models/{model}:generateContent`,
  `x-goog-api-key`, `systemInstruction`, `generationConfig.maxOutputTokens`, `responseMimeType` +
  `responseJsonSchema`), https://ai.google.dev/gemini-api/docs/generate-content/structured-output (type arrays with
  `"null"`, `enum`, `additionalProperties`). https://ai.google.dev/gemini-api/docs/migrate-to-interactions says
  generateContent "remains fully supported"; it is kept because it is stateless (the Interactions API stores
  conversations server-side by default).
- Anthropic JSON output: https://docs.claude.com/en/docs/build-with-claude/structured-outputs (`output_config.format
  = {type: "json_schema", schema}`, JSON in the text block; no beta header).
- Ollama: https://github.com/ollama/ollama/blob/main/docs/api.md (`format` takes a JSON schema).
- ACP adapter: https://github.com/agentclientprotocol/claude-agent-acp (README; `src/session-model.ts`: model from
  `ANTHROPIC_MODEL`, then `settings.model`; `src/session-env.ts`: `MAX_THINKING_TOKENS`; `src/settings.ts`: reads
  user/project/local/managed settings incl. `<cwd>/.claude/settings.json`).
- Claude Code settings: https://code.claude.com/docs/en/settings, https://code.claude.com/docs/en/settings-reference
  (`permissions.deny`: `"*"` denies every tool, `"mcp__*"` every MCP tool; `disableAllHooks`; `deniedMcpServers`;
  `ANTHROPIC_MODEL` overrides `model`).

## Privacy: what goes to a model (for Settings)

Only when a model is configured. For Anthropic / OpenAI / Gemini it goes to that company's API with the user's key;
for Ollama to the given URL; for acp to Claude Code on this machine, which sends it to Anthropic under the user's
Claude login (their plan's usage).

The understanding call (a message the parser did not understand) sends:
- the user's message (≤ 1000 chars) and the pet's persona (name, species, personality text, mood);
- machine names and online state; installed agent names per machine; repo folder names per machine (index names,
  never paths); pet names with their agent and machine;
- up to 12 sessions: pet name, agent, machine, repo folder name, state in words (incl. the current tool's title, an
  error message ≤ 60 chars, or the pending tool title), the cleaned session title, whether it needs the user, and the
  agent's last message (≤ 160 chars);
- up to 6 pending approvals: agent, machine, tool title, tool kind.
It never sends file contents, raw hook or tool JSON (`rawInput`, transcript paths, env), paths, ids, API keys or
tokens. The agent's last message and titles are what the agent wrote, so they can mention a file name.

The other model calls (unchanged since earlier steps, also only with a model): a session summary (that session's
facts plus up to 30 recent event lines: messages, tool titles), the daily report text, and small talk with
`understand` off (the session overview).

## For Settings (t29)

- `bot.provider` options, `openaiApiKeySet` / `geminiApiKeySet` (set-only key fields), models (blank = default:
  gpt-6-luna, gemini-3.5-flash-lite, sonnet), `acpAgent` (default claude-code), `understand` (default on).
- The acp option must say it uses the user's own Claude plan (Claude Code's login), no API key, no tools; the CLI
  line reads "acp agent  claude-code (uses your own plan, no tools)".
- The privacy text above.

## Files

- brain: `chat/llm.rs` (providers, `ModelSession`, request/response shapes), `chat/understand.rs` (new: schema,
  instructions, pack, validation, `clean_title`), `chat/mod.rs` (`BrainConfig` fields + defaults, `Brain::with_session`,
  understanding path, `act_on`, `session_proposal`, `card_hint`, `others_line`, once-only hint, new "not sure"
  reply), `chat/summary.rs` (hook phrasing, `hook_waiting`, placeholder titles), `chat/tests.rs`.
- hub: `config.rs` (`BrainSection` fields, env keys, effective defaults, template), `settings.rs` (keys, view,
  validation, config.toml writes, `Overrides::with_env`, `BRAIN_KEYS`, CLI lines), `core/brain_session.rs` (new),
  `core.rs` (state, routing, `rebuild_brain`, hook drop, loop), `core/settings.rs`, `core/host.rs` (fake records
  started agents), `cli.rs`, tests.

## Commands (in the worktree)

- `cargo test -p pawl-brain -p pawl-hub`: brain 102 passed, hub 201 passed, 0 failed.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean (`apps/desktop/src-tauri` is outside the workspace).

Tests: `llm::tests` (provider selection incl. auto order and acp last, request bodies per API, canned replies,
refusals and blocked prompts, fake OpenAI and Gemini servers with auth headers, acp gets the schema in its
instructions), `understand::tests` (strict schema, validation of bad ids / paths / unknown agents / machines / repos /
pets / out-of-pack references, pack has names and states and never raw JSON, secrets or paths), brain
`a_model_read_command_is_always_a_proposal`, `a_model_read_approval_never_approves`,
`bad_model_answers_fall_back_to_the_deterministic_reply` (incl. an unreachable model),
`model_answers_and_status_come_back_as_text`, `without_a_model_replies_are_plain_and_hint_once`; hub
`bot_providers_are_validated_persisted_and_never_shown`, `the_acp_brain_session_is_locked_down_and_never_a_session`
(env model, locked settings file, permission rejected, output collected, nothing stored or broadcast, warm reuse,
tool call ends it, hooks from its folder dropped), `a_silent_brain_session_times_out_and_restarts`,
`the_default_pet_reads_free_form_messages_through_the_agent_session` (end to end: a "yes" read by the model never
approves; the card hint).

## Decisions

1. Understanding replaces the small-talk wording call (one model call per message); a model-read status is
   answered from hub data.
2. A chosen provider that is not configured means no model (deterministic replies), not another provider.
3. "Says once" = once per brain instance (hub run or settings change); the brain is otherwise stateless.
4. acp turns put the instructions in the user turn; history is capped by restarting every 20 turns.
5. Model-read prompts for a busy session explain instead of proposing (a deterministic "tell X to …" still proposes
   a new session in its folder).

## Gaps

- The acp tests drive the hub's fake host at the host boundary (permission requests, chunks, stop, tool calls,
  timeouts); a spawned fake agent process would need `crates/acp` test internals (not my path). Live Claude Code was
  not run here.
- User-level MCP servers can still start inside Claude Code (no name wildcard in `deniedMcpServers`); their tools are
  denied by `"mcp__*"` and every permission request is rejected.
- The host exposes no ACP `session/set_model`; the model is set by env (documented) and settings.
- Older paired hubs may still send raw titles; the brain cleans what it shows, the desktop (t29) cleans display.
