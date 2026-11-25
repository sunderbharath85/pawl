//! The voice worker: one thread that owns the models, the microphone stream and the audio pipeline.
//!
//! microphone (cpal callback: mono f32) -> 16 kHz (sherpa LinearResampler) -> 2 s ring buffer
//!   -> keyword spotter while the wake word is on -> on the pet's name (or push-to-talk): an utterance with the
//!      audio from just before -> Silero VAD ends it on silence (or after 15 s; "stop" ends it at once)
//!   -> offline speech model -> `transcript` event.
//!
//! Audio only ever lives in memory (the ring buffer and the current utterance) and is dropped when used. Models are
//! loaded when voice is on and the files are there, and unloaded when it is turned off. The mic is open only while
//! `Inputs::mic_wanted()` says so.

use super::capture::{self, CaptureMsg};
use super::config::{AsrModel, VoiceConfig};
use super::keyword::{self, KeywordEncoder};
use super::lock;
use super::models::{ModelId, ModelSet, ModelStore};
use super::state::{Inputs, Phase, VoiceState};
use serde::{Deserialize, Serialize};
use sherpa_onnx::{
    KeywordSpotter, KeywordSpotterConfig, LinearResampler, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineTransducerModelConfig, OnlineStream, VadModelConfig, VoiceActivityDetector,
};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender};
use std::time::{Duration, Instant};

pub const SAMPLE_RATE: u32 = 16_000;
const SR: usize = SAMPLE_RATE as usize;
/// The ring buffer of recent audio (for the audio from just before a wake word or push-to-talk).
const RING_SAMPLES: usize = 2 * SR;
/// After the wake word: at most this much audio from before the hit (normally from the end of the name).
const WAKE_PRE_ROLL: usize = SR;
/// Push-to-talk: a little audio from before the press (people start talking as they press).
const PUSH_PRE_ROLL: usize = SR * 3 / 10;
/// The wake word's last token time marks its start; the token itself lasts about this long.
const KEYWORD_TAIL: usize = SR * 12 / 100;
const MAX_UTTERANCE: usize = 15 * SR;
/// An utterance in which nobody spoke ends after this long (an empty transcript: "I didn't catch that").
const NO_SPEECH: usize = 6 * SR;
const VAD_WINDOW: usize = 512;
/// Speech kept around what the VAD found, when choosing what to transcribe.
const SPEECH_PAD_BEFORE: usize = SR / 4;
const SPEECH_PAD_AFTER: usize = SR * 3 / 10;
const LEVEL_EVERY: Duration = Duration::from_millis(50);
const MIC_RETRY: Duration = Duration::from_secs(5);
/// A mic test the UI forgot to stop ends by itself.
const MIC_TEST_MAX: Duration = Duration::from_secs(120);
const TICK: Duration = Duration::from_millis(250);
const MAX_TRANSCRIPT_CHARS: usize = 2000;

/// Events on the `voice` channel (docs/CONTRACTS.md "v0.2 Voice").
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum VoiceEvent {
    State { state: VoiceState },
    Wake { keyword: String, score: f32 },
    /// The microphone level, 0..1 (RMS in dBFS mapped from -60..0).
    Level { rms: f32 },
    Transcript { id: String, text: String },
    Download { models: ModelSet, received: u64, total: u64 },
    Error { message: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListenAction {
    Start,
    Stop,
    Cancel,
}

pub type Reply<T> = Option<Sender<T>>;

/// `Some(true)` while the screen is locked; `None` when the OS does not say.
pub type LockProbe = fn() -> Option<bool>;

/// The OS's answer (the app's probe).
pub const OS_LOCK_PROBE: LockProbe = lock::screen_locked;

pub enum Cmd {
    Configure(VoiceConfig, Reply<()>),
    /// Models were downloaded or deleted.
    ModelsChanged(Reply<()>),
    /// Unload every model (before deleting files); `ModelsChanged` afterwards loads again what is still there.
    ReleaseModels(Reply<()>),
    Downloading(bool),
    Listen(ListenAction, Reply<Result<(), String>>),
    MicTest(bool, Reply<Result<(), String>>),
    /// App exit: close the mic, free the models, end the thread.
    Shutdown,
}

pub enum Msg {
    Capture(CaptureMsg),
    Cmd(Cmd),
}

/// Where the engine reports to (the app: status for `voice_status`, events to both windows, the tray).
pub trait Sink: Send + 'static {
    fn emit(&self, event: VoiceEvent);
    /// The current state and error, and whether the mic is open (after every change).
    fn publish(&self, state: VoiceState, error: Option<String>, mic_live: bool);
}

/// An open microphone: its rate, and a handle that closes it when dropped.
pub struct OpenMic {
    pub sample_rate: u32,
    pub _handle: Box<dyn std::any::Any>,
}

pub trait MicOpener: Send + 'static {
    fn open(&mut self, mic_id: Option<&str>, generation: u64, tx: SyncSender<Msg>) -> Result<OpenMic, String>;
}

/// The real microphone (cpal).
pub struct CpalMic;

impl MicOpener for CpalMic {
    fn open(&mut self, mic_id: Option<&str>, generation: u64, tx: SyncSender<Msg>) -> Result<OpenMic, String> {
        let cap = capture::open(mic_id, generation, tx)?;
        Ok(OpenMic { sample_rate: cap.sample_rate, _handle: Box::new(cap) })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trigger {
    Wake,
    Push,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    Silence,
    NoSpeech,
    Cap,
    Stop,
}

/// What the user is saying right now: its samples (16 kHz) and the speech the VAD found in them.
struct Utterance {
    trigger: Trigger,
    samples: Vec<f32>,
    /// Samples not yet given to the VAD (it takes whole windows).
    pending: Vec<f32>,
    /// Speech segments: (start, len) in `samples`.
    spans: Vec<(usize, usize)>,
    speech_seen: bool,
}

impl Utterance {
    fn new(trigger: Trigger) -> Self {
        Self { trigger, samples: Vec::with_capacity(4 * SR), pending: Vec::new(), spans: Vec::new(), speech_seen: false }
    }

    fn push(&mut self, pcm: &[f32], vad: &VoiceActivityDetector) {
        let room = (MAX_UTTERANCE + SR).saturating_sub(self.samples.len());
        let pcm = &pcm[..pcm.len().min(room)];
        self.samples.extend_from_slice(pcm);
        self.pending.extend_from_slice(pcm);
        while self.pending.len() >= VAD_WINDOW {
            vad.accept_waveform(&self.pending[..VAD_WINDOW]);
            self.pending.drain(..VAD_WINDOW);
            self.collect(vad);
        }
    }

    fn collect(&mut self, vad: &VoiceActivityDetector) {
        if vad.detected() {
            self.speech_seen = true;
        }
        while let Some(seg) = vad.front() {
            let start = usize::try_from(seg.start()).unwrap_or(0);
            let len = usize::try_from(seg.n()).unwrap_or(0);
            if len > 0 {
                self.spans.push((start, len));
                self.speech_seen = true;
            }
            vad.pop();
        }
    }

    fn end_reason(&self, vad: &VoiceActivityDetector) -> Option<End> {
        if self.samples.len() >= MAX_UTTERANCE {
            Some(End::Cap)
        } else if self.speech_seen && !self.spans.is_empty() && !vad.detected() {
            Some(End::Silence)
        } else if !self.speech_seen && self.samples.len() >= NO_SPEECH {
            Some(End::NoSpeech)
        } else {
            None
        }
    }

    /// Give the VAD the rest and take every segment it still holds.
    fn finish(&mut self, vad: &VoiceActivityDetector) {
        if !self.pending.is_empty() {
            let mut last = std::mem::take(&mut self.pending);
            last.resize(VAD_WINDOW, 0.0);
            vad.accept_waveform(&last);
        }
        vad.flush();
        self.collect(vad);
    }
}

/// The part of an utterance worth transcribing: from a little before the first speech to a little after the last.
fn speech_range(spans: &[(usize, usize)], len: usize) -> Option<(usize, usize)> {
    let first = spans.iter().map(|s| s.0).min()?;
    let last = spans.iter().map(|s| s.0.saturating_add(s.1)).max()?;
    let start = first.saturating_sub(SPEECH_PAD_BEFORE).min(len);
    let end = last.saturating_add(SPEECH_PAD_AFTER).min(len);
    (end > start).then_some((start, end))
}

/// Microphone level 0..1 from the RMS of a block, on a -60..0 dBFS scale.
pub fn level(pcm: &[f32]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let mean_sq = pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32;
    let rms = mean_sq.sqrt();
    if rms <= 1e-6 {
        return 0.0;
    }
    ((20.0 * rms.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Where the utterance after a wake word starts, as an absolute sample index: just after the name (from the
/// spotter's timing), but at most `WAKE_PRE_ROLL` back; a timing that makes no sense falls back to a short
/// look-back.
fn wake_start(now: u64, stream_origin: u64, start_time: f32, last_token: Option<f32>) -> u64 {
    let earliest = now.saturating_sub(WAKE_PRE_ROLL as u64);
    let fallback = now.saturating_sub(PUSH_PRE_ROLL as u64);
    let Some(t) = last_token else {
        return fallback;
    };
    let secs = f64::from(start_time) + f64::from(t);
    if !secs.is_finite() || secs < 0.0 {
        return fallback;
    }
    // `as` saturates for huge or negative floats; the sums saturate too (no overflow on odd timings).
    let end = stream_origin.saturating_add((secs * f64::from(SAMPLE_RATE)) as u64).saturating_add(KEYWORD_TAIL as u64);
    if end > now || end.saturating_add(RING_SAMPLES as u64 * 2) < now {
        return fallback;
    }
    end.max(earliest)
}

/// The transcript as a chat message: trimmed, one line, without a leading "hey <name>" after a wake word, at most
/// `MAX_TRANSCRIPT_CHARS`, and empty when only punctuation (or a non-speech tag like "[BLANK_AUDIO]") is left.
fn clean_transcript(raw: &str, after_wake: bool, spoken: &str) -> String {
    let mut text: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.starts_with('[') && text.ends_with(']') || text.starts_with('(') && text.ends_with(')') {
        text.clear();
    }
    if after_wake {
        text = keyword::strip_wake_prefix(&text, spoken);
    }
    if !text.chars().any(char::is_alphanumeric) {
        return String::new();
    }
    if text.chars().count() > MAX_TRANSCRIPT_CHARS {
        text = text.chars().take(MAX_TRANSCRIPT_CHARS).collect();
    }
    text
}

/// A model path for sherpa's C config (which panics on an interior NUL, so refuse one here).
fn path_str(p: &Path) -> Result<String, String> {
    p.to_str()
        .filter(|s| !s.contains('\0'))
        .map(str::to_string)
        .ok_or_else(|| format!("model path is not valid text: {}", p.display()))
}

struct Loaded {
    asr_model: AsrModel,
    vad: VoiceActivityDetector,
    asr: OfflineRecognizer,
    /// The wake word model's tokenizer (`None` when it could not be read: no wake word then).
    encoder: Result<KeywordEncoder, String>,
}

fn create_vad(store: &ModelStore) -> Result<VoiceActivityDetector, String> {
    let mut c = VadModelConfig::default();
    c.silero_vad.model = Some(path_str(&store.file(ModelId::Vad, "silero_vad.onnx"))?);
    c.silero_vad.threshold = 0.5;
    c.silero_vad.min_silence_duration = 0.8;
    c.silero_vad.min_speech_duration = 0.25;
    c.silero_vad.window_size = VAD_WINDOW as i32;
    c.silero_vad.max_speech_duration = 20.0;
    c.sample_rate = SAMPLE_RATE as i32;
    c.num_threads = 1;
    c.provider = Some("cpu".into());
    VoiceActivityDetector::create(&c, 30.0).ok_or_else(|| "Could not load the voice activity model.".to_string())
}

fn create_asr(store: &ModelStore, model: AsrModel) -> Result<OfflineRecognizer, String> {
    let mut c = OfflineRecognizerConfig::default();
    match model {
        AsrModel::Fast => {
            c.model_config.moonshine.encoder = Some(path_str(&store.file(ModelId::Moonshine, "encoder_model.ort"))?);
            c.model_config.moonshine.merged_decoder =
                Some(path_str(&store.file(ModelId::Moonshine, "decoder_model_merged.ort"))?);
            c.model_config.tokens = Some(path_str(&store.file(ModelId::Moonshine, "tokens.txt"))?);
        }
        AsrModel::Accurate => {
            c.model_config.transducer = OfflineTransducerModelConfig {
                encoder: Some(path_str(&store.file(ModelId::Parakeet, "encoder.int8.onnx"))?),
                decoder: Some(path_str(&store.file(ModelId::Parakeet, "decoder.int8.onnx"))?),
                joiner: Some(path_str(&store.file(ModelId::Parakeet, "joiner.int8.onnx"))?),
            };
            c.model_config.model_type = Some("nemo_transducer".into());
            c.model_config.tokens = Some(path_str(&store.file(ModelId::Parakeet, "tokens.txt"))?);
        }
    }
    c.model_config.num_threads = 1;
    c.model_config.provider = Some("cpu".into());
    c.decoding_method = Some("greedy_search".into());
    OfflineRecognizer::create(&c).ok_or_else(|| "Could not load the speech model.".to_string())
}

/// `keywords` must come from `KeywordSpec::to_keywords` (every piece checked against the model's tokens): the
/// spotter exits the process on a keyword it cannot encode.
fn create_spotter(store: &ModelStore, keywords: &str) -> Result<KeywordSpotter, String> {
    if keywords.is_empty() || keywords.contains('\0') {
        return Err("The wake word could not be built.".into());
    }
    let p = store.kws_paths();
    let mut c = KeywordSpotterConfig::default();
    c.model_config.transducer.encoder = Some(path_str(&p.encoder)?);
    c.model_config.transducer.decoder = Some(path_str(&p.decoder)?);
    c.model_config.transducer.joiner = Some(path_str(&p.joiner)?);
    c.model_config.tokens = Some(path_str(&p.tokens)?);
    c.model_config.num_threads = 1;
    c.model_config.provider = Some("cpu".into());
    c.keywords_buf = Some(keywords.replace('/', "\n"));
    KeywordSpotter::create(&c).ok_or_else(|| "Could not load the wake word model.".to_string())
}

fn transcribe(asr: &OfflineRecognizer, samples: &[f32]) -> String {
    let mut padded = Vec::with_capacity(samples.len() + SR / 2);
    padded.extend_from_slice(samples);
    padded.resize(samples.len() + SR * 3 / 10, 0.0);
    let stream = asr.create_stream();
    stream.accept_waveform(SAMPLE_RATE as i32, &padded);
    asr.decode(&stream);
    stream.get_result().map(|r| r.text).unwrap_or_default()
}

pub struct Engine {
    store: ModelStore,
    config: VoiceConfig,
    sink: Box<dyn Sink>,
    tx: SyncSender<Msg>,
    mic: Box<dyn MicOpener>,
    /// Asks the OS whether the screen is locked (`lock::screen_locked` in the app; none or a fake in tests).
    lock_probe: Option<LockProbe>,

    models_ready: bool,
    loading: bool,
    loaded: Option<Loaded>,
    load_error: Option<String>,
    spotter: Option<KeywordSpotter>,
    /// What the spotter was built for (the spoken name and the sensitivity), also when building failed.
    wake_key: Option<(String, u32)>,
    wake_error: Option<String>,

    open_mic: Option<OpenMic>,
    generation: u64,
    resampler: Option<LinearResampler>,
    mic_error: Option<String>,
    mic_retry_at: Option<Instant>,

    ring: VecDeque<f32>,
    /// 16 kHz samples since the mic opened.
    total: u64,
    kws_stream: Option<OnlineStream>,
    kws_origin: u64,
    utt: Option<Utterance>,
    phase: Phase,

    mic_test_until: Option<Instant>,
    downloading: bool,
    screen_locked: bool,
    next_lock_check: Instant,
    last_level: Option<Instant>,
    published: Option<(VoiceState, Option<String>, bool)>,
    reported_errors: Vec<String>,
    transcript_seq: u64,
    session_tag: String,
}

impl Drop for Engine {
    fn drop(&mut self) {
        // The keyword stream before its spotter, the utterance before the models.
        self.kws_stream = None;
        self.utt = None;
        self.open_mic = None;
    }
}

impl Engine {
    pub fn new(
        store: ModelStore,
        config: VoiceConfig,
        sink: Box<dyn Sink>,
        mic: Box<dyn MicOpener>,
        tx: SyncSender<Msg>,
        lock_probe: Option<LockProbe>,
    ) -> Self {
        let tag = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let mut e = Self {
            store,
            config,
            sink,
            tx,
            mic,
            lock_probe,
            models_ready: false,
            loading: false,
            loaded: None,
            load_error: None,
            spotter: None,
            wake_key: None,
            wake_error: None,
            open_mic: None,
            generation: 0,
            resampler: None,
            mic_error: None,
            mic_retry_at: None,
            ring: VecDeque::with_capacity(RING_SAMPLES),
            total: 0,
            kws_stream: None,
            kws_origin: 0,
            utt: None,
            phase: Phase::None,
            mic_test_until: None,
            downloading: false,
            screen_locked: false,
            next_lock_check: Instant::now(),
            last_level: None,
            published: None,
            reported_errors: Vec::new(),
            transcript_seq: 0,
            session_tag: format!("v{tag:x}"),
        };
        e.refresh_models();
        e
    }

    /// The worker loop, until `Shutdown` (app exit) or every sender is gone.
    pub fn run(mut self, rx: Receiver<Msg>) {
        self.reconcile();
        loop {
            let idle = !self.config.enabled && !self.downloading && self.mic_test_until.is_none();
            let msg = if idle {
                rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
            } else {
                rx.recv_timeout(TICK)
            };
            match msg {
                Ok(Msg::Cmd(Cmd::Shutdown)) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(m) => self.handle(m),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.tick();
            self.reconcile();
        }
        self.unload();
    }

    pub fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Capture(CaptureMsg::Audio { generation, samples }) => {
                if generation != self.generation || self.open_mic.is_none() {
                    return;
                }
                let pcm = match &self.resampler {
                    Some(r) => r.resample(&samples, false),
                    None => samples,
                };
                self.on_audio(&pcm);
            }
            Msg::Capture(CaptureMsg::Failed { generation, message }) => {
                if generation == self.generation && self.open_mic.is_some() {
                    self.close_mic();
                    self.mic_failed(message);
                }
            }
            Msg::Cmd(cmd) => self.on_cmd(cmd),
        }
    }

    fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Configure(config, reply) => {
                self.configure(config);
                self.reconcile_without_loading();
                if let Some(r) = reply {
                    let _ = r.send(());
                }
            }
            Cmd::ModelsChanged(reply) => {
                self.load_error = None;
                self.refresh_models();
                self.reconcile_without_loading();
                if let Some(r) = reply {
                    let _ = r.send(());
                }
            }
            Cmd::ReleaseModels(reply) => {
                self.unload();
                self.models_ready = false;
                self.publish();
                if let Some(r) = reply {
                    let _ = r.send(());
                }
            }
            Cmd::Downloading(on) => self.downloading = on,
            Cmd::Listen(action, reply) => {
                let res = self.listen(action);
                if let Some(r) = reply {
                    let _ = r.send(res);
                }
            }
            Cmd::MicTest(on, reply) => {
                let res = self.mic_test(on);
                if let Some(r) = reply {
                    let _ = r.send(res);
                }
            }
            Cmd::Shutdown => self.unload(),
        }
    }

    fn configure(&mut self, new: VoiceConfig) {
        let old = std::mem::replace(&mut self.config, new);
        let c = self.config.clone();
        if old.mic_id != c.mic_id {
            self.close_mic();
        }
        if !c.enabled || (c.muted && !old.muted) {
            self.cancel_utterance();
            self.mic_test_until = None;
        }
        if old.enabled != c.enabled || old.asr_model != c.asr_model {
            self.load_error = None;
        }
        self.mic_error = None;
        self.mic_retry_at = None;
        self.refresh_models();
    }

    fn refresh_models(&mut self) {
        let asr = match self.config.asr_model {
            AsrModel::Fast => ModelId::Moonshine,
            AsrModel::Accurate => ModelId::Parakeet,
        };
        self.models_ready = [ModelId::Kws, ModelId::Vad, asr].iter().all(|id| self.store.installed(*id));
    }

    fn inputs(&self) -> Inputs {
        Inputs {
            enabled: self.config.enabled,
            muted: self.config.muted,
            wake_word: self.config.wake_word,
            models_ready: self.models_ready,
            downloading: self.downloading,
            loading: self.loading,
            loaded: self.loaded.as_ref().is_some_and(|l| l.asr_model == self.config.asr_model),
            load_failed: self.load_error.is_some(),
            wake_ready: self.spotter.is_some(),
            mic_failed: self.mic_error.is_some(),
            mic_open: self.open_mic.is_some(),
            screen_locked: self.screen_locked,
            mic_test: self.mic_test_until.is_some(),
            phase: self.phase,
        }
    }

    fn listen(&mut self, action: ListenAction) -> Result<(), String> {
        match action {
            ListenAction::Start => {
                if self.phase != Phase::None {
                    return Ok(());
                }
                // The poll may be up to a few seconds old.
                self.check_screen_lock();
                let i = self.inputs();
                if !i.can_listen() {
                    return Err(match () {
                        _ if !i.enabled => "Voice is off.",
                        _ if i.muted => "The microphone is muted.",
                        _ if !i.models_ready => "The voice models are not downloaded yet.",
                        _ if i.load_failed => "The voice models could not be loaded.",
                        _ if i.screen_locked => "The screen is locked.",
                        _ => "Voice is still loading.",
                    }
                    .to_string());
                }
                let pre = self.ring_tail(PUSH_PRE_ROLL);
                self.start_utterance(Trigger::Push, &pre);
                // Without the wake word the mic opens now.
                self.reconcile_without_loading();
                if self.open_mic.is_none() {
                    let err = self.mic_error.clone().unwrap_or_else(|| "Could not open the microphone.".into());
                    self.cancel_utterance();
                    self.publish();
                    return Err(err);
                }
                Ok(())
            }
            ListenAction::Stop => {
                if self.phase == Phase::Hearing {
                    self.finish_utterance(End::Stop);
                }
                Ok(())
            }
            ListenAction::Cancel => {
                if self.phase == Phase::Hearing {
                    self.cancel_utterance();
                    self.publish();
                }
                Ok(())
            }
        }
    }

    fn mic_test(&mut self, on: bool) -> Result<(), String> {
        if !on {
            self.mic_test_until = None;
            return Ok(());
        }
        if !self.config.enabled {
            return Err("Turn voice on to test the microphone.".into());
        }
        if self.config.muted {
            return Err("The microphone is muted.".into());
        }
        self.check_screen_lock();
        if self.screen_locked {
            return Err("The screen is locked.".into());
        }
        self.mic_test_until = Some(Instant::now() + MIC_TEST_MAX);
        self.mic_error = None;
        self.mic_retry_at = None;
        self.reconcile_without_loading();
        match (&self.open_mic, &self.mic_error) {
            (Some(_), _) => Ok(()),
            (None, Some(e)) => {
                let e = e.clone();
                self.mic_test_until = None;
                Err(e)
            }
            (None, None) => Ok(()),
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if self.mic_test_until.is_some_and(|t| now >= t) {
            self.mic_test_until = None;
        }
        // While voice could use the mic at all (also push-to-talk only: it can start any moment).
        if self.config.enabled && !self.config.muted && now >= self.next_lock_check {
            self.next_lock_check = now + lock::POLL_EVERY;
            self.check_screen_lock();
        }
        if !self.config.enabled || self.config.muted {
            self.screen_locked = false;
        }
    }

    /// Ask the OS now; on lock, end the utterance and the mic test (the mic follows in `sync_mic`).
    fn check_screen_lock(&mut self) {
        let Some(probe) = self.lock_probe else {
            return;
        };
        let locked = probe().unwrap_or(false);
        if locked && !self.screen_locked {
            self.cancel_utterance();
            self.mic_test_until = None;
        }
        self.screen_locked = locked;
    }

    /// Load or unload models, keep the wake word in step with the config, open or close the mic, publish.
    pub fn reconcile(&mut self) {
        self.sync_models(true);
        self.sync_wake();
        self.sync_mic();
        self.publish();
    }

    /// The same without starting a (slow) model load: for command replies, which should not wait for it.
    fn reconcile_without_loading(&mut self) {
        self.sync_models(false);
        self.sync_wake();
        self.sync_mic();
        self.publish();
    }

    fn sync_models(&mut self, may_load: bool) {
        if !self.config.enabled || !self.models_ready {
            if self.loaded.is_some() || self.spotter.is_some() {
                self.unload();
            }
            return;
        }
        let current = self.loaded.as_ref().is_some_and(|l| l.asr_model == self.config.asr_model);
        if current || self.load_error.is_some() || !may_load {
            return;
        }
        // No listening with half-swapped models (and no audio piling up while they load).
        self.cancel_utterance();
        self.close_mic();
        self.loading = true;
        self.publish();
        let model = self.config.asr_model;
        let result = (|| -> Result<Loaded, String> {
            let vad = create_vad(&self.store)?;
            let asr = create_asr(&self.store, model)?;
            let p = self.store.kws_paths();
            let encoder = KeywordEncoder::load(&p.bpe, &p.tokens);
            Ok(Loaded { asr_model: model, vad, asr, encoder })
        })();
        self.loading = false;
        match result {
            Ok(l) => {
                self.loaded = Some(l);
                // A new tokenizer: rebuild the spotter.
                self.wake_key = None;
            }
            Err(e) => {
                self.loaded = None;
                self.report(&e);
                self.load_error = Some(e);
            }
        }
    }

    fn sync_wake(&mut self) {
        let c = &self.config;
        let want = c.enabled && c.wake_word && self.loaded.is_some();
        if !want {
            self.kws_stream = None;
            self.spotter = None;
            self.wake_key = None;
            self.wake_error = None;
            return;
        }
        let spoken = c.spoken_keyword().trim().to_string();
        let key = (spoken.clone(), c.sensitivity.to_bits());
        if self.wake_key.as_ref() == Some(&key) {
            return;
        }
        self.kws_stream = None;
        self.spotter = None;
        self.wake_key = Some(key);
        let built = if spoken.is_empty() {
            Err("The wake word needs the pet's name.".to_string())
        } else {
            match self.loaded.as_ref().map(|l| &l.encoder) {
                Some(Ok(enc)) => enc
                    .spec(&spoken, c.sensitivity)
                    .and_then(|spec| create_spotter(&self.store, &spec.to_keywords())),
                Some(Err(e)) => Err(e.clone()),
                None => Err("Voice is still loading.".to_string()),
            }
        };
        match built {
            Ok(s) => {
                self.kws_stream = Some(s.create_stream());
                self.kws_origin = self.total;
                self.spotter = Some(s);
                self.wake_error = None;
            }
            Err(e) => {
                self.report(&e);
                self.wake_error = Some(e);
            }
        }
    }

    fn sync_mic(&mut self) {
        let want = self.inputs().mic_wanted();
        if !want {
            if self.open_mic.is_some() {
                self.close_mic();
            }
            self.mic_error = None;
            self.mic_retry_at = None;
            return;
        }
        if self.open_mic.is_some() || self.mic_retry_at.is_some_and(|t| Instant::now() < t) {
            return;
        }
        self.generation += 1;
        match self.mic.open(self.config.mic_id.as_deref(), self.generation, self.tx.clone()) {
            Ok(m) => {
                self.resampler = if m.sample_rate == SAMPLE_RATE {
                    None
                } else {
                    i32::try_from(m.sample_rate).ok().and_then(|r| LinearResampler::create(r, SAMPLE_RATE as i32))
                };
                if m.sample_rate != SAMPLE_RATE && self.resampler.is_none() {
                    self.mic_failed(format!("The microphone's rate ({} Hz) is not supported.", m.sample_rate));
                    return;
                }
                self.open_mic = Some(m);
                self.ring.clear();
                self.total = 0;
                self.kws_stream = self.spotter.as_ref().map(KeywordSpotter::create_stream);
                self.kws_origin = 0;
                self.mic_error = None;
                self.mic_retry_at = None;
            }
            Err(e) => self.mic_failed(e),
        }
    }

    fn mic_failed(&mut self, message: String) {
        self.report(&message);
        self.mic_error = Some(message);
        self.mic_retry_at = Some(Instant::now() + MIC_RETRY);
        if self.utt.as_ref().is_some_and(|u| u.trigger == Trigger::Push) && self.open_mic.is_none() {
            self.cancel_utterance();
        }
    }

    fn close_mic(&mut self) {
        self.open_mic = None;
        self.resampler = None;
        self.ring.clear();
        self.total = 0;
        self.kws_stream = None;
        if self.phase == Phase::Hearing {
            self.cancel_utterance();
        }
    }

    fn unload(&mut self) {
        self.cancel_utterance();
        self.close_mic();
        self.kws_stream = None;
        self.spotter = None;
        self.wake_key = None;
        self.loaded = None;
    }

    /// Emit an error event once per distinct message (repeats while it persists are not news).
    fn report(&mut self, message: &str) {
        if !self.reported_errors.iter().any(|m| m == message) {
            self.reported_errors.push(message.to_string());
            if self.reported_errors.len() > 16 {
                self.reported_errors.remove(0);
            }
            self.sink.emit(VoiceEvent::Error { message: message.to_string() });
        }
    }

    fn publish(&mut self) {
        let state = self.inputs().state();
        let error = self.load_error.clone().or_else(|| self.mic_error.clone()).or_else(|| self.wake_error.clone());
        let live = self.open_mic.is_some();
        let next = (state, error.clone(), live);
        if self.published.as_ref() == Some(&next) {
            return;
        }
        let changed = self.published.as_ref().map(|p| p.0) != Some(state);
        self.published = Some(next);
        // Errors that went away can be reported again later.
        if error.is_none() {
            self.reported_errors.clear();
        }
        self.sink.publish(state, error, live);
        if changed {
            self.sink.emit(VoiceEvent::State { state });
        }
    }

    fn ring_tail(&self, n: usize) -> Vec<f32> {
        let n = n.min(self.ring.len());
        self.ring.iter().skip(self.ring.len() - n).copied().collect()
    }

    fn on_audio(&mut self, pcm: &[f32]) {
        if pcm.is_empty() {
            return;
        }
        self.total += pcm.len() as u64;
        self.ring.extend(pcm.iter().copied());
        let over = self.ring.len().saturating_sub(RING_SAMPLES);
        self.ring.drain(..over);

        if self.phase == Phase::Hearing || self.mic_test_until.is_some() {
            let now = Instant::now();
            if self.last_level.is_none_or(|t| now.duration_since(t) >= LEVEL_EVERY) {
                self.last_level = Some(now);
                self.sink.emit(VoiceEvent::Level { rms: level(pcm) });
            }
        }

        if self.phase == Phase::None {
            if let (Some(spotter), Some(stream)) = (&self.spotter, &self.kws_stream) {
                stream.accept_waveform(SAMPLE_RATE as i32, pcm);
                let mut hit = None;
                while spotter.is_ready(stream) {
                    spotter.decode(stream);
                    if let Some(r) = spotter.get_result(stream) {
                        if !r.keyword.is_empty() {
                            spotter.reset(stream);
                            hit = Some(r);
                            break;
                        }
                    }
                }
                if let Some(r) = hit {
                    let start = wake_start(self.total, self.kws_origin, r.start_time, r.timestamps.last().copied());
                    let back = usize::try_from(self.total.saturating_sub(start)).unwrap_or(0);
                    let pre = self.ring_tail(back);
                    self.sink.emit(VoiceEvent::Wake { keyword: self.config.keyword.clone(), score: 1.0 });
                    self.start_utterance(Trigger::Wake, &pre);
                    return;
                }
            }
        }

        if self.phase == Phase::Hearing {
            let end = match (&mut self.utt, &self.loaded) {
                (Some(u), Some(l)) => {
                    u.push(pcm, &l.vad);
                    u.end_reason(&l.vad)
                }
                _ => None,
            };
            if let Some(end) = end {
                self.finish_utterance(end);
            }
        }
    }

    fn start_utterance(&mut self, trigger: Trigger, pre: &[f32]) {
        let Some(l) = &self.loaded else {
            return;
        };
        l.vad.reset();
        let mut u = Utterance::new(trigger);
        u.push(pre, &l.vad);
        self.utt = Some(u);
        self.phase = Phase::Hearing;
        self.last_level = None;
        self.publish();
    }

    fn cancel_utterance(&mut self) {
        self.utt = None;
        if self.phase != Phase::None {
            self.phase = Phase::None;
            if let Some(l) = &self.loaded {
                l.vad.reset();
            }
            self.restart_spotting();
        }
    }

    /// A fresh keyword stream after an utterance (its own words must not wake the pet again).
    fn restart_spotting(&mut self) {
        self.kws_stream = None;
        if self.open_mic.is_some() {
            self.kws_stream = self.spotter.as_ref().map(KeywordSpotter::create_stream);
            self.kws_origin = self.total;
        }
    }

    fn finish_utterance(&mut self, _end: End) {
        let Some(mut u) = self.utt.take() else {
            return;
        };
        self.phase = Phase::Transcribing;
        self.publish();
        let raw = match &self.loaded {
            Some(l) => {
                u.finish(&l.vad);
                l.vad.reset();
                match speech_range(&u.spans, u.samples.len()) {
                    Some((a, b)) => transcribe(&l.asr, &u.samples[a..b]),
                    None => String::new(),
                }
            }
            None => String::new(),
        };
        // The audio is not needed any more.
        drop(u.samples);
        let text = clean_transcript(&raw, u.trigger == Trigger::Wake, self.config.spoken_keyword());
        self.transcript_seq += 1;
        let id = format!("{}-{}", self.session_tag, self.transcript_seq);
        self.sink.emit(VoiceEvent::Transcript { id, text });
        self.phase = Phase::None;
        self.restart_spotting();
        self.publish();
    }

    #[cfg(test)]
    fn feed(&mut self, samples: Vec<f32>) {
        let generation = self.generation;
        self.handle(Msg::Capture(CaptureMsg::Audio { generation, samples }));
    }
}

/// Fakes for the engine's tests and the glue's (`voice::tests`): a recording sink and a pretend microphone.
#[cfg(test)]
pub(super) mod testing {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// The last `publish`: state, error, mic live.
    pub type Published = (VoiceState, Option<String>, bool);

    #[derive(Clone, Default)]
    pub struct Recorder {
        pub events: Arc<Mutex<Vec<VoiceEvent>>>,
        pub status: Arc<Mutex<Option<Published>>>,
    }

    impl Sink for Recorder {
        fn emit(&self, event: VoiceEvent) {
            self.events.lock().unwrap().push(event);
        }
        fn publish(&self, state: VoiceState, error: Option<String>, mic_live: bool) {
            *self.status.lock().unwrap() = Some((state, error, mic_live));
        }
    }

    impl Recorder {
        pub fn events(&self) -> Vec<VoiceEvent> {
            self.events.lock().unwrap().clone()
        }
        pub fn states(&self) -> Vec<VoiceState> {
            self.events()
                .into_iter()
                .filter_map(|e| match e {
                    VoiceEvent::State { state } => Some(state),
                    _ => None,
                })
                .collect()
        }
        pub fn transcripts(&self) -> Vec<(String, String)> {
            self.events()
                .into_iter()
                .filter_map(|e| match e {
                    VoiceEvent::Transcript { id, text } => Some((id, text)),
                    _ => None,
                })
                .collect()
        }
        pub fn state(&self) -> VoiceState {
            self.status.lock().unwrap().as_ref().unwrap().0
        }
    }

    /// A pretend microphone at `rate` Hz: counts opens, knows whether it is open.
    #[derive(Clone)]
    pub struct FakeMic {
        pub rate: u32,
        opens: Arc<AtomicUsize>,
        live: Arc<AtomicUsize>,
    }

    impl Default for FakeMic {
        fn default() -> Self {
            Self { rate: SAMPLE_RATE, opens: Arc::default(), live: Arc::default() }
        }
    }

    pub struct Live(Arc<AtomicUsize>);
    impl Drop for Live {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl MicOpener for FakeMic {
        fn open(&mut self, _: Option<&str>, _: u64, _: SyncSender<Msg>) -> Result<OpenMic, String> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            self.live.fetch_add(1, Ordering::SeqCst);
            Ok(OpenMic { sample_rate: self.rate, _handle: Box::new(Live(self.live.clone())) })
        }
    }

    impl FakeMic {
        pub fn at(rate: u32) -> Self {
            Self { rate, ..Default::default() }
        }
        pub fn is_open(&self) -> bool {
            self.live.load(Ordering::SeqCst) > 0
        }
        pub fn opens(&self) -> usize {
            self.opens.load(Ordering::SeqCst)
        }
    }

}

#[cfg(test)]
mod tests {
    use super::super::models::{self, ModelStore};
    use super::testing::{FakeMic, Recorder};
    use super::*;

    fn engine(store: ModelStore, config: VoiceConfig) -> (Engine, Recorder, FakeMic) {
        engine_at(store, config, SAMPLE_RATE)
    }

    fn engine_at(store: ModelStore, config: VoiceConfig, rate: u32) -> (Engine, Recorder, FakeMic) {
        let rec = Recorder::default();
        let mic = FakeMic::at(rate);
        let (tx, _rx) = std::sync::mpsc::sync_channel(8);
        let e = Engine::new(store, config, Box::new(rec.clone()), Box::new(mic.clone()), tx, None);
        (e, rec, mic)
    }

    fn empty_store(tag: &str) -> ModelStore {
        let d = std::env::temp_dir().join(format!("pawl-voice-engine-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        ModelStore::new(d)
    }

    fn cfg(enabled: bool) -> VoiceConfig {
        VoiceConfig { enabled, keyword: "Biscuit".into(), ..Default::default() }
    }

    fn configure(e: &mut Engine, c: VoiceConfig) {
        e.handle(Msg::Cmd(Cmd::Configure(c, None)));
        e.reconcile();
    }

    fn listen(e: &mut Engine, a: ListenAction) -> Result<(), String> {
        let (tx, rx) = std::sync::mpsc::channel();
        e.handle(Msg::Cmd(Cmd::Listen(a, Some(tx))));
        rx.recv().unwrap()
    }

    fn mic_test(e: &mut Engine, on: bool) -> Result<(), String> {
        let (tx, rx) = std::sync::mpsc::channel();
        e.handle(Msg::Cmd(Cmd::MicTest(on, Some(tx))));
        rx.recv().unwrap()
    }

    #[test]
    fn off_and_muted_never_open_the_mic() {
        let (mut e, rec, mic) = engine(empty_store("off"), cfg(false));
        e.reconcile();
        assert_eq!(rec.state(), VoiceState::Off);
        assert!(mic_test(&mut e, true).is_err(), "no mic test while voice is off");
        assert!(listen(&mut e, ListenAction::Start).is_err());
        assert_eq!(mic.opens(), 0);

        configure(&mut e, VoiceConfig { muted: true, ..cfg(true) });
        assert_eq!(rec.state(), VoiceState::MissingModels);
        assert!(mic_test(&mut e, true).is_err(), "no mic test while muted");
        assert!(listen(&mut e, ListenAction::Start).is_err());
        assert_eq!(mic.opens(), 0);
    }

    #[test]
    fn mic_test_opens_and_releases_the_mic_without_changing_state() {
        let (mut e, rec, mic) = engine(empty_store("mictest"), cfg(true));
        e.reconcile();
        assert_eq!(rec.state(), VoiceState::MissingModels);
        mic_test(&mut e, true).unwrap();
        assert!(mic.is_open());
        e.feed(vec![0.1; 1600]);
        assert!(rec.events().iter().any(|ev| matches!(ev, VoiceEvent::Level { rms } if *rms > 0.0)));
        assert_eq!(rec.states(), vec![VoiceState::MissingModels], "the test does not change the state");
        // Muting ends the test and closes the mic.
        configure(&mut e, VoiceConfig { muted: true, ..cfg(true) });
        assert!(!mic.is_open());
        configure(&mut e, cfg(true));
        assert!(!mic.is_open(), "the test does not come back by itself");
        mic_test(&mut e, true).unwrap();
        assert!(mic.is_open());
        mic_test(&mut e, false).unwrap();
        e.reconcile();
        assert!(!mic.is_open());
        configure(&mut e, cfg(false));
        assert_eq!(rec.state(), VoiceState::Off);
    }

    static FAKE_LOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    fn fake_lock() -> Option<bool> {
        Some(FAKE_LOCKED.load(std::sync::atomic::Ordering::SeqCst))
    }

    #[test]
    fn screen_lock_is_checked_without_the_wake_word_and_right_before_listening() {
        use std::sync::atomic::Ordering::SeqCst;
        FAKE_LOCKED.store(false, SeqCst);
        let (mut e, rec, mic) = engine(empty_store("lock"), VoiceConfig { wake_word: false, ..cfg(true) });
        e.lock_probe = Some(fake_lock);
        e.reconcile();
        // Locked since the last poll: a mic test (and push-to-talk) asks the OS again and is refused.
        FAKE_LOCKED.store(true, SeqCst);
        let err = mic_test(&mut e, true).unwrap_err();
        assert!(err.contains("locked"), "{err}");
        assert_eq!(mic.opens(), 0);
        // Unlocked: the test opens the mic; locking while it runs closes it at the next poll, push-to-talk only
        // mode included (wake word off).
        FAKE_LOCKED.store(false, SeqCst);
        mic_test(&mut e, true).unwrap();
        assert!(mic.is_open());
        FAKE_LOCKED.store(true, SeqCst);
        e.next_lock_check = Instant::now();
        e.tick();
        e.reconcile();
        assert!(!mic.is_open(), "released on lock");
        assert!(listen(&mut e, ListenAction::Start).is_err());
        assert_eq!(rec.states(), vec![VoiceState::MissingModels]);
        FAKE_LOCKED.store(false, SeqCst);
    }

    #[test]
    fn missing_models_state_and_push_to_talk_refused() {
        let (mut e, rec, mic) = engine(empty_store("missing"), cfg(true));
        e.reconcile();
        assert_eq!(rec.state(), VoiceState::MissingModels);
        let err = listen(&mut e, ListenAction::Start).unwrap_err();
        assert!(err.contains("not downloaded"), "{err}");
        assert_eq!(mic.opens(), 0);
        e.handle(Msg::Cmd(Cmd::Downloading(true)));
        e.reconcile();
        assert_eq!(rec.state(), VoiceState::Downloading);
        e.handle(Msg::Cmd(Cmd::Downloading(false)));
        e.reconcile();
        assert_eq!(rec.state(), VoiceState::MissingModels);
    }

    #[test]
    fn helpers() {
        assert_eq!(level(&[]), 0.0);
        assert_eq!(level(&[0.0; 160]), 0.0);
        assert!((level(&[1.0; 160]) - 1.0).abs() < 1e-6);
        let quiet = level(&[0.01; 160]);
        assert!(quiet > 0.2 && quiet < 0.5, "{quiet}");

        assert_eq!(speech_range(&[], 100), None);
        let len = 10 * SR;
        assert_eq!(speech_range(&[(SR, SR)], len), Some((SR - SPEECH_PAD_BEFORE, 2 * SR + SPEECH_PAD_AFTER)));
        assert_eq!(speech_range(&[(0, 10), (5 * SR, SR)], 6 * SR), Some((0, 6 * SR)));

        // Wake timing: the name ended 0.5 s before now -> start there.
        let now = 10 * SR as u64;
        let s = wake_start(now, 0, 9.0, Some(0.375));
        assert_eq!(s, (9.375 * SR as f64) as u64 + KEYWORD_TAIL as u64);
        // Earlier than the pre-roll allows -> clamped.
        assert_eq!(wake_start(now, 0, 7.0, Some(0.5)), now - WAKE_PRE_ROLL as u64);
        // Nonsense -> short look-back.
        assert_eq!(wake_start(now, 0, 99.0, Some(1.0)), now - PUSH_PRE_ROLL as u64);
        assert_eq!(wake_start(now, 0, 1.0, None), now - PUSH_PRE_ROLL as u64);
        assert_eq!(wake_start(100, 0, f32::NAN, Some(0.1)), 0);

        assert_eq!(clean_transcript("  Hey Biscuit,  run the\ntests. ", true, "Biscuit"), "run the tests.");
        assert_eq!(clean_transcript("Hey Biscuit, run the tests.", false, "Biscuit"), "Hey Biscuit, run the tests.");
        assert_eq!(clean_transcript("[BLANK_AUDIO]", false, "Biscuit"), "");
        assert_eq!(clean_transcript(" . , ", false, "Biscuit"), "");
        assert_eq!(clean_transcript(&"a".repeat(5000), false, "x").len(), MAX_TRANSCRIPT_CHARS);
    }

    #[test]
    fn events_serialize_like_the_contract() {
        let j = |e: VoiceEvent| serde_json::to_value(e).unwrap();
        assert_eq!(j(VoiceEvent::State { state: VoiceState::WakeListening }), serde_json::json!({"type":"state","state":"wake_listening"}));
        assert_eq!(j(VoiceEvent::Wake { keyword: "Rex".into(), score: 1.0 }), serde_json::json!({"type":"wake","keyword":"Rex","score":1.0}));
        assert_eq!(j(VoiceEvent::Level { rms: 0.5 }), serde_json::json!({"type":"level","rms":0.5}));
        assert_eq!(j(VoiceEvent::Transcript { id: "v1-1".into(), text: "hi".into() }), serde_json::json!({"type":"transcript","id":"v1-1","text":"hi"}));
        assert_eq!(
            j(VoiceEvent::Download { models: ModelSet::Accurate, received: 1, total: 2 }),
            serde_json::json!({"type":"download","models":"accurate","received":1,"total":2})
        );
        assert_eq!(j(VoiceEvent::Error { message: "x".into() }), serde_json::json!({"type":"error","message":"x"}));
        assert_eq!(serde_json::from_str::<ListenAction>("\"cancel\"").unwrap(), ListenAction::Cancel);
    }

    // ---------- with real models (local only: PAWL_VOICE_TEST_MODELS) ----------

    /// A model store laid out like a real install, linked (or copied) from the upstream-unpacked test folder.
    fn store_from_test_models(tag: &str, ids: &[ModelId]) -> Option<(ModelStore, PathBuf)> {
        let src = models::test_models_dir()?;
        let store = empty_store(tag);
        for id in ids {
            let spec = models::spec(*id);
            let dir = store.dir(*id);
            std::fs::create_dir_all(&dir).unwrap();
            for f in spec.files {
                let from = match spec.packaging {
                    models::Packaging::TarBz2 { prefix } => src.join(prefix).join(f.name),
                    models::Packaging::Single => src.join(f.name),
                };
                let to = dir.join(f.name);
                if std::fs::hard_link(&from, &to).is_err() {
                    std::fs::copy(&from, &to).unwrap_or_else(|e| panic!("{}: {e}", from.display()));
                }
            }
            assert!(store.installed(*id), "{id:?} incomplete in the test models folder");
        }
        Some((store, src))
    }

    use std::path::PathBuf;

    /// (rate, mono samples) of a test wav from the local models folder.
    fn read_wav(p: &Path) -> (u32, Vec<f32>) {
        let w = sherpa_onnx::Wave::read(p.to_str().unwrap()).unwrap_or_else(|| panic!("read {}", p.display()));
        (u32::try_from(w.sample_rate()).unwrap(), w.samples().to_vec())
    }

    /// Feed like a microphone does: 10 ms blocks at the mic's rate.
    fn feed_in_blocks(e: &mut Engine, pcm: &[f32], rate: u32) {
        for block in pcm.chunks(rate as usize / 100) {
            e.feed(block.to_vec());
        }
    }

    fn wait_loaded(e: &mut Engine, rec: &Recorder) {
        e.reconcile();
        assert!(
            matches!(rec.state(), VoiceState::Idle | VoiceState::WakeListening),
            "loaded: {:?} {:?}",
            rec.state(),
            rec.events()
        );
    }

    #[test]
    fn wake_word_then_transcript_with_real_models() {
        let ids = [ModelId::Kws, ModelId::Vad, ModelId::Moonshine];
        let Some((store, src)) = store_from_test_models("wake", &ids) else {
            eprintln!("skipped: set PAWL_VOICE_TEST_MODELS");
            return;
        };
        // "... the yellow lamps would LIGHT UP here and there the squalid quarter of the brothels"
        let (rate, wav) = read_wav(&src.join(models::KWS_TEST_SUBDIR).join("test_wavs").join("0.wav"));
        assert_eq!(rate, SAMPLE_RATE);
        let config = VoiceConfig { enabled: true, wake_word: true, keyword: "Light up".into(), ..Default::default() };
        let (mut e, rec, mic) = engine(store.clone(), config);
        let t0 = Instant::now();
        wait_loaded(&mut e, &rec);
        eprintln!("loaded in {:?}", t0.elapsed());
        assert_eq!(rec.state(), VoiceState::WakeListening);
        assert!(mic.is_open());

        let mut pcm = vec![0.0; SR / 2];
        pcm.extend_from_slice(&wav);
        pcm.extend(std::iter::repeat_n(0.0, 2 * SR));
        let t1 = Instant::now();
        feed_in_blocks(&mut e, &pcm, rate);
        eprintln!("processed {:.1} s of audio in {:?}", pcm.len() as f32 / SR as f32, t1.elapsed());
        let events = rec.events();
        assert!(events.iter().any(|ev| matches!(ev, VoiceEvent::Wake { keyword, .. } if keyword == "Light up")), "{events:?}");
        let states = rec.states();
        let i = states.iter().position(|s| *s == VoiceState::Hearing).expect("hearing");
        assert_eq!(states[i + 1], VoiceState::Transcribing);
        assert_eq!(*states.last().unwrap(), VoiceState::WakeListening);
        let ts = rec.transcripts();
        assert_eq!(ts.len(), 1, "{ts:?}");
        let text = ts[0].1.to_lowercase();
        eprintln!("transcript: {text:?}");
        assert!(text.contains("here and there"), "{text:?}");
        assert!(!text.starts_with("light up"), "the wake word is not part of the message: {text:?}");
        assert!(mic.is_open(), "still listening for the name");

        // Muting releases the mic; turning voice off unloads.
        let c = e.config.clone();
        configure(&mut e, VoiceConfig { muted: true, ..c });
        assert!(!mic.is_open());
        assert_eq!(rec.state(), VoiceState::Muted);
        let c = e.config.clone();
        configure(&mut e, VoiceConfig { enabled: false, muted: false, ..c });
        assert!(e.loaded.is_none() && e.spotter.is_none());
        assert_eq!(rec.state(), VoiceState::Off);
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn push_to_talk_with_real_models() {
        for (asr, id, wav_dir) in [
            (AsrModel::Fast, ModelId::Moonshine, "sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27"),
            (AsrModel::Accurate, ModelId::Parakeet, "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8"),
        ] {
            let Some((store, src)) = store_from_test_models(&format!("ptt-{asr:?}"), &[ModelId::Kws, ModelId::Vad, id])
            else {
                eprintln!("skipped: set PAWL_VOICE_TEST_MODELS");
                return;
            };
            // Whatever rate the clip has (Moonshine's is 24 kHz): the engine resamples like for a real mic.
            let (rate, wav) = read_wav(&src.join(wav_dir).join("test_wavs").join("0.wav"));
            let r = rate as usize;
            let config = VoiceConfig { enabled: true, wake_word: false, asr_model: asr, ..cfg(true) };
            let (mut e, rec, mic) = engine_at(store.clone(), config, rate);
            let t0 = Instant::now();
            wait_loaded(&mut e, &rec);
            eprintln!("{asr:?} loaded in {:?}", t0.elapsed());
            assert_eq!(rec.state(), VoiceState::Idle);
            assert!(!mic.is_open(), "no wake word: mic closed until push-to-talk");

            listen(&mut e, ListenAction::Start).unwrap();
            assert!(mic.is_open());
            assert_eq!(rec.state(), VoiceState::Hearing);
            listen(&mut e, ListenAction::Start).unwrap(); // a repeated press (key repeat) is harmless
            // Hold for the clip, release (stop) before the VAD would end it.
            let pcm: Vec<f32> = wav[..wav.len().min(12 * r)].to_vec();
            feed_in_blocks(&mut e, &pcm, rate);
            let t1 = Instant::now();
            listen(&mut e, ListenAction::Stop).unwrap();
            e.reconcile();
            let ts = rec.transcripts();
            eprintln!("{asr:?}: {:?} ({:?} after release)", ts, t1.elapsed());
            assert!(!ts.is_empty() && !ts[0].1.is_empty(), "{asr:?}: {ts:?}");
            assert!(!mic.is_open(), "released after the utterance");
            assert_eq!(rec.state(), VoiceState::Idle);

            // Cancel drops the utterance without a transcript.
            listen(&mut e, ListenAction::Start).unwrap();
            feed_in_blocks(&mut e, &pcm[..r], rate);
            listen(&mut e, ListenAction::Cancel).unwrap();
            e.reconcile();
            assert_eq!(rec.transcripts().len(), ts.len());
            assert!(!mic.is_open());

            // Silence only: an empty transcript after the no-speech timeout.
            listen(&mut e, ListenAction::Start).unwrap();
            feed_in_blocks(&mut e, &vec![0.0; (NO_SPEECH + SR) * r / SR], rate);
            e.reconcile();
            let last = rec.transcripts().last().cloned().unwrap();
            assert_eq!(last.1, "", "{last:?}");
            let ids: std::collections::HashSet<_> = rec.transcripts().into_iter().map(|t| t.0).collect();
            assert_eq!(ids.len(), rec.transcripts().len(), "unique ids");
            let _ = std::fs::remove_dir_all(store.root());
        }
    }
}
