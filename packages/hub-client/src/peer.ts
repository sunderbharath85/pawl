// Pairing helpers (migration step 2; relay invites step 6). Pure, no I/O. Invite URIs carry secrets (the one-time
// `code`, the relay pairing token `rt` and the relay access key `rk`): nothing here logs, stores or returns them.
import { MACHINE_ID_RE, type LinkPath, type MachineId, type RelayStatus, type UnixMs } from "@krypto/protocol";

/** Invite lifetime bounds the hub enforces (seconds). */
export const INVITE_TTL_MIN_SECS = 60;
export const INVITE_TTL_MAX_SECS = 3600;
export const INVITE_TTL_DEFAULT_SECS = 600;

/** Every invite URI starts with this. */
export const INVITE_URI_PREFIX = "krypto://peer?";

/** The non-secret parts of an invite URI (the code, relay token and relay access key are deliberately left out). */
export interface InviteInfo {
  /** The inviting hub's machine id. */
  machineId: MachineId;
  /** Where the joining hub will dial on the LAN, "host:port"; null for a relay-only invite. */
  addr: string | null;
  /** The relay the joining hub falls back to (as written in the invite), or null for a LAN-only invite. */
  relay: string | null;
  /** Unix ms, by the inviting machine's clock. */
  expiresAt: UnixMs;
}

/** "192.168.1.20:47801", "relay relay.example.dev" or both: where an invite will be dialed (no secrets). */
export function inviteRoute(info: Pick<InviteInfo, "addr" | "relay">): string {
  const host = info.relay ? relayHost(info.relay) : null;
  if (info.addr && host) return `${info.addr} (or the relay ${host})`;
  if (info.addr) return info.addr;
  return host ? `the relay ${host}` : "an unknown address";
}

/** The host[:port] of a relay URL ("wss://relay.example.dev/x" → "relay.example.dev"), or the input. */
export function relayHost(url: string): string {
  const m = /^[a-zA-Z][a-zA-Z0-9+.-]*:\/\/([^/?#]+)/.exec(url.trim());
  return m ? m[1]! : url.trim();
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
  const addr = params.has("addr") ? (params.get("addr") ?? "").trim() : null;
  const relay = params.has("relay") ? (params.get("relay") ?? "").trim() : null;
  const rt = params.get("rt");
  const rk = params.get("rk");
  const exp = params.get("exp") ?? "";
  if (params.get("v") !== "1") return null;
  if (!MACHINE_ID_RE.test(machineId)) return null;
  if (!B64URL_RE.test(pk) || !B64URL_RE.test(code)) return null;
  if (addr !== null && (!addr || /\s/.test(addr))) return null;
  // A relay needs its pairing token; the access key only travels with a relay.
  if ((relay === null) !== (rt === null)) return null;
  if (relay !== null && (!/^(wss?|https?):\/\/[^\s/?#]+/i.test(relay) || !B64URL_RE.test(rt!))) return null;
  if (rk !== null && (relay === null || !/^[A-Za-z0-9_-]+$/.test(rk))) return null;
  if (addr === null && relay === null) return null;
  if (!/^\d{1,16}$/.test(exp)) return null;
  return { machineId, addr, relay, expiresAt: Number(exp) };
}

/** True if a peer-listener address binds every interface (0.0.0.0 / ::), so it cannot be advertised as is. */
export function isUnspecifiedAddr(addr: string): boolean {
  const host = addr.trim().replace(/:\d+$/, "").replace(/^\[|\]$/g, "");
  return host === "0.0.0.0" || host === "::" || host === "";
}

// ---------------------------------------------------------------- relay (migration step 6)

/** What each `RelayStatus.lastError` code means for the user. */
export const RELAY_ERROR_TEXT: Record<string, string> = {
  unreachable: "Can't reach the relay. Check the URL and this computer's internet connection.",
  timeout: "The relay did not answer in time.",
  bad_key: "The relay turned down the access key. Set the key from your relay deployment.",
  unavailable: "The relay is not accepting connections right now (it may be down or have no access key set up).",
  rate_limited: "The relay is limiting connections for now. The hub tries again by itself.",
  bad_auth: "The relay could not verify this hub's identity.",
  bad_frame: "The relay and this hub misunderstood each other. Update krypto if this keeps happening.",
  unsupported_version: "The relay runs a different protocol version. Update krypto or the relay.",
  replaced: "Another connection with this hub's identity took over (is this hub running twice?).",
  closed: "The relay closed the connection. The hub reconnects by itself.",
};

export type RelayTone = "off" | "connected" | "connecting" | "error";

/** A one-line relay state for display (never contains a key). */
export function relayStatusText(status: RelayStatus | null | undefined): { tone: RelayTone; text: string } {
  if (!status || !status.url) return { tone: "off", text: "Relay off" };
  const host = relayHost(status.url);
  if (status.connected) return { tone: "connected", text: `Connected to ${host}` };
  if (status.lastError) {
    const why = RELAY_ERROR_TEXT[status.lastError] ?? `The relay connection failed (${status.lastError}).`;
    return { tone: "error", text: `Not connected to ${host}. ${why}` };
  }
  return { tone: "connecting", text: `Connecting to ${host}…` };
}

/** "LAN" / "Relay" for a paired machine's live link (null when it has none). */
export function linkPathLabel(path: LinkPath | null | undefined): string | null {
  return path === "lan" ? "LAN" : path === "relay" ? "Relay" : null;
}

/**
 * An invite (or a text holding one) for display: the relay access key (`rk`) is replaced by dots. Copy the real URI;
 * this one cannot be used to pair.
 */
export function maskInviteSecrets(text: string): string {
  return text.replace(/([?&]rk=)[^&\s"'<>`]*/g, "$1••••••");
}

/**
 * One line for the pairing UI, shown before Create invite / Join: what a paired machine can do with this one.
 * `acceptIntents` = this hub's remote control setting (undefined while unknown). Pairing works both ways, so the
 * same is true for this machine on the other one, as far as that one allows.
 */
export function pairingAllowsText(acceptIntents: boolean | undefined): string {
  const sees = "A paired machine sees this machine's sessions, pets and approvals";
  if (acceptIntents === false) return `${sees}. Remote control is off here, so it cannot act on this machine.`;
  if (acceptIntents === true) {
    return `${sees}, and can start agents in allowed folders, send prompts, cancel, and approve once or deny here (remote control is on).`;
  }
  return `${sees}; with remote control on, it can also start agents, prompt, cancel and approve once or deny here.`;
}
