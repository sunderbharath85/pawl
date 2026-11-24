# sde-hub report: step 7 (phone), protocol v8

Branch `step-7-phone`, nothing committed. Owned paths only: `crates/hub/`. No new dependencies. All files LF.

## Gate

| Command | Result |
|---|---|
| `cargo test -p krypto-hub` | **177 passed** (was 158) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| Live smoke: local relay (`wrangler dev` on 127.0.0.1:8797), two scratch hubs, fe-mobile's `headless-phone.ts` | all PASS (below) |

## Security review fixes (t13)

| # | Fix | Tests |
|---|---|---|
| 1 | `on_roster_intro` is rate-limited per sender. A token bucket gives a burst of 3, then 1 per 10 s (`take_intro`). Drops are audited at most once per 60 s, with a count of the quiet ones, and never notify. The bucket is checked before anything else, including the "not a paired hub" path. | `intros_are_rate_limited_per_sender_and_drops_noted_once_a_minute` (pure); flood part of `received_intros_add_new_devices_once_and_never_replace` |
| 1 | Roster caps for intros: no Add past **64 stored devices** (paired + revoked rows), nor past **32 added by one introducer** (paired + revoked). Refusals go into the intro's single report. | `intros_never_grow_the_roster_past_its_caps` |
| 1 | Coalesced reporting. Each added device keeps its own `introduced` row; everything not added is **one audit row + one notification per intro**. The row is `intro_conflict` if any conflict, else `intro_ignored`; its detail is grouped as "conflict: ...; malformed: ...; full: ...; off: ..." with at most 8 named per group, then "and N more". It names a device only when exactly one entry was skipped. So at most 2 notifications per intro, and intros are rate-limited. The >32 refusal is audit-only. | `not_added_outcomes_make_one_report` (pure); `received_intros_...` |
| 2 | `revoke_device` ack result now lists the devices the revoked one introduced that are **still paired** (sorted by name). They are not revoked automatically. `krypto peer revoke` prints "Laptop added Pixel (phone), Studio here; they stay paired. Revoke them too with ...". | `revoking_an_introducer_lists_what_it_added`; CLI `a_revoke_names_the_devices_it_had_added` |
| 3 | `create_invite{forPhone}` now calls the node's `create_phone_invite(ttl)` (purpose-bound: only an ios / android joiner, stored as kind phone). The hub never infers a kind. `update_device_seen` never writes `kind` (hellos may change os, never kind). | `hellos_never_change_a_devices_kind` (store); real-link test: a phone (os ios) joining a **hub** invite gets `invite_rejected`, and a hello from a paired phone keeps kind phone |

**Names (t13 follow-up):**
- `krypto_peer::device_name_ok` now applies to every name the hub accepts from the UI or CLI that reaches peers:
  - the machine name: `machine::validate_name`, used by `update_machine`, `krypto machine --name` and machine.toml loading,
    on top of the 1-40 char limit;
  - pet names: `validate_pet_name`;
  - custom agent names: `invalid:customAgents[i].name`.
- Bidi controls and zero-width / invisible formatting characters are refused with a clear reason ("... must not contain
  invisible characters (bidi controls, zero-width ...)"). The ack code stays `invalid`.
- The OS host name used as the default machine name has those characters dropped.
- `WakeError::Disconnected` (our flood cooldown) is logged at debug level. On `Backpressure` (nothing queued) the hub
  forgets that wake, so the next request retries.
- Tests:
  - `machine::names_are_validated` (bidi / zero-width / BOM / word-joiner refused; visible Unicode such as emoji, umlauts
    and CJK accepted and accepted by the peers' rule);
  - `host_names_are_cleaned`;
  - `pet_names_follow_the_device_name_rule`;
  - a new row in the settings validation table.
- Gate: hub **184 passed**, workspace clippy clean.

**`revoke_device` ack result** (for fe-settings):

```json
{ "introduced": [ { "id": "pppppp000002", "name": "Pixel", "kind": "phone" },
                  { "id": "ssssss000001", "name": "Studio", "kind": "hub" } ] }
```

`introduced` is always present: `[]` when the device introduced nothing that is still paired, which matches t13's TS
shape `introduced?: [...]`. Entries are sorted by name and are devices still paired here whose `introducedBy` = the
revoked id. Revoke each with `revoke_device` to remove them.

The real-link test changed because of the node's invite purpose binding: a phone can no longer pair through a LAN hub
invite. The test now seeds both rosters, and the phone's LAN hello brings the link up. It still checks the snapshot,
`sync_done`, live updates, the owed intro and `intent_ack` as mobile.

Gate after the fixes: `cargo test -p krypto-hub` **183 passed**; `cargo clippy --workspace --all-targets -- -D warnings`
clean.

## What was built

### 1. Phones in the roster
- Phones are roster devices with `kind: phone`. The kind always comes from the node's `Paired` / `RosterIntro` result; the
  hub never infers it.
- They are listed in `devices` with online / offline state and `path` (from `LinkUp` / `LinkPath` / `LinkDown`).
- They never appear in `machines` or the `machine` / `machine_removed` messages, so they are never the active machine.
- A phone's `presence` is only a keepalive. Its repos, agents, pets and activity are never stored or used (no pickers, no
  brain context, no active machine).
- `Device.os` comes from the stored os. `Device.introducedBy` is a new store column.
- `krypto machines` prints a separate PHONE table: os, status, last seen, path, and "here" or "introduced by Mac mini".

### 2. `create_invite{forPhone: true}`
- Calls the node's `create_phone_invite(ttl)` (was `create_relay_invite`): a relay-only, purpose-bound invite (no
  `addr`), even when the LAN listener is on.
- Same TTL range (60-3600 s, default 600) and single use as other invites.
- Errors:
  - `relay_off` while the relay is not connected;
  - `invalid` when an `addr` is given with forPhone, or for a bad TTL.
- `krypto peer invite --phone` prints the URI plus a short how-to. `--phone` conflicts with `--addr`.
  - `relay_off` gets readable CLI text.
  - No ASCII QR: there is no QR crate in the workspace, and adding one would be a new dependency.

### 3. Group roster (`crates/hub/src/core/peers/intro.rs`)

**Fan-out.** On every `Paired`, `intro_group` picks the group:
- a new hub's group is every other hub and every phone;
- a new phone's group is every other hub (phones never meet phones).

The intros owed go into a new table `roster_intro_pending(recipient_id, device_id)`. That covers each group member being
owed the new device, and the new device being owed the group. They are sent right away to linked devices and on every
`LinkUp` otherwise, so offline devices get them on their next hello. Phones hello first, so a phone gets its intros as
soon as it connects.
- Intros go out with `send_roster_intro` in chunks of at most 32.
- They are built with `intro_device`: a hub's addr only if it passes `peer_addr_ok`, never an addr for a phone, and the
  os.
- An intro is cleared when it is queued on a link.
- Revoking a device deletes what it was owed and what others were owed about it.

**Receiving** (`PeerEvent::RosterIntro{from, devices, invalid}`). The node already checked that the sender is a linked
paired hub, the count and the structure. The hub then:
- re-checks that the sender is a paired hub (anything else is audited `intro_ignored`);
- runs `judge_intro` against every stored row, revoked rows included:

| Case | Outcome |
|---|---|
| Listed twice in the intro | Invalid |
| This machine's id or key | Invalid |
| The sender's id or key | Invalid |
| Known id with the same key and kind | Known (nothing done, nothing audited) |
| Known id with another key | Conflict |
| Known id as another kind | Conflict |
| Known key under another id | Conflict |
| Revoked id or key | Conflict |
| Anything else | Add (canonical key) |

- `invalid` entries from the node are audited as `intro_ignored` with the node's code.
- With `[peer] accept_roster_intros = false`, new devices are audited `intro_ignored` ("accept_roster_intros is off") and
  shown, never added.
- Added devices are stored with `introduced_by = sender` and audited `introduced` (detail "introduced by Mac mini (id)").
  Then the hub calls `roster_changed()`, which updates the relay allow list and dial loops.
- Pairing audit kinds `introduced` / `intro_conflict` / `intro_ignored` hold the id, name and reason, never keys.
- Clients get a local `HubNotification` for each outcome:

| Outcome | Example title |
|---|---|
| Added (success) | "Laptop added Pixel and Studio" |
| Setting off (info) | "Laptop introduced Pixel" |
| Conflict (warning) | "... : not added" |
| Malformed entries (warning) | "Laptop sent an intro that was not used" |

  A `devices` broadcast always follows (it comes from the roster sync when something was added).

**Store.** `add_introduced_device` is one `INSERT ... SELECT ... WHERE NOT EXISTS (id = ? OR public_key = ?)`. It is
atomic in SQLite, and it never replaces a key or id and never re-adds a revoked one, even if a pairing races the intro.
This is sde-peer's report item (a). A direct (re-)pairing `upsert_device` clears `introduced_by`.

### 4. Setting `peer.acceptRosterIntros`
- `[peer] accept_roster_intros` (default true) appears in `HubSettings.peer.acceptRosterIntros` and in the
  `update_settings` patch.
- It applies at once (no restart), is written with toml_edit, is audited like any setting (key `peer.acceptRosterIntros`),
  and is shown in `krypto settings` and the config template.

### 5. Mirror and intents for phone links (`core/mirror.rs`)
- `on_peer_body` accepts bodies from any paired device. Phone bodies are limited to:
  - `sync_request`, answered like a hub's: this hub's own sessions, pets and pending requests, then `sync_done`;
  - `intent`, always run as client `mobile`, whatever the phone claims.
- Everything else from a phone is dropped, including mirror bodies, so a phone never feeds the mirror. The hub never
  sends a phone a `sync_request`.
- Live mirror updates go to every linked paired device (hubs and phones), still only this hub's own items.
- The step-4 owner rules are unchanged:
  - approve and start need `confirmed`;
  - approve is allow_once only;
  - only known folders;
  - remote-driven sessions never auto-approve;
  - the per-device rate limit applies.
- Audit and messages name the phone:
  - audit actor `mobile@<phone name>`, origin `{client: mobile, name: <phone name>}`;
  - intent_log `client = mobile`;
  - `permission_resolved.byMachineId = <phone id>`, so clients can show the name from `devices`;
  - notification "Approved from <phone name>".
- `krypto start` now prints "answered allow from iPhone" instead of "by you" when `byMachineId` is set.

### 6. Offline wake
- `on_permission_request` (a new local request that asks a human) calls `wake_offline_phones()`.
- For each paired phone without a live link, it calls `node.wake_phone(id)`, at most once per phone per 60 s on the hub
  side (`take_wake`).
- The node keeps its own once-a-minute limit too. `Linked`, `RelayOff` and `RateLimited` are logged at debug level, any
  other error at warn.

### t13 / sde-peer follow-ups
- **(a)** The store refuses a second key for an id, a second id for a key, and any revoked id or key in one statement.
  Tests:
  - `store::introduced_devices_never_replace_anything`;
  - `core::tests::phones::received_intros_add_new_devices_once_and_never_replace`.
- **(b)** `Roster::get_revoked` has been in the hub store since step 6: `revoked_roster_entry`, test
  `store::revoked_devices_keep_their_key_for_a_bye`.
- **Invite purpose:** the hub takes the device kind only from the node (`Paired.device.kind`). When sde-peer renames or
  changes the phone-invite constructor, only `Hub::create_invite` (`core/peers.rs`) needs to change.

## Tests added (19)
- **Store:**
  - `v8_migration_keeps_the_roster` (v7 db -> v8, counters kept);
  - `introduced_devices_never_replace_anything`;
  - `owed_intros_are_kept_per_recipient_until_sent_or_revoked`.
- **Pure rules (`core/peers/intro/tests.rs`):**
  - `intro_groups_follow_the_fan_out_rules`;
  - `intro_devices_carry_only_what_receivers_take`;
  - `intros_are_judged_against_what_this_hub_knows` (every outcome);
  - `name_lists_read_naturally`;
  - `wakes_are_rate_limited_per_phone`.
- **Hub (`core/tests/phones.rs`):**
  - `phones_are_devices_never_machines`: not in machines or `machine` msgs, presence ignored, path online / offline;
  - `pairing_a_device_introduces_its_group`: fan-out table for a new phone and a new hub, offline kept, revoke clears;
  - `received_intros_add_new_devices_once_and_never_replace`: added + audit + notification + wire shape, repeat is a
    no-op, conflicts, revoked never re-added;
  - `intros_wait_for_the_setting_and_never_come_from_phones`: setting off, then on via `update_settings` (config,
    audit), phone sender refused;
  - `a_phone_reads_the_mirror_and_acts_as_mobile`: mirror bodies dropped, client forced to mobile, unconfirmed refused,
    confirmed applied with `mobile@iPhone` audit, `byMachineId`, "Approved from iPhone";
  - `phone_invites_are_relay_only`: `relay_off`, `invalid` with addr or bad TTL;
  - `local_requests_wake_offline_phones_once_a_minute`: phones only, rate limit, revoked forgotten;
  - `a_linked_phone_gets_this_hubs_items_only`: a real LAN link with a peer node that says iOS. It checks that the phone
    is stored as a phone with no addr, gets this hub's snapshot (never another machine's mirrored session), `sync_done`,
    live mirror updates and the intro listing the other hub, and gets an `intent_ack` with client mobile.
- **Settings:** `roster_intros_are_a_peer_setting_applied_at_once`.
- **CLI:**
  - `phones_are_listed_apart_from_machines`;
  - `remote_decisions_name_the_device`;
  - `--phone` parsing (conflicts with `--addr`).
- Existing tests updated for v8: `for_phone`, the new Device fields, and `RosterIntro` in the compile-time
  peer-surface guard.

## Live smoke (scratch dirs, local relay, real binaries)
Setup:
- `wrangler dev --port 8797` serves the relay locally. Its key was read from `services/relay/.dev.vars` and never printed.
- Hubs A and B run with scratch `--data-dir`, scratch `HOME` / `USERPROFILE`, `[relay] url = ws://127.0.0.1:8797` and LAN
  listeners on 127.0.0.1.
- fe-mobile's `apps/mobile/scripts/headless-phone.ts` plays the phone.
- Invite URIs stayed in shell variables and were never printed.

| Check | Result |
|---|---|
| A and B relay status | `connected` (`krypto settings`) |
| B `peer join` A's LAN invite | paired |
| `krypto peer invite --phone` on A | PASS: no `addr`, has `relay` / `rt` / `rk` |
| Headless phone pairs from that URI | paired with Hub a |
| Phone status | Hub a online (relay) synced; **Hub b online (relay) synced `introducedBy=Hub a`**: the intro worked and B let the phone in over the relay |
| `krypto machines` on A | Hub b in machines; PHONE table: Smoke phone, ios, online, relay, "here" |
| `krypto machines` on B | PHONE table: Smoke phone ... "introduced by Hub a" |
| B pairing_audit / devices | `introduced` "introduced by Hub a (id)"; the phone stored as kind phone, os ios, `introduced_by` = A, no addr |
| Mock ACP agent on A asks a permission, phone offline | log `woke an offline phone`; a 2nd request within 60 s: no 2nd wake |
| Phone `status` | sees A's session `waiting_permission` and the pending request (mirror over the relay) |
| Phone `approve <id>` | `applied`; the agent got `allow`; A audit_log `mobile@Smoke phone`, origin client `mobile`; intent_log `approve mobile Smoke phone applied` |
| Pending intros on A afterwards | empty |

The smoke hubs, the local relay and its wrapper shells were stopped by PID afterwards; only scratch-path / port-8797
processes were touched. The real `~/.claude` was never touched (no hooks were installed).

## Notes for others
- fe-mobile: `headless-phone.ts status` without `--os` reported the phone as "android". Each hello's os updates the
  roster (`update_seen`), so a phone app must always send its real os. The headless script defaults to android unless
  `--os` is passed on every command.

## Known gaps
- No ASCII QR in the CLI (that would need a new dependency); the desktop draws the QR (fe-settings, `qrcode`).
- Revocation stays per hub (contract): no "revoke everywhere" yet.
- A queued intro counts as delivered once the link accepts it. If that link drops before the frame leaves, the next
  re-pairing or the next intro covers it, but there is no resend on its own.

---

# sde-hub report — step 5b (settings in the UI) + step 6 phase B (relay wiring)

Branch `step-5b-settings-ui`, nothing committed. Owned paths only: `crates/hub/`, `crates/discovery/` (hook status + remove).
`Cargo.lock` changed by cargo for the approved `toml_edit` dependency (already in the lock through krypto-discovery).

## Gate

| Command | Result |
|---|---|
| `cargo test -p krypto-hub -p krypto-discovery` | discovery **39 passed**, hub **158 passed** (was 31 / 128) |
| `cargo clippy -p krypto-hub -p krypto-discovery --all-targets -- -D warnings` | clean (after t13 boxed `UpdateSettings.patch`) |
| WebSocket smoke, scratch `--data-dir` + scratch `HOME`/`USERPROFILE`/`CLAUDE_CONFIG_DIR` | all PASS (below) |
| restart smoke (`restart_hub` + runtime.json + `krypto hook`) | all PASS (below) |

## What was built

### Hub settings (`crates/hub/src/settings.rs`, pure; `crates/hub/src/core/settings.rs`, runtime)
- `get_settings` -> `{settings, restartRequired, configPath}`; `overriddenBy` from `--port` / `KRYPTO_HUB_URL` /
  `--peer-listen` / `ANTHROPIC_API_KEY` (env key only while config.toml has none). Secrets only as `...Set` booleans.
- `update_settings{patch}`:
  1. `apply_patch` validates the whole patch: first failure -> `invalid:<field>: <reason>`, camelCase wire path with
     indices (`peer.startRoots[2]`, `customAgents[1].command`), as fe-settings asked. Nothing written or applied.
  2. Runtime changes that can fail run first: peer listener (`stop_listening` / `listen`, bind failure ->
     `invalid:peer.listen: could not listen on ...`, old listener restored), relay (`set_relay`, `bad_url` / `bad_key`
     -> `invalid:relay.url` / `invalid:relay.accessKey`).
  3. config.toml is edited with `toml_edit`: only the changed keys. Comments above a key and after a value are kept,
     as are unknown keys/sections and the `env` of custom agents that keep their id. The result is parsed back and
     must give the new values. Written atomically (temp file in the same dir + rename, 0600 on Unix). A write
     failure undoes steps 2.
  4. Runtime: brain rebuilt (bot keys), watchdog `set_config` (timings, nudge state kept), watchdog / daily-report loops re-read the config
     (`Notify`), retention prunes now, repos index rebuilt, discovery re-run (custom agents / offline);
     accept_intents, start_roots and session timeouts are read at use time. `hub.port` -> `restartRequired`.
  5. Audit row (`settings_audit`, schema v7: ts, actor, client kind, kind, keys; never values), log line with key
     names only, `ServerMsg settings` via `send_local`.
- `hook_status` (claude, codex, copilot-cli, plus copilot-vscode for up to 20 repo-index folders) and
  `install_hooks{target, repo, action}`: preview = exact diff, nothing written; apply / remove are audited.
- `open_panel{view}` -> `send_local(open_panel)`, ack `{delivered}` = a desktop client is connected (counted in server.rs).
- `restart_hub`: ack, then after 300 ms `krypto serve` stops the server, `hub.shutdown()` (bye{shutdown} to peers,
  listener/relay closed, agents stopped), removes runtime.json, spawns `current_exe` with the same args (detached,
  stdio null, own process group, env `KRYPTO_HUB_RESTARTED=1`), exits 0. The replacement retries binding the API port
  and the peer listener for 10 s. Audited (kind `restart`, keys = pending restart keys). Refused for in-process hubs.
- `data_dir/runtime.json` `{port, pid, startedAt}`: written once the API port is bound, removed on clean exit (only
  if the pid is ours). **Every** CLI client command (`krypto hook` included) resolves `--port` > `KRYPTO_HUB_URL` >
  runtime.json > config.toml (`HubEndpoint::for_client`). The daemon's own port ignores runtime.json.
- CLI: `krypto settings [--json]` (running hub, else config.toml; secrets "set (hidden)"; relay status line).

### Step 6 phase B (relay wiring)
- `[relay] url` / `access_key` (secret, redacted in `Debug`) in config.toml; applied with `set_relay` at startup (a bad
  value -> warning + startup notice, the hub still starts) and on update.
- `PeerEvent::RelayStatus` -> `ServerMsg peer_status`; `Welcome.peer.relay` = `relay_status()`.
- `PeerEvent::LinkPath` -> `Device.path` (cleared on LinkDown); `devices` broadcast when it changes.
- `join_peer` uses `join_invite`; an adopted relay is written to config.toml (url + key), audited (kind `pairing`),
  announced with `settings`. Never replaces a configured relay.
- `krypto machines` shows a PATH column and a `relay:` status line (never the key).

### Discovery (`crates/discovery/src/hooks`)
- `HookChange::{Install, Remove}`, `change_with_home` / `change`, `status_with_home` / `status` (`HookState`:
  installed / outdated / missing / unsupported + detail), `config_path_with_home`, `agent_found`, `agent_home`.
- Remove takes out only our entries (Claude settings.json, Codex notify + inline hooks + hooks.json, Copilot files);
  our own `krypto.json` is deleted when nothing else is left; backups first. Status never writes.
- "Changed" is now semantic for JSON (a file that differs only in formatting is not rewritten).

## Smoke (scratch data dir, scratch HOME + USERPROFILE + CLAUDE_CONFIG_DIR/CODEX_HOME/COPILOT_HOME)
- get_settings over WebSocket: config.toml values, `overriddenBy {"hub.port":"--port"}`.
- update_settings listen on `127.0.0.1:47951` -> TCP connect succeeds, create_invite works; listen null -> connect
  refused, create_invite `peer_listener_off`.
- Hand-written comments, trailing comments and `[unknown_future]` kept in config.toml; `listen` removed.
- Invalid patch (`acceptIntents:false` + `dailyReportTime:"7pm"`) -> `invalid:hub.dailyReportTime: ...`, config.toml
  byte-identical (sha256), acceptIntents unchanged.
- API key set/clear: only `anthropicApiKeySet`; the key never in hub.log or the serve output.
- Port change -> `restartRequired ["hub.port"]`.
- hook_status claude = missing, config path in the scratch home; install_hooks preview printed the full diff and wrote
  nothing. Real `~/.claude/settings.json` sha256 unchanged before/after.
- Restart: port 47971 saved (runtime.json keeps 47950), `krypto hook claude` without `--port` reached the running hub;
  `restart_hub` -> old process exited 0, replacement (new pid) wrote runtime.json 47971, answered there with
  `restartRequired []`, and the next hook reached it. Smoke processes stopped by pid afterwards.

## Decisions for t13
1. **0 = off is accepted** for `hookSessionStaleHours`, `ownedSessionIdleHours` (<= 8760) and `eventRetentionDays`
   (<= 3650): config.toml documents 0 as "never"/"keep forever", and the UI must be able to keep a value it reads.
   The contract says "> 0"; watchdog values are strictly bounded (10 s-24 h, tick 5-3600 s).
2. `""` clears optional non-secret strings (ollamaUrl/Model, listen, relay.url); secrets need `null`, `""` is
   `invalid:<field>: is empty; send null to clear it` (guards against an accidental clear).
3. A UI change of `peer.listen` drops the `--peer-listen` override marker (the setting now decides). `--port` keeps its
   marker: on the next start the flag still wins.
4. Watchdog timing changes use sde-brain's `Watchdog::set_config` (follow-up after review): new timings, nudge state
   kept, so nothing is nudged twice (test `watchdog_timings_change_without_repeating_nudges`).
5. `agent_home()` matches Node's `os.homedir()` (USERPROFILE on Windows, HOME elsewhere); `krypto install-hooks` uses it
   too (was `dirs::home_dir`).
6. Relay URL/key validation mirrors krypto-peer's rules (wss/https; ws/http to loopback only; key base64url 1-256); the
   link layer's refusals still map to field errors. The stored URL is the trimmed user text; `peer.relay.url` shows the
   link layer's normalized form.
7. The replacement is spawned after the old hub has said bye and closed its ports (not before), plus the 10 s bind
   retry; a hub started by the desktop app as a child continues detached, so the desktop must reconnect via runtime.json.

## Security review fixes (t13's list)
| # | Fix | Test |
|---|---|---|
| 1 | `hub.port` equal to the peer listener's port (loopback / every interface) -> `invalid:hub.port: is the peer listener's port`. A port change test-binds `127.0.0.1:<port>` first -> `invalid:hub.port: is in use on this machine (...)`, nothing written. After `restart_hub` the replacement that cannot bind the new port within 10 s stays on the previous one (passed in the restart marker), logs why and tells clients with a notification; `restartRequired` keeps `hub.port`. | `the_api_port_never_shares_the_listener_port`, `a_port_in_use_or_shared_with_the_listener_is_refused`, `markers_parse_to_a_fallback_port`; live fallback smoke (blocked new port -> replacement on 47971, notice, restart still pending) |
| 2 | Hook writes: random temp name created with `create_new` (O_EXCL, retried on a clash), the original's Unix mode copied onto the temp file (Windows: inherited ACL), then rename. Backups are created with `create_new` too (a planted link at the backup name fails). For copilot-vscode, `.github`, `.github/hooks`, `krypto.json` and the temp path must not be symlinks or junctions: checked before anything is read (preview included), before the folders are created and again before the rename. | `repo_hooks_never_go_through_a_symlink` (runs here through a directory junction, since this account cannot create symlinks), `rewrites_leave_no_temp_files_and_keep_permissions` |
| 3 | `GET /health` -> `{ok, version, machineId}`. CLI clients (`krypto hook` included) use runtime.json's port only when `/health` there answers within 500 ms with machine.toml's id; otherwise config.toml's port (`cli::client_endpoint`). runtime.json is written 0600 on Unix (same atomic writer as config.toml). | `clients_verify_runtime_json_before_using_it` (real server: verified port, wrong machine id, nothing listening, `--port`) |
| 5 | start_roots / repos.paths: the whole drive / filesystem root (`/`, `C:\`, `\?\C:\`) refused; stored canonicalized (links and `..` resolved, no verbatim prefix); duplicates compared after resolving. | `folders_are_canonical_and_never_a_whole_drive` |
| 6 | `install_hooks` repo: absolute, `repos::plain_path` (no UNC / device path), an existing local folder that is in the repo index or has `.git`; the canonical path is used. | `hooks_preview_writes_nothing_then_apply_and_remove` (refused without `.git`, UNC and device paths refused, index folder accepted) |
| 7 | `adopt_relay` runs `check_relay_url` / `check_relay_key` before writing; an invalid invite relay is never written or audited. | `a_relay_from_a_pairing_invite_is_kept_once` |
| 8 | `KRYPTO_HUB_RESTARTED` (now the previous port) is read and removed from the environment at the very start of `main`, before the Tokio runtime starts (main builds the runtime by hand), so agents and hooks never inherit it; only the startup binds retry. | `markers_parse_to_a_fallback_port`, `the_replacement_gets_the_same_arguments_and_the_previous_port` |

Also: `Roster::get_revoked` implemented in the store (`Store::revoked_roster_entry`; revoked rows keep key and counters, re-pairing makes them normal entries again), test `revoked_devices_keep_their_key_for_a_bye`. sde-peer's hub spots are real handling now: `LinkPath` -> `Device.path` + `devices` broadcast, `RelayStatus` -> `peer_status`; the invite log at `peers.rs` (create_invite, "relay only" when there is no addr) and `core/tests.rs` (`Invite.addr` is `Option`) are kept as they fit the new API.

## Known gaps
- Relay adoption is unit-tested (`adopt_relay`) but not end to end over a real relay (needs sde-relay's worker).
- No client-facing read API for the settings audit (store only: `Store::settings_audit`).
- Removing a key keeps the comment lines above it by moving them to the next key in its table; if it was the last
  key, those lines go.
- `agentInstalled` is PATH detection (`claude`, `codex`, `copilot`, `code`/`code-insiders`).
- Housekeeping: sde-peer's patch had turned `core/peers.rs` and `core/tests.rs` into CRLF files; I restored LF (content
  unchanged) so the diff shows only real changes.

---

# Earlier sde-hub report (v1 build, kept as written)

Renamed to Krypto on 2026-11-23; names below are pre-rename.

# sde-hub report: `crates/hub` (`pet-hub`, binary and lib)

Status: **DONE**.
- `cargo check -p pet-hub --all-targets` passes.
- `cargo clippy -p pet-hub --all-targets` is clean.
- `cargo test -p pet-hub` passes all 33 tests.
- No `todo!()` or placeholder bodies are left.

## Files (all inside `crates/hub/`)

| File | Purpose |
|---|---|
| `Cargo.toml` | Lib `pet_hub` and bin `pet-hub`. Pinned dependencies: axum 0.8.9 (ws), rusqlite 0.43 (bundled), clap 4.6 (derive, env), toml 1.2, chrono 0.4, subtle 2.6, getrandom 0.4, hex, tokio-tungstenite 0.29 (same version axum uses), tracing-appender 0.2. Dev dependency: tempfile. |
| `src/lib.rs` | Module list and `HUB_VERSION` |
| `src/main.rs` | `#[tokio::main]` calling `cli::run()` |
| `src/config.rs` | `Paths` (data dir layout), `HubConfig` and `config.toml` (serde defaults, commented template written on first `serve`), token (32 random bytes as hex, `create_new`, mode 0600 on Unix), `HubEndpoint` (parses `AI_PET_HUB_URL` as http or ws) |
| `src/store.rs` | `Store` over rusqlite: `PRAGMA user_version` migrations, WAL, blocking methods, and `Store::run` (spawn_blocking) for async callers. Tables: `agents`, `sessions`, `events` (`id INTEGER PRIMARY KEY AUTOINCREMENT` = `HubEvent.id`), `pets`, `rules`, `chat`, `audit_log` and `settings`. |
| `src/bus.rs` | `Bus`: a tokio broadcast of `ServerMsg` (capacity 4096) |
| `src/notify.rs` | `HubNotification` builders (stop, error, permission) and session labels. The hub only emits `notification` messages; clients decide how to show them. |
| `src/core.rs` | `Hub`: in-memory cache plus store, ACP and hook feed merge, rules, permissions and audit, pets and growth, chat and brain actions, watchdog, daily report, discovery refresh |
| `src/core/host.rs` | A thin enum seam over `AcpHost`, with a `FakeHost` for unit tests (`#[cfg(test)]`) |
| `src/core/tests.rs` | Message-handling tests against the fake host |
| `src/server.rs` | axum router: `/health`, `/ws` (hello first, Origin allowlist, constant-time token check), `/hooks/{source}` (Bearer token). Binds `127.0.0.1` only. |
| `src/logging.rs` | tracing to stderr plus `data_dir/hub.log` (non-blocking; rotated to `hub.log.1` above 10 MB) |
| `src/cli.rs` | clap CLI, `RemoteHub` WebSocket client, `Conn` (remote or in-process), event printer, permission prompts |

## Behaviour

**Startup**
- Loads agents, sessions (open ones, plus any active in the last 7 days), pets and rules.
- Rules come from hub.db if a client ever called `set_rules`, else from `config.toml` `rules`, else `pet_brain::rules::default_rules()`.
- Prunes events older than `event_retention_days` (default 30).
- Owned sessions left open by a previous hub process are marked ended (`error` if they were mid-turn), because their ACP processes died with that process.

**Client messages:** every `ClientMsg` gets exactly one `ack`. The result shapes match `AckResults`, and the tests check them.
- `start_session`
  - Calls `session/new` (`AcpHost::start_session` with `prompt=None`).
  - Logs a `session_start` event (the host does not emit one).
  - If a prompt was given, calls `AcpHost::prompt`. The host emits the `user_message` event itself; the hub does not log a second copy.
  - `cwd` must be an existing directory. It is made absolute (not canonicalized, so no `\\?\` prefix) and `~` is expanded.
- `prompt` and `cancel` work only on open owned sessions. Hook sessions get a clear error.
- `cancel` cancels any permission requests still pending for that session.
- `resume_session` reuses an open owned session with the same `acpSessionId`.
- `list_sessions` returns open sessions. With `includeEnded` it also returns up to 500 recent sessions from the database.
- `get_events` defaults to the latest 500 events, `sinceId` pages forward, and the limit is capped at 5000.
- `hello` returns a `Welcome`. It includes open sessions, sessions ended within the last 24 h, pets, pending permissions and rules.

**Permissions**
- Every ACP `PermissionRequest` is checked with `pet_brain::rules::evaluate_in(rules, req, Some(session cwd))` (through one helper, `Hub::rule_action`).
- **auto_allow / deny:** answered through `AcpHost::resolve_permission`, then `permission_resolved{by:"rule"}` is broadcast. No `permission_request` and no `request_permission` event are emitted, so the dog doesn't bark at auto-approved reads.
  - If auto_allow finds no allow option, the hub falls back to asking.
  - If deny finds no reject option, the hub answers `cancelled`.
- **ask:** the request goes into pending. The hub logs a `request_permission` event and broadcasts `permission_request` plus an alert notification.
- The pending entry is removed before resolving, so two clients answering at once can't both resolve it. An unknown `optionId` is rejected and the request stays pending.
- **Audit log:** every decision is written to `audit_log`: when, request, session, agent, tool kind, title and paths, `decided_by`, actor (`desktop#3`, `chat:vscode#2`, `rule:auto_allow`, `acp-host`, `hub:session_finished`), option, outcome (`allowed`, `rejected`, `cancelled` or `failed`) and detail.
- **Host-initiated cancels:** when the host emits `Event{PermissionResolved}`, the hub removes the request from pending, audits it, logs the event and broadcasts `permission_resolved` with the event's `by`. If the hub already closed that request, the echo is dropped, so it is never broadcast twice.
- When a session is done, errored or ended, any requests still pending for it are closed with `by: system`.
- The `permission_resolved` event payload is `PermissionResolvedPayload` plus a `toolCall` field.

**Hooks:** `POST /hooks/{source}` runs `pet_discovery::hooks::normalize`, which yields a session keyed by `(agentId, externalId)`.
- The session is upserted: status, title, cwd, `vscode_hook` promotion, ended, and reopened if the same external id comes back.
- The event is logged and the notification is broadcast.
- Alert notifications for the same hook session are deduped within 10 s. This is the Claude PermissionRequest-then-Notification double fire that sde-discovery flagged.
- Hook sessions with no hooks for `hook_session_stale_hours` (default 12) are marked ended by the watchdog tick.

**Pets**
- Each installed agent, and each agent that gets a session, gets `growth::default_pet` (a dog).
- `growth::on_event` runs on every logged event. Changed pets are persisted and broadcast on a 2 s flush, which avoids one `pet` message per streaming chunk.
- `update_pet` validates its input. Names are 1–40 characters.

**Chat**
- The user's line is stored, broadcast (hub-client upserts chat by id, so the requester sees no duplicate) and acked.
- `Brain::handle` then runs in a spawned task with a `BrainContext`: agents, open and recent sessions, about 400 recent events (milestones plus streaming text), pending requests and the pet.
- The resulting `BrainAction` is executed. StartSession ignores an empty prompt, as t13 asked.
- The reply is sent as `ServerMsg::Chat`. If the action fails, the error is appended to the reply text.

**Watchdog and daily report**
- `watchdog.tick` runs every `watchdog.tick_secs` (30 s). Its defaults are 5 min to the first nudge and 10 min between repeats, matching pet-brain.
- The daily report fires once per local date, in the hour after `daily_report_time` (default `18:00`, `"off"` disables it).
  - It is generated by `Brain::daily_report`, with a deterministic summary as the fallback.
  - It is sent as an info notification and stored as a pet chat line.
  - The last report date is stored in `settings`, so a restart doesn't repeat it.

**Server**
- `/ws` rejects any non-empty Origin that is not on the allowlist (`vscode-webview://…`, `tauri://localhost`, `http(s)://tauri.localhost`, `http://localhost:1420`). `null` is rejected.
- The first frame must be `hello` within 5 s.
- A bad token gets `ack{ok:false,error:"unauthorized"}` and then close code 1008.
- After the hello, the connection subscribes to the bus before taking the Welcome snapshot.
- Requests are handled concurrently, so a slow `start_session` doesn't block other requests.
- The server pings every 30 s.
- A lagging client is closed with code 1013 so it reconnects and resyncs.
- `/hooks` also checks Origin, and the token comparison is constant-time.

**CLI**
- `serve` runs the daemon (Ctrl-C or SIGTERM triggers a graceful stop and shuts down the agents).
- `agents [--refresh] [--json]`
  - Goes through the running hub when one is up, otherwise runs `discover` locally.
  - `--refresh` moves the 24 h registry cache aside so the registry is fetched again, and restores the cache if the fetch fails.
- `sessions [--json]`
- `start <agent> --cwd <dir> [--prompt <text>] [--local]`
  - Goes through the running hub when one is up, otherwise drives the agent in-process. In-process runs still log to hub.db.
  - Streams readable events: message and thought text, tools, plans and permission outcomes.
  - Asks on stdin for `ask` permissions: an option number, `y` or `n`. End of input means deny.
  - Ctrl-C cancels the turn; a second Ctrl-C quits.
  - Without `--prompt` it runs an interactive prompt loop.
- `hook <source> [json…]` calls `pet_discovery::hooks::run_hook_cli`, which:
  - accepts argv JSON or stdin
  - prints the stdout reply Codex and Copilot require, even when there is no token or the hub is down
  - forwards with a 2 s cap
  - always exits 0
- `install-hooks <claude|codex|copilot-cli|copilot-vscode> [--apply] [--repo]` runs `hooks::install` with `current_exe()`.
- `token`
- Global options:
  - `--data-dir` (env `AI_PET_HUB_DATA_DIR`)
  - `--port`, which takes precedence over `AI_PET_HUB_URL`, then `config.toml`, then 47800

## Commands run and results

- `cargo check -p pet-hub --all-targets`: OK, once all dependencies existed. Earlier runs failed only while pet-brain's `chat.rs` was mid-rewrite.
- `cargo clippy -p pet-hub --all-targets`: clean. The remaining warnings are in pet-brain.
- `cargo test -p pet-hub`: **33 passed**. They cover store, config, token, endpoint, origin, bearer, CLI parsing, and core message handling with a fake host: ack shapes, ask then user-resolve plus audit, auto allow and deny, `set_rules` re-evaluation, cancel and session-end cleanup, host-initiated cancel dedupe, stop notifications, pet updates, chat ack, resume reuse,, close_session cleanup, and restart closing owned sessions.
- **Binary smoke test:** `serve` with a scratch `--data-dir`, `offline = true` and port 48123.
  - `GET /health` returned `{"ok":true,"version":"0.1.0"}`.
  - The token file is 64 hex characters.
  - `/hooks/claude` with no auth returned 401, a bad Origin returned 403, and a valid request returned 200 `{"ok":true}`.
  - `/ws` with an evil Origin returned 403.
  - `pet-hub hook codex '<notify json>'` and a Claude `Stop` hook on stdin both exited 0 and created sessions.
  - `pet-hub sessions` and `pet-hub agents` went through the hub over WebSocket and printed tables. Discovery found Claude, Codex, Gemini and Copilot.
  - A bad `AI_PET_HUB_TOKEN` was refused with a clear message.
  - `start` with an unknown agent or a missing cwd failed with clear errors.
- **Not run:** a real Claude or Codex ACP turn. That is E2E and paid usage, so it is out of scope. The command for it is `pet-hub start claude-code --cwd <repo> --prompt "..."`.

## Known gaps

- **`AcpHost::close_session` has no client trigger:** the protocol has no "end session" message. The hub uses it (via `Hub::close_session`) only to close owned sessions idle in idle/done/error for `owned_session_idle_hours` (default 12, 0 = never), freeing their agent process. `shutdown()` closes everything when the hub exits.
- **Windows token file perms:** the token relies on the inherited ACL of the user-only `%APPDATA%` directory; no explicit DACL is set. Unix uses 0600.
- **`set_rules` has no broadcast:** the protocol has no `rules` `ServerMsg`, so other clients see new rules only on reconnect (`Welcome.rules`).
- **Notifications are not persisted:** they are broadcast-only, so a client that was offline misses them. The daily report is also stored as a chat line.
- **Daily report timing:** it only fires if the hub runs during the hour after `daily_report_time`.

## Decisions for the orchestrator

1. **Rules precedence:** rules in hub.db, once a client has called `set_rules`, win over `config.toml` `rules`, which win over `default_rules()`. Should `config.toml` rules win instead?
2. **`cancel` by a user:** pending requests get `permission_resolved{by:"user"}`. Automatic cleanups (session done or ended) use `by:"system"`. Host echoes keep the `by` the host sent.
3. **Should a `rules` `ServerMsg` be added** so `set_rules` reaches every client live?
4. **Should an "end session" `ClientMsg` be added** that calls `AcpHost::close_session`?
5. **`AI_PET_HUB_DATA_DIR` / `--data-dir`:** this env var is extra (meant for tests and portable installs). Clients don't know about it, so they would need `AI_PET_HUB_TOKEN` with it.

## t13 follow-ups applied

- `pet-hub hook <source> [args…]` delegates to `pet_discovery::hooks::run_hook_cli`. The source is parsed with `parse_hook_source`, and trailing argv JSON is accepted.
- `BrainAction::StartSession` treats an empty prompt as no prompt.
- The default for `watchdog.repeat_every_ms` is now 10 min.
- `ResolvedBy::System` is used for automatic cancels, and the CLI printer handles it.
- The host's `Event{PermissionResolved}` clears pending, is audited and is broadcast once.
- Rule checks use `pet_brain::rules::evaluate_in` with the session cwd.
- `AcpHost::close_session` is wired in through `Hub::close_session`, which closes idle owned sessions.

## Step-0 security floor (t13, unfrozen task)

- **`set_rules` validates first.** It runs `pet_brain::rules::validate_rules(&rules)` before anything else.
  - On failure the ack is `invalid_rule: <reason>` (for example `invalid_rule: rule "x": toolKind "execute" can never be auto-allowed (...)`) and **nothing changes**: neither the live rules nor hub.db are touched.
  - Empty or duplicate ids also fail with an `invalid_rule:` error.
  - Pending requests are re-checked only after a valid set has been stored.
- **Startup sanitizing.** Rules loaded from hub.db, config.toml or the built-in defaults go through `core::sanitize_rules`. That function keeps a rule only if `validate_rules` passes on it alone and its id is unique and non-empty.
  - Dropped rules are logged with `warn!`, naming their source, ids and reasons.
  - They also raise a `HubNotification` (level `warning`, "Unsafe permission rules ignored"). It is broadcast on the bus and **replayed to every new WebSocket client right after its Welcome**, because no client is connected at startup. It stops being replayed after the next successful `set_rules`.
  - The stored rules are not rewritten, so the user can still fix the source.
- **The rules engine is the only automatic approval path.** I audited every `resolve_permission` call site:
  - Automatic answers happen only in `on_permission_request` and in the post-`set_rules` re-evaluation. Both go through `Hub::rule_action`, which calls `evaluate_in`, and `auto_option`.
  - `close_permission` only ever answers `cancelled`.
  - User decisions come from `resolve_permission` client messages, chat-parsed "approve"/"deny" commands (audited as `chat:<client>`), and CLI stdin. When CLI input closes, the CLI denies.
- **Tests:**
  - `set_rules_rejects_unsafe_rules_and_changes_nothing`: auto_allow for execute, delete, fetch, other and a missing toolKind, plus duplicate ids. Ask and deny rules for any kind are accepted.
  - `startup_drops_invalid_rules_from_the_store`: invalid, missing-kind and duplicate rules are dropped and the notice names them. An execute request still asks the user. A later fix clears the notice.
  - `startup_drops_invalid_rules_from_config`: valid config and the defaults raise no notice.
  - The existing auto-allow tests now use `edit`, a kind that may be auto-allowed.

## Step-0 fix: prompt printed twice

- **Cause:** `AcpHost::prompt` already emits `Event{UserMessage}` for every prompt (pet-acp `host.rs`), and `Hub::prompt` recorded a second copy. So `pet-hub start --prompt X` printed `you: X` twice, and the event log stored the prompt twice.
- **Fix:** the hub no longer records its own `user_message`; the host's event is the only one. As a belt-and-braces guard, the CLI printer also drops a `you:` line identical to the one it just printed. That covers agents that replay the prompt as `user_message_chunk`. A later turn with the same text still prints.
- **Tests:** `prompt_echo_printed_once`, and `start_prompt_events_and_ack_shapes` now feeds the host's user_message events.
