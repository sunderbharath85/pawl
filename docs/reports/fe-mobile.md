# fe-mobile — `apps/mobile` (@pawl/mobile), step 7 (phone)

Branch `step-7-phone`. Nothing committed (the orchestrator commits). No dependencies added, removed or upgraded.

## What was built

1. **Platform-free peer core** `apps/mobile/src/lib/peer/` (no React Native / Expo imports; runs under plain bun):
   - `b64.ts`: strict canonical base64 / base64url (port of `crates/peer/src/b64.rs`), strict UTF-8, hex, bytewise compare.
   - `keys.ts`: `DeviceKeys` (X25519 via tweetnacl, secret in a private field, `isLowOrder` = the Rust LOW_ORDER table + all-zero
     shared-secret check, precomputed box keys). `isLowOrderKey`.
   - `envelope.ts`: `seal` / `sealPlaintext` / `open`. Nonce = 15 random || dir || seq u64 BE; `open` checks version, encoding,
     direction and seq binding, then decrypts; errors are the Rust `OpenError` codes (`bad_version`, `bad_encoding`, `bad_nonce`,
     `decrypt_failed`, `bad_body`).
   - `invite.ts`: `parseInvite` (exactly `Invite::parse`, incl. relay-only invites, `rk`, strict percent-decoding, low-order pk
     refused), `inviteToUri`, `extractInviteUri`, `inviteSummary`, `keyFingerprint`.
   - `net.ts`: `validAddr` / `validHost` / `peerAddrOk`, `normalizeRelayUrl`, `validAccessKey`, relay address helpers,
     `relayHostHeader`, `relayConnectUrl`.
   - `relay-client.ts`: `RelayClient` (connect `{url}/v1/connect?pk=&k=`, challenge -> auth -> ok, refuses byte-15 < 2 and its own or
     any roster / pending-join key as `relayPk`, `allow` on every session and on change, `register_push` / `unregister_push`, ping 15 s, dead socket
     after 45 s of silence, reconnect 1 s -> 60 s with jitter, close 4000 `replaced` waits the max). `authAnswer`.
   - `mirror.ts`: per-hub mirror on top of `@pawl/hub-client` (`reduceServerMsg`, `mergeEvents`, `initialHubState` are pure and
     RN-safe). Every item must carry the sender's machine id / id prefix (events `id == ${machineId}:${seq}`); snapshot replaces
     sessions / pets / pending; event cursor `maxSeq`; `Resolution` records who decided (`byMachineId`).
   - `roster.ts`: `evaluateIntro` (≤ 32 devices, kind hub only, id / name (≤ 64 chars, no control chars) / key (canonical, not low
     order, not ours, not the sender's) / addr (`peerAddrOk`) checks; existing id with another key or key under another id =
     conflict, never replaced; revoked ids / keys never re-added). `HELLO_RETRY_MS` 1 s, 2 s, 5 s, then 30 s.
   - `intents.ts`: `IntentError`, `approveOption` (allow_once only), `denyOption` (reject_once else null), `phoneIntentErrorText`.
   - `gate.ts`: the biometric gate, pure + injectable: `gateAvailability` (biometrics, else device passcode, else disabled with an
     explanation), `ApprovalGate.confirm` (only `success === true` confirms; one prompt at a time).
   - `store.ts`: persistence split over small secure-store keys (see "Storage"), coalesced write-behind, `wipe`.
   - `node.ts`: **`PhoneNode`** (facade below). `index.ts` re-exports everything.
2. **Expo adapters** `src/lib/expo/`: `secureStore` (expo-secure-store, `AFTER_FIRST_UNLOCK_THIS_DEVICE_ONLY` = never synced or
   restored elsewhere), `expoRandomBytes` (expo-crypto `getRandomValues`, not `getRandomBytes`, which may use Math.random in dev)
   + `installNaclPrng`, `phoneOs` (ios / android), `defaultDeviceName`, `expoAuthenticator` (expo-local-authentication),
   `push.ts` (EAS project id from `extra.eas.projectId`, permission, Expo push token, Android `default` channel, Expo
   unregister on "push off").
3. **Provider** `src/lib/phone/`: `PhoneProvider` (opens the node, AppState foreground -> reconnect + re-hello, background ->
   flush, push token -> `register_push` once paired, tapping a push opens Approvals), `usePhone`, `useActions` (approve / start go
   through the gate first), `usePetViews` (pet-engine `derivePetViews` + its decay timer), `decisionText`.
4. **Screens** (expo-router, `app/`): Pets home (dog hero + "the pack"; welcome + pairing steps when unpaired), Approvals
   (biometric Approve / Deny, allow-always-only note, offline note, "Recently decided" with "Approved from <device>"), Computers
   (online, via relay, refused-by-relay hint, introduced-by), Computer detail (sessions open / finished, pets, start here), Session
   (status, read-only transcript via `buildFeed`, pending approval, prompt box for owned sessions, Stop), Start (computer -> repo
   -> agent -> optional first message -> biometric confirm), Pair (QR scanner, paste, confirm with key fingerprint / relay /
   expiry), Settings (phone name, relay status, paired computers + remove, push switch, approvals-lock status, roster notices,
   Unpair this phone). Native tabs (Approvals badge = pending count). Light + dark from the existing palette.
   Deep link: `app/+native-intent.tsx` rewrites `pawl://peer?…` to `/pair?uri=…` (confirm step still required).

## PhoneNode facade (headless use)

```ts
import { PhoneNode, MemoryStore } from "./apps/mobile/src/lib/peer";
const phone = await PhoneNode.open({ store /* KeyValueStore, default memory */, name: "Smoke phone", os: "android" });
// optional: socketFactory (default global WebSocket), clock, random (default crypto.getRandomValues), logger, timings
phone.start();                                  // connects the stored relay (if paired) and hellos every hub
const hub = await phone.pair(inviteUri);        // phone invite (relay-only). Throws PairError{code: bad_uri|no_relay|
                                                //   other_relay|self_invite|busy|unreachable(detail)|timeout|invite_rejected}
phone.getSnapshot();      // { deviceId, name, os, publicKey, paired, joining, relay, hubs: HubView[], machines, sessions,
                          //   pets, pending, events, agents, repos, resolutions, notices, pushEnabled, pushToken, version }
phone.subscribe(fn);      // coalesced after every change;  phone.onEvent(fn): paired | link | hub_added | hub_removed |
                          //   permission_request | permission_resolved | relay | notice
await phone.waitFor((s) => s.hubs.every((h) => h.online && h.sync === "synced"), 20_000);
phone.hubs(); phone.sessions(machineId?); phone.pendingPermissions(); phone.events(sessionId); phone.resolution(requestId);
await phone.approve(requestId, { confirmed: true });   // allow_once; confirmed must come from the gate on a real phone
await phone.deny(requestId);                           // reject_once, else null
await phone.prompt(sessionId, text); await phone.cancel(sessionId);
await phone.startSession({ machineId, agentId, cwd, prompt?, confirmed: true });
await phone.sendIntent(machineId, intent, { confirmed });  // raw; resolves {intentId, result?} on applied
// IntentError{code: already_decided|not_found|not_owner|forbidden|unconfirmed|invalid|rate_limited|offline|timeout|
//   no_allow_once|closed}
phone.setName(name); phone.setPushToken(token|null);
phone.setPushEnabled(bool);   // on: register_push; off: unregister_push now, or on the next relay session (flag persisted)
await phone.removeHub(id);    // bye{revoked} to it, forget it, never re-added by an intro
await phone.unpairPhone();    // unregister_push, then bye{revoked} everywhere, wipe, fresh key + device id
phone.onForeground(); await phone.onBackground(); await phone.flush(); await phone.stop();
```

CLI wrapper (JSON-file store, no secrets printed): `cd apps/mobile && bun scripts/headless-phone.ts --state <file.json>
[--name N] [--os ios|android] [--wait secs] pair "<uri>" | status | watch [secs] | approve <requestId> | deny <requestId> |
prompt <sessionId> <text> | cancel <sessionId> | start <machineId> <agentId> <cwd> [prompt] | unpair`.
`approve` / `start` there send `confirmed: true` without biometrics (headless smoke only; the app always uses the gate).

Behaviour notes: hello on every relay session (hubs never hello phones); the hub's answer (presence) or any fresh authenticated
frame brings the link up; on link up the phone sends presence + `sync_request{sinceSeq: maxSeq}`; presence every 15 s; no
authenticated frame for 45 s -> link down; `not_allowed` for a hub -> retry hello 1 s, 2 s, 5 s, then 30 s; no answer -> retry
after 10 s + that backoff; hello / presence stamped > 1 h from local time dropped; `msg.from` must be the hub's roster key; seq
`max(last_tx + 1, now_ms * 1000)`, receiver accepts only `seq > last_rx` (both persisted per hub). During a join on an adopted
relay the allow list holds only the inviter and no hellos go out; a failed join turns an adopted relay off again.
`sync_request` and `intent` from a hub are ignored (a phone owns nothing).

## Storage (expo-secure-store keys)

`pawl.sk` (device secret, 64 hex), `pawl.identity` {deviceId, name, os}, `pawl.relay` {url, accessKey}, `pawl.hubs`
[ids], `pawl.hub.<id>` (name, os, publicKey, pairedAt, introducedBy, addr), `pawl.seq.<id>` {rx, tx}, `pawl.revoked`
{ids, pks} (≤ 64), `pawl.settings` {pushEnabled, pushToken, pushClear} (`pushClear` = the relay still has to be told
`unregister_push`). The mirror is memory only (resynced on every hello).

## Files

Added: `src/lib/peer/{b64,keys,envelope,invite,net,relay-client,mirror,roster,intents,gate,store,node,types,index}.ts`,
`src/lib/expo/{adapters,push}.ts`, `src/lib/phone/{phone-provider.tsx,actions.ts,pets.ts}`, `src/components/ui/index.tsx`,
`src/components/session-row.tsx`, `app/_layout.tsx`, `app/+native-intent.tsx`, `app/pair.tsx`, `app/start.tsx`,
`app/session/[id].tsx`, `app/(tabs)/_layout.tsx`, `app/(tabs)/{(home),approvals,machines,settings}/{_layout,index}.tsx`,
`app/(tabs)/machines/[id].tsx`, `scripts/headless-phone.ts`, `test/{helpers,crypto,invite,logic,node,vectors}.test.ts`
(helpers.ts: manual clock, in-memory relay speaking the deployed relay's frames, fake hub).
Changed: `src/lib/feed.ts` (TruncatedPayload -> "Large output" line). `app.json` unchanged in the end (the temporary
expo-router `root` pin was removed together with the empty `src/app/`).
Removed (approved by t13): `src/lib/relay/{crypto,encoding,frames,pairing-link,pairing,prng,relay-link}.ts`,
`src/lib/hub/hub-provider.tsx`, `src/lib/hubs/storage.ts` (the old relay design; nothing imports them), and the empty
pre-existing `src/app/` directory (it made Expo Router use `src/app` as its routes root).

## Commands + results (in `apps/mobile`)

- `bunx tsc --noEmit` — pass (after fe-settings' hub-client v8 fix).
- `bun test` — **79 pass, 0 fail** (5 files; see "Security review fixes" for the newest): vectors (keys, base64 samples + invalid, 12 envelopes sealed byte-identically from
  the given plaintext / nonce and opened to the given body, 7 rejects with the same OpenError codes, replay, 5 valid + 14 invalid
  invites, relay auth 2 valid boxes + connect URLs, 3 refused), envelope round trip / tweetnacl interop / seq-bump / tamper /
  reflected-direction, invite parsing ported from Rust, relay URL / addr rules, relay client (auth, allow + register_push per
  session, restart reconnect, byte-15 0/1, own key, roster key, bad access key, dead socket), mirror reducers + validation,
  roster_intro rules, intent helpers, biometric gate (mocked), PhoneNode end to end against the fake relay + hubs (pairing,
  used / wrong code, timeout, bad key, LAN-only refused, restart from storage, offline after 45 s + recovery, presence cadence,
  replay and forged-sender drops, approve / deny / prompt / cancel / start / timeout / offline / already_decided /
  no_allow_once, intro + not_allowed retry schedule, intro conflicts / phones / revoked / unknown sender, bye{revoked},
  unpair, push: register after pairing, off -> one `unregister_push`, on again -> register, off while the relay is down ->
  cleared on the next session after a restart, `unregister_push` before the byes on unpair).
- `bunx expo-doctor` — 19/20; the failing check is "duplicate native modules" for expo-constants / expo-font / expo-linking /
  expo-status-bar, all the **same version** twice (bun's isolated linker keeps per-peer copies). Not a version conflict; a root
  install-layout matter, not apps/mobile.
- `bunx expo export --platform android` — **success** (1796 modules, routes from `app/` with the default root, pet sprites),
  re-run after deleting `src/app/`. `--platform ios` also bundles. Output went to the session scratchpad, nothing written in
  the repo. No EAS builds.

## Security review fixes (t13 review)

1. **Peer names** (`roster.ts` `deviceNameOk` / `cleanDeviceName`, exactly Rust `device_name_ok`, tested against
   `crates/peer/tests/vectors/names.json`: 11 valid, 35 invalid, 21 intro devices): reject any Unicode Cc character and any
   of U+061C, U+200B-200F, U+2028-202E, U+2060-2064, U+2066-2069, U+FEFF anywhere in the untrimmed name, then trim
   Unicode White_Space (Rust `str::trim` set), then 1..=64 code points. Built from code-point tables (no regex escapes).
   Applied to: `roster_intro` devices; `hello` (a hello naming a refused name is dropped, it never brings a link up);
   `presence` (ignored whole); `pair_accept` (refused -> `invite_rejected`, like Rust `PairReply::Reject`); the phone's own
   name (`setName`, 40 chars) and the OS-derived default name (`stripNameChars`). Skipped intro devices never echo a refused
   name into a notice.
2. **Intro `bad_kind`**: `checkIntroDevice` (exported, Rust `check_intro_device` order bad_id, bad_name, bad_kind,
   bad_key, bad_addr): kind phone iff os is ios / android (a hub claiming ios / android, a phone without one, an unknown
   kind or os -> `bad_kind`); any addr on a phone -> `bad_addr`. Then the phone's own rules: structurally valid phones are
   not introduced to phones (`not_a_hub`), repeats in one intro (`duplicate`), self, sender, revoked, conflicts.
3. **Biometric gate**: a `confirm` while another prompt is open now returns `{confirmed: false, code: "busy"}` instead of
   sharing the open prompt's result, so one success can never confirm two actions.
4. **Mirror caps per hub**: sessions 500 (newest kept), distinct sessions with events 500 (oldest last event out), pending
   500 (oldest createdAt out), pets 200 (earliest received out), presence pets 200, agents 64, repos 200, events per
   session 500, resolutions 100, notifications 100. Applied on every snapshot, batch and live update (`capState`).
5. **rx seq persisted before acting**: accepted frames go to a write-ahead queue; the node flushes the store (one group
   commit per burst, which includes every queued `last_rx`) and only then acts on the bodies, in arrival order. A frame
   arriving during a flush waits for the next commit. A failed write stays queued and is retried on the next flush. The
   tx side needs no write-ahead: `seq = max(last_tx + 1, now_ms * 1000)`, so after a crash that lost `last_tx` the next
   seq is still above every seq sent before (as long as the clock does not go back; < 1000 frames per ms).
   Tests: a store that holds writes back shows the body is not applied until its `last_rx` is stored.

## Known gaps

- Not run on a device or simulator (E2E out of scope): screens typecheck and bundle; behaviour is covered by the bun tests of the
  core only.
- **Push needs an EAS project id** (decision: ship documented): read from `extra.eas.projectId` (or `Constants.easConfig`) when
  present. Without it the Settings switch stays off and disabled with that explanation, and no permission prompt or token request
  happens. Android release builds also need FCM credentials (google-services.json).
- `unregister_push` needs the redeployed relay (sde-relay). Against the old relay it gets `error{bad_frame}` and the relay keeps
  the token; the phone clears its pending flag once the frame is sent, so it does not retry (Expo unregister still stops
  the wakes).
- The mirror is not persisted: after a cold start with every hub offline the lists are empty until a hub answers.
- No phone-side "accept roster intros" toggle: the phone always applies the intro rules (hubs have the setting).
- iOS 16+ returns a generic `Device.deviceName` without the entitlement; the user can rename the phone in Settings.
- `test/` stays outside `tsconfig` (pre-existing exclude); bun type-strips it when running.

## Decisions (resolved by t13)

1. EAS project id: ship with push documented as "needs an EAS project id" (done as above).
2. Relay `unregister_push`: sent when push is turned off (now or on the next session) and before unpairing (done, tested).
3. Empty `apps/mobile/src/app/` deleted; app.json `root` pin dropped; export re-verified.
