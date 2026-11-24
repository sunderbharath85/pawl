//! Machine identity: `machine.toml` in the hub data dir holds `{ id, name }` for this machine.
//!
//! - `id` is 12 characters of `[a-z0-9]` from the OS CSPRNG, generated once. Every global id this hub
//!   issues (`${id}:${local}`) is prefixed with it, and hub.db records it, so it must never change.
//! - `name` is the display name ("Mac mini"). It defaults to the OS host name and can be changed with
//!   `krypto machine --name` or the `update_machine` client message.

use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use krypto_protocol::{is_valid_machine_id, Machine, MachineId, MachineOs, UnixMs};
use serde::{Deserialize, Serialize};

use crate::config::Paths;
use crate::core::AckCode;

pub const MACHINE_FILE: &str = "machine.toml";
/// Longest machine name, in characters (after trimming).
pub const MAX_NAME_CHARS: usize = 40;
/// Name used when the OS host name cannot be determined.
pub const FALLBACK_NAME: &str = "This machine";

const ID_LEN: usize = 12;
const ID_ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
/// Largest multiple of 36 that fits in a byte: bytes below it map uniformly onto the alphabet.
const ID_BYTE_LIMIT: u8 = 252;

const FILE_HEADER: &str = "# Krypto machine identity.\n\
# `id` was generated once for this machine and is the prefix of every id this hub hands out.\n\
# Never change it or copy this file to another machine. `name` may be edited\n\
# (or use `krypto machine --name \"...\"`).\n";

/// Contents of `machine.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineIdentity {
    pub id: MachineId,
    #[serde(default)]
    pub name: String,
}

impl MachineIdentity {
    /// This machine as a protocol `Machine`. `online` = its hub is running.
    pub fn to_machine(&self, online: bool, last_seen: UnixMs) -> Machine {
        Machine {
            id: self.id.clone(),
            name: self.name.clone(),
            os: MachineOs::current(),
            last_seen,
            online,
            hub_version: Some(crate::HUB_VERSION.to_string()),
            user_active_at: None,
        }
    }

    /// This hub's own machine, as reported to its clients right now.
    pub fn local_machine(&self) -> Machine {
        self.to_machine(true, krypto_protocol::now_ms())
    }
}

/// A fresh machine id: 12 characters of `[a-z0-9]` from the OS random source (rejection sampled,
/// so every character is uniform).
pub fn generate_id() -> Result<MachineId> {
    let mut id = String::with_capacity(ID_LEN);
    let mut buf = [0u8; 32];
    while id.len() < ID_LEN {
        getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("OS random source failed: {e}"))?;
        for &b in buf.iter().filter(|&&b| b < ID_BYTE_LIMIT) {
            id.push(ID_ALPHABET[(b % 36) as usize] as char);
            if id.len() == ID_LEN {
                break;
            }
        }
    }
    Ok(id)
}

/// Trimmed name, 1..=40 characters (counted as `char`s), no control characters, and nothing paired
/// devices would refuse (`krypto_peer::device_name_ok`: no bidi controls or zero-width / invisible
/// formatting characters). A rejected name's error carries [`AckCode::Invalid`], so the
/// `update_machine` ack error is exactly `"invalid"`.
pub fn validate_name(name: &str) -> Result<String> {
    let name = name.trim();
    let len = name.chars().count();
    let why = if len == 0 || len > MAX_NAME_CHARS {
        format!("machine name must be 1-{MAX_NAME_CHARS} characters (got {len})")
    } else if name.chars().any(char::is_control) {
        "machine name must not contain control characters".to_string()
    } else if !krypto_peer::device_name_ok(name) {
        "machine name must not contain invisible characters (bidi controls, zero-width or other formatting characters)".to_string()
    } else {
        return Ok(name.to_string());
    };
    Err(anyhow::Error::new(AckCode::Invalid).context(why))
}

/// The OS host name (Windows `COMPUTERNAME`, macOS `scutil --get ComputerName`, Linux
/// `/etc/hostname`), else "This machine".
pub fn default_name() -> String {
    host_name().as_deref().and_then(clean_host_name).unwrap_or_else(|| FALLBACK_NAME.to_string())
}

/// A character a device name may hold anywhere (see `krypto_peer::device_name_ok`).
pub(crate) fn name_char_ok(c: char) -> bool {
    !c.is_control() && krypto_peer::device_name_ok(&format!("x{c}"))
}

/// First line, control and invisible formatting characters dropped, cut to [`MAX_NAME_CHARS`],
/// trimmed; None if empty.
fn clean_host_name(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let cut: String = line.chars().filter(|c| name_char_ok(*c)).take(MAX_NAME_CHARS).collect();
    let cut = cut.trim();
    (!cut.is_empty()).then(|| cut.to_string())
}

#[cfg(windows)]
fn host_name() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

#[cfg(target_os = "macos")]
fn host_name() -> Option<String> {
    let from_scutil = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .filter(|s| !s.trim().is_empty());
    from_scutil.or_else(|| std::env::var("HOSTNAME").ok())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn host_name() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
}

#[cfg(not(any(windows, unix)))]
fn host_name() -> Option<String> {
    None
}

/// Parse `machine.toml`. The id must be valid; a missing or invalid name falls back to the host name.
pub fn parse(text: &str) -> Result<MachineIdentity> {
    let raw: MachineIdentity = toml::from_str(text)?;
    if !is_valid_machine_id(&raw.id) {
        bail!("id {:?} is not 12 characters of [a-z0-9]", raw.id);
    }
    let name = match validate_name(&raw.name) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("machine.toml name {:?} ignored ({e:#}); using the host name", raw.name);
            default_name()
        }
    };
    Ok(MachineIdentity { id: raw.id, name })
}

fn render(m: &MachineIdentity) -> Result<String> {
    Ok(format!("{FILE_HEADER}{}", toml::to_string(m).context("serialize machine.toml")?))
}

/// `machine.toml`, or None if it does not exist. A file that exists but is invalid is an error:
/// silently replacing it would give this machine a new id and orphan everything it stored.
pub fn load(paths: &Paths) -> Result<Option<MachineIdentity>> {
    match std::fs::read_to_string(&paths.machine) {
        Ok(text) => parse(&text)
            .map(Some)
            .with_context(|| format!("{} is invalid; fix it or restore it from a backup", paths.machine.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", paths.machine.display())),
    }
}

/// Load `machine.toml`, creating it on first run. A missing file is recreated with the id hub.db
/// recorded (if any), so losing the file never orphans the stored ids.
pub fn load_or_create(paths: &Paths) -> Result<MachineIdentity> {
    if let Some(m) = load(paths)? {
        return Ok(m);
    }
    paths.ensure()?;
    let id = match crate::store::recorded_machine_id(&paths.db) {
        Some(id) => {
            tracing::warn!("{} was missing; restored machine id {id} from hub.db", paths.machine.display());
            id
        }
        None => generate_id()?,
    };
    let ident = MachineIdentity { id, name: default_name() };
    match write_new(&paths.machine, &render(&ident)?) {
        Ok(()) => Ok(ident),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Another krypto process created it first: use theirs (it may still be writing).
            for _ in 0..40 {
                if let Ok(Some(m)) = load(paths) {
                    return Ok(m);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            load(paths)?.context("machine.toml was created concurrently but is unreadable")
        }
        Err(e) => Err(e).with_context(|| format!("create {}", paths.machine.display())),
    }
}

/// Replace `machine.toml` atomically (temp file + rename).
pub fn save(paths: &Paths, m: &MachineIdentity) -> Result<()> {
    if !is_valid_machine_id(&m.id) {
        bail!("refusing to save invalid machine id {:?}", m.id);
    }
    paths.ensure()?;
    let text = render(m)?;
    let tmp = paths.machine.with_extension(format!("toml.{}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &paths.machine)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::Error::new(e).context(format!("write {}", paths.machine.display()))
    })
}

fn write_new(path: &Path, text: &str) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_valid_and_distinct() {
        let ids: std::collections::HashSet<String> = (0..200).map(|_| generate_id().unwrap()).collect();
        assert_eq!(ids.len(), 200);
        assert!(ids.iter().all(|id| is_valid_machine_id(id)), "{ids:?}");
    }

    #[test]
    fn names_are_validated() {
        assert_eq!(validate_name("  Mac mini ").unwrap(), "Mac mini");
        assert_eq!(validate_name(&"é".repeat(40)).unwrap(), "é".repeat(40));
        for bad in ["", "   ", &"x".repeat(41), &"é".repeat(41), "two\nlines"] {
            let e = validate_name(bad).unwrap_err();
            assert_eq!(AckCode::find(&e), Some(AckCode::Invalid), "{e:#}");
            assert!(e.to_string().starts_with("machine name must"), "{e:#}");
        }
        // Names paired devices would refuse are refused here, with a clear reason.
        for bad in ["Mac\u{200B}mini", "\u{202E}inim caM", "a\u{2066}b\u{2069}", "\u{FEFF}Mac", "Mac\u{2060}"] {
            assert!(!krypto_peer::device_name_ok(bad), "{bad:?}");
            let e = validate_name(bad).unwrap_err();
            assert_eq!(AckCode::find(&e), Some(AckCode::Invalid), "{e:#}");
            assert!(e.to_string().contains("invisible characters"), "{e:#}");
        }
        // Visible Unicode is fine, and whatever passes here passes the peers' rule.
        for good in ["Sam\u{2019}s Mac \u{1F600}", "Büro-PC", "日本のマシン"] {
            assert_eq!(validate_name(good).unwrap(), good);
            assert!(krypto_peer::device_name_ok(good));
        }
    }

    #[test]
    fn host_names_are_cleaned() {
        assert_eq!(clean_host_name("my-box\n").as_deref(), Some("my-box"));
        assert_eq!(clean_host_name("\n  Sam's Mac mini  \nignored").as_deref(), Some("Sam's Mac mini"));
        assert_eq!(clean_host_name(&"a".repeat(60)).map(|s| s.len()), Some(MAX_NAME_CHARS));
        assert_eq!(clean_host_name("  \n "), None);
        assert_eq!(clean_host_name("my\u{200B}box\u{202E}").as_deref(), Some("mybox"), "invisible characters dropped");
        assert_eq!(clean_host_name("\u{200B}\u{FEFF}"), None);
        let d = default_name();
        assert!(validate_name(&d).is_ok(), "{d:?}");
    }

    #[test]
    fn parse_is_strict_about_ids_and_lenient_about_names() {
        let m = parse("id = \"abcdef012345\"\nname = \"Windows PC\"\n").unwrap();
        assert_eq!(m, MachineIdentity { id: "abcdef012345".into(), name: "Windows PC".into() });
        assert!(parse("id = \"ABCDEF012345\"\nname = \"x\"").is_err());
        assert!(parse("id = \"abc:ef012345\"\nname = \"x\"").is_err());
        assert!(parse("name = \"x\"").is_err());
        let m = parse("id = \"abcdef012345\"\nname = \"\"").unwrap();
        assert_eq!(m.name, default_name());
        let m = parse("id = \"abcdef012345\"").unwrap();
        assert_eq!(m.name, default_name());
    }

    #[test]
    fn load_or_create_is_stable_and_save_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path().join("data"));
        assert_eq!(load(&paths).unwrap(), None);
        let first = load_or_create(&paths).unwrap();
        assert!(is_valid_machine_id(&first.id));
        assert_eq!(first.name, default_name());
        assert_eq!(load_or_create(&paths).unwrap(), first);
        let text = std::fs::read_to_string(&paths.machine).unwrap();
        assert!(text.starts_with("# Krypto machine identity."), "{text}");

        let renamed = MachineIdentity { name: "Windows \"PC\" #2".into(), ..first.clone() };
        save(&paths, &renamed).unwrap();
        assert_eq!(load(&paths).unwrap(), Some(renamed.clone()));
        assert_eq!(load_or_create(&paths).unwrap(), renamed);
        let leftovers: Vec<_> = std::fs::read_dir(&paths.data_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());

        std::fs::write(&paths.machine, "id = 7").unwrap();
        assert!(load(&paths).is_err(), "a corrupt file is never silently replaced");
        assert!(load_or_create(&paths).is_err());
    }

    #[test]
    fn missing_file_is_restored_from_hub_db() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path());
        crate::store::Store::open(&paths.db, "zyxwvu987654").unwrap();
        let m = load_or_create(&paths).unwrap();
        assert_eq!(m.id, "zyxwvu987654");
        assert_eq!(load(&paths).unwrap().unwrap().id, "zyxwvu987654");
    }

    #[test]
    fn machine_view() {
        let m = MachineIdentity { id: "abcdef012345".into(), name: "Mac mini".into() }.to_machine(false, 5);
        assert_eq!(m.os, MachineOs::current());
        assert!(!m.online);
        assert_eq!(m.last_seen, 5);
        assert_eq!(m.hub_version.as_deref(), Some(crate::HUB_VERSION));
    }
}
