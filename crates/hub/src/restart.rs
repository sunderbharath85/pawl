//! `restart_hub` (PLAN step 5b): the running `krypto serve` replaces itself with a new process
//! started with the same arguments, so a setting that needs a restart (the port) takes effect
//! from the UI. The replacement is detached (it outlives this process) and waits up to
//! [`RESTART_BIND_RETRY`] for the ports this process lets go of. If the new API port still cannot
//! be bound, it stays on the previous one (a restart never leaves no hub).

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};

/// In the replacement's environment: the previous hub's API port. Taken out of the environment
/// first thing (see [`take_marker`]) so agents and hooks this hub starts never inherit it.
pub const RESTARTED_ENV: &str = "KRYPTO_HUB_RESTARTED";
/// How long a replacement retries binding the API port and the peer listener.
pub const RESTART_BIND_RETRY: Duration = Duration::from_secs(10);

/// This process is a replacement started by `restart_hub`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartMarker {
    /// The API port the previous hub served on (the fallback if the new one is taken).
    pub previous_port: Option<u16>,
}

static MARKER: OnceLock<Option<RestartMarker>> = OnceLock::new();

/// The marker's value as the old hub writes it.
pub fn parse_marker(v: &str) -> Option<RestartMarker> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    Some(RestartMarker { previous_port: v.parse::<u16>().ok().filter(|p| *p != 0) })
}

/// Read and remove [`RESTARTED_ENV`]. Call at the very start of `main`, before any other thread
/// exists (changing the environment is only sound then). Later calls keep the first answer.
pub fn take_marker() {
    let value = std::env::var(RESTARTED_ENV).ok();
    if value.is_some() {
        std::env::remove_var(RESTARTED_ENV);
    }
    let _ = MARKER.set(value.as_deref().and_then(parse_marker));
}

/// This process's restart marker (None: a normal start).
pub fn marker() -> Option<RestartMarker> {
    MARKER.get().copied().flatten()
}

/// How long to retry a bind that finds its port taken at startup: [`RESTART_BIND_RETRY`] in a
/// replacement, else not at all (a second `krypto serve` should fail at once).
pub fn bind_retry() -> Duration {
    if marker().is_some() {
        RESTART_BIND_RETRY
    } else {
        Duration::ZERO
    }
}

/// The replacement's command line: `exe` with `args`, no console, no inherited stdio, in its own
/// process group (so the old terminal's Ctrl+C or its closing does not reach it).
/// `previous_port` = the API port this hub serves on (the replacement's fallback).
pub fn replacement_command(exe: &Path, args: impl IntoIterator<Item = OsString>, previous_port: u16) -> Command {
    let mut cmd = Command::new(exe);
    cmd.args(args).env(RESTARTED_ENV, previous_port.to_string()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

/// Start the replacement: this executable with this process's arguments. Returns its pid.
pub fn spawn_replacement(previous_port: u16) -> Result<u32> {
    let exe = std::env::current_exe().context("locate the krypto executable")?;
    let child = replacement_command(&exe, std::env::args_os().skip(1), previous_port)
        .spawn()
        .with_context(|| format!("start {}", exe.display()))?;
    Ok(child.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_replacement_gets_the_same_arguments_and_the_previous_port() {
        let exe = Path::new("/opt/krypto/krypto");
        let cmd = replacement_command(exe, ["--data-dir", "/d", "--port", "47950", "serve"].map(OsString::from), 47950);
        assert_eq!(cmd.get_program(), exe.as_os_str());
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["--data-dir", "/d", "--port", "47950", "serve"]);
        let marker = cmd.get_envs().find(|(k, _)| *k == RESTARTED_ENV).and_then(|(_, v)| v);
        assert_eq!(marker, Some(std::ffi::OsStr::new("47950")));
    }

    #[test]
    fn markers_parse_to_a_fallback_port() {
        assert_eq!(parse_marker("47950"), Some(RestartMarker { previous_port: Some(47950) }));
        assert_eq!(parse_marker("1x"), Some(RestartMarker { previous_port: None }), "still a replacement, no fallback");
        assert_eq!(parse_marker("0"), Some(RestartMarker { previous_port: None }));
        assert_eq!(parse_marker(" "), None);
        // Without take_marker() (tests, normal starts) nothing retries.
        assert_eq!(bind_retry(), Duration::ZERO);
    }
}
