import { describe, expect, test } from "bun:test";
import { extractInviteUri, INVITE_URI_PREFIX, inviteRoute, isUnspecifiedAddr, linkPathLabel, maskInviteSecrets, pairingAllowsText, parseInviteUri, RELAY_ERROR_TEXT, relayHost, relayStatusText } from "../src/index";
import { inviteFrom2, MID2 } from "./fixtures";

describe("parseInviteUri", () => {
  test("returns the public parts of a well-formed invite", () => {
    expect(parseInviteUri(inviteFrom2)).toEqual({ machineId: MID2, addr: "192.168.1.20:47801", relay: null, expiresAt: 1900000000000 });
    expect(parseInviteUri(`  ${inviteFrom2}  `)?.machineId).toBe(MID2);
  });

  test("never exposes the code or key", () => {
    const info = parseInviteUri(inviteFrom2)!;
    expect(Object.keys(info).sort()).toEqual(["addr", "expiresAt", "machineId", "relay"]);
  });

  const RT = "BBBBBBBBBBBBBBBBBBBBBB";
  const relayParams = `relay=${encodeURIComponent("wss://relay.example.dev")}&rt=${RT}&rk=s3cr3t-Key_1`;

  test("accepts relay invites, with or without a LAN addr, and never exposes rt or rk", () => {
    const both = `${inviteFrom2}&${relayParams}`;
    expect(parseInviteUri(both)).toEqual({ machineId: MID2, addr: "192.168.1.20:47801", relay: "wss://relay.example.dev", expiresAt: 1900000000000 });
    const relayOnly = inviteFrom2.replace("&addr=192.168.1.20:47801", "") + `&${relayParams}`;
    const info = parseInviteUri(relayOnly)!;
    expect(info).toEqual({ machineId: MID2, addr: null, relay: "wss://relay.example.dev", expiresAt: 1900000000000 });
    const shown = JSON.stringify(info) + inviteRoute(info);
    expect(shown).not.toContain("s3cr3t");
    expect(shown).not.toContain(RT);
    expect(inviteRoute(info)).toBe("the relay relay.example.dev");
    expect(inviteRoute(parseInviteUri(both)!)).toBe("192.168.1.20:47801 (or the relay relay.example.dev)");
    // rk is optional with a relay.
    expect(parseInviteUri(relayOnly.replace("&rk=s3cr3t-Key_1", ""))?.relay).toBe("wss://relay.example.dev");
  });

  test("rejects broken relay invites", () => {
    const lanless = inviteFrom2.replace("&addr=192.168.1.20:47801", "");
    const bad = [
      lanless, // neither addr nor relay
      `${lanless}&relay=${encodeURIComponent("wss://relay.example.dev")}`, // relay without rt
      `${lanless}&rt=${RT}`, // rt without relay
      `${inviteFrom2}&rk=abc`, // rk without relay
      `${lanless}&relay=${encodeURIComponent("ftp://x")}&rt=${RT}`,
      `${lanless}&${relayParams.replace("s3cr3t-Key_1", "bad key!")}`,
    ];
    for (const uri of bad) expect(parseInviteUri(uri)).toBeNull();
  });

  test("relayHost", () => {
    expect(relayHost("wss://relay.example.dev/v1")).toBe("relay.example.dev");
    expect(relayHost("ws://127.0.0.1:8787")).toBe("127.0.0.1:8787");
  });

  test("accepts params in any order and an IPv6 addr", () => {
    const uri = `${INVITE_URI_PREFIX}exp=5&code=abc_-&addr=%5Bfe80%3A%3A1%5D%3A47801&pk=a-b_c&mid=${MID2}&v=1`;
    expect(parseInviteUri(uri)).toEqual({ machineId: MID2, addr: "[fe80::1]:47801", relay: null, expiresAt: 5 });
  });

  test("rejects anything else", () => {
    const bad = [
      "",
      "krypto://hub?v=1",
      "https://example.com",
      inviteFrom2.replace("v=1", "v=2"),
      inviteFrom2.replace(MID2, "NOT-A-MID"),
      inviteFrom2.replace("&code=AAAAAAAAAAAAAAAAAAAAAA", ""),
      inviteFrom2.replace("pk=cGstbWFjbWluaQ", "pk=has+plus"),
      inviteFrom2.replace("addr=192.168.1.20:47801", "addr="),
      inviteFrom2.replace("exp=1900000000000", "exp=soon"),
    ];
    for (const uri of bad) expect(parseInviteUri(uri)).toBeNull();
  });
});

describe("extractInviteUri", () => {
  test("finds the URI in a bare paste, in quotes, or in the CLI line", () => {
    expect(extractInviteUri(inviteFrom2)).toBe(inviteFrom2);
    expect(extractInviteUri(`"${inviteFrom2}"`)).toBe(inviteFrom2);
    expect(extractInviteUri(`krypto peer join "${inviteFrom2}"\n`)).toBe(inviteFrom2);
    expect(extractInviteUri(`  ${inviteFrom2} trailing words`)).toBe(inviteFrom2);
  });

  test("returns null without an invite", () => {
    expect(extractInviteUri("krypto peer join")).toBeNull();
    expect(extractInviteUri(INVITE_URI_PREFIX)).toBeNull();
  });
});

test("isUnspecifiedAddr spots wildcard listener addresses", () => {
  for (const a of ["0.0.0.0:47801", "0.0.0.0", "[::]:47801", "::", ":47801"]) expect(isUnspecifiedAddr(a)).toBe(true);
  for (const a of ["192.168.1.10:47801", "[::1]:47801", "[fe80::1]:47801", "mac-mini.local:47801"]) expect(isUnspecifiedAddr(a)).toBe(false);
});

describe("relay status + link path", () => {
  test("off, connected, connecting, error codes", () => {
    expect(relayStatusText(undefined).tone).toBe("off");
    expect(relayStatusText({ url: null, connected: false }).text).toBe("Relay off");
    expect(relayStatusText({ url: "wss://relay.example.dev", connected: true })).toEqual({ tone: "connected", text: "Connected to relay.example.dev" });
    expect(relayStatusText({ url: "wss://relay.example.dev", connected: false }).tone).toBe("connecting");
    const bad = relayStatusText({ url: "wss://relay.example.dev", connected: false, lastError: "bad_key" });
    expect(bad.tone).toBe("error");
    expect(bad.text).toContain("access key");
    for (const code of ["unreachable", "timeout", "bad_key", "unavailable", "rate_limited", "bad_auth", "bad_frame", "unsupported_version", "replaced", "closed"]) {
      expect(RELAY_ERROR_TEXT[code]).toBeTruthy();
    }
    expect(relayStatusText({ url: "wss://r.dev", connected: false, lastError: "new_code" }).text).toContain("new_code");
  });
  test("link path labels", () => {
    expect(linkPathLabel("lan")).toBe("LAN");
    expect(linkPathLabel("relay")).toBe("Relay");
    expect(linkPathLabel(undefined)).toBeNull();
  });
});

test("maskInviteSecrets hides the relay access key only", () => {
  const uri = `${inviteFrom2}&relay=wss%3A%2F%2Fr.dev&rt=BBBBBBBBBBBBBBBBBBBBBB&rk=s3cr3t-Key_1`;
  const masked = maskInviteSecrets(`krypto peer join "${uri}"`);
  expect(masked).not.toContain("s3cr3t");
  expect(masked).toContain("&rk=••••••\"");
  expect(masked).toContain("rt=BBBB");
  expect(maskInviteSecrets(inviteFrom2)).toBe(inviteFrom2);
});

test("pairingAllowsText depends on remote control", () => {
  expect(pairingAllowsText(true)).toContain("start agents");
  expect(pairingAllowsText(false, "phone")).toMatch(/^A paired phone sees .*cannot act on this machine/);
  expect(pairingAllowsText(true)).toContain("remote control is on");
  expect(pairingAllowsText(false)).toContain("cannot act");
  expect(pairingAllowsText(undefined)).toContain("with remote control on");
});
