# sde-hubcore report: Step 1 (machine identity) in `krypto-hub`

Branch: `step-1-machine-identity`. Nothing committed. I only edited `crates/hub/`, plus this report.

## Files changed

| File | Change |
|---|---|
| `crates/hub/src/machine.rs` (new) | `machine.toml` `{id, name}`. `generate_id()` produces 12 chars of `[a-z0-9]` from `getrandom`, an existing dependency, with rejection sampling so no character is biased. `default_name()` uses the OS host name: Windows `COMPUTERNAME`, macOS `scutil --get ComputerName`, Linux `/etc/hostname`, else "This machine". `validate_name()` trims, then requires 1..=40 `char`s and no control characters; on failure the error chain carries `AckCode::Invalid`. Also `load`, `load_or_create` and an atomic `save` (temp file + rename). |
| `crates/hub/src/config.rs` | Adds `Paths.machine`. `discovery_config(paths, machine_id)`. |
| `crates/hub/src/store.rs` | Schema v2 migration, written in Rust (see below). `Store::open(path, machine_id)` / `open_in_memory(machine_id)`. Every row writes `machine_id`. `HubEvent { id: "<mid>:<seq>", seq: rowid, machineId }`. `events(session, since_seq, limit)`. New `session_exists()`, `recorded_machine_id(db)` (read-only peek) and `AuditEntry.machine_id`. |
| `crates/hub/src/core.rs` | `AckCode` (`not_owner` / `not_found` / `invalid`): when one is anywhere in a request's error chain, the ack error is exactly that code. `HubOptions.machine`. Ownership checks on every id in a `ClientMsg`. New global ids for sessions, hook sessions, chat and notifications. `update_machine`. `Welcome.machine` / `machines`. `BrainContext.machine` / `machines` (`[self]`, same list as Welcome). `Watchdog::new(cfg, mid)`. `growth::default_pet(agent, taken_names)`, where `taken_names` is every known pet name. Pets are matched by `(machine_id, agent_id)`. |
| `crates/hub/src/core/host.rs` | `Host::acp(machine_id, events)` → `AcpHost::new(machine_id, events)`. The fake host stamps `machine_id`. |
| `crates/hub/src/notify.rs` | `make(machine_id, …)`: ids are global and carry `machineId`. `for_permission(agent, pet, &req)`. |
| `crates/hub/src/cli.rs` | Adds `krypto machine [--name "…"] [--json]`. `serve` and in-process `start` load `machine.toml`. Offline `agents`/`sessions` pass the machine id. `short_id` shows the first 8 characters of the local part of a global id. |
| `crates/hub/src/core/tests.rs`, unit tests in each module | Updated for v2. New tests are listed under "Commands run". |

## Behaviour

- **Machine identity.**
  - `machine.toml` is loaded at startup (`serve`, in-process `start`, offline `agents`/`sessions`/`machine`) and created on first use.
  - If the file exists but is corrupt, startup fails with an error instead of replacing it: a new id would orphan every stored id.
  - If the file is missing but `hub.db` exists, the id recorded in `hub.db` (`settings.machine_id`) is reused.
  - `Store::open` refuses a `hub.db` whose recorded machine id differs from `machine.toml`. It checks this before migrating, so a foreign db is never rewritten.
- **Schema v2 migration.** It runs in one transaction with `user_version` 1→2.
  - `machine_id` columns are added to agents, sessions, events, chat and audit_log.
  - Pets are rebuilt with `UNIQUE(machine_id, agent_id)` and the deterministic id `<mid>:pet-<agent>`, which is the same as `growth::pet_id_for`. Chat `pet_id`s are remapped to match.
  - Session ids, plus the `id`/`machineId` inside their JSON, become `<mid>:<id>`. Events keep their rowid as `seq`, and `session_id` becomes global.
  - `request_permission` / `permission_resolved` payloads get global `requestId`/`sessionId` (and `machineId` on `request_permission`).
  - Chat ids and session refs become global, as do audit `request_id`/`session_id`. Agent ids stay local (`codex`) and their JSON gains `machineId`.
  - An id is left unchanged if it already parses as a global id.
- **Ownership acks.**
  - `prompt`, `cancel`, `get_events`, `resolve_permission`, `update_pet`, `chat.petId`: another machine's prefix → `not_owner`; no prefix, or an unknown id with our prefix → `not_found`.
  - `update_machine`: a malformed machine id → `invalid`; a well-formed id of another machine → `not_owner`. A bad name → exactly `invalid` (per t13).
  - On success, `update_machine` writes `machine.toml`, broadcasts `ServerMsg::Machine`, and acks `{machine}`.
- **`krypto machine`.**
  - It goes through the running hub's WebSocket when the hub's machine id matches this data dir's `machine.toml` (or there is no local file). Otherwise it edits `machine.toml` directly.
  - The CLI validates the name first, so users see the reason rather than the bare `invalid`.
  - `--json` prints the `Machine`. When no hub is running, `online` is false.

## Follow-up: unique pet names (live-test bug)

v1 picked pet names without checking for collisions. A migrated v1 `hub.db` therefore kept duplicates: claude-code, copilot and opencode-v2 were all "Mochi".

- **Startup dedupe** (`core::dedupe_pet_names`, run in `Hub::build` after the migrations).
  - Pets are taken oldest first (store rowid order, which the v2 migration preserves). Names are compared trimmed and case-insensitively.
  - The oldest pet keeps its name. Each later duplicate, and any blank name, gets `growth::default_name(machine_id, agent_id, taken)`, where `taken` is every known pet name plus the names assigned so far.
  - Renamed pets are persisted and broadcast as `ServerMsg::Pet`, and each rename is logged. A restart finds nothing left to rename.
  - Other machines' pets are never renamed (their hub owns them). Their names are reserved first. This only matters once mirrors exist (step 3).
- **`update_pet` name patch.** The name is trimmed and must be 1..=40 `char`s with no control characters, else exactly `invalid`. If another known pet has the same name (case-insensitive, any machine), the error is exactly `name_taken` (new `AckCode::NameTaken`) and nothing changes. A pet may change the case of its own name. The uniqueness check and the write happen under one state lock, so two concurrent renames can't both win. Skin and personality validation is unchanged.
- **Tests:**
  - `dedupe_keeps_the_oldest_name_and_renames_later_duplicates`
  - `dedupe_never_renames_other_machines_pets`
  - `migrated_duplicate_pet_names_are_fixed_at_startup`: a v1 db fixture with three "Mochi"/"mochi" pets. It checks that claude-code keeps "Mochi", that the changes are persisted, and that a restart is stable.
  - `update_pet_names_are_validated_and_unique`: `name_taken` across case and whitespace, re-casing its own name, 40 vs 41 `ö`, blank, and a control character.
  - The test-only helper `store::create_v1_db_for_tests` builds the v1 fixture.
- After this follow-up, `cargo test -p krypto-hub` passes 53 tests and `cargo clippy -p krypto-hub --all-targets -- -D warnings` is clean.

## Commands run and results

- `cargo test -p krypto-hub`: **49 passed**, 0 failed (it was 29 at v1). New tests:
  - machine: `generated_ids_are_valid_and_distinct`, `names_are_validated`, `host_names_are_cleaned`, `parse_is_strict_about_ids_and_lenient_about_names`, `load_or_create_is_stable_and_save_roundtrips`, `missing_file_is_restored_from_hub_db`, `machine_view`
  - store: `v2_migration_rewrites_v1_ids` (builds a real v1 db with sessions, events, pets, chat and audit, then migrates), `migrations_are_idempotent_and_record_the_machine`, `globalize_keeps_global_ids`
  - core: `ids_on_the_wire_are_global`, `foreign_and_legacy_ids_are_refused` (18 cases, including malformed `update_machine` ids → `invalid`), `update_machine_persists_and_broadcasts` (40 vs 41 `ü` chars, blank, control character → `invalid`), `default_pets_get_distinct_names`, `store_of_another_machine_is_refused`
  - cli: `machine_command_without_a_hub`, `short_ids_use_the_local_part`
- `cargo clippy -p krypto-hub --all-targets -- -D warnings`: clean.
- `target/debug/krypto.exe --data-dir <scratch> machine` printed a valid machine:
  ```
  id:    qds2h0e7h0x5
  name:  SURI-DESKTOP
  os:    windows
  hub:   not running
  ```
  - `--json` printed the full Machine.
  - `--name "  Windows PC "` saved it trimmed.
  - `--name "   "` exited 1 with `machine name must be 1-40 characters (got 0)`.
- Smoke test with a scratch hub (scratch data dir, `offline = true`, port 48123; the real data dir was never touched):
  - `machine` reported `hub: running (v0.1.0)`.
  - `machine --name "Smoke PC"` updated it through the WebSocket, and `machine.toml` was rewritten.
  - A hook POSTed to `/hooks/claude` appeared in `krypto sessions` as `9f831c81` (short local id). `sessions --json` showed the global id `nvgk3rw0l3b6:9f831c81-…` and `machineId`.
  - `agents` listed agents. The scratch hub was then stopped.

## Known gaps

- `already_decided` is not separate yet. A request id with our prefix that is no longer pending answers `not_found` (PLAN step 4).
- `Welcome.machines` and `BrainContext.machines` are `[self]` only (pairing is step 2).
- Migration is one-way: a v1 binary refuses a v2 `hub.db` with "schema v2 is newer than this krypto".
- No E2E test, per the ground rules. The migration was tested on a synthetic v1 db, not on a copy of a real user `hub.db`.
- Hook raw payloads (`hook` events) are left as-is. They hold the agents' own ids, not hub ids.

## Decisions (resolved by t13)

1. CONTRACTS will list `machine.toml` under the data dir (t13 is updating it).
2. `update_machine` with a malformed machine id answers `invalid`; a well-formed id of another machine stays `not_owner`. Done, with tests.
3. Bare codes (`not_found`, `not_owner`, `invalid`) are kept.
4. Confirmed: the hub refuses to start when the `hub.db` and `machine.toml` ids don't match.
5. Global notification ids are fine.
