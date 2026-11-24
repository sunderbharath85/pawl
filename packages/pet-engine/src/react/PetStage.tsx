import { Fragment, useCallback, useEffect, useMemo, useReducer, useRef, type CSSProperties, type JSX, type KeyboardEvent } from "react";
import { SPECIES_META } from "../lines";
import type { PetSpritesConfig } from "../sprites";
import type { PetView } from "../views";
import { PetBubble } from "./PetBubble";
import { PetSprite } from "./PetSprite";
import { usePetStyles, usePrefersReducedMotion } from "./hooks";
import {
  bubblePlacement,
  clampToLane,
  computeLanes,
  haltMotion,
  layoutVisitors,
  MAX_VISITORS,
  offscreenX,
  planWander,
  restingMotion,
  VISITOR_SCALE,
  walkMotion,
  type Lane,
  type Motion,
} from "./stage-logic";

export interface PetStageProps {
  views: PetView[];
  /** Strip width in px. */
  width: number;
  /** `confirmed` = the user passed the bubble's confirm step (another machine's request, `bubble.confirm`). */
  onApprove(requestId: string, opts: { confirmed: boolean }): void | Promise<unknown>;
  onDeny(requestId: string): void | Promise<unknown>;
  onPetClick?: ((petId: string) => void) | undefined;
  /** Pet box size in px. Default 96. Visitors are drawn at VISITOR_SCALE of it. */
  petSize?: number | undefined;
  /** Total height in px. Default petSize + room for a bubble (+ names). */
  height?: number | undefined;
  /** Sprite sheets (else built-in SVG). */
  sprites?: PetSpritesConfig | null | undefined;
  /** Let idle pets stroll around their lane. Default true (always off with prefers-reduced-motion). */
  wander?: boolean | undefined;
  /** Walk speed in px/s. Default petSize × 0.45. */
  walkSpeed?: number | undefined;
  /** Show a name tag under each pet. Default true (every pet has a name). */
  showNames?: boolean | undefined;
  /** Max bubble width in px. Default 240 (clamped to the strip). */
  bubbleMaxWidth?: number | undefined;
  /** Visitors shown at once in the visitors area; the rest become a "+N" chip. Default 6. */
  maxVisitors?: number | undefined;
  className?: string | undefined;
  style?: CSSProperties | undefined;
}

const BUBBLE_ROOM = 88;
const NAME_ROOM = 18;
/** Running (away, back home, or in as a visitor) is this much faster than strolling. */
const RUN_FACTOR = 2.5;
/** Delay between parking a visitor at the edge and starting its run, so the browser paints the start. */
const RUN_IN_DELAY_MS = 30;

function sameMotions(a: Record<string, Motion>, b: Record<string, Motion>): boolean {
  const ka = Object.keys(a);
  if (ka.length !== Object.keys(b).length) return false;
  return ka.every((k) => a[k] === b[k]);
}

/** Visitors with a request that ran here come first (nearest the edge, never hidden), then input order. */
function visitorOrder(views: readonly PetView[]): PetView[] {
  return views
    .map((v, i) => ({ v, i }))
    .filter(({ v }) => v.visitor)
    .sort((a, b) => Number(b.v.bubble?.kind === "permission") - Number(a.v.bubble?.kind === "permission") || a.i - b.i)
    .map(({ v }) => v);
}

function petLabel(view: PetView): string {
  const species = SPECIES_META[view.species]?.label.toLowerCase() ?? "pet";
  if (view.visitor) return `${view.name} the ${species} from ${view.machineName ?? "another machine"} is ${view.activity}`;
  if (view.away) return `${view.name} the ${species} ran to another machine`;
  return `${view.name} the ${species} is ${view.activity}`;
}

/**
 * Horizontal strip of pets (e.g. along the taskbar). Home pets own equal lanes (idle ones wander inside
 * theirs); visitors (pets of other online machines) sit in a visitors area at the right edge, at most
 * `maxVisitors`, then "+N". A home pet that is `away` runs off the left edge and comes back when it is
 * not; a visitor that brings a request runs in from the right edge. Bubbles float above their pet.
 * Pets of offline machines (`remoteOffline`) are not drawn.
 */
export function PetStage(props: PetStageProps): JSX.Element {
  const {
    views,
    width,
    onApprove,
    onDeny,
    onPetClick,
    petSize = 96,
    height,
    sprites,
    wander = true,
    walkSpeed,
    showNames = true,
    bubbleMaxWidth = 240,
    maxVisitors = MAX_VISITORS,
    className,
    style,
  } = props;
  usePetStyles();
  const reduced = usePrefersReducedMotion();
  const canWander = wander && !reduced;
  const animate = !reduced;
  const speed = walkSpeed ?? petSize * 0.45;
  const runSpeed = speed * RUN_FACTOR;
  const visitorSize = Math.round(petSize * VISITOR_SCALE);

  const home = useMemo(() => views.filter((v) => !v.visitor && !v.remoteOffline), [views]);
  const visitors = useMemo(() => visitorOrder(views), [views]);

  const visitorKey = JSON.stringify(visitors.map((v) => v.petId));
  const layout = useMemo(
    () => layoutVisitors(JSON.parse(visitorKey) as string[], width, visitorSize, maxVisitors),
    [visitorKey, width, visitorSize, maxVisitors],
  );
  const homeWidth = Math.max(Math.min(width, petSize), width - layout.areaWidth);
  const homeKey = JSON.stringify(home.map((v) => v.petId));
  const lanes = useMemo(() => computeLanes(JSON.parse(homeKey) as string[], homeWidth, petSize), [homeKey, homeWidth, petSize]);
  const offLeft = offscreenX("left", width, petSize);
  const offRight = offscreenX("right", width, visitorSize);

  // Motion is the source of truth in a ref (timers read it synchronously); a counter re-renders.
  const motionsRef = useRef<Record<string, Motion>>({});
  /** "petId|requestId" of visitors that already ran in with that request. */
  const arrivedRef = useRef<Set<string>>(new Set());
  const [, rerender] = useReducer((n: number) => n + 1, 0);
  const commit = useCallback((next: Record<string, Motion>) => {
    if (next === motionsRef.current) return;
    motionsRef.current = next;
    rerender();
  }, []);

  // Which home pets may stroll right now (idle and at home), which ran away, which visitors brought a request.
  const idleKey = JSON.stringify(home.filter((v) => v.activity === "idle" && !v.away).map((v) => v.petId));
  const awayKey = JSON.stringify(home.filter((v) => v.away).map((v) => v.petId));
  const ranKey = JSON.stringify(visitors.filter((v) => v.bubble?.kind === "permission").map((v) => [v.petId, v.bubble?.requestId ?? ""]));

  // Reconcile: place new pets, drop removed ones, run away / back / in, clamp to lanes, halt non-idle strollers.
  useEffect(() => {
    const idle = new Set(JSON.parse(idleKey) as string[]);
    const away = new Set(JSON.parse(awayKey) as string[]);
    const ran = new Map((JSON.parse(ranKey) as Array<[string, string]>).map(([id, req]) => [id, `${id}|${req}`]));
    const prev = motionsRef.current;
    const now = Date.now();
    const next: Record<string, Motion> = {};

    for (const [id, lane] of Object.entries(lanes)) {
      let m = prev[id];
      if (away.has(id)) {
        // Ran to the machine where the user is: off the left edge (instantly when first seen or reduced motion).
        if (!m || !animate) m = restingMotion(offLeft, "left");
        else if (m.x !== offLeft) m = walkMotion(m, offLeft, now, runSpeed);
      } else if (!m) {
        m = restingMotion(lane.home);
      } else if (m.x <= offLeft + 0.5) {
        // Back from another machine.
        m = animate ? walkMotion(m, lane.home, now, runSpeed / 1.5) : restingMotion(lane.home);
      } else {
        if (m.walking && !m.travel && (!canWander || !idle.has(id) || m.x < lane.min || m.x > lane.max)) m = haltMotion(m, now);
        if (!m.walking) {
          const x = clampToLane(m.x, lane);
          if (x !== m.x) m = { ...m, x, fromX: x };
        }
      }
      next[id] = m;
    }

    for (const [id, x] of Object.entries(layout.slots)) {
      const tag = ran.get(id);
      let m = prev[id];
      if (tag !== undefined && !arrivedRef.current.has(tag)) {
        // It just ran over from its machine with a request: start at the right edge.
        arrivedRef.current.add(tag);
        if (animate) {
          next[id] = restingMotion(offRight, "left");
          continue;
        }
      }
      const runInPending = !!m && m.x === offRight && !m.walking; // started below
      if (!m) m = restingMotion(x, "left");
      else if (!runInPending && m.x !== x) m = animate ? walkMotion(m, x, now, m.walking ? runSpeed : speed) : restingMotion(x, "left");
      next[id] = m;
    }
    // Forget requests that are gone, so a request that comes back runs in again.
    const current = new Set(ran.values());
    for (const tag of [...arrivedRef.current]) if (!current.has(tag)) arrivedRef.current.delete(tag);

    commit(sameMotions(prev, next) ? prev : next);

    const parked = Object.entries(next).filter(([id, m]) => layout.slots[id] !== undefined && m.x === offRight && !m.walking);
    if (parked.length === 0) return;
    const t = setTimeout(() => {
      const cur = { ...motionsRef.current };
      const at = Date.now();
      for (const [id] of parked) {
        const slot = layout.slots[id];
        const m = cur[id];
        if (slot !== undefined && m) cur[id] = walkMotion(m, slot, at, runSpeed);
      }
      commit(cur);
    }, RUN_IN_DELAY_MS);
    return () => clearTimeout(t);
  }, [lanes, layout, idleKey, awayKey, ranKey, canWander, animate, offLeft, offRight, speed, runSpeed, commit]);

  // Finish purposeful walks (the wander scheduler finishes its own strolls).
  const travelKey = JSON.stringify(
    Object.entries(motionsRef.current)
      .filter(([, m]) => m.walking && m.travel)
      .map(([id, m]) => [id, m.startedAt, m.durationMs]),
  );
  useEffect(() => {
    const list = JSON.parse(travelKey) as Array<[string, number, number]>;
    if (list.length === 0) return;
    const timers = list.map(([id, startedAt, durationMs]) =>
      setTimeout(
        () => {
          const cur = motionsRef.current[id];
          if (cur && cur.walking && cur.startedAt === startedAt) {
            commit({ ...motionsRef.current, [id]: { ...cur, fromX: cur.x, walking: false, durationMs: 0, travel: false } });
          }
        },
        Math.max(0, startedAt + durationMs - Date.now()),
      ),
    );
    return () => timers.forEach(clearTimeout);
  }, [travelKey, commit]);

  // Wander scheduler for idle home pets.
  useEffect(() => {
    const idleIds = JSON.parse(idleKey) as string[];
    if (!canWander || idleIds.length === 0) return;
    const timers = new Set<ReturnType<typeof setTimeout>>();
    const later = (fn: () => void, ms: number) => {
      const t = setTimeout(() => {
        timers.delete(t);
        fn();
      }, ms);
      timers.add(t);
    };
    const minStep = Math.max(12, petSize * 0.3);
    const rest = () => 2500 + Math.random() * 5000;
    const decide = (id: string) => {
      const lane: Lane | undefined = lanes[id];
      const m = motionsRef.current[id];
      if (!lane || !m) return;
      if (m.walking || m.x < lane.min - 1 || m.x > lane.max + 1) {
        later(() => decide(id), rest());
        return;
      }
      const planned = planWander(m, lane, Date.now(), speed, Math.random, minStep);
      if (!planned) {
        later(() => decide(id), rest());
        return;
      }
      commit({ ...motionsRef.current, [id]: planned });
      later(() => {
        const cur = motionsRef.current[id];
        if (cur && cur.walking && cur.startedAt === planned.startedAt) {
          commit({ ...motionsRef.current, [id]: { ...cur, fromX: cur.x, walking: false, durationMs: 0 } });
        }
        later(() => decide(id), rest());
      }, planned.durationMs);
    };
    for (const id of idleIds) later(() => decide(id), 1200 + Math.random() * 4000);
    return () => {
      for (const t of timers) clearTimeout(t);
      timers.clear();
    };
  }, [idleKey, lanes, canWander, speed, petSize, commit]);

  const motions = motionsRef.current;
  const bubbleMax = Math.max(80, Math.min(bubbleMaxWidth, width - 8));
  const stageHeight = height ?? petSize + BUBBLE_ROOM + (showNames ? NAME_ROOM : 0);
  const petBottom = showNames ? NAME_ROOM : 0;

  const renderPet = (view: PetView, size: number, fallbackX: number) => {
    const m = motions[view.petId] ?? restingMotion(fallbackX, view.visitor ? "left" : "right");
    const gone = !!view.away && !m.walking && m.x <= offLeft + 0.5;
    const left = m.x - size / 2;
    const petStyle: CSSProperties = {
      width: size,
      height: size,
      bottom: petBottom,
      transform: `translateX(${left.toFixed(1)}px)`,
      transition: m.walking ? `transform ${m.durationMs}ms linear` : "none",
      ...(gone ? { visibility: "hidden" } : null),
    };
    const label = petLabel(view);
    const clickable = Boolean(onPetClick) && !gone;
    const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
      if (!onPetClick || e.target !== e.currentTarget) return;
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        onPetClick(view.petId);
      }
    };
    // Away pets carry no bubble here (it shows on the machine they ran to); visitors speak once they arrived.
    const bubble = view.bubble && !view.away && !(view.visitor && m.walking) ? view.bubble : undefined;
    const { center, tail } = bubblePlacement(m.x, width, bubbleMax);
    const requestId = bubble?.kind === "permission" && !bubble.readOnly ? bubble.requestId : undefined;
    const nameTag = view.visitor && view.machineName ? `${view.name} · ${view.machineName}` : view.name;
    return (
      <Fragment key={view.petId}>
        <div
          className={["ap-stage-pet", view.visitor ? "ap-stage-visitor" : ""].filter(Boolean).join(" ")}
          style={petStyle}
          role={clickable ? "button" : undefined}
          tabIndex={clickable ? 0 : undefined}
          aria-label={label}
          aria-hidden={gone ? true : undefined}
          title={label}
          data-pet-id={view.petId}
          data-pet-hit={gone ? undefined : "pet"}
          onClick={clickable ? () => onPetClick!(view.petId) : undefined}
          onKeyDown={clickable ? onKey : undefined}
        >
          <PetSprite
            species={view.species}
            activity={view.activity}
            size={size}
            facing={m.facing}
            walking={m.walking}
            skin={view.skin}
            sprites={sprites}
            title={label}
          />
          {showNames ? <div className="ap-stage-name">{nameTag}</div> : null}
        </div>
        {bubble ? (
          <div
            className="ap-stage-bubble"
            style={
              {
                left: center,
                bottom: petBottom + size + 8,
                "--ap-bubble-max": `${bubbleMax}px`,
              } as CSSProperties
            }
          >
            <div style={{ "--ap-tail-x": `${tail.toFixed(1)}px` } as CSSProperties}>
              <PetBubble
                view={{ ...view, bubble }}
                onApprove={requestId ? (opts) => onApprove(requestId, opts) : undefined}
                onDeny={requestId ? () => onDeny(requestId) : undefined}
                onClick={onPetClick ? () => onPetClick(view.petId) : undefined}
              />
            </div>
          </div>
        ) : null}
      </Fragment>
    );
  };

  const hiddenNames = visitors
    .filter((v) => layout.hidden.includes(v.petId))
    .map((v) => (v.machineName ? `${v.name} · ${v.machineName}` : v.name));

  return (
    <div
      className={["ap-stage", reduced ? "ap-reduced-motion" : "", className ?? ""].filter(Boolean).join(" ")}
      style={{ width, height: stageHeight, ...style }}
    >
      {home.map((view) => renderPet(view, petSize, view.away ? offLeft : (lanes[view.petId]?.home ?? width / 2)))}
      {visitors.map((view) => {
        const slot = layout.slots[view.petId];
        return slot === undefined ? null : renderPet(view, visitorSize, slot);
      })}
      {layout.moreX !== null ? (
        <div
          className="ap-stage-more"
          style={{ left: layout.moreX, bottom: petBottom + visitorSize / 2 - 11 }}
          title={hiddenNames.join("\n")}
          aria-label={`${layout.hidden.length} more visiting pets`}
          data-pet-hit="more"
        >
          +{layout.hidden.length}
        </div>
      ) : null}
    </div>
  );
}
