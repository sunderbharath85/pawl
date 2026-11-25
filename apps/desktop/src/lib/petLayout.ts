// Geometry of the desktop pet window (pure, no DOM). The window is small and transparent: the one pet (the default
// pet, which orchestrates every session) and room for its bubble on the side of the screen that has space. Opening or
// closing a bubble never moves the window (the room is always there and click-through), so the pet never jumps;
// the room only flips (above / below, shifted left / right) when the pet was moved to another part of the screen.
// All values are logical (CSS) px. "Pet coords" have their origin at the default pet's top-left corner.

/** Free space from the default pet's box to the edges of its monitor's work area (logical px, from Rust). */
export interface PetSpace {
  left: number;
  right: number;
  above: number;
  below: number;
}

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export const BUBBLE_W = 320;
export const BUBBLE_MAX_H = 420;
export const BUBBLE_GAP = 10;
/** Keep bubbles this far from the work area's edges. */
export const SCREEN_EDGE = 6;
/** Transparent margin around everything (drop shadows). */
export const FRAME_PAD = 10;
export const NAME_H = 20;
/** The bubble's tail stays this far from the bubble's corners. */
const TAIL_INSET = 22;

export interface PetLayoutInput {
  petSize: number;
  showNames: boolean;
  /** Null until Rust reported where the pet is (layout as if the screen were endless, bubble above). */
  space: PetSpace | null;
}

export interface PetLayout {
  /** Window size. */
  width: number;
  height: number;
  /** The pet's box, window coords. */
  main: Rect;
  /** The bubble opens above or below the pets. */
  vertical: "above" | "below";
  /** The bubble's max height (it scrolls inside beyond that). */
  bubbleMaxH: number;
  /** Window offset of pet coords (= the default pet's top-left in the window). */
  origin: { x: number; y: number };
  /** The pet coords → screen clamp range for a bubble's left edge (null = no limit). */
  bubbleRange: { min: number; max: number } | null;
}

/** Above when there is room for a full bubble there, else below when there is, else the roomier side. */
export function bubbleVertical(space: PetSpace | null): "above" | "below" {
  if (!space) return "above";
  const need = BUBBLE_MAX_H + BUBBLE_GAP + SCREEN_EDGE;
  if (space.above >= need) return "above";
  if (space.below >= need) return "below";
  return space.above >= space.below ? "above" : "below";
}

/** Left edge (pet coords) of the bubble pointing at `target`: centred on it, shifted to stay on screen. */
export function bubbleLeft(target: Rect, range: PetLayout["bubbleRange"]): number {
  const x = target.x + target.w / 2 - BUBBLE_W / 2;
  if (!range) return x;
  if (range.max < range.min) return range.min;
  return Math.min(range.max, Math.max(range.min, x));
}

export function layoutPetWindow(input: PetLayoutInput): PetLayout {
  const { petSize, showNames, space } = input;
  // Pet coords.
  const main: Rect = { x: 0, y: 0, w: petSize, h: petSize + (showNames ? NAME_H : 0) };

  const vertical = bubbleVertical(space);
  const available = space ? (vertical === "above" ? space.above : space.below) - BUBBLE_GAP - SCREEN_EDGE : BUBBLE_MAX_H;
  // Never taller than the screen has room for on that side (a short screen gets a short, scrolling bubble).
  const bubbleMaxH = Math.round(Math.max(0, Math.min(BUBBLE_MAX_H, available)));
  const bubbleRange = space ? { min: -space.left + SCREEN_EDGE, max: main.w + space.right - BUBBLE_W - SCREEN_EDGE } : null;

  // Room: the pet plus its bubble.
  const left = bubbleLeft(main, bubbleRange);
  const minX = Math.min(main.x, left);
  const maxX = Math.max(main.x + main.w, left + BUBBLE_W);
  const minY = vertical === "above" ? -(BUBBLE_GAP + bubbleMaxH) : 0;
  const maxY = vertical === "above" ? main.h : main.h + BUBBLE_GAP + bubbleMaxH;
  const ox = Math.ceil(FRAME_PAD - minX);
  const oy = Math.ceil(FRAME_PAD - minY);
  const shift = (r: Rect): Rect => ({ x: Math.round(r.x + ox), y: Math.round(r.y + oy), w: Math.round(r.w), h: Math.round(r.h) });

  return {
    width: Math.ceil(maxX - minX + 2 * FRAME_PAD),
    height: Math.ceil(maxY - minY + 2 * FRAME_PAD),
    main: shift(main),
    vertical,
    bubbleMaxH,
    origin: { x: ox, y: oy },
    bubbleRange,
  };
}

export interface BubblePlace {
  /** Window coords of the bubble's left edge. */
  left: number;
  /** Above: distance of the bubble's bottom edge from the window's top; below: its top edge. */
  anchorY: number;
  vertical: "above" | "below";
  /** Tail position from the bubble's left edge. */
  tailX: number;
  maxHeight: number;
}

/** Where the bubble for the pet box `target` (window coords) goes inside `layout`. */
export function placeBubble(layout: PetLayout, target: Rect): BubblePlace {
  const t: Rect = { ...target, x: target.x - layout.origin.x, y: target.y - layout.origin.y };
  const left = bubbleLeft(t, layout.bubbleRange);
  const center = t.x + t.w / 2;
  const tailX = Math.min(BUBBLE_W - TAIL_INSET, Math.max(TAIL_INSET, center - left));
  const mainTop = layout.main.y;
  const mainBottom = layout.main.y + layout.main.h;
  return {
    left: Math.round(left + layout.origin.x),
    anchorY: layout.vertical === "above" ? mainTop - BUBBLE_GAP : mainBottom + BUBBLE_GAP,
    vertical: layout.vertical,
    tailX: Math.round(tailX),
    maxHeight: layout.bubbleMaxH,
  };
}

/** What Rust needs to size the window and keep the default pet where the user put it. */
export interface PetFrame {
  width: number;
  height: number;
  petX: number;
  petY: number;
  petW: number;
  petH: number;
}

export function petFrame(layout: PetLayout): PetFrame {
  return { width: layout.width, height: layout.height, petX: layout.main.x, petY: layout.main.y, petW: layout.main.w, petH: layout.main.h };
}

// ---------------------------------------------------------------- press: click vs drag vs hold

/** A press that moves farther than this (px) drags the pet; less is a click. */
export const DRAG_THRESHOLD_PX = 4;
/** A press held this long without moving is push-to-talk (voice on): listening starts, and the release stops it. */
export const HOLD_MS = 400;

export function pastDragThreshold(dx: number, dy: number, threshold = DRAG_THRESHOLD_PX): boolean {
  return Math.hypot(dx, dy) >= threshold;
}

/**
 * Tells a click from a drag from a hold for one pointer: `down`, then `move` returns true once (start the native
 * drag) when the pointer moved past the threshold; `hold` returns true once when the press lasted `HOLD_MS` without
 * moving (start listening); `up` returns true when it ends a hold (stop listening); `click` says whether the click
 * that follows counts (not after a drag or a hold).
 */
export class PressTracker {
  private start: { x: number; y: number; at: number } | null = null;
  private dragged = false;
  private held = false;

  constructor(
    private readonly threshold = DRAG_THRESHOLD_PX,
    private readonly holdMs = HOLD_MS,
  ) {}

  down(x: number, y: number, at = 0): void {
    this.start = { x, y, at };
    this.dragged = false;
    this.held = false;
  }

  /** True exactly once per press, when it becomes a drag (never once it is a hold: the pet stays put while listening). */
  move(x: number, y: number): boolean {
    if (!this.start || this.dragged || this.held) return false;
    if (!pastDragThreshold(x - this.start.x, y - this.start.y, this.threshold)) return false;
    this.dragged = true;
    this.start = null;
    return true;
  }

  /** True exactly once per press, when it has lasted `holdMs` without becoming a drag. */
  hold(at: number): boolean {
    if (!this.start || this.dragged || this.held) return false;
    if (at - this.start.at < this.holdMs) return false;
    this.held = true;
    return true;
  }

  /** The press ends: true when it was a hold (stop listening). */
  up(): boolean {
    const wasHold = this.held && this.start !== null;
    this.start = null;
    return wasHold;
  }

  /** Whether a click counts: false right after a drag or a hold (then forgotten, so the next click counts again). */
  click(): boolean {
    const ok = !this.dragged && !this.held;
    this.dragged = false;
    this.held = false;
    return ok;
  }
}
