import { afterEach, describe, expect, test } from "bun:test";
import { clearSpriteManifestCache, loadSpriteManifest, parseSpriteManifest, resolveSpriteAnimation, spriteUrl } from "../src/index";

const sample = {
  version: 1,
  frameSize: 256,
  species: {
    dog: {
      skins: ["default"],
      animations: {
        idle: { src: "dog/idle.png", frames: 4, fps: 6, loop: true },
        alert: { src: "dog/alert.png", frames: 4, fps: 10, loop: true },
        walk: { src: "dog/walk.png", frames: 6, fps: 12 },
        bogus: { src: "dog/x.png", frames: 4, fps: 6, loop: true },
        sad: { src: "", frames: 4, fps: 6, loop: true },
        happy: { src: "dog/happy.png", frames: 0, fps: 6, loop: true },
      },
    },
    cat: "nope",
  },
};

describe("parseSpriteManifest", () => {
  test("keeps valid animations, drops invalid ones", () => {
    const m = parseSpriteManifest(sample)!;
    expect(m.frameSize).toBe(256);
    expect(Object.keys(m.species)).toEqual(["dog"]);
    expect(Object.keys(m.species.dog!.animations).sort()).toEqual(["alert", "idle", "walk"]);
    expect(m.species.dog!.animations.walk!.loop).toBe(true);
  });
  test("rejects unusable documents", () => {
    expect(parseSpriteManifest(null)).toBeNull();
    expect(parseSpriteManifest({ ...sample, version: 2 })).toBeNull();
    expect(parseSpriteManifest({ ...sample, frameSize: -1 })).toBeNull();
    expect(parseSpriteManifest({ version: 1, frameSize: 64 })).toBeNull();
  });
});

describe("resolveSpriteAnimation", () => {
  const m = parseSpriteManifest(sample)!;
  test("activity strip, walk preference, fallbacks", () => {
    expect(resolveSpriteAnimation(m, "dog", "idle")!.src).toBe("dog/idle.png");
    expect(resolveSpriteAnimation(m, "dog", "idle", true)!.src).toBe("dog/walk.png");
    expect(resolveSpriteAnimation(m, "dog", "thinking")).toBeNull();
    expect(resolveSpriteAnimation(m, "cat", "idle")).toBeNull();
    expect(resolveSpriteAnimation(null, "dog", "idle")).toBeNull();
  });
});

test("spriteUrl joins paths", () => {
  expect(spriteUrl("/pets/", "dog/idle.png")).toBe("/pets/dog/idle.png");
  expect(spriteUrl("https://x.test/media/pets", "./dog/idle.png")).toBe("https://x.test/media/pets/dog/idle.png");
  expect(spriteUrl("/pets", "https://cdn/x.png")).toBe("https://cdn/x.png");
});

describe("loadSpriteManifest", () => {
  const realFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = realFetch;
    clearSpriteManifestCache();
  });
  test("fetches, validates and caches", async () => {
    let calls = 0;
    globalThis.fetch = (async (url: string) => {
      calls++;
      expect(url).toBe("/pets/manifest.json");
      return new Response(JSON.stringify(sample), { status: 200 });
    }) as unknown as typeof fetch;
    const m = await loadSpriteManifest("/pets/");
    expect(m?.species.dog?.animations.idle?.frames).toBe(4);
    await loadSpriteManifest("/pets");
    expect(calls).toBe(1);
  });
  test("resolves null on 404 / network error / bad json, and retries later", async () => {
    let calls = 0;
    globalThis.fetch = (async () => {
      calls++;
      if (calls === 1) return new Response("nope", { status: 404 });
      if (calls === 2) throw new Error("offline");
      return new Response("{not json", { status: 200 });
    }) as unknown as typeof fetch;
    expect(await loadSpriteManifest("/a")).toBeNull();
    expect(await loadSpriteManifest("/a")).toBeNull();
    expect(await loadSpriteManifest("/a")).toBeNull();
    expect(calls).toBe(3);
  });
});
