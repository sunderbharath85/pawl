// Worker request handling, with its dependencies injected so it runs under `bun test`:
//   GET /health                          -> {"ok":true}
//   GET /v1/connect?pk=<pk>&k=<key>      -> forwarded to the device's mailbox
// Every refusal happens here, before any Durable Object is addressed (`forward` is the only path to one):
// 404 unknown path, 426 not a WebSocket upgrade, 503 access key not configured (unless RELAY_OPEN = "1"),
// 429 per-IP connect budget spent, 401 missing / wrong access key, 400 bad pk or Host.

import { bytesEqual, decodeUrl, utf8 } from "./b64";
import { isValidPk } from "./frames";
import { HOST_MAX_CHARS } from "./limits";
import { log } from "./log";

/** Structurally the Workers Rate Limiting binding (`RateLimit`). */
export interface ConnectLimiter {
  limit(options: { key: string }): Promise<{ success: boolean }>;
}

export interface WorkerDeps {
  /** Worker secret RELAY_ACCESS_KEY: base64url, 32 bytes. */
  accessKey: string | undefined;
  /** Var RELAY_OPEN: "1" serves without an access key (not recommended). */
  open: string | undefined;
  limiter: ConnectLimiter;
  /** Hand the upgrade to mailbox idFromName(pk). Only called after every check passed. */
  forward(pk: string, host: string): Promise<Response>;
}

const ACCESS_KEY_RE = /^[A-Za-z0-9_-]{43}$/;
const HOST_RE = /^[A-Za-z0-9.\-:[\]]+$/;
const IP_KEY_MAX_CHARS = 64;

export function isValidAccessKey(s: unknown): s is string {
  return typeof s === "string" && ACCESS_KEY_RE.test(s) && decodeUrl(s)?.length === 32;
}

/** Constant-time comparison of the presented key with the configured one (lengths are public: 43 chars). */
export function accessKeyMatches(given: string | undefined, expected: string): boolean {
  if (given === undefined || given.length !== expected.length) return false;
  return bytesEqual(utf8(given), utf8(expected));
}

function text(status: number, body: string, headers: Record<string, string> = {}): Response {
  return new Response(body, {
    status,
    headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "no-store", ...headers },
  });
}

export async function handleRequest(request: Request, deps: WorkerDeps): Promise<Response> {
  let url: URL;
  try {
    url = new URL(request.url);
  } catch {
    return text(400, "bad request");
  }

  if (url.pathname === "/health") {
    if (request.method !== "GET" && request.method !== "HEAD") return text(405, "method not allowed", { allow: "GET, HEAD" });
    return Response.json({ ok: true }, { headers: { "cache-control": "no-store" } });
  }
  if (url.pathname !== "/v1/connect") return text(404, "not found");
  if (request.method !== "GET" || request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
    return text(426, "websocket upgrade required", { upgrade: "websocket" });
  }

  // Fail closed on configuration: no key and not explicitly open -> serve nobody.
  const keyed = deps.accessKey !== undefined && deps.accessKey !== "";
  if (keyed && !isValidAccessKey(deps.accessKey)) {
    log("access_key_misconfigured");
    return text(503, "relay misconfigured");
  }
  if (!keyed && deps.open !== "1") return text(503, "relay access key not configured");

  // Per-IP connect budget (Cloudflare sets CF-Connecting-IP; clients cannot override it at the edge).
  const ip = (request.headers.get("cf-connecting-ip") ?? "unknown").slice(0, IP_KEY_MAX_CHARS);
  let allowed: boolean;
  try {
    allowed = (await deps.limiter.limit({ key: `connect:${ip}` })).success;
  } catch {
    log("rate_limiter_error");
    return text(503, "try again later");
  }
  if (!allowed) {
    log("connect_rate_limited");
    return text(429, "too many connections", { "retry-after": "60" });
  }

  if (keyed) {
    const ks = url.searchParams.getAll("k");
    if (ks.length !== 1 || !accessKeyMatches(ks[0], deps.accessKey as string)) {
      log("access_denied");
      return text(401, "access key required");
    }
  }

  const pks = url.searchParams.getAll("pk");
  const pk = pks[0];
  if (pks.length !== 1 || !isValidPk(pk)) return text(400, "bad pk");
  const host = request.headers.get("host") ?? url.host;
  if (host.length === 0 || host.length > HOST_MAX_CHARS || !HOST_RE.test(host)) return text(400, "bad host");

  return deps.forward(pk, host);
}
