#!/usr/bin/env bun
/**
 * Generate raw sprite art with Gemini (Nano Banana) into scripts/art/.cache/raw/.
 *
 *   bun run generate.ts [--species dog,cat] [--activities idle,walk] [--frames 2,3]
 *                       [--reference] [--regen] [--note "extra instruction"]
 *                       [--concurrency 4] [--dry-run]
 *
 * Per species: a character reference sheet is generated first (once). Then, per animation,
 * frame 1 is drawn as a single static sticker from the reference sheet, and frames 2..N are
 * produced as *edits of frame 1* (frame 1 + reference sheet as inputs). Editing keeps the
 * character, scale and canvas position identical across frames, which a single multi-pose
 * image does not (the model smears "animation" strips with motion blur and ghosting).
 *
 * Cached variants are reused. --regen makes new variants for the selected frames (default all);
 * edits whose frame-1 variant is no longer the selected one are regenerated automatically.
 * Run process.ts afterwards.
 */
import { parseArgs } from "node:util";
import type { Species } from "@ai-pet/protocol";
import {
  ACTIVITIES,
  FRAME_ASPECT,
  FRAME_GEN_SIZE,
  MODEL,
  REFERENCE_ASPECT,
  REFERENCE_SIZE,
  SPECIES,
  basePrompt,
  editPrompt,
  frameCount,
  loadOverrides,
  parseList,
  referencePrompt,
  type Activity,
  type ImageSize,
} from "./config.ts";
import { frameName, listVariants, readMeta, saveRaw, selectedRaw, type RawImage, type RawName } from "./cache.ts";
import { generateImage } from "./gemini.ts";

const { values } = parseArgs({
  args: Bun.argv.slice(2),
  options: {
    species: { type: "string" },
    activities: { type: "string" },
    frames: { type: "string" },
    reference: { type: "boolean", default: false },
    regen: { type: "boolean", default: false },
    note: { type: "string" },
    concurrency: { type: "string", default: "4" },
    "dry-run": { type: "boolean", default: false },
  },
  strict: true,
});

const speciesList = parseList<Species>(values.species, SPECIES, "species");
const activities = parseList<Activity>(values.activities, ACTIVITIES, "activity");
const frameFilter = values.frames ? new Set(values.frames.split(",").map((s) => Number(s.trim()))) : null;
if (frameFilter && [...frameFilter].some((n) => !Number.isInteger(n) || n < 1)) throw new Error(`bad --frames "${values.frames}"`);
const concurrency = Math.max(1, Number(values.concurrency) || 1);
const dryRun = values["dry-run"] ?? false;
const overrides = await loadOverrides();

// ---------------------------------------------------------------------------

let active = 0;
const waiters: (() => void)[] = [];
async function limited<T>(fn: () => Promise<T>): Promise<T> {
  while (active >= concurrency) await new Promise<void>((r) => waiters.push(r));
  active++;
  try {
    return await fn();
  } finally {
    active--;
    waiters.shift()?.();
  }
}

let calls = 0;
const failures: string[] = [];

async function generate(opts: {
  species: Species;
  name: RawName;
  prompt: string;
  refs: RawImage[];
  aspectRatio: string;
  imageSize: ImageSize;
  baseVariant?: number;
}): Promise<boolean> {
  const label = `${opts.species}/${opts.name}`;
  if (dryRun) {
    console.log(`\n--- [dry-run] ${label} (${opts.aspectRatio} ${opts.imageSize}, refs: ${opts.refs.map((r) => r.meta.file).join(", ") || "none"})\n${opts.prompt}`);
    return true;
  }
  try {
    await limited(async () => {
      const t0 = performance.now();
      console.log(`[gen] ${label} ...`);
      const res = await generateImage({
        prompt: opts.prompt,
        refs: opts.refs.map((r) => ({ data: r.data, mime: r.meta.mime })),
        aspectRatio: opts.aspectRatio,
        imageSize: opts.imageSize,
      });
      calls++;
      const meta = await saveRaw(
        {
          species: opts.species,
          name: opts.name,
          mime: res.mime,
          model: MODEL,
          prompt: opts.prompt,
          aspectRatio: opts.aspectRatio,
          imageSize: opts.imageSize,
          refs: opts.refs.map((r) => `${r.meta.species}/${r.meta.file}`),
          note: values.note,
          baseVariant: opts.baseVariant,
          text: res.text || undefined,
          interactionId: res.interactionId,
          usage: res.usage,
        },
        res.data,
      );
      console.log(`[ok ] ${label} -> .cache/raw/${opts.species}/${meta.file} (${((performance.now() - t0) / 1000).toFixed(1)}s)`);
    });
    return true;
  } catch (e) {
    failures.push(`${label}: ${(e as Error).message}`);
    console.error(`[err] ${label}: ${(e as Error).message}`);
    return false;
  }
}

const wants = (frame: number) => !frameFilter || frameFilter.has(frame);

async function animation(species: Species, activity: Activity, ref: RawImage | null): Promise<void> {
  const n = frameCount(species, activity);
  const f1 = frameName(activity, 1);
  const hasBase = (await listVariants(species, f1)).length > 0;
  if (!hasBase || (values.regen && wants(1))) {
    const ok = await generate({
      species,
      name: f1,
      prompt: basePrompt(species, activity, values.note),
      refs: ref ? [ref] : [],
      aspectRatio: FRAME_ASPECT,
      imageSize: FRAME_GEN_SIZE,
    });
    if (!ok) return;
  }
  // A freshly generated base becomes the newest variant; an override may still pin an older one.
  const base = await selectedRaw(species, f1, overrides);
  if (!base && !dryRun) return;

  await Promise.all(
    Array.from({ length: n - 1 }, (_, i) => i + 2).map(async (k) => {
      const name = frameName(activity, k);
      const current = await selectedRaw(species, name, overrides);
      const stale = !!current && !!base && current.meta.baseVariant !== base.meta.variant;
      if (current && !stale && !(values.regen && wants(k))) return;
      if (dryRun && !base) {
        console.log(`\n--- [dry-run] ${species}/${name} (edit of frame 1)\n${editPrompt(species, activity, k, values.note)}`);
        return;
      }
      await generate({
        species,
        name,
        prompt: editPrompt(species, activity, k, values.note),
        refs: [base!, ...(ref ? [ref] : [])],
        aspectRatio: FRAME_ASPECT,
        imageSize: FRAME_GEN_SIZE,
        baseVariant: base!.meta.variant,
      });
    }),
  );
}

for (const species of speciesList) {
  let ref = await selectedRaw(species, "reference", overrides);
  if (!ref || values.reference) {
    const ok = await generate({
      species,
      name: "reference",
      prompt: referencePrompt(species),
      refs: [],
      aspectRatio: REFERENCE_ASPECT,
      imageSize: REFERENCE_SIZE,
    });
    if (!ok) continue;
    const newest = (await listVariants(species, "reference")).at(-1);
    ref = newest === undefined ? null : await selectedRaw(species, "reference", { [species]: { reference: { variant: newest } } });
  }
  if (!ref && !dryRun) continue;
  await Promise.all(activities.map((a) => animation(species, a, ref)));
}

// Report stale edits that could not be refreshed (e.g. failures) so the review loop sees them.
if (!dryRun) {
  for (const species of speciesList) {
    for (const a of activities) {
      const base = await selectedRaw(species, frameName(a, 1), overrides);
      if (!base) continue;
      for (let k = 2; k <= frameCount(species, a); k++) {
        const vs = await listVariants(species, frameName(a, k));
        if (!vs.length) continue;
        const m = await readMeta(species, frameName(a, k), vs.at(-1)!);
        if (m.baseVariant !== base.meta.variant) console.warn(`[warn] ${species}/${a}.f${k} is derived from base v${m.baseVariant}, selected base is v${base.meta.variant}`);
      }
    }
  }
}

console.log(`\n${dryRun ? "dry run" : `${calls} image generation call(s)`}; ${failures.length} failure(s).`);
if (failures.length) {
  for (const f of failures) console.error(`  - ${f}`);
  process.exit(1);
}
