# @krypto/art — pet sprite pipeline

Generates the animated pet sprite strips in `assets/pets/` (contract: `docs/CONTRACTS.md`,
"Pet sprite assets") with Google Gemini image generation, then keys, aligns and packs them.

```
assets/pets/manifest.json            { version: 1, frameSize: 256, species: { dog: { skins, animations } } }
assets/pets/<species>/<activity>.png horizontal strip (indexed PNG + tRNS alpha), frames x 256 wide, 256 tall,
                                     feet on a shared baseline (y = 246), facing right
```

Activities: `idle walk napping thinking working planning alert happy sad` (pet-engine's
`PetActivity` + `walk`). Species: `dog cat bird robot`. The manifest is rebuilt from the PNGs
that exist, so it only ever lists real animations.

## Setup

`scripts/art` is outside the root bun workspaces and has its own lockfile:

```bash
bash scripts/bun-locked.sh install --cwd scripts/art     # from the repo root
```

The Gemini key is read from `GEMINI_API_KEY` in the repo-root `.env` (falls back to the process
environment). It is only sent in the `x-goog-api-key` header; nothing logs or stores it.

## Usage (run inside `scripts/art`)

```bash
bun run generate.ts --species dog                 # reference sheet, then every frame of every animation (cached)
bun run generate.ts --species dog --activities sad --frames 3 --regen      # redo one bad frame
bun run generate.ts --species dog --activities walk --regen --note "..."   # redo a whole animation
bun run generate.ts --species cat --reference     # new reference-sheet variant
bun run generate.ts --dry-run                     # print prompts, no API calls
bun run process.ts                                # key/align/pack everything, write manifest, validate
bun run process.ts --species dog --activities walk
bun run process.ts --rgba                         # write 8-bit truecolor RGBA instead of the default indexed + alpha
bun run process.ts --check                        # gate only: manifest valid, PNGs exist, sizes, alpha
bunx tsc --noEmit -p .                            # typecheck
```

## How it works

1. **Model**: `gemini-nano-banana-2.1` through the Interactions API
   (`POST https://generativelanguage.googleapis.com/v1beta/interactions`, header
   `Api-Revision: 2026-05-20`, `response_format: {type: "image", aspect_ratio, image_size}`),
   per https://ai.google.dev/gemini-api/docs/image-generation. ~$0.034 per 1K image,
   ~$0.05 per 2K image. Retries 429/5xx with backoff; `store: false`.
2. **Reference sheet** (`generate.ts`): per species one 16:9 2K sheet (side view + sitting view,
   facing right) from the design bible in `config.ts`. Everything else is drawn from it.
3. **Frames**: each animation frame is its own 1:1 1K image.
   - Frame 1 = a single static sticker of the pose, with the reference sheet as image input.
   - Frames 2..N = **edits of frame 1** ("change only: ears lifted / eyes closed / next leg
     forward ..."), with frame 1 + the reference sheet as inputs. Editing keeps the character,
     scale and canvas position identical, so animations don't wobble.
   - Why not one multi-pose strip per call: tried first; the model renders "animation strips"
     with motion blur, smears and ghost copies (kept in `.cache/legacy-strips/` for reference).
4. **Prompts**: flat `#00FF00` chroma background, no green on the character, no
   shadows/ground/labels/motion blur; props are outlined so they read on light and dark desktops.
5. **Cache**: every call is saved as `.cache/raw/<species>/<name>.v<N>.<ext>` plus a `.json`
   with prompt, refs, base variant, model and token usage (`name` = `reference` or
   `<activity>.f<frame>`). Variants are append-only; reruns skip cached frames unless `--regen`.
   Edits derived from a frame-1 variant that is no longer selected are regenerated automatically.
6. **Processing** (`process.ts`):
   - chroma key: `spill = G - max(R, B)`; alpha ramps from opaque (spill <= 12) to clear
     (spill >= 72); green is despilled to `max(R, B)` so anti-aliased edges carry no halo
     (and again after resampling, for Lanczos ringing on soft edges);
   - 8-connected components; specks below 0.15% of the largest part are dropped; parts touching
     the image edge or a second character-sized part are reported as issues;
   - registration: frames 2..N are shifted onto frame 1 by maximising alpha overlap
     (coarse-to-fine search), so static parts never jitter;
   - alignment per animation (`ANIMATIONS[a].align`): `register` (most), `feet` (walk: feet snap
     to the baseline every frame), `shared` (alert/happy: keep real vertical motion), plus an
     optional procedural `hop` (px per frame) for bounces;
   - size: idle's bbox is fitted to 80% of the frame; every other animation is scaled so the
     character's body area matches idle (times `sizeBias`), then clamped to fit inside the frame;
   - Lanczos downscale (sharp premultiplies alpha), fringe below alpha 10 dropped, frames packed
     left to right; manifest rebuilt from the files and validated (`--check` logic).
   - encoding: indexed PNG (colour type 3) with a tRNS alpha table via libimagequant
     (`quality: 100`, `effort: 10`), about 3x smaller than truecolor. It decodes to RGBA in
     browsers, webviews and image libraries. Measured against the truecolor strips: mean error
     1.9/255; 0.006% of pixels off by more than 24/255, all isolated interior anti-aliasing pixels;
     no green fringe; edges visually identical at 3x zoom on dark and light. Use `--rgba` for
     colour type 6.
   - QA: `.cache/qa/<species>.png` contact sheet (light + dark checkerboard, baseline in red,
     frame separators in blue), `.cache/qa/<species>/<activity>.webp` animated previews, and
     `.cache/qa/report.json` (variants used, shifts, issues, edge-touching and green pixels).

## Review loop / overrides

Inspect `.cache/qa/<species>.png`, the previews and the raw frames. Decisions go in
`overrides.json` (committed), keyed by species, then `<activity>` or `<activity>.f<N>` or
`reference`:

```json
{
  "cat": {
    "idle": { "flip": true },
    "walk": { "scale": 1.05, "align": "feet", "dropFrames": [3] },
    "walk.f2": { "variant": 1 },
    "sad": { "skip": true }
  }
}
```

- `<activity>`: `flip` (mirror an animation drawn facing left), `scale` (extra size multiplier),
  `align` (override the alignment mode), `dropFrames` (1-based), `skip` (exclude from output and
  manifest)
- `<activity>.f<N>` / `reference`: `variant` pins a cached generation (default: newest)

Regenerate a bad frame with `generate.ts --activities <a> --frames <n> --regen [--note "..."]`,
then rerun `process.ts`.
