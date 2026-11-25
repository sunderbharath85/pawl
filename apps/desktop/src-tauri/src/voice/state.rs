//! The voice state machine, kept pure: what the engine knows -> the `VoiceState` the UI shows, whether the
//! microphone may be open, and whether push-to-talk can start. The engine recomputes it after every change.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceState {
    Off,
    MissingModels,
    Downloading,
    Loading,
    Idle,
    WakeListening,
    Hearing,
    Transcribing,
    Muted,
    Error,
}

/// Where an utterance is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    None,
    /// Capturing what the user says (after the wake word or push-to-talk).
    Hearing,
    /// The speech model is turning it into text.
    Transcribing,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Inputs {
    pub enabled: bool,
    pub muted: bool,
    pub wake_word: bool,
    /// Every model the current config needs is on disk.
    pub models_ready: bool,
    pub downloading: bool,
    pub loading: bool,
    /// The models are in memory.
    pub loaded: bool,
    /// Loading the models failed (cleared by a config change, a download or a delete).
    pub load_failed: bool,
    /// The keyword list for the pet's name was built (false when the name can't be spelled for the model).
    pub wake_ready: bool,
    /// The microphone could not be opened while it was needed (retried in the background).
    pub mic_failed: bool,
    /// The microphone stream is running.
    pub mic_open: bool,
    pub screen_locked: bool,
    /// The Settings level meter is on.
    pub mic_test: bool,
    pub phase: Phase,
}

impl Inputs {
    fn usable(&self) -> bool {
        self.enabled && self.models_ready && self.loaded && !self.load_failed
    }

    fn listening_for_name(&self) -> bool {
        self.wake_word && self.wake_ready
    }

    /// The microphone may be open. Never while voice is off, muted or the screen is locked.
    pub fn mic_wanted(&self) -> bool {
        if !self.enabled || self.muted || self.screen_locked {
            return false;
        }
        if self.mic_test {
            return true;
        }
        if !self.usable() {
            return false;
        }
        match self.phase {
            Phase::Hearing => true,
            Phase::Transcribing | Phase::None => self.listening_for_name(),
        }
    }

    /// Push-to-talk (or the bubble's mic button) can start an utterance now.
    pub fn can_listen(&self) -> bool {
        self.usable() && !self.muted && !self.screen_locked && !self.downloading && self.phase == Phase::None
    }

    pub fn state(&self) -> VoiceState {
        if self.downloading {
            return VoiceState::Downloading;
        }
        if !self.enabled {
            return VoiceState::Off;
        }
        if !self.models_ready {
            return VoiceState::MissingModels;
        }
        if self.load_failed {
            return VoiceState::Error;
        }
        if self.loading || !self.loaded {
            return VoiceState::Loading;
        }
        match self.phase {
            Phase::Hearing => return VoiceState::Hearing,
            Phase::Transcribing => return VoiceState::Transcribing,
            Phase::None => {}
        }
        if self.muted {
            return VoiceState::Muted;
        }
        if self.mic_failed && self.listening_for_name() && !self.screen_locked {
            return VoiceState::Error;
        }
        if self.mic_open && self.listening_for_name() && !self.screen_locked {
            return VoiceState::WakeListening;
        }
        VoiceState::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Voice on, models on disk and loaded, wake word on and buildable, mic open.
    fn ready() -> Inputs {
        Inputs {
            enabled: true,
            wake_word: true,
            models_ready: true,
            loaded: true,
            wake_ready: true,
            mic_open: true,
            ..Default::default()
        }
    }

    #[test]
    fn off_by_default_and_mic_closed() {
        let i = Inputs::default();
        assert_eq!(i.state(), VoiceState::Off);
        assert!(!i.mic_wanted());
        assert!(!i.can_listen());
    }

    #[test]
    fn disabled_never_opens_the_mic() {
        for phase in [Phase::None, Phase::Hearing, Phase::Transcribing] {
            let i = Inputs { enabled: false, mic_test: true, phase, ..ready() };
            assert!(!i.mic_wanted(), "{phase:?}");
            assert_eq!(i.state(), VoiceState::Off);
        }
    }

    #[test]
    fn muted_never_opens_the_mic() {
        for phase in [Phase::None, Phase::Hearing] {
            let i = Inputs { muted: true, mic_test: true, phase, ..ready() };
            assert!(!i.mic_wanted());
        }
        let i = Inputs { muted: true, mic_open: false, ..ready() };
        assert_eq!(i.state(), VoiceState::Muted);
        assert!(!i.can_listen(), "no push-to-talk while muted");
    }

    #[test]
    fn screen_lock_releases_the_mic() {
        let i = Inputs { screen_locked: true, ..ready() };
        assert!(!i.mic_wanted());
        assert!(!i.can_listen());
        assert_eq!(i.state(), VoiceState::Idle);
        assert!(!Inputs { screen_locked: true, mic_test: true, ..ready() }.mic_wanted());
    }

    #[test]
    fn enable_walks_through_models_and_loading() {
        let mut i = Inputs { enabled: true, wake_word: true, ..Default::default() };
        assert_eq!(i.state(), VoiceState::MissingModels);
        assert!(!i.mic_wanted());
        i.downloading = true;
        assert_eq!(i.state(), VoiceState::Downloading);
        i.downloading = false;
        i.models_ready = true;
        assert_eq!(i.state(), VoiceState::Loading, "not loaded yet");
        i.loading = true;
        assert_eq!(i.state(), VoiceState::Loading);
        assert!(!i.mic_wanted(), "no mic before the models are in memory");
        i.loading = false;
        i.loaded = true;
        i.wake_ready = true;
        assert!(i.mic_wanted());
        assert_eq!(i.state(), VoiceState::Idle, "until the stream runs");
        i.mic_open = true;
        assert_eq!(i.state(), VoiceState::WakeListening);
        i.load_failed = true;
        assert_eq!(i.state(), VoiceState::Error);
        assert!(!i.mic_wanted());
    }

    #[test]
    fn downloading_shows_even_while_off() {
        let i = Inputs { downloading: true, ..Default::default() };
        assert_eq!(i.state(), VoiceState::Downloading);
        assert!(!i.mic_wanted());
    }

    #[test]
    fn push_to_talk_without_wake_word() {
        let mut i = Inputs { wake_word: false, mic_open: false, ..ready() };
        assert_eq!(i.state(), VoiceState::Idle);
        assert!(!i.mic_wanted(), "no wake word: mic closed until push-to-talk");
        assert!(i.can_listen());
        i.phase = Phase::Hearing;
        assert!(i.mic_wanted());
        assert_eq!(i.state(), VoiceState::Hearing);
        assert!(!i.can_listen(), "one utterance at a time");
        i.phase = Phase::Transcribing;
        assert!(!i.mic_wanted(), "released while transcribing");
        assert_eq!(i.state(), VoiceState::Transcribing);
        i.phase = Phase::None;
        assert_eq!(i.state(), VoiceState::Idle);
    }

    #[test]
    fn wake_word_keeps_the_mic_through_an_utterance() {
        let mut i = ready();
        assert_eq!(i.state(), VoiceState::WakeListening);
        i.phase = Phase::Hearing;
        assert_eq!(i.state(), VoiceState::Hearing);
        i.phase = Phase::Transcribing;
        assert!(i.mic_wanted());
        assert_eq!(i.state(), VoiceState::Transcribing);
    }

    #[test]
    fn unspellable_name_still_allows_push_to_talk() {
        let i = Inputs { wake_ready: false, mic_open: false, ..ready() };
        assert_eq!(i.state(), VoiceState::Idle);
        assert!(!i.mic_wanted());
        assert!(i.can_listen());
    }

    #[test]
    fn mic_failure_is_an_error_only_when_listening_for_the_name() {
        let i = Inputs { mic_failed: true, mic_open: false, ..ready() };
        assert_eq!(i.state(), VoiceState::Error);
        assert!(i.mic_wanted(), "keeps retrying");
        let i = Inputs { mic_failed: true, mic_open: false, wake_word: false, ..ready() };
        assert_eq!(i.state(), VoiceState::Idle);
    }

    #[test]
    fn mic_test_opens_the_mic_without_models() {
        let i = Inputs { enabled: true, mic_test: true, ..Default::default() };
        assert!(i.mic_wanted());
        assert_eq!(i.state(), VoiceState::MissingModels, "the test does not change the state");
    }

    #[test]
    fn states_serialize_like_the_contract() {
        let all = [
            (VoiceState::Off, "off"),
            (VoiceState::MissingModels, "missing_models"),
            (VoiceState::Downloading, "downloading"),
            (VoiceState::Loading, "loading"),
            (VoiceState::Idle, "idle"),
            (VoiceState::WakeListening, "wake_listening"),
            (VoiceState::Hearing, "hearing"),
            (VoiceState::Transcribing, "transcribing"),
            (VoiceState::Muted, "muted"),
            (VoiceState::Error, "error"),
        ];
        for (s, name) in all {
            assert_eq!(serde_json::to_value(s).unwrap(), name);
        }
    }
}
