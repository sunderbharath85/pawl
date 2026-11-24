// Reports where the pets/bubbles are so Rust can turn click-through off only
// while the cursor is over one. Works without knowing PetStage's DOM: every
// visible element narrower than half the strip counts (full-width tracks and
// wrappers are skipped); explicit `[data-pet-hit]` always counts.
import { native, type HitRect } from "../lib/native";

const MAX_RECTS = 96;

function visible(el: Element): boolean {
  const cs = getComputedStyle(el);
  return cs.visibility !== "hidden" && cs.display !== "none" && cs.pointerEvents !== "none" && Number(cs.opacity) > 0.05;
}

export function collectHitRects(root: Element): HitRect[] {
  const maxW = window.innerWidth * 0.5;
  const out: HitRect[] = [];
  const visit = (el: Element) => {
    for (const child of Array.from(el.children)) {
      if (out.length >= MAX_RECTS) return;
      const style = getComputedStyle(child);
      if (style.display === "none") continue;
      const r = child.getBoundingClientRect();
      const marked = child.hasAttribute("data-pet-hit");
      const small = r.width > 0 && r.height > 0 && r.width < maxW;
      if ((marked || small) && visible(child)) {
        out.push({ x: r.left, y: r.top, w: r.width, h: r.height });
      }
      // SVG internals are covered by the <svg> box; skip them.
      if (child instanceof SVGElement || child.hasAttribute("data-pet-hit-leaf")) continue;
      visit(child);
    }
  };
  visit(root);
  return dedupe(out);
}

function contains(a: HitRect, b: HitRect): boolean {
  return b.x >= a.x - 0.5 && b.y >= a.y - 0.5 && b.x + b.w <= a.x + a.w + 0.5 && b.y + b.h <= a.y + a.h + 0.5;
}

function dedupe(rects: HitRect[]): HitRect[] {
  const sorted = [...rects].sort((a, b) => b.w * b.h - a.w * a.h);
  const kept: HitRect[] = [];
  for (const r of sorted) if (!kept.some((k) => contains(k, r))) kept.push(r);
  return kept.map((r) => ({ x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.w), h: Math.round(r.h) }));
}

/** Poll the DOM (~12 Hz) and push changes to Rust. Returns a stop function. */
export function startHitRectReporter(getRoot: () => Element | null, everyMs = 80): () => void {
  let last = "";
  let inFlight = false;
  const tick = () => {
    const root = getRoot();
    if (!root || inFlight) return;
    const rects = collectHitRects(root);
    const key = JSON.stringify(rects);
    if (key === last) return;
    last = key;
    inFlight = true;
    native
      .setHitRects(rects)
      .catch((err) => {
        last = "";
        console.warn("[desktop] set_hit_rects failed", err);
      })
      .finally(() => {
        inFlight = false;
      });
  };
  const timer = setInterval(tick, everyMs);
  tick();
  return () => {
    clearInterval(timer);
    void native.setHitRects([]).catch(() => {});
  };
}
