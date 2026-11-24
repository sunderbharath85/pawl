import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import { createReadStream, existsSync, statSync } from "node:fs";
import { cp } from "node:fs/promises";
import { dirname, extname, join, normalize, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
/** Sprite sheets + manifest written by art-sprites (repo-root `assets/pets`). */
const PETS_DIR = resolve(here, "../../assets/pets");
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
 * Serve `assets/pets` at `/pets/` in dev and copy it to `dist/pets` on build.
 * Missing assets are fine: the engine falls back to its built-in SVG pets.
 */
function petAssets(): Plugin {
  let outDir = resolve(here, "dist");
  return {
    name: "krypto-assets",
    configResolved(cfg) {
      outDir = resolve(cfg.root, cfg.build.outDir);
    },
    configureServer(server) {
      server.middlewares.use("/pets", (req, res, next) => {
        const urlPath = decodeURIComponent((req.url ?? "/").split("?")[0] ?? "/");
        const file = normalize(join(PETS_DIR, urlPath));
        if (!file.startsWith(PETS_DIR + sep) || !existsSync(file) || !statSync(file).isFile()) {
          res.statusCode = 404;
          res.end();
          return;
        }
        res.setHeader("Content-Type", MIME[extname(file).toLowerCase()] ?? "application/octet-stream");
        res.setHeader("Cache-Control", "no-cache");
        createReadStream(file).on("error", next).pipe(res);
      });
    },
    async closeBundle() {
      if (existsSync(PETS_DIR)) {
        await cp(PETS_DIR, join(outDir, "pets"), { recursive: true });
      }
    },
  };
}

// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [react(), petAssets()],
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
