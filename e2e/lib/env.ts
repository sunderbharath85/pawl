// Shared plumbing for the end-to-end tests: repo paths, the pawl binary, temp dirs, free ports,
// waiting on conditions, and killing child process trees (Windows does not kill children with a parent).
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import type { Subprocess } from "bun";

export const REPO = resolve(import.meta.dir, "../..");
export const IS_WINDOWS = process.platform === "win32";
export const DEMO_AGENT = join(REPO, "e2e/fixtures/demo-agent.ts");

/** The `pawl` binary under test: $PAWL_BIN, else target/debug, else target/release. */
export function pawlBin(): string {
  const exe = IS_WINDOWS ? "pawl.exe" : "pawl";
  const candidates = [process.env.PAWL_BIN, join(REPO, "target/debug", exe), join(REPO, "target/release", exe)];
  const found = candidates.find((p): p is string => !!p && existsSync(p));
  if (!found) throw new Error("no pawl binary: run `cargo build -p pawl-hub` first (or set PAWL_BIN)");
  return found;
}

/**
 * A fresh temp dir; removed by `cleanup` unless E2E_KEEP=1 (then its path is printed). Windows can hold a
 * file lock for a moment after a child exits, so removal retries and only warns if the dir stays busy.
 */
export function tempRoot(label: string): { dir: string; cleanup: () => Promise<void> } {
  const dir = mkdtempSync(join(tmpdir(), `pawl-e2e-${label}-`));
  return {
    dir,
    cleanup: async () => {
      if (process.env.E2E_KEEP === "1") {
        console.log(`[e2e] kept ${dir}`);
        return;
      }
      for (let i = 0; ; i++) {
        try {
          rmSync(dir, { recursive: true, force: true });
          return;
        } catch (e) {
          if (i >= 20) {
            console.warn(`[e2e] could not remove ${dir}: ${String(e)}`);
            return;
          }
          await Bun.sleep(250);
        }
      }
    },
  };
}

export async function freePort(): Promise<number> {
  return new Promise((ok, fail) => {
    const srv = createServer();
    srv.once("error", fail);
    srv.listen(0, "127.0.0.1", () => {
      const addr = srv.address();
      const port = typeof addr === "object" && addr ? addr.port : 0;
      srv.close(() => ok(port));
    });
  });
}

/** Poll `fn` until it returns a truthy value; fail with `what` after `timeoutMs`. */
export async function waitFor<T>(fn: () => T | Promise<T>, what: string, timeoutMs = 20_000, everyMs = 100): Promise<NonNullable<T>> {
  const deadline = Date.now() + timeoutMs;
  let last: unknown;
  while (Date.now() < deadline) {
    try {
      const v = await fn();
      if (v) return v as NonNullable<T>;
    } catch (e) {
      last = e;
    }
    await Bun.sleep(everyMs);
  }
  throw new Error(`timed out after ${timeoutMs} ms waiting for ${what}${last ? ` (last error: ${String(last)})` : ""}`);
}

/** The environment for child processes: never inherit a developer's real hub settings. */
export function childEnv(extra: Record<string, string> = {}): Record<string, string> {
  const env: Record<string, string> = {};
  for (const [k, v] of Object.entries(process.env)) {
    if (v !== undefined && !k.startsWith("PAWL_HUB_") && k !== "PAWL_RELAY_URL" && k !== "PAWL_RELAY_ACCESS_KEY") env[k] = v;
  }
  return { ...env, ...extra };
}

/** Kill a process and everything it started. */
export async function killTree(proc: Subprocess): Promise<void> {
  if (proc.exitCode !== null || proc.signalCode !== null) return;
  if (IS_WINDOWS) {
    Bun.spawnSync(["taskkill", "/F", "/T", "/PID", String(proc.pid)], { stdout: "ignore", stderr: "ignore" });
  } else {
    const children = (pid: number): number[] =>
      Bun.spawnSync(["pgrep", "-P", String(pid)]).stdout.toString().split(/\s+/).filter(Boolean).map(Number);
    const all: number[] = [];
    const walk = (pid: number) => {
      for (const c of children(pid)) {
        walk(c);
        all.push(c);
      }
    };
    walk(proc.pid);
    for (const pid of all) {
      try {
        process.kill(pid, "SIGKILL");
      } catch {
        // already gone
      }
    }
    proc.kill("SIGKILL");
  }
  await Promise.race([proc.exited, Bun.sleep(5000)]);
}

/** Every event of a session as one string, for "does the transcript mention X" checks. */
export function transcript(events: unknown[] | undefined): string {
  return JSON.stringify(events ?? []);
}
