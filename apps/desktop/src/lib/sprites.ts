import { useEffect, useState } from "react";
import { loadSpriteManifest, type PetSpritesConfig } from "@krypto/pet-engine";

/** Sprite sheets are served at /pets/ (see vite.config.ts). */
export const PETS_BASE_URL = "/pets/";

let cached: Promise<PetSpritesConfig | null> | null = null;

function loadOnce(): Promise<PetSpritesConfig | null> {
  if (!cached) {
    cached = loadSpriteManifest(PETS_BASE_URL)
      .then((manifest) => (manifest ? { manifest, baseUrl: PETS_BASE_URL } : null))
      .catch((err) => {
        console.warn("[desktop] sprite manifest unavailable, using built-in pets", err);
        return null;
      });
  }
  return cached;
}

/** `null` until/unless assets/pets/manifest.json exists; the engine then falls back to SVG pets. */
export function useSprites(): PetSpritesConfig | null {
  const [sprites, setSprites] = useState<PetSpritesConfig | null>(null);
  useEffect(() => {
    let alive = true;
    void loadOnce().then((s) => {
      if (alive) setSprites(s);
    });
    return () => {
      alive = false;
    };
  }, []);
  return sprites;
}
