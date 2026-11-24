// Set the product version everywhere it is declared (Rust workspace, desktop app, phone app, every package).
// Usage: bun scripts/set-version.ts 0.1.2   then build once so Cargo.lock follows, commit, and tag v0.1.2.
import { readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

const version = process.argv[2] ?? "";
if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$/.test(version)) {
  console.error("usage: bun scripts/set-version.ts <major.minor.patch[-pre]>");
  process.exit(2);
}
const root = resolve(import.meta.dir, "..");
const toml = ["Cargo.toml", "apps/desktop/src-tauri/Cargo.toml"];
const json = [
  "package.json",
  "apps/desktop/package.json",
  "apps/desktop/src-tauri/tauri.conf.json",
  "apps/mobile/package.json",
  "apps/vscode/package.json",
  "e2e/package.json",
  "packages/hub-client/package.json",
  "packages/pet-engine/package.json",
  "packages/protocol/package.json",
  "scripts/art/package.json",
  "services/relay/package.json",
];

function edit(file: string, re: RegExp, replacement: string) {
  const path = join(root, file);
  const text = readFileSync(path, "utf8");
  if (!re.test(text)) throw new Error(`no version found in ${file}`);
  writeFileSync(path, text.replace(re, replacement));
  console.log(`${file} -> ${version}`);
}

// The first `version = "..."` in [package] / [workspace.package].
for (const f of toml) edit(f, /^version = "[^"]*"/m, `version = "${version}"`);
// The top-level "version" of each manifest (the first one in the file).
for (const f of json) edit(f, /"version": "[^"]*"/, `"version": "${version}"`);
// Expo's app version (inside "expo").
edit("apps/mobile/app.json", /("expo":\s*\{[\s\S]*?"version": )"[^"]*"/, `$1"${version}"`);
