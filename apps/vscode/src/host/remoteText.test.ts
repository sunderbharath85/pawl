import { describe, expect, test } from "bun:test";
import type { Machine, PermissionRequest } from "@pawl/protocol";
import { machineSuffix, openSettingsErrorText, remoteApprovalText, remoteStartText } from "./remoteText";

const ME = "winpc0000001";
const MAC = "macmini00002";
const me: Machine = { id: ME, name: "Windows PC", os: "windows", lastSeen: 1, online: true };
const mac: Machine = { id: MAC, name: "Mac mini", os: "macos", lastSeen: 1, online: true };
const state = {
  machine: me,
  machines: { [ME]: me, [MAC]: mac },
  agents: [{ id: "claude-code", machineId: MAC, name: "Claude Code", command: ["x"], installed: true, source: "registry" as const }],
};
const req: PermissionRequest = {
  requestId: `${MAC}:r1`,
  machineId: MAC,
  sessionId: `${MAC}:s1`,
  agentId: "claude-code",
  toolCall: { toolCallId: "t", title: "Run bun test", kind: "execute", locations: [{ path: "/repo" }] },
  options: [{ optionId: "a", name: "Allow", kind: "allow_once" }],
  createdAt: 1,
};

describe("remote confirmation wording", () => {
  test("approve: machine name first, one go-ahead button naming it", () => {
    const t = remoteApprovalText(state, req);
    expect(t.message).toBe("Approve on Mac mini: Run bun test?");
    expect(t.button).toBe("Approve on Mac mini");
    expect(t.detail).toContain("Claude Code on Mac mini");
    expect(t.detail).toContain("/repo");
  });

  test("start: agent, machine and folder", () => {
    const t = remoteStartText(state, MAC, { name: "Claude Code" }, "/repo");
    expect(t.message).toBe("Start Claude Code on Mac mini in /repo?");
    expect(t.button).toBe("Start on Mac mini");
    expect(t.detail).toBe("It runs on Mac mini, with access to that folder there.");
  });

  test("an unknown machine is 'that machine'; pickers tag only other machines", () => {
    expect(remoteApprovalText({ ...state, machines: {} }, req).button).toBe("Approve on that machine");
    expect(machineSuffix(state, MAC)).toBe(" · Mac mini");
    expect(machineSuffix(state, ME)).toBe("");
  });
});

test("openSettingsErrorText: old hub, timeout, other errors", () => {
  expect(openSettingsErrorText({ code: "unknown_type" })).toContain("too old");
  expect(openSettingsErrorText({ code: "timeout" })).toContain("did not answer");
  expect(openSettingsErrorText(new Error("boom"))).toBe("boom");
  expect(openSettingsErrorText(null)).toBe("Could not reach the desktop app.");
});
