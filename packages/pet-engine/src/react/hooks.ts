import { useEffect, useInsertionEffect, useMemo, useState } from "react";
import type { HubEvent, Pet, PermissionRequest, Session } from "@ai-pet/protocol";
import { lastSignificantEvents } from "../activity";
import { derivePetViews, nextPetViewChange, type PetView } from "../views";
import { ensurePetStyles } from "./styles";

/** Inject the pet stylesheet before first paint (once per document). */
export function usePetStyles(): void {
  useInsertionEffect(() => {
    ensurePetStyles();
  }, []);
}

const REDUCED_QUERY = "(prefers-reduced-motion: reduce)";

/** Live `prefers-reduced-motion: reduce` (false without matchMedia, e.g. SSR/tests). */
export function usePrefersReducedMotion(): boolean {
  const [reduced, setReduced] = useState<boolean>(() =>
    typeof window !== "undefined" && typeof window.matchMedia === "function" ? window.matchMedia(REDUCED_QUERY).matches : false,
  );
  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
    const mql = window.matchMedia(REDUCED_QUERY);
    const onChange = () => setReduced(mql.matches);
    onChange();
    if (typeof mql.addEventListener === "function") {
      mql.addEventListener("change", onChange);
      return () => mql.removeEventListener("change", onChange);
    }
    mql.addListener(onChange);
    return () => mql.removeListener(onChange);
  }, []);
  return reduced;
}

type Coll<T> = readonly T[] | Readonly<Record<string, T>>;

function toArray<T>(c: Coll<T> | undefined): T[] {
  if (!c) return [];
  return Array.isArray(c) ? (c as T[]) : Object.values(c as Record<string, T>);
}

/** Anything shaped like hub-client's `HubState` (records or arrays both work). */
export interface PetViewsSource {
  pets: Coll<Pet>;
  sessions: Coll<Session>;
  pending: Coll<PermissionRequest>;
  /** Per-session event lists (newest last). */
  events?: Readonly<Record<string, readonly HubEvent[] | undefined>> | undefined;
  /** Or: latest event per session, if you track it yourself. Takes precedence over `events`. */
  lastEvents?: Readonly<Record<string, HubEvent | undefined>> | undefined;
}

/**
 * Derive pet views from hub state and re-derive exactly when a decay/nap timer fires
 * (happy → idle after 6 s, sad → idle after 10 s, idle → napping after 60 s).
 */
export function usePetViews(source: PetViewsSource): PetView[] {
  const { pets, sessions, pending, events, lastEvents } = source;
  const [tick, setTick] = useState(0);
  const input = useMemo(
    () => ({
      pets: toArray(pets),
      sessions: toArray(sessions),
      pending: toArray(pending),
      lastEvents: (lastEvents as Record<string, HubEvent | undefined> | undefined) ?? lastSignificantEvents(events ?? {}),
    }),
    [pets, sessions, pending, events, lastEvents],
  );
  const { views, nextAt } = useMemo(() => {
    const now = Date.now();
    const full = { ...input, now };
    return { views: derivePetViews(full), nextAt: nextPetViewChange(full) };
    // `tick` forces a re-derive when a timer fires.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [input, tick]);
  useEffect(() => {
    if (nextAt == null) return;
    const delay = Math.max(0, nextAt - Date.now()) + 20;
    const t = setTimeout(() => setTick((n) => n + 1), Math.min(delay, 2_147_000_000));
    return () => clearTimeout(t);
  }, [nextAt]);
  return views;
}

type ImageStatus = "loading" | "ok" | "error";
const imageCache = new Map<string, Exclude<ImageStatus, "loading">>();

/** Preload an image URL; "error" for null/failed URLs. Results are cached per URL for the session. */
export function useImageStatus(url: string | null): ImageStatus {
  const [state, setState] = useState<{ url: string | null; status: ImageStatus }>(() => ({
    url,
    status: url ? (imageCache.get(url) ?? "loading") : "error",
  }));
  useEffect(() => {
    if (!url) {
      setState({ url, status: "error" });
      return;
    }
    const known = imageCache.get(url);
    if (known) {
      setState({ url, status: known });
      return;
    }
    if (typeof Image === "undefined") {
      setState({ url, status: "error" });
      return;
    }
    let alive = true;
    setState({ url, status: "loading" });
    const img = new Image();
    img.onload = () => {
      imageCache.set(url, "ok");
      if (alive) setState({ url, status: "ok" });
    };
    img.onerror = () => {
      imageCache.set(url, "error");
      if (alive) setState({ url, status: "error" });
    };
    img.src = url;
    return () => {
      alive = false;
    };
  }, [url]);
  if (!url) return "error";
  if (state.url === url) return state.status;
  return imageCache.get(url) ?? "loading";
}
