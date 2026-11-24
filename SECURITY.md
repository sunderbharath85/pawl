# Security policy

Pawl approves actions for coding agents on your machine, so a bug here can let an agent or another program run commands, read secrets or change files. Please report security problems privately.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting:

1. Open the repository's **Security** tab.
2. Choose **Report a vulnerability**.
3. Describe the problem: the affected component (hub, CLI, desktop app, VS Code extension, a crate or package), the version or commit, the steps to reproduce, and what an attacker gains.

GitHub's guide: [Privately reporting a security vulnerability](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability).

Do not open a public issue, pull request or discussion for a vulnerability until a fix is released. Do not include real tokens, keys or other people's data in the report; a redacted example is enough.

What to expect:

- The maintainers aim to acknowledge a report within 7 days.
- We will confirm whether it is a vulnerability, agree on a fix and a disclosure date with you, and publish a GitHub Security Advisory when the fix is out.
- We credit reporters in the advisory unless you ask us not to.

## Supported versions

Pawl is pre-1.0. Only the latest commit on `main` gets security fixes. There are no release branches yet.

| Version | Supported |
|---|---|
| `main` | Yes |
| Anything older | No |

## Threat model

This is a summary. The design is in [PLAN.md](PLAN.md) and the interfaces are in [docs/CONTRACTS.md](docs/CONTRACTS.md).

### What Pawl protects

- **Approvals.** Approving a permission request can let an agent run a command, delete files or reach the network. Only you, through a Pawl client you run, should be able to approve.
- **The hub token.** It authorizes every local API call, including approvals.
- **Session content.** Prompts, agent messages, tool calls and file paths in `hub.db` and in transit to the clients.

### Trust boundaries today (single machine)

- **Your OS user account is trusted.** The hub, its data directory (`token`, `config.toml`, `hub.db`, `hub.log`) and the clients run as you. A program that already runs as your user can read the token; Pawl does not defend against that.
- **Web pages are not trusted.** The hub binds `127.0.0.1` only. WebSocket and hook requests need the bearer token, and WebSocket connections with a browser `Origin` are refused unless it is the Tauri app, a VS Code webview or the desktop app's Vite dev server.
- **Agents are not fully trusted.** Their permission requests go through the rules engine:
  - the first matching rule wins and no match means *ask*;
  - by default only reads and searches inside the session's working folder are auto-approved;
  - `execute`, `delete`, `fetch` (network) and unknown tool kinds are never auto-approved, whatever the rules say, and rules that try are rejected when saved;
  - requests that touch sensitive paths (`.env*`, keys and certificates, `.ssh`, `.aws`, credential files, the hub's own token and data files) always ask;
  - the ACP file read/write methods the hub serves are limited to the session's working folder.
- **Every approval and denial is written to the audit log** with who decided, what, when and the outcome.
- **The bot cannot act on model output.** Actions such as starting a session or approving come only from a deterministic parser. A language model, if configured, only writes chatter and summaries.

Out of scope for these protections:

- **The agent process itself.** It runs with your user's permissions. Pawl controls what the hub approves over ACP; it does not sandbox the agent, and an agent can still act outside ACP if it has its own tools.
- **Sessions tracked through hooks.** For agents you start yourself, `pawl hook` only reports events to the hub. Approvals for those sessions stay in the agent's own UI.
- **Third-party agents, ACP adapters and the ACP registry.** Report problems in those to their maintainers.

### Planned: multiple machines and the phone

These parts are designed but not built yet (see the migration steps in PLAN.md). The intended properties are:

- No inbound ports: hubs and the phone connect outbound to a relay. An optional direct LAN or Tailscale link between paired hubs is off by default, accepts only encrypted frames from paired keys, and never exposes the local API.
- Each device has a long-term X25519 key, exchanged at pairing (one-time code or QR). Devices can be revoked.
- Every payload is encrypted end to end with NaCl box (X25519 + XSalsa20-Poly1305). The relay sees only routing ids and ciphertext. Per-sender sequence numbers protect against replay.
- Only the hub that owns a session applies intents for it. Approvals made on another device need an explicit confirmation there, or biometrics on the phone. The first valid approval wins.
- Push notifications carry no content.

Reports about these designs are welcome before they ship.
