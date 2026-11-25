# fe-settings: the voice UI (v0.2)

**What this covers.** The desktop UI for "talk to your pet": a wake word (the pet's name), push-to-talk, transcripts sent to the chat, and replies read aloud.

- It is built against `docs/CONTRACTS.md` "v0.2 Voice", including the Glue decisions.
- The engine is sde-voice's, in `apps/desktop/src-tauri/src/voice/`. I did not edit `src-tauri`. My wrappers match its commands (argument names, lowercase enums, camelCase events).
- Voice is off by default. The models are never bundled: the user downloads them from Settings > Voice.

## Pieces

| File | What |
|---|---|
| `lib/voice.ts` | See "lib/voice.ts" below. |
| `lib/voiceMock.ts` | A stand-in engine for `vite dev` in a browser tab, loaded only in dev and only with `?voice=mock` (fresh) or `?voice=ready`. It follows the contract and is driven through `window.__pawlVoiceMock`. The production build contains none of it (checked: no mock code, no model files in `dist`). |
| `lib/voiceShared.ts` | The ids of spoken chat messages (localStorage, so the panel's Chat shows "You (voice): …" too). Also the wake-word test flag, so the pet window stays out of a Settings test. |
| `lib/speech.ts` | Read-aloud with the webview's `speechSynthesis` (the system's voices, no download). `spokenText` drops the reaction emoji, markdown marks, code blocks and link addresses. |
| `pet/useVoiceTalk.ts` | The pet window's part. See "useVoiceTalk" below. |
| `pet/VoiceBits.tsx` | The static mic marker (live or crossed), the listening panel and the mic button. |
| `panel/views/settings/VoiceSection.tsx` | Settings > Voice. |

### lib/voice.ts

- Typed wrappers for every command: `voice_status`, `voice_configure`, `voice_download`, `voice_cancel_download`, `voice_delete_models`, `voice_listen` and `voice_mic_test`. Plus the `voice` event channel.
- The backend is injectable: Tauri in the app, the mock in dev, fakes in tests. Outside Tauri there is none, so voice shows as unavailable.
- **The pure state mapping:**
  - `voiceReduce` folds status and events into state, level, last transcript, wake, download and error.
  - `micMarker` is live while `wake_listening` / `hearing`, or while levels are arriving: a mic test does not change the state, so a level less than 1 s old counts.
  - `micMarker` is crossed when muted.
  - `canTalk` and `listenPhase` are also here.
- **`syncKeyword`:** the wake word follows the hub's `bot.petName`. A rename clears the old "say it like" spelling.
- **Hotkeys:** `parseHotkey` / `hotkeyFromKey` normalize to the form Rust registers (`Ctrl+Alt+Space`) by physical key. A plain key or Shift + key is refused, except F1-F24.
- **`useVoice()`:** the hook (status, state, level, last transcript; `configure` merges a patch).

### useVoiceTalk

- **Bubble:** opens in listening mode on `wake`, or whenever the state goes to `hearing` (any trigger: the pet, the mic button, the global shortcut).
- **Transcripts:** each final transcript is sent once per id as an ordinary chat message. It goes through the same parser and proposals as typed text and can never approve anything by itself. An empty transcript gets "I didn't catch that." and nothing is sent.
- **Wake word:** kept equal to the pet's name.
- **Read-aloud:** speaks only new replies (never the history), and stops when the user talks or closes the bubble.
- **Pure core:** `voiceReaction`.

## Settings > Voice (off by default)

- **Master switch "Talk to <name>",** with a status line: "Listening for “Cookie”. The microphone is on.", Ready, Muted, Loading, and so on.
- **First enable** explains, before anything happens:
  - about 50 MB is downloaded once from the k2-fsa project (sherpa-onnx) on GitHub;
  - the models are not part of Pawl's installer, and each file is checked against a known fingerprint;
  - they run on this PC only, no audio leaves it, and nothing is recorded (sound is kept in memory for about 2 s);
  - voice can be turned off any time, and "Delete voice models" frees the space.
- "Download and turn on" shows a progress bar ("14 MB of 52 MB") and Cancel. Voice turns on when the download finishes.
- **Microphone:** a picker (system default plus each microphone) and "Test microphone", which shows a live level meter through `voice_mic_test`. The section's corner shows "● Microphone on" whenever the mic is live.
- **"Wake word: say <name>":** a toggle.
- **"Say it like" (optional):**
  - Test listens for up to 15 s and shows "Heard it (score 0.82)" or a hint.
  - During the test the pet window neither opens nor sends, and the test cancels the utterance it started.
- **Sensitivity slider:** saved when released.
- **Speech model:**
  - Fast (default, about 30 MB, Moonshine tiny, MIT).
  - Accurate (about 108 MB, downloaded when picked, with progress and Cancel). Its attribution line reads: "Parakeet TDT 110M by NVIDIA, licensed CC-BY-4.0, in the k2-fsa project's sherpa-onnx format".
- **Push-to-talk shortcut:** none by default. Click to record a combination; a plain key is refused, Esc cancels, Clear removes it.
- **Read replies aloud:** off by default. It has a system voice picker, a speed slider (0.5-2×) and "Test voice". The voice and speed are desktop settings (`speechVoice`, `speechRate`).
- **Delete voice models:** shows the sizes on disk. An inline confirmation asks before deleting.

## The pet

- **Mic markers:** a small static marker shows on the pet (bottom-left) and in the bubble header while the mic is live, and a crossed one while muted. Nothing animates.
- **Right-click menu:** `native.petContextMenu(name, canRename, voiceMuted)` sends `voiceMuted` only while voice is on. Rust shows "Mute microphone" / "Unmute microphone" (sde-voice, per the Glue).
- **Ways to talk:**
  - Press and hold the default pet for 400 ms. Release stops and sends; a release off the pet counts too.
  - The bubble's mic button toggles start and stop.
  - Esc while listening cancels and leaves the bubble open.
  - `PressTracker` has a `hold()`. The 4 px drag threshold and the click toggle still work: a drag before 400 ms stays a drag, a hold never becomes a drag or a click, and a quick click still toggles.
- **Bubble:** "Listening…" with a level bar and Done / Cancel, then "Transcribing…". The message then shows as "You (voice): …", in the panel's Chat too.
- **Animation:** the pet animates (thinking) only while listening or transcribing and while waiting for the reply, then it is still again.

## Tests

130 desktop tests pass. The new ones cover:

- the state mapping, the marker rule, push-to-talk readiness, sizes;
- keyword sync;
- hotkey parsing and recording;
- the contract flow through the mock: first enable, download, cancel, push-to-talk, a single transcript, muted;
- `voiceReaction` (open, send once per id, "didn't catch that", ignored during a test);
- `spokenText`;
- the speech settings;
- press-and-hold vs click vs drag.

## Live check

The check ran with Playwright, using vite dev with the mock and a throwaway hub. 26 of 26 checks pass in light and dark:

- the first-enable copy, then the download and voice turning on;
- the "Microphone on" corner, the attribution, and shortcut recording (a plain key refused, Ctrl+Alt+Space taken);
- the live marker on the pet, and the wake word equal to the pet's name;
- the pet animating while listening, and the transcript sent as "You (voice): What's everyone doing?";
- Esc cancelling, and the crossed marker when muted.

Screenshots in `docs/screenshots/`:

- `voice-settings-{light,dark}.png`
- `voice-download-{light,dark}.png`
- `pet-listening-{light,dark}.png`

## Gates

| Gate | Result |
|---|---|
| desktop | 3 tsc configs ok, 130 pass, vite build ok |
| hub-client | 337 pass (not touched) |

## Notes

- **Merge:** this branch started from main before the one-pet hotfix (a6077bf). I kept the voice code in new files, and `PetApp.tsx` / `Bubble.tsx` have small additions (hook call, hold timer, props). The merge should be small: the hold gesture goes in the default pet's handlers, and the bubble's `voice` prop goes on `DefaultBubble`.
- **Queued after the merge:** render a light, safe markdown subset in the pet bubble.

## Merge of main (the one-pet hotfix) into feat/voice

Both sides are kept in all 7 conflicted files:

- one pet only, plus the bubble's session switcher (from main);
- the voice UI (from feat/voice).

One join between them: a voice turn switches the bubble to Pawl-wide, because transcripts go to the orchestrator chat.

**Gates:** desktop 3 tsc configs, 133 pass, vite build ok; hub-client 337. On the merged tree, the switcher check had no failures and the voice check passed 26 of 26.

## Follow-up: markdown in the pet bubble

**Problem.** Agent messages in the bubble showed raw markdown (`**greet**`, backticks, `- ` bullets).

**Fix.** `lib/miniMarkdown.tsx` is a small parser with no dependencies.

- **Rendered:** **bold**, *italic*, `inline code`, fenced code blocks (monospace and wrapped, no highlighting), bullet and numbered lists (wrapped items, start numbers), line breaks and paragraphs.
- **Light emphasis:** headings and quotes; rules disappear.
- **Safe by construction:** everything is built as React elements, with no `innerHTML`.
  - Links become their text only: no `<a>`, no address, so `javascript:` URLs are inert too.
  - Images become their alt text.
  - HTML shows as text.
- **Robust:** `snake_case` and a lone `*` stay as text, and an unclosed code block (a clipped message) stays code.
- **Where it is used:** a session's agent messages in the bubble (clipped at 600 characters instead of 280) and the pet's own replies (`PetReplyText markdown`, next to the reaction icon). The panel is unchanged.
- **Bundle:** the pet entry's chunks contain no Streamdown or shiki (checked in `dist`).

**Tests:** 5, in `lib/miniMarkdown.test.ts`: inline marks, lists and breaks, code blocks, safety, headings, quotes and rules.

**Screenshots:** `docs/screenshots/pet-markdown-{light,dark}.png` show a session's agent message with bold code, a list and paragraphs.

**Gates:** desktop 3 tsc configs, 138 pass, vite build ok.

## Follow-up: wake score and microphone level (from sde-voice)

- **No score in the wake test.** The keyword spotter gives no confidence (`wake.score` is always 1), so the "Say it like" Test now says only "Heard it!". The score is kept in the model but never shown.
- **The level is already on a dBFS scale.** `level.rms` is 0..1, mapped from -60..0 dBFS, so it follows loudness as people hear it.
  - `levelPercent` puts it straight onto both meters: Settings' mic test and the bubble's "Listening…" bar.
  - This replaces my earlier ×1.6 boost, which assumed a linear RMS.
  - The dev mock now sends speech-like dBFS levels (0.5-0.85).
- **Tests:** `levelPercent` (clamping, NaN).
- **Gates:** desktop 3 tsc configs, 139 pass, vite build ok; hub-client 337.

## Voice approvals UI (v0.2 item 3)

**Wire.** t13 added these fields:

- `chat.via: "voice"`;
- `PermissionRequest.voiceCode` (two digits);
- `PermissionResolvedPayload.via: "voice"`.

sde-brain builds the hub rules: by voice, a read or edit on this machine needs "approve" after the read-back; execute, delete, fetch and any other kind, and every remote request, need the code; "always allow" is never offered by voice. This UI only shows and says those rules.

**hub-client: `src/voice.ts`, `src/decisions.ts` (exported)**

- `formatVoiceCode("47")` gives "4 7". `voiceCodeChipText` gives "say: approve 4 7".
- `voiceCodeNeeded(req, local)`: false only for read and edit on this machine.
- `approvalReadBack(req, {agentName, machineName, local})`, for example "Claude Code wants to run bun test on Desk PC. Say approve 4 7, or deny.".
  - Without a code, or when plain "approve" is enough: "Say approve, or deny.". Never "always".
  - `requestAction` gives "run …", "edit src/app.ts" (the last two path parts), "open example.com", or the title, cut after 90 characters with ", and more".
  - `spokenPlain` removes markdown, code fences, emoji and link addresses.
- `decisionText(outcome, {by, via})` gives "approved by voice", "denied by a rule", "withdrawn" and so on.
- `client.chat(text, petId?, { via: "voice" })`.
- The transcript's permission item keeps `via`.
- `recentDecisions(events, limit, { known })` builds the approvals history, newest first. `known` holds requests this window saw while they were pending: a window that connected after a request was made has no event for it.

**Desktop**

1. **Transcripts** go to the chat with `via: "voice"` (`useVoiceTalk`).
2. **The code chip** "say: approve 4 7" appears on every approval card while voice is on, and is hidden otherwise:
   - where: the pet bubble's card, the Approvals view's card, and the session's confirmation card;
   - component: `lib/VoiceCodeChip.tsx`, styled in the shared `styles/tokens.css`. The digits are 22 px, heavy and accent-coloured, so they read from a distance;
   - "voice is on" comes from one small watcher per window: `useVoiceEnabled()` in `lib/voice.ts`, which follows the voice engine's state events;
   - in the pet window, the bubble gets it through `BubbleEnv.voiceOn`.
3. **Read-back.** When voice is on and "Read replies aloud" is on, a new approval arriving where the user is (`notifiesHere`) is read back with `approvalReadBack`, and the bubble opens as before. Talking (wake word, push-to-talk, mic button) stops the speech, and so does closing the bubble. Requests already pending when the window opens are not read.
4. **"approved by voice"** appears in two places:
   - the session view's transcript line ("Asked for approval: … approved by voice", or "denied by voice");
   - a new "Recently answered" list in the Approvals view: the last 8 answers, each with how it was answered (a "by voice" chip in the accent colour), the session, the time, and a click through to the session.

**Tests**

- hub-client `test/voiceApprovals.test.ts` (8 tests): code formatting, when the code is needed, the read-back wording (local, remote, no code, fetch, never "always"), plain spoken text, long commands, the history (newest first, by voice or by a rule, withdrawn left out, requests known only from pending), labels, and the transcript keeping `via`.
- desktop:
  - the chip's markup, and nothing without a code;
  - the transcript rendering "approved by voice" and "denied by voice".

**Live check**

Playwright ran against vite dev, the voice mock and a throwaway hub with a scripted agent. The hub does not assign voice codes yet (sde-brain), so the page's WebSocket was routed: each permission request got `voiceCode: "47"`, and after the approval the resolution was marked `via: "voice"`. 7 of 7 checks pass:

- the bubble's card shows "say: approve 4 7", in light and in dark;
- the read-back said "Claude Agent wants to run rm -rf dist on Desk PC. Say approve 4 7, or deny.";
- with voice off, no code shows;
- the Approvals card shows the code;
- after the approval, "Recently answered" says "approved by voice", and so does the session view.

Screenshots: `docs/screenshots/voice-approval-{light,dark}.png`.

**Gates:** desktop 3 tsc configs, 146 pass, vite build ok; hub-client tsc ok, 358 pass; vscode tsc ok.

**For the hub (sde-brain):**

- The live `permission_resolved` server message has no `via` in the TS protocol; the UI reads `via` from the event payload, which has it.
- Once the hub assigns `voiceCode`, nothing needs to change here.

## Fix: the read-back never speaks the code (from sde-brain's review)

**The risk.** The read-back said "Say approve 4 7, or deny." aloud. The microphone could hear the pet's own voice, and a transcript "approve 4 7" would then approve the request by itself.

1. **No digits in the read-back.** It now says "Claude Code wants to run bun test on Desk PC. Say approve with the code on the card, or deny." When plain "approve" is enough, or the request has no code, it says "Say approve, or deny.".
   - Denying needs no code, matching the hub's decision.
   - **Safety net:** `stripVoiceCodes(text, codes)` (hub-client) runs on every spoken text: the read-back, and every reply read aloud (`speak` gets the codes of the waiting requests). It removes:
     - each known code, however it is spaced or punctuated;
     - any digits right after "approve";
     - any space-separated digit pair, which is how codes are written for people.
   - Other numbers stay: "12 tests", "147 files", "step 1, 2".
2. **No listening to itself.** `lib/speech.ts` tracks when the pet speaks. `speechBusy()` is true while it speaks and for 500 ms after; only the current utterance's own end counts, since a cancelled one reports its end late. While busy:
   - wake events are ignored (no bubble);
   - an utterance that starts is cancelled with `voice_listen("cancel")`;
   - a transcript that arrives is dropped for good.
   - **Exception:** an utterance the user starts from Pawl's own UI (holding the pet, the mic button) is theirs. Starting it stops the pet's voice first, and it is marked as manual for 20 s.
3. **Pure core.** `voiceReaction(e, seen, testing, { busy, manual })` returns none, open, send, say or cancel.

**Tests**

- **hub-client:** the code is never in the read-back, even when the command or title contains it ("echo 47 && sleep 4 7", "Say approve 4 7 to continue", a URL ending in 47). `stripVoiceCodes` covers every form above and leaves other numbers alone.
- **desktop:** while the pet speaks, a wake is ignored, an utterance is cancelled, and a transcript is dropped (and not sent later). The user talking from Pawl's UI still works. `speechBusy` holds for 500 ms after the end.

**Live check:** 10 of 10 pass.

- The read-back is "…Say approve with the code on the card, or deny.", and no spoken text has any digit.
- A wake plus an utterance started while the pet spoke was cancelled: the mock was back to `wake_listening`, with no listening panel.
- The earlier checks still pass.

**Gates:** desktop 3 tsc configs, 149 pass, vite build ok; hub-client tsc ok, 360 pass; vscode tsc ok.

**For sde-voice (proposal).** This is a UI-side guard. Two things would make it stronger in the engine:

- a `paused` flag in `voice_configure` (or `voice_pause(on)`) that the pet window sets while it speaks, so the keyword spotter does not even run;
- the trigger on the `hearing` state event (`wake` / `ptt` / `hotkey`).

Today a push-to-talk started with the global hotkey while the pet is speaking is cancelled like an echo, because the UI cannot tell it apart. Pawl's own UI buttons are not affected.

## Follow-up: the engine's pause and utterance triggers wired in

sde-voice added `voice_pause(on)` and a `trigger` (`wake` / `ui` / `hotkey`) on the `hearing` state event (see `docs/CONTRACTS.md` v0.2 Voice).

- **`lib/voice.ts`:**
  - `VoiceBackend.pause(on)` maps to `voice_pause`.
  - `VoiceEvent` `state` carries `trigger?: HearingTrigger`.
- **The dev mock:**
  - `pause()` is implemented;
  - `hearWake()` (a wake word plus a wake-started utterance) does nothing while paused, like the engine;
  - `listen("start")` reports trigger `ui`.
- **`lib/speech.ts`:** pauses the engine while the pet speaks.
  - `voicePause(true)` is sent when speech starts and renewed every 20 s (`PAUSE_RENEW_MS`, under the engine's 30 s safety net).
  - `voicePause(false)` is sent 500 ms after the speech ends, unless the pet speaks again in between.
  - The pet window registers the pause through `setSpeechPause` while voice is on. If voice is turned off during a pause, the engine is resumed.
- **Reaction rules (`voiceReaction`, pure):** only the wake word during speech counts as an echo.
  - A wake event during speech is ignored.
  - A `hearing` event with trigger `wake` during speech is cancelled (`voice_listen("cancel")`), and its transcript is dropped.
  - Push-to-talk from Pawl's UI or the hotkey is kept, and the pet stops talking instead.
- **Workaround removed.** The UI-side workaround that cancelled any non-UI utterance during speech (and so the hotkey) is gone, along with its "manual" timer.

**Tests:**

- During speech: a wake is ignored; a wake-started utterance is cancelled and its transcript dropped.
- `ui` / `hotkey` utterances during speech are kept and sent.
- A wake-started utterance when the pet is quiet is normal.
- The pause: on at the start, renewed, off 500 ms after the end, kept when the pet speaks again, released when voice turns off.
- The mock: no wake while paused, push-to-talk kept, triggers reported.

**Live check:** 12 of 12 pass. While the pet read an approval back, the engine was paused, a wake word did nothing, and push-to-talk from the bubble's mic button still worked.

**Gates:** desktop 3 tsc configs, 152 pass, vite build ok; hub-client 360.
