//! Spawning agent subprocesses (stdio piped, whole process tree killed on drop).

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context as _};
use tokio::io::AsyncBufReadExt as _;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// Environment variables never forwarded to agent processes.
const SCRUBBED_ENV: &[&str] = &["PAWL_HUB_TOKEN"];

/// How many trailing stderr lines to keep for error reports.
const STDERR_TAIL_LINES: usize = 20;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A running agent process with its stdio handles split out.
pub(crate) struct Spawned {
    pub child: Child,
    pub tree: ProcessTree,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub stderr_tail: StderrTail,
    pub program: PathBuf,
}

/// Rolling buffer of the agent's last stderr lines (also forwarded to `tracing`).
#[derive(Clone, Default)]
pub(crate) struct StderrTail(Arc<Mutex<VecDeque<String>>>);

impl StderrTail {
    fn push(&self, line: String) {
        let mut q = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if q.len() == STDERR_TAIL_LINES {
            q.pop_front();
        }
        q.push_back(line);
    }

    /// Last `n` non-empty lines joined, capped at `max_chars`.
    pub fn summary(&self, n: usize, max_chars: usize) -> String {
        let q = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let lines: Vec<&str> = q.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        let start = lines.len().saturating_sub(n);
        let joined = lines[start..].join(" | ");
        if joined.chars().count() > max_chars {
            let skip = joined.chars().count() - max_chars;
            format!("…{}", joined.chars().skip(skip).collect::<String>())
        } else {
            joined
        }
    }
}

fn env_lookup<'a>(env: Option<&'a BTreeMap<String, String>>, key: &str) -> Option<&'a String> {
    env?.iter().find(|(k, _)| if cfg!(windows) { k.eq_ignore_ascii_case(key) } else { k.as_str() == key }).map(|(_, v)| v)
}

/// Resolve the agent's program to an executable path.
///
/// Uses the agent's own `PATH` override when given, else the hub's `PATH`.
/// On Windows this honours `PATHEXT`, so `npx` resolves to `npx.cmd`; Rust's
/// `Command` then runs `.cmd`/`.bat` shims through `cmd.exe` with safe argument
/// quoting. Relative paths (`./agent`) resolve against `cwd`.
pub(crate) fn resolve_program(program: &str, env: Option<&BTreeMap<String, String>>, cwd: &Path) -> anyhow::Result<PathBuf> {
    if program.trim().is_empty() {
        bail!("agent command is empty");
    }
    let path_var: Option<OsString> = env_lookup(env, "PATH").map(OsString::from).or_else(|| std::env::var_os("PATH"));
    which::which_in(program, path_var, cwd).with_context(|| format!("could not find agent executable `{program}` on PATH"))
}

/// Spawn `command` (program + args) in `cwd` with `env` layered over the hub's environment.
pub(crate) fn spawn_agent(
    agent_id: &str,
    command: &[String],
    env: Option<&BTreeMap<String, String>>,
    cwd: &Path,
) -> anyhow::Result<Spawned> {
    let Some((program, args)) = command.split_first() else {
        bail!("agent `{agent_id}` has no command configured");
    };
    let resolved = resolve_program(program, env, cwd)?;

    let mut cmd = Command::new(&resolved);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for key in SCRUBBED_ENV {
        cmd.env_remove(key);
    }
    if let Some(env) = env {
        cmd.envs(env);
    }
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    // Own process group, so the agent and everything it starts can be killed together.
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to start agent `{agent_id}` ({})", resolved.display()))?;
    let tree = ProcessTree::attach(agent_id, &child);
    let stdin = child.stdin.take().context("agent stdin was not piped")?;
    let stdout = child.stdout.take().context("agent stdout was not piped")?;
    let stderr = child.stderr.take().context("agent stderr was not piped")?;

    let tail = StderrTail::default();
    {
        let tail = tail.clone();
        let agent_id = agent_id.to_string();
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        tracing::debug!(target: "pawl_acp::agent_stderr", agent = %agent_id, "{line}");
                        tail.push(line);
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::debug!(agent = %agent_id, error = %e, "agent stderr read failed");
                        break;
                    }
                }
            }
        });
    }

    Ok(Spawned { child, tree, stdin, stdout, stderr_tail: tail, program: resolved })
}

/// The agent's whole process tree: a Windows Job Object with
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, or the Unix process group the agent leads.
///
/// `npx`/`.cmd` shims and Node launchers start the real agent as a child, so
/// killing only the direct child would orphan it. [`ProcessTree::kill`] (also run on
/// drop) kills every process in the tree, including ones still running after the
/// agent itself exited.
///
/// Windows: the job is assigned right after `CreateProcess` returns; a grandchild
/// started in the microseconds before that would escape (Rust's `Command` cannot
/// spawn suspended), which a shell shim never does in practice.
pub(crate) struct ProcessTree {
    #[cfg(windows)]
    job: Option<win::Job>,
    #[cfg(unix)]
    pgid: Option<i32>,
}

impl ProcessTree {
    fn attach(agent_id: &str, child: &Child) -> Self {
        #[cfg(windows)]
        {
            let job = match child.raw_handle() {
                Some(handle) => match win::Job::for_process(handle) {
                    Ok(job) => Some(job),
                    Err(e) => {
                        tracing::warn!(agent = %agent_id, error = %e, "could not put agent in a job object; only the direct child will be killed");
                        None
                    }
                },
                None => None,
            };
            ProcessTree { job }
        }
        #[cfg(unix)]
        {
            let _ = agent_id;
            ProcessTree { pgid: child.id().and_then(|id| i32::try_from(id).ok()) }
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = (agent_id, child);
            ProcessTree {}
        }
    }

    /// Kill every process in the tree. Idempotent; errors (already gone) are ignored.
    pub fn kill(&self) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            // SAFETY: plain syscall on a process group we created; ESRCH (already gone) is fine.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(windows)]
mod win {
    use std::io;
    use std::os::windows::io::RawHandle;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// An owned job object handle. Closing it kills every process still in the job.
    pub(super) struct Job(HANDLE);

    // SAFETY: a job handle is a kernel object handle; it may be used and closed from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub(super) fn for_process(process: RawHandle) -> io::Result<Job> {
            // SAFETY: FFI calls with valid arguments; the job handle is owned by `Job`
            // (closed on drop) and `process` is a live handle owned by tokio's `Child`.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let job = Job(handle);
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                if AssignProcessToJobObject(job.0, process as HANDLE) == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(job)
            }
        }

        pub(super) fn terminate(&self) {
            // SAFETY: `self.0` is a valid job handle until drop.
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: we own the handle; KILL_ON_JOB_CLOSE ends any survivors.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_programs_on_path() {
        let cwd = std::env::current_dir().unwrap();
        // `cargo` is on PATH whenever these tests run.
        let p = resolve_program("cargo", None, &cwd).unwrap();
        assert!(p.is_absolute());
        assert!(resolve_program("definitely-not-a-real-agent-binary-xyz", None, &cwd).is_err());
        assert!(resolve_program("", None, &cwd).is_err());
    }

    #[test]
    fn path_override_is_used() {
        let cwd = std::env::current_dir().unwrap();
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), std::env::temp_dir().join("pawl-acp-empty-path").to_string_lossy().into_owned());
        assert!(resolve_program("cargo", Some(&env), &cwd).is_err());
    }

    /// On Windows `npx` is a `.cmd` shim; make sure it resolves and actually runs.
    #[tokio::test]
    async fn spawns_cmd_shims() {
        let cwd = std::env::current_dir().unwrap();
        if resolve_program("npx", None, &cwd).is_err() {
            eprintln!("npx not on PATH; skipping");
            return;
        }
        let mut s = spawn_agent("npx-test", &["npx".into(), "--version".into()], None, &cwd).unwrap();
        drop(s.stdin);
        let mut out = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut s.stdout, &mut out).await.unwrap();
        let status = s.child.wait().await.unwrap();
        assert!(status.success(), "npx --version failed: {status}");
        assert!(out.trim().chars().next().is_some_and(|c| c.is_ascii_digit()), "unexpected output {out:?}");
    }

    #[test]
    fn stderr_tail_summary() {
        let t = StderrTail::default();
        for i in 0..30 {
            t.push(format!("line {i}"));
        }
        assert_eq!(t.summary(2, 100), "line 28 | line 29");
        assert!(t.summary(20, 10).starts_with('…'));
    }
}
