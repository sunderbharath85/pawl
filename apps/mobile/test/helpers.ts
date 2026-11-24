// Test doubles: a manual clock, deterministic randomness, an in-memory relay that speaks the deployed relay's
// protocol (challenge/auth/ok, allow, allow_pairing, send, not_allowed, offline queues), and a fake hub peer built
// on the same envelope code (playing the Rust hub's part: pairing, hello -> presence, sync, intents, intros).
import nacl from "tweetnacl";
import {
  RELAY_AUTH_CONTEXT,
  type IntroDevice,
  type Intent,
  type IntentRejectReason,
  type Machine,
  type MirrorUpdate,
  type PeerBody,
  type PeerEnvelope,
  type PermissionRequest,
  type Session,
  type HubEvent,
  type Pet,
} from "@pawl/protocol";
import {
  DeviceKeys,
  decode,
  encode,
  encodeUrl,
  open,
  relayAddress,
  seal,
  utf8Decode,
  type Clock,
  type RandomBytes,
  type SocketFactory,
  type SocketLike,
  type TimerHandle,
} from "../src/lib/peer";

// ---------- clock + randomness ----------

export class FakeClock implements Clock {
  t: number;
  #timers = new Map<number, { at: number; fn: () => void }>();
  #next = 1;
  constructor(start = 1_800_000_000_000) {
    this.t = start;
  }
  now(): number {
    return this.t;
  }
  setTimeout(fn: () => void, ms: number): TimerHandle {
    const id = this.#next++;
    this.#timers.set(id, { at: this.t + Math.max(0, ms), fn });
    return id;
  }
  clearTimeout(h: TimerHandle): void {
    this.#timers.delete(h as number);
  }
  /** Move time forward, firing due timers in order (and letting microtasks run between them). */
  async advance(ms: number): Promise<void> {
    const end = this.t + ms;
    for (;;) {
      await flush();
      let next: [number, { at: number; fn: () => void }] | null = null;
      for (const e of this.#timers) if (e[1].at <= end && (!next || e[1].at < next[1].at)) next = e;
      if (!next) break;
      this.#timers.delete(next[0]);
      this.t = Math.max(this.t, next[1].at);
      next[1].fn();
    }
    this.t = end;
    await flush();
  }
  pending(): number {
    return this.#timers.size;
  }
}

/** Let queued microtasks (socket deliveries, snapshot notifications) run. */
export async function flush(rounds = 20): Promise<void> {
  for (let i = 0; i < rounds; i++) await Promise.resolve();
}

/** Deterministic xorshift bytes (tests only). */
export function seededRandom(seed = 1): RandomBytes {
  let x = seed >>> 0 || 1;
  return (n) => {
    const out = new Uint8Array(n);
    for (let i = 0; i < n; i++) {
      x ^= x << 13;
      x >>>= 0;
      x ^= x >>> 17;
      x ^= x << 5;
      x >>>= 0;
      out[i] = x & 0xff;
    }
    return out;
  };
}

// ---------- fake sockets + relay ----------

class FakeSocket implements SocketLike {
  readyState = 0;
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  onclose: ((ev: { code?: number; reason?: string }) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  sent: string[] = [];
  constructor(
    readonly url: string,
    readonly relay: FakeRelay,
  ) {}
  send(data: string): void {
    if (this.readyState !== 1) throw new Error("not open");
    this.sent.push(data);
    queueMicrotask(() => this.relay.fromClient(this, data));
  }
  close(code?: number): void {
    if (this.readyState === 3) return;
    this.readyState = 3;
    queueMicrotask(() => this.relay.closed(this));
    void code;
  }
  /** Relay -> client. */
  deliver(data: string): void {
    if (this.readyState !== 1) return;
    queueMicrotask(() => this.onmessage?.({ data }));
  }
  /** Relay closes the socket. */
  drop(code = 1006): void {
    if (this.readyState === 3) return;
    this.readyState = 3;
    queueMicrotask(() => this.onclose?.({ code }));
  }
}

interface Mailbox {
  pk: string;
  allow: Set<string>;
  slots: Map<string, number>;
  queue: string[];
  socket: FakeSocket | null;
  /** In-process device (a FakeHub) instead of a socket. */
  sink: ((from: string, env: PeerEnvelope) => void) | null;
  pushToken: string | null;
}

interface SocketState {
  pk: string;
  host: string;
  phase: "challenge" | "live";
  nonce: string;
  relaySk: Uint8Array;
}

export class FakeRelay {
  readonly mailboxes = new Map<string, Mailbox>();
  readonly sockets = new Map<FakeSocket, SocketState>();
  readonly log: { from: string; frame: Record<string, unknown> }[] = [];
  readonly errors: { to: string; code: string }[] = [];
  /** Byte 15 of the next challenge nonce (tests set 0/1 to check the client refuses it). */
  nonceByte15: number | null = null;
  /** Use this key as the next challenge's relayPk (tests: own key / roster key refusal). */
  relayPkOverride: Uint8Array | null = null;
  refuseConnections = false;
  readonly random: RandomBytes;
  constructor(
    readonly accessKey: string | null = "k".repeat(43),
    readonly host = "relay.test",
    seed = 7,
  ) {
    this.random = seededRandom(seed);
  }

  get url(): string {
    return `wss://${this.host}`;
  }

  factory: SocketFactory = (url) => {
    const s = new FakeSocket(url, this);
    queueMicrotask(() => this.accept(s));
    return s;
  };

  mailbox(pk: string): Mailbox {
    let m = this.mailboxes.get(pk);
    if (!m) {
      m = { pk, allow: new Set(), slots: new Map(), queue: [], socket: null, sink: null, pushToken: null };
      this.mailboxes.set(pk, m);
    }
    return m;
  }

  accept(s: FakeSocket): void {
    const u = new URL(s.url.replace(/^wss:/, "https:").replace(/^ws:/, "http:"));
    const pk = u.searchParams.get("pk") ?? "";
    if (this.refuseConnections || (this.accessKey !== null && u.searchParams.get("k") !== this.accessKey) || pk.length !== 43) {
      s.drop(1006);
      return;
    }
    s.readyState = 1;
    s.onopen?.({});
    const kp = nacl.box.keyPair.fromSecretKey(this.random(32));
    const nonce = this.random(24);
    nonce[15] = this.nonceByte15 ?? 2 + (nonce[15]! % 254);
    const relayPk = this.relayPkOverride ?? kp.publicKey;
    this.sockets.set(s, { pk, host: u.host, phase: "challenge", nonce: encode(nonce), relaySk: kp.secretKey });
    s.deliver(JSON.stringify({ t: "challenge", nonce: encode(nonce), relayPk: encode(relayPk), v: 1 }));
  }

  closed(s: FakeSocket): void {
    const st = this.sockets.get(s);
    this.sockets.delete(s);
    if (st) {
      const m = this.mailboxes.get(st.pk);
      if (m && m.socket === s) m.socket = null;
    }
  }

  fromClient(s: FakeSocket, data: string): void {
    const st = this.sockets.get(s);
    if (!st) return;
    const f = JSON.parse(data) as Record<string, unknown>;
    this.log.push({ from: st.pk, frame: f });
    if (st.phase === "challenge") {
      const devicePk = Uint8Array.from(Buffer.from(st.pk.replace(/-/g, "+").replace(/_/g, "/") + "=", "base64"));
      const box = typeof f.box === "string" ? decode(f.box) : new Uint8Array();
      const pt = f.t === "auth" ? nacl.box.open(box, decode(st.nonce), devicePk, st.relaySk) : null;
      const want = `${RELAY_AUTH_CONTEXT}:${st.nonce}:${st.host}`;
      if (!pt || utf8Decode(pt) !== want) {
        s.deliver(JSON.stringify({ t: "error", code: "bad_auth" }));
        s.drop(1008);
        return;
      }
      st.phase = "live";
      const m = this.mailbox(st.pk);
      if (m.socket && m.socket !== s) m.socket.drop(4000);
      m.socket = s;
      s.deliver(JSON.stringify({ t: "ok", queued: m.queue.length }));
      for (const q of m.queue.splice(0)) s.deliver(q);
      return;
    }
    const m = this.mailbox(st.pk);
    switch (f.t) {
      case "allow":
        m.allow = new Set(f.pks as string[]);
        return;
      case "allow_pairing":
        m.slots.set(f.token as string, f.exp as number);
        return;
      case "register_push":
        m.pushToken = f.expoToken as string;
        return;
      case "unregister_push":
        m.pushToken = null;
        return;
      case "ping":
        s.deliver(JSON.stringify({ t: "pong", ts: f.ts }));
        return;
      case "send": {
        const ok = this.route(st.pk, f.to as string, f.env as PeerEnvelope, f.pairing as string | undefined);
        if (!ok) {
          this.errors.push({ to: f.to as string, code: "not_allowed" });
          s.deliver(JSON.stringify({ t: "error", code: "not_allowed", to: f.to }));
        }
        return;
      }
    }
  }

  /** Deliver (or queue) a frame from `from` to mailbox `to`. False = not allowed. */
  route(from: string, to: string, env: PeerEnvelope, pairing?: string): boolean {
    const m = this.mailbox(to);
    let ok = m.allow.has(from);
    if (!ok && pairing !== undefined && m.slots.has(pairing)) {
      m.slots.delete(pairing);
      ok = true;
    }
    if (!ok) return false;
    const frame = JSON.stringify({ t: "msg", from, env });
    if (m.sink) queueMicrotask(() => m.sink!(from, JSON.parse(JSON.stringify(env)) as PeerEnvelope));
    else if (m.socket) m.socket.deliver(frame);
    else m.queue.push(frame);
    return true;
  }

  /** Drop every client socket (relay restart). */
  restart(): void {
    for (const s of [...this.sockets.keys()]) s.drop(1012);
    this.sockets.clear();
  }

  /** Frames clients sent with type `t`. */
  framesOf(t: string): Record<string, unknown>[] {
    return this.log.filter((l) => l.frame.t === t).map((l) => l.frame);
  }
}

// ---------- fake hub ----------

export interface FakeHubOptions {
  id: string;
  name: string;
  relay: FakeRelay;
  clock: FakeClock;
  seed?: number;
}

/** A hub peer (in-process device on the FakeRelay) that answers like the Rust hub does. */
export class FakeHub {
  readonly keys: DeviceKeys;
  readonly id: string;
  name: string;
  readonly relay: FakeRelay;
  readonly clock: FakeClock;
  readonly random: RandomBytes;
  /** Paired devices: id -> { pk, rx, tx, kind }. */
  readonly roster = new Map<string, { pk: Uint8Array; rx: number; tx: number; kind: "hub" | "phone"; name: string }>();
  readonly received: { from: string; body: PeerBody }[] = [];
  invites = new Map<string, { code: string; exp: number }>();
  sessions: Session[] = [];
  pets: Pet[] = [];
  pending: PermissionRequest[] = [];
  events: HubEvent[] = [];
  /** How intents are answered (default: like the owner's step-4 rules). */
  answerIntent: ((intent: Intent, confirmed: boolean) => { status: "applied"; result?: unknown } | { status: "rejected"; reason: IntentRejectReason }) | null = null;
  decided = new Set<string>();
  online = true;
  constructor(o: FakeHubOptions) {
    this.id = o.id;
    this.name = o.name;
    this.relay = o.relay;
    this.clock = o.clock;
    this.random = seededRandom(o.seed ?? 99);
    this.keys = DeviceKeys.generate(this.random);
    const m = this.relay.mailbox(this.keys.address);
    m.sink = (from, env) => {
      if (this.online) this.onMsg(from, env);
    };
  }

  machine(): Machine {
    return { id: this.id, name: this.name, os: "macos", lastSeen: this.clock.now(), online: true, hubVersion: "0.8.0" };
  }

  /** A relay-only phone invite (registers the pairing slot), like `create_invite{forPhone: true}`. */
  createPhoneInvite(ttlMs = 600_000): string {
    const code = encodeUrl(this.random(16));
    const rt = encodeUrl(this.random(16));
    const exp = this.clock.now() + ttlMs;
    this.invites.set(code, { code, exp });
    this.relay.mailbox(this.keys.address).slots.set(rt, exp);
    const relay = encodeURIComponent(this.relay.url);
    const rk = this.relay.accessKey ? `&rk=${this.relay.accessKey}` : "";
    return `pawl://peer?v=1&mid=${this.id}&pk=${encodeUrl(this.keys.publicKey)}&code=${code}&exp=${exp}&relay=${relay}&rt=${rt}${rk}`;
  }

  #syncAllow(): void {
    this.relay.mailbox(this.keys.address).allow = new Set([...this.roster.values()].map((d) => relayAddress(d.pk)));
  }

  send(deviceId: string, body: PeerBody): boolean {
    const d = this.roster.get(deviceId);
    if (!d) return false;
    d.tx = Math.max(d.tx + 1, this.clock.now() * 1000);
    const env = seal(this.keys, this.id, deviceId, d.pk, d.tx, body, this.random);
    return this.relay.route(this.keys.address, relayAddress(d.pk), env);
  }

  presence(): PeerBody {
    return { type: "presence", presence: { machine: this.machine(), agents: [], repos: [{ name: "ai-pet", path: "/code/ai-pet" }], pets: this.pets, ts: this.clock.now() } };
  }

  /** Add a device to this hub's roster directly (as if introduced and accepted). */
  addDevice(id: string, pk: Uint8Array, kind: "hub" | "phone", name: string): void {
    this.roster.set(id, { pk, rx: 0, tx: 0, kind, name });
    this.#syncAllow();
  }

  mirror(update: MirrorUpdate): void {
    for (const [id, d] of this.roster) if (d.kind === "phone") this.send(id, { type: "mirror", update });
  }

  intro(to: string, devices: IntroDevice[]): void {
    this.send(to, { type: "roster_intro", devices });
  }

  onMsg(from: string, env: PeerEnvelope): void {
    const fromPk = Uint8Array.from(Buffer.from(from.replace(/-/g, "+").replace(/_/g, "/") + "=", "base64"));
    if (env.pk !== undefined) {
      // pair_request
      const r = open(this.keys, env, fromPk);
      if (!r.ok || r.body.type !== "pair_request") return;
      const body = r.body;
      const inv = this.invites.get(body.code);
      if (!inv) {
        const reject: PeerBody = { type: "pair_reject", reason: "invalid_invite", ts: this.clock.now() };
        this.relay.route(this.keys.address, from, seal(this.keys, this.id, env.from, fromPk, this.clock.now() * 1000, reject, this.random));
        return;
      }
      this.invites.delete(body.code);
      const kind = body.machine.os === "ios" || body.machine.os === "android" ? "phone" : "hub";
      this.roster.set(env.from, { pk: fromPk, rx: env.seq, tx: 0, kind, name: body.machine.name });
      this.#syncAllow();
      this.received.push({ from: env.from, body });
      this.send(env.from, { type: "pair_accept", machine: this.machine(), reqSeq: env.seq, ts: this.clock.now() });
      this.send(env.from, this.presence());
      return;
    }
    const d = this.roster.get(env.from);
    if (!d || encode(d.pk) !== encode(fromPk) || env.seq <= d.rx) return;
    const r = open(this.keys, env, d.pk);
    if (!r.ok) return;
    d.rx = env.seq;
    this.received.push({ from: env.from, body: r.body });
    const b = r.body;
    switch (b.type) {
      case "hello":
        this.send(env.from, this.presence());
        return;
      case "sync_request":
        this.send(env.from, { type: "mirror_snapshot", sessions: this.sessions, pets: this.pets, pendingPermissions: this.pending });
        this.send(env.from, { type: "mirror_events", events: this.events.filter((e) => e.seq > b.sinceSeq) });
        this.send(env.from, { type: "sync_done", seq: Math.max(0, ...this.events.map((e) => e.seq)), truncated: false });
        return;
      case "intent": {
        const ans = this.answerIntent?.(b.intent, b.confirmed) ?? this.#defaultAnswer(b.intent, b.confirmed);
        this.send(env.from, { type: "intent_ack", intentId: b.intentId, ...ans });
        return;
      }
      case "bye":
        if (b.reason === "revoked") {
          this.roster.delete(env.from);
          this.#syncAllow();
        }
        return;
      default:
        return;
    }
  }

  #defaultAnswer(intent: Intent, confirmed: boolean): { status: "applied"; result?: unknown } | { status: "rejected"; reason: IntentRejectReason } {
    if ((intent.kind === "approve" || intent.kind === "start") && !confirmed) return { status: "rejected", reason: "unconfirmed" };
    if (intent.kind === "approve" || intent.kind === "deny") {
      if (this.decided.has(intent.requestId)) return { status: "rejected", reason: "already_decided" };
      const req = this.pending.find((p) => p.requestId === intent.requestId);
      if (!req) return { status: "rejected", reason: "not_found" };
      const opt = req.options.find((o) => o.optionId === intent.optionId);
      if (intent.kind === "approve" && opt?.kind !== "allow_once") return { status: "rejected", reason: "forbidden" };
      this.decided.add(intent.requestId);
      this.pending = this.pending.filter((p) => p !== req);
      return { status: "applied", result: {} };
    }
    if (intent.kind === "start") {
      const session: Session = {
        id: `${this.id}:s-new`,
        machineId: this.id,
        agentId: intent.agentId,
        cwd: intent.cwd,
        source: "owned",
        status: "thinking",
        startedAt: this.clock.now(),
        lastEventAt: this.clock.now(),
      };
      return { status: "applied", result: { session } };
    }
    return { status: "applied", result: {} };
  }

  bodiesFrom(id: string, type: PeerBody["type"]): PeerBody[] {
    return this.received.filter((r) => r.from === id && r.body.type === type).map((r) => r.body);
  }
}

export function sampleSession(hub: string, local = "s1", over: Partial<Session> = {}): Session {
  return {
    id: `${hub}:${local}`,
    machineId: hub,
    agentId: "claude-code",
    cwd: "/code/ai-pet",
    source: "owned",
    status: "working",
    startedAt: 1,
    lastEventAt: 2,
    ...over,
  };
}

export function sampleRequest(hub: string, local = "r1", session = "s1"): PermissionRequest {
  return {
    requestId: `${hub}:${local}`,
    machineId: hub,
    sessionId: `${hub}:${session}`,
    agentId: "claude-code",
    toolCall: { toolCallId: "t1", title: "Run bun test", kind: "execute" },
    options: [
      { optionId: "allow", name: "Allow once", kind: "allow_once" },
      { optionId: "always", name: "Always", kind: "allow_always" },
      { optionId: "reject", name: "Reject", kind: "reject_once" },
    ],
    createdAt: 3,
  };
}

export function sampleEvent(hub: string, seq: number, session = "s1"): HubEvent {
  return { id: `${hub}:${seq}`, seq, machineId: hub, sessionId: `${hub}:${session}`, type: "agent_message_chunk", payload: { text: `m${seq}` }, ts: seq };
}
