// @pawl/hub-client/node — Node-only helpers (VS Code extension host, CLIs, tests).
// Never import this from browser/webview code: it uses node:fs/os/path.
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { HUB_DATA_DIR_NAME, HUB_DEFAULT_PORT, HUB_TOKEN_FILE, MACHINE_ID_RE } from "@pawl/protocol";
import { fetchHubHealth, type HubHealth } from "./health";
import { toHubHttpUrl, toHubWsUrl } from "./url";

export interface HubCredentials {
  /** WebSocket URL for `HubClient` (e.g. `ws://127.0.0.1:47800/ws`). */
  url: string;
  /** HTTP base for `/health` and `/hooks/*` (e.g. `http://127.0.0.1:47800`). */
  httpUrl: string;
  token: string;
  /** Where the token came from. */
  tokenSource: "env" | "file";
  /** Hub data dir (`<os data dir>/pawl`). */
  dataDir: string;
  /** Token file path (`<dataDir>/token`), even if the token came from env. */
  tokenFile: string;
  /** Where the URL came from: `PAWL_HUB_URL`, the verified running hub (runtime.json), config.toml (Settings), or the default. */
  urlSource?: "env" | "runtime" | "config" | "default";
}

export interface HubCredentialsOptions {
  /** Environment to read overrides from. Default `process.env`. */
  env?: Record<string, string | undefined>;
  /** Override OS detection (tests). Default `process.platform`. */
  platform?: string;
  /** Override the home directory (tests). Default `os.homedir()`. */
  homeDir?: string;
  /** Use this hub data dir instead of the OS default. */
  dataDir?: string;
}

export class HubCredentialsError extends Error {
  readonly tokenFile: string;
  constructor(message: string, tokenFile: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = "HubCredentialsError";
    this.tokenFile = tokenFile;
  }
}

/**
 * OS data dir, matching Rust `dirs::data_dir()`:
 * Windows `%APPDATA%`, macOS `~/Library/Application Support`, else `$XDG_DATA_HOME` or `~/.local/share`.
 */
export function osDataDir(opts: HubCredentialsOptions = {}): string {
  const env = opts.env ?? process.env;
  const platform = opts.platform ?? process.platform;
  const home = opts.homeDir ?? homedir();
  if (platform === "win32") {
    return nonEmpty(env["APPDATA"]) ?? join(home, "AppData", "Roaming");
  }
  if (platform === "darwin") {
    return join(home, "Library", "Application Support");
  }
  const xdg = nonEmpty(env["XDG_DATA_HOME"]);
  // XDG spec: relative paths are invalid and must be ignored.
  if (xdg && isAbsolutePath(xdg)) return xdg;
  return join(home, ".local", "share");
}

/** `<os data dir>/pawl` — holds `hub.db`, `token`, `config.toml`. */
export function hubDataDir(opts: HubCredentialsOptions = {}): string {
  return opts.dataDir ?? join(osDataDir(opts), HUB_DATA_DIR_NAME);
}

/** `<hub data dir>/token`. */
export function hubTokenFile(opts: HubCredentialsOptions = {}): string {
  return join(hubDataDir(opts), HUB_TOKEN_FILE);
}

/** `<hub data dir>/config.toml`. */
export function hubConfigFile(opts: HubCredentialsOptions = {}): string {
  return join(hubDataDir(opts), "config.toml");
}

/**
 * The top-level `port = N` of a hub config.toml (null when absent or not a valid port). Only keys before the
 * first `[table]` are top-level; comments and other keys are ignored.
 */
export function configPortFromToml(text: string): number | null {
  for (const rawLine of text.replace(/^\uFEFF/, "").split(/\r?\n/)) {
    const line = rawLine.trim();
    if (line.startsWith("[")) return null;
    const m = /^port\s*=\s*(\d+)\s*(?:#.*)?$/.exec(line);
    if (m) {
      const port = Number(m[1]);
      return port >= 1 && port <= 65535 ? port : null;
    }
  }
  return null;
}

/** The hub port configured in config.toml (Settings → Hub port), or null. Never throws. */
export function readConfigPort(opts: HubCredentialsOptions = {}): number | null {
  try {
    return configPortFromToml(readFileSync(hubConfigFile(opts), "utf8"));
  } catch {
    return null;
  }
}

/** `<hub data dir>/runtime.json`: `{port, pid, startedAt}` of the running hub (removed on a clean exit). */
export function hubRuntimeFile(opts: HubCredentialsOptions = {}): string {
  return join(hubDataDir(opts), "runtime.json");
}

/** The `port` of a runtime.json text (null when missing or not a valid port). */
export function runtimePortFromJson(text: string): number | null {
  try {
    const port = (JSON.parse(text.replace(/^\uFEFF/, "")) as { port?: unknown } | null)?.port;
    return typeof port === "number" && Number.isInteger(port) && port >= 1 && port <= 65535 ? port : null;
  } catch {
    return null;
  }
}

/** The port in data_dir/runtime.json, or null. Never throws. Unverified: see `resolveHubCredentials`. */
export function readRuntimePort(opts: HubCredentialsOptions = {}): number | null {
  try {
    return runtimePortFromJson(readFileSync(hubRuntimeFile(opts), "utf8"));
  } catch {
    return null;
  }
}

/** `<hub data dir>/machine.toml`: this machine's `id` and `name` (written by the hub on first start). */
export function hubMachineFile(opts: HubCredentialsOptions = {}): string {
  return join(hubDataDir(opts), "machine.toml");
}

/** The top-level `id = "…"` of a machine.toml text (a valid machine id), or null. */
export function machineIdFromToml(text: string): string | null {
  for (const rawLine of text.replace(/^\uFEFF/, "").split(/\r?\n/)) {
    const line = rawLine.trim();
    if (line.startsWith("[")) return null;
    const m = /^id\s*=\s*(?:"([^"]*)"|'([^']*)')\s*(?:#.*)?$/.exec(line);
    if (m) {
      const id = m[1] ?? m[2] ?? "";
      return MACHINE_ID_RE.test(id) ? id : null;
    }
  }
  return null;
}

/** This machine's id from machine.toml, or null. Never throws. */
export function readMachineId(opts: HubCredentialsOptions = {}): string | null {
  try {
    return machineIdFromToml(readFileSync(hubMachineFile(opts), "utf8"));
  } catch {
    return null;
  }
}

/**
 * Read the hub URL and token synchronously. `PAWL_HUB_TOKEN` / `PAWL_HUB_URL` env vars override the token
 * file and the URL; without `PAWL_HUB_URL` the port is the one in config.toml, else the default
 * `127.0.0.1:47800`. It never uses runtime.json (that port must be verified first: use `resolveHubCredentials`).
 * Throws `HubCredentialsError` when no token can be found (typically: the hub has never been started here).
 */
export function readHubCredentials(opts: HubCredentialsOptions = {}): HubCredentials {
  const env = opts.env ?? process.env;
  const dataDir = hubDataDir(opts);
  const tokenFile = join(dataDir, HUB_TOKEN_FILE);
  const envUrl = nonEmpty(env["PAWL_HUB_URL"]);
  const configPort = envUrl ? null : readConfigPort(opts);
  const rawUrl = envUrl ?? (configPort !== null && configPort !== HUB_DEFAULT_PORT ? `http://127.0.0.1:${configPort}` : undefined);
  const urlSource: HubCredentials["urlSource"] = envUrl ? "env" : configPort !== null ? "config" : "default";
  const url = toHubWsUrl(rawUrl);
  const httpUrl = toHubHttpUrl(rawUrl);

  const envToken = nonEmpty(env["PAWL_HUB_TOKEN"]);
  if (envToken) return { url, httpUrl, token: envToken, tokenSource: "env", dataDir, tokenFile, urlSource };

  let text: string;
  try {
    text = readFileSync(tokenFile, "utf8");
  } catch (err) {
    const code = (err as { code?: string }).code;
    const why = code === "ENOENT" ? "not found (has the hub been started?)" : `unreadable (${code ?? String(err)})`;
    throw new HubCredentialsError(`Pawl hub token file ${why}: ${tokenFile}`, tokenFile, { cause: err });
  }
  const token = text.replace(/^\uFEFF/, "").trim();
  if (!token) throw new HubCredentialsError(`Pawl hub token file is empty: ${tokenFile}`, tokenFile);
  return { url, httpUrl, token, tokenSource: "file", dataDir, tokenFile, urlSource };
}

export interface ResolveHubCredentialsOptions extends HubCredentialsOptions {
  /** `GET /health` (no auth). Default: `fetchHubHealth`. Tests pass a fake. */
  fetchHealth?: (httpUrl: string) => Promise<HubHealth | null>;
  /** Timeout of the runtime.json check. Default 800 ms (localhost). */
  timeoutMs?: number;
}

/**
 * The port in runtime.json when it is this machine's running hub: it differs from the configured port and its
 * `/health` (no token sent) answers ok with the machine id from machine.toml. Null otherwise (no file, stale file,
 * another program on that port, no machine.toml), so callers fall back to config.toml.
 */
export async function verifiedRuntimePort(configured: number, opts: ResolveHubCredentialsOptions = {}): Promise<number | null> {
  const port = readRuntimePort(opts);
  if (port === null || port === configured) return null;
  const machineId = readMachineId(opts);
  if (!machineId) return null;
  const health = await (opts.fetchHealth ?? ((u: string) => fetchHubHealth(u, opts.timeoutMs ?? 800)))(`http://127.0.0.1:${port}`);
  return health?.ok === true && health.machineId === machineId ? port : null;
}

/**
 * Like `readHubCredentials`, but also finds a hub that runs on a port other than the configured one (a saved Hub
 * port that is not applied yet): runtime.json's port is used only after `/health` there names this machine (see
 * `verifiedRuntimePort`). Order: `PAWL_HUB_URL` > verified runtime.json > config.toml > default. The token is
 * never sent anywhere here. Throws `HubCredentialsError` like `readHubCredentials`.
 */
export async function resolveHubCredentials(opts: ResolveHubCredentialsOptions = {}): Promise<HubCredentials> {
  const base = readHubCredentials(opts);
  if (base.urlSource === "env") return base;
  const configured = Number(new URL(base.httpUrl).port || HUB_DEFAULT_PORT);
  const port = await verifiedRuntimePort(configured, opts);
  if (port === null) return base;
  const raw = `http://127.0.0.1:${port}`;
  return { ...base, url: toHubWsUrl(raw), httpUrl: toHubHttpUrl(raw), urlSource: "runtime" };
}

/** Like `readHubCredentials` but returns null instead of throwing. */
export function tryReadHubCredentials(opts: HubCredentialsOptions = {}): HubCredentials | null {
  try {
    return readHubCredentials(opts);
  } catch {
    return null;
  }
}

function nonEmpty(v: string | undefined): string | undefined {
  const t = v?.trim();
  return t ? t : undefined;
}

function isAbsolutePath(p: string): boolean {
  return p.startsWith("/") || /^[a-zA-Z]:[\\/]/.test(p) || p.startsWith("\\\\");
}
