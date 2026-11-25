# End-to-end tests

Real processes, no mocks: the `pawl` hub binary on throwaway data dirs and ports, a scripted ACP agent
([fixtures/demo-agent.ts](fixtures/demo-agent.ts)), the relay Worker under `wrangler dev`, the phone's protocol
code (`PhoneNode` from `apps/mobile`), and the desktop UI in headless Chromium.

```sh
cargo build -p pawl-hub            # the binary under test (or set PAWL_BIN)
cd e2e
bunx playwright install chromium   # once
bun test --timeout 120000 ./tests  # about 10 s
```

`bun run test` builds the hub first. Set `E2E_KEEP=1` to keep each suite's temp dir (hub data dirs, `serve-*.log`,
`relay.log`); its path is printed at the end.

| Suite | Covers |
|---|---|
| `hub.test.ts` | One hub: hello, agent discovery, an execute request approved from the CLI (`pawl pending` / `pawl approve`), deny from a client, default rules auto-allowing a read in the repo, cancel withdrawing a request, a deny rule, Claude Code hooks (`pawl hook claude`), a pet rename and sessions surviving a restart, no prompts / commands / token in the logs |
| `pairing.test.ts` | Two hubs on the LAN: pairing from an invite (one use only), the mirror of the other machine's sessions, approving its request (with confirmation) and denying from the CLI, revoke dropping the machine and its mirror in connected clients, no payloads or invite secrets in the logs |
| `relay.test.ts` | The relay refusing a wrong access key, two hubs with no LAN listener pairing and approving over the relay, a phone pairing from a phone invite and reaching both hubs (roster introduction), the phone approving with confirmation and denying, revoking the phone, nothing readable in the relay or hub logs |
| `ui.test.ts` | The built panel and pet window against a real hub: New session starting the agent, Allow on the approval card, renaming an agent's pet in Settings > Pets (advanced), renaming the default pet (the hub's `bot.petName`) in Settings > Pet on screen, and the pet window: one pet only, a click opening its bubble, Approve answering the agent, a new session adding no pet (it shows in the bubble's session switcher) |

Notes:
- The UI suite serves the build on `http://localhost:1420`, the only plain web origin the hub accepts, so it needs
  that port free (stop a running `bun run dev` in `apps/desktop`).
- The relay gets a fresh access key as a `wrangler dev` var, so `services/relay/.dev.vars` is never read or changed.
- On Windows, child processes are stopped with `taskkill /T` (the hub's agents and wrangler's `workerd` would
  otherwise outlive their parents).
