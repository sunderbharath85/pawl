// Two hubs on the LAN, end to end: pairing from an invite, the mirror of the other machine's sessions,
// answering its permission requests (client and CLI), revoking, and keeping payloads out of the logs.
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { PermissionRequest } from "@pawl/protocol";
import { tempRoot, transcript } from "../lib/env";
import { DEMO_AGENT_ID, TestHub } from "../lib/hub";

const tmp = tempRoot("lan");
const repo = join(tmp.dir, "repo");
let desk: TestHub; // listens for hub links
let laptop: TestHub; // joins the desk's invite
let inviteUri = "";

beforeAll(async () => {
  mkdirSync(repo, { recursive: true });
  writeFileSync(join(repo, "README.md"), "# e2e repo\n");
  [desk, laptop] = await Promise.all([
    TestHub.start(tmp.dir, { name: "E2E Desk", peerListen: true }),
    TestHub.start(tmp.dir, { name: "E2E Laptop" }),
  ]);
}, 60_000);

afterAll(async () => {
  await Promise.all([desk?.stop(), laptop?.stop()]);
  await tmp.cleanup();
});

/** Start a session on the laptop and wait until the desk sees its permission request. */
async function laptopRequestSeenOnDesk(prompt: string): Promise<{ sessionId: string; request: PermissionRequest }> {
  const session = await laptop.client.startSession({ agentId: DEMO_AGENT_ID, cwd: repo, prompt });
  const request = await desk.waitState(
    (s) => Object.values(s.pending).find((p) => p.sessionId === session.id),
    `the laptop's request for "${prompt}"`,
  );
  return { sessionId: session.id, request };
}

function laptopAnswer(sessionId: string): Promise<string> {
  return laptop.waitState((s) => transcript(s.events[sessionId]).match(/permission answer: (\w+)/)?.[1], `the answer in ${sessionId}`);
}

describe("two hubs on the LAN", () => {
  test("the laptop joins the desk's invite and both see each other online", async () => {
    const invite = await desk.client.createInvite({ addr: `127.0.0.1:${desk.peerPort}` });
    inviteUri = invite.uri;
    expect(inviteUri.startsWith("pawl://peer")).toBe(true);
    await laptop.client.joinPeer(inviteUri);

    await desk.waitState((s) => s.machines[laptop.machineId]?.online, "the laptop online on the desk");
    await laptop.waitState((s) => s.machines[desk.machineId]?.online, "the desk online on the laptop");
    expect(desk.state.machines[laptop.machineId]?.name).toBe("E2E Laptop");
    expect(desk.state.devices.find((d) => d.name === "E2E Laptop")?.path).toBe("lan");
  });

  test("an invite works once", async () => {
    await expect(laptop.client.joinPeer(inviteUri)).rejects.toThrow();
  });

  test("a session on the laptop shows up on the desk, and the desk approves its request", async () => {
    const { sessionId, request } = await laptopRequestSeenOnDesk("run the test suite");
    expect(request.machineId).toBe(laptop.machineId);
    expect(desk.state.sessions[sessionId]?.machineId).toBe(laptop.machineId);

    await desk.client.approve(request.requestId, { confirmed: true });
    expect(await laptopAnswer(sessionId)).toBe("allow");
    await desk.waitState((s) => !s.pending[request.requestId], "the request to clear on the desk");
  });

  test("the desk's CLI lists the laptop and its request, and denies it", async () => {
    const { sessionId, request } = await laptopRequestSeenOnDesk("run the tests again");

    const machines = await desk.cli(["machines"]);
    expect(machines.stdout).toContain("E2E Laptop");
    expect(machines.stdout).toContain("online");
    const pending = await desk.cli(["pending"]);
    expect(pending.stdout).toContain("E2E Laptop");
    expect(pending.stdout).toContain("bun test");

    const denied = await desk.cli(["deny", request.requestId]);
    expect(denied.code).toBe(0);
    expect(denied.stdout).toContain("Denied on E2E Laptop");
    expect(await laptopAnswer(sessionId)).toBe("reject");
  });

  test("an approval from another machine needs confirmation", async () => {
    const { request } = await laptopRequestSeenOnDesk("run the build");
    await expect(desk.client.approve(request.requestId)).rejects.toThrow();
    expect(desk.state.pending[request.requestId]).toBeDefined();
    await desk.client.deny(request.requestId);
  });

  test("revoking the laptop drops it and its mirrored sessions", async () => {
    const device = desk.state.devices.find((d) => d.name === "E2E Laptop")!;
    await desk.client.revokeDevice(device.id);
    await desk.waitState((s) => !s.machines[laptop.machineId], "the laptop to leave the desk's machines");
    // Both the hub and a connected client drop the laptop's mirror (the client used to keep it until a reconnect).
    expect((await desk.client.listSessions(true)).some((x) => x.machineId === laptop.machineId)).toBe(false);
    await desk.waitState((s) => !Object.values(s.sessions).some((x) => x.machineId === laptop.machineId), "the mirror to clear");
    expect(Object.values(desk.state.pending).some((p) => p.machineId === laptop.machineId)).toBe(false);
    await laptop.waitState((s) => !s.machines[desk.machineId]?.online, "the desk to go offline on the laptop");
  });

  test("neither hub logs prompts, commands or the invite", () => {
    const secret = new URL(inviteUri.replace(/^pawl:/, "http:")).searchParams;
    const code = [...secret.values()].sort((a, b) => b.length - a.length)[0] ?? inviteUri;
    for (const logs of [desk.logs(), laptop.logs()]) {
      expect(logs).not.toContain("bun test");
      expect(logs).not.toContain("run the test suite");
      expect(logs).not.toContain(inviteUri);
      expect(logs).not.toContain(code);
    }
  });
});
