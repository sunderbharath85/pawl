//! `pet-hub` command line.

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use futures::{SinkExt, StreamExt};
use pet_discovery::hooks::HookTarget;
use pet_protocol::{
    Agent, ClientKind, ClientMsg, EventType, HubEvent, PermissionOptionKind, PermissionOutcome,
    PermissionRequest, PlanPayload, ResolvedBy, ServerMsg, Session, ToolCallPayload, Welcome,
};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::config::{self, HubConfig, HubEndpoint, Paths};
use crate::core::{self as hubcore, Actor, Hub, HubOptions};
use crate::{logging, server, HUB_VERSION};

#[derive(Parser, Debug)]
#[command(name = "pet-hub", version, about = "AI Pet Hub: dog pets that watch and drive your coding agents over ACP")]
pub struct Cli {
    /// Hub data directory (default: <OS data dir>/ai-pet-hub)
    #[arg(long, global = true, env = config::ENV_DATA_DIR)]
    pub data_dir: Option<PathBuf>,
    /// Hub port (default: AI_PET_HUB_URL, then config.toml, then 47800)
    #[arg(long, global = true)]
    pub port: Option<u16>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run the hub daemon (listens on 127.0.0.1 only)
    Serve,
    /// List ACP agents (from the running hub, else discovered locally)
    Agents {
        /// Re-fetch the ACP registry instead of using the 24h cache
        #[arg(long)]
        refresh: bool,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// List sessions (open and recent)
    Sessions {
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// Start an ACP session and stream its events (via the running hub, else in-process)
    Start {
        /// Agent id, e.g. claude-code or codex (see `pet-hub agents`)
        agent: String,
        /// Working directory for the agent
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        /// First prompt; without it you get an interactive prompt loop
        #[arg(long)]
        prompt: Option<String>,
        /// Run the agent in this process even if a hub is running
        #[arg(long)]
        local: bool,
    },
    /// Forward an agent hook payload (stdin JSON, or JSON as the last argument) to the hub
    Hook {
        /// claude | codex | copilot
        source: String,
        /// Codex `notify` passes the JSON payload as the last argument
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
        args: Vec<String>,
    },
    /// Install the hub's hook into an agent's config (dry run unless --apply)
    InstallHooks {
        target: HookTargetArg,
        /// Write the change (a backup is made first)
        #[arg(long)]
        apply: bool,
        /// Repository for copilot-vscode (default: current directory)
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Print the hub bearer token (created if missing)
    Token,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookTargetArg {
    Claude,
    Codex,
    CopilotCli,
    CopilotVscode,
}

pub async fn run() -> ExitCode {
    let cli = Cli::parse();
    let paths = match Paths::resolve(cli.data_dir.clone()) {
        Ok(p) => p,
        Err(e) => {
            if matches!(cli.command, Command::Hook { .. }) {
                return ExitCode::SUCCESS;
            }
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let result = match cli.command {
        // Hooks must never block or fail the calling agent.
        Command::Hook { source, args } => {
            run_hook(&paths, cli.port, &source, args).await;
            return ExitCode::SUCCESS;
        }
        Command::Serve => run_serve(paths, cli.port).await,
        Command::Agents { refresh, json } => run_agents(&paths, cli.port, refresh, json).await,
        Command::Sessions { json } => run_sessions(&paths, cli.port, json).await,
        Command::Start { agent, cwd, prompt, local } => run_start(paths, cli.port, agent, cwd, prompt, local).await,
        Command::InstallHooks { target, apply, repo } => run_install_hooks(target, apply, repo),
        Command::Token => run_token(&paths),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------- serve

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let term = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut s) => {
                    s.recv().await;
                }
                Err(_) => std::future::pending::<()>().await,
            }
        };
        tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    }
    #[cfg(not(unix))]
    ctrl_c.await;
    tracing::info!("shutting down");
}

async fn run_serve(paths: Paths, port_flag: Option<u16>) -> Result<()> {
    paths.ensure()?;
    let _log = logging::init(Some(&paths.log), "info", "info");
    let cfg = HubConfig::load_or_init(&paths)?;
    let token = config::load_or_create_token(&paths)?;
    let port = config::serve_port(port_flag, &cfg);
    tracing::info!("data dir {}", paths.data_dir.display());
    let hub = Hub::start(HubOptions { paths, config: cfg, token, background: true }).await?;
    let result = server::serve(hub.clone(), port, shutdown_signal()).await;
    hub.shutdown().await;
    result
}

// ---------------------------------------------------------------- token / install-hooks

fn run_token(paths: &Paths) -> Result<()> {
    println!("{}", config::load_or_create_token(paths)?);
    Ok(())
}

fn run_install_hooks(target: HookTargetArg, apply: bool, repo: Option<PathBuf>) -> Result<()> {
    let target = match target {
        HookTargetArg::Claude => HookTarget::Claude,
        HookTargetArg::Codex => HookTarget::Codex,
        HookTargetArg::CopilotCli => HookTarget::CopilotCli,
        HookTargetArg::CopilotVscode => {
            let repo = match repo {
                Some(r) => r,
                None => std::env::current_dir()?,
            };
            HookTarget::CopilotVscode { repo: std::path::absolute(&repo)? }
        }
    };
    let exe = std::env::current_exe().context("locate the pet-hub executable")?;
    let out = pet_discovery::hooks::install(target, &exe, apply)?;
    println!("{out}");
    if !apply {
        eprintln!("(dry run: nothing written; re-run with --apply)");
    }
    Ok(())
}

// ---------------------------------------------------------------- hook

/// `pet_discovery::hooks::run_hook_cli` reads argv/stdin JSON, prints the stdout reply some agents
/// require (even when the hub is down or no token exists yet), and forwards with a 2 s cap.
async fn run_hook(paths: &Paths, port_flag: Option<u16>, source: &str, args: Vec<String>) {
    let Some(source) = pet_discovery::hooks::parse_hook_source(source) else { return };
    let token = config::read_token(paths).unwrap_or_default();
    let cfg = HubConfig::load(paths).unwrap_or_default();
    let ep = HubEndpoint::resolve(port_flag, cfg.port);
    pet_discovery::hooks::run_hook_cli(source, &args, &ep.http_base, &token).await;
}

// ---------------------------------------------------------------- hub connection

type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A connection to the running hub over WebSocket.
pub struct RemoteHub {
    ws: WsStream,
    buffered: VecDeque<ServerMsg>,
    pub welcome: Welcome,
    pub url: String,
}

impl RemoteHub {
    /// None if no hub answers (or we have no token yet). Err if a hub answered but refused us.
    pub async fn connect(paths: &Paths, port_flag: Option<u16>) -> Result<Option<Self>> {
        let cfg = HubConfig::load(paths).unwrap_or_default();
        let ep = HubEndpoint::resolve(port_flag, cfg.port);
        let Some(token) = config::read_token(paths) else { return Ok(None) };
        let ws = match tokio::time::timeout(Duration::from_millis(1500), tokio_tungstenite::connect_async(ep.ws_url.as_str())).await {
            Ok(Ok((ws, _))) => ws,
            _ => return Ok(None),
        };
        let mut hub = RemoteHub { ws, buffered: VecDeque::new(), welcome: empty_welcome(), url: ep.ws_url.clone() };
        let hello = ClientMsg::Hello {
            id: "hello".into(),
            client: ClientKind::Cli,
            client_version: HUB_VERSION.into(),
            token,
        };
        let result = tokio::time::timeout(Duration::from_secs(5), hub.request_raw(hello))
            .await
            .map_err(|_| anyhow!("hub at {} did not answer hello", ep.ws_url))?
            .map_err(|e| anyhow!("hub at {} refused the connection: {e:#} (token mismatch? check AI_PET_HUB_TOKEN)", ep.ws_url))?;
        hub.welcome = serde_json::from_value(result).context("parse welcome")?;
        Ok(Some(hub))
    }

    async fn send(&mut self, msg: &ClientMsg) -> Result<()> {
        let text = serde_json::to_string(msg)?;
        self.ws.send(WsMessage::Text(text.into())).await.context("send to hub")
    }

    /// Next server message straight off the socket (None = closed). Cancel-safe.
    async fn recv_socket(&mut self) -> Result<Option<ServerMsg>> {
        loop {
            let Some(frame) = self.ws.next().await else { return Ok(None) };
            match frame.context("hub connection")? {
                WsMessage::Text(t) => match serde_json::from_str::<ServerMsg>(t.as_str()) {
                    Ok(m) => return Ok(Some(m)),
                    Err(e) => tracing::debug!("ignoring unparseable hub frame: {e}"),
                },
                WsMessage::Close(_) => return Ok(None),
                _ => {}
            }
        }
    }

    async fn request_raw(&mut self, msg: ClientMsg) -> Result<Value> {
        let id = msg.id().to_string();
        self.send(&msg).await?;
        loop {
            match self.recv_socket().await? {
                Some(ServerMsg::Ack { id: aid, ok, result, error }) if aid == id => {
                    return if ok {
                        Ok(result.unwrap_or(Value::Null))
                    } else {
                        Err(anyhow!(error.unwrap_or_else(|| "request failed".into())))
                    };
                }
                Some(other) => self.buffered.push_back(other),
                None => bail!("hub closed the connection"),
            }
        }
    }
}

fn empty_welcome() -> Welcome {
    Welcome {
        protocol_version: pet_protocol::PROTOCOL_VERSION,
        hub_version: String::new(),
        agents: vec![],
        sessions: vec![],
        pets: vec![],
        pending_permissions: vec![],
        rules: vec![],
    }
}

/// Same request/stream interface over a running hub or an in-process one.
pub enum Conn {
    Remote(Box<RemoteHub>),
    Local { hub: Hub, rx: broadcast::Receiver<ServerMsg>, actor: Actor },
}

impl Conn {
    pub fn local(hub: Hub) -> Self {
        let rx = hub.subscribe();
        Conn::Local { hub, rx, actor: Actor::client(ClientKind::Cli, 0) }
    }

    pub async fn request(&mut self, msg: ClientMsg) -> Result<Value> {
        match self {
            Conn::Remote(r) => r.request_raw(msg).await,
            Conn::Local { hub, actor, .. } => match hub.handle(msg, actor).await {
                ServerMsg::Ack { ok: true, result, .. } => Ok(result.unwrap_or(Value::Null)),
                ServerMsg::Ack { error, .. } => Err(anyhow!(error.unwrap_or_else(|| "request failed".into()))),
                other => Err(anyhow!("unexpected reply {other:?}")),
            },
        }
    }

    /// Next broadcast message (None = connection closed). Cancel-safe.
    pub async fn next(&mut self) -> Result<Option<ServerMsg>> {
        match self {
            Conn::Remote(r) => {
                if let Some(m) = r.buffered.pop_front() {
                    return Ok(Some(m));
                }
                r.recv_socket().await
            }
            Conn::Local { rx, .. } => loop {
                match rx.recv().await {
                    Ok(m) => return Ok(Some(m)),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("[hub] (skipped {n} events: output too slow)");
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(None),
                }
            },
        }
    }
}

fn rid() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ---------------------------------------------------------------- agents / sessions

async fn run_agents(paths: &Paths, port_flag: Option<u16>, refresh: bool, as_json: bool) -> Result<()> {
    let agents: Vec<Agent> = match RemoteHub::connect(paths, port_flag).await? {
        Some(hub) => {
            let mut conn = Conn::Remote(Box::new(hub));
            let v = conn.request(ClientMsg::ListAgents { id: rid(), refresh: Some(refresh) }).await?;
            serde_json::from_value(v.get("agents").cloned().unwrap_or_default())?
        }
        None => {
            let cfg = HubConfig::load(paths)?;
            paths.ensure()?;
            hubcore::discover_agents(paths, &cfg, refresh).await?
        }
    };
    if as_json {
        println!("{}", serde_json::to_string_pretty(&agents)?);
        return Ok(());
    }
    if agents.is_empty() {
        println!("No agents found.");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = agents
        .iter()
        .map(|a| {
            vec![
                a.id.clone(),
                a.name.clone(),
                if a.installed { "yes".into() } else { "no".into() },
                a.version.clone().unwrap_or_default(),
                a.command.join(" "),
            ]
        })
        .collect();
    print_table(&["ID", "NAME", "INSTALLED", "VERSION", "COMMAND"], &rows);
    Ok(())
}

async fn run_sessions(paths: &Paths, port_flag: Option<u16>, as_json: bool) -> Result<()> {
    let sessions: Vec<Session> = match RemoteHub::connect(paths, port_flag).await? {
        Some(hub) => {
            let mut conn = Conn::Remote(Box::new(hub));
            let v = conn.request(ClientMsg::ListSessions { id: rid(), include_ended: Some(true) }).await?;
            serde_json::from_value(v.get("sessions").cloned().unwrap_or_default())?
        }
        None => {
            if !paths.db.exists() {
                Vec::new()
            } else {
                let store = crate::store::Store::open(&paths.db)?;
                store.run(|s| s.recent_sessions(50)).await?
            }
        }
    };
    if as_json {
        println!("{}", serde_json::to_string_pretty(&sessions)?);
        return Ok(());
    }
    if sessions.is_empty() {
        println!("No sessions yet.");
        return Ok(());
    }
    let now = pet_protocol::now_ms();
    let rows: Vec<Vec<String>> = sessions
        .iter()
        .map(|s| {
            vec![
                short_id(&s.id),
                s.agent_id.clone(),
                enum_label(&s.source),
                if s.ended_at.is_some() { format!("{} (ended)", enum_label(&s.status)) } else { enum_label(&s.status) },
                ago(now, s.last_event_at),
                crate::notify::truncate(&crate::notify::session_label(s), 40),
                s.cwd.clone(),
            ]
        })
        .collect();
    print_table(&["ID", "AGENT", "SOURCE", "STATUS", "LAST", "TITLE", "CWD"], &rows);
    Ok(())
}

fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let line = |cells: Vec<String>| {
        let mut out = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                out.push_str(c);
            } else {
                out.push_str(c);
                out.push_str(&" ".repeat(widths[i] - c.chars().count() + 2));
            }
        }
        println!("{}", out.trim_end());
    };
    line(headers.iter().map(|h| h.to_string()).collect());
    for r in rows {
        line(r.clone());
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn enum_label<T: serde::Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(Value::String(s)) => s,
        _ => "?".into(),
    }
}

fn ago(now: i64, ts: i64) -> String {
    let s = ((now - ts).max(0) / 1000) as u64;
    match s {
        0..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86_399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

// ---------------------------------------------------------------- start

/// Lines from stdin on a dedicated thread (blocking reads never stall the runtime).
struct StdinLines {
    rx: mpsc::UnboundedReceiver<String>,
}

impl StdinLines {
    fn spawn() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            let mut line = String::new();
            loop {
                line.clear();
                match stdin.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(line.trim_end_matches(['\r', '\n']).to_string()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Self { rx }
    }

    async fn next(&mut self) -> Option<String> {
        self.rx.recv().await
    }
}

async fn run_start(
    paths: Paths,
    port_flag: Option<u16>,
    agent: String,
    cwd: PathBuf,
    prompt: Option<String>,
    force_local: bool,
) -> Result<()> {
    let cwd = std::path::absolute(&cwd).with_context(|| format!("bad --cwd {}", cwd.display()))?;
    let remote = if force_local { None } else { RemoteHub::connect(&paths, port_flag).await? };
    let mut local_hub = None;
    let _log;
    let mut conn = match remote {
        Some(r) => {
            eprintln!("[hub] connected to {}", r.url);
            Conn::Remote(Box::new(r))
        }
        None => {
            paths.ensure()?;
            _log = logging::init(Some(&paths.log), "warn", "info");
            eprintln!("[hub] no hub running; driving the agent in-process (events are still logged to hub.db)");
            let cfg = HubConfig::load(&paths)?;
            let token = config::load_or_create_token(&paths)?;
            let hub = Hub::start(HubOptions { paths: paths.clone(), config: cfg, token, background: false }).await?;
            local_hub = Some(hub.clone());
            Conn::local(hub)
        }
    };

    let result = drive_session(&mut conn, &agent, &cwd.to_string_lossy(), prompt).await;
    if let Some(hub) = local_hub {
        hub.shutdown().await;
    }
    result
}

async fn drive_session(conn: &mut Conn, agent: &str, cwd: &str, prompt: Option<String>) -> Result<()> {
    eprintln!("[hub] starting {agent} in {cwd} ...");
    let v = conn
        .request(ClientMsg::StartSession { id: rid(), agent_id: agent.into(), cwd: cwd.into(), prompt: prompt.clone() })
        .await?;
    let session: Session = serde_json::from_value(v.get("session").cloned().unwrap_or_default()).context("parse session")?;
    eprintln!("[hub] session {} ({})", session.id, enum_label(&session.status));
    let mut stdin = StdinLines::spawn();
    let mut printer = Printer::new(agent);

    if prompt.is_some() {
        return run_turn(conn, &session.id, &mut stdin, &mut printer).await.map(|_| ());
    }
    eprintln!("[hub] type a prompt and press Enter; /quit (or Ctrl-C / end of input) to leave");
    loop {
        eprint!("> ");
        let _ = std::io::stderr().flush();
        let line = tokio::select! {
            l = stdin.next() => l,
            _ = tokio::signal::ctrl_c() => None,
        };
        let Some(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "/quit" || line == "/exit" {
            break;
        }
        if let Err(e) = conn.request(ClientMsg::Prompt { id: rid(), session_id: session.id.clone(), text: line.into() }).await {
            eprintln!("[hub] {e:#}");
            continue;
        }
        if run_turn(conn, &session.id, &mut stdin, &mut printer).await? == TurnEnd::SessionOver {
            break;
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum TurnEnd {
    Stopped,
    SessionOver,
}

/// Stream one turn; answers permission prompts from stdin; Ctrl-C cancels (twice quits).
async fn run_turn(conn: &mut Conn, sid: &str, stdin: &mut StdinLines, printer: &mut Printer) -> Result<TurnEnd> {
    let mut asking: Option<PermissionRequest> = None;
    let mut cancel_sent = false;
    loop {
        tokio::select! {
            msg = conn.next() => {
                let Some(msg) = msg? else { bail!("hub connection closed") };
                match msg {
                    ServerMsg::Event { event } if event.session_id == sid => {
                        printer.event(&event);
                        if matches!(event.kind, EventType::Stop | EventType::Error | EventType::SessionEnd) {
                            return Ok(if event.kind == EventType::SessionEnd { TurnEnd::SessionOver } else { TurnEnd::Stopped });
                        }
                    }
                    ServerMsg::PermissionRequest { request } if request.session_id == sid => {
                        printer.ask(&request);
                        asking = Some(request);
                    }
                    ServerMsg::PermissionResolved { request_id, .. }
                        if asking.as_ref().is_some_and(|r| r.request_id == request_id) => {
                            asking = None;
                        }
                    ServerMsg::Session { session } if session.id == sid && session.ended_at.is_some() => {
                        printer.finish_line();
                        eprintln!("[hub] session ended ({})", enum_label(&session.status));
                        return Ok(TurnEnd::SessionOver);
                    }
                    _ => {}
                }
            }
            line = stdin.next(), if asking.is_some() => {
                let Some(req) = asking.clone() else { continue };
                let answer = match line {
                    Some(l) => parse_answer(&req, &l),
                    None => Some(deny_option(&req)), // input closed: refuse rather than hang
                };
                match answer {
                    Some(option_id) => {
                        match conn.request(ClientMsg::ResolvePermission { id: rid(), request_id: req.request_id.clone(), option_id }).await {
                            Ok(_) => asking = None,
                            Err(e) => eprintln!("[hub] {e:#}"),
                        }
                    }
                    None => eprintln!("[hub] answer with an option number, y(es) or n(o)"),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                if cancel_sent {
                    bail!("interrupted");
                }
                cancel_sent = true;
                printer.finish_line();
                eprintln!("[hub] cancelling the turn (Ctrl-C again to quit)");
                if let Err(e) = conn.request(ClientMsg::Cancel { id: rid(), session_id: sid.into() }).await {
                    eprintln!("[hub] cancel failed: {e:#}");
                }
            }
        }
    }
}

fn first_of(req: &PermissionRequest, kinds: &[PermissionOptionKind]) -> Option<String> {
    kinds.iter().find_map(|k| req.options.iter().find(|o| o.kind == *k).map(|o| o.option_id.clone()))
}

fn deny_option(req: &PermissionRequest) -> Option<String> {
    first_of(req, &[PermissionOptionKind::RejectOnce, PermissionOptionKind::RejectAlways])
}

/// `Some(answer)` for a valid reply (inner None = cancelled), `None` = unrecognized.
fn parse_answer(req: &PermissionRequest, line: &str) -> Option<Option<String>> {
    let l = line.trim().to_ascii_lowercase();
    if let Ok(n) = l.parse::<usize>() {
        return req.options.get(n.checked_sub(1)?).map(|o| Some(o.option_id.clone()));
    }
    match l.as_str() {
        "y" | "yes" | "a" | "allow" => {
            first_of(req, &[PermissionOptionKind::AllowOnce, PermissionOptionKind::AllowAlways]).map(Some)
        }
        "n" | "no" | "d" | "deny" | "r" | "reject" => Some(deny_option(req)),
        "c" | "cancel" => Some(None),
        _ => None,
    }
}

/// Human-readable event stream on stdout.
struct Printer {
    agent: String,
    streaming: Option<EventType>,
}

impl Printer {
    fn new(agent: &str) -> Self {
        Self { agent: agent.to_string(), streaming: None }
    }

    fn finish_line(&mut self) {
        if self.streaming.take().is_some() {
            println!();
        }
    }

    fn stream(&mut self, kind: EventType, prefix: &str, text: &str) {
        if self.streaming != Some(kind) {
            self.finish_line();
            print!("{prefix}");
            self.streaming = Some(kind);
        }
        print!("{text}");
        let _ = std::io::stdout().flush();
    }

    fn event(&mut self, e: &HubEvent) {
        let text = e.payload.get("text").and_then(Value::as_str).unwrap_or_default();
        match e.kind {
            EventType::AgentMessageChunk => {
                let prefix = format!("{}: ", self.agent);
                self.stream(e.kind, &prefix, text)
            }
            EventType::AgentThoughtChunk => self.stream(e.kind, "(thinking) ", text),
            EventType::UserMessage => {
                self.finish_line();
                println!("you: {text}");
            }
            EventType::ToolCall => {
                self.finish_line();
                let tc: ToolCallPayload = serde_json::from_value(e.payload.clone()).unwrap_or_default();
                let kind = tc.kind.map(|k| enum_label(&k)).unwrap_or_else(|| "tool".into());
                let title = tc.title.unwrap_or_else(|| tc.tool_call_id.clone());
                let paths: Vec<String> = tc.locations.unwrap_or_default().into_iter().map(|l| l.path).collect();
                if paths.is_empty() {
                    println!("[{kind}] {title}");
                } else {
                    println!("[{kind}] {title}  ({})", paths.join(", "));
                }
            }
            EventType::ToolCallUpdate => {
                let tc: ToolCallPayload = serde_json::from_value(e.payload.clone()).unwrap_or_default();
                if let Some(status) = tc.status {
                    let label = enum_label(&status);
                    if label == "completed" || label == "failed" {
                        self.finish_line();
                        println!("  -> {label}: {}", tc.title.unwrap_or(tc.tool_call_id));
                    }
                }
            }
            EventType::Plan => {
                self.finish_line();
                if let Ok(plan) = serde_json::from_value::<PlanPayload>(e.payload.clone()) {
                    println!("[plan]");
                    for entry in plan.entries {
                        let mark = match enum_label(&entry.status).as_str() {
                            "completed" => "x",
                            "in_progress" => ">",
                            _ => " ",
                        };
                        println!("  [{mark}] {}", entry.content);
                    }
                }
            }
            EventType::PermissionResolved => {
                self.finish_line();
                let by = e.payload.get("by").and_then(|b| serde_json::from_value::<ResolvedBy>(b.clone()).ok());
                let outcome = e.payload.get("outcome").and_then(|o| serde_json::from_value::<PermissionOutcome>(o.clone()).ok());
                let what = match outcome {
                    Some(PermissionOutcome::Selected { option_id }) => format!("answered {option_id}"),
                    _ => "cancelled".into(),
                };
                let who = match by {
                    Some(ResolvedBy::Rule) => "by rule",
                    Some(ResolvedBy::System) => "automatically",
                    _ => "by you",
                };
                let title = e.payload.pointer("/toolCall/title").and_then(Value::as_str).unwrap_or("permission");
                println!("[permission] {title}: {what} {who}");
            }
            EventType::Stop => {
                self.finish_line();
                let reason = e.payload.get("stopReason").and_then(Value::as_str).unwrap_or("stopped");
                println!("[turn ended: {reason}]");
            }
            EventType::Error => {
                self.finish_line();
                let msg = e.payload.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| e.payload.to_string());
                println!("[error] {msg}");
            }
            EventType::SessionStart => {
                self.finish_line();
                println!("[session started]");
            }
            EventType::SessionEnd => {
                self.finish_line();
                println!("[session ended]");
            }
            EventType::RequestPermission | EventType::Notification | EventType::Hook => {}
        }
    }

    fn ask(&mut self, req: &PermissionRequest) {
        self.finish_line();
        let title = req.tool_call.title.as_deref().unwrap_or("a tool call");
        let kind = req.tool_call.kind.map(|k| enum_label(&k)).unwrap_or_else(|| "tool".into());
        println!("[permission] {} wants to run [{kind}] {title}", self.agent);
        for loc in req.tool_call.locations.iter().flatten() {
            println!("    {}", loc.path);
        }
        for (i, o) in req.options.iter().enumerate() {
            println!("  {}) {} [{}]", i + 1, o.name, enum_label(&o.kind));
        }
        print!("choose (number, y, n): ");
        let _ = std::io::stdout().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pet_protocol::PermissionOption;

    fn req() -> PermissionRequest {
        PermissionRequest {
            request_id: "r".into(),
            session_id: "s".into(),
            agent_id: "codex".into(),
            tool_call: ToolCallPayload { tool_call_id: "t".into(), ..Default::default() },
            options: vec![
                PermissionOption { option_id: "ok".into(), name: "Allow".into(), kind: PermissionOptionKind::AllowOnce },
                PermissionOption { option_id: "always".into(), name: "Always".into(), kind: PermissionOptionKind::AllowAlways },
                PermissionOption { option_id: "no".into(), name: "Reject".into(), kind: PermissionOptionKind::RejectOnce },
            ],
            created_at: 0,
        }
    }

    #[test]
    fn answers() {
        let r = req();
        assert_eq!(parse_answer(&r, "2"), Some(Some("always".into())));
        assert_eq!(parse_answer(&r, "y"), Some(Some("ok".into())));
        assert_eq!(parse_answer(&r, "No"), Some(Some("no".into())));
        assert_eq!(parse_answer(&r, "cancel"), Some(None));
        assert_eq!(parse_answer(&r, "9"), None);
        assert_eq!(parse_answer(&r, "0"), None);
        assert_eq!(parse_answer(&r, "maybe"), None);
    }

    #[test]
    fn cli_parses() {
        let c = Cli::try_parse_from(["pet-hub", "start", "codex", "--cwd", ".", "--prompt", "run tests"]).unwrap();
        assert!(matches!(c.command, Command::Start { ref agent, .. } if agent == "codex"));
        let c = Cli::try_parse_from(["pet-hub", "hook", "codex", r#"{"type":"agent-turn-complete"}"#]).unwrap();
        assert!(matches!(c.command, Command::Hook { ref source, ref args } if source == "codex" && args.len() == 1));
        let c = Cli::try_parse_from(["pet-hub", "install-hooks", "copilot-vscode", "--apply"]).unwrap();
        assert!(matches!(c.command, Command::InstallHooks { target: HookTargetArg::CopilotVscode, apply: true, .. }));
        let c = Cli::try_parse_from(["pet-hub", "--port", "48000", "serve"]).unwrap();
        assert_eq!(c.port, Some(48000));
    }

    #[test]
    fn ago_formats() {
        assert_eq!(ago(10_000, 0), "10s ago");
        assert_eq!(ago(3_600_000, 0), "1h ago");
    }
}
