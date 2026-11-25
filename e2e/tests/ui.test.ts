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
