// The desktop UI end to end: the real panel and the pet window (a production `vite build` of apps/desktop, served
// locally) in Chromium, talking to a real hub, with the scripted agent behind it. One pet on screen: the default pet
// (it orchestrates every session); agents' pets are renamed under Settings > Pets (advanced).
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Server } from "bun";
import { type Browser, type Page, chromium } from "playwright";
import type { A2aCallerSetting, A2aConnectStatus, HubSettings } from "@pawl/protocol";
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
