# fe-settings: the screen and browser UI (v0.2 items 4 and 5)

**Scope.** Built on `docs/CONTRACTS.md` "v0.2 Screen and Browser".

- I changed only `apps/desktop/src` and `packages/hub-client`.
- The screen commands are sde-voice's (`src-tauri/src/screen`). My wrappers match them: `screen_sources()` gives `{id, kind, title, app, thumb}`, and `screen_capture(id)` gives `{mediaType, data, width, height}`, camelCase.
- The hub side (vision replies, `start_browser_task`, the `[browser]` settings) is sde-brain's.
- t13 added `start_browser_task` to `AckResults`, so the client needs no cast.

## Item 4: look at the screen

**hub-client: `src/screen.ts`**

- `pictureDestination(bot, agents, machineId)`, built on `botProviderNow`, gives:
  - "to Claude Code on your plan";
  - "to Anthropic / OpenAI / Google Gemini with your key";
  - "to Ollama on this PC (llava, if it can see pictures)", or "at <host>";
  - "nowhere: your model can't see pictures", with `canSee: false`, which disables Send.
- `imageAttachProblem(session, agents, local)`: a picture goes with a prompt only to this machine's open sessions that Pawl started, whose agent advertises `promptImage`. Otherwise it gives the reason:
  - "Pictures never go to another machine."
  - "<agent> can't take pictures."
  - "<agent> hasn't said yet whether it takes pictures."
- `isLookRequest` / `lookQuestion`: "look at this", "look at my screen", "what's on my screen", "can you see my screen". The phrase must start the message (after an optional "Hey Mochi," or "please"), so "I'll look at this later" stays an ordinary message. The question is "What do you see?" unless the user said more.
- Also exported: `CAPTURE_WARNING`, `IMAGE_MAX_BYTES` and `base64Bytes`.
- `client.chat(text, petId?, { images })` and `client.prompt(sessionId, text, { images })`.

**Desktop**

- **Screen access:**
  - `lib/screen.ts`: the Tauri wrappers, `toChatImage` and data URLs.
  - `lib/screenMock.ts`: dev only (`?screen=mock`), four made-up screens and windows drawn on a canvas. It is not in the build (checked).
- **The flow, `pet/ScreenLook.tsx`, inside the bubble:**
  - **Picker:** screens first, then windows by app, with thumbnails, under the one-line warning "A capture shows everything in it, including passwords or private messages…".
  - **Preview:** the capture, a line saying where it goes ("Goes to Claude Code on your plan, for this one message. Pawl keeps no copy." or the "nowhere" line with a hint), the question (default "What do you see?"), and Send, Pick another and Cancel.
  - **Memory:** the picture lives only in that component's state. It is dropped as soon as Send is pressed, and the flow closes on success. Closing the bubble, or switching away from the session it was for, also drops it (PetApp).
- **Ways in:**
  - the bubble's "Show my screen" button (a screen icon beside the text box);
  - typing "look at this" in the bubble;
  - saying "look at this" (`useVoiceTalk`), which opens the bubble and the same flow instead of sending the words.
- **Sessions:** the session pane's composer has "Add a picture of your screen". It is disabled with the reason above, and sends a prompt with `images`.
- **Your own message with a picture** (`ChatMessage.image`) shows a small "screenshot" chip, in the bubble and in the panel's Chat. The picture itself is never shown again.

## Item 5: browser use

**hub-client: `src/browser.ts`**

- `browserOf` and `isBrowserSession` (`purpose: "browser"`).
- `parseAllowSite` takes hosts from what people paste ("https://github.com/x" becomes github.com; `*.example.com`; `localhost`) and refuses nonsense.
- `browserPatch`.
- **`browserRiskyChanges`** (part of `riskyChanges`):
  - **Turning it on.** Your own Chrome through auto-connect; Chrome asks you to Allow each connection and shows a banner; every click and every typing asks first; never passwords or payment details; page text is never instructions.
  - **New allowed sites.** "Open docs.rs without asking?"; clicking, typing, submitting, uploads and dialogs still ask.
- Labels for `browser.enabled` and `browser.allowSites`.
- **Proposals:**
  - A `browse` proposal needs only its task (it runs on this machine's hub).
  - `proposalLabel` gives "Start browser task: <task>".
  - The confirm reads "Start a browser task? <task>. Your pet uses your Chrome: Chrome asks you to Allow it, and every click or typing asks you here first." with the button "Start browser task".
  - "Browser task started." when it went through.
  - `executeProposal` sends `start_browser_task` (`confirmed: true`) through the new `client.startBrowserTask(task)`.

**Desktop**

- **Settings > Browser** (`BrowserSection.tsx`, after Bot):
  - the switch, off by default and confirmed;
  - "How it works": Chrome 144+, auto-connect, no separate browser;
  - the one-time step: `chrome://inspect/#remote-debugging` with a Copy button and "Paste it into Chrome's address bar (Pawl can't open Chrome's own pages for you)";
  - each task: Allow and the banner; every click and typing asks; no passwords or payment details; page text is not instructions;
  - when on: the editor for sites opened without asking (chips with remove; add confirmed).
  - A hub without the setting gets a note.
- **Proposals:** a `browse` proposal in the bubble and in the panel's Chat reads "Start browser task: <task>" with a globe icon, and its confirm leads to `start_browser_task`.
- **Browser sessions** show a "Browser" chip in the sidebar's session list, the session view's header, and the pet's switcher.

## Tests

- **hub-client** (`test/screenBrowser.test.ts`, 8 tests, 368 pass in total):
  - every destination;
  - the attach rules;
  - the look phrase and question, including "I'll look at this later" not matching;
  - picture size;
  - allowed sites;
  - the browser patch and both confirms with their promises;
  - browser sessions;
  - the browse proposal's label, problem, confirm and done text.
- **desktop** (157 pass in total):
  - `lib/screen.test.ts`: no screen access outside the app; source order; the picture as the chat image; the flow's first view (title, warning, no image before a capture; nothing without screen access);
  - the switcher's Browser mark.

## Live check

Playwright ran against vite dev with `?screen=mock` and a throwaway hub. The hub here predates the browser settings, so the page's WebSocket was routed to add `browser` to the settings and to inject a pet message with a `browse` proposal. 20 of 20 checks pass, in light and dark:

- **The look flow:**
  - the picker warns, and shows screens and windows with thumbnails;
  - the preview says "Goes to Claude Code on your plan…", with the default question;
  - Send sent one chat frame with one PNG picture and the typed question;
  - after that, no picture was left anywhere in the bubble.
- **Typed "look at this"** opened the picker instead of sending.
- **Settings > Browser** explains auto-connect, Chrome 144 and the one-time step, and lists the two allowed sites.
- **The browse proposal** reads "Start browser task: open PR 42…", and its confirm says what will happen.

Screenshots in `docs/screenshots/`:

- `screen-look-{light,dark}.png` (the preview)
- `screen-pick-light.png` (the picker)
- `browser-settings-{light,dark}.png`

## Gates

| Gate | Result |
|---|---|
| desktop | 3 tsc configs, 157 pass, vite build ok (no mock in it) |
| hub-client | tsc ok, 368 pass |
| vscode | tsc ok |

## Notes

- **The session transcript does not mark a prompt that had a picture.** Its `user_message` event has no `image` flag in the contract. If the hub adds one, the transcript can show the same chip.
- **Not checked live:** the session pane's attach button. It is enabled only for an agent advertising `promptImage`, and the test hub's agents had not reported capabilities. The rules are unit-tested.

## Review fixes: picture destinations

**(6) Ollama URLs without a scheme.**

- `ollamaHost(url)` reads the URL like the hub's `ollama_chat_url`: `http://` is added when there is no scheme.
- `isThisPcHost` counts only localhost, 127.0.0.0/8 and ::1 as this PC.
- So "192.168.1.5:11434" and "gpu-box:11434" now say "to Ollama at 192.168.1.5 / gpu-box (…)" and leave this PC. Before, the empty host they produced read as "on this PC".

**(6) Pictures given to a session's agent.**

- `agentPictureDestination(agentId, name)` names the maker's cloud for the known agents, with `leavesPc: true`:
  - Claude Code (also `claude-acp`): "to Claude Code (Anthropic's cloud, through your plan)";
  - Codex: OpenAI;
  - Gemini CLI: Google;
  - Copilot: GitHub.
- Any other agent: "to <agent> (may leave this PC)".
- It is used for the session pane, and for the bot's ACP provider too, so the same agent reads the same everywhere.

**(3) "Pawl keeps no copy" was too strong.** `PictureDestination.copy` now says what happens afterwards:

- to an agent: "Pawl keeps no copy; the agent may keep it in its own history.";
- to a cloud provider (or a remote Ollama): "Pawl keeps no copy; the service may keep it under its own terms.";
- to Ollama on this PC, or nowhere: "Pawl keeps no copy.".

**Also fixed (found by the live check).** Settings > Browser cleared the site field after a save. If the user had already typed the next site, it was wiped and "Add site" stayed disabled. The field is now cleared only if it still holds the site that was added.

**Tests:** hub-client +2:

- scheme-less, LAN and remote Ollama URLs against the loopback forms;
- agent destinations for the known and unknown agents;
- the copy lines.

**Live check:** 22 of 22 pass. The hub binary now speaks the browser settings, so the live check turns browser tasks on through the confirm, adds two sites (each confirmed) and checks the hub saved `{enabled: true, allowSites: ["github.com", "docs.rs"]}`. The preview now reads "Goes to Claude Code (Anthropic's cloud, through your plan), for this one message. Pawl keeps no copy; the agent may keep it in its own history." The screenshots are retaken.

**Gates:** desktop 3 tsc configs, 157 pass, vite build ok; hub-client tsc ok, 370 pass; vscode tsc ok.

## Follow-up: the screen gate wired in

sde-voice's gate is in `src-tauri/src/screen/gate.rs`. Every capture now runs inside a picker session:

| Command | Rules |
|---|---|
| `screen_begin()` → nonce | valid 2 min; bound to this window, which must be visible; at most once a second and 20 in 10 min; a new begin replaces the old |
| `screen_sources({ nonce })` | at most every 2 s, 5 per session |
| `screen_capture({ nonce, id })` | the id must be from the latest list; at most every 1 s, 3 per session |
| `screen_end({ nonce })` | closes the session |

- **`lib/screen.ts`:** the backend is `begin` / `sources(nonce)` / `capture(nonce, id)` / `end(nonce)` on those exact commands.
  - The gate's three messages are constants and are shown as they are.
  - `screenRetry(message)` decides what "Try again" does: a new session after "Open the screen picker again.", the list again after "Pick the screen or window again.", or the list after a 2 s wait after "Too many screen pictures in a short time…".
- **The flow (`ScreenLook`):**
  - It begins when the picker opens, a tick after mounting, so React's development double mount begins only once (the gate allows one a second).
  - It ends on Send (before the picture goes out), Cancel, the bubble closing, or the bubble switching to another session; all of these unmount the flow.
  - Each start has a generation number: answers from an older start are ignored, and a nonce that arrives late is ended.
  - If a send fails, the session is over, so "Try again" starts a new one.
- **The dev mock** follows the gate's rules (nonce, latest list only, 3 captures and 5 lists per session, end), with the same messages, and counts begins.

**Tests**

- `screenRetry` for each message.
- Through the mock:
  - no list without a session;
  - a new begin replaces the old;
  - end closes the session;
  - ids only from the latest list;
  - the per-session limits.

Desktop: 160 pass.

**Live check:** 28 of 28 pass.

- Opening the picker begins exactly one session, even under React's development double mount.
- Send ends it, and Cancel ends it.
- The earlier checks still pass.

**Gates:** desktop 3 tsc configs, 160 pass, vite build ok (no mock in it); hub-client 370.

## Follow-up: allowed sites are origins

sde-brain stores `[browser] allowSites` as **origins**: a bare host means `https://host` (port 443), and http or another port must be written out. The hub returns values like `https://github.com`.

- **`parseAllowSite`** gives `{ origin, warning }`:
  - "github.com" becomes `https://github.com`;
  - a pasted URL keeps its origin ("https://GitHub.com/acme/web/pull/42" becomes `https://github.com`);
  - a default port is dropped (`:443`, `:80`);
  - wildcards are refused (each site is listed);
  - only http(s), with no user info.
- **Warnings:** plain `http://` ("the pages are not encrypted…") and any other port ("not the usual web port, so make sure this is the site you mean") get a warning.
- **Settings > Browser:**
  - the chips show the origin with its scheme;
  - under the field, a live line says what the typed site is saved as ("Saved as https://docs.rs."), or gives the warning;
  - the placeholder says "github.com (means https://github.com)";
  - the add confirm repeats any warning ("Careful: http://localhost:3000 uses plain http…").
- **Tests:** hub-client covers the origins, warnings and refusals, and the confirm with and without a warning. 370 pass.
- **Live check:** the hub saved `["https://github.com", "https://docs.rs"]` from "https://github.com/acme/web/pull/42" and "docs.rs", and the warning for `http://localhost:3000` showed. The whole run passed.
- **Gates:** desktop 3 tsc configs, 160 pass, vite build ok; hub-client tsc ok, 370 pass; vscode tsc ok.
