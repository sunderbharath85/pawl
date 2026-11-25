//! Model downloads: HTTPS only, from the pinned GitHub release URLs, into a temp file under `voice-models/`. The
//! whole download is checked against its pinned size and SHA-256 BEFORE anything is unpacked; then only the listed
//! files are extracted (regular files, at the expected path and size, under our own file names) into a temp
//! directory that is renamed into place. A cancel (or any failure) removes every partial file.

use super::models::{ModelSpec, ModelStore, Packaging};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use std::time::Duration;

/// Names of temp files and directories in `voice-models/` (removed on start and on delete).
pub const TEMP_PREFIX: &str = ".partial-";

#[derive(Debug, PartialEq)]
pub enum DownloadError {
    Cancelled,
    Failed(String),
}

impl DownloadError {
    fn failed(msg: impl Into<String>) -> Self {
        DownloadError::Failed(msg.into())
    }
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::Cancelled => f.write_str("download cancelled"),
            DownloadError::Failed(m) => f.write_str(m),
        }
    }
}

/// A cancel switch that also wakes an `await` blocked on the network.
#[derive(Default)]
pub struct CancelFlag {
    set: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl CancelFlag {
    pub fn cancel(&self) {
        self.set.store(true, Ordering::SeqCst);
        if let Some(w) = self.waker.lock().ok().and_then(|mut g| g.take()) {
            w.wake();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.set.load(Ordering::SeqCst)
    }

    fn check(&self) -> Result<(), DownloadError> {
        if self.is_cancelled() {
            Err(DownloadError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// `fut`'s output, or `None` as soon as the flag is set.
    pub async fn or_cancel<F: Future>(&self, fut: F) -> Option<F::Output> {
        let mut fut = pin!(fut);
        std::future::poll_fn(|cx| {
            if self.is_cancelled() {
                return Poll::Ready(None);
            }
            if let Poll::Ready(v) = fut.as_mut().poll(cx) {
                return Poll::Ready(Some(v));
            }
            if let Ok(mut g) = self.waker.lock() {
                *g = Some(cx.waker().clone());
            }
            // Set between the first check and storing the waker: don't sleep through it.
            if self.is_cancelled() {
                return Poll::Ready(None);
            }
            Poll::Pending
        })
        .await
    }
}

/// A path removed when dropped unless kept (temp files and directories, also on panic or cancel).
struct TempPath {
    path: PathBuf,
    keep: bool,
}

impl TempPath {
    fn new(path: PathBuf) -> Self {
        Self { path, keep: false }
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let _ = if self.path.is_dir() { std::fs::remove_dir_all(&self.path) } else { std::fs::remove_file(&self.path) };
    }
}

fn unique_suffix() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{}-{nanos:x}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn io_err(what: &str, e: impl std::fmt::Display) -> DownloadError {
    DownloadError::failed(format!("{what}: {e}"))
}

/// Check the downloaded file's size and SHA-256 against the pin, then (and only then) unpack the model into
/// `<store>/<spec.dir>`. Blocking. The archive file itself is left for the caller to remove.
pub fn install_from_file(store: &ModelStore, spec: &ModelSpec, archive: &Path, cancel: &CancelFlag) -> Result<(), DownloadError> {
    let mut file = std::fs::File::open(archive).map_err(|e| io_err("open download", e))?;
    let len = file.metadata().map_err(|e| io_err("open download", e))?.len();
    if len != spec.download_size {
        return Err(DownloadError::failed(format!(
            "{} has the wrong size ({len} bytes, expected {}); nothing was installed",
            spec.dir, spec.download_size
        )));
    }
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        cancel.check()?;
        let n = file.read(&mut buf).map_err(|e| io_err("read download", e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let got = hex(&hasher.finalize());
    if got != spec.sha256 {
        return Err(DownloadError::failed(format!(
            "{} failed its checksum (SHA-256 {got}, expected {}); nothing was installed",
            spec.dir, spec.sha256
        )));
    }
    // Unpack from the same open file that was hashed.
    file.seek(SeekFrom::Start(0)).map_err(|e| io_err("read download", e))?;

    std::fs::create_dir_all(store.root()).map_err(|e| io_err("create models folder", e))?;
    let tmp = TempPath::new(store.root().join(format!("{TEMP_PREFIX}{}-{}", spec.dir, unique_suffix())));
    std::fs::create_dir(&tmp.path).map_err(|e| io_err("create temp folder", e))?;

    match spec.packaging {
        Packaging::Single => {
            let f = spec.files[0];
            let mut out = std::fs::File::create(tmp.path.join(f.name)).map_err(|e| io_err("write model", e))?;
            copy_exact(&mut file, &mut out, f.size, cancel)?;
            out.sync_all().map_err(|e| io_err("write model", e))?;
        }
        Packaging::TarBz2 { prefix } => extract_tar_bz2(file, prefix, spec, &tmp.path, cancel)?,
    }
    for f in spec.files {
        let ok = std::fs::metadata(tmp.path.join(f.name)).is_ok_and(|m| m.is_file() && m.len() == f.size);
        if !ok {
            return Err(DownloadError::failed(format!("{}: {} is missing from the download", spec.dir, f.name)));
        }
    }
    cancel.check()?;
    let dest = store.root().join(spec.dir);
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| io_err("replace old model", e))?;
    }
    std::fs::rename(&tmp.path, &dest).map_err(|e| io_err("install model", e))?;
    let mut tmp = tmp;
    tmp.keep = true;
    Ok(())
}

/// Copy exactly `size` bytes (the reader must have them), checking for a cancel between blocks.
fn copy_exact(r: &mut impl Read, w: &mut impl Write, size: u64, cancel: &CancelFlag) -> Result<(), DownloadError> {
    let mut left = size;
    let mut buf = vec![0u8; 256 * 1024];
    while left > 0 {
        cancel.check()?;
        let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let n = r.read(&mut buf[..want]).map_err(|e| io_err("unpack model", e))?;
        if n == 0 {
            return Err(DownloadError::failed("unpack model: unexpected end of data"));
        }
        w.write_all(&buf[..n]).map_err(|e| io_err("write model", e))?;
        left -= n as u64;
    }
    Ok(())
}

fn extract_tar_bz2(
    file: std::fs::File,
    prefix: &str,
    spec: &ModelSpec,
    dest: &Path,
    cancel: &CancelFlag,
) -> Result<(), DownloadError> {
    let decoder = bzip2::read::MultiBzDecoder::new(std::io::BufReader::new(file));
    let mut archive = tar::Archive::new(decoder);
    let mut seen = vec![false; spec.files.len()];
    let entries = archive.entries().map_err(|e| io_err("read archive", e))?;
    for entry in entries {
        cancel.check()?;
        let mut entry = entry.map_err(|e| io_err("read archive", e))?;
        if entry.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let path = entry.path().map_err(|e| io_err("read archive", e))?.into_owned();
        // Some archives store "./dir/file": a leading "." is not a level.
        let mut comps = path.components().filter(|c| !matches!(c, std::path::Component::CurDir));
        let (Some(std::path::Component::Normal(dir)), Some(std::path::Component::Normal(name)), None) =
            (comps.next(), comps.next(), comps.next())
        else {
            continue;
        };
        if dir != prefix {
            continue;
        }
        let Some(idx) = spec.files.iter().position(|f| name == f.name) else {
            continue;
        };
        let want = spec.files[idx];
        if seen[idx] {
            return Err(DownloadError::failed(format!("{}: {} appears twice in the archive", spec.dir, want.name)));
        }
        let size = entry.header().size().map_err(|e| io_err("read archive", e))?;
        if size != want.size {
            return Err(DownloadError::failed(format!(
                "{}: {} has {size} bytes, expected {}",
                spec.dir, want.name, want.size
            )));
        }
        // Our own file name, never the archive's path.
        let mut out = std::fs::File::create(dest.join(want.name)).map_err(|e| io_err("write model", e))?;
        copy_exact(&mut entry, &mut out, want.size, cancel)?;
        out.sync_all().map_err(|e| io_err("write model", e))?;
        seen[idx] = true;
        if seen.iter().all(|s| *s) {
            break;
        }
    }
    Ok(())
}

/// Where model downloads may come from: GitHub, and the hosts its release downloads redirect to.
const DOWNLOAD_HOSTS: &[&str] = &["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"];
const MAX_REDIRECTS: usize = 5;

/// HTTPS on an allowed host (no other scheme, no other host, no credentials or odd port in the URL).
fn allowed_url(u: &reqwest::Url) -> bool {
    u.scheme() == "https"
        && u.username().is_empty()
        && u.password().is_none()
        && u.port().is_none()
        && u.host_str().is_some_and(|h| DOWNLOAD_HOSTS.contains(&h))
}

pub fn http_client() -> Result<reqwest::Client, String> {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else if allowed_url(attempt.url()) {
            attempt.follow()
        } else {
            let to = attempt.url().host_str().unwrap_or("?").to_string();
            attempt.error(format!("refused a redirect to {to}"))
        }
    });
    reqwest::Client::builder()
        .https_only(true)
        .redirect(policy)
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .user_agent(concat!("Pawl/", env!("CARGO_PKG_VERSION"), " (voice model download)"))
        .build()
        .map_err(|e| e.to_string())
}

/// Download `spec` and install it. `on_bytes` gets the byte count of each chunk as it arrives.
pub async fn download_model(
    client: &reqwest::Client,
    store: &ModelStore,
    spec: &'static ModelSpec,
    cancel: &Arc<CancelFlag>,
    mut on_bytes: impl FnMut(u64),
) -> Result<(), DownloadError> {
    std::fs::create_dir_all(store.root()).map_err(|e| io_err("create models folder", e))?;
    let tmp = TempPath::new(store.root().join(format!("{TEMP_PREFIX}{}-{}.download", spec.dir, unique_suffix())));
    let mut out = std::fs::File::create(&tmp.path).map_err(|e| io_err("create download file", e))?;

    let url = reqwest::Url::parse(spec.url).map_err(|e| io_err("model address", e))?;
    if !allowed_url(&url) {
        return Err(DownloadError::failed(format!("{} is not an allowed download address", spec.url)));
    }
    let resp = cancel
        .or_cancel(client.get(url).send())
        .await
        .ok_or(DownloadError::Cancelled)?
        .map_err(|e| io_err(&format!("download {}", spec.dir), e))?;
    if !resp.status().is_success() {
        return Err(DownloadError::failed(format!("download {}: HTTP {}", spec.dir, resp.status())));
    }
    if let Some(len) = resp.content_length() {
        if len != spec.download_size {
            return Err(DownloadError::failed(format!(
                "download {}: the server offers {len} bytes, expected {}",
                spec.dir, spec.download_size
            )));
        }
    }
    let mut resp = resp;
    let mut received: u64 = 0;
    loop {
        let chunk = cancel
            .or_cancel(resp.chunk())
            .await
            .ok_or(DownloadError::Cancelled)?
            .map_err(|e| io_err(&format!("download {}", spec.dir), e))?;
        let Some(chunk) = chunk else { break };
        received += chunk.len() as u64;
        if received > spec.download_size {
            return Err(DownloadError::failed(format!("download {}: more data than expected", spec.dir)));
        }
        out.write_all(&chunk).map_err(|e| io_err("write download", e))?;
        on_bytes(chunk.len() as u64);
    }
    drop(resp);
    out.sync_all().map_err(|e| io_err("write download", e))?;
    drop(out);
    if received != spec.download_size {
        return Err(DownloadError::failed(format!(
            "download {}: got {received} of {} bytes",
            spec.dir, spec.download_size
        )));
    }

    let (store2, path, cancel2) = (store.clone(), tmp.path.clone(), cancel.clone());
    tauri::async_runtime::spawn_blocking(move || install_from_file(&store2, spec, &path, &cancel2))
        .await
        .map_err(|e| io_err("install model", e))??;
    drop(tmp);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::models::{ModelFile, ModelId};
    use super::*;

    fn store(tag: &str) -> ModelStore {
        let d = std::env::temp_dir().join(format!("pawl-voice-dl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        ModelStore::new(d)
    }

    fn sha(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    /// A .tar.bz2 with `entries` (path, contents).
    fn tar_bz2(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data) in entries {
            // The name is written raw (not through set_path) so tests can build hostile paths like "m/../x".
            let mut h = tar::Header::new_gnu();
            let name = path.as_bytes();
            h.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name);
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_entry_type(tar::EntryType::Regular);
            h.set_cksum();
            builder.append(&h, *data).unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap()
    }

    fn leak<T>(v: T) -> &'static T {
        Box::leak(Box::new(v))
    }

    fn spec_for(archive: &[u8], sha256: &str, packaging: Packaging, files: &'static [ModelFile]) -> &'static ModelSpec {
        leak(ModelSpec {
            id: ModelId::Kws,
            dir: "test-model",
            url: "https://example.invalid/",
            sha256: leak(sha256.to_string()).as_str(),
            download_size: archive.len() as u64,
            packaging,
            files,
        })
    }

    const FILES: &[ModelFile] = &[ModelFile { name: "a.onnx", size: 5 }, ModelFile { name: "tokens.txt", size: 3 }];

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn installs_only_the_listed_files() {
        let s = store("ok");
        let archive = tar_bz2(&[
            ("m/a.onnx", b"AAAAA"),
            ("m/a.fp32.onnx", b"big weights"),
            ("m/test_wavs/0.wav", b"RIFF"),
            ("m/tokens.txt", b"x 0"),
            ("other/tokens.txt", b"no"),
        ]);
        let spec = spec_for(&archive, &sha(&archive), Packaging::TarBz2 { prefix: "m" }, FILES);
        let dl = std::env::temp_dir().join(format!("pawl-voice-dl-src-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dl).unwrap();
        let path = write(&dl, "x.tar.bz2", &archive);
        install_from_file(&s, spec, &path, &CancelFlag::default()).unwrap();
        let dest = s.root().join("test-model");
        assert_eq!(names(&dest), vec!["a.onnx", "tokens.txt"]);
        assert_eq!(std::fs::read(dest.join("a.onnx")).unwrap(), b"AAAAA");
        assert_eq!(names(s.root()), vec!["test-model"], "no temp dir left");
        let _ = std::fs::remove_dir_all(s.root());
        let _ = std::fs::remove_dir_all(&dl);
    }

    #[test]
    fn bad_hash_is_rejected_and_nothing_is_extracted() {
        let s = store("badhash");
        let archive = tar_bz2(&[("m/a.onnx", b"AAAAA"), ("m/tokens.txt", b"x 0")]);
        let spec = spec_for(&archive, &"0".repeat(64), Packaging::TarBz2 { prefix: "m" }, FILES);
        let dl = std::env::temp_dir().join(format!("pawl-voice-dl-src-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dl).unwrap();
        let path = write(&dl, "x.tar.bz2", &archive);
        let err = install_from_file(&s, spec, &path, &CancelFlag::default()).unwrap_err();
        assert!(matches!(&err, DownloadError::Failed(m) if m.contains("checksum")), "{err}");
        assert!(names(s.root()).is_empty(), "nothing extracted: {:?}", names(s.root()));

        // A tampered archive with the right size but other bytes is rejected the same way.
        let mut tampered = archive.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        let spec = spec_for(&archive, &sha(&archive), Packaging::TarBz2 { prefix: "m" }, FILES);
        let path = write(&dl, "y.tar.bz2", &tampered);
        assert!(install_from_file(&s, spec, &path, &CancelFlag::default()).is_err());
        assert!(names(s.root()).is_empty());

        // Wrong size: rejected before hashing.
        let path = write(&dl, "z.tar.bz2", &archive[..archive.len() - 1]);
        let err = install_from_file(&s, spec, &path, &CancelFlag::default()).unwrap_err();
        assert!(matches!(&err, DownloadError::Failed(m) if m.contains("wrong size")), "{err}");
        assert!(names(s.root()).is_empty());
        let _ = std::fs::remove_dir_all(s.root());
        let _ = std::fs::remove_dir_all(&dl);
    }

    #[test]
    fn rejects_archives_missing_files_or_with_wrong_sizes_or_traversal() {
        let s = store("shape");
        let dl = std::env::temp_dir().join(format!("pawl-voice-dl-src-shape-{}", std::process::id()));
        std::fs::create_dir_all(&dl).unwrap();
        let cases: Vec<Vec<u8>> = vec![
            tar_bz2(&[("m/a.onnx", b"AAAAA")]),                             // tokens.txt missing
            tar_bz2(&[("m/a.onnx", b"AAAA"), ("m/tokens.txt", b"x 0")]),    // wrong size
            tar_bz2(&[("m/../a.onnx", b"AAAAA"), ("m/tokens.txt", b"x 0")]), // traversal: ignored, so missing
            tar_bz2(&[("m/sub/a.onnx", b"AAAAA"), ("m/tokens.txt", b"x 0")]), // nested: ignored
            tar_bz2(&[("/m/a.onnx", b"AAAAA"), ("m/tokens.txt", b"x 0")]),     // absolute: ignored
        ];
        for (i, archive) in cases.iter().enumerate() {
            let spec = spec_for(archive, &sha(archive), Packaging::TarBz2 { prefix: "m" }, FILES);
            let path = write(&dl, &format!("{i}.tar.bz2"), archive);
            assert!(install_from_file(&s, spec, &path, &CancelFlag::default()).is_err(), "case {i}");
            assert!(names(s.root()).is_empty(), "case {i}: {:?}", names(s.root()));
        }
        assert!(!dl.parent().unwrap().join("a.onnx").exists());
        let _ = std::fs::remove_dir_all(s.root());
        let _ = std::fs::remove_dir_all(&dl);
    }

    #[test]
    fn accepts_a_leading_dot_directory() {
        let s = store("dot");
        let archive = tar_bz2(&[("./m/a.onnx", b"AAAAA"), ("./m/tokens.txt", b"x 0")]);
        let spec = spec_for(&archive, &sha(&archive), Packaging::TarBz2 { prefix: "m" }, FILES);
        let dl = std::env::temp_dir().join(format!("pawl-voice-dl-src-dot-{}", std::process::id()));
        std::fs::create_dir_all(&dl).unwrap();
        let path = write(&dl, "x.tar.bz2", &archive);
        install_from_file(&s, spec, &path, &CancelFlag::default()).unwrap();
        assert_eq!(names(&s.root().join("test-model")), vec!["a.onnx", "tokens.txt"]);
        let _ = std::fs::remove_dir_all(s.root());
        let _ = std::fs::remove_dir_all(&dl);
    }

    #[test]
    fn single_file_model_and_replacing_an_old_install() {
        let s = store("single");
        const ONE: &[ModelFile] = &[ModelFile { name: "silero_vad.onnx", size: 4 }];
        let bytes = b"onnx";
        let spec = spec_for(bytes, &sha(bytes), Packaging::Single, ONE);
        let dl = std::env::temp_dir().join(format!("pawl-voice-dl-src-single-{}", std::process::id()));
        std::fs::create_dir_all(&dl).unwrap();
        std::fs::create_dir_all(s.root().join("test-model")).unwrap();
        std::fs::write(s.root().join("test-model").join("stale.bin"), b"old").unwrap();
        let path = write(&dl, "vad", bytes);
        install_from_file(&s, spec, &path, &CancelFlag::default()).unwrap();
        assert_eq!(names(&s.root().join("test-model")), vec!["silero_vad.onnx"]);
        let _ = std::fs::remove_dir_all(s.root());
        let _ = std::fs::remove_dir_all(&dl);
    }

    #[test]
    fn cancel_stops_before_installing() {
        let s = store("cancel");
        let archive = tar_bz2(&[("m/a.onnx", b"AAAAA"), ("m/tokens.txt", b"x 0")]);
        let spec = spec_for(&archive, &sha(&archive), Packaging::TarBz2 { prefix: "m" }, FILES);
        let dl = std::env::temp_dir().join(format!("pawl-voice-dl-src-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dl).unwrap();
        let path = write(&dl, "x.tar.bz2", &archive);
        let c = CancelFlag::default();
        c.cancel();
        assert_eq!(install_from_file(&s, spec, &path, &c), Err(DownloadError::Cancelled));
        assert!(names(s.root()).is_empty());
        let _ = std::fs::remove_dir_all(s.root());
        let _ = std::fs::remove_dir_all(&dl);
    }

    #[test]
    fn only_https_github_hosts_are_allowed() {
        let ok = |u: &str| allowed_url(&reqwest::Url::parse(u).unwrap());
        assert!(ok("https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx"));
        assert!(ok("https://release-assets.githubusercontent.com/github-production-release-asset/1/x?sig=y"));
        assert!(ok("https://objects.githubusercontent.com/x"));
        for bad in [
            "http://github.com/x",
            "http://release-assets.githubusercontent.com/x",
            "https://github.com.evil.example/x",
            "https://evil.example/github.com",
            "https://user:pw@github.com/x",
            "https://github.com:8443/x",
            "ftp://github.com/x",
        ] {
            assert!(!ok(bad), "{bad}");
        }
        for spec in super::super::models::ALL {
            assert!(ok(spec.url), "{}", spec.url);
        }
    }

    #[test]
    fn or_cancel_wakes_a_pending_future() {
        let flag = Arc::new(CancelFlag::default());
        let f2 = flag.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            f2.cancel();
        });
        let out = tauri::async_runtime::block_on(flag.or_cancel(std::future::pending::<()>()));
        assert_eq!(out, None);
        t.join().unwrap();
        let out = tauri::async_runtime::block_on(CancelFlag::default().or_cancel(async { 7 }));
        assert_eq!(out, Some(7));
    }

    /// The real thing over the network (local only: PAWL_VOICE_TEST_NETWORK=1): HTTPS from GitHub with its
    /// redirect to the release CDN, pinned hash, install; then a cancel part-way leaves nothing behind.
    #[test]
    fn downloads_from_github_and_cancels_cleanly() {
        if std::env::var_os("PAWL_VOICE_TEST_NETWORK").is_none() {
            eprintln!("skipped: set PAWL_VOICE_TEST_NETWORK=1 to download from GitHub");
            return;
        }
        use super::super::models::{KWS, VAD};
        let s = store("net");
        let client = http_client().unwrap();
        let cancel = Arc::new(CancelFlag::default());
        let mut got = 0;
        tauri::async_runtime::block_on(download_model(&client, &s, &VAD, &cancel, |n| got += n)).unwrap();
        assert_eq!(got, VAD.download_size);
        assert!(s.installed(ModelId::Vad));

        let cancel = Arc::new(CancelFlag::default());
        let c2 = cancel.clone();
        let mut seen = 0;
        let r = tauri::async_runtime::block_on(download_model(&client, &s, &KWS, &cancel, |n| {
            seen += n;
            if seen > 1_000_000 {
                c2.cancel();
            }
        }));
        assert_eq!(r, Err(DownloadError::Cancelled));
        assert_eq!(names(s.root()), vec![VAD.dir], "the partial download was removed");
        let _ = std::fs::remove_dir_all(s.root());
    }

    #[test]
    fn pinned_catalog_hashes_match_local_archives() {
        // Optional: PAWL_VOICE_TEST_ARCHIVES = a folder with the upstream downloads (never the repo).
        let Some(dir) = std::env::var_os("PAWL_VOICE_TEST_ARCHIVES").map(PathBuf::from) else {
            eprintln!("skipped: set PAWL_VOICE_TEST_ARCHIVES to check local archives against the pins");
            return;
        };
        for spec in super::super::models::ALL {
            let name = spec.url.rsplit('/').next().unwrap();
            let p = dir.join(name);
            if !p.is_file() {
                eprintln!("not checked: {name}");
                continue;
            }
            let s = store(&format!("pin-{}", spec.dir));
            install_from_file(&s, &spec, &p, &CancelFlag::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(s.installed(spec.id), "{name}");
            let _ = std::fs::remove_dir_all(s.root());
        }
    }
}
