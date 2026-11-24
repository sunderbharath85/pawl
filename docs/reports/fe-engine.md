Renamed to Pawl on 2026-11-23; names below are pre-rename.

# fe-engine report: `@ai-pet/hub-client` + `@ai-pet/pet-engine`

Status: **DONE**. Both packages implement the CONTRACTS.md APIs. Both gates pass: `bun run typecheck` and `bun test`.

## Files

### `packages/hub-client` (`@ai-pet/hub-client`)
| File | What it holds |
|---|---|
| `package.json` | Exports `.` (core), `./react`, and `./node`. Depends on `@ai-pet/protocol: workspace:*`. Optional peer dependency on react >=18. |
| `tsconfig.json` | Extends `../../tsconfig.base.json`, adds `noUnusedLocals` and `noUnusedParameters`. |
| `src/index.ts` | Framework-free core entry. No React or Node imports. |
| `src/client.ts` | `HubClient`, `HubRequestError`, `pickPermissionOption`. |
| `src/state.ts` | `HubState`, `initialHubState`, `applyWelcome`, `reduceServerMsg` (covers every `ServerMsg`), `applyAck`, `mergeEvents`, `lastEventsBySession`, `DistributiveOmit`, `DEFAULT_LIMITS`. |
| `src/url.ts` | `DEFAULT_HUB_WS_URL`, `DEFAULT_HUB_HTTP_URL`, `toHubWsUrl`, `toHubHttpUrl`. |
| `src/health.ts` | `fetchHubHealth(url?, timeoutMs)`: `GET /health`, returns `null` when the hub is unreachable. |
| `src/react.ts` | `useHubState(client)` (built on `useSyncExternalStore`), plus `useHubSelector`. |
| `src/node.ts` | `readHubCredentials`, `tryReadHubCredentials`, `hubDataDir`, `osDataDir`, `hubTokenFile`, `HubCredentialsError`. |
| `test/*.test.ts` | 53 tests: reducers, a fake-socket client (handshake, correlation, queueing, timeout, reconnect, unauthorized, approve/deny, listeners), Node credentials, URL helpers. |

### `packages/pet-engine` (`@ai-pet/pet-engine`)
| File | What it holds |
|---|---|
| `package.json` | Exports `.` and `./react`. Script `preview` (see below). |
| `src/activity.ts` | `PetActivity`, `activityForEvent`, `activityForStatus`, `lastSignificantEvents`, the decay constants (`HAPPY_DECAY_MS` 6 s, `SAD_DECAY_MS` 10 s, `NAP_AFTER_MS` 60 s). |
| `src/views.ts` | `PetView`, `derivePetViews` (pure; time comes only from `now`), `nextPetViewChange`, `describePermission`. |
| `src/lines.ts` | `SPECIES_META`, `petLine` (deterministic by seed), `PET_LINES` (4 species × 8 activities), `hashString`. |
| `src/sprites.ts` | `SpriteManifest`, `PetSpritesConfig`, `loadSpriteManifest` (validated, cached, never rejects), `parseSpriteManifest`, `resolveSpriteAnimation`, `spriteUrl`. |
| `src/react/art.tsx` | Hand-built SVG dog (the hero), cat, bird and robot. Each also has a bed, nest or charging pad, and the effects listed below. |
| `src/react/styles.ts` | `PET_CSS`, injected once as `<style id="ai-pet-engine-styles">`. Also `ensurePetStyles`, and `configurePetStyles({nonce, inject})` for CSP. |
| `src/react/PetSprite.tsx` | Draws the sprite strip with a CSS `steps()` animation once the image has preloaded. Falls back to the SVG while the image is loading, if it fails, or if the manifest has no entry. |
| `src/react/PetBubble.tsx` | Say bubble or permission bubble with Approve/Deny, busy state, and `data-pet-hit="bubble"`. |
| `src/react/PetStage.tsx` | Horizontal strip with one lane per pet. Idle pets wander and walk with the walk cycle. Bubbles are clamped to the strip. Each pet root has `data-pet-hit="pet"`. The stage root has `pointer-events:none`. |
| `src/react/hooks.ts` | `usePetViews(hubStateLike)`: derives the views and sets one timer for the next decay or nap. Also `usePrefersReducedMotion`, `usePetStyles`, `useImageStatus`. |
| `src/react/stage-logic.ts` | Pure lane, wander and bubble-placement helpers (unit-tested). |
| `scripts/preview.tsx` | `bun run preview -- out.html` writes a static page with every species × activity, for visual QA. |
| `test/*.test.ts(x)` | 39 tests: event mapping, view derivation with decay timers, lines, manifest parse/load, server-side renders of every species × activity, bubble, stage layout, stage logic. |

**Effects per activity in the SVG fallback:**
- napping: lies in bed, eyes closed, floating zzz.
- thinking: head tilt, ears up, "?" thought cloud.
- working: digs, front paws flying, dirt thrown backwards. The bird pecks; the robot hammers.
- planning: carries a checklist and its ticks animate.
- alert: bark and bounce, "!" plus the species sound ("WOOF!"), mouth open.
- happy: fast tail wag, zoomies that run and flip, tongue out, hearts.
- sad: droops, tear, rain cloud, slight whimper.
- walk: alternating legs and a body bob.

Under `prefers-reduced-motion`, every animation and transition stops but the static poses stay. Wandering is off too.

## Commands and results
- `bash scripts/bun-locked.sh install`: ok. The workspace uses isolated installs (`node_modules/.bun`).
- `cd packages/hub-client && bun run typecheck && bun test`: clean; **53 pass, 0 fail**.
- `cd packages/pet-engine && bun run typecheck && bun test`: clean; **39 pass, 0 fail**.
- `bunx tsc --noEmit -p . --verbatimModuleSyntax --exactOptionalPropertyTypes --noUncheckedIndexedAccess --noPropertyAccessFromIndexSignature --noImplicitOverride` in both packages: `src/` is clean. Consumers compile this source with their own flags, which is how the TS6133 that fe-vscode reported came about. It is fixed, and `noUnused*` is now on in both tsconfigs.
- `cd apps/desktop && bunx tsc --noEmit` and `cd apps/vscode && bunx tsc --noEmit` (read-only check): both clean against the current packages.
- Visual check: rendered `scripts/preview.tsx` and took a Playwright screenshot of all 4 species × 9 states. I fixed one issue: the alert sound text spilled outside the pet's box. The temporary `.playwright-mcp/` files at the repo root have been removed.

## Behaviour decisions (please confirm or adjust)
1. **`readHubCredentials()` is not specified in CONTRACTS.** I made it synchronous. It returns `{url (ws), httpUrl, token, tokenSource, dataDir, tokenFile}` and throws `HubCredentialsError` when there is no token. `tryReadHubCredentials()` returns `null` instead of throwing. The env vars `AI_PET_HUB_TOKEN` and `AI_PET_HUB_URL` override the file and default; the URL can be in http or ws form. The OS data dir matches `dirs::data_dir()`, and a relative `XDG_DATA_HOME` is ignored, as the XDG spec requires. fe-vscode already uses it inside a try/catch. **Please add this to CONTRACTS.**
2. **Additions beyond the contract** (all optional or extra, none breaking):
   - `HubState.hubVersion` and `HubState.lastError`.
   - `HubClient.setToken` (also reconnects after an `unauthorized`), `listSessions`, `setRules`, the `wsUrl` getter, and `getEvents(sessionId, sinceId?, limit?)`.
   - `PetView.skin`, passed through from `Pet.skin`.
   - Optional `PetSprite`/`PetStage` props: `sprites`, `petSize`, `showNames`, `wander`, and others.
   - `PetStage`/`PetBubble` callbacks may return a Promise. While it is pending the buttons stay disabled, and they re-enable if it rejects.
3. **`approve()`** picks `allow_once` and falls back to `allow_always`. It rejects with code `unknown_request` or `no_allow_option` when it can't pick. **`deny()`** picks `reject_once`, otherwise `null`, as the contract says.
4. **Request handling:**
   - Requests made while connecting or reconnecting are queued and sent after the hello completes.
   - Requests already sent when the connection drops reject with code `disconnected`. They are not retried, because they are not idempotent.
   - `ok:false` rejects with `code = ack.error`. Timeouts reject with code `timeout` after 15 s.
   - `unauthorized` sets that status and stops reconnecting; calling `connect()` or `setToken()` resumes.
   - A successful ack is also folded into state, so a started session, a fetched event list or an updated pet appears even if the hub doesn't broadcast it. A `resolve_permission` ack removes the pending item right away.
5. **Caps:** 500 events per session (the contract value), 300 chat messages and 100 notifications, newest kept. Events are deduped by id and kept in ascending order.
6. **Event to activity mapping** (beyond the PLAN table):
   - `tool_call` with kind read, search, think or switch_mode → thinking ("sniffing"); every other kind → working.
   - `agent_message_chunk` and `user_message` → thinking.
   - `stop` cancelled → idle; max_tokens or max_turn_requests → sad.
   - `notification` events → alert.
   - Hook names: Stop / agent-turn-complete → happy; PreToolUse → working; UserPromptSubmit → thinking; Notification → alert.
7. **View priority**, highest first:
   1. Pending permission: alert with an Approve/Deny bubble. The oldest request comes first, with "+N more" when there are several.
   2. A busy session (status thinking, working or waiting_permission): the status, refined by the last event (thinking, working or planning).
   3. Recent end_turn: happy for 6 s. Recent error: sad for 10 s.
   4. Idle.
   5. Napping, once nothing has happened for 60 s.

   A hook-tracked `waiting_permission` with no `PermissionRequest` shows alert with a "Waiting for you in VS Code / the terminal" bubble. **An open idle session still lets the pet nap**, because "active" means busy.
   For `lastEvents`, pass `lastSignificantEvents(state.events)` (pet-engine), which skips noise such as `permission_resolved`. `lastEventsBySession` (hub-client) also works.

## Known gaps
- I haven't run anything against a live hub; E2E is out of scope. The WebSocket logic is covered only by an in-memory fake socket. The Node `ws` package and browser sockets both satisfy the `onopen`/`onmessage`/`onclose`/`onerror` surface used here.
- Sprite strips use `var()` inside `@keyframes`, which current Chromium, WebKit and Gecko support. I haven't tested this with real art-sprites PNGs yet because none had been generated.
- Skins are SVG palettes only:
  - dog: golden, black, white, choco
  - cat: orange, black, white
  - bird: yellow, green, pink
  - robot: gold, blue, retro

  Sprite strips ignore skin, because the manifest layout has no per-skin animations.
- Rive is not integrated; the PLAN calls it "later".

## For dependents
- VS Code webview CSP: call `configurePetStyles({ nonce })` before rendering, or `configurePetStyles({ inject: false })` and ship `PET_CSS` yourself. Inline `style` props are set through CSSOM, so they are CSP-safe.
- Desktop click-through: hit targets are `[data-pet-hit="pet"]` and `[data-pet-hit="bubble"]`, and both also carry `data-pet-id`. Everything else in the stage has `pointer-events:none`.
