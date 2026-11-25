import { afterEach, describe, expect, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { ScreenLook } from "../pet/ScreenLook";
import {
  captureDataUrl,
  pngDataUrl,
  screenBackend,
  screenRetry,
  SCREEN_OPEN_AGAIN,
  SCREEN_PICK_AGAIN,
  SCREEN_SLOW_DOWN,
  setScreenBackend,
  sortSources,
  toChatImage,
  type ScreenSource,
} from "./screen";
import { createScreenMock } from "./screenMock";

const src = (over: Partial<ScreenSource>): ScreenSource => ({ id: "x", kind: "window", title: "t", app: "a", thumb: "AAAA", ...over });

afterEach(() => setScreenBackend(null));

describe("screen captures", () => {
  test("no screen access outside the desktop app (browser dev without the mock)", () => {
    expect(screenBackend()).toBeNull();
  });

  test("screens first, then windows by app and title", () => {
    const list = sortSources([src({ id: "w2", app: "Zed" }), src({ id: "s1", kind: "screen", title: "Screen 1" }), src({ id: "w1", app: "Chrome" })]);
    expect(list.map((s) => s.id)).toEqual(["s1", "w1", "w2"]);
  });

  test("a capture becomes the message's one picture (type and data only), shown from memory", () => {
    const c = { mediaType: "image/jpeg" as const, data: "QUJD", width: 10, height: 5 };
    expect(toChatImage(c)).toEqual({ mediaType: "image/jpeg", data: "QUJD" });
    expect(captureDataUrl(c)).toBe("data:image/jpeg;base64,QUJD");
    expect(pngDataUrl("QUJD")).toBe("data:image/png;base64,QUJD");
  });

  test("the look flow says what a capture can show, before anything is captured; nothing without screen access", () => {
    const props = {
      title: "Show my screen",
      question: "What do you see?",
      destination: { text: "to Claude Code (Anthropic's cloud, through your plan)", canSee: true, leavesPc: true, copy: "Pawl keeps no copy; the agent may keep it in its own history." },
      onSend: async () => {},
      onClose: () => {},
    };
    expect(renderToStaticMarkup(createElement(ScreenLook, props))).toBe("");
    setScreenBackend({
      begin: async () => "n",
      sources: async () => [],
      capture: async () => ({ mediaType: "image/png", data: "", width: 0, height: 0 }),
      end: async () => {},
    });
    const html = renderToStaticMarkup(createElement(ScreenLook, props));
    expect(html).toContain("Show my screen");
    expect(html).toContain("including passwords or private messages");
    expect(html).toContain("Looking for screens and windows");
    expect(html).not.toContain("<img");
  });

  test("after a gate error, Try again: a new picker session, the list again, or the list after a wait", () => {
    expect(screenRetry(SCREEN_OPEN_AGAIN)).toBe("restart");
    expect(screenRetry(SCREEN_PICK_AGAIN)).toBe("reload");
    expect(screenRetry(SCREEN_SLOW_DOWN)).toBe("wait");
    expect(screenRetry("network down")).toBe("reload");
  });
});

describe("the screen gate (through the dev mock, which follows its rules)", () => {
  test("nothing without a picker session; a new session replaces the old; end closes it", async () => {
    const m = createScreenMock({ delayMs: 0 });
    await expect(m.sources("nope")).rejects.toThrow(SCREEN_OPEN_AGAIN);
    const a = await m.begin();
    const b = await m.begin();
    await expect(m.sources(a)).rejects.toThrow(SCREEN_OPEN_AGAIN);
    expect((await m.sources(b)).length).toBe(4);
    await m.end(b);
    expect(m.open()).toBeNull();
    await expect(m.capture(b, "screen:1")).rejects.toThrow(SCREEN_OPEN_AGAIN);
  });

  test("only ids from the latest list; at most 3 captures and 5 lists per session", async () => {
    const m = createScreenMock({ delayMs: 0 });
    const n = await m.begin();
    await expect(m.capture(n, "screen:1")).rejects.toThrow(SCREEN_PICK_AGAIN);
    await m.sources(n);
    await expect(m.capture(n, "win:gone")).rejects.toThrow(SCREEN_PICK_AGAIN);
    for (let i = 0; i < 3; i++) expect((await m.capture(n, "win:term")).mediaType).toBe("image/png");
    await expect(m.capture(n, "win:term")).rejects.toThrow(SCREEN_SLOW_DOWN);
    for (let i = 0; i < 4; i++) await m.sources(n);
    await expect(m.sources(n)).rejects.toThrow(SCREEN_SLOW_DOWN);
  });
});
