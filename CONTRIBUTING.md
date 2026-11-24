# Contributing to Pawl

Thanks for helping. This file covers setting up a dev environment, the checks a pull request must pass, and the conventions the project uses. By taking part you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md). Security problems go through [SECURITY.md](SECURITY.md), not public issues.

## Development setup

### Tools

| Tool | Used for | Notes |
|---|---|---|
| Rust, stable (via [rustup](https://rustup.rs/)) | hub, crates, desktop backend | Install `clippy` too: `rustup component add clippy` |
| [bun](https://bun.sh/) 1.4 or later | TypeScript packages and apps, package manager | The repo pins `bun@1.4.4` in `package.json` |
| Node.js | running ACP adapters through `npx` | Needed to start Claude Code or Codex sessions |
| Tauri prerequisites | desktop app | See [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your OS |
| VS Code 1.100 or later | extension development | Only if you work on `apps/vscode` |

Per OS:

- **Windows:** use Git Bash or PowerShell. The Rust toolchain needs the MSVC build tools (rustup offers to install them). WebView2, which Tauri needs, ships with Windows 11. Binaries end in `.exe` (`.\target\debug\pawl.exe` in PowerShell).
- **macOS:** install the Xcode Command Line Tools (`xcode-select --install`).
- **Linux:** install the WebKitGTK and other system packages listed in the Tauri prerequisites for your distribution.

### First build

```sh
bun install                    # at the repository root: installs every workspace package
cargo build -p pawl-hub      # builds the hub and the `pawl` CLI into target/debug/
```

The [README quickstart](README.md#quickstart-from-source) shows how to run the hub, the desktop app and the VS Code extension.

### bun

- The TypeScript packages are bun workspaces declared in the root `package.json` (`packages/*`, `apps/*`, `services/*`). Run `bun install` at the repository root, not inside a package.
- Packages in this repository depend on each other with `"workspace:*"`. Use that for any new internal dependency.
- Add a dependency to one package with `bun add <dep> --cwd <package dir>`, and commit the updated `bun.lock`.
- `scripts/bun-locked.sh` serializes installs for automated agents that run in parallel. You do not need it when working by hand.
- The art pipeline in `scripts/art/` is a separate bun project with its own lockfile. It reads `GEMINI_API_KEY` from a repository-root `.env`, which is git-ignored. Never commit `.env` or paste keys into issues or logs.

### Dependencies

Keep the dependency list small. Call out every added, removed or upgraded dependency (Rust or TypeScript) in the pull request description with the reason. A maintainer has to approve it before the pull request is merged.

## Checks before a pull request

Run these from the repository root. A pull request should pass all of them.

Rust (the workspace covers `crates/*`):

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The desktop backend (`apps/desktop/src-tauri`) is a separate Cargo project, so check and test it on its own:

```sh
(cd apps/desktop/src-tauri && cargo check && cargo test)
```

TypeScript:

```sh
bun run typecheck                          # runs each workspace package's typecheck script
(cd packages/hub-client && bun test)
(cd packages/pet-engine && bun test)
(cd apps/vscode && bun test)
```

The desktop app's TypeScript code and `apps/mobile` have no unit tests yet; the typecheck covers them.

The parentheses run each command in a subshell, so you stay at the repository root (Git Bash, macOS, Linux). In PowerShell, run each `cd` + command pair from the root instead.

If you change the desktop or VS Code app, also make sure it still builds: `bun run build` in `apps/desktop` or `apps/vscode`.

End to end (real hubs, the relay under `wrangler dev`, the phone protocol and the desktop UI in Chromium; about 10 s, also run in CI). See [e2e/README.md](e2e/README.md):

```sh
cargo build -p pawl-hub
(cd e2e && bunx playwright install chromium && bun test --timeout 120000 ./tests)
```

Add focused unit tests for new logic. Rust tests live next to the code (`#[cfg(test)]`); TypeScript tests use `bun test` (`*.test.ts` / `*.test.tsx`).

## Contracts and wire types

- [docs/CONTRACTS.md](docs/CONTRACTS.md) defines the interfaces between the parts of the system: crate APIs, the hub's endpoints, the client packages, and who owns which directory. Read the sections for the code you are changing.
- [packages/protocol/src/index.ts](packages/protocol/src/index.ts) (`@pawl/protocol`) is the source of truth for every type that goes over the wire. [crates/protocol/src/lib.rs](crates/protocol/src/lib.rs) (`pawl-protocol`) mirrors it in Rust.
- Import the shared types; do not redefine them in another package or crate.
- A change to the wire format updates both files in the same pull request, keeps them in step (field names, enum values, optional fields), and updates `docs/CONTRACTS.md`. Bump `PROTOCOL_VERSION` in both files when the change is breaking.
- [PLAN.md](PLAN.md) describes the target design and the order of the migration steps. Larger changes should fit it, or start with an issue that proposes changing it.

## Branches and commits

- Branch from `main` and open pull requests against `main`. Name branches after the change, for example `fix/hook-timeout` or `feat/vscode-rename-machine`. Work on a step of the PLAN.md migration uses `step-<n>-<topic>`, for example `step-1-machine-identity`.
- Keep a pull request to one topic. Fill in the pull request template, including the checks you ran.
- Write commit messages in [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) form:

  ```
  <type>(<optional scope>): <summary in the imperative, lower case, no period>
  ```

  Types: `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `ci`, `chore`. Use the part of the repo as the scope: `hub`, `acp`, `discovery`, `brain`, `protocol`, `hub-client`, `pet-engine`, `desktop`, `vscode`, `mobile`, `art`, or `security`. Mark breaking changes with `!` after the type or scope and a `BREAKING CHANGE:` footer. Examples:

  ```
  feat(hub): add `pawl machine` to show and rename this machine
  fix(discovery): honour PATHEXT when detecting agents on Windows
  feat(protocol)!: make every wire id global
  ```

## Releasing

1. `bun scripts/set-version.ts 0.1.2` sets the version everywhere; build once so the lockfiles follow, then commit.
2. Tag and push: `git tag -a v0.1.2 -m "Pawl 0.1.2" && git push origin v0.1.2`.
3. The Release workflow (`.github/workflows/release.yml`) builds Windows x64, macOS (Apple silicon and Intel) and Linux x64
   with `scripts/release.sh`, checks that each desktop app carries the `pawl` hub, and publishes one GitHub release with
   every installer, the CLI archives and `SHA256SUMS.txt`. Versions 0.x are marked pre-release.

`bash scripts/release.sh` builds the same files for your own machine into `dist/release/`.

## Reporting bugs and asking for features

Use the issue templates. For bugs, include `pawl --version`, your OS, the agent and adapter involved, and the relevant part of `hub.log` from the hub's data directory. Remove tokens, keys and private paths before you paste anything.
