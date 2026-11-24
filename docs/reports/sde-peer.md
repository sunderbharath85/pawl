# sde-peer — `pawl-peer`, step 7 (phone)

Steps 2–6 are committed (f19744a); their reports are in git history. This report covers step 7 only: protocol v8,
the roster introduction, phone peers over the relay, the offline wake and the cross-language test vectors.

Status: done. `cargo test -p pawl-peer` passed 102 tests (+1 ignored) on each of 5 runs; 12 parallel copies × 3 passed 36/36. `cargo clippy -p pawl-peer --all-targets -- -D warnings`
is clean. The vectors agree byte for byte with tweetnacl (72 checks, see "Test vectors").

## 1. Protocol v8

- `Device` literals fill `introduced_by: None` and `os: Some(machine.os)` on both pairing sides.
- `PeerBody::RosterIntro` is handled by the node (section 3). It never reaches the hub as a `PeerEvent::Body`.
- The invite decides the kind (security review, section 12): a phone invite stores the joiner as `kind: phone`
  and takes only an ios / android machine; a hub invite refuses one. The phone's addr is dropped there and from any
  later hello: phones are reached over the relay only.

## 2. Node API for the hub (final; sent to t13 as API READY)

Everything is exported from `pawl_peer`.

| Item | What it does |
|---|---|
| `check_intro_device(&IntroDevice) -> Result<[u8; 32], &'static str>` | Structural check of one introduced device; returns its key. Codes: `bad_id` (not 12 `[a-z0-9]`), `bad_name` (fails `device_name_ok`), `bad_kind` (a phone without an ios / android os, or a hub with one), `bad_key` (canonical standard padded base64 of 32 bytes, not of small order), `bad_addr` (an addr that `peer_addr_ok` refuses, or any addr on a phone). It does not look at the roster. |
| `public_key_ok(&str) -> Option<[u8; 32]>` | The key rule above, on its own. |
| `device_name_ok(&str) -> bool` | 1..=64 chars after trimming, no control characters, no bidi embedding / override / isolate (U+202A–202E, U+2066–2069), zero-width or mark (U+200B–200F) or BOM (U+FEFF). Applied to every name a peer sends (pair_request, pair_accept, hello, presence, roster_intro); the hub should hold its own machine name to it too, or peers refuse its frames. |
| `peer_addr_ok(&str) -> bool` | Private / LAN IP literal `ip:port` only (the rule pairing already used for peer-reported addrs). |
| `MAX_INTRO_DEVICES = 32`, `MAX_INTRO_NAME_CHARS = 64` | Limits. |
| `PeerEvent::RosterIntro { from, devices, invalid: Vec<(IntroDevice, &'static str)> }` | A `roster_intro` arrived on the live link of `from`, a paired device of kind hub, with at most 32 devices. `devices` passed the checks (in the sender's order); `invalid` failed, with a `check_intro_device` code or `own_device` / `sender_device` / `duplicate` (section 12). Nothing is stored. An intro from a phone, or one with more than 32 devices, is dropped whole without an event (logged without its payload). |
| `PeerNode::send_roster_intro(&self, device_id, Vec<IntroDevice>) -> Result<(), SendError>` | `roster_intro` on the device's live link, like `send`. `TooLarge` above 32 devices (nothing sent). |
| `PeerNode::roster_changed(&self)` | Same as `connect_known`: resend the relay allow list and keep a dial loop for every roster hub. Call it after storing introduced devices. LAN hellos and relay frames read the roster live, so stored devices are accepted from then on. |
| `PeerNode::wake_phone(&self, device_id) -> Result<(), WakeError>` | Offline wake (section 5). |
| `WakeError` | `NotPhone` "not_phone" (not a paired phone), `Linked` "linked" (send normally instead), `RelayOff` "relay_off" (no relay session, or a relay a running join adopted), `RateLimited` "rate_limited" (woken less than 60 s ago), `Disconnected` "disconnected" (the hub's `disconnect` cooldown is running), `Backpressure` "backpressure" (relay queue full, nothing queued; a retry is allowed). |
| `PeerNode::create_phone_invite(&self, ttl) -> Result<Invite, InviteError>` | For `create_invite{forPhone: true}`: a relay-only invite (`relay`, `rt`, `rk`, no `addr`) even when the LAN listener is on, usable only by an ios / android joiner, who is stored as a phone. `InviteError::RelayOff` ("relay_off", new variant) when the relay is not connected. |
| `PeerNode::create_invite(&self, advertise, ttl)` | Unchanged signature; now a hub-only invite (an ios / android joiner gets `pair_reject{invalid_invite}`). |

Agreed split (t13): the node checks the sender and the structure, then reports. The hub checks id and key
conflicts, revoked ids and keys, its own key and the sender's, applies `accept_roster_intros`, stores what it
accepts with `introduced_by = from`, audits and notifies, then calls `roster_changed()`.

The `Roster` trait is unchanged. `get_revoked` (still to be implemented in the hub store) also serves the hub's
"revoked id" check.

### What sde-hub has to do with it

- Add a `PeerEvent::RosterIntro` arm (the match in `core/peers.rs` is exhaustive).
- Fill `introduced_by` and `os` in `Device` literals.
- Store `os` from `Paired`, from hellos (`update_seen` gets the machine) and from intros.
- Build `IntroDevice` from roster rows. Filter `addr` with `peer_addr_ok` and leave it out for phones.
- Send intros with `send_roster_intro` per the fan-out rules, on pairing and on the recipient's next `LinkUp`.
- Use `create_phone_invite` for `forPhone`.
- Call `wake_phone` for a new local permission request when a paired phone is not linked. Ignore `Linked`,
  `RateLimited` and `RelayOff`.

## 3. Roster introduction (node side, `node/phone.rs`)

`on_body` routes `roster_intro` to `Inner::roster_intro`. It reaches that function only from a link that is up.
It applies these checks in order:

1. The roster entry of `from` must exist and be of kind hub.
2. The intro holds at most 32 devices.
3. Each device passes `check_intro_device`, or is listed in `invalid` with its code.

Then the node emits a single event. It never writes to the roster for an intro.

Low-order keys use libsodium's blocklist of 7 points, compared with bit 255 cleared. A test shows that each point
is really weak: with any of them, two out of nine random secret keys get the same box key.

Note: crypto_box 0.9 reduces the clamped scalar mod ℓ, so its box key for a small-order point differs from the
all-zero RFC 7748 result. It is still one of at most 8 values. For honest keys (prime-order subgroup), crypto_box and
tweetnacl agree; the vectors prove it.

## 4. Phones over the relay

### Hub side

- **Phones are never dialed or greeted.** `dial_loop` exits at once for a phone row. A hub would otherwise send
  hellos to an offline phone, and each frame queued for an offline phone makes the relay push to it.
  - A test checks that the hub sends nothing to the phone's mailbox after LinkDown, and that no dial loop runs for
    it.
- **Pairing** uses the same wire as a hub joiner. A test pairs a `PeerNode` whose machine os is ios through
  `create_phone_invite`. The hub listens on the LAN too, yet the invite has no addr. The test then checks:
  - kind phone and os ios on the inviter;
  - links on the relay path, and both allow lists;
  - presence both ways;
  - `sync_request` and an intent with `client: mobile` from the phone;
  - `mirror_snapshot` and an `intent_ack` from the hub.

### Wire summary for fe-mobile

Every field format is in the vectors.

1. **Connect:** `{relay}/v1/connect?pk=<own address>&k=<rk>`.
   - Answer `challenge` with `auth{box}` (see `relay_auth.json`).
   - Refuse a nonce whose byte 15 is below 2, and a relayPk equal to the phone's own key or a paired hub's key.
2. **Pair:**
   - Send `allow{[inviter address]}`.
   - Then send `send{to: inviter address, env: pair_request (env.pk = own key), pairing: rt}`.
   - Wait for `msg` with a `pair_accept` whose `reqSeq` equals the pair_request's seq, or a `pair_reject`, or
     `error{not_allowed}`.
   - Then store the hub, its last rx seq (the pair_accept's) and its last tx seq (the pair_request's).
3. **Link:**
   - Both sides treat the link as up after pairing.
   - On every (re)connect the phone sends `hello` to each paired hub. The hub answers with a presence.
   - Each hub sends a presence when the link comes up and then every `PRESENCE_INTERVAL_MS` (15 s). The phone
     must send something at least that often, or the hub drops the link after `PRESENCE_OFFLINE_MS` (45 s).
   - Retry a hello refused with `not_allowed` with backoff (1 s, 2 s, 5 s, then every 30 s; clarifications).
4. **Every inbound `msg`:**
   - `msg.from` must equal the stored key of `env.from` (for a pair_request: `env.pk`).
   - Drop any `seq <= last rx seq` before opening.
   - Then open (v, nonce length, direction byte, seq in nonce) and persist the new seq.
   - Drop a hello or presence whose `ts` is more than 1 h from local time.
5. **Outbound seq:** `max(last tx + 1, now_ms * 1000)`, persisted per hub. Every value stays below 2^53.
6. **`roster_intro` from a hub:** same structural rules as `check_intro_device`, plus the conflict and revoked
   rules. Phones never get phones.

## 5. Offline wake

`wake_phone(id)` runs under the node's gate (like `revoke`, so a revoke either finished before it or waits), and
checks, in this order:

1. The id is a paired phone (a revoked one is not).
2. No `disconnect` cooldown runs for it (`Disconnected`).
3. It has no live link.
4. A relay session is up and not restricted to an adopted join.
5. It was not woken in the last 60 s. The slot is taken before sealing and given back if the relay queue refuses
   the frame.

It then seals the current presence with `next_tx_seq` (seq rules intact) and queues
`send{to: phone address, env}`. The relay stores it in the phone's mailbox and sends the content-free push.

When the phone reconnects, the wake is the first frame it reads: an ordinary presence from that hub. A test checks
that the returning phone's rx seq for the hub reaches the wake's seq, and that the link comes back.

## 6. Test vectors (`crates/peer/tests/vectors/`)

### Shared format

- JSON, LF, object keys sorted; every file has a `description`.
- Bytes are `…Hex` (lowercase hex), standard padded base64 (keys, nonces, boxes), or base64url without padding
  (relay addresses, codes, tokens).
- Keys are referred to by name: `hubA`, `hubB`, `phone`, `relay`.

### `keys.json`

`keys[]`: `{name, id, secretKeyHex, publicKeyHex, publicKey (std b64), relayAddress (b64url)}`.

- Each secret is 32 consecutive bytes. The secrets are test data, never used for anything else.
- `relay` is the relay's per-connection key; its `id` is null.

### `base64.json`

- `samples[]`: `{hex, std, url}`.
- `invalidStd[]` and `invalidUrl[]` are strings a strict decoder must refuse: wrong length, misplaced or extra
  `=`, non-zero trailing bits, the wrong alphabet, whitespace.

### `envelopes.json`

`vectors[]`: `{name, sender, receiver, seq, randomHex (15 bytes), dir, nonceHex (24 bytes), plaintext, body, envelope}`.

- `nonce = random || dir || seq` (u64 big-endian). `dir` is 1 if the sender's pk is below the receiver's
  (bytewise), else 0.
- `plaintext` is the exact UTF-8 JSON that was boxed; `body` is the same value parsed.
- `envelope` is the wire `PeerEnvelope` `{v, from, to, seq, nonce, box, pk?}`.
- There are 12 vectors covering both directions:
  - pairing: pair_request with pk, pair_accept;
  - link: hello, presence both ways, bye;
  - mirror and sync: sync_request, mirror_snapshot;
  - intents: intent (approve, mobile), intent_ack;
  - roster_intro hub→phone and hub→hub.
- Producing: `nacl.box(utf8(plaintext), nonce, receiverPk, senderSk)` must equal `envelope.box`. Opening
  `envelope` must give `plaintext`.

`rejects[]`: `{name, receiver, sender, envelope, error}`, where `error` is the crate's `OpenError` code. There
are 7 cases:

| Case | Expected error |
|---|---|
| reflected to its sender | `bad_nonce` |
| seq changed | `bad_nonce` |
| box tampered | `decrypt_failed` |
| random part of the nonce changed | `decrypt_failed` |
| v = 2 | `bad_version` |
| 23-byte nonce | `bad_encoding` |
| wrong sender key | `decrypt_failed` |

`replay`: `{receiver, sender, lastRxSeq, envelope, error: "replay"}`. The envelope opens fine; only the
`seq <= lastRxSeq` rule refuses it.

### `invites.json`

`valid[]`: `{name, uri, machineId, publicKey (std b64), addr, code, expiresAt, relay (normalized), relayToken, relayKey}`.

- Missing fields are null; `to_uri` of the parsed invite gives `uri` back.
- There are 5: LAN only, LAN plus relay, relay only (the phone's QR), IPv6 plus a relay with port and path, ws on
  loopback.

`invalid[]`: `{name, uri}`. 14 URIs that must be refused:
- scheme, version, upper-case id;
- short or low-order key, 15-byte code, non-numeric expiry;
- duplicate parameter, fragment;
- neither addr nor relay, relay without rt, rk without relay;
- ws relay off loopback, addr without port.

### `relay_auth.json`

`valid[]`: `{name, device, relayUrl, accessKey, connectUrl, host, challenge {type, nonce, relayPk, v}, plaintext, auth {type, box}}`.

- `plaintext = "pawl-relay-auth-v1:" + nonce + ":" + host`.
- `box = nacl.box(utf8(plaintext), nonce, relayPk, deviceSk)`; the relay opens it with its secret from
  `keys.json`.
- There are 2: a phone on `relay.example.com`, and a hub on `127.0.0.1:8787` (wrangler dev).

`refused[]`: `{name, device, host, nonce, relayPk, error: "bad_auth"}`. There are 3: nonce byte 15 = 0, byte
15 = 1, and relayPk equal to the device's own key.

### `names.json` (added with the security review)

- `maxChars`: 64.
- `valid[]` and `invalid[]`: `{name, utf8Hex, note}`. `utf8Hex` is the exact name.
- `introDevices[]`: `{device (IntroDevice), result ("ok" or a check_intro_device code), note}`.

The name rule:
1. Check the **untrimmed** name. No character anywhere may be a control character (Cc: U+0000–001F, U+007F–009F)
   or one of U+061C, U+200B–200F, U+2028–202E, U+2060–2064, U+2066–2069, U+FEFF.
2. Trim Unicode White_Space from both ends.
3. Count Unicode scalar values (`Array.from(s).length` in JS, not `.length`); the count must be 1..=64.

Cases cover:
- astral characters (64 valid, 65 not);
- NBSP and ideographic space, which are trimmed;
- a leading BOM, which must be refused even though JS `trim()` would strip it;
- C0 and C1 controls;
- every forbidden code point, one each.

The `introDevices` results come in code order `bad_id`, `bad_name`, `bad_kind`, `bad_key`, `bad_addr`. They
cover:
- every kind / os combination;
- LAN, CGNAT, ULA, public, DNS and portless addrs;
- ids, names and keys.

Cross-checked with a JS implementation written only from the file's description: 46 of 46 names agree.

### Rust side and regeneration

| Test | What it checks |
|---|---|
| `vectors::vectors_match_this_crate` | Regenerates every file in memory and compares it with the committed one. |
| `vectors::vector_files_hold` | Validates each committed vector independently: open, refuse, parse, auth box opened by the relay. |
| `vectors::fixed_keys_cover_both_directions` | The fixed keys give both direction bytes. |

To regenerate after a deliberate wire change:
`PAWL_REGEN_VECTORS=1 cargo test -p pawl-peer vectors`.

Cross-check done: a throwaway bun script loaded `apps/mobile/node_modules/tweetnacl` and passed 72 of 72 checks:
- key derivation;
- seal and open of all 12 envelopes, plaintext round-trip, direction and seq;
- relay auth box from both sides.

## 7. Other changes

- **Small-order keys are refused:**
  - in `pair_request`, where the request is dropped like one carrying our own key;
  - in `Invite::parse`, which returns `bad_uri`.
- **Envelope refactor:** `nonce_with` / `seal_with_nonce` split out of `seal_plaintext` so the vectors can fix the
  random part. The wire is unchanged.
- **`on_body`, `on_frame` and `drain_lan` take `self: &Arc<Self>`.**
- **Line endings:** `envelope.rs`, `invite.rs`, `limits.rs`, `node.rs`, `node/relay.rs` and `relay.rs` were CRLF in
  the index. They are LF now; that part of the diff is whitespace only (t13: keep crates/peer LF).

## 8. Files

- New:
  - `src/intro.rs`
  - `src/node/phone.rs`
  - `src/vectors.rs` (cfg(test))
  - `src/node/tests/relay/phone.rs`
  - `tests/vectors/*.json` (5)
- Changed:
  - `src/node.rs`
  - `src/node/relay.rs`
  - `src/envelope.rs`
  - `src/invite.rs`
  - `src/lib.rs`
  - `src/roster.rs` (test literal)
  - `src/node/tests.rs` (literal)
  - `src/node/tests/relay.rs` (`mod phone;`)
- `crates/hub` is not touched; it needs the items in section 2.

## 9. Tests added (93 → 105, +1 ignored; the last 3 with the security review)

| Area | Tests |
|---|---|
| `intro` | `low_order_points_really_have_a_guessable_shared_secret`, `public_key_rules`, `intro_device_rules` |
| `vectors` | `vectors_match_this_crate`, `vector_files_hold`, `fixed_keys_cover_both_directions` |
| `node::tests::relay::phone` | `a_phone_pairs_through_a_relay_only_invite_and_talks_over_the_relay` |
| | `introduced_hubs_are_reported_and_once_stored_they_link` |
| | `roster_intros_are_checked_and_only_a_paired_hub_may_send_one` |
| `invite` | `parse_rejects_bad` gained a case for a key of small order |
| `node::tests::relay::phone` (review) | `the_invite_decides_whether_a_joiner_is_a_phone_or_a_hub`, `no_wake_for_a_disconnected_or_revoked_phone` |
| `node::tests` (review) | `machine_names_with_control_or_invisible_characters_are_refused` |

What the three `phone` tests cover:

- **`a_phone_pairs_through_a_relay_only_invite_and_talks_over_the_relay`:** relay-only invite rules, phone kind and
  os, bodies both ways, never dialed, the wake and its rate limit, the phone reading the wake, `RelayOff`.
- **`introduced_hubs_are_reported_and_once_stored_they_link`:**
  - the event carries the intro;
  - nothing is stored, and nobody is let in, until the hub stores the devices;
  - after storing, the allow lists take the new devices, and they link over the LAN;
  - `introduced_by` is set, and the intro never shows up as a Body.
- **`roster_intros_are_checked_and_only_a_paired_hub_may_send_one`:** every reason code, 32 devices accepted, 33
  refused by the sender and dropped whole by the receiver, and an intro from a phone ignored.

## 10. Known gaps

- **Conflict checks run when the hub stores the devices, not inside the node** (t13's split). Two intros naming one
  id with different keys are both reported. The hub store must refuse the second at write time, never replace the
  key.
- **The phone's link drop is noticed after `PRESENCE_OFFLINE_MS` (45 s) of silence** unless the phone says bye.
  Presences sent in that window queue in its mailbox; the relay pushes at most once a minute.
- **The wake carries the regular presence.** It holds agents, repos and pets, so it is as big as any presence
  frame, well under the relay frame limit.
- **A paired device with a mixed-order key** (prime-order point plus small-order point, which the blocklist cannot
  see) could learn at most 3 bits of our scalar mod ℓ through crypto_box's reduction. Pairing needs an invite code,
  and those 3 bits leave the key at well over 250 bits of strength.
- **Step 6 gaps still apply:**
  - frames in flight across a path switch can be lost;
  - a relay blip drops relay links;
  - with more than 64 paired devices, the least recently seen are left out of the allow list;
  - `get_revoked` is still the hub store's to implement.

## 11. Commands run

- `cargo test -p pawl-peer` ×5: 102 passed, 1 ignored, each run.
- Stress: the test binary 12 in parallel × 3 rounds: 36/36. A first round failed 2 of 36 on mock-relay timing:
  - a frame handed to a socket the relay had not yet seen close was lost;
  - frames still in flight were logged after the snapshot.
  
  Fixes: `a_phone_pairs…` and the step-6 `revoked_while_off_the_relay…` now wait for the relay to see the socket close (and for the relay log to go quiet) before acting. No product change.
- `cargo clippy -p pawl-peer --all-targets -- -D warnings`: clean.
- `PAWL_REGEN_VECTORS=1 cargo test -p pawl-peer vectors`, then twice without the variable: 3 passed each.
- `bun <scratchpad>/check_vectors.cjs <apps/mobile tweetnacl> <vectors dir>`: tweetnacl agrees, 72 checks.
- After the security review: `cargo test -p pawl-peer` ×5: 105 passed, 1 ignored; stress 12×3 = 36/36 (after widening the step-6 relay ping test's 800 ms window to 2 s); clippy clean; `cargo check -p pawl-hub --tests` passes; JS names check 46/46; tweetnacl still 72/72.

## 12. Security review fixes (t13)

1. **The invite decides the kind, not the joiner (important).**
   - What was wrong: the kind came only from the joiner's claimed os. Anyone holding a "Pair a phone" QR code could
     claim linux, be stored as a hub, then send roster_intro, get dialed and have a LAN addr stored.
   - The fix: `LiveInvite` now records its kind. `create_phone_invite` makes phone
     invites; `create_invite` makes hub invites. `consume_invite(code, os)` returns the invite's addr and kind, and
     the device is stored with that kind.
   - A phone invite takes only an ios / android joiner, and a hub invite refuses one. A mismatch is answered with
     `pair_reject{invalid_invite}`, counts as a limiter failure, and leaves the invite live (no denial of service
     on the real phone).
   - Test: `the_invite_decides_whether_a_joiner_is_a_phone_or_a_hub`.
2. **`bad_kind` in `check_intro_device`.**
   - A phone needs os ios / android; a hub must not have one (its os may be unknown).
   - Unit cases in `intro_device_rules`; node cases in `roster_intros_are_checked_and_only_a_paired_hub_may_send_one`.
3. **A second layer in the node.** On top of the hub's checks, the node marks an intro device invalid when it names:
   - our id or key: `own_device`;
   - the sender's id or key: `sender_device`;
   - an id or key already named earlier in the same intro: `duplicate`.

   Tested in `roster_intros_are_checked_…`, which now uses 32 distinct devices for the size limit.
4. **Names.** `device_name_ok` (exported) adds the invisible-format characters to the old rule (1..=64 chars
   trimmed, no control characters).
   - The set: t13's U+202A–202E, U+2066–2069, U+200B–200F and U+FEFF, plus U+061C (Arabic letter mark, a bidi
     mark), U+2028–2029 (line and paragraph separators) and U+2060–2064 (word joiner and invisible operators).
   - Specified exactly in `names.json`.

   Where a bad name is now refused:

   | Frame | What happens |
   |---|---|
   | `pair_request` | `pair_reject{bad_request}` |
   | `pair_accept` | The joiner refuses: `invite_rejected` |
   | hello opening a link (LAN, dial, relay) | Dropped |
   | hello on a live link | Ignored (no roster update) |
   | presence | Ignored (no update, no event) |
   | roster_intro | `bad_name` |

   - `set_machine` warns when our own name breaks the rule.
   - Note for sde-hub: `machine::validate_name` should use `device_name_ok`. A machine renamed to a name with a
     zero-width joiner (some emoji sequences) would otherwise be refused by peers.
   - Test: `machine_names_with_control_or_invisible_characters_are_refused`.
5. **`wake_phone` respects the revoke gate and `disconnect`.**
   - It holds the gate, so a wake and a revoke cannot interleave.
   - A revoked phone gives `NotPhone`; one under a disconnect cooldown gives the new `WakeError::Disconnected`
     ("disconnected"). The hub's match has a catch-all arm, so it compiles.
   - Test: `no_wake_for_a_disconnected_or_revoked_phone`.

The five earlier vector files are unchanged (regenerating gives the same bytes) and still valid. `names.json` is
new (t13).
`cargo check -p pawl-hub --tests` passes.
