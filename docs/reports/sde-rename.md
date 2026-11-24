# sde-rename report: AI Pet Hub → Krypto (whole repo)

Branch `step-0.5-rename`, nothing committed. 162 files changed (+848 / −788), no files added or
removed apart from this report, and no new dependencies.

## What changed

Mechanical rename following the assignment's mapping, applied with an ordered perl rewrite over
`git ls-files` (lockfiles, `docs/reports/*` and binaries excluded), then hand fixes and a review.

| Kind | Old | New |
|---|---|---|
| Display name | `AI Pet Hub`, `AI Pet` | `Krypto`. Where the text means the daemon it says "the Krypto hub" (VS Code description and settings, hub-client errors, config template, `HUB_TERMINAL_NAME`, crate docs) |
| VS Code display name | `Pet for VS Code` | `Krypto for VS Code` |
| Rust crates | `pet-protocol`, `pet-acp`, `pet-discovery`, `pet-brain`, `pet-relay-link`, `pet-hub` | `krypto-protocol`, `krypto-acp`, `krypto-discovery`, `krypto-brain`, `krypto-relay-link`, `krypto-hub` (lib `krypto_hub`, **binary `krypto`**). All `use pet_*` paths and root `workspace.dependencies` updated |
| Desktop crate | `ai-pet-desktop` / `ai_pet_desktop_lib` | `krypto-desktop` / `krypto_desktop_lib` |
| npm | `@ai-pet/{protocol,hub-client,pet-engine,desktop,mobile,art,relay}`, `ai-pet-hub`, `ai-pet-vscode` | `@krypto/*`, `krypto`, `krypto-vscode` (also `scripts/art/tsconfig.json` paths) |
| VS Code contributions | `ai-pet.*` commands, settings, views, `ai-pet` container, `ai-pet.hasPending` context, `onWebviewPanel:ai-pet.petPanel`, `getConfiguration("ai-pet")`, globalState keys, `ai-pet.status`, `.vscode` task label | `krypto.*` / `krypto` everywhere |
| Data dir | `ai-pet-hub` (both `HUB_DATA_DIR_NAME`s, desktop `hub.rs`, hub-client/node, brain `HUB_SECRETS_GLOB`, tests) | `krypto` (`%APPDATA%\krypto`, `~/Library/Application Support/krypto`, `~/.local/share/krypto`) |
| Env vars | `AI_PET_HUB_TOKEN/URL/DATA_DIR/BIN`, `AI_PET_BRAIN_MODEL`, `AI_PET_OLLAMA_URL`, `AI_PET_OLLAMA_MODEL`, `AI_PET_REGISTRY_URL`, `AI_PET_HOOK_EVENT` | `KRYPTO_HUB_*`, with the same suffixes (`KRYPTO_HUB_BRAIN_MODEL`, `KRYPTO_HUB_REGISTRY_URL`, `KRYPTO_HUB_HOOK_EVENT`, ...) |
| URI scheme | `aipet://pair` (CONTRACTS, mobile parser + messages) | `krypto://pair` |
| Tauri | productName/window title `AI Pet Hub`, identifier `dev.aipet.hub.desktop` | `Krypto`, `dev.krypto.desktop` |
| Expo | name `AI Pet Hub`, slug `ai-pet-hub`, scheme `aipet`, bundle id / package `dev.aipet.hub` | `Krypto`, `krypto`, `krypto`, `dev.krypto.mobile` |
| Hub binary lookup | `pet-hub(.exe)` on PATH, next to the app, `Contents/Resources/pet-hub`, `target/{release,debug}/pet-hub` (desktop + VS Code), VS Code `hubExecutable` default | `krypto(.exe)` |
| Hook install | snippets call `<exe> hook <source>` (exe = `current_exe()`, so `krypto`). Copilot files `~/.copilot/hooks/ai-pet.json`, `.github/hooks/ai-pet.json`; hook meta key `_ai_pet`; temp suffix `.ai-pet-tmp` | `krypto.json`, `_krypto`, `.krypto-tmp` |
| Misc identifiers | localStorage `aipet.*`, `aipet-settings` event, mobile storage keys `aipet.*`, `ai-pet-nonce` meta, `ai-pet-engine-styles` style id, `__ai-pet-placeholder`, vite plugin `ai-pet-assets`, user agents `ai-pet-hub/`, `ai-pet-hub-brain/`, `ai-pet-desktop`, ACP `client_info` + thread name `ai-pet-hub`, test temp prefixes | `krypto*` equivalents (UA `krypto/<ver> (krypto-discovery)`, `krypto-brain/<ver>`, `krypto-desktop`; ACP client name `krypto`) |
| Docs | PLAN.md, CONTRACTS.md, `apps/vscode/README.md`, `scripts/art/README.md`, crate descriptions, CLI help | renamed. Each `docs/reports/*.md` got the first line "Renamed to Krypto on 2026-11-23; names below are pre-rename." and is otherwise untouched |
| License | | every `package.json` has `"license": "MIT"` (VS Code was `UNLICENSED`). All crates already had `license.workspace = true`, the root has `license = "MIT"` and `src-tauri` has `license = "MIT"` (verified, unchanged) |

### Hook-entry recognition (follow-up per t13: no pre-rename compatibility)

`crates/discovery/src/hooks/install.rs`: `is_ours` / `notify_is_ours` used to recognise our hook
entries by the current exe stem or the literal `"pet-hub"`. That literal is now
`HUB_BIN_NAME = "krypto"`, and both checks share one helper, `HubCmd::names_hub`. `pet-hub` is
**not** recognised any more: the project is pre-release and no hooks were ever installed. A new
test, `install_claude_replaces_entry_written_by_other_hub_copy`, covers the name fallback: an
existing `C:/tools/krypto.exe hook claude` entry is replaced, not duplicated, when
`install-hooks` runs from a differently named copy.

## Commands run and results

Lockfiles:
- `cargo build --workspace` regenerated `Cargo.lock`. The diff is only the 5 renamed packages moving (`pet-*` → `krypto-*`), with no version changes.
- `cargo check` in `apps/desktop/src-tauri` regenerated its own `Cargo.lock`. The diff is only `ai-pet-desktop` → `krypto-desktop` moving position.
- `bash scripts/bun-locked.sh install` updated the `@krypto/*` workspace entries in `bun.lock` ("Saved lockfile; 6 packages removed": the stale `@ai-pet` links).
- `bash scripts/bun-locked.sh install --cwd scripts/art` reported no changes.
- bun does not rewrite the root workspace `name` field when only that changes. I set it by hand in `bun.lock` (`krypto`) and `scripts/art/bun.lock` (`@krypto/art`), then verified both with `bash scripts/bun-locked.sh install --frozen-lockfile [--cwd scripts/art]`: "Checked 548 installs across 592 packages (no changes)" and "Checked 10 installs across 38 packages (no changes)".
- I removed 5 now-empty `node_modules/@ai-pet` directories (they were empty dirs, not links).

Gates (all pass):

| Gate | Result |
|---|---|
| `cargo build --workspace` | `Finished dev profile ... in 31.31s`, 0 warnings |
| `cargo test --workspace` | krypto_acp 22 passed · krypto_brain 54 passed · krypto_discovery 32 passed (+1 new) · krypto_hub 33 passed · krypto_protocol 2 passed; 0 failed |
| `cargo clippy --workspace --all-targets` | `Finished`, 0 warnings, 0 errors |
| `cargo check` (apps/desktop/src-tauri) | `Compiling krypto-desktop v0.1.0 ... Finished dev profile ... in 1m 02s` |
| packages/protocol `bunx tsc --noEmit` | exit 0 (no tests in this package) |
| packages/hub-client `tsc` / `bun test` | exit 0 / `53 pass, 0 fail` |
| packages/pet-engine `tsc` / `bun test` | exit 0 / `39 pass, 0 fail` |
| apps/desktop `bunx tsc --noEmit` / `bunx vite build` | exit 0 / `✓ built in 213ms` |
| apps/vscode `tsc` / `bun run build` / `bun test` | exit 0 / `✓ build ok in 93 ms` / `4 pass, 0 fail` |
| apps/mobile `bunx tsc --noEmit` | exit 0 |
| scripts/art `bunx tsc --noEmit` (extra) | exit 0 |
| `target/debug/krypto.exe --help` | `Krypto: dog pets that watch and drive your coding agents over ACP` / `Usage: krypto.exe [OPTIONS] <COMMAND>` / `--data-dir <DATA_DIR>  Hub data directory (default: <OS data dir>/krypto) [env: KRYPTO_HUB_DATA_DIR=]` / `--port <PORT>  Hub port (default: KRYPTO_HUB_URL, then config.toml, then 47800)` |

## Final ripgrep (intentional leftovers)

I ran the assignment's token list, case-insensitive, over everything except `node_modules`,
`target`, `.git` and `docs/reports`, including lockfiles. These hits remain:

- **Repo folder name (kept by rule):** `docs/CONTRACTS.md:10` (`E:\workspace\ai-pet`) and `PLAN.md:191` (`ai-pet/` root of the repo-layout tree, now annotated "repo folder (name kept after the Krypto rename)").
- **Test fixtures whose repo/cwd is named `ai-pet`** (the strings are fixture data, not the product name): `crates/brain/src/util.rs` (4), `crates/brain/src/watchdog.rs` (2), `crates/brain/src/chat/tests.rs` (16), `crates/brain/src/chat/intent.rs` (8, including the fuzzy repo-match cases `"AI-PET"` and `"aipet"`).
- **`PLAN.md:22` "Cross-platform AI pets that…"**: the concept phrase, not the brand. It only matches because the search is case-insensitive.
- **`docs/reports/*`**: history, kept by rule, with the rename note at the top of each file.

## Known gaps

- **No data migration.** This machine still has `%APPDATA%\ai-pet-hub` (token, `hub.db`, maybe `config.toml`) from the v1 smoke runs. The renamed hub starts fresh in `%APPDATA%\krypto` with a new token and an empty DB. The brain's sensitive-path guard (`HUB_SECRETS_GLOB`) now covers `**/krypto/...`, not the old folder.
- **No hooks are installed on this machine** (`~/.claude/settings.json` and `~/.codex/config.toml` have no `pet-hub` refs; `~/.copilot/hooks/ai-pet.json` is absent). Pre-rename hook entries or files would not be recognised (by decision, see below).
- Stale build artifacts `target/debug/pet-hub.exe` and `pet_hub*` are still in `target/` (gitignored). Nothing looks them up any more; `cargo clean` removes them.
- Saved client state is reset by design: desktop localStorage `aipet.*`, mobile storage `aipet.*`, and VS Code settings under `ai-pet.*` are not carried over.
- The desktop app and VS Code extension were not launched (out of scope, as before). Only typecheck, build and unit gates ran.

## Decisions (resolved by t13 on 2026-11-23)

1. **Env vars:** keep `KRYPTO_HUB_*` for all of them (`KRYPTO_HUB_BRAIN_MODEL`, `KRYPTO_HUB_OLLAMA_URL`, `KRYPTO_HUB_OLLAMA_MODEL`, `KRYPTO_HUB_REGISTRY_URL`, `KRYPTO_HUB_HOOK_EVENT`).
2. **Expo bundle id / Android package:** keep `dev.krypto.mobile`.
3. **VS Code publisher:** now `krypto` (placeholder until a Marketplace publish), so the extension id is `krypto.krypto-vscode`. Nothing in code or docs referenced the old id `ai-pet.ai-pet-vscode`; the only other `getExtension` call is `vscode.git`.
4. **Old data dir:** no migration. The hub starts fresh in `%APPDATA%\krypto`, and the stale `%APPDATA%\ai-pet-hub` can be deleted by hand.
5. **`pet-hub` hook compatibility:** dropped (see "Hook-entry recognition" above).

Still open: "Krypto" is also a DC Comics character name (flagged in the brand memo). It needs a trademark check before any public launch.

### Follow-up gates (after the decisions)

- `cargo test -p krypto-discovery`: `test result: ok. 32 passed; 0 failed` (includes `install_claude_replaces_entry_written_by_other_hub_copy ... ok`)
- `cargo clippy -p krypto-discovery --all-targets`: `Finished`, 0 warnings
- apps/vscode `bunx tsc --noEmit`: exit 0. `bun run build`: `✓ build ok in 94 ms`
- Leftover ripgrep: hits only in `PLAN.md` (2: repo folder, concept phrase), `docs/CONTRACTS.md` (1: repo folder) and the brain fixtures (`intent.rs` 8, `chat/tests.rs` 16, `util.rs` 4, `watchdog.rs` 2). There are no other hits outside `docs/reports/`.
