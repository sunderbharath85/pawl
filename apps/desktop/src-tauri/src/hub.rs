//! Hub discovery for the desktop shell: credentials (URL + bearer token from the
//! shared data dir), a tiny `/health` probe, and spawning `pet-hub serve` when the
//! hub is not running.
//!
//! The hub only ever listens on localhost, so `/health` is probed with a minimal
//! HTTP/1.1 client over `std::net::TcpStream` instead of pulling in a full HTTP stack.

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Folder under the OS data dir. Mirrors `HUB_DATA_DIR_NAME` in `@ai-pet/protocol` / `pet-protocol`.
pub const HUB_DATA_DIR_NAME: &str = "ai-pet-hub";
/// Token file inside the data dir. Mirrors `HUB_TOKEN_FILE`.
pub const HUB_TOKEN_FILE: &str = "token";
pub const HUB_DEFAULT_PORT: u16 = 47800;
pub const HUB_HOST: &str = "127.0.0.1";
const SPAWN_LOG_FILE: &str = "desktop-hub.log";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubCredentials {
    /// HTTP base, e.g. `http://127.0.0.1:47800`.
    pub url: String,
    /// WebSocket endpoint, e.g. `ws://127.0.0.1:47800/ws`.
    pub ws_url: String,
    /// Bearer token; empty when the hub never ran (no token file yet).
    pub token: String,
    /// "env" | "file" | "missing".
    pub token_source: &'static str,
    pub data_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubHealth {
    pub ok: bool,
    pub version: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartHubResult {
    /// Path of the binary that was launched (or would have been).
    pub path: Option<String>,
    pub pid: Option<u32>,
    /// The hub already answered `/health`; nothing was spawned.
    pub already_running: bool,
    /// `/health` answered OK within the wait window.
    pub healthy: bool,
    pub version: Option<String>,
}

#[derive(Deserialize)]
struct HealthBody {
    ok: bool,
    version: Option<String>,
}

/// The `pet-hub serve` child we launched (if any). The hub is a background daemon:
/// it keeps running after the desktop app quits, so we never kill it.
#[derive(Default)]
pub struct HubProcess {
    child: Mutex<Option<Child>>,
}

pub fn data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(HUB_DATA_DIR_NAME))
}

/// Hub HTTP base URL: `AI_PET_HUB_URL` (http/ws, with or without `/ws`) or the default.
pub fn hub_http_url() -> String {
    let default = format!("http://{HUB_HOST}:{HUB_DEFAULT_PORT}");
    let Ok(raw) = std::env::var("AI_PET_HUB_URL") else {
        return default;
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return default;
    }
    normalize_http_url(raw)
}

fn normalize_http_url(raw: &str) -> String {
    let mut url = raw.trim().trim_end_matches('/').to_string();
    if let Some(rest) = url.strip_prefix("ws://") {
        url = format!("http://{rest}");
    } else if let Some(rest) = url.strip_prefix("wss://") {
        url = format!("https://{rest}");
    } else if !url.starts_with("http://") && !url.starts_with("https://") {
        url = format!("http://{url}");
    }
    if let Some(stripped) = url.strip_suffix("/ws") {
        url = stripped.to_string();
    }
    url.trim_end_matches('/').to_string()
}

fn ws_url_for(http: &str) -> String {
    let base = if let Some(rest) = http.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = http.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("ws://{http}")
    };
    format!("{base}/ws")
}

pub fn credentials() -> HubCredentials {
    let url = hub_http_url();
    let ws_url = ws_url_for(&url);
    let dir = data_dir();
    let (token, token_source) = match std::env::var("AI_PET_HUB_TOKEN") {
        Ok(t) if !t.trim().is_empty() => (t.trim().to_string(), "env"),
        _ => match dir
            .as_ref()
            .map(|d| d.join(HUB_TOKEN_FILE))
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|s| s.trim().to_string())
        {
            Some(t) if !t.is_empty() => (t, "file"),
            _ => (String::new(), "missing"),
        },
    };
    HubCredentials {
        url,
        ws_url,
        token,
        token_source,
        data_dir: dir.map(|d| d.display().to_string()),
    }
}

struct HostPort {
    host: String,
    port: u16,
}

fn parse_host_port(http_url: &str) -> Result<HostPort, String> {
    let (rest, default_port) = if let Some(r) = http_url.strip_prefix("http://") {
        (r, 80)
    } else if http_url.starts_with("https://") {
        return Err("TLS hub URLs are not supported by the desktop health probe".into());
    } else {
        (http_url, 80)
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.is_empty() {
        return Err(format!("invalid hub URL: {http_url}"));
    }
    // [::1]:port
    if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6
            .split_once(']')
            .ok_or_else(|| format!("invalid hub URL: {http_url}"))?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| format!("invalid port in {http_url}"))?,
            None => default_port,
        };
        return Ok(HostPort { host: host.to_string(), port });
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Ok(HostPort {
            host: host.to_string(),
            port: port.parse().map_err(|_| format!("invalid port in {http_url}"))?,
        }),
        None => Ok(HostPort { host: authority.to_string(), port: default_port }),
    }
}

/// Blocking `GET {url}/health` with a short timeout.
pub fn probe_health(http_url: &str, timeout: Duration) -> HubHealth {
    match probe_health_inner(http_url, timeout) {
        Ok((ok, version)) => HubHealth { ok, version, error: if ok { None } else { Some("hub reported not ok".into()) } },
        Err(e) => HubHealth { ok: false, version: None, error: Some(e) },
    }
}

fn probe_health_inner(http_url: &str, timeout: Duration) -> Result<(bool, Option<String>), String> {
    let hp = parse_host_port(http_url)?;
    let addr = (hp.host.as_str(), hp.port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve {}: {e}", hp.host))?
        .next()
        .ok_or_else(|| format!("no address for {}", hp.host))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("connect {addr}: {e}"))?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    let host_header = if hp.host.contains(':') { format!("[{}]:{}", hp.host, hp.port) } else { format!("{}:{}", hp.host, hp.port) };
    let req = format!(
        "GET /health HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: ai-pet-desktop\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).map_err(|e| format!("write: {e}"))?;

    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 64 * 1024 {
                    return Err("health response too large".into());
                }
                if response_complete(&buf) {
                    break;
                }
            }
            Err(e) if buf.is_empty() => return Err(format!("read: {e}")),
            Err(_) => break,
        }
    }
    let (status, headers, body) = split_response(&buf).ok_or("malformed HTTP response")?;
    if status != 200 {
        return Err(format!("/health returned HTTP {status}"));
    }
    let body = if headers.contains("transfer-encoding: chunked") { decode_chunked(body) } else { body.to_vec() };
    let parsed: HealthBody = serde_json::from_slice(&body).map_err(|e| format!("bad /health body: {e}"))?;
    Ok((parsed.ok, parsed.version))
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn response_complete(buf: &[u8]) -> bool {
    let Some(end) = find_header_end(buf) else { return false };
    let headers = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
    let body_len = buf.len() - (end + 4);
    for line in headers.lines() {
        if let Some(v) = line.strip_prefix("content-length:") {
            if let Ok(n) = v.trim().parse::<usize>() {
                return body_len >= n;
            }
        }
    }
    if headers.contains("transfer-encoding: chunked") {
        return buf.ends_with(b"0\r\n\r\n");
    }
    false
}

fn split_response(buf: &[u8]) -> Option<(u16, String, &[u8])> {
    let end = find_header_end(buf)?;
    let head = String::from_utf8_lossy(&buf[..end]).to_string();
    let status_line = head.lines().next()?;
    let status: u16 = status_line.split_whitespace().nth(1)?.parse().ok()?;
    let mut body = &buf[end + 4..];
    let lower = head.to_ascii_lowercase();
    for line in lower.lines() {
        if let Some(v) = line.strip_prefix("content-length:") {
            if let Ok(n) = v.trim().parse::<usize>() {
                body = &body[..n.min(body.len())];
            }
        }
    }
    Some((status, lower, body))
}

fn decode_chunked(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(line_end) = data.windows(2).position(|w| w == b"\r\n") else { break };
        let size_str = String::from_utf8_lossy(&data[..line_end]);
        let size = usize::from_str_radix(size_str.split(';').next().unwrap_or("").trim(), 16).unwrap_or(0);
        data = &data[line_end + 2..];
        if size == 0 || data.len() < size {
            break;
        }
        out.extend_from_slice(&data[..size]);
        data = &data[(size + 2).min(data.len())..];
    }
    out
}

fn hub_exe_name() -> &'static str {
    if cfg!(windows) {
        "pet-hub.exe"
    } else {
        "pet-hub"
    }
}

fn is_file(p: &Path) -> bool {
    p.metadata().map(|m| m.is_file()).unwrap_or(false)
}

/// Look for `pet-hub` in PATH (honours PATHEXT on Windows).
fn which_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        Vec::new()
    };
    let stem = name.trim_end_matches(".exe");
    for dir in std::env::split_paths(&path) {
        if cfg!(windows) {
            for ext in &exts {
                let candidate = dir.join(format!("{stem}{ext}"));
                if is_file(&candidate) {
                    return Some(candidate);
                }
            }
        } else {
            let candidate = dir.join(name);
            if is_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Where `pet-hub` lives: `AI_PET_HUB_BIN`, next to this executable (bundled installs),
/// on PATH, or (debug builds only) the repo's cargo target dirs.
pub fn find_hub_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AI_PET_HUB_BIN") {
        let p = PathBuf::from(p);
        if is_file(&p) {
            return Some(p);
        }
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        let candidate = dir.join(hub_exe_name());
        if is_file(&candidate) {
            return Some(candidate);
        }
        // macOS bundles: Contents/MacOS/<app> → Contents/Resources/pet-hub
        let resources = dir.join("../Resources").join(hub_exe_name());
        if is_file(&resources) {
            return Some(resources);
        }
    }
    if let Some(p) = which_in_path(hub_exe_name()) {
        return Some(p);
    }
    if cfg!(debug_assertions) {
        // apps/desktop/src-tauri → repo root
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        for profile in ["debug", "release"] {
            let candidate = repo.join("target").join(profile).join(hub_exe_name());
            if is_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn open_spawn_log() -> Option<File> {
    let dir = data_dir()?;
    fs::create_dir_all(&dir).ok()?;
    OpenOptions::new().create(true).write(true).truncate(true).open(dir.join(SPAWN_LOG_FILE)).ok()
}

fn port_override(http_url: &str) -> Option<u16> {
    let hp = parse_host_port(http_url).ok()?;
    (hp.port != HUB_DEFAULT_PORT).then_some(hp.port)
}

/// Blocking: if the hub is healthy return immediately, otherwise spawn `pet-hub serve`
/// detached and wait up to ~10 s for `/health`.
pub fn start_hub_blocking(proc_state: &HubProcess) -> Result<StartHubResult, String> {
    let url = hub_http_url();
    let health = probe_health(&url, Duration::from_millis(800));
    if health.ok {
        return Ok(StartHubResult { path: None, pid: None, already_running: true, healthy: true, version: health.version });
    }

    let mut guard = proc_state.child.lock().map_err(|_| "hub process lock poisoned")?;
    // A previous spawn may still be booting.
    let still_booting = match guard.as_mut() {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => false,
    };

    let (path, pid) = if still_booting {
        let child = guard.as_ref().expect("checked above");
        (None, Some(child.id()))
    } else {
        let bin = find_hub_binary().ok_or_else(|| {
            format!(
                "Could not find `{}`. Install it on PATH, place it next to the desktop app, or set AI_PET_HUB_BIN.",
                hub_exe_name()
            )
        })?;
        let mut cmd = Command::new(&bin);
        cmd.arg("serve");
        if let Some(port) = port_override(&url) {
            cmd.arg("--port").arg(port.to_string());
        }
        cmd.stdin(Stdio::null());
        match open_spawn_log() {
            Some(log) => {
                let err = log.try_clone().map(Stdio::from).unwrap_or_else(|_| Stdio::null());
                cmd.stdout(Stdio::from(log)).stderr(err);
            }
            None => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Own process group: Ctrl+C in a dev terminal must not take the hub down with us.
            cmd.process_group(0);
        }
        let child = cmd.spawn().map_err(|e| format!("failed to launch {}: {e}", bin.display()))?;
        let pid = child.id();
        *guard = Some(child);
        (Some(bin.display().to_string()), Some(pid))
    };

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let h = probe_health(&url, Duration::from_millis(500));
        if h.ok {
            return Ok(StartHubResult { path, pid, already_running: false, healthy: true, version: h.version });
        }
        if let Some(child) = guard.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                *guard = None;
                let log_hint = data_dir()
                    .map(|d| format!(" See {}.", d.join(SPAWN_LOG_FILE).display()))
                    .unwrap_or_default();
                return Err(format!("pet-hub exited early ({status}).{log_hint}"));
            }
        }
        if Instant::now() >= deadline {
            return Ok(StartHubResult { path, pid, already_running: false, healthy: false, version: None });
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_urls() {
        assert_eq!(normalize_http_url("ws://127.0.0.1:47800/ws"), "http://127.0.0.1:47800");
        assert_eq!(normalize_http_url("127.0.0.1:9000/"), "http://127.0.0.1:9000");
        assert_eq!(normalize_http_url("http://localhost:47800"), "http://localhost:47800");
        assert_eq!(ws_url_for("http://127.0.0.1:47800"), "ws://127.0.0.1:47800/ws");
    }

    #[test]
    fn parses_host_port() {
        let hp = parse_host_port("http://127.0.0.1:47800").unwrap();
        assert_eq!((hp.host.as_str(), hp.port), ("127.0.0.1", 47800));
        let hp = parse_host_port("http://[::1]:5000").unwrap();
        assert_eq!((hp.host.as_str(), hp.port), ("::1", 5000));
        assert_eq!(port_override("http://127.0.0.1:47800"), None);
        assert_eq!(port_override("http://127.0.0.1:47801"), Some(47801));
    }

    #[test]
    fn parses_responses() {
        let raw = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 29\r\n\r\n{\"ok\":true,\"version\":\"0.1.0\"}";
        assert!(response_complete(raw));
        let (status, _, body) = split_response(raw).unwrap();
        assert_eq!(status, 200);
        let parsed: HealthBody = serde_json::from_slice(body).unwrap();
        assert!(parsed.ok);
        assert_eq!(parsed.version.as_deref(), Some("0.1.0"));

        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nb\r\n{\"ok\":true}\r\n0\r\n\r\n";
        assert!(response_complete(chunked));
        let (_, headers, body) = split_response(chunked).unwrap();
        assert!(headers.contains("transfer-encoding: chunked"));
        assert_eq!(decode_chunked(body), b"{\"ok\":true}");
    }
}
