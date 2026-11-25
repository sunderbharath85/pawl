import { describe, expect, test } from "bun:test";
import {
  BUBBLE_GAP,
  BUBBLE_MAX_H,
  BUBBLE_W,
  bubbleVertical,
  DRAG_THRESHOLD_PX,
  FRAME_PAD,
  layoutPetWindow,
  NAME_H,
  pastDragThreshold,
  petFrame,
  placeBubble,
  PressTracker,
  SCREEN_EDGE,
  sessionSide,
  type PetSpace,
} from "./petLayout";

const roomy: PetSpace = { left: 900, right: 900, above: 900, below: 900 };

describe("pet window layout", () => {
  test("one pet, plenty of room: bubble above, centred, window = pet + bubble room", () => {
    const l = layoutPetWindow({ petSize: 112, showNames: true, sessionCount: 0, space: roomy });
    expect(l.vertical).toBe("above");
    expect(l.main).toEqual({ x: FRAME_PAD + (BUBBLE_W - 112) / 2, y: FRAME_PAD + BUBBLE_GAP + BUBBLE_MAX_H, w: 112, h: 112 + NAME_H });
    expect(l.width).toBe(BUBBLE_W + 2 * FRAME_PAD);
    expect(l.height).toBe(BUBBLE_MAX_H + BUBBLE_GAP + 112 + NAME_H + 2 * FRAME_PAD);
    const b = placeBubble(l, l.main);
    expect(b.left).toBe(FRAME_PAD);
    expect(b.tailX).toBe(BUBBLE_W / 2);
    expect(b.anchorY).toBe(l.main.y - BUBBLE_GAP);
    expect(petFrame(l)).toEqual({ width: l.width, height: l.height, petX: l.main.x, petY: l.main.y, petW: 112, petH: 132 });
  });

  test("near the top of the screen the bubble flips below", () => {
    const l = layoutPetWindow({ petSize: 112, showNames: false, sessionCount: 0, space: { ...roomy, above: 40 } });
    expect(l.vertical).toBe("below");
    expect(l.main.y).toBe(FRAME_PAD);
    expect(placeBubble(l, l.main).anchorY).toBe(FRAME_PAD + 112 + BUBBLE_GAP);
    expect(l.height).toBe(112 + BUBBLE_GAP + BUBBLE_MAX_H + 2 * FRAME_PAD);
  });

  test("short screens: the roomier side, and the bubble gets shorter, never taller than the room there", () => {
    expect(bubbleVertical({ ...roomy, above: 300, below: 200 })).toBe("above");
    expect(bubbleVertical({ ...roomy, above: 200, below: 300 })).toBe("below");
    expect(bubbleVertical(null)).toBe("above");
    const l = layoutPetWindow({ petSize: 112, showNames: false, sessionCount: 0, space: { ...roomy, above: 300, below: 200 } });
    expect(l.bubbleMaxH).toBe(300 - BUBBLE_GAP - SCREEN_EDGE);
    // Neither side has much room: the bubble fits the roomier side (above: 150 px) instead of overflowing the screen.
    const short = layoutPetWindow({ petSize: 112, showNames: false, sessionCount: 0, space: { ...roomy, above: 150, below: 40 } });
    expect(short.vertical).toBe("above");
    expect(short.bubbleMaxH).toBe(150 - BUBBLE_GAP - SCREEN_EDGE);
    expect(short.bubbleMaxH + BUBBLE_GAP).toBeLessThanOrEqual(150);
    // Its room in the window matches (the window does not reach past the screen for it).
    expect(short.main.y).toBe(FRAME_PAD + BUBBLE_GAP + short.bubbleMaxH);
    // No room at all: zero, never negative.
    const none = layoutPetWindow({ petSize: 112, showNames: false, sessionCount: 0, space: { ...roomy, above: 0, below: 0 } });
    expect(none.bubbleMaxH).toBe(0);
  });

  test("near the right edge the bubble shifts left to stay on screen (and the tail still points at the pet)", () => {
    const l = layoutPetWindow({ petSize: 100, showNames: false, sessionCount: 0, space: { ...roomy, right: 20 } });
    const b = placeBubble(l, l.main);
    // Bubble's right edge = pet's right edge + 20 px of space - the screen margin.
    expect(b.left - l.main.x + BUBBLE_W).toBe(100 + 20 - SCREEN_EDGE);
    expect(b.tailX).toBe(l.main.x + 50 - b.left);
    expect(b.tailX).toBeGreaterThan(BUBBLE_W / 2);
  });

  test("near the left edge the bubble shifts right", () => {
    const l = layoutPetWindow({ petSize: 100, showNames: false, sessionCount: 0, space: { ...roomy, left: 0 } });
    const b = placeBubble(l, l.main);
    expect(b.left - l.main.x).toBe(SCREEN_EDGE);
    expect(b.tailX).toBeLessThan(BUBBLE_W / 2);
    expect(b.left).toBeGreaterThanOrEqual(FRAME_PAD);
  });

  test("a screen narrower than the bubble pins it to the left edge", () => {
    const l = layoutPetWindow({ petSize: 100, showNames: false, sessionCount: 0, space: { left: 10, right: 10, above: 900, below: 0 } });
    expect(placeBubble(l, l.main).left - l.main.x).toBe(-10 + SCREEN_EDGE);
  });

  test("session pets line up right of the default pet, or left when the right has no room", () => {
    const right = layoutPetWindow({ petSize: 100, showNames: true, sessionCount: 2, more: true, space: roomy });
    expect(right.side).toBe("right");
    expect(right.sessions).toHaveLength(2);
    expect(right.sessions[0]!.x).toBeGreaterThan(right.main.x + right.main.w);
    expect(right.sessions[1]!.x).toBeGreaterThan(right.sessions[0]!.x);
    // Bottom-aligned with the default pet, smaller.
    expect(right.sessions[0]!.y + right.sessions[0]!.h).toBe(right.main.y + right.main.h);
    expect(right.sessions[0]!.w).toBe(75);
    expect(right.more!.x).toBeGreaterThan(right.sessions[1]!.x);
    // Every session pet's bubble fits in the window.
    for (const s of right.sessions) {
      const b = placeBubble(right, s);
      expect(b.left).toBeGreaterThanOrEqual(0);
      expect(b.left + BUBBLE_W).toBeLessThanOrEqual(right.width);
    }

    const left = layoutPetWindow({ petSize: 100, showNames: true, sessionCount: 2, space: { ...roomy, right: 30 } });
    expect(left.side).toBe("left");
    expect(left.sessions[0]!.x + left.sessions[0]!.w).toBeLessThan(left.main.x);
    expect(left.sessions[1]!.x).toBeLessThan(left.sessions[0]!.x);
    expect(Math.min(...left.sessions.map((s) => s.x))).toBeGreaterThanOrEqual(FRAME_PAD);
    expect(sessionSide({ ...roomy, right: 30, left: 20 }, 200)).toBe("right");
    expect(sessionSide(null, 200)).toBe("right");
  });

  test("opening a bubble needs no window change: the layout does not depend on it", () => {
    const a = layoutPetWindow({ petSize: 112, showNames: true, sessionCount: 1, space: roomy });
    const b = layoutPetWindow({ petSize: 112, showNames: true, sessionCount: 1, space: roomy });
    expect(petFrame(a)).toEqual(petFrame(b));
  });
});

describe("click vs drag", () => {
  test("threshold", () => {
    expect(DRAG_THRESHOLD_PX).toBe(4);
    expect(pastDragThreshold(3, 0)).toBe(false);
    expect(pastDragThreshold(2, 2)).toBe(false);
    expect(pastDragThreshold(4, 0)).toBe(true);
    expect(pastDragThreshold(-3, -3)).toBe(true);
  });

  test("a small wobble is still a click", () => {
    const p = new PressTracker();
    p.down(100, 100);
    expect(p.move(102, 101)).toBe(false);
    p.up();
    expect(p.click()).toBe(true);
  });

  test("moving past the threshold starts one drag, and the click after it does not count", () => {
    const p = new PressTracker();
    p.down(100, 100);
    expect(p.move(103, 100)).toBe(false);
    expect(p.move(105, 100)).toBe(true);
    expect(p.move(140, 100)).toBe(false);
    p.up();
    expect(p.click()).toBe(false);
    // The next press is a fresh click.
    p.down(10, 10);
    p.up();
    expect(p.click()).toBe(true);
  });

  test("a drag whose release never reaches the page (native move loop) does not eat the next click", () => {
    const p = new PressTracker();
    p.down(0, 0);
    expect(p.move(0, 10)).toBe(true);
    p.down(50, 50);
    p.up();
    expect(p.click()).toBe(true);
  });

  test("moves without a press do nothing", () => {
    expect(new PressTracker().move(100, 100)).toBe(false);
    expect(new PressTracker().hold(10_000)).toBe(false);
    expect(new PressTracker().up()).toBe(false);
  });

  test("press and hold 400 ms without moving is push-to-talk: once; the release stops it; no click after it", () => {
    const p = new PressTracker();
    p.down(100, 100, 1000);
    expect(p.hold(1399)).toBe(false);
    expect(p.move(102, 101)).toBe(false);
    expect(p.hold(1400)).toBe(true);
    expect(p.hold(1500)).toBe(false);
    // Moving while listening does not start a drag.
    expect(p.move(140, 100)).toBe(false);
    expect(p.up()).toBe(true);
    expect(p.click()).toBe(false);
    // The next press is a fresh click.
    p.down(10, 10, 5000);
    expect(p.up()).toBe(false);
    expect(p.click()).toBe(true);
  });

  test("a drag before 400 ms stays a drag (no hold after it); a quick click stays a click", () => {
    const p = new PressTracker();
    p.down(0, 0, 0);
    expect(p.move(6, 0)).toBe(true);
    expect(p.hold(1000)).toBe(false);
    expect(p.up()).toBe(false);
    expect(p.click()).toBe(false);
    p.down(0, 0, 2000);
    expect(p.hold(2100)).toBe(false);
    expect(p.up()).toBe(false);
    expect(p.click()).toBe(true);
  });
});
