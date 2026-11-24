# AI Pet Hub — Contracts & Ownership (read before coding)

Plan: `PLAN.md`. This file fixes the interfaces so parallel engineers can build
against each other without waiting. **Code against these APIs exactly.** If one
is wrong or missing, tell the orchestrator (`hc send t13 "..."`); do not edit
another owner's files.

## Ground rules (all engineers)

- Repo root: `E:\workspace\ai-pet` (Windows; Git Bash + PowerShell available). Not a git repo.
- Stay inside your owned paths (table below). Root files (`Cargo.toml`,
  `package.json`, `tsconfig.base.json`, `docs/CONTRACTS.md`,
  `packages/protocol`, `crates/protocol`) are orchestrator-owned: read-only for you.
- Wire types: `packages/protocol/src/index.ts` (TS, source of truth) and
  `crates/protocol/src/lib.rs` (Rust mirror, crate `pet-protocol`). Import them; never redefine.
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
| sde-acp | `crates/acp/` | `pet-acp` |
| sde-hub | `crates/hub/` | `pet-hub` (bin + lib) |
| sde-discovery | `crates/discovery/` | `pet-discovery` |
| sde-brain | `crates/brain/` | `pet-brain` |
| sde-relay (wave 2) | `crates/relay-link/`, `services/relay/` | `pet-relay-link`, `@ai-pet/relay` |
| fe-engine | `packages/pet-engine/`, `packages/hub-client/` | `@ai-pet/pet-engine`, `@ai-pet/hub-client` |
| fe-desktop | `apps/desktop/` (incl. `src-tauri/`) | `@ai-pet/desktop` |
| fe-vscode | `apps/vscode/` | `ai-pet-vscode` |
| fe-mobile (wave 2) | `apps/mobile/` | `@ai-pet/mobile` |
| art-sprites | `assets/pets/`, `scripts/art/` | sprite sheets + manifest |

## Runtime facts

- Hub binds **only** `127.0.0.1:47800` (override `AI_PET_HUB_URL` / `--port`).
- Data dir: `pet_protocol::data_dir()` = `dirs::data_dir()/ai-pet-hub`
  (Windows `%APPDATA%\ai-pet-hub`, macOS `~/Library/Application Support/ai-pet-hub`,
  Linux `~/.local/share/ai-pet-hub`). Contains `hub.db` (SQLite), `token`,
  `config.toml`, `registry-cache.json`.
- `token`: 32 random bytes hex, created by the hub on first start. Clients read it
  (env `AI_PET_HUB_TOKEN` overrides).
- Endpoints (crate `pet-hub`):
  - `GET  /health` → `{"ok":true,"version":"x"}` (no auth)
  - `GET  /ws` → WebSocket. First frame `hello{token}`. Reject non-empty `Origin`
    headers other than `vscode-webview://*`, `tauri://localhost`, `http://tauri.localhost`,
    `https://tauri.localhost`, `http://localhost:1420` (vite dev). Browsers can reach localhost; this blocks drive-by pages.
  - `POST /hooks/{claude|codex|copilot}` with `Authorization: Bearer <token>`, body = raw hook JSON → `{"ok":true}`.
- Hook forwarding: agents' hook configs call `pet-hub hook <source>` which reads
  stdin JSON and POSTs it (fails silently in <2 s if the hub is down; never blocks the agent).

## Rust crate APIs

### `pet-acp` (sde-acp) — ACP client host

Uses the official `agent-client-protocol` crate (check docs.rs for the current API).
Spawns agents as stdio subprocesses. If the crate's futures are `!Send`, run each
connection on a dedicated thread with a `LocalSet` and talk via channels — the
public API below MUST be `Send + Sync + Clone`.

```rust
pub enum HostEvent {
    /// Upsert (status changes, title, ended).
    Session(pet_protocol::Session),
    /// Normalized ACP update for the event log. payload = protocol payload shapes
    /// (TextPayload, ToolCallPayload, PlanPayload, StopPayload, ErrorPayload).
    Event { session_id: SessionId, kind: pet_protocol::EventType, payload: serde_json::Value },
    /// Agent asked `session/request_permission`. Host waits for resolve_permission().
    PermissionRequest(pet_protocol::PermissionRequest),
    AgentCapabilities { agent_id: AgentId, capabilities: pet_protocol::AgentCapabilities },
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

### `pet-discovery` (sde-discovery) — agents + hooks

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
    /// Used by `pet-hub hook <source>`: read stdin JSON, POST to hub, 2s timeout, never error loudly.
    pub async fn forward(source: HookSource, raw: serde_json::Value, hub_url: &str, token: &str) -> anyhow::Result<()>;
    /// Config snippets/merges for: ~/.claude/settings.json, ~/.codex/config.toml (notify), ~/.copilot/hooks/ai-pet.json,
    /// .github/hooks/ai-pet.json. `apply=false` returns a dry-run diff text; `apply=true` merges (backup file first).
    pub fn install(target: HookTarget, hub_exe: &Path, apply: bool) -> anyhow::Result<String>;
    pub enum HookTarget { Claude, Codex, CopilotCli, CopilotVscode { repo: PathBuf } }
}
```

### `pet-brain` (sde-brain) — rules, bot, watchdog, pet growth

```rust
pub mod rules {
    pub fn default_rules() -> Vec<Rule>;          // auto_allow read/search/think; ask execute/delete/fetch/edit/move/other
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

### `pet-hub` (sde-hub) — the daemon

Modules: `config` (data dir, token, config.toml: port, custom agents, rules, brain), `store` (rusqlite bundled; tables agents, sessions, events, pets, rules, chat, audit_log), `bus` (tokio broadcast of `ServerMsg`), `server` (axum: /health, /ws, /hooks/:source), `core` (owns AcpHost, discovery results, rules, brain, watchdog; merges ACP + hook feeds into sessions/events; applies rules to permission requests; writes audit log for every approval), `notify` (desktop notifications are a client concern — hub only emits `notification` msgs).
CLI (`clap`): `pet-hub serve [--port]`, `agents [--refresh]`, `sessions`, `start <agent> --cwd <dir> [--prompt <text>]` (talks to a running hub over WS if up, else runs in-process and streams events to stdout), `hook <source>`, `install-hooks <claude|codex|copilot-cli|copilot-vscode> [--apply]`, `token`.
Relay integration is wave 2 (sde-relay will add it behind a `relay` module).

## TypeScript package APIs

### `@ai-pet/hub-client` (fe-engine) — works in browser, Tauri webview, VS Code webview and Node

```ts
import type { ClientMsg, ServerMsg, AckResults, ClientKind, Agent, Session, Pet, PermissionRequest, HubEvent, ChatMessage, HubNotification, Rule } from "@ai-pet/protocol";
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
// subpath "@ai-pet/hub-client/react"
export function useHubState(client: HubClient): HubState;
// subpath "@ai-pet/hub-client/node" (Node only; never import from browser code)
export interface HubCredentials { url: string /* ws */; httpUrl: string; token: string; tokenSource: "env" | "file"; dataDir: string; tokenFile: string }
export function readHubCredentials(opts?: HubCredentialsOptions): HubCredentials;   // sync; throws HubCredentialsError if no token
export function tryReadHubCredentials(opts?: HubCredentialsOptions): HubCredentials | null;
// Env AI_PET_HUB_TOKEN / AI_PET_HUB_URL (http or ws form) override. Extras (non-breaking): HubState.hubVersion/lastError,
// HubClient.setToken/listSessions/setRules/wsUrl; PetStage/PetBubble callbacks may return Promise (buttons disabled while pending).
```

### `@ai-pet/pet-engine` (fe-engine)

```ts
import type { Species, Session, HubEvent, PermissionRequest, Pet } from "@ai-pet/protocol";
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
// subpath "@ai-pet/pet-engine/react" — pure SVG + CSS keyframes, no assets needed (Rive later)
export function PetSprite(props: { species: Species; activity: PetActivity; size?: number; facing?: "left" | "right"; walking?: boolean; skin?: string }): JSX.Element;
export function PetBubble(props: { view: PetView; onApprove?: () => void; onDeny?: () => void; onClick?: () => void }): JSX.Element | null;
export function PetStage(props: { views: PetView[]; width: number; onApprove(requestId: string): void; onDeny(requestId: string): void; onPetClick?(petId: string): void }): JSX.Element; // pets walk/idle along a strip
```
Both packages: `"type":"module"`, `main`/`types`/`exports` point at `src/*.ts(x)` (consumed as source by Vite/bun build/esbuild; no build step). Workspace deps use `"workspace:*"`. Peer dep `react >=18`.

## Relay (wave 2, sde-relay + fe-mobile)

- Crypto: NaCl box (X25519 + XSalsa20-Poly1305, 24-byte nonce). Rust `crypto_box::SalsaBox`; TS `tweetnacl` `nacl.box`. Base64 (standard) on the wire.
- Pairing QR: `aipet://pair?relay=<https-url>&hub=<hubId>&pk=<hubPkB64>&code=<8 chars>` (code one-time, 5 min).
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
