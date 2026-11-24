# sde-peer — `krypto-peer`, step 6 phase A (relay transport)

Status: **done**, including t13's follow-ups (the revoke gap and the transport security review; see the end).
Branch `step-5b-settings-ui`, nothing committed.

- `cargo test -p krypto-peer`: 93 passed, 1 ignored (the real-relay smoke). Run 5× in a row.
- Stress: 12 copies of the test binary in parallel. After the last race fix, 5 rounds were 60/60; after the final
  queue-size change, 1 more round was 12/12.
- `cargo clippy -p krypto-peer --all-targets -- -D warnings`: clean.
- Smoke against the real relay (`wrangler dev` in services/relay, with the dev access key): passed.
- `cargo test -p krypto-hub`: 158 passed with my hub patch spots.

Contract: CONTRACTS.md "Step 6 — Relay", plus t13's follow-ups:
- the access key;
- `stop_listening` semantics;
- protocol `LinkPath` / `RelayStatusInfo`;
- sde-relay's notes.

## API (all additive except `Invite.addr` and the new `PeerEvent` variants; hub patched, see below)

- **`PeerNode::set_relay(&self, relay: Option<RelayConfig>) -> Result<(), RelayConfigError>`**
  - Config: `RelayConfig { url, access_key: Option<String> }`. Its Debug redacts the key, and the key is never logged.
  - URL rules: `wss://`/`https://` for any host; `ws://`/`http://` only for loopback (tests, `wrangler dev`). No user info, query, fragment or `%`.
  - The URL is normalized: lowercase host, no default port, no trailing `/`. The key is 1..=256 base64url characters.
  - Errors: `bad_url` / `bad_key`.
  - Same config: no-op. A different config (or `None`) stops the client, so links on the relay path go down.
- **`relay_status() -> RelayStatusInfo`** and **`PeerEvent::RelayStatus(RelayStatusInfo)`** on every change. These
  are the protocol types, re-exported from krypto-peer.
  - `last_error` codes:
    - `unreachable`, `timeout`
    - `bad_key` (HTTP 401/403), `unavailable` (503), `rate_limited` (429)
    - `bad_auth`, `bad_frame`, `unsupported_version`
    - `replaced` (close 4000: another socket with our key; reconnects at `backoff_max`)
    - `closed`
- **`PeerEvent::LinkPath { device_id, path }`** comes right after every `LinkUp`. `link_path(&str) -> Option<LinkPath>`
  uses the protocol `LinkPath`.
- **Path changes:**
  - Every path change also emits `LinkUp` (LAN takeover, relay fallback, a peer that re-dialed over the relay).
  - The hub's existing "resync on every LinkUp" covers frames lost in the switch. LinkDown happens only when the
    logical link really ends.
- **`stop_listening().await`** (as t13 pinned):
  - No-op when the listener is off.
  - On return the listener is closed and its port released. Every link it accepted has been closed: `LinkDown`,
    awaited.
  - Links this hub dialed, and relay links, stay up. `listen()` works again afterwards.
- **`join_invite(&str) -> Result<Joined { device, machine, adopted_relay }, JoinError>`.** `join()` is unchanged (a
  wrapper).
  - The LAN addr goes first: 3 s when the URI names a relay, else the full 10 s. Then the relay.
  - With no relay configured, the invite's `relay` + `rk` is turned on and kept: `adopted_relay: Some(..)`, so the
    hub stores it or calls `set_relay(None)`. If the join fails it is turned off again.
  - A different configured relay is never switched: `unreachable`.
- **`Invite`:**
  - `addr: Option<String>` is now optional; the URI may leave it out when `relay` is present.
  - New fields `relay`, `relay_token` (`rt=`), `relay_key` (`rk=`), all `Option`.
  - `rt` and `rk` are redacted in Debug.
  - `parse` stays strict: `relay`+`rt` come together, `rk` needs `relay`, the URL is normalized, and duplicates are
    refused.
- **`create_invite`:**
  - It also works with the listener off while the relay is connected (relay-only URI). `peer_listener_off` only
    when neither is up.
  - It registers `allow_pairing{rt, exp}` on the relay.
  - At most 4 live relay invites; the oldest is dropped, as the relay drops its slot.

## Relay client (`node/relay.rs`)

- **Connect and auth:**
  - Connect to `{url}/v1/connect?pk=<b64url pk>&k=<key>`.
  - Answer the challenge: a NaCl box of `krypto-relay-auth-v1:{nonce}:{host}`, where host is the Host header exactly
    as tungstenite sends it.
  - After `bad_auth` we close the socket ourselves.
- **First frames:** then `allow` with every non-revoked roster key (resent when it changes) and `allow_pairing` for
  every live relay invite.
- **Liveness:** `ping` every `presence_interval`; the session is dead if nothing is heard for `offline_after`.
- **Reconnect** with backoff 1 s → 60 s plus jitter.
- **Inbound limit:** RELAY_FRAME_MAX_BYTES + 4 KiB (a msg is the frame + 64 bytes, per sde-relay).
- **Pacing:** client-side token bucket at 40 frames/s, burst 150 (the relay allows 50/200).
- **One outbound queue (256)** is shared by all relay-path links. A link pulls bodies only while there is room, so
  the existing per-link queue, byte budget and `Backpressure` apply unchanged.
- **Inbound `msg`** (all mismatches are dropped with a warning, never the payload):
  - `from` must equal the roster key of `env.from`, or `env.pk` for a pair_request.
  - Then the usual open, seq and direction checks.
- **Handshake over the relay** (there is no socket per link):
  - `hello` means "I just (re)connected, answer". The answer is a presence, which never asks for one, so there is
    no ping-pong.
  - Any fresh, authenticated frame from a paired device without a link registers one. If it was not the answer to
    our own hello, we say hello, so the peer resyncs.
  - A hello on a live relay link means the peer lost its state: `LinkUp` (resync) and presence.
- **When the dial loop sends a hello:**
  - Once per relay session per unlinked device, only after a LAN dial failed if there is an addr (LAN preferred).
  - Again after a LinkDown or a cooldown.
  - With backoff after `not_allowed`.
  - Never periodically: mailboxes queue frames for an offline peer.
- **LAN vs relay:**
  - A LAN link replaces a relay link (register tie-break).
  - **Fallback without LinkDown** when any of these happens and the relay is up:
    - the LAN socket closes, or a send to it fails;
    - it is quiet past `offline_after`;
    - a relay hello arrives;
    - relay frames arrive after the LAN has been quiet for a presence interval.
  - After a fallback, the peer must be heard within `handshake_timeout`, else LinkDown.
  - The relay session ending drops relay-only links at once.
- **Relay pairing:**
  - The joiner allows the inviter's key *before* sending its pair_request with `pairing = rt` (sde-relay note 1).
  - The inviter allows the new device before its pair_accept, which goes out on the same queue.
  - `not_allowed` for a pending join fails it fast with `invite_rejected` (the slot was used or expired).
  - Pair requests through the relay get the same rate limiter, keyed by the sender's key, and
    `PairRejected.remote_addr = "relay"`.
  - A pair_request older than JOIN_TIMEOUT + 60 s is refused: it waited in our mailbox and its joiner gave up
    (prevents one-sided pairings).
- **Revoke, disconnect, shutdown:**
  - Revoke sends `bye{revoked}` on the active path and then drops the peer from our allow list.
  - A `bye{revoked}` that waited in our mailbox is honoured even without a link.
  - `disconnect` cooldowns also drop relay frames.
  - Shutdown flushes the links' byes through the relay before closing it.
- **TLS:** the approved direct `rustls` dependency (std + tls12 + aws_lc_rs) installs the provider once before the
  first `wss://` connect.

## Flake review (t13 asked)

The 58/59 flake was most likely a **real race, now fixed**. Three pre-existing bugs, all visible under load:

1. **`io.rs` writer.**
   - When a link queued `bye` + close and dropped its receiver, the socket reader exited on the peer's next frame.
     The writer's `select!` then picked "reader finished" at random and could exit without sending the bye.
   - Fix: queued frames come first (`biased`), and once the reader is gone the writer flushes what is queued.
   - Regression test: the old code fails it 2 times in 3.
2. **A failed LAN send ended the link without reading what the socket had already delivered.** The usual victim was
   the peer's `bye{revoked}`, whose close had ended our writer.
   - Fix: a failed send is handled like a lost socket. Drain the delivered frames (honour a bye), then fall back to
     the relay or end.
3. **A `bye{revoked}` arriving on a link that just lost a tie-break** (both sides dialed) was ignored during its
   grace period.
   - Fix: it now revokes, and closes the winning link first, so LinkDown still comes before Revoked.
   - Deterministic regression test with raw sockets.

Test timing fixes:
- `fast()` handshake 500 ms → 2 s. It also bounds dial replies and the replaced-link grace.
- Cooldown windows are measured from the disconnect, with longer cooldowns and `sleep_until`.
- Stalled-peer tests use `offline_after = 30 s`.
- A `link_works` helper is used where both sides dial: a body queued on a tie-break loser is dropped by design.
- `settle()` waits before revoking, and the mock-relay tests wait for the pairing slot.

## Hub patch spots (approved; sde-hub owns the rest)

- `crates/hub/src/core/peers.rs` `create_invite` log line: `invite.addr.as_deref().unwrap_or("relay only")`.
- `crates/hub/src/core/peers.rs` `on_peer_event`: two debug-log arms, `PeerEvent::LinkPath` and
  `PeerEvent::RelayStatus`.
- `crates/hub/src/core/tests.rs:991`: `.addr.as_deref() == Some(listening.as_str())`.

Phase B for sde-hub:
- persist `adopted_relay` (use `join_invite`);
- wire `set_relay` / `relay_status` / `RelayStatus` into settings and `PeerStatus.relay`;
- fill `Device.path` from `LinkPath`;
- toggle the listener with `stop_listening()` / `listen()`.

## Files (crates/peer unless noted)

| File | Change |
|---|---|
| `Cargo.toml` | tokio-tungstenite `rustls-tls-webpki-roots`; `rustls` 0.23 (std, tls12, aws_lc_rs). Approved. Cargo.lock gains rustls/tokio-rustls/webpki-roots |
| `src/relay.rs` (new) | `RelayConfig`, `RelayConfigError`, URL/key rules, relay addresses, auth answer, pacer, TLS provider; unit tests |
| `src/node/relay.rs` (new) | relay client, session/auth/pump, inbound routing + key binding, first-frame registration, relay pairing, allow list, join over the relay + adoption |
| `src/node.rs` | logical links with LAN/relay paths, fallback/relink, `PeerEvent::{LinkPath, RelayStatus}`, `Joined`/`join_invite`, `set_relay`/`relay_status`/`link_path`/`stop_listening`, relay-aware dial loop, invites with relay slots, the three race fixes |
| `src/invite.rs` | `addr: Option`, `relay`/`rt`/`rk` params, `valid_host` |
| `src/io.rs` | close codes (`close_code()`), `split()`/`WsTx`, `send_wait`, writer flush fix + test |
| `src/limits.rs` | `PairKey` (IP or relay sender key) for the pair limiter |
| `src/lib.rs` | exports; re-exports protocol `LinkPath`, `RelayStatusInfo` |
| `src/node/tests.rs` | Option addr, timing fixes, `link_works`/`settle`, stop_listening and revoke-in-grace tests |
| `src/node/tests/relay.rs` (new) | in-process mock relay (access key, Host-bound auth, allow, one-shot slots, queueing, 4000 replace) + 10 tests + ignored real-relay smoke |

## Tests added (59 → 80, +1 ignored)

- **Unit:**
  - URL rules, config/key rules (Debug redaction), strict addresses, auth answer opens on the relay side, pacer, TLS
    provider idempotent.
  - Invite relay params round trip / strictness.
  - The io writer flush.
- **Mock relay:**
  - auth, status and config errors, same-config no-op, pings keep the session;
  - bad key, bad auth (another Host), retry, `replaced`;
  - relay-only pairing, link, bodies both ways, the largest body, allow lists, one-shot slot (a second join is
    refused fast);
  - relay restart → LinkDown then relink; relay off drops relay links;
  - peer restart over the relay → relink without LinkDown;
  - LAN takeover, LAN loss fallback and stop_listening fallback, no LinkDown on the dialer;
  - `from` ≠ roster key dropped (the genuine copy passes), forged pair_request dropped;
  - join with a dead LAN addr falls back and adopts the invite relay; a bad-key adoption is reverted; another
    configured relay is not switched;
  - revoke over the relay reaches the peer and both allow lists;
  - disconnect cooldown drops relay frames, relink afterwards.
- **Node:** `stop_listening_closes_accepted_links_only`, `bye_revoked_on_a_replaced_link_still_revokes`.
- **Smoke:** `relay_smoke_against_a_real_relay`, ignored by default. Run it with
  `KRYPTO_RELAY_SMOKE_URL=ws://127.0.0.1:8787 KRYPTO_RELAY_SMOKE_KEY=<key> cargo test -p krypto-peer -- --ignored relay_smoke`.

## Known gaps

- **Frames in flight across a path switch or a LAN tie-break swap can be lost** (one seq counter, two paths). Every
  switch emits LinkUp, and the hub resyncs.
- **A revoked device learns it on its next hello** (fixed below), but only once the hub's store implements
  `Roster::get_revoked` (sde-hub). Until then the default (None) only loses that answer.
- **A relay blip drops relay-only links** (LinkDown, then a relink after reconnect).
- **A half-open LAN socket** is detected by `offline_after` unless relay frames arrive first.
- **If the peer drops our hello** (e.g. its cooldown), the next hello comes only after a LinkDown, the cooldown's
  end, a reconnect, or a refusal backoff.
- **Pasting an invite within milliseconds:** the slot travels on the inviter's socket, so the joiner can reach the
  relay first and get `invite_rejected`.
- **More than 64 paired devices:** the least recently seen are left out of the allow list.
- **`join()` (old signature) also adopts a relay** but cannot report it. The hub should move to `join_invite`.
- **aws-lc-rs builds for krypto-peer alone too** (build time). It is the provider the workspace already builds for
  reqwest.

## Commands run

- `cargo test -p krypto-peer` ×5 (80 passed each).
- The test binary 12× in parallel, many rounds (the last 5 rounds 60/60, then 12/12 after the final change).
- `cargo clippy -p krypto-peer --all-targets -- -D warnings`, with and without `--no-deps`.
- `cargo check -p krypto-hub --tests` and `cargo test -p krypto-hub` (152 passed).
- The real-relay smoke via `bunx wrangler dev` (started and stopped by me; the access key was read from `.dev.vars`
  into the environment only, never printed).

## Follow-up 1: revoked devices are told (t13 approved)

- **New `Roster::get_revoked(&self, id) -> Option<RosterEntry>`.** It has a default impl returning None, so the hub
  compiles; MemoryRoster implements it. **sde-hub: implement it in the store.**
- **A revoked device's LAN hello gets `bye{revoked}`,** at most once per device per minute. The rate limit is checked
  before any crypto. The hello must open with the stored key and pass the seq check, so a replay gets nothing.
- **The relay path:** the same answer goes to a revoked device's relayed frames when they reach us (only while our
  allow list lags). Otherwise the bye from the revoke itself waits in its mailbox and is honoured even without a link.
- **The dialer side:** an authentic `bye{revoked}` in answer to our hello now revokes the peer here too (`Revoked`,
  `by_peer: true`), and dialing stops.
- **Tests:**
  - `revoked_device_hello_is_answered_with_bye_once_a_minute`: forged and replayed hellos stay silent; then one bye;
    then silent within the minute.
  - `revoked_while_offline_peer_learns_it_when_it_comes_back`.
  - `revoked_while_off_the_relay_the_peer_still_learns_it`: the queued bye, and the answer path through the relay,
    with a forged sender ignored.
  - Roster unit tests for MemoryRoster and the default.

## Follow-up 2: transport security review (t13)

1. **(HIGH) Auth nonce domain.**
   - The auth box is made with our long-term key and a relay-chosen key and nonce. A hostile relay could pick a
     peer's key and the nonce of an envelope we sent that peer, and learn its keystream.
   - Fix: a challenge whose nonce byte 15 is 0/1 (the envelope direction domain) is refused. So is a `relay_pk` equal
     to our own key, any roster key, or a pending join's inviter. Refusal means `bad_auth`, close, backoff.
   - The Roster cannot list revoked keys; the nonce domain alone already rules out every envelope nonce.
   - The mock relay now sends byte 15 >= 2, like the real one. The real-relay smoke was rerun 3×: passed.
   - Tests: `auth_refuses_challenges_that_could_reuse_an_envelope_nonce` (unit, including real envelope nonces) and
     `hostile_relay_challenges_are_refused`. For the envelope nonce, our own key and a peer key: no auth frame is
     ever sent; then an honest challenge connects.
2. **(MED) An adopted relay sees only the inviter.** While a join runs on the invite's relay (turned on because none
   was configured), the allow list holds only the inviter's key and no hellos go out. Once the pairing succeeds, the
   full allow list follows, then the hellos.
   - Test: `an_adopted_relay_sees_only_the_inviter_until_pairing_succeeds` (an ordered log of everything the joiner
     sent the relay).
3. **(MED) Pair limiter.**
   - All relay origins share one bucket, `PairKey::Relay`, kept outside the IP table, so relay traffic never evicts
     an IP entry.
   - Global cap: 60 pair attempts per minute across all origins, checked before any crypto.
   - Tests: unit `relay_origins_share_one_bucket_that_never_evicts_ips` and `global_cap_on_pair_attempts`. Node test
     `relay_pair_attempts_share_one_bucket_and_spare_the_lan`: 5 bad codes from 5 made-up relay senders block the
     relay bucket; the right code over the relay is then dropped; the LAN listener still pairs.
4. **(LOW)** A hello or presence stamped more than 1 h from our clock is dropped: link handshakes, live links and the
   relay first frame.
   - Test: `hello_and_presence_stamped_far_from_our_clock_are_dropped`.
6. **Cheap seq check before `open()`** everywhere an envelope is opened: links, handshakes, dial reply, pair
   request/reply, relay first frame, revoked answer.
   - Test: `replayed_frames_are_dropped_before_decryption`, which counts decryptions (cfg(test)): replays and a stale
     garbage box cost none.

Files touched by the follow-ups: `src/roster.rs`, `src/envelope.rs` (`RANDOM_LEN` is crate-visible), `src/relay.rs`,
`src/limits.rs`, `src/node.rs`, `src/node/relay.rs`, tests.
