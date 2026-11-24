// Build Pawl for VS Code with Bun's bundler.
//   dist/extension.js  — extension host (node, CommonJS, `vscode` external)
//   dist/webview.js    — webview UI (browser, IIFE) + dist/webview.css
//   dist/media/pets/   — sprite sheets copied from repo-root assets/pets (optional)
// Usage: bun run scripts/build.ts [--watch] [--dev]
import { watch } from "node:fs";
import { cp, copyFile, mkdir, rm, stat } from "node:fs/promises";
import { extname, join, resolve } from "node:path";

const APP = resolve(import.meta.dir, "..");
const ROOT = resolve(APP, "..", "..");
const DIST = join(APP, "dist");
const PETS_SRC = join(ROOT, "assets", "pets");
const PETS_DST = join(DIST, "media", "pets");
const ASSET_EXTS = new Set([".png", ".webp", ".gif", ".svg", ".json", ".riv"]);

const args = new Set(process.argv.slice(2));
const isWatch = args.has("--watch");
const isDev = isWatch || args.has("--dev");

type BuildConfig = Parameters<typeof Bun.build>[0];

async function bundle(label: string, config: BuildConfig): Promise<boolean> {
  try {
    const result = await Bun.build({ ...config, throw: false } as BuildConfig);
    for (const log of result.logs) {
      if (log.level === "error") console.error(`[${label}]`, log);
      else if (log.level === "warning") console.warn(`[${label}]`, log);
    }
    if (!result.success) {
      console.error(`✗ ${label} failed`);
      return false;
    }
    for (const out of result.outputs) {
      if (out.kind === "sourcemap") continue;
      console.log(`✓ ${label}: ${out.path.slice(APP.length + 1)} (${(out.size / 1024).toFixed(1)} KiB)`);
    }
    return true;
  } catch (err) {
    console.error(`✗ ${label} failed`, err);
    return false;
  }
}

async function exists(p: string): Promise<boolean> {
  try {
    await stat(p);
    return true;
  } catch {
    return false;
  }
}

async function copyPets(): Promise<void> {
  await rm(PETS_DST, { recursive: true, force: true });
  if (!(await exists(PETS_SRC))) {
    console.log("• assets/pets not found — webview falls back to built-in SVG pets");
    return;
  }
  await mkdir(PETS_DST, { recursive: true });
  await cp(PETS_SRC, PETS_DST, {
    recursive: true,
    filter: async (src) => {
      const s = await stat(src);
      return s.isDirectory() || ASSET_EXTS.has(extname(src).toLowerCase());
    },
  });
  const hasManifest = await exists(join(PETS_DST, "manifest.json"));
  console.log(`✓ pets: copied assets/pets → dist/media/pets${hasManifest ? "" : " (no manifest.json yet)"}`);
}

async function buildAll(clean: boolean): Promise<boolean> {
  const started = performance.now();
  if (clean) await rm(DIST, { recursive: true, force: true });
  await mkdir(DIST, { recursive: true });
  const define = { "process.env.NODE_ENV": JSON.stringify(isDev ? "development" : "production") };

  const [ext, web] = await Promise.all([
    bundle("extension", {
      entrypoints: [join(APP, "src", "extension.ts")],
      outdir: DIST,
      naming: "extension.js",
      target: "node",
      format: "cjs",
      // `vscode` is provided by the extension host; ws' native speedups are optional and not shipped.
      external: ["vscode", "bufferutil", "utf-8-validate"],
      sourcemap: "linked",
      minify: !isDev,
    }),
    bundle("webview", {
      entrypoints: [join(APP, "src", "webview", "main.tsx")],
      outdir: DIST,
      naming: "webview.js",
      target: "browser",
      format: "iife",
      sourcemap: "linked",
      minify: !isDev,
      define,
    }),
  ]);
  await copyFile(join(APP, "src", "webview", "webview.css"), join(DIST, "webview.css"));
  await copyPets();
  const ok = ext && web;
  console.log(`${ok ? "✓ build ok" : "✗ build failed"} in ${Math.round(performance.now() - started)} ms`);
  return ok;
}

const ok = await buildAll(true);

if (isWatch) {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let running = false;
  let again = false;
  const trigger = () => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(async () => {
      if (running) {
        again = true;
        return;
      }
      running = true;
      do {
        again = false;
        await buildAll(false);
      } while (again);
      running = false;
    }, 120);
  };
  const watched = [join(APP, "src"), join(ROOT, "packages")];
  if (await exists(PETS_SRC)) watched.push(PETS_SRC);
  for (const dir of watched) {
    watch(dir, { recursive: true }, (_event, file) => {
      if (file && /node_modules|[\\/]dist[\\/]/.test(String(file))) return;
      trigger();
    });
  }
  console.log(`watching ${watched.map((d) => d.slice(ROOT.length + 1)).join(", ")} …`);
} else if (!ok) {
  process.exit(1);
}
