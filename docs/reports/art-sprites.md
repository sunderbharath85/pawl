Renamed to Krypto on 2026-11-23; names below are pre-rename.

# art-sprites report

Status: **DONE**. All four species have the full set of 9 animations (36 strips): dog, cat, bird
and robot. Each is 4 frames of 256 px. `assets/pets/manifest.json` is valid, and pet-engine's own
`parseSpriteManifest` accepts every entry.

## Deliverables

| Path | What |
|---|---|
| `assets/pets/manifest.json` | `{version:1, frameSize:256, species:{dog,cat,bird,robot}}`. The skins list is `["default"]` and each species has 9 animations. The file is rebuilt from the PNGs that exist, so it only lists real files. |
| `assets/pets/<species>/<activity>.png` | Horizontal strips, **indexed PNG + tRNS alpha** (colour type 3, as the orchestrator decided), 1024x256 (4 frames x 256). They decode to RGBA. Background transparent, character faces right. Feet sit on the baseline at y=246, with 10 px of padding below. |
| `scripts/art/package.json` | `@ai-pet/art` (private, outside the root workspaces, own `bun.lock`). Deps: `sharp ^0.35.5`, dev `typescript ^5.9`, `@types/bun`. |
| `scripts/art/config.ts` | Paths, model, chroma and layout constants, a design bible per species, and the pose table per animation (frame 1 description plus 3 edit deltas, fps, loop, align mode, size bias, hop). Also the prompt builders and the overrides schema. Species is type-imported from `@ai-pet/protocol` via tsconfig `paths`. |
| `scripts/art/gemini.ts` | Minimal client for the Interactions API. It reads the key from `.env` and sends it only in a header (never logged). Retries 429/5xx and responses with no image, with backoff. Scrubs the key out of error text. |
| `scripts/art/cache.ts` | Append-only raw cache: `.cache/raw/<species>/<name>.v<N>.{jpg,json}`. The json holds prompt, refs, base variant and token usage. |
| `scripts/art/generate.ts` | Generation CLI: reference sheet, then frame 1 of each animation, then frames 2..N as edits of frame 1. Uses a concurrency semaphore, and `--frames/--regen/--note/--dry-run`. |
| `scripts/art/process.ts` | Chroma key and despill, component cleanup, registration, size normalisation, baseline alignment and packing. Also writes the manifest, runs the `--check` gate and produces the QA sheets and WebP previews. Output is palette PNG by default; `--rgba` writes truecolor instead. |
| `scripts/art/overrides.json` | Review decisions, currently just `cat.idle.flip` (the model drew the cat's idle facing left). |
| `scripts/art/README.md` | Usage, pipeline design and the review loop. |
| `scripts/art/.gitignore` | Ignores `node_modules/` and `.cache/`. |

## Commands run and results

- `bash scripts/bun-locked.sh install --cwd scripts/art` installed 10 packages (sharp 0.35.5, libvips 8.18.7).
- `bunx tsc --noEmit -p .` (in scripts/art) passes clean.
- `bun run generate.ts --species dog|cat|bird|robot` ran the generation. There were 0 API failures. **165 image calls** in total: 4 reference sheets, 144 frames, the first strip experiment (7 calls), and targeted regenerations (dog napping, dog alert f2/f4, bird napping).
- Estimated spend from the logged usage (364k input tokens, 274k output tokens) is **about $7**. The upper bound is $8.8, if every output token is priced at the image rate.
- `bun run process.ts` ran on all 36 strips: 0 edge-touching pixels and 0 green-fringe pixels. The only warning is `dog/working` being "shrunk to fit", because the dirt spray is wide; it is still the same visual size as idle.
- `bun run process.ts --check` reports `[check] manifest + PNGs valid`. It checks version, frameSize, known species and activities, that each file exists and is a PNG that carries alpha (tRNS or RGBA) and decodes to 4 channels, that width = frames x frameSize and height = frameSize, that each frame's corners are transparent, and that at least 30% of pixels are transparent.
- Consumer check: `packages/pet-engine/src/sprites.ts` `parseSpriteManifest()` keeps 36/36 animations (dog, cat, bird, robot).

- Secret scan: the actual `GEMINI_API_KEY` value appears 0 times across the 428 files in `scripts/art`, `assets/pets` and `docs/reports`. `.cache/` is gitignored.

## Pipeline decisions (worth knowing)

- **Model and API.** `gemini-nano-banana-2.1` (the current Nano Banana workhorse) through
  `POST /v1beta/interactions` with `Api-Revision: 2026-05-20`, per ai.google.dev image-generation docs.
  Image output is JPEG.
- **One image per frame, with frames 2..N as edits of frame 1.** The first attempt was one 4:1 image
  holding 4 poses per animation. The model consistently drew "animation strips" with motion blur, smears
  and ghost duplicates. Editing frame 1 keeps the character, scale and canvas position identical, so
  registration shifts came out 0,0 on almost every frame. It also allows regenerating a single bad frame
  (`--frames N --regen`).
- **Chroma.** The background is #00FF00 and the designs contain no green. Alpha comes from
  spill = G − max(R,B), ramped from 12 to 72, with despill both before and after resampling. Zoomed
  checks on dark, white and blue backgrounds show no halo.
- **Size.** Each species' idle is fitted to 80% of the frame. The other animations are matched to idle's
  body area, so the pet does not change size when switching animation.
- **Motion.** `alert` (bounce of 8 px) and `happy` (jump of 0/14/22/0 px) get a procedural hop, because the
  model kept the character mostly grounded. The other animations snap the feet to the baseline.
- **Readability.** Props are outlined or coloured to show on both light and dark desktops: lavender
  outlined "z", orange sound lines, white thought bubble with an outline.
- **QA artefacts** (not committed, in `scripts/art/.cache/qa/`): a contact sheet per species, animated
  WebP previews, and `report.json`.

## Known gaps / nits

- Every animation is 4 frames. The walk cycle is readable but basic; 6–8 frames would be smoother and
  could be added per animation by extending its `edits` list.
- `cat/alert` frames 2 and 4 drop the "!" mark, so it reads as a blinking alert, which is fine.
  The robot's `happy` frame 4 has faint grey motion ticks. Both are cosmetic; regenerating
  a frame is a single command (`--frames N --regen`).
- Only the `default` skin exists. There is no Rive version; these are sprite strips only.
- Size: **2.3 MB** for all 36 strips (about 62 KB each) as palette PNG. Truecolor via `--rgba` is 6.7 MB.

## Orchestrator decisions (applied)

1. **Palette PNG is now the default.** `process.ts` writes indexed PNG + tRNS; `--rgba` opts out.
   All 36 strips were rebuilt: colour type 3 with tRNS (36/36), 0 green-fringe pixels, `--check` passes,
   and pet-engine `parseSpriteManifest` keeps 36/36. Total size went from 6.85 MB to 2.3 MB.
   Spot-check against the truecolor strips:
   - Every strip: mean premultiplied error is 1.9/255. 188 pixels (0.006%) are off by more than 24/255,
     at most 21 in any 262k-pixel strip. They are isolated interior anti-alias pixels (blush and red tones
     snapped to the nearest palette entry), not edges or clusters.
   - 3x zooms of robot/happy (cyan glow), dog/napping (lavender), cat/idle (grey stripes) and
     bird/happy (sparkles) on dark and white backgrounds show no visible banding and no halo.
   - The dither setting (1.0, 0.5, 0) made no measurable difference, so the default is kept.
2. **frameSize stays at 256.**
3. **`scripts/art/bun.lock` is kept** and should be committed with the package.
