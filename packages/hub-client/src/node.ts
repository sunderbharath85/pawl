// @ai-pet/hub-client/node — Node-only helpers (VS Code extension host, CLIs, tests).
// Never import this from browser/webview code: it uses node:fs/os/path.
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { HUB_DATA_DIR_NAME, HUB_TOKEN_FILE } from "@ai-pet/protocol";
import { toHubHttpUrl, toHubWsUrl } from "./url";

export interface HubCredentials {
  /** WebSocket URL for `HubClient` (e.g. `ws://127.0.0.1:47800/ws`). */
  url: string;
  /** HTTP base for `/health` and `/hooks/*` (e.g. `http://127.0.0.1:47800`). */
  httpUrl: string;
  token: string;
  /** Where the token came from. */
  tokenSource: "env" | "file";
  /** Hub data dir (`<os data dir>/ai-pet-hub`). */
  dataDir: string;
  /** Token file path (`<dataDir>/token`), even if the token came from env. */
  tokenFile: string;
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

/** `<os data dir>/ai-pet-hub` — holds `hub.db`, `token`, `config.toml`. */
export function hubDataDir(opts: HubCredentialsOptions = {}): string {
  return opts.dataDir ?? join(osDataDir(opts), HUB_DATA_DIR_NAME);
}

/** `<hub data dir>/token`. */
export function hubTokenFile(opts: HubCredentialsOptions = {}): string {
  return join(hubDataDir(opts), HUB_TOKEN_FILE);
}

/**
 * Resolve the hub URL and token. `AI_PET_HUB_TOKEN` / `AI_PET_HUB_URL` env vars override the token
 * file and the default `127.0.0.1:47800`. Throws `HubCredentialsError` when no token can be found
 * (typically: the hub has never been started on this machine).
 */
export function readHubCredentials(opts: HubCredentialsOptions = {}): HubCredentials {
  const env = opts.env ?? process.env;
  const dataDir = hubDataDir(opts);
  const tokenFile = join(dataDir, HUB_TOKEN_FILE);
  const rawUrl = nonEmpty(env["AI_PET_HUB_URL"]);
  const url = toHubWsUrl(rawUrl);
  const httpUrl = toHubHttpUrl(rawUrl);

  const envToken = nonEmpty(env["AI_PET_HUB_TOKEN"]);
  if (envToken) return { url, httpUrl, token: envToken, tokenSource: "env", dataDir, tokenFile };

  let text: string;
  try {
    text = readFileSync(tokenFile, "utf8");
  } catch (err) {
    const code = (err as { code?: string }).code;
    const why = code === "ENOENT" ? "not found (has the hub been started?)" : `unreadable (${code ?? String(err)})`;
    throw new HubCredentialsError(`AI Pet Hub token file ${why}: ${tokenFile}`, tokenFile, { cause: err });
  }
  const token = text.replace(/^\uFEFF/, "").trim();
  if (!token) throw new HubCredentialsError(`AI Pet Hub token file is empty: ${tokenFile}`, tokenFile);
  return { url, httpUrl, token, tokenSource: "file", dataDir, tokenFile };
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
