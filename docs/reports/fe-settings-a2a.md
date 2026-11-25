# fe-settings: Settings > A2A (PLAN.md "A2A orchestration", the desktop UI)

**Scope.** Built on sde-a2a's wire from `docs/reports/sde-a2a.md`: `HubSettings.a2a`, the `a2a` patch, `a2a_rotate_token` and `list_tasks`.

- I changed only `apps/desktop/src` and `packages/hub-client`. I did not touch `crates` or `packages/protocol`.
- **Defaults:** off by default, and the server is on 127.0.0.1 only.
- **Tokens:** a caller's token is shown once, then only "Token set". Outside agents' tokens are write-only.
- **Confirms:** every risky change asks first.

## hub-client: `src/a2a.ts` (pure; exported)

- **Patches** (lists replace the hub's; tokens are never echoed):
  - `a2aServerPatch`: only what changed.
  - Callers: `addCallerPatch` (a free id from the name, no approvals, no token until `a2a_rotate_token`), `removeCallerPatch` and `callerApprovalsPatch`.
  - Outside agents:
    - `addAgentPatch`: the id comes from the name or the host; the optional token is a string.
    - `removeAgentPatch`.
    - `agentTokenPatch`: a string sets the token, null clears it, and the other agents keep theirs because their tokens are left out of the patch.
- **Checks,** mirroring the hub:
  - `checkA2aAgentUrl`: https, or http only to 127.0.0.1 / localhost / [::1], with no user info.
  - `checkA2aName` (1-80 characters).
  - `checkA2aToken` (1-512 visible ASCII characters).
  - `suggestA2aId` (`[a-z0-9-]{1,40}`, unique).
- **Labels:**
  - `a2aServerStatus`: "Running at … (this PC only)", "Not running: <error>", "Off".
  - `a2aCardUrl`.
  - `A2A_TASK_STATE_LABEL` and `a2aTaskTone`.
  - `a2aCallerLabel` (you / a paired machine / a caller's name).
  - `isA2aAgentId`, `urlHost`, `isLoopbackUrl`.
  - Field labels for `a2a.server`, `a2a.port`, `a2a.callers` and `a2a.agents` (so "invalid:a2a.port: is in use" reads well).
- **Risky changes** (`a2aRiskyChanges`, part of `riskyChanges`, so `useSaveSettings` asks before saving):
  - **Turning the server on.** The confirm says: programs on this PC with a token can ask Pawl what your agents are doing and start work in folders Pawl already knows; they cannot approve anything unless you allow it per caller; it listens on 127.0.0.1:<port> only.
  - **Letting a caller answer approvals.** The confirm says: allow once or deny, only in the sessions it started; never "always allow"; never running commands, deleting files or network access; every answer is audited.
  - **Adding an outside agent that is not on this PC.** The confirm says: prompts go to <host>; it does not get your files.
  - Turning things off, removing, and changing tokens ask nothing.
- **`folderName`:** an outside agent's session has the agent's URL as its cwd, so its "folder" now shows the URL's host.

## Desktop: Settings > A2A (`panel/views/settings/A2aSection.tsx`)

The section sits after "Agents & hooks". A hub without A2A gets a one-line note instead.

**1. "Let other tools talk to Pawl (A2A)"**

- A switch, off by default. Turning it on shows the confirm described above.
- **Port:** saved separately and checked like the other ports. The hub's "is in use" shows under the field.
- **Live status:** "Running at http://127.0.0.1:<port> (this PC only)", "Not running: <error>", or "Off".
- **While it runs,** the agent card URL is shown with a Copy button, to give to the tool.

**2. Callers**

- **Adding:** "Add and create token" adds the caller (by name) and immediately makes its token with `a2a_rotate_token`.
- **The token is shown once**, with Copy and a warning: "Copy this token now: Pawl never shows it again… Anyone who has it can use Pawl as <name>". It lives only in that component's state, and "Done, I copied it" forgets it.
- **Each caller has:**
  - "Token set" / "No token yet";
  - "Make new token" (asks first: the old one stops working at once);
  - "Remove" (asks first);
  - "Can answer approvals" (the risky confirm; turning it off saves directly).

**3. Outside agents**

- **Adding:** URL, optional name, optional token (a password field, write-only), with live hints ("On this PC: prompts stay here" or "Prompts you give it go to <host>").
- **Each agent shows:**
  - its card: name, description, protocol version, streaming, version, "Unsigned card" or "Signature unverified" (with a note that Pawl does not check signatures yet), and up to 8 skills with their descriptions on hover;
  - when the card was read;
  - the hub's error ("Could not use it: …").
- **Each agent can:** add or replace its token, clear it (asks first), or be removed (asks first). The limit is 16 agents.
- **Refresh workaround.** The hub announces the agent list after reading a card, but not the settings, so the section reads the settings again:
  - when an outside agent appears or becomes usable;
  - every 1.5 s, for up to 30 s, while a card is still on its way.

## Outside agents elsewhere in the app

- **New session:**
  - Outside agents are listed like the others (installed once their card is read).
  - Picking one replaces the folder field with a note: it works where it runs (the host), not in a folder here; your prompts go to it; its questions come back as approvals.
  - Start sends the agent's URL as the folder, which the hub ignores for outside agents. It is never empty.
  - The remote-start confirm has wording for an outside agent.
  - `lib/a2aUi.ts`: `isOutsideAgent`, `outsideAgentUrl`, `startCwd`.
- **Sessions read like the others.** The folder chip shows the agent's host with a machine icon, and `repoName` / `folderName` show the host in the pet's switcher, the bubble and titles.
- **A2A task chip in the session view** (`useA2aTasks` → `list_tasks{sessionId}`, refreshed when the session's status changes; a hub without A2A just gives none). It shows only when useful:
  - an outside agent's session: "A2A task · Needs input";
  - a session an outside caller or a paired machine started: "A2A · Zed · Working".
  - Your own ACP sessions get no chip, since the status pill already says it.

## Tests

- **hub-client** (`test/a2a.test.ts`, +11, 350 pass in total):
  - the server, caller and agent patches (tokens absent / string / null; never echoed);
  - ids;
  - URL, name and token checks;
  - labels, the card URL, caller labels, the field label;
  - `folderName` for URLs;
  - the three confirms and their exact promises; what does not need a confirm.
- **desktop** (`lib/a2aUi.test.ts`, 117 pass in total): New session for outside agents, `repoName` for URLs, and the task chip (outside agent, outside caller, paired machine, own ACP session, none).

## Live check

Playwright ran against vite dev and a throwaway hub built from this worktree into a separate target folder, on its own A2A port. 15 of 15 checks pass:

- **Server:** off by default; turning it on asks with the promised text; it then runs and shows its live status and the card URL to copy.
- **Callers:**
  - "Claude Desktop" was added and its token shown once (64 hex characters, with the warning).
  - The hub has the token write-only (it never appears in `get_settings`).
  - "Can answer approvals" asked first with the promised text, and the change was saved.
- **Outside agents:**
  - An agent at Pawl's own A2A server on loopback, with that caller's token, showed its real card: A2A 1.0, streaming, unsigned, the skills Status / Delegate a task / Prompt a session, and "Token set".
  - An agent at `http://127.0.0.1:9` showed "Could not use it: could not reach the agent: error sending request".
  - The outside agent appeared in New session, installed and enabled, with no folder field.
- **Its session:**
  - The session showed `127.0.0.1:<port>` where a folder would be.
  - The pet's switcher listed it as "<pet> · 127.0.0.1:<port>".
  - The task chip matched `list_tasks`: there were no tasks for that session, see the hub notes.

Screenshots in `docs/screenshots/`: `a2a-settings-light.png` and `a2a-settings-dark.png` (the dark one shows a second caller's fresh token).

## Gates

| Gate | Result |
|---|---|
| desktop | 3 tsc configs, 117 pass, vite build ok |
| hub-client | tsc ok, 350 pass |
| vscode | tsc ok, 23 pass |

## Notes for sde-a2a (hub side; not changed here)

- **Settings are not pushed after a card fetch.** After the hub reads an outside agent's card (or fails to), it sends the agent list but no `settings` message, so a client's `a2a.agents[].card` / `error` stay stale until it asks again. The UI works around it (above); a `settings` push after a card fetch would remove the need.
- **Pawl's own A2A server rejects a plain prompt.** With Pawl's own A2A server added as an outside agent, a plain first prompt ("What's everyone doing?") was refused with `invalid_params` ("the A2A agent did not take the prompt: the agent said: a message …"), and the session ended in error without a task.
  - Maybe intended, since Pawl-as-agent expects its delegate or status skills.
  - Either way, a plain-text message to Pawl could be answered by the status skill.
