//! The voice models: what to download, from where, the pinned SHA-256 of each download, and which files of it to
//! keep. Models are never bundled with the app: they arrive only through `voice_download`, at the user's request,
//! into `<app data dir>/voice-models/<model>/`.
//!
//! Sources (k2-fsa/sherpa-onnx GitHub releases): https://github.com/k2-fsa/sherpa-onnx/releases/tag/kws-models and
//! https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models. Hashes checked 2026-11-24 against the downloads and,
//! where GitHub publishes one, the asset digest.

use serde::Serialize;
use std::path::{Path, PathBuf};

pub const MODELS_DIR: &str = "voice-models";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModelId {
    /// Open-vocabulary keyword spotter (zipformer, GigaSpeech, 3.3M params), Apache-2.0.
    Kws,
    /// Silero voice activity detector, MIT.
    Vad,
    /// Moonshine tiny English, quantized: the "fast" speech model, MIT.
    Moonshine,
    /// NVIDIA Parakeet TDT 110M English, int8: the "accurate" speech model, CC-BY-4.0.
    Parakeet,
}

#[derive(Clone, Copy, Debug)]
pub enum Packaging {
    /// A `.tar.bz2`; only the listed files under `prefix/` are extracted.
    TarBz2 { prefix: &'static str },
    /// The download is the single file `files[0]`.
    Single,
}

#[derive(Clone, Copy, Debug)]
pub struct ModelFile {
    pub name: &'static str,
    pub size: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct ModelSpec {
    pub id: ModelId,
    /// Directory under `voice-models/`.
    pub dir: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    /// Exact size of the download in bytes.
    pub download_size: u64,
    pub packaging: Packaging,
    pub files: &'static [ModelFile],
}

const KWS_ENCODER: &str = "encoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx";
const KWS_DECODER: &str = "decoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx";
const KWS_JOINER: &str = "joiner-epoch-12-avg-2-chunk-16-left-64.int8.onnx";

pub const KWS: ModelSpec = ModelSpec {
    id: ModelId::Kws,
    dir: "kws-zipformer-gigaspeech-3.3M-2024-01-01",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/kws-models/sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01.tar.bz2",
    sha256: "f170013b4716e41b62b9bfd809687c207cef798ef9bc6534d524e17af9b6561a",
    download_size: 17_626_723,
    packaging: Packaging::TarBz2 { prefix: "sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01" },
    files: &[
        ModelFile { name: KWS_ENCODER, size: 4_807_159 },
        ModelFile { name: KWS_DECODER, size: 277_985 },
        ModelFile { name: KWS_JOINER, size: 163_380 },
        ModelFile { name: "tokens.txt", size: 5_006 },
        ModelFile { name: "bpe.model", size: 244_837 },
    ],
};

pub const VAD: ModelSpec = ModelSpec {
    id: ModelId::Vad,
    dir: "silero-vad",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx",
    sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
    download_size: 643_854,
    packaging: Packaging::Single,
    files: &[ModelFile { name: "silero_vad.onnx", size: 643_854 }],
};

pub const MOONSHINE: ModelSpec = ModelSpec {
    id: ModelId::Moonshine,
    dir: "moonshine-tiny-en-quantized-2026-02-27",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27.tar.bz2",
    sha256: "9ec31b342d8fa3240c3b81b8f82e1cf7e3ac467c93ca5a999b741d5887164f8d",
    download_size: 29_858_559,
    packaging: Packaging::TarBz2 { prefix: "sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27" },
    files: &[
        ModelFile { name: "encoder_model.ort", size: 13_281_600 },
        ModelFile { name: "decoder_model_merged.ort", size: 30_412_256 },
        ModelFile { name: "tokens.txt", size: 549_350 },
        ModelFile { name: "LICENSE", size: 13_344 },
    ],
};

pub const PARAKEET: ModelSpec = ModelSpec {
    id: ModelId::Parakeet,
    dir: "parakeet-tdt-110m-en-int8",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8.tar.bz2",
    sha256: "f628312e9fdf8686374cb01a69425c41732529d540860311f16f37cbc32cfe9b",
    download_size: 108_035_095,
    packaging: Packaging::TarBz2 { prefix: "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8" },
    files: &[
        ModelFile { name: "encoder.int8.onnx", size: 131_113_202 },
        ModelFile { name: "decoder.int8.onnx", size: 3_955_863 },
        ModelFile { name: "joiner.int8.onnx", size: 1_411_403 },
        ModelFile { name: "tokens.txt", size: 9_953 },
    ],
};

pub const ALL: [ModelSpec; 4] = [KWS, VAD, MOONSHINE, PARAKEET];

pub fn spec(id: ModelId) -> &'static ModelSpec {
    ALL.iter().find(|s| s.id == id).expect("every ModelId has a spec")
}

/// The sets the UI downloads and deletes (`voice_download(models)`, `voice_delete_models(models)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelSet {
    /// Wake word + VAD + the fast speech model.
    Default,
    /// The accurate speech model (plus the wake word and VAD models if they are missing).
    Accurate,
}

impl ModelSet {
    /// The models a download of this set fetches when missing.
    pub fn download_ids(self) -> &'static [ModelId] {
        match self {
            ModelSet::Default => &[ModelId::Kws, ModelId::Vad, ModelId::Moonshine],
            ModelSet::Accurate => &[ModelId::Kws, ModelId::Vad, ModelId::Parakeet],
        }
    }

    /// The models whose presence `models.<set>.installed` reports.
    pub fn status_ids(self) -> &'static [ModelId] {
        match self {
            ModelSet::Default => &[ModelId::Kws, ModelId::Vad, ModelId::Moonshine],
            ModelSet::Accurate => &[ModelId::Parakeet],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeleteSet {
    Default,
    Accurate,
    All,
}

impl DeleteSet {
    pub fn ids(self) -> &'static [ModelId] {
        match self {
            DeleteSet::Default => &[ModelId::Kws, ModelId::Vad, ModelId::Moonshine],
            DeleteSet::Accurate => &[ModelId::Parakeet],
            DeleteSet::All => &[ModelId::Kws, ModelId::Vad, ModelId::Moonshine, ModelId::Parakeet],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SetStatus {
    pub installed: bool,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ModelsStatus {
    pub default: SetStatus,
    pub accurate: SetStatus,
}

/// Where the models live: `<app data dir>/voice-models`.
#[derive(Clone, Debug)]
pub struct ModelStore {
    root: PathBuf,
}

impl ModelStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir(&self, id: ModelId) -> PathBuf {
        self.root.join(spec(id).dir)
    }

    pub fn file(&self, id: ModelId, name: &str) -> PathBuf {
        self.dir(id).join(name)
    }

    /// Every expected file is there with its expected size (a model directory only appears by an atomic rename
    /// after its download was verified, so this catches later damage, not half-written downloads).
    pub fn installed(&self, id: ModelId) -> bool {
        let s = spec(id);
        s.files.iter().all(|f| std::fs::metadata(self.file(id, f.name)).is_ok_and(|m| m.is_file() && m.len() == f.size))
    }

    pub fn bytes(&self, id: ModelId) -> u64 {
        let Ok(entries) = std::fs::read_dir(self.dir(id)) else {
            return 0;
        };
        entries.filter_map(Result::ok).filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum()
    }

    pub fn set_status(&self, set: ModelSet) -> SetStatus {
        let ids = set.status_ids();
        SetStatus { installed: ids.iter().all(|id| self.installed(*id)), bytes: ids.iter().map(|id| self.bytes(*id)).sum() }
    }

    pub fn status(&self) -> ModelsStatus {
        ModelsStatus { default: self.set_status(ModelSet::Default), accurate: self.set_status(ModelSet::Accurate) }
    }

    /// Remove the models' directories (and any leftover temp directories of an interrupted download).
    pub fn delete(&self, ids: &[ModelId]) -> std::io::Result<()> {
        for id in ids {
            let d = self.dir(*id);
            if d.exists() {
                std::fs::remove_dir_all(&d)?;
            }
        }
        self.remove_leftovers();
        Ok(())
    }

    /// Temp files/directories of downloads that did not finish (a crash or a kill mid-download).
    pub fn remove_leftovers(&self) {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return;
        };
        for e in entries.filter_map(Result::ok) {
            let name = e.file_name();
            if name.to_string_lossy().starts_with(super::download::TEMP_PREFIX) {
                let p = e.path();
                let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
            }
        }
    }

    pub fn kws_paths(&self) -> KwsPaths {
        KwsPaths {
            encoder: self.file(ModelId::Kws, KWS_ENCODER),
            decoder: self.file(ModelId::Kws, KWS_DECODER),
            joiner: self.file(ModelId::Kws, KWS_JOINER),
            tokens: self.file(ModelId::Kws, "tokens.txt"),
            bpe: self.file(ModelId::Kws, "bpe.model"),
        }
    }
}

pub struct KwsPaths {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
    pub bpe: PathBuf,
}

/// For local tests only: a directory holding the models as the upstream archives unpack
/// (`PAWL_VOICE_TEST_MODELS`; nothing in the repo or in CI provides one, so those tests skip there).
#[cfg(test)]
pub fn test_models_dir() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("PAWL_VOICE_TEST_MODELS")?);
    d.is_dir().then_some(d)
}

#[cfg(test)]
pub const KWS_TEST_SUBDIR: &str = "sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01";

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> ModelStore {
        let d = std::env::temp_dir().join(format!("pawl-voice-models-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        ModelStore::new(d)
    }

    fn fake_install(s: &ModelStore, id: ModelId) {
        let d = s.dir(id);
        std::fs::create_dir_all(&d).unwrap();
        for f in spec(id).files {
            let file = std::fs::File::create(d.join(f.name)).unwrap();
            file.set_len(f.size).unwrap();
        }
    }

    #[test]
    fn catalog_is_pinned_and_https() {
        for s in ALL {
            assert!(s.url.starts_with("https://github.com/k2-fsa/sherpa-onnx/releases/download/"), "{:?}", s.id);
            assert_eq!(s.sha256.len(), 64);
            assert!(s.sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
            assert!(!s.files.is_empty() && s.download_size > 0);
            for f in s.files {
                assert!(!f.name.contains('/') && !f.name.contains('\\') && !f.name.starts_with('.'));
            }
        }
        // Only int8 / quantized weights are kept.
        assert!(KWS.files.iter().filter(|f| f.name.ends_with(".onnx")).all(|f| f.name.contains(".int8.")));
        assert!(PARAKEET.files.iter().filter(|f| f.name.ends_with(".onnx")).all(|f| f.name.contains(".int8.")));
    }

    #[test]
    fn install_status_and_delete() {
        let s = store("status");
        assert_eq!(s.status(), ModelsStatus::default());
        fake_install(&s, ModelId::Kws);
        fake_install(&s, ModelId::Vad);
        assert!(!s.status().default.installed, "moonshine still missing");
        fake_install(&s, ModelId::Moonshine);
        let st = s.status();
        assert!(st.default.installed && !st.accurate.installed);
        let want: u64 = [KWS, VAD, MOONSHINE].iter().flat_map(|m| m.files.iter()).map(|f| f.size).sum();
        assert_eq!(st.default.bytes, want);

        // A truncated file is not installed.
        let f = s.file(ModelId::Vad, "silero_vad.onnx");
        std::fs::OpenOptions::new().write(true).open(&f).unwrap().set_len(10).unwrap();
        assert!(!s.installed(ModelId::Vad));

        std::fs::create_dir_all(s.root().join(format!("{}x", super::super::download::TEMP_PREFIX))).unwrap();
        s.delete(DeleteSet::Default.ids()).unwrap();
        assert_eq!(s.status(), ModelsStatus::default());
        assert_eq!(std::fs::read_dir(s.root()).unwrap().count(), 0, "leftovers removed too");
        let _ = std::fs::remove_dir_all(s.root());
    }

    #[test]
    fn sets_parse_like_the_contract() {
        assert_eq!(serde_json::from_str::<ModelSet>("\"default\"").unwrap(), ModelSet::Default);
        assert_eq!(serde_json::from_str::<ModelSet>("\"accurate\"").unwrap(), ModelSet::Accurate);
        assert!(serde_json::from_str::<ModelSet>("\"all\"").is_err());
        assert_eq!(serde_json::from_str::<DeleteSet>("\"all\"").unwrap(), DeleteSet::All);
        let v = serde_json::to_value(ModelsStatus::default()).unwrap();
        assert_eq!(v, serde_json::json!({"default": {"installed": false, "bytes": 0}, "accurate": {"installed": false, "bytes": 0}}));
    }
}
