// The desktop UI end to end: the real panel and the pet window (a production `vite build` of apps/desktop, served
// locally) in Chromium, talking to a real hub, with the scripted agent behind it. One pet on screen: the default pet
// (it orchestrates every session); agents' pets are renamed under Settings > Pets (advanced).
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Server } from "bun";
import { type Browser, type Page, chromium } from "playwright";
import type { A2aCallerSetting, A2aConnectStatus, HubEvent, HubSettings, HubSettingsAssistant, HubSettingsAssistantPatch, HubSettingsPatch, Session } from "@pawl/protocol";
import { REPO, tempRoot, transcript, waitFor } from "../lib/env";
import { DEMO_AGENT_ID, TestHub } from "../lib/hub";

const tmp = tempRoot("ui");
const repo = join(tmp.dir, "repo");
const dist = join(tmp.dir, "desktop-dist");
let hub: TestHub;
let server: Server<undefined>;
let browser: Browser;
let panel: Page;
/** The hub only accepts browser clients from the Tauri app, VS Code webviews and the desktop dev origin. */
const ORIGIN = "http://localhost:1420";

/** The page URL; the token goes in through localStorage (see `openPage`), never the URL or logs. */
function appUrl(page: "panel.html" | "index.html"): string {
  return `${ORIGIN}/${page}?hub=http://127.0.0.1:${hub.port}`;
}

async function openPage(page: "panel.html" | "index.html", viewport: { width: number; height: number }): Promise<Page> {
  const p = await browser.newPage({ viewport });
  await p.addInitScript((token) => localStorage.setItem("pawl.devToken", token), hub.token);
  await p.goto(appUrl(page));
  return p;
}

/** The panel's Settings view (from the sidebar). */
async function openSettings(): Promise<void> {
  await panel.locator(".nav-item", { hasText: "Settings" }).click();
  await panel.locator("#settings-pet").waitFor({ state: "visible", timeout: 15_000 });
}

function answerOf(sessionId: string): Promise<string> {
  return hub.waitState((s) => transcript(s.events[sessionId]).match(/permission answer: (\w+)/)?.[1], `the answer in ${sessionId}`);
}

beforeAll(async () => {
  mkdirSync(repo, { recursive: true });
  writeFileSync(join(repo, "README.md"), "# e2e repo\n");
  const build = Bun.spawn([process.execPath, "x", "vite", "build", "--outDir", dist, "--emptyOutDir", "--logLevel", "warn"], {
    cwd: join(REPO, "apps/desktop"),
    stdout: "pipe",
    stderr: "pipe",
  });
  if ((await build.exited) !== 0) throw new Error(`desktop build failed: ${await new Response(build.stderr).text()}`);
  try {
    server = Bun.serve({
      hostname: "localhost",
      port: 1420,
    async fetch(req) {
      const path = decodeURIComponent(new URL(req.url).pathname);
      const file = Bun.file(join(dist, path === "/" ? "index.html" : path));
        return (await file.exists()) ? new Response(file) : new Response("not found", { status: 404 });
      },
    });
  } catch (e) {
    throw new Error(`port 1420 is busy (a desktop dev server?): the hub only accepts the panel from ${ORIGIN}. ${String(e)}`);
  }
  hub = await TestHub.start(tmp.dir, { name: "E2E Desk" });
  browser = await chromium.launch();
  panel = await openPage("panel.html", { width: 1280, height: 860 });
  const skip = panel.getByRole("button", { name: "Skip setup" });
  if (await skip.isVisible({ timeout: 10_000 }).catch(() => false)) await skip.click();
}, 180_000);

afterAll(async () => {
  await browser?.close();
  server?.stop(true);
  await hub?.stop();
  await tmp.cleanup();
});

describe("desktop UI", () => {
  test("the panel connects to the hub and names this machine", async () => {
    await panel.getByText("E2E Desk").first().waitFor({ state: "visible", timeout: 15_000 });
  });

  test("New session starts the agent, and Allow on the approval card answers it", async () => {
    await panel.getByRole("button", { name: /New session/ }).first().click();
    await panel.locator(".agent-card", { hasText: "Demo Agent" }).click();
    await panel.locator("#new-session-cwd").fill(repo);
    await panel.getByPlaceholder("Run the tests and fix whatever is failing.").fill("run the ui tests");
    await panel.getByRole("button", { name: "Start with Demo Agent" }).click();

    const session = await hub.waitState(
      (s) => Object.values(s.sessions).find((x) => x.agentId === DEMO_AGENT_ID && transcript(s.events[x.id]).includes("run the ui tests")),
      "the session started from the panel",
    );
    await hub.waitState((s) => Object.values(s.pending).some((p) => p.sessionId === session.id), "its permission request");
    await panel.getByRole("button", { name: "Allow", exact: true }).first().click();
    expect(await answerOf(session.id)).toBe("allow");
  }, 60_000);

  test("Settings > Pets (advanced) renames an agent's pet on the hub", async () => {
    await openSettings();
    const row = panel.locator("#settings-pets form.agent-pet", { hasText: "Demo Agent" });
    await row.scrollIntoViewIfNeeded();
    await row.getByLabel("Name of Demo Agent's pet").fill("Pixelpaw");
    await row.getByRole("button", { name: "Save" }).click();
    const pet = await hub.waitState((s) => Object.values(s.pets).find((p) => p.agentId === DEMO_AGENT_ID && p.name === "Pixelpaw"), "the new pet name");
    expect(pet.name).toBe("Pixelpaw");
  }, 30_000);

  test("the default pet's name is the hub's bot.petName: renamed in Settings > Pet on screen", async () => {
    await openSettings();
    const field = panel.locator("#default-pet-name");
    await field.scrollIntoViewIfNeeded();
    await expect(field.inputValue()).resolves.not.toBe("Pawl");
    await field.fill("Biscotti");
    await panel.locator("form", { has: field }).getByRole("button", { name: "Rename" }).click();
    await waitFor(async () => (await hub.client.getSettings()).settings.bot.petName === "Biscotti", "bot.petName saved on the hub", 15_000);
    expect((await hub.client.getSettings()).settings.bot.petName).toBe("Biscotti");
  }, 30_000);

  test("the pet window: one pet; a click opens its bubble; Approve answers the agent; a session adds no pet", async () => {
    const pet = await openPage("index.html", { width: 520, height: 760 });
    const pets = pet.locator(".pet-box");
    await pets.first().waitFor({ state: "visible", timeout: 20_000 });
    expect(await pets.count()).toBe(1);
    // Its own name (the hub's bot.petName, renamed above), once the hub's settings arrive.
    await waitFor(async () => (await pet.locator(".pet-name").first().innerText()) === "Biscotti", "the pet's name tag", 15_000);

    await pets.first().click();
    await pet.locator(".pet-bubble").waitFor({ state: "visible", timeout: 10_000 });

    const session = await hub.client.startSession({ agentId: DEMO_AGENT_ID, cwd: repo, prompt: "run the pet window tests" });
    await hub.waitState((s) => Object.values(s.pending).some((p) => p.sessionId === session.id), "the request");
    const approve = pet.locator(".pb-approval").getByRole("button", { name: "Approve", exact: true }).first();
    await approve.waitFor({ state: "visible", timeout: 20_000 });
    // The running session gets no pet of its own: it shows in the bubble's session switcher instead.
    expect(await pets.count()).toBe(1);
    await pet.locator(".pb-switch-chip", { hasText: "Pixelpaw" }).first().waitFor({ state: "visible", timeout: 10_000 });

    await approve.click();
    expect(await answerOf(session.id)).toBe("allow");
    expect(await pets.count()).toBe(1);
    await pet.close();
  }, 60_000);
});

// ---------------------------------------------------------------- the app's shell, faked for the panel

/**
 * `voice_status` as the real app answers it (0.1.1, captured from the Tauri app on a PC without the models), except
 * that the default models count as downloaded, so turning voice on needs no 50 MB download here.
 */
const REAL_VOICE_STATUS = {
  state: "off",
  config: { enabled: false, wakeWord: true, keyword: "Biscotti", spokenAs: null, sensitivity: 0.5, micId: null, asrModel: "fast", hotkey: null, muted: false, readReplies: false },
  error: null,
  models: { default: { installed: true, bytes: 52_000_000 }, accurate: { installed: false, bytes: 0 } },
  mics: [],
};

/**
 * Runs in the page before the app: a stand-in for the desktop app's IPC (`__TAURI_INTERNALS__`), so the production
 * build takes its Tauri paths (hub credentials from the app, the home folder, the voice engine) in plain Chromium.
 * Voice answers with the real payload shapes; `window.__voiceBroken = true` makes voice_status drop `models`.
 */
function fakeTauriShell(o: { url: string; token: string; voice: unknown }): void {
  const w = window as unknown as Record<string, unknown> & { __voiceBroken?: boolean };
  const callbacks = new Map<number, (data: unknown) => void>();
  const listeners = new Map<string, number[]>();
  let seq = 0;
  let voice = JSON.parse(JSON.stringify(o.voice)) as { state: string; config: { enabled: boolean; muted: boolean; wakeWord: boolean } };
  const emit = (event: string, payload: unknown) => {
    for (const id of listeners.get(event) ?? []) callbacks.get(id)?.({ event, id, payload });
  };
  const commands: Record<string, (a: Record<string, unknown>) => unknown> = {
    hub_credentials: () => ({ url: o.url, wsUrl: `${o.url.replace(/^http/, "ws")}/ws`, token: o.token, tokenSource: "env", dataDir: null }),
    hub_health: () => ({ ok: true, version: null, error: null }),
    "plugin:event|listen": (a) => {
      listeners.set(String(a.event), [...(listeners.get(String(a.event)) ?? []), Number(a.handler)]);
      return a.handler;
    },
    "plugin:event|unlisten": (a) => {
      listeners.set(String(a.event), (listeners.get(String(a.event)) ?? []).filter((h) => h !== a.eventId));
      return null;
    },
    "plugin:path|resolve_directory": () => "C:\\Users\\e2e",
    voice_status: () => (w.__voiceBroken ? { ...voice, models: undefined } : voice),
    // Settings > Assistant's Open folder, and the bubble's Assistant line (opens Settings > Assistant): recorded.
    open_folder: (a) => {
      ((w.__opened ??= []) as unknown[]).push(a.path);
      if (typeof w.__openFolderError === "string") throw w.__openFolderError;
      return null;
    },
    open_panel: (a) => {
      ((w.__panels ??= []) as unknown[]).push(a.focus);
      return null;
    },
    voice_configure: (a) => {
      const config = a.config as typeof voice.config;
      const state = !config.enabled ? "off" : config.muted ? "muted" : config.wakeWord ? "wake_listening" : "idle";
      voice = { ...voice, config, state };
      setTimeout(() => emit("voice", { type: "state", state }), 50);
      return voice;
    },
  };
  w.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: "panel" }, currentWebview: { windowLabel: "panel", label: "panel" } },
    transformCallback: (cb: (d: unknown) => void, once = false) => {
      const id = ++seq;
      callbacks.set(id, (d) => {
        if (once) callbacks.delete(id);
        cb(d);
      });
      return id;
    },
    unregisterCallback: (id: number) => callbacks.delete(id),
    runCallback: (id: number, d: unknown) => callbacks.get(id)?.(d),
    callbacks,
    invoke: async (cmd: string, args?: Record<string, unknown>) => (commands[cmd] ? commands[cmd](args ?? {}) : null),
    convertFileSrc: (path: string) => path,
  };
  w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  localStorage.setItem("pawl.desktop.settings.v1", JSON.stringify({ setupOffered: true, setupDone: true }));
}

/** The panel as the desktop app shows it (through the fake shell), at the app's default size. Page errors are collected. */
async function openAppPanel(errors: string[], voiceBroken = false): Promise<Page> {
  const p = await browser.newPage({ viewport: { width: 1120, height: 740 } });
  p.on("pageerror", (e) => errors.push(e.message));
  if (voiceBroken) await p.addInitScript(() => ((window as unknown as { __voiceBroken: boolean }).__voiceBroken = true));
  await p.addInitScript(fakeTauriShell, { url: `http://127.0.0.1:${hub.port}`, token: hub.token, voice: REAL_VOICE_STATUS });
  await p.goto(`${ORIGIN}/panel.html`);
  await p.locator(".nav-item", { hasText: "Settings" }).click();
  await p.locator("#settings-a2a .a2a-part").first().waitFor({ state: "visible", timeout: 20_000 });
  return p;
}

/**
 * Is the whole panel on screen? The 0.1.1 bug: clicking a toggle far down Settings scrolled the window itself, the app
 * went out of view and the window looked blank (the DOM was all there).
 */
async function panelOnScreen(p: Page): Promise<{ windowScrollY: number; appTop: number; sidebar: boolean }> {
  // Two frames first: the browser fires scroll events (and the panel's pin answers them) in the next frame.
  await p.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  return p.evaluate(() => ({
    windowScrollY: Math.round(window.scrollY),
    appTop: Math.round(document.querySelector(".app")?.getBoundingClientRect().top ?? -1),
    sidebar: !!document.querySelector(".sidebar"),
  }));
}
const ON_SCREEN = { windowScrollY: 0, appTop: 0, sidebar: true };

function freePort(): number {
  const s = Bun.serve({ port: 0, hostname: "127.0.0.1", fetch: () => new Response() });
  const port = s.port!;
  s.stop(true);
  return port;
}

describe("desktop UI: Settings never blanks the window", () => {
  test("turning on the A2A server and voice (far down Settings) keeps the panel on screen", async () => {
    // A port of its own: a real Pawl on this machine may hold the default.
    await hub.client.updateSettings({ a2a: { port: freePort() } });
    const errors: string[] = [];
    const p = await openAppPanel(errors);

    const a2a = p.locator("#settings-a2a");
    await a2a.locator("label.toggle", { hasText: "Let other tools talk to Pawl" }).click();
    await p.locator(".confirm-dialog").getByRole("button", { name: "Turn on A2A server" }).click();
    await waitFor(async () => (await hub.client.getSettings()).settings.a2a?.running === true, "the A2A server running", 15_000);
    await a2a.locator(".a2a-status", { hasText: "Running at" }).waitFor({ timeout: 10_000 });
    expect(await panelOnScreen(p)).toEqual(ON_SCREEN);

    const voice = p.locator("#settings-voice");
    await voice.locator("label.toggle", { hasText: "Talk to" }).click();
    await voice.getByText("Sensitivity").waitFor({ state: "visible", timeout: 10_000 });
    expect(await panelOnScreen(p)).toEqual(ON_SCREEN);

    // Even with a hidden checkbox positioned against the window again (the 0.1.1 CSS), the window snaps back.
    await p.addStyleTag({ content: ".toggle { position: static !important; } .toggle input { inset: auto !important; }" });
    await voice.locator("label.toggle", { hasText: "Wake word" }).click();
    await p.waitForTimeout(300);
    expect(await panelOnScreen(p)).toEqual(ON_SCREEN);

    await a2a.locator("label.toggle", { hasText: "Let other tools talk to Pawl" }).click();
    await waitFor(async () => (await hub.client.getSettings()).settings.a2a?.server === false, "the A2A server off again", 15_000);
    expect(await panelOnScreen(p)).toEqual(ON_SCREEN);
    expect(errors).toEqual([]);
    await p.close();
  }, 60_000);

  test("a section that crashes shows a card with the error and Reload; the rest of the panel keeps working", async () => {
    const errors: string[] = [];
    const p = await openAppPanel(errors, true);
    const card = p.locator("#settings-voice .crash-card");
    await card.waitFor({ state: "visible", timeout: 20_000 });
    expect(await card.innerText()).toContain("Something went wrong in this section.");
    expect(await card.locator(".crash-message").innerText()).toMatch(/default/);
    // Every other section and the sidebar still render.
    expect(await p.locator("#settings-a2a .a2a-status").count()).toBe(1);
    expect(await p.locator(".sidebar").count()).toBe(1);

    // Reload renders the section again (the voice engine answers properly now).
    await p.evaluate(() => ((window as unknown as { __voiceBroken: boolean }).__voiceBroken = false));
    await card.getByRole("button", { name: "Reload" }).click();
    await p.locator("#settings-voice label.toggle", { hasText: "Talk to" }).waitFor({ state: "visible", timeout: 10_000 });
    expect(await p.locator("#settings-voice .crash-card").count()).toBe(0);
    await p.close();
  }, 60_000);
});

// ---------------------------------------------------------------- Settings > A2A > Connect coding agents

/** What a 0.1.1 hub answers to a message it does not know (captured). */
const OLD_HUB_ERROR = "invalid message: unknown variant `a2a_connect_status`, expected one of `hello`, `list_agents`, `list_tasks` at line 1 column 28";

/**
 * The hub's a2a_connect_status / a2a_connect, faked on the panel's socket (everything else goes to the real hub), so
 * the test never writes a real agent config. Connect and Remove change the real hub's A2A settings the way the hub
 * does (server on, the agent's caller added / removed), so the server part and the callers must follow.
 */
function fakeConnectHub(opts: { old?: boolean; rows?: (rows: A2aConnectStatus[]) => A2aConnectStatus[]; serverError?: string } = {}) {
  const base: A2aConnectStatus[] = [
    { target: "claude", name: "Claude Code", agentInstalled: true, state: "missing", configPath: "C:\\Users\\e2e\\.claude.json", callerId: "claude-code", callerExists: false },
    { target: "codex", name: "Codex", agentInstalled: false, state: "missing", configPath: "C:\\Users\\e2e\\.codex\\config.toml", callerId: "codex", callerExists: false },
    { target: "copilot-cli", name: "Copilot CLI", agentInstalled: true, state: "missing", configPath: "C:\\Users\\e2e\\.copilot\\mcp-config.json", callerId: "copilot-cli", callerExists: false },
    {
      target: "copilot-vscode",
      name: "Copilot in VS Code",
      agentInstalled: true,
      state: "missing",
      configPath: "C:\\Users\\e2e\\AppData\\Roaming\\Code\\User\\mcp.json",
      callerId: "copilot-vscode",
      callerExists: false,
    },
  ];
  const rows = opts.rows ? opts.rows(base) : base;
  const seen: string[] = [];
  /** Callers Connect made (id -> target): the hub marks them with `connectTarget` in every settings it sends. */
  const made = new Map<string, A2aConnectStatus["target"]>();
  const mark = (raw: string | Buffer): string | Buffer => {
    if (typeof raw !== "string" || !raw.includes('"callers"')) return raw;
    const msg = JSON.parse(raw) as { settings?: HubSettings; result?: { settings?: HubSettings } };
    const a2a = (msg.settings ?? msg.result?.settings)?.a2a;
    for (const c of a2a?.callers ?? []) {
      const target = made.get(c.id);
      if (target) c.connectTarget = target;
    }
    // The A2A server on but not running (as the hub reports a port taken after it started).
    if (a2a && opts.serverError) Object.assign(a2a, { server: true, running: false, url: null, error: opts.serverError });
    return JSON.stringify(msg);
  };
  const a2a = async () => (await hub.client.getSettings()).settings.a2a!;
  const callers = (list: A2aCallerSetting[]) => list.map((c) => ({ id: c.id, name: c.name, allowApprovals: c.allowApprovals }));

  // Rows that start with a caller Connect made (callerMarked).
  for (const r of rows) if (r.callerMarked) made.set(r.callerId, r.target);
  /** A row as the hub reports it: callerMarked = the caller is one Connect made. */
  const withMark = (r: A2aConnectStatus, callerExists = r.callerExists) => ({ ...r, callerExists, callerMarked: callerExists && made.has(r.callerId) });

  async function answer(msg: { type: string; id: string; target?: string; action?: string }): Promise<object> {
    if (opts.old) return { type: "ack", id: msg.id, ok: false, error: OLD_HUB_ERROR };
    if (msg.type === "a2a_connect_status") return { type: "ack", id: msg.id, ok: true, result: { targets: rows.map((r) => withMark(r)) } };
    const i = rows.findIndex((r) => r.target === msg.target);
    const row = rows[i]!;
    seen.push(`${msg.action}:${msg.target}`);
    const now = await a2a();
    // Like the hub: the caller exists when it is in the settings; Connect always makes a new token.
    const callerExists = now.callers.some((c) => c.id === row.callerId);
    if (msg.action === "preview") {
      const plan = `${now.server ? "" : "turn on the A2A server (127.0.0.1 only)\n"}caller ${row.callerId}: ${callerExists ? "new token" : "new"}\n+ ${row.configPath}: "pawl" entry (token <new token>)`;
      return { type: "ack", id: msg.id, ok: true, result: { plan, applied: false, status: withMark(row, callerExists), startsServer: !now.server, newToken: true } };
    }
    if (msg.action === "apply") {
      made.set(row.callerId, row.target);
      const list = callerExists ? callers(now.callers) : [...callers(now.callers), { id: row.callerId, name: row.name }];
      await hub.client.updateSettings({ a2a: { server: true, callers: list } });
      rows[i] = { ...row, state: "connected", callerExists: true };
      return { type: "ack", id: msg.id, ok: true, result: { plan: "", applied: true, status: withMark(rows[i]!), startsServer: !now.server, newToken: true } };
    }
    // Like the hub: Remove deletes only a caller Connect made; one the user added by hand stays.
    const marked = callerExists && made.has(row.callerId);
    if (marked) await hub.client.updateSettings({ a2a: { callers: callers(now.callers.filter((c) => c.id !== row.callerId)) } });
    made.delete(row.callerId);
    // A config Pawl cannot read: the caller (token) goes, the file stays as it is.
    const warning =
      row.state === "unsupported" && marked ? `Revoked ${row.name}'s token, but could not edit ${row.configPath}: take the "pawl" entry out by hand.` : undefined;
    rows[i] = { ...row, state: row.state === "unsupported" ? "unsupported" : "missing", callerExists: callerExists && !marked };
    return {
      type: "ack",
      id: msg.id,
      ok: true,
      result: { plan: "", applied: marked || row.state !== "missing", status: withMark(rows[i]!), startsServer: false, newToken: false, ...(warning ? { warning } : {}) },
    };
  }

  async function route(p: Page): Promise<void> {
    await p.routeWebSocket(/\/ws$/, (ws) => {
      const server = ws.connectToServer();
      ws.onMessage((raw) => {
        const msg = JSON.parse(String(raw)) as { type: string; id: string };
        if (msg.type === "a2a_connect_status" || msg.type === "a2a_connect") void answer(msg).then((ack) => ws.send(JSON.stringify(ack)));
        else server.send(raw);
      });
      server.onMessage((raw) => ws.send(mark(raw)));
    });
  }
  return { route, seen };
}

/** The panel (vite build, plain browser) on Settings > A2A, with the connect messages faked. */
async function openConnect(fake: ReturnType<typeof fakeConnectHub>): Promise<Page> {
  const p = await browser.newPage({ viewport: { width: 1180, height: 900 } });
  await fake.route(p);
  await p.addInitScript((token) => {
    localStorage.setItem("pawl.devToken", token);
    localStorage.setItem("pawl.desktop.settings.v1", JSON.stringify({ setupOffered: true, setupDone: true }));
  }, hub.token);
  await p.goto(appUrl("panel.html"));
  await p.locator(".nav-item", { hasText: "Settings" }).click();
  await p.locator("#settings-a2a .a2a-connect").waitFor({ state: "visible", timeout: 20_000 });
  return p;
}

describe("desktop UI: Settings > A2A > Connect coding agents", () => {
  test("preview, connect (asks first; turns the server on), the row turns Connected, remove (asks first)", async () => {
    await hub.client.updateSettings({ a2a: { port: freePort(), server: false } });
    const fake = fakeConnectHub();
    const p = await openConnect(fake);
    const part = p.locator("#settings-a2a .a2a-connect");
    const claude = part.locator(".a2a-connect-row", { hasText: "Claude Code" });
    await claude.waitFor({ state: "visible", timeout: 10_000 });
    expect(await part.locator(".a2a-connect-row").count()).toBe(4);
    expect(await claude.locator(".status-pill").textContent()).toBe("Not connected");
    expect(await claude.locator(".hook-path").innerText()).toBe("C:\\Users\\e2e\\.claude.json");
    expect(await part.locator(".a2a-connect-row", { hasText: "Codex" }).locator(".chip", { hasText: "not found" }).count()).toBe(1);

    // Preview: the plan, nothing written.
    await claude.getByRole("button", { name: "Preview" }).click();
    await claude.locator(".plan-preview").waitFor({ state: "visible", timeout: 10_000 });
    expect(await claude.locator(".plan-preview").innerText()).toContain('"pawl" entry');
    expect((await hub.client.getSettings()).settings.a2a?.callers.some((c) => c.id === "claude-code")).toBe(false);

    // Connect asks first: the file, the agent's own token in it, and the server going on.
    await claude.getByRole("button", { name: "Connect", exact: true }).click();
    const dialog = p.locator(".confirm-dialog");
    await dialog.waitFor({ state: "visible", timeout: 10_000 });
    const asked = (await dialog.innerText()).replace(/\s+/g, " ");
    expect(asked).toContain("Connect Claude Code to Pawl?");
    expect(asked).toContain('Pawl adds a "pawl" entry to C:\\Users\\e2e\\.claude.json.');
    expect(asked).toContain("Claude Code gets its own token, stored in that file.");
    expect(asked).toContain("This turns on Pawl's A2A server, on 127.0.0.1 only.");
    await dialog.getByRole("button", { name: "Connect", exact: true }).click();
    await claude.locator(".status-pill", { hasText: "Connected" }).waitFor({ timeout: 10_000 });
    expect(fake.seen).toEqual(["preview:claude", "preview:claude", "apply:claude"]);
    // The server part and the callers follow (fetched again after Connect).
    await p.locator("#settings-a2a .a2a-status", { hasText: "Running at" }).waitFor({ timeout: 15_000 });
    await p.locator("#settings-a2a .a2a-item", { hasText: "claude-code" }).waitFor({ timeout: 10_000 });

    // Remove asks first (danger), then the row is back to Not connected and the caller is gone.
    await claude.getByRole("button", { name: "Remove" }).click();
    await dialog.waitFor({ state: "visible", timeout: 10_000 });
    const removing = (await dialog.innerText()).replace(/\s+/g, " ");
    expect(removing).toContain("Disconnect Claude Code from Pawl?");
    expect(removing).toContain("its token stops working at once");
    await dialog.getByRole("button", { name: "Remove", exact: true }).click();
    await claude.locator(".status-pill", { hasText: "Not connected" }).waitFor({ timeout: 10_000 });
    await waitFor(async () => !(await hub.client.getSettings()).settings.a2a?.callers.some((c) => c.id === "claude-code"), "the caller removed", 10_000);
    expect(await p.locator("#settings-a2a .a2a-item", { hasText: "claude-code" }).count()).toBe(0);
    expect(fake.seen.at(-1)).toBe("remove:claude");

    await hub.client.updateSettings({ a2a: { server: false } });
    await p.close();
  }, 60_000);

  test("a caller Connect made: a hint instead of the approvals switch; its Remove disconnects the agent too", async () => {
    await hub.client.updateSettings({ a2a: { port: freePort(), server: false, callers: [{ id: "zed", name: "Zed" }] } });
    const fake = fakeConnectHub();
    const p = await openConnect(fake);
    const row = p.locator("#settings-a2a .a2a-connect-row", { hasText: "Copilot CLI" });
    await row.getByRole("button", { name: "Connect", exact: true }).click();
    const dialog = p.locator(".confirm-dialog");
    await dialog.getByRole("button", { name: "Connect", exact: true }).click();
    await row.locator(".status-pill", { hasText: "Connected" }).waitFor({ timeout: 10_000 });

    const caller = p.locator("#settings-a2a .a2a-item", { hasText: "copilot-cli" });
    await caller.locator(".a2a-connect-note").waitFor({ timeout: 10_000 });
    expect(await caller.locator(".a2a-connect-note").innerText()).toBe("Connected by Pawl for Copilot CLI. It can never answer approvals.");
    expect(await caller.locator("label.toggle", { hasText: "Can answer approvals" }).count()).toBe(0);
    // Its token lives in the agent's config: renewed by Connect's Update, never made here.
    expect(await caller.getByRole("button", { name: /token/ }).count()).toBe(0);
    // A caller added by hand keeps its switch and its token button.
    const zed = p.locator("#settings-a2a .a2a-item", { hasText: "Zed" });
    expect(await zed.locator("label.toggle", { hasText: "Can answer approvals" }).count()).toBe(1);
    expect(await zed.getByRole("button", { name: /token/ }).count()).toBe(1);

    // Remove in Callers = Connect's Remove: the same danger confirm, then the entry and the caller go.
    await caller.getByRole("button", { name: "Remove" }).click();
    await dialog.waitFor({ state: "visible", timeout: 10_000 });
    const asked = (await dialog.innerText()).replace(/\s+/g, " ");
    expect(asked).toContain("Disconnect Copilot CLI from Pawl?");
    expect(asked).toContain('takes its "pawl" entry out of C:\\Users\\e2e\\.copilot\\mcp-config.json');
    await dialog.getByRole("button", { name: "Remove", exact: true }).click();
    await row.locator(".status-pill", { hasText: "Not connected" }).waitFor({ timeout: 10_000 });
    expect(fake.seen).toEqual(["preview:copilot-cli", "apply:copilot-cli", "remove:copilot-cli"]);
    await waitFor(async () => !(await hub.client.getSettings()).settings.a2a?.callers.some((c) => c.id === "copilot-cli"), "the caller removed", 10_000);
    expect(await p.locator("#settings-a2a .a2a-item", { hasText: "copilot-cli" }).count()).toBe(0);
    expect(await zed.count()).toBe(1);

    await hub.client.updateSettings({ a2a: { server: false, callers: [] } });
    await p.close();
  }, 60_000);

  test("Connect takes over a caller you added (said so in the confirm, with the new token)", async () => {
    await hub.client.updateSettings({ a2a: { port: freePort(), server: true, callers: [{ id: "codex", name: "My Codex" }] } });
    const p = await openConnect(fakeConnectHub());
    const row = p.locator("#settings-a2a .a2a-connect-row", { hasText: "Codex" });
    await row.getByRole("button", { name: "Connect", exact: true }).click();
    const dialog = p.locator(".confirm-dialog");
    await dialog.waitFor({ state: "visible", timeout: 10_000 });
    const asked = (await dialog.innerText()).replace(/\s+/g, " ");
    expect(asked).toContain('It takes over the caller "codex" you added: Codex uses it from now on, with a new token stored in that file; its old token stops working.');
    // The server is already on: no word about turning it on.
    expect(asked).not.toContain("This turns on Pawl's A2A server");
    await dialog.getByRole("button", { name: "Cancel" }).click();
    await hub.client.updateSettings({ a2a: { server: false, callers: [] } });
    await p.close();
  }, 30_000);

  test("Copilot in VS Code: the confirm mentions Settings Sync", async () => {
    const p = await openConnect(fakeConnectHub());
    const row = p.locator("#settings-a2a .a2a-connect-row", { hasText: "Copilot in VS Code" });
    await row.getByRole("button", { name: "Connect", exact: true }).click();
    const dialog = p.locator(".confirm-dialog");
    await dialog.waitFor({ state: "visible", timeout: 10_000 });
    expect((await dialog.innerText()).replace(/\s+/g, " ")).toContain("If VS Code's Settings Sync is on, it can copy mcp.json, token included, to your other computers.");
    await dialog.getByRole("button", { name: "Cancel" }).click();
    await p.close();
  }, 30_000);

  test("an unsupported config can still be removed: the token is revoked, the file warning shows as a warning", async () => {
    await hub.client.updateSettings({ a2a: { callers: [{ id: "copilot-vscode", name: "Copilot in VS Code" }] } });
    const fake = fakeConnectHub({
      rows: (rows) =>
        rows.map((r) =>
          r.target === "copilot-vscode"
            ? { ...r, state: "unsupported", callerExists: true, callerMarked: true, detail: "mcp.json is not valid JSON; fix it or add the entry by hand" }
            : r,
        ),
    });
    const p = await openConnect(fake);
    const row = p.locator("#settings-a2a .a2a-connect-row", { hasText: "Copilot in VS Code" });
    await row.locator(".status-pill", { hasText: "Not supported" }).waitFor({ timeout: 10_000 });
    expect(await row.getByRole("button", { name: "Preview" }).count()).toBe(0);
    await row.getByRole("button", { name: "Remove" }).click();
    const dialog = p.locator(".confirm-dialog");
    await dialog.waitFor({ state: "visible", timeout: 10_000 });
    const asked = (await dialog.innerText()).replace(/\s+/g, " ");
    // Pawl cannot read the file: it tries, and may not manage; the token goes either way.
    expect(asked).toContain('Pawl tries to take its "pawl" entry out of');
    expect(asked).toContain("but it may not be able to: it cannot read that file.");
    expect(asked).toContain("Either way it removes Copilot in VS Code's caller: its token stops working at once");
    await dialog.getByRole("button", { name: "Remove", exact: true }).click();
    const warning = row.locator(".a2a-connect-warning");
    await warning.waitFor({ state: "visible", timeout: 10_000 });
    expect(await warning.innerText()).toContain("take the \"pawl\" entry out by hand");
    // A warning, not an error; and the toast says what did happen.
    expect(await row.locator(".field-error").count()).toBe(0);
    await p.locator(".toast", { hasText: "Token revoked" }).waitFor({ timeout: 5000 });
    await waitFor(async () => !(await hub.client.getSettings()).settings.a2a?.callers.some((c) => c.id === "copilot-vscode"), "the caller revoked", 10_000);
    await p.close();
  }, 30_000);

  test("Remove of an entry whose caller is already gone: no word about a token", async () => {
    const fake = fakeConnectHub({ rows: (rows) => rows.map((r) => (r.target === "claude" ? { ...r, state: "outdated", callerExists: false } : r)) });
    const p = await openConnect(fake);
    const row = p.locator("#settings-a2a .a2a-connect-row", { hasText: "Claude Code" });
    await row.getByRole("button", { name: "Remove" }).click();
    const dialog = p.locator(".confirm-dialog");
    await dialog.waitFor({ state: "visible", timeout: 10_000 });
    const asked = (await dialog.innerText()).replace(/\s+/g, " ");
    expect(asked).toContain('Pawl takes its "pawl" entry out of C:\\Users\\e2e\\.claude.json (a backup is made first).');
    expect(asked).not.toContain("token");
    await dialog.getByRole("button", { name: "Cancel" }).click();
    await p.close();
  }, 30_000);

  test("a connected row shows the A2A server's error (its port is taken)", async () => {
    const error = "port 47802 is in use";
    // Claude Code: a row without a detail (a hub from before it named the server there); Codex: the newer hub's detail.
    const detail = `Pawl's A2A server is not running: ${error}`;
    const fake = fakeConnectHub({
      serverError: error,
      rows: (rows) =>
        rows.map((r) =>
          r.target === "claude"
            ? { ...r, state: "connected", callerExists: true }
            : r.target === "codex"
              ? { ...r, state: "connected", callerExists: true, agentInstalled: true, detail }
              : r,
        ),
    });
    const p = await openConnect(fake);
    const claude = p.locator("#settings-a2a .a2a-connect-row", { hasText: "Claude Code" });
    const problem = claude.locator(".a2a-connect-server");
    await problem.waitFor({ state: "visible", timeout: 10_000 });
    expect(await problem.innerText()).toBe("Claude Code cannot reach Pawl: the A2A server is not running (port 47802 is in use).");
    // The hub's detail is shown once, as the row's error (not again as a hint).
    const codex = p.locator("#settings-a2a .a2a-connect-row", { hasText: "Codex" });
    expect(await codex.locator(".a2a-connect-server").innerText()).toBe(detail);
    expect(await codex.getByText(detail).count()).toBe(1);
    // Only the connected rows say so; the server part shows the same error.
    expect(await p.locator("#settings-a2a .a2a-connect-server").count()).toBe(2);
    expect(await p.locator("#settings-a2a .a2a-status").innerText()).toContain(error);
    await p.close();
  }, 30_000);

  test("a caller you added with an agent's id is never offered for removal there; Connect's own caller is", async () => {
    // copilot-cli: added by hand (not marked); codex: made by Connect, its entry since deleted by hand (marked).
    await hub.client.updateSettings({ a2a: { callers: [{ id: "copilot-cli", name: "My script" }, { id: "codex", name: "Codex" }] } });
    const fake = fakeConnectHub({
      rows: (rows) =>
        rows.map((r) =>
          r.target === "copilot-cli" ? { ...r, callerExists: true } : r.target === "codex" ? { ...r, callerExists: true, callerMarked: true, agentInstalled: true } : r,
        ),
    });
    const p = await openConnect(fake);
    const mine = p.locator("#settings-a2a .a2a-connect-row", { hasText: "Copilot CLI" });
    await mine.locator(".status-pill", { hasText: "Not connected" }).waitFor({ timeout: 10_000 });
    expect(await mine.getByRole("button", { name: "Remove" }).count()).toBe(0);
    expect(await mine.getByRole("button", { name: "Connect", exact: true }).count()).toBe(1);
    // In Callers it is an ordinary caller: its approvals switch and its own Remove.
    const caller = p.locator("#settings-a2a .a2a-item", { hasText: "My script" });
    expect(await caller.locator("label.toggle", { hasText: "Can answer approvals" }).count()).toBe(1);

    const codex = p.locator("#settings-a2a .a2a-connect-row", { hasText: "Codex" });
    await codex.getByRole("button", { name: "Remove" }).click();
    const dialog = p.locator(".confirm-dialog");
    await dialog.waitFor({ state: "visible", timeout: 10_000 });
    expect((await dialog.innerText()).replace(/\s+/g, " ")).toContain("removes Codex's caller: its token stops working at once");
    await dialog.getByRole("button", { name: "Remove", exact: true }).click();
    await waitFor(async () => !(await hub.client.getSettings()).settings.a2a?.callers.some((c) => c.id === "codex"), "Connect's caller removed", 10_000);
    // The hand-added caller is untouched.
    expect((await hub.client.getSettings()).settings.a2a?.callers.some((c) => c.id === "copilot-cli")).toBe(true);
    await hub.client.updateSettings({ a2a: { callers: [] } });
    await p.close();
  }, 30_000);

  test("Cancel in the confirm connects nothing", async () => {
    const fake = fakeConnectHub();
    const p = await openConnect(fake);
    const row = p.locator("#settings-a2a .a2a-connect-row", { hasText: "Copilot CLI" });
    await row.getByRole("button", { name: "Connect", exact: true }).click();
    const dialog = p.locator(".confirm-dialog");
    await dialog.getByRole("button", { name: "Cancel" }).click();
    await dialog.waitFor({ state: "detached", timeout: 5000 });
    expect(fake.seen).toEqual(["preview:copilot-cli"]);
    expect(await row.locator(".status-pill").textContent()).toBe("Not connected");
    await p.close();
  }, 30_000);

  test("an older hub: a hint to update Pawl instead of the list", async () => {
    const p = await openConnect(fakeConnectHub({ old: true }));
    const part = p.locator("#settings-a2a .a2a-connect");
    await part.getByText("Update Pawl to connect coding agents").waitFor({ state: "visible", timeout: 10_000 });
    expect(await part.locator(".a2a-connect-row").count()).toBe(0);
    // The rest of the A2A section still works.
    expect(await p.locator("#settings-a2a .a2a-status").count()).toBe(1);
    await p.close();
  }, 30_000);
});

// ---------------------------------------------------------------- Assistant mode

const E2E_HOME = "C:\\Users\\e2e";
const ASSISTANT_DEFAULT = `${E2E_HOME}\\Pawl`;

/**
 * Assistant mode (Settings > Assistant, the streaming bubble), faked on the page's socket: `[assistant]` in the
 * settings, its patches, assistant_reset, and a chat turn that streams (chat, then chat_update) while its session's
 * events say what it does. Everything else goes to the real hub (which may predate Assistant mode).
 */
function fakeAssistantHub(init: Partial<HubSettingsAssistant> = {}) {
  let asst: HubSettingsAssistant = {
    enabled: true,
    agent: "auto",
    workspace: ASSISTANT_DEFAULT,
    sessions: true,
    browser: true,
    resolvedAgent: DEMO_AGENT_ID,
    state: "idle",
    ...init,
  };
  const sockets: Array<{ send(m: string): void }> = [];
  const patches: HubSettingsAssistantPatch[] = [];
  let resets = 0;
  let last: HubSettings | null = null;
  let seq = 900_000;
  let turn: { session: Session; replyId: string } | null = null;
  const mid = () => hub.client.getState().machine!.id;
  const send = (m: object) => sockets.forEach((s) => s.send(JSON.stringify(m)));
  const withAssistant = (raw: string | Buffer): string | Buffer => {
    if (typeof raw !== "string" || !raw.includes('"settings"')) return raw;
    const msg = JSON.parse(raw) as { settings?: HubSettings; result?: { settings?: HubSettings } };
    const s = msg.settings ?? msg.result?.settings;
    if (!s) return raw;
    last = s;
    s.assistant = asst;
    return JSON.stringify(msg);
  };
  const broadcast = () => {
    if (last) send({ type: "settings", settings: { ...last, assistant: asst }, restartRequired: [] });
  };
  const set = (next: Partial<HubSettingsAssistant>) => {
    asst = { ...asst, ...next };
    broadcast();
  };
  /** Start cards the user answered (request id -> option id). */
  const answers = new Map<string, string | null>();
  const event = (sessionId: string, type: HubEvent["type"], payload: unknown): HubEvent => {
    seq += 1;
    return { id: `${mid()}:${seq}`, seq, machineId: mid(), sessionId, type, payload, ts: Date.now() };
  };

  function onClient(raw: string, ws: { send(m: string): void }, server: { send(m: string): void }): void {
    const msg = JSON.parse(raw) as { type: string; id: string; text?: string; patch?: HubSettingsPatch };
    if (msg.type === "update_settings" && msg.patch?.assistant) {
      const { assistant: p, ...rest } = msg.patch;
      patches.push(p);
      if (p.workspace !== undefined && /^c:\\windows/i.test(p.workspace)) {
        ws.send(JSON.stringify({ type: "ack", id: msg.id, ok: false, error: "invalid:assistant.workspace: is a system folder" }));
        return;
      }
      asst = { ...asst, ...p, workspace: p.workspace === "" ? ASSISTANT_DEFAULT : (p.workspace ?? asst.workspace) };
      if (p.enabled === false) asst = { ...asst, state: "off", sessionId: undefined };
      if (p.enabled === true) asst = { ...asst, state: "idle" };
      // The rest of the patch goes to the real hub; an assistant-only patch is answered with its settings.
      server.send(JSON.stringify(Object.keys(rest).length ? { ...msg, patch: rest } : { type: "get_settings", id: msg.id }));
      setTimeout(broadcast, 50);
      return;
    }
    if (msg.type === "resolve_permission" && (msg as { requestId?: string }).requestId?.startsWith(`${mid()}:start-`)) {
      // The assistant's start card (faked here): answered on this card, then gone everywhere.
      const m = msg as unknown as { requestId: string; optionId: string | null };
      answers.set(m.requestId, m.optionId);
      ws.send(JSON.stringify({ type: "ack", id: msg.id, ok: true, result: {} }));
      send({
        type: "permission_resolved",
        requestId: m.requestId,
        sessionId: asst.sessionId ?? "",
        outcome: m.optionId ? { outcome: "selected", optionId: m.optionId } : { outcome: "cancelled" },
        by: "user",
      });
      return;
    }
    if (msg.type === "assistant_reset") {
      resets += 1;
      turn = null;
      ws.send(JSON.stringify({ type: "ack", id: msg.id, ok: true, result: {} }));
      set({ sessionId: undefined, state: asst.enabled ? "idle" : "off" });
      return;
    }
    if (msg.type === "chat" && asst.enabled && msg.text) {
      // A turn starts: the user's message, the assistant session, the reply (streaming), what it is doing.
      const now = Date.now();
      const user = { id: `e2e-u-${now}`, machineId: mid(), role: "user", text: msg.text, ts: now };
      ws.send(JSON.stringify({ type: "ack", id: msg.id, ok: true, result: { message: user } }));
      const session: Session = turn?.session ?? {
        id: `${mid()}:assistant-${now}`,
        machineId: mid(),
        agentId: DEMO_AGENT_ID,
        cwd: asst.workspace,
        source: "owned",
        status: "working",
        startedAt: now,
        lastEventAt: now,
        purpose: "assistant",
      };
      turn = { session, replyId: `e2e-r-${now}` };
      send({ type: "session", session: { ...session, status: "working", lastEventAt: now } });
      set({ state: "working", sessionId: session.id });
      const reply = { id: turn.replyId, machineId: mid(), role: "pet", text: "", ts: now + 1, sessionId: session.id, streaming: true };
      send({ type: "chat", message: reply });
      send({ type: "event", event: event(session.id, "user_message", { text: msg.text }) });
      send({
        type: "event",
        event: event(session.id, "tool_call", { toolCallId: "t1", title: "Read", kind: "read", status: "in_progress", locations: [{ path: `${asst.workspace}\\notes.md` }] }),
      });
      send({ type: "chat_update", message: { ...reply, text: "On it. Reading your notes first" } });
      return;
    }
    server.send(raw);
  }

  /** The assistant asks to start a session: the hub's start card, exactly as sde-a2a sends it. */
  function askToStart(prompt: string): string {
    if (!turn) throw new Error("no assistant session");
    const requestId = `${mid()}:start-${Date.now()}`;
    send({
      type: "permission_request",
      request: {
        requestId,
        machineId: mid(),
        sessionId: turn.session.id,
        agentId: turn.session.agentId,
        toolCall: {
          toolCallId: `pawl-start:${requestId}`,
          title: "Start Codex in api",
          kind: "execute",
          locations: [{ path: "E:\\work\\api" }],
          rawInput: { kind: "pawl_start", agentId: "codex", agentName: "Codex", cwd: "E:\\work\\api", prompt },
        },
        options: [
          { optionId: "allow", name: "Start", kind: "allow_once" },
          { optionId: "reject", name: "Don't start", kind: "reject_once" },
        ],
        createdAt: Date.now(),
      },
    });
    return requestId;
  }

  /** The turn's end: the reply's final text, the tool call done, the session idle again. */
  function finish(text: string): void {
    if (!turn) throw new Error("no turn running");
    const { session, replyId } = turn;
    send({ type: "event", event: event(session.id, "tool_call_update", { toolCallId: "t1", status: "completed" }) });
    send({ type: "event", event: event(session.id, "stop", { stopReason: "end_turn" }) });
    send({ type: "chat_update", message: { id: replyId, machineId: mid(), role: "pet", text, ts: session.startedAt + 1, sessionId: session.id, streaming: false } });
    send({ type: "session", session: { ...session, status: "idle", lastEventAt: Date.now() } });
    set({ state: "idle" });
  }

  async function route(p: Page): Promise<void> {
    await p.routeWebSocket(/\/ws$/, (ws) => {
      const server = ws.connectToServer();
      sockets.push(ws);
      ws.onMessage((raw) => onClient(String(raw), ws, server));
      server.onMessage((raw) => ws.send(withAssistant(raw)));
    });
  }
  return {
    route,
    set,
    finish,
    askToStart,
    answers,
    patches,
    get resets() {
      return resets;
    },
    get assistant() {
      return asst;
    },
  };
}

/** A page of the app as the desktop app shows it (fake shell), with Assistant mode faked on its socket. */
async function openWithAssistant(
  fake: ReturnType<typeof fakeAssistantHub>,
  page: "panel.html" | "index.html",
  errors: string[],
  viewport = { width: 1180, height: 900 },
): Promise<Page> {
  const p = await browser.newPage({ viewport });
  p.on("pageerror", (e) => errors.push(e.message));
  await fake.route(p);
  await p.addInitScript(fakeTauriShell, { url: `http://127.0.0.1:${hub.port}`, token: hub.token, voice: REAL_VOICE_STATUS });
  await p.goto(`${ORIGIN}/${page}`);
  return p;
}

describe("desktop UI: Assistant mode", () => {
  test("Settings > Assistant: status, agent, workspace (save, open, reset), its tools, the browser link, off and on, New chat", async () => {
    await hub.client.updateSettings({ browser: { enabled: false } });
    const pet = (await hub.client.getSettings()).settings.bot.petName;
    const fake = fakeAssistantHub();
    const errors: string[] = [];
    const p = await openWithAssistant(fake, "panel.html", errors);
    await p.locator(".nav-item", { hasText: "Settings" }).click();
    const sec = p.locator("#settings-assistant");
    await sec.waitFor({ state: "visible", timeout: 20_000 });
    // First of the sections, with what it means under the title.
    expect(await p.locator(".settings-nav-item").first().innerText()).toBe("Assistant");
    expect(await sec.locator(".settings-section-head .hint").innerText()).toBe(
      `${pet} does the work with Demo Agent in ~/Pawl. Changes, commands, web use and new sessions ask you first, by your approval rules.`,
    );
    expect(await sec.locator(".assistant-status-pill").textContent()).toContain("Ready");
    const agent = sec.getByLabel("Agent", { exact: true });
    expect(await agent.inputValue()).toBe("auto");
    expect(await agent.locator("option").first().innerText()).toBe("Automatic (Demo Agent)");

    // The browser needs browser use: off here, so the switch is off and disabled, with a link to Browser.
    const browserToggle = sec.locator("label.toggle", { hasText: "Can use the browser" });
    expect(await browserToggle.locator("input").isDisabled()).toBe(true);
    expect(await browserToggle.innerText()).toContain("Browser use is off");
    await sec.getByRole("button", { name: "Turn on browser use in Settings > Browser" }).click();
    await waitFor(
      async () => p.locator("#settings-browser").evaluate((el) => el.getBoundingClientRect().top >= 0 && el.getBoundingClientRect().top < window.innerHeight / 2),
      "Browser in view",
      5_000,
    );

    // Its session tools: off, then on again (that asks first).
    const tools = sec.locator("label.toggle", { hasText: "Can start and steer other sessions" });
    await tools.click();
    await waitFor(async () => fake.patches.some((x) => x.sessions === false), "sessions off", 10_000);
    await waitFor(async () => !(await tools.locator("input").isChecked()), "the switch off", 10_000);
    await tools.click();
    const dialog = p.locator(".confirm-dialog");
    await dialog.waitFor({ state: "visible", timeout: 10_000 });
    expect((await dialog.innerText()).replace(/\s+/g, " ")).toContain(`Let ${pet} start and steer other sessions?`);
    await dialog.getByRole("button", { name: "Allow", exact: true }).click();
    await waitFor(async () => fake.patches.some((x) => x.sessions === true), "sessions on", 10_000);

    // Workspace: a system folder comes back as the field's error; a typed folder is saved; reset to default.
    const ws = sec.getByLabel("Workspace", { exact: true });
    expect(await ws.inputValue()).toBe(ASSISTANT_DEFAULT);
    await ws.fill("C:\\Windows\\Temp");
    await ws.press("Enter");
    await sec.locator(".field-error", { hasText: "is a system folder" }).waitFor({ timeout: 10_000 });
    await ws.fill("E:\\jobs");
    await sec.getByRole("button", { name: "Save", exact: true }).click();
    await waitFor(async () => fake.assistant.workspace === "E:\\jobs", "the workspace saved", 10_000);
    await waitFor(async () => (await sec.locator(".settings-section-head .hint").innerText()).includes("in E:\\jobs."), "the intro follows", 10_000);
    await sec.getByRole("button", { name: "Reset to default" }).click();
    await waitFor(async () => (await ws.inputValue()) === ASSISTANT_DEFAULT, "back to the default", 10_000);
    expect(fake.patches.at(-1)).toEqual({ workspace: "" });

    // Open folder: the app's open_folder; a folder not made yet says so.
    await sec.getByRole("button", { name: "Open folder" }).click();
    await waitFor(
      async () => ((await p.evaluate(() => (window as unknown as { __opened?: string[] }).__opened)) ?? []).includes(ASSISTANT_DEFAULT),
      "open_folder",
      5_000,
    );
    await p.evaluate(() => ((window as unknown as { __openFolderError: string }).__openFolderError = "That folder doesn't exist."));
    await sec.getByRole("button", { name: "Open folder" }).click();
    await p.getByText(`~/Pawl doesn't exist yet: Pawl creates it the first time ${pet} works.`).waitFor({ timeout: 5_000 });

    // The hub's caution for an agent other than Claude Code, under the Agent dropdown (none for Claude Code).
    expect(await sec.locator(".assistant-agent-note").count()).toBe(0);
    const NOTE = "Codex keeps its own approval settings: Pawl can't lock them, so it may run some tool calls without a card.";
    fake.set({ resolvedAgent: "codex", agentNote: NOTE });
    await sec.locator(".assistant-agent-note", { hasText: NOTE }).waitFor({ timeout: 10_000 });
    fake.set({ resolvedAgent: DEMO_AGENT_ID, agentNote: undefined });
    await waitFor(async () => (await sec.locator(".assistant-agent-note").count()) === 0, "the caution gone", 10_000);

    // The hub's error shows as-is in the status line.
    fake.set({ state: "error", error: "Demo Agent could not start: the workspace is not writable" });
    await sec.locator(".assistant-status .field-error", { hasText: "the workspace is not writable" }).waitFor({ timeout: 10_000 });
    expect(await sec.locator(".assistant-status-pill").textContent()).toContain("Can't start");
    fake.set({ state: "idle", error: undefined });

    // New chat ends the work session.
    await sec.getByRole("button", { name: "New chat" }).click();
    await p.getByText(`New chat: ${pet} starts fresh`).waitFor({ timeout: 5_000 });
    expect(fake.resets).toBe(1);

    // Off: as before (New chat off too); on again asks first.
    const onOff = sec.locator("label.toggle", { hasText: `Let ${pet} do the work` });
    await onOff.click();
    await sec.locator(".assistant-status-pill", { hasText: "Off" }).waitFor({ timeout: 10_000 });
    expect(await sec.getByRole("button", { name: "New chat" }).isDisabled()).toBe(true);
    await onOff.click();
    await dialog.waitFor({ state: "visible", timeout: 10_000 });
    const asked = (await dialog.innerText()).replace(/\s+/g, " ");
    expect(asked).toContain(`Let ${pet} do the work?`);
    expect(asked).toContain("working in ~/Pawl");
    expect(asked).toContain("can never approve anything itself");
    await dialog.getByRole("button", { name: "Turn on", exact: true }).click();
    await sec.locator(".assistant-status-pill", { hasText: "Ready" }).waitFor({ timeout: 10_000 });
    expect(fake.patches.filter((x) => x.enabled !== undefined).map((x) => x.enabled)).toEqual([false, true]);
    expect(errors).toEqual([]);
    await p.close();
  }, 90_000);

  test("the bubble: the reply streams in place, a status line says what it does, the Assistant line and New chat; the session is tagged", async () => {
    const pet = (await hub.client.getSettings()).settings.bot.petName;
    const fake = fakeAssistantHub();
    const errors: string[] = [];
    // The panel first, so it sees the assistant session start.
    const panelPage = await openWithAssistant(fake, "panel.html", errors);
    await panelPage.locator(".sidebar").waitFor({ state: "visible", timeout: 20_000 });
    const petPage = await openWithAssistant(fake, "index.html", errors, { width: 520, height: 760 });
    const box = petPage.locator(".pet-box").first();
    await box.waitFor({ state: "visible", timeout: 20_000 });
    await box.click();
    const bubble = petPage.locator(".pet-bubble");
    await bubble.waitFor({ state: "visible", timeout: 10_000 });
    // With the assistant on: what it works with, under the text box.
    const line = bubble.locator(".pb-assistant-line");
    await waitFor(async () => (await line.innerText().catch(() => "")) === "Working with Demo Agent in ~/Pawl", "the assistant line", 15_000);

    await bubble.locator(".pb-composer textarea").fill("make me a todo app");
    await bubble.locator(".pb-composer textarea").press("Enter");
    const streaming = bubble.locator(".pb-msg.is-pet.is-streaming");
    await streaming.waitFor({ state: "visible", timeout: 10_000 });
    await waitFor(async () => (await streaming.innerText()).includes("On it. Reading your notes first"), "the reply so far", 10_000);
    expect(await streaming.locator(".pb-stream-dots").count()).toBe(1);
    await bubble.locator(".pb-activity", { hasText: "Reading notes.md" }).waitFor({ timeout: 10_000 });
    // The session in the switcher: the pet's name and an Assistant tag.
    await bubble.locator(".pb-switch-chip", { hasText: "Assistant" }).locator(".pb-switch-name", { hasText: pet }).waitFor({ timeout: 10_000 });
    // ...and in the panel's session list.
    const row = panelPage.locator(".session-row", { has: panelPage.locator(".chip-assistant") });
    await row.waitFor({ timeout: 10_000 });
    expect(await row.locator(".session-row-title").innerText()).toContain(pet);

    // The turn ends: the same message, finished in place; the status line goes.
    fake.finish("Done: ~/Pawl/todo has the app. Open index.html to try it.");
    await waitFor(async () => (await streaming.count()) === 0, "the reply finished", 10_000);
    const replies = bubble.locator(".pb-msg.is-pet");
    expect(await replies.last().innerText()).toContain("Done: ~/Pawl/todo has the app.");
    expect(await replies.last().locator(".pb-time").count()).toBe(1);
    expect(await replies.filter({ hasText: "On it." }).count()).toBe(0);
    await waitFor(async () => (await bubble.locator(".pb-activity").count()) === 0, "no status line", 10_000);

    // The assistant asks to start a session: it is the pet asking, with the whole prompt (scrolls, never cut).
    const PROMPT = `Fix the flaky login test in api.\n\n${"The test waits on a timer that races the mock clock, so it fails one run in ten. ".repeat(12)}Run it 20 times to be sure, then tell me.`;
    const startId = fake.askToStart(PROMPT);
    const card = bubble.locator(".pb-approval.is-assistant");
    await card.waitFor({ state: "visible", timeout: 10_000 });
    // A new card scrolls the bubble to it (cards sit on top of the chat).
    await waitFor(
      async () => card.evaluate((el) => el.getBoundingClientRect().top >= el.closest(".pb-body")!.getBoundingClientRect().top - 1),
      "the card in view",
      5_000,
    );
    expect(await card.locator(".pb-approval-who").innerText()).toBe(`${pet} (your assistant) asks:`);
    expect(await card.locator(".pb-approval-text").innerText()).toBe("Start Codex in api?");
    const prompt = card.locator(".pb-prompt");
    expect(await prompt.textContent()).toBe(PROMPT);
    expect(await prompt.evaluate((el) => el.scrollHeight > el.clientHeight && getComputedStyle(el).overflowY === "auto")).toBe(true);
    // The panel's card too, even the compact one on top: the whole prompt.
    const panelPrompt = panelPage.locator(".perm-card .perm-prompt").first();
    await panelPrompt.waitFor({ timeout: 10_000 });
    expect(await panelPrompt.textContent()).toBe(PROMPT);
    expect(await panelPage.locator(".perm-card .perm-who").first().innerText()).toContain(`${pet} (your assistant) asks`);
    await card.getByRole("button", { name: "Start", exact: true }).click();
    await waitFor(async () => fake.answers.get(startId) === "allow", "Start answered", 10_000);
    await waitFor(async () => (await bubble.locator(".pb-approval.is-assistant").count()) === 0, "the card gone", 10_000);

    // The Assistant line opens Settings > Assistant; New chat starts over (the messages stay).
    await line.click();
    await waitFor(
      async () => JSON.stringify(await petPage.evaluate(() => (window as unknown as { __panels?: unknown[] }).__panels ?? [])).includes('"section":"assistant"'),
      "open_panel settings/assistant",
      5_000,
    );
    await bubble.getByRole("button", { name: "New chat" }).click();
    await bubble.locator(".pb-divider", { hasText: "New chat" }).waitFor({ timeout: 5_000 });
    expect(fake.resets).toBe(1);
    expect(await replies.count()).toBeGreaterThan(0);

    // Off: the bubble as before (no line, no New chat).
    fake.set({ enabled: false, state: "off", sessionId: undefined });
    await waitFor(async () => (await bubble.locator(".pb-assistant").count()) === 0, "no assistant line", 10_000);
    expect(errors).toEqual([]);
    await petPage.close();
    await panelPage.close();
  }, 90_000);
});
