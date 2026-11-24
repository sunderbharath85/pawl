# sde-hubcore report: Step 5 (remote commands by machine + repo) in `krypto-hub`

Branch: `step-5-remote-commands`. Nothing committed. I edited `crates/hub/` (including the CLI) and this report, which replaces the step 4 report. `crates/brain` is sde-brain's this step; I only coded against its API. No dependencies were added. Files keep their LF line endings. No secrets, prompt text or chat text are logged.

## Files changed

| File | Change |
|---|---|
| `crates/hub/src/repos.rs` (new) | The repo index (pure). `build_index`: candidates are canonicalized as plain local paths (`\\?\C:\` stripped; shares and devices dropped). Folders that no longer exist are dropped, one entry is kept per folder (case-insensitive on Windows; latest use wins), the list is sorted newest first and capped at `REPO_INDEX_MAX`. `unique_names` gives "api (work)", then "api (code/work)", …, as a last resort "api (x) (2)". `validate_peer_repos`. The shared path helpers moved here from `intents.rs`: `plain_path`, `canonical_dir`, `path_key`, `is_within`. |
| `crates/hub/src/core/repo_index.rs` (new) | `repos()`; `rebuild_repo_index()` (session folders + `[repos] paths`; a change pushes presence); `note_session_folder` (the first session in a new folder triggers a rebuild); `repo_index_loop` (hourly). `is_known_folder` is the step-4 remote-start rule, now in one place: the folder must be in the index or under `[peer] start_roots`, and a miss triggers one rebuild before refusing. `repos_of` answers `list_repos`. |
| `crates/hub/src/core.rs` | `Inner.repos` / `seen_cwds`; index built at startup and rebuilt hourly; new local session folders noted in `persist_and_broadcast_session`; `list_repos` dispatch. `BrainContext` gets `remote_agents` (from presence, else persisted), `repos` (every machine, this one included) and `pets` (every machine's). `chat_reply` → `deliver_reply`: local actions run as before; `proposals` (max 5) are stored and broadcast with the pet's message and never run by the hub. |
| `crates/hub/src/core/peers.rs` | Presence carries `repos` (the index). Inbound presence repos are validated (`validate_peer_repos`) and persisted when changed. `presence_repos`. |
| `crates/hub/src/core/intents.rs` | `remote_start_dir` now uses `is_known_folder`. The duplicated path helpers are gone. |
| `crates/hub/src/store.rs` | **Schema v6:** `chat.proposals` (JSON) and `devices.repos`. `insert_chat` / `recent_chat` keep proposals. `local_session_folders` (cwd + last activity). `set_device_repos` / `device_repos`. |
| `crates/hub/src/config.rs` | `[repos] paths = [...]` (documented in the template). |
| `crates/hub/src/cli.rs` | `krypto repos [--machine <name\|id>] [--json]`, which works without a hub from hub.db (this machine) or the last known index (a peer). `krypto start <agent> --repo <name> [--machine X]`: the repo name becomes a path via that machine's index; an unknown or ambiguous name errors, listing what it has. `krypto chat "<text>" [--pick N]`: prints the reply and numbered proposals; `--pick N` executes one through the normal path. A remote start asks on the TTY (`--yes` never applies); a prompt is sent as is. |
| `crates/hub/src/lib.rs`, `core/tests.rs` | `pub mod repos`; tests below. |

## Behaviour

- **Repo index.**
  - **Sources:** the canonical cwds of this machine's own sessions (owned and hook-tracked; never mirrored ones), each with its last activity, plus config.toml `[repos] paths` (no lastUsedAt).
  - **Clean-up:** folders that no longer exist are dropped; names are unique per machine; the newest 200 are kept.
  - **Rebuilt:** at startup, the first time a session uses a new folder, hourly, and once on a remote-start miss.
  - **Sharing:** it goes out in presence; changes push a presence.
- **Peer repos.** A peer's presence repos are validated:
  - at most `REPO_INDEX_MAX`;
  - name 1..=80 chars and path 1..=1024 chars, no control characters;
  - invalid entries are dropped.
  They are kept in memory and persisted (`devices.repos`), so `list_repos` and the brain still have them while that hub is offline or after a restart here.
- **`list_repos{machineId?}`:** none or this machine → the local index; a paired hub → its latest presence, else the last persisted index; unknown → `not_found`.
- **Remote start** (step 4) allows exactly the repo index plus `[peer] start_roots`. A `[repos] paths` folder therefore allows a remote start without any session there, and a folder that left the index is refused.
- **Brain wiring:**
  - `remote_agents` per paired hub (presence, else persisted);
  - `repos` for every machine;
  - `pets` from this machine and the mirror.
  The hub executes only `BrainReply.action` (local). `proposals` are capped at 5, stored with the chat message (`chat.proposals`) and broadcast in `ServerMsg::Chat`. A client executes one as the ordinary `start_session` / `prompt`, which the step-4 rules check.

## Tests

- `cargo test -p krypto-hub`: **126 passed**; `cargo test -p krypto-brain`: **78 passed** (both twice). `cargo clippy -p krypto-hub -p krypto-brain --all-targets -- -D warnings`: clean (forced re-check).
- **Repo index** (`repos.rs`):
  - `the_index_is_canonical_deduped_named_and_newest_first`: `..` and (Windows) case variants collapse to one entry with the latest use; missing folders and blanks dropped; config paths last; plain canonical paths.
  - `the_index_is_capped`: 220 folders → 200 newest.
  - `clashing_names_get_more_of_their_path`.
  - `plain_paths_and_containment`.
- **Peer repo validation:** `repos::peer_repos_are_validated` (empty, over-long name or path, control characters, over 200) and `peer_repos_are_validated_stored_and_listed`. The latter feeds a real presence with bad entries plus 300 repos: 200 valid kept, persisted, listed, an unknown machine `not_found`, and the brain context holds the remote agents, both machines' repos and the mirrored pet.
- **`list_repos` local:** `the_repo_index_follows_sessions_and_config`. Only config folders that exist count; a new session folder (given non-canonical) is indexed canonically and newest first; served by `list_repos` with no id or this machine's id; a deleted folder is dropped at the next rebuild.
- **Remote start uses the index:** `remote_start_uses_the_repo_index` (a `[repos] paths` folder → applied; an unlisted folder → `forbidden`). The step-4 tests still pass on the shared function.
- **Proposals stored + broadcast:** `chat_proposals_are_stored_and_broadcast` (7 → capped at 5, broadcast and stored identically; nothing started, no intent sent). `store::sessions_agents_pets_rules_chat` round-trips a proposal.
- **End to end with the real brain** (two hubs, real links): `a_chat_on_one_hub_proposes_a_command_on_the_other`.
  - B's "tell Desk's codex to run the tests in repo api" → one **prompt** proposal for A's open idle session.
  - After that session ends → one **start** proposal in A's indexed folder.
  - Neither chat started or prompted anything on A.
- **CLI:** `repo_commands_parse_and_repos_resolve` (exact name first, unique prefix, ambiguous lists both, unknown lists what the machine has, empty index).

## Smoke (binary, scratch dirs under `%TEMP%\krypto-smoke-step5`; the real data dir untouched)

Setup:
- A = "Smoke A" `2famf9h2lw2k` (peer listener 47911), B = "Smoke B" `cwxlm5zn8ayc`; later C = "Smoke C" `orzffmbt0znq` (listener 47931) and D = "Smoke D".
- A, B and C run the scripted ACP agent from step 4 as `[[custom_agents]] smoke-agent`.
- A gets its local session in folder X = `a-work\webapp` through a Claude hook (`SessionStart`, cwd X).
- TTY confirmations were typed into the CLI's own console, as in step 4.

```
$ krypto --data-dir <tmpA> --port 47910 repos
Repos on Smoke A:
NAME    LAST USED  PATH
webapp  1s ago     C:\Users\sunde\AppData\Local\Temp\krypto-smoke-step5\a-work\webapp

$ krypto --data-dir <tmpB> --port 47920 peer join "<uri, code=<redacted>>"
Paired with Smoke A (2famf9h2lw2k, windows) at 127.0.0.1:47911

$ krypto --data-dir <tmpB> --port 47920 repos --machine "Smoke A"
Repos on Smoke A:
NAME    LAST USED  PATH
webapp  10s ago    C:\Users\sunde\AppData\Local\Temp\krypto-smoke-step5\a-work\webapp

$ krypto --data-dir <tmpB> --port 47920 chat "tell Smoke A's smoke agent to run the tests in repo webapp"
Woof! Ready: Start Smoke Agent on Smoke A in webapp: run the tests. It runs on Smoke A, so it needs your OK first: confirm it to go ahead.

  1) Start Smoke Agent on Smoke A in webapp: run the tests

Run one with `krypto chat "<same text>" --pick N`.
```

`--pick 1` with the TTY confirmation ("y" typed):

```
$ krypto --data-dir <tmpB> --port 47920 chat "tell Smoke A's smoke agent to run the tests in repo webapp" --pick 1
…
  1) Start Smoke Agent on Smoke A in webapp: run the tests
…
Run on Smoke A: Start Smoke Agent on Smoke A in webapp: run the tests? [y/N] Started smoke-agent on Smoke A: session c6c14041 in C:\…\a-work\webapp

$ krypto --data-dir <tmpA> --port 47910 sessions
ID        MACHINE  AGENT        SOURCE    STATUS              LAST     TITLE          CWD
c6c14041  Smoke A  smoke-agent  owned     waiting_permission  2s ago   run the tests  C:\…\a-work\webapp
6c36508f  Smoke A  claude-code  cli_hook  idle                26s ago  webapp         C:\…\a-work\webapp
$ krypto --data-dir <tmpA> --port 47910 pending
2dabc3d4  Smoke A  smoke-agent  bun test  allow,always,reject  2s ago        <- the new session's own first request
```

**A repo on more than one machine asks.** sde-brain's routing keeps a repo found in *this* machine's index local ("today's behaviour", see the findings below), so ambiguity is only asked among *other* machines.

The demonstration therefore uses hub D, which has no sessions of its own, paired with A and C. Both have a `webapp` folder with a smoke-agent session in it (C's ran a read and is idle):

```
$ krypto --data-dir <tmpD> --port 47940 repos --machine "Smoke A"     ->  webapp  …\a-work\webapp
$ krypto --data-dir <tmpD> --port 47940 repos --machine "Smoke C"     ->  webapp  …\c-work\webapp

$ krypto --data-dir <tmpD> --port 47940 chat "tell the smoke agent to run the tests in repo webapp"
*barks* That matches more than one place: webapp on Smoke A, webapp on Smoke C. Which one did you mean? Pick one to confirm.

  1) Start Smoke Agent on Smoke A in webapp: run the tests
  2) Send to Smoke Agent on Smoke C (webapp): run the tests

$ krypto --data-dir <tmpD> --port 47940 chat "tell Smoke C's smoke agent to run the tests in repo webapp"
Ruff! Ready: Send to Smoke Agent on Smoke C (webapp): run the tests. It runs on Smoke C, so it needs your OK first: confirm it to go ahead.
  1) Send to Smoke Agent on Smoke C (webapp): run the tests
```

The proposals follow the contract: A's session is waiting on a permission, so A gets a start; C's is idle, so C gets a prompt. Logs: the hub logs of A, B, C and D contain no chat or prompt text. Every hub and agent process was stopped at the end.

## Findings for t13 / sde-brain (not changed by me: `crates/brain` is sde-brain's)

1. **Local-first routing.** With the same repo on this machine and a paired one, the brain runs locally without asking. Its `route` doc says this ("a repo in this machine's index stays local (today's behaviour)"). Your smoke step expected "a repo on both machines asks (2 proposals)"; it asks only between other machines (shown above with hub D).
2. **Stale local repos.** The brain also treats folders of this machine's recent *sessions* as local repos, not just `BrainContext.repos`. After B's webapp folder was deleted, B's index no longer had it, but B's earlier sessions there still made the brain route locally. The local start then failed with "cwd …\b-work\webapp is not an existing directory". Suggestion: treat `ctx.repos[this machine]` (existing folders only) as the authority for local repos.
3. **Orphaned agents (pre-existing, not step 5).** Killing a hub with `taskkill /F` on Windows leaves its ACP agent child processes running; I cleaned them up by hand in the smoke. The agents are spawned by `krypto-acp`; a Windows job object on the hub would let them die with it.

## Decisions for t13 to check

- `krypto chat --pick N` sends the same text again to get fresh proposals, then executes proposal N. Proposals are single-use per reply and nothing is cached across CLI runs. A remote **prompt** proposal is sent without an extra question; the user picked it explicitly, and prompts needed no confirmation in step 4.
- `krypto repos` without a running hub builds this machine's index from hub.db + config, and for `--machine` shows the last index that peer reported (labelled "last known").
- Proposals are capped at 5 on the hub as well (defence in depth; the brain already caps).

## Review fixes (step 5)

1. **`krypto chat --pick` (CLI).**
   - **Asks for both kinds:** a proposal for another machine, start or **prompt**, is confirmed by a person on this terminal (`Run on <machine>: <summary>? [y/N]`). Without a TTY it is refused ("this needs a confirmation on an interactive terminal"), so a script cannot run proposals on another machine.
   - **Consistency check before running** (`check_proposal`):
     - the proposal's machine must be one this hub knows;
     - a start needs a non-blank folder and no session;
     - a prompt needs a session whose `parse_global_id` machine is the proposal's machine.
     Otherwise it is refused and nothing is sent.
   - Test: `proposals_must_add_up_before_they_run`.
2. **Peer-supplied data bounded and sanitized.**
   - **Presence agents** (`repos::validate_peer_agents`, applied in `peers.rs` next to repos):
     - at most 64;
     - id 1-64 chars of `[A-Za-z0-9._-]`;
     - name 1-80 chars, no control characters;
     - invalid agents are dropped before they are kept, persisted, listed (`list_agents{machineId}`) or given to the brain.
     Tests: `repos::peer_agents_are_validated`, and `peer_repos_are_validated_stored_and_listed` (a `../evil` id and an ANSI name are dropped and never persisted).
   - **Mirrored sessions:** `check_session` drops a session whose cwd is over 1024 characters or contains control characters. Test: in `items_must_carry_the_senders_machine_everywhere`.
   - **CLI output:**
     - every table cell goes through `printable` (`print_table`), which covers sessions, pending requests, machines and repos from other machines;
     - the chat reply goes through `printable_lines`, which keeps line breaks and replaces every other control character;
     - proposal summaries, agent ids, session ids and folders in `--pick` output go through `printable`.

After these fixes: `cargo test -p krypto-hub` **128 passed** (twice), and `cargo clippy -p krypto-hub --all-targets -- -D warnings` is clean (forced re-check). The new checks extend existing tests, so the test count is unchanged.
