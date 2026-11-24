# Krypto — Plan v2.0 (multi-machine)

Named dog pets that watch and drive your coding agents over ACP (Agent Client Protocol), on every machine you code on.

- Whiteboard: https://claude.ai/artifact/E8zVPktys5q8qKw5AjmjtB
- Last updated: 2026-11-23
- Status: migration path approved 2026-11-23. Done: step 0 (baseline + security floor), rename to Krypto, step 1 (machine identity). Next: step 2 (pairing + LAN presence).

## Decisions

- **One hub per machine.** Example: a Mac mini and a Windows PC. Coding agents always run on the machine that has the code, and a hub never runs another machine's agents.
- **The owning hub is the single source of truth** for its sessions. Other screens (the other machine's desktop, VS Code, the phone) send *intents*. The owner applies them and broadcasts the result.
- **No inbound ports.** Hubs and the phone connect outbound to a relay over WebSocket. A direct LAN or Tailscale link between paired hubs is an optional fast path.
- **End-to-end encryption.** Every hub↔hub and hub↔phone payload is encrypted; the relay only routes ciphertext.
- **The dog is the default pet.** There is one pet per agent per machine. Cat, bird and robot are optional skins (art and SVG already exist; keep them, low priority).
- **Every pet has a name.** It is auto-picked, unique across paired machines and user-editable. It shows above the pet everywhere and can be used in commands ("tell Biscuit to run the tests").
- **The editor is VS Code.** VS Code has no native ACP support, so VS Code sessions are tracked through agent hooks plus the "Krypto for VS Code" extension. A Zed/JetBrains wrapper is optional, later.
- **TypeScript toolchain: bun.** The hub is Rust, the desktop app is Tauri 2 + React, the phone app is Expo, and the relay runs on Cloudflare Workers + Durable Objects.

## Goal

Cross-platform AI pets that, across all your machines:

- connect to the coding agents installed on each machine over ACP
- track every session and its status, on whichever screen you're using
- send notifications and let you approve from anywhere, safely
- start and prompt sessions on the right machine and repo
- act as chat bots

## Key constraint

Local ACP agents run as subprocesses of the client and talk JSON-RPC over stdio, and ACP's remote transport is still a draft RFD. So each machine's **hub** owns that machine's ACP connections. Pets, other machines and the phone are views and intent senders on top of the hubs.

## Topology

```
 Mac mini                                             Windows PC
 ┌───────────────────────┐                            ┌───────────────────────┐
 │ desktop dog, VS Code  │                            │ desktop dog, VS Code  │
 │   │ ws://127.0.0.1    │◄── optional LAN/Tailscale ─►│   │ ws://127.0.0.1    │
 │ hub (ACP + hooks)     │      direct link (E2E)      │ hub (ACP + hooks)     │
 └──────────┬────────────┘                            └──────────┬────────────┘
            │ outbound WSS (E2E frames)                           │ outbound WSS
            └────────────────►  relay (routes only)  ◄────────────┘
                                      ▲ outbound WSS
                                    phone
```

- Local clients (desktop dog, VS Code) talk only to their own machine's hub on `127.0.0.1`.
- Each hub keeps a **read-only mirror** of paired machines' sessions and pets (fed by `event` messages), so local clients see visitors without new connections.
- Intents that local clients send for a remote session are forwarded to the owning hub, chosen by the `machineId` prefix of the id.
- The phone keeps an E2E channel to each hub and sends intents straight to the owner.

## Architecture (per machine)

- **Hub (background daemon, Rust):**
  - ACP client, agent discovery, hook endpoint, SQLite (event log, audit log), rules engine, bot brain
  - machine identity
  - peer link (presence, event, intent and ack messages over the LAN link and/or the relay)
  - localhost API (HTTP + WebSocket) on `127.0.0.1` only
- **Desktop dog (Tauri 2):** a transparent, always-on-top, click-through strip.
  - Your own dogs walk on the left; a **visitors** area on the right shows other machines' dogs, labelled "Biscuit · Mac mini".
  - When any dog's session needs approval, that dog runs onto whichever screen you're using (see Pet engine).
  - It also has a panel window for sessions, the feed, chat, approvals, pets and rules.
- **Krypto for VS Code (TypeScript extension):** shows pets in the status bar and a webview, handles approvals and commands, and opens edited files or diffs.
  - It talks to its own machine's hub only; remote sessions show up through the hub's mirror.
- **Phone app (Expo):** pairs by QR, gets push notifications, approves with biometrics, starts sessions on machine + repo, and chats with pets.
- **Relay (Cloudflare Worker + Durable Object per pairing group):** routes encrypted frames between hubs and phones, and sends content-free push wakes (APNs/FCM via Expo).

## Agent discovery (per hub)

- Pull the ACP Registry: https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json (24 h cache, with built-in fallbacks).
- Check what's installed: PATH (with Windows PATHEXT), npx/uvx, or a cached binary. Custom agents come from config.
- Claude Code connects through `@agentclientprotocol/claude-agent-acp`, Codex through `codex-acp`. Gemini CLI, Copilot CLI and others come from the registry.
- Each hub publishes its agent list in `presence`.

## Session feeds (per hub, merged by that hub)

1. **Owned sessions:** started by the hub with `session/new` and `session/prompt`. The hub gets the full `session/update` stream: thoughts, messages, `tool_call` and `tool_call_update`, `plan`, `request_permission`, and stop reasons.
2. **VS Code:**
   - Claude Code hooks in `~/.claude/settings.json` (Windows: `%USERPROFILE%\.claude\settings.json`).
   - Copilot agent hooks in `.github/hooks/*.json`; Copilot CLI hooks in `~/.copilot/hooks/*.json`.
   - Codex hooks or `notify`.
   - Sessions in the vendors' own extensions can alert you, but their approvals stay in VS Code.
3. **Terminal CLIs, including VS Code's integrated terminal:** Claude Code and Codex hooks POST to the hub on localhost through `krypto hook <source>`.
4. **History:** `session/list` and `session/resume` (falling back to `session/load`), used only when the agent advertises the capability.

## Ownership and intents

- **Global ids** are `${machineId}:${localId}` for sessions, pets, permission requests and chat. Events use `${machineId}:${seq}`.
  - `machineId` is a random 12-character `[a-z0-9]` string, generated once per hub and stored in the data dir. It never contains `:`.
  - Ids on the wire are always global. A hub accepts an id with its own prefix and forwards any other.
- **Intents:** `approve`, `deny`, `prompt`, `start`, `cancel`.
  - The owner validates and applies each intent, writes the audit log, then broadcasts the resulting `event`.
  - The sender gets `ack{applied}` or `ack{rejected, reason}`.
- **The first valid approval wins.** Later approvals for the same request get `ack{rejected, reason:"already_decided"}`, and their bubbles close on the next event.
- **Remote approvals** need biometrics (phone) or an explicit confirmation on the device you approve from (desktop/VS Code: a confirm step for sessions owned by another machine).

## Hub ↔ hub, hub ↔ phone, and relay messages

Every frame is an envelope `{v, from, to, seq, nonce, box}`:

- `from` and `to` are device ids (a hub's id is its `machineId`).
- `box` is a NaCl box (X25519 + XSalsa20-Poly1305) of the JSON body.
- `seq` is per sender, per peer, for replay protection.

The relay sees only the envelope. Body types:

| Type | Direction | Body |
|---|---|---|
| `presence` | hub → peers (heartbeat 15 s; offline after 45 s) | `machine` (id, name, os), `agents`, `repos`, `pets`, `userActiveAt`, `ts` |
| `event` | owner → peers/phones | `session`, `event`, `pet`, `permission_request`, `permission_resolved`, `notification` updates (each carries `machineId`) |
| `intent` | any → owner | `{intentId, by: deviceId, kind: approve\|deny\|prompt\|start\|cancel, target, args, confirmed: bool}` |
| `ack` | owner → sender | `{intentId, status: applied\|rejected, reason?: already_decided\|not_found\|not_owner\|forbidden\|unconfirmed\|invalid, result?}` |

## Data model

```ts
Machine { id, name /* "Mac mini" */, os, lastSeen, online }
Device  { id, kind: 'hub' | 'phone', name, publicKey, pairedAt, lastSeen }          // pairing roster
Agent   { id, name, command, version, capabilities, installed }                     // per machine
Session { id /* global */, machineId, agentId, cwd, source: 'owned' | 'vscode_hook' | 'cli_hook' | 'wrapper',
          status: 'idle' | 'thinking' | 'working' | 'waiting_permission' | 'done' | 'error', startedAt, lastEventAt }
Event   { id /* machineId:seq */, machineId, sessionId, type, payload, ts }
Pet     { id /* global */, machineId, agentId, species /* default 'dog' */, skin, name, personality, mood, xp }
PermissionRequest { requestId /* global */, machineId, sessionId, agentId, toolCall, options, createdAt }
Rule    { match: { toolKind?, path?, agent? }, action: 'auto_allow' | 'ask' | 'deny' }   // hard floor below
```

## Pet engine

One state machine with four skins, and one pet per agent per machine. Every pet shows its name.

| ACP event | What the dog does |
|---|---|
| No active sessions | Naps in its bed |
| `agent_thought_chunk` | Tilts its head, ears up |
| `tool_call` (edit/execute) | Digs and fetches |
| `plan` | Carries a checklist |
| `request_permission` | Barks, runs to you and shows an Approve/Deny bubble |
| `end_turn` | Wags its tail, zoomies |
| Error or refusal | Droops and whines |

- **Visitors:** other machines' pets show in the visitors area of each desktop strip, labelled with their machine name. They are greyed out when that machine is offline.
- **"Runs to you":** each hub reports `userActiveAt` in presence, which is the last time you used that machine. The desktop app already polls the cursor, and the phone counts while its app is in the foreground.
  - The dog whose session needs approval appears on the most recently active screen: as a visitor if it's remote, and on the monitor that has the cursor.
  - Other screens show a badge only.
- **Names:** the default name is seeded from `machineId + agentId`, with collisions across paired machines avoided. Names are editable in the desktop, VS Code and phone, and are shown by default.
- **Art:** Gemini-generated sprite strips in `assets/pets/` (all 4 species, 9 animations each), with SVG fallbacks.

## Bot behavior

- **Ask:** "What's the Mac mini's Claude doing?" gets a summary of the live session(s) on that machine.
- **Command:** "Tell the Mac mini's Claude to run the tests in repo X" (or "tell Biscuit …") is resolved in steps:
  1. Resolve the machine (by name, or by the pet name's machine).
  2. Resolve the agent.
  3. Resolve the repo from that hub's **repo index**: session cwds, configured project roots, and VS Code workspace folders reported by the extension, published in presence.
  4. Send a `start` or `prompt` intent to the owning hub.
  - If anything is ambiguous, the bot asks you to choose.
- **Watchdog:** nudges you when an agent has been waiting on you longer than N minutes, and sends an end-of-day report across machines.
- **Rules:** auto-approve safe reads only inside the session's cwd, and always ask for sensitive files (`.env`, keys, `.ssh`). See the hard floor under Security.
- **Brain:** a small, cheap or local model (default `claude-haiku-5-5`, Ollama optional) for chatter and summaries. Actions only ever come from the deterministic parser.
- **Later:** the same bots on Slack, Telegram and Discord, and an MCP server so agents can message the pet.

## Security

- **Pairing:**
  - Hub ↔ hub pairs with a one-time code or URI; the phone pairs by QR.
  - Each device has a long-term X25519 key, and pairing exchanges the public keys.
  - Pairing a new device with any hub introduces it to the whole group roster.
  - Devices can be revoked.
- **Approvals:**
  - Remote approvals need biometrics or confirmation on the approving device.
  - **Hard floor:** `execute`, `delete` and `fetch` (network) are never auto-approved, even if a user rule says so. Such rules are rejected when saved.
- **Network:**
  - The local API binds `127.0.0.1` only, with a bearer token and an Origin allowlist.
  - The optional peer fast-path listener is separate and off by default. It accepts only E2E frames from paired keys and never exposes the local API.
  - The relay sees only routing ids and ciphertext.
- **Audit log:** every approval or denial is logged with who, which device, which machine, what, when and the outcome.

## Stack

- **Hub:** Rust, using the official `agent-client-protocol` crate (5.x), axum 0.8, rusqlite and tokio.
- **Desktop:** Tauri 2 + React 19 + Vite. **VS Code:** a TypeScript extension bundled with bun. **Phone:** Expo SDK 58 (Expo Router).
- **Shared TS packages:** `@krypto/protocol` (wire types), `@krypto/hub-client`, `@krypto/pet-engine`.
- **Relay:** Cloudflare Workers + Durable Objects, with push through the Expo Push API (APNs/FCM).

## Repo layout (actual)

```
ai-pet/                  # repo folder (name kept after the Krypto rename)
  apps/desktop/          # Tauri 2 shell + React (pet strip + panel + tray)
  apps/vscode/           # Krypto for VS Code
  apps/mobile/           # Expo app (scaffold + relay/crypto libs; screens not built)
  crates/protocol/       # krypto-protocol: Rust mirror of the wire types
  crates/acp/            # krypto-acp: ACP client host (spawns agents, permission flow)
  crates/discovery/      # krypto-discovery: registry + install detection + hook normalize/forward/install
  crates/brain/          # krypto-brain: rules, chat intents, watchdog, pet growth
  crates/hub/            # krypto-hub: daemon + `krypto` CLI (SQLite, core, axum server)
  packages/protocol/     # @krypto/protocol: TS wire types (source of truth)
  packages/hub-client/   # @krypto/hub-client: WS client + state (+ /react, /node)
  packages/pet-engine/   # @krypto/pet-engine: activity logic + SVG/sprite pets (+ /react)
  assets/pets/           # sprite strips + manifest.json (dog, cat, bird, robot)
  scripts/art/           # Gemini sprite pipeline
  docs/CONTRACTS.md      # interface contracts (v1; to be revised in migration step 1)
  docs/reports/          # per-workstream build reports
```

## Status (2026-11-23, single-machine v1)

| Area | State |
|---|---|
| Hub daemon + CLI (`serve`, `agents`, `sessions`, `start`, `hook`, `install-hooks`, `token`) | Built. 29 tests; smoke-tested binary |
| ACP host (owned sessions, permissions, history, fs sandbox, process-tree cleanup) | Built. 22 tests; not yet run against a live agent |
| Discovery + hooks (Claude/Codex/Copilot normalize, forward, install) | Built. 31 tests |
| Rules, bot, watchdog, growth | Built. 51 tests (cwd-scoped reads, sensitive-path guard) |
| hub-client, pet-engine | Built. 53 + 39 tests |
| Desktop app | Built. Typecheck, `cargo check` and `vite build` pass; not launched |
| VS Code extension | Built. Typecheck and build pass; not run in an Extension Development Host |
| Pet art | Done: 4 species × 9 animations |
| Phone app | Scaffold + relay/crypto/pairing libs only. Frozen for the redesign |
| Relay | Not started. Held for the redesign |

## Migration path (approved 2026-11-23)

Each step can ship and be tested on its own.

0. ✅ **Baseline.** (done 2026-11-23) `git init` and a first commit of the v1 tree. Run every gate. Do one live smoke run of `krypto agents` and `krypto start claude-code` on the Windows PC. Also enforce the rules hard floor: execute, delete and fetch are never auto-allowed, and `set_rules` rejects rules that try.
1. ✅ **Machine identity (one hub, no networking).** (done 2026-11-23; protocol v2)
   - Add `Machine` and `machineId` to the protocol (TS + Rust) on Session, Event, Pet, PermissionRequest, Notification and Chat.
   - Make wire ids global. Run a store migration that adds `machine_id` and backfills the local machine.
   - Add `krypto machine [--name "Windows PC"]`.
   - Seed pet names from `machineId + agentId`.
   - Clients key everything by global id. The pet engine matches pets to sessions by `(machineId, agentId)`. Desktop shows names by default.
   - Test: unit tests, plus the existing desktop and VS Code working against one hub with no visible change except names.
2. **Pairing + presence over the LAN (no relay).**
   - Long-term device keys. `krypto peer invite` prints a one-time URI; `krypto peer join <uri>` on the other hub.
   - An opt-in peer listener on the LAN or Tailscale interface carries encrypted `presence` heartbeats both ways.
   - Hubs show `machines` (online, lastSeen) in Welcome, the CLI (`krypto machines`) and the desktop panel.
   - Test: two hubs on one PC (separate data dirs and ports), then the Mac mini and the Windows PC.
3. **Mirror events + visitors.**
   - Owners stream `event` messages to peers, and peers keep a read-only mirror.
   - Desktop and VS Code render a visitors area and remote sessions read-only.
   - Presence carries `userActiveAt`, and the approval dog "runs to you" on the active screen.
   - Test: a session on the PC shows as a visitor dog on the Mac.
4. **Intents + acks.**
   - Approve, deny, prompt, start and cancel are routed to the owning hub.
   - First valid approval wins (`already_decided`).
   - Remote approvals need a confirmation step on desktop and VS Code.
   - The audit log records origin device and machine.
   - Test: approve a PC session from the Mac; a double approval gets rejected.
5. **Remote commands by machine + repo.**
   - Repo index in presence.
   - The brain parses machine and pet-name targets and routes `start`/`prompt` intents.
   - The bot asks when the target is ambiguous.
   - Test: "Tell the Mac mini's Claude to run the tests in repo X" from the PC.
6. **Relay.**
   - A Cloudflare Worker routes the same envelopes. Hubs connect outbound.
   - The LAN link becomes a preferred fast path with relay fallback.
   - Push wakes.
   - Test: approval with the LAN link disabled.
7. **Phone.**
   - QR pairing joins the group, with per-hub E2E channels and a merged multi-machine view.
   - Biometric approvals, push, and start-on-machine+repo.
   - This reuses the frozen `apps/mobile` libs, adapted to multiple hubs.
8. **Later:** cat, bird and robot polish, moods and XP, chat-app bots, an MCP server, and the Zed/JetBrains wrapper.
