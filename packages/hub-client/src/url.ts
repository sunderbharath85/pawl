// Browser-safe URL helpers (no Node imports).
import { HUB_DEFAULT_PORT, HUB_HOST } from "@ai-pet/protocol";

export const DEFAULT_HUB_HTTP_URL = `http://${HUB_HOST}:${HUB_DEFAULT_PORT}`;
export const DEFAULT_HUB_WS_URL = `ws://${HUB_HOST}:${HUB_DEFAULT_PORT}/ws`;

function parse(input: string): URL | null {
  const trimmed = input.trim();
  if (!trimmed) return null;
  try {
    // Accept bare "host:port" / "127.0.0.1:47800".
    return new URL(/^[a-z][a-z0-9+.-]*:\/\//i.test(trimmed) ? trimmed : `http://${trimmed}`);
  } catch {
    return null;
  }
}

/**
 * Normalize any hub address to the WebSocket endpoint:
 * `http://127.0.0.1:47800` → `ws://127.0.0.1:47800/ws`, `https://x` → `wss://x/ws`,
 * `ws://h:1/ws` unchanged. Invalid input → `DEFAULT_HUB_WS_URL`.
 */
export function toHubWsUrl(input: string | undefined | null): string {
  if (!input) return DEFAULT_HUB_WS_URL;
  const url = parse(input);
  if (!url) return DEFAULT_HUB_WS_URL;
  if (url.protocol === "http:") url.protocol = "ws:";
  else if (url.protocol === "https:") url.protocol = "wss:";
  else if (url.protocol !== "ws:" && url.protocol !== "wss:") return DEFAULT_HUB_WS_URL;
  if (url.pathname === "" || url.pathname === "/") url.pathname = "/ws";
  url.hash = "";
  return url.toString();
}

/**
 * Normalize any hub address to its HTTP base (no trailing slash, no path):
 * `ws://127.0.0.1:47800/ws` → `http://127.0.0.1:47800`.
 */
export function toHubHttpUrl(input: string | undefined | null): string {
  if (!input) return DEFAULT_HUB_HTTP_URL;
  const url = parse(input);
  if (!url) return DEFAULT_HUB_HTTP_URL;
  if (url.protocol === "ws:") url.protocol = "http:";
  else if (url.protocol === "wss:") url.protocol = "https:";
  else if (url.protocol !== "http:" && url.protocol !== "https:") return DEFAULT_HUB_HTTP_URL;
  return `${url.protocol}//${url.host}`;
}
