# fe-apps: Step 3 (mirror events + visitors): hub-client, pet-engine, desktop, VS Code

Branch `step-3-mirror-visitors`. Nothing committed.

I edited only my owned paths: `packages/hub-client`, `packages/pet-engine`, `apps/desktop` (incl. `src-tauri`) and `apps/vscode`. I coded against the protocol v4 types and the CONTRACTS.md "Step 3" section, not against a running hub, because sde-hubcore and sde-peer are building their side in parallel.

Files I did not touch: the modified `PLAN.md`, `docs/CONTRACTS.md`, `packages/protocol`, `crates/protocol` and `crates/brain` files in the tree.

## What changed

### hub-client (`packages/hub-client`)

**Helpers moved in.** The contract asked for these to move, and each app's copy is deleted.

| New file | What it holds | Former copies |
|---|---|---|
| `src/machine.ts` | `petFor`, `agentFor`, `renameErrorText`, `checkMachineName`, `OS_LABEL`, `machineDisplayName`, `machineSummary`, `UNKNOWN_MACHINE_NAME`, `machinesLine(total, online?)`. `MACHINE_NAME_MAX` and `normalizeMachineName` moved here from `client.ts` (same exports). | desktop `lib/machine.ts`, VS Code `shared/machine.ts` (VS Code keeps only its own `presenceLabel`) |
| `src/petNames.ts` | pet-name rules | desktop `lib/petNames.ts`, VS Code `shared/petNames.ts` |
| `src/backfill.ts` | `SessionBackfill`, `sessionBackfill`, `backfillSession` | desktop `lib/backfill.ts`, VS Code `host/backfill.ts` |

The tests moved with them: `test/machine.test.ts`, `test/petNames.test.ts` and `test/backfill.test.ts`.

**New:**
- **`activeMachineId(state, now)`** (where the user is):
  - It picks the online machine with the latest `userActiveAt` within `USER_ACTIVE_WINDOW_MS`. This machine always counts as online.
  - If no machine qualifies, it returns this machine. Ties go to this machine, then to the lower id.
  - Before the hub's hello it returns `""`.
- **`isLocal(state, id)`**: true when a machine id or any global id belongs to this machine. Everything counts as local before the hub's hello.
- **`groupByMachine(state, items)`**: groups items with this machine first, then online machines, then by name, and unknown machines last. Each group says whether it is local and whether it is online.
- **`readOnlyReason(state, machineId)`**: `null` for this machine's items. For another machine's it returns "Open on Mac mini to act — remote control arrives in a later update."
- **`HubClient.reportUserActivity(now?)`**:
  - It sends `user_activity` at most once per `USER_ACTIVITY_REPORT_MS`, and only while connected (nothing is queued).
  - The first input after each hello is reported at once.
  - A rejection (from an older hub) is swallowed.

**Removed:** `scopeToMachine` (the step-2 filter) and its tests. Welcome, reducers and mirrored ServerMsgs needed no change, because mirrored items arrive as ordinary messages.

### pet-engine (`packages/pet-engine`)

**`PetView` gains:**
- `visitor: boolean` (the pet's machine is another one, and online);
- `away?: MachineId`;
- `remoteOffline: boolean` (the pet's machine is another one, and offline or unknown).

`PetBubbleData` gains `readOnly?`. `derivePetViews` input gains optional `localMachineId` and `activeMachineId` (the active machine defaults to the local one). Without `localMachineId` every pet is a home pet, as before.

**Rules,** on local machine L with active machine A:
- **Home pet with a pending approval:** `away = A` when A ≠ L. Its bubble data is kept for lists, but the stage hides it.
- **Visitor with a pending approval:**
  - When A == L, it shows a read-only permission bubble, "Run tests? Needs approval on Mac mini."
  - Otherwise it is in the waiting pose (`alert`) with no bubble.
- **Visitors without an approval** keep their activity but drop chatter bubbles.
- **Pets of offline machines** nap with no bubble, `since` = the machine's last seen, and no session.

**`PetStage`:**
- **Home pets** keep their lanes. The lanes now shrink to leave room for the visitors area on the right.
- **Visitors area:**
  - Visitors are drawn at 0.75× size with a "Biscuit · Mac mini" name tag.
  - At most `maxVisitors` (default 6) are shown, never more than half the strip. The rest go into a "+N" chip whose tooltip lists the hidden names.
  - Visitors with a request come first.
- **Away pets** run off the left edge, become hidden (and drop out of the hit-rects) once there, and walk back when no longer away.
- **A visitor that brings a request** runs in from the right edge. Its bubble appears once it arrives.
- **Offline machines' pets** are not drawn.

The walks to and from the edges are a new kind of motion (`Motion.travel`) that the wander rules never stop. With reduced motion, pets just appear and disappear.

`PetBubble` shows no Approve/Deny on `readOnly` bubbles (role `status`, its own style). New exports: `layoutVisitors`, `MAX_VISITORS`, `VISITOR_SCALE`, `VisitorLayout`, and `stageShowsApproval(views)` (true while a stage shows an approval: a home pet's permission bubble that is not `away`, or a visitor that ran here).

**Tests:** 12 tests for the L/A rules in `engine.test.ts`, visitor stage tests (badge, offline hidden, +N, read-only bubble, away hidden), and layout and walk tests.

### Desktop (`apps/desktop`)

**Step-2 filters removed** (`useLocalHubState`, the `isRemote` guards). Panel and strip use `useHubState`.

**Strip (`PetApp`, `lib/pets.ts`):**
- `derivePetViews` gets `localMachineId` and `activeMachineId`.
- The placeholder dog stays home when this machine has no pet of its own, even when visitors are present.
- Clicking a visitor's bubble opens the panel at that request (the existing `approvals` focus).

**`src-tauri/src/pet_window.rs`:**
- **User activity.** The existing 50 ms cursor poll now emits `user-activity`, at most every 5 s and only for moves of 2 px or more. This continues while pets are paused.
- **Following the cursor.** New command `set_follow_cursor(follow)`. While it is on, the strip re-anchors every ~250 ms to the work area of `app.monitor_from_point(cursor)` (Tauri 2.12 API). It falls back to the primary monitor.
- `PetApp` turns following on only while the strip shows an approval: a home pet's request that is not `away`, or a visitor that ran here with its request (`stageShowsApproval`, below). It turns following off on unmount.
- `lib.rs` registers the command. There is a unit test for the cursor-jitter threshold.

**Activity reporting:** the strip calls `client.reportUserActivity()` on `user-activity`. The panel calls it on pointer, key, wheel and focus events.

**Panel:**
- **Sidebar:**
  - Sessions are grouped by machine, with headings only once another machine's sessions are mirrored.
  - An offline machine's group is greyed with "last seen 5m ago".
  - The hero pet is chosen from home pets.
- **Session view:**
  - The header shows a machine chip, which says "offline, last seen …" when that machine is offline.
  - For a remote session the prompt bar is disabled, with the read-only reason as its placeholder and tooltip, and so is Cancel turn.
  - An offline machine's feed is greyed.
- **`feed.ts`:** a `TruncatedPayload` becomes a "📦 Large output — open on Mac mini" row. A run of them merges into one row, and the tooltip gives the size.
- **Approval cards** (PermissionCard, the Approvals view and the pinned strip): another machine's request shows "on Mac mini". Its buttons are disabled with the reason and a note below. Offline machines' cards are greyed. This machine's requests sort first.
- **Pets view:**
  - Pets are grouped by machine ("Visiting from Mac mini").
  - Remote pets are read-only (inputs and species buttons disabled, Save hidden, reason shown).
  - Offline groups are greyed.
- **Home view:** home pets go on the rug, and remote sessions get a machine chip.
- **Only this machine's items where acting is needed:** Chat lists only this machine's pets, and New Session offers only this machine's agents.

**Notifications** (`lib/notifications.ts`, `lib/where.ts` new):
- They cover every machine's events, but only on the active machine (`notifiesHere`).
- Remote items say "on Mac mini". A remote request's notification says to answer it on that machine.
- Events and hub notifications older than 2 min are skipped, so a mirror sync after a reconnect does not flood the user.

**Tests:** `src/lib/where.test.ts`: where to notify, the "on Mac mini" text, the placeholder-dog rule, and the truncated feed row. The old copied tests moved to hub-client.

### VS Code (`apps/vscode`)

**Step-2 filters removed** (`HubConnection.local`, the notifier's `remote()`).

**Pets** (`petViewsOf` and the webview): `derivePetViews` gets `localMachineId` and `activeMachineId`. The webview `PetStage` shows visitors and away pets the same way the desktop strip does. The sleepy dog stays home when this machine has no pet of its own.

**Status bar:**
- The lead pet is a home pet, with "(ran to Mac mini)" when it is away.
- The bell counts only this machine's approvals, the ones Approve and Deny act on. The `krypto.hasPending` context follows the same count.
- The tooltip lists home pets (an away pet says "ran to Mac mini with a request"), a "Visiting: …" line, and "N waiting on other machines".

**Webview:**
- Sessions are grouped by machine, with headings once more than one machine is present. Offline groups are greyed with "last seen".
- Each remote item gets a machine chip.
- For remote sessions, Cancel and the composer are disabled with the reason.
- For remote approval cards, the buttons are disabled with the reason, and offline cards are greyed.
- A `TruncatedPayload` shows as "📦 Large output — open on Mac mini" in the transcript (merged per run).
- Chat, Rename pet and Change species use only this machine's pets.
- Clicking a visitor with no session opens the sessions list, not a chat with a remote pet.

**Commands:**
- Prompt, Cancel, Approve and Deny pickers list only this machine's items, and an explicit remote id fails with the read-only reason.
- Approve/Deny with only remote requests waiting says they are on other machines.
- Rename Pet and Change Species pick only this machine's pets.
- Start Session offers only this machine's agents.

**Notifications:**
- They cover every machine, but only when this machine is the active one.
- Remote items say "on Mac mini". Remote requests get only "Show" (no Approve/Deny), and remote turn-finished notifications get no "Reply".
- Events and notifications older than 2 min are skipped.

**Activity (`host/activity.ts`, new):** reports window focus, text edits, selection and active-editor changes, and a reconnect, but only while the window is focused. Files an agent rewrites in the background therefore don't count.

**Tests and README:** `src/host/petViews.test.ts` (visitor, away, remoteOffline) and a truncated-payload test in `transcript.test.ts`. The README is updated.

## Commands run (final; all pass)

| Where | Command | Result |
|---|---|---|
| packages/hub-client | `bunx tsc --noEmit` | ok |
| packages/hub-client | `bun test` | 135 pass / 0 fail (7 files) |
| packages/pet-engine | `bunx tsc --noEmit` | ok |
| packages/pet-engine | `bun test` | 71 pass / 0 fail (3 files) |
| apps/desktop | `bunx tsc --noEmit` | ok |
| apps/desktop | `bunx tsc --noEmit -p tsconfig.node.json` | ok |
| apps/desktop | `bunx tsc --noEmit -p tsconfig.test.json` | ok |
| apps/desktop | `bun test src` | 20 pass / 0 fail (2 files) |
| apps/desktop | `bunx vite build` | ok |
| apps/desktop/src-tauri | `cargo check` (and `cargo check --tests`) | ok |
| apps/vscode | `bunx tsc --noEmit` | ok |
| apps/vscode | `bun test` | 15 pass / 0 fail (3 files). Lower than step 2 because 19 tests moved to hub-client. |
| apps/vscode | `bun run build` | ok |
| apps/mobile | `bunx tsc --noEmit` | ok. No fix needed: it builds its state with `initialHubState()` and doesn't use `PetView`. |

No dependencies were added, removed or upgraded, and no install was run.

Line endings are preserved: CRLF files stay CRLF, new files are LF, and no file has mixed line endings.

Nothing logs mirrored payloads, invite URIs or tokens. The new code adds no `console.*` or `log.*` calls.

## Known gaps
- **Not run against the real hub** (it is being built in parallel), and no Tauri window or Extension Development Host was launched. The runs in and out, the visitors area and following the cursor across monitors have only been checked by reading the code and the SSR tests.
- **Desktop activity** is global cursor movement plus input in the panel. There is no global keyboard hook, which would need OS hooks or a new dependency. Typing elsewhere without moving the mouse doesn't count.
- **`activeMachineId` compares `userActiveAt` values stamped by different hubs' clocks.** If machines' clocks differ by more than the gap between two activity reports, the wrong machine can be chosen. See decision 1.
- **The 2-minute freshness window** also uses the owner machine's clock (`event.ts`). A machine whose clock is more than 2 minutes behind would have its notifications dropped.
- **Remote request bubbles on the strip are read-only** by design (step 4 turns on approving remotely).

## Decisions for the orchestrator
1. **Clock skew in "where the user is".** The hub could rewrite a peer's `userActiveAt` (and maybe `lastSeen`) into its own clock, using the peer's presence `ts` against the arrival time. Then `activeMachineId` compares like with like. The clients take `Machine.userActiveAt` as given.
2. **`Welcome.agents` for remote machines** is not specified. The clients cope either way: they start sessions only with this machine's agents, and agent-name lookups fall back to an id-only match.
3. **Follow-the-cursor (narrowed, per t13).** The strip now follows only while it shows an approval: a home pet's request (not `away`) or a visitor that ran here. A pet-engine test covers the rule: A == L or A != L, home or visitor, an offline machine, and say-bubbles.
4. **Notification freshness (2 min)** is a client rule against sync floods. If the hub marks synced items (for example a `replay` flag on mirrored ServerMsgs), the clients could use that instead.

## Follow-up (t13 review)
- **Decision 1:** the hub will normalize peers' `userActiveAt` / `lastSeen` into its own clock (sde-hubcore). The clients keep using the values as given.
- **Decision 2:** `Welcome.agents` stays this machine only in step 3. The client filters stay as a safety net.
- **Decision 3:** follow-the-cursor is narrowed to `stageShowsApproval(views)`, with a test (`engine.test.ts`). The comments in `pet_window.rs` and `native.ts` are updated.
- **Decision 4:** the 2-minute freshness rule stays.
- **Gates rerun, all green:**
  - hub-client: tsc ok, 135 tests.
  - pet-engine: tsc ok, 71 tests.
  - desktop: three tsc configs ok, 20 tests, `vite build` ok, `cargo check` ok.
  - VS Code: tsc ok, 15 tests, build ok.
  - mobile: tsc ok.
