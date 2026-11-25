# Pawl — Contracts & Ownership (read before coding)

Plan: `PLAN.md`. This file fixes the interfaces so parallel engineers can build
against each other without waiting. **Code against these APIs exactly.** If one
is wrong or missing, tell the orchestrator (`hc send t13 "..."`); do not edit
another owner's files.

## Ground rules (all engineers)

- Repo root: `E:\workspace\ai-pet` (Windows; Git Bash + PowerShell available). Git repo; work on the step branch the orchestrator names and never commit yourself.
- Stay inside your owned paths (table below). Root files (`Cargo.toml`,
  `package.json`, `tsconfig.base.json`, `docs/CONTRACTS.md`,
  `packages/protocol`, `crates/protocol`) are orchestrator-owned: read-only for you.
- Wire types: `packages/protocol/src/index.ts` (TS, source of truth) and
  `crates/protocol/src/lib.rs` (Rust mirror, crate `pawl-protocol`). Import them; never redefine.
- E2E tests are out of scope for now. Write real, working code; keep it compiling
  (`cargo check -p <crate>` / `tsc --noEmit`). Small unit tests are welcome where cheap.
- bun (TypeScript toolchain + package manager, workspaces in root package.json): never run bare `bun install`/`bun add` — parallel installs race on bun.lock and node_modules. Use `bash scripts/bun-locked.sh install` or
  `bash scripts/bun-locked.sh add <dep> --cwd <your/package/dir>` (serializes via a lock). Run scripts with `bun run <script>` inside your package; typecheck with `bunx tsc --noEmit`.

- Cargo: `cargo check -p <your-crate>` is safe in parallel (cargo locks). Add
  crate-specific deps to your crate's own `Cargo.toml`; shared ones use `{ workspace = true }`.
- Skeleton first: within your first few minutes, create your package/crate with the
  public API below (stub bodies OK: `todo!()` / `throw new Error("todo")`) so
  dependents compile. Then fill it in.
- No subagents, no new hc agents. When done or blocked:
  `hc send t13 "<role>: DONE|BLOCKED — <summary>; details in docs/reports/<role>.md"`.
  Write the report file (files changed, commands run + results, known gaps, decisions needed).

## Ownership map

| Role | Owns | Package/crate |
|---|---|---|
| sde-acp | `crates/acp/` | `pawl-acp` |
| sde-hub | `crates/hub/` | `pawl-hub` (lib + bin `pawl`) |
| sde-discovery | `crates/discovery/` | `pawl-discovery` |
| sde-brain | `crates/brain/` | `pawl-brain` |
| sde-relay (wave 2) | `crates/relay-link/`, `services/relay/` | `pawl-relay-link`, `@pawl/relay` |
| fe-engine | `packages/pet-engine/`, `packages/hub-client/` | `@pawl/pet-engine`, `@pawl/hub-client` |
| fe-desktop | `apps/desktop/` (incl. `src-tauri/`) | `@pawl/desktop` |
| fe-vscode | `apps/vscode/` | `pawl-vscode` |
| fe-mobile (wave 2) | `apps/mobile/` | `@pawl/mobile` |
| art-sprites | `assets/pets/`, `scripts/art/` | sprite sheets + manifest |

## Runtime facts

- Hub binds **only** `127.0.0.1:47800` (override `PAWL_HUB_URL` / `--port`).
- Data dir: `pawl_protocol::data_dir()` = `dirs::data_dir()/pawl`
  (Windows `%APPDATA%\pawl`, macOS `~/Library/Application Support/pawl`,
  Linux `~/.local/share/pawl`). Contains `machine.toml` (`id`, `name`: this machine's identity; never edit `id`), `hub.db` (SQLite), `token`,
  `config.toml`, `registry-cache.json`, `device.key` (X25519 secret for peer links; never read by clients, never logged).
- `token`: 32 random bytes hex, created by the hub on first start. Clients read it
  (env `PAWL_HUB_TOKEN` overrides).
- Endpoints (crate `pawl-hub`):
  - `GET  /health` → `{"ok":true,"version":"x"}` (no auth)
  - `GET  /ws` → WebSocket. First frame `hello{token}`. Reject non-empty `Origin`
    headers other than `vscode-webview://*`, `tauri://localhost`, `http://tauri.localhost`,
    `https://tauri.localhost`, `http://localhost:1420` (vite dev). Browsers can reach localhost; this blocks drive-by pages.
  - `POST /hooks/{claude|codex|copilot}` with `Authorization: Bearer <token>`, body = raw hook JSON → `{"ok":true}`.
- Hook forwarding: agents' hook configs call `pawl hook <source>` which reads
  stdin JSON and POSTs it (fails silently in <2 s if the hub is down; never blocks the agent).

## Rust crate APIs

### `pawl-acp` (sde-acp) — ACP client host

Uses the official `agent-client-protocol` crate (check docs.rs for the current API).
Spawns agents as stdio subprocesses. If the crate's futures are `!Send`, run each
connection on a dedicated thread with a `LocalSet` and talk via channels — the
public API below MUST be `Send + Sync + Clone`.

```rust
pub enum HostEvent {
    /// Upsert (status changes, title, ended).
    Session(pawl_protocol::Session),
    /// Normalized ACP update for the event log. payload = protocol payload shapes
    /// (TextPayload, ToolCallPayload, PlanPayload, StopPayload, ErrorPayload).
    Event { session_id: SessionId, kind: pawl_protocol::EventType, payload: serde_json::Value },
    /// Agent asked `session/request_permission`. Host waits for resolve_permission().
    PermissionRequest(pawl_protocol::PermissionRequest),
    AgentCapabilities { agent_id: AgentId, capabilities: pawl_protocol::AgentCapabilities },
}

#[derive(Clone)]
pub struct AcpHost { /* ... */ }
impl AcpHost {
    pub fn new(events: tokio::sync::mpsc::UnboundedSender<HostEvent>) -> Self;
    /// Spawn (or reuse) the agent process, `initialize`, `session/new`. If `prompt` is Some, send it (non-blocking).
    pub async fn start_session(&self, agent: &Agent, cwd: PathBuf, hub_session_id: SessionId, prompt: Option<String>) -> anyhow::Result<Session>;
    /// Starts a turn; returns once sent. Turn end arrives as Event{kind: Stop} + Session upsert.
    pub async fn prompt(&self, hub_session_id: &str, text: String) -> anyhow::Result<()>;
    pub async fn cancel(&self, hub_session_id: &str) -> anyhow::Result<()>;
    /// None = cancelled outcome.
    pub async fn resolve_permission(&self, request_id: &str, option_id: Option<String>) -> anyhow::Result<()>;
    /// `session/list` if capability present, else Ok(vec![]).
    pub async fn list_history(&self, agent: &Agent, cwd: Option<PathBuf>) -> anyhow::Result<Vec<HistorySession>>;
    /// `session/resume` (preferred) or `session/load` fallback per capabilities.
    pub async fn resume_session(&self, agent: &Agent, acp_session_id: String, cwd: PathBuf, hub_session_id: SessionId) -> anyhow::Result<Session>;
    /// Cancel pending permissions (by: System), session/close or cancel, mark done + endedAt; stops the agent
    /// process after its last session. No-op if unknown.
    pub async fn close_session(&self, hub_session_id: &str) -> anyhow::Result<()>;
    pub async fn shutdown(&self);
}
```
Status mapping the host emits: prompt sent → `thinking`; thought chunk → `thinking`;
tool_call/tool_call_update in_progress → `working`; permission request → `waiting_permission`;
stop end_turn → `done` (session stays open, idle for next prompt); refusal/error → `error`.
Client-side ACP capabilities: implement `fs/read_text_file` + `fs/write_text_file`
(restricted to the session cwd), terminals optional (advertise false if not implemented).

### `pawl-discovery` (sde-discovery) — agents + hooks

```rust
pub struct CustomAgent { pub id: String, pub name: String, pub command: Vec<String>, pub env: Option<BTreeMap<String,String>> }
pub struct DiscoveryConfig { pub cache_dir: PathBuf, pub custom_agents: Vec<CustomAgent>, pub offline: bool }
/// Fetch https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json (cache 24h in cache_dir/registry-cache.json,
/// fall back to cache, then to a built-in list incl. claude-code via @agentclientprotocol/claude-agent-acp and codex via codex-acp).
/// Detect installed (PATH lookup incl. Windows PATHEXT, npx availability, cached binary). Merge custom agents.
pub async fn discover(cfg: &DiscoveryConfig) -> anyhow::Result<Vec<Agent>>;

pub mod hooks {
    pub struct NormalizedHook {
        pub external_session_id: String,     // e.g. Claude `session_id`; synthesize from cwd if absent
        pub agent_id: AgentId,               // "claude-code" | "codex" | "copilot"
        pub source: SessionSource,           // VscodeHook if detectable (env/term_program/entrypoint), else CliHook
        pub cwd: Option<String>,
        pub title: Option<String>,
        pub status: Option<SessionStatus>,   // SessionStart→idle, UserPromptSubmit→thinking, PreToolUse→working,
                                             // Notification(permission)→waiting_permission, Stop→done, SessionEnd→done+ended
        pub ended: bool,
        pub event: EventType,                // usually Hook; SessionStart/SessionEnd map to session_start/session_end
        pub payload: HookPayload,
        pub notification: Option<(NotificationLevel, String, String)>, // (level, title, body) worth surfacing
    }
    pub fn normalize(source: HookSource, raw: serde_json::Value) -> Option<NormalizedHook>;
    /// Used by `pawl hook <source>`: read stdin JSON, POST to hub, 2s timeout, never error loudly.
    pub async fn forward(source: HookSource, raw: serde_json::Value, hub_url: &str, token: &str) -> anyhow::Result<()>;
    /// Config snippets/merges for: ~/.claude/settings.json, ~/.codex/config.toml (notify), ~/.copilot/hooks/pawl.json,
    /// .github/hooks/pawl.json. `apply=false` returns a dry-run diff text; `apply=true` merges (backup file first).
    pub fn install(target: HookTarget, hub_exe: &Path, apply: bool) -> anyhow::Result<String>;
    pub enum HookTarget { Claude, Codex, CopilotCli, CopilotVscode { repo: PathBuf } }
}
```

### `pawl-brain` (sde-brain) — rules, bot, watchdog, pet growth

```rust
pub mod rules {
    pub fn default_rules() -> Vec<Rule>;          // auto_allow think only; ask read/search/edit/move/delete/execute/fetch/other (2026-11-24: "by default it should be Ask")
    /// First matching rule wins; no match → Ask. Never AutoAllow delete/execute/fetch unless a rule explicitly
    /// matches that toolKind (default_rules never does). Glob on every location path.
    pub fn evaluate(rules: &[Rule], req: &PermissionRequest) -> RuleAction;
    /// Pick the option for an auto decision: AutoAllow → first allow_once (else allow_always); Deny → reject_once.
    pub fn pick_option(req: &PermissionRequest, action: RuleAction) -> Option<String>;
}
pub mod chat {
    pub struct BrainConfig { pub anthropic_api_key: Option<String>, pub model: String /* "claude-haiku-5-5" */, pub ollama_url: Option<String>, pub ollama_model: Option<String> }
    pub struct BrainContext { pub agents: Vec<Agent>, pub sessions: Vec<Session>, pub recent_events: Vec<HubEvent>, pub pending: Vec<PermissionRequest>, pub pet: Option<Pet> }
    pub enum BrainAction {
        StartSession { agent_id: AgentId, cwd: String, prompt: String },
        Prompt { session_id: SessionId, text: String },
        ResolvePermission { request_id: RequestId, option_id: Option<String> },
    }
    pub struct BrainReply { pub text: String, pub action: Option<BrainAction> }
    pub struct Brain { /* ... */ }
    impl Brain {
        pub fn new(cfg: BrainConfig) -> Self;
        /// Rule-based intent parse first ("what's claude doing", "tell codex to run the tests in repo X",
        /// "approve"/"deny"), LLM for summaries/small talk, deterministic fallback when no model configured.
        pub async fn handle(&self, text: &str, ctx: &BrainContext) -> anyhow::Result<BrainReply>;
        pub async fn summarize_session(&self, session: &Session, events: &[HubEvent]) -> anyhow::Result<String>;
        pub async fn daily_report(&self, sessions: &[Session], events: &[HubEvent]) -> anyhow::Result<String>;
    }
}
pub mod watchdog {
    pub struct WatchdogConfig { pub waiting_nudge_after_ms: i64 /* default 5 min */, pub repeat_every_ms: i64 }
    pub struct Watchdog { /* remembers what it already nudged */ }
    impl Watchdog { pub fn new(cfg: WatchdogConfig) -> Self; pub fn tick(&mut self, sessions: &[Session], pending: &[PermissionRequest], now: UnixMs) -> Vec<HubNotification>; }
}
pub mod growth {
    pub fn default_pet(agent: &Agent) -> Pet;     // species Dog, fun default name, mood 70, xp 0
    pub fn on_event(pet: &mut Pet, kind: EventType, payload: &serde_json::Value) -> bool; // true if changed
}
```
Anthropic calls: plain `reqwest` POST `https://api.anthropic.com/v1/messages`,
headers `x-api-key`, `anthropic-version: 2023-06-01`; model default `claude-haiku-5-5`.

### `pawl-hub` (sde-hub) — the daemon

Modules: `config` (data dir, token, config.toml: port, custom agents, rules, brain), `store` (rusqlite bundled; tables agents, sessions, events, pets, rules, chat, audit_log), `bus` (tokio broadcast of `ServerMsg`), `server` (axum: /health, /ws, /hooks/:source), `core` (owns AcpHost, discovery results, rules, brain, watchdog; merges ACP + hook feeds into sessions/events; applies rules to permission requests; writes audit log for every approval), `notify` (desktop notifications are a client concern — hub only emits `notification` msgs).
CLI (`clap`): `pawl serve [--port]`, `agents [--refresh]`, `sessions`, `start <agent> --cwd <dir> [--prompt <text>]` (talks to a running hub over WS if up, else runs in-process and streams events to stdout), `hook <source>`, `install-hooks <claude|codex|copilot-cli|copilot-vscode> [--apply]`, `token`.
Relay integration is wave 2 (sde-relay will add it behind a `relay` module).

## TypeScript package APIs

### `@pawl/hub-client` (fe-engine) — works in browser, Tauri webview, VS Code webview and Node

```ts
import type { ClientMsg, ServerMsg, AckResults, ClientKind, Agent, Session, Pet, PermissionRequest, HubEvent, ChatMessage, HubNotification, Rule } from "@pawl/protocol";
export interface HubState {
  status: "disconnected" | "connecting" | "connected" | "unauthorized";
  agents: Agent[]; sessions: Record<string, Session>; pets: Record<string, Pet>;
  pending: Record<string, PermissionRequest>; rules: Rule[];
  events: Record<string, HubEvent[]>;      // per session, capped (last 500)
  chat: ChatMessage[]; notifications: HubNotification[];
}
export interface HubClientOptions { url?: string /* default ws://127.0.0.1:47800/ws */; token: string; client: ClientKind; clientVersion: string;
  WebSocketImpl?: any /* pass `ws` in Node */; reconnect?: boolean /* default true, backoff 0.5s→10s */ }
export class HubClient {
  constructor(opts: HubClientOptions);
  connect(): void; close(): void;
  getState(): HubState;
  subscribe(fn: (state: HubState, msg?: ServerMsg) => void): () => void;
  onMessage<T extends ServerMsg["type"]>(type: T, fn: (msg: Extract<ServerMsg, { type: T }>) => void): () => void;
  request<T extends keyof AckResults>(msg: DistributiveOmit<Extract<ClientMsg, { type: T }>, "id">): Promise<AckResults[T]>; // rejects on ok:false or 15s timeout
  startSession(agentId: string, cwd: string, prompt?: string): Promise<Session>;
  prompt(sessionId: string, text: string): Promise<void>;
  cancel(sessionId: string): Promise<void>;
  resolvePermission(requestId: string, optionId: string | null): Promise<void>;
  approve(requestId: string): Promise<void>;   // picks allow_once
  deny(requestId: string): Promise<void>;      // picks reject_once, else null
  chat(text: string, petId?: string): Promise<ChatMessage>;
  updatePet(petId: string, patch: Partial<Pick<Pet,"species"|"name"|"skin"|"personality">>): Promise<Pet>;
  listAgents(refresh?: boolean): Promise<Agent[]>;
  getEvents(sessionId: string, sinceId?: number): Promise<HubEvent[]>;
  listHistory(agentId: string, cwd?: string): Promise<HistorySession[]>;
  resumeSession(agentId: string, acpSessionId: string, cwd: string): Promise<Session>;
}
// subpath "@pawl/hub-client/react"
export function useHubState(client: HubClient): HubState;
// subpath "@pawl/hub-client/node" (Node only; never import from browser code)
export interface HubCredentials { url: string /* ws */; httpUrl: string; token: string; tokenSource: "env" | "file"; dataDir: string; tokenFile: string }
export function readHubCredentials(opts?: HubCredentialsOptions): HubCredentials;   // sync; throws HubCredentialsError if no token
export function tryReadHubCredentials(opts?: HubCredentialsOptions): HubCredentials | null;
// Env PAWL_HUB_TOKEN / PAWL_HUB_URL (http or ws form) override. Extras (non-breaking): HubState.hubVersion/lastError,
// HubClient.setToken/listSessions/setRules/wsUrl; PetStage/PetBubble callbacks may return Promise (buttons disabled while pending).
```

### `@pawl/pet-engine` (fe-engine)

```ts
import type { Species, Session, HubEvent, PermissionRequest, Pet } from "@pawl/protocol";
export type PetActivity = "napping" | "idle" | "thinking" | "working" | "planning" | "alert" | "happy" | "sad";
export interface PetView { petId: string; agentId: string; species: Species; name: string; activity: PetActivity; since: number;
  bubble?: { text: string; kind: "say" | "permission"; requestId?: string }; sessionId?: string; mood: number; }
/** Pure. Table in PLAN.md "Pet engine". thought→thinking, tool_call edit/execute→working, plan→planning,
 * request_permission→alert, stop end_turn→happy (decays to idle after 6s), error/refusal→sad (decays 10s),
 * no active sessions for 60s→napping. */
export function activityForEvent(e: HubEvent): PetActivity | null;
export function derivePetViews(input: { pets: Pet[]; sessions: Session[]; pending: PermissionRequest[]; lastEvents: Record<string, HubEvent | undefined>; now: number }): PetView[];
export function petLine(species: Species, activity: PetActivity, seed?: number): string; // flavor text ("Woof! Need you!")
export const SPECIES_META: Record<Species, { label: string; emoji: string; sound: string }>;
// subpath "@pawl/pet-engine/react" — pure SVG + CSS keyframes, no assets needed (Rive later)
export function PetSprite(props: { species: Species; activity: PetActivity; size?: number; facing?: "left" | "right"; walking?: boolean; skin?: string }): JSX.Element;
export function PetBubble(props: { view: PetView; onApprove?: () => void; onDeny?: () => void; onClick?: () => void }): JSX.Element | null;
export function PetStage(props: { views: PetView[]; width: number; onApprove(requestId: string): void; onDeny(requestId: string): void; onPetClick?(petId: string): void }): JSX.Element; // pets walk/idle along a strip
```
Both packages: `"type":"module"`, `main`/`types`/`exports` point at `src/*.ts(x)` (consumed as source by Vite/bun build/esbuild; no build step). Workspace deps use `"workspace:*"`. Peer dep `react >=18`.

## Relay (wave 2, sde-relay + fe-mobile)

- Crypto: NaCl box (X25519 + XSalsa20-Poly1305, 24-byte nonce). Rust `crypto_box::SalsaBox`; TS `tweetnacl` `nacl.box`. Base64 (standard) on the wire.
- Pairing QR: `pawl://pair?relay=<https-url>&hub=<hubId>&pk=<hubPkB64>&code=<8 chars>` (code one-time, 5 min).
- Relay frames (JSON over WS `wss://<relay>/v1/connect?role=hub|device|pairing&hubId=..`):
  `{t:"msg", from, to, nonce, box}` (box = encrypted UTF-8 JSON of a protocol `ClientMsg`/`ServerMsg`),
  `{t:"pair_request", deviceId, devicePk, deviceName, code}`, `{t:"pair_ok", deviceId, nonce, box}`,
  `{t:"register_push", deviceId, expoPushToken}`, `{t:"push", to}` (content-free wake: "Your pet needs you").
- Relay sees only ciphertext + routing ids. Remote approvals require biometrics on the phone (expo-local-authentication).

## Pet sprite assets (art-sprites → consumed by pet-engine, desktop, vscode, mobile)

- Generated with Google Gemini image models (key `GEMINI_API_KEY` in repo-root `.env`; never print, log, or commit it).
- Layout: `assets/pets/<species>/<activity>.png` — horizontal sprite strip, transparent background (RGBA),
  each frame `frameSize` square, character feet on the same baseline, facing **right**.
  Activities = `PetActivity` ("napping","idle","thinking","working","planning","alert","happy","sad") + `"walk"`.
- `assets/pets/manifest.json` (written by art-sprites; pet-engine ships a matching TS type):
```json
{
  "version": 1,
  "frameSize": 256,
  "species": {
    "dog": {
      "skins": ["default"],
      "animations": {
        "idle":  { "src": "dog/idle.png",  "frames": 4, "fps": 6, "loop": true },
        "alert": { "src": "dog/alert.png", "frames": 4, "fps": 10, "loop": true }
      }
    }
  }
}
```
- pet-engine: `PetSprite` accepts optional `sprites?: { manifest: SpriteManifest; baseUrl: string }`;
  when the species/activity animation exists it renders the strip with CSS `steps()` animation, else falls
  back to the built-in SVG character. Export `type SpriteManifest` and `loadSpriteManifest(baseUrl): Promise<SpriteManifest|null>`.
- Desktop serves `assets/pets` as static files (Vite `publicDir` copy or a copy step) at `/pets/`; VS Code copies
  to `dist/media/pets/` and passes a `webview.asWebviewUri` base. Missing assets must never break rendering.

---

## Step 1 — Machine identity (PLAN.md migration step 1) — protocol v2

Protocol already updated by the orchestrator (`packages/protocol/src/index.ts`, `crates/protocol/src/lib.rs`, PROTOCOL_VERSION = 2):
`Machine {id,name,os,lastSeen,online,hubVersion?}`, `MachineId` (12 chars `[a-z0-9]`), `globalId()/parseGlobalId()`
(Rust `global_id/parse_global_id/is_valid_machine_id`, `MachineOs::current()`), `machineId` on Agent, Session,
HistorySession, HubEvent, PermissionRequest, Pet, ChatMessage, HubNotification. `HubEvent.id` is now the global
string `${machineId}:${seq}` and `HubEvent.seq` the numeric cursor; `get_events.sinceId` -> `sinceSeq`.
New `ClientMsg update_machine{machineId,name}` -> `ack{machine}`; new `ServerMsg machine{machine}`;
`Welcome.machine` (this hub) + `Welcome.machines` (all known, incl. self).

Rules: **every id on the wire is global** (`${machineId}:${localId}`). Hubs store global ids directly
(no boundary translation). A ClientMsg that names an id whose machine prefix is not this hub's ->
`ack{ok:false,error:"not_owner"}` for now (step 4 routes it). Legacy un-prefixed ids -> `"not_found"`.

### Rust API changes (sde-libs implements; sde-hubcore codes against them)
```rust
// pawl-acp
impl AcpHost { pub fn new(machine_id: MachineId, events: UnboundedSender<HostEvent>) -> Self; }
// every Session / HistorySession / PermissionRequest / HubEvent payload it builds carries machine_id;
// request ids are global: global_id(machine_id, uuid). hub_session_id passed in is already global.

// pawl-discovery
pub struct DiscoveryConfig { pub machine_id: MachineId, /* existing fields */ }  // Agent.machine_id = cfg.machine_id
// hooks::NormalizedHook unchanged (hub assigns machine ids)

// pawl-brain
pub fn growth::default_pet(agent: &Agent, taken_names: &[String]) -> Pet;
//   id = global_id(&agent.machine_id, format!("pet-{}", agent.id)) (deterministic, one per agent per machine)
//   name seeded by fnv1a(machine_id + "/" + agent_id); skip names in taken_names (case-insensitive) — pass the
//   names of every known pet on every machine so paired machines never show two "Biscuit"s.
impl Watchdog { pub fn new(cfg: WatchdogConfig, machine_id: MachineId) -> Self; }   // notifications carry machine_id
// chat: BrainContext gains `machine: Machine`; replies/ChatMessages carry machine_id. Status replies name the
//   machine when more than one machine is known ("Claude on Mac mini is working on ...").
```

### Hub (sde-hubcore)
- `data_dir/machine.toml` `{ id, name }`: id generated once (12 chars `[a-z0-9]` from a CSPRNG), name defaults
  to the host name (Windows `COMPUTERNAME`, macOS `scutil --get ComputerName`, Linux `/etc/hostname`, else "This machine").
  No new dependencies.
- CLI: `pawl machine` (print id/name/os), `pawl machine --name "Windows PC"` (works with or without a running hub;
  a running hub broadcasts `ServerMsg::Machine`).
- Store migration (schema v2): add `machine_id` columns; rewrite existing ids to global form; events keep rowid as seq.
- Welcome: `machine` + `machines` (`[self]` for now, online=true, lastSeen=now).
- `update_machine` only for the local machine id (else `not_owner`); trims, 1–40 chars.

### Clients
- hub-client (fe-core): `HubState.machine?: Machine`, `HubState.machines: Record<MachineId, Machine>`; handles
  `machine` msgs; events stored per session keyed by global id, cursor = `seq`; `getEvents(sessionId, sinceSeq?, limit?)`;
  `HubClient.updateMachine(name): Promise<Machine>` (local machine).
- pet-engine (fe-core): `PetView` gains `machineId` and `machineName?`; derivePetViews input gains
  `machines?: Record<MachineId, Machine>`; pets match sessions/pending by `(machineId, agentId)`; `PetStage.showNames`
  defaults to **true**; name label shows `name` (visitors area comes in step 3).
- desktop/vscode (fe-apps): show pet names by default; show the machine name (panel header / status-bar tooltip);
  desktop Settings + VS Code command "Pawl: Rename This Machine" call `updateMachine`; use `seq` cursors.
- mobile: frozen; only keep `bunx tsc --noEmit` green against protocol v2 (fe-core does the minimal type fixes).

## Step 2 — Pairing + LAN presence (PLAN.md migration step 2) — protocol v3

Branch `step-2-pairing-presence`. Protocol already updated by the orchestrator (PROTOCOL_VERSION = 3):
`Device`, `DeviceKind`, `PeerStatus`, `RepoRef`, `Presence`, `PeerEnvelope`, `PeerBody`, `PairRejectReason`,
`ByeReason`, constants `PEER_DEFAULT_PORT` (47801), `PEER_PATH` ("/peer"), `PRESENCE_INTERVAL_MS` (15 s),
`PRESENCE_OFFLINE_MS` (45 s). ClientMsgs `create_invite{ttlSecs?,addr?}` -> `{uri,expiresAt}`,
`join_peer{uri}` -> `{device,machine}`, `revoke_device{deviceId}` -> `{}`. ServerMsgs `machine_removed{machineId}`,
`devices{devices}`, `peer_status{peer}`. `Welcome.devices` + `Welcome.peer`. Read the doc comments there; they are the spec.

Scope: pairing, encrypted links, presence heartbeats, online/offline. **Not** in step 2: event mirroring and remote
pets on screen (step 3), intents (step 4), relay (step 6), phone and group-roster introduction (step 7).
Peer frames never reach ClientMsg handling: a peer can only pair, say hello, send presence and say bye.

### Ownership (step 2)
| Role | Owns |
|---|---|
| sde-peer | `crates/peer/` (`pawl-peer`, new; skeleton exists) |
| sde-hubcore | `crates/hub/` |
| fe-apps | `apps/desktop/`, `apps/vscode/`, `packages/hub-client/` |

Dependencies: `crypto_box` is the only new one (approved; sde-peer adds it to `crates/peer/Cargo.toml`).
Reuse the versions the hub already uses for `axum 0.8 (ws)`, `tokio-tungstenite 0.29`, `getrandom 0.4`, `subtle`,
`hex`. Base64 is hand-rolled in `pawl-peer` (no `base64` crate). Ask t13 before adding anything else.

### Crypto + wire
- Device key: X25519 secret in `data_dir/device.key` (64 hex chars; unix mode 0600). Created on first start from
  getrandom. Never logged, printed or sent. Public key on the wire = base64 standard padded (URI: base64url, no padding).
- Box: `crypto_box::SalsaBox` (NaCl `crypto_box`, compatible with tweetnacl `nacl.box`). Nonce (24 bytes) =
  15 random bytes || dir byte (1 if sender pk < receiver pk bytewise, else 0) || seq u64 big-endian. NaCl box has no
  AAD, so this binds the plaintext `seq` and the direction into the authenticated nonce; `open()` rejects a nonce
  whose seq differs from `env.seq` or whose dir is wrong (no seq-bumped replays, no frames reflected to their
  sender). The TS side (step 7) must use the same layout.
- `seq`: sender uses `max(last_tx + 1, now_ms * 1000)` per peer and persists `last_tx`; receiver accepts only
  `seq > last_rx` for that peer and persists `last_rx`. Survives restarts on both sides.
- Invite URI: `pawl://peer?v=1&mid=<machineId>&pk=<b64url pk>&addr=<host:port>&code=<b64url 16 random bytes>&exp=<unix ms>`.
  Code: CSPRNG, single use (consumed on the first matching attempt even if a later step fails), compared in constant
  time (`subtle`), held in memory only, never logged. TTL 60..=3600 s, default 600. A wrong code does NOT burn
  invites (anyone who knows the hub's public key could block pairing); instead pair attempts are rate-limited
  per remote IP and `PairRejected` events are coalesced. 128-bit codes make guessing infeasible.
- Peer-reported addrs (`hello`/`pair_request`/`pair_accept` `addr`) are stored only if they are IP literals in
  private (10/8, 172.16/12, 192.168/16), Tailscale CGNAT (100.64/10), link-local, loopback or IPv6 ULA ranges;
  otherwise they are ignored. The addr in an invite URI (typed by the user) is not restricted.

### Pairing (inviter A must have the listener on; joiner B dials)
1. A `create_invite` -> URI (addr = the `addr` arg, else the bound listener addr; error `invalid` if that is 0.0.0.0 / ::).
2. B `join_peer{uri}`: parse (`bad_uri`), reject its own mid (`self_invite`), dial `ws://<addr>/peer` (`unreachable`,
   10 s `timeout`), send envelope `{from:B, to:A, pk:B.pk, box: pair_request{code, machine:B, addr:B.listening?}}`
   sealed B.sk -> A.pk (A.pk comes from the URI, so a man in the middle cannot answer).
3. A: a first frame carrying `pk` is accepted only while at least one invite is live (else close, no reply). Decrypt,
   check `to == A`, `machine.id == from`, valid machine id, `from != A`, then the code. On success upsert B in the roster
   (re-pairing replaces the key and clears revoked), audit-log it, reply `pair_accept{machine:A, addr, reqSeq}`
  (`reqSeq` = the request envelope's seq; B rejects a mismatch, so an old captured accept cannot be replayed); the socket
   becomes the live link. On failure reply `pair_reject{invalid_invite|bad_request}` and close; audit-log the attempt.
4. B: decrypt `pair_accept` with A.pk from the URI, store A (addr from the URI); the link is live.

### Links + presence
- Listener (opt-in, off by default): `pawl serve --peer-listen <ip[:port]>` or config.toml `[peer] listen = "ip:port"`
  (default port 47801). Serves **only** `GET /peer` (WebSocket); every other path 404; any request carrying an
  `Origin` header -> 403 (blocks browsers); max WebSocket frame/message 320 KiB (a 216 KiB body is ~288 KiB boxed + base64; raised in step 3); an unauthenticated socket must send a valid first frame
  within 5 s; at most 16 unauthenticated sockets at once. Binding 0.0.0.0 / :: logs a warning. It never exposes the
  local API.
- Dialer: for every non-revoked hub in the roster with an `addr` and no live link, dial with backoff 1 s -> 60 s (jitter).
  First frame: `hello{machine, addr?}`; the listener checks `from` is in the roster and not revoked, `to` is itself,
  decrypts with the roster key, checks seq, replies `hello`. Any check fails -> close without reply.
- Authenticated `hello`/`presence` update the stored addr, name, os, hubVersion and lastSeen.
- Two live links to one peer: keep the one dialed by the hub with the smaller machine id; close the other.
- Presence: sent right after hello, then every 15 s, and immediately when the snapshot changes (rename, agents, pets).
  No authenticated frame for 45 s -> offline, close the link. Socket closed or `bye` -> offline at once.
- Revoke: `bye{revoked}` on the live link, close, mark revoked, stop dialing. Receiving `bye{revoked}` marks the
  sender revoked too (mutual). Audit-log both.

### `pawl-peer` API (sde-peer implements; sde-hubcore codes against it)
```rust
pub mod b64 { pub fn encode(b: &[u8]) -> String; pub fn decode(s: &str) -> Result<Vec<u8>, B64Error>;
              pub fn encode_url(b: &[u8]) -> String; pub fn decode_url(s: &str) -> Result<Vec<u8>, B64Error>; }

pub struct DeviceKeys { /* private */ }
impl DeviceKeys {
    pub fn load_or_create(path: &Path) -> anyhow::Result<Self>;
    pub fn generate() -> Self;
    pub fn public_key(&self) -> [u8; 32];
    pub fn public_b64(&self) -> String;
}   // no Debug/Display that could print the secret

pub struct Invite { pub machine_id: MachineId, pub public_key: [u8; 32], pub addr: String, pub code: String, pub expires_at: UnixMs }
impl Invite { pub fn to_uri(&self) -> String; pub fn parse(uri: &str) -> Result<Invite, InviteError>; }

pub fn seal(keys: &DeviceKeys, from: &str, to: &str, to_pk: &[u8; 32], seq: u64, body: &PeerBody, include_pk: bool) -> PeerEnvelope;
pub fn open(keys: &DeviceKeys, env: &PeerEnvelope, sender_pk: &[u8; 32]) -> Result<PeerBody, OpenError>;

/// Roster storage the hub provides (SQLite). Sync, quick calls.
pub trait Roster: Send + Sync + 'static {
    fn get(&self, device_id: &str) -> Option<RosterEntry>;      // non-revoked only
    fn list(&self) -> Vec<RosterEntry>;                          // non-revoked only
    fn upsert(&self, entry: RosterEntry);                        // re-pair: replace key, clear revoked
    fn update_seen(&self, device_id: &str, machine: &Machine, addr: Option<&str>, ts: UnixMs);
    fn revoke(&self, device_id: &str);
    fn last_rx_seq(&self, device_id: &str) -> u64;
    fn set_last_rx_seq(&self, device_id: &str, seq: u64);
    fn last_tx_seq(&self, device_id: &str) -> u64;
    fn set_last_tx_seq(&self, device_id: &str, seq: u64);
}
pub struct RosterEntry { pub device: Device, pub machine: Machine /* last known; online false */ }

pub enum PeerEvent {
    Paired { device: Device, machine: Machine, we_invited: bool },
    PairRejected { remote_addr: String, reason: PairRejectReason },
    LinkUp { device_id: String, machine: Machine },
    LinkDown { device_id: String },
    Presence { device_id: String, presence: Presence },
    Revoked { device_id: String, by_peer: bool },
}

pub struct PeerTimings { pub presence_interval: Duration, pub offline_after: Duration, pub handshake_timeout: Duration,
                         pub backoff_min: Duration, pub backoff_max: Duration }   // Default = the constants above

#[derive(Clone)] pub struct PeerNode { /* Arc inner */ }
impl PeerNode {
    pub fn new(keys: DeviceKeys, me: Machine, roster: Arc<dyn Roster>, events: UnboundedSender<PeerEvent>) -> Self;
    pub fn with_timings(self, t: PeerTimings) -> Self;
    pub fn public_b64(&self) -> String;
    pub async fn listen(&self, addr: SocketAddr) -> anyhow::Result<SocketAddr>;   // returns the bound addr
    pub fn listening(&self) -> Option<SocketAddr>;
    pub fn create_invite(&self, advertise: Option<String>, ttl: Duration) -> Result<Invite, InviteError>; // ListenerOff, Invalid
    pub async fn join(&self, uri: &str) -> Result<(Device, Machine), JoinError>; // BadUri, SelfInvite, Unreachable, Rejected, Timeout
    pub fn connect_known(&self);                 // idempotent: dial loops for every roster hub with an addr
    pub fn set_machine(&self, me: Machine);
    pub fn set_presence(&self, p: Presence);     // latest snapshot; pushed now and every presence_interval
    pub fn send(&self, device_id: &str, body: PeerBody) -> bool;   // for step 3+
    pub async fn revoke(&self, device_id: &str);
    pub fn online(&self) -> Vec<(String, UnixMs)>;   // linked device ids + last authenticated frame ts
    pub async fn shutdown(&self);                // bye{shutdown} to every link
}
```
Error enums implement `Display` with the snake_case wire codes (`bad_uri`, `self_invite`, `unreachable`,
`invite_rejected`, `timeout`, `peer_listener_off`, `invalid`) so the hub can ack them verbatim.

### Hub (sde-hubcore)
- Store schema v3: `devices` (id, kind, name, os, hub_version, public_key, addr, paired_at, last_seen, revoked_at,
  last_rx_seq, last_tx_seq) + `impl Roster`. The audit log gains pairing entries (paired, pair_rejected, revoked,
  revoked_by_peer).
- Startup: `DeviceKeys::load_or_create(data_dir/device.key)`, `PeerNode::new`, listen if configured, `connect_known()`.
  Keep the presence snapshot fresh (machine, agents, own pets, `repos: []`). Shutdown sends `bye{shutdown}`.
- PeerEvents -> `ServerMsg::Machine` (online flips, renames), `MachineRemoved`, `Devices`. Welcome: `machines` = self
  then roster hubs (`online` from `PeerNode::online()`), `devices`, `peer`.
- ClientMsgs `create_invite`, `join_peer`, `revoke_device` per the protocol doc comments.
- CLI: `pawl serve --peer-listen <ip[:port]>`; `pawl peer invite [--ttl <secs>] [--addr <host:port>]` (prints the URI,
  the expiry and the exact `pawl peer join "<uri>"` line); `pawl peer join <uri>`; `pawl peer revoke <machineId|name>`;
  `pawl machines [--json]` (online, last seen, addr; works without a hub from hub.db, showing peers offline).
  invite/join/revoke need a running hub.

### Clients (fe-apps)
- hub-client: `HubState.devices: Device[]`, `HubState.peer?: PeerStatus`; Welcome fields; reducers for `devices`,
  `machine_removed` (drop from `machines`), `peer_status`. `HubClient.createInvite(opts?: {ttlSecs?, addr?})`,
  `joinPeer(uri)`, `revokeDevice(deviceId)`. Tests for the reducers.
- Desktop panel "Machines" view: this machine first, then paired hubs (online dot, OS, last seen, addr), Revoke with an
  in-app confirm (no browser `confirm()`); "Pair a machine": Create invite -> URI + copy button + expiry countdown +
  the CLI line, and a Join field (paste a URI). Listener off -> show how to turn it on (`pawl serve --peer-listen
  <LAN-IP>` or config.toml `[peer] listen`). Friendly text for every error code. Sidebar shows "N machines · M online".
- VS Code: status-bar tooltip lists machines with online state; command "Pawl: Show Machines" (read-only QuickPick).
  No pairing UI in VS Code.
- Remote pets and sessions are not shown yet (step 3).

## Step 3 — Mirror events + visitors (PLAN.md migration step 3) — protocol v4

Branch `step-3-mirror-visitors`. Protocol already updated by the orchestrator (PROTOCOL_VERSION = 4):
`Machine.userActiveAt?`, `USER_ACTIVITY_REPORT_MS` (10 s), `USER_ACTIVE_WINDOW_MS` (10 min), ClientMsg
`user_activity{}` -> `{}`, `MirrorUpdate`, PeerBody `mirror{update}`, `sync_request{sinceSeq}`,
`mirror_snapshot{sessions,pets,pendingPermissions}`, `mirror_events{events}`, `sync_done{seq,truncated}`,
`MIRROR_PAYLOAD_MAX_BYTES` (64 KiB), `MIRROR_SYNC_MAX_EVENTS` (2000), `MIRROR_FRAME_MAX_BYTES` (200 KiB),
`TruncatedPayload` / `isTruncatedPayload` (Rust `truncated_payload(bytes)`). Welcome's sessions / pets /
pendingPermissions now cover every machine. Read the doc comments there; they are the spec.

Scope: owners stream their state to paired hubs; each hub keeps a **read-only** mirror and shows it to its local
clients; visitors area; "runs to you"; notify only where the user is. **Not** in step 3: acting on a remote session
or approval (step 4 — remote items stay read-only and every ClientMsg naming a remote id still gets `not_owner`),
repos (step 5), relay (step 6). Peer frames still never reach ClientMsg handling.

### Ownership (step 3)
| Role | Owns |
|---|---|
| sde-peer | `crates/peer/` |
| sde-hubcore | `crates/hub/` (+ `crates/brain/` for the status-reply change below) |
| fe-apps | `packages/hub-client/`, `packages/pet-engine/`, `apps/desktop/`, `apps/vscode/` |

No new dependencies expected; ask t13 first if one seems needed.

### Peer layer (sde-peer)
- Deliver every authenticated body other than pair/hello/presence/bye to the hub as
  `PeerEvent::Body { device_id: String, body: PeerBody }`, in arrival order per link.
- `PeerNode::send(device_id, body) -> Result<(), SendError>` (replaces the bool): `NotLinked`, `TooLarge` (serialized
  body > MIRROR_FRAME_MAX_BYTES + 16 KiB headroom, never sent), `Backpressure`. Per-link outbound queue bounded
  (1024 frames); on overflow close the link (the peer resyncs on reconnect) and return `Backpressure`.
- `PeerEvent::LinkUp` must fire before any `Body` from that link.

### Mirror protocol (sde-hubcore)
- **Outbound (owner):** every local `ServerMsg` of type session / event / pet / permission_request /
  permission_resolved / notification for an item with this hub's machineId is also sent to every linked peer as
  `mirror{update}`. Mirrored items are never re-forwarded (no relaying of a third machine's data). Event payloads whose
  JSON exceeds MIRROR_PAYLOAD_MAX_BYTES are replaced by `truncated_payload(bytes)`.
- **Sync on link-up:** right after LinkUp each side sends `sync_request{sinceSeq}` = highest event seq it has mirrored
  from that peer (0 if none). The owner answers `mirror_snapshot` (open sessions + sessions ended in the last 7 days,
  max 200; all pets; all pending permissions), then `mirror_events` batches (events with seq > sinceSeq, newest
  MIRROR_SYNC_MAX_EVENTS, sent oldest first, each body < MIRROR_FRAME_MAX_BYTES), then `sync_done`. Live `mirror`
  updates may interleave; apply idempotently (events keyed by global id, upserts by id).
- **Inbound validation (security):** drop (and log at warn with the device id, never the payload) any item whose
  machineId, or the machine prefix of its id / sessionId / requestId, is not the sender's device id. Drop events whose
  id is not `${machineId}:${seq}`. Never let a peer write a row owned by this hub or a third machine. Cap mirror storage
  per peer (max 500 sessions, 2000 events per session, oldest pruned).
- **Storage:** mirror rows live in the normal tables with their machine_id (schema v4 if columns are needed, e.g.
  `mirrored` flag or per-peer sync cursor). `mirror_snapshot` replaces that peer's sessions/pets/pending set. Revoke
  (either side) deletes every mirrored row of that machine. Prune ended mirrored sessions older than 7 days.
- **Local clients:** mirrored items are broadcast as ordinary ServerMsgs (`session`, `event`, `pet`,
  `permission_request`, `permission_resolved`, `notification`) and included in Welcome. `get_events` /
  `list_sessions` serve mirrored sessions from the mirror. Rules, watchdog and auto-approve never run on mirrored items.
  When a peer goes offline its mirror stays (clients grey it out); nothing is deleted.
- **Activity:** `user_activity` stamps this machine's `userActiveAt = now`; presence carries it (heartbeat, plus an
  immediate presence when the user becomes active after >= 30 s idle). Peers' presence updates their Machine. Broadcast
  `ServerMsg::Machine` when userActiveAt moves by >= 30 s or the active machine changes (not every report).
- **Brain:** status replies include mirrored sessions, naming the machine ("Claude on Mac mini is working on ...").
  Commands that would act on a remote session answer that remote control arrives later.

### Clients (fe-apps)
- **Remove the step-2 client filters** (`useLocalHubState`, `HubConnection.local`, `isRemote` / `remote()` guards);
  show every machine's data, filtered by the rules below.
- **hub-client:** `activeMachineId(state, now): MachineId` (online machine with the latest userActiveAt within
  USER_ACTIVE_WINDOW_MS; else this machine); `isLocal(state, id)`; `reportUserActivity()` throttled to
  USER_ACTIVITY_REPORT_MS. Move the duplicated desktop/VS Code helpers into hub-client (`petFor`, `agentFor`,
  rename-error text, `SessionBackfill`, `petNames`) and delete the copies.
- **pet-engine:** `PetView` gains `visitor: boolean` (pet's machine is not this one), `away?: MachineId` (this pet has
  a pending approval but the user is active on another machine: it "ran to" that machine) and `remoteOffline: boolean`.
  `derivePetViews` input gains `localMachineId` and `activeMachineId`. Visitors are only produced for online machines.
  Rules on local machine L with active machine A: a home pet with a pending approval gets `away = A` when A != L; a
  visitor with a pending approval shows its request bubble only when A == L (it "ran here"), otherwise it is a plain
  visitor in the waiting pose.
- **Desktop strip:** home pets as today; a visitors area on the right (machine badge "Biscuit · Mac mini", max 6,
  then "+N"); a visitor with a pending approval runs in from the screen edge with a read-only bubble ("needs approval
  on Mac mini" — approving here is step 4; click opens the panel at that request). A local pet whose approval is
  `away` walks off-screen and returns when this machine becomes active again. While any approval is pending, the strip
  follows the monitor under the cursor (primary monitor otherwise).
- **Desktop panel + VS Code:** sessions grouped by machine; remote sessions and transcripts read-only (prompt, cancel,
  approve disabled with "Open on <machine> to act — remote control arrives in a later update"); offline machines'
  items greyed with "last seen"; `TruncatedPayload` renders as "Large output — open on <machine>".
- **Notifications:** OS / VS Code notifications only on the active machine (`activeMachineId === local`), for every
  machine's events, so the user is told once, where they are.
- **Activity reporting:** desktop reports on global cursor movement (pet_window already polls it) and panel input;
  VS Code on window focus, editor edits and selection changes. Both throttled.

## Step 4 — Intents + acks (PLAN.md migration step 4) — protocol v5

Branch `step-4-intents`. Protocol already updated by the orchestrator (PROTOCOL_VERSION = 5): `Intent`
(approve / deny / prompt / start / cancel), `IntentKind`, `IntentRejectReason`, `INTENT_TIMEOUT_MS` (15 s),
`INTENT_RATE_PER_SEC` (20), PeerBody `intent{intentId,client,confirmed,intent,ts}` and
`intent_ack{intentId,status,reason?,result?}` (Rust `IntentStatus`), `resolve_permission.confirmed?`,
`start_session.machineId?` + `confirmed?`, `list_agents.machineId?`, `permission_resolved.byMachineId?` (ServerMsg and
MirrorUpdate). Read the doc comments there; they are the spec.

Scope: act on another machine's session from here. The owner is the source of truth; the origin hub only forwards.
**Not** in step 4: bot routing of remote commands by machine/repo/pet name (step 5), relay (step 6), phone (step 7).
`crates/peer` needs no change (intents are ordinary bodies).

### Ownership (step 4)
| Role | Owns |
|---|---|
| sde-hubcore | `crates/hub/`, `crates/brain/` |
| fe-apps | `packages/hub-client/`, `packages/pet-engine/`, `apps/desktop/`, `apps/vscode/` |

No new dependencies expected; ask t13 first.

### Origin hub (the one whose client asked)
- A ClientMsg naming another machine's id (resolve_permission, prompt, cancel by id prefix; start_session by
  `machineId`) is no longer `not_owner`: if that machine is paired and linked, send `intent` and wait for
  `intent_ack` (INTENT_TIMEOUT_MS), then ack the client with the owner's result or reason. Not linked -> `offline`;
  no ack in time -> `timeout` (the outcome may still arrive through the mirror). Unknown / unpaired machine ->
  `not_found`. A third machine's data is never forwarded on someone else's behalf: the target is always the id's owner.
- `confirmed` is required, and checked on the origin before sending, for: resolve_permission with an allow_* option,
  and start_session on another machine. Missing -> `unconfirmed` without sending. Deny, prompt and cancel need none.
- Audit (origin): every forwarded approve/deny is logged with intentId, target machine, client kind and the owner's
  answer (or offline/timeout).

### Owner hub (the one that has the session)
- Accept `intent` only from a paired, linked hub; dedupe by intentId (keep the last 1000 acks for 10 min; a repeat
  gets the same ack and changes nothing). More than INTENT_RATE_PER_SEC per peer -> `rate_limited`.
  `[peer] accept_intents = false` in config.toml -> every intent `forbidden` (default true).
- Validate against own state: target id has this hub's prefix (else `not_owner`); the request or session exists
  (`not_found`); a request that existed but is no longer pending -> `already_decided` (keep resolved request ids
  long enough to tell, e.g. the audit log); approve needs an allow_* option of that request and deny a reject_* option
  or null (`invalid`); approve and start need `confirmed` (`unconfirmed`); start: agent installed here, cwd absolute,
  existing directory (`invalid`), prompt length limits as for local.
- **Review hardening (owner):** a remote approve may only pick an `allow_once` option (`allow_always` -> `forbidden`;
  choose it on the owner). Remote start canonicalizes cwd, refuses UNC, `\?\UNC\` and device paths, and requires a
  folder the owner already knows (canonical cwd of a previous local session, or under `[peer] start_roots`), else
  `forbidden`. Sessions started or prompted by a peer are marked remote-driven and rules never auto-approve their
  requests. The owner cannot verify the origin's UI, so `confirmed` is a statement by a paired hub; pairing is the
  trust decision (`[peer] accept_intents = false` turns all of this off).
- **First valid decision wins**: resolving is atomic with the pending check, shared with the local path, so a local
  click and a remote intent racing for the same request give exactly one `applied`; the loser gets `already_decided`
  (local ClientMsg path too: it now answers `already_decided` instead of `not_found` for a request that was decided).
- Apply through the same code path as a local ClientMsg (rules never re-run; the user decided). The resulting
  `permission_resolved` carries `by: "user"` and `byMachineId` = origin. `start` runs the agent on this machine only.
- Audit (owner): every remote approve/deny with intentId, origin machine id + name, client kind, option, outcome.
  Remote prompt/start/cancel are logged too (kind, origin, target, outcome; prompt text is not logged).
- Notification on the owner when a remote approval was applied ("Approved from Mac mini: run `bun test`").
- `list_agents{machineId}` for a peer returns the agents from its latest presence (offline -> last known).
- Brain: chat "approve"/"deny" for a remote request answers with how to do it in the panel (confirmation needed);
  it does not forward. Status replies say who decided ("approved from Mac mini").

### Clients (fe-apps)
- Enable approve / deny / prompt / cancel on remote items (they were read-only). Remote **approve** always shows a
  confirmation step on this device first ("Approve on Mac mini: run `bun test`?" with the machine name prominent),
  then sends `confirmed: true`; deny/prompt/cancel need none. Desktop: inline confirm in the bubble and the panel (no
  window.confirm); VS Code: modal `showWarningMessage(..., {modal: true}, "Approve on Mac mini")`.
- Results: `already_decided` -> "Already answered" and close the bubble (the mirror closes it anyway);
  `offline` -> "Mac mini is offline"; `timeout` -> "No answer from Mac mini yet — check that machine";
  `unconfirmed` / `forbidden` / `rate_limited` / `invalid` -> clear text. Keep the request visible until the
  mirror's permission_resolved arrives.
- `permission_resolved.byMachineId` -> "Approved from Windows PC" in history/transcript and in notifications.
- New session: machine picker (this machine default; online paired machines), agents from `list_agents{machineId}`,
  cwd input with suggestions from that machine's recent session cwds; remote start needs the confirm step and shows
  "Runs on Mac mini". VS Code "New session" gets the same picker as a QuickPick.
- hub-client: `resolvePermission(requestId, optionId, {confirmed})`, `startSession({..., machineId, confirmed})`,
  `listAgents({machineId})`, `needsConfirmation(state, request, optionId)` helper, error-text helper for the new codes.
- CLI (sde-hubcore): `pawl start <agent> --cwd <dir> --machine <name|id> [--yes]` asks "Start claude-code on
  Mac mini in <cwd>? [y/N]" unless --yes; `pawl approve <requestId> [--option <id>] [--yes]` and `pawl deny <requestId>`
  work for local and remote requests. A remote approve always asks interactively on the terminal (it needs a TTY;
  `--yes` is NOT honoured for remote approvals, so a script cannot approve on another machine).

## Step 5 — Remote commands by machine + repo (PLAN.md migration step 5) — protocol v6

Branch `step-5-remote-commands`. Protocol already updated by the orchestrator (PROTOCOL_VERSION = 6): `RepoRef`
(+ `lastUsedAt`, doc = the repo index), `REPO_INDEX_MAX` (200), `ChatProposal` / `ChatProposalKind` (Rust),
`ChatMessage.proposals`, ClientMsg `list_repos{machineId?}` -> `{repos}`. Read the doc comments; they are the spec.

Goal: "Tell the Mac mini's Claude to run the tests in repo X" typed on the PC does the right thing, and asks when
the target is ambiguous. **Security rule:** the bot never runs a command on another machine by itself. For a remote
target it replies with `proposals`; the user confirms in the client, which sends the ordinary `start_session` /
`prompt` ClientMsg (step-4 rules apply unchanged). Only the deterministic parser creates proposals or actions; LLM
output stays text. Local targets keep today's behaviour (BrainAction executed by the hub).

### Ownership (step 5)
| Role | Owns |
|---|---|
| sde-brain (new) | `crates/brain/` |
| sde-hubcore | `crates/hub/` |
| fe-apps | `packages/hub-client/`, `packages/pet-engine/`, `apps/desktop/`, `apps/vscode/` |

No new dependencies expected; ask t13 first.

### Brain API (sde-brain implements; sde-hubcore codes against it)
```rust
pub struct BrainContext {
    pub machine: Machine, pub machines: Vec<Machine>,
    pub agents: Vec<Agent>,                                  // this machine (unchanged)
    pub remote_agents: BTreeMap<MachineId, Vec<Agent>>,      // NEW: paired machines' agents from presence
    pub repos: BTreeMap<MachineId, Vec<RepoRef>>,            // NEW: repo index per machine, this one included
    pub pets: Vec<Pet>,                                      // NEW: every machine's pets (for "tell Biscuit to ...")
    pub sessions: Vec<Session>, pub recent_events: Vec<HubEvent>, pub pending: Vec<PermissionRequest>,
    pub pet: Option<Pet>,                                    // the pet being chatted with (unchanged)
}
pub struct BrainReply {
    pub text: String,
    pub action: Option<BrainAction>,     // local targets only (unchanged)
    pub proposals: Vec<ChatProposal>,    // NEW: remote targets only; ids unique (uuid v4)
}
```
Parsing (deterministic, case-insensitive, tolerant of "the", "'s", punctuation):
- Machine: its display name or a unique prefix ("mac mini", "the mac mini's", "on windows pc"); "here"/"this
  machine" = local.
- Pet: any known pet name ("tell Biscuit to ...", "Biscuit, ..."). A pet fixes machine + agent.
- Agent: id, name or common alias (claude / claude code, codex, gemini, copilot).
- Repo: "in repo X", "in the X repo", "in X" / "for X" where X matches a repo name (exact, then unique prefix) in the
  candidate machines' indexes.
- Task text: the rest after "to" / ":" ("run the tests"). Empty task -> start without a prompt (like today).
Resolution: pet -> machine+agent; else explicit machine; else infer: the repo exists on exactly one machine, or the
agent has a session in that repo on exactly one machine. Agent missing -> the machine's only installed agent, else
ask. Repo missing -> that agent's most recent session cwd on that machine, else ask. Several candidates -> reply with
one proposal per candidate (max 5) and a question. Nothing matches -> say what is known ("Mac mini has: api, web").
Offline machine -> say so (with last seen), no proposal. Agent not installed there -> say so.
Prompt vs start: if that agent has an open **owned** session (not ended, status idle/done, not waiting on a
permission) in that repo on that machine -> `prompt` proposal for it; else `start`. Hook sessions are never prompted.
Summary text names everything: "Start Claude Code on Mac mini in ai-pet: run the tests" /
"Send to Claude Code on Mac mini (ai-pet): run the tests".
Status questions with targets work too ("what is the Mac mini's Claude doing?").
Tests: the example sentence, pet addressing, ambiguity (two machines with repo X), offline, unknown repo, prompt vs
start, local target still yields a BrainAction, and that no LLM path can produce a proposal or action.

### Hub (sde-hubcore)
- Repo index: canonical cwds of local sessions (owned + hook) with lastUsedAt, plus config.toml `[repos] paths = [...]`;
  drop folders that no longer exist; unique names per machine ("name (parent)"); newest REPO_INDEX_MAX. Rebuild on a
  new local session cwd, on config load and hourly. The step-4 "known folder" check for remote start uses the same
  index (+ `[peer] start_roots`), one function.
- Presence carries `repos` (was []). Inbound peer repos validated: <= REPO_INDEX_MAX, name <= 80 chars, path <= 1024,
  no control chars; stored per peer (persist like agents).
- `list_repos{machineId?}`; BrainContext wiring (remote_agents, repos, pets of all machines); chat replies store and
  broadcast `ChatMessage.proposals`.
- CLI: `pawl repos [--machine <name|id>] [--json]`; `pawl start <agent> --machine X --repo <name>` (repo name ->
  path via that machine's index; ambiguous/unknown -> error listing names); `pawl chat "<text>"` prints the reply
  and numbered proposals, `--pick N` executes proposal N through the normal path (remote start still asks on the TTY).

### Clients (fe-apps)
- hub-client: `listRepos({machineId})`, `executeProposal(p, {confirmed})` (maps to start_session / prompt; start needs
  confirmed, checked before sending), tests.
- Desktop chat view and VS Code chat: render proposals as buttons with the summary; click -> confirm step on this
  device ("Run on Mac mini?") -> execute; show the result inline (started / sent / error text from intentErrorText).
  Several proposals = choose one. A proposal is single-use in the UI (disable after use) and expires after 10 min.
- New session view: repo picker from `listRepos({machineId})` (name + path), replacing recent-cwd suggestions.

## Step 6 — Relay (PLAN.md migration step 6)

Types already in both protocol files ("relay (migration step 6)"): `RelayServerFrame`, `RelayClientFrame`,
`RelayErrorCode`, `RELAY_*` constants. They are the spec; this section adds behaviour. Supersedes the old
"Relay (wave 2)" section above.

Goal: paired devices reach each other when they are not on the same LAN. Every hub connects **outbound** to the relay;
the LAN link stays the preferred fast path with relay fallback. The relay sees public keys and ciphertext only.

### Ownership (step 6, phase A — runs in parallel with steps 5 / 5b)
| Role | Owns |
|---|---|
| sde-relay (new) | `services/relay/` (`@pawl/relay`, Cloudflare Worker + Durable Objects) |
| sde-peer | `crates/peer/` (relay transport) |
Phase B (after 5b): sde-hubcore wires `[relay]` settings + status into the hub; fe-apps adds the relay settings UI.

Dependencies approved by the user ("complete all steps, use recommended choices"): `wrangler` and
`@cloudflare/workers-types` (dev, services/relay), `tweetnacl` (services/relay runtime; already used by apps/mobile),
`tokio-tungstenite` feature `rustls-tls-webpki-roots` (crates/peer, for wss://). Nothing else without asking t13.

### Relay service (sde-relay)
- Worker `GET /v1/connect?pk=<b64url pk>` (WebSocket upgrade only; anything else 404, `/health` -> `{ok:true}`):
  validate pk (43 chars base64url -> 32 bytes) and route to Durable Object `Mailbox` `idFromName(pk)`. Use the
  WebSocket Hibernation API. One live socket per mailbox (a new authenticated one replaces the old).
- Auth: on connect send `challenge{nonce (24 random bytes b64), relayPk (fresh X25519 per connection), v:1}`. The device
  answers `auth{box}` within 10 s; open with tweetnacl `nacl.box.open(box, nonce, devicePk, relaySk)`; plaintext must
  equal `${RELAY_AUTH_CONTEXT}:${nonce}:${host}` (host = request Host header). Else `error{bad_auth}` + close. Then
  `ok{queued}` and flush the queue. Nothing but `auth` is accepted before that.
- `allow{pks}`: replace the allowed-sender set (<= RELAY_MAX_ALLOWED, each a valid pk); persisted in DO storage.
  `allow_pairing{token, exp}`: one-shot slot (token 16 bytes b64url, exp <= now + 1 h, max 4 live, persisted).
- `send{to, env, pairing?}`: frame <= RELAY_FRAME_MAX_BYTES (else `too_large`); per-socket rate limit (e.g. 50
  frames/s, burst 200 -> `rate_limited`); forward to the recipient DO, which delivers `msg{from: senderPk, env}` if the
  sender is allowed, or if `pairing` matches a live slot (consume it); else `error{not_allowed, to}` to the sender.
  Recipient offline -> queue (RELAY_QUEUE_MAX_FRAMES / _BYTES / _TTL_MS; oldest dropped first; alarm cleans expired;
  full -> `queue_full`). The relay never parses `env` beyond size and JSON shape.
- `register_push{expoToken}` (store; validate `ExponentPushToken[...]` shape): when a frame is queued for an offline
  mailbox with a token, send one content-free Expo push ("Your pet needs you") at most once per minute.
- `ping` -> `pong`. Idle sockets are fine (hibernation).
- No logging of envelopes, tokens or push tokens; log only counts and error codes.
- Tests (bun test): pure modules for frame validation, auth verify, allowlist / pairing slots, queue caps + TTL,
  rate limiter, with an in-memory storage fake. Smoke: `wrangler dev` locally + a bun script acting as two devices
  (auth, allow, send/deliver, not_allowed, queue while offline then flush, pairing slot one-shot, bad auth).
  **Do not deploy**; t13 deploys after review.

- **Encodings (fixed by sde-relay):** relay addresses (`?pk=`, `allow.pks`, `send.to`, `msg.from`) = base64url
  unpadded, 43 chars, canonical only. `challenge.nonce` (24 B), `challenge.relayPk` (32 B) and `auth.box` = standard
  base64, padded. Pairing token = base64url unpadded, 22 chars (16 B). Auth plaintext host = the connect request's
  Host header (e.g. `127.0.0.1:8787` under `wrangler dev`). Queue overflow drops the oldest frames to fit; the
  sender still gets `error{queue_full, to}` although its frame is queued. `pong.ts` = relay clock. Close codes: 1008
  bad_auth / auth timeout, 4000 replaced by a newer authenticated socket.

- **Access key + abuse limits (after security review):** the Worker requires `k=<RELAY_ACCESS_KEY>` on
  `/v1/connect` (constant-time compare, checked before `idFromName`; missing/wrong -> 401, no DO touched; secret unset ->
  refuse to serve with 503 unless `RELAY_OPEN = "1"` is set, documented as not recommended). Per-IP connect limit via a
  Workers Rate Limiting binding (e.g. 30/min). Per-recipient token bucket in `deliver` before the allowlist read (shed
  with `rate_limited`). Pending-socket eviction prefers sockets that have not sent `auth`. Unchanged `allow` /
  `register_push` / same pairing token writes are skipped; config frames budget 5/s per mailbox. Push only if the
  mailbox has authenticated and has a non-empty allowlist. `allow` changes drop queued frames from senders no longer
  allowed. Size is checked before a pairing slot is consumed. `wrangler.jsonc` sets observability and logpush off.
  Invite URIs carry `rk=<access key>` next to `relay=`; hub settings store it write-only (`relay.accessKey`).

- **Auth nonce domain (after the transport review):** the relay's challenge nonce has byte 15 >= 2; envelope nonces
  have 0/1 there, so the auth box (long-term key, relay-chosen relay_pk) can never share a (key, nonce) pair with an
  envelope. Clients refuse other nonces and any relay_pk equal to their own or a roster key. During a join with an
  adopted relay the client allows only the inviter and sends no hellos until pairing succeeds. Relay-origin pairing
  attempts share one limiter bucket plus a global cap. Hello / presence more than 1 h from local time are dropped.
  The relay access key travels in invite URIs: anyone who saw an invite can use the relay until the key is rotated.

### Relay transport (sde-peer, crates/peer)
- `PeerNode::set_relay(&self, url: Option<String>)` (runtime; None = off). Connect outbound to
  `{url}/v1/connect?pk=<own pk b64url>`, answer the challenge with `crypto_box` (same key as the LAN link), send
  `allow` with every non-revoked roster device's pk (resend on roster change), `allow_pairing` for every live invite.
  Reconnect with backoff (1 s -> 60 s, jitter). `relay_status() -> RelayStatus { url, connected, last_error }`
  and `PeerEvent::RelayStatus(RelayStatus)` on change.
- Logical links per peer: path LAN or relay. No LAN link -> send `hello` via the relay; the reply makes the link
  up on the relay path (LinkUp as today, plus `PeerEvent::LinkPath { device_id, path }`). A LAN link that comes up
  later takes over (prefer LAN); losing it falls back to the relay without a LinkDown if the relay path is up.
  Presence / mirror / intents / bye use the active path. Offline rules unchanged.
- Inbound `msg{from, env}`: `from` must equal the roster key of `env.from` (for pair_request: the envelope `pk`);
  then the normal open + seq + direction checks. Mismatch -> drop + warn (no payload).
- Invites: when the relay is connected, `create_invite` adds `relay=<percent-encoded url>&rt=<b64url 16 bytes>` to the
  URI and registers `allow_pairing{rt, exp}`; `addr` becomes optional when `relay` is present (a phone or a hub off
  the LAN has no addr). `join` tries the LAN addr first (3 s) and falls back to the relay with `pairing = rt`.
  `Invite::parse` accepts these params (still strict otherwise).
- Tests: an in-process mock relay (Rust, tokio-tungstenite server implementing the frames above) — auth, allow,
  relay-only pairing, link over relay, LAN takeover and fallback, from/env.from mismatch dropped, relay restart
  reconnect. Gate: cargo test -p pawl-peer + clippy -D warnings.

## Step 5b — Settings in the UI (PLAN.md migration step 5b) — protocol v7

Rule (PLAN.md Decisions): every setting can be changed in the desktop UI; CLI / config.toml are for initial setup.
Protocol already updated by the orchestrator (PROTOCOL_VERSION = 7): `HubSettings`, `HubSettingsPatch`,
`CustomAgentSetting`, `HookTargetName`, `HookStatus` (Rust `HookState`, `HookAction`), `PanelView`, ClientMsgs `get_settings` / `update_settings{patch}` /
`hook_status` / `install_hooks{target, action}` / `open_panel{view}`, ServerMsgs `settings` / `open_panel`.
Read the doc comments; they are the spec.

### Ownership (step 5b)
| Role | Owns |
|---|---|
| sde-hub (new, replaces sde-hubcore) | `crates/hub/`, `crates/discovery/` (hook status + remove) |
| fe-settings (new, replaces fe-apps) | `packages/hub-client/`, `packages/pet-engine/`, `apps/desktop/`, `apps/vscode/` |
(sde-peer adds `PeerNode::stop_listening()` in its step-6 pass; sde-hub uses it for the runtime listener toggle.)

Dependency approved by the user ("use recommended choices"): `toml_edit` in crates/hub (edit config.toml in place,
keeping comments). Nothing else without asking t13.

### Hub (sde-hub)
- `get_settings` returns the effective settings (config.toml + defaults; CLI flags that override a value are reported
  in `overriddenBy`). Secrets are never returned: only `anthropicApiKeySet: boolean`.
- `update_settings{patch}`: validate everything first (ports 1024-65535, durations > 0 and bounded, "HH:MM" or "",
  listen = IP literal[:port], URLs http(s) / ws(s), paths absolute + existing for start_roots / repos, custom agent
  id [a-z0-9-]{1,40} unique and not a built-in id, command non-empty argv, <= 16 custom agents). Invalid ->
  `ack error "invalid:<field>: <reason>"` and nothing changes (`<field>` = camelCase wire path of the patch, e.g.
  `hub.port`, `peer.startRoots[2]`, `customAgents[1].command`). Then write config.toml with `toml_edit` (keep comments
  and unknown keys; atomic write via temp file + rename) and apply at runtime where possible: peer listener start /
  stop / rebind, accept_intents, start_roots, repos paths (rebuild index), watchdog timings, bot model / keys / ollama,
  retention, daily report time, custom agents (re-run discovery), relay url (stored; wired in step 6). `port` needs a
  restart -> listed in `restartRequired`. Broadcast `ServerMsg settings` to all local clients after every change.
- Settings audit: every update logged with client kind, changed keys (never secret values) and time.
- Settings calls are local-only by construction (only local WebSocket clients send ClientMsgs); no peer body, intent,
  mirror or relay frame may ever reach them. Test that.
- `hook_status` / `install_hooks{target, action: "preview" | "apply" | "remove"}` reuse `pawl-discovery`'s hook
  install code (the same code as `pawl install-hooks`); preview returns the exact change without writing.
- `open_panel{view}` -> broadcast `ServerMsg open_panel{view}` (VS Code asks the desktop app to open a view).
- `restart_hub`: ack, spawn a replacement (`current_exe` with the same args, detached; it retries binding the port for
  ~10 s), send `bye{shutdown}` to peers, exit. The UI confirms first ("running sessions end").
- `data_dir/runtime.json` `{port, pid, startedAt}` is written at startup (removed on clean exit). `pawl hook`, the
  desktop app and VS Code resolve the port from it first, then config.toml, so a saved-but-not-yet-applied port change
  never loses hook events.
- CLI stays as is (initial setup); add `pawl settings [--json]` (print effective settings, secrets masked).
- Tests: validation table, toml_edit round-trip keeps comments, runtime apply (listener on/off, accept_intents,
  start_roots), restartRequired for port, audit rows without secret values, hooks preview does not write.

### Clients (fe-settings)
- hub-client: `getSettings()`, `updateSettings(patch)`, `hookStatus()`, `installHooks(target, action)`,
  `openPanel(view)`; HubState.settings updated from `settings` msgs; error parsing for `invalid:<field>`.
- Desktop Settings view, sections: Hub (port, offline, retention, idle timeouts), This machine (name — exists),
  Machines & pairing (link to Machines view + peer listener on/off and address), Remote control (accept_intents,
  start_roots list), Repos (paths list + the live repo index), Agents & hooks (per-agent hook status with
  preview -> apply / remove, custom agents list add/edit/remove), Bot (model, Anthropic key write-only with
  set/clear, Ollama URL/model), Notifications (client-side, exists), Relay (url; status arrives in step 6).
  Field-level errors from `invalid:<field>`; "Restart the hub to apply" banner for restartRequired.
- In-app confirms (never window.confirm) that say what the change allows: turning the peer listener on, enabling
  remote control, adding a start root, adding or changing a custom agent command, removing hooks.
- First-run setup (desktop, shown once; reopen from Settings): hub running? (start it) -> name this machine ->
  install hooks for the agents found (preview + apply) -> optional: turn on the listener and pair a machine.
- VS Code: command "Pawl: Open Settings" -> `openPanel("settings")`; if no desktop app is connected, say
  "Open the Pawl desktop app to change settings". Desktop handles `open_panel` by showing the panel at that view.

## Step 7 — Phone (PLAN.md migration step 7) — protocol v8

Protocol already updated by the orchestrator (PROTOCOL_VERSION = 8): `MachineOs` gains `ios` / `android`;
PeerBody `roster_intro{devices: IntroDevice[]}` (the sender is the introducer); `Device.introducedBy?`; ClientMsg
`create_invite.forPhone?` (relay-only, QR-friendly; error `relay_off` when no relay is connected). Read the doc comments.

Goal: the phone pairs once by scanning a QR code on any hub, then reaches every paired hub over the relay, sees all
machines' sessions, pets and pending approvals, approves with biometrics, gets a content-free push when a pet needs
it, and can start / prompt on machine + repo (step-5 proposals or a picker).

### Ownership (step 7)
| Role | Owns |
|---|---|
| fe-mobile (new) | `apps/mobile/` (unfrozen) |
| sde-peer | `crates/peer/` (roster introduction, phone peers, cross-language test vectors) |
| sde-hub | `crates/hub/` (phones in the roster, mirror + intents for phones, roster intro, QR invites) |
| fe-settings | desktop + VS Code + hub-client (QR pairing screen, phones in the devices list) |

Dependencies approved by the user ("use recommended choices"): `qrcode` (+ `@types/qrcode` dev) in apps/desktop to draw
the pairing QR. apps/mobile already has expo-camera, expo-local-authentication, expo-notifications, expo-secure-store,
tweetnacl. Nothing else without asking t13.

### Pairing a phone
- Desktop "Pair a phone": `create_invite{forPhone: true}` -> relay-only invite (`relay=`, `rk=`, `rt=`, no `addr`), shown
  as a QR code plus the URI; expires like other invites; single use.
- The phone scans it, generates its long-term X25519 key (expo-secure-store; never leaves the device), a device id
  (12 chars [a-z0-9]), connects to the relay with `rk`, `allow`s the inviter pk, sends `pair_request` with
  `pairing = rt` (machine: id, name = device name, os ios/android). The hub adds it to the roster as `kind: phone`.
- **Roster introduction (group roster):** after pairing a new device, the hub sends `roster_intro` with that device to
  every other linked hub, and sends the new device a `roster_intro` listing the other hubs (id, kind, name, pk, addr?).
  Receivers accept `roster_intro` only from a paired hub, never let it replace an existing device's key, notify the
  user ("Mac mini added iPhone"), audit it, and add the devices with `introducedBy`. Revoking a device on one hub sends
  `bye{revoked}` to it and a `roster_intro`-style revocation is NOT propagated automatically (each hub keeps its own
  decision; the UI offers "revoke everywhere", which revokes locally and asks each linked hub through an intent-like
  request in a later step) — for step 7 revocation stays per hub.
- Phones never appear in `machines`; they are `devices` with kind `phone`.

### Phone as a peer
- Over the relay only. After hello the phone sends `sync_request` to every hub and keeps a mirror (reuse
  @pawl/hub-client reducers: MirrorUpdate variants match ServerMsg variants).
- Intents from a phone use `client: "mobile"`. Approve requires a successful biometric check
  (expo-local-authentication) on the phone before `confirmed: true`; no biometrics available -> device passcode; neither ->
  approvals disabled with an explanation. The step-4 owner rules apply unchanged.
- `register_push` with the Expo push token after pairing; the relay sends the content-free wake when a frame (e.g. a
  mirrored permission_request) is queued while the phone is offline.
- Presence: the phone sends hello on connect; hubs treat a phone link like any link (online / offline) but phones do
  not take part in "active machine" (step 3) and do not send repos / agents.

### Cross-language compatibility (sde-peer)
- Commit test vectors in `crates/peer/tests/vectors/` (JSON): fixed keys, envelopes for several bodies with known
  nonce/seq/direction, base64 / base64url samples, invite URIs (LAN, relay-only), relay auth plaintext + box. A Rust test
  regenerates/validates them; fe-mobile's bun tests must open/produce byte-identical results.

### Mobile app (fe-mobile)
- Screens (expo-router): Pair (scanner + paste URI), Machines (online, path), Sessions by machine (status, transcript
  read-only + prompt / cancel), Approvals (biometric approve / deny, "Approved from <device>"), Pets (names, activity),
  Start (machine -> repo -> agent, confirm), Settings (relay status, paired hubs, unpair this phone, push on/off).
- Libraries: peer envelope + nonce layout, seq persistence + replay guard, relay client (auth, allow, send, queue
  handling, reconnect), invite parsing, pairing flow, roster intro handling, per-hub mirror state.
- Tests (bun test, runnable without a device): vectors, envelope seal/open both ways, replay + direction rejection,
  invite parsing, relay auth, reducers, biometric gate logic (mocked). `bunx tsc --noEmit` and `bun test` must pass;
  `bunx expo export --platform android` (or `expo-doctor`) as a build check if it works offline.

### Step 7 clarifications (orchestrator)
- **Intro fan-out:** after pairing device N, the hub sends `roster_intro{[N]}` to every other linked paired hub, and to
  every linked paired phone when N is a hub. N gets `roster_intro` listing the other paired hubs, plus the paired phones
  when N is a hub. Phones are never introduced to phones. Offline recipients get the intro on their next hello.
- **Validation of a received intro:** sender must be a paired, linked device of kind `hub`; at most 32 devices; ids,
  names and addr checked like pairing; public key = 32 bytes, not low-order, not ours, not the sender's; an id or key
  that already exists with a different key / id is a conflict (skipped, audited, user notified), never a replacement;
  a revoked id or key is never re-added by an intro. Accepted devices get `introducedBy = sender`.
- **Setting:** `[peer] accept_roster_intros` (default true) = `HubSettings.peer.acceptRosterIntros` (protocol v8),
  editable in the desktop Settings view; when false, intros are audited and shown as a notification but not added.
- **Offline phone wake:** when a new local permission request appears and a paired phone is not linked, the hub queues
  one small envelope (a `presence` body) to the phone's relay mailbox so the relay sends the content-free push; at most
  one per phone per 60 s. The node exposes a "send through the relay mailbox while not linked" path for this only.
- **Phone retries:** a phone that just learned a hub from an intro may reach it before that hub has processed its own
  intro (relay `not_allowed`); the phone retries hello with backoff (1 s, 2 s, 5 s, then every 30 s).
- **Notifications:** intro accepted / conflict / ignored use `ServerMsg notification` (HubNotification) plus a `devices`
  broadcast; text like "Mac mini added iPhone".
