# Krypto

Named dog pets that watch and drive your coding agents over ACP ([Agent Client Protocol](https://agentclientprotocol.com/)).

![Krypto's dog, idle animation frames](assets/pets/dog/idle.png)

*Placeholder: these are the dog's idle sprite frames. A screenshot of the desktop strip and VS Code view will replace this.*

## What it is

Krypto runs one **hub** per machine. The hub connects to the coding agents installed on that machine (Claude Code, Codex, Gemini CLI and others) and tracks every session it can see. Each agent gets a pet, a dog by default, with a name.

The pet shows what its agent is doing. It naps when nothing is running, digs while the agent edits or runs tools, and barks when the agent needs your approval. You can approve or deny from the pet, start a session in a repo, send a prompt, or ask the pet what its agent is doing.

You see the pets in three places:

- **Desktop dog:** a Tauri app with a transparent, always-on-top strip where the dogs walk, plus a panel for sessions, the event feed, chat, approvals, pets and rules.
- **Krypto for VS Code:** pets in the status bar and a webview, approvals, commands, and opening the files an agent edited.
- **Phone:** planned, not built yet (see [Status](#status)).

The goal is to cover all of your machines: a session on a Windows PC shows up as a visiting dog on a Mac, and you can approve it from either one or from the phone. That part is in progress; today Krypto works on a single machine.

## How it works

```
  desktop dog      VS Code extension
       \               /
        ws://127.0.0.1:47800 (bearer token)
              |
        hub (krypto serve)  ---- ACP over stdio ---->  agents it starts
              ^                                        (Claude Code, Codex, ...)
              |
        krypto hook <source>  <---- hooks ----  agents you run yourself
                                                (terminal, VS Code)
```

- **The hub is an ACP client.** ACP agents run as subprocesses of the client and talk JSON-RPC over stdio. When you start a session through Krypto, the hub spawns the agent and gets its full stream: thoughts, messages, tool calls, plans, permission requests and stop reasons.
- **Hooks cover the sessions you start yourself.** Claude Code, Codex and Copilot can call a command on hook events. `krypto install-hooks` adds `krypto hook <source>` to their config; it POSTs the hook JSON to the hub and gives up silently within 2 seconds if the hub is down, so it never blocks the agent. Approvals for these sessions stay in the agent's own UI; Krypto shows their status.
- **Clients talk only to their own machine's hub,** over HTTP and WebSocket on `127.0.0.1`.
- **Discovery** reads the [ACP registry](https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json) (cached for 24 hours, with a built-in fallback list) and checks what is installed on `PATH`, through `npx`/`uvx`, or as a cached binary. Claude Code is reached through the `@agentclientprotocol/claude-agent-acp` adapter and Codex through `codex-acp`.
- **Multi-machine (in progress):** hubs on the same LAN or Tailscale network can already pair over an end-to-end encrypted link. Each hub keeps a read-only copy of the other machines' sessions and pets: their dogs visit your screen, an approval dog runs to the machine you are using, and notifications appear only there. You can approve (after a confirmation on your device), deny, prompt, cancel and start sessions on another machine; the owning machine decides and logs it. The bot understands "tell the Mac mini's Claude to run the tests in repo api" and proposes it for you to confirm. Next: every setting in the app, then a relay so hubs connect from anywhere. Every hub-to-hub and hub-to-phone payload will be end-to-end encrypted, so the relay only routes ciphertext. A direct LAN or Tailscale link between paired hubs will be an optional fast path. No machine will need an inbound port. See [PLAN.md](PLAN.md).

## Status

Krypto is pre-1.0. Single-machine v1 is built; multi-machine support is being added in the steps listed under "Migration path" in [PLAN.md](PLAN.md).

| Area | State |
|---|---|
| Hub daemon and `krypto` CLI | Built, unit-tested |
| ACP host (owned sessions, permissions, history, file access limited to the session folder) | Built, unit-tested; live-agent testing in progress |
| Agent discovery and hooks (Claude Code, Codex, Copilot) | Built, unit-tested |
| Rules, bot, watchdog, pet growth | Built, unit-tested |
| `@krypto/hub-client`, `@krypto/pet-engine` | Built, unit-tested |
| Desktop app | Builds and typechecks; not yet tested end to end |
| VS Code extension | Builds and typechecks; not yet tested end to end |
| Pet art | Done: dog, cat, bird and robot, 9 animations each |
| Multi-machine (PLAN.md migration steps 1–7) | In progress. Done: step 1 (machine identity: global ids, machine names, pet names), step 2 (pairing + presence over the LAN) step 3 (mirror of other machines, visitor dogs, "runs to you") step 4 (approve, prompt, cancel and start on another machine) and step 5 (commands by machine, repo and pet name). Next: settings in the app, then the relay |
| Relay | Not built |
| Phone app | Not built. Only a scaffold and the relay/crypto/pairing libraries exist |

## Quickstart (from source)

You need:

- Rust (stable, via [rustup](https://rustup.rs/))
- [bun](https://bun.sh/) 1.4 or later, for the TypeScript packages and apps
- Node.js, so the hub can run ACP adapters through `npx`
- An agent that is installed and signed in, for example Claude Code
- For the desktop app: the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your OS

Commands below use the Git Bash / macOS / Linux form `./target/debug/krypto`. In PowerShell, use `.\target\debug\krypto.exe`.

### 1. Build the hub and CLI

```sh
cargo build -p krypto-hub
./target/debug/krypto --help
```

### 2. See which agents Krypto found

```sh
./target/debug/krypto agents
./target/debug/krypto agents --refresh   # re-fetch the ACP registry instead of using the 24 h cache
./target/debug/krypto agents --json
```

### 3. Start a session

```sh
./target/debug/krypto start claude-code --cwd . --prompt "Summarize this repository"
```

This streams the session's events to your terminal. Without `--prompt` you get an interactive prompt loop. If a hub is running, the session runs inside the hub (so the desktop dog and VS Code see it); otherwise it runs in this process. `--local` forces the in-process mode.

### 4. Run the hub

```sh
./target/debug/krypto serve
```

The hub listens on `127.0.0.1:47800` only. `--port` changes the port. On first start it creates its data directory with a bearer token (`krypto token` prints it), `config.toml`, `hub.db` (SQLite) and `hub.log`:

| OS | Data directory |
|---|---|
| Windows | `%APPDATA%\krypto` |
| macOS | `~/Library/Application Support/krypto` |
| Linux | `~/.local/share/krypto` |

`--data-dir` or `KRYPTO_HUB_DATA_DIR` points it somewhere else. `krypto sessions` lists open and recent sessions.

### Optional: pair two machines (LAN or Tailscale)

The hub-to-hub listener is off by default. Turn it on for the machine that creates the invite, using that machine's LAN or Tailscale IP (default port 47801), or set `[peer] listen = "ip:port"` in `config.toml`:

```sh
./target/debug/krypto serve --peer-listen 192.168.1.20
./target/debug/krypto peer invite            # prints a one-time URI (valid 10 min, --ttl to change)
```

On the other machine, with its hub running:

```sh
./target/debug/krypto peer join "krypto://peer?v=1&..."
./target/debug/krypto machines               # both machines, online state, last seen
```

`krypto peer revoke <machine id or name>` unpairs a machine on both sides. The desktop panel's Machines view does the same with buttons. Treat an invite URI like a password until it is used or expires.

### 5. Track the agent sessions you start yourself

```sh
./target/debug/krypto install-hooks claude            # dry run: prints the change to ~/.claude/settings.json
./target/debug/krypto install-hooks claude --apply    # writes it (backs up the file first)
```

Targets: `claude`, `codex`, `copilot-cli`, `copilot-vscode` (`--repo <dir>` picks the repository; default is the current directory). The hook calls the `krypto` binary you ran this from, by absolute path, so install hooks from a binary that will stay where it is.

### 6. Desktop dog

```sh
bun install                        # once, at the repository root
cd apps/desktop && bun run tauri:dev
```

The app reads the hub token from the default data directory (or `KRYPTO_HUB_TOKEN`). If no hub is running it can start `krypto serve` itself, using the binary on `PATH` or, in a dev build, the one in this repository's `target/` folder, so build the hub first (step 1).

### 7. Krypto for VS Code

```sh
bun install                        # once, at the repository root (skip if done in step 6)
cd apps/vscode && bun run build
```

Then open the `apps/vscode` folder in VS Code and press F5 ("Run Krypto for VS Code") to start an Extension Development Host. If the hub is not running, the status bar offers **Start hub**. See [apps/vscode/README.md](apps/vscode/README.md) for settings and commands.

### Optional: the bot's language model

The pet's chatter and session summaries can use a small model. Set `ANTHROPIC_API_KEY` in the hub's environment (default model `claude-haiku-5-5`), or point `config.toml` at a local Ollama model. Without either, the bot uses deterministic replies. Actions such as starting a session or approving a request only ever come from a deterministic parser, never from the model.

## Repository layout

```
apps/desktop/          Tauri 2 + React desktop app (pet strip, panel, tray)
apps/vscode/           Krypto for VS Code
apps/mobile/           Expo phone app (scaffold + relay/crypto libraries; screens not built)
crates/protocol/       krypto-protocol: Rust mirror of the wire types
crates/acp/            krypto-acp: ACP client host (spawns agents, permission flow)
crates/discovery/      krypto-discovery: ACP registry, install detection, hook normalize/forward/install
crates/brain/          krypto-brain: permission rules, chat intents, watchdog, pet growth
crates/peer/           krypto-peer: device keys, pairing invites, encrypted hub-to-hub links and presence
crates/hub/            krypto-hub: the hub daemon and the `krypto` CLI (SQLite store, axum server)
packages/protocol/     @krypto/protocol: TypeScript wire types (source of truth)
packages/hub-client/   @krypto/hub-client: WebSocket client and state (+ React and Node helpers)
packages/pet-engine/   @krypto/pet-engine: pet activity logic and SVG/sprite pets (+ React)
assets/pets/           sprite strips and manifest.json (dog, cat, bird, robot)
scripts/art/           sprite generation pipeline
docs/CONTRACTS.md      interface contracts between the parts above
```

## Security model

Approving an agent's request can run commands on your machine, so the defaults are strict.

- **Local API:** the hub binds `127.0.0.1` only. Every WebSocket connection and hook POST needs the bearer token from the data directory (32 random bytes, created on first start). WebSocket connections that carry a browser `Origin` are refused unless it is the Tauri app, a VS Code webview, or the desktop app's Vite dev server, so a web page you visit cannot drive the hub.
- **Approval rules:** the first matching rule wins, and no match means *ask*. By default only reads and searches inside the session's working folder are auto-approved.
- **Hard floor:** commands (`execute`), deletes, network access (`fetch`) and unknown tool kinds are never auto-approved, whatever the rules say. Rules that try are rejected when saved.
- **Sensitive paths always ask:** `.env*`, keys and certificates, `.ssh`, `.aws`, credential files, the hub's own token and similar paths are never auto-approved. A deny rule still denies.
- **File access:** the ACP file read/write methods the hub serves are limited to the session's working folder. The agent process itself runs with your user's permissions, as it would without Krypto.
- **Audit log:** every approval and denial is recorded in `hub.db` with who decided, what, when and the outcome.
- **Pairing and peer links:** each hub has a long-term X25519 key (`device.key` in the data directory, never logged). Pairing uses a one-time invite (single use, expires in at most an hour) that carries the inviting hub's public key, so a machine in the middle cannot answer. Every frame is a NaCl box (X25519 + XSalsa20-Poly1305), and its sequence number and direction are bound into the nonce, so frames cannot be replayed or reflected. The peer listener is off by default, serves only `/peer`, refuses browser requests and unknown or revoked keys, and limits connections and pairing attempts per IP. Peers can only send presence and a read-only copy of their own sessions and pets; your hub drops anything that claims to belong to it or to a third machine, caps what each peer may store, and never runs rules or approvals on mirrored items. With a confirmation on their own device, peers can ask your hub to approve once, deny, prompt, cancel or start an agent. Your hub validates each request against its own state (the first decision wins), refuses always-allow, only starts agents in folders it already uses, never auto-approves in sessions another machine drives, and logs every remote decision with the origin machine. A paired machine is trusted to report your confirmation honestly, so pair only your own machines; `[peer] accept_intents = false` turns remote control off.
- **Multi-machine (planned):** an explicit confirmation (or biometrics on the phone) for approvals made on another device, and a relay that only sees ciphertext.

Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, the checks to run before a pull request, and commit conventions. Everyone taking part is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## License

MIT. See [LICENSE](LICENSE).
