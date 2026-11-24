// The shared name vectors (crates/peer/tests/vectors/names.json): the same rule as the Rust peers' device_name_ok.
import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { checkMachineName, DEVICE_NAME_MAX, deviceNameOk, deviceNameProblem, MACHINE_NAME_MAX, normalizeMachineName, trimWhiteSpace } from "../src/index";

interface NameVector {
  name: string;
  note: string;
  utf8Hex: string;
}
interface NameVectors {
  maxChars: number;
  valid: NameVector[];
  invalid: NameVector[];
}

const file = fileURLToPath(new URL("../../../crates/peer/tests/vectors/names.json", import.meta.url));
const vectors = JSON.parse(readFileSync(file, "utf8")) as NameVectors;

/** The exact name: decoded from its UTF-8 bytes (the JSON string must match it). */
function exact(v: NameVector): string {
  const bytes = new Uint8Array((v.utf8Hex.match(/../g) ?? []).map((h) => parseInt(h, 16)));
  // ignoreBOM keeps a leading U+FEFF (one vector starts with it) instead of silently dropping it.
  const name = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
  expect(name).toBe(v.name);
  return name;
}

describe("names.json vectors (crates/peer device_name_ok)", () => {
  test("same maximum as the peers", () => {
    expect(vectors.maxChars).toBe(DEVICE_NAME_MAX);
    expect(vectors.valid.length).toBeGreaterThan(0);
    expect(vectors.invalid.length).toBeGreaterThan(0);
  });

  for (const v of vectors.valid) {
    test(`valid: ${v.note}`, () => {
      expect(deviceNameOk(exact(v))).toBe(true);
    });
  }

  for (const v of vectors.invalid) {
    test(`invalid: ${v.note}`, () => {
      expect(deviceNameOk(exact(v))).toBe(false);
    });
  }
});

describe("machine names (the hub's 1-40 on top of the peer rule)", () => {
  test("every valid vector up to 40 characters is a valid machine name, trimmed like Rust", () => {
    for (const v of vectors.valid) {
      const name = exact(v);
      const trimmed = trimWhiteSpace(name);
      const fits = [...trimmed].length <= MACHINE_NAME_MAX;
      expect(normalizeMachineName(name)).toBe(fits ? trimmed : null);
      expect(checkMachineName(name).ok).toBe(fits);
    }
  });

  test("every invalid vector is refused before it is sent", () => {
    for (const v of vectors.invalid) {
      expect(normalizeMachineName(exact(v))).toBeNull();
      expect(checkMachineName(exact(v)).ok).toBe(false);
    }
  });

  test("problems and their messages", () => {
    const zwsp = String.fromCodePoint(0x200b);
    const bom = String.fromCodePoint(0xfeff);
    expect(deviceNameProblem(`Mac${zwsp}mini`)).toBe("bad_char");
    expect(deviceNameProblem(`${bom}Mac`)).toBe("bad_char"); // JS trim() would have hidden it
    expect(deviceNameProblem("Mac\tmini")).toBe("bad_char");
    expect(deviceNameProblem(String.fromCharCode(0xd800))).toBe("bad_char"); // lone surrogate
    expect(deviceNameProblem(" ".repeat(3))).toBe("empty");
    expect(deviceNameProblem("a".repeat(65))).toBe("too_long");
    expect(deviceNameProblem("a".repeat(41), MACHINE_NAME_MAX)).toBe("too_long");
    expect(checkMachineName(`Mac${zwsp}`)).toMatchObject({ ok: false, error: expect.stringContaining("invisible characters") });
    expect(checkMachineName("a".repeat(41))).toMatchObject({ ok: false, error: "Keep it to 40 characters or fewer." });
    expect(checkMachineName(`${String.fromCodePoint(0x3000)}Mac mini${String.fromCodePoint(0xa0)}`)).toEqual({ ok: true, name: "Mac mini" });
  });
});
