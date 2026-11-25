// One hub, end to end: a real `pawl serve`, the scripted ACP agent, the typed client and the CLI.
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { PermissionRequest, Rule } from "@pawl/protocol";
import { tempRoot, transcript } from "../lib/env";
import { DEMO_AGENT_ID, TestHub } from "../lib/hub";

const tmp = tempRoot("hub");
const repo = join(tmp.dir, "repo");
let hub: TestHub;

beforeAll(async () => {
  mkdirSync(repo, { recursive: true });
  writeFileSync(join(repo, "README.md"), "# e2e repo\n");
  hub = await TestHub.start(tmp.dir, { name: "E2E Desk" });
}, 60_000);

afterAll(async () => {
  await hub?.stop();
  await tmp.cleanup();
});

/** Start a demo-agent session and wait for its permission request. */
async function startAndWaitForRequest(prompt: string): Promise<{ sessionId: string; request: PermissionRequest }> {
  const session = await hub.client.startSession({ agentId: DEMO_AGENT_ID, cwd: repo, prompt });
  const request = await hub.waitState(
    (s) => Object.values(s.pending).find((p) => p.sessionId === session.id),
    `a permission request for "${prompt}"`,
  );
  return { sessionId: session.id, request };
}

function answerOf(sessionId: string): Promise<string> {
  return hub.waitState((s) => transcript(s.events[sessionId]).match(/permission answer: (\w+)/)?.[1], `the agent's answer in ${sessionId}`);
}

describe("one hub", () => {
  test("says hello with its machine name and lists the demo agent", async () => {
    expect(hub.state.machine?.name).toBe("E2E Desk");
    const agents = await hub.client.listAgents({ refresh: true });
    const demo = agents.find((a) => a.id === DEMO_AGENT_ID);
    expect(demo?.name).toBe("Demo Agent");
    expect(demo?.installed).toBe(true);
  });

  test("an agent's command waits for approval; the CLI lists it and approves it", async () => {
    const { sessionId, request } = await startAndWaitForRequest("run the test suite");
    expect(request.toolCall.title).toBe("bun test");
    expect(request.toolCall.kind).toBe("execute");

    const pending = await hub.cli(["pending"]);
    expect(pending.code).toBe(0);
    expect(pending.stdout).toContain("bun test");
    expect(pending.stdout).toContain("E2E Desk");

    const approved = await hub.cli(["approve", request.requestId, "--yes"]);
    expect(approved.code).toBe(0);
    expect(approved.stdout).toContain("Approved: bun test");
    expect(await answerOf(sessionId)).toBe("allow");
    await hub.waitState((s) => !s.pending[request.requestId], "the request to leave the pending list");
  });

  test("a deny from a client fails the tool call", async () => {
    const { sessionId, request } = await startAndWaitForRequest("run the tests again");
    await hub.client.deny(request.requestId);
    expect(await answerOf(sessionId)).toBe("reject");
  });

  test("the default rules ask before a read inside the repo", async () => {
    const { sessionId, request } = await startAndWaitForRequest("read the README");
    expect(request.toolCall.kind).toBe("read");
    await hub.client.approve(request.requestId);
    expect(await answerOf(sessionId)).toBe("allow");
  });

  test("a rule the user adds can allow reads inside the repo without asking", async () => {
    const before: Rule[] = hub.state.rules;
    try {
      await hub.client.setRules([
        { id: "e2e-reads", match: { toolKind: "read", path: "{cwd}/**" }, action: "auto_allow" },
        ...before,
      ]);
      const session = await hub.client.startSession({ agentId: DEMO_AGENT_ID, cwd: repo, prompt: "read the README" });
      expect(await answerOf(session.id)).toBe("allow");
      expect(Object.values(hub.state.pending).some((p) => p.sessionId === session.id)).toBe(false);
    } finally {
      await hub.client.setRules(before);
    }
  });

  test("an edit asks, and cancelling the session withdraws the request", async () => {
    const { sessionId, request } = await startAndWaitForRequest("edit the notes");
    expect(request.toolCall.kind).toBe("edit");
    await hub.client.cancel(sessionId);
    await hub.waitState((s) => !s.pending[request.requestId], "the cancelled request to go away");
  });

  test("a deny rule answers for you", async () => {
    const before: Rule[] = hub.state.rules;
    try {
      await hub.client.setRules([{ id: "e2e-no-commands", match: { toolKind: "execute" }, action: "deny" }, ...before]);
      const session = await hub.client.startSession({ agentId: DEMO_AGENT_ID, cwd: repo, prompt: "run the tests" });
      expect(await answerOf(session.id)).toBe("reject");
    } finally {
      await hub.client.setRules(before);
    }
  });

  test("Claude Code hooks feed a tracked session", async () => {
    const hook = (event: Record<string, unknown>) =>
      hub.cli(["hook", "claude"], JSON.stringify({ session_id: "e2e-hook-1", cwd: repo, ...event }));
    expect((await hook({ hook_event_name: "SessionStart", source: "startup" })).code).toBe(0);
    expect((await hook({ hook_event_name: "UserPromptSubmit", prompt: "tidy the readme" })).code).toBe(0);
    const tracked = await hub.waitState(
      (s) => Object.values(s.sessions).find((x) => x.externalId === "e2e-hook-1"),
      "the hook session",
    );
    expect(tracked.cwd.replace(/\\/g, "/").toLowerCase()).toBe(repo.replace(/\\/g, "/").toLowerCase());
    expect((await hook({ hook_event_name: "Stop" })).code).toBe(0);
  });

  test("a pet rename survives a hub restart, and so do the sessions", async () => {
    const pet = await hub.waitState((s) => Object.values(s.pets).find((p) => p.agentId === DEMO_AGENT_ID), "the demo agent's pet");
    // Pet names are unique per hub: pick one no other pet has.
    const taken = new Set(Object.values(hub.state.pets).map((p) => p.name.toLowerCase()));
    const name = ["Biscuit", "Waffle", "Pretzel", "Zephyr", "Quokka"].find((n) => !taken.has(n.toLowerCase()))!;
    await hub.client.updatePet(pet.id, { name, species: "cat" });
    const sessionsBefore = (await hub.client.listSessions(true)).length;

    await hub.restart();

    const after = await hub.waitState((s) => s.pets[pet.id], "the pet after the restart");
    expect(after.name).toBe(name);
    expect(after.species).toBe("cat");
    expect((await hub.client.listSessions(true)).length).toBeGreaterThanOrEqual(sessionsBefore);
  }, 60_000);

  test("the hub never logs prompts, commands or its token", () => {
    const logs = hub.logs();
    expect(logs.length).toBeGreaterThan(0);
    for (const secret of ["bun test", "run the test suite", "tidy the readme", hub.token]) {
      expect(logs).not.toContain(secret);
    }
  });
});
