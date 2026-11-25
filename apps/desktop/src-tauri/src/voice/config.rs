//! `VoiceConfig` (docs/CONTRACTS.md "v0.2 Voice"): the voice settings the UI edits, persisted by Rust in
//! `<app config dir>/voice.json`. Validation is strict for what the UI sends (a bad value is an error, nothing is
//! saved) and forgiving for what is on disk (a hand-edited or older file falls back to defaults field by field).

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const CONFIG_FILE: &str = "voice.json";

/// Longest pet name / "say it like" spelling we accept (hub pet names are at most 40 characters).
pub const MAX_KEYWORD_CHARS: usize = 64;
/// Longest microphone id (cpal ids are "<host>:<device id>", e.g. a WASAPI endpoint id of ~60 characters).
pub const MAX_MIC_ID_CHARS: usize = 512;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AsrModel {
    /// Moonshine tiny (English, quantized): the default.
    #[default]
    Fast,
    /// Parakeet TDT 110M (English, int8).
    Accurate,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VoiceConfig {
    /// Master switch (off by default).
    pub enabled: bool,
    /// Listen for the pet's name.
    pub wake_word: bool,
    /// The pet's name (the hub's `bot.petName`), updated by the UI on rename.
    pub keyword: String,
    /// Optional "say it like" spelling for names the speech model would not recognise as written.
    pub spoken_as: Option<String>,
    /// 0..1: higher wakes more easily (lower trigger threshold, more boost).
    pub sensitivity: f32,
    /// A cpal device id ("<host>:<id>"); `None` = the system default input.
    pub mic_id: Option<String>,
    pub asr_model: AsrModel,
    /// Push-to-talk global shortcut, e.g. "Ctrl+Alt+Space"; `None` = none.
    pub hotkey: Option<String>,
    /// Tray / right-click "Mute microphone": the mic is never opened while true.
    pub muted: bool,
    /// Speak the pet's replies (webview speech synthesis; the UI's business, stored here with the rest).
    pub read_replies: bool,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            wake_word: true,
            keyword: String::new(),
            spoken_as: None,
            sensitivity: 0.5,
            mic_id: None,
            asr_model: AsrModel::Fast,
            hotkey: None,
            muted: false,
            read_replies: false,
        }
    }
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

fn clean_opt(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

impl VoiceConfig {
    /// Trim text fields and turn blank optional strings into `None`.
    pub fn normalized(mut self) -> Self {
        self.keyword = self.keyword.trim().to_string();
        self.spoken_as = clean_opt(self.spoken_as);
        self.mic_id = clean_opt(self.mic_id);
        self.hotkey = clean_opt(self.hotkey);
        self
    }

    /// The config the UI sent, normalized, or a message saying what is wrong with it.
    pub fn validated(self) -> Result<Self, String> {
        let c = self.normalized();
        if !c.sensitivity.is_finite() || !(0.0..=1.0).contains(&c.sensitivity) {
            return Err("Sensitivity must be between 0 and 1.".into());
        }
        if c.keyword.chars().count() > MAX_KEYWORD_CHARS || has_control(&c.keyword) {
            return Err(format!("The pet's name must be one line of at most {MAX_KEYWORD_CHARS} characters."));
        }
        if let Some(s) = &c.spoken_as {
            if s.chars().count() > MAX_KEYWORD_CHARS || has_control(s) {
                return Err(format!("“Say it like” must be one line of at most {MAX_KEYWORD_CHARS} characters."));
            }
        }
        if let Some(m) = &c.mic_id {
            if m.chars().count() > MAX_MIC_ID_CHARS || has_control(m) {
                return Err("That microphone id is not valid.".into());
            }
        }
        if let Some(h) = &c.hotkey {
            super::hotkey::parse(h)?;
        }
        Ok(c)
    }

    /// What the wake word listens for: the "say it like" spelling when there is one, else the pet's name.
    pub fn spoken_keyword(&self) -> &str {
        self.spoken_as.as_deref().unwrap_or(&self.keyword)
    }

    /// A config read from disk made safe: each invalid field falls back to its default.
    fn sanitized(self) -> Self {
        let d = Self::default();
        let mut c = self.normalized();
        if !c.sensitivity.is_finite() || !(0.0..=1.0).contains(&c.sensitivity) {
            c.sensitivity = d.sensitivity;
        }
        if c.keyword.chars().count() > MAX_KEYWORD_CHARS || has_control(&c.keyword) {
            c.keyword = d.keyword;
        }
        if c.spoken_as.as_deref().is_some_and(|s| s.chars().count() > MAX_KEYWORD_CHARS || has_control(s)) {
            c.spoken_as = None;
        }
        if c.mic_id.as_deref().is_some_and(|m| m.chars().count() > MAX_MIC_ID_CHARS || has_control(m)) {
            c.mic_id = None;
        }
        if c.hotkey.as_deref().is_some_and(|h| super::hotkey::parse(h).is_err()) {
            c.hotkey = None;
        }
        c
    }
}

/// The saved config, or the defaults when there is none or it cannot be read.
pub fn load(path: &Path) -> VoiceConfig {
    let Ok(bytes) = std::fs::read(path) else {
        return VoiceConfig::default();
    };
    match serde_json::from_slice::<VoiceConfig>(&bytes) {
        Ok(c) => c.sanitized(),
        Err(err) => {
            eprintln!("[pawl] voice: ignoring unreadable {}: {err}", path.display());
            VoiceConfig::default()
        }
    }
}

/// Write the config atomically (temp file in the same directory, then rename).
pub fn save(path: &Path, config: &VoiceConfig) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| std::io::Error::other("config path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_vec_pretty(config).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!(".{CONFIG_FILE}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, json)?;
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pawl-voice-cfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn defaults_are_off_and_match_the_ui() {
        let c = VoiceConfig::default();
        assert!(!c.enabled);
        assert!(c.wake_word);
        assert!(!c.muted);
        assert_eq!(c.sensitivity, 0.5);
        assert_eq!(c.asr_model, AsrModel::Fast);
        assert_eq!(c.hotkey, None);
    }

    #[test]
    fn camel_case_json_round_trip() {
        let c = VoiceConfig {
            enabled: true,
            keyword: "Biscuit".into(),
            spoken_as: Some("biss kit".into()),
            asr_model: AsrModel::Accurate,
            hotkey: Some("Ctrl+Alt+Space".into()),
            ..Default::default()
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["wakeWord"], true);
        assert_eq!(v["spokenAs"], "biss kit");
        assert_eq!(v["asrModel"], "accurate");
        assert_eq!(v["micId"], serde_json::Value::Null);
        assert_eq!(v["readReplies"], false);
        let back: VoiceConfig = serde_json::from_value(v).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let c: VoiceConfig = serde_json::from_str(r#"{"enabled":true,"keyword":"Rex"}"#).unwrap();
        assert!(c.enabled && c.wake_word);
        assert_eq!(c.keyword, "Rex");
        assert_eq!(c.sensitivity, 0.5);
    }

    #[test]
    fn validation_rejects_bad_values() {
        let ok = VoiceConfig { keyword: "  Biscuit ".into(), spoken_as: Some("   ".into()), ..Default::default() };
        let v = ok.validated().unwrap();
        assert_eq!(v.keyword, "Biscuit");
        assert_eq!(v.spoken_as, None);

        for s in [-0.1, 1.5, f32::NAN, f32::INFINITY] {
            assert!(VoiceConfig { sensitivity: s, ..Default::default() }.validated().is_err(), "{s}");
        }
        assert!(VoiceConfig { keyword: "a\nb".into(), ..Default::default() }.validated().is_err());
        assert!(VoiceConfig { keyword: "x".repeat(65), ..Default::default() }.validated().is_err());
        assert!(VoiceConfig { spoken_as: Some("y".repeat(65)), ..Default::default() }.validated().is_err());
        assert!(VoiceConfig { mic_id: Some("bad\u{7}".into()), ..Default::default() }.validated().is_err());
        assert!(VoiceConfig { hotkey: Some("Space".into()), ..Default::default() }.validated().is_err());
        assert!(VoiceConfig { hotkey: Some("Ctrl+Alt+Space".into()), ..Default::default() }.validated().is_ok());
    }

    #[test]
    fn spoken_keyword_prefers_say_it_like() {
        let mut c = VoiceConfig { keyword: "Xyzzy".into(), ..Default::default() };
        assert_eq!(c.spoken_keyword(), "Xyzzy");
        c.spoken_as = Some("zizzy".into());
        assert_eq!(c.spoken_keyword(), "zizzy");
    }

    #[test]
    fn save_then_load() {
        let d = tmpdir("roundtrip");
        let p = d.join(CONFIG_FILE);
        assert_eq!(load(&p), VoiceConfig::default(), "no file = defaults");
        let c = VoiceConfig { enabled: true, keyword: "Pawl".into(), muted: true, ..Default::default() };
        save(&p, &c).unwrap();
        assert_eq!(load(&p), c);
        // No temp file left behind.
        let names: Vec<_> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from(CONFIG_FILE)]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn load_sanitizes_a_bad_file() {
        let d = tmpdir("sanitize");
        let p = d.join(CONFIG_FILE);
        std::fs::write(&p, br#"{"enabled":true,"sensitivity":7,"hotkey":"Q","keyword":"Rex","micId":"  "}"#).unwrap();
        let c = load(&p);
        assert!(c.enabled);
        assert_eq!(c.keyword, "Rex");
        assert_eq!(c.sensitivity, 0.5);
        assert_eq!(c.hotkey, None);
        assert_eq!(c.mic_id, None);
        std::fs::write(&p, b"not json").unwrap();
        assert_eq!(load(&p), VoiceConfig::default());
        let _ = std::fs::remove_dir_all(&d);
    }
}
