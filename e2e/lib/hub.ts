// A real `pawl serve` on a throwaway data dir and port, with the scripted demo agent configured,
// plus a connected HubClient and a helper to run the `pawl` CLI against it.
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Subprocess } from "bun";
import { HubClient, type HubState } from "@pawl/hub-client";
import { DEMO_AGENT, childEnv, freePort, killTree, pawlBin, waitFor } from "./env";

export interface HubOptions {
  /** Machine name shown on other devices. */
  name: string;
  /** Accept hub-to-hub links on 127.0.0.1:<free port> (LAN pairing). */
  peerListen?: boolean;
}

export interface CliResult {
  code: number;
  stdout: string;
  stderr: string;
}

export const DEMO_AGENT_ID = "demo-agent";

export class TestHub {
  readonly bin = pawlBin();
  client!: HubClient;
  /** The local API token (a secret: tests only check it never reaches a log). */
  token = "";
  port = 0;
  peerPort: number | null = null;
  private proc: Subprocess | null = null;
  private runs = 0;

  private constructor(
    readonly dataDir: string,
    readonly opts: HubOptions,
  ) {}

  static async start(root: string, opts: HubOptions): Promise<TestHub> {
    const dataDir = join(root, opts.name.toLowerCase().replace(/[^a-z0-9]+/g, "-"));
    mkdirSync(dataDir, { recursive: true });
    writeFileSync(
      join(dataDir, "config.toml"),
      `[[custom_agents]]\nid = "${DEMO_AGENT_ID}"\nname = "Demo Agent"\ncommand = [${JSON.stringify(process.execPath)}, ${JSON.stringify(DEMO_AGENT)}]\n`,
    );
    const hub = new TestHub(dataDir, opts);
    hub.port = await freePort();
    if (opts.peerListen) hub.peerPort = await freePort();
    const named = await hub.cli(["machine", "--name", opts.name]);
    if (named.code !== 0) throw new Error(`pawl machine --name failed: ${named.stderr}`);
    await hub.serve();
    return hub;
  }

  private baseArgs(): string[] {
    return ["--data-dir", this.dataDir, "--port", String(this.port)];
  }

  private async serve(): Promise<void> {
    const args = [this.bin, ...this.baseArgs(), "serve"];
    if (this.peerPort) args.push("--peer-listen", `127.0.0.1:${this.peerPort}`);
    const log = Bun.file(join(this.dataDir, `serve-${++this.runs}.log`));
    this.proc = Bun.spawn(args, { stdout: log, stderr: log, stdin: "ignore", env: childEnv() });
    await waitFor(async () => (await fetch(`http://127.0.0.1:${this.port}/health`)).ok, `${this.opts.name} /health`, 20_000, 150);
    this.token = readFileSync(join(this.dataDir, "token"), "utf8").trim();
    this.client = new HubClient({
      url: `ws://127.0.0.1:${this.port}/ws`,
      token: this.token,
      client: "cli",
      clientVersion: "e2e",
      minReconnectDelayMs: 100,
      maxReconnectDelayMs: 1000,
    });
    this.client.connect();
    await this.waitState((s) => s.status === "connected" && !!s.machine, `${this.opts.name} hello`);
  }

  get state(): HubState {
    return this.client.getState();
  }

  get machineId(): string {
    return this.state.machine!.id;
  }

  waitState<T>(pred: (s: HubState) => T, what: string, timeoutMs = 20_000): Promise<NonNullable<T>> {
    return waitFor(() => pred(this.client.getState()), `${this.opts.name}: ${what}`, timeoutMs, 50);
  }

  /** Run `pawl <args>` against this hub. */
  async cli(args: string[], stdin?: string): Promise<CliResult> {
    const proc = Bun.spawn([this.bin, ...this.baseArgs(), ...args], {
      stdin: stdin === undefined ? "ignore" : new TextEncoder().encode(stdin),
      stdout: "pipe",
      stderr: "pipe",
      env: childEnv(),
    });
    const [stdout, stderr, code] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text(), proc.exited]);
    return { code, stdout, stderr };
  }

  /** Stop the hub (and the agents it started), keeping its data dir. */
  async stop(): Promise<void> {
    this.client?.close();
    if (this.proc) await killTree(this.proc);
    this.proc = null;
  }

  async restart(): Promise<void> {
    await this.stop();
    await this.serve();
  }

  /** Everything the hub wrote: our captured stdout/stderr plus any log files in its data dir. */
  logs(): string {
    const files: string[] = [];
    const walk = (dir: string) => {
      for (const e of readdirSync(dir, { withFileTypes: true })) {
        const p = join(dir, e.name);
        if (e.isDirectory()) walk(p);
        else if (/\.log(\.\d+)?$|\.jsonl$/.test(e.name) && !/events|sessions/.test(e.name)) files.push(p);
      }
    };
    walk(this.dataDir);
    return files.map((f) => readFileSync(f, "utf8")).join("\n");
  }
}
