# fe-core — Step 1 (Machine identity), client side

Branch: `step-1-machine-identity` (nothing committed; orchestrator reviews and commits).
Scope: `packages/hub-client`, `packages/pet-engine`, `apps/mobile` (type fixes only).

## Gate

| Package | `bunx tsc --noEmit` | `bun test` |
|---|---|---|
| packages/hub-client | exit 0 | 74 pass, 0 fail (was 53) |
| packages/pet-engine | exit 0 | 50 pass, 0 fail (was 39) |
| apps/mobile | exit 0 | `bun run test` exit 0 (0 test files exist; see follow-up below) |

## Files changed

### packages/hub-client
- `src/state.ts`
  - `HubState.machine?: Machine` (this hub; kept across reconnects) and `HubState.machines: Record<MachineId, Machine>` (`{}` initially).
  - `applyWelcome` stores `welcome.machine` and replaces `machines` with the `welcome.machines` snapshot. Our own machine is always included, even if `machines` leaves it out. A pre-v2 Welcome with no `machine` keeps the previous `state.machine`.
  - `reduceServerMsg` handles the new `machine` ServerMsg: it upserts into `machines` and also refreshes `state.machine` when the id is this hub's.
  - `applyAck` folds the `update_machine` ack result in the same way.
  - Events: `mergeEvents` dedupes by the global string `id` and orders by `seq` (ties broken by id), with the same 500 cap. The live-stream fast path is kept.
  - New exports: `latestEventSeq(state, sessionId)` (the `sinceSeq` cursor) and `upsertMachine(state, machine)`.
- `src/client.ts`
  - `getEvents(sessionId, sinceSeq?, limit?)` now sends `sinceSeq`. `sinceId` is gone, as in the protocol.
  - `updateMachine(name): Promise<Machine>` sends `update_machine{machineId: state.machine.id, name}` and folds the ack into state.
    - It trims the name first. A name outside 1–40 code points (Rust `chars().count()` semantics) is rejected locally with code `invalid` and nothing is sent.
    - If called before the Welcome arrives, it waits for it, up to the request timeout.
    - It rejects with `closed` (not connected), `unauthorized`, `timeout`, or `no_machine` (the hub's Welcome had no machine, i.e. a pre-v2 hub).
    - Hub errors such as `not_owner` pass through as `HubRequestError.code`.
  - New exports: `MACHINE_NAME_MAX = 40` and `normalizeMachineName(name)`, for UI validation.
- `src/index.ts`: re-exports the new symbols.
- `test/fixtures.ts`: v2 shapes. Two machine ids (`winpc0000001`, `macmini00002`), `machineId` on every entity, events with `id = "${machineId}:${seq}"` plus `seq`, and `Welcome.machine`/`machines`.
- `test/state.test.ts`, `test/client.test.ts`: migrated to `seq`/global ids, plus new tests:
  - Welcome machine/machines: snapshot replace, self always included, pre-v2 fallback.
  - `machine` message for self and for a peer; `update_machine` ack.
  - Global-id dedupe: a live event plus a `get_events` overlap; the same seq on two machines stays as two distinct events.
  - Late (lower-seq) inserts and cap-by-seq; `latestEventSeq`.
  - `getEvents` sends `sinceSeq` and never `sinceId`.
  - `updateMachine`: happy path, waits for the Welcome, `not_owner` passthrough, local validation, closed / pre-v2 / closed-while-waiting, timeout, `normalizeMachineName` with code points.

### packages/pet-engine
- `src/views.ts`
  - `PetView.machineId` (required) and `PetView.machineName?`.
  - `DerivePetViewsInput.machines?: Record<MachineId, Machine>`.
  - Pets match sessions and pending requests by **(machineId, agentId)**. A pending request also matches when its global `sessionId` is one of this pet's sessions. Two machines' `claude-code` pets never mix.
- `src/react/PetStage.tsx`: `showNames` now defaults to **true**. The label shows `name` only; visitor labels come in step 3. The default stage height grows by the 18 px name room.
- `scripts/preview.tsx`: the literal `PetView` gets `machineId` so it keeps typechecking.
- `test/engine.test.ts`: v2 fixtures, plus a new "derivePetViews across machines" suite (9 tests) that uses the same agent id on two machines:
  - machineId/machineName on views, and an absent machines map
  - sessions don't cross machines
  - a pending request goes only to its machine's pet, including before its session is known
  - per-pet pending counts
  - lastEvents looked up by global session id
  - per-machine decay timers via `nextPetViewChange`
- `test/react.test.tsx`: v2 view fixture, plus tests that names show by default (with no machine name), and that `showNames={false}` hides them and the name room.

### apps/mobile
- No source changes. `bunx tsc --noEmit` already passes against protocol v2. Event ids are only used as React keys (`e${ev.id}`), which works with string ids.
- Follow-up (t13): `package.json` `"test"` changed from `bun test test` to `bun test --pass-with-no-tests ./test`.
  - **Why:** `apps/mobile/test/` is an empty directory. No `*.test.*` or `*.spec.*` file exists anywhere in `apps/mobile`, and git has no history for `apps/mobile/test` (no test file was ever committed). So bun exited 1 with "filters did not match any test files".
  - **What the flag does:** `--pass-with-no-tests` is a built-in bun flag (no new dependency). It only changes the zero-files case.
  - **Checked:** with a temporary failing `test/zz-probe.test.ts`, the script still exits 1 (I deleted the probe afterwards). Any tests added to `test/` later will run and fail normally.
  - **Discrepancy:** `docs/reports/fe-mobile.md:46` says "unit tests in `test/` for encoding, crypto, pairing-link and feed". Those tests are not in the tree or in git history, so they were either never written or lost before the baseline commit. I did not write them, because mobile is frozen and that is outside the "type fixes only" scope. Decide whether step 7 should add them.

## Commands run
- `bunx tsc --noEmit` in hub-client, pet-engine and apps/mobile: all exit 0.
- `bun test` in hub-client (74 pass) and pet-engine (50 pass).
- apps/mobile: `bun run test` exited 1 before the fix and exits 0 after it. With a temporary failing probe test it exits 1, so real failures still fail.
- No installs and no dependency changes.

## Known gaps / notes for other owners
- `apps/vscode/src/webview/App.tsx:159` passes `showNames={pets.length > 1}` explicitly, which overrides the new default. Per CONTRACTS ("show pet names by default"), fe-apps should probably drop it or pass `true`. That file is not mine, so I didn't touch it.
- `apps/vscode/src/extension.ts:85` calls `getEvents(msg.sessionId)` with no cursor. That still compiles; fe-apps may want `latestEventSeq(state, sid)` for incremental backfill. Desktop `backfill.ts` already uses `sinceSeq`.
- Mobile `src/lib/format.ts` looks agents up by `agentId` only. That is harmless on one machine; it needs `(machineId, agentId)` in step 7, since the app is frozen now.
- `PetView.machineName` is filled for every pet when `machines` is passed, including local ones. PetStage doesn't render it yet (step 3 visitors).

## Decisions for the orchestrator
1. **Name validation lives client-side too.** `updateMachine` rejects names that are empty or longer than 40 code points with code `invalid` before sending. sde-hubcore should count `chars()` (not bytes) so the two agree. Tell me if the hub uses a different error code or limit.
2. **`no_machine` error code.** If a pre-v2 hub omits `Welcome.machine`, `updateMachine` rejects with `no_machine`. This is a client-only code, not in the ack reason list.
