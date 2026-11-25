import { describe, expect, test } from "bun:test";
import type { ChatProposal, HubSettings } from "@pawl/protocol";
import {
  agentPictureDestination,
  base64Bytes,
  isThisPcHost,
  ollamaHost,
  browserPatch,
  browserRiskyChanges,
  imageAttachProblem,
  isBrowserSession,
  isLookRequest,
  LOOK_DEFAULT_QUESTION,
  lookQuestion,
  parseAllowSite,
  pictureDestination,
  proposalConfirmText,
  proposalDoneText,
  proposalLabel,
  proposalProblem,
  riskyChanges,
} from "../src/index";

const agents = [
  { id: "claude-code", name: "Claude Code", installed: true, machineId: "m", capabilities: { loadSession: true, listSessions: false, resumeSession: false, promptImage: true, promptAudio: false } },
  { id: "codex", name: "Codex", installed: true, machineId: "m", capabilities: { loadSession: false, listSessions: false, resumeSession: false, promptImage: false, promptAudio: false } },
  { id: "copilot", name: "Copilot", installed: true, machineId: "m" },
];

describe("where a screen capture goes", () => {
  test("the bot's provider right now", () => {
    expect(pictureDestination({ provider: "auto" }, agents, "m")).toEqual({
      text: "to Claude Code (Anthropic's cloud, through your plan)",
      canSee: true,
      leavesPc: true,
      copy: "Pawl keeps no copy; the agent may keep it in its own history.",
    });
    expect(pictureDestination({ provider: "openai", openaiApiKeySet: true }, agents, "m").text).toBe("to OpenAI with your key");
    expect(pictureDestination({ provider: "auto", anthropicApiKeySet: true }, agents, "m").text).toBe("to Anthropic with your key");
    expect(pictureDestination({ provider: "gemini", geminiApiKeySet: true }, agents, "m").text).toBe("to Google Gemini with your key");
    expect(pictureDestination({ provider: "ollama", ollamaUrl: "http://127.0.0.1:11434", ollamaModel: "llava" }, agents, "m")).toEqual({
      text: "to Ollama on this PC (llava, if it can see pictures)",
      canSee: true,
      leavesPc: false,
      copy: "Pawl keeps no copy.",
    });
    expect(pictureDestination({ provider: "ollama", ollamaUrl: "http://gpu-box:11434", ollamaModel: "llava" }, agents, "m").leavesPc).toBe(true);
    expect(pictureDestination({ provider: "openai", openaiApiKeySet: true }, agents, "m").copy).toBe("Pawl keeps no copy; the service may keep it under its own terms.");
    expect(pictureDestination({ provider: "auto" }, [], "m")).toEqual({ text: "nowhere: your model can't see pictures", canSee: false, leavesPc: false, copy: "Pawl keeps no copy." });
    expect(pictureDestination({ provider: "openai", openaiApiKeySet: false }, agents, "m").canSee).toBe(false);
  });

  test("Ollama URLs are read like the hub reads them: no scheme means http://, and only loopback is this PC", () => {
    const at = (url: string) => pictureDestination({ provider: "ollama", ollamaUrl: url, ollamaModel: "llava" }, agents, "m");
    for (const url of ["192.168.1.5:11434", "gpu-box:11434", "http://10.0.0.2:11434", "https://ollama.example.com"]) {
      expect(at(url).leavesPc).toBe(true);
      expect(at(url).text).not.toContain("on this PC");
    }
    expect(at("192.168.1.5:11434").text).toBe("to Ollama at 192.168.1.5 (llava, if it can see pictures)");
    expect(at("gpu-box:11434").text).toBe("to Ollama at gpu-box (llava, if it can see pictures)");
    for (const url of ["localhost:11434", "127.0.0.1:11434", "http://127.5.6.7:11434", "http://[::1]:11434"]) expect(at(url).leavesPc).toBe(false);
    expect(ollamaHost("gpu-box:11434")).toBe("gpu-box");
    expect(ollamaHost("")).toBeNull();
    expect(isThisPcHost("127.0.0.1")).toBe(true);
    expect(isThisPcHost("[::1]")).toBe(true);
    expect(isThisPcHost("128.0.0.1")).toBe(false);
    expect(isThisPcHost(null)).toBe(false);
  });

  test("a picture given to a session's agent: its maker's cloud for the known agents, else it may leave this PC", () => {
    expect(agentPictureDestination("claude-code", "Claude Code")).toEqual({
      text: "to Claude Code (Anthropic's cloud, through your plan)",
      canSee: true,
      leavesPc: true,
      copy: "Pawl keeps no copy; the agent may keep it in its own history.",
    });
    expect(agentPictureDestination("codex", "Codex").text).toBe("to Codex (OpenAI's cloud, through your plan)");
    expect(agentPictureDestination("gemini", "Gemini CLI").text).toBe("to Gemini CLI (Google's cloud, through your plan)");
    expect(agentPictureDestination("copilot", "Copilot").text).toBe("to Copilot (GitHub's cloud, through your plan)");
    expect(agentPictureDestination("my-agent", "My Agent")).toMatchObject({ text: "to My Agent (may leave this PC)", leavesPc: true });
  });

  test("a session takes a picture only when it is this machine's, Pawl's, open, and its agent takes pictures", () => {
    const s = { machineId: "m", agentId: "claude-code", source: "owned" as const, endedAt: undefined };
    expect(imageAttachProblem(s, agents, true)).toBeNull();
    expect(imageAttachProblem(s, agents, false)).toBe("Pictures never go to another machine.");
    expect(imageAttachProblem({ ...s, source: "cli_hook" }, agents, true)).toBe("Only sessions Pawl started take pictures.");
    expect(imageAttachProblem({ ...s, endedAt: 5 }, agents, true)).toBe("This session has ended.");
    expect(imageAttachProblem({ ...s, agentId: "codex" }, agents, true)).toBe("Codex can't take pictures.");
    expect(imageAttachProblem({ ...s, agentId: "copilot" }, agents, true)).toBe("Copilot hasn't said yet whether it takes pictures.");
  });

  test("'look at this', typed or said, opens the picker; the question is the default unless there is more", () => {
    for (const t of ["look at this", "Hey Mochi, look at this!", "can you look at my screen please", "Mochi: what's on my screen?"]) {
      expect(isLookRequest(t)).toBe(true);
      expect(lookQuestion(t)).toBe(LOOK_DEFAULT_QUESTION);
    }
    expect(lookQuestion("look at this error, why does the build fail?")).toBe("look at this error, why does the build fail?");
    expect(isLookRequest("run the tests in api")).toBe(false);
    // Only as a request at the start: an ordinary message that mentions it is sent as it is.
    expect(isLookRequest("I'll look at this later")).toBe(false);
    expect(isLookRequest("please take a look at this")).toBe(true);
  });

  test("picture size", () => {
    expect(base64Bytes("aGVsbG8=")).toBe(5);
    expect(base64Bytes("aGk=")).toBe(2);
  });
});

describe("browser tasks", () => {
  test("sites are origins: a bare host means https; a pasted URL keeps its origin; http and other ports are warned about", () => {
    expect(parseAllowSite("github.com")).toEqual({ ok: true, origin: "https://github.com", warning: null });
    expect(parseAllowSite("https://GitHub.com/acme/web/pull/42")).toEqual({ ok: true, origin: "https://github.com", warning: null });
    expect(parseAllowSite(" docs.rs/serde ")).toEqual({ ok: true, origin: "https://docs.rs", warning: null });
    expect(parseAllowSite("https://github.com:443/x")).toEqual({ ok: true, origin: "https://github.com", warning: null });
    const http = parseAllowSite("http://localhost:3000/app");
    expect(http.ok && http.origin).toBe("http://localhost:3000");
    expect(http.ok && http.warning).toContain("plain http");
    expect(http.ok && http.warning).toContain("port 3000");
    const port = parseAllowSite("staging.acme.dev:8443");
    expect(port).toEqual({ ok: true, origin: "https://staging.acme.dev:8443", warning: "https://staging.acme.dev:8443 uses port 8443: not the usual web port, so make sure this is the site you mean." });
    expect(parseAllowSite("http://intranet.acme.dev:80").ok && (parseAllowSite("http://intranet.acme.dev:80") as { origin: string }).origin).toBe("http://intranet.acme.dev");
    for (const bad of ["", "not a site", "chrome://settings", "*.example.com", "ftp://x.com", "https://me:pw@x.com", "a..b"]) expect(parseAllowSite(bad).ok).toBe(false);
  });

  test("patch only what changed; turning it on and new sites ask first", () => {
    const base = { enabled: false, allowSites: ["https://github.com"] };
    expect(browserPatch(base, { enabled: false })).toEqual({});
    expect(browserPatch(base, { enabled: true })).toEqual({ browser: { enabled: true } });
    expect(browserPatch(base, { allowSites: ["https://github.com", "https://docs.rs"] })).toEqual({ browser: { allowSites: ["https://github.com", "https://docs.rs"] } });
    const settings = {
      browser: base,
      bot: { provider: "auto", understand: false },
      peer: { listen: null, acceptIntents: false, startRoots: [] },
      relay: { url: null, accessKeySet: false },
      customAgents: [],
    } as unknown as HubSettings;
    const on = browserRiskyChanges(settings, { browser: { enabled: true } });
    expect(on).toHaveLength(1);
    expect(on[0]!.body).toContain("Chrome asks you to Allow each connection");
    expect(on[0]!.body).toContain("Every click and every typing asks you here first");
    expect(on[0]!.body).toContain("never types passwords or payment details");
    const sites = riskyChanges(settings, { browser: { allowSites: ["https://github.com", "https://docs.rs"] } });
    expect(sites.map((r) => r.title)).toEqual(["Open https://docs.rs without asking?"]);
    expect(sites[0]!.body).toContain("Clicking, typing, submitting, uploads and dialogs still ask every time.");
    expect(sites[0]!.body).not.toContain("Careful");
    const risky = riskyChanges(settings, { browser: { allowSites: ["https://github.com", "http://localhost:3000"] } });
    expect(risky[0]!.body).toContain("Careful: http://localhost:3000 uses plain http");
    expect(browserRiskyChanges(settings, { browser: { allowSites: [] } })).toEqual([]);
    expect(browserRiskyChanges({ ...settings, browser: { enabled: true, allowSites: [] } } as HubSettings, { browser: { enabled: false } })).toEqual([]);
  });

  test("browser sessions are marked", () => {
    expect(isBrowserSession({ purpose: "browser" })).toBe(true);
    expect(isBrowserSession({})).toBe(false);
  });

  test("a 'browse' proposal: 'Start browser task: <task>', its confirm, and what it needs", () => {
    const p: ChatProposal = { id: "p1", kind: "browse", machineId: "abcdefghijkl", agentId: "", text: "open PR 42 and tell me if CI passed", summary: "x" };
    expect(proposalProblem(p)).toBeNull();
    expect(proposalProblem({ ...p, text: " " })).toBe("it has no task");
    expect(proposalLabel(p)).toBe("Start browser task: open PR 42 and tell me if CI passed");
    const c = proposalConfirmText({ machine: undefined, machines: {} }, p);
    expect(c.button).toBe("Start browser task");
    expect(c.detail).toContain("Chrome asks you to Allow it, and every click or typing asks you here first.");
    expect(proposalDoneText({ machine: undefined, machines: {} }, p)).toBe("Browser task started.");
    // Other kinds are unchanged.
    expect(proposalLabel({ ...p, kind: "start", summary: "Start Codex in api" })).toBe("Start Codex in api");
  });
});
