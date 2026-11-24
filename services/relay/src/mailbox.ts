// Durable Object `Mailbox`: one per device public key (idFromName(pk)). Holds that device's single live socket
// (WebSocket Hibernation API) around an Inbox (inbox.ts: allowlist, pairing slots, offline queue, push token and
// the delivery decisions).
//
// Socket lifecycle: accept -> `challenge` -> (auth within 10 s) -> `ok{queued}` + queued frames -> live.
// Per-socket state lives in the socket attachment so it survives hibernation; everything else is in the
// SQLite-backed synchronous KV storage (ctx.storage.kv).
//
// Frames between mailboxes go through `deliver()` (Durable Object RPC). A sender forwards to each recipient over
// one ordered chain, so frames from A to B arrive in the order A sent them (receivers drop seq regressions).

import { DurableObject } from "cloudflare:workers";
import { RELAY_FRAME_MAX_BYTES, type RelayErrorCode } from "@pawl/protocol";
import { createChallenge, verifyAuth } from "./auth";
import {
  decodePk,
  encodeServerFrame,
  errorFrame,
  isConfigFrame,
  isValidPk,
  messageBytes,
  okFrame,
  parseClientFrame,
  pongFrame,
} from "./frames";
import { Inbox, type DeliverRequest, type DeliverResult } from "./inbox";
import {
  ALARM_SLACK_MS,
  AUTH_FRAME_MAX_BYTES,
  AUTH_TIMEOUT_MS,
  CLOSE_INTERNAL,
  CLOSE_POLICY,
  CLOSE_REPLACED,
  HOST_MAX_CHARS,
  MAX_INFLIGHT_BYTES,
  MAX_INFLIGHT_FRAMES,
  MAX_INFLIGHT_RECIPIENTS,
} from "./limits";
import { log } from "./log";
import { planAdmission } from "./pending";
import { isValidExpoToken, sendWake } from "./push";
import { FrameLimiter } from "./ratelimit";

export type { DeliverRequest, DeliverResult } from "./inbox";

export interface Env {
  MAILBOX: DurableObjectNamespace<Mailbox>;
  /** Workers Rate Limiting binding: connects per client IP. */
  CONNECT_LIMIT: RateLimit;
  /** Secret: base64url of 32 random bytes; devices pass it as `k=` on /v1/connect. */
  RELAY_ACCESS_KEY?: string;
  /** "1" = serve without an access key (not recommended). */
  RELAY_OPEN?: string;
  /** Optional Expo access token (secret) for enhanced push security. */
  EXPO_ACCESS_TOKEN?: string;
}

/** Headers the Worker sets on the request it forwards to the mailbox (client headers are never passed through). */
export const H_PK = "x-pawl-relay-pk";
export const H_HOST = "x-pawl-relay-host";

type PendingAttachment = {
  s: "pending";
  pk: string;
  host: string;
  /** Challenge nonce (standard base64). */
  nonce: string;
  /** Per-connection relay secret key (standard base64). Lives only in this socket's attachment. */
  relaySk: string;
  deadline: number;
  at: number;
};
type Attachment = PendingAttachment | { s: "live"; pk: string; at: number } | { s: "closed" };

const WS_OPEN = 1;

export class Mailbox extends DurableObject<Env> {
  private readonly inbox: Inbox;
  private readonly limiters = new WeakMap<WebSocket, FrameLimiter>();
  /** One ordered forwarding chain per recipient pk; entries are removed when their chain drains. */
  private readonly chains = new Map<string, Promise<void>>();
  private inflightFrames = 0;
  private inflightBytes = 0;
  /** Cached alarm time; undefined = not read since this instance started. */
  private alarmAt: number | null | undefined = undefined;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.inbox = new Inbox(ctx.storage.kv);
  }

  // ---------- connect ----------

  async fetch(request: Request): Promise<Response> {
    const pk = request.headers.get(H_PK) ?? "";
    const host = request.headers.get(H_HOST) ?? "";
    if (
      request.headers.get("upgrade")?.toLowerCase() !== "websocket" ||
      !isValidPk(pk) ||
      host.length === 0 ||
      host.length > HOST_MAX_CHARS ||
      !this.ctx.id.equals(this.env.MAILBOX.idFromName(pk))
    ) {
      return new Response("bad request", { status: 400 });
    }
    const now = Date.now();
    // Only pending sockets are capped. Sockets we already closed are never counted: a client that ignores our close
    // frame keeps its socket in getWebSockets() until TCP dies, and counting those would let anyone lock the real
    // device out of its mailbox.
    const pending = this.ctx
      .getWebSockets()
      .map((ws) => ({ socket: ws, a: this.attachment(ws) }))
      .filter((p): p is { socket: WebSocket; a: PendingAttachment } => p.a?.s === "pending")
      .map((p) => ({ socket: p.socket, at: p.a.at }));
    const plan = planAdmission(pending, now);
    if (!plan.admit) {
      log("mailbox_busy");
      return new Response("mailbox busy", { status: 429, headers: { "retry-after": "3" } });
    }
    for (const ws of plan.evict) {
      this.sendTo(ws, errorFrame("bad_auth"));
      this.closeSocket(ws, CLOSE_POLICY, "auth timeout");
    }

    const pair = new WebSocketPair();
    const client = pair[0];
    const server = pair[1];
    this.ctx.acceptWebSocket(server);
    const { frame, pending: secret } = createChallenge();
    const att: PendingAttachment = {
      s: "pending",
      pk,
      host,
      nonce: secret.nonce,
      relaySk: secret.relaySk,
      deadline: now + AUTH_TIMEOUT_MS,
      at: now,
    };
    server.serializeAttachment(att);
    server.send(encodeServerFrame(frame));
    await this.scheduleAlarm(now);
    return new Response(null, { status: 101, webSocket: client });
  }

  // ---------- socket events (Hibernation API) ----------

  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    const att = this.attachment(ws);
    if (!att || att.s === "closed") {
      this.closeSocket(ws, CLOSE_POLICY, "closed");
      return;
    }
    const now = Date.now();
    if (att.s === "pending") {
      await this.handleAuth(ws, att, message, now);
      return;
    }

    let limiter = this.limiters.get(ws);
    if (!limiter) {
      limiter = new FrameLimiter(now);
      this.limiters.set(ws, limiter);
    }
    const verdict = limiter.check(now);
    if (verdict === "close") {
      log("rate_abuse_closed");
      this.closeSocket(ws, CLOSE_POLICY, "rate limit");
      return;
    }
    if (verdict !== "ok") {
      if (verdict === "notify") this.sendTo(ws, errorFrame("rate_limited"));
      return;
    }

    const bytes = messageBytes(message, RELAY_FRAME_MAX_BYTES);
    if (bytes > RELAY_FRAME_MAX_BYTES) {
      this.fail(ws, "too_large");
      return;
    }
    if (typeof message !== "string") {
      this.fail(ws, "bad_frame");
      return;
    }
    const parsed = parseClientFrame(message);
    if (!parsed.ok) {
      this.fail(ws, parsed.code);
      return;
    }
    const f = parsed.frame;
    if (isConfigFrame(f) && !this.inbox.takeConfig(now)) {
      this.fail(ws, "rate_limited");
      return;
    }
    switch (f.t) {
      case "auth":
        this.fail(ws, "bad_frame"); // already authenticated
        return;
      case "allow": {
        const r = this.inbox.setAllowed(f.pks);
        if (!r.ok) this.fail(ws, "bad_frame");
        else if (r.droppedQueued > 0) {
          log("queue_dropped_disallowed", { dropped: r.droppedQueued });
          await this.scheduleAlarm(now);
        }
        return;
      }
      case "allow_pairing": {
        const r = this.inbox.addPairingSlot(f.token, f.exp, now);
        if (!r.ok) this.fail(ws, "bad_frame");
        else if (r.changed) await this.scheduleAlarm(now);
        return;
      }
      case "send":
        this.forward(
          ws,
          f.to,
          f.pairing === undefined ? { from: att.pk, env: f.env } : { from: att.pk, env: f.env, pairing: f.pairing },
          bytes,
        );
        return;
      case "register_push":
        if (!isValidExpoToken(f.expoToken)) this.fail(ws, "bad_frame");
        else this.inbox.registerPush(f.expoToken);
        return;
      case "unregister_push":
        this.inbox.unregisterPush(now);
        return;
      case "ping":
        this.sendTo(ws, pongFrame(now));
        return;
    }
  }

  async webSocketClose(ws: WebSocket, code: number, reason: string, _wasClean: boolean): Promise<void> {
    this.limiters.delete(ws);
    try {
      if (code >= 1000 && code < 5000 && ![1004, 1005, 1006, 1015].includes(code)) ws.close(code, reason);
      else ws.close();
    } catch {
      // already closed
    }
  }

  async webSocketError(ws: WebSocket, _error: unknown): Promise<void> {
    this.limiters.delete(ws);
    this.closeSocket(ws, CLOSE_INTERNAL, "error");
  }

  // ---------- auth ----------

  private async handleAuth(ws: WebSocket, att: PendingAttachment, message: string | ArrayBuffer, now: number): Promise<void> {
    const reject = (): void => {
      log("bad_auth");
      this.sendTo(ws, errorFrame("bad_auth"));
      this.closeSocket(ws, CLOSE_POLICY, "bad auth");
    };
    if (now > att.deadline || typeof message !== "string" || message.length > AUTH_FRAME_MAX_BYTES) return reject();
    const parsed = parseClientFrame(message);
    if (!parsed.ok || parsed.frame.t !== "auth") return reject();
    const devicePk = decodePk(att.pk);
    if (!devicePk || !verifyAuth(parsed.frame.box, att, att.host, devicePk)) return reject();

    // One live socket per mailbox: the newly authenticated one replaces any older one.
    for (const other of this.ctx.getWebSockets()) {
      if (other !== ws && this.attachment(other)?.s === "live") this.closeSocket(other, CLOSE_REPLACED, "replaced");
    }
    ws.serializeAttachment({ s: "live", pk: att.pk, at: now } satisfies Attachment);

    const frames = this.inbox.onAuthenticated(now);
    this.sendTo(ws, okFrame(frames.length));
    let sent = 0;
    for (const f of frames) {
      if (!this.sendTo(ws, f.msg)) break;
      sent++;
    }
    this.inbox.queue.ack(sent);
    log("auth_ok", { flushed: sent });
    await this.scheduleAlarm(now);
  }

  // ---------- sending ----------

  private forward(ws: WebSocket, to: string, req: DeliverRequest, bytes: number): void {
    if (to === req.from) {
      this.fail(ws, "bad_frame");
      return;
    }
    if (
      this.inflightFrames >= MAX_INFLIGHT_FRAMES ||
      this.inflightBytes + bytes > MAX_INFLIGHT_BYTES ||
      (!this.chains.has(to) && this.chains.size >= MAX_INFLIGHT_RECIPIENTS)
    ) {
      this.fail(ws, "rate_limited", to);
      return;
    }
    this.inflightFrames++;
    this.inflightBytes += bytes;
    const prev = this.chains.get(to) ?? Promise.resolve();
    const next: Promise<void> = prev
      .then(async () => {
        const result = await this.deliverRemote(to, req);
        if (result === "unavailable") this.fail(ws, "rate_limited", to);
        else if (result !== "delivered" && result !== "queued") this.fail(ws, result, to);
      })
      .catch(() => {
        log("forward_error");
      })
      .finally(() => {
        this.inflightFrames--;
        this.inflightBytes -= bytes;
        if (this.chains.get(to) === next) this.chains.delete(to);
      });
    this.chains.set(to, next);
  }

  private async deliverRemote(to: string, req: DeliverRequest): Promise<DeliverResult | "unavailable"> {
    for (let attempt = 0; attempt < 3; attempt++) {
      try {
        const stub = this.env.MAILBOX.get(this.env.MAILBOX.idFromName(to));
        return await stub.deliver(req);
      } catch (e) {
        const err = e as { retryable?: boolean; overloaded?: boolean } | null;
        if (!err?.retryable || err.overloaded) break;
      }
    }
    log("deliver_unavailable");
    return "unavailable";
  }

  /**
   * RPC from another mailbox: a frame for this device. Delivered live, or queued while the device is offline.
   * Accepted only from an allowed sender, or once per pairing slot (see Inbox.deliver for the order of checks).
   */
  async deliver(req: DeliverRequest): Promise<DeliverResult> {
    const now = Date.now();
    const out = this.inbox.deliver(req, now, (msg) => {
      const live = this.liveSocket();
      return live !== null && this.sendTo(live, msg);
    });
    if (out.viaPairing) log("pairing_slot_used");
    if (out.dropped > 0) log("queue_dropped", { dropped: out.dropped });
    if (out.wake) this.wake(out.wake);
    if (out.result === "queued" || out.result === "queue_full") await this.scheduleAlarm(now);
    return out.result;
  }

  /** Content-free push wake for an offline device (Inbox decides when one is due). */
  private wake(token: string): void {
    this.ctx.waitUntil(
      sendWake(token, this.env.EXPO_ACCESS_TOKEN).then((outcome) => {
        if (outcome === "device_not_registered") this.inbox.push.unregister(token);
        log(`push_${outcome}`);
      }),
    );
  }

  // ---------- alarm: queue TTL, auth deadlines, pairing slot expiry ----------

  async alarm(): Promise<void> {
    this.alarmAt = null;
    const now = Date.now();
    const expired = this.inbox.queue.prune(now);
    if (expired > 0) log("queue_expired", { count: expired });
    this.inbox.acl.pruneSlots(now);
    for (const ws of this.ctx.getWebSockets()) {
      const a = this.attachment(ws);
      if (a?.s === "pending" && a.deadline <= now + ALARM_SLACK_MS) {
        log("auth_timeout");
        this.sendTo(ws, errorFrame("bad_auth"));
        this.closeSocket(ws, CLOSE_POLICY, "auth timeout");
      }
    }
    await this.scheduleAlarm(now);
  }

  private async scheduleAlarm(now: number): Promise<void> {
    let next: number | null = this.inbox.queue.nextExpiry();
    const slot = this.inbox.acl.nextSlotExpiry();
    if (slot !== null) next = next === null ? slot : Math.min(next, slot);
    for (const ws of this.ctx.getWebSockets()) {
      const a = this.attachment(ws);
      if (a?.s === "pending") next = next === null ? a.deadline : Math.min(next, a.deadline);
    }
    if (this.alarmAt === undefined) this.alarmAt = await this.ctx.storage.getAlarm();
    if (next === null) {
      if (this.alarmAt !== null) {
        await this.ctx.storage.deleteAlarm();
        this.alarmAt = null;
      }
      return;
    }
    next = Math.max(next, now);
    if (this.alarmAt !== null && this.alarmAt <= next + ALARM_SLACK_MS) return; // an earlier alarm reschedules
    await this.ctx.storage.setAlarm(next);
    this.alarmAt = next;
  }

  // ---------- helpers ----------

  private attachment(ws: WebSocket): Attachment | null {
    try {
      const a = ws.deserializeAttachment() as Attachment | null;
      return a && typeof a === "object" && typeof a.s === "string" ? a : null;
    } catch {
      return null;
    }
  }

  private liveSocket(): WebSocket | null {
    for (const ws of this.ctx.getWebSockets()) {
      if (ws.readyState === WS_OPEN && this.attachment(ws)?.s === "live") return ws;
    }
    return null;
  }

  private sendTo(ws: WebSocket, text: string): boolean {
    try {
      ws.send(text);
      return true;
    } catch {
      return false;
    }
  }

  private fail(ws: WebSocket, code: RelayErrorCode, to?: string): void {
    this.sendTo(ws, errorFrame(code, to));
  }

  private closeSocket(ws: WebSocket, code: number, reason: string): void {
    try {
      ws.serializeAttachment({ s: "closed" } satisfies Attachment);
    } catch {
      // closed already
    }
    try {
      ws.close(code, reason);
    } catch {
      // closed already
    }
  }
}
