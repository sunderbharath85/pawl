# Pawl for VS Code

A dog (or cat, bird, robot) that watches and drives your coding agents through the local **Pawl hub**.

- **Status bar:** the lead pet's emoji and name, what it is doing, and how many approvals are waiting. The
  tooltip names this machine (with a rename link), lists the paired machines with their online state, and
  names visiting pets and approvals waiting on other machines. Click to open the pet.
- **Pet view** (activity bar → Pawl): this machine's name (plus "N machines · M online" once paired), the pets
  (every pet shows its name), sessions grouped by machine, live transcript, Approve/Deny cards and chat.
  Paired machines' pets visit on the right, and a pet with a waiting approval runs to the machine you are
  using. Other machines' sessions can be prompted, cancelled, approved and denied from here; approving one
  (or starting a session on another machine) asks in a modal first ("Approve on Mac mini: …?").
  In chat, "tell the Mac mini's Claude to run the tests in ai-pet" comes back as a button; clicking it asks
  "Run on Mac mini?" before anything is sent (each suggestion works once, for 10 minutes). Start Session on
  another machine picks from that machine's repos.
  `Pawl: Open Pet in Editor` opens the same view in an editor tab.
- **Commands:** Start Session (pick this machine or an online paired one), Prompt Session, Cancel Session, Approve Pending, Deny Pending, Show Pet,
  Rename Pet, Change Pet Species, Rename This Machine, Show Machines (read-only; pair machines in the desktop
  app or with `pawl peer`; online machines show whether they are linked over the LAN or the relay), Install Agent
  Hooks, Start Hub, Reconnect to Hub, and **Open Settings**: every hub setting lives in the Pawl desktop app, so
  this asks it to open its Settings view (or tells you to open the desktop app when it is not running).
- **Edits:** when an agent finishes editing a file inside this workspace, the file (or a diff against git
  HEAD) opens in a preview tab — `pawl.openEditedFiles`: `off` | `open` | `diff`.
- **Notifications:** permission requests of pet-owned sessions (with Approve/Deny), finished turns and hub
  nudges — `pawl.notifications`: `all` | `permissions` | `off`. A window only speaks up for sessions inside
  its workspace, or when it is the focused window. With paired machines, only the machine you are using
  notifies (for every machine's sessions); VS Code focus, edits and selections tell the hub you are here.

The extension host owns the only hub connection (`ws://127.0.0.1:47800/ws`, token from the hub data dir);
the webview never opens a socket. When the hub is down the pet naps and the status bar offers **Start hub**,
which runs `pawl serve` in a terminal (`pawl.hubExecutable`, default `pawl` on PATH, falling back to
`target/{release,debug}/pawl` in the workspace).

## Settings

| Setting | Default | |
|---|---|---|
| `pawl.hubUrl` | `""` | Hub URL; empty = `PAWL_HUB_URL` or `ws://127.0.0.1:47800/ws` |
| `pawl.hubToken` | `""` | Token override; empty = `PAWL_HUB_TOKEN` or the hub's `token` file |
| `pawl.hubExecutable` | `pawl` | Used by Start Hub and Install Agent Hooks |
| `pawl.openEditedFiles` | `open` | `off` / `open` / `diff` |
| `pawl.notifications` | `all` | `all` / `permissions` / `off` |
| `pawl.notifyOnlyWorkspaceSessions` | `false` | Only notify for sessions inside this workspace |
| `pawl.defaultAgent` | `""` | Agent preselected by Start Session |

## Develop

```sh
bun run build        # dist/extension.js (node/cjs), dist/webview.js (browser/iife), dist/webview.css, dist/media/pets
bun run watch        # rebuild on change
bunx tsc --noEmit    # typecheck
bun test             # unit tests
bun run package      # .vsix via @vscode/vsce (--no-dependencies; everything is bundled)
```

Open `apps/vscode` in VS Code and press F5 (“Run Pawl for VS Code”, see `.vscode/launch.json`)
to try it in an Extension Development Host.
