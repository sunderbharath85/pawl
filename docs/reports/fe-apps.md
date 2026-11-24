# fe-apps: Step 1 (machine identity): desktop + VS Code

Branch `step-1-machine-identity`. Nothing committed. Owned paths only: `apps/desktop` (incl. `src-tauri`) and `apps/vscode`.
I did not touch the `apps/mobile/package.json` change in the tree (`bun test --pass-with-no-tests`). Someone else made it, probably fe-core.

## What changed

### Desktop (`apps/desktop`)
- **Pet names on by default.**
  - Strip: `PetStage showNames={settings.showPetNames}`. The new setting `showPetNames` defaults to `true`, and Settings → "On the strip" has a "Show pet names" toggle for it.
  - Panel: names were already shown in the hero, the home pets, the session header and the pet cards. They now also appear on the New Session agent cards and in turn-finished OS notifications ("Biscuit (Claude Code) finished").
  - `src-tauri/src/pet_window.rs`: `STRIP_HEIGHT` went from 220 to 280. The largest pet (168) plus bubble room (88) plus the name tag (18) is 274. The old value already clipped the bubble at large sizes. The strip is transparent and click-through outside its hit-rects, so the extra height costs nothing.
- **This machine's name.**
  - The panel header is a new `machine-tag` at the top of the sidebar: "THIS MACHINE / Windows PC" plus an OS chip. Its tooltip shows the name, OS and id. Clicking it opens Settings.
  - Settings has a new **This machine** card:
    - A rename field that calls `client.updateMachine(name)`.
    - Inline `field-error` text for an empty name, a name over 40 characters, or a hub rejection. Codes are mapped by `renameErrorText`: `invalid`, `no_machine`, `not_owner`, `not_found`, `unauthorized`.
    - Enter submits and Esc resets.
    - If another client renames the machine, the field follows unless the user has already typed.
    - It also shows the OS and the machine id.
  - The Settings subtitle names the machine.
- **Global ids and seq cursors.**
  - `src/lib/backfill.ts` (new):
    - The first sync of a session per `HubClient` loads the latest window (500, the hub-client cap).
    - Later syncs (reconnects) call `getEvents(sid, latestEventSeq(state, sid), 500)`. If that page comes back full, the client loads the latest window instead, because older events would be trimmed by the cap anyway.
    - Concurrent syncs of the same session share one request chain, and a failed sync is retried as a first sync.
    - `SessionView` uses it.
  - Feed item keys are now the events' global ids (`panel/feed.ts`).
  - Pets and agents are matched by `(machineId, agentId)` via `src/lib/machine.ts` (new: `petFor`, `agentFor`, `checkMachineName`, `renameErrorText`, `OS_LABEL`). This applies in PermissionCard, NewSession, notifications, and in `agentName(..., machineId)` in format.ts, which feeds the Sidebar, Home, Pets, Chat and Session views.
  - `derivePetViews` gets `machines: state.machines`.
  - `FALLBACK_PET` became `fallbackPet(machineId)`. Its client-only id is never sent to the hub.
- **Tests.**
  - New: `src/lib/{backfill,machine}.test.ts`, 13 tests.
  - New `tsconfig.test.json` (bun types). The test files are excluded from the main `tsconfig.json`, so `vite/client` and bun types never mix.
  - `package.json`: added `"test": "bun test src"`, and `typecheck` now also checks the test config.

### VS Code (`apps/vscode`)
- **Pet names on by default.**
  - The webview `PetStage` always shows names. The old `showNames={pets.length > 1}` is gone, as t13 asked.
  - The status bar text leads with the main pet's name ("🐕 Biscuit working").
  - Session rows, the session header and approval cards show "🐕 Biscuit · Claude Code".
  - The agent quick pick shows "Biscuit watches".
- **This machine's name.**
  - The status-bar tooltip header reads "**Krypto** · Windows PC _(Windows)_" with a `$(edit)` link to rename. The offline tooltip shows the last known machine name.
  - The webview has a `machine-bar` header (monitor icon, name, **Rename** button that posts a `renameMachine` webview command).
  - New command **Krypto: Rename This Machine** (`krypto.renameMachine`):
    - It opens an input box prefilled with the current name and validates with `normalizeMachineName` (1–40 characters).
    - It then calls `hub.api.updateMachine` and shows a status-bar confirmation. Errors are mapped to friendly text.
    - It is registered in `package.json` and in the view's `...` menu (`1_pet@4`). It also accepts an optional `name` argument.
- **Global ids and seq cursors.**
  - `extension.ts` `loadEvents` now runs `sessionBackfill.sync(hub.api, sid)` (`src/host/backfill.ts`, the same algorithm as desktop, built on `latestEventSeq`).
  - The webview now asks on every (connected, session) change, so reconnects catch up. The old "once, only if fewer than 20 events" rule is gone.
  - `HubConnection.restart()` creates a new HubClient, so that path starts with a fresh window (tracking is per client, in a WeakMap).
  - Pets and agents are matched by `(machineId, agentId)` via `src/shared/machine.ts` (new) in the notifications, commands, util `agentName`, the webview parts and `onPetClick`.
  - `petViewsOf` and the webview pass `machines`. `emptyHubState`, `EMPTY_STATE` and the placeholder `PetView` were updated for v2.
- **Tests.**
  - New: `src/host/backfill.test.ts` and `src/shared/machine.test.ts`.
  - `transcript.test.ts` was moved to v2 events (string global ids plus `seq`), and a test now checks that row keys are unique and derived from global ids.
  - The VS Code tests total 18.
- **README:** documents the new command and that pets and the machine name are shown.

### Follow-up: pet rename errors (`name_taken` / `invalid`)
- **Shared rule.**
  - `petNames.ts` is new in both apps: `apps/desktop/src/lib/` and `apps/vscode/src/shared/` (identical copies).
  - It mirrors the hub: the name is trimmed and must be 1–40 code points. It must also be unused by every other known pet on every machine. The check ignores case (it matches how the brain seeds names); a pet may re-case its own name.
  - `petRenameErrorText` maps the hub's `name_taken` and `invalid` acks to friendly text, e.g. "Another pet already answers to “Pepper”. Pick a different name." and "Pet names need 1–40 characters." Other errors fall through.
- **Desktop Pets view** (`PetCard`):
  - Inline `field-error` under the name field. It shows live while typing (taken or too long), and also when the hub rejects a save with `name_taken` or `invalid`.
  - The input gets an `is-invalid` border and `aria-invalid` / `aria-describedby`.
  - Save is disabled while the live check fails, Enter saves, and Reset clears the error. Errors that aren't about the name still show as toasts.
  - Dropped the UTF-16 `maxLength={40}` (it blocked emoji names); validation covers the limit.
- **VS Code.** It had no pet-rename path, and PLAN.md says names are editable in VS Code, so I added one.
  - New command **Krypto: Rename Pet** (`krypto.renamePet`, optional args `petId` and `name`):
    - It picks the pet with a quick pick (shared `pickPet`, also used by Change Species), then opens an input box.
    - `validateInput` shows the same messages inline in the input box.
    - It calls `updatePet(petId, {name})`. Hub rejections are reported as "Krypto: <friendly text>".
  - Registered in `package.json` and in the view's `...` menu (`1_pet@0`).
  - Webview footer link **Rename pet** (a `renamePet` webview command).
  - README updated.
- **Tests:** `petNames.test.ts` in both apps (6 tests each).

## Commands run (final; all pass)

| Where | Command | Result |
|---|---|---|
| apps/desktop | `bunx tsc --noEmit` | ok |
| apps/desktop | `bunx tsc --noEmit -p tsconfig.node.json` | ok |
| apps/desktop | `bunx tsc --noEmit -p tsconfig.test.json` | ok |
| apps/desktop | `bunx vite build` | ok (built in ~0.2 s) |
| apps/desktop | `bun test src` | 19 pass / 0 fail (3 files) |
| apps/desktop/src-tauri | `cargo check` | ok |
| apps/vscode | `bunx tsc --noEmit` | ok |
| apps/vscode | `bun run build` | ok (extension, webview, pets copied) |
| apps/vscode | `bun test` | 24 pass / 0 fail (4 files) |

These results are from the rerun after the pet-rename follow-up.

No dependencies were added, removed or upgraded, and no install was run.

## Known gaps
- Nothing has been launched yet: no Tauri window and no Extension Development Host. E2E is out of scope. Layout (the sidebar machine tag, the 280 px strip, the webview header) has been checked by reading the code and CSS only.
- `pet-engine`'s name label shows `name` only. There is no "· Mac mini" suffix and no visitors area (that is step 3, by design).
- The native panel window title is still "Krypto". Putting the machine name there would need `core:window:allow-set-title` in capabilities. I left capabilities alone; the name is shown in the UI.
- The Tauri tray tooltip does not name the machine. That would need a Rust↔webview round trip; low value for step 1.
- `machine.ts` and `backfill.ts` are duplicated between desktop and VS Code, because neither app may own a shared package. They are small and identical.

## Decisions for the orchestrator
1. **Shared helpers:** consider moving `petFor`/`agentFor`/`renameErrorText` and the backfill algorithm (`SessionBackfill`) into `@krypto/hub-client` (fe-core), so both apps (and mobile later) import one copy.
2. **The desktop "Show pet names" toggle** (default on) is kept as a user preference. VS Code has no toggle (names are always on, per t13). Say if desktop should drop it too.
3. **`STRIP_HEIGHT` 220 → 280** in `src-tauri`. Please confirm you're fine with a taller transparent strip window.
4. **Pet name uniqueness ignores case** on the client, so with "Biscuit" taken, "biscuit" is blocked before it reaches the hub. If the hub's `name_taken` check is case-sensitive, the client is the stricter of the two. Please align the hub or tell me to relax the client check.
5. **`petNames.ts` could move into `@krypto/hub-client`** (as `PET_NAME_MAX` / `normalizePetName` / `checkPetName`), alongside `normalizeMachineName`.
