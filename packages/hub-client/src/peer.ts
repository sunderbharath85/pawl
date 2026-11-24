// Pairing helpers (migration step 2). Pure, no I/O. Invite URIs carry a one-time secret (`code`):
// nothing here logs, stores or returns it.
import { MACHINE_ID_RE, type MachineId, type UnixMs } from "@krypto/protocol";

/** Invite lifetime bounds the hub enforces (seconds). */
export const INVITE_TTL_MIN_SECS = 60;
export const INVITE_TTL_MAX_SECS = 3600;
export const INVITE_TTL_DEFAULT_SECS = 600;

/** Every invite URI starts with this. */
export const INVITE_URI_PREFIX = "krypto://peer?";

/** The non-secret parts of an invite URI (the code and key are deliberately left out). */
export interface InviteInfo {
  /** The inviting hub's machine id. */
  machineId: MachineId;
  /** Where the joining hub will dial, "host:port". */
  addr: string;
  /** Unix ms, by the inviting machine's clock. */
  expiresAt: UnixMs;
}

const B64URL_RE = /^[A-Za-z0-9_-]+=*$/;

/**
 * Find an invite URI in pasted text: the bare URI, the URI in quotes, or the whole
 * `krypto peer join "<uri>"` line. Returns the URI or null.
 */
export function extractInviteUri(text: string): string | null {
  const start = text.indexOf(INVITE_URI_PREFIX);
  if (start < 0) return null;
  const rest = text.slice(start);
  const end = rest.search(/[\s"'<>`]/);
  const uri = end < 0 ? rest : rest.slice(0, end);
  return uri.length > INVITE_URI_PREFIX.length ? uri : null;
}

/**
 * Check an invite URI's shape (`krypto://peer?v=1&mid=…&pk=…&addr=…&code=…&exp=…`) and return its
 * public parts, or null if it is not one. The hub stays the authority (`bad_uri`); this only lets a
 * client explain a bad paste before sending it.
 */
export function parseInviteUri(uri: string): InviteInfo | null {
  const trimmed = uri.trim();
  if (!trimmed.startsWith(INVITE_URI_PREFIX)) return null;
  let params: URLSearchParams;
  try {
    params = new URLSearchParams(trimmed.slice(INVITE_URI_PREFIX.length));
  } catch {
    return null;
  }
  const machineId = params.get("mid") ?? "";
  const pk = params.get("pk") ?? "";
  const code = params.get("code") ?? "";
  const addr = (params.get("addr") ?? "").trim();
  const exp = params.get("exp") ?? "";
  if (params.get("v") !== "1") return null;
  if (!MACHINE_ID_RE.test(machineId)) return null;
  if (!B64URL_RE.test(pk) || !B64URL_RE.test(code)) return null;
  if (!addr || /\s/.test(addr)) return null;
  if (!/^\d{1,16}$/.test(exp)) return null;
  return { machineId, addr, expiresAt: Number(exp) };
}

/** True if a peer-listener address binds every interface (0.0.0.0 / ::), so it cannot be advertised as is. */
export function isUnspecifiedAddr(addr: string): boolean {
  const host = addr.trim().replace(/:\d+$/, "").replace(/^\[|\]$/g, "");
  return host === "0.0.0.0" || host === "::" || host === "";
}
