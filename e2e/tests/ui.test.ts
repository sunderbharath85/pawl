// The desktop UI end to end: the real panel and the pet window (a production `vite build` of apps/desktop, served
// locally) in Chromium, talking to a real hub, with the scripted agent behind it. One pet on screen: the default pet
// (it orchestrates every session); agents' pets are renamed under Settings > Pets (advanced).
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Server } from "bun";
import { type Browser, type Page, chromium } from "playwright";
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
    expect(await p.locator("#settings-a2a .a2a-part").count()).toBe(3);
    expect(await p.locator(".sidebar").count()).toBe(1);

    // Reload renders the section again (the voice engine answers properly now).
    await p.evaluate(() => ((window as unknown as { __voiceBroken: boolean }).__voiceBroken = false));
    await card.getByRole("button", { name: "Reload" }).click();
    await p.locator("#settings-voice label.toggle", { hasText: "Talk to" }).waitFor({ state: "visible", timeout: 10_000 });
    expect(await p.locator("#settings-voice .crash-card").count()).toBe(0);
    await p.close();
  }, 60_000);
});
