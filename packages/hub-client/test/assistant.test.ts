import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import type { Agent, ChatMessage, HubEvent, HubSettings, HubSettingsAssistant, PermissionRequest } from "@pawl/protocol";
import {
  ASSISTANT_ASKS,
  assistantActivity,
  assistantAgentChoices,
  assistantAgentName,
  assistantAgentNote,
  assistantAsker,
  assistantStartCard,
  isAssistantReply,
  isAssistantRequest,
  stripApprovalWords,
  assistantAgentProblem,
  assistantIntro,
  assistantOf,
  assistantPatch,
  assistantStatus,
  assistantWorkingLine,
  defaultAssistantWorkspace,
  HubClient,
  initialHubState,
  isAssistantSession,
  isDefaultWorkspace,
  isStreaming,
  reduceServerMsg,
  riskyChanges,
  settingsFieldLabel,
  tildePath,
  toolActivity,
} from "../src/index";
import { event, FakeSocket, MID, tick, welcome } from "./fixtures";

const HOME = "C:\\Users\\you";
const WS = "C:\\Users\\you\\Pawl";

function asst(over: Partial<HubSettingsAssistant> = {}): HubSettingsAssistant {
  return { enabled: true, agent: "auto", workspace: WS, sessions: true, browser: true, resolvedAgent: "claude-code", state: "idle", ...over };
}

const agents: Agent[] = [
  { id: "claude-code", machineId: MID, name: "Claude Code", command: ["npx", "x"], installed: true, source: "registry" },
  { id: "codex", machineId: MID, name: "Codex", command: ["npx", "y"], installed: true, source: "registry" },
  { id: "gemini", machineId: MID, name: "Gemini CLI", command: ["gemini"], installed: false, source: "registry" },
  { id: "a2a:helper", machineId: MID, name: "Helper", command: [], installed: true, source: "a2a" },
  { id: "claude-code", machineId: "macmini00002", name: "Claude Code", command: ["npx", "x"], installed: true, source: "registry" },
];

function settings(over: Partial<HubSettingsAssistant> = {}, rest: Partial<HubSettings> = {}): HubSettings {
  return {
    hub: { port: 47800, offline: false, hookSessionStaleHours: 12, ownedSessionIdleHours: 12, eventRetentionDays: 30, dailyReportTime: "" },
    bot: {
      model: "m",
      anthropicApiKeySet: false,
      ollamaUrl: null,
      ollamaModel: null,
      defaultAgent: null,
      provider: "auto",
      openaiApiKeySet: false,
      openaiModel: "o",
      geminiApiKeySet: false,
      geminiModel: "g",
      acpAgent: "claude-code",
      acpModel: "sonnet",
      understand: false,
      style: "plain",
      petName: "Ollie",
    },
    watchdog: { waitingNudgeAfterMs: 1, repeatEveryMs: 1, tickSecs: 30 },
    peer: { listen: null, acceptIntents: false, acceptRosterIntros: true, startRoots: [] },
    repos: { paths: [] },
    relay: { url: null, accessKeySet: false },
    customAgents: [],
    overriddenBy: {},
    a2a: { server: false, port: 41241, running: false, url: null, error: null, callers: [] },
    browser: { enabled: true, allowSites: [] },
    assistant: asst(over),
    ...rest,
  };
}

describe("assistant settings", () => {
  test("assistantOf: null for a hub from before Assistant mode", () => {
    const { assistant: _gone, ...old } = settings();
    expect(assistantOf(old as HubSettings)).toBeNull();
    expect(assistantOf(undefined)).toBeNull();
    expect(assistantOf(settings())?.agent).toBe("auto");
  });

  test("the dropdown: Automatic (what auto picked), then this machine's installed ACP agents", () => {
    expect(assistantAgentChoices(asst(), agents, MID)).toEqual([
      { value: "auto", label: "Automatic (Claude Code)", installed: true },
      { value: "claude-code", label: "Claude Code", installed: true },
      { value: "codex", label: "Codex", installed: true },
    ]);
  });

  test("auto with nothing installed says so", () => {
    const none = assistantAgentChoices(asst({ resolvedAgent: undefined }), [], MID);
    expect(none).toEqual([{ value: "auto", label: "Automatic (none installed)", installed: false }]);
    expect(assistantAgentProblem(asst({ resolvedAgent: undefined }), [], MID)).toContain("No coding agent is installed");
  });

  test("a chosen agent that isn't installed stays in the list, marked, with the problem", () => {
    const a = asst({ agent: "gemini", resolvedAgent: undefined });
    const choices = assistantAgentChoices(a, agents, MID);
    expect(choices.at(-1)).toEqual({ value: "gemini", label: "Gemini CLI (not installed)", installed: false });
    // The auto label still names what auto would pick.
    expect(choices[0]!.label).toBe("Automatic (Claude Code)");
    expect(assistantAgentProblem(a, agents, MID)).toBe("Gemini CLI is not installed on this machine. Install it, or pick another agent.");
    expect(assistantAgentProblem(asst({ agent: "codex", resolvedAgent: "codex" }), agents, MID)).toBeNull();
  });

  test("agent name: the resolved agent, null when none", () => {
    expect(assistantAgentName(asst({ resolvedAgent: "codex" }), agents)).toBe("Codex");
    expect(assistantAgentName(asst({ resolvedAgent: undefined }), agents)).toBeNull();
    // An agent the list doesn't know yet still gets a readable name.
    expect(assistantAgentName(asst({ resolvedAgent: "github-copilot-cli" }), [])).toBe("Copilot CLI");
  });

  test("patch: only what changed; '' resets the workspace to the hub's default", () => {
    expect(assistantPatch(asst(), { enabled: true, sessions: true })).toEqual({});
    expect(assistantPatch(asst(), { enabled: false })).toEqual({ assistant: { enabled: false } });
    expect(assistantPatch(asst(), { workspace: "" })).toEqual({ assistant: { workspace: "" } });
    expect(assistantPatch(asst(), { agent: "codex", browser: false })).toEqual({ assistant: { agent: "codex", browser: false } });
  });

  test("status line: Off / Ready / Working / the hub's error as-is", () => {
    expect(assistantStatus(asst({ enabled: false, state: "off" }))).toEqual({ label: "Off", tone: "idle", error: null });
    expect(assistantStatus(asst())).toEqual({ label: "Ready", tone: "done", error: null });
    expect(assistantStatus(asst({ state: "working" }))).toEqual({ label: "Working", tone: "working", error: null });
    expect(assistantStatus(asst({ state: "error", error: "Claude Code is not installed" }))).toEqual({
      label: "Can't start",
      tone: "error",
      error: "Claude Code is not installed",
    });
    expect(assistantStatus(asst({ state: "error" })).error).toBe("The assistant could not start.");
  });

  test("intro and the bubble's line", () => {
    expect(assistantIntro("Ollie", "Claude Code", "~/Pawl")).toBe(
      "Ollie does the work with Claude Code in ~/Pawl. Changes, commands, web use and new sessions ask you first, by your approval rules.",
    );
    expect(assistantWorkingLine("Claude Code", "~/Pawl")).toBe("Working with Claude Code in ~/Pawl");
    expect(assistantWorkingLine(null, "~/Pawl")).toBe("Assistant: no coding agent installed");
  });

  test("field labels", () => {
    expect(settingsFieldLabel("assistant.workspace")).toBe("Assistant workspace");
    expect(settingsFieldLabel("assistant.agent")).toBe("Assistant agent");
  });
});

describe("workspace paths", () => {
  test("tildePath: under home becomes ~/..., case-insensitive on Windows", () => {
    expect(tildePath(WS, HOME)).toBe("~/Pawl");
    expect(tildePath("c:\\users\\YOU\\Pawl\\jobs\\", HOME)).toBe("~/Pawl/jobs");
    expect(tildePath(HOME, HOME)).toBe("~");
    expect(tildePath("C:\\Users\\yourself\\Pawl", HOME)).toBe("C:\\Users\\yourself\\Pawl");
    expect(tildePath("/home/you/Pawl", "/home/you")).toBe("~/Pawl");
    expect(tildePath("/home/You/Pawl", "/home/you")).toBe("/home/You/Pawl");
    expect(tildePath("E:\\work", null)).toBe("E:\\work");
  });

  test("the default spelled out, and recognised", () => {
    expect(defaultAssistantWorkspace(HOME)).toBe(WS);
    expect(defaultAssistantWorkspace("/home/you/")).toBe("/home/you/Pawl");
    expect(defaultAssistantWorkspace(null)).toBe("~/Pawl");
    expect(isDefaultWorkspace(WS, HOME)).toBe(true);
    expect(isDefaultWorkspace("E:\\jobs", HOME)).toBe(false);
  });
});

describe("assistant confirms", () => {
  const ctx = { homeDir: HOME };

  test("turning it on says what it means, and the A2A server going on", () => {
    const base = settings({ enabled: false, state: "off" });
    const [c] = riskyChanges(base, { assistant: { enabled: true } }, ctx);
    expect(c!.key).toBe("assistant.enabled");
    expect(c!.title).toBe("Let Ollie do the work?");
    expect(c!.body).toContain("working in ~/Pawl");
    expect(c!.body).toContain("where it may read files without asking");
    expect(c!.body).toContain("Changes, commands, web use and new sessions ask you first, by your approval rules.");
    expect(c!.body).not.toMatch(/every step/i);
    expect(c!.body).toContain("can never approve anything itself");
    expect(c!.body).toContain("This turns on Pawl's A2A server, on 127.0.0.1 only.");
    // Server already on: no server line.
    const on = settings({ enabled: false }, { a2a: { server: true, port: 41241, running: true, url: null, error: null, callers: [] } });
    expect(riskyChanges(on, { assistant: { enabled: true } }, ctx)[0]!.body).not.toContain("A2A server");
  });

  test("session tools on asks; off, or turning it off, does not", () => {
    const base = settings({ sessions: false });
    const [c] = riskyChanges(base, { assistant: { sessions: true } }, ctx);
    expect(c!.title).toBe("Let Ollie start and steer other sessions?");
    expect(c!.body).toContain("Each start asks you first");
    expect(riskyChanges(settings(), { assistant: { sessions: false } }, ctx)).toEqual([]);
    expect(riskyChanges(settings(), { assistant: { enabled: false } }, ctx)).toEqual([]);
  });

  test("the browser asks only while browser use is on", () => {
    const [c] = riskyChanges(settings({ browser: false }), { assistant: { browser: true } }, ctx);
    expect(c!.title).toBe("Let Ollie use your Chrome?");
    const off = settings({ browser: false }, { browser: { enabled: false, allowSites: [] } });
    expect(riskyChanges(off, { assistant: { browser: true } }, ctx)).toEqual([]);
  });

  test("a very broad workspace asks; a folder of its own does not; reset does not", () => {
    const [c] = riskyChanges(settings(), { assistant: { workspace: HOME } }, ctx);
    expect(c!.key).toBe("assistant.workspace");
    expect(c!.body).toContain("your home folder");
    expect(riskyChanges(settings(), { assistant: { workspace: "E:\\" } }, ctx)[0]!.body).toContain("a whole drive");
    expect(riskyChanges(settings(), { assistant: { workspace: "E:\\jobs" } }, ctx)).toEqual([]);
    expect(riskyChanges(settings(), { assistant: { workspace: "" } }, ctx)).toEqual([]);
  });
});

describe("assistant sessions and streaming", () => {
  test("isAssistantSession / isStreaming", () => {
    expect(isAssistantSession({ purpose: "assistant" })).toBe(true);
    expect(isAssistantSession({ purpose: "browser" })).toBe(false);
    expect(isAssistantSession({})).toBe(false);
    expect(isStreaming({ streaming: true })).toBe(true);
    expect(isStreaming({ streaming: false })).toBe(false);
    expect(isStreaming({})).toBe(false);
  });

  test("chat_update replaces the message with the same id in place (streaming), and adds a missed one", () => {
    const m = (text: string, over: Partial<ChatMessage> = {}): ChatMessage => ({ id: "r1", machineId: MID, role: "pet", text, ts: 20, sessionId: "s1", ...over });
    let s = reduceServerMsg(initialHubState(), { type: "chat", message: { id: "u1", machineId: MID, role: "user", text: "make a todo app", ts: 10 } });
    s = reduceServerMsg(s, { type: "chat", message: m("", { streaming: true }) });
    s = reduceServerMsg(s, { type: "chat_update", message: m("On it.", { streaming: true }) });
    expect(s.chat.map((c) => c.id)).toEqual(["u1", "r1"]);
    expect(s.chat[1]).toMatchObject({ text: "On it.", streaming: true });
    s = reduceServerMsg(s, { type: "chat_update", message: m("On it. Done: ~/Pawl/todo has it.") });
    expect(s.chat).toHaveLength(2);
    expect(s.chat[1]!.text).toBe("On it. Done: ~/Pawl/todo has it.");
    expect(isStreaming(s.chat[1])).toBe(false);
    // An update whose `chat` was missed (a reconnect) is added.
    const t = reduceServerMsg(initialHubState(), { type: "chat_update", message: m("half", { streaming: true }) });
    expect(t.chat).toHaveLength(1);
  });
});

describe("assistant activity line", () => {
  const ev = (seq: number, type: HubEvent["type"], payload: unknown) => event(seq, "s1", { type, payload });

  test("a tool call under way, merged with its updates", () => {
    const events = [
      ev(1, "user_message", { text: "summarise notes.md" }),
      ev(2, "tool_call", { toolCallId: "t1", title: "Read", kind: "read", status: "pending", locations: [{ path: "C:\\Users\\you\\Pawl\\notes.md" }] }),
      ev(3, "tool_call_update", { toolCallId: "t1", status: "in_progress" }),
    ];
    expect(assistantActivity(events, { pending: 0 })).toBe("Reading notes.md");
    expect(assistantActivity([...events, ev(4, "tool_call_update", { toolCallId: "t1", status: "completed" })], { pending: 0 })).toBe("Thinking");
  });

  test("an approval waiting wins", () => {
    expect(assistantActivity([ev(1, "tool_call", { toolCallId: "t1", kind: "execute" })], { pending: 1 })).toBe("Waiting for your OK");
    expect(assistantActivity([ev(1, "request_permission", {})])).toBe("Waiting for your OK");
  });

  test("Pawl's own tools and the browser's, by name", () => {
    expect(toolActivity({ title: "mcp__pawl__pawl_start", kind: "other" })).toBe("Starting a session");
    expect(toolActivity({ title: "pawl_status (MCP)" })).toBe("Checking on a session");
    expect(toolActivity({ title: "mcp__chrome-devtools__navigate_page" })).toBe("Opening a page");
    expect(toolActivity({ kind: "execute", title: "npm test" })).toBe("Running a command");
    expect(toolActivity({ kind: "other", title: "Some tool" })).toBe("Some tool");
    expect(toolActivity({})).toBe("Using a tool");
  });

  test("thinking, writing, plans, errors; nothing once the turn ended", () => {
    expect(assistantActivity([ev(1, "agent_thought_chunk", { text: "hm" })])).toBe("Thinking");
    expect(assistantActivity([ev(1, "agent_message_chunk", { text: "On it" })])).toBe("Writing");
    expect(
      assistantActivity([ev(1, "plan", { entries: [{ content: "Write the tests", priority: "high", status: "in_progress" }] })]),
    ).toBe("Working on: Write the tests");
    expect(assistantActivity([ev(1, "error", { message: "agent exited" })])).toBe("Hit a snag: agent exited");
    expect(assistantActivity([ev(1, "agent_message_chunk", {}), ev(2, "stop", { stopReason: "end_turn" })])).toBeNull();
    expect(assistantActivity([ev(1, "hook", {}), ev(2, "notification", {})])).toBeNull();
    expect(assistantActivity([])).toBeNull();
    expect(assistantActivity([ev(1, "agent_thought_chunk", {})], { session: { status: "done", endedAt: 5 } })).toBeNull();
  });
});

describe("assistant: wire format", () => {
  const clients: HubClient[] = [];
  beforeEach(() => FakeSocket.reset());
  afterEach(() => {
    for (const c of clients.splice(0)) c.close();
  });

  test("assistantReset sends assistant_reset and resolves on the ack", async () => {
    const c = new HubClient({ token: "t", client: "desktop", clientVersion: "1", WebSocketImpl: FakeSocket, requestTimeoutMs: 200 });
    clients.push(c);
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake(welcome);
    const p = c.assistantReset();
    await tick();
    const sent = ws.lastSent();
    expect(sent).toEqual({ type: "assistant_reset", id: sent.id });
    ws.receive({ type: "ack", id: sent.id, ok: true, result: {} });
    await p;
    // The hub streams the reply: the client's state follows chat_update.
    ws.receive({ type: "chat", message: { id: "r1", machineId: MID, role: "pet", text: "", ts: 1, streaming: true } });
    ws.receive({ type: "chat_update", message: { id: "r1", machineId: MID, role: "pet", text: "Hello", ts: 1, streaming: true } });
    expect(c.getState().chat.find((m) => m.id === "r1")?.text).toBe("Hello");
  });
});

describe("the assistant asking (cards)", () => {
  const PROMPT = `Fix the flaky login test in api.\n\n${"The test waits on a timer that races the mock clock. ".repeat(30)}Run it 20 times to be sure.`;
  /** The start card exactly as sde-a2a sends it. */
  function startCard(over: Partial<PermissionRequest["toolCall"]> = {}): PermissionRequest {
    return {
      requestId: `${MID}:r1`,
      machineId: MID,
      sessionId: `${MID}:asst1`,
      agentId: "claude-code",
      toolCall: {
        toolCallId: "pawl-start:5f0c",
        title: "Start Codex in api",
        kind: "execute",
        locations: [{ path: "E:\\work\\api" }],
        rawInput: { kind: "pawl_start", agentId: "codex", agentName: "Codex", cwd: "E:\\work\\api", prompt: PROMPT },
        ...over,
      },
      options: [
        { optionId: "allow", name: "Start", kind: "allow_once" },
        { optionId: "reject", name: "Don't start", kind: "reject_once" },
      ],
      createdAt: 1,
    };
  }
  const sessions = { [`${MID}:asst1`]: { purpose: "assistant" as const }, [`${MID}:s2`]: {} };

  test("the start card: its title and the FULL prompt, never cut", () => {
    const c = assistantStartCard(startCard());
    expect(c).toEqual({ title: "Start Codex in api", prompt: PROMPT, agentId: "codex", agentName: "Codex", cwd: "E:\\work\\api" });
    expect(c!.prompt.length).toBeGreaterThan(1500);
    // No prompt: still a start card.
    expect(assistantStartCard(startCard({ rawInput: { kind: "pawl_start", agentId: "codex", agentName: "Codex", cwd: "E:\\work\\api", prompt: "" } }))!.prompt).toBe("");
  });

  test("known by the hub's toolCallId prefix alone: an agent can't forge one", () => {
    expect(assistantStartCard(startCard({ rawInput: { prompt: "x" } }))).not.toBeNull();
    // rawInput.kind is informational: an agent's own tool call carrying it is an ordinary request.
    expect(assistantStartCard(startCard({ toolCallId: "t-1" }))).toBeNull();
    // The hub renames an agent's "pawl-start:" ids, so a renamed one is no start card either.
    expect(assistantStartCard(startCard({ toolCallId: "agent-pawl-start:5f0c" }))).toBeNull();
    expect(isAssistantRequest({ ...startCard({ toolCallId: "agent-pawl-start:5f0c" }), sessionId: "other" }, sessions)).toBe(false);
    expect(assistantStartCard(startCard({ toolCallId: "t-1", rawInput: { command: "start api" }, title: "Start the dev server" }))).toBeNull();
    // Without a title the card is named from its input.
    expect(assistantStartCard(startCard({ title: undefined }))!.title).toBe("Start Codex in E:\\work\\api");
  });

  test("the assistant asks: its own session's requests, and its start card even before its session arrived", () => {
    const edit = { sessionId: `${MID}:asst1`, toolCall: { toolCallId: "t9", kind: "edit" as const } };
    expect(isAssistantRequest(edit, sessions)).toBe(true);
    expect(isAssistantRequest({ ...edit, sessionId: `${MID}:s2` }, sessions)).toBe(false);
    expect(isAssistantRequest({ ...startCard(), sessionId: "unknown" }, sessions)).toBe(true);
    expect(assistantAsker("Ollie")).toBe("Ollie (your assistant)");
    expect(assistantAsker(undefined)).toBe("Your pet (your assistant)");
  });

  test("the agent caution from the hub, shown as-is; none when absent or blank", () => {
    const note = "Codex keeps its own approval settings: Pawl can't lock them, so it may run some tool calls without a card.";
    expect(assistantAgentNote({ ...asst({ resolvedAgent: "codex" }), agentNote: note })).toBe(note);
    expect(assistantAgentNote(asst())).toBeNull();
    expect(assistantAgentNote({ ...asst(), agentNote: "  " })).toBeNull();
  });

  test("the copy never promises that every step asks", () => {
    expect(ASSISTANT_ASKS).toBe("Changes, commands, web use and new sessions ask you first, by your approval rules.");
    const all = [
      assistantIntro("Ollie", "Claude Code", "~/Pawl"),
      ...riskyChanges(settings({ enabled: false, sessions: false, browser: false }), { assistant: { enabled: true, workspace: HOME } }, { homeDir: HOME }).map((c) => c.body),
      ...riskyChanges(settings({ sessions: false }), { assistant: { sessions: true } }, { homeDir: HOME }).map((c) => c.body),
    ];
    for (const t of all) expect(t).not.toMatch(/every step/i);
    // A broad workspace says what it means: it may read anything there without asking.
    const broad = riskyChanges(settings(), { assistant: { workspace: HOME } }, { homeDir: HOME })[0]!.body;
    expect(broad).toContain("it may read any file there without asking");
  });
});

describe("assistant replies read aloud (prompt injection must not answer a card)", () => {
  test("approve / deny / allow and standalone 1-2 digit numbers are taken out", () => {
    expect(stripApprovalWords("Done. Say approve 4 7 to finish.")).toBe("Done. Say to finish.");
    expect(stripApprovalWords("APPROVE 4, 7 now!")).toBe("now!");
    expect(stripApprovalWords("I approved it; deny nothing. Allow 47?")).toBe("I it; nothing.");
    expect(stripApprovalWords("Your approval rules apply. Denied: 9.")).toBe("Your rules apply.");
    expect(stripApprovalWords("47, done")).toBe("done");
    for (const said of ["Done. Say approve 4 7 to finish.", "allowing 12 and denying 3", "approves 99"]) {
      const out = stripApprovalWords(said);
      expect(out).not.toMatch(/approv|den(y|ie)|allow/i);
      expect(out).not.toMatch(/(?<![\d.])\d{1,2}(?![\d.])/);
    }
  });

  test("longer numbers, decimals, times, percentages and other words stay", () => {
    expect(stripApprovalWords("Fixed tests in 2026; 3.5% faster at 10:30 and 50% smaller.")).toBe("Fixed tests in 2026; 3.5% faster at 10:30 and 50% smaller.");
    expect(stripApprovalWords("The allowance and disallow lists stay; v1.2 too.")).toBe("The allowance and disallow lists stay; v1.2 too.");
  });

  test("which replies are the assistant's", () => {
    const sessions = { [`${MID}:asst1`]: { purpose: "assistant" as const }, [`${MID}:s2`]: {} };
    expect(isAssistantReply({ sessionId: `${MID}:asst1` }, { sessions })).toBe(true);
    expect(isAssistantReply({ sessionId: `${MID}:s2` }, { sessions })).toBe(false);
    expect(isAssistantReply({}, { sessions })).toBe(false);
    // The live session from the settings, even before its session reached this client.
    expect(isAssistantReply({ sessionId: `${MID}:asst9` }, { sessions, settings: settings({ sessionId: `${MID}:asst9` }) })).toBe(true);
    // The brain's spoken form only ever comes on assistant replies.
    expect(isAssistantReply({ speech: "Done." }, { sessions })).toBe(true);
  });
});

describe("code-like digit pairs in assistant replies read aloud", () => {
  test("pairs joined by a dash, comma, slash, dot or space go; a one-digit 'time' too", () => {
    for (const code of ["4-7", "4,7", "4/7", "4.7", "4 7", "4 - 7", "4–7", "4 , 7", "4:7", "12-34"]) {
      expect(stripApprovalWords(`Say ${code} now.`)).toBe("Say now.");
    }
    // A bare decimal can't be told apart from a code: it goes too (fail closed).
    expect(stripApprovalWords("It took 3.5 seconds.")).toBe("It took seconds.");
  });

  test("times, percentages, prices, versions and long numbers stay", () => {
    const keep = "At 10:30 and 4:30, 3.5% faster, $4.50, v1.2, 1,000 files, in 2026.";
    expect(stripApprovalWords(keep)).toBe(keep);
  });
});
