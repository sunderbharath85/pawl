## What and why

<!-- What this change does and why. Link the issue it closes, if any ("Closes #123"). -->

## How it was tested

<!-- Unit tests added or changed, manual steps you ran, and on which OS. -->

## Checks

<!-- See CONTRIBUTING.md. Tick what you ran; say why if you skipped one. -->

- [ ] `cargo test --workspace`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo check` and `cargo test` in `apps/desktop/src-tauri` (if the desktop app changed)
- [ ] `bun run typecheck` at the repository root
- [ ] `bun test` / `bun run test` in each changed package that has tests

## Contracts, dependencies and security

- [ ] Wire types: no change, or `packages/protocol` and `crates/protocol` changed together and `docs/CONTRACTS.md` is updated (and `PROTOCOL_VERSION` bumped if breaking)
- [ ] Dependencies: none added, removed or upgraded, or each one is listed below with the reason
- [ ] Approvals and the local API: no change, or the change is described below (rules hard floor, sensitive paths, token, Origin allowlist, audit log)
- [ ] No secrets, tokens or personal paths in code, tests, logs or screenshots

<!-- Dependency changes and security notes go here. -->
