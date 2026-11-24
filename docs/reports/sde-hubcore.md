# sde-hubcore report: Step 2 (pairing + LAN presence) in `krypto-hub`

Branch: `step-2-pairing-presence`. Nothing committed. I only edited `crates/hub/`, plus this report (which replaces the step 1 report).

Invite codes and `device.key` contents never appear in this report. Every `code=` value below is redacted. Public keys (`pk=`) are public.

## Files changed

| File | Change |
|---|---|
| `crates/hub/Cargo.toml` | Adds `krypto-peer = { workspace = true }`. No other dependency changes. |
| `crates/hub/src/store.rs` | **Schema v3:** a `devices` table (id, kind, name, os, hub_version, public_key, addr, paired_at, last_seen, revoked_at, last_rx_seq, last_tx_seq) and a `pairing_audit` table. `impl krypto_peer::Roster for Store`. Typed helpers: `roster`, `roster_entry`, `upsert_device`, `update_device_seen`, `revoke_device`, `insert_pairing_audit`, `pairing_audit`. Also `PairingAuditKind` (`paired`, `pair_rejected`, `revoked`, `revoked_by_peer`) and `PairingAuditEntry`. |
| `crates/hub/src/core/peers.rs` (new) | The hub side of pairing: roster cache, presence snapshot, `create_invite` / `join_peer` / `revoke_device`, and turning `PeerEvent`s into `ServerMsg`s. Pairing audit and notifications. |
| `crates/hub/src/core.rs` | The `PeerNode` is created in `Hub::build` (`device.key` + store as roster). The daemon calls `start_peer_links` (listener if configured, then `connect_known`). Shutdown sends `bye{shutdown}`. `machines()` = self, then paired hubs; `BrainContext.machines` uses the same list. Welcome gains `devices` and `peer`. Dispatches the 3 new ClientMsgs. Presence is refreshed on rename, agents, capabilities, pet creation, pet updates and pet flushes. Pet names seen in a peer's presence count as taken for new default pets and for `update_pet` (`name_taken`). |
| `crates/hub/src/config.rs` | `Paths.device_key` (`data_dir/device.key`). A `[peer] listen = "ip[:port]"` section (default port 47801; checked when config.toml is parsed; host names refused). `parse_peer_listen`. The config template documents it. |
| `crates/hub/src/cli.rs` | `serve --peer-listen <IP[:PORT]>` (overrides config). `peer invite [--ttl] [--addr]`, `peer join <uri>`, `peer revoke <machineId\|name>`. `machines [--json]`. Error codes become readable text. Names a peer sent have control characters replaced before printing. |
| `crates/hub/src/core/tests.rs`, unit tests in `store`, `config`, `cli` | See "Tests". |

## Behaviour

- **Startup.**
  - `DeviceKeys::load_or_create(data_dir/device.key)`, then `PeerNode::new(keys, this machine, Arc<Store> as Roster, events)`.
  - The roster cache is loaded with every paired hub offline, and the first presence snapshot is handed over.
  - The daemon (`serve`) then listens, but only if `--peer-listen` or `[peer] listen` is set. Binding to 0.0.0.0 or :: logs a warning; a bind failure stops `serve`. It then calls `connect_known()`.
  - The in-process `krypto start` creates the node but never listens or dials.
  - The listener is krypto-peer's own server on its own port. The local API router (`/health`, `/ws`, `/hooks`) has no peer route, and peer frames only ever reach the hub as `PeerEvent`s. So peer frames never reach `ClientMsg` handling (smoke check b).
- **Presence.**
  - The snapshot is `{machine, agents, own pets, repos: [], ts}`.
  - It is handed over only when something peers can see changed: the comparison ignores `ts` and `machine.lastSeen`. That happens on a rename, an agent refresh, a capabilities change, a new pet or `update_pet`, and after each pet flush, so mood and XP changes reach peers at most every 2 s.
  - Shutdown: `peer.shutdown()` (sends `bye{shutdown}`), then the agents are shut down.
- **PeerEvents → clients.** After each event the hub re-reads the roster and compares it with what it last sent:
  - `devices` is sent when the roster changed. A change in lastSeen alone doesn't count.
  - `machine` is sent when a hub is added, renamed, changes OS or hub version, or goes online/offline. Heartbeats alone send nothing.
  - `machine_removed` is sent when a hub leaves the roster.
  - The online state comes from the event itself (`Paired`/`LinkUp`/`Presence` → online, `LinkDown` → offline). For every other hub it comes from `PeerNode::online()`.
  - `Paired` → Success notification "Paired with X". `bye{revoked}` from a peer → Info notification "X unpaired this machine".
  - Pairing audit: `paired` (detail: "accepted our invite" / "joined their invite"), `pair_rejected` (remote addr + reason), `revoked` (actor = the client, e.g. `cli#3`), `revoked_by_peer`. Each revoke is audited once.
- **Welcome.**
  - `machines` = this machine, then paired non-revoked hubs sorted by name. `online` comes from `PeerNode::online()`, and `lastSeen` is the latest authenticated frame.
  - `devices` = the roster.
  - `peer` = `{listening, publicKey}`.
- **ClientMsgs** (ack errors are the bare codes):
  - `create_invite{ttlSecs?, addr?}`: ttl defaults to 600 and must be 60..=3600, else `invalid`. A blank `addr` counts as none. → `{uri, expiresAt}`. The URI is never logged.
  - `join_peer{uri}` → `{device, machine}` (machine `online: true`). Errors come verbatim from krypto-peer: `bad_uri`, `self_invite`, `unreachable`, `invite_rejected`, `timeout`.
  - `revoke_device{deviceId}`: `not_found` unless the device is in the roster. Otherwise `PeerNode::revoke` (bye{revoked}, close, stop dialing), then the store marks it revoked, it is audited, and clients get `machine_removed` + `devices`.
- **Roster semantics** (per sde-peer's trait notes):
  - `get`/`list` return non-revoked entries only.
  - `upsert` replaces key, name, addr and machine and clears `revoked_at`. On a key change it also resets `last_rx_seq`, because old frames can no longer be opened.
  - `update_seen` keeps the stored addr when `addr` is None, and is a no-op for unknown or revoked ids.
  - Seq counters are stored exactly as given, bit-for-bit as i64 (so values above `i64::MAX` survive). Unknown ids read 0. A read error on `last_rx_seq` answers `u64::MAX`, which refuses frames instead of risking a replay.
- **CLI.**
  - `peer invite` prints the URI, the expiry (local time + minutes) and the exact `krypto peer join "<uri>"` line.
  - `peer revoke` accepts an id or a unique name (trimmed, case-insensitive). An ambiguous name lists the matching ids.
  - invite/join/revoke need a running hub; without one: "no hub is running for this data dir…".
  - `machines` works without a hub: it reads `machine.toml` + `hub.db` and shows peers offline. `--json` prints `{machines, devices}`.

## Decision for t13 to confirm

- **Pairing audit table.** Pairing entries go to their own `pairing_audit` table rather than into `audit_log`. `audit_log`'s NOT NULL columns are all about permissions (request_id, session_id, agent_id, decided_by, outcome), so pairing rows would need dummy values there. Nothing reads either table outside tests yet. If you want a single table, I can merge them with a `kind` column.

## Bug found by the two-hub test (fixed)

- When a peer sent `bye{revoked}`, krypto-peer marked the roster revoked and reported `LinkDown`, then `Revoked`. The hub's `LinkDown` sync re-read the roster and dropped the entry, so the later `Revoked` event found nothing and `revoked_by_peer` was never audited.
- Fix: entries that leave the roster are kept in a `departed` map until their `Revoked` event audits them. Revoking from a client takes the entry directly, so the event krypto-peer sends afterwards is a no-op.

## Tests

- `cargo test -p krypto-hub`: **64 passed**, 0 failed. 6 runs across the two krypto-peer builds, all green, about 0.35 s each.
- `cargo clippy -p krypto-hub --all-targets -- -D warnings`: clean.
- New tests:
  - store:
    - `roster_follows_the_peer_contract`: unknown ids read 0; set on an unknown row is a no-op; counters above `i64::MAX` and counters going down; `update_seen` with/without addr and for unknown/revoked ids; re-pair clears revoked and resets rx on a key change.
    - `pairing_audit_roundtrip`
    - `v3_migration_adds_the_roster` (v2 db → v3, data kept)
  - config: `peer_listen_addresses` (ip, ip:port, IPv6 with/without brackets, 0.0.0.0; rejects host names and bad ports; `[peer] listen` errors carry context).
  - cli: `cli_parses` (serve/peer/machines), `revoke_targets_resolve_by_id_or_unique_name`, `pairing_error_codes_read_well`, `machines_command_without_a_hub`.
  - core (real `PeerNode`s on 127.0.0.1:0, fake ACP host):
    - `pairing_requests_answer_bare_codes`: Welcome peer status; `peer_listener_off`; ttl 0/59/3601 `invalid`; `bad_uri`; `self_invite`; forged code → `invite_rejected` + `pair_rejected` audit; revoke unknown → `not_found`.
    - `two_hubs_pair_see_each_other_and_revoke`: join ack shape; `devices` + `machine` online on both sides; Welcome machines/devices; both `paired` audits; the invite is single use; a rename travels by presence; a peer's machine id is `not_owner`; mutual revoke with `machine_removed` on both sides, `revoked` (client actor) and `revoked_by_peer` audits each written once, empty rosters, and a second revoke → `not_found`.
    - `shutdown_says_bye_and_restart_reconnects`: `bye{shutdown}` → offline at once and still paired; a restarted hub reconnects through `connect_known`.
    - `presence_pets_are_taken_names`: a peer's pets arrive by presence; the local default pet avoids their names; `update_pet` → `name_taken`; remote pets are not shown before step 3.
  - `hello_returns_welcome_shape` now also checks `devices: []`, `peer.listening: null` and a 44-char public key.

## Two-hub smoke on this PC (binary `target/debug/krypto.exe`)

Scratch data dirs: `%TEMP%\krypto-smoke-step2\A` and `\B`, never the real data dir. Each config.toml is `offline = true` and `daily_report_time = "off"`. No `KRYPTO_HUB_*` env vars were set. `<tmpA>`/`<tmpB>` stand for those dirs. Both hubs run on one host, so I renamed them "Smoke A" and "Smoke B" (both had defaulted to the host name).

### Start

```
krypto --data-dir <tmpA> --port 47910 serve --peer-listen 127.0.0.1:47911
krypto --data-dir <tmpB> --port 47920 serve

LocalAddress LocalPort OwningProcess
127.0.0.1        47920         42836     (B: local API only)
127.0.0.1        47911         47908     (A: peer listener)
127.0.0.1        47910         47908     (A: local API)
```

### Invite, then check (e): wrong code, then the right code on the same invite

```
$ krypto --data-dir <tmpB> --port 47920 peer invite        (B has no listener)
error: this hub's peer listener is off, so it cannot invite. Restart it with `krypto serve --peer-listen <LAN-IP>` or set [peer] listen = "<LAN-IP>:47801" in config.toml (peer_listener_off)

$ krypto --data-dir <tmpA> --port 47910 peer invite
Invite to pair with SURI-DESKTOP (single use, expires at 23:03:30 - in 10 min):

  krypto://peer?v=1&mid=zaggeox1mcxa&pk=NeOwi_bUaawgp8IwEegJHhDHvRA5b6t7DMPzHVZAuno&addr=127.0.0.1:47911&code=<redacted>&exp=1795503810093

On the other machine, run:

  krypto peer join "krypto://peer?v=1&mid=zaggeox1mcxa&pk=NeOwi_bUaawgp8IwEegJHhDHvRA5b6t7DMPzHVZAuno&addr=127.0.0.1:47911&code=<redacted>&exp=1795503810093"

(renamed: krypto ... machine --name "Smoke A" / "Smoke B")

$ krypto --data-dir <tmpB> --port 47920 peer join "<uri with a wrong code>"
error: the inviting hub rejected the invite: it is wrong, expired or already used; create a new one (invite_rejected)

$ krypto --data-dir <tmpB> --port 47920 peer join "<uri>"
Paired with Smoke A (zaggeox1mcxa, windows) at 127.0.0.1:47911

$ krypto --data-dir <tmpA> --port 47910 machines
NAME     ID            OS       STATUS        LAST SEEN  PEER ADDR
Smoke A  zaggeox1mcxa  windows  this machine  now        127.0.0.1:47911
Smoke B  ilr5hql707d0  windows  online        now        -

$ krypto --data-dir <tmpB> --port 47920 machines
NAME     ID            OS       STATUS        LAST SEEN  PEER ADDR
Smoke B  ilr5hql707d0  windows  this machine  now        -
Smoke A  zaggeox1mcxa  windows  online        now        127.0.0.1:47911

$ krypto --data-dir <tmpB> --port 47920 peer join "<same uri again>"
error: the inviting hub rejected the invite: it is wrong, expired or already used; create a new one (invite_rejected)
```

Rename while paired (travels by presence):

```
$ krypto --data-dir <tmpB> --port 47920 machine --name "Smoke B laptop"
$ krypto --data-dir <tmpA> --port 47910 machines
Smoke B laptop  ilr5hql707d0  windows  online        now        -
```

### (a) The peer port serves only `/peer`, and an `Origin` header gets 403

```
GET /        -> 404
GET /ws      -> 404
GET /health  -> 404
GET /peer    -> 400   (not a WebSocket upgrade)
GET /peer (WebSocket upgrade, Origin: http://evil.example) -> 403
GET /peer (Origin: http://evil.example, no upgrade)        -> 403
```

### (b) The local API can't be reached through the peer port

```
POST 47911/hooks/claude -> 404
GET  47911/health       -> 404
WS   47911/ws upgrade   -> 404
GET  47910/health       -> 200 {"ok":true,"version":"0.1.0"}
```

### Offline detection

- **B killed** (`taskkill /F /PID 42836`, no bye): Windows closes the socket, so A showed B offline on its first poll (0 s):
  ```
  Smoke B laptop  ilr5hql707d0  windows  offline       1s ago     -
  ```
- **45 s silence timeout.**
  - B was restarted and reconnected by itself, then suspended (`NtSuspendProcess`): its socket stays open but no frames arrive.
  - A marked it offline after 33 s, which is 45 s after B's last frame (`LAST SEEN 45s ago`).
  - After `NtResumeProcess`, B redialed and showed online again within 4 s.
  ```
  suspended hub B (pid 35956) at 22:54:38: its socket stays open but it sends nothing
  A shows B offline after 33 s:
  Smoke B laptop  ilr5hql707d0  windows  offline       45s ago    -
  resumed hub B
  Smoke B laptop  ilr5hql707d0  windows  online        now        -
  ```

### (c) Restart hub A, and B reconnects by itself

```
taskkill /F /PID 47908  (hub A)
B after A died:
Smoke A         zaggeox1mcxa  windows  offline       2s ago     127.0.0.1:47911
restarted hub A (pid 37828) at 22:55:29
B shows A online again after 1 s (B redialed on its own):
Smoke A         zaggeox1mcxa  windows  online        now        127.0.0.1:47911
```

### (d) Revoke on A, and B shows A removed too

```
$ krypto --data-dir <tmpA> --port 47910 machines --json      (publicKey shortened here)
{"machines": [{"id": "zaggeox1mcxa", "name": "Smoke A", "os": "windows", "lastSeen": 1795503343843, "online": true, "hubVersion": "0.1.0"},
              {"id": "ilr5hql707d0", "name": "Smoke B laptop", "os": "windows", "lastSeen": 1795503330090, "online": true, "hubVersion": "0.1.0"}],
 "devices": [{"id": "ilr5hql707d0", "kind": "hub", "name": "Smoke B laptop", "publicKey": "jWxZCRRm...", "pairedAt": 1795503219326, "lastSeen": 1795503330090}]}

$ krypto --data-dir <tmpA> --port 47910 peer revoke "smoke b laptop"
Revoked Smoke B laptop (ilr5hql707d0): it can no longer connect. Pair again with a new invite.

$ krypto --data-dir <tmpA> --port 47910 machines
Smoke A  zaggeox1mcxa  windows  this machine  now        127.0.0.1:47911
No paired machines. ...

$ krypto --data-dir <tmpB> --port 47920 machines
Smoke B laptop  ilr5hql707d0  windows  this machine  now        -
No paired machines. ...

$ krypto --data-dir <tmpA> --port 47910 peer revoke ilr5hql707d0      (again)
error: no paired machine with id or name "ilr5hql707d0" (see `krypto machines`)
```

Pairing audit, read-only from each `hub.db` (schema 3):

```
hub A: ('pair_rejected', None, None, '127.0.0.1:54164', 'peer', 'invalid_invite')
       ('paired', 'ilr5hql707d0', 'Smoke B', None, 'peer', 'accepted our invite')
       ('revoked', 'ilr5hql707d0', 'Smoke B laptop', None, 'cli#3', None)
hub B: ('paired', 'zaggeox1mcxa', 'Smoke A', '127.0.0.1:47911', 'peer', 'joined their invite')
       ('revoked_by_peer', 'zaggeox1mcxa', 'Smoke A', '127.0.0.1:47911', 'peer', None)
```

### Re-pair after revoke, then `machines` without a hub

```
$ krypto --data-dir <tmpA> --port 47910 peer invite --ttl 120
Invite to pair with Smoke A (single use, expires at 22:58:13 - in 2 min): ... code=<redacted> ...
$ krypto --data-dir <tmpB> --port 47920 peer join "<uri2>"
Paired with Smoke A (zaggeox1mcxa, windows) at 127.0.0.1:47911
$ krypto --data-dir <tmpA> --port 47910 machines
Smoke B laptop  ilr5hql707d0  windows  online        now        -

(both hubs stopped)
$ krypto --data-dir <tmpA> --port 47910 machines
NAME            ID            OS       STATUS                          LAST SEEN  PEER ADDR
Smoke A         zaggeox1mcxa  windows  this machine (hub not running)  -          -
Smoke B laptop  ilr5hql707d0  windows  offline                         58s ago    -
$ krypto --data-dir <tmpB> --port 47920 machines
Smoke B laptop  ilr5hql707d0  windows  this machine (hub not running)  now        -     <- printed before the "-" fix below
Smoke A         zaggeox1mcxa  windows  offline                         15s ago    127.0.0.1:47911
$ krypto --data-dir <tmpB> --port 47920 peer invite
error: no hub is running for this data dir; start it first (`krypto serve`, add --peer-listen <LAN-IP> to invite)
```

After that run I changed `machines` to show "-" instead of "now" for this machine's last seen when no hub is running, then rebuilt and reran tests and clippy. The hub-A listing above is from the rebuilt binary.

### Secrets check

- I grepped `A/hub.log`, `B/hub.log` and every stdout/stderr capture for the first invite's code and for both `device.key` values: 0 matches in every file.
- `device.key` is 65 bytes (64 hex chars + newline).
- All hubs were stopped at the end; ports 47910, 47911 and 47920 are free.

## Known gaps

- **Not tested on a real LAN.** The smoke ran on one PC over 127.0.0.1. Two PCs need `--peer-listen <LAN-IP>` and a firewall rule for the peer port.
- **The hub can't stop the listener at runtime.** `ServerMsg::PeerStatus` is only sent when the listener starts.
- **Peer-sent names reach clients unvalidated.** The hub trusts krypto-peer for peer `Machine` fields; the CLI only replaces control characters when printing.
- **Not in step 2:** remote pets and sessions are not shown (step 3), `repos` is empty (step 5), and phones and group introduction come in step 7.
