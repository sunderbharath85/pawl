# Pet for VS Code

A dog (or cat, bird, robot) that watches and drives your coding agents through the local **AI Pet Hub**.

- **Status bar:** pet emoji + what it is doing + how many approvals are waiting. Click to open the pet.
- **Pet view** (activity bar → AI Pet): the pets, sessions, live transcript, Approve/Deny cards and chat.
  `AI Pet: Open Pet in Editor` opens the same view in an editor tab.
- **Commands:** Start Session, Prompt Session, Cancel Session, Approve Pending, Deny Pending, Show Pet,
  Change Pet Species, Install Agent Hooks, Start Hub, Reconnect to Hub.
- **Edits:** when an agent finishes editing a file inside this workspace, the file (or a diff against git
  HEAD) opens in a preview tab — `ai-pet.openEditedFiles`: `off` | `open` | `diff`.
- **Notifications:** permission requests of pet-owned sessions (with Approve/Deny), finished turns and hub
  nudges — `ai-pet.notifications`: `all` | `permissions` | `off`. A window only speaks up for sessions inside
  its workspace, or when it is the focused window.

The extension host owns the only hub connection (`ws://127.0.0.1:47800/ws`, token from the hub data dir);
the webview never opens a socket. When the hub is down the pet naps and the status bar offers **Start hub**,
which runs `pet-hub serve` in a terminal (`ai-pet.hubExecutable`, default `pet-hub` on PATH, falling back to
`target/{release,debug}/pet-hub` in the workspace).

## Settings

| Setting | Default | |
|---|---|---|
| `ai-pet.hubUrl` | `""` | Hub URL; empty = `AI_PET_HUB_URL` or `ws://127.0.0.1:47800/ws` |
| `ai-pet.hubToken` | `""` | Token override; empty = `AI_PET_HUB_TOKEN` or the hub's `token` file |
| `ai-pet.hubExecutable` | `pet-hub` | Used by Start Hub and Install Agent Hooks |
| `ai-pet.openEditedFiles` | `open` | `off` / `open` / `diff` |
| `ai-pet.notifications` | `all` | `all` / `permissions` / `off` |
| `ai-pet.notifyOnlyWorkspaceSessions` | `false` | Only notify for sessions inside this workspace |
| `ai-pet.defaultAgent` | `""` | Agent preselected by Start Session |

## Develop

```sh
bun run build        # dist/extension.js (node/cjs), dist/webview.js (browser/iife), dist/webview.css, dist/media/pets
bun run watch        # rebuild on change
bunx tsc --noEmit    # typecheck
bun test             # unit tests
bun run package      # .vsix via @vscode/vsce (--no-dependencies; everything is bundled)
```

Open `apps/vscode` in VS Code and press F5 (“Run Pet for VS Code”, see `.vscode/launch.json`)
to try it in an Extension Development Host.
