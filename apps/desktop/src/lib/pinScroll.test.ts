import { describe, expect, test } from "bun:test";
import { pinWindowScroll, type ScrollableWindow } from "./pinScroll";

/** A window that scrolls like the panel's did: focus moved it, the scroll event followed. */
function fakeWindow(start = { x: 0, y: 0 }) {
  let listener: (() => void) | null = null;
  const calls: Array<[number, number]> = [];
  const win = {
    scrollX: start.x,
    scrollY: start.y,
    scrollTo(x: number, y: number) {
      calls.push([x, y]);
      win.scrollX = x;
      win.scrollY = y;
    },
    addEventListener: (_type: "scroll", fn: () => void) => (listener = fn),
    removeEventListener: (_type: "scroll", fn: () => void) => {
      if (listener === fn) listener = null;
    },
  };
  /** The browser scrolled the window (bringing a focused element into view). */
  const scrollBy = (y: number) => {
    win.scrollY += y;
    listener?.();
  };
  return { win: win satisfies ScrollableWindow, calls, scrollBy, listening: () => listener !== null };
}

describe("pinWindowScroll", () => {
  test("a window scrolled by focus (the blank Settings window: 1903 px down) goes straight back to the top", () => {
    const f = fakeWindow();
    pinWindowScroll(f.win);
    expect(f.calls).toEqual([]);
    f.scrollBy(1903);
    expect(f.calls).toEqual([[0, 0]]);
    expect(f.win.scrollY).toBe(0);
  });

  test("a window already scrolled when it starts is put back at once", () => {
    const f = fakeWindow({ x: 0, y: 240 });
    pinWindowScroll(f.win);
    expect(f.win.scrollY).toBe(0);
  });

  test("scroll events that leave it at the top change nothing; the undo stops listening", () => {
    const f = fakeWindow();
    const undo = pinWindowScroll(f.win);
    f.scrollBy(0);
    expect(f.calls).toEqual([]);
    undo();
    expect(f.listening()).toBe(false);
  });
});
