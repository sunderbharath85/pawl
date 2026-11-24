import type { Agent, HubEvent, Pet, PermissionRequest, Session, Welcome } from "@ai-pet/protocol";

export const agent: Agent = { id: "claude-code", name: "Claude Code", command: ["npx", "x"], installed: true, source: "registry" };

export function session(id: string, over: Partial<Session> = {}): Session {
  return { id, agentId: "claude-code", cwd: "/repo", source: "owned", status: "idle", startedAt: 1, lastEventAt: 1, ...over };
}

export function event(id: number, sessionId = "s1", over: Partial<HubEvent> = {}): HubEvent {
  return { id, sessionId, type: "agent_thought_chunk", payload: { text: "hm" }, ts: id * 10, ...over };
}

export const pet: Pet = { id: "p1", agentId: "claude-code", species: "dog", skin: "default", name: "Biscuit", personality: "goofy", mood: 70, xp: 0 };

export function permission(requestId: string, over: Partial<PermissionRequest> = {}): PermissionRequest {
  return {
    requestId,
    sessionId: "s1",
    agentId: "claude-code",
    toolCall: { toolCallId: "t1", title: "Run tests", kind: "execute" },
    options: [
      { optionId: "o-allow", name: "Allow", kind: "allow_once" },
      { optionId: "o-always", name: "Always", kind: "allow_always" },
      { optionId: "o-reject", name: "Reject", kind: "reject_once" },
    ],
    createdAt: 5,
    ...over,
  };
}

export const welcome: Welcome = {
  protocolVersion: 1,
  hubVersion: "0.1.0",
  agents: [agent],
  sessions: [session("s1")],
  pets: [pet],
  pendingPermissions: [permission("r1")],
  rules: [{ id: "rule1", match: { toolKind: "read" }, action: "auto_allow" }],
};

/** In-memory WebSocket double. Tests drive it via the static `instances` list. */
export class FakeSocket {
  static instances: FakeSocket[] = [];
  static failConstruct = false;
  readyState = 0;
  sent: any[] = [];
  closed: { code?: number; reason?: string } | null = null;
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  onclose: ((ev: { code?: number; reason?: string }) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  constructor(public url: string) {
    if (FakeSocket.failConstruct) throw new Error("boom");
    FakeSocket.instances.push(this);
  }
  static reset(): void {
    FakeSocket.instances = [];
    FakeSocket.failConstruct = false;
  }
  static last(): FakeSocket {
    const s = FakeSocket.instances[FakeSocket.instances.length - 1];
    if (!s) throw new Error("no socket");
    return s;
  }
  send(data: string): void {
    if (this.readyState !== 1) throw new Error("not open");
    this.sent.push(JSON.parse(data));
  }
  close(code?: number, reason?: string): void {
    if (this.readyState === 3) return;
    this.closed = { code, reason };
    this.readyState = 3;
    this.onclose?.({ code: code ?? 1005, reason: reason ?? "" });
  }
  // --- test drivers
  open(): void {
    this.readyState = 1;
    this.onopen?.({});
  }
  receive(msg: unknown): void {
    this.onmessage?.({ data: typeof msg === "string" ? msg : JSON.stringify(msg) });
  }
  receiveBytes(msg: unknown): void {
    this.onmessage?.({ data: new TextEncoder().encode(JSON.stringify(msg)) });
  }
  serverClose(code = 1006): void {
    this.readyState = 3;
    this.onclose?.({ code, reason: "" });
  }
  lastSent(): any {
    return this.sent[this.sent.length - 1];
  }
  /** Open + answer hello with `welcome`. */
  handshake(w: Welcome = welcome): void {
    this.open();
    const hello = this.sent.find((m) => m.type === "hello");
    if (!hello) throw new Error("no hello sent");
    this.receive({ type: "ack", id: hello.id, ok: true, result: w });
  }
}

export const tick = (ms = 0) => new Promise((r) => setTimeout(r, ms));
