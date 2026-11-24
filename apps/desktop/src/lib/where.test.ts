import { describe, expect, test } from "bun:test";
import { initialHubState, type HubState } from "@krypto/hub-client";
import type { Machine, Pet } from "@krypto/protocol";
import { buildFeed } from "../panel/feed";
import { FALLBACK_PET_ID, petList } from "./pets";
import { notifiesHere, onMachine } from "./where";

const ME = "winpc0000001";
const MAC = "macmini00002";
const NOW = 50_000_000;
const me: Machine = { id: ME, name: "Windows PC", os: "windows", lastSeen: NOW, online: true };
const mac: Machine = { id: MAC, name: "Mac mini", os: "macos", lastSeen: NOW, online: true };

function state(over: Partial<HubState> = {}): HubState {
  return { ...initialHubState(), machine: me, machines: { [ME]: me, [MAC]: mac }, ...over };
}

describe("notifications go where the user is", () => {
  test("this machine notifies when it is the active one (or nobody is active)", () => {
    expect(notifiesHere(state(), NOW)).toBe(true);
    expect(notifiesHere(state({ machines: { [ME]: { ...me, userActiveAt: NOW - 1000 }, [MAC]: mac } }), NOW)).toBe(true);
  });

  test("it stays quiet while the user is active on another machine", () => {
    const s = state({ machines: { [ME]: { ...me, userActiveAt: NOW - 60_000 }, [MAC]: { ...mac, userActiveAt: NOW - 1000 } } });
    expect(notifiesHere(s, NOW)).toBe(false);
    // ...unless that machine went offline.
    const off = state({ machines: { [ME]: me, [MAC]: { ...mac, online: false, userActiveAt: NOW - 1000 } } });
    expect(notifiesHere(off, NOW)).toBe(true);
  });

  test("before the hub introduced this machine, notify", () => {
    expect(notifiesHere({ machine: undefined, machines: {} }, NOW)).toBe(true);
  });

  test("onMachine names other machines only", () => {
    expect(onMachine(state(), ME)).toBe("");
    expect(onMachine(state(), MAC)).toBe(" on Mac mini");
  });
});

describe("petList", () => {
  const pet = (id: string, machineId: string): Pet => ({
    id,
    machineId,
    agentId: "claude-code",
    species: "dog",
    skin: "default",
    name: id,
    personality: "",
    mood: 70,
    xp: 0,
  });

  test("adds the placeholder dog when this machine has no pet of its own, even with visitors", () => {
    const visitorsOnly = petList(state({ pets: { v: pet(`${MAC}:p`, MAC) } }));
    expect(visitorsOnly.map((p) => p.id)).toEqual([FALLBACK_PET_ID, `${MAC}:p`]);
    expect(visitorsOnly[0]!.machineId).toBe(ME);
    const own = petList(state({ pets: { a: pet(`${ME}:p`, ME), v: pet(`${MAC}:p`, MAC) } }));
    expect(own.map((p) => p.id)).toEqual([`${ME}:p`, `${MAC}:p`]);
  });
});

describe("feed: mirrored payloads that were too large", () => {
  test("render as one 'large output' row per run of truncated events", () => {
    const ev = (seq: number, payload: unknown, type: "agent_message_chunk" | "tool_call" = "agent_message_chunk") => ({
      id: `${MAC}:${seq}`,
      seq,
      machineId: MAC,
      sessionId: `${MAC}:s1`,
      type,
      payload,
      ts: seq,
    });
    const feed = buildFeed([
      ev(1, { text: "Hello" }),
      ev(2, { truncated: true, bytes: 70_000 }),
      ev(3, { truncated: true, bytes: 30_000 }, "tool_call"),
      ev(4, { text: "Done" }),
    ]);
    expect(feed.map((f) => f.kind)).toEqual(["text", "truncated", "text"]);
    expect(feed[1]).toMatchObject({ kind: "truncated", bytes: 100_000, key: `${MAC}:2` });
  });
});
