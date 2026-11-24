Renamed to Pawl on 2026-11-23; names below are pre-rename.

# fe-vscode report — Pet for VS Code (`apps/vscode`, package `ai-pet-vscode`)

Status: **DONE**. Gate: `bunx tsc --noEmit` PASS, `bun run build` PASS (in `apps/vscode`).

## Files created (all under `apps/vscode/`)

| File | Purpose |
|---|---|
| `package.json` | Extension manifest: displayName "Pet for VS Code", `main: ./dist/extension.js`, `activationEvents: onStartupFinished, onWebviewPanel:ai-pet.petPanel`, `engines.vscode ^1.100.0`; contributes 11 commands, activity-bar container `ai-pet` + webview view `ai-pet.petView`, view-title menus, configuration (7 settings). Scripts: `build`, `watch`, `typecheck`, `package`. |
| `tsconfig.json` | Extends root base; types `node`, `vscode`, `bun`; includes `src`, `scripts`. |
| `scripts/build.ts` | `Bun.build` → `dist/extension.js` (target node, **cjs**, external `vscode`/`bufferutil`/`utf-8-validate`) and `dist/webview.js` (target browser, **iife**, `NODE_ENV` define); copies `src/webview/webview.css` → `dist/webview.css`; copies `assets/pets` (png/webp/gif/svg/json/riv only) → `dist/media/pets` (missing dir is fine). `--watch`, `--dev`. |
| `src/extension.ts` | `activate`/`deactivate`; wires connection, status bar, webviews, notifier, edited-files watcher, commands; routes webview messages to the hub. |
| `src/host/connection.ts` | `HubConnection`: owns the single `HubClient` (`ws` as `WebSocketImpl`, client `"vscode"`). Credentials: settings `ai-pet.hubUrl`/`hubToken` > `readHubCredentials()` (`@ai-pet/hub-client/node`, handles env + token file). Quiet reconnect (hub-client backoff), "offline" only after 3 s grace; missing token / `unauthorized` → polls the token file every 5 s and reconnects when it appears/changes; restarts on settings change. |
| `src/host/statusBar.ts` | Status item: `<pet emoji> <activity>` + `$(bell-dot) N` pending (warning background); offline = `$(debug-disconnect) 🐕 hub offline` with markdown tooltip links **Start hub / Retry / Show pet**; click → `ai-pet.showPet`. Re-renders every 2 s so happy→idle / napping decay. Maintains context key `ai-pet.hasPending`. |
| `src/host/petViews.ts` | `derivePetViews` + `lastSignificantEvents` (pet-engine) → most important pet for the status bar. |
| `src/host/petView.ts` | `WebviewViewProvider` for `ai-pet.petView` (retainContextWhenHidden) + optional editor panel (`AI Pet: Open Pet in Editor`, restored via `WebviewPanelSerializer`). Strict CSP: `default-src 'none'; script-src 'nonce-…'; style-src <cspSource> 'nonce-…'; img-src <cspSource> data: blob:; …`, `localResourceRoots = dist/`. Relays state via `postMessage` (throttled 80 ms, events trimmed to 200/session); sprites = manifest read from `dist/media/pets/manifest.json` (validated with `parseSpriteManifest`) + `asWebviewUri` base. |
| `src/host/commands.ts` | Start Session (agent quick pick → cwd = workspace folder / folder pick / open dialog → prompt input), Prompt Session, Cancel Session, Approve/Deny Pending, Show Pet, Open Pet in Editor, Change Pet Species, Install Agent Hooks, Start Hub, Reconnect. All accept optional args (used by webview/notifications). Offline → "Start Hub" prompt. |
| `src/host/hubProcess.ts` | **Start hub**: `pet-hub serve [--port N]` in a VS Code terminal (`shellPath` = resolved exe), then reconnect. Exe = `ai-pet.hubExecutable` (PATH + PATHEXT lookup) else `target/{release,debug}/pet-hub(.exe)` in a workspace folder. **Install hooks**: target pick → `pet-hub install-hooks <target> [--repo <folder>]` dry-run → output shown in an untitled `diff` doc → modal "Apply" → `--apply`. `.cmd/.bat` shims spawned via shell with quoting. |
| `src/host/editedFiles.ts` | Tracks live `tool_call`/`tool_call_update` per toolCallId; when an `edit` call reaches `completed`, opens its locations inside the workspace per `ai-pet.openEditedFiles` (`open`: preview tab, focus kept, line selected; `diff`: `vscode.diff(gitHEAD, file)`; `off`). 1.5 s per-file cooldown. |
| `src/host/git.ts` | Minimal typed access to the built-in git extension API v1 (`getRepository`, `toGitUri(uri,"HEAD")`); falls back to plain open outside git. |
| `src/host/notifications.ts` | `ai-pet.notifications` = `all`/`permissions`/`off`. Permission requests of **owned** sessions → warning toast with **Approve / Deny / Show** (stale clicks → "already answered"). Owned turn end → info "finished" with **Reply / Show**; refusal/max tokens/error → warning. Hub `notification` msgs (hooks, watchdog) → toast (+Approve/Deny when that session has exactly one pending request). 5 s dedupe per session+kind. A window only notifies for sessions in its workspace, or when focused (`ai-pet.notifyOnlyWorkspaceSessions` to restrict further). |
| `src/host/util.ts` | nonce, path/workspace helpers, `which` (PATHEXT), hub exe resolution, Windows arg quoting, relative time. |
| `src/shared/messages.ts` | Host ⇄ webview message types (`init`, `state`, `focusSession`, `error` / `ready`, `approve`, `deny`, `resolve`, `prompt`, `cancel`, `chat`, `loadEvents`, `openFile`, `command`). Wire types come from `@ai-pet/protocol`/`@ai-pet/hub-client` (not redefined). |
| `src/webview/main.tsx` | React root; calls `configurePetStyles({ nonce })` so pet-engine's injected `<style>` passes the CSP. |
| `src/webview/App.tsx` | `PetStage` (width via ResizeObserver, sprites, names when >1 pet; sleepy placeholder dog when offline / no pets), "Needs you" approvals, Sessions/Chat tabs, footer actions. Persists tab/selection via `vscode.setState`. |
| `src/webview/parts.tsx` | Offline banner (Start hub / Retry), approval cards (Approve, Deny, plus allow/reject-always options), session list (active first, ended toggle), session detail (live transcript, tool rows with clickable file locations, plan, Cancel, composer for owned sessions; backfills via `get_events`), pet chat. |
| `src/webview/transcript.ts` (+ `transcript.test.ts`) | Pure event-log → transcript folding (chunk concat, tool_call_update merge, latest plan). |
| `src/webview/ErrorBoundary.tsx` | A failing PetStage never blanks the view. |
| `src/webview/webview.css` | Theme-aware styles (VS Code CSS variables). |
| `src/webview/vscodeApi.ts` | Typed `acquireVsCodeApi` wrapper, persisted state, CSP nonce reader. |
| `media/pet.svg` | Activity-bar / panel icon. |
| `README.md`, `.vscodeignore`, `.vscode/launch.json`, `.vscode/tasks.json` | Docs, VSIX contents (dist + media only), F5 Extension Development Host config. |

Deps added via `bash scripts/bun-locked.sh add -d … --cwd apps/vscode`: `@ai-pet/protocol`, `@ai-pet/hub-client`, `@ai-pet/pet-engine` (`workspace:*`), `@types/vscode ~1.100.0`, `@types/node ^22`, `ws ^8.23.0`, `@types/ws`, `react`/`react-dom ^19.3.1`, `@types/react(-dom)`. All devDependencies (everything is bundled; package with `vsce --no-dependencies`).

## Commands run + results

- `bunx tsc --noEmit` (apps/vscode) → **PASS** (also clean with `--noUnusedLocals --noUnusedParameters` for files under `src/`/`scripts/`).
- `bun run build` → **PASS**: `dist/extension.js` 106 KiB, `dist/webview.js` 292 KiB, `dist/webview.css`, `dist/media/pets/` (manifest + dog).
- `bun test` → 4 pass / 0 fail (transcript folding).
- Node smoke: `require("dist/extension.js")` with a stubbed `vscode` → `activate`/`deactivate` exported; bundle has no `import.meta`, only `require("vscode")` + node built-ins (+ optional `bufferutil` in ws' try/catch).
- Scratch integration (not committed): bundled `HubConnection` (bun cjs + `ws` + hub-client) against a fake hub WS server under plain Node → hello carries `client:"vscode"` + token, **no `Origin` header**, Welcome applied (agents/pets), `event` messages relayed; after the server dies: quiet reconnect attempts, `offline:true` after the 3 s grace.
- Scratch browser check (Playwright, not committed): `dist/webview.js` + css served behind the exact production CSP with a stub `acquireVsCodeApi` → no CSP violations, pet-engine styles applied via nonce, PetStage renders dog + robot with permission bubble, approval card → posts `{type:"approve"}`, session detail renders transcript/tool row/plan and requests `loadEvents`, offline banner + napping dog → "Start hub" posts `{type:"command",command:"startHub"}`.

## Known gaps

- Not run inside a real VS Code Extension Development Host (E2E out of scope). The VS Code-specific parts (terminal launch, git diff, notifications, view registration) are typechecked against `@types/vscode 1.100` but not exercised live.
- `pet-hub` binary was not built yet, so Start Hub / Install Agent Hooks were not run end-to-end; args match `crates/hub/src/cli.rs` (`serve`, global `--port`, `install-hooks <claude|codex|copilot-cli|copilot-vscode> [--apply] [--repo]`).
- VS Code toasts cannot be closed programmatically: a permission toast stays visible after the request is answered elsewhere; clicking it then says "already answered".
- Hook-based (`vscode_hook`/`cli_hook`) sessions can't be prompted/approved from the pet (by design, PLAN.md); the UI says so.
- `vsce package` will warn/prompt about missing `LICENSE`/`repository`; `publisher` is a placeholder `ai-pet`.
- Sprite manifest is read once per extension activation (rebuild + reload picks up new art).

## Decisions for the orchestrator

1. **Default `ai-pet.openEditedFiles = "open"`** (preview tab, focus kept). Alternatives: `diff` (more useful, noisier) or `off`.
2. **Marketplace identity**: real `publisher`, `license`, `repository` before publishing.
3. **pet-engine source + strict tsconfig**: I dropped `noUnusedLocals` from my tsconfig because pet-engine is consumed as source and `packages/pet-engine/src/react/art.tsx(8,7)` currently has an unused `S` (TS6133). fe-engine may want to fix it.
4. **Hub notifications for owned-session turn ends**: the extension shows its own "finished" toast from the `stop` event and dedupes a hub `notification` for the same session within 5 s. If the hub always emits one, we could rely on it instead.
5. `ai-pet.hubToken` setting exists for remote/dev setups but stores the token in plain settings; README says to leave it empty.
