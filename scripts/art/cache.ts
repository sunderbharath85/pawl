/**
 * Raw generation cache: scripts/art/.cache/raw/<species>/<name>.v<N>.<ext> + .v<N>.json metadata.
 * `name` is "reference" or "<activity>.f<frame>" (1-based). Variants are append-only so nothing
 * is ever re-paid for.
 */
import { mkdir, readdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { Species } from "@pawl/protocol";
import { RAW_DIR, type Activity, type Overrides } from "./config.ts";

export type RawName = "reference" | `${Activity}.f${number}`;

export function frameName(activity: Activity, frame: number): RawName {
  return `${activity}.f${frame}`;
}

export interface RawMeta {
  species: Species;
  name: RawName;
  variant: number;
  file: string;
  mime: string;
  model: string;
  prompt: string;
  aspectRatio: string;
  imageSize: string;
  refs: string[];
  note?: string;
  /** For edited frames (2..N): the variant of frame 1 they were derived from. */
  baseVariant?: number;
  text?: string;
  interactionId?: string;
  usage?: unknown;
  createdAt: string;
}

const EXT: Record<string, string> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/webp": "webp",
};

export function speciesDir(species: Species): string {
  return join(RAW_DIR, species);
}

export async function listVariants(species: Species, name: RawName): Promise<number[]> {
  let files: string[] = [];
  try {
    files = await readdir(speciesDir(species));
  } catch {
    return [];
  }
  const re = new RegExp(`^${name.replaceAll(".", "\\.")}\\.v(\\d+)\\.json$`);
  return files
    .map((f) => re.exec(f)?.[1])
    .filter((v): v is string => v !== undefined)
    .map(Number)
    .sort((a, b) => a - b);
}

export async function readMeta(species: Species, name: RawName, variant: number): Promise<RawMeta> {
  const p = join(speciesDir(species), `${name}.v${variant}.json`);
  return JSON.parse(await readFile(p, "utf8")) as RawMeta;
}

export interface RawImage {
  meta: RawMeta;
  path: string;
  data: Buffer;
}

/** Variant chosen by overrides.json, else the newest one. */
export async function selectedRaw(species: Species, name: RawName, overrides: Overrides): Promise<RawImage | null> {
  const variants = await listVariants(species, name);
  if (variants.length === 0) return null;
  const wanted = overrides[species]?.[name]?.variant;
  const variant = wanted !== undefined && variants.includes(wanted) ? wanted : variants.at(-1)!;
  const meta = await readMeta(species, name, variant);
  const path = join(speciesDir(species), meta.file);
  return { meta, path, data: await readFile(path) };
}

export async function saveRaw(
  meta: Omit<RawMeta, "variant" | "file" | "createdAt">,
  data: Buffer,
): Promise<RawMeta> {
  const dir = speciesDir(meta.species);
  await mkdir(dir, { recursive: true });
  const variant = ((await listVariants(meta.species, meta.name)).at(-1) ?? 0) + 1;
  const ext = EXT[meta.mime] ?? "bin";
  const file = `${meta.name}.v${variant}.${ext}`;
  const full: RawMeta = { ...meta, variant, file, createdAt: new Date().toISOString() };
  await writeFile(join(dir, file), data);
  await writeFile(join(dir, `${meta.name}.v${variant}.json`), JSON.stringify(full, null, 2) + "\n");
  return full;
}
