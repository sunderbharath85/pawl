// Pure layout/motion helpers for PetStage (no React; unit-tested).

export interface Lane {
  /** Leftmost / rightmost allowed pet center x. */
  min: number;
  max: number;
  /** Resting center x. */
  home: number;
}

/** Split the strip into equal lanes, one per pet; pets wander only inside their own lane. */
export function computeLanes(ids: readonly string[], width: number, petSize: number): Record<string, Lane> {
  const out: Record<string, Lane> = {};
  const n = ids.length;
  if (n === 0) return out;
  const w = Math.max(0, width);
  const laneW = w / n;
  const half = petSize / 2;
  ids.forEach((id, i) => {
    const start = i * laneW;
    const home = start + laneW / 2;
    let min = start + half;
    let max = start + laneW - half;
    if (max < min) {
      // Lane narrower than a pet: no wandering, stay centered (pets may overlap a little).
      min = max = home;
    }
    out[id] = { min, max, home };
  });
  return out;
}

export function clampToLane(x: number, lane: Lane): number {
  return Math.min(lane.max, Math.max(lane.min, x));
}

export interface Motion {
  /** Center x the element is (or is heading) at. While walking this is the target. */
  x: number;
  fromX: number;
  startedAt: number;
  durationMs: number;
  walking: boolean;
  facing: "left" | "right";
}

export function restingMotion(x: number, facing: "left" | "right" = "right"): Motion {
  return { x, fromX: x, startedAt: 0, durationMs: 0, walking: false, facing };
}

/** Where a walking pet is right now (linear interpolation of its CSS transition). */
export function currentX(m: Motion, now: number): number {
  if (!m.walking || m.durationMs <= 0) return m.x;
  const t = Math.min(1, Math.max(0, (now - m.startedAt) / m.durationMs));
  return m.fromX + (m.x - m.fromX) * t;
}

/** Stop a walk at the current position. */
export function haltMotion(m: Motion, now: number): Motion {
  if (!m.walking) return m;
  const x = currentX(m, now);
  return { ...m, x, fromX: x, walking: false, durationMs: 0 };
}

/**
 * Plan a stroll inside the lane, or null to stay put. `rand` in [0,1).
 * Speed is in px/s.
 */
export function planWander(m: Motion, lane: Lane, now: number, speed: number, rand: () => number, minStep: number): Motion | null {
  if (lane.max - lane.min < minStep) return null;
  if (rand() < 0.4) return null; // sometimes just sit there
  const from = clampToLane(m.x, lane);
  let target = lane.min + rand() * (lane.max - lane.min);
  if (Math.abs(target - from) < minStep) {
    // nudge to a meaningful distance in whichever direction has room
    target = from + (from - lane.min > lane.max - from ? -minStep : minStep);
    target = clampToLane(target, lane);
  }
  const dist = Math.abs(target - from);
  if (dist < 1) return null;
  return {
    x: target,
    fromX: from,
    startedAt: now,
    durationMs: Math.round((dist / Math.max(1, speed)) * 1000),
    walking: true,
    facing: target > from ? "right" : "left",
  };
}

/** Center the bubble over the pet but keep it inside the strip; returns center x and tail offset. */
export function bubblePlacement(petX: number, width: number, bubbleMax: number): { center: number; tail: number } {
  const half = bubbleMax / 2;
  const lo = half + 4;
  const hi = width - half - 4;
  const center = hi < lo ? width / 2 : Math.min(hi, Math.max(lo, petX));
  const maxTail = Math.max(0, half - 18);
  const tail = Math.max(-maxTail, Math.min(maxTail, petX - center));
  return { center, tail };
}
