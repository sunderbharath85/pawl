// Parse the pairing QR / deep link: aipet://pair?relay=<https-url>&hub=<hubId>&pk=<hubPkB64>&code=<8 chars>
import { decodePublicKey, encodeBase64 } from "./crypto";

export interface PairingOffer {
  relay: string;
  hubId: string;
  /** Hub public key, standard base64 (normalized). */
  hubPk: string;
  code: string;
  /** True when the relay is not https/wss (LAN/dev relay). */
  insecure: boolean;
}

export type PairingParseResult = { ok: true; offer: PairingOffer } | { ok: false; error: string };

type Params = Record<string, string | string[] | undefined>;

function first(v: string | string[] | undefined): string {
  return (Array.isArray(v) ? v[0] : v)?.trim() ?? "";
}

function decodeComponent(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

/** Query params of a URL without relying on URL/URLSearchParams (keeps a literal `+` as `+`). */
function queryParams(url: string): Params {
  const q = url.indexOf("?");
  if (q < 0) return {};
  const hash = url.indexOf("#", q);
  const query = url.slice(q + 1, hash < 0 ? undefined : hash);
  const out: Params = {};
  for (const part of query.split("&")) {
    if (!part) continue;
    const eq = part.indexOf("=");
    const k = decodeComponent(eq < 0 ? part : part.slice(0, eq));
    const v = eq < 0 ? "" : decodeComponent(part.slice(eq + 1));
    if (!(k in out)) out[k] = v;
  }
  return out;
}

/** Validate already-split params (e.g. from expo-router `useLocalSearchParams`). */
export function parsePairingParams(params: Params): PairingParseResult {
  const relay = first(params.relay);
  const hubId = first(params.hub);
  // Query parsers may turn an unescaped base64 `+` into a space: undo that.
  const pkRaw = first(params.pk).replace(/ /g, "+");
  const code = first(params.code);
  if (!relay || !hubId || !pkRaw || !code) return { ok: false, error: "This QR code is missing pairing details." };
  if (!/^(https?|wss?):\/\/[^\s/?#]+/i.test(relay)) return { ok: false, error: "The relay address in this code is not a valid URL." };
  if (!/^[A-Za-z0-9._:-]{1,128}$/.test(hubId)) return { ok: false, error: "The hub id in this code looks wrong." };
  if (!/^[A-Za-z0-9]{8}$/.test(code)) return { ok: false, error: "The pairing code must be 8 letters or digits." };
  let hubPk: string;
  try {
    // Re-encode so URL-safe / unpadded input is stored as canonical standard base64.
    hubPk = encodeBase64(decodePublicKey(pkRaw));
  } catch (err) {
    return { ok: false, error: `The hub key in this code is invalid (${err instanceof Error ? err.message : String(err)}).` };
  }
  const insecure = !/^(https|wss):/i.test(relay);
  return { ok: true, offer: { relay: relay.replace(/\/+$/, ""), hubId, hubPk, code, insecure } };
}

/** Parse a scanned/opened link. Accepts `aipet://pair?...` (also `aipet:///pair?...` and `aipet:pair?...`). */
export function parsePairingUrl(url: string): PairingParseResult {
  const trimmed = url.trim();
  if (!/^aipet:(\/\/\/?)?pair(\/)?\?/i.test(trimmed)) {
    return { ok: false, error: "That QR code is not an AI Pet Hub pairing code." };
  }
  return parsePairingParams(queryParams(trimmed));
}
