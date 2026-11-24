import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// wrangler.jsonc uses full-line // comments only.
const raw = readFileSync(join(import.meta.dir, "..", "wrangler.jsonc"), "utf8");
const config = JSON.parse(raw.replace(/^\s*\/\/.*$/gm, "")) as Record<string, any>;

describe("(8) wrangler.jsonc", () => {
  test("no request logging of any kind", () => {
    expect(config.observability?.enabled).toBe(false);
    expect(config.observability?.logs?.enabled).toBe(false);
    expect(config.observability?.logs?.invocation_logs).toBe(false);
    expect(config.logpush).toBe(false);
  });

  test("per-IP connect limit binding", () => {
    const rl = (config.ratelimits as any[]).find((r) => r.name === "CONNECT_LIMIT");
    expect(rl).toBeDefined();
    expect(Number.isInteger(Number(rl.namespace_id))).toBe(true);
    expect(rl.simple).toEqual({ limit: 30, period: 60 });
  });

  test("SQLite-backed Mailbox, closed by default", () => {
    expect(config.durable_objects.bindings).toEqual([{ name: "MAILBOX", class_name: "Mailbox" }]);
    expect(config.migrations[0].new_sqlite_classes).toEqual(["Mailbox"]);
    expect(config.vars?.RELAY_OPEN).toBeUndefined(); // open mode is an explicit opt-in by the deployer
    expect(raw).not.toMatch(/RELAY_ACCESS_KEY\s*[:=]\s*"[A-Za-z0-9_-]{20,}/); // no secret in the file
  });
});
