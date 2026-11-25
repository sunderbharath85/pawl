#!/usr/bin/env bun
/**
 * Reaction icons for the pet's chat replies (assets/icons/), in the dog sprites' art style.
 *
 *   bun run icons.ts                                   # generate missing icons (cached), then process all
 *   bun run icons.ts --names alert,done --regen [--note "..."]   # new variants for some icons
 *   bun run icons.ts --no-gen                          # process the cached raws only (no API calls)
 *   bun run icons.ts --dry-run                         # print the prompts, no API calls, no output
 *   bun run icons.ts --check                           # validate assets/icons only
 *
 * Output:
 *   assets/icons/<name>.png      128 x 128 RGBA, icon centred with a small transparent margin
 *   assets/icons/<name>@48.png   48 x 48, resampled from the full-res key (crisper than 128 -> 24 in a browser)
 *   assets/icons/manifest.json   { version: 1, size, smallSize, icons: { <name>: { file, small, emoji, meaning } } }
 *   docs/screenshots/reaction-icons.png   contact sheet (128 / 48 / 24 / 20 px inline, light + dark)
 *
 * Each icon is one 1:1 1K sticker on the flat #00FF00 chroma background (same rules as the pets),
 * drawn with a style sheet of the shipped dog sprites (idle, thinking, alert, napping) as image
 * input. Raw generations are cached append-only at .cache/raw/icons/<name>.v<N>.<ext> + .json;
 * the newest variant is used unless overrides.json pins one: { "icons": { "<name>": { "variant": 1 } } }.
 * Keying (spill = G - max(R, B), despill) matches process.ts; pet processing is not involved.
 */
import { mkdir, readdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { parseArgs } from "node:util";
import sharp, { type OverlayOptions } from "sharp";
import {
  BACKGROUND_RULES,
  CHROMA_HEX,
  FRAME_ASPECT,
  FRAME_GEN_SIZE,
  FRAME_SIZE,
  MODEL,
  OUT_DIR,
  OVERRIDES_PATH,
  RAW_DIR,
  REPO_ROOT,
  SPILL_HI,
  SPILL_LO,
  parseList,
} from "./config.ts";
import { generateImage } from "./gemini.ts";

sharp.cache(false);

// ---------------------------------------------------------------------------
// Icon set
// ---------------------------------------------------------------------------

/** Manifest order. */
export const ICON_NAMES = ["paw", "look", "alert", "done", "thinking", "sleep", "tools"] as const;
export type IconName = (typeof ICON_NAMES)[number];

interface IconSpec {
  /** Fallback when the PNG is missing or images are off. */
  emoji: string;
  meaning: string;
  /** What to draw. Glyphs other than '?', 'Zz' and '!' are not allowed. */
  subject: string;
  /** Short sample text for the contact sheet's inline row. */
  sample: string;
}

export const ICONS: Record<IconName, IconSpec> = {
  paw: {
    emoji: "🐾",
    meaning: "greeting / general reply",
    subject:
      "one chunky cartoon dog paw print seen flat from the front: one large rounded main pad with four round toe pads " +
      "above it in a gentle arc, each pad a separate shape with a small gap between them, all filled golden-tan like the " +
      "puppy's fur with a small cream highlight on each pad",
    sample: "Hi there!",
  },
  look: {
    emoji: "👀",
    meaning: "status overview",
    subject:
      "the puppy peeking: its head seen from the front, only from the nose up (golden-tan fur, darker-tan floppy ears " +
      "hanging at the sides, cream around the eyes), rising above a short, thick, horizontal cream bar with a dark-brown " +
      "outline that is no wider than the head and hides the lower half of the face; two very big round dark-brown eyes, " +
      "identical in shape, each with one round white highlight, both glancing to the right; no mouth, no nose, no body. " +
      "The icon is compact and nearly square: the head is large and the bar is short",
    sample: "Status",
  },
  alert: {
    emoji: "⚠️",
    meaning: "something needs you",
    subject:
      "a round badge in the same red as the puppy's collar, with a soft lighter-red highlight on its upper left, and one big " +
      "bold cream-white exclamation mark '!' in the middle (thick dark-brown outline around the badge and around the '!')",
    sample: "Needs you",
  },
  done: {
    emoji: "✅",
    meaning: "finished",
    subject:
      "a round badge in the warm golden-yellow of the puppy's round collar tag, with a soft lighter highlight on its upper " +
      "left, and one big bold cream-white check mark in the middle (thick dark-brown outline around the badge and around " +
      "the check mark); the check mark is cream-white, never green",
    sample: "Done",
  },
  thinking: {
    emoji: "🤔",
    meaning: "not sure / asking which",
    subject:
      "a puffy white cloud-shaped thought bubble like the one above the thinking puppy in image 1, with two small round " +
      "trailing bubbles at its lower left, and one big bold dark-brown question mark '?' in its centre",
    sample: "Which one?",
  },
  sleep: {
    emoji: "💤",
    meaning: "all quiet",
    subject:
      "a small cream-yellow crescent moon with a peaceful closed-eye curve, and two big bold light-lavender letters 'Z' " +
      "(larger) and 'z' (smaller, higher) rising to its upper right, like the puppy's sleeping 'z' letters in image 1; the " +
      "big 'Z' is about as tall as half the moon",
    sample: "All quiet",
  },
  tools: {
    emoji: "🛠️",
    meaning: "proposing to start or prompt an agent",
    subject:
      "a chunky cream dog bone crossed in an X with a small sturdy wrench that has a light steel-grey head and a warm " +
      "orange handle; both are short and thick",
    sample: "Start agent?",
  },
};

// ---------------------------------------------------------------------------
// Paths + constants
// ---------------------------------------------------------------------------

export const ICON_OUT_DIR = join(REPO_ROOT, "assets", "icons");
export const ICON_MANIFEST_PATH = join(ICON_OUT_DIR, "manifest.json");
const ICON_RAW_DIR = join(RAW_DIR, "icons");
const CONTACT_SHEET_PATH = join(REPO_ROOT, "docs", "screenshots", "reaction-icons.png");
const STYLE_REF_FILE = "style-ref.png";

/** Output sizes: `file` (main) and `small` (pre-resampled for 20-24 CSS px at 1x/2x). */
export const ICON_SIZE = 128;
export const ICON_SMALL_SIZE = 48;
/** Transparent margin on each side, as a fraction of the output size. */
const PAD_FRAC = 0.06;

/** Dog sprite frames (activity, 0-based frame) composed into the style reference, 2 x 2 on green. */
const STYLE_REF_FRAMES: [string, number][] = [
  ["idle", 0],
  ["thinking", 3],
  ["alert", 0],
  ["napping", 2],
];

// ---------------------------------------------------------------------------
// Prompts
// ---------------------------------------------------------------------------

const ICON_STYLE = [
  "Cute, cozy 2D cartoon sticker art in exactly the art style of image 1 (the app's puppy mascot and its props): the same clean dark-brown outline (#3b2a20), flat cel shading with at most one soft shade tone per color, no gradients, no texture, no fine detail.",
  "Palette taken from the puppy: golden-tan and warm orange, cream, dark-brown outlines, with the collar red, the tag gold, the cheek pink and the lavender of its sleeping 'z' letters as accents.",
  "Extra-thick, bold, uniform dark-brown outline around the whole icon and around every inner shape, clearly thicker than in a normal illustration, so the icon still reads when shrunk to 20 pixels on a light or a dark background. The dark-brown outline is the outermost edge of the icon: no white, beige or light sticker border around it.",
  "Very simple, chunky, rounded shapes with a strong, instantly recognizable silhouette; no thin lines, no small details.",
  "A single crisp, static, front-facing flat icon: no motion lines, no sparkles, no blur, no drop shadow, no transparency or fading effects.",
].join(" ");

export function iconPrompt(name: IconName, note?: string): string {
  const s = ICONS[name];
  return [
    "A single sticker icon for a desktop app's chat, shown on its own.",
    `Icon: ${s.subject}.`,
    "Image 1 shows the app's puppy mascot and its props. Use it only as the reference for art style, outline and palette." +
      (name === "look" ? "" : " Do not draw the puppy, any other character, or any face."),
    "The icon is centered and fills about 70% of the image width and height, with wide empty margins on every side; nothing touches the image edges. " +
      "Exactly one icon: no copies, no extra decorations, no frame, badge or shape behind it unless described.",
    "No text, letters or numbers except the characters named in the icon description.",
    ICON_STYLE,
    BACKGROUND_RULES,
    "Never use any green, teal or yellow-green color anywhere on the icon.",
    ...(note ? [`Important: ${note}`] : []),
  ].join("\n");
}

// ---------------------------------------------------------------------------
// Raw cache (.cache/raw/icons/<name>.v<N>.<ext> + .json), same scheme as cache.ts
// ---------------------------------------------------------------------------

interface IconMeta {
  group: "icons";
  name: IconName;
  variant: number;
  file: string;
  mime: string;
  model: string;
  prompt: string;
  aspectRatio: string;
  imageSize: string;
  refs: string[];
  note?: string;
  text?: string;
  interactionId?: string;
  usage?: unknown;
  createdAt: string;
}

const EXT: Record<string, string> = { "image/png": "png", "image/jpeg": "jpg", "image/webp": "webp" };

async function listVariants(name: IconName): Promise<number[]> {
  const files = await readdir(ICON_RAW_DIR).catch(() => [] as string[]);
  const re = new RegExp(`^${name}\\.v(\\d+)\\.json$`);
  return files
    .map((f) => re.exec(f)?.[1])
    .filter((v): v is string => v !== undefined)
    .map(Number)
    .sort((a, b) => a - b);
}

async function saveRaw(meta: Omit<IconMeta, "variant" | "file" | "createdAt">, data: Buffer): Promise<IconMeta> {
  await mkdir(ICON_RAW_DIR, { recursive: true });
  const variant = ((await listVariants(meta.name)).at(-1) ?? 0) + 1;
  const file = `${meta.name}.v${variant}.${EXT[meta.mime] ?? "bin"}`;
  const full: IconMeta = { ...meta, variant, file, createdAt: new Date().toISOString() };
  await writeFile(join(ICON_RAW_DIR, file), data);
  await writeFile(join(ICON_RAW_DIR, `${meta.name}.v${variant}.json`), JSON.stringify(full, null, 2) + "\n");
  return full;
}

/** overrides.json "icons" section: { "<name>": { "variant": N } }. */
async function loadPins(): Promise<Partial<Record<IconName, { variant?: number }>>> {
  const f = Bun.file(OVERRIDES_PATH);
  if (!(await f.exists())) return {};
  const all = (await f.json()) as { icons?: Partial<Record<IconName, { variant?: number }>> };
  return all.icons ?? {};
}

async function selectedRaw(name: IconName, pins: Partial<Record<IconName, { variant?: number }>>): Promise<{ meta: IconMeta; data: Buffer } | null> {
  const variants = await listVariants(name);
  if (!variants.length) return null;
  const wanted = pins[name]?.variant;
  const variant = wanted !== undefined && variants.includes(wanted) ? wanted : variants.at(-1)!;
  const meta = JSON.parse(await readFile(join(ICON_RAW_DIR, `${name}.v${variant}.json`), "utf8")) as IconMeta;
  return { meta, data: await readFile(join(ICON_RAW_DIR, meta.file)) };
}

/** Style reference: four dog sprite frames on the chroma green, rebuilt from the committed strips. */
async function buildStyleRef(): Promise<Buffer> {
  const tiles = await Promise.all(
    STYLE_REF_FRAMES.map(([activity, frame]) =>
      sharp(join(OUT_DIR, "dog", `${activity}.png`))
        .extract({ left: frame * FRAME_SIZE, top: 0, width: FRAME_SIZE, height: FRAME_SIZE })
        .png()
        .toBuffer(),
    ),
  );
  const data = await sharp({ create: { width: 2 * FRAME_SIZE, height: 2 * FRAME_SIZE, channels: 3, background: CHROMA_HEX } })
    .composite(tiles.map((input, i) => ({ input, left: (i % 2) * FRAME_SIZE, top: Math.floor(i / 2) * FRAME_SIZE })))
    .png()
    .toBuffer();
  await mkdir(ICON_RAW_DIR, { recursive: true });
  await writeFile(join(ICON_RAW_DIR, STYLE_REF_FILE), data);
  return data;
}

// ---------------------------------------------------------------------------
// Keying + layout
// ---------------------------------------------------------------------------

interface Rgba {
  data: Buffer;
  w: number;
  h: number;
}

/** Green-screen key, identical to process.ts: alpha from spill = G - max(R,B), then despill. In place. */
function chromaKey(img: Rgba): void {
  const d = img.data;
  const span = SPILL_HI - SPILL_LO;
  for (let i = 0; i < d.length; i += 4) {
    const r = d[i]!;
    const g = d[i + 1]!;
    const b = d[i + 2]!;
    const m = r > b ? r : b;
    const spill = g - m;
    let a = 255;
    if (spill >= SPILL_HI) a = 0;
    else if (spill > SPILL_LO) a = Math.round((255 * (SPILL_HI - spill)) / span);
    if (spill > 0) d[i + 1] = m;
    const na = Math.min(a, d[i + 3]!);
    d[i + 3] = na;
    if (na === 0) d[i] = d[i + 1] = d[i + 2] = 0;
  }
}

interface Box {
  minX: number;
  maxX: number;
  minY: number;
  maxY: number;
}

/**
 * 8-connected components over alpha > 0: keep every part with at least 0.05% of the largest
 * part's solid area (drops keying specks, keeps small trailing bubbles / letters), clear the
 * rest in place, and return the union bbox of what is kept.
 */
function keepParts(img: Rgba, issues: string[]): Box | null {
  const { w, h, data } = img;
  const n = w * h;
  const labels = new Int32Array(n);
  const stack = new Int32Array(n);
  const parts: (Box & { id: number; solid: number; edge: boolean })[] = [];
  for (let start = 0; start < n; start++) {
    if (labels[start] !== 0 || data[start * 4 + 3] === 0) continue;
    const c = { id: parts.length + 1, solid: 0, edge: false, minX: w, maxX: -1, minY: h, maxY: -1 };
    let sp = 0;
    stack[sp++] = start;
    labels[start] = c.id;
    while (sp > 0) {
      const p = stack[--sp]!;
      const x = p % w;
      const y = (p - x) / w;
      if (data[p * 4 + 3]! >= 128) c.solid++;
      if (x < c.minX) c.minX = x;
      if (x > c.maxX) c.maxX = x;
      if (y < c.minY) c.minY = y;
      if (y > c.maxY) c.maxY = y;
      if (x === 0 || y === 0 || x === w - 1 || y === h - 1) c.edge = true;
      for (let dy = -1; dy <= 1; dy++) {
        const yy = y + dy;
        if (yy < 0 || yy >= h) continue;
        for (let dx = -1; dx <= 1; dx++) {
          const xx = x + dx;
          if ((dx === 0 && dy === 0) || xx < 0 || xx >= w) continue;
          const q = yy * w + xx;
          if (labels[q] === 0 && data[q * 4 + 3] !== 0) {
            labels[q] = c.id;
            stack[sp++] = q;
          }
        }
      }
    }
    parts.push(c);
  }
  if (!parts.length) {
    issues.push("empty after keying");
    return null;
  }
  const largest = Math.max(...parts.map((c) => c.solid));
  const kept = parts.filter((c) => c.solid >= Math.max(64, largest * 0.0005));
  const keep = new Set(kept.map((c) => c.id));
  for (let p = 0; p < n; p++) {
    const l = labels[p]!;
    if (l !== 0 && !keep.has(l)) data[p * 4] = data[p * 4 + 1] = data[p * 4 + 2] = data[p * 4 + 3] = 0;
  }
  for (const c of kept) if (c.edge) issues.push(`touches the image edge at x=${c.minX}..${c.maxX} y=${c.minY}..${c.maxY} (cropped?)`);
  return {
    minX: Math.min(...kept.map((c) => c.minX)),
    maxX: Math.max(...kept.map((c) => c.maxX)),
    minY: Math.min(...kept.map((c) => c.minY)),
    maxY: Math.max(...kept.map((c) => c.maxY)),
  };
}

/** Crop to `box`, fit its longer side into size - 2 * pad (Lanczos), centre on a transparent square. */
async function renderIcon(img: Rgba, box: Box, size: number): Promise<Buffer> {
  const pad = Math.max(2, Math.round(size * PAD_FRAC));
  const cw = box.maxX - box.minX + 1;
  const ch = box.maxY - box.minY + 1;
  const crop = Buffer.alloc(cw * ch * 4);
  for (let y = 0; y < ch; y++) {
    const from = ((box.minY + y) * img.w + box.minX) * 4;
    img.data.copy(crop, y * cw * 4, from, from + cw * 4);
  }
  const s = (size - 2 * pad) / Math.max(cw, ch);
  const tw = Math.max(1, Math.round(cw * s));
  const th = Math.max(1, Math.round(ch * s));
  const resized = await sharp(crop, { raw: { width: cw, height: ch, channels: 4 } })
    .resize(tw, th, { kernel: "lanczos3", fit: "fill" })
    .raw()
    .toBuffer();
  const left = Math.floor((size - tw) / 2);
  const top = Math.floor((size - th) / 2);
  const out = Buffer.alloc(size * size * 4);
  for (let y = 0; y < th; y++) {
    for (let x = 0; x < tw; x++) {
      const si = (y * tw + x) * 4;
      const a = resized[si + 3]!;
      if (a < 10) continue; // faint resampling fringe
      const oi = ((top + y) * size + left + x) * 4;
      const r = resized[si]!;
      const g = resized[si + 1]!;
      const b = resized[si + 2]!;
      const m = r > b ? r : b;
      out[oi] = r;
      out[oi + 1] = a < 192 && g > m ? m : g; // despill Lanczos ringing on soft edges
      out[oi + 2] = b;
      out[oi + 3] = a;
    }
  }
  return out;
}

function greenPixels(rgba: Buffer): number {
  let n = 0;
  for (let i = 0; i < rgba.length; i += 4) if (rgba[i + 3]! > 0 && rgba[i + 1]! > Math.max(rgba[i]!, rgba[i + 2]!) + 24) n++;
  return n;
}

// ---------------------------------------------------------------------------
// Manifest + validation
// ---------------------------------------------------------------------------

export interface IconEntry {
  /** 128 px PNG, relative to assets/icons. */
  file: string;
  /** 48 px PNG (use for 20-24 CSS px). */
  small: string;
  emoji: string;
  meaning: string;
}
export interface IconManifest {
  version: 1;
  size: number;
  smallSize: number;
  icons: Partial<Record<IconName, IconEntry>>;
}

const fileOf = (name: IconName) => `${name}.png`;
const smallOf = (name: IconName) => `${name}@${ICON_SMALL_SIZE}.png`;

async function buildManifest(): Promise<IconManifest> {
  const icons: IconManifest["icons"] = {};
  for (const name of ICON_NAMES) {
    if (!(await Bun.file(join(ICON_OUT_DIR, fileOf(name))).exists())) continue;
    if (!(await Bun.file(join(ICON_OUT_DIR, smallOf(name))).exists())) continue;
    icons[name] = { file: fileOf(name), small: smallOf(name), emoji: ICONS[name].emoji, meaning: ICONS[name].meaning };
  }
  return { version: 1, size: ICON_SIZE, smallSize: ICON_SMALL_SIZE, icons };
}

async function check(): Promise<string[]> {
  const errors: string[] = [];
  if (!(await Bun.file(ICON_MANIFEST_PATH).exists())) return ["assets/icons/manifest.json missing"];
  const m = (await Bun.file(ICON_MANIFEST_PATH).json()) as IconManifest;
  if (m.version !== 1) errors.push(`version ${String(m.version)} != 1`);
  for (const [name, e] of Object.entries(m.icons)) {
    if (!(ICON_NAMES as readonly string[]).includes(name)) errors.push(`unknown icon ${name}`);
    if (!e?.emoji || !e.meaning) errors.push(`${name}: emoji and meaning are required`);
    for (const [file, size] of [
      [e?.file, m.size],
      [e?.small, m.smallSize],
    ] as const) {
      const path = join(ICON_OUT_DIR, file ?? "");
      if (!file || !(await Bun.file(path).exists())) {
        errors.push(`${name}: missing ${file}`);
        continue;
      }
      const meta = await sharp(path).metadata();
      if (meta.format !== "png" || !meta.hasAlpha) errors.push(`${file}: must be a PNG with alpha`);
      if (meta.width !== size || meta.height !== size) errors.push(`${file}: ${meta.width}x${meta.height} != ${size}x${size}`);
      const { data, info } = await sharp(path).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
      for (const [x, y] of [
        [0, 0],
        [info.width - 1, 0],
        [0, info.height - 1],
        [info.width - 1, info.height - 1],
      ] as const) {
        if (data[(y * info.width + x) * 4 + 3] !== 0) errors.push(`${file}: corner (${x},${y}) not transparent`);
      }
      const g = greenPixels(data);
      if (g > 0) errors.push(`${file}: ${g} green-fringe pixel(s)`);
    }
  }
  return errors;
}

// ---------------------------------------------------------------------------
// Contact sheet: 128 / 48 / 24 / 20 px inline, on light and dark
// ---------------------------------------------------------------------------

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

async function contactSheet(manifest: IconManifest): Promise<void> {
  const names = ICON_NAMES.filter((n) => manifest.icons[n]);
  if (!names.length) return;
  const cell = 160;
  const labelW = 110;
  const rows = [
    { label: "128 px", h: 150 },
    { label: "48 px", h: 64 },
    { label: "24 px", h: 40 },
    { label: "20 px inline", h: 40 },
  ];
  const headH = 34;
  const panelH = headH + rows.reduce((s, r) => s + r.h, 0) + 12;
  const W = labelW + names.length * cell;
  const H = panelH * 2;
  const panels = [
    { bg: "#f7f3ec", fg: "#2b2118", sub: "#7a6a5c", name: "light" },
    { bg: "#1c1a19", fg: "#f1e9df", sub: "#a39484", name: "dark" },
  ];
  const layers: OverlayOptions[] = [];
  const svg: string[] = [];
  const at = async (name: IconName, size: number) => {
    const src = join(ICON_OUT_DIR, size > ICON_SMALL_SIZE ? fileOf(name) : smallOf(name));
    return size === ICON_SIZE || size === ICON_SMALL_SIZE ? readFile(src) : sharp(src).resize(size, size, { kernel: "lanczos3" }).png().toBuffer();
  };
  for (const [p, panel] of panels.entries()) {
    const y0 = p * panelH;
    svg.push(`<rect x="0" y="${y0}" width="${W}" height="${panelH}" fill="${panel.bg}"/>`);
    for (const [i, name] of names.entries()) {
      svg.push(`<text x="${labelW + i * cell + cell / 2}" y="${y0 + 24}" text-anchor="middle" font-family="Arial, sans-serif" font-size="16" font-weight="bold" fill="${panel.fg}">${name}</text>`);
    }
    let y = y0 + headH;
    for (const row of rows) {
      svg.push(`<text x="12" y="${y + row.h / 2 + 5}" font-family="Arial, sans-serif" font-size="13" fill="${panel.sub}">${row.label}</text>`);
      for (const [i, name] of names.entries()) {
        const cx = labelW + i * cell;
        if (row.label.endsWith("inline")) {
          const textW = ICONS[name].sample.length * 7.2;
          const x = Math.round(cx + (cell - (20 + 6 + textW)) / 2);
          layers.push({ input: await at(name, 20), left: x, top: Math.round(y + (row.h - 20) / 2) });
          svg.push(`<text x="${x + 26}" y="${y + row.h / 2 + 5}" font-family="Arial, sans-serif" font-size="13" fill="${panel.fg}">${esc(ICONS[name].sample)}</text>`);
        } else {
          const size = Number.parseInt(row.label, 10);
          layers.push({ input: await at(name, size), left: Math.round(cx + (cell - size) / 2), top: Math.round(y + (row.h - size) / 2) });
        }
      }
      y += row.h;
    }
  }
  await mkdir(join(REPO_ROOT, "docs", "screenshots"), { recursive: true });
  await sharp({ create: { width: W, height: H, channels: 4, background: { r: 255, g: 255, b: 255, alpha: 1 } } })
    .composite([{ input: Buffer.from(`<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}">${svg.join("")}</svg>`), left: 0, top: 0 }, ...layers])
    .png({ compressionLevel: 9 })
    .toFile(CONTACT_SHEET_PATH);
  console.log(`[qa ] docs/screenshots/reaction-icons.png`);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

const { values } = parseArgs({
  args: Bun.argv.slice(2),
  options: {
    names: { type: "string" },
    regen: { type: "boolean", default: false },
    note: { type: "string" },
    "no-gen": { type: "boolean", default: false },
    "dry-run": { type: "boolean", default: false },
    check: { type: "boolean", default: false },
    concurrency: { type: "string", default: "4" },
  },
  strict: true,
});

async function generateMissing(names: IconName[]): Promise<{ calls: number; failures: string[] }> {
  const styleRef = await buildStyleRef();
  const todo: IconName[] = [];
  for (const name of names) if (values.regen || !(await listVariants(name)).length) todo.push(name);
  let calls = 0;
  const failures: string[] = [];
  const queue = [...todo];
  const worker = async () => {
    for (let name = queue.shift(); name; name = queue.shift()) {
      const prompt = iconPrompt(name, values.note);
      const t0 = performance.now();
      console.log(`[gen] icons/${name} ...`);
      try {
        const res = await generateImage({
          prompt,
          refs: [{ data: styleRef, mime: "image/png" }],
          aspectRatio: FRAME_ASPECT,
          imageSize: FRAME_GEN_SIZE,
        });
        calls++;
        const meta = await saveRaw(
          {
            group: "icons",
            name,
            mime: res.mime,
            model: MODEL,
            prompt,
            aspectRatio: FRAME_ASPECT,
            imageSize: FRAME_GEN_SIZE,
            refs: [`icons/${STYLE_REF_FILE}`],
            note: values.note,
            text: res.text || undefined,
            interactionId: res.interactionId,
            usage: res.usage,
          },
          res.data,
        );
        console.log(`[ok ] icons/${name} -> .cache/raw/icons/${meta.file} (${((performance.now() - t0) / 1000).toFixed(1)}s)`);
      } catch (e) {
        failures.push(`${name}: ${(e as Error).message}`);
        console.error(`[err] icons/${name}: ${(e as Error).message}`);
      }
    }
  };
  await Promise.all(Array.from({ length: Math.max(1, Number(values.concurrency) || 1) }, worker));
  return { calls, failures };
}

async function processAll(): Promise<IconManifest> {
  const pins = await loadPins();
  await mkdir(ICON_OUT_DIR, { recursive: true });
  for (const name of ICON_NAMES) {
    const raw = await selectedRaw(name, pins);
    if (!raw) {
      console.log(`[skip] ${name}: no raw generation yet`);
      continue;
    }
    const { data, info } = await sharp(raw.data).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
    const img: Rgba = { data, w: info.width, h: info.height };
    chromaKey(img);
    const issues: string[] = [];
    const box = keepParts(img, issues);
    if (!box) {
      console.log(`[WARN] ${name} (v${raw.meta.variant}): ${issues.join("; ")}`);
      continue;
    }
    let green = 0;
    for (const [size, file] of [
      [ICON_SIZE, fileOf(name)],
      [ICON_SMALL_SIZE, smallOf(name)],
    ] as const) {
      const out = await renderIcon(img, box, size);
      green += greenPixels(out);
      await sharp(out, { raw: { width: size, height: size, channels: 4 } })
        .png({ compressionLevel: 9, adaptiveFiltering: true })
        .toFile(join(ICON_OUT_DIR, file));
    }
    const flag = issues.length || green ? "WARN" : "ok  ";
    console.log(
      `[${flag}] ${name} (v${raw.meta.variant}): bbox ${box.maxX - box.minX + 1}x${box.maxY - box.minY + 1} of ${img.w}x${img.h}, green px ${green}` +
        (issues.length ? `\n        - ${issues.join("\n        - ")}` : ""),
    );
  }
  const manifest = await buildManifest();
  await writeFile(ICON_MANIFEST_PATH, JSON.stringify(manifest, null, 2) + "\n");
  console.log(`[out] assets/icons/manifest.json: ${Object.keys(manifest.icons).length} icons`);
  return manifest;
}

if (import.meta.main) {
  const names = parseList<IconName>(values.names, ICON_NAMES, "icon");
  if (values["dry-run"]) {
    for (const name of names) console.log(`\n--- [dry-run] icons/${name} (${FRAME_ASPECT} ${FRAME_GEN_SIZE}, refs: icons/${STYLE_REF_FILE})\n${iconPrompt(name, values.note)}`);
    process.exit(0);
  }
  if (!values.check) {
    if (!values["no-gen"]) {
      const { calls, failures } = await generateMissing(names);
      console.log(`${calls} image generation call(s); ${failures.length} failure(s).`);
      for (const f of failures) console.error(`  - ${f}`);
    }
    const manifest = await processAll();
    await contactSheet(manifest);
  }
  const errors = await check();
  if (errors.length) {
    console.error(`[check] FAIL (${errors.length}):\n  - ${errors.join("\n  - ")}`);
    process.exit(1);
  }
  console.log("[check] assets/icons manifest + PNGs valid");
}
