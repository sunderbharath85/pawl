# sde-peer — `krypto-peer` (step 2: pairing + LAN presence)

Status: **done, security-review fixes landed** (see "Security fixes" below). `cargo test -p krypto-peer` passes
(51 tests, 5 parallel runs plus 1 serial run, no flakes), and `cargo clippy -p krypto-peer --all-targets -- -D warnings`
is clean. `cargo check -p krypto-hub` (sde-hubcore's integration) compiles against the crate.

## Security fixes (independent review, relayed by t13)

| # | Finding | Fix | Regression test |
|---|---|---|---|
| 1 | **Stale `pair_accept` replay.** The joiner skipped the seq check for revoked or unknown rows, then set `last_rx` to the reply's seq. An on-path attacker could replay an old accept, roll `last_rx` back, then replay old hello/presence/`bye{revoked}`. | The inviter puts `reqSeq` (the request envelope's seq) in `pair_accept`, and the joiner rejects a mismatch. Both sides now require `seq > stored last_rx`, even for revoked or unknown rows. The inviter checks this before the code (no invite is spent), so `last_rx` never goes down. `Roster::last_rx_seq` must return the stored value for revoked rows: `MemoryRoster` and the hub's `device_seq` both do, so the trait needs no change. | `replayed_old_pair_accept_rejected_on_rejoin`: replayed old accept, echoed req_seq with an old seq, wrong req_seq, plus a control that is accepted. Also the revoked-row part of `pair_request_with_live_invite_but_bad_body_gets_bad_request`. |
| 2 | A wrong code burned every invite, so any past or revoked peer that knows the hub's public key could block pairing forever. | A wrong code consumes **nothing**; a correct code is still single use. `PairLimiter`: 5 failed attempts per IP per 60 s window block pair requests from that IP for 60 s (dropped, no reply, no event). It tracks at most 1024 IPs, pruning idle ones and then evicting the oldest. Keys are the IPv4 address or the IPv6 /64. `PairRejected` is coalesced to at most one per IP per minute; the coalesced count goes to the log. | `wrong_code_rejected_invite_still_usable`, `pair_failures_rate_limited_per_ip_and_events_coalesced`, plus `limits` unit tests (block/coalesce, window reset, bounded map). |
| 3 | Unauthenticated sockets could be exhausted, and nothing limited the pre-upgrade HTTP phase. | `limits::PeerListener` is a custom `axum::serve::Listener`, no new dependencies. It caps accepted connections at **64** in total (authenticated links included; at the cap it stops accepting). Unauthenticated connections are capped at **16** in total and **4 per IP** (per /64 for IPv6); over the cap they are dropped at accept, before any HTTP. `GuardedStream` fails reads and writes with `TimedOut` once `handshake_timeout` (5 s) passes after accept unless the connection has authenticated. The deadline covers the header read, keep-alive idling, the upgrade and the first frame; the timer is polled, so a silent client is cut too. The handler gets a `PeerConn` connect-info carrying the shared state. Authenticating lifts the deadline and frees the slot. | `first_frame_deadline_and_per_ip_unauthenticated_cap`, `total_unauthenticated_cap_across_ips` (16 silent sockets from 127.0.0.1–4, then 127.0.0.5 refused; skips if the OS can't bind extra loopback sources, and it ran here), `slow_http_cut_at_deadline` (partial headers, and an idle keep-alive after a 404), plus a `limits` unit test. |
| 4 | Peer-supplied addrs were dialed. | `invite::peer_addr_ok`: a peer-reported addr (`pair_request` / `hello` on first frame / `hello` reply / `hello` on a live link) is stored only if it is an IP literal in 10/8, 172.16/12, 192.168/16, 100.64/10, 169.254/16, 127/8, ::1, fe80::/10 or fc00::/7 (IPv4-mapped included). Anything else is ignored and the stored addr kept. A public addr in a `pair_request` no longer fails pairing; it is just not stored. The invite URI addr (user-typed) stays unrestricted. | `peer_reported_public_or_dns_addr_ignored` (public IP in pair_request, then DNS / public / LAN / IPv6-doc / CGNAT in hellos) and `invite::peer_reported_addr_allowlist`. |
| 7 | `percent_decode` accepted `%+f` (`from_str_radix` takes a sign). | Requires exactly two ASCII hex digits. | `invite::percent_decode_needs_two_hex_digits`. |

`PairAccept` now carries `req_seq` (`reqSeq` on the wire); t13 added it to the protocol. The contract's
"at most 16 unauthenticated sockets" is now enforced at accept: the connection is dropped, where before it got HTTP 503.

## Files (all new, under `crates/peer/`)

| File | What |
|---|---|
| `Cargo.toml` | deps: workspace tokio/serde_json/anyhow/thiserror/tracing/futures; hub versions of axum 0.8.9 (ws), tokio-tungstenite 0.29.0, getrandom 0.4.3, subtle 2.6.1, hex 0.4.3; **new: `crypto_box = 0.9.1`** (latest stable; 0.10 is pre-release) with `default-features = false, features = ["alloc", "salsa20"]`. Keys and nonces come from getrandom 0.4, so crypto_box's rand_core/getrandom feature stays off. |
| `src/lib.rs` | re-exports the contract API |
| `src/b64.rs` | hand-rolled RFC 4648 base64: standard padded + url-safe unpadded, strict and canonical decoding (rejects bad padding, mixed alphabets, whitespace, non-zero trailing bits). RFC 4648 §10 vectors. |
| `src/keys.rs` | `DeviceKeys`: `load_or_create` (64 hex chars, `create_new` so two hubs racing on first start don't overwrite each other, unix mode 0600, a corrupt file is an error and is never replaced), `generate`, `public_key`, `public_b64`. `Debug` prints only the public key. No secret accessor. Buffers are wiped. |
| `src/invite.rs` | `Invite` (`to_uri`/`parse`; `Debug` redacts the code), `InviteError` (`bad_uri`, `peer_listener_off`, `invalid`), addr validation |
| `src/envelope.rs` | `seal`/`open`/`OpenError` |
| `src/roster.rs` | `Roster` trait (implementer notes in the doc comment), `RosterEntry`, **extra `MemoryRoster`** (reference implementation, t13 OK'd it) |
| `src/limits.rs` | connection caps + unauthenticated deadline (`PeerListener`, `GuardedStream`, `PeerConn`) and the pair-attempt limiter (`PairLimiter`) |
| `src/io.rs` | one WebSocket adapter for both axum and tungstenite sockets (reader/writer tasks, channels, 10 s write timeout, 1 s close grace) |
| `src/node.rs` | `PeerNode`, `PeerEvent`, `PeerTimings` (Copy), `JoinError`: listener, pairing, dialer, links, presence, revoke, shutdown |
| `src/node/tests.rs` | node tests (real sockets on 127.0.0.1:0, `MemoryRoster`, short timings) |

`Cargo.lock` changed: crypto_box and its RustCrypto deps were added (crypto_secretbox, salsa20, poly1305, x25519/curve25519, etc.).

## Behaviour (contract plus the decisions I made)

- **Nonce layout (approved by t13):** `15 random bytes || dir || seq u64 BE`, where `dir = 1` if sender_pk < receiver_pk
  (bytewise). `open` rejects a mismatch. This authenticates `seq`, so a captured box can't be replayed with a bumped seq.
  It also stops reflection: an X25519 box is valid in both directions of a key pair.
- **Listener:** only `GET /peer`. Every other path and method gets 404. Any `Origin` header gets 403; the check is a
  middleware covering the fallback too. Frames are capped at 256 KiB. The first frame must arrive within
  `handshake_timeout` (5 s), and the same deadline (from accept) covers the HTTP phase. Connection caps: 64 total,
  16 unauthenticated, 4 unauthenticated per IP (see fix 3). Binding 0.0.0.0 or :: logs a warning.
- **First frame with `pk`:** these are closed without a reply: the IP is blocked by the pair limiter, no invite is
  live, or the pk is malformed or equals our own. These fail with a `pair_reject{bad_request}` reply, count as an IP
  failure and give a (coalesced) `PairRejected`: decrypt failure, a body other than `PairRequest`, `machine.id != from`,
  an invalid machine id, `from == me`, `seq <= stored last_rx`. These checks do **not** consume invites. Then the code
  is checked:
  - The code is compared in constant time against every live invite, with no early exit.
  - On a match, that invite is consumed.
  - A wrong code consumes **nothing** (fix 2). It replies `invalid_invite` and counts as an IP failure.
  - On success: the addr is filtered by `peer_addr_ok`, then upsert the device, `set_last_rx_seq`, emit `Paired`, and
    reply `pair_accept{machine, addr: the invite's addr, reqSeq}`. The socket becomes the live link.
- **First frame without `pk`:** `from` must be in the roster and not revoked, open with the roster key, the body must be
  `Hello` with `machine.id == from`, and `seq > last_rx`. Otherwise it is closed without a reply.
- **Every later frame:** `from`/`to` are fixed per link, it must open with the link's roster key, and seq must be
  strictly above the persisted `last_rx`. Otherwise the frame is **dropped**; the link stays up. Any authenticated
  frame resets the offline timer.
- **seq:** sender uses `max(last_tx+1, now_ms*1000)`, persisted per frame under a lock. Receiver check-and-set is under a
  lock and runs only after a successful decrypt. Reject replies use a time-based seq and never write the roster.
- **Dialer:** one loop per roster hub that has an addr. It dials only while no link exists in either direction.
  - Backoff runs `backoff_min` → `backoff_max`, with jitter uniform in [d/2, d).
  - After one of our own links ends, the redial waits `backoff_min` if the link lasted at least `offline_after`;
    otherwise the backoff keeps doubling, so a flapping peer can't cause a tight loop.
  - The loop exits on revoke, a missing addr or shutdown. `connect_known` is idempotent.
  - Pairing and inbound links also start the loop when the peer has an addr.
- **Tie-break:** when dialers differ, keep the link dialed by the smaller machine id. A reconnect by the same dialer
  replaces the old link, which fixes half-open sockets. A pairing link always replaces the old link. The losing link
  is closed without a reply. A **replaced** link stops being used at once but stays open for `handshake_timeout`
  before closing, so the far side never sees a LinkDown/LinkUp flap. A test covers this.
- **Presence:** sent right after the hello or pair, every `presence_interval`, and right after `set_presence` or
  `set_machine`. `ts`, `machine.lastSeen` and `machine.online` are set at send time. Before the first `set_presence`
  call, a minimal snapshot (machine only) is sent. `set_presence`'s machine also updates our machine; the id never
  changes.
- **Revoke:** marks the roster revoked first, then stops the dialer, sends `bye{revoked}` and closes.
  - Events: `LinkDown` (if it was linked) then `Revoked{by_peer:false}`.
  - Receiving `bye{revoked}` gives the mutual result: roster revoked, dialer stopped, `LinkDown` then `Revoked{by_peer:true}`.
  - An id that is neither paired nor linked is a no-op.
  - A `gate` lock serializes revocation against link registration, so a handshake racing a revoke can't bring a
    revoked device back up.
- **Shutdown:** idempotent. Aborts dialers, sends `bye{shutdown}` to every link (waiting up to 2 s each, in parallel),
  clears invites, stops the listener (graceful, aborted after 2 s) and frees the port. The restart test rebinds the
  same port.
- **Event order:**
  - `Paired` → `LinkUp`. `LinkUp` is also emitted when re-pairing over a live link.
  - `LinkDown` → `Revoked`.
  - `LinkUp` fires only on the transition from no link to a link (or when pairing).
- **`join`:** the whole call is capped at 10 s (`timeout`). Errors:
  - `SelfInvite` if the URI has our machine id or our public key.
  - Connect failure → `unreachable`.
  - These give `invite_rejected`: closed without a reply, `pair_reject`, a reply that won't open with the URI key, a
    wrong `from`/`machine.id`, `reqSeq != our request seq`, or `seq <= stored last_rx`.
  - It stores A with the URI addr and returns the machine with `online: true`.
  - Expiry is **not** checked locally (clock skew); the inviter decides.
- **`create_invite`:** `peer_listener_off` when the listener is off. `invalid` when the TTL is outside 60..=3600 s, the
  addr is invalid or unspecified, or no addr is given while bound to 0.0.0.0. At most 16 live invites (the oldest is
  dropped). An explicit addr is remembered and advertised in hello/pair_request when the listener binds all interfaces.
- **Secrets:** never in Debug, Display or logs. That covers the secret key (no accessor), invite codes and URIs.
  Logs carry device ids, socket addrs and reasons only. Codes and key bytes are wiped where we own the buffer.
  (crypto_box 0.9.1's `SecretKey` zeroizes its scalar on drop, but not its byte copy; that's upstream.)

### Lock-order rule for sde-hubcore (also in the `node.rs` module doc)

Never call `PeerNode::revoke`, `join`, `connect_known` or `shutdown` while holding the lock your `Roster` impl uses;
they call into the roster. `online`, `send`, `set_presence`, `set_machine`, `create_invite`, `listening` and
`public_b64` never touch the roster, so they are safe anywhere. I checked `crates/hub/src/store.rs`: `update_seen` does
`COALESCE(addr)` and the seq setters store the value as given. That matches what the node expects.

## Invite URI (answer to t13 / fe-apps)

`to_uri` emits exactly `krypto://peer?v=1&mid=<12 [a-z0-9]>&pk=<b64url, 43 chars, no padding>&addr=<host:port>&code=<b64url, 22 chars, no padding>&exp=<unix ms digits>`.
`addr` is percent-escaped for any char outside `[A-Za-z0-9-._~:]`, so IPv6 `[::1]:47801` is emitted as `%5B::1%5D:47801`.

`parse` is **stricter** than the fe-apps pre-check in these ways. The pre-check can stay looser, because `join_peer`
is authoritative and returns `bad_uri`.

- Surrounding whitespace is trimmed. The `krypto://peer?` prefix is compared case-insensitively. Max 1024 chars. Any `#` → bad_uri.
- Param order is free. **Unknown params are ignored**; a **duplicate** known param → bad_uri. All values are
  percent-decoded; `+` and malformed `%XX` → bad_uri.
- `v` must be exactly `1`. `mid` must pass `is_valid_machine_id`.
- `pk`: canonical base64url, **padding rejected**, must decode to exactly 32 bytes.
- `code`: same rules, exactly 16 bytes.
- `addr`: `host:port` with port 1–65535 (digits only). Host is IPv4, a DNS name (labels of alnum and `-`, each ≤63,
  total ≤253) or bracketed IPv6. Bare IPv6 and unspecified 0.0.0.0/:: → bad_uri.
- `exp`: ASCII digits only, > 0. Not checked against the clock.

## Commands run

- `cargo fetch`: added crypto_box 0.9.1 and its deps.
- `cargo check -p krypto-peer`: clean.
- `cargo test -p krypto-peer`: 39 passed, run 6× in parallel plus once with `--test-threads=1` (39 passed).
- Security-fix round: `cargo test -p krypto-peer`: 51 passed, 5× in parallel plus 1× `--test-threads=1`.
  `cargo test ... total_unauthenticated -- --nocapture` confirmed the cross-IP test ran (no skip). Clippy is clean
  and `cargo check -p krypto-hub` compiles.
- `cargo clippy -p krypto-peer --all-targets -- -D warnings`: clean.
- `cargo check -p krypto-hub`: compiles (hubcore's in-progress integration).

## Tests (51; the first round's 39 are listed below, the 12 new ones are in the security-fix table above)

- **b64 (5):** RFC 4648 standard and url-unpadded vectors, the two alphabets, round trip over all lengths, malformed input.
- **keys (3):** persist/reload (0600 on unix), corrupt file is an error and is kept, Debug has no secret.
- **envelope (5):**
  - seal/open round trip and wrong-key failure.
  - Tamper / bumped seq / changed nonce / version fail.
  - A reflected frame fails.
  - A plain-SalsaBox interop check.
- **invite (6):** URI round trip (IPv4, DNS, IPv6), any order plus unknown params, bad URIs, Debug redacts the code,
  error codes, addr rules.
- **node (18):**
  - Pairing:
    - Pair two nodes: LinkUp + Presence both ways, Paired before LinkUp, invite consumed, rename pushes presence,
      `send`, shutdown gives LinkDown.
    - A wrong code gives `invite_rejected` and leaves the invite usable (it used to burn it).
    - Expired invite: closed silently.
    - Reused invite.
    - `self_invite` (also our own id under another key), `bad_uri`, `unreachable`, a spoofed mid gives `invite_rejected`.
    - `create_invite` rules (ListenerOff, TTL bounds, addr, 0.0.0.0).
    - A pair_request with a bad body gets `bad_request` and doesn't consume the invite.
  - Rejections at the listener:
    - Third unpaired key, a paired id with the wrong key, pair_request with no invite, garbage: all closed without a reply.
    - A replayed hello on a new socket is closed.
  - Live links:
    - Replayed (same seq), bumped-seq, tampered, wrong-`from`, non-JSON and older-seq frames are all dropped while
      the link stays up.
    - Revoke gives bye, both sides Revoked with LinkDown first, no redial after `connect_known`, no-ops.
    - Revoke while the peer is offline.
    - Reconnect after a listener restart (same key file, roster and port) with backoff.
    - Duplicate-link tie-break both ways with no LinkDown flap.
    - Offline after `offline_after` of silence, and the socket closes.
  - Listener:
    - 404 for `/other`, `/peer/`, `GET /`, `POST /peer`.
    - 403 on Origin for `/peer` and other paths.
    - A plain `GET /peer` is not upgraded.
    - 4 silent sockets from one IP: the 5th is dropped at accept, each is closed at the deadline, then slots free up.
    - An oversized frame closes the socket.
  - Unit tests: seq rules, jitter range.

## Known gaps

- **The 64-connection total cap has no socket-level test.** Reaching it needs 64 sockets from at least 16 loopback
  sources; the logic is a plain semaphore taken before `accept`. At the cap, new connections wait in the OS backlog
  and are not refused.
- **Pair failures are counted only for attempts that reach the crypto** (a live invite exists). Attempts while no
  invite is live are closed before any work, so they are not counted.
- **Clock regressions:** if a peer's clock goes back below the seqs we stored for it, re-pairing is refused
  (`bad_request` / `invite_rejected`) until its clock passes them, because `last_rx` never goes down. Deleting the
  device row resets it.
- **Revoking a peer that is offline** (or that can't reach us) doesn't tell it. It keeps dialing us at up to 60 s
  backoff, and we close each attempt silently, per the contract.
- **Peer-supplied strings** (machine name etc.) are stored as given. The only limit is the 256 KiB frame cap. The hub
  may want to clamp names (1–40 like `update_machine`) before showing them.
- **Invite list is in memory**, so invites are lost on restart (intended).
- **`listen` is once per node.** There is no `stop_listening`; the contract doesn't have one.

## Decisions (resolved by t13)

1. A wrong code burns *all* live invites: kept at first, then **reversed** after the security review. A wrong code
   now burns nothing, and per-IP rate limiting plus event coalescing replace it.
2. `stop_listening()`: **not for now**.
3. Still open for later steps: the TS side (phone, step 7) must use the same nonce layout and the strict base64.
