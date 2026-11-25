import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Agent, HookStatus, HubSettings } from "@pawl/protocol";
import {
  broadFolderReason,
  isBroadFolder,
  checkCustomAgent,
  checkFolder,
  checkListenAddr,
  checkPort,
  checkReportTime,
  checkUrl,
  checkWholeNumber,
  editableSettings,
  fieldIndex,
  fieldMatches,
  folderKey,
  formatCommandLine,
  hookAction,
  hookLabel,
  hookRemovable,
  hookRemoveConfirm,
  HubClient,
  HubRequestError,
  initialHubState,
  isAbsolutePath,
  isEmptyPatch,
  isPanelView,
  isWildcardListen,
  normalizeReportTime,
  normalizeSettingsField,
  parseCommandLine,
  parseListenAddr,
  parsePositiveDecimal,
  parseSettingsError,
  patchKeys,
  reduceServerMsg,
  restartText,
  riskyChanges,
  secretPatch,
  SettingsError,
  settingsErrorText,
  settingsFieldLabel,
  settingsPatch,
  defaultAgentOptions,
  BOT_PROVIDER_LABEL,
  BOT_STYLE_LABEL,
  BOT_STYLES,
  isBotStyle,
  botProviderLabel,
  BOT_PROVIDERS,
  ACP_BOT_AGENTS,
  botProviderNow,
  cloudPrivacyText,
  cloudProviderInUse,
  isBotProvider,
} from "../src/index";
import { configPortFromToml, machineIdFromToml, readHubCredentials, resolveHubCredentials, runtimePortFromJson, verifiedRuntimePort } from "../src/node";
import { FakeSocket, tick, welcome } from "./fixtures";

const base: HubSettings = {
  hub: { port: 47800, offline: false, hookSessionStaleHours: 12, ownedSessionIdleHours: 12, eventRetentionDays: 30, dailyReportTime: "18:00" },
  bot: {
    model: "claude-haiku-5-5",
    anthropicApiKeySet: false,
    ollamaUrl: null,
    ollamaModel: null,
    defaultAgent: null,
    provider: "auto",
    openaiApiKeySet: false,
    openaiModel: "openai-small",
    geminiApiKeySet: false,
    geminiModel: "gemini-flash",
    acpAgent: "claude-code",
    acpModel: "sonnet",
    understand: false,
    style: "plain",
    petName: "Biscuit",
  },
  watchdog: { waitingNudgeAfterMs: 300_000, repeatEveryMs: 600_000, tickSecs: 30 },
  peer: { listen: null, acceptIntents: false, acceptRosterIntros: true, startRoots: ["E:/work"] },
  repos: { paths: [] },
  relay: { url: null, accessKeySet: false },
  customAgents: [{ id: "my-agent", name: "My Agent", command: ["my-agent", "--acp"] }],
  overriddenBy: {},
};

describe("field errors", () => {
  test("normalizes snake_case and numeric segments to the camelCase wire path", () => {
    expect(normalizeSettingsField("hub.port")).toBe("hub.port");
    expect(normalizeSettingsField("peer.start_roots[2]")).toBe("peer.startRoots[2]");
    expect(normalizeSettingsField("custom_agents.1.command")).toBe("customAgents[1].command");
    expect(normalizeSettingsField("watchdog.waiting_nudge_after_ms")).toBe("watchdog.waitingNudgeAfterMs");
  });

  test("parses invalid:<field>: <reason> from strings, HubRequestError and SettingsError", () => {
    expect(parseSettingsError("invalid:hub.port: must be 1024-65535")).toEqual({ field: "hub.port", reason: "must be 1024-65535" });
    expect(parseSettingsError(new HubRequestError("invalid:peer.start_roots[0]: does not exist", "update_settings"))).toEqual({
      field: "peer.startRoots[0]",
      reason: "does not exist",
    });
    expect(parseSettingsError(new SettingsError("relay.url", "must be wss://", "update_settings"))).toEqual({ field: "relay.url", reason: "must be wss://" });
    expect(parseSettingsError("invalid:bot.model")).toEqual({ field: "bot.model", reason: "is not valid" });
  });

  test("other errors are not field errors", () => {
    expect(parseSettingsError("invalid")).toBeNull();
    expect(parseSettingsError("timeout")).toBeNull();
    expect(parseSettingsError(new Error("x"))).toBeNull();
    expect(parseSettingsError(null)).toBeNull();
  });

  test("fieldMatches covers the field and its items; fieldIndex finds the item", () => {
    expect(fieldMatches("peer.startRoots[2]", "peer.startRoots")).toBe(true);
    expect(fieldMatches("customAgents[1].command", "customAgents")).toBe(true);
    expect(fieldMatches("peer.startRootsX", "peer.startRoots")).toBe(false);
    expect(fieldIndex("peer.startRoots[2]", "peer.startRoots")).toBe(2);
    expect(fieldIndex("peer.startRoots", "peer.startRoots")).toBeNull();
  });

  test("labels and error text name the field, never a secret value", () => {
    expect(settingsFieldLabel("hub.port")).toBe("Hub port");
    expect(settingsFieldLabel("peer.start_roots.0")).toBe("Start folders (item 1)");
    expect(settingsFieldLabel("customAgents[1].command")).toBe("Custom agents (item 2, command)");
    expect(settingsErrorText("invalid:hub.port: must be 1024-65535")).toBe("Hub port: must be 1024-65535");
    expect(settingsErrorText(new HubRequestError("unknown_type", "get_settings"))).toContain("too old");
    expect(settingsErrorText(new HubRequestError("timeout", "update_settings"))).toContain("did not answer");
  });
});

describe("client-side checks", () => {
  test("ports", () => {
    expect(checkPort("47800")).toBeNull();
    expect(checkPort(1024)).toBeNull();
    expect(checkPort("80")).toContain("1024");
    expect(checkPort("70000")).toContain("65535");
    expect(checkPort("47.8")).toContain("port number");
    expect(checkPort("")).toContain("port number");
  });

  test("whole numbers and decimals", () => {
    expect(checkWholeNumber("12", 1, 8760, "hours")).toBeNull();
    expect(checkWholeNumber("0", 1, 8760, "hours")).toBe("Use 1–8760 hours.");
    expect(checkWholeNumber("-1", 0, 10, "days")).toContain("whole number");
    expect(parsePositiveDecimal("1.5")).toBe(1.5);
    expect(parsePositiveDecimal("0")).toBeNull();
    expect(parsePositiveDecimal("abc")).toBeNull();
  });

  test("report time", () => {
    expect(checkReportTime("")).toBeNull();
    expect(checkReportTime("18:00")).toBeNull();
    expect(checkReportTime("9:05")).toBeNull();
    expect(checkReportTime("24:00")).not.toBeNull();
    expect(checkReportTime("18:60")).not.toBeNull();
    expect(checkReportTime("6pm")).not.toBeNull();
    expect(normalizeReportTime("9:05")).toBe("09:05");
  });

  test("listen addresses: IP literals with optional port", () => {
    expect(parseListenAddr("192.168.1.20:47801")).toEqual({ ip: "192.168.1.20", port: 47801 });
    expect(parseListenAddr("192.168.1.20")).toEqual({ ip: "192.168.1.20", port: null });
    expect(parseListenAddr("[::1]:47801")).toEqual({ ip: "::1", port: 47801 });
    expect(parseListenAddr("::")).toEqual({ ip: "::", port: null });
    expect(parseListenAddr("fe80::1")).toEqual({ ip: "fe80::1", port: null });
    expect(parseListenAddr("my-pc.local:47801")).toBeNull();
    expect(parseListenAddr("256.1.1.1")).toBeNull();
    expect(parseListenAddr("1.2.3.4:99999")).toBeNull();
    expect(checkListenAddr("192.168.1.20:47801")).toBeNull();
    expect(checkListenAddr("my-pc:47801")).toContain("IP address");
    expect(checkListenAddr("10.0.0.2:80")).toContain("1024");
    expect(checkListenAddr("")).toContain("network address");
    expect(isWildcardListen("0.0.0.0:47801")).toBe(true);
    expect(isWildcardListen("[::]:47801")).toBe(true);
    expect(isWildcardListen("10.0.0.2")).toBe(false);
  });

  test("urls", () => {
    expect(checkUrl("http://127.0.0.1:11434", ["http", "https"])).toBeNull();
    expect(checkUrl("wss://relay.example.dev", ["ws", "wss"])).toBeNull();
    expect(checkUrl("ftp://x", ["http", "https"])).toContain("http://");
    expect(checkUrl("localhost:11434", ["http", "https"])).toContain("full URL");
    expect(checkUrl("", ["ws", "wss"])).toContain("wss://");
  });

  test("folders: absolute, not a duplicate", () => {
    expect(isAbsolutePath("E:\\work")).toBe(true);
    expect(isAbsolutePath("E:/work")).toBe(true);
    expect(isAbsolutePath("\\\\nas\\share")).toBe(true);
    expect(isAbsolutePath("/home/me")).toBe(true);
    expect(isAbsolutePath("work")).toBe(false);
    expect(checkFolder("work")).toContain("full folder path");
    expect(checkFolder("E:\\Work\\", ["e:/work"])).toContain("already");
    expect(checkFolder("E:\\api", ["E:/work"])).toBeNull();
    expect(folderKey("C:\\")).toBe("c:/");
  });

  test("command lines round-trip, Windows backslashes stay literal", () => {
    expect(parseCommandLine('npx -y "@scope/agent acp" --flag')).toEqual(["npx", "-y", "@scope/agent acp", "--flag"]);
    expect(parseCommandLine("C:\\Tools\\agent.exe --acp")).toEqual(["C:\\Tools\\agent.exe", "--acp"]);
    expect(parseCommandLine("  ")).toEqual([]);
    expect(parseCommandLine('a ""')).toEqual(["a", ""]);
    expect(() => parseCommandLine('a "b')).toThrow();
    const argv = ["C:\\Program Files\\agent.exe", "--name", 'say "hi"', "it's", ""];
    expect(parseCommandLine(formatCommandLine(argv))).toEqual(argv);
  });

  test("custom agents: id rules, uniqueness, built-ins, command", () => {
    const all = base.customAgents;
    const ok = checkCustomAgent({ id: "other", name: "Other", commandLine: "other --acp" }, all, null, ["claude-code"]);
    expect(ok).toEqual({ id: null, name: null, command: null });
    const bad = checkCustomAgent({ id: "My Agent", name: " ", commandLine: "" }, all, null, []);
    expect(bad.id).toContain("lowercase");
    expect(bad.name).toContain("name");
    expect(bad.command).toContain("command");
    expect(checkCustomAgent({ id: "my-agent", name: "x", commandLine: "x" }, all, null, []).id).toContain("Another");
    expect(checkCustomAgent({ id: "my-agent", name: "x", commandLine: "x" }, all, 0, []).id).toBeNull();
    expect(checkCustomAgent({ id: "claude-code", name: "x", commandLine: "x" }, all, null, ["claude-code"]).id).toContain("built-in");
    expect(checkCustomAgent({ id: "a", name: "x", commandLine: '"x' }, all, null, []).command).toContain("quote");
  });
});

describe("patches", () => {
  test("no change → empty patch", () => {
    const p = settingsPatch(base, editableSettings(base));
    expect(p).toEqual({});
    expect(isEmptyPatch(p)).toBe(true);
  });

  test("only changed keys; lists replace whole; null turns things off", () => {
    const d = editableSettings(base);
    d.hub.port = 47900;
    d.peer.listen = "192.168.1.20:47801";
    d.peer.startRoots.push("E:/api");
    d.bot.ollamaUrl = "http://127.0.0.1:11434";
    d.relay.url = "wss://relay.example.dev";
    const p = settingsPatch(base, d);
    expect(p).toEqual({
      hub: { port: 47900 },
      bot: { ollamaUrl: "http://127.0.0.1:11434" },
      peer: { listen: "192.168.1.20:47801", startRoots: ["E:/work", "E:/api"] },
      relay: { url: "wss://relay.example.dev" },
    });
    expect(patchKeys(p)).toEqual(["hub.port", "bot.ollamaUrl", "peer.listen", "peer.startRoots", "relay.url"]);
    const on = { ...base, peer: { ...base.peer, listen: "10.0.0.2" } };
    const off = editableSettings(on);
    off.peer.listen = null;
    expect(settingsPatch(on, off)).toEqual({ peer: { listen: null } });
  });

  test("editableSettings copies (editing the draft never touches the base)", () => {
    const d = editableSettings(base);
    d.peer.startRoots.push("x");
    d.customAgents[0]!.command.push("y");
    expect(base.peer.startRoots).toEqual(["E:/work"]);
    expect(base.customAgents[0]!.command).toEqual(["my-agent", "--acp"]);
    expect(settingsPatch(base, d).customAgents?.[0]?.command).toEqual(["my-agent", "--acp", "y"]);
  });

  test("secrets: set (trimmed) or clear, under their own section", () => {
    expect(secretPatch("bot.anthropicApiKey", " sk-x ")).toEqual({ bot: { anthropicApiKey: "sk-x" } });
    expect(secretPatch("bot.anthropicApiKey", null)).toEqual({ bot: { anthropicApiKey: null } });
    expect(secretPatch("relay.accessKey", "k")).toEqual({ relay: { accessKey: "k" } });
    expect(patchKeys(secretPatch("relay.accessKey", "secret-value"))).toEqual(["relay.accessKey"]);
  });
});

describe("risky changes", () => {
  test("nothing risky → no confirm", () => {
    expect(riskyChanges(base, { hub: { port: 47900 }, peer: { listen: null } })).toEqual([]);
    expect(riskyChanges(base, { peer: { acceptIntents: false } })).toEqual([]);
    expect(riskyChanges(base, { peer: { startRoots: [] } })).toEqual([]);
  });

  test("listener on says where it listens and what pairing allows", () => {
    const [c] = riskyChanges(base, { peer: { listen: "0.0.0.0:47801" } }, { machineName: "Windows PC" });
    expect(c!.key).toBe("peer.listen");
    expect(c!.title).toBe("Turn on the peer listener at 0.0.0.0:47801?");
    expect(c!.body).toContain("every network");
    expect(c!.body).toContain("Windows PC");
    expect(c!.body).toContain("invite");
    const [moved] = riskyChanges({ ...base, peer: { ...base.peer, listen: "10.0.0.2" } }, { peer: { listen: "10.0.0.3" } });
    expect(moved!.title).toContain("Move");
  });

  test("remote control on lists what paired machines can do", () => {
    const [c] = riskyChanges(base, { peer: { acceptIntents: true } });
    expect(c!.key).toBe("peer.acceptIntents");
    for (const word of ["start agents", "prompts", "cancel", "approve", "deny"]) expect(c!.body).toContain(word);
  });

  test("new start folders only (case/separator-insensitive), with a note when remote control is off", () => {
    const changes = riskyChanges(base, { peer: { startRoots: ["e:\\work\\", "E:/api"] } });
    expect(changes).toHaveLength(1);
    expect(changes[0]!.title).toBe("Allow starting agents in E:/api?");
    expect(changes[0]!.body).toContain("once remote control is on");
    const withRc = riskyChanges(base, { peer: { acceptIntents: true, startRoots: ["E:/work", "E:/api", "E:/web"] } });
    expect(withRc.map((c) => c.key)).toEqual(["peer.acceptIntents", "peer.startRoots"]);
    expect(withRc[1]!.title).toContain("2 new folders");
    expect(withRc[1]!.body).not.toContain("once remote control is on");
  });

  test("custom agents: added or command changed (renames are fine)", () => {
    const renamed = [{ ...base.customAgents[0]!, name: "Renamed" }];
    expect(riskyChanges(base, { customAgents: renamed })).toEqual([]);
    const changed = [{ ...base.customAgents[0]!, command: ["my-agent", "--acp", "--yolo"] }];
    const [c] = riskyChanges(base, { customAgents: changed });
    expect(c!.title).toBe("Change the command for My Agent?");
    expect(c!.body).toContain("`my-agent --acp --yolo`");
    const added = [...base.customAgents, { id: "x", name: "X", command: ["C:\\x y\\x.exe"] }];
    const [a] = riskyChanges(base, { customAgents: added });
    expect(a!.title).toBe("Add the agent X?");
    expect(a!.body).toContain('`"C:\\x y\\x.exe"`');
  });
});

describe("hooks + restart + panel", () => {
  const claude: HookStatus = { target: "claude", agentInstalled: true, state: "installed", configPath: "C:\\Users\\x\\.claude\\settings.json" };
  test("labels and actions", () => {
    expect(hookLabel(claude)).toBe("Claude Code");
    expect(hookLabel({ target: "copilot-vscode", repo: "E:\\work\\api\\" })).toBe("Copilot in VS Code (api)");
    expect(hookAction({ state: "missing" })).toBe("install");
    expect(hookAction({ state: "outdated" })).toBe("update");
    expect(hookAction({ state: "installed" })).toBeNull();
    expect(hookRemovable({ state: "outdated" })).toBe(true);
    expect(hookRemovable({ state: "unsupported" })).toBe(false);
    const c = hookRemoveConfirm(claude);
    expect(c.body).toContain(claude.configPath);
    expect(c.body).toContain("stop showing up");
  });

  test("restart banner text", () => {
    expect(restartText([])).toBeNull();
    expect(restartText(["hub.port", "hub.port"])).toBe("Restart the hub to apply: Hub port.");
  });

  test("panel views", () => {
    expect(isPanelView("settings")).toBe(true);
    expect(isPanelView("setup")).toBe(true);
    expect(isPanelView("nope")).toBe(false);
  });
});

describe("HubClient settings calls", () => {
  const clients: HubClient[] = [];
  function connected(protocolVersion = 7) {
    const c = new HubClient({ token: "t", client: "desktop", clientVersion: "1", WebSocketImpl: FakeSocket, requestTimeoutMs: 200 });
    clients.push(c);
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake({ ...welcome, protocolVersion });
    return { c, ws };
  }
  beforeEach(() => FakeSocket.reset());
  afterEach(() => {
    for (const c of clients.splice(0)) c.close();
  });

  test("Welcome protocol version lands in state", () => {
    const { c } = connected(7);
    expect(c.getState().protocolVersion).toBe(7);
  });

  test("getSettings folds settings, restartRequired and configPath", async () => {
    const { c, ws } = connected();
    const p = c.getSettings();
    await tick();
    const req = ws.lastSent();
    expect(req).toEqual({ type: "get_settings", id: req.id });
    ws.receive({ type: "ack", id: req.id, ok: true, result: { settings: base, restartRequired: ["hub.port"], configPath: "C:\\k\\config.toml" } });
    const res = await p;
    expect(res.configPath).toBe("C:\\k\\config.toml");
    const st = c.getState();
    expect(st.settings).toEqual(base);
    expect(st.restartRequired).toEqual(["hub.port"]);
    expect(st.settingsConfigPath).toBe("C:\\k\\config.toml");
  });

  test("updateSettings sends the patch; the ack and later broadcasts update state", async () => {
    const { c, ws } = connected();
    const p = c.updateSettings({ hub: { port: 47900 } });
    await tick();
    const req = ws.lastSent();
    expect(req).toEqual({ type: "update_settings", id: req.id, patch: { hub: { port: 47900 } } });
    const saved = { ...base, hub: { ...base.hub, port: 47900 } };
    ws.receive({ type: "ack", id: req.id, ok: true, result: { settings: saved, restartRequired: ["hub.port"], applied: [] } });
    expect((await p).restartRequired).toEqual(["hub.port"]);
    expect(c.getState().settings?.hub.port).toBe(47900);
    const other = { ...saved, bot: { ...saved.bot, anthropicApiKeySet: true } };
    ws.receive({ type: "settings", settings: other, restartRequired: [] });
    expect(c.getState().settings?.bot.anthropicApiKeySet).toBe(true);
    expect(c.getState().restartRequired).toEqual([]);
  });

  test("an invalid field rejects with SettingsError (code invalid, field, reason)", async () => {
    const { c, ws } = connected();
    const p = c.updateSettings({ peer: { startRoots: ["E:/nope"] } });
    await tick();
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: false, error: "invalid:peer.start_roots[0]: folder does not exist" });
    const err = await p.catch((e) => e);
    expect(err).toBeInstanceOf(SettingsError);
    expect(err).toBeInstanceOf(HubRequestError);
    expect(err.code).toBe("invalid");
    expect(err.field).toBe("peer.startRoots[0]");
    expect(err.reason).toBe("folder does not exist");
    expect(err.message).toBe("Start folders (item 1): folder does not exist");
  });

  test("other errors keep their code", async () => {
    const { c, ws } = connected();
    const p = c.getSettings();
    await tick();
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: false, error: "unknown_type" });
    const err = await p.catch((e) => e);
    expect(err).not.toBeInstanceOf(SettingsError);
    expect(err.code).toBe("unknown_type");
  });

  test("an empty patch does not reach the hub once settings are known", async () => {
    const { c, ws } = connected();
    ws.receive({ type: "settings", settings: base, restartRequired: [] });
    const before = ws.sent.length;
    const res = await c.updateSettings({ hub: {} });
    expect(res.settings).toEqual(base);
    expect(ws.sent.length).toBe(before);
  });

  test("secrets go out write-only and are never stored in state", async () => {
    const { c, ws } = connected();
    const p = c.updateSettings(secretPatch("bot.anthropicApiKey", "sk-secret"));
    await tick();
    const req = ws.lastSent();
    expect(req.patch).toEqual({ bot: { anthropicApiKey: "sk-secret" } });
    ws.receive({ type: "ack", id: req.id, ok: true, result: { settings: { ...base, bot: { ...base.bot, anthropicApiKeySet: true } }, restartRequired: [], applied: ["bot.anthropicApiKey"] } });
    await p;
    expect(JSON.stringify(c.getState())).not.toContain("sk-secret");
  });

  test("hookStatus / installHooks wire format", async () => {
    const { c, ws } = connected();
    const hs = c.hookStatus();
    await tick();
    const r1 = ws.lastSent();
    expect(r1).toEqual({ type: "hook_status", id: r1.id });
    const status: HookStatus = { target: "claude", agentInstalled: true, state: "missing", configPath: "/x" };
    ws.receive({ type: "ack", id: r1.id, ok: true, result: { hooks: [status] } });
    expect(await hs).toEqual([status]);

    const pv = c.installHooks("copilot-vscode", "preview", { repo: "E:/work/api" });
    await tick();
    const r2 = ws.lastSent();
    expect(r2).toEqual({ type: "install_hooks", id: r2.id, target: "copilot-vscode", action: "preview", repo: "E:/work/api" });
    ws.receive({ type: "ack", id: r2.id, ok: true, result: { plan: "+ hooks", applied: false, status } });
    expect((await pv).plan).toBe("+ hooks");

    const rm = c.installHooks("claude", "remove");
    await tick();
    expect(ws.lastSent()).toEqual({ type: "install_hooks", id: ws.lastSent().id, target: "claude", action: "remove" });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { plan: "- hooks", applied: true, status } });
    expect((await rm).applied).toBe(true);
  });

  test("openPanel: delivered flag; unknown views never reach the hub", async () => {
    const { c, ws } = connected();
    const p = c.openPanel("settings");
    await tick();
    const req = ws.lastSent();
    expect(req).toEqual({ type: "open_panel", id: req.id, view: "settings" });
    ws.receive({ type: "ack", id: req.id, ok: true, result: { delivered: false } });
    expect(await p).toBe(false);
    const before = ws.sent.length;
    const err = await c.openPanel("bogus" as never).catch((e) => e);
    expect(err.code).toBe("invalid");
    expect(ws.sent.length).toBe(before);
  });

  test("open_panel broadcasts reach onMessage and leave state alone", () => {
    const { c, ws } = connected();
    const seen: string[] = [];
    c.onMessage("open_panel", (m) => seen.push(m.view));
    const before = c.getState();
    ws.receive({ type: "open_panel", view: "machines" });
    expect(seen).toEqual(["machines"]);
    expect(c.getState()).toBe(before);
  });

  test("reducer: settings message", () => {
    const s = reduceServerMsg(initialHubState(), { type: "settings", settings: base, restartRequired: ["hub.port"] });
    expect(s.settings).toBe(base);
    expect(s.restartRequired).toEqual(["hub.port"]);
  });
});

describe("config.toml port (clients follow Settings → Hub port)", () => {
  let root: string;
  beforeAll(() => {
    root = mkdtempSync(join(tmpdir(), "pawl-port-"));
  });
  afterAll(() => rmSync(root, { recursive: true, force: true }));

  test("top-level port only; tables, comments and bad values ignored", () => {
    expect(configPortFromToml("port = 47900\n")).toBe(47900);
    expect(configPortFromToml("\uFEFF# c\r\noffline = true\r\nport=48000 # mine\r\n")).toBe(48000);
    expect(configPortFromToml("# port = 47900\n")).toBeNull();
    expect(configPortFromToml("[peer]\nport = 1\n")).toBeNull();
    expect(configPortFromToml("port = 99999\n")).toBeNull();
    expect(configPortFromToml('port = "47900"\n')).toBeNull();
  });

  test("readHubCredentials: env URL > config port > default", () => {
    const appdata = join(root, "appdata");
    mkdirSync(join(appdata, "pawl"), { recursive: true });
    writeFileSync(join(appdata, "pawl", "token"), "tok");
    const opts = { platform: "win32", env: { APPDATA: appdata } as Record<string, string | undefined> };
    let c = readHubCredentials(opts);
    expect(c.url).toBe("ws://127.0.0.1:47800/ws");
    expect(c.urlSource).toBe("default");
    writeFileSync(join(appdata, "pawl", "config.toml"), "port = 47900\n[peer]\naccept_intents = true\n");
    c = readHubCredentials(opts);
    expect(c.url).toBe("ws://127.0.0.1:47900/ws");
    expect(c.httpUrl).toBe("http://127.0.0.1:47900");
    expect(c.urlSource).toBe("config");
    c = readHubCredentials({ ...opts, env: { ...opts.env, PAWL_HUB_URL: "http://127.0.0.1:48001" } });
    expect(c.url).toBe("ws://127.0.0.1:48001/ws");
    expect(c.urlSource).toBe("env");
  });
});

describe("runtime.json: used only after /health names this machine", () => {
  let root: string;
  beforeAll(() => {
    root = mkdtempSync(join(tmpdir(), "pawl-runtime-"));
  });
  afterAll(() => rmSync(root, { recursive: true, force: true }));

  const BOM = String.fromCharCode(0xfeff);
  test("runtimePortFromJson / machineIdFromToml", () => {
    expect(runtimePortFromJson('{"port":47900,"pid":1,"startedAt":2}')).toBe(47900);
    expect(runtimePortFromJson(`${BOM}{"port":48000}`)).toBe(48000);
    expect(runtimePortFromJson('{"port":"47900"}')).toBeNull();
    expect(runtimePortFromJson('{"port":0}')).toBeNull();
    expect(runtimePortFromJson("not json")).toBeNull();
    expect(runtimePortFromJson("null")).toBeNull();
    expect(machineIdFromToml('id = "abcdef012345"\nname = "PC"\n')).toBe("abcdef012345");
    expect(machineIdFromToml(`${BOM}# m\r\nname = 'PC'\r\nid='abcdef012345' # x\r\n`)).toBe("abcdef012345");
    expect(machineIdFromToml('id = "NOT-VALID"\n')).toBeNull();
    expect(machineIdFromToml('[x]\nid = "abcdef012345"\n')).toBeNull();
  });

  function setup(name: string) {
    const appdata = join(root, name);
    const dir = join(appdata, "pawl");
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "token"), "tok");
    writeFileSync(join(dir, "machine.toml"), 'id = "abcdef012345"\nname = "PC"\n');
    // Saved port 47950 not applied yet; runtime.json says the running hub is on 47900.
    writeFileSync(join(dir, "config.toml"), "port = 47950\n");
    writeFileSync(join(dir, "runtime.json"), JSON.stringify({ port: 47900, pid: 42, startedAt: 1 }));
    return { dir, env: { APPDATA: appdata } as Record<string, string | undefined> };
  }

  test("the sync reader never uses runtime.json", () => {
    const { env } = setup("sync");
    const c = readHubCredentials({ platform: "win32", env });
    expect(c.url).toBe("ws://127.0.0.1:47950/ws");
    expect(c.urlSource).toBe("config");
  });

  test("verified: /health on the runtime port names this machine → runtime port", async () => {
    const { env } = setup("ok");
    const asked: string[] = [];
    const c = await resolveHubCredentials({
      platform: "win32",
      env,
      fetchHealth: async (u) => {
        asked.push(u);
        return { ok: true, machineId: "abcdef012345" };
      },
    });
    expect(asked).toEqual(["http://127.0.0.1:47900"]);
    expect(c.url).toBe("ws://127.0.0.1:47900/ws");
    expect(c.urlSource).toBe("runtime");
    expect(c.token).toBe("tok");
  });

  test("another machine id, no answer, not ok, or no machine.toml → config.toml port", async () => {
    for (const [name, health, dropMachine] of [
      ["other", { ok: true, machineId: "zzzzzz000000" }, false],
      ["noid", { ok: true }, false],
      ["down", null, false],
      ["notok", { ok: false, machineId: "abcdef012345" }, false],
      ["nomachine", { ok: true, machineId: "abcdef012345" }, true],
    ] as const) {
      const { dir, env } = setup(name);
      if (dropMachine) rmSync(join(dir, "machine.toml"));
      const c = await resolveHubCredentials({ platform: "win32", env, fetchHealth: async () => health });
      expect(c.url).toBe("ws://127.0.0.1:47950/ws");
      expect(c.urlSource).toBe("config");
    }
  });

  test("runtime port equal to the configured one, missing or broken: nothing is checked", async () => {
    const { dir, env } = setup("same");
    const never = async () => {
      throw new Error("no check expected");
    };
    writeFileSync(join(dir, "runtime.json"), JSON.stringify({ port: 47950 }));
    expect((await resolveHubCredentials({ platform: "win32", env, fetchHealth: never })).urlSource).toBe("config");
    rmSync(join(dir, "runtime.json"));
    expect((await resolveHubCredentials({ platform: "win32", env, fetchHealth: never })).urlSource).toBe("config");
    writeFileSync(join(dir, "runtime.json"), "{");
    expect(await verifiedRuntimePort(47950, { platform: "win32", env, fetchHealth: never })).toBeNull();
  });

  test("PAWL_HUB_URL wins and skips the check", async () => {
    const { env } = setup("env");
    const c = await resolveHubCredentials({
      platform: "win32",
      env: { ...env, PAWL_HUB_URL: "ws://127.0.0.1:48001/ws" },
      fetchHealth: async () => {
        throw new Error("no check expected");
      },
    });
    expect(c.url).toBe("ws://127.0.0.1:48001/ws");
    expect(c.urlSource).toBe("env");
  });
});

describe("restartHub", () => {
  beforeEach(() => FakeSocket.reset());
  test("sends restart_hub and resolves on the ack; the connection then reconnects by itself", async () => {
    const c = new HubClient({ token: "t", client: "desktop", clientVersion: "1", WebSocketImpl: FakeSocket, requestTimeoutMs: 200, minReconnectDelayMs: 1, maxReconnectDelayMs: 1 });
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake({ ...welcome, protocolVersion: 7 });
    const p = c.restartHub();
    await tick();
    const req = ws.lastSent();
    expect(req).toEqual({ type: "restart_hub", id: req.id });
    ws.receive({ type: "ack", id: req.id, ok: true, result: {} });
    await p;
    ws.serverClose(1001);
    expect(c.getState().status).toBe("disconnected");
    await tick(10);
    expect(FakeSocket.instances.length).toBe(2);
    c.close();
  });
});

describe("very broad folders (start roots and repo paths)", () => {
  test("broadFolderReason", () => {
    expect(broadFolderReason("C:\\")).toBe("a whole drive");
    expect(broadFolderReason("/")).toBe("a whole drive");
    expect(broadFolderReason("\\\\nas\\share")).toBe("a whole network share");
    expect(broadFolderReason("C:\\Users")).toBe("the folder that holds every user's home");
    expect(broadFolderReason("/home")).toBe("the folder that holds every user's home");
    expect(broadFolderReason("C:\\Users\\me\\")).toBe("a home folder");
    expect(broadFolderReason("/Users/me")).toBe("a home folder");
    expect(broadFolderReason("/root")).toBe("a home folder");
    expect(broadFolderReason("E:\\")).toBe("a whole drive");
    expect(broadFolderReason("D:\\Profiles", "D:\\Profiles\\me")).toBe("a folder that contains your home folder");
    expect(broadFolderReason("d:/profiles/me", "D:\\Profiles\\me")).toBe("your home folder");
    // Ordinary project folders are fine.
    for (const ok of ["E:\\work", "E:/work/api", "C:\\Users\\me\\code", "/home/me/src", "/Users/me/ai-pet"]) {
      expect(isBroadFolder(ok, "C:\\Users\\me")).toBe(false);
    }
  });

  test("a broad start root gets its own warning (not the plain item)", () => {
    const changes = riskyChanges(base, { peer: { startRoots: ["E:/work", "C:\\Users\\me", "E:/api"] } }, { acceptIntents: true });
    expect(changes.map((c) => c.key)).toEqual(["peer.startRoots.broad:c:/users/me", "peer.startRoots"]);
    expect(changes[0]!.body).toContain("any paired machine could start agents anywhere under this folder");
    expect(changes[0]!.body).toContain("a home folder");
    expect(changes[0]!.confirm).toBe("Allow anyway");
    expect(changes[1]!.title).toBe("Allow starting agents in E:/api?");
  });

  test("a broad repo path asks too; ordinary repo paths do not", () => {
    expect(riskyChanges(base, { repos: { paths: ["E:/work/api"] } })).toEqual([]);
    const [c] = riskyChanges(base, { repos: { paths: ["/home/me"] } }, { homeDir: "/home/me" });
    expect(c!.key).toBe("repos.paths.broad:/home/me");
    expect(c!.body).toContain("your home folder");
    expect(c!.body).toContain("any paired machine could start agents anywhere under this folder");
    expect(c!.body).toContain("once remote control is on");
  });
});

describe("accept devices introduced by paired hubs (peer.acceptRosterIntros)", () => {
  test("draft and patch carry it; only a change is sent", () => {
    const d = editableSettings(base);
    expect(d.peer.acceptRosterIntros).toBe(true);
    expect(settingsPatch(base, d)).toEqual({});
    d.peer.acceptRosterIntros = false;
    expect(settingsPatch(base, d)).toEqual({ peer: { acceptRosterIntros: false } });
    expect(settingsFieldLabel("peer.accept_roster_intros")).toBe("Accept introduced devices");
  });

  test("a protocol v7 hub (no field) never gets it in a patch", () => {
    const v7 = { ...base, peer: { listen: null, acceptIntents: false, startRoots: ["E:/work"] } } as unknown as HubSettings;
    const d = editableSettings(v7);
    expect(settingsPatch(v7, d)).toEqual({});
    d.peer.startRoots = [];
    expect(settingsPatch(v7, d)).toEqual({ peer: { startRoots: [] } });
  });

  test("turning it on asks first; turning it off does not", () => {
    const off = { ...base, peer: { ...base.peer, acceptRosterIntros: false } };
    const [c] = riskyChanges(off, { peer: { acceptRosterIntros: true } }, { machineName: "Windows PC" });
    expect(c!.key).toBe("peer.acceptRosterIntros");
    expect(c!.title).toBe("Let paired machines add devices to Windows PC?");
    expect(c!.confirm).toBe("Accept introductions");
    expect(c!.body).toContain("without an invite from Windows PC");
    expect(c!.body).toContain("never replaces a device already paired here");
    expect(c!.body).not.toContain("remote control is on");
    const withRemote = riskyChanges({ ...off, peer: { ...off.peer, acceptIntents: true } }, { peer: { acceptRosterIntros: true } });
    expect(withRemote[0]!.body).toContain("because remote control is on");
    expect(riskyChanges(base, { peer: { acceptRosterIntros: false } })).toEqual([]);
    expect(riskyChanges(base, { peer: { acceptRosterIntros: true } })).toEqual([]); // already on
  });
});

describe("default agent (bot.defaultAgent)", () => {
  test("draft and patch carry it; null is Automatic; only a change is sent", () => {
    const d = editableSettings(base);
    expect(d.bot.defaultAgent).toBeNull();
    expect(settingsPatch(base, d)).toEqual({});
    d.bot.defaultAgent = "codex";
    expect(settingsPatch(base, d)).toEqual({ bot: { defaultAgent: "codex" } });
    const picked = { ...base, bot: { ...base.bot, defaultAgent: "codex" } };
    const back = editableSettings(picked);
    back.bot.defaultAgent = null;
    expect(settingsPatch(picked, back)).toEqual({ bot: { defaultAgent: null } });
    expect(settingsFieldLabel("bot.default_agent")).toBe("Default agent");
  });

  test("a hub without the field never gets it in a patch", () => {
    const old = { ...base, bot: { model: "m", anthropicApiKeySet: false, ollamaUrl: null, ollamaModel: null } } as unknown as HubSettings;
    const d = editableSettings(old);
    expect(settingsPatch(old, d)).toEqual({});
    d.bot.model = "other";
    expect(settingsPatch(old, d)).toEqual({ bot: { model: "other" } });
  });

  test("options: Automatic, then this machine's installed agents by name, plus a saved one that is gone", () => {
    const agent = (id: string, name: string, machineId = "m1", installed = true): Agent => ({ id, name, machineId, command: [id], installed, source: "registry" });
    const agents = [agent("codex", "Codex"), agent("claude-code", "Claude Code"), agent("gemini", "Gemini", "m1", false), agent("codex", "Codex", "m2")];
    expect(defaultAgentOptions(agents, "m1", null)).toEqual([
      { value: null, label: "Automatic (most recent session's agent)" },
      { value: "claude-code", label: "Claude Code" },
      { value: "codex", label: "Codex" },
    ]);
    expect(defaultAgentOptions(agents, "m1", "goose").at(-1)).toEqual({ value: "goose", label: "goose (not installed)" });
    expect(defaultAgentOptions(agents, "m1", "codex")).toHaveLength(3);
  });
});

describe("bot provider (provider, OpenAI / Gemini keys and models, understand)", () => {
  test("draft and patch carry them; keys never; only changes are sent", () => {
    const d = editableSettings(base);
    expect(d.bot).toMatchObject({ provider: "auto", openaiModel: "openai-small", geminiModel: "gemini-flash", understand: false });
    expect(JSON.stringify(d)).not.toContain("ApiKey");
    expect(settingsPatch(base, d)).toEqual({});
    d.bot.provider = "gemini";
    d.bot.geminiModel = "gemini-pro";
    d.bot.understand = true;
    expect(settingsPatch(base, d)).toEqual({ bot: { provider: "gemini", geminiModel: "gemini-pro", understand: true } });
    d.bot.acpAgent = "codex";
    d.bot.acpModel = "gpt-5";
    expect(settingsPatch(base, d).bot).toMatchObject({ acpAgent: "codex", acpModel: "gpt-5" });
    d.bot.acpAgent = "";
    expect(settingsPatch(base, d).bot?.acpAgent).toBeUndefined();
    expect(settingsFieldLabel("bot.understand")).toBe("Understand any wording");
    expect(settingsFieldLabel("bot.openai_api_key")).toBe("OpenAI API key");
  });

  test("a hub without the fields never gets them in a patch", () => {
    const old = { ...base, bot: { model: "m", anthropicApiKeySet: false, ollamaUrl: null, ollamaModel: null, defaultAgent: null } } as unknown as HubSettings;
    const d = editableSettings(old);
    expect(settingsPatch(old, d)).toEqual({});
    (d.bot as { provider: unknown }).provider = "nonsense";
    expect(settingsPatch(old, d)).toEqual({});
  });

  test("write-only keys: set, trimmed, or cleared with null", () => {
    expect(secretPatch("bot.openaiApiKey", " sk-x ")).toEqual({ bot: { openaiApiKey: "sk-x" } });
    expect(secretPatch("bot.geminiApiKey", null)).toEqual({ bot: { geminiApiKey: null } });
    expect(secretPatch("bot.anthropicApiKey", "a")).toEqual({ bot: { anthropicApiKey: "a" } });
  });

  test("adding or replacing a cloud key asks first and says what leaves the machine; clearing does not", () => {
    const [add] = riskyChanges(base, { bot: { openaiApiKey: "sk-new" } }, { machineName: "Windows PC" });
    expect(add).toMatchObject({ key: "bot.openaiApiKey", title: "Use OpenAI for the bot?", confirm: "Save key" });
    expect(add!.body).toContain("summary of your sessions");
    expect(add!.body).toContain("never file contents");
    expect(add!.body).toContain("is sent to OpenAI");
    const withGemini = { ...base, bot: { ...base.bot, geminiApiKeySet: true } };
    expect(riskyChanges(withGemini, { bot: { geminiApiKey: "new" } })[0]).toMatchObject({ title: "Replace the Google (Gemini) key?", confirm: "Replace key" });
    expect(riskyChanges(withGemini, { bot: { geminiApiKey: null } })).toEqual([]);
    expect(riskyChanges(base, { bot: { anthropicApiKey: "  " } })).toEqual([]);
    expect(riskyChanges(base, { bot: { provider: "openai" } })).toEqual([]);
  });

  test("switching to a cloud provider with a key, or turning on understand with one in use, asks first", () => {
    const keys = { ...base, bot: { ...base.bot, openaiApiKeySet: true, provider: "ollama" as const } };
    const [sw] = riskyChanges(keys, { bot: { provider: "openai" } }, { machineName: "Windows PC" });
    expect(sw).toMatchObject({ key: "bot.provider", title: "Use OpenAI for the bot?", confirm: "Use OpenAI" });
    expect(sw!.body).toContain("is sent to OpenAI");
    // Auto picks the first cloud provider with a key.
    expect(riskyChanges(keys, { bot: { provider: "auto" } })[0]).toMatchObject({ key: "bot.provider", title: "Use OpenAI for the bot?" });
    // A cloud provider without a key, or a local one: nothing leaves.
    expect(riskyChanges(keys, { bot: { provider: "gemini" } })).toEqual([]);
    expect(riskyChanges({ ...keys, bot: { ...keys.bot, provider: "openai" } }, { bot: { provider: "acp" } })).toEqual([]);
    // Saving the key in the same change asks once (the key's own confirm).
    expect(riskyChanges(keys, { bot: { provider: "gemini", geminiApiKey: "k" } }).map((c) => c.key)).toEqual(["bot.geminiApiKey"]);
    // Understand on while a cloud provider is in use.
    const inUse = { ...keys, bot: { ...keys.bot, provider: "openai" as const } };
    const [u] = riskyChanges(inUse, { bot: { understand: true } });
    expect(u).toMatchObject({ key: "bot.understand", title: "Let OpenAI read what you write?", confirm: "Turn on" });
    expect(riskyChanges(keys, { bot: { understand: true } })).toEqual([]);
    expect(riskyChanges(inUse, { bot: { understand: false } })).toEqual([]);
    expect(cloudProviderInUse("auto", { geminiApiKeySet: true })).toEqual({ provider: "gemini", name: "Google (Gemini)" });
    expect(cloudProviderInUse("acp", { anthropicApiKeySet: true })).toBeNull();
  });

  test("what Auto uses right now (Settings says it, because an agent over ACP spends the person's plan)", () => {
    const claude = { id: "claude-code", name: "Claude Code", installed: true, machineId: "m1" };
    const bot = { ...base.bot, provider: "auto" as const };
    expect(botProviderNow(bot, [claude], "m1")).toEqual({ provider: "acp", text: "Auto: using Claude Code on your Claude plan", usesPlan: true });
    expect(botProviderNow({ ...bot, openaiApiKeySet: true }, [claude], "m1")).toEqual({ provider: "openai", text: "Auto: using OpenAI (your API key)", usesPlan: false });
    expect(botProviderNow({ ...bot, anthropicApiKeySet: true, geminiApiKeySet: true }, [claude], "m1").provider).toBe("anthropic");
    expect(botProviderNow({ ...bot, ollamaUrl: "http://x:1", ollamaModel: "llama3.2" }, [claude], "m1").text).toBe("Auto: using Ollama (llama3.2)");
    // Claude Code not installed here (or only on another machine): nothing.
    expect(botProviderNow(bot, [{ ...claude, machineId: "m2" }], "m1")).toMatchObject({ provider: null, usesPlan: false });
    expect(botProviderNow(bot, [], "m1").text).toBe("Auto: nothing is set up yet, so your pet answers with simple built-in replies.");
    // A chosen provider that is not set up says what it needs.
    expect(botProviderNow({ ...bot, provider: "openai" }, [claude], "m1").text).toBe("OpenAI needs its API key; until then your pet answers with simple built-in replies.");
    expect(botProviderNow({ ...bot, provider: "acp" }, [claude], "m1").text).toBe("Using Claude Code on your Claude plan");
    expect(ACP_BOT_AGENTS).toEqual(["claude-code"]);
  });

  test("how Pawl talks (bot.style): plain by default, only a change is sent, never to a hub without it", () => {
    const d = editableSettings(base);
    expect(d.bot.style).toBe("plain");
    d.bot.style = "playful";
    expect(settingsPatch(base, d)).toEqual({ bot: { style: "playful" } });
    expect(settingsFieldLabel("bot.style")).toBe("How Pawl talks");
    expect(BOT_STYLES).toEqual(["plain", "playful"]);
    expect(BOT_STYLE_LABEL.playful).toBe("Playful");
    expect(isBotStyle("loud")).toBe(false);
    const old = { ...base, bot: { ...base.bot, style: undefined } } as unknown as HubSettings;
    const od = editableSettings(old);
    expect(settingsPatch(old, od)).toEqual({});
    (od.bot as { style: unknown }).style = "shouty";
    expect(settingsPatch(old, od)).toEqual({});
  });

  test("the default pet's name (bot.petName): trimmed, only a real change is sent, never to a hub without it", () => {
    const d = editableSettings(base);
    expect(d.bot.petName).toBe("Biscuit");
    d.bot.petName = "  Biscuit ";
    expect(settingsPatch(base, d)).toEqual({});
    d.bot.petName = " Mochi ";
    expect(settingsPatch(base, d)).toEqual({ bot: { petName: "Mochi" } });
    expect(settingsFieldLabel("bot.pet_name")).toBe("Default pet name");
    const old = { ...base, bot: { ...base.bot, petName: undefined } } as unknown as HubSettings;
    const od = editableSettings(old);
    (od.bot as { petName: unknown }).petName = "Rex";
    expect(settingsPatch(old, od)).toEqual({});
  });

  test("providers and the privacy text", () => {
    expect(BOT_PROVIDERS).toEqual(["auto", "acp", "anthropic", "openai", "gemini", "ollama"]);
    expect(botProviderLabel("acp")).toBe("Claude Code (your Claude plan, no API key)");
    expect(botProviderLabel("acp", "Codex")).toBe("Codex (your plan, no API key)");
    expect(botProviderLabel("openai", "Codex")).toBe("OpenAI");
    expect(isBotProvider("ollama")).toBe(true);
    expect(isBotProvider("mistral")).toBe(false);
    expect(BOT_PROVIDER_LABEL.auto).toBe("Automatic");
    expect(cloudPrivacyText("OpenAI")).toContain("titles, repo names, states and pending approvals");
  });
});
