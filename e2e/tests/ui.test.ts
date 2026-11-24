// The desktop UI end to end: the real panel and pet strip (a production `vite build` of apps/desktop, served
// locally) in Chromium, talking to a real hub, with the scripted agent behind it.
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Server } from "bun";
import { type Browser, type Page, chromium } from "playwright";
import { REPO, tempRoot, transcript } from "../lib/env";
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

  test("renaming a pet in the Pets view saves it on the hub", async () => {
    await panel.getByRole("button", { name: /^Pets/ }).first().click();
    const card = panel.locator("article.pet-card", { hasText: "Watches Demo Agent" });
    const name = card.getByLabel("Pet name");
    await name.fill("Pixelpaw");
    await card.getByRole("button", { name: "Save" }).click();
    const pet = await hub.waitState((s) => Object.values(s.pets).find((p) => p.agentId === DEMO_AGENT_ID && p.name === "Pixelpaw"), "the new pet name");
    expect(pet.name).toBe("Pixelpaw");
  }, 30_000);

  test("the pet strip shows a bubble, and Approve answers the agent", async () => {
    const strip = await openPage("index.html", { width: 1280, height: 280 });
    const session = await hub.client.startSession({ agentId: DEMO_AGENT_ID, cwd: repo, prompt: "run the strip tests" });
    await hub.waitState((s) => Object.values(s.pending).some((p) => p.sessionId === session.id), "the request");
    const approve = strip.getByRole("button", { name: "Approve" }).first();
    await approve.waitFor({ state: "visible", timeout: 20_000 });
    await approve.click();
    expect(await answerOf(session.id)).toBe("allow");
    await strip.close();
  }, 60_000);
});
