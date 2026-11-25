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
