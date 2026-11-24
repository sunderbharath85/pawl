// Invite parsing (ported from crates/peer/src/invite.rs tests) and the URL / address rules.
import { describe, expect, test } from "bun:test";
import {
  encodeUrl,
  extractInviteUri,
  inviteToUri,
  normalizeRelayUrl,
  parseInvite,
  peerAddrOk,
  percentDecode,
  relayHostHeader,
  tryParseInvite,
  validAddr,
  validHost,
  type Invite,
} from "../src/lib/peer";
import { seededRandom } from "./helpers";

const random = seededRandom(31);
const code = () => encodeUrl(random(16));

function sample(addr: string): Invite {
  return {
    machineId: "abcdef012345",
    publicKey: new Uint8Array(32).fill(9),
    addr,
    code: code(),
    expiresAt: 1_700_000_000_000,
    relay: null,
    relayToken: null,
    relayKey: null,
  };
}

function withRelay(addr: string | null): Invite {
  return { ...sample("10.0.0.2:47801"), addr, relay: "wss://relay.pawl.dev/r", relayToken: code(), relayKey: "k3y_-AZ" };
}

describe("invite", () => {
  test("LAN URI round trip", () => {
    for (const addr of ["192.168.1.5:47801", "mac-mini.local:47801", "[fe80::1]:47801", "[::1]:9"]) {
      const inv = sample(addr);
      const uri = inviteToUri(inv);
      expect(uri.startsWith("pawl://peer?v=1&mid=abcdef012345&pk=")).toBe(true);
      expect(uri.includes("[")).toBe(false);
      expect(parseInvite(uri)).toEqual(inv);
      expect(parseInvite(`  ${uri}\n`)).toEqual(inv);
    }
  });

  test("relay params round trip (relay-only = phone invite)", () => {
    const open = { ...withRelay(null), relayKey: null };
    for (const inv of [withRelay("10.0.0.2:47801"), withRelay(null), open]) {
      const uri = inviteToUri(inv);
      expect(uri).toContain("&relay=wss%3A%2F%2Frelay.pawl.dev%2Fr&rt=");
      expect(uri.includes("&rk=k3y_-AZ")).toBe(inv.relayKey !== null);
      expect(uri.includes("addr=")).toBe(inv.addr !== null);
      expect(parseInvite(uri)).toEqual(inv);
    }
    const uri = inviteToUri(withRelay(null)).replace("wss%3A%2F%2Frelay.pawl.dev%2Fr", "https%3A%2F%2FRELAY.pawl.dev%3A443%2Fr%2F");
    expect(parseInvite(uri).relay).toBe("wss://relay.pawl.dev/r");
  });

  test("relay params are strict", () => {
    const good = inviteToUri(withRelay(null));
    const rt = withRelay(null).relayToken!;
    const noRt = good.split("&rt=")[0]!;
    const bad = [
      noRt,
      inviteToUri(sample("10.0.0.2:47801")) + `&rt=${rt}`,
      inviteToUri(sample("10.0.0.2:47801")).replace("&addr=10.0.0.2:47801", ""),
      good.replace("wss%3A%2F%2Frelay.pawl.dev%2Fr", "ws%3A%2F%2Frelay.pawl.dev"),
      good.replace("wss%3A%2F%2Frelay.pawl.dev%2Fr", "relay.pawl.dev"),
      good.replace("wss%3A%2F%2Frelay.pawl.dev%2Fr", "wss%3A%2F%2Frelay.pawl.dev%3Fx%3D1"),
      `${noRt}&rt=AAAA`,
      `${noRt}&rt=${rt}A`,
      `${good}&rt=${rt}`,
      `${good}&relay=wss%3A%2F%2Fother.dev`,
      inviteToUri(sample("10.0.0.2:47801")) + "&rk=k3y",
      good.replace("&rk=k3y_-AZ", "&rk=k%2By"),
      `${good}&rk=k3y`,
    ];
    for (const b of bad) expect(tryParseInvite(b)).toBeNull();
  });

  test("unknown params and order are fine", () => {
    const inv = sample("10.0.0.2:47801");
    const uri = `pawl://peer?exp=${inv.expiresAt}&code=${inv.code}&x=y&addr=10.0.0.2:47801&pk=${encodeUrl(inv.publicKey)}&mid=abcdef012345&v=1`;
    expect(parseInvite(uri)).toEqual(inv);
  });

  test("rejects bad URIs", () => {
    const good = inviteToUri(sample("10.0.0.2:47801"));
    const bad = [
      "",
      "garbage",
      good.replace("pawl://peer?", "http://peer?"),
      good.replace("v=1", "v=2"),
      good.replace("mid=abcdef012345", "mid=ABCDEF012345"),
      good.replace("10.0.0.2:47801", "10.0.0.2"),
      good.replace("10.0.0.2:47801", "10.0.0.2:0"),
      good.replace("10.0.0.2:47801", "0.0.0.0:47801"),
      good.replace("10.0.0.2:47801", "%5B%3A%3A%5D:47801"),
      good.replace("10.0.0.2:47801", "ev il:1"),
      good.replace("&exp=", "&exp=-"),
      good.replace("&exp=", "&exp=x"),
      `${good}&v=1`,
      `${good}#frag`,
      good.replace("&pk=", "&pk=AAAA"),
      good.split("&code=")[0] + "&code=AAAA&exp=1",
      good + "x".repeat(1024),
      // Low-order inviter key.
      good.replace(/pk=[^&]+/, `pk=${encodeUrl(new Uint8Array(32))}`),
    ];
    for (const b of bad) expect(tryParseInvite(b)).toBeNull();
    expect(() => parseInvite("nope")).toThrow("bad_uri");
  });

  test("percent decoding needs two hex digits", () => {
    expect(percentDecode("%41%3a")).toBe("A:");
    for (const bad of ["%+f", "%-1", "% f", "%4", "%", "a+b"]) expect(percentDecode(bad)).toBeNull();
    const good = inviteToUri(sample("10.0.0.2:47801"));
    expect(tryParseInvite(good.replace("10.0.0.2:47801", "10.0.0.2%+f:47801"))).toBeNull();
    expect(parseInvite(good.replace("10.0.0.2:47801", "10.0.0.2%3A47801")).addr).toBe("10.0.0.2:47801");
  });

  test("extract from pasted text", () => {
    const uri = inviteToUri(withRelay(null));
    expect(extractInviteUri(`pawl peer join "${uri}"`)).toBe(uri);
    expect(extractInviteUri(`see ${uri} now`)).toBe(uri);
    expect(extractInviteUri("nothing here")).toBeNull();
  });
});

describe("net rules", () => {
  test("peer-reported addr allowlist", () => {
    for (const ok of [
      "10.1.2.3:47801",
      "172.16.0.1:1",
      "172.31.255.255:1",
      "192.168.1.5:47801",
      "100.64.0.1:47801",
      "100.127.255.255:47801",
      "169.254.10.10:47801",
      "127.0.0.1:47801",
      "[::1]:47801",
      "[fe80::1]:47801",
      "[fd7a:115c:a1e0::1]:47801",
      "[::ffff:192.168.1.5]:47801",
    ]) {
      expect(peerAddrOk(ok)).toBe(true);
    }
    for (const bad of [
      "8.8.8.8:53",
      "172.32.0.1:1",
      "100.128.0.1:1",
      "100.63.255.255:1",
      "1.1.1.1:443",
      "[2001:db8::1]:47801",
      "[::ffff:8.8.8.8]:53",
      "localhost:47801",
      "mac-mini.local:47801",
      "example.com:443",
      "0.0.0.0:47801",
      "[::]:47801",
      "10.0.0.1:0",
      "10.0.0.1",
      "",
    ]) {
      expect(peerAddrOk(bad)).toBe(false);
    }
  });

  test("addr and host rules", () => {
    expect(validAddr("127.0.0.1:47801")).toBe(true);
    expect(validAddr("host:1")).toBe(true);
    expect(validAddr("[2001:db8::1]:65535")).toBe(true);
    for (const bad of ["[::]:47801", "::1:47801", "host:65536", "host:+1", ":47801", "a..b:1", "host/x:1", "[::1]47801"]) {
      expect(validAddr(bad)).toBe(false);
    }
    expect(validHost("[::1]") && validHost("relay.pawl.dev") && validHost("10.0.0.1")).toBe(true);
    for (const bad of ["::1", "[::]", "0.0.0.0", "[::1"]) expect(validHost(bad)).toBe(false);
  });

  test("relay URL normalization", () => {
    const ok: [string, string][] = [
      ["wss://relay.pawl.dev", "wss://relay.pawl.dev"],
      ["https://Relay.Pawl.dev/", "wss://relay.pawl.dev"],
      [" wss://relay.pawl.dev:443 ", "wss://relay.pawl.dev"],
      ["wss://relay.pawl.dev:8443/r/v-1", "wss://relay.pawl.dev:8443/r/v-1"],
      ["WSS://10.0.0.5", "wss://10.0.0.5"],
      ["wss://[2001:db8::1]:9", "wss://[2001:db8::1]:9"],
      ["ws://127.0.0.1:8787", "ws://127.0.0.1:8787"],
      ["http://localhost:8787//", "ws://localhost:8787"],
      ["ws://[::1]:80", "ws://[::1]"],
    ];
    for (const [raw, want] of ok) expect(normalizeRelayUrl(raw)).toBe(want);
    for (const raw of [
      "",
      "relay.pawl.dev",
      "ftp://relay.pawl.dev",
      "ws://relay.pawl.dev",
      "http://10.0.0.5:8787",
      "wss://user@relay.pawl.dev",
      "wss://relay.pawl.dev/?x=1",
      "wss://relay.pawl.dev/#x",
      "wss://relay.pawl.dev/a%20b",
      "wss://relay.pawl.dev/a//b",
      "wss://relay.pawl.dev/../x",
      "wss://relay.pawl.dev:0",
      "wss://relay.pawl.dev:65536",
      "wss://relay.pawl.dev:",
      "wss://:443",
      "wss://a..b",
      "wss://-a.dev",
      "wss://[::]:443",
      "wss://::1",
      "wss://relay .dev",
      `wss://${"a".repeat(600)}.dev`,
    ]) {
      expect(normalizeRelayUrl(raw)).toBeNull();
    }
    expect(relayHostHeader("wss://relay.pawl.dev:8443/r")).toBe("relay.pawl.dev:8443");
    expect(relayHostHeader("ws://127.0.0.1:8787")).toBe("127.0.0.1:8787");
  });
});
