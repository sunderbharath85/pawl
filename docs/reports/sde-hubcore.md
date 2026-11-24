# sde-hubcore report: Step 4 (intents + acks) in `krypto-hub` (+ `krypto-brain`, CLI)

Branch: `step-4-intents`. Nothing committed. I edited `crates/hub/` (including the CLI) and `crates/brain/`, plus this report, which replaces the step 3 report. No dependencies were added.

`crates/acp` needed no change. The hub writes the `permission_resolved` event and broadcast for every decision it applies, local or remote (`after_resolved`). The ACP host emits that event only for its own cancels, which are never remote, so t13's `by_machine_id: None` in `crates/acp/src/host.rs` is correct. Files keep their LF line endings. No secrets, invite codes or prompt text are logged.

## Files changed

| File | Change |
|---|---|
| `crates/hub/src/core/intents.rs` (new) | **Origin:** `forward` (paired? linked? send `intent`, wait for `intent_ack` with a timeout, map the answer to an ack code, log), `resolve_remote` (classifies the option from the mirrored request; allow needs `confirmed`), `start_remote` (needs `confirmed`), `on_intent_ack`, `peer_agents`. **Owner:** `on_intent` → `handle_intent` (dedupe by intent id, `forbidden`, per-peer rate limit, validate, apply, log) → `apply_intent` (approve/deny through `decide`, prompt/cancel/start through the local functions) and the "Approved from …" notification. |
| `crates/hub/src/core.rs` | The decision path is now `decide()`, shared by local clients, rules, chat and intents (first valid decision wins, below). It keeps a set of decided request ids, so a repeat answer gets `already_decided`. `after_resolved` carries `by_machine_id` into the event payload (with `optionKind`) and into `ServerMsg::PermissionResolved`. Audit entries can carry an origin. New `AckCode`s: `already_decided`, `unconfirmed`, `forbidden`, `rate_limited`, `offline`, `timeout`. `Actor` now records the client kind. Dispatch: resolve_permission / prompt / cancel on another machine's id and start_session with another `machineId` go to the intent code; `list_agents{machineId}` too. Local prompts now go through `validate_prompt` (non-empty, ≤ 100,000 chars). |
| `crates/hub/src/store.rs` | **Schema v5:** audit_log `origin_machine_id`/`origin_name`/`intent_id`/`client` (+ index on request_id); an `intent_log` table (role origin/owner, intent id, kind, peer machine id + name, client kind, target, option, status, reason; never prompt text); `devices.agents` (last reported agents). `AuditOrigin`, `IntentLogEntry`, `IntentRole`; `request_was_decided`, `insert_intent_log`/`intent_log`, `set_device_agents`/`device_agents`. |
| `crates/hub/src/core/mirror.rs` | Dispatches `intent` / `intent_ack` bodies (after the existing paired-hub and flood checks). `permission_resolved` mirror updates carry `byMachineId`; inbound, it is kept only if it is a valid machine id. |
| `crates/hub/src/core/peers.rs` | `presence_agents`. A peer's agents are saved when its presence reports a change, so they stay available while it is offline or after a restart here. |
| `crates/hub/src/config.rs` | `[peer] accept_intents` (default `true`); documented in the template. |
| `crates/hub/src/cli.rs` | `krypto start <agent> --cwd <dir> --machine <name\|id> [--yes]`, `krypto approve <id> [--option] [--yes]`, `krypto deny <id> [--option]`, `krypto pending [--json]`. Answering a permission in `krypto start`'s turn sends `confirmed: true` (the person answered at that terminal). Intent error codes become readable text. |
| `crates/brain/src/chat/{mod,summary,tests}.rs`, `growth.rs` | Chat approve/deny of another machine's request answers how to do it ("…lives on Mac mini, so I won't approve it from chat: answer it in the Krypto panel (it asks you to confirm), or run `krypto approve <id>`"); it never forwards. Status replies say who decided ("Last request approved from Mac mini.") from the event's `byMachineId`/`optionKind`. `by_machine_id: None` in the growth test. |

## Behaviour

### Origin hub

- A ClientMsg naming another machine's id goes to that id's owner, never to anyone else. This covers resolve_permission / prompt / cancel by id prefix, and start_session by `machineId`.
  - Unknown or unpaired machine → `not_found`.
  - Paired but not linked (the send fails) → `offline`.
  - No ack within `INTENT_TIMEOUT_MS` → `timeout`.
  - Otherwise the client gets the owner's result, or the reject reason as its code.
- **Confirmation, checked before sending:** an allow option of a remote request and a remote start need `confirmed: true`, else `unconfirmed` and nothing is sent.
  - The option kind comes from the mirrored request. An option of a request the mirror does not know counts as an approval, so it also needs confirming.
  - Deny, prompt and cancel need no confirmation.
- **Audit:** every forwarded intent is logged in `intent_log` (role `origin`) with its intent id, kind, target machine + name, client kind, target, option, and the owner's answer, or `offline`/`timeout`. Prompt text is never logged.
- `ack` results: approve/deny/prompt/cancel → `{}`; start → `{session}` (the owner's session; the mirror brings it here too).

### Owner hub

- **Intake:**
  - An `intent` is accepted only from a paired hub over its live link; `on_peer_body` already refuses anything else.
  - It is deduped by (origin, intentId): the last 1000 acks for 10 min. A repeat gets the same ack and changes nothing, even with a different body. A repeat while the first copy is still being handled is ignored; the first copy is answered.
  - More than 20 intents per second from one peer → `rate_limited`; that ack is not cached, so the same id may be retried.
  - `accept_intents = false` → `forbidden`.
- **Validation:**
  - The target id must have this hub's prefix, else `not_owner`; this includes a third machine's ids and the origin's own ids. An unprefixed id is `not_found`.
  - Approve and start need `confirmed`, else `unconfirmed`.
  - A request or session that doesn't exist → `not_found`. A request that was decided → `already_decided`.
  - Approve needs an allow_* option of that request, and deny a reject_* option or none, else `invalid`. An unknown option is also `invalid`.
  - Prompt: non-empty and ≤ 100,000 chars.
  - Start: the agent is installed here; the cwd is an absolute, existing directory (no `~`, no relative paths); the prompt follows the prompt rules.
- **First valid decision wins.** `decide()` does the pending check, the option check, taking the request out and marking it decided under one lock. Of two answers racing for one request (two intents, or a local click and an intent), exactly one is applied and the other gets `already_decided`. The local ClientMsg path now answers `already_decided` instead of `not_found` for a request that was decided. The memory holds the latest 10,000 decided ids; older ones are found in the audit log, which also covers decisions from before a restart. If the agent refuses the answer, the request goes back to pending.
- **Apply.** The same code path as a local ClientMsg; rules never re-run. `permission_resolved` has `by: "user"` and `byMachineId` = origin, in the broadcast, the mirror update and the stored event (with `optionKind`). Start runs the agent on this machine only.
- **Audit.**
  - Every intent goes to `intent_log` (role `owner`: kind, origin machine id + name, client kind, target, option, status/reason).
  - Remote approve/deny are also in `audit_log` with intent id, origin machine id + name, client kind, option and outcome; actor is e.g. `cli@Smoke B`.
- **Notification on the owner** when a remote approval was applied: title "Approved from Mac mini", body the tool title ("bun test").
- **`list_agents{machineId}`** for a paired hub returns the agents of its latest presence, else the last ones it reported (persisted), else `[]`. An unknown machine → `not_found`.

### How approvals via chat are gated (unchanged in spirit)

Chat never forwards. `intent::parse` on the user's own message is the only source of a resolve action. For another machine's request, the reply says to use the panel or `krypto approve`, both of which ask for confirmation. Status replies now name who decided.

## Smoke-test setup (as asked)

Hooks cannot create a pending ACP request: hook sessions are informational and have no permission flow. I used an **owned ACP session with a real agent process**: a small scripted ACP agent, `smoke-acp-agent.js` in my scratchpad, registered on hub A as a `[[custom_agents]]` entry (`command = ["node", "<script>"]`).

It speaks ACP over stdio: `initialize`, `session/new`, `session/prompt`. On each prompt it sends a `tool_call` ("bun test", execute), asks `session/request_permission` (allow_once / reject_once), waits for the answer, reports it, and ends the turn. **No test-only code in the binary.**

The CLI's interactive confirm needs a TTY, which the agent's shell does not have, so `krypto approve` / `krypto start` ran in their own console window with output redirected to a file. The answers were typed into that console as key presses by `type_console.ps1` (scratchpad): `AttachConsole` + `WriteConsoleInput`, exactly what a person at that terminal does.

## Tests

- `cargo test -p krypto-hub`: **103 passed** (stable over 4 runs). `cargo test -p krypto-brain`: **66 passed**. `cargo clippy -p krypto-hub -p krypto-brain --all-targets -- -D warnings`: clean.
- New tests, covering everything the assignment listed:
  - `remote_approve_is_applied_audited_and_announced`: the agent gets the answer once; `audit_log` has the origin (machine, name, intent id, client) with `decided_by: user` and actor `desktop@Mac mini`; `intent_log` (owner, applied); `permission_resolved` with `byMachineId`; notification "Approved from Mac mini"; event payload `byMachineId` + `optionKind`.
  - `first_decision_wins_between_intents_and_local_clicks`: two concurrent intents → exactly one applied, one `already_decided`; a concurrent local click and intent → exactly one wins and the local loser answers `already_decided`; the agent is answered once each. Later answers (intent or local) are `already_decided`, also after the in-memory record is cleared (audit log fallback).
  - `the_owner_validates_intents`: unconfirmed approve/start → `unconfirmed`; approve with a reject option, deny with an allow option, unknown option → `invalid`; a third machine's request/session and the origin's own id → `not_owner`; unknown/unprefixed → `not_found`; empty/oversize prompt → `invalid`. Nothing decided, no audit entry; a deny with no option is then applied without confirmation.
  - `remote_start_runs_only_on_the_owner_in_a_valid_cwd`: relative / `~` / missing directory / unknown agent → `invalid`, nothing started; a valid start runs on the owner with the prompt, and the session's `machineId` is the owner.
  - `accept_intents_false_forbids_every_intent` (and default `true`).
  - `intents_are_rate_limited_per_peer`: the 21st within a second → `rate_limited`; other peers unaffected; the same id may be retried after the window.
  - `a_repeated_intent_id_returns_the_same_ack`: the same ack, even for a different body; nothing re-applied or re-broadcast; the same id from another peer is a different intent.
  - `the_origin_checks_before_sending`: unconfirmed approve/start → `unconfirmed` with nothing sent or logged; an unknown option of a mirrored request → `invalid`; paired-not-linked → `offline` (logged, approve/deny/start); unpaired machine → `not_found`; `list_agents{machineId}` known → `[]`, unknown → `not_found`.
  - `two_hubs_approve_start_and_time_out` (real peer links): B approves A's request → applied on A; A's audit names B (machine, name, client), B's intent log has the same intent id; B sees `byMachineId`; a second approval from B and a local click on A → `already_decided`, agent answered once; B starts a session on A (it runs on A and B's mirror gets it); B lists A's agents; A stops answering → B gets `timeout` after the timeout.
  - Changed expectations: actions on an unpaired machine's ids are `not_found` (they were `not_owner`); on a paired-not-linked machine they are `offline`/`unconfirmed`; `update_pet`/`chat` on remote ids stay `not_owner`; an unknown option and a repeat decision are `invalid` / `already_decided`.
  - CLI: `intent_commands_parse`, `machines_resolve_by_id_or_unique_name`, `requests_resolve_and_options_follow_the_verb` (approve with a reject option and deny with an allow option are refused in the CLI too), `intent_errors_read_well`.
  - Brain: the remote approve/deny reply points to the panel and CLI with no action; `status_says_which_machine_decided`.
  - Test-only knobs (in `#[cfg(test)]` only): a 400 ms intent timeout and a switch that makes a hub drop incoming intents, used for the `timeout` test.

## Two-hub binary smoke (`target/debug/krypto.exe`, scratch dirs under `%TEMP%\krypto-smoke-step4`)

A = "Smoke A" `nojrub3xpxaa` (API 47910, peer listener 127.0.0.1:47911, `smoke-agent` custom agent). B = "Smoke B" `8xh398ll1xo3` (API 47920). The real data dir was never touched.

```
$ krypto --data-dir <tmpB> --port 47920 peer join "<uri, code=<redacted>>"
Paired with Smoke A (nojrub3xpxaa, windows) at 127.0.0.1:47911

$ node ws_probe.mjs 47910 <tmpA>/token start smoke-agent <work> run the tests   (owned ACP session on A)
started smoke-agent session nojrub3xpxaa:565f7706-26dc-4377-87d5-167ba8d5cc71 on Smoke A

$ krypto --data-dir <tmpB> --port 47920 pending
ID        MACHINE  AGENT        TOOL      OPTIONS       WAITING
ea8740e3  Smoke A  smoke-agent  bun test  allow,reject  2s ago
```

A script cannot approve on another machine (`--yes` is not honoured, and there is no TTY):

```
$ krypto --data-dir <tmpB> --port 47920 approve ea8740e3 --yes < /dev/null
error: this needs a confirmation on an interactive terminal
exit 1
```

Interactive approval on B, in its own terminal; "y" + Enter typed at the prompt:

```
--- the terminal shows (before typing):
Approve on Smoke A: bun test? [y/N]
--- typing 'y' + Enter into that terminal: typed 4 key events
--- krypto approve exited with 0; its terminal output:
Approve on Smoke A: bun test? [y/N] Approved on Smoke A: bun test
```

On A, the agent received the answer and finished its turn:

```
get_events nojrub3xpxaa:565f7706-…: 8 events
  session_start, user_message, tool_call, request_permission, permission_resolved, tool_call_update, agent_message_chunk, stop
--- hub A audit_log:
   ('nojrub3xpxaa:ea8740e3-…', 'user', 'cli@Smoke B', 'allow', 'allowed', '8xh398ll1xo3', 'Smoke B', <intent id>, 'cli')
--- A permission_resolved event payload: by user byMachineId 8xh398ll1xo3 optionKind allow_once
--- hub A intent_log: ('owner', 'approve', '8xh398ll1xo3', 'Smoke B', 'cli', 'nojrub3xpxaa:ea8740e3-…', 'allow', 'applied', None) intent 853829f6…
--- hub B intent_log: ('origin', 'approve', 'nojrub3xpxaa', 'Smoke A', 'cli', 'nojrub3xpxaa:ea8740e3-…', 'allow', 'applied', None) intent 853829f6…
```

A second approval (interactive again, with the full id) and a deny on the owner itself:

```
$ krypto --data-dir <tmpB> --port 47920 approve nojrub3xpxaa:ea8740e3-… --option allow   ('y' typed)
Approve on Smoke A: this request? [y/N] error: Already answered. (already_decided)
$ krypto --data-dir <tmpA> --port 47910 deny ea8740e3
error: Already answered. (already_decided)
```

`krypto start --machine` on B runs on A. The start confirm was answered "y", and so was the permission prompt of the new turn:

```
Start smoke-agent on Smoke A in C:\Users\sunde\AppData\Local\Temp\krypto-smoke-step4\work? [y/N] [hub] starting smoke-agent in C:\…\work on Smoke A ...
[hub] session nojrub3xpxaa:6ef4e567-7048-4aa7-890e-ff2770bfa4ab (idle)
[session started]
you: run the tests
[execute] bun test
[permission] smoke-agent wants to run [execute] bun test
  1) Allow [allow_once]
  2) Reject [reject_once]
choose (number, y, n):   -> completed: bun test
[permission] bun test: answered allow by you
smoke-agent: permission answer: allow
[turn ended: end_turn]
(exit 0)
```

Both intent logs at the end, with the A/B logs checked for secrets:

```
--- hub A intent_log:
   ('owner', 'approve', 'Smoke B', 'cli', 'nojrub3xpxaa:ea8740e3-…', 'allow', 'applied', None)
   ('owner', 'approve', 'Smoke B', 'cli', 'nojrub3xpxaa:ea8740e3-…', 'allow', 'rejected', 'already_decided')
   ('owner', 'start', 'Smoke B', 'cli', 'smoke-agent', None, 'applied', None)
   ('owner', 'approve', 'Smoke B', 'cli', 'nojrub3xpxaa:168dabe7-…', 'allow', 'applied', None)
--- hub A sessions (smoke-agent): nojrub3xpxaa:565f7706-…, nojrub3xpxaa:6ef4e567-…   (both on A)
--- hub B intent_log: the same four with role 'origin' and target machine Smoke A
A/hub.log: invite code 0, prompt text 'run the tests' 0, intent lines 4
B/hub.log: invite code 0, prompt text 'run the tests' 0, intent lines 4
```

Both hubs were stopped; ports 47910, 47911 and 47920 are free.

## Review fixes (step 4)

Threat: a paired but compromised hub, which can send `confirmed: true` for anything. After these fixes, `cargo test -p krypto-hub` passes **115** (stable over 3 runs) and `cargo test -p krypto-brain` **66**; `cargo clippy -p krypto-hub -p krypto-brain --all-targets -- -D warnings` is clean. The two-hub smoke was re-run (below). Each fix has a regression test.

- **H1. A remote approve only ever picks `allow_once`.**
  - `decide()` checks the option kind of a remote decision under the same lock as the pending check (`check_remote_option`).
  - `allow_always` → `forbidden`, and the request stays pending for a person on the owner. Any other non-allow kind for approve, or allow kind for deny → `invalid`.
  - Test: `remote_approve_never_chooses_allow_always`.
- **H2. Remote-driven sessions.**
  - A session started by an intent is marked `remote_driven` before its first prompt is sent, so even the first request is covered. A session prompted by an intent is marked before that prompt goes out.
  - The mark is stored in `sessions.remote_driven` (schema v5) and loaded at startup.
  - `rule_action` turns `auto_allow` into `ask` for these sessions; deny rules still deny. This applies on arrival and when `set_rules` re-evaluates. Watchdog notifications are unchanged.
  - Test: `remote_driven_sessions_always_ask_a_human`. A local session's read is auto-approved; the same read in a remote-started or once-remote-prompted session waits for a human; a deny rule still denies; the mark survives a restart.
- **M3. Remote start cwd policy** (`remote_start_dir`, `plain_path`, `is_within`).
  - **Refused before any lookup:** raw `\\…` or `//…` (shares, device paths, verbatim forms) → `forbidden`.
  - **Must exist:** the path must be absolute and existing (`invalid` otherwise).
  - **Canonicalized** with `std::fs::canonicalize`. `\\?\C:\…` becomes `C:\…`; `\\?\UNC\…` and other verbatim or device prefixes → `forbidden`.
  - **Known folder required:** it must be equal to or inside the canonical cwd of one of this hub's own sessions (owned or hook, never mirrored), or inside a canonical `[peer] start_roots` entry (new config key, default `[]`). Else `forbidden`.
  - Comparison is component-wise, case-insensitive on Windows, so `..` escapes are caught after canonicalization. The agent starts in the canonical path.
  - Tests: `remote_start_runs_only_in_folders_this_machine_knows` (unknown folder, `..` escape, share, `//` share, device path, `\\?\UNC` → `forbidden`; known folder, a subfolder, a start root → applied with a plain canonical cwd; on Windows also an upper-case path) and `plain_paths_drop_the_verbatim_prefix_and_refuse_shares`.
- **M4. Bounded intent log.**
  - `prune_intent_log` drops rows older than 30 days and keeps the newest 100,000, at startup and in the hourly prune.
  - `rate_limited` and config-`forbidden` refusals are logged at most once per peer per minute (per reason). The skipped ones are counted in a new `intent_log.repeats` column on the next entry.
  - Tests: `store::the_intent_log_is_pruned`, `intents::flood_logging_is_once_a_minute_per_peer`, `flood_refusals_are_logged_once_a_minute` (50 refusals → 1 row).
- **M5. The origin validates owner results** (`checked_result`).
  - Start: the result must be at most 64 KiB, its `session` must parse, and `machineId` and the id prefix must both be the target. Otherwise the client gets `invalid`. A `warning` is kept with control characters stripped and at most 300 chars.
  - Every other kind returns `{}` whatever the owner sent.
  - Test: `the_origin_checks_owner_results` (third machine's session, our id under their machine, junk, missing, oversize, hostile warning).
- **M6. `krypto start --machine <other>` always asks** on the TTY. `--yes` is ignored there, with a note; local starts are unchanged. Both CLI paths now use one rule, `asks_first`. Test: `another_machine_always_asks_a_person`. Smoke: `--yes` given, the prompt still shown.
- **L7. Dedupe cache per peer:** 1000 acks per peer for 10 min (`PeerAcks`), so one peer cannot evict another's. Test: `intents::the_dedupe_cache_is_per_peer`.
- **L8. A start whose first prompt fails** still returns the session, as `applied`, with `warning` set. This applies to intents and local `start_session` acks alike (`launch_session` + `first_prompt`). Test: `a_start_whose_first_prompt_fails_returns_the_session_and_a_warning`.
- **L9. No spurious `already_decided` during a rollback.**
  - `decide()` puts a request in `deciding` while the agent call is in flight.
  - A concurrent answer waits on a `Notify` and then re-checks. If the first answer was refused and rolled back, the second is applied; otherwise it gets `already_decided`.
  - Only a successful answer marks the request decided.
  - `request_was_decided` (the audit-log fallback) no longer counts `failed` attempts.
  - Test: `a_concurrent_answer_waits_for_the_one_in_flight` (a slow answer the agent refuses, plus a concurrent remote approve → the approve is applied; audit: failed, then allowed).
- **L10. Peer names are sanitized at ingestion.**
  - `sanitize_machine_name`: control characters dropped, trimmed, at most 40 chars, "Unnamed machine" if empty.
  - It is applied in the roster (`upsert` from pairing, `update_seen` from hello/presence) and to the `Paired` event's names, so audit actor labels, notifications and every client use the clean value.
  - Test: `store::peer_names_are_sanitized_at_the_roster` (ANSI/OSC sequences, BEL, blank, 60 chars).
- **Pairing output.** `krypto peer invite` and `krypto peer join` now print: "It can see this machine's sessions and, with confirmation on its side, approve once, prompt, cancel and start agents in folders this machine already uses. Turn off with [peer] accept_intents = false."
- **Also found in the smoke.** The CLI's `forbidden` text claimed `accept_intents = false` for every refusal. It now names all three causes ("always allow" is chosen on the owner; starts only in folders it already uses or under its start_roots; nothing when accept_intents = false). Test extended.

### Smoke re-run (two hubs, scratch dirs under `%TEMP%\krypto-smoke-step4`)

Setup: A = "Smoke A" `itlg747r4pox` with the scripted ACP agent (now offering allow_once / allow_always / reject_once, and a "read" mode). B = "Smoke B" `ecacoeppwwrc`. Answers were typed into each CLI's own console as before.

```
$ krypto --data-dir <tmpB> --port 47920 peer join "<uri, code=<redacted>>"
Paired with Smoke A (itlg747r4pox, windows) at 127.0.0.1:47911
It can see this machine's sessions and, with confirmation on its side, approve once, prompt, cancel and start agents in folders this machine already uses. Turn off with [peer] accept_intents = false.

--- a local session on A (normal rules): its read request is auto-approved
$ krypto --data-dir <tmpA> --port 47910 pending
Nothing is waiting for an answer.

$ krypto --data-dir <tmpB> --port 47920 approve 769adc9d --option always   ('y' typed)
Approve on Smoke A: bun test? [y/N] error: Smoke A does not allow that from another machine: "always allow" is chosen on Smoke A itself, agents start only in folders it already uses (or under its [peer] start_roots), and nothing is accepted when its [peer] accept_intents = false. (forbidden)
$ krypto --data-dir <tmpB> --port 47920 approve 769adc9d   ('y' typed)
Approve on Smoke A: bun test? [y/N] Approved on Smoke A: bun test
$ krypto --data-dir <tmpB> --port 47920 deny itlg747r4pox:<that request id>
error: Already answered. (already_decided)

$ krypto --data-dir <tmpB> --port 47920 start smoke-agent --cwd <unknown-folder> --machine "Smoke A" --yes --prompt "read the readme"   ('y' typed)
(--yes is not honoured for another machine: confirm below)
Start smoke-agent on Smoke A in C:\…\krypto-smoke-step4\unknown-folder? [y/N] [hub] starting smoke-agent in C:\…\unknown-folder on Smoke A ...
error: Smoke A does not allow that from another machine: … (forbidden)        <- run with the earlier binary; the code is the point

$ krypto --data-dir <tmpB> --port 47920 start smoke-agent --cwd <work> --machine "Smoke A" --prompt "read the readme"   ('y', then 'y')
Start smoke-agent on Smoke A in C:\…\krypto-smoke-step4\work? [y/N] [hub] starting smoke-agent in C:\…\work on Smoke A ...
[hub] session itlg747r4pox:e179831d-d421-4f05-96cc-f241fb18db39 (idle)
[session started]
you: read the readme
[read] read README.md  (C:\…\work\README.md)
[permission] smoke-agent wants to run [read] read README.md        <- remote-driven: asked, not auto-approved
  1) Allow [allow_once]
  2) Always allow [allow_always]
  3) Reject [reject_once]
choose (number, y, n):   -> completed: read README.md
[permission] read README.md: answered allow by you
smoke-agent: permission answer: allow
[turn ended: end_turn]

--- hub A audit_log (tool, decided_by, actor, option, outcome, origin):
   ('read README.md', 'rule', 'rule:auto_allow', 'allow', 'allowed', None)          <- local session: auto-approved
   ('bun test', 'user', 'cli@Smoke B', 'allow', 'allowed', 'Smoke B')
   ('read README.md', 'user', 'cli@Smoke B', 'allow', 'allowed', 'Smoke B')          <- remote-driven: a person answered
--- hub A intent_log: approve rejected forbidden (allow_always), approve applied, start rejected forbidden (unknown folder), start applied, approve applied
--- hub A remote-driven sessions: ['itlg747r4pox:e179831d-…']
A/hub.log, B/hub.log: invite code 0, prompt text 0
```

The run with the unknown folder used the binary from before the wording fix, so its message still said `accept_intents`. Its code (`forbidden`) and the owner's intent_log entry are what count. The corrected text appears in the `allow_always` run, which was done after the rebuild.

## Decisions for t13 to check

1. **`krypto pending [--json]`** lists waiting requests on every machine with short ids. It isn't in the contract, but without it there is no way to find a request id from the terminal.
2. **Local `krypto approve`** asks too, unless `--yes` (on a TTY). Remote approvals always ask and need a TTY. Local prompts and starts are unchanged.
3. **Unknown option → `invalid`.** A local resolve with an option the request doesn't offer is now `invalid`; it used to be a free-text error.
4. **Prompt length cap.** Local and remote prompts are now capped at 100,000 characters (`invalid`); "as for local" had no limit before.
5. **Options of requests the origin's mirror lacks** count as approvals on the origin, so they need `confirmed`. The owner still decides.
6. **Intents are handled concurrently per peer** (each in its own task, so a slow start never blocks approvals). Two intents from one peer therefore have no order guarantee.

## Known gaps

- **No acting on a decision already made elsewhere.** A request whose agent refuses the answer goes back to pending; a racing loser that already got `already_decided` must answer again.
- **`list_agents` before any presence.** After this hub restarts, a peer's agents come from `devices.agents`, which is only saved once that peer has sent a presence with agents.
- **Not in step 4:** bot routing of remote commands (step 5), relay (step 6), phone (step 7).
