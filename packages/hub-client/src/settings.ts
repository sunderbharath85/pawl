// Hub settings (PLAN.md migration step 5b): pure helpers shared by the desktop Settings view, the first-run setup
// and VS Code. Field errors, client-side checks that mirror the hub's validation, patch diffing, the wording of
// in-app confirms for risky changes, and hook labels. No DOM, Node or React imports here.
import type { CustomAgentSetting, HookStatus, HookTargetName, HubSettings, HubSettingsPatch, PanelView } from "@krypto/protocol";

/** The first protocol version with `get_settings` / `update_settings` / `hook_status` / `install_hooks` / `open_panel`. */
export const SETTINGS_PROTOCOL_VERSION = 7;
/** At most this many custom agents (the hub refuses more). */
export const CUSTOM_AGENTS_MAX = 16;
/** Custom agent ids: lowercase letters, digits and dashes, 1–40 characters. */
export const CUSTOM_AGENT_ID_RE = /^[a-z0-9-]{1,40}$/;
export const PORT_MIN = 1024;
export const PORT_MAX = 65535;

// ---------------------------------------------------------------- field errors

/** A hub validation error (`invalid:<field>: <reason>`), with `field` as a camelCase wire path. */
export interface SettingsFieldError {
  /** e.g. "hub.port", "peer.startRoots[2]", "customAgents[1].command". */
  field: string;
  reason: string;
}

function camel(segment: string): string {
  return segment.replace(/_([a-z0-9])/g, (_m, c: string) => c.toUpperCase());
}

/**
 * Normalize a field path from the hub to the camelCase wire form used by the forms:
 * snake_case segments become camelCase and numeric segments become `[n]`
 * ("custom_agents.1.command" → "customAgents[1].command", "peer.start_roots[2]" → "peer.startRoots[2]").
 */
export function normalizeSettingsField(raw: string): string {
  const parts = raw
    .trim()
    .replace(/\[(\d+)\]/g, ".$1")
    .split(".")
    .filter((p) => p.length > 0);
  let out = "";
  for (const p of parts) {
    if (/^\d+$/.test(p)) out += `[${p}]`;
    else out += (out ? "." : "") + camel(p);
  }
  return out;
}

function codeOf(err: unknown): string {
  if (typeof err === "string") return err;
  if (err && typeof err === "object") {
    const e = err as { code?: unknown; field?: unknown; reason?: unknown };
    if (typeof e.field === "string" && typeof e.reason === "string") return `invalid:${e.field}: ${e.reason}`;
    if (typeof e.code === "string") return e.code;
  }
  return "";
}

/**
 * The field error in a hub error (`invalid:<field>: <reason>`), or null for any other error. Accepts the raw
 * ack error string, a `HubRequestError` (its `code`) or a `SettingsError` (its `field` / `reason`).
 */
export function parseSettingsError(err: unknown): SettingsFieldError | null {
  const code = codeOf(err);
  const m = /^invalid:\s*([A-Za-z_][\w.[\]-]*)\s*(?::\s*([\s\S]*))?$/.exec(code);
  if (!m) return null;
  const field = normalizeSettingsField(m[1]!);
  if (!field) return null;
  const reason = (m[2] ?? "").trim();
  return { field, reason: reason || "is not valid" };
}

/** True when an error about `errorField` belongs to the form field `key` (the field itself or an item of it). */
export function fieldMatches(errorField: string, key: string): boolean {
  return errorField === key || errorField.startsWith(`${key}[`) || errorField.startsWith(`${key}.`);
}

/** The list index in a field path such as "peer.startRoots[2]" (null when there is none). */
export function fieldIndex(errorField: string, key: string): number | null {
  if (!errorField.startsWith(`${key}[`)) return null;
  const m = /^\[(\d+)\]/.exec(errorField.slice(key.length));
  return m ? Number(m[1]) : null;
}

/** Human labels for settings fields (used in errors, audit lines and the restart banner). */
export const SETTINGS_FIELD_LABELS: Record<string, string> = {
  "hub.port": "Hub port",
  "hub.offline": "Offline mode",
  "hub.hookSessionStaleHours": "Hook session timeout",
  "hub.ownedSessionIdleHours": "Idle session timeout",
  "hub.eventRetentionDays": "Event history",
  "hub.dailyReportTime": "Daily report time",
  "bot.model": "Bot model",
  "bot.anthropicApiKey": "Anthropic API key",
  "bot.ollamaUrl": "Ollama URL",
  "bot.ollamaModel": "Ollama model",
  "watchdog.waitingNudgeAfterMs": "First nudge after",
  "watchdog.repeatEveryMs": "Repeat nudges every",
  "watchdog.tickSecs": "Watchdog check interval",
  "peer.listen": "Peer listener address",
  "peer.acceptIntents": "Remote control",
  "peer.startRoots": "Start folders",
  "repos.paths": "Repo folders",
  "relay.url": "Relay URL",
  "relay.accessKey": "Relay access key",
  customAgents: "Custom agents",
};

/** "Hub port", "Start folders (item 3)", or the raw path for an unknown field. */
export function settingsFieldLabel(field: string): string {
  const f = normalizeSettingsField(field);
  if (SETTINGS_FIELD_LABELS[f]) return SETTINGS_FIELD_LABELS[f]!;
  const m = /^(.*?)\[(\d+)\](?:\.(\w+))?$/.exec(f);
  if (m && SETTINGS_FIELD_LABELS[m[1]!]) {
    const item = `${SETTINGS_FIELD_LABELS[m[1]!]} (item ${Number(m[2]) + 1}${m[3] ? `, ${m[3]}` : ""})`;
    return item;
  }
  return f;
}

/** One line for a failed settings request (field errors name the field). */
export function settingsErrorText(err: unknown): string {
  const fe = parseSettingsError(err);
  if (fe) return `${settingsFieldLabel(fe.field)}: ${fe.reason}`;
  const code = codeOf(err);
  switch (code) {
    case "unknown_type":
    case "unknown_message":
    case "unsupported":
    case "bad_request":
      return "This hub is too old to change settings from here. Update krypto and restart the hub.";
    case "forbidden":
      return "Only apps on this machine can change its settings.";
    case "timeout":
      return "The hub did not answer in time. Check that it is still running, then try again.";
    case "disconnected":
    case "closed":
      return "Lost the connection to the hub. The change may not have been saved; reload the settings to check.";
    case "unauthorized":
      return "The hub did not accept this app's token.";
    case "invalid":
      return "The hub refused the change as invalid.";
    default:
      if (err instanceof Error && err.message && !/^hub request "/.test(err.message)) return err.message;
      return code ? `The hub refused the change (${code}).` : "The change could not be saved.";
  }
}

// ---------------------------------------------------------------- client-side checks (mirror the hub)

/** A check result: an error message, or null when fine. */
export type FieldProblem = string | null;

/** Parse a whole number from a form value ("" → null). */
export function parseWholeNumber(raw: string | number): number | null {
  if (typeof raw === "number") return Number.isInteger(raw) ? raw : null;
  const t = raw.trim();
  if (!/^\d+$/.test(t)) return null;
  return Number(t);
}

export function checkPort(raw: string | number): FieldProblem {
  const n = parseWholeNumber(raw);
  if (n === null) return "Enter a port number.";
  if (n < PORT_MIN || n > PORT_MAX) return `Use a port from ${PORT_MIN} to ${PORT_MAX}.`;
  return null;
}

/** A whole number in `min..max`. */
export function checkWholeNumber(raw: string | number, min: number, max: number, unit: string): FieldProblem {
  const n = parseWholeNumber(raw);
  if (n === null) return `Enter a whole number of ${unit}.`;
  if (n < min || n > max) return `Use ${min}–${max} ${unit}.`;
  return null;
}

/** A positive number (decimals allowed, e.g. minutes "1.5"), at most `max`. */
export function parsePositiveDecimal(raw: string): number | null {
  const t = raw.trim();
  if (!/^\d+(\.\d+)?$|^\.\d+$/.test(t)) return null;
  const n = Number(t);
  return Number.isFinite(n) && n > 0 ? n : null;
}

/** "HH:MM" (24 h) or "" for off. */
export function checkReportTime(raw: string): FieldProblem {
  const t = raw.trim();
  if (t === "") return null;
  const m = /^(\d{1,2}):(\d{2})$/.exec(t);
  if (!m || Number(m[1]) > 23 || Number(m[2]) > 59) return "Use a time like 18:00.";
  return null;
}

/** "HH:MM" padded ("9:05" → "09:05"); "" stays "". */
export function normalizeReportTime(raw: string): string {
  const t = raw.trim();
  const m = /^(\d{1,2}):(\d{2})$/.exec(t);
  return m ? `${m[1]!.padStart(2, "0")}:${m[2]}` : t;
}

function isIPv4(s: string): boolean {
  const parts = s.split(".");
  return parts.length === 4 && parts.every((p) => /^\d{1,3}$/.test(p) && Number(p) <= 255 && (p === "0" || !p.startsWith("0")));
}

function isIPv6(s: string): boolean {
  if (!/^[0-9a-fA-F:.]+$/.test(s) || !s.includes(":")) return false;
  const double = s.split("::");
  if (double.length > 2) return false;
  const groups = s.split(":").filter((g) => g !== "");
  const last = groups[groups.length - 1];
  const v4tail = last !== undefined && last.includes(".");
  if (v4tail && !isIPv4(last!)) return false;
  const hexGroups = v4tail ? groups.slice(0, -1) : groups;
  if (!hexGroups.every((g) => /^[0-9a-fA-F]{1,4}$/.test(g))) return false;
  const count = hexGroups.length + (v4tail ? 2 : 0);
  return double.length === 2 ? count < 8 : count === 8;
}

/** Split "ip[:port]" / "[v6]:port" / bare v6 into parts, or null when it is not an IP literal with an optional port. */
export function parseListenAddr(raw: string): { ip: string; port: number | null } | null {
  const t = raw.trim();
  if (!t) return null;
  let ip: string;
  let portText: string | null = null;
  if (t.startsWith("[")) {
    const end = t.indexOf("]");
    if (end < 0) return null;
    ip = t.slice(1, end);
    const rest = t.slice(end + 1);
    if (rest !== "") {
      if (!rest.startsWith(":")) return null;
      portText = rest.slice(1);
    }
    if (!isIPv6(ip)) return null;
  } else if ((t.match(/:/g) ?? []).length > 1) {
    ip = t; // bare IPv6, no port
    if (!isIPv6(ip)) return null;
  } else {
    const idx = t.indexOf(":");
    ip = idx >= 0 ? t.slice(0, idx) : t;
    portText = idx >= 0 ? t.slice(idx + 1) : null;
    if (!isIPv4(ip)) return null;
  }
  let port: number | null = null;
  if (portText !== null) {
    if (!/^\d{1,5}$/.test(portText)) return null;
    port = Number(portText);
    if (port < 1 || port > 65535) return null;
  }
  return { ip, port };
}

/** Peer listener address: an IP literal with an optional port (hostnames are not accepted by the hub). */
export function checkListenAddr(raw: string): FieldProblem {
  if (!raw.trim()) return "Enter this machine's network address, e.g. 192.168.1.20:47801.";
  const p = parseListenAddr(raw);
  if (!p) return "Use an IP address with an optional port, e.g. 192.168.1.20:47801 (names like my-pc.local are not accepted).";
  if (p.port !== null && p.port < PORT_MIN) return `Use a port from ${PORT_MIN} to ${PORT_MAX}.`;
  return null;
}

/** True for 0.0.0.0 / :: (the listener binds every network). */
export function isWildcardListen(raw: string | null | undefined): boolean {
  if (!raw) return false;
  const p = parseListenAddr(raw);
  if (!p) return false;
  return p.ip === "0.0.0.0" || /^[0:]+$/.test(p.ip);
}

/** True for loopback addresses (only this machine can connect). */
export function isLoopbackListen(raw: string | null | undefined): boolean {
  if (!raw) return false;
  const p = parseListenAddr(raw);
  if (!p) return false;
  return p.ip.startsWith("127.") || p.ip === "::1";
}

/** A URL with one of `schemes` (e.g. ["http", "https"]) and a host. */
export function checkUrl(raw: string, schemes: readonly string[]): FieldProblem {
  const t = raw.trim();
  const example = `${schemes[schemes.length - 1]}://…`;
  if (!t) return `Enter a URL (${example}).`;
  const m = /^([a-zA-Z][a-zA-Z0-9+.-]*):\/\/([^/?#\s]+)([^\s]*)$/.exec(t);
  if (!m) return `Enter a full URL starting with ${schemes.map((s) => `${s}://`).join(" or ")}.`;
  if (!schemes.includes(m[1]!.toLowerCase())) return `The URL must start with ${schemes.map((s) => `${s}://`).join(" or ")}.`;
  return null;
}

/** Absolute path on Windows ("C:\…", "C:/…", "\\server\share") or POSIX ("/…"). Existence is checked by the hub. */
export function isAbsolutePath(raw: string): boolean {
  const t = raw.trim();
  return /^[a-zA-Z]:[\\/]/.test(t) || /^\\\\[^\\]+\\[^\\]+/.test(t) || t.startsWith("/");
}

export function checkFolder(raw: string, existing: readonly string[] = []): FieldProblem {
  const t = raw.trim();
  if (!t) return "Enter a folder.";
  if (!isAbsolutePath(t)) return "Use a full folder path, e.g. E:\\work or /home/me/work.";
  const key = folderKey(t);
  if (existing.some((e) => folderKey(e) === key)) return "That folder is already in the list.";
  return null;
}

/** Compare key for folders: separators unified, trailing separator dropped, case-insensitive (Windows-style). */
export function folderKey(path: string): string {
  let p = path.trim().replace(/\\/g, "/");
  while (p.length > 1 && p.endsWith("/") && !/^[a-zA-Z]:\/$/.test(p)) p = p.slice(0, -1);
  return p.toLowerCase();
}

/**
 * Split a command line into argv. Double or single quotes group words; backslashes are literal (Windows paths),
 * except `\"` inside double quotes. Throws on an unclosed quote.
 */
export function parseCommandLine(line: string): string[] {
  const out: string[] = [];
  let cur = "";
  let inWord = false;
  let quote: '"' | "'" | null = null;
  for (let i = 0; i < line.length; i++) {
    const ch = line[i]!;
    if (quote) {
      if (ch === quote) {
        quote = null;
      } else if (quote === '"' && ch === "\\" && line[i + 1] === '"') {
        cur += '"';
        i++;
      } else {
        cur += ch;
      }
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
      inWord = true;
    } else if (/\s/.test(ch)) {
      if (inWord) {
        out.push(cur);
        cur = "";
        inWord = false;
      }
    } else {
      cur += ch;
      inWord = true;
    }
  }
  if (quote) throw new Error("unclosed quote");
  if (inWord) out.push(cur);
  return out;
}

/** argv → one editable line (quotes words with spaces or quotes). */
export function formatCommandLine(argv: readonly string[]): string {
  return argv
    .map((a) => {
      if (a !== "" && !/[\s"']/.test(a)) return a;
      if (!a.includes('"')) return `"${a}"`;
      if (!a.includes("'")) return `'${a}'`;
      return `"${a.replace(/"/g, '\\"')}"`;
    })
    .join(" ");
}

/** Per-field problems of one custom agent. */
export interface CustomAgentProblems {
  id: FieldProblem;
  name: FieldProblem;
  command: FieldProblem;
}

/**
 * Check one custom agent (`commandLine` is the edited text) against the others and the built-in agent ids.
 * `index` = its position in `all` (null for a new one).
 */
export function checkCustomAgent(
  draft: { id: string; name: string; commandLine: string },
  all: readonly CustomAgentSetting[],
  index: number | null,
  builtinIds: readonly string[],
): CustomAgentProblems {
  const id = draft.id.trim();
  let idProblem: FieldProblem = null;
  if (!id) idProblem = "Enter an id.";
  else if (!CUSTOM_AGENT_ID_RE.test(id)) idProblem = "Use 1–40 lowercase letters, digits or dashes.";
  else if (builtinIds.includes(id)) idProblem = "A built-in agent already uses this id.";
  else if (all.some((a, i) => i !== index && a.id === id)) idProblem = "Another custom agent already uses this id.";
  const nameProblem: FieldProblem = draft.name.trim() ? null : "Enter a name.";
  let commandProblem: FieldProblem = null;
  try {
    const argv = parseCommandLine(draft.commandLine);
    if (argv.length === 0 || !argv[0]!.trim()) commandProblem = "Enter the command that starts the agent's ACP server.";
  } catch {
    commandProblem = "A quote is not closed.";
  }
  return { id: idProblem, name: nameProblem, command: commandProblem };
}

// ---------------------------------------------------------------- patch diffing

function sameList(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((x, i) => x === b[i]);
}

function sameAgents(a: readonly CustomAgentSetting[], b: readonly CustomAgentSetting[]): boolean {
  return a.length === b.length && a.every((x, i) => x.id === b[i]!.id && x.name === b[i]!.name && sameList(x.command, b[i]!.command));
}

/** The editable (non-secret) settings: what a form holds. */
export type EditableSettings = Omit<HubSettings, "overriddenBy" | "bot" | "relay"> & {
  bot: Omit<HubSettings["bot"], "anthropicApiKeySet">;
  relay: Omit<HubSettings["relay"], "accessKeySet">;
};

/**
 * The minimal patch that turns `base` into `draft` (only changed keys; lists replace the whole list).
 * Secrets are never part of it: set them with `secretPatch`.
 */
export function settingsPatch(base: HubSettings, draft: EditableSettings): HubSettingsPatch {
  const patch: HubSettingsPatch = {};
  const hub: NonNullable<HubSettingsPatch["hub"]> = {};
  for (const k of ["port", "offline", "hookSessionStaleHours", "ownedSessionIdleHours", "eventRetentionDays", "dailyReportTime"] as const) {
    if (draft.hub[k] !== base.hub[k]) (hub as Record<string, unknown>)[k] = draft.hub[k];
  }
  if (Object.keys(hub).length) patch.hub = hub;

  const bot: NonNullable<HubSettingsPatch["bot"]> = {};
  if (draft.bot.model !== base.bot.model) bot.model = draft.bot.model;
  if (draft.bot.ollamaUrl !== base.bot.ollamaUrl) bot.ollamaUrl = draft.bot.ollamaUrl;
  if (draft.bot.ollamaModel !== base.bot.ollamaModel) bot.ollamaModel = draft.bot.ollamaModel;
  if (Object.keys(bot).length) patch.bot = bot;

  const watchdog: NonNullable<HubSettingsPatch["watchdog"]> = {};
  for (const k of ["waitingNudgeAfterMs", "repeatEveryMs", "tickSecs"] as const) {
    if (draft.watchdog[k] !== base.watchdog[k]) watchdog[k] = draft.watchdog[k];
  }
  if (Object.keys(watchdog).length) patch.watchdog = watchdog;

  const peer: NonNullable<HubSettingsPatch["peer"]> = {};
  if (draft.peer.listen !== base.peer.listen) peer.listen = draft.peer.listen;
  if (draft.peer.acceptIntents !== base.peer.acceptIntents) peer.acceptIntents = draft.peer.acceptIntents;
  if (!sameList(draft.peer.startRoots, base.peer.startRoots)) peer.startRoots = [...draft.peer.startRoots];
  if (Object.keys(peer).length) patch.peer = peer;

  if (!sameList(draft.repos.paths, base.repos.paths)) patch.repos = { paths: [...draft.repos.paths] };
  if (draft.relay.url !== base.relay.url) patch.relay = { url: draft.relay.url };
  if (!sameAgents(draft.customAgents, base.customAgents)) patch.customAgents = draft.customAgents.map((a) => ({ ...a, command: [...a.command] }));
  return patch;
}

/** The editable part of `settings` (a fresh copy for a form). */
export function editableSettings(s: HubSettings): EditableSettings {
  return {
    hub: { ...s.hub },
    bot: { model: s.bot.model, ollamaUrl: s.bot.ollamaUrl, ollamaModel: s.bot.ollamaModel },
    watchdog: { ...s.watchdog },
    peer: { listen: s.peer.listen, acceptIntents: s.peer.acceptIntents, startRoots: [...s.peer.startRoots] },
    repos: { paths: [...s.repos.paths] },
    relay: { url: s.relay.url },
    customAgents: s.customAgents.map((a) => ({ ...a, command: [...a.command] })),
  };
}

/** Write-only secrets. A string sets (or replaces) it, null clears it. */
export type SecretKey = "bot.anthropicApiKey" | "relay.accessKey";

export function secretPatch(key: SecretKey, value: string | null): HubSettingsPatch {
  const v = value === null ? null : value.trim();
  return key === "bot.anthropicApiKey" ? { bot: { anthropicApiKey: v } } : { relay: { accessKey: v } };
}

export function isEmptyPatch(p: HubSettingsPatch): boolean {
  return Object.values(p).every((v) => v === undefined || (typeof v === "object" && v !== null && !Array.isArray(v) && Object.keys(v).length === 0));
}

/** Changed keys of a patch as field paths ("hub.port", "customAgents"); secrets are named, never their values. */
export function patchKeys(p: HubSettingsPatch): string[] {
  const out: string[] = [];
  for (const [section, value] of Object.entries(p)) {
    if (value === undefined) continue;
    if (Array.isArray(value) || typeof value !== "object" || value === null) out.push(section);
    else for (const k of Object.keys(value)) out.push(`${section}.${k}`);
  }
  return out;
}

// ---------------------------------------------------------------- risky changes (in-app confirms)

/** One risky change: what the confirm says it allows. */
export interface RiskyChange {
  key: string;
  title: string;
  body: string;
  /** The go-ahead button ("Turn on listener"). */
  confirm: string;
}

export interface RiskContext {
  /** This machine's name ("Windows PC"). */
  machineName?: string | undefined;
  /** Remote control after the change (start roots only matter with it on). */
  acceptIntents?: boolean | undefined;
  /** This user's home folder, when known (it and every folder above it count as very broad). */
  homeDir?: string | null | undefined;
}

/**
 * Why a folder is too broad to hand to paired machines, or null when it is not: a drive or filesystem root, a
 * network share root, the folder that holds every user's home, a home folder, or (with `homeDir`) any folder
 * above this user's home. The hub refuses roots anyway; the rest is allowed after a warning.
 */
export function broadFolderReason(path: string, homeDir?: string | null): string | null {
  const k = folderKey(path);
  if (!k) return null;
  if (k === "/" || /^[a-z]:\/?$/.test(k)) return "a whole drive";
  if (/^\/\/[^/]+\/[^/]+$/.test(k)) return "a whole network share";
  if (homeDir) {
    const h = folderKey(homeDir);
    if (h === k) return "your home folder";
    if (h.startsWith(k.endsWith("/") ? k : `${k}/`)) return "a folder that contains your home folder";
  }
  if (/^[a-z]:\/users$/.test(k) || /^\/(home|users)$/.test(k) || k === "/var/home") return "the folder that holds every user's home";
  if (/^[a-z]:\/users\/[^/]+$/.test(k) || /^\/(home|users)\/[^/]+$/.test(k) || /^\/var\/home\/[^/]+$/.test(k) || k === "/root") {
    return "a home folder";
  }
  return null;
}

export function isBroadFolder(path: string, homeDir?: string | null): boolean {
  return broadFolderReason(path, homeDir) !== null;
}

/** The warning for a very broad start root or repo path (part of its confirm). */
export function broadFolderWarning(path: string, reason: string): string {
  return (
    `${path} is ${reason}: any paired machine could start agents anywhere under this folder, and those agents could ` +
    "read and change every file in it and run commands with your account. Prefer the project folders you actually work in."
  );
}

/**
 * The changes in `patch` (against `base`) that need an in-app confirm, each saying exactly what it allows:
 * turning the peer listener on (or moving it to another address), turning remote control on, adding start
 * folders, and adding a custom agent or changing its command. Empty = save without asking.
 */
export function riskyChanges(base: HubSettings, patch: HubSettingsPatch, ctx: RiskContext = {}): RiskyChange[] {
  const here = ctx.machineName ? ctx.machineName : "this machine";
  const out: RiskyChange[] = [];
  const listen = patch.peer?.listen;
  if (listen !== undefined && listen !== null && listen !== base.peer.listen) {
    const wide = isWildcardListen(listen);
    const loop = isLoopbackListen(listen);
    const scope = wide
      ? "every network this computer is connected to (all addresses)"
      : loop
        ? "this computer only (a loopback address, so other machines cannot reach it)"
        : `the network address ${listen}`;
    out.push({
      key: "peer.listen",
      title: base.peer.listen ? `Move the peer listener to ${listen}?` : `Turn on the peer listener at ${listen}?`,
      confirm: base.peer.listen ? "Move listener" : "Turn on listener",
      body:
        `The hub on ${here} will accept connections on ${scope}. ` +
        "That lets you create one-time pairing invites here. Only machines that pair with an invite can link to this hub; " +
        "any other connection is turned away before it can see anything. Your firewall may ask to allow krypto.",
    });
  }
  const acceptIntents = patch.peer?.acceptIntents ?? base.peer.acceptIntents;
  if (patch.peer?.acceptIntents === true && !base.peer.acceptIntents) {
    out.push({
      key: "peer.acceptIntents",
      title: `Let paired machines control ${here}?`,
      confirm: "Allow remote control",
      body:
        `Every machine paired with ${here} will be able to start agents here (in folders this machine's sessions already ` +
        "used and in your start folders), send prompts to its sessions, cancel them, and approve (once) or deny its " +
        "permission requests. Starting an agent and approving need a confirm on the machine the action comes from; " +
        "always-allow can only be chosen here.",
    });
  }
  if (patch.peer?.startRoots) {
    const before = new Set(base.peer.startRoots.map(folderKey));
    const added = patch.peer.startRoots.filter((r) => !before.has(folderKey(r)));
    for (const root of added) {
      const reason = broadFolderReason(root, ctx.homeDir);
      if (!reason) continue;
      out.push({
        key: `peer.startRoots.broad:${folderKey(root)}`,
        title: `Let paired machines start agents anywhere under ${root}?`,
        confirm: "Allow anyway",
        body: broadFolderWarning(root, reason) + (acceptIntents ? "" : " This takes effect once remote control is on."),
      });
    }
    const plain = added.filter((r) => !broadFolderReason(r, ctx.homeDir));
    if (plain.length > 0) {
      const list = plain.join(", ");
      out.push({
        key: "peer.startRoots",
        title: plain.length === 1 ? `Allow starting agents in ${plain[0]}?` : `Allow starting agents in ${plain.length} new folders?`,
        confirm: plain.length === 1 ? "Allow folder" : "Allow folders",
        body:
          `Paired machines will be able to start an agent session in ${plain.length === 1 ? "this folder" : list} and every folder inside it. ` +
          "That agent runs on this machine with your user account: it can read and change the files there and run commands, " +
          "asking for permission as its rules say." +
          (acceptIntents ? "" : " This takes effect once remote control is on."),
      });
    }
  }
  if (patch.repos?.paths) {
    const before = new Set(base.repos.paths.map(folderKey));
    for (const path of patch.repos.paths.filter((r) => !before.has(folderKey(r)))) {
      const reason = broadFolderReason(path, ctx.homeDir);
      if (!reason) continue;
      out.push({
        key: `repos.paths.broad:${folderKey(path)}`,
        title: `Add ${path} as a repo?`,
        confirm: "Add anyway",
        body:
          "Repos are folders paired machines may start agents in. " +
          broadFolderWarning(path, reason) +
          (acceptIntents ? "" : " This takes effect once remote control is on."),
      });
    }
  }
  if (patch.customAgents) {
    const byId = new Map(base.customAgents.map((a) => [a.id, a]));
    for (const a of patch.customAgents) {
      const prev = byId.get(a.id);
      if (prev && sameList(prev.command, a.command)) continue;
      const cmd = formatCommandLine(a.command);
      out.push({
        key: `customAgents.${a.id}`,
        title: prev ? `Change the command for ${a.name || a.id}?` : `Add the agent ${a.name || a.id}?`,
        confirm: prev ? "Change command" : "Add agent",
        body:
          `Every time a ${a.name || a.id} session starts, the hub runs \`${cmd}\` on ${here} with your user account's permissions` +
          (acceptIntents ? ", including sessions a paired machine starts here." : ".") +
          " Only add commands you trust.",
      });
    }
  }
  return out;
}

// ---------------------------------------------------------------- hooks

export const HOOK_TARGETS: readonly HookTargetName[] = ["claude", "codex", "copilot-cli", "copilot-vscode"];

export const HOOK_TARGET_LABEL: Record<HookTargetName, string> = {
  claude: "Claude Code",
  codex: "Codex",
  "copilot-cli": "Copilot CLI",
  "copilot-vscode": "Copilot in VS Code",
};

export const HOOK_STATE_LABEL: Record<HookStatus["state"], string> = {
  installed: "Installed",
  outdated: "Needs update",
  missing: "Not installed",
  unsupported: "Not supported",
};

/** "Claude Code" or "Copilot in VS Code (api)" for a per-repo hook. */
export function hookLabel(s: Pick<HookStatus, "target" | "repo">): string {
  const base = HOOK_TARGET_LABEL[s.target] ?? s.target;
  if (!s.repo) return base;
  const name = s.repo.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || s.repo;
  return `${base} (${name})`;
}

/** Stable key for a hook row (target + repo). */
export function hookKey(s: Pick<HookStatus, "target" | "repo">): string {
  return s.repo ? `${s.target}:${s.repo}` : s.target;
}

/** What the main button does for a hook: install, update, nothing (installed) or nothing possible. */
export function hookAction(s: Pick<HookStatus, "state">): "install" | "update" | null {
  return s.state === "missing" ? "install" : s.state === "outdated" ? "update" : null;
}

/** True when the hooks can be removed (they are there, current or not). */
export function hookRemovable(s: Pick<HookStatus, "state">): boolean {
  return s.state === "installed" || s.state === "outdated";
}

/** The confirm for removing hooks: what stops working. */
export function hookRemoveConfirm(s: Pick<HookStatus, "target" | "repo" | "configPath">): RiskyChange {
  const name = hookLabel(s);
  return {
    key: `hooks.${hookKey(s)}`,
    title: `Remove Krypto's hooks for ${name}?`,
    confirm: "Remove hooks",
    body:
      `Krypto's entries are taken out of ${s.configPath}. ${HOOK_TARGET_LABEL[s.target] ?? s.target} sessions you start outside ` +
      "Krypto (in a terminal or editor) then stop showing up here: no pet reacts to them, their permission prompts are not " +
      "shown or remembered, and paired machines no longer see them. Sessions Krypto starts itself are not affected.",
  };
}

// ---------------------------------------------------------------- restart + panel

/** "Restart the hub to apply: Hub port." (null when nothing waits for a restart). */
export function restartText(keys: readonly string[]): string | null {
  if (keys.length === 0) return null;
  const labels = [...new Set(keys.map(settingsFieldLabel))];
  return `Restart the hub to apply: ${labels.join(", ")}.`;
}

export const PANEL_VIEWS: readonly PanelView[] = ["home", "settings", "machines", "approvals", "pets", "rules", "chat", "setup"];

export function isPanelView(v: unknown): v is PanelView {
  return typeof v === "string" && (PANEL_VIEWS as readonly string[]).includes(v);
}
