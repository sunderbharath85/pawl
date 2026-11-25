import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { createReadStream, existsSync, statSync } from "node:fs";
import { cp } from "node:fs/promises";
import { dirname, extname, join, normalize, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
/** Sprite sheets + manifest written by art-sprites (repo-root `assets/pets`), and the reaction icons (`assets/icons`). */
const PETS_DIR = resolve(here, "../../assets/pets");
const ICONS_DIR = resolve(here, "../../assets/icons");
/** URL prefix -> folder, served in dev and copied into the build. */
const ASSET_DIRS: ReadonlyArray<[string, string]> = [
  ["/pets", PETS_DIR],
  ["/icons", ICONS_DIR],
];
const host = process.env.TAURI_DEV_HOST;

const MIME: Record<string, string> = {
  ".json": "application/json",
  ".png": "image/png",
  ".webp": "image/webp",
  ".gif": "image/gif",
  ".svg": "image/svg+xml",
  ".riv": "application/octet-stream",
};

/**
 * Serve `assets/pets` at `/pets/` and `assets/icons` at `/icons/` in dev, and copy them to `dist/pets` and
 * `dist/icons` on build. Missing assets are fine: the engine falls back to its built-in SVG pets, and a reply's
 * reaction to its emoji.
 */
function petAssets(): Plugin {
  let outDir = resolve(here, "dist");
  return {
    name: "pawl-assets",
    configResolved(cfg) {
      outDir = resolve(cfg.root, cfg.build.outDir);
    },
    configureServer(server) {
      for (const [prefix, dir] of ASSET_DIRS) {
        server.middlewares.use(prefix, (req, res, next) => {
          const urlPath = decodeURIComponent((req.url ?? "/").split("?")[0] ?? "/");
          const file = normalize(join(dir, urlPath));
          if (!file.startsWith(dir + sep) || !existsSync(file) || !statSync(file).isFile()) {
            res.statusCode = 404;
            res.end();
            return;
          }
          res.setHeader("Content-Type", MIME[extname(file).toLowerCase()] ?? "application/octet-stream");
          res.setHeader("Cache-Control", "no-cache");
          createReadStream(file).on("error", next).pipe(res);
        });
      }
    },
    async closeBundle() {
      for (const [prefix, dir] of ASSET_DIRS) {
        if (existsSync(dir)) await cp(dir, join(outDir, prefix.slice(1)), { recursive: true });
      }
    },
  };
}

// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [react(), tailwindcss(), petAssets()],
  resolve: {
    // shadcn/ui components import from "@/…".
    alias: { "@": resolve(here, "src") },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
    fs: { allow: [resolve(here, "../..")] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13",
    minify: process.env.TAURI_ENV_DEBUG ? false : "oxc",
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    rolldownOptions: {
      input: {
        pet: resolve(here, "index.html"),
        panel: resolve(here, "panel.html"),
      },
    },
  },
});
