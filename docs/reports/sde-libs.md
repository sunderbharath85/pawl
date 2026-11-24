# sde-libs — Step 1 (machine identity) for pawl-acp, pawl-discovery, pawl-brain

Branch `step-1-machine-identity`. Nothing committed. Every change is inside `crates/acp`, `crates/discovery` and `crates/brain`.

## API (matches CONTRACTS.md "Step 1", plus the extras marked below)

| Crate | Change |
|---|---|
| pawl-acp | `AcpHost::new(machine_id: MachineId, events)`. Every `Session` / `HistorySession` / `PermissionRequest` the host builds carries `machine_id`. Permission request ids are `global_id(machine_id, uuid_v4)`. `hub_session_id` is used exactly as passed in (already global). **Extra:** `AcpHost::machine_id()`. **Extra:** `start_session` / `list_history` / `resume_session` refuse an `Agent` whose `machine_id` is non-empty and not the host's ("a hub never runs another machine's agents"). An empty `machine_id` counts as local. |
| pawl-discovery | `DiscoveryConfig.machine_id` (first field; `Default` still derives, giving `""`). Every discovered agent (built-in, registry or custom) gets `Agent.machine_id = cfg.machine_id`. `hooks::NormalizedHook` is unchanged. |
| pawl-brain `growth` | `default_pet(agent, taken_names)`: id `${machine_id}:pet-${agent_id}`, `machine_id` from the agent. Name: start at `fnv1a(machine_id + "/" + agent_id) % len` in `DOG_NAMES` and take the first name not in `taken_names` (trimmed, case-insensitive). When the whole list is taken it loops again with "II", "III" and so on ("Biscuit II"). Helpers changed: `pet_id_for(machine_id, agent_id)` and `default_name(machine_id, agent_id, taken)`. `DOG_NAMES` is now `pub` and has 32 names (was 16), so collisions are rarer. |
| pawl-brain `watchdog` | `Watchdog::new(cfg, machine_id)`. Notifications carry `machine_id`, and their ids are global: `${machine}:watchdog:<key>:<n>`. **Behaviour:** `tick` only nudges sessions and requests whose `machine_id` equals its own, so the owner nudges and mirrored remote sessions are skipped. **Extra:** `Watchdog::machine_id()`. |
| pawl-brain `chat` | `BrainContext` gains `machine: Machine` (per contract). **Extra field:** `machines: Vec<Machine>` (every known machine including self; may be empty). `BrainContext` still implements `Default`, with a placeholder machine that has an empty id. |

### Brain chat behaviour
- New `chat/machines.rs` → `MachineNames`. A reply goes into multi-machine mode when more than one distinct machine id shows up across `machine`, `machines`, agents, sessions and pending requests. A machine missing from `machines` is labelled by its id.
- In multi-machine mode, every agent mention names its machine: the status summary ("Claude Code on Mac mini in web is waiting for your OK to: …"), the "Also: …" list, overview lines ("• Claude Code on Mac mini · web: …"), last activity, the plain LLM fact sheet, the pending list, approve/deny replies, the daily report ("… in 2 repos on 2 machines", per-line and "Needs you"), and the LLM summary prompt. With one machine, replies are unchanged from v1.
- Commands only target this machine:
  - The agent lookup prefers the local agent. A remote-only agent gets "X lives on Mac mini, and I can only start agents on this machine for now." with no action.
  - Existing-session reuse, `default_cwd` and the repo index (`RepoIndex`, used for "in repo X") use local sessions only, because cwds are local paths.
  - Start/prompt replies name this machine in multi-machine mode.
- The brain does not build `ChatMessage`. The hub stamps `machine_id` on chat messages.
- `Brain::summarize_session` has no machine context and never names a machine. `Brain::daily_report(sessions, events)` labels machines by id when the sessions span more than one, since no names are available there.

## Tests added
- acp: `other_machines_agents_are_refused`. `full_turn_with_permission_and_fs` now also asserts `machine_id` on the Session, PermissionRequest (global id whose local part is a UUID), HistorySession and the resumed Session.
- discovery: every agent carries `cfg.machine_id` (both `registry_mapping_and_detection` and the offline end-to-end test).
- growth: `same_agent_on_two_machines_gets_different_names` (the seed differs for ≥4/6 agents, never collides once the roster is passed, and a whole 2-machine roster stays unique), `taken_names_are_skipped_case_insensitively`, `exhausted_list_falls_back_to_numbered_names`. The default-pet test checks the global id.
- watchdog: `other_machines_sessions_are_not_nudged`. Global notification id and `machine_id` are asserted.
- chat: `single_machine_replies_never_name_the_machine`, `status_names_machines_when_several_are_known`, `machine_names_also_come_from_ids_alone`, `commands_only_target_this_machine`. summary: `summaries_name_the_machine_when_several_are_known`.

## Commands run
- `cargo check -p pawl-acp -p pawl-discovery -p pawl-brain --all-targets` → ok
- `cargo test -p pawl-acp -p pawl-discovery -p pawl-brain` → acp 23 passed, brain 63 passed, discovery 32 passed, 0 failed
- `cargo clippy -p pawl-acp -p pawl-discovery -p pawl-brain --all-targets -- -D warnings` → exit 0, no warnings
- `cargo check -p pawl-hub` (information only, not my crate) → 6 errors, all from sde-hubcore's in-progress `HubOptions.machine` / `Welcome.machine(s)` work. None come from the lib APIs.

## Known gaps / notes
- `cargo fmt --check` already reports diffs in files I didn't touch (for example `crates/acp/examples/smoke.rs` line 31). The repo isn't rustfmt-clean and fmt isn't a gate, so I left formatting alone.
- The examples (`acp/examples/smoke.rs`, `discovery/examples/discover.rs`) use fixed placeholder machine ids (`smoke0000000`, `example00000`), because examples have no hub identity.
- The pet name roster is only as good as `taken_names`. The hub must pass the names of every known pet, including already-stored ones, before calling `default_pet` for a new agent.

## Decisions for the orchestrator
1. **Accept `BrainContext.machines`?** It's outside the contract. The hub should fill it with the same list as `Welcome.machines` (`[self]` for now). Leaving it empty also works: then only `machine` plus the ids found on entities are used.
2. **Watchdog filters to its own machine.** OK for step 3? Peers would get nudges through the event stream instead of nudging mirrors themselves.
3. **AcpHost refuses foreign-machine agents.** If the hub ever hands it an `Agent` with another machine's id, it gets an error. I'd keep this.
4. **Brain commands are local-only** (remote agents, sessions and repos are never targeted). Step 5 will relax this with routed intents.
