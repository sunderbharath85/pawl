import { describe, expect, test } from "bun:test";
import { routeForView } from "../panel/routes";
import type { HubSettings } from "@krypto/protocol";
import {
  agentDraft,
  applyAgentDraft,
  botForm,
  botPatch,
  checkRelayUrl,
  customAgentsPatch,
  hubForm,
  hubPatch,
  msToMinutes,
  nudgesForm,
  nudgesPatch,
  pairingForm,
  pairingPatch,
  relayPatch,
  remoteForm,
  remotePatch,
  reposPatch,
  suggestAgentId,
} from "./settingsForms";

const s: HubSettings = {
  hub: { port: 47800, offline: false, hookSessionStaleHours: 12, ownedSessionIdleHours: 12, eventRetentionDays: 30, dailyReportTime: "18:00" },
  bot: { model: "claude-haiku-5-5", anthropicApiKeySet: true, ollamaUrl: null, ollamaModel: null },
  watchdog: { waitingNudgeAfterMs: 300_000, repeatEveryMs: 600_000, tickSecs: 30 },
  peer: { listen: null, acceptIntents: true, startRoots: ["E:/work"] },
  repos: { paths: ["E:/work/api"] },
  relay: { url: null, accessKeySet: false },
  customAgents: [{ id: "my-agent", name: "My Agent", command: ["my-agent", "--acp"] }],
  overriddenBy: {},
};

describe("hub form", () => {
  test("round-trips without changes", () => {
    expect(hubPatch(hubForm(s), s)).toEqual({ patch: {}, errors: {} });
  });
  test("changed numbers become a patch; 0 allowed where it means never", () => {
    const f = { ...hubForm(s), port: "47900", ownedSessionIdleHours: "0", eventRetentionDays: "0", offline: true };
    expect(hubPatch(f, s)).toEqual({ patch: { hub: { port: 47900, offline: true, ownedSessionIdleHours: 0, eventRetentionDays: 0 } }, errors: {} });
  });
  test("errors are keyed by the hub's field paths and block the patch", () => {
    const f = { ...hubForm(s), port: "80", hookSessionStaleHours: "0", eventRetentionDays: "x" };
    const r = hubPatch(f, s);
    expect(r.patch).toEqual({});
    expect(Object.keys(r.errors).sort()).toEqual(["hub.eventRetentionDays", "hub.hookSessionStaleHours", "hub.port"]);
  });
});

describe("nudges form", () => {
  test("minutes ⇄ ms, report off = empty time", () => {
    expect(msToMinutes(300_000)).toBe("5");
    expect(msToMinutes(90_000)).toBe("1.5");
    const f = nudgesForm(s);
    expect(f).toEqual({ reportOn: true, reportTime: "18:00", nudgeMinutes: "5", repeatMinutes: "10", tickSecs: "30" });
    expect(nudgesPatch(f, s).patch).toEqual({});
    const r = nudgesPatch({ ...f, reportOn: false, nudgeMinutes: "1.5", tickSecs: "15" }, s);
    expect(r.patch).toEqual({ hub: { dailyReportTime: "" }, watchdog: { waitingNudgeAfterMs: 90_000, tickSecs: 15 } });
  });
  test("off in settings shows a default time; turning on pads the time", () => {
    const off = { ...s, hub: { ...s.hub, dailyReportTime: "" } };
    const f = nudgesForm(off);
    expect(f.reportOn).toBe(false);
    expect(f.reportTime).toBe("18:00");
    expect(nudgesPatch({ ...f, reportOn: true, reportTime: "9:05" }, off).patch).toEqual({ hub: { dailyReportTime: "09:05" } });
  });
  test("unchanged minutes keep an exact stored value that does not round-trip", () => {
    const odd = { ...s, watchdog: { ...s.watchdog, waitingNudgeAfterMs: 123_457 } };
    expect(nudgesPatch(nudgesForm(odd), odd).patch).toEqual({});
  });
  test("errors", () => {
    const r = nudgesPatch({ ...nudgesForm(s), reportTime: "25:00", nudgeMinutes: "0", repeatMinutes: "5000", tickSecs: "0" }, s);
    expect(Object.keys(r.errors).sort()).toEqual(["hub.dailyReportTime", "watchdog.repeatEveryMs", "watchdog.tickSecs", "watchdog.waitingNudgeAfterMs"]);
  });
});

describe("bot form", () => {
  test("empty Ollama fields are null", () => {
    expect(botPatch({ ...botForm(s), ollamaUrl: " " }, s).patch).toEqual({});
    expect(botPatch({ ...botForm(s), ollamaUrl: "http://127.0.0.1:11434", ollamaModel: "llama3.2" }, s).patch).toEqual({
      bot: { ollamaUrl: "http://127.0.0.1:11434", ollamaModel: "llama3.2" },
    });
    const withOllama = { ...s, bot: { ...s.bot, ollamaUrl: "http://x:1", ollamaModel: "m" } };
    expect(botPatch({ ...botForm(withOllama), ollamaUrl: "", ollamaModel: "" }, withOllama).patch).toEqual({ bot: { ollamaUrl: null, ollamaModel: null } });
  });
  test("the secret never appears in the form or the patch", () => {
    expect(JSON.stringify(botForm(s))).not.toContain("anthropic");
    expect(botPatch({ ...botForm(s), model: "claude-sonnet-5-5" }, s).patch).toEqual({ bot: { model: "claude-sonnet-5-5" } });
  });
  test("errors", () => {
    const r = botPatch({ model: "", ollamaUrl: "localhost:11434", ollamaModel: "a b" }, s);
    expect(Object.keys(r.errors).sort()).toEqual(["bot.model", "bot.ollamaModel", "bot.ollamaUrl"]);
  });
});

describe("pairing, remote control, repos, relay", () => {
  test("listener on / off", () => {
    expect(pairingForm(s)).toEqual({ on: false, listen: "" });
    expect(pairingPatch({ on: true, listen: " 192.168.1.20:47801 " }, s).patch).toEqual({ peer: { listen: "192.168.1.20:47801" } });
    expect(pairingPatch({ on: true, listen: "my-pc:47801" }, s).errors["peer.listen"]).toBeTruthy();
    const on = { ...s, peer: { ...s.peer, listen: "10.0.0.2:47801" } };
    expect(pairingPatch({ on: false, listen: "10.0.0.2:47801" }, on).patch).toEqual({ peer: { listen: null } });
  });
  test("start folders: per-item errors", () => {
    expect(remotePatch({ ...remoteForm(s), startRoots: ["E:/work", "E:/api"] }, s).patch).toEqual({ peer: { startRoots: ["E:/work", "E:/api"] } });
    const r = remotePatch({ acceptIntents: false, startRoots: ["E:/work", "relative", "e:\\work"] }, s);
    expect(r.errors).toEqual({ "peer.startRoots[1]": expect.stringContaining("full folder path"), "peer.startRoots[2]": expect.stringContaining("already") });
    expect(remotePatch({ acceptIntents: false, startRoots: ["E:/work"] }, s).patch).toEqual({ peer: { acceptIntents: false } });
  });
  test("repos", () => {
    expect(reposPatch([], s).patch).toEqual({ repos: { paths: [] } });
    expect(reposPatch(["x"], s).errors["repos.paths[0]"]).toBeTruthy();
  });
  test("relay URL: wss anywhere, ws only on this computer, empty = off", () => {
    expect(checkRelayUrl("wss://relay.example.dev")).toBeNull();
    expect(checkRelayUrl("https://relay.example.dev/base")).toBeNull();
    expect(checkRelayUrl("ws://127.0.0.1:8787")).toBeNull();
    expect(checkRelayUrl("ws://localhost:8787")).toBeNull();
    expect(checkRelayUrl("ws://relay.example.dev")).toContain("wss://");
    expect(checkRelayUrl("wss://relay.example.dev/?x=1")).toContain("query");
    expect(checkRelayUrl("wss://user@relay.example.dev")).toContain("user");
    expect(checkRelayUrl("relay.example.dev")).toContain("relay URL");
    expect(relayPatch("wss://relay.example.dev", s).patch).toEqual({ relay: { url: "wss://relay.example.dev" } });
    const on = { ...s, relay: { url: "wss://r.dev", accessKeySet: true } };
    expect(relayPatch("", on).patch).toEqual({ relay: { url: null } });
  });
});

describe("custom agents", () => {
  test("add, edit, errors per item", () => {
    const added = applyAgentDraft(s.customAgents, null, { id: "other", name: "Other", commandLine: 'npx -y "other agent"' }, ["claude-code"]);
    expect(added.list).toEqual([...s.customAgents, { id: "other", name: "Other", command: ["npx", "-y", "other agent"] }]);
    expect(customAgentsPatch(added.list!, s)).toEqual({ customAgents: added.list! });
    const bad = applyAgentDraft(s.customAgents, null, { id: "my-agent", name: "", commandLine: "" }, []);
    expect(bad.list).toBeNull();
    expect(Object.keys(bad.errors).sort()).toEqual(["customAgents[1].command", "customAgents[1].id", "customAgents[1].name"]);
    const edited = applyAgentDraft(s.customAgents, 0, { ...agentDraft(s.customAgents[0]), name: "Renamed" }, []);
    expect(edited.list![0]).toEqual({ id: "my-agent", name: "Renamed", command: ["my-agent", "--acp"] });
    expect(customAgentsPatch(s.customAgents, s)).toEqual({});
  });
  test("draft from an agent quotes parts with spaces", () => {
    expect(agentDraft({ id: "x", name: "X", command: ["C:\\Program Files\\x.exe", "--acp"] }).commandLine).toBe('"C:\\Program Files\\x.exe" --acp');
  });
  test("ids suggested from names", () => {
    expect(suggestAgentId("My Agent 2!")).toBe("my-agent-2");
    expect(suggestAgentId("  ")).toBe("");
  });
  test("at most 16", () => {
    const many = Array.from({ length: 16 }, (_, i) => ({ id: `a${i}`, name: `A${i}`, command: ["a"] }));
    expect(applyAgentDraft(many, null, { id: "z", name: "Z", commandLine: "z" }, []).errors["customAgents"]).toContain("16");
  });
});

test("open_panel views map to panel routes", () => {
  expect(routeForView("settings")).toEqual({ view: "settings" });
  expect(routeForView("setup")).toEqual({ view: "setup" });
  expect(routeForView("machines")).toEqual({ view: "machines" });
  expect(routeForView("bogus" as never)).toEqual({ view: "home" });
});
