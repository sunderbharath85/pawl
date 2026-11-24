Renamed to Pawl on 2026-11-23; names below are pre-rename.

# fe-desktop report: `apps/desktop` (@ai-pet/desktop)

Tauri 2 + React 19 + TypeScript + Vite 8 desktop dog (PLAN.md "Desktop dog", build-order step 2).

## Gate

| Check | Result |
|---|---|
| `bunx tsc --noEmit` (apps/desktop, `src/`) | PASS |
| `bunx tsc --noEmit -p tsconfig.node.json` (vite.config.ts) | PASS |
| `cargo check` (apps/desktop/src-tauri) | PASS |
| `cargo test --lib` (src-tauri) | PASS (4/4) |
| `bunx vite build` | PASS (pet + panel entries; `assets/pets` copied to `dist/pets`) |

## Files created (all under `apps/desktop/`)

- `package.json`: scripts `dev`, `build`, `typecheck`, `tauri`, `tauri:dev`, `tauri:build`, `icons`. Deps: `@ai-pet/{protocol,hub-client,pet-engine}` (`workspace:*`), `@tauri-apps/api`, `@tauri-apps/plugin-{notification,dialog}`, React 19. Dev deps: `@tauri-apps/cli` 2.12, Vite 8, `@vitejs/plugin-react` 6, TypeScript, React/Node types.
- `tsconfig.json` (src) + `tsconfig.node.json` (vite.config.ts).
- `vite.config.ts`: port 1420 (the hub allows that origin), two entries (`index.html` = pet strip, `panel.html` = panel), plus a small plugin that serves repo `assets/pets` at `/pets/` in dev and copies it to `dist/pets` on build. Missing assets are a no-op.
- `index.html`, `panel.html`, `public/favicon.png`
- `scripts/make-icon.py`: draws the 1024² source icon (dog face on a terracotta tile); `tauri icon` generated `src-tauri/icons/*`.
- `src/lib/`
  - `native.ts`: typed `invoke` wrappers and event listeners for the Rust side.
  - `connection.ts`: `HubConnection` owns the `HubClient`. It reads credentials from Rust, probes `/health` natively every 4 s while not connected, reconnects when the hub comes back or the token rotates, and exposes a phase (`booting | hub-down | starting | connecting | connected | unauthorized`).
  - `useConnection.ts`, `useNow.ts`, `sprites.ts` (`loadSpriteManifest("/pets/")`, cached), `pets.ts` (`derivePetViews` wrapper plus a fallback dog when the hub has no pets), `settings.ts` (localStorage prefs synced across both windows), `format.ts`, `ErrorBoundary.tsx`.
  - `notifications.ts`: OS notifications via `@tauri-apps/plugin-notification` for `permission_request`, owned-session `stop` (end_turn and other reasons) and `error` events, and hub `notification` messages. Deduped, and each kind can be toggled in Settings.
- `src/pet/`: the strip window.
  - `PetApp.tsx`: `PetStage` from `@ai-pet/pet-engine/react` (sprites, petSize, approve/deny, pet click opens the panel focused on that pet's session or approval). It sits inside an ErrorBoundary. When the hub is offline it shows a "sleepy dog" with a bubble: *Wake the hub* / *Panel*.
  - `hitRects.ts`: every 80 ms, collects the rects of visible elements narrower than half the strip (pets, bubbles, buttons; `[data-pet-hit]` always counts), dedupes them and sends them to Rust only when they change. It doesn't depend on PetStage's DOM.
  - `pet.css`
- `src/panel/`: the panel window.
  - `PanelApp.tsx` (routing, toasts, `panel-focus` listener), `Sidebar.tsx` (hero pet card, nav with an approvals badge, sessions list with status dots and a "show finished" toggle), `HubBanner.tsx` (hub down → *Start hub* / *Retry*), `PetAvatar.tsx`, `feed.ts` (folds streamed chunks, merges `tool_call` and its updates into one card, latest plan, stops, permissions), `icons.tsx`, `panel.css`.
  - `views/`: `HomeView`, `SessionView` (live event feed with history backfill via `getEvents`, prompt bar, cancel turn, inline pending approvals), `NewSessionView` (agent picker, cwd via the dialog plugin folder picker, first prompt), `ChatView` (chat with a chosen pet, suggestions), `ApprovalsView` + `PermissionCard` (every ACP option as a button, locations, raw input), `PendingStrip` (oldest pending approval pinned on every view), `PetsView` (species dog/cat/bird/robot, name, personality, mood/xp), `RulesView` (edit/reorder/add/delete, `setRules`, enforces the auto-allow hard floor; see below), `SettingsView` (pause pets, pet size, notification toggles, hub info, quit).
- `src-tauri/` (excluded from the root workspace; has its own `[workspace]`)
  - `Cargo.toml`: tauri 2.12 (`tray-icon`, `image-png`, `macos-private-api`), tauri-plugin-notification 2.8, -dialog 2.8, -single-instance 2.5, serde, `dirs = "6"` (same major as the hub, so both resolve the same data dir).
  - `tauri.conf.json`: v2 schema, `panel` window (hidden, closing it hides it), CSP plus `devCsp` (Vite's inline refresh preamble), `macOSPrivateApi`, bundle icons.
  - `capabilities/default.json`: windows `pet` and `panel`: `core:default`, `notification:default`, `dialog:default`.
  - `src/lib.rs`: plugins, managed state, commands, `CloseRequested` → hide panel. On macOS it sets the activation policy to Accessory, because this is a tray app.
  - `src/hub.rs`
    - `hub_credentials` → `{url, wsUrl, token, tokenSource, dataDir}`. Token comes from `AI_PET_HUB_TOKEN`, else `<data_dir>/ai-pet-hub/token`. URL comes from `AI_PET_HUB_URL` in http or ws form, else `http://127.0.0.1:47800`.
    - `hub_health`: a minimal HTTP/1.1 GET `/health` over TcpStream. Done natively because the hub sends no CORS headers.
    - `start_hub`: finds `pet-hub` via `AI_PET_HUB_BIN`, next to the app exe (or `../Resources` on macOS), on PATH (with PATHEXT), or in the repo's `target/{debug,release}` (debug builds only). Spawns it detached (`CREATE_NO_WINDOW`, or its own process group on unix), logs to `<data_dir>/desktop-hub.log`, and waits up to 10 s for `/health`. Never kills the hub on quit.
    - `locate_hub`
    - Unit tests.
  - `src/pet_window.rs`: creates the `pet` window: transparent, undecorated, no shadow, always on top, skip taskbar, all workspaces, `accept_first_mouse`, starts click-through. It's anchored to the primary monitor's **work area**: full width × 220 logical px, sitting just above the taskbar or dock, and re-anchors every ~2 s if the geometry changes. A background thread polls `AppHandle::cursor_position()` every 50 ms, converts it to webview CSS px and calls `set_ignore_cursor_events(!hit)` only on transitions (it also emits `pet-hover`). Commands: `set_hit_rects`, `set_capture_all`, `set_pets_paused`, `get_pets_paused`, `open_panel` (emits `panel-focus`).
  - `src/tray.rs`: tray icon and menu: *Show panel*, *Pause pets* (check item, kept in sync), *Start hub*, *Quit*. Left click shows the panel.
  - `icons/`: generated by `tauri icon`.

## Commands run

- `bash scripts/bun-locked.sh install` (twice: initial, then after adding workspace deps). OK.
- `node_modules/.bin/tauri icon src-tauri/icons/source.png`. OK.
- `cargo check` in src-tauri. OK (first build 1m16s).
- `cargo test --lib` in src-tauri. OK, 4/4 passed (URL normalisation, host:port parsing, HTTP/chunked health parsing, hit-rect padding).
- `bunx tsc --noEmit`, `bunx tsc --noEmit -p tsconfig.node.json`. OK.
- `bunx vite build`. OK (dist ≈ 300 kB JS before gzip, mostly React).

Not run: `tauri dev` / `tauri build`. They would put an always-on-top strip on the shared desktop, and E2E is out of scope.

## Known gaps

- Not exercised against a live hub or live engine yet (E2E out of scope). The engine's `PetStage` was still a stub when last checked. The desktop typechecks against the contract signatures, and an ErrorBoundary keeps the strip usable if the stage throws.
- Hit-testing is heuristic: anything visible and narrower than 50% of the strip is clickable. If PetStage ever renders a wide visible element (e.g. a full-width ground line), that area becomes non-click-through. fe-engine can opt out with `pointer-events: none` on decorative full-width elements, or mark pets/bubbles with `data-pet-hit` (always honoured).
- The strip anchors to the **primary** monitor only. There's no per-monitor choice and no auto-hide-taskbar special-casing (the work area already accounts for it).
- Clicking an OS notification doesn't open the panel: tauri-plugin-notification has no desktop click callback.
- `pet-hub` isn't bundled as a Tauri sidecar (`bundle.externalBin`), because that would break `tauri build` until the hub binary is built for the target triple. The spawn logic already looks next to the app exe, so bundling it later is a config-only change.
- Prompting is offered only for `owned` sessions. Hook-fed sessions are watch-only, as in PLAN.md.

## Follow-up 1: auto-allow hard floor (RulesView)

The change uses `NEVER_AUTO_ALLOW` and `isAutoAllowable` from `@ai-pet/protocol`. No new deps.

- The "Allow" option is disabled (struck through, with a tooltip) when a rule's toolKind is empty ("any tool") or in `NEVER_AUTO_ALLOW` (execute, delete, fetch, other).
- Changing a rule's toolKind to a kind below the floor while it is set to auto_allow switches it to "ask" automatically.
- A rule that already violates the floor (e.g. loaded from an older hub) is outlined red with an inline explanation. Save stays disabled until it is fixed ("Fix N rules to save").
- The old "Careful: …" warning is removed; the header copy now lists the floor kinds.
- If the hub rejects a save with ack error `invalid_rule: <detail>`, the detail is shown in an inline alert above Save. It's read from `HubRequestError.code`, falls back to a generic line when there's no detail, and clears on the next edit. Other errors still go to a toast.
- Gate: `bunx tsc --noEmit` PASS, `bunx vite build` PASS. I checked the error-text extraction with a bun script against three cases: code with detail → detail, bare code → fallback line, unrelated error → null.

## Decisions for the orchestrator

1. **"Pause pets" hides the strip window** (and stops the poller); notifications continue. If you wanted "freeze in place" instead, it's a small change in `pet_window::set_paused`.
2. **Bundling `pet-hub`**: OK to add it as a sidecar (`externalBin`) once sde-hub ships release binaries?
3. **Identifier** `dev.aipet.hub.desktop`: change it if you have a preferred reverse-DNS.
4. fe-engine: please keep full-width decorative stage elements at `pointer-events: none`, or add `data-pet-hit` to pet/bubble roots (optional; the heuristic already works without it).
