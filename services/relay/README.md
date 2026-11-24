# @pawl/relay

The Pawl relay: a Cloudflare Worker plus one Durable Object **mailbox per device**. It lets paired Pawl
devices (hubs today, phones in step 7) reach each other when they are not on the same LAN. Every device connects
**outbound** over WebSocket; nobody needs an open port. The relay routes end-to-end encrypted envelopes and never
holds a key that can open them.

It is a **closed relay**: you deploy it on your own Cloudflare account, and only devices that know its access key
can use it, so strangers cannot create mailboxes on your bill. The access key is the real control; the per-IP
connect limit and the other budgets below are best-effort backstops.

Wire types live in `@pawl/protocol` (section "relay (migration step 6)"); behaviour is specified in
`docs/CONTRACTS.md` "Step 6 — Relay".

## What the relay sees (and what it does not)

The relay **can see**:

- Each device's X25519 **public key**. A device's relay address is its public key (base64url, 43 chars), so a
  mailbox can only be claimed by whoever holds the matching secret key.
- Which public keys each device **allows** to write to it (its paired devices), and its live pairing slots
  (one-time tokens, at most 4, at most 1 hour).
- Envelope metadata: `from` / `to` device ids, `seq`, the nonce, sizes, and when frames are sent and delivered.
- Connection metadata Cloudflare sees for any Worker: client IP, time, TLS details.
- For phones only: an Expo push token, used to send a fixed, content-free wake ("Your pet needs you"). The phone
  removes it with `unregister_push` (push turned off, or unpaired).

The relay **cannot see**: anything inside an envelope. Bodies are NaCl boxes (X25519 + XSalsa20-Poly1305) from the
sender's long-term key to the receiver's; the relay never has either secret key. It does not parse `env` beyond its
size and JSON shape.

It **does not log** envelopes, keys, tokens, the access key or push tokens: the only log calls take an event name
and numbers (`src/log.ts`). `wrangler.jsonc` turns observability, invocation logs and Logpush off, because request
URLs carry the device public key and the access key.

## Protocol in one screen

```
device                                          relay
  GET /v1/connect?pk=<b64url pk>&k=<access key>  ──►  Worker: access key, per-IP budget, pk, Host; then idFromName(pk)
                                                 ◄──  {t:"challenge", nonce, relayPk, v:1}   fresh X25519 key per connection
  {t:"auth", box}                                ──►  box = nacl.box(utf8("pawl-relay-auth-v1:<nonce>:<host>"),
                                                                     nonce, relayPk, deviceSecretKey)   within 10 s
                                                 ◄──  {t:"ok", queued:n}  then n queued {t:"msg"} frames
  {t:"allow", pks:[...]}                         ──►  replace allowed senders (<= 64)
  {t:"allow_pairing", token, exp}                ──►  one-shot slot for an invite (<= 4 live, exp <= now + 1 h)
  {t:"send", to, env, pairing?}                  ──►  forwarded to mailbox `to`:
                                                        allowed sender or matching slot -> {t:"msg", from, env}
                                                        (live or queued), else {t:"error", code:"not_allowed", to}
  {t:"register_push", expoToken}                 ──►  phones: content-free wake when a frame is queued while offline
  {t:"unregister_push"}                          ──►  phones: forget the push token; no wake until register_push again
  {t:"ping", ts}                                 ──►  {t:"pong", ts: relay clock}
```

- **HTTP refusals** (all before any Durable Object is touched): 404 unknown path, 426 not a WebSocket upgrade,
  503 access key not configured on the relay, 429 per-IP connect budget spent (`Retry-After: 60`), 401 missing or
  wrong access key, 400 bad `pk` or Host. A mailbox whose 8 pending sockets are all still within their first 3 s
  answers 429 (`Retry-After: 3`).
- **Encodings:** relay addresses (`?pk=`, `allow.pks`, `send.to`, `msg.from`) are base64url unpadded, 43 chars,
  canonical only. The access key is base64url unpadded, 43 chars (32 bytes). `challenge.nonce`, `challenge.relayPk`
  and `auth.box` are standard padded base64. Pairing tokens are base64url unpadded, 22 chars (16 bytes).
- **Host binding:** `host` in the auth text is the connect request's `Host` header, i.e. the URL authority exactly
  as the device dialled it (`127.0.0.1:8787` under `wrangler dev`, `relay.example.com` for
  `wss://relay.example.com`). Use a lowercase host and do not spell out default ports. Binding the host stops a
  malicious relay from replaying another relay's challenge.
- **Nonce domain separation:** the device boxes its auth answer with its long-term key, the same key it uses for
  envelopes. Envelope nonces are `15 random bytes || dir (0 or 1) || seq (u64 BE)`, so their byte 15 is always 0 or 1;
  every challenge nonce has byte 15 in 2..=255 (`src/auth.ts` `challengeNonce`). An auth answer therefore never
  reuses an envelope nonce, even if a malicious relay sent a paired peer's public key as `relayPk` (that would make
  the device box under the device↔peer key; a repeated nonce there would leak plaintext and allow forgeries).
  **Devices must refuse a challenge whose nonce byte 15 is 0 or 1** (the reference `answerChallenge` does).
- Nothing but `auth` is accepted before authentication; anything else, a wrong box, a low-order key or the 10 s
  deadline gives `error{bad_auth}` and close 1008.
- One live socket per mailbox: a newly authenticated socket replaces the old one (close 4000).
- Frames from one sender to one recipient are delivered in the order they were sent (the receivers' replay
  protection drops seq regressions).
- **Pairing over the relay:** the inviter registers `allow_pairing{rt, exp}`; the joiner sends its `pair_request`
  with `pairing: rt`. The joiner must `allow` the inviter's pk **before** sending, or the `pair_accept` reply is
  `not_allowed`. After pairing both sides re-send `allow` with their roster. Invite URIs carry the relay URL
  (`relay=`), the pairing token (`rt=`) and the access key (`rk=`).
- **Errors:** `bad_auth`, `bad_frame`, `too_large`, `not_allowed` (with `to`), `queue_full` (with `to`; the
  recipient's queue overflowed and its oldest frames were dropped; your frame **was** queued), `rate_limited` (your
  socket, your mailbox's config budget, the recipient's inbound budget, or the recipient mailbox is unavailable).

## Limits

| What | Limit | Where |
|---|---|---|
| Connects per client IP | 30 per minute per Cloudflare location, else 429. Best-effort backstop: the Rate Limiting API is approximate and per location, so short bursts can get through (a live smoke saw 45 connects in ~15 s pass); the access key is what keeps strangers out | `wrangler.jsonc` `ratelimits` |
| Frame size (`send` and everything else) | 320 KiB (`RELAY_FRAME_MAX_BYTES`) | protocol |
| Allowed senders per mailbox | 64 (`RELAY_MAX_ALLOWED`) | protocol |
| Offline queue | 256 frames, 2 MiB, 24 h (`RELAY_QUEUE_*`); oldest dropped first, alarm prunes expired; frames from senders removed by `allow` are dropped | protocol |
| Pairing slots | 4 live, expiry clamped to now + 1 h, oldest registration evicted | `src/limits.ts` |
| Auth deadline | 10 s; auth frame <= 2 KiB | `src/limits.ts` |
| Unauthenticated sockets per mailbox | 8; to admit another, the oldest one older than 3 s is closed (younger ones may be mid-auth and are never evicted; if all are, 429) | `src/limits.ts`, `src/pending.ts` |
| Rate limit per socket | 50 frames/s, burst 200; one `rate_limited` notice per second; closed after 5000 drops in 10 s | `src/limits.ts` |
| Inbound per mailbox (all senders) | 100 frames/s, burst 400, checked before the allowlist | `src/limits.ts` |
| Config frames per mailbox (`allow`, `allow_pairing`, `register_push`, `unregister_push`) | 5/s, burst 10; unchanged values are not rewritten | `src/limits.ts` |
| In-flight forwards per mailbox | 256 frames / 8 MiB / 128 recipients, else `rate_limited` | `src/limits.ts` |
| Push wakes | at most 1 per minute per mailbox (also across `unregister_push` / `register_push`), only when a frame is queued for an offline device that has authenticated before, has paired senders and has a registered token | `src/push.ts`, `src/inbox.ts` |

## Layout

```
src/index.ts      Worker entry: wires env into gate.ts; the only path to a mailbox
src/gate.ts       request checks: /health, /v1/connect (access key, per-IP limit, pk, Host)
src/mailbox.ts    Durable Object Mailbox: Hibernation API sockets, auth, forwarding (DO RPC deliver()), alarm
src/inbox.ts      the mailbox's decisions: inbound budget, size, allowlist, pairing slots, queue, push, config budget
src/pending.ts    admission of unauthenticated sockets (which pending socket to evict, if any)
src/frames.ts     client frame validation, envelope shape check, server frame builders
src/auth.ts       challenge + verify (tweetnacl)
src/allow.ts      allowlist + pairing slots
src/queue.ts      offline queue (caps + TTL + per-sender retain)
src/ratelimit.ts  token bucket + notice throttle + abuse cut-off
src/push.ts       Expo push payload, token check, throttle
src/storage.ts    KvStore interface (= ctx.storage.kv) + MemoryKv for tests
scripts/smoke.ts  end-to-end check against a running relay (several devices with real keys)
```

## The access key

Every device needs the relay's access key: base64url (no padding) of 32 random bytes, 43 characters. Create one:

```sh
bun -e 'console.log(Buffer.from(crypto.getRandomValues(new Uint8Array(32))).toString("base64url"))'
# or: openssl rand -base64 32 | tr '+/' '-_' | tr -d '='
```

Store it as a Worker secret (it never goes into `wrangler.jsonc` or git):

```sh
bunx wrangler secret put RELAY_ACCESS_KEY      # paste the key when asked
```

Devices get the key from you once: enter it in the hub's relay settings (stored write-only as `relay.accessKey`),
and invite URIs created while the relay is on carry it (`rk=`) so a newly paired device can use the relay too.
To rotate it, put a new secret and update every hub; devices with the old key get 401 until then.

Without the secret the relay refuses every connect with 503. Setting the var `RELAY_OPEN = "1"` (in `wrangler.jsonc`
`vars` or the dashboard) serves without a key. **Not recommended**: anyone could then create mailboxes and traffic
on your Cloudflare bill. A key that is set is always required, even with `RELAY_OPEN`.

## Run it locally

From the repo root, install once with the repo's locked installer: `bash scripts/bun-locked.sh install`.

```sh
cd services/relay
# once: a local access key for wrangler dev (.dev.vars is git-ignored; never commit it)
bun -e 'await Bun.write(".dev.vars", "RELAY_ACCESS_KEY=" + Buffer.from(crypto.getRandomValues(new Uint8Array(32))).toString("base64url") + "\n")'
bunx wrangler dev                 # http://127.0.0.1:8787, local Durable Objects, no Cloudflare login needed
bun run smoke                     # in another terminal; reads the key from .dev.vars (or RELAY_ACCESS_KEY)
bun test                          # unit tests (pure modules, in-memory storage)
bun run typecheck                 # worker (workers-types) + tests/scripts (bun types)
```

`RELAY_URL=https://... RELAY_ACCESS_KEY=... bun run smoke` runs the same checks against a deployed relay
(`SMOKE_FAST=1` skips the 10 s auth-timeout check). The smoke spends the per-IP connect budget on purpose: its last
check expects a 429, which is a failure locally but only a WARN against a deployed relay (the edge limit is
approximate). A second run within the same minute waits for the next window. Only a local relay gets a per-run
`CF-Connecting-IP` from the smoke; Cloudflare's edge refuses requests that set it (403).

Local state lives in `services/relay/.wrangler/` (git-ignored). Under `wrangler dev`, a close issued from the alarm
handler (auth timeout) reaches the client only when the idle Durable Object is evicted, about 10 s later; the
`bad_auth` frame itself is immediate.

## Deploy your own

You need a Cloudflare account. SQLite-backed Durable Objects and the Rate Limiting binding are available on the
Workers Free and Paid plans; check the current limits and pricing before relying on it.

1. Authenticate wrangler, either interactively with `bunx wrangler login`, or for CI / headless use set
   `CLOUDFLARE_API_TOKEN` (an API token from the "Edit Cloudflare Workers" template) and `CLOUDFLARE_ACCOUNT_ID`
   in the environment.
2. Optionally change `name` in `wrangler.jsonc` (the workers.dev URL is `https://<name>.<subdomain>.workers.dev`),
   or add a custom domain route. The `ratelimits` `namespace_id` (`"47800"`) must be unique among the rate limiters
   in your account; change it if another Worker already uses that number.
3. `cd services/relay && bunx wrangler deploy`. The first deploy applies the `v1` migration that creates the
   `Mailbox` class with SQLite storage.
4. `bunx wrangler secret put RELAY_ACCESS_KEY` (see "The access key"). Until it is set the relay answers 503.
5. Optional: `bunx wrangler secret put EXPO_ACCESS_TOKEN` if your Expo project requires access tokens for push.
6. Check: `curl https://<your relay>/health` returns `{"ok":true}`, then
   `RELAY_URL=https://<your relay> RELAY_ACCESS_KEY=<key> bun run smoke`. Point the hubs at `https://<your relay>`
   with the key.

Keep the access key secret: it is what stops strangers from using your relay. The built-in per-IP limit is a
best-effort backstop (approximate, counted per Cloudflare location) and will not stop a determined flood by
itself. For a relay under attack, rotate the key if it leaked, and add a Cloudflare WAF rate-limiting rule on
`/v1/connect` for an edge-level cap in front of the Worker.
