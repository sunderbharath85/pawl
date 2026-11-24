//! `krypto` command line.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use futures::{SinkExt, StreamExt};
use krypto_discovery::hooks::HookTarget;
use krypto_protocol::{
    Agent, ClientKind, ClientMsg, Device, DeviceKind, EventType, HubEvent, Machine, MachineOs, PeerStatus,
    PermissionOptionKind, PermissionOutcome, PermissionRequest, PlanPayload, ResolvedBy, ServerMsg, Session,
    ToolCallPayload, Welcome,
};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::config::{self, HubConfig, HubEndpoint, Paths};
use crate::core::{self as hubcore, Actor, Hub, HubOptions};
use crate::machine::{self, MachineIdentity};
use crate::{logging, server, HUB_VERSION};

#[derive(Parser, Debug)]
#[command(name = "krypto", version, about = "Krypto: dog pets that watch and drive your coding agents over ACP")]
pub struct Cli {
    /// Hub data directory (default: <OS data dir>/krypto)
    #[arg(long, global = true, env = config::ENV_DATA_DIR)]
    pub data_dir: Option<PathBuf>,
    /// Hub port (default: KRYPTO_HUB_URL, then config.toml, then 47800)
    #[arg(long, global = true)]
    pub port: Option<u16>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run the hub daemon (the local API listens on 127.0.0.1 only)
    Serve {
        /// Also accept hub-to-hub links (pairing + presence) on this address, e.g. 192.168.1.20 or
        /// 192.168.1.20:47801 (default port 47801). Overrides config.toml [peer] listen.
        #[arg(long, value_name = "IP[:PORT]")]
        peer_listen: Option<String>,
    },
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
        /// Agent id, e.g. claude-code or codex (see `krypto agents`)
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
        /// Run it on a paired machine (name or id; see `krypto machines`); --cwd is then a path there
        #[arg(long, value_name = "NAME|ID")]
        machine: Option<String>,
        /// Accepted for scripts, but a start on another machine always asks on this terminal
        #[arg(long)]
        yes: bool,
    },
    /// Approve a waiting permission request, on this machine or a paired one (see `krypto pending`)
    Approve {
        /// Request id (or a unique start of it, 4+ characters)
        request: String,
        /// Option id to choose (default: the first allow option)
        #[arg(long)]
        option: Option<String>,
        /// Do not ask for this machine's requests (another machine's always ask, on a terminal)
        #[arg(long)]
        yes: bool,
    },
    /// Deny a waiting permission request, on this machine or a paired one
    Deny {
        /// Request id (or a unique start of it, 4+ characters)
        request: String,
        /// Option id to choose (default: the first reject option, else cancel)
        #[arg(long)]
        option: Option<String>,
    },
    /// List permission requests waiting for an answer, on every machine
    Pending {
        /// Print JSON
        #[arg(long)]
        json: bool,
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
    /// Show this machine's id and name, or rename it with --name
    Machine {
        /// New display name, e.g. "Windows PC" (1-40 characters)
        #[arg(long)]
        name: Option<String>,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// Pair with other machines' hubs (needs a running hub)
    Peer {
        #[command(subcommand)]
        action: PeerAction,
    },
    /// List this machine and every paired machine (online state, last seen, address)
    Machines {
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum PeerAction {
    /// Create a one-time invite (prints the URI and the `krypto peer join` line for the other machine)
    Invite {
        /// Seconds the invite stays valid (60-3600, default 600)
        #[arg(long, value_name = "SECS")]
        ttl: Option<u32>,
        /// Address the other hub dials, "host:port" (default: the listener's; required if it binds 0.0.0.0)
        #[arg(long, value_name = "HOST:PORT")]
        addr: Option<String>,
    },
    /// Pair with the hub that created this invite URI
    Join {
        /// krypto://peer?... (quote it: it contains '&')
        uri: String,
    },
    /// Unpair a machine: it can no longer connect, and it is told
    Revoke {
        /// Machine id or name (see `krypto machines`)
        machine: String,
    },
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
        Command::Serve { peer_listen } => run_serve(paths, cli.port, peer_listen).await,
        Command::Agents { refresh, json } => run_agents(&paths, cli.port, refresh, json).await,
        Command::Sessions { json } => run_sessions(&paths, cli.port, json).await,
        Command::Start { agent, cwd, prompt, local, machine, yes } => {
            run_start(paths, cli.port, agent, cwd, prompt, local, machine, yes).await
        }
        Command::Approve { request, option, yes } => run_decide(&paths, cli.port, true, request, option, yes).await,
        Command::Deny { request, option } => run_decide(&paths, cli.port, false, request, option, false).await,
        Command::Pending { json } => run_pending(&paths, cli.port, json).await,
        Command::InstallHooks { target, apply, repo } => run_install_hooks(target, apply, repo),
        Command::Token => run_token(&paths),
        Command::Machine { name, json } => run_machine(&paths, cli.port, name, json).await,
        Command::Peer { action } => run_peer(&paths, cli.port, action).await,
        Command::Machines { json } => run_machines(&paths, cli.port, json).await,
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

async fn run_serve(paths: Paths, port_flag: Option<u16>, peer_listen: Option<String>) -> Result<()> {
    if let Some(l) = &peer_listen {
        config::parse_peer_listen(l).context("--peer-listen")?;
    }
    paths.ensure()?;
    let _log = logging::init(Some(&paths.log), "info", "info");
    let mut cfg = HubConfig::load_or_init(&paths)?;
    if peer_listen.is_some() {
        cfg.peer.listen = peer_listen;
    }
    let token = config::load_or_create_token(&paths)?;
    let machine = machine::load_or_create(&paths)?;
    let port = config::serve_port(port_flag, &cfg);
    tracing::info!("data dir {}", paths.data_dir.display());
    tracing::info!("machine {} ({:?})", machine.id, machine.name);
    let hub = Hub::start(HubOptions { paths, config: cfg, token, machine, background: true }).await?;
    let result = server::serve(hub.clone(), port, shutdown_signal()).await;
    hub.shutdown().await;
    result
}

// ---------------------------------------------------------------- token / install-hooks

fn run_token(paths: &Paths) -> Result<()> {
    println!("{}", config::load_or_create_token(paths)?);
    Ok(())
}

// ---------------------------------------------------------------- machine

/// `krypto machine [--name ...]`: through the running hub when it serves this data dir (it saves
/// machine.toml and broadcasts `machine` to its clients), else straight on machine.toml.
async fn run_machine(paths: &Paths, port_flag: Option<u16>, name: Option<String>, as_json: bool) -> Result<()> {
    // Validate up front: the hub only answers a bad name with the bare code "invalid"; the CLI
    // prints the reason instead.
    let name = name.map(|n| machine::validate_name(&n)).transpose().map_err(|e| anyhow!("{e}"))?;
    let local = machine::load(paths)?;
    let remote = match RemoteHub::connect(paths, port_flag).await? {
        Some(hub) if local.as_ref().is_none_or(|l| l.id == hub.welcome.machine.id) => Some(hub),
        Some(hub) => {
            eprintln!(
                "[hub] the hub at {} is machine {}, not this data dir's {}; using machine.toml directly",
                hub.url,
                hub.welcome.machine.id,
                local.as_ref().map_or("?", |l| l.id.as_str())
            );
            None
        }
        None => None,
    };
    let renamed = name.is_some();
    let (machine, hub_running) = match (remote, name) {
        (Some(hub), Some(name)) => {
            let machine_id = hub.welcome.machine.id.clone();
            let mut conn = Conn::Remote(Box::new(hub));
            let v = conn.request(ClientMsg::UpdateMachine { id: rid(), machine_id, name }).await?;
            let m: Machine = serde_json::from_value(v.get("machine").cloned().unwrap_or_default()).context("parse machine")?;
            (m, true)
        }
        (Some(hub), None) => (hub.welcome.machine.clone(), true),
        (None, name) => {
            let mut ident: MachineIdentity = match local {
                Some(l) => l,
                None => machine::load_or_create(paths)?,
            };
            if let Some(name) = name {
                ident.name = name;
                machine::save(paths, &ident)?;
            }
            (ident.to_machine(false, krypto_protocol::now_ms()), false)
        }
    };
    if renamed {
        let how = if hub_running { "the running hub saved it and told its clients" } else { "saved to machine.toml" };
        eprintln!("renamed this machine to {:?} ({how})", machine.name);
    }
    if as_json {
        println!("{}", serde_json::to_string_pretty(&machine)?);
        return Ok(());
    }
    println!("id:    {}", machine.id);
    println!("name:  {}", machine.name);
    println!("os:    {}", enum_label(&machine.os));
    match (hub_running, machine.hub_version.as_deref()) {
        (true, Some(v)) => println!("hub:   running (v{v})"),
        (true, None) => println!("hub:   running"),
        (false, _) => println!("hub:   not running"),
    }
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
    let exe = std::env::current_exe().context("locate the krypto executable")?;
    let out = krypto_discovery::hooks::install(target, &exe, apply)?;
    println!("{out}");
    if !apply {
        eprintln!("(dry run: nothing written; re-run with --apply)");
    }
    Ok(())
}

// ---------------------------------------------------------------- hook

/// `krypto_discovery::hooks::run_hook_cli` reads argv/stdin JSON, prints the stdout reply some agents
/// require (even when the hub is down or no token exists yet), and forwards with a 2 s cap.
async fn run_hook(paths: &Paths, port_flag: Option<u16>, source: &str, args: Vec<String>) {
    let Some(source) = krypto_discovery::hooks::parse_hook_source(source) else { return };
    let token = config::read_token(paths).unwrap_or_default();
    let cfg = HubConfig::load(paths).unwrap_or_default();
    let ep = HubEndpoint::resolve(port_flag, cfg.port);
    krypto_discovery::hooks::run_hook_cli(source, &args, &ep.http_base, &token).await;
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
            .map_err(|e| anyhow!("hub at {} refused the connection: {e:#} (token mismatch? check KRYPTO_HUB_TOKEN)", ep.ws_url))?;
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

/// Placeholder until the hub's `hello` ack arrives.
fn empty_welcome() -> Welcome {
    let machine =
        Machine { id: String::new(), name: String::new(), os: MachineOs::current(), last_seen: 0, online: false, hub_version: None, user_active_at: None };
    Welcome {
        protocol_version: krypto_protocol::PROTOCOL_VERSION,
        hub_version: String::new(),
        machine,
        machines: vec![],
        devices: vec![],
        peer: PeerStatus { listening: None, public_key: String::new() },
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

// ---------------------------------------------------------------- peer / machines

/// What pairing gives the other machine, said once by `peer invite` and `peer join`.
const PAIRED_CAN: &str = "It can see this machine's sessions and, with confirmation on its side, approve once, prompt, cancel and start agents in folders this machine already uses. Turn off with [peer] accept_intents = false.";

/// Friendly text for the pairing ack error codes (the hub answers with the bare code).
fn peer_error_text(code: &str) -> Option<&'static str> {
    Some(match code {
        "peer_listener_off" => {
            "this hub's peer listener is off, so it cannot invite. Restart it with \
             `krypto serve --peer-listen <LAN-IP>` or set [peer] listen = \"<LAN-IP>:47801\" in config.toml"
        }
        "invalid" => "invalid invite options: --ttl must be 60-3600 seconds and --addr a reachable host:port \
                      (required when the listener binds 0.0.0.0 or ::)",
        "bad_uri" => "that is not a valid Krypto invite (expected krypto://peer?...)",
        "self_invite" => "that invite was created by this machine; run `krypto peer join` on the other machine",
        "unreachable" => "could not reach the inviting hub at the invite's address (same network? firewall?)",
        "invite_rejected" => "the inviting hub rejected the invite: it is wrong, expired or already used; create a new one",
        "timeout" => "the inviting hub did not answer in time",
        "not_found" => "no such paired machine",
        _ => return None,
    })
}

/// A request on the running hub, with pairing error codes turned into readable text.
async fn peer_request(conn: &mut Conn, msg: ClientMsg) -> Result<Value> {
    conn.request(msg).await.map_err(|e| {
        let code = e.to_string();
        match peer_error_text(&code) {
            Some(text) => anyhow!("{text} ({code})"),
            None => e,
        }
    })
}

/// invite/join/revoke act on the running hub's link layer.
async fn running_hub(paths: &Paths, port_flag: Option<u16>) -> Result<RemoteHub> {
    match RemoteHub::connect(paths, port_flag).await? {
        Some(hub) => Ok(hub),
        None => bail!("no hub is running for this data dir; start it first (`krypto serve`, add --peer-listen <LAN-IP> to invite)"),
    }
}

/// Peer-supplied names are shown as-is but never as terminal control sequences.
fn printable(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

fn local_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "?".into())
}

async fn run_peer(paths: &Paths, port_flag: Option<u16>, action: PeerAction) -> Result<()> {
    let hub = running_hub(paths, port_flag).await?;
    let me = hub.welcome.machine.clone();
    let devices = hub.welcome.devices.clone();
    let mut conn = Conn::Remote(Box::new(hub));
    match action {
        PeerAction::Invite { ttl, addr } => {
            let v = peer_request(&mut conn, ClientMsg::CreateInvite { id: rid(), ttl_secs: ttl, addr }).await?;
            let uri = v.get("uri").and_then(Value::as_str).context("hub sent no invite uri")?;
            let expires_at = v.get("expiresAt").and_then(Value::as_i64).unwrap_or(0);
            let mins = ((expires_at - krypto_protocol::now_ms()).max(0) + 59_999) / 60_000;
            println!(
                "Invite to pair with {} (single use, expires at {} - in {mins} min):",
                printable(&me.name),
                local_time(expires_at)
            );
            println!();
            println!("  {uri}");
            println!();
            println!("On the other machine, run:");
            println!();
            println!("  krypto peer join \"{uri}\"");
            println!();
            println!("{PAIRED_CAN}");
        }
        PeerAction::Join { uri } => {
            let v = peer_request(&mut conn, ClientMsg::JoinPeer { id: rid(), uri }).await?;
            let device: Device = serde_json::from_value(v.get("device").cloned().unwrap_or_default()).context("parse device")?;
            let machine: Machine =
                serde_json::from_value(v.get("machine").cloned().unwrap_or_default()).context("parse machine")?;
            println!(
                "Paired with {} ({}, {}){}",
                printable(&machine.name),
                machine.id,
                enum_label(&machine.os),
                device.addr.as_deref().map(|a| format!(" at {a}")).unwrap_or_default()
            );
            println!("{PAIRED_CAN}");
        }
        PeerAction::Revoke { machine } => {
            let device = resolve_device(&devices, &machine)?;
            peer_request(&mut conn, ClientMsg::RevokeDevice { id: rid(), device_id: device.id.clone() }).await?;
            println!(
                "Revoked {} ({}): it can no longer connect. Pair again with a new invite.",
                printable(&device.name),
                device.id
            );
        }
    }
    Ok(())
}

/// A paired device by exact id, else by name (trimmed, case-insensitive; must be unambiguous).
fn resolve_device<'a>(devices: &'a [Device], query: &str) -> Result<&'a Device> {
    let q = query.trim();
    if let Some(d) = devices.iter().find(|d| d.id == q) {
        return Ok(d);
    }
    let by_name: Vec<&Device> = devices.iter().filter(|d| d.name.trim().to_lowercase() == q.to_lowercase()).collect();
    match by_name.as_slice() {
        [d] => Ok(d),
        [] => bail!("no paired machine with id or name {q:?} (see `krypto machines`)"),
        many => bail!(
            "{} paired machines are named {q:?}; use the id: {}",
            many.len(),
            many.iter().map(|d| d.id.as_str()).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `krypto machines`: from the running hub (live online state), else from hub.db (peers offline).
async fn run_machines(paths: &Paths, port_flag: Option<u16>, as_json: bool) -> Result<()> {
    let (machines, devices, listening, hub_running) = match RemoteHub::connect(paths, port_flag).await? {
        Some(hub) => {
            let w = hub.welcome;
            (w.machines, w.devices, w.peer.listening, true)
        }
        None => {
            paths.ensure()?;
            let me = machine::load_or_create(paths)?;
            let roster = if paths.db.exists() {
                let store = crate::store::Store::open(&paths.db, &me.id)?;
                store.run(|s| s.roster()).await?
            } else {
                Vec::new()
            };
            let mut machines = vec![me.to_machine(false, krypto_protocol::now_ms())];
            let mut devices = Vec::new();
            for e in roster {
                let mut m = e.machine;
                m.online = false;
                if e.device.kind == DeviceKind::Hub {
                    machines.push(m);
                }
                devices.push(e.device);
            }
            (machines, devices, None, false)
        }
    };
    if as_json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "machines": machines, "devices": devices }))?);
        return Ok(());
    }
    let now = krypto_protocol::now_ms();
    let rows: Vec<Vec<String>> = machines
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let this = i == 0;
            let status = match (this, m.online) {
                (true, _) if hub_running => "this machine".to_string(),
                (true, _) => "this machine (hub not running)".to_string(),
                (false, true) => "online".to_string(),
                (false, false) => "offline".to_string(),
            };
            let seen = match (this, m.online) {
                (true, _) if !hub_running => "-".to_string(),
                (true, _) | (false, true) => "now".to_string(),
                (false, false) => ago(now, m.last_seen),
            };
            let addr = if this {
                listening.clone()
            } else {
                devices.iter().find(|d| d.id == m.id).and_then(|d| d.addr.clone())
            };
            vec![
                printable(&m.name),
                m.id.clone(),
                enum_label(&m.os),
                status,
                seen,
                addr.unwrap_or_else(|| "-".into()),
            ]
        })
        .collect();
    print_table(&["NAME", "ID", "OS", "STATUS", "LAST SEEN", "PEER ADDR"], &rows);
    if machines.len() == 1 {
        println!();
        println!("No paired machines. Pair one with `krypto peer invite` (needs `krypto serve --peer-listen <LAN-IP>`).");
    }
    Ok(())
}

// ---------------------------------------------------------------- approve / deny / pending (intents)

/// Readable text for the codes an action on another machine (or a decided request) can return.
fn intent_error_text(code: &str, machine: &str) -> Option<String> {
    Some(match code {
        "already_decided" => "Already answered.".to_string(),
        "offline" => format!("{machine} is offline."),
        "timeout" => format!("No answer from {machine} yet; check that machine (the result may still arrive)."),
        "unconfirmed" => "This needs a confirmation on this device first.".to_string(),
        "forbidden" => format!(
            "{machine} does not allow that from another machine: \"always allow\" is chosen on {machine} itself, agents start only in folders it already uses (or under its [peer] start_roots), and nothing is accepted when its [peer] accept_intents = false."
        ),
        "rate_limited" => format!("Too many requests to {machine}; try again in a moment."),
        "invalid" => "That request, option, agent or directory is not valid there.".to_string(),
        "not_found" => "No such request or session (or that machine is not paired).".to_string(),
        "not_owner" => "That id does not belong to the machine it was sent to.".to_string(),
        _ => return None,
    })
}

/// A request on the running hub; intent error codes become readable text (`machine` = the target).
async fn intent_request(conn: &mut Conn, msg: ClientMsg, machine: &str) -> Result<Value> {
    conn.request(msg).await.map_err(|e| {
        let code = e.to_string();
        match intent_error_text(&code, machine) {
            Some(text) => anyhow!("{text} ({code})"),
            None => e,
        }
    })
}

/// A machine by exact id, else by name (trimmed, case-insensitive; must be unambiguous).
fn resolve_machine<'a>(machines: &'a [Machine], query: &str) -> Result<&'a Machine> {
    let q = query.trim();
    if let Some(m) = machines.iter().find(|m| m.id == q) {
        return Ok(m);
    }
    let by_name: Vec<&Machine> = machines.iter().filter(|m| m.name.trim().to_lowercase() == q.to_lowercase()).collect();
    match by_name.as_slice() {
        [m] => Ok(m),
        [] => bail!("no machine with id or name {q:?} (see `krypto machines`)"),
        many => bail!(
            "{} machines are named {q:?}; use the id: {}",
            many.len(),
            many.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// A pending request by full id, or by a unique prefix (4+ characters) of its local part.
fn resolve_request<'a>(pending: &'a [PermissionRequest], query: &str) -> Result<Option<&'a PermissionRequest>> {
    let q = query.trim();
    if let Some(r) = pending.iter().find(|r| r.request_id == q) {
        return Ok(Some(r));
    }
    if krypto_protocol::parse_global_id(q).is_some() {
        return Ok(None); // a full id that is not pending: let its owner say why
    }
    if q.chars().count() < 4 {
        bail!("request id {q:?} is too short (use at least 4 characters, see `krypto pending`)");
    }
    let hits: Vec<&PermissionRequest> = pending
        .iter()
        .filter(|r| krypto_protocol::parse_global_id(&r.request_id).is_some_and(|(_, local)| local.starts_with(q)))
        .collect();
    match hits.as_slice() {
        [r] => Ok(Some(r)),
        [] => bail!("no pending request {q:?} (see `krypto pending`)"),
        _ => bail!("{q:?} matches {} pending requests; give more of the id", hits.len()),
    }
}

/// The option an approve/deny uses: `--option` if given (it must be of the right kind), else the
/// first allow (once, then always) / reject (once, then always; none = cancel).
fn pick_option(req: &PermissionRequest, approve: bool, explicit: Option<&str>) -> Result<Option<String>> {
    if let Some(o) = explicit {
        let Some(opt) = req.options.iter().find(|x| x.option_id == o) else {
            bail!("request has no option {o:?} (options: {})", req.options.iter().map(|x| x.option_id.as_str()).collect::<Vec<_>>().join(", "));
        };
        let allow = matches!(opt.kind, PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways);
        if allow != approve {
            bail!("option {o:?} is {} option; use `krypto {}`", if allow { "an allow" } else { "a reject" }, if allow { "approve" } else { "deny" });
        }
        return Ok(Some(o.to_string()));
    }
    Ok(if approve {
        Some(
            first_of(req, &[PermissionOptionKind::AllowOnce, PermissionOptionKind::AllowAlways])
                .context("the request has no allow option")?,
        )
    } else {
        deny_option(req)
    })
}

/// Whether the CLI asks before acting. On another machine always: a person at this terminal,
/// never `--yes` or a script. On this machine only for actions that ask at all, unless `--yes`.
fn asks_first(remote: bool, yes: bool, asks_locally: bool) -> bool {
    remote || (asks_locally && !yes)
}

/// Ask a yes/no question on the terminal (default no). Needs a TTY.
async fn ask_yes_no(question: String) -> Result<bool> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        bail!("this needs a confirmation on an interactive terminal");
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let line = tokio::task::spawn_blocking(|| {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s).map(|_| s)
    })
    .await
    .context("read the answer")??;
    Ok(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

fn machine_name(machines: &[Machine], id: &str) -> String {
    machines.iter().find(|m| m.id == id).map(|m| printable(&m.name)).unwrap_or_else(|| id.to_string())
}

/// `krypto approve|deny <requestId>`: this machine's requests, or another machine's (forwarded to
/// its owner). A remote approval always asks on this terminal; `--yes` is not honoured for it, so a
/// script cannot approve on another machine.
async fn run_decide(
    paths: &Paths,
    port_flag: Option<u16>,
    approve: bool,
    request: String,
    option: Option<String>,
    yes: bool,
) -> Result<()> {
    let hub = running_hub(paths, port_flag).await?;
    let me = hub.welcome.machine.id.clone();
    let machines = hub.welcome.machines.clone();
    let pending = hub.welcome.pending_permissions.clone();
    let req = resolve_request(&pending, &request)?.cloned();
    let request_id = req.as_ref().map_or(request.trim().to_string(), |r| r.request_id.clone());
    let owner = krypto_protocol::parse_global_id(&request_id).map(|(m, _)| m.to_string()).unwrap_or_else(|| me.clone());
    let remote = owner != me;
    let where_ = machine_name(&machines, &owner);
    let option_id = match &req {
        Some(r) => pick_option(r, approve, option.as_deref())?,
        None if approve => Some(option.context("the request is not pending here; pass --option <id>")?),
        None => option,
    };
    let what = req.as_ref().and_then(|r| r.tool_call.title.clone()).unwrap_or_else(|| "this request".into());
    let mut confirmed = None;
    if approve {
        let question = if remote {
            format!("Approve on {where_}: {}?", printable(&what))
        } else {
            format!("Approve: {}?", printable(&what))
        };
        // Remote approvals are always confirmed by a person here; local ones may skip with --yes.
        if asks_first(remote, yes, true) && !ask_yes_no(question).await? {
            eprintln!("Not approved.");
            return Ok(());
        }
        confirmed = Some(true);
    }
    let mut conn = Conn::Remote(Box::new(hub));
    intent_request(&mut conn, ClientMsg::ResolvePermission { id: rid(), request_id: request_id.clone(), option_id, confirmed }, &where_)
        .await?;
    let verb = if approve { "Approved" } else { "Denied" };
    if remote {
        println!("{verb} on {where_}: {}", printable(&what));
    } else {
        println!("{verb}: {}", printable(&what));
    }
    Ok(())
}

/// `krypto pending`: requests waiting for an answer, on every machine.
async fn run_pending(paths: &Paths, port_flag: Option<u16>, as_json: bool) -> Result<()> {
    let hub = running_hub(paths, port_flag).await?;
    let w = hub.welcome;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&w.pending_permissions)?);
        return Ok(());
    }
    if w.pending_permissions.is_empty() {
        println!("Nothing is waiting for an answer.");
        return Ok(());
    }
    let now = krypto_protocol::now_ms();
    let rows: Vec<Vec<String>> = w
        .pending_permissions
        .iter()
        .map(|r| {
            vec![
                short_id(&r.request_id),
                machine_name(&w.machines, &r.machine_id),
                r.agent_id.clone(),
                printable(&crate::notify::truncate(r.tool_call.title.as_deref().unwrap_or("a tool call"), 50)),
                r.options.iter().map(|o| o.option_id.as_str()).collect::<Vec<_>>().join(","),
                ago(now, r.created_at),
            ]
        })
        .collect();
    print_table(&["ID", "MACHINE", "AGENT", "TOOL", "OPTIONS", "WAITING"], &rows);
    println!();
    println!("Answer with `krypto approve <ID>` or `krypto deny <ID>`.");
    Ok(())
}

// ---------------------------------------------------------------- agents / sessions

async fn run_agents(paths: &Paths, port_flag: Option<u16>, refresh: bool, as_json: bool) -> Result<()> {
    let agents: Vec<Agent> = match RemoteHub::connect(paths, port_flag).await? {
        Some(hub) => {
            let mut conn = Conn::Remote(Box::new(hub));
            let v = conn.request(ClientMsg::ListAgents { id: rid(), refresh: Some(refresh), machine_id: None }).await?;
            serde_json::from_value(v.get("agents").cloned().unwrap_or_default())?
        }
        None => {
            let cfg = HubConfig::load(paths)?;
            paths.ensure()?;
            let machine = machine::load_or_create(paths)?;
            hubcore::discover_agents(paths, &cfg, &machine.id, refresh).await?
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

/// `krypto sessions`: every machine's sessions (this one's plus the mirror of paired hubs).
async fn run_sessions(paths: &Paths, port_flag: Option<u16>, as_json: bool) -> Result<()> {
    let (sessions, names): (Vec<Session>, HashMap<String, String>) = match RemoteHub::connect(paths, port_flag).await? {
        Some(hub) => {
            let names = hub.welcome.machines.iter().map(|m| (m.id.clone(), m.name.clone())).collect();
            let mut conn = Conn::Remote(Box::new(hub));
            let v = conn.request(ClientMsg::ListSessions { id: rid(), include_ended: Some(true) }).await?;
            (serde_json::from_value(v.get("sessions").cloned().unwrap_or_default())?, names)
        }
        None => {
            if !paths.db.exists() {
                (Vec::new(), HashMap::new())
            } else {
                let machine = machine::load_or_create(paths)?;
                let store = crate::store::Store::open(&paths.db, &machine.id)?;
                let (sessions, roster) = store.run(|s| Ok((s.recent_sessions(50)?, s.roster()?))).await?;
                let mut names: HashMap<String, String> =
                    roster.into_iter().map(|e| (e.device.id, e.device.name)).collect();
                names.insert(machine.id.clone(), machine.name.clone());
                (sessions, names)
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
    let now = krypto_protocol::now_ms();
    let rows: Vec<Vec<String>> = sessions
        .iter()
        .map(|s| {
            vec![
                short_id(&s.id),
                printable(names.get(&s.machine_id).map_or(s.machine_id.as_str(), String::as_str)),
                s.agent_id.clone(),
                enum_label(&s.source),
                if s.ended_at.is_some() { format!("{} (ended)", enum_label(&s.status)) } else { enum_label(&s.status) },
                ago(now, s.last_event_at),
                crate::notify::truncate(&crate::notify::session_label(s), 40),
                s.cwd.clone(),
            ]
        })
        .collect();
    print_table(&["ID", "MACHINE", "AGENT", "SOURCE", "STATUS", "LAST", "TITLE", "CWD"], &rows);
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

/// First 8 characters of the local part of a global id (`machine:local`), else of the id itself.
fn short_id(id: &str) -> String {
    let local = krypto_protocol::parse_global_id(id).map_or(id, |(_, l)| l);
    local.chars().take(8).collect()
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

#[allow(clippy::too_many_arguments)]
async fn run_start(
    paths: Paths,
    port_flag: Option<u16>,
    agent: String,
    cwd: PathBuf,
    prompt: Option<String>,
    force_local: bool,
    machine: Option<String>,
    yes: bool,
) -> Result<()> {
    if let Some(query) = machine {
        if force_local {
            bail!("--machine and --local cannot be combined");
        }
        let hub = running_hub(&paths, port_flag).await?;
        let target = resolve_machine(&hub.welcome.machines, &query)?.clone();
        if target.id != hub.welcome.machine.id {
            // A path on that machine: passed as given (it checks it is an absolute, existing directory).
            let dir = cwd.to_string_lossy().into_owned();
            if dir.trim().is_empty() || dir == "." {
                bail!("--cwd <absolute path on {}> is required with --machine", printable(&target.name));
            }
            if !target.online {
                bail!("{} is offline (see `krypto machines`)", printable(&target.name));
            }
            // Like a remote approval: always a person at this terminal, never --yes or a script.
            if yes {
                eprintln!("(--yes is not honoured for another machine: confirm below)");
            }
            let question = format!("Start {agent} on {} in {dir}?", printable(&target.name));
            if asks_first(true, yes, false) && !ask_yes_no(question).await? {
                eprintln!("Not started.");
                return Ok(());
            }
            let mut conn = Conn::Remote(Box::new(hub));
            return drive_session(&mut conn, &agent, &dir, prompt, Some(&target)).await;
        }
    }
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
            let machine = machine::load_or_create(&paths)?;
            let hub = Hub::start(HubOptions { paths: paths.clone(), config: cfg, token, machine, background: false }).await?;
            local_hub = Some(hub.clone());
            Conn::local(hub)
        }
    };

    let result = drive_session(&mut conn, &agent, &cwd.to_string_lossy(), prompt, None).await;
    if let Some(hub) = local_hub {
        hub.shutdown().await;
    }
    result
}

/// Start a session (here, or on `remote` after the user confirmed) and drive it from the terminal.
async fn drive_session(conn: &mut Conn, agent: &str, cwd: &str, prompt: Option<String>, remote: Option<&Machine>) -> Result<()> {
    let on = remote.map(|m| format!(" on {}", printable(&m.name))).unwrap_or_default();
    eprintln!("[hub] starting {agent} in {cwd}{on} ...");
    let msg = ClientMsg::StartSession {
        id: rid(),
        agent_id: agent.into(),
        cwd: cwd.into(),
        prompt: prompt.clone(),
        machine_id: remote.map(|m| m.id.clone()),
        confirmed: remote.map(|_| true),
    };
    let target = remote.map_or_else(|| "this machine".to_string(), |m| printable(&m.name));
    let v = intent_request(conn, msg, &target).await?;
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
                        // The person answered on this terminal: that is the confirmation a remote approval needs.
                        let msg = ClientMsg::ResolvePermission {
                            id: rid(),
                            request_id: req.request_id.clone(),
                            option_id,
                            confirmed: Some(true),
                        };
                        match conn.request(msg).await {
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
    /// Text of the `you:` line just printed; an identical user_message right after it (an agent
    /// replaying the prompt as user_message_chunk) is not printed again.
    last_user: Option<String>,
}

impl Printer {
    fn new(agent: &str) -> Self {
        Self { agent: agent.to_string(), streaming: None, last_user: None }
    }

    /// The `you:` line for a user_message, or None if it repeats the line just printed.
    fn user_line(&mut self, text: &str) -> Option<String> {
        let t = text.trim();
        if self.last_user.as_deref() == Some(t) {
            return None;
        }
        self.last_user = Some(t.to_string());
        Some(format!("you: {text}"))
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
        if e.kind != EventType::UserMessage && !matches!(e.kind, EventType::RequestPermission | EventType::Notification | EventType::Hook) {
            self.last_user = None;
        }
        match e.kind {
            EventType::AgentMessageChunk => {
                let prefix = format!("{}: ", self.agent);
                self.stream(e.kind, &prefix, text)
            }
            EventType::AgentThoughtChunk => self.stream(e.kind, "(thinking) ", text),
            EventType::UserMessage => {
                if let Some(line) = self.user_line(text) {
                    self.finish_line();
                    println!("{line}");
                }
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
    use krypto_protocol::PermissionOption;

    fn req() -> PermissionRequest {
        PermissionRequest {
            request_id: "abcdef012345:r".into(),
            machine_id: "abcdef012345".into(),
            session_id: "abcdef012345:s".into(),
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
        let c = Cli::try_parse_from(["krypto", "start", "codex", "--cwd", ".", "--prompt", "run tests"]).unwrap();
        assert!(matches!(c.command, Command::Start { ref agent, .. } if agent == "codex"));
        let c = Cli::try_parse_from(["krypto", "hook", "codex", r#"{"type":"agent-turn-complete"}"#]).unwrap();
        assert!(matches!(c.command, Command::Hook { ref source, ref args } if source == "codex" && args.len() == 1));
        let c = Cli::try_parse_from(["krypto", "install-hooks", "copilot-vscode", "--apply"]).unwrap();
        assert!(matches!(c.command, Command::InstallHooks { target: HookTargetArg::CopilotVscode, apply: true, .. }));
        let c = Cli::try_parse_from(["krypto", "--port", "48000", "serve"]).unwrap();
        assert_eq!(c.port, Some(48000));
        let c = Cli::try_parse_from(["krypto", "machine", "--name", "Windows PC"]).unwrap();
        assert!(matches!(c.command, Command::Machine { name: Some(ref n), json: false } if n == "Windows PC"));
        let c = Cli::try_parse_from(["krypto", "machine", "--json"]).unwrap();
        assert!(matches!(c.command, Command::Machine { name: None, json: true }));
        let c = Cli::try_parse_from(["krypto", "serve", "--peer-listen", "192.168.1.20"]).unwrap();
        assert!(matches!(c.command, Command::Serve { peer_listen: Some(ref l) } if l == "192.168.1.20"));
        let c = Cli::try_parse_from(["krypto", "peer", "invite", "--ttl", "120", "--addr", "10.0.0.2:47801"]).unwrap();
        assert!(matches!(
            c.command,
            Command::Peer { action: PeerAction::Invite { ttl: Some(120), addr: Some(ref a) } } if a == "10.0.0.2:47801"
        ));
        let c = Cli::try_parse_from(["krypto", "peer", "join", "krypto://peer?v=1&mid=x"]).unwrap();
        assert!(matches!(c.command, Command::Peer { action: PeerAction::Join { ref uri } } if uri.starts_with("krypto://")));
        let c = Cli::try_parse_from(["krypto", "peer", "revoke", "Mac mini"]).unwrap();
        assert!(matches!(c.command, Command::Peer { action: PeerAction::Revoke { ref machine } } if machine == "Mac mini"));
        let c = Cli::try_parse_from(["krypto", "machines", "--json"]).unwrap();
        assert!(matches!(c.command, Command::Machines { json: true }));
        assert!(Cli::try_parse_from(["krypto", "peer"]).is_err());
    }

    fn device(id: &str, name: &str) -> Device {
        Device {
            id: id.into(),
            kind: DeviceKind::Hub,
            name: name.into(),
            public_key: "pk".into(),
            paired_at: 0,
            last_seen: None,
            addr: None,
        }
    }

    #[test]
    fn revoke_targets_resolve_by_id_or_unique_name() {
        let ds = vec![device("aaaaaa111111", "Mac mini"), device("bbbbbb222222", "Laptop"), device("cccccc333333", "laptop")];
        assert_eq!(resolve_device(&ds, "aaaaaa111111").unwrap().id, "aaaaaa111111");
        assert_eq!(resolve_device(&ds, " mac MINI ").unwrap().id, "aaaaaa111111");
        let err = resolve_device(&ds, "Laptop").unwrap_err().to_string();
        assert!(err.contains("bbbbbb222222") && err.contains("cccccc333333"), "{err}");
        assert!(resolve_device(&ds, "nope").unwrap_err().to_string().contains("no paired machine"));
    }

    #[test]
    fn pairing_error_codes_read_well() {
        for code in ["peer_listener_off", "invalid", "bad_uri", "self_invite", "unreachable", "invite_rejected", "timeout", "not_found"] {
            assert!(peer_error_text(code).is_some(), "{code}");
        }
        assert_eq!(peer_error_text("something else"), None);
        assert_eq!(printable("Mac\u{1b}[2Jmini"), "Mac?[2Jmini");
    }

    /// `krypto machines` without a hub lists this machine from machine.toml and peers from hub.db.
    #[tokio::test]
    async fn machines_command_without_a_hub() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path());
        if std::env::var(config::ENV_TOKEN).is_ok() {
            return; // an env token could reach a real hub; never touch it from tests
        }
        run_machines(&paths, Some(9), true).await.unwrap();
        let me = machine::load(&paths).unwrap().expect("created");
        let store = crate::store::Store::open(&paths.db, &me.id).unwrap();
        let mut d = device("zyxwvu987654", "Mac mini");
        d.addr = Some("10.0.0.2:47801".into());
        let m = Machine {
            id: d.id.clone(),
            name: d.name.clone(),
            os: MachineOs::Macos,
            last_seen: 5,
            online: false,
            hub_version: Some("0.1.0".into()),
            user_active_at: None,
        };
        store.upsert_device(&krypto_peer::RosterEntry { device: d, machine: m }).unwrap();
        drop(store);
        run_machines(&paths, Some(9), false).await.unwrap();
        run_machines(&paths, Some(9), true).await.unwrap();
    }

    #[test]
    fn intent_commands_parse() {
        let c = Cli::try_parse_from(["krypto", "start", "codex", "--cwd", "/srv/api", "--machine", "Mac mini", "--yes"]).unwrap();
        assert!(matches!(c.command, Command::Start { machine: Some(ref m), yes: true, .. } if m == "Mac mini"));
        let c = Cli::try_parse_from(["krypto", "approve", "abcd1234", "--option", "allow-always", "--yes"]).unwrap();
        assert!(matches!(c.command, Command::Approve { ref request, option: Some(ref o), yes: true } if request == "abcd1234" && o == "allow-always"));
        let c = Cli::try_parse_from(["krypto", "deny", "abcd1234"]).unwrap();
        assert!(matches!(c.command, Command::Deny { ref request, option: None } if request == "abcd1234"));
        assert!(Cli::try_parse_from(["krypto", "deny", "abcd", "--yes"]).is_err(), "deny has no --yes");
        let c = Cli::try_parse_from(["krypto", "pending", "--json"]).unwrap();
        assert!(matches!(c.command, Command::Pending { json: true }));
    }

    fn machine(id: &str, name: &str) -> Machine {
        Machine { id: id.into(), name: name.into(), os: MachineOs::Linux, last_seen: 0, online: true, hub_version: None, user_active_at: None }
    }

    #[test]
    fn machines_resolve_by_id_or_unique_name() {
        let ms = vec![machine("aaaaaa111111", "Mac mini"), machine("bbbbbb222222", "Laptop"), machine("cccccc333333", "laptop")];
        assert_eq!(resolve_machine(&ms, "aaaaaa111111").unwrap().id, "aaaaaa111111");
        assert_eq!(resolve_machine(&ms, " MAC MINI ").unwrap().id, "aaaaaa111111");
        assert!(resolve_machine(&ms, "laptop").unwrap_err().to_string().contains("bbbbbb222222"));
        assert!(resolve_machine(&ms, "desk").is_err());
    }

    #[test]
    fn requests_resolve_and_options_follow_the_verb() {
        let mut r1 = req();
        r1.request_id = "zyxwvu987654:abcd1234-0000".into();
        let mut r2 = req();
        r2.request_id = "zyxwvu987654:abce9999-0000".into();
        let pending = vec![r1.clone(), r2.clone()];
        assert_eq!(resolve_request(&pending, "abcd1").unwrap().unwrap().request_id, r1.request_id);
        assert_eq!(resolve_request(&pending, &r2.request_id).unwrap().unwrap().request_id, r2.request_id);
        assert!(resolve_request(&pending, "abc").is_err(), "too short");
        assert!(resolve_request(&pending, "abcx").is_err(), "no match");
        assert!(resolve_request(&pending, "zyxwvu987654:gone").unwrap().is_none(), "a full id goes to its owner as is");
        let mut amb = pending.clone();
        amb[1].request_id = "zyxwvu987654:abcd9999".into();
        assert!(resolve_request(&amb, "abcd").unwrap_err().to_string().contains("matches 2"));

        assert_eq!(pick_option(&r1, true, None).unwrap().as_deref(), Some("ok"));
        assert_eq!(pick_option(&r1, false, None).unwrap().as_deref(), Some("no"));
        assert_eq!(pick_option(&r1, true, Some("always")).unwrap().as_deref(), Some("always"));
        assert!(pick_option(&r1, true, Some("no")).unwrap_err().to_string().contains("krypto deny"), "approve with a reject option");
        assert!(pick_option(&r1, false, Some("ok")).unwrap_err().to_string().contains("krypto approve"), "deny with an allow option");
        assert!(pick_option(&r1, true, Some("bogus")).is_err());
    }

    #[test]
    fn another_machine_always_asks_a_person() {
        // Remote approve and remote start: always asked, --yes or not.
        for yes in [false, true] {
            assert!(asks_first(true, yes, true), "remote approve, yes={yes}");
            assert!(asks_first(true, yes, false), "remote start, yes={yes}");
        }
        // This machine: approve asks unless --yes; start never asks.
        assert!(asks_first(false, false, true));
        assert!(!asks_first(false, true, true));
        assert!(!asks_first(false, false, false));
        assert!(PAIRED_CAN.contains("approve once") && PAIRED_CAN.contains("accept_intents = false"));
    }

    #[test]
    fn intent_errors_read_well() {
        for code in ["already_decided", "offline", "timeout", "unconfirmed", "forbidden", "rate_limited", "invalid", "not_found", "not_owner"] {
            assert!(intent_error_text(code, "Mac mini").is_some(), "{code}");
        }
        assert_eq!(intent_error_text("offline", "Mac mini").unwrap(), "Mac mini is offline.");
        assert_eq!(intent_error_text("already_decided", "x").unwrap(), "Already answered.");
        let f = intent_error_text("forbidden", "Mac mini").unwrap();
        assert!(f.contains("\"always allow\" is chosen on Mac mini itself") && f.contains("start_roots") && f.contains("accept_intents"), "{f}");
        assert!(intent_error_text("whatever", "x").is_none());
    }

    #[test]
    fn short_ids_use_the_local_part() {
        assert_eq!(short_id("abcdef012345:0f1e2d3c-4b5a-6978"), "0f1e2d3c");
        assert_eq!(short_id("legacy-session-id"), "legacy-s");
        assert_eq!(short_id("abcdef012345:ab"), "ab");
    }

    /// `krypto machine` without a hub works on machine.toml directly (scratch data dir; the port is
    /// one nothing listens on, and there is no token, so no hub is ever contacted).
    #[tokio::test]
    async fn machine_command_without_a_hub() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path());
        if std::env::var(config::ENV_TOKEN).is_ok() {
            return; // an env token could reach a real hub; never touch it from tests
        }
        run_machine(&paths, Some(9), None, true).await.unwrap();
        let first = machine::load(&paths).unwrap().expect("created");
        run_machine(&paths, Some(9), Some("  Windows PC ".into()), false).await.unwrap();
        let renamed = machine::load(&paths).unwrap().unwrap();
        assert_eq!(renamed, MachineIdentity { id: first.id.clone(), name: "Windows PC".into() });
        let err = run_machine(&paths, Some(9), Some(" ".into()), false).await.unwrap_err();
        assert_eq!(format!("{err:#}"), "machine name must be 1-40 characters (got 0)");
        assert_eq!(machine::load(&paths).unwrap().unwrap(), renamed, "a rejected name changes nothing");
    }

    #[test]
    fn prompt_echo_printed_once() {
        let mut p = Printer::new("codex");
        let ev = |kind: EventType, text: &str| HubEvent {
            id: "abcdef012345:1".into(),
            seq: 1,
            machine_id: "abcdef012345".into(),
            session_id: "abcdef012345:s".into(),
            kind,
            payload: serde_json::json!({ "text": text }),
            ts: 0,
        };
        assert_eq!(p.user_line("run tests").as_deref(), Some("you: run tests"));
        assert_eq!(p.user_line("run tests "), None, "agent replay of the same prompt is suppressed");
        p.event(&ev(EventType::AgentMessageChunk, "ok"));
        assert_eq!(p.user_line("run tests").as_deref(), Some("you: run tests"), "a new turn with the same text prints again");
        p.finish_line();
    }

    #[test]
    fn ago_formats() {
        assert_eq!(ago(10_000, 0), "10s ago");
        assert_eq!(ago(3_600_000, 0), "1h ago");
    }
}
