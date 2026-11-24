// The real relay Worker (services/relay) under `wrangler dev`, on a free port with its own storage and a
// fresh access key (passed as a var, so a developer's services/relay/.dev.vars is never used or changed).
import { join } from "node:path";
import type { Subprocess } from "bun";
import { REPO, childEnv, freePort, killTree, waitFor } from "./env";

export class TestRelay {
  private constructor(
    private readonly proc: Subprocess,
    readonly url: string,
    readonly accessKey: string,
  ) {}

  static async start(root: string): Promise<TestRelay> {
    const port = await freePort();
    const inspector = await freePort();
    const accessKey = Buffer.from(crypto.getRandomValues(new Uint8Array(32))).toString("base64url");
    const log = Bun.file(join(root, "relay.log"));
    const proc = Bun.spawn(
      [
        process.execPath, "x", "wrangler", "dev",
        "--ip", "127.0.0.1", "--port", String(port), "--inspector-port", String(inspector),
        "--persist-to", join(root, "relay-state"),
        "--var", `RELAY_ACCESS_KEY:${accessKey}`,
        "--show-interactive-dev-session=false", "--log-level", "warn",
      ],
      { cwd: join(REPO, "services/relay"), stdout: log, stderr: log, stdin: "ignore", env: childEnv({ CI: "1" }) },
    );
    try {
      await waitFor(async () => (await fetch(`http://127.0.0.1:${port}/health`)).ok, "relay /health (wrangler dev)", 90_000, 250);
    } catch (e) {
      await killTree(proc);
      throw e;
    }
    return new TestRelay(proc, `ws://127.0.0.1:${port}`, accessKey);
  }

  stop(): Promise<void> {
    return killTree(this.proc);
  }
}
