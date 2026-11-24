# sde-peer — `krypto-peer`, step 3 (mirror events + visitors: peer layer)

Status: **done**, including t13's follow-ups (byte budget, `Reserved`, `disconnect`). `cargo test -p krypto-peer`: 59 passed
(5 parallel runs plus 1 serial run, no flakes).
`cargo clippy -p krypto-peer --all-targets -- -D warnings`: clean. Branch `step-3-mirror-visitors`, nothing committed.
Step 2 (pairing, links, presence and the security fixes) is committed in f59e839. Its full report is in git history;
this file now covers step 3.

## API changes (contract "Peer layer (sde-peer)")

- **`PeerEvent::Body { device_id: String, body: PeerBody }`**
  - Every authenticated body other than pair, hello, presence and bye is delivered this way: mirror, sync_request,
    mirror_snapshot, mirror_events, sync_done, and any variant later steps add (the match has a catch-all).
  - Bodies arrive in order per link, because one link task processes its frames in sequence.
  - Bodies are never logged.
- **`PeerNode::send(&str, PeerBody) -> Result<(), SendError>`** replaces the old `bool`. It never blocks.
  - `NotLinked`: there is no live link.
  - `TooLarge`: the serialized body is over `MAX_SEND_BODY_BYTES` (= `MIRROR_FRAME_MAX_BYTES` + 16 KiB = 216 KiB,
    exported as `krypto_peer::MAX_SEND_BODY_BYTES`). Nothing is sent.
  - `Backpressure`: the link's body queue is full, either 1024 frames or a **16 MiB budget of serialized body bytes**
    (approved by t13). The link is closed at once, without bye; `LinkDown` follows, and the peer resyncs when it
    reconnects. Bytes are reserved atomically before queueing (concurrent senders can't overshoot together) and
    released when the link task takes the body.
  - `Reserved` (approved by t13): pair_request / pair_accept / pair_reject / hello / presence / bye are refused,
    whether or not a link exists. Presence still goes through `set_presence`.
  - `Display` gives `not_linked` / `too_large` / `backpressure` / `reserved`.
  - The body is serialized once in `send` (needed for the size check) and sealed from those bytes
    (`envelope::seal_plaintext`).
- **LinkUp ordering:** every `Body` comes after the `LinkUp` of its link. `register` emits LinkUp before the link task
  reads the first frame.
  - **Change:** LinkUp now also fires when a new link *replaces* the live one (re-pair, reconnect, duplicate-link
    tie-break). Bodies still queued on the replaced link are dropped, so the hub must resync on every LinkUp. The
    hub's handler is already idempotent. I told t13 / sde-hubcore.

- **`PeerNode::disconnect(&self, device_id: &str, cooldown: Duration)`** (sync; added for sde-hubcore to close a
  flooding peer):
  - It closes the live link without revoking and without bye (a WebSocket close frame, or a kill if the control queue
    is backed up). `LinkDown` follows.
  - For `cooldown` the dial loop sleeps instead of dialing, and inbound hellos from the device are closed silently
    (checked before any crypto). `register` also refuses non-pairing links, so a handshake already in flight can't
    slip through. The cooldown is set under the same gate as registration.
  - After the cooldown the dialer resumes with its usual backoff.
  - A new pairing (an explicit user action) ends the cooldown. A zero cooldown just drops the link.
  - No-op for an id that is neither paired nor linked.

## Internals

- **Per-link queues:**
  - A control queue (16; presence push, close, superseded) is always served.
  - A body queue (1024) is pulled only while the socket queue has room. When the socket queue is full, the link task
    waits on `reserve()`, so a slow peer backs up into the body queue, and the body queue's overflow is the
    `Backpressure` close.
  - Presence is best effort: if the socket queue is full, that beat is skipped and the link stays up.
- **`WsIo`:**
  - `send` now reports `Full` vs `Closed`.
  - `close()` aborts the socket if the close frame can't be queued, and there is a new `abort()` used on overflow, so
    a stuck peer's socket is dropped at once instead of after the 10 s write timeout.
  - `revoke` / `shutdown` fall back to killing the link if its control queue doesn't take the close within 2 s.
- **WebSocket frame/message cap raised from 256 KiB to 320 KiB.** The largest allowed body (216 KiB) is about
  288 KiB once boxed and base64'd. t13 approved this and recorded it in CONTRACTS.md. A test sends exactly
  `MAX_SEND_BODY_BYTES` both ways.
- `Machine.user_active_at`: only the test helper's struct literal needed it. The node passes `Machine` values through
  untouched, so presence carries whatever the hub sets.

## Files changed (all in `crates/peer/`)

| File | Change |
|---|---|
| `src/node.rs` | `PeerEvent::Body`, `SendError`, `MAX_SEND_BODY_BYTES`, control + body queues with kill-on-overflow, LinkUp on every registration, catch-all Body delivery |
| `src/io.rs` | `MAX_FRAME` 320 KiB, `SendFail`, `WsIo::abort`, `close()` falls back to abort |
| `src/envelope.rs` | `seal_plaintext` (seal pre-serialized bytes); `seal` uses it |
| `src/lib.rs` | exports `SendError`, `MAX_SEND_BODY_BYTES` |
| `src/node/tests.rs` | `user_active_at: None` in the helper, `send` assertions on `Result` (the step-2 pairing test now sends a mirror body instead of presence), 6 new tests, LinkUp-on-replacement assertions in the tie-break test |

## Tests added (59 total)

- `bodies_follow_link_up_in_arrival_order`: a raw peer bursts hello plus 51 bodies (with a presence and a second hello
  mixed in) before reading. The hub gets LinkUp first, the bodies in order, and no Body for presence/hello.
- `bodies_between_nodes_in_order_both_ways`: the joiner sends 300 bodies the moment `join` returns, and the inviter
  sends 300 the moment it sees LinkUp. Both sides get them in order, after LinkUp.
- `too_large_body_never_sent_largest_allowed_delivered`: `MAX_SEND_BODY_BYTES + 1` gives `TooLarge` and nothing
  arrives. Exactly `MAX_SEND_BODY_BYTES` is delivered both ways, the link stays up, and the error codes are checked.
- `backpressure_closes_link_and_peer_reconnects`:
  - A raw peer stops reading. Sends succeed for at least 1024 bodies, then give `Backpressure`.
  - LinkDown follows and `send` gives `NotLinked`.
  - The peer reconnects with a fresh hello, LinkUp fires, and a body is delivered and opens on the peer.
- Tie-break test: both sides now also see LinkUp for the replacing link.
- `byte_budget_overflow_closes_link`: a stalled peer gets 200 KiB bodies. `Backpressure` comes after at least
  16 MiB / 200 KiB bodies and well before 1024 frames; LinkDown follows and then `NotLinked`. (The frame-cap test now
  uses 4 KiB bodies, so it trips on the frame count, not the budget.)
- `send_refuses_reserved_bodies`: all six control bodies (both bye reasons) give `Reserved`, linked or not. The peer
  sees no effect (not revoked, no presence, no Body, link up); an ordinary body still goes through.
- `bodies_between_nodes_in_order_both_ways` also checks that the queued-byte counter is back to 0 after delivery.
- `disconnect_cools_down_both_directions_then_reconnects`:
  - Two listening nodes (so both dial): LinkDown on both sides, and no LinkUp in either direction during the 1.5 s
    cooldown.
  - Not a revoke: rosters intact, no Revoked.
  - It reconnects by itself after the cooldown and bodies flow.
  - Zero cooldown gives LinkDown then LinkUp; an unknown id is a no-op.
- `disconnect_refuses_raw_hello_during_cooldown`: the old socket closes without bye, a fresh hello during the cooldown
  is closed silently, and the same hello after it is accepted (LinkUp).

## Commands run

- `cargo check -p krypto-peer --tests`: clean.
- `cargo test -p krypto-peer`: 59 passed, 5× in parallel plus 1× `--test-threads=1`.
- `cargo clippy -p krypto-peer --all-targets -- -D warnings`: clean.
- `cargo check -p krypto-hub`: fails only on sde-hubcore's pending step-3 work: the new `PeerEvent::Body` arm,
  `ClientMsg::UserActivity`, and `user_active_at` in Machine literals.

## Known gaps

- **Worst-case memory per link:** the 16 MiB body budget, plus up to 64 sealed frames in the socket queue (≈ 18 MB at
  the max body size), plus TCP buffers.
- **Bodies queued on a replaced link** (tie-break swap) are dropped. The resync after the new LinkUp covers sessions,
  pets, pending permissions and events, but not a live `notification` or `permission_resolved` sent in that window.
- **seq persistence costs one roster write per frame each way** (`set_last_tx_seq` on send, `set_last_rx_seq` on
  receive). That is fine at mirror rates; a sync burst of a few hundred frames means a few hundred SQLite UPDATEs.

## Decisions (resolved by t13)

1. Per-link byte budget: **16 MiB** of serialized bodies, handled exactly like the frame-cap overflow. Done.
2. `send` refuses control bodies with **`SendError::Reserved`** (`"reserved"`). Done. sde-hubcore sends none.

Nothing open.
