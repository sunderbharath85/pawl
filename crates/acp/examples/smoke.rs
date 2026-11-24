//! Smoke test for pet-acp against a real ACP agent.
//!
//! Starts the given agent command in a working directory, sends one prompt,
//! prints every `HostEvent` as it arrives and cancels all permission requests
//! (so the agent cannot change anything). Exits when the turn ends.
//!
//! ```text
//! cargo run -p pet-acp --example smoke -- [--cwd DIR] [--prompt TEXT] [--timeout SECS] -- <command> [args...]
//! cargo run -p pet-acp --example smoke -- --cwd . --prompt "Say hi" -- npx -y @agentclientprotocol/claude-agent-acp
//! ```
//!
//! Set `RUST_LOG=pet_acp=debug` to see the agent's stderr.

use std::path::PathBuf;
use std::time::Duration;

use pet_acp::{AcpHost, HostEvent};
use pet_protocol::{Agent, AgentSource, EventType};

struct Args {
    cwd: PathBuf,
    prompt: String,
    timeout: Duration,
    command: Vec<String>,
}

fn usage() -> ! {
    eprintln!("usage: smoke [--cwd DIR] [--prompt TEXT] [--timeout SECS] -- <command> [args...]");
    std::process::exit(2);
}

fn parse_args() -> Args {
    let mut cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut prompt = "Reply with one short sentence saying hello. Do not use any tools.".to_string();
    let mut timeout = Duration::from_secs(300);
    let mut command = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--cwd" => cwd = it.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--prompt" => prompt = it.next().unwrap_or_else(|| usage()),
            "--timeout" => {
                let secs: u64 = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage());
                timeout = Duration::from_secs(secs);
            }
            "--" => {
                command.extend(it.by_ref());
                break;
            }
            "-h" | "--help" => usage(),
            other if command.is_empty() && !other.starts_with("--") => {
                command.push(other.to_string());
                command.extend(it.by_ref());
                break;
            }
            _ => usage(),
        }
    }
    if command.is_empty() {
        usage();
    }
    Args { cwd, prompt, timeout, command }
}

fn describe(ev: &HostEvent) -> String {
    match ev {
        HostEvent::Session(s) => format!("session  {} status={:?} title={:?} ended={:?}", s.id, s.status, s.title, s.ended_at),
        HostEvent::Event { session_id, kind, payload } => format!("event    {session_id} {} {payload}", kind.as_str()),
        HostEvent::PermissionRequest(r) => format!(
            "perm     {} tool={:?} kind={:?} options={:?}",
            r.request_id,
            r.tool_call.title,
            r.tool_call.kind,
            r.options.iter().map(|o| &o.option_id).collect::<Vec<_>>()
        ),
        HostEvent::AgentCapabilities { agent_id, capabilities } => format!("caps     {agent_id} {capabilities:?}"),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "pet_acp=info".into()))
        .with_writer(std::io::stderr)
        .init();
    let args = parse_args();
    let agent = Agent {
        id: "smoke".into(),
        name: "smoke".into(),
        command: args.command.clone(),
        env: None,
        version: None,
        description: None,
        icon: None,
        installed: true,
        source: AgentSource::Custom,
        capabilities: None,
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let host = AcpHost::new(tx);
    let hub_session_id = uuid::Uuid::new_v4().to_string();
    eprintln!("starting {:?} in {}", args.command, args.cwd.display());
    let session = host.start_session(&agent, args.cwd.clone(), hub_session_id.clone(), Some(args.prompt.clone())).await?;
    eprintln!("session {} (acp {:?}) status {:?}", session.id, session.acp_session_id, session.status);

    let deadline = tokio::time::sleep(args.timeout);
    tokio::pin!(deadline);
    let mut code = 0;
    loop {
        tokio::select! {
            ev = rx.recv() => {
                let Some(ev) = ev else { break };
                println!("{}", describe(&ev));
                match &ev {
                    HostEvent::PermissionRequest(r) => {
                        // Never grant anything from a smoke test.
                        host.resolve_permission(&r.request_id, None).await?;
                        println!("perm     {} -> cancelled", r.request_id);
                    }
                    HostEvent::Event { kind: EventType::Stop, .. } => break,
                    HostEvent::Session(s) if s.ended_at.is_some() => { code = 1; break }
                    _ => {}
                }
            }
            () = &mut deadline => {
                eprintln!("timed out after {}s; cancelling", args.timeout.as_secs());
                let _ = host.cancel(&hub_session_id).await;
                code = 1;
                break;
            }
        }
    }
    host.shutdown().await;
    // Drain the final upserts produced by shutdown.
    while let Ok(ev) = rx.try_recv() {
        println!("{}", describe(&ev));
    }
    std::process::exit(code);
}
