// Relay wire frames (CONTRACTS.md "Relay"): JSON over WS `wss://<relay>/v1/connect?role=hub|device|pairing&hubId=..`.
// The relay only ever sees routing ids and ciphertext; `box` holds an encrypted protocol ClientMsg/ServerMsg.

export type RelayRole = "hub" | "device" | "pairing";

export interface MsgFrame {
  t: "msg";
  from: string;
  to: string;
  nonce: string;
  box: string;
}
export interface PairRequestFrame {
  t: "pair_request";
  deviceId: string;
  devicePk: string;
  deviceName: string;
  code: string;
}
export interface PairOkFrame {
  t: "pair_ok";
  deviceId: string;
  nonce: string;
  box: string;
}
export interface RegisterPushFrame {
  t: "register_push";
  deviceId: string;
  expoPushToken: string;
}
export interface PushFrame {
  t: "push";
  to: string;
}
/** Relay-originated status/error frames (shape tolerant: relay may add fields). */
export interface RelayErrorFrame {
  t: "error";
  code?: string;
  message?: string;
}
export interface RelayPresenceFrame {
  t: "presence";
  hubOnline?: boolean;
  online?: boolean;
  [k: string]: unknown;
}

export type RelayFrame = MsgFrame | PairRequestFrame | PairOkFrame | RegisterPushFrame | PushFrame | RelayErrorFrame | RelayPresenceFrame;

/** Frames a device sends. */
export type DeviceOutFrame = MsgFrame | PairRequestFrame | RegisterPushFrame;

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}
const str = (v: unknown): v is string => typeof v === "string";

/** Parse one relay text frame. Returns null for non-JSON or frames missing required fields. */
export function parseRelayFrame(text: string): RelayFrame | { t: "unknown"; raw: Record<string, unknown> } | null {
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    return null;
  }
  if (!isObj(v) || !str(v.t)) return null;
  switch (v.t) {
    case "msg":
      return str(v.from) && str(v.to) && str(v.nonce) && str(v.box) ? (v as unknown as MsgFrame) : null;
    case "pair_ok":
      return str(v.deviceId) && str(v.nonce) && str(v.box) ? (v as unknown as PairOkFrame) : null;
    case "pair_request":
      return str(v.deviceId) && str(v.devicePk) && str(v.code) ? (v as unknown as PairRequestFrame) : null;
    case "register_push":
      return str(v.deviceId) && str(v.expoPushToken) ? (v as unknown as RegisterPushFrame) : null;
    case "push":
      return str(v.to) ? (v as unknown as PushFrame) : null;
    case "error":
      return { t: "error", code: str(v.code) ? v.code : undefined, message: str(v.message) ? v.message : undefined };
    case "presence":
      return v as RelayPresenceFrame;
    default:
      return { t: "unknown", raw: v };
  }
}

/**
 * `https://relay.example` → `wss://relay.example/v1/connect?role=device&hubId=..&deviceId=..`.
 * http → ws (LAN/dev relays). A path prefix on the relay URL is kept.
 */
export function relayConnectUrl(relayUrl: string, params: { role: RelayRole; hubId: string; deviceId?: string }): string {
  const m = /^(https?|wss?):\/\/([^/?#]+)([^?#]*)/i.exec(relayUrl.trim());
  if (!m) throw new Error(`invalid relay url: ${relayUrl}`);
  const scheme = m[1]!.toLowerCase();
  const ws = scheme === "https" || scheme === "wss" ? "wss" : "ws";
  const base = (m[3] ?? "").replace(/\/+$/, "");
  const q = [`role=${encodeURIComponent(params.role)}`, `hubId=${encodeURIComponent(params.hubId)}`];
  if (params.deviceId) q.push(`deviceId=${encodeURIComponent(params.deviceId)}`);
  return `${ws}://${m[2]}${base}/v1/connect?${q.join("&")}`;
}
