# fe-apps: Step 2 (pairing + LAN presence): hub-client, desktop, VS Code

Branch `step-2-pairing-presence`. Nothing committed.

I edited only my owned paths: `packages/hub-client`, `apps/desktop` (incl. `src-tauri`, which needed no change) and `apps/vscode`. I coded against the protocol v3 types and the CONTRACTS.md "Step 2" section, not against a running hub, because sde-hubcore is building the hub side in parallel.

Files I did not touch: the modified `PLAN.md`, `docs/CONTRACTS.md`, `packages/protocol`, `crates/*` and `Cargo.*` files in the tree.

## What changed

### hub-client (`packages/hub-client`)

**State (`src/state.ts`):**
- New fields:
  - `HubState.devices: Device[]` (the pairing roster; initially `[]`).
  - `HubState.peer?: PeerStatus`. It is undefined until a v3 hub says hello.
- `applyWelcome` copies `devices` and `peer`. A pre-v3 Welcome gives `devices: []` and `peer: undefined`.

**Reducers:**
- `devices`: replaces the roster.
- `peer_status`: replaces `peer`.
- `machine_removed`: drops the machine from `machines` and its entry from `devices` (a hub's device id is its machine id). This hub's own machine is never dropped, and an unknown id leaves the state unchanged (same object).
- Acks:
  - `join_peer` adds or replaces the device and machine.
  - `revoke_device` removes them optimistically. The hub's `machine_removed` and `devices` still follow.
  - `create_invite` leaves the state unchanged: the URI is never stored.

**New helpers (shared by both apps, and usable by mobile later):**
- `sortedMachines(state)`: this machine first, then online machines, then by name.
- `machineCounts(state)`: `{total, online}`.
- `removeDevice` and `upsertDevice`.
- `scopeToMachine(state, machineId)`: removes another machine's pets, sessions and pending requests. It returns the same object when nothing is foreign.

**HubClient (`src/client.ts`):**
- `createInvite({ttlSecs?, addr?})` returns `{uri, expiresAt}`.
  - `ttlSecs` must be an integer from 60 to 3600, and `addr` must not contain spaces. Both are checked locally first (code `invalid`). A blank `addr` is left out.
- `joinPeer(uri)` returns `{device, machine}`.
  - It trims the URI and rejects locally with `bad_uri` (wrong shape) or `self_invite` (our own machine id) without sending anything.
  - It waits up to `JOIN_PEER_TIMEOUT_MS` = 30 s, because the hub dials for up to 10 s and then answers `unreachable` / `timeout` itself.
- `revokeDevice(deviceId)`.
- `request()` takes an optional `{timeoutMs}` per call.

**Invite parsing (`src/peer.ts`, new):**
- `parseInviteUri(uri)` checks the URI shape and returns only the public parts `{machineId, addr, expiresAt}`. The code and key are never returned.
- `extractInviteUri(text)` finds the URI whether pasted bare, in quotes, or inside the whole `krypto peer join "<uri>"` line.
- `isUnspecifiedAddr` recognises 0.0.0.0, `::` and `[::]`.
- Constants: `INVITE_TTL_{MIN,MAX,DEFAULT}_SECS` and `INVITE_URI_PREFIX`.
- Nothing logs or stores a URI. Error messages never include it (there is a test for this).

**Tests:** `test/state.test.ts` (v3 reducers and helpers), `test/client.test.ts` (pairing requests), `test/peer.test.ts` (new). The fixture Welcome is now v3.

### Desktop (`apps/desktop`)

**New Machines view** (`src/panel/views/MachinesView.tsx`, route `{view: "machines"}`, sidebar nav item "Machines"):
- **Machine list.** This machine comes first (dashed card, "This machine" chip, "listening on …" or "peer listener off"). Paired hubs follow, each with:
  - an online dot and OS chip,
  - "Online" or "Last seen 5m ago",
  - its address, how long ago it was paired, and its krypto version.
- **When the hub is offline,** peers show "Unknown while the hub is offline" instead of stale online flags.
- **Revoke** uses an in-app inline confirm in the row ("Unpair Mac mini? …" with Keep / Revoke). Keep has focus and Esc cancels. There is no `window.confirm`. A failure shows inline in the row.
- **Roster entries with no machine** (phones, in a later step) still get a row, so they can be revoked.
- **Pre-v3 hub:** a notice says this hub can't pair yet (update krypto).

**Pair a machine: "Invite a machine" card:**
- **Listener off:** the card shows how to turn it on, each line with a copy button:
  - `krypto serve --peer-listen <LAN-IP>`, or
  - `[peer] listen = "192.168.1.20:47801"` in `config.toml`, naming the real data folder from `hub_credentials`.
  - It also notes the firewall, and that only the inviting machine needs the listener.
- **Listener on:**
  - **Address to share** is optional. It is required when the listener binds 0.0.0.0 / `::`, and Create stays disabled until it is filled.
  - **Valid for:** 5 min, 10 min (default), 30 min or 1 h.
- **Invite shown:**
  - the link with a Copy button, and the `krypto peer join "<uri>"` line with its own Copy button;
  - an `expires in m:ss` countdown, which turns amber in the last minute;
  - a note that it works once, until HH:MM;
  - **Hide** (its tooltip says the hub keeps the invite valid) and **New invite**.
- **When the invite ends:**
  - At expiry it is cleared from memory and the card says it expired.
  - When a machine pairs with it, the card clears it and names that machine.

**Pair a machine: "Join a machine" card:**
- A textarea that accepts the URI or the whole CLI line.
- A live preview: the machine (or "already paired; joining again refreshes the link"), the address and the expiry, with a warning if it looks expired by this machine's clock.
- `bad_uri` and `self_invite` show inline as soon as they are pasted. Enter joins.
- While pairing it shows "Dialing … can take up to 10 seconds". On success the field is cleared and a toast says "Paired with Mac mini".

**Friendly text for every code** (`src/lib/pairing.ts` `pairingErrorText`):
- Contract codes: `peer_listener_off`, `invalid` (worded per request), `bad_uri`, `self_invite`, `unreachable`, `invite_rejected`, `timeout`, `not_found`.
- Connection codes: `unauthorized`, `closed`, `disconnected`.
- An unknown code is shown as its name. The request itself is never echoed.

**Invite secrecy** (`InviteHolder` in `src/lib/pairing.ts`):
- The invite lives only in memory: never in localStorage and never logged.
- It survives switching panel views, not a reload.
- A timer clears it at `expiresAt`.

**Sidebar:** a line under the machine tag, "● 3 machines · 2 online", that opens Machines. With only this machine it shows "1 machine · 1 online" and "Pair".

**Remote pets and sessions stay hidden** (as required until step 3):
- `useLocalHubState` = `useHubState` + `scopeToMachine`. Both the panel and the strip use it.
- OS notifications ignore another machine's permission requests, events and hub notifications.

**Tests:** `src/lib/pairing.test.ts` (new): the InviteHolder expiry with fake timers, used-invite detection, every error code's text, the countdown, presence, the sidebar line and the CLI line.

`src-tauri` needed no change.

### VS Code (`apps/vscode`)
- **Status-bar tooltip:**
  - Below the machine header it shows "3 machines · 2 online · [Machines]".
  - Then one line per paired machine: ● or ○, the name, the OS, and "online" or "offline · last seen 14:05". Absolute times are used so a cached tooltip never goes stale.
  - With no peers it shows "No other machines paired yet".
  - The render key includes each machine's online state, so the tooltip updates when a machine flips.
- **New command "Krypto: Show Machines"** (`krypto.showMachines`, read-only QuickPick):
  - Rows list this machine first, then the others, with OS, presence, address and version. They update live while the picker is open.
  - The title is "Krypto: 3 machines · 2 online", and the placeholder says pairing happens in the desktop app or with `krypto peer`.
  - Choosing a row just closes the picker.
  - Items are built by the pure `src/host/machineList.ts`, which has tests.
  - The command is registered in `package.json` and in the view's `...` menu (`1_pet@5`).
- **Webview header:** a "3 machines · 2 online" link opens Show Machines (shown once something is paired). There is no pairing UI in VS Code.
- **Remote work stays hidden:**
  - `HubConnection.state` (and the state passed to listeners and the webview) is `scopeToMachine`'d, memoized on the client's state object.
  - The notifier skips another machine's permission requests and hub notifications.
- **Other:** `emptyHubState` and the webview's `EMPTY_STATE` gained `devices: []`. `shared/machine.ts` gained `machinesLine` and `presenceLabel`. The README is updated.

## Commands run (final; all pass)

| Where | Command | Result |
|---|---|---|
| packages/hub-client | `bunx tsc --noEmit` | ok |
| packages/hub-client | `bun test` | 106 pass / 0 fail (4 files) |
| apps/desktop | `bunx tsc --noEmit` | ok |
| apps/desktop | `bunx tsc --noEmit -p tsconfig.node.json` | ok |
| apps/desktop | `bunx tsc --noEmit -p tsconfig.test.json` | ok |
| apps/desktop | `bun test src` | 34 pass / 0 fail (4 files) |
| apps/desktop | `bunx vite build` | ok |
| apps/desktop/src-tauri | `cargo check` | ok |
| apps/vscode | `bunx tsc --noEmit` | ok |
| apps/vscode | `bun run build` | ok (extension, webview, pets) |
| apps/vscode | `bun test` | 30 pass / 0 fail (5 files) |
| packages/pet-engine | `bunx tsc --noEmit` | ok |
| apps/mobile | `bunx tsc --noEmit` | ok. No type fix needed: it builds its state with `initialHubState()`, which now includes `devices`. |

No dependencies were added, removed or upgraded, and no install was run.

Line endings are preserved: CRLF files (Sidebar, PetApp, panel.css) stay CRLF and new files are LF. `PLAN.md`, `docs/CONTRACTS.md`, `packages/protocol/src/index.ts` and `crates/protocol/src/lib.rs` show `i/crlf w/lf` in the tree. Those are not my edits.

## Known gaps
- **Not run against the real hub** (it is being built in parallel), and no Tauri window or Extension Development Host was launched. The layout has only been checked by reading the code and CSS.
- **Copy buttons:**
  - They use `navigator.clipboard.writeText`, falling back to a hidden textarea + `execCommand("copy")`. If both fail, the button says "Select to copy" and the text can be selected with one click.
  - I did not add `tauri-plugin-clipboard-manager` (no new dependencies).
  - Not checked in WebView2 or WKWebView.
- **Used-invite detection is a heuristic.** The wire does not say who invited whom (`PeerEvent::Paired.we_invited` never reaches clients).
  - The client treats an invite as used when a device appears that was not in the roster when the invite was made, or when a known device's `pairedAt` is after the invite was created.
  - A machine this hub joined itself while the invite was showing is excluded.
  - Rare miss: a re-pair whose `pairedAt` the hub does not update would leave the invite showing until it expires.
- **Hide only hides.** There is no `cancel_invite`, so a hidden invite stays valid on the hub until it expires or is used. The Hide button's tooltip says so.
- **Pairing UI only exists in desktop**, as the contract says. The tray can't open the Machines view directly yet (`PanelFocus` has no `machines` view).
- **Phone devices** (`kind: "phone"`) get a generic row on desktop and a generic item in VS Code. Untested until step 7.

## Decisions for the orchestrator
1. **Remote pets, sessions and approvals are filtered out on the client.** Both apps do it, and OS/VS Code notifications for other machines are skipped. A hub that forwards them early can't put them on screen. In step 3, remove `useLocalHubState` (desktop: PanelApp and PetApp) and `HubConnection.local` (VS Code), and the `isRemote` / `remote()` notifier guards.
2. **`machine_removed` also drops the device** with that id from `devices`. The roster is consistent at once instead of waiting for the `devices` push.
3. **The client pre-checks the invite shape** before `join_peer`. It requires `v=1`, a 12-character `mid` of [a-z0-9], base64url `pk` and `code`, a non-empty `addr` and a numeric `exp`. Please have sde-peer confirm `Invite::to_uri` emits exactly that. If the Rust parser is looser, the client could reject a URI the hub would accept.
4. **`join_peer` waits 30 s on the client.** A normal request waits 15 s, but the hub's own 10 s dial timeout plus the handshake could come close to that.
5. **Optional hub additions** that would make the UX exact:
   - (a) `cancel_invite`, so Hide can revoke an invite.
   - (b) a `paired` ServerMsg (or a `weInvited` flag), so "your invite was used" is not inferred from roster changes.
