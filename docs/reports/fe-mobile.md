Renamed to Krypto on 2026-11-23; names below are pre-rename.

> **Correction (2026-11-23, orchestrator):** the unit tests this report mentions for encoding/crypto/pairing/feed were never committed; `apps/mobile/test` is empty. Writing them is part of migration step 7.


# fe-mobile report: `apps/mobile` (`@ai-pet/mobile`)

Status: **FROZEN** at the orchestrator's request (t13, 2026-11-23). The user is moving to multi-machine hubs and the relay/pairing design will change. I stopped mid-build. No code changes were made after the freeze, except a one-line type fix in `src/lib/feed.ts` so the file I was in typechecks.

## Where things stand

- **Typecheck:** `cd apps/mobile && bunx tsc --noEmit` exits 0.
- **Libraries vs screens:** the libraries and pet rendering are written and typecheck. **No screens exist.** `src/app/` is empty, so the app does not launch yet.
- **expo-doctor:** not run. It would complain about the missing routes.
- **Relay spec:** `docs/reports/sde-relay-protocol.md` did not exist yet. All relay code follows CONTRACTS.md "Relay" only.

## Files created (all inside `apps/mobile/`)

| File | Status | What it is |
|---|---|---|
| `package.json` | done | `@ai-pet/mobile`. Expo SDK 58 (`expo ~58.0.26`, `expo-router ~58.0.32`, RN 0.88.1, React 19.3.0). Versions come from expo 58's `bundledNativeModules.json`. Scripts: `start`/`ios`/`android` (each runs `copy-pets` first), `copy-pets`, `typecheck`, `test`, `doctor`. |
| `app.json` | done | Scheme `aipet` (deep link `aipet://pair?...`), bundle/package id `dev.aipet.hub`, iOS + Android only. Plugins: expo-router, splash (light and dark), expo-camera (camera permission text, mic off), expo-local-authentication (Face ID text), expo-notifications, expo-secure-store. `typedRoutes` and `reactCompiler` are on. |
| `tsconfig.json` | done | Extends `expo/tsconfig.base`, strict, `noUncheckedIndexedAccess`, `@/*` → `src/*`. Excludes `test/` and `scripts/`. |
| `metro.config.js` | done | Expo defaults, plus one override: tweetnacl's Node-only `require('crypto')` resolves to an empty module. |
| `expo-env.d.ts`, `.gitignore` | done | Standard Expo files. |
| `scripts/copy-pets.ts` | done | Copy step. Copies repo `assets/pets/**` (png + manifest) to `apps/mobile/assets/pets/`, validates the manifest with pet-engine's `parseSpriteManifest`, and generates `src/generated/pet-sprites.ts` with static `require()`s, since Metro needs those. It has been run once (37 files copied). |
| `assets/images/{icon,adaptive-icon,splash-icon,hero-dog}.png` | done | Generated with PIL from the dog's `happy` sprite frame. |
| `assets/pets/**` | generated | Copy of the art-sprites output. |
| `src/generated/pet-sprites.ts` | generated | Species × animation → `{source, frames, fps, loop}`. |
| `src/lib/relay/encoding.ts` | done | Pure-JS base64 (standard; URL-safe input accepted), UTF-8 and hex. No dependence on atob/TextEncoder, so it behaves the same on Hermes and bun. |
| `src/lib/relay/crypto.ts` | done | tweetnacl `nacl.box` wrapper. `BoxChannel` precomputes the key with `box.before`/`after` and offers `sealText/openText/sealJson/openJson` (fresh random 24-byte nonce, base64 on the wire). Also keypair helpers and `keyFingerprint`. |
| `src/lib/relay/prng.ts` | done | `installNaclPrng()`: tweetnacl PRNG backed by `expo-crypto` `getRandomValues`, because Hermes has no WebCrypto. |
| `src/lib/relay/frames.ts` | done | Relay frame types from CONTRACTS: `msg`, `pair_request`, `pair_ok`, `register_push`, `push`, plus tolerant `error`/`presence`. Also `parseRelayFrame` and `relayConnectUrl()` (`https→wss`, `/v1/connect?role=..&hubId=..&deviceId=..`). |
| `src/lib/relay/pairing-link.ts` | done | Parses and validates `aipet://pair?relay=&hub=&pk=&code=` and also accepts expo-router params. It restores a `+` in the pk that a query parser turned into a space, checks that the pk is 32 bytes and the code is 8 alphanumerics, and flags non-https relays as insecure. |
| `src/lib/relay/pairing.ts` | done | `pairWithHub(offer)`, the device side of pairing. Steps: new keypair and deviceId, open a `role=pairing` WebSocket, send `pair_request`, wait for `pair_ok`, then open its box with the QR's hub pk. Opening the box authenticates the hub against the relay. It maps error codes to friendly text and has a 30 s timeout. |
| `src/lib/relay/relay-link.ts` | done | **The RelayTransport adapter.** `RelayLink.SocketImpl` is a WebSocket-shaped class passed to the unchanged `HubClient` from `@ai-pet/hub-client` as `WebSocketImpl`. Each protocol frame is sealed into `{t:"msg",from,to,nonce,box}`; incoming `msg` frames from the hub are opened and delivered as `onmessage({data})`. hub-client therefore keeps all of its own semantics: hello/Welcome, acks, reducers, queueing and reconnect. `registerPush(token)` is resent on every reconnect. A status store tracks `relayConnected`, `hubOnline`, `lastError` and `deviceRevoked`. |
| `src/lib/hubs/storage.ts` | done | expo-secure-store persistence with the `AFTER_FIRST_UNLOCK_THIS_DEVICE_ONLY` accessibility setting. Holds the hub records list, a per-hub device secret key under a separate key, and app settings (`activeHubId`, `notificationsEnabled`, `expoPushToken`). |
| `src/lib/hub/hub-provider.tsx` | done | `HubProvider` context: loads records, builds `RelayLink` + `HubClient` for the active hub, reconnects when the app returns to the foreground, and exposes `pair`/`unpair`/`setActiveHub`/`updateSettings`. Hooks: `useHub`, `useHubState` (its own `useSyncExternalStore`), `useRelayStatus`. |
| `src/lib/format.ts` | done | Display helpers: timeAgo, baseName, agentName, sessionTitle, knownCwds, and similar. |
| `src/lib/feed.ts` | done | Pure: `buildFeed(events)` merges message/thought chunks, folds `tool_call_update` into `tool_call`, marks permissions resolved, and collapses consecutive plans. Also `stopReasonLabel`. |
| `src/theme/index.ts` | done | Light and dark palettes (honey, cocoa, cream, collar red), activity and status labels and colours. |
| `src/components/pet/pet-sprite.tsx` | done | Plays the PNG strip with a stepped `Animated` translateX on the native driver (no re-render per frame). It respects reduced motion and falls back to SVG when an image errors or is missing. |
| `src/components/pet/pet-svg.tsx` | done | react-native-svg dog, cat, bird and robot with per-activity expressions and badges. |

**Not written yet:** everything below was planned and none of it exists.
- **Routes:** `src/app/**`, which would be root Stack + NativeTabs Home/Chat/Settings, `session/[id]`, `start`, `pair` (scanner + deep-link confirm) and `approvals`.
- **UI components:** buttons, cards, approval card, event feed view, composer.
- **Helpers:** a `usePetViews` hook (pet-engine `derivePetViews` + `nextPetViewChange` timer), a biometrics helper (expo-local-authentication) and a push helper (expo-notifications token, channel, and tap → `/approvals`).
- **Tests:** unit tests in `test/` for encoding, crypto, pairing-link and feed.

## Dependencies added (apps/mobile/package.json only; installed with `bash scripts/bun-locked.sh install`)

- **Runtime:** `@ai-pet/{protocol,pet-engine,hub-client}` (`workspace:*`), `expo`, `expo-router`, `expo-camera`, `expo-constants`, `expo-crypto`, `expo-device`, `expo-font`, `expo-haptics`, `expo-image`, `expo-linking`, `expo-local-authentication`, `expo-notifications`, `expo-secure-store`, `expo-splash-screen`, `expo-status-bar`, `expo-system-ui`, `react@19.3.0`, `react-native@0.88.1`, `react-native-gesture-handler`, `react-native-reanimated`, `react-native-safe-area-context`, `react-native-screens`, `react-native-svg`, `react-native-worklets`, `tweetnacl@^1.0.3`.
- **Dev:** `@types/react ~19.3.0`, `typescript ~6.0.3` (the SDK 58 template's version; the rest of the repo uses ^5.9).
- **Install:** the root `bun.lock` changed (501 packages, isolated linker). Expo has supported isolated installs since SDK 54.
- **React version:** mobile pins React **19.3.0**, which Expo 58 requires; the rest of the repo uses 19.3.1. The app must **not** import `@ai-pet/hub-client/react` or `@ai-pet/pet-engine/react`, because those resolve react 19.3.1 and would put two copies of React in the bundle. The core entries of both packages are React-free and are what the app uses.

## Parts that assume a single hub / single relay (what the redesign will touch)

1. **One active connection.** `HubProvider` keeps a *list* of `HubRecord`s and `settings.activeHubId`, but it builds exactly **one** `RelayLink` + `HubClient`, for the active hub only. Multi-machine use would need one client per hub, plus a merged view or a hub switcher in every screen.
2. **Relay URL per hub record.** `HubRecord.relay` comes from the QR. Each hub can name its own relay, so there is no global relay setting, but each record is tied to one relay.
3. **Pairing is 1 device ↔ 1 hub.** `pairWithHub` makes a fresh keypair and `deviceId` per hub, and the secret key is stored per hubId. If several machines should share one identity (for example a user/account key, or hubs vouching for each other), the key model in `storage.ts` and `pairing.ts` changes.
4. **Frame routing.** `RelayLink` accepts `msg` frames only when `from === hubId` and always sends `to: hubId`. Fan-out to several hubs on one relay socket would need routing by `from`/`to`, and a channel per hub pk.
5. **Push.** `register_push` is sent per hub link with that hub's `deviceId`. Tapping a push was going to open `/approvals` for the active hub only. Multi-hub would need the push data to carry a hubId.
6. **The hub record holds the hello token** (`PairWelcome.token`). That assumes per-hub hub-client auth over the relay.
7. **Relay connect URL and frame shapes** follow CONTRACTS.md only (`/v1/connect?role=device&hubId&deviceId`, with no device auth handshake). The changes are confined to `frames.ts` (`relayConnectUrl`), `relay-link.ts` (`open()`) and `pairing.ts`.

The parts that do not depend on the relay design survive as-is: the theme, pet sprites/SVG, `feed.ts`, `format.ts`, `encoding.ts`, `crypto.ts` (`BoxChannel`) and the copy-pets step.

## Commands run

| Command | Result |
|---|---|
| `bash scripts/bun-locked.sh install` | ok, 501 packages, about 67 s |
| `cd apps/mobile && bun run copy-pets` | ok, 37 files, generated sprite map |
| `python <scratch>/mkicons.py apps/mobile` | ok, icons written |
| `cd apps/mobile && bunx tsc --noEmit` | **exit 0** |
| `bunx expo-doctor` | **not run** (frozen before any routes existed) |

## Decisions for the orchestrator / user

1. **Single vs multi-hub client model.** Choose between one `HubClient` per paired hub with merged Home/approvals, or an active-hub switcher. The provider can do either.
2. **Device identity.** Choose between a per-hub keypair (current) and one device keypair shared across hubs, which is simpler for multi-machine and lets one `register_push` cover all hubs.
3. **Biometrics policy.** My plan, not yet written: Approve needs biometrics, with the OS passcode fallback allowed; Deny does not. If the phone has no biometrics but has a passcode, a passcode prompt is allowed. Please confirm.
4. **Push in Expo Go.** On Android, remote push requires a dev build, and `getExpoPushTokenAsync` needs an EAS `projectId`. `app.json` has none. This is needed before push can work, and I ran no EAS commands.
5. **Hello token over the relay.** I need the relay spec to say whether the hub expects a `hello.token` from relay devices (it would come from the `pair_ok` box) or authenticates them by device key alone.
