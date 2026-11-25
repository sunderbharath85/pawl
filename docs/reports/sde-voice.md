# sde-voice: v0.2 voice engine (wake word = the pet's name, speech to text)

Branch `feat/voice`, worktree `E:\workspace\ai-pet-voice`. Nothing committed (t13 commits).

## Summary

The desktop app's Rust side now has an on-device voice engine that implements docs/CONTRACTS.md "v0.2 Voice"
(including the later additions: `voice_cancel_download`, transcript `id`, Rust-registered push-to-talk shortcut, the
`pet_voice_mute` item in the pet menu and the tray).

- **Pipeline:** cpal capture (the callback only down-mixes to mono) -> one worker thread: 16 kHz (sherpa
  `LinearResampler`) -> a 2 s ring buffer -> the keyword spotter while the wake word is on -> on the pet's name (or
  push-to-talk) an utterance with the audio from just before -> Silero VAD ends it after 0.8 s of silence (or 6 s
  with no speech, or 15 s, or "stop") -> the offline speech model -> a `transcript` event.
- **Wake word:** built at runtime from `config.keyword` (or `spokenAs`). The name is folded to the model's alphabet
  (accents folded, digits spelled out, apostrophes kept inside words), then encoded with the model's own `bpe.model`
  through kitoken, and every piece is checked against `tokens.txt`. It listens for "<name>" and "hey <name>". Boost
  and threshold come from `sensitivity`. A leading "hey <name>," is removed from the transcript.
- **Models:** downloaded only through `voice_download`, over HTTPS, from the pinned GitHub release URLs. The whole
  download is checked against a pinned size and SHA-256 before anything is unpacked. Only the listed int8/quantized
  files are extracted, under our own file names, into a temp directory that is renamed into
  `<app data dir>/voice-models/<model>/`. Downloads are cancellable and partial files are removed.
- **Privacy:** audio exists only in memory (the ring buffer and the current utterance). Nothing is written to disk
  except `voice.json` and the models the user downloads.
- **Mic rules:**
  - The mic is open only while `enabled && !muted && !screen locked && (wake word, or an utterance, or a mic test)`.
  - It is released on screen lock (Windows, macOS, Linux/logind; best effort).
  - Models load when voice is on and unload when it is off.
  - The engine uses one worker thread, plus cpal's audio thread, and runs ONNX with `num_threads = 1`.
- **Supply chain:** `scripts/fetch-sherpa.sh` fetches the sherpa-onnx native archive and checks it against a pinned
  SHA-256. `.cargo/config.toml` makes every build use that checked archive, and release.sh and CI use it too. The
  build never downloads an unchecked archive.

## Files

New:
- `apps/desktop/src-tauri/src/voice/mod.rs`: the Tauri commands, the managed `Voice` state, the event sink (status
  for `voice_status`, events to both windows, the tray), the push-to-talk shortcut, and mute from the menus.
- `voice/engine.rs`: the worker thread: models, mic, ring buffer, KWS, utterance/VAD, ASR, the `VoiceEvent` enum.
- `voice/state.rs`: a pure state machine (`Inputs` -> `VoiceState`, `mic_wanted()`, `can_listen()`).
- `voice/config.rs`: `VoiceConfig` (camelCase), strict validation for UI input, forgiving load from disk, atomic save.
- `voice/keyword.rs`: name normalization, kitoken BPE -> keyword lines, the sensitivity mapping, stripping the wake
  phrase.
- `voice/models.rs`: the model catalog (URLs, SHA-256, sizes, files to keep), install status, delete.
- `voice/download.rs`: the HTTPS download, verify-then-extract, atomic install, `CancelFlag`.
- `voice/capture.rs`: the cpal device list and stream (down-mix in the callback, non-blocking hand-off).
- `voice/hotkey.rs`: push-to-talk shortcut validation (the same rules as the UI's `parseHotkey`) and parsing.
- `voice/lock.rs`: the screen-lock check per OS.
- `apps/desktop/src-tauri/tests/no_bundled_models.rs`: fails if a model file is in the repo or named in the bundle
  config.
- `apps/desktop/src-tauri/.cargo/config.toml`: `SHERPA_ONNX_ARCHIVE_DIR` defaults to the checked-archive folder.
- `apps/desktop/src-tauri/Info.plist`: `NSMicrophoneUsageDescription` ("Pawl listens for your pet's name and to
  what you say to it, only while voice is on.").
- `apps/desktop/src-tauri/Entitlements.plist`: `com.apple.security.device.audio-input`.
- `scripts/fetch-sherpa.sh`: download the sherpa-onnx static archive for a target and check its pinned SHA-256.
  It prints the folder and appends to `$GITHUB_ENV` in CI.
- `scripts/check-no-models.sh`: fails on model files that are git-tracked or inside the given folders (built
  bundles).

Changed:
- `Cargo.toml` / `Cargo.lock`: the approved dependencies only (see below).
- `lib.rs`: `mod voice`; the global-shortcut plugin, with a handler and no JS permission; the 7 voice commands;
  `voice::init` in setup (the app still runs if voice fails to start).
- `tray.rs`: a "Mute microphone" / "Unmute microphone" item, greyed out while voice is off, and the tooltip
  "Pawl - microphone on" while the mic is live. Its handler also serves the pet menu's `pet_voice_mute`.
- `pet_window.rs` (CRLF kept): `pet_context_menu` takes an optional `voiceMuted`. When it is present, the menu has
  the `pet_voice_mute` item.
- `tauri.conf.json` (CRLF kept):
  - `bundle.macOS.minimumSystemVersion = "14.2"` (cpal 0.18's CoreAudio minimum, which t13 accepted) and
    `bundle.macOS.entitlements`.
  - `bundle.linux.deb.depends = ["libasound2"]` and `bundle.linux.rpm.depends = ["alsa-lib"]`.
- `scripts/release.sh`: fetch-sherpa before `tauri build`, then check-no-models on the bundle folder and the
  `nsis/` / `wix/` staging folders.
- `README.md`: the macOS 14.2 floor and `libasound2-dev` in the requirements, and `fetch-sherpa.sh` in the desktop
  step (t13 asked for the README note).
- `.github/workflows/release.yml`: `libasound2-dev` on Linux, and a "Voice engine library (pinned SHA-256)" step.
- `.github/workflows/e2e.yml`: a "No voice models in the repo" step (`check-no-models.sh`, runs on every PR).

## Dependencies (`apps/desktop/src-tauri` only, all from the approved list)

- `sherpa-onnx ~2.0.3`: static (the default feature), resolved to 2.0.3.
- `kitoken 0.11` (0.11.1): `default-features = false`, with `std`, `normalization` and `convert-sentencepiece`.
- `cpal 0.18` (0.18.2): default features. Linux uses ALSA, which PipeWire and PulseAudio serve through their ALSA
  plugins, so neither cpal feature is needed.
- `reqwest 0.13` (0.13.5): `default-features = false`, with `rustls` and `system-proxy`. No native-tls. The
  `stream` feature is not needed: `Response::chunk()` streams without it.
- `sha2 0.10`, `tar 0.4` (`default-features = false`, no xattr), `bzip2 0.6` (default backend = `libbz2-rs-sys`,
  pure Rust, no system libbz2).
- `tauri-plugin-global-shortcut 2` (2.4.1): Rust side only. No JS package and no capability.
- `hound` was approved as a dev-dependency but is not used: the tests read wavs with `sherpa_onnx::Wave` from the
  local models folder, and generate silence in code. No audio fixture is in the repo.

## Pinned hashes

sherpa-onnx v2.0.3 static archives (`scripts/fetch-sherpa.sh`). I downloaded each one, hashed it, and checked the
hash against GitHub's own asset digest (`releases/tags/v2.0.3`); all match.

| Archive | Size | SHA-256 |
|---|---|---|
| win-x64-static-MT-Release-lib | 127,590,743 | 2dfaeacf899e8d21584caab54a55933b481b5dccbb3615e92daecc6f0618e781 |
| osx-x64-static-lib | 21,611,475 | aa2669f175bc3b496788308e211e1bbb792ca7ba654f79db69bf20bef1d597a0 |
| osx-arm64-static-lib | 22,092,534 | c86468ad8f79b679550f0865cc1a67747f58b8cc8780623ed7d8367fd4b541a3 |
| linux-x64-static-lib | 24,301,893 | f927e695d29b8c7ea3fda97eed215204b210fd57f9faf990a93377bac098ad57 |

Models (`voice/models.rs`). I re-downloaded all four from the release URLs and hashed them. The hashes equal t13's
pins and GitHub's asset digests. The KWS asset has no digest on GitHub, so for it I only checked my own download.

| Model | URL tag | Size | SHA-256 |
|---|---|---|---|
| kws-zipformer-gigaspeech-3.3M-2024-01-01.tar.bz2 | kws-models | 17,626,723 | f170013b4716e41b62b9bfd809687c207cef798ef9bc6534d524e17af9b6561a |
| silero_vad.onnx | asr-models | 643,854 | 9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6 |
| moonshine-tiny-en-quantized-2026-02-27.tar.bz2 | asr-models | 29,858,559 | 9ec31b342d8fa3240c3b81b8f82e1cf7e3ac467c93ca5a999b741d5887164f8d |
| nemo-parakeet_tdt_transducer_110m-en-36000-int8.tar.bz2 | asr-models | 108,035,095 | f628312e9fdf8686374cb01a69425c41732529d540860311f16f37cbc32cfe9b |

Files kept:
- KWS: encoder, decoder and joiner `*.int8.onnx`, `tokens.txt` and `bpe.model` (5.5 MB). The int8 decoder works:
  the wake-word test passes with it.
- Moonshine: `encoder_model.ort`, `decoder_model_merged.ort`, `tokens.txt` and `LICENSE` (44 MB).
- Parakeet: encoder, decoder and joiner `*.int8.onnx` and `tokens.txt` (136.5 MB).
- VAD: the single file.

The default set downloads 48.1 MB (50 MB unpacked). Accurate downloads 108 MB (137 MB unpacked).

## Commands and results

All runs are in `apps/desktop/src-tauri` unless noted.

- `cargo test`: **86 passed** (83 lib + 3 in `tests/no_bundled_models.rs`). This is after t13's review fixes (see
  "Review fixes" below). Without the env vars, the model, archive and network tests print "skipped" and pass.
- With local models:
  `PAWL_VOICE_TEST_MODELS=<t13's smodels> PAWL_VOICE_TEST_ARCHIVES=<my downloaded archives> PAWL_VOICE_TEST_NETWORK=1 cargo test`
  gives **86 passed**. Debug build:
  - **Wake word:** the KWS test clip "... the yellow lamps would *light up* here and there the squalid quarter of
    the brothels" with the keyword "Light up".
    - The pipeline emits `wake`, then the states `hearing` and `transcribing`.
    - The transcript is "here and there the squalid quarter of the brothels." (the wake phrase is not part of it).
    - The state then returns to `wake_listening`.
    - 9.1 s of audio was processed in ~0.3 s.
  - **Push-to-talk, fast:** Moonshine's 24 kHz clip goes through the resampler.
    - Transcript: "Ask not what your country can do for you. Ask what you can do for your country."
    - 93 ms after "stop". Load 0.5 s.
    - The mic opens on start and closes after.
  - **Push-to-talk, accurate:** Parakeet transcribes its clip correctly, 292 ms after "stop". Load 1.1 s.
  - **Cancel** gives no transcript. **Silence only** gives an empty transcript after 6 s. Transcript ids are unique.
  - **kitoken** gives the same pieces as upstream's Python tool: `▁HE Y ▁S I RI`, `▁ L IGHT ▁UP` (both from the
    model's own keyword files), `▁BI S C U IT`, `▁K RY P T O`. Odd names (Zoë, R2-D2, O'Neil, Xyzzy,
    "Mr. Fluffington III") encode too.
  - **Real archives** through `install_from_file` (pinned size and hash, extraction): all four pass. This caught a
    real bug: the Parakeet archive stores `./dir/file`, and leading `./` is now accepted.
  - **Live download** from GitHub (`download_model`, VAD): it follows the redirect to
    `release-assets.githubusercontent.com` and installs. This now goes through the host allowlist. A cancel part-way
    through the KWS download leaves nothing behind.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `bunx tauri build --debug --no-bundle` (in `apps/desktop`, after t13's merge): OK, 2m00s. Run again after the review
  fixes: OK.
- `bash scripts/fetch-sherpa.sh`: "already present and verified" (it was seeded from my checked download). Prints
  `E:/workspace/ai-pet-voice/apps/desktop/src-tauri/target/sherpa-onnx-archives`.
- `bash scripts/check-no-models.sh`: ok on the repo. It fails, as it should, on a folder holding the model archives.
- Root workspace (`cargo check --workspace --all-targets` at the repo root): green (1m33s). No root crate changed.

## Exe size

Windows x64, `pawl-desktop.exe`. The baseline is HEAD's `src-tauri`, unpacked with `git archive` into the
scratchpad and built there with its own target dir. Both builds use the same frontend `dist`.

| Build | Before (HEAD) | After (voice) | Change |
|---|---|---|---|
| Release (`cargo build --release --features tauri/custom-protocol`: LTO, opt-level s, strip) | 8,398,848 B (8.0 MiB) | 31,352,832 B (29.9 MiB) | **+22,953,984 B (+21.9 MiB)** |
| Debug (`tauri build --debug --no-bundle`) | 22,375,936 B | 60,502,528 B | +38.1 MB |

Almost all of the growth is the statically linked ONNX Runtime plus sherpa-onnx (the static library archive is 128 MB
on Windows). The macOS and Linux archives are about 22-24 MB compressed, so expect a smaller growth there, but it is
not measured. No model is in the binary or the bundle: they are downloaded on first enable.

Checked: the build took the sherpa-onnx archive from the pinned folder. The build script's log has no
"Downloading … from" line, and its cached copy has the pinned SHA-256 `2dfaeacf…e781`.

## Decisions

- **`voice_download` resolves when the download is done.**
  - On a cancel it also resolves `Ok` (the user chose it). Status still shows the models as not installed. This fits
    fe-settings' code, which turns voice on only once `status.models.*.installed` is true.
  - On a failure it rejects with the reason and also emits an `error` event.
  - Downloading the same set again while one runs is `Ok` at once. A different set is refused.
  - With nothing missing it returns `Ok` at once.
- **What a set downloads:** "accurate" downloads Parakeet, plus the KWS and VAD models if they are missing.
  `models.accurate.installed` reports Parakeet only. The state is `missing_models` while any model the current config
  needs is absent: KWS, VAD and the chosen speech model.
- **`level.rms` is 0..1:** the block's RMS on a -60..0 dBFS scale, so the meter moves at normal speech levels (raw RMS
  is ~0.01-0.1). At most 20 per second, and only while `hearing` or during a mic test.
- **`wake.score` is always 1.0.** Sherpa's keyword spotter reports no confidence: its result has only the keyword,
  tokens and timestamps. fe-settings' "Heard it (score 0.82)" will show 1.00; it should drop the number.
- **The mic test needs voice on and unmuted.** Contract rule: the mic is never opened while off or muted. It stops by
  itself after 2 minutes if the UI forgets to stop it, and does not change the state.
- **Push-to-talk:**
  - Allowed in `idle` / `wake_listening` only. Otherwise it fails with a reason: off, muted, models missing, loading,
    or screen locked.
  - It includes 0.3 s from before the press.
  - A repeated "start" (key repeat) is ignored.
  - Every utterance, from the hotkey, the pet or the mic button, also ends on silence.
  - Hotkey failures (for example while muted) come as `error` events.
- **After the wake word:**
  - The utterance starts where the name ended (from the spotter's timestamps), at most 1 s back. That way a pause after
    "hey Biscuit" does not end the utterance with just the name.
  - If the timing looks wrong, it falls back to 0.3 s back.
  - As a backstop, a leading "hey <name>" is stripped from the text.
- **The spotter is rebuilt** when the name or the sensitivity changes, instead of using
  `create_stream_with_keywords`. That function adds the spotter's default keywords to the stream, and the spotter
  calls `exit()` when given a keyword it cannot encode. Only keyword lines whose every piece is in `tokens.txt` reach
  it.
- **Sensitivity mapping:** boost = 0.5 + 2.0·s, threshold = 0.40 - 0.30·s. At the default 0.5 this gives boost 1.5
  and threshold 0.25.
- **A name the model cannot spell** (emoji, other scripts, empty) does not block voice. Push-to-talk still works, and
  `status.error` says to add a "say it like" spelling.
- **What can fail `voice_configure`:** only an invalid config (range, length, control characters, an unparsable
  hotkey) fails it, and then nothing is applied. A valid config is always handed to the engine first, so mute and off
  always take effect. Only then is it saved, and the shortcut registered.
  - A save error or a shortcut another app holds is a warning: it shows in `status.error` and comes as an `error`
    event (once). The change stays applied, and the config keeps the chosen shortcut.
  - The shortcut is only touched when `hotkey` or `enabled` changed. Mute never depends on it.
- **The push-to-talk shortcut** is registered only while voice is on, and released at exit.
- **A missing chosen mic** falls back to the system default device.
- **Screen lock:** polled every 1 s (3 s on Linux via `loginctl`) while voice is on and unmuted, also in
  push-to-talk-only mode. It is checked again right before push-to-talk starts and before a mic test. On lock, the
  utterance and the mic test are cancelled.
- **Threads and the main thread:** the global-shortcut plugin and tray updates wait for the main thread. So no voice
  lock is held across those calls, and menu and hotkey handlers never wait on the worker: they hand the work to a
  thread. Review found a deadlock between changing the shortcut and pressing it, and it is fixed.
- **No `stream` feature on reqwest, no `futures-util`:** `Response::chunk()` with a small `CancelFlag` future wrapper
  does the job, and a cancel takes effect even while waiting on the network.

## Review fixes (t13's review, round 1)

1. **Important: mute failed when the shortcut could not be registered.**
   - Before: `apply_config` returned early when registration failed, before saving and before telling the engine. A
     taken push-to-talk shortcut therefore blocked mute and every other change, and the mic stayed open.
   - Now the config is applied to the engine first. Saving and the shortcut come after it, and their failures are
     warnings. The shortcut is only touched when `hotkey` or `enabled` changed, so mute never depends on it.
   - Shortcut registration goes through a small `Registrar` trait (the plugin in the app, fakes in tests).
   - Tests (real engine thread, pretend mic):
     - `mute_works_when_the_shortcut_cannot_be_registered`: the shortcut is taken, a mic test opens the mic, mute
       closes it and is saved. After that, a change that unmutes and picks another taken shortcut still applies.
     - `mute_works_when_the_settings_cannot_be_saved`
     - `invalid_config_changes_nothing`
     - `shortcut_follows_the_hotkey_and_the_master_switch`: unrelated changes never touch the shortcut.
2. **The engine now shuts down.**
   - `RunEvent::Exit` (lib.rs now builds the app and runs it with an event handler) calls `voice::shutdown`.
   - Shutdown releases the shortcut, cancels a running download and stops the push-to-talk thread. It sends
     `Cmd::Shutdown` (the worker unloads: mic closed, models freed) and joins the worker, waiting at most 3 s.
   - Test: `shutdown_stops_the_engine_and_closes_the_mic`.
3. **Push-to-talk order.** Each press or release used to spawn its own thread, so they could arrive out of order.
   Now they go in order through one channel to one thread, which waits for each answer and reports refusals as
   `error` events. Test: `hotkey_thread_forwards_in_order_and_reports_refusals`.
4. **Screen lock in push-to-talk-only mode.** The lock is now polled whenever voice is on and unmuted, and checked
   again right before Start and the mic test. The OS probe is injectable. Test:
   `screen_lock_is_checked_without_the_wake_word_and_right_before_listening`.
5. **Low items:**
   - **Download hosts.** Downloads and every redirect must be HTTPS on `github.com`, `objects.githubusercontent.com`
     or `release-assets.githubusercontent.com`, without credentials or a port, and at most 5 redirects. Test:
     `only_https_github_hosts_are_allowed`; the live download passes.
   - **Overflow.** `wake_start` and `speech_range` use saturating math.
   - **NUL bytes.** Sherpa's config conversion panics on an interior NUL (`CString::new(..).unwrap()`). Model paths
     and keyword strings with a NUL are now refused before they reach it. These were the only panicking calls on the
     voice thread I found; the result parsers return `Option`.
   - **Linux packages.** `bundle.linux.deb.depends = ["libasound2"]`; on Ubuntu 24.04 this is a virtual package
     provided by `libasound2t64`. `bundle.linux.rpm.depends = ["alsa-lib"]`.
   - **Installer check.**
     - `check-no-models.sh` now also reads the NSIS `.nsi` and WiX `.wxs` scripts, whose file lists are what the
       Windows installers contain. It also lists `.deb` and `.rpm` contents when `dpkg-deb` / `rpm` is installed.
     - `release.sh` checks the bundle folder plus the `nsis/` and `wix/` staging folders.
     - Checked against fake staging trees, both failing and passing.
   - **README.** The requirements now state macOS 14.2 or later and `libasound2-dev` on Linux. Desktop step 6 now
     includes `bash scripts/fetch-sherpa.sh`, and the README says the models are downloaded from Settings.

## Follow-up: pause + trigger (t13, after round 1)

1. **`trigger` on `hearing`.**
   - The `state` event for `hearing` now carries `trigger`: `"wake"` (the pet's name), `"ui"` (`voice_listen` from
     the webview) or `"hotkey"` (the global push-to-talk shortcut). Other states have no `trigger` field.
   - `Cmd::Listen` now says who asked. The engine's public `Trigger` enum replaces its private Wake/Push.
   - A change of trigger alone (one utterance dropped and another started between two publishes) still emits a
     state event.
   - "Mute microphone" re-emits the state with its trigger (kept in `Core`).
2. **`voice_pause(on: bool)`** (new command, registered in `lib.rs`):
   - **On:** keyword spotting is suspended (audio is not fed to the spotter, so no `wake` events). An utterance the
     wake word started is cancelled, without a transcript. Push-to-talk from the UI or the shortcut still starts
     while paused, and an utterance it started is kept, also across further pauses.
   - **The mic stays open while paused.** It is not reopened when the pet stops talking, and the push-to-talk
     pre-roll stays available. Pausing never opens it.
   - **Off, or 30 s after the last `on`** (the safety net, checked on the 250 ms tick, also while voice is off):
     spotting restarts with a fresh stream, because the old one has a gap and may hold half a keyword.
   - **`on` again** extends the pause to 30 s from then.
   - **The state is unchanged by pausing itself.** The only change is when a wake utterance is dropped:
     `hearing` → `wake_listening`.
3. **Contract:** docs/CONTRACTS.md "v0.2 Voice" now lists `voice_pause` and the `trigger` field (CRLF kept).
4. **Tests:**
   - `pause_ends_on_request_or_by_itself_and_keeps_the_state` (no models): a pause extends, ends on request, ends
     by itself after `PAUSE_MAX`, never changes the state and never opens the mic.
   - `events_serialize_like_the_contract`: `trigger` for each value, and no field without one.
   - Real models (`PAWL_VOICE_TEST_MODELS`):
     - `wake_word_then_transcript_with_real_models` first feeds the clip while paused: no wake, no transcript, mic
       still open. Unpaused: wake, `hearing` with `trigger: "wake"`, transcript. Then it pauses right after a second
       wake hit: the state is back to `wake_listening`, with no transcript and no more wakes.
     - `push_to_talk_with_real_models` checks `trigger: "ui"`. A shortcut press while paused gives
       `trigger: "hotkey"`, is kept across a second pause, and gives a transcript (fast and accurate models).
5. **Gates:** `cargo test` **87 passed** (84 lib + 3), run with `PAWL_VOICE_TEST_MODELS` so the real-model tests
   ran too. `cargo clippy --all-targets -- -D warnings` is clean.
6. **UI side (not mine):** `apps/desktop/src/lib/voice.ts` needs a `voicePause(on)` call and the optional `trigger`
   on the state event.

## Known gaps

- **Not run on macOS or Linux.**
  - The macOS screen-lock FFI (`CGSessionCopyCurrentDictionary`), cpal on CoreAudio and ALSA, and the Linux and macOS
    archive pins have not been compiled or run. Only the Windows build and tests ran here.
  - The release workflow will be the first real build on those targets.
  - The macOS app is built with deployment target 14.2: Tauri sets `MACOSX_DEPLOYMENT_TARGET` from
    `minimumSystemVersion`.
- **No live microphone test.** Capture was exercised with a fake mic. A real mic, device unplug and macOS's
  permission prompt need a manual run of the app.
- **Linux:** `global-hotkey` uses X11, so a pure Wayland session may get no global shortcut. Not tested.
- **Windows model paths:** model paths go to sherpa as UTF-8. Profile paths with non-ASCII characters are not tested.
- **The cargo cache is trusted:** sherpa's build script reuses an already unpacked
  `target/sherpa-onnx-prebuilt/<stem>/lib` without checking it. Every unpack now comes from the checked archive, but a
  target dir from before this change, or a restored CI cache, is trusted as it is.
- **A missing archive is not tested:** a build where neither the checked archive nor the unpacked cache exists
  should fail with "SHERPA_ONNX_ARCHIVE_DIR does not contain expected archive" instead of downloading. That is what the
  sherpa-onnx-sys 2.0.3 build script does, but I did not run it here.
- **Load time:** loading models (0.5-2 s) runs on the worker. Commands that arrive during a load wait for it (20 s
  timeout).

## Documentation used

- sherpa-onnx:
  - releases v2.0.3 / kws-models / asr-models: https://github.com/k2-fsa/sherpa-onnx/releases/tag/v2.0.3,
    https://github.com/k2-fsa/sherpa-onnx/releases/tag/kws-models,
    https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models
  - Rust examples (`keyword_spotter.rs`, `moonshine_v2.rs`, `nemo_parakeet.rs`):
    https://github.com/k2-fsa/sherpa-onnx/tree/v2.0.3/rust-api-examples/examples
  - crate docs: https://docs.rs/sherpa-onnx/2.0.3/sherpa_onnx/ (the `SHERPA_ONNX_ARCHIVE_DIR` / static setup comes
    from the crate docs and the `sherpa-onnx-sys` 2.0.3 build script)
  - keyword format (`:boost #threshold @phrase`): https://k2-fsa.github.io/sherpa/onnx/kws/index.html
  - KWS implementation (stream keywords merged with defaults, timestamps, reset):
    https://github.com/k2-fsa/sherpa-onnx/blob/v2.0.3/sherpa-onnx/csrc/keyword-spotter-transducer-impl.h
- kitoken: https://docs.rs/kitoken/0.11.1/kitoken/
- cpal:
  - docs: https://docs.rs/cpal/0.18.2/cpal/
  - minimum OS versions: https://github.com/RustAudio/cpal/blob/v0.18.2/README.md
- reqwest: https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html
- tar: https://docs.rs/tar/0.4.46/tar/
- bzip2: https://docs.rs/bzip2/0.6.1/bzip2/read/struct.MultiBzDecoder.html
- tauri-plugin-global-shortcut: https://v2.tauri.app/plugin/global-shortcut/,
  https://docs.rs/tauri-plugin-global-shortcut/2.4.1/tauri_plugin_global_shortcut/
- Tauri macOS bundle (Info.plist merge, entitlements, minimumSystemVersion):
  https://v2.tauri.app/distribute/macos-application-bundle/, https://v2.tauri.app/reference/config/
- Apple:
  - https://developer.apple.com/documentation/bundleresources/information-property-list/nsmicrophoneusagedescription
  - https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.device.audio-input
  - https://developer.apple.com/documentation/coregraphics/cgsessioncopycurrentdictionary()
- Windows: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-openinputdesktop
- logind `LockedHint`: https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html
- reqwest redirect policy: https://docs.rs/reqwest/0.13.5/reqwest/redirect/struct.Policy.html
- Tauri deb/rpm `depends`: https://v2.tauri.app/reference/config/ (`DebConfig`, `RpmConfig`); Ubuntu's
  `libasound2` virtual package: https://packages.ubuntu.com/noble/libasound2
- Tauri app events (`RunEvent::Exit`): https://docs.rs/tauri/2.12.7/tauri/enum.RunEvent.html

# v0.2 item 4: screen capture engine (branch feat/screen-browser)

## Summary

- **Commands** (`apps/desktop/src-tauri/src/screen/`), as in docs/CONTRACTS.md "v0.2 Screen and Browser":
  - `screen_sources() -> { id, kind: "screen" | "window", title, app, thumb }[]`
  - `screen_capture(id) -> { mediaType, data, width, height }`
- **Pictures live in memory only:** never written to disk, never logged (no logger is installed), and not kept
  after the command returns.
- **Files:**
  - `src/screen/mod.rs`: the commands, the "offered ids" check, and keeping Pawl's windows out of screen captures.
  - `src/screen/sources.rs`: listing, filtering and capturing by id (xcap).
  - `src/screen/encode.rs`: scaling, JPEG/PNG choice, the 4 MiB limit, thumbnails, base64.
  - `src/screen/permission.rs`: the macOS Screen Recording permission.
  - Glue in `lib.rs`; `Cargo.toml` / `Cargo.lock`.
- **Outside src-tauri** (Linux build packages, needed for the Linux compile; please check):
  - `.github/workflows/release.yml` (apt packages)
  - `tauri.conf.json` deb/rpm `depends`
  - one README requirements line
- **Dependencies:**
  - `xcap = { version = "0.9.10", features = ["wgc"] }`
  - `image = { version = "0.25", default-features = false, features = ["png", "jpeg"] }` (already in the tree
    through tauri, same 0.25.10)
  - base64 is a small built-in encoder, not a new crate.

## Behaviour

- **`screen_sources`:**
  - Lists every screen (the main one first: "Screen 1 (DELL ...)"), then up to 40 windows of other apps, front to
    back. Each has a thumbnail: PNG, long side at most 240 px, base64.
  - Left out:
    - Pawl's own windows (same process)
    - minimized windows
    - windows with no title (taskbar, tool windows)
    - windows smaller than 64 px
    - on macOS, the system's own surfaces (Dock, Window Server, Control Center, ...)
  - xcap already skips invisible and cloaked windows, which includes windows on other virtual desktops.
  - A window whose thumbnail can't be taken is left out, because it couldn't be captured either.
- **`screen_capture(id)`:**
  - Accepts only an id from the latest `screen_sources` list, at most 10 minutes old; otherwise it says "Pick the
    screen or window again." It is not a security boundary, but a capture always follows the picker.
  - A window is checked again (still open, not minimized, not Pawl's).
  - It is then captured, scaled so the long side is at most 2048 px (bilinear), and encoded as JPEG at quality 85,
    or PNG if that is smaller.
  - Over 4 MiB: JPEG at 75/65/50, then 3/4 the size each step, down to 256 px; otherwise an error. It never loops
    endlessly.
- **Pawl's bubble is kept out of screen captures.**
  - While a screen is captured (and while the picker's screen thumbnails are taken), every Pawl window gets Tauri's
    content protection: `WDA_EXCLUDEFROMCAPTURE` on Windows, `NSWindowSharingNone` on macOS. It is restored right
    after.
  - The windows stay visible on the monitor. Without this, every full-screen capture would contain the picker
    itself.
  - It is applied on the main thread, and the capture waits for that. Linux has no such switch.
- **Alpha:**
  - Windows (WGC) and macOS give premultiplied alpha (rounded corners, transparent windows, the macOS shadow). It is
    laid over white, so those parts don't turn black.
  - Linux alpha is ignored.
- **macOS Screen Recording permission:**
  - Checked with `CGPreflightScreenCaptureAccess` before listing or capturing.
  - If it is missing, Pawl calls `CGRequestScreenCaptureAccess` and returns: "Pawl needs permission to see your
    screen. Turn on Pawl in System Settings > Privacy & Security > Screen & System Audio Recording (Screen
    Recording on macOS 14), then quit and reopen Pawl."
  - What the user sees: the first time, the system alert "Pawl would like to record this computer's screen and
    audio" (Open System Settings / Deny). After turning Pawl on there, macOS offers "Quit & Reopen"; the grant only
    takes effect after a restart.
  - On macOS 15, the system also asks again from time to time (monthly): "... is requesting to bypass the system
    private window picker and directly access your screen and audio" (Allow For One Month).
  - No Info.plist key or entitlement is needed (Pawl is not sandboxed), so none was added.

## Decision: Windows Graphics Capture (`wgc`), not GDI

I measured this with a temporary probe on this machine (Windows 11 build 26300). It printed only app names, sizes
and pixel statistics, and was removed afterwards.

- **GDI's PrintWindow** (xcap's default) returned GPU-drawn windows black: Task Manager was 98% black with alpha 0.
  It also cropped windows by 8 px.
- **With `wgc`,** Task Manager is correct and windows come out at full size.
- **Edge and VS Code** were dark either way. That fits Chromium pausing painting while its window is covered, which
  no capture API can work around.
- **Yellow border:** none was seen. I sampled the top screen edge with GDI 316 times during 31 WGC captures of the
  front window. xcap asks for no border (`IsBorderRequired = false`). Microsoft documents that a border may still
  show without the user's consent (always on Windows 10), so a yellow frame may flash around the window during a
  capture on some machines.
- **Exclusion check:** a magenta test window with `WDA_EXCLUDEFROMCAPTURE` is absent from both a GDI and a WGC
  screen grab within 5 ms of the call, and present again right after resetting. So the exclusion works and needs no
  long wait (20 ms margin).

## Tests and gates

- **Unit tests:**
  - scaling (aspect ratio, no upscaling, no 0 px, no overflow)
  - alpha (ignored / over white)
  - scaling to 2048
  - PNG for flat UI, JPEG for photo-like images, always the smaller of the two
  - the 4 MiB logic (lower quality, then smaller, then an error for the impossible case)
  - thumbnails
  - base64 against the RFC 4648 vectors
  - source ids (round trip, junk rejected)
  - the window filter
  - screen titles
  - serialization of the commands' JSON
  - the offered-ids window
- **Gated real capture:** `PAWL_SCREEN_TEST=1`, in memory only; it prints only sizes and timings. Release build,
  1920x1080 screen:
  - 8 sources listed (with thumbnails) in 0.38 s with GDI, 0.57 s with WGC
  - screen: grab 40-60 ms, encode 0.17-0.19 s (PNG, 363 KiB)
  - window: 340x566 grab 23-51 ms
  - Debug builds are about 10x slower to encode.
- **Gates:**
  - `cargo test`: **101 passed** (98 lib + 3), with `PAWL_SCREEN_TEST=1` and the voice models.
  - `cargo clippy --all-targets -- -D warnings`: clean.
  - `bunx tauri build --debug --no-bundle`: OK. The debug exe is 62.3 MB, up from 60.5 MB.
  - `scripts/check-no-models.sh`: ok.

## Linux and macOS (compiled only by CI; nothing run here)

- **Linux build** (xcap README): `pkg-config libclang-dev libxcb1-dev libxrandr-dev libdbus-1-dev
  libpipewire-0.3-dev libwayland-dev libegl-dev`. These are now in `release.yml`'s apt step and the README.
- **Linux runtime:** `libpipewire-0.3-0`, `libxcb1` and `libxcb-randr0` were added to the deb depends, and
  `pipewire-libs` / `libxcb` to the rpm depends. On Ubuntu 24.04, `libpipewire-0.3-0t64` provides the first. I
  haven't confirmed which libraries the binary really links without a Linux build.
- **Wayland:** xcap captures through the desktop portal (it may show a system dialog), and window lists may be
  empty. Untested.
- **macOS:** `permission.rs` is a two-function CoreGraphics FFI, like `voice/lock.rs`. Everything else goes through
  xcap. Not compiled here.

## Known risks

- **Possible crash instead of an error:** xcap's WGC code creates its D3D11 device in a `LazyLock` with
  `expect(...)`, using the hardware driver type only. With the release profile's `panic = "abort"`, a machine with
  no D3D11 device at all would close Pawl on the first capture instead of showing an error. Normal Windows 10/11 has
  at least the Basic Render Driver, so this is rare. **Resolved:** t13 chose `panic = "unwind"`, and capture code
  now runs under `catch_unwind` (see "Follow-up: unwind + Linux deps").
- **macOS 15+:** `NSWindowSharingNone` may not hide Pawl from every capture path. Unverified.
- **Picker speed:** each window thumbnail is a full capture (WGC about 50-130 ms each, serialized inside xcap).
  With many windows the picker takes 1-3 s (40 at most).

## Documentation used (screen)

- xcap 0.9.10: https://docs.rs/xcap/0.9.10/xcap/ and https://github.com/nashaofu/xcap (README: Linux
  requirements). I also read the source of `src/windows/{impl_window,capture,gdi,wgc}.rs` and
  `src/macos/{impl_window,capture}.rs`.
- image 0.25.10: https://docs.rs/image/0.25.10/image/ (`codecs::jpeg::JpegEncoder::new_with_quality`,
  `codecs::png::PngEncoder::new_with_quality`, `imageops::{resize, thumbnail}`).
- SetWindowDisplayAffinity / `WDA_EXCLUDEFROMCAPTURE`:
  https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity
- WGC border: https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscapturesession.isborderrequired
- Tauri content protection: https://docs.rs/tauri/2.12.7/tauri/webview/struct.WebviewWindow.html#method.set_content_protected
  (tao 0.37.3: `SetWindowDisplayAffinity` on Windows, `setSharingType` on macOS).
- macOS permission: https://developer.apple.com/documentation/coregraphics/cgrequestscreencaptureaccess() and
  https://developer.apple.com/documentation/coregraphics/cgpreflightscreencaptureaccess(); no entitlement needed:
  https://developer.apple.com/forums/thread/683860; Sequoia's monthly prompt:
  https://9to5mac.com/2024/08/14/macos-sequoia-screen-recording-prompt-monthly/

## Follow-up: unwind + Linux deps (t13's decisions)

1. **The release profile now unwinds.**
   - `apps/desktop/src-tauri/Cargo.toml` `[profile.release]` now has `panic = "unwind"` (was `"abort"`), with a
     comment explaining why.
   - New `src/panics.rs`: `guarded(what, user_message, f)` runs `f` under `catch_unwind`. On a panic it logs
     `[pawl] <what> failed: <panic message>` (only the message, shortened to 300 chars, never image data) and
     returns `Err(user_message)`.
   - **Screen:** every xcap call goes through it on the blocking worker thread: the listing (screens, windows,
     thumbnails), the capture, and the encoding. A panic becomes "Screen capture isn't available on this PC."
     Pawl's windows still get their capture exclusion removed: the guard sits inside it, so cleanup runs normally.
     The `busy` lock is never poisoned, because the panic is caught before it reaches the lock guard.
   - **Voice:** with unwinding, a panic on the voice worker would end only that thread. That would leave a stale
     status ("listening", mic shown as live). So the worker now runs under `catch_unwind` too (`spawn_worker`):
     - Unwinding drops the engine, which closes the mic and frees the models.
     - The status becomes `error` with "Voice stopped after an internal error. Restart Pawl to use it again.", and
       the mic is shown closed in the status and the tray.
     - A `state` and an `error` event go to both windows, and the panic message is logged.
   - **FFI audit:** `catch_unwind` does not cross FFI. Since Rust 1.81, a panic leaving an `extern "C"` function
     aborts the process (defined behaviour, not UB); we build with 1.95.
     - Our only Rust code that C calls is cpal's input data and error callbacks. On macOS CoreAudio calls them from
       C. They only downmix (iterators, no indexing) and `try_send`, so they cannot panic.
     - sherpa-onnx's `extern "C"` callbacks are only in its TTS module, which we don't use. Our sherpa paths are the
       ones already guarded against panics in round 1: NUL checks before its `CString::new(..).unwrap()`.
     - The screen-lock FFI (`voice/lock.rs`) and `screen/permission.rs` are plain calls without callbacks.
     - Main-thread handlers (tray, menus, the global shortcut) are called from tao's window procedure on Windows.
       tao catches panics there and re-raises them outside the FFI frames.
   - **Tests:**
     - `panics::tests`: a panic becomes the user message; `&str`, `String` and other payloads are read; long
       messages are shortened.
     - `screen::tests::a_panicking_capture_is_an_error`
     - `voice::tests::a_panicking_engine_reports_an_error_instead_of_a_stale_state`: a pretend mic that panics when
       opened takes the real worker down. The status turns to `error` with the message, the mic shows as closed,
       commands fail fast, and shutdown still joins.
2. **Linux build bits** (now mine for this):
   - **`release.yml`:** the apt step has xcap's packages (`pkg-config libclang-dev libxcb1-dev libxrandr-dev
     libdbus-1-dev libpipewire-0.3-dev libwayland-dev libegl-dev`).
   - **`tauri.conf.json`:** deb `depends` has `libpipewire-0.3-0`, `libxcb1`, `libxcb-randr0`; rpm has
     `pipewire-libs`, `libxcb` (CRLF kept).
   - **README:** the requirements line lists the same packages.
   - **`e2e.yml` is unchanged, on purpose.** It builds only `pawl-hub` (`cargo build -p pawl-hub`); the desktop
     crate is excluded from the root workspace, and the e2e tests drive the web UI in Playwright's Chromium. xcap is
     never compiled there, so the packages would only slow the job down. If e2e ever builds `src-tauri`, it needs
     the same apt line as `release.yml`.
3. **Exe size** (Windows x64, `cargo build --release` in src-tauri, same code, only the panic strategy changed):

   | Build | `pawl-desktop.exe` |
   |---|---|
   | `panic = "abort"` (forced with `--config 'profile.release.panic="abort"'`) | 27,283,456 B (26.0 MiB) |
   | `panic = "unwind"` (now) | 32,036,352 B (30.6 MiB) |
   | **Change** | **+4,752,896 B (+4.5 MiB, +17.4%)**: unwind tables and landing pads |

   - Each LTO build takes about 5.5 min.
   - A plain `cargo build` does not embed the web UI (no `custom-protocol`), so these totals are smaller than a
     `tauri build` exe. The change from the panic strategy is the same.
4. **Gates:**
   - `cargo test`: **105 passed** (102 lib + 3), with `PAWL_SCREEN_TEST=1` and the voice models.
   - `cargo clippy --all-targets -- -D warnings`: clean.
   - `bunx tauri build --debug --no-bundle`: OK.
   - Release builds with both panic strategies: OK.

## Screen review fixes (t13's review)

1. **(a) Hiding Pawl's windows now fails closed.**
   - `HideOwnWindows::new` returns a `Result`. The exclusion must be posted to the main thread, applied there to
     every Pawl window (each `set_content_protected(true)` must succeed), and confirmed within 2 s.
   - Otherwise the screen capture, or the listing with its screen thumbnails, is aborted with "Couldn't hide Pawl's
     windows; try again." and nothing is captured.
   - After a failure every window is restored. If the main thread runs the apply late, the restore sits behind it in
     the same event queue, so no window stays protected.
   - Window captures don't need the exclusion, since they show only that window.
   - Linux has no such switch and still captures with the pet possibly visible (as documented before).
   - Tests:
     - `nothing_is_listed_or_captured_when_pawl_cannot_be_hidden`: the list and capture closures are never called.
     - `the_exclusion_must_be_confirmed_in_time`: not posted, timeout, disconnected, or a window refusing all fail.
2. **(b) `Debug` never shows picture data.**
   - `Capture` has a hand-written `Debug` (media type, `data_len`, width, height), like `ChatImage`.
   - `Source` shows id, kind, app, `title_len` and `thumb_len`. The window title is left out too, because it can
     name a private document.
   - The internal `WindowInfo` no longer derives `Debug`. `Encoded` never had it.
   - Tests check that the base64 and the title don't appear.
3. **(c) Picker handshake + rate limits** (`src/screen/gate.rs`). The commands are now:

   | Command | Returns | Notes |
   |---|---|---|
   | `screen_begin()` | `string` (nonce, 32 hex chars) | Call when the picker opens. Valid 2 minutes, for the calling webview only; a new one replaces the old. Refused unless the calling window is visible. At most once a second and 20 times in 10 minutes. |
   | `screen_sources({ nonce })` | `ScreenSource[]` | As before. At most every 2 s, 5 times per picker session. |
   | `screen_capture({ nonce, id })` | `ScreenCapture` | `id` must be from this session's latest `screen_sources` list. At most every second, 3 times per session. |
   | `screen_end({ nonce })` | nothing | Call when the picker closes (sent or cancelled); the nonce stops working. |

   - Errors: "Open the screen picker again." (no, wrong, expired or ended nonce, or another webview), "Pick the
     screen or window again." (id not in this session's list), "Too many screen pictures in a short time. Wait a
     moment, then try again."
   - The nonce comes from std's randomly keyed SipHash (`RandomState`), over a counter and the time. It is compared
     without an early exit. It is not a secret from the page itself; it ties every listing and capture to an open
     picker, and the limits bound what a misbehaving page could do.
   - **The UI must update.** `apps/desktop/src/lib/screen.ts` (fe-settings) still calls `screen_sources()` and
     `screen_capture({ id })` without a nonce. Those calls now fail with "Open the screen picker again." until it
     calls `screen_begin` first.
   - Tests:
     - `gate.rs`: nothing without an open picker; wrong nonce or another webview; ids only from the session's own
       list; expiry, end and replacement; every rate limit, including the 10-minute window; exact comparison.
     - Flows: the nonce and its list are required; a panic inside a capture gives the user message and leaves the
       lock usable.
     - The gated real capture now runs through the same flows as the commands.
4. **Gates:**
   - `cargo test`: **113 passed** (110 lib + 3), with `PAWL_SCREEN_TEST=1` and the voice models.
   - `cargo clippy --all-targets -- -D warnings`: clean.
   - `bunx tauri build --debug --no-bundle`: OK.

## Bug: `cargo test --lib` crashed with STATUS_ACCESS_VIOLATION (t13's report)

1. **Root cause: cpal 0.18.2 (WASAPI host) keeps a COM object after COM may have shut down.**
   - `src/host/wasapi/device.rs` keeps one `IMMDeviceEnumerator` for the whole process (`static ENUMERATOR:
     OnceLock<Enumerator>`). It is created in a single-threaded apartment of whichever thread first touches audio.
   - `src/host/com.rs` keeps COM initialized per thread (a `thread_local!` guard) and calls `CoUninitialize` when
     that thread exits.
   - If no other thread has COM initialized at that moment, COM shuts down for the process and unloads
     MMDevApi.dll. The cached enumerator then points into an unloaded DLL. The next `list_mics` or `open` from any
     thread crashes the process in `IMMDeviceEnumerator::GetDefaultAudioEndpoint`.
   - Proven with a temporary vectored exception handler in the crashing test (removed afterwards):
     - The access violation is a read of `0x7fff04122ed0`, an address in no loaded module.
     - MMDevApi.dll was already unloaded (`GetModuleHandleW` gave null) when the test started.
     - Stack: `GetDefaultAudioEndpoint` ← `cpal::host::wasapi::device::current_default_endpoint` ←
       `default_input_device` ← `capture::list_mics` ← `Inner::mics` ← `Inner::status` ← the test.
   - The enumerator's class is registered with `ThreadingModel=both`, so the pointer stays usable while the DLL is
     loaded. Only the unload breaks it.
   - cpal's master has the same code. I found no upstream issue.
2. **Why `voice::tests` triggered it, and why it passed earlier.**
   - In the tests, `Inner::status()` lists the mics through real cpal on the test's own thread. The libtest
     thread ends with the test.
   - `a_panicking_engine_reports_an_error_instead_of_a_stale_state` (added with the unwind change) was the first
     test to do that. Its thread exit took COM down. The next test that listed mics (`mute_works_…`) crashed.
   - The panic had nothing to do with it. It only made this the first test, in name order, to call
     `status()`.
   - The crash only happens when no other thread has COM up at that moment, so parallel runs were a race:
     - With the unfixed binary and `PAWL_SCREEN_TEST=1` (the env of my earlier gates): 3 of 3 passed, because the
       real screen capture test holds COM at the same time.
     - Without it: 1 of 2 crashed.
3. **The real app.** It should not crash, but that was luck rather than design. The webview's main thread
   initializes COM (wry's `CoInitializeEx`, tao's `OleInitialize`) and never uninitializes it, so COM never goes
   to zero while Pawl runs. Still, the first mic listing usually runs on a tokio blocking-pool thread, which exits
   after it has been idle.
4. **Fix (`src/voice/capture.rs`).**
   - Every cpal use now goes through `host()`. It first calls `keep_com_loaded()`.
   - `keep_com_loaded()` runs once (`Once`). It takes an MTA usage with `CoIncrementMTAUsage` and never gives it
     back, so COM, and with it MMDevApi.dll, stays loaded until the process exits.
   - It runs on a short-lived thread of its own (`pawl-com`, MTA init, then uninit). The docs say the call moves a
     thread that has no apartment yet into the MTA, and the caller's thread must keep its own choice: tao, wry and
     the dialogs need single-threaded apartments.
   - If it fails, only `[pawl] could not keep COM loaded for audio (HRESULT …)` is logged, and audio works as
     before.
   - No other platform changes (no-op there).
   - No new dependency: three `ole32` imports.
   - Proposed upstream fix for cpal (not filed): either take an MTA usage, or create the enumerator on a COM thread
     that stays alive, before caching it in the static.
5. **Regression test.** `voice::capture::tests::audio_keeps_working_after_the_first_audio_thread_exits` (Windows
   only):
   - A thread lists the mics and exits.
   - The test asserts MMDevApi.dll is still loaded, then lists the mics from a second thread.
   - It then checks that a new thread can still enter a single-threaded apartment (`S_OK`).
   - With the fix disabled, the test fails every time when run alone: "COM unloaded the audio DLL under cpal's
     cached enumerator".
6. **Gates** (plain env, as t13 ran them):
   - `cargo test`: 5 runs in a row, each 114 passed (111 lib + 3).
   - `cargo test --lib voice:: -- --test-threads=1`: 59 passed.
   - `cargo test --lib -- --test-threads=1`: 111 passed.
   - The crashing pair (`a_panicking…` then `mute_works_when_the_settings…`): passes.
   - `cargo clippy --all-targets -- -D warnings`: clean.
7. **Docs used:**
   - https://learn.microsoft.com/windows/win32/api/combaseapi/nf-combaseapi-coincrementmtausage
   - https://learn.microsoft.com/windows/win32/api/combaseapi/nf-combaseapi-couninitialize
   - cpal 0.18.2 source (`src/host/com.rs`, `src/host/wasapi/device.rs`) and its master on GitHub.
