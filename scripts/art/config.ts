/**
 * Shared configuration for the AI Pet Hub sprite pipeline.
 *
 * Contract (docs/CONTRACTS.md "Pet sprite assets"):
 *   assets/pets/<species>/<activity>.png  horizontal strip, RGBA, square frames of
 *   `FRAME_SIZE`, feet on a shared baseline, character facing right.
 *   assets/pets/manifest.json lists only the animations that exist.
 */
import { join, resolve } from "node:path";
import type { Species } from "@ai-pet/protocol";

// `Species` comes from @ai-pet/protocol (type-only import, resolved via tsconfig "paths"
// because scripts/art is outside the root workspaces). The runtime list below must cover
// every Species: the assertion fails typechecking if the protocol adds one.
export const SPECIES = ["dog", "cat", "bird", "robot"] as const satisfies readonly Species[];
type _SpeciesCovered = Exclude<Species, (typeof SPECIES)[number]> extends never ? true : never;
export const SPECIES_COVERED: _SpeciesCovered = true;

/** `PetActivity` from @ai-pet/pet-engine plus "walk". Order = manifest order. */
export const ACTIVITIES = [
  "idle",
  "walk",
  "napping",
  "thinking",
  "working",
  "planning",
  "alert",
  "happy",
  "sad",
] as const;
export type Activity = (typeof ACTIVITIES)[number];

export const FRAME_SIZE = 256;
/** Feet line, in px from the top of each frame. */
export const BASELINE = FRAME_SIZE - 10;
/** Minimum transparent margin kept around content inside a frame. */
export const PAD = 6;
/** Fraction of FRAME_SIZE the idle pose's larger bbox side is fitted to. */
export const IDLE_FILL = 0.8;

export const ART_DIR = import.meta.dir;
export const REPO_ROOT = resolve(ART_DIR, "..", "..");
export const ENV_PATH = join(REPO_ROOT, ".env");
export const CACHE_DIR = join(ART_DIR, ".cache");
export const RAW_DIR = join(CACHE_DIR, "raw");
export const QA_DIR = join(CACHE_DIR, "qa");
export const OUT_DIR = join(REPO_ROOT, "assets", "pets");
export const MANIFEST_PATH = join(OUT_DIR, "manifest.json");
export const OVERRIDES_PATH = join(ART_DIR, "overrides.json");

// ---------------------------------------------------------------------------
// Gemini
// ---------------------------------------------------------------------------

/** Current Nano Banana workhorse (ai.google.dev/gemini-api/docs/image-generation). */
export const MODEL = "gemini-nano-banana-2.1";
export const API_URL = "https://generativelanguage.googleapis.com/v1beta/interactions";
export const API_REVISION = "2026-05-20";
export type ImageSize = "512" | "1K" | "2K" | "4K";
/** Every animation frame is its own square image (base pose, then edits of it). */
export const FRAME_ASPECT = "1:1";
export const FRAME_GEN_SIZE: ImageSize = "1K";
export const REFERENCE_ASPECT = "16:9";
export const REFERENCE_SIZE: ImageSize = "2K";

// ---------------------------------------------------------------------------
// Chroma key
// ---------------------------------------------------------------------------

/** Pure green screen. Characters/props are designed with no green so the key is safe. */
export const CHROMA_HEX = "#00FF00";
/** spill = G - max(R, B). <= SPILL_LO fully opaque, >= SPILL_HI fully transparent. */
export const SPILL_LO = 12;
export const SPILL_HI = 72;

// ---------------------------------------------------------------------------
// Art direction
// ---------------------------------------------------------------------------

export const STYLE = [
  "Cute, cozy 2D cartoon sticker art for a desktop pet game.",
  "Chibi proportions: big round head, small chubby body, short legs, large expressive glossy eyes.",
  "Clean, bold, uniform dark-brown outline (#3b2a20) around the whole character and every prop; flat cel shading with at most one soft shade tone per color; no gradients, no texture, no fine detail.",
  "Simple, chunky shapes with a strong silhouette that stays readable when shrunk to 64 pixels.",
  "Warm, friendly palette.",
  "A single crisp, static illustration: no motion blur, no smears, no afterimages, no ghosted duplicate limbs or tails, no speed streaks, no transparency or fading effects.",
].join(" ");

export const BACKGROUND_RULES = [
  `Background: one solid, perfectly flat pure chroma-key green (${CHROMA_HEX}) filling the entire image edge to edge.`,
  "No floor, no ground line, no cast shadows, no gradient, no vignette, no texture, no border, no frame lines, no grid, no panels, no frame numbers, no captions, no labels, no watermark.",
  "Never use any green or yellow-green color anywhere on the character or props.",
].join(" ");

export interface SpeciesDesign {
  label: string;
  /** Canonical design bible, repeated verbatim in every prompt. */
  design: string;
  /** Full-body standing description for the reference sheet. */
  stance: string;
  /** What touches the ground ("paws", "feet"). */
  feet: string;
}

export const DESIGNS: Record<Species, SpeciesDesign> = {
  dog: {
    label: "puppy",
    design:
      "a small chubby puppy with golden-tan fur, a cream muzzle, cream chest and cream paw tips, " +
      "soft floppy ears a shade darker tan, a short fluffy tail with a cream tip, a round black nose, " +
      "big round dark-brown eyes with white sparkle highlights, rosy cheek blush, and a red collar with a round gold tag",
    stance: "standing on all four short legs",
    feet: "paws",
  },
  cat: {
    label: "kitten",
    design:
      "a small chubby kitten with soft blue-grey fur, darker grey tabby stripes on the forehead and tail, " +
      "a white muzzle, white chest and white paws, pointed ears with soft pink insides, a tiny soft pink nose, " +
      "big round amber eyes with white sparkle highlights, rosy cheek blush, a long curvy tail, and a blue collar with a small gold bell",
    stance: "standing on all four short legs",
    feet: "paws",
  },
  bird: {
    label: "bird",
    design:
      "a small round chubby songbird shaped like a fluffy ball, with a sky-blue body, a white face and white belly, " +
      "darker royal-blue wing tips and short tail feathers, a small orange beak, tiny orange legs and feet, " +
      "a little blue head tuft, big round black eyes with white sparkle highlights, and rosy cheek blush",
    stance: "standing on its two tiny orange legs",
    feet: "feet",
  },
  robot: {
    label: "robot",
    design:
      "a small cute round robot pet with a rounded cream-white shell body, a dark navy rounded screen for a face " +
      "showing two bright cyan oval eyes, a short antenna with a bright orange ball tip, small stubby cream arms " +
      "with round orange hands, two short stubby legs with rounded navy feet, and orange accent panels on its sides",
    stance: "standing on its two stubby legs",
    feet: "feet",
  },
};


/**
 * Frame alignment in process.ts:
 * - "register": shift each frame so its silhouette best overlaps frame 1 (x and y); the strip's
 *   lowest feet go on the baseline. Best for mostly-static poses (no jitter).
 * - "feet": x from registration, y snaps every frame's feet to the baseline (walk cycles).
 * - "shared": raw canvas coordinates, keeping real vertical motion (jumps, bounces).
 */
export type AlignMode = "register" | "feet" | "shared";

/** Frame 1 = `base` (a full scene description); frames 2..N = edits of frame 1. */
export interface PoseSet {
  /** Frame 1: the full pose/scene, drawn from the reference sheet. `{name}` = species label. */
  base: string;
  /** Frames 2..N: what changes relative to frame 1 (everything else stays identical). */
  edits: string[];
}

export interface ActivitySpec extends PoseSet {
  fps: number;
  loop: boolean;
  /** How frames are aligned to each other and to the baseline (see AlignMode). */
  align: AlignMode;
  /** Multiplier on the species-normalised size (props attached to the body inflate its area). */
  sizeBias: number;
  /**
   * Procedural lift per frame, in output px (positive = up), applied after alignment. Gives
   * jumps/bounces a clear arc even when the model keeps the character grounded.
   */
  hop?: number[];
  /** Species-specific replacements. */
  perSpecies?: Partial<Record<Species, Partial<PoseSet>>>;
}

export const ANIMATIONS: Record<Activity, ActivitySpec> = {
  idle: {
    fps: 6,
    loop: true,
    align: "register",
    sizeBias: 1,
    base: "the {name} sits calmly and contentedly, relaxed, eyes open, small smile",
    edits: [
      "the head is tilted very slightly upward and the ears are lifted a little, as if breathing in",
      "the eyes are closed in a happy blink (curved closed-eye lines); nothing else changes",
      "the tail is raised higher in a little wag; nothing else changes",
    ],
    perSpecies: {
      bird: { base: "the bird stands upright on both feet, relaxed, eyes open, small smile" },
      robot: {
        base: "the robot stands upright, arms relaxed at its sides, eyes open (two bright cyan ovals)",
        edits: [
          "the antenna is tilted slightly to one side and its orange ball tip is a little bigger",
          "the screen eyes are closed, shown as two short flat cyan lines (a blink); nothing else changes",
          "one stubby arm is raised slightly in a small wave; nothing else changes",
        ],
      },
    },
  },
  walk: {
    fps: 8,
    loop: true,
    align: "feet",
    sizeBias: 1,
    base:
      "the {name} walks toward the right in a cheerful side view, mid-step: front-left leg and back-right leg " +
      "reaching forward (contact pose), tail up",
    edits: [
      "the legs are now passing under the body (all four legs close together under the body, passing pose), body raised a tiny bit",
      "the opposite legs now reach forward: front-right leg and back-left leg forward (opposite contact pose)",
      "the legs are passing under the body again, body raised a tiny bit, tail bounced up",
    ],
    perSpecies: {
      bird: {
        base: "the bird hops toward the right in a cheerful side view, crouched slightly, about to hop, feet on the ground",
        edits: [
          "the bird is in a small hop: both feet just off the ground, body a little higher, wings slightly open",
          "the bird is landing: feet touching the ground again, body slightly squashed",
          "the bird stands tall after landing, tail feathers flicked up",
        ],
      },
      robot: {
        base: "the robot walks toward the right in a cheerful side view, front leg stepping forward, arms swinging",
        edits: [
          "both legs are together under the body, body raised a tiny bit",
          "the other leg is stepping forward and the arms swing the other way",
          "both legs are together again, body raised a tiny bit, antenna bounced",
        ],
      },
    },
  },
  napping: {
    fps: 3,
    loop: true,
    align: "register",
    sizeBias: 1.05,
    base:
      "the {name} is curled up asleep on a small round soft lavender cushion bed, eyes closed, peaceful smile, " +
      "with one small light-lavender letter 'z' with a bold dark-brown outline floating above its head",
    edits: [
      "the body is slightly puffed up (breathing in) and there are now two outlined light-lavender 'z' letters floating above, the second one higher and bigger",
      "the body is back to normal and there are now three outlined light-lavender 'z' letters rising above in a diagonal line, each bigger than the last",
      "the body is slightly flatter (breathing out) and the two outlined light-lavender 'z' letters float higher than before",
    ],
    perSpecies: {
      robot: {
        base:
          "the robot sits powered down on a small round lavender charging pad, screen eyes closed as two curved cyan lines, " +
          "with one small light-lavender letter 'z' with a bold dark-brown outline floating above its head",
      },
    },
  },
  thinking: {
    fps: 5,
    loop: true,
    align: "register",
    sizeBias: 0.95,
    base:
      "the {name} sits and ponders with its head tilted, ears perked up, curious look, with a small white cloud-shaped " +
      "thought bubble (dark-brown outline) floating above its head, empty",
    edits: [
      "the thought bubble now contains one dark dot",
      "the head is tilted a little the other way and the thought bubble contains two dark dots",
      "the eyes look upward and the thought bubble contains three dark dots",
    ],
  },
  working: {
    fps: 8,
    loop: true,
    align: "register",
    sizeBias: 1.05,
    base:
      "the {name} sits behind a tiny grey laptop placed in front of it (the laptop screen is light blue), " +
      "both front paws on the keyboard, focused happy face",
    edits: [
      "one front paw is raised above the keyboard mid-keystroke, with two short orange motion ticks next to it",
      "the other front paw is raised mid-keystroke instead, with two short orange motion ticks next to it",
      "both paws are on the keys and a tiny yellow spark star pops above the laptop",
    ],
    perSpecies: {
      dog: {
        base:
          "the puppy digs eagerly: standing on all fours, nose down, rear up, tail up, one front paw scooping at the " +
          "ground, with two small brown dirt clumps flying back behind it",
        edits: [
          "the other front paw is now scooping, and three brown dirt clumps fly back in a small arc behind it",
          "the first front paw scoops again, and the brown dirt clumps are higher in the arc behind it",
          "both front paws are on the ground for a moment, tail wagging higher, a few brown dirt clumps falling behind it",
        ],
      },
      bird: {
        base:
          "the bird stands behind a tiny grey laptop placed in front of it (the laptop screen is light blue), " +
          "looking at the screen, focused happy face",
        edits: [
          "the bird pecks a key with its beak, two short orange motion ticks next to the beak",
          "the head is raised between pecks, eyes on the screen",
          "the bird pecks again, wings slightly open, and a tiny yellow spark star pops above the laptop",
        ],
      },
      robot: {
        base:
          "the robot stands behind a tiny grey laptop placed in front of it (the laptop screen is light blue), " +
          "both orange hands on the keyboard, screen eyes as happy arcs",
        edits: [
          "one hand is raised above the keyboard mid-keystroke, with two short orange motion ticks next to it",
          "the other hand is raised mid-keystroke instead, with two short orange motion ticks next to it",
          "both hands on the keys and a tiny orange spark star pops above the laptop",
        ],
      },
    },
  },
  planning: {
    fps: 5,
    loop: true,
    align: "register",
    sizeBias: 1.05,
    base:
      "the {name} sits holding up a small white clipboard (brown clip) showing a checklist of three empty boxes with lines, " +
      "looking at it thoughtfully",
    edits: [
      "the first box on the clipboard now has a bold red check mark",
      "the first and second boxes on the clipboard now have bold red check marks, pleased face",
      "all three boxes on the clipboard now have bold red check marks, proud happy face with eyes closed in joy",
    ],
    perSpecies: {
      bird: {
        base:
          "the bird stands next to a small white clipboard (brown clip) leaning upright beside it, showing a checklist of " +
          "three empty boxes with lines, looking at it thoughtfully",
      },
    },
  },
  alert: {
    fps: 10,
    loop: true,
    align: "shared",
    hop: [0, 8, 0, 8],
    sizeBias: 1,
    base:
      "the {name} stands alert calling for attention, head up, mouth open wide, with a bold yellow exclamation mark '!' " +
      "(dark-brown outline) above its head",
    edits: [
      "the {name} is bouncing: front of the body lifted up off the ground in an excited hop, mouth open wide, three short bold orange sound lines in front of the mouth",
      "the {name} is back down standing alert, mouth half open, the yellow '!' slightly bigger",
      "the {name} bounces again: front of the body lifted off the ground, mouth open wide, three short bold orange sound lines in front of the mouth",
    ],
    perSpecies: {
      dog: {
        base:
          "the puppy barks urgently for attention, standing alert on all fours, ears up, mouth open wide, with a bold yellow " +
          "exclamation mark '!' (dark-brown outline) above its head",
      },
      robot: {
        base:
          "the robot beeps urgently for attention, standing alert, arms raised, antenna ball bright red, screen eyes wide, " +
          "with a bold yellow exclamation mark '!' (dark-brown outline) above its head",
      },
    },
  },
  happy: {
    fps: 10,
    loop: true,
    align: "shared",
    hop: [0, 14, 22, 0],
    sizeBias: 1,
    base:
      "the {name} is overjoyed, crouched down ready to jump, eyes closed in happy upside-down-U curves, big open smile, tail wagging",
    edits: [
      "the {name} is jumping up: the whole body is higher in the image with all legs off the ground, body stretched, tail wagging, big open smile",
      "the {name} is at the top of the jump, highest in the image, legs tucked, with two small pink hearts and two yellow sparkle stars around it",
      "the {name} has landed back on the ground, body slightly squashed, big open smile, tail wagging, one small pink heart above",
    ],
  },
  sad: {
    fps: 4,
    loop: true,
    align: "register",
    sizeBias: 1,
    base: "the {name} sits droopy and sad, head hung low, ears drooping, watery eyes, small frown, tail low",
    edits: [
      "the head sinks a little lower and a small light-blue tear drop forms at the corner of the eye",
      "the light-blue tear drop is rolling down the cheek, mouth in a small wobbly frown",
      "the head lifts slightly in a little sniffle, eyes squeezed shut, no tear",
    ],
    perSpecies: {
      bird: { base: "the bird stands droopy and sad, feathers deflated, head tuft drooping, watery eyes, small frown" },
      robot: {
        base: "the robot stands slumped and sad, antenna drooping, screen eyes shown as sad downturned cyan arcs",
        edits: [
          "the robot slumps a little lower and a small light-blue square pixel tear appears under one eye",
          "the light-blue pixel tear has moved down the screen",
          "the robot lifts slightly with a little sigh, antenna drooping further, no tear",
        ],
      },
    },
  },
};

export function frameCount(species: Species, activity: Activity): number {
  return 1 + posesFor(species, activity).edits.length;
}

export function posesFor(species: Species, activity: Activity): PoseSet {
  const spec = ANIMATIONS[activity];
  const o = spec.perSpecies?.[species];
  const label = DESIGNS[species].label;
  const fill = (s: string) => s.replaceAll("{name}", label);
  return { base: fill(o?.base ?? spec.base), edits: (o?.edits ?? spec.edits).map(fill) };
}

export function referencePrompt(species: Species): string {
  const d = DESIGNS[species];
  return [
    `Character reference sheet for a cute 2D desktop pet: ${d.design}.`,
    "Show exactly two full-body views of this one character side by side, evenly spaced, with plenty of empty background around each and nothing touching the image edges:",
    `left view: ${d.stance}, side view facing RIGHT (looking toward the right edge of the image);`,
    "right view: sitting, three-quarter view facing RIGHT, happy expression.",
    "Both views must be the identical character at the identical scale.",
    STYLE,
    BACKGROUND_RULES,
  ].join("\n");
}

/** Frame 1 of an animation: a single sticker drawn from the reference sheet (image 1). */
export function basePrompt(species: Species, activity: Activity, note?: string): string {
  const d = DESIGNS[species];
  const { base } = posesFor(species, activity);
  return [
    "A single static sticker illustration of one character for a 2D desktop-pet sprite.",
    `Character: ${d.design}.`,
    "Image 1 is this character's reference sheet: draw exactly this character with the same design, proportions, colors, markings and accessories.",
    `Pose: ${base}.`,
    "The character faces RIGHT (side or three-quarter view, nose toward the right edge of the image), is shown full body, centered horizontally, " +
      `and fills about half of the image height, with wide empty margins on every side; its ${d.feet} rest on an invisible ground line about 80% of the way down the image. ` +
      "Nothing touches the image edges.",
    STYLE,
    BACKGROUND_RULES,
    ...(note ? [`Important: ${note}`] : []),
  ].join("\n");
}

/** Frames 2..N: an edit of frame 1 (image 1), with the reference sheet as image 2. */
export function editPrompt(species: Species, activity: Activity, frame: number, note?: string): string {
  const d = DESIGNS[species];
  const { base, edits } = posesFor(species, activity);
  const change = edits[frame - 2];
  if (!change) throw new Error(`${species}/${activity} has no frame ${frame}`);
  return [
    `Image 1 is frame 1 of a sprite animation of this ${d.label} (${base}). Create frame ${frame} by editing image 1.`,
    `Change only this: ${change}.`,
    "Keep everything else exactly the same as image 1: the same character design, colors, outline weight, size, position on the canvas, framing, props, " +
      `facing RIGHT, ${d.feet} on the same ground line, and the same solid flat pure chroma-key green (${CHROMA_HEX}) background edge to edge.`,
    "Image 2 is the character's reference sheet, for design accuracy only.",
    "The result is one crisp, static illustration: no motion blur, no smears, no afterimages, no ghost copies, no speed streaks, no transparency or fading. " +
      "No shadows, no ground, no text or labels. Never use green on the character or props.",
    ...(note ? [`Important: ${note}`] : []),
  ].join("\n");
}

// ---------------------------------------------------------------------------
// Overrides (scripts/art/overrides.json) — human review decisions, committed.
// ---------------------------------------------------------------------------

export interface AnimOverride {
  /** Mirror horizontally (if the model drew the animation facing left). */
  flip?: boolean;
  /** Extra size multiplier after species normalisation. */
  scale?: number;
  /** Exclude from the output/manifest (e.g. rejected and not yet regenerated). */
  skip?: boolean;
  /** Drop these frames (1-based) from the strip. */
  dropFrames?: number[];
  /** Override ANIMATIONS[activity].align. */
  align?: AlignMode;
}
export interface VariantOverride {
  /** Raw cache variant to use (default: newest). */
  variant?: number;
}
/**
 * Keys: "reference" and "<activity>.f<N>" take a VariantOverride; "<activity>" takes an AnimOverride.
 */
export type Overrides = Partial<Record<Species, Record<string, AnimOverride & VariantOverride>>>;

export async function loadOverrides(): Promise<Overrides> {
  const f = Bun.file(OVERRIDES_PATH);
  if (!(await f.exists())) return {};
  return (await f.json()) as Overrides;
}

export function parseList<T extends string>(arg: string | undefined, all: readonly T[], what: string): T[] {
  if (!arg) return [...all];
  const out = arg.split(",").map((s) => s.trim()).filter(Boolean);
  for (const s of out) {
    if (!(all as readonly string[]).includes(s)) throw new Error(`unknown ${what} "${s}" (expected one of ${all.join(", ")})`);
  }
  return out as T[];
}
