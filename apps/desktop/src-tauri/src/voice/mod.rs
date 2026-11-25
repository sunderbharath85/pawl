//! Voice (v0.2, docs/CONTRACTS.md "v0.2 Voice"): the pet's name as a wake word, and speech to text, on this
//! machine. Commands and events for the UI, the push-to-talk global shortcut, "Mute microphone" in the tray and
//! the pet's menu. The engine itself (models, microphone, audio pipeline) runs on one worker thread
//! (`engine.rs`); this module only talks to it through a channel.
//!
//! Privacy: no audio, transcript or recording is written to disk or sent anywhere; the only files are the
//! settings (`<app config dir>/voice.json`) and the models the user chose to download
//! (`<app data dir>/voice-models/`). Off by default; the mic is never opened while off or muted.

mod capture;
mod config;
mod download;
mod engine;
mod hotkey;
mod keyword;
mod lock;
mod models;
mod state;

pub use config::VoiceConfig;
pub use state::VoiceState;

use capture::MicInfo;
use download::{CancelFlag, DownloadError};
use engine::{Cmd, CpalMic, Engine, ListenAction, Msg, Sink, VoiceEvent};
use models::{DeleteSet, ModelSet, ModelStore, ModelsStatus};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// The event channel (both windows listen).
pub const EVENT: &str = "voice";
/// "Mute microphone" / "Unmute microphone" in the pet's right-click menu (the tray has its own id).
pub const MENU_MUTE: &str = "pet_voice_mute";

/// Messages queued for the worker: audio blocks (about 10 ms each) and commands. Full = the worker is behind and
/// new audio blocks are dropped.
const QUEUE: usize = 512;
/// How long a command waits for the worker (it may be finishing a model load).
const REPLY_TIMEOUT: Duration = Duration::from_secs(20);
const MICS_CACHE_FOR: Duration = Duration::from_secs(3);
const PROGRESS_EVERY: Duration = Duration::from_millis(100);
/// At exit, how long to wait for the engine to stop.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceStatus {
    pub state: VoiceState,
    pub config: VoiceConfig,
    pub error: Option<String>,
    pub models: ModelsStatus,
    pub mics: Vec<MicInfo>,
}

struct Core {
    config: VoiceConfig,
    state: VoiceState,
    error: Option<String>,
    mic_live: bool,
}

/// Problems that do not stop voice: the settings still apply (mute above all), and the status says what went wrong.
#[derive(Default)]
struct Warnings {
    /// The push-to-talk shortcut could not be registered (another app holds it).
    hotkey: Option<String>,
    /// `voice.json` could not be written (the change applies until Pawl quits).
    save: Option<String>,
}

struct DownloadJob {
    set: ModelSet,
    cancel: Arc<CancelFlag>,
}

struct Inner {
    tx: SyncSender<Msg>,
    core: Mutex<Core>,
    warnings: Mutex<Warnings>,
    /// Serializes config writes (UI, menus) and the shortcut registration that goes with them.
    config_lock: Mutex<()>,
    config_path: PathBuf,
    store: ModelStore,
    download: Mutex<Option<DownloadJob>>,
    /// The shortcut registered now (`None` also when registering failed).
    hotkey: Mutex<Option<Shortcut>>,
    /// Push-to-talk press/release, in order, to the thread that hands them to the engine (`None` = stop).
    hotkey_tx: Mutex<Option<mpsc::Sender<Option<ListenAction>>>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    mics: Mutex<Option<(Instant, Vec<MicInfo>)>>,
}

/// Managed state: the handle to the voice engine.
#[derive(Clone)]
pub struct Voice {
    inner: Arc<Inner>,
}

struct AppSink<R: Runtime> {
    app: AppHandle<R>,
    inner: Arc<Inner>,
}

impl<R: Runtime> Sink for AppSink<R> {
    fn emit(&self, event: VoiceEvent) {
        let _ = self.app.emit(EVENT, event);
    }

    fn publish(&self, state: VoiceState, error: Option<String>, mic_live: bool) {
        let (enabled, muted) = {
            let mut core = lock(&self.inner.core);
            core.state = state;
            core.error = error;
            core.mic_live = mic_live;
            (core.config.enabled, core.config.muted)
        };
        crate::tray::sync_voice(&self.app, enabled, muted, mic_live);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Registers global shortcuts (the global-shortcut plugin in the app; a fake in tests).
trait Registrar {
    fn register(&self, shortcut: Shortcut) -> Result<(), String>;
    fn unregister(&self, shortcut: Shortcut);
}

struct AppRegistrar<'a, R: Runtime>(&'a AppHandle<R>);

impl<R: Runtime> Registrar for AppRegistrar<'_, R> {
    fn register(&self, shortcut: Shortcut) -> Result<(), String> {
        self.0.global_shortcut().register(shortcut).map_err(|e| e.to_string())
    }
    fn unregister(&self, shortcut: Shortcut) {
        let _ = self.0.global_shortcut().unregister(shortcut);
    }
}

impl Inner {
    fn new(tx: SyncSender<Msg>, config: VoiceConfig, config_path: PathBuf, store: ModelStore) -> Self {
        Self {
            tx,
            core: Mutex::new(Core { config, state: VoiceState::Off, error: None, mic_live: false }),
            warnings: Mutex::new(Warnings::default()),
            config_lock: Mutex::new(()),
            config_path,
            store,
            download: Mutex::new(None),
            hotkey: Mutex::new(None),
            hotkey_tx: Mutex::new(None),
            worker: Mutex::new(None),
            mics: Mutex::new(None),
        }
    }

    fn send(&self, cmd: Cmd) -> Result<(), String> {
        self.tx.send(Msg::Cmd(cmd)).map_err(|_| "The voice engine stopped.".to_string())
    }

    /// Send a command that replies, and wait for the reply (blocking: call from a blocking thread).
    fn ask<T>(&self, make: impl FnOnce(Option<mpsc::Sender<T>>) -> Cmd) -> Result<T, String> {
        let (tx, rx) = mpsc::channel();
        self.send(make(Some(tx)))?;
        rx.recv_timeout(REPLY_TIMEOUT).map_err(|_| "The voice engine is busy; try again.".to_string())
    }

    fn mics(&self) -> Vec<MicInfo> {
        let mut cache = lock(&self.mics);
        if let Some((at, list)) = cache.as_ref() {
            if at.elapsed() < MICS_CACHE_FOR {
                return list.clone();
            }
        }
        let list = capture::list_mics();
        *cache = Some((Instant::now(), list.clone()));
        list
    }

    fn status(&self) -> VoiceStatus {
        let (state, config, error) = {
            let core = lock(&self.core);
            (core.state, core.config.clone(), core.error.clone())
        };
        let error = error.or_else(|| {
            let w = lock(&self.warnings);
            w.hotkey.clone().or_else(|| w.save.clone())
        });
        VoiceStatus { state, config, error, models: self.store.status(), mics: self.mics() }
    }

    fn config(&self) -> VoiceConfig {
        lock(&self.core).config.clone()
    }

    /// Register the push-to-talk shortcut the config wants (none while voice is off), replacing the old one.
    /// Callers are serialized (`config_lock`, or setup before the state exists). The `hotkey` lock is not held
    /// while the plugin registers: it waits for the main thread, and the main thread takes that lock in
    /// `on_hotkey`.
    fn sync_hotkey(&self, reg: &dyn Registrar, config: &VoiceConfig) -> Result<(), String> {
        let want = match (&config.hotkey, config.enabled) {
            (Some(h), true) => Some(hotkey::parse(h)?),
            _ => None,
        };
        let current = *lock(&self.hotkey);
        if current == want {
            return Ok(());
        }
        if let Some(old) = current {
            reg.unregister(old);
            *lock(&self.hotkey) = None;
        }
        if let Some(new) = want {
            reg.register(new).map_err(|err| {
                format!(
                    "Pawl can't use {} as the push-to-talk shortcut ({err}). Another app may be using it.",
                    config.hotkey.as_deref().unwrap_or_default()
                )
            })?;
            *lock(&self.hotkey) = Some(new);
        }
        Ok(())
    }

    /// Hand the config to the engine. A reply that is slow to come (a model load in progress) is not an error:
    /// the command is queued and applies in order.
    fn configure_engine(&self, config: VoiceConfig) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Configure(config, Some(tx)))?;
        let _ = rx.recv_timeout(REPLY_TIMEOUT);
        Ok(())
    }

    /// Apply a config: validate it, give it to the engine FIRST (so mute and off always take effect), then save
    /// it, then register the shortcut if it or the master switch changed. Saving and the shortcut can fail without
    /// undoing anything: those failures are warnings (in the status and as `error` events), returned here when
    /// new. Only an invalid config (nothing applied) or a stopped engine is an `Err`.
    fn apply_config(&self, reg: &dyn Registrar, config: VoiceConfig) -> Result<Vec<String>, String> {
        let config = config.validated()?;
        let _guard = lock(&self.config_lock);
        let old = self.config();
        lock(&self.core).config = config.clone();
        self.configure_engine(config.clone())?;

        let save = config::save(&self.config_path, &config)
            .err()
            .map(|e| format!("Could not save the voice settings: {e}"));
        let hotkey = (old.hotkey != config.hotkey || old.enabled != config.enabled)
            .then(|| self.sync_hotkey(reg, &config).err());
        let mut news = Vec::new();
        let mut w = lock(&self.warnings);
        if save.is_some() && save != w.save {
            news.extend(save.clone());
        }
        w.save = save;
        if let Some(hk) = hotkey {
            if hk.is_some() && hk != w.hotkey {
                news.extend(hk.clone());
            }
            w.hotkey = hk;
        }
        Ok(news)
    }

    /// Stop the worker (it closes the mic and frees the models) and the push-to-talk thread, and release the
    /// shortcut. Waits at most `SHUTDOWN_WAIT`.
    fn shutdown(&self, reg: &dyn Registrar) {
        if let Some(old) = lock(&self.hotkey).take() {
            reg.unregister(old);
        }
        if let Some(job) = lock(&self.download).as_ref() {
            job.cancel.cancel();
        }
        if let Some(tx) = lock(&self.hotkey_tx).take() {
            let _ = tx.send(None);
        }
        let deadline = Instant::now() + SHUTDOWN_WAIT;
        // Never block on a full queue: the worker drains it, or the deadline passes.
        loop {
            match self.tx.try_send(Msg::Cmd(Cmd::Shutdown)) {
                Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => break,
                Err(mpsc::TrySendError::Full(_)) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(mpsc::TrySendError::Full(_)) => break,
            }
        }
        if let Some(h) = lock(&self.worker).take() {
            while !h.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if h.is_finished() {
                let _ = h.join();
            } else {
                eprintln!("[pawl] voice: the engine did not stop in time");
            }
        }
    }
}

fn emit_warnings<R: Runtime>(app: &AppHandle<R>, warnings: Vec<String>) {
    for message in warnings {
        eprintln!("[pawl] voice: {message}");
        let _ = app.emit(EVENT, VoiceEvent::Error { message });
    }
}

/// Start the voice engine (from the app's setup). Voice stays off unless the saved settings turn it on.
pub fn init<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let config_dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let config_path = config_dir.join(config::CONFIG_FILE);
    let config = config::load(&config_path);
    let store = ModelStore::new(data_dir.join(models::MODELS_DIR));
    store.remove_leftovers();

    let (tx, rx) = mpsc::sync_channel(QUEUE);
    let inner = Arc::new(Inner::new(tx.clone(), config.clone(), config_path, store.clone()));
    let sink = AppSink { app: app.clone(), inner: inner.clone() };
    let engine_config = config.clone();
    let worker = std::thread::Builder::new()
        .name("pawl-voice".into())
        .spawn(move || Engine::new(store, engine_config, Box::new(sink), Box::new(CpalMic), tx, Some(engine::OS_LOCK_PROBE)).run(rx))
        .map_err(|e| e.to_string())?;
    *lock(&inner.worker) = Some(worker);

    let hk_app = app.clone();
    start_hotkey_thread(&inner, move |message| {
        let _ = hk_app.emit(EVENT, VoiceEvent::Error { message });
    })
    .map_err(|e| e.to_string())?;

    if let Err(err) = inner.sync_hotkey(&AppRegistrar(app), &config) {
        lock(&inner.warnings).hotkey = Some(err.clone());
        emit_warnings(app, vec![err]);
    }
    app.manage(Voice { inner });
    Ok(())
}

/// Push-to-talk: one thread hands presses and releases to the engine in the order they happened (a press and its
/// release are never reordered), waiting for each answer; refusals go to `report`. It ends on `None`.
fn start_hotkey_thread(inner: &Arc<Inner>, report: impl Fn(String) + Send + 'static) -> std::io::Result<()> {
    let (tx, rx) = mpsc::channel::<Option<ListenAction>>();
    *lock(&inner.hotkey_tx) = Some(tx);
    let inner = inner.clone();
    std::thread::Builder::new().name("pawl-voice-hotkey".into()).spawn(move || {
        while let Ok(Some(action)) = rx.recv() {
            match inner.ask(|r| Cmd::Listen(action, r)) {
                Ok(Ok(())) => {}
                Ok(Err(message)) | Err(message) => report(message),
            }
        }
    })?;
    Ok(())
}

/// App exit: stop the engine (closing the mic and freeing the models) and release the shortcut.
pub fn shutdown<R: Runtime>(app: &AppHandle<R>) {
    if let Some(voice) = app.try_state::<Voice>() {
        voice.inner.shutdown(&AppRegistrar(app));
    }
}

/// The push-to-talk shortcut went down or up (the global-shortcut plugin's handler, on the main thread).
pub fn on_hotkey<R: Runtime>(app: &AppHandle<R>, shortcut: &Shortcut, state: ShortcutState) {
    let Some(voice) = app.try_state::<Voice>() else { return };
    if lock(&voice.inner.hotkey).as_ref() != Some(shortcut) {
        return;
    }
    let action = match state {
        ShortcutState::Pressed => ListenAction::Start,
        ShortcutState::Released => ListenAction::Stop,
    };
    let tx = lock(&voice.inner.hotkey_tx).clone();
    if let Some(tx) = tx {
        let _ = tx.send(Some(action));
    }
}

/// "Mute microphone" / "Unmute microphone" from the tray or the pet's menu: flip `config.muted`. It never
/// depends on the shortcut (untouched when only `muted` changes) or on saving the file.
pub fn toggle_mute<R: Runtime>(app: &AppHandle<R>) {
    let Some(voice) = app.try_state::<Voice>() else { return };
    let inner = voice.inner.clone();
    let app = app.clone();
    // Off the main thread (menu handlers run there; the engine may be busy for a moment).
    std::thread::spawn(move || {
        let mut config = inner.config();
        config.muted = !config.muted;
        match inner.apply_config(&AppRegistrar(&app), config) {
            Ok(warnings) => {
                emit_warnings(&app, warnings);
                // The usual state event, also when the state itself did not change (voice off).
                let state = lock(&inner.core).state;
                let _ = app.emit(EVENT, VoiceEvent::State { state });
            }
            Err(message) => {
                let _ = app.emit(EVENT, VoiceEvent::Error { message });
            }
        }
    });
}

/// The menu label for the mute item.
pub fn mute_label(muted: bool) -> &'static str {
    if muted {
        "Unmute microphone"
    } else {
        "Mute microphone"
    }
}

/// Whether voice is on and muted, for menus.
pub fn menu_state<R: Runtime>(app: &AppHandle<R>) -> Option<(bool, bool)> {
    let voice = app.try_state::<Voice>()?;
    let core = lock(&voice.inner.core);
    Some((core.config.enabled, core.config.muted))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

// ---------- commands ----------

#[tauri::command]
pub async fn voice_status(voice: tauri::State<'_, Voice>) -> Result<VoiceStatus, String> {
    let inner = voice.inner.clone();
    blocking(move || Ok(inner.status())).await
}

#[tauri::command]
pub async fn voice_configure<R: Runtime>(
    app: AppHandle<R>,
    voice: tauri::State<'_, Voice>,
    config: VoiceConfig,
) -> Result<VoiceStatus, String> {
    let inner = voice.inner.clone();
    blocking(move || {
        let warnings = inner.apply_config(&AppRegistrar(&app), config)?;
        emit_warnings(&app, warnings);
        Ok(inner.status())
    })
    .await
}

/// Clears the running download when dropped (also if the command's future goes away).
struct JobGuard(Arc<Inner>);

impl Drop for JobGuard {
    fn drop(&mut self) {
        *lock(&self.0.download) = None;
        let _ = self.0.send(Cmd::Downloading(false));
        let _ = self.0.send(Cmd::ModelsChanged(None));
    }
}

/// Download the models of `models` that are missing (HTTPS, pinned SHA-256, then unpack). Resolves when done or
/// cancelled; fails with the reason (also sent as an `error` event). Progress comes as `download` events.
#[tauri::command]
pub async fn voice_download<R: Runtime>(
    app: AppHandle<R>,
    voice: tauri::State<'_, Voice>,
    models: ModelSet,
) -> Result<(), String> {
    let inner = voice.inner.clone();
    let todo: Vec<&'static models::ModelSpec> = models
        .download_ids()
        .iter()
        .filter(|id| !inner.store.installed(**id))
        .map(|id| models::spec(*id))
        .collect();
    if todo.is_empty() {
        let _ = inner.send(Cmd::ModelsChanged(None));
        return Ok(());
    }
    let cancel = Arc::new(CancelFlag::default());
    {
        let mut job = lock(&inner.download);
        if let Some(j) = job.as_ref() {
            return if j.set == models { Ok(()) } else { Err("Another voice download is running.".into()) };
        }
        *job = Some(DownloadJob { set: models, cancel: cancel.clone() });
    }
    let _guard = JobGuard(inner.clone());
    inner.send(Cmd::Downloading(true))?;

    let total: u64 = todo.iter().map(|s| s.download_size).sum();
    let emit = |received: u64| {
        let _ = app.emit(EVENT, VoiceEvent::Download { models, received, total });
    };
    emit(0);
    let client = download::http_client()?;
    let mut received: u64 = 0;
    let mut last = Instant::now();
    let mut result = Ok(());
    for spec in todo {
        let before = received;
        let r = download::download_model(&client, &inner.store, spec, &cancel, |n| {
            received += n;
            if last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                emit(received);
            }
        })
        .await;
        if let Err(e) = r {
            result = Err(e);
            break;
        }
        received = before + spec.download_size;
        emit(received);
    }
    match result {
        Ok(()) | Err(DownloadError::Cancelled) => Ok(()),
        Err(DownloadError::Failed(message)) => {
            let _ = app.emit(EVENT, VoiceEvent::Error { message: message.clone() });
            Err(message)
        }
    }
}

/// Stop a running download; its partial files are removed.
#[tauri::command]
pub fn voice_cancel_download(voice: tauri::State<'_, Voice>) {
    if let Some(job) = lock(&voice.inner.download).as_ref() {
        job.cancel.cancel();
    }
}

#[tauri::command]
pub async fn voice_delete_models(voice: tauri::State<'_, Voice>, models: DeleteSet) -> Result<VoiceStatus, String> {
    let inner = voice.inner.clone();
    blocking(move || {
        if lock(&inner.download).is_some() {
            return Err("Wait for the download to finish, or cancel it.".into());
        }
        // Let go of the models first (an open model file can't be deleted on Windows).
        inner.ask(Cmd::ReleaseModels)?;
        let res = inner.store.delete(models.ids());
        inner.ask(Cmd::ModelsChanged)?;
        res.map_err(|e| format!("Could not delete the voice models: {e}"))?;
        Ok(inner.status())
    })
    .await
}

/// Push-to-talk from the pet, the bubble's mic button or the hotkey: "start", then "stop" (transcribe now) or
/// "cancel" (drop it). An utterance also ends by itself on silence or after 15 s.
#[tauri::command]
pub async fn voice_listen(voice: tauri::State<'_, Voice>, action: ListenAction) -> Result<(), String> {
    let inner = voice.inner.clone();
    blocking(move || inner.ask(|r| Cmd::Listen(action, r))?).await
}

/// The Settings level meter: `level` events while on. Does not change the state; needs voice on and unmuted.
#[tauri::command]
pub async fn voice_mic_test(voice: tauri::State<'_, Voice>, on: bool) -> Result<(), String> {
    let inner = voice.inner.clone();
    blocking(move || inner.ask(|r| Cmd::MicTest(on, r))?).await
}

#[cfg(test)]
mod tests {
    use super::engine::testing::{FakeMic, Recorder};
    use super::*;

    /// Every registration fails, as when another app holds the shortcut.
    #[derive(Default)]
    struct Taken {
        unregistered: Mutex<Vec<Shortcut>>,
    }

    impl Registrar for Taken {
        fn register(&self, _: Shortcut) -> Result<(), String> {
            Err("HotKey already registered".into())
        }
        fn unregister(&self, s: Shortcut) {
            lock(&self.unregistered).push(s);
        }
    }

    #[derive(Default)]
    struct Free {
        registered: Mutex<Vec<Shortcut>>,
        unregistered: Mutex<Vec<Shortcut>>,
    }

    impl Registrar for Free {
        fn register(&self, s: Shortcut) -> Result<(), String> {
            lock(&self.registered).push(s);
            Ok(())
        }
        fn unregister(&self, s: Shortcut) {
            lock(&self.unregistered).push(s);
        }
    }

    struct Running {
        inner: Arc<Inner>,
        mic: FakeMic,
        dir: PathBuf,
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.inner.shutdown(&Free::default());
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The glue with a real engine thread, a pretend mic and no models.
    fn running(tag: &str, config_path: Option<PathBuf>) -> Running {
        let dir = std::env::temp_dir().join(format!("pawl-voice-glue-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = ModelStore::new(dir.join("models"));
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let path = config_path.unwrap_or_else(|| dir.join(config::CONFIG_FILE));
        let inner = Arc::new(Inner::new(tx.clone(), VoiceConfig::default(), path, store.clone()));
        let mic = FakeMic::default();
        let (sink, m) = (Recorder::default(), mic.clone());
        let worker = std::thread::spawn(move || {
            Engine::new(store, VoiceConfig::default(), Box::new(sink), Box::new(m), tx, None).run(rx)
        });
        *lock(&inner.worker) = Some(worker);
        Running { inner, mic, dir }
    }

    fn on_with_hotkey() -> VoiceConfig {
        VoiceConfig {
            enabled: true,
            keyword: "Biscuit".into(),
            hotkey: Some("Ctrl+Alt+Space".into()),
            ..Default::default()
        }
    }

    fn mic_test(inner: &Inner, on: bool) -> Result<(), String> {
        inner.ask(|r| Cmd::MicTest(on, r)).unwrap()
    }

    #[test]
    fn mute_works_when_the_shortcut_cannot_be_registered() {
        let r = running("taken", None);
        let reg = Taken::default();
        let on = on_with_hotkey();

        // The shortcut is taken: a warning, but voice still turns on and the settings are saved.
        let warnings = r.inner.apply_config(&reg, on.clone()).unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("push-to-talk"), "{warnings:?}");
        assert!(r.inner.status().error.unwrap().contains("push-to-talk"));
        assert_eq!(config::load(&r.inner.config_path), on);
        assert!(lock(&r.inner.hotkey).is_none());

        mic_test(&r.inner, true).unwrap();
        assert!(r.mic.is_open());

        // Mute: applied at once, the shortcut is not touched (no new warning), and saved.
        let warnings = r.inner.apply_config(&reg, VoiceConfig { muted: true, ..on.clone() }).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!r.mic.is_open(), "muting closes the mic");
        assert!(config::load(&r.inner.config_path).muted);
        assert!(mic_test(&r.inner, true).is_err(), "no mic while muted");
        assert!(!r.mic.is_open());

        // Unmute and pick another taken shortcut in one change: the change applies, the warning is new.
        let next = VoiceConfig { hotkey: Some("Ctrl+Alt+K".into()), ..on.clone() };
        let warnings = r.inner.apply_config(&reg, next.clone()).unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(!r.inner.config().muted);
        mic_test(&r.inner, true).unwrap();
        assert!(r.mic.is_open());

        // Turning voice off closes the mic and clears the shortcut warning (nothing to register).
        let warnings = r.inner.apply_config(&reg, VoiceConfig { enabled: false, ..next }).unwrap();
        assert!(warnings.is_empty());
        assert!(!r.mic.is_open());
        assert_eq!(r.inner.status().error, None);
    }

    #[test]
    fn mute_works_when_the_settings_cannot_be_saved() {
        let dir = std::env::temp_dir().join(format!("pawl-voice-glue-blocker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let blocker = dir.join("not-a-folder");
        std::fs::write(&blocker, b"x").unwrap();
        let r = running("nosave", Some(blocker.join(config::CONFIG_FILE)));
        let reg = Free::default();
        let on = VoiceConfig { hotkey: None, ..on_with_hotkey() };
        let warnings = r.inner.apply_config(&reg, on.clone()).unwrap();
        assert!(warnings[0].contains("Could not save"), "{warnings:?}");
        mic_test(&r.inner, true).unwrap();
        assert!(r.mic.is_open());
        let warnings = r.inner.apply_config(&reg, VoiceConfig { muted: true, ..on }).unwrap();
        assert!(warnings.is_empty(), "the same warning is not repeated: {warnings:?}");
        assert!(!r.mic.is_open(), "muted although nothing could be saved");
        assert!(r.inner.status().error.unwrap().contains("Could not save"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_config_changes_nothing() {
        let r = running("invalid", None);
        let reg = Free::default();
        r.inner.apply_config(&reg, on_with_hotkey()).unwrap();
        let bad = VoiceConfig { sensitivity: 3.0, muted: true, ..on_with_hotkey() };
        assert!(r.inner.apply_config(&reg, bad).is_err());
        assert!(!r.inner.config().muted);
        assert!(!config::load(&r.inner.config_path).muted);
    }

    #[test]
    fn shortcut_follows_the_hotkey_and_the_master_switch() {
        let r = running("free", None);
        let reg = Free::default();
        let on = on_with_hotkey();
        r.inner.apply_config(&reg, on.clone()).unwrap();
        let space = hotkey::parse("Ctrl+Alt+Space").unwrap();
        assert_eq!(*lock(&reg.registered), vec![space]);
        // Unrelated changes never touch it.
        r.inner.apply_config(&reg, VoiceConfig { muted: true, sensitivity: 0.8, ..on.clone() }).unwrap();
        assert_eq!(lock(&reg.registered).len(), 1);
        assert!(lock(&reg.unregistered).is_empty());
        // Voice off releases it; shutdown releases what is registered.
        r.inner.apply_config(&reg, VoiceConfig { enabled: false, ..on.clone() }).unwrap();
        assert_eq!(*lock(&reg.unregistered), vec![space]);
        assert!(lock(&r.inner.hotkey).is_none());
        r.inner.apply_config(&reg, on).unwrap();
        let end = Free::default();
        r.inner.shutdown(&end);
        assert_eq!(*lock(&end.unregistered), vec![space]);
    }

    #[test]
    fn shutdown_stops_the_engine_and_closes_the_mic() {
        let r = running("shutdown", None);
        r.inner.apply_config(&Free::default(), VoiceConfig { hotkey: None, ..on_with_hotkey() }).unwrap();
        mic_test(&r.inner, true).unwrap();
        assert!(r.mic.is_open());
        r.inner.shutdown(&Free::default());
        assert!(lock(&r.inner.worker).is_none(), "joined");
        assert!(!r.mic.is_open(), "the engine closed the mic on its way out");
        assert!(r.inner.send(Cmd::Downloading(false)).is_err(), "the worker is gone");
    }

    #[test]
    fn hotkey_thread_forwards_in_order_and_reports_refusals() {
        let r = running("hk", None);
        let (seen_tx, seen_rx) = mpsc::channel();
        start_hotkey_thread(&r.inner, move |m| {
            let _ = seen_tx.send(m);
        })
        .unwrap();
        let tx = lock(&r.inner.hotkey_tx).clone().unwrap();
        // Voice is off: a press is refused (and reported); the release after it is fine (nothing to stop).
        tx.send(Some(ListenAction::Start)).unwrap();
        tx.send(Some(ListenAction::Stop)).unwrap();
        let msg = seen_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(msg.contains("Voice is off"), "{msg}");
        assert!(seen_rx.recv_timeout(Duration::from_millis(300)).is_err(), "only the press was refused");
        tx.send(None).unwrap();
        assert_eq!(r.mic.opens(), 0);
    }
}
