# fe-apps: Step 5 (remote commands by machine + repo): hub-client, desktop, VS Code

Branch `step-5-remote-commands`. Nothing committed.

I edited only my owned paths: `packages/hub-client`, `apps/desktop` and `apps/vscode`. pet-engine needed no change this step. I coded against the protocol v6 types and the CONTRACTS.md "Step 5" section, not against a running hub or brain, because those are being built in parallel.

## Security rule, on the client

The bot never runs a command on another machine. A proposal runs only after the user clicks it and confirms on this device:
- **Desktop:** an inline confirm, "Run on **Mac mini**? Start Claude Code on Mac mini in ai-pet: run the tests", with a single "Run on Mac mini" button and Back.
- **VS Code:** a modal with a single "Run on Mac mini" button.

Only then does the client send the ordinary step-4 `start_session` or `prompt`, which the hubs validate as usual.

Two more layers back this up:
- **hub-client refuses without the confirmation.** `executeProposal` rejects with `unconfirmed` *before sending* unless `confirmed: true` is passed. This applies to **both** kinds (the contract only requires it for start; I added it for prompt too).
- **Malformed proposals never leave this machine.** No valid machine or agent, a start without a folder, a prompt without a session or text, or a prompt whose session belongs to a different machine than the proposal names: all reject with `invalid` without sending.

Task text and chat text are never logged or echoed in errors (there is a test for this). This step adds no new logging.

## What changed

### hub-client (`packages/hub-client`)

**`HubClient`:**
- **`listRepos({machineId?})`** returns a machine's repo index. `machineId` is sent only for another machine.
- **`executeProposal(p, {confirmed})`** returns `{kind: "started", session}` or `{kind: "sent", sessionId}`.
  - A `start` becomes `startSession({agentId, cwd, prompt: text, machineId, confirmed: true})`. An empty task starts without a prompt, and step-4 remote rules and waits apply.
  - A `prompt` becomes `prompt(sessionId, text)`.

**New `src/proposals.ts`:**
- `PROPOSAL_TTL_MS` (10 min) and `proposalExpired(message, now)`.
- `proposalProblem(p)`: the shape check.
- Wording: `proposalConfirmText` ("Run on Mac mini?" / summary / "Run on Mac mini") and `proposalDoneText` ("Started on Mac mini." / "Sent to Mac mini.").
- `proposalRetryable(err)`: true only for codes after which nothing ran on the owner (offline, rate_limited, forbidden, invalid, not_found, not_owner, unconfirmed, closed, unauthorized). `timeout` and `disconnected` leave the proposal used, because the outcome is unknown.
- **`ProposalLedger`:** an in-memory single-use record per chat message.
  - A message's proposals are alternatives, so once one runs or is running, the others are spent too.
  - A failure that ran nothing frees the message for another try.
  - It is subscribable, and bounded to 200 entries.

**Tests:** new `test/proposals.test.ts` covers the `listRepos` wire format, `unconfirmed` refusals with nothing sent (both kinds), `invalid` shapes with nothing sent and no task text in errors, the start/prompt wire formats, an empty task, hub error codes kept, TTL, wording, retryable codes, and the ledger's single-use and retry rules. 169 tests in total.

### Desktop (`apps/desktop`)

**Chat** (`ChatView` + new `views/ChatProposals.tsx`):
- **Display:** pet messages with `proposals` show them as buttons with the summary, plus "Pick one:" when there are several.
  - A button is disabled while its machine is offline (with the reason) and after 10 min ("This suggestion expired. Ask again to get a fresh one.").
- **Running one:** a click opens the inline confirm step. Confirming claims the message in the ledger and runs `executeProposal(p, {confirmed: true})`.
- **Results show inline:**
  - Success: "Started on Mac mini." / "Sent to Mac mini." with "open session →".
  - Failure: the step-4 `intentErrorText` wording, with "You can try again" when nothing ran.
- **Single use:** the ledger lives as long as the panel window, so switching views doesn't re-enable a used proposal.

**New session:**
- **Repo picker:** the folder field now uses `listRepos({machineId})`, for this machine too. This replaces the recent-cwd suggestions.
  - Up to 12 repo buttons (name plus short path) are filtered by what is typed; the `<datalist>` holds every repo path.
  - There are loading and error states.
- **Remote machine:** its newest repo is preselected, and the hint says "Only folders Mac mini already uses (pick one of its repos)". If the machine has none, it says to start a session there first.
- The folder `<label>` became a `div` + `label htmlFor`, so the picker buttons don't steal focus.

### VS Code (`apps/vscode`)

**Chat (webview `ChatPanel`):**
- Pet messages show proposal buttons (disabled when offline or expired).
- A click posts `{type: "proposal", messageId, proposalId}`.

**Host (new `host/proposals.ts`):**
- `runProposal` checks that the proposal still exists, isn't expired or used, and that its machine is online.
- It then asks in a modal: `showWarningMessage("Run on Mac mini?", {modal: true, detail: summary}, "Run on Mac mini")`.
- Only that button runs `executeProposal(p, {confirmed: true})`. The outcome goes into a host-wide `ProposalLedger`.

**Results:**
- The ledger's entries ride on every `state` push (`HostToWebview.state.proposals`). All views show the same single-use state, with "Open session" or the error inline, and a failure also shows a warning toast.

**Start Session:**
- **Remote folder:** a QuickPick of that machine's repo index (`$(repo) name`, path as description), plus "Enter another folder Mac mini already uses…". This replaces the recent cwds.
- **This machine with no workspace folder:** its repos are offered first, then Browse….

**README** is updated.

## Commands run (final; all pass)

| Where | Command | Result |
|---|---|---|
| packages/hub-client | `bunx tsc --noEmit` | ok |
| packages/hub-client | `bun test` | 174 pass / 0 fail (9 files) |
| packages/pet-engine | `bunx tsc --noEmit` | ok |
| packages/pet-engine | `bun test` | 75 pass / 0 fail (no change this step) |
| apps/desktop | `bunx tsc --noEmit` | ok |
| apps/desktop | `bunx tsc --noEmit -p tsconfig.node.json` | ok |
| apps/desktop | `bunx tsc --noEmit -p tsconfig.test.json` | ok |
| apps/desktop | `bun test src` | 23 pass / 0 fail |
| apps/desktop | `bunx vite build` | ok |
| apps/desktop/src-tauri | `cargo check` | ok (no Rust changes) |
| apps/vscode | `bunx tsc --noEmit` | ok |
| apps/vscode | `bun test` | 19 pass / 0 fail |
| apps/vscode | `bun run build` | ok |
| apps/mobile | `bunx tsc --noEmit` | ok. No fix needed: `ChatMessage.proposals` is optional. |

No dependencies were added, removed or upgraded, and no install was run.

Line endings are preserved: CRLF files (NewSessionView, panel.css) stay CRLF and new files are LF. No file has mixed line endings.

## Known gaps
- **Not run against the real hub or brain.** No Tauri window or Extension Development Host was launched. The proposal UI was checked through code and the hub-client tests only; there are no DOM tests in these apps.
- **Single use is per window, not across devices.** The ledger is in memory: per desktop panel window, and per VS Code extension host. A proposal used in the desktop app can still be clicked in VS Code within its 10 minutes. The owner validates every run, and a second start creates a second session. If that matters, the hub could mark a proposal used. That would mean a protocol change, for example a `used` flag on the chat message.
- **`recentCwds` (hub-client)** is no longer used by the apps but is kept, since it is tested and harmless.

## Decisions for the orchestrator
1. **`executeProposal` requires `confirmed` for `prompt` proposals too,** not only `start`. The rule is that the bot never acts by itself, so a proposed prompt to another machine's agent goes through the same confirm step.
2. **Proposals could be single-use across clients** if the hub records use (for example `proposalUsed{messageId}` broadcast, or a hub-side check when the ClientMsg carries the proposal id). Say if you want that in this step.

## Follow-up (t13 security review fix 5)
- **No start proposals for this machine.** `proposalProblem(p, ownMachineId)` and `executeProposal` reject a `start` proposal whose `machineId` is this machine with `invalid` ("local starts are not proposals"). Nothing is sent, not even `list_repos`. Before the hub's hello (machine unknown), a start proposal rejects with `no_machine`.
- **A start's folder must be in that machine's repo index.**
  - `executeProposal` checks the folder against that machine's `list_repos`.
  - It uses the cached answer when that already contains the folder, and fetches fresh otherwise. Every `listRepos({machineId})` call fills the cache.
  - A mismatch → `invalid` ("its folder is not one of that machine's repos"), and no `start_session` is sent.
  - A failing `list_repos` (for example `offline`) starts nothing and keeps its code.
- **How folders are compared** (`repoIndexHas`): exact folders only. Trailing separators are ignored. Case is ignored for Windows and macOS owners, not for Linux. A parent or subfolder doesn't match.
- **Wording:** `intentErrorText` for an `invalid` start now says "…the agent must be installed there and the folder must be one of its repos."
- **Tests:** hub-client 174. New: a local start is refused with nothing sent; a folder outside the index starts nothing; the cache hit and the refetch on a miss; offline `list_repos`; `repoIndexHas` cases.
- **Gates rerun, all green:** hub-client, pet-engine, desktop (three tsc configs, 23 tests, vite build, cargo check), VS Code (tsc, 19 tests, build), mobile tsc. The apps needed no change: they already go through `executeProposal`.
