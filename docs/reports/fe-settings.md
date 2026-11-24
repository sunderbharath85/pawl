# fe-settings: Step 5b (settings in the UI) clients + step 6 relay client bits

Branch `step-5b-settings-ui`. Nothing committed. I edited only my owned paths (`packages/hub-client`, `apps/desktop` incl.
`src-tauri`, `apps/vscode`) plus this report. pet-engine needed no change. I coded against the protocol v7 types, the
CONTRACTS.md "Step 5b" section and t13's two follow-ups (relay `accessKey`; `PeerStatus.relay`, `Device.path`,
relay-only invites). Nothing was run against a real hub; sde-hub builds the hub side in parallel.

## hub-client (`packages/hub-client`)

- **`HubClient`:** `getSettings()`, `updateSettings(patch)`, `hookStatus()`, `installHooks(target, action, {repo})`,
  `openPanel(view)`.
  - `invalid:<field>: <reason>` becomes a **`SettingsError`** (extends `HubRequestError`, `code: "invalid"`,
    `field` = camelCase wire path, `reason`). Other errors keep their code.
  - An empty patch resolves locally when settings are known. `openPanel` rejects an unknown view locally with `invalid`.
- **`HubState`** (optional fields, non-breaking): `settings`, `restartRequired`, `settingsConfigPath`, `protocolVersion`.
  They are filled from the get/update acks and from every `settings` broadcast. `open_panel` changes no state; it reaches
  `onMessage`. Secrets never enter state.
- **New `src/settings.ts`** (pure, all exported):
  - **Field errors:** `parseSettingsError` and `normalizeSettingsField` (snake_case and `.1.` become `customAgents[1].command`),
    `fieldMatches`, `fieldIndex`, `settingsFieldLabel`, `settingsErrorText`.
  - **Client-side checks that mirror the hub:** port 1024–65535, whole numbers, `HH:MM`, listen = IP literal[:port]
    (v4, v6, `[v6]:port`), URLs, absolute folders and duplicates, custom agent id/name/command (built-in ids, unique,
    ≤16 agents). `parseCommandLine` / `formatCommandLine` round-trip, and Windows backslashes stay literal.
  - **Patches:** `editableSettings`, `settingsPatch(base, draft)` (only changed keys), `secretPatch` (write-only
    set/clear for `bot.anthropicApiKey` and `relay.accessKey`), `patchKeys`, `isEmptyPatch`.
  - **`riskyChanges(base, patch, ctx)`:** the confirm text for each risky change. It covers the listener turning on or
    moving (it says which networks), remote control turning on (what paired machines can do), new start folders (agents
    can read and change files there and run commands), and an added custom agent or a changed command (the exact argv
    runs with your account). Removing hooks uses `hookRemoveConfirm`.
  - **Hook labels and actions,** `restartText`, `PANEL_VIEWS` / `isPanelView`.
- **Relay (step 6), in `src/peer.ts`:**
  - `parseInviteUri` accepts relay invites: `addr` is optional when `relay` + `rt` are present, and `rk` is allowed only
    with a relay. The rules mirror `Invite::parse`.
  - `InviteInfo` is now `{machineId, addr: string|null, relay: string|null, expiresAt}`. The code, `rt` and `rk` are never
    returned.
  - New helpers: `inviteRoute`, `relayHost`, `maskInviteSecrets` (rk → dots), `relayStatusText` (wording for every
    `lastError` code), `RELAY_ERROR_TEXT`, `linkPathLabel`.
- **`/node`:** `readHubCredentials` falls back to the top-level `port` in config.toml when `KRYPTO_HUB_URL` is unset. This
  lets clients follow Settings → Hub port. There is a new `urlSource`, plus `configPortFromToml`, `readConfigPort` and
  `hubConfigFile`.
- **Tests:** new `test/settings.test.ts` and more in `peer.test.ts` cover the parsers, checks, patches, risky wording,
  the wire format of all 5 calls, `SettingsError`, state folding, open_panel, secrets kept out of state, config port
  order, relay invites (rk/rt never exposed) and relay status texts. Total: 218.

## Desktop (`apps/desktop`)

**Settings view** (`views/SettingsView.tsx` + `views/settings/*`):
- **Layout:** a sticky section nav, plus one card per section with its own Save / Discard bar.
- **Sections:**
  - **Hub:** port with a "restart needed" chip, event history, idle-session and hook-session timeouts, offline mode,
    the config path.
  - **This machine:** rename (existing).
  - **Machines & pairing:** listener on/off and address, a "Use 192.168.x.y:47801" suggestion, the live state against
    the saved state, the LAN/Relay links, and a link to Machines.
  - **Remote control:** `acceptIntents` and the start-folders list (typed or Browse…).
  - **Repos:** paths list and the live repo index.
  - **Agents & hooks:** per-hook status. Preview shows the exact plan, then you apply it. Remove asks first.
    The custom-agent editor adds, edits and removes agents (command as one line, quoted parts).
  - **Bot:** model, the write-only Anthropic key, Ollama URL and model.
  - **Nudges & reports:** watchdog timings in minutes and seconds, and the daily report toggle and time.
  - **Notifications** (existing).
  - **Relay:** URL; a live status pill and text (connected / connecting / per-code error); the write-only access key
    with t13's note.
  - **On the strip** and **This app:** hub status, data folder, binary, Reconnect / Start hub / Run setup again / Quit.
- **Field-level errors:** client checks run first. A hub `invalid:<field>` lands on the same input, including list
  items such as `peer.startRoots[2]`. An unknown field shows as a section-level message.
- **Restart banner:** "Restart the hub to apply: Hub port." Fields with an `overriddenBy` entry say which flag or env var
  pins them.
- **Secrets (`SecretField`):** shows only Set / Not set. Set and Replace open an empty password field; Clear goes through
  an in-app confirm. A secret is never displayed, prefilled, logged or kept after saving.
- **Notices:** hub not running (with Start hub), too-old hub (< v7) and load errors each have one.

**In-app confirm** (`panel/Confirm.tsx`, provider in PanelApp):
- An alertdialog with focus trapping; Escape and the backdrop cancel, and Cancel has focus first.
- Several risky changes in one save show as one list.
- `useSaveSettings` runs `riskyChanges` before every save. `window.confirm` is never used.

**First-run setup** (`views/SetupView.tsx`, route `setup`):
- Steps: Hub (start it) → Name → Hooks for the agents found (preview → install) → Pair (listener toggle, "Pair a machine
  now") → Done.
- It opens by itself once (`setupOffered` in the desktop prefs; this also shows the panel window) and is reopened from
  Settings → This app.

**`open_panel`:** the panel window handles `onMessage("open_panel")`. It routes to the view and shows the window.
`PanelFocus` also accepts settings / machines / setup and a `section`.

**Machines view:**
- Each online paired machine has a LAN / Relay chip.
- Relay-only invites are accepted, and the preview says "via the relay host".
- Invite creation is allowed when the relay is connected but the listener is off.
- The shown invite and the CLI line mask `rk`; copying still copies the real URI.
- The old "restart with `--peer-listen`" help is now the inline listener toggle (with its confirm) and a link to the
  Relay settings.

**Hub port follows Settings:**
- **Tauri (`hub.rs`):** `hub_http_url()` = `KRYPTO_HUB_URL` > the config.toml top-level `port` > 47800, the same order as
  the hub. `krypto serve` is now spawned **without `--port`** (the hub reads env and config itself; a flag would pin the
  port and lock the setting). New command `lan_address` (std only: a UDP route lookup, no packet sent).
- **`connection.ts`:** keeps a working connection on the old port until it drops, so a saved port change does not cut the
  running hub off.

**Tests:** new `lib/settingsForms.test.ts` covers form ⇄ patch for every section, error keys, relay URL rules (wss
anywhere; ws only on loopback, as in `relay::normalize_url`), custom agents, minutes rounding, and `routeForView`.
Rust gained `reads_top_level_port_from_config`.

## VS Code (`apps/vscode`)

- **`Krypto: Open Settings`** (`krypto.openSettings`, in the palette and the Pet view menu) sends `openPanel("settings")`.
  Without a desktop app it says "Open the Krypto desktop app to change settings." An old hub or a timeout gets clear
  text (`openSettingsErrorText`, tested).
- **Show Machines and the status-bar tooltip** label online machines **LAN** / **Relay**.
- **Credentials** are polled while disconnected (not only when unauthorized), so a new config.toml port is picked up
  after the hub restarts.
- **`Start Hub`** passes `--port` only for an explicit `krypto.hubUrl` setting.
- README updated.

## Commands run (final; all pass)

| Where | Command | Result |
|---|---|---|
| packages/hub-client | `bunx tsc --noEmit` / `bun test` | ok / 218 pass, 0 fail |
| packages/pet-engine | `bunx tsc --noEmit` / `bun test` | ok / 75 pass (no change) |
| apps/desktop | `bunx tsc --noEmit` (+ `-p tsconfig.node.json`, `-p tsconfig.test.json`) | ok |
| apps/desktop | `bun run test` | 42 pass, 0 fail |
| apps/desktop | `bunx vite build` | ok |
| apps/desktop/src-tauri | `cargo check` / `cargo test --lib` | ok / 6 pass |
| apps/vscode | `bunx tsc --noEmit` / `bun test` / `bun run build` | ok / 21 pass / ok |
| apps/mobile | `bunx tsc --noEmit` | ok |

No dependencies were added, removed or upgraded, and no install was run. Line endings match HEAD per file (new files LF).

## Known gaps
- **Not run against a real hub, Tauri window or Extension Host.** The UI was checked by types and unit tests only; there
  are no DOM tests. The hook `plan` text is shown verbatim in a `<pre>`, whatever format sde-hub returns.
- **No restart from the UI.** The banner explains it, but there is no protocol call to stop or restart the hub (see
  decision 1).
- **The Join card's textarea still shows a pasted invite as typed, `rk` included** (it is the user's own paste).
  Everywhere else `rk` is masked.
- **Client-side bounds are generous; the hub has the final say:**
  - hours 1–8760 (idle 0 = never), retention 0–3650 days (0 = forever), watchdog minutes ≤ 1440, tick 1–3600 s.
  - The contract says "durations > 0", but config.toml documents 0 = never / keep forever for those two fields.
  - If sde-hub rejects 0, the field shows its error.
- **Removing a custom agent saves at once, without a confirm.** The contract only lists adding an agent or changing
  its command as risky.

## Decisions for the orchestrator
1. **Restarting the hub from the UI.** Port (and anything else in `restartRequired`) needs a restart that only a
   terminal can do today. Options:
   - a `restart_hub` / `shutdown` ClientMsg (local only);
   - or the desktop restarts a hub it spawned itself. This ends running sessions, and on Windows it can leave agent
     processes behind (the step-5 backlog note).
2. **Port change before the restart.** `krypto hook` (and now the desktop and VS Code, when they reconnect) resolve the
   port from config.toml. Between saving a new port and restarting, hook POSTs go to a port nobody listens on, so those
   hook events are lost until the restart. sde-hub may want `krypto hook` to try the old port as well, or apply the
   port only at restart.
3. **Field names in `invalid:<field>`.** I sent t13 an FYI: clients expect camelCase wire paths (`peer.startRoots[2]`,
   `customAgents[1].command`) and also accept snake_case config paths.
4. **`accept_intents` defaults to true** in the hub's `PeerSection::default`. Remote control is therefore on unless
   turned off, while turning it *on* from the UI needs a confirm. Is that default still wanted?

## Follow-up (t13 decisions 1–4)

1. **Restart hub (`restart_hub`):**
   - New `HubClient.restartHub()`.
   - In the desktop app, "Restart hub" sits in the restart-needed banner and in the Hub section's header.
   - It asks first with an in-app confirm: "Running sessions end; paired machines reconnect." The confirm also gives
     the number of Krypto sessions running on this machine, says that sessions started in a terminal keep running,
     and lists what the restart applies.
   - After the ack, `HubConnection` enters a new `restarting` phase until the new hub says hello. The HubBanner shows
     "Restarting the hub…" with a spinner, and so does the settings banner.
   - The phase also ends after 30 s; if the hub is still not back, a message tells the user to start it.
   - An older hub without `restart_hub` gets "This hub can't restart itself…".
2. **runtime.json first:**
   - The Node helper `readHubCredentials` resolves `KRYPTO_HUB_URL` > `runtime.json` `port` > config.toml `port` > 47800
     (`urlSource: "runtime"`; new `readRuntimePort` / `runtimePortFromJson` / `hubRuntimeFile`). VS Code goes through
     this helper.
   - Tauri `hub_http_url()` uses the same order. `start_hub` re-resolves the URL on each health probe, so it finds the
     port the new hub actually bound.
   - A stale runtime.json after a crash only points at a dead port until Start hub. The new hub then rewrites the file.
3. ok, nothing to change.
4. **`accept_intents` stays default true.** A one-line "what a paired machine can do" now shows above Create invite
   and Join in the Machines view, in the Machines & pairing section and in the setup's pair step.
   - The text follows this hub's remote-control setting (`pairingAllowsText`). With it on, a paired machine sees this
     machine's sessions, pets and approvals and can start agents in allowed folders, prompt, cancel and approve once
     or deny. With it off, it can watch but not act.
   - A link goes to the Remote control settings.

**Tests:**
- hub-client 222: runtime.json order and parsing, the `restartHub` wire format and reconnect, and `pairingAllowsText`.
- Rust 7: `reads_running_port_from_runtime_json`.

**Gap:** the desktop `restarting` phase logic has no unit test, because `HubConnection` depends on a real Tauri and
WebSocket environment.

**Gates rerun, all green:** hub-client tsc + 222; pet-engine tsc + 75; desktop three tsc configs + 42 tests + vite
build; src-tauri `cargo check` + 7 tests; vscode tsc + 21 tests + build; mobile tsc.

## Follow-up 2 (t13 security review fixes 3 and 5)

**3. runtime.json is trusted only after `/health` names this machine.** No token is sent before that check.

The rule is the same everywhere a client reads runtime.json:
- A runtime.json port that differs from the configured port is checked first with `GET /health` (no auth).
- The port is used only when the answer is `ok` and `machineId` equals `id` in machine.toml.
- Otherwise the client falls back to config.toml, then to 47800.
- With no machine.toml, the port is not checked and is never used.
- A runtime.json port equal to the configured port needs no check. `KRYPTO_HUB_URL` wins and skips the check.

Where it applies:
- **hub-client `/node`:**
  - `readHubCredentials` (sync) no longer uses runtime.json at all.
  - New async `resolveHubCredentials()` adds the verified runtime.json step (`urlSource: "runtime"`). Tests can pass a
    fake `fetchHealth`.
  - Other new helpers: `verifiedRuntimePort`, `readMachineId`, `machineIdFromToml`, `hubMachineFile`.
  - `fetchHubHealth` now returns `machineId`.
- **Tauri `hub.rs`:**
  - `hub_http_url()` and the new `hub_health_now()` go through `resolve_hub_url()`. Its port choice is a pure
    `choose_port` helper, unit-tested.
  - `HubHealth` / `HealthBody` carry `machineId`.
  - `machine_id_from_toml` accepts both quote styles and the 12-character id rule.
  - The `hub_health` command and `start_hub` reuse the check's answer, so each probe makes one request.
- **VS Code:** credential lookup is now async (`resolveHubCredentials` without a `krypto.hubUrl` setting), both for
  start and for the disconnected/unauthorized poll. A generation counter drops lookups that finish after a
  stop/restart, and polls never overlap.

Tests:
- hub-client: the sync reader ignores runtime.json; verified → runtime port; another machine id, no id, no answer, not
  ok or no machine.toml → config port; equal, missing or broken file → no check; env wins.
- Rust: `choose_port`, `runtime_verified`, machine.toml parsing, and a real local TCP `/health` server. It checks that
  the machine id is parsed and that the request carries no `Authorization` header.

**5. Very broad folders warn before saving.**
- New `broadFolderReason` / `isBroadFolder` / `broadFolderWarning`. They flag a drive or filesystem root, a share root,
  the users folder (`C:\Users`, `/home`, `/Users`), a home folder, and (with the home folder known) any folder that
  contains it.
- `riskyChanges` gives each broad new start root its own confirm: "Let paired machines start agents anywhere under X?",
  button "Allow anyway". The text says "X is a home folder: any paired machine could start agents anywhere under this
  folder…". The plain start-folder item no longer repeats it.
- A broad new **repo path** now asks too ("Add X as a repo?", "Add anyway"). Ordinary repo paths still save without
  asking.
- The desktop passes the home folder (`@tauri-apps/api/path` `homeDir()`, looked up once) and shows a "very broad" chip on
  such entries in both folder lists.

**Gates rerun, all green:** hub-client tsc + 229; pet-engine 75; desktop three tsc configs + 42 + vite build;
src-tauri `cargo check` + 10 tests; VS Code tsc + 21 + build; mobile tsc.

**Gap:** `homeDir()` relies on `core:default` including the path permissions (Tauri 2). If it is denied, the lookup
returns null and the pattern rules (C:\Users\name, /home/name, /Users/name) still catch the usual home folders.
