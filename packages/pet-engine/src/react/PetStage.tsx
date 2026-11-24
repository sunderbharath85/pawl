import { Fragment, useCallback, useEffect, useMemo, useReducer, useRef, type CSSProperties, type JSX, type KeyboardEvent } from "react";
import { SPECIES_META } from "../lines";
import type { PetSpritesConfig } from "../sprites";
import type { PetView } from "../views";
import { PetBubble } from "./PetBubble";
import { PetSprite } from "./PetSprite";
import { usePetStyles, usePrefersReducedMotion } from "./hooks";
import { bubblePlacement, clampToLane, computeLanes, haltMotion, planWander, restingMotion, type Lane, type Motion } from "./stage-logic";

export interface PetStageProps {
  views: PetView[];
  /** Strip width in px. */
  width: number;
  onApprove(requestId: string): void | Promise<unknown>;
  onDeny(requestId: string): void | Promise<unknown>;
  onPetClick?: ((petId: string) => void) | undefined;
  /** Pet box size in px. Default 96. */
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
  className?: string | undefined;
  style?: CSSProperties | undefined;
}

const BUBBLE_ROOM = 88;
const NAME_ROOM = 18;

function sameMotions(a: Record<string, Motion>, b: Record<string, Motion>): boolean {
  const ka = Object.keys(a);
  if (ka.length !== Object.keys(b).length) return false;
  return ka.every((k) => a[k] === b[k]);
}

/**
 * Horizontal strip of pets (e.g. along the taskbar). Each pet owns an equal lane; idle pets wander
 * inside it, others stay put. Bubbles float above their pet, clamped to the strip.
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
    className,
    style,
  } = props;
  usePetStyles();
  const reduced = usePrefersReducedMotion();
  const canWander = wander && !reduced;
  const speed = walkSpeed ?? petSize * 0.45;

  const idsKey = JSON.stringify(views.map((v) => v.petId));
  const lanes = useMemo(() => computeLanes(JSON.parse(idsKey) as string[], width, petSize), [idsKey, width, petSize]);
  // Motion is the source of truth in a ref (timers read it synchronously); a counter re-renders.
  const motionsRef = useRef<Record<string, Motion>>({});
  const [, rerender] = useReducer((n: number) => n + 1, 0);
  const commit = useCallback((next: Record<string, Motion>) => {
    if (next === motionsRef.current) return;
    motionsRef.current = next;
    rerender();
  }, []);

  // Which pets may stroll right now (idle only).
  const idleKey = JSON.stringify(views.filter((v) => v.activity === "idle").map((v) => v.petId));

  // Reconcile: add new pets at home, drop removed ones, clamp to lanes, halt non-idle walkers.
  useEffect(() => {
    const idle = new Set(JSON.parse(idleKey) as string[]);
    const prev = motionsRef.current;
    const now = Date.now();
    const next: Record<string, Motion> = {};
    for (const [id, lane] of Object.entries(lanes)) {
      let m = prev[id] ?? restingMotion(lane.home);
      if (m.walking && (!canWander || !idle.has(id) || m.x < lane.min || m.x > lane.max)) m = haltMotion(m, now);
      if (!m.walking) {
        const x = clampToLane(m.x, lane);
        if (x !== m.x) m = { ...m, x, fromX: x };
      }
      next[id] = m;
    }
    commit(sameMotions(prev, next) ? prev : next);
  }, [lanes, idleKey, canWander, commit]);

  // Wander scheduler for idle pets.
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
      if (m.walking) {
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

  return (
    <div
      className={["ap-stage", reduced ? "ap-reduced-motion" : "", className ?? ""].filter(Boolean).join(" ")}
      style={{ width, height: stageHeight, ...style }}
    >
      {views.map((view) => {
        const lane = lanes[view.petId];
        const m = motions[view.petId] ?? restingMotion(lane?.home ?? width / 2);
        const left = m.x - petSize / 2;
        const petStyle: CSSProperties = {
          width: petSize,
          height: petSize,
          bottom: petBottom,
          transform: `translateX(${left.toFixed(1)}px)`,
          transition: m.walking ? `transform ${m.durationMs}ms linear` : "none",
        };
        const label = `${view.name} the ${SPECIES_META[view.species]?.label.toLowerCase() ?? "pet"} is ${view.activity}`;
        const clickable = Boolean(onPetClick);
        const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
          if (!onPetClick || e.target !== e.currentTarget) return;
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onPetClick(view.petId);
          }
        };
        const { center, tail } = bubblePlacement(m.x, width, bubbleMax);
        const requestId = view.bubble?.requestId;
        return (
          <Fragment key={view.petId}>
            <div
              className="ap-stage-pet"
              style={petStyle}
              role={clickable ? "button" : undefined}
              tabIndex={clickable ? 0 : undefined}
              aria-label={label}
              title={label}
              data-pet-id={view.petId}
              data-pet-hit="pet"
              onClick={clickable ? () => onPetClick!(view.petId) : undefined}
              onKeyDown={clickable ? onKey : undefined}
            >
              <PetSprite
                species={view.species}
                activity={view.activity}
                size={petSize}
                facing={m.facing}
                walking={m.walking}
                skin={view.skin}
                sprites={sprites}
                title={label}
              />
              {showNames ? <div className="ap-stage-name">{view.name}</div> : null}
            </div>
            {view.bubble ? (
              <div
                className="ap-stage-bubble"
                style={
                  {
                    left: center,
                    bottom: petBottom + petSize + 8,
                    "--ap-bubble-max": `${bubbleMax}px`,
                  } as CSSProperties
                }
              >
                <div style={{ "--ap-tail-x": `${tail.toFixed(1)}px` } as CSSProperties}>
                  <PetBubble
                    view={view}
                    onApprove={requestId ? () => onApprove(requestId) : undefined}
                    onDeny={requestId ? () => onDeny(requestId) : undefined}
                    onClick={onPetClick ? () => onPetClick(view.petId) : undefined}
                  />
                </div>
              </div>
            ) : null}
          </Fragment>
        );
      })}
    </div>
  );
}
