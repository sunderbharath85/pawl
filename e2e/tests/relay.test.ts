// Relay end to end: the real relay Worker (wrangler dev), two hubs with no LAN listener, and the phone's
// real protocol code (PhoneNode from apps/mobile) over a real WebSocket. Everything crosses the relay sealed.
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { PermissionRequest } from "@pawl/protocol";
import { MemoryStore, PhoneNode } from "../../apps/mobile/src/lib/peer";
import { tempRoot, transcript, waitFor } from "../lib/env";
import { DEMO_AGENT_ID, TestHub } from "../lib/hub";
import { TestRelay } from "../lib/relay";

const tmp = tempRoot("relay");
const repo = join(tmp.dir, "repo");
let relay: TestRelay;
let desk: TestHub;
let laptop: TestHub;
let phone: PhoneNode;

beforeAll(async () => {
  mkdirSync(repo, { recursive: true });
  writeFileSync(join(repo, "README.md"), "# e2e repo\n");
  relay = await TestRelay.start(tmp.dir);
  [desk, laptop] = await Promise.all([
    TestHub.start(tmp.dir, { name: "E2E Desk" }),
    TestHub.start(tmp.dir, { name: "E2E Laptop" }),
  ]);
  for (const hub of [desk, laptop]) {
    await hub.client.updateSettings({ relay: { url: relay.url, accessKey: relay.accessKey } });
    await hub.waitState((s) => s.peer?.relay.connected, "the relay connection");
  }
}, 120_000);

afterAll(async () => {
  await phone?.stop();
  await Promise.all([desk?.stop(), laptop?.stop()]);
  await relay?.stop();
  await tmp.cleanup();
});

async function laptopRequest(prompt: string): Promise<{ sessionId: string; request: PermissionRequest }> {
  const session = await laptop.client.startSession({ agentId: DEMO_AGENT_ID, cwd: repo, prompt });
  const request = await laptop.waitState(
    (s) => Object.values(s.pending).find((p) => p.sessionId === session.id),
    `the request for "${prompt}"`,
  );
  return { sessionId: session.id, request };
}

function laptopAnswer(sessionId: string): Promise<string> {
  return laptop.waitState((s) => transcript(s.events[sessionId]).match(/permission answer: (\w+)/)?.[1], `the answer in ${sessionId}`);
}

describe("relay: two hubs and a phone", () => {
  test("the relay is closed: a wrong access key is refused", async () => {
    const refused = await new Promise<boolean>((done) => {
      const ws = new WebSocket(`${relay.url}/v1/connect?pk=${"A".repeat(43)}&k=wrong`);
      ws.onmessage = () => done(false);
      ws.onerror = () => done(true);
      ws.onclose = () => done(true);
    });
    expect(refused).toBe(true);
  });

  test("hubs with no LAN listener pair through the relay", async () => {
    expect(desk.state.peer?.listening ?? null).toBeNull();
    const invite = await desk.client.createInvite();
    await laptop.client.joinPeer(invite.uri);
    await desk.waitState((s) => s.machines[laptop.machineId]?.online, "the laptop online over the relay");
    await laptop.waitState((s) => s.machines[desk.machineId]?.online, "the desk online over the relay");
    expect(desk.state.devices.find((d) => d.id === laptop.machineId)?.path).toBe("relay");
  }, 60_000);

  test("the desk approves the laptop's request over the relay", async () => {
    const { sessionId, request } = await laptopRequest("run the test suite");
    await desk.waitState((s) => s.pending[request.requestId], "the request mirrored to the desk");
    await desk.client.approve(request.requestId, { confirmed: true });
    expect(await laptopAnswer(sessionId)).toBe("allow");
  }, 60_000);

  test("a phone pairs from a phone invite and reaches both hubs", async () => {
    phone = await PhoneNode.open({ store: new MemoryStore(), name: "E2E Phone", os: "ios" });
    phone.start();
    const invite = await desk.client.createInvite({ forPhone: true });
    const hubView = await phone.pair(invite.uri);
    expect(hubView.id).toBe(desk.machineId);

    // The desk introduces the laptop, and the phone links to it through the relay too.
    await phone.waitFor((s) => s.hubs.length === 2 && s.hubs.every((h) => h.online), 45_000);
    await desk.waitState((s) => s.devices.some((d) => d.kind === "phone" && d.name === "E2E Phone"), "the phone in the desk's roster");
    await laptop.waitState((s) => s.devices.some((d) => d.kind === "phone"), "the phone in the laptop's roster");
  }, 90_000);

  test("the phone sees a request on the laptop and approves it", async () => {
    const { sessionId, request } = await laptopRequest("run the tests from the phone");
    await phone.waitFor((s) => !!s.pending[request.requestId], 30_000);
    const applied = await phone.approve(request.requestId, { confirmed: true });
    expect(applied).toBeTruthy();
    expect(await laptopAnswer(sessionId)).toBe("allow");
  }, 60_000);

  test("the phone cannot approve without confirmation, but can deny", async () => {
    const { sessionId, request } = await laptopRequest("run the release");
    await phone.waitFor((s) => !!s.pending[request.requestId], 30_000);
    await expect(phone.approve(request.requestId, { confirmed: false })).rejects.toThrow();
    await phone.deny(request.requestId);
    expect(await laptopAnswer(sessionId)).toBe("reject");
  }, 60_000);

  test("revoking the phone on the desk cuts it off", async () => {
    const device = desk.state.devices.find((d) => d.kind === "phone")!;
    await desk.client.revokeDevice(device.id);
    await desk.waitState((s) => !s.devices.some((d) => d.id === device.id), "the phone to leave the desk's roster");
    await waitFor(() => !phone.getSnapshot().hubs.find((h) => h.id === desk.machineId)?.online, "the desk to drop the phone", 30_000);
  }, 60_000);

  test("nothing readable crosses the relay or reaches the logs", () => {
    const relayLog = readFileSync(join(tmp.dir, "relay.log"), "utf8");
    for (const logs of [relayLog, desk.logs(), laptop.logs()]) {
      expect(logs).not.toContain("bun test");
      expect(logs).not.toContain("run the test suite");
      expect(logs).not.toContain(relay.accessKey);
    }
  });
});
