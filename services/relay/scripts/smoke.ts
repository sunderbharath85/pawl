// Local smoke test against a running relay (`bunx wrangler dev` in services/relay, default http://127.0.0.1:8787).
// Acts as several devices with real tweetnacl keys and checks the relay's behaviour end to end:
// access key, per-IP connect limit, auth ok / bad auth / auth timeout, challenge nonce domain, allow + send/deliver,
// not_allowed, queue while offline then flush, pairing slot one-shot, too_large, bad_frame, replace, pending flood,
// ping/pong, rate limit.
//
//   bun run scripts/smoke.ts
//   RELAY_URL=https://relay.example.com RELAY_ACCESS_KEY=... bun run scripts/smoke.ts
// The access key comes from RELAY_ACCESS_KEY, else from services/relay/.dev.vars (what `wrangler dev` loads).
// SMOKE_FAST=1 skips the 10 s auth-timeout check. The key is never printed.

import http from "node:http";
import https from "node:https";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import nacl from "tweetnacl";
import type { PeerEnvelope, RelayClientFrame, RelayServerFrame } from "@krypto/protocol";
import { encodeStd, encodeUrl, randomBytes, utf8 } from "../src/b64";
import { answerChallenge, isChallengeNonce } from "../src/auth";

const BASE = (process.env.RELAY_URL ?? "http://127.0.0.1:8787").replace(/\/+$/, "");
const HOST = new URL(BASE).host;
const WS_BASE = BASE.replace(/^http/, "ws");
const FAST = process.env.SMOKE_FAST === "1";
const START = Date.now();
const KEY = accessKey();
/**
 * A per-run client address, sent only to a local relay: `wrangler dev` passes a client-set CF-Connecting-IP through
 * (each run gets its own connect budget), while Cloudflare's edge refuses requests that set it (403).
 */
const LOCAL = /^https?:\/\/(127\.0\.0\.1|localhost|\[::1\])(:|\/|$)/.test(process.env.RELAY_URL ?? "http://127.0.0.1:8787");
const IP_HEADERS: Record<string, string> = {};
const CLIENT_IP = `2001:db8:${[0, 0, 0].map(() => Math.floor(Math.random() * 0xffff).toString(16)).join(":")}::1`;
if (LOCAL) IP_HEADERS["cf-connecting-ip"] = CLIENT_IP;

function accessKey(): string {
  if (process.env.RELAY_ACCESS_KEY) return process.env.RELAY_ACCESS_KEY;
  const file = join(import.meta.dir, "..", ".dev.vars");
  if (existsSync(file)) {
    const m = /^RELAY_ACCESS_KEY\s*=\s*"?([A-Za-z0-9_-]+)"?\s*$/m.exec(readFileSync(file, "utf8"));
    if (m) return m[1]!;
  }
  throw new Error("no access key: set RELAY_ACCESS_KEY or create services/relay/.dev.vars (see README)");
}

const connectUrl = (pk: string, key: string | null = KEY) =>
  `${WS_BASE}/v1/connect?pk=${pk}${key === null ? "" : `&k=${key}`}`;

/** Status of a WebSocket upgrade attempt (101 when accepted; the socket is dropped at once). */
function upgradeStatus(query: string): Promise<number> {
  const u = new URL(`${BASE}/v1/connect?${query}`);
  const mod = u.protocol === "https:" ? https : http;
  return new Promise((resolve, reject) => {
    const req = mod.request(u, {
      headers: {
        connection: "Upgrade",
        upgrade: "websocket",
        "sec-websocket-version": "13",
        "sec-websocket-key": encodeStd(randomBytes(16)),
        ...IP_HEADERS,
      },
    });
    req.on("upgrade", (_res, socket) => {
      socket.destroy();
      resolve(101);
    });
    req.on("response", (res) => {
      res.resume();
      resolve(res.statusCode ?? 0);
    });
    req.on("error", reject);
    req.setTimeout(10_000, () => req.destroy(new Error("timeout")));
    req.end();
  });
}

type Frame = RelayServerFrame;

class Device {
  readonly kp = nacl.box.keyPair.fromSecretKey(randomBytes(32));
  readonly pk = encodeUrl(this.kp.publicKey);
  private seq = Date.now() * 1000;
  constructor(readonly name: string) {}

  /** A real sealed envelope to `to` (the relay never opens it). */
  /** A real sealed envelope with the real nonce layout: 15 random || dir (0/1) || seq u64 BE. */
  envelope(to: Device, body: unknown = { type: "presence", from: this.name }, pad = 0): PeerEnvelope {
    const seq = ++this.seq;
    const nonce = randomBytes(24);
    nonce[15] = Buffer.compare(Buffer.from(this.kp.publicKey), Buffer.from(to.kp.publicKey)) < 0 ? 1 : 0;
    new DataView(nonce.buffer).setBigUint64(16, BigInt(seq));
    const msg = utf8(JSON.stringify({ body, pad: "x".repeat(pad) }));
    const box = nacl.box(msg, nonce, to.kp.publicKey, this.kp.secretKey);
    return { v: 1, from: this.name, to: to.name, seq, nonce: encodeStd(nonce), box: encodeStd(box) };
  }
}

class Conn {
  readonly frames: Frame[] = [];
  closed: { code: number; reason: string } | null = null;
  private waiters: Array<() => void> = [];
  private constructor(readonly ws: WebSocket) {
    ws.addEventListener("message", (e) => {
      const f = JSON.parse(String(e.data)) as Frame;
      if (f.t === "challenge") challengeNonces.push(f.nonce);
      this.frames.push(f);
      this.wake();
    });
    ws.addEventListener("close", (e) => {
      this.closed = { code: e.code, reason: e.reason };
      this.wake();
    });
  }

  private wake(): void {
    for (const w of this.waiters.splice(0)) w();
  }

  static open(pk: string): Promise<Conn> {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(connectUrl(pk), { headers: IP_HEADERS });
      const conn = new Conn(ws);
      ws.addEventListener("open", () => resolve(conn), { once: true });
      ws.addEventListener("error", () => reject(new Error("websocket error")), { once: true });
    });
  }

  send(frame: RelayClientFrame | string): void {
    this.ws.send(typeof frame === "string" ? frame : JSON.stringify(frame));
  }

  /** Wait for (and remove) the first frame matching `pred`. */
  async next<T extends Frame>(pred: (f: Frame) => f is T, ms?: number): Promise<T>;
  async next(pred: (f: Frame) => boolean, ms?: number): Promise<Frame>;
  async next(pred: (f: Frame) => boolean, ms = 5000): Promise<Frame> {
    const deadline = Date.now() + ms;
    for (;;) {
      const i = this.frames.findIndex(pred);
      if (i >= 0) return this.frames.splice(i, 1)[0]!;
      if (this.closed) throw new Error(`socket closed (${this.closed.code} ${this.closed.reason}) while waiting`);
      const left = deadline - Date.now();
      if (left <= 0) throw new Error(`timeout; got ${JSON.stringify(this.frames.map((f) => f.t))}`);
      await new Promise<void>((r) => {
        const t = setTimeout(r, left);
        this.waiters.push(() => {
          clearTimeout(t);
          r();
        });
      });
    }
  }

  async waitClosed(ms = 5000): Promise<{ code: number; reason: string }> {
    const deadline = Date.now() + ms;
    while (!this.closed) {
      if (Date.now() > deadline) throw new Error("socket did not close");
      await new Promise<void>((r) => {
        const t = setTimeout(r, 100);
        this.waiters.push(() => {
          clearTimeout(t);
          r();
        });
      });
    }
    return this.closed;
  }

  /** Round trip through this socket's mailbox: every frame sent before is processed when the pong arrives. */
  async sync(): Promise<void> {
    const ts = Date.now();
    this.send({ t: "ping", ts });
    await this.next((f) => f.t === "pong");
  }

  close(): void {
    this.ws.close(1000, "bye");
  }
}

const isError = (code: string, to?: string) => (f: Frame) => f.t === "error" && f.code === code && (to === undefined || f.to === to);
const isMsg = (f: Frame): f is Extract<Frame, { t: "msg" }> => f.t === "msg";

async function login(d: Device): Promise<{ conn: Conn; queued: number }> {
  const conn = await Conn.open(d.pk);
  const ch = await conn.next((f): f is Extract<Frame, { t: "challenge" }> => f.t === "challenge");
  conn.send({ t: "auth", box: answerChallenge(ch, HOST, d.kp.secretKey) });
  const ok = await conn.next((f): f is Extract<Frame, { t: "ok" }> => f.t === "ok");
  return { conn, queued: ok.queued };
}

async function expectBadAuth(conn: Conn): Promise<void> {
  await conn.next(isError("bad_auth"));
  const c = await conn.waitClosed();
  if (c.code !== 1008) throw new Error(`close code ${c.code}`);
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const results: Array<{ name: string; ok: boolean; err?: string }> = [];
/** Every challenge nonce the relay sent this run (checked against the envelope nonce domain at the end). */
const challengeNonces: string[] = [];

/** Thrown by a check whose expectation is best-effort in this environment: reported, not a failure. */
class Warn extends Error {}
let warnings = 0;

async function check(name: string, fn: () => Promise<void>): Promise<void> {
  const t = Date.now();
  const elapsed = () => `${Date.now() - t} ms, t+${Date.now() - START} ms`;
  try {
    await fn();
    results.push({ name, ok: true });
    console.log(`  ok    ${name} (${elapsed()})`);
  } catch (e) {
    if (e instanceof Warn) {
      warnings++;
      results.push({ name, ok: true });
      console.log(`  WARN  ${name}: ${e.message} (${elapsed()})`);
      return;
    }
    results.push({ name, ok: false, err: (e as Error).message });
    console.log(`  FAIL  ${name}: ${(e as Error).message} (${elapsed()})`);
  }
}

function assert(cond: unknown, what: string): asserts cond {
  if (!cond) throw new Error(what);
}

async function main(): Promise<void> {
  console.log(`relay smoke against ${BASE} (auth host ${HOST})`);
  const A = new Device("dev-a-000001");
  const B = new Device("dev-b-000002");
  const C = new Device("dev-c-000003");

  // The per-IP connect budget (30/min) is shared with an earlier run when the client address cannot be varied:
  // wait for a fresh window instead of failing half-way. A refused attempt does not count against the budget.
  for (let waited = 0; ; waited += 5) {
    const s = await upgradeStatus(`pk=${new Device("probe").pk}&k=${KEY}`);
    if (s === 101) break;
    if (s !== 429 || waited >= 75) throw new Error(`relay not usable: connect status ${s}`);
    if (waited === 0) console.log("  ..    connect budget spent by an earlier run; waiting for the next window");
    await sleep(5000);
  }

  // Started first, checked last: a socket that never answers is closed after 10 s.
  const silent = FAST ? null : await Conn.open(new Device("dev-silent-01").pk);

  await check("GET /health -> {ok:true}", async () => {
    const r = await fetch(`${BASE}/health`);
    assert(r.status === 200, `status ${r.status}`);
    const body = (await r.json()) as unknown;
    assert(JSON.stringify(body) === '{"ok":true}', `body ${JSON.stringify(body)}`);
  });

  await check("plain HTTP refused (404 unknown path, 426 no upgrade)", async () => {
    assert((await fetch(`${BASE}/nope`)).status === 404, "unknown path");
    assert((await fetch(`${BASE}/v1/connect?pk=${A.pk}&k=${KEY}`)).status === 426, "no upgrade");
  });

  await check("access key: missing or wrong -> 401", async () => {
    const missing = await upgradeStatus(`pk=${A.pk}`);
    assert(missing === 401, `missing key: ${missing}`);
    const wrong = await upgradeStatus(`pk=${A.pk}&k=${encodeUrl(randomBytes(32))}`);
    assert(wrong === 401, `wrong key: ${wrong}`);
  });

  await check("bad pk -> 400 before any mailbox", async () => {
    for (const pk of ["short", encodeURIComponent(encodeStd(A.kp.publicKey)), `${A.pk}&pk=${B.pk}`]) {
      const s = await upgradeStatus(`pk=${pk}&k=${KEY}`);
      assert(s === 400, `pk=${pk}: ${s}`);
    }
  });

  await check("bad auth: box from another key -> bad_auth + close 1008", async () => {
    const conn = await Conn.open(A.pk);
    const ch = await conn.next((f): f is Extract<Frame, { t: "challenge" }> => f.t === "challenge");
    conn.send({ t: "auth", box: answerChallenge(ch, HOST, randomBytes(32)) });
    await expectBadAuth(conn);
  });

  await check("bad auth: wrong host in the plaintext", async () => {
    const conn = await Conn.open(A.pk);
    const ch = await conn.next((f): f is Extract<Frame, { t: "challenge" }> => f.t === "challenge");
    conn.send({ t: "auth", box: answerChallenge(ch, "evil.example", A.kp.secretKey) });
    await expectBadAuth(conn);
  });

  await check("bad auth: anything but auth first", async () => {
    const conn = await Conn.open(A.pk);
    await conn.next((f) => f.t === "challenge");
    conn.send({ t: "allow", pks: [B.pk] });
    await expectBadAuth(conn);
  });

  let a = await login(A).catch(() => null);
  let b = await login(B).catch(() => null);
  await check("auth ok -> ok{queued:0}", async () => {
    assert(a && b, "login failed");
    assert(a.queued === 0 && b.queued === 0, `queued ${a.queued}/${b.queued}`);
  });
  if (!a || !b) return;

  await check("ping -> pong (relay clock)", async () => {
    const before = Date.now();
    b!.conn.send({ t: "ping", ts: 1 });
    const p = await b!.conn.next((f): f is Extract<Frame, { t: "pong" }> => f.t === "pong");
    assert(p.ts >= before - 60_000 && p.ts <= Date.now() + 60_000, `pong ts ${p.ts}`);
  });

  await check("send before allow -> not_allowed{to}", async () => {
    b!.conn.send({ t: "send", to: A.pk, env: B.envelope(A) });
    await b!.conn.next(isError("not_allowed", A.pk));
    assert(!a!.conn.frames.some(isMsg), "A got a msg it did not allow");
  });

  await check("allow + send -> msg{from: sender pk, env} delivered live", async () => {
    a!.conn.send({ t: "allow", pks: [B.pk] });
    await a!.conn.sync();
    const env = B.envelope(A, { type: "hello" });
    b!.conn.send({ t: "send", to: A.pk, env });
    const m = await a!.conn.next(isMsg);
    assert(m.from === B.pk, "from is not B's pk");
    assert(JSON.stringify(m.env) === JSON.stringify(env), "envelope changed in transit");
    const opened = nacl.box.open(
      Uint8Array.from(atob(m.env.box), (c) => c.charCodeAt(0)),
      Uint8Array.from(atob(m.env.nonce), (c) => c.charCodeAt(0)),
      B.kp.publicKey,
      A.kp.secretKey,
    );
    assert(opened, "A cannot open B's box");
  });

  await check("frames arrive in send order", async () => {
    const envs = Array.from({ length: 20 }, (_, i) => B.envelope(A, { i }));
    for (const env of envs) b!.conn.send({ t: "send", to: A.pk, env });
    for (const env of envs) {
      const m = await a!.conn.next(isMsg);
      assert(m.env.seq === env.seq, `got seq ${m.env.seq}, want ${env.seq}`);
    }
  });

  await check("bad_frame: not JSON, unknown type, self-send, bad allow, bad push token", async () => {
    b!.conn.send("this is not json");
    await b!.conn.next(isError("bad_frame"));
    b!.conn.send(JSON.stringify({ t: "launch_missiles" }));
    await b!.conn.next(isError("bad_frame"));
    b!.conn.send({ t: "send", to: B.pk, env: B.envelope(B) });
    await b!.conn.next(isError("bad_frame"));
    b!.conn.send({ t: "allow", pks: ["not-a-pk"] });
    await b!.conn.next(isError("bad_frame"));
    b!.conn.send({ t: "register_push", expoToken: "https://example.com/hook" });
    await b!.conn.next(isError("bad_frame"));
    b!.conn.send({ t: "auth", box: encodeStd(randomBytes(48)) });
    await b!.conn.next(isError("bad_frame"));
  });

  await check("register_push then unregister_push (twice) accepted without errors", async () => {
    b!.conn.frames.length = 0;
    b!.conn.send({ t: "register_push", expoToken: "ExponentPushToken[krypto-smoke]" });
    b!.conn.send({ t: "unregister_push" });
    b!.conn.send({ t: "unregister_push" }); // already off: still fine (nothing to write)
    await b!.conn.sync();
    assert(!b!.conn.frames.some((f) => f.t === "error"), `B got ${JSON.stringify(b!.conn.frames)}`);
  });

  await check("too_large: frame over RELAY_FRAME_MAX_BYTES", async () => {
    b!.conn.send({ t: "send", to: A.pk, env: B.envelope(A, {}, 330 * 1024) });
    await b!.conn.next(isError("too_large"));
    // Just under the cap still goes through.
    b!.conn.send({ t: "send", to: A.pk, env: B.envelope(A, {}, 200 * 1024) });
    await a!.conn.next(isMsg);
  });

  await check("queue while offline, flush on connect (in order)", async () => {
    a!.conn.close();
    await a!.conn.waitClosed();
    await sleep(200);
    const envs = [B.envelope(A, { q: 1 }), B.envelope(A, { q: 2 }), B.envelope(A, { q: 3 })];
    for (const env of envs) b!.conn.send({ t: "send", to: A.pk, env });
    await b!.conn.sync();
    await sleep(300);
    assert(!b!.conn.frames.some((f) => f.t === "error"), `B got ${JSON.stringify(b!.conn.frames)}`);
    a = await login(A);
    assert(a.queued === 3, `ok.queued = ${a.queued}`);
    for (const env of envs) {
      const m = await a.conn.next(isMsg);
      assert(m.env.seq === env.seq, `flushed seq ${m.env.seq}, want ${env.seq}`);
    }
    const again = await login(A);
    assert(again.queued === 0, "queue not emptied after flush");
    a = again;
    a.conn.send({ t: "allow", pks: [B.pk] }); // still allowed after reconnect (persisted)
    await a.conn.sync();
  });

  await check("allowlist persists across reconnects", async () => {
    b!.conn.send({ t: "send", to: A.pk, env: B.envelope(A, { persisted: true }) });
    await a!.conn.next(isMsg);
  });

  await check("pairing slot: works once from an unknown sender", async () => {
    const c = await login(C);
    const token = encodeUrl(randomBytes(16));
    a!.conn.send({ t: "allow_pairing", token, exp: Date.now() + 60_000 });
    await a!.conn.sync();
    c.conn.send({ t: "send", to: A.pk, env: C.envelope(A, { type: "pair_request" }), pairing: token });
    const m = await a!.conn.next(isMsg);
    assert(m.from === C.pk, "pairing frame from wrong pk");
    c.conn.send({ t: "send", to: A.pk, env: C.envelope(A, { type: "pair_request" }), pairing: token });
    await c.conn.next(isError("not_allowed", A.pk));
    c.conn.send({ t: "send", to: A.pk, env: C.envelope(A), pairing: encodeUrl(randomBytes(16)) });
    await c.conn.next(isError("not_allowed", A.pk));
    c.conn.close();
  });

  await check("new authenticated socket replaces the old one (close 4000)", async () => {
    const old = a!.conn;
    const fresh = await login(A);
    const c = await old.waitClosed();
    assert(c.code === 4000, `old socket close code ${c.code}`);
    b!.conn.send({ t: "send", to: A.pk, env: B.envelope(A, { after: "replace" }) });
    await fresh.conn.next(isMsg);
    a = fresh;
  });

  await check("pending flood: fresh (maybe mid-auth) sockets are not evicted, stale ones are", async () => {
    // Anyone with the access key who knows A's pk can open sockets to A's mailbox without authenticating.
    const squatters: Conn[] = [];
    for (let i = 0; i < 8; i++) squatters.push(await Conn.open(A.pk));
    for (const s of squatters) await s.next((f) => f.t === "challenge");
    const busy = await upgradeStatus(`pk=${A.pk}&k=${KEY}`);
    assert(busy === 429, `9th pending socket while all 8 are fresh: ${busy}`);
    assert(!squatters.some((s) => s.closed || s.frames.some(isError("bad_auth"))), "a fresh socket was evicted");
    await sleep(3500); // past AUTH_GRACE_MS: the squatters have clearly not answered
    const fresh = await login(A);
    await sleep(100);
    const evicted = squatters.filter((s) => s.frames.some(isError("bad_auth")) || s.closed).length;
    assert(evicted === 1, `${evicted} squatters evicted, want 1`);
    b!.conn.send({ t: "send", to: A.pk, env: B.envelope(A, { after: "flood" }) });
    await fresh.conn.next(isMsg);
    a = fresh;
    for (const s of squatters) s.close();
  });

  await check("rate limit: burst beyond 200 frames -> rate_limited", async () => {
    for (let i = 0; i < 400; i++) b!.conn.send({ t: "ping", ts: i });
    await b!.conn.next(isError("rate_limited"), 10_000);
    await sleep(300);
    const pongs = b!.conn.frames.filter((f) => f.t === "pong").length;
    assert(pongs < 400, `all ${pongs} pings answered`);
    b!.conn.frames.length = 0;
    await sleep(4500); // refill
    await b!.conn.sync();
  });

  if (silent) {
    await check("auth timeout: silent socket gets bad_auth + 1008 after 10 s", async () => {
      await silent.next((f) => f.t === "challenge");
      await silent.next(isError("bad_auth"), 15_000);
      // Under local `wrangler dev`, a close() issued from the alarm handler reaches the client only when the
      // idle Durable Object is evicted (~10 s later); the bad_auth frame itself is immediate.
      const c = await silent.waitClosed(25_000);
      assert(c.code === 1008, `close code ${c.code}`);
    });
  }

  await check("every challenge nonce is outside the envelope nonce domain (byte 15 >= 2)", async () => {
    assert(challengeNonces.length >= 10, `only ${challengeNonces.length} challenges seen`);
    for (const n of challengeNonces) {
      const bytes = Uint8Array.from(atob(n), (c) => c.charCodeAt(0));
      assert(isChallengeNonce(bytes), `challenge nonce byte 15 = ${bytes[15]}`);
    }
    console.log(`        (${challengeNonces.length} challenges checked)`);
  });

  await check("per-IP connect limit -> 429 before any mailbox (30/min)", async () => {
    let attempts = 0;
    let status = 0;
    while (attempts < 45) {
      attempts++;
      status = await upgradeStatus(`pk=${new Device("probe").pk}&k=${KEY}`);
      if (status !== 101) break;
    }
    // The Workers Rate Limiting API is approximate and counted per Cloudflare location, so a deployed relay may let a
    // short burst through: there it is a best-effort backstop (the access key is the real control), not a failure.
    if (status === 101 && !LOCAL) throw new Warn(`no 429 after ${attempts} connects (approximate limit at the edge)`);
    assert(status === 429, `after ${attempts} connects: ${status}`);
    console.log(`        (429 after ${attempts} more connects this run)`);
  });

  a?.conn.close();
  b?.conn.close();
}

await main().catch((e) => {
  results.push({ name: "smoke runner", ok: false, err: (e as Error).message });
  console.log(`  FAIL  smoke runner: ${(e as Error).message}`);
});
const failed = results.filter((r) => !r.ok);
console.log(
  `\n${results.length - failed.length}/${results.length} checks passed` +
    (warnings > 0 ? ` (${warnings} warning${warnings === 1 ? "" : "s"})` : ""),
);
process.exit(failed.length === 0 ? 0 : 1);
