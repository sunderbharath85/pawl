//! Voice models are never shipped: they reach a user's machine only through `voice_download`, at the user's request.
//! This fails when a model file (or a model archive) is in the repository or named by the app bundle config.
//! scripts/check-no-models.sh does the same for git-tracked files and for built bundles.

use std::path::{Path, PathBuf};

/// Directories that hold build output, dependencies or VCS data (not part of the repository's own files).
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "gen", "dist", ".turbo", ".expo", ".wrangler", ".locks"];

fn forbidden(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".onnx", ".ort", ".bz2", ".rknn"].iter().any(|ext| lower.ends_with(ext))
        || lower == "tokens.txt"
        || lower == "bpe.model"
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().expect("repo root")
}

fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                walk(&e.path(), found);
            }
        } else if forbidden(&name) {
            found.push(e.path());
        }
    }
}

#[test]
fn no_model_files_in_the_repository() {
    let root = repo_root();
    assert!(root.join("PLAN.md").is_file(), "not the repo root: {}", root.display());
    let mut found = Vec::new();
    walk(&root, &mut found);
    assert!(found.is_empty(), "model files must not be in the repo (users download them in Settings): {found:#?}");
}

fn strings(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        serde_json::Value::Object(o) => o.iter().for_each(|(k, x)| {
            out.push(k.clone());
            strings(x, out);
        }),
        _ => {}
    }
}

#[test]
fn the_bundle_config_ships_no_models() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in ["tauri.conf.json", "tauri.release.json"] {
        let text = std::fs::read_to_string(dir.join(file)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut all = Vec::new();
        if let Some(bundle) = json.get("bundle") {
            strings(bundle, &mut all);
        }
        for s in all {
            let last = s.rsplit(['/', '\\']).next().unwrap_or(&s);
            assert!(
                !forbidden(last) && !s.contains("voice-models") && !s.contains("sherpa-onnx"),
                "{file}: the bundle must not carry voice models ({s})"
            );
        }
    }
}

#[test]
fn the_check_itself_matches_model_files() {
    for bad in ["encoder.int8.onnx", "decoder_model_merged.ort", "x.tar.bz2", "tokens.txt", "bpe.model", "m.RKNN"] {
        assert!(forbidden(bad), "{bad}");
    }
    for ok in ["tokens.ts", "model.rs", "bpe.md", "README.md", "icon.png"] {
        assert!(!forbidden(ok), "{ok}");
    }
}
