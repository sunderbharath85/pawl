# doc-writer report — open-source docs (step-1 branch)

Status: **DONE**. All files are new; nothing outside the owned paths was touched (besides this report).

## Files created

| File | Content |
|---|---|
| `README.md` | What Pawl is, how it works (hub = ACP client + hooks, localhost clients, relay in progress), status table, quickstart from source (7 steps), optional brain model, repo layout, security model, contributing/license links. Screenshot placeholder = `assets/pets/dog/idle.png`. |
| `CONTRIBUTING.md` | Tools per OS (Windows/macOS/Linux), first build, bun rules (plain `bun install` for humans; `scripts/bun-locked.sh` only for parallel agents), dependency policy, PR gates, contracts/wire-type rules, branch + Conventional Commits conventions. |
| `SECURITY.md` | Private reporting via GitHub "Report a vulnerability" (no email), supported versions (pre-1.0: `main` only), threat model (protected assets, trust boundaries today, out of scope, planned multi-machine properties). |
| `CODE_OF_CONDUCT.md` | Contributor Covenant 2.1, generated from the official source text; the only change is the contact line (diffed against upstream: 1 line differs). |
| `.github/ISSUE_TEMPLATE/bug_report.md` | Bug template (labels `bug`), redaction + security reminder, env fields, `hub.log` location per OS. |
| `.github/ISSUE_TEMPLATE/feature_request.md` | Feature template (labels `enhancement`), asks how it fits PLAN.md / protocol / security. |
| `.github/pull_request_template.md` | What/why, testing, gate checklist, contracts/dependencies/security checklist. |
| `docs/reports/doc-writer.md` | This report. |

## Commands run and results

- `target/debug/pawl.exe --help`, `--version` (`pawl 0.1.0`) and `<sub> --help` for `serve agents sessions start hook install-hooks token`: every documented subcommand, flag, target and env var matches (`--data-dir`/`PAWL_HUB_DATA_DIR`, `--port`, `agents --refresh --json`, `sessions --json`, `start <AGENT> --cwd --prompt --local`, `install-hooks <claude|codex|copilot-cli|copilot-vscode> --apply --repo`). Binary (18:19) matches current `crates/hub/src/cli.rs`; there is no `machine` subcommand yet, so it is not documented.
- `pawl --data-dir <temp> install-hooks claude` (dry run): prints a diff of `~/.claude/settings.json`, writes nothing, hook command is the absolute path of the running binary (README says so). Temp dir removed.
- Package scripts read from every `package.json`: `apps/desktop` `tauri:dev`/`build`, `apps/vscode` `build` (+ `.vscode/launch.json` "Run Pawl for VS Code" with preLaunchTask build), root `typecheck`. Source checks for README claims: Origin allowlist (`crates/hub/src/server.rs`), hard floor + sensitive paths (`crates/brain/src/rules.rs`), audit_log table, `hub.log`, `config.toml` template, `ANTHROPIC_API_KEY`, Ollama provider, desktop token lookup + `pawl serve` spawn (`apps/desktop/src-tauri/src/hub.rs`).
- `bun run typecheck` (root): runs every workspace package. protocol, hub-client, pet-engine, mobile, desktop exit 0; **pawl-vscode exits 2** on step-1 WIP (`machines`/`machineId` missing, `transcript.test.ts` number vs string id) — fe-apps' in-progress work, not a docs issue.
- `bun test`: hub-client 74 pass, pet-engine 50 pass, apps/vscode 4 pass, 0 fail.
- `apps/mobile`: `bun run test` **exits 1** — `apps/mobile/test/` is empty, so no test files match. Left out of the CONTRIBUTING gates.
- External links: all return HTTP 200 (ACP site + registry JSON, rustup, bun, Tauri v2 prerequisites, Conventional Commits 1.0.0, Contributor Covenant 2.1, GitHub private-reporting guide).
- Relative links: script over all 7 files; every target exists; anchors `#status` and `README.md#quickstart-from-source` match headings.
- Personal-info grep (name, email, home path) over all 7 files: no matches.
- **Not run:** `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, desktop `cargo check`/`cargo test`. The workspace is mid-migration to protocol v2 and other engineers are building; these are standard cargo invocations, documented as the gates, not as verified-green.

## Known gaps

- `pawl machine [--name]` (step 1) is not in the quickstart because the CLI does not have it yet. Add one line under "Run the hub" when sde-hubcore lands it.
- Screenshot is a placeholder (the dog's idle sprite strip). Replace with a real desktop strip / VS Code screenshot once the apps have been run.
- Status table avoids test counts (they drift) and follows PLAN.md's 2026-11-23 status; update it as steps land.
- No `.github/ISSUE_TEMPLATE/config.yml` (not in my owned paths: it is `.yml`). Suggest adding one with `blank_issues_enabled: false` and a contact link to the Security tab.

## Decisions for the orchestrator

1. **Code of Conduct contact.** Set as requested: "the maintainers, via GitHub Security Advisories … or a private GitHub discussion". GitHub Discussions in a public repo are public, and Security Advisories are meant for vulnerabilities, so conduct reports have no truly fitting private channel. Consider a dedicated project address (not a personal one) later.
2. **Enable private vulnerability reporting** in the GitHub repo settings (Settings → Code security → Private vulnerability reporting) when the repo is published, or SECURITY.md's instructions will not work.
3. **SECURITY.md promises acknowledgement "within 7 days"** and reporter credit by default. Confirm or change.
4. **CONTRIBUTING gates** use `cargo clippy … -- -D warnings` (warnings fail) and include `cargo test` for `apps/desktop/src-tauri` (it has unit tests in `hub.rs` and `pet_window.rs`). Confirm that is the bar you want for PRs.
5. **`apps/mobile` `test` script fails** with no tests present. Either add tests or change the script (fe-mobile/fe-core own it).
6. Branch naming in CONTRIBUTING: `feat/…`, `fix/…`, and `step-<n>-<topic>` for migration steps. Commit scopes listed: hub, acp, discovery, brain, protocol, hub-client, pet-engine, desktop, vscode, mobile, art, security.
