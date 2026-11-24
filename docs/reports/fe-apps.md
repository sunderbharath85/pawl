# fe-apps: Step 4 (intents + acks): hub-client, pet-engine, desktop, VS Code

Branch `step-4-intents`. Nothing committed.

I edited only my owned paths: `packages/hub-client`, `packages/pet-engine`, `apps/desktop` and `apps/vscode`. I coded against the protocol v5 types and the CONTRACTS.md "Step 4" section, not against a running hub, because sde-hubcore is building the hub side in parallel. The CLI part of the section is sde-hubcore's.

**Contract change during the step:** t13 added `PermissionResolvedPayload.byMachineId` (event payload, TS and Rust) at my request. History is built from events, so "Approved from Windows PC" survives reloads and backfill. The clients read that field and fall back to plain "Approved".

## Security model on the client

The owner hub decides, and the origin hub checks `confirmed` too. The client adds two more layers of its own:

1. **Every UI asks before approving another machine's request, or starting a session on another machine.** Nothing in the clients ever sets `confirmed: true` automatically.
   - Desktop: the bubble's confirm step, the card's confirm step, and the New Session confirm step.
   - VS Code: a modal with a single "Approve on Mac mini" / "Start on Mac mini" button. Cancel or Esc does nothing.
   - The only places `confirmed: true` is set are the handlers of those confirm buttons.
2. **hub-client refuses without the confirmation.** `resolvePermission` with an allow_* option on another machine's request, and `startSession` on another machine, reject with `unconfirmed` *before sending* unless `confirmed: true` is passed. A UI bug can't skip the step silently. Tests cover both.
- **Deny, prompt and cancel need no confirmation,** as the contract says.
- **No logging of prompt text or secrets.** The only new log line is VS Code's "started session <id> (<agent>) on <machineId>".

## What changed

### hub-client (`packages/hub-client`)

**`HubClient`:**
- `resolvePermission(requestId, optionId, {confirmed?})` and `approve(requestId, {confirmed?})`. Remote allow without `confirmed` → local `unconfirmed`. `confirmed` goes on the wire only when given.
- `startSession({agentId, cwd, prompt?, machineId?, confirmed?})`. The positional `startSession(agentId, cwd, prompt?)` still works (mobile and older callers).
  - `machineId` is sent only for another machine (this machine stays the hub's default).
  - Remote without `confirmed` → local `unconfirmed`.
- `listAgents({refresh?, machineId?})`, and `listAgents(refresh?)` still works. Another machine's agents are returned only: `applyAck` no longer replaces `state.agents` for a `machineId` request.
- **Longer waits for forwarded requests.** Approvals, prompts, cancels and starts for another machine wait `REMOTE_REQUEST_TIMEOUT_MS` (INTENT_TIMEOUT_MS + 10 s), so the hub's own `timeout` ack arrives first.
- **Which requests count as remote.** A request is remote when its held machineId (or failing that, the global id's prefix) isn't this machine. Ids without a machine prefix count as local.

**New `src/intents.ts`:**
- **Confirmation rules:** `needsConfirmation(state, request, optionId)` (another machine's allow_* answer), `startNeedsConfirmation(state, machineId)` and `isAllowOption`.
- **Machine helpers:** `offlineReason(state, machineId)` ("Mac mini is offline") and `ownerName`.
- **`intentErrorText(err, machineName, action)`.** Clear text for every code: `already_decided`, `offline`, `timeout`, `unconfirmed`, `forbidden`, `invalid`, `rate_limited`, `not_found`, `not_owner`, and connection loss.
  - The contract's three headline texts are used as written: "Already answered.", "Mac mini is offline." and "No answer from Mac mini yet — check that machine."
  - `invalid` and `not_found` are worded per action.
  - Also: `isAlreadyDecided` and `INTENT_ERROR_CODES`.
- **`recentCwds(state, machineId)`:** a machine's distinct recent session folders, for the cwd suggestions.

**`readOnlyReason`** now covers only pets ("Change it on Mac mini: pets belong to their machine."). Sessions and approvals are actionable.

**Tests:** new `test/intents.test.ts` covers the confirmation rules, every code's text, `recentCwds`, the local `unconfirmed` refusals with nothing sent, the `confirmed` wire format, the longer waits, `listAgents` not touching state, and the `byMachineId` resolution. 153 tests in total.

### pet-engine (`packages/pet-engine`)
- **`PetBubbleData.confirm?: {machineName, action}`.** A visitor that ran here now gets an *actionable* bubble: "Run npm test? (on Mac mini)". It is no longer read-only.
- **`approvalConfirmText(bubble)`** returns "Approve on Mac mini: Run npm test?".
- **`PetBubble`:**
  - With `confirm`, the Approve button ("Approve…") turns the bubble into a confirm step. The step shows the machine name in bold, an "Approve on Mac mini" button and Back; Esc goes back.
  - Only that button calls `onApprove({confirmed: true})`. Deny stays one click.
  - `readOnly` bubbles still have no buttons.
- **`PetStage.onApprove(requestId, {confirmed})`.**
- **Tests:** the visitor bubble carries `confirm`, home bubbles don't, the SSR "Approve…" label and aria-label, and read-only still has no buttons. 73 tests in total.

### Desktop (`apps/desktop`)
- **Strip (`PetApp`):**
  - Approve passes the bubble's `confirmed` through.
  - Failures show the intent text naming the machine, and the bubble's buttons come back (the rejection is passed on).
  - `already_decided` → "Already answered." and the bubble closes at once. The request stays out of the bubble until the mirror's `permission_resolved` removes it.
- **Approval cards (`PermissionCard`, also the pinned strip and the Approvals view):**
  - Remote cards are enabled. An allow option opens an inline confirm in the card: "Allow on **Mac mini**: Run npm test?" with "Allow on Mac mini" / Back.
  - Results show inline: "Already answered.", "Mac mini is offline.", "No answer from Mac mini yet — check that machine.", and the rest.
  - After a successful send the card stays, showing "Sent to Mac mini…", until the mirror resolves it.
  - Offline owners → buttons disabled with "Mac mini is offline".
- **Session view:**
  - Prompt and Cancel are enabled for remote sessions (they are forwarded), with the machine in the placeholder and tooltip and intent-worded errors.
  - Offline → disabled with the reason.
  - History rows say "Approved from Windows PC" (`byMachineId` from the event payload, via `feed.ts`).
- **New session:**
  - A machine picker shows this machine (the default) plus the online paired machines.
  - Agents come from `listAgents({machineId})`, with a loading state, an error state and Reload.
  - The cwd field has a `<datalist>` of that machine's recent folders, and switching machines prefills its latest. Browse is offered only for this machine.
  - A remote start shows "Runs on Mac mini", then a confirm step: "Start Claude Code on **Mac mini** in `/repo`? It runs on Mac mini…".
  - The last agent/cwd settings are saved only for this machine.
- **Notifications:** remote requests no longer say "answer it there". Who decided is in the owner hub's own notification, which every client already shows.
- **`lib/where.ts`:** `answerErrorText(state, id, action, err)` gives intent wording for another machine's item and the plain error for this machine's.
- **Tests:** `where.test.ts` gained the answer-error cases and the feed `byMachineId` case. 23 tests in total.

### VS Code (`apps/vscode`)
- **`host/remote.ts` (new):**
  - `answerRequest(hub, requestId, decision)`, used by the webview cards, the Approve/Deny commands and notifications. A remote approval first opens `showWarningMessage("Approve on Mac mini: Run bun test?", {modal: true, detail}, "Approve on Mac mini")`, and only that button goes ahead.
  - "Already answered" is an info message, not an error.
  - `remoteError` wraps failures in intent text. `confirmModal` is shared.
- **`host/remoteText.ts` (new, pure):** the modal wording for approve and start, and a " · Mac mini" picker suffix.
- **Start Session:**
  - It opens with a QuickPick machine picker (this machine plus online paired machines; skipped with only one), then that machine's agents (`listAgents({machineId})`).
  - The folder comes from a QuickPick of that machine's recent folders or "Enter a folder path…", then the prompt.
  - A remote start then asks in a modal ("Start Claude Code on Mac mini in /repo?") and sends `machineId` + `confirmed: true`.
  - The command also takes an optional 4th argument, `machineId`.
- **Prompt, Cancel, Approve and Deny commands:**
  - They list every reachable machine's items (local first, tagged " · Mac mini"); errors name the machine.
  - Pets stay this machine's (Rename / Change Species).
- **Webview:**
  - Remote approval cards are enabled ("Approve…" with a hint that it asks to confirm). Offline → disabled with the reason.
  - Remote Cancel and the composer are enabled. The transcript shows "· from Windows PC" on decisions.
- **Notifications:** remote requests get Approve/Deny again (approve goes through the modal), and remote turns get Reply when that machine is online.
- **Status bar:** the bell counts all answerable approvals (local plus online remote), and the tooltip says how many are on other machines.
- **Tests and README:** `src/host/remoteText.test.ts` (new) and a `byMachineId` transcript case. 19 tests in total. The README is updated.

## Commands run (final; all pass)

| Where | Command | Result |
|---|---|---|
| packages/hub-client | `bunx tsc --noEmit` | ok |
| packages/hub-client | `bun test` | 158 pass / 0 fail (8 files) |
| packages/pet-engine | `bunx tsc --noEmit` | ok |
| packages/pet-engine | `bun test` | 75 pass / 0 fail (3 files) |
| apps/desktop | `bunx tsc --noEmit` | ok |
| apps/desktop | `bunx tsc --noEmit -p tsconfig.node.json` | ok |
| apps/desktop | `bunx tsc --noEmit -p tsconfig.test.json` | ok |
| apps/desktop | `bun test src` | 23 pass / 0 fail |
| apps/desktop | `bunx vite build` | ok |
| apps/desktop/src-tauri | `cargo check` | ok (no Rust changes this step) |
| apps/vscode | `bunx tsc --noEmit` | ok |
| apps/vscode | `bun test` | 19 pass / 0 fail (4 files) |
| apps/vscode | `bun run build` | ok |
| apps/mobile | `bunx tsc --noEmit` | ok. No fix needed: the positional `startSession` and boolean `listAgents` still work. |

No dependencies were added, removed or upgraded, and no install was run.

Line endings are preserved: CRLF files (NewSessionView, PermissionCard, SessionView, PetApp, panel.css) stay CRLF and new files are LF. No file has mixed line endings.

## Known gaps
- **Not run against the real hub** (it is being built in parallel), and no Tauri window or Extension Development Host was launched. The confirm steps were checked through code, SSR tests and the hub-client unit tests.
- **Some states clear only when the mirror catches up.** The desktop card's "Sent to Mac mini…" state and the strip's closed "Already answered" bubble last until the mirror's `permission_resolved` removes the request. If the mirror never delivers it (the owner went offline right after applying), the card stays in "Sent" until a resync.
- **No folder browser for a remote cwd.** It can't be browsed from here; it is typed or picked from that machine's recent sessions, and the owner validates it (`invalid` → "the folder must exist on it").
- **The machine picker shows only online machines.** `list_agents{machineId}` would also return an offline machine's last-known agents, but starting there would fail with `offline` anyway.

## Decisions for the orchestrator
1. **Client-side `unconfirmed` refusal in hub-client** (no frame sent) on top of the origin and owner checks. This could only bite a caller that deliberately passes a remote allow without `confirmed`, and that is the point.
2. **The bell and pickers show online remote items only.** Offline machines' items stay visible in lists, greyed and disabled. Say if offline items should also be in the VS Code pickers (they would fail with `offline`).

## Follow-up (t13 security review)
1. **Always-allow is chosen only on the owning machine.**
   - hub-client:
     - `remoteOptionBlocked(state, request, optionId)` returns "Choose always-allow on Mac mini" for another machine's allow_always.
     - `approveOptionId(state, request)` returns allow_once; this machine's own request may still fall back to always-allow.
     - `resolvePermission` refuses a remote allow_always locally with `forbidden`, even when confirmed, and sends nothing.
     - A remote `approve()` uses allow_once only, and refuses with `forbidden` when the request offers only always-allow.
   - pet-engine: a visitor bubble without allow_once gets `approveBlocked` (Approve disabled, tooltip "Choose always-allow on Mac mini"; Deny still works).
   - Desktop `PermissionCard` and the VS Code webview card: remote always-allow buttons are disabled with that tooltip, plus a note. VS Code `answerRequest` refuses it before any modal.
2. **`forbidden` explains the owner's policy per action** (`intentErrorText`):
   - start: "Mac mini only lets other machines start agents in folders it already uses, or it does not accept actions from other machines (accept_intents is off in its config.toml)."
   - approve and resolve: "Always-allow must be chosen on Mac mini itself, or …accept_intents…".
   - deny, prompt and cancel: only the accept_intents reason.
3. **Remote start folders:**
   - Desktop: the placeholder and a hint say "Only folders Mac mini already uses". The hint adds "(pick one from the list)", or says to start a session there first when that machine has no recent folders.
   - VS Code: the folder QuickPick says "Only folders Mac mini already uses", with "Enter another folder Mac mini already uses…".
   - Suggestions are still that machine's recent session folders. The owner remains the authority.
4. **Remote-driven sessions ask for every permission:** no client change was needed.
- **Tests:** hub-client 158 (blocked options, the approve option, local `forbidden` refusals with nothing sent, the always-only request, `forbidden` wording per action) and pet-engine 75 (`approveBlocked` in the derived view and the SSR markup).
- **Gates rerun, all green:** hub-client, pet-engine, desktop (three tsc configs, 23 tests, vite build, cargo check), VS Code (tsc, 19 tests, build), mobile tsc.
