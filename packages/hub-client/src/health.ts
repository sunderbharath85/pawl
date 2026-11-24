// Browser/Node-safe hub liveness probe (uses global fetch).
import { toHubHttpUrl } from "./url";

export interface HubHealth {
  ok: boolean;
  version?: string;
}

/**
 * `GET <hub>/health` (no auth). Resolves `null` if the hub is unreachable, times out or answers garbage.
 * `url` may be any hub address (http or ws form); default `http://127.0.0.1:47800`.
 */
export async function fetchHubHealth(url?: string, timeoutMs = 2000): Promise<HubHealth | null> {
  if (typeof fetch !== "function") return null;
  const controller = typeof AbortController !== "undefined" ? new AbortController() : null;
  const timer = controller ? setTimeout(() => controller.abort(), timeoutMs) : null;
  try {
    const res = await fetch(`${toHubHttpUrl(url)}/health`, controller ? { signal: controller.signal } : undefined);
    if (!res.ok) return null;
    const body = (await res.json()) as { ok?: unknown; version?: unknown };
    if (typeof body !== "object" || body === null) return null;
    return { ok: body.ok === true, ...(typeof body.version === "string" ? { version: body.version } : {}) };
  } catch {
    return null;
  } finally {
    if (timer) clearTimeout(timer);
  }
}
