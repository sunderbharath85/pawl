import { describe, expect, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { initialHubState, type HubClient, type HubState } from "@pawl/hub-client";
import type { ChatMessage, HubEvent, HubSettings, HubSettingsAssistant, PermissionRequest, Session } from "@pawl/protocol";
import { DefaultBubble, type BubbleEnv } from "./Bubble";

const MID = "winpc0000001";
const SID = `${MID}:asst1`;

function settings(a: Partial<HubSettingsAssistant> | null): HubSettings {
  return {
    bot: { petName: "Ollie" },
    ...(a ? { assistant: { enabled: true, agent: "auto", workspace: "C:\\Users\\you\\Pawl", sessions: true, browser: true, resolvedAgent: "claude-code", state: "working", sessionId: SID, ...a } } : {}),
  } as unknown as HubSettings;
}

const session: Session = { id: SID, machineId: MID, agentId: "claude-code", cwd: "C:\\Users\\you\\Pawl", source: "owned", status: "working", startedAt: 1, lastEventAt: 5, purpose: "assistant" };

function ev(seq: number, type: HubEvent["type"], payload: unknown): HubEvent {
  return { id: `${MID}:${seq}`, seq, machineId: MID, sessionId: SID, type, payload, ts: seq };
}

function render(opts: { assistant: Partial<HubSettingsAssistant> | null; chat: ChatMessage[]; events?: HubEvent[]; pending?: PermissionRequest[] }): string {
  const state: HubState = {
    ...initialHubState(),
    status: "connected",
    machine: { id: MID, name: "PC", os: "windows", lastSeen: 1, online: true },
    agents: [{ id: "claude-code", machineId: MID, name: "Claude Code", command: ["x"], installed: true, source: "registry" }],
    sessions: { [SID]: session },
    events: { [SID]: opts.events ?? [] },
    chat: opts.chat,
    settings: settings(opts.assistant),
    pending: Object.fromEntries((opts.pending ?? []).map((r) => [r.requestId, r])),
  };
  const env: BubbleEnv = {
    client: {} as HubClient,
    state,
    now: 100_000,
    connected: true,
    say: () => {},
    dismiss: () => {},
    chatSent: () => {},
    promptSent: () => {},
  };
  return renderToStaticMarkup(
    createElement(DefaultBubble, {
      env,
      place: { left: 0, anchorY: 0, vertical: "above", tailX: 20, maxHeight: 600 },
      windowHeight: 700,
      name: "Ollie",
      main: { mood: "working", activity: "working", pending: [], waiting: [], busy: [], line: "Working on it." },
      pending: opts.pending ?? [],
      setupDone: true,
      focusInput: false,
      note: null,
      switcher: { chips: [], more: [] },
      focus: null,
      onFocus: () => {},
      onClose: () => {},
    }),
  );
}

const user: ChatMessage = { id: "u1", machineId: MID, role: "user", text: "make me a todo app", ts: 90_000 };
const reply = (text: string, streaming: boolean): ChatMessage => ({ id: "r1", machineId: MID, role: "pet", text, ts: 90_500, sessionId: SID, streaming });

describe("the bubble in Assistant mode", () => {
  test("a streaming reply shows its text so far with a typing indicator, and no time yet", () => {
    const html = render({ assistant: {}, chat: [user, reply("On it: making ~/Pawl/todo", true)] });
    expect(html).toContain("is-streaming");
    expect(html).toContain('aria-busy="true"');
    expect(html).toContain("On it: making ~/Pawl/todo");
    expect(html).toContain('aria-label="Ollie is still writing"');
    expect(html).not.toContain('class="pb-time"');
  });

  test("a finished reply: no indicator, its time shows", () => {
    const html = render({ assistant: { state: "idle" }, chat: [user, reply("Done: ~/Pawl/todo/index.html", false)] });
    expect(html).not.toContain("is-streaming");
    expect(html).not.toContain("still writing");
    expect(html).toContain('class="pb-time"');
  });

  test("the activity line from the session's latest event, and the line under the text box", () => {
    const events = [ev(1, "tool_call", { toolCallId: "t1", kind: "read", status: "in_progress", locations: [{ path: "C:\\Users\\you\\Pawl\\notes.md" }] })];
    const html = render({ assistant: {}, chat: [user, reply("", true)], events });
    expect(html).toContain('class="pb-activity"');
    expect(html).toContain("Reading notes.md");
    // Outside the app the home folder is unknown: the full path.
    expect(html).toContain("Working with Claude Code in C:\\Users\\you\\Pawl");
    expect(html).toContain(">New chat<");
  });

  test("assistant off, or a hub without it: the bubble as before", () => {
    for (const a of [{ enabled: false, state: "off" as const }, null]) {
      const html = render({ assistant: a, chat: [user, { ...reply("Rex is running the tests.", false), streaming: undefined }] });
      expect(html).not.toContain("pb-assistant");
      expect(html).not.toContain("New chat");
      expect(html).not.toContain("pb-activity");
    }
  });
});

describe("the assistant asking in the bubble", () => {
  const PROMPT = `Fix the flaky login test in api.\n\n${"The test waits on a timer that races the mock clock. ".repeat(30)}Run it 20 times.`;
  const start: PermissionRequest = {
    requestId: `${MID}:r1`,
    machineId: MID,
    sessionId: SID,
    agentId: "claude-code",
    toolCall: {
      toolCallId: "pawl-start:5f0c",
      title: "Start Codex in api",
      kind: "execute",
      locations: [{ path: "E:\\work\\api" }],
      rawInput: { kind: "pawl_start", agentId: "codex", agentName: "Codex", cwd: "E:\\work\\api", prompt: PROMPT },
    },
    options: [
      { optionId: "allow", name: "Start", kind: "allow_once" },
      { optionId: "reject", name: "Don't start", kind: "reject_once" },
    ],
    createdAt: 1,
  };

  test("a start card: Ollie asks, the whole prompt (scrollable, not cut), Start / Don't start", () => {
    const html = render({ assistant: {}, chat: [], pending: [start] });
    expect(html).toContain("Ollie (your assistant) asks:");
    expect(html).toContain("Start Codex in api?");
    expect(html).toContain('class="pb-prompt"');
    expect(html).toContain("Run it 20 times.");
    // The whole prompt is in the card, not cut.
    expect(html.includes(PROMPT.split("\n\n")[1]!)).toBe(true);
    expect(html).toContain(">Start<");
    expect(html).toContain(">Don&#x27;t start<");
    expect(html).not.toContain('class="pb-command"');
  });

  test("any other request from the assistant's session: the pet asks, as an ordinary card", () => {
    const edit: PermissionRequest = { ...start, requestId: `${MID}:r2`, toolCall: { toolCallId: "t2", title: "Edit index.html", kind: "edit" }, options: [{ optionId: "a", name: "Allow", kind: "allow_once" }] };
    const html = render({ assistant: {}, chat: [], pending: [edit] });
    expect(html).toContain("Ollie (your assistant) asks:");
    expect(html).toContain("Edit index.html?");
    expect(html).toContain(">Approve<");
  });
});
