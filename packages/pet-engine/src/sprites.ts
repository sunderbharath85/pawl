// Sprite-sheet manifest (assets/pets/manifest.json, written by art-sprites). Browser-safe; no DOM required.
import type { Species } from "@krypto/protocol";
import type { PetActivity } from "./activity";

export type SpriteAnimationName = PetActivity | "walk";

export const SPRITE_ANIMATION_NAMES: readonly SpriteAnimationName[] = [
  "napping",
  "idle",
  "thinking",
  "working",
  "planning",
  "alert",
  "happy",
  "sad",
  "walk",
];

export interface SpriteAnimation {
  /** Path relative to the manifest's base URL, e.g. "dog/idle.png". Horizontal strip, frames facing right. */
  src: string;
  frames: number;
  fps: number;
  loop: boolean;
}

export interface SpriteSpecies {
  skins: string[];
  animations: Partial<Record<SpriteAnimationName, SpriteAnimation>>;
}

export interface SpriteManifest {
  version: 1;
  /** Square frame edge in px inside the PNG strips. */
  frameSize: number;
  species: Partial<Record<Species, SpriteSpecies>>;
}

/** What `PetSprite` takes: a manifest plus the URL it was loaded from (`/pets`, a webview URI, …). */
export interface PetSpritesConfig {
  manifest: SpriteManifest;
  baseUrl: string;
}

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function parseAnimation(v: unknown): SpriteAnimation | null {
  if (!isObj(v)) return null;
  const src = v["src"], frames = v["frames"], fps = v["fps"], loop = v["loop"];
  if (typeof src !== "string" || !src.trim()) return null;
  if (typeof frames !== "number" || !Number.isInteger(frames) || frames < 1 || frames > 512) return null;
  if (typeof fps !== "number" || !(fps > 0) || fps > 120) return null;
  return { src: src.trim(), frames, fps, loop: typeof loop === "boolean" ? loop : true };
}

/**
 * Validate untrusted manifest JSON. Invalid animations/species are dropped; returns null if the
 * document itself is unusable (wrong version, bad frameSize, no species object).
 */
export function parseSpriteManifest(json: unknown): SpriteManifest | null {
  if (!isObj(json)) return null;
  if (json["version"] !== 1) return null;
  const frameSize = json["frameSize"];
  if (typeof frameSize !== "number" || !(frameSize > 0) || frameSize > 4096) return null;
  if (!isObj(json["species"])) return null;
  const species: Partial<Record<Species, SpriteSpecies>> = {};
  for (const [name, raw] of Object.entries(json["species"])) {
    if (!isObj(raw) || !isObj(raw["animations"])) continue;
    const animations: Partial<Record<SpriteAnimationName, SpriteAnimation>> = {};
    for (const [anim, rawAnim] of Object.entries(raw["animations"])) {
      if (!(SPRITE_ANIMATION_NAMES as readonly string[]).includes(anim)) continue;
      const parsed = parseAnimation(rawAnim);
      if (parsed) animations[anim as SpriteAnimationName] = parsed;
    }
    const skins = Array.isArray(raw["skins"]) ? raw["skins"].filter((s): s is string => typeof s === "string") : [];
    species[name as Species] = { skins: skins.length ? skins : ["default"], animations };
  }
  return { version: 1, frameSize, species };
}

function trimSlash(url: string): string {
  return url.replace(/\/+$/, "");
}

/** Join a manifest base URL and an animation `src` (absolute `src` URLs are kept as-is). */
export function spriteUrl(baseUrl: string, src: string): string {
  if (/^[a-z][a-z0-9+.-]*:/i.test(src) || src.startsWith("/")) return src;
  return `${trimSlash(baseUrl)}/${src.replace(/^\.?\/+/, "")}`;
}

/**
 * Pick the animation to show. Walking prefers "walk" and falls back to the activity strip.
 * Returns null when the species/activity has no strip (→ render the built-in SVG).
 */
export function resolveSpriteAnimation(
  manifest: SpriteManifest | null | undefined,
  species: Species,
  activity: PetActivity,
  walking = false,
): SpriteAnimation | null {
  const sp = manifest?.species?.[species];
  if (!sp) return null;
  if (walking && sp.animations.walk) return sp.animations.walk;
  return sp.animations[activity] ?? null;
}

const manifestCache = new Map<string, Promise<SpriteManifest | null>>();

/**
 * Fetch and validate `<baseUrl>/manifest.json`. Resolves null (never rejects) if it is missing or
 * invalid. Successful loads are cached per baseUrl; failures are retried on the next call.
 */
export function loadSpriteManifest(baseUrl: string): Promise<SpriteManifest | null> {
  const key = trimSlash(baseUrl);
  const cached = manifestCache.get(key);
  if (cached) return cached;
  const p = (async (): Promise<SpriteManifest | null> => {
    if (typeof fetch !== "function") return null;
    try {
      const res = await fetch(`${key}/manifest.json`);
      if (!res.ok) return null;
      return parseSpriteManifest(await res.json());
    } catch {
      return null;
    }
  })();
  manifestCache.set(key, p);
  void p.then((m) => {
    if (!m) manifestCache.delete(key);
  });
  return p;
}

/** Test/HMR helper: forget cached manifests. */
export function clearSpriteManifestCache(): void {
  manifestCache.clear();
}
