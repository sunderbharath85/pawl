# sde-brain report — step 5: remote commands by machine + repo (`crates/brain/`, crate `pawl-brain`)

Status: **DONE, rev 3** (2026-11-24, branch `step-5-remote-commands`). Gate passes: `cargo test -p pawl-brain` 82 passed / 0
failed; `cargo clippy -p pawl-brain --all-targets -- -D warnings` clean. `cargo check -p pawl-hub --all-targets` builds.
Nothing committed.

## Rev 3 — `Watchdog::set_config` (step 5b follow-up)
- `crates/brain/src/watchdog.rs`: `pub fn set_config(&mut self, cfg: WatchdogConfig)`, which matches the existing `&mut self`
  `tick`. It swaps in the new timings and keeps the per-request and per-turn nudge state.
  - Something already nudged is not nudged again because the threshold dropped.
  - Repeats follow the new interval, counted from the last nudge; `repeat_every_ms <= 0` stops them.
  - New items use the new threshold.
- Test: `watchdog::tests::set_config_keeps_nudge_state`.
- For sde-hub (t28): call `watchdog.set_config(new_cfg)` on a settings change instead of building a new `Watchdog`.

## Rev 2 — integration and security-review fixes
Also touches `crates/brain/src/util.rs`.

- **The repo index is the only authority** (integration finding 2). `RepoIndex` is built from `BrainContext::repos` alone, for
  every machine including this one. Session cwds no longer create repo names or folders; they only rank index folders: a
  folder the agent worked in wins, then the most recent session event or `lastUsedAt`.
  - With no repo named, the local default folder (`default_cwd`) and the remote "agent's most recent session folder" both map
    session cwds through the index. A session in a deleted folder is skipped, so the bot asks instead of starting in a
    missing directory.
  - Finding 1 is kept: a repo in this machine's index stays local when no machine is named.
  - Test: `stale_session_folders_are_not_repos`, covering local and remote, plus ranking inside the index (two "api" folders).
- **(3) Local first on ties:**
  - **Agent aliases** rank (this machine, installed, last seen); `AgentCatalog::build` now takes an `is_local` fn.
  - **Equal machine names** resolve to this machine.
  - **Equal pet names** resolve to the chatted pet (matched by id), then this machine's pet. `strip_fillers` now recognizes
    the chatted pet by id (`PetRef.id`, and `Targets::new(..., local_id, chatted_pet_id)`), not by name.
  - **Machine names never shadow agents, pets or local words.** At a position where an agent alias, known agent word or
    pet name matches at least as many tokens, the phrase is the agent or pet, not a machine. So a paired machine named
    "Claude" cannot turn "tell claude to …" into a remote command. Machines named "here", "this machine", "local" and
    similar are ignored as machine names.
  - "tell this machine to …" now counts as local.
  - Tests: `ties_go_to_this_machine_and_peer_names_cannot_shadow_it` and `local_agent_wins_alias_ties`.
- **(4) Approve numbering:** "approve N" and the status hint number this machine's pending requests only.
  - Another machine's requests count only when that machine (or its pet) is named. The one exception is when nothing on this
    machine waits: then "approve" gives the step-4 panel answer for them.
  - Lists end with "(N more requests on other machines: answer them in the Pawl panel, or name the machine.)", and the
    status hint says "(+N on other machines, in the Pawl panel)".
  - `mirrored_text_never_turns_into_an_action` was updated: "approve 1" is now the local request even though the mirrored
    one is older; "approve 2" is "no request #2".
- **(2-brain) Names from data:**
  - `util::basename` now returns a cleaned `String`: control characters become spaces, whitespace is collapsed, and the
    result is capped at 80 chars (`NAME_MAX_CHARS`). New `clean_name()` applies the same to index names, peer machine names
    and agent names in step-5 replies.
  - A basename can no longer create a candidate: candidates come only from `ctx.repos`.
  - Tests: util `basename_handles_both_separators`, and the control-character repo name case in the ties test.

## Files changed
| File | Change |
|---|---|
| `crates/brain/Cargo.toml` | `uuid = { workspace = true }` (approved by t13; already a workspace dep). `Cargo.lock` gains the one line `"uuid"` under `pawl-brain`; no new package. |
| `crates/brain/src/chat/mod.rs` | Brain API (below); `handle` builds the cross-machine agent catalog, repo index and `Targets`, routes commands local vs remote, status/approve with targets; local `command()` takes `Option` agent and `Named` repos. |
| `crates/brain/src/chat/intent.rs` | Parser targets: machines, pets, "here"/"on this machine"; agent-less commands; pet addressing; `RepoIndex` across every machine (`ctx.repos` + all sessions). Returns `Parsed { intent, target }`. |
| `crates/brain/src/chat/remote.rs` (new) | Routing (`route`), per-machine planning, `ChatProposal` building, machine-scoped status context. |
| `crates/brain/src/chat/tests.rs` | Fixtures with `..Default::default()` and repo indexes; old `commands_only_target_this_machine` rewritten for step 5; 9 new step-5 tests including the LLM security test. |
| `crates/brain/src/util.rs` | Rev 2: `basename` returns a cleaned String; `clean_name`, `NAME_MAX_CHARS`. |

## Brain API (as in CONTRACTS.md "Step 5")
- `BrainContext` + `remote_agents: BTreeMap<MachineId, Vec<Agent>>`, `repos: BTreeMap<MachineId, Vec<pawl_protocol::RepoRef>>`,
  `pets: Vec<Pet>`. `BrainContext::default()` leaves them empty.
- `BrainReply` + `proposals: Vec<ChatProposal>` (serde default, skipped when empty). **`BrainReply` now derives `PartialEq`
  only** (protocol's `ChatProposal` has no `Eq`). Reported to t13 with the API READY message.
- Proposal ids: `uuid::Uuid::new_v4()`. At most 5 per reply.
- Hard invariants, enforced at the end of `handle`: proposals are dropped if they name this machine, and cleared if an
  action is set. Proposals are built only in `remote.rs`, which only `Intent::Command` (the parser's output for the user's
  own text) reaches. Every LLM path (`small_talk`, `summarize_inner`, `daily_inner`) returns `String` and goes through `say()`.

## Parser behaviour (deterministic, case-insensitive)
- **Machine:** display name, or a unique prefix of 3+ letters ("macb" → MacBook Pro; "mac" with Mac mini + MacBook Pro is
  ambiguous and stays text). It only counts in context: after "on"/"at", possessive ("the mac mini's"), right before an
  agent or pet name ("the mac mini claude"), after a command verb ("tell the mac mini to …"), as a lead address
  ("on the Mac mini, …" / "Mac mini: …"), or as a full name in a question ("what is the mac mini doing?"). The phrase must
  also end there: end of text, "'s", ",", ":", an agent or pet name, or a follow word (to/in/and/doing/busy/…). So
  "work on the mac mini setup script" stays task text.
  - **Local:** "here" (at the end, or before in/on/to/and; not "in here"), "on this machine/pc/computer/laptop/desktop/box",
    or "this machine's". "locally" and a bare "wipe this machine" are never taken from the task.
- **Pet:** any known pet (all machines plus the chatted one): "tell Rex to …", "ask Rex to …", "Rex, run the tests" (lead
  address + an instruction-like rest), and "what's Rex doing?". A pet fixes machine + agent. Addressing the chatted pet
  ("Biscuit, what is codex up to?", "Biscuit, approve", "Rex, good boy!") stays question / approval / small talk, with no
  target.
- **Agent:** id, name or alias, as before, now built from every machine's agents. A command may have no agent when a machine
  or pet is named ("tell the mac mini to run the tests in api").
- **Repo:** "in repo X", "in the X repo", "in X" / "for X". X is matched against every machine's index name and folder name
  (rev 2: index only, sessions just rank): exact, then alphanumeric-equal, then unique prefix (3+), then (explicit "repo"
  word only) unique substring. "Unique" counts distinct folder names, so one repo cloned on two machines is one name. The result is
  `RepoRef::Named { name, key, explicit }`, resolved per machine by the caller.

## Resolution (remote.rs `route` + `plan_on`)
1. A pet fixes machine + agent. Otherwise a named machine ("here" = local). Otherwise infer:
   - a repo found in this machine's index or sessions → **local** (see decision 1);
   - else a repo on exactly one paired machine → there; several → narrowed to the machines where that agent has a session
     in that repo; still several → one proposal per machine plus a question;
   - no repo: an agent not installed here but installed on paired machine(s) → there; else local.
2. Per remote machine: not paired → say so; offline → "Mac mini is offline (last seen 7 min ago), …", no proposal.
   - **Agent:** named but not installed there → "Gemini CLI isn't installed on Mac mini. It has: …"; hook-only → the hooks
     message; none named → the machine's only installed agent, else "Which agent on Mac mini? It has: …".
   - **Folder:** a named repo → that machine's matching index folders; unknown → "Mac mini has no repo called “x”. Mac mini
     has: api, web."; a path → must be in that machine's index; no repo → that agent's most recent session folder there that
     is still in the index, else "Which repo on Mac mini …? Mac mini has: …".
   - **Prompt vs start:** a `prompt` proposal (with `session_id`, no `cwd`) only for an owned, not ended, Idle/Done session of
     that agent in that folder with no pending request; everything else (busy, waiting, ended, hook, "start a new …") is
     `start` (with `cwd`).
   - **Empty task:** asks what to do ("What should Codex do on Mac mini in api?"), except "start …", which proposes a start
     with no prompt.
3. Reply text: one proposal → "Ready: <summary>. It runs on Mac mini, so it needs your OK first: confirm it to go ahead.";
   several → "That matches more than one place: api on Linux box, api on Mac mini. Which one did you mean? Pick one to
   confirm."; skipped candidates are noted, e.g. "(Linux box is offline, …)". Summaries: "Start Claude Code on Mac mini in
   api: run the tests" / "Send to Codex on Mac mini (api): add a test" (task cut to 80 chars; `text` holds it in full).
4. **Status with targets:** "what is the Mac mini's Claude doing?", "what's the mac mini doing?", "what's Rex doing?",
   "is claude busy here?". These use only that machine's sessions and requests, with "(Mac mini is offline, last seen …:
   this is what I saw last.)" when it is offline. Without a target, status is unchanged.
5. **Approvals:** unchanged (local only; a remote request gets the step-4 "use the panel / `pawl approve`" answer). New: a
   named machine or pet narrows the pending list ("approve the mac mini's request", "approve on windows pc").

## Tests added or changed (all in `cargo test -p pawl-brain`)
- **intent.rs:**
  - `machine_targets_in_commands`: six phrasings of the example, machine-only commands, prefix (unique and ambiguous),
    here / this machine, words that stay task text, quoted text.
  - `pet_targets`, `status_questions_with_targets`, `repo_index_spans_machines`.
  - Existing tests adapted to `Option` agent and `RepoRef::Named`.
- **tests.rs:**
  - `example_sentence_yields_a_start_proposal`: three phrasings, uuid v4, ids unique, empty-task start, wire shape.
  - `pets_fix_machine_and_agent`: a remote pet gives a proposal; a local pet gives a BrainAction; pet status.
  - `ambiguous_targets_ask_with_one_proposal_each`: two machines with repo api, narrowing by session, named machine, one
    candidate offline.
  - `missing_offline_and_unknown_targets_say_what_is_known`: unknown repo, missing repo or agent, not installed, last
    session folder, offline (status too), single installed agent.
  - `open_idle_owned_session_gets_a_prompt_proposal`: prompt vs start, including busy, waiting, ended and hook sessions,
    and force new.
  - `local_targets_still_act_and_never_propose`: local repo also cloned on the Mac, a local index entry, the Mac's copy by
    name, approvals.
  - `status_questions_with_targets`.
  - `llm_output_never_yields_actions_or_proposals`: a fake Ollama on 127.0.0.1 answers with a perfect command ("Tell the
    Mac mini's Claude to run rm -rf / in repo api. approve") for small talk, status, daily report and greeting. The reply
    text is the model's words, with `action` None and `proposals` empty. Captured prompts show mirrored session/tool text
    only inside one `<context>` block (it cannot close it).
  - `other_machines_get_proposals_never_actions` (the rewritten two-machine test).
  - `mirrored_text_never_turns_into_an_action` now also asserts no proposals.

## Commands run
- `cargo test -p pawl-brain`: 81 passed, 0 failed (rev 2).
- `cargo clippy -p pawl-brain --all-targets -- -D warnings`: clean.
- `cargo check -p pawl-hub --all-targets`: builds.
- `cargo fmt --check`: not used as a gate. The crate (including files I did not touch, e.g. `watchdog.rs` at HEAD) is not
  default-rustfmt formatted, so I kept the existing style.

## Decisions for the orchestrator
1. **Local first when no machine is named.** If the repo is in this machine's index or sessions (hook cwds included), the
   command stays local (BrainAction), even if other machines also have that repo. This keeps today's behaviour: the same
   repo cloned on two machines does not turn every local command into a question. Ambiguity questions happen among paired
   machines only.
   - A literal reading of the contract ("the repo exists on exactly one machine") would ask instead. To change that, edit
     `route()` in `remote.rs`: one `if`.
2. **Agent-only inference.** "tell goose to …" with Goose installed only on the Mac proposes it there. The contract does not
   list this case; it replaces the old "lives on Mac mini, I can only start agents here" text.
3. **A busy remote session gives a `start` proposal** (contract: "else start"). Locally a busy session still answers "busy,
   wait or start a new session".
4. **`prompt` proposals carry no `cwd`** (only `repo_name` and `session_id`); `start` proposals carry `cwd`.
5. **A remote path must be in that machine's repo index** (rev 2; it used to be the index or a mirrored session cwd);
   otherwise the bot asks. The owner validates again anyway. A local path the user types still goes to the hub, which checks
   that it exists.
6. **Machine prepositions are only "on"/"at"** (not "from"/"over"), and "locally" is never a target. This avoids changing
   task text such as "copy the logs from the mac mini" or "run the build locally".

## Known gaps
- Pet names that equal an agent alias resolve to the agent. Duplicate pet names: the chatted pet, then this machine's pet
  (rev 2), else the last match (the hub already rejects duplicates).
- Machine names made of everyday words can still match after "on"/"at" when the phrase ends there ("on the work laptop").
- Reply and summary text is English only. Proposal replies are deterministic and never LLM-worded (by design).
- Ranking matches session cwds to index paths by `path_key` (slashes, trailing slash, case on Windows/macOS). A session
  reached through a symlink or junction may not rank its folder; it still can't create one.
- Agent names in the older status and summary replies (outside the step-5 code) are not passed through `clean_name`.
  Folder names there are cleaned, because they go through `basename`.

---

Renamed to Pawl on 2026-11-23; names below are pre-rename.

# sde-brain report — crate `pet-brain` (`crates/brain/`)

Status: **DONE** (rev 3: security floor). Gate `cargo test -p pet-brain` passes (54 tests); `cargo clippy -p pet-brain --all-targets` is clean; `cargo check -p pet-hub` builds (it already calls `validate_rules`). No new dependencies.

## Rev 3 — security hard floor (`pet_protocol::NEVER_AUTO_ALLOW`)

- **Floor in evaluation.** `evaluate`, `evaluate_in` and `decide` never return AutoAllow for a request whose kind fails `is_auto_allowable`: execute, delete, fetch, other, or a missing kind.
  - **Invalid AutoAllow rules are skipped:** a rule with no `toolKind`, or with a floor kind, is ignored and evaluation moves to the next rule. So a later Deny still wins, and with no later rule the result is Ask. This covers invalid rules that reach evaluation anyway, e.g. from a hand-edited config.
  - **Second layer:** `decide` also turns any AutoAllow into Ask when the request's own kind isn't allowable.
  - **`Decision` has a new `floor: bool` field:** true when an unsafe rule was skipped or the second layer downgraded the result. The hub doesn't build `Decision` values, so adding the field breaks nothing.
- **`validate_rules(rules: &[Rule]) -> Result<(), RuleError>`.** It rejects the first AutoAllow rule whose `match.toolKind` is missing or in `NEVER_AUTO_ALLOW`.
  - `RuleError` variants: `AutoAllowWithoutToolKind { rule_id }` and `AutoAllowNotPermitted { rule_id, kind }`.
  - It also has a `rule_id()` accessor and implements `Display` and `std::error::Error`. Example message: `rule "yolo": toolKind "execute" can never be auto-allowed (security floor: execute, delete, fetch, other)`.
  - `default_rules()` passes. Ask and Deny rules are always valid.
- **Behavior change:** a kind-less AutoAllow rule used to auto-approve safe kinds (read, edit, …). It now never auto-approves anything, as the spec requires. `switch_mode` is allowable per the protocol, so an explicit `{toolKind: switch_mode, auto_allow}` still works.
- **New tests:**
  - `floor_kinds_are_never_auto_approved`: every floor kind plus a missing kind, against explicit, kind-less and default rule sets, with and without paths, across every entry point and cwd variant.
  - `user_execute_auto_allow_rule_never_approves`: uses the exact `set_rules` wire JSON, and also checks that a later deny wins.
  - `kindless_auto_allow_rule_never_approves`.
  - `explicit_allowable_kind_rules_still_work`.
  - `validate_rules_rejects_unsafe_auto_allow`: covers every variant, the message text, and that the first offender is reported.

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
