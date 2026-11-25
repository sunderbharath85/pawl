// Geometry of the desktop pet window (pure, no DOM). The window is small and transparent: the default pet, the
// session pets lined up beside it, and room for one bubble on the side of the screen that has space. Opening or
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
/** Session pets are drawn at this fraction of the default pet's size. */
export const SESSION_PET_SCALE = 0.75;
export const ROW_GAP = 8;
export const MORE_CHIP_W = 36;
/** The bubble's tail stays this far from the bubble's corners. */
const TAIL_INSET = 22;

export interface PetLayoutInput {
  petSize: number;
  showNames: boolean;
  sessionCount: number;
  /** Show a "+N" chip after the session pets. */
  more?: boolean;
  /** Null until Rust reported where the pet is (layout as if the screen were endless, bubble above). */
  space: PetSpace | null;
}

export interface PetLayout {
  /** Window size. */
  width: number;
  height: number;
  /** Default pet box, window coords. */
  main: Rect;
  /** Session pet boxes, window coords, nearest the default pet first. */
  sessions: Rect[];
  more: Rect | null;
  /** Session pets line up on this side of the default pet. */
  side: "right" | "left";
  /** The bubble opens above or below the pets. */
  vertical: "above" | "below";
  /** The bubble's max height (it scrolls inside beyond that). */
  bubbleMaxH: number;
  /** Window offset of pet coords (= the default pet's top-left in the window). */
  origin: { x: number; y: number };
  /** The pet coords → screen clamp range for a bubble's left edge (null = no limit). */
  bubbleRange: { min: number; max: number } | null;
}

function sizes(petSize: number, showNames: boolean) {
  const nameH = showNames ? NAME_H : 0;
  const s = Math.round(petSize * SESSION_PET_SCALE);
  return { mainW: petSize, mainH: petSize + nameH, sessW: s, sessH: s + nameH };
}

/** Above when there is room for a full bubble there, else below when there is, else the roomier side. */
export function bubbleVertical(space: PetSpace | null): "above" | "below" {
  if (!space) return "above";
  const need = BUBBLE_MAX_H + BUBBLE_GAP + SCREEN_EDGE;
  if (space.above >= need) return "above";
  if (space.below >= need) return "below";
  return space.above >= space.below ? "above" : "below";
}

/** Session pets go right of the default pet when they fit there (or right has more room), else left. */
export function sessionSide(space: PetSpace | null, rowWidth: number): "right" | "left" {
  if (!space || rowWidth <= 0) return "right";
  if (space.right >= rowWidth) return "right";
  if (space.left >= rowWidth) return "left";
  return space.right >= space.left ? "right" : "left";
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
  const n = Math.max(0, Math.floor(input.sessionCount));
  const { mainW, mainH, sessW, sessH } = sizes(petSize, showNames);
  const step = sessW + ROW_GAP;
  const rowWidth = n > 0 ? n * step + (input.more ? MORE_CHIP_W + ROW_GAP : 0) : 0;
  const side = sessionSide(space, rowWidth);

  // Pet coords.
  const main: Rect = { x: 0, y: 0, w: mainW, h: mainH };
  const sessions: Rect[] = [];
  for (let i = 0; i < n; i++) {
    const x = side === "right" ? mainW + ROW_GAP + i * step : -(i + 1) * step;
    sessions.push({ x, y: mainH - sessH, w: sessW, h: sessH });
  }
  let more: Rect | null = null;
  if (n > 0 && input.more) {
    const x = side === "right" ? mainW + ROW_GAP + n * step : -(n * step) - MORE_CHIP_W - ROW_GAP;
    more = { x, y: mainH - sessH / 2 - 14, w: MORE_CHIP_W, h: 28 };
  }

  const vertical = bubbleVertical(space);
  const available = space ? (vertical === "above" ? space.above : space.below) - BUBBLE_GAP - SCREEN_EDGE : BUBBLE_MAX_H;
  // Never taller than the screen has room for on that side (a short screen gets a short, scrolling bubble).
  const bubbleMaxH = Math.round(Math.max(0, Math.min(BUBBLE_MAX_H, available)));
  const bubbleRange = space ? { min: -space.left + SCREEN_EDGE, max: mainW + space.right - BUBBLE_W - SCREEN_EDGE } : null;

  // Room: the pets plus a bubble for any of them.
  const boxes = [main, ...sessions, ...(more ? [more] : [])];
  let minX = Math.min(...boxes.map((b) => b.x));
  let maxX = Math.max(...boxes.map((b) => b.x + b.w));
  for (const t of [main, ...sessions]) {
    const left = bubbleLeft(t, bubbleRange);
    minX = Math.min(minX, left);
    maxX = Math.max(maxX, left + BUBBLE_W);
  }
  const minY = vertical === "above" ? -(BUBBLE_GAP + bubbleMaxH) : 0;
  const maxY = vertical === "above" ? mainH : mainH + BUBBLE_GAP + bubbleMaxH;

  const ox = Math.ceil(FRAME_PAD - minX);
  const oy = Math.ceil(FRAME_PAD - minY);
  const shift = (r: Rect): Rect => ({ x: Math.round(r.x + ox), y: Math.round(r.y + oy), w: Math.round(r.w), h: Math.round(r.h) });
  return {
    width: Math.ceil(maxX - minX + 2 * FRAME_PAD),
    height: Math.ceil(maxY - minY + 2 * FRAME_PAD),
    main: shift(main),
    sessions: sessions.map(shift),
    more: more ? shift(more) : null,
    side,
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
