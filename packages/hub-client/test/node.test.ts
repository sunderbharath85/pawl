import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { HubCredentialsError, hubDataDir, osDataDir, readHubCredentials, tryReadHubCredentials } from "../src/node";
import { toHubHttpUrl, toHubWsUrl } from "../src/url";

let root: string;
beforeAll(() => {
  root = mkdtempSync(join(tmpdir(), "pawl-cred-"));
});
afterAll(() => {
  rmSync(root, { recursive: true, force: true });
});

describe("osDataDir", () => {
  test("windows uses APPDATA", () => {
    expect(osDataDir({ platform: "win32", env: { APPDATA: "C:\\Users\\x\\AppData\\Roaming" }, homeDir: "C:\\Users\\x" })).toBe(
      "C:\\Users\\x\\AppData\\Roaming",
    );
  });
  test("mac uses Application Support", () => {
    expect(osDataDir({ platform: "darwin", env: {}, homeDir: "/Users/x" })).toBe(join("/Users/x", "Library", "Application Support"));
  });
  test("linux uses XDG_DATA_HOME if absolute, else ~/.local/share", () => {
    expect(osDataDir({ platform: "linux", env: { XDG_DATA_HOME: "/data" }, homeDir: "/home/x" })).toBe("/data");
    expect(osDataDir({ platform: "linux", env: { XDG_DATA_HOME: "rel" }, homeDir: "/home/x" })).toBe(join("/home/x", ".local", "share"));
    expect(osDataDir({ platform: "linux", env: {}, homeDir: "/home/x" })).toBe(join("/home/x", ".local", "share"));
  });
  test("hubDataDir appends pawl", () => {
    expect(hubDataDir({ platform: "linux", env: { XDG_DATA_HOME: "/data" } })).toBe(join("/data", "pawl"));
  });
});

describe("readHubCredentials", () => {
  test("reads and trims the token file", () => {
    const appdata = join(root, "appdata");
    mkdirSync(join(appdata, "pawl"), { recursive: true });
    writeFileSync(join(appdata, "pawl", "token"), "\uFEFFabc123\r\n");
    const c = readHubCredentials({ platform: "win32", env: { APPDATA: appdata } });
    expect(c.token).toBe("abc123");
    expect(c.tokenSource).toBe("file");
    expect(c.url).toBe("ws://127.0.0.1:47800/ws");
    expect(c.httpUrl).toBe("http://127.0.0.1:47800");
    expect(c.tokenFile).toBe(join(appdata, "pawl", "token"));
  });

  test("env overrides token and url", () => {
    const c = readHubCredentials({
      platform: "linux",
      env: { XDG_DATA_HOME: join(root, "nothing"), PAWL_HUB_TOKEN: " envtok ", PAWL_HUB_URL: "http://127.0.0.1:5000" },
    });
    expect(c.token).toBe("envtok");
    expect(c.tokenSource).toBe("env");
    expect(c.url).toBe("ws://127.0.0.1:5000/ws");
    expect(c.httpUrl).toBe("http://127.0.0.1:5000");
  });

  test("missing token throws HubCredentialsError; try variant returns null", () => {
    const opts = { platform: "linux", env: { XDG_DATA_HOME: join(root, "missing") } };
    expect(() => readHubCredentials(opts)).toThrow(HubCredentialsError);
    expect(tryReadHubCredentials(opts)).toBeNull();
  });

  test("empty token file throws", () => {
    const dir = join(root, "empty");
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "token"), "  \n");
    expect(() => readHubCredentials({ dataDir: dir, env: {} })).toThrow(/empty/);
  });
});

describe("url helpers", () => {
  test("toHubWsUrl", () => {
    expect(toHubWsUrl(undefined)).toBe("ws://127.0.0.1:47800/ws");
    expect(toHubWsUrl("http://127.0.0.1:47800")).toBe("ws://127.0.0.1:47800/ws");
    expect(toHubWsUrl("https://hub.example/")).toBe("wss://hub.example/ws");
    expect(toHubWsUrl("ws://localhost:1/ws")).toBe("ws://localhost:1/ws");
    expect(toHubWsUrl("127.0.0.1:5000")).toBe("ws://127.0.0.1:5000/ws");
    expect(toHubWsUrl("ftp://x")).toBe("ws://127.0.0.1:47800/ws");
  });
  test("toHubHttpUrl", () => {
    expect(toHubHttpUrl("ws://127.0.0.1:47800/ws")).toBe("http://127.0.0.1:47800");
    expect(toHubHttpUrl("wss://hub.example/ws")).toBe("https://hub.example");
    expect(toHubHttpUrl(null)).toBe("http://127.0.0.1:47800");
  });
});
