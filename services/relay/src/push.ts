// Content-free push wakes through the Expo Push API (https://docs.expo.dev/push-notifications/sending-notifications/).
// When a frame is queued for an offline mailbox that registered a token, the relay sends one fixed notification
// ("Your pet needs you"), at most once per minute. Nothing from the frame goes into the push.

import type { KvStore } from "./storage";

export const EXPO_PUSH_URL = "https://exp.host/--/api/v2/push/send";
export const PUSH_MIN_INTERVAL_MS = 60_000;
export const PUSH_TITLE = "Pawl";
export const PUSH_BODY = "Your pet needs you";
export const PUSH_KEY = "push";

const EXPO_TOKEN_RE = /^Expo(nent)?PushToken\[[A-Za-z0-9_-]{1,256}\]$/;

export function isValidExpoToken(s: unknown): s is string {
  return typeof s === "string" && EXPO_TOKEN_RE.test(s);
}

export interface PushState {
  /** Absent after `unregister_push` while the throttle window is still open (see `PushRegistry.clear`). */
  token?: string;
  /** Last push attempt (ms); 0 = never. */
  lastAt: number;
}

export function shouldPush(state: PushState | undefined, now: number): state is PushState & { token: string } {
  return !!state?.token && now - state.lastAt >= PUSH_MIN_INTERVAL_MS;
}

/** The request for one wake. `accessToken` = optional Expo access token (enhanced push security). */
export function buildExpoPushRequest(token: string, accessToken?: string): { url: string; init: RequestInit } {
  const headers: Record<string, string> = {
    accept: "application/json",
    "content-type": "application/json",
  };
  if (accessToken) headers.authorization = `Bearer ${accessToken}`;
  const message = { to: token, title: PUSH_TITLE, body: PUSH_BODY, sound: "default", priority: "high" };
  return { url: EXPO_PUSH_URL, init: { method: "POST", headers, body: JSON.stringify([message]) } };
}

export type PushOutcome = "ok" | "device_not_registered" | "error";

/** Read the push ticket(s) Expo answers with. Only DeviceNotRegistered changes what we store. */
export function parseExpoResponse(status: number, body: unknown): PushOutcome {
  if (status < 200 || status >= 300) return "error";
  if (typeof body !== "object" || body === null) return "error";
  const data = (body as { data?: unknown }).data;
  const tickets = Array.isArray(data) ? data : data !== undefined ? [data] : [];
  if (tickets.length === 0) return "error";
  for (const t of tickets) {
    if (typeof t !== "object" || t === null) return "error";
    const ticket = t as { status?: unknown; details?: { error?: unknown } };
    if (ticket.status === "ok") continue;
    if (ticket.details?.error === "DeviceNotRegistered") return "device_not_registered";
    return "error";
  }
  return "ok";
}

/** Push token storage for one mailbox. */
export class PushRegistry {
  constructor(private readonly kv: KvStore) {}

  get(): PushState | undefined {
    return this.kv.get<PushState>(PUSH_KEY);
  }

  /** Store the token. Returns false (and writes nothing) when it is already the stored one. */
  register(token: string): boolean {
    const prev = this.get();
    if (prev?.token === token) return false;
    this.kv.put<PushState>(PUSH_KEY, { token, lastAt: prev?.lastAt ?? 0 });
    return true;
  }

  /** Mark an attempt now and return the token to push to, or null if none / throttled. */
  claim(now: number): string | null {
    const state = this.get();
    if (!shouldPush(state, now)) return null;
    this.kv.put<PushState>(PUSH_KEY, { token: state.token, lastAt: now });
    return state.token;
  }

  /** Forget the token if it is still the one that failed. */
  unregister(token: string): void {
    if (this.get()?.token === token) this.kv.delete(PUSH_KEY);
  }

  /**
   * `unregister_push`: forget the stored token so no wake is sent afterwards. Returns false (and writes nothing)
   * when no token is stored. While the throttle window of the last push is still open, only `lastAt` is kept, so
   * toggling push off and on cannot send more than one wake per minute.
   */
  clear(now: number): boolean {
    const state = this.get();
    if (!state?.token) return false;
    if (now - state.lastAt >= PUSH_MIN_INTERVAL_MS) this.kv.delete(PUSH_KEY);
    else this.kv.put<PushState>(PUSH_KEY, { lastAt: state.lastAt });
    return true;
  }
}

/** Send one wake with `fetchImpl`; never throws, never logs the token. */
export async function sendWake(
  token: string,
  accessToken: string | undefined,
  fetchImpl: typeof fetch = fetch,
): Promise<PushOutcome> {
  const { url, init } = buildExpoPushRequest(token, accessToken);
  try {
    const res = await fetchImpl(url, init);
    let body: unknown = null;
    try {
      body = await res.json();
    } catch {
      body = null;
    }
    return parseExpoResponse(res.status, body);
  } catch {
    return "error";
  }
}
