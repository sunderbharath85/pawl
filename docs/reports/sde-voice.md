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
