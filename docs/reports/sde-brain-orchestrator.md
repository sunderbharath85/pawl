# sde-brain: the default pet orchestrates every agent

Branch `feat/one-pet`, worktree `E:\workspace\ai-pet-onepet`. Owned paths: `crates/brain`, `crates/hub` (settings and the
chat glue). Nothing committed; nothing outside those paths touched.

## Session titles (rev 3)

The sidebar showed titles like `[Image #40]`, `<task-notification> <task-id>bt7…`, `<\pasted_content id=…> <hc fr…`:
the hub took them from the raw prompt. New pure module `crates/hub/src/titles.rs`:

- `from_prompt(raw) -> Real(text) | Pasted | Nothing`: strips `<task-notification>`, `<system-reminder>`, `<hc from=…>`,
  `<pasted_content …>` (also the escaped `<\pasted_content …>…<\/pasted_content>`), Claude Code's
  `<command-name>` / `<command-message>` / `<command-args>` / `<local-command-stdout|stderr>` /
  `<user-prompt-submit-hook>` blocks (tag names case-insensitive; an unclosed block, e.g. cut by a length limit, runs
  to the end), and `[Image #N]` / `[Pasted text #N …]` placeholders; control characters and whitespace runs become
  single spaces; at most 80 chars, cut on a char boundary with "…". Only pasted content left = `Pasted text`. Other
  tags and brackets (`<div>`, `[WIP]`) stay.
- `fallback(agent, cwd)` = "Claude Code in tiny-whisper" (agent name alone while the folder is unknown);
  `is_placeholder` (none, fallback, "Pasted text", or a title cleaning would still change); `next(current, candidate,
  overwrite, …)` decides.
- Hook sessions (`ingest_hook`): the hook's title, else (prompt events) the full prompt text from the payload when the
  80-char clipped title is all injected text. Real text follows the latest prompt as before; injected-only prompts
  and tool hooks never change it; pasted-only fills a placeholder only. Every hook event already persists and
  broadcasts the session.
- Owned sessions (`merge_session`): the host's title is cleaned (a raw "[Image #1]" from the agent's first prompt
  becomes the fallback); a `user_message` event upgrades a placeholder title to the first real prompt and broadcasts
  the session (`retitle_from_prompt`); a real title is never replaced by injected text; an agent-set real title is
  followed.
- Hub start: `Store::sessions_with_injected_titles` (this machine's sessions whose title starts with `<` or
  `[Image`, any age) are cleaned once and written back; clean titles are not touched, so later starts read nothing.
- The brain's deterministic summary no longer quotes a placeholder title ("Claude Code in ai-pet (“Claude Code in
  ai-pet”)").
- Display-side cleaning (hub-client, new session view) is t29's; `apps/` and `packages/` untouched.

Tests: `titles::tests` (each injected kind alone and mixed with text, unclosed blocks, pasted, escaped forms, caps and
control chars, fallback/next rules, retitle candidates), `hook_session_titles_skip_injected_text` (fallback until a
real prompt, broadcast, tool hooks keep it, full prompt used past the clipped title), `owned_session_titles_skip_injected_text`,
`stored_injected_titles_are_cleaned_at_start`. Gates: `cargo test -p pawl-hub -p pawl-brain` hub 195, brain 93 passed;
workspace clippy `-D warnings` clean. (Two llm.rs items for the next task carry a temporary `allow(dead_code)`; the
understanding step uses them.)

The protocol's new bot fields (provider, OpenAI/Gemini keys and models, acp agent/model, understand) are already
persisted and validated in `settings.rs`/`config.rs` so the hub compiles; their tests and the rest come with the
understand task (`docs/reports/sde-brain-understand.md`).

## Review fixes (rev 2)

1. **HIGH: agent-less commands never run by themselves.** When the user names no agent or pet (the default pet's
   "fix the failing test in repo api", "start a session in repo api", "tell this machine to …", "tell the Laptop to …"),
   the brain picked the agent, so the reply is always a `ChatProposal`, also for this machine: `Ready: Start Claude
   Code in api: fix the failing test. Confirm it to go ahead.` (a free open session gets a `Prompt` proposal with its
   `sessionId`). `Brain::handle` keeps local proposals only for those commands and drops any action for them
   (`debug_assert` + clear); every other command keeps the old invariant (local = action, remote = proposals).
   Named-agent and named-pet commands ("tell Claude to …", "tell Biscuit to …") run as before. A path outside this
   machine's repo index is refused for agent-less commands (`I only pick an agent for folders I know. I know: … Or
   name the agent: “tell claude to … in <path>”.`); index paths are used as given by the index. `TASK_VERBS` is now
   coding-only: fix, debug, refactor, implement, lint, test, retest, run, rerun, build, rebuild, compile, typecheck,
   deploy, commit, rebase, revert, bisect, benchmark, migrate, reproduce, review; start verbs ("open", "new", …) no
   longer count for a bare instruction. The confirmed proposal is the ordinary `start_session{machineId: <this
   hub>, confirmed: true}` / `prompt`, which the hub already runs locally (hub test confirms one end to end).
2. **MEDIUM: status first.** `is_instruction` rejects any message with a status word (`STATUS_WORDS`: update,
   running, pending, sessions, status, progress, …) or status phrase, so "update me", "check what's running", "look at
   the pending sessions", "run status", "review the progress" are status asks; recaps ("review what happened today")
   stay the daily report.
3. **LOW: a bare yes only for harmless requests.** `Intent::Resolve` carries `explicit` (an approve/deny verb was
   said). "yes" / "sure" / "sounds good" / "go ahead" / 👍 approve only a read, search or think request; anything
   that runs, deletes, moves, fetches, edits, switches modes or has no kind gets `Claude Code wants to: Run bun test.
   Say “approve” to let it (or “deny”).` Applied in every chat, not only the default pet's (the same oldest-request
   rule applies there). Bare "no" still denies (denying never runs anything).

Tests added/updated: `everyday_sentences_never_reach_an_agent` ("make me a sandwich", "continue with the plan", "update
me", "check what's running", "look at the pending sessions", … never act or propose; "tell this machine to …" is a
local proposal in any chat), `default_pet_picks_an_agent_and_says_which` (local start/prompt proposals, named agent
still acts, path outside/inside the index), `approve_and_deny_single_pending_request` (bare yes per tool kind), the
intent test (trimmed verbs, status phrases, `explicit`), and the hub test (`proposal stored, nothing ran, confirmed
start_session starts it`).

Gates after the fixes: `cargo test -p pawl-brain -p pawl-hub` brain 89 passed, hub 187 passed; `cargo clippy
--workspace --all-targets -- -D warnings` clean.

Also closed the same misparse on the addressed path (pre-existing): "Biscuit, update me", "Rex, check what's
running", "on the mac mini, look at the pending sessions" are status asks now, not prompts to that agent.

## What it does (rev 1; agent-less commands are proposals since rev 2)

- **Setting `bot.defaultAgent`** (wire `HubSettings.bot.defaultAgent`, patch `Some(None)` = automatic). Stored in
  config.toml as `[brain] default_agent = "codex"` (see decisions). Validated against the agents this hub knows:
  built-in ids and their registry ids, every discovered agent, and the custom agents of the config being written (one
  added in the same patch counts). Unknown id = `invalid:bot.defaultAgent: "x" is not a known agent (installed,
  registry or custom)` (the value is not echoed when it has control characters or is long). `""` = automatic. A
  custom agent that was the default and is removed by a patch resets it to automatic. Read at use time (no Brain
  rebuild), so it is reported in `applied`. `pawl settings` shows `default agent  codex` / `(automatic)`.
- **The chat with no pet attached is the default pet.** `ClientMsg::Chat` without `petId` no longer borrows the pet of
  the most recent session: the brain gets `ctx.pet = None` and both the user line and the reply carry `petId: null`.
  The brain treats `pet == None` as the default pet.
- **Agent choice when a command names none** (`remote::pick_agent`, used for this machine and for paired machines,
  in every chat, replacing "Which agent should do it?" / "Which agent on X?"): the `default_agent` setting if installed
  on the target machine; else the agent of the most recent session there (when installed: hook-only agents can't be
  started); else the first installed in the order claude-code, codex, gemini, github-copilot (ids `copilot`,
  `github-copilot`, `github-copilot-cli`), opencode (registry ids `claude-acp`/`codex-acp` count too), then by name.
  When the machine has more than one installed agent the reply says so: `I picked Codex (your default agent); for
  another, name it: “tell claude to …”.` (remote: `“tell the Laptop's claude to …”`; reasons: `your default agent`,
  `it had the last session here` / `on Laptop`, `the first one installed`).
- **Agent-less commands for the default pet** (parser, default pet only): a message of 2+ words that starts with a task
  verb (`fix`, `add`, `run`, `review`, `refactor`, `check`, `test`, `build`, … or any start verb) and is not a
  question, status ask, recap or small talk is a command with no agent: "fix the failing test in repo api", "run the
  tests on the Laptop", "can you add a changelog entry", "start a new session in repo api". Routing is unchanged (a
  repo here stays local, a repo only a paired machine has or a named machine goes there as a proposal). A pet's own
  chat keeps today's rules (a bare instruction is small talk there).
- **Talk to one session** ("tell Biscuit to also run lint", "tell the Laptop's codex in repo api to …"): with no repo
  named, the agent's open owned sessions on that machine (in folders its repo index lists) are the targets. Here: one
  = prompt it (busy = today's "busy" answer); several = ask which, listing repo and state, e.g. `Biscuit (Claude Code)
  has 2 open sessions on Windows PC: ai-pet (idle), api (idle). Which one? Say “tell Biscuit to also run lint in repo
  ai-pet”.` Another machine: one prompt proposal per open session (several = the usual "Which one did you mean?"
  with a proposal each); a busy one gets today's start proposal in its folder. No open session: today's behaviour.
- **Status overview** ("status", "what's everyone doing?"): every live session on every machine as an individual,
  waiting ones first, up to 8 lines: `2 sessions open, 2 busy, 1 needs you:` then `• Rex · Codex on Mac mini · api:
  waiting for your OK: Run rm -rf build`, `• Biscuit · Claude Code on Windows PC · ai-pet: working: Run cargo build`
  (pet name when the agent has a pet; machine once several are known), then the approve hint as before. Per-agent and
  per-machine status answers are unchanged.
- **Help for the default pet** (5-6 lines, with this hub's repo, agent, pet and machine names): status, "fix the
  failing test in repo X" (I pick the agent, or "tell codex to …"), "tell Biscuit to also run lint", "run the tests on
  the Mac mini" (other machines need your OK), approve/deny and daily report. Pet chats keep their help.

Safety is unchanged: actions only for this machine, proposals only for other machines (the final invariant in
`Brain::handle` is untouched), everything comes from the deterministic parse of the user's own message, LLM output is
only text, approve/deny by chat unchanged (local only; remote = "use the panel"). Nothing is auto-approved.

## Files

- `crates/brain/src/chat/mod.rs`: `BrainContext.default_agent`; `command()` picks the agent (`pick_agent`) and appends
  the note, `command_for()` targets the one open session / asks which; `overview()` lists sessions as individuals;
  `default_pet_help()`; `OVERVIEW_LINES = 8`.
- `crates/brain/src/chat/remote.rs`: `AGENT_ORDER`, `Picked`, `pick_agent()`, `picked_note()`; `plan_on()` picks the
  agent, proposes per open session, returns the note; `remote_command()` adds it.
- `crates/brain/src/chat/intent.rs`: `TASK_VERBS`, `is_instruction()`; `parse()` treats `pet_name == None` as the
  default pet (`classify(.., orchestrate)`, `parse_command(.., orchestrate)` for "start a session in repo api").
- `crates/brain/src/chat/tests.rs`: 5 new tests (agent choice local, remote proposals, overview, one-session talk,
  help); one assertion updated (the old "Which agent on Mac mini?" is now a pick).
- `crates/hub/src/config.rs`: `BrainSection.default_agent` (+ Debug, template comment).
- `crates/hub/src/settings.rs`: `BOT_DEFAULT_AGENT`, view, changed keys, `check_default_agent`, patch apply (incl. the
  removed-custom-agent reset), config.toml write, `pawl settings` line.
- `crates/hub/src/core.rs`: `chat(None)` = no pet attached; `brain_context` passes `default_agent`.
- Tests: `crates/hub/src/settings/tests.rs` (table row + `default_agent_is_validated_persisted_and_cleared`),
  `crates/hub/src/core/settings/tests.rs` (`the_default_agent_is_saved_shown_and_refused_when_unknown`),
  `crates/hub/src/core/tests.rs` (`the_default_pet_hands_agentless_work_to_the_default_agent`; the chat ack test now
  expects `petId: null`).

## Commands (in the worktree)

- `cargo test -p pawl-brain -p pawl-hub`: brain 88 passed, hub 187 passed, 0 failed.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean. (`apps/desktop/src-tauri` is excluded from the
  workspace, so clippy does not cover pawl-desktop; not run.)

## Decisions

1. config.toml key is `[brain] default_agent`, not `[bot]`: every other `bot.*` setting lives in `[brain]`, and there
   is no `[bot]` section. Say if you want a `[bot]` alias.
2. `chat` without `petId` stores and broadcasts messages with `petId: null` (previously the most recent session's pet).
   The panel's `ChatView` already renders a null `petId` with its selected pet. The daily report still posts as
   `default_chat_pet()` (unchanged). docs/CONTRACTS.md may want a line on this (read-only for me).
3. Agent-less commands need a task verb first (list in `TASK_VERBS`) and two words or more: "the build is broken" or
   "test" stay small talk, so a stray sentence never starts an agent.
4. The "I picked …" note only appears when the target machine has more than one installed agent (nothing was chosen
   otherwise).
5. Open sessions count as "that session" only in folders the machine's repo index lists (ctx.repos stays the only
   authority for repo names and paths), so a session in a deleted folder is not a target.
6. The overview names the machine only once several machines are known (the brain's existing rule).

## Gaps

- The bubble's own UI (t29) is untested against these replies; the hub contract is: send `chat` without `petId`, show
  messages whose `petId` is null.
- "everyone" is not a reserved word: a machine or pet called "everyone" would be a target in "what's everyone doing".
- No E2E (out of scope).
