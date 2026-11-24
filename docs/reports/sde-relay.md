# sde-relay report — migration step 6, relay service (phase A)

Branch `step-5-remote-commands` (nothing committed). Scope: `services/relay/` (`@krypto/relay`), per CONTRACTS.md
"Step 6 — Relay" → "Relay service (sde-relay)" incl. "Access key + abuse limits". **Not deployed.**

Two rounds: the initial build (decisions 1–6 accepted by t13), then the security-review fixes A + 1–8 (below).

## Files

All under `services/relay/` (plus `bun.lock`, changed by the locked installer when the approved deps were added in
round 1; no dependency changes in round 2):

| File | What |
|---|---|
| `package.json` | `@krypto/relay`; deps `@krypto/protocol` (workspace:*), `tweetnacl` ^1.0.3; dev `wrangler` ^4.167.0, `@cloudflare/workers-types` ^5.20261124.1, `typescript`; scripts typecheck (both configs) / test / dev / smoke |
| `wrangler.jsonc` | Worker `krypto-relay`, compat date 2026-11-20, DO `MAILBOX` → `Mailbox` (`v1` `new_sqlite_classes`), `ratelimits` `CONNECT_LIMIT` 30/60 s, observability + logs + invocation logs + logpush **off**, dev 127.0.0.1:8787. No secrets |
| `.gitignore` | `.dev.vars`, `.dev.vars.*`, `.wrangler/` |
| `.dev.vars` | **local only, git-ignored, not for commit**: a fresh random `RELAY_ACCESS_KEY` for `wrangler dev` |
| `tsconfig.json` / `tsconfig.bun.json` | worker (`src/`, workers-types, ES2022) / tests + scripts (bun types, ES2022) |
| `src/index.ts` | Worker entry: wires env into `gate.ts`; `forward` (the only `idFromName`) builds a fresh internal request |
| `src/gate.ts` | **new**: /health, /v1/connect checks: 426 → 503 (no key, not open) → per-IP 429 → 401 key → 400 pk/Host |
| `src/mailbox.ts` | DO: Hibernation API sockets, auth, ordered DO-RPC forwarding, alarm; decisions delegated to `Inbox` |
| `src/inbox.ts` | **new**: inbound budget, size, allowlist, pairing slots, queue, push gating, config budget |
| `src/pending.ts` | **new**: admission of unauthenticated sockets (grace-period eviction) |
| `src/frames.ts`, `src/auth.ts` | frame validation + envelope shape; challenge + verify (unchanged in round 2) |
| `src/allow.ts`, `src/queue.ts`, `src/push.ts` | + "changed" results (skip unchanged writes), `hasAllowed`, queue sender metadata + `retain` |
| `src/ratelimit.ts`, `src/storage.ts`, `src/b64.ts`, `src/limits.ts`, `src/log.ts` | token bucket; `KvStore` + `MemoryKv` (now counts writes); strict base64; bounds; counts-only logger |
| `test/*.test.ts` | 78 unit tests in 9 files |
| `scripts/smoke.ts` | end-to-end smoke (access key from env or `.dev.vars`, never printed) |
| `README.md` | what the relay sees, protocol, limits, access key, run locally, deploy your own |

## Gate (final run, after all changes) — in `services/relay/`

```
$ bunx tsc --noEmit                         -> exit 0
$ bunx tsc --noEmit -p tsconfig.bun.json    -> exit 0
$ bun test                                  -> 78 pass, 0 fail, 334 expect() calls, 9 files
$ bunx wrangler dev --ip 127.0.0.1 --port 8787   (local; RELAY_ACCESS_KEY from .dev.vars; no login)
$ bun run smoke                             -> 22/22 checks passed, exit 0  (also 22/22 on two earlier back-to-back runs)
```

Smoke checks: `/health`; 404 / 426; **missing or wrong access key → 401**; bad pk → 400 (now probed by status code);
bad auth ×3 (other key, wrong host, non-auth first) → `bad_auth` + 1008; auth ok; ping/pong; `not_allowed{to}`;
allow + send/deliver (envelope byte-identical, openable); 20 frames in order; `bad_frame` ×6; `too_large`; queue
while offline → `ok{queued:3}` + in-order flush; allowlist persists; pairing slot one-shot; replace (4000);
**pending flood: 8 fresh squatters → 9th connect gets 429 and none is evicted; after 3.5 s exactly one stale squatter
is evicted for the real device's login**; socket rate limit; auth timeout (10 s); **per-IP connect limit → 429**.
The smoke spends the IP budget on purpose and waits for a fresh window if an earlier run spent it. Only to a local
relay does it send a per-run `CF-Connecting-IP` (t13's patch: Cloudflare's edge answers 403 when a client sets it).

**Round 3 (after t13's live smoke, 21/22):** against a deployed relay the per-IP limit did not trip after 45 connects
in ~15 s (the Rate Limiting API is approximate and per location). That check is now a **WARN** (not a failure) when
`RELAY_URL` is not local, and still a hard failure locally; the summary line counts warnings. README now says the
per-IP limit is a best-effort backstop and the access key is the real control. Re-run after round 3:
`bunx tsc --noEmit` exit 0, `bunx tsc --noEmit -p tsconfig.bun.json` exit 0, `bun test` 78/78, local smoke 22/22.

**Round 4 (high, from the relay-transport review): challenge / envelope nonce domain separation.** The device boxes
its auth answer with its long-term key, the same key it uses for envelopes (nonce = 15 random || dir 0/1 || seq u64
BE). A malicious relay could send a paired peer's pk as `relayPk` plus an envelope-shaped nonce and get the device to
box under the device↔peer key with a reused nonce. Fix: `src/auth.ts` `challengeNonce()` forces byte 15 into
2..=255 (`2 + random % 254`); `createChallenge` uses it; `verifyAuth` refuses a pending nonce outside that domain;
the reference device helper `answerChallenge` throws on byte 15 < 2 (sde-peer / mobile must do the same). Tests
(`auth.test.ts`, 5 new): byte 15 ≥ 2 over 2000 challenges (and spread over > 200 values), the mapping for all 256
input values keeps the other 23 bytes, envelope nonces are never in the challenge domain, devices refuse dir-0/1
challenges, the relay never verifies an envelope-domain nonce. Smoke: envelopes now use the real nonce layout, and a
new check asserts every challenge nonce seen in the run is in the challenge domain. README: "Nonce domain separation"
bullet. Re-run: `bunx tsc --noEmit` exit 0, `bunx tsc --noEmit -p tsconfig.bun.json` exit 0, `bun test` 83/83,
local smoke 23/23 (19 challenges checked). Needs a redeploy.

## Security fixes (round 2) — each with its test

| # | Fix | Code | Test |
|---|---|---|---|
| A | Closed relay: `k=` must equal secret `RELAY_ACCESS_KEY` (base64url 32 B, constant-time compare), checked before `idFromName` (`forward` is the only path to a DO) → 401. Secret unset → 503 unless `RELAY_OPEN = "1"`; a set key is required even when open; malformed secret → 503 (fail closed) | `gate.ts`, `index.ts`, `mailbox.ts` `Env` | `gate.test.ts` "(A) access key" (5 cases + helpers); smoke "access key → 401" |
| 1 | Per-IP connect limit: Workers Rate Limiting binding `CONNECT_LIMIT` (30 / 60 s, key `connect:<CF-Connecting-IP>`), before the key check and any DO → 429 `Retry-After: 60`; limiter error → 503 | `gate.ts`, `wrangler.jsonc` | `gate.test.ts` "(1)" (429 before key/pk/forward, IP keying, limiter failure); `config.test.ts`; smoke "per-IP connect limit → 429" |
| 2 | Per-recipient inbound token bucket (100/s, burst 400) inside `deliver`, before the allowlist read and before any pairing slot → `rate_limited` to the sender; allow set stays in memory | `inbox.ts` `deliver` | `inbox.test.ts` "(2)" (counts allowlist reads, slot untouched, refill) |
| 3 | Pending eviction: when 8 are pending, close the oldest one that has had its challenge ≥ 3 s (`AUTH_GRACE_MS`; it has not answered); never evict a younger one (may be mid-auth) for a fresh connection — if all are young, the newcomer gets 429 | `pending.ts`, `mailbox.ts` `fetch` | `pending.test.ts` (5 cases); smoke "pending flood" |
| 4 | Unchanged `allow` (same set), `register_push` (same token), `allow_pairing` (same live token, exp within 1 s) are not written; config frames budget 5/s, burst 10 per mailbox → `rate_limited` | `allow.ts`, `push.ts`, `inbox.ts` `takeConfig`, `mailbox.ts` | `allow.test.ts` (write counter), `inbox.test.ts` "(4)" |
| 5 | Push wake only if the owner has authenticated at least once (`seen`, written once) and the allowlist is non-empty | `inbox.ts` `mayPush`, `onAuthenticated` | `inbox.test.ts` "(5)" ×2 |
| 6 | An `allow` change drops queued frames whose sender is no longer allowed (frames admitted by a pairing slot are kept; their sender is in no allowlist yet) | `queue.ts` `retain`, `inbox.ts` `setAllowed` | `queue.test.ts` "retain", `inbox.test.ts` "(6)" |
| 7 | `deliver` order: inbound budget → shapes → size (`too_large`) → allowlist → pairing slot; an oversized or malformed frame never consumes a slot | `inbox.ts` `deliver` | `inbox.test.ts` "(7)" |
| 8 | `observability.enabled=false` (+ `logs.enabled=false`, `invocation_logs=false`), `logpush=false` | `wrangler.jsonc` | `config.test.ts` "(8)" |

Notes on the fixes:
- **(3) "has not sent auth":** auth verification is synchronous, so a socket that sent `auth` is never still pending
  when we look; what we cannot see is an auth frame in flight. The 3 s grace period is the observable proxy (a real
  device answers within one round trip). With the access key and the per-IP limit, holding 8 fresh pending sockets on
  one mailbox needs > 160 connects/min, i.e. many IPs and the key.
- **(1) IP source:** Cloudflare docs: for direct requests `cf-connecting-ip` is the client IP (a client that sets it
  gets 403 at the edge, per t13's live test); requests from other Workers in another zone arrive as one fixed
  Cloudflare address, so they share one bucket. The limit is approximate and per location: a best-effort backstop,
  the access key is the real control (README).
- **(2)** sheds by recipient regardless of sender, as asked; a legit sender to a flooded mailbox also gets
  `rate_limited` until the flood stops. The flood itself now needs the access key.
- **(A)** the key is in the URL (`k=`): fine over WSS, and why every form of request logging is off (fix 8).

## Decisions (round 1, accepted by t13)

1. `/v1/connect` without upgrade → 426, bad pk/Host → 400 (now also 401 / 429 / 503 above).
2. `allow_pairing` exp > now + 1 h clamped; 5th slot evicts the oldest registration.
3. Encodings, host binding, queue-overflow semantics, close codes (CONTRACTS.md "Encodings").
4. Recipient unavailable → `rate_limited`; `queue_full` = older frames dropped, yours queued.
5. Envelopes rebuilt with PeerEnvelope fields only; `seq` must be a JS safe integer.
6. Serial forwarding chain per sender→recipient for ordering.

## Notes for sde-peer / sde-hubcore — please forward

- Connect URL is now `{url}/v1/connect?pk=<pk>&k=<access key>`; 401 = wrong/missing key (don't retry fast), 429 =
  per-IP budget or mailbox busy (honour `Retry-After`), 503 = relay has no key configured.
- The joiner must `allow` the inviter's pk before sending the `pair_request` (else the `pair_accept` is refused).
- Auth host = the `Host` header the client sends (URL authority as dialled, lowercase, no default port).
- `allow` / `allow_pairing` / `register_push` share a 5/s (burst 10) budget per mailbox: on reconnect send one
  `allow` plus at most the live invites (≤ 4 slots), not a burst per roster change.
- After `error{bad_auth}` close the socket yourself (local alarm-initiated closes arrive ~10 s late).
- `msg` frames are at most `RELAY_FRAME_MAX_BYTES + 64` bytes.

## Known gaps

- Not deployed (by instruction). t13: `bunx wrangler secret put RELAY_ACCESS_KEY` is required after the first deploy
  (until then 503). The `ratelimits` `namespace_id` `"47800"` must be unique in the account.
- `.dev.vars` (local key) exists in `services/relay/`, git-ignored; do not commit it.
- The DO glue (`mailbox.ts`) has no unit tests (needs the Workers runtime); its decisions moved to `inbox.ts` /
  `pending.ts` / `gate.ts`, which are unit tested, and the glue is covered by the smoke.
- Push wake not exercised end to end (needs a real Expo token and app).
- Round-1 anomaly (first-ever cold run: >5 s stalls, 4 checks failed) has not reproduced since in 9 runs. Running the
  smoke once against the deployed relay settles it: `RELAY_URL=https://... RELAY_ACCESS_KEY=... bun run smoke`.
