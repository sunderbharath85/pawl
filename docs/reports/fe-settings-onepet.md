# fe-settings: one pet on screen (desktop pet UX)

Branch `feat/one-pet` (worktree `E:\workspace\ai-pet-onepet`). Nothing committed; t13 commits.

## What the user asked

From the first message:

- "I want one single pet to show initially, why are there so many pets."
- "I should be able to place the pet anywhere on the screen."
- "A window should open every time I click on the pet; it should have its own chat window or something like a bubble."

Two changes from the user came later, through t13. Both are built:

- **Change to items 3 and 4:** "unless I right click on pet, open hub, then I should see window".
  - The panel opens only from the pet's right-click menu ("Open hub" or "Settings") or from the tray's "Open hub".
  - A click on the pet only toggles its bubble.
  - A double-click does not open the panel.
  - There is no "Open Pawl" button in the bubble.
  - Nothing opens the panel by itself: not app start, approvals or notification clicks. An approval opens the bubble instead.
- **Addition:** "one pet should orchestrate all the agents" and "sessions are individuals but the default pet should orchestrate".
  - The default pet's bubble is the general bot chat (`chat` with no pet id). It shows the bot's replies and proposals, with pending approvals on top.
  - The default pet's identity (name "Pawl", species, size) is a desktop setting.
  - "Pets on screen" offers "Default pet only" (the default) or "Default pet + one per active session". Session pets are individuals with their own bubble: that session's approvals, recent messages and a prompt box that sends `prompt{sessionId}`.
  - Settings has a "Default agent" picker for `bot.defaultAgent`.

## What it does now

### One pet

- The screen shows one pet by default: the default pet ("Pawl", a dog unless changed). It is local to this app, not a hub pet.
- It shows the most important state across all sessions and machines. Offline machines are left out. In order:
  1. **Needs you:** any approval, from this machine or an online paired one, or a hook session waiting in its terminal.
  2. **Working:** any running session. It shows the newest session's refined pose, such as "planning".
  3. **Just finished, or hit a snag:** a pet's recent happy or sad state.
  4. **Idle.**
  5. **Napping.**
- A red badge shows how many approvals are waiting while the bubble is closed.
- "Default pet + one per active session" adds a smaller pet beside it for each session that is running or needs you. It never adds one for an idle agent with no session.
  - At most 6 session pets show; the rest become a "+N" chip.
  - Each session pet is labelled with its pet's name and repo, plus the machine for a remote session.
  - Oldest session comes first, so pets keep their places.
- Existing users get "Default pet only": the setting is new, so missing stored keys take the default.

### Place it anywhere

- The full-width bottom strip is gone. The pet window is now small, transparent, undecorated and always on top: the pets plus room for one bubble.
- Click-through for the transparent parts still uses the hit-rect poll.
- Press and drag a pet to move it. The drag threshold is 4 px, so a small wobble is still a click. The drag uses the OS move loop (`start_dragging`).
- When the pet stops moving for 0.5 s, Rust clamps it onto a visible work area and saves the spot to `pet-position.json` in the app config folder.
  - Spots are saved per monitor setup: the monitors' bounds and scale. A last-resort spot is also kept.
  - On start, the spot is restored and clamped.
  - Every 2 s Rust re-checks the monitors. If a monitor is unplugged or the resolution changes, the pet returns to its saved spot for that setup, or is clamped onto the nearest screen.
- "Reset pet position" is in the tray menu, the pet's right-click menu and Settings. It goes to the bottom-right of the main monitor.
- Session pets line up beside the default pet and move with it. They go on the right, or the left when the right has no room.
- The old behaviour that moved the strip to the monitor under the cursor (`set_follow_cursor`) is removed.

### Click opens a chat bubble

- A click on a pet toggles its bubble.
  - The bubble opens above, or below when there is no room above. It shifts left or right to stay on screen, with the tail still pointing at the pet.
  - The window always keeps that room. Opening or closing the bubble therefore never moves or resizes the window, so the pet never jumps. The room only flips after a drag.
- The **default pet's bubble** is the orchestrator:
  - all pending approvals on top, with Approve and Deny;
  - another machine's request keeps the confirm step ("Approve on Mac mini: Run npm test?"), and Approve is blocked when it offers no allow_once;
  - "waiting in its terminal" notes;
  - the last 6 general-bot chat messages, with proposals (the same component as the panel, `ChatProposalsView`);
  - a typing indicator, and a text box that sends `chat` with no pet id.
- A **session pet's bubble** has:
  - that session's approvals;
  - its last 4 user and agent messages, after a backfill;
  - a prompt box that sends `client.prompt(sessionId)`. It is disabled with a reason for hook sessions, offline machines, or no hub.
- Esc closes the bubble, and so does clicking outside it (the window loses focus).
- A new approval opens the bubble by itself, on the machine the user is active on (`notifiesHere`), alongside the existing OS notification. The session pet's bubble opens if that session has a pet on screen; otherwise the default pet's.
- While the hub is down or rejects the token, the default pet shows the hub-status bubble ("Wake the hub"). It closes on reconnect.

### The panel opens every time the user asks, and only then

- Right-click on any pet opens a native menu: **Open hub**, **Reset position**, **Settings** (the panel on Settings), and **Quit Pawl**.
- The tray menu has: **Open hub**, Pause pets, **Reset pet position**, Start hub, and Quit Pawl.
- `show_panel` now always works:
  - it recreates the panel from its `tauri.conf.json` entry if the window is gone;
  - it calls `show()`, then `unminimize()` if minimized, then `set_focus()`;
  - it toggles always-on-top briefly to force it to the front.
  - A recreated panel asks for its view with `take_panel_focus`, because the `panel-focus` event fires before it listens.
- Why it may not have reopened before, from tao 0.37.3 on Windows:
  - **No re-create:** `show_panel` silently did nothing when the panel window was gone.
  - **Minimized panel:** `set_focus` is skipped while the window flags still say minimized. The old order (unminimize, show, focus) dispatches asynchronously, so focus could run before the restore took effect.

### Settings

- "On the strip" is now **"Pet on screen"**:
  - the default pet's name, checked like other pet names (1–40 characters, not another pet's name, and no invisible characters);
  - species;
  - the "Pets on screen" radio choice;
  - show names, pet size, Pause pets, and **Reset pet position**.
- The Bot section has a **"Default agent"** picker: Automatic, then this machine's installed agents.
  - A saved agent that is no longer installed stays listed.
  - The picker is hidden for a hub that predates the setting.

## Files changed (mine)

**apps/desktop/src-tauri**

- `pet_window.rs`: rewritten. Adds frame and anchor, drag (`pet_start_drag`), `on_moved`, settle and clamp, saved spots per monitor setup, the monitor watch, the context menu, `reset_pet_position`, `pet_attention`, `show_panel` (recreate), `take_panel_focus`, and unit tests.
- `tray.rs`: new items, one app-wide menu handler (Tauri menu events are global), and a left-click that opens the pet's bubble.
- `lib.rs`: the new commands, `PanelFocusState`, pet `Moved` events, and a second launch that calls `pet_attention` instead of `show_panel`.
- `hub.rs`: a `while let` fix for clippy 1.95 (`while_let_loop`), an issue that existed before this work.

**apps/desktop/src**

- `pet/PetApp.tsx`: rewritten.
- `pet/Bubble.tsx`: new.
- `pet/pet.css`: rewritten.
- `lib/petScreen.ts`: new. State picking, session pets, and the default pet name check.
- `lib/petLayout.ts`: new. Window layout, bubble placement and flips, and the drag threshold (`PressTracker`).
- `lib/native.ts`: new commands and events; `setFollowCursor` removed.
- `lib/settings.ts`:
  - adds `petsOnScreen`, `defaultPetName` and `defaultPetSpecies`;
  - adds `normalizeSettings`, which repairs malformed stored values and clamps the pet size.
- `lib/settingsForms.ts`: `BotForm.defaultAgent`.
- `lib/pets.ts`: comment only.
- `panel/PanelApp.tsx`:
  - the panel no longer opens by itself on first run; the setup is its first view when the user opens it, and is marked offered on focus;
  - adds `take_panel_focus`.
- `panel/views/ChatProposals.tsx`: split into `ChatProposals` (panel context) and `ChatProposalsView` (props). The "open session" link is optional; the bubble leaves it out because it opens the panel.
- `panel/views/settings/AppSections.tsx`: `PetSection` and `DefaultPetForm` replace `StripSection`.
- `panel/views/settings/sections.tsx`: the Default agent picker.
- `panel/views/SettingsView.tsx`: section id `pet`, "Pet on screen".
- `panel/settings.css`: choice and default-pet styles.
- `panel/views/NewSessionView.tsx`, `panel/views/SetupView.tsx`: copy that mentioned "the strip".

**Tests (new and extended)**

- `src/lib/petScreen.test.ts`, `src/lib/petLayout.test.ts`, `src/lib/settings.test.ts`: new.
- `src/lib/settingsForms.test.ts`: extended.

**packages/hub-client**

- `src/settings.ts`:
  - `bot.defaultAgent` in `editableSettings` and `settingsPatch`; it is only sent when changed, and never to a hub without the field;
  - the label "Default agent";
  - `defaultAgentOptions`.
- `src/index.ts`: exports.
- `test/settings.test.ts`: fixture plus 3 tests. This also fixes the typecheck fallout from the protocol change.
- Review fixes:
  - `src/proposals.ts`: `ProposalLedgerStore`, `reload()`, `MAX_LEDGER_ENTRIES`; local proposals allowed; "this machine" wording.
  - `src/client.ts`: the `proposalProblem` call.
  - `src/index.ts`: exports.
  - `test/proposals.test.ts`: tests.

**Review fixes (apps/desktop)**

- `src/lib/bubble.ts` (new) and `src/lib/bubble.test.ts` (new).
- `src/lib/proposalStore.ts` (new) and `src/lib/proposalStore.test.ts` (new).
- `src/pet/PetApp.tsx`, `src/pet/Bubble.tsx`: keys and per-target drafts.
- `src/panel/views/ChatProposals.tsx`: the shared ledger.
- `src/lib/settings.ts`, `src/lib/petLayout.ts`: the type coercion and the height clamp, with their tests.
- `src-tauri/src/pet_window.rs`: async panel build, `SavedSpots::normalized`, `SpotsWrite` / `write_spots`, and tests.

## Commands and results

All were run in the worktree.

| Command | Result |
|---|---|
| `packages/hub-client`: `bunx tsc --noEmit -p .` | ok |
| `packages/hub-client`: `bun test` | 307 pass, 0 fail (after the review fixes) |
| `apps/desktop`: `bun run typecheck` (3 tsc configs) | ok |
| `apps/desktop`: `bun test src` | 84 pass, 0 fail (9 files, after the review fixes) |
| `apps/desktop`: `bunx vite build` | ok |
| `apps/desktop/src-tauri`: `cargo test` | 24 pass (after the review fixes) |
| `apps/desktop/src-tauri`: `cargo clippy --all-targets -- -D warnings` | clean |
| `apps/vscode`: `bunx tsc --noEmit -p .` | ok |
| `apps/vscode`: `bun test` | 23 pass (hub-client changed) |
| `apps/desktop`: `bunx tauri build --debug --no-bundle` | ok: `src-tauri/target/debug/pawl-desktop.exe` |
| exe start | ok, see below |

### The exe start

The debug exe was started in isolation:

- `PAWL_HUB_URL=http://127.0.0.1:9`: nothing listens there, so it never touched the real hub.
- `PAWL_HUB_TOKEN`: a dummy value.
- `WEBVIEW2_USER_DATA_FOLDER`: a scratch folder, so the real app's settings were not touched.

The installed desktop app was not running; only the hub was (`pawl serve`).

Result:

- The process kept running and responded.
- One "Pawl" dog napped in the bottom-right corner, with the hub-down bubble ("Wake the hub") above it. The bubble shifted left to stay on screen, with its tail on the pet.
- The panel did not open.
- No position file was written.

The first two runs found two real bugs, now fixed and covered by a unit test:

1. **The app hung.** Windows showed its "not responding" ghost frame.
   - `apply()` moved the window while holding the pet-state lock.
   - On the main thread, that move fires `WindowEvent::Moved` synchronously, and `on_moved` takes the same lock.
   - Fix: window changes are now decided under the lock and made after it is released (`Ops`).
2. **A wrong spot was saved without any drag.**
   - A `Moved` event from window creation was taken as a user drag.
   - Fix: moves of the hidden window are ignored. Several recent moves of our own are remembered, because their events can arrive late. A move to where the window already is counts as ours (`own_move`).

The test run had written `pet-position.json` in the app config folder; I deleted it.

## Review fixes (t13 review)

1. **HIGH: a draft could be sent to the wrong session.**
   - When the open bubble switched from session A to B, A's draft stayed in the box, and Enter sent it to B.
   - Fixes:
     - Each bubble is keyed by its target (`key={sessionId}`, `key="default"`), so the text box is new per target.
     - The text box also stores the draft with its target (`Draft {target, text}`, `draftFor`). A draft typed for one target is never shown in, or sent to, another.
     - An auto-open (new approval, tray click) never replaces an open bubble (`autoOpenBubble`). The pet's badge shows the approval instead. The default pet's badge now also shows while a session bubble is open.
   - Tests: `src/lib/bubble.test.ts`.
2. **MEDIUM: proposal single use was per window.**
   - `ProposalLedger` takes an optional `ProposalLedgerStore`:
     - it re-reads the store before every claim and writes after every change;
     - it has a `reload()` method, and is capped at `MAX_LEDGER_ENTRIES` = 200, oldest first.
   - The desktop store (`src/lib/proposalStore.ts`) keeps the ledger in localStorage under `pawl.desktop.proposals.v1`, which both windows share.
     - Each entry has a timestamp.
     - A "running" entry older than 2 minutes (its window closed mid-run) loads as "sent, no answer", non-retryable, so it is not stuck as running.
   - Both windows reload on the `storage` event.
   - Tests: hub-client `proposals.test.ts` (two ledgers over one store) and desktop `src/lib/proposalStore.test.ts`.
3. **LOW: `normalizeSettings` only checked some fields.**
   - Every known key now takes the stored value only when it has the default's type, and unknown keys are dropped.
   - Then the enum, species, non-empty name and size range are checked as before.
   - Tests: `src/lib/settings.test.ts`.
4. **LOW: the bubble could be taller than the screen allows.**
   - `bubbleMaxH` is now `min(BUBBLE_MAX_H, available)`, at least 0. `BUBBLE_MIN_H` is removed.
   - The window room matches the bubble height.
   - Tests: `src/lib/petLayout.test.ts`.
5. **Rust (a): building the panel could deadlock.**
   - If the panel window is gone, `show_panel` now builds it with `tauri::async_runtime::spawn`, not on the main thread.
   - This matters because tray and menu handlers, and the sync `open_panel` command, run on the main thread, where `WebviewWindowBuilder::build` can deadlock on Windows.
   - An existing panel is shown at once. A pending focus is stored for the new panel, which asks with `take_panel_focus`.
6. **Rust (b): saved spots.**
   - `load_spots` normalises what it reads (`SavedSpots::normalized`):
     - `order` lists each saved setup once, and setups and order agree;
     - setups missing from `order` count as the oldest;
     - at most 16 setups are kept.
   - `remember()` no longer does file I/O under the `Place` lock. It returns a `SpotsWrite`, which runs after unlocking.
   - `write_spots` writes to a unique temp file next to the target, then renames it over the target.
   - Tests: `saved_spots_are_repaired_on_load`, `spots_are_written_whole_via_a_temp_file`.
7. **Proposals for this machine.** sde-brain now sends agent-less commands as proposals with the local `machineId`.
   - `proposalProblem` no longer refuses a start aimed at this machine. Its `ownMachineId` parameter is removed.
   - `executeProposal` (still only with `confirmed: true`):
     - checks this machine's own repos (`list_repos` without `machineId`);
     - sends a local `start_session` (no `machineId` or `confirmed` on the wire; those are for other machines) or a local `prompt`.
   - The confirm and result wording say "Run on this machine?", "Started on this machine." and "Sent to this machine."
   - The panel's Chat view, the pet bubble (both use `ChatProposalsView`) and VS Code (`runProposal`) all go through these helpers.
   - Tests: two new hub-client tests (local start, local prompt) plus the wording test.

A debug exe built and run again after these fixes, isolated as before, stayed responsive, showed the pet and the hub-down bubble, and wrote no position file.

## Decisions (beyond the brief)

- **The bubble room is always there.**
  - The window is the pets plus one bubble's room (320 × up to 420 px) on the side that has space.
  - Resizing a transparent window each time a bubble opens would move it and make the pet jump for a frame; this way the window only re-lays out after a drag.
  - The room is click-through.
- **Saved spot:**
  - It is the default pet's top-left in physical px, keyed by monitor setup.
  - It is stored in Rust (`pet-position.json`, app config folder) rather than localStorage, so it is known before the window shows. The window stays hidden until the webview sends its layout, with a 3 s fallback.
  - At most 16 setups are kept.
- **Drag end:** a drag counts as finished after 0.5 s without a `Moved` event; the OS move loop has no "drag ended" event.
- **Getters and locks:** window getters are read before locking the pet state. Tauri getters called from the poller thread wait for the main thread, and a command running on the main thread may be waiting for the same lock.
- **Tray left-click** opens the pet's bubble. It used to open the panel; per the change, the panel opens only on request. A **second launch**, which is also what clicking an OS notification can do, does the same.
- **The only path to the panel outside a menu** is VS Code's "Pawl: Open Settings" command: `open_panel`, forwarded by the hub to `PanelApp`, which calls `native.openPanel()`. It stays because it is an explicit user request from another client.
  - Every other path is the pet's right-click menu (Open hub, Settings) or the tray's Open hub.
  - Nothing opens the panel by itself: not app start, the first-run setup, approvals, notifications, a second launch, or a tray click.
- **First-run setup:** there is no auto-open. The panel's first view is the setup, and the default pet's bubble says "Right-click me and choose Open hub to finish setting up" until setup is done.
- **The orchestrator chat** shows messages with no `petId`. Per the hub (sde-brain), replies to a pet-less chat carry no pet id.
- **"Open session →"** after a proposal is not shown in the bubble, because it would open the panel.
- **Approvals on the default pet** count from every reachable machine, whichever machine the user is active on. The bubble only opens by itself where the user is (`notifiesHere`), like notifications.
- **Session pet size and anchoring:** session pets are 75% of the default pet's size, and each bubble anchors to its own pet.
- **Default pet name check:** `checkPetName` against all known hub pets, plus the peer rule's invisible-character check.
- **Default pet when the hub is down:** it shows the hub phase (napping, thinking while the hub starts, sad when unauthorized) instead of a separate dog.

## Known gaps

- **Clamping during a held drag:** if the user holds the mouse still for more than 0.5 s mid-drag with the pet partly off-screen, the clamp pulls it back while they still hold. The OS move loop does not report button release to the app.
- **Drag across monitors with different scales:** the pet is re-anchored from the window position at drop. Windows' DPI resize during the move can shift it by a few px, and the next settle corrects it.
- **Not verified by hand:**
  - drag feel, the native context menu and multi-monitor clamping on real hardware;
  - Linux/X11 window managers that constrain undecorated windows;
  - macOS.
- **The old `PetStage` strip component** stays in pet-engine. VS Code still uses it.

## Follow-up: pet animation only while chatting

The user said: "only show pet animation when the user asks something in its own chat window".

**By default the pets on screen stay still** (the default pet and the session pets): a calm idle pose with no CSS animation at all.

- `.pet-box.is-still` sets `animation: none !important` on the whole drawing. A sprite strip shows its first frame and the SVG pet rests.
- Agent activity no longer drives walking, digging, barking or napping. What the agents do shows without motion:
  - the count badge (its pop-in animation is removed);
  - a small static "!" mark when the pet needs you and there is no count (a hook session waiting in its terminal, or the bubble already open);
  - the bubble opening by itself on a new approval;
  - OS notifications.

**A pet moves only while the person talks to it in its own bubble.**

- **The default pet:** after a message is sent from its bubble (`client.chat` returns the message id), it shows the thinking pose until the bot's reply. Then it shows one short happy reaction (1.6 s) and is still again. The wait is given up after 60 s.
- **A session pet:** after the session is prompted from its bubble, it thinks until the agent's first reply (its first words, the end of the turn, or an error after the prompt). Then the same reaction. The wait is given up after 5 minutes.
- Messages sent anywhere else (the panel's Chat view, another machine) do not animate the pet.

**The mapping** is the pure, tested `lib/petMotion.ts`: `petPose` (setting × reduced motion × agent activity × chat phase), `chatPhase` and `sessionPhase`.

**New setting** in Settings > Pet on screen, "Pet animation":

- "Only while we chat" is the default, and existing users get it (`normalizeSettings`).
- "Always (shows what agents are doing)" restores the old behaviour, with the pose following the agents.

**Reduced motion:** with `prefers-reduced-motion`, pets are always still, in both modes, also while chatting.

The panel's own pet cards are unchanged.

**Gates:** desktop 3 tsc configs, 96 pass (7 new for the mapping, plus the settings default), vite build ok; hub-client 335 pass; vscode 23 pass.

**Not checked by eye:** the pet window in the Tauri app. The change is CSS plus the mapping above.

## Follow-up: dark mode in the pet window, and the setup hint

Two problems the user saw in Windows dark mode:

- The name tag was a light pill with light text.
- The bubble's text box had a white background with light text, so typed text was nearly invisible.

**Audit.** Every pet-window element now pairs a background and a foreground from the same theme, using tokens instead of hard-coded light colours:

| Element | Change |
|---|---|
| Name tag | background `color-mix(var(--card) 92%)` with `var(--ink)` |
| Text box | `var(--card)` with `var(--ink)`, placeholder `var(--ink-soft)` |
| Your messages | `var(--on-accent)` on the accent colour |
| Count badge | `var(--on-accent)` on berry |
| Proposal result | `var(--sage)` |
| Bubble outline and tail | new `--outline` token: ink in light, a soft brown edge in dark |
| Filled buttons (Approve, Send) | dark text in dark mode (`.pill-primary` / `.pill-good` use `var(--on-accent)`) |

Already token-based, and checked: approval cards, notes, waiting lines, the command block, chips, the typing dots and the scrollbars. The right-click menu is the OS's own menu.

**Visual check.** Screenshots of the pet window with its bubble open, with an approval card and "Teste" typed in the box, light and dark: `docs/screenshots/pet-bubble-light.png` and `pet-bubble-dark.png`. They were taken with Playwright against a throwaway hub.

**Setup hint.** "New here? … finish setting up" is no longer shown to a set-up hub.

- `setupLooksDone` (tested) counts a hub as set up when this machine has a name and the hub has a session, or an installed hook (`hook_status`, asked once and only when there is no session).
- When it is set up, the pet window marks `setupDone` (and `setupOffered`). Both windows share the setting.

**How Pawl talks.** Settings > Bot has "How Pawl talks: Plain (default) | Playful" for `bot.style`.

- hub-client: draft and patch (sent only when changed, never to a hub without the field), the label, `BOT_STYLES`, `BOT_STYLE_LABEL` and `isBotStyle`, with tests.
- Desktop: `BotForm.style`, with tests.

**Gates:** hub-client 336 pass; desktop 3 tsc configs, 97 pass, vite build ok; vscode tsc ok.

## Follow-up: reaction icons in place of the leading emoji

**The set.** Pet replies start with one emoji from a fixed set:

| Emoji | Icon |
|---|---|
| 🐾 | paw |
| 👀 | look |
| ⚠️ | alert |
| ✅ | done |
| 🤔 | thinking |
| 💤 | sleep |
| 🛠️ | tools |

**Splitting.** `lib/reactions.ts` (pure, tested) splits that leading emoji off the reply, with or without its U+FE0F. Any other text is left alone.

**Rendering.** `lib/ReactionIcon.tsx` shows the icon `<img src="/icons/<name>@48.png" width=22 height=22 alt="">` beside the text, aligned with the first line.

- It is decorative (`aria-hidden`): the text carries the meaning.
- If the file fails to load, the emoji itself shows instead.
- It is used in the pet bubble and in the panel's Chat.

**Serving the icons.** The `petAssets()` plugin in `vite.config.ts` now serves `assets/icons` at `/icons/` in dev, as it does `assets/pets` at `/pets/`. It copies both into `dist` on build. Tauri's `frontendDist` (`../dist`, built by `beforeBuildCommand`) therefore carries them into the release bundle: `dist/icons` has the 14 PNGs plus the manifest.

**Fonts.** Emoji fonts ("Segoe UI Emoji", "Apple Color Emoji", "Noto Color Emoji") were added at the end of `--font` and `--font-display`, so the emoji fallback renders in both themes.

**Gates:** desktop 3 tsc configs, 101 pass (with a render test), vite build ok.

## Follow-up: the installer upgrade bug, and Quit stopping the hub

**The bug.** Upgrading over an installed Pawl failed with "Error opening file for writing: %LOCALAPPDATA%\Pawl\pawl.exe". The hub sidecar (`pawl.exe`) keeps running after the desktop app quits, and the NSIS installer cannot replace a running exe.

**The fix: NSIS installer hooks.**

- `apps/desktop/src-tauri/windows/hooks.nsh` is wired up as `bundle.windows.nsis.installerHooks` in `tauri.conf.json`, following the current Tauri 2 docs ("Extending the Installer").
- `NSIS_HOOK_PREINSTALL` and `NSIS_HOOK_PREUNINSTALL` both run `PAWL_STOP_RUNNING`. It uses the built-in Windows PowerShell and looks for `pawl.exe` and `pawl-desktop.exe` processes whose executable is in `$INSTDIR`.
- It ends each one with its child processes (the hub's agents) through `taskkill /T /F`, then waits up to 15 s for each to exit.
- Only processes whose executable lives in this install folder are touched. A `pawl serve` the user runs from anywhere else, and another install's or another user's Pawl, are left alone.
- The folder reaches PowerShell through an environment variable (`PAWL_INSTDIR`, set with `System::Call` and cleared afterwards), never pasted into the command. An install path with quotes can therefore not change the script.

**Tested on this machine** with a debug NSIS installer built under another product name and identifier (`--config` override: PawlInstallTest / `dev.pawl.installtest`), so the real install was never touched.

1. A silent install into `%LOCALAPPDATA%\PawlInstallTest` exited 0.
2. I started a hub from the install folder, plus a copy of `pawl.exe` outside it.
3. Upgrading over them with a silent install exited 0, with no write error. The hub in the install folder was stopped; the outside copy kept running; the real Pawl processes (pids 43988 and 44476 in `%LOCALAPPDATA%\Pawl`) kept running.
4. With a hub and the installed desktop app both running from the install folder, a silent uninstall stopped both. It removed the folder, the Start Menu link and the uninstall key; the outside copy and the real Pawl were untouched.
5. All test leftovers were removed. The test desktop app ran with a dead hub URL and a scratch WebView2 profile.

**MSI: not changed.** Doing this with WiX would need a fragment with a custom action scheduled before `InstallValidate`. It would be pulled in through a dummy component reference, because Tauri only offers component, feature and merge refs. It would run PowerShell from an MSI, which is per-machine and elevated. I could not test that here, since a per-machine MSI install needs UAC, so I did not ship it.

- The NSIS setup (`-setup.exe`) is the recommended way to install and upgrade.
- Before upgrading with the MSI, quit Pawl, and stop the hub if Pawl did not start it.

**Quit Pawl now stops the hub this app started.**

- `HubProcess::stop_owned` handles it, and all three Quit paths go through `crate::quit`: tray, the pet's menu, and Settings (`quit_app`).
- Only the `pawl serve` child this app spawned is stopped. A hub the user runs themselves, or one the app found already running, keeps running.
- On Unix the hub's process group gets SIGTERM, which is a clean hub shutdown, then SIGKILL after 3 s.
- On Windows a windowless process cannot be asked to close, so `taskkill /T /F` ends it with its agents.
- Tests: `quitting_stops_only_the_hub_this_app_started` and `a_hub_that_already_exited_is_forgotten`.

**Known gaps:**

- **A restarted hub is not stopped.** If the hub restarted itself (Settings → restart), the replacement is no longer a child of the app, so Quit leaves it running.
- **Windows stop is forced.** A clean stop on Windows would need the hub to accept a shutdown request (for example a `shutdown_hub` client message for local clients). That is hub work for sde-brain, if wanted.

**Gates:** src-tauri `cargo test` 26 pass, `cargo clippy --all-targets -D warnings` clean; desktop 3 tsc configs, 101 pass, vite build ok; hub-client 336 pass; vscode 23 pass.

## Follow-up: the default pet shows its own name (not "Pawl")

**The problem.** The default pet always showed "Pawl", the product's name. Its name was a desktop setting (`defaultPetName`) whose default was "Pawl".

**The name now comes from the hub.** The hub keeps it in `HubSettings.bot.petName`: on first start it picks a name from its pet-name pool, and it validates renames. Every window and machine therefore shows the same name.

**hub-client**

- `editableSettings` / `settingsPatch` carry `bot.petName`, sent trimmed and only when it really changed. A hub without the field never gets it.
- The label is "Default pet name".
- Tests cover all of this.

**Desktop**

- **Where the name shows:** the name tag under the pet, the bubble header, the input placeholder ("Tell <name> what to do, or ask what's going on…"), the bubble's empty state ("I'm <name>. I keep an eye on all your agents…"), the typing indicator and the fallback dog's tooltip.
  - The right-click menu now starts with the pet's name as a disabled header (one line, `&` kept literal, Rust-tested).
  - In the panel's Chat, replies from the default pet (no pet id) are labelled with its name and species, and its empty state and placeholder use the name.
  - `lib/usePetName.ts` asks the hub for its settings once per connection; after that the hub pushes every change, so a rename made elsewhere shows at once.
- **Rename**
  - The right-click menu has "Rename…", enabled only while the hub is connected and has the setting. It focuses the pet window and opens the bubble with an inline rename field: Enter or Save, Esc or Cancel (Esc leaves the field, not the bubble). The panel is not involved.
  - Settings > Pet on screen renames the same way, and the field is disabled while the hub is down.
  - Both go through `renameDefaultPet`. It runs the local check every pet name gets (1-40 characters, no invisible characters, not another pet's name, same messages as before), then `update_settings`. A hub refusal is shown as "The name <hub's reason>.".
- **Migration**
  - The old `defaultPetName` key is dropped.
  - If it held a name the person had chosen (not "Pawl"), the pet window sends it to the hub once, if it is valid and different from the hub's name, then marks `petNameMigrated`.
  - If the hub refuses it (for example, the name is taken), the hub's name stays. A lost connection retries on the next connection.
  - Until the hub has it, that name is shown.
- **While the hub is down,** the last name seen (`lastPetName`, kept locally) shows. With no name known at all, the pet shows "Your pet".
- **Desktop settings that stay:** species and size.
- **Tests:** `lib/petName.test.ts` (which name shows, what migrates), `lib/usePetName.test.ts` (renaming: what is sent, local checks, the hub's refusal, a hub without the setting, and the rename field's markup) and `lib/settings.test.ts` (the old key is captured once, then dropped).

**Live check**

The check ran with Playwright against a throwaway hub built from this worktree (into a separate target directory). On first start that hub named its pet "Maple". All 9 checks pass:

- An old local "Rex" goes to the hub. It then shows on the tag, header, empty state and placeholder, the settings record the move, and the old key is dropped.
- A rename by another client ("Mochi") shows at once.
- Reloading does not send "Rex" a second time.
- A fresh profile, and one that only had "Pawl", show the hub's name and send nothing.

Screenshots: `docs/screenshots/pet-name-{light,dark}.png`.

The native menu and its rename event need the Tauri app, so they were not driven here. Unit tests and a render test cover them instead.

**Gates:** hub-client 337 pass; desktop 3 tsc configs, 109 pass, vite build ok; vscode tsc ok, 23 pass; src-tauri cargo test 27 pass, clippy `-D warnings` clean.

## Hotfix: one pet, always

The user said: "Any new session that comes, a pet shows up; we do not need that many pets, one pet should orchestrate everything." There is now exactly one pet on screen: the default pet.

**Pet window**

- The "Pets on screen" choice and every session-pet code path are gone:
  - `petsOnScreen` (`normalizeSettings` drops the old key);
  - `activeSessionPets` / `SessionPet`;
  - the session pets' layout (`sessions`, `more`, `side`, `sessionSide`);
  - `SessionBubble`, the "+N" chip and the `.is-session` / `.pet-more` styles.
- The window holds only the pet and its bubble. Paired machines' (visitor) pets never appear on screen either.

**The bubble's session switcher** (`SessionSwitcher` in `pet/Bubble.tsx`, `switcherSessions` in `lib/petScreen.ts`)

- The chips are "Pawl-wide" (the default, the orchestrator chat) followed by up to 5 sessions, then "+N more" as a select.
- **Which sessions:** every live session that needs you, is running, or was active in the last 30 min. That covers this machine's and online paired machines' sessions; ended ones and offline machines are left out.
- **Order:** needs-you first (with a dot and the approval count), then running, then most recent.
- **Chip label:** the agent pet's name and the repo (with the machine in the tooltip for remote sessions).
- **The focused session:**
  - It always gets a chip, even while it would sit under "more" or has gone quiet.
  - If it ends or goes out of reach, the bubble goes back to Pawl-wide.
- **Picking a session** shows what a session pet's bubble used to: its approvals, a waiting note, its recent messages, and a prompt box ("Tell Toffee (api) what to do next…") with its own draft. The header line becomes that session's status and title.
- **Pawl-wide** lists every approval and the orchestrator chat.
- **A new approval** opens the bubble on Pawl-wide (or stays on the focused session if the approval is that session's). It never replaces an open bubble.
- **Animation:** the pet animates while waiting for the orchestrator's reply or for a session prompted from the bubble, then stays still.

**Panel: no crowd of dogs**

- **Home:** the hero shows only the default pet: its name, its state line, a click to open Chat.
- **Sidebar:** the pet card shows the default pet: no "+N", no cycling through pets, no mood bar.
- **Chat:** always with the default pet (no pet chips). Older replies that came from an agent pet keep that pet's name.
- **Session rows and lists:** they keep the agent pet names as text. The small static avatars already there (session header, permission card, new-session picker) stay.
- **Pets view → Settings > Pets (advanced):** the Pets view and its nav item are gone. Renaming agent pets (with species and personality) moved there as a compact list with no sprites. `open_panel` "pets" now opens that section.
- **Settings > Pet on screen:** copy is in the singular ("the pet").

**Tests**

- The switcher order, cap, focus pinning and the quiet focused session (`lib/petScreen.test.ts`).
- The switcher's markup (`pet/switcher.test.ts`).
- The one-pet layout, the old key being dropped, and the "pets" route.
- The session-pet tests were removed.

**Live check (Playwright)** ran against a throwaway hub with 3 sessions (one asking), using a stored `petsOnScreen: "sessions"`. In light and dark, all of these hold:

- One pet on screen.
- The switcher shows Pawl-wide + 3 sessions, with the asking session first and its count.
- Pawl-wide lists the approval.
- Picking "api" shows its messages and an api prompt box.

Screenshots: `docs/screenshots/pet-switcher-{light,dark}.png`.

**Gates:** desktop 3 tsc configs, 112 pass, vite build ok; hub-client 337 pass; vscode tsc ok.
