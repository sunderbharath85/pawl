import { describe, expect, test } from "bun:test";
import { extractInviteUri, INVITE_URI_PREFIX, isUnspecifiedAddr, parseInviteUri } from "../src/index";
import { inviteFrom2, MID2 } from "./fixtures";

describe("parseInviteUri", () => {
  test("returns the public parts of a well-formed invite", () => {
    expect(parseInviteUri(inviteFrom2)).toEqual({ machineId: MID2, addr: "192.168.1.20:47801", expiresAt: 1900000000000 });
    expect(parseInviteUri(`  ${inviteFrom2}  `)?.machineId).toBe(MID2);
  });

  test("never exposes the code or key", () => {
    const info = parseInviteUri(inviteFrom2)!;
    expect(Object.keys(info).sort()).toEqual(["addr", "expiresAt", "machineId"]);
  });

  test("accepts params in any order and an IPv6 addr", () => {
    const uri = `${INVITE_URI_PREFIX}exp=5&code=abc_-&addr=%5Bfe80%3A%3A1%5D%3A47801&pk=a-b_c&mid=${MID2}&v=1`;
    expect(parseInviteUri(uri)).toEqual({ machineId: MID2, addr: "[fe80::1]:47801", expiresAt: 5 });
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
