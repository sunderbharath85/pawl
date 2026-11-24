#!/usr/bin/env bun
/**
 * Turn cached raw Gemini frames into contract sprite strips + manifest.
 *
 *   bun run process.ts [--species dog] [--activities idle,walk] [--no-qa] [--clean] [--rgba]
 *   bun run process.ts --check            # validate assets/pets/manifest.json + PNGs only
 *
 * Per animation: chroma-key each frame image to alpha (with despill so edges carry no green
 * halo), drop specks, register frames 2..N onto frame 1 (alpha-overlap search) so static
 * parts never jitter, scale every animation of a species to the same character size
 * (body-area normalisation anchored on idle), put the feet on the baseline, centre, pack a
 * horizontal strip of `frames x FRAME_SIZE`, and rebuild the manifest from the files that
 * exist. Strips are written as indexed PNG + tRNS alpha (libimagequant, ~3x smaller than
 * truecolor, decodes to RGBA everywhere); --rgba writes 8-bit truecolor RGBA instead.
 * QA contact sheets land in scripts/art/.cache/qa/.
 */
import { mkdir, readdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { parseArgs } from "node:util";
import sharp, { type OverlayOptions } from "sharp";
import type { Species } from "@ai-pet/protocol";
import {
  ACTIVITIES,
  ANIMATIONS,
  BASELINE,
  FRAME_SIZE,
  IDLE_FILL,
  MANIFEST_PATH,
  OUT_DIR,
  PAD,
  QA_DIR,
  SPECIES,
  SPILL_HI,
  SPILL_LO,
  loadOverrides,
  frameCount,
  parseList,
  type Activity,
  type AlignMode,
  type Overrides,
} from "./config.ts";
import { frameName, selectedRaw } from "./cache.ts";

sharp.cache(false);

// ---------------------------------------------------------------------------
// Manifest types (mirrors docs/CONTRACTS.md "Pet sprite assets")
// ---------------------------------------------------------------------------

export interface SpriteAnimation {
  src: string;
  frames: number;
  fps: number;
  loop: boolean;
}
export interface SpriteManifest {
  version: 1;
  frameSize: number;
  species: Partial<Record<Species, { skins: string[]; animations: Partial<Record<Activity, SpriteAnimation>> }>>;
}

// ---------------------------------------------------------------------------
// Image helpers
// ---------------------------------------------------------------------------

interface Rgba {
  data: Buffer;
  w: number;
  h: number;
}

async function loadRgba(buf: Buffer, flip: boolean): Promise<Rgba> {
  let img = sharp(buf).ensureAlpha();
  if (flip) img = img.flop();
  const { data, info } = await img.raw().toBuffer({ resolveWithObject: true });
  if (info.channels !== 4) throw new Error(`expected 4 channels, got ${info.channels}`);
  return { data, w: info.width, h: info.height };
}

/** Green-screen key: alpha from spill = G - max(R,B), then despill G to max(R,B). In place. */
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
    const a0 = d[i + 3]!;
    const na = a < a0 ? a : a0;
    d[i + 3] = na;
    if (na === 0) {
      d[i] = 0;
      d[i + 1] = 0;
      d[i + 2] = 0;
    }
  }
}

interface Comp {
  id: number;
  /** Pixels with alpha > 0. */
  area: number;
  /** Pixels with alpha >= 128. */
  solid: number;
  cx: number;
  minX: number;
  maxX: number;
  minY: number;
  maxY: number;
  touchesEdge: boolean;
}

/** 8-connected components over alpha > 0. labels[i] = component id (1-based), 0 = background. */
function components(img: Rgba): { labels: Int32Array; comps: Comp[] } {
  const { w, h, data } = img;
  const n = w * h;
  const labels = new Int32Array(n);
  const stack = new Int32Array(n);
  const comps: Comp[] = [];
  let next = 0;
  for (let start = 0; start < n; start++) {
    if (labels[start] !== 0 || data[start * 4 + 3] === 0) continue;
    const id = ++next;
    const c: Comp = { id, area: 0, solid: 0, cx: 0, minX: w, maxX: -1, minY: h, maxY: -1, touchesEdge: false };
    let sumX = 0;
    let sumA = 0;
    let sp = 0;
    stack[sp++] = start;
    labels[start] = id;
    while (sp > 0) {
      const p = stack[--sp]!;
      const x = p % w;
      const y = (p - x) / w;
      const a = data[p * 4 + 3]!;
      c.area++;
      if (a >= 128) c.solid++;
      sumX += x * a;
      sumA += a;
      if (x < c.minX) c.minX = x;
      if (x > c.maxX) c.maxX = x;
      if (y < c.minY) c.minY = y;
      if (y > c.maxY) c.maxY = y;
      if (x === 0 || y === 0 || x === w - 1 || y === h - 1) c.touchesEdge = true;
      for (let dy = -1; dy <= 1; dy++) {
        const yy = y + dy;
        if (yy < 0 || yy >= h) continue;
        for (let dx = -1; dx <= 1; dx++) {
          const xx = x + dx;
          if ((dx === 0 && dy === 0) || xx < 0 || xx >= w) continue;
          const q = yy * w + xx;
          if (labels[q] === 0 && data[q * 4 + 3] !== 0) {
            labels[q] = id;
            stack[sp++] = q;
          }
        }
      }
    }
    c.cx = sumA > 0 ? sumX / sumA : (c.minX + c.maxX) / 2;
    comps.push(c);
  }
  return { labels, comps };
}

/** Clear every pixel whose component is not in `keep`. */
function maskTo(img: Rgba, labels: Int32Array, keep: Set<number>): void {
  const d = img.data;
  for (let p = 0; p < labels.length; p++) {
    const l = labels[p]!;
    if (l !== 0 && !keep.has(l)) {
      const i = p * 4;
      d[i] = d[i + 1] = d[i + 2] = d[i + 3] = 0;
    }
  }
}

// ---------------------------------------------------------------------------
// Frame extraction + registration
// ---------------------------------------------------------------------------

interface Frame {
  comps: Comp[];
  body: Comp;
  minX: number;
  maxX: number;
  minY: number;
  maxY: number;
  /** Body anchor: alpha-weighted centroid x and lowest y. */
  ax: number;
  bottom: number;
  bodyArea: number;
}

/** One generated image = one frame: keep the character + its props, drop specks. */
function extractFrame(comps: Comp[], k: number, issues: string[]): Frame | null {
  if (comps.length === 0) {
    issues.push(`frame ${k}: empty after keying`);
    return null;
  }
  const largest = Math.max(...comps.map((c) => c.solid));
  const kept = comps.filter((c) => c.solid >= Math.max(120, largest * 0.0015));
  const body = kept.reduce((a, b) => (b.solid > a.solid ? b : a));
  for (const c of kept) {
    if (c.touchesEdge) issues.push(`frame ${k}: content touches the image edge (cropped?) at x=${c.minX}..${c.maxX} y=${c.minY}..${c.maxY}`);
    if (c !== body && c.solid >= body.solid * 0.35 && Math.abs(c.cx - body.cx) > (body.maxX - body.minX) * 0.8) {
      issues.push(`frame ${k}: second character-sized part at x=${c.minX}..${c.maxX} (duplicate pose?)`);
    }
  }
  return {
    comps: kept,
    body,
    minX: Math.min(...kept.map((c) => c.minX)),
    maxX: Math.max(...kept.map((c) => c.maxX)),
    minY: Math.min(...kept.map((c) => c.minY)),
    maxY: Math.max(...kept.map((c) => c.maxY)),
    ax: body.cx,
    bottom: body.maxY,
    bodyArea: body.solid,
  };
}

interface Grid {
  g: Float32Array;
  w: number;
  h: number;
}

/** Block-averaged alpha (0..1) at 1/f resolution. */
function alphaGrid(img: Rgba, f: number): Grid {
  const w = Math.floor(img.w / f);
  const h = Math.floor(img.h / f);
  const g = new Float32Array(w * h);
  const norm = 1 / (255 * f * f);
  for (let y = 0; y < h * f; y++) {
    const row = y * img.w;
    const gy = ((y / f) | 0) * w;
    for (let x = 0; x < w * f; x++) g[gy + ((x / f) | 0)]! += img.data[(row + x) * 4 + 3]! * norm;
  }
  return { g, w, h };
}

/** Shift (dx, dy) in grid cells, around (cx, cy) ± r, maximising Σ min(a[x,y], b[x+dx, y+dy]). */
function bestShift(a: Grid, b: Grid, cx: number, cy: number, r: number): [number, number] {
  let best = -1;
  let bx = cx;
  let by = cy;
  for (let dy = cy - r; dy <= cy + r; dy++) {
    for (let dx = cx - r; dx <= cx + r; dx++) {
      let s = 0;
      const y0 = Math.max(0, -dy);
      const y1 = Math.min(a.h, b.h - dy);
      const x0 = Math.max(0, -dx);
      const x1 = Math.min(a.w, b.w - dx);
      for (let y = y0; y < y1; y++) {
        const ai = y * a.w;
        const bi = (y + dy) * b.w + dx;
        for (let x = x0; x < x1; x++) {
          const va = a.g[ai + x]!;
          if (va === 0) continue;
          const vb = b.g[bi + x]!;
          s += va < vb ? va : vb;
        }
      }
      if (s > best) {
        best = s;
        bx = dx;
        by = dy;
      }
    }
  }
  return [bx, by];
}

/** Translation (full-res px) that maps frame-1 content onto `img`: a[x,y] ≈ img[x+dx, y+dy]. */
function register(a: Rgba, b: Rgba): [number, number] {
  const [cx, cy] = bestShift(alphaGrid(a, 8), alphaGrid(b, 8), 0, 0, 16);
  const [fx, fy] = bestShift(alphaGrid(a, 2), alphaGrid(b, 2), cx * 4, cy * 4, 5);
  return [fx * 2, fy * 2];
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

interface Placed {
  frame: Frame;
  /** Subtract from raw x to get common (frame-1) coordinates. */
  tx: number;
  /** Raw y that lands on BASELINE. */
  refY: number;
  /** Procedural lift in output px (ActivitySpec.hop). */
  hop: number;
}

interface Layout {
  scale: number;
  fitScale: number;
  maxScale: number;
  /** Common-coordinate x of the anchor (frame-1 body centroid). */
  ax: number;
  /** Output x of the anchor. */
  anchorX: number;
  placed: Placed[];
}

function median(xs: number[]): number {
  const s = [...xs].sort((a, b) => a - b);
  const m = s.length >> 1;
  return s.length % 2 ? s[m]! : (s[m - 1]! + s[m]!) / 2;
}

function layout(
  frames: Frame[],
  shifts: [number, number][],
  mode: AlignMode,
  desiredScale: number | null,
  hops: number[] = [],
): Layout {
  const ax = frames[0]!.ax;
  let placed: Placed[];
  if (mode === "shared") {
    const ground = Math.max(...frames.map((f) => f.bottom));
    placed = frames.map((frame, i) => ({ frame, tx: 0, refY: ground, hop: hops[i] ?? 0 }));
  } else if (mode === "feet") {
    placed = frames.map((frame, i) => ({ frame, tx: shifts[i]![0], refY: frame.bottom, hop: hops[i] ?? 0 }));
  } else {
    const ground = Math.max(...frames.map((f, i) => f.bottom - shifts[i]![1]));
    placed = frames.map((frame, i) => ({ frame, tx: shifts[i]![0], refY: ground + shifts[i]![1], hop: hops[i] ?? 0 }));
  }
  let left = Infinity;
  let right = -Infinity;
  let top = Infinity;
  let below = 0;
  let upLimit = Infinity;
  for (const { frame: f, tx, refY, hop } of placed) {
    left = Math.min(left, f.minX - tx - ax);
    right = Math.max(right, f.maxX + 1 - tx - ax);
    top = Math.min(top, f.minY - refY);
    below = Math.max(below, f.maxY + 1 - refY);
    if (f.minY < refY) upLimit = Math.min(upLimit, (BASELINE - PAD - hop) / (refY - f.minY));
  }
  const maxScale = Math.min(
    (FRAME_SIZE - 2 * PAD) / (right - left),
    upLimit,
    below > 1 ? (FRAME_SIZE - PAD / 2 - BASELINE) / below : Infinity,
  );
  const fitScale = Math.min(maxScale, (IDLE_FILL * FRAME_SIZE) / Math.max(right - left, below - top));
  const scale = Math.min(maxScale, desiredScale ?? fitScale);
  const anchorX = FRAME_SIZE / 2 - ((left + right) / 2) * scale;
  return { scale, fitScale, maxScale, ax, anchorX, placed };
}

async function renderFrame(src: Rgba, labels: Int32Array, p: Placed, lay: Layout): Promise<Buffer> {
  const f = p.frame;
  const ids = new Set(f.comps.map((c) => c.id));
  const cw = f.maxX - f.minX + 1;
  const ch = f.maxY - f.minY + 1;
  const crop = Buffer.alloc(cw * ch * 4);
  for (let y = 0; y < ch; y++) {
    for (let x = 0; x < cw; x++) {
      const q = (f.minY + y) * src.w + (f.minX + x);
      if (!ids.has(labels[q]!)) continue;
      src.data.copy(crop, (y * cw + x) * 4, q * 4, q * 4 + 4);
    }
  }
  const tw = Math.max(1, Math.round(cw * lay.scale));
  const th = Math.max(1, Math.round(ch * lay.scale));
  const resized = await sharp(crop, { raw: { width: cw, height: ch, channels: 4 } })
    .resize(tw, th, { kernel: "lanczos3", fit: "fill" })
    .raw()
    .toBuffer();
  const left = Math.round(lay.anchorX + (f.minX - p.tx - lay.ax) * lay.scale);
  const top = Math.round(BASELINE + (f.minY - p.refY) * lay.scale - p.hop);
  const out = Buffer.alloc(FRAME_SIZE * FRAME_SIZE * 4);
  for (let y = 0; y < th; y++) {
    const oy = top + y;
    if (oy < 0 || oy >= FRAME_SIZE) continue;
    for (let x = 0; x < tw; x++) {
      const ox = left + x;
      if (ox < 0 || ox >= FRAME_SIZE) continue;
      const si = (y * tw + x) * 4;
      const a = resized[si + 3]!;
      if (a < 10) continue; // drop faint resampling fringe
      const oi = (oy * FRAME_SIZE + ox) * 4;
      const r = resized[si]!;
      const g = resized[si + 1]!;
      const b = resized[si + 2]!;
      const m = r > b ? r : b;
      out[oi] = r;
      // Lanczos ringing can reintroduce a green tint on soft edge pixels: despill them again.
      out[oi + 1] = a < 192 && g > m ? m : g;
      out[oi + 2] = b;
      out[oi + 3] = a;
    }
  }
  return out;
}

interface StripQa {
  species: Species;
  activity: Activity;
  variants: string;
  frames: number;
  scale: number;
  shifts: [number, number][];
  issues: string[];
  edgePixels: number;
  greenFringe: number;
}

function qaFrame(frame: Buffer): { edge: number; green: number } {
  let edge = 0;
  let green = 0;
  for (let y = 0; y < FRAME_SIZE; y++) {
    for (let x = 0; x < FRAME_SIZE; x++) {
      const i = (y * FRAME_SIZE + x) * 4;
      if (frame[i + 3] === 0) continue;
      if (x === 0 || y === 0 || x === FRAME_SIZE - 1 || y === FRAME_SIZE - 1) edge++;
      if (frame[i + 1]! > Math.max(frame[i]!, frame[i + 2]!) + 24) green++;
    }
  }
  return { edge, green };
}

// ---------------------------------------------------------------------------
// Species pipeline
// ---------------------------------------------------------------------------

interface SourceFrame {
  k: number;
  variant: number;
  img: Rgba;
  labels: Int32Array;
  frame: Frame;
}

interface Prepared {
  activity: Activity;
  frames: SourceFrame[];
  shifts: [number, number][];
  issues: string[];
}

async function prepare(species: Species, activity: Activity, overrides: Overrides): Promise<Prepared | null> {
  const o = overrides[species]?.[activity];
  if (o?.skip) return null;
  const n = frameCount(species, activity);
  const issues: string[] = [];
  const frames: SourceFrame[] = [];
  let baseVariant: number | undefined;
  for (let k = 1; k <= n; k++) {
    if (o?.dropFrames?.includes(k)) continue;
    const raw = await selectedRaw(species, frameName(activity, k), overrides);
    if (!raw) {
      // An animation is only emitted once every frame exists.
      if (k === 1 || frames.length > 0) return null;
      continue;
    }
    if (k === 1) baseVariant = raw.meta.variant;
    else if (baseVariant !== undefined && raw.meta.baseVariant !== baseVariant) {
      issues.push(`frame ${k} was edited from base v${raw.meta.baseVariant}, selected base is v${baseVariant}`);
    }
    const img = await loadRgba(raw.data, o?.flip ?? false);
    chromaKey(img);
    const { labels, comps } = components(img);
    const frame = extractFrame(comps, k, issues);
    if (!frame) continue;
    maskTo(img, labels, new Set(frame.comps.map((c) => c.id)));
    frames.push({ k, variant: raw.meta.variant, img, labels, frame });
  }
  if (frames.length === 0) return null;
  const first = frames[0]!;
  const shifts = frames.map((f): [number, number] => (f === first ? [0, 0] : register(first.img, f.img)));
  return { activity, frames, shifts, issues };
}

async function processSpecies(
  species: Species,
  activities: Activity[],
  overrides: Overrides,
  qa: StripQa[],
  previews: boolean,
  palette: boolean,
): Promise<void> {
  const all = new Map<Activity, Prepared>();
  // Idle is always prepared (even when not requested) because it anchors the species scale.
  for (const a of ACTIVITIES) {
    if (a !== "idle" && !activities.includes(a)) continue;
    const p = await prepare(species, a, overrides);
    if (p) all.set(a, p);
  }
  if (all.size === 0) return;
  const alignOf = (a: Activity) => overrides[species]?.[a]?.align ?? ANIMATIONS[a].align;

  // Species size: idle's union bbox fitted to IDLE_FILL; everything else matches idle's body area.
  const anchor = all.get("idle") ?? [...all.values()][0]!;
  const anchorFrames = anchor.frames.map((f) => f.frame);
  const anchorScale = layout(anchorFrames, anchor.shifts, alignOf(anchor.activity), null).fitScale * (overrides[species]?.[anchor.activity]?.scale ?? 1);
  const targetArea = median(anchorFrames.map((f) => f.bodyArea)) * anchorScale ** 2;

  await mkdir(join(OUT_DIR, species), { recursive: true });
  for (const a of activities) {
    const p = all.get(a);
    if (!p) continue;
    const frames = p.frames.map((f) => f.frame);
    const desired =
      p === anchor
        ? anchorScale
        : Math.sqrt(targetArea / median(frames.map((f) => f.bodyArea))) * ANIMATIONS[a].sizeBias * (overrides[species]?.[a]?.scale ?? 1);
    const hops = p.frames.map((f) => ANIMATIONS[a].hop?.[f.k - 1] ?? 0);
    const lay = layout(frames, p.shifts, alignOf(a), desired, hops);
    if (lay.scale < desired * 0.9) p.issues.push(`shrunk to fit: scale ${lay.scale.toFixed(3)} < desired ${desired.toFixed(3)}`);
    const rendered: Buffer[] = [];
    let edge = 0;
    let green = 0;
    for (let i = 0; i < p.frames.length; i++) {
      const sf = p.frames[i]!;
      const buf = await renderFrame(sf.img, sf.labels, lay.placed[i]!, lay);
      const q = qaFrame(buf);
      edge += q.edge;
      green += q.green;
      rendered.push(buf);
    }
    const n = rendered.length;
    const strip = await sharp({
      create: { width: n * FRAME_SIZE, height: FRAME_SIZE, channels: 4, background: { r: 0, g: 0, b: 0, alpha: 0 } },
    })
      .composite(rendered.map((buf, i) => ({ input: buf, raw: { width: FRAME_SIZE, height: FRAME_SIZE, channels: 4 as const }, left: i * FRAME_SIZE, top: 0 })))
      .png(
        palette
          ? { palette: true, quality: 100, effort: 10, compressionLevel: 9 }
          : { compressionLevel: 9, adaptiveFiltering: true },
      )
      .toBuffer();
    await writeFile(join(OUT_DIR, species, `${a}.png`), strip);
    if (previews) {
      // Animated WebP for eyeballing timing (QA only, not part of the asset contract).
      const pngs = await Promise.all(
        rendered.map((buf) => sharp(buf, { raw: { width: FRAME_SIZE, height: FRAME_SIZE, channels: 4 } }).png().toBuffer()),
      );
      await mkdir(join(QA_DIR, species), { recursive: true });
      await sharp(pngs, { join: { animated: true } })
        .webp({ lossless: true, loop: 0, delay: Math.round(1000 / ANIMATIONS[a].fps) })
        .toFile(join(QA_DIR, species, `${a}.webp`));
    }
    const variants = p.frames.map((f) => `f${f.k}v${f.variant}`).join(" ");
    qa.push({ species, activity: a, variants, frames: n, scale: lay.scale, shifts: p.shifts, issues: p.issues, edgePixels: edge, greenFringe: green });
    const flag = p.issues.length || edge || green > 50 ? "WARN" : "ok  ";
    console.log(
      `[${flag}] ${species}/${a} (${variants}): ${n} frames, scale ${lay.scale.toFixed(3)}, shifts ${p.shifts.map(([x, y]) => `${x},${y}`).join(" ")}, edge px ${edge}, green px ${green}` +
        (p.issues.length ? `\n        - ${p.issues.join("\n        - ")}` : ""),
    );
  }
}

// ---------------------------------------------------------------------------
// Manifest + validation
// ---------------------------------------------------------------------------

async function buildManifest(): Promise<SpriteManifest> {
  const manifest: SpriteManifest = { version: 1, frameSize: FRAME_SIZE, species: {} };
  for (const s of SPECIES) {
    const animations: Partial<Record<Activity, SpriteAnimation>> = {};
    for (const a of ACTIVITIES) {
      const file = join(OUT_DIR, s, `${a}.png`);
      if (!(await Bun.file(file).exists())) continue;
      const meta = await sharp(file).metadata();
      if (!meta.width || meta.height !== FRAME_SIZE || meta.width % FRAME_SIZE !== 0) {
        console.warn(`[skip] ${s}/${a}.png has bad dimensions ${meta.width}x${meta.height}`);
        continue;
      }
      const spec = ANIMATIONS[a];
      animations[a] = { src: `${s}/${a}.png`, frames: meta.width / FRAME_SIZE, fps: spec.fps, loop: spec.loop };
    }
    if (Object.keys(animations).length) manifest.species[s] = { skins: ["default"], animations };
  }
  return manifest;
}

async function check(): Promise<string[]> {
  const errors: string[] = [];
  const m = (await Bun.file(MANIFEST_PATH).json()) as SpriteManifest;
  if (m.version !== 1) errors.push(`version ${String(m.version)} != 1`);
  if (!Number.isInteger(m.frameSize) || m.frameSize <= 0) errors.push(`bad frameSize ${m.frameSize}`);
  for (const [s, entry] of Object.entries(m.species)) {
    if (!(SPECIES as readonly string[]).includes(s)) errors.push(`unknown species ${s}`);
    if (!entry || !Array.isArray(entry.skins) || !entry.skins.includes("default")) errors.push(`${s}: skins must include "default"`);
    for (const [a, anim] of Object.entries(entry?.animations ?? {})) {
      const where = `${s}/${a}`;
      if (!(ACTIVITIES as readonly string[]).includes(a)) errors.push(`${where}: unknown activity`);
      if (!anim) continue;
      if (!(anim.frames >= 1 && Number.isInteger(anim.frames))) errors.push(`${where}: bad frames`);
      if (!(anim.fps > 0)) errors.push(`${where}: bad fps`);
      if (typeof anim.loop !== "boolean") errors.push(`${where}: loop must be boolean`);
      const file = join(OUT_DIR, anim.src);
      if (!(await Bun.file(file).exists())) {
        errors.push(`${where}: missing ${anim.src}`);
        continue;
      }
      const { data, info } = await sharp(file).raw().toBuffer({ resolveWithObject: true });
      const meta = await sharp(file).metadata();
      if (meta.format !== "png") errors.push(`${where}: not a PNG`);
      if (info.channels !== 4 || !meta.hasAlpha) errors.push(`${where}: no alpha channel (needs RGBA or indexed + tRNS)`);
      if (info.width !== anim.frames * m.frameSize) errors.push(`${where}: width ${info.width} != ${anim.frames} x ${m.frameSize}`);
      if (info.height !== m.frameSize) errors.push(`${where}: height ${info.height} != ${m.frameSize}`);
      if (info.channels === 4) {
        let transparent = 0;
        for (let i = 3; i < data.length; i += 4) if (data[i] === 0) transparent++;
        const frac = transparent / (info.width * info.height);
        if (frac < 0.3) errors.push(`${where}: only ${(frac * 100).toFixed(1)}% transparent (background not keyed?)`);
        for (let f = 0; f < anim.frames; f++) {
          for (const [x, y] of [
            [0, 0],
            [m.frameSize - 1, 0],
            [0, m.frameSize - 1],
            [m.frameSize - 1, m.frameSize - 1],
          ] as const) {
            const i = (y * info.width + f * m.frameSize + x) * 4 + 3;
            if (data[i] !== 0) errors.push(`${where}: frame ${f} corner (${x},${y}) not transparent`);
          }
        }
      }
    }
  }
  return errors;
}

// ---------------------------------------------------------------------------
// QA contact sheets
// ---------------------------------------------------------------------------

function checker(w: number, h: number, light: boolean): Buffer {
  const buf = Buffer.alloc(w * h * 4);
  const [c1, c2] = light ? [236, 214] : [40, 52];
  for (let y = 0; y < h; y++)
    for (let x = 0; x < w; x++) {
      const v = ((x >> 4) + (y >> 4)) % 2 ? c1 : c2;
      const i = (y * w + x) * 4;
      buf[i] = buf[i + 1] = buf[i + 2] = v;
      buf[i + 3] = 255;
    }
  return buf;
}

async function contactSheet(species: Species): Promise<void> {
  const rows: { a: Activity; file: string; frames: number }[] = [];
  for (const a of ACTIVITIES) {
    const file = join(OUT_DIR, species, `${a}.png`);
    if (!(await Bun.file(file).exists())) continue;
    const meta = await sharp(file).metadata();
    rows.push({ a, file, frames: (meta.width ?? 0) / FRAME_SIZE });
  }
  if (!rows.length) return;
  const maxFrames = Math.max(...rows.map((r) => r.frames));
  const labelW = 120;
  const half = labelW + maxFrames * FRAME_SIZE;
  const W = half * 2 + 16;
  const H = rows.length * FRAME_SIZE;
  const layers: OverlayOptions[] = [];
  const lines: string[] = [];
  rows.forEach((r, i) => {
    for (const [k, light] of [
      [0, true],
      [1, false],
    ] as const) {
      const x0 = k * (half + 16) + labelW;
      layers.push({ input: checker(r.frames * FRAME_SIZE, FRAME_SIZE, light), raw: { width: r.frames * FRAME_SIZE, height: FRAME_SIZE, channels: 4 }, left: x0, top: i * FRAME_SIZE });
      layers.push({ input: r.file, left: x0, top: i * FRAME_SIZE });
      lines.push(`<line x1="${x0}" y1="${i * FRAME_SIZE + BASELINE + 0.5}" x2="${x0 + r.frames * FRAME_SIZE}" y2="${i * FRAME_SIZE + BASELINE + 0.5}" stroke="#ff3355" stroke-width="1" opacity="0.6"/>`);
      for (let f = 1; f < r.frames; f++) lines.push(`<line x1="${x0 + f * FRAME_SIZE + 0.5}" y1="${i * FRAME_SIZE}" x2="${x0 + f * FRAME_SIZE + 0.5}" y2="${(i + 1) * FRAME_SIZE}" stroke="#3366ff" stroke-width="1" opacity="0.5"/>`);
    }
    lines.push(`<text x="8" y="${i * FRAME_SIZE + 132}" font-family="Arial, sans-serif" font-size="22" fill="#111">${r.a}</text>`);
  });
  const svg = Buffer.from(`<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}">${lines.join("")}</svg>`);
  layers.push({ input: svg, left: 0, top: 0 });
  await mkdir(QA_DIR, { recursive: true });
  await sharp({ create: { width: W, height: H, channels: 4, background: { r: 255, g: 255, b: 255, alpha: 1 } } })
    .composite(layers)
    .png()
    .toFile(join(QA_DIR, `${species}.png`));
  console.log(`[qa ] .cache/qa/${species}.png`);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

const { values } = parseArgs({
  args: Bun.argv.slice(2),
  options: {
    species: { type: "string" },
    activities: { type: "string" },
    check: { type: "boolean", default: false },
    "no-qa": { type: "boolean", default: false },
    clean: { type: "boolean", default: false },
    /** Write truecolor RGBA (PNG colour type 6) instead of the default indexed PNG + tRNS alpha. */
    rgba: { type: "boolean", default: false },
  },
  strict: true,
});

if (import.meta.main) {
  if (!values.check) {
    const overrides = await loadOverrides();
    const speciesList = parseList<Species>(values.species, SPECIES, "species");
    const activities = parseList<Activity>(values.activities, ACTIVITIES, "activity");
    const qa: StripQa[] = [];
    for (const s of speciesList) {
      // Remove outputs that are explicitly skipped so the manifest never lists rejected art.
      for (const a of activities) {
        if (overrides[s]?.[a]?.skip) await rm(join(OUT_DIR, s, `${a}.png`), { force: true });
      }
      if (values.clean) {
        const dir = join(OUT_DIR, s);
        for (const f of await readdir(dir).catch(() => [] as string[])) {
          if (f.endsWith(".png") && activities.includes(f.slice(0, -4) as Activity)) await rm(join(dir, f), { force: true });
        }
      }
      await processSpecies(s, activities, overrides, qa, !values["no-qa"], !values.rgba);
      if (!values["no-qa"]) await contactSheet(s);
    }
    await mkdir(QA_DIR, { recursive: true });
    await writeFile(join(QA_DIR, "report.json"), JSON.stringify(qa, null, 2) + "\n");
    const manifest = await buildManifest();
    await writeFile(MANIFEST_PATH, JSON.stringify(manifest, null, 2) + "\n");
    const count = Object.values(manifest.species).reduce((n, e) => n + Object.keys(e?.animations ?? {}).length, 0);
    console.log(`[out] assets/pets/manifest.json: ${Object.keys(manifest.species).length} species, ${count} animations`);
  }
  const errors = await check();
  if (errors.length) {
    console.error(`[check] FAIL (${errors.length}):\n  - ${errors.join("\n  - ")}`);
    process.exit(1);
  }
  console.log("[check] manifest + PNGs valid");
}
